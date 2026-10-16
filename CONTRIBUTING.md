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

## How a pull request lands

Open a pull request against `main`. The owner's agent reviews it, and when the change is low risk it
merges it for the owner, without waiting for the owner, once:

- CI is green on your latest commit;
- a second, independent review agrees;
- every comment on the pull request is answered;
- your branch is up to date with `main` (the agent merges `main` in when only that is missing).

The agent's findings come to you as comments on the pull request: answer them there, or push a fix.
Merges are fast-forward only, so your commits land as you wrote them.

Some changes still wait for the owner, after the same review: releases and versions, CI and workflow
configuration, authentication, pairing and secret handling, vendored releases and new dependencies,
the public command-line flags and file formats, removing a feature, and larger design changes. If
yours is one, say so in the description and expect a slower merge.

## Reporting bugs

Open an issue with the smallest transcript snippet that reproduces it (redact
anything private), the terminal width, and what you saw vs. expected. A
`claude-replay <path> --dump - --width N` excerpt is ideal.

## License

By contributing, you agree that your contributions are licensed under the
[MIT License](LICENSE).
