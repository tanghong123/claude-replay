---
description: Hand this session off to an unattended agent-jdi run (quits it, then resumes in the background)
argument-hint: [what's left to do]
allowed-tools: Bash(agent-jdi handoff:*)
---

Hand this interactive session over to `agent-jdi` so it continues **unattended** in the background.

Run exactly this, and nothing else:

```
agent-jdi handoff $ARGUMENTS
```

That spawns a detached watcher and quits this session; once it exits, agent-jdi
resumes the same session unattended and drives the work to completion. Do not
take any other actions or ask for confirmation — just run the command.
