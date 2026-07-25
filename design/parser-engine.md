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
/// the session cwd, and which agent produced it — from ONE streaming parse.
pub struct Session {
    pub blocks: Vec<Block>,
    pub metrics: Metrics,
    pub user_times: Vec<Option<f64>>, // one per UserText/Command turn, in order
    pub cwd: Option<PathBuf>,
    pub agent: Agent,
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

---

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
Migrate in seven green steps, highest risk being the bit-for-bit back-patch semantics
in Step 1 — guarded by the existing golden tests and the `--dump` equivalence sweep.
Keep the two-read streaming invariant intact and re-measure RSS at Step 3.
