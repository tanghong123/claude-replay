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

/// The URL a start PRINTS on one stream (#s204): the tokened URL only where a person reads it, a
/// terminal, and the bare one anywhere else. Run as a LaunchAgent, both streams are a log file,
/// which kept the token at every restart until an agent reading that log for a stall printed it
/// into its transcript. The browser a start opens still gets the token; only the printing changes.
pub fn printed_url(base: &str, token: Option<&str>, terminal: bool) -> String {
    if terminal {
        tokened_url(base, token)
    } else {
        base.to_string()
    }
}

/// What a start prints in place of a token it kept out of a log: where the token is, never the
/// token. `agent-monitor --pair` run in a terminal hands off to the running monitor and prints its
/// URL with the token; the file is there for the other binaries, which share it.
pub fn withheld_token_note() -> String {
    format!(
        "  note: the token is not written where this goes — `agent-monitor --pair` in a terminal \
         prints the URL to open (the token is in {})",
        token_path().display()
    )
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
pub fn tmux_send(sock: Option<&str>, pane: &str, prompt: &str) -> Result<()> {
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

/// What `herdr agent get <pane>` says, reduced to the two facts a send needs (#s226): the
/// agent's state (`idle`, `working`, `blocked`, `done`, `unknown`) and the Claude session herdr's
/// hook reported for the pane (`agent_session.value`). Read at the reply's top level or under
/// `result` / `agent`, never from anywhere deeper, where another pane's record could answer.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct HerdrAgent {
    pub status: Option<String>,
    pub session: Option<String>,
}

pub fn parse_herdr_agent(reply: &str) -> HerdrAgent {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(reply) else {
        return HerdrAgent::default();
    };
    let record = ["", "/result", "/agent", "/result/agent"]
        .iter()
        .filter_map(|at| v.pointer(at))
        .find(|r| r.get("agent_status").is_some() || r.get("agent_session").is_some());
    let Some(record) = record else {
        return HerdrAgent::default();
    };
    HerdrAgent {
        status: record
            .get("agent_status")
            .and_then(|s| s.as_str())
            .map(str::to_string),
        session: record
            .pointer("/agent_session/value")
            .and_then(|s| s.as_str())
            .map(str::to_string),
    }
}

/// One pane's agent as `herdr agent list` reports it (#s238): the pane, the session herdr's hook
/// named there at the agent's start (`agent_session.value`, Claude only), and herdr's own status
/// for it, which herdr reads off the screen it draws for that pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HerdrPane {
    pub pane: String,
    pub session: Option<String>,
    pub status: String,
}

/// The panes of a `herdr agent list` reply (`result.agents[]`, measured on herdr 0.9.5); an entry
/// without a pane id or a status is left out.
pub fn parse_herdr_agent_list(reply: &str) -> Vec<HerdrPane> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(reply) else {
        return Vec::new();
    };
    let Some(agents) = ["/result/agents", "/agents"]
        .iter()
        .find_map(|at| v.pointer(at).and_then(|a| a.as_array()))
    else {
        return Vec::new();
    };
    agents
        .iter()
        .filter_map(|a| {
            Some(HerdrPane {
                pane: a.get("pane_id")?.as_str()?.to_string(),
                session: a
                    .pointer("/agent_session/value")
                    .and_then(|s| s.as_str())
                    .map(str::to_string),
                status: a.get("agent_status")?.as_str()?.to_string(),
            })
        })
        .collect()
}

/// `herdr agent list` from the server at `socket`, when it is a socket of this user (#s238).
pub fn herdr_agent_list(socket: &str) -> Result<Vec<HerdrPane>> {
    let socket = herdr_socket_ok(socket)?;
    let reply = herdr_cli(&herdr_program(), &socket, &["agent", "list"])?;
    Ok(parse_herdr_agent_list(&reply))
}

/// What the Claude pane's input box holds, read off `herdr pane read --source visible --ansi`
/// (#s226). Measured on aries-black (herdr 0.9.5, Claude Code 2.1.309): the box is the LAST pair of
/// `─` rules on screen with the prompt `❯` and a NO-BREAK SPACE on its first line; an empty
/// input shows a DIM suggestion (`ESC[2m`); a typed draft is not dim, and a second line of it
/// continues below, indented; a permission dialog REPLACES the box. So the box is found by its
/// rules, everything in it after the prompt counts, and only text drawn wholly dim is empty.
#[derive(Debug, PartialEq, Eq)]
pub enum InputLine {
    Empty,
    Draft,
    Missing,
}

/// One SGR code's effect on "dim": `2` sets it, a reset (`0`, empty) or `22` clears it, and a
/// colour's sub-parameters are skipped whole — `38;2;r;g;b` and `38;5;n` (and the `48`/`58`
/// forms) carry numbers that are not attributes, and the colon form `38:2::r:g:b` is a single
/// parameter. Read naively, the `2` in a truecolor code turned dim on, so a draft Claude colours
/// read as an empty box (the second review of #s226).
fn sgr_dim(code: &str, dim: &mut bool) {
    let params: Vec<&str> = code.split(';').collect();
    let mut i = 0;
    while i < params.len() {
        match params[i] {
            "2" => *dim = true,
            "" | "0" | "22" => *dim = false,
            "38" | "48" | "58" => {
                i += match params.get(i + 1) {
                    Some(&"5") => 2,
                    Some(&"2") => 4,
                    _ => 0,
                };
            }
            _ => {}
        }
        i += 1;
    }
}

/// A line's text with its SGR codes, as `(char, dim)` for every visible character.
/// Escape sequences are skipped whole, by their own grammar, so none can swallow the text after
/// it: a CSI (`ESC [`) ends at its final byte (`@` to `~`, so `ESC[2~` too, not only a letter), an
/// OSC (`ESC ]`, such as a hyperlink) at BEL or `ESC \`, and any other escape is two characters.
fn visible_cells(line: &str, dim: &mut bool) -> Vec<(char, bool)> {
    let mut out = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push((c, *dim));
            continue;
        }
        match chars.next() {
            Some('[') => {
                let mut code = String::new();
                for d in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&d) {
                        if d == 'm' {
                            sgr_dim(&code, dim);
                        }
                        break;
                    }
                    code.push(d);
                }
            }
            Some(']') => {
                while let Some(d) = chars.next() {
                    if d == '\u{7}' || (d == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

pub fn herdr_input_line(screen: &str) -> InputLine {
    const PROMPT: &str = "\u{276f}\u{a0}";
    let lines: Vec<&str> = screen.lines().collect();
    let is_rule = |line: &str| {
        let text: String = visible_cells(line, &mut false)
            .into_iter()
            .map(|(c, _)| c)
            .collect();
        text.trim_start().starts_with('\u{2500}')
    };
    // The box: the last rule, the rule above it, and the prompt on the first line between them.
    let Some(bottom) = lines.iter().rposition(|l| is_rule(l)) else {
        return InputLine::Missing;
    };
    let Some(top) = lines[..bottom].iter().rposition(|l| is_rule(l)) else {
        return InputLine::Missing;
    };
    let body = &lines[top + 1..bottom];
    let Some((_, after)) = body.first().and_then(|l| l.split_once(PROMPT)) else {
        return InputLine::Missing;
    };
    let mut dim = false;
    let drafted = std::iter::once(after)
        .chain(body[1..].iter().copied())
        .flat_map(|line| visible_cells(line, &mut dim))
        .any(|(c, dim)| !c.is_whitespace() && !dim);
    if drafted {
        InputLine::Draft
    } else {
        InputLine::Empty
    }
}

/// The herdr send's checks, PURE (#s226): the pane is PROVEN to run this session (herdr's own
/// hook says so, on top of the scan's proven link), no dialog is open, and the input holds no
/// draft. Each refusal says what to do, since the reader acts in the pane, not here.
pub fn herdr_send_verdict(
    sid: &str,
    pane: &str,
    agent: &HerdrAgent,
    input: InputLine,
    client_wait: bool,
) -> Result<(), String> {
    match agent.session.as_deref() {
        None => {
            return Err(format!(
                "herdr does not say which session runs in pane {pane}: run `herdr integration install claude` on this machine, then send again"
            ))
        }
        Some(other) if other != sid => {
            return Err(format!(
                "pane {pane} runs another session ({other}), not this one: nothing was typed"
            ))
        }
        Some(_) => {}
    }
    if agent.status.as_deref() == Some("blocked") || client_wait {
        return Err(
            "a question or a permission prompt is open in the session: answer it there first"
                .into(),
        );
    }
    match input {
        InputLine::Missing => Err(
            "the session's input box is not on screen (a dialog or a menu may be open): nothing was typed"
                .into(),
        ),
        InputLine::Draft => Err(
            "the session's input holds an unsent draft: send or clear it there first".into(),
        ),
        InputLine::Empty => Ok(()),
    }
}

/// A herdr pane id as herdr writes it (`w1:p2`, measured): anything else, from another
/// process's environment, is never handed to the herdr CLI, where it could read as a flag.
pub fn herdr_pane_id_ok(pane: &str) -> bool {
    let Some((w, p)) = pane.split_once(":p") else {
        return false;
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    w.strip_prefix('w').is_some_and(digits) && digits(p)
}

/// This process's effective uid, read as the owner of a file it just made (no libc in a plain
/// build; the home directory's owner is not the same thing under a job shell that runs as
/// another user). It never changes, so it is read once: the state pass checks a herdr socket
/// every few seconds (#s238).
fn effective_uid() -> Result<u32> {
    use std::os::unix::fs::MetadataExt;
    static UID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    if let Some(uid) = UID.get() {
        return Ok(*uid);
    }
    // One name per call: two sends at once must not remove each other's probe.
    static PROBES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = PROBES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let probe = std::env::temp_dir().join(format!("agent-monitor-uid-{}-{n}", std::process::id()));
    std::fs::write(&probe, b"").context("a scratch file to read this process's uid")?;
    let uid = std::fs::metadata(&probe).map(|m| m.uid());
    let _ = std::fs::remove_file(&probe);
    let uid = uid?;
    Ok(*UID.get_or_init(|| uid))
}

/// The pane's herdr socket, when it is one we may talk to: an absolute path named `herdr.sock`
/// that is a socket owned by this process's own user (it comes from another process's
/// environment, so it is checked rather than trusted).
fn herdr_socket_ok(socket: &str) -> Result<std::path::PathBuf> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let path = std::path::PathBuf::from(socket);
    if !path.is_absolute() || path.file_name() != Some(std::ffi::OsStr::new("herdr.sock")) {
        anyhow::bail!("{socket} is not a herdr socket path");
    }
    let meta = std::fs::metadata(&path).with_context(|| format!("the herdr socket {socket}"))?;
    if !meta.file_type().is_socket() || meta.uid() != effective_uid()? {
        anyhow::bail!("{socket} is not a herdr socket of this user");
    }
    Ok(path)
}

/// The prompt as herdr types it: a carriage return becomes a newline and every other control
/// character goes, so nothing in it can act as a key — a CR submitting the first line before the
/// Enter-time checks, an ESC interrupting the agent. tmux's bracketed paste keeps them inert;
/// whether herdr's `send_text` does was not measured.
fn herdr_text(prompt: &str) -> String {
    prompt
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

/// The herdr command, final when `AGENT_MONITOR_HERDR` names one (a test's stand-in), else
/// `herdr` on PATH.
fn herdr_program() -> std::ffi::OsString {
    std::env::var_os("AGENT_MONITOR_HERDR")
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| "herdr".into())
}

/// How long one herdr step may take before the send gives up (a hung herdr must not hold the
/// request, nor the reader's pending send, for ever).
const HERDR_STEP: std::time::Duration = std::time::Duration::from_secs(5);

/// Run `herdr` against the pane's own server and return its stdout. Every step addresses that
/// one server the same way, by `HERDR_SOCKET_PATH` (as herdr sets it in its panes), so the checks
/// and the typing can never reach two servers whose pane ids repeat; the monitor's own `HERDR_*`
/// variables (it may itself run in a herdr pane) are removed first. Each step is bounded at
/// `HERDR_STEP`, its two pipes read side by side. herdr reports an error as JSON on stderr with
/// exit 1.
fn herdr_cli(program: &std::ffi::OsStr, socket: &std::path::Path, args: &[&str]) -> Result<String> {
    use std::io::Read;
    let mut cmd = std::process::Command::new(program);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("HERDR_") {
            cmd.env_remove(key);
        }
    }
    let mut child = cmd
        .env("HERDR_SOCKET_PATH", socket)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("herdr {}", args.join(" ")))?;
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut buf);
            }
            let _ = tx.send(buf);
        });
        rx
    };
    let out = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let err = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let deadline = std::time::Instant::now() + HERDR_STEP;
    let timed_out = || {
        anyhow::anyhow!(
            "herdr {} did not answer within {}s",
            args.join(" "),
            HERDR_STEP.as_secs()
        )
    };
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20))
            }
            other => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(match other {
                    Err(e) => anyhow::Error::from(e).context("wait for herdr"),
                    _ => timed_out(),
                });
            }
        }
    };
    // A grandchild holding the pipes open must not hold the request either; a child that exited
    // just before the deadline still gets a moment for its last bytes.
    let left = || {
        deadline
            .saturating_duration_since(std::time::Instant::now())
            .max(std::time::Duration::from_millis(500))
    };
    let out = out.recv_timeout(left()).map_err(|_| timed_out())?;
    let err = err.recv_timeout(left()).unwrap_or_default();
    if !status.success() {
        anyhow::bail!(
            "herdr {} failed (exit {:?}): {}",
            args.first().unwrap_or(&""),
            status.code(),
            String::from_utf8_lossy(&err)
                .trim()
                .chars()
                .take(300)
                .collect::<String>()
        );
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// Type the prompt into the pane through herdr's socket API (`pane.send_text`, one JSON line),
/// so it is never an argv: `herdr pane send-text` takes the text only as an argument (measured,
/// a `-` is typed literally). A multi-line prompt arrives as one draft; its newline does not
/// submit (measured).
fn herdr_send_text(socket: &std::path::Path, pane: &str, text: &str) -> Result<()> {
    use std::io::{BufRead, Write};
    let stream = std::os::unix::net::UnixStream::connect(socket)
        .with_context(|| format!("connect to the herdr server at {}", socket.display()))?;
    stream.set_read_timeout(Some(HERDR_STEP))?;
    stream.set_write_timeout(Some(HERDR_STEP))?;
    let request = json!({
        "id": "agent-monitor-send",
        "method": "pane.send_text",
        "params": {"pane_id": pane, "text": text},
    });
    (&stream).write_all(format!("{request}\n").as_bytes())?;
    let mut reply = String::new();
    std::io::BufReader::new(&stream)
        .read_line(&mut reply)
        .context("read the herdr server's reply")?;
    let v: serde_json::Value = serde_json::from_str(reply.trim()).context("the herdr reply")?;
    if v.get("id").and_then(|i| i.as_str()) != Some("agent-monitor-send")
        || v.pointer("/result/type").and_then(|t| t.as_str()) != Some("ok")
    {
        anyhow::bail!(
            "herdr did not type the prompt: {}",
            reply.trim().chars().take(300).collect::<String>()
        );
    }
    Ok(())
}

/// Why a herdr send did not complete: a check held it (said to the reader as is), or a step
/// failed.
pub enum HerdrSendError {
    Held(String),
    Failed(anyhow::Error),
}

/// Type a prompt into a live agent's herdr pane (#s226): prove the pane runs this session and
/// that its input is empty and no dialog is open (`herdr_send_verdict`), type the prompt through
/// the socket, look AGAIN — the typed text must be in the input and no dialog open — and only
/// then press Enter once. A dialog that opened in between would otherwise take the Enter as its
/// first choice. herdr has no command measured to announce the send in the pane, as tmux's status
/// line does; the typed text is visible in the input box before it is submitted.
pub fn herdr_send(
    socket: Option<&str>,
    pane: &str,
    sid: &str,
    client_wait: bool,
    prompt: &str,
) -> Result<(), HerdrSendError> {
    herdr_send_with(&herdr_program(), socket, pane, sid, client_wait, prompt)
}

fn herdr_send_with(
    program: &std::ffi::OsStr,
    socket: Option<&str>,
    pane: &str,
    sid: &str,
    client_wait: bool,
    prompt: &str,
) -> Result<(), HerdrSendError> {
    if !herdr_pane_id_ok(pane) {
        return Err(HerdrSendError::Held(format!(
            "{pane} is not a herdr pane id: nothing was typed"
        )));
    }
    let socket = socket
        .context("the pane named no herdr server (HERDR_SOCKET_PATH)")
        .and_then(herdr_socket_ok)
        .map_err(HerdrSendError::Failed)?;
    let look = || -> Result<(HerdrAgent, InputLine), HerdrSendError> {
        let agent =
            herdr_cli(program, &socket, &["agent", "get", pane]).map_err(HerdrSendError::Failed)?;
        let screen = herdr_cli(
            program,
            &socket,
            &[
                "pane", "read", pane, "--source", "visible", "--lines", "40", "--ansi",
            ],
        )
        .map_err(HerdrSendError::Failed)?;
        Ok((parse_herdr_agent(&agent), herdr_input_line(&screen)))
    };
    let (agent, input) = look()?;
    herdr_send_verdict(sid, pane, &agent, input, client_wait).map_err(HerdrSendError::Held)?;
    // Once more right before typing: a dialog that opened since would take the text itself (a
    // leading digit picks a choice). A window remains, as it does for tmux with no check at all.
    let agent = parse_herdr_agent(
        &herdr_cli(program, &socket, &["agent", "get", pane]).map_err(HerdrSendError::Failed)?,
    );
    herdr_send_verdict(sid, pane, &agent, InputLine::Empty, false).map_err(HerdrSendError::Held)?;
    herdr_send_text(&socket, pane, &herdr_text(prompt)).map_err(HerdrSendError::Failed)?;
    // And before Enter: still this session, no dialog, and our text in the box. (Whether every
    // character in the box is ours is not read back: Claude may show a long prompt as a paste.)
    // The box is read until it shows the text or something else, for about a second, since
    // Claude may not have redrawn the moment herdr has typed.
    let mut seen = look()?;
    for _ in 0..6 {
        if seen.1 != InputLine::Empty || seen.0.status.as_deref() == Some("blocked") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
        seen = look()?;
    }
    let (agent, input) = seen;
    if agent.session.as_deref() != Some(sid)
        || agent.status.as_deref() == Some("blocked")
        || input != InputLine::Draft
    {
        return Err(HerdrSendError::Held(
            "the prompt is typed in the session's input but not sent: something opened over it, so send it there".into(),
        ));
    }
    herdr_cli(program, &socket, &["pane", "send-keys", pane, "enter"]).map_err(|e| {
        HerdrSendError::Failed(
            e.context("the prompt is typed in the session's input but Enter did not reach it"),
        )
    })?;
    Ok(())
}

/// The live-session send (#133 tmux slice, #s226 herdr): resolve the target as a PROVEN live
/// pane link, require standing consent for its exact pane/pid, then inject. Returns the
/// `/api/send` JSON body. Consent is checked against the pid THIS scan just observed, so a
/// restarted process (a new pid) has no matching grant → `no-consent`, and the owner re-grants — the pid-change
/// invalidation of §3.4. The distinct `"code":"no-consent"` lets the rail offer a grant button
/// instead of showing a dead-end refusal.
pub fn send_tmux(idx: &crate::index::Index, sid: &str, prompt: &str) -> serde_json::Value {
    send_tmux_checked(idx, sid, prompt).unwrap_or_else(|reason| {
        if idx.shared_codex_required(sid) {
            shared_runtime_body()
        } else {
            send_refusal_body(reason)
        }
    })
}

fn send_tmux_checked(
    idx: &crate::index::Index,
    sid: &str,
    prompt: &str,
) -> Result<serde_json::Value, index::SendRefusal> {
    let target = idx.resolve_pane_send(sid)?;
    let store = crate::consent::ConsentStore::open();
    if !store.consents(
        target.addr.consent_sock().as_deref(),
        target.addr.pane(),
        &target.sid,
        target.pid,
        target.addr.grant_follows(target.named),
    ) {
        return Ok(json!({
            "ok": false,
            "error": crate::index::SendRefusal::NoConsent.as_str(),
            "code": "no-consent",
        }));
    }
    Ok(match &target.addr {
        index::PaneAddr::Tmux { sock, pane } => match tmux_send(sock.as_deref(), pane, prompt) {
            Ok(()) => json!({"ok": true, "sid": sid, "via": "tmux"}),
            Err(e) => json!({"ok": false, "error": format!("{e:#}")}),
        },
        index::PaneAddr::Herdr { socket, pane } => {
            match herdr_send(socket.as_deref(), pane, sid, target.client_wait, prompt) {
                Ok(()) => json!({"ok": true, "sid": sid, "via": "herdr"}),
                // A held send typed nothing; `held` says so to a client that tells them apart.
                Err(HerdrSendError::Held(why)) => {
                    json!({"ok": false, "code": "held", "error": why})
                }
                Err(HerdrSendError::Failed(e)) => json!({"ok": false, "error": format!("{e:#}")}),
            }
        }
    })
}

fn send_refusal_body(reason: index::SendRefusal) -> serde_json::Value {
    json!({"ok":false,"error":reason.as_str()})
}

/// The shared-runtime refusal used by both the HTTP ownership guard and the
/// legacy transport resolver. No CLI process or pane injection has run.
pub(crate) fn shared_runtime_required() -> HttpResponse {
    let mut reply = HttpResponse::json(shared_runtime_body().to_string());
    reply.code = "409 Conflict";
    reply
}

fn shared_runtime_body() -> serde_json::Value {
    json!({"ok":false,"code":"shared_runtime_required",
        "error":"Enable Codex native mode under Write mode to use this shared server."})
}

fn checked_send_refusal(idx: &index::Index, sid: &str, reason: index::SendRefusal) -> HttpResponse {
    if idx.shared_codex_required(sid) {
        shared_runtime_required()
    } else {
        send_refusal_response(reason)
    }
}

fn send_refusal_response(reason: index::SendRefusal) -> HttpResponse {
    HttpResponse::json(send_refusal_body(reason).to_string())
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
            Err(crate::index::SendRefusal::SessionIsLive) => {
                match send_tmux_checked(idx, &sid, &prompt) {
                    Ok(body) => body,
                    Err(reason) => return checked_send_refusal(idx, &sid, reason),
                }
            }
            Err(reason) => return checked_send_refusal(idx, &sid, reason),
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
    // Only a resolvable (proven, live, in a tmux or herdr pane, project-quiet) target
    // can be granted — resolve BEFORE asking for a passcode, so an ungrantable target
    // never prompts.
    let target = match idx.resolve_pane_send(&sid) {
        Ok(t) => t,
        Err(reason) => return checked_send_refusal(idx, &sid, reason),
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
        target.addr.consent_sock().as_deref(),
        target.addr.pane(),
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

    const SID: &str = "bdc93d7b-cd2b-4853-87d8-e88f9e6d71e3";

    /// #s226: `herdr agent get` reduced to the state and the session herdr's hook reported, at
    /// the reply's top level or under `result`/`agent` (aries-black, herdr 0.9.5: `agent_status`,
    /// `agent_session` {source herdr:claude, kind id, value}) — never from a sibling deeper down.
    #[test]
    fn herdr_agent_reports_its_state_and_session() {
        let top = format!(
            r#"{{"agent_status":"blocked","interactive_ready":true,"agent_session":{{"source":"herdr:claude","kind":"id","value":"{SID}"}}}}"#
        );
        let nested = format!(
            r#"{{"result":{{"agent":{{"agent_status":"idle","agent_session":{{"value":"{SID}"}}}}}}}}"#
        );
        assert_eq!(
            parse_herdr_agent(&top),
            HerdrAgent {
                status: Some("blocked".into()),
                session: Some(SID.into())
            }
        );
        assert_eq!(parse_herdr_agent(&nested).status.as_deref(), Some("idle"));
        assert_eq!(parse_herdr_agent(&nested).session.as_deref(), Some(SID));
        // No hook: no session; a reply that is not JSON says nothing; and another pane's record
        // further down never answers for this one.
        assert_eq!(
            parse_herdr_agent(r#"{"agent_status":"idle"}"#).session,
            None
        );
        assert_eq!(parse_herdr_agent("not json"), HerdrAgent::default());
        let sibling = format!(
            r#"{{"panes":[{{"agent_status":"idle","agent_session":{{"value":"{SID}"}}}}]}}"#
        );
        assert_eq!(parse_herdr_agent(&sibling), HerdrAgent::default());
    }

    const RULE: &str =
        "\u{1b}[38;2;136;136;136m\u{2500}\u{2500}\u{2500}\u{2500} herdr-probe \u{2500}\u{1b}[0m";

    /// A Claude pane's screen with `input` in its box, as aries-black showed it.
    fn screen(input: &str) -> String {
        format!("Some reply\n{RULE}\n{input}\n{RULE}\n  \u{23f8} manual mode on\n")
    }

    /// #s238: `herdr agent list`'s shape, measured on herdr 0.9.5 — each pane's id, the session
    /// herdr's hook named there, and herdr's own status; an entry missing either half is dropped.
    #[test]
    fn herdr_agent_list_reads_each_panes_session_and_status() {
        let reply = format!(
            r#"{{"id":"cli:agent:list","result":{{"type":"agent_list","agents":[
                {{"agent":"claude","agent_session":{{"agent":"claude","kind":"id","source":"herdr:claude","value":"{SID}"}},
                  "agent_status":"working","pane_id":"wD:p1","cwd":"/w","interactive_ready":true}},
                {{"agent":"codex","agent_status":"idle","pane_id":"w2:p1"}},
                {{"agent":"claude","agent_status":"blocked"}},
                {{"agent":"claude","pane_id":"w3:p1"}}
            ]}}}}"#
        );
        assert_eq!(
            parse_herdr_agent_list(&reply),
            vec![
                HerdrPane {
                    pane: "wD:p1".into(),
                    session: Some(SID.into()),
                    status: "working".into(),
                },
                HerdrPane {
                    pane: "w2:p1".into(),
                    session: None,
                    status: "idle".into(),
                },
            ]
        );
        assert!(parse_herdr_agent_list("not json").is_empty());
        assert!(parse_herdr_agent_list(r#"{"result":{"agents":{}}}"#).is_empty());
    }

    /// #s226: the input box as `herdr pane read --ansi` shows it, measured on aries-black: the
    /// last pair of `─` rules with `❯` and a NO-BREAK SPACE on its first line, a DIM suggestion
    /// when empty, plain text for a draft (a second line continues below), and no box at all while
    /// a permission dialog holds the screen.
    #[test]
    fn herdr_input_line_tells_an_empty_input_from_a_draft_and_a_dialog() {
        let empty =
            screen("\u{276f}\u{a0}\u{1b}[0m\u{1b}[2mTry \"write a test for repos.list\"\u{1b}[0m");
        assert_eq!(
            herdr_input_line(&empty),
            InputLine::Empty,
            "a dim suggestion is no draft"
        );
        assert_eq!(
            herdr_input_line(&screen("\u{276f}\u{a0}")),
            InputLine::Empty
        );
        let draft = screen("\u{276f}\u{a0}herdr probe draft, not sent");
        assert_eq!(herdr_input_line(&draft), InputLine::Draft);
        let two = screen("\u{276f}\u{a0}herdr probe line one\n  herdr probe line two");
        assert_eq!(herdr_input_line(&two), InputLine::Draft);
        // A draft begun with Shift+Enter: its first line is empty, its text below.
        let below = screen("\u{276f}\u{a0}\n  my real draft text");
        assert_eq!(herdr_input_line(&below), InputLine::Draft);
        // Dim that the line turns off before the text is not a suggestion.
        let undimmed = screen("\u{276f}\u{a0}\u{1b}[2m\u{1b}[22mmy own words");
        assert_eq!(herdr_input_line(&undimmed), InputLine::Draft);
        // A dim suggestion followed by plain text is a draft.
        let mixed = screen("\u{276f}\u{a0}\u{1b}[2mTry\u{1b}[0m and mine");
        assert_eq!(herdr_input_line(&mixed), InputLine::Draft);
        // Every escape is skipped whole: a CSI ending in `~` and an OSC hyperlink (either end)
        // swallow none of the draft after them.
        for wrapped in [
            "\u{276f}\u{a0}\u{1b}[2~123",
            "\u{276f}\u{a0}\u{1b}]8;;https://example.test\u{7}link\u{1b}]8;;\u{7}",
            "\u{276f}\u{a0}\u{1b}]8;;https://example.test\u{1b}\\link",
        ] {
            assert_eq!(
                herdr_input_line(&screen(wrapped)),
                InputLine::Draft,
                "{wrapped:?}"
            );
        }
        // Colour is not dim: a truecolor or 256-colour code's numbers are not attributes (the
        // second review: `38;2;…` read as "dim on"), nor is a background or the colon form; a
        // dim suggestion that also carries a colour is still a suggestion.
        for coloured in [
            "\u{276f}\u{a0}\u{1b}[38;2;215;119;87m/compact\u{1b}[39m",
            "\u{276f}\u{a0}\u{1b}[38;5;2mgreen words\u{1b}[39m",
            "\u{276f}\u{a0}\u{1b}[48;2;55;55;55m[Pasted text #1 +20 lines]\u{1b}[49m",
            "\u{276f}\u{a0}\u{1b}[38:2::10:20:30mcolon colour",
        ] {
            assert_eq!(
                herdr_input_line(&screen(coloured)),
                InputLine::Draft,
                "{coloured:?}"
            );
        }
        let dim_coloured = screen("\u{276f}\u{a0}\u{1b}[2;38;2;136;136;136mTry \"x\"\u{1b}[0m");
        assert_eq!(herdr_input_line(&dim_coloured), InputLine::Empty);
        // A permission dialog, its choices drawn with the same ❯ and no-break space, and rules from
        // output above it: no box, so nothing may be typed.
        let dialog = format!(
            "{RULE}\nsome tool output\n{RULE}\n Bash command\n   rm -rf build\n Do you want to proceed?\n \u{276f}\u{a0}1. Yes\n   2. Yes, and always allow\n   4. No\n Esc to cancel \u{b7} Tab to amend\n"
        );
        assert_eq!(
            herdr_input_line(&dialog),
            InputLine::Missing,
            "a dialog replaces the box"
        );
        assert_eq!(
            herdr_input_line("\u{276f}\u{a0}a prompt with no box\n"),
            InputLine::Missing
        );
    }

    /// #s226: the send's checks, in order — the pane must be PROVEN this session's by herdr's own
    /// hook, no question or permission prompt may be open, and the input must hold no draft.
    #[test]
    fn herdr_send_verdict_holds_for_an_unproven_pane_a_dialog_or_a_draft() {
        let ok = HerdrAgent {
            status: Some("idle".into()),
            session: Some(SID.into()),
        };
        assert_eq!(
            herdr_send_verdict(SID, "w1:p2", &ok, InputLine::Empty, false),
            Ok(())
        );
        let verdict = |agent: &HerdrAgent, input, wait| {
            herdr_send_verdict(SID, "w1:p2", agent, input, wait).unwrap_err()
        };
        let no_hook = HerdrAgent {
            status: Some("idle".into()),
            session: None,
        };
        assert!(
            verdict(&no_hook, InputLine::Empty, false).contains("herdr integration install claude")
        );
        let other = HerdrAgent {
            status: Some("idle".into()),
            session: Some("someone-else".into()),
        };
        assert!(verdict(&other, InputLine::Empty, false).contains("another session"));
        let blocked = HerdrAgent {
            status: Some("blocked".into()),
            session: Some(SID.into()),
        };
        assert!(verdict(&blocked, InputLine::Empty, false).contains("permission prompt"));
        assert!(
            verdict(&ok, InputLine::Empty, true).contains("permission prompt"),
            "the client's own wait"
        );
        assert!(verdict(&ok, InputLine::Missing, false).contains("not on screen"));
        assert!(verdict(&ok, InputLine::Draft, false).contains("draft"));
        // A working agent takes the prompt as its next input, as a tmux send does.
        let working = HerdrAgent {
            status: Some("working".into()),
            session: Some(SID.into()),
        };
        assert_eq!(
            herdr_send_verdict(SID, "w1:p2", &working, InputLine::Empty, false),
            Ok(())
        );
        // Pane ids come from another process's environment: only herdr's own shape passes.
        assert!(herdr_pane_id_ok("w1:p2") && herdr_pane_id_ok("w12:p305"));
        for bad in ["--help", "w1:p", "w:p2", "w1:p2 ", "p2", "w1:p2;x", ""] {
            assert!(!herdr_pane_id_ok(bad), "{bad:?}");
        }
    }

    /// A stand-in herdr world for one send: a `herdr` script that logs its calls and answers
    /// `agent get` and `pane read` from files, and a socket server that takes one `pane.send_text`
    /// and then changes what those files say, as the pane would.
    struct HerdrWorld {
        dir: std::path::PathBuf,
        socket: std::path::PathBuf,
        listener: std::cell::RefCell<Option<std::os::unix::net::UnixListener>>,
        fake: std::path::PathBuf,
        log: std::path::PathBuf,
        agent: std::path::PathBuf,
        screen: std::path::PathBuf,
    }

    impl HerdrWorld {
        fn new(name: &str) -> HerdrWorld {
            use std::os::unix::fs::PermissionsExt;
            // Short: a Unix socket's path must fit in `SUN_LEN` (104 bytes on macOS), and the
            // scratch sits deep in the workspace's target.
            let dir = std::env::temp_dir().join(format!("h{name}{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let socket = dir.join("herdr.sock");
            let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
            let w = HerdrWorld {
                socket,
                listener: std::cell::RefCell::new(Some(listener)),
                fake: dir.join("herdr"),
                log: dir.join("calls.log"),
                agent: dir.join("agent.json"),
                screen: dir.join("screen.txt"),
                dir,
            };
            std::fs::write(
                &w.fake,
                format!(
                    "#!/bin/sh\necho \"$HERDR_SOCKET_PATH $*\" >> '{log}'\ncase \"$1 $2\" in\n  'agent get') if [ -f '{agent}.later' ] && [ -f '{agent}.seen' ]; then cat '{agent}.later'; else touch '{agent}.seen'; cat '{agent}'; fi ;;\n  'pane read') cat '{screen}' ;;\nesac\n",
                    log = w.log.display(),
                    agent = w.agent.display(),
                    screen = w.screen.display(),
                ),
            )
            .unwrap();
            std::fs::set_permissions(&w.fake, std::fs::Permissions::from_mode(0o755)).unwrap();
            w.set(
                &format!(r#"{{"agent_status":"idle","agent_session":{{"value":"{SID}"}}}}"#),
                &screen("\u{276f}\u{a0}\u{1b}[2mTry \"something\"\u{1b}[0m"),
            );
            w
        }
        fn set(&self, agent: &str, screen: &str) {
            std::fs::write(&self.agent, agent).unwrap();
            std::fs::write(&self.screen, screen).unwrap();
        }
        /// Serve one `pane.send_text`; after it, the pane shows `then` (agent, screen).
        fn serve(&self, then: (String, String)) -> std::thread::JoinHandle<String> {
            use std::io::{BufRead, Write};
            let listener = self
                .listener
                .borrow_mut()
                .take()
                .expect("one send per world");
            let (agent, screen) = (self.agent.clone(), self.screen.clone());
            std::thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                std::io::BufReader::new(&stream)
                    .read_line(&mut line)
                    .unwrap();
                std::fs::write(&agent, &then.0).unwrap();
                std::fs::write(&screen, &then.1).unwrap();
                (&stream)
                    .write_all(b"{\"id\":\"agent-monitor-send\",\"result\":{\"type\":\"ok\"}}\n")
                    .unwrap();
                line
            })
        }
        fn send(&self, prompt: &str) -> Result<(), HerdrSendError> {
            herdr_send_with(
                self.fake.as_os_str(),
                self.socket.to_str(),
                "w1:p2",
                SID,
                false,
                prompt,
            )
        }
        fn calls(&self) -> String {
            std::fs::read_to_string(&self.log).unwrap_or_default()
        }
    }

    impl Drop for HerdrWorld {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// #s226 end to end against a stand-in `herdr` and a stand-in herdr server: every step names
    /// the pane's own server by its socket, the checks read the pane, the prompt goes over the
    /// socket as `pane.send_text` (never an argv), the pane is read AGAIN, and one Enter follows; a
    /// draft types nothing at all.
    #[test]
    fn herdr_send_types_over_the_socket_and_presses_enter_once() {
        let w = HerdrWorld::new("s");
        // A draft in the pane: held, and nothing reaches the socket (no server is even listening).
        let idle = format!(r#"{{"agent_status":"idle","agent_session":{{"value":"{SID}"}}}}"#);
        w.set(&idle, &screen("\u{276f}\u{a0}half a thought"));
        let held = w.send("hello");
        assert!(matches!(held, Err(HerdrSendError::Held(ref why)) if why.contains("draft")));
        // An empty input: typed over the socket, seen in the input, then one Enter.
        w.set(
            &idle,
            &screen("\u{276f}\u{a0}\u{1b}[2mTry \"something\"\u{1b}[0m"),
        );
        let server = w.serve((idle.clone(), screen("\u{276f}\u{a0}line one\n  line two")));
        assert!(
            w.send("line one\nline two").is_ok(),
            "the send went through"
        );
        let request: serde_json::Value =
            serde_json::from_str(server.join().unwrap().trim()).unwrap();
        assert_eq!(request["method"], "pane.send_text");
        assert_eq!(request["params"]["pane_id"], "w1:p2");
        assert_eq!(request["params"]["text"], "line one\nline two");
        let calls = w.calls();
        let sock = w.socket.display().to_string();
        assert!(
            calls.lines().all(|l| l.starts_with(&format!("{sock} "))),
            "{calls}"
        );
        assert!(
            !calls.contains("--session"),
            "one way of naming the server: {calls}"
        );
        assert_eq!(
            calls.matches("pane read w1:p2").count(),
            3,
            "the held send's look, then before and after typing: {calls}"
        );
        assert_eq!(
            calls.matches("agent get w1:p2").count(),
            4,
            "the held send's look, then before, just before typing, and after: {calls}"
        );
        assert_eq!(
            calls.matches("pane send-keys w1:p2 enter").count(),
            1,
            "{calls}"
        );
        assert!(
            !calls.contains("line one"),
            "the prompt is never an argv: {calls}"
        );
    }

    /// #s226: a dialog that opens while the prompt is being typed gets no Enter (it would take it
    /// as its first choice), whether herdr says `blocked` with the box still drawn or the box is
    /// gone with herdr still saying idle; the prompt stays in the input and the reader is told so.
    /// One that opens just before typing gets nothing typed. A pane id or a socket that is not
    /// herdr's never reaches the herdr CLI.
    #[test]
    fn herdr_send_presses_no_enter_when_a_dialog_opened_meanwhile() {
        let idle = format!(r#"{{"agent_status":"idle","agent_session":{{"value":"{SID}"}}}}"#);
        let blocked =
            format!(r#"{{"agent_status":"blocked","agent_session":{{"value":"{SID}"}}}}"#);
        let typed = screen("\u{276f}\u{a0}rm the build");
        let dialog = " Do you want to proceed?\n \u{276f}\u{a0}1. Yes\n".to_string();
        for (name, after) in [
            ("b", (blocked.clone(), typed)),
            ("m", (idle.clone(), dialog)),
        ] {
            let w = HerdrWorld::new(name);
            let server = w.serve(after);
            let held = w.send("rm the build");
            assert!(
                matches!(held, Err(HerdrSendError::Held(ref why)) if why.contains("typed") && why.contains("not sent")),
                "{name}"
            );
            server.join().unwrap();
            assert_eq!(
                w.calls().matches("send-keys").count(),
                0,
                "no Enter: {}",
                w.calls()
            );
        }

        // Blocked between the first look and the typing: the ask just before typing holds it,
        // and nothing reaches the socket.
        let w = HerdrWorld::new("p");
        std::fs::write(w.agent.with_extension("json.later"), &blocked).unwrap();
        let held = w.send("rm the build");
        assert!(
            matches!(held, Err(HerdrSendError::Held(ref why)) if why.contains("permission prompt"))
        );
        assert_eq!(w.calls().matches("agent get").count(), 2, "{}", w.calls());
        assert!(
            w.listener.borrow().is_some(),
            "nothing was typed: no one connected"
        );

        let before = w.calls().lines().count();
        let bad = herdr_send_with(
            w.fake.as_os_str(),
            w.socket.to_str(),
            "--help",
            SID,
            false,
            "x",
        );
        assert!(
            matches!(bad, Err(HerdrSendError::Held(ref why)) if why.contains("not a herdr pane id"))
        );
        let elsewhere = w.dir.join("calls.log");
        let not_a_socket = herdr_send_with(
            w.fake.as_os_str(),
            elsewhere.to_str(),
            "w1:p2",
            SID,
            false,
            "x",
        );
        assert!(matches!(not_a_socket, Err(HerdrSendError::Failed(_))));
        let none = herdr_send_with(w.fake.as_os_str(), None, "w1:p2", SID, false, "x");
        assert!(matches!(none, Err(HerdrSendError::Failed(_))));
        assert_eq!(
            w.calls().lines().count(),
            before,
            "the CLI was never run for them"
        );
    }

    /// #s226: nothing in a prompt can act as a key once typed: a carriage return becomes a
    /// newline, and every other control character (an ESC, a bell) goes; tabs and newlines stay.
    #[test]
    fn herdr_text_keeps_no_control_character_but_newline_and_tab() {
        assert_eq!(
            herdr_text("one\r\ntwo\rthree\n\tfour"),
            "one\ntwo\nthree\n\tfour"
        );
        assert_eq!(herdr_text("\u{1b}[A\u{1b}\u{1b}up\u{7}"), "[Aup");
    }

    /// The token lives in the STATE dir at 0600, round-trips, and a second `ensure_token`
    /// is idempotent (#197 — pairing twice does not rotate).
    #[test]
    fn ensure_token_persists_0600_in_state_and_is_idempotent() {
        let state = std::env::temp_dir().join(format!("cm-tok-state-{}", std::process::id()));
        let cache = std::env::temp_dir().join(format!("cm-tok-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        let _ = std::fs::remove_dir_all(&cache);
        let _env = crate::index::StateEnv::set(&state);

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

    /// #s204: a start prints the token only to a terminal. Anywhere else (a LaunchAgent's log, a
    /// pipe, a file) the URL is bare; unpaired there is no token to withhold.
    #[test]
    fn a_start_prints_the_token_to_a_terminal_only() {
        let base = "http://127.0.0.1:2727/";
        assert_eq!(
            printed_url(base, Some("abc"), true),
            "http://127.0.0.1:2727/?token=abc"
        );
        assert_eq!(printed_url(base, Some("abc"), false), base);
        assert_eq!(printed_url(base, None, true), base);
        assert_eq!(printed_url(base, None, false), base);
    }

    /// #197 token migration: a monitor paired under v1.79.x (token in the CACHE) stays paired
    /// after the move — `read_token` finds the cache copy, and `ensure_token` promotes it to
    /// the state path without rotating, leaving the cache copy for a downgrade.
    #[test]
    fn a_cache_token_migrates_to_state_without_re_pairing() {
        let state = std::env::temp_dir().join(format!("cm-mig-state-{}", std::process::id()));
        let cache = std::env::temp_dir().join(format!("cm-mig-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        let _ = std::fs::remove_dir_all(&cache);
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("auth-token"), "legacy-token-xyz").unwrap();
        let _env = crate::index::StateEnv::set(&state);

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

        let _ = std::fs::remove_dir_all(&state);
        let _ = std::fs::remove_dir_all(&cache);
    }
}
