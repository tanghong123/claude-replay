# agent-replay

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
brew install tanghong123/tap/agent-replay
```

This taps `tanghong123/tap` and puts the `agent-replay` command on your `PATH`
(with `claude-replay` kept as a symlink through the rename transition), so
you can run it from anywhere:

```bash
agent-replay --latest      # open your most recent session
agent-replay --version
```

On macOS and Linux (x86_64 and arm64) this downloads a prebuilt binary — no Rust
toolchain, no compile. Later, `brew upgrade agent-replay` updates it and
`brew uninstall agent-replay` removes it — existing installs under the old
`claude-replay` name upgrade transparently (the tap records the rename).
(Equivalent two-step: `brew tap tanghong123/tap` then `brew install agent-replay`.)

**Prebuilt binary** (no Homebrew, no Rust) — `cargo-binstall` grabs the release
tarball for your platform:

```bash
cargo binstall claude-replay      # crate name unchanged; installs the agent-replay binary
```

Or download an `agent-replay-<target>.tar.gz` from the
[releases page](https://github.com/tanghong123/claude-replay/releases) directly
(static musl builds for Linux; run on any distro).

**From source** (needs a Rust toolchain):

```bash
cargo install --path .          # → ~/.cargo/bin/agent-replay
# or
cargo build --release           # → target/release/agent-replay
```

## Usage

```
agent-replay                                 pick from this dir's sessions (Claude + Codex)
agent-replay <session-id | path/to.jsonl>   render that transcript (agent auto-detected)
agent-replay --latest                        newest session for THIS dir or an ancestor (not the global newest)
agent-replay --agent codex                   only show Codex sessions (or --agent claude)
agent-replay <id|--latest> --dump -          plain text to stdout (no TUI) — for pipes/tests
agent-replay <id|--latest> --dump [stem]     write <stem>.txt + <stem>.ansi (deduced stem if omitted)
agent-replay <id|--latest> --dump --width N  dump at width N (default: terminal width, else 100)
agent-replay <id|--latest> --dump --full     dump with everything expanded (default folds like the TUI)
agent-replay <id|--latest> --dump-html [stem] export a single self-contained <stem>.html (deduced stem if omitted)
agent-replay <id|--latest> --dump-html -      write the HTML page to stdout (no TUI) — for pipes/tests
agent-replay <id|--latest> --html             open in a browser (no TUI): serves over loopback, follows
                                               the session live, Ctrl-C to stop
agent-replay <id> --no-cache                  skip the durable cache (fold from scratch; also allows
                                               a second LIVE view of a session another instance follows)
```

**Live by default.** The viewer and `--html` always tail — new turns appear as the session
writes them; nothing to pass. Dumps (`--dump`, `--dump-html`, `--dump-all-html`) are always
snapshots. `-f`/`--follow` is accepted and ignored, so old commands keep working.

**The durable cache.** Opening a session writes its folded blocks under
`~/.cache/claude-replay/sessions/`, so opening it again resumes where the last run stopped
instead of re-reading the transcript — on a 100 MB session that is most of the file skipped.
It is validated against the transcript (a rewritten or truncated source rebuilds from
scratch), one writer at a time, and swept after two weeks idle. `--no-cache` opts out.

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

**`--dump-html`** exports a **single self-contained `.html`** — no network, no
external assets — that reproduces the TUI's structure in the browser: folding,
search (`/`), `j/k`/`[ ]` keys, a turn sidebar with scroll-spy, a usage/cost panel,
light/dark themes (persisted), and click-to-copy on the session id and code fences.
It honors the same `--fold`/`--unfold`/`--full` flags, but never caps a body — the
full content is always in the file (grep-able), with long tails hidden behind a
`⋯ N more lines` expander. The page is a fixed shell plus an append-only JSONL block
stream inlined in the file — a self-contained snapshot. To watch a running session,
use `--html`, which serves the same content and follows it live.
Clicking a **file path** in a tool header (`Write`/`Update`/`Read`) opens that file
(`file://`, new tab) — the browser's stand-in for the TUI's reveal-in-Finder;
clicking elsewhere on the header still folds.

**`--html`** is the GUI counterpart to the TUI: it renders the page and **opens it
in your browser** instead of drawing the terminal UI. It serves the page over a
tiny **loopback HTTP server** and prints the `http://127.0.0.1:…` URL (copy it to
open elsewhere); Ctrl-C stops the server. Serving (rather than a bare `file://`
page) is what lets a click on a tool-header path **reveal the file in Finder** —
the browser can't shell out, so the click asks the local server, which does the
same `open -R` the TUI does. The page **follows the session live** — new turns
stream in and the cost updates as they land.

When you give **no** session and this directory has **several**, `--html` keeps the
session picker on screen and serves *every* discovered session at once on the one
port. `Enter` — or a click on a row — opens that session in a browser tab and the
list **stays up**, marking what you've already opened (`●`), so you can fan several
sessions out and keep going; `Esc` quits. They are all live simultaneously — each
open tab tails its own session. A directory with exactly one match still opens it
directly.

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
agent-jdi resume --id <slot>     # resume an exact tracked slot from `agent-jdi list`
agent-jdi resume --session <id>  # resume an exact session (skips discovery + the stale-session prompt)
agent-jdi resume --agent codex   # force an agent
agent-jdi handoff "finish the refactor and commit"    # hand THIS interactive session to an unattended run
agent-jdi log               # reattach the viewer to the supervised session
agent-jdi status            # rich status: live progress, task checklist, recent commits, start/finish
agent-jdi backlog "also update the changelog"   # queue follow-up; a live run drains it when its work finishes
agent-jdi backlog --drain   # drain the queue now (relaunches a stopped session)
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
add **`-f`/`--follow`** to open the live viewer instead (equivalent to running
`agent-jdi log <id>` afterward).

**Handing sessions across the human ↔ jdi boundary.** `takeover` and `handoff` are
mirrors: `takeover` stops an unattended run and **launches the agent interactively
resumed** on the session so you continue it yourself (`--no-launch` to just stop and
print the resume commands); `handoff`, run from *inside* an interactive session,
hands it the other way — it quits your session and resumes it unattended in the
background (`--armed` to arm without auto-quitting). The shared `jdi-handoff`
Skill wraps this flow for both clients: use `$jdi-handoff` in Codex or the native
`/jdi-handoff` command in Claude Code. Install and usage details are in
[`integrations/`](integrations/).

`takeover` resumes with the run's **unattended posture** (Claude's
`--dangerously-skip-permissions`) so it doesn't start prompting on every action;
`--supervised` resumes with approvals on instead. With **no agent-jdi run tracked**
for the directory it takes over the newest *unmanaged* claude/codex session there:
if another agent is already live on that session it refuses and prints the resume
command, unless `--force`, which kills that agent first.

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

Codex integration is validated against CLI 0.145.0: authentication uses
`codex login status`, unattended turns use `codex exec` / `codex exec resume`,
interactive takeover uses `codex resume`, and fresh-run identity is read from the
JSON `thread.started.thread_id` event (with rollout-marker discovery as a fallback).

## `agent-monitor-fleet` — several machines' monitors on one page

The workspace also builds **`agent-monitor`**: one loopback page showing every agent
session on *this* machine. It is single-machine on purpose and stays that way —
**`agent-monitor-fleet`** is a separate binary that opens one SSH tunnel per machine
and serves a switcher whose tabs are those monitors' own pages, unmodified, in iframes.

```bash
cargo install --path claude-monitor   # → ~/.cargo/bin/agent-monitor
agent-monitor                         # opens the app-shell monitor at 127.0.0.1:2727
```

Two interfaces ship, and both are supported. The **app shell** is the default: it consumes the
structured session index and the `/records` pipeline directly, in one document. The **classic**
rail-and-iframe page is still there. Each carries a button that switches to the other and
remembers the choice; `?ui=classic` or `?ui=app` on the URL overrides for a single request
without changing what is remembered, so the two can be compared side by side. The classic page
stays until the app shell has been validated in real use.

```bash
cargo install --path claude-monitor-fleet   # → ~/.cargo/bin/agent-monitor-fleet
agent-monitor-fleet discover --add         # find the monitors you have, and keep them
agent-monitor-fleet up                     # tunnels + the page, opened in your browser
```

That is the whole first run. `discover` without `--add` probes and writes nothing, and
`up --discover` uses what it finds once without saving — either is a way to look before
anything lands in the config.

```bash
agent-monitor-fleet up [--port N] [--no-open] [--discover]
agent-monitor-fleet discover [--add] [--host DEST]... [--ssh-config PATH]
agent-monitor-fleet status     # probe the configured environments; open nothing
agent-monitor-fleet list       # what is in the config
agent-monitor-fleet add NAME [--ssh DEST] [--ssh-option ARG]... [--cache-root PATH] [--port N]
agent-monitor-fleet remove NAME
```

**Nothing about your machines is assumed.** The config — `$CLAUDE_MONITOR_FLEET_CONFIG`,
else `fleet.json` under `$XDG_CONFIG_HOME`/`~/.config` in `claude-monitor/` when that
directory already exists (pre-rename installs) or `agent-monitor/` otherwise — is a
JSON file you can edit, and it **ships
empty**: every host in it is one you or discovery put there. Each monitor's port is
**read from that monitor's own lock** (`<cache root>/LOCK`), so one on a non-default
`--port`, or a second one under its own `$CLAUDE_MONITOR_CACHE`, is found as it is; local
tunnel ports are allocated by the kernel, so nothing collides with what you already run.
`add NAME` with no `--ssh` means this machine; `--cache-root` says *which* monitor on a
machine that runs two, and `--port` pins one whose lock can't be read.

**Discovery** probes this machine and every literal `Host` in *your* SSH config, in
parallel, with `ssh BatchMode` — a host that would ask for a passphrase is skipped rather
than left hanging (load the key into an agent, or `add` that host, where prompts work).
Two `Host` aliases for one machine collapse into one environment, and a host with no
monitor is reported as exactly that instead of being filled in.

**`up`** brings environments up one at a time, since a host-key or passphrase prompt needs
the terminal to itself, and one that doesn't answer is **skipped with the reason** rather
than costing you the others. It prints the URL on stdout — `--no-open` leaves the browser
alone, `--port N` pins the page's own port instead of letting the kernel choose — and holds
the tunnels for as long as it runs, taking them down when it stops, on `Ctrl-C` or `kill`
alike. **A tunnel that drops is re-opened**: a laptop lid, a changed network or a timed-out
connection costs that one tab a few seconds instead of costing it the rest of the run — with
backoff for a host that stays away, and on the same local port whenever it is still free, so
the tab follows its machine by itself. A *monitor* that is down is still only reported; the
fleet starts nothing on your machines. On the page, `1`–`9` pick a tab, `[` / `]` cycle, `r`
reloads the visible one, the URL fragment names the current machine so a bookmark points at
it, and the dot beside each name is health this process polls (the tabs are cross-origin, so
the browser can't ask them anything — this process can).

One prompt is enough if you'd rather ask an agent: install the Skill with
`./integrations/install-skill.sh monitor-fleet`, then `/monitor-fleet` in Claude Code or
`$monitor-fleet` in Codex — see [`integrations/`](integrations/). Why this is a companion
binary instead of a flag on the monitor is in
[`design/monitor-fleet.md`](design/monitor-fleet.md).

## Develop

It is **fully testable headless (no TTY)** — see [`CLAUDE.md`](CLAUDE.md).

```bash
cargo fmt --check
cargo clippy --all-targets
cargo test                                  # deterministic; no terminal needed
cargo test --test tmux_smoke -- --ignored   # opt-in end-to-end via private tmux
```

### Developer docs

> **Hosted:** [architecture](https://tanghong123.github.io/claude-replay/architecture.html) ·
> [developer guide](https://tanghong123.github.io/claude-replay/developer-guide.html) ·
> [API reference](https://tanghong123.github.io/claude-replay/) — rebuilt on every push.

- **[docs/architecture.md](docs/architecture.md)** — the system design: the three-layer
  engine (decode → fold → present), the two-crate boundary, the data model, and the
  per-agent `TranscriptAdapter` seam. (Also as a standalone graphics-rich page:
  [docs/architecture.html](docs/architecture.html) — open locally / host it.)
- **[docs/developer-guide.md](docs/developer-guide.md)** — build & test (incl. headless TUI
  testing and the byte-identical gate), using the engine as a library, and a step-by-step
  **[add-an-agent walkthrough](docs/developer-guide.md#4-adding-an-agent)**.
- **[docs/agent-monitor-deck.html](docs/agent-monitor-deck.html)** — an overview deck of
  `agent-monitor` and the reusable modules it offers (14 slides; ←/→ to navigate); also in
  Chinese: [docs/agent-monitor-deck.zh.html](docs/agent-monitor-deck.zh.html). Both are
  generated from one template, so they never drift apart.
- **[docs/adapter-rendering-validation.md](docs/adapter-rendering-validation.md)** — the reusable
  synthetic-transcript method for mapping a new agent's native events onto the shared Claude
  vocabulary without adding agent-specific rendering branches.
- **API reference** — auto-generated from the source, so it always matches the code; documents
  every object incl. internal ones (the `TranscriptAdapter` seam, the `Replayer` fold, …), not
  just the public API. Read it locally with `cargo apidoc --open` (alias for `cargo doc
  --workspace --no-deps --document-private-items`), or browse the hosted copy — the
  [`API docs` workflow](.github/workflows/docs.yml) rebuilds and publishes it to GitHub Pages
  on every push to `main`.

### Source layout

A Cargo **workspace** with two crates, split so the parsing engine is reusable and its
agent-neutrality is compiler-enforced:

- **`claude-replay-core/`** — the agent-agnostic engine. Its only dependencies are
  `serde_json` + `anyhow` (no TUI/HTML/CLI), so nothing here can reach into presentation.
  - **Shared engine:** `model.rs` (the `Block` data-model vocabulary + block classification —
    `block_kind`/`fold_key`) · `engine/` (`replay` = the L2 `Replayer`/`Shaping` fold +
    `parse_stream` driver that *builds* blocks · `message` · `session` · `index` · `store` ·
    `path` · `time`) · `metrics.rs` (the `Metrics` value + pricing) · `discover.rs` (the
    `Candidate` type, `detect_agent`, `session_cwd`, `session_id`, `subagent_source`,
    `resolve_any`) · `follow.rs` · `tail.rs` · `agent.rs` · `adapter.rs` (the `TranscriptAdapter`
    trait + registry — the single per-agent seam).
  - **Per-agent adapters** (symmetric — each is Layer 1 only, feeding the shared engine):
    `claude_model.rs` / `codex_model.rs` (tokenizer + `Shaping`), `claude_metrics.rs` /
    `codex_metrics.rs` (token/cost folding), `claude_discover.rs` / `codex_discover.rs`
    (that agent's transcript store). Adding an agent is a new `*_model`/`*_metrics`/
    `*_discover` trio plus one `impl TranscriptAdapter` row in `adapter.rs` — the shared
    engine stays untouched.
- **`claude-replay`** (root crate) — the ratatui viewer + HTML export + clap CLI, plus the
  `agent-jdi` binary. `markdown`/`render`/`wrap`/`view`/`app`/`theme`/`highlight`/`picker` ·
  `html_export/` (`mod`=render core, `bundle`=offline writers, `serve`=live server) ·
  `jdi/` (see [`src/jdi/DESIGN.md`](src/jdi/DESIGN.md)).

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
