---
name: monitor-fleet
description: Put every machine's claude-monitor on one page, using claude-monitor-fleet to discover monitors and open SSH tunnels to them. Trigger when the user says "manage my environments", "show all my monitors in one place", "monitor my other machines", "aggregate claude-monitor across hosts", "多环境管理", or otherwise wants agent activity from more than one machine in a single view.
---

# monitor-fleet

`claude-monitor` shows what is happening on **one** machine, over loopback only.
`claude-monitor-fleet` is a separate binary that opens one SSH tunnel per
environment and serves a switcher page with each machine's unmodified monitor
page inside it. The monitor itself is unchanged and needs no flag.

## Discover before you configure

**Never write a host name the user did not give you, and never assume a port.**
The tool ships with an empty config on purpose. Everything in it comes from
discovery or from the user.

```
claude-monitor-fleet discover
```

This probes this machine and every literal `Host` in the user's own SSH config,
reads each monitor's port out of its lock file, and prints what exists. It writes
nothing. Show the user the list.

Then persist it:

```
claude-monitor-fleet discover --add
```

Or curate by hand when the user only wants some of it:

```
claude-monitor-fleet add prod --ssh prod-box
claude-monitor-fleet add laptop                 # no --ssh means this machine
claude-monitor-fleet remove prod
claude-monitor-fleet list
```

The config is JSON at `$CLAUDE_MONITOR_FLEET_CONFIG`, else
`$XDG_CONFIG_HOME/claude-monitor/fleet.json`, else
`~/.config/claude-monitor/fleet.json` — the user can edit it directly.

## Serve the page

```
claude-monitor-fleet up
```

It brings up the tunnels, prints a `http://127.0.0.1:<port>/` URL and opens a
browser. **It stays in the foreground and owns the tunnels: they close when it
exits.** So run it where it will outlive your turn — the user's own terminal is
the honest answer, and if you background it yourself, tell the user that closing
that shell takes the page down with it. `--no-open` skips the browser, `--port N`
pins the local port.

To look without saving anything to the config:

```
claude-monitor-fleet up --discover
```

## When something is missing

Report what the tool reported. Do not fill gaps by guessing.

- No monitor on a host: it is not running there. Suggest starting
  `claude-monitor` on that machine, or `add <name> --port N` if the user knows
  the port.
- A host skipped during discovery: discovery uses `ssh BatchMode`, so a host
  that would ask for a passphrase is skipped rather than left hanging. Suggest
  loading the key into an agent (`ssh-add`), or adding that host with `add`,
  where prompts work.
- Two monitors on one machine: they are found separately, each at its own
  published port, and told apart by cache root. That is expected, not a bug.
- An alias missing from the results: two `Host` entries for the same machine are
  collapsed to one environment — discovery says so on stderr when it happens.
- A tunnel that dies later: the page's health dots go red and name the reason.
  `claude-monitor-fleet status` re-probes from the command line.

## Getting the binary

If `claude-monitor-fleet --help` fails, it is not installed. From a
`claude-replay` checkout:

```
cargo install --path claude-monitor-fleet
```
