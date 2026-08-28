//! The monitor's CONTROL PLANE — everything that acts on a session rather than reading one:
//! the pairing token, the injection passcode, and the two send transports (#133).
//!
//! It lives in the library half of this crate, not in `main.rs`, because it is shared: a
//! second monitor front-end (`agent-monitor-v2`) must run the SAME send decisions, the same
//! consent model and the same token, and a security surface with two implementations has two
//! behaviours. Nothing here is v1-specific — every function takes what it needs.

use crate::index;
use anyhow::{Context, Result};
use claude_replay_html::HttpResponse;
use serde_json::json;

/// The URL to hand the owner when paired: the token as a **query**, never a `#fragment`.
/// A fragment is not sent to the server, so the server-side 302→`Set-Cookie` bootstrap
/// would never see it and the first load would 401 — the exact bug the query form fixes.
/// The 302 to bare `/` is what then strips the token from the address bar and history.
pub fn tokened_url(base: &str, token: Option<&str>) -> String {
    match token {
        Some(t) => format!("{base}?token={t}"),
        None => base.to_string(),
    }
}

/// The pairing token's file — the monitor's STATE dir, mode 0600 (#197). It moved OUT of the
/// cache with the hide list, and for the same reason plus a sharper one: the token is a
/// CREDENTIAL that cannot be recomputed, so an `rm -rf ~/.cache` would silently un-pair the
/// monitor — the owner's bookmark and cookie would then 401 and they'd have to re-pair. Its
/// EXISTENCE is what flips the gate from D3b (same-user loopback) to §4.2 (token required
/// where the peer is unverifiable, i.e. macOS): `--pair` creates it, a normal run reads it.
pub fn token_path() -> std::path::PathBuf {
    crate::index::state_dir().join("auth-token")
}

/// The pre-#197 location, read only for the migration (a monitor paired under v1.79.x).
pub fn legacy_token_path(cache_root: &std::path::Path) -> std::path::PathBuf {
    cache_root.join("auth-token")
}

/// Read the persisted token, if the monitor has been paired — preferring the state path and
/// falling back to the old cache path (#197 migration), so a monitor paired before the move
/// stays paired with no re-pairing.
pub fn read_token(cache_root: &std::path::Path) -> Option<String> {
    let read = |p: std::path::PathBuf| {
        std::fs::read_to_string(p)
            .ok()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
    };
    read(token_path()).or_else(|| read(legacy_token_path(cache_root)))
}

/// Mint-and-persist a token at 0600 if absent (honoring a migrated one); return it either
/// way. Written to the STATE path, with the mode set AT OPEN (never write-then-chmod — a
/// world-readable window is exactly the hole this closes).
pub fn ensure_token(cache_root: &std::path::Path) -> Result<String> {
    if let Some(t) = read_token(cache_root) {
        // A token migrated from the cache is re-written to the state path here (`--pair`),
        // so the state copy becomes authoritative; the cache copy is left for downgrades.
        let tok = t.clone();
        let path = token_path();
        if !path.exists() {
            let _ = write_token_0600(&path, &tok);
        }
        return Ok(tok);
    }
    let tok = claude_replay_html::mint_token().context("read /dev/urandom to mint a token")?;
    write_token_0600(&token_path(), &tok)?;
    Ok(tok)
}

/// Write `tok` to `path` with mode 0600 set at open.
pub fn write_token_0600(path: &std::path::Path, tok: &str) -> Result<()> {
    crate::consent::write_0600(path, tok.as_bytes())
        .with_context(|| format!("write token {}", path.display()))
}

/// Read a line from the terminal WITHOUT echoing it — for the passcode. `stty -echo` on the
/// inherited controlling tty (dep-free, unix); if there is no tty (piped) it falls back to a
/// visible read, which is fine for a non-interactive set.
pub fn prompt_noecho(prompt: &str) -> Result<String> {
    use std::io::Write;
    eprint!("{prompt}");
    std::io::stderr().flush().ok();
    let echo_off = std::process::Command::new("stty")
        .arg("-echo")
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let mut line = String::new();
    let read = std::io::stdin().read_line(&mut line);
    if echo_off {
        let _ = std::process::Command::new("stty").arg("echo").status();
        eprintln!();
    }
    read.context("read passcode")?;
    Ok(line.trim_end_matches(['\n', '\r']).to_string())
}

/// `--set-passcode`: set or clear the injection-grant passcode, then exit. Prompts twice to
/// catch a typo; an empty entry CLEARS the passcode (the gate goes off). Stores only a salted
/// hash (0600) — never the passcode.
pub fn set_passcode_interactive() -> Result<()> {
    let p1 = prompt_noecho("New injection passcode (empty to clear): ")?;
    if p1.is_empty() {
        crate::consent::Passcode::open()
            .clear()
            .context("clear passcode")?;
        eprintln!("injection passcode cleared — granting no longer asks for one.");
        return Ok(());
    }
    let p2 = prompt_noecho("Confirm passcode: ")?;
    if p1 != p2 {
        anyhow::bail!("passcodes did not match — nothing changed");
    }
    crate::consent::Passcode::open()
        .set(&p1)
        .context("store passcode")?;
    eprintln!(
        "injection passcode set (hashed at {}). Granting a pane now requires it.",
        crate::consent::passcode_path().display()
    );
    Ok(())
}

/// A few wrong passcodes then a short lockout — blunts online guessing of a short code through
/// the browser. Single-user, so one global counter suffices.
#[derive(Default)]
pub struct Attempts {
    fails: u32,
    until: Option<std::time::Instant>,
}

/// How a passcode attempt resolved (pure). `Ok` lets the grant proceed; the rest short-circuit
/// the route with a distinct code the UI reacts to.
#[derive(Debug, PartialEq, Eq)]
pub enum PassVerdict {
    Ok,
    /// Too many wrong tries — locked for this many more seconds.
    Locked(u64),
    /// A passcode is set but none was supplied → the UI reveals the field.
    Required,
    Bad,
}

/// The lockout + verify decision, PURE over the attempt counter and an injected `now` (so the
/// lockout is unit-tested without sleeping). `verify` is the passcode check. Five wrong tries
/// arm a 30 s lockout; a correct one resets the counter.
pub fn passcode_verdict(
    body: &[u8],
    at: &mut Attempts,
    verify: impl Fn(&str) -> bool,
    now: std::time::Instant,
) -> PassVerdict {
    let submitted = String::from_utf8_lossy(body);
    let submitted = submitted.trim();
    if let Some(until) = at.until {
        if now < until {
            return PassVerdict::Locked((until - now).as_secs() + 1);
        }
        at.until = None;
    }
    if submitted.is_empty() {
        return PassVerdict::Required;
    }
    if !verify(submitted) {
        at.fails += 1;
        if at.fails >= 5 {
            at.until = Some(now + std::time::Duration::from_secs(30));
            at.fails = 0;
        }
        return PassVerdict::Bad;
    }
    at.fails = 0;
    PassVerdict::Ok
}

/// Verify the passcode carried in a grant's request body, applying the lockout. Returns
/// `Some(refusal)` to short-circuit the route, or `None` when the passcode is correct and the
/// grant may proceed. Only called when a passcode is set.
pub fn passcode_check(
    req: &claude_replay_html::Request,
    attempts: &std::sync::Mutex<Attempts>,
    pass: &crate::consent::Passcode,
) -> Option<HttpResponse> {
    let mut at = attempts.lock().unwrap_or_else(|e| e.into_inner());
    let verdict = passcode_verdict(
        req.body,
        &mut at,
        |p| pass.verify(p),
        std::time::Instant::now(),
    );
    let body = match verdict {
        PassVerdict::Ok => return None,
        PassVerdict::Locked(secs) => {
            json!({"ok": false, "code": "locked", "error": format!("too many attempts — wait {secs}s")})
        }
        PassVerdict::Required => {
            json!({"ok": false, "code": "passcode-required", "error": "enter your passcode to authorise injection into this pane"})
        }
        PassVerdict::Bad => {
            json!({"ok": false, "code": "bad-passcode", "error": "incorrect passcode"})
        }
    };
    Some(HttpResponse::json(body.to_string()))
}

/// The headless-resume command for a send-prompt (#133 idle slice), by agent. Verified
/// shapes from agent-jdi: Claude resumes the SAME session id (append, not fork) with
/// `-p`; Codex resumes by id. `--dangerously-skip-permissions` is deliberate (owner
/// decision): a headless `-p` turn with no TTY would otherwise STALL on the first
/// permission prompt — so an idle-send is an autonomous agent turn, not a chat.
pub fn resume_command(
    target: &crate::index::SendTarget,
    prompt: &str,
) -> Option<(String, Vec<String>)> {
    match target.agent.label() {
        "claude" => Some((
            "claude".into(),
            vec![
                "--resume".into(),
                target.sid.clone(),
                "--dangerously-skip-permissions".into(),
                "-p".into(),
                prompt.to_string(),
            ],
        )),
        "codex" => Some((
            "codex".into(),
            vec![
                "exec".into(),
                "resume".into(),
                target.sid.clone(),
                "--dangerously-bypass-approvals-and-sandbox".into(),
                prompt.to_string(),
            ],
        )),
        _ => None,
    }
}

/// Spawn a resume DETACHED, in the session's cwd, output discarded — the monitor does not
/// supervise it; the turn's progress shows up as the row going active (its transcript grows).
pub fn spawn_resume(target: &crate::index::SendTarget, prompt: &str) -> Result<()> {
    let (program, args) = resume_command(target, prompt)
        .ok_or_else(|| anyhow::anyhow!("no resume shape for {}", target.agent.label()))?;
    let mut cmd = std::process::Command::new(program);
    cmd.args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Some(cwd) = &target.cwd {
        cmd.current_dir(cwd);
    }
    cmd.spawn().context("spawn the resume")?;
    Ok(())
}

/// Run a tmux subcommand for a target's socket, output discarded, and fail on a non-zero
/// exit — the caller must know an injection step did not land.
pub fn tmux_cmd(sock: Option<&str>, args: &[&str], what: &str) -> Result<()> {
    let mut cmd = std::process::Command::new("tmux");
    if let Some(sock) = sock {
        cmd.arg("-L").arg(sock);
    }
    let status = cmd
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .with_context(|| format!("tmux {what}"))?;
    if !status.success() {
        anyhow::bail!("tmux {what} failed (exit {:?})", status.code());
    }
    Ok(())
}

/// Inject a prompt into a live agent's tmux pane (#133 tmux slice, verified from a foreign
/// process against a foreign pane). Four steps, each chosen to keep the prompt OUT of a
/// command line and to submit exactly once:
///  1. `load-buffer -` reads the prompt on STDIN into a named buffer — the text is never an
///     argv, so there is no shell/quoting surface to get wrong or exploit.
///  2. `paste-buffer -p` pastes it BRACKETED (`-d` deletes the buffer after): a multi-line
///     prompt arrives as one pasted block the agent's input treats as text, not as N lines
///     each Enter-submitted.
///  3. a single `send-keys Enter` submits (bracketed paste never submits on its own).
///  4. `display-message` announces the send on the pane's STATUS LINE (§3.3 — verified NOT
///     to enter stdin), so the injection is visible in the target, never silent.
pub fn tmux_send(target: &crate::index::TmuxTarget, prompt: &str) -> Result<()> {
    let sock = target.sock.as_deref();
    let pane = target.pane.as_str();
    const BUF: &str = "agent-monitor-send";

    // 1) prompt → a named tmux buffer, via stdin (never argv).
    {
        use std::io::Write;
        let mut cmd = std::process::Command::new("tmux");
        if let Some(sock) = sock {
            cmd.arg("-L").arg(sock);
        }
        let mut child = cmd
            .args(["load-buffer", "-b", BUF, "-"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .context("tmux load-buffer")?;
        child
            .stdin
            .take()
            .context("tmux load-buffer stdin")?
            .write_all(prompt.as_bytes())?;
        let status = child.wait().context("tmux load-buffer wait")?;
        if !status.success() {
            anyhow::bail!("tmux load-buffer failed (exit {:?})", status.code());
        }
    }
    // 2) paste it bracketed into the pane, deleting the buffer after.
    tmux_cmd(
        sock,
        &["paste-buffer", "-d", "-p", "-b", BUF, "-t", pane],
        "paste-buffer",
    )?;
    // 3) submit with a single Enter.
    tmux_cmd(sock, &["send-keys", "-t", pane, "Enter"], "send-keys Enter")?;
    // 4) announce on the status line — visibility without touching stdin (§3.3).
    let _ = tmux_cmd(
        sock,
        &[
            "display-message",
            "-t",
            pane,
            "agent-monitor: a prompt was sent from the rail",
        ],
        "display-message",
    );
    Ok(())
}

/// The live-session send (#133 tmux slice): resolve the target as a PROVEN live tmux link,
/// require standing consent for its exact pane/pid, then inject. Returns the `/api/send` JSON
/// body. Consent is checked against the pid THIS scan just observed, so a restarted process (a
/// new pid) has no matching grant → `no-consent`, and the owner re-grants — the pid-change
/// invalidation of §3.4. The distinct `"code":"no-consent"` lets the rail offer a grant button
/// instead of showing a dead-end refusal.
pub fn send_tmux(idx: &crate::index::Index, sid: &str, prompt: &str) -> serde_json::Value {
    let target = match idx.resolve_tmux_send(sid) {
        Ok(t) => t,
        Err(reason) => return json!({"ok": false, "error": reason.as_str()}),
    };
    let store = crate::consent::ConsentStore::open();
    if !store.is_granted(
        target.sock.as_deref(),
        &target.pane,
        &target.sid,
        target.pid,
    ) {
        return json!({
            "ok": false,
            "error": crate::index::SendRefusal::NoConsent.as_str(),
            "code": "no-consent",
        });
    }
    match tmux_send(&target, prompt) {
        Ok(()) => json!({"ok": true, "sid": sid, "via": "tmux"}),
        Err(e) => json!({"ok": false, "error": format!("{e:#}")}),
    }
}

///
/// Shared by both front-ends: one implementation of "may this prompt reach that session",
/// because a second copy of a send decision is a second security behaviour.
pub fn send_route(idx: &index::Index, req: &claude_replay_html::Request) -> HttpResponse {
    if let Some(deny) = req.deny_write() {
        return deny;
    }
    let sid = claude_replay_html::query_get(req.query, "target").map(crate::index::percent_decode);
    let prompt = String::from_utf8_lossy(req.body).trim().to_string();
    let resp = match (sid, prompt.is_empty()) {
        (None, _) => json!({"ok": false, "error": "no target session"}),
        (_, true) => json!({"ok": false, "error": "empty prompt"}),
        (Some(sid), false) => match idx.resolve_send(&sid) {
            // Finished session → resume-spawn (the idle transport, 3a).
            Ok(target) => match spawn_resume(&target, &prompt) {
                Ok(()) => json!({"ok": true, "sid": sid, "via": "resume"}),
                Err(e) => json!({"ok": false, "error": format!("{e:#}")}),
            },
            // Live session → the tmux transport (4), but ONLY for a proven
            // pane link AND standing consent. A live session with no proven
            // link, not in tmux, or not consented is refused with its reason.
            Err(crate::index::SendRefusal::SessionIsLive) => send_tmux(idx, &sid, &prompt),
            Err(reason) => json!({"ok": false, "error": reason.as_str()}),
        },
    };
    HttpResponse::json(resp.to_string())
}

///
/// Shared by both front-ends, like [`send_route`]. `attempts` is the caller's lockout counter
/// (one per process — the machine has one user).
pub fn consent_route(
    idx: &index::Index,
    req: &claude_replay_html::Request,
    attempts: &std::sync::Mutex<Attempts>,
) -> HttpResponse {
    if let Some(deny) = req.deny_write() {
        return deny;
    }
    let store = crate::consent::ConsentStore::open();
    let Some(sid) =
        claude_replay_html::query_get(req.query, "target").map(crate::index::percent_decode)
    else {
        return HttpResponse::json(json!({"ok": false, "error": "no target session"}).to_string());
    };
    // Revoke needs no passcode — removing access is always safe.
    if claude_replay_html::query_get(req.query, "op") == Some("revoke") {
        store.revoke(&sid);
        return HttpResponse::json(json!({"ok": true, "sid": sid, "granted": false}).to_string());
    }
    // Only a resolvable (proven, live, in-tmux, project-quiet) target can be
    // granted — resolve BEFORE asking for a passcode, so an ungrantable target
    // never prompts.
    let target = match idx.resolve_tmux_send(&sid) {
        Ok(t) => t,
        Err(reason) => {
            return HttpResponse::json(json!({"ok": false, "error": reason.as_str()}).to_string())
        }
    };
    // The passcode gate (#133): when set, arming a pane requires it — the
    // "something you know" on top of the cookie's "something you have".
    let pass = crate::consent::Passcode::open();
    if pass.is_set() {
        if let Some(refusal) = passcode_check(req, attempts, &pass) {
            return refusal;
        }
    }
    let resp = match store.grant(
        target.sock.as_deref(),
        &target.pane,
        &target.sid,
        target.pid,
    ) {
        Ok(_) => json!({"ok": true, "sid": sid, "granted": true}),
        Err(e) => json!({"ok": false, "error": format!("{e:#}")}),
    };
    HttpResponse::json(resp.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `state_dir()` reads process-global env; serialize the token tests that set it.
    static STATE_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// The token lives in the STATE dir at 0600, round-trips, and a second `ensure_token`
    /// is idempotent (#197 — pairing twice does not rotate).
    #[test]
    fn ensure_token_persists_0600_in_state_and_is_idempotent() {
        let _g = STATE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let state = std::env::temp_dir().join(format!("cm-tok-state-{}", std::process::id()));
        let cache = std::env::temp_dir().join(format!("cm-tok-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        let _ = std::fs::remove_dir_all(&cache);
        std::env::set_var("CLAUDE_MONITOR_STATE", &state);

        let a = ensure_token(&cache).unwrap();
        assert_eq!(a.len(), 64, "32 bytes as hex");
        assert!(
            token_path().starts_with(&state),
            "token lives in STATE, not cache"
        );
        assert!(
            !cache.join("auth-token").exists(),
            "nothing written to cache"
        );
        assert_eq!(read_token(&cache).as_deref(), Some(a.as_str()));
        assert_eq!(ensure_token(&cache).unwrap(), a, "idempotent");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(token_path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "owner-only");
        }
        std::env::remove_var("CLAUDE_MONITOR_STATE");
        let _ = std::fs::remove_dir_all(&state);
        let _ = std::fs::remove_dir_all(&cache);
    }

    /// #133 passcode gate (pure): empty body asks for the passcode; a wrong one is `Bad` and,
    /// on the FIFTH miss, arms a 30 s lockout that reports `Locked` until it lapses; a correct
    /// passcode is `Ok` and clears the counter.
    #[test]
    fn passcode_lockout_after_five_misses_then_clears_on_success() {
        let t0 = std::time::Instant::now();
        let mut at = Attempts::default();
        let right = |p: &str| p == "swordfish";

        // No passcode supplied → the UI is told to reveal the field.
        assert_eq!(
            passcode_verdict(b"  ", &mut at, right, t0),
            PassVerdict::Required
        );
        // Four wrong tries are each just Bad — no lockout yet.
        for _ in 0..4 {
            assert_eq!(
                passcode_verdict(b"nope", &mut at, right, t0),
                PassVerdict::Bad
            );
        }
        assert_eq!(at.fails, 4);
        // The fifth wrong try arms the lockout (and resets the visible counter).
        assert_eq!(
            passcode_verdict(b"nope", &mut at, right, t0),
            PassVerdict::Bad
        );
        // Now even the RIGHT passcode is refused while locked.
        match passcode_verdict(
            b"swordfish",
            &mut at,
            right,
            t0 + std::time::Duration::from_secs(5),
        ) {
            PassVerdict::Locked(secs) => assert!(secs > 0 && secs <= 30, "secs={secs}"),
            v => panic!("expected Locked, got {v:?}"),
        }
        // After the lockout lapses, the correct passcode succeeds and clears state.
        assert_eq!(
            passcode_verdict(
                b"swordfish",
                &mut at,
                right,
                t0 + std::time::Duration::from_secs(31)
            ),
            PassVerdict::Ok
        );
        assert_eq!(at.fails, 0);
        assert!(at.until.is_none());
    }

    /// #196 §4.2 regression: the paired URL MUST carry the token as a `?query`, not a
    /// `#fragment`. A fragment is never sent to the server, so the printed URL would
    /// 401 on open — the bug that shipped in v1.79.0 and this pins shut. Unpaired, the
    /// URL is bare.
    #[test]
    fn the_paired_url_uses_a_query_token_not_a_fragment() {
        let u = tokened_url("http://127.0.0.1:2727/", Some("abc"));
        assert!(u.contains("?token=abc"), "query token: {u}");
        assert!(!u.contains('#'), "never a fragment: {u}");
        assert_eq!(
            tokened_url("http://127.0.0.1:2727/", None),
            "http://127.0.0.1:2727/"
        );
    }

    /// #197 token migration: a monitor paired under v1.79.x (token in the CACHE) stays paired
    /// after the move — `read_token` finds the cache copy, and `ensure_token` promotes it to
    /// the state path without rotating, leaving the cache copy for a downgrade.
    #[test]
    fn a_cache_token_migrates_to_state_without_re_pairing() {
        let _g = STATE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let state = std::env::temp_dir().join(format!("cm-mig-state-{}", std::process::id()));
        let cache = std::env::temp_dir().join(format!("cm-mig-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        let _ = std::fs::remove_dir_all(&cache);
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("auth-token"), "legacy-token-xyz").unwrap();
        std::env::set_var("CLAUDE_MONITOR_STATE", &state);

        assert_eq!(
            read_token(&cache).as_deref(),
            Some("legacy-token-xyz"),
            "found in cache"
        );
        assert_eq!(
            ensure_token(&cache).unwrap(),
            "legacy-token-xyz",
            "not rotated"
        );
        assert_eq!(
            std::fs::read_to_string(token_path()).unwrap().trim(),
            "legacy-token-xyz",
            "promoted to the state path"
        );
        assert!(
            cache.join("auth-token").exists(),
            "cache copy left for a downgrade"
        );

        std::env::remove_var("CLAUDE_MONITOR_STATE");
        let _ = std::fs::remove_dir_all(&state);
        let _ = std::fs::remove_dir_all(&cache);
    }
}
