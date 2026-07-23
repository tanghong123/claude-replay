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
    let mut fallback = None;
    for line in BufReader::new(file).lines().map_while(Result::ok).take(300) {
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if fallback.is_none() {
            fallback = subagent_snippet(&value);
        }
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
            .filter(|text| !crate::codex_model::is_host_context(text))
            .collect::<Vec<_>>()
            .join(" ");
        let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if !compact.is_empty() {
            return compact.chars().take(72).collect();
        }
    }
    fallback.unwrap_or_else(|| "(no user prompt)".to_string())
}

fn subagent_snippet(value: &Value) -> Option<String> {
    if value.get("type").and_then(Value::as_str) != Some("session_meta") {
        return None;
    }
    let payload = value.get("payload")?;
    let is_subagent = payload.get("thread_source").and_then(Value::as_str) == Some("subagent")
        || payload.pointer("/source/subagent").is_some();
    if !is_subagent {
        return None;
    }
    let label = payload
        .get("agent_path")
        .and_then(Value::as_str)
        .or_else(|| {
            payload
                .pointer("/source/subagent/thread_spawn/agent_path")
                .and_then(Value::as_str)
        })
        .and_then(|path| path.trim_end_matches('/').rsplit('/').next())
        .filter(|name| !name.is_empty())
        .or_else(|| payload.get("agent_nickname").and_then(Value::as_str));
    Some(match label {
        Some(label) => format!("↳ subagent {label}"),
        None => "↳ subagent".to_string(),
    })
}

#[cfg_attr(not(test), allow(dead_code))] // exercised by tests with an explicit root
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

/// Codex sessions scoped strictly to `cwd` or its **nearest ancestor that has
/// sessions** — no global fallback (so a session for an unrelated directory never
/// leaks into another directory's picker).
pub(crate) fn candidates_scoped(cwd: &Path) -> Vec<Candidate> {
    candidates_scoped_in(&sessions_dir(), cwd)
}

/// Same scoping as `candidates_scoped`, but keeping each session's **id** (the
/// `Candidate` drops it) — `(id, mtime, snippet)`, newest-first. For `resume`'s
/// stale-confirm picker, which needs the id to resume the chosen one.
pub(crate) fn sessions_for_cwd(cwd: &Path) -> Vec<(String, SystemTime, String)> {
    let root = sessions_dir();
    let sessions = sessions_in(&root); // newest-first
    for anc in crate::discover::ancestors_of(cwd) {
        let anc_n = normalized(&anc);
        let matched: Vec<&CodexSession> = sessions
            .iter()
            .filter(|s| normalized(&s.cwd) == anc_n)
            .collect();
        if matched.is_empty() {
            continue;
        }
        return matched
            .into_iter()
            .map(|s| (s.id.clone(), s.mtime, first_user_snippet(&s.path)))
            .collect();
    }
    Vec::new()
}

fn candidates_scoped_in(root: &Path, cwd: &Path) -> Vec<Candidate> {
    let sessions = sessions_in(root); // newest-first
    let cwd_n = normalized(cwd);
    for anc in crate::discover::ancestors_of(cwd) {
        let anc_n = normalized(&anc);
        let matched: Vec<&CodexSession> = sessions
            .iter()
            .filter(|s| normalized(&s.cwd) == anc_n)
            .collect();
        if matched.is_empty() {
            continue;
        }
        let is_exact = anc_n == cwd_n;
        return matched
            .into_iter()
            .map(|s| {
                let project = s
                    .cwd
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("session")
                    .to_string();
                Candidate {
                    path: s.path.clone(),
                    mtime: s.mtime,
                    project,
                    snippet: first_user_snippet(&s.path),
                    cwd_affinity: is_exact,
                    agent: Agent::Codex,
                }
            })
            .collect();
    }
    Vec::new()
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

/// The session id of the newest rollout whose contents contain `marker` (a nonce
/// embedded in a fresh-run prompt) — used by `agent-jdi start` to recover the id
/// Codex assigned. Scans newest-first and stops at the first match.
pub(crate) fn session_id_with_marker(marker: &str) -> Option<String> {
    for s in sessions_in(&sessions_dir()) {
        let Ok(file) = File::open(&s.path) else {
            continue;
        };
        for line in BufReader::new(file).lines().map_while(Result::ok).take(300) {
            if line.contains(marker) {
                return Some(s.id);
            }
        }
    }
    None
}

/// The newest Codex session recorded for `cwd` **or its nearest ancestor that has
/// sessions** — never a session from an unrelated directory (no global fallback).
/// Used by the `agent-jdi` Codex adapter to pick a resume target, so `resume` in a
/// directory with no Codex history fails cleanly instead of hijacking some other
/// project's session.
pub(crate) fn latest_for_cwd(cwd: &Path) -> Option<CodexSession> {
    latest_for_cwd_in(&sessions_dir(), cwd)
}

fn latest_for_cwd_in(root: &Path, cwd: &Path) -> Option<CodexSession> {
    let sessions = sessions_in(root); // newest-first
    for anc in crate::discover::ancestors_of(cwd) {
        let anc_n = normalized(&anc);
        // `sessions` is newest-first, so the first match at this ancestor is newest.
        if let Some(s) = sessions.iter().find(|s| normalized(&s.cwd) == anc_n) {
            return Some(s.clone());
        }
    }
    None
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
            Self::append_user(&path, message);
            path
        }

        fn subagent_rollout(&self, id: &str, cwd: &Path, agent_path: &str) -> PathBuf {
            let path = self.rollout("2026/07/18", id, cwd, "codex-tui");
            let meta = serde_json::json!({
                "timestamp": "2026-07-18T01:00:00Z",
                "type": "session_meta",
                "payload": {
                    "id": id,
                    "cwd": cwd,
                    "originator": "codex-tui",
                    "source": {
                        "subagent": {
                            "thread_spawn": {
                                "parent_thread_id": "parent-session",
                                "depth": 1,
                                "agent_path": agent_path,
                                "agent_nickname": "Nash"
                            }
                        }
                    },
                    "thread_source": "subagent",
                    "agent_path": agent_path,
                    "agent_nickname": "Nash",
                    "cli_version": "test"
                }
            });
            fs::write(&path, format!("{meta}\n")).unwrap();
            path
        }

        fn append_user(path: &Path, message: &str) {
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
                fs::OpenOptions::new().append(true).open(path).unwrap(),
                "{user}"
            )
            .unwrap();
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

    #[test]
    fn picker_snippet_skips_host_context_messages() {
        let fixture = Fixture::new();
        let cwd = fixture.root.join("repo");
        let path = fixture.rollout("2026/07/18", "abc", &cwd, "codex-tui");
        for context in [
            "# AGENTS.md instructions for /repo\n<INSTRUCTIONS>...</INSTRUCTIONS>",
            "<recommended_plugins>available but not installed</recommended_plugins>",
            "<environment_context><cwd>/repo</cwd></environment_context>",
            "<permissions instructions>read only</permissions instructions>",
        ] {
            Fixture::append_user(&path, context);
        }
        Fixture::append_user(&path, "Fix the parser carefully");

        let candidates = candidates_in(&fixture.sessions, &cwd);

        assert_eq!(candidates[0].snippet, "Fix the parser carefully");
    }

    #[test]
    fn picker_snippet_labels_subagent_without_user_prompt() {
        let fixture = Fixture::new();
        let cwd = fixture.root.join("repo");
        let path = fixture.subagent_rollout("abc", &cwd, "/root/review_picker_fix");
        Fixture::append_user(
            &path,
            "<recommended_plugins>available but not installed</recommended_plugins>",
        );

        let candidates = candidates_in(&fixture.sessions, &cwd);

        assert_eq!(candidates[0].snippet, "↳ subagent review_picker_fix");
    }

    #[test]
    fn picker_snippet_labels_regular_session_without_user_prompt() {
        let fixture = Fixture::new();
        let cwd = fixture.root.join("repo");
        let path = fixture.rollout("2026/07/18", "abc", &cwd, "codex-tui");
        Fixture::append_user(
            &path,
            "<recommended_plugins>available but not installed</recommended_plugins>",
        );

        let candidates = candidates_in(&fixture.sessions, &cwd);

        assert_eq!(candidates[0].snippet, "(no user prompt)");
    }

    #[test]
    fn scoped_does_not_leak_sessions_from_unrelated_dirs() {
        let fixture = Fixture::new();
        let repo_a = fixture.root.join("a");
        let repo_b = fixture.root.join("b");
        fs::create_dir_all(&repo_a).unwrap();
        fs::create_dir_all(&repo_b).unwrap();
        // A session only for repo_a.
        fixture.rollout("2026/07/20", "sa", &repo_a, "codex-tui");

        // From repo_b (and its ancestors), repo_a's session must NOT show.
        assert!(
            candidates_scoped_in(&fixture.sessions, &repo_b).is_empty(),
            "a sibling dir's session leaked in"
        );
        // From repo_a itself, it shows (exact cwd → affinity).
        let here = candidates_scoped_in(&fixture.sessions, &repo_a);
        assert_eq!(here.len(), 1);
        assert!(here[0].cwd_affinity);
    }

    #[test]
    fn latest_for_cwd_never_returns_an_unrelated_dirs_session() {
        let fixture = Fixture::new();
        let repo_a = fixture.root.join("a");
        let repo_b = fixture.root.join("b");
        fs::create_dir_all(&repo_a).unwrap();
        fs::create_dir_all(&repo_b).unwrap();
        // The only Codex session anywhere belongs to repo_a.
        fixture.rollout("2026/07/20", "sa", &repo_a, "codex-tui");

        // From repo_b (no session of its own, no ancestor with one under the root),
        // the resume target must be None — NOT repo_a's session.
        assert!(
            latest_for_cwd_in(&fixture.sessions, &repo_b).is_none(),
            "leaked a sibling dir's session as the resume target"
        );
        // From repo_a itself it resolves.
        assert_eq!(
            latest_for_cwd_in(&fixture.sessions, &repo_a).map(|s| s.id),
            Some("sa".to_string())
        );
    }
}
