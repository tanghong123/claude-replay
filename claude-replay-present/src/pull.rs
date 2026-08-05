//! The pull-based streaming protocol (design §9a) — **server-side auto-patch**.
//!
//! Both halves live here: [`pull`] (the server's reply computation) and [`PullClient`] (the
//! client's apply state machine — the executable specification any detached consumer follows;
//! the JS client in `html/export.js` mirrors it transition for transition).
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
//!   the **finalized** provisional stops being append-only from the client's perspective — an
//!   in-place patch, or a finalization reshape (grouping/absorption) rewriting the prefix (#54).
//!   Within a generation the served provisional is append-only, so the client's
//!   `provisional_index` stays a valid suffix pointer.
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

    /// The cursor addressing this reply's FIRST blocks — where the client applies it: the
    /// zone start positions under the reply's epoch/gen. [`next_cursor`](Self::next_cursor)
    /// is the same cursor advanced past the payload.
    pub fn start_cursor(&self) -> Cursor {
        Cursor {
            epoch: self.epoch,
            committed_id: self.committed_from,
            provisional_gen: self.provisional_gen,
            provisional_index: self.provisional_from,
        }
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

/// What one [`PullClient::apply`] did to the client's joined view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Applied {
    /// Index (in the joined `committed ++ provisional` view) of the first block the consumer
    /// must consider changed — `None` on an idle tick. Conservative on a commit: the blocks
    /// that crossed from provisional to committed may finalize content-identical, but they are
    /// still reported (mirroring the wire, which resends them as committed). Feed this to a
    /// `View::apply_from`-style renderer.
    pub first_changed: Option<usize>,
    /// The reply's epoch differed from ours — the whole view was rebuilt from scratch.
    pub resync: bool,
}

/// The **client half** of the pull protocol — the executable specification of the cursor
/// semantics. The server half is [`pull`]; the JS client in `html/export.js` necessarily
/// reimplements this logic (it runs in the browser) and must match it transition for
/// transition — the tests on this type are the reference the JS mirrors.
///
/// Content-blind and protocol-aware, exactly like the JS client: it holds the two tracked
/// zones and applies one rule per zone — *truncate to `*_from`, then extend* — plus the
/// epoch-resync rule. A decoupled TUI (worker thread or remote process) drives its `View`
/// with the [`Applied::first_changed`] this returns, the same way the in-process viewer
/// consumes `FollowParser::poll_delta`'s `changed_from`.
#[derive(Debug, Default, Clone)]
pub struct PullClient {
    cursor: Cursor,
    committed: Vec<Block>,
    provisional: Vec<Block>,
}

impl PullClient {
    /// A fresh client. The default cursor (`epoch == 0`) makes the first pull a full resync —
    /// real epochs start at 1, so no handshake is needed.
    pub fn new() -> Self {
        Self::default()
    }

    /// The cursor to send with the next pull request.
    pub fn cursor(&self) -> Cursor {
        self.cursor
    }

    /// The committed zone (append-only from the client's perspective).
    pub fn committed(&self) -> &[Block] {
        &self.committed
    }

    /// The provisional zone (the open turn — replaced or extended per reply).
    pub fn provisional(&self) -> &[Block] {
        &self.provisional
    }

    /// The joined view: `committed ++ provisional` — what a renderer displays.
    pub fn blocks(&self) -> impl Iterator<Item = &Block> {
        self.committed.iter().chain(self.provisional.iter())
    }

    /// Total blocks in the joined view.
    pub fn len(&self) -> usize {
        self.committed.len() + self.provisional.len()
    }

    /// Whether the joined view is empty.
    pub fn is_empty(&self) -> bool {
        self.committed.is_empty() && self.provisional.is_empty()
    }

    /// Apply one reply: per zone *truncate to `*_from`, then extend*; on an epoch change,
    /// rebuild from scratch. Adopts the reply's cursor ([`PullReply::next_cursor`]) so the
    /// next request continues from here.
    pub fn apply(&mut self, r: &PullReply) -> Applied {
        let resync = r.epoch != self.cursor.epoch;
        // Idle tick (same epoch, both zones empty): nothing to do, cursor unchanged.
        if !resync && r.is_idle() {
            return Applied {
                first_changed: None,
                resync: false,
            };
        }
        let mut first = usize::MAX;
        // Committed zone. On a resync `committed_from == 0`, so the truncate clears it.
        if resync || !r.committed.is_empty() {
            first = first.min(r.committed_from);
            self.committed.truncate(r.committed_from);
            self.committed.extend(r.committed.iter().cloned());
        }
        // Provisional zone. Anything we held at/after `provisional_from` is stale (a commit
        // moved it into committed; a gen bump resent it patched) — mirror the wire even when
        // the new suffix is empty.
        let stale = self.provisional.len() > r.provisional_from;
        if stale || !r.provisional.is_empty() {
            first = first.min(self.committed.len() + r.provisional_from);
            self.provisional.truncate(r.provisional_from);
            self.provisional.extend(r.provisional.iter().cloned());
        }
        self.cursor = r.next_cursor();
        debug_assert_eq!(self.cursor.committed_id, self.committed.len());
        debug_assert_eq!(self.cursor.provisional_index, self.provisional.len());
        Applied {
            first_changed: (first != usize::MAX).then_some(first),
            resync,
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
    let (committed_from, provisional_from) = pull_indices(
        epoch,
        committed.len(),
        provisional.len(),
        provisional_gen,
        cursor,
    );
    PullReply {
        epoch,
        committed_from,
        committed: committed[committed_from..].to_vec(),
        provisional_gen,
        provisional_from,
        provisional: provisional[provisional_from..].to_vec(),
    }
}

/// The zone start indices [`pull`] would produce — the index math **without** the block payloads,
/// from the zone *lengths* alone. Lets a caller decide idleness cheaply (both `*_from` at their
/// zone end + a matching epoch ⇒ nothing to send) *before* paying an O(N) clone/render. On an epoch
/// mismatch both are `0` (a full resync). A commit (committed grew past the cursor) or a gen change
/// resets `provisional_from` to `0`; else it is the append-only suffix within the generation.
pub fn pull_indices(
    epoch: u64,
    n_committed: usize,
    n_provisional: usize,
    provisional_gen: u64,
    cursor: Cursor,
) -> (usize, usize) {
    if cursor.epoch != epoch {
        return (0, 0); // resync: everything from 0
    }
    // A cursor AHEAD of the committed prefix, at a matching epoch (#96 §4.3). The committed zone
    // is append-only within an epoch, so this used to be unreachable and was clamped — but a
    // durable resume aligns the content stream DOWN to what the meta stream corroborates, which
    // shrinks `n_committed` without a reset. Clamping would then serve the client blocks it
    // already has while silently skipping the ones it does not; a resync is the honest answer.
    if cursor.committed_id > n_committed {
        return (0, 0);
    }
    let committed_from = cursor.committed_id;
    let committed_grew = n_committed > committed_from;
    let provisional_from = if committed_grew || cursor.provisional_gen != provisional_gen {
        0
    } else {
        cursor.provisional_index.min(n_provisional)
    };
    (committed_from, provisional_from)
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

    /// **The cursor guard** (#96 §4.3). A cursor AHEAD of the committed prefix at a MATCHING
    /// epoch used to be unreachable — the committed zone is append-only within an epoch — and
    /// was silently clamped. A durable resume makes it reachable: alignment cuts the content
    /// stream down to what the record stream corroborates, which SHRINKS `n_committed` without
    /// a reset. Clamping would then hand the client blocks it already has while skipping the
    /// ones it does not; the honest answer is a resync.
    #[test]
    fn a_cursor_ahead_of_committed_resyncs_at_a_matching_epoch() {
        // Ahead by one after an alignment cut: resync, both zones from 0.
        assert_eq!(pull_indices(1, 5, 3, 7, cur(1, 6, 7, 2)), (0, 0));
        // Far ahead — same answer, no clamping.
        assert_eq!(pull_indices(1, 5, 3, 7, cur(1, 99, 7, 0)), (0, 0));
        // Exactly caught up is NOT ahead: serve from where it is, provisional preserved.
        assert_eq!(pull_indices(1, 5, 3, 7, cur(1, 5, 7, 2)), (5, 2));
        // Behind is the ordinary case and must still stream forward, not resync.
        assert_eq!(pull_indices(1, 5, 3, 7, cur(1, 2, 7, 0)), (2, 0));
    }

    /// The **client-side specification**: a `PullClient` driven through every protocol
    /// transition against a simulated server, asserting after each step that (a) the joined
    /// view equals the server's, (b) the cursor is caught up (the next pull is idle), and
    /// (c) `first_changed` points at the first joined index a renderer must re-draw. The JS
    /// client (`html/export.js`) mirrors these transitions one for one.
    #[test]
    fn pull_client_walks_every_transition_like_the_js_client() {
        // A simulated server: (epoch, committed, provisional, gen).
        let mut committed = vec![b("c0")];
        let mut prov = vec![b("p0")];
        let (mut epoch, mut gen) = (1u64, 3u64);
        let mut client = PullClient::new();
        let joined = |c: &PullClient| c.blocks().cloned().collect::<Vec<_>>();
        let server = |committed: &[Block], prov: &[Block]| {
            committed
                .iter()
                .chain(prov.iter())
                .cloned()
                .collect::<Vec<_>>()
        };

        // 1) First contact: default cursor (epoch 0) ⇒ full resync snapshot.
        let a = client.apply(&pull(epoch, &committed, &prov, gen, client.cursor()));
        assert_eq!(
            a,
            Applied {
                first_changed: Some(0),
                resync: true
            }
        );
        assert_eq!(joined(&client), server(&committed, &prov));
        assert!(pull(epoch, &committed, &prov, gen, client.cursor()).is_idle());

        // 2) Idle tick: nothing changes, cursor unchanged.
        let before = client.cursor();
        let a = client.apply(&pull(epoch, &committed, &prov, gen, client.cursor()));
        assert_eq!(
            a,
            Applied {
                first_changed: None,
                resync: false
            }
        );
        assert_eq!(client.cursor(), before);

        // 3) Same-gen provisional append: only the new suffix re-renders.
        prov.push(b("p1"));
        prov.push(b("p2"));
        let a = client.apply(&pull(epoch, &committed, &prov, gen, client.cursor()));
        assert_eq!(
            a.first_changed,
            Some(2),
            "append starts after committed(1) + prov(1)"
        );
        assert!(!a.resync);
        assert_eq!(joined(&client), server(&committed, &prov));

        // 4) Gen bump (in-place back-patch): the whole provisional re-renders, committed intact.
        prov[0] = b("p0-patched");
        gen += 1;
        let a = client.apply(&pull(epoch, &committed, &prov, gen, client.cursor()));
        assert_eq!(
            a.first_changed,
            Some(1),
            "provisional zone starts after committed(1)"
        );
        assert_eq!(joined(&client), server(&committed, &prov));

        // 5) Commit: the open turn (possibly reshaped) becomes committed; a new turn opens.
        committed.push(b("p0-final"));
        committed.push(b("p1+p2-coalesced"));
        prov = vec![b("q0")];
        gen += 1;
        let a = client.apply(&pull(epoch, &committed, &prov, gen, client.cursor()));
        assert_eq!(
            a.first_changed,
            Some(1),
            "re-render from the old committed frontier (conservative: finalized blocks resend)"
        );
        assert!(!a.resync);
        assert_eq!(joined(&client), server(&committed, &prov));

        // 6) Epoch bump (source truncated / session reset): full resync.
        committed = vec![b("new0")];
        prov = vec![];
        epoch += 1;
        gen = 1;
        let a = client.apply(&pull(epoch, &committed, &prov, gen, client.cursor()));
        assert_eq!(
            a,
            Applied {
                first_changed: Some(0),
                resync: true
            }
        );
        assert_eq!(joined(&client), server(&committed, &prov));
        assert!(pull(epoch, &committed, &prov, gen, client.cursor()).is_idle());
    }

    /// `start_cursor` addresses the reply's first blocks (where it applies);
    /// `next_cursor` is the same cursor advanced past the payload.
    #[test]
    fn start_cursor_addresses_the_replys_first_blocks() {
        let prov = vec![b("p0"), b("p1")];
        let r = pull(1, &[b("a")], &prov, 3, cur(1, 1, 3, 1));
        assert_eq!(r.start_cursor(), cur(1, 1, 3, 1));
        assert_eq!(r.next_cursor(), cur(1, 1, 3, 2));
        // On a resync both zones address 0.
        let r = pull(2, &[b("a")], &prov, 3, cur(1, 1, 3, 1));
        assert_eq!(r.start_cursor(), cur(2, 0, 3, 0));
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
