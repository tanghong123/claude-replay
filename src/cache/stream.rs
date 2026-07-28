//! The pull-based streaming protocol (design §9a) — **server-side auto-patch**.
//!
//! The cache holds the *joined* view: `committed` (append-only for the whole session) and
//! `provisional` (the open turn), which the server **patches in place** as the Replayer folds
//! messages — a tool's output lands on its `ToolUse` block, a sub-agent flips `Running →
//! Completed`. Because the join stays server-side, the client is **content-blind**: it never
//! inspects a block, matches a `tool_use_id`, or learns what a `ToolUse` is. It is **not**, though,
//! a dumb framebuffer — it is **protocol-aware and stateful**: it must understand the returned
//! cursor's semantics and keep two distinct tracked zones (a `committed` buffer it only *appends*
//! to; a `provisional` buffer it *replaces or extends* per the reply). Content-blind, protocol-aware.
//!
//! A client's position is **four numbers**: `Cursor { epoch, committed_id, provisional_gen,
//! provisional_index }`.
//! - `committed_id` — append index into the session-wide committed log (append-only ⇒ sent once).
//! - `provisional_gen` — the identity of the current provisional *generation*. It bumps whenever
//!   an **in-place patch** touches an existing provisional block (a pure append does **not** bump
//!   it). Within a generation the provisional is append-only, so the client's `provisional_index`
//!   stays a valid suffix pointer.
//! - `provisional_index` — append position within the current generation.
//! - `epoch` — session validity (a mismatch ⇒ resync).
//!
//! Per zone the reply is **self-describing** via `*_from`, and the client applies one rule *per
//! tracked zone*: *"truncate to `from`, then extend."* `provisional_from` is:
//! - the request's `provisional_index` when the **gen is unchanged** (append-only suffix — the
//!   common, cheap case);
//! - `0` when the **gen changed** (an in-place patch may have altered a block the client already
//!   holds ⇒ resend the whole, already-patched, provisional);
//! - `0` on a **commit** (`committed` grew ⇒ the old provisional became committed ⇒ the open turn
//!   restarts) or on a **resync** (`epoch` changed).
//!
//! Cost note: a gen bump resends the whole open turn (its block count can reach the hundreds —
//! p90 ≈ 150, worst ≈ 1500 in the measured corpus), but only *once per poll that saw any patch*,
//! not once per patch — and polls are infrequent relative to fold events, so the practical cost is
//! low. A future optimization (a 5th cursor member `provisional_gen_prefix`) can avoid resending
//! the unchanged head of the provisional and amortize gen bumps to O(log n) by doubling; it is
//! deliberately **not** implemented yet — see `Cursor`'s note.
//!
//! (This replaces the server-side snapshot diff in `html_export::serve::stream_delta`: the client
//! tracks its own position, so the server keeps no per-client baseline, and a remote process can
//! hold the cursor and interpret the reply itself. Live view is byte-identical to a `--dump` of
//! the same prefix — the provisional carries the same joined blocks a commit would fold.)

use crate::model::Block;
use serde::{Deserialize, Serialize};

/// A client's serializable read position — four numbers. `committed_id` is durable and monotonic
/// (committed is append-only for the whole session); `provisional_gen` identifies the current
/// provisional generation (bumped by an in-place back-patch, not by an append); `provisional_index`
/// is the append position within that generation and resets when the generation changes or the open
/// turn commits; `epoch` is session validity (a mismatch ⇒ resync).
///
/// A future optimization adds a 5th member `provisional_gen_prefix` — the position after the last
/// *unchanged* provisional block within the current gen — so a poll can serve from
/// `min(provisional_index, prefix)` and skip resending the stable head; the gen bumps (and prefix
/// resets to "all stable") only when the prefix falls below half the provisional length, giving
/// O(log n) bumps under tail-biased patching. Not implemented yet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub epoch: u64,
    pub committed_id: usize,
    pub provisional_gen: u64,
    pub provisional_index: usize,
}

/// A pull result. `*_from` is the index of the first block in each zone — authoritative for
/// placement; the client does `zone.truncate(from); zone.extend(blocks)`, adopts `provisional_gen`,
/// and renders. The provisional blocks are already server-patched (joined), so the client never
/// patches. An idle tick carries empty `committed`/`provisional`.
#[derive(Debug, Clone, PartialEq)]
pub struct PullReply {
    /// Current session epoch. If it differs from the request cursor's, this is a full resync.
    pub epoch: u64,
    /// Index of the first block in `committed` (== the request `committed_id`, or 0 on resync).
    pub committed_from: usize,
    /// New committed blocks — always a pure append after `committed_from`.
    pub committed: Vec<Block>,
    /// The generation of the provisional blocks in this reply — the client adopts it as its
    /// `provisional_gen`. A change from the request cursor's means the provisional was resent whole.
    pub provisional_gen: u64,
    /// Index of the first block in `provisional`: the request `provisional_index` on an append
    /// within the same gen; `0` on a gen change, a commit, or a resync.
    pub provisional_from: usize,
    /// The open-turn blocks from `provisional_from` — already joined/patched by the server.
    pub provisional: Vec<Block>,
}

impl Cursor {
    /// Encode as a compact query value: `epoch.committed_id.provisional_gen.provisional_index`.
    /// Round-trips with [`from_query`](Self::from_query); the four fields are the whole cursor.
    pub fn to_query(self) -> String {
        format!(
            "{}.{}.{}.{}",
            self.epoch, self.committed_id, self.provisional_gen, self.provisional_index
        )
    }

    /// Parse [`to_query`](Self::to_query)'s form. Any malformed input yields the default cursor
    /// (`epoch == 0`), which the server treats as a resync — so a missing/garbled cursor is safe.
    pub fn from_query(s: &str) -> Cursor {
        let p: Vec<&str> = s.split('.').collect();
        if p.len() == 4 {
            if let (Ok(epoch), Ok(committed_id), Ok(provisional_gen), Ok(provisional_index)) =
                (p[0].parse(), p[1].parse(), p[2].parse(), p[3].parse())
            {
                return Cursor {
                    epoch,
                    committed_id,
                    provisional_gen,
                    provisional_index,
                };
            }
        }
        Cursor::default()
    }
}

impl PullReply {
    /// Whether this reply carries no blocks (a pure idle tick).
    pub fn is_idle(&self) -> bool {
        self.committed.is_empty() && self.provisional.is_empty()
    }

    /// The cursor a client holds after applying this reply.
    pub fn next_cursor(&self) -> Cursor {
        Cursor {
            epoch: self.epoch,
            committed_id: self.committed_from + self.committed.len(),
            provisional_gen: self.provisional_gen,
            provisional_index: self.provisional_from + self.provisional.len(),
        }
    }
}

/// Compute the reply for `cursor` against the live shared state: `epoch`, the append-only
/// `committed` slice, the current open-turn `provisional` (already patched in place by the server),
/// and its `provisional_gen`. Committed progress takes priority — when committed grew, the old
/// provisional the client held *became* that committed range, so the provisional resets. Otherwise a
/// gen change (an in-place patch may have altered a held block) forces a whole-provisional resend;
/// an unchanged gen serves the append-only suffix.
pub fn pull(
    epoch: u64,
    committed: &[Block],
    provisional: &[Block],
    provisional_gen: u64,
    cursor: Cursor,
) -> PullReply {
    if cursor.epoch != epoch {
        // Resync: everything from 0, at the current provisional generation.
        return PullReply {
            epoch,
            committed_from: 0,
            committed: committed.to_vec(),
            provisional_gen,
            provisional_from: 0,
            provisional: provisional.to_vec(),
        };
    }
    let committed_from = cursor.committed_id.min(committed.len());
    let committed_grew = committed.len() > committed_from;
    // provisional_from: 0 on a commit (old provisional became committed) or a gen change (an
    // in-place patch may have altered a block the client holds ⇒ resend whole); else the
    // append-only suffix within the same generation.
    let provisional_from = if committed_grew || cursor.provisional_gen != provisional_gen {
        0
    } else {
        cursor.provisional_index.min(provisional.len())
    };
    PullReply {
        epoch,
        committed_from,
        committed: committed[committed_from..].to_vec(),
        provisional_gen,
        provisional_from,
        provisional: provisional[provisional_from..].to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(s: &str) -> Block {
        Block::AssistantText(s.into())
    }

    fn cur(
        epoch: u64,
        committed_id: usize,
        provisional_gen: u64,
        provisional_index: usize,
    ) -> Cursor {
        Cursor {
            epoch,
            committed_id,
            provisional_gen,
            provisional_index,
        }
    }

    #[test]
    fn idle_returns_empty_zones_at_the_current_ends() {
        // Same epoch, gen, and both ends ⇒ nothing to send; cursor unchanged.
        let r = pull(1, &[b("a"), b("b")], &[b("p0")], 3, cur(1, 2, 3, 1));
        assert!(r.is_idle());
        assert_eq!(r.committed_from, 2);
        assert_eq!(r.provisional_gen, 3);
        assert_eq!(r.provisional_from, 1);
        assert_eq!(r.next_cursor(), cur(1, 2, 3, 1));
    }

    #[test]
    fn provisional_append_same_gen_serves_only_the_new_suffix() {
        // Open turn grew 1 → 3 by pure append (new tool_use blocks). Gen unchanged.
        let prov = vec![b("p0"), b("p1"), b("p2")];
        let r = pull(1, &[b("a")], &prov, 3, cur(1, 1, 3, 1));
        assert_eq!(r.committed, Vec::<Block>::new());
        assert_eq!(r.provisional_gen, 3);
        assert_eq!(r.provisional_from, 1);
        assert_eq!(r.provisional, vec![b("p1"), b("p2")]);
        assert_eq!(r.next_cursor(), cur(1, 1, 3, 3));
    }

    #[test]
    fn gen_bump_resends_the_whole_provisional() {
        // A back-patch touched an existing provisional block ⇒ server bumped gen 3 → 4. No commit.
        // The client's index is stale within the old gen, so it re-reads the whole (patched) turn.
        let prov = vec![b("p0-patched"), b("p1"), b("p2")];
        let r = pull(1, &[b("a")], &prov, 4, cur(1, 1, 3, 2));
        assert_eq!(r.committed, Vec::<Block>::new());
        assert_eq!(r.provisional_gen, 4);
        assert_eq!(r.provisional_from, 0, "gen changed ⇒ resend from 0");
        assert_eq!(r.provisional, prov);
        assert_eq!(r.next_cursor(), cur(1, 1, 4, 3));
    }

    #[test]
    fn commit_appends_committed_and_resets_provisional() {
        // Two turns committed since committed_id=2; the old provisional became committed and a new
        // open turn started at a fresh gen. Commit wins regardless of the cursor's stale gen/index.
        let committed = vec![b("a"), b("b"), b("c"), b("d")];
        let prov = vec![b("new-open")];
        let r = pull(1, &committed, &prov, 5, cur(1, 2, 3, 4));
        assert_eq!(r.committed_from, 2);
        assert_eq!(r.committed, vec![b("c"), b("d")]);
        assert_eq!(r.provisional_gen, 5);
        assert_eq!(
            r.provisional_from, 0,
            "old provisional became committed ⇒ reset"
        );
        assert_eq!(r.provisional, vec![b("new-open")]);
        assert_eq!(r.next_cursor(), cur(1, 4, 5, 1));
    }

    #[test]
    fn cursor_query_round_trips_and_tolerates_garbage() {
        let c = cur(3, 12, 7, 4);
        assert_eq!(c.to_query(), "3.12.7.4");
        assert_eq!(Cursor::from_query("3.12.7.4"), c);
        // Malformed ⇒ default (epoch 0 ⇒ resync), never a panic.
        assert_eq!(Cursor::from_query(""), Cursor::default());
        assert_eq!(Cursor::from_query("1.2.3"), Cursor::default());
        assert_eq!(Cursor::from_query("a.b.c.d"), Cursor::default());
        assert_eq!(Cursor::from_query("1.2.3.4.5"), Cursor::default());
    }

    #[test]
    fn epoch_mismatch_resyncs_from_zero() {
        let r = pull(2, &[b("a")], &[b("open")], 0, cur(1, 7, 9, 3));
        assert_eq!(r.epoch, 2);
        assert_eq!(r.committed_from, 0, "resync, not the stale committed_id");
        assert_eq!(r.committed, vec![b("a")]);
        assert_eq!(r.provisional_gen, 0);
        assert_eq!(r.provisional_from, 0);
        assert_eq!(r.next_cursor(), cur(2, 1, 0, 1));
    }
}
