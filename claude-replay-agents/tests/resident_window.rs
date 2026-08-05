//! **The open window stays O(one turn)** — on real transcripts, not fixtures. `#[ignore]`d,
//! since it needs whatever sessions the developer has on disk.
//!
//! `cargo test -p claude-replay-agents --test resident_window -- --ignored --nocapture`
//!
//! This is the invariant #74/#84/#85 all rest on: committed blocks leave RAM for the store, and
//! only the open turn stays resident. A back-reference that pins the durability frontier breaks
//! it *silently* — the session still renders perfectly, it just never commits anything again —
//! so nothing fails until someone measures. Before the frontier's pins were bounded, one `Skill`
//! call left **3218 of 12466 blocks (26%)** resident for the rest of the session, re-finalized
//! and re-diffed on every 250 ms tick.

use claude_replay_agents::ClaudeAdapter;
use claude_replay_core::{discover, Agent};
use claude_replay_engine::SessionAccumulator;

/// Sessions this size are where an O(session) window would be obvious — and large enough that a
/// legitimately long open turn cannot plausibly account for the count.
const BIG: usize = 500;
/// A generous ceiling on a legitimate open window: one turn, plus the bounded queue pin.
const MAX_RESIDENT: usize = 200;

#[test]
#[ignore]
fn the_open_window_never_grows_to_o_of_session() {
    let mut paths: Vec<_> = discover::candidates_all(None)
        .into_iter()
        .map(|c| c.path)
        .collect();
    paths.sort_by_key(|p| std::cmp::Reverse(std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)));
    paths.truncate(10);
    assert!(!paths.is_empty(), "no real transcripts discovered");

    let mut checked = 0;
    for p in &paths {
        if discover::detect_agent(p) != Agent::CLAUDE {
            continue;
        }
        let Ok(data) = std::fs::read_to_string(p) else {
            continue;
        };
        let mut acc = SessionAccumulator::new(&ClaudeAdapter);
        let mut off: u64 = 0;
        for l in data.lines() {
            acc.advance_at(off, l);
            off += l.len() as u64 + 1;
        }
        let total = acc.snapshot().blocks().len();
        let resident = total - acc.committed_len();
        println!(
            "{total:>6} blocks  {resident:>4} resident ({:.1}%)  {}",
            100.0 * resident as f64 / total.max(1) as f64,
            p.file_name().unwrap().to_string_lossy()
        );
        if total >= BIG {
            checked += 1;
            assert!(
                resident <= MAX_RESIDENT,
                "the frontier is pinned: {resident} of {total} blocks still resident in {p:?}"
            );
        }
    }
    assert!(checked > 0, "no session large enough to be meaningful");
}
