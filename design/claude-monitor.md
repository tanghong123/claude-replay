# Design: `claude-monitor` — every session on the machine, over HTTP

> **v5 — settled, and BUILT (v1.49.0).** The crate exists (`claude-monitor/`); §6.6's library
> changes all landed (SessionService/ServiceConfig, /session?id + embed chrome, listener takes
> a handler, liveness helpers in core, `store_transcripts` + `workspace_anchored` on the seam).
>
> Original settlement note: The 2026-08-07 owner review closed the design, and closed it *smaller*
> again: the background sweep is **gone** (§3 — the owner rejected an upfront fold of every
> transcript on startup-delay and storage grounds; population is now a side effect of visiting),
> the overview lists **main sessions only** (§4.2/§13), grouping is **per agent kind** — project
> for workspace-anchored agents, agent for desktop-collaboration agents like QoderWork (§4.2) —
> and the monitor is confirmed as a **web service on a loopback port** (§7, §11). The remaining
> §13 items were settled with implementer's calls recorded there; none block building.
>
> v4's history: three rounds of review each made the design smaller — v3's two central
> proposals were both rejected and replaced with less (the title left the meta record for the
> monitor's own store, §4.1; the host-owned rail slot became no extension point at all, §6.3).
> One survivor, `session_card`, already ships in both frontends (#106).
>
> Unblocked by #96 (v1.35.0–v1.39.0) and #109/v1.46+ (release/retention). Read §2, §3 and §6.

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

Under §3's lazy population the stream only EXISTS for sessions that have been visited: a
never-visited row shows the card columns (title, project/agent, state, last activity — none of
which fold) and leaves the counters blank. That is the owner's accepted trade (2026-08-07):
recognition needs no fold, and richness follows use.

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

What the root holds — and deliberately does NOT hold — changed in the 2026-08-07 review.
v2–v4 populated it with a background **sweep**: fold every transcript once, so every row is
uniformly rich. The owner rejected that on two grounds that compound at scale: a first launch
that folds a machine's whole history is a **long delay** before the tool is useful, and the
durable entries it writes (content stream + meta stream per session) are **real storage** —
tens of MB for one large session, GBs for a machine's worth — most of it for sessions nobody
will ever open. So:

**Population is lazy.** The root holds exactly two kinds of state:

- **The card index** — one small persisted file (title, path, agent, project, mtime per
  session; a few hundred bytes each). Cards come from §4's bounded tail read — **no fold, no
  BVs, nothing per-session beyond the row itself** — and re-derive when a transcript's mtime
  moves. This is what every row is born from, and for a never-visited session it is all there
  is.
- **Durable entries for VISITED sessions only.** Opening a row serves it through the existing
  HTML presentation (§6) against this root — and that serve IS the fold, writing the entry as
  its ordinary side effect (#96). The monitor never folds anything itself; there is **no sweep
  worker at all**. From the first visit on, that session's row reads counters from its meta
  stream (§2, lock-free) and stays current: subsequent serves resume, and the row rides along.

What owning the root still buys:

- **No lock contention with viewers, at all.** A different root means the monitor and a running
  `claude-replay` cannot collide even in principle. R9 stops being something to be careful about.
- **Its own eviction policy.** Card entries are near-free and keep long; visited entries GC on
  the ordinary durable-cache sweep at this root.
- **Its own `FOLD_VERSION` rollout.** An upgrade invalidates visited entries only; they rebuild
  on next visit rather than in a re-sweep nobody asked for.

The accepted costs, stated plainly rather than discovered later: **counters are blank until a
session's first visit**, and **the first visit to a large never-opened session pays its cold
fold interactively** (~0.8 s for 107 MB in the TUI's presentation; the HTML render-to-record
fold is heavier). If the second cost ever matters, the bolt-on is to pre-fold only *currently
growing* sessions — the handful you are about to open — which changes no architecture and is
explicitly **not** in v1.

### 3.1 What replaced the sweep

Nothing runs in the background but the §8 scan (a `stat` walk + card re-derives for moved
mtimes). The fold pipeline is exercised exactly as the viewer already exercises it — by
serving — so the monitor adds **no new fold call sites** to keep correct.

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

**#119 — QoderWork's title, confirmed empirically.** The transcript has none; the DB does. It
lives in SQLite at `~/Library/Application Support/QoderWork/data/agents.db`, table `sub_chats`,
column `name`, joined to a transcript by `sub_chats.session_id = <file stem>` (with a
`chats.name` fallback via the `…workspace-<chat_id>` slug). Measured on this machine: **30/30**
`sub_chats` rows carry a non-empty, human-chosen name — Chinese task titles (`重构解析器模块`,
`整理接口迁移记录`) and English skill names (`Read skill documentation`) — every one joining
cleanly to a real transcript stem. The reader already ships behind the **`qoderwork-titles`**
feature (#106): `QoderWorkAdapter::session_card` → `db_title`, reached the same way every other
frontend gets a title (`display_title` → `Transcript::card`). The only gap #119 found was that
nothing enabled the feature, so the monitor rail showed bare UUIDs. Resolution: the monitor turns
`qoderwork-titles` **on**, but only on the macOS release targets — `db_path` is a macOS-only path
(QoderWork is a macOS Electron app), so a Linux binary would compile bundled SQLite to reach a
file that cannot exist. Caveat carried forward: the rail caches the title on the transcript's
mtime, so a DB-only rename of an *idle* session does not refresh until the transcript next moves.

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

**The overview lists MAIN sessions only** (owner decision, 2026-08-07). Sub-agents are the
session view's job — the drill-down that exists today — not rows in the index. The one thing
the index still tells you about them: a visited session's row carries its sub-agent count and
running state from `MaterializedMeta.session_meta.children`, so "something is still working
under this session" is visible on the parent without child rows.

Grouping is **per agent kind** (owner decision, same review) — a display concern, not a stored
one:

- **Workspace-anchored agents** (Claude Code, Codex — sessions belong to a repo/directory):
  grouped by **project** (`Candidate::project`, the working directory's leaf), the axis people
  think in. Agent shown within the group when more than one is present.
- **Desktop-collaboration agents** (QoderWork): grouped by **agent**. Their sessions are not
  repo-anchored — cwd is often `$HOME` or meaningless — so a project grouping would manufacture
  junk groups out of noise.
- Within a group, **state** (§5) by sort rather than grouping: growing first, then idle, then
  finished.

The kind is a one-bit fact about the agent, supplied through the adapter seam (a
`TranscriptAdapter` method with a workspace-anchored default) — the same place every other
per-agent fact lives, so a new adapter states it in its own family.

On the leaf-merge case (two checkouts of one repo share a leaf): keep the leaf as the group and
show the full cwd as the group's secondary line. Watch rather than pre-solve — settled as an
implementer's call (§13).

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

**Recognition is extensible, and the extension keeps the argv trap closed.** The built-in
basenames are `claude`, `codex`, `qoderwork`, `qoder`; a wrapper launch (`npx codex`,
`node ./node_modules/.bin/codex`) has an interpreter's basename and is therefore invisible to
them. `$CLAUDE_MONITOR_AGENT_PATTERNS` adds patterns, comma-separated: `basename:<name>`,
`argv:<substring>`, or a bare `<name>` — which means `basename:`, not "either". The bare form has
to be the safe one, because the paragraph above is exactly what an argv substring risks: a loose
`node` or `codex` claims tool shells and helper processes as agents, and every consumer downstream
(the growth proof, the cwd heuristic) then picks among candidates that were never agents. Matching
a command line is available, but it has to be asked for by name. The variable is read ONCE per run:
the check runs for every process in the table on every refresh, and the value cannot change
mid-run.

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
 claude-monitor  (separate crate — §10) — a web service on one loopback port (§11)
   ├── scan       discover::candidates_all(filter)     every agent by default (R1);
   │                                                   incremental by mtime (§8)
   ├── diff       previous scan vs this one            new (R4) + growing (R3)
   ├── card       per-agent title/description          bounded tail read (§4); persisted as
   │                                                   ONE small index file (§3)
   ├── index      main sessions only (§4.2)            cards for every row; MaterializedMeta
   │                                                   read lock-free for VISITED rows (§2, §3)
   ├── liveness   process, only to split idle/finished secondary (§5.1)
   └── serve      index page + hand-off                loopback; view via claude-replay-html;
                                                       serving a visit IS the fold (§3)
```

Row model — the table is the acceptance test for R7, since every source is a listing, a `stat`, a
bounded tail read, or the meta stream:

| column | source | folds? | present |
|---|---|---|---|
| title · description | §4 per-agent card | no — bounded tail | every row |
| project · agent | `Candidate` | no | every row |
| last activity | tree mtime | no | every row |
| state | §5 | no | every row |
| turns · tools · sub-agents | `MaterializedMeta.session_meta` | no | visited rows |
| cost | `MaterializedMeta.tokens` + `metrics::total_cost` | no | visited rows |
| tasks | `MaterializedMeta.tasks` | no | visited rows |

## 8. Refresh

A **poll**, not a watcher. A watcher over N store roots is platform-specific and fails open (a
missed event is a silently stale row); §5 is a diff of two scans, which wants a timer by
construction; and the liveness fallback has no event to subscribe to anyway.

The server re-scans on a floor of ~2 s and serves a cached snapshot in between, so N open tabs
cost one scan. Per-session views keep tailing through the existing pull protocol, untouched.

The scan is **incremental by mtime** — settled by §3's lazy design, which forces it anyway:
a cycle is a `stat` walk, and only sessions whose mtime moved get a card re-derive. A full
per-cycle re-read of anything is exactly the shape the review removed.

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

The per-agent card is an adapter hook (`session_card` on the seam) — resolved in the v4 review
and since SHIPPED (#106): both frontends already show agent-supplied titles through it, so the
monitor consumes an API that exists rather than one designed for it.

## 11. Exposure

**A web service on one loopback port** (owner decision, 2026-08-07): `127.0.0.1` with a stable
default port so the monitor is a bookmarkable place, `--port` to override, and no bind-address
flag in the design.
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

**None block building.** The 2026-08-07 owner review settled the four structural questions
(main-sessions-only overview; per-agent-kind grouping; lazy population, no sweep; a web service
on a loopback port), and the rest are settled below as implementer's calls — recorded so a veto
is one line, not an excavation.

**Settled 2026-08-07** (owner decisions marked ◆, implementer's calls ◇):

1. ◆ **No upfront sweep** (was Q3/Q4's whole context). Lazy population per §3: cards for every
   row, durable entries only for visited sessions. Rejected on startup delay + storage.
2. ◆ **Main sessions only in the overview** (was Q6). Sub-agents are drill-down; a visited
   parent's row still shows child count + running state, which covers the long-running-child
   case for exactly the sessions that can show it.
3. ◆ **Grouping per agent kind** (was Q2's axis): project for workspace-anchored agents, agent
   for desktop-collaboration agents (QoderWork). One adapter-supplied bit.
4. ◆ **A web service at `localhost:<port>`** — stable default port, `--port` override (§11).
5. ◇ **Title staleness** (was Q1): dissolved by lazy — a card re-derives when its transcript's
   mtime moves, at the §8 scan cadence. The turn-count trigger died with the sweep (turns need
   the fold; mtime does not).
6. ◇ **Leaf merge** (was Q2's remainder): keep the leaf as the group key, full cwd as the
   group's secondary line. Watch; two checkouts sharing a group is mildly wrong, not broken.
7. ◇ **Failed reads** (was Q4): no sweep, so no retry loop to guard. A failed card read =
   a stem-titled row; a failed fold surfaces on visit, in front of the user, once.
8. ◇ **Ghost rows** (was Q5): presence comes from the scan, so a deleted transcript's row
   simply vanishes; its card entry is dead weight until the card file's next compaction.
9. ◇ **Configuration** (was Q7): CLI flags only for v1 (`--agents`, `--port`,
   `$CLAUDE_MONITOR_CACHE`); a config file when someone actually asks.
10. ◇ **Cross-frame keyboard** (was Q8): v1 is click-focus only — the rail is plain clickable
    HTML with a filter input; `postMessage` keyboard unification is a nicety deferred until the
    rail exists to want it.

**Resolved by earlier review** — kept as a record of what moved and why:

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

## 14. Cost — the ledger revision (2026-08-12)

Cost used to be a counter like any other: read from a visited entry's meta stream, absent
otherwise. An audit against one real project (lumen) showed why that cannot stand for THIS
number: the rail showed **$120.95 of an actual $2,420.65 — 5%**. Three causes, in size order:
sub-agent rollouts carry ~95% of the project's spend and were rolled up nowhere (they are not
rows — §13.2 — and no row claimed their cost); the main sessions' own cost was gated on visits
and 551 of 556 sessions had never been opened; and usage a rollout reports before naming a
model was priced $0 by the monitor's own per-model re-derivation. Every other counter degrades
gracefully when it is missing — "tbd" on an unvisited row is honest. A COST that silently
reads 5% of reality is not a degraded answer; it is a wrong one, on the one number people
quote out of the page.

So cost moved off the meta stream onto its own ledger (`claude-monitor/src/cost.rs`):

- **Mechanism.** The engine's resumable metrics fold (#14, `MetricsFold`) — a line scan that
  folds only `token_count`-class records and stops at a serializable cursor. Pricing goes
  through the accumulator's `finish()`, so attribution rules (the blank-model bucket claimed
  by the first model named, #16) live in one place and the $0-blank-bucket bug cannot recur.
- **Why this is a carve-out from R7, not a violation.** R7 bans the BLOCK fold, whose cost is
  proportional to a transcript's full content on every load. The metrics fold reads each byte
  once EVER: cursors persist at the monitor's own root (`<cache_root>/costs/`, R5), so every
  later cycle costs a `stat` for a quiet file and the appended bytes for a growing one.
  Measured cold: 837 files / 1.45 GB in ~3 s — and a per-cycle fresh-byte budget (256 MiB)
  spreads even that across a few polls, so the first paint never stalls. No durable entry is
  produced, so §3's no-sweep rule keeps its point: serving is still what writes entries.
- **Sub-agent roll-up.** The adapter surface grew the complement of `store_transcripts`:
  `store_subagent_transcripts` lists `(path, own id, parent thread id)` — same single-line
  head read, same marker as the main-listing exclusion, so no rollout can fall between the
  two listings. The scan prices each sub-agent rollout and chases `parent_thread_id` up
  (a parent may itself be a sub-agent; 64-hop cap, as `family_root`) to the main row, which
  reports `cost` (total) and `costSubs` (the rolled-up share). The overview stays
  main-sessions-only (§13.2); it is the *money* that rolls up, not the rows.
- **The archive.** `~/.codex/archived_sessions` joins the dated tree in both listings — an
  archived session's spend is as real as a live one's, and it is a row at all now.

The numbers stay **equivalent-API dollars** — Codex under a ChatGPT plan bills $0; the figure
is what the same tokens would cost at API list price, and `costPartial` marks a mix containing
a model the price table does not know (a `≥` lower bound, never a guess).

### 14.1 Three scopes, three numbers

The page now shows cost at three scopes, and they are *supposed* to differ — a reader who
sums one against another is comparing different populations, not catching a bug:

- **The detail pane's USAGE** is ONE transcript: the opened session's own tokens, nothing
  rolled in. A root session that spent $70 itself shows $70 here even when its sub-agents
  spent ten times that.
- **The session row** is the TASK TREE: the root transcript plus every sub-agent rollout
  whose `parent_thread_id` chain reaches it (§14's roll-up), reported as `cost` with
  `costSubs` naming the delegated share. The same $70 session with $661 of sub-agent work
  is a $731 row.
- **The group header** is the PROJECT: the sum of its root rows. Sub-agents are already
  inside their roots' rows, so the group adds nothing twice.

A `≥` prefix on any of these is `costPartial` (above) — most commonly a LIVE session whose
newest segment has not yet named a model, so its tokens cannot be priced *yet*. The figure
is a lower bound that catches up on a later poll, never a guess.

### 14.2 A metrics change bumps `FOLD_VERSION` too

The rule used to read "bump on block-output changes." The ledger revision proved it is
broader: a durable entry's meta stream persists the token DELTAS its drain observed, so a
metrics-accumulator fix (the blank-model bucket, the fork-baseline double count) leaves the
WRONG history in every already-written stream. The version gate is what decides whether a
resume trusts that history — with `FOLD_VERSION` unbumped, the fixed binary spliced its
correct increments onto the old binary's inflated base and served the sum as truth: one
audited session showed **7.47 B cache-read tokens (~$1,177) off a 437 MB transcript whose
real usage was 437 M (~$70)**. No amount of redeploying fixed code changes a cached number
the code is told to trust. The bump (v6) forces a cold refold under the fixed accumulator;
the byte-identical gate catches block-output changes, but a metrics change is invisible to
it — the reviewer has to remember this rule, which is why it is written here.

## Rejected

| shape | why |
|---|---|
| Fold every transcript on index load | O(bytes) per page load — the entire reason #96 came first |
| The background sweep itself (v2–v4) | rejected by the owner 2026-08-07: a first launch that folds a machine's history is a long delay, and its durable entries are GBs of storage mostly for sessions nobody opens. Presence and recognition need no fold (cards); richness follows visits (§3) |
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
