//! Minimal library-consumer example for `claude-replay-core`.
//!
//! Shows the two entry points a third-party program uses:
//!   - `parse_session(path)` — parse a whole transcript into a `Session` (one shot).
//!   - `FollowParser::open(adapter, path).poll()` — fold only newly-appended bytes for a live
//!     tail (call `poll()` on a timer; it returns `None` when the file hasn't grown).
//!
//! Run: `cargo run -p claude-replay-core --example parse -- <transcript.jsonl> [--follow]`

use claude_replay_core::{parse_session, Block, FollowParser};
use std::path::Path;

fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: parse <transcript.jsonl> [--follow]");
        std::process::exit(2);
    };
    let follow = args.any(|a| a == "--follow");
    let path = Path::new(&path);

    // One-shot parse: the whole transcript as one agent-neutral value.
    let session = parse_session(path)?;
    println!(
        "agent={:?}  cwd={:?}  {} blocks  {} turns  {} tools  {} sub-agents",
        session.agent,
        session.cwd,
        session.block_count(),
        session.index.turns.len(),
        session.index.tools.len(),
        session.sub_agents.len(),
    );
    println!("metrics: {}", session.metrics.footer());

    // A tiny "renderer": one line per block, labelled by its kind.
    for (i, block) in session.blocks().iter().enumerate() {
        let (kind, preview) = describe(block);
        println!("{i:>4}  {kind:<10}  {preview}");
    }

    // Live tail: fold only the delta each poll. (Here: one extra poll to show the API.)
    if follow {
        println!("\n-- following (Ctrl-C to stop) --");
        let mut follower = FollowParser::open(claude_replay_core::adapter(session.agent), path);
        loop {
            if let Ok(Some((blocks, _times, metrics))) = follower.poll() {
                println!("[poll] {} blocks · {}", blocks.len(), metrics.footer());
            }
            std::thread::sleep(std::time::Duration::from_millis(1000));
        }
    }
    Ok(())
}

/// A one-line label + short preview for a block — the shape a real presenter fills in.
fn describe(b: &Block) -> (&'static str, String) {
    let clip = |s: &str| s.chars().take(60).collect::<String>().replace('\n', " ");
    match b {
        Block::UserText(t) => ("user", clip(t)),
        Block::AssistantText(t) => ("assistant", clip(t)),
        Block::Thinking { text, tools, .. } => (
            "thinking",
            format!("{} tools · {}", tools.len(), clip(text)),
        ),
        Block::ToolUse { name, target, .. } => ("tool", format!("{name}({})", clip(target))),
        Block::ToolResult(t) => ("result", clip(t)),
        Block::Command { name, .. } => ("command", name.clone()),
        Block::SubAgent(sa) => (
            "agent",
            format!("{}: {}", sa.agent_type, clip(&sa.description)),
        ),
        Block::AgentDone { description, .. } => ("agent-done", clip(description)),
        Block::Attachment(a) => ("attachment", a.name.clone()),
        Block::QueueEvent { text } => ("queued", clip(text)),
    }
}
