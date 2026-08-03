//! **The meta record stream** (#96) — the frontend-agnostic companion to the block stream.
//!
//! The replayer emits `Block`s; alongside them it emits **meta records** carrying the
//! *delta* changes those blocks make to the agent-neutral session state. Persisted together
//! the two streams let a later invocation resume without re-parsing the transcript.
//!
//! # The emission principle
//!
//! For each batch of regular (block) records the replayer emits, it emits **0–2** meta
//! records such that:
//!
//! - **(A)** every meta delta caused by those blocks is captured in a meta record; and
//! - **(B)** if the batch contained a **committed** block, a meta record lands
//!   **immediately after the last committed one**, capturing all deltas since the previous
//!   meta record.
//!
//! | batch | meta records |
//! |---|---|
//! | no commit, no meta change | **0** |
//! | no commit, meta change | **1** — at the end, *unanchored* |
//! | commit is the last record | **1** — right after it, **anchored** |
//! | commit mid-batch, no change after it | **1** — right after the last commit, **anchored** |
//! | commit mid-batch, changes after it | **2** — anchored, then unanchored at the end |
//!
//! # Why rule (B) is the load-bearing one
//!
//! A record placed immediately after a committed block describes state **as of that
//! committed block** — nothing from the still-open turn has leaked into it. That is exactly
//! the committed-only state a resume needs, which is why the anchor
//! ([`MetaRecord::committed_id`]) lives on **anchored records only**. A resume additionally
//! needs byte offsets, but those are the *cache's* concern: the accumulator owns reader
//! positions and adds them when the persistent cache is built. The replayer supplies only
//! the anchor.
//!
//! **Unanchored records are never resumed from.** They describe provisional-turn
//! contributions, which a resume rebuilds by re-reading the open turn — applying them would
//! double-count. They exist so a *live* reader's metadata stays current between commits, and
//! so the stream can be replayed to "now" without the transcript.
//!
//! On load: replay records up to and including the anchored record for the chosen committed
//! id, then **truncate the stream there** — leaving a trailing unanchored record would let a
//! later load apply it ahead of records computed relative to that point.

use crate::model::{AgentId, Block};
use serde::{Deserialize, Serialize};

/// One change to a child (sub-agent) entry, by **ordinal** rather than by id: duplicate ids
/// are deliberately preserved by `SessionMeta`, so an id-keyed upsert would collapse them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ChildOp {
    /// Append a child entry (id, description, agent type).
    Add(AgentId, String, String),
    /// Mark the children at these ordinals finished.
    Done(Vec<usize>),
}

/// The delta a batch of blocks makes to the agent-neutral session state.
///
/// Every field is `Option`/empty when unchanged — per requirement 7, a record carries only
/// what changed, never a snapshot. [`MetaDelta::is_empty`] is what rule (A) tests.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MetaDelta {
    /// Added user turns (absent when zero).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turns: Option<usize>,
    /// Added tool calls (absent when zero).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<usize>,
    /// Ordinal child operations, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<ChildOp>,
    /// Spawn-identity upserts: key → (agent_id, agent_type). Idempotent, so a replay may
    /// re-apply them safely.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agent_ids: Vec<(String, String, String)>,
}

impl MetaDelta {
    /// Nothing changed — rule (A) emits no record for such a batch.
    pub fn is_empty(&self) -> bool {
        self.turns.is_none()
            && self.tools.is_none()
            && self.children.is_empty()
            && self.agent_ids.is_empty()
    }

    /// Fold one emitted block's contribution into this delta. Mirrors what
    /// `SessionMeta::push` and `record_agent` do, so the two can be cross-checked
    /// (the equivalence oracle).
    pub fn push(&mut self, b: &Block) {
        match b {
            Block::UserText(_) | Block::Command { .. } => {
                *self.turns.get_or_insert(0) += 1;
            }
            Block::ToolUse { .. } => {
                *self.tools.get_or_insert(0) += 1;
            }
            Block::SubAgent(sa) => {
                if !sa.agent_id.is_empty() {
                    self.children.push(ChildOp::Add(
                        sa.agent_id.clone(),
                        sa.description.clone(),
                        sa.agent_type.clone(),
                    ));
                }
                let v = (sa.agent_id.clone(), sa.agent_type.clone());
                if !sa.tool_use_id.is_empty() {
                    self.agent_ids
                        .push((sa.tool_use_id.clone(), v.0.clone(), v.1.clone()));
                }
                if !sa.agent_id.is_empty() {
                    self.agent_ids.push((sa.agent_id.clone(), v.0, v.1));
                }
            }
            _ => {}
        }
    }

    /// Merge `other` (a later delta) into this one — used when two batches coalesce into
    /// one record.
    pub fn merge(&mut self, other: MetaDelta) {
        if let Some(t) = other.turns {
            *self.turns.get_or_insert(0) += t;
        }
        if let Some(t) = other.tools {
            *self.tools.get_or_insert(0) += t;
        }
        self.children.extend(other.children);
        self.agent_ids.extend(other.agent_ids);
    }
}

/// One record in the meta stream. `stamp` is `Some` **iff** the record is anchored — placed
/// immediately after a committed block — and only anchored records are resume points.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MetaRecord {
    /// `Some(n)` **iff** anchored — placed immediately after the committed block that
    /// brought the committed count to `n`. Only anchored records are resume points.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub committed_id: Option<usize>,
    pub delta: MetaDelta,
}

impl MetaRecord {
    pub fn anchored(committed_id: usize, delta: MetaDelta) -> Self {
        Self {
            committed_id: Some(committed_id),
            delta,
        }
    }
    pub fn unanchored(delta: MetaDelta) -> Self {
        Self {
            committed_id: None,
            delta,
        }
    }
    /// Anchored records are the only resume points (see the module docs).
    pub fn is_resume_point(&self) -> bool {
        self.committed_id.is_some()
    }
}

/// **The emission protocol**, as a pure function of one batch.
///
/// `committed` is the delta caused by blocks that committed in this batch (empty if the
/// batch committed nothing); `provisional` is the delta caused by blocks emitted *after* the
/// last committed one. `committed_id` is the committed-block count after this batch.
///
/// This is the whole of rules (A) and (B) — see the module docs for the five cases. It is a
/// free function so the protocol can be exercised directly, independent of whatever call
/// pattern the fold happens to have today.
pub fn emit_batch(
    committed_id: usize,
    committed_blocks: usize,
    committed: MetaDelta,
    provisional: MetaDelta,
) -> Vec<MetaRecord> {
    let mut out = Vec::new();
    if committed_blocks > 0 {
        // Rule (B): a record lands immediately after the last committed block — even when
        // its delta is empty, because it is the resume anchor, not merely a change report.
        out.push(MetaRecord::anchored(committed_id, committed));
    } else if !committed.is_empty() {
        // No commit, but the batch changed metadata: rule (A) still requires capture.
        out.push(MetaRecord::unanchored(committed));
    }
    if !provisional.is_empty() {
        out.push(MetaRecord::unanchored(provisional));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(turns: usize) -> MetaDelta {
        MetaDelta {
            turns: Some(turns),
            ..Default::default()
        }
    }

    /// The five cases of the emission protocol, exactly as specified.
    #[test]
    fn emission_protocol_five_cases() {
        // 1. no commit, no meta change -> zero records
        assert!(emit_batch(0, 0, MetaDelta::default(), MetaDelta::default()).is_empty());

        // 2. no commit, meta change -> one UNANCHORED record
        let r = emit_batch(0, 0, d(1), MetaDelta::default());
        assert_eq!(r.len(), 1);
        assert!(!r[0].is_resume_point(), "no commit ⇒ not a resume point");
        assert_eq!(r[0].delta, d(1));

        // 3. the commit is the last record -> one ANCHORED record after it
        let r = emit_batch(7, 2, d(1), MetaDelta::default());
        assert_eq!(r.len(), 1);
        assert!(r[0].is_resume_point());
        assert_eq!(r[0].committed_id, Some(7));

        // 4. commit mid-batch, no change after it -> still exactly one anchored record
        let r = emit_batch(7, 2, MetaDelta::default(), MetaDelta::default());
        assert_eq!(
            r.len(),
            1,
            "rule (B) emits the anchor even with an empty delta"
        );
        assert!(r[0].is_resume_point());
        assert!(r[0].delta.is_empty());

        // 5. commit mid-batch, further changes after it -> two records, anchored first
        let r = emit_batch(7, 2, d(1), d(3));
        assert_eq!(r.len(), 2);
        assert!(r[0].is_resume_point(), "the anchor comes first");
        assert_eq!(r[0].committed_id, Some(7));
        assert!(
            !r[1].is_resume_point(),
            "post-commit changes are never a resume point: a resume re-reads the open turn \
             and would double-count them"
        );
        assert_eq!(r[1].delta, d(3));
    }

    /// Rule (A): a delta is never silently dropped — every non-empty input reaches a record.
    #[test]
    fn no_delta_is_ever_dropped() {
        for (cb, c, p) in [
            (0usize, d(1), MetaDelta::default()),
            (0, MetaDelta::default(), d(2)),
            (2, d(1), d(2)),
            (2, MetaDelta::default(), d(2)),
        ] {
            let want: usize = c.turns.unwrap_or(0) + p.turns.unwrap_or(0);
            let got: usize = emit_batch(9, cb, c, p)
                .iter()
                .map(|r| r.delta.turns.unwrap_or(0))
                .sum();
            assert_eq!(got, want, "every delta must land in some record");
        }
    }

    /// A block's contribution is folded once and only once.
    #[test]
    fn delta_push_matches_block_kinds() {
        let mut m = MetaDelta::default();
        m.push(&Block::UserText("hi".into()));
        m.push(&Block::ToolUse {
            name: "Bash".into(),
            target: "ls".into(),
            diffs: vec![],
            output: None,
            patch: None,
            read_lines: None,
        });
        m.push(&Block::AssistantText("ignored".into()));
        assert_eq!(m.turns, Some(1));
        assert_eq!(m.tools, Some(1));
        assert!(m.children.is_empty());
    }
}
