# claude-replay

An interactive, **read-only** viewer for Claude Code session transcripts — *like
`claude --resume`, but you can only read*: scroll, fold, search, and live-tail the
JSONL files Claude Code writes under `~/.claude/projects/`. A Rust + [ratatui](https://ratatui.rs)
TUI that renders a session the way Claude Code does (assistant text, thinking,
tool calls, `+/-` diffs, markdown, syntect-highlighted code) without ever
continuing or mutating the session.

> Extracted from [`claude-toolbox`](https://github.com/tanghong123/claude-toolbox)
> (the `justdoit/` module), where it began life as `claude-peek-v2`. Its bash
> predecessor, `claude-peek`, still ships there.

## Install

**Homebrew** (macOS / Linux) — recommended:

```bash
brew install tanghong123/tap/claude-replay
```

This taps `tanghong123/tap` and puts the `claude-replay` command on your `PATH`, so
you can run it from anywhere:

```bash
claude-replay --latest      # open your most recent session
claude-replay --version
```

On macOS and Linux (x86_64 and arm64) this downloads a prebuilt binary — no Rust
toolchain, no compile. Later, `brew upgrade claude-replay` updates it and
`brew uninstall claude-replay` removes it. (Equivalent two-step: `brew tap
tanghong123/tap` then `brew install claude-replay`.)

**Prebuilt binary** (no Homebrew, no Rust) — `cargo-binstall` grabs the release
tarball for your platform:

```bash
cargo binstall claude-replay
```

Or download a `claude-replay-<target>.tar.gz` from the
[releases page](https://github.com/tanghong123/claude-replay/releases) directly
(static musl builds for Linux; run on any distro).

**From source** (needs a Rust toolchain):

```bash
cargo install --path .          # → ~/.cargo/bin/claude-replay
# or
cargo build --release           # → target/release/claude-replay
```

## Codex version: `codex-replay` + `codex-jdi`

The same read-only TUI is available for local Codex sessions, with a supervisor
that continues an exited interactive session in the background.

First install the official Codex CLI on macOS or Linux and sign in:

```bash
curl -fsSL https://chatgpt.com/codex/install.sh | sh
codex login
codex --version
```

Official alternative install methods are `npm install -g @openai/codex` and
`brew install --cask codex`. Then install both Codex replay commands:

```bash
brew install tanghong123/tap/codex-replay
codex-replay --version
codex-jdi --version
```

The intended handoff is:

1. Finish or pause work in interactive Codex and enter `/quit`.
2. Stay in the same repository directory.
3. Run `codex-jdi resume`.

`codex-jdi` finds the newest interactive Codex session recorded for that exact
working directory, resumes its UUID with `codex exec resume`, starts the worker
without a TUI, and immediately opens `codex-replay --follow` on the same rollout.
Press `q` to leave the viewer; the Codex worker keeps running. Reattach later:

```bash
codex-jdi log
```

An extra instruction can be appended without replacing the built-in persistence
prompt:

```bash
codex-jdi resume "Prioritize tests and finish the smallest complete slice"
```

The headless worker uses `approval_policy="never"` so it cannot stop on a hidden
approval prompt, plus `sandbox_mode="workspace-write"` so it can edit the current
repository while retaining the filesystem sandbox. It never enables
`--dangerously-bypass-approvals-and-sandbox` implicitly. If an operation is
blocked, the built-in prompt tells Codex to try safe alternatives and record the
remaining blocker.

Use the viewer directly when no continuation is needed:

```bash
codex-replay                         # current-repository session picker
codex-replay --latest                # newest local Codex rollout
codex-replay <session-id> -f         # follow one session by UUID
codex-replay path/to/rollout -v      # open an explicit rollout, fully expanded
codex-replay --latest --dump -       # plain text for pipes
```

Install the Codex commands from this checkout with Rust:

```bash
cargo install --path . --bin codex-replay --bin codex-jdi
```

## Usage

```
claude-replay <session-id | path/to.jsonl>   render that transcript
claude-replay --latest                        the most-recently-active transcript
claude-replay <id> -f                         follow a running session live
claude-replay <id|--latest> --dump -          plain text to stdout (no TUI) — for pipes/tests
claude-replay <id|--latest> --dump [stem]     write <stem>.txt + <stem>.ansi (deduced stem if omitted)
claude-replay <id|--latest> --dump --width N  dump at width N (default: terminal width, else 100)
claude-replay <id|--latest> --dump --full     dump with everything expanded (default folds like the TUI)
```

`--dump` renders through the same pipeline as the live viewer and applies the same
default fold policy, so its output matches what the TUI shows (add `--full` to expand
every block).

Default view: user turns (`❯`), assistant text (`⏺`), `✻` thinking summaries, and
code-**modifying** actions (Edit/Write/MultiEdit + mutating Bash) with each edit as
a red/green `-`/`+` diff. Non-modifying ops and tool output are hidden to stay
skimmable; reveal with `--reads`, `--results`, `-v`/`--full`. Per-type fold control
via `--fold`/`--unfold` (`user, assistant, thinking, read, bash, edit, write, tool,
tool_result, command`).

### Keys
`j`/`k` line · `C-d`/`C-u` half-page · `PageDown`/`PageUp` page · `g`/`G` top/bottom ·
`Space` toggle the focused/first-visible fold (`Enter` toggles the focused one) ·
`T` toggle all · `]`/`[` next/prev foldable · `/` search, `n`/`N` next/prev ·
`?` help · `q` quit. When launched from the session picker (more than one session),
`Esc` returns to that list to pick another; otherwise `Esc` quits too. After
`--latest`, `s` opens the session switcher (a picker overlay) so you can hop to
another session — `Enter` switches, `Esc` returns to where you were.

## Develop

It is **fully testable headless (no TTY)** — see [`CLAUDE.md`](CLAUDE.md).

```bash
cargo fmt --check
cargo clippy --all-targets
cargo test                                  # deterministic; no terminal needed
cargo test --test tmux_smoke -- --ignored   # opt-in end-to-end via private tmux
```

The golden visual-parity fixtures **and** the comparison harness live in a separate
private repo, `claude-replay-eval` (they contain real Claude session content and are
kept out of this tree). It holds `golden/cc.scroll.{txt,ansi}` (Claude Code's own
render), `capture-golden.sh` (mint a golden from a session id via real `claude
--resume`), `capture-peek.sh` (drive this viewer), `stitch-frames.py`, and
`compare-scroll.py`. See `DESIGN.md` › "Visual-parity harness".

## Roadmap

- Lazy/viewport-only syntax highlighting (large transcripts open instantly).
- Point `claude-toolbox`'s `claude-jdi` installer at the brewed `claude-replay`
  (with bash `claude-peek` as the fallback).

See [`DESIGN.md`](DESIGN.md) for the phased plan and design notes, and
[`ATTRIBUTION.md`](ATTRIBUTION.md) for borrowed ideas.

## License

MIT
