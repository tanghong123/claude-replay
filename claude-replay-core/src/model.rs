//! The **agent-neutral engine**: the [`Block`] render-block data model + classification
//! ([`block_kind`]/[`fold_key`]), the stateful Layer-2 [`Replayer`]/[`replay`] fold and its
//! [`Shaping`] seam, the streaming [`parse_stream`] driver, the shared message-handling, and
//! the cross-agent parse dispatchers ([`parse_for`]/[`parse_path_for`]/…). Each agent's
//! Layer-1 tokenizer + `Shaping` lives in its own adapter — [`crate::claude_model`] and
//! [`crate::codex_model`]. Nothing is dropped or truncated; what shows collapsed is a
//! fold-policy decision made in `view`.

use crate::engine::message::{Message, QueueOpKind};
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
pub(crate) fn summary_description(summary: &str) -> String {
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
pub(crate) fn tag_inner<'a>(s: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = s.find(&open)? + open.len();
    let rest = &s[start..];
    let end = rest.find(&close)?;
    Some(&rest[..end])
}

/// Remove every `<local-command-caveat>…</local-command-caveat>` block (pure
/// noise Claude Code injects around local commands), returning the remainder.
pub(crate) fn strip_caveat(s: &str) -> String {
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
pub(crate) fn is_skill_body(s: &str) -> bool {
    s.trim_start().starts_with("Base directory for this skill:")
}

/// Append a loaded skill's instruction body to its `Skill` tool_use block at `idx`,
/// so the whole skill load reads as one collapsible unit (named by the skill) instead
/// of a loose result block beside the call. Returns `false` when there's no recent
/// `Skill` block to attach to — the caller then falls back to a standalone result.
pub(crate) fn attach_skill_body(out: &mut [Block], idx: Option<usize>, body: &str) -> bool {
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
pub(crate) fn push_injected(s: &str, out: &mut Vec<Block>) {
    let cleaned = strip_caveat(s);
    let cleaned = cleaned.trim();
    if !cleaned.is_empty() {
        out.push(Block::ToolResult(cleaned.to_string()));
    }
}

/// Turn one plain-string `user` message into block(s). A slash-command
/// invocation (`<command-name>`) and its `<local-command-stdout>` become a
/// `Block::Command`; the `<local-command-caveat>` noise is dropped; everything
/// else is ordinary `UserText`.
pub(crate) fn push_user_string(s: &str, out: &mut Vec<Block>) {
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

/// Tools Claude Code summarizes into a `Thought for …` turn line (transient reads/
/// searches whose results feed the thinking) rather than showing expanded. Edit/
/// Write/other tools produce durable output (diffs, etc.) and stay expanded.
/// `pub(crate)` so the live-tail path (`view::ingest`) can re-group a thinking
/// block with activity tools that arrived in an earlier poll.
pub fn is_activity_tool(name: &str) -> bool {
    matches!(
        name,
        "Bash" | "Read" | "NotebookRead" | "Grep" | "Glob" | "LS"
    )
}

/// Parse JSONL text with the parser for `agent`.
pub fn parse_for(agent: Agent, jsonl: &str) -> Vec<Block> {
    match agent {
        Agent::Claude => crate::claude_model::parse(jsonl),
        Agent::Codex => crate::codex_model::parse_codex(jsonl),
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
    // Each agent's L1 adapter owns its streaming parse; the shared driver is `parse_stream`.
    let (blocks, metrics) = match agent {
        Agent::Claude => crate::claude_model::parse_claude_path_timed(path, &mut times)?,
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
        crate::claude_model::enrich_from_subagents(path, &mut blocks);
    }
    Ok((blocks, times))
}

/// Streaming file parse with the parser for `agent`.
pub fn parse_path_for(agent: Agent, path: &std::path::Path) -> std::io::Result<Vec<Block>> {
    match agent {
        Agent::Claude => crate::claude_model::parse_path(path),
        Agent::Codex => crate::codex_model::parse_codex_path(path),
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

/// One entry in the reconstructed prompt queue. `marker_idx` is the index of this
/// prompt's `⧗ queued:` marker in the block list (prose only); `content_at_enqueue`
/// snapshots `content_seq` at submit so a later pop can tell whether any agent work
/// happened in between (immediate → suppress the marker).
pub(crate) struct QueueItem {
    pub(crate) content: String,
    pub(crate) marker_idx: Option<usize>,
    pub(crate) content_at_enqueue: usize,
}

// (queue-operation handling is inlined in `parse_main`'s `Some("queue-operation")`
// arm — it needs the block list, `content_seq`, and `suppress`.)

/// Is this string an agent-completion `<task-notification>` — `summary` "Agent \"…\"
/// finished" with a `status` — as opposed to a background-`Bash` or `Monitor` one?
/// (Their task-id namespaces differ too: agents `a…`, background `b…`.)
pub(crate) fn is_agent_notification(s: &str) -> bool {
    tag_inner(s, "status").is_some()
        && tag_inner(s, "summary")
            .map(|sm| sm.trim_start().starts_with("Agent \""))
            .unwrap_or(false)
}

/// A queued message worth showing as a pending human turn — genuine prose, not a
/// background `<task-notification>`, an interrupt marker, or blank input.
pub(crate) fn is_queue_prose(s: &str) -> bool {
    let t = s.trim_start();
    !t.is_empty()
        && !t.starts_with("<task-notification>")
        && !t.starts_with("[Request interrupted")
        && t.chars().any(|c| !c.is_whitespace() && !c.is_control())
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

#[cfg(test)]
mod tests {
    use super::*;
    // The Claude-parsing tests below exercise the shared engine through Claude's adapter;
    // its parse entries + decode helpers now live in `claude_model`.
    use crate::claude_model::*;

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
