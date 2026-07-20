# agent-jdi — design

A multi-agent, unattended-run supervisor that shares the `claude-replay` crate's
transcript discovery/parsing. Ported from the bash `claude-jdi` (claude-toolbox),
restructured as an **agent-agnostic spine + per-agent adapters**.

## Layout

```
src/bin/agent-jdi.rs   thin shim → claude_replay::jdi::run()
src/jdi/
  mod.rs         CLI (clap) + Config + command dispatch (resume/log/status/list/backlog/takeover/__run)
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

**`start` (fresh run).** The first turn feeds the task (`Mode::Start`); the spine
then captures the assigned id — pinned up front for Claude, recovered for Codex from
the rollout carrying a per-run nonce (or the `--json` stream) — and drops into
`continue_mode()` for relaunches. `new_run_id()` mints the UUID/nonce.

The tricky **done-signal** (claude-jdi's `cmd_run` 470-511) lives entirely in
`classify`: the spine just acts on the returned `TurnOutcome`
(`Done`/`Retry`/`AdvanceMode`/`RecreateSession`/`Failed`/`Stopped`/`GaveUp`). For
Claude, "planned ≠ done" comes from `task_queue().open_count()` — `Some(0)`/`None`
(unknown ⇒ trust exit code) → done, `Some(n>0)` → re-drain.

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
  `meta` (key=value), `task.md`, `supervisor.log`,
  `output.log`, `backlog/{pending,draining,drained}/`, `.lock/owner`.
- **One supervisor per directory:** before `start`/`resume`, refuse if the bash
  `claude-jdi` is *live* for this cwd (`detect::claude_jdi_live_for_cwd` — a `cwd=`
  match in a legacy `meta` whose `pid` is alive). The bash tool has the symmetric
  check against `agent-jdi`'s state. Each tool's own slot lock covers same-tool
  concurrency; these cross-checks cover the two-tool case.

## Known gaps / TODO

- **Codex CLI unverified** — every `codex` flag is `TODO(verify)` in `codex.rs`.
- Backlog **drain-as-a-run**, the interactive
  stale-session picker, and `status`'s rich progress rendering are simplified vs.
  the bash original; the contract (trait + spine) is in place to wire them.
- `resume`/`log` follow the viewer **in-process** (needs a TTY); the detached
  worker survives the viewer exiting.

## Testing

- Unit: `meta` atomicity, slot-lock acquire/reclaim/already-running, backlog state
  machine, each adapter's `classify`/invocation truth tables, the Claude task-queue
  reader.
- Integration: a fake `codex` (via `AGENT_JDI_CODEX_BIN`) drives the whole loop
  (clean→done, failing→gaveup). All headless — no TTY.
