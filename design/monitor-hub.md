# Design: one Agent Monitor over many machines — a native app (#s407)

> **DRAFT 2 — PROPOSED, nothing built.** Draft 1 proposed a hub, one server that relays to every
> monitor. The owner chose a native app instead (2026-11-07): "I have devices spread across the
> globe, and having a remote aggregation point will add more constraints." This draft designs that.
> The monitor stays a single-machine service (`design/claude-monitor.md` §12); what spans machines is
> an app on the reader's own device, talking to each monitor as a phone already does. It supersedes
> the fleet's page of tabs (`design/monitor-fleet.md`) and builds on what `design/fleet-pairing.md`
> shipped (the gate, the token, the one-time pairing code). Its stage 0 is shared with
> `design/embedding.md` (#s406), which cites this file.

**What it is for.** A person with agents on machines around the world wants one Agent Monitor: pair
it once with each machine, then see every session in ONE session list, each row saying where it
runs, with a new way to arrange the list by device first. Everything else reads and behaves as the
monitor does today: the session view, search, the panes, the compose box and dialogs, files and
mdrev. On a phone, and on a Mac.

**The recommendation in four lines.** A native app, for iPhone, iPad and Mac from one codebase. It
pairs with each monitor directly, by the QR code `--pair-phone` already shows, and keeps each token
in the Keychain. It shows today's app shell in a web view, served from a custom URL scheme whose
handler is a relay on the device itself: it sends each request to the monitor it is for, over the
tailnet, with that monitor's token. The shell merges the lists and routes each session to its own
machine. No monitor changes a route, a field or its gate.

---

## 1. Requirements

| | |
|---|---|
| **R1** | One session list over N machines' monitors; each row says which device it runs on. |
| **R2** | An Arrange choice "by device", then each device arranged as the reader already chose. |
| **R3** | Otherwise the same look and feel: the session view, search, panes, files, mdrev, compose and dialogs, for a session on any paired device. |
| **R4** | Pairing is once per machine, from the app; a machine can be forgotten. |
| **R5** | Works on a phone and on a Mac, with no server of its own anywhere: each request goes from the reader's device straight to the machine it is for. |
| **R6** | A machine that is offline or slow costs its own rows only: the list still answers. |
| **R7** | Nothing new is exposed: each monitor is reached exactly as a paired phone reaches it today. |
| **R8** | No monitor is made multi-machine: its routes, gate and §11 exposure rule are unchanged. |

## 2. What exists, and the facts the design rests on

- **The fleet** (`agent-monitor-fleet`, built) reads its topology from a config file, probes each
  machine over ssh for the monitor's port and a machine id, and keeps one `ssh -L` tunnel per
  machine alive. Its page is a strip of iframe tabs pointing at `127.0.0.1`, so it never worked
  from a phone. It knows nothing about sessions.
- **Pairing** (built): one 32-byte token per monitor, 0600 in its state directory. `--pair-phone`
  shows a QR for `<address>/pair#code=<code>`, a single-use five-minute code; `POST /api/pair`,
  answered ahead of the gate, swaps it for a cookie whose value is the token itself. The gate also
  takes `Authorization: Bearer`. A request through `tailscale serve` is never the same-user bypass.
- **The write and file bar**: `origin_ok` asks that `Host` and `Origin` each be the loopback or a
  trusted host (the names `tailscale serve` relays privately to this port), and a request with no
  `Origin` passes. There is no CORS anywhere.
- **So a native client needs no monitor change**: it sends no `Origin`, sends `Host` as the monitor's
  tailnet name, carries the token as a bearer, and reads that token from the pairing answer's
  `Set-Cookie`, which native code can see and page script cannot. CORS does not apply to it.
- **The app shell is bound to its own origin**: 22 distinct root-absolute routes at 74 places in 17
  files (`/file` alone 18 times; `/records`, `/pull`, `/search`, `/turn-index`, `/api/*`,
  `/markdown`, `/image`, `/__reveal`, `/mdrev/<version>/`, `/codex-workspace/*`). Whether the reader
  is at this machine (reveal or download, windowed reading) is read from the page's own origin.
- **The session list's shape**: groups of rows, `cwd` on the group, nothing naming the machine.
  Session ids are UUIDs, but **project keys collide**: the same `~/code/x` on two machines gives the
  same group key. `/file`, `/__reveal` and mdrev's routes carry no session id, and their stamps are
  signed by a per-machine key: a stamp is good only at the machine that minted it.
- **Arrange** (#s387) offers "like the Claude app", "like the Codex app" and "like Herdr", each read
  from that machine's own app (`/api/layout`).

## 3. Decisions

### D1 — Where the merged view runs: on the reader's device

| | (a) **Native app** (the owner's choice) | (b) Hub (draft 1) | (c) A page calling N monitors with CORS | (d) The fleet's tabs |
|---|---|---|---|---|
| Aggregates at | the reader's device | one server | the reader's browser | nowhere |
| Machines around the world | each reached directly | every request via the hub's location | each reached directly | per tab |
| A point that must be up | none | the hub | none | the fleet's machine |
| Credentials | Keychain on the device | N tokens on the hub | N tokens in browser storage | per tab |
| Monitors change | no | no | CORS and token hand-off on every one | no |
| Phone | a native app | a web app | a web app, N pairings | dead tabs |

The owner chose (a): with machines spread around the world, an aggregation point in one place adds
latency and a dependency on one machine, and holds every token in one place. (c) changes every
monitor's gate and keeps N full credentials where any injected script can read them. (d) is not one
list and does not work from a phone.

### D2 — The app shows today's shell, not a second UI

The app is a thin native shell around the web app shell, not a rewrite. A `WKWebView` loads the
shell from a custom URL scheme (`app://`), and a `WKURLSchemeHandler` answers every request the
page makes. Its own pieces are native: the device list, pairing (a QR scanner), the Keychain, and
the scheme handler. Everything the reader sees inside a session is the same JavaScript and CSS the
monitor serves, so a fix to the shell reaches the app with no second implementation. A fully native
UI (SwiftUI throughout) is the alternative. It would read better as a native app, but it is a second
renderer to keep in step with the first forever, which `design/claude-monitor.md` §12 ruled out
("not a second renderer").

**Where the shell's bytes come from.** Each monitor already serves them. The app loads them from one
paired monitor (the first that answers, at the version it serves), and keeps a copy for when none
answers. A shell can then talk to a monitor of another version, and some of its routes are newer
than some installed monitors (the turn index, server search, records by turn). So the shell
declares the oldest monitor version it works with, a device older than that is listed with a note
to upgrade it rather than read wrongly, and the routes inside that window must keep working across
versions, as they already must for a page that outlives its monitor (#s352). Which window is
realistic is a phase-0 spike.

### D3 — The relay on the device: the scheme handler

The page's requests go to `app://d/<device>/<path>`. The handler:

- maps `<device>` to that machine's address (its `tailscale serve` name and port) and token;
- sends the request there natively, with `Host` set to that name, no `Origin`, the token as a bearer,
  and the page's own body, so the monitor's `origin_ok` holds and its writes work;
- relays the answer's status, headers and body back to the page, streaming large ones;
- relays only to paired devices, by device id, never to an address a request names, with the path
  normalized first, so the page cannot reach anything that was not paired;
- never relays `/__reveal`, which opens the file manager on the machine that serves it, a side effect
  on that machine's desktop, except to the monitor on the Mac the app runs on.

The token never enters the web view: the page cannot read it and no script in it can leak it.

### D4 — Pairing and revoking

**Add a device**: on that machine, `agent-monitor --pair-phone` shows its QR as today. The app scans
it, reads the address and the code, redeems the code with `POST /api/pair`, takes the token from the
answer's `Set-Cookie`, and keeps it in the Keychain with the device's name, address and identity.
Nothing is added to the monitor. **Forget a device**: delete it in the app. **A leaked phone**: every
paired device holds the same token per monitor, and monitors have no rotate today; `--pair --rotate`
on the monitor is a follow-up the owner may want before writes cross devices (D9).

The device's identity is the address it was paired at: its tailnet name (one per device on a
tailnet, and stable). No monitor route says who a monitor is, so the same machine paired under two
names lists twice; a route that names the machine would be a monitor change, for §5 if it is
wanted.

### D5 — Stage 0, shared with `design/embedding.md`: the data source and the backend handle

Two layers, agreed with `design/embedding.md` (#s406) so neither shell change is done twice. That
design adds the stage after this one, the views mounting inside a host's element.

- **The backend handle** (layer B): what the shell needs to reach one monitor. A *base* (empty for
  today's same origin; `app://d/<device>` in the app; an absolute origin for an embed); how
  credentials ride (the ambient cookie, the app's relay, or a bearer); whether the reader is *local*
  to that monitor (reveal or download; windowed reading); and where that monitor's mdrev bundle loads
  from (each machine may pin a different mdrev). Every one of the 74 root-absolute paths goes through
  it.
- **The data source** (layer A): what the views consume. Index rows; a session's records, snapshot
  plus growth, with the #165 rewrite rule; the turn index; search; files; writes as optional
  capabilities (send, dialog, consent). Three implementations: a *monitor source* over a handle
  (today's behaviour), a *merged source* over several (this design), and a host-fed source
  (`design/embedding.md`).

Stage 0 changes no route, no JSON and nothing a reader sees. It is page code, so it is held by the
browser suite on both surfaces and walked on the iOS Simulator before it ships. The monitor's own
page keeps working exactly as today: it is the shell with one handle whose base is empty.

### D6 — Merging the session list, in the page

- The merged source polls each device's `/api/sessions` through its handle, on the list's clock,
  with a per-device timeout, and keeps each device's last good answer, marked stale. One slow
  machine never holds the list (R6). Merging in the page, not in Swift, keeps one implementation for
  the app and anything else that hosts the shell.
- Each answer is tagged in the page with its device, and ids and group keys are namespaced by device
  (`<device>/<id>`), so the same `~/code/x` on two machines stays two groups. No monitor's
  `/api/sessions` gains a field.
- The session filter's buckets and counts sum across devices. Recency compares each machine's own
  clock; tailnet machines keep time well enough for "the last six hours".
- A machine that does not answer keeps its last rows, greyed, under a head saying since when. One
  that never answered is a head saying why.
- Cost: one list answer per machine per poll (about 13 KB gzipped). The app polls only while it is
  open, as iOS suspends it otherwise.

### D7 — The by-device arrangement

A new Arrange choice, "By device", beside "By agent and project" and the app arrangements: an outer
head per device, the device's sessions under it, arranged as the reader chose for the rest of the
list. An app arrangement (like the Claude app, the Codex app, Herdr) is read per device from that
device's `/api/layout`, since each machine has its own app. Every other choice shows one merged
list, each row wearing its device as a small label.

### D8 — Everything about a session goes to its own machine

The merged source routes every per-session call (records, growth, the turn index, search, the panes'
lists) to the row's monitor source. Files, reveal and mdrev carry stamps signed by the machine that
offered them, so the page remembers, per record, which device it came from, and asks that one. The
mdrev guest loads that device's bundle. **Locality is per device**: on the Mac the app runs on, the
monitor of that same Mac is local (reveal works); every other device is remote, its files download,
and long sessions read windowed. On a phone nothing is local.

### D9 — Writes across devices

The compose box, dialogs and pane grants work on any device's session, under that monitor's own
gate: the token the app holds, and its consent, kept per pane and pid on that machine. **Choice for
the owner**: writes in the first release with the rest (parity, R3), or a read-only first release
with writes a stage later, after `--rotate` exists.

### D10 — Machines off the tailnet

Today corp machines are reached only by ssh, through the fleet's tunnels (fleet-pairing D1). An iOS
app cannot run those tunnels. The Mac app can, by keeping the fleet's tunnel code: a tunnelled
monitor is then a device at a loopback port, reached through the same relay. So on a Mac every
machine the fleet reaches today is in the list, and on a phone only the machines on the tailnet. The
alternative would be one machine relaying for the others, which is the remote point the owner ruled
out.

### D11 — Notifications: the owner's choice

With no server of its own, a push needs something to send it through Apple's push service. Either
**each monitor sends** "a session on <device> needs you" (an APNs key on every Mac, and a payload
with no session content, since it leaves the tailnet through Apple's servers), or **there are
none**, and the app shows attention only while it is open. Background refresh on iOS is
opportunistic and is not a substitute. Recommended for the first release: none.

### D12 — The fleet's tabs retire

Once the Mac app covers what the tabs did, the fleet's page is retired. Its tunnel code moves into
the Mac app (D10), and its binary and formula are retired with a migration note, in the way the
classic view was (#s98).

## 4. Security

- **No monitor exposes more than today.** Each sees the app as one more paired phone, over the same
  tailnet path.
- **Tokens stay in the Keychain**, device-only, and never enter the web view, a log or a URL.
- **The relay reaches only paired devices**, by id, with the path normalized: the page cannot make
  it reach a host or a route that was not paired.
- **The web view loads only the app's own scheme**; a navigation anywhere else opens in the browser,
  never inside it, so no other page can call the relay. That also covers `/api/ignore`, a GET that
  writes with no origin check today.
- **Reveal is never relayed** to another machine (D3).
- **Masking is each monitor's.** The app reaches a tailnet monitor by its tailnet name, so that
  monitor masks secrets for it as for any client across a network (#365). Two monitors are reached
  on the loopback and mask nothing: the one on the app's own Mac, which is the reader's own machine,
  and one through an ssh tunnel (D10), exactly as the fleet's tabs reach it today, its content
  reaching only the reader's own Mac over ssh. That is a limitation to name: the app adds no masking
  of its own.
- **What crosses the network**: the reader's device to each machine, over the tailnet (WireGuard),
  and, if the owner chooses pushes, a content-free notice through Apple's push service. Nothing else
  leaves the tailnet.

## 5. Contracts this creates or changes, and who relies on them

Each is the owner's to approve:

- **A new app**: its repository, its bundle id, its signing and how it is installed (TestFlight, an
  ad hoc build, the App Store). These need the owner's Apple developer account.
- **The app shell's backend handle (stage 0)**: internal to the page, no route or JSON change. It
  becomes a contract when `design/embedding.md`'s embed or this app ships.
- **The routes the app relies on across versions** (D2): the shell's routes become a contract
  between monitor versions, already true for a page that outlives its monitor.
- **The fleet's retirement** (D12): its page, binary, formula and the `monitor-fleet` integration.
- **On the monitor, at most one optional flag**: `--pair --rotate`, to mint a new token (D4), if the
  owner wants it before writes cross devices. No route, field or gate change.

## 6. Phases

| Phase | Ships | Accept |
|---|---|---|
| 0a | Spikes: a `WKURLSchemeHandler` relaying POST bodies and streaming a large `/records` range; the web view's storage and cookies for a custom scheme (the shell keeps its preferences in `localStorage`); the mdrev bundle loading per device through the scheme; whether a custom scheme is a secure context for the APIs the shell uses | Each answered by measurement on the iOS Simulator and a device, written down |
| 0b | The backend handle and the data source in the shell (shared with `design/embedding.md`) | No change a reader can see: every browser case green on both surfaces, a Simulator walk |
| 1 | The iOS app: pairing by QR, the Keychain, the relay, the merged list (read-only) | Paired with two machines, the app lists both; an offline machine costs only its own rows |
| 2 | "By device", per-device locality, the session view, files and mdrev routed per row | Reading and searching a session on another machine behaves as on that machine |
| 3 | Writes across devices (or in 1, by the owner's choice), `--rotate` on the monitor | The compose box, a dialog and a pane grant on another machine's session, each refused where today's rules refuse it |
| 4 | The Mac app with the fleet's tunnels; the fleet retired | A machine reached only by ssh appears in the Mac app's list |
| 5 | Notifications, if the owner chooses them (D11) | — |

## 7. What this is not

- **Not a change to the monitor.** No route, field or gate; the one optional flag, `--rotate`, is
  §5's.
- **Not a server.** Nothing runs anywhere but the monitors and the app on the reader's device.
- **Not a second renderer.** The app shows the monitor's own shell (D2).
- **Not a sync service.** The app stores no session content beyond each machine's last list answer
  and what the page already caches.

## 8. Open questions for the owner

1. **The app's code and distribution** (§5): a new private repository, signed with the owner's
   developer account and installed through TestFlight (recommended), or kept for personal ad hoc
   builds. Phase 1 waits on this.
2. **The UI** (D2): today's shell in a web view (recommended), or a fully native UI.
3. **Writes** (D9): in the first release with the rest, or read-only first and writes after
   `--rotate`.
4. **Notifications** (D11): none in the first release (recommended), or pushes sent by each monitor.
