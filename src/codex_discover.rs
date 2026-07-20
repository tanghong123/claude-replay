use crate::discover::Candidate;
use crate::Agent;
use anyhow::{anyhow, Result};
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone)]
pub(crate) struct CodexSession {
    pub id: String,
    pub path: PathBuf,
    pub cwd: PathBuf,
    pub mtime: SystemTime,
}

pub(crate) fn codex_home() -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            home.join(".codex")
        })
}

pub(crate) fn sessions_dir() -> PathBuf {
    std::env::var_os("CODEX_SESSIONS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| codex_home().join("sessions"))
}

fn jsonl_files(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                walk(&path, out);
            } else if kind.is_file()
                && path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
            {
                out.push(path);
            }
        }
    }

    let mut out = Vec::new();
    walk(root, &mut out);
    out
}

fn session_from_path(path: &Path) -> Option<CodexSession> {
    let file = File::open(path).ok()?;
    for line in BufReader::new(file).lines().map_while(Result::ok).take(100) {
        // Skip noise lines rather than abandoning the whole session on the first
        // non-JSON line (a leading blank/comment before `session_meta`).
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("session_meta") {
            continue;
        }
        let payload = value.get("payload")?;
        let id = payload
            .get("id")
            .or_else(|| payload.get("session_id"))?
            .as_str()?
            .to_string();
        let cwd = PathBuf::from(payload.get("cwd")?.as_str()?);
        let mtime = std::fs::metadata(path)
            .and_then(|meta| meta.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        return Some(CodexSession {
            id,
            path: path.to_path_buf(),
            cwd,
            mtime,
        });
    }
    None
}

fn sessions_in(root: &Path) -> Vec<CodexSession> {
    let mut sessions: Vec<_> = jsonl_files(root)
        .iter()
        .filter_map(|path| session_from_path(path))
        .collect();
    sessions.sort_by_key(|session| std::cmp::Reverse(session.mtime));
    sessions
}

fn normalized(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn first_user_snippet(path: &Path) -> String {
    let Ok(file) = File::open(path) else {
        return String::new();
    };
    for line in BufReader::new(file).lines().map_while(Result::ok).take(300) {
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("response_item")
            || value.pointer("/payload/type").and_then(Value::as_str) != Some("message")
            || value.pointer("/payload/role").and_then(Value::as_str) != Some("user")
        {
            continue;
        }
        let text = value
            .pointer("/payload/content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("input_text"))
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(" ");
        let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if !compact.is_empty() {
            return compact.chars().take(72).collect();
        }
    }
    String::new()
}

pub(crate) fn candidates_in(root: &Path, cwd: &Path) -> Vec<Candidate> {
    let wanted = normalized(cwd);
    let mut out: Vec<_> = sessions_in(root)
        .into_iter()
        .map(|session| {
            let cwd_affinity = normalized(&session.cwd) == wanted;
            let project = session
                .cwd
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("session")
                .to_string();
            Candidate {
                path: session.path.clone(),
                mtime: session.mtime,
                project,
                snippet: first_user_snippet(&session.path),
                cwd_affinity,
                agent: Agent::Codex,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.cwd_affinity
            .cmp(&a.cwd_affinity)
            .then(b.mtime.cmp(&a.mtime))
    });
    out
}

pub(crate) fn candidates() -> Vec<Candidate> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    candidates_in(&sessions_dir(), &cwd)
}

pub(crate) fn resolve_in(root: &Path, target: Option<&str>, latest: bool) -> Result<PathBuf> {
    if let Some(target) = target {
        let path = PathBuf::from(target);
        if path.is_file() {
            return Ok(path);
        }
        if let Some(session) = sessions_in(root)
            .into_iter()
            .find(|session| session.id == target)
        {
            return Ok(session.path);
        }
        return Err(anyhow!(
            "no Codex transcript found for '{target}' under {}",
            root.display()
        ));
    }
    if latest {
        return sessions_in(root)
            .into_iter()
            .next()
            .map(|session| session.path)
            .ok_or_else(|| anyhow!("no Codex transcripts found under {}", root.display()));
    }
    Err(anyhow!(
        "give a Codex session id or rollout path, or use --latest"
    ))
}

pub(crate) fn resolve(target: Option<&str>, latest: bool) -> Result<PathBuf> {
    resolve_in(&sessions_dir(), target, latest)
}

/// The newest Codex session recorded for `cwd` (exact cwd match preferred, else the
/// newest overall). Used by the `agent-jdi` Codex adapter to pick a resume target.
pub(crate) fn latest_for_cwd(cwd: &Path) -> Option<CodexSession> {
    let wanted = normalized(cwd);
    let all = sessions_in(&sessions_dir());
    all.iter()
        .find(|s| normalized(&s.cwd) == wanted)
        .or_else(|| all.first())
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    struct Fixture {
        root: PathBuf,
        sessions: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "codex-replay-discover-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            fs::remove_dir_all(&root).ok();
            let sessions = root.join("sessions");
            fs::create_dir_all(&sessions).unwrap();
            Self { root, sessions }
        }

        fn rollout(&self, day: &str, id: &str, cwd: &Path, originator: &str) -> PathBuf {
            fs::create_dir_all(cwd).unwrap();
            let dir = self.sessions.join(day);
            fs::create_dir_all(&dir).unwrap();
            let path = dir.join(format!("rollout-{id}.jsonl"));
            let meta = serde_json::json!({
                "timestamp": "2026-07-18T01:00:00Z",
                "type": "session_meta",
                "payload": {
                    "id": id,
                    "cwd": cwd,
                    "originator": originator,
                    "source": "cli",
                    "cli_version": "test"
                }
            });
            fs::write(&path, format!("{meta}\n")).unwrap();
            path
        }

        fn rollout_with_user(&self, id: &str, cwd: &Path, message: &str) -> PathBuf {
            let path = self.rollout("2026/07/18", id, cwd, "codex-tui");
            let user = serde_json::json!({
                "timestamp": "2026-07-18T01:00:01Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": message}]
                }
            });
            use std::io::Write;
            writeln!(
                fs::OpenOptions::new().append(true).open(&path).unwrap(),
                "{user}"
            )
            .unwrap();
            path
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).ok();
        }
    }

    #[test]
    fn resolves_uuid_and_first_user_snippet() {
        let fixture = Fixture::new();
        let cwd = fixture.root.join("repo");
        let path = fixture.rollout_with_user("abc", &cwd, "Fix the parser carefully");
        assert_eq!(
            resolve_in(&fixture.sessions, Some("abc"), false).unwrap(),
            path
        );
        let candidates = candidates_in(&fixture.sessions, &cwd);
        assert_eq!(candidates[0].snippet, "Fix the parser carefully");
    }
}
