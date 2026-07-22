# Claude Code integrations for `agent-jdi`

In-session entry points that hand a live Claude Code session over to an unattended
`agent-jdi` run (the mirror of `agent-jdi takeover`, which pulls one back to a human).

Both wrap `agent-jdi handoff`, which arms a detached watcher and quits the session;
when it exits, `agent-jdi` resumes it unattended.

## Install

```sh
# Slash command → /jdi-handoff
mkdir -p ~/.claude/commands
cp integrations/claude/commands/jdi-handoff.md ~/.claude/commands/

# Skill → triggers on "hand this off to jdi" / "justdoit"
mkdir -p ~/.claude/skills/jdi-handoff
cp integrations/claude/skills/jdi-handoff/SKILL.md ~/.claude/skills/jdi-handoff/
```

New sessions pick them up. Then, from inside a session:

- `/jdi-handoff finish the refactor and commit` — explicit slash command, or
- just say "hand this off to jdi" / "justdoit" to trigger the skill.

Use `agent-jdi handoff --armed …` to arm without auto-quitting (you press `/exit`).
