# claude-replay — Developer Guide

Practical guidance for working on `claude-replay`: how to build and test it, how to reuse the
engine as a library, and how to add support for a new agent. For the system design behind all
of this, see [Architecture](architecture.md).

---

## 1. Build & test

```sh
cargo build --workspace
cargo test  --workspace       # deterministic; no terminal needed
```

**Gate every change on** (CI enforces these):

```sh
cargo fmt --check
cargo clippy --all-targets    # no new warnings
cargo test                    # default suite is deterministic
```

## 2. Testing a TUI with no terminal

The viewer is **fully testable headless** — never skip or stub a feature "because it needs a
terminal". Three levels:

1. **Deterministic (preferred).** Drive `view::View` under ratatui's `TestBackend`: render to
   an in-memory buffer, call the view's methods, assert cells. All viewer state lives in
   `View` (separate from the terminal wiring in `app.rs`) precisely so this works. See the
   `#[cfg(test)]` tests in `src/view.rs`. **Add a `TestBackend` test for any new interactive
   behavior.**
2. **End-to-end.** `tests/tmux_smoke.rs` runs the real binary inside a private `tmux -L`
   server with no controlling TTY, driving it via `send-keys`/`capture-pane`. It's
   `#[ignore]`d; run `cargo test --test tmux_smoke -- --ignored`.
3. **Quick plain check.** `claude-replay <path|--latest> --dump -` renders the transcript to
   stdout with no TUI — good for eyeballing parsing/markdown/diffs in a pipe.

### The byte-identical gate (for engine refactors)

Any change that reorganizes the parser must be **output-preserving**. Two safety nets:

- The streaming parse is asserted bit-identical to frozen `parse_main`/`parse_lines` oracles
  (`#[cfg(test)]` in `claude_model`/`codex_model`).
- Before/after a refactor, diff `--dump`/`--dump-html` output on frozen Claude **and** Codex
  transcripts:

  ```sh
  claude-replay frozen.jsonl        --dump - --full --width 100 > before.txt
  # …refactor…
  claude-replay frozen.jsonl        --dump - --full --width 100 > after.txt
  diff before.txt after.txt         # must be empty
  claude-replay frozen.jsonl        --dump-html - > after.html   # and the HTML path
  ```

## 3. Using the engine as a library

`claude-replay-core` is a standalone crate (`serde_json` + `anyhow` only). The public surface
is small and curated — a consumer never reaches through module paths.

```rust
use claude_replay_core::{parse_session, FollowParser, Block};

// One-shot: parse a whole transcript into an agent-neutral Session.
let session = parse_session(std::path::Path::new("session.jsonl"))?;
println!("{} blocks · {} turns · {}", session.blocks.len(),
         session.index.turns.len(), session.metrics.footer());
for block in &session.blocks {
    match block {
        Block::AssistantText(t) => { /* render markdown */ }
        Block::ToolUse { name, target, .. } => { /* render a tool line */ }
        _ => {}
    }
}

// Live tail: fold only appended bytes each poll (call on a timer).
let mut follower = FollowParser::open(session.agent, path);
while let Some((blocks, _times, metrics)) = follower.poll()? {
    // re-render from `blocks`; `poll()` returns None when the file hasn't grown
}
```

The complete curated API: `parse_session` / `parse_session_as` / `parse_session_enriched[_as]`,
`Session`, `SessionIndex` (+ `TurnEntry`/`AgentEntry`/`ToolEntry`/`ToolCount`/`AttachmentEntry`),
`FollowParser`, `Metrics`, `Block` (+ `Attachment`/`AttachmentContent`/`SubAgent`/`AgentStatus`/
`Hunk`), and `Agent`. A runnable example lives at
[`claude-replay-core/examples/parse.rs`](../claude-replay-core/examples/parse.rs):

```sh
cargo run -p claude-replay-core --example parse -- <transcript.jsonl> [--follow]
```

## 4. Adding an agent

This is the payoff of the [three-layer design](architecture.md#3-the-three-layer-engine):
a new agent is **a `*_model` / `*_metrics` / `*_discover` trio + one `impl TranscriptAdapter`
row** — the shared engine is never touched. Say we're adding `Gemini`.

### Step 1 — register the agent

Add the variant + labels in `claude-replay-core/src/agent.rs`:

```rust
pub enum Agent { Claude, Codex, Gemini }
// …extend label() / from_label() with "gemini".
```

### Step 2 — Layer 1: the decoder (`gemini_model.rs`)

This is the only place that knows Gemini's raw line format. Provide:

- **`decode_line(line, cwd, out)`** — map one raw JSONL line to 0+ canonical
  [`Message`](../claude-replay-core/src/engine/message.rs)s (`UserText`, `AssistantText`,
  `Thinking`, `ToolUse`, `ToolResult`, …). This is where Gemini's field names get mapped onto
  the shared vocabulary. Thread `cwd` across lines if the format carries it in a header.
- **`scan_join_ids(lines)`** — the pass-1 pre-scan: collect the tool-call ids a later result
  will join onto. Reuse the shared skeleton: `engine::replay::scan_ids(lines, |v, ids| { …pull
  Gemini's call ids… })`.
- **`GEMINI_SHAPING: Shaping`** — the four L2 hooks: `build_tool` (raw tool fields → a `Block`,
  incl. any agent-specific tool-name normalization), `join_result` (attach a result onto its
  `ToolUse`), `keep_orphan` (keep a resultless output?), `finish_turns` (final grouping, or
  identity). Model it on `codex_model` (the simpler of the two).

> Everything else about parsing — the fold, back-patching, turn grouping, the queue lifecycle,
> streaming — is shared and already done. You are writing a *decoder*, not a parser.

### Step 3 — Layer: metrics (`gemini_metrics.rs`)

A small accumulator implementing the crate-internal `MetricsAccumulator` (push a line's
token usage, `finish()` into [`Metrics`]). Reuse `metrics::TimeSpan` for the duration and
`metrics::estimate_cost` for pricing. See `codex_metrics.rs`.

### Step 4 — discovery (`gemini_discover.rs`)

Where Gemini keeps its transcripts on disk. Provide `candidates_scoped(cwd)` (sessions for a
directory, scoped to the nearest ancestor that has any — reuse `discover::ancestors_of`) and
`resolve_id(id)` (id → path). If Gemini has sub-agents, also a `subagent_source`.

### Step 5 — wire it up (`adapter.rs`)

Implement `TranscriptAdapter` for a `GeminiAdapter` unit struct, delegating each hook to the
modules above:

```rust
pub(crate) struct GeminiAdapter;
impl TranscriptAdapter for GeminiAdapter {
    fn agent(&self) -> Agent { Agent::Gemini }
    fn sniff(&self, head: &Value) -> bool { /* recognize a Gemini head */ }
    fn scan_join_ids(&self, path: &Path) -> io::Result<HashSet<String>> {
        // open + stream lines to gemini_model::scan_join_ids
    }
    fn decode_line(&self, line: &str, cwd: &mut String, out: &mut Vec<Message>) {
        crate::gemini_model::decode_line(line, cwd, out)
    }
    fn shaping(&self) -> &'static Shaping { &crate::gemini_model::GEMINI_SHAPING }
    fn metrics_acc(&self) -> Box<dyn MetricsAccumulator> { Box::new(GeminiMetricsAcc::default()) }
    fn candidates_scoped(&self, cwd: &Path) -> Vec<Candidate> { crate::gemini_discover::candidates_scoped(cwd) }
    fn resolve_id(&self, id: &str) -> Option<PathBuf> { crate::gemini_discover::resolve_id(id) }
    // enrich / subagent_source / parse_path_timed / parse_reader: inherit the defaults
}
```

Then add the one registry row (both places):

```rust
pub(crate) fn adapter(agent: Agent) -> &'static dyn TranscriptAdapter {
    match agent { Agent::Claude => &ClaudeAdapter, Agent::Codex => &CodexAdapter,
                  Agent::Gemini => &GeminiAdapter }
}
pub(crate) fn adapters() -> &'static [&'static dyn TranscriptAdapter] {
    &[&ClaudeAdapter, &CodexAdapter, &GeminiAdapter]
}
```

Declare the new modules in `claude-replay-core/src/lib.rs` (`pub(crate) mod gemini_model;`
etc.).

### Step 6 — that's it (what you did NOT touch)

You wrote three small per-agent files + one adapter impl. You did **not** touch: the fold
(`engine::replay`), the `Block` model, `Session`/`SessionIndex`, discovery's facade
(`detect_agent`/`resolve_any` pick up the new `sniff`/registry automatically), the live
follower, or any presenter. `sniff` makes `detect_agent` recognize the format; the registry
row makes it reachable everywhere.

### Step 7 — test it

- Add an equivalence gate in `gemini_model` (a frozen fixture: assert
  `replay(tokenize(x))` matches a hand-checked expected block list), mirroring the
  `replay_tokenize_matches_*` tests.
- Add a `FollowParser` round-trip: the follower's incremental output must equal a full
  `parse_session_as` at each append (see `follow.rs`'s `assert_follow`).
- Run the full gate (`fmt` / `clippy` / `test`).

## 5. Repo conventions

- **Layout & module map:** [Architecture §10](architecture.md#10-where-things-live).
- **Design notes** for specific subsystems live under `design/` (e.g. the queued-message
  lifecycle, transcript-source formats, the deferred agent-agnostic-supervisor plan) and
  `src/jdi/DESIGN.md` for the supervisor.
- Keep the per-agent pairs **symmetric** (same shape + naming on the Claude and Codex sides);
  the reviews check for drift.
