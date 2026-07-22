# `jdi-handoff` integration

Hand a live Claude Code or Codex session to an unattended `agent-jdi` run. Both
clients use the same agent-neutral Skill; Claude Code also gets its native
`/jdi-handoff` slash command.

Both entry points run `agent-jdi handoff`, which arms a detached watcher and
quits the interactive session. After that process exits, `agent-jdi` resumes the
same session unattended. Install the `agent-jdi` binary first, then install the
integration from this checkout:

```sh
./integrations/install-jdi-handoff.sh
```

The installer creates this layout:

```text
~/.agents/skills/jdi-handoff/SKILL.md       # shared installed Skill; Codex reads it
~/.claude/skills/jdi-handoff/SKILL.md       # symlink to the shared installed Skill
~/.claude/commands/jdi-handoff.md           # Claude-only slash command
```

The installed Skill is copied out of the Git checkout, so moving or deleting the
checkout does not break the clients. Re-running the installer refreshes managed
files. When migrating an older copied Claude Skill, the previous regular file is
preserved once as `SKILL.md.pre-shared-backup` before the symlink is created.
Managed command-file symlinks are replaced rather than followed, and the
installer refuses installer-owned directories that are themselves symlinks so
it cannot write outside the selected client roots.

Open a new client session after installation. Then use:

- Claude Code: `/jdi-handoff finish the refactor and commit`, or say "hand this
  off to jdi" to trigger the Skill.
- Codex: `$jdi-handoff finish the refactor and commit`, or select `jdi-handoff`
  through `/skills`.

Codex does not expose arbitrary custom first-level slash commands, so the shared
Skill is `$jdi-handoff`, not `/jdi-handoff`. Both clients ultimately run:

```sh
agent-jdi handoff <what remains to do>
```

Use `--armed` if you want to exit the interactive session yourself:

```sh
agent-jdi handoff --armed <what remains to do>
```

To install into non-default client roots, pass both or either destination:

```sh
./integrations/install-jdi-handoff.sh \
  --agents-dir /path/to/.agents/skills \
  --claude-dir /path/to/.claude
```

Run `agent-jdi takeover` from the original working directory to stop the
unattended run and resume it interactively.
