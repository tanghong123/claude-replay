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
