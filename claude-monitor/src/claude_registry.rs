//! Which session a plain `claude` process is running, from the record Claude Code keeps for it,
//! `<claude home>/sessions/<pid>.json` (observed on macOS with Claude Code 2.1.309–2.1.310; not a
//! published format). A plain `claude` names no session in its argv, so without the record a
//! process that went back to an older session lost its pid to the newest transcript in its
//! directory. The record names the session (`sessionId`) and the process (`pid`, and `procStart`,
//! the output of `LC_ALL=C TZ=UTC ps -o lstart= -p <pid>`, which the client runs on every platform).
//! A record counts only while that pid still has that start, so a record left by an earlier
//! process with a reused pid names nothing. No peer credential file is read.

use serde_json::Value;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

/// The client's own bound on the record it writes (262144 bytes).
const RECORD_LIMIT: u64 = 256 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Identity {
    /// No usable record: none written (an older client), or one that could not be verified
    /// (a pid or start that does not match, a file caught mid-write, a file over the bound).
    /// Discovery then works as it did before the record was read.
    #[default]
    Missing,
    /// A record this process wrote, naming the session it runs now.
    Verified(String),
}

impl Identity {
    pub(crate) fn session(&self) -> Option<&str> {
        match self {
            Self::Verified(session) => Some(session),
            Self::Missing => None,
        }
    }

    /// Whether this process may be the one writing `session`: anything without a record may.
    pub(crate) fn permits(&self, session: &str) -> bool {
        self.session()
            .is_none_or(|registered| registered == session)
    }
}

/// Read the record of each `(pid, sessions directory)` pair, for processes already known to be
/// Claude. The starts come from one bounded `ps`; a process `ps` does not report reads Missing.
pub(crate) fn scan(targets: &[(u32, PathBuf)]) -> HashMap<u32, Identity> {
    if targets.is_empty() {
        return HashMap::new();
    }
    // SAFETY: geteuid has no preconditions and only reads this process's effective UID. It is
    // `libc`'s, so the UID is known without creating a probe file in a scratch directory.
    let uid = unsafe { libc::geteuid() };
    let pids: Vec<u32> = targets.iter().map(|(pid, _)| *pid).collect();
    let starts = process_starts(&pids, uid);
    targets
        .iter()
        .map(|(pid, dir)| {
            let identity = read(dir, *pid, uid, starts.get(pid).map(String::as_str))
                .map_or(Identity::Missing, Identity::Verified);
            (*pid, identity)
        })
        .collect()
}

/// What one look at a record found.
enum Attempt {
    /// Nothing usable there: no file, not ours, not a plain file, or over the bound.
    Unusable,
    /// Bytes that are not JSON: the client may be rewriting the file in place.
    Partial,
    Record(Value),
}

fn read(dir: &Path, pid: u32, uid: u32, start: Option<&str>) -> Option<String> {
    let path = dir.join(format!("{pid}.json"));
    retry_partial(|| attempt(&path, uid)).and_then(|record| validated_session(&record, pid, start))
}

/// A record caught mid-write is read once more; a second partial read counts as no record.
fn retry_partial(mut attempt: impl FnMut() -> Attempt) -> Option<Value> {
    let first = match attempt() {
        Attempt::Partial => attempt(),
        other => other,
    };
    match first {
        Attempt::Record(value) => Some(value),
        Attempt::Unusable | Attempt::Partial => None,
    }
}

fn attempt(path: &Path, uid: u32) -> Attempt {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::OpenOptionsExt;
    let Ok(before) = std::fs::symlink_metadata(path) else {
        return Attempt::Unusable;
    };
    if !before.is_file() || before.uid() != uid || before.len() > RECORD_LIMIT {
        return Attempt::Unusable;
    }
    // Metadata inspection cannot prevent a pathname replacement between the look and the open.
    // O_NOFOLLOW refuses a symlink put there, O_NONBLOCK keeps a FIFO put there from blocking the
    // scan (it holds the index's lock), and the opened descriptor must be the inode inspected.
    let Ok(file) = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    else {
        return Attempt::Unusable;
    };
    let Ok(opened) = file.metadata() else {
        return Attempt::Unusable;
    };
    if !opened.is_file()
        || opened.uid() != uid
        || opened.dev() != before.dev()
        || opened.ino() != before.ino()
    {
        return Attempt::Unusable;
    }
    let mut bytes = Vec::new();
    if file.take(RECORD_LIMIT + 1).read_to_end(&mut bytes).is_err()
        || bytes.len() as u64 > RECORD_LIMIT
    {
        return Attempt::Unusable;
    }
    serde_json::from_slice(&bytes).map_or(Attempt::Partial, Attempt::Record)
}

fn validated_session(record: &Value, pid: u32, start: Option<&str>) -> Option<String> {
    let start = start.filter(|s| !s.trim().is_empty())?;
    // Compared word by word: `ps` pads a single-digit day ("Oct  4"), and the record keeps it.
    if record.get("pid")?.as_u64()? != u64::from(pid)
        || !record
            .get("procStart")?
            .as_str()?
            .split_whitespace()
            .eq(start.split_whitespace())
    {
        return None;
    }
    let session = record.get("sessionId")?.as_str()?;
    crate::index::is_uuid(session).then(|| session.to_string())
}

/// Each pid's start as `ps -o lstart=` prints it in the C locale and UTC, the form the client
/// stores, on macOS and Linux alike. Bounded like every scan probe (`run_bounded`): a `ps` that
/// does not answer gives no starts, and every record reads Missing for that scan.
fn process_starts(pids: &[u32], uid: u32) -> HashMap<u32, String> {
    let pids = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let out = crate::index::run_bounded_env(
        &["ps", "-p", &pids, "-o", "pid=,uid=,lstart="],
        &[("TZ", "UTC"), ("LC_ALL", "C")],
        crate::index::SCAN_PROBE_LIMIT,
    );
    parse_process_starts(&out, uid)
}

/// `pid uid lstart` lines → each start of a process this user owns.
fn parse_process_starts(text: &str, uid: u32) -> HashMap<u32, String> {
    text.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let pid = words.next()?.parse().ok()?;
            let owner = words.next()?.parse::<u32>().ok()?;
            let start = words.collect::<Vec<_>>().join(" ");
            (owner == uid && !start.is_empty()).then_some((pid, start))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SID: &str = "11111111-2222-4333-8444-555555555555";

    /// A record as Claude Code 2.1.310 writes it: `procStart` is `ps -o lstart=` output.
    fn client_record(pid: u32, start: &str) -> String {
        json!({
            "pid": pid,
            "sessionId": SID,
            "cwd": "/fixture/project",
            "startedAt": 1_761_387_736_000_u64,
            "procStart": start,
            "kind": "interactive",
            "entrypoint": "cli",
        })
        .to_string()
    }

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

    /// The client stores `ps -o lstart=` text on Linux too; procps and BSD `ps` both print the
    /// C-locale form, a single-digit day padded with a space.
    #[test]
    fn a_client_record_matches_ps_lstart_on_either_platform() {
        let record: Value =
            serde_json::from_str(&client_record(42, "Sun Oct  4 03:43:32 2026")).unwrap();
        let macos = parse_process_starts(" 42   501 Sun Oct  4 03:43:32 2026\n", 501);
        let linux = parse_process_starts("     42  1000 Sun Oct  4 03:43:32 2026\n", 1000);
        assert_eq!(
            validated_session(&record, 42, macos.get(&42).map(String::as_str)),
            Some(SID.into())
        );
        assert_eq!(
            validated_session(&record, 42, linux.get(&42).map(String::as_str)),
            Some(SID.into())
        );
        // A clock-tick start (what /proc/<pid>/stat holds) never matches the client's text.
        assert_eq!(validated_session(&record, 42, Some("987654")), None);
    }

    #[test]
    fn ps_lines_of_other_users_and_noise_are_ignored() {
        let starts = parse_process_starts(
            " 42 501 Sun Oct  4 03:43:32 2026\n43 502 Sun Oct  4 03:43:32 2026\ninvalid\n44 501\n",
            501,
        );
        assert_eq!(starts.len(), 1);
        assert_eq!(starts[&42], "Sun Oct 4 03:43:32 2026");
    }

    #[test]
    fn registry_files_are_bounded_owned_regular_files() {
        use std::os::unix::fs::{symlink, MetadataExt};
        let root = std::env::temp_dir().join(format!("cm-native-registry-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let uid = std::fs::metadata(&root).unwrap().uid();
        let path = root.join("42.json");
        assert_eq!(read(&root, 42, uid, Some("start")), None);
        let bytes = json!({"pid":42,"sessionId":SID,"procStart":"start"}).to_string();
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(read(&root, 42, uid, Some("start")), Some(SID.into()));
        assert_eq!(read(&root, 42, uid + 1, Some("start")), None);
        assert_eq!(read(&root, 42, uid, Some("other")), None);
        std::fs::write(&path, "{").unwrap();
        assert_eq!(read(&root, 42, uid, Some("start")), None);
        // Up to the client's own bound a record is read; past it, it is not.
        let padded = |len: usize| {
            let mut record = bytes.clone();
            record.pop();
            record.push_str(",\"pad\":\"");
            let fill = len - record.len() - 2;
            record.push_str(&"x".repeat(fill));
            record.push_str("\"}");
            record
        };
        std::fs::write(&path, padded(RECORD_LIMIT as usize)).unwrap();
        assert_eq!(read(&root, 42, uid, Some("start")), Some(SID.into()));
        std::fs::write(&path, padded(RECORD_LIMIT as usize + 1)).unwrap();
        assert_eq!(read(&root, 42, uid, Some("start")), None);
        std::fs::remove_file(&path).unwrap();
        std::fs::write(root.join("real.json"), bytes).unwrap();
        symlink(root.join("real.json"), &path).unwrap();
        assert_eq!(read(&root, 42, uid, Some("start")), None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_record_caught_mid_write_is_read_once_more() {
        let record = || Attempt::Record(json!({"pid": 42}));
        let mut seq = vec![Attempt::Partial, record()].into_iter();
        assert!(retry_partial(|| seq.next().unwrap()).is_some());
        let mut seq = vec![Attempt::Partial, Attempt::Partial, record()].into_iter();
        assert!(
            retry_partial(|| seq.next().unwrap()).is_none(),
            "a second partial read is no record"
        );
        let mut calls = 0;
        assert!(retry_partial(|| {
            calls += 1;
            Attempt::Unusable
        })
        .is_none());
        assert_eq!(calls, 1, "only a partial read is retried");
    }

    #[test]
    fn replacing_an_inspected_record_with_a_symlink_or_fifo_is_refused() {
        let root = std::env::temp_dir().join(format!("cm-registry-swap-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("42.json");
        std::fs::write(root.join("moved.json"), "{}").unwrap();
        std::os::unix::fs::symlink(root.join("moved.json"), &path).unwrap();
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        assert!(matches!(attempt(&path, uid), Attempt::Unusable));
        std::fs::remove_file(&path).unwrap();
        let fifo = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: fifo is a valid NUL-terminated path, and this test owns its scratch directory.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(
            matches!(attempt(&path, uid), Attempt::Unusable),
            "a FIFO is refused without waiting for a writer"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The whole scan against this test process, with a record whose `procStart` comes from the
    /// client's own command rather than from `process_starts`, and with no writable scratch.
    #[test]
    fn registry_scan_verifies_a_record_written_with_the_clients_own_command() {
        const PROBE: &str = "CM_REGISTRY_SCAN_PROBE";
        if let Some(root) = std::env::var_os(PROBE) {
            let root = PathBuf::from(root);
            let pid = std::process::id();
            let targets = [(pid, root.clone())];
            assert_eq!(scan(&targets).get(&pid), Some(&Identity::Missing));
            let out = std::process::Command::new("ps")
                .args(["-o", "lstart=", "-p", &pid.to_string()])
                .env("LC_ALL", "C")
                .env("TZ", "UTC")
                .output()
                .unwrap();
            let start = String::from_utf8_lossy(&out.stdout).trim().to_string();
            assert!(!start.is_empty());
            std::fs::write(root.join(format!("{pid}.json")), client_record(pid, &start)).unwrap();
            assert_eq!(
                scan(&targets).get(&pid),
                Some(&Identity::Verified(SID.into()))
            );
            return;
        }
        let root = std::env::temp_dir().join(format!("cm-registry-scan-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        // A fresh child, so the unavailable TMPDIR is its own.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "claude_registry::tests::registry_scan_verifies_a_record_written_with_the_clients_own_command",
            ])
            .env(PROBE, &root)
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
