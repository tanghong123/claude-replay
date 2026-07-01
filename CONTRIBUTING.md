# Contributing to claude-replay

Thanks for your interest! This is a small, focused Rust + [ratatui](https://ratatui.rs)
TUI. Contributions are welcome — bug reports, fixes, and well-scoped features.

## Development

```bash
cargo build                                 # debug build
cargo run -- --latest                       # run against your latest transcript
```

The viewer is **fully testable headless (no TTY)** — never skip or stub a feature
"because it needs a terminal." All viewer state lives in `view::View`, driven under
ratatui's `TestBackend` in unit tests, and `claude-replay <path> --dump -` renders a
transcript to stdout for quick parsing/markdown checks. See `CLAUDE.md` for the full
testing playbook and `DESIGN.md` for the architecture.

## Every change must pass the gate

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
```

CI runs exactly these on every push and PR. Add a `TestBackend` or unit test for each
behavior change.

## Install the git hooks (required)

This repo ships hooks that block committing/pushing secrets or non-project (e.g.
corporate) email addresses. Enable them once per clone:

```bash
git config core.hooksPath .githooks
```

The same checks run in CI as a backstop, so a push that bypasses local hooks will
still fail there.

## Commit & PR conventions

- **Conventional commits** (`feat:`, `fix:`, `refactor:`, `docs:`, `test:`, …).
- Keep commits small and reviewable; one logical change per commit.
- Match the surrounding code's style, comment density, and idioms.
- Never commit real Claude session transcripts (`*.jsonl`), captured fixtures, or
  scratch notes — they may contain private content and are `.gitignore`d.

## Reporting bugs

Open an issue with the smallest transcript snippet that reproduces it (redact
anything private), the terminal width, and what you saw vs. expected. A
`claude-replay <path> --dump - --width N` excerpt is ideal.

## License

By contributing, you agree that your contributions are licensed under the
[MIT License](LICENSE).
