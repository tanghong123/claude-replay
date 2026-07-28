//! The pull-based streaming protocol (design §9a) — **append-only**.
//!
//! Both zones are append-only, so a client's position is just **three indices**:
//! `Cursor { epoch, committed_id, provisional_index }`. The server never mutates a provisional block
//! in place — a status update (a tool's output arriving) is *appended* as another provisional block,
//! and the client **replays** the provisional stream to render. Grouping and result-joining are
//! deferred to **commit**: when the open turn closes, the server folds its provisional blocks (join
//! + `finish_turns`) into committed blocks and resets the provisional stream. So:
//!
//! - **committed** is append-only across the whole session (a block, once committed, never changes);
//! - **provisional** is append-only *within* the current open turn, and **resets** at each commit.
//!
//! A pull is therefore trivial: serve `committed[committed_id..]` and `provisional[provisional_index..]`.
//! The reply is **self-describing** — it carries the actual first index of each zone (`*_from`), which
//! differs from the request only on a **resync** (`epoch` changed ⇒ both `0`) or a **commit** (the old
//! provisional became committed ⇒ `provisional_from = 0`). The client applies one rule per zone:
//! *"truncate to `from`, then append."* No divergence to compute, no O(N²) re-send.
//!
//! (This replaces the server-side snapshot diff in `html_export::serve::stream_delta`: the client
//! tracks its own position, so the server keeps no per-client baseline, and a remote process can hold
//! the cursor and interpret the reply itself. Live view is intentionally *rawer* than a `--dump` —
//! it shows ungrouped, un-joined blocks that coalesce at commit; `--dump` commits/folds the last turn,
//! so it stays byte-identical.)

use crate::model::Block;
use serde::{Deserialize, Serialize};

/// A client's serializable read position — three append indices. `committed_id` is durable and
/// monotonic (committed is append-only for the whole session); `provisional_index` is monotonic
/// *within* the current open turn and resets at each commit; `epoch` is session validity (a mismatch
/// ⇒ resync).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub epoch: u64,
    pub committed_id: usize,
    pub provisional_index: usize,
}

/// A pull result. `*_from` is the index of the first block in each zone — authoritative for
/// placement; the client does `zone.truncate(from); zone.extend(blocks)`, then replays the
/// provisional to render. An idle tick carries empty `committed`/`provisional`.
#[derive(Debug, Clone, PartialEq)]
pub struct PullReply {
    /// Current session epoch. If it differs from the request cursor's, this is a full resync.
    pub epoch: u64,
    /// Index of the first block in `committed` (== the request `committed_id`, or 0 on resync).
    pub committed_from: usize,
    /// New committed blocks — always a pure append after `committed_from`.
    pub committed: Vec<Block>,
    /// Index of the first block in `provisional` (== the request `provisional_index`; 0 on a commit
    /// — the old provisional became committed — or on resync).
    pub provisional_from: usize,
    /// New provisional (open-turn) blocks — append-only within the current turn.
    pub provisional: Vec<Block>,
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
            provisional_index: self.provisional_from + self.provisional.len(),
        }
    }
}

/// Compute the reply for `cursor` against the live shared state (`epoch`, the append-only `committed`
/// slice, and the current open-turn `provisional`). Committed progress takes priority: when committed
/// grew, the old provisional the client held *became* that committed range, so the provisional resets
/// (`provisional_from = 0`).
pub fn pull(epoch: u64, committed: &[Block], provisional: &[Block], cursor: Cursor) -> PullReply {
    if cursor.epoch != epoch {
        // Resync: everything from 0.
        return PullReply {
            epoch,
            committed_from: 0,
            committed: committed.to_vec(),
            provisional_from: 0,
            provisional: provisional.to_vec(),
        };
    }
    let committed_from = cursor.committed_id.min(committed.len());
    let committed_grew = committed.len() > committed_from;
    // A commit ⇒ the client's provisional became committed ⇒ discard it (replace from 0). Otherwise
    // the provisional is append-only within the turn ⇒ serve the new suffix.
    let provisional_from = if committed_grew {
        0
    } else {
        cursor.provisional_index.min(provisional.len())
    };
    PullReply {
        epoch,
        committed_from,
        committed: committed[committed_from..].to_vec(),
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

    fn cur(epoch: u64, committed_id: usize, provisional_index: usize) -> Cursor {
        Cursor {
            epoch,
            committed_id,
            provisional_index,
        }
    }

    #[test]
    fn idle_returns_empty_zones_at_the_current_ends() {
        let r = pull(1, &[b("a"), b("b")], &[b("p0")], cur(1, 2, 1));
        assert!(r.is_idle());
        assert_eq!(r.committed_from, 2);
        assert_eq!(r.provisional_from, 1);
        assert_eq!(r.next_cursor(), cur(1, 2, 1));
    }

    #[test]
    fn provisional_append_serves_only_the_new_suffix() {
        // Open turn grew 1 → 3 (a new tool_use + its appended result). No commit.
        let prov = vec![b("p0"), b("p1"), b("p2")];
        let r = pull(1, &[b("a")], &prov, cur(1, 1, 1));
        assert_eq!(r.committed, Vec::<Block>::new());
        assert_eq!(r.provisional_from, 1);
        assert_eq!(r.provisional, vec![b("p1"), b("p2")]);
        assert_eq!(r.next_cursor(), cur(1, 1, 3));
    }

    #[test]
    fn commit_appends_committed_and_resets_provisional() {
        // Two turns committed since committed_id=2; the old provisional became committed.
        let committed = vec![b("a"), b("b"), b("c"), b("d")];
        let prov = vec![b("new-open")];
        let r = pull(1, &committed, &prov, cur(1, 2, 4));
        assert_eq!(r.committed_from, 2);
        assert_eq!(r.committed, vec![b("c"), b("d")]);
        assert_eq!(
            r.provisional_from, 0,
            "old provisional became committed ⇒ reset"
        );
        assert_eq!(r.provisional, vec![b("new-open")]);
        assert_eq!(r.next_cursor(), cur(1, 4, 1));
    }

    #[test]
    fn epoch_mismatch_resyncs_from_zero() {
        let r = pull(2, &[b("a")], &[b("open")], cur(1, 7, 3));
        assert_eq!(r.epoch, 2);
        assert_eq!(r.committed_from, 0, "resync, not the stale committed_id");
        assert_eq!(r.committed, vec![b("a")]);
        assert_eq!(r.provisional_from, 0);
        assert_eq!(r.next_cursor(), cur(2, 1, 1));
    }
}
