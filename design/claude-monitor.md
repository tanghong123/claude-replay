# Design: `claude-monitor` — every session on the machine, over HTTP

> **v2 — for review.** Revised against six requirements from the owner (§1), three of which
> changed the design rather than extending it: its own cache (§3) dissolves v1's biggest open
> question, growth detection (§5) demotes v1's weakest mechanism, and "a separate repo one day"
> (§9) turns out to need two concrete changes in `claude-replay` itself.
>
> Unblocked by #96, shipped v1.35.0–v1.39.0. Read §2 and §3 first; the rest follows.

Today's viewer answers *"show me this session"*. This answers **"what is happening on this
machine"** — one page over every agent's store, with enough per row to triage, and a click
through to the existing per-session view.

---

## 1. Requirements

The first six are the owner's, stated 2026-08-06; R7–R9 are mine, carried from v1.

| | |
|---|---|
| **R1** | See sessions from **every agent `claude-replay` can parse**, by default. User-configurable to narrow. |
| **R2** | **Organization and description** per session — agent-specific where that helps — good enough for quick identification. |
| **R3** | Automatically detect which sessions are **growing**. |
| **R4** | Automatically detect **new sessions** appearing. |
| **R5** | A **completely separate cache**, not `~/.cache/claude-replay`, owned by the monitor. |
| **R6** | A **separate crate**, and plausibly a separate **repo** later, depending on `claude-replay` as a library. |
| **R7** | The index must **not fold transcripts** on a page load. |
| **R8** | Read-only, loopback only (§10). |
| **R9** | It must never block or degrade a running `claude-replay`. |

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
> §4 is the one deliberate exception, and it is bounded rather than a fold.

### 2.1 Reading is lock-free, and that is a property not a shortcut

The index reads its stream **without taking the entry's lock**, while a writer may be appending.
Safe by construction: the stream is append-only so a reader never sees a rewritten prefix;
`MetaReader` drops a torn trailing line, which is exactly what a concurrent partial append looks
like; so the worst case is a row one commit stale for a moment. Taking the lock would let the
monitor deny sessions to itself (§9) — and R9 forbids anything of that shape.

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

## 4. The session card (R2) — organization and description

Recognition is the point of the index, and the first user prompt is a poor label: it says what a
session *started* as, not what it is. Probing real transcripts found much better material, and it
is **agent-specific**, exactly as R2 anticipated.

Claude Code writes these line types, and rewrites them as the session evolves:

| line | field | example (real) |
|---|---|---|
| `custom-title` | `customTitle` | `"Project status"` — the user's own name for it |
| `ai-title` | `aiTitle` | `"Check project status"` — the agent's generated title |
| `last-prompt` | `lastPrompt` | `"status of the project"` — the most recent prompt, not the first |

Codex has none of them; it carries `session_meta` / `turn_context` instead. So the derivation is a
**per-agent seam**, not a shared function.

**Precedence** for the display name: `customTitle` → `aiTitle` → first-prompt snippet
(`Candidate::snippet`, which always exists). `lastPrompt` is a *separate* field — "what it is doing
now" — and is the most useful thing on the row for a growing session.

### 4.1 A bounded tail read, not a fold

These lines are appended repeatedly as the session evolves, so the current value is the **last**
occurrence — a tail read, not a head read. The monitor reads a bounded window off the end (the
same shape `jdi::inflight_tool_in_tail` already uses, 256 KiB) and takes the last of each.

That is the one place the monitor touches a transcript on the index path, and it is deliberate:
O(window), not O(bytes). It is also why §2's rule is stated as "never *fold*" rather than "never
*open*".

**Open (§12 Q1):** whether these belong in the meta record instead, which would make the index
purely a metadata read again. Argument for: purity, and the sweep already folds the transcript
once so it could capture them for free. Argument against: they are *gauges that change*, so a
stale cached title would be wrong for exactly the growing sessions where the title matters most —
and the tail read is cheap enough that caching it buys little.

### 4.2 Organization

Grouping, in priority order, with the group being a display concern rather than a stored one:

1. **project** (`Candidate::project`, the working directory's leaf) — the axis people actually
   think in;
2. **agent** within a project, when more than one is present;
3. **state** (§5) surfaced by sort rather than grouping: growing first, then idle, then finished.

**Open (§12 Q2):** whether the project leaf is enough. Two checkouts of the same repo in different
directories share a leaf name and would merge into one group. The full cwd disambiguates but is
too long to show.

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

## 6. Shape

```
 claude-monitor  (separate crate — §9)
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

## 7. Refresh

A **poll**, not a watcher. A watcher over N store roots is platform-specific and fails open (a
missed event is a silently stale row); §5 is a diff of two scans, which wants a timer by
construction; and the liveness fallback has no event to subscribe to anyway.

The server re-scans on a floor of ~2 s and serves a cached snapshot in between, so N open tabs
cost one scan. Per-session views keep tailing through the existing pull protocol, untouched.

**Open (§12 Q3):** whether the scan is incremental (only sessions whose mtime moved) or a full
re-read per cycle. Full is simpler and fine at ~100; unmeasured at 1000.

## 8. Interaction with a running viewer (R9)

- **Indexing** — its own cache root (§3) plus lock-free reads (§2.1). No contention is possible.
- **Opening a session nobody serves** — the monitor stands up (or reuses) `start_server`.
- **Opening a session someone already serves** — `existing_server(root, sid)` reads the holder's
  published port from the lock and the monitor **redirects there** instead of standing up a
  duplicate server, fold and copy. This is #96's rendezvous, and the monitor is the consumer it
  was really built for.

## 9. Where it lives, and what `claude-replay` must expose first (R6)

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
| **`Presentation` is a closed enum** (`Tui`/`Html`) | the monitor's cache needs its own namespace, and a third-party frontend cannot mint one | make it an **open interned id**, exactly as `Agent` already is: `pub struct Presentation(&'static str)` with `TUI`/`HTML` as associated constants. Same argument, same shape, and it is a small change. |
| **The liveness helpers live in the root binary crate** — `jdi::latest_tree_activity`, `jdi::inflight_tool_in_tail` are reachable (`pub mod jdi`) but only by depending on `claude-replay` itself, which pulls in clap, ratatui, crossterm and the whole viewer | a separate repo would link a terminal UI to `stat` a file | move both **down into `claude-replay-core`** (they are transcript-shaped, agent-neutral, and read files — core's exact remit), and have `jdi` re-export them |

Neither is large. Both are worth doing before the monitor exists rather than after, because both
are API decisions and the second run of an API is the expensive one.

**Open (§12 Q4):** whether the per-agent card (§4) should be a hook on `TranscriptAdapter` — which
would make it available to every consumer and keep agent knowledge behind the one seam — or a
monitor-side trait, which keeps a display concern out of the parser. I lean toward the adapter
hook, because "what is this session called" is a fact about the agent's format, not about the
monitor.

## 10. Exposure

**Loopback only.** `127.0.0.1`, as `--html` does today, with no bind-address flag in the design.
The page aggregates every session on the machine — prompts, file contents, tool output, working
directories — so a careless bind exposes the machine's whole body of work, not one session. If
remote access is ever wanted it is a separate decision with its own review, and the honest
mechanism is an SSH tunnel, not a flag.

Read-only is structural, not a setting: the monitor links nothing that writes a transcript or
injects input. #99 §4 set out what productising injection would require — per-session consent at
the time, visibility in the target, local-only, refuse-by-default. A read-only monitor needs none
of it and should keep needing none of it.

## 11. What this is not

- Not a scheduler or supervisor — `agent-jdi` owns unattended runs.
- Not a second renderer; the session view is the existing HTML frontend.
- Not multi-machine. Everything assumes one filesystem and one process table.
- Not an auth boundary. §10's answer to "who can see this" is "whoever is on this machine".

## 12. Open questions

1. **Should the session card live in the meta record instead of a tail read?** (§4.1) Purity vs
   staleness — a cached title is wrong for exactly the growing sessions where it matters most.
2. **Is the project leaf the right grouping key?** (§4.2) Two checkouts of one repo share a leaf
   and would merge; the full cwd disambiguates but is too long to show.
3. **Scan strategy at scale.** (§7) Full re-read per cycle vs incremental by mtime; unmeasured at
   ~1000 sessions.
4. **Is the card a `TranscriptAdapter` hook or monitor-side?** (§9) I lean adapter hook.
5. **What is the sweep's completion signal?** A session whose fold *fails* (corrupt transcript,
   unknown agent) must not be retried every cycle forever. Some negative-cache with a reason, but
   its shape and its invalidation are undesigned.
6. **A session whose transcript was deleted but whose index entry survives.** A ghost row is
   arguably useful history and arguably confusing. Undecided.
7. **Sub-agent rows.** A session with 40 sub-agents has 40 child transcripts. List them, nest
   them, or leave them to drill-down? Leaning drill-down, but a long-running child is exactly what
   you would want to see from the index.
8. **Configuration surface for R1.** A flag, a config file, or both — and whether "which agents"
   is the only axis worth configuring (versus which store roots, or which projects).

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
| Bind non-loopback behind a flag | §10 — the aggregate is the machine's whole body of work; a flag is too small a gesture |
