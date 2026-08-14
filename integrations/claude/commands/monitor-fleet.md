---
description: Put every machine's claude-monitor on one page (discovers monitors, opens SSH tunnels, serves a switcher)
argument-hint: [optional extra ssh destination]
allowed-tools: Bash(claude-monitor-fleet:*)
---

Aggregate the user's `claude-monitor` instances into one page with
`claude-monitor-fleet`.

Do it in this order, and do not invent host names or ports — everything comes
from discovery or from the user. `$ARGUMENTS`, if given, is an extra SSH
destination to include.

1. Survey what actually exists (this writes nothing):

   ```
   claude-monitor-fleet discover
   ```

   With an argument: `claude-monitor-fleet discover --host $ARGUMENTS`

2. Show the user the list. If it is empty, say so — no monitor is running
   anywhere reachable — and stop rather than guessing a port.

3. Save it, then serve it:

   ```
   claude-monitor-fleet discover --add
   claude-monitor-fleet up
   ```

`up` holds the tunnels open in the foreground and they close when it exits, so
run it where it will outlive this turn and tell the user the
`http://127.0.0.1:<port>/` URL it prints. A tunnel that drops later is re-opened
by that same process, with backoff — a red dot is not a reason to restart it.
