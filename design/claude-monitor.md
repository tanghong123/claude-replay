# Design: `claude-monitor` — every session on the machine, over HTTP

> **v1 — for review.** Unblocked by #96 (the durable session cache), which shipped in
> v1.35.0–v1.39.0. Read §2 first: one constraint decides most of the rest. §11 lists what I could
> not settle alone.

Today's viewer answers *"show me this session"*. This answers **"what is happening on this
machine"** — one page listing every session in every registered agent's store, with enough per
row to triage, and a click through to the existing per-session view.

---

## 1. Requirements

| | |
|---|---|
| **R1** | List every session in every registered agent's store, machine-wide. |
| **R2** | The index must **not fold transcripts**. A page load is O(sessions), not O(bytes). |
| **R3** | Each row carries what triage needs: project, agent, last activity, turns, tools, sub-agents, cost, and **running / idle / finished**. |
| **R4** | Clicking a row opens the **existing** HTML session view — the monitor never forks a second renderer. |
| **R5** | Read-only. It never writes a transcript, never injects input, never takes an action on a session. |
| **R6** | Loopback only, no exceptions without an explicit decision (§9). |
| **R7** | It must not fight a running `claude-replay`. Opening a session someone is already serving hands off (#96's rendezvous), and the index never blocks on a lock. |

## 2. The one constraint: the index reads metadata, never transcripts

#96's record is split so that **Part I — the delta half — is frontend-agnostic and
fold-free**: turns, tools, sub-agent lifecycle, `user_times`, per-model tokens, `extra`,
task ops, cwd, span. A reader deserialises those records and folds them with
`MaterializedMeta::push`. No adapter, no `BV`, no transcript, no `Replayer`.

That is the whole reason this is cheap. Measured on the largest real session here:

| | |
|---|---|
| transcript | 107 MB |
| its `meta.jsonl` | **0.43 MB**, 711 records |
| cold fold of the transcript | 764 ms |
| resume from the cache | 49 ms |

So an index row costs a sub-millisecond read of a small append-only file. **A hundred sessions
is a directory walk plus a hundred small reads** — not a hundred folds.

> **The rule that keeps it true.** If the monitor ever wants to parse a transcript to render
> something on the index, that is a signal the *record* is missing a field — not that the reader
> needs a fold. Adding the field to Part I is the fix; folding is not.

### 2.1 Reading is lock-free, and that is a property not a shortcut

The index reads `meta.jsonl` **without taking the entry's lock**, while a writer may be
appending to it. That is safe by construction rather than by luck:

- the stream is **append-only**, so a reader never sees a rewritten prefix;
- `MetaReader` **drops a torn trailing line**, which is exactly what a concurrent partial append
  looks like;
- the worst case is therefore *missing the most recent drain*, which for an index is a row that
  is one commit stale for a moment.

Taking the lock instead would make the monitor deny sessions to the viewer, which R7 forbids.

### 2.2 The index does not need alignment

`admit`'s alignment exists so a **resume** never trusts a record the content stream cannot
corroborate. The index resumes nothing — it displays counters. So it folds the records it finds
and skips the content stream entirely. The exposure is one drain of over-count on a torn tail;
the alternative is reading the frontend's `BV` table, which would drag `BV` decoding into a
reader that R2 wants free of it.

**When a checkpoint is present, start there** (§6.6 of the cache design): adopt the newest one
and fold only the tail. That makes a row O(records since the last checkpoint) rather than
O(records).

## 3. The cold-index problem — the real gap

§2 is true **only for sessions that have a durable entry**. An entry exists once some frontend
has *opened* that session. A machine-wide monitor is precisely the tool you point at sessions
you have not opened.

So rows come in two grades:

| grade | source | cost | shows |
|---|---|---|---|
| **indexed** | `<root>/<presentation>/<session>/meta.jsonl` | a small read | everything in R3 |
| **unindexed** | `discover::candidates_all()` alone | a `stat` + a bounded head read | path, agent, project, first-prompt snippet, mtime, liveness |

`Candidate` already carries `path`/`agent`/`project`/`snippet`/`mtime`/`cwd_affinity` without a
fold, so an unindexed row is not blank — it is missing the *counters* (turns, tools, cost), not
its identity.

**Filling them in is the open design decision (§11 Q1).** Three shapes, and I do not think the
answer is obvious:

- **(a) Lazy.** A session becomes indexed the first time someone opens it. Zero background cost;
  the index is permanently partial for sessions nobody visits.
- **(b) Eager sweep.** A background worker folds unindexed sessions one at a time, oldest-cheapest
  first, and rows fill in. Costs one full fold per session **once, ever** — after that the
  durable cache carries it. On this machine that is ~20 sessions × ~0.8 s ≈ 16 s of background
  work, spread out, and then never again.
- **(c) On demand.** The row shows an "index" affordance; the user pays for what they want.

My inclination is **(b) with a strict budget** — one session at a time, lowest priority, skipped
entirely when a session already has a live writer — because it converges to a complete index and
the cost is paid once per session for the life of the cache. But it is the one place the monitor
does real work, and it deserves the review.

### 3.1 Which presentation's entry does the index read?

Entries are keyed `<presentation, session>` — `tui/…` and `html/…` are separate directories. Part
I is identical in both, because it is frontend-agnostic. So the index reads **whichever entry
exists**, preferring the one with more records, and does not care which frontend wrote it.

The per-session *view* is different: opening a session in the browser goes through the HTML
presentation and its lock, which is where the rendezvous hand-off applies (§7).

## 4. Liveness — three states, from #99

`design/session-liveness-probe.md` settled this empirically; the monitor should not re-derive it.

| state | rule |
|---|---|
| **Finished** | no live agent process for this session |
| **Running** | a process **and** (fresh tree write **or** a tool in flight) |
| **IdleAlive** | a process, but quiet |

Two signals are necessary and one looks redundant but is not:

- **live process** — matched on the **basename of `argv[0]`**. Nothing else separates *finished*
  from *idle*.
- **tree mtime** — root transcript plus every child under `<stem>/subagents/`
  (`jdi::latest_tree_activity`). The only continuous progress signal; strictly stronger than the
  root's mtime alone.
- **in-flight tool** — a `tool_use` with no `tool_result` in the tail
  (`jdi::inflight_tool_in_tail`). It never fired in the #99 sample, and it must still ship: an
  agent blocked in a long tool call writes *nothing* anywhere, so its mtime ages past any
  threshold while it is maximally busy. A correctness signal for a rare state.

### 4.1 The two traps, and why they are worth restating here

Both lose agents **silently**, and #99 hit one of them:

- **Do not match argv anywhere.** An agent's own tool shells carry `claude` in their argv, so a
  broad match makes every shell look like an agent.
- **Do not read `comm` from a bulk `ps` listing.** The multi-column form truncates it
  (`/Users/hong/.local/bin/claude` → `/Users/hong/.loc`), dropping every agent launched by
  absolute path. This cost the #99 prototype 2 of its 4 resolvable sessions before it was found.

### 4.2 Process → session is the weak link

This is the part I would not call solved. `--resume <uuid>` in argv is the reliable link, but it
exists only for *resumed* sessions — **7 of 11 live agents in the #99 sample carried no id**.
Measured fallbacks:

- **open fd** (`lsof`): works for Codex (holds its rollout open), **fails for Claude** (appends
  and closes).
- **cwd → project slug**: the process cwd maps to Claude's store directory by replacing `/` with
  `-`; the newest transcript there is the session. Verified on three processes, but "newest in
  this directory" is a heuristic, not an identity.

Proposal: argv id when present → else cwd+recency → **cross-check the transcript's recorded
`session_id`** either way, and mark the row's liveness *unconfirmed* when the check fails rather
than asserting a state. A wrong "Running" badge is worse than an honest "probably".

## 5. Shape

```
 claude-replay-monitor  (new crate — §8)
   ├── scan        discover::candidates_all(None)         → every session, no fold
   ├── index       read <root>/<pres>/<sid>/meta.jsonl     → MaterializedMeta per session
   │                 (lock-free, checkpoint-seeded — §2)
   ├── liveness    ps + tree mtime + in-flight tail        → Finished / Running / IdleAlive
   ├── serve       an index page + a per-session hand-off  → loopback HTTP
   └── view        claude-replay-html::start_server        → the EXISTING session view (R4)
```

Row model, and where each field comes from — the table is the acceptance test for R2, because
every source is either a `stat`, a bounded head read, or the meta stream:

| column | source | fold? |
|---|---|---|
| project · agent · snippet | `Candidate` | no |
| last activity | tree mtime | no |
| state | §4 | no |
| turns · tools · sub-agents | `MaterializedMeta.session_meta` | no |
| cost | `MaterializedMeta.tokens` + `metrics::total_cost` | no |
| tasks | `MaterializedMeta.tasks` | no |

## 6. Refresh

The index is a **poll**, not a watcher. Reasons, in order: a watcher over N agent store roots is
platform-specific and fails open (a missed event is a silently stale row); the data is already
cheap enough that polling is not the cost; and the liveness probe has to run on a timer anyway
because "is that pid alive" has no event to subscribe to.

Cadence: the page polls the server; the server re-scans on a floor of ~2 s and serves a cached
snapshot in between, so N open tabs cost one scan. Per-session views keep tailing through the
existing pull protocol, untouched by this.

**Open (§11 Q3):** whether the scan is incremental (only sessions whose mtime moved) or a full
re-read each cycle. Full is simpler and probably fine at 100 sessions; I have not measured it at
1000.

## 7. Interaction with a running viewer (R7)

Three cases, all already handled by machinery that exists:

- **Index vs anything** — lock-free reads (§2.1). The monitor never blocks a viewer, ever.
- **Open a session nobody is serving** — the monitor stands up (or reuses) its own
  `start_server` and serves it.
- **Open a session someone is already serving** — `existing_server(root, sid)` reads the holder's
  published port out of the lock and the monitor **redirects there** rather than standing up a
  duplicate server, fold and copy. This is #96's rendezvous, and the monitor is the consumer it
  was really built for.

A TUI holding that session's `tui/` lock is irrelevant: locks are per `<presentation, session>`,
so a terminal viewer and the monitor never contend.

## 8. Where it lives

**A new crate, `claude-replay-monitor`**, depending on `claude-replay-html` for the session view.

Not a mode of the html crate. The html crate is a *frontend* — it renders one session's blocks.
The monitor is an *application*: it scans stores machine-wide, probes processes, and routes
between servers. Putting process probing and store-wide discovery inside the presentation layer
would break the property §2 of the architecture doc enforces by dependency graph, and would make
`--dump-html` link a process scanner.

Binary: `claude-monitor`, alongside `claude-replay` and `agent-jdi` — three binaries, one
workspace, which is the shape `agent-jdi` already established.

## 9. Exposure

**Loopback only.** `127.0.0.1`, exactly as `--html` does today, and the design does not include a
bind address flag. The page aggregates every session on the machine — prompts, file contents,
tool output, working directories — so the blast radius of a careless bind is the whole machine's
work, not one session.

If remote access is ever wanted, it is a separate decision with its own review, and the honest
mechanism is an SSH tunnel rather than a flag.

Read-only is likewise structural, not a setting: the monitor links nothing that writes a
transcript or injects input. #99 §4 spelled out what productising injection would require
(per-session consent at the time, visibility in the target, local-only, refuse-by-default); a
read-only monitor needs none of it, and should keep needing none of it.

## 10. What this is not

- Not a scheduler or supervisor — that is `agent-jdi`, and it already owns unattended runs.
- Not a second renderer (R4).
- Not multi-machine. Everything here assumes one filesystem and one process table.
- Not an auth boundary. §9's answer to "who can see this" is "whoever is on this machine".

## 11. Open questions

1. **How do unindexed sessions get indexed?** (§3) Lazy, eager sweep with a budget, or on
   demand. I lean eager-with-a-budget; it is the only place the monitor does real work, and the
   choice sets whether the index is ever complete.
2. **Is "every agent" right, or Claude only?** The task is titled *claude*-monitor, but
   `candidates_all(None)` already spans Claude, Codex and QoderWork, and the meta stream is
   agent-neutral. Filtering to Claude would be a deliberate narrowing, not a simplification.
3. **Scan strategy at scale.** (§6) Full re-read per cycle vs incremental by mtime. Needs a
   measurement at ~1000 sessions, which I have not done.
4. **Does the index need cost at all?** It is the one column that is a *sum over models* and the
   one most likely to be wrong-looking after a partial index (a half-indexed session under-reports).
   Possibly show it only for indexed rows, blank otherwise — which is the honest rendering but
   invites "why is this blank".
5. **What happens to a session whose transcript was deleted but whose cache entry survives?** The
   GC sweeps by idle age, not by source existence. A ghost row is arguably useful (it is history)
   and arguably confusing. Undecided.
6. **Sub-agent rows.** A session with 40 sub-agents has 40 child transcripts. Does the index list
   them, nest them under the parent, or ignore them and let the session view handle drill-down?
   Leaning ignore-and-drill-down, but a long-running child is exactly the thing you would want to
   see from the index.
7. **Liveness for non-Claude agents.** §4's process matching is Claude-shaped. Codex's `lsof` fd
   trick works where Claude's does not; QoderWork is unmeasured. The three-state model should
   hold, but the probes are per-agent and that smells like an adapter seam I have not designed.

## Rejected

| shape | why |
|---|---|
| Fold every transcript on index load | O(bytes) per page load; the entire reason #96 came first |
| Read the frontend's `BV` table for counters | drags `BV` decoding into a reader R2 wants free of it, and makes the index presentation-specific for no gain |
| Take the entry lock while indexing | would let the monitor deny sessions to the viewer (R7) |
| A filesystem watcher instead of polling | platform-specific, fails open on a missed event, and liveness needs a timer regardless |
| A second HTML renderer tuned for "summary" views | R4 — two renderers drift, which is the same argument that keeps one classifier and one fold |
| Bind non-loopback behind a flag | §9 — the aggregate is the whole machine's work; a flag is too small a gesture for that |
