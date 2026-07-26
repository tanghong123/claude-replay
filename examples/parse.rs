//! Minimal library example: parse a transcript into a `Session` and print a one-line
//! summary — no TUI, no HTML, just the `replay-core` data model (design §3.5, example 1).
//!
//! Run: `cargo run --example parse -- <path-to-transcript.jsonl>`

use claude_replay::engine::parse_session;

fn main() -> std::io::Result<()> {
    let path = match std::env::args().nth(1) {
        Some(p) => p,
        None => {
            eprintln!("usage: parse <transcript.jsonl>");
            std::process::exit(2);
        }
    };
    let s = parse_session(std::path::Path::new(&path))?;
    println!(
        "{:?} · {} blocks · {} user turns · ${:.4}",
        s.agent,
        s.blocks.len(),
        s.user_times.len(),
        s.metrics.cost_usd.unwrap_or(0.0),
    );
    Ok(())
}
