//! The [`SessionIndex`] — per-session derived indices for fast filter / jump / analysis
//! (design `parser-engine.md` §7). The sub-agent entity map (spawn liveness + lifecycle
//! pointers) now lives on [`Session.sub_agents`](crate::Session), keyed by agent id.
//!
//! Milestone status: this is the **derived-view-first** cut (§5.2 Phase 5). It is built by
//! one scan over a `Session`'s **top-level** blocks, so positions are flat [`BlockIndex`]es
//! into `Session.blocks`. The tree-addressing `BlockPath` (for descending into a sub-agent's
//! own blocks, §7.3) is a later refinement; nothing here needs it yet. Building it as a
//! derived view keeps it additive and byte-identical — no change to the block model.

use std::collections::{BTreeMap, HashMap};

use crate::model::{AttachmentKind, Block, BlockIndex, EpochSeconds};

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
            // Advance the user-turn cursor exactly as the incremental caller would: a user turn
            // consumes the next `user_times` entry, everything else passes `None`.
            let turn_time = if matches!(b, Block::UserText(_) | Block::Command { .. }) {
                let t = user_times.get(turn_i).copied().flatten();
                turn_i += 1;
                t
            } else {
                None
            };
            idx.push(at, b, turn_time);
        }
        idx
    }

    /// Fold ONE more block (at its flat [`BlockIndex`] `at`) into the index — the incremental
    /// unit [`build`](Self::build) is a loop over. `turn_time` is this block's timestamp **iff** it
    /// is a user turn (`UserText`/`Command`), in the same order [`stamp_user_turns`] emits them;
    /// pass `None` for any other block. Lets the accumulator maintain the index as durable blocks
    /// are emitted, so the full `Vec<Block>` need never be resident to (re)build it — the emit-and-
    /// drop / tier-b path. Proven equal to `build` block-for-block (see the test).
    pub fn push(&mut self, at: BlockIndex, b: &Block, turn_time: Option<EpochSeconds>) {
        *self.counts.entry(crate::model::fold_key(b)).or_default() += 1;
        match b {
            Block::UserText(_) | Block::Command { .. } => self.turns.push(TurnEntry {
                at,
                time: turn_time,
            }),
            Block::ToolUse { name, target, .. } => self.tools.push(ToolEntry {
                name: name.clone(),
                target: target.clone(),
                at,
            }),
            Block::Attachment(a) => self.attachments.push(AttachmentEntry {
                kind: a.kind,
                name: a.name.clone(),
                at,
            }),
            _ => {}
        }
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
    use crate::model::{AgentStatus, Attachment, SubAgent};

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
                content: crate::model::AttachmentContent::None,
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

    // The incremental `push` fold must reproduce the batch `build` exactly — the property the
    // emit-and-drop / tier-b accumulator relies on to maintain the index without the full blocks
    // resident. Compared via Debug (the index's inner entries carry no PartialEq).
    #[test]
    fn incremental_push_equals_batch_build() {
        use crate::model::{AttachmentContent, AttachmentKind};
        let blocks = vec![
            Block::UserText("hi".into()),
            tool("Read"),
            Block::AssistantText("ok".into()),
            Block::Command {
                name: "/compact".into(),
                args: "".into(),
                output: vec!["done".into()],
            },
            tool("Bash"),
            sub("a1", AgentStatus::Running),
            Block::Attachment(Attachment {
                kind: AttachmentKind::Plan,
                name: "plan.md".into(),
                path: None,
                content: AttachmentContent::None,
            }),
            Block::UserText("again".into()),
        ];
        // Three user turns (two UserText + one Command), in emit order.
        let user_times = vec![Some(10.0), Some(20.0), Some(30.0)];

        let batch = SessionIndex::build(&blocks, &user_times);

        let mut incr = SessionIndex::default();
        let mut turn_i = 0usize;
        for (at, b) in blocks.iter().enumerate() {
            let tt = if matches!(b, Block::UserText(_) | Block::Command { .. }) {
                let t = user_times.get(turn_i).copied().flatten();
                turn_i += 1;
                t
            } else {
                None
            };
            incr.push(at, b, tt);
        }

        assert_eq!(
            format!("{batch:?}"),
            format!("{incr:?}"),
            "incremental push must equal batch build"
        );
    }
}
