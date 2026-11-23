//! **Qwenwork's opencode engine** (#s657): the chats no JSONL file holds.
//!
//! Qwenwork picks an engine per chat. Its app database records the pick on each chat
//! (`sub_chats.ext.modelExecutionRouteV2.harness`): `qoder-cli` writes the family's JSONL under
//! `~/.qwenworkcn/projects`, which [`super::discover`] reads; `opencode`, a bundled engine
//! (flash-cli), writes nothing there. Its record is a SQLite database of its own,
//! `~/.qwenworkcn/.harness/flash-cli/qwenwork/qwenwork.db`, in opencode's schema, with an
//! append-only event log: `event` rows (`aggregate_id` the engine session, `seq` rising), each the
//! whole object as it then stood (`message.updated.1` an `info`, `message.part.updated.1` a
//! `part`). Measured on Qwenwork 2.1.0 (engine 1.18.18), keys and counts only.
//!
//! Every reader here works on a transcript FILE, and so do the tools that shell out to
//! `agent-replay --paths --all` (one reads the file at a row's `path`), so each opencode chat is
//! written out as a MIRROR: one JSONL file in the family's own record shapes, the ones its
//! qoder-cli chats already have, under [`mirror_root_for`] in the store's layout
//! (`<slug of its folder>/<chat uuid>.jsonl`). The family's discovery lists it beside the store,
//! the title join (`sub_chats.session_id` = the file's stem) is unchanged, and the reader is the
//! one the family has. The mirror is the projection of the event log in `seq` order, so as the
//! log grows the file only grows: a record is written when its part is final (a reply once its
//! text ends, a tool call once it runs and its result once it ends, a step's usage when the step
//! finishes), and a message that completes writes whatever of it is still open. Anything else
//! (a removal, a reordering) builds the file again with a new first line, which is what tells a
//! cache that read the old one it is stale.
//!
//! One chat can be several engine sessions: the chat's binding (`engineSessionBindingV1`) names
//! the current one and a `generation`, and a new generation starts in the same folder with none of
//! the history replayed. Where the chat's folder is its own, every top-level engine session in it
//! is one of the chat's generations, joined in the order they began. Where several chats share a
//! folder (a project folder), only the bound session is mirrored: which earlier session belongs to
//! which chat is not recorded.
//!
//! A task call runs as a CHILD session (`parent_id` the caller's; the call's
//! `state.metadata.sessionId` names it from its first running update, measured), whose events are
//! its own (#s659). Each is mirrored as the family files a sub-agent,
//! `<chat's mirror without .jsonl>/subagents/agent-<child id>.jsonl`, its records on the sidechain
//! (`isSidechain`, `agentId` the child's id, `parent_tool_use_id` the call's), with a sidecar
//! `agent-<child id>.meta.json` naming the call, so a call still running links to its child and the
//! reader draws the child under it ([`super::discover::spawn_links`]). A child's growth is its
//! chat's, so the chat's position counts its children's.
//!
//! Nothing here writes to either database: both are opened read-only, each read in one short
//! transaction (the app's twice a refresh: its chats, then, once the mirrors are written, every
//! chat id it names, for pruning), and a read that fails leaves the mirror as it was.

// Off macOS there is no engine database to read (rusqlite is a macOS dependency), so the
// projection and the writer go unused there; the listing still finds mirrors made elsewhere.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use claude_replay_engine::seam::Agent;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::discover::Home;

/// Overrides the engine's database (tests, alternate installs).
const ENGINE_DB_ENV: &str = "QWENWORK_ENGINE_DB";
/// Overrides where the mirrors go.
const MIRROR_DIR_ENV: &str = "QWENWORK_MIRROR_DIR";

/// The `system` subtype the mirror writes for the files one of the engine's steps changed (#s662),
/// one of the family's subtypes ([`super::discover::FAMILY_SYSTEM_SUBTYPES`]).
pub(crate) const FILES_CHANGED: &str = "files_changed";

/// The engine's database, if this home has one and it is there.
///
/// Hermetic by construction: a process that moved the family's projects root (every test does)
/// finds the engine's database beside THAT root, where a test puts its own, never this machine's.
pub(crate) fn engine_db_for(home: &Home) -> Option<PathBuf> {
    if home.agent != Agent::QWENWORK {
        return None;
    }
    let p = if let Some(p) = std::env::var_os(ENGINE_DB_ENV) {
        PathBuf::from(p)
    } else if let Some(projects) = std::env::var_os(home.projects_env) {
        PathBuf::from(projects)
            .parent()?
            .join(".harness/flash-cli/qwenwork/qwenwork.db")
    } else {
        claude_replay_engine::seam::home_dir()?
            .join(home.dir)
            .join(".harness/flash-cli/qwenwork/qwenwork.db")
    };
    p.is_file().then_some(p)
}

/// Where this home's mirrors go: `$QWENWORK_MIRROR_DIR`; beside a moved projects root (tests);
/// else the tool's own cache (`<cache home>/qwenwork-opencode/projects`), never the agent's home,
/// which is the app's.
pub(crate) fn mirror_root_for(home: &Home) -> Option<PathBuf> {
    if home.agent != Agent::QWENWORK {
        return None;
    }
    if let Some(p) = std::env::var_os(MIRROR_DIR_ENV) {
        return Some(PathBuf::from(p));
    }
    if let Some(projects) = std::env::var_os(home.projects_env) {
        return Some(PathBuf::from(projects).parent()?.join("opencode-mirror"));
    }
    Some(claude_replay_engine::seam::cache_home()?.join("qwenwork-opencode/projects"))
}

/// One opencode chat, as the app database names it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Chat {
    /// `sub_chats.session_id`: the chat's uuid, the mirror's stem and the title's key.
    pub(crate) id: String,
    /// The folder the chat works in (`engineSessionBindingV1.workingDirectory`).
    pub(crate) dir: String,
    /// The engine session the binding names now.
    pub(crate) native: String,
    /// How many engine sessions the binding says the chat has been (`generation`), when it says.
    pub(crate) generation: Option<u64>,
}

/// One engine session: its row and its events, in `seq` order.
#[derive(Debug, Clone, Default)]
pub(crate) struct Session {
    pub(crate) id: String,
    pub(crate) directory: String,
    pub(crate) version: String,
    pub(crate) created_ms: i64,
    pub(crate) events: Vec<(i64, String, Value)>,
    /// The session that ran this one as a sub-agent (`parent_id`); `None` for a chat's own.
    pub(crate) parent: Option<String>,
    /// The engine's agent for the session (`general` for a sub-agent run), when it names one.
    pub(crate) agent: String,
}

/// What makes a projection a sub-agent's (#s659): the child session's own id, which is its
/// `agentId` and its file's name, and the parent's call that ran it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Side {
    pub(crate) agent_id: String,
    pub(crate) tool_use_id: String,
}

/// A sub-agent call in a projection's events: the child session it ran (`state.metadata.sessionId`,
/// named from the call's first running update, measured), the call's id and its description.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Spawn {
    pub(crate) child: String,
    pub(crate) call: String,
    pub(crate) description: String,
}

/// The messages and parts a removal event took out of `sessions`: a projection leaves them out
/// from the start, since the log keeps the events that wrote them.
fn removed_in<'a>(sessions: &[&'a Session]) -> HashSet<&'a str> {
    sessions
        .iter()
        .flat_map(|s| s.events.iter())
        .filter(|(_, ty, _)| is_removal(ty))
        .filter_map(|(_, ty, d)| {
            let key = if ty == "message.removed.1" {
                "messageID"
            } else {
                "partID"
            };
            d.get(key).and_then(Value::as_str)
        })
        .collect()
}

/// Whether an event of type `ty` takes a message or a part out.
fn is_removal(ty: &str) -> bool {
    ty == "message.removed.1" || ty == "message.part.removed.1"
}

/// Whether an event writes a message or part a removal took out.
fn is_removed<T: std::borrow::Borrow<str> + Eq + std::hash::Hash>(
    data: &Value,
    removed: &HashSet<T>,
) -> bool {
    data.pointer("/part/id")
        .or_else(|| data.pointer("/part/messageID"))
        .or_else(|| data.pointer("/info/id"))
        .and_then(Value::as_str)
        .is_some_and(|id| removed.contains(id))
        || data
            .pointer("/part/messageID")
            .and_then(Value::as_str)
            .is_some_and(|id| removed.contains(id))
}

/// The sub-agent calls in `sessions`' events, each child once, in the order they were named.
pub(crate) fn spawns(sessions: &[&Session]) -> Vec<Spawn> {
    let removed = removed_in(sessions);
    let mut out: Vec<Spawn> = Vec::new();
    for (_, ty, data) in sessions.iter().flat_map(|s| s.events.iter()) {
        note_spawn(&mut out, ty, data, &removed);
    }
    out
}

/// Adds to `out` the sub-agent call an event names, unless its child is there already or a
/// removal took the call out: [`spawns`], one event at a time.
fn note_spawn<T: std::borrow::Borrow<str> + Eq + std::hash::Hash>(
    out: &mut Vec<Spawn>,
    ty: &str,
    data: &Value,
    removed: &HashSet<T>,
) {
    if ty != "message.part.updated.1" || is_removed(data, removed) {
        return;
    }
    let Some(part) = data.get("part") else {
        return;
    };
    if part.get("tool").and_then(Value::as_str) != Some("task") {
        return;
    }
    let Some(child) = part
        .pointer("/state/metadata/sessionId")
        .and_then(Value::as_str)
    else {
        return;
    };
    if out.iter().any(|s| s.child == child) {
        return;
    }
    let call = part
        .get("callID")
        .or_else(|| part.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("");
    out.push(Spawn {
        child: child.to_string(),
        call: call.to_string(),
        description: part
            .pointer("/state/input/description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    });
}

/// Every session run below `roots` as a sub-agent, at any depth, in the order they began: the
/// engine records each child's parent (`parent_id`), which is the tree whatever names it.
pub(crate) fn descendants<'a>(roots: &[&Session], all: &'a [Session]) -> Vec<&'a Session> {
    let mut known: HashSet<&str> = roots.iter().map(|s| s.id.as_str()).collect();
    let mut out: Vec<&Session> = Vec::new();
    loop {
        let more: Vec<&Session> = all
            .iter()
            .filter(|s| {
                s.parent.as_deref().is_some_and(|p| known.contains(p))
                    && !known.contains(s.id.as_str())
            })
            .collect();
        if more.is_empty() {
            break;
        }
        for s in more {
            known.insert(&s.id);
            out.push(s);
        }
    }
    out.sort_by(|a, b| a.created_ms.cmp(&b.created_ms).then(a.id.cmp(&b.id)));
    out
}

/// The engine sessions a chat is made of, in the order they began.
///
/// The engine does not record which chat a session is; only the chat's binding names its current
/// session, and how many it has had. So in a folder that is the chat's alone, every top-level
/// session there is one of its generations. In a folder several chats share, the sessions no chat
/// names now are earlier generations of one of them, which says whose only when one chat there is
/// missing any, and they are exactly as many as it misses and began before its current one. Every
/// join must come to the count the binding gives; any other case, a binding that gives no count
/// included, is the bound session alone.
pub(crate) fn generations<'a>(
    chat: &Chat,
    chats: &[Chat],
    sessions: &'a [Session],
) -> Vec<&'a Session> {
    let Some(native) = sessions.iter().find(|s| s.id == chat.native) else {
        return Vec::new();
    };
    let neighbours = chats.iter().any(|c| c.id != chat.id && c.dir == chat.dir);
    let in_dir = sessions
        .iter()
        .filter(|s| s.directory == chat.dir && s.id != native.id);
    let earlier: Vec<&Session> = if !neighbours {
        in_dir.collect()
    } else {
        let wanting: Vec<&Chat> = chats
            .iter()
            .filter(|c| c.dir == chat.dir && c.generation.is_some_and(|g| g > 1))
            .collect();
        let spare: Vec<&Session> = in_dir
            .filter(|s| !chats.iter().any(|c| c.native == s.id))
            .collect();
        let mine = wanting.len() == 1
            && wanting[0].id == chat.id
            && spare.iter().all(|s| s.created_ms <= native.created_ms);
        if mine {
            spare
        } else {
            Vec::new()
        }
    };
    let mut out = earlier;
    out.push(native);
    out.sort_by(|a, b| a.created_ms.cmp(&b.created_ms).then(a.id.cmp(&b.id)));
    match chat.generation {
        Some(g) if out.len() as u64 == g => out,
        _ => vec![native],
    }
}

/// A timestamp in the records' form, from the engine's milliseconds.
fn iso(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// The engine's tool, in the names and input shapes the family's reader draws (Claude Code's).
/// A tool with no counterpart keeps its own name and input.
fn tool_call(tool: &str, input: &Value) -> (String, Value) {
    let s = |k: &str| input.get(k).cloned().unwrap_or(Value::Null);
    let pick = |pairs: &[(&str, &str)]| {
        let mut o = serde_json::Map::new();
        for (from, to) in pairs {
            if let Some(v) = input.get(*from) {
                o.insert((*to).to_string(), v.clone());
            }
        }
        Value::Object(o)
    };
    let bash = tool == "bash" || tool.ends_with("__bash");
    match tool {
        _ if bash => (
            "Bash".into(),
            pick(&[
                ("command", "command"),
                ("description", "description"),
                ("timeout", "timeout"),
            ]),
        ),
        "read" => (
            "Read".into(),
            pick(&[
                ("filePath", "file_path"),
                ("offset", "offset"),
                ("limit", "limit"),
            ]),
        ),
        "edit" => (
            "Edit".into(),
            pick(&[
                ("filePath", "file_path"),
                ("oldString", "old_string"),
                ("newString", "new_string"),
                ("replaceAll", "replace_all"),
            ]),
        ),
        "write" => (
            "Write".into(),
            pick(&[("filePath", "file_path"), ("content", "content")]),
        ),
        "glob" => (
            "Glob".into(),
            pick(&[("pattern", "pattern"), ("path", "path")]),
        ),
        "grep" => (
            "Grep".into(),
            pick(&[
                ("pattern", "pattern"),
                ("path", "path"),
                ("include", "glob"),
            ]),
        ),
        "task" => (
            "Agent".into(),
            pick(&[
                ("description", "description"),
                ("prompt", "prompt"),
                ("subagent_type", "subagent_type"),
            ]),
        ),
        "todowrite" => ("TodoWrite".into(), pick(&[("todos", "todos")])),
        "webfetch" => (
            "WebFetch".into(),
            pick(&[("url", "url"), ("prompt", "prompt")]),
        ),
        "skill" => ("Skill".into(), json!({"skill": s("name")})),
        "question" => (
            "AskUserQuestion".into(),
            pick(&[("questions", "questions")]),
        ),
        // The app's own tools: `qw-builtin_<name>` here, `mcp__qw-builtin__<name>` in the
        // family's JSONL, which is the name its readers know.
        _ => match tool.strip_prefix("qw-builtin_") {
            Some(rest) => (format!("mcp__qw-builtin__{rest}"), input.clone()),
            None => (tool.to_string(), input.clone()),
        },
    }
}

/// What the family's reader takes from a result beyond its text, in Claude Code's
/// `toolUseResult` keys: a sub-agent's end (`status`, `content`, and the engine session it ran
/// as for `agentId`), and a question's answers (`answers`, the question's text to the labels
/// chosen; the engine keeps one list of labels per question, in the questions' order).
fn tool_result(tool: &str, part: &Value, error: bool, out: &str) -> Option<Value> {
    match tool {
        "task" => {
            let mut r = json!({
                "status": if error { "failed" } else { "completed" },
                "content": out,
            });
            if let Some(child) = part
                .pointer("/state/metadata/sessionId")
                .and_then(Value::as_str)
            {
                r["agentId"] = json!(child);
            }
            Some(r)
        }
        "question" => {
            let questions = part.pointer("/state/input/questions")?.as_array()?;
            let chosen = part.pointer("/state/metadata/answers")?.as_array()?;
            let mut answers = serde_json::Map::new();
            for (q, a) in questions.iter().zip(chosen) {
                let Some(text) = q.get("question").and_then(Value::as_str) else {
                    continue;
                };
                let labels: Vec<&str> = a
                    .as_array()
                    .map(|l| l.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                answers.insert(text.to_string(), json!(labels.join(", ")));
            }
            Some(json!({ "questions": [], "answers": answers }))
        }
        _ => None,
    }
}

/// The tokens a step-finish (or a message) recorded, in the usage shape the family's metrics read.
fn usage(tokens: &Value) -> Option<Value> {
    let n = |p: &str| tokens.pointer(p).and_then(Value::as_u64).unwrap_or(0);
    let u = json!({
        "input_tokens": n("/input"),
        "output_tokens": n("/output") + n("/reasoning"),
        "cache_read_input_tokens": n("/cache/read"),
        "cache_creation_input_tokens": n("/cache/write"),
    });
    (tokens.is_object()).then_some(u)
}

/// What a part has had written so far.
#[derive(Default, Clone, Copy)]
struct Done {
    /// The part's own record (a reply, a thought, a prompt, a tool's call).
    call: bool,
    /// A tool's result.
    result: bool,
}

/// The projection of a chat's engine sessions into the family's records. It takes the log in
/// pieces as well as whole (#s659): what it has read is all it keeps of it, never the events, and
/// of a part only what more it can write.
struct Projector {
    chat: Chat,
    /// A sub-agent's projection: its records are the sidechain's, as the family writes them.
    side: Option<Side>,
    /// What the removals in the whole log took out, left out of every piece.
    removed: HashSet<String>,
    /// The records written since [`Projector::take`] was last asked.
    lines: Vec<String>,
    last_uuid: Option<String>,
    roles: HashMap<String, String>,
    models: HashMap<String, String>,
    parts: BTreeMap<String, Value>,
    done: HashMap<String, Done>,
    version: String,
    /// Each message's own times (`created`, `completed`), and the latest time any of its parts
    /// named: what a part with no time of its own is dated by (a prompt's text, a step's end).
    created: HashMap<String, i64>,
    completed: HashMap<String, i64>,
    latest: HashMap<String, i64>,
    /// The latest time written, the last resort, so no record is ever dated 1970.
    last_ms: i64,
}

impl Projector {
    fn new(chat: &Chat, side: Option<&Side>) -> Self {
        Projector {
            chat: chat.clone(),
            side: side.cloned(),
            removed: HashSet::new(),
            lines: Vec::new(),
            last_uuid: None,
            roles: HashMap::new(),
            models: HashMap::new(),
            parts: BTreeMap::new(),
            done: HashMap::new(),
            version: String::new(),
            created: HashMap::new(),
            completed: HashMap::new(),
            latest: HashMap::new(),
            last_ms: 0,
        }
    }

    /// Takes the next of a session's events (`version` the session's own), leaving out what a
    /// removal took out.
    fn feed(&mut self, version: &str, events: &[(i64, String, Value)]) {
        self.version = version.to_string();
        for (_, ty, data) in events {
            if !is_removed(data, &self.removed) {
                self.event(ty, data);
            }
        }
    }

    /// The records written since this was last asked.
    fn take(&mut self) -> Vec<String> {
        std::mem::take(&mut self.lines)
    }

    /// When a record of message `msg` happened: its part's own time when it has one, else the
    /// latest its message's parts named, else the message's end, else its start, else the latest
    /// time written.
    fn when(&self, msg: &str, own: Option<i64>) -> i64 {
        own.filter(|t| *t > 0)
            .or_else(|| self.latest.get(msg).copied())
            .or_else(|| self.completed.get(msg).copied())
            .or_else(|| self.created.get(msg).copied())
            .unwrap_or(self.last_ms)
    }

    fn emit(&mut self, uuid: String, ms: i64, kind: &str, mut rec: Value) {
        let o = rec.as_object_mut().expect("a record is an object");
        o.insert("type".into(), json!(kind));
        o.insert("uuid".into(), json!(uuid));
        o.insert("parentUuid".into(), json!(self.last_uuid));
        o.insert("timestamp".into(), json!(iso(ms)));
        self.last_ms = self.last_ms.max(ms);
        o.insert("sessionId".into(), json!(self.chat.id));
        o.insert("cwd".into(), json!(self.chat.dir));
        o.insert("isSidechain".into(), json!(self.side.is_some()));
        if let Some(side) = &self.side {
            // The family's sub-agent records: the child's id, and the call that ran it.
            o.insert("agentId".into(), json!(side.agent_id));
            o.insert("parent_tool_use_id".into(), json!(side.tool_use_id));
        }
        o.insert("userType".into(), json!("external"));
        if !self.version.is_empty() {
            o.insert("version".into(), json!(self.version));
        }
        self.lines.push(rec.to_string());
        self.last_uuid = Some(uuid);
    }

    fn assistant(
        &mut self,
        uuid: String,
        ms: i64,
        msg: &str,
        content: Value,
        usage: Option<Value>,
    ) {
        let mut m = json!({
            "id": msg,
            "type": "message",
            "role": "assistant",
            "model": self.models.get(msg).cloned().unwrap_or_default(),
            "content": [content],
            "stop_reason": null,
            "stop_sequence": null,
        });
        if let Some(u) = usage {
            m["usage"] = u;
        }
        self.emit(uuid, ms, "assistant", json!({ "message": m }));
    }

    /// Writes what of `part` is final and not yet written; `flush` writes it as it stands.
    fn part(&mut self, part: &Value, flush: bool) {
        let Some(id) = part.get("id").and_then(Value::as_str) else {
            return;
        };
        let msg = part.get("messageID").and_then(Value::as_str).unwrap_or("");
        let Some(role) = self.roles.get(msg).cloned() else {
            return; // its message is not known yet: written when it is
        };
        let done = self.done.get(id).copied().unwrap_or_default();
        let ty = part.get("type").and_then(Value::as_str).unwrap_or("");
        let start = part.pointer("/time/start").and_then(Value::as_i64);
        let end = part.pointer("/time/end").and_then(Value::as_i64);
        let seen = [
            start,
            end,
            part.pointer("/state/time/start").and_then(Value::as_i64),
            part.pointer("/state/time/end").and_then(Value::as_i64),
        ]
        .into_iter()
        .flatten()
        .filter(|t| *t > 0)
        .max();
        if let Some(t) = seen {
            let l = self.latest.entry(msg.to_string()).or_insert(t);
            *l = (*l).max(t);
        }
        let mut now = done;
        match ty {
            "text" if !done.call => {
                if part.get("ignored").and_then(Value::as_bool) == Some(true) {
                    // Never written as it stands: let go until an update says otherwise.
                    self.parts.remove(id);
                    return;
                }
                let text = part.get("text").and_then(Value::as_str).unwrap_or("");
                if role == "user" {
                    let meta = part.get("synthetic").and_then(Value::as_bool) == Some(true);
                    // A typed prompt's shape in Claude Code's transcripts: the text as a string.
                    let mut rec = json!({ "message": { "role": "user", "content": text } });
                    if meta {
                        rec["isMeta"] = json!(true);
                    }
                    let at = self.when(msg, start);
                    self.emit(id.to_string(), at, "user", rec);
                    now.call = true;
                } else if end.is_some() || flush {
                    if !text.is_empty() {
                        let at = self.when(msg, end.or(start));
                        self.assistant(
                            id.into(),
                            at,
                            msg,
                            json!({ "type": "text", "text": text }),
                            None,
                        );
                    }
                    now.call = true;
                }
            }
            "reasoning" if !done.call && (end.is_some() || flush) => {
                let text = part.get("text").and_then(Value::as_str).unwrap_or("");
                if !text.is_empty() {
                    let at = self.when(msg, end.or(start));
                    self.assistant(
                        id.into(),
                        at,
                        msg,
                        json!({ "type": "thinking", "thinking": text, "signature": "" }),
                        None,
                    );
                }
                now.call = true;
            }
            "tool" => {
                let status = part
                    .pointer("/state/status")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let call_id = part.get("callID").and_then(Value::as_str).unwrap_or(id);
                let began = part.pointer("/state/time/start").and_then(Value::as_i64);
                let ended = part.pointer("/state/time/end").and_then(Value::as_i64);
                let input = part.pointer("/state/input").cloned().unwrap_or(json!({}));
                let finished = status == "completed" || status == "error";
                if !done.call && (status == "running" || finished || flush) {
                    let tool = part.get("tool").and_then(Value::as_str).unwrap_or("tool");
                    let (name, input) = tool_call(tool, &input);
                    let at = self.when(msg, began);
                    self.assistant(
                        id.into(),
                        at,
                        msg,
                        json!({ "type": "tool_use", "id": call_id, "name": name, "input": input }),
                        None,
                    );
                    now.call = true;
                }
                if now.call && !done.result && (finished || flush) {
                    let error = status != "completed";
                    let out = if error {
                        part.pointer("/state/error")
                            .and_then(Value::as_str)
                            .unwrap_or(if finished {
                                ""
                            } else {
                                "(stopped before it finished)"
                            })
                    } else {
                        part.pointer("/state/output")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                    };
                    let mut block =
                        json!({ "type": "tool_result", "tool_use_id": call_id, "content": out });
                    if error {
                        block["is_error"] = json!(true);
                    }
                    let mut rec = json!({ "message": { "role": "user", "content": [block] }, "sourceToolAssistantUUID": id });
                    let tool = part.get("tool").and_then(Value::as_str).unwrap_or("");
                    if let Some(r) = tool_result(tool, part, error, out) {
                        rec["toolUseResult"] = r;
                    }
                    let at = self.when(msg, ended.or(began));
                    self.emit(format!("{id}-result"), at, "user", rec);
                    now.result = true;
                }
            }
            "step-finish" if !done.call => {
                // The step's tokens and how it ended, on a record of the step's own: a reply
                // drawn earlier had them unknown, and the metrics credit a message's usage once.
                // A step that stopped ends the turn (`end_turn`), one that called tools does not.
                let stop = match part.get("reason").and_then(Value::as_str) {
                    Some("stop") => json!("end_turn"),
                    Some("tool-calls") => json!("tool_use"),
                    Some("length") => json!("max_tokens"),
                    _ => Value::Null,
                };
                let mut tokens = part.get("tokens").and_then(usage);
                // What the engine figured the step cost, in US dollars from its model's configured
                // rates per million tokens (#s662), where it figured any: every step of this Mac's
                // chats says 0, its custom model having no rates, and a 0 is no figure.
                if let Some(cost) = part
                    .get("cost")
                    .and_then(Value::as_f64)
                    .filter(|c| c.is_finite() && *c > 0.0)
                {
                    tokens.get_or_insert_with(|| json!({}))["cost_usd"] = json!(cost);
                }
                if tokens.is_some() || !stop.is_null() {
                    let mut m = json!({ "id": id, "type": "message", "role": "assistant", "model": self.models.get(msg).cloned().unwrap_or_default(), "content": [], "stop_reason": stop, "stop_sequence": null });
                    if let Some(u) = tokens {
                        m["usage"] = u;
                    }
                    let at = self.when(msg, end.or(start));
                    self.emit(id.to_string(), at, "assistant", json!({ "message": m }));
                }
                now.call = true;
            }
            "patch" if !done.call => {
                // The files a step changed (#s662): the engine compares its snapshot from the
                // step's start with the folder at its end and writes this after the step-finish,
                // once, when anything changed, whatever changed it (a command's change included,
                // where a call names only its own file). On a `system` record of the family's own
                // subtype, which the family's decoder draws as a note.
                let files: Vec<&str> = part
                    .get("files")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                if !files.is_empty() {
                    let at = self.when(msg, None);
                    self.emit(
                        id.to_string(),
                        at,
                        "system",
                        json!({ "subtype": FILES_CHANGED, "files": files }),
                    );
                }
                now.call = true;
            }
            _ => {}
        }
        self.done.insert(id.to_string(), now);
        // A part that can write nothing more is let go, so what is kept between refreshes is
        // what is still to come (#s659): a message's end writes only what is unwritten, and an
        // update of the part brings it back. A kind this does not draw never writes.
        let spent = match ty {
            "tool" => now.call && now.result,
            "text" | "reasoning" | "step-finish" | "patch" => now.call,
            _ => true,
        };
        if spent {
            self.parts.remove(id);
        }
    }

    fn event(&mut self, ty: &str, data: &Value) {
        match ty {
            "message.updated.1" => {
                let Some(info) = data.get("info") else { return };
                let Some(id) = info.get("id").and_then(Value::as_str) else {
                    return;
                };
                let new = !self.roles.contains_key(id);
                if let Some(r) = info.get("role").and_then(Value::as_str) {
                    self.roles.insert(id.to_string(), r.to_string());
                }
                if let Some(m) = info.get("modelID").and_then(Value::as_str) {
                    self.models.insert(id.to_string(), m.to_string());
                }
                for (key, times) in [
                    ("created", &mut self.created),
                    ("completed", &mut self.completed),
                ] {
                    if let Some(t) = info
                        .pointer(&format!("/time/{key}"))
                        .and_then(Value::as_i64)
                        .filter(|t| *t > 0)
                    {
                        times.insert(id.to_string(), t);
                    }
                }
                // Parts that came before their message are written now it is known.
                let mine: Vec<Value> = self
                    .parts
                    .values()
                    .filter(|p| p.get("messageID").and_then(Value::as_str) == Some(id))
                    .cloned()
                    .collect();
                let completed = info.pointer("/time/completed").is_some();
                if new || completed {
                    for p in &mine {
                        self.part(p, completed);
                    }
                }
            }
            "message.part.updated.1" => {
                let Some(part) = data.get("part") else { return };
                let Some(id) = part.get("id").and_then(Value::as_str) else {
                    return;
                };
                self.parts.insert(id.to_string(), part.clone());
                let p = part.clone();
                self.part(&p, false);
            }
            _ => {}
        }
    }
}

/// The mirror's records for `chat`, made of its `sessions` (its generations, in order), without
/// the first line: [`projected`]'s, taken.
#[cfg(test)]
pub(crate) fn project(chat: &Chat, sessions: &[&Session]) -> Vec<String> {
    projected(chat, sessions, None).take()
}

/// The projector that has taken `sessions` (a chat's generations, in order) whole, its records not
/// yet taken: what a refresh keeps to take what the log adds later (#s659). As a sub-agent's
/// transcript when `side` says whose: the same records, on the sidechain. Deterministic: the same
/// log gives the same records, and a longer log gives these and more, unless something was
/// removed; taken in pieces it gives what it gives taken whole.
fn projected(chat: &Chat, sessions: &[&Session], side: Option<&Side>) -> Projector {
    let mut p = Projector::new(chat, side);
    p.removed = removed_in(sessions)
        .into_iter()
        .map(str::to_owned)
        .collect();
    for s in sessions {
        p.feed(&s.version, &s.events);
    }
    p
}

/// The mirror's first line: the family's head record, naming the engine's model of the chat's last
/// generation. `built` is when this file was begun: a rebuilt mirror gets a new one, so its first
/// line differs from the file it replaces.
pub(crate) fn head(chat: &Chat, model: &str, built_ms: i64) -> String {
    json!({
        "type": "runtime-config",
        "sessionId": chat.id,
        "model": model,
        "reasoningEffort": null,
        "contextWindow": null,
        "timestamp": iso(built_ms),
    })
    .to_string()
}

/// How a mirror's file begins, which is what tells a reader a rebuilt file from the one it read.
pub(crate) enum Lead<'a> {
    /// A chat's mirror: a head record of its own, given when the file was begun.
    Head(&'a dyn Fn(i64) -> String),
    /// A sub-agent's (#s659): no head, as the family writes one (a head record is the family's
    /// alone, and the Claude reader that folds a child does not know it). Its first record carries
    /// when the file was begun as its `promptId`, a key the family's records already have and no
    /// reader draws, so a rebuilt file's first line is new too.
    Stamp,
}

/// `line` with `promptId` set to the stamp of a file begun at `begun`.
fn stamped(line: &str, begun: i64) -> String {
    match serde_json::from_str::<Value>(line) {
        Ok(Value::Object(mut o)) => {
            o.insert("promptId".into(), json!(format!("mirror-{begun}")));
            Value::Object(o).to_string()
        }
        _ => line.to_string(),
    }
}

/// Whether `first`, a file's first line, is `line` under any stamp.
fn stamp_of(first: &str, line: &str) -> bool {
    let (Ok(Value::Object(mut a)), Ok(b)) = (
        serde_json::from_str::<Value>(first),
        serde_json::from_str::<Value>(line),
    ) else {
        return false;
    };
    a.remove("promptId").is_some() && Value::Object(a) == b
}

/// Brings the mirror at `path` to `lines`: nothing when it holds them; the new lines appended when
/// it holds a beginning of them; otherwise a new file under a new first line (`lead`, given when
/// it was begun), written whole and moved into place. The caller holds the mirrors' lock
/// ([`lock_mirrors`]) from its read of the engine's log to here, so a process that read an older
/// log can never write after one that read a newer. Files 0600 in 0700 folders: they are a copy of
/// the person's conversations.
pub(crate) fn write_mirror(
    path: &Path,
    lead: Lead<'_>,
    lines: &[String],
    now_ms: i64,
) -> std::io::Result<()> {
    use std::io::Write;
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("no parent"))?;
    create_private_dir(dir)?;
    // What follows the file's first line: every record under a head, the rest under a stamp.
    let (opening, after) = match lead {
        Lead::Head(_) => (None, lines),
        Lead::Stamp => match lines.split_first() {
            Some((first, rest)) => (Some(first), rest),
            None => return Ok(()),
        },
    };
    let body: String = after.iter().map(|l| format!("{l}\n")).collect();
    let old = std::fs::read_to_string(path).ok();
    if let Some(old) = &old {
        if let Some((first, rest)) = old.split_once('\n') {
            let same_start = match opening {
                None => !first.is_empty(),
                Some(line) => stamp_of(first, line),
            };
            if same_start && rest == body {
                return Ok(());
            }
            if same_start && body.starts_with(rest) {
                let mut f = std::fs::OpenOptions::new().append(true).open(path)?;
                f.write_all(&body.as_bytes()[rest.len()..])?;
                drop(f);
                return set_modified(path, lines);
            }
        }
    }
    // A new file: begun now when it replaces one (so its first line is new), else when the chat's
    // first record was.
    let begun = if old.is_some() {
        now_ms
    } else {
        lines
            .first()
            .and_then(|l| serde_json::from_str::<Value>(l).ok())
            .and_then(|v| {
                v.get("timestamp")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .and_then(|t| parse_iso_ms(&t))
            .unwrap_or(now_ms)
    };
    let first = match (lead, opening) {
        (Lead::Head(head_at), _) => head_at(begun),
        (Lead::Stamp, Some(line)) => stamped(line, begun),
        (Lead::Stamp, None) => return Ok(()),
    };
    write_whole(path, format!("{first}\n{body}").as_bytes(), lines)
}

/// Appends `lines` to the mirror at `path`, which the caller knows holds the records before them
/// (#s659: the file as this process left it), and dates it by `latest_ms`, the latest time any of
/// its records names. Under the mirrors' lock, as every write is.
fn append(path: &Path, lines: &[String], latest_ms: i64) -> std::io::Result<()> {
    use std::io::Write;
    let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
    let mut f = std::fs::OpenOptions::new().append(true).open(path)?;
    f.write_all(body.as_bytes())?;
    drop(f);
    set_modified_at(path, latest_ms)
}

/// `bytes` at `path`, written beside it and moved into place, 0600, dated by the latest of `lines`
/// ([`set_modified`]): a reader sees the old file or the new one, never part of one.
fn write_whole(path: &Path, bytes: &[u8], lines: &[String]) -> std::io::Result<()> {
    use std::io::Write;
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    let tmp = path.with_file_name(name);
    {
        let mut f = private_file(&tmp, true)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    set_modified(&tmp, lines)?;
    std::fs::rename(&tmp, path)
}

/// A sub-agent's sidecar (#s659), `agent-<id>.meta.json` beside its transcript, in the family's
/// shape: the call that ran it (`toolUseId`, the parent mirror's `tool_use` id), so a call still
/// running links to its child before its result names it, and the agent's kind and description.
/// Written only when it would change.
pub(crate) fn write_meta(
    path: &Path,
    side: &Side,
    agent: &str,
    description: &str,
) -> std::io::Result<()> {
    let body = json!({
        "toolUseId": side.tool_use_id,
        "agentType": agent,
        "description": description,
    })
    .to_string();
    if std::fs::read_to_string(path).ok().as_deref() == Some(body.as_str()) {
        return Ok(());
    }
    create_private_dir(
        path.parent()
            .ok_or_else(|| std::io::Error::other("no parent"))?,
    )?;
    write_whole(path, body.as_bytes(), &[])
}

/// The milliseconds of a timestamp [`iso`] wrote (the mirror reads back only its own).
fn parse_iso_ms(t: &str) -> Option<i64> {
    let n = |a: usize, b: usize| t.get(a..b)?.parse::<i64>().ok();
    let (y, m, d) = (n(0, 4)?, n(5, 7)?, n(8, 10)?);
    let (h, mi, sec, ms) = (n(11, 13)?, n(14, 16)?, n(17, 19)?, n(20, 23)?);
    // Days since 1970-01-01 from a civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 86_400 + h * 3600 + mi * 60 + sec) * 1000) + ms)
}

/// The latest time the records name: the mirror's modified time, so a chat reads as active when
/// it last was, not when the mirror was written.
fn latest_ms(lines: &[String]) -> Option<i64> {
    lines
        .iter()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| {
            v.get("timestamp")
                .and_then(Value::as_str)
                .and_then(parse_iso_ms)
        })
        .max()
}

fn set_modified(path: &Path, lines: &[String]) -> std::io::Result<()> {
    set_modified_at(path, latest_ms(lines).unwrap_or(0))
}

/// Dates the file at `path` by `ms`, the latest time its records name (none when it is 0).
fn set_modified_at(path: &Path, ms: i64) -> std::io::Result<()> {
    if ms <= 0 {
        return Ok(());
    }
    let at = std::time::UNIX_EPOCH + std::time::Duration::from_millis(ms as u64);
    std::fs::OpenOptions::new()
        .append(true)
        .open(path)?
        .set_modified(at)
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

fn private_file(path: &Path, truncate: bool) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(truncate)
        .mode(0o600)
        .open(path)
}

/// The lock every process holds while it brings this root's mirrors up to date, beside the root
/// (so the store's walk never meets it). `None` when another process holds it for longer than a
/// second: that one is bringing them up to date, and this one leaves them to it.
pub(crate) fn lock_mirrors(root: &Path) -> Option<std::fs::File> {
    let parent = root.parent()?;
    create_private_dir(parent).ok()?;
    let name = format!("{}.lock", root.file_name()?.to_string_lossy());
    let f = private_file(&parent.join(name), false).ok()?;
    for _ in 0..20 {
        match f.try_lock() {
            Ok(()) => return Some(f),
            Err(std::fs::TryLockError::WouldBlock) => {
                std::thread::sleep(std::time::Duration::from_millis(50))
            }
            Err(std::fs::TryLockError::Error(_)) => return None,
        }
    }
    None
}

/// The mark a folder `QWENWORK_MIRROR_DIR` names carries once the mirror has taken it (#s659).
const ROOT_MARK: &str = ".opencode-mirror";

/// Whether the mirror may write and prune under `root` (#s659). A root of its own naming is its
/// own. One `QWENWORK_MIRROR_DIR` names (`overridden`) is its own once it carries the mark, which
/// the mirror puts in a folder that is new or empty: a folder holding anything else is someone's,
/// and prune would delete there whatever it took for a deleted chat's mirror.
pub(crate) fn owns_root(root: &Path, overridden: bool) -> bool {
    if !overridden {
        return true;
    }
    let mark = root.join(ROOT_MARK);
    if mark.is_file() {
        return true;
    }
    let empty = match std::fs::read_dir(root) {
        Ok(mut entries) => entries.next().is_none(),
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    };
    empty && create_private_dir(root).is_ok() && private_file(&mark, true).is_ok()
}

/// Removes the files of `chat`'s sub-agents that are not among `kids` (#s659): a child whose
/// generation stopped being the chat's (a neighbour in its folder gained one), or one the engine
/// no longer has. Left there, they would stay listed, and the monitor would bank a child's cost
/// onto a chat whose mirror no longer shows it. Only the writer's own names are touched.
fn remove_gone_subagents(root: &Path, chat: &Chat, kids: &HashSet<&str>) {
    let dir = root
        .join(slug_for(&chat.dir))
        .join(&chat.id)
        .join("subagents");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(id) = name.to_str().and_then(|n| {
            let n = n.strip_prefix("agent-")?;
            n.strip_suffix(".jsonl")
                .or_else(|| n.strip_suffix(".jsonl.tmp"))
                .or_else(|| n.strip_suffix(".meta.json"))
        }) else {
            continue;
        };
        if !kids.contains(id) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// The slug the store gives a folder: '/' and '.' as '-'.
fn slug_for(dir: &str) -> String {
    dir.replace(['/', '.'], "-")
}

/// Where `chat`'s mirror is under `root`.
pub(crate) fn mirror_path(root: &Path, chat: &Chat) -> PathBuf {
    root.join(slug_for(&chat.dir))
        .join(format!("{}.jsonl", chat.id))
}

/// Where the transcript of `chat`'s sub-agent `agent_id` is under `root` (#s659): the family's
/// layout, `<the chat's mirror without .jsonl>/subagents/agent-<id>.jsonl`, which the reader finds
/// from a call's `agentId` and the sweeps walk.
pub(crate) fn subagent_path(root: &Path, chat: &Chat, agent_id: &str) -> PathBuf {
    root.join(slug_for(&chat.dir))
        .join(&chat.id)
        .join("subagents")
        .join(format!("agent-{agent_id}.jsonl"))
}

/// Removes what the mirrors keep for chats the app no longer has (#s659), and returns how many
/// chats' it removed. The app deletes a chat's row (`deleteSubChat` in its bundle, 2.1.0), so a
/// mirror whose stem `keep` (every chat id the app names, read whole) does not name is a deleted
/// chat's; and one a chat has left is at a folder it no longer works in (`current`, each mirrored
/// chat's mirror now). Only what the writer makes is touched: `<slug>/<id>.jsonl`, its `.tmp`, and
/// its sub-agents' `<slug>/<id>/`; a slug left empty goes too.
pub(crate) fn prune(
    root: &Path,
    keep: &HashSet<String>,
    current: &HashMap<String, PathBuf>,
) -> usize {
    let Ok(slugs) = std::fs::read_dir(root) else {
        return 0;
    };
    let mut gone: HashSet<String> = HashSet::new();
    for slug in slugs.flatten() {
        let dir = slug.path();
        if !slug.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
            let id = if is_dir {
                name
            } else if let Some(id) = name
                .strip_suffix(".jsonl.tmp")
                .or_else(|| name.strip_suffix(".jsonl"))
            {
                id
            } else {
                continue;
            };
            let here = dir.join(format!("{id}.jsonl"));
            let stale = !keep.contains(id)
                || current.get(id).is_some_and(|now| {
                    now != &here && !now.parent().is_some_and(|p| same_folder(p, &dir))
                });
            if !stale {
                continue;
            }
            let removed = if is_dir {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            if removed.is_ok() {
                gone.insert(id.to_string());
            }
        }
        // A folder no chat is left in.
        let _ = std::fs::remove_dir(&dir);
    }
    gone.len()
}

/// Whether two paths name one folder: the same spelling, or one folder on disk under two (a
/// case-insensitive disk, as a Mac's is by default, takes `-w-A` and `-w-a` for one folder, and so
/// do two chats whose folders differ only in case: the first one's spelling is the folder's).
fn same_folder(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(x), Ok(y)) = (std::fs::metadata(a), std::fs::metadata(b)) {
            return (x.dev(), x.ino()) == (y.dev(), y.ino());
        }
    }
    false
}

/// Whether an engine id can name a file as it is: the engine's ids are `ses_` and letters and
/// digits, and one that is anything else (a separator, a dot) is not written out.
fn file_safe(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Brings every opencode chat's mirror up to date, at most once every two seconds per process (a
/// scan asks for each listing several times). Quietly nothing where this home has no engine.
pub(crate) fn refresh(home: &Home) {
    #[cfg(target_os = "macos")]
    db::refresh(home, true);
    #[cfg(not(target_os = "macos"))]
    let _ = home;
}

/// [`refresh`] without the two seconds' rest, for a test that changes the engine's log and looks.
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn refresh_now(home: &Home) {
    db::refresh(home, false);
}

#[cfg(target_os = "macos")]
mod db {
    use super::*;
    use rusqlite::{Connection, OpenFlags};
    use std::sync::Mutex;
    use std::time::{Duration, Instant, SystemTime};

    /// What this process keeps of each chat it brought up to date, and when it last looked, so an
    /// unchanged chat costs one small query and one that grew costs what it grew by.
    struct Memo {
        at: Option<Instant>,
        /// The database looked at, so a test moving it starts afresh.
        db: PathBuf,
        kept: HashMap<String, Kept>,
    }
    static MEMO: Mutex<Option<Memo>> = Mutex::new(None);

    type Event = (i64, String, Value);

    /// A chat as this process last brought its mirrors up to date (#s659): each mirror's projector
    /// past the events it read, and the file it left, so the next refresh reads and projects only
    /// what the log added. Nothing of it is written down: a process that has kept nothing (a new
    /// one, or one that cannot grow what it kept) projects the chat's whole log once, as every
    /// refresh did before.
    struct Kept {
        /// The log positions it was brought to: the chat's generations, then its sub-agents.
        at: Vec<(String, i64)>,
        /// The chat's generations, in order.
        gens: Vec<String>,
        /// The last event read of each session.
        read: HashMap<String, i64>,
        /// What every removal in the chat's sessions took out.
        removed: HashSet<String>,
        /// Each session's engine version, which every record of it carries.
        versions: HashMap<String, String>,
        main: Mirror,
        kids: HashMap<String, Mirror>,
        spawns: Vec<Spawn>,
    }

    /// One mirror's projector, and the file as this process left it holding exactly its records
    /// (`None` while it has written none).
    struct Mirror {
        p: Projector,
        file: Option<FileId>,
    }

    /// What tells a file another process changed from the one this process left.
    #[derive(Debug, Clone, Copy, PartialEq)]
    struct FileId {
        dev: u64,
        ino: u64,
        len: u64,
        modified: Option<SystemTime>,
    }

    fn file_id(path: &Path) -> Option<FileId> {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::metadata(path).ok()?;
        m.is_file().then(|| FileId {
            dev: m.dev(),
            ino: m.ino(),
            len: m.len(),
            modified: m.modified().ok(),
        })
    }

    /// What one refresh brings a chat's mirrors to.
    struct Target<'a> {
        chat: &'a Chat,
        root: &'a Path,
        gens: &'a [&'a Session],
        kids: &'a [&'a Session],
        at: &'a [(String, i64)],
        now_ms: i64,
    }

    /// A sub-agent's side: its own id, and the call that ran it where the log names one.
    fn side_of(s: &Session, spawns: &[Spawn]) -> Side {
        Side {
            agent_id: s.id.clone(),
            tool_use_id: spawns
                .iter()
                .find(|x| x.child == s.id)
                .map(|x| x.call.clone())
                .unwrap_or_default(),
        }
    }

    /// A sub-agent's sidecar beside its mirror at `file`, when the log names the call that ran it.
    fn write_spawn_meta(
        file: &Path,
        side: &Side,
        s: &Session,
        spawns: &[Spawn],
    ) -> std::io::Result<()> {
        match spawns.iter().find(|x| x.child == s.id) {
            Some(spawn) => write_meta(
                &file.with_file_name(format!("agent-{}.meta.json", s.id)),
                side,
                &s.agent,
                &spawn.description,
            ),
            None => Ok(()),
        }
    }

    fn open(path: &Path) -> rusqlite::Result<Connection> {
        let c = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        c.busy_timeout(Duration::from_millis(200))?;
        Ok(c)
    }

    /// The opencode chats the app database names.
    pub(super) fn chats(app: &Path) -> rusqlite::Result<Vec<Chat>> {
        let c = open(app)?;
        let mut q = c.prepare(
            "select session_id, ext from sub_chats \
             where json_valid(ext) and json_extract(ext, '$.engineSessionBindingV1.engineId') = 'opencode'",
        )?;
        // A step that fails is an error, never the end of the rows: a list cut short would read as
        // the whole of it. A VALUE that is not text (NULL, which the schema allows, a blob, text
        // that is not UTF-8) is that row's alone: it names no chat to mirror, and is skipped.
        let text = |r: &rusqlite::Row, i| -> rusqlite::Result<Option<String>> {
            Ok(r.get_ref(i)?.as_str().ok().map(str::to_owned))
        };
        let rows = q
            .query_map([], |r| Ok((text(r, 0)?, text(r, 1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut out = Vec::new();
        for row in rows {
            let (Some(id), Some(ext)) = row else {
                continue;
            };
            let row = (id, ext);
            let Ok(ext) = serde_json::from_str::<Value>(&row.1) else {
                continue;
            };
            let b = &ext["engineSessionBindingV1"];
            let (Some(dir), Some(native)) = (
                b.get("workingDirectory").and_then(Value::as_str),
                b.get("nativeSessionId").and_then(Value::as_str),
            ) else {
                continue;
            };
            out.push(Chat {
                id: row.0,
                dir: dir.to_string(),
                native: native.to_string(),
                generation: b.get("generation").and_then(Value::as_u64),
            });
        }
        Ok(out)
    }

    /// Every engine session with its log position: a chat's own, and the sub-agents' (#s659),
    /// whose parent says whose.
    ///
    /// A session with no sequence row yet is at -1: the engine numbers a session's events from 0,
    /// so 0 is a session holding one. A schema with no `agent` column (an engine older than the
    /// one measured) reads each agent as empty, which only a sub-agent's sidecar shows.
    fn positions(c: &Connection) -> rusqlite::Result<Vec<(Session, i64)>> {
        let has_agent: bool = c.query_row(
            "select count(*) > 0 from pragma_table_info('session') where name = 'agent'",
            [],
            |r| r.get(0),
        )?;
        let agent = if has_agent {
            "coalesce(s.agent, '')"
        } else {
            "''"
        };
        let mut q = c.prepare(&format!(
            "select s.id, s.directory, s.version, s.time_created, coalesce(q.seq, -1), \
                    s.parent_id, {agent} \
             from session s left join event_sequence q on q.aggregate_id = s.id"
        ))?;
        let rows = q.query_map([], |r| {
            Ok((
                Session {
                    id: r.get(0)?,
                    directory: r.get(1)?,
                    version: r.get(2)?,
                    created_ms: r.get(3)?,
                    events: Vec::new(),
                    parent: r.get(5)?,
                    agent: r.get(6)?,
                },
                r.get::<_, i64>(4)?,
            ))
        })?;
        rows.collect()
    }

    /// A session's events past `after`, and the last seq read (`after` when there are none): the
    /// last seq of the rows themselves, so what the next read starts past is what this one read,
    /// whatever the log's position says.
    fn events_after(c: &Connection, id: &str, after: i64) -> rusqlite::Result<(i64, Vec<Event>)> {
        let mut q = c.prepare(
            "select seq, type, data from event where aggregate_id = ?1 and seq > ?2 order by seq",
        )?;
        let rows = q
            .query_map(rusqlite::params![id, after], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        // Nothing read from the start is -1, the position of a session with nothing yet.
        let last = rows.last().map_or(after.max(-1), |(seq, _, _)| *seq);
        let events = rows
            .into_iter()
            .filter_map(|(seq, ty, d)| serde_json::from_str(&d).ok().map(|v| (seq, ty, v)))
            .collect();
        Ok((last, events))
    }

    /// `s` with all its events and the last seq read, each session's read once per refresh.
    fn with_events(
        c: &Connection,
        loaded: &mut HashMap<String, (i64, Vec<Event>)>,
        s: &Session,
    ) -> rusqlite::Result<(Session, i64)> {
        if !loaded.contains_key(&s.id) {
            loaded.insert(s.id.clone(), events_after(c, &s.id, i64::MIN)?);
        }
        let (last, events) = &loaded[&s.id];
        let mut out = s.clone();
        out.events = events.clone();
        Ok((out, *last))
    }

    /// Projects a chat's whole log and brings its mirrors to it, its sub-agents' first so a call's
    /// result never names a child whose file is not there yet: what a chat costs the first time
    /// this process brings it up to date, and whenever what it kept cannot grow. `None` when the
    /// chat has nothing to write yet or a write failed, so the next refresh tries again.
    fn build(
        c: &Connection,
        loaded: &mut HashMap<String, (i64, Vec<Event>)>,
        t: &Target,
    ) -> rusqlite::Result<Option<Kept>> {
        let mut read: HashMap<String, i64> = HashMap::new();
        let mut full: Vec<Session> = Vec::new();
        let mut subs: Vec<Session> = Vec::new();
        for (i, s) in t.gens.iter().chain(t.kids.iter()).enumerate() {
            let (s, last) = with_events(c, loaded, s)?;
            read.insert(s.id.clone(), last);
            if i < t.gens.len() {
                full.push(s);
            } else {
                subs.push(s);
            }
        }
        let refs: Vec<&Session> = full.iter().collect();
        let mut main = projected(t.chat, &refs, None);
        let lines = main.take();
        if lines.is_empty() {
            return Ok(None);
        }
        let named: Vec<&Session> = full.iter().chain(subs.iter()).collect();
        let removed: HashSet<String> = removed_in(&named).into_iter().map(str::to_owned).collect();
        let calls = spawns(&named);
        let mut ok = true;
        let mut kids: HashMap<String, Mirror> = HashMap::new();
        for k in &subs {
            let side = side_of(k, &calls);
            let mut p = projected(t.chat, &[k], Some(&side));
            let lines = p.take();
            let mut file = None;
            if !lines.is_empty() {
                let path = subagent_path(t.root, t.chat, &k.id);
                ok &= write_mirror(&path, Lead::Stamp, &lines, t.now_ms).is_ok();
                ok &= write_spawn_meta(&path, &side, k, &calls).is_ok();
                file = file_id(&path);
                ok &= file.is_some();
            }
            kids.insert(k.id.clone(), Mirror { p, file });
        }
        let ids: HashSet<&str> = t.kids.iter().map(|s| s.id.as_str()).collect();
        remove_gone_subagents(t.root, t.chat, &ids);
        let model = refs
            .last()
            .and_then(|s| {
                s.events
                    .iter()
                    .rev()
                    .find_map(|(_, _, d)| d.pointer("/info/modelID").and_then(Value::as_str))
            })
            .unwrap_or("")
            .to_string();
        let head_at = |ms: i64| head(t.chat, &model, ms);
        let path = mirror_path(t.root, t.chat);
        ok &= write_mirror(&path, Lead::Head(&head_at), &lines, t.now_ms).is_ok();
        let file = file_id(&path);
        ok &= file.is_some();
        Ok(ok.then(|| Kept {
            at: t.at.to_vec(),
            gens: t.gens.iter().map(|s| s.id.clone()).collect(),
            read,
            removed,
            versions: t
                .gens
                .iter()
                .chain(t.kids.iter())
                .map(|s| (s.id.clone(), s.version.clone()))
                .collect(),
            main: Mirror { p: main, file },
            kids,
            spawns: calls,
        }))
    }

    /// Brings what this process kept of a chat to the log's new positions from the events past
    /// what it read, appending what they add to each mirror: `None` when only the whole log can
    /// say. That is when an earlier generation moved or was not read to its end (its events come
    /// before the later one's), the generations are not the ones kept, a sub-agent kept is gone (a
    /// new one is taken in), a position went back, a session's version changed (every record of
    /// it carries the version), a removal came (it takes out what was written), a sub-agent's call
    /// is named only now (it is in every record of the child's), a file is not the one this
    /// process left (another process wrote it, or the chat was pruned), or a write failed.
    fn grow(c: &Connection, mut k: Kept, t: &Target) -> rusqlite::Result<Option<Kept>> {
        let (Some(last), earlier) = (t.gens.last(), &t.gens[..t.gens.len().saturating_sub(1)])
        else {
            return Ok(None);
        };
        let gens: Vec<&str> = t.gens.iter().map(|s| s.id.as_str()).collect();
        let pos = |at: &[(String, i64)], id: &str| at.iter().find(|(s, _)| s == id).map(|x| x.1);
        // A position that went back is a log that lost events (the engine's database put back
        // from an older copy): what was written from them only the whole log can take out.
        if gens != k.gens
            || earlier
                .iter()
                .any(|g| pos(t.at, &g.id) != pos(&k.at, &g.id))
            || k.kids.keys().any(|id| !t.kids.iter().any(|s| &s.id == id))
            || std::iter::once(last)
                .chain(t.kids.iter())
                .any(|s| pos(t.at, &s.id) < pos(&k.at, &s.id))
            || earlier.iter().any(|g| {
                k.read
                    .get(&g.id)
                    .is_none_or(|r| Some(*r) < pos(t.at, &g.id))
            })
            || t.gens
                .iter()
                .chain(t.kids.iter())
                .any(|s| k.versions.get(&s.id).is_some_and(|v| v != &s.version))
        {
            return Ok(None);
        }
        if file_id(&mirror_path(t.root, t.chat)) != k.main.file
            || k.kids.iter().any(|(id, m)| {
                m.file.is_some() && file_id(&subagent_path(t.root, t.chat, id)) != m.file
            })
        {
            return Ok(None);
        }
        // What the log added, the chat's own and then its sub-agents', as the whole log is taken;
        // a session already read to its position is not read again.
        let mut new: Vec<(&Session, Vec<Event>)> = Vec::new();
        for s in std::iter::once(last).chain(t.kids.iter()) {
            let read = k.read.get(&s.id).copied();
            if read.is_some_and(|r| Some(r) >= pos(t.at, &s.id)) {
                new.push((s, Vec::new()));
                continue;
            }
            k.versions.insert(s.id.clone(), s.version.clone());
            let (upto, events) = events_after(c, &s.id, read.unwrap_or(i64::MIN))?;
            if events.iter().any(|(_, ty, _)| is_removal(ty)) {
                return Ok(None);
            }
            k.read.insert(s.id.clone(), upto);
            new.push((s, events));
        }
        for (_, events) in &new {
            for (_, ty, data) in events {
                note_spawn(&mut k.spawns, ty, data, &k.removed);
            }
        }
        let mut ok = true;
        for (s, events) in &new[1..] {
            let side = side_of(s, &k.spawns);
            let m = k.kids.entry(s.id.clone()).or_insert_with(|| Mirror {
                p: Projector::new(t.chat, Some(&side)),
                file: None,
            });
            if m.p.side.as_ref() != Some(&side) {
                return Ok(None);
            }
            m.p.feed(&s.version, events);
            let lines = m.p.take();
            let path = subagent_path(t.root, t.chat, &s.id);
            if !lines.is_empty() {
                ok &= match m.file {
                    Some(_) => append(&path, &lines, m.p.last_ms).is_ok(),
                    // Its first records: the whole of what it has written.
                    None => write_mirror(&path, Lead::Stamp, &lines, t.now_ms).is_ok(),
                };
                m.file = file_id(&path);
                ok &= m.file.is_some();
            }
            if m.file.is_some() {
                ok &= write_spawn_meta(&path, &side, s, &k.spawns).is_ok();
            }
        }
        let (_, events) = &new[0];
        k.main.p.feed(&last.version, events);
        let lines = k.main.p.take();
        if !lines.is_empty() {
            let path = mirror_path(t.root, t.chat);
            ok &= append(&path, &lines, k.main.p.last_ms).is_ok();
            k.main.file = file_id(&path);
            ok &= k.main.file.is_some();
        }
        k.at = t.at.to_vec();
        Ok(ok.then_some(k))
    }

    pub(super) fn refresh(home: &Home, throttle: bool) {
        let (Some(engine), Some(app), Some(root)) = (
            engine_db_for(home),
            super::super::discover::db_path_for(home),
            mirror_root_for(home),
        ) else {
            return;
        };
        if !owns_root(&root, std::env::var_os(super::MIRROR_DIR_ENV).is_some()) {
            return;
        }
        let mut memo = MEMO.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(m) = memo.as_ref() {
            if throttle
                && m.db == engine
                && m.at.is_some_and(|t| t.elapsed() < Duration::from_secs(2))
            {
                return;
            }
        }
        if memo.as_ref().is_none_or(|m| m.db != engine) {
            *memo = Some(Memo {
                at: None,
                db: engine.clone(),
                kept: HashMap::new(),
            });
        }
        let m = memo.as_mut().expect("set above");
        m.at = Some(Instant::now());
        // Held from the read of the log to the last write (see `write_mirror`).
        let Some(_lock) = lock_mirrors(&root) else {
            return;
        };
        let Ok(chats) = chats(&app) else { return };
        let Ok(c) = open(&engine) else { return };
        // One snapshot of the engine's log for the positions and every session's events.
        if c.execute_batch("begin").is_err() {
            return;
        }
        let Ok(sessions) = positions(&c) else {
            return;
        };
        let now_ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let seqs: HashMap<String, i64> = sessions.iter().map(|(s, q)| (s.id.clone(), *q)).collect();
        let all: Vec<Session> = sessions.iter().map(|(s, _)| s.clone()).collect();
        let top: Vec<Session> = all.iter().filter(|s| s.parent.is_none()).cloned().collect();
        let mut loaded: HashMap<String, (i64, Vec<Event>)> = HashMap::new();
        for chat in &chats {
            let gens = generations(chat, &chats, &top);
            if gens.is_empty() {
                continue;
            }
            // Its sub-agents' sessions (#s659), at any depth: their growth is the chat's too.
            let kids: Vec<&Session> = descendants(&gens, &all)
                .into_iter()
                .filter(|s| file_safe(&s.id))
                .collect();
            let at: Vec<(String, i64)> = gens
                .iter()
                .chain(kids.iter())
                .map(|s| (s.id.clone(), seqs.get(&s.id).copied().unwrap_or(-1)))
                .collect();
            // Nothing to do where the log has not moved and what was kept was read to its end.
            if m.kept.get(&chat.id).is_some_and(|k| {
                k.at == at
                    && k.at
                        .iter()
                        .all(|(id, p)| k.read.get(id).is_some_and(|r| r >= p))
            }) && mirror_path(&root, chat).is_file()
            {
                continue;
            }
            let t = Target {
                chat,
                root: &root,
                gens: &gens,
                kids: &kids,
                at: &at,
                now_ms,
            };
            // What was kept grows by what the log added; else the whole log is projected again.
            let grown = match m.kept.remove(&chat.id) {
                Some(k) => match grow(&c, k, &t) {
                    Ok(g) => g,
                    Err(_) => return,
                },
                None => None,
            };
            let kept = match grown {
                Some(k) => Some(k),
                None => match build(&c, &mut loaded, &t) {
                    Ok(k) => k,
                    Err(_) => return,
                },
            };
            if let Some(k) = kept {
                m.kept.insert(chat.id.clone(), k);
            }
        }
        let _ = c.execute_batch("commit");
        m.kept.retain(|id, _| chats.iter().any(|c| &c.id == id));
        // Read after the chats were: a chat mirrored above is named here unless it was deleted
        // since, and a read that fails or is cut short removes nothing. Nor does one that misses
        // a chat the first read had (a table caught empty while the app rewrites it): the next
        // refresh decides, rather than every mirror going and being built again.
        let Ok(keep) = chat_ids(&app) else { return };
        if chats.iter().all(|c| keep.contains(&c.id)) {
            let current: HashMap<String, PathBuf> = chats
                .iter()
                .map(|c| (c.id.clone(), mirror_path(&root, c)))
                .collect();
            prune(&root, &keep, &current);
        }
    }

    /// Every chat id the app database names, on whichever engine and however its binding reads:
    /// what a mirror may stay for (#s659). A row whose id is not text names no mirror.
    fn chat_ids(app: &Path) -> rusqlite::Result<HashSet<String>> {
        let c = open(app)?;
        let mut q = c.prepare("select session_id from sub_chats")?;
        let rows = q
            .query_map([], |r| Ok(r.get_ref(0)?.as_str().ok().map(str::to_owned)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows.into_iter().flatten().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat() -> Chat {
        Chat {
            id: "chat-1".into(),
            dir: "/w/2026-01-02/abc".into(),
            native: "ses_b".into(),
            generation: Some(2),
        }
    }

    fn msg(id: &str, role: &str, completed: bool) -> (String, Value) {
        let mut info =
            json!({ "id": id, "role": role, "modelID": "mode-x", "time": { "created": 1000 } });
        if completed {
            info["time"]["completed"] = json!(9000);
        }
        (
            "message.updated.1".into(),
            json!({ "sessionID": "ses_a", "info": info }),
        )
    }

    fn part(p: Value) -> (String, Value) {
        (
            "message.part.updated.1".into(),
            json!({ "sessionID": "ses_a", "part": p, "time": 1 }),
        )
    }

    fn session(id: &str, created: i64, events: Vec<(String, Value)>) -> Session {
        Session {
            id: id.into(),
            directory: "/w/2026-01-02/abc".into(),
            version: "1.0.0".into(),
            created_ms: created,
            events: events
                .into_iter()
                .enumerate()
                .map(|(i, (t, d))| (i as i64 + 1, t, d))
                .collect(),
            ..Session::default()
        }
    }

    /// A turn: a prompt, a thought, a tool that runs and ends, a reply, a step's tokens.
    fn turn() -> Vec<(String, Value)> {
        vec![
            msg("msg_u", "user", false),
            part(
                json!({ "id": "prt_1", "messageID": "msg_u", "type": "text", "text": "list the files" }),
            ),
            msg("msg_a", "assistant", false),
            part(
                json!({ "id": "prt_2", "messageID": "msg_a", "type": "reasoning", "text": "", "time": { "start": 2000 } }),
            ),
            part(
                json!({ "id": "prt_2", "messageID": "msg_a", "type": "reasoning", "text": "I will run ls.", "time": { "start": 2000, "end": 2100 } }),
            ),
            part(
                json!({ "id": "prt_3", "messageID": "msg_a", "type": "tool", "tool": "bash", "callID": "call_1", "state": { "status": "pending", "input": {} } }),
            ),
            part(
                json!({ "id": "prt_3", "messageID": "msg_a", "type": "tool", "tool": "bash", "callID": "call_1", "state": { "status": "running", "input": { "command": "ls" }, "time": { "start": 2200 } } }),
            ),
            part(
                json!({ "id": "prt_3", "messageID": "msg_a", "type": "tool", "tool": "bash", "callID": "call_1", "state": { "status": "completed", "input": { "command": "ls" }, "output": "a.txt\n", "time": { "start": 2200, "end": 2300 } } }),
            ),
            part(
                json!({ "id": "prt_4", "messageID": "msg_a", "type": "text", "text": "One file: a.txt.", "time": { "start": 2400, "end": 2500 } }),
            ),
            part(
                json!({ "id": "prt_5", "messageID": "msg_a", "type": "step-finish", "reason": "stop", "tokens": { "input": 100, "output": 20, "reasoning": 5, "cache": { "read": 50, "write": 0 } } }),
            ),
            msg("msg_a", "assistant", true),
        ]
    }

    /// A longer log than [`turn`]: it, then a turn whose first part comes before its message and
    /// whose tool never ends, and one with a sub-agent call, a question, the app's own tool, a
    /// part left out (`ignored`), the files a step changed and a kind nothing draws.
    fn long_log() -> Vec<(String, Value)> {
        let mut ev = turn();
        ev.extend([
            part(
                json!({ "id": "prt_6", "messageID": "msg_b", "type": "text", "text": "early", "time": { "start": 3000, "end": 3100 } }),
            ),
            msg("msg_b", "assistant", false),
            part(
                json!({ "id": "prt_7", "messageID": "msg_b", "type": "tool", "tool": "read", "callID": "c7", "state": { "status": "running", "input": { "filePath": "/a" }, "time": { "start": 3200 } } }),
            ),
            part(json!({ "id": "prt_8", "messageID": "msg_b", "type": "text", "text": "cut off" })),
            part(json!({ "id": "prt_9", "messageID": "msg_b", "type": "step-start" })),
            msg("msg_b", "assistant", true),
            msg("msg_c", "assistant", false),
            part(
                json!({ "id": "prt_t", "messageID": "msg_c", "type": "tool", "tool": "task", "callID": "c1", "state": { "status": "running", "input": { "description": "look", "prompt": "look around" }, "time": { "start": 4000 } } }),
            ),
            part(
                json!({ "id": "prt_t", "messageID": "msg_c", "type": "tool", "tool": "task", "callID": "c1", "state": { "status": "completed", "input": { "description": "look", "prompt": "look around" }, "output": "found it", "metadata": { "sessionId": "ses_child" }, "time": { "start": 4000, "end": 4500 } } }),
            ),
            part(
                json!({ "id": "prt_q", "messageID": "msg_c", "type": "tool", "tool": "question", "callID": "c2", "state": { "status": "completed", "input": { "questions": [{ "question": "Which one?", "header": "Pick", "options": [] }] }, "output": "", "metadata": { "answers": [["Left"]] } } }),
            ),
            part(
                json!({ "id": "prt_i", "messageID": "msg_c", "type": "text", "text": "hidden", "ignored": true }),
            ),
            part(
                json!({ "id": "prt_r", "messageID": "msg_c", "type": "text", "text": "Done looking.", "time": { "start": 4600 } }),
            ),
            part(
                json!({ "id": "prt_p", "messageID": "msg_c", "type": "patch", "hash": "h1", "files": ["/w/2026-01-02/abc/a.txt", "/w/2026-01-02/abc/src/b.rs"] }),
            ),
            msg("msg_c", "assistant", true),
        ]);
        ev
    }

    /// #s662: a mirror holding a step's changed files and a step's cost reads, through the
    /// Qwenwork adapter, as a note naming the files under the chat's folder and a session cost of
    /// what the steps cost; a step that cost nothing writes no figure. Its model is one the
    /// catalog cannot price, so the cost is a lower bound (`≥`). Checked on this fixture only:
    /// every step on this Mac's real chats cost 0.
    #[test]
    fn a_mirror_shows_a_step_s_changed_files_and_its_cost() {
        use claude_replay_engine::seam::{Agent, Block};
        let c = chat();
        let mut ev = turn();
        let finish = ev.len() - 2;
        ev[finish].1["part"]["cost"] = json!(0.25);
        ev.insert(
            ev.len() - 1,
            part(
                json!({ "id": "prt_p", "messageID": "msg_a", "type": "patch", "hash": "h1", "files": ["/w/2026-01-02/abc/a.txt", "/w/2026-01-02/abc/src/b.rs"] }),
            ),
        );
        // A second step that cost nothing.
        ev.extend([
            msg("msg_d", "assistant", false),
            part(
                json!({ "id": "prt_d", "messageID": "msg_d", "type": "step-finish", "reason": "stop", "cost": 0, "tokens": { "input": 10, "output": 2, "reasoning": 0, "cache": { "read": 0, "write": 0 } } }),
            ),
            msg("msg_d", "assistant", true),
        ]);
        let lines = project(&c, &[&session("ses_b", 1, ev)]);
        let figures: Vec<Value> = lines
            .iter()
            .filter_map(|l| {
                let v: Value = serde_json::from_str(l).unwrap();
                v["message"]["usage"].get("cost_usd").cloned()
            })
            .collect();
        assert_eq!(figures, [json!(0.25)], "only the step that cost something");

        let d = std::env::temp_dir().join(format!("qw-s662-cost-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let path = mirror_path(&d, &c);
        let head_at = |t: i64| head(&c, "mode-x", t);
        write_mirror(&path, Lead::Head(&head_at), &lines, 10_000).unwrap();
        let session = claude_replay_core::parse_session_as(Agent::QWENWORK, &path).unwrap();
        let notes: Vec<String> = session
            .blocks()
            .into_iter()
            .filter_map(|b| match b {
                Block::ToolResult(t) if t.starts_with("Changed") => Some(t),
                _ => None,
            })
            .collect();
        assert_eq!(notes, ["Changed 2 files: a.txt, src/b.rs"]);
        let m = &session.metrics;
        assert_eq!(m.extra.get("cost_usd_micro"), Some(&250_000));
        assert!((m.cost_usd.expect("the steps' cost") - 0.25).abs() < 1e-12);
        assert!(m.footer().contains("≥$0.25"), "footer: {}", m.footer());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// #s662: the files a step changed are a record of the family's own subtype, naming them as
    /// the engine does, dated by their message; a patch that names none writes nothing, and one
    /// is written once however often it is seen.
    #[test]
    fn a_step_s_changed_files_are_a_record_of_the_family_s_own() {
        let c = chat();
        let mut ev = turn();
        let changed = json!({ "id": "prt_p", "messageID": "msg_a", "type": "patch", "hash": "h1", "files": ["/w/2026-01-02/abc/a.txt", "/elsewhere/b.rs"] });
        ev.insert(ev.len() - 1, part(changed.clone()));
        ev.insert(ev.len() - 1, part(changed));
        ev.insert(
            ev.len() - 1,
            part(
                json!({ "id": "prt_e", "messageID": "msg_a", "type": "patch", "hash": "h2", "files": [] }),
            ),
        );
        let lines = project(&c, &[&session("ses_b", 1, ev)]);
        let systems: Vec<Value> = lines
            .iter()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .filter(|v| v["type"] == "system")
            .collect();
        assert_eq!(systems.len(), 1, "{systems:?}");
        let s = &systems[0];
        assert_eq!(s["subtype"], FILES_CHANGED);
        assert_eq!(
            s["files"],
            json!(["/w/2026-01-02/abc/a.txt", "/elsewhere/b.rs"])
        );
        assert_eq!(s["uuid"], "prt_p");
        assert_eq!(s["cwd"], "/w/2026-01-02/abc");
        assert_eq!(s["timestamp"], iso(2500), "the latest its message named");
        assert!(super::super::discover::FAMILY_SYSTEM_SUBTYPES.contains(&FILES_CHANGED));
    }

    /// #s659: a projector that took the log in two pieces, cut anywhere, wrote what one that took
    /// it whole wrote; so did one that took it an event at a time, and it kept no part once every
    /// message had ended.
    #[test]
    fn a_log_taken_in_pieces_writes_what_it_writes_taken_whole() {
        let c = chat();
        let first = session("ses_a", 1, turn());
        let s = session("ses_b", 2, long_log());
        let whole = project(&c, &[&first, &s]);
        assert!(whole.len() > 12, "{}", whole.len());
        for n in 0..=s.events.len() {
            let mut p = Projector::new(&c, None);
            p.feed(&first.version, &first.events);
            p.feed(&s.version, &s.events[..n]);
            let mut lines = p.take();
            p.feed(&s.version, &s.events[n..]);
            lines.extend(p.take());
            assert_eq!(lines, whole, "cut at {n}");
        }
        let mut p = Projector::new(&c, None);
        let mut lines = Vec::new();
        for g in [&first, &s] {
            for e in &g.events {
                p.feed(&g.version, std::slice::from_ref(e));
                lines.extend(p.take());
            }
        }
        assert_eq!(lines, whole);
        assert!(
            p.parts.is_empty(),
            "kept: {:?}",
            p.parts.keys().collect::<Vec<_>>()
        );
        assert_eq!(
            latest_ms(&whole),
            Some(p.last_ms),
            "what a mirror is dated by"
        );
    }

    fn kinds(lines: &[String]) -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                let v: Value = serde_json::from_str(l).unwrap();
                let c = &v["message"]["content"];
                let k = if c.is_string() {
                    "text"
                } else {
                    c.get(0)
                        .and_then(|b| b.get("type"))
                        .and_then(Value::as_str)
                        .unwrap_or("usage")
                };
                format!("{}:{k}", v["type"].as_str().unwrap())
            })
            .collect()
    }

    #[test]
    fn a_turn_becomes_the_familys_records_in_the_order_they_ended() {
        let c = chat();
        let s = session("ses_b", 1, turn());
        let lines = project(&c, &[&s]);
        assert_eq!(
            kinds(&lines),
            [
                "user:text",
                "assistant:thinking",
                "assistant:tool_use",
                "user:tool_result",
                "assistant:text",
                "assistant:usage"
            ]
        );
        let tool: Value = serde_json::from_str(&lines[2]).unwrap();
        assert_eq!(tool["message"]["content"][0]["name"], "Bash");
        assert_eq!(
            tool["message"]["content"][0]["input"],
            json!({ "command": "ls" })
        );
        let usage: Value = serde_json::from_str(&lines[5]).unwrap();
        assert_eq!(usage["message"]["usage"]["output_tokens"], 25);
        // A step that stopped ends the turn.
        assert_eq!(usage["message"]["stop_reason"], "end_turn");
        // A prompt is a typed prompt's shape: its text as a string.
        let prompt: Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(prompt["message"]["content"], "list the files");
        // Parts with no time of their own take their message's: the prompt its message's start,
        // the step the latest its message's parts named. Nothing is dated 1970.
        let at = |i: usize| -> String {
            let v: Value = serde_json::from_str(&lines[i]).unwrap();
            v["timestamp"].as_str().unwrap().to_string()
        };
        assert_eq!(at(0), iso(1000));
        assert_eq!(at(5), iso(2500));
        for i in 0..lines.len() {
            assert!(
                parse_iso_ms(&at(i)).unwrap() >= 1000,
                "record {i} at {}",
                at(i)
            );
        }
        assert_eq!(usage["message"]["usage"]["cache_read_input_tokens"], 50);
        // Every record names the chat and its folder, and links to the one before.
        for (i, l) in lines.iter().enumerate() {
            let v: Value = serde_json::from_str(l).unwrap();
            assert_eq!(v["sessionId"], "chat-1");
            assert_eq!(v["cwd"], "/w/2026-01-02/abc");
            if i > 0 {
                let prev: Value = serde_json::from_str(&lines[i - 1]).unwrap();
                assert_eq!(v["parentUuid"], prev["uuid"]);
            }
        }
    }

    /// The mirror only grows as the log does: the records of every cut of the log begin the
    /// records of the next.
    #[test]
    fn every_cut_of_the_log_begins_the_next() {
        let c = chat();
        let all = turn();
        let mut before: Vec<String> = Vec::new();
        for n in 0..=all.len() {
            let s = session("ses_b", 1, all[..n].to_vec());
            let now = project(&c, &[&s]);
            assert!(
                now.starts_with(&before),
                "cut {n} rewrote what an earlier cut wrote"
            );
            before = now;
        }
    }

    #[test]
    fn a_tool_that_never_ended_is_written_when_its_message_completes() {
        let c = chat();
        let s = session(
            "ses_b",
            1,
            vec![
                msg("msg_a", "assistant", false),
                part(
                    json!({ "id": "prt_3", "messageID": "msg_a", "type": "tool", "tool": "read", "callID": "c", "state": { "status": "running", "input": { "filePath": "/a" } } }),
                ),
                part(
                    json!({ "id": "prt_4", "messageID": "msg_a", "type": "text", "text": "cut off" }),
                ),
                msg("msg_a", "assistant", true),
            ],
        );
        let lines = project(&c, &[&s]);
        assert_eq!(
            kinds(&lines),
            ["assistant:tool_use", "user:tool_result", "assistant:text"]
        );
        let r: Value = serde_json::from_str(&lines[1]).unwrap();
        assert_eq!(r["message"]["content"][0]["is_error"], true);
    }

    #[test]
    fn a_chat_s_generations_join_in_its_own_folder_and_not_in_a_shared_one() {
        let a = session("ses_a", 1, vec![]);
        let b = session("ses_b", 2, vec![]);
        let mut other = session("ses_o", 3, vec![]);
        other.directory = "/elsewhere".into();
        let all = [b.clone(), a.clone(), other];
        let c = chat();
        let ids = |v: Vec<&Session>| v.iter().map(|s| s.id.clone()).collect::<Vec<_>>();
        // Its own folder: both, in the order they began.
        assert_eq!(
            ids(generations(&c, std::slice::from_ref(&c), &all)),
            ["ses_a", "ses_b"]
        );
        // A count the folder does not give: the bound session alone.
        let three = Chat {
            generation: Some(3),
            ..c.clone()
        };
        assert_eq!(
            ids(generations(&three, std::slice::from_ref(&three), &all)),
            ["ses_b"]
        );
        // A shared folder where the other chat names the earlier session: not ours.
        let named = Chat {
            id: "chat-2".into(),
            native: "ses_a".into(),
            generation: Some(1),
            ..c.clone()
        };
        assert_eq!(ids(generations(&c, &[c.clone(), named], &all)), ["ses_b"]);
        // A shared folder where nobody names it and only this chat misses one: ours.
        let mut x = session("ses_x", 4, vec![]);
        x.directory = c.dir.clone();
        let with_x = [b.clone(), a.clone(), x];
        let neighbour = Chat {
            id: "chat-2".into(),
            native: "ses_x".into(),
            generation: Some(1),
            ..c.clone()
        };
        assert_eq!(
            ids(generations(&c, &[c.clone(), neighbour.clone()], &with_x)),
            ["ses_a", "ses_b"]
        );
        // A binding that gives no count: the bound session alone, even in its own folder.
        let uncounted = Chat {
            generation: None,
            ..c.clone()
        };
        assert_eq!(
            ids(generations(
                &uncounted,
                std::slice::from_ref(&uncounted),
                &all
            )),
            ["ses_b"]
        );
        // …unless the other chat misses one too.
        let wanting = Chat {
            generation: Some(2),
            ..neighbour
        };
        assert_eq!(
            ids(generations(&c, &[c.clone(), wanting], &with_x)),
            ["ses_b"]
        );
    }

    #[test]
    fn a_removed_part_is_not_written() {
        let c = chat();
        let mut ev = turn();
        ev.push((
            "message.part.removed.1".into(),
            json!({ "sessionID": "ses_a", "messageID": "msg_a", "partID": "prt_4" }),
        ));
        let s = session("ses_b", 1, ev);
        let lines = project(&c, &[&s]);
        assert!(!kinds(&lines).contains(&"assistant:text".to_string()));
    }

    /// #s659: a deleted chat's mirror goes with its sub-agents and its folder; a chat that moved
    /// folder keeps only its mirror where it works now; a chat the app still has keeps its mirror
    /// whatever engine it is on; and nothing else in the root is touched.
    #[test]
    fn prune_removes_the_mirrors_of_deleted_and_moved_chats_and_nothing_else() {
        let root = std::env::temp_dir().join(format!("qw-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let put = |p: &str| {
            let p = root.join(p);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b"{}\n").unwrap();
            p
        };
        let deleted = put("-w-a/gone.jsonl");
        let deleted_sub = put("-w-a/gone/subagents/agent-ses_k.jsonl");
        let deleted_tmp = put("-w-a/gone2.jsonl.tmp");
        let kept = put("-w-a/kept.jsonl");
        let kept_sub = put("-w-a/kept/subagents/agent-ses_k.jsonl");
        let other_engine = put("-w-a/elsewhere.jsonl");
        let moved_old = put("-w-old/moved.jsonl");
        let moved_old_sub = put("-w-old/moved/subagents/agent-ses_m.jsonl");
        let moved_new = put("-w-new/moved.jsonl");
        let not_ours = put("-w-a/notes.txt");
        let keep: HashSet<String> = ["kept", "elsewhere", "moved"]
            .into_iter()
            .map(String::from)
            .collect();
        let current: HashMap<String, PathBuf> = [
            ("kept".to_string(), kept.clone()),
            ("moved".to_string(), moved_new.clone()),
        ]
        .into_iter()
        .collect();

        let n = prune(&root, &keep, &current);

        assert_eq!(n, 3, "gone, gone2 and the moved chat's old place");
        for p in [
            &deleted,
            &deleted_sub,
            &deleted_tmp,
            &moved_old,
            &moved_old_sub,
        ] {
            assert!(!p.exists(), "removed: {}", p.display());
        }
        assert!(
            !root.join("-w-a/gone").exists(),
            "with its sub-agents' folder"
        );
        assert!(!root.join("-w-old").exists(), "a folder no chat is left in");
        for p in [&kept, &kept_sub, &other_engine, &moved_new, &not_ours] {
            assert!(p.exists(), "kept: {}", p.display());
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A folder QWENWORK_MIRROR_DIR names is the mirror's only if it was new or empty, and it is
    /// marked so; one holding anything else is never written or pruned in (#s659).
    #[test]
    fn a_mirror_root_named_by_hand_is_taken_only_new_or_empty() {
        let base = std::env::temp_dir().join(format!("qw-root-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let fresh = base.join("fresh");
        let empty = base.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let someones = base.join("someones");
        std::fs::create_dir_all(someones.join("-w-a/ses/subagents")).unwrap();
        std::fs::write(someones.join("-w-a/notes.jsonl"), b"{}\n").unwrap();

        assert!(
            owns_root(&someones, false),
            "a root of its own naming is its own"
        );
        assert!(
            !owns_root(&someones, true),
            "a folder holding anything else is not"
        );
        assert!(!someones.join(ROOT_MARK).exists(), "and is left as it was");
        assert!(owns_root(&fresh, true), "a new folder is taken");
        assert!(fresh.join(ROOT_MARK).is_file(), "and marked");
        assert!(owns_root(&empty, true), "so is an empty one");
        std::fs::write(empty.join("-w-b.jsonl"), b"{}\n").unwrap();
        assert!(
            owns_root(&empty, true),
            "marked, it stays the mirror's as it fills"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A chat whose folder differs from another's only in case shares that one's slug folder on a
    /// case-insensitive disk, spelled the first one's way: its mirror is where it is, not moved.
    #[test]
    fn prune_keeps_a_mirror_whose_folder_is_spelled_another_way_on_disk() {
        let root = std::env::temp_dir().join(format!("qw-prune-case-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let on_disk = root.join("-w-Notes/second.jsonl");
        std::fs::create_dir_all(on_disk.parent().unwrap()).unwrap();
        std::fs::write(&on_disk, b"{}\n").unwrap();
        let spelled = root.join("-w-notes/second.jsonl");
        let insensitive = spelled.exists();
        let keep: HashSet<String> = ["second".to_string()].into_iter().collect();
        let current: HashMap<String, PathBuf> =
            [("second".to_string(), spelled)].into_iter().collect();

        let n = prune(&root, &keep, &current);

        if insensitive {
            assert_eq!(n, 0, "one folder under two spellings");
            assert!(on_disk.exists());
        } else {
            assert_eq!(
                n, 1,
                "a case-sensitive disk: another folder, so the chat moved"
            );
            assert!(!on_disk.exists());
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_mirror_grows_in_place_and_is_built_again_under_a_new_head() {
        let dir = std::env::temp_dir().join(format!("qw-mirror-{}", std::process::id()));
        let path = dir.join("slug").join("chat-1.jsonl");
        let head_at = |ms: i64| format!("{{\"head\":{ms}}}");
        let a = vec!["{\"n\":1}".to_string()];
        let ab = vec!["{\"n\":1}".to_string(), "{\"n\":2}".to_string()];
        write_mirror(&path, Lead::Head(&head_at), &a, 5).unwrap();
        let first = std::fs::read_to_string(&path).unwrap();
        write_mirror(&path, Lead::Head(&head_at), &ab, 6).unwrap();
        let grown = std::fs::read_to_string(&path).unwrap();
        assert!(grown.starts_with(&first), "an append keeps what was there");
        assert_eq!(grown.lines().count(), 3);
        // Something removed: built again, its first line new.
        write_mirror(&path, Lead::Head(&head_at), &["{\"n\":2}".to_string()], 7).unwrap();
        let rebuilt = std::fs::read_to_string(&path).unwrap();
        assert_eq!(rebuilt, "{\"head\":7}\n{\"n\":2}\n");
        // The file's time is its latest record's.
        let dated = vec![format!("{{\"timestamp\":\"{}\"}}", iso(1_795_340_431_123))];
        write_mirror(&path, Lead::Head(&head_at), &dated, 8).unwrap();
        let at = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(
            at.duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis(),
            1_795_340_431_123
        );
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_sub_agent_ends_and_a_question_keeps_its_answers() {
        let c = chat();
        let s = session(
            "ses_b",
            1,
            vec![
                msg("msg_a", "assistant", false),
                part(
                    json!({ "id": "prt_t", "messageID": "msg_a", "type": "tool", "tool": "task", "callID": "c1", "state": { "status": "completed", "input": { "description": "look", "prompt": "look around", "subagent_type": "explore" }, "output": "found it", "metadata": { "sessionId": "ses_child" } } }),
                ),
                part(
                    json!({ "id": "prt_q", "messageID": "msg_a", "type": "tool", "tool": "question", "callID": "c2", "state": { "status": "completed", "input": { "questions": [{ "question": "Which one?", "header": "Pick", "options": [] }] }, "output": "", "metadata": { "answers": [["Left", "Right"]] } } }),
                ),
                part(
                    json!({ "id": "prt_m", "messageID": "msg_a", "type": "tool", "tool": "qw-builtin_app_tool", "callID": "c3", "state": { "status": "completed", "input": {}, "output": "ok" } }),
                ),
            ],
        );
        let lines = project(&c, &[&s]);
        let v: Vec<Value> = lines
            .iter()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(v[0]["message"]["content"][0]["name"], "Agent");
        assert_eq!(
            v[1]["toolUseResult"],
            json!({ "status": "completed", "content": "found it", "agentId": "ses_child" })
        );
        assert_eq!(
            v[3]["toolUseResult"]["answers"],
            json!({ "Which one?": "Left, Right" })
        );
        assert_eq!(
            v[4]["message"]["content"][0]["name"],
            "mcp__qw-builtin__app_tool"
        );
        assert!(v[5].get("toolUseResult").is_none());
    }

    /// Two processes never bring the mirrors up to date at once: the second waits, then leaves
    /// them to the first, and takes the lock once it is let go.
    #[test]
    fn one_process_at_a_time_brings_the_mirrors_up_to_date() {
        let dir = std::env::temp_dir().join(format!("qw-lock-{}", std::process::id()));
        let root = dir.join("mirror");
        let held = lock_mirrors(&root).expect("the first takes it");
        assert!(lock_mirrors(&root).is_none(), "the second leaves it");
        drop(held);
        assert!(lock_mirrors(&root).is_some(), "and takes it once let go");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn timestamps_read_as_the_records_write_them() {
        assert_eq!(iso(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso(1_795_340_431_123), "2026-11-22T09:40:31.123Z");
        for ms in [0, 951_782_400_000, 1_795_340_431_123, 4_107_542_399_999] {
            assert_eq!(parse_iso_ms(&iso(ms)), Some(ms), "{}", iso(ms));
        }
    }
}
