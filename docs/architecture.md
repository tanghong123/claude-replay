# claude-replay — Architecture

> This Markdown is the maintained source of the architecture narrative (it renders inline on
> GitHub). The **standalone, graphics-rich HTML render** at
> [`docs/architecture.html`](architecture.html) (pipeline diagram, block-glyph legend, diff
> sample) predates the five-crate split and may lag this text. For the exhaustive per-object
> API, generate the reference with `cargo apidoc` (see the
> [Developer Guide](developer-guide.md#the-api-reference-auto-generated-always-in-sync)).

A developer-facing design document for the `claude-replay` workspace: the reusable transcript
**engine**, the **presentation-support layer**, and the **presenters** built on them (a
terminal viewer, an HTML export/live server, and the `agent-jdi` supervisor). For the
hands-on "how do I build/test/extend this" material — including the
**[add-an-agent walkthrough](developer-guide.md#adding-an-agent)** and the
**[build-your-own-frontend guide](developer-guide.md#level-2--build-your-own-frontend-on-core--present)**
— see the [Developer Guide](developer-guide.md).

---

## 1. What it is

`claude-replay` reads an AI coding-agent's **session transcript** (a JSONL log — Claude Code,
Codex, or QoderWork) and replays it as a readable, foldable stream of blocks: user turns,
assistant prose, thinking, tool calls with their results and diffs, sub-agent spawns,
attachments, slash commands. It is **read-only** and **fully testable headless** — no TTY
required.

Two design goals shape everything below:

1. **Modularity as compiler-enforced layering.** The engine is agent-agnostic and
   presentation-agnostic; adding an agent is one small adapter, and adding a *presentation*
   is a new crate on top of the shared support layer — in both directions, no shared code is
   touched.
2. **Leanness as a design discipline.** Memory is bounded by keeping each representation no
   longer than it must live (§7), and CPU is bounded by doing only delta work on borrowed
   threads (§8). A live session with a million-line transcript costs O(open turn) RAM on the
   server and O(viewport) rendered state in either frontend.

## 2. The workspace: five crates, three levels of reuse

```
claude-replay-core      the sans-io engine: parse, fold, follow, discover, fold-policy
        ▲
claude-replay-present   presentation SUPPORT: session cache, pull protocol, text/highlight
        ▲                helpers, the shared Args — frontend-agnostic
   ┌────┴────┐
claude-replay-tui   claude-replay-html     the two presenters (mutually independent)
   └────┬────┘
claude-replay           the thin assembly crate: clap CLI + agent-jdi + compat re-exports
```

Each boundary is a **compiler-enforced invariant**, not a convention:

| crate | may depend on | must NOT contain |
|---|---|---|
| `claude-replay-core` | `serde`/`serde_json`, `anyhow` | any I/O policy, ratatui, syntect, clap, HTML |
| `claude-replay-present` | core (+ syntect; ratatui *types only*, no backend) | a terminal, an HTTP server, clap (unless the `cli` feature is on) |
| `claude-replay-tui` | core + present + ratatui/crossterm | HTML |
| `claude-replay-html` | core + present | any terminal dep (verified: zero crossterm/nucleo/arboard in its tree) |

The three levels of reuse this buys (each is a real, supported consumer story — worked
examples in the [Developer Guide §4–6](developer-guide.md)):

1. **Core alone** — parse/analyze transcripts in any program (`serde_json` + `anyhow` is the
   whole dependency bill).
2. **Core + present** — build a *new* frontend (native app, web service, notebook widget):
   the session cache, the incremental pull protocol with both halves implemented, fold
   policy, summaries, and highlighting are all reusable without adopting either existing UI.
3. **The finished presenters** — embed the TUI (`app::run`, or drive `View` directly) or the
   HTML exporter/server (`dump_html`/`dump_all_html`/`serve`) from your own binary.

Internally, each crate re-exports the layers below it at its own root (`crate::model`,
`crate::present`, …) — the same transparency trick at every boundary, so moved code reads as
if the split weren't there. One workspace version, bumped in one place.

## 3. The three-layer engine (core)

Parsing is a pipeline. Each layer has one job, and the agent-specific knowledge is confined
to Layer 1.

```mermaid
flowchart LR
  subgraph L1["Layer 1 — per agent (claude_model / codex_model)"]
    RAW["raw JSONL line"] -->|decode_line| MSG["canonical Message"]
  end
  subgraph L2["Layer 2 — shared (engine::replay)"]
    MSG -->|"Replayer fold + Shaping"| BLK["Block stream"]
  end
  subgraph L3["Layer 3 — presenters"]
    BLK --> TUI["ratatui view"]
    BLK --> HTML["HTML export / live server"]
    BLK --> DUMP["--dump text/ansi"]
  end
```

- **Layer 1 — decode (agent-specific).** `claude_model` / `codex_model` map that agent's raw
  line shapes onto a single **canonical [`Message`] vocabulary** (`engine::message`):
  `UserText`, `AssistantText`, `Thinking`, `ToolUse`, `ToolResult`, `Command`, `Attachment`,
  `Completion`, `QueueOp`, … The L1 decoder is the *only* place that knows an agent's field
  names. (QoderWork demonstrates the degenerate case: its format matches Claude's, so its
  adapter rides Claude's decoder wholesale — an agent can cost *zero* new parsing code.)
- **Layer 2 — fold (shared).** `engine::replay::Replayer` folds the `Message` stream into the
  `Block` render model: joining tool results onto their calls, grouping thinking turns,
  coalescing activity runs (the empirically-derived Claude Code span rules —
  [`design/cc-activity-coalescing.md`](../design/cc-activity-coalescing.md)), resolving the
  queued-prompt lifecycle, stamping per-turn times. The four points agents genuinely differ
  are isolated in a `Shaping` seam (build a tool block, join a result, keep-orphan policy,
  final turn grouping).
- **Layer 3 — present.** The viewer, HTML export, and `--dump` consume the `Block` stream.
  They never see a `Message` or a raw line.

**Why a canonical `Message` between L1 and L2?** It lets the meaty fold logic (hundreds of
lines: back-patching, grouping, the queue state machine) be written **once** and shared by
every agent. A new agent writes a small decoder, not a new fold.

## 4. The sans-io accumulator — one fold, every acquisition mode

The engine's heart deliberately does **no I/O**. [`SessionAccumulator`]
(`engine/builder.rs`) is a push-based fold: the *caller* acquires bytes however it likes and
pushes lines in; the accumulator threads them through L1 → L2 → metrics behind one method:

```rust
let mut acc = SessionAccumulator::new(Agent::Claude);   // or ::with_store(agent, store)
acc.advance_at(byte_offset, &line);                     // push one line — that's the I/O seam
let session = acc.snapshot();                           // the current Session, any time
```

Because byte acquisition is the caller's job, **every parse path in the workspace is the same
fold**, differing only in who feeds it:

| consumer | who pushes lines | why it matters |
|---|---|---|
| `parse_session_as` (batch) | a file reader, one line resident | whole-file parse never builds a `Vec<String>` |
| [`FollowParser`] (live tail) | the byte-offset `tail::LineReader`, only appended bytes | O(delta) per poll; proven byte-identical to a full re-parse |
| `SharedSession` (live server) | the follower, on a *client's request thread* | see §8 — no server-side polling loop |
| your code | a socket, an mmap, a test vector, a decompressor | the library never dictates your I/O |

The accumulator also owns the **durability frontier** that the whole leanness story hangs on:
as each turn completes, its blocks are drained **put-once** into a pluggable [`BlockStore`]
`S` (`committed: Vec<S::Bv>`), while the replayer retains only the **open turn** — so the
fold's resident content is O(turn) regardless of session length. Storage policy is injected,
not baked in:

- `InMemoryStore` (default) — committed blocks resident in RAM (`Bv = Block`).
- `TierBStore` (`engine/tier_b.rs`) — committed *content* serialized append-only to an
  off-heap buffer or an on-disk file; `Bv = Deferred { offset, size }`, a 12-byte locator.
  Reading a block back is a positional read + decode, dropped after use.

Incremental facts are maintained the same way: `session_meta()` folds the live header
(turns/tools/children) once per committed block on drain, and `stream_read(from)` hands a
live consumer `committed[from..]` + the open turn — O(delta), never an O(N) rescan.

## 5. The data model

The presenter-facing vocabulary lives in `model.rs` and is the stable public API.

- **[`Block`]** — one render unit. Variants: `UserText`, `AssistantText`, `Thinking { text,
  duration_secs, tools }`, `ToolUse { name, target, diffs, output, patch, read_lines }`,
  `ToolResult`, `Attachment`, `SubAgent`, `AgentDone`, `Command`, `QueueEvent`. Tool results
  are already joined onto their calls; nothing is dropped or truncated (what shows collapsed
  is a *view* decision, not a parse decision).
- **[`Session<BV = Block>`]** — the whole parse: `{ agent, cwd, blocks, user_times, metrics,
  index, tasks }`. Generic over the block representation: `Session<Block>` is fully resident;
  `Session<Deferred>` holds only the locator table (content in a tier-b backing) and reads
  blocks back through the [`BlockAccess`] trait.
- **[`SessionIndex`]** — derived within-session rollups (turns, sub-agents, tool counts,
  attachments) computed in one scan, so presenters don't re-walk the blocks.
- **[`Metrics`]** — token/cost tally + a formatted footer.
- **Classification** — `block_kind(&Block) -> BlockKind`, projected to a coarse `fold_key`
  (fold grouping — and the key of core's `FoldPolicy`) and a fine `BlockKind::html`
  (styling). One classifier, two projections — the TUI and HTML can't disagree on what a
  block *is*.

## 6. The per-agent seam

Everything that varies by agent is behind **one trait**, `TranscriptAdapter` (`adapter.rs`),
resolved through a tiny registry (`adapter(agent)` / `adapters()`). The hooks:

| Hook | Role | Default |
|------|------|---------|
| `agent()` | which `Agent` | — |
| `sniff(head)` | `SniffClaim::{Owns, CanParse, No}` — format *ownership* vs mere compatibility (drives `detect_agent` and the picker's "compatible" badge) | — |
| `store_contains(path)` | provenance: is this path inside my on-disk store? (ownership without a format marker) | `false` |
| `scan_join_ids(path)` | pass-1: the tool-call ids a later result joins onto | — |
| `decode_line(line, cwd, out)` | **L1**: raw line → 0+ canonical `Message`s | — |
| `shaping()` | the L2 `Shaping` const (4 fn-pointers) | — |
| `metrics_acc()` | a fresh token/cost accumulator | — |
| `candidates_scoped(cwd)` | discovery: sessions for a cwd | — |
| `resolve_id(id)` | discovery: id → transcript path | — |
| `load_tasks(path)` | the session's task/todo list from the agent's store | `None` |
| `parse_path_timed(path, times)` | whole-file parse | **provided** (composes the hooks) |
| `parse_reader(reader)` | metrics-only fold | **provided** |
| `enrich(path, blocks)` | load the sub-agent tree | **no-op** |
| `subagent_source(root, id)` | a child transcript's path | **None** |

Everything agent-neutral reaches per-agent behavior *only* through the registry — there is no
`match agent` scattered across the engine. Three adapters exist today and demonstrate the
cost floor: Claude (full), Codex (no sub-agent tree ⇒ omits those hooks), QoderWork (delegates
decoding to Claude's modules entirely; its adapter is discovery + identity).

> The `agent-jdi` supervisor mirrors this with its own `jdi::agent::AgentAdapter` registry.

## 7. Lean memory: a ladder of representations, each windowed

The memory design is one idea applied five times: **every representation is kept only as
long, and only as wide, as its consumer needs**. Data climbs this ladder, and each rung holds
a *window*, not the session:

```
  transcript bytes (agent's store, disk)     resident: none — byte-offset tail reads deltas
      │  L1 decode (one line at a time)
  canonical Message                          resident: ~1 line's worth, transient
      │  L2 fold (Replayer)
  Block — open turn                          resident: O(turn) in the accumulator
  Block — committed                          resident: policy! InMemoryStore = RAM;
      │                                        TierBStore = 12-byte locators, content on disk
      │  presentation shaping (fold policy, summaries, records/lines)
  presentation-friendly form                 TUI: heights+prefix (O(N) small ints);
      │                                        HTML: JSON records (client), record log (server)
      │  render
  rendered result                            TUI: `hot` window ≈ 96 blocks near the viewport;
                                             HTML: DOM window ≈ viewport ± 1500px
```

The windows at the top rung deserve emphasis, because both frontends independently converged
on the same structure — an O(N) *index* of cheap integers plus an O(viewport) cache of
expensive rendered output:

- **TUI** (`view.rs`): `heights: Vec<usize>` (wrapped height per block at the current width
  and fold state) + `prefix` sums make scroll math a binary search — but **rendered styled
  lines are NOT retained per block**. The only styled-line residency is `hot`, a bounded map
  (cap 96 blocks, evicted by distance from the viewport ± 8) filled on demand. A deliberate
  asymmetry: `search_text` keeps plain text for ALL wrapped lines (content-sized, cheap, and
  search must see everything) while styling stays windowed.
- **HTML client** (`html/export.js`): the JSON `records[]` array is the source of truth; the
  DOM holds only records within ~1500px of the viewport between two spacer `<div>`s whose
  heights are estimated then measured (prefix sums + binary search again). Eviction is
  lossless because interaction state (folds, search cursor, expansions) lives in id-keyed
  maps beside the records, re-applied on re-materialization. The full technique write-up:
  [`design/dom-virtualization.md`](../design/dom-virtualization.md).
- **HTML server** (`serve.rs` + `cache/`): a followed session's resident footprint is
  O(open turn) + tables — committed content spills to an on-disk `TierBStore` next to the
  render log, and the committed wire zone is served as a **pointer** (`committed_ext:
  {offset, len}`) that clients range-read from the append-only record log at their own pace
  (one serialization, N readers).

And the residency *cache* around all of it — [`SessionCache`] (`present::cache`) — is tiered:

| tier | holds | cost | transition |
|---|---|---|---|
| (c) registered | agent + path | ~nothing | the default for a discovered-but-unopened session (a large sub-agent tree stays here) |
| (a) resident | an open `FollowParser` | O(turn) + tables | `poll`ed recently; reaped to (c) after 30 s idle |
| (a′) pull-resident | a `SharedSession` | O(turn) + tables + disk backing | same reap policy; on eviction it **hibernates** its serving state to a sidecar, so revisiting an *unchanged* session restores (same epoch/gen — clients' cursors stay valid) instead of re-folding |

The TUI applies the same thinking one level up: sub-agent frames you drill into are kept
under an LRU cap of 4 (`MAX_RESIDENT_SUBAGENTS`); ancestors evict to registrations and
reload on demand.

## 8. Lean CPU: borrowed threads and delta-only protocols

**No dedicated work happens when nothing changed, and almost no thread exists solely to
wait.** Two mechanisms:

**Borrowed-thread tailing.** Neither frontend runs a follower thread:

- The **TUI** pumps the live tail on its *input event loop*: when `event::poll(250 ms)` times
  out with no keystroke, the idle tick calls `cache.poll_delta(id)` — so tailing costs zero
  threads and zero work while the user is interacting flat-out, and at most 4 polls/s idle.
- The **HTML server** (pull mode, the default) has **no background tailer at all**: the fold
  advances on the *client's request thread* (`/pull` → `SharedSession.advance()` → reply).
  An idle page = an idle server; a closed page costs nothing until the 30 s reap tidies the
  resident. The accept loop is one detached thread; connections are thread-per-request on
  loopback.

**Delta-only protocols, at both distances.** The committed/open split (§4) makes "what
changed?" answerable in O(turn), and both consumption protocols exploit it:

- In-process: [`FollowParser::poll_delta`] returns `(blocks, times, metrics,
  changed_from)` — the first index that differs, computed against the prior committed length
  plus a `common_prefix` over the small open-turn region. `View::apply_poll` keeps geometry
  and render cache for `[0..changed_from)` and re-derives only the tail (`dirty_from`).
- Cross-process: the **4-tuple cursor protocol** (`present::cache::stream`) — `Cursor
  { epoch, committed_id, provisional_gen, provisional_index }`. The cursor travels with each
  request, so the server is **per-client stateless** and N tabs/windows each read at their
  own pace; committed data is a pure append (sent once, or range-read from the record log),
  and only the open turn ever re-sends (gen bump). Both halves are implemented in Rust:
  [`pull`] (server) and [`PullClient`] (client — the executable specification the JS client
  mirrors transition-for-transition, and the ready-made consumer for a future decoupled TUI).
  A caught-up client's pull is idle: one `pull_indices` length-check, no clone, no render.

The same discipline shows up smaller: pass-1 id scan is a cheap stream; `put`-once into the
block store (never re-serialize committed content); `session_meta` folded per commit rather
than rescanned; the HTML emitter diffs block lines and re-sends only from the first
divergence; the DOM reconciler batches reads/writes to avoid layout thrash.

## 9. Discovery

`discover.rs` is the agent-neutral front door for *finding* transcripts: `detect_agent`
(sniff + store provenance → ownership, so a merely-*compatible* file is labeled, not
mislabeled), `session_cwd`/`session_id`, `candidates_all` (the cross-agent picker list),
`resolve_any` (id/path/latest → a path), `session_tasks`, and `subagent_source`. Cwd-based
auto-discovery is scoped **strictly inside `$HOME`** (`ancestors_below`): a cwd outside it
probes nothing, and the probe never reaches `$HOME`'s own slug — so stores polluted by
misbehaving agents (sessions recorded against `$HOME` or `/`) can never leak into an
unrelated directory's picker. Explicit paths and ids are never scoped.

## 10. Invariants the codebase holds itself to

- **Crate boundaries** — the §2 table is enforced by the dependency graph, not review.
- **Agent-agnostic everything above L1** — no presenter or shared-engine code matches on
  `Agent` for behavior; it routes through the adapter registry.
- **Byte-identical refactors** — the streaming parse is proven identical to frozen oracles
  (`#[cfg(test)]` equivalence gates in `claude_model`/`codex_model`), the follower to a full
  re-parse at every append, and every change runs `scripts/gate/gate.sh` — a frozen-fixture
  `--dump`/`--dump-html`/bundle diff that must print `BYTE-IDENTICAL: PASS`.
- **One classifier / one fold / one phrasing** — block classification, the L2 fold, and the
  summary vocabulary (`core::summary`) exist once; per-agent difference lives only in
  `decode_line` + `Shaping` (see
  [`design/fold-coalesce-summarize-extensibility.md`](../design/fold-coalesce-summarize-extensibility.md)).
- **Protocol equivalence** — `SharedSession` pulls are asserted identical across storage
  backings (RAM vs tier-b), and `PullClient` is walked through every protocol transition
  against the server half.

## 11. Where things live

| Path | What |
|------|------|
| `claude-replay-core/src/model.rs` | `Block` model + classification |
| `claude-replay-core/src/engine/replay.rs` | L2 `Replayer`/`Shaping`/`parse_stream` |
| `claude-replay-core/src/engine/builder.rs` | `SessionAccumulator` (sans-io fold) + `StreamRead` |
| `claude-replay-core/src/engine/{message,session,index,tasks,tier_b,path,time}.rs` | canonical log · `Session<BV>`/`BlockStore`/`BlockAccess` · rollups · task model · tier-b backing · helpers |
| `claude-replay-core/src/adapter.rs` | `TranscriptAdapter` trait + registry + `SniffClaim` |
| `claude-replay-core/src/{claude,codex}_model.rs` | L1 tokenizers + `Shaping` |
| `claude-replay-core/src/{claude,codex}_metrics.rs` | token/cost folding |
| `claude-replay-core/src/{claude,codex,qoderwork}_discover.rs` | per-agent transcript stores |
| `claude-replay-core/src/{discover,metrics,follow,tail,agent,fold,summary,diff}.rs` | discovery facade · metrics · live follower · byte-offset tail · `Agent` · fold policy · span phrasing · diff-row model |
| `claude-replay-present/src/cache/{mod,shared,stream}.rs` | `SessionCache` tiers · `SharedSession` (+hibernation) · the pull protocol (`Cursor`/`PullReply`/`pull`/`PullClient`) |
| `claude-replay-present/src/{present,highlight,sys,args}.rs` | text formatters · syntect highlighter · OS/path helpers · shared `Args` (`cli` feature) |
| `claude-replay-tui/src/{view,app,render,markdown,wrap,theme,picker,clipboard}.rs` | the terminal viewer |
| `claude-replay-html/src/html_export/{mod,bundle,serve}.rs` + `src/html/` | HTML render core · offline writers · live server · embedded CSS/JS |
| `src/jdi/` | the `agent-jdi` supervisor (see `src/jdi/DESIGN.md`) |

[`Block`]: ../claude-replay-core/src/model.rs
[`Message`]: ../claude-replay-core/src/engine/message.rs
[`Session<BV = Block>`]: ../claude-replay-core/src/engine/session.rs
[`BlockAccess`]: ../claude-replay-core/src/engine/session.rs
[`BlockStore`]: ../claude-replay-core/src/engine/session.rs
[`SessionIndex`]: ../claude-replay-core/src/engine/index.rs
[`Metrics`]: ../claude-replay-core/src/metrics.rs
[`FollowParser`]: ../claude-replay-core/src/follow.rs
[`FollowParser::poll_delta`]: ../claude-replay-core/src/follow.rs
[`SessionAccumulator`]: ../claude-replay-core/src/engine/builder.rs
[`SessionCache`]: ../claude-replay-present/src/cache/mod.rs
[`pull`]: ../claude-replay-present/src/cache/stream.rs
[`PullClient`]: ../claude-replay-present/src/cache/stream.rs
