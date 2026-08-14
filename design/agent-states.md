# #194 — Agent states: busy / wait / idle, observed, dumped locally

> **BUILT** (v1.77.0): engine `state` module (`derive_state`/`tail_pulse`/`StateEvent`),
> the liveness names upgrade, the `turn_ended`/`ends_with_question` adapter hooks, and
> the monitor's state pass writing `state/events.jsonl` + `state/current.json`.
> Originally a proposal studying `~/personal/claude-toolbox/notifications/` (the prior
> hook-based system); the study and rationale below are kept as written. Companion reading:
> `design/session-liveness-probe.md` (#99 — the verified process/attachment mechanisms
> this builds on) and `design/claude-monitor.md` (the scan cycle and R-rules it must
> respect).

## 1. The three states (the contract)

| State | Meaning | Reader's action |
|---|---|---|
| **busy** | More progress is coming WITHOUT user attention. | None — walk away. |
| **wait** | Blocked by a SIMPLE user action (a modal: permission, a question dialog, plan approval). One tap resumes progress. | Do the tap. |
| **idle** | All work finished — or the session needs human attention to UNBLOCK (turn ended with a question, process died mid-work, stalled). | Read the context, decide. |

The line between **wait** and **idle** is *modality*: wait means the agent is mid-turn
behind a dialog and one action resumes it; idle means the turn (or the process) is over
and whatever happens next starts with the human. A turn that ended with a prose question
is **idle** with context `question` — attention-needing, but not a modal block.

Every state carries **context** (§5) — the old system's `busy: Bash` idea, taken further.

## 2. What the old system taught (and where it broke)

The toolbox pipeline: Claude Code **hooks** write `/tmp/claude-status/<sid>.json`
transitions (`UserPromptSubmit→busy`, `PreToolUse→busy:<tool>`,
`Notification[permission_prompt]→waiting_permission`, `Stop→idle`), an **external
observer** launchd agent reconciles every 45s against the transcript tail
(`tail -400 | jq`: last assistant `stop_reason=end_turn` ⇒ waiting, else busy), and
fan-out layers (ntfy, iCloud, widgets) consume the files.

What it got right — and this proposal keeps:
- **The transcript is ground truth; hooks are hearsay.** Its own README concedes hooks
  drop events; the observer existed because the transcript never lies.
- **Local JSON files as the consumable seam.** CLI, menu bar, phone — all thin readers.
- **Sticky project identity + fresh branch** per event; dedup keyed on file signatures.

Why it broke, mapped to the user-reported failures:

1. **"Idle" vs "waiting for tools/sub-agents" indistinguishable.** Hooks know a tool
   STARTED (`PreToolUse`) but a missed `PostToolUse`/`Stop` leaves no way to ask "is
   anything still open?". The observer's `stop_reason` read sees only the last assistant
   line — a parent whose sub-agent is running looks identical to a parent that finished.
   *Root cause: no in-flight accounting.* The engine has it: `inflight_tool_in_tail`
   pairs `tool_use` ids with `tool_use_id` results across the tail (sub-agent spawns
   included — a running child's spawning tool is unresolved in the ROOT tail), for both
   Claude and Codex shapes.
2. **Stuck in busy.** Hook state with a missed `Stop` has no self-correction except the
   45s observer, and the observer's age fallback (`waiting_stale` after 900s) fires on
   working sessions too — the #82 lesson: during a long `cargo build`, the whole
   transcript tree sits untouched, so quiet ≠ done. *Root cause: state that is asserted,
   never re-derived.* Here, every state is re-DERIVED from observation each scan tick;
   there is nothing to get stuck.
3. **Waiting for input without AskUserQuestion** (the hard one). No hook fires for a
   prose question; `Notification[idle_prompt]` arrives ~60s late and was disabled as
   noise. The observer's `end_turn` catches "turn over" but cannot tell a completion
   report from a question. *Root cause: the signal is in the CONTENT, and the old reader
   had no parser.* The monitor has the real decoder.

## 3. What the monitor already owns (the unfair advantages)

All of these exist today; the state machine is mostly wiring:

- **The parser.** Bounded tail decode through the adapter seam — not `jq` guesses:
  pending `tool_use` vs joined results, `AskUserQuestion`/`ExitPlanMode` identified by
  `TranscriptAdapter::tool_is_interactive` (#21), tool failures by `is_error` (#23),
  `SubAgent` blocks with running/completed status, `QueueEvent` (a QUEUED user prompt —
  decisive below), compaction markers, task ops.
- **Liveness** (`core::liveness`, per the #99 probe doc): `latest_tree_activity`
  (tree-aware mtime including sub-agent files) and `inflight_tool_in_tail` (256 KiB
  window, lossy-decode on purpose — the #82 hardening).
- **Process attribution** (#99/#145/#146): which LIVE agent process drives which
  session — open-fd links for Codex, env/cwd + growth-proof for Claude-family
  (`ps eww`, `TMUX_PANE`), and the §3 capability matrix for whether input could even be
  injected. The monitor's scan rows already carry `growing` (with a one-minute hold
  against flapping) and terminal attachment.
- **The scan cycle.** A stat-walk over every store already runs per tick; cards already
  do bounded tail reads. R7 allows exactly this shape: bounded tails, never the block
  fold, on the index path.

## 4. The state machine

Per session, per scan tick, derive — never carry — the state from four signals:

| | Signal | Source | Cost |
|---|---|---|---|
| S1 | Growth: transcript tree grew since last tick | scan stat + `latest_tree_activity` | already paid |
| S2 | In-flight tools: unresolved calls in the tail, each with name + `tool_is_interactive` | extended `inflight_tool_in_tail` → `inflight_tools_in_tail() -> Vec<PendingTool>` | one 256 KiB read on changed/undecided sessions |
| S3 | Process: attributed agent process alive; does it have live CHILD processes (a tool executing) | existing attribution + one `ps` children query per tick for pending-tool sessions only | one `ps` per tick, batched |
| S4 | Tail semantics: last conversational record (real user vs assistant end-of-turn), final assistant text, queued prompts, last result's `is_error` | bounded tail decode (the card read, slightly widened) | bounded |

Decision procedure (first match wins):

```
1. process dead (no attributed live process)
     → idle    · context: exited          (+ "mid-work" if S2 pending — needs attention)
2. S2 has a pending INTERACTIVE tool (AskUserQuestion / ExitPlanMode / adapter-declared)
     → wait    · context: question | plan-approval   (+ the question text, first line)
3. S4 shows a queued user prompt (QueueEvent unconsumed in tail)
     → busy    · context: queued-prompt   (the user already answered; progress resumes)
4. S1 grew within GROWING_HOLD (60s, the existing hysteresis)
     → busy    · context: tool:<name> if S2 pending, else subagents:<n> if running,
                 else thinking/streaming
5. S2 has pending NON-interactive tools
     → if the agent process has live child processes → busy · context: tool:<name> running
       else after PERMISSION_QUIET (~20s of no growth)
            → wait · context: permission:<tool> (CONFIDENCE: inferred — see §6)
6. S4 last record is an assistant END of turn (no pending tools, no running sub-agents)
     → idle    · context: question  if the final text is interrogative (§6),
                 error     if the last tool result before the end was is_error,
                 done      otherwise
7. S4 last record is a real user prompt, no growth yet
     → busy    · context: starting   (API call in flight)
       … but if it stays this way past STALL_AFTER (10 min) with no children
       → idle · context: stalled     (needs attention to unblock)
```

Rules the old system's failures dictate:

- **Nothing is sticky.** Every tick re-derives from the file + process; "stuck busy"
  requires a stuck DERIVATION, and rule 5's child-process check plus rule 1's process
  check bound every hang: a tool that runs for an hour stays busy (its child is alive —
  correct, the old `waiting_stale` false-positive); a tool whose process DIED goes idle
  with `exited mid-work`.
- **Quiet ≠ done** (#82): rule 4's growth window never demotes past rules 5's pending
  accounting — a silent `cargo build` is busy via its live child even when the tree is
  untouched for ten minutes.
- **A queued prompt beats everything below it**: the user has already acted; showing
  wait/idle would send them back to a session that needs nothing.

### Hysteresis

State transitions publish only after the derivation is stable for one tick (except
into `wait`/`idle` from rule 1–2, which are immediate — they are the states someone is
waiting to hear about). The existing one-minute `growing` hold already absorbs the
model's think-pauses; `PERMISSION_QUIET` absorbs the pre-permission write burst.

## 5. The context payload (the "more context")

```json
{
  "v": 1,
  "ts": "2026-08-14T23:59:59Z",
  "sid": "82c4d0fd-…",
  "agent": "claude",
  "cwd": "/Users/hong/code/agent-metrics",
  "title": "Charge one API call once",
  "state": "wait",
  "prev": "busy",
  "reason": "question",
  "detail": "Should the ledger version include FOLD_VERSION?  (AskUserQuestion, 2 options)",
  "since": "2026-08-14T23:58:41Z",
  "attribution": { "pid": 81719, "tmux": "%3", "controllable": true },
  "extra": { "pending_tools": ["AskUserQuestion"], "subagents_running": 0 }
}
```

`detail` is the human line the old phone pushes wanted: the question's first line, the
pending tool + target, the final prose sentence for `idle·question`, the error text for
`idle·error`. `attribution` comes from the #99 probe and tells a consumer whether a
"jump to tmux pane" affordance is even possible — the safety boundary of §4 of the
probe doc still applies to anyone acting on it.

## 6. The two honest heuristics (flagged as such)

- **`wait · permission`** (rule 5): a pending non-interactive tool + no child process +
  quiet. Claude Code's permission dialog writes NOTHING to the transcript, so this is
  inference, not observation. The payload carries `"confidence": "inferred"`; consumers
  can render it softer. **Resolved (owner):** no CPU-delta hardening for now — an
  in-process MCP tool (never a child) may misread as `wait · permission` while it runs;
  the confidence flag is the mitigation, simplicity wins. Adapters declaring
  never-prompting tools stays available as a later refinement.
- **`idle · question`** (rule 6): interrogative-final-text detection — ends with `?`, or
  a last-paragraph match on offer/confirm shapes ("let me know", "shall I", "want me
  to", "which of"). Cheap, language-dependent (CJK question marks included), and
  honest: it only refines idle's CONTEXT, never flips busy/wait, so a miss costs a
  softer notification, not a wrong state. **Resolved (owner):** this IS an adapter
  hook from day one, in the #21 mold — a defaulted
  `TranscriptAdapter::ends_with_question(final_text)` whose default is the generic
  heuristic above; agents with distinctive turn-closing formats override.

## 7. The dump: files other tools consume

Everything lands under the monitor's own root (R5): `~/.cache/claude-monitor/state/`.

- **`events.jsonl`** — append-only state TRANSITIONS, one JSON object (§5 schema) per
  line. No heartbeats, no repeats: a session that stays busy for an hour writes
  nothing. Rotation: at 4 MiB rename to `events.jsonl.1` (one generation kept) —
  consumers that tail get a clean break, and a day of heavy multi-agent work measures
  in tens of KiB.
- **`current.json`** — the full snapshot `{ "scanned_at": …, "sessions": [ … ] }`,
  atomically replaced when anything changed, `scanned_at` refreshed every tick either
  way — the monitor's own heartbeat, so a consumer can tell "all quiet" from "monitor
  gone".

Consumers replace the ENTIRE hook stack of the old system: the desktop notifier, the
ntfy push, the SwiftBar plugin and the phone widget become `tail -f events.jsonl` (or
an `fswatch` one-liner) plus their existing render code — no hooks in
`~/.claude/settings.json`, no launchd observer, no per-session `/tmp` files, and every
agent the monitor understands (claude, codex, qoder, qoderwork) is covered by the same
stream, not just the one with hooks. `/api/fleet`-style aggregation across machines
stays out of scope here; monitor-fleet can proxy `current.json` later if wanted.

## 7.5 Where the code lives

The monitor owns the loop and the files; the general crates expose exactly four things,
split by the #21/#22/#23 rule — agent vocabulary behind the adapter seam, agent-free
machinery in the engine, OS probing never below the monitor:

| Piece | Crate | Shape |
|---|---|---|
| `AgentState`/`StateReason`/`StateSignals` + `derive_state()` (the §4 table, pure) + hysteresis constants | engine | new `state` module — unit-testable with no OS, no store; reusable by other consumers (e.g. agent-metrics over historical transcripts) |
| `StateEvent` (the §5 schema, serde) | engine | beside the vocabulary, so every consumer deserializes against the type the monitor serializes |
| `inflight_tools_in_tail(path) -> Vec<InflightTool{id, name}>` | core `liveness` | upgrade of the bool (which stays as a wrapper); the name rides the same field-level scan. The monitor joins names against the EXISTING `tool_is_interactive` (#21) |
| `tail_pulse(adapter, path) -> TailPulse` + defaulted `TranscriptAdapter::turn_ended(raw_line) -> Option<bool>` | engine (+ two one-field overrides in agents) | the generic pulse runs the adapter's own `line_preprocessor`/`decode_line` over a bounded tail — last-record kind, final text, queued prompts, `is_error` (#23) all fall out; only END-OF-TURN is agent vocabulary (Claude `stop_reason`, Codex `task_complete`), hence the hook. `None` default keeps third-party adapters compiling and degrades rule 6 to the growth/inflight signals |

Monitor-only, deliberately: growth clocks (already per-row), process attribution (#99's
`ps eww`/fd machinery in `index.rs` — machine-level, not transcript-level), the rule-5
children probe (pure `ps`), hysteresis STAGING (state held across ticks is scan-loop
state), and all writing (append, rotation, `current.json` atomicity). Net new code in
the `agents` crate: the two `turn_ended` overrides — the seam audit stays happy.

## 8. What this deliberately is not

- **Not a supervisor** (§7 of the monitor doc): it never starts, stops, or nudges a
  session. `wait` is a fact, not a trigger.
- **Not hook-based**: zero writes into agent configs; sessions on this machine are
  covered the moment they write a transcript.
- **Not an injection surface**: `attribution.controllable` reports the §3 capability;
  acting on it is a different feature behind the §4 safety boundary.

## 9. Open questions

1. ~~In-process MCP tools and the CPU-delta hardening~~ — **resolved: skip it** (§6);
   the confidence flag carries the ambiguity.
2. ~~Should `idle · question` be an adapter hook~~ — **resolved: yes** (§6), shipped as
   a defaulted `ends_with_question` with the generic heuristic as the default body.
3. Event retention: is one 4 MiB generation enough, or should events also fold into a
   daily file for history? (The consumer that wants history can also just keep its own.)
4. Codex parity: rules 2 and 5 need Codex's interactive-tool vocabulary (none declared
   today — #21 left it `false`) and its permission model mapped; until then Codex
   sessions get busy/idle with full fidelity and `wait` only via queued-prompt absence.
