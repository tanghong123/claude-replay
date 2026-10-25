//! Claude's live process/session registrations, observed in Claude Code 2.1.309–2.1.310.
//! These are runtime evidence, not transcript recency: a PID is trusted only while its
//! owner and OS process-start identity match the registration. No peer credentials are read.

use serde_json::Value;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Identity {
    /// Older Claude versions and installations without a registration retain legacy discovery.
    #[default]
    Missing,
    Verified(String),
    /// An existing record could not be validated. It must never authorize a guessed pane.
    Rejected,
}

impl Identity {
    pub(crate) fn session(&self) -> Option<&str> {
        match self {
            Self::Verified(session) => Some(session),
            _ => None,
        }
    }

    pub(crate) fn permits(&self, session: &str) -> bool {
        match self {
            Self::Missing => true,
            Self::Verified(registered) => registered == session,
            Self::Rejected => false,
        }
    }
}

fn directory() -> PathBuf {
    if let Some(config) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return PathBuf::from(config).join("sessions");
    }
    // A relocated transcript store, including a test fixture, must not borrow live identities
    // from the user's ordinary store. Claude's sessions and projects are sibling directories.
    if let Some(projects) = std::env::var_os("CLAUDE_PROJECTS_DIR") {
        return PathBuf::from(projects)
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("sessions");
    }
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".claude/sessions")
}

/// Read registrations only for already discovered Claude processes. The process table is
/// queried in UTC/C locale because macOS Claude stores `ps lstart` in that representation.
pub(crate) fn scan(pids: &[u32]) -> HashMap<u32, Identity> {
    // SAFETY: geteuid has no preconditions and only reads this process's effective UID.
    // Registry discovery must not depend on being able to write a temporary probe file.
    let uid = unsafe { libc::geteuid() };
    let root = directory();
    let starts = process_starts(pids, uid);
    pids.iter()
        .map(|pid| {
            let identity = read(&root, *pid, uid, starts.get(pid).map(String::as_str));
            (*pid, identity)
        })
        .collect()
}

fn read(root: &Path, pid: u32, uid: u32, start: Option<&str>) -> Identity {
    use std::os::unix::fs::MetadataExt;
    let path = root.join(format!("{pid}.json"));
    let before = match std::fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Identity::Missing,
        Err(_) => return Identity::Rejected,
    };
    if !before.is_file() || before.uid() != uid || before.len() > 64 * 1024 {
        return Identity::Rejected;
    }
    load_record(&path, &before, uid)
        .and_then(|record| validated_session(&record, pid, start))
        .map_or(Identity::Rejected, Identity::Verified)
}

fn load_record(path: &Path, before: &std::fs::Metadata, uid: u32) -> Option<Value> {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::OpenOptionsExt;
    // Metadata inspection cannot prevent a pathname replacement. Refuse symlinks at open,
    // and do not block if a regular entry was replaced with a FIFO before descriptor checks.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let opened = file.metadata().ok()?;
    if !opened.is_file()
        || opened.uid() != uid
        || opened.dev() != before.dev()
        || opened.ino() != before.ino()
    {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(64 * 1024 + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() > 64 * 1024 {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

fn validated_session(record: &Value, pid: u32, start: Option<&str>) -> Option<String> {
    let start = start.filter(|s| !s.is_empty())?;
    if record.get("pid")?.as_u64()? != u64::from(pid) || record.get("procStart")?.as_str()? != start
    {
        return None;
    }
    let session = record.get("sessionId")?.as_str()?;
    crate::index::is_uuid(session).then(|| session.to_string())
}

#[cfg(target_os = "macos")]
fn process_starts(pids: &[u32], uid: u32) -> HashMap<u32, String> {
    let pids = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    if pids.is_empty() {
        return HashMap::new();
    }
    let Ok(output) = std::process::Command::new("ps")
        .args(["-p", &pids, "-o", "pid=,uid=,lstart="])
        .env("TZ", "UTC")
        .env("LC_ALL", "C")
        .output()
    else {
        return HashMap::new();
    };
    parse_process_starts(&String::from_utf8_lossy(&output.stdout), uid)
}

#[cfg(any(target_os = "macos", test))]
fn parse_process_starts(text: &str, uid: u32) -> HashMap<u32, String> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let at = line.find(char::is_whitespace)?;
            let pid = line[..at].parse().ok()?;
            let rest = line[at..].trim_start();
            let at = rest.find(char::is_whitespace)?;
            (rest[..at].parse::<u32>().ok()? == uid).then(|| (pid, rest[at..].trim().to_string()))
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn process_starts(pids: &[u32], uid: u32) -> HashMap<u32, String> {
    use std::os::unix::fs::MetadataExt;
    pids.iter()
        .filter_map(|pid| {
            let path = PathBuf::from(format!("/proc/{pid}/stat"));
            if std::fs::metadata(&path).ok()?.uid() != uid {
                return None;
            }
            let stat = std::fs::read_to_string(path).ok()?;
            Some((*pid, linux_start(&stat)?.to_string()))
        })
        .collect()
}

#[cfg(any(target_os = "linux", test))]
fn linux_start(stat: &str) -> Option<&str> {
    // Field 2 is a parenthesized command name and can itself contain spaces and ')'.
    let after_name = stat.rsplit_once(')')?.1;
    let start = after_name.split_whitespace().nth(19)?;
    start.parse::<u64>().ok()?;
    Some(start)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn process_starts(_: &[u32], _: u32) -> HashMap<u32, String> {
    HashMap::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SID: &str = "11111111-2222-4333-8444-555555555555";

    #[test]
    fn native_record_requires_the_exact_pid_and_process_start() {
        let record = json!({"pid": 42, "sessionId": SID, "procStart": "start"});
        assert_eq!(
            validated_session(&record, 42, Some("start")),
            Some(SID.into())
        );
        for (pid, start) in [(43, Some("start")), (42, Some("reused")), (42, None)] {
            assert_eq!(validated_session(&record, pid, start), None);
        }
        for invalid in [
            json!({"pid":"42","sessionId":SID,"procStart":"start"}),
            json!({"pid":42,"sessionId":"../other","procStart":"start"}),
            json!({"pid":42,"sessionId":SID}),
            json!({"pid":42,"sessionId":SID,"procStart":""}),
            json!([]),
        ] {
            assert_eq!(validated_session(&invalid, 42, Some("start")), None);
        }
    }

    #[test]
    fn registry_files_are_bounded_owned_regular_files() {
        use std::os::unix::fs::{symlink, MetadataExt};
        let root = std::env::temp_dir().join(format!("cm-native-registry-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let uid = std::fs::metadata(&root).unwrap().uid();
        let path = root.join("42.json");
        assert_eq!(read(&root, 42, uid, Some("start")), Identity::Missing);
        let bytes = json!({"pid":42,"sessionId":SID,"procStart":"start"}).to_string();
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(
            read(&root, 42, uid, Some("start")),
            Identity::Verified(SID.into())
        );
        assert_eq!(read(&root, 42, uid + 1, Some("start")), Identity::Rejected);
        assert_eq!(read(&root, 42, uid, Some("other")), Identity::Rejected);
        std::fs::write(&path, "{").unwrap();
        assert_eq!(read(&root, 42, uid, Some("start")), Identity::Rejected);
        std::fs::write(&path, " ".repeat(64 * 1024 + 1)).unwrap();
        assert_eq!(read(&root, 42, uid, Some("start")), Identity::Rejected);
        std::fs::remove_file(&path).unwrap();
        std::fs::write(root.join("real.json"), bytes).unwrap();
        symlink(root.join("real.json"), &path).unwrap();
        assert_eq!(read(&root, 42, uid, Some("start")), Identity::Rejected);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn platform_start_parsers_keep_exact_identity_and_ignore_other_users() {
        let starts = parse_process_starts(
            " 42 501 Sun Oct  4 03:43:32 2026\n43 502 Sun Oct  4 03:43:32 2026\ninvalid\n",
            501,
        );
        assert_eq!(starts.len(), 1);
        assert_eq!(starts[&42], "Sun Oct  4 03:43:32 2026");
        let stat = "42 (claude ) worker) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 987654 20";
        assert_eq!(linux_start(stat), Some("987654"));
        assert_eq!(linux_start("42 (claude) S 1"), None);
    }

    #[test]
    fn replacing_an_inspected_record_with_a_symlink_is_rejected() {
        use std::os::unix::fs::{symlink, MetadataExt};
        let root = std::env::temp_dir().join(format!("cm-registry-swap-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("42.json");
        std::fs::write(&path, "{}").unwrap();
        let before = std::fs::symlink_metadata(&path).unwrap();
        let moved = root.join("moved.json");
        std::fs::rename(&path, &moved).unwrap();
        symlink(moved, &path).unwrap();
        assert!(
            load_record(&path, &before, before.uid()).is_none(),
            "an unchanged inode does not authorize following a replacement symlink"
        );
        std::fs::remove_file(&path).unwrap();
        let fifo = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: fifo is a valid NUL-terminated path, and this test owns its scratch directory.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(
            load_record(&path, &before, before.uid()).is_none(),
            "a replacement FIFO is rejected without waiting for a writer"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn registry_scan_does_not_require_writable_scratch() {
        const PROBE: &str = "CM_REGISTRY_NO_SCRATCH_PROBE";
        if std::env::var_os(PROBE).is_some() {
            let pid = std::process::id();
            assert_eq!(scan(&[pid]).get(&pid), Some(&Identity::Missing));
            // Verify a present record too: Missing alone would pass even if UID/start
            // discovery failed before validation. The fixture does not use TMPDIR.
            let root = directory();
            std::fs::create_dir_all(&root).unwrap();
            // SAFETY: geteuid has no preconditions and only reads the child's effective UID.
            let start = process_starts(&[pid], unsafe { libc::geteuid() });
            let sid = "11111111-2222-4333-8444-555555555555";
            let record = serde_json::json!({
                "pid": pid, "sessionId": sid, "procStart": start[&pid],
            });
            std::fs::write(root.join(format!("{pid}.json")), record.to_string()).unwrap();
            assert_eq!(
                scan(&[pid]).get(&pid),
                Some(&Identity::Verified(sid.into()))
            );
            return;
        }
        let root =
            std::env::temp_dir().join(format!("cm-registry-no-scratch-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        // A fresh child isolates both the environment and the UID helper's cached state.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "claude_registry::tests::registry_scan_does_not_require_writable_scratch",
            ])
            .env(PROBE, "1")
            .env("CLAUDE_CONFIG_DIR", &root)
            .env("TMPDIR", root.join("unavailable/scratch"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
