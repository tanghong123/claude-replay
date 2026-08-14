# Design: `claude-monitor-fleet` — several machines, one page

> **v1 — BUILT.** The crate exists (`claude-monitor-fleet/`), the `monitor-fleet` integration
> installs the Skill and slash command that let an agent drive it from one prompt, and the whole
> thing is additive: `claude-monitor` is not modified, not extended, and gains no flag.
>
> This answers "manage several environments" without contradicting `design/claude-monitor.md` §12
> ("Not multi-machine. Everything assumes one filesystem and one process table"), because it does
> not make the monitor multi-machine. It puts N single-machine monitors behind one page.

`claude-monitor` answers *"what is happening on this machine"*. Anyone with more than one machine
then wants *"what is happening on all of them"*, and today does it by hand: an `ssh -L` per box,
remembered ports, a browser window per tab, and a stale forward left over when the laptop lid
closes.

---

## 1. Requirements

| | |
|---|---|
| **R1** | One page over **several machines'** monitors, each machine's view unchanged. |
| **R2** | **No assumptions about the user's environment.** No shipped host, no shipped port, no shipped topology. Everything is discovered or supplied. |
| **R3** | **The monitor is not modified.** No new flag, no remote mode, no extension point. |
| **R4** | Usable by an **agent from one prompt**, and by a human from a command line. |
| **R5** | Opt-in. Someone who wants the single-machine monitor sees nothing new. |
| **R6** | Leave the machine as it was found: no orphaned tunnels, no occupied ports, on any exit path. |

## 2. Why a companion binary and not an extension point

#98 §6.3 settled that the monitor "offers **no extension point at all**" — a slot is an extension
point shaped like one host's layout, and hosts would accumulate one each. The unit of reuse is a
URL, and composition happens at the document level.

A fleet view is exactly the host §6.3 anticipated. So it takes the deal on offer: it consumes the
monitor's page at its URL and composes at the document level, one `<iframe>` per environment.
Nothing is proxied, no route is re-implemented, and the tab content is byte-for-byte what that
machine serves to itself. A monitor upgraded on one machine changes what its tab shows and nothing
else.

Three consequences worth naming:

**A second binary, not a subcommand of the monitor.** #98 §11's answer to "who can see this" is
"whoever is on this machine", and R8 makes loopback structural. A `--remote` flag on the monitor
would put an aggregator inside the process whose entire security story is that it only talks to
localhost. Separating them keeps that story intact: each monitor still binds loopback only, still
serves only its own machine, and the thing that crosses machines is `ssh` — which #98 §11 already
names as the honest mechanism, "an SSH tunnel, not a flag".

**Cross-origin by construction, so health is measured server-side.** The iframes are separate
origins; the page cannot read them. It therefore never claims a tab is healthy because it rendered:
`/api/fleet` probes each target from the server side and the tab's dot reflects that.

**The fleet knows nothing about sessions.** It does not read a transcript, a cache, or a meta
record. Whatever the monitor learns to show, the fleet shows too, without being taught.

## 3. R2 in the code: topology is data

The requirement "do not assume my environment" is easy to agree with and easy to violate later, so
it is enforced in three places rather than documented in one.

**The config ships empty, and a test says so.** `Fleet::default()` has no environments, and
`a_default_fleet_has_no_hosts` fails if a convenience default is ever added — a shipped host list
would be someone else's machines. The file lives at `$CLAUDE_MONITOR_FLEET_CONFIG`, else
`$XDG_CONFIG_HOME/claude-monitor/fleet.json`, else `~/.config/claude-monitor/fleet.json`:
deliberately *not* under the cache root, because a cache is wipeable and this is the user's own
editable list.

**Ports are read, never guessed.** A monitor publishes `{"pid":…,"note":{"port":…}}` into
`<cache root>/LOCK` when it binds. The probe reads that. This is not fastidiousness: a hard-coded
`2727` lands on whichever monitor happens to hold 2727 — an older build, a colleague's instance,
or nothing at all — and the failure surfaces as a silently wrong tab. The test
`an_absent_monitor_is_reported_not_guessed` fails if a default port creeps back in. A monitor
started on a non-default `--port`, or a second one under its own `$CLAUDE_MONITOR_CACHE`, is found
as it is.

**Local ports are allocated by the kernel.** A ladder (first environment gets N, second N+1) walks
straight into whatever else the user runs, and the collision shows up as an empty tab much later.
`free_port()` asks for a free one; the tunnel is not reported up until it answers HTTP.

Discovery then needs no configuration to be useful: it probes this machine and every **literal**
`Host` in the user's own SSH config (patterns like `tier-*` are not destinations), reads each
machine's locks, and prints what exists. `--add` persists it. Nothing is written by a bare
`discover`.

## 4. One probe program, run two ways

The probe is a single POSIX shell script, fed to `sh -s` locally and `ssh host "sh -s -- '<root>'"`
remotely. One implementation, so the local and remote answers cannot drift as one gets fixed.

It enumerates cache roots — an explicit one, `$CLAUDE_MONITOR_CACHE`, then the glob
`~/.cache/claude-monitor*` — and prints one line per lock. The glob is what finds a second monitor
without being told its name (`-next`, `-staging`, whatever the user called it); it is a heuristic
about *this tool's own* directory naming, not about the user's machines, and `--cache-root` covers a
root somewhere else entirely. A non-interactive `ssh host sh` reads no `.zshrc`, so
`$CLAUDE_MONITOR_CACHE` is usually unset remotely even when the user's shell sets it — hence the
glob, and hence `add --cache-root` as the escape hatch.

Every line is prefixed (`MON`, `ID`), so a MOTD, a banner or `Last login:` is ignored rather than
parsed as a result.

**`BatchMode` is split by intent.** A survey of every host in a config uses `BatchMode=yes`: one
host waiting for a passphrase must not stall a scan the user did not aim at it, so it is skipped
with the reason reported. A deliberate connection to a *configured* environment uses
`BatchMode=no` with stderr inherited — a passphrase prompt, a host-key question and an auth failure
belong to the user, not to an error string.

**Machines are asked who they are.** Two `Host` aliases for one box (a short name and an FQDN, a
direct route and one through a jump host) are two destinations and one monitor, and forwarding to
both gives the user two tabs showing the identical page. So the probe emits an `ID` line
(`/etc/machine-id`, else `hostname`) and discovery collapses `(machine, root, pid)` duplicates,
reporting the collapse. When a machine will not identify itself the destination name stands in, so
an unknown identity never merges hosts that are genuinely different. The first destination wins,
and since this machine is probed first, a local monitor is never offered as a tunnel back to
itself.

## 5. The tunnel, and the mess it must not leave

`ssh -L <kernel-picked local>:127.0.0.1:<published remote>`. The remote side is `127.0.0.1` because
that is the only address a monitor binds.

**A forward that accepts is not a monitor that answers.** With a forward open, `ssh` accepts the
local connection and only *then* discovers the far side is dead, so a TCP connect succeeds against
a stopped monitor. Health is therefore an HTTP status line, not a connect
(`accepting_a_connection_is_not_serving_http`).

**R6 is the part that is easy to get wrong, and this design got it wrong first.** `Drop` kills and
reaps the child, which covers a normal exit — and covers nothing else, because Rust runs no
destructor when the process is signalled. The first implementation used `ssh -N` and claimed
"`Ctrl-C` never leaves an `ssh -N` behind"; a real run, killed the way anyone kills a foreground
server, left one forward per environment alive, reparented to init, holding ports the user believed
were free.

The fix is not a signal handler: it would add a dependency to a deliberately dependency-frugal
workspace, and it would still lose to `kill -9`. Instead the far end runs `cat >/dev/null` instead
of `-N`, and its stdin is a pipe this process holds. However this process dies — cleanly, on a
signal, or outright — the kernel closes that end, the far side reads EOF and exits, and `ssh`
follows it. `the_keepalive_ends_when_its_input_does` pins the property on the command itself, so
replacing it with anything that ignores stdin fails a test instead of quietly restoring the leak.

Piping stdin costs the user nothing: OpenSSH asks for passphrases and host-key confirmations on
`/dev/tty`, and stderr stays attached.

*Verified on real hardware, not only in tests:* with several environments up, `pkill` on the fleet
left zero forwards behind, while forwards from the pre-fix `-N` build were still running and had to
be killed by hand.

### 5.1 Owning a tunnel means putting it back, not only taking it down

R6 was read at first as being only about cleanup, and the same sentence has another half. A forward
the fleet opened is the fleet's for the whole run — and the machine it runs on sleeps, changes
networks and times connections out. The first implementation opened each forward once and parked:
`/api/fleet` then reported `the ssh tunnel exited` accurately and forever, and the only cure was to
kill the process, which also cost the tabs that were fine. Observed exactly that way — `Read from
remote host …: Operation timed out`, one line per environment, after the laptop had spent a while off
the network.

This is not the supervision §7 refuses. A *monitor* is someone else's process on someone else's
machine, and starting it is a cluster manager's job. A forward is this process's own child, promised
in the README for as long as the process runs. The rule is one line: **re-open what we opened, report
what we found.**

"Dropped" means one thing precisely: the `ssh` child has exited. That is what the watcher tests, and
`ServerAliveInterval=15` with `ServerAliveCountMax=3` — already there for the first connect — is what
turns a network that died silently into a child that exits within a minute rather than a forward that
hangs forever. A monitor that answers no HTTP is deliberately *not* treated as a dropped tunnel, even
though the health column already knows about it: the forward is alive, the thing behind it is not, and
re-opening would tear down a working tunnel to a machine whose monitor someone stopped on purpose.
That asymmetry *is* §7's boundary, expressed in the one place it could be blurred by accident.

Three details are load-bearing.

**The re-open aims at the same local port.** The page's `ENVS` — every iframe `src`, every bookmark —
is substituted into `fleet.html` once, when it is rendered, so a repair that took a fresh kernel port
each time would leave the tab pointing at a dead number. `port_preferring(want)` keeps the port while
it is still bindable, which makes the usual reconnect invisible to the browser. It cannot be
promised — the dead `ssh` released the port and anything on the machine may have taken it since — so
`/api/fleet` publishes each environment's *current* URL on every poll and the page adopts it when it
changes. That same signal gives a recovered tab its reload: the iframe is holding the browser's error
page from while the tunnel was down, and health is the thing that knows the moment it stopped being
true.

**Re-opening happens outside the lock.** `ssh` can take twenty seconds to decide. Holding the
environment list across that would freeze `/api/fleet`, and a health strip that stops answering looks
exactly like a fleet that died — so the watcher takes the list, notes which forwards are down, drops
the lock, and re-acquires it only to store the tunnel it got back.

**Backoff, per environment, kept by the watcher.** The first attempt is immediate, because the common
case is an `ssh` that died while the network is fine; after that the wait doubles to a ceiling, so a
host that is genuinely away is asked once a minute instead of once a tick, and a key whose passphrase
nobody is there to type does not spin. The schedule lives in the watcher rather than in each
environment: nothing else needs it, and a `Live` that carried one could hand a stale appointment to a
tunnel that has since been replaced.

The orphan guard survives all of this because there is exactly one place that spawns an `ssh`
(`Tunnel::attempt`). First connect and re-open both go through it, so `KEEPALIVE` and the held stdin
pipe cannot be present on one path and forgotten on the other.

A limit, accepted knowingly: recovery is **serialized**. One watcher thread re-opens dropped
forwards in turn, and one attempt can spend up to twenty seconds deciding — so when a lid-close
drops every forward at once, the last of N environments can wait ~N×20 s, and a drop that happens
during an in-flight attempt is not even noticed until it resolves. Bounded, and fine for the
handfuls of machines this tool is for; if fleets grow, the fix is a shorter serving budget on
re-opens (the common reconnect answers in a couple of seconds) or a thread per environment — not a
supervisor framework.

## 6. One prompt: the `monitor-fleet` integration

R4 is met the way this repo already does it (`integrations/`, the `jdi-handoff` precedent): one
agent-neutral Skill that Codex reads and Claude Code links to, plus a native `/monitor-fleet` slash
command. Its triggers are what a person actually says — "manage my environments", "put all my
monitors on one page", "多环境管理" — and never a host name, which would be one user's topology
baked into a trigger.

The Skill's substance is a rule, not a recipe: **discover before you configure, and report what
discovery found instead of filling gaps.** An agent that invents a plausible hostname or a
plausible port produces a page that looks right and shows the wrong machine, which is worse than an
empty list. "No monitor there" is an answer.

There is one installer — `integrations/install-skill.sh monitor-fleet` — and this integration adds
no second one. The delicate part of installing a Skill is not the copying but the refusals: a
managed path that is a symlink, destinations that collapse onto each other, a local edit preserved
exactly once. A second copy of those would drift from the first the day one is fixed, and an entry
point whose whole body re-invokes the installer buys nothing to justify the copy it invites. An
integration is a Skill, a command, and a name.

## 7. What this is not

- **Not a change to the monitor.** If a feature here needs the monitor modified, that is a separate
  design with its own review.
- **Not a proxy.** Each tab is that machine's own page from that machine's own port. No route is
  re-implemented, so nothing to keep in sync.
- **Not an auth boundary.** #98 §11's answer stands, once per machine: whoever can `ssh` there
  could already see it. The fleet adds reach for the person who already had the keys, and every
  port it opens is loopback.
- **Not a cluster manager.** It starts no monitor, installs nothing remotely, and configures no
  host. It finds what is running and shows it.
- **Not a supervisor.** A monitor that is down stays down and is reported down; `agent-jdi` owns
  unattended runs.
