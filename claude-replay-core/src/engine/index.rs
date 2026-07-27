//! The [`SessionIndex`] — per-session derived indices for fast filter / jump / analysis,
//! and the sub-agent liveness truth (design `parser-engine.md` §7).
//!
//! Milestone status: this is the **derived-view-first** cut (§5.2 Phase 5). It is built by
//! one scan over a `Session`'s **top-level** blocks, so positions are flat `usize` indices
//! into `Session.blocks`. The tree-addressing `BlockPath` (for descending into a sub-agent's
//! own blocks, §7.3) is a later refinement; nothing here needs it yet. Building it as a
//! derived view keeps it additive and byte-identical — no change to the block model.

use std::collections::{BTreeMap, HashMap};

use crate::model::{AgentStatus, Block};

/// A user/human turn boundary. Re-homes today's parallel `user_times` onto the turn.
#[derive(Debug, Clone)]
pub struct TurnEntry {
    /// Position of the `UserText` / `Command` block that opens the turn.
    pub at: usize,
    /// Wall-clock of the event that produced it (epoch seconds), when recorded.
    pub time: Option<f64>,
}

/// A spawned sub-agent. `status` is the liveness truth (`active_agents` filters on it).
#[derive(Debug, Clone)]
pub struct AgentEntry {
    pub id: String,
    pub agent_type: String,
    pub description: String,
    pub status: AgentStatus,
    /// Position of the spawn (`SubAgent`) block — the jump target.
    pub at: usize,
    pub subtree_cost: Option<f64>,
}

/// A tool call (`ToolUse`) — its display name + target, at its block position.
#[derive(Debug, Clone)]
pub struct ToolEntry {
    pub name: String,
    pub target: String,
    pub at: usize,
}

/// A surfaced attachment (`Attachment`).
#[derive(Debug, Clone)]
pub struct AttachmentEntry {
    pub kind: &'static str,
    pub name: String,
    pub at: usize,
}

/// A tool name paired with how many times it was called (the auditor primitive, §3.5).
#[derive(Debug, Clone)]
pub struct ToolCount {
    pub name: String,
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
                kind: "image",
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
        assert_eq!(idx.attachments[0].kind, "image");
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
