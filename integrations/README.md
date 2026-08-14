# Integrations

Opt-in Skills that let an agent drive these tools by being asked in plain
language. Each integration ships one agent-neutral Skill (Codex reads it; Claude
Code links to the same file) plus a native Claude Code slash command, and one
installer serves all of them — `./integrations/install-skill.sh <name>`. The
refusals that matter (a managed path that is a symlink, destinations that
collapse onto each other, a local edit that must be preserved once) therefore
have a single implementation, and adding an integration adds no script.
`install-jdi-handoff.sh` stays beside it because `agent-jdi install-skill` and
this document already name that path.

- [`jdi-handoff`](#jdi-handoff-integration) — hand a live session to an
  unattended `agent-jdi` run.
- [`monitor-fleet`](#monitor-fleet-integration) — put several machines'
  `claude-monitor` instances on one page.

## `jdi-handoff` integration

Hand a live Claude Code or Codex session to an unattended `agent-jdi` run. Both
clients use the same agent-neutral Skill; Claude Code also gets its native
`/jdi-handoff` slash command.

Both entry points run `agent-jdi handoff`, which arms a detached watcher and
quits the interactive session. After that process exits, `agent-jdi` resumes the
same session unattended. A Codex handoff preserves the current turn's sandbox
(`read-only`, `workspace-write`, or `danger-full-access`) and the exact
workspace-write network setting. If that policy cannot be captured, handoff
fails before the watcher starts or the current session is stopped. A direct
`agent-jdi start` or `resume` instead uses the safe default
`workspace-write` with network disabled; Claude behavior is unchanged.
Retries and backlog drains reuse the tracked handoff policy. The Skill requires
no additional permission flag.

Install the `agent-jdi` binary first, then install the integration — no checkout
needed, the binary bundles the skill content (#26):

```sh
agent-jdi install-skill
```

From a source checkout the shell installer is equivalent:

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
`agent-jdi install-skill` additionally preserves a locally-modified managed file
once as `<name>.pre-install-backup` before refreshing it — and refuses to touch a
file modified again after that backup exists, so local edits are never silently
clobbered.
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

Use `agent-jdi handoff --dry-run` to preview the captured Codex permission
policy without spawning a watcher or stopping the session. After handoff,
`agent-jdi status` reports the effective persisted policy.

To install into non-default client roots, pass both or either destination:

```sh
./integrations/install-jdi-handoff.sh \
  --agents-dir /path/to/.agents/skills \
  --claude-dir /path/to/.claude
```

Run `agent-jdi takeover` from the original working directory to stop the
unattended run and resume it interactively.

## `monitor-fleet` integration

Ask an agent to manage several machines at once, and it puts every reachable
`claude-monitor` on one page. Both clients use the same agent-neutral Skill;
Claude Code also gets its native `/monitor-fleet` slash command.

`claude-monitor` shows one machine and binds loopback only, and it stays that
way — this integration adds no flag to it and no remote mode. What both entry
points drive is `claude-monitor-fleet`, a separate binary that opens one SSH
tunnel per environment and serves a switcher whose tabs are each machine's
unmodified monitor page in an iframe. `design/monitor-fleet.md` explains why it
is a companion rather than an extension point.

Install the binary, then the integration:

```sh
cargo install --path claude-monitor-fleet
./integrations/install-skill.sh monitor-fleet
```

Pass `--agents-dir` and `--claude-dir` to install into non-default client roots.

The layout matches the other integrations:

```text
~/.agents/skills/monitor-fleet/SKILL.md     # shared installed Skill; Codex reads it
~/.claude/skills/monitor-fleet/SKILL.md     # symlink to the shared installed Skill
~/.claude/commands/monitor-fleet.md         # Claude-only slash command
```

Open a new client session, then one prompt is enough:

- Claude Code: `/monitor-fleet`, or say "put all my monitors on one page".
- Codex: `$monitor-fleet`, or select `monitor-fleet` through `/skills`.

Either way the agent runs the same three steps, and the first one is the point:

```sh
claude-monitor-fleet discover        # probe; writes nothing
claude-monitor-fleet discover --add  # keep what was found
claude-monitor-fleet up              # tunnels + the switcher page
```

**Nothing about your machines is assumed.** The config ships empty, every host
in it is one you or discovery put there, each monitor's port is read from that
monitor's own lock file rather than guessed, and local tunnel ports are
allocated by the kernel so they cannot collide with what you already run. The
Skill tells the agent to report what discovery found instead of filling gaps —
so "no monitor there" stays "no monitor there".

Discovery probes this machine and every literal `Host` in your SSH config using
`ssh BatchMode`, so a host that would ask for a passphrase is skipped rather
than left hanging; load the key into an agent, or add that host with
`claude-monitor-fleet add`, where prompts work. Two `Host` aliases for the same
machine are collapsed into one environment.

`up` holds the tunnels for as long as it runs and takes them down when it stops,
including on `Ctrl-C` or `kill` — the port is yours again either way. A tunnel
that drops while it runs is re-opened, per environment and with backoff, on the
same local port when that port is still free, so a network blip costs one tab a
few seconds. A monitor that is down is still only reported: the fleet starts
nothing on anyone's machine.
