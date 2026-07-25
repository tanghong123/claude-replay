# Design: unify the parse backend + expose a reusable engine

Status: **proposal** (no code yet). Source: the DESIGN.md backlog item "Unify the
parse backend + make it a reusable engine" (`DESIGN.md:501`). Goal: an
agent-agnostic **core** (block model + streaming pipeline + fold policy +
sub-agent tree + metrics) with a thin **adapter** per agent, consumed by BOTH
in-repo surfaces (TUI/`--dump` and HTML) and exposable as a standalone library.

Guiding constraint: **preserve the streaming parse** (one JSONL line resident at a
time; the 298 MB session must stay ~811 MB, not balloon to ~2 GB — see
`STREAMING-PARSE-DESIGN.md`). This refactor must be byte-identical on output and
must not regress peak RSS.

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

The library goal: a caller gets the block/session model without the TUI, HTML,
syntect, or clap layers. Nothing in `engine/` depends on ratatui/syntect/clap
(the block model already doesn't — that's all in `render`/`view`).

```rust
// engine/mod.rs — the documented public surface

/// A fully-parsed session: the block tree, folded metrics, per-turn timestamps,
/// the session cwd, which agent produced it, and the derived within-session indices
/// (agents/tools/attachments) — all from ONE streaming parse.
pub struct Session {
    pub blocks: Vec<Block>,
    pub metrics: Metrics,
    pub user_times: Vec<Option<f64>>, // one per UserText/Command turn, in order
    pub cwd: Option<PathBuf>,
    pub agent: Agent,
    pub index: SessionIndex,          // §7 — fast filter/jump + the agent-liveness truth
}

/// Knobs a caller may set; today parse ignores `Args` (parse_main's `_args`), so
/// this is deliberately tiny. Fold flags are a VIEW concern and stay out of here.
#[derive(Default, Clone)]
pub struct ParseOptions {
    pub subagents: SubAgentLoad, // Eager (default, today's behavior) | Summary | Skip
}

pub enum SubAgentLoad { Eager, Summary, Skip }

/// Auto-detect the agent from the file head, then parse. The one call most callers
/// want. Streaming; sub-agent tree resolved per `opts.subagents`.
pub fn parse_session(path: &Path) -> std::io::Result<Session>;
pub fn parse_session_with(path: &Path, opts: &ParseOptions) -> std::io::Result<Session>;

/// Parse for a KNOWN agent (skips detection) — e.g. a caller that already sniffed.
pub fn parse_session_as(agent: Agent, path: &Path, opts: &ParseOptions)
    -> std::io::Result<Session>;

/// In-memory batch (no file, no sub-agent enrich) — the live-tail / test entry.
/// Replaces `model::parse` / `model::parse_for` (model.rs:601,607).
pub fn parse_batch(agent: Agent, jsonl: &str) -> Vec<Block>;

/// Detect the agent from a transcript's head (replaces discover::detect_agent).
pub fn detect_agent(path: &Path) -> Agent;
```

Sub-agent tree exposure (unchanged shape): the tree stays inline in
`Block::SubAgent.blocks` (`model.rs:139`), resolved recursively against the flat
`<session>/subagents/` dir. `SubAgentLoad` controls *how much* is materialized
(§4.3).

The two in-repo renderers become thin formatters over `Session`:
- **`render.rs`** already takes `&[Block]`; unchanged. It reads `engine::block::*`.
- **`html_export::snapshot`** (`html_export.rs:772`) collapses from *two* reads
  (`parse_path_timed_for` + a second metrics `File::open`) to **one**
  `engine::parse_session_as(agent, path, opts)` call, taking `blocks`,
  `user_times`, `metrics`, and `cwd` straight off the returned `Session`.
- The TUI's `build_frame` (`app.rs:199`) likewise drops its second metrics open
  (`app.rs:217-220`): `let s = engine::parse_session(path)?;` then
  `view.set_footer_segments(s.metrics.footer_segments())`.

Third-party example to ship (`examples/parse.rs`):

```rust
fn main() -> std::io::Result<()> {
    let s = claude_replay::engine::parse_session(std::path::Path::new(
        std::env::args().nth(1).unwrap().as_str(),
    ))?;
    println!("{} · {} blocks · {} turns · ~${:.2}",
        s.agent.label(), s.blocks.len(), s.user_times.len(),
        s.metrics.cost_usd.unwrap_or(0.0));
    Ok(())
}
```

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

---

## 5. Migration plan (incremental; each step compiles + `cargo test` green)

The whole point is **no big-bang rewrite**. Each step is independently shippable and
guarded by the existing golden tests: `parse_path_matches_parse_str` (`model.rs:2038`),
`result_before_tool_use_still_joins` (`model.rs:2001`),
`orphan_result_with_no_tool_use_shown_inline` (`model.rs:2022`),
`joins_tooluseresult_metadata` (`model.rs:1932`), the coalesce/group tests, and the
Codex `parse_path_matches_string…` (`codex_model.rs:485`). Plus the manual
`--dump` equivalence sweep from STREAMING-PARSE-DESIGN.md §Testing.

**Step 0 — dedupe pure helpers (no behavior change).**
Create `engine/time.rs` (`epoch_secs`) and `engine/path.rs` (`relativize*`); delete
the copies at `codex_model.rs:396,381` and fold `metrics::parse_ts` (`metrics.rs:34`)
onto `engine::time`. Risk: `parse_ts` returns `i64`, `epoch_secs` returns `f64` — add
one `as` cast, keep both call sites. Green by construction (algorithms are identical;
`relativize_uses_cwd_then_home_tilde` at `model.rs:1867` still passes).

**Step 1 — introduce the core skeleton, Claude on it, behind the same API.**
Add `engine/{mod,event,block,metrics}.rs`. Move `Block`/`SubAgent`/`Attachment`/
`fold_key` into `engine/block.rs` (re-export from `model` so nothing else moves yet).
Write `Engine::run`/`run_str` reproducing `parse_main` exactly. Write
`ClaudeTranscript` = today's `parse_main` `match` body split into
`steps`+`apply_result`+`tool_use_id`+`finish`(=group+coalesce). Repoint
`model::parse`/`parse_path` at `Engine`. **Risk: the back-patch + duplicate-id +
result-before-use semantics** (`model.rs:838-846` doc, the `pending`/`tool_slot`
dance) must be reproduced bit-for-bit — this is the highest-risk step; the four
golden tests above are the gate. Green.

**Step 2 — Codex onto the same skeleton.**
Write `CodexTranscript` from `parse_lines` (`codex_model.rs:59`); `finish` = identity
(Codex doesn't group/coalesce). Delete `parse_lines`'s duplicated slot/pending loop.
Gate: `codex_model.rs:447,485` tests. Green.

**Step 3 — fold metrics into the pass; return `Session`.**
Add `fold_metrics` to both adapters (bodies from `metrics::parse_from_lines` and
`codex_metrics::parse_codex_reader`). `Engine::run` returns `EngineOut { blocks,
metrics, user_times }`. Add `parse_session*`. Repoint `app.rs:199-222` and
`html_export::snapshot` (`html_export.rs:772`) to `Session`, deleting their second
`File::open`. Repoint `subtree_cost` to the child `Session.metrics` (drop
`model.rs:698`'s open). **Risk: `_timed` semantics** — `user_times` must stay one
entry per `UserText`/`Command`, in order (`stamp_user_turns`, `model.rs:1364`); the
Engine now always fills it (TUI ignores it). Gate: HTML export tests +
`parse_path_timed_for` callers. **Re-measure RSS** here. Green.

**Step 4 — collapse the `match agent` dispatch into the registry.**
Replace `model.rs:607,718,744` + `metrics.rs:88` with `engine::transcript(agent)`.
Move `detect_agent`/`session_cwd` arms (`discover.rs:288,311`) behind
`Transcript::detect`/`session_cwd` (or a sibling `Discover` trait). Green.

**Step 5 — one classification; kill the drifted maps + the diff-numbering bug.**
Add `engine::block::BlockKind` / `tool_kind(name)`; derive `fold_key`
(`model.rs:192`), `html_kind` (`html_export.rs:211`), and render's inline arms from
it. Extract ONE hunk-numbering function used by both `render::render_patch`
(`render.rs:349`) and `html_export::diff_part` (`html_export.rs:288`) — closing the
TUI-diff-numbering bug (`DESIGN.md:494`) as a side effect. Gate: the render + html
diff tests. Green.

**Step 6 — the library surface + example (+ optional crate split).**
Document `parse_session`/`Session`/`ParseOptions` in `lib.rs`; add `examples/parse.rs`
(§3). Optionally lift `engine/` into a `claude-replay-core` workspace crate: it has
no ratatui/syntect/clap deps, so the split is mechanical — `render`/`view`/
`html_export` depend on `core`, the CLI depends on both. Defer unless a real external
consumer wants it; the `pub mod engine` surface satisfies the backlog deliverable
either way.

**Step 7 (optional) — lazy sub-agents.**
Implement `SubAgentLoad::Summary` (§4.3) and switch the TUI to it if the load-latency
win measures out. Purely additive; `Eager` stays the default.

**Step 8 — the derived `SessionIndex` (§7).**
Build `agents`/`tools`/`attachments` during pass 2, returned on `Session.index`. Start
with the **derived-view fallback** (§7.2): `SubAgent` stays inline; the index mirrors
its status, rebuilt on every parse/ingest. Repoint `view::active_agent_indices` +
the `a` popup at `index.active_agents()` (removing the block-scan). Gate: the popup /
`a active N` tests. Green, and additive (no block-model change). Lifting agent metadata
fully into the index (`Block::AgentSpawn{id}`, §7.2) is a later, separately-gated step
taken only when a second consumer needs it.

**Step 9 (optional) — the `SessionStore` (§8).**
Introduce `engine::store` with `load`/`evict` + `engine::ingest(&mut Session, delta)`
(promoting `view::ingest`). Collapse the `app.rs` `Frame` stack to a `Vec<SessionId>`
over the store; route descend/ascend/switch/live-tail through `load`. `build_child_frame`'s
hand-rolled lazy child load falls out as the store default. Gate: the tmux descend/
ascend e2e + a new store test (load → grow file → fast-forward vs evict → reload). This
is the largest behavioral change and stays optional until the memory ceiling on
wide/deep trees (or a long-lived server) actually bites.

---

## 6. Open questions / tradeoffs

1. **`Step` granularity: coarse (Blocks) vs fine (semantic events).** The proposal
   is *coarse* — the adapter builds `Block`s and the core only does mechanism. This
   keeps agent-specific block-shaping (e.g. Claude's skill-body nesting
   `attach_skill_body` at `model.rs:247`, queue-marker suppression `model.rs:1214`)
   inside the adapter, at the cost of those Claude-only quirks not being "core". A
   *fine* event vocabulary would centralize block-shaping but bloat the enum and
   force Codex to model concepts it lacks. **Recommend coarse**; revisit only if a
   third agent shares Claude's quirks. Validate: can Claude's `suppress`/`queue`
   post-pass (`model.rs:1214-1228`) live cleanly in `finish`, or does it need loop
   state? (It uses `content_seq`/`marker_idx` tracked during the loop — may need to
   ride along in `EngineOut` or a per-adapter scratch, a wrinkle in the coarse model.)

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

/// Derived, extensible — a new axis (commands, errors, thinking turns) is one more Vec.
pub struct SessionIndex {
    pub agents: Vec<AgentEntry>,
    pub tools: Vec<ToolEntry>,
    pub attachments: Vec<AttachmentEntry>,
    // agent_by_id: HashMap<String, usize>  — O(1) id → agents[] slot (built at finish)
}

impl SessionIndex {
    pub fn agent(&self, id: &str) -> Option<&AgentEntry>;
    pub fn active_agents(&self) -> impl Iterator<Item = &AgentEntry>;  // status not terminal
    pub fn tools_of_kind(&self, k: BlockKind) -> impl Iterator<Item = &ToolEntry>;
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

Introduce an **agent-agnostic `engine`** that owns the streaming skeleton (id
pre-scan, back-patch slots/pending, timestamps, user-turn stamping, thinking
duration, sub-agent enrich, and **metrics folded into the same pass**), and a
**`Transcript` trait** (one file per agent, mirroring the existing
`jdi::AgentAdapter` precedent) whose whole job is `tool_use_id` + `steps` +
`apply_result` + `fold_metrics` + `finish`. `parse_main` and `parse_lines` — two
copies of the same loop today — collapse onto that one skeleton. Expose
`parse_session(path) -> Session { blocks, metrics, user_times, cwd, agent }` as the
public library surface; the TUI's `render.rs` and the HTML `html_export.rs` become
thin formatters over `Session`, each losing its **second file open** for metrics and
sharing **one** classification + diff-numbering (closing the current TUI diff bug).
Migrate in green steps, highest risk being the bit-for-bit back-patch semantics
in Step 1 — guarded by the existing golden tests and the `--dump` equivalence sweep.
Keep the two-read streaming invariant intact and re-measure RSS at Step 3.

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
