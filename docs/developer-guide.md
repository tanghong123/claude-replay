# claude-replay — Developer Guide

Practical guidance for working on — and building on — the `claude-replay` workspace: how to
build and test it, which crate to depend on for which job, how to add an agent, and how to
build a new presentation on the shared layers. For the system design behind all of this, see
[Architecture](architecture.md).

---

## 1. Build & test

```sh
cargo build --workspace
cargo test  --workspace       # deterministic; every crate; no terminal needed
```

**Gate every change on** (CI enforces these):

```sh
cargo fmt --check
cargo clippy --all-targets    # no new warnings
cargo test                    # workspace default-members = all five crates
scripts/gate/gate.sh          # byte-identical frozen-fixture diff → must print PASS
```

The byte gate renders a frozen transcript set through `--dump`, `--dump-html`, and the
bundle writer and diffs against a baseline (fixture data in `$SC_GATE_DIR`, default
`/tmp/sc-gate`). Engine refactors must be output-preserving; intentional output changes are
verified line-by-line and re-baselined — see [`scripts/gate/README.md`](../scripts/gate/README.md).
Underneath, the streaming parse is additionally pinned to frozen `parse_main`/`parse_lines`
oracles (`#[cfg(test)]` equivalence gates in `claude_model`/`codex_model`).

## 2. Testing a TUI with no terminal

The viewer is **fully testable headless** — never skip or stub a feature "because it needs a
terminal". Three levels:

1. **Deterministic (preferred).** Drive `view::View` under ratatui's `TestBackend`: render to
   an in-memory buffer, call the view's methods, assert cells. All viewer state lives in
   `View` (separate from the terminal wiring in `app.rs`) precisely so this works. See the
   `#[cfg(test)]` tests in `claude-replay-tui/src/view.rs`. **Add a `TestBackend` test for
   any new interactive behavior.**
2. **End-to-end.** `tests/tmux_smoke.rs` runs the real binary inside a private `tmux -L`
   server with no controlling TTY, driving it via `send-keys`/`capture-pane`. It's
   `#[ignore]`d; run `cargo test --test tmux_smoke -- --ignored`.
3. **Quick plain check.** `claude-replay <path|--latest> --dump -` renders the transcript to
   stdout with no TUI — good for eyeballing parsing/markdown/diffs in a pipe.

## 3. The crates and their responsibilities

Pick your dependency by the job (details: [Architecture §2](architecture.md#2-the-workspace-five-crates-three-levels-of-reuse)):

| you want to… | depend on | you get |
|---|---|---|
| analyze transcripts (stats, ETL, CI checks) | `claude-replay-core` | parse/follow/discover, `Block`/`Session`/`Metrics` — deps: `serde_json` + `anyhow` |
| build your own frontend | + `claude-replay-present` | `SessionCache`, the pull protocol (both halves), fold policy, summaries, highlighting, `Args` |
| embed the existing UIs | + `claude-replay-tui` / `claude-replay-html` | the terminal viewer / the HTML exporter + live server |

The sections below are one worked example of each level — and every example is how **our own
binaries actually consume the layers**, so they can't rot into pseudo-code.

## 4. Level 1 — core alone: transcript analysis

`claude-replay-core` is sans-io and presentation-free. The one-shot surface:

```rust
use claude_replay_core::{parse_session, Block};

let session = parse_session(std::path::Path::new("session.jsonl"))?;  // agent auto-detected
println!("{} blocks · {} turns · {}", session.blocks().len(),
         session.index.turns.len(), session.metrics.footer());
for block in session.blocks().iter() {
    if let Block::ToolUse { name, target, .. } = block { /* tally tool usage… */ }
}
```

For anything incremental — or any I/O the library shouldn't dictate — drop one level to the
**sans-io [`SessionAccumulator`]** and push lines yourself:

```rust
use claude_replay_core::{engine::SessionAccumulator, Agent};

let mut acc = SessionAccumulator::new(Agent::Claude);
let mut offset = 0u64;
for line in my_source_of_lines() {            // a file, a socket, a decompressor, a test
    acc.advance_at(offset, &line);            // one line resident at a time
    offset += line.len() as u64 + 1;
}
println!("committed {} blocks, {} in the open turn",
         acc.committed_len(), acc.provisional_len());
let session = acc.snapshot();
```

This is not a side door — it is the *only* fold in the workspace. Batch parsing feeds it from
a file reader; the live [`FollowParser`] feeds it appended bytes each `poll()`
(`FollowParser::open(agent, path)` bundles the byte-offset tail + the accumulator, and
`poll_delta()` additionally reports `changed_from`, the first index that differs — O(turn),
not O(session)). Two performance levers worth knowing:

- **Storage injection:** `SessionAccumulator::with_store(agent, TierBStore::file(path)?)`
  spills committed block *content* to disk as it folds — RAM stays O(open turn) + a
  12-byte-per-block locator table. `snapshot()` then yields a `Session<Deferred>`; read
  blocks back through the `BlockAccess` trait. (This is exactly how the live HTML server
  keeps million-line sessions cheap.)
- **Delta reads:** `acc.stream_read(from)` returns `committed[from..]` + the open turn +
  O(turn) metadata — the primitive under the pull protocol; never clones the whole session.

A runnable example lives at
[`claude-replay-core/examples/parse.rs`](../claude-replay-core/examples/parse.rs):

```sh
cargo run -p claude-replay-core --example parse -- <transcript.jsonl> [--follow]
```

### The API reference (auto-generated, always in sync)

For the **exhaustive per-object reference** across all five crates, use rustdoc — generated
from source + doc comments, so it never drifts:

```sh
cargo apidoc          # build the reference into target/doc/  (alias in .cargo/config.toml)
cargo apidoc --open
```

It documents internal items too (`TranscriptAdapter`, the `Replayer`, the tokenizers) — the
reference manual behind this guided overview.

## 5. Level 2 — build your own frontend on core + present

`claude-replay-present` is the layer that makes a *new* presentation cheap — say a native
macOS app (SwiftUI shell over a Rust core), an egui viewer, or a bot that posts session
summaries. What it hands you, and where our own frontends use exactly the same thing:

| entity | what it does for you | real consumer in this repo |
|---|---|---|
| [`SessionCache`] | keyed residency: register sessions cheaply, `poll`/`poll_delta` materializes a follower on demand, 30 s TTL reaps idle residents | the TUI event loop (`claude-replay-tui/src/app.rs`) and the HTML server (`serve.rs`) share it — one resident set, one policy |
| `SharedSession` | a pull-servable live session: server-side patched committed/provisional zones + epoch/gen, hibernate/restore across evictions | `serve.rs` builds one per followed session, tier-b backed |
| `Cursor`/`pull`/[`PullClient`] | the incremental wire protocol, both halves in Rust | the `/pull` route serves it; the embedded JS mirrors `PullClient` transition-for-transition |
| `fold` (core) + `Args` | which block types start collapsed; the shared options type (clap only behind the `cli` feature) | both frontends call `args.fold_policy()` |
| `present` + core's `summary` | spawn chips, edit summaries, tool display names, activity/turn phrasing — the *voice* of the product | TUI `render.rs` and the HTML emitter, so wording can't drift |
| `highlight` | syntect highlighting returning spans (ratatui types, no terminal backend) | TUI styles them directly; the HTML exporter adapts them to `<span>`s |
| `sys` | `deduce_stem`, `reveal_in_file_manager` | the dump stem + the ⏎-on-a-path affordance in both UIs |

**The in-process shape** (what the TUI does — `app.rs`, simplified): borrow your UI's idle
tick, never spawn a follower thread.

```rust
let cache = SessionCache::new();
cache.register(&id, Transcript::open(agent, path));
loop {
    if no_input_for_250ms() {
        if let Some(Ok((blocks, _t, metrics, changed_from))) = cache.poll_delta(&id) {
            view.apply_from(blocks, changed_from);   // re-render only the tail
            view.set_footer(metrics);
        }
    }
    cache.reap(30_000);   // idle residents fall back to cheap registrations
}
```

**The decoupled shape** (a worker thread, a subprocess, or a network hop away): serve
`PullReply`s from a `SharedSession` on one side, hold a [`PullClient`] on the other. The
client owns the cursor; the server stays per-client stateless — N windows work for free.

```rust
// server side (any transport):            // client side:
let ss = SharedSession::open(agent, path); let mut pc = PullClient::new();
ss.advance()?;                             let applied = pc.apply(&reply);
let reply = ss.pull(request_cursor);       if let Some(d) = applied.first_changed {
/* send `reply` */                             rerender_from(pc.blocks(), d);
                                           } // next request carries pc.cursor()
```

For the macOS-app sketch specifically: the Rust side is those ~10 lines behind a small FFI
(or a local socket); SwiftUI renders `pc.blocks()` and re-draws from `first_changed`. Fold
defaults come from `FoldPolicy`, block styling classes from `model::block_kind`, summaries
from `core::summary` — your app speaks the same visual language as the TUI/HTML for free.
Keep the rendered-window discipline (only materialize views near the viewport;
[`design/dom-virtualization.md`](../design/dom-virtualization.md) is the transferable
technique doc).

## 6. Level 3 — embed the finished presenters

Both frontends are libraries with small public surfaces:

```rust
// The terminal viewer (claude-replay-tui):
claude_replay_tui::app::run(&args, &path)?;          // one session, full TUI
claude_replay_tui::app::run_interactive(&args)?;     // picker ↔ viewer flow
// …or drive `view::View` directly under TestBackend/your own terminal loop.

// The HTML frontend (claude-replay-html):
claude_replay_html::dump_html(&args, &path)?;        // one self-contained .html
claude_replay_html::dump_all_html(&args, &path)?;    // offline bundle, full agent tree
claude_replay_html::serve(&args, &path)?;            // loopback live server (pull protocol)
```

`Args` is plain data (`Default` + struct literal) unless you enable the `cli` feature for
clap parsing — a headless service can call `dump_all_html` with a hand-built `Args` and never
link clap. The root `claude-replay` crate is itself just this: ~60 lines of CLI dispatch over
the two frontends (`src/lib.rs::run_viewer`).

## 7. Adding an agent

The payoff of the [three-layer design](architecture.md#3-the-three-layer-engine-core): a new
agent is **a `*_model` / `*_metrics` / `*_discover` trio + one `impl TranscriptAdapter` row**
— the shared engine is never touched. Calibrate the cost by the three existing adapters:
Claude (full), Codex (no sub-agent tree), QoderWork (format matches Claude's, so it reuses
Claude's decoder wholesale — its whole adapter is discovery + a `sniff`). Say we're adding
`Gemini`.

### Step 1 — register the agent

Add the variant + labels in `claude-replay-core/src/agent.rs`:

```rust
pub enum Agent { Claude, Codex, QoderWork, Gemini }
// …extend label() / from_label() with "gemini".
```

### Step 2 — Layer 1: the decoder (`gemini_model.rs`)

The only place that knows Gemini's raw line format. Provide:

- **`decode_line(line, cwd, out)`** — map one raw JSONL line to 0+ canonical
  [`Message`](../claude-replay-core/src/engine/message.rs)s. Thread `cwd` across lines if the
  format carries it in a header.
- **`scan_join_ids(lines)`** — the pass-1 pre-scan: collect the tool-call ids a later result
  joins onto (reuse `engine::replay::scan_ids`).
- **`GEMINI_SHAPING: Shaping`** — the four L2 hooks: `build_tool` (raw tool fields → a
  `Block`, including tool-name normalization onto the canonical vocabulary — this is what
  makes the shared summaries/folding classify your agent correctly), `join_result`,
  `keep_orphan`, `finish_turns` (`coalesce_spans` for Claude-style grouping, or identity).
  Model it on `codex_model` (the simpler one).

> Everything else about parsing — the fold, back-patching, turn grouping, the queue
> lifecycle, streaming, the live follower — is shared and already done. You are writing a
> *decoder*, not a parser.

### Step 3 — metrics (`gemini_metrics.rs`)

A small `MetricsAccumulator` impl (push a line's token usage, `finish()` into [`Metrics`]).
See `codex_metrics.rs`.

### Step 4 — discovery (`gemini_discover.rs`)

Where Gemini keeps its transcripts. Provide `candidates_scoped(cwd)` — scope with
`discover::ancestors_below(cwd, home)`: auto-discovery must stay strictly inside `$HOME`
(see [Architecture §9](architecture.md#9-discovery)) — and `resolve_id(id)`. If Gemini has
sub-agents, also `subagent_source`; if it records a task list, `load_tasks`.

### Step 5 — wire it up (`adapter.rs`)

Implement `TranscriptAdapter` for a `GeminiAdapter` unit struct delegating to the modules
above. Two hooks deserve thought:

```rust
fn sniff(&self, head: &Value) -> SniffClaim {
    // Owns     — an unmistakable format marker of YOURS (Codex's rollout head)
    // CanParse — the format is readable but not proof of origin (Claude's generic shape)
    // No       — not mine
}
fn store_contains(&self, path: &Path) -> bool {
    // provenance: paths inside ~/.gemini/… are yours even if the format is generic —
    // this + sniff drives ownership vs the picker's "compatible (gemini)" badge
}
```

Then add the one registry row in `adapter()`/`adapters()` and declare the modules in
`lib.rs`. Discovery, detection, the picker, the follower, and both frontends pick the agent
up with no further changes.

### Step 6 — test it

- An equivalence gate in `gemini_model` (frozen fixture → expected block list), mirroring
  `replay_tokenize_matches_*`.
- A `FollowParser` round-trip: incremental output == full re-parse at each append.
- The full gate (§1), including `scripts/gate/gate.sh`.

## 8. Repo conventions

- **Layout & module map:** [Architecture §11](architecture.md#11-where-things-live).
- **Design notes** for specific subsystems live under `design/` — e.g. the CC span rules
  (`cc-activity-coalescing.md`), the fold/coalesce/summarize extensibility study, the DOM
  virtualization technique — and `src/jdi/DESIGN.md` for the supervisor.
- Keep the per-agent modules **symmetric** (same shape + naming across agents); reviews
  check for drift.
- One workspace version: bump `[workspace.package] version` in the root `Cargo.toml` only.

[`SessionAccumulator`]: ../claude-replay-core/src/engine/builder.rs
[`FollowParser`]: ../claude-replay-core/src/follow.rs
[`Metrics`]: ../claude-replay-core/src/metrics.rs
[`SessionCache`]: ../claude-replay-present/src/cache/mod.rs
[`PullClient`]: ../claude-replay-present/src/cache/stream.rs
