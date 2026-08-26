//! **The single incremental fold orchestrator.** One `SessionAccumulator` threads everything the
//! parse paths used to thread by hand — the agent's L1 `decode_line` (with its `cwd`), the
//! shared L2 [`Replayer`] fold, and the per-agent metrics accumulator — behind one `advance`.
//!
//! Incremental parsing is first-class; one-shot is derived: the whole-file batch parse
//! (the facade's `parse_session_as` /
//! `parse_path_timed_for`) feeds this builder
//! line-by-line (one line resident, no whole-file `Vec<String>`), and the live
//! [`FollowParser`](crate::FollowParser) feeds it the appended lines each poll — so batch and
//! live share exactly one line loop. The output is proven byte-identical to the retired
//! whole-file oracles (the equivalence gates in `claude_model`/`codex_model`) and to a full
//! re-parse (the `follow_*` tests).

use crate::adapter::{LinePreprocessor, MetricsAccumulator, PreprocessedLine, TranscriptAdapter};
use crate::engine::message::Message;
use crate::engine::replay::Replayer;
use crate::engine::session::{BlockRead, BlockStore, InMemoryStore, Session, SessionMeta};
use crate::engine::SessionIndex;
use crate::metrics::Metrics;
use crate::model::{AttachmentContent, Block, ByteOffset, EpochSeconds};
use crate::Agent;
use serde_json::Value;
use std::io;

/// Folds a transcript incrementally into a [`Session`], threading its blocks through a
/// [`BlockStore`] `S`: `advance` a batch of lines, `fold`/`snapshot` the current state, `reset`
/// on a truncation/rewrite. Everything agent-specific — the L1 decoder, the L2 `Shaping`, the
/// metrics accumulator — comes from the agent's `TranscriptAdapter`, so the accumulator itself is
/// agent-agnostic. It's an accumulator/fold (`advance` + `snapshot`), not a configure-then-build
/// "Builder"; the storage policy is `S` (default [`InMemoryStore`] ⇒ blocks resident in RAM).
pub struct SessionAccumulator<S: BlockStore = InMemoryStore> {
    agent: Agent,
    adapter: &'static dyn TranscriptAdapter,
    replayer: Replayer<'static>,
    /// The FOLD cwd — threaded across lines by `decode_line` for path relativization. Running-
    /// current (#173): a line's non-empty `cwd` moves it forward, so its final value is the
    /// LATEST recorded cwd (the resume anchor), and each `cd` emits a cwd delta at the emit
    /// point below. Session identity uses `first_cwd` instead (filled by the facade), not this.
    cwd: String,
    /// Agent-specific physical→logical line boundary state. Usually pass-through; Codex uses
    /// it to exclude a child rollout's cloned parent bootstrap from content and metrics.
    preprocessor: Box<dyn LinePreprocessor>,
    metrics: Box<dyn MetricsAccumulator>,
    /// The per-block storage policy — where each committed block's content goes.
    store: S,
    /// The **committed** blocks, `put` through the store exactly once as each turn crosses the
    /// durability frontier (drained from the replayer per `advance`). The replayer keeps only the
    /// open window, so its resident content is O(turn); this owns the O(N) committed prefix (which
    /// for a deferred store is a tiny locator table, content on disk).
    committed: Vec<S::Bv>,
    /// The live-header facts of the **committed** prefix, folded once per committed block on drain
    /// (never rescanned). The full header for a poll is this + the open turn re-folded on top (see
    /// [`session_meta`](Self::session_meta)) — so a live consumer reads it without an O(N) scan.
    committed_meta: SessionMeta,
    /// The task op-log fold (#15) — session state maintained here (like metrics),
    /// never seen by the block replayer.
    task_fold: crate::engine::tasks::TaskFold,
    /// One entry per **authored turn** in the open window, oldest first (#96 §6.1). The front
    /// entry locates the resume partition; the drain prunes below the frontier.
    boundary: std::collections::VecDeque<Boundary>,
    /// The state as of the last resume payload written — counter deltas are the difference
    /// between two `replay_from` captures, not "since the last commit".
    emitted: Boundary,
    /// Meta records authored but not yet drained by the persistence layer.
    meta_out: Vec<crate::engine::meta_stream::MetaRecord>,
    /// Out-of-band spawn identity (see [`SpawnLink`](crate::adapter::SpawnLink)), handed down by
    /// the I/O layer that knows the transcript's path — the fold itself reads no files. Empty for
    /// every agent that names its children in-band, and then adoption costs a length check.
    links: Vec<crate::adapter::SpawnLink>,

    /// Spawn identity for the **committed** prefix only, folded on drain. The replayer's live
    /// map also holds open-window spawns, which a checkpoint must not claim.
    committed_agents: std::collections::HashMap<String, (crate::model::AgentId, String)>,
    /// Resumable drains since the last checkpoint. Only resumable ones count: a checkpoint must
    /// ride a record that carries a resume payload (§6.6), so a straddling drain does not
    /// advance the clock — it waits for the next qualifying one.
    since_checkpoint: usize,
}

/// A turn-opening line's state, captured **before** the line is folded — the candidate
/// `replay_from` and everything a resume from there must be seeded with.
#[derive(Clone, Debug, Default)]
struct Boundary {
    /// Raw-logical index of this line's FIRST authored block. Equals the post-drain frontier
    /// exactly when this line's first block is the first uncommitted one — §3's partition.
    logical: usize,
    offset: ByteOffset,
    prev_ts: Option<EpochSeconds>,
    pending_ts: Option<EpochSeconds>,
    /// Metrics TOTALS as of this line's start (`metrics.push` runs last, below).
    tokens: std::collections::BTreeMap<String, crate::metrics::TokenCounts>,
    extra: std::collections::BTreeMap<String, u64>,
    span: Option<(EpochSeconds, EpochSeconds)>,
    cwd: String,
    /// Opaque adapter preprocessing state as of this line's start. A durable resume begins at
    /// `offset`, so it must restore everything the adapter learned from bytes below that point
    /// before classifying the first re-read line.
    pre: Value,
    /// Opaque agent metrics-accumulator state at the same boundary. Shared totals alone cannot
    /// resume accumulators such as Codex's cumulative counter: it also needs the last raw total
    /// and model in force to attribute only the appended delta.
    metrics_state: Value,
}

/// The delta-sized read a live streaming consumer (the pull protocol) needs each poll — WITHOUT
/// the O(N) `index`/`sub_agents` build or a whole-committed clone. `committed_delta` is only
/// `committed[from..]` (the blocks past what the caller already rendered); everything else is
/// O(turn). Produced by [`SessionAccumulator::stream_read`].
pub struct StreamRead {
    /// `committed[from..]` — the newly-committed blocks the caller hasn't rendered yet.
    pub committed_delta: Vec<Block>,
    /// The finalized open turn (provisional zone) — O(turn).
    pub provisional: Vec<Block>,
    /// The whole session's per-turn timestamps (the renderer indexes into it by turn).
    pub user_times: Vec<Option<EpochSeconds>>,
    /// The current folded metrics.
    pub metrics: Metrics,
    /// The full live header — committed meta + the open turn folded on top (matches the tail).
    pub meta: SessionMeta,
    /// The current committed count (== the split point between `committed_delta`'s base and the
    /// provisional zone).
    pub n_committed: usize,
    /// The current task op-log state (#15) — rides the read so the pull path's meta
    /// carries it without a session assembly.
    pub tasks: crate::engine::tasks::TaskList,
}

/// Fold one committed block's contribution into a record's counters.
///
/// **Must stay arm-for-arm with `SessionMeta::push`** — that equality is what the oracle test
/// asserts, and an earlier hand-mirrored copy silently dropped `Thinking{tools}`.
fn count_into(rec: &mut crate::engine::meta_stream::MetaRecord, b: &Block) {
    use crate::engine::meta_stream::{AgentEvent, Spawn};
    match b {
        Block::UserText(_) | Block::Command { .. } => *rec.turns.get_or_insert(0) += 1,
        Block::ToolUse { .. } => *rec.tools.get_or_insert(0) += 1,
        // An activity-coalesced run is ONE block carrying its nested calls.
        Block::Thinking { tools, .. } if !tools.is_empty() => {
            *rec.tools.get_or_insert(0) += tools.len() as u32
        }
        Block::SubAgent(sa) => rec.agents.push(AgentEvent::Spawned(Spawn {
            tool_use_id: sa.tool_use_id.clone(),
            agent_id: sa.agent_id.clone(),
            agent_type: sa.agent_type.clone(),
            description: sa.description.clone(),
            status: sa.status,
        })),
        Block::AgentDone { agent_id, .. } if !agent_id.is_empty() => {
            rec.agents.push(AgentEvent::Finished(agent_id.clone()))
        }
        _ => {}
    }
}

impl SessionAccumulator<InMemoryStore> {
    /// A fresh accumulator for the agent behind `adapter`, with the in-memory
    /// (identity) store: empty replayer/cwd/metrics, ready to `advance`.
    pub fn new(adapter: &'static dyn TranscriptAdapter) -> Self {
        Self::with_store(adapter, InMemoryStore)
    }
}

impl<S: BlockStore> SessionAccumulator<S> {
    /// A fresh accumulator for the agent behind `adapter`, with an explicit
    /// [`BlockStore`]: empty replayer/cwd/metrics, ready to `advance`. The facade's
    /// entry points resolve the adapter from its registry (#87 step 3).
    pub fn with_store(adapter: &'static dyn TranscriptAdapter, store: S) -> Self {
        Self {
            agent: adapter.agent(),
            adapter,
            replayer: Replayer::new(adapter.shaping()),
            cwd: String::new(),
            preprocessor: adapter.line_preprocessor(),
            metrics: adapter.metrics_acc(),
            store,
            committed: Vec::new(),
            committed_meta: SessionMeta::default(),
            task_fold: crate::engine::tasks::TaskFold::default(),
            boundary: std::collections::VecDeque::new(),
            emitted: Boundary::default(),
            meta_out: Vec::new(),
            committed_agents: Default::default(),
            since_checkpoint: 0,
            links: Vec::new(),
        }
    }

    /// Hand down the session's out-of-band spawn identity — see
    /// [`SpawnLink`](crate::adapter::SpawnLink). The fold stays sans-io: whoever opened the
    /// transcript reads the links and pushes them here, and re-pushing a refreshed table is how a
    /// live follower keeps up with children spawned mid-session.
    pub fn set_spawn_links(&mut self, links: Vec<crate::adapter::SpawnLink>) {
        self.links = links;
    }

    /// Rebuild an accumulator from a persisted cache (#96 §6.3) — the inverse of the record
    /// stream.
    ///
    /// Takes the two loaded halves as plain values: the committed `Bv`s (whose loader is the one
    /// frontend-specific piece) and the materialized meta folded from the records up to the same
    /// point. It opens no file and decodes no `Bv`, so this is ONE implementation for every
    /// presentation — which is what requirement R5 asks for. The caller then feeds
    /// [`advance_at`](Self::advance_at) from `resume.replay_from`, folding normally and
    /// suppressing nothing.
    pub fn restore(
        adapter: &'static dyn TranscriptAdapter,
        store: S,
        committed: Vec<S::Bv>,
        mm: crate::engine::meta_stream::MaterializedMeta,
        resume: &crate::engine::meta_stream::Resume,
    ) -> Self {
        let mut acc = Self::with_store(adapter, store);
        acc.committed = committed;
        // A resumed writer continues from the restored state: without this its next checkpoint
        // would claim the session began at the resume point.
        acc.committed_agents = mm.agent_ids.clone();
        acc.committed_meta = mm.session_meta.clone();
        acc.cwd = mm.cwd.clone();
        acc.task_fold = mm.tasks.clone();
        acc.metrics
            .reseed(mm.tokens.clone(), mm.extra.clone(), mm.span);
        acc.metrics.restore(&resume.metrics_state);
        // `user_times` has length `committed_meta.turns` — its value at `replay_from`, since by
        // the §3 partition every uncommitted `UserText` lies at or above that offset and none
        // has been stamped.
        acc.replayer.reseed(
            mm.agent_ids.clone(),
            mm.user_times.clone(),
            resume.prev_ts,
            resume.pending_ts,
        );
        acc.preprocessor.restore(&resume.pre);
        // A resumed writer measures its counter deltas from where the last record left off,
        // not from zero — otherwise the next record would re-report the whole session.
        acc.emitted = Boundary {
            logical: 0,
            offset: resume.replay_from,
            prev_ts: resume.prev_ts,
            pending_ts: resume.pending_ts,
            tokens: mm.tokens,
            extra: mm.extra,
            span: mm.span,
            cwd: mm.cwd,
            pre: resume.pre.clone(),
            metrics_state: resume.metrics_state.clone(),
        };
        acc
    }

    /// Attach a checkpoint when one is due (§6.6).
    ///
    /// Only a record carrying a resume payload may hold one. A checkpoint with no `replay_from`
    /// after it would let compaction leave a cache holding complete state that nothing can
    /// resume from.
    fn checkpoint_maybe(&mut self, rec: &mut crate::engine::meta_stream::MetaRecord) {
        if rec.resume.is_none() {
            return;
        }
        self.since_checkpoint += 1;
        if self.since_checkpoint >= crate::engine::meta_stream::CHECKPOINT_EVERY {
            rec.checkpoint = Some(self.materialized());
            self.since_checkpoint = 0;
        }
    }

    /// The absolute state as of the last authored resume point — the value a checkpoint carries.
    ///
    /// **Built from the accumulator's own maintained state, deliberately NOT by folding the
    /// records it just wrote.** A checkpoint's job on load is to be a *second, independent*
    /// answer to "what does this stream say the session is": a reader folds the deltas, compares,
    /// and rejects on a disagreement. Deriving it from those same deltas would make the
    /// comparison tautological — it could still catch a corrupted byte on disk or a writer/reader
    /// version skew, but never a bug in the deltas themselves.
    ///
    /// That bug class is not hypothetical. `count_into` is a hand-mirrored copy of
    /// `SessionMeta::push` that has already drifted once, silently dropping `Thinking{tools}`.
    /// Sourcing `session_meta` from `committed_meta` — the `SessionMeta::push` side — is what
    /// makes a future drift of that pair a *load-time rejection* rather than a wrong session.
    ///
    /// Per field: `session_meta` from the maintained header, `user_times` from the replayer's
    /// stamps, `tasks` from the op-log fold, and the metrics/cwd from the resume point's own
    /// capture — each the counterpart of a delta the reader folds, and none of them the delta.
    /// (`agent_ids` is the stated exception: both sides already route through the shared
    /// `agent_pairs`, so there is no second opinion to be had — only the committed/open split
    /// matters, which is what `committed_agents` tracks.)
    pub fn materialized(&self) -> crate::engine::meta_stream::MaterializedMeta {
        let e = &self.emitted;
        crate::engine::meta_stream::MaterializedMeta {
            session_meta: self.committed_meta.clone(),
            agent_ids: self.committed_agents.clone(),
            user_times: self
                .replayer
                .user_times()
                .iter()
                .take(self.committed_meta.turns)
                .copied()
                .collect(),
            // Zero entries are dropped, because the DELTA stream cannot express one: a record
            // carries a counter only when it changed (R7), so a model or key that never scored
            // reaches a reader as an absent key, not a zero. The checkpoint has to speak the
            // same vocabulary or it disagrees over nothing — a real session tripped exactly
            // this, on a `<synthetic>` model whose counts were all zero.
            tokens: e
                .tokens
                .iter()
                .filter(|(_, v)| **v != crate::metrics::TokenCounts::default())
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            extra: e
                .extra
                .iter()
                .filter(|(_, v)| **v != 0)
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            tasks: self.task_fold.clone(),
            cwd: e.cwd.clone(),
            span: e.span,
        }
    }

    /// Fold ONE line into the running state, knowing its **start byte offset** in the transcript:
    /// decode it to canonical messages, stamp that offset into any content-bearing attachment's
    /// [`Deferred`](AttachmentContent::Deferred) locator (so the bytes can be re-loaded on
    /// demand), `apply` the messages to the replayer, and push the raw line's usage into metrics.
    /// The single per-line unit shared by the batch parse ([`advance_reader`](Self::advance_reader))
    /// and the live follower.
    ///
    /// Returns the replayer's back-patch signal (see `Replayer::apply`): the min raw-logical index
    /// of any already-emitted provisional block this line mutated in place, or `None` if it only
    /// appended. The streaming layer's pull (§9a) bumps the provisional generation when it is `Some`;
    /// batch callers ([`advance_reader`](Self::advance_reader)) ignore it. (A coincident commit — the
    /// drain below — makes it moot, since a grown committed prefix resets the provisional regardless;
    /// the caller reconciles.)
    pub fn advance_at(&mut self, offset: ByteOffset, line: &str) -> Option<usize> {
        // `process` may mutate adapter-private state (for example Codex learns from session_meta
        // that later JavaScript exec wrappers have semantic mirrors). A resume at this line must
        // receive the state from BEFORE the line, not the state after classifying it.
        let pre = self.preprocessor.state();
        let metrics_state = self.metrics.state();
        let mut delta: Vec<Message> = match self.preprocessor.process(line) {
            PreprocessedLine::Include => {
                let mut messages = Vec::new();
                self.adapter.decode_line(line, &mut self.cwd, &mut messages);
                messages
            }
            PreprocessedLine::Ignore => return None,
            PreprocessedLine::Messages(messages) => messages,
        };
        // Stamp `offset` (and a per-line ordinal) onto every content-bearing attachment this
        // line produced — the builder is the level that knows the byte offset; the L1 decoder
        // left the locators as placeholders. Path-only (`None`) attachments are untouched.
        let mut index = 0usize;
        for m in &mut delta {
            if let Message::Attachment(a) = m {
                if let AttachmentContent::Deferred { at, index: ix, .. } = &mut a.content {
                    *at = offset;
                    *ix = index;
                    index += 1;
                }
            }
        }
        // #96 §6.1: capture a candidate boundary BEFORE anything folds — the snapshot must
        // predate this line's effects. Over-approximating `can_open_turn` is safe only because
        // an entry that authors nothing is discarded below.
        let cand = delta.iter().any(Message::can_open_turn).then(|| {
            let (tokens, extra, span) = self.metrics.totals();
            Boundary {
                logical: self.replayer.raw_len(),
                offset,
                prev_ts: self.replayer.prev_ts(),
                pending_ts: self.replayer.pending_ts(),
                tokens,
                extra,
                span,
                cwd: self.cwd.clone(),
                pre,
                metrics_state,
            }
        });
        // The task op-log (#15) folds HERE, at the accumulator — task state is
        // session state like metrics/meta, not a block, so the block replayer (and
        // its parse_main equivalence oracle) never see it. Tool results feed the
        // create→id join.
        for m in &delta {
            match m {
                Message::TaskOp(op) => self.task_fold.apply(op),
                Message::ToolResult {
                    tool_use_id, text, ..
                } => self.task_fold.on_tool_result(tool_use_id, text),
                _ => {}
            }
        }
        let patched = self.replayer.apply(&delta);
        // A flagged line can author NOTHING — a `CommandStdout` that patches into a prior
        // `Command`. Its entry would then carry the NEXT line's raw index and match the
        // frontier falsely, and a resume from that offset would fabricate an orphan block a
        // cold fold never had. So the entry only lands if the line really authored one.
        if let Some(c) = cand {
            if self.replayer.raw_len() > c.logical {
                self.boundary.push_back(c);
            }
        }
        // Drain the turns that just crossed the durability frontier and `put` each once — the
        // replayer drops them, keeping its content O(turn); we own the committed prefix. Fold each
        // finalized committed block into the maintained header **once**, before it's stored, so a
        // live poll reads the header without rescanning the committed prefix.
        let mut drained: Vec<Block> = self.replayer.drain_committed();
        if !drained.is_empty() {
            // Adopt before the block is counted, stored, or folded into the header — a committed
            // block is never revisited, so this is its one chance to learn who its child is.
            crate::adapter::apply_spawn_links(&mut drained, &self.links);
            let turns0 = self.committed_meta.turns;
            let mut rec = crate::engine::meta_stream::MetaRecord::default();
            for b in &drained {
                count_into(&mut rec, b);
                // The committed half of the replayer's live spawn map — folded here, from the
                // blocks, so a checkpoint never claims an open-window spawn.
                for (k, id, ty) in crate::engine::meta_stream::agent_pairs(b) {
                    self.committed_agents.insert(k, (id, ty));
                }
            }
            let times = self.replayer.user_times().to_vec();
            for b in drained {
                self.committed_meta.push(&b);
                let at = self.committed.len();
                let bv = self.store.put(b, at, &times);
                self.committed.push(bv);
            }
            // `user_times` cannot come from the blocks: the stamps live in the replayer,
            // indexed by TURN. Slice by the turn count this drain added.
            rec.user_times = times[turns0..self.committed_meta.turns].to_vec();
            rec.task_ops = self.task_fold.drain_recorded();
            self.boundary.retain(|e| e.logical >= self.replayer.base());
            self.author_resume(&mut rec);
            self.checkpoint_maybe(&mut rec);
            self.meta_out.push(rec);
        }
        match serde_json::from_str::<Value>(line) {
            Ok(v) => self.metrics.push(&v),
            // The reader hands this path complete lines only, so unlike `parse_reader` there
            // is no torn tail to excuse — but a blank line is still nothing, not drift.
            Err(_) if !line.trim().is_empty() => self.metrics.malformed_line(),
            Err(_) => {}
        }
        patched
    }

    /// Attach the resume payload and the gauge/counter deltas, **iff** the §3 partition exists
    /// at this drain (#96 I5).
    ///
    /// The check is on the deque FRONT, not the current line: at a `last_skill`-pinned drain the
    /// current line opens the *newest* turn while `replay_from` is the line of the *oldest*
    /// uncommitted block. A straddling line's entry has already been pruned (its `logical` sits
    /// below the frontier), so the check simply fails on whatever follows.
    fn author_resume(&mut self, rec: &mut crate::engine::meta_stream::MetaRecord) {
        let Some(e) = self.boundary.front().cloned() else {
            return;
        };
        if e.logical != self.replayer.base() {
            return; // the partition falls inside a line — no resume point here
        }
        // Counter deltas are the difference between two `replay_from` captures, NOT "since the
        // last commit": the state is as-of that line, so the baseline must be too.
        for (m, c) in &e.tokens {
            let prev = self.emitted.tokens.get(m).copied().unwrap_or_default();
            let d = crate::metrics::TokenCounts {
                input: c.input.saturating_sub(prev.input),
                cache_creation: c.cache_creation.saturating_sub(prev.cache_creation),
                cache_read: c.cache_read.saturating_sub(prev.cache_read),
                output: c.output.saturating_sub(prev.output),
            };
            if d != crate::metrics::TokenCounts::default() {
                rec.tokens.insert(m.clone(), d);
            }
        }
        for (k, n) in &e.extra {
            let d = n.saturating_sub(self.emitted.extra.get(k).copied().unwrap_or(0));
            if d > 0 {
                rec.extra.insert(k.clone(), d);
            }
        }
        // Gauges: written only when changed (R7).
        if e.span != self.emitted.span {
            rec.span = e.span;
        }
        if e.cwd != self.emitted.cwd {
            rec.cwd = Some(e.cwd.clone());
        }
        rec.resume = Some(crate::engine::meta_stream::Resume {
            id: self.committed.len(),
            replay_from: e.offset,
            // The window CRC is the persistence layer's to compute — it owns the source bytes.
            // Zero here means "unset"; the writer fills it before the record lands on disk.
            window: 0,
            prev_ts: e.prev_ts,
            pending_ts: e.pending_ts,
            pre: e.pre.clone(),
            metrics_state: e.metrics_state.clone(),
        });
        self.emitted = e;
    }

    /// Take the meta records authored since the last call (#96) — drained in lockstep with the
    /// committed blocks, so a consumer persisting both streams keeps them aligned.
    pub fn drain_meta(&mut self) -> Vec<crate::engine::meta_stream::MetaRecord> {
        std::mem::take(&mut self.meta_out)
    }

    /// Fold a whole transcript `reader` line-by-line, tracking each line's start byte offset so
    /// attachment locators are stamped correctly. One line resident at a time (no whole-file
    /// `Vec<String>`), so a multi-gigabyte transcript never balloons into memory. The batch
    /// parse entry points feed the builder through this; the live follower uses
    /// [`advance_at`](Self::advance_at) directly with the reader's per-line offsets.
    pub fn advance_reader(&mut self, reader: &mut dyn io::BufRead) -> io::Result<()> {
        // #193: the one bounded source, under the adapter's α-lite policy — only values the
        // fold defers (attachment bodies) are elided, so `fold(elide(line)) ≡ fold(line)`.
        // `Yield` because a batch parse folds a torn final line, as it always has.
        let mut src = crate::engine::reader::LineSource::new(
            reader,
            0,
            crate::engine::reader::TornTail::Yield,
            self.adapter.elision(),
        );
        while let Some((at, line)) = src.next()? {
            self.advance_at(at, line);
        }
        let counts = src.elided;
        self.bank_elision(counts);
        Ok(())
    }

    /// Bank the read layer's elision gauges into the accumulating `Metrics::extra` (#193,
    /// decision ③a): the reader owns the counts, the accumulator owns the bag. Zeroes are
    /// never banked, so an un-elided fold's `extra` is byte-identical to the pre-#193 shape.
    pub(crate) fn bank_elision(&mut self, c: crate::engine::reader::ElisionCounts) {
        for (key, n) in [
            ("elided_lines", c.elided_lines),
            ("elided_bytes", c.elided_bytes),
            ("skipped_lines", c.skipped_lines),
        ] {
            if n > 0 {
                self.metrics.bump_extra(key, n);
            }
        }
    }

    /// Consume the accumulator, returning its [`BlockStore`]. For a tier-b store this is how a
    /// consumer reclaims the backing after the final [`snapshot`](Self::snapshot) (pair it with the
    /// `Session<Deferred>` in a [`TierBSession`](crate::engine::tier_b::TierBSession) to read blocks).
    pub fn into_store(self) -> S {
        self.store
    }

    /// How many blocks have crossed the durability frontier into `committed` — i.e. the split point
    /// between the append-only committed prefix and the open provisional turn in `fold`'s
    /// block list. Lets a live consumer locate the settled/in-flight boundary in O(1) instead of
    /// re-deriving it by scanning blocks.
    /// The maintained header of the **committed** prefix — the right-hand side of #96's
    /// oracle (`session_meta()` is the merged value, which includes the open turn).
    pub fn committed_meta(&self) -> &SessionMeta {
        &self.committed_meta
    }

    pub fn committed_len(&self) -> usize {
        self.committed.len()
    }

    /// Rebuild from scratch — recreate the replayer, clear the cwd, and take a fresh metrics
    /// accumulator. The live follower calls this on a truncation/compaction (the reader re-read
    /// from 0, so the next `advance` folds the whole new file).
    pub(crate) fn reset(&mut self) {
        self.replayer = Replayer::new(self.adapter.shaping());
        self.cwd.clear();
        self.preprocessor = self.adapter.line_preprocessor();
        self.metrics = self.adapter.metrics_acc();
        self.committed.clear();
        self.committed_meta = SessionMeta::default();
        self.committed_agents.clear();
        self.since_checkpoint = 0;
        self.task_fold = crate::engine::tasks::TaskFold::default();
        // An append-only store (tier-b) discards its backing too — the rebuilt session's
        // locators start from a clean slate instead of accreting dead content.
        self.store.reset();
    }

    /// The committed locator/value slice itself — `&[S::Bv]`, no decode. A persistence layer
    /// serializes these directly (for tier-b they are tiny `Deferred` locators; the content is
    /// already in the store's backing).
    pub fn committed(&self) -> &[S::Bv] {
        &self.committed
    }

    /// The storage policy — for reading store-level facts (e.g. a tier-b backing's length).
    pub fn store(&self) -> &S {
        &self.store
    }

    /// Mutable store access — the handoff drain (#76).
    pub fn store_mut(&mut self) -> &mut S {
        &mut self.store
    }

    /// `committed[from..]` as owned [`Block`]s — the newly-committed tail past what a live consumer
    /// already rendered. O(delta): it copies only the tail, never the whole committed prefix (the
    /// step off the per-poll O(N) clone). `from` is clamped to the committed length. Requires a
    /// lossless store ([`BlockRead`]); a projection store serves its committed zone from its own
    /// representation instead (#74).
    pub fn committed_tail(&self, from: usize) -> Vec<Block>
    where
        S: BlockRead,
    {
        self.committed[from.min(self.committed.len())..]
            .iter()
            .map(|bv| self.store.get(bv).into_owned())
            .collect()
    }

    /// The finalized open turn, with out-of-band spawn identity adopted — the ONE place the open
    /// window is read, so blocks and the header derived from it can never disagree about who a
    /// child is. Adoption sits here rather than at the consumer's edge because
    /// [`session_meta`](Self::session_meta) is built from these blocks and the live server
    /// registers children from THAT: an id that reached only the blocks would render a link the
    /// server could not resolve.
    fn open_snapshot(&self) -> (Vec<Block>, Vec<Option<EpochSeconds>>) {
        let (mut open, times) = self.replayer.open_snapshot();
        crate::adapter::apply_spawn_links(&mut open, &self.links);
        (open, times)
    }

    /// The finalized open turn's block count — the provisional zone length a live consumer's cursor
    /// addresses. O(turn) (finalizes the open window).
    pub fn provisional_len(&self) -> usize {
        self.open_snapshot().0.len()
    }

    /// The finalized open turn (the provisional zone) + the WHOLE session's per-turn timestamps —
    /// O(turn), no committed clone. The block-level complement to
    /// [`committed_tail`](Self::committed_tail): a consumer serving the two zones separately reads
    /// `committed_tail(from)` for the settled prefix and this for the open tail.
    pub fn open_finalized(&self) -> (Vec<Block>, Vec<Option<EpochSeconds>>) {
        self.open_snapshot()
    }

    /// The live header for the current tail: the maintained **committed** meta with the finalized
    /// **open** turn folded on top — so it equals `SessionMeta::build(committed ++ provisional)`
    /// (a poll's header) without rescanning the committed prefix. O(turn).
    pub fn session_meta(&self) -> SessionMeta {
        let mut m = self.committed_meta.clone();
        for b in &self.open_snapshot().0 {
            m.push(b);
        }
        m
    }

    /// The full delta-sized read for one streaming poll (see [`StreamRead`]) — one `open_snapshot`
    /// so provisional, `user_times`, and the header all come from a single finalize. Copies only
    /// `committed[from..]`; never the whole committed prefix.
    pub fn stream_read(&self, from: usize) -> StreamRead
    where
        S: BlockRead,
    {
        let mut r = self.open_read();
        r.committed_delta = self.committed_tail(from);
        r
    }

    /// [`stream_read`](Self::stream_read) WITHOUT the committed content — everything a live
    /// consumer needs that is O(turn): the finalized open turn, times, metrics, header, counters.
    /// Store-agnostic (no [`BlockRead`] bound), so a projection-store session (#74) reads its open
    /// zone through this while serving committed from its own representation.
    pub fn open_read(&self) -> StreamRead {
        let (provisional, user_times) = self.open_snapshot();
        let mut meta = self.committed_meta.clone();
        for b in &provisional {
            meta.push(b);
        }
        StreamRead {
            committed_delta: Vec::new(),
            provisional,
            user_times,
            metrics: self.metrics.finish(),
            meta,
            n_committed: self.committed.len(),
            tasks: self.task_fold.snapshot().clone(),
        }
    }

    /// The current presentable blocks + per-turn times + folded metrics, WITHOUT consuming the
    /// builder (so the follower can `advance` a delta, `fold` to render, then keep folding).
    /// Same output as a full whole-file parse.
    pub fn fold(&self) -> (Vec<Block>, Vec<Option<EpochSeconds>>, Metrics)
    where
        S: BlockRead,
    {
        let (open, times) = self.open_snapshot();
        // Reconstruct the block stream: committed (read back from the store) ++ the open tail.
        let mut blocks: Vec<Block> = self
            .committed
            .iter()
            .map(|bv| self.store.get(bv).into_owned())
            .collect();
        blocks.extend(open);
        (blocks, times, self.metrics.finish())
    }

    /// The current state as a [`Session`] (blocks + per-turn times + metrics + derived index),
    /// with `cwd` left `None` — the batch entry fills it from the transcript path. Each finalized
    /// block is mapped through the [`BlockStore`] into `S::Bv` (for [`InMemoryStore`] this is
    /// identity ⇒ byte-identical to today's `Vec<Block>`).
    ///
    /// Takes `&mut self` because `put` is `&mut` (the store may append to a backing tier).
    /// Stage 1 maps-through-put per snapshot (fine for identity); Stage 2 moves to
    /// put-once-on-emit.
    /// The current task op-log state (#15) — cheap (no session assembly).
    pub fn tasks(&self) -> &crate::engine::tasks::TaskList {
        self.task_fold.snapshot()
    }

    /// [`into_session`](Self::into_session) with a fleet's members merged in — the READ-side
    /// reconcile (#38).
    ///
    /// **Batch only, and deliberately so.** A whole-file parse is one shot: no cursor, no record
    /// log, no cache, nothing addressed by block index afterwards — so merging runtime state into
    /// the view here costs nothing, and every block-shaped consumer (the TUI, `--dump`,
    /// `--dump --json`, the offline bundle) keeps working with no second vocabulary to learn.
    ///
    /// The LIVE path must never do this. There the committed prefix is append-only, addressed by
    /// index, and already written to a durable record log, so a member arriving late cannot be
    /// inserted into it — the roster rides the META instead, recomputed on every poll, exactly as
    /// live task files already do. Which is why this takes the roster as an argument rather than
    /// the fold holding one: the fold stays a function of the transcript.
    pub fn into_session_with_runs(
        self,
        adapter: &'static dyn TranscriptAdapter,
        rosters: &[crate::adapter::SpawnRoster],
    ) -> Session<Block>
    where
        S: BlockStore<Bv = Block> + BlockRead,
    {
        let mut s = self.into_session();
        if rosters.is_empty() {
            return s;
        }
        s.committed = crate::adapter::expand_spawn_rosters(adapter, s.committed, rosters);
        s.provisional = crate::adapter::expand_spawn_rosters(adapter, s.provisional, rosters);
        let view: Vec<&Block> = s.committed.iter().chain(s.provisional.iter()).collect();
        s.index = SessionIndex::build(&view, &s.user_times);
        s.sub_agents = crate::engine::session::build_sub_agents(&view);
        s
    }

    /// Finish the fold and take the [`Session`] BY MOVE — the one-shot ending every batch
    /// parse uses. Unlike [`snapshot`](Self::snapshot) (a mid-flight copy for a fold that
    /// keeps going), this transfers the committed values out and borrows content only to
    /// build the derived index — for the in-memory and `Arc` stores that is ZERO block
    /// clones for a whole-file parse.
    pub fn into_session(mut self) -> Session<S::Bv>
    where
        S: BlockRead,
    {
        let (open, user_times) = self.open_snapshot();
        // Borrowed view of committed ++ open for the derived passes (`Cow: Borrow<Block>`
        // — identity for RAM/Arc stores, a one-time decode for on-disk backings).
        let mut view: Vec<std::borrow::Cow<Block>> =
            self.committed.iter().map(|bv| self.store.get(bv)).collect();
        view.extend(open.iter().map(std::borrow::Cow::Borrowed));
        let index = SessionIndex::build(&view, &user_times);
        let sub_agents = crate::engine::session::build_sub_agents(&view);
        drop(view);
        Session {
            agent: self.agent,
            cwd: None,
            committed: std::mem::take(&mut self.committed),
            provisional: open,
            user_times,
            metrics: self.metrics.finish(),
            index,
            sub_agents,
            tasks: self.task_fold.snapshot().clone(),
        }
    }

    /// A MID-FLIGHT copy of the current state (the fold keeps going) — this clones the
    /// committed content; a finished one-shot parse should use
    /// [`into_session`](Self::into_session) instead, which moves it.
    pub fn snapshot(&mut self) -> Session<S::Bv>
    where
        S: BlockRead,
    {
        let (blocks, user_times, metrics) = self.fold();
        let index = SessionIndex::build(&blocks, &user_times);
        // Post-pass over the finished blocks (the fold is untouched): the sub-agent entity map.
        // `transcript` stays None here — the path-aware parse fills it (it alone knows the path).
        let sub_agents = crate::engine::session::build_sub_agents(&blocks);
        // The committed prefix was already `put` once (on drain). The open tail (`blocks[base..]`)
        // is the finalized-but-not-committed **provisional** turn — it is NEVER stored (kept as raw
        // `Block`s), so the store stays committed-only.
        let base = self.committed.len();
        let provisional: Vec<Block> = blocks[base..].to_vec();
        Session {
            agent: self.agent,
            cwd: None,
            committed: self.committed.clone(),
            provisional,
            user_times,
            metrics,
            index,
            sub_agents,
            tasks: self.task_fold.snapshot().clone(),
        }
    }
}

#[cfg(test)]
mod tests {}
