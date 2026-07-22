# agent-jdi — design

A multi-agent, unattended-run supervisor that shares the `claude-replay` crate's
transcript discovery/parsing. Ported from the bash `claude-jdi` (claude-toolbox),
restructured as an **agent-agnostic spine + per-agent adapters**.

## Layout

```
src/bin/agent-jdi.rs   thin shim → claude_replay::jdi::run()
src/jdi/
  mod.rs         CLI (clap) + Config + dispatch (start/resume/handoff/log/status/list/backlog/takeover + __run/__handoff)
  supervisor.rs  detached __run worker + the retry loop + takeover     ── spine
  state.rs       <home>/<id>/ layout, atomic `meta` key=value, RunState, slot_id, liveness
  lock.rs        mkdir-atomic slot lock (owner pidfile + stale reclaim) ── spine
  backlog.rs     pending→draining→drained crash-safe queue             ── spine
  agent.rs       the AgentAdapter trait + shared types + the registry
  detect.rs      pick the agent for a cwd + the claude-jdi live-conflict check
  claude.rs      Claude adapter (native task queue, plan→execute, ~/.claude/projects)
  codex.rs       Codex adapter (codex exec resume, no task queue, exit-code done)
```

## The spine vs. the adapter

The **spine** owns the state dir, slot lock, backlog dirs, the detach, and the
retry loop's control flow (mode sequencing, attempt/backoff/max-attempts, signal
handling). It is agent-neutral.

Each **agent** implements `AgentAdapter`:

| method | Claude | Codex |
|---|---|---|
| `initial_mode(trigger)` | `ResumeDump`→`ResumeExecute` (plan then do) | `Execute` (no plan step) |
| `build_invocation(ctx)` | `claude --resume\|--session-id … --dangerously-skip-permissions -p <prompt>` | `codex exec resume -c approval_policy=… -c sandbox_mode=… <id> <prompt>` |
| `classify(rc, out, ctx)` | dump→advance; execute + task-queue-empty→Done; "No conversation found"→recreate; UNRECOVERABLE→failed | rc 0→Done; 130/143→stopped; else retry |
| `discover_resumable(cwd)` | newest `~/.claude/projects/<slug>/*.jsonl` | newest `~/.codex/sessions/**` for cwd |
| `task_queue()` *(optional)* | `Some` (`~/.claude/tasks/`) | `None` |
| `pins_session_id()` | `true` (`--session-id`) | `false` (Codex assigns; captured after turn 1) |
| `fresh_invocation()` / `capture_session_id()` | pins → default reuse | `codex exec …` + nonce scan / `--json` |
| `interactive_invocation()` / `resume_commands()` *(optional)* | `claude --resume <id>` (+ the autonomous variant for the printout) | `codex resume <id>` |
| `unattended_note()` | `--dangerously-skip-permissions (unattended)` | `sandbox=workspace-write, approvals=never` |

`interactive_invocation` is the **human-in-the-loop** resume (no `-p`/skip flags) that
`takeover` launches and `handoff` schedules; `resume_commands` are the copy-paste
resume lines `takeover` prints.

**`start` (fresh run).** The first turn feeds the task (`Mode::Start`); the spine
then captures the assigned id — pinned up front for Claude, recovered for Codex from
the rollout carrying a per-run nonce (or the `--json` stream) — and drops into
`continue_mode()` for relaunches. `new_run_id()` mints the UUID/nonce.

**`start`/`resume` output.** Both launch the detached worker and, by default, print a
summary and return (`-f/--follow` opens the viewer instead). The worker stamps
`started` at launch and `finished` on any terminal exit (via the `run_loop` wrapper),
so `status` can show both.

**Human ↔ jdi boundary (`takeover` / `handoff`).** Mirrors. `takeover` stops a run and
launches `interactive_invocation(autonomous)` so a human continues it — autonomous by
default, since the run was already unattended and dropping the flag would prompt on
every action (`--supervised` flips it; `--no-launch` prints the `resume_commands()`
block instead). With no tracked run for the cwd it falls back to taking over the
newest **unmanaged** session: it refuses when another agent still holds that
transcript (`live_agent_for_session`) unless `--force` kills it first.

`handoff` runs *inside* an interactive session:
it finds the session's process (nearest ancestor whose **executable name** — `ps -o
comm=`, never the full argv — is the agent binary), spawns a detached `__handoff`
watcher that waits for that pid to exit then runs `resume`, and — unless `--armed` —
SIGTERMs the session (the watcher escalates to SIGKILL after a 10s grace) so it's
fully hands-off. The deferral is required: two agents can't drive the same
transcript at once.

Because `handoff` executes during a live agent turn, it does the **bare minimum**
there: the ancestor walk identifies the agent from the process itself (so nothing
scans sessions on disk), then it spawns the watcher and signals. Discovery, the
conflict guard and the resume all happen later in the headless watcher. Process
lookups are targeted (`ps -p <pid>`, one per level) rather than a whole-table dump.

> Matching on argv instead of `comm` was a real bug: a Claude Code tool shell runs
> `zsh -c source ~/.claude/shell-snapshots/…`, whose argv contains "claude", so the
> *shell* matched first — handoff signalled it (leaving the TUI alive but wedged) and
> the watcher, seeing it die instantly, fired the resume while the session was still
> running, draining the task queue underneath it.

The tricky **done-signal** (claude-jdi's `cmd_run` 470-511) lives entirely in
`classify`: the spine just acts on the returned `TurnOutcome`
(`Done`/`Retry`/`AdvanceMode`/`RecreateSession`/`Failed`/`Stopped`/`GaveUp`). For
Claude, "planned ≠ done" comes from `task_queue().open_count()` — `Some(0)`/`None`
(unknown ⇒ trust exit code) → done, `Some(n>0)` → re-drain.

**Prompts are ported from the bash claude-jdi**, whose specificity is what actually
gets a usable queue built: a self-contained subject + description per unit of work,
`pending` status, blocks/blockedBy wiring, explicit rules (only fully-scoped work;
re-derive fresh; reconcile rather than duplicate) and a `queued: <N> task(s)`
receipt; the execute turn adds mark-`in_progress`-before-starting and commit-per-task
so progress survives an interrupt. Queued `Brief::backlog` items are folded into dump
turns ("go through them ONE BY ONE"), since that turn is what converts them to tasks.

**Every phase states the same queue discipline** — including `Start`, which plans and
executes in one turn:

- *Behaviour first, mechanism second.* Each prompt describes the durable **queue**,
  then names the task tools "if this session has them" and the queue file otherwise.
  Tool-first phrasing ("use TaskCreate…") derails a session that lacks them into
  arguing with the instruction and inventing an unreadable file of its own.
- *FIFO*, decided at build time ("put the entries in the order they should be
  done"), so execution never re-plans.
- *Skip on blocked*: write the blocker onto the task and move to the next one. One
  blocked item must never stall the queue, and the run must not "end the turn early
  while actionable work remains".
- *New work is placed by kind*: a prerequisite of the **current** task is done now;
  ordinary follow-ups append to the END. Appending a prerequisite would leave the
  task that needs it permanently unfinishable.

**Task tools are not guaranteed.** A session may have no `TaskCreate`/`TaskUpdate`,
in which case the queue is empty, `open_count` is `None`, and an unfinished run would
read as "done" after one turn. So `Brief::checklist` names a `checklist.md` in the
session's state dir: prompts ask for the native tools *if present* and that file
otherwise, and `classify` falls back to counting its unchecked `- [ ]` items. The
prompt must stay conditional — demanding `TaskCreate` outright made agents improvise
their own file, which the supervisor then couldn't read. The fallback paragraph is
**adaptive**: it is omitted for a session that has already written real task files
(`has_native_tasks`), so a session that doesn't need it doesn't pay for it. Dir
existence alone proves nothing — the harness pre-creates `.lock`/`.highwatermark`
even where the tools never appear.

Claude's on-disk schema is one `<n>.json` per task,
`{id, subject, description, activeForm, status, blocks, blockedBy}` — read `subject`
(not the prose `description`) and sort **numerically** (a string sort gives 18, 19, 2).

**The operator instruction reaches every mode.** `Brief::text` (what `resume`/
`handoff` pass) is appended as `Additional instruction:` on resume/execute turns, not
only folded into a fresh `Start` preamble — it was previously written to `task.md` and
then dropped, so a handoff message never reached the agent.

## Adding an agent

1. New `src/jdi/<agent>.rs` implementing `AgentAdapter`.
2. One arm in `agent::adapter()` and a variant on `crate::Agent`.
3. Discovery: reuse or add a `discover`-style module (the viewer already parses
   the transcript, so rendering/`log` come free).

Optional capabilities default to unsupported, so a new agent can ship with just
resume+log and fill in a task queue / fresh-run later.

## State & compatibility

- Own state root `$XDG_STATE_HOME/agent-jdi/<slot>/` (default `~/.local/state/agent-jdi`;
  neutral, not under `~/.claude`; `AGENT_JDI_HOME` overrides the whole path). Files:
  `meta` (key=value — id/agent/cwd/session_id/nonce/state/mode/attempts/interval/
  max_attempts/started/finished/…), `task.md`, `supervisor.log`,
  `output.log`, `backlog/{pending,draining,drained}/`, `.lock/owner`.
- **One supervisor per directory:** before `start`/`resume`, refuse if the bash
  `claude-jdi` is *live* for this cwd (`detect::claude_jdi_live_for_cwd` — a `cwd=`
  match in a legacy `meta` whose `pid` is alive). The bash tool has the symmetric
  check against `agent-jdi`'s state. Each tool's own slot lock covers same-tool
  concurrency; these cross-checks cover the two-tool case.

## Known gaps / TODO

- **Codex CLI unverified** — every `codex` flag is `TODO(verify)` in `codex.rs`,
  including the interactive `codex resume` used by `takeover`/`handoff`.
- Backlog **drain-as-a-run** and the interactive stale-session picker are simplified
  vs. the bash original; the contract (trait + spine) is in place to wire them.
  (`status` now renders the rich progress block — live tool histogram, task
  checklist, recent commits, start/finish — from the transcript + task queue.)
- `resume`/`log` follow the viewer **in-process** (needs a TTY); the detached
  worker survives the viewer exiting.

## Testing

- Unit: `meta` atomicity, slot-lock acquire/reclaim/already-running, backlog state
  machine, each adapter's `classify`/invocation truth tables, the Claude task-queue
  reader.
- Integration: a fake `codex` (via `AGENT_JDI_CODEX_BIN`) drives the whole loop
  (clean→done, failing→gaveup). All headless — no TTY.
