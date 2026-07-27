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

/// One render block — the agent-neutral unit of a parsed transcript. A [`Session`]'s
/// `blocks` is an ordered `Vec<Block>` with tool results already joined onto their calls and
/// activity coalesced into thinking turns; each variant carries the content a presenter needs.
/// Classify a block with [`block_kind`] / [`fold_key`] and test collapsibility with [`foldable`].
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

/// Whether a block can be collapsed/expanded (has foldable body content). The single
/// source of truth for both presenters — the TUI (`render::foldable`) and the HTML export
/// (`html_export::is_fold`) delegate here so they can never disagree. Prose turns
/// (`UserText`/`AssistantText`), the `⧗ queued` marker, and attachments are not foldable;
/// tools, results, thinking, commands, and sub-agent spawn/completion blocks are.
pub fn foldable(b: &Block) -> bool {
    matches!(
        b,
        Block::ToolUse { .. }
            | Block::ToolResult(_)
            | Block::Thinking { .. }
            | Block::Command { .. }
            | Block::SubAgent(_)
            | Block::AgentDone { .. }
    )
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
/// Agent-agnostic: it folds the shared [`Message`](crate::engine::message::Message)
/// vocabulary and parses **no** raw agent formats — each agent's L1 decoder maps its own
/// transcript shapes onto these structured messages (completions, commands, skill bodies,
/// injected notes, the queue lifecycle), so the fold is the same code for every agent. The
/// one agent-specific seam is `shaping` (tool-block build, result back-patch, orphan policy,
/// turn `finish`). Variants an agent doesn't produce (e.g. Codex emits no `QueueOp`/
/// `Completion`/`SkillBody`) simply never reach their arms.
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
    completions: Vec<CompletionRec>,
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
                Message::UserText { text } => {
                    self.out.push(Block::UserText(text.clone()));
                }
                Message::SystemNote { text } => {
                    self.out.push(Block::ToolResult(text.clone()));
                }
                Message::SkillBody { text, fallback } => {
                    // L1 detected the skill body; the fold only nests it into the most recent
                    // `Skill` block (stateful), falling back to a loose result block.
                    if !attach_skill_body(&mut self.out, self.last_skill, text)
                        && !fallback.is_empty()
                    {
                        self.out.push(Block::ToolResult(fallback.clone()));
                    }
                }
                Message::Command { name, args, output } => {
                    self.out.push(Block::Command {
                        name: name.clone(),
                        args: args.clone(),
                        output: output.clone(),
                    });
                }
                Message::CommandStdout { text } => {
                    // Attach to the command it follows, else show it command-less.
                    if let Some(Block::Command { output, .. }) = self.out.last_mut() {
                        output.push(text.clone());
                    } else {
                        self.out.push(Block::Command {
                            name: String::new(),
                            args: String::new(),
                            output: vec![text.clone()],
                        });
                    }
                }
                Message::AttachmentPrompt { text } => {
                    self.out.push(Block::UserText(text.clone()));
                }
                Message::Attachment(att) => {
                    self.out.push(Block::Attachment(att.clone()));
                }
                Message::Completion {
                    tool_use_id,
                    task_id,
                    status,
                    description,
                    result,
                } => {
                    // L1 already parsed the notification; the fold only places the block and
                    // records the terminal status for the post-loop `SubAgent` back-patch.
                    self.completions.push(CompletionRec {
                        tool_use_id: tool_use_id.clone(),
                        task_id: task_id.clone(),
                        status: *status,
                    });
                    let agent_id = if !task_id.is_empty() {
                        task_id.clone()
                    } else {
                        tool_use_id.clone()
                    };
                    self.out.push(Block::AgentDone {
                        agent_id,
                        agent_type: String::new(),
                        description: description.clone(),
                        status: status.unwrap_or(AgentStatus::Completed),
                        result: result.clone(),
                    });
                }
                Message::QueueOp { op, content, prose } => match op {
                    QueueOpKind::Enqueue => {
                        if let Some(c) = content {
                            let marker_idx = if *prose {
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
    completions: &[CompletionRec],
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
        for rec in completions {
            let idx = (!rec.tool_use_id.is_empty())
                .then(|| tool_slot.get(&rec.tool_use_id).copied())
                .flatten()
                .or_else(|| {
                    (!rec.task_id.is_empty())
                        .then(|| agent_slot.get(&rec.task_id).copied())
                        .flatten()
                });
            if let Some(Block::SubAgent(sa)) = idx.and_then(|i| out.get_mut(i)) {
                if let Some(st) = rec.status {
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

/// A structured agent/task completion (L1-parsed from the raw notification) — the fold's
/// record for back-patching a `SubAgent`'s terminal status after the loop. `status` is
/// `None` when the source carried no explicit status (then the spawn is left untouched).
pub(crate) struct CompletionRec {
    pub(crate) tool_use_id: String,
    pub(crate) task_id: String,
    pub(crate) status: Option<AgentStatus>,
}

// (queue-operation handling is inlined in `parse_main`'s `Some("queue-operation")`
// arm — it needs the block list, `content_seq`, and `suppress`.)

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
    // its parse entries + decode helpers now live in `claude_model`.

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
}
