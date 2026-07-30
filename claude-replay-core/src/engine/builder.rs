//! **The single incremental fold orchestrator.** One `SessionAccumulator` threads everything the
//! parse paths used to thread by hand — the agent's L1 `decode_line` (with its `cwd`), the
//! shared L2 [`Replayer`] fold, and the per-agent metrics accumulator — behind one `advance`.
//!
//! Incremental parsing is first-class; one-shot is derived: the whole-file batch parse
//! ([`parse_session_as`](crate::parse_session_as) /
//! [`parse_path_timed_for`](crate::engine::replay::parse_path_timed_for)) feeds this builder
//! line-by-line (one line resident, no whole-file `Vec<String>`), and the live
//! [`FollowParser`](crate::FollowParser) feeds it the appended lines each poll — so batch and
//! live share exactly one line loop. The output is proven byte-identical to the retired
//! whole-file oracles (the equivalence gates in `claude_model`/`codex_model`) and to a full
//! re-parse (the `follow_*` tests).

use crate::adapter::{adapter, MetricsAccumulator, TranscriptAdapter};
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
    /// The FOLD cwd — threaded across lines by `decode_line` for path relativization.
    cwd: String,
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

impl SessionAccumulator<InMemoryStore> {
    /// A fresh accumulator for `agent` with the in-memory (identity) store: empty
    /// replayer/cwd/metrics, ready to `advance`.
    pub fn new(agent: Agent) -> Self {
        Self::with_store(agent, InMemoryStore)
    }
}

impl<S: BlockStore> SessionAccumulator<S> {
    /// A fresh accumulator for `agent` with an explicit [`BlockStore`]: empty
    /// replayer/cwd/metrics, ready to `advance`.
    pub fn with_store(agent: Agent, store: S) -> Self {
        let adapter = adapter(agent);
        Self {
            agent,
            adapter,
            replayer: Replayer::new(adapter.shaping()),
            cwd: String::new(),
            metrics: adapter.metrics_acc(),
            store,
            committed: Vec::new(),
            committed_meta: SessionMeta::default(),
            task_fold: crate::engine::tasks::TaskFold::default(),
        }
    }

    /// Fold ONE line into the running state, knowing its **start byte offset** in the transcript:
    /// decode it to canonical messages, stamp that offset into any content-bearing attachment's
    /// [`Deferred`](AttachmentContent::Deferred) locator (so the bytes can be re-loaded on
    /// demand), `apply` the messages to the replayer, and push the raw line's usage into metrics.
    /// The single per-line unit shared by the batch parse ([`advance_reader`](Self::advance_reader))
    /// and the live follower.
    ///
    /// Returns the replayer's back-patch signal (see [`Replayer::apply`]): the min raw-logical index
    /// of any already-emitted provisional block this line mutated in place, or `None` if it only
    /// appended. The streaming layer's pull (§9a) bumps the provisional generation when it is `Some`;
    /// batch callers ([`advance_reader`](Self::advance_reader)) ignore it. (A coincident commit — the
    /// drain below — makes it moot, since a grown committed prefix resets the provisional regardless;
    /// the caller reconciles.)
    pub fn advance_at(&mut self, offset: ByteOffset, line: &str) -> Option<usize> {
        let mut delta: Vec<Message> = Vec::new();
        self.adapter.decode_line(line, &mut self.cwd, &mut delta);
        // Stamp `offset` (and a per-line ordinal) onto every content-bearing attachment this
        // line produced — the builder is the level that knows the byte offset; the L1 decoder
        // left the locators as placeholders. Path-only (`None`) attachments are untouched.
        let mut index = 0usize;
        for m in &mut delta {
            if let Message::Attachment(a) = m {
                if let AttachmentContent::Deferred { at, index: ix } = &mut a.content {
                    *at = offset;
                    *ix = index;
                    index += 1;
                }
            }
        }
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
        // Drain the turns that just crossed the durability frontier and `put` each once — the
        // replayer drops them, keeping its content O(turn); we own the committed prefix. Fold each
        // finalized committed block into the maintained header **once**, before it's stored, so a
        // live poll reads the header without rescanning the committed prefix.
        let drained: Vec<Block> = self.replayer.drain_committed();
        if !drained.is_empty() {
            let times = self.replayer.user_times();
            for b in drained {
                self.committed_meta.push(&b);
                let at = self.committed.len();
                let bv = self.store.put(b, at, times);
                self.committed.push(bv);
            }
        }
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            self.metrics.push(&v);
        }
        patched
    }

    /// Fold a whole transcript `reader` line-by-line, tracking each line's start byte offset so
    /// attachment locators are stamped correctly. One line resident at a time (no whole-file
    /// `Vec<String>`), so a multi-gigabyte transcript never balloons into memory. The batch
    /// parse entry points feed the builder through this; the live follower uses
    /// [`advance_at`](Self::advance_at) directly with the reader's per-line offsets.
    pub fn advance_reader(&mut self, reader: &mut dyn io::BufRead) -> io::Result<()> {
        let mut offset: ByteOffset = 0;
        let mut buf = String::new();
        loop {
            buf.clear();
            let n = reader.read_line(&mut buf)?;
            if n == 0 {
                break;
            }
            let start = offset;
            offset += n as ByteOffset;
            // Match `BufRead::lines()`: strip a trailing `\n` (and a paired `\r`). `decode_line`
            // trims anyway, but this keeps the fed line byte-for-byte what the old loop produced.
            let line = buf
                .strip_suffix('\n')
                .map(|s| s.strip_suffix('\r').unwrap_or(s))
                .unwrap_or(&buf);
            self.advance_at(start, line);
        }
        Ok(())
    }

    /// Consume the accumulator, returning its [`BlockStore`]. For a tier-b store this is how a
    /// consumer reclaims the backing after the final [`snapshot`](Self::snapshot) (pair it with the
    /// `Session<Deferred>` in a [`TierBSession`](crate::engine::tier_b::TierBSession) to read blocks).
    pub fn into_store(self) -> S {
        self.store
    }

    /// How many blocks have crossed the durability frontier into `committed` — i.e. the split point
    /// between the append-only committed prefix and the open provisional turn in [`fold`](Self::fold)'s
    /// block list. Lets a live consumer locate the settled/in-flight boundary in O(1) instead of
    /// re-deriving it by scanning blocks.
    pub fn committed_len(&self) -> usize {
        self.committed.len()
    }

    /// Rebuild from scratch — recreate the replayer, clear the cwd, and take a fresh metrics
    /// accumulator. The live follower calls this on a truncation/compaction (the reader re-read
    /// from 0, so the next `advance` folds the whole new file).
    pub(crate) fn reset(&mut self) {
        self.replayer = Replayer::new(self.adapter.shaping());
        self.cwd.clear();
        self.metrics = self.adapter.metrics_acc();
        self.committed.clear();
        self.committed_meta = SessionMeta::default();
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

    /// The finalized open turn's block count — the provisional zone length a live consumer's cursor
    /// addresses. O(turn) (finalizes the open window).
    pub fn provisional_len(&self) -> usize {
        self.replayer.open_snapshot().0.len()
    }

    /// The finalized open turn (the provisional zone) + the WHOLE session's per-turn timestamps —
    /// O(turn), no committed clone. The block-level complement to
    /// [`committed_tail`](Self::committed_tail): a consumer serving the two zones separately reads
    /// `committed_tail(from)` for the settled prefix and this for the open tail.
    pub fn open_finalized(&self) -> (Vec<Block>, Vec<Option<EpochSeconds>>) {
        self.replayer.open_snapshot()
    }

    /// The live header for the current tail: the maintained **committed** meta with the finalized
    /// **open** turn folded on top — so it equals `SessionMeta::build(committed ++ provisional)`
    /// (a poll's header) without rescanning the committed prefix. O(turn).
    pub fn session_meta(&self) -> SessionMeta {
        let mut m = self.committed_meta.clone();
        for b in &self.replayer.open_snapshot().0 {
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
        let (provisional, user_times) = self.replayer.open_snapshot();
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
    pub(crate) fn fold(&self) -> (Vec<Block>, Vec<Option<EpochSeconds>>, Metrics)
    where
        S: BlockRead,
    {
        let (open, times) = self.replayer.open_snapshot();
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

    /// Finish the fold and take the [`Session`] BY MOVE — the one-shot ending every batch
    /// parse uses. Unlike [`snapshot`](Self::snapshot) (a mid-flight copy for a fold that
    /// keeps going), this transfers the committed values out and borrows content only to
    /// build the derived index — for the in-memory and `Arc` stores that is ZERO block
    /// clones for a whole-file parse.
    pub fn into_session(mut self) -> Session<S::Bv>
    where
        S: BlockRead,
    {
        let (open, user_times) = self.replayer.open_snapshot();
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
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn tmp(body: &str) -> std::path::PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "cr-builder-meta-{}-{}.jsonl",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::File::create(&p)
            .unwrap()
            .write_all(body.as_bytes())
            .unwrap();
        p
    }

    /// The maintained [`SessionMeta`] (committed folded on drain + the open turn folded on top)
    /// must equal a batch `SessionMeta::build` over the whole current block stream — at **every**
    /// step of an incremental fold, including mid-open-turn, across a sub-agent spawn, and across a
    /// commit. This is what lets a live poll read the header without rescanning the session.
    #[test]
    fn maintained_meta_equals_batch_build_at_every_step() {
        // A turn that spawns a sub-agent (Agent tool → SubAgent block), then a second user turn
        // that commits the first.
        let lines = [
            r#"{"type":"user","cwd":"/r","message":{"role":"user","content":[{"type":"text","text":"go"}]},"timestamp":"2026-07-26T10:00:00Z"}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls"}}]},"timestamp":"2026-07-26T10:00:01Z"}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"b1","content":"out"}]},"timestamp":"2026-07-26T10:00:02Z"}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_A","name":"Task","input":{"subagent_type":"general-purpose","description":"child","prompt":"go"}}]},"timestamp":"2026-07-26T10:00:03Z"}"#,
            r#"{"type":"user","toolUseResult":{"agentId":"achild01","status":"completed"},"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_A","content":"done"}]},"timestamp":"2026-07-26T10:00:04Z"}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"next"}]},"timestamp":"2026-07-26T10:00:05Z"}"#,
        ];
        let mut acc = SessionAccumulator::new(Agent::Claude);
        let mut off: crate::model::ByteOffset = 0;
        for line in lines {
            acc.advance_at(off, line);
            off += line.len() as u64 + 1;
            // The maintained header == a batch build over the whole current block stream.
            let blocks = acc.snapshot().blocks();
            assert_eq!(
                acc.session_meta(),
                SessionMeta::build(&blocks),
                "maintained meta diverged from batch build"
            );
        }
        // Final shape: 2 user turns, 2 tool calls (Bash + Task counts as... Task is a spawn ⇒ NOT
        // a tool), one child.
        let meta = acc.session_meta();
        assert_eq!(meta.turns, 2, "two user turns");
        assert_eq!(
            meta.tools, 1,
            "Bash is a tool; the Task spawn is a child, not a tool"
        );
        assert_eq!(meta.children.len(), 1);
        assert_eq!(meta.children[0].id, "achild01");
    }

    /// #56 regression (the QoderWork panic): ONE transcript line can carry SEVERAL user text
    /// items — the second turn then closes the first within the same fold batch, committing it
    /// before any later `LineStart` stamps its timestamp. The drain must stamp first: no lost
    /// per-turn timestamp, `stamped` never falls behind `base`, and no window-math wrap — with a
    /// foreign head line (`runtime-config`) and an unknown mid-stream type thrown in, since a
    /// parser fed arbitrary JSONL must degrade, never panic.
    #[test]
    fn multi_text_user_line_commits_stamped_and_never_panics() {
        use crate::model::Block;
        let lines = [
            r#"{"type":"runtime-config","sessionId":"s","model":"qwork-ultimate","timestamp":1785068132048}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"<system-reminder>env</system-reminder>"},{"type":"text","text":"the real prompt"}]},"timestamp":"2026-07-26T12:15:33.904Z"}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"reply"}]},"timestamp":"2026-07-26T12:15:40Z"}"#,
            r#"{"type":"totally-unknown","x":1}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"next"}]},"timestamp":"2026-07-26T12:16:00Z"}"#,
        ];
        let mut acc = SessionAccumulator::new(Agent::Claude);
        let mut off: crate::model::ByteOffset = 0;
        for line in lines {
            acc.advance_at(off, line);
            off += line.len() as u64 + 1;
            let _ = acc.snapshot(); // the panic site was the snapshot's window math
        }
        let s = acc.snapshot();
        let users = s
            .blocks()
            .iter()
            .filter(|b| matches!(b, Block::UserText(_)))
            .count();
        assert_eq!(
            users, 3,
            "two turns from the multi-text line + the follow-up"
        );
        assert_eq!(s.user_times.len(), 3, "one timestamp per user turn");
        assert!(
            s.user_times.iter().all(|t| t.is_some()),
            "no turn lost its stamp to an early commit: {:?}",
            s.user_times
        );
        assert_eq!(
            s.user_times[0], s.user_times[1],
            "both turns from the one line carry its timestamp"
        );
    }

    /// The queued-marker lifecycle (#52): a `⧗ queued:` marker lives only while its prompt is
    /// PENDING — it collapses on ANY pop, matching Claude Code's "only the one message":
    /// (a) immediate enqueue → dequeue → delivery; (b) TYPE-AHEAD (agent content between enqueue
    /// and dequeue — previously kept the marker, the #52 duplicate); (c) a `remove` op (the user
    /// withdrew it); (d) an OP-LESS delivery — a user message whose text matches the pending
    /// content (a jdi restart re-delivers without writing the dequeue), which must also DRAIN
    /// the queue so the durability frontier un-pins and turns keep committing.
    #[test]
    fn queued_marker_collapses_on_every_pop_and_opless_delivery() {
        use crate::model::Block;
        let user = |t: &str| {
            format!(
                r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"{t}"}}]}},"timestamp":"2026-07-26T10:00:00Z"}}"#
            )
        };
        let asst = |t: &str| {
            format!(
                r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"{t}"}}]}},"timestamp":"2026-07-26T10:00:01Z"}}"#
            )
        };
        let enq = |t: &str| {
            format!(r#"{{"type":"queue-operation","operation":"enqueue","content":"{t}"}}"#)
        };
        let markers = |acc: &mut SessionAccumulator| {
            acc.snapshot()
                .blocks()
                .iter()
                .filter(|b| matches!(b, Block::QueueEvent { .. }))
                .count()
        };
        let drive = |lines: &[String]| {
            let mut acc = SessionAccumulator::new(Agent::Claude);
            let mut off: crate::model::ByteOffset = 0;
            for l in lines {
                acc.advance_at(off, l);
                off += l.len() as u64 + 1;
            }
            acc
        };

        // (a) immediate pickup ⇒ collapsed.
        let mut a = drive(&[
            user("go"),
            asst("working"),
            enq("next thing"),
            r#"{"type":"queue-operation","operation":"dequeue"}"#.into(),
            user("next thing"),
        ]);
        assert_eq!(markers(&mut a), 0, "immediate pickup collapses");

        // (b) TYPE-AHEAD: agent content between enqueue and dequeue ⇒ STILL collapsed (the fix).
        let mut b = drive(&[
            user("go"),
            enq("typed ahead"),
            asst("kept working"),
            asst("more work"),
            r#"{"type":"queue-operation","operation":"dequeue"}"#.into(),
            user("typed ahead"),
        ]);
        assert_eq!(
            markers(&mut b),
            0,
            "type-ahead delivery collapses too (CC parity)"
        );

        // (c) remove ⇒ collapsed (the prompt was withdrawn; CC drops the bubble).
        let mut c = drive(&[
            user("go"),
            enq("changed my mind"),
            asst("working"),
            r#"{"type":"queue-operation","operation":"remove","content":"changed my mind"}"#.into(),
        ]);
        assert_eq!(markers(&mut c), 0, "removed prompt leaves no marker");

        // (d) op-less delivery: the matching user message pops the item, collapses the marker,
        // and DRAINS the queue — so later turns commit (the frontier un-pins).
        let mut d = drive(&[
            user("go"),
            enq("restart prompt"),
            asst("killed here"),
            user("restart prompt"),
            asst("resumed"),
            user("later turn"),
            asst("done"),
        ]);
        assert_eq!(markers(&mut d), 0, "op-less delivery collapses the marker");
        assert!(
            d.committed_len() > 0,
            "queue drained ⇒ the durability frontier advanced past the delivered turns"
        );
    }

    /// The task op-log end-to-end (#15): real transcript lines with
    /// `TaskCreate`/`TaskUpdate` calls fold into `Session.tasks` — the create's id
    /// joined from its RESULT text, updates applied by task id — while the BLOCK
    /// stream is untouched (task tools still render as ordinary ToolUse blocks).
    #[test]
    fn task_ops_fold_into_session_tasks() {
        use crate::engine::tasks::TaskStatus;
        let lines = [
            r#"{"type":"user","timestamp":"2026-07-29T10:00:00Z","message":{"role":"user","content":"go"}}"#.to_string(),
            r#"{"type":"assistant","timestamp":"2026-07-29T10:00:05Z","message":{"content":[{"type":"tool_use","id":"tc1","name":"TaskCreate","input":{"subject":"fix the parser","description":"long details","activeForm":"Fixing the parser"}}]}}"#.to_string(),
            r#"{"type":"user","timestamp":"2026-07-29T10:00:06Z","message":{"content":[{"type":"tool_result","tool_use_id":"tc1","content":"Created task #12: fix the parser"}]}}"#.to_string(),
            r#"{"type":"assistant","timestamp":"2026-07-29T10:00:10Z","message":{"content":[{"type":"tool_use","id":"tu1","name":"TaskUpdate","input":{"taskId":"12","status":"in_progress"}}]}}"#.to_string(),
            r#"{"type":"user","timestamp":"2026-07-29T10:00:11Z","message":{"content":[{"type":"tool_result","tool_use_id":"tu1","content":"Updated task #12 status"}]}}"#.to_string(),
        ];
        let mut acc = SessionAccumulator::new(Agent::Claude);
        let mut off: ByteOffset = 0;
        for l in &lines {
            acc.advance_at(off, l);
            off += l.len() as u64 + 1;
        }
        let s = acc.snapshot();
        assert_eq!(s.tasks.items.len(), 1, "{:?}", s.tasks);
        let t = &s.tasks.items[0];
        assert_eq!(t.id, "12");
        assert_eq!(t.subject, "fix the parser");
        assert_eq!(t.description, "long details");
        assert_eq!(t.active_form, "Fixing the parser");
        assert_eq!(t.status, TaskStatus::InProgress);
        // The block stream still shows the two tool calls as ordinary blocks.
        let tools = s
            .blocks()
            .iter()
            .filter(|b| matches!(b, Block::ToolUse { .. } | Block::Thinking { .. }))
            .count();
        assert!(tools >= 1, "task tools still render as blocks");
    }

    /// A truncation/rewrite resets the maintained committed meta (no stale carry-over).
    #[test]
    fn reset_clears_committed_meta() {
        let a = r#"{"type":"user","cwd":"/r","message":{"role":"user","content":[{"type":"text","text":"a"}]},"timestamp":"2026-07-26T10:00:00Z"}"#;
        let b = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"b"}]},"timestamp":"2026-07-26T10:00:01Z"}"#;
        let path = tmp(&format!("{a}\n{b}\n"));
        let mut fp = crate::FollowParser::open(Agent::Claude, &path);
        fp.advance_stream().unwrap();
        assert_eq!(fp.stream_read(0).meta.turns, 2);
        // Rewrite to a single turn → reset → meta rebuilt from scratch.
        std::fs::write(&path, format!("{a}\n")).unwrap();
        fp.advance_stream().unwrap();
        assert_eq!(fp.stream_read(0).meta.turns, 1, "reset rebuilt the header");
        let _ = std::fs::remove_file(&path);
    }
}
