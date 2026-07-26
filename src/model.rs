//! Parse a Claude Code transcript JSONL into a flat list of render blocks.
//! Nothing is dropped or truncated — every event becomes a block with its full
//! content; what's shown collapsed is a fold-policy decision made in `view`.
//! One JSONL line can yield several blocks.

use crate::engine::message::{Message, QueueOpKind};
use crate::engine::path::relativize;
use crate::engine::time::epoch_secs;
use crate::Agent;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// One hunk of a Claude Code `structuredPatch` — gives the real file line
/// numbers so an Edit diff can number its rows correctly.
#[derive(Debug, Clone, PartialEq)]
pub struct Hunk {
    /// 1-based line number of this hunk's first line on the OLD side.
    pub old_start: usize,
    /// 1-based line number of this hunk's first line on the NEW side.
    pub new_start: usize,
    /// Patch lines; each begins with ' ' (context), '+' (added), or '-' (removed).
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    /// A human turn (a `user` event whose content is a plain string).
    UserText(String),
    /// A mid-turn prompt the human submitted while the agent was busy — recorded as
    /// a `queue-operation` `enqueue`, shown as a dim `⧗ queued: …` marker at submit
    /// time. It marks the human's in-flight input and the submit-vs-pickup lag. If the
    /// agent picks it up immediately (no work in between) the marker is suppressed and
    /// only the `❯` turn (from the `queued_command` attachment) renders; if there was
    /// a gap, the marker stays and the turn appears later at pickup. Not a turn itself
    /// (no sidebar/sticky/`user_times` entry).
    QueueEvent { text: String },
    /// Assistant prose (markdown).
    AssistantText(String),
    /// A ✻ thinking block, grouped as a "turn" like Claude Code: the thinking text,
    /// the wall-clock seconds it took (floored, from transcript timestamps — `None`
    /// if not derivable), and the tool calls that ran just before it (whose results
    /// it processed). Collapsed → `<activities>, thought for Xs` (natural order —
    /// tools ran first); expanded → the tools followed by the thinking.
    Thinking {
        text: String,
        duration_secs: Option<u64>,
        tools: Vec<Block>,
    },
    /// A tool invocation: name + a short target (file/command/…), with its result
    /// joined in from the matching `tool_result`'s `toolUseResult` metadata.
    ToolUse {
        name: String,
        target: String,
        /// For Edit/Write/MultiEdit/NotebookEdit: (old, new) pairs to diff
        /// (fallback when `patch` is absent).
        diffs: Vec<(String, String)>,
        /// Tool output to show under the call (Bash stdout/stderr, Read content,
        /// generic result text). Edit/Write boilerplate is stripped → `None`.
        output: Option<String>,
        /// Edit/MultiEdit `structuredPatch` (real file line numbers), if present.
        patch: Option<Vec<Hunk>>,
        /// Read line count (from `toolUseResult.file.numLines`), if present.
        read_lines: Option<usize>,
    },
    /// A tool result with no matching tool_use (rare).
    ToolResult(String),
    /// A file, plan, or image the transcript embedded or referenced. Surfaced (the
    /// viewer used to drop these) so the reader can **download** embedded content or
    /// **reveal** a path-only reference in the file manager. See `Attachment`.
    Attachment(Attachment),
    /// An `Agent`/`Task` spawn of a sub-agent. Collapsed it reads like an ordinary
    /// `⏺ Agent(type: description)` tool line (in the agent hue); expanded it adds the
    /// prompt, the result, and one selectable row per spawned agent id — activating a
    /// row descends into that agent's transcript (`blocks`). See `SubAgent`.
    SubAgent(SubAgent),
    /// A sub-agent **completion** event — the later `<task-notification>` for a spawned
    /// agent, rendered as its OWN message at the point the notification arrived (the
    /// spawn `SubAgent` block stays "launched" up where it was created). Reads
    /// `⏺ Agent(type: description) done · <result>` in the agent hue. See the two-event
    /// rendering in `render.rs`.
    AgentDone {
        /// The completing agent's id (matches its spawn `SubAgent.agent_id`).
        agent_id: String,
        /// The agent kind, copied from the matching spawn (may be empty if unmatched).
        agent_type: String,
        /// The agent's description, from the notification `<summary>` (`Agent "…" …`).
        description: String,
        /// The terminal state from the notification `<status>`.
        status: AgentStatus,
        /// The agent's returned text, from the notification `<result>` (if any).
        result: Option<String>,
    },
    /// A slash command (e.g. `/compact`) and its local stdout. Rendered like
    /// Claude Code's `❯ /command` header + dim `⎿ output` lines, folded by
    /// default. Parsed from the `<command-name>`/`<command-args>`/
    /// `<local-command-stdout>` wrappers Claude Code injects as user messages.
    Command {
        /// The command, e.g. `/compact`.
        name: String,
        /// Command arguments (may be empty).
        args: String,
        /// `local-command-stdout` chunks shown beneath the header (may be empty).
        output: Vec<String>,
    },
}

/// A file / plan / image the transcript carried. The viewer surfaces it so the reader
/// can act on it: `content.is_some()` ⇒ the bytes are embedded and **downloadable**;
/// `content.is_none()` ⇒ only a path is known, so the action is **reveal in the file
/// manager** (`path`). `--dump`/`--dump-html` only ever show the name.
#[derive(Debug, Clone, PartialEq)]
pub struct Attachment {
    /// Short kind label for the header: `file` · `plan` · `edited` · `ref` · `image`.
    pub kind: &'static str,
    /// Display name — a repo-relative path when available, else the file's basename.
    pub name: String,
    /// Absolute on-disk path, when known — the reveal-in-file-manager target and the
    /// default filename for a download.
    pub path: Option<String>,
    /// Embedded content, when the transcript carried it (makes it downloadable).
    pub content: Option<AttachmentContent>,
}

/// Embedded attachment payload. Decoded lazily — the base64 stays a string until a
/// download actually happens (or HTML inlines it as a `data:` URI).
#[derive(Debug, Clone, PartialEq)]
pub enum AttachmentContent {
    /// UTF-8 text (a `file` body or a plan) — written verbatim on download.
    Text(String),
    /// Base64 bytes + MIME type (an image) — decoded on download, or inlined as a
    /// `data:<mime>;base64,<b64>` URI in the HTML export.
    Base64 { mime: String, b64: String },
}

/// A spawned sub-agent (`Agent`/`Task` tool). The spawn's `input` gives
/// `agent_type`/`description`/`prompt`; its `tool_result`'s `toolUseResult` gives
/// `agent_id`/`status`/`result`/`output_file`; a later `<task-notification>` keyed by
/// `tool_use_id` (or `agent_id`) supplies the terminal status. `blocks` is the child
/// transcript (`subagents/agent-<id>.jsonl`), parsed by the same `parse_main` and
/// filled in by the path-aware wrapper; nested `SubAgent`s inside it are grandchildren.
#[derive(Debug, Clone, PartialEq)]
pub struct SubAgent {
    /// The child's agent id (== the completion notification's `task-id`; file stem).
    pub agent_id: String,
    /// The spawn `Agent` tool_use id — the primary join key to the completion event.
    pub tool_use_id: String,
    pub agent_type: String,
    pub description: String,
    /// The spawn prompt (the child's first user message, byte-equal).
    pub prompt: String,
    pub status: AgentStatus,
    /// The agent's returned text — inline for a sync spawn, from the completion
    /// notification's `<result>` / `output_file` / the child's final message otherwise.
    pub result: Option<String>,
    /// For an async spawn, the `tasks/agent-<id>.output` path (result lands here).
    pub output_file: Option<String>,
    /// The child transcript, parsed via `parse_main`. Empty until the path-aware
    /// wrapper resolves `subagents/agent-<id>.jsonl` (absent for a copied `.jsonl`).
    pub blocks: Vec<Block>,
    /// This agent's own cost plus all descendants', rolled up. `None` if unknown.
    pub subtree_cost: Option<f64>,
}

/// A sub-agent's lifecycle state. Launched states come from the spawn's
/// `toolUseResult.status`; terminal states from its completion `<task-notification>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    /// Spawned synchronously and still shown running (no result yet seen).
    Running,
    /// Spawned in the background (`toolUseResult.status == "async_launched"`).
    AsyncLaunched,
    Completed,
    Failed,
    Killed,
    Stopped,
}

impl AgentStatus {
    /// Parse a `toolUseResult.status` / notification `<status>` string.
    pub fn from_status(s: &str) -> Option<Self> {
        Some(match s {
            "async_launched" => Self::AsyncLaunched,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "killed" => Self::Killed,
            "stopped" => Self::Stopped,
            _ => return None,
        })
    }
    /// A terminal state — no more activity expected (drives "running" animation off).
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Killed | Self::Stopped
        )
    }
    /// Short label for the collapsed spawn line / footer.
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::AsyncLaunched => "running",
            Self::Completed => "done",
            Self::Failed => "failed",
            Self::Killed => "killed",
            Self::Stopped => "stopped",
        }
    }
    /// Past-tense verb for the completion event line (`Agent "…" completed`).
    pub fn done_verb(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Killed => "killed",
            Self::Stopped => "stopped",
            // Non-terminal states never reach a completion event; treat as "finished".
            Self::Running | Self::AsyncLaunched => "finished",
        }
    }
}

/// Extract the quoted description from a completion `<summary>` like
/// `Agent "Design the parser" finished` → `Design the parser`. Falls back to the whole
/// trimmed summary when there's no quoted span.
fn summary_description(summary: &str) -> String {
    if let (Some(a), Some(b)) = (summary.find('"'), summary.rfind('"')) {
        if b > a {
            return summary[a + 1..b].to_string();
        }
    }
    summary.trim().to_string()
}

/// The fold-policy category for a block. One key per block; `--fold`/`--unfold`
/// and the default fold policy are keyed on these (see `view`).
/// The one presentation classification (M13) both the TUI fold policy and the HTML
/// `data-kind` derive from. The **fine** set: it splits a `Thinking` block into bare
/// `think` vs. a grouped-activity `act`, and keeps `ToolResult` distinct from a generic
/// tool. Two projections tame it: [`BlockKind::fold_key`] (the coarser TUI / type-filter
/// view) and [`BlockKind::html`] (the finer CSS `data-kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    User,
    Queue,
    Assistant,
    Think,
    Act,
    ToolResult,
    Attachment,
    Agent,
    Command,
    Bash,
    Edit,
    Write,
    Read,
    Skill,
    Tool,
}

/// Classify a block. (An `Agent`/`Task` `ToolUse` never reaches here — the tokenizer turns
/// it into a `SubAgent` — so it maps to the generic `Tool`, harmlessly.)
pub fn block_kind(b: &Block) -> BlockKind {
    use BlockKind::*;
    match b {
        Block::UserText(_) => User,
        Block::QueueEvent { .. } => Queue,
        Block::AssistantText(_) => Assistant,
        Block::Thinking { tools, .. } => {
            if tools.is_empty() {
                Think
            } else {
                Act
            }
        }
        Block::ToolResult(_) => ToolResult,
        Block::Attachment(_) => Attachment,
        Block::SubAgent(_) | Block::AgentDone { .. } => Agent,
        Block::Command { .. } => Command,
        Block::ToolUse { name, .. } => match name.as_str() {
            "Bash" => Bash,
            "Edit" | "MultiEdit" => Edit,
            "Write" | "NotebookEdit" => Write,
            "Read" | "Grep" | "Glob" | "LS" | "NotebookRead" => Read,
            "Skill" => Skill,
            _ => Tool,
        },
    }
}

impl BlockKind {
    /// The TUI fold-policy / HTML type-filter key — coarser: `thinking` for both think/act,
    /// `tool_result` kept distinct.
    pub fn fold_key(self) -> &'static str {
        use BlockKind::*;
        match self {
            User => "user",
            Queue => "queue",
            Assistant => "assistant",
            Think | Act => "thinking",
            ToolResult => "tool_result",
            Attachment => "attachment",
            Agent => "agent",
            Command => "command",
            Bash => "bash",
            Edit => "edit",
            Write => "write",
            Read => "read",
            Skill => "skill",
            Tool => "tool",
        }
    }

    /// The HTML `data-kind` — finer: `think`/`act` split, `tool` for a bare result.
    pub fn html(self) -> &'static str {
        use BlockKind::*;
        match self {
            User => "user",
            Queue => "queue",
            Assistant => "assistant",
            Think => "think",
            Act => "act",
            ToolResult => "tool",
            Attachment => "attachment",
            Agent => "agent",
            Command => "command",
            Bash => "bash",
            Edit => "edit",
            Write => "write",
            Read => "read",
            Skill => "skill",
            Tool => "tool",
        }
    }
}

pub fn fold_key(b: &Block) -> &'static str {
    block_kind(b).fold_key()
}

/// Inner text of the first `<tag>…</tag>` in `s`, if present.
fn tag_inner<'a>(s: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = s.find(&open)? + open.len();
    let rest = &s[start..];
    let end = rest.find(&close)?;
    Some(&rest[..end])
}

/// Remove every `<local-command-caveat>…</local-command-caveat>` block (pure
/// noise Claude Code injects around local commands), returning the remainder.
fn strip_caveat(s: &str) -> String {
    let (open, close) = ("<local-command-caveat>", "</local-command-caveat>");
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(open) {
        out.push_str(&rest[..i]);
        match rest[i + open.len()..].find(close) {
            Some(j) => rest = &rest[i + open.len() + j + close.len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The harness injects a loaded skill's instruction body as a user message that
/// opens with this marker. It's reference material, not user prose, so we model it
/// as a foldable (default-collapsed) result block instead of a `❯` user turn.
fn is_skill_body(s: &str) -> bool {
    s.trim_start().starts_with("Base directory for this skill:")
}

/// Append a loaded skill's instruction body to its `Skill` tool_use block at `idx`,
/// so the whole skill load reads as one collapsible unit (named by the skill) instead
/// of a loose result block beside the call. Returns `false` when there's no recent
/// `Skill` block to attach to — the caller then falls back to a standalone result.
fn attach_skill_body(out: &mut [Block], idx: Option<usize>, body: &str) -> bool {
    let Some(i) = idx else { return false };
    if let Some(Block::ToolUse { name, output, .. }) = out.get_mut(i) {
        if name == "Skill" {
            let b = body.trim();
            match output {
                Some(o) => {
                    o.push_str("\n\n");
                    o.push_str(b);
                }
                None => *output = Some(b.to_string()),
            }
            return true;
        }
    }
    false
}

/// Injected/system content that Claude Code flags at the event level — a skill or
/// slash-command **instruction body** (`isMeta`), a caveat, or a `/compact`
/// continuation summary (`isCompactSummary`). None of it is a human-initiated
/// turn, so it must never become `UserText`/`Command` (which would give it a
/// sidebar/sticky "turn" entry it doesn't deserve). It folds as a system block
/// instead; caveat-only noise is dropped entirely.
fn push_injected(s: &str, out: &mut Vec<Block>) {
    let cleaned = strip_caveat(s);
    let cleaned = cleaned.trim();
    if !cleaned.is_empty() {
        out.push(Block::ToolResult(cleaned.to_string()));
    }
}

/// Is this `user` event injected/system content rather than a human turn?
/// `isMeta` marks instruction/skill/caveat bodies; `isCompactSummary` marks the
/// summary `/compact` writes back into the transcript.
fn is_injected_event(v: &Value) -> bool {
    v.get("isMeta").and_then(Value::as_bool).unwrap_or(false)
        || v.get("isCompactSummary")
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

/// Turn one plain-string `user` message into block(s). A slash-command
/// invocation (`<command-name>`) and its `<local-command-stdout>` become a
/// `Block::Command`; the `<local-command-caveat>` noise is dropped; everything
/// else is ordinary `UserText`.
fn push_user_string(s: &str, out: &mut Vec<Block>) {
    // A background-execution notification (`<task-notification>…`): collapse the raw
    // XML to its one-line `<summary>` (else `<status>`), as a foldable result block.
    if tag_inner(s, "task-notification").is_some() {
        if let Some(line) = tag_inner(s, "summary").or_else(|| tag_inner(s, "status")) {
            let line = line.trim();
            if !line.is_empty() {
                out.push(Block::ToolResult(line.to_string()));
                return;
            }
        }
    }
    // A slash command: `<command-name>/foo</command-name>` (+ optional args /
    // inline stdout). The caveat, if bundled in the same message, is ignored.
    if let Some(name) = tag_inner(s, "command-name") {
        let args = tag_inner(s, "command-args")
            .unwrap_or("")
            .trim()
            .to_string();
        let mut output = Vec::new();
        if let Some(o) = tag_inner(s, "local-command-stdout") {
            if !o.trim().is_empty() {
                output.push(o.trim().to_string());
            }
        }
        out.push(Block::Command {
            name: name.trim().to_string(),
            args,
            output,
        });
        return;
    }
    // A standalone stdout message — attach to the command it follows, else show
    // it on its own (command-less).
    if let Some(o) = tag_inner(s, "local-command-stdout") {
        let o = o.trim().to_string();
        if o.is_empty() {
            return;
        }
        if let Some(Block::Command { output, .. }) = out.last_mut() {
            output.push(o);
        } else {
            out.push(Block::Command {
                name: String::new(),
                args: String::new(),
                output: vec![o],
            });
        }
        return;
    }
    // Drop pure caveat noise; otherwise it's ordinary user prose (unless it's an
    // injected skill body, which folds as a result block). A message with no
    // visible character — only whitespace or control bytes like a stray `\x11`
    // (Ctrl-Q) — is a phantom keystroke, not a turn: skip it.
    let cleaned = strip_caveat(s);
    let has_visible = cleaned
        .chars()
        .any(|c| !c.is_whitespace() && !c.is_control());
    if has_visible {
        if is_skill_body(&cleaned) {
            out.push(Block::ToolResult(cleaned));
        } else {
            out.push(Block::UserText(cleaned));
        }
    }
}

fn tool_target(input: &Value, cwd: &str) -> String {
    for k in ["file_path", "path"] {
        if let Some(v) = input.get(k).and_then(|v| v.as_str()) {
            return relativize(v, cwd);
        }
    }
    // A shell command keeps its line breaks — the header lays a multi-line command
    // out across rows (see `render::tool_header_lines`), matching Claude Code.
    if let Some(v) = input.get("command").and_then(|v| v.as_str()) {
        return v.to_string();
    }
    // Descriptions/patterns/skill-names are kept in full (no truncation), but their
    // newlines are flattened so these one-line headers stay one line.
    for k in ["description", "pattern", "skill"] {
        if let Some(v) = input.get(k).and_then(|v| v.as_str()) {
            return v.replace('\n', " ");
        }
    }
    String::new()
}

/// Tools Claude Code summarizes into a `Thought for …` turn line (transient reads/
/// searches whose results feed the thinking) rather than showing expanded. Edit/
/// Write/other tools produce durable output (diffs, etc.) and stay expanded.
/// `pub(crate)` so the live-tail path (`view::ingest`) can re-group a thinking
/// block with activity tools that arrived in an earlier poll.
pub(crate) fn is_activity_tool(name: &str) -> bool {
    matches!(
        name,
        "Bash" | "Read" | "NotebookRead" | "Grep" | "Glob" | "LS"
    )
}

/// Fold each `Thinking` block together with the contiguous run of *activity* tool
/// calls that immediately precede it (whose results it processed), matching Claude
/// Code's `Thought for Xs, <activities>` turn summary. Edit/Write and other tools
/// (and any tool not directly before a thinking) are left expanded.
fn group_turns(blocks: Vec<Block>) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::with_capacity(blocks.len());
    for b in blocks {
        if let Block::Thinking {
            text,
            duration_secs,
            ..
        } = b
        {
            let mut tools = Vec::new();
            while matches!(out.last(), Some(Block::ToolUse { name, .. }) if is_activity_tool(name))
            {
                tools.push(out.pop().unwrap());
            }
            tools.reverse();
            out.push(Block::Thinking {
                text,
                duration_secs,
                tools,
            });
        } else {
            out.push(b);
        }
    }
    out
}

/// Coalesce a contiguous run (≥2) of *activity* tool calls that isn't part of a
/// thinking turn into one `<activities>` summary block — matching Claude Code, which
/// shows e.g. "Searched for 1 pattern, ran 9 shell commands" rather than nine
/// separate lines. A lone activity tool keeps its own detailed summary; Edit/Write
/// and other non-activity tools break a run and stay expanded. (A tools-only
/// `Thinking` — empty text, no duration — is how such a run is represented; `render`
/// shows it as the activities line without a "thought".)
fn coalesce_activity_runs(blocks: Vec<Block>) -> Vec<Block> {
    fn flush(run: &mut Vec<Block>, out: &mut Vec<Block>) {
        if run.len() >= 2 {
            out.push(Block::Thinking {
                text: String::new(),
                duration_secs: None,
                tools: std::mem::take(run),
            });
        } else {
            out.append(run);
        }
    }
    let mut out: Vec<Block> = Vec::with_capacity(blocks.len());
    let mut run: Vec<Block> = Vec::new();
    for b in blocks {
        if matches!(&b, Block::ToolUse { name, .. } if is_activity_tool(name)) {
            run.push(b);
        } else {
            flush(&mut run, &mut out);
            out.push(b);
        }
    }
    flush(&mut run, &mut out);
    out
}

fn result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .first()
            .and_then(|b| b.get("text"))
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string(),
        _ => content.to_string(),
    }
}

/// Is this tool_result text the no-information boilerplate Edit/Write emits?
fn is_boilerplate(s: &str) -> bool {
    let s = s.trim();
    (s.starts_with("The file ") && s.contains("has been updated successfully"))
        || s.starts_with("File created successfully at")
}

/// Parse `toolUseResult.structuredPatch` into hunks (real line numbers).
fn parse_patch(tur: &Value) -> Option<Vec<Hunk>> {
    let arr = tur.get("structuredPatch")?.as_array()?;
    let hunks: Vec<Hunk> = arr
        .iter()
        .filter_map(|h| {
            let new_start = h.get("newStart").and_then(|n| n.as_u64())? as usize;
            let old_start = h
                .get("oldStart")
                .and_then(|n| n.as_u64())
                .map(|n| n as usize)
                .unwrap_or(new_start);
            let lines = h
                .get("lines")?
                .as_array()?
                .iter()
                .filter_map(|l| l.as_str().map(String::from))
                .collect();
            Some(Hunk {
                old_start,
                new_start,
                lines,
            })
        })
        .collect();
    (!hunks.is_empty()).then_some(hunks)
}

/// The output text to show under a tool call. Edit/Write show their diff/code,
/// not the boilerplate result, so they get `None`. Bash uses stdout/stderr; Read
/// uses the file content; other tools use the raw result (unless boilerplate).
fn tool_output(name: &str, tur: Option<&Value>, res_txt: &str) -> Option<String> {
    match name {
        "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => None,
        "Bash" => {
            if let Some(tur) = tur {
                let out = tur.get("stdout").and_then(|s| s.as_str()).unwrap_or("");
                let err = tur.get("stderr").and_then(|s| s.as_str()).unwrap_or("");
                let combined = match (out.trim().is_empty(), err.trim().is_empty()) {
                    (true, true) => String::new(),
                    (false, true) => out.to_string(),
                    (true, false) => err.to_string(),
                    (false, false) => format!("{out}\n{err}"),
                };
                if !combined.trim().is_empty() {
                    return Some(combined);
                }
            }
            (!res_txt.trim().is_empty()).then(|| res_txt.to_string())
        }
        "Read" => tur
            .and_then(|t| t.pointer("/file/content"))
            .and_then(|c| c.as_str())
            .map(String::from)
            .or_else(|| (!res_txt.trim().is_empty()).then(|| res_txt.to_string())),
        _ => (!res_txt.trim().is_empty() && !is_boilerplate(res_txt)).then(|| res_txt.to_string()),
    }
}

/// Parse JSONL text into the **complete** block list. Kept for tests and the
/// live-tail path (small in-memory batches).
///
/// This in-memory batch entry runs the new two-layer engine — Layer 1 [`tokenize`]
/// (message log) then Layer 2 [`replay`] (the forward fold) — which is asserted
/// bit-identical to the (now frozen, test-only) `parse_main` — see
/// `replay_tokenize_matches_parse_main`. The large-file streaming path
/// (`parse_path` → `parse_file` → `parse_stream`) runs the same engine per line (M9), so
/// production no longer touches `parse_main`.
pub fn parse(jsonl: &str) -> Vec<Block> {
    replay(&tokenize(jsonl.lines()), &mut Vec::new(), &CLAUDE_SHAPING)
}

/// Parse JSONL text with the parser for `agent`.
pub fn parse_for(agent: Agent, jsonl: &str) -> Vec<Block> {
    match agent {
        Agent::Claude => parse(jsonl),
        Agent::Codex => crate::codex_model::parse_codex(jsonl),
    }
}

/// Parse a transcript file by **streaming** it — one line resident at a time, in
/// two passes (each a fresh read) — so a large transcript never balloons into a
/// whole-file `Vec<Value>` (~5–8× the file in RAM) or a whole-file `String`. See
/// `STREAMING-PARSE-DESIGN.md`.
pub fn parse_path(path: &std::path::Path) -> std::io::Result<Vec<Block>> {
    let mut blocks = parse_file(path)?;
    // Load each spawned sub-agent's child transcript (recursively) so a `SubAgent`
    // block can be descended into and its subtree cost rolled up. All of a session's
    // agents — any depth — share one flat `<session>/subagents/` dir (they share the
    // session id), so one dir resolves the whole tree.
    if let Some(dir) = subagents_dir(path) {
        enrich_subagents(&mut blocks, &dir);
    }
    Ok(blocks)
}

/// Parse a transcript file into blocks WITHOUT loading sub-agent children — the raw
/// pass. `parse_path` wraps this with `enrich_subagents`; the recursion reuses this so
/// grandchildren resolve against the same session `subagents/` dir.
fn parse_file(path: &std::path::Path) -> std::io::Result<Vec<Block>> {
    use std::io::BufRead;
    let open = || -> std::io::Result<_> { Ok(std::io::BufReader::new(std::fs::File::open(path)?)) };
    // Pass 1: collect the set of all tool_use ids (small — ids only), so pass 2 can
    // tell a genuine orphan tool_result from one whose tool_use appears later.
    let tool_ids = scan_tool_ids(open()?.lines().map_while(|r| r.ok()));
    // Pass 2: stream through the engine, one line resident.
    let mut cwd = String::new();
    parse_stream(
        open()?,
        tool_ids,
        &CLAUDE_SHAPING,
        |line, out| decode_line(line, &mut cwd, out),
        |_| {}, // blocks only — this path doesn't need metrics
        &mut Vec::new(),
    )
}

/// The `<project>/<sessionId>/subagents/` dir for a transcript at
/// `<project>/<sessionId>.jsonl`, if it exists on disk.
fn subagents_dir(path: &std::path::Path) -> Option<std::path::PathBuf> {
    let stem = path.file_stem()?.to_str()?;
    let dir = path.parent()?.join(stem).join("subagents");
    dir.is_dir().then_some(dir)
}

/// The on-disk transcript for `agent_id` under the root session at `session_path`
/// (`<session>/subagents/agent-<id>.jsonl`), if it exists — the file a descended child is
/// live-tailed from. All of a session's agents (any depth) share this one flat dir.
pub fn subagent_file(session_path: &std::path::Path, agent_id: &str) -> Option<std::path::PathBuf> {
    let stem = session_path.file_stem()?.to_str()?;
    let f = session_path
        .parent()?
        .join(stem)
        .join("subagents")
        .join(format!("agent-{agent_id}.jsonl"));
    f.is_file().then_some(f)
}

/// Fill each `SubAgent` block's `blocks` (child transcript) + `subtree_cost` by parsing
/// `<sadir>/agent-<id>.jsonl`, recursing into grandchildren against the same `sadir`.
/// A missing child file (older session, a copied `.jsonl`) leaves `blocks` empty —
/// never a dead affordance.
fn enrich_subagents(blocks: &mut [Block], sadir: &std::path::Path) {
    for b in blocks.iter_mut() {
        if let Block::SubAgent(sa) = b {
            if sa.agent_id.is_empty() {
                continue;
            }
            let child = sadir.join(format!("agent-{}.jsonl", sa.agent_id));
            let Ok(mut cb) = parse_file(&child) else {
                continue;
            };
            enrich_subagents(&mut cb, sadir); // grandchildren (same flat dir)
                                              // The completion `<task-notification>` is the sole authority for terminal
                                              // status — a child file existing does NOT mean the agent finished (it keeps
                                              // growing while it runs). Upgrading to Completed here would hide a live agent
                                              // from `active`, so leave the status alone and only attach the transcript.
            sa.subtree_cost = subtree_cost(&child, &cb);
            sa.blocks = cb;
        }
    }
}

/// A sub-agent's own cost (from its transcript's metrics) plus all descendants'
/// rolled-up costs. `None` when neither is known.
fn subtree_cost(child_path: &std::path::Path, child_blocks: &[Block]) -> Option<f64> {
    let own = std::fs::File::open(child_path).ok().and_then(|f| {
        crate::metrics::parse_reader_for(Agent::Claude, std::io::BufReader::new(f)).cost_usd
    });
    let desc: f64 = child_blocks
        .iter()
        .filter_map(|b| match b {
            Block::SubAgent(sa) => sa.subtree_cost,
            _ => None,
        })
        .sum();
    match own {
        Some(o) => Some(o + desc),
        None if desc > 0.0 => Some(desc),
        None => None,
    }
}

/// Like `parse_path_for`, but also returns one wall-clock timestamp (epoch
/// seconds) per **user turn**, in order — the HTML export shows them beside each
/// turn. Codex transcripts yield the same shape.
/// **The streaming L2 driver** (M9). Feed a [`Replayer`] one line's messages at a time —
/// `decode` (the agent's per-line L1, capturing its `cwd`) turns each line into a few
/// messages that are folded immediately — so no whole-file `Vec<Message>` is built: peak
/// memory is one line + the block buffer, matching the retired `parse_main`. `tool_ids` is
/// the pass-1 id pre-scan; `reader` is a fresh pass-2 read. This equals `replay(tokenize(x))`
/// over the same input (proven by `parse_path_matches_parse_str` + the golden corpus).
pub(crate) fn parse_stream<R: std::io::BufRead>(
    reader: R,
    tool_ids: HashSet<String>,
    shaping: &Shaping,
    mut decode: impl FnMut(&str, &mut Vec<Message>),
    mut fold_metrics: impl FnMut(&Value),
    user_times: &mut Vec<Option<f64>>,
) -> std::io::Result<Vec<Block>> {
    let mut r = Replayer::new(shaping, tool_ids);
    let mut buf: Vec<Message> = Vec::new();
    for line in reader.lines() {
        let line = line?;
        buf.clear();
        decode(&line, &mut buf);
        // Fold token/cost metrics in the SAME pass (M10) — one read instead of two. The
        // metrics re-parse of the line matches the retired `parse_reader_for` exactly
        // (from the raw line, skip on parse error), so the tally is byte-identical.
        if let Ok(v) = serde_json::from_str::<Value>(&line) {
            fold_metrics(&v);
        }
        r.apply(&buf);
    }
    let (blocks, ut) = r.into_blocks();
    user_times.extend(ut);
    Ok(blocks)
}

pub fn parse_path_timed_for(
    agent: Agent,
    path: &std::path::Path,
) -> std::io::Result<(Vec<Block>, Vec<Option<f64>>, crate::metrics::Metrics)> {
    let mut times = Vec::new();
    let (blocks, metrics) = match agent {
        Agent::Claude => {
            use std::io::BufRead;
            let open = || -> std::io::Result<_> {
                Ok(std::io::BufReader::new(std::fs::File::open(path)?))
            };
            let tool_ids = scan_tool_ids(open()?.lines().map_while(|r| r.ok()));
            let mut cwd = String::new();
            let mut macc = crate::metrics::MetricsAcc::default();
            let blocks = parse_stream(
                open()?,
                tool_ids,
                &CLAUDE_SHAPING,
                |line, out| decode_line(line, &mut cwd, out),
                |v| macc.push(v),
                &mut times,
            )?;
            (blocks, macc.finish())
        }
        Agent::Codex => crate::codex_model::parse_codex_path_timed(path, &mut times)?,
    };
    Ok((blocks, times, metrics))
}

/// Like [`parse_path_timed_for`] but ALSO loads the sub-agent tree (each `SubAgent`'s
/// child transcript, recursively) — the enriched form the multi-file HTML bundle needs
/// so every agent's blocks and subtree cost are available. (The plain timed parse skips
/// enrichment for the single-file snapshot, which never drills down.)
pub fn parse_path_timed_enriched_for(
    agent: Agent,
    path: &std::path::Path,
) -> std::io::Result<(Vec<Block>, Vec<Option<f64>>)> {
    let (mut blocks, times, _metrics) = parse_path_timed_for(agent, path)?;
    if agent == Agent::Claude {
        if let Some(dir) = subagents_dir(path) {
            enrich_subagents(&mut blocks, &dir);
        }
    }
    Ok((blocks, times))
}

/// Streaming file parse with the parser for `agent`.
pub fn parse_path_for(agent: Agent, path: &std::path::Path) -> std::io::Result<Vec<Block>> {
    match agent {
        Agent::Claude => parse_path(path),
        Agent::Codex => crate::codex_model::parse_codex_path(path),
    }
}

/// Pass 1: the set of every `tool_use` id in the transcript.
fn scan_tool_ids<S: AsRef<str>>(lines: impl Iterator<Item = S>) -> HashSet<String> {
    let mut ids = HashSet::new();
    for line in lines {
        let line = line.as_ref().trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) == Some("assistant") {
            if let Some(arr) = v.pointer("/message/content").and_then(|c| c.as_array()) {
                for blk in arr {
                    if blk.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                        if let Some(id) = blk.get("id").and_then(|s| s.as_str()) {
                            ids.insert(id.to_string());
                        }
                    }
                }
            }
        }
    }
    ids
}

/// Fill a `tool_use` block's result fields (output / diff line numbers / read
/// count) from its matching `tool_result`'s `toolUseResult` metadata + text.
fn apply_result(block: &mut Block, txt: &str, tur: &Value) {
    match block {
        Block::ToolUse {
            name,
            output,
            patch,
            read_lines,
            ..
        } => {
            *output = tool_output(name, Some(tur), txt);
            *patch = parse_patch(tur);
            *read_lines = tur
                .pointer("/file/numLines")
                .and_then(|n| n.as_u64())
                .map(|n| n as usize);
        }
        // An `Agent`/`Task` spawn's result: `toolUseResult` carries the agent id, the
        // launch status, and (sync) the inline result or (async) the output-file path.
        Block::SubAgent(sa) => {
            if let Some(aid) = tur.get("agentId").and_then(|v| v.as_str()) {
                sa.agent_id = aid.to_string();
            }
            if let Some(st) = tur
                .get("status")
                .and_then(|v| v.as_str())
                .and_then(AgentStatus::from_status)
            {
                sa.status = st;
            }
            if let Some(of) = tur.get("outputFile").and_then(|v| v.as_str()) {
                if !of.is_empty() {
                    sa.output_file = Some(of.to_string());
                }
            }
            // A synchronous spawn returns its answer inline; an async one returns only
            // the "async_launched" marker here (the real result arrives in the completion
            // notification / output-file), so it must NOT be captured as the result.
            let inline = tur
                .get("content")
                .and_then(|v| v.as_str())
                .filter(|c| !c.trim().is_empty())
                .or_else(|| {
                    Some(txt).filter(|t| {
                        let t = t.trim();
                        !t.is_empty() && t != "async_launched" && !t.starts_with("Launching")
                    })
                });
            if let Some(c) = inline {
                sa.result = Some(c.to_string());
            }
        }
        _ => {}
    }
}

/// **Layer 1 (Claude) — tokenize.** Map each JSONL line to zero or more canonical
/// [`Message`]s: pure line-shape classification, **no** back-patch, grouping, joins,
/// queue lifecycle, or turn stamping (those are the fold's job — see [`replay`]). The
/// session cwd is captured here (first non-empty `cwd` wins) purely to shape tool
/// targets, exactly as `parse_main` does. Streaming: one `Value` resident at a time.
///
/// This is the L1 half of `parse_main`; `replay(tokenize(x))` is asserted bit-identical
/// to `parse_main(x)` (see the tests). `parse_main` stays live and unchanged.
pub(crate) fn tokenize<S: AsRef<str>>(lines: impl Iterator<Item = S>) -> Vec<Message> {
    let mut msgs: Vec<Message> = Vec::new();
    let mut cwd = String::new();
    for line in lines {
        decode_line(line.as_ref(), &mut cwd, &mut msgs);
    }
    msgs
}

/// **Layer 1 — Claude decode, per line** (the streaming unit). Decode ONE raw transcript
/// line into 0+ canonical messages appended to `msgs`. `cwd` is threaded across lines (set
/// once from the first line that carries it) so tool targets relativize. `tokenize` is this
/// over every line; the streaming driver (M9) calls it one line at a time so no whole-file
/// `Vec<Message>` is ever built.
pub(crate) fn decode_line(line: &str, cwd: &mut String, msgs: &mut Vec<Message>) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return;
    };
    if cwd.is_empty() {
        if let Some(c) = v.get("cwd").and_then(|c| c.as_str()) {
            *cwd = c.to_string();
        }
    }
    let ev_ts = v
        .get("timestamp")
        .and_then(|t| t.as_str())
        .and_then(epoch_secs);
    msgs.push(Message::LineStart(ev_ts));
    match v.get("type").and_then(|t| t.as_str()) {
        Some("assistant") => {
            let Some(content) = v.pointer("/message/content").and_then(|c| c.as_array()) else {
                return;
            };
            for blk in content {
                match blk.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        if let Some(t) = blk.get("text").and_then(|t| t.as_str()) {
                            if !t.trim().is_empty() {
                                msgs.push(Message::AssistantText(t.to_string()));
                            }
                        }
                    }
                    Some("thinking") => {
                        let t = blk
                            .get("thinking")
                            .or_else(|| blk.get("text"))
                            .and_then(|t| t.as_str())
                            .unwrap_or("");
                        if !t.trim().is_empty() {
                            msgs.push(Message::Thinking {
                                text: t.to_string(),
                                ts: ev_ts,
                            });
                        }
                    }
                    Some("tool_use") => {
                        let name = blk.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
                        let input = blk.get("input").cloned().unwrap_or(Value::Null);
                        let id = blk.get("id").and_then(|s| s.as_str()).unwrap_or("");
                        // Raw fields only — the block is shaped in L2 via `claude_build_tool`.
                        msgs.push(Message::ToolUse {
                            id: id.to_string(),
                            name: name.to_string(),
                            input,
                            cwd: cwd.to_string(),
                        });
                    }
                    _ => {}
                }
            }
        }
        Some("user") => {
            msgs.push(Message::Trigger(ev_ts));
            let tur = v.get("toolUseResult").cloned().unwrap_or(Value::Null);
            let injected = is_injected_event(&v);
            let Some(content) = v.pointer("/message/content") else {
                return;
            };
            if let Some(s) = content.as_str() {
                msgs.push(Message::UserString {
                    text: s.to_string(),
                    injected,
                });
            } else if let Some(arr) = content.as_array() {
                for blk in arr {
                    match blk.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            if let Some(t) = blk.get("text").and_then(|t| t.as_str()) {
                                if !t.trim().is_empty() {
                                    msgs.push(Message::UserArrayText {
                                        text: t.to_string(),
                                        injected,
                                    });
                                }
                            }
                        }
                        Some("image") => {
                            if let Some(att) = image_attachment(blk) {
                                msgs.push(Message::Attachment(att));
                            }
                        }
                        Some("tool_result") => {
                            let tid = blk
                                .get("tool_use_id")
                                .and_then(|s| s.as_str())
                                .unwrap_or("");
                            let txt = result_text(blk.get("content").unwrap_or(&Value::Null));
                            msgs.push(Message::ToolResult {
                                tool_use_id: tid.to_string(),
                                text: txt,
                                tur: tur.clone(),
                            });
                            if let Some(items) = blk.get("content").and_then(|c| c.as_array()) {
                                for item in items {
                                    if let Some(att) = image_attachment(item) {
                                        msgs.push(Message::Attachment(att));
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        Some("queue-operation") => {
            let content = v
                .get("content")
                .and_then(|c| c.as_str())
                .map(|c| c.to_string());
            let op = match v.get("operation").and_then(|o| o.as_str()) {
                Some("enqueue") => Some(QueueOpKind::Enqueue),
                Some("remove") => Some(QueueOpKind::Remove),
                Some("dequeue") => Some(QueueOpKind::Dequeue),
                _ => None,
            };
            if let Some(op) = op {
                msgs.push(Message::QueueOp { op, content });
            }
        }
        Some("attachment") => {
            let a = v.get("attachment");
            let is_prompt = a.and_then(|a| a.get("type")).and_then(|t| t.as_str())
                == Some("queued_command")
                && a.and_then(|a| a.get("commandMode"))
                    .and_then(|m| m.as_str())
                    == Some("prompt");
            if is_prompt {
                if let Some(p) = a.and_then(|a| a.get("prompt")).and_then(|p| p.as_str()) {
                    if !p.trim().is_empty() {
                        msgs.push(Message::AttachmentPrompt {
                            text: p.to_string(),
                        });
                    }
                }
            } else if let Some(att) = a.and_then(attachment_from_event) {
                msgs.push(Message::Attachment(att));
            }
        }
        _ => {}
    }
}

/// The small agent-specific seam of the otherwise-shared L2 fold — the embryo of the
/// `Adapter` (design §3.2). Everything else in [`replay`] is agent-agnostic; these three
/// hooks are the only points Claude and Codex differ:
/// - `apply`: back-patch a tool result onto its `ToolUse` block (Claude reads the
///   `toolUseResult` metadata for diffs/read-count; Codex just sets the output text).
/// - `keep_orphan`: keep a resultless orphan result (already checked non-empty)? Claude
///   drops boilerplate; Codex keeps every non-empty output.
/// - `finish`: final turn shaping — Claude groups + coalesces activity; Codex is identity.
pub(crate) struct Shaping {
    /// Build the block for a `tool_use` from its raw fields (`id`, `name`, `input`, `cwd`).
    /// This is the block-model lift's L2 hook (M14): the tokenizer emits raw
    /// `Message::ToolUse` fields and the fold shapes the block here, so agent-specific
    /// block construction (Claude's `Agent`/`Task`→`SubAgent`, Codex's name normalization)
    /// lives in Layer 2, not the tokenizer.
    pub build_tool: fn(&str, &str, &Value, &str) -> Block,
    pub apply: fn(&mut Block, &str, &Value),
    pub keep_orphan: fn(&str) -> bool,
    pub finish: fn(Vec<Block>) -> Vec<Block>,
}

fn claude_keep_orphan(t: &str) -> bool {
    !is_boilerplate(t)
}
fn claude_finish(blocks: Vec<Block>) -> Vec<Block> {
    coalesce_activity_runs(group_turns(blocks))
}

/// Claude's `build_tool`: an `Agent`/`Task` spawn becomes a launched `SubAgent` block;
/// any other tool becomes a `ToolUse` with its path target relativized against `cwd` and
/// its diffs extracted. (Formerly inline in `decode_line`; lifted to L2 in M14.)
pub(crate) fn claude_build_tool(id: &str, name: &str, input: &Value, cwd: &str) -> Block {
    if name == "Agent" || name == "Task" {
        let s = |k: &str| {
            input
                .get(k)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        let agent_type = {
            let t = s("subagent_type");
            if t.is_empty() {
                "agent".to_string()
            } else {
                t
            }
        };
        Block::SubAgent(SubAgent {
            agent_id: String::new(),
            tool_use_id: id.to_string(),
            agent_type,
            description: s("description"),
            prompt: s("prompt"),
            status: AgentStatus::Running,
            result: None,
            output_file: None,
            blocks: Vec::new(),
            subtree_cost: None,
        })
    } else {
        Block::ToolUse {
            name: name.to_string(),
            target: tool_target(input, cwd),
            diffs: extract_diffs(name, input),
            output: None,
            patch: None,
            read_lines: None,
        }
    }
}

/// Claude's shaping — the historical `parse_main` behavior.
pub(crate) const CLAUDE_SHAPING: Shaping = Shaping {
    build_tool: claude_build_tool,
    apply: apply_result,
    keep_orphan: claude_keep_orphan,
    finish: claude_finish,
};

/// **Layer 2 — the stateful replayer** (design §3.3). `apply` folds a batch of messages
/// into the running block buffer (the `id → block index` back-patch, the thinking clock,
/// the queue lifecycle, user-turn stamping); `into_blocks` finalizes (the final user-turn
/// flush, completions, then the agent-specific `finish`). Fed all messages at once it
/// reproduces the old one-shot `replay` exactly; fed in pieces it folds **incrementally** —
/// the keystone for the streaming production path (M9) and the live `ingest` (M11).
/// Agent-agnostic except for `shaping`: the Claude-only quirks (skill-body nesting, the
/// two-event spawn/completion split, queue lifecycle) fire only on Claude-shaped messages
/// that a Codex tokenizer never emits.
///
/// `tool_ids` is the L1 id pre-scan (so an orphan result is told from a not-yet-seen one);
/// the caller supplies it — from the whole message log for a batch, or a streaming pre-scan.
pub(crate) struct Replayer<'a> {
    shaping: &'a Shaping,
    tool_ids: HashSet<String>,
    out: Vec<Block>,
    user_times: Vec<Option<f64>>,
    pending_ts: Option<f64>,
    stamped: usize,
    tool_slot: HashMap<String, usize>,
    pending: HashMap<String, (String, Value)>,
    trigger_ts: Option<f64>,
    queue: Vec<QueueItem>,
    content_seq: usize,
    suppress: Vec<usize>,
    last_skill: Option<usize>,
    completions: Vec<String>,
}

impl<'a> Replayer<'a> {
    pub fn new(shaping: &'a Shaping, tool_ids: HashSet<String>) -> Self {
        Replayer {
            shaping,
            tool_ids,
            out: Vec::new(),
            user_times: Vec::new(),
            pending_ts: None,
            stamped: 0,
            tool_slot: HashMap::new(),
            pending: HashMap::new(),
            trigger_ts: None,
            queue: Vec::new(),
            content_seq: 0,
            suppress: Vec::new(),
            last_skill: None,
            completions: Vec::new(),
        }
    }

    /// Fold a batch of messages into the running state (append, back-patch, stamp).
    pub fn apply(&mut self, messages: &[Message]) {
        let (apply, keep_orphan) = (self.shaping.apply, self.shaping.keep_orphan);
        for m in messages {
            match m {
                Message::LineStart(ts) => {
                    stamp_user_turns(
                        &self.out,
                        &mut self.stamped,
                        self.pending_ts,
                        &mut self.user_times,
                    );
                    self.pending_ts = *ts;
                }
                Message::Trigger(ts) => {
                    if let Some(t) = ts {
                        self.trigger_ts = Some(*t);
                    }
                }
                Message::AssistantText(t) => {
                    self.out.push(Block::AssistantText(t.clone()));
                    self.content_seq += 1;
                }
                Message::Thinking { text, ts } => {
                    let duration_secs = match (ts, self.trigger_ts) {
                        (Some(end), Some(start)) if *end >= start => Some((end - start) as u64),
                        _ => None,
                    };
                    self.out.push(Block::Thinking {
                        text: text.clone(),
                        duration_secs,
                        tools: Vec::new(),
                    });
                    self.content_seq += 1;
                }
                Message::ToolUse {
                    id,
                    name,
                    input,
                    cwd,
                } => {
                    self.out
                        .push((self.shaping.build_tool)(id, name, input, cwd));
                    self.content_seq += 1;
                    let idx = self.out.len() - 1;
                    if let Block::ToolUse { name, .. } = &self.out[idx] {
                        if name == "Skill" {
                            self.last_skill = Some(idx);
                        }
                    }
                    if !id.is_empty() {
                        self.tool_slot.insert(id.clone(), idx);
                        if let Some((txt, tur)) = self.pending.remove(id) {
                            apply(&mut self.out[idx], &txt, &tur);
                        }
                    }
                }
                Message::ToolResult {
                    tool_use_id,
                    text,
                    tur,
                } => {
                    if let Some(&idx) = self.tool_slot.get(tool_use_id) {
                        apply(&mut self.out[idx], text, tur);
                    } else if self.tool_ids.contains(tool_use_id) {
                        self.pending
                            .insert(tool_use_id.clone(), (text.clone(), tur.clone()));
                    } else if !text.trim().is_empty() && keep_orphan(text) {
                        self.out.push(Block::ToolResult(text.clone()));
                    }
                }
                Message::UserString { text, injected } => {
                    if is_skill_body(text)
                        && attach_skill_body(&mut self.out, self.last_skill, text)
                    {
                        // Nested into its `Skill` block above — no loose result block.
                    } else if *injected {
                        push_injected(text, &mut self.out);
                    } else {
                        push_user_string(text, &mut self.out);
                    }
                }
                Message::UserArrayText { text, injected } => {
                    if is_skill_body(text)
                        && attach_skill_body(&mut self.out, self.last_skill, text)
                    {
                        // Nested into its `Skill` block above.
                    } else if *injected || is_skill_body(text) {
                        self.out.push(Block::ToolResult(text.clone()));
                    } else {
                        self.out.push(Block::UserText(text.clone()));
                    }
                }
                Message::AttachmentPrompt { text } => {
                    self.out.push(Block::UserText(text.clone()));
                }
                Message::Attachment(att) => {
                    self.out.push(Block::Attachment(att.clone()));
                }
                Message::QueueOp { op, content } => match op {
                    QueueOpKind::Enqueue => {
                        if let Some(c) = content {
                            if is_agent_notification(c) {
                                self.completions.push(c.to_string());
                                let status = tag_inner(c, "status")
                                    .and_then(AgentStatus::from_status)
                                    .unwrap_or(AgentStatus::Completed);
                                let description = tag_inner(c, "summary")
                                    .map(summary_description)
                                    .unwrap_or_default();
                                let result = tag_inner(c, "result")
                                    .map(str::trim)
                                    .filter(|r| !r.is_empty())
                                    .map(str::to_string);
                                let agent_id = tag_inner(c, "task-id")
                                    .or_else(|| tag_inner(c, "tool-use-id"))
                                    .unwrap_or_default()
                                    .to_string();
                                self.out.push(Block::AgentDone {
                                    agent_id,
                                    agent_type: String::new(),
                                    description,
                                    status,
                                    result,
                                });
                            }
                            let is_prose = is_queue_prose(c);
                            let marker_idx = if is_prose {
                                self.out.push(Block::QueueEvent {
                                    text: c.trim().to_string(),
                                });
                                Some(self.out.len() - 1)
                            } else {
                                None
                            };
                            self.queue.push(QueueItem {
                                content: c.trim().to_string(),
                                marker_idx,
                                content_at_enqueue: self.content_seq,
                            });
                        }
                    }
                    QueueOpKind::Remove | QueueOpKind::Dequeue => {
                        let popped = match content.as_deref().map(str::trim) {
                            Some(c) => self
                                .queue
                                .iter()
                                .position(|q| q.content == c)
                                .map(|i| self.queue.remove(i)),
                            None if !self.queue.is_empty() => Some(self.queue.remove(0)),
                            None => None,
                        };
                        if let Some(item) = popped {
                            if let Some(mi) = item.marker_idx {
                                if self.content_seq == item.content_at_enqueue {
                                    self.suppress.push(mi);
                                }
                            }
                        }
                    }
                },
            }
        }
    }

    /// Finalize (consuming): final user-turn flush + completions + the agent `finish`.
    /// Returns the grouped blocks and the per-turn timestamps.
    pub fn into_blocks(mut self) -> (Vec<Block>, Vec<Option<f64>>) {
        stamp_user_turns(
            &self.out,
            &mut self.stamped,
            self.pending_ts,
            &mut self.user_times,
        );
        apply_completions_and_suppress(
            &mut self.out,
            &self.tool_slot,
            &self.completions,
            self.suppress,
        );
        let blocks = (self.shaping.finish)(self.out);
        (blocks, self.user_times)
    }

    /// Non-consuming finalize (M11): the current presentable blocks + per-turn times, WITHOUT
    /// consuming the Replayer — so a live follower can `apply` a delta, `snapshot` to render,
    /// then keep folding. Same output as `into_blocks`, computed over cloned working state.
    /// (Proven byte-identical vs a full re-parse — used by the live `FollowParser`, M16.)
    pub fn snapshot(&self) -> (Vec<Block>, Vec<Option<f64>>) {
        let mut out = self.out.clone();
        let mut user_times = self.user_times.clone();
        let mut stamped = self.stamped;
        stamp_user_turns(&out, &mut stamped, self.pending_ts, &mut user_times);
        apply_completions_and_suppress(
            &mut out,
            &self.tool_slot,
            &self.completions,
            self.suppress.clone(),
        );
        let blocks = (self.shaping.finish)(out);
        (blocks, user_times)
    }

    /// Merge more tool_use join ids into the pre-scan set (M11): a live follower pre-scans
    /// each *delta* for its ids and extends before applying, so a result whose tool_use is
    /// later in the SAME delta is held pending (not mis-emitted as an orphan) — exactly as a
    /// batch pre-scan would. Across polls, earlier deltas' ids are already accumulated; the
    /// only remaining reorder (a result physically before its tool_use) is a rewritten tail,
    /// which the follower handles by rebuilding from scratch (a `reset`).
    pub fn extend_tool_ids(&mut self, ids: impl IntoIterator<Item = String>) {
        self.tool_ids.extend(ids);
    }
}

/// Batch L2 fold — `Replayer::new(); apply(all); into_blocks()`. For Claude,
/// `replay(tokenize(x), &CLAUDE_SHAPING)` is asserted bit-identical to `parse_main(x)`; for
/// Codex, to `parse_lines(x)`. `user_times` is filled with one entry per emitted user turn.
pub(crate) fn replay(
    messages: &[Message],
    user_times: &mut Vec<Option<f64>>,
    shaping: &Shaping,
) -> Vec<Block> {
    let tool_ids: HashSet<String> = messages
        .iter()
        .filter_map(|m| match m {
            Message::ToolUse { id, .. } if !id.is_empty() => Some(id.clone()),
            _ => None,
        })
        .collect();
    let mut r = Replayer::new(shaping, tool_ids);
    r.apply(messages);
    let (blocks, ut) = r.into_blocks();
    user_times.extend(ut);
    blocks
}

/// The `parse_main` post-loop: apply agent-completion notifications to their `SubAgent`
/// / `AgentDone` blocks (by tool-use-id, else task-id), then drop the `⧗ queued:` markers
/// of prompts picked up immediately. Split out so both `parse_main` and [`replay`] share
/// one copy. Runs before turn grouping so surviving markers keep their positions.
fn apply_completions_and_suppress(
    out: &mut Vec<Block>,
    tool_slot: &HashMap<String, usize>,
    completions: &[String],
    suppress: Vec<usize>,
) {
    if !completions.is_empty() {
        let mut agent_slot: HashMap<String, usize> = HashMap::new();
        for (i, b) in out.iter().enumerate() {
            if let Block::SubAgent(sa) = b {
                if !sa.agent_id.is_empty() {
                    agent_slot.insert(sa.agent_id.clone(), i);
                }
            }
        }
        for note in completions {
            let idx = tag_inner(note, "tool-use-id")
                .and_then(|t| tool_slot.get(t).copied())
                .or_else(|| tag_inner(note, "task-id").and_then(|t| agent_slot.get(t).copied()));
            if let Some(Block::SubAgent(sa)) = idx.and_then(|i| out.get_mut(i)) {
                if let Some(st) = tag_inner(note, "status").and_then(AgentStatus::from_status) {
                    sa.status = st;
                }
            }
        }
        let mut by_id: HashMap<String, (String, String)> = HashMap::new();
        for b in out.iter() {
            if let Block::SubAgent(sa) = b {
                let v = (sa.agent_id.clone(), sa.agent_type.clone());
                if !sa.agent_id.is_empty() {
                    by_id.insert(sa.agent_id.clone(), v.clone());
                }
                if !sa.tool_use_id.is_empty() {
                    by_id.insert(sa.tool_use_id.clone(), v);
                }
            }
        }
        for b in out.iter_mut() {
            if let Block::AgentDone {
                agent_id,
                agent_type,
                ..
            } = b
            {
                if let Some((real_id, ty)) = by_id.get(agent_id.as_str()) {
                    *agent_type = ty.clone();
                    *agent_id = real_id.clone();
                }
            }
        }
    }
    if !suppress.is_empty() {
        let drop: HashSet<usize> = suppress.into_iter().collect();
        let mut i = 0usize;
        out.retain(|_| {
            let keep = !drop.contains(&i);
            i += 1;
            keep
        });
    }
}

/// Pass 2: build blocks in order, streaming one line at a time. Nothing is dropped
/// or truncated. A `tool_use` is emitted immediately with an empty result; its
/// `tool_result` **back-patches** the already-emitted block in place (via
/// `tool_slot`: id → block index). Transcripts are **not** strictly ordered — a
/// result can precede its tool_use (compaction / sidechain reordering) — so a
/// result whose tool_use we haven't emitted yet is held in `pending` and applied
/// when that tool_use arrives (its id is in `tool_ids`); only a result whose id is
/// in **no** tool_use is a genuine orphan, emitted inline. This reproduces the old
/// two-pass semantics exactly while keeping at most one line's `Value` live.
/// `_args` is unused (fold flags are resolved in `view`).
/// `user_times` is filled with one entry per emitted **user turn** (`UserText` /
/// `Command`), in order: the epoch-seconds of the event that produced it (`None`
/// when unparsable). Turn grouping never absorbs or reorders user blocks, so the
/// Nth user turn of the returned list is `user_times[N]`. Only the HTML export
/// consumes it; the TUI passes a throwaway vec.
///
/// **Frozen golden reference** (M9): production parses through the streaming engine
/// (`parse_stream` → `decode_line` + `Replayer`); this pre-engine parser is retained only
/// to pin `replay(tokenize(x))` bit-identical in `replay_tokenize_matches_parse_main`.
#[cfg(test)]
fn parse_main<S: AsRef<str>>(
    lines: impl Iterator<Item = S>,
    tool_ids: &HashSet<String>,
    user_times: &mut Vec<Option<f64>>,
) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    // Timestamp for blocks emitted by the event being processed, plus how far we've
    // stamped. Flushed at the next iteration so an early `continue` can't lose it.
    let mut pending_ts: Option<f64> = None;
    let mut stamped = 0usize;
    // tool_use id -> index of its ToolUse block in `out`, for result back-patching.
    let mut tool_slot: HashMap<String, usize> = HashMap::new();
    // Results seen before their tool_use (id is in `tool_ids`), awaiting it.
    let mut pending: HashMap<String, (String, Value)> = HashMap::new();
    // The session's cwd (from the transcript) — tool targets are shown relative to
    // it. CC records it on every event, so it's set from the first line, before any
    // tool_use; fall back to "" (absolute paths) if a tool_use somehow precedes it.
    let mut cwd = String::new();
    // Timestamp of the last user/tool-result event — the moment the model's next
    // generation was requested — so a thinking block's duration is `its ts − this`.
    let mut trigger_ts: Option<f64> = None;
    // Messages the human submits mid-turn are recorded as `queue-operation` events
    // (not `user` events). Their lifecycle: `enqueue` → `remove`/`dequeue` (a FIFO
    // front pop) when the agent picks the prompt up → a `queued_command` **attachment**
    // at the consumption point that carries the prompt text. For the vast majority of
    // typed prompts that attachment is the ONLY record — CC never writes a standalone
    // `user` event — so we render `queued_command`/"prompt" attachments inline as user
    // turns (see the `attachment` arm below); that recovers messages that would
    // otherwise vanish. The `queue` here tracks only what is enqueued-but-not-yet-
    // consumed: a content-less pop drops the front, so on a settled transcript it nets
    // to empty. Whatever prose is still queued at the end (a live `-f` session mid-
    // flight) renders as pending user turns.
    let mut queue: Vec<QueueItem> = Vec::new();
    // Monotonic count of *agent* content blocks emitted (assistant text/thinking/
    // tool_use). A queued prompt whose enqueue and pickup straddle no agent work
    // (`content_seq` unchanged) was picked up immediately: its `⧗ queued:` marker is
    // redundant with the `❯` turn, so we suppress it. Marker indices to drop collect
    // in `suppress` and are filtered out after the loop (safe — `tool_slot` is only
    // used during the loop).
    let mut content_seq = 0usize;
    let mut suppress: Vec<usize> = Vec::new();
    // Index of the most recent `Skill` tool_use block. The harness delivers a loaded
    // skill's instruction body as a following injected user message ("Base directory
    // for this skill: …"); we nest that body into this block so a skill load reads as
    // ONE collapsible unit named by the skill, instead of a loose result block beside it.
    let mut last_skill: Option<usize> = None;
    // Agent-completion `<task-notification>` strings, collected as seen and applied to
    // their `SubAgent` block after the loop (by `tool-use-id`, else `task-id`==agentId),
    // before any block removal shifts `tool_slot`'s indices.
    let mut completions: Vec<String> = Vec::new();

    for line in lines {
        let line = line.as_ref().trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if cwd.is_empty() {
            if let Some(c) = v.get("cwd").and_then(|c| c.as_str()) {
                cwd = c.to_string();
            }
        }
        let ev_ts = v
            .get("timestamp")
            .and_then(|t| t.as_str())
            .and_then(epoch_secs);
        // Stamp the user turns the previous event emitted, then claim this event's ts.
        stamp_user_turns(&out, &mut stamped, pending_ts, user_times);
        pending_ts = ev_ts;
        match v.get("type").and_then(|t| t.as_str()) {
            Some("assistant") => {
                let Some(content) = v.pointer("/message/content").and_then(|c| c.as_array()) else {
                    continue;
                };
                for blk in content {
                    match blk.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            if let Some(t) = blk.get("text").and_then(|t| t.as_str()) {
                                if !t.trim().is_empty() {
                                    out.push(Block::AssistantText(t.to_string()));
                                    content_seq += 1;
                                }
                            }
                        }
                        Some("thinking") => {
                            let t = blk
                                .get("thinking")
                                .or_else(|| blk.get("text"))
                                .and_then(|t| t.as_str())
                                .unwrap_or("");
                            if !t.trim().is_empty() {
                                let duration_secs = match (ev_ts, trigger_ts) {
                                    (Some(end), Some(start)) if end >= start => {
                                        Some((end - start) as u64)
                                    }
                                    _ => None,
                                };
                                out.push(Block::Thinking {
                                    text: t.to_string(),
                                    duration_secs,
                                    tools: Vec::new(),
                                });
                                content_seq += 1;
                            }
                        }
                        Some("tool_use") => {
                            let name = blk.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
                            let input = blk.get("input").cloned().unwrap_or(Value::Null);
                            let id = blk.get("id").and_then(|s| s.as_str()).unwrap_or("");
                            // An `Agent`/`Task` spawn becomes a `SubAgent` block (agent hue,
                            // descendable). Its result back-patches the id/status/result.
                            if name == "Agent" || name == "Task" {
                                let s = |k: &str| {
                                    input
                                        .get(k)
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string()
                                };
                                let agent_type = {
                                    let t = s("subagent_type");
                                    if t.is_empty() {
                                        "agent".to_string()
                                    } else {
                                        t
                                    }
                                };
                                out.push(Block::SubAgent(SubAgent {
                                    agent_id: String::new(),
                                    tool_use_id: id.to_string(),
                                    agent_type,
                                    description: s("description"),
                                    prompt: s("prompt"),
                                    status: AgentStatus::Running,
                                    result: None,
                                    output_file: None,
                                    blocks: Vec::new(),
                                    subtree_cost: None,
                                }));
                            } else {
                                out.push(Block::ToolUse {
                                    name: name.to_string(),
                                    target: tool_target(&input, &cwd),
                                    diffs: extract_diffs(name, &input),
                                    output: None,
                                    patch: None,
                                    read_lines: None,
                                });
                            }
                            content_seq += 1;
                            let idx = out.len() - 1;
                            if name == "Skill" {
                                last_skill = Some(idx);
                            }
                            if !id.is_empty() {
                                tool_slot.insert(id.to_string(), idx);
                                // A result that arrived before this tool_use? Apply it now.
                                if let Some((txt, tur)) = pending.remove(id) {
                                    apply_result(&mut out[idx], &txt, &tur);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some("user") => {
                // A user turn or tool_result — the trigger for the next generation.
                if let Some(t) = ev_ts {
                    trigger_ts = Some(t);
                }
                // The message-level toolUseResult metadata (shared by its result blocks).
                let tur = v.get("toolUseResult").cloned().unwrap_or(Value::Null);
                // `isMeta`/`isCompactSummary` events are injected system content, not
                // human turns — route their prose to a folded system block so it never
                // gets a turn/sidebar/sticky entry (see `push_injected`).
                let injected = is_injected_event(&v);
                let Some(content) = v.pointer("/message/content") else {
                    continue;
                };
                if let Some(s) = content.as_str() {
                    if is_skill_body(s) && attach_skill_body(&mut out, last_skill, s) {
                        // Nested into its `Skill` block above — no loose result block.
                    } else if injected {
                        push_injected(s, &mut out);
                    } else {
                        push_user_string(s, &mut out);
                    }
                } else if let Some(arr) = content.as_array() {
                    for blk in arr {
                        match blk.get("type").and_then(|t| t.as_str()) {
                            Some("text") => {
                                if let Some(t) = blk.get("text").and_then(|t| t.as_str()) {
                                    if !t.trim().is_empty() {
                                        if is_skill_body(t)
                                            && attach_skill_body(&mut out, last_skill, t)
                                        {
                                            // Nested into its `Skill` block above.
                                        } else if injected || is_skill_body(t) {
                                            out.push(Block::ToolResult(t.to_string()));
                                        } else {
                                            out.push(Block::UserText(t.to_string()));
                                        }
                                    }
                                }
                            }
                            // A pasted image (a top-level image block in the prompt).
                            Some("image") => {
                                if let Some(att) = image_attachment(blk) {
                                    out.push(Block::Attachment(att));
                                }
                            }
                            Some("tool_result") => {
                                let tid = blk
                                    .get("tool_use_id")
                                    .and_then(|s| s.as_str())
                                    .unwrap_or("");
                                let txt = result_text(blk.get("content").unwrap_or(&Value::Null));
                                if let Some(&idx) = tool_slot.get(tid) {
                                    // Its tool_use is already emitted — back-patch in place.
                                    apply_result(&mut out[idx], &txt, &tur);
                                } else if tool_ids.contains(tid) {
                                    // Its tool_use appears later — hold until then (last wins).
                                    pending.insert(tid.to_string(), (txt, tur.clone()));
                                } else if !txt.trim().is_empty() && !is_boilerplate(&txt) {
                                    // No tool_use anywhere — a genuine orphan, shown inline.
                                    out.push(Block::ToolResult(txt));
                                }
                                // A tool result may also carry image(s) (e.g. reading a
                                // screenshot) — surface each as a downloadable attachment.
                                if let Some(items) = blk.get("content").and_then(|c| c.as_array()) {
                                    for item in items {
                                        if let Some(att) = image_attachment(item) {
                                            out.push(Block::Attachment(att));
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            // Human input submitted mid-turn: queued, not yet a `user` event. We model
            // the WHOLE queue — prose prompts AND background `<task-notification>`s — so
            // a content-less `dequeue`/`remove` (a FIFO front pop) lands on the right
            // entry. A prose `enqueue` also emits a `⧗ queued:` marker in place; a pop
            // that finds the prompt was picked up with no agent work in between marks
            // that marker for suppression (immediate → the `❯` turn alone suffices).
            Some("queue-operation") => {
                let content = v.get("content").and_then(|c| c.as_str());
                match v.get("operation").and_then(|o| o.as_str()) {
                    Some("enqueue") => {
                        if let Some(c) = content {
                            if is_agent_notification(c) {
                                completions.push(c.to_string());
                                // Also render the completion as its OWN event at this
                                // position (the spawn stays "launched" up where it was
                                // created). Type is copied from the matching spawn in a
                                // post-enrich pass (`stamp_agent_done_types`).
                                let status = tag_inner(c, "status")
                                    .and_then(AgentStatus::from_status)
                                    .unwrap_or(AgentStatus::Completed);
                                let description = tag_inner(c, "summary")
                                    .map(summary_description)
                                    .unwrap_or_default();
                                let result = tag_inner(c, "result")
                                    .map(str::trim)
                                    .filter(|r| !r.is_empty())
                                    .map(str::to_string);
                                let agent_id = tag_inner(c, "task-id")
                                    .or_else(|| tag_inner(c, "tool-use-id"))
                                    .unwrap_or_default()
                                    .to_string();
                                out.push(Block::AgentDone {
                                    agent_id,
                                    agent_type: String::new(),
                                    description,
                                    status,
                                    result,
                                });
                            }
                            let is_prose = is_queue_prose(c);
                            let marker_idx = if is_prose {
                                out.push(Block::QueueEvent {
                                    text: c.trim().to_string(),
                                });
                                Some(out.len() - 1)
                            } else {
                                None
                            };
                            queue.push(QueueItem {
                                content: c.trim().to_string(),
                                marker_idx,
                                content_at_enqueue: content_seq,
                            });
                        }
                    }
                    Some("remove") | Some("dequeue") => {
                        let popped = match content.map(str::trim) {
                            Some(c) => queue
                                .iter()
                                .position(|q| q.content == c)
                                .map(|i| queue.remove(i)),
                            None if !queue.is_empty() => Some(queue.remove(0)), // FIFO front pop
                            None => None,
                        };
                        // Picked up with no agent work since enqueue → the marker is
                        // redundant with the turn; drop it.
                        if let Some(item) = popped {
                            if let Some(mi) = item.marker_idx {
                                if content_seq == item.content_at_enqueue {
                                    suppress.push(mi);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            // The authoritative record of a consumed mid-turn prompt. CC emits a
            // `queued_command` attachment at the moment the agent picks the prompt up,
            // grouped with the tool-result-carrying `user` event of the running turn —
            // and for typed prompts this is usually the ONLY place the text survives
            // (no standalone `user` event is ever written). Render the human ones
            // (`commandMode == "prompt"`; `"task-notification"` is background noise) as
            // a user turn right here, so they land in chronological order at the point
            // they took effect.
            Some("attachment") => {
                let a = v.get("attachment");
                let is_prompt = a.and_then(|a| a.get("type")).and_then(|t| t.as_str())
                    == Some("queued_command")
                    && a.and_then(|a| a.get("commandMode"))
                        .and_then(|m| m.as_str())
                        == Some("prompt");
                if is_prompt {
                    if let Some(p) = a.and_then(|a| a.get("prompt")).and_then(|p| p.as_str()) {
                        if !p.trim().is_empty() {
                            out.push(Block::UserText(p.to_string()));
                        }
                    }
                } else if let Some(att) = a.and_then(attachment_from_event) {
                    // A file/plan/edited/compact attachment — surface it so the reader
                    // can download the embedded content or reveal the path (see
                    // `attachment_from_event`). Other attachment types (listings,
                    // reminders, deltas) are harness bookkeeping and stay dropped.
                    out.push(Block::Attachment(att));
                }
            }
            _ => {}
        }
    }
    stamp_user_turns(&out, &mut stamped, pending_ts, user_times);
    // Apply agent-completion notifications to their `SubAgent` block — terminal status
    // + inline result. MUST run before the `suppress` filter below removes blocks (which
    // would invalidate `tool_slot`'s indices). Join by `tool-use-id`, else `task-id`.
    if !completions.is_empty() {
        let mut agent_slot: HashMap<String, usize> = HashMap::new();
        for (i, b) in out.iter().enumerate() {
            if let Block::SubAgent(sa) = b {
                if !sa.agent_id.is_empty() {
                    agent_slot.insert(sa.agent_id.clone(), i);
                }
            }
        }
        for note in &completions {
            let idx = tag_inner(note, "tool-use-id")
                .and_then(|t| tool_slot.get(t).copied())
                .or_else(|| tag_inner(note, "task-id").and_then(|t| agent_slot.get(t).copied()));
            // Back-patch only the spawn's status (drives active-tracking: a terminal
            // status drops the agent from `a active N`). The result text renders on the
            // separate `AgentDone` completion event, not folded back onto the spawn.
            if let Some(Block::SubAgent(sa)) = idx.and_then(|i| out.get_mut(i)) {
                if let Some(st) = tag_inner(note, "status").and_then(AgentStatus::from_status) {
                    sa.status = st;
                }
            }
        }
        // Give each `AgentDone` event its spawn's `agent_type` (the notification carries
        // only status/summary/result), resolving its id from the spawn's `tool_use_id`
        // when the notification keyed by `tool-use-id` rather than `task-id`.
        let mut by_id: HashMap<String, (String, String)> = HashMap::new(); // id/toolid → (agent_id, type)
        for b in out.iter() {
            if let Block::SubAgent(sa) = b {
                let v = (sa.agent_id.clone(), sa.agent_type.clone());
                if !sa.agent_id.is_empty() {
                    by_id.insert(sa.agent_id.clone(), v.clone());
                }
                if !sa.tool_use_id.is_empty() {
                    by_id.insert(sa.tool_use_id.clone(), v);
                }
            }
        }
        for b in out.iter_mut() {
            if let Block::AgentDone {
                agent_id,
                agent_type,
                ..
            } = b
            {
                if let Some((real_id, ty)) = by_id.get(agent_id.as_str()) {
                    *agent_type = ty.clone();
                    *agent_id = real_id.clone();
                }
            }
        }
    }
    // Drop the `⧗ queued:` markers of prompts picked up immediately (no agent work
    // between submit and pickup) — their `❯` turn alone conveys them. Prompts still
    // queued at the end keep their marker (a live `-f` session's in-flight input).
    // Safe here: `tool_slot`/`pending` are finished, and this runs before turn grouping
    // so surviving markers keep their positions.
    let _ = queue; // consumed via `suppress` during the loop; nothing to flush
    if !suppress.is_empty() {
        let drop: HashSet<usize> = suppress.into_iter().collect();
        let mut i = 0usize;
        out.retain(|_| {
            let keep = !drop.contains(&i);
            i += 1;
            keep
        });
    }
    coalesce_activity_runs(group_turns(out))
}

/// One entry in the reconstructed prompt queue. `marker_idx` is the index of this
/// prompt's `⧗ queued:` marker in the block list (prose only); `content_at_enqueue`
/// snapshots `content_seq` at submit so a later pop can tell whether any agent work
/// happened in between (immediate → suppress the marker).
struct QueueItem {
    content: String,
    marker_idx: Option<usize>,
    content_at_enqueue: usize,
}

// (queue-operation handling is inlined in `parse_main`'s `Some("queue-operation")`
// arm — it needs the block list, `content_seq`, and `suppress`.)

/// Is this string an agent-completion `<task-notification>` — `summary` "Agent \"…\"
/// finished" with a `status` — as opposed to a background-`Bash` or `Monitor` one?
/// (Their task-id namespaces differ too: agents `a…`, background `b…`.)
fn is_agent_notification(s: &str) -> bool {
    tag_inner(s, "status").is_some()
        && tag_inner(s, "summary")
            .map(|sm| sm.trim_start().starts_with("Agent \""))
            .unwrap_or(false)
}

/// A queued message worth showing as a pending human turn — genuine prose, not a
/// background `<task-notification>`, an interrupt marker, or blank input.
fn is_queue_prose(s: &str) -> bool {
    let t = s.trim_start();
    !t.is_empty()
        && !t.starts_with("<task-notification>")
        && !t.starts_with("[Request interrupted")
        && t.chars().any(|c| !c.is_whitespace() && !c.is_control())
}

/// Build an [`Attachment`] from a `type:"attachment"` event's inner `attachment`
/// object, for the types that carry a file/plan worth surfacing. Returns `None` for
/// harness bookkeeping (listings, reminders, deltas, plan-mode toggles) and for
/// `queued_command` (rendered as a turn elsewhere). `content: Some` ⇒ downloadable
/// (embedded bytes); `content: None` ⇒ path-only (reveal in file manager).
fn attachment_from_event(a: &Value) -> Option<Attachment> {
    fn basename(p: &str) -> String {
        p.rsplit('/').next().unwrap_or(p).to_string()
    }
    let s = |k: &str| a.get(k).and_then(|x| x.as_str());
    match a.get("type").and_then(|t| t.as_str())? {
        // Full attached-file bytes, embedded → downloadable.
        "file" => {
            let f = a.get("content")?.get("file")?;
            let content = f.get("content").and_then(|c| c.as_str())?;
            let path = f
                .get("filePath")
                .and_then(|p| p.as_str())
                .or_else(|| s("filename"));
            let name = s("displayPath")
                .map(str::to_string)
                .or_else(|| path.map(basename))?;
            Some(Attachment {
                kind: "file",
                name,
                path: path.map(str::to_string),
                content: Some(AttachmentContent::Text(content.to_string())),
            })
        }
        // Full plan markdown, embedded and not shown inline anywhere → downloadable.
        "plan_file_reference" => {
            let content = s("planContent")?;
            let path = s("planFilePath");
            Some(Attachment {
                kind: "plan",
                name: path.map(basename).unwrap_or_else(|| "plan.md".to_string()),
                path: path.map(str::to_string),
                content: Some(AttachmentContent::Text(content.to_string())),
            })
        }
        // An in-editor file — its inline `snippet` is truncated, so reveal the real file.
        "edited_text_file" => {
            let path = s("filename")?;
            Some(Attachment {
                kind: "edited",
                name: basename(path),
                path: Some(path.to_string()),
                content: None,
            })
        }
        // A bare pointer to a file that was in context → reveal.
        "compact_file_reference" => {
            let path = s("filename")?;
            let name = s("displayPath")
                .map(str::to_string)
                .unwrap_or_else(|| basename(path));
            Some(Attachment {
                kind: "ref",
                name,
                path: Some(path.to_string()),
                content: None,
            })
        }
        _ => None,
    }
}

/// Build an image [`Attachment`] from an `{type:"image", source:{type:"base64",…}}`
/// content block (a pasted image, or a tool result that returned one). Images are not
/// `attachment` events — they ride inside message/tool-result content — so this is a
/// separate path from [`attachment_from_event`]. `None` for non-image / non-base64.
fn image_attachment(blk: &Value) -> Option<Attachment> {
    if blk.get("type").and_then(|t| t.as_str()) != Some("image") {
        return None;
    }
    let src = blk.get("source")?;
    if src.get("type").and_then(|t| t.as_str()) != Some("base64") {
        return None;
    }
    let b64 = src.get("data").and_then(|d| d.as_str())?.to_string();
    let mime = src
        .get("media_type")
        .and_then(|m| m.as_str())
        .unwrap_or("image/png")
        .to_string();
    let ext = mime
        .rsplit('/')
        .next()
        .filter(|e| !e.is_empty())
        .unwrap_or("png");
    Some(Attachment {
        kind: "image",
        name: format!("image.{ext}"),
        path: None,
        content: Some(AttachmentContent::Base64 { mime, b64 }),
    })
}

/// Record `ts` for every user turn in `out[*stamped..]`, advancing `stamped`.
pub(crate) fn stamp_user_turns(
    out: &[Block],
    stamped: &mut usize,
    ts: Option<f64>,
    user_times: &mut Vec<Option<f64>>,
) {
    for b in &out[*stamped..] {
        if matches!(b, Block::UserText(_) | Block::Command { .. }) {
            user_times.push(ts);
        }
    }
    *stamped = out.len();
}

fn extract_diffs(name: &str, input: &Value) -> Vec<(String, String)> {
    match name {
        "Edit" => {
            let o = input
                .get("old_string")
                .and_then(|s| s.as_str())
                .unwrap_or("");
            let n = input
                .get("new_string")
                .and_then(|s| s.as_str())
                .unwrap_or("");
            vec![(o.to_string(), n.to_string())]
        }
        "Write" => {
            let n = input.get("content").and_then(|s| s.as_str()).unwrap_or("");
            vec![(String::new(), n.to_string())]
        }
        "NotebookEdit" => {
            let n = input
                .get("new_source")
                .and_then(|s| s.as_str())
                .unwrap_or("");
            vec![(String::new(), n.to_string())]
        }
        "MultiEdit" => input
            .get("edits")
            .and_then(|e| e.as_array())
            .map(|edits| {
                edits
                    .iter()
                    .map(|e| {
                        (
                            e.get("old_string")
                                .and_then(|s| s.as_str())
                                .unwrap_or("")
                                .to_string(),
                            e.get("new_string")
                                .and_then(|s| s.as_str())
                                .unwrap_or("")
                                .to_string(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(blocks: &[Block]) -> Vec<&'static str> {
        blocks.iter().map(fold_key).collect()
    }

    /// Injected system content — a skill/command instruction body (`isMeta`) or a
    /// `/compact` continuation summary (`isCompactSummary`) — is NOT a human turn.
    /// It must fold as a system block, never a `❯` UserText (which would give it a
    /// phantom sidebar/sticky turn entry). A genuine user message between them still
    /// reads as a turn, so the turn count stays correct.
    #[test]
    fn injected_meta_and_compact_summary_are_not_user_turns() {
        let jsonl = r##"
{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"real question"}}
{"type":"user","isMeta":true,"timestamp":"2026-06-30T03:00:01.000Z","message":{"content":"# /loop — schedule a recurring or self-paced prompt\nParse the input below…"}}
{"type":"user","isCompactSummary":true,"isVisibleInTranscriptOnly":true,"timestamp":"2026-06-30T03:00:02.000Z","message":{"content":"This session is being continued from a previous conversation…"}}
{"type":"user","timestamp":"2026-06-30T03:00:03.000Z","message":{"content":"another real question"}}
"##;
        let blocks = parse(jsonl);
        // Two genuine turns; the injected pair folds as tool_result, not user.
        assert_eq!(
            kinds(&blocks),
            vec!["user", "tool_result", "tool_result", "user"],
            "{blocks:?}"
        );
        assert_eq!(
            blocks
                .iter()
                .filter(|b| matches!(b, Block::UserText(_)))
                .count(),
            2,
            "only the two human messages are turns"
        );
    }

    /// The two-tier queue model: a prose `enqueue` emits a `⧗ queued:` marker
    /// (`QueueEvent`); when it's later picked up (a content-less FIFO front pop or a
    /// content-named remove) the marker is dropped **only if no agent work happened in
    /// between** (immediate pickup — the `❯` turn alone conveys it). A prompt still
    /// queued at the end keeps its marker (live in-flight input). The interleaved
    /// background `<task-notification>` is tracked (no marker) so a front pop lands on
    /// it, not on a real prompt.
    #[test]
    fn queue_markers_suppress_on_immediate_pickup_but_survive_a_gap() {
        let jsonl = r##"
{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"real turn"}}
{"type":"queue-operation","operation":"enqueue","timestamp":"2026-06-30T03:00:01.000Z","content":"picked up immediately"}
{"type":"queue-operation","operation":"dequeue","timestamp":"2026-06-30T03:00:02.000Z"}
{"type":"queue-operation","operation":"enqueue","timestamp":"2026-06-30T03:00:03.000Z","content":"picked up after a gap"}
{"type":"assistant","timestamp":"2026-06-30T03:00:04.000Z","message":{"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls"}}]}}
{"type":"queue-operation","operation":"enqueue","timestamp":"2026-06-30T03:00:05.000Z","content":"<task-notification>\nbg\n</task-notification>"}
{"type":"queue-operation","operation":"dequeue","timestamp":"2026-06-30T03:00:06.000Z"}
{"type":"queue-operation","operation":"enqueue","timestamp":"2026-06-30T03:00:07.000Z","content":"still waiting"}
"##;
        let blocks = parse(jsonl);
        // "picked up immediately": enqueue→dequeue with no agent work → marker dropped.
        // "picked up after a gap": a Bash ran between enqueue and its front pop → marker kept.
        // "still waiting": never popped → marker kept. The task-notification: no marker.
        let markers: Vec<&str> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::QueueEvent { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            markers,
            vec!["picked up after a gap", "still waiting"],
            "{blocks:?}"
        );
        // The one real user turn is unaffected; markers are not turns.
        let users: Vec<&str> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::UserText(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(users, vec!["real turn"], "{blocks:?}");
    }

    /// A mid-turn prompt is usually recorded ONLY as a `queued_command` attachment at
    /// the point the agent consumes it (no standalone `user` event is ever written), so
    /// we render the human ones (`commandMode == "prompt"`) as a user turn in place —
    /// keeping the true chronological order — and skip `task-notification`s. Losing
    /// these would drop real messages the human typed.
    #[test]
    fn queued_command_attachment_renders_as_a_turn_in_order() {
        let jsonl = r##"
{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"first turn"}}
{"type":"assistant","timestamp":"2026-06-30T03:00:01.000Z","message":{"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls"}}]}}
{"type":"user","timestamp":"2026-06-30T03:00:02.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"b1","content":"out"}]}}
{"type":"attachment","timestamp":"2026-06-30T03:00:03.000Z","attachment":{"type":"queued_command","commandMode":"task-notification","prompt":"<task-notification>bg</task-notification>"}}
{"type":"attachment","timestamp":"2026-06-30T03:00:04.000Z","attachment":{"type":"queued_command","commandMode":"prompt","origin":{"kind":"human"},"prompt":"mid-turn interjection"}}
{"type":"assistant","timestamp":"2026-06-30T03:00:05.000Z","message":{"content":[{"type":"text","text":"ok"}]}}
{"type":"user","timestamp":"2026-06-30T03:00:06.000Z","message":{"content":"last turn"}}
"##;
        let blocks = parse(jsonl);
        let users: Vec<&str> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::UserText(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        // The human interjection appears as a turn between "first turn" and "last turn";
        // the task-notification attachment is not a turn.
        assert_eq!(
            users,
            vec!["first turn", "mid-turn interjection", "last turn"],
            "{blocks:?}"
        );
    }

    /// The four content-bearing attachment types surface as `Block::Attachment`:
    /// `file`/`plan` carry embedded text (downloadable → `content: Some`), while
    /// `edited_text_file`/`compact_file_reference` are path-only (reveal → `content:
    /// None`). Bookkeeping attachments (e.g. `skill_listing`) stay dropped.
    #[test]
    fn attachment_events_surface_with_download_vs_reveal() {
        let jsonl = r##"
{"type":"attachment","timestamp":"2026-06-30T03:00:00.000Z","attachment":{"type":"file","filename":"/w/backlog.md","displayPath":"backlog.md","content":{"type":"text","file":{"filePath":"/w/backlog.md","content":"# Backlog\nitem"}}}}
{"type":"attachment","timestamp":"2026-06-30T03:00:01.000Z","attachment":{"type":"plan_file_reference","planFilePath":"/p/plan-x.md","planContent":"# Plan\nstep 1"}}
{"type":"attachment","timestamp":"2026-06-30T03:00:02.000Z","attachment":{"type":"edited_text_file","filename":"/w/src/main.rs","snippet":"1\tfn main(){}"}}
{"type":"attachment","timestamp":"2026-06-30T03:00:03.000Z","attachment":{"type":"compact_file_reference","filename":"/w/src/lib.rs","displayPath":"src/lib.rs"}}
{"type":"attachment","timestamp":"2026-06-30T03:00:04.000Z","attachment":{"type":"skill_listing","content":"noise"}}
"##;
        let blocks = parse(jsonl);
        let atts: Vec<(&str, &str, bool, Option<&str>)> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Attachment(a) => Some((
                    a.kind,
                    a.name.as_str(),
                    a.content.is_some(),
                    a.path.as_deref(),
                )),
                _ => None,
            })
            .collect();
        assert_eq!(
            atts,
            vec![
                ("file", "backlog.md", true, Some("/w/backlog.md")),
                ("plan", "plan-x.md", true, Some("/p/plan-x.md")),
                ("edited", "main.rs", false, Some("/w/src/main.rs")),
                ("ref", "src/lib.rs", false, Some("/w/src/lib.rs")),
            ],
            "{blocks:?}"
        );
        // The embedded `file` content is the real bytes, ready to download.
        let file_text = blocks.iter().find_map(|b| match b {
            Block::Attachment(a) if a.kind == "file" => a.content.as_ref(),
            _ => None,
        });
        assert!(matches!(
            file_text,
            Some(AttachmentContent::Text(t)) if t == "# Backlog\nitem"
        ));
    }

    /// Base64 images surface as downloadable `Block::Attachment`s from both paths: a
    /// top-level image block in a prompt, and an image inside a tool result (e.g.
    /// reading a screenshot). Images ride in message/tool-result content, NOT in
    /// `attachment` events.
    #[test]
    fn base64_images_surface_from_prompt_and_tool_result() {
        let jsonl = r##"
{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":[{"type":"text","text":"look at this"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"Zm9v"}}]}}
{"type":"assistant","timestamp":"2026-06-30T03:00:01.000Z","message":{"content":[{"type":"tool_use","id":"r1","name":"Read","input":{"file_path":"/w/shot.png"}}]}}
{"type":"user","timestamp":"2026-06-30T03:00:02.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"r1","content":[{"type":"image","source":{"type":"base64","media_type":"image/jpeg","data":"YmFy"}}]}]}}
"##;
        let blocks = parse(jsonl);
        let imgs: Vec<(&str, &str)> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Attachment(a) if a.kind == "image" => match &a.content {
                    Some(AttachmentContent::Base64 { mime, .. }) => {
                        Some((a.name.as_str(), mime.as_str()))
                    }
                    _ => None,
                },
                _ => None,
            })
            .collect();
        assert_eq!(
            imgs,
            vec![("image.png", "image/png"), ("image.jpeg", "image/jpeg")],
            "{blocks:?}"
        );
    }

    /// A user message with no visible character — only whitespace or a control
    /// byte like `\x11` (a stray Ctrl-Q keystroke) — is a phantom, not a turn.
    #[test]
    fn control_only_user_message_is_dropped() {
        let jsonl = "\
{\"type\":\"user\",\"timestamp\":\"2026-06-30T03:00:00.000Z\",\"message\":{\"content\":\"\u{11}\"}}
{\"type\":\"user\",\"timestamp\":\"2026-06-30T03:00:01.000Z\",\"message\":{\"content\":\"real\"}}
";
        let blocks = parse(jsonl);
        assert_eq!(kinds(&blocks), vec!["user"], "{blocks:?}");
        assert!(matches!(&blocks[0], Block::UserText(t) if t == "real"));
    }

    /// A thinking block absorbs the activity tools that ran just before it and
    /// carries a duration = (its timestamp − the triggering event's timestamp).
    #[test]
    fn thinking_groups_preceding_tools_with_duration() {
        let jsonl = r#"
{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"go"}}
{"type":"assistant","timestamp":"2026-06-30T03:00:02.000Z","message":{"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls"}}]}}
{"type":"user","timestamp":"2026-06-30T03:00:03.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"b1","content":"out"}]}}
{"type":"assistant","timestamp":"2026-06-30T03:00:12.000Z","message":{"content":[{"type":"thinking","thinking":"hmm let me consider"}]}}
"#;
        let blocks = parse(jsonl);
        // The Bash is absorbed into the thinking (not a top-level block).
        assert_eq!(kinds(&blocks), vec!["user", "thinking"], "{blocks:?}");
        let Block::Thinking {
            duration_secs,
            tools,
            ..
        } = &blocks[1]
        else {
            panic!("not a thinking turn: {blocks:?}");
        };
        // 03:00:12 − 03:00:03 (last tool_result) = 9s, floored.
        assert_eq!(*duration_secs, Some(9));
        assert_eq!(tools.len(), 1, "did not absorb the preceding Bash");
    }

    /// Edit/Write tools are NOT absorbed into a following thinking (CC shows their
    /// diffs expanded); only transient activity tools (Bash/Read/…) group in.
    #[test]
    fn edit_stays_expanded_next_to_thinking() {
        let jsonl = r#"
{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"go"}}
{"type":"assistant","timestamp":"2026-06-30T03:00:02.000Z","message":{"content":[{"type":"tool_use","id":"e1","name":"Edit","input":{"file_path":"/x.rs","old_string":"a","new_string":"b"}}]}}
{"type":"assistant","timestamp":"2026-06-30T03:00:05.000Z","message":{"content":[{"type":"thinking","thinking":"ok"}]}}
"#;
        let blocks = parse(jsonl);
        assert_eq!(
            kinds(&blocks),
            vec!["user", "edit", "thinking"],
            "{blocks:?}"
        );
    }

    /// A skill load is ONE collapsible unit: the `Skill` tool_use names the skill, and
    /// the injected "Base directory for this skill: …" body is NESTED into that block's
    /// output — not a loose result block beside it, and never a `❯` user turn.
    #[test]
    fn skill_body_nests_into_the_skill_call() {
        let jsonl = r#"
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"s1","name":"Skill","input":{"skill":"dump-tasks"}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"s1","content":"Launching skill: dump-tasks"}]}}
{"type":"user","message":{"content":[{"type":"text","text":"Base directory for this skill: /Users/dev/.claude/skills/dump-tasks\n\n# dump-tasks\n\nTurn the work into a brief."}]}}
"#;
        let blocks = parse(jsonl);
        // Exactly one block — the Skill call — with the skill name as its target and
        // the fold key "skill" (default-folded, like reads/thinking).
        assert_eq!(kinds(&blocks), vec!["skill"], "{blocks:?}");
        match &blocks[0] {
            Block::ToolUse {
                name,
                target,
                output,
                ..
            } => {
                assert_eq!(name, "Skill");
                assert_eq!(target, "dump-tasks", "skill name not used as target");
                let out = output.as_deref().unwrap_or("");
                assert!(
                    out.contains("Launching skill: dump-tasks"),
                    "keeps the result"
                );
                assert!(
                    out.contains("Base directory for this skill:"),
                    "skill body nested into the Skill block: {out:?}"
                );
            }
            other => panic!("expected Skill ToolUse, got {other:?}"),
        }
    }

    /// With no preceding `Skill` block, a "Base directory…" body still folds on its own
    /// as a result block (the nesting is a best-effort attach, not a hard requirement).
    #[test]
    fn orphan_skill_body_still_folds_as_result() {
        let jsonl = r#"
{"type":"user","message":{"content":[{"type":"text","text":"Base directory for this skill: /x\n\n# s"}]}}
"#;
        let blocks = parse(jsonl);
        assert_eq!(kinds(&blocks), vec!["tool_result"], "{blocks:?}");
    }

    /// An `Agent` spawn becomes a `SubAgent` block (the "launched" event); its later
    /// completion `<task-notification>` becomes a SEPARATE `AgentDone` event at the point
    /// it arrived — the two-message model. The spawn's status is still back-patched to
    /// terminal (so active-tracking drops it from `a active N`), but the returned result
    /// renders on the `AgentDone`, not folded back onto the spawn.
    #[test]
    fn agent_spawn_and_completion_are_two_events() {
        let jsonl = r##"
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_A","name":"Agent","input":{"subagent_type":"code-reviewer","description":"Review the rewrite","prompt":"Review render.rs"}}]}}
{"type":"user","toolUseResult":{"agentId":"aXYZ1234","status":"async_launched","outputFile":"/t/aXYZ1234.output"},"message":{"content":[{"type":"tool_result","tool_use_id":"toolu_A","content":"async_launched"}]}}
{"type":"queue-operation","operation":"enqueue","content":"<task-notification>\n<task-id>aXYZ1234</task-id>\n<tool-use-id>toolu_A</tool-use-id>\n<status>completed</status>\n<summary>Agent \"Review the rewrite\" finished</summary>\n<result>Two gaps found.</result>\n</task-notification>"}
"##;
        let blocks = parse(jsonl);
        // Two agent blocks: the spawn (launched) then the completion (done).
        assert_eq!(kinds(&blocks), vec!["agent", "agent"], "{blocks:?}");
        let Block::SubAgent(sa) = &blocks[0] else {
            panic!("not a SubAgent: {blocks:?}")
        };
        assert_eq!(sa.tool_use_id, "toolu_A");
        assert_eq!(sa.agent_id, "aXYZ1234");
        assert_eq!(sa.agent_type, "code-reviewer");
        assert_eq!(sa.description, "Review the rewrite");
        assert_eq!(sa.prompt, "Review render.rs");
        assert_eq!(
            sa.status,
            AgentStatus::Completed,
            "spawn status back-patched for active-tracking"
        );
        assert_eq!(
            sa.result, None,
            "result renders on AgentDone, not the spawn"
        );
        // The completion is a distinct AgentDone event carrying status + result, with the
        // agent_type resolved back from the spawn.
        let Block::AgentDone {
            agent_id,
            agent_type,
            description,
            status,
            result,
        } = &blocks[1]
        else {
            panic!("second block is not AgentDone: {blocks:?}")
        };
        assert_eq!(agent_id, "aXYZ1234");
        assert_eq!(agent_type, "code-reviewer", "type resolved from the spawn");
        assert_eq!(description, "Review the rewrite");
        assert_eq!(*status, AgentStatus::Completed);
        assert_eq!(result.as_deref(), Some("Two gaps found."));
        // Both fold under the "agent" key; the default-collapse *policy* is a view concern
        // (asserted in `view`'s `default_fold_policy_collapses_agent_blocks`).
        assert_eq!(fold_key(&blocks[0]), "agent");
        assert_eq!(fold_key(&blocks[1]), "agent");
    }

    /// `parse_path` loads each `SubAgent`'s child transcript from the flat
    /// `<session>/subagents/agent-<id>.jsonl`, so the spawn's tool count is **node-
    /// scoped** (the child's tools, not the parent's), and `subtree_cost` rolls up.
    #[test]
    fn enrich_loads_child_scoped_and_rolls_up_cost() {
        use std::io::Write;
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let base = std::env::temp_dir().join(format!(
            "cr-subagent-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&base);
        let sess = base.join("proj").join("sid.jsonl");
        let sadir = base.join("proj").join("sid").join("subagents");
        std::fs::create_dir_all(&sadir).unwrap();
        // Parent: one Agent spawn; its own transcript has a Bash tool the child must NOT
        // be credited with.
        let parent = r##"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_P","name":"Bash","input":{"command":"ls"}}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_A","name":"Agent","input":{"subagent_type":"general-purpose","description":"child","prompt":"go"}}]}}
{"type":"user","toolUseResult":{"agentId":"achild01","status":"completed"},"message":{"content":[{"type":"tool_result","tool_use_id":"toolu_A","content":"done"}]}}
"##;
        std::fs::File::create(&sess)
            .unwrap()
            .write_all(parent.as_bytes())
            .unwrap();
        // Child transcript: two Read tools + model tokens (for a nonzero cost).
        let child = r##"{"type":"user","message":{"content":"go"}}
{"type":"assistant","message":{"model":"claude-opus-4-8","usage":{"input_tokens":1000,"output_tokens":500},"content":[{"type":"tool_use","id":"c1","name":"Read","input":{"file_path":"/a"}}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"c2","name":"Read","input":{"file_path":"/b"}}]}}
"##;
        std::fs::File::create(sadir.join("agent-achild01.jsonl"))
            .unwrap()
            .write_all(child.as_bytes())
            .unwrap();

        let blocks = parse_path(&sess).unwrap();
        let Some(Block::SubAgent(sa)) = blocks.iter().find(|b| matches!(b, Block::SubAgent(_)))
        else {
            panic!("no SubAgent: {blocks:?}")
        };
        assert!(
            sa.blocks.len() >= 2,
            "child transcript loaded: {}",
            sa.blocks.len()
        );
        // The live-tail child-file resolver finds the same file (Stage 6), and misses.
        assert!(
            subagent_file(&sess, "achild01").is_some(),
            "child file resolved"
        );
        assert!(subagent_file(&sess, "nope").is_none());
        // Node-scoped: the child's blocks are its own 2 Reads, not the parent's Bash. The
        // *count* (which folds coalesced activity into a thinking block's tool list) is a
        // render concern, asserted in `render`'s `child_scoped_tool_count`. Here we assert
        // the pure-model contract: the child transcript loaded and the cost rolled up.
        assert!(
            sa.subtree_cost.unwrap_or(0.0) > 0.0,
            "subtree cost rolled up"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn task_notification_folds_to_summary_line() {
        let jsonl = r#"
{"type":"user","message":{"role":"user","content":"<task-notification>\n<task-id>b1</task-id>\n<status>completed</status>\n<summary>Background command \"Build release\" completed (exit code 0)</summary>\n</task-notification>"}}
"#;
        let blocks = parse(jsonl);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::ToolResult(t) => {
                assert_eq!(
                    t,
                    "Background command \"Build release\" completed (exit code 0)"
                );
                assert!(!t.contains("task-notification"), "raw XML leaked: {t}");
                assert!(!t.contains("task-id"), "raw XML leaked: {t}");
            }
            other => panic!("expected ToolResult summary, got {other:?}"),
        }
    }

    #[test]
    fn relativize_uses_cwd_then_home_tilde() {
        use crate::engine::path::relativize_with;
        let home = Some("/Users/h");
        // Under the session cwd → relative.
        assert_eq!(
            relativize_with("/Users/h/proj/src/a.rs", "/Users/h/proj", home),
            "src/a.rs"
        );
        // Not under cwd but under $HOME → ~/…  (matches Claude Code).
        assert_eq!(
            relativize_with("/Users/h/.claude/x.md", "/Users/h/proj", home),
            "~/.claude/x.md"
        );
        // Outside both → left absolute.
        assert_eq!(
            relativize_with("/etc/hosts", "/Users/h/proj", home),
            "/etc/hosts"
        );
    }

    #[test]
    fn nothing_is_dropped_by_default() {
        // A Read, a non-modifying Bash (`ls`), an Edit, and a tool_result must
        // ALL produce blocks now — no parse-time filtering.
        let jsonl = r#"
{"type":"user","message":{"role":"user","content":"do it"}}
{"type":"assistant","message":{"content":[{"type":"text","text":"ok"},{"type":"tool_use","name":"Read","input":{"file_path":"/x.rs"}},{"type":"tool_use","name":"Bash","input":{"command":"ls -la"}},{"type":"tool_use","name":"Edit","input":{"file_path":"/x.rs","old_string":"a","new_string":"b"}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","content":"FILE CONTENTS"}]}}
"#;
        let blocks = parse(jsonl);
        // Nothing is dropped — but the consecutive Read + Bash coalesce into one
        // activity run (their blocks live inside it), and Edit stays expanded.
        assert_eq!(
            kinds(&blocks),
            vec!["user", "assistant", "thinking", "edit", "tool_result"]
        );
        let Block::Thinking { tools, .. } = &blocks[2] else {
            panic!("expected the coalesced Read+Bash run");
        };
        assert_eq!(
            kinds(tools),
            vec!["read", "bash"],
            "both preserved in the run"
        );
    }

    #[test]
    fn tool_result_text_is_not_truncated() {
        // Build a >20-line, long result; the full text must survive parsing.
        let big: String = (0..40)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\\n");
        let jsonl = format!(
            r#"{{"type":"user","message":{{"content":[{{"type":"tool_result","content":"{big}"}}]}}}}"#
        );
        let blocks = parse(&jsonl);
        assert_eq!(blocks.len(), 1);
        let Block::ToolResult(t) = &blocks[0] else {
            panic!("expected a tool_result block");
        };
        assert_eq!(t.lines().count(), 40, "result was truncated: {t:?}");
        assert!(t.contains("line 39"), "tail line missing");
    }

    #[test]
    fn joins_tooluseresult_metadata() {
        let jsonl = r#"
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Edit","input":{"file_path":"/x.rs","old_string":"a","new_string":"b"}}]}}
{"type":"user","toolUseResult":{"filePath":"/x.rs","structuredPatch":[{"oldStart":10,"oldLines":1,"newStart":12,"newLines":1,"lines":[" ctx","-a","+b"]}]},"message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"The file /x.rs has been updated successfully. (file state is current in your context — no need to Read it back)"}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t2","name":"Read","input":{"file_path":"/y.rs"}}]}}
{"type":"user","toolUseResult":{"type":"text","file":{"filePath":"/y.rs","content":"l1\nl2\nl3","numLines":3,"startLine":1,"totalLines":3}},"message":{"content":[{"type":"tool_result","tool_use_id":"t2","content":"l1\nl2\nl3"}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t3","name":"Bash","input":{"command":"ls"}}]}}
{"type":"user","toolUseResult":{"stdout":"file1\nfile2","stderr":"","interrupted":false},"message":{"content":[{"type":"tool_result","tool_use_id":"t3","content":"file1\nfile2"}]}}
"#;
        let blocks = parse(jsonl);
        // Edit stays expanded; the boilerplate Edit result is NOT a separate block.
        // Read + Bash are consecutive activity tools → coalesced into one activity run.
        assert_eq!(kinds(&blocks), vec!["edit", "thinking"]);

        let Block::ToolUse { patch, output, .. } = &blocks[0] else {
            panic!("expected Edit ToolUse");
        };
        assert_eq!(patch.as_ref().unwrap()[0].new_start, 12, "real newStart");
        assert!(output.is_none(), "edit boilerplate dropped");

        // Metadata is joined into the tools *before* coalescing — dig into the run.
        let Block::Thinking { tools, .. } = &blocks[1] else {
            panic!("expected a coalesced activity run");
        };
        let Block::ToolUse { read_lines, .. } = &tools[0] else {
            panic!("expected Read ToolUse");
        };
        assert_eq!(*read_lines, Some(3));

        let Block::ToolUse { output, .. } = &tools[1] else {
            panic!("expected Bash ToolUse");
        };
        assert_eq!(output.as_deref(), Some("file1\nfile2"));
    }

    #[test]
    fn consecutive_activity_tools_coalesce_into_one_summary() {
        // A run of activity tools with no thinking → one activity block (like CC's
        // "Searched for 1 pattern, ran N shell commands"); a lone one stays itself.
        let mut jsonl = String::from(
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"go\"}]}}\n",
        );
        jsonl.push_str("{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"g\",\"name\":\"Grep\",\"input\":{\"pattern\":\"foo\"}}]}}\n");
        for i in 0..9 {
            jsonl.push_str(&format!("{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"tool_use\",\"id\":\"b{i}\",\"name\":\"Bash\",\"input\":{{\"command\":\"echo {i}\"}}}}]}}}}\n"));
        }
        let blocks = parse(&jsonl);
        assert_eq!(kinds(&blocks), vec!["assistant", "thinking"]);
        let Block::Thinking {
            tools,
            text,
            duration_secs,
        } = &blocks[1]
        else {
            panic!("expected a coalesced activity run");
        };
        assert_eq!(tools.len(), 10, "1 grep + 9 bash coalesced");
        assert!(
            text.is_empty() && duration_secs.is_none(),
            "pure activity run"
        );
    }

    /// Transcripts are NOT strictly ordered: a `tool_result` can appear *before*
    /// its `tool_use` (compaction / sidechain reordering — seen in real 78/298 MB
    /// sessions). The streaming parse must still join them (via the tool_use id
    /// pre-scan + a pending buffer), or the Edit loses its structuredPatch line
    /// numbers and a Read loses its content.
    #[test]
    fn result_before_tool_use_still_joins() {
        let jsonl = r#"
{"type":"user","toolUseResult":{"filePath":"/x.rs","structuredPatch":[{"oldStart":10,"newStart":88,"lines":[" c","-a","+b"]}]},"message":{"content":[{"type":"tool_result","tool_use_id":"e1","content":"The file /x.rs has been updated successfully."}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"e1","name":"Edit","input":{"file_path":"/x.rs","old_string":"a","new_string":"b"}}]}}
"#;
        let blocks = parse(jsonl);
        // The out-of-order result joined its Edit — no stray orphan block.
        assert_eq!(kinds(&blocks), vec!["edit"], "{blocks:?}");
        let Block::ToolUse { patch, .. } = &blocks[0] else {
            panic!("expected Edit ToolUse");
        };
        assert_eq!(
            patch.as_ref().expect("patch joined from earlier result")[0].new_start,
            88,
            "structuredPatch line number lost — result-before-use not joined"
        );
    }

    /// A `tool_result` whose id belongs to no `tool_use` anywhere is a genuine
    /// orphan and is shown inline (not swallowed).
    #[test]
    fn orphan_result_with_no_tool_use_shown_inline() {
        let jsonl = r#"
{"type":"user","message":{"content":"go"}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"ghost","content":"orphan output"}]}}
"#;
        let blocks = parse(jsonl);
        assert_eq!(kinds(&blocks), vec!["user", "tool_result"], "{blocks:?}");
        let Block::ToolResult(t) = &blocks[1] else {
            panic!("expected orphan ToolResult");
        };
        assert_eq!(t, "orphan output");
    }

    /// `parse_path` (streaming file read, two passes) must produce exactly what
    /// `parse(&str)` produces for the same content.
    #[test]
    fn parse_path_matches_parse_str() {
        let jsonl = concat!(
            r#"{"type":"user","cwd":"/p","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"go"}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-06-30T03:00:02.000Z","message":{"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls"}}]}}"#,
            "\n",
            r#"{"type":"user","timestamp":"2026-06-30T03:00:03.000Z","toolUseResult":{"stdout":"out","stderr":""},"message":{"content":[{"type":"tool_result","tool_use_id":"b1","content":"out"}]}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-06-30T03:00:09.000Z","message":{"content":[{"type":"thinking","thinking":"hmm"}]}}"#,
            "\n",
        );
        let via_str = parse(jsonl);
        let file = std::env::temp_dir().join("claude-replay-parse-path-test.jsonl");
        std::fs::write(&file, jsonl).unwrap();
        let via_path = parse_path(&file).unwrap();
        std::fs::remove_file(&file).ok();
        assert_eq!(format!("{via_str:?}"), format!("{via_path:?}"));
    }

    /// The Layer-1 (`tokenize`) + Layer-2 (`replay`) split must be **bit-identical** to
    /// the fused `parse_main` — same blocks AND same `user_times` — across the whole
    /// golden corpus. This is the Phase-1 equivalence gate: only once this is rock-solid
    /// may `parse_main` be repointed at `tokenize`+`replay`.
    #[test]
    fn replay_tokenize_matches_parse_main() {
        fn assert_equiv(jsonl: &str) {
            let tool_ids = scan_tool_ids(jsonl.lines());
            let mut ut_main = Vec::new();
            let via_main = parse_main(jsonl.lines(), &tool_ids, &mut ut_main);
            let mut ut_engine = Vec::new();
            let via_engine = replay(&tokenize(jsonl.lines()), &mut ut_engine, &CLAUDE_SHAPING);
            assert_eq!(
                format!("{via_main:?}"),
                format!("{via_engine:?}"),
                "blocks differ for:\n{jsonl}"
            );
            assert_eq!(ut_main, ut_engine, "user_times differ for:\n{jsonl}");
        }

        let corpus: &[&str] = &[
            // Injected meta / compact-summary are not turns.
            r##"
{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"real question"}}
{"type":"user","isMeta":true,"timestamp":"2026-06-30T03:00:01.000Z","message":{"content":"# /loop — schedule\nParse the input…"}}
{"type":"user","isCompactSummary":true,"timestamp":"2026-06-30T03:00:02.000Z","message":{"content":"This session is being continued…"}}
{"type":"user","timestamp":"2026-06-30T03:00:03.000Z","message":{"content":"another real question"}}
"##,
            // Queue markers: immediate pickup vs a gap; interleaved task-notification.
            r##"
{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"real turn"}}
{"type":"queue-operation","operation":"enqueue","timestamp":"2026-06-30T03:00:01.000Z","content":"picked up immediately"}
{"type":"queue-operation","operation":"dequeue","timestamp":"2026-06-30T03:00:02.000Z"}
{"type":"queue-operation","operation":"enqueue","timestamp":"2026-06-30T03:00:03.000Z","content":"picked up after a gap"}
{"type":"assistant","timestamp":"2026-06-30T03:00:04.000Z","message":{"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls"}}]}}
{"type":"queue-operation","operation":"enqueue","timestamp":"2026-06-30T03:00:05.000Z","content":"<task-notification>\nbg\n</task-notification>"}
{"type":"queue-operation","operation":"dequeue","timestamp":"2026-06-30T03:00:06.000Z"}
{"type":"queue-operation","operation":"enqueue","timestamp":"2026-06-30T03:00:07.000Z","content":"still waiting"}
"##,
            // Queued-command attachment renders as a turn in order; task-notification skipped.
            r##"
{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"first turn"}}
{"type":"assistant","timestamp":"2026-06-30T03:00:01.000Z","message":{"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls"}}]}}
{"type":"user","timestamp":"2026-06-30T03:00:02.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"b1","content":"out"}]}}
{"type":"attachment","timestamp":"2026-06-30T03:00:03.000Z","attachment":{"type":"queued_command","commandMode":"task-notification","prompt":"<task-notification>bg</task-notification>"}}
{"type":"attachment","timestamp":"2026-06-30T03:00:04.000Z","attachment":{"type":"queued_command","commandMode":"prompt","origin":{"kind":"human"},"prompt":"mid-turn interjection"}}
{"type":"assistant","timestamp":"2026-06-30T03:00:05.000Z","message":{"content":[{"type":"text","text":"ok"}]}}
{"type":"user","timestamp":"2026-06-30T03:00:06.000Z","message":{"content":"last turn"}}
"##,
            // The four content-bearing attachment types + a dropped bookkeeping one.
            r##"
{"type":"attachment","timestamp":"2026-06-30T03:00:00.000Z","attachment":{"type":"file","filename":"/w/backlog.md","displayPath":"backlog.md","content":{"type":"text","file":{"filePath":"/w/backlog.md","content":"# Backlog\nitem"}}}}
{"type":"attachment","timestamp":"2026-06-30T03:00:01.000Z","attachment":{"type":"plan_file_reference","planFilePath":"/p/plan-x.md","planContent":"# Plan\nstep 1"}}
{"type":"attachment","timestamp":"2026-06-30T03:00:02.000Z","attachment":{"type":"edited_text_file","filename":"/w/src/main.rs","snippet":"1\tfn main(){}"}}
{"type":"attachment","timestamp":"2026-06-30T03:00:03.000Z","attachment":{"type":"compact_file_reference","filename":"/w/src/lib.rs","displayPath":"src/lib.rs"}}
{"type":"attachment","timestamp":"2026-06-30T03:00:04.000Z","attachment":{"type":"skill_listing","content":"noise"}}
"##,
            // Base64 images from a prompt and a tool result.
            r##"
{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":[{"type":"text","text":"look at this"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"Zm9v"}}]}}
{"type":"assistant","timestamp":"2026-06-30T03:00:01.000Z","message":{"content":[{"type":"tool_use","id":"r1","name":"Read","input":{"file_path":"/w/shot.png"}}]}}
{"type":"user","timestamp":"2026-06-30T03:00:02.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"r1","content":[{"type":"image","source":{"type":"base64","media_type":"image/jpeg","data":"YmFy"}}]}]}}
"##,
            // Control-only phantom message dropped.
            "{\"type\":\"user\",\"timestamp\":\"2026-06-30T03:00:00.000Z\",\"message\":{\"content\":\"\u{11}\"}}\n{\"type\":\"user\",\"timestamp\":\"2026-06-30T03:00:01.000Z\",\"message\":{\"content\":\"real\"}}\n",
            // Thinking groups preceding activity tools + duration.
            r#"
{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"go"}}
{"type":"assistant","timestamp":"2026-06-30T03:00:02.000Z","message":{"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls"}}]}}
{"type":"user","timestamp":"2026-06-30T03:00:03.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"b1","content":"out"}]}}
{"type":"assistant","timestamp":"2026-06-30T03:00:12.000Z","message":{"content":[{"type":"thinking","thinking":"hmm let me consider"}]}}
"#,
            // Edit stays expanded next to thinking.
            r#"
{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"go"}}
{"type":"assistant","timestamp":"2026-06-30T03:00:02.000Z","message":{"content":[{"type":"tool_use","id":"e1","name":"Edit","input":{"file_path":"/x.rs","old_string":"a","new_string":"b"}}]}}
{"type":"assistant","timestamp":"2026-06-30T03:00:05.000Z","message":{"content":[{"type":"thinking","thinking":"ok"}]}}
"#,
            // Skill body nests into the Skill call.
            r#"
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"s1","name":"Skill","input":{"skill":"dump-tasks"}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"s1","content":"Launching skill: dump-tasks"}]}}
{"type":"user","message":{"content":[{"type":"text","text":"Base directory for this skill: /Users/dev/.claude/skills/dump-tasks\n\n# dump-tasks\n\nTurn the work into a brief."}]}}
"#,
            // Orphan skill body still folds as a result.
            r#"
{"type":"user","message":{"content":[{"type":"text","text":"Base directory for this skill: /x\n\n# s"}]}}
"#,
            // Agent spawn + completion are two events.
            r##"
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_A","name":"Agent","input":{"subagent_type":"code-reviewer","description":"Review the rewrite","prompt":"Review render.rs"}}]}}
{"type":"user","toolUseResult":{"agentId":"aXYZ1234","status":"async_launched","outputFile":"/t/aXYZ1234.output"},"message":{"content":[{"type":"tool_result","tool_use_id":"toolu_A","content":"async_launched"}]}}
{"type":"queue-operation","operation":"enqueue","content":"<task-notification>\n<task-id>aXYZ1234</task-id>\n<tool-use-id>toolu_A</tool-use-id>\n<status>completed</status>\n<summary>Agent \"Review the rewrite\" finished</summary>\n<result>Two gaps found.</result>\n</task-notification>"}
"##,
            // Task-notification folds to its summary line.
            r#"
{"type":"user","message":{"role":"user","content":"<task-notification>\n<task-id>b1</task-id>\n<status>completed</status>\n<summary>Background command \"Build release\" completed (exit code 0)</summary>\n</task-notification>"}}
"#,
            // Nothing dropped by default: coalesced Read+Bash run, Edit expanded.
            r#"
{"type":"user","message":{"role":"user","content":"do it"}}
{"type":"assistant","message":{"content":[{"type":"text","text":"ok"},{"type":"tool_use","name":"Read","input":{"file_path":"/x.rs"}},{"type":"tool_use","name":"Bash","input":{"command":"ls -la"}},{"type":"tool_use","name":"Edit","input":{"file_path":"/x.rs","old_string":"a","new_string":"b"}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","content":"FILE CONTENTS"}]}}
"#,
            // toolUseResult metadata joins (Edit patch, Read numLines, Bash stdout).
            r#"
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Edit","input":{"file_path":"/x.rs","old_string":"a","new_string":"b"}}]}}
{"type":"user","toolUseResult":{"filePath":"/x.rs","structuredPatch":[{"oldStart":10,"oldLines":1,"newStart":12,"newLines":1,"lines":[" ctx","-a","+b"]}]},"message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"The file /x.rs has been updated successfully."}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t2","name":"Read","input":{"file_path":"/y.rs"}}]}}
{"type":"user","toolUseResult":{"type":"text","file":{"filePath":"/y.rs","content":"l1\nl2\nl3","numLines":3}},"message":{"content":[{"type":"tool_result","tool_use_id":"t2","content":"l1\nl2\nl3"}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t3","name":"Bash","input":{"command":"ls"}}]}}
{"type":"user","toolUseResult":{"stdout":"file1\nfile2","stderr":""},"message":{"content":[{"type":"tool_result","tool_use_id":"t3","content":"file1\nfile2"}]}}
"#,
            // Result-before-tool_use still joins (out-of-order).
            r#"
{"type":"user","toolUseResult":{"filePath":"/x.rs","structuredPatch":[{"oldStart":10,"newStart":88,"lines":[" c","-a","+b"]}]},"message":{"content":[{"type":"tool_result","tool_use_id":"e1","content":"The file /x.rs has been updated successfully."}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"e1","name":"Edit","input":{"file_path":"/x.rs","old_string":"a","new_string":"b"}}]}}
"#,
            // Orphan result with no tool_use anywhere shown inline.
            r#"
{"type":"user","message":{"content":"go"}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"ghost","content":"orphan output"}]}}
"#,
            // Slash command + inline stdout, caveat stripped.
            r#"
{"type":"user","message":{"role":"user","content":"<local-command-caveat>Caveat: noise</local-command-caveat><command-name>/compact</command-name><command-message>compact</command-message><command-args></command-args>"}}
{"type":"user","message":{"role":"user","content":"<local-command-stdout>Compacted (ctrl+o to see full summary)</local-command-stdout>"}}
"#,
            // Caveat-only message dropped.
            r#"{"type":"user","message":{"role":"user","content":"<local-command-caveat>just noise</local-command-caveat>"}}"#,
            // A standalone assistant thinking + text with a cwd on the first line.
            r#"{"type":"user","cwd":"/p","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"go"}}
{"type":"assistant","timestamp":"2026-06-30T03:00:02.000Z","message":{"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls"}}]}}
{"type":"user","timestamp":"2026-06-30T03:00:03.000Z","toolUseResult":{"stdout":"out","stderr":""},"message":{"content":[{"type":"tool_result","tool_use_id":"b1","content":"out"}]}}
{"type":"assistant","timestamp":"2026-06-30T03:00:09.000Z","message":{"content":[{"type":"thinking","thinking":"hmm"}]}}
"#,
        ];
        for j in corpus {
            assert_equiv(j);
        }
    }

    /// M8 keystone: folding messages in two pieces (`apply(a); apply(b)`) equals one
    /// `apply(all)` for every split point — same blocks, same `user_times`. This is the
    /// property that makes the streaming (M9) and incremental (M11) paths safe. Covers the
    /// state that must survive a split: the tool back-patch (`tool_slot`/`pending`), the
    /// queue lifecycle, the thinking clock (`trigger_ts`), and stamping (`pending_ts`).
    #[test]
    fn replayer_split_apply_is_identical() {
        fn ids(msgs: &[Message]) -> std::collections::HashSet<String> {
            msgs.iter()
                .filter_map(|m| match m {
                    Message::ToolUse { id, .. } if !id.is_empty() => Some(id.clone()),
                    _ => None,
                })
                .collect()
        }
        fn assert_split(jsonl: &str) {
            let msgs = tokenize(jsonl.lines());
            let ti = ids(&msgs);
            let mut whole = Replayer::new(&CLAUDE_SHAPING, ti.clone());
            whole.apply(&msgs);
            let whole = whole.into_blocks();
            for k in 0..=msgs.len() {
                let mut r = Replayer::new(&CLAUDE_SHAPING, ti.clone());
                r.apply(&msgs[..k]);
                r.apply(&msgs[k..]);
                let split = r.into_blocks();
                assert_eq!(
                    format!("{:?}", whole.0),
                    format!("{:?}", split.0),
                    "blocks differ, split at {k} of {}:\n{jsonl}",
                    msgs.len()
                );
                assert_eq!(
                    whole.1, split.1,
                    "user_times differ, split at {k}:\n{jsonl}"
                );
            }
        }
        // tool_use then its result (back-patch across the split) + a later thinking block.
        assert_split(concat!(
            r#"{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"go"}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-06-30T03:00:01.000Z","message":{"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls"}}]}}"#,
            "\n",
            r#"{"type":"user","timestamp":"2026-06-30T03:00:02.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"b1","content":"out"}]}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-06-30T03:00:09.000Z","message":{"content":[{"type":"thinking","thinking":"hmm"}]}}"#,
            "\n",
        ));
        // queue enqueue/dequeue lifecycle across the split.
        assert_split(concat!(
            r#"{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"real turn"}}"#,
            "\n",
            r#"{"type":"queue-operation","operation":"enqueue","timestamp":"2026-06-30T03:00:01.000Z","content":"picked up after a gap"}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-06-30T03:00:02.000Z","message":{"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls"}}]}}"#,
            "\n",
            r#"{"type":"queue-operation","operation":"dequeue","timestamp":"2026-06-30T03:00:03.000Z"}"#,
            "\n",
        ));
        // injected meta + real turns (user-turn stamping across the split).
        assert_split(concat!(
            r#"{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"real question"}}"#,
            "\n",
            r#"{"type":"user","isMeta":true,"timestamp":"2026-06-30T03:00:01.000Z","message":{"content":"meta note"}}"#,
            "\n",
            r#"{"type":"user","timestamp":"2026-06-30T03:00:03.000Z","message":{"content":"another real question"}}"#,
            "\n",
        ));
    }

    /// M11 keystone: driving the `Replayer` **one line at a time** (a live tail: `decode` the
    /// line, pre-scan its ids via `extend_tool_ids`, `apply`, `snapshot`) yields byte-identical
    /// blocks + user_times to a full batch `replay(tokenize(whole))` — at EVERY prefix, not
    /// just the end. This is the incremental-fold guarantee the live follower (M11 routing)
    /// stands on; a rewritten tail is handled by the follower rebuilding from scratch (which
    /// is trivially the full replay of the new content).
    #[test]
    fn incremental_line_by_line_matches_full_replay() {
        fn assert_follow(lines: &[&str]) {
            let mut cwd = String::new();
            let mut r = Replayer::new(&CLAUDE_SHAPING, std::collections::HashSet::new());
            for (i, line) in lines.iter().enumerate() {
                let mut delta = Vec::new();
                decode_line(line, &mut cwd, &mut delta);
                r.extend_tool_ids(delta.iter().filter_map(|m| match m {
                    Message::ToolUse { id, .. } if !id.is_empty() => Some(id.clone()),
                    _ => None,
                }));
                r.apply(&delta);
                // Snapshot after each line must match a full replay of the lines so far.
                let (inc_blocks, inc_ut) = r.snapshot();
                let mut ref_ut = Vec::new();
                let ref_blocks = replay(
                    &tokenize(lines[..=i].iter().copied()),
                    &mut ref_ut,
                    &CLAUDE_SHAPING,
                );
                assert_eq!(
                    format!("{ref_blocks:?}"),
                    format!("{inc_blocks:?}"),
                    "blocks differ after line {i}"
                );
                assert_eq!(ref_ut, inc_ut, "user_times differ after line {i}");
            }
        }
        // tool_use then its result (back-patch across poll boundaries) + a trailing thinking.
        assert_follow(&[
            r#"{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"go"}}"#,
            r#"{"type":"assistant","timestamp":"2026-06-30T03:00:01.000Z","message":{"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls"}}]}}"#,
            r#"{"type":"user","timestamp":"2026-06-30T03:00:02.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"b1","content":"out"}]}}"#,
            r#"{"type":"assistant","timestamp":"2026-06-30T03:00:09.000Z","message":{"content":[{"type":"thinking","thinking":"hmm"}]}}"#,
        ]);
        // queue enqueue then (later poll) dequeue — the lifecycle spans polls.
        assert_follow(&[
            r#"{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"turn"}}"#,
            r#"{"type":"queue-operation","operation":"enqueue","timestamp":"2026-06-30T03:00:01.000Z","content":"picked up after a gap"}"#,
            r#"{"type":"assistant","timestamp":"2026-06-30T03:00:02.000Z","message":{"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls"}}]}}"#,
            r#"{"type":"queue-operation","operation":"dequeue","timestamp":"2026-06-30T03:00:03.000Z"}"#,
        ]);
        // injected meta between real turns (user-turn stamping across polls).
        assert_follow(&[
            r#"{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"real question"}}"#,
            r#"{"type":"user","isMeta":true,"timestamp":"2026-06-30T03:00:01.000Z","message":{"content":"meta note"}}"#,
            r#"{"type":"user","timestamp":"2026-06-30T03:00:03.000Z","message":{"content":"another real question"}}"#,
        ]);
    }

    #[test]
    fn fold_keys_categorize_tools() {
        let mk = |name: &str| Block::ToolUse {
            name: name.into(),
            target: String::new(),
            diffs: vec![],
            output: None,
            patch: None,
            read_lines: None,
        };
        assert_eq!(fold_key(&mk("Read")), "read");
        assert_eq!(fold_key(&mk("Grep")), "read");
        assert_eq!(fold_key(&mk("Bash")), "bash");
        assert_eq!(fold_key(&mk("Edit")), "edit");
        assert_eq!(fold_key(&mk("MultiEdit")), "edit");
        assert_eq!(fold_key(&mk("Write")), "write");
        assert_eq!(fold_key(&mk("SomeMcpTool")), "tool");
        assert_eq!(
            fold_key(&Block::Thinking {
                text: "x".into(),
                duration_secs: None,
                tools: vec![]
            }),
            "thinking"
        );
        assert_eq!(fold_key(&Block::ToolResult("x".into())), "tool_result");
    }

    /// M13: the one `BlockKind` projects to a coarse `fold_key` (TUI / filter) and a fine
    /// `html` (`data-kind`). Pin the distinctions that used to be two separate functions:
    /// Thinking splits think/act in html but coarsens to `thinking`; ToolResult is `tool` in
    /// html but `tool_result` in fold_key; tool names project identically.
    #[test]
    fn block_kind_fine_html_vs_coarse_fold_key() {
        let tool = |name: &str| Block::ToolUse {
            name: name.into(),
            target: String::new(),
            diffs: vec![],
            output: None,
            patch: None,
            read_lines: None,
        };
        let bare = Block::Thinking {
            text: "x".into(),
            duration_secs: None,
            tools: vec![],
        };
        let act = Block::Thinking {
            text: "x".into(),
            duration_secs: None,
            tools: vec![tool("Bash")],
        };
        assert_eq!(block_kind(&bare).html(), "think");
        assert_eq!(block_kind(&bare).fold_key(), "thinking");
        assert_eq!(block_kind(&act).html(), "act");
        assert_eq!(block_kind(&act).fold_key(), "thinking");
        assert_eq!(block_kind(&Block::ToolResult("x".into())).html(), "tool");
        assert_eq!(
            block_kind(&Block::ToolResult("x".into())).fold_key(),
            "tool_result"
        );
        for (n, k) in [
            ("Bash", "bash"),
            ("Edit", "edit"),
            ("MultiEdit", "edit"),
            ("Read", "read"),
            ("Grep", "read"),
            ("Write", "write"),
            ("Skill", "skill"),
            ("SomeMcpTool", "tool"),
        ] {
            assert_eq!(block_kind(&tool(n)).html(), k, "html {n}");
            assert_eq!(block_kind(&tool(n)).fold_key(), k, "fold {n}");
        }
    }

    #[test]
    fn slash_command_becomes_command_block_caveat_stripped() {
        // A /compact invocation with inline stdout and a caveat: one Command
        // block, caveat dropped, no raw tags surviving.
        let jsonl = r#"
{"type":"user","message":{"role":"user","content":"<local-command-caveat>Caveat: noise</local-command-caveat><command-name>/compact</command-name><command-message>compact</command-message><command-args></command-args>"}}
{"type":"user","message":{"role":"user","content":"<local-command-stdout>Compacted (ctrl+o to see full summary)</local-command-stdout>"}}
"#;
        let blocks = parse(jsonl);
        assert_eq!(
            blocks.len(),
            1,
            "should be a single Command block: {blocks:?}"
        );
        let Block::Command { name, args, output } = &blocks[0] else {
            panic!("expected Block::Command, got {:?}", blocks[0]);
        };
        assert_eq!(name, "/compact");
        assert!(args.is_empty(), "no args expected: {args:?}");
        assert_eq!(
            output,
            &vec!["Compacted (ctrl+o to see full summary)".to_string()]
        );
        // No raw wrapper tags leaked through.
        let joined = format!("{blocks:?}");
        assert!(!joined.contains("command-name"), "raw tag leaked: {joined}");
        assert!(!joined.contains("caveat"), "caveat leaked: {joined}");
    }

    #[test]
    fn caveat_only_message_is_dropped() {
        let jsonl = r#"{"type":"user","message":{"role":"user","content":"<local-command-caveat>just noise</local-command-caveat>"}}"#;
        assert!(parse(jsonl).is_empty(), "caveat-only should yield nothing");
    }

    #[test]
    fn tool_target_relativizes_paths_under_session_cwd() {
        // Relative to the transcript's cwd (the repo root), not peek's runtime cwd.
        let base = "/Users/dev/project";
        let input = serde_json::json!({ "file_path": "/Users/dev/project/src/picker.rs" });
        assert_eq!(tool_target(&input, base), "src/picker.rs");

        // A path outside the session cwd is left absolute.
        let outside = serde_json::json!({ "file_path": "/etc/hosts" });
        assert_eq!(tool_target(&outside, base), "/etc/hosts");
    }

    #[test]
    fn tool_target_keeps_command_newlines_but_flattens_others() {
        // A multi-line shell command keeps its line breaks (the header lays it out
        // across rows); descriptions/patterns stay one line.
        let cmd = serde_json::json!({ "command": "cd /x\ncargo test" });
        assert_eq!(tool_target(&cmd, "/x"), "cd /x\ncargo test");

        let desc = serde_json::json!({ "description": "line one\nline two" });
        assert_eq!(tool_target(&desc, "/x"), "line one line two");
    }
}
