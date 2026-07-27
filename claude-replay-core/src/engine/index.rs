//! The [`SessionIndex`] — per-session derived indices for fast filter / jump / analysis,
//! and the sub-agent liveness truth (design `parser-engine.md` §7).
//!
//! Milestone status: this is the **derived-view-first** cut (§5.2 Phase 5). It is built by
//! one scan over a `Session`'s **top-level** blocks, so positions are flat [`BlockIndex`]es
//! into `Session.blocks`. The tree-addressing `BlockPath` (for descending into a sub-agent's
//! own blocks, §7.3) is a later refinement; nothing here needs it yet. Building it as a
//! derived view keeps it additive and byte-identical — no change to the block model.

use std::collections::{BTreeMap, HashMap};

use crate::model::{AgentStatus, AttachmentKind, Block, BlockIndex, EpochSeconds, UsdCost};

/// A user/human turn boundary. Re-homes today's parallel `user_times` onto the turn.
#[derive(Debug, Clone)]
pub struct TurnEntry {
    /// Zero-based **index into the [`Session`](crate::Session)'s `blocks`** of the `UserText`
    /// or `Command` block that opens this turn — a position in the flat top-level block list,
    /// **not** a byte offset into the transcript nor a line number. It's a valid subscript of
    /// `session.blocks` (`session.blocks[at]`), used to scroll/jump to the turn.
    pub at: BlockIndex,
    /// When the turn was submitted, as a Unix timestamp in **seconds since the epoch** (`f64`
    /// so sub-second precision survives, though transcripts are whole-second in practice).
    /// `None` when the transcript recorded no timestamp for this turn.
    pub time: Option<EpochSeconds>,
}

/// A spawned sub-agent (an `Agent`/`Task` tool call), with its liveness. `status` is the
/// liveness truth ([`active_agents`](SessionIndex::active_agents) filters on it).
#[derive(Debug, Clone)]
pub struct AgentEntry {
    /// The sub-agent's id **as recorded in the transcript** (Claude's `toolUseResult.agentId`);
    /// it also names the child transcript file `agent-<id>.jsonl`. **Empty string** when the
    /// transcript hasn't assigned one yet (e.g. a spawn whose completion event hasn't arrived).
    pub id: String,
    /// The sub-agent's **type label from the spawn** — a free-form string the caller chose
    /// (Claude's `subagent_type`, e.g. `general-purpose`, `code-reviewer`, `Explore`, or a
    /// user-defined agent). This is an **open set**, not a fixed enum, and it is **not** the
    /// [`Agent`](crate::Agent) that produced the transcript. **Empty string** when the spawn
    /// recorded no type.
    pub agent_type: String,
    /// The human task description from the spawn input (Claude's `description`). May be empty.
    pub description: String,
    /// Terminal-or-running state — the liveness signal; see [`AgentStatus`].
    pub status: AgentStatus,
    /// Zero-based **index into the [`Session`](crate::Session)'s `blocks`** of this spawn's
    /// `SubAgent` block — the jump target. A flat top-level position, **not** a byte offset.
    pub at: BlockIndex,
    /// Estimated **cost in US dollars** of this sub-agent *and everything it spawned* (its
    /// subtree), rolled up from token usage — dollars, not tokens, hence `f64` not `u64`.
    /// `None` when the child transcript wasn't loaded or the cost couldn't be derived.
    pub subtree_cost: Option<UsdCost>,
}

/// A tool call (`ToolUse`).
#[derive(Debug, Clone)]
pub struct ToolEntry {
    /// The tool's canonical display name (e.g. `Read`, `Bash`, `Edit`, `Agent`). An **open
    /// set** — includes arbitrary MCP/skill tool names — so it stays a `String`, not an enum.
    pub name: String,
    /// The short human target shown in the header — a repo-relative path, a command, a
    /// description — exactly as rendered. May be empty for a tool with no natural target.
    pub target: String,
    /// Index of the `ToolUse` block; see [`BlockIndex`].
    pub at: BlockIndex,
}

/// A surfaced attachment (`Attachment`).
#[derive(Debug, Clone)]
pub struct AttachmentEntry {
    /// What the attachment is; see [`AttachmentKind`].
    pub kind: AttachmentKind,
    /// Display name — a repo-relative path when known, else the basename.
    pub name: String,
    /// Index of the `Attachment` block; see [`BlockIndex`].
    pub at: BlockIndex,
}

/// A tool name paired with how many times it was called (the auditor primitive, §3.5).
#[derive(Debug, Clone)]
pub struct ToolCount {
    /// The tool's display name (matches the `name` of the [`ToolEntry`]s it counts).
    pub name: String,
    /// Number of `ToolUse` blocks with this name in the session (always ≥ 1).
    pub count: usize,
}

/// Derived, extensible — a new axis (commands, errors, thinking turns) is one more `Vec`.
#[derive(Debug, Clone, Default)]
pub struct SessionIndex {
    pub turns: Vec<TurnEntry>,
    pub agents: Vec<AgentEntry>,
    pub tools: Vec<ToolEntry>,
    pub attachments: Vec<AttachmentEntry>,
    /// How many blocks of each kind, keyed by the canonical `fold_key` classification (the
    /// same one the TUI fold policy and the HTML type/tool filter group by) — e.g.
    /// `{user, assistant, thinking, edit, bash, agent, …}`. `BTreeMap` for a stable order.
    pub counts: BTreeMap<&'static str, usize>,
}

impl SessionIndex {
    /// Build the index in one scan over the top-level blocks. `user_times` supplies each
    /// turn's timestamp in order (exactly the order `stamp_user_turns` emits them). Internal:
    /// a consumer always receives an already-built index via [`Session`](crate::Session).
    pub(crate) fn build(blocks: &[Block], user_times: &[Option<f64>]) -> Self {
        let mut idx = SessionIndex::default();
        let mut turn_i = 0usize;
        for (at, b) in blocks.iter().enumerate() {
            *idx.counts.entry(crate::model::fold_key(b)).or_default() += 1;
            match b {
                Block::UserText(_) | Block::Command { .. } => {
                    let time = user_times.get(turn_i).copied().flatten();
                    idx.turns.push(TurnEntry { at, time });
                    turn_i += 1;
                }
                Block::SubAgent(sa) => idx.agents.push(AgentEntry {
                    id: sa.agent_id.clone(),
                    agent_type: sa.agent_type.clone(),
                    description: sa.description.clone(),
                    status: sa.status,
                    at,
                    subtree_cost: sa.subtree_cost,
                }),
                Block::ToolUse { name, target, .. } => idx.tools.push(ToolEntry {
                    name: name.clone(),
                    target: target.clone(),
                    at,
                }),
                Block::Attachment(a) => idx.attachments.push(AttachmentEntry {
                    kind: a.kind,
                    name: a.name.clone(),
                    at,
                }),
                _ => {}
            }
        }
        idx
    }

    /// The sub-agents that are still running (status not terminal) — the liveness truth the
    /// TUI's `a active N` footer and the HTML "Agents ▾" menu read.
    pub fn active_agents(&self) -> impl Iterator<Item = &AgentEntry> {
        self.agents.iter().filter(|a| !a.status.is_terminal())
    }

    /// Look up a sub-agent by id.
    pub fn agent(&self, id: &str) -> Option<&AgentEntry> {
        self.agents.iter().find(|a| a.id == id)
    }

    /// How many blocks of a given `fold_key` kind (0 if none).
    pub fn count(&self, kind: &str) -> usize {
        self.counts.get(kind).copied().unwrap_or(0)
    }

    /// Tool names by descending call frequency (ties broken by name).
    pub fn tools_by_count(&self) -> Vec<ToolCount> {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for t in &self.tools {
            *counts.entry(t.name.as_str()).or_default() += 1;
        }
        let mut v: Vec<ToolCount> = counts
            .into_iter()
            .map(|(name, count)| ToolCount {
                name: name.to_string(),
                count,
            })
            .collect();
        v.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Attachment, SubAgent};

    fn sub(id: &str, status: AgentStatus) -> Block {
        Block::SubAgent(SubAgent {
            agent_id: id.into(),
            tool_use_id: format!("t_{id}"),
            agent_type: "gp".into(),
            description: format!("do {id}"),
            prompt: "go".into(),
            status,
            result: None,
            output_file: None,
            blocks: Vec::new(),
            subtree_cost: Some(1.0),
        })
    }
    fn tool(name: &str) -> Block {
        Block::ToolUse {
            name: name.into(),
            target: "x".into(),
            diffs: Vec::new(),
            output: None,
            patch: None,
            read_lines: None,
        }
    }

    #[test]
    fn index_captures_turns_agents_tools_attachments() {
        let blocks = vec![
            Block::UserText("hi".into()),
            tool("Read"),
            tool("Bash"),
            tool("Read"),
            sub("a1", AgentStatus::Running),
            sub("a2", AgentStatus::Completed),
            Block::Attachment(Attachment {
                kind: AttachmentKind::Image,
                name: "img.png".into(),
                path: None,
                content: None,
            }),
            Block::UserText("again".into()),
        ];
        let times = vec![Some(10.0), Some(20.0)];
        let idx = SessionIndex::build(&blocks, &times);

        // Turns carry their positions + times, in order.
        assert_eq!(idx.turns.len(), 2);
        assert_eq!(idx.turns[0].at, 0);
        assert_eq!(idx.turns[0].time, Some(10.0));
        assert_eq!(idx.turns[1].at, 7);
        assert_eq!(idx.turns[1].time, Some(20.0));

        // Agents + liveness: a1 running, a2 done → only a1 is active.
        assert_eq!(idx.agents.len(), 2);
        let active: Vec<&str> = idx.active_agents().map(|a| a.id.as_str()).collect();
        assert_eq!(active, vec!["a1"]);
        assert_eq!(idx.agent("a2").unwrap().status, AgentStatus::Completed);

        // Tools by count: Read (2) before Bash (1).
        let counts = idx.tools_by_count();
        assert_eq!(counts[0].name, "Read");
        assert_eq!(counts[0].count, 2);
        assert_eq!(counts[1].name, "Bash");

        // Attachments.
        assert_eq!(idx.attachments.len(), 1);
        assert_eq!(idx.attachments[0].kind, AttachmentKind::Image);
        assert_eq!(idx.attachments[0].at, 6);

        // Block-kind histogram (fold_key-keyed): 2 users, 2 reads + 1 bash, 2 agents, 1 image.
        assert_eq!(idx.count("user"), 2);
        assert_eq!(idx.count("read"), 2);
        assert_eq!(idx.count("bash"), 1);
        assert_eq!(idx.count("agent"), 2);
        assert_eq!(idx.count("attachment"), 1);
        assert_eq!(idx.count("thinking"), 0, "none present");
        // Every block is counted exactly once.
        assert_eq!(idx.counts.values().sum::<usize>(), blocks.len());
    }
}
