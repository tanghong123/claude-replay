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

```bash
cargo install --path .          # → ~/.cargo/bin/claude-replay
# or
cargo build --release           # → target/release/claude-replay
```

A Homebrew release is planned (see [Roadmap](#roadmap)).

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
`t` toggle the focused/first-visible fold · `T` toggle all · `]`/`[` next/prev foldable ·
`Enter` toggle focused · `/` search, `n`/`N` next/prev · `?` help · `q`/`Esc` quit.

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
- Homebrew tap + formula for a `brew install` release; once shipped,
  `claude-toolbox`'s `claude-jdi` installer switches to the brewed `claude-replay`
  with bash `claude-peek` as the fallback.

See [`DESIGN.md`](DESIGN.md) for the phased plan and design notes, and
[`ATTRIBUTION.md`](ATTRIBUTION.md) for borrowed ideas.

## License

MIT
