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

use std::path::Path;
use std::sync::Mutex;

use super::stream::{pull_indices, Cursor, PullReply};
use crate::engine::SessionMeta;
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
/// The block state lives **in the follower's accumulator** (the committed prefix is owned there
/// and never cloned whole); this adds only the two protocol counters and a cached zone length.
struct Inner {
    follower: FollowParser,
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

/// The live, pull-servable state of one followed session (see the module docs). `Arc`-shareable;
/// all methods take `&self`.
pub struct SharedSession {
    inner: Mutex<Inner>,
}

impl SharedSession {
    /// Open a shared session following `path` for `agent`. The first [`advance`](Self::advance)
    /// folds the current file; later ones fold only appends.
    pub fn open(agent: Agent, path: &Path) -> Self {
        Self {
            inner: Mutex::new(Inner {
                follower: FollowParser::open(agent, path),
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
        // A commit is visible as growth of the append-only committed prefix (compare BEFORE the
        // fold). Reset takes priority (it also invalidates committed).
        let prev_committed = g.follower.committed_len();
        let Some((reset, patch_floor)) = g.follower.advance_stream()? else {
            return Ok(false);
        };
        let committed_grew = g.follower.committed_len() > prev_committed;
        if reset {
            g.epoch += 1;
            g.provisional_gen += 1;
        } else if committed_grew || patch_floor.is_some() {
            g.provisional_gen += 1;
        }
        g.n_provisional = g.follower.provisional_len();
        Ok(true)
    }

    /// Serve a client's [`Cursor`] against the current state — the same reply the free
    /// [`pull`](super::stream::pull) computes, built from the accumulator's zones without cloning
    /// the whole committed prefix (only `committed[committed_from..]` is copied). Does **not**
    /// advance; a client that wants the freshest tail calls [`advance`](Self::advance) first (the
    /// `/pull` handler does).
    pub fn pull(&self, cursor: Cursor) -> PullReply {
        let g = self.inner.lock().unwrap();
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
    pub fn pull_delta(&self, prev_epoch: u64, rendered_committed: usize) -> PullDelta {
        let g = self.inner.lock().unwrap();
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
            g.follower.committed_len(),
            g.n_provisional,
        )
    }

    /// The maintained live header (turns / tools / children) for the current tail — O(turn) to
    /// read (committed side maintained, open turn folded on top), no block scan. This is what a
    /// *child* session's first resolve reads off its **parent** to derive its title/breadcrumb
    /// once (the child-nav inversion) — no per-pull cross-session writes.
    pub fn session_meta(&self) -> SessionMeta {
        self.inner.lock().unwrap().follower.session_meta()
    }

    /// The current session epoch (bumped on reset).
    pub fn epoch(&self) -> u64 {
        self.inner.lock().unwrap().epoch
    }

    /// The current provisional generation (bumped on commit or back-patch).
    pub fn provisional_gen(&self) -> u64 {
        self.inner.lock().unwrap().provisional_gen
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
        ss.inner.lock().unwrap().follower.committed_len()
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
