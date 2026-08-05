//! **The meta stream** (#96) — the frontend-agnostic companion to the content stream.
//!
//! The content stream holds committed blocks; this one holds everything else a session knows
//! about itself, plus what a later invocation needs to resume folding. See
//! `design/durable-session-cache.md`; the vocabulary here mirrors its §4 exactly.
//!
//! A record is written **once per committing drain** and has three parts:
//!
//! - a **delta** — session facts, present on every record. Two classes: *counters* fold
//!   (scalars add, lists append, maps sum per key) and *gauges* replace.
//! - a **resume** payload — present iff this drain is a resume point (the §3 partition exists).
//!   Its presence *is* the indicator; there is no separate flag.
//! - a **checkpoint** — an absolute [`MaterializedMeta`], written periodically. Present only
//!   where `resume` is: a checkpoint a reader cannot resume from would let compaction leave a
//!   cache with state and no resume point.
//!
//! [`MaterializedMeta`] is the stream's **materialized view**: the stream is the deltas, this is
//! their sum. A checkpoint is simply one of these written down, which is why adopting one and
//! folding from the start must agree.

use crate::engine::session::{ChildMeta, SessionMeta};
use crate::engine::tasks::{TaskFold, TaskOp};
use crate::metrics::TokenCounts;
use crate::model::{AgentId, AgentStatus, Block, ByteOffset, EpochSeconds};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// A model name — the key tokens are attributed to (#104).
pub type Model = String;

/// The format this build writes. Bump when the record schema changes incompatibly; a new
/// *field* needs no bump (it arrives with `#[serde(default)]` and older records still load).
pub const FORMAT_VERSION: u16 = 1;

/// How many resumable drains between checkpoints. A checkpoint costs O(turns + tasks + spawns)
/// to write, so this amortises it; it also bounds a reader's work, which is otherwise O(records)
/// and grows without limit for a long-lived session.
///
/// The value is a **policy** knob, not a correctness one — every value produces a valid stream.
pub const CHECKPOINT_EVERY: usize = 64;

/// How many records must precede a checkpoint before compaction bothers rewriting the stream.
/// Also policy.
pub const COMPACT_AFTER: usize = 256;

/// The fold's version. **Bump this whenever block output changes** — otherwise a resume splices
/// blocks built by the old fold onto blocks built by the new one, and the seam is invisible.
///
/// The byte-identical gate is what catches a change that needs a bump: an intentional output
/// change re-baselines the gate, and that is the moment to bump here too.
pub const FOLD_VERSION: u16 = 2;

impl Versions {
    /// This build's versions for a presentation whose output has no render parameters (the TUI).
    /// A presentation that has them passes its fingerprint as `flavor`.
    pub fn current(flavor: Option<u64>) -> Self {
        Self {
            format: FORMAT_VERSION,
            fold: FOLD_VERSION,
            flavor,
        }
    }
}

// ── the stream ────────────────────────────────────────────────────────────────────────────

/// Record 0 of the meta stream, written once.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamHeader {
    /// CRC32 of the transcript's **first line** — identity, so a stream is never matched
    /// against a different file that happens to sit at the same path.
    pub anchor: u32,
    pub versions: Versions,
}

/// What must match for a cached stream to be reusable at all.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Versions {
    pub format: u16,
    /// Fold-logic version: bump when block output changes, or a resume would splice blocks
    /// built by two different folds.
    pub fold: u16,
    /// HTML only: the render fingerprint (`FoldPolicy` + cwd + record schema). `None` for a
    /// presentation whose output has no such parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flavor: Option<u64>,
}

// ── the record ────────────────────────────────────────────────────────────────────────────

/// One committing drain's contribution. Every field is optional: **absent means no update**,
/// and the field's class fixes what an update *is*.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MetaRecord {
    // ── delta, counters: fold across records ──
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<u32>,
    /// Sub-agent lifecycle, **in order** — see [`AgentEvent`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<AgentEvent>,
    /// Timestamps of the turns *this* drain stamped — normally one entry, not the running
    /// vector (that would be O(turns²) over a session).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub user_times: Vec<Option<EpochSeconds>>,
    /// Per-model token increments (#104). Summed per key on fold.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tokens: BTreeMap<Model, TokenCounts>,
    /// Agent-specific **counters**; a repeated key ADDS. A gauge must never live here —
    /// summing it would be wrong live as well as cached.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, u64>,
    /// The task op-log — never the task list, which is a full snapshot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub task_ops: Vec<TaskOp>,

    // ── delta, gauges: last present value wins ──
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<(EpochSeconds, EpochSeconds)>,

    // ── resumption: presence IS the resume-point indicator ──
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<Resume>,

    // ── an absolute materialized view as of this record, replacing every delta before it ──
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<MaterializedMeta>,
}

/// What a resume needs beyond the folded facts. Present iff the transcript admits a clean
/// partition at this drain (no line authored blocks on both sides of the frontier).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Resume {
    /// Committed-block count after this drain. **Not** an index into the record vector — a
    /// multi-turn line commits several blocks under one record, so these jump.
    pub id: usize,
    /// The partition offset: bytes below it authored only committed blocks.
    pub replay_from: ByteOffset,
    /// CRC32 of the 64 KiB ending at `replay_from` — everything restored derives from bytes
    /// below it, so that is the only region a rewrite can silently corrupt.
    pub window: u32,
    /// The thinking clock's zero — a `Thinking`'s duration measures from the previous event
    /// line, so without this the first re-read line renders `None` where a cold fold gives
    /// `Some`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev_ts: Option<EpochSeconds>,
    /// Stamps turns authored on the resume's first line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_ts: Option<EpochSeconds>,
}

/// Sub-agent lifecycle. **One ordered list**, because order is load-bearing: for
/// `Spawned(X)`, `Finished(X)`, `Spawned(X)` in one record, block order yields
/// `[finished, running]` while "all spawns then all dones" yields `[finished, finished]`.
/// `SessionMeta` deliberately keeps duplicate ids, so that is reachable.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum AgentEvent {
    Spawned(Spawn),
    /// Clears **every** spawn with this id — `SessionMeta::push`'s linear scan. By id and not
    /// by ordinal: it can refer to a spawn appended many records earlier, and a delta has no
    /// view of the accumulated list.
    Finished(AgentId),
}

/// Everything known about one sub-agent at spawn time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Spawn {
    /// The key a completion may arrive under, before `agent_id` exists.
    pub tool_use_id: String,
    /// Empty until the spawn's tool result lands.
    pub agent_id: AgentId,
    pub agent_type: String,
    pub description: String,
    /// A spawn can be born terminal (a synchronous `Task` returns done), so `running` is
    /// derived from this and any later `Finished` — never stored.
    pub status: AgentStatus,
}

// ── the materialized view ─────────────────────────────────────────────────────────────────

/// The fold of every record: what a reader reconstructs, and what a checkpoint stores.
///
/// **Iterative, not slice-at-once.** Unlike the committed `BV` vector — resident by definition,
/// being the committed index itself — records are consumed one at a time and never all held.
/// [`push`](Self::push) has no bound check: the *caller* stops at its aligned `n`, which is what
/// lets a metadata reader with no bound at all use the same type.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MaterializedMeta {
    /// Turns, tools and children — the header both frontends render.
    pub session_meta: SessionMeta,
    /// Spawn identity, under **both** keys per spawn (the id arrives late, and a completion
    /// names whichever the agent emitted).
    pub agent_ids: HashMap<String, (AgentId, String)>,
    pub user_times: Vec<Option<EpochSeconds>>,
    pub tokens: BTreeMap<Model, TokenCounts>,
    pub extra: BTreeMap<String, u64>,
    /// Ops **applied** as they arrive — carries the list and the unjoined creates, so
    /// `pending` is derived rather than persisted.
    pub tasks: TaskFold,
    pub cwd: String,
    pub span: Option<(EpochSeconds, EpochSeconds)>,
}

impl MaterializedMeta {
    /// Fold one record in.
    ///
    /// If `r.checkpoint` is `Some`, **adopt it and discard everything folded so far** — the
    /// checkpoint is the state *as of* this record, so its own delta fields are already
    /// included and must not be applied twice.
    pub fn push(&mut self, r: &MetaRecord) {
        if let Some(c) = &r.checkpoint {
            *self = c.clone();
            return;
        }
        self.push_delta(r);
    }

    /// Fold a record's **deltas only**, ignoring any checkpoint it carries.
    ///
    /// Separate from [`push`](Self::push) so a reader can compute what its running state *would*
    /// be at a checkpointed record and compare — the §6.6 validate-on-pass check, which is what
    /// turns "a resume equals a cold fold" from a property tests assert into one production
    /// verifies on every load.
    pub fn push_delta(&mut self, r: &MetaRecord) {
        // counters
        self.session_meta.turns += r.turns.unwrap_or(0) as usize;
        self.session_meta.tools += r.tools.unwrap_or(0) as usize;
        for e in &r.agents {
            match e {
                AgentEvent::Spawned(s) => {
                    if !s.agent_id.is_empty() {
                        self.session_meta.children.push(ChildMeta {
                            id: s.agent_id.clone(),
                            description: s.description.clone(),
                            agent_type: s.agent_type.clone(),
                            running: !s.status.is_terminal(),
                        });
                    }
                    for (k, id, ty) in spawn_keys(s) {
                        self.agent_ids.insert(k, (id, ty));
                    }
                }
                AgentEvent::Finished(id) => {
                    for c in self
                        .session_meta
                        .children
                        .iter_mut()
                        .filter(|c| &c.id == id)
                    {
                        c.running = false;
                    }
                }
            }
        }
        self.user_times.extend(r.user_times.iter().copied());
        for (m, c) in &r.tokens {
            *self.tokens.entry(m.clone()).or_default() += *c;
        }
        for (k, n) in &r.extra {
            *self.extra.entry(k.clone()).or_default() += n;
        }
        for op in &r.task_ops {
            self.tasks.apply_recorded(op);
        }
        // gauges
        if let Some(cwd) = &r.cwd {
            self.cwd = cwd.clone();
        }
        if let Some(s) = r.span {
            self.span = Some(s);
        }
    }
}

/// The `(key, agent_id, agent_type)` identity rows a spawn contributes.
///
/// The **single** definition of that mapping: the replayer's live map and the stream's
/// `Spawned` events both go through it, so they cannot drift. Two keys because the id is
/// discovered late — `tool_use_id` is known when the spawn folds, the real `agent_id` arrives
/// with the tool result, and a later completion may name either.
pub fn spawn_keys(s: &Spawn) -> Vec<(String, AgentId, String)> {
    let mut out = Vec::new();
    if !s.tool_use_id.is_empty() {
        out.push((
            s.tool_use_id.clone(),
            s.agent_id.clone(),
            s.agent_type.clone(),
        ));
    }
    if !s.agent_id.is_empty() {
        out.push((s.agent_id.clone(), s.agent_id.clone(), s.agent_type.clone()));
    }
    out
}

/// The identity rows a committed `Block` contributes — the block-side entry point, kept beside
/// [`spawn_keys`] so the two shapes cannot diverge.
pub fn agent_pairs(b: &Block) -> Vec<(String, AgentId, String)> {
    match b {
        Block::SubAgent(sa) => spawn_keys(&Spawn {
            tool_use_id: sa.tool_use_id.clone(),
            agent_id: sa.agent_id.clone(),
            agent_type: sa.agent_type.clone(),
            description: sa.description.clone(),
            status: sa.status,
        }),
        _ => Vec::new(),
    }
}

// ── CRC32 ─────────────────────────────────────────────────────────────────────────────────

/// CRC32 (IEEE), table-free.
///
/// Deliberately **not** a cryptographic digest: this detects *accidental* divergence — a
/// compaction, a truncation, a different file under the same name — never tampering, and it
/// could not be a trust boundary anyway, since anything able to rewrite the transcript can
/// rewrite the cache beside it. Measured 13× cheaper than sha256 over 64 KiB, and it keeps a
/// crypto dependency out of a crate that has three.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            // 0xEDB88320 is the reversed IEEE polynomial.
            crc = (crc >> 1) ^ (0xEDB8_8320 & (!(crc & 1)).wrapping_add(1));
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spawn(id: &str, tuid: &str, status: AgentStatus) -> Spawn {
        Spawn {
            tool_use_id: tuid.into(),
            agent_id: id.into(),
            agent_type: "general-purpose".into(),
            description: "child".into(),
            status,
        }
    }
    fn rec_agents(evs: Vec<AgentEvent>) -> MetaRecord {
        MetaRecord {
            agents: evs,
            ..Default::default()
        }
    }

    /// The known IEEE CRC32 of "123456789" — pins the hand-rolled implementation against the
    /// standard rather than against itself.
    #[test]
    fn crc32_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
        assert_ne!(crc32(b"a"), crc32(b"b"));
    }

    /// Counters fold; gauges replace. The distinction is the whole format.
    #[test]
    fn counters_fold_and_gauges_replace() {
        let mut mm = MaterializedMeta::default();
        for (turns, cwd) in [(1u32, "/a"), (2, "/b")] {
            mm.push(&MetaRecord {
                turns: Some(turns),
                tokens: BTreeMap::from([(
                    "m".to_string(),
                    TokenCounts {
                        output: 5,
                        ..Default::default()
                    },
                )]),
                extra: BTreeMap::from([("k".to_string(), 3)]),
                cwd: Some(cwd.into()),
                ..Default::default()
            });
        }
        assert_eq!(mm.session_meta.turns, 3, "counters add");
        assert_eq!(mm.tokens["m"].output, 10, "maps sum per key");
        assert_eq!(mm.extra["k"], 6, "a repeated extra key ADDS");
        assert_eq!(mm.cwd, "/b", "gauges take the last value");
    }

    /// Order within `agents` is load-bearing: a done between two spawns of the SAME id must
    /// not clear the later one. Two parallel lists would.
    #[test]
    fn agent_event_order_is_preserved() {
        let mut mm = MaterializedMeta::default();
        mm.push(&rec_agents(vec![
            AgentEvent::Spawned(spawn("a1", "t1", AgentStatus::Running)),
            AgentEvent::Finished("a1".into()),
            AgentEvent::Spawned(spawn("a1", "t2", AgentStatus::Running)),
        ]));
        let c = &mm.session_meta.children;
        assert_eq!(c.len(), 2, "duplicate ids are kept");
        assert!(!c[0].running, "the first was finished");
        assert!(c[1].running, "the second must NOT be");
    }

    /// `Finished` clears every match — `SessionMeta::push`'s linear scan, duplicates included.
    #[test]
    fn finished_clears_every_spawn_with_that_id() {
        let mut mm = MaterializedMeta::default();
        mm.push(&rec_agents(vec![
            AgentEvent::Spawned(spawn("a1", "t1", AgentStatus::Running)),
            AgentEvent::Spawned(spawn("a1", "t2", AgentStatus::Running)),
            AgentEvent::Finished("a1".into()),
        ]));
        assert!(mm.session_meta.children.iter().all(|c| !c.running));
    }

    /// A spawn registers under BOTH keys, and one whose id has not arrived still registers
    /// under its tool_use_id — a completion may name either.
    #[test]
    fn spawn_registers_under_both_keys() {
        let mut mm = MaterializedMeta::default();
        mm.push(&rec_agents(vec![AgentEvent::Spawned(spawn(
            "a1",
            "t1",
            AgentStatus::Running,
        ))]));
        assert_eq!(mm.agent_ids.len(), 2);
        assert_eq!(mm.agent_ids["t1"].0, "a1");
        assert_eq!(mm.agent_ids["a1"].0, "a1");

        let mut mm2 = MaterializedMeta::default();
        mm2.push(&rec_agents(vec![AgentEvent::Spawned(spawn(
            "",
            "t9",
            AgentStatus::Running,
        ))]));
        assert_eq!(mm2.agent_ids.len(), 1, "no id yet, but still resolvable");
        assert!(mm2.session_meta.children.is_empty(), "not a child yet");
    }

    /// Build a turn-counting record with a resume payload at `id`.
    fn r(id: usize, turns: u32) -> MetaRecord {
        MetaRecord {
            turns: Some(turns),
            resume: Some(Resume {
                id,
                replay_from: id as u64 * 10,
                window: 0,
                prev_ts: None,
                pending_ts: None,
            }),
            ..Default::default()
        }
    }

    /// **I11.** A stream that BEGINS at a checkpointed record and the same stream replayed from
    /// the start must materialize identically — that is what makes compaction sound, and what
    /// lets a reader's work be bounded rather than O(records).
    #[test]
    fn a_compacted_stream_materializes_like_an_uncompacted_one() {
        let mut whole: Vec<MetaRecord> = (1..=6).map(|i| r(i, 1)).collect();
        // Checkpoint at record 4, carrying the state as of it (4 turns folded).
        let mut at4 = MaterializedMeta::default();
        for x in &whole[..4] {
            at4.push(x);
        }
        whole[3].checkpoint = Some(at4);

        let full = align(&whole, 6).unwrap().expect("resumes");
        let compacted = align(&whole[3..], 6).unwrap().expect("resumes");
        assert_eq!(
            full.meta, compacted.meta,
            "a compacted stream is the same view as the whole one"
        );
        assert_eq!(full.committed, compacted.committed);
        assert_eq!(full.resume, compacted.resume);
    }

    /// **Validate on pass** (§6.6). A checkpoint that disagrees with the state folded up to it
    /// means the stream is corrupt or writer and reader have drifted — the false-accept class,
    /// which yields wrong output rather than a no-op. Reject the whole cache.
    #[test]
    fn a_checkpoint_that_disagrees_rejects_the_stream() {
        let mut recs: Vec<MetaRecord> = (1..=5).map(|i| r(i, 1)).collect();
        let mut wrong = MaterializedMeta::default();
        wrong.session_meta.turns = 999; // not what folding 1..=4 yields
        recs[3].checkpoint = Some(wrong);
        assert!(
            matches!(align(&recs, 5), Err(AlignError::CheckpointMismatch)),
            "a disagreeing checkpoint must reject, not resume"
        );
    }

    /// A checkpoint that AGREES is adopted and the fold carries on — the ordinary path, which
    /// must not be collateral damage of the check above.
    #[test]
    fn a_matching_checkpoint_passes_and_the_fold_continues() {
        let mut recs: Vec<MetaRecord> = (1..=5).map(|i| r(i, 1)).collect();
        let mut at4 = MaterializedMeta::default();
        for x in &recs[..4] {
            at4.push(x);
        }
        recs[3].checkpoint = Some(at4);
        let a = align(&recs, 5).unwrap().expect("resumes");
        assert_eq!(a.meta.session_meta.turns, 5);
        assert_eq!(a.committed, 5);
    }

    /// The case §8's review found: a checkpoint as the LAST record must still be resumable —
    /// it rides a record that carries a resume payload, so the stream never holds complete
    /// state with no `replay_from` anywhere.
    #[test]
    fn a_trailing_checkpoint_is_still_a_resume_point() {
        let mut recs: Vec<MetaRecord> = (1..=3).map(|i| r(i, 1)).collect();
        let mut at3 = MaterializedMeta::default();
        for x in &recs[..3] {
            at3.push(x);
        }
        recs[2].checkpoint = Some(at3);
        let a = align(&recs, 3)
            .unwrap()
            .expect("a trailing checkpoint still resumes");
        assert_eq!(a.committed, 3);
        assert_eq!(a.resume.replay_from, 30);
        assert_eq!(a.meta.session_meta.turns, 3);
    }

    /// A checkpoint ABOVE what the content stream corroborates is never reached, so it can
    /// neither be adopted nor trip the mismatch check (I1: content is the authority).
    #[test]
    fn a_checkpoint_above_the_content_stream_is_ignored() {
        let mut recs: Vec<MetaRecord> = (1..=5).map(|i| r(i, 1)).collect();
        let mut wrong = MaterializedMeta::default();
        wrong.session_meta.turns = 999;
        recs[4].checkpoint = Some(wrong); // record 5, beyond a 3-block content stream
        let a = align(&recs, 3).unwrap().expect("resumes at 3");
        assert_eq!(a.committed, 3, "stopped where content stops");
    }

    /// A checkpoint REPLACES the fold so far — its own delta fields are already included in
    /// it, so applying both would double-count. This is what makes compaction sound.
    #[test]
    fn a_checkpoint_replaces_rather_than_adds() {
        let mut folded = MaterializedMeta::default();
        folded.push(&MetaRecord {
            turns: Some(7),
            ..Default::default()
        });

        let mut absolute = MaterializedMeta::default();
        absolute.push(&MetaRecord {
            turns: Some(7),
            ..Default::default()
        });

        // A record carrying that absolute state, plus the delta it already contains.
        let mut seeded = MaterializedMeta::default();
        seeded.push(&MetaRecord {
            turns: Some(7),
            checkpoint: Some(absolute),
            ..Default::default()
        });
        assert_eq!(seeded, folded, "adopt, not add — turns must be 7, never 14");
    }
}

// ── alignment ─────────────────────────────────────────────────────────────────────────────

/// What a load recovered: the committed prefix it may keep, the meta folded to the same point,
/// and the resume payload to restart from.
#[derive(Clone, Debug)]
pub struct Aligned {
    /// How many committed blocks survive. The caller truncates its `BV` vector AND the store
    /// backing to this (I6) — orphaned bytes past it are read as garbage by a range-serving
    /// frontend.
    pub committed: usize,
    pub meta: MaterializedMeta,
    pub resume: Resume,
    /// How many records the fold consumed to get here, INCLUDING the resume record.
    ///
    /// The meta stream needs the same cut as the content stream (I6), and for the same reason:
    /// a resumed writer re-folds from `replay_from` and writes records for those commits again,
    /// so records past this point would be **counted twice** — once from the run that wrote
    /// them, once from the run that redid them. Blocks are unaffected (the content stream is
    /// cut), which is exactly why a block-only test cannot see it.
    pub records: usize,
}

/// Align a meta stream against a loaded committed count (#96 §6.2, I1).
///
/// **A pure function of two inputs** — no filesystem, no `BV` decoding — which is what lets the
/// crash cases be tested with hand-built vectors instead of by killing a writer at exactly the
/// right instant.
///
/// The `BV` vector's length is the sole authority; no committed count is persisted separately.
/// Records are folded in order and the last one whose resume payload the content stream
/// corroborates wins. **There is no positional correspondence between the two streams** (I2):
/// `finalize_completed` runs once per *line*, so a multi-turn line commits several blocks under
/// one record and `resume.id` jumps — the link is the id, never an index.
///
/// Returns `None` when no record qualifies: an empty or header-only stream, a torn tail below
/// the first resume point, or a content stream shorter than every recorded commit. The caller
/// then rebuilds cold.
pub fn align(records: &[MetaRecord], committed_len: usize) -> Result<Option<Aligned>, AlignError> {
    let mut mm = MaterializedMeta::default();
    let mut best: Option<Aligned> = None;
    for (folded, r) in records.iter().enumerate() {
        // Stop before folding a record whose commit the content stream cannot corroborate:
        // beyond this point the meta stream describes blocks that are not there.
        if let Some(res) = &r.resume {
            if res.id > committed_len {
                break;
            }
        }
        // §6.6 validate-on-pass: a checkpoint is an absolute claim about the state at its
        // record, so a reader that has folded its way here can check it instead of trusting
        // it. A disagreement means the stream is corrupt or writer and reader have drifted —
        // the false-accept class, which yields wrong output rather than a no-op, so the answer
        // is to reject the whole cache. Skipped for the FIRST record: a compacted stream opens
        // on a checkpoint with nothing before it to compare against.
        if let Some(c) = &r.checkpoint {
            if folded > 0 {
                let mut probe = mm.clone();
                probe.push_delta(r);
                if probe != *c {
                    return Err(AlignError::CheckpointMismatch);
                }
            }
        }
        mm.push(r);
        if let Some(res) = &r.resume {
            best = Some(Aligned {
                committed: res.id,
                meta: mm.clone(),
                resume: res.clone(),
                records: folded + 1,
            });
        }
    }
    Ok(best)
}

/// Why an otherwise-readable stream must be rejected outright.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlignError {
    /// A checkpoint disagreed with the state folded up to it (§6.6).
    CheckpointMismatch,
}
