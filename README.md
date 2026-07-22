# claude-replay

An interactive, **read-only** viewer for AI coding-agent session transcripts —
*like `claude --resume`, but you can only read*: scroll, fold, search, and
live-tail. Reads both **Claude Code** (`~/.claude/projects/`) and **Codex**
(`~/.codex/sessions/`) transcripts, auto-detecting each. A Rust + [ratatui](https://ratatui.rs)
TUI that renders a session the way the agent does (assistant text, thinking,
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

## Usage

```
claude-replay                                 pick from this dir's sessions (Claude + Codex)
claude-replay <session-id | path/to.jsonl>   render that transcript (agent auto-detected)
claude-replay --latest                        the most-recently-active transcript (any agent)
claude-replay --agent codex                   only show Codex sessions (or --agent claude)
claude-replay <id> -f                         follow a running session live
claude-replay <id|--latest> --dump -          plain text to stdout (no TUI) — for pipes/tests
claude-replay <id|--latest> --dump [stem]     write <stem>.txt + <stem>.ansi (deduced stem if omitted)
claude-replay <id|--latest> --dump --width N  dump at width N (default: terminal width, else 100)
claude-replay <id|--latest> --dump --full     dump with everything expanded (default folds like the TUI)
```

**Multi-agent.** With no argument, the picker merges this directory's sessions from
**every agent** — Claude Code (`~/.claude/projects/`) and Codex
(`~/.codex/sessions/`) — into one list, each row tagged with its agent; one session
opens straight in. The agent for any opened file is auto-detected from its contents,
so an explicit path or `--latest` just works. `--agent claude|codex` filters the
picker/`--latest` to a single agent. (`CODEX_HOME` / `CODEX_SESSIONS_DIR` override
the Codex root.)

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

**Mouse.** Scroll-wheel scrolls; click a block to toggle its fold; click the **path**
in a file tool's header (`⏺ Write(…)`, `Update(…)`, `Read(…)`) to reveal that file in
your OS file manager; drag to select text (auto-copied to the clipboard, or an OSC 52
escape over SSH). A `Write` folds to a 10-line numbered preview and expands to the
whole file, and consecutive tool calls coalesce into one activity line — matching
Claude Code.

## `agent-jdi` — supervise unattended runs

The repo also ships a second binary, **`agent-jdi`**: it runs an AI agent
*unattended* (relaunching on recoverable exits) and follows it live with the
viewer. It's multi-agent and **auto-detects** the agent from the directory's
sessions (Claude or Codex), so one tool covers both.

```bash
agent-jdi start "refactor the parser and add tests"   # fresh unattended run (prints a summary; -f to watch live)
agent-jdi resume            # resume this dir's newest session, unattended (prints a summary; -f to watch live)
agent-jdi resume --id avatar-kit-5ce3fb   # resume an exact tracked slot from `agent-jdi list`
agent-jdi resume --agent codex   # force an agent
agent-jdi handoff "finish the refactor and commit"    # hand THIS interactive session to an unattended run
agent-jdi log               # reattach the viewer to the supervised session
agent-jdi status            # rich status: live progress, task checklist, recent commits, start/finish
agent-jdi backlog "also update the changelog"   # queue follow-up for the next drain
agent-jdi takeover          # stop the run and hand it back to you (launches the resumed agent)
agent-jdi list
```

`start` runs a **fresh** task (vs. `resume`, which continues an existing session).
The session id is pinned up front for Claude (`--session-id`) and **captured** for
Codex (which assigns its own id — recovered after the first turn via a nonce). With
no `--agent`, `start` reuses the agent of the **latest run in this directory** (its
last `agent-jdi` run, else the most recent session of any kind), defaulting to Claude
only when the directory has no history.

By default `start`/`resume` launch the worker **detached in the background** and
print a summary (session, retry policy, autonomy, follow-up commands), then return —
add **`-f`/`--follow`** to open the live viewer instead (equivalent to `agent-jdi log
<id>` afterward; `log` follows by default).

**Handing sessions across the human ↔ jdi boundary.** `takeover` and `handoff` are
mirrors: `takeover` stops an unattended run and **launches the agent interactively
resumed** on the session so you continue it yourself (`--no-launch` to just stop and
print the resume commands); `handoff`, run from *inside* an interactive session,
hands it the other way — it quits your session and resumes it unattended in the
background (`--armed` to arm without auto-quitting). A ready-made `/jdi-handoff`
slash command and a `jdi-handoff` skill (triggers on "hand this off to jdi" /
"justdoit") wrap `handoff` — see [`integrations/claude/`](integrations/claude/).

Any command that would affect a real agent (`start`/`resume`/`backlog`/`takeover`/
`handoff`) accepts **`--dry-run`** — it prints exactly what it would do (agent,
resolved binary, the full invocation, what it would kill/queue) and exits with
**no** spawn, kill, or state change. Use it to verify before committing to a real run.

Install: `brew install tanghong123/tap/agent-jdi` (depends on the viewer formula).
It uses its own state under `~/.local/state/agent-jdi/` (`$XDG_STATE_HOME`; override
the whole path with `AGENT_JDI_HOME`) — not under `~/.claude`, since it's agent-neutral. It
supersedes the bash `claude-jdi` from `claude-toolbox`. The two enforce **one
supervisor per directory**: each refuses to `start`/`resume` a directory the other
is already live in (stop the other first, or use it).

Architecture: an **agent-agnostic supervisor spine** (detached worker, slot lock,
`meta` state, backlog queue, retry loop) drives per-agent **`AgentAdapter`s**
(`src/jdi/{claude,codex}.rs`). Adding an agent is one module + one registry arm;
adapters may leave optional capabilities (e.g. a native task queue) unimplemented.
See [`src/jdi/DESIGN.md`](src/jdi/DESIGN.md).

> Codex's unattended `codex exec resume` and interactive `codex resume` command
> shapes were verified against Codex CLI 0.145.0. The remaining compatibility
> risk is the content of a real network turn's JSON event stream and whether a
> resumed turn writes a new rollout file; those details remain isolated in
> `codex.rs`.

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
