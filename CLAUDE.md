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
- `model.rs` JSONL → blocks (+ view filters) · `markdown.rs` md → ratatui lines
- `render.rs` blocks → styled lines · `wrap.rs` width-aware wrapping
- `view.rs` state machine + draw (TestBackend-testable) · `app.rs` terminal + input
- `tail.rs` byte-offset live tail · `discover.rs` find transcript · `theme.rs` styles
- `metrics.rs` footer tokens/cost · `highlight.rs` syntect · `codex_{model,discover,metrics}.rs` Codex
- `html_export.rs` `--dump-html` → one self-contained `.html` (fixed shell + `html/export.{css,js}`
  embedded; Rust emits an append-only JSON block stream, the JS renders it; `-f` writes a
  companion `<stem>.jsonl` the page polls). Reuses `model`/`render`/`markdown`/`highlight`.
- `jdi/` the **`agent-jdi`** binary (unattended-run supervisor); see `src/jdi/DESIGN.md`

The viewer's phased plan (P0–P8) is **built** — see `DESIGN.md` for the design
notes and the open backlog. Borrowed ideas are credited in `ATTRIBUTION.md`.
