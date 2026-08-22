//! claude-replay — interactive, read-only AI-agent transcript viewer.
//!
//! Like `claude --resume`, but you can only read: scroll, fold, search, live-tail.
//! All logic lives in the `claude_replay` library (shared with `agent-jdi`).

fn main() -> anyhow::Result<()> {
    claude_replay::run_viewer()
}
