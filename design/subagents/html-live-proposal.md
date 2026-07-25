# Sub-agent drill-down in the HTML export — with the live feed

Design of record for build-order step 7 in `README.md` §5 (HTML export): bringing the
TUI's sub-agent drill-down to the served/live HTML (`--html`, `-f`). The TUI (steps
1-6) is shipped.

**This supersedes the first draft's "one monolithic stream + node-aware reset" model.**
That machinery existed only to cram the whole agent tree into a single stream. The
model below — decided with the maintainer — is **one independent stream per agent**,
which deletes all of it.

> The mock (`html-export-mock.html`) still governs the **visual** design of a node
> (section header, prompt-from-parent card, usage, step outline, status pip). But its
> **navigation mechanism** — pre-rendering every node's `.agentview` in one DOM and
> toggling `display` — is replaced by **real per-agent pages** reached by navigation.
> Where the mock's *layout* and this doc disagree, the mock wins; where its *navigation*
> and this doc disagree, this doc wins.

---

## 1. The model

**One browser tab shows exactly one agent's transcript.** The root session is one
transcript; each sub-agent is *another* transcript. A sub-agent is not a node inside a
tree the page walks — it is a different session the tab points at.

```
claude-replay -f  (the backend / producer)
  │
  ├─ serves ONE shared static shell  (export.html + export.{css,js}, identical for all)
  │     page is parameterised by  ?session=<id>  → which stream it polls
  │
  ├─ per agent id, on disk:   <cache>/<id>.jsonl   (append-only parsed stream)
  │                           <cache>/<id>.pos     (consumed byte offset into the source)
  │
  └─ per CONNECTED agent, in memory:  one Tailer thread (the single writer of <id>.jsonl)

browser tab (?session=<id>)  ──poll <id>.jsonl from byte offset──▶  renders blocks + meta
  clicking a sub-agent spawn  ──navigate to ?session=<child-id>──▶  a fresh tab-load of that stream
```

Four decisions fixed this shape:

1. **Shared shell, `?session=<id>`.** A single static HTML+JS shell is served for every
   agent; the query string says which `<id>.jsonl` to poll. The renderer is identical —
   only the stream differs. (No per-agent HTML written to disk.)
2. **Full navigation between agents.** Clicking a sub-agent navigates the tab to
   `?session=<child-id>` — an ordinary page load of that agent's stream. The browser
   Back button ascends to the parent. No in-page tree, no view toggling, no scroll-memo
   juggling across nodes.
3. **Disk-backed streams with a resume offset.** Each agent's parsed output is
   materialised to `<id>.jsonl`; the source-transcript byte offset consumed so far is
   persisted (`<id>.pos`). A reconnect / new tab / switch-back resumes from that offset
   instead of re-parsing. The `.jsonl` is the durable cache; the client is a pure reader
   of it.
4. **Metadata rides the same stream.** Footer/liveness updates are appended to the same
   `<id>.jsonl` as `{t:"meta",…}` records whenever they change; the client keeps the
   latest and repaints the metadata display. No separate metadata channel or endpoint.

**Why this is the right shape.** Each `<id>.jsonl` is a *flat, independent* stream —
exactly today's single-session live feed (`follow_and_append` + `{t:"reset",from:N}`),
one per agent. A late tool-result that back-patches an earlier block resets only *that*
file, because that file only ever contains that one agent's blocks. There is no global
counter to shift, no sibling/child growth to renumber around, no node-tagged routing —
the entire "block ids must become per-node" + "per-node `nreset`" problem simply does
not arise. Sub-agents are *more streams*, not a harder stream.

### 1.1 Convergence with the engine's `SessionStore`

This is the disk-backed instance of `parser-engine.md` §8: `<id>` = `SessionId`, the
`.pos` offset = §8's "virtual position / `consumed`", resume-from-offset = §8's
fast-forward, one-Tailer-per-id = the store's single-producer residency, and the
parent's spawn-block liveness comes from the §7 agent index of the *parent* transcript.
Concretely, **`<id>.jsonl` IS the store's tier-(b) "materialized" artifact** (§8's
three tiers: (a) resident in RAM, (b) parsed on disk, (c) path-only): the HTML server
lives in tier (b), serving the stream file to clients, while an active tailer's in-memory
parse state is tier (a). Every agent a parent mentions is `see`n at tier (c) for free
the moment its spawn is parsed — so `?session=<child-id>` always resolves to a known
source path. If the engine refactor lands first, this backend is a thin serving layer
over the store; if not, it stands alone with the same contract and the same on-disk
files, and merges later with no format change.

---

## 2. The backend (producer)

### 2.1 One producer per agent, refcounted by clients

The guard against "two threads writing the same `.jsonl`" is **not** a per-write file
lock — it is **at most one Tailer per agent id**, shared by all its clients:

```rust
struct Backend {
    cache_dir: PathBuf,                              // <cache>/ (a temp dir for this run)
    tailers: Mutex<HashMap<SessionId, TailerHandle>>,// id → the single producer, refcounted
    sources: SourceResolver,                         // id → source transcript path (§2.3)
}

struct TailerHandle {
    clients: usize,        // connected readers; 0 ⇒ eligible to stop
    stop: Arc<AtomicBool>, // cooperative shutdown for the tail thread
    // the thread owns the ONLY write handle to <id>.jsonl
}
```

- **Client connects** (a tab starts polling `?session=<id>`): lock `tailers`; if a
  handle exists, `clients += 1` and return (the tab just reads the existing file). Else
  **create** the tailer — resume `consumed` from `<id>.pos` (or 0), spawn the tail
  thread, `clients = 1`. Creating is inside the lock, so two simultaneous first-clients
  can never spawn two writers.
- **Client disconnects** (poll idle past a TTL, or an explicit close beacon):
  `clients -= 1`; at 0, **stop the tail thread but keep `<id>.jsonl` + `<id>.pos`**
  (decision 3). The next visit re-attaches and resumes.
- Readers never write, so N tabs on one agent = N pollers of one file + one writer.
  The mutex only serialises tailer *creation/teardown*, never the append hot path.

### 2.2 The tail thread (the single writer)

Per agent, one thread runs the loop the current `-f` runs for the root, now keyed by id:

```
open source transcript for <id>            (root: main file; child: subagents/agent-<id>.jsonl)
seek/parse forward from `consumed`
loop until stop:
    parse newly-appended source bytes → Blocks (engine incremental ingest, §8.3)
    diff against last-emitted block tail → append {t:"reset",from:N}? then new {t:"block"} lines
    if metrics/liveness changed → append one {t:"meta", …} record
    persist consumed = new source offset  → <id>.pos
    sleep POLL_MS
```

This is byte-for-byte the existing `follow_and_append` (`html_export.rs:941`) contract —
re-parse the tail, positional-diff, append with a local `reset` — restricted to one
agent's file. The only new record is the `meta` append being *event-driven* (on change)
rather than re-emitted every cycle.

Back-patch note: because a child's source is its own `subagents/agent-<id>.jsonl`, its
stream's `reset` is naturally scoped to it. The parent's stream is unaffected by any
child growth — the parent only re-emits when the *parent* transcript grows (a new spawn,
a completion notification), which is a pure append except for the spawn block's own
result back-patch.

### 2.3 Source resolution & liveness (parent knows its children without reading them)

- **id → source path.** Root id → the main transcript. Child id → its
  `subagents/agent-<id>.jsonl` (via `model::subagent_file`, already exists). A
  `?session=<id>` for a child that hasn't spawned yet → no source file yet → the tailer
  waits (polls for its appearance) and the page shows an empty "awaiting" state.
- **Parent spawn-block status comes from the parent transcript**, not from reading the
  child: the parent's own events (the `Agent` spawn + the later `<task-notification>`
  completion) drive the spawn block's status pip and the parent's active-agents list —
  this is the §7 agent index applied to the parent stream. So a completing child flips
  its pip on the *parent* page with no child read and no navigation.
- **Subtree cost rollup** *does* need the children read. Defer it: the spawn block shows
  the child's own cost once that child has been tailed at least once (its `<id>.pos`
  exists), and a full subtree rollup is a Phase-C nicety, not load-bearing.

### 2.4 HTTP routes (extend the existing loopback server)

`serve_connection` (`html_export.rs:1110`) today serves the shell + one companion. Add:

| Route | Serves |
|---|---|
| `GET /?session=<id>` | the shared shell (static; `<id>` read client-side from the query) |
| `GET /stream?session=<id>&from=<byte>` | `<id>.jsonl` bytes from `from` (range-read); registers/refreshes the client's tailer refcount |
| `POST /close?session=<id>` (or a poll-TTL) | decrement the refcount |

The `from=<byte>` cursor is the client's read position in the `.jsonl` (distinct from the
backend's `consumed` position in the *source*). Same polling shape the page already uses;
it just carries the session id.

---

## 3. The stream format (`<id>.jsonl`)

One append-only file per agent, three record kinds — a strict superset of today's:

```jsonc
{"t":"meta", "model":"opus4.8","in":"1.8M","out":"212K","cache":"14M","cost":11.3,
             "status":"running","active_children":1, …}   // appended when it changes
{"t":"block","id":7,"kind":"edit", …}                     // a rendered block, monotonic id
{"t":"reset","from":5}                                    // local back-patch: drop blocks ≥5, re-emit
```

- `id` is a plain per-file monotonic counter (today's `block_id`, unchanged — no
  per-node scheme needed, because the file is single-agent).
- `reset` is today's protocol, unchanged, now trivially local.
- `meta` is appended on change; the client keeps the latest and repaints the footer /
  active-agents chip. The first `meta` is emitted at stream head so a fresh tab paints
  immediately.
- A sub-agent **spawn** block carries the child id + a link target: `{"t":"block",
  "kind":"agent","agent_id":"…","child":"?session=…","status":"running"}`. The client
  renders it as the collapsed spawn fold (mock visual) with the id as an `<a href>` to
  the child page.

---

## 4. The client (shared shell)

One static `export.html`+`export.js`, parameterised by `?session=<id>`:

1. On load, read `id` from the query (default: the root id the server injects).
2. Poll `GET /stream?session=<id>&from=<cursor>`; `consume(text)` exactly as today, with
   one added arm — `meta` updates the metadata display, `reset` truncates, `block`
   appends. No `tree`/`nreset`/`node`-routing — all deleted.
3. A **spawn block's id link** is an ordinary `<a href="?session=<child-id>">`. Clicking
   it navigates the tab; the browser Back button returns to the parent. The parent page's
   scroll is restored by the browser's own bfcache on Back — no `scrollMemo`.
4. The **active-agents affordance** (the `a`-menu equivalent): the parent's `meta` /
   spawn blocks list its running children as links; selecting one navigates. This is the
   HTML analogue of the TUI's `a` popup, but as hyperlinks.

Everything the mock draws *inside* one node (header, prompt card, usage, filter, step
outline) is per-page and already single-scope — no container swapping, since a page only
ever shows one agent.

---

## 5. Concurrency & lifecycle (the guarantees)

- **Single writer per file:** one Tailer per id; readers are read-only. Enforced by the
  `tailers` mutex around create/teardown (§2.1).
- **Many readers:** N tabs on one agent share one file + one writer; each keeps its own
  `from` cursor.
- **Resume:** disconnect stops the writer but keeps `<id>.jsonl` + `<id>.pos`; reconnect
  resumes from `consumed` (fast-forward) — never a re-parse of already-consumed bytes.
- **Bounded threads:** at most one thread per *currently-connected* agent (decision 3),
  not per ever-seen agent — a wide swarm browsed one page at a time uses one or two
  tailers, not hundreds.
- **Crash/stale safety:** `<id>.pos` is written after each successful append, so a
  restart resumes cleanly; a partially-written trailing line is re-derived by the next
  re-parse of the source tail (the append is idempotent under the positional diff).

---

## 6. Static `--dump-html` is out of scope here

This model is the **served/live** path (`--html`, `-f`) — it needs the backend to tail
and serve, and `file://` pages can't `fetch` (the reason the loopback server exists).
The self-contained offline `--dump-html` single file is a *separate* question and keeps
today's behavior for now (flat spawn fold, no drill-down) unless we later decide to emit
a directory of `<id>.jsonl` + the shared shell for offline serving. Flagged as open
question 1; not blocking the live work.

---

## 7. Phased build plan (each phase shippable)

- **Phase A — per-agent served streams (no sub-agents yet).** Refactor the current
  single companion into the id-keyed cache: `<id>.jsonl` + `<id>.pos`, the `tailers`
  registry (§2.1), `/stream?session=<id>` routing, the shared shell reading `?session=`.
  Move `meta` to event-driven appends. Root-only — proves the resume-offset + single-
  writer + refcount machinery with one agent. Gate: extend `html_export.rs` stream tests
  to assert offset resume (tail, stop, re-attach → no duplicate blocks) and single-writer
  (two readers, one file).

- **Phase B — sub-agent navigation.** Emit the spawn block's child link
  (`?session=<child-id>`) + the running-children list in `meta`; resolve child source
  paths (§2.3); flip spawn-block status from the parent's completion events. Clicking a
  spawn navigates to the child's stream; Back ascends. Gate: a two-file fixture (parent +
  one `subagents/agent-*.jsonl`) asserting the child stream tails independently and the
  parent's pip flips on the completion notification without touching the child.

- **Phase C — polish.** Subtree cost rollup (§2.3), the "awaiting async result" slot for
  a not-yet-spawned `?session=<id>`, the transient `+N` badge on a growing unopened child
  (from the parent `meta`), and optional offline `--dump-html` directory export (Q1).

---

## 8. Open questions

1. **Offline `--dump-html` (§6).** Keep it flat, or emit a directory (`<id>.jsonl` ×N +
   shared shell) that a `python -m http.server` can serve offline? The single-self-
   contained-file promise and the served model pull in different directions.
2. **Disconnect detection.** Poll-TTL (server marks a client gone after `k·POLL_MS` of
   silence) vs. an explicit `POST /close` beacon on `pagehide`. TTL is robust to crashes
   but lags; a beacon is prompt but unreliable. Probably both (beacon best-effort, TTL as
   backstop) — confirm.
3. **Cache location & cleanup.** A per-run temp dir wiped on exit (simplest; loses resume
   across restarts) vs. a stable per-session cache under `~/.cache` (survives restarts,
   needs a GC policy). Which matters more — clean exit or cross-restart resume?
4. **Cursor vs offset trust.** The client's `from` byte cursor assumes `<id>.jsonl` is
   strictly append-only between the client's reads. A `reset` record already covers
   block-level rewrites; confirm the *file* is never truncated under a live reader (it
   isn't — `reset` is an appended record, not a file rewrite).
