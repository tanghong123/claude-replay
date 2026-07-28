use crate::discover::Candidate;
use crate::session_graph::SessionGraphBackend;
use crate::Agent;
use anyhow::{anyhow, Result};
use serde_json::Value;
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone)]
pub struct CodexSession {
    pub id: String,
    pub path: PathBuf,
    pub cwd: PathBuf,
    pub mtime: SystemTime,
}

#[derive(Debug, Clone)]
struct CodexGraphNode {
    id: String,
    path: PathBuf,
    parent_thread_id: Option<String>,
    agent_path: Option<String>,
    agent_nickname: Option<String>,
}

pub(crate) struct CodexSessionGraph {
    sessions_root: PathBuf,
    anchor_path: PathBuf,
    anchor_id: Option<String>,
    nodes: Vec<CodexGraphNode>,
}

impl CodexSessionGraph {
    pub(crate) fn open(anchor: &Path) -> Self {
        let root = anchor
            .ancestors()
            .find(|path| path.file_name().and_then(|name| name.to_str()) == Some("sessions"))
            .map(Path::to_path_buf)
            .unwrap_or_else(sessions_dir);
        Self::open_in(&root, anchor)
    }

    pub(crate) fn open_in(sessions_root: &Path, anchor: &Path) -> Self {
        let nodes = graph_nodes_in(sessions_root);
        let anchor_id = nodes
            .iter()
            .find(|node| same_path(&node.path, anchor))
            .map(|node| node.id.clone())
            .or_else(|| graph_node_from_path(anchor).map(|node| node.id));
        Self {
            sessions_root: sessions_root.to_path_buf(),
            anchor_path: anchor.to_path_buf(),
            anchor_id,
            nodes,
        }
    }

    fn refresh(&mut self) {
        self.nodes = graph_nodes_in(&self.sessions_root);
        if self.anchor_id.is_none() {
            self.anchor_id = self
                .nodes
                .iter()
                .find(|node| same_path(&node.path, &self.anchor_path))
                .map(|node| node.id.clone())
                .or_else(|| graph_node_from_path(&self.anchor_path).map(|node| node.id));
        }
    }

    fn source_id(&self, source: &Path) -> Option<String> {
        self.nodes
            .iter()
            .find(|node| same_path(&node.path, source))
            .map(|node| node.id.clone())
            .or_else(|| graph_node_from_path(source).map(|node| node.id))
            .filter(|id| self.is_reachable(id))
    }

    fn is_reachable(&self, id: &str) -> bool {
        let Some(anchor) = self.anchor_id.as_deref() else {
            return false;
        };
        let mut current = id;
        let mut seen = HashSet::new();
        loop {
            if current == anchor {
                return true;
            }
            if !seen.insert(current.to_string()) {
                return false;
            }
            let Some(parent) = self
                .nodes
                .iter()
                .find(|node| node.id == current)
                .and_then(|node| node.parent_thread_id.as_deref())
            else {
                return false;
            };
            current = parent;
        }
    }

    /// Resolve all relationship-bearing blocks and return stable identities for
    /// unresolved spawns. The caller uses those identities to budget at most one
    /// sessions-root refresh per newly observed spawn.
    fn resolve_blocks(&self, source: &Path, blocks: &mut [crate::Block]) -> Vec<(String, String)> {
        let Some(source_id) = self.source_id(source) else {
            return Vec::new();
        };
        let children: Vec<_> = self
            .nodes
            .iter()
            .filter(|node| {
                node.parent_thread_id.as_deref() == Some(source_id.as_str()) && node.path.is_file()
            })
            .collect();

        for block in blocks.iter_mut() {
            match block {
                crate::Block::SubAgent(agent) if agent.agent_id.is_empty() => {
                    let matches: Vec<_> = children
                        .iter()
                        .copied()
                        .filter(|node| child_matches_description(node, &agent.description))
                        .collect();
                    if let [child] = matches.as_slice() {
                        agent.agent_id.clone_from(&child.id);
                        agent.agent_type = child
                            .agent_nickname
                            .clone()
                            .unwrap_or_else(|| "agent".to_string());
                    }
                }
                crate::Block::AgentDone {
                    agent_id,
                    agent_type,
                    ..
                } => {
                    let matches: Vec<_> = children
                        .iter()
                        .copied()
                        .filter(|node| node.agent_path.as_deref() == Some(agent_id.as_str()))
                        .collect();
                    if let [child] = matches.as_slice() {
                        agent_id.clone_from(&child.id);
                        *agent_type = child
                            .agent_nickname
                            .clone()
                            .unwrap_or_else(|| "agent".to_string());
                    }
                }
                _ => {}
            }
        }

        blocks
            .iter()
            .filter_map(|block| match block {
                crate::Block::SubAgent(agent) if agent.agent_id.is_empty() => Some((
                    source_id.clone(),
                    if agent.tool_use_id.is_empty() {
                        agent.description.clone()
                    } else {
                        agent.tool_use_id.clone()
                    },
                )),
                _ => None,
            })
            .collect()
    }
}

impl SessionGraphBackend for CodexSessionGraph {
    fn resolve(&mut self, source: &Path, blocks: &mut [crate::Block]) {
        // A live follower can open an empty rollout before `session_meta` is
        // appended. Recover the anchor as soon as that metadata becomes visible,
        // so the later child refresh remains scoped to the correct tree.
        if self.anchor_id.is_none() && graph_node_from_path(source).is_some() {
            self.refresh();
        }
        if !self.resolve_blocks(source, blocks).is_empty() {
            // `resolve` is only called after a batch parse or when FollowParser observes new
            // source bytes, never on an idle tick. Refreshing here therefore catches a child
            // rollout created after its spawn without continuously rescanning the store.
            self.refresh();
            self.resolve_blocks(source, blocks);
        }
    }

    fn subagent_source(&mut self, _root: &Path, child_id: &str) -> Option<PathBuf> {
        self.nodes
            .iter()
            .find(|node| {
                node.id == child_id
                    && self.anchor_id.as_deref() != Some(child_id)
                    && self.is_reachable(child_id)
                    && node.path.is_file()
            })
            .map(|node| node.path.clone())
    }
}

fn graph_nodes_in(root: &Path) -> Vec<CodexGraphNode> {
    jsonl_files(root)
        .iter()
        .filter_map(|path| graph_node_from_path(path))
        .collect()
}

fn graph_node_from_path(path: &Path) -> Option<CodexGraphNode> {
    let file = File::open(path).ok()?;
    for line in BufReader::new(file).lines().map_while(Result::ok).take(100) {
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
        let spawn = payload.pointer("/source/subagent/thread_spawn");
        let string = |key: &str| {
            payload
                .get(key)
                .and_then(Value::as_str)
                .or_else(|| {
                    spawn
                        .and_then(|value| value.get(key))
                        .and_then(Value::as_str)
                })
                .map(str::to_string)
        };
        return Some(CodexGraphNode {
            id,
            path: path.to_path_buf(),
            parent_thread_id: string("parent_thread_id"),
            agent_path: string("agent_path"),
            agent_nickname: string("agent_nickname"),
        });
    }
    None
}

fn same_path(left: &Path, right: &Path) -> bool {
    left == right
        || std::fs::canonicalize(left)
            .ok()
            .zip(std::fs::canonicalize(right).ok())
            .is_some_and(|(left, right)| left == right)
}

fn child_matches_description(node: &CodexGraphNode, description: &str) -> bool {
    node.agent_path.as_deref().is_some_and(|path| {
        path == description
            || path
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .is_some_and(|leaf| leaf == description)
    })
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

/// The **nearest ancestor of `cwd`** (walking up the directory chain) that owns any Codex
/// sessions, and its sessions — the "no global fallback" scoping every scoped lookup shares.
/// Returns those sessions (newest-first, borrowed from `sessions`) plus whether the match was
/// `cwd` itself (exact — used for picker affinity). Empty if no ancestor up to the root owns one.
fn nearest_ancestor_sessions<'a>(
    sessions: &'a [CodexSession],
    cwd: &Path,
) -> (Vec<&'a CodexSession>, bool) {
    let cwd_n = normalized(cwd);
    for anc in crate::discover::ancestors_of(cwd) {
        let anc_n = normalized(&anc);
        let matched: Vec<&CodexSession> = sessions
            .iter()
            .filter(|s| normalized(&s.cwd) == anc_n)
            .collect();
        if !matched.is_empty() {
            return (matched, anc_n == cwd_n);
        }
    }
    (Vec::new(), false)
}

/// Same scoping as `candidates_scoped`, but keeping each session's **id** (the
/// `Candidate` drops it) — `(id, mtime, snippet)`, newest-first. For `resume`'s
/// stale-confirm picker, which needs the id to resume the chosen one.
pub fn sessions_for_cwd(cwd: &Path) -> Vec<(String, SystemTime, String)> {
    let sessions = sessions_in(&sessions_dir()); // newest-first
    let (matched, _) = nearest_ancestor_sessions(&sessions, cwd);
    matched
        .into_iter()
        .map(|s| (s.id.clone(), s.mtime, first_user_snippet(&s.path)))
        .collect()
}

fn candidates_scoped_in(root: &Path, cwd: &Path) -> Vec<Candidate> {
    let sessions = sessions_in(root); // newest-first
    let (matched, is_exact) = nearest_ancestor_sessions(&sessions, cwd);
    matched
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
        .collect()
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

pub fn resolve(target: Option<&str>, latest: bool) -> Result<PathBuf> {
    resolve_in(&sessions_dir(), target, latest)
}

/// The session id of the newest rollout whose contents contain `marker` (a nonce
/// embedded in a fresh-run prompt) — used by `agent-jdi start` to recover the id
/// Codex assigned. Scans newest-first and stops at the first match.
pub fn session_id_with_marker(marker: &str) -> Option<String> {
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
pub fn latest_for_cwd(cwd: &Path) -> Option<CodexSession> {
    latest_for_cwd_in(&sessions_dir(), cwd)
}

fn latest_for_cwd_in(root: &Path, cwd: &Path) -> Option<CodexSession> {
    let sessions = sessions_in(root); // newest-first
                                      // The nearest ancestor's matches are newest-first, so the first is the latest.
    nearest_ancestor_sessions(&sessions, cwd)
        .0
        .first()
        .map(|s| (*s).clone())
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

        fn graph_rollout(
            &self,
            id: &str,
            cwd: &Path,
            parent_thread_id: Option<&str>,
            agent_path: Option<&str>,
            agent_nickname: Option<&str>,
        ) -> PathBuf {
            fs::create_dir_all(cwd).unwrap();
            let dir = self.sessions.join("2026/07/23");
            fs::create_dir_all(&dir).unwrap();
            let path = dir.join(format!("rollout-{id}.jsonl"));
            let source = parent_thread_id.map(|parent| {
                serde_json::json!({
                    "subagent": {
                        "thread_spawn": {
                            "parent_thread_id": parent,
                            "agent_path": agent_path,
                            "agent_nickname": agent_nickname
                        }
                    }
                })
            });
            let meta = serde_json::json!({
                "type": "session_meta",
                "payload": {
                    "id": id,
                    "cwd": cwd,
                    "source": source.unwrap_or_else(|| serde_json::json!("cli")),
                    "agent_path": agent_path,
                    "agent_nickname": agent_nickname
                }
            });
            fs::write(&path, format!("{meta}\n")).unwrap();
            path
        }

        fn append_spawn(path: &Path, task_name: &str, author: Option<&str>) {
            use std::io::Write;
            let call = serde_json::json!({
                "type": "response_item",
                "payload": {
                    "type": "function_call",
                    "name": "spawn_agent",
                    "namespace": "collaboration",
                    "call_id": "call-spawn",
                    "arguments": serde_json::json!({
                        "task_name": task_name,
                        "message": "encrypted"
                    }).to_string()
                }
            });
            let output = serde_json::json!({
                "type": "response_item",
                "payload": {
                    "type": "function_call_output",
                    "call_id": "call-spawn",
                    "output": serde_json::json!({
                        "task_name": format!("/root/review/{task_name}")
                    }).to_string()
                }
            });
            let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
            writeln!(file, "{call}").unwrap();
            writeln!(file, "{output}").unwrap();
            if let Some(author) = author {
                let completion = serde_json::json!({
                    "type": "response_item",
                    "payload": {
                        "type": "agent_message",
                        "author": author,
                        "content": [{
                            "type": "input_text",
                            "text": "Message Type: FINAL_ANSWER\nPayload:\nPASS"
                        }]
                    }
                });
                writeln!(file, "{completion}").unwrap();
            }
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

    #[test]
    fn graph_resolves_direct_child_id_nickname_source_and_completion() {
        let fixture = Fixture::new();
        let cwd = fixture.root.join("repo");
        let parent = fixture.graph_rollout("p", &cwd, None, None, None);
        Fixture::append_spawn(&parent, "spec_axis", Some("/root/review/spec_axis"));
        let child = fixture.graph_rollout(
            "c",
            &cwd,
            Some("p"),
            Some("/root/review/spec_axis"),
            Some("Hume"),
        );
        fixture.graph_rollout(
            "g",
            &cwd,
            Some("c"),
            Some("/root/review/spec_axis/audit"),
            Some("Nash"),
        );
        let graph = crate::SessionGraph::from_backend(Box::new(CodexSessionGraph::open_in(
            &fixture.sessions,
            &parent,
        )));
        let mut blocks = crate::engine::parse_session_as(crate::Agent::Codex, &parent)
            .unwrap()
            .blocks();

        graph.resolve_relationships(&parent, &mut blocks);

        let crate::Block::SubAgent(spawn) = &blocks[0] else {
            panic!("expected spawn");
        };
        assert_eq!(spawn.agent_id, "c");
        assert_eq!(spawn.agent_type, "Hume");
        assert_eq!(
            spawn.status,
            crate::AgentStatus::Running,
            "spawn events keep their launch status"
        );
        assert!(matches!(
            &blocks[1],
            crate::Block::AgentDone { agent_id, .. } if agent_id == "c"
        ));
        assert_eq!(
            crate::engine::build_sub_agents(&blocks)["c"].status,
            crate::AgentStatus::Completed,
            "the session map derives terminal status from AgentDone"
        );
        assert_eq!(graph.subagent_source(&parent, "c"), Some(child));
    }

    #[test]
    fn graph_leaves_ambiguous_or_missing_spawns_unlinked() {
        let fixture = Fixture::new();
        let cwd = fixture.root.join("repo");
        let parent = fixture.graph_rollout("p", &cwd, None, None, None);
        Fixture::append_spawn(&parent, "spec_axis", None);
        fixture.graph_rollout(
            "c1",
            &cwd,
            Some("p"),
            Some("/root/one/spec_axis"),
            Some("One"),
        );
        fixture.graph_rollout(
            "c2",
            &cwd,
            Some("p"),
            Some("/root/two/spec_axis"),
            Some("Two"),
        );
        let graph = crate::SessionGraph::from_backend(Box::new(CodexSessionGraph::open_in(
            &fixture.sessions,
            &parent,
        )));
        let mut blocks = crate::engine::parse_session_as(crate::Agent::Codex, &parent)
            .unwrap()
            .blocks();

        graph.resolve_relationships(&parent, &mut blocks);

        assert!(matches!(
            &blocks[0],
            crate::Block::SubAgent(spawn) if spawn.agent_id.is_empty()
        ));

        let missing_parent = fixture.graph_rollout("missing-p", &cwd, None, None, None);
        Fixture::append_spawn(&missing_parent, "not_created", None);
        let missing_graph = crate::SessionGraph::from_backend(Box::new(
            CodexSessionGraph::open_in(&fixture.sessions, &missing_parent),
        ));
        let mut missing = crate::engine::parse_session_as(crate::Agent::Codex, &missing_parent)
            .unwrap()
            .blocks();
        missing_graph.resolve_relationships(&missing_parent, &mut missing);
        assert!(matches!(
            &missing[0],
            crate::Block::SubAgent(spawn) if spawn.agent_id.is_empty()
        ));
    }

    #[test]
    fn graph_refreshes_when_child_appears_after_open() {
        let fixture = Fixture::new();
        let cwd = fixture.root.join("repo");
        let parent = fixture.graph_rollout("p", &cwd, None, None, None);
        Fixture::append_spawn(&parent, "late_child", None);
        let graph = crate::SessionGraph::from_backend(Box::new(CodexSessionGraph::open_in(
            &fixture.sessions,
            &parent,
        )));
        let mut blocks = crate::engine::parse_session_as(crate::Agent::Codex, &parent)
            .unwrap()
            .blocks();
        let child = fixture.graph_rollout(
            "late",
            &cwd,
            Some("p"),
            Some("/root/review/late_child"),
            Some("Late"),
        );
        graph.resolve_relationships(&parent, &mut blocks);

        assert!(matches!(
            &blocks[0],
            crate::Block::SubAgent(spawn)
                if spawn.agent_id == "late" && spawn.agent_type == "Late"
        ));
        assert_eq!(graph.subagent_source(&parent, "late"), Some(child));
    }

    #[test]
    fn graph_rejects_session_ids_outside_the_anchored_tree() {
        let fixture = Fixture::new();
        let cwd = fixture.root.join("repo");
        let parent = fixture.graph_rollout("p", &cwd, None, None, None);
        Fixture::append_spawn(&parent, "linked", None);
        let linked = fixture.graph_rollout(
            "linked-id",
            &cwd,
            Some("p"),
            Some("/root/linked"),
            Some("Link"),
        );
        fixture.graph_rollout("unrelated", &cwd, None, None, None);
        let graph = crate::SessionGraph::from_backend(Box::new(CodexSessionGraph::open_in(
            &fixture.sessions,
            &parent,
        )));

        assert_eq!(graph.subagent_source(&parent, "linked-id"), Some(linked));
        assert_eq!(
            graph.subagent_source(&parent, "unrelated"),
            None,
            "an operation graph must not resolve an unrelated rollout by UUID"
        );
    }

    #[test]
    fn graph_suppresses_links_when_the_cached_child_source_is_missing() {
        let fixture = Fixture::new();
        let cwd = fixture.root.join("repo");
        let parent = fixture.graph_rollout("p", &cwd, None, None, None);
        Fixture::append_spawn(&parent, "gone", None);
        let child =
            fixture.graph_rollout("gone-id", &cwd, Some("p"), Some("/root/gone"), Some("Gone"));
        let graph = crate::SessionGraph::from_backend(Box::new(CodexSessionGraph::open_in(
            &fixture.sessions,
            &parent,
        )));
        fs::remove_file(&child).unwrap();
        let mut blocks = crate::engine::parse_session_as(crate::Agent::Codex, &parent)
            .unwrap()
            .blocks();

        graph.resolve_relationships(&parent, &mut blocks);

        assert!(matches!(
            &blocks[0],
            crate::Block::SubAgent(spawn) if spawn.agent_id.is_empty()
        ));
        assert_eq!(graph.subagent_source(&parent, "gone-id"), None);
    }

    #[test]
    fn graph_retries_an_unresolved_spawn_after_the_source_advances() {
        let fixture = Fixture::new();
        let cwd = fixture.root.join("repo");
        let parent = fixture.graph_rollout("p", &cwd, None, None, None);
        Fixture::append_spawn(&parent, "missing", None);
        let graph = crate::SessionGraph::from_backend(Box::new(CodexSessionGraph::open_in(
            &fixture.sessions,
            &parent,
        )));
        let mut blocks = crate::engine::parse_session_as(crate::Agent::Codex, &parent)
            .unwrap()
            .blocks();

        graph.resolve_relationships(&parent, &mut blocks);
        fixture.graph_rollout(
            "too-late",
            &cwd,
            Some("p"),
            Some("/root/missing"),
            Some("Late"),
        );
        graph.resolve_relationships(&parent, &mut blocks);

        assert!(matches!(
            &blocks[0],
            crate::Block::SubAgent(spawn) if spawn.agent_id == "too-late"
        ));
    }

    #[test]
    fn graph_defaults_resolved_agent_types_when_nickname_is_missing() {
        let fixture = Fixture::new();
        let cwd = fixture.root.join("repo");
        let parent = fixture.graph_rollout("p", &cwd, None, None, None);
        Fixture::append_spawn(&parent, "review", Some("/root/review"));
        fixture.graph_rollout("c", &cwd, Some("p"), Some("/root/review"), None);
        let graph = crate::SessionGraph::from_backend(Box::new(CodexSessionGraph::open_in(
            &fixture.sessions,
            &parent,
        )));
        let mut blocks = crate::engine::parse_session_as(crate::Agent::Codex, &parent)
            .unwrap()
            .blocks();

        graph.resolve_relationships(&parent, &mut blocks);

        assert!(matches!(
            &blocks[0],
            crate::Block::SubAgent(spawn)
                if spawn.agent_id == "c" && spawn.agent_type == "agent"
        ));
        assert!(matches!(
            &blocks[1],
            crate::Block::AgentDone {
                agent_id,
                agent_type,
                ..
            } if agent_id == "c" && agent_type == "agent"
        ));
    }

    #[test]
    fn public_session_graph_open_uses_the_adapter_backed_codex_tree() {
        let fixture = Fixture::new();
        let cwd = fixture.root.join("repo");
        let parent = fixture.graph_rollout("p", &cwd, None, None, None);
        Fixture::append_spawn(&parent, "review", None);
        let child = fixture.graph_rollout("c", &cwd, Some("p"), Some("/root/review"), Some("Hume"));
        let graph = crate::SessionGraph::open(crate::Agent::Codex, &parent);
        let mut blocks = crate::engine::parse_session_as(crate::Agent::Codex, &parent)
            .unwrap()
            .blocks();

        graph.resolve_relationships(&parent, &mut blocks);

        assert!(matches!(
            &blocks[0],
            crate::Block::SubAgent(spawn)
                if spawn.agent_id == "c" && spawn.agent_type == "Hume"
        ));
        assert_eq!(graph.subagent_source(&parent, "c"), Some(child));
    }
}
