//! Which session started which, as the Claude desktop app records it (#s340, Hong's yes on
//! #s342). The desktop app shows a session started from another (an agent calling
//! `start_session`, or a "btw" side question the person forks) as its child in its sidebar, and
//! nothing the monitor otherwise reads says so: the transcript names no starter, and its id
//! appears only inside paths and command text, which are never matched (#269).
//!
//! The app keeps one JSON record per Code-tab session at
//! `<store>/<account uuid>/<org uuid>/local_<uuid>.json` (measured with Claude desktop 4.4212.0
//! hosting Claude Code 2.1.309, on the machine the task was filed from): `sessionId` is the
//! desktop's id (`local_<uuid>`), `cliSessionId` the transcript's, which is the monitor's row id,
//! and a child carries `spawnedFrom.sessionId`, its starter's desktop id. The store is the app's
//! own and undocumented, so it is read read-only and tolerantly: a file that is not a record, too
//! large, unreadable or malformed is skipped, unknown keys are ignored, and nothing here can fail
//! a scan. Only the three ids are kept.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// A record bigger than this is not read: the ids sit at its top, and the largest measured was a
/// few kilobytes, but a record can carry a side session's report.
const RECORD_LIMIT: u64 = 1 << 20;

/// Where the desktop app keeps its session records: `CLAUDE_DESKTOP_SESSIONS_DIR` when set (a
/// test's hermetic store), else `~/Library/Application Support/Claude/claude-code-sessions` on
/// macOS. Elsewhere there is no desktop app, so nothing is read.
pub fn store_root() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_DESKTOP_SESSIONS_DIR") {
        return Some(PathBuf::from(dir));
    }
    // A unit test that scans never reads this machine's store (the review of #s340).
    if cfg!(test) || !cfg!(target_os = "macos") {
        return None;
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Application Support/Claude/claude-code-sessions"))
}

/// One record's ids: the desktop's own, the transcript's, and its starter's desktop id.
#[derive(Clone, Debug, PartialEq)]
struct Record {
    desktop: String,
    cli: Option<String>,
    starter: Option<String>,
}

/// The records read so far, each kept with the modification time and length it was read at, so a
/// scan re-reads only a record that changed. A file that did not read as a record is not kept, so
/// one caught half-written is read again at the next refresh.
#[derive(Default)]
pub struct DesktopSessions {
    read: HashMap<PathBuf, (Option<SystemTime>, u64, Record)>,
}

impl DesktopSessions {
    /// Every side session the store names whose starter it also names: the child's transcript id
    /// to its starter's. A record with no transcript id, or whose starter's record is gone or has
    /// none, links nothing.
    pub fn links(&mut self, root: Option<&Path>) -> HashMap<String, String> {
        let Some(root) = root else {
            self.read.clear();
            return HashMap::new();
        };
        let mut seen = HashSet::new();
        for path in record_paths(root) {
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            let (modified, len) = (meta.modified().ok(), meta.len());
            seen.insert(path.clone());
            if let Some((was_modified, was_len, _)) = self.read.get(&path) {
                if *was_modified == modified && *was_len == len {
                    continue;
                }
            }
            let record = (len <= RECORD_LIMIT)
                .then(|| std::fs::read(&path).ok())
                .flatten()
                .and_then(|bytes| parse_record(&bytes));
            match record {
                Some(record) => self.read.insert(path, (modified, len, record)),
                None => self.read.remove(&path),
            };
        }
        self.read.retain(|path, _| seen.contains(path));
        link_records(self.read.iter().map(|(path, (.., r))| (path, r)))
    }
}

/// The record files: `local_*.json` exactly two directories below the store (account, then
/// organization). Other files there (`scheduled-tasks.json`, measured on aries-black) are not
/// session records.
fn record_paths(root: &Path) -> Vec<PathBuf> {
    let dirs = |dir: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_dir())
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut out = Vec::new();
    for account in dirs(root) {
        for org in dirs(&account) {
            let Ok(entries) = std::fs::read_dir(&org) else {
                continue;
            };
            out.extend(entries.flatten().map(|e| e.path()).filter(|p| {
                p.extension().is_some_and(|e| e == "json")
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("local_"))
            }));
        }
    }
    out
}

/// A record's three ids, or `None` for a file that is not a session record.
fn parse_record(bytes: &[u8]) -> Option<Record> {
    let v: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let text = |v: Option<&serde_json::Value>| {
        v.and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    Some(Record {
        desktop: text(v.get("sessionId"))?,
        cli: text(v.get("cliSessionId")),
        starter: text(v.pointer("/spawnedFrom/sessionId")),
    })
}

/// The child-to-starter links between transcript ids, through the desktop ids. Records are taken
/// in the order of their desktop ids, then their paths, and the first to name a transcript (or a
/// desktop id) decides, so two records naming one never make its link flip between scans.
fn link_records<'a>(
    records: impl Iterator<Item = (&'a PathBuf, &'a Record)>,
) -> HashMap<String, String> {
    let mut records: Vec<(&PathBuf, &Record)> = records.collect();
    records.sort_by(|(pa, a), (pb, b)| a.desktop.cmp(&b.desktop).then_with(|| pa.cmp(pb)));
    let records: Vec<&Record> = records.into_iter().map(|(_, r)| r).collect();
    let mut cli_of: HashMap<&str, &str> = HashMap::new();
    for r in &records {
        if let Some(cli) = r.cli.as_deref() {
            cli_of.entry(r.desktop.as_str()).or_insert(cli);
        }
    }
    let mut links = HashMap::new();
    for r in &records {
        let (Some(child), Some(starter)) = (r.cli.as_deref(), r.starter.as_deref()) else {
            continue;
        };
        if let Some(starter) = cli_of.get(starter).filter(|s| **s != child) {
            links
                .entry(child.to_string())
                .or_insert_with(|| starter.to_string());
        }
    }
    links
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A record as the desktop app writes it, in the measured shape (synthetic ids).
    fn record(desktop: &str, cli: Option<&str>, starter: Option<&str>) -> String {
        let mut v = serde_json::json!({
            "sessionId": desktop,
            "cwd": "/w/harbor",
            "title": "a session",
            "model": "claude-opus",
            "isArchived": false,
        });
        if let Some(cli) = cli {
            v["cliSessionId"] = serde_json::json!(cli);
        }
        if let Some(starter) = starter {
            v["spawnedFrom"] = serde_json::json!({
                "sessionId": starter,
                "taskId": "t1",
                "title": "a side question",
                "sideSession": true,
                "btw": true,
                "userFork": true,
            });
        }
        v.to_string()
    }

    /// #s340: a side session's record names its starter's desktop id, which the starter's record
    /// maps to its transcript id; everything else in the store links nothing and breaks nothing.
    #[test]
    fn a_desktop_side_session_links_to_the_session_that_started_it() {
        let root = std::env::temp_dir().join(format!("cm-desktop-{}", std::process::id()));
        let org = root.join("acct-0000").join("org-0000");
        std::fs::create_dir_all(&org).unwrap();
        let write = |name: &str, body: &str| std::fs::write(org.join(name), body).unwrap();
        write("local_aaaa.json", &record("local_aaaa", Some("1111"), None));
        write(
            "local_bbbb.json",
            &record("local_bbbb", Some("2222"), Some("local_aaaa")),
        );
        // A child whose starter has no transcript id, one whose starter's record is gone, one
        // with no transcript id itself, a file that is not JSON, a JSON file that is not a
        // record, and the store's other files: no link from any of them.
        write("local_cccc.json", &record("local_cccc", None, None));
        write(
            "local_dddd.json",
            &record("local_dddd", Some("4444"), Some("local_cccc")),
        );
        write(
            "local_eeee.json",
            &record("local_eeee", Some("5555"), Some("local_gone")),
        );
        write(
            "local_ffff.json",
            &record("local_ffff", None, Some("local_aaaa")),
        );
        write("local_bad.json", "{not json");
        write("local_list.json", "[1, 2]");
        write("scheduled-tasks.json", r#"{"scheduledTasks": []}"#);
        std::fs::write(
            root.join("acct-0000").join("local_shallow.json"),
            record("local_shallow", Some("6666"), Some("local_aaaa")),
        )
        .unwrap();

        let mut store = DesktopSessions::default();
        let links = store.links(Some(&root));
        assert_eq!(
            links,
            HashMap::from([("2222".to_string(), "1111".to_string())])
        );
        // A record that changes is read again; one that goes is forgotten.
        // (A byte longer, since a coarse clock may give the rewrite the same time.)
        write(
            "local_dddd.json",
            &format!(
                "{} ",
                record("local_dddd", Some("4444"), Some("local_aaaa"))
            ),
        );
        assert_eq!(
            store.links(Some(&root)).get("4444").map(String::as_str),
            Some("1111")
        );
        std::fs::remove_file(org.join("local_bbbb.json")).unwrap();
        assert!(!store.links(Some(&root)).contains_key("2222"));
        // A record caught half-written is read again at the next refresh, even when the finished
        // file has the same length and time (a coarse clock), which the cache alone would miss.
        let whole = record("local_gggg", Some("7777"), Some("local_aaaa"));
        let half = format!(
            "{}{}",
            &whole[..whole.len() / 2],
            " ".repeat(whole.len() - whole.len() / 2)
        );
        let at = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        let pinned = |body: &str| {
            write("local_gggg.json", body);
            std::fs::File::options()
                .write(true)
                .open(org.join("local_gggg.json"))
                .unwrap()
                .set_modified(at)
                .unwrap();
        };
        pinned(&half);
        assert!(!store.links(Some(&root)).contains_key("7777"));
        pinned(&whole);
        assert_eq!(
            store.links(Some(&root)).get("7777").map(String::as_str),
            Some("1111")
        );
        // No store at all, or none on this machine: nothing.
        assert!(store.links(Some(&root.join("missing"))).is_empty());
        assert!(store.links(None).is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}
