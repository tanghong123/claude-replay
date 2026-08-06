# Design: `claude-monitor` — every session on the machine, over HTTP

> **v4 — for review.** Three rounds of owner review have made this design *smaller* each time,
> which is the direction to want. v3's two central proposals were both rejected and both replaced
> with less: the session title left the meta record for the monitor's own store (§4.1), and the
> page's host-owned rail slot became no extension point at all (§6.3).
>
> What survived is a shorter list of changes to `claude-replay` (§6.6) and one of them —
> `session_card` — now pays for itself in the shipped frontends before the monitor exists.
>
> Unblocked by #96, shipped v1.35.0–v1.39.0. Read §2, §3 and §6.

Today's viewer answers *"show me this session"*. This answers **"what is happening on this
machine"** — one page over every agent's store, with enough per row to triage, and a click
through to the existing per-session view.

---

## 1. Requirements

The first six are the owner's, stated 2026-08-06; R7–R9 are mine, carried from v1.

| | |
|---|---|
| **R1** | See sessions from **every agent `claude-replay` can parse**, by default. User-configurable to narrow. |
| **R2** | **Organization and description** per session — agent-specific where that helps — good enough for quick identification. A title an agent writes goes **through the meta record** and may change over time; with none, degrade to the **last user message**. |
| **R3** | Automatically detect which sessions are **growing**. |
| **R4** | Automatically detect **new sessions** appearing. |
| **R5** | A **completely separate cache**, not `~/.cache/claude-replay`, owned by the monitor. |
| **R6** | A **separate crate**, and plausibly a separate **repo** later, depending on `claude-replay` as a library. |
| **R7** | The index must **not fold transcripts** on a page load. |
| **R8** | Read-only, loopback only (§11). |
| **R9** | It must never block or degrade a running `claude-replay`. |
| **R10** | **Reuse the HTML presentation.** The page is a thin left rail listing sessions *plus* today's session view — not a second renderer, and not a fork. |

## 2. The constraint that shapes everything: metadata, never transcripts

#96's meta record splits so **Part I is frontend-agnostic and fold-free** — turns, tools,
sub-agent lifecycle, `user_times`, per-model tokens, `extra`, task ops, cwd, span. A reader
deserialises those records and folds them with `MaterializedMeta::push`. No adapter, no `BV`, no
transcript, no `Replayer`.

Measured on the largest real session here:

| | |
|---|---|
| transcript | 107 MB |
| its `meta.jsonl` | **0.43 MB**, 711 records |
| cold fold of the transcript | 764 ms |
| resume from the cache | 49 ms |

An index row is therefore a sub-millisecond read of a small append-only file. A hundred sessions
is a directory walk plus a hundred small reads.

> **The rule that keeps it true.** If the monitor ever wants to parse a transcript to render
> something on the index, that is a signal the *record* is missing a field — not that the reader
> needs a fold. Adding the field to Part I is the fix.
>
> The rule is about the **index page**, not about the record swallowing everything. §4 is the
> counter-example that keeps it honest: the session title is *not* a session fact, so it lives in
> the monitor's own derived store on its own cadence — outside both the record and the fold.

### 2.1 Reading is lock-free, and that is a property not a shortcut

The index reads its stream **without taking the entry's lock**, while a writer may be appending.
Safe by construction: the stream is append-only so a reader never sees a rewritten prefix;
`MetaReader` drops a torn trailing line, which is exactly what a concurrent partial append looks
like; so the worst case is a row one commit stale for a moment. Taking the lock would let the
monitor deny sessions to itself (§10) — and R9 forbids anything of that shape.

### 2.2 The index does not align

Alignment exists so a **resume** never trusts a record the content stream cannot corroborate. The
index resumes nothing — it displays counters — so it folds the records it finds and never touches
the content stream. Exposure: one drain of over-count on a torn tail. The alternative is reading
a `BV` table, which drags `BV` decoding into a reader R7 wants free of it.

When a checkpoint is present, start there: a row becomes O(records since the last checkpoint).

## 3. Its own cache (R5) — and why that is a simplification

v1 had the monitor read whichever entry `tui/` or `html/` happened to hold. R5 removes that, and
it removes v1's largest open question with it.

**The monitor owns a cache at its own root** — `$CLAUDE_MONITOR_CACHE`, else
`$XDG_CACHE_HOME/claude-monitor`, else `~/.cache/claude-monitor` — using `claude-replay`'s durable
cache machinery, at a root the viewer never touches.

What this buys, and it is more than isolation:

- **No cold-index gap.** v1's worst problem was that the viewer's cache only holds sessions
  someone *opened*, while a machine-wide index is exactly the tool you point at sessions nobody
  has. Owning the cache makes populating it the monitor's job, so the **sweep is the design, not
  an edge case**, and every row is uniformly complete.
- **No lock contention with viewers, at all.** Locks are keyed by presentation; a different root
  means the monitor and a running `claude-replay` cannot collide even in principle. R9 stops
  being something to be careful about.
- **Its own eviction policy.** The monitor wants to retain the *index* for sessions it has not
  shown in weeks; a viewer wants to reclaim space. Different policies, now independently settable.
- **Its own `FOLD_VERSION` rollout.** A `claude-replay` upgrade invalidates the viewer's cache;
  the monitor's invalidates on its own schedule and re-sweeps in the background rather than making
  a user wait.

The cost is honest and one-time: **the monitor folds each session once, itself.** ~20 sessions ×
~0.8 s ≈ 16 s of background work on this machine, spread out, then never again — each subsequent
scan is a resume from the last committed block.

### 3.1 The sweep

One worker, strictly lowest priority, bounded:

- one session at a time — never a thread per session;
- **skip anything currently growing** (§5): folding a moving target wastes the work, and the
  session will be swept when it settles;
- newest-first, because the sessions you want indexed are the ones you were just working in;
- yields to serving — an index request never waits on the sweep.

A session that has never been swept still gets a row (§4 gives it identity from a bounded read);
it is missing counters, not presence. **The index is complete from the first page load; it becomes
*rich* as the sweep catches up.**

## 4. The session card (R2) — a general capability the monitor consumes

Recognition is the point of the index, and the first user prompt is a poor label: it says what a
session *started* as, not what it is. Probing real transcripts found much better material, and it
is **agent-specific**, exactly as R2 anticipated. Claude Code writes these, and **rewrites them as
the session evolves**:

| line | field | example (real) |
|---|---|---|
| `custom-title` | `customTitle` | `"Project status"` — the user's own name for it |
| `ai-title` | `aiTitle` | `"Check project status"` — the agent's generated title |
| `last-prompt` | `lastPrompt` | `"status of the project"` — the most recent prompt, not the first |

Codex carries `session_meta`/`turn_context` instead and no title at all; QoderWork may hold one
outside the transcript entirely, in its own store. So the derivation is a **per-agent seam**.

### 4.1 The title is derived OUTSIDE the fold, on its own cadence

v3 put the title in the meta record as a gauge. That was wrong, and the reason is sharper than
layering.

**`SessionAccumulator` is sans-io by design** — the caller acquires bytes and pushes lines; the
fold never touches a file (architecture §5). A title whose source may be *an agent's own
database* (QoderWork) cannot be produced without I/O, so asking for it at drain time would put I/O
inside the one component built to have none. The v3 hook, `session_title(path, tail)` called by
the accumulator, was that mistake written down.

Three more reasons, each independently sufficient:

- **Cadence mismatch.** A record is written per committing drain; a title needs refreshing
  *occasionally*. Coupling them over-serves the title and costs a tail scan per commit.
- **It taxes everyone for one consumer.** A new gauge bumps `FOLD_VERSION`, invalidating every
  existing durable cache entry for every `claude-replay` user — to carry a field only the monitor
  reads.
- **It is a derived view, not a session fact.** Turns and tokens are what the session *did*. A
  title is a label someone (or something) chose for it, revisable at any time and reconstructible
  from the transcript whenever wanted. Caches of derived views want their own lifecycle.

So: **the monitor owns the title, in its own store, on its own schedule.**

#### The agent knowledge belongs behind the one seam — and not only for the monitor

`TranscriptAdapter` already has a *class* of hook for exactly this — path-taking,
I/O-performing, and **never called by the fold**: `load_tasks(path)`, `candidates_scoped(cwd)`,
`resolve_id(id)`, `subagent_source(root, id)`, `load_attachment(...)`. One more joins them:

```rust
/// The agent's own idea of what this session is called, and what it was last asked — read from
/// wherever the agent keeps it. Discovery-side: like `load_tasks`, this does I/O and the fold
/// never calls it.
fn session_card(&self, _path: &Path) -> Option<SessionCard> { None }

pub struct SessionCard {
    /// A name the agent or the user gave this session.
    pub title: Option<String>,
    /// The most recent prompt — "what it is doing now".
    pub last_prompt: Option<String>,
}
```

Claude reads the last `custom-title` / `ai-title` / `last-prompt` from a bounded tail; Codex
returns `None` today; QoderWork queries its database. Default `None` means an agent opts in with
one method, and no adapter is forced to care.

**Display precedence**, all three sources agent-side or discovery-side, none in the fold:
`title` → `last_prompt` → `Candidate::snippet` (the first prompt, which always exists).

#### This is not monitor scaffolding — both frontends want it today

The hook pays for itself before the monitor exists, because **the product currently shows a UUID
where a name belongs**:

| surface | today | with `session_card` |
|---|---|---|
| TUI viewer title | `path.file_stem()` — the raw session UUID (`app.rs`) | `"Project status"` |
| HTML page title | `display_title` → the UUID, falling back to the *repo name* when it looks like an id | the session's own name, repo name as the fallback |
| picker rows | project + first prompt | project + the session's name, which is what the user called it |

So this lands in `claude-replay` **on its own**, as a small self-contained improvement, and the
monitor is its third consumer rather than its reason. That ordering is worth keeping: it means the
hook gets exercised by two shipped frontends before a new application depends on it, which is a
much better way to find out the shape is wrong.

The facade surfaces it once — `Transcript::card()` — so no frontend reaches for an adapter
directly, exactly as `Transcript::parse`/`load_attachment` already work.

#### The store

`cards.json` at the monitor's cache root — **one file**, rewritten atomically (temp + rename).
One file rather than one per entry because the index reads every card on every page load, and N
small files would be N syscalls for no benefit. Small: N × a couple of hundred bytes.

```
{ "<session-id>": { title, last_prompt, source, derived_at, turns_at_derivation } }
```

#### When it refreshes

| trigger | why |
|---|---|
| **a new session appears** (§5) | a row without a name is not identifiable, and this is the cheapest possible moment |
| **the sweep folds it** (§3.1) | the file is already warm; a bounded tail costs nothing on top |
| **turns advanced by ≥ N since derivation** | a title tracks the topic, and the turn count is the topic's clock |
| never otherwise | a session that is not growing cannot have changed its title |

Using **turns** rather than bytes is the load-bearing choice: `session_meta.turns` is *already on
the row* from the index, so evaluating staleness is free. A byte threshold would need a `stat` per
session per cycle — paying I/O to decide whether to do I/O.

**Open (§13 Q1):** what N is. Small enough that a title does not lag a pivot in the work, large
enough that a chatty session is not re-derived every cycle. Guessing 10–20 turns; it wants
watching rather than deciding up front.

**The refresh is cheap because the adapter memoizes**, not because it is rare — see
**`design/session-card.md`**. Each derivation returns an opaque JSON memo the monitor stores
beside the card and hands back next time; an adapter that finds nothing changed answers from a
single `stat`. Measured: **0.96 ms → 1.3 µs** for an unchanged session. That is what makes
"re-derive on every scan" affordable at all, and it is why the staleness decision belongs to the
adapter: a QoderWork title changes when its *database* changes, with the transcript untouched, so
no caller-side mtime rule could be correct for both agents.

### 4.2 Organization

Grouping, in priority order — a display concern, not a stored one:

1. **project** (`Candidate::project`, the working directory's leaf) — the axis people think in;
2. **agent** within a project, when more than one is present;
3. **state** (§5) by sort rather than grouping: growing first, then idle, then finished.

**Open (§13 Q2):** whether the project leaf is enough. Two checkouts of one repo in different
directories share a leaf and would merge; the full cwd disambiguates but is too long to show.

## 5. Change detection (R3, R4)

Both requirements are one mechanism: **diff two scans.**

| | detected by | cost |
|---|---|---|
| **new session** (R4) | a path in `candidates_all()` that was not in the previous scan | a directory listing |
| **growing session** (R3) | tree mtime newer than the previous scan's, over the root transcript **and** every child under `<stem>/subagents/` | a `stat` per file |

This is a better primary signal than v1's, and it is worth being explicit about why. v1 led with
process matching, whose weak point (§5.2) is that a process cannot be reliably mapped to its
session. **Growth needs no process at all** — no `ps`, no argv parsing, no pid→session mapping. It
answers R3 directly and it is exact: the file grew or it did not.

### 5.1 Process liveness is the *secondary* signal

Growth alone cannot separate *idle but alive* from *finished*, and that distinction is worth
showing. So the three-state model from #99 stays, with growth promoted to primary:

| state | rule |
|---|---|
| **Growing** | the tree grew since the last scan, or a tool is in flight |
| **IdleAlive** | not growing, but a live agent process maps to this session |
| **Finished** | not growing, no process |

The **in-flight tool** check (`jdi::inflight_tool_in_tail`) must ship even though it never fired in
#99's sample: an agent blocked in a long tool call writes *nothing anywhere*, so its mtime ages
past any threshold while it is maximally busy. A correctness signal for a rare state.

### 5.2 Where it stays honest

The process→session link has no reliable mechanism, and #99 measured it: `--resume <uuid>` in argv
is exact but **7 of 11 live agents carried no id**, having been started fresh. Fallbacks: an open
fd works for Codex, fails for Claude; cwd→project-slug plus recency works for Claude but is a
heuristic.

Because growth is primary, this only degrades **IdleAlive → Finished** — a quiet session shown as
finished when it is merely waiting. That is a much smaller error than v1's, where the same
weakness could mislabel a *running* session. Rows whose process match is unconfirmed should say
so rather than assert.

#99's two traps still apply and still lose agents silently: do not match argv anywhere (an agent's
own tool shells carry `claude` in theirs), and do not read `comm` from a bulk `ps` listing (it
truncates `/Users/hong/.local/bin/claude` to `/Users/hong/.loc`, dropping every agent launched by
absolute path — this cost #99's prototype 2 of 4 resolvable sessions).

## 6. Reusing the HTML presentation (R10)

The page the owner wants is **a thin left rail listing sessions, plus today's session view**. So
the monitor is not a sibling of the HTML frontend — it is a *host* for it. That is a bigger claim
than "depend on the crate", because today the crate does not offer a presentation; it offers a
**server**.

### 6.1 What is actually in the way

Three things are decided inside `start_server` that a host must decide instead:

| hardcoded today | where | why it blocks reuse |
|---|---|---|
| `cache::admit::default_root()` | `serve.rs` | the monitor owns a different root (R5) |
| `Presentation::Html` | `serve.rs` | fine as-is (§10), but it must be *passed*, not assumed |
| `render_flavor(&fold)` + `FoldPolicy` | `serve.rs` | the host chooses the render parameters |

And two things are shaped as a program rather than a library:

- **the crate owns the listener** (`spawn_http_server` binds, loops, routes, and serves static
  files), so a host with its own routes has no way in;
- **the page shell is closed** — `build_page` composes header, sidebar and content with no slot a
  host can fill.

Nothing here is deep. It is all "a binary grew outward"; the fix is to name the service that is
already in there.

### 6.2 The seam: a session **service**, not a server

Extract what `Live` already is into a public type, parameterised by what §6.1 lists, and reduce
`serve`/`start_server` to thin users of it — so there is **one implementation**, and the byte gate
keeps covering it because `--html` still walks the same code.

```rust
// claude-replay-html
pub struct SessionService { /* today's `Live`, plus the config below */ }

pub struct ServiceConfig {
    /// Durable cache root. `None` ⇒ ephemeral. The monitor passes its own (§3);
    /// `--html` passes `claude-replay`'s.
    pub cache_root: Option<PathBuf>,
    /// Namespace within that root. Both callers pass `Presentation::HTML` — they differ by
    /// ROOT, not by namespace (§10).
    pub presentation: Presentation,
    /// Render parameters. Also the flavor the durable stream is validated against.
    pub fold: FoldPolicy,
    /// Scratch directory for the cache-less fallback and static assets.
    pub scratch: PathBuf,
}

impl SessionService {
    pub fn new(cfg: ServiceConfig) -> Self;

    // — the session domain the host drives —
    pub fn register(&self, id: &str, src: Transcript, cwd: Option<String>);
    pub fn set_port(&self, port: u16);          // publishes the rendezvous note (#96)
    pub fn reap(&self, ttl_ms: u128);

    // — one method per wire surface; each is today's handler body —
    pub fn page(&self, id: &str) -> String;                                  // the shell
    pub fn pull(&self, id: &str, cursor: Cursor) -> Option<String>;          // /pull
    pub fn records(&self, id: &str, from: u64, len: u64, epoch: u64) -> Result<Vec<u8>, ()>;
    pub fn reveal(&self, path: &str) -> bool;                                // /__reveal
    pub fn asset(name: &str) -> Option<(&'static str, &'static [u8])>;       // export.css/js
}
```

`RecordStore`, `Emitter`, `render_blocks` and the page internals all stay **private**. The host
never names a `BV`, never renders a block, never touches the cache — it hands over a config and
calls four methods. That is the test for whether this is a real seam or a leak.

### 6.3 The unit of reuse is a **URL**, not a slot

v3 gave the page a host-owned `#rail` region. That was wrong, and the objection generalises past
the rail: a slot is an extension point shaped like *one* host's layout. A host that wants the view
in a top strip, a right pane, a tab set, a modal, or two side-by-side has no way in — it would
have to ask for another slot, and the crate would accumulate one per host.

So the crate offers **no extension point at all**. It offers a self-contained page at a URL:

```
GET /session?id=<sid>     → the complete session view, exactly as `--html` serves it today
GET /pull, /records, …    → its wire surface
GET /export.css, .js      → its assets
```

The monitor then composes **at the document level**, where composition belongs:

```html
<aside id="rail"><!-- the monitor's session list, its own markup, its own script --></aside>
<iframe id="view" src="/session?id=…"></iframe>
```

Nothing host-specific enters `claude-replay-html`. The page it serves is byte-identical to
today's, so the byte gate over `--dump-html`/`--dump-all-html` covers it with no new argument
needed.

**This is better than the slot on its own terms, not just purer:**

- **It generalises.** Any layout a host can express in a document, it can have — because the view
  is a URL and a URL goes anywhere.
- **Isolation is total.** The view's global ids (`#sidebar`, `#layout`, `#taskbox`), its
  document-level keyboard handlers, and its window-scroll virtualization cannot collide with the
  host's. With a slot, all three are shared and every one of them is a real collision risk — the
  virtualizer in particular measures against the viewport.
- **It fixes §6.4's open question rather than answering it.** Swapping `view.src` keeps the rail's
  own state — scroll, filter, selection — because the rail was never re-rendered. A full page
  load, which the slot design implied, threw that away.
- **Several views at once become free.** Two frames, two independent pull cursors — which is
  already the protocol's design (per-client stateless, N tabs at their own pace).

### 6.4 What the URL boundary does not give

Worth stating, because the boundary is a real one and I would rather name its limit than discover
it later:

- A host **cannot restyle** the view — no theme injection, no CSS variables reaching in. Same
  origin means it *could* reach in via the frame's DOM, but that is a coupling that would break on
  the crate's next render change, so treat it as unavailable.
- A host **cannot share a scroll context** with the view. Here that is a feature (the rail stays
  put while the view scrolls); for a host that wants one continuous document it is a wall.

If either is ever wanted, the honest answer is not a slot — it is making the view a **scoped
component**: shadow-DOM custom element, ids namespaced, virtualization rooted at a scroll
container instead of the window, `export.css` scoped. That is a real piece of work in the most
intricate file in the crate, and it should be driven by a host that actually needs it rather than
designed on spec. The URL boundary is what makes deferring it safe: nothing about it forecloses
the component later.

### 6.5 Navigating between sessions

Clicking a rail row sets `view.src = "/session?id=<sid>"`. The rail is untouched — it keeps
scroll, filter and selection — and the view gets a clean per-session pull cursor, which is exactly
what the protocol wants. No history juggling in the crate; the monitor owns its own URL bar if it
wants deep links.

### 6.6 What this costs `claude-replay`

Honest accounting, because R10 pushes work into the library rather than the monitor:

| change | size | risk |
|---|---|---|
| `Live` → `SessionService` + `ServiceConfig` | mechanical: move fields, thread config | low — `--html` becomes a caller and the gate covers it |
| listener takes a handler | small | low |
| ~~rail slot in the shell~~ | **none** | the page is untouched (§6.3) |
| `/session?id=` route serving today's page | trivial | none — it is `page(id)` behind a route |
| `session_card` adapter hook (§4.1) | one defaulted method | low — discovery-side, alongside `load_tasks`; the fold never calls it, and **no `FOLD_VERSION` bump** |
| liveness helpers move to core (§10) | mechanical | low |

None of it is speculative generality: every item is something the monitor needs on day one, and
each leaves `--html` walking the same code it walks now. Two rounds of review have made this table
*shorter* — the rail became nothing, and the title stopped touching the fold — which is the
direction a design should move in.

## 7. Shape

```
 claude-monitor  (separate crate — §10)
   ├── scan       discover::candidates_all(filter)     every agent by default (R1)
   ├── diff       previous scan vs this one            new (R4) + growing (R3)
   ├── card       per-agent title/description          bounded tail read (§4)
   ├── index      own cache root, lock-free reads      MaterializedMeta per session (§2, §3)
   ├── sweep      fold-once, background, skips growing populates its own cache (§3.1)
   ├── liveness   process, only to split idle/finished secondary (§5.1)
   └── serve      index page + hand-off                loopback; view via claude-replay-html
```

Row model — the table is the acceptance test for R7, since every source is a listing, a `stat`, a
bounded tail read, or the meta stream:

| column | source | folds? |
|---|---|---|
| title · description | §4 per-agent card | no — bounded tail |
| project · agent | `Candidate` | no |
| last activity | tree mtime | no |
| state | §5 | no |
| turns · tools · sub-agents | `MaterializedMeta.session_meta` | no |
| cost | `MaterializedMeta.tokens` + `metrics::total_cost` | no |
| tasks | `MaterializedMeta.tasks` | no |

## 8. Refresh

A **poll**, not a watcher. A watcher over N store roots is platform-specific and fails open (a
missed event is a silently stale row); §5 is a diff of two scans, which wants a timer by
construction; and the liveness fallback has no event to subscribe to anyway.

The server re-scans on a floor of ~2 s and serves a cached snapshot in between, so N open tabs
cost one scan. Per-session views keep tailing through the existing pull protocol, untouched.

**Open (§13 Q3):** whether the scan is incremental (only sessions whose mtime moved) or a full
re-read per cycle. Full is simpler and fine at ~100; unmeasured at 1000.

## 9. Interaction with a running viewer (R9)

- **Indexing** — its own cache root (§3) plus lock-free reads (§2.1). No contention is possible.
- **Opening a session nobody serves** — the monitor stands up (or reuses) `start_server`.
- **Opening a session someone already serves** — `existing_server(root, sid)` reads the holder's
  published port from the lock and the monitor **redirects there** instead of standing up a
  duplicate server, fold and copy. This is #96's rendezvous, and the monitor is the consumer it
  was really built for.

## 10. Where it lives, and what `claude-replay` must expose first (R6)

**A separate crate now, `claude-monitor`**, depending on `claude-replay-present` (cache, meta
stream), `claude-replay-core` (discovery) and `claude-replay-html` (the session view). Not a mode
of the html crate: html is a *frontend* that renders one session; the monitor is an *application*
that scans stores, probes processes and routes between servers. Putting that inside the
presentation layer would break the dependency rule the architecture enforces by manifest, and
would make `--dump-html` link a process scanner.

**A separate repo is a stronger constraint than a separate crate**, and probing found two places
the current API does not survive the move:

| gap | why it blocks R6 | proposed fix |
|---|---|---|
| ~~`Presentation` is a closed enum~~ | **v2 got this wrong.** It claimed the monitor needs a namespace of its own. It does not: it reuses the HTML presentation (§6) at **its own root**, and the root already isolates it — `html/<session>/` under `~/.cache/claude-monitor` cannot collide with `html/<session>/` under `~/.cache/claude-replay`. Opening the enum is still right *eventually*, for a third-party frontend with its own `BV`, but it is not on this task's path. |
| **The liveness helpers live in the root binary crate** — `jdi::latest_tree_activity`, `jdi::inflight_tool_in_tail` are reachable (`pub mod jdi`) but only by depending on `claude-replay` itself, which pulls in clap, ratatui, crossterm and the whole viewer | a separate repo would link a terminal UI to `stat` a file | move both **down into `claude-replay-core`** (they are transcript-shaped, agent-neutral, and read files — core's exact remit), and have `jdi` re-export them |

Neither is large. Both are worth doing before the monitor exists rather than after, because both
are API decisions and the second run of an API is the expensive one.

**Open (§13 Q4):** whether the per-agent card (§4) should be a hook on `TranscriptAdapter` — which
would make it available to every consumer and keep agent knowledge behind the one seam — or a
monitor-side trait, which keeps a display concern out of the parser. I lean toward the adapter
hook, because "what is this session called" is a fact about the agent's format, not about the
monitor.

## 11. Exposure

**Loopback only.** `127.0.0.1`, as `--html` does today, with no bind-address flag in the design.
The page aggregates every session on the machine — prompts, file contents, tool output, working
directories — so a careless bind exposes the machine's whole body of work, not one session. If
remote access is ever wanted it is a separate decision with its own review, and the honest
mechanism is an SSH tunnel, not a flag.

Read-only is structural, not a setting: the monitor links nothing that writes a transcript or
injects input. #99 §4 set out what productising injection would require — per-session consent at
the time, visibility in the target, local-only, refuse-by-default. A read-only monitor needs none
of it and should keep needing none of it.

## 12. What this is not

- Not a scheduler or supervisor — `agent-jdi` owns unattended runs.
- Not a second renderer; the session view is the existing HTML frontend.
- Not multi-machine. Everything assumes one filesystem and one process table.
- Not an auth boundary. §11's answer to "who can see this" is "whoever is on this machine".

## 13. Open questions

1. **How stale may a title get?** (§4.1) The refresh trigger is "turns advanced by ≥ N since
   derivation", because the turn count is already on the row and costs nothing to test. N wants
   watching rather than deciding up front — guessing 10–20.
2. **Is the project leaf the right grouping key?** (§4.2) Two checkouts of one repo share a leaf
   and would merge; the full cwd disambiguates but is too long to show.
3. **Scan strategy at scale.** (§8) Full re-read per cycle vs incremental by mtime; unmeasured at
   ~1000 sessions.
4. **What is the sweep's completion signal?** A session whose fold *fails* (corrupt transcript,
   unknown agent) must not be retried every cycle forever. Some negative cache with a reason, but
   its shape and invalidation are undesigned.
5. **A session whose transcript was deleted but whose index entry survives.** A ghost row is
   arguably useful history and arguably confusing. Undecided.
6. **Sub-agent rows.** A session with 40 sub-agents has 40 child transcripts. List, nest, or leave
   to drill-down? Leaning drill-down — but a long-running child is exactly what you would want to
   see from the index.
7. **Configuration surface for R1.** A flag, a config file, or both — and whether "which agents"
   is the only axis worth configuring (versus store roots, or projects).
8. **Keyboard focus across the frame boundary.** (§6.3) The rail wants `↑/↓` and `/`; the view
   already binds `j k`, `/`, `[ ]`, `space`. Same-origin `postMessage` handles it, but who owns a
   keystroke when the rail has focus and the view does not is a decision, not a detail.

**Resolved by review** — kept as a record of what moved and why:

- *Should the title live in the meta record?* No (§4.1). It would put I/O in the sans-io fold,
  bump `FOLD_VERSION` for every user to serve one consumer, and couple an occasional refresh to a
  per-commit cadence. It is a derived view with its own lifecycle.
- *Is the card an adapter hook or monitor-side?* Adapter hook (§4.1) — and the deciding argument
  was not the monitor at all: the TUI and HTML both show a UUID today where a name belongs.
- *Does the rail belong in the html crate?* No (§6.3). A slot is shaped like one host's layout;
  the unit of reuse is a URL, which anticipates none and serves all.
- *Full page load on session switch?* Moot (§6.5) — swapping the frame's `src` keeps the rail's
  state, which was the concern.
- *Must `Presentation` become an open id?* Not for this (§10) — the monitor reuses
  `Presentation::HTML` at its own root, and the root already isolates it.

## Rejected

| shape | why |
|---|---|
| Fold every transcript on index load | O(bytes) per page load — the entire reason #96 came first |
| Share the viewer's cache | R5; and it left the index permanently partial for sessions nobody opened, which is the population a machine-wide monitor exists for |
| Read the frontend's `BV` table for counters | drags `BV` decoding into a reader R7 wants free of it, and makes the index presentation-specific for no gain |
| Take the entry lock while indexing | R9 — the monitor must never be able to deny a session to anything |
| Lead with process liveness | v1 did; its weak link (process→session) is unreliable, and growth answers R3 directly with a `stat` |
| A filesystem watcher instead of polling | platform-specific, fails open on a missed event, and the diff wants a timer regardless |
| A second HTML renderer tuned for summaries | two renderers drift — the same argument that keeps one classifier and one fold |
| First-prompt snippet as the display name | says what a session *started* as; `custom-title`/`ai-title`/`last-prompt` say what it *is* |
| Bind non-loopback behind a flag | §11 — the aggregate is the machine's whole body of work; a flag is too small a gesture |
| A monitor-side title function instead of an adapter hook | could not serve QoderWork, whose title lives in its own database rather than the transcript; and it would leave the TUI and HTML showing a UUID forever (§4.1) |
| The title as a meta-record gauge | v3 proposed it. It puts I/O in the **sans-io** fold, bumps `FOLD_VERSION` for every user to serve one consumer, and ties an occasional refresh to a per-commit cadence (§4.1) |
| A host-owned `#rail` slot in the page | v3 proposed it. A slot is shaped like one host's layout; the next host needs a different one, and the crate accumulates one per host (§6.3) |
| The monitor reimplementing the loopback server | ~100 lines duplicated that would drift on the first header fix (§6.4) |
| The monitor serving its own page shell that embeds the session view | duplicates the page template — the divergence would be silent and permanent (§13 Q10) |
| Forking the html crate for a "monitor mode" | R10; and two renderers drift, the same argument that keeps one classifier and one fold |
