//! agent-jdi — supervise unattended AI-agent runs (Claude, Codex, …) and follow
//! them with the agent-replay viewer. Multi-agent, auto-detecting.

fn main() -> anyhow::Result<()> {
    claude_replay::jdi::run()
}
