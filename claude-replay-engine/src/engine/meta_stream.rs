//! **The meta record stream** (#96) — the frontend-agnostic companion to the block stream.
//!
//! The replayer emits `Block`s; alongside them it emits **meta records** carrying the changes
//! those blocks make to the agent-neutral session state. Persisted together the two streams let
//! a later invocation resume without re-parsing the transcript.
//!
//! # The emission principle
//!
//! For each batch of regular (block) records the replayer emits, it emits **0–2** meta records
//! such that:
//!
//! - **(A)** every meta change caused by those blocks is captured in a meta record; and
//! - **(B)** if the batch contained a **committed** block, a meta record lands **immediately
//!   after the last committed one**, capturing all committed changes since the previous such
//!   record.
//!
//! | batch | meta records |
//! |---|---|
//! | no commit, no meta change | **0** |
//! | no commit, meta change | **1** — at the end, *unanchored* |
//! | commit is the last record | **1** — right after it, **anchored** |
//! | commit mid-batch, no change after it | **1** — right after the last commit, **anchored** |
//! | commit mid-batch, changes after it | **2** — anchored, then unanchored at the end |
//!
//! # The two record kinds are combined DIFFERENTLY
//!
//! This is the load-bearing distinction, and it is forced by how the accumulator maintains the
//! same value: `SessionAccumulator::session_meta` accumulates committed blocks into
//! `committed_meta` once each, then folds the open turn **freshly on top** each time it is asked.
//! The stream mirrors exactly that:
//!
//! - **Anchored** records (`committed_id.is_some()`) carry an *incremental* delta of blocks that
//!   **committed**. They **accumulate** — apply every one, in order, exactly once.
//! - **Unanchored** records carry a *full restatement* of the still-open turn's contribution,
//!   relative to the most recent anchor. They **supersede** — only the latest one after the last
//!   anchored record counts, and an anchored record voids any unanchored record before it.
//!
//! So the reconstruction is:
//!
//! ```text
//! committed_meta = Σ (anchored deltas, in order)          // accumulate
//! live_meta      = committed_meta + latest unanchored      // supersede
//! ```
//!
//! Treating an unanchored record as incremental would **double-count**: a block restated while
//! provisional is counted again by the anchored record that later commits it.
//!
//! # Resume
//!
//! Only **anchored** records are resume points. A record placed immediately after a committed
//! block describes state *as of that committed block*, with nothing from the still-open turn
//! leaked in — exactly the committed-only state a resume needs. Unanchored records describe an
//! open turn that a resume rebuilds by re-reading the transcript, so applying them would
//! double-count. They exist so a *live* reader stays current between commits.
//!
//! A resume additionally needs byte offsets, but those are the *cache's* concern: the accumulator
//! owns reader positions and adds them when the persistent cache is built. The replayer supplies
//! only the anchor.
//!
//! On load: replay records up to and including the anchored record for the chosen committed id,
//! then **truncate the stream there** — leaving a trailing unanchored record would let a later
//! load apply it ahead of records computed relative to that point.

use crate::engine::session::{ChildMeta, SessionMeta};
use crate::model::{AgentId, Block};
use serde::{Deserialize, Serialize};

/// The `(key, agent_id, agent_type)` identity rows a block contributes to the replayer's
/// spawn-identity map.
///
/// The **single** definition of that mapping: `Replayer::record_agent` inserts these rows into its
/// live `HashMap`, and [`MetaDelta::push`] records the same rows for the stream. Two hand-mirrored
/// copies would drift (see [`MetaDelta::push`]); one shared list cannot.
///
/// A spawn is reachable by **two** keys because the id is discovered late: the `tool_use_id` is
/// known when the spawn folds, while the real `agent_id` only arrives with the tool result, and a
/// later `AgentDone` may name either.
pub fn agent_pairs(b: &Block) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    if let Block::SubAgent(sa) = b {
        if !sa.tool_use_id.is_empty() {
            out.push((
                sa.tool_use_id.clone(),
                sa.agent_id.clone(),
                sa.agent_type.clone(),
            ));
        }
        if !sa.agent_id.is_empty() {
            out.push((
                sa.agent_id.clone(),
                sa.agent_id.clone(),
                sa.agent_type.clone(),
            ));
        }
    }
    out
}

/// One change to the child (sub-agent) list.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ChildOp {
    /// Append a child. **Positional**: `SessionMeta` deliberately keeps duplicate ids (a block
    /// walk's order and multiplicity), so this appends and never upserts by id.
    Add(ChildMeta),
    /// Mark finished every child with this id — **by id, not by ordinal**. An `AgentDone` in one
    /// batch can match a child added many batches earlier, and a delta has no view of the
    /// accumulated list, so an ordinal is not computable here. Clearing *every* match reproduces
    /// [`SessionMeta::push`]'s linear scan, duplicates included.
    Done(AgentId),
}

/// What a batch of blocks changes in the agent-neutral session state.
///
/// Carries only what changed — never a snapshot (requirement 7): `turns`/`tools` are absent when
/// zero and `children`/`agent_ids` are empty when untouched. [`MetaDelta::is_empty`] is what rule
/// (A) tests.
///
/// Read as an *increment* on an anchored record and as a *full restatement of the open turn* on an
/// unanchored one — see the module docs.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MetaDelta {
    /// Added user turns (absent when zero).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turns: Option<usize>,
    /// Added tool calls (absent when zero).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<usize>,
    /// Child list operations, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<ChildOp>,
    /// Spawn-identity rows — `(key, agent_id, agent_type)`, see [`agent_pairs`]. Upserts, so a
    /// replay may re-apply them safely. Not part of [`SessionMeta`]: this is replayer state the
    /// stream carries so a resumed fold can still resolve a later `AgentDone`.
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

    /// Fold one block's contribution in.
    ///
    /// **Must stay arm-for-arm with [`SessionMeta::push`]** — that equality is the whole contract,
    /// and `MetaDelta::apply` ∘ `push` reproducing `SessionMeta::push` is asserted by the oracle
    /// tests. (An earlier revision hand-mirrored it and silently dropped `Thinking{tools}`, which
    /// is why the oracle exists rather than a code comment promising the two agree.)
    pub fn push(&mut self, b: &Block) {
        match b {
            Block::UserText(_) | Block::Command { .. } => *self.turns.get_or_insert(0) += 1,
            Block::ToolUse { .. } => *self.tools.get_or_insert(0) += 1,
            // An activity-coalesced run is ONE Thinking block carrying its nested tool calls.
            Block::Thinking { tools, .. } if !tools.is_empty() => {
                *self.tools.get_or_insert(0) += tools.len()
            }
            Block::SubAgent(sa) if !sa.agent_id.is_empty() => {
                self.children.push(ChildOp::Add(ChildMeta {
                    id: sa.agent_id.clone(),
                    description: sa.description.clone(),
                    agent_type: sa.agent_type.clone(),
                    running: !sa.status.is_terminal(),
                }))
            }
            Block::AgentDone { agent_id, .. } if !agent_id.is_empty() => {
                self.children.push(ChildOp::Done(agent_id.clone()))
            }
            _ => {}
        }
        // Identity rows are recorded for EVERY spawn, including one whose `agent_id` has not
        // arrived yet (keyed by `tool_use_id` alone) — the arm above skips those for `children`,
        // but the map still needs the row.
        self.agent_ids.extend(agent_pairs(b));
    }

    /// Apply this delta to a running [`SessionMeta`] — the inverse of [`push`](Self::push), and
    /// the operation a load performs per record. Mirrors `SessionMeta::push`'s effects exactly.
    pub fn apply(&self, m: &mut SessionMeta) {
        m.turns += self.turns.unwrap_or(0);
        m.tools += self.tools.unwrap_or(0);
        for op in &self.children {
            match op {
                ChildOp::Add(c) => m.children.push(c.clone()),
                ChildOp::Done(id) => {
                    for c in m.children.iter_mut().filter(|c| &c.id == id) {
                        c.running = false;
                    }
                }
            }
        }
    }

    /// Merge `other` (a later delta) into this one — used when two batches coalesce into one
    /// record. Valid for *accumulating* (anchored) deltas only; unanchored records supersede
    /// rather than merge.
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

/// One record in the meta stream. Anchored **iff** `committed_id` is set; that also selects how
/// the record combines (accumulate vs supersede) — see the module docs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MetaRecord {
    /// `Some(n)` **iff** anchored — placed immediately after the committed block that brought the
    /// committed count to `n`. Only anchored records are resume points.
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

/// The deltas a load must apply, in order — **the** definition of accumulate-vs-supersede, so
/// every reader shares one implementation of the subtle part.
///
/// `through` bounds the replay to a resume point: `Some(n)` stops after the anchored record for
/// committed id `n` (committed-only state, no open turn), `None` replays everything (live state).
/// An unknown `n` yields nothing rather than a partial answer.
fn effective(records: &[MetaRecord], through: Option<usize>) -> Vec<&MetaDelta> {
    let end = match through {
        Some(n) => match records.iter().position(|r| r.committed_id == Some(n)) {
            Some(i) => i + 1,
            None => return Vec::new(),
        },
        None => records.len(),
    };
    let mut out: Vec<&MetaDelta> = Vec::new();
    let mut pending: Option<&MetaDelta> = None;
    for r in &records[..end] {
        if r.is_resume_point() {
            // An anchor accumulates, and voids any provisional restatement before it.
            out.push(&r.delta);
            pending = None;
        } else {
            // Supersede: only the latest restatement survives.
            pending = Some(&r.delta);
        }
    }
    out.extend(pending);
    out
}

/// Replay a stream to the [`SessionMeta`] it describes — the reader half of the protocol, and the
/// left-hand side of the oracle tests. See [`effective`] for `through`.
pub fn replay_meta(records: &[MetaRecord], through: Option<usize>) -> SessionMeta {
    let mut m = SessionMeta::default();
    for d in effective(records, through) {
        d.apply(&mut m);
    }
    m
}

/// Replay a stream to the spawn-identity map it describes (`key → (agent_id, agent_type)`, see
/// [`agent_pairs`]) — the replayer state a resumed fold needs to resolve a later `AgentDone`
/// whose spawn is long since committed and dropped. Rebuilding it is why the rows are in the
/// stream at all; without it a resumed session renders completions with no type or id.
pub fn replay_agent_ids(
    records: &[MetaRecord],
    through: Option<usize>,
) -> std::collections::HashMap<String, (String, String)> {
    let mut map = std::collections::HashMap::new();
    for d in effective(records, through) {
        for (key, agent_id, agent_type) in &d.agent_ids {
            map.insert(key.clone(), (agent_id.clone(), agent_type.clone()));
        }
    }
    map
}

/// **The emission protocol**, as a pure function of one batch — the ONE implementation, called by
/// both of the replayer's emission points (the drain, which commits, and `drain_meta`, which
/// restates the open turn).
///
/// `committed` is the incremental delta of blocks that committed in this batch, and
/// `committed_blocks` how many did; `provisional` is the **full restatement** of the open turn's
/// contribution afterwards. `committed_id` is the committed-block count after this batch (ignored
/// when nothing committed).
///
/// This is the whole of rules (A) and (B) — see the module docs for the five cases. Being a free
/// function, the protocol can be exercised directly, independent of the fold's call pattern.
pub fn emit_batch(
    committed_id: usize,
    committed_blocks: usize,
    committed: MetaDelta,
    provisional: MetaDelta,
) -> Vec<MetaRecord> {
    debug_assert!(
        committed_blocks > 0 || committed.is_empty(),
        "a batch that committed no blocks cannot have a committed delta"
    );
    let mut out = Vec::new();
    if committed_blocks > 0 {
        // Rule (B): a record lands immediately after the last committed block — even when its
        // delta is empty, because it is the resume anchor, not merely a change report.
        out.push(MetaRecord::anchored(committed_id, committed));
    }
    // Rule (A): whatever the open turn now contributes is stated in full, superseding the last
    // restatement. This is the only source of unanchored records.
    if !provisional.is_empty() {
        out.push(MetaRecord::unanchored(provisional));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AgentStatus, SubAgent};

    fn d(turns: usize) -> MetaDelta {
        MetaDelta {
            turns: Some(turns),
            ..Default::default()
        }
    }

    fn tool(name: &str) -> Block {
        Block::ToolUse {
            name: name.into(),
            target: String::new(),
            diffs: vec![],
            output: None,
            patch: None,
            read_lines: None,
        }
    }

    fn spawn(id: &str, tool_use_id: &str, status: AgentStatus) -> Block {
        Block::SubAgent(SubAgent {
            agent_id: id.into(),
            tool_use_id: tool_use_id.into(),
            agent_type: "general-purpose".into(),
            description: "child".into(),
            prompt: String::new(),
            status,
            result: None,
            output_file: None,
            blocks: Vec::new(),
            subtree_cost: None,
        })
    }

    /// The five cases of the emission protocol, exactly as specified.
    #[test]
    fn emission_protocol_five_cases() {
        // 1. no commit, no meta change -> zero records
        assert!(emit_batch(0, 0, MetaDelta::default(), MetaDelta::default()).is_empty());

        // 2. no commit, meta change -> one UNANCHORED record. With nothing committed the change
        // can only be the open turn's, so it arrives as the restatement.
        let r = emit_batch(0, 0, MetaDelta::default(), d(1));
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
            "the open-turn restatement is never a resume point: a resume re-reads the open turn \
             and would double-count it"
        );
        assert_eq!(r[1].delta, d(3));
    }

    /// Rule (A): a change is never silently dropped — every non-empty input reaches a record.
    #[test]
    fn no_delta_is_ever_dropped() {
        for (cb, c, p) in [
            (0usize, MetaDelta::default(), d(1)),
            (0, MetaDelta::default(), d(2)),
            (2, d(1), d(2)),
            (2, MetaDelta::default(), d(2)),
            (2, d(1), MetaDelta::default()),
        ] {
            let want: usize = c.turns.unwrap_or(0) + p.turns.unwrap_or(0);
            let got: usize = emit_batch(9, cb, c, p)
                .iter()
                .map(|r| r.delta.turns.unwrap_or(0))
                .sum();
            assert_eq!(got, want, "every change must land in some record");
        }
    }

    /// `MetaDelta::push` + `apply` must reproduce `SessionMeta::push` block for block — the
    /// property the whole stream rests on, checked over every arm that carries meaning
    /// (including `Thinking{tools}`, which a hand-mirrored earlier revision dropped).
    #[test]
    fn push_then_apply_equals_session_meta_push() {
        let blocks = vec![
            Block::UserText("one".into()),
            tool("Bash"),
            // An activity-coalesced run: three nested tool calls in ONE block.
            Block::Thinking {
                text: "hm".into(),
                duration_secs: None,
                tools: vec![tool("Read"), tool("Grep"), tool("Edit")],
            },
            spawn("a1", "toolu_1", AgentStatus::Running),
            spawn("a2", "toolu_2", AgentStatus::Running),
            // A duplicate id: SessionMeta keeps both, so the delta must too.
            spawn("a1", "toolu_3", AgentStatus::Running),
            Block::AgentDone {
                agent_id: "a1".into(),
                agent_type: "general-purpose".into(),
                description: "child".into(),
                status: AgentStatus::Completed,
                result: None,
            },
            Block::AssistantText("ignored".into()),
        ];
        let want = SessionMeta::build(&blocks);
        let mut delta = MetaDelta::default();
        for b in &blocks {
            delta.push(b);
        }
        let mut got = SessionMeta::default();
        delta.apply(&mut got);
        assert_eq!(got, want, "delta fold must equal the SessionMeta fold");
        assert_eq!(want.tools, 4, "Bash + three coalesced nested calls");
        assert_eq!(want.children.len(), 3, "duplicate ids are kept");
        // `Done("a1")` clears BOTH children carrying that id, as SessionMeta's linear scan does.
        assert!(!got.children[0].running && !got.children[2].running);
        assert!(got.children[1].running, "a2 is untouched");
    }

    /// The reader half: anchored records ACCUMULATE, unanchored ones SUPERSEDE. Getting this
    /// backwards double-counts a block that was restated while provisional and then committed.
    #[test]
    fn anchored_accumulate_and_unanchored_supersede() {
        let recs = vec![
            MetaRecord::anchored(1, d(1)),
            MetaRecord::unanchored(d(1)),  // open turn: one turn so far
            MetaRecord::unanchored(d(1)),  // restated, NOT a second turn
            MetaRecord::anchored(2, d(1)), // that turn commits; the restatement is voided
            MetaRecord::unanchored(d(1)),  // a new open turn
        ];
        assert_eq!(
            replay_meta(&recs, None).turns,
            3,
            "1 + 1 committed + 1 open"
        );
        // Truncating at a resume point yields committed-only state — no open turn.
        assert_eq!(replay_meta(&recs, Some(2)).turns, 2);
        assert_eq!(replay_meta(&recs, Some(1)).turns, 1);
    }

    /// Identity rows come from ONE definition, so the replayer's map and the stream cannot drift.
    /// A spawn is reachable by both keys, and one without an `agent_id` yet still records a row.
    #[test]
    fn agent_pairs_covers_both_keys() {
        let p = agent_pairs(&spawn("a1", "toolu_1", AgentStatus::Running));
        assert_eq!(p.len(), 2, "reachable by tool_use_id AND agent_id");
        assert!(p.iter().any(|(k, ..)| k == "toolu_1"));
        assert!(p.iter().any(|(k, ..)| k == "a1"));

        // id not yet known: still keyed by tool_use_id, and contributes NO child.
        let pending = spawn("", "toolu_9", AgentStatus::Running);
        assert_eq!(agent_pairs(&pending).len(), 1);
        let mut delta = MetaDelta::default();
        delta.push(&pending);
        assert!(delta.children.is_empty(), "no id ⇒ not a child yet");
        assert_eq!(delta.agent_ids.len(), 1, "but the identity row is recorded");
    }
}
