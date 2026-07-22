# Shared jdi-handoff Skill Design

## Goal

Ship one agent-neutral `jdi-handoff` Skill that both Codex and Claude Code use,
while retaining Claude Code's dedicated `/jdi-handoff` command.

## Repository layout

The canonical source moves from the Claude-only directory to:

```text
integrations/shared/skills/jdi-handoff/SKILL.md
```

Claude-specific command metadata remains at:

```text
integrations/claude/commands/jdi-handoff.md
```

There is no duplicated `integrations/codex/.../SKILL.md` copy. The Skill already
describes an agent-neutral Claude/Codex workflow and calls the shared
`agent-jdi handoff` binary.

## Installation model

`integrations/install-jdi-handoff.sh` installs both clients by default:

1. Copy the repository Skill into
   `~/.agents/skills/jdi-handoff/SKILL.md`. Codex discovers this standard user
   Skill directly.
2. Create
   `~/.claude/skills/jdi-handoff/SKILL.md` as a symbolic link to the installed
   canonical Skill.
3. Copy the Claude-only slash command to
   `~/.claude/commands/jdi-handoff.md`.

The script accepts `--agents-dir` and `--claude-dir` overrides so tests and
non-default client layouts never need to replace `$HOME`.

The installed canonical copy is independent of the Git checkout, so moving or
deleting the repository does not break either client. Re-running the installer
refreshes managed files and leaves the same link topology. If an older Claude
installation has a regular `SKILL.md`, the installer preserves it once as
`SKILL.md.pre-shared-backup` before replacing it with the shared link.

## User-facing behavior

- Claude Code retains `/jdi-handoff <instruction>` and natural-language Skill
  triggering.
- Codex gains `$jdi-handoff <instruction>` and `/skills` discovery after a new
  session is opened.
- Both paths ultimately execute `agent-jdi handoff <instruction>`.

The installer does not modify client configuration files and does not attempt to
invent a Codex top-level `/jdi-handoff` command; arbitrary first-level Codex slash
commands are not an extension surface.

## Verification

A macOS/Linux `/bin/sh` integration test installs into temporary directories containing
spaces and verifies canonical content, symlink targets, Claude command content,
idempotent reinstall, migration from the previous regular-file layout, safe
replacement of managed file symlinks, and rejection of installer-owned directory
symlinks. CI runs this test alongside the existing Rust gates.

## Scope boundaries

- No Rust JDI behavior changes.
- No changes from the separate `fix/codex-jdi-picker` branch or PR.
- No automatic modification of the developer's real home-directory installation
  during tests or builds.
