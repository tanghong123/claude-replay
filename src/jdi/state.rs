//! Per-session state: the `<home>/<id>/` directory, its line-oriented `meta`
//! key=value file (atomic set), lifecycle state, and slot identity from a cwd.
//! Agent-neutral — a port of claude-jdi's `meta_*` / `dir_session_id` helpers.

use anyhow::{Context, Result};
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// Lifecycle state written to `meta`'s `state=` key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Starting,
    Running,
    Retrying,
    Stopped,
    Done,
    Failed,
    GaveUp,
}

impl RunState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Retrying => "retrying",
            Self::Stopped => "stopped",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::GaveUp => "gaveup",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "starting" => Self::Starting,
            "running" => Self::Running,
            "retrying" => Self::Retrying,
            "stopped" => Self::Stopped,
            "done" => Self::Done,
            "failed" => Self::Failed,
            "gaveup" => Self::GaveUp,
            _ => return None,
        })
    }
}

/// A per-session state directory `<home>/<id>/`.
pub struct Session {
    pub dir: PathBuf,
}

impl Session {
    pub fn new(home: &Path, id: &str) -> Self {
        Self { dir: home.join(id) }
    }

    pub fn exists(&self) -> bool {
        self.meta_path().is_file()
    }

    pub fn meta_path(&self) -> PathBuf {
        self.dir.join("meta")
    }
    pub fn cargs_path(&self) -> PathBuf {
        self.dir.join("cargs")
    }
    pub fn supervisor_log(&self) -> PathBuf {
        self.dir.join("supervisor.log")
    }
    pub fn output_log(&self) -> PathBuf {
        self.dir.join("output.log")
    }
    pub fn backlog_root(&self) -> PathBuf {
        self.dir.join("backlog")
    }

    pub fn ensure_dir(&self) -> Result<()> {
        fs::create_dir_all(&self.dir)
            .with_context(|| format!("create state dir {}", self.dir.display()))
    }

    /// First value for `key` in `meta`, if present (matches claude-jdi's
    /// `sed … | head -1`).
    pub fn meta_get(&self, key: &str) -> Option<String> {
        let content = fs::read_to_string(self.meta_path()).ok()?;
        let prefix = format!("{key}=");
        content
            .lines()
            .find_map(|l| l.strip_prefix(&prefix).map(str::to_string))
    }

    /// Set `key=value` atomically: drop any prior lines for `key`, append the new
    /// one, write to a temp file, then rename over `meta`.
    pub fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        self.ensure_dir()?;
        let path = self.meta_path();
        let prefix = format!("{key}=");
        let mut lines: Vec<String> = fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.starts_with(&prefix))
            .map(str::to_string)
            .collect();
        lines.push(format!("{key}={value}"));
        let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
        fs::write(&tmp, format!("{}\n", lines.join("\n")))
            .with_context(|| format!("write {}", tmp.display()))?;
        fs::rename(&tmp, &path).with_context(|| format!("rename into {}", path.display()))?;
        Ok(())
    }

    pub fn state(&self) -> Option<RunState> {
        self.meta_get("state").and_then(|s| RunState::parse(&s))
    }

    /// The supervisor pid recorded in `meta`, if any.
    pub fn pid(&self) -> Option<u32> {
        self.meta_get("pid").and_then(|p| p.parse().ok())
    }

    /// Is the supervisor process alive? (`kill -0`, unix.)
    pub fn alive(&self) -> bool {
        match self.pid() {
            Some(pid) => pid_alive(pid),
            None => false,
        }
    }
}

/// `kill -0 <pid>` → true if the process exists (and we may signal it). Unix only;
/// elsewhere conservatively reports not-alive.
pub fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// A fresh UUID-v4-shaped run id — used as Claude's pinned `--session-id` and as
/// the Codex capture nonce. Not cryptographic; just needs to be unique per run.
pub fn new_run_id() -> String {
    use std::hash::{Hash, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static CTR: AtomicU64 = AtomicU64::new(0);
    let mix = |salt: u64| {
        let mut h = DefaultHasher::new();
        std::time::SystemTime::now().hash(&mut h);
        std::process::id().hash(&mut h);
        CTR.fetch_add(1, Ordering::Relaxed).hash(&mut h);
        salt.hash(&mut h);
        h.finish()
    };
    let mut b = [0u8; 16];
    b[..8].copy_from_slice(&mix(0x9e37).to_le_bytes());
    b[8..].copy_from_slice(&mix(0x1234).to_le_bytes());
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // variant
    let h = |r: std::ops::Range<usize>| b[r].iter().map(|x| format!("{x:02x}")).collect::<String>();
    format!(
        "{}-{}-{}-{}-{}",
        h(0..4),
        h(4..6),
        h(6..8),
        h(8..10),
        h(10..16)
    )
}

/// One tracked slot per project directory: `<sanitized-basename>-<6-hex hash>`.
/// (Our own key — distinct from claude-jdi's sha1-based key, which keeps the two
/// tools' state dirs from colliding; the deprecation check computes the legacy key
/// separately.)
pub fn slot_id(cwd: &Path) -> String {
    let base = cwd
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("session");
    let base: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let mut h = DefaultHasher::new();
    cwd.to_string_lossy().hash(&mut h);
    let hash: String = format!("{:016x}", h.finish())[..6].to_string();
    format!("{base}-{hash}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "agent-jdi-state-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::remove_dir_all(&d).ok();
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn meta_set_get_is_atomic_and_last_wins() {
        let home = tmp();
        let s = Session::new(&home, "sess");
        assert_eq!(s.meta_get("state"), None);
        s.meta_set("state", "running").unwrap();
        s.meta_set("pid", "1234").unwrap();
        s.meta_set("state", "done").unwrap(); // overwrite, not duplicate
        assert_eq!(s.state(), Some(RunState::Done));
        assert_eq!(s.pid(), Some(1234));
        // Exactly one state line remains.
        let content = fs::read_to_string(s.meta_path()).unwrap();
        assert_eq!(content.matches("state=").count(), 1, "meta:\n{content}");
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn slot_id_is_stable_and_dir_scoped() {
        let a = slot_id(Path::new("/Users/dev/projects/foo"));
        let b = slot_id(Path::new("/Users/dev/projects/foo"));
        let c = slot_id(Path::new("/Users/dev/projects/bar"));
        assert_eq!(a, b, "same dir → same slot");
        assert_ne!(a, c, "different dir → different slot");
        assert!(a.starts_with("foo-"), "keeps basename: {a}");
    }

    #[test]
    fn new_run_id_is_uuid_v4_shaped_and_unique() {
        let id = new_run_id();
        let parts: Vec<usize> = id.split('-').map(str::len).collect();
        assert_eq!(parts, vec![8, 4, 4, 4, 12], "uuid layout: {id}");
        assert!(
            id.chars().all(|c| c == '-' || c.is_ascii_hexdigit()),
            "hex only: {id}"
        );
        assert_eq!(id.as_bytes()[14], b'4', "version nibble: {id}");
        assert_ne!(new_run_id(), new_run_id(), "ids are unique");
    }

    #[test]
    fn pid_alive_tracks_our_own_process() {
        assert!(pid_alive(std::process::id()), "our own pid is alive");
        // pid 0 / a very high pid is (almost surely) not a signalable process.
        assert!(!pid_alive(999_999_999));
    }
}
