//! Discover Antigravity CLI conversations and join their workspace catalog read-only.

use super::{limits::config_home, model};
use claude_replay_engine::seam::{
    ancestors_below, bounded_lines, home_dir, Agent, Candidate, Elision, SessionCard, SNIPPET_CHARS,
};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use url::Url;

/// Conversation logs named `transcript.jsonl` live under a UUID-named trajectory directory.
/// Use that directory to distinguish sessions whose filenames are identical.
pub(crate) fn trajectory_id(path: &Path) -> Option<&str> {
    if !matches!(
        path.file_name()?.to_str()?,
        "transcript.jsonl" | "transcript_full.jsonl"
    ) {
        return None;
    }
    let logs = path.parent()?;
    let generated = logs.parent()?;
    if logs.file_name()? != "logs" || generated.file_name()? != ".system_generated" {
        return None;
    }
    let id = generated.parent()?.file_name()?.to_str()?;
    (id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        }))
    .then_some(id)
}

pub fn brain_dir() -> PathBuf {
    config_home().join("brain")
}

pub fn store_contains(path: &Path) -> bool {
    path.starts_with(brain_dir()) && trajectory_id(path).is_some()
}

pub fn store_transcripts_in(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let logs = entry.path().join(".system_generated/logs");
            ["transcript_full.jsonl", "transcript.jsonl"]
                .into_iter()
                .map(|name| logs.join(name))
                .find(|path| trajectory_id(path).is_some() && path.is_file())
        })
        .collect()
}

#[derive(Default)]
struct CatalogEntry {
    title: Option<String>,
    workspaces: Vec<PathBuf>,
}

fn open_catalog(root: &Path) -> Option<Connection> {
    let db = Connection::open_with_flags(
        root.join("conversation_summaries.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    // The live CLI owns the database. Retry on a later discovery cycle if it is busy.
    db.busy_timeout(std::time::Duration::ZERO).ok()?;
    Some(db)
}

fn catalog(db: &Connection, id: &str) -> Option<CatalogEntry> {
    let (title, uris): (String, String) = db
        .query_row(
            "SELECT title, workspace_uris FROM conversation_summaries WHERE conversation_id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok()?;
    let uris: Vec<String> = serde_json::from_str(&uris).unwrap_or_default();
    Some(CatalogEntry {
        title: (!title.trim().is_empty()).then_some(title),
        workspaces: uris
            .iter()
            .filter_map(|uri| Url::parse(uri).ok()?.to_file_path().ok())
            .collect(),
    })
}

fn catalog_entry(path: &Path) -> Option<CatalogEntry> {
    catalog(
        &open_catalog(path.ancestors().nth(5)?)?,
        trajectory_id(path)?,
    )
}

fn workspace_cache(root: &Path) -> std::collections::BTreeMap<PathBuf, String> {
    fs::read(root.join("cache/last_conversations.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn workspaces(
    id: &str,
    entry: CatalogEntry,
    cached: &std::collections::BTreeMap<PathBuf, String>,
) -> Vec<PathBuf> {
    if !entry.workspaces.is_empty() {
        return entry.workspaces;
    }
    cached
        .iter()
        .filter(|(workspace, value)| *value == id && workspace.is_absolute())
        .map(|(workspace, _)| workspace.clone())
        .collect()
}

pub fn project_dir(path: &Path) -> Option<PathBuf> {
    let entry = catalog_entry(path).unwrap_or_default();
    if !entry.workspaces.is_empty() {
        return entry.workspaces.into_iter().next();
    }
    workspaces(
        trajectory_id(path)?,
        entry,
        &workspace_cache(path.ancestors().nth(5)?),
    )
    .into_iter()
    .next()
}

fn prompts(path: &Path) -> impl Iterator<Item = String> {
    bounded_lines(path, Elision::Aggressive).filter_map(|line| {
        model::user_text(&serde_json::from_str::<Value>(&line).ok()?).map(str::to_owned)
    })
}

pub fn session_card(path: &Path) -> SessionCard {
    SessionCard {
        title: catalog_entry(path).and_then(|entry| entry.title),
        last_prompt: prompts(path).last(),
        name: None,
    }
}

pub fn candidates_scoped(cwd: &Path) -> Vec<Candidate> {
    candidates_scoped_in(&config_home(), cwd)
}

fn candidates_scoped_in(root: &Path, cwd: &Path) -> Vec<Candidate> {
    let ancestors = ancestors_below(cwd, home_dir().as_deref());
    if ancestors.is_empty() {
        return Vec::new();
    }
    let db = open_catalog(root);
    let cached = workspace_cache(root);
    let candidates: Vec<_> = store_transcripts_in(&root.join("brain"))
        .into_iter()
        .filter_map(|path| {
            let id = trajectory_id(&path)?;
            let entry = db
                .as_ref()
                .and_then(|db| catalog(db, id))
                .unwrap_or_default();
            let workspaces = workspaces(id, entry, &cached);
            let ancestor = ancestors
                .iter()
                .position(|parent| workspaces.iter().any(|w| w == parent))?;
            Some((ancestor, path))
        })
        .collect();
    let Some(nearest) = candidates.iter().map(|(ancestor, _)| *ancestor).min() else {
        return Vec::new();
    };
    let project = ancestors[nearest]
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    candidates
        .into_iter()
        .filter_map(|(ancestor, path)| {
            if ancestor != nearest {
                return None;
            }
            let mtime = fs::metadata(&path).ok()?.modified().ok()?;
            let snippet = prompts(&path)
                .next()
                .unwrap_or_default()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(SNIPPET_CHARS)
                .collect();
            Some(Candidate {
                path,
                mtime,
                project: project.to_string(),
                snippet,
                cwd_affinity: nearest == 0,
                agent: Agent::ANTIGRAVITY,
            })
        })
        .collect()
}

pub fn resolve_id(id: &str) -> Option<PathBuf> {
    resolve_id_in(&brain_dir(), id)
}

fn resolve_id_in(brain: &Path, id: &str) -> Option<PathBuf> {
    if id.is_empty() {
        return None;
    }
    let mut matches = store_transcripts_in(brain)
        .into_iter()
        .filter(|path| trajectory_id(path).is_some_and(|candidate| candidate.starts_with(id)));
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn antigravity_discovers_one_log_per_conversation_and_decodes_workspace_uris() {
        let root = std::env::temp_dir().join(format!("cr-agy-store-{}", std::process::id()));
        let brain = root.join("brain");
        let a = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
        let b = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
        let first = brain.join(a).join(".system_generated/logs");
        let second = brain.join(b).join(".system_generated/logs");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        let full = first.join("transcript_full.jsonl");
        let plain = second.join("transcript.jsonl");
        fs::write(first.join("transcript.jsonl"), "").unwrap();
        fs::write(&plain, "").unwrap();
        fs::write(&full, r#"{"type":"USER_INPUT","content":"first prompt"}"#).unwrap();
        let workspace = root.join("project with spaces");
        {
            let db = Connection::open(root.join("conversation_summaries.db")).unwrap();
            db.execute_batch(
                "CREATE TABLE conversation_summaries
                    (conversation_id TEXT PRIMARY KEY, title TEXT, workspace_uris TEXT);",
            )
            .unwrap();
            let uri = Url::from_file_path(&workspace).unwrap();
            let uris = serde_json::json!([uri.as_str()]).to_string();
            db.execute(
                "INSERT INTO conversation_summaries VALUES (?1, ?2, ?3)",
                [a, "Session title", &uris],
            )
            .unwrap();
        }
        let mut found = store_transcripts_in(&brain);
        found.sort();
        assert_eq!(found, [full.clone(), plain.clone()]);
        assert_eq!(trajectory_id(&full), Some(a));
        assert_eq!(trajectory_id(&plain), Some(b));
        assert_eq!(project_dir(&full), Some(workspace.clone()));
        let list = candidates_scoped_in(&root, &workspace);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].path, full);
        assert!(list[0].cwd_affinity);
        assert!(candidates_scoped_in(&root, &root.join("unrelated")).is_empty());
        assert_eq!(resolve_id_in(&brain, "aaaa"), Some(full.clone()));
        assert_eq!(resolve_id_in(&brain, b), Some(plain));
        assert_eq!(resolve_id_in(&brain, "absent"), None);
        let card = session_card(&full);
        assert_eq!(card.title.as_deref(), Some("Session title"));
        assert_eq!(card.last_prompt.as_deref(), Some("first prompt"));
        fs::remove_dir_all(root).unwrap();
    }
}
