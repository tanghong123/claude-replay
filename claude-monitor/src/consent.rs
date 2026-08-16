//! #133 §3.4 — per-target consent for tmux injection.
//!
//! Writing keystrokes into a live agent's pane is arbitrary command execution — several
//! sessions on this machine run with `--dangerously-skip-permissions`, so injected text is
//! a command run with that user's tools and credentials, no further prompt. "I can write to
//! the pane" is a *permission* fact (filesystem ownership); it is not consent. A grant is
//! the owner authorising ONE target, at a time, and it expires.
//!
//! Consent is keyed by the `(socket, pane, sid, pid)` quadruple. The **pid is load-bearing**:
//! a pane outlives the process in it, so consenting to "pane %0" must never carry to whatever
//! runs there next — a restart mints a new pid, no grant matches it, and the owner re-grants
//! (§3.4). A wall-clock expiry is the backstop for a grant left behind by a long-lived
//! process. Stored 0600 at the monitor's state root beside the auth token and the hide list
//! (#197) — it is security state other local users must not read or forge.

use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Wall-clock backstop for a grant — a workday. The PRIMARY invalidation is the pid changing
/// (a process restart), which no timer can observe; this only bounds a grant whose process is
/// still the same one much later. The owner chose the expiring-GRANT model (§7.2) over
/// per-send confirmation precisely to trade a bounded window of standing consent for the
/// ergonomics of sending freely — so this is generous on purpose, with pid-change as the
/// sharp edge.
const GRANT_TTL_SECS: u64 = 8 * 3600;

/// `<state_dir>/consent.json` — beside `ignored.json` and `auth-token` (#197), not the cache.
pub fn consent_path() -> PathBuf {
    crate::index::state_dir().join("consent.json")
}

/// One authorisation to inject into a specific pane running a specific process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    /// tmux socket basename (`None` = the default socket).
    pub sock: Option<String>,
    /// tmux pane id, e.g. `%3`.
    pub pane: String,
    /// The session this grant authorises sends to.
    pub sid: String,
    /// The EXACT pid consent was granted for — a different pid is a different process, never
    /// this grant (a pane's process was replaced).
    pub pid: u32,
    /// Unix seconds when granted.
    pub granted_at: u64,
    /// Unix seconds after which the grant is dead (the wall-clock backstop).
    pub expires_at: u64,
}

impl Grant {
    fn to_value(&self) -> serde_json::Value {
        json!({
            "sock": self.sock,
            "pane": self.pane,
            "sid": self.sid,
            "pid": self.pid,
            "granted_at": self.granted_at,
            "expires_at": self.expires_at,
        })
    }

    fn from_value(v: &serde_json::Value) -> Option<Grant> {
        Some(Grant {
            sock: v.get("sock").and_then(|s| s.as_str()).map(str::to_string),
            pane: v.get("pane")?.as_str()?.to_string(),
            sid: v.get("sid")?.as_str()?.to_string(),
            pid: u32::try_from(v.get("pid")?.as_u64()?).ok()?,
            granted_at: v.get("granted_at")?.as_u64()?,
            expires_at: v.get("expires_at")?.as_u64()?,
        })
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Is there a live grant for exactly this target? PURE over the grant list — the caller passes
/// the CURRENT pid (from a fresh scan), so a restarted process (a new pid) can never match an
/// old grant, and an expired grant (`expires_at <= now`) never matches. Unit-tested without
/// the clock or the filesystem.
fn granted(
    grants: &[Grant],
    sock: Option<&str>,
    pane: &str,
    sid: &str,
    pid: u32,
    now: u64,
) -> bool {
    grants.iter().any(|g| {
        g.sock.as_deref() == sock
            && g.pane == pane
            && g.sid == sid
            && g.pid == pid
            && g.expires_at > now
    })
}

/// Drop dead grants — housekeeping on every load so the file cannot grow without bound and an
/// expired grant cannot linger.
fn prune(grants: &mut Vec<Grant>, now: u64) {
    grants.retain(|g| g.expires_at > now);
}

/// The consent file, JSON `[{sock,pane,sid,pid,granted_at,expires_at}, …]` at `consent_path()`.
/// Cheap to construct (a path); every op reads the file fresh, so it stays correct across the
/// several routes and the (rare) concurrent request without holding shared state.
pub struct ConsentStore {
    path: PathBuf,
}

impl ConsentStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// A store at the default `consent_path()`.
    pub fn open() -> Self {
        Self::new(consent_path())
    }

    fn load(&self) -> Vec<Grant> {
        let raw = match std::fs::read(&self.path) {
            Ok(b) => b,
            Err(_) => return Vec::new(), // absent or unreadable → no consent (fail closed)
        };
        match serde_json::from_slice::<serde_json::Value>(&raw) {
            Ok(serde_json::Value::Array(items)) => {
                items.iter().filter_map(Grant::from_value).collect()
            }
            _ => Vec::new(),
        }
    }

    fn save(&self, grants: &[Grant]) -> std::io::Result<()> {
        let arr = serde_json::Value::Array(grants.iter().map(Grant::to_value).collect());
        let body = serde_json::to_vec_pretty(&arr).unwrap_or_else(|_| b"[]".to_vec());
        write_0600(&self.path, &body)
    }

    /// Grant (or refresh) consent for a target, replacing any prior grant for the same
    /// `(sock, pane, sid)` — a re-grant after a restart SUPERSEDES the dead one rather than
    /// piling a second row on top. Returns the stored grant. A write failure is surfaced:
    /// a grant that was not persisted would read back as "no consent" on the next send, so
    /// the caller must know it did not take.
    pub fn grant(
        &self,
        sock: Option<&str>,
        pane: &str,
        sid: &str,
        pid: u32,
    ) -> std::io::Result<Grant> {
        let now = now_secs();
        let mut grants = self.load();
        prune(&mut grants, now);
        grants.retain(|g| !(g.sock.as_deref() == sock && g.pane == pane && g.sid == sid));
        let g = Grant {
            sock: sock.map(str::to_string),
            pane: pane.to_string(),
            sid: sid.to_string(),
            pid,
            granted_at: now,
            expires_at: now + GRANT_TTL_SECS,
        };
        grants.push(g.clone());
        self.save(&grants)?;
        Ok(g)
    }

    /// Revoke every grant for a session — the rail's revoke button. Idempotent, and works even
    /// if the session is gone (the point is to remove a stale grant).
    pub fn revoke(&self, sid: &str) {
        let now = now_secs();
        let mut grants = self.load();
        let before = grants.len();
        prune(&mut grants, now);
        grants.retain(|g| g.sid != sid);
        if grants.len() != before {
            let _ = self.save(&grants);
        }
    }

    /// Is this exact target consented right now?
    pub fn is_granted(&self, sock: Option<&str>, pane: &str, sid: &str, pid: u32) -> bool {
        granted(&self.load(), sock, pane, sid, pid, now_secs())
    }

    /// The live grants right now (expired pruned) — the rail marks a row `consented` by
    /// matching a row's own `(sock, pane, sid, pid)` against these, so the badge tracks the
    /// SAME pid the send will check (a restarted process shows as not-consented, honestly).
    pub fn active_grants(&self) -> Vec<Grant> {
        let now = now_secs();
        let mut grants = self.load();
        prune(&mut grants, now);
        grants
    }
}

/// Write bytes to a path 0600 (owner-only). Consent — like the auth token and the hide list —
/// is security state other local users must not read or forge. Shared with the token writer.
pub(crate) fn write_0600(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(bytes)?;
    }
    #[cfg(not(unix))]
    std::fs::write(path, bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(sid: &str, pid: u32, expires_at: u64) -> Grant {
        Grant {
            sock: None,
            pane: "%1".into(),
            sid: sid.into(),
            pid,
            granted_at: 0,
            expires_at,
        }
    }

    /// The matching rule (pure): a grant matches only its exact `(sock,pane,sid,pid)`, and only
    /// while unexpired. The pid is the sharp edge — a restarted process (new pid) never matches.
    #[test]
    fn granted_matches_exact_target_while_unexpired() {
        let grants = vec![g("s", 100, 1000)];
        assert!(granted(&grants, None, "%1", "s", 100, 500)); // exact, unexpired
        assert!(!granted(&grants, None, "%1", "s", 100, 1000)); // expired (now == expires_at)
        assert!(!granted(&grants, None, "%1", "s", 999, 500)); // pid changed → no match
        assert!(!granted(&grants, None, "%2", "s", 100, 500)); // different pane
        assert!(!granted(&grants, None, "%1", "other", 100, 500)); // different sid
        assert!(!granted(&grants, Some("sock"), "%1", "s", 100, 500)); // different socket
    }

    #[test]
    fn prune_drops_only_the_expired() {
        let mut grants = vec![g("live", 1, 1000), g("dead", 2, 100)];
        prune(&mut grants, 500);
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].sid, "live");
    }

    fn store_in(dir: &Path) -> ConsentStore {
        ConsentStore::new(dir.join("consent.json"))
    }

    /// Round-trip through the file: grant → is_granted for the same target, and a DIFFERENT pid
    /// (a restart) reads as not-granted even though the pane/sid are unchanged.
    #[test]
    fn grant_persists_and_pid_change_invalidates() {
        let dir = std::env::temp_dir().join(format!("cr-consent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = store_in(&dir);

        store.grant(Some("knack"), "%3", "sid-a", 4242).unwrap();
        assert!(store.is_granted(Some("knack"), "%3", "sid-a", 4242));
        // pane's process was replaced → the old grant must not carry over.
        assert!(!store.is_granted(Some("knack"), "%3", "sid-a", 9999));
        // a different pane on the same socket is a different target.
        assert!(!store.is_granted(Some("knack"), "%4", "sid-a", 4242));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A re-grant for the same `(sock,pane,sid)` supersedes the old row (no pile-up), and revoke
    /// removes the session's consent.
    #[test]
    fn regrant_supersedes_and_revoke_clears() {
        let dir = std::env::temp_dir().join(format!("cr-consent-re-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = store_in(&dir);

        store.grant(None, "%0", "sid-b", 1).unwrap();
        store.grant(None, "%0", "sid-b", 2).unwrap(); // process restarted, re-granted
        assert_eq!(store.load().len(), 1, "re-grant replaces, not appends");
        assert!(store.is_granted(None, "%0", "sid-b", 2));
        assert!(!store.is_granted(None, "%0", "sid-b", 1)); // old pid is gone

        assert_eq!(store.active_grants().len(), 1);
        store.revoke("sid-b");
        assert!(!store.is_granted(None, "%0", "sid-b", 2));
        assert!(store.active_grants().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
