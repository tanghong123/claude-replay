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
(runs BOTH crates via workspace default-members; deterministic — no terminal needed;
the tmux e2e is opt-in), and `scripts/gate/gate.sh` printing `BYTE-IDENTICAL: PASS`
(fixture data lives in `$SC_GATE_DIR`, default `/tmp/sc-gate`; intentional output
changes are verified line-by-line then re-baselined — see `scripts/gate/README.md`).

## Layout
A Cargo **workspace** with two crates. The viewer refers to core modules by their original
paths (`crate::model`, `crate::engine`, …) via re-exports in `src/lib.rs`, so the split is
mostly transparent when reading viewer code.

**`claude-replay-core/`** — the agent-agnostic parser/replay engine. **No** TUI/HTML/CLI deps
(only `serde_json` + `anyhow`); the crate boundary enforces "core is presentation-agnostic".
- **Shared engine** (agent-neutral): `model.rs` the `Block` data-model vocabulary + block
  classification (`block_kind`/`fold_key`) · `engine/` `replay` (the L2 `Replayer`/`Shaping`
  fold + `parse_stream` driver that *builds* blocks) · `message` (L1↔L2 log) ·
  `session`/`index` (`Session`/`SessionIndex`) · `store` (`SessionStore` tiers) · `path`/`time` ·
  `metrics.rs` the `Metrics` value + pricing · `discover.rs` the `Candidate` type +
  `detect_agent`/`session_cwd`/`session_id`/`subagent_source`/`resolve_any` ·
  `follow.rs` incremental `FollowParser` · `tail.rs` byte-offset tail · `agent.rs` the `Agent` enum ·
  `adapter.rs` the `TranscriptAdapter` trait + `adapter()`/`adapters()` registry (the one per-agent seam)
- **Per-agent L1 adapters** (symmetric, each feeds the shared engine): `claude_model.rs` /
  `codex_model.rs` (tokenizer + `Shaping`) · `claude_metrics.rs` / `codex_metrics.rs` (token/cost
  folding) · `claude_discover.rs` / `codex_discover.rs` (that agent's transcript store). A new
  agent = a `*_model`/`*_metrics`/`*_discover` trio + one `impl TranscriptAdapter` row in
  `adapter.rs`; the shared engine is
  never touched. Each adapter owns its test suite (the byte-identical equivalence gates live
  in `claude_model`/`codex_model`); `model`'s tests are the agent-neutral ones only
  (`block_kind`/`fold_key`, `relativize`).

**`claude-replay`** (root crate) — the ratatui viewer + HTML export + clap CLI + `agent-jdi`.
Shared modules sit at the top level (used by both frontends): `present.rs` the plain-text summary
formatters (spawn chips, activity/turn summaries, tool display names, edit summaries), `fold.rs`
the `FoldPolicy`, and `highlight.rs` the syntect highlighter (returns ratatui `Span`s; the HTML
exporter adapts them). The agent-neutral diff-row model
(`DiffKind`/`DiffRow`/`DiffGroup`/`diff_row_groups`/`line_diff` + `base64_decode`) lives in
`claude-replay-core::diff` (re-exported as `crate::diff`).
- `tui/` the terminal frontend: `view.rs` state machine + draw (TestBackend-testable) ·
  `app.rs` terminal + input · `render.rs` blocks → styled ratatui lines · `markdown.rs` md →
  ratatui lines · `wrap.rs` wrapping · `theme.rs` styles · `picker.rs` fuzzy session picker ·
  `clipboard.rs`. Only `app`/`view` are public. `render` calls `crate::diff` + `crate::present`
  + `crate::highlight`.
- `html_export/` (`mod.rs` render core · `bundle.rs` the `--dump-html`/`--dump-all-html` offline
  writers · `serve.rs` the `--html` live server) `--dump-html` (write files) / `--html` (open
  browser; `-f` serves live over a loopback HTTP server since a `file://` page can't `fetch`) →
  one self-contained `.html` (fixed shell + `html/export.{css,js}` embedded; Rust emits an
  append-only JSON block stream, the JS renders it; `-f` writes a companion `<stem>.jsonl` the
  page polls). Its shared deps are `model` + `fold` + `crate::diff` + `present` + `highlight`.
- `jdi/` the **`agent-jdi`** binary (unattended-run supervisor); see `src/jdi/DESIGN.md`

The viewer's phased plan (P0–P8) is **built** — see `DESIGN.md` for the design
notes and the open backlog. Borrowed ideas are credited in `ATTRIBUTION.md`.
