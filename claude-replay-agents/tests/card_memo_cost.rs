//! What the memo actually buys, on real transcripts. `#[ignore]`d — it needs real sessions.
use claude_replay_core::engine::seam::CardOutcome;
use claude_replay_core::{discover, Agent};
use std::time::Instant;

#[test]
#[ignore]
fn memo_cost_on_real_transcripts() {
    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        for d in std::fs::read_dir(format!("{home}/.claude/projects"))
            .into_iter()
            .flatten()
            .flatten()
        {
            for f in std::fs::read_dir(d.path()).into_iter().flatten().flatten() {
                let p = f.path();
                if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                    paths.push(p);
                }
            }
        }
    }
    paths.sort();
    assert!(!paths.is_empty(), "no real transcripts");
    let mb: u64 = paths
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum();

    let t = Instant::now();
    let memos: Vec<_> = paths
        .iter()
        .map(
            |p| match discover::session_card_memo(Agent::CLAUDE, p, None) {
                CardOutcome::Fresh { memo, .. } => memo,
                CardOutcome::Unchanged { memo } => Some(memo),
                CardOutcome::Absent => None,
            },
        )
        .collect();
    let cold = t.elapsed();

    let t = Instant::now();
    let mut unchanged = 0;
    for (p, m) in paths.iter().zip(&memos) {
        if matches!(
            discover::session_card_memo(Agent::CLAUDE, p, m.as_ref()),
            CardOutcome::Unchanged { .. }
        ) {
            unchanged += 1;
        }
    }
    let warm = t.elapsed();

    println!(
        "{} transcripts, {:.0} MB\n  cold: {:>9.3} ms total ({:.3} ms/session)\n  warm: {:>9.3} ms total ({:.1} us/session) — {unchanged} answered Unchanged\n  speedup: {:.0}x",
        paths.len(), mb as f64 / 1e6,
        cold.as_secs_f64() * 1e3, cold.as_secs_f64() * 1e3 / paths.len() as f64,
        warm.as_secs_f64() * 1e3, warm.as_secs_f64() * 1e6 / paths.len() as f64,
        cold.as_secs_f64() / warm.as_secs_f64().max(f64::MIN_POSITIVE),
    );
}
