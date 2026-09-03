use anyhow::{anyhow, Result};
use claude_replay_engine::seam::{Agent, Candidate, CardMemo, CardOutcome, SessionCard};
use serde_json::Value;
#[cfg(test)]
use std::cell::Cell;
#[cfg(target_os = "macos")]
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::sync::{Mutex, OnceLock};
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
    for line in
        claude_replay_engine::seam::bounded_lines(path, claude_replay_engine::seam::Elision::None)
            .take(100)
    {
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

    for line in
        claude_replay_engine::seam::bounded_lines(path, claude_replay_engine::seam::Elision::None)
            .take(100)
    {
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
    let mut fallback = None;
    for line in
        claude_replay_engine::seam::bounded_lines(path, claude_replay_engine::seam::Elision::None)
            .take(300)
    {
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
            // The same three rules the transcript decode applies to a Desktop prompt: the
            // `# Files mentioned by the user: … ## My request:` envelope is transport, so only
            // the request is the prompt; an `<image …>` marker and its `</image>` close are the
            // host's, not the person's. Without this the card read "# Files mentioned by the
            // user: ## shot.png…" and could run out of SNIPPET_CHARS before the request
            // (found by the review bot on the takeover MR).
            .filter(|text| super::model::desktop_image_marker(text).is_none())
            .filter(|text| text.trim() != "</image>")
            .map(|text| {
                super::model::desktop_prompt(text)
                    .map(|(request, _)| request)
                    .unwrap_or(text)
            })
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

const SESSION_TITLE_CHARS: usize = 52;

/// A picker/monitor title is an identity, not a second copy of the prompt. Codex Desktop can
/// leave `threads.title` equal to the complete first user message (observed at 408 chars), and
/// CLI-only sessions have no generated title at all. Keep a useful bounded fallback and reduce
/// file URLs to the filename a person can actually recognise.
fn compact_session_title(text: &str) -> String {
    let mut readable = String::with_capacity(text.len());
    let mut cursor = 0;
    while let Some(relative) = text[cursor..].find("file://") {
        let start = cursor + relative;
        readable.push_str(&text[cursor..start]);
        let tail = &text[start..];
        let end = tail
            .char_indices()
            .find(|(_, ch)| {
                ch.is_whitespace()
                    || matches!(
                        ch,
                        '”' | '"' | '\'' | '`' | '）' | ')' | '】' | ']' | '，' | ',' | '。'
                    )
            })
            .map(|(at, _)| at)
            .unwrap_or(tail.len());
        let path = percent_decode_title(tail[..end].trim_start_matches("file://"));
        readable.push_str(
            path.rsplit('/')
                .find(|part| !part.is_empty())
                .unwrap_or(&path),
        );
        cursor = start + end;
    }
    readable.push_str(&text[cursor..]);
    let one_line = readable.split_whitespace().collect::<Vec<_>>().join(" ");
    let focused = [
        "帮我",
        "请你",
        "请帮",
        "能否",
        "能不能",
        "可以帮",
        "需要你",
        "麻烦",
    ]
    .iter()
    .filter_map(|marker| one_line.find(marker).map(|at| (at, marker.len())))
    .min_by_key(|(at, _)| *at)
    .map(|(at, len)| {
        one_line[at + len..]
            .trim_start_matches(|ch: char| ch.is_whitespace() || "，,:：。".contains(ch))
    })
    .filter(|candidate| candidate.chars().count() >= 8)
    .unwrap_or(&one_line);
    let all: Vec<char> = focused.chars().collect();
    let mut end = all.len().min(SESSION_TITLE_CHARS);
    if all.len() > SESSION_TITLE_CHARS {
        // Prefer a clause boundary after enough identifying content. This turns a request like
        // “帮我拆解对应一下，研发 demo 每一步的设计元素，在旧稿里…” into a title-sized
        // “拆解对应一下，研发 demo 每一步的设计元素…” rather than cutting mid-phrase.
        if let Some(boundary) = all[..end]
            .iter()
            .enumerate()
            .rev()
            .find(|(index, ch)| *index >= 24 && matches!(ch, '，' | ',' | '；' | ';'))
            .map(|(index, _)| index)
        {
            end = boundary;
        }
    }
    let prefix: String = all[..end].iter().collect();
    if end < all.len() {
        format!("{}…", prefix.trim_end())
    } else {
        prefix
    }
}

fn percent_decode_title(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (
                (bytes[index + 1] as char).to_digit(16),
                (bytes[index + 2] as char).to_digit(16),
            ) {
                out.push((hi * 16 + lo) as u8);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The Codex card's memo (#25). What a card depends on: the rollout's length (its FIRST user
/// prompt never changes once written, so a file that has not grown cannot change its prompt),
/// the prompt itself once found, and the Desktop catalog's stamp (a rename or a generated
/// title lands there, not in the rollout). With the memo in hand a repeat call touches
/// `metadata()` and nothing else; without it every monitor scan re-read the head of every
/// Codex transcript. Versioned and validated like every memo: unreadable or foreign → cold.
#[derive(serde::Serialize, serde::Deserialize)]
struct Memo {
    v: u8,
    len: u64,
    /// `Some` once a genuine prompt was read; `None` while the rollout had none yet.
    prompt: Option<String>,
    stamp: (u64, u64),
}
const MEMO_V: u8 = 1;

/// Codex does not persist the generated task title in its rollout; the Desktop catalog does,
/// when there is one. The first genuine user prompt is otherwise the stable, human-authored
/// label — far more discriminating than the repository name on every row.
pub(crate) fn session_card(path: &Path, memo: Option<&CardMemo>) -> CardOutcome {
    let Ok(len) = std::fs::metadata(path).map(|m| m.len()) else {
        return CardOutcome::Absent;
    };
    let stamp = catalog_stamp();
    let prev: Option<Memo> =
        CardMemo::decode(memo).filter(|m: &Memo| m.v == MEMO_V && m.len <= len);
    if let Some(m) = &prev {
        // Nothing appended and the catalog unchanged: the card cannot have changed. The memo
        // still comes back — its length is what keeps the NEXT call this cheap.
        if m.len == len && m.stamp == stamp {
            return CardOutcome::Unchanged {
                memo: CardMemo::encode(m).unwrap_or_else(|| CardMemo::new(serde_json::Value::Null)),
            };
        }
    }
    // A prompt already found is final; otherwise (cold, or a rollout that had no prompt yet
    // and has since grown) read the head again.
    let prompt = match prev.as_ref().and_then(|m| m.prompt.clone()) {
        Some(prompt) => prompt,
        None => first_user_snippet(path),
    };
    let genuine = prompt != "(no user prompt)" && !prompt.starts_with("↳ subagent");
    let memo = CardMemo::encode(&Memo {
        v: MEMO_V,
        len,
        prompt: genuine.then(|| prompt.clone()),
        stamp,
    });
    let title = codex_thread_title(path).map(|title| compact_session_title(&title));
    if !genuine {
        return match title {
            Some(title) => CardOutcome::Fresh {
                card: SessionCard {
                    title: Some(title),
                    last_prompt: None,
                },
                memo,
            },
            None => CardOutcome::Absent,
        };
    }
    CardOutcome::Fresh {
        card: SessionCard {
            title: Some(title.unwrap_or_else(|| compact_session_title(&prompt))),
            last_prompt: Some(prompt),
        },
        memo,
    }
}

/// The Desktop catalog's modification stamp — `(db, wal)` mtimes in seconds — so a memo can
/// tell "a title may have changed" without opening the database. `(0, 0)` where there is no
/// catalog (Linux, or a CLI-only install), which is a stable stamp too.
fn catalog_stamp() -> (u64, u64) {
    #[cfg(target_os = "macos")]
    {
        let db = codex_home().join("state_5.sqlite");
        let secs = |p: &Path| {
            std::fs::metadata(p)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0)
        };
        (
            secs(&db),
            secs(&PathBuf::from(format!("{}-wal", db.display()))),
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        (0, 0)
    }
}

#[cfg(target_os = "macos")]
#[derive(Default)]
struct ThreadTitleCache {
    db: PathBuf,
    modified: Option<(SystemTime, Option<SystemTime>)>,
    by_rollout: HashMap<PathBuf, String>,
}

#[cfg(target_os = "macos")]
static THREAD_TITLES: OnceLock<Mutex<ThreadTitleCache>> = OnceLock::new();

/// Codex Desktop's thread catalog is the authoritative source for generated titles and
/// explicit renames. Load it once per database mtime rather than opening SQLite for every row
/// on every monitor scan; CLI-only installs simply fall through to the rollout prompt.
///
/// macOS-only for the same reason as QoderWork's reader (`agents/qoderwork/discover.rs`):
/// rusqlite is the workspace's one C dependency and is declared under
/// `cfg(target_os = "macos")`, so a Linux build must not reference it at all. Codex Desktop
/// ships on macOS; elsewhere every session falls back to its first prompt, which is the same
/// answer this returns when the catalog is simply absent.
#[cfg(target_os = "macos")]
fn codex_thread_title(path: &Path) -> Option<String> {
    let db = codex_home().join("state_5.sqlite");
    let db_modified = std::fs::metadata(&db)
        .and_then(|meta| meta.modified())
        .ok()?;
    let wal = PathBuf::from(format!("{}-wal", db.display()));
    let wal_modified = std::fs::metadata(wal).and_then(|meta| meta.modified()).ok();
    let modified = (db_modified, wal_modified);
    let cache = THREAD_TITLES.get_or_init(|| Mutex::new(ThreadTitleCache::default()));
    let mut cache = cache.lock().ok()?;
    if cache.db != db || cache.modified != Some(modified) {
        cache.db = db.clone();
        cache.modified = Some(modified);
        cache.by_rollout = read_thread_titles(&db);
    }
    cache.by_rollout.get(path).cloned()
}

#[cfg(target_os = "macos")]
fn read_thread_titles(db: &Path) -> HashMap<PathBuf, String> {
    let Ok(connection) = rusqlite::Connection::open_with_flags(
        db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return HashMap::new();
    };
    // Newer catalogs expose `first_user_message`, which lets us reject the common non-title:
    // `title` is merely the entire prompt copied verbatim. Explicit `name` always wins.
    if let Ok(mut query) = connection.prepare(
        "SELECT rollout_path, NULLIF(TRIM(name), ''), NULLIF(TRIM(title), ''), \
         NULLIF(TRIM(first_user_message), '') FROM threads WHERE rollout_path IS NOT NULL",
    ) {
        if let Ok(rows) = query.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        }) {
            return rows
                .flatten()
                .filter_map(|(path, name, title, first)| {
                    let title =
                        name.or_else(|| title.filter(|value| first.as_deref() != Some(value)));
                    title.map(|title| (PathBuf::from(path), title))
                })
                .collect();
        }
    }
    // Compatibility with pre-`first_user_message` catalogs.
    let Ok(mut query) = connection.prepare(
        "SELECT rollout_path, COALESCE(NULLIF(TRIM(name), ''), NULLIF(TRIM(title), '')) \
         FROM threads WHERE rollout_path IS NOT NULL",
    ) else {
        return HashMap::new();
    };
    let Ok(rows) = query.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
    }) else {
        return HashMap::new();
    };
    rows.flatten()
        .filter_map(|(path, title)| title.map(|title| (PathBuf::from(path), title)))
        .collect()
}

#[cfg(not(target_os = "macos"))]
fn codex_thread_title(_path: &Path) -> Option<String> {
    None
}

/// The archive Codex moves retired rollouts into (`~/.codex/archived_sessions`, flat).
/// An archived session's spend is as real as a live one's — a machine-wide consumer
/// that skipped it under-counted whole projects (measured: 144 files here).
pub(crate) fn archived_dir() -> PathBuf {
    codex_home().join("archived_sessions")
}

/// Every `rollout-*.jsonl` under `dir`, bounded to the dated tree's depth.
fn rollout_files(dir: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() && depth < 4 {
                walk(&p, depth + 1, out);
            } else if p
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("rollout-") && n.ends_with(".jsonl"))
            {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, 0, &mut out);
    out
}

/// Every MAIN rollout in the Codex store, MACHINE-WIDE (#98): the dated
/// `YYYY/MM/DD/rollout-*.jsonl` tree PLUS the flat archive, minus sub-agent rollouts
/// (their `session_meta` head names a subagent thread source — the same marker the
/// picker snippet uses).
pub(crate) fn store_transcripts_machine() -> Vec<PathBuf> {
    store_transcripts_machine_in(&[sessions_dir(), archived_dir()])
}

fn store_transcripts_machine_in(roots: &[PathBuf]) -> Vec<PathBuf> {
    roots
        .iter()
        .flat_map(|root| rollout_files(root))
        .filter(|p| !head_is_subagent(p))
        .collect()
}

/// Every SUB-AGENT rollout in the Codex store, MACHINE-WIDE, with its lineage:
/// `(path, own session id, parent thread id)` — the complement of
/// [`store_transcripts_machine`], for consumers that bank sub-agent spend onto the root
/// session's account. A sub-agent head that names no parent is unattributable and is
/// omitted (nothing to bank it on).
pub(crate) fn subagent_transcripts_machine() -> Vec<(PathBuf, String, String)> {
    subagent_transcripts_in(&[sessions_dir(), archived_dir()])
}

fn subagent_transcripts_in(roots: &[PathBuf]) -> Vec<(PathBuf, String, String)> {
    roots
        .iter()
        .flat_map(|root| rollout_files(root))
        .filter_map(|p| {
            let (id, parent) = head_subagent_lineage(&p)?;
            Some((p, id, parent?))
        })
        .collect()
}

/// Whether a rollout's HEAD marks it as a sub-agent thread — one bounded line read.
fn head_is_subagent(path: &Path) -> bool {
    head_subagent_lineage(path).is_some()
}

/// The head's sub-agent lineage — `(own session id, parent thread id)` — when the rollout
/// at `path` IS a sub-agent thread. The SAME single-line read and the SAME marker
/// ([`subagent_snippet`]) as the main-listing exclusion, deliberately: a file excluded
/// from the main listing must be exactly the file this lineage listing surfaces, or a
/// rollout falls between the two and its spend vanishes.
fn head_subagent_lineage(path: &Path) -> Option<(String, Option<String>)> {
    let line =
        claude_replay_engine::seam::bounded_lines(path, claude_replay_engine::seam::Elision::None)
            .next()?;
    let v = serde_json::from_str::<Value>(&line).ok()?;
    subagent_snippet(&v)?;
    let payload = v.get("payload")?;
    // Prefer `id` over `session_id`: on real sub-agent heads `id` is the thread's OWN id
    // while `session_id` carries the ROOT's (observed on Codex Desktop 0.147 rollouts).
    let id = payload
        .get("id")
        .or_else(|| payload.get("session_id"))?
        .as_str()?
        .to_string();
    let parent = payload
        .pointer("/source/subagent/thread_spawn/parent_thread_id")
        .or_else(|| payload.get("parent_thread_id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    Some((id, parent))
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
        for line in claude_replay_engine::seam::bounded_lines(
            &s.path,
            claude_replay_engine::seam::Elision::None,
        )
        .take(300)
        {
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

    /// The machine-wide walk (#98): main rollouts listed — from the dated tree AND the
    /// flat archive — with SUB-AGENT rollouts excluded by their `session_meta` head; and
    /// the lineage listing is the exact COMPLEMENT of the exclusion, preferring the
    /// head's own `id` over `session_id` (which carries the ROOT's id on real heads).
    #[test]
    fn store_walk_lists_main_rollouts_and_skips_subagents() {
        let fixture = Fixture::new();
        let cwd = fixture.root.join("repo");
        let main = fixture.rollout_with_user("m1", &cwd, "build the thing");
        let sub = fixture.related_rollout("s1", &cwd, "m1", "agents/reviewer");
        // The flat archive: a retired MAIN session (its spend and row are as real as a
        // live one's) and a retired sub-agent whose head carries BOTH ids — `id` is the
        // thread's OWN, `session_id` the root's (observed on Codex Desktop 0.147).
        let archive = fixture.root.join("archived_sessions");
        fs::create_dir_all(&archive).unwrap();
        let arch_main = archive.join("rollout-a1.jsonl");
        fs::write(
            &arch_main,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"a1\",\"cwd\":\"/repo\"}}\n",
        )
        .unwrap();
        let arch_sub = archive.join("rollout-s2.jsonl");
        fs::write(
            &arch_sub,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s2\",\"session_id\":\"m1\",\"thread_source\":\"subagent\",\"parent_thread_id\":\"s1\",\"cwd\":\"/repo\"}}\n",
        )
        .unwrap();

        let roots = [fixture.sessions.clone(), archive];
        let got = store_transcripts_machine_in(&roots);
        assert!(got.contains(&main), "main rollout listed: {got:?}");
        assert!(got.contains(&arch_main), "archived main listed: {got:?}");
        assert!(
            !got.contains(&sub) && !got.contains(&arch_sub),
            "subagent rollouts excluded: {got:?}"
        );

        let mut lineage = subagent_transcripts_in(&roots);
        lineage.sort_by(|a, b| a.1.cmp(&b.1));
        assert_eq!(
            lineage,
            vec![
                (sub, "s1".to_string(), "m1".to_string()),
                (arch_sub, "s2".to_string(), "s1".to_string()),
            ],
            "lineage is the complement of the main listing, own id preferred"
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
        let CardOutcome::Fresh { card, .. } = session_card(&path, None) else {
            panic!("a genuine user prompt should label the session");
        };
        assert_eq!(card.label(), Some("Fix the parser carefully"));
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
    #[cfg(target_os = "macos")] // the catalog reader is macOS-only; see `codex_thread_title`
    fn thread_catalog_prefers_an_explicit_name_then_generated_title() {
        let fixture = Fixture::new();
        let db = fixture.root.join("state_5.sqlite");
        let connection = rusqlite::Connection::open(&db).unwrap();
        connection
            .execute(
                "CREATE TABLE threads (rollout_path TEXT, name TEXT, title TEXT NOT NULL, first_user_message TEXT)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO threads VALUES (?1, ?2, ?3, ?4)",
                (
                    "/tmp/a.jsonl",
                    "Renamed task",
                    "Generated task",
                    "Original prompt",
                ),
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO threads VALUES (?1, ?2, ?3, ?4)",
                (
                    "/tmp/b.jsonl",
                    Option::<String>::None,
                    "Generated task",
                    "Original prompt",
                ),
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO threads VALUES (?1, ?2, ?3, ?4)",
                (
                    "/tmp/c.jsonl",
                    Option::<String>::None,
                    "The whole original prompt",
                    "The whole original prompt",
                ),
            )
            .unwrap();
        drop(connection);

        let titles = read_thread_titles(&db);
        assert_eq!(titles[Path::new("/tmp/a.jsonl")], "Renamed task");
        assert_eq!(titles[Path::new("/tmp/b.jsonl")], "Generated task");
        assert!(!titles.contains_key(Path::new("/tmp/c.jsonl")));
    }

    /// The prompts are Chinese because the clause-boundary rule under test is; the PATHS are
    /// synthetic because this repository is public and a fixture is not a place to leave
    /// somebody's home directory and folder names.
    #[test]
    fn prompt_fallback_is_a_bounded_title_and_reduces_file_urls() {
        let text = "你看下 file:///Users/example/docs/%E5%8D%8F%E4%BD%9C%E6%B5%81%E7%A8%8B.html，帮我梳理这个设计里的组件、状态和概念，并逐项对照研发版本中对应的实现细节。还需要标出没有对应上的部分。";
        let title = compact_session_title(text);
        // The file preamble is useful only when no task clause exists; here the action is the
        // stronger identity and should win completely.
        assert!(!title.contains("file://") && !title.contains("/Users/"));
        assert!(
            title.chars().count() <= SESSION_TITLE_CHARS + 1,
            "bounded title: {title}"
        );
        assert!(
            title.starts_with("梳理这个设计"),
            "action-focused title: {title}"
        );
        let file_only = compact_session_title(
            "查看 file:///Users/example/docs/%E5%8D%8F%E4%BD%9C%E6%B5%81%E7%A8%8B.html 的布局",
        );
        assert!(
            file_only.contains("协作流程.html"),
            "recognisable basename: {file_only}"
        );
    }

    #[test]
    fn prompt_fallback_skips_file_preamble_for_the_task_request() {
        let text = "你看下，这个：“file:///Users/example/docs/design-flow.html”是一个带流程的设计稿，这个“file:///Users/example/downloads/%E8%AE%BE%E8%AE%A1/build.dc.html”也是研发版本，但我觉得很多组件已经设计了，你帮我各自拆解对应一下，研发 demo 每一步在之前设计稿里有没有对应，并标出缺失内容。";
        let title = compact_session_title(text);
        assert!(
            title.starts_with("各自拆解对应一下"),
            "action-focused title: {title}"
        );
        assert!(!title.contains("file://") && !title.contains("/Users/"));
        assert!(title.chars().count() <= SESSION_TITLE_CHARS + 1);
    }

    /// A Codex Desktop prompt with attachments arrives wrapped in a transport envelope. The
    /// picker snippet and the session card must show the person's request, not the envelope.
    #[test]
    fn picker_snippet_and_card_use_the_request_inside_a_desktop_envelope() {
        let fixture = Fixture::new();
        let cwd = fixture.root.join("repo");
        let path = fixture.rollout("2026/08/30", "desk", &cwd, "codex-desktop");
        // The Desktop host's whole first turn: envelope, image marker, the image, the close.
        let turn = r##"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"\n# Files mentioned by the user:\n\n## shot.png: /tmp/shot.png\n\n## plan.md: /tmp/plan.md\n\n## My request:\nPlease inspect this design\n"},{"type":"input_text","text":"<image name=[Image #1] path=\"/tmp/shot.png\">"},{"type":"input_image","image_url":"data:image/png;base64,YQ=="},{"type":"input_text","text":"</image>"}]}}"##;
        {
            use std::io::Write;
            let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
            writeln!(f, "{turn}").unwrap();
        }
        assert_eq!(first_user_snippet(&path), "Please inspect this design");
        let CardOutcome::Fresh { card, .. } = session_card(&path, None) else {
            panic!("a Desktop prompt still labels the session");
        };
        assert_eq!(card.label(), Some("Please inspect this design"));
        assert_eq!(
            card.last_prompt.as_deref(),
            Some("Please inspect this design")
        );
    }

    /// The memo makes a repeat call answer from `metadata()` alone: nothing appended and the
    /// catalog untouched → `Unchanged`, carrying the memo; a rollout with no prompt yet is
    /// re-read once it grows; a found prompt is final and is never re-read.
    #[test]
    fn card_memo_short_circuits_an_unchanged_rollout_and_rereads_only_when_it_must() {
        let fixture = Fixture::new();
        let cwd = fixture.root.join("repo");
        let path = fixture.rollout("2026/08/30", "memo", &cwd, "codex-tui");
        // No prompt yet: the card is Absent (no catalog here), but the memo still records the length.
        let cold = session_card(&path, None);
        assert!(
            matches!(cold, CardOutcome::Absent),
            "no prompt, no catalog: nothing to name"
        );
        Fixture::append_user(&path, "First real prompt");
        let CardOutcome::Fresh { card, memo } = session_card(&path, None) else {
            panic!("a prompt names the session");
        };
        assert_eq!(card.label(), Some("First real prompt"));
        let memo = memo.expect("a memo comes back with the card");
        // Same length, same catalog stamp → Unchanged, and the memo is preserved.
        let again = session_card(&path, Some(&memo));
        let CardOutcome::Unchanged { memo: kept } = again else {
            panic!("nothing changed, so the card must be kept: {again:?}");
        };
        let decoded: Memo = CardMemo::decode(Some(&kept)).expect("memo decodes");
        assert_eq!(decoded.prompt.as_deref(), Some("First real prompt"));
        // Appending does not change the FIRST prompt: Fresh again (the caller re-derives), but
        // from the memo's prompt, not a re-read — the second user line must not win.
        Fixture::append_user(&path, "A later prompt");
        let CardOutcome::Fresh { card, .. } = session_card(&path, Some(&kept)) else {
            panic!("a grown file re-derives");
        };
        assert_eq!(card.label(), Some("First real prompt"));
        // A memo from a longer file than the one on disk (rewritten) is ignored, not trusted.
        let stale = CardMemo::encode(&Memo {
            v: MEMO_V,
            len: u64::MAX,
            prompt: Some("ghost".into()),
            stamp: (0, 0),
        })
        .unwrap();
        let CardOutcome::Fresh { card, .. } = session_card(&path, Some(&stale)) else {
            panic!("a stale memo falls back to a cold read");
        };
        assert_eq!(
            card.label(),
            Some("First real prompt"),
            "the cold read wins over a ghost memo"
        );
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
