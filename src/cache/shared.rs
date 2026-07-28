//! `SharedSession` — the present-layer state the pull protocol (`super::stream`) serves against.
//!
//! It owns the follower over a source and maintains the two zones the cursor addresses — the
//! append-only `committed` prefix and the current-turn `provisional` — plus the two protocol
//! counters (`epoch`, `provisional_gen`). [`advance`](SharedSession::advance) borrows the caller's
//! thread to tail the source (fold appended lines), then updates the zones and bumps the counters
//! per design §9a:
//!
//! - **`epoch`** bumps on a **reset** (truncation/rewrite) — outstanding cursors then resync.
//! - **`provisional_gen`** bumps on a **commit** (the committed prefix grew — the old provisional
//!   became committed) or an **in-place back-patch** (the follower's `patch_floor` signal, from
//!   [`Replayer::apply`](crate::engine) via `advance_at`), and **never on a pure append**.
//!
//! [`pull`](SharedSession::pull) then answers any client [`Cursor`] against the current zones.
//!
//! Concurrency (§9a): the state is interior-mutable behind one `Mutex`, so a `SharedSession` wraps
//! in an `Arc` and any number of client threads call [`pull`](SharedSession::pull) / [`advance`]
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

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::stream::{pull_indices, Cursor, PullReply};
use crate::engine::session::{BlockStore, InMemoryStore};
use crate::engine::{SessionMeta, StreamRead};
use crate::follow::FollowParser;
use crate::metrics::Metrics;
use crate::model::{Block, EpochSeconds};
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
}

/// The mutable state of one followed session, guarded as a unit by [`SharedSession`]'s `Mutex`.
/// The block state lives **in the body** (the committed prefix is owned by the follower's
/// accumulator — or by a restored materialization — and never cloned whole); this adds only the
/// two protocol counters and a cached zone length.
struct Inner<S: BlockStore> {
    body: Body<S>,
    /// Session validity token. A client cursor with a stale `epoch` resyncs. Starts at 1 so a
    /// default (`epoch == 0`) cursor from a fresh client mismatches and resyncs on its first pull.
    epoch: u64,
    /// The current open-turn generation — see the module docs / `super::stream`.
    provisional_gen: u64,
    /// The finalized open-turn length, refreshed on each advancing tick — so [`counters`]
    /// (SharedSession::counters), the per-poll idle check, stays O(1) instead of re-finalizing
    /// the open window on every quiet poll.
    n_provisional: usize,
}

/// Where a session's state comes from: a **live** follower folding the source, or a
/// **hibernated** materialization reloaded from disk (an evicted resident whose source hasn't
/// changed since it was persisted — see [`SharedSession::hibernate`]/[`restore`]
/// (SharedSession::restore)). A hibernated body serves every read from restored state and never
/// folds; if its source later changes, [`hibernation_stale`](SharedSession::hibernation_stale)
/// reports it and the owner replaces the whole session with a fresh live one (the epoch change
/// resyncs clients — identical to today's discard-and-refold behavior).
enum Body<S: BlockStore> {
    // Boxed: a follower (reader buffers + accumulator) dwarfs the hibernated sidecar state.
    Live(Box<FollowParser<S>>),
    Hibernated(Box<Hibernated<S>>),
}

/// A restored materialization: the store re-opened over its on-disk backing + the small sidecar
/// state. Content stays on disk; reads decode on demand through the locators.
struct Hibernated<S: BlockStore> {
    store: S,
    committed: Vec<S::Bv>,
    provisional: Vec<Block>,
    user_times: Vec<Option<EpochSeconds>>,
    metrics: Metrics,
    meta: SessionMeta,
    /// The tailed source + its byte length at hibernation — the validity check
    /// [`hibernation_stale`](SharedSession::hibernation_stale) re-evaluates.
    src: PathBuf,
    src_len: u64,
}

impl<S: BlockStore> Body<S> {
    fn committed_len(&self) -> usize {
        match self {
            Body::Live(f) => f.committed_len(),
            Body::Hibernated(h) => h.committed.len(),
        }
    }
    fn open_finalized(&self) -> (Vec<Block>, Vec<Option<EpochSeconds>>) {
        match self {
            Body::Live(f) => f.open_finalized(),
            Body::Hibernated(h) => (h.provisional.clone(), h.user_times.clone()),
        }
    }
    fn committed_tail(&self, from: usize) -> Vec<Block> {
        match self {
            Body::Live(f) => f.committed_tail(from),
            Body::Hibernated(h) => h.committed[from.min(h.committed.len())..]
                .iter()
                .map(|bv| h.store.get(bv).into_owned())
                .collect(),
        }
    }
    fn stream_read(&self, from: usize) -> StreamRead {
        match self {
            Body::Live(f) => f.stream_read(from),
            Body::Hibernated(h) => StreamRead {
                committed_delta: self.committed_tail(from),
                provisional: h.provisional.clone(),
                user_times: h.user_times.clone(),
                metrics: h.metrics.clone(),
                meta: h.meta.clone(),
                n_committed: h.committed.len(),
            },
        }
    }
    fn session_meta(&self) -> SessionMeta {
        match self {
            Body::Live(f) => f.session_meta(),
            Body::Hibernated(h) => h.meta.clone(),
        }
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
                body: Body::Live(Box::new(FollowParser::with_store(agent, path, store))),
                epoch: 1,
                provisional_gen: 0,
                n_provisional: 0,
            }),
        }
    }

    /// Borrow the caller's thread to tail the source: fold any newly-appended lines and advance
    /// `epoch` / `provisional_gen` per §9a (see the module docs). Returns `true` when content
    /// advanced, `false` on an idle tick. Uses the follower's **light** streaming advance — no
    /// `Session` assembly (no O(N) index/sub-agent build, no whole-committed clone); the zones
    /// stay owned by the accumulator and are read delta-sized at pull time.
    pub fn advance(&self) -> std::io::Result<bool> {
        let mut g = self.inner.lock().unwrap();
        // A hibernated body never folds: its source was unchanged when restored. If the source
        // later changes, `hibernation_stale` reports it and the owner swaps in a fresh live
        // session (whose new epoch resyncs clients).
        let Body::Live(follower) = &mut g.body else {
            return Ok(false);
        };
        // A commit is visible as growth of the append-only committed prefix (compare BEFORE the
        // fold). Reset takes priority (it also invalidates committed).
        let prev_committed = follower.committed_len();
        let Some((reset, patch_floor)) = follower.advance_stream()? else {
            return Ok(false);
        };
        let committed_grew = follower.committed_len() > prev_committed;
        let n_provisional = follower.provisional_len();
        if reset {
            g.epoch += 1;
            g.provisional_gen += 1;
        } else if committed_grew || patch_floor.is_some() {
            g.provisional_gen += 1;
        }
        g.n_provisional = n_provisional;
        Ok(true)
    }

    /// Serve a client's [`Cursor`] against the current state — the same reply the free
    /// [`pull`](super::stream::pull) computes, built from the accumulator's zones without cloning
    /// the whole committed prefix (only `committed[committed_from..]` is copied). Does **not**
    /// advance; a client that wants the freshest tail calls [`advance`](Self::advance) first (the
    /// `/pull` handler does).
    pub fn pull(&self, cursor: Cursor) -> PullReply {
        let g = self.inner.lock().unwrap();
        let (provisional, _times) = g.body.open_finalized();
        let (committed_from, provisional_from) = pull_indices(
            g.epoch,
            g.body.committed_len(),
            provisional.len(),
            g.provisional_gen,
            cursor,
        );
        PullReply {
            epoch: g.epoch,
            committed_from,
            committed: g.body.committed_tail(committed_from),
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
    pub fn pull_delta(&self, prev_epoch: u64, rendered_committed: usize) -> PullDelta {
        let g = self.inner.lock().unwrap();
        let reset = prev_epoch != g.epoch;
        let from = if reset {
            0 // stale render cache: every committed block re-renders
        } else {
            rendered_committed.min(g.body.committed_len())
        };
        let r = g.body.stream_read(from);
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
        }
    }

    /// The protocol counters only — `(epoch, provisional_gen, n_committed, n_provisional)` — an
    /// O(1) read (no block clone, no open-window finalize) so the `/pull` handler can decide
    /// idleness via [`pull_indices`](super::stream::pull_indices) before paying
    /// [`pull_delta`](Self::pull_delta)'s delta read + render.
    pub fn counters(&self) -> (u64, u64, usize, usize) {
        let g = self.inner.lock().unwrap();
        (
            g.epoch,
            g.provisional_gen,
            g.body.committed_len(),
            g.n_provisional,
        )
    }

    /// The maintained live header (turns / tools / children) for the current tail — O(turn) to
    /// read (committed side maintained, open turn folded on top), no block scan. This is what a
    /// *child* session's first resolve reads off its **parent** to derive its title/breadcrumb
    /// once (the child-nav inversion) — no per-pull cross-session writes.
    pub fn session_meta(&self) -> SessionMeta {
        self.inner.lock().unwrap().body.session_meta()
    }

    /// The current session epoch (bumped on reset).
    pub fn epoch(&self) -> u64 {
        self.inner.lock().unwrap().epoch
    }

    /// The current provisional generation (bumped on commit or back-patch).
    pub fn provisional_gen(&self) -> u64 {
        self.inner.lock().unwrap().provisional_gen
    }

    /// Whether this is a **hibernated** session whose source has changed since it was persisted —
    /// its restored state no longer matches the file, so the owner should drop it and materialize
    /// a fresh live session (the new epoch resyncs clients; same effect as today's
    /// discard-and-refold after an eviction). Always `false` for a live body.
    pub fn hibernation_stale(&self) -> bool {
        let g = self.inner.lock().unwrap();
        match &g.body {
            Body::Live(_) => false,
            Body::Hibernated(h) => {
                std::fs::metadata(&h.src).map(|m| m.len()).unwrap_or(0) != h.src_len
            }
        }
    }
}

/// The persisted sidecar beside a tier-b `.blocks` backing: everything a pull needs to serve an
/// **unchanged** source without re-folding the transcript. Deliberately NOT the fold state — the
/// replayer's open-window internals aren't persistable; a source that grew after hibernation is
/// re-folded from scratch instead (see [`SharedSession::restore`]).
#[derive(serde::Serialize, serde::Deserialize)]
struct HibernatedSidecar {
    epoch: u64,
    provisional_gen: u64,
    /// Validity: the source transcript's byte length at hibernation.
    src_len: u64,
    /// Validity: the `.blocks` backing's byte length (a truncated/rewritten backing ⇒ refold).
    backing_len: u64,
    committed: Vec<crate::engine::tier_b::Deferred>,
    provisional: Vec<Block>,
    user_times: Vec<Option<EpochSeconds>>,
    metrics: Metrics,
    meta: SessionMeta,
}

impl SharedSession<crate::engine::tier_b::TierBStore> {
    /// Persist this session's serving state to `sidecar` (its block content is already in the
    /// tier-b `.blocks` backing) so an evicted resident can be [`restore`](Self::restore)d without
    /// re-folding the transcript. Small: locators + the open turn + times/metrics/header +
    /// counters + the two validity lengths.
    pub fn hibernate(&self, sidecar: &Path) -> std::io::Result<()> {
        let g = self.inner.lock().unwrap();
        let side = match &g.body {
            Body::Live(f) => {
                let r = f.stream_read(f.committed_len()); // empty delta: open turn + header only
                HibernatedSidecar {
                    epoch: g.epoch,
                    provisional_gen: g.provisional_gen,
                    src_len: std::fs::metadata(f.path()).map(|m| m.len()).unwrap_or(0),
                    backing_len: f.store().len() as u64,
                    committed: f.committed().to_vec(),
                    provisional: r.provisional,
                    user_times: r.user_times,
                    metrics: r.metrics,
                    meta: r.meta,
                }
            }
            Body::Hibernated(h) => HibernatedSidecar {
                epoch: g.epoch,
                provisional_gen: g.provisional_gen,
                src_len: h.src_len,
                backing_len: h.store.len() as u64,
                committed: h.committed.clone(),
                provisional: h.provisional.clone(),
                user_times: h.user_times.clone(),
                metrics: h.metrics.clone(),
                meta: h.meta.clone(),
            },
        };
        let json = serde_json::to_vec(&side)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(sidecar, json)
    }

    /// Reload a [`hibernate`](Self::hibernate)d session — **iff the source is unchanged** (same
    /// byte length as recorded) and the backing still matches. Returns `None` when either check
    /// fails, the files are missing, or the sidecar doesn't parse — the caller then materializes a
    /// fresh live session and re-folds (exactly the pre-C-3 behavior). A restored session serves
    /// pulls from the reloaded state at its original epoch/gen, so a client that kept its cursor
    /// across the eviction continues seamlessly.
    pub fn restore(src: &Path, sidecar: &Path, backing: &Path) -> Option<Self> {
        let side: HibernatedSidecar = serde_json::from_slice(&std::fs::read(sidecar).ok()?).ok()?;
        if std::fs::metadata(src).ok()?.len() != side.src_len {
            return None; // the source moved on — refold instead
        }
        let store = crate::engine::tier_b::TierBStore::open(backing).ok()?;
        if store.len() as u64 != side.backing_len {
            return None; // backing rewritten/torn — refold instead
        }
        Some(Self {
            inner: Mutex::new(Inner {
                n_provisional: side.provisional.len(),
                epoch: side.epoch,
                provisional_gen: side.provisional_gen,
                body: Body::Hibernated(Box::new(Hibernated {
                    store,
                    committed: side.committed,
                    provisional: side.provisional,
                    user_times: side.user_times,
                    metrics: side.metrics,
                    meta: side.meta,
                    src: src.to_path_buf(),
                    src_len: side.src_len,
                })),
            }),
        })
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
        ss.inner.lock().unwrap().body.committed_len()
    }

    #[test]
    fn gen_and_epoch_track_append_backpatch_commit_reset() {
        let path = tmp();
        let ss = SharedSession::open(Agent::Claude, &path);
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
        let ss = SharedSession::open(Agent::Claude, &path);
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
        let ss = SharedSession::open(Agent::Claude, &path);
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
        let mem = SharedSession::open(Agent::Claude, &path);
        let tb = SharedSession::with_store(
            Agent::Claude,
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

    /// The evict→persist→reload cycle: a hibernated-then-restored session (source unchanged)
    /// serves **identical** wire output to the live session it replaced — the full resync (blocks
    /// decoded off the reopened backing), a held cursor's idle continuation at the original
    /// epoch/gen, and the delta read — all without re-folding the transcript. Growth after
    /// restore flags `hibernation_stale` (the owner then refolds fresh).
    #[test]
    fn hibernate_then_restore_serves_identical_pulls_without_refold() {
        use crate::engine::tier_b::TierBStore;
        let path = tmp();
        let backing = path.with_extension("blocks");
        let sidecar = path.with_extension("state");
        let live =
            SharedSession::with_store(Agent::Claude, &path, TierBStore::file(&backing).unwrap());
        for chunk in [USER1, TOOL, RESULT, TEXT, USER2] {
            append(&path, chunk);
        }
        live.advance().unwrap();
        let baseline_full = live.pull(Cursor::default());
        assert!(!baseline_full.committed.is_empty(), "turn 1 committed");
        let held = baseline_full.next_cursor();
        let baseline_idle = live.pull(held);
        assert!(baseline_idle.is_idle());
        let (e, gen) = (live.epoch(), live.provisional_gen());
        live.hibernate(&sidecar).unwrap();
        drop(live);

        let r = SharedSession::restore(&path, &sidecar, &backing).expect("unchanged ⇒ restores");
        assert!(!r.hibernation_stale());
        assert!(!r.advance().unwrap(), "a hibernated body never folds");
        assert_eq!(
            (r.epoch(), r.provisional_gen()),
            (e, gen),
            "counters survive"
        );
        assert_eq!(
            r.pull(Cursor::default()),
            baseline_full,
            "full resync identical — committed decoded off the reopened backing"
        );
        assert_eq!(
            r.pull(held),
            baseline_idle,
            "held cursor continues seamlessly"
        );
        let d = r.pull_delta(e, 0);
        assert!(!d.reset);
        assert_eq!(d.n_committed, baseline_full.committed.len());
        assert_eq!(d.meta.turns, 2, "restored header intact");

        // The source grows after restore ⇒ stale; the owner swaps in a fresh live session.
        append(&path, TEXT);
        assert!(r.hibernation_stale(), "grown source flags stale");

        for f in [&path, &backing, &sidecar] {
            let _ = std::fs::remove_file(f);
        }
    }

    /// `restore` refuses a source that changed since hibernation (the caller refolds fresh) —
    /// the unchanged-source-only policy, enforced at reload time.
    #[test]
    fn restore_refuses_a_changed_source() {
        use crate::engine::tier_b::TierBStore;
        let path = tmp();
        let backing = path.with_extension("blocks");
        let sidecar = path.with_extension("state");
        let live =
            SharedSession::with_store(Agent::Claude, &path, TierBStore::file(&backing).unwrap());
        append(&path, USER1);
        live.advance().unwrap();
        live.hibernate(&sidecar).unwrap();
        drop(live);

        append(&path, USER2); // the source moved on
        assert!(
            SharedSession::restore(&path, &sidecar, &backing).is_none(),
            "changed source ⇒ no restore (refold instead)"
        );
        for f in [&path, &backing, &sidecar] {
            let _ = std::fs::remove_file(f);
        }
    }

    /// The `Arc` sharing the concurrency wrapper exists for: many client threads hold the same
    /// `Arc<SharedSession>` and pull concurrently while another advances the tail — no data race,
    /// no deadlock, and every reply is internally consistent (a self-describing cursor round-trip).
    #[test]
    fn arc_shared_across_threads_concurrent_pull_and_advance() {
        use std::sync::Arc;
        let path = tmp();
        std::fs::write(&path, USER1).unwrap();
        let ss = Arc::new(SharedSession::open(Agent::Claude, &path));
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
