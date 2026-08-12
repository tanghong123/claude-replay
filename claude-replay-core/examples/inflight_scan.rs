//! Differential probe for `liveness::inflight_tool_in_tail`. No binary depends on it — it exists
//! so a change to the tail scan can be judged against real transcripts rather than fixtures:
//! build it at two revisions, run both over the same corpus, and `diff` the outputs to see
//! exactly which sessions change verdict. Anyone touching that function should rerun it. The
//! failure it guards against — a session mid-tool read as idle, then reaped (#82) — depends on
//! the shape of real tails, so no unit test can be written that would have caught it.
//!
//! Prints one deterministic line per transcript: `<0|1>\t<path>`, sorted by path, so the two
//! runs are directly `diff`-able. Sub-agent transcripts are included: they are transcripts in
//! their own right and exercise the same tail scan.
//!
//! Run: `cargo run --release -p claude-replay-core --example inflight_scan -- <dir>...`

use claude_replay_core::liveness::inflight_tool_in_tail;
use std::path::{Path, PathBuf};

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        match e.file_type() {
            Ok(t) if t.is_dir() => collect(&p, out),
            Ok(t) if t.is_file() && p.extension().is_some_and(|x| x == "jsonl") => out.push(p),
            _ => {}
        }
    }
}

fn main() {
    let roots: Vec<String> = std::env::args().skip(1).collect();
    if roots.is_empty() {
        eprintln!("usage: inflight_scan <dir>...");
        std::process::exit(2);
    }
    let mut files = Vec::new();
    for r in &roots {
        collect(Path::new(r), &mut files);
    }
    files.sort();
    for f in &files {
        println!("{}\t{}", u8::from(inflight_tool_in_tail(f)), f.display());
    }
    eprintln!("scanned {} transcripts", files.len());
}
