//! The pull-based streaming protocol (design §9a).
//!
//! A client holds a serializable [`Cursor`] and pulls [`PullDelta`]s; the server serves either new
//! **committed** blocks (append-only — never a rewind) or the refreshed **provisional** (open) turn.
//! This replaces the server-side snapshot diff (`html_export::serve::stream_delta`): the *client*
//! tracks its own position, so the server keeps no per-client baseline, and a remote process can
//! hold the cursor and decide what to read next.
//!
//! ## Why the second number is a *generation*, not a block index
//! The design sketched `Cursor(committed_id, provisional_index)`. Building it surfaced that the open
//! turn is **not append-only**: a tool block in the current turn back-patches — its output fills in
//! when the `tool_result` arrives — **without adding a block**. A raw `provisional_index` (a count)
//! can't see that same-length content change, so a client would show a stale, output-less tool
//! call. So the second coordinate is a **`provisional_gen`**: a version the server bumps on *any*
//! open-turn change (append, back-patch, or regroup). It's still two positions + an epoch; only the
//! second position's meaning changed from "index" to "generation." (Committed stays a true index —
//! it's append-only, so it can't have this problem.)

use crate::model::Block;
use serde::{Deserialize, Serialize};

/// A client's serializable read position. `committed_id` is a durable, monotonic anchor into the
/// append-only committed log (safe for a remote / across restart). `provisional_gen` is the version
/// of the open turn last seen (see the module note). `epoch` is the session validity token — a
/// mismatch means the source was truncated/reset and the client must resync from 0.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    /// Session validity; a mismatch vs. the live session ⇒ resync.
    pub epoch: u64,
    /// Committed blocks already consumed (append-only ⇒ monotonic, never rewinds).
    pub committed_id: usize,
    /// The open-turn version last seen (bumps on any provisional change).
    pub provisional_gen: u64,
}

/// The result of a pull — what the client applies to its rendered stream.
#[derive(Debug, Clone, PartialEq)]
pub enum PullDelta {
    /// Nothing changed since the cursor.
    Idle,
    /// **Append** `committed` (new committed blocks since the cursor — may be empty when only the
    /// open turn changed) and **replace** the provisional tail with `provisional`. Committed is
    /// append-only, so `committed` is always a pure append, never a rewind.
    Update {
        committed: Vec<Block>,
        provisional: Vec<Block>,
    },
    /// The session reset (epoch mismatch / source truncation): discard everything and render fresh.
    Resync {
        committed: Vec<Block>,
        provisional: Vec<Block>,
    },
}

/// Compute the delta for `cursor` against the current shared state: the session `epoch`, the
/// `committed` block slice (append-only), the current `provisional` (open turn), and its `gen`.
/// Returns the delta and the client's **next** cursor. Committed progress takes priority — a pull
/// never serves stale provisional deltas from before a commit; it serves the new committed range
/// and a fresh provisional together.
pub fn pull(
    epoch: u64,
    committed: &[Block],
    provisional: &[Block],
    gen: u64,
    cursor: Cursor,
) -> (PullDelta, Cursor) {
    let next = Cursor {
        epoch,
        committed_id: committed.len(),
        provisional_gen: gen,
    };
    if cursor.epoch != epoch {
        return (
            PullDelta::Resync {
                committed: committed.to_vec(),
                provisional: provisional.to_vec(),
            },
            next,
        );
    }
    let committed_grew = committed.len() > cursor.committed_id;
    let provisional_changed = gen != cursor.provisional_gen;
    if !committed_grew && !provisional_changed {
        return (PullDelta::Idle, cursor);
    }
    // Serve the new committed range (empty if only the open turn changed) + the full current
    // provisional (replace). On a commit (`committed_grew`), the old provisional the client held
    // *became* part of this committed range, so replacing it is exactly the discard.
    (
        PullDelta::Update {
            committed: committed[cursor.committed_id..].to_vec(),
            provisional: provisional.to_vec(),
        },
        next,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Block {
        Block::AssistantText(s.into())
    }

    #[test]
    fn idle_when_nothing_changed() {
        let committed = vec![t("a"), t("b")];
        let prov = vec![t("open")];
        let cursor = Cursor {
            epoch: 1,
            committed_id: 2,
            provisional_gen: 5,
        };
        let (delta, next) = pull(1, &committed, &prov, 5, cursor);
        assert_eq!(delta, PullDelta::Idle);
        assert_eq!(next, cursor, "cursor unchanged on idle");
    }

    #[test]
    fn committed_advance_appends_and_replaces_provisional() {
        // Client saw committed[..2] + an old provisional; two turns committed since.
        let committed = vec![t("a"), t("b"), t("c"), t("d")];
        let prov = vec![t("new-open")];
        let cursor = Cursor {
            epoch: 1,
            committed_id: 2,
            provisional_gen: 5,
        };
        let (delta, next) = pull(1, &committed, &prov, 9, cursor);
        assert_eq!(
            delta,
            PullDelta::Update {
                committed: vec![t("c"), t("d")],  // only the new committed range
                provisional: vec![t("new-open")], // fresh provisional (old one discarded)
            }
        );
        assert_eq!(next.committed_id, 4);
        assert_eq!(next.provisional_gen, 9);
    }

    #[test]
    fn provisional_change_without_commit_replaces_only_the_open_turn() {
        // A back-patch: same committed, same provisional length, but the gen bumped (tool output
        // filled in). Committed range is empty; the provisional is replaced.
        let committed = vec![t("a"), t("b")];
        let prov = vec![t("tool: (now with output)")];
        let cursor = Cursor {
            epoch: 1,
            committed_id: 2,
            provisional_gen: 5,
        };
        let (delta, next) = pull(1, &committed, &prov, 6, cursor);
        assert_eq!(
            delta,
            PullDelta::Update {
                committed: vec![],
                provisional: prov.clone(),
            }
        );
        assert_eq!(next.committed_id, 2);
        assert_eq!(next.provisional_gen, 6);
    }

    #[test]
    fn epoch_mismatch_resyncs_from_zero() {
        let committed = vec![t("a")];
        let prov = vec![t("open")];
        let stale = Cursor {
            epoch: 1,
            committed_id: 7,
            provisional_gen: 3,
        };
        let (delta, next) = pull(2, &committed, &prov, 1, stale);
        assert_eq!(
            delta,
            PullDelta::Resync {
                committed: vec![t("a")],
                provisional: vec![t("open")],
            }
        );
        assert_eq!(next.epoch, 2);
        assert_eq!(next.committed_id, 1);
    }
}
