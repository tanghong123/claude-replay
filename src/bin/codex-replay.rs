fn main() -> anyhow::Result<()> {
    claude_replay::run(claude_replay::Backend::Codex)
}
