use anyhow::{anyhow, Result};
use claude_replay_engine::seam::{Agent, Candidate};
use serde_json::Value;
#[cfg(test)]
use std::cell::Cell;
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[cfg(test)]
thread_local! {
    static RELATIONSHIP_READS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_relationship_reads() {
    RELATIONSHIP_READS.set(0);
}

#[cfg(test)]
pub(crate) fn relationship_reads() -> usize {
    RELATIONSHIP_READS.get()
}

#[derive(Debug, Clone)]
pub struct CodexSession {
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

#[derive(Debug)]
struct CodexRelationship {
    id: String,
    path: PathBuf,
    parent_thread_id: Option<String>,
    agent_path: Option<String>,
}

fn relationship_from_path(path: &Path) -> Option<CodexRelationship> {
    #[cfg(test)]
    RELATIONSHIP_READS.set(RELATIONSHIP_READS.get() + 1);

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
        let parent_thread_id = payload
            .pointer("/source/subagent/thread_spawn/parent_thread_id")
            .or_else(|| payload.get("parent_thread_id"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let agent_path = payload
            .pointer("/source/subagent/thread_spawn/agent_path")
            .or_else(|| payload.get("agent_path"))
            .and_then(Value::as_str)
            .map(str::to_string);
        return Some(CodexRelationship {
            id,
            path: path.to_path_buf(),
            parent_thread_id,
            agent_path,
        });
    }
    None
}

fn containing_sessions_dir(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|ancestor| ancestor.file_name().and_then(|name| name.to_str()) == Some("sessions"))
        .map(Path::to_path_buf)
}

pub(crate) struct CodexRelationshipIndex {
    nodes: Vec<CodexRelationship>,
}

impl CodexRelationshipIndex {
    pub(crate) fn load(root: &Path) -> Option<Self> {
        let sessions = containing_sessions_dir(root)?;
        let nodes: Vec<_> = jsonl_files(&sessions)
            .into_iter()
            .filter_map(|path| relationship_from_path(&path))
            .collect();
        let index = Self { nodes };
        index.root_id(root).is_some().then_some(index)
    }

    /// The relationship node for the rollout at `root`. Raw path equality first (the walk
    /// and the caller normally spell the path identically); a canonicalized rescan only on
    /// miss, so a symlinked/non-canonical root still resolves instead of silently
    /// disabling the tree.
    fn root_id(&self, root: &Path) -> Option<&str> {
        if let Some(node) = self.nodes.iter().find(|node| node.path == root) {
            return Some(node.id.as_str());
        }
        let root = normalized(root);
        self.nodes
            .iter()
            .find(|node| normalized(&node.path) == root)
            .map(|node| node.id.as_str())
    }

    pub(crate) fn subagent_source(&self, root: &Path, child_id: &str) -> Option<PathBuf> {
        let root_id = self.root_id(root)?;
        let reaches_root = |candidate: &CodexRelationship| {
            let mut current = candidate.parent_thread_id.as_deref();
            let mut seen = HashSet::new();
            while let Some(id) = current {
                if id == root_id {
                    return true;
                }
                if !seen.insert(id) {
                    return false;
                }
                current = self
                    .nodes
                    .iter()
                    .find(|node| node.id == id)
                    .and_then(|node| node.parent_thread_id.as_deref());
            }
            false
        };
        let mut exact = self
            .nodes
            .iter()
            .filter(|node| node.id == child_id && reaches_root(node))
            .map(|node| node.path.clone());
        let exact_match = exact.next();
        if exact.next().is_none() && exact_match.is_some() {
            return exact_match;
        }

        // Compatibility fallback for old or copy-trimmed rollouts that have no
        // sub_agent_activity event in the parent and therefore still carry an encoded path key.
        let agent_path = crate::agents::codex::model::decode_agent_path(child_id)?;
        let mut matches = self
            .nodes
            .iter()
            .filter(|node| {
                node.agent_path.as_deref() == Some(agent_path.as_str()) && reaches_root(node)
            })
            .map(|node| node.path.clone());
        let matched = matches.next()?;
        matches.next().is_none().then_some(matched)
    }
}

pub(crate) fn subagent_source(root: &Path, child_id: &str) -> Option<PathBuf> {
    CodexRelationshipIndex::load(root)?.subagent_source(root, child_id)
}

pub(crate) fn subagent_sources(root: &Path, ids: &[&str]) -> Vec<Option<PathBuf>> {
    if ids.is_empty() {
        return Vec::new();
    }
    let Some(relationships) = CodexRelationshipIndex::load(root) else {
        return vec![None; ids.len()];
    };
    ids.iter()
        .map(|id| relationships.subagent_source(root, id))
        .collect()
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
            .filter(|text| !super::model::is_host_context(text))
            .collect::<Vec<_>>()
            .join(" ");
        let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if !compact.is_empty() {
            return compact
                .chars()
                .take(claude_replay_engine::seam::SNIPPET_CHARS)
                .collect();
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
                agent: Agent::CODEX,
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
    candidates_scoped_in(
        &sessions_dir(),
        cwd,
        claude_replay_engine::seam::home_dir().as_deref(),
    )
}

/// The **nearest ancestor of `cwd`** (walking up the directory chain, strictly inside
/// `home` — #69) that owns any Codex sessions, and its sessions — the "no global fallback"
/// scoping every scoped lookup shares. Returns those sessions (newest-first, borrowed from
/// `sessions`) plus whether the match was `cwd` itself (exact — used for picker affinity).
/// Empty if no ancestor inside home owns one.
fn nearest_ancestor_sessions<'a>(
    sessions: &'a [CodexSession],
    cwd: &Path,
    home: Option<&Path>,
) -> (Vec<&'a CodexSession>, bool) {
    let cwd_n = normalized(cwd);
    for anc in claude_replay_engine::seam::ancestors_below(cwd, home) {
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
    let (matched, _) = nearest_ancestor_sessions(
        &sessions,
        cwd,
        claude_replay_engine::seam::home_dir().as_deref(),
    );
    matched
        .into_iter()
        .map(|s| (s.id.clone(), s.mtime, first_user_snippet(&s.path)))
        .collect()
}

fn candidates_scoped_in(root: &Path, cwd: &Path, home: Option<&Path>) -> Vec<Candidate> {
    let sessions = sessions_in(root); // newest-first
    let (matched, is_exact) = nearest_ancestor_sessions(&sessions, cwd, home);
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
                agent: Agent::CODEX,
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
    latest_for_cwd_in(
        &sessions_dir(),
        cwd,
        claude_replay_engine::seam::home_dir().as_deref(),
    )
}

fn latest_for_cwd_in(root: &Path, cwd: &Path, home: Option<&Path>) -> Option<CodexSession> {
    let sessions = sessions_in(root); // newest-first
                                      // The nearest ancestor's matches are newest-first, so the first is the latest.
    nearest_ancestor_sessions(&sessions, cwd, home)
        .0
        .first()
        .map(|s| (*s).clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use claude_replay_engine::seam::{AgentStatus, Block, SubAgent};
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
            self.related_rollout(id, cwd, "parent-session", agent_path)
        }

        fn related_rollout(
            &self,
            id: &str,
            cwd: &Path,
            parent_thread_id: &str,
            agent_path: &str,
        ) -> PathBuf {
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
                                "parent_thread_id": parent_thread_id,
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
    fn subagent_source_is_scoped_to_root_operation() {
        let fixture = Fixture::new();
        let cwd = fixture.root.join("repo");
        let root_a = fixture.rollout("2026/07/18", "root-a", &cwd, "codex-tui");
        fixture.rollout("2026/07/18", "root-b", &cwd, "codex-tui");
        let child_a = fixture.related_rollout("child-a", &cwd, "root-a", "/root/spec_review");
        fixture.related_rollout("child-b", &cwd, "root-b", "/root/spec_review");

        let child_id = crate::agents::codex::model::encode_agent_path("/root/spec_review");
        assert_eq!(
            subagent_source(&root_a, &child_id).as_deref(),
            Some(child_a.as_path())
        );
        assert_eq!(subagent_source(&root_a, "not-a-codex-child-key"), None);
    }

    #[test]
    fn subagent_source_uses_thread_id_when_agent_paths_repeat() {
        let fixture = Fixture::new();
        let cwd = fixture.root.join("repo");
        let root = fixture.rollout("2026/07/18", "root", &cwd, "codex-tui");
        fixture.related_rollout("child-a", &cwd, "root", "/root/review");
        let child_b = fixture.related_rollout("child-b", &cwd, "root", "/root/review");

        assert_eq!(
            subagent_source(&root, "child-b").as_deref(),
            Some(child_b.as_path())
        );
    }

    #[test]
    fn enrich_tree_scans_relationship_store_once() {
        let fixture = Fixture::new();
        let cwd = fixture.root.join("repo");
        let root = fixture.rollout("2026/07/18", "root", &cwd, "codex-tui");
        let child_a = fixture.related_rollout("child-a", &cwd, "root", "/root/child-a");
        let child_b = fixture.related_rollout("child-b", &cwd, "root", "/root/child-b");
        let child = |id: &str| {
            Block::SubAgent(SubAgent {
                agent_id: id.to_string(),
                tool_use_id: format!("spawn-{id}"),
                agent_type: "agent".to_string(),
                description: id.to_string(),
                prompt: "work".to_string(),
                status: AgentStatus::Running,
                result: None,
                output_file: None,
                blocks: Vec::new(),
                subtree_cost: None,
            })
        };
        let mut blocks = vec![child("child-a"), child("child-b")];

        reset_relationship_reads();
        crate::agents::codex::model::enrich_tree(&root, &mut blocks);

        assert_eq!(
            relationship_reads(),
            3,
            "one relationship read per rollout, independent of child count"
        );

        reset_relationship_reads();
        let sources = subagent_sources(&root, &["child-a", "child-b"]);
        assert_eq!(
            sources,
            vec![Some(child_a.clone()), Some(child_b.clone())],
            "batch resolution returns paths in ids order"
        );
        assert_eq!(
            relationship_reads(),
            3,
            "batch path resolution also reads each rollout once"
        );

        reset_relationship_reads();
        assert!(subagent_sources(&root, &[]).is_empty());
        assert_eq!(
            relationship_reads(),
            0,
            "a leaf transcript must not scan the relationship store"
        );
    }

    #[test]
    fn subagent_source_resolves_a_symlinked_root() {
        let fixture = Fixture::new();
        let cwd = fixture.root.join("repo");
        let root = fixture.rollout("2026/07/18", "root", &cwd, "codex-tui");
        let child = fixture.related_rollout("child", &cwd, "root", "/root/review");

        // The caller's path spells the same rollout differently (a symlink beside it):
        // raw equality misses, the canonicalized fallback must still find the root node.
        let link = root.with_file_name("rollout-link.jsonl");
        std::os::unix::fs::symlink(&root, &link).unwrap();
        assert_eq!(
            subagent_source(&link, "child").as_deref(),
            Some(child.as_path())
        );
    }

    #[test]
    fn subagent_source_accepts_only_reachable_descendants() {
        let fixture = Fixture::new();
        let cwd = fixture.root.join("repo");
        let root = fixture.rollout("2026/07/18", "root", &cwd, "codex-tui");
        fixture.related_rollout("child", &cwd, "root", "/root/spec_review");
        let grandchild = fixture.related_rollout(
            "grandchild",
            &cwd,
            "child",
            "/root/spec_review/standards_axis",
        );
        fixture.related_rollout("broken", &cwd, "missing", "/root/broken");
        fixture.related_rollout("cycle-a", &cwd, "cycle-b", "/root/cycle");
        fixture.related_rollout("cycle-b", &cwd, "cycle-a", "/root/cycle-parent");

        let nested_id =
            crate::agents::codex::model::encode_agent_path("/root/spec_review/standards_axis");
        assert_eq!(
            subagent_source(&root, &nested_id).as_deref(),
            Some(grandchild.as_path())
        );
        assert_eq!(
            subagent_source(
                &root,
                &crate::agents::codex::model::encode_agent_path("/root/broken")
            ),
            None
        );
        assert_eq!(
            subagent_source(
                &root,
                &crate::agents::codex::model::encode_agent_path("/root/cycle")
            ),
            None
        );
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

        // From repo_b (and its ancestors), repo_a's session must NOT show. The fixture
        // root stands in for $HOME (#69: probes never leave it).
        let home = Some(fixture.root.as_path());
        assert!(
            candidates_scoped_in(&fixture.sessions, &repo_b, home).is_empty(),
            "a sibling dir's session leaked in"
        );
        // From repo_a itself, it shows (exact cwd → affinity).
        let here = candidates_scoped_in(&fixture.sessions, &repo_a, home);
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
        // the resume target must be None — NOT repo_a's session. The fixture root
        // stands in for $HOME (#69).
        let home = Some(fixture.root.as_path());
        assert!(
            latest_for_cwd_in(&fixture.sessions, &repo_b, home).is_none(),
            "leaked a sibling dir's session as the resume target"
        );
        // From repo_a itself it resolves.
        assert_eq!(
            latest_for_cwd_in(&fixture.sessions, &repo_a, home).map(|s| s.id),
            Some("sa".to_string())
        );
    }
}
