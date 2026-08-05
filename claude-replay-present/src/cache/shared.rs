//! `SharedSession` — the present-layer state the pull protocol (`super::stream`) serves against.
//!
//! It owns the follower over a source and maintains the two zones the cursor addresses — the
//! append-only `committed` prefix and the current-turn `provisional` — plus the two protocol
//! counters (`epoch`, `provisional_gen`). [`advance`](SharedSession::advance) borrows the caller's
//! thread to tail the source (fold appended lines), then updates the zones and bumps the counters
//! per design §9a:
//!
//! - **`epoch`** bumps on a **reset** (truncation/rewrite) — outstanding cursors then resync.
//! - **`provisional_gen`** bumps whenever the **finalized** provisional the client received is
//!   no longer a prefix of the current one: a **commit** (the committed prefix grew), an
//!   **in-place back-patch** (the follower's `patch_floor` signal), or a **finalization
//!   reshape** (activity grouping / thinking absorption rewriting the prefix on what was a
//!   raw-level pure append — detected by diffing against the previous tick's finalized view,
//!   #54). A pure append that leaves the finalized prefix intact never bumps it.
//!
//! [`pull`](SharedSession::pull) then answers any client [`Cursor`] against the current zones.
//!
//! Concurrency (§9a): the state is interior-mutable behind one `Mutex`, so a `SharedSession` wraps
//! in an `Arc` and any number of client threads call [`pull`](SharedSession::pull) / [`advance`](SharedSession::advance)
//! (SharedSession::advance) on `&self`. Advancing is **borrow-to-tail**: the thread that folds new
//! source lines is a client's own (the HTTP `/pull` handler calls `advance` then `pull`) — the
//! cache owns no background thread. A pull is O(delta): fold (idle-cheap when nothing new) + a
//! zone-slice copy, all under the brief lock.
//!
//! On the single `Mutex` vs the design's "lock-free committed": every pull reads the *provisional*
//! zone (to serve it), which lives with the tail, so a pull always touches the tail regardless —
//! lock-free committed reads would help only a *tail-bypassing historical range-read* API, which
//! does not exist yet. So one `Mutex` is the honest fit for the current pull-only access; a
//! lock-free committed volume is a later optimization gated on that future API.

use std::path::Path;
use std::sync::Mutex;

use crate::engine::session::{BlockRead, BlockStore, InMemoryStore};
use crate::engine::SessionMeta;
use crate::follow::FollowParser;
use crate::metrics::Metrics;
use crate::model::{Block, EpochSeconds};
use crate::pull::{pull_indices, Cursor, PullReply};
use crate::Agent;

/// A consistent, delta-sized read of everything one `/pull` render needs, taken under a single
/// lock so the counters match the blocks — **without** the whole-session block clone the retired
/// `render_snapshot` paid per poll. `committed_delta` is only `committed[from..]` (the blocks past
/// what the caller already rendered — `from` comes from the caller's render-cache state, see
/// [`pull_delta`](SharedSession::pull_delta)); `provisional` is the open turn (O(turn)); `meta` is
/// the accumulator-**maintained** live header (never rescanned). So the returned object's size is
/// O(tail delta), not O(session).
pub struct PullDelta {
    pub epoch: u64,
    pub provisional_gen: u64,
    /// Whether the session epoch moved past the caller's (`prev_epoch`): its render cache is stale
    /// — discard it; `committed_delta` then restarts from 0.
    pub reset: bool,
    /// Current committed count (`committed_delta` ends here).
    pub n_committed: usize,
    /// `committed[from..]` — `from` = 0 on `reset`, else the caller's already-rendered count.
    pub committed_delta: Vec<Block>,
    /// The finalized open turn — O(turn).
    pub provisional: Vec<Block>,
    /// The WHOLE session's per-turn timestamps (the renderer indexes into them by turn).
    pub user_times: Vec<Option<EpochSeconds>>,
    pub metrics: Metrics,
    /// The maintained live header (turns / tools / children) — matches `committed ++ provisional`.
    pub meta: SessionMeta,
    /// The task op-log state (#15) — from the live fold, or the hibernated sidecar.
    pub tasks: crate::engine::TaskList,
}

/// One in-process tick's payload (#85): the splice-shaped delta an interactive view
/// applies, plus the chrome state (times, metrics, tasks) — everything a frontend's tick
/// needs from ONE call. Committed blocks are `Arc` clones of the cache-owned copy.
pub struct ViewDelta {
    pub reset: bool,
    /// `committed[prev..]` as `Arc` clones — shared content, cheap to hand over.
    pub committed_delta: Vec<std::sync::Arc<Block>>,
    /// Total committed after this delta; the consumer splices at
    /// `committed_len - committed_delta.len()`.
    pub committed_len: usize,
    /// The finalized open turn, freshly wrapped (O(turn) small allocations per tick).
    pub provisional: Vec<std::sync::Arc<Block>>,
    /// First index (in the joined `committed ++ open` view) the consumer must re-derive.
    pub changed_from: usize,
    pub user_times: Vec<Option<EpochSeconds>>,
    pub metrics: Metrics,
    /// The session's task op-log state (#15) — refreshed with the same tick.
    pub tasks: crate::engine::TaskList,
}

/// The mutable state of one followed session, guarded as a unit by [`SharedSession`]'s `Mutex`.
/// The block state lives in the follower's accumulator and is never cloned whole; this adds only
/// the two protocol counters and a cached zone length.
///
/// There is exactly **one** shape here. A session used to be either live or *hibernated* — a
/// frozen materialization that served reads but could never fold again, so a source that grew
/// forced a full re-fold. #96 replaces it: a restored session is an ordinary live follower whose
/// reader starts above the resume point, so it keeps folding where it left off.
struct Inner<S: BlockStore> {
    // Boxed: a follower (reader buffers + accumulator) dwarfs the rest of this struct.
    follower: Box<FollowParser<S>>,
    /// Session validity token. A client cursor with a stale `epoch` resyncs. Starts at 1 so a
    /// default (`epoch == 0`) cursor from a fresh client mismatches and resyncs on its first pull.
    epoch: u64,
    /// The current open-turn generation — see the module docs / `super::stream`.
    provisional_gen: u64,
    /// The finalized open-turn length, refreshed on each advancing tick — so [`counters`](SharedSession::counters)
    /// (SharedSession::counters), the per-poll idle check, stays O(1) instead of re-finalizing
    /// the open window on every quiet poll.
    n_provisional: usize,
    /// The previous tick's FINALIZED provisional — the reference for the reshape check in
    /// [`advance`](SharedSession::advance). The raw `patch_floor` signal alone is not enough:
    /// finalization (activity grouping, thinking absorption, run coalescing) can change the
    /// served provisional's PREFIX on a raw-level pure append, and the protocol's "append-only
    /// within a generation" promise is about what the client actually received — the finalized
    /// view. O(turn), same class as the follower's own `poll_delta` state.
    prev_provisional: Vec<Block>,
    /// The durable record writer, when this session is a durable cache's (#96). It lives HERE,
    /// under the same lock as the fold, so records are appended in the same critical section
    /// that produced them: content reaches disk before the meta describing it (I1's ordering),
    /// and there is no "remember to record after advancing" for a caller to get wrong.
    meta: Option<super::stream::MetaWriter>,
}

impl<S: BlockStore> Inner<S> {
    /// Append whatever the fold just authored. Errors are swallowed: a stream that cannot be
    /// written costs a longer resume next time, never a wrong session.
    fn record(&mut self) {
        let Some(w) = self.meta.as_mut() else { return };
        for r in &self.follower.drain_meta() {
            let _ = w.append(r);
        }
        let _ = w.flush();
    }
}

/// The live, pull-servable state of one followed session (see the module docs). `Arc`-shareable;
/// all methods take `&self`. Generic over the follower's [`BlockStore`] `S` (default
/// [`InMemoryStore`]): a live server opens its residents with a tier-b store so committed block
/// content lives on disk — every method here rides the follower's light streaming surface, which
/// is store-agnostic.
pub struct SharedSession<S: BlockStore = InMemoryStore> {
    inner: Mutex<Inner<S>>,
}

impl SharedSession<InMemoryStore> {
    /// Open a shared session following `path` for `agent`. The first [`advance`](Self::advance)
    /// folds the current file; later ones fold only appends.
    pub fn open(agent: Agent, path: &Path) -> Self {
        Self::with_store(agent, path, InMemoryStore)
    }
}

impl<S: BlockStore> SharedSession<S> {
    /// [`open`](SharedSession::open) with an explicit [`BlockStore`] — e.g.
    /// [`TierBStore::file`](crate::engine::tier_b::TierBStore::file) so the committed content of a
    /// followed session never sits in RAM.
    pub fn with_store(agent: Agent, path: &Path, store: S) -> Self {
        Self {
            inner: Mutex::new(Inner {
                follower: Box::new(FollowParser::with_store(
                    claude_replay_core::adapter(agent),
                    path,
                    store,
                )),
                epoch: 1,
                provisional_gen: 0,
                n_provisional: 0,
                prev_provisional: Vec::new(),
                meta: None,
            }),
        }
    }

    /// Borrow the caller's thread to tail the source: fold any newly-appended lines and advance
    /// `epoch` / `provisional_gen` per §9a (see the module docs). Returns `true` when content
    /// advanced, `false` on an idle tick. Uses the follower's **light** streaming advance — no
    /// `Session` assembly (no O(N) index/sub-agent build, no whole-committed clone); the zones
    /// stay owned by the accumulator and are read delta-sized at pull time.
    pub fn advance(&self) -> std::io::Result<bool> {
        let mut g = super::lock_recover(&self.inner);
        let follower = &mut g.follower;
        // A commit is visible as growth of the append-only committed prefix (compare BEFORE the
        // fold). Reset takes priority (it also invalidates committed).
        let prev_committed = follower.committed_len();
        let Some((reset, patch_floor)) = follower.advance_stream()? else {
            return Ok(false);
        };
        let committed_grew = follower.committed_len() > prev_committed;
        // The reshape check (#54): the gen's append-only promise is about the FINALIZED
        // provisional the client received. Finalization can rewrite the prefix on a raw-level
        // pure append (a new tool joins an activity group and absorbs earlier blocks), which
        // `patch_floor` — a raw-buffer signal — never sees. So diff the finalized view against
        // last tick's: any changed/shrunk prefix forces a gen bump (whole-provisional resend).
        let (provisional, _times) = follower.open_finalized();
        let prefix_intact = g.prev_provisional.len() <= provisional.len()
            && g.prev_provisional
                .iter()
                .zip(&provisional)
                .all(|(a, b)| a == b);
        if reset {
            g.epoch += 1;
            g.provisional_gen += 1;
        } else if committed_grew || patch_floor.is_some() || !prefix_intact {
            g.provisional_gen += 1;
        }
        g.n_provisional = provisional.len();
        g.prev_provisional = provisional;
        g.record();
        Ok(true)
    }

    /// The in-process consumer's ONE tick call (#85): borrow-to-tail advance plus the
    /// splice-shaped delta plus everything the frontend's chrome needs (times, metrics,
    /// tasks) — under a single lock, with the change boundary and the wire protocol's
    /// epoch/gen updates derived from the SAME reshape comparison (one implementation of
    /// "what changed since last tick" for both consumption styles). `Ok(None)` on idle.
    /// Committed blocks arrive as `Arc` clones of the RETAINED authoritative vector: one
    /// content copy in the process, shared by reference.
    pub fn poll_view(&self) -> std::io::Result<Option<ViewDelta>>
    where
        S: BlockStore<Bv = std::sync::Arc<Block>>,
    {
        let mut g = super::lock_recover(&self.inner);
        let follower = &mut g.follower;
        let prev_committed = follower.committed_len();
        let Some((reset, patch_floor)) = follower.advance_stream()? else {
            return Ok(None);
        };
        let committed_len = follower.committed_len();
        let committed_grew = committed_len > prev_committed;
        let r = follower.open_read();
        let committed_delta: Vec<std::sync::Arc<Block>> =
            follower.committed()[prev_committed.min(committed_len)..].to_vec();
        // Same reshape/gen accounting as [`advance`](Self::advance) — see its comments.
        let prefix_intact = g.prev_provisional.len() <= r.provisional.len()
            && g.prev_provisional
                .iter()
                .zip(&r.provisional)
                .all(|(a, b)| a == b);
        if reset {
            g.epoch += 1;
            g.provisional_gen += 1;
        } else if committed_grew || patch_floor.is_some() || !prefix_intact {
            g.provisional_gen += 1;
        }
        let changed_from = if reset {
            0 // truncation/rewrite ⇒ everything changed
        } else {
            // Blocks below the prior committed length are unchanged (append-only); one
            // O(turn) comparison of the previous open region against the new tail finds
            // the exact boundary.
            let stable = g
                .prev_provisional
                .iter()
                .zip(
                    committed_delta
                        .iter()
                        .map(|a| a.as_ref())
                        .chain(r.provisional.iter()),
                )
                .take_while(|(a, b)| a == b)
                .count();
            prev_committed + stable
        };
        g.n_provisional = r.provisional.len();
        g.prev_provisional = r.provisional.clone();
        g.record();
        Ok(Some(ViewDelta {
            reset,
            committed_delta,
            committed_len,
            provisional: r.provisional.into_iter().map(std::sync::Arc::new).collect(),
            changed_from,
            user_times: r.user_times,
            metrics: r.metrics,
            tasks: r.tasks,
        }))
    }

    /// Every committed block, as `Arc` clones.
    ///
    /// This is what a **fresh** consumer needs after a resume: [`poll_view`](Self::poll_view)'s
    /// delta covers only what was folded since the restore, which on a cold start is everything
    /// and on a resume is almost nothing — correct in both cases for a consumer that already
    /// holds the prefix, and useless to a process that just started. O(N) refcount bumps, paid
    /// once at open.
    pub fn committed_arcs(&self) -> Vec<std::sync::Arc<Block>>
    where
        S: BlockStore<Bv = std::sync::Arc<Block>>,
    {
        super::lock_recover(&self.inner)
            .follower
            .committed()
            .to_vec()
    }

    /// Serve a client's [`Cursor`] against the current state — the same reply the free
    /// [`pull`](crate::pull::pull) computes, built from the accumulator's zones without cloning
    /// the whole committed prefix (only `committed[committed_from..]` is copied). Does **not**
    /// advance; a client that wants the freshest tail calls [`advance`](Self::advance) first (the
    /// `/pull` handler does).
    pub fn pull(&self, cursor: Cursor) -> PullReply
    where
        S: BlockRead,
    {
        let g = super::lock_recover(&self.inner);
        let (provisional, _times) = g.follower.open_finalized();
        let (committed_from, provisional_from) = pull_indices(
            g.epoch,
            g.follower.committed_len(),
            provisional.len(),
            g.provisional_gen,
            cursor,
        );
        PullReply {
            epoch: g.epoch,
            committed_from,
            committed: g.follower.committed_tail(committed_from),
            provisional_gen: g.provisional_gen,
            provisional_from,
            provisional: provisional[provisional_from..].to_vec(),
        }
    }

    /// A consistent, delta-sized [`PullDelta`] for one `/pull` render, under one lock so the
    /// counters match the blocks. `prev_epoch` / `rendered_committed` are the **caller's**
    /// render-cache state (its recorded epoch and how many committed blocks it has already
    /// rendered), so the method decides the committed slice and the reset flag together with the
    /// counters — no torn state. Lock order: a caller holding its own render lock may call this
    /// (render ⊃ shared); nothing takes them in the reverse order.
    pub fn pull_delta(&self, prev_epoch: u64, rendered_committed: usize) -> PullDelta
    where
        S: BlockRead,
    {
        let g = super::lock_recover(&self.inner);
        let reset = prev_epoch != g.epoch;
        let from = if reset {
            0 // stale render cache: every committed block re-renders
        } else {
            rendered_committed.min(g.follower.committed_len())
        };
        let r = g.follower.stream_read(from);
        PullDelta {
            epoch: g.epoch,
            provisional_gen: g.provisional_gen,
            reset,
            n_committed: r.n_committed,
            committed_delta: r.committed_delta,
            provisional: r.provisional,
            user_times: r.user_times,
            metrics: r.metrics,
            meta: r.meta,
            tasks: r.tasks,
        }
    }

    /// [`pull_delta`](Self::pull_delta) for a PROJECTION store (#74): everything a wire-serving
    /// poll needs EXCEPT committed content (`committed_delta` stays empty — the store already
    /// holds the committed zone in its own presentation form), plus one consistent look at the
    /// store and the committed `Bv` table under the SAME lock — so the derived facts (a
    /// wire-record byte range, a render-state snapshot) can't tear against the counters.
    /// Store-agnostic: no [`BlockRead`] bound.
    pub fn open_delta_with<R>(
        &self,
        f: impl FnOnce(&S, &[S::Bv], &PullDelta) -> R,
    ) -> (PullDelta, R) {
        let g = super::lock_recover(&self.inner);
        let r = g.follower.open_read();
        let d = PullDelta {
            epoch: g.epoch,
            provisional_gen: g.provisional_gen,
            reset: false,
            n_committed: r.n_committed,
            committed_delta: Vec::new(),
            provisional: r.provisional,
            user_times: r.user_times,
            metrics: r.metrics,
            meta: r.meta,
            tasks: r.tasks,
        };
        let derived = f(g.follower.store(), g.follower.committed(), &d);
        (d, derived)
    }

    /// The protocol counters only — `(epoch, provisional_gen, n_committed, n_provisional)` — an
    /// O(1) read (no block clone, no open-window finalize) so the `/pull` handler can decide
    /// idleness via [`pull_indices`] before paying
    /// [`pull_delta`](Self::pull_delta)'s delta read + render.
    pub fn counters(&self) -> (u64, u64, usize, usize) {
        let g = super::lock_recover(&self.inner);
        (
            g.epoch,
            g.provisional_gen,
            g.follower.committed_len(),
            g.n_provisional,
        )
    }

    /// The maintained live header (turns / tools / children) for the current tail — O(turn) to
    /// read (committed side maintained, open turn folded on top), no block scan. This is what a
    /// *child* session's first resolve reads off its **parent** to derive its title/breadcrumb
    /// once (the child-nav inversion) — no per-pull cross-session writes.
    pub fn session_meta(&self) -> SessionMeta {
        super::lock_recover(&self.inner).follower.session_meta()
    }

    /// The cursor a fully caught-up client of this session would hold — the current epoch/gen
    /// with both zone lengths as the positions. By definition `pull(end_cursor())` is idle; a
    /// same-process consumer bootstrapping a [`PullClient`](crate::pull::PullClient) tail
    /// (skipping the initial snapshot it obtained elsewhere) starts here.
    pub fn end_cursor(&self) -> crate::pull::Cursor {
        let (epoch, provisional_gen, committed_id, provisional_index) = self.counters();
        crate::pull::Cursor {
            epoch,
            committed_id,
            provisional_gen,
            provisional_index,
        }
    }

    /// One consistent look at the current epoch + the store, under the session lock — the
    /// wire-record range-read path (#74): the epoch check and the byte read can't tear against
    /// a concurrent reset.
    pub fn store_read<R>(&self, f: impl FnOnce(u64, &S) -> R) -> R {
        let g = super::lock_recover(&self.inner);
        f(g.epoch, g.follower.store())
    }

    /// The session's task op-log state (#15) — live from the fold, or the hibernated
    /// sidecar. Cheap (a clone of the maintained list, no session assembly).
    pub fn tasks(&self) -> crate::engine::TaskList {
        super::lock_recover(&self.inner).follower.tasks()
    }

    /// Make this session durable: every later advance appends its records through `w`.
    ///
    /// Records authored BEFORE this call are drained and written straight away, which matters on
    /// a cold start — the load itself folds the whole transcript, and those commits would
    /// otherwise never be recorded.
    pub fn attach_writer(&self, w: super::stream::MetaWriter) {
        let mut g = super::lock_recover(&self.inner);
        g.meta = Some(w);
        g.record();
    }

    /// Flush the record stream — before a `process::exit(0)`, which skips destructors.
    pub fn flush_meta(&self) {
        let mut g = super::lock_recover(&self.inner);
        g.record();
    }

    /// The current session epoch (bumped on reset).
    pub fn epoch(&self) -> u64 {
        super::lock_recover(&self.inner).epoch
    }

    /// The current provisional generation (bumped on commit or back-patch).
    pub fn provisional_gen(&self) -> u64 {
        super::lock_recover(&self.inner).provisional_gen
    }

    /// Whether a panic has poisoned this session's inner lock — its state may be torn mid-
    /// update, so the owner should DROP it and materialize a fresh session (the new epoch
    /// resyncs clients). Every accessor here already
    /// recovers the guard (no PoisonError cascade); this signal is how the state itself gets
    /// replaced rather than served torn.
    pub fn poisoned(&self) -> bool {
        self.inner.is_poisoned()
    }
}

/// A [`BlockStore`] whose backing survives the process (#96 §8.3) — what a durable cache needs
/// from a frontend's store that a construction closure cannot express, because both calls happen
/// *during* a load.
///
/// It replaces the retired `PersistentStore`. `hibernate_state`/`restore_state` are gone rather
/// than ported: they parked a render continuation across hibernation, and #96 §4.3 settles that
/// no presentation state is persisted at all — a continuation is **derived** from the restored
/// prefix instead, which cannot go stale against it.
pub trait DurableStore: BlockStore + Sized {
    /// What this frontend leaves in its lock for a peer that finds it held: a port for the
    /// server, a pane for the TUI. **Typed, not opaque** — locks are per-presentation, so the
    /// only reader is the same frontend that wrote it. Keeping it here rather than on
    /// [`Holder`](super::Holder) is what stops a `port` field, meaningless to the TUI, from
    /// leaking into a shared type.
    type Note: serde::Serialize + serde::de::DeserializeOwned + Clone;

    /// Reload the committed `Bv`s from the backing — the ONE frontend-specific step in the load
    /// (§6.2). HTML scans its record log for locators; the TUI decodes serialized blocks.
    fn load(&mut self) -> std::io::Result<Vec<Self::Bv>>;

    /// Adopt a restored prefix of exactly `n` committed blocks whose header is `meta`.
    ///
    /// Two things happen together because they are one event. The backing is cut to `n` (I6) —
    /// not optional, since HTML serves committed bytes as one range to EOF and orphaned bytes
    /// are read as garbage — and any render continuation the store derives from the prefix is
    /// seeded from it. Splitting them would let a continuation count blocks the truncation
    /// just removed.
    fn adopt(&mut self, n: usize, meta: &SessionMeta) -> std::io::Result<()>;
}

impl<S: DurableStore> SharedSession<S> {
    /// A session restored from a durable cache (#96 §6.3): an ordinary live follower whose
    /// accumulator is rebuilt from the record stream and whose reader starts at
    /// `resume.replay_from`, so the bytes below it are never re-read.
    ///
    /// `epoch` starts at 1, exactly as a cold open does. A resumed session is not a new epoch —
    /// it is the same session continuing, and a client that kept its cursor across the restart
    /// stays caught up.
    pub fn resume(
        agent: Agent,
        path: &Path,
        store: S,
        committed: Vec<S::Bv>,
        mm: crate::engine::meta_stream::MaterializedMeta,
        resume: &crate::engine::meta_stream::Resume,
    ) -> Self {
        Self {
            inner: Mutex::new(Inner {
                follower: Box::new(FollowParser::resume(
                    claude_replay_core::adapter(agent),
                    path,
                    store,
                    committed,
                    mm,
                    resume,
                )),
                epoch: 1,
                provisional_gen: 0,
                n_provisional: 0,
                prev_provisional: Vec::new(),
                meta: None,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn tmp() -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        std::env::temp_dir().join(format!(
            "cr-shared-{}-{}.jsonl",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn append(path: &PathBuf, s: &str) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        f.write_all(s.as_bytes()).unwrap();
    }

    // A single open turn: user "go", then a tool_use, then its tool_result (back-patch), then
    // assistant text — then a *second* user turn that commits the first.
    const USER1: &str = "{\"type\":\"user\",\"cwd\":\"/r\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"go\"}]},\"timestamp\":\"2026-07-26T10:00:00Z\"}\n";
    const TOOL: &str = "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"tool_use\",\"id\":\"b1\",\"name\":\"Bash\",\"input\":{\"command\":\"ls\"}}]},\"timestamp\":\"2026-07-26T10:00:01Z\"}\n";
    const RESULT: &str = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"b1\",\"content\":\"out\"}]},\"timestamp\":\"2026-07-26T10:00:02Z\"}\n";
    const TEXT: &str = "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"done\"}]},\"timestamp\":\"2026-07-26T10:00:03Z\"}\n";
    const USER2: &str = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"next\"}]},\"timestamp\":\"2026-07-26T10:00:04Z\"}\n";

    /// The gen/epoch transitions §9a prescribes: append leaves `gen` unchanged, an in-place
    /// back-patch bumps it, a commit bumps it, and a truncation bumps `epoch`.
    // Test-only view of the committed count (the state lives behind the Mutex now).
    fn committed_len(ss: &SharedSession) -> usize {
        crate::cache::lock_recover(&ss.inner)
            .follower
            .committed_len()
    }

    #[test]
    fn gen_and_epoch_track_append_backpatch_commit_reset() {
        let path = tmp();
        let ss = SharedSession::open(Agent::CLAUDE, &path);
        assert_eq!((ss.epoch(), ss.provisional_gen()), (1, 0));

        // Open the turn (append only) — no gen bump.
        append(&path, USER1);
        assert!(ss.advance().unwrap());
        append(&path, TOOL);
        assert!(ss.advance().unwrap());
        assert_eq!(ss.provisional_gen(), 0, "pure appends don't bump gen");
        assert_eq!(committed_len(&ss), 0, "turn still open");

        // tool_result back-patches the already-emitted ToolUse ⇒ gen bumps.
        append(&path, RESULT);
        assert!(ss.advance().unwrap());
        assert_eq!(ss.provisional_gen(), 1, "back-patch bumps gen");

        // More append ⇒ no bump.
        append(&path, TEXT);
        assert!(ss.advance().unwrap());
        assert_eq!(ss.provisional_gen(), 1, "append after back-patch: still 1");

        // A second user turn commits the first ⇒ committed grows, gen bumps.
        append(&path, USER2);
        assert!(ss.advance().unwrap());
        assert_eq!(ss.provisional_gen(), 2, "commit bumps gen");
        assert!(committed_len(&ss) > 0, "turn 1 committed");
        assert_eq!(ss.epoch(), 1, "no reset yet");

        // Idle tick: nothing advances, counters unchanged.
        assert!(!ss.advance().unwrap());
        assert_eq!((ss.epoch(), ss.provisional_gen()), (1, 2));

        // Truncate/rewrite ⇒ reset ⇒ epoch bumps.
        std::fs::write(&path, USER1).unwrap();
        assert!(ss.advance().unwrap());
        assert_eq!(ss.epoch(), 2, "reset bumps epoch");

        let _ = std::fs::remove_file(&path);
    }

    /// `pull` semantics end-to-end: a fresh (epoch-0) cursor resyncs; a same-gen re-pull is idle;
    /// a stale-gen cursor after a back-patch gets the whole provisional (from 0); a commit advances
    /// `committed_from` and resets the provisional.
    #[test]
    fn pull_serves_resync_append_backpatch_and_commit() {
        let path = tmp();
        let ss = SharedSession::open(Agent::CLAUDE, &path);
        append(&path, USER1);
        append(&path, TOOL);
        ss.advance().unwrap();

        // Fresh client (default cursor, epoch 0) ⇒ full resync.
        let r = ss.pull(Cursor::default());
        assert_eq!(r.epoch, 1);
        assert_eq!(r.committed_from, 0);
        assert_eq!(r.provisional_from, 0);
        assert!(r.committed.is_empty(), "turn still open ⇒ no committed");
        let prov_len = r.provisional.len();
        assert!(prov_len >= 2, "user + tool_use resident");
        let c = r.next_cursor();

        // Re-pull with the up-to-date cursor, nothing changed ⇒ idle.
        assert!(ss.pull(c).is_idle());

        // Back-patch: the tool_result fills the ToolUse ⇒ gen bumps ⇒ the stale-gen cursor gets the
        // whole provisional resent from 0 (content-blind client just replaces its provisional zone).
        append(&path, RESULT);
        ss.advance().unwrap();
        let r = ss.pull(c);
        assert!(r.committed.is_empty());
        assert_eq!(
            r.provisional_from, 0,
            "gen changed ⇒ resend whole provisional"
        );
        assert_eq!(r.provisional.len(), prov_len, "same blocks, now patched");
        let c = r.next_cursor();

        // Commit: a second user turn ⇒ committed grows, provisional resets to the new open turn.
        append(&path, TEXT);
        append(&path, USER2);
        ss.advance().unwrap();
        let r = ss.pull(c);
        assert!(!r.committed.is_empty(), "turn 1 delivered as committed");
        assert_eq!(r.committed_from, 0, "client had no committed yet");
        assert_eq!(r.provisional_from, 0, "provisional reset on commit");

        let _ = std::fs::remove_file(&path);
    }

    /// `pull_delta` returns delta-sized render inputs: after a commit, `committed_delta` is
    /// exactly `committed[rendered..]` (not the whole prefix); `reset` fires iff the caller's
    /// epoch is stale; `user_times` stays whole-session; and the maintained `meta` matches the
    /// full tail (a provisional tool call counts immediately).
    #[test]
    fn pull_delta_returns_the_unrendered_tail_and_maintained_meta() {
        let path = tmp();
        let ss = SharedSession::open(Agent::CLAUDE, &path);
        append(&path, USER1);
        append(&path, TOOL);
        ss.advance().unwrap();

        // Open turn only: nothing committed, provisional carries the turn, meta counts the
        // in-flight tool call immediately (the header matches the tail, not just committed).
        let d = ss.pull_delta(ss.epoch(), 0);
        assert!(!d.reset, "same epoch");
        assert_eq!(d.n_committed, 0);
        assert!(d.committed_delta.is_empty());
        assert!(!d.provisional.is_empty());
        assert_eq!(d.meta.turns, 1);
        assert_eq!(d.meta.tools, 1, "provisional ToolUse counts in the header");

        // Commit turn 1 (a second user turn): the caller has rendered 0 committed blocks, so the
        // delta is the whole (small) committed prefix; a caller that already rendered k gets
        // committed[k..] only.
        append(&path, RESULT);
        append(&path, TEXT);
        append(&path, USER2);
        ss.advance().unwrap();
        let d = ss.pull_delta(ss.epoch(), 0);
        let n = d.n_committed;
        assert!(n > 0, "turn 1 committed");
        assert_eq!(d.committed_delta.len(), n);
        assert_eq!(d.meta.turns, 2);
        let d2 = ss.pull_delta(ss.epoch(), n);
        assert!(
            d2.committed_delta.is_empty(),
            "already-rendered prefix is not re-read"
        );
        assert_eq!(
            d2.user_times.len(),
            2,
            "user_times stays whole-session (the renderer indexes it by turn)"
        );

        // Stale epoch (e.g. after a truncation): reset + the full committed range.
        std::fs::write(&path, USER1).unwrap();
        ss.advance().unwrap();
        let d3 = ss.pull_delta(1, n);
        assert!(d3.reset, "epoch moved past the caller's");
        assert_eq!(
            d3.committed_delta.len(),
            d3.n_committed,
            "reset serves committed from 0"
        );
        assert_eq!(d3.meta.turns, 1, "meta rebuilt for the new file");

        let _ = std::fs::remove_file(&path);
    }

    /// A tier-b-backed `SharedSession` (committed content in the store's on-disk backing, never
    /// resident) must serve **byte-identical** pull output to the in-memory default — the wire
    /// reply, the delta blocks (decoded back off disk), and the maintained header — across the
    /// whole protocol: append, back-patch, commit, and a truncation reset (which also resets the
    /// backing).
    #[test]
    fn tier_b_backed_shared_session_serves_identical_pulls() {
        use crate::engine::tier_b::TierBStore;
        let path = tmp();
        let backing =
            std::env::temp_dir().join(format!("cr-shared-tb-{}.blocks", std::process::id()));
        let _ = std::fs::remove_file(&backing);
        let mem = SharedSession::open(Agent::CLAUDE, &path);
        let tb = SharedSession::with_store(
            Agent::CLAUDE,
            &path,
            TierBStore::file(&backing).expect("create backing"),
        );

        let (mut cm, mut ct) = (Cursor::default(), Cursor::default());
        for chunk in [USER1, TOOL, RESULT, TEXT, USER2] {
            append(&path, chunk);
            mem.advance().unwrap();
            tb.advance().unwrap();
            let (rm, rt) = (mem.pull(cm), tb.pull(ct));
            assert_eq!(rm, rt, "wire replies identical after {chunk:.20}");
            cm = rm.next_cursor();
            ct = rt.next_cursor();
            // A caught-up cursor equals the session's end_cursor, whose pull is idle.
            assert_eq!(cm, mem.end_cursor());
            assert!(mem.pull(mem.end_cursor()).is_idle());
            let (dm, dt) = (mem.pull_delta(mem.epoch(), 0), tb.pull_delta(tb.epoch(), 0));
            assert_eq!(
                format!("{:?}", dm.committed_delta),
                format!("{:?}", dt.committed_delta),
                "committed delta decodes off disk identically"
            );
            assert_eq!(dm.meta, dt.meta, "maintained header identical");
        }

        // Truncation: the accumulator resets and the tier-b backing resets with it.
        std::fs::write(&path, USER1).unwrap();
        mem.advance().unwrap();
        tb.advance().unwrap();
        assert_eq!(mem.pull(cm), tb.pull(ct), "post-reset resync identical");

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&backing);
    }

    /// Poison resilience (#56): a panic on one request thread while holding the inner lock must
    /// not brick the session — every accessor recovers the guard (no PoisonError cascade), and
    /// `poisoned()` reports the state as suspect so the owner swaps in a fresh session (the
    /// server's stale/poison branch).
    #[test]
    fn poisoned_session_keeps_answering_and_reports_itself() {
        use std::sync::Arc;
        let path = tmp();
        append(&path, USER1);
        let ss = Arc::new(SharedSession::open(Agent::CLAUDE, &path));
        ss.advance().unwrap();
        assert!(!ss.poisoned());

        // Poison the inner lock: a scoped thread panics while holding it.
        let poisoner = {
            let ss = Arc::clone(&ss);
            std::thread::spawn(move || {
                let _g = crate::cache::lock_recover(&ss.inner);
                panic!("simulated fold panic");
            })
        };
        assert!(poisoner.join().is_err(), "the panic happened");
        assert!(ss.poisoned(), "lock reports poisoned");

        // Every accessor still answers instead of cascading PoisonError panics.
        let _ = ss.counters();
        let _ = ss.epoch();
        let r = ss.pull(Cursor::default());
        assert_eq!(r.epoch, 1, "still serving (state pre-panic was consistent)");
        let _ = ss.pull_delta(1, 0);
        let _ = std::fs::remove_file(&path);
    }

    /// The `Arc` sharing the concurrency wrapper exists for: many client threads hold the same
    /// `Arc<SharedSession>` and pull concurrently while another advances the tail — no data race,
    /// no deadlock, and every reply is internally consistent (a self-describing cursor round-trip).
    #[test]
    fn arc_shared_across_threads_concurrent_pull_and_advance() {
        use std::sync::Arc;
        let path = tmp();
        std::fs::write(&path, USER1).unwrap();
        let ss = Arc::new(SharedSession::open(Agent::CLAUDE, &path));
        ss.advance().unwrap();

        let writer = {
            let ss = Arc::clone(&ss);
            let path = path.clone();
            std::thread::spawn(move || {
                for chunk in [TOOL, RESULT, TEXT, USER2] {
                    append(&path, chunk);
                    ss.advance().unwrap();
                }
            })
        };
        let readers: Vec<_> = (0..4)
            .map(|_| {
                let ss = Arc::clone(&ss);
                std::thread::spawn(move || {
                    let mut c = Cursor::default();
                    for _ in 0..50 {
                        let r = ss.pull(c);
                        // The reply's own next_cursor must be a valid position to pull again.
                        c = r.next_cursor();
                    }
                    c
                })
            })
            .collect();
        writer.join().unwrap();
        for r in readers {
            let c = r.join().unwrap();
            // Every reader ended holding this session's current epoch (it resynced past the start).
            assert_eq!(c.epoch, ss.epoch());
        }
        let _ = std::fs::remove_file(&path);
    }
}
