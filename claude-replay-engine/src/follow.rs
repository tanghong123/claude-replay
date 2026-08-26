//! Incremental live follower (M16): fold only the newly-appended lines through a persistent
//! `Replayer`, instead of re-parsing the whole file each poll. The `snapshot` is proven
//! byte-identical to a full re-parse (`follow_matches_full_reparse`), at O(delta) work per
//! poll — no whole-file re-read/re-decode. On a truncation/rewrite (compaction) the tail
//! resets and the Replayer rebuilds from scratch (a full replay of the new content).

use std::path::{Path, PathBuf};

use crate::engine::builder::{SessionAccumulator, StreamRead};
use crate::engine::reader::{ElisionCounts, LineSource, TornTail};
use crate::engine::session::{BlockStore, InMemoryStore, Session};
use crate::metrics::Metrics;
use crate::model::{Block, EpochSeconds};
use crate::Agent;
use std::io::{BufReader, Seek, SeekFrom};

/// One `poll_delta` tick's payload: `(blocks, user_times, metrics, changed_from)`.
pub type PollDelta = (Vec<Block>, Vec<Option<EpochSeconds>>, Metrics, usize);

/// The outcome of one advance-from-source: whether content moved, whether it was a reset
/// (truncation/rewrite), and the batch's back-patch signal (`patch_floor`).
struct Tick {
    advanced: bool,
    reset: bool,
    patch_floor: Option<usize>,
}

impl Tick {
    fn idle() -> Self {
        Tick {
            advanced: false,
            reset: false,
            patch_floor: None,
        }
    }
}

/// Follows a transcript file, folding only newly-appended lines each poll through a shared
/// [`SessionAccumulator`]. Everything agent-specific — the L1 decoder, the L2 `Shaping`, the
/// metrics accumulator — lives in the accumulator, so the follower itself is agent-agnostic and
/// is just the byte-offset reader plus the same incremental fold the batch parse uses.
///
/// Generic over the accumulator's [`BlockStore`] `S` (default [`InMemoryStore`] — blocks resident
/// in RAM). The **light streaming surface** (`advance_stream` / `stream_read` / the accessors)
/// works for any store — a live server follows with a tier-b store so committed content lives on
/// disk; the `Session`-assembling polls (`poll*`) stay on the in-memory default.
pub struct FollowParser<S: BlockStore = InMemoryStore> {
    agent: Agent,
    adapter: &'static dyn crate::adapter::TranscriptAdapter,
    path: PathBuf,
    builder: SessionAccumulator<S>,
    /// Cursor into the file: the next unread byte. The live source is (re)opened lazily and
    /// kept across polls (#193 §9.1 — `LineReader` dissolved into this).
    cursor: crate::model::ByteOffset,
    src: Option<LineSource<BufReader<std::fs::File>>>,
    /// Gauges already banked into the metrics accumulator; each poll banks the delta.
    banked: ElisionCounts,
    /// Previous poll's committed length + open-turn blocks — the O(turn) state
    /// [`poll_delta`](Self::poll_delta) diffs against to report `changed_from`. Only touched by
    /// `poll_delta`.
    prev_committed: usize,
    prev_provisional: Vec<Block>,
}

/// Length of the shared prefix of two block slices — O(min(len)) block-equality.
fn common_prefix_len(a: &[Block], b: &[Block]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

impl FollowParser<InMemoryStore> {
    /// Follow `path` from the beginning: the first `poll` folds the whole current file, then
    /// subsequent polls fold only appends. Takes the agent's resolved adapter — the facade's
    /// `Transcript::follow` (or `core::adapter(agent)`) supplies it (#87 step 3).
    pub fn open(adapter: &'static dyn crate::adapter::TranscriptAdapter, path: &Path) -> Self {
        Self::with_store(adapter, path, InMemoryStore)
    }
}

impl<S: BlockStore> FollowParser<S> {
    /// [`open`](FollowParser::open) with an explicit [`BlockStore`] — e.g. a
    /// [`TierBStore`](crate::engine::tier_b::TierBStore) so committed block content lives in the
    /// store's backing (disk) instead of RAM.
    pub fn with_store(
        adapter: &'static dyn crate::adapter::TranscriptAdapter,
        path: &Path,
        store: S,
    ) -> Self {
        let mut builder = SessionAccumulator::with_store(adapter, store);
        // BEFORE the first poll, which folds the whole current file: a committed block is never
        // revisited, so a spawn whose turn had already closed would stay anonymous for the life
        // of this follower. Free for an agent that names its children in-band.
        builder.set_spawn_links(adapter.spawn_links(path));
        Self {
            agent: adapter.agent(),
            adapter,
            path: path.to_path_buf(),
            builder,
            cursor: 0,
            src: None,
            banked: ElisionCounts::default(),
            prev_committed: 0,
            prev_provisional: Vec::new(),
        }
    }

    /// Follow `path` from a **restored** fold (#96 §6.3): the accumulator is rebuilt from the
    /// durable cache, and the reader starts at `resume.replay_from` — so the bytes below it are
    /// never re-read, which is the whole point of the resume.
    ///
    /// `prev_committed` starts at the restored length rather than zero. Left at zero, the first
    /// poll would report every restored block as changed and the frontend would re-render the
    /// entire session — a resume that loads instantly and then repaints everything is not a
    /// resume. `prev_provisional` stays empty because it is: by §3's partition, nothing above
    /// `replay_from` has been folded yet.
    pub fn resume(
        adapter: &'static dyn crate::adapter::TranscriptAdapter,
        path: &Path,
        store: S,
        committed: Vec<S::Bv>,
        mm: crate::engine::meta_stream::MaterializedMeta,
        resume: &crate::engine::meta_stream::Resume,
    ) -> Self {
        let prev_committed = committed.len();
        let mut builder = SessionAccumulator::restore(adapter, store, committed, mm, resume);
        // Same reason as `with_store`: the replay from `replay_from` is this follower's first
        // fold, and whatever it commits it commits once.
        builder.set_spawn_links(adapter.spawn_links(path));
        Self {
            agent: adapter.agent(),
            adapter,
            path: path.to_path_buf(),
            builder,
            cursor: resume.replay_from,
            src: None,
            banked: ElisionCounts::default(),
            prev_committed,
            prev_provisional: Vec::new(),
        }
    }

    /// Fold any newly-appended lines (or rebuild on a truncation/rewrite) into the running
    /// builder. Returns a [`Tick`]: whether content advanced, whether it was a reset (truncation),
    /// and the batch's back-patch signal (the min already-emitted index any line mutated in place,
    /// `None` if append-only — see [`SessionAccumulator::advance_at`](crate::SessionAccumulator)).
    /// O(delta) except on a rewrite.
    fn advance_from_source(&mut self) -> std::io::Result<Tick> {
        // Truncation / compaction: the file shrank below the cursor — the kept prefix
        // changed. Rebuild from scratch (fresh metrics accumulator too, so the gauge bank
        // restarts with it) and re-fold the whole new file.
        let len = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        let cursor = self.src.as_ref().map(|s| s.offset()).unwrap_or(self.cursor);
        let reset = len < cursor;
        if reset {
            self.builder.reset();
            self.src = None;
            self.cursor = 0;
            self.banked = ElisionCounts::default();
        }
        match self.src.as_mut() {
            None => {
                // (Re)open — lazily on the first poll, after a reset, or if the file was
                // briefly unopenable. A missing file is an idle tick, exactly as before.
                let Ok(f) = std::fs::File::open(&self.path) else {
                    return Ok(if reset {
                        Tick {
                            advanced: true,
                            reset: true,
                            patch_floor: None,
                        }
                    } else {
                        Tick::idle()
                    });
                };
                let mut r = BufReader::new(f);
                r.seek(SeekFrom::Start(self.cursor))?;
                self.src = Some(LineSource::new(
                    r,
                    self.cursor,
                    TornTail::Stop,
                    self.adapter.elision(),
                ));
            }
            // The reader consumed torn bytes discovering the last tail — walk it back to
            // the cursor so a completed line is re-read WHOLE (§9.1).
            Some(src) => src.rewind_to_cursor()?,
        }
        let src = self.src.as_mut().expect("just ensured");
        // Fold each appended line with its file start offset, so attachment locators in a
        // LIVE session get correct byte offsets (same as a batch parse). OR-reduce the
        // per-line back-patch reports into the batch's min.
        let mut advanced = false;
        let mut patch_floor: Option<usize> = None;
        while let Some((offset, line)) = src.next()? {
            advanced = true;
            if let Some(i) = self.builder.advance_at(offset, line) {
                patch_floor = Some(patch_floor.map_or(i, |cur| cur.min(i)));
            }
        }
        self.cursor = src.offset();
        let counts = src.elided;
        let delta = ElisionCounts {
            elided_lines: counts.elided_lines - self.banked.elided_lines,
            elided_bytes: counts.elided_bytes - self.banked.elided_bytes,
            skipped_lines: counts.skipped_lines - self.banked.skipped_lines,
        };
        self.banked = counts;
        self.builder.bank_elision(delta);
        if !reset && !advanced {
            return Ok(Tick::idle()); // nothing new this tick
        }
        // The spawn-id table (#37) is refreshed per advancing poll: a sidecar is written a beat
        // after the line that spawned it, so a read from before the fold can miss the very child
        // this poll revealed — the next poll picks it up, and the open turn adopts at read time
        // meanwhile. Free for an agent that names its children in-band.
        self.builder
            .set_spawn_links(self.adapter.spawn_links(&self.path));
        Ok(Tick {
            advanced: true,
            reset,
            patch_floor,
        })
    }

    /// Poll: fold any newly-appended lines and return the current blocks + per-turn times +
    /// metrics. Returns `None` when nothing changed since the last poll (the common idle tick).
    #[allow(clippy::type_complexity)]
    pub fn poll(
        &mut self,
    ) -> std::io::Result<Option<(Vec<Block>, Vec<Option<EpochSeconds>>, Metrics)>>
    where
        S: crate::engine::session::BlockRead,
    {
        Ok(self
            .advance_from_source()?
            .advanced
            .then(|| self.builder.fold()))
    }

    /// Like [`poll`](Self::poll), but also returns **`changed_from`** — the first block index that
    /// differs from the previous poll, so a windowed/cached consumer keeps its fold-state and render
    /// cache for `[0..changed_from]` and rebuilds only the rest. The engine computes it in **O(turn)**
    /// from its own committed/provisional structure (committed is append-only ⇒ unchanged below the
    /// prior committed length; only the open turn is re-derived), instead of the consumer re-scanning
    /// the whole block list for the common prefix. `None` on an idle tick.
    #[allow(clippy::type_complexity)]
    /// The current task op-log state (#15) — for consumers that refresh a panel
    /// alongside `poll_delta` without assembling a session.
    /// Take the meta records the fold has authored since the last call (#96 §6.1) — the durable
    /// cache appends them; a cache-less run simply never asks and they are dropped.
    pub fn drain_meta(&mut self) -> Vec<crate::engine::meta_stream::MetaRecord> {
        self.builder.drain_meta()
    }

    pub fn tasks(&self) -> crate::engine::tasks::TaskList {
        self.builder.tasks().clone()
    }

    pub fn poll_delta(&mut self) -> std::io::Result<Option<PollDelta>>
    where
        S: crate::engine::session::BlockRead,
    {
        let tick = self.advance_from_source()?;
        if !tick.advanced {
            return Ok(None);
        }
        let (blocks, times, metrics) = self.builder.fold();
        let n_committed = self.builder.committed_len();
        let changed_from = if tick.reset {
            0 // truncation/rewrite ⇒ everything changed
        } else {
            // Committed is append-only, so blocks below the *prior* committed length are unchanged.
            // The exact common prefix beyond it — whether the open turn appended, back-patched, or
            // committed (its blocks finalized/grouped into the new committed tail) — is one
            // `common_prefix` over the small open-turn region: O(turn), not O(N).
            self.prev_committed
                + common_prefix_len(&self.prev_provisional, &blocks[self.prev_committed..])
        };
        self.prev_committed = n_committed;
        self.prev_provisional = blocks[n_committed..].to_vec();
        Ok(Some((blocks, times, metrics, changed_from)))
    }

    /// The **light** streaming advance (the pull path): fold the delta and report only the two
    /// protocol signals — `(reset, patch_floor)` — **without** assembling a `Session` (no O(N)
    /// `index`/`sub_agents` build, no whole-committed clone). `None` on an idle tick. The caller
    /// reads the delta-sized state afterward via [`stream_read`](Self::stream_read) /
    /// [`committed_len`](Self::committed_len) / [`provisional_len`](Self::provisional_len).
    pub fn advance_stream(&mut self) -> std::io::Result<Option<(bool, Option<usize>)>> {
        let tick = self.advance_from_source()?;
        Ok(tick.advanced.then_some((tick.reset, tick.patch_floor)))
    }

    /// Committed count (the append-only prefix length) — O(1).
    pub fn committed_len(&self) -> usize {
        self.builder.committed_len()
    }

    /// Finalized open-turn (provisional) length — O(turn).
    pub fn provisional_len(&self) -> usize {
        self.builder.provisional_len()
    }

    /// The delta-sized read for one pull (see [`StreamRead`]): `committed[from..]` + the open turn +
    /// times + metrics + the live header, from a single finalize. Copies only the committed tail.
    pub fn stream_read(&self, from: usize) -> StreamRead
    where
        S: crate::engine::session::BlockRead,
    {
        self.builder.stream_read(from)
    }

    /// [`stream_read`](Self::stream_read) without the committed content — O(turn) and
    /// store-agnostic (a projection-store session serves committed from its own form, #74).
    pub fn open_read(&self) -> StreamRead {
        self.builder.open_read()
    }

    /// `committed[from..]` as owned blocks — O(delta), never the whole committed prefix.
    pub fn committed_tail(&self, from: usize) -> Vec<Block>
    where
        S: crate::engine::session::BlockRead,
    {
        self.builder.committed_tail(from)
    }

    /// The finalized open turn + the whole session's per-turn timestamps — O(turn).
    pub fn open_finalized(&self) -> (Vec<Block>, Vec<Option<EpochSeconds>>) {
        self.builder.open_finalized()
    }

    /// The maintained live header for the current tail (committed meta + open turn) — O(turn).
    pub fn session_meta(&self) -> crate::engine::session::SessionMeta {
        self.builder.session_meta()
    }

    /// The committed locator/value slice (`&[S::Bv]`, no decode) — see
    /// [`SessionAccumulator::committed`](crate::engine::SessionAccumulator::committed).
    pub fn committed(&self) -> &[S::Bv] {
        self.builder.committed()
    }

    /// The follower's storage policy (store-level facts, e.g. a tier-b backing's length).
    pub fn store(&self) -> &S {
        self.builder.store()
    }

    /// The transcript path this follower tails.
    /// The agent whose adapter this follower folds with.
    pub fn agent(&self) -> crate::Agent {
        self.agent
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// The `Session`-assembling polls — only for the in-memory store, whose `Bv` **is** [`Block`]
/// (a `Session<Deferred>` consumer reads through the light streaming surface instead).
impl FollowParser<InMemoryStore> {
    /// Poll and return the current state as a **fully-assembled** owned [`Session`] — blocks +
    /// per-turn times + metrics + derived index + sub-agent map, with `cwd` and each sub-agent's
    /// `transcript` filled from the source path (exactly as
    /// the facade's `parse_session_as` does). Returns `None` when the source hasn't
    /// grown since the last poll (idle). This is the residency cache's single assembly point — it
    /// needs no core internals.
    pub fn poll_session(&mut self) -> std::io::Result<Option<Session>> {
        Ok(self.poll_shared()?.map(|(s, _reset, _patch_floor)| s))
    }

    /// Like [`poll_session`](Self::poll_session), but also returns the streaming signals the
    /// present-layer pull protocol needs: `reset` (a truncation happened this tick ⇒ bump the
    /// session epoch) and `patch_floor` (the batch back-patched an already-emitted open-turn block
    /// ⇒ bump the provisional generation; `None` = append-only). The `Session` carries the
    /// committed / provisional split the pull serves. Returns `None` on an idle tick.
    #[allow(clippy::type_complexity)]
    pub fn poll_shared(&mut self) -> std::io::Result<Option<(Session, bool, Option<usize>)>> {
        let tick = self.advance_from_source()?;
        if !tick.advanced {
            return Ok(None);
        }
        let mut s = self.builder.snapshot();
        s.cwd = crate::discover::first_cwd(&self.path);
        crate::engine::session::populate_sub_agent_transcripts(
            self.adapter,
            &self.path,
            &mut s.sub_agents,
        );
        Ok(Some((s, tick.reset, tick.patch_floor)))
    }
}

#[cfg(test)]
mod tests {}
