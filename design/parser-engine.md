# Design: the three-layer session engine (parse · replay · present)

Status: **built** — M1–M16 merged; production runs entirely on this engine, split into the
`claude-replay-core` crate (per-agent L1 adapters + shared L2 fold + presenters). This file
is the **design of record**; see `engine-refactor-plan.md` for per-milestone status. Grew out
of the DESIGN.md backlog item "Unify the parse backend + make it a reusable engine", but the
**scope is now the whole engine, not just the parser**: the three-layer architecture (§0) — an agent-specific
**raw parser** (Layer 1), an agent-agnostic **replay / state builder** (Layer 2), and thin
per-surface **presenters** (Layer 3) — made **incremental and live** (byte-offset resume,
mutation-safe append-only log, forward-fold replay, a tiered session store), consumed by
**every** surface (TUI, `--dump`, `--dump-html`, `--dump-all-html`, served/live `--html`)
and exposable as a standalone library.

It absorbs and supersedes several things already shipped or half-built: the sub-agent
model, the HTML multi-file bundle + live server + `/stream` byte cursor + lazy generation,
the id→path registry (the "level-3 cache" — tier (c) here, §8), and the live-tail CPU work
(§8.3). Those become *instances* of this
architecture rather than parallel implementations.

**The prime directive: zero user-facing change.** This is an internal re-architecture. At
every step the CLI, the on-disk stream format (`<id>.jsonl`), the TUI rendering, and the
`--dump*`/HTML outputs stay **byte-identical** (the sole intentional exception is the TUI
Edit diff-numbering bug, fixed as a side effect of unifying the one diff numberer). See §5
for the step-by-step migration and its preservation guarantees.

Guiding constraint: **preserve the streaming parse** (one JSONL line resident at a
time; the 298 MB session must stay ~811 MB, not balloon to ~2 GB — see
`STREAMING-PARSE-DESIGN.md`). This refactor must not regress peak RSS, and the
incremental paths (§8) must strictly *reduce* live CPU, never increase it.

---

## 0. The three layers (the north star)

Everything below reduces to a strict three-layer pipeline. Each layer has one clean
contract, is independently testable, and is incremental. **The layer boundaries are the
reuse boundaries: a new *agent* touches only Layer 1; a new *surface* touches only Layer
3; all the hard, shared machinery lives once in Layers 1–2.**

1. **Raw parser — transcript bytes → a canonical *message log*.** The *agent-specific*
   layer: Claude vs Codex line shapes live here (the `Transcript` adapter, §2.2). Cheap,
   near-stateless tokenization — **no back-patch, no grouping, no joins**. Incremental by
   byte offset and mutation-safe (kept-tail compare → `reset`, §8.3.2). Output: a
   **strictly append-only** message log (appends + `reset` markers).

2. **Replay / state builder — message log → in-memory `Session`.** *Agent-agnostic*,
   operating only on canonical messages. A **strictly-forward fold**: appends blocks,
   back-patches via an `id → block ref` index (O(1), never rewinds/re-scans), groups +
   coalesces the current turn, and builds the §7 indices + metrics. Incremental; the only
   rewind is a Layer-1 `reset`. Output: the `Session` — the single source of truth.

3. **Presentation — `Session` state → what the user sees.** *Agent-agnostic,
   surface-specific*: one thin formatter per surface over the same `Session` — `render.rs`
   (ratatui lines, TUI), `--dump` (text/ansi), `html_export` (the block-record stream).
   Incremental too (append blocks + `reset`; the client DOM patches).

The layers also have distinct **memory profiles** (§4.4): L1 is **O(1)** (streams messages,
holds none); L2 is **O(index)** — a resident `Session` is an *index* (id/kind/metadata +
byte offset per block) + the current turn's content, with block content loaded on-demand
from tier (b), **not** all blocks in RAM; L3 is **O(working set)** — only the viewport +
margin is materialized. So holding a session resident costs ~its index, not ~its blocks.

### How each surface maps onto the layers

| Surface | L1 parse | L2 replay | L3 present |
|---|---|---|---|
| `--dump` / `--dump-html` / `--dump-all-html` | backend, one-shot | backend | backend → text·ansi, or block-record file(s) |
| TUI (`-f`) | backend, incremental | backend, incremental | backend → ratatui redraw |
| live HTML (`-f --html`) | backend, incremental | backend, incremental | **backend** → append-only `<id>.jsonl` block stream · **client** → DOM |

For live HTML the client/server split lands **inside Layer 3**: the backend does
L1 + L2 + L3a (`Session` → the block-record stream), the client does L3b (block records →
DOM). Replay (L2) stays **server-side** — one Rust fold shared with the TUI *and* the dump
family; going client-side would fork L2 into JS and diverge from the TUI (§8.3.2 "replay
location"). The server holds only the **unstable-tail** replay state in RAM; stable
history lives on disk (`<id>.jsonl`, tier b) + the client DOM, so it is not a full
duplicate — the §8.3.2 incremental fold is what keeps L2's footprint bounded to the
current turn.

§1–§7 below detail Layers 1–3 (adapters, engine core, block model, indices); §8 details the
incremental + residency machinery that makes Layers 1–2 cheap and live.

### 0.1 Terminology (one name per concept — used verbatim below)

- **Layer 1 (L1) / the parser** — raw transcript bytes → the *message log*. Agent-specific.
- **Layer 2 (L2) / the replayer** — message log → a `Session`, by a strictly-forward
  **fold** (also called *replay*). Agent-agnostic.
- **Layer 3 (L3) / a presenter** — a `Session` → what a surface shows. One per surface.
- **the engine** — the L1 + L2 machinery (the `engine/` module) that produces a `Session`;
  everything below the presenters.
- **message · message log** — L1's output vocabulary: canonical events (user text, tool
  call, tool result, thinking, sub-agent spawn, completion, …) — **not** built blocks. The
  log is **append-only**: appends plus `reset` markers only.
- **`reset(N)`** — a message-log marker meaning "discard everything the replayer built from
  message N onward" (a rewritten tail). The only thing that rewinds L2.
- **block** — a presentation unit L2 builds (the `Block` enum: `UserText`, `ToolUse`,
  `SubAgent`, …). **block record** — a block serialized into a stream (the HTML
  `<id>.jsonl`); the HTML presenter's output.
- **`Session`** — L2's output and the single source of truth: `blocks` + `metrics` +
  `SessionIndex` + cwd/agent (§3).
- **`SessionIndex`** — the derived within-session indices (agents / tools / attachments)
  L2 builds; also the sub-agent liveness truth (§7).
- **`Transcript` adapter** — the agent-specific half of L1 (one impl per agent, §2.2).
- **residency tiers** — how resident a session is, managed by the **`SessionStore`** ("the
  store", §8): **(a) resident** (`Session` in RAM), **(b) materialized** (parsed, on disk
  as `<id>.jsonl` + a consumed byte offset), **(c) path-only** (just the source path — the
  cheap registry the live server calls its "level-3 cache").

---

## 1. Current architecture map

### 1.1 The two parse entry points (per-agent `match` dispatch)

```
model::parse_path_for(agent, path, args)          model.rs:744
   ├─ Agent::Claude → parse_path                   model.rs:618
   │     ├─ parse_file                             model.rs:633   (2 streaming reads)
   │     │     ├─ scan_tool_ids (pass 1: id set)   model.rs:756
   │     │     └─ parse_main    (pass 2: blocks)   model.rs:853
   │     └─ enrich_subagents (recursive)           model.rs:672
   └─ Agent::Codex → codex_model::parse_codex_path codex_model.rs:13
         ├─ scan_call_ids (pass 1)                 codex_model.rs:40
         └─ parse_lines   (pass 2)                 codex_model.rs:59
```

`parse_main` (Claude) and `parse_lines` (Codex) are **two copies of the same
streaming skeleton**: a `slots: HashMap<id,usize>` (tool_use id → block index), a
`pending: HashMap<id, result>` buffer for results that precede their tool_use, a
`cwd` capture, a `trigger_ts` for thinking-duration, and a final block list. See
`model.rs:864-867` vs `codex_model.rs:70-73`. What genuinely differs between them
is only **how one JSONL line maps to blocks** (Claude's `type:assistant/user/
queue-operation/attachment` + `toolUseResult` shape vs Codex's `response_item`
`payload` shape) — everything around that is duplicated.

Duplicated verbatim (or near-verbatim) across the two agents:
- `epoch_secs` — `model.rs:422` **and** `codex_model.rs:396` (identical algorithm).
- `relativize` — `model.rs:383` (`relativize_with`) **and** `codex_model.rs:381`.
- the id-prescan pass — `scan_tool_ids` (`model.rs:756`) **and** `scan_call_ids`
  (`codex_model.rs:40`).
- the back-patch loop shape — `parse_main` **and** `parse_lines`.
- `stamp_user_turns` is the one piece already shared (`pub(crate)`, `model.rs:1364`,
  called from `codex_model.rs:83,179`).

### 1.2 Metrics: a *separate* pass that re-opens the file

`metrics::parse_reader_for(agent, reader)` (`metrics.rs:88`) streams the file again,
dispatching to `parse_reader` (Claude, `metrics.rs:83`) or
`codex_metrics::parse_codex_reader` (`codex_metrics.rs:5`). It has a **third**
timestamp parser, `parse_ts` (`metrics.rs:34`), distinct from the two `epoch_secs`.

The metrics pass is a second **file open** on top of the parse's two reads:
- TUI: `app.rs:205` parses, then `app.rs:217-220` opens the file **again** for metrics.
- HTML: `html_export.rs:779` parses (via `parse_path_timed_for`), then
  `html_export.rs:781-787` opens the file **again** for metrics.
- Sub-agents: `subtree_cost` (`model.rs:697`) opens **each child file again**
  (`model.rs:698`) for its cost, even though `enrich_subagents` just parsed that
  same child via `parse_file` (`model.rs:679`). So a node costs 2 parse reads + 1
  metrics read = **3 opens per sub-agent**.

### 1.3 The `_timed` variant duplicates the file read

`parse_path_timed_for` (`model.rs:718`) is what the HTML export needs (one wall-clock
timestamp per user turn). Its `Agent::Claude` arm **re-inlines** `parse_file`'s
two-pass open/scan/parse (`model.rs:726-737`) instead of reusing it, purely to
thread a real `user_times` vec through (the non-timed path passes a throwaway
`&mut Vec::new()`).

### 1.4 Two renderers over the same blocks — and a classification split

Both renderers are already **agent-agnostic** `Block → …` formatters (good):
- `render.rs` — `Block → Vec<Line<'static>>` (ratatui). Core entry `block_body` /
  `assemble` (`render.rs:1091,1102`). Owns the summary/label helpers `activities`,
  `turn_summary`, `edit_summary`, `display_name`, `line_diff`, `agent_chip`,
  `diff_counts`, `WRITE_PREVIEW`.
- `html_export.rs` — `Block → JSON block stream`. Core entry `Emitter::block`
  (`html_export.rs:399`). It **reuses** many render helpers (`render::activities`,
  `render::turn_summary`, `render::edit_summary`, `render::display_name`,
  `render::line_diff`, `render::agent_chip`, `render::capitalize`,
  `render::WRITE_PREVIEW`) — so the summary strings are shared.

But three separate **name → category** maps have drifted apart:
- `model::tool_fold_key` (`model.rs:362`) → `read/edit/write/bash/skill/tool`.
- `html_export::html_kind` (`html_export.rs:211`) → adds `act/think/agent` splits.
- render's inline `match name` arms (`render.rs:547,562,584`, `display_name:650`).

The two diff renderers have **already diverged into a bug**: the TUI Edit diff
numbering is wrong while the HTML path is correct (`DESIGN.md:494`). That is the
concrete cost of `render::render_patch`/`diff_lines` (`render.rs:349,297`) and
`html_export::diff_part` (`html_export.rs:288`) each re-implementing hunk numbering.

### 1.5 Fold policy — already shared (keep it)

`view::FoldPolicy` (`view.rs:167`) keys off `model::fold_key` (`model.rs:192`) and is
consumed by both surfaces: the TUI via `collapsed_for` (`view.rs:223`) and the HTML
export via `fold.collapses(b)` → `data-open` (`html_export.rs:406`). This is the
model the whole refactor should imitate: **one core policy, two consumers.**

### 1.6 The coupling/duplication problems, itemized

1. **Agent dispatch is scattered.** `match agent { Claude, Codex }` appears in
   `model.rs:607,718,744`, `metrics.rs:88`, plus `discover.rs` (`detect_agent:311`,
   `resolve_any`, `candidates_all`). Adding a third agent edits every arm + the
   `Agent` enum (`lib.rs:32`). Contrast the JDI side, where a new agent is "one
   module + one registry arm" (`jdi/agent.rs:265`, `jdi/DESIGN.md:168`).
2. **Agent-specific event parsing is duplicated, not abstracted** (§1.1).
3. **Double/triple file opens for metrics** (§1.2) — a separate pass that re-reads
   what parse already read.
4. **`_timed` duplicates the file-read plumbing** (§1.3).
5. **Per-surface re-walks + drifted classification** (§1.4) — three name→kind maps,
   two diff numberers (one already buggy).
6. **Sub-agent enrich is Claude-only.** `enrich_subagents` (`model.rs:672`) runs only
   in `parse_path`; Codex sub-agents (which exist — `codex_discover.rs:140`
   `subagent_snippet`) are never resolved.
7. **No library surface.** Everything is reachable only through `pub` functions that
   assume the `Args` clap struct (`model.rs` threads `&Args` everywhere though the
   parse ignores it — `parse_main`'s `_args`, `model.rs:856`). A third party can't
   get `Session { blocks, metrics }` without depending on the CLI.

---

## 2. Proposed layering

> **Note (reconciled with §0):** this section was written before the three-layer framing
> and shows L1+L2 *fused* — `Transcript::steps` emitting `Step::Emit(Block)` with the
> `Engine::run` back-patch loop. The current design splits them at the **canonical message
> log** (§0, §6.1-resolved): L1 (`Transcript`) emits *messages*, not built blocks; L2 folds
> messages → `Session`. Read `Step` below as "the message an adapter emits" and `Engine::run`
> as "L2's fold"; the `EngineOut`/`Session` shape and the registry idea are unchanged.

```
                         ┌──────────────────────────────────────────┐
   thin per-surface      │  render.rs   (Block → ratatui Line)       │  formatters
   FORMATTERS  ─────────▶│  html_export.rs (Block → JSON stream)     │  (agent-agnostic)
                         └──────────────────────────────────────────┘
                                          ▲  Session { blocks, metrics, user_times, cwd, agent }
                         ┌────────────────┴─────────────────────────┐
   agent-agnostic        │  engine::                                 │
   CORE (no TUI, no      │    Engine::run(&dyn Transcript, reader)    │  streaming skeleton:
   syntect, no clap)     │      · id pre-scan  · back-patch loop      │  slots/pending, cwd,
                         │      · trigger_ts + thinking duration      │  trigger_ts, user_times,
                         │      · user-turn stamping                  │  metrics fold-in
                         │      · sub-agent enrich (lazy option)      │
                         │    Block / SubAgent / Attachment (model)   │  block model + tree
                         │    fold_key / BlockKind / tool_kind        │  ONE classification
                         │    Metrics + MetricsAcc                    │  metrics (folded in)
                         │    time::epoch_secs · path::relativize     │  shared helpers
                         └────────────────▲─────────────────────────┘
                                          │  trait Transcript
                         ┌────────────────┴─────────────────────────┐
   agent-specific        │  claude_transcript.rs   (one file/agent)  │
   ADAPTERS              │  codex_transcript.rs                       │
   (one file per agent)  │  registry: transcript(Agent) -> Box<dyn>  │  ← mirrors jdi::agent::adapter
                         └──────────────────────────────────────────┘
```

Crate/module layout (staying in the existing crate; a separate `claude-replay-core`
crate is an optional final step — see §5):

```
src/
  engine/
    mod.rs        Engine::run, parse_session*, Session, ParseOptions      (agent-agnostic)
    event.rs      Step, RawResult, LineCtx  (the adapter boundary types)
    block.rs      Block, SubAgent, Attachment, AgentStatus, Hunk, fold_key, BlockKind  (moved from model.rs)
    metrics.rs    Metrics, MetricsAcc                                     (moved from metrics.rs)
    time.rs       epoch_secs (the single copy)
    path.rs       relativize / relativize_with (the single copy)
    subagent.rs   enrich (lazy/eager), subtree cost from folded metrics
    registry.rs   transcript(Agent) -> Box<dyn Transcript>
  agents/
    claude.rs     impl Transcript for ClaudeTranscript   (was parse_main + apply_result + scan_tool_ids)
    codex.rs      impl Transcript for CodexTranscript     (was parse_lines + apply_output + scan_call_ids)
  render.rs       Block → ratatui Line   (unchanged formatter; reads engine::block)
  html_export.rs  Session → JSON stream  (drops its 2nd metrics open + parse_path_timed_for)
  view.rs app.rs tail.rs discover.rs …   (unchanged, re-pointed imports)
```

### 2.1 The adapter boundary types (core-owned)

```rust
// engine/event.rs

/// One ordered step an adapter emits for a single JSONL line. The core owns all
/// mechanism (ordering, id→index slots, back-patching, timestamps); the adapter
/// only classifies + shapes blocks.
pub enum Step {
    /// Session cwd (first non-empty wins; used to relativize tool targets).
    Cwd(String),
    /// Append a fully-formed block in order (UserText, AssistantText, Attachment,
    /// Command, QueueEvent, orphan ToolResult, SubAgent spawn, …).
    Emit(Block),
    /// A tool_use whose result arrives later — the core records `id → index` so a
    /// matching ToolResult can back-patch it (and applies any already-`pending` one).
    ToolUse { id: String, block: Block },
    /// A tool_result to join onto its ToolUse by `id`. The core routes it: patch the
    /// emitted block (via `apply_result`), else hold in `pending`, else (id in no
    /// tool_use anywhere) hand back as an orphan for the adapter to `Emit`.
    ToolResult { id: String, raw: RawResult },
    /// Assistant thinking — the core computes `duration_secs` from this line's
    /// timestamp minus the running `trigger_ts`, so adapters don't touch the clock.
    Thinking { text: String },
    /// This line is a generation trigger (a user turn / tool result) — resets the
    /// thinking clock. (Claude: `type:user`; Codex: a user message / call output.)
    Trigger,
    /// A sub-agent completion notification, applied after the loop (Claude only).
    Completion(String),
}

/// A tool result before agent-specific interpretation. The adapter's `apply_result`
/// re-reads it (Claude wants `toolUseResult` + text; Codex wants the output string).
pub struct RawResult {
    pub text: String,
    pub meta: serde_json::Value, // Claude toolUseResult; Value::Null for Codex
}

/// Read-only per-line context the core hands the adapter (timestamp already parsed).
pub struct LineCtx<'a> {
    pub cwd: &'a str,          // session cwd captured so far ("" until first Cwd)
    pub ts: Option<f64>,       // epoch seconds of this line, via engine::time::epoch_secs
}
```

### 2.2 The `Transcript` trait (one impl per agent)

Modeled on `jdi::agent::AgentAdapter` (`jdi/agent.rs:143`) and its registry
(`jdi/agent.rs:265`): a small required core + defaulted optional hooks.

```rust
// engine/mod.rs

pub trait Transcript {
    /// Which agent this adapter is (drives `Session.agent` + the registry arm).
    fn agent(&self) -> Agent;

    /// Pass 1 (id pre-scan): if `line` is a tool_use, its join id. Collected into a
    /// `HashSet` so pass 2 can tell a genuine orphan result from a not-yet-seen one.
    /// (Claude: assistant `tool_use.id`; Codex: `payload.call_id`.)
    fn tool_use_id<'a>(&self, line: &'a serde_json::Value) -> Option<&'a str>;

    /// Pass 2: map one JSONL line to zero or more ordered `Step`s. This is the ONLY
    /// place agent event shapes live. (Replaces the bodies of `parse_main` /
    /// `parse_lines`.)
    fn steps(&self, line: &serde_json::Value, cx: &LineCtx) -> Vec<Step>;

    /// Back-patch a tool_use `Block` from its raw result (agent-specific fields:
    /// Claude fills output/patch/read_lines + SubAgent id/status/result; Codex fills
    /// output). Was `model::apply_result` (model.rs:783) / `codex_model::apply_output`.
    fn apply_result(&self, block: &mut Block, raw: &RawResult);

    /// Fold this line's token/model/timestamp usage into the running accumulator —
    /// so metrics come out of the SAME streaming pass (no second file open). Was
    /// `metrics::parse_from_lines` (metrics.rs:95) / `codex_metrics` (codex_metrics.rs).
    fn fold_metrics(&self, line: &serde_json::Value, acc: &mut MetricsAcc);

    // ── optional hooks (defaults = "agent doesn't do this") ──────────────────

    /// Post-process the flat block list. Claude groups thinking-turns + coalesces
    /// activity runs (`group_turns`+`coalesce_activity_runs`, model.rs:1229); Codex
    /// leaves it flat. Default: identity.
    fn finish(&self, blocks: Vec<Block>) -> Vec<Block> { blocks }

    /// After the loop, apply completion notifications collected via `Step::Completion`
    /// (Claude's `<task-notification>` join, model.rs:1187). Default: no-op.
    fn apply_completions(&self, _blocks: &mut [Block], _notes: &[String]) {}

    /// The on-disk child transcript for a spawned sub-agent, if the agent has them.
    /// Was `model::subagent_file` (model.rs:658). Default `None` (no sub-agents).
    fn subagent_child(&self, _session: &Path, _agent_id: &str) -> Option<PathBuf> { None }

    // ── discovery / detection (could also be split into a `Discover` trait) ──

    /// Does this transcript head belong to me? Was the arms of `discover::detect_agent`
    /// (discover.rs:311). Registry tries each adapter; default Claude wins ties.
    fn detect(&self, head: &[serde_json::Value]) -> bool;

    /// Session cwd from the transcript head. Was `discover::session_cwd` (discover.rs:288).
    fn session_cwd(&self, head: &[serde_json::Value]) -> Option<PathBuf>;
}

/// The registry — the single place that knows every agent (mirrors jdi/agent.rs:265).
/// Adding an agent = one module in `agents/` + one arm here.
pub fn transcript(agent: Agent) -> Box<dyn Transcript> {
    match agent {
        Agent::Claude => Box::new(crate::agents::claude::ClaudeTranscript),
        Agent::Codex  => Box::new(crate::agents::codex::CodexTranscript),
    }
}
```

### 2.3 The core engine (the shared skeleton, agent-neutral)

> `EngineOut` below is the L2 output in this older sketch — the same thing §3 promotes to
> the public **`Session`** (it gains `cwd`/`agent` + the `SessionIndex`). Read the two as one
> type; `Session` is the canonical name.

```rust
// engine/mod.rs

pub struct EngineOut {
    pub blocks: Vec<Block>,
    pub metrics: Metrics,
    pub user_times: Vec<Option<f64>>,
}

impl Engine {
    /// The whole streaming pipeline, once, for any adapter. Two fresh reads over
    /// `open()` (pass-1 id scan, pass-2 build) — the invariant from
    /// STREAMING-PARSE-DESIGN.md, unchanged. Metrics fold into pass 2 (no 3rd read).
    pub fn run(
        adapter: &dyn Transcript,
        open: impl Fn() -> std::io::Result<Box<dyn BufRead>>,
    ) -> std::io::Result<EngineOut>;

    /// In-memory batch variant for the live tail (`view::ingest`) and tests — no
    /// sub-agent enrich, no metrics needed. Wraps the `&str` over a Cursor so the
    /// existing `parse(&str)` callers are unchanged.
    pub fn run_str(adapter: &dyn Transcript, jsonl: &str) -> Vec<Block>;
}
```

Pass 2 body (pseudocode — this is exactly today's `parse_main` loop, minus the
agent-specific `match` which becomes `adapter.steps(...)`):

```
for line in reader:                       # one Value live at a time (unchanged)
    v = parse(line)?  (skip on error)
    ts = time::epoch_secs(v["timestamp"])
    stamp_user_turns(out, ..., pending_ts, user_times)   # model.rs:1364, shared
    pending_ts = ts
    adapter.fold_metrics(&v, &mut acc)                   # metrics folded IN
    cx = LineCtx { cwd, ts }
    for step in adapter.steps(&v, &cx):
        match step:
          Cwd(c)               => if cwd.is_empty() { cwd = c }
          Trigger              => trigger_ts = ts
          Emit(b)              => out.push(b)
          Thinking{text}       => out.push(Block::Thinking{ text,
                                    duration_secs: dur(ts, trigger_ts), tools: [] })
          ToolUse{id, block}   => i = out.push(block); slots[id]=i;
                                   if let Some(r)=pending.remove(id): adapter.apply_result(out[i], r)
          ToolResult{id, raw}  => if let Some(&i)=slots.get(id): adapter.apply_result(out[i], &raw)
                                   elif id in tool_ids: pending.insert(id, raw)
                                   else: out.push(orphan(raw))   # adapter decides shape
          Completion(s)        => notes.push(s)
stamp_user_turns(...)          # final flush
adapter.apply_completions(&mut out, &notes)
out = adapter.finish(out)                                # Claude: group+coalesce; Codex: identity
```

Claude and Codex each collapse to **one file** whose only content is
`tool_use_id` + `steps` + `apply_result` + `fold_metrics` + `finish`/`detect`/
`session_cwd`. All the slot/pending/timestamp/stamp machinery lives once in the core.

---

## 3. The public engine API (third-party surface)

The second goal of the refactor (beyond dedupe) is a **library other apps can build
on** — a web dashboard, a cost auditor, a CI transcript viewer, a "resume picker" —
with claude-replay itself as merely the reference consumer. That raises the bar: the
public types must be **clean-slate and agent-neutral**, *not* a re-export of today's
Claude-shaped `Block`/`model.rs`. Where today's model carries dialect warts — a
`QueueEvent`, the two-event completion verbs, `user_times` as a side `Vec` — the library
normalizes them into general shapes (below), and §5's compatibility map is how today's
code re-homes onto them without changing a pixel of user-visible output.

Three crates, one per layer, each usable alone:

| Crate | Layer | Depends on | Gives you |
|---|---|---|---|
| **`replay-core`** | L1 + L2 | `serde` only | `Message`, `Adapter`, `Parser`, `Session`, `Replayer`, `SessionStore` |
| **`replay-tui`** | L3 | core + ratatui + syntect | `Viewer`, `ViewState`, ratatui `Line`s |
| **`replay-html`** | L3 | core (+ syntect) | `HtmlPresenter`, `HtmlServer`, the record stream |

`replay-core` is the load-bearing one — no ratatui / syntect / clap, so it drops into any
Rust context (native, server, WASM). The two presenters are optional and independent; a
caller can take the data model and render it their own way.

### 3.1 The message log — the L1↔L2 contract

The single vocabulary L2 folds over. Agent-agnostic and append-only, so teaching the
engine a new agent is a new `Adapter` (§3.2) and *never* a change here. Every event
carries a common envelope (position, byte offset, time) so L2 can index and resume
without re-deriving it.

```rust
// replay-core::msg

pub struct Message {
    pub seq: Seq,            // monotonic position in the log
    pub offset: u64,         // byte offset in the source → the resume cursor (§3.2)
    pub time: Option<Time>,  // wall-clock, if the raw line carried one
    pub event: Event,
}

/// One canonical event decoded from a single raw line. The dialect never leaks past here.
#[non_exhaustive]
pub enum Event {
    User { text: Text, attachments: Vec<Attachment> }, // a human turn
    Say { text: Text },                                // model-authored prose
    Think { text: Text },                              // model private reasoning
    ToolCall { call: CallId, tool: String, input: Value },   // joins to a later result
    ToolResult { call: CallId, output: Value, ok: bool },    // joined by `call`
    Spawn { agent: AgentId, kind: String, prompt: Text },    // a sub-agent launched
    AgentEnd { agent: AgentId, status: RunStatus, summary: Text }, // …reached terminal
    Meta(Meta),   // a session-scoped fact on this line: cwd, model, usage delta, title
    Reset { from: Seq },  // the source tail from `from` was rewritten — see below
}
```

`Reset` is the **only** rewind: on a live edit or compaction the parser detects that the
bytes it already consumed changed and emits `Reset { from }`, telling the replayer to drop
everything it built at/after `from` and re-fold what follows. Everything else is a pure
append. This is what makes an incremental live tail cheap and a crash-safe on-disk log
possible (§8.3.2).

> **Implementation status — the `Message` waypoint.** Phase 1 (§5.2) has *landed* a working
> L1/L2 split: `model::tokenize` (L1) + `model::replay` (L2), proven `replay(tokenize(x))`
> **bit-identical** to `parse_main` across the golden corpus. Its message log
> (`engine::message::Message`) is a deliberate **waypoint**, not yet this clean `Event`: to
> reuse the exact block-shaping the golden tests already pin, its `ToolUse` variant still
> carries a built `model::Block`, its variants stay Claude-shaped (`QueueOp`, `UserString` /
> `UserArrayText`, `LineStart` / `Trigger`), and it has no `seq` / `offset` / `Reset`
> envelope yet. It converges on the `Event` above in two later, separately-gated steps: the
> **block-model lift** drops the `Block` back-reference and folds the variants into this
> agent-neutral set (so the shaping — `tool_target`, `extract_diffs`, the two-event
> spawn/completion split — moves wholly into L2's fold); the **incremental phase** (Phase 6)
> adds the envelope + `Reset`. Crucially, `tokenize(lines) -> Vec<Message>` and
> `replay(&[Message]) -> …` are already **pure, synchronous, I/O-free** functions — the split
> embodies §3.6's sans-I/O pull core from day one, so only the *vocabulary* has to converge,
> not the control-flow shape.

### 3.2 Layer 1 — the `Adapter` trait and the `Parser`

Two pieces: the **`Adapter`** is the *entire* agent-specific surface (one impl per
transcript dialect); the **`Parser`** is the agent-neutral driver that turns bytes into the
message log, batch or incrementally.

```rust
// replay-core::l1

/// Teach the engine one agent's raw line format. Stateless w.r.t. other lines — grouping,
/// tool-result joining and turn-shaping are L2's job, not the adapter's. This is all a new
/// agent needs to implement.
pub trait Adapter: Send {
    fn agent(&self) -> Agent;                       // the provenance tag it stamps
    fn detect(head: &str) -> bool where Self: Sized; // sniff a transcript's first bytes
    fn decode(&mut self, line: &RawLine, out: &mut dyn Extend<Event>); // 1 line → 0..n events
    fn session_cwd(&self, line: &RawLine) -> Option<PathBuf> { None }
}

/// Drives an `Adapter` over bytes → the append-only `Message` log. **Single pass**,
/// streaming: only one raw line is resident at a time (the file is never buffered whole),
/// and each `advance` consumes *as many lines as the reader currently yields*, returning
/// them as a **batch** of messages. "One line at a time" is the memory bound, not the call
/// granularity — a call typically emits many messages.
///
/// The `Parser` is **stateful** — it holds the adapter, a byte `cursor`, and a small
/// kept-tail digest — so a cold read and a live resume are the *same* operation at a
/// different starting offset. (There is no second "pre-scan" pass: the `tool_use`-id set
/// that tells an orphan result from a not-yet-seen one is derived by the **`Replayer`** from
/// the message log, §3.3 / §6.1 — an L2 concern, not a second L1 read. That is what keeps
/// L1 a clean single pass and lets cold + live share one shape.)
pub struct Parser { /* adapter + cursor + a small kept-tail digest */ }

impl Parser {
    /// Sniff which adapter owns a transcript (tries each registered `Adapter::detect`).
    pub fn detect(head: &str) -> Option<Agent>;

    /// Cold start — cursor at byte 0.
    pub fn new(agent: Agent) -> Self;
    /// Live start — cursor at a previously-saved offset.
    pub fn resume_at(agent: Agent, cursor: Cursor) -> Self;

    /// Consume whatever `reader` yields from the current cursor onward; return the messages
    /// those bytes complete, and advance the cursor. A partial trailing line is left
    /// unconsumed for the next call. On a live poll, if the kept tail no longer matches the
    /// source (an edited / compacted transcript), the returned batch begins with an
    /// `Event::Reset`. The caller positions `reader` at `self.cursor().offset` (sans-I/O:
    /// the `Parser` owns no file handle — §3.6).
    pub fn advance(&mut self, reader: impl BufRead) -> io::Result<Vec<Message>>;

    /// The current byte cursor — persist it between live polls.
    pub fn cursor(&self) -> Cursor;
}

pub struct Cursor { pub offset: u64, pub seq: Seq /* + kept-tail digest */ }
```

So both paths are one method: a **cold read** is `Parser::new(agent).advance(whole_file)`; a
**live poll** is `Parser::resume_at(agent, saved).advance(tail)` then persist `cursor()`.
Same shape, same single pass — the offset is the only difference.

A third party adding a new agent (say a local LLM's logs) writes **one** `Adapter` and
registers it; every presenter and the whole store/live machinery work unchanged.

### 3.3 Layer 2 — the `Session` object and the `Replayer`

`Session` is the single source of truth: immutable **data** — everything a presenter needs,
and nothing about *how* it's being viewed (that is `ViewState`, §3.4). The `Replayer` is the
only place back-patch/grouping happens; it folds forward and rewinds only on `Reset`.

```rust
// replay-core::session

pub struct Session {
    pub agent: Agent,
    pub meta: SessionMeta,     // cwd, model, title, started_at, source path
    pub blocks: Vec<Block>,    // the ordered transcript, tool results already joined
    pub metrics: Metrics,      // token / cost tally, folded during replay (§4.2)
    pub index: SessionIndex,   // §7: turn spans, tool / agent / attachment indices, liveness
}

/// A presentation-ready unit — agent-neutral. A Claude `QueueEvent`, a Codex reasoning
/// summary, and a future agent's system line all normalize into these; the block model
/// never encodes a dialect.
#[non_exhaustive]
pub enum Block {
    User(Text),
    Say(Text),
    Think { text: Text, duration: Option<Duration> },
    Tool(ToolUse),      // one call: input + joined result + status
    Agent(SubAgent),    // a spawned child: id, kind, status, rolled-up cost, lazy sub-session
    AgentEnd(AgentRef), // the child's terminal event, at its position in the stream (§ two-event)
    Notice(Notice),     // a generic system / meta line (queue, hook, compaction marker)
}

/// Folds a message log into a `Session`. Forward-only except on `Event::Reset`.
pub struct Replayer { /* raw fold state: block buffer + call_id→index map + pending + cursor */ }
impl Replayer {
    pub fn new(agent: Agent) -> Self;
    /// Fold a batch of messages into the running state: append, back-patch tool results via
    /// the id index, rewind only on `Event::Reset`. Call repeatedly to stream a live tail.
    pub fn apply(&mut self, delta: &[Message]);
    /// The current presentable session — runs `finish` (turn grouping) + builds the index
    /// over the accumulated blocks, returned **owned**. Non-consuming: keep applying after.
    /// (It cannot be a `&Session` borrow: `finish` isn't incremental — it recomputes the
    /// grouped view each call — so the presentable session is freshly built, not stored.)
    pub fn snapshot(&self) -> Session;
    /// Finalize: like `snapshot` but **consumes** the `Replayer`, reclaiming the intermediate
    /// fold state instead of cloning out of it. The terminal call of a batch parse —
    /// `parse_session` is `Replayer::new(a); apply(msgs); into_session()`.
    pub fn into_session(self) -> Session;
}

/// The one call most callers want: detect → parse → replay, sub-agents per `opts`.
pub fn parse_session(path: &Path) -> io::Result<Session>;
pub fn parse_session_with(path: &Path, opts: &ParseOptions) -> io::Result<Session>;

#[derive(Default, Clone)]
pub struct ParseOptions { pub subagents: SubAgentLoad } // Eager (default) | Summary | Skip (§4.3)
```

Two normalizations worth calling out, because they retire today's warts:
- **`user_times`** (today a parallel `Vec<Option<f64>>`) becomes `index.turns[i].time` —
  the per-turn timestamp lives on the turn, not a side channel the caller must zip.
- **Sub-agent completion** is the clean two-event shape: `Block::Agent` at the spawn point
  carries the child's live `status`; `Block::AgentEnd` marks the terminal event at its own
  point in the stream. Both the sync (`Agent`-tool, inline result) and async (`Task`-tool,
  later notification) paths converge on this — the presenter reads `status`, never the raw
  dialect. (This is exactly the bug class that made sync sub-agents show "running" forever:
  one status field, two producers.)

### 3.4 Layer 3 — the `Presenter` contract (data vs. view)

The key separation: **`Session` is the data; `ViewState` is what the user is looking at.**
Keeping them apart means the same session renders to any surface, at any fold / width /
filter, with no mutation — and an incremental surface re-renders only what changed.

```rust
// replay-core::present  (the contract; concrete presenters live in the L3 crates)

pub struct ViewState {
    pub width: u16,
    pub fold: FoldPolicy,       // default | expand-all | per-block overrides
    pub filter: Option<Filter>, // by block kind / tool name
    pub focus: Option<BlockId>, // selection / scroll anchor
    pub theme: Theme,
}

/// A pure function from (data, view) to a surface. Stateless; incremental surfaces add a
/// `present_delta` so settled blocks aren't re-rendered.
pub trait Presenter {
    type Output;
    fn present(&self, session: &Session, view: &ViewState) -> Self::Output;
}
```

**TUI (`replay-tui`).** A `TuiPresenter` produces ratatui `Line`s; the `Viewer` wraps it
into the full interactive object claude-replay's `app.rs` is built on — it owns a `Session`
+ `ViewState`, maps input to view changes, tails live sessions, and descends into
sub-agents.

```rust
pub struct TuiPresenter;
impl Presenter for TuiPresenter { type Output = Vec<ratatui::text::Line<'static>>; /* … */ }

pub struct Viewer { /* session + view + a descend stack */ }
impl Viewer {
    pub fn new(session: Session) -> Self;
    pub fn render(&self) -> Vec<Line<'static>>;     // = TuiPresenter over the current state
    pub fn handle(&mut self, input: Input) -> Dirty; // j/k, fold, /search, filter, …
    pub fn ingest(&mut self, delta: &[Message]);     // live tail (drives a Replayer inside)
    pub fn descend(&mut self, agent: &AgentId) -> io::Result<()>; // push a child session
    pub fn ascend(&mut self);
}
```

**HTML (`replay-html`).** An `HtmlPresenter` emits the append-only record stream
(`meta`/`block`/`reset` JSON — the format the browser JS already renders); `page` wraps a
snapshot in the self-contained shell; `HtmlServer` is the live multi-agent server.

```rust
pub struct HtmlPresenter;
impl Presenter for HtmlPresenter { type Output = Vec<Record>; /* meta/block/reset */ }

pub fn page(session: &Session, view: &ViewState) -> String; // one self-contained .html

pub struct HtmlServer { store: SessionStore }               // §8
impl HtmlServer {
    pub fn new(store: SessionStore) -> Self;
    pub fn serve(self, addr: &str) -> io::Result<Url>;      // lazy per-agent streams + byte cursor
}
```

### 3.5 Composition — building apps from the pieces

The proof the interface is clean is that claude-replay's own binary is *nothing but*
composition over it. Each example below is a real claude-replay surface reduced to its
essence.

**(1) A cost / tool auditor — `replay-core` alone, no presentation.** Proves the data model
stands on its own:

```rust
let s = replay_core::parse_session(path)?;
println!("{}: {} turns · ${:.2}", s.agent, s.index.turns.len(),
         s.metrics.cost_usd.unwrap_or(0.0));
for t in s.index.tools_by_count() { println!("  {:>4}× {}", t.count, t.name); }
```

**(2) A one-shot text dump — core + one presenter** (this is `--dump`):

```rust
let s = replay_core::parse_session(path)?;
let view = ViewState { fold: FoldPolicy::Default, width: 100, ..ViewState::plain() };
for line in replay_tui::TuiPresenter.present(&s, &view) { println!("{}", line); }
```

**(3) The interactive TUI — the `Viewer` loop** (this *is* `claude-replay <path>`):

```rust
let mut viewer = Viewer::new(replay_core::parse_session(path)?);
let mut term = ratatui::init();
loop {
    term.draw(|f| f.render_widget(viewer.render_widget(), f.area()))?;
    match input::next()? {
        Input::Quit => break,
        Input::Descend(id) => viewer.descend(&id)?, // into a sub-agent
        Input::Ascend => viewer.ascend(),
        ev => { viewer.handle(ev); }
    }
}
```

**(4) A live tail — incremental resume + ingest** (this is `-f`):

```rust
let agent = Parser::detect(&head(path)?).expect("known agent");
let mut parser = Parser::new(agent);
let mut viewer = Viewer::new(Session::empty(agent));
loop {
    let mut f = File::open(path)?;
    f.seek(SeekFrom::Start(parser.cursor().offset))?; // read only the new tail
    let msgs = parser.advance(BufReader::new(f))?;    // many lines → a batch; cursor advances
    if !msgs.is_empty() {
        viewer.ingest(&msgs); // back-patch + Reset handled inside; O(delta), not O(file)
        redraw(&viewer);
    }
    fs_notify.wait();         // block until the file grows
}
```

**(5) The HTML live server — the store + server** (this is `--html -f`):

```rust
let store = SessionStore::new(cache_dir); // §8 tiers
store.see(root_path);                      // register the tree as tier (c), parse nothing yet
let url = HtmlServer::new(store).serve("127.0.0.1:0")?; // materializes each agent on first request
open::that(url)?;
```

Put together, claude-replay's `main.rs` is Example 3 plus a session picker; `--dump` is
Example 2; `--html -f` is Example 5. Nothing in the binary reaches under the library — which
is the working proof a third party could build their own transcript app (a web dashboard, a
CI log viewer, a cost report over a directory of sessions) on the same `replay-core`.

### 3.6 Push vs. pull — a sans-I/O core, with push as an opt-in shell

A streaming engine forces a control-flow choice: does the engine **call you** (push — it
owns a thread / fs-watch and invokes your callback when a block arrives) or do you **call
it** (pull — you hand it bytes and ask for the delta when you want)? The decision here, and
why it matters for a *library*:

**`replay-core` is strictly pull — a sans-I/O state machine.** `Parser::advance(reader)` and
`Replayer::apply(delta)` are pure and synchronous: the client supplies the bytes *and* the
timing; the engine never opens a file, spawns a thread, blocks, or holds a runtime. It is a
[sans-I/O](https://sans-io.readthedocs.io/) value you feed — like `rustls`'s connection
state machine or `h11` — not a service you subscribe to.

Why pull for the core:
- **Zero forced dependencies / portability.** No `tokio`, `notify`, or threads in
  `replay-core`, so it embeds anywhere: a native TUI, a server with its *own* runtime, WASM
  in a browser tab. A push core would conscript every consumer into its concurrency model.
- **Determinism & headless testing.** Feed a `&str`, assert the `Session` — no clock, no
  threads, no flake. The same property that lets the TUI test under `TestBackend` with no
  TTY; the core inherits it by never owning I/O.
- **Backpressure is free.** The client reads at its own pace and coalesces polls. The
  live-feed freeze bug (§8.3, the `/stream` byte cursor) was exactly a *pull-cursor
  discipline* bug, and its fix was a client-side cursor rule — no engine change. Pull put the
  control point where the bug, and the fix, actually lived.
- **Scheduling lives in ONE place — the store, not the core.** Many HTML clients tail the
  same session; the `SessionStore` (§8.3) owns a *single* background tailer and fans the
  delta out. A pushing core would duplicate that scheduling and fight it.

**Push is a thin, optional driver built ON the pull core** — for consumers who genuinely
want "tell me when it changed." It owns the I/O (an fs-watch + the pull loop) and delivers
deltas by callback or channel; it sits behind a feature flag (or inside `-tui` / `-html`),
and the core never depends on it:

```rust
// replay-core::follow  (feature = "follow"; this is the only thing that pulls in `notify`)
pub struct Follower { /* Parser + Replayer + cursor + fs-watch */ }
impl Follower {
    /// Reactive: call back on each batch (drop the handle to stop).
    pub fn spawn(path: &Path,
                 on_delta: impl FnMut(&[Message], &Session) + Send + 'static)
        -> io::Result<Follower>;
    /// …or stay pull-friendly: hand each batch of messages over a channel the client
    /// selects on (it drives its own `Replayer`).
    pub fn deltas(path: &Path) -> io::Result<Receiver<Vec<Message>>>;
}
```

Two inversions that look like push but are **not** this axis — internal, and deliberate:
- **`Adapter::decode(line, &mut out)`** is the engine calling the adapter — but only while
  the *client* is pulling bytes through it; the adapter is a mapping function, not an I/O
  owner. The `&mut dyn Extend<Event>` sink (vs. a returned `Vec`) is a zero-alloc streaming
  choice, nothing more.
- **`Viewer::handle(input) -> Dirty`** returns a redraw hint as a *value*; the client
  decides when to repaint. A return signal, not a callback — still pull.

So: **passive core, reactive edges.** The engine is something you drive; if you'd rather be
driven, you opt into a `Follower` — which is itself just a loop that drives the engine for
you. The push option never leaks its threads or deps into anyone who didn't ask for it.

---

## 4. Resource strategy

### 4.1 Keep the single streaming pass (two reads, one line resident)

Unchanged from `STREAMING-PARSE-DESIGN.md`: **pass 1** is the id-only pre-scan
(`adapter.tool_use_id`, `<1 MB` for 17 k tools); **pass 2** streams one `Value` at a
time, back-patching results via `slots`/`pending`. `Engine::run` takes an `open()`
closure that yields a fresh `BufRead` per pass, exactly like `parse_file`'s `open`
(`model.rs:635`). No whole-file `String`, no whole-file `Vec<Value>`. The refactor
must re-run the RSS table (298 MB ⇒ ~811 MB) to confirm zero regression.

### 4.2 Fold metrics INTO the parse pass (kill the extra open)

`adapter.fold_metrics(&v, &mut acc)` runs inside pass 2, on the `Value` already
decoded for `steps`. `Metrics` is produced from the same read, returned in
`EngineOut`. This removes:
- the second `File::open` in `app.rs:217` and `html_export.rs:781`;
- the whole `metrics::parse_reader_for` dispatch (`metrics.rs:88`) as a separate
  file pass — its per-agent token extraction moves into `fold_metrics`;
- the third timestamp parser `parse_ts` (`metrics.rs:34`) — reuse `engine::time`.

`MetricsAcc` is the running tally (`input/cache/output/model/tmin/tmax`); `Metrics`
(the public struct, `metrics.rs:7`) is unchanged, produced by `acc.finish()`
(pricing/duration as today, `metrics.rs:129-142`). Net reads for a root session:
**2 (was 3)**; for a sub-agent node: **2 (was 3)** — see §4.3.

Tradeoff: `fold_metrics` decodes the same `Value` as `steps`, but that `Value`
already exists in pass 2 — this is *free* CPU vs today's extra full file read.
Live-tail metrics refresh (`app.rs:325-331`) can either keep its cheap standalone
`fold_metrics` reader **or** just reuse the `Session.metrics` from the re-parse it
already does on Codex/reset — a minor follow-up, not load-bearing.

### 4.3 Make the sub-agent tree lazy where it helps

Today `enrich_subagents` (`model.rs:672`) is **eager + recursive**: opening a root
session parses the *entire* agent subtree up front, and `subtree_cost` (`model.rs:697`)
opens each child a *second* time for its cost. For a wide tree this is many opens at
load — latency the TUI pays before the first frame even though most sub-agents are
never descended into.

`SubAgentLoad`:
- **`Eager`** (default; today's behavior) — parse every child fully, inline into
  `.blocks`, roll up cost. Keeps `render::agent_chip` (`render.rs:30`, needs the
  child tool count) and the descend path (`app.rs:243`, reuses `sa.blocks`) working
  with zero change.
- **`Summary`** — resolve each child with a **metrics-only** pass (`fold_metrics`,
  no block build) to fill `status` + `subtree_cost` + a `tool_count`, but leave
  `.blocks` empty until descend. The collapsed chip renders from the summary; the
  full child parse happens lazily in `build_child_frame` (`app.rs:236`) via
  `engine::parse_session_as(child_path)`. This is the memory-vs-latency win: a
  root with 200 sub-agents pays ~200 cheap metric scans instead of 200 full parses
  + 200 cost re-opens.
- **`Skip`** — spawns render as leaves (`.blocks` empty, no cost); the cheapest,
  for a library caller that only wants the top-level transcript.

Because §4.2 folds metrics into the parse, `subtree_cost` no longer needs its own
`File::open` (`model.rs:698`) even in `Eager` mode — the child's cost is just
`child_session.metrics.cost_usd`. So Eager drops from 3 opens/node to 2, and
Summary is 1 (metrics-only) + a deferred full parse only on descend.

Tradeoff to validate: `Summary` needs a metrics-only fast path that does **not**
build blocks (else it's not cheaper). Either a `fold_metrics`-only loop (one read,
no `steps`) or accept that counting tools needs `steps` anyway — measure whether a
tool-count needs the block build or can come from `tool_use_id` pass-1 counting.

`SubAgentLoad` is the *per-parse* materialization knob; the `SessionStore` (§8) is the
*cross-session* residency layer above it. A `Summary`-loaded child's `AgentEntry.child`
is `NotLoaded{path}` until the store's `load` promotes it to a full `Session` on
descend — the two compose: `SubAgentLoad` decides how much a single parse materializes,
the store decides which sessions stay resident.

### 4.4 Per-layer memory footprint — index-resident, content-on-demand

The three layers have very different memory profiles; the design exploits that so a large
session is **cheap to hold resident**. The rule per layer:

**Layer 1 (raw parser) — O(1), streaming.** Emits parsed messages **sequentially as the
consumer pulls them** (a pull iterator, not a buffered `Vec`); it never holds more than the
message it is mid-emitting. This is the one-line-resident invariant (§4.1) restated as "one
*message* resident." L2 consumes each message and lets it go.

**Layer 2 (replay / state builder) — O(index), *not* O(content).** As it folds the message
stream it builds and keeps in RAM only:
- the **index** — per block, its `id`, kind, a few metadata fields, and a **byte offset**
  locating its content (in the materialized block stream on disk — tier (b) — or a
  compressed in-RAM arena); and
- the **session metadata** — metrics, cwd, the turn map, the §7 agent/tool/attachment
  indices.
It does **not** retain the full block/JSON objects. To hand a block to L3 it **loads the
content on-demand** from the offset (read + deserialize the one record, or decompress the
one slot). So a resident `Session` *is* the index — a small fraction of the content (a
47 MB transcript's index is KB–low-MB, not the ~hundreds of MB the blocks would be).
- This unifies with the incremental fold (§8.3.2): the **stable** prefix's block records
  are immutable in tier (b) and indexed by offset; only the **unstable tail** (current
  turn, still subject to back-patch/grouping) is held as live block objects in RAM, then
  flushed to tier (b) when the turn completes. So L2's RAM = *the whole index + the current
  turn's content*, nothing more.
- It also sharpens the §8 tiers: **tier (a) "resident" = the index in RAM**; content is
  always tier (b) (on disk / compressed). Eviction drops the index too (→ tier (c)); a
  re-`load` rebuilds the index by a streaming L1→L2 re-parse — cheap relative to holding
  everything, and the store can keep *many* sessions' indexes resident at once.

**Layer 3 (presentation) — O(rendered working set).** The only layer that holds heavy
objects (ratatui lines / DOM / highlighted code), and it holds only the **working set**:
the current viewport + a margin (virtualized rendering), materialized from L2 on demand and
dropped when scrolled far away. The TUI's existing positional `body_cache` is the seed of
this; the HTML client's DOM is naturally windowable.

**Net:** holding a session resident costs ~its index (L2); blocks stream through L1 and
live in tier (b); only what's on screen is fully materialized (L3). To make the on-demand
load a **direct read, not a re-fold**, tier (b) stores L2's *output* (the already
folded/grouped block records — exactly the HTML `<id>.jsonl`), so loading block `N` never
requires re-examining its neighbors. Refine when built: the compressed-in-RAM arena vs
disk choice for tier (b); index-entry layout; and the L3 window/eviction policy.

---

## 5. Migration plan — refactor to the three layers with **zero user impact**

The re-architecture is large, so it lands as a sequence of independently-shippable phases,
each compiling + `cargo test`-green, each preserving user-visible behavior byte-for-byte.
No big-bang rewrite; the old code path stays live until the new one is proven equal.

### 5.0 The preservation contract (what "no user impact" means, and how it's enforced)

Every phase must hold these invariant outputs — they are the acceptance gate:

- **`--dump` / `--dump-html` / `--dump-all-html`** produce **byte-identical** files (the
  one intentional exception, called out where it lands: the TUI/HTML Edit diff-numbering
  bug is *fixed* when the two numberers unify — that changes TUI output on purpose).
- **The live HTML on-disk stream format** (`<id>.jsonl` records: `meta`/`block`/`reset`,
  their `head`/`body`/`kind` shapes) is **unchanged**, so a page served by an older binary
  and one served by the new engine render identically, and the client JS needs no change.
- **The TUI** renders identically (same `TestBackend` cell output; same tmux e2e).
- **Same CLI** — every flag, `--latest`, the picker, `-f`, `agent-jdi` — unchanged.
- **RSS not regressed** (the streaming invariant) and **live CPU only improves** (the
  incremental phases are strict wins over today's whole-file re-parse).

Verification harness (build once, run every phase):

1. **Golden parse tests** (already exist): `parse_path_matches_parse_str` (`model.rs`),
   `result_before_tool_use_still_joins`, `orphan_result_with_no_tool_use_shown_inline`,
   `joins_tooluseresult_metadata`, the coalesce/group tests, the Codex
   `parse_path_matches_string…`, plus the sub-agent + two-event + attachment tests.
2. **A dump-equivalence corpus sweep**: a script that runs `--dump`, `--dump-html`,
   `--dump-all-html` over a fixed corpus of real transcripts and diffs against a
   checked-in set of output hashes. Run before/after each phase — the primary "no user
   impact" proof. (Corpus lives in the sibling `claude-replay-eval` repo; no real `.jsonl`
   in this tree.)
3. **HTML stream tests** (`html_export` `stream(...)`) + the served/live browser smoke.
4. **The `tmux` e2e** for TUI + descend/ascend + live tail.
5. **An RSS probe** re-run at the metrics-fold and incremental phases (298 MB session).

### 5.1 Existing functionality → where it lands (the compatibility map)

Nothing is dropped; everything re-homes onto a layer. The middle column is what *moves*;
the right column is what *proves it still works*.

| Existing functionality (today) | Re-homes to | Preserved by |
|---|---|---|
| `parse_main` / `parse_lines` (parse **+** back-patch **+** group, fused) | **L1** tokenize (per agent) **+ L2** fold (shared) | golden parse tests + dump sweep |
| `metrics::parse_reader_for` (separate 3rd pass) | **L2** metrics fold (same pass) | metrics footer tests + RSS probe |
| `enrich_subagents` / `subtree_cost` (eager tree) | **L1/L2** per-source + `SubAgentLoad` + the store | sub-agent + descend tests |
| `render.rs` (blocks → ratatui) | **L3** presenter over `Session` | TestBackend + tmux e2e |
| `--dump` text/ansi | **L3** presenter | dump sweep |
| `html_export` Emitter (blocks → records) | **L3** presenter (the record stream) | HTML stream tests |
| the live HTML client JS (records → DOM) | **L3b** (client half; unchanged) | browser smoke |
| `html_export::{serve, follow_tree, agent_stream, Live}` | the **SessionStore** + **L2 incremental ingest** | served smoke + a store test |
| `tail.rs` `TailReader` + `view::ingest` (TUI live) | **L1 incremental** + **L2 ingest** (shared with HTML) | tmux live-tail e2e |
| `discover` / `codex_discover` (`detect_agent`, cwd, candidates) | **L1** adapter discovery hooks | discovery tests |
| `agent-jdi` (uses `discover`/`model`) | consumes the **library `Session`** (§3); no behavior change | jdi fixture tests |
| the `--dump-html` / `--dump-all-html` / attachments / two-event model | **L3** over the same `Session`; unchanged records | existing tests + dump sweep |

### 5.2 The phases

**Phase 0 — dedupe pure helpers (no behavior change).** `engine/time.rs` (`epoch_secs`),
`engine/path.rs` (`relativize*`); fold `metrics::parse_ts` onto `engine::time` (one `i64`
→ `f64` cast). Green by construction.

**Phase 1 — carve out Layer 1 and Layer 2, Claude first (the highest-risk step).** Define
the canonical **`Message`** log (the L1↔L2 contract) and split `parse_main` into: L1
`tokenize(lines) -> Vec<Message>` (the Claude adapter — line shapes only, **no**
back-patch/grouping) and L2 `replay(messages) -> Session` (agent-agnostic forward fold: the
`id → block ref` back-patch, `group_turns`/`coalesce_activity_runs`, `stamp_user_turns`).
Keep the old `parse_main` alongside; assert `replay(tokenize(x))` is **bit-identical** to it
on the whole golden corpus before deleting it. This is where the `pending`/`tool_slot`
semantics (`model.rs:838-846`) move into L2's index — the four join/order golden tests are
the gate. *Resolves §6.1 toward the message log (fine-grained), because L1 emitting messages
(not blocks) is what makes incremental replay + the append-only-log contract possible.*

**Phase 2 — Codex onto the same L1/L2.** *(Landed.)* Codex adapter = an L1 tokenizer
(`codex_model::tokenize`) for Codex's `response_item` shapes; L2 (`replay`) is shared. The
three agent-specific differences are captured by a `Shaping` seam — the embryo of the
`Adapter` (§3.2): `apply` (result back-patch), `keep_orphan` (Claude drops boilerplate,
Codex keeps all), and `finish` (Claude groups + coalesces, Codex is identity). Gate:
`codex_replay_matches_parse_lines` (bit-identical) + the Codex golden tests. The in-memory
entry now runs on the shared engine; the streaming `parse_lines` stays until L1 grows a pull
iterator (so its duplicated slot/pending loop is retired then, not yet).

**Phase 3 — Layer 3 becomes thin presenters over `Session`; unify classification.** Repoint
`render.rs`, `--dump`, and `html_export`'s Emitter to consume `Session` (they nearly do —
all read `blocks`). Introduce one `BlockKind`/`tool_kind` that `fold_key`, `html_kind`, and
render's inline arms derive from, and **one** hunk-numberer shared by `render::render_patch`
and `html_export::diff_part` — this is the single **intentional** output change (fixes the
TUI Edit diff-numbering bug). Gate: render + html diff tests, dump sweep (with the diff-fix
delta acknowledged).

**Phase 4 — `Session` as the public shape + fold metrics in.** L2 folds metrics as it
builds (kills the separate `metrics::parse_reader_for` pass and the 2nd/3rd `File::open` in
`app.rs`/`html_export`). Add `parse_session(path) -> Session` (§3). Repoint the TUI
`build_frame` and `html_export::snapshot`/`agent_stream` at it. **Re-measure RSS.** Gate:
metrics + HTML + `parse_path_timed_for` callers; dump sweep.

**Phase 5 — the `SessionIndex` (§7), derived-view first.** Build `agents`/`tools`/
`attachments` in L2, on `Session.index`. Repoint `active_agent_indices` + the `a` popup at
`index.active_agents()` (drop the block-scan). Additive, no block-model change. Gate: the
popup / `a active N` tests.

**Phase 6 — Layer 1 incremental (byte-offset resume + mutation-safe append-only log).**
Promote L1 to resume from a byte offset and emit a `reset` on a kept-tail mismatch (§8.3.2).
The batch path still works; incremental is additive and only exercised by the live paths.
Gate: a new L1 test (append → resume identical; mutate tail → realign + `reset`).

**Phase 7 — Layer 2 incremental (`engine::ingest`) — the live-CPU fix.** Add
`ingest(&mut Session, delta_messages)`: forward-fold the delta, back-patch via the index,
rewind only on `reset`. Route **both** the TUI live tail and the HTML `follow_tree`/
`agent_stream` through it, replacing today's whole-file re-parse and the TUI's ad-hoc
`view::ingest`. This retires the §8.3 stop-gap (skip-if-unchanged) — now the tailer folds
only the tail. Gate: tmux live-tail e2e (unchanged behavior) + a CPU probe on the 47 MB
session (must drop vs today) + byte-identical stream output vs a full re-parse.

**Phase 8 — the `SessionStore` + tiers re-home the store-shaped code (§8).** Fold
`html_export::Live` (registry, lazy generation, tailer, `/stream` cursor) and the TUI
`Frame` stack into `engine::store` (`see`/`load`/`evict`, the three tiers). The served HTML
becomes a thin serving layer over the store; `<id>.jsonl` becomes tier (b) explicitly. No
user-visible change — same URLs, same records, same lazy behavior. Gate: served smoke +
descend/ascend e2e + the store test (load → grow → fast-forward vs evict → reload).

**Phase 9 — library surface + optional crate split.** Document `parse_session`/`Session`/
`ParseOptions`; add `examples/parse.rs`. `agent-jdi` switches to the library `Session`
(no behavior change). Optionally lift `engine/` into a `claude-replay-core` crate (no
ratatui/syntect/clap deps → mechanical). Defer the split until an external consumer wants
it.

**Optional, any time after Phase 5:** `SubAgentLoad::Summary` lazy sub-agents (§4.3);
lifting agent metadata fully into the index (`Block::AgentSpawn{id}`, §7.2) once a second
consumer needs it.

Ordering rationale: Phases 0–4 are the *pure re-architecture* (behavior frozen, dump sweep
is the proof); Phases 6–8 are the *incremental/live* wins (they change internals + CPU, not
output); Phase 9 is *exposure*. A reader can stop after Phase 4 and already have the unified
engine with byte-identical output; 6–8 are what make the live surfaces cheap.

---

## 6. Open questions / tradeoffs

1. **~~`Step` granularity: coarse (Blocks) vs fine (semantic events).~~ RESOLVED — the
   L1↔L2 boundary is the fine-grained canonical *message log*, not blocks** (§0, §5.2
   Phase 1). The three-layer model forces this: L1 must emit messages (not built blocks)
   for incremental replay + the append-only-log-with-reset contract to work, and to keep
   L2 (the fold: back-patch/grouping) agent-agnostic. So the adapter (L1) does line-shape
   tokenization only; the Claude-only shaping quirks — skill-body nesting
   (`attach_skill_body`, `model.rs:247`), queue enqueue/dequeue suppression
   (`model.rs:1214-1228`), the two-event spawn/completion split — move to **L2's fold**,
   which is where the `content_seq`/`marker_idx` loop state naturally lives (it folds
   forward with exactly that state). Open sub-question deferred to build time: the precise
   `Message` vocabulary — how much stays a raw-ish "one message per interesting line" vs a
   few synthetic helper messages (turn-boundary, metrics-delta) that make L2 cheaper.

2. **Metrics fold: pass 1 or pass 2?** Pass 2 (proposed) reuses the already-decoded
   `Value`. Pass 1 is id-only today and cheaper to keep that way. No strong reason
   for pass 1 — but if a future one-pass parse ever lands (buffering results instead
   of pre-scanning), metrics should ride whichever pass survives.

3. **`Summary` sub-agent load — is a tool count cheap without a full parse?**
   `agent_chip` needs the child's tool count (`render.rs:30`). If that count can come
   from the pass-1 `tool_use_id` scan (just `count()` the ids), `Summary` is genuinely
   cheap; if it needs `steps` (to fold coalesced activity tools), `Summary` isn't much
   cheaper than `Eager` and may not be worth the complexity. **Measure before building.**

4. **Split `Transcript` vs a separate `Discover` trait.** `detect`/`session_cwd` are
   discovery, not parsing; the rest of `discover.rs` (candidate listing, id resolution,
   `latest_for_cwd`) is already deeply per-agent (`discover.rs` vs `codex_discover.rs`)
   and overlaps the JDI adapter's discovery methods (`jdi/agent.rs:166,184`). Worth
   asking whether transcript-discovery should unify with `jdi::AgentAdapter`'s
   discovery hooks rather than growing a parallel trait. Out of scope here, but the
   `Transcript` trait should not preclude it.

5. **Separate crate now or later?** The backlog says "a `claude-replay-core` crate,
   or a stable `pub` API on the existing lib" (`DESIGN.md:507`). The `pub mod engine`
   path ships the deliverable with the least churn; the crate split is a clean but
   mechanical follow-up worth doing only when an external consumer appears (it also
   forces `Agent`/`Args` to stop being a shared clap type — `parse_main` already
   ignores `Args`, so `ParseOptions` is the clean replacement).

6. **Codex sub-agents.** Codex *has* sub-agents (`codex_discover.rs:140`) but they're
   never enriched today. The `subagent_child` hook makes wiring them possible, but the
   on-disk layout differs from Claude's flat `subagents/` dir — needs its own
   investigation before claiming Codex drill-down works. Not a blocker for the
   refactor; the hook just stops precluding it.

7. **Index as derived-view vs canonical (§7.2).** The low-churn fallback keeps
   `SubAgent` inline and treats `SessionIndex` as a rebuilt-on-ingest mirror; the
   canonical form (`Block::AgentSpawn{id}`, metadata in the entry) is cleaner but
   changes the block model and every renderer signature (`&[Block]` → `&Session`).
   **Recommend derived-view first.** Open sub-question: does any consumer need to
   *mutate* an agent's state through the index (vs read-only jump/filter)? If not, the
   derived view may be permanent.

8. **Incremental index/metrics update on ingest (§8.3).** Appending blocks is easy;
   keeping `slots`/`pending` (results whose tool_use was in an earlier batch) and the
   metrics accumulator alive on the `Session` across ingest calls is the real work —
   `view::ingest` today rebuilds from scratch per batch for small tails. Measure whether
   persisting parser state is worth it, or whether re-deriving the index from the full
   (already-resident) `blocks` after each append is simpler and fast enough.

9. **Residency budget units (§8.4).** Max-sessions is trivial but a poor proxy for
   memory (a 300 MB root vs a 2 KB leaf). Max-bytes needs a `Session` size estimate;
   is `blocks.len() × const` good enough, or does a real `approx_bytes()` (summing
   string lengths) earn its cost? And should the budget be global or per-root-tree?

10. **Eviction vs live tail.** Evicting a session that is being actively tailed by
    another surface (a served HTML page polling while the TUI views a sibling) drops its
    `consumed` offset and forces a re-parse mid-stream. The store must pin any id with a
    live follower, not just the currently-viewed one — who owns that follower registry,
    the store or the caller?

11. **Tier-(b) representation (§8.2) — the load-bearing one.** Tier (b) is "parsed and
    on disk." Three forms: (i) the **HTML render stream** `<id>.jsonl` — free, since the
    server writes it anyway, but display-oriented and likely lossy for reconstructing a
    full `Session`, so (b)→(a) for the TUI would still re-parse the source; (ii) a
    **serialized `Session`** (bincode/JSON of the block model + index) — a faithful,
    cheap rehydrate for *both* surfaces, but a second on-disk artifact to write and
    version; (iii) **both** (stream for HTML, serialized Session for the engine). Since
    the whole appeal of (b) is "keep it forever, cheap," (iii) may be worth the disk;
    but if the TUI rarely reopens an evicted session, (i) alone (accept a source
    re-parse on cold TUI reload) is simplest. **Decide before building §8** — it sets
    what `evict` writes and what `load` reads.

---

## 7. Per-session indices (fast filter/jump + the liveness truth)

> **Implementation status.** `engine::SessionIndex` has *landed* (§5.2 Phase 5) as the
> **derived-view-first** cut: one scan over a `Session`'s **top-level** blocks builds
> `turns` / `agents` / `tools` / `attachments`, so entry positions are flat `usize` indices
> into `Session.blocks` (not the tree-addressing `BlockPath` below). `BlockPath` — needed
> only to point into a sub-agent's *own* blocks (§7.3) — is a later refinement; the
> single-session index needs nothing more than a position. It is built by a post-parse scan
> (additive, byte-identical), not yet folded into pass 2 as §4.2 envisions.

Alongside `blocks`, the engine builds a small set of **derived indices** during the
same pass 2 (`§4.2` style — no extra read): every sub-agent, tool use, and attachment
mentioned in the session, each entry carrying its metadata + a back-pointer to the
block it lives at. Two jobs:

1. **Fast within-session navigation** — filter ("show only Edits", "jump to the next
   attachment") and jump-to without re-walking `blocks`.
2. **The agent index is the single source of truth for liveness** — which sub-agents
   are *active*. Today "active" is recomputed by scanning `blocks` for non-terminal
   `SubAgent`s (`view::active_agent_indices`), and status lives inline on the spawn
   block — the exact shape that produced the "running agent invisible to `a active N`"
   bug (a spawn and its later completion notification are two blocks that must agree).
   Folding agent identity+status into one indexed entry, which *both* the spawn step
   and the completion notification update **by id**, makes that class of bug
   structural-impossible.

```rust
// engine/index.rs

/// A block's location within a session's tree: the path of child indices from the
/// root block list (len 1 = a top-level block; deeper = inside a SubAgent's blocks).
/// A plain `usize` suffices for a single node; the path composes across the tree (§7.3).
/// Per §4.4 the index is *content-free*: a `BlockPath` resolves (via the store's per-block
/// `id → byte offset` map) to the block record in tier (b), loaded on-demand — so the
/// whole `SessionIndex` stays O(index), never holding block content.
pub type BlockPath = smallvec::SmallVec<[u32; 4]>;

/// Every sub-agent spawned FROM this session node, in spawn order. Canonical for the
/// agent's identity, liveness, and (lazily) its child transcript — blocks only carry
/// the id and resolve through here.
pub struct AgentEntry {
    pub id: String,                 // "aXXXX" join id (the key blocks point with)
    pub agent_type: String,
    pub description: String,
    pub prompt: String,
    pub status: AgentStatus,        // Running/AsyncLaunched/Completed/… — the liveness truth
    pub spawn_at: BlockPath,        // the spawn block (jump target)
    pub mentions: Vec<BlockPath>,   // every block referencing this id (spawn + completion note)
    pub output_file: Option<String>,
    pub child: ChildSlot,           // Loaded | Summary{tools,cost} | NotLoaded{path} (§4.3, §8)
    pub subtree_cost: Option<f64>,
}

pub struct ToolEntry {
    pub kind: BlockKind,            // the ONE classification (§5 / step 5) — read/edit/write/bash/…
    pub name: String,               // "Edit", "Bash", …
    pub target: String,             // file path / command (already relativized)
    pub at: BlockPath,
    pub ok: Option<bool>,           // result success, once back-patched (None while pending)
}

pub struct AttachmentEntry {
    pub kind: AttachmentKind,       // file / image / plan-ref / … (engine::block)
    pub name: String,
    pub at: BlockPath,
    pub downloadable: bool,         // has inline content vs path-only (reveal)
}

/// One human/user turn boundary. This is where today's parallel `user_times`
/// (`Vec<Option<f64>>`, model.rs) re-homes: the per-turn timestamp lives ON the turn, so
/// a presenter never has to zip a side channel against the block list (§3.3).
pub struct TurnEntry {
    pub at: BlockPath,              // the User/Command block that opens the turn
    pub time: Option<Time>,         // wall-clock of the event that produced it
}

/// Derived, extensible — a new axis (commands, errors, thinking turns) is one more Vec.
pub struct SessionIndex {
    pub turns: Vec<TurnEntry>,      // user-turn spans + times (replaces `user_times`)
    pub agents: Vec<AgentEntry>,
    pub tools: Vec<ToolEntry>,
    pub attachments: Vec<AttachmentEntry>,
    // agent_by_id: HashMap<String, usize>  — O(1) id → agents[] slot (built at finish)
}

impl SessionIndex {
    pub fn agent(&self, id: &str) -> Option<&AgentEntry>;
    pub fn active_agents(&self) -> impl Iterator<Item = &AgentEntry>;  // status not terminal
    pub fn tools_of_kind(&self, k: BlockKind) -> impl Iterator<Item = &ToolEntry>;
    /// Tool names by descending frequency — the auditor primitive (§3.5, example 1).
    pub fn tools_by_count(&self) -> impl Iterator<Item = ToolCount>;
    /// The next indexed entry at/after `from` matching a predicate — the "jump" primitive
    /// the TUI's `[`/`]` and a future `f`ilter mode share.
    pub fn next_from(&self, from: BlockPath, pred: impl Fn(&Entry) -> bool) -> Option<BlockPath>;
}
```

### 7.1 Built in the one pass, updated by id

The core already routes every `Step` (§2.3). Index-building rides that routing, so
it costs no extra read and stays in lockstep with `blocks`:

- `ToolUse{id, block}` → push a `ToolEntry` (kind from `tool_kind(name)`), record its
  `slots` index; a later `ToolResult` sets `ok` when it back-patches.
- a sub-agent spawn `Step` → upsert `AgentEntry` by id (first mention sets it, records
  `spawn_at`); every block that names the id appends to `mentions`.
- `Completion(id, status)` (applied after the loop, §2.3) → look the entry up **by id**
  and set its terminal `status` — the same entry the spawn created. No block re-scan.
- `Emit(Attachment)` → push an `AttachmentEntry`.

`finish` builds `agent_by_id` and rolls up `subtree_cost` from each child's
`Session.metrics` (§4.3). The index is regenerated wholesale by a batch parse and
updated incrementally by the live-tail ingest (§8.3).

### 7.2 Blocks point INTO the index (the block model shrinks)

The recommended end state: `Block::SubAgent`'s heavy fields (type/description/prompt/
status/blocks/cost) move to `AgentEntry`; the block becomes a thin
`Block::AgentSpawn { id: String }` (plus the completion note as
`Block::AgentNote { id, .. }`). Renderers resolve through `session.index.agent(id)`.
This is a **signature change** — `render.rs`/`html_export.rs` currently take `&[Block]`
and read `SubAgent` inline; they'd take `&Session` (or `&SessionIndex`) too. Payoff:
one liveness truth, spawn+completion naturally unified, and the `a` popup / `a active N`
footer read straight off `index.active_agents()`.

Lower-churn fallback if that change is too broad for one pass: keep `SubAgent` inline
and make `SessionIndex` a **derived view** whose `AgentEntry.status` is a *copy* kept
authoritative by construction (rebuilt on every ingest). Gets the fast-filter/jump and
the single active-set query, without touching the block model or the renderer
signatures — at the cost of the spawn block still storing its own status (so the two
must be rebuilt together, never mutated in isolation). **Recommend the derived-view
fallback first** (ships with the engine refactor), and lift agent metadata fully into
the index only once a second consumer needs it.

### 7.3 Composition across the sub-agent tree

Each `Session` (root or a descended child) owns the index of *its own* direct blocks;
`AgentEntry.child` holds the child `Session` (when loaded), whose `.index` covers the
grandchildren. So "active children of the current node" = this node's
`index.active_agents()` (node-scoped, matching today's semantics), while a "whole-tree"
query (e.g. every attachment anywhere below) is a recursive walk composing child
indices — the `BlockPath` prefixes with the descent path. No global flat index is
maintained eagerly; it's computed on demand from the resident subtree.

## 8. Session cache & resident manager (lazy load, fast-forward, evict)

Generalize the ad-hoc `app.rs` `Frame` stack (which keeps ancestor `View`s alive to
avoid re-parse) and the per-frame `TailReader` into one **`SessionStore`**: a cache
keyed by session id that owns residency, incremental catch-up, and eviction. This is
what lets the multi-session sub-agent world (a parent + N descended children + their
live tails) stay bounded in memory.

```rust
// engine/store.rs

pub type SessionId = String;   // Claude: the UUID stem; resolves to a path via discover

/// Three residency tiers, cheapest to keep on the right, fastest to serve on the left.
/// A session moves up on access and down under memory pressure; only tier (a) costs RAM.
enum Slot {
    /// (a) RESIDENT — parsed and in memory. The full `Session` (blocks + index +
    /// metrics), with the source byte offset consumed so far (the "virtual position")
    /// so a grown live file is fast-forwarded, not re-parsed. The only tier bounded by
    /// the memory budget.
    Resident { session: Session, consumed: u64, last_used: u64 /* LRU tick */ },
    /// (b) MATERIALIZED — parsed, evicted from RAM, but the parsed output persists on
    /// disk (`stream` = the append-only `<id>.jsonl` the HTML backend already writes,
    /// §8 of html-live-proposal) with its `consumed` offset. Disk is cheap → this tier
    /// is kept ~indefinitely (lazy GC only). Rehydrating to (a) resumes from `consumed`
    /// instead of re-reading the whole source. This is the SAME artifact the HTML server
    /// serves to clients — the two designs share one tier.
    Materialized { source: PathBuf, stream: PathBuf, consumed: u64 },
    /// (c) PATH-ONLY — not parsed; just the source transcript path. Costs one string, so
    /// the store registers EVERY agent it merely *sees* (a spawn in some parent's §7
    /// index) here, eagerly, long before anyone opens it. A load re-parses from source.
    PathOnly { source: PathBuf },
}

pub struct SessionStore {
    slots: HashMap<SessionId, Slot>,
    cache_dir: PathBuf,        // where (b) stream files live
    budget: ResidencyBudget,   // bounds tier (a) only — max resident sessions and/or bytes
    tick: u64,
}

impl SessionStore {
    /// Register a session id ↔ source path at tier (c) without parsing. Discovery and
    /// every §7 agent-index entry the store observes call this — cheap, so the store
    /// knows about all agents in a tree the moment their parents mention them.
    pub fn see(&mut self, id: SessionId, source: PathBuf);

    /// Get the parsed session, promoting up the tiers as needed:
    ///  · PathOnly      → full `parse_session` from source            → Resident (+ write (b)).
    ///  · Materialized  → rehydrate: resume from `consumed`, catching  → Resident.
    ///                    up only the source tail since it was written.
    ///  · Resident, grew→ fast-forward: ingest bytes `[consumed..len)` (§8.3).
    ///  · Resident, same→ as-is.
    /// Then enforce the budget (may demote *other* LRU residents to (b)). Pins this id.
    pub fn load(&mut self, id: &SessionId) -> std::io::Result<&Session>;

    /// Demote (a)→(b): drop the in-memory `Session`, keep the on-disk stream + offset.
    /// Called by the budget sweep and by a caller reacting to external memory pressure.
    /// (b)→(c) is a separate, rare lazy-GC step (disk reclaim), never automatic.
    pub fn evict(&mut self, id: &SessionId);

    /// Current resident (tier a) set — for a status line / debugging.
    pub fn resident(&self) -> impl Iterator<Item = (&SessionId, &Session)>;
}
```

The tiers and their transition costs:

| Tier | Holds | Cost to keep | Promote to (a) |
|---|---|---|---|
| (a) Resident | `Session` in RAM | memory (budgeted) | — |
| (b) Materialized | `<id>.jsonl` + offset on disk | disk (cheap; ~kept forever) | resume from `consumed` |
| (c) Path-only | one `PathBuf` string | ~nothing | full re-parse from source |

`see` (c) is the eager, free registration; `load` promotes; `evict` demotes (a)→(b).
The memory budget bounds **only tier (a)**, so pressure never loses parsed work — it
just spills to the cheap disk tier that the HTML server is already maintaining anyway.

### 8.1 Load-by-id is the whole point

A caller (the TUI descending into a sub-agent, an HTTP handler serving a live page,
a third-party tool) never manages files — it asks the store for a `SessionId`:
- **descend** → `store.load(child_id)` (the child id comes from the `AgentEntry`, §7);
- **ascend** → the parent id is still resident (or reloads transparently);
- **switch** → `store.load(other_session_id)`.

The `Frame` stack collapses to a `Vec<SessionId>` (the descent breadcrumb); the store
owns the `Session`s. Because descend goes through `load`, the running-agent lazy-load
we just hand-rolled in `build_child_frame` (parse the child file fresh when `.blocks`
is empty) becomes the store's default behavior for free. And because every §7
`AgentEntry` the store observes is `see`n at tier (c), descending into an agent nobody
opened yet is always a valid `load` — the path is already registered.

### 8.2 Fast-forward, rehydrate, re-parse (the three promotion costs)

The "virtual position" is `consumed: u64` — the source byte offset already parsed
(exactly what `tail.rs` tracks). The three ways a `load` reaches tier (a) differ only in
how much source they must read:

- **(a) resident, file grew → fast-forward.** Ingest only the new tail bytes
  `[consumed..len)` through the incremental path (§8.3), preserving fold/scroll/index.
  Cheapest; what the live TUI/HTML poll does every cycle.
- **(b) materialized → rehydrate.** The parsed output already sits on disk as
  `<id>.jsonl`; promotion resumes from the stored `consumed` and catches up only the
  source tail written since — never re-reading the consumed prefix. This is why tier (b)
  earns keeping forever: an evicted-then-reopened session (a sub-agent you descended,
  ascended from, and came back to) costs a tail catch-up, not a full re-parse.
- **(c) path-only → re-parse.** No parsed state exists; `parse_session` reads the whole
  source. The floor cost, paid once per session the first time it's ever opened.

Whether (b)→(a) rehydration reconstructs the `Session` by **replaying the `<id>.jsonl`
stream** or by **re-parsing the source up to `consumed`** is the load-bearing open
question (§6 Q11): the stream is the HTML *render* form (display-oriented, possibly
lossy for the full `Block` model), so a faithful `Session` may still need the source —
in which case (b)'s win is the resume-offset for *live* catch-up + serving HTML, not a
cheaper cold TUI reload. Resolving this decides whether (b) stores the render stream
only, a serialized `Session`, or both.

### 8.3 Incremental ingest = the engine's batch path applied as a delta

Fast-forward needs `Engine` to append to an existing `Session`, not just produce a
fresh one. This already half-exists: the TUI live tail calls `view::ingest` with a
batch of new lines (`Engine::run_str`, §2.3). Promote it to
`engine::ingest(&mut Session, new_lines)` that: parses the delta lines into `Step`s,
appends/back-patches `blocks` (its `slots`/`pending` must persist on the `Session` for
a result whose tool_use arrived in an earlier batch), folds metrics into
`Session.metrics`, and **updates the indices incrementally** (push new tool/attachment
entries; upsert agent status by id — a completion note in the delta flips a running
agent to terminal, and `active_agents()` reflects it on the very next frame). This is
the mechanism the `a active N` footer needs to update live without a re-parse.

### 8.3.1 Bounded-tail re-parse — the cheap, correct incremental tail (deferred, to refine here)

Status: **deferred to this refactor.** A rough but promising idea captured now; refine it
when the engine work starts. It's the fix for the live server's remaining CPU cost.
**Superseded by §8.3.2** (the two-stage parse-to-log + replay-to-state architecture) —
§8.3.1 is kept as the motivating measurements + the minimal tactic it generalizes.

**The problem it solves.** The served HTML tailer (`html_export::agent_stream`) and any
full-file live tail re-parse the WHOLE source on every change. For a large live session
(a measured 47 MB transcript) that pins a core. The shipped stop-gap only *skips* the
parse when the source byte length is unchanged (Claude's JSONL is **append-only** —
empirically verified: appending 30 KB left the first 47.8 MB byte-identical), so an idle
session costs ~nothing — but each *real* append still triggers a full re-parse.

**The key measurement (why a full resumable parser is overkill).** On that 47 MB session
(19,546 lines, 3,693 tool pairs):
- **0** tool results appear before their `tool_use` — the `pending` (out-of-order) path
  is never exercised in practice.
- `tool_use → result` line distance: **median 1, p95 7, max 9** — the join is strictly
  *local*. The `tool_slot` map spans the file only in theory.

So the only genuinely cross-block work is the **tail grouping** — `group_turns` /
`coalesce_activity_runs` (thinking absorbing its preceding activity run), the queue
enqueue/dequeue markers, and attachments — and all of it lives **within the current,
in-progress turn**; nothing reaches back past a *completed* turn.

**The design.** Re-parse only the **unstable tail**, not the file:
- Track the byte offset of the **last turn boundary** — the last top-level `user` message
  (not a `tool_result`), i.e. the start of the current turn. Everything before it is
  stable: all tools joined, all grouping settled, append-only guarantees it never changes.
- On a change, re-parse **`[last_turn_boundary .. EOF]`** — the current turn plus new
  lines. Cost is bounded by *turn size* (KB), not file size.
- Diff those tail blocks against what was already emitted from that boundary; emit
  `{t:"reset", from:<boundary block index>}` + the new tail. The stable prefix is untouched.

**Edge cases to handle:**
- A **sub-agent spawned in an earlier turn** whose completion `<task-notification>` lands
  in the current one: the `AgentDone` needs the spawn's `agent_type`/description, which is
  now outside the re-parse window. Keep a tiny `id → (type, description)` map of spawns
  seen — cheap, and it *is* the §7 agent index / the live server's level-3 registry.
- **Compaction** rewrites the whole file (length shrinks) → fall back to a full re-parse
  (the byte-length guard already detects this).
- Choosing the boundary conservatively: a *turn* boundary is provably safe for grouping;
  if a turn is pathologically huge, cap the window and accept a larger (still bounded)
  re-parse. Validate the whole thing by asserting the incremental stream output is
  **byte-identical** to a full re-parse on real transcripts before trusting it.

**Where it plugs in.** This is the concrete, low-risk form of `engine::ingest` (§8.3): the
parser itself doesn't change — the tailer feeds it a tail *slice* and splices the result.
It supersedes the "must persist `slots`/`pending` across batches" worry above: with a
per-turn re-parse, the join state is rebuilt within the window, so no cross-batch parser
state needs to survive. Refine the exact boundary rule + the spawn-map here when built.

### 8.3.2 The two-stage refinement: parse-to-log + replay-to-state (the direction to build)

§8.3.1 is a tactic ("re-parse the current turn"); this is the architecture it wants to be.
Split the pipeline into two independent stages with an **append-only log** as the contract
between them — event sourcing for the transcript.

**Stage 1 — Parser: raw JSONL → a canonical *message log*.** Maps each raw line to zero or
more **canonical messages** — the events we actually care about (user text, assistant
text, thinking, tool-call, tool-result, attachment, spawn, completion-notification, …).
This is cheap, near-stateless tokenization: **no back-patching, no grouping, no joins** —
it only reduces the raw JSONL to the message stream, and may inject **helper messages**
that make Stage 2 cheaper (e.g. a `turn-boundary` marker so replay knows where the stable
prefix ends, or a pre-summed metrics delta).
- **Incremental by byte offset.** The parser remembers the raw byte offset up to which it
  has emitted messages; on resume it reads only from there. The one expensive thing
  (serde_json over the 47 MB file) happens once, then only over new bytes.
- **Mutation-safe *without assuming append-only*.** It also retains the **last few raw
  messages** it consumed. On resume it re-reads from the remembered offset and compares:
  identical → pure append, keep going; **differ** (a rewrite / compaction / any silent
  mutation) → walk **back** a few messages to find the alignment point, **emit
  `reset(to = <message index>)`** into the log, then re-parse forward from the aligned
  offset. This replaces §8.3.1's append-only assumption + compaction special-case with one
  general mechanism.
- **The log is strictly append-only** (appends + `reset` markers). That is the load-bearing
  contract: every consumer — the replayer, the HTML `<id>.jsonl` stream, the §7 indices —
  only ever sees "append these messages" / "discard back to N". The HTML stream's existing
  `{t:"reset",from:N}` **is** this `reset`, surfaced verbatim; the `<id>.jsonl` file may
  simply *be* the on-disk log (tier (b)).

**Stage 2 — Replayer: message log → in-memory `Session`, a *strictly forward* fold.** A
deterministic reducer that builds the presentation state, maintaining `blocks` + a live
`id → block ref` index + the current turn's grouping context. Each message is applied
**forward, once**:
- append → push a block (and index it if it is a tool-call / spawn);
- tool-result / completion-notification → **patch the referenced block in place via the id
  index** — O(1), and crucially **never re-winds or re-examines already-ingested
  messages**. Back-patching is a forward-only point update, not a re-scan (this is why the
  measured-local join distance doesn't even matter to correctness here — the index holds
  the ref regardless of distance);
- grouping / activity-coalescing → operate on the *current tail* context only.

The **only** rewind in the whole system is an explicit `reset(to=N)` from Stage 1 (a
detected mutation): discard state after message N and resume folding. Normal operation —
including all back-patching — is append-and-patch, no rewind, no re-scan.

**Why this is the right shape.**
- The costly raw-JSONL parse runs **once, incrementally** (Stage 1, byte-offset resume).
- **Append-only-with-reset is one clean contract** for every consumer; the HTML server,
  the TUI live tail, and the index-builders all collapse into the same "replay a log" fold.
- **Mutation safety is explicit and general** (the kept-raw-messages compare), not an
  assumption — truncation, compaction, or an out-of-band edit all resolve to a `reset`.
- **Separately testable**: Stage 1 (bytes → log) and Stage 2 (log → state) are pure and
  golden-testable in isolation; the whole is validated by asserting incremental-parse +
  replay is identical to a from-scratch full parse on real transcripts.

**Mapping onto §2's engine.** Stage 1 ≈ `Transcript::steps` promoted to a resumable,
offset-aware, mutation-checking *message* emitter (the adapter still owns per-agent line
shapes; the canonical message is a leaner `Step`). Stage 2 ≈ today's `parse_main`
back-patch loop + `finish` grouping, but driven by the log (not raw lines) and folded
forward with an id index. `engine::ingest` (§8.3) becomes "hand Stage 2 the new tail of
the log."

**Open refinements (when we build it):** the exact set of canonical message kinds + which
helper messages earn their keep; how many raw messages to retain for the realign window;
whether Stage 2 keeps fully-rewindable state or only the current-turn tail (a `reset` never
reaches past the last stable turn in practice); and confirming the on-disk log form (likely
the HTML `<id>.jsonl` itself).

### 8.4 Memory pressure / eviction policy

Eviction demotes **(a)→(b)** — drop the RAM `Session`, keep the on-disk stream + offset
— so it never loses parsed work, only reclaims memory into the cheap disk tier the HTML
backend already maintains. It never touches (b) or (c) automatically; reclaiming *disk*
((b)→(c), deleting a stream file) is a rare, explicit lazy-GC step, justified only by
disk pressure, which "disk is cheap" says is essentially never.

Portable "system memory pressure" has no clean cross-platform Rust signal, so the
store drives (a)-eviction off a **configurable `ResidencyBudget`** (max resident
sessions and/or max resident bytes, estimated from `blocks.len()` × a per-block guess
or a cheap `Session::approx_bytes()`), demoting least-recently-used residents on each
`load` that exceeds it. The *currently-viewed* id, its ancestor breadcrumb, and any id
with a live follower (an HTML client polling it, §10-adjacent) are pinned. A caller that
*does* have a platform pressure signal (a macOS `DISPATCH_SOURCE_MEMORYPRESSURE` bridge,
or an RSS high-water check) can call `evict`/lower the budget directly — the store
mandates the mechanism, not the source. Default budget stays generous enough that
today's single-root + shallow-descent usage never demotes; the policy earns its keep
only for wide/deep sub-agent trees and long-lived server processes — exactly where the
three tiers pay off (thousands of `see`n agents at (c) for free, a bounded resident set
at (a), everything ever opened durable at (b)).

## Recommendation (summary)

Adopt the **three-layer session engine** (§0): an agent-specific **Layer 1** raw parser
(transcript bytes → a canonical, append-only **message log**; incremental by byte offset,
mutation-safe via a kept-tail compare + `reset`), an agent-agnostic **Layer 2** replay /
state builder (message log → `Session` by a strictly-forward fold: `id → block ref`
back-patch, grouping, **metrics folded in**, the §7 indices), and thin per-surface **Layer
3** presenters over the one `Session` (`render.rs`, `--dump`, the `html_export` record
stream; the live-HTML client is the only piece outside the binary, a dumb DOM renderer).
The boundaries are the reuse boundaries: a new **agent** = one Layer-1 tokenizer; a new
**surface** = one Layer-3 presenter.

`parse_main`/`parse_lines` (parse+back-patch+group fused today) split into L1+L2;
`metrics::parse_reader_for`'s separate pass folds into L2; `render`/`dump`/`html_export`
become L3 formatters over `Session`, sharing **one** classification + diff-numberer
(closing the TUI diff bug). The multi-session machinery becomes *instances* of L1/L2: the
**`SessionIndex`** (§7 — the liveness truth + fast filter/jump), the **incremental
ingest** (§8.3.2 — L1 resume + L2 forward-fold, the live-CPU fix that retires the
skip-if-unchanged stop-gap), and the tiered **`SessionStore`** (§8) that the HTML `Live`
server + `/stream` cursor + lazy generation + the TUI `Frame` stack all collapse onto.

**Ship it as a behavior-preserving migration (§5), not a rewrite.** The prime directive is
**zero user impact**: byte-identical `--dump*`/HTML/TUI output and an unchanged `<id>.jsonl`
stream format at every phase (sole intentional exception: the diff-numbering fix), enforced
by the golden tests + a dump-equivalence corpus sweep + the tmux/browser smokes + an RSS
probe. Phases 0–4 are the frozen-behavior re-architecture (stop here and you already have
the unified engine); 6–8 are the incremental/live CPU wins; 9 exposes the library. Highest
risk is Phase 1's bit-for-bit L1+L2 = `parse_main` equivalence — proven against the corpus
before the old path is deleted. Keep the two-read streaming invariant; live CPU only
improves.

On top of that skeleton, build two capabilities the multi-session sub-agent world
needs (§7–§8): a **`SessionIndex`** derived in the same pass — agents, tools,
attachments, each with a block back-pointer — that makes the *agent index the single
source of truth for liveness* (`active_agents()`), turning the "running agent invisible
to `a active N`" class of bug structural-impossible, and giving fast within-session
filter/jump; and a **`SessionStore`** that keys sessions by id, lazy-loads on
`load(id)`, **fast-forwards a resident session** by ingesting only new tail bytes
(preserving fold/scroll/index) while re-parsing an evicted one from scratch, and evicts
LRU residents under a configurable budget (pinning the viewed id + any live follower).
The store subsumes the `app.rs` `Frame` stack and per-frame `TailReader`, and makes the
lazy child-load we hand-rolled in `build_child_frame` the default. Both ship as
additive, separately-gated steps (8–9) after the core refactor; the index starts as a
rebuilt-on-ingest *derived view* (no block-model change), the store stays optional until
wide/deep trees or a long-lived server process make the memory ceiling bite.
