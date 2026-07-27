//! The **agent-neutral render-block vocabulary**: the [`Block`] data model (+ [`Attachment`],
//! [`SubAgent`], [`AgentStatus`], …) and its classification — [`block_kind`] and the coarse
//! [`fold_key`] / fine [`BlockKind::html`] projections that drive folding and styling. The
//! machinery that *builds* these blocks — the Layer-2 `Replayer` fold, its `Shaping` seam, the
//! `parse_stream` driver — lives in `engine::replay`; each agent's Layer-1 tokenizer in
//! `claude_model` / `codex_model` (all crate-internal). Nothing here is dropped or truncated;
//! what shows collapsed is a fold-policy decision made in `view`.

use std::path::PathBuf;

// ── semantic aliases for otherwise-ambiguous primitives ────────────────────────────────────
// Transparent type aliases (not distinct types): they make a field's *meaning* — and its unit
// — readable at the API surface, without the `.0` ergonomics of a newtype. Use them wherever
// the public data model carries one of these quantities.

/// A flat, **zero-based index into a [`Session`](crate::Session)'s `blocks`** — the position of
/// a block in the top-level stream (a valid subscript, `session.blocks[i]`). It is **not** a
/// byte offset into the transcript nor a line number.
pub type BlockIndex = usize;

/// An absolute instant as **seconds since the Unix epoch, 1970-01-01T00:00:00 UTC**. Parsed
/// from the transcript's ISO-8601 UTC timestamps (`…Z`), so it is **UTC-based and
/// timezone-independent** — no local-zone offset is ever applied; render in whatever zone you
/// like. `f64` keeps sub-second precision (transcripts are whole-second in practice). A
/// duration is the difference of two of these, in seconds.
pub type EpochSeconds = f64;

/// A monetary amount in **US dollars** — e.g. an estimated token cost (dollars, not tokens).
pub type UsdCost = f64;

/// A byte offset into a transcript file (a position, not a length/count).
pub type ByteOffset = u64;

/// A spawned sub-agent's id — its [`SubAgent::agent_id`] (== the completion event's
/// `AgentDone::agent_id`), which also names the child transcript file `agent-<id>.jsonl`. The
/// key of a [`Session`](crate::Session)'s `sub_agents` map ([`SubAgentMeta`]).
pub type AgentId = String;

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

/// One render block — the agent-neutral unit of a parsed transcript. A [`Session`](crate::Session)'s
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
/// What an [`Attachment`] is — the closed set that drives its header label and how it's
/// surfaced. `File` is an embedded file body; `Plan` a plan-mode document; `Edited` / `Ref`
/// mark a file the turn edited or merely referenced; `Image` an embedded image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentKind {
    File,
    Plan,
    Edited,
    Ref,
    Image,
}
impl AttachmentKind {
    /// The lowercase label shown in the block header (`file`/`plan`/`edited`/`ref`/`image`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Plan => "plan",
            Self::Edited => "edited",
            Self::Ref => "ref",
            Self::Image => "image",
        }
    }
}
impl std::fmt::Display for AttachmentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Attachment {
    /// What kind of attachment this is (drives the header label); see [`AttachmentKind`].
    pub kind: AttachmentKind,
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
    /// This agent's own cost plus all descendants', rolled up (US dollars); see [`UsdCost`].
    /// `None` if unknown.
    pub subtree_cost: Option<UsdCost>,
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
    /// A terminal state — no more activity expected (drives "running" animation off).
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Killed | Self::Stopped
        )
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

/// The single lookup-owner of a spawned sub-agent's intrinsic attributes + the pointers needed
/// to locate its lifecycle events (in the parent's `blocks`) and its on-disk artifacts.
/// Replaces the derived `SessionIndex.agents` copy. The blocks remain the source for what they
/// render; this is the navigation/lookup index. Keyed by [`AgentId`] in a
/// [`Session`](crate::Session)'s `sub_agents` map.
#[derive(Debug, Clone, PartialEq)]
pub struct SubAgentMeta {
    /// The sub-agent's **type label from the spawn** (`SubAgent::agent_type`) — a free-form,
    /// open-set string (e.g. `general-purpose`, `code-reviewer`), **not** the [`Agent`](crate::Agent)
    /// that produced the transcript. May be empty.
    pub agent_type: String,
    /// Terminal-or-running truth — the liveness signal; see [`AgentStatus`]. Mirrors the spawn
    /// `SubAgent::status`.
    pub status: AgentStatus,
    /// Rolled-up **cost in US dollars** of this sub-agent *and* its descendants, from the spawn
    /// `SubAgent::subtree_cost`. `None` when the child transcript wasn't loaded or cost couldn't
    /// be derived.
    pub subtree_cost: Option<UsdCost>,
    // ── locate the agent's generated artifacts ──
    /// The child transcript file (`subagents/agent-<id>.jsonl`), resolved by the path-aware
    /// parse. `None` on a flat/unresolved parse or when the file is absent.
    pub transcript: Option<PathBuf>,
    /// The async result sidecar (`tasks/agent-<id>.output`) for a background spawn, from the
    /// spawn `SubAgent::output_file`. `None` for a synchronous spawn.
    pub output_file: Option<String>,
    // ── pointers to the two events in the parent's blocks vector ──
    /// Index into the parent [`Session`](crate::Session)'s `blocks` of this agent's spawn
    /// [`Block::SubAgent`] — the jump target; see [`BlockIndex`].
    pub spawn_at: BlockIndex,
    /// Index into the parent's `blocks` of the matching completion [`Block::AgentDone`], if it
    /// arrived; `None` while the agent is still running / its completion never came.
    pub done_at: Option<BlockIndex>,
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

/// Tools that get summarized into a `Thought for …` turn line (transient reads/searches
/// whose results feed the thinking) rather than shown expanded; Edit/Write/other tools
/// produce durable output (diffs, etc.) and stay expanded.
///
/// **Canonical tool vocabulary.** This and [`block_kind`] (and the fold's `"Skill"` check)
/// classify on Claude Code's tool *names* — that's the engine's canonical vocabulary. It's
/// agent-neutral by contract, not by coincidence: each agent's `Shaping::build_tool`
/// normalizes its own tool names onto these (see `codex_model::normalize_tool_name`), so a
/// tool a given agent never emits simply never matches. A new agent maps its tools into this
/// vocabulary in its adapter; the shared classifiers don't grow per-agent arms.
pub(crate) fn is_activity_tool(name: &str) -> bool {
    matches!(
        name,
        "Bash" | "Read" | "NotebookRead" | "Grep" | "Glob" | "LS"
    )
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
