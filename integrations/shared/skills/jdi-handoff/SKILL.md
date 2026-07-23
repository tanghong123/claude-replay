---
name: jdi-handoff
description: Hand the current interactive Claude/Codex session off to an unattended agent-jdi background run — it quits this session and resumes it autonomously so the human can walk away. Trigger when the user says "hand this off to jdi", "justdoit", "let jdi finish this", "hand off to agent-jdi", "take it from here unattended", or otherwise wants the agent to keep working on its own after they leave.
---

# jdi-handoff

Hand the current session to `agent-jdi` so it continues **unattended** in the
background. This is the mirror of `agent-jdi takeover` (which hands a background
run back to a human).

## When to use

The user wants to stop babysitting the session and let it finish on its own —
"hand this off to jdi", "justdoit", "let jdi take it from here", etc.

## How

1. Figure out a short instruction for what remains — ask the user, or summarize
   the current goal in one line.
2. Run, from **inside** this session (it finds the session automatically):

   ```
   agent-jdi handoff <instruction>
   ```

   This spawns a detached watcher and then quits this session. Once the session
   exits, agent-jdi resumes it unattended and drives the work to completion,
   retrying on recoverable failures.

   For Codex, handoff preserves the current Codex permission context: `read-only`,
   `workspace-write` with its exact network setting, or `danger-full-access`.
   Capture happens before the watcher starts or the current session exits. If the
   current turn's permission policy cannot be read safely, handoff aborts and
   leaves the interactive session running. Retries and backlog drains reuse the
   captured policy. No extra Skill flag is required. Claude permission behavior
   is unchanged.

Do not ask for extra confirmation once the user has asked to hand off — just run
the command.

## Options

- `agent-jdi handoff --armed <instruction>` — arm the handoff **without** quitting;
  the user presses `/exit` themselves when ready.
- `agent-jdi handoff --interval <secs> --max-attempts <n> <instruction>` — tune the
  unattended retry loop.

## Getting it back

To pull the unattended run back to an interactive session later:

```
agent-jdi takeover
```
