# CLAUDE.md — claude-replay

A Rust + ratatui terminal UI viewer. It is **fully testable headless (no TTY)** —
never skip, stub, or defer a feature "because it needs a terminal."

## Test the TUI without a TTY
- **Deterministic (preferred):** drive `view::View` under ratatui **`TestBackend`**
  — render to an in-memory buffer, call the view's methods, assert cells. See the
  `#[cfg(test)]` tests in `src/view.rs` for the pattern. All viewer state lives in
  `View`, separate from the terminal wiring in `app.rs`, precisely so it's testable
  this way. Add a `TestBackend` test for any new interactive behavior.
- **End-to-end:** `tests/tmux_smoke.rs` runs the real binary inside a private
  `tmux -L` server with no controlling TTY and drives it via `send-keys` /
  `capture-pane` (`tmux new-session -d` works without a TTY). `#[ignore]`d; run
  `cargo test --test tmux_smoke -- --ignored`.
- **Quick plain check:** `claude-replay <path|--latest> --dump -` renders to stdout
  (no TUI) — good for verifying parsing/markdown/diffs in a pipe. (`--dump <stem>` or
  bare `--dump` instead write `<stem>.txt` + `<stem>.ansi` at the terminal width or
  `--width N`; bare `--dump` deduces the stem.) `--dump` renders through the View
  pipeline and applies the TUI's default fold policy (add `--full` to expand all).

## Gate every change on
`cargo fmt --check`, `cargo clippy --all-targets` (no new warnings), `cargo test`
(the default suite is deterministic — no terminal needed; the tmux e2e is opt-in).

## Layout
A Cargo **workspace** with two crates. The viewer refers to core modules by their original
paths (`crate::model`, `crate::engine`, …) via re-exports in `src/lib.rs`, so the split is
mostly transparent when reading viewer code.

**`claude-replay-core/`** — the agent-agnostic parser/replay engine. **No** TUI/HTML/CLI deps
(only `serde_json` + `anyhow`); the crate boundary enforces "core is presentation-agnostic".
- **Shared engine** (agent-neutral): `model.rs` the `Block` data model + L2 `Replayer`/`replay`
  fold + `Shaping` seam + `parse_stream` + block classification + the `parse_*_for` dispatchers ·
  `engine/` `message` (L1↔L2 log) · `session`/`index` (`Session`/`SessionIndex`) · `store`
  (`SessionStore` tiers) · `path`/`time` · `metrics.rs` the `Metrics` value + pricing ·
  `discover.rs` the `Candidate` type + `detect_agent`/`session_cwd`/`resolve_any` ·
  `follow.rs` incremental `FollowParser` · `tail.rs` byte-offset tail · `agent.rs` the `Agent` enum
- **Per-agent L1 adapters** (symmetric, each feeds the shared engine): `claude_model.rs` /
  `codex_model.rs` (tokenizer + `Shaping`) · `claude_metrics.rs` / `codex_metrics.rs` (token/cost
  folding) · `claude_discover.rs` / `codex_discover.rs` (that agent's transcript store). A new
  agent = a `*_model`/`*_metrics`/`*_discover` trio + one dispatcher arm; the shared engine is
  never touched. (Claude-parsing tests currently live in `model.rs`'s test module, driving the
  shared engine through `claude_model`.)

**`claude-replay`** (root crate) — the ratatui viewer + HTML export + clap CLI + `agent-jdi`.
- `markdown.rs` md → ratatui lines · `render.rs` blocks → styled lines · `wrap.rs` wrapping
- `view.rs` state machine + draw (TestBackend-testable) · `app.rs` terminal + input
- `theme.rs` styles · `highlight.rs` syntect · `picker.rs` fuzzy session picker · `clipboard.rs`
- `html_export.rs` `--dump-html` (write files) / `--html` (open browser; `-f` serves live over a
  loopback HTTP server since a `file://` page can't `fetch`) → one self-contained `.html` (fixed
  shell + `html/export.{css,js}` embedded; Rust emits an append-only JSON block stream, the JS
  renders it; `-f` writes a companion `<stem>.jsonl` the page polls). Reuses `model`/`render`/`markdown`/`highlight`.
- `jdi/` the **`agent-jdi`** binary (unattended-run supervisor); see `src/jdi/DESIGN.md`

The viewer's phased plan (P0–P8) is **built** — see `DESIGN.md` for the design
notes and the open backlog. Borrowed ideas are credited in `ATTRIBUTION.md`.
