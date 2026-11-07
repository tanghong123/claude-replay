# Design: one Agent Monitor over many machines — the hub (#s407)

> **DRAFT — PROPOSED, nothing built.** It goes to the owner as a decision before any code. It
> amends `design/claude-monitor.md` §12 ("Not multi-machine") the way `design/monitor-fleet.md`
> did: the monitor stays a single-machine service, and the thing that spans machines is a separate
> binary. It supersedes the fleet's page of tabs and builds on what `design/fleet-pairing.md`
> shipped (the gate, the token, the one-time pairing code). Its stage 0 is shared with
> `design/embedding.md` (#s406, the session view embedded in another web app), which cites it.

**What it is for.** A person with agents on several machines wants one Agent Monitor: pair it once
with each machine, then see every session in ONE session list, each row saying where it runs, with
a new way to arrange the list by device first. Everything else reads and behaves as the monitor
does today: the session view, search, the panes, the compose box and dialogs, files and mdrev. On a
phone too, saved to the Home Screen.

**The recommendation in three lines.** A *hub*: one origin, served by the fleet binary rebuilt, that
the browser pairs with once, and that relays to each machine's monitor over the tailnet (or an ssh
tunnel, as the fleet does today) with that monitor's own token. The hub merges the session lists and
routes everything about a session to the monitor it runs on. No monitor changes its routes, its gate
or its exposure. The app shell first learns to reach its monitor through a *backend handle* instead
of its own origin (stage 0, shared with #s406).

---

## 1. Requirements

| | |
|---|---|
| **R1** | One session list over N machines' monitors; each row says which device it runs on. |
| **R2** | An Arrange choice "by device", then each device arranged as the reader already chose. |
| **R3** | Otherwise the same look and feel: the session view, search, panes, files, mdrev, compose and dialogs, for a session on any paired device. |
| **R4** | Pairing is once per backend, from the hub; the browser (desktop or phone) pairs once, with the hub. A backend can be revoked. |
| **R5** | Works in Safari on a phone and as a Home Screen app, as the monitor does today. |
| **R6** | A backend that is offline or slow costs its own rows only: the list still answers. |
| **R7** | Nothing new is exposed by default; corp machines keep their one path, ssh (fleet-pairing D1). |
| **R8** | No monitor is made multi-machine: its routes, gate and §11 exposure rule are unchanged. |

## 2. What exists, and the facts the design rests on

- **The fleet** (`agent-monitor-fleet`, built, `design/monitor-fleet.md`) reads its topology from a
  config file, probes each machine over `ssh host sh -s` for the monitor's port (from its `LOCK`) and
  a machine id (which collapses two aliases of one machine), and keeps one `ssh -L` tunnel per
  machine alive with backoff. Its page is a strip of iframe tabs, one monitor each. It knows nothing
  about sessions, and it never worked from a phone: the tabs point at `127.0.0.1`. fleet-pairing's
  `/h/<idx>/` proxy and `--listen tailscale` were never built.
- **Pairing** (built): one 32-byte token per monitor, 0600 in its state directory, shared by every
  paired device. `--pair-phone` mints a single-use five-minute code; `POST /api/pair`, answered ahead
  of the gate, swaps it for a cookie whose value is the token itself (`cmauth-<8 hex>`, HttpOnly,
  `SameSite=Strict`, host-scoped). The gate also takes `Authorization: Bearer`. A request through
  `tailscale serve` is never the same-user bypass, so every tailnet client must pair.
- **The write and file bar**: `origin_ok` asks that `Host` and `Origin` each be the loopback or a
  trusted host (the names `tailscale serve` relays privately to this port, no `*.ts.net` wildcard).
- **No CORS anywhere.** A cross-origin preflight is refused at the gate with 401.
- **The app shell is bound to its own origin.** It names 22 distinct root-absolute routes at 74
  places in 17 files (`/file` alone 18 times; `/records`, `/pull`, `/search`, `/turn-index`,
  `/api/*`, `/markdown`, `/image`, `/__reveal`, `/mdrev/<version>/`, `/codex-workspace/*`), so it
  cannot be served under a sub-path as fleet-pairing D2 assumed, nor pointed at another machine.
  Whether the reader is at this machine (reveal or download, windowed reading of a long session) is
  read from the page's own origin (`revealHere`, `WINDOWED`).
- **The session list's shape**: groups of rows, `cwd` on the group. Nothing names the machine; `pid`,
  `sock` and `target` are meaningful only on it. Session ids are UUIDs and do not collide across
  machines in practice, but the page keys everything by the bare id, and **project keys do
  collide**: the same `~/code/x` on two machines gives the same group key, and the list would merge
  them. `/file`, `/__reveal` and mdrev's routes carry no session id, and their stamps are signed by a
  per-machine key: a stamp is good only at the machine that minted it.
- **Arrange** (#s387) just gained "like the Claude app", "like the Codex app" and "like Herdr", each
  read from that machine's own app (`/api/layout`).

## 3. Decisions

### D1 — Where the merged view runs: a hub, one origin relaying to N

| | (a) **Hub** (recommended) | (b) Static client, N origins | (c) The fleet's tabs, kept |
|---|---|---|---|
| How | One origin serves the shell and relays every request to the backend it is for | The page calls each monitor's origin directly with CORS | One iframe per monitor, as today |
| R1/R2 one list, by device | Yes, merged at the hub | Yes, merged in the page | No: one list per tab |
| R4 pairing | The browser pairs once with the hub; the hub pairs with each backend | The browser pairs with every backend | Per tab, and never from a phone |
| R5 phone | One origin, one pairing, as today | N origins; whether a cookie set while pairing a second origin from a Home Screen app lands in its store is unmeasured | The tabs point at `127.0.0.1`: dead on a phone |
| R7/R8 monitors | Unchanged: the hub is one more paired client | Every monitor gains CORS, `OPTIONS`, an allow-list of client origins in the origin check, and a way to hand page script a token | Unchanged |
| Corp machines (ssh only) | The hub keeps the fleet's tunnels | Unreachable from a browser: a hub is needed anyway | Yes |
| Credentials | N tokens at the hub, 0600, never in a browser | N full tokens in browser storage, each exposed to any script injection | Each tab's own cookie |
| Load on a phone | One list poll | N list polls every 5 s | N pages |

(c) is the no-change option, judged against the goal: it fails R1, R2 and R5, which are the goal.
(b) changes every monitor's gate and auth surface (the owner's), still needs a hub for corp
machines, and puts N credentials where any injected script can read them. (a) changes no monitor
and gives the phone exactly today's single-origin pairing.

### D2 — The hub is the fleet binary, rebuilt

The hub is `agent-monitor-fleet`, not a flag on the monitor and not a third binary. The monitor
stays what `design/monitor-fleet.md` §2 argued it must be: a process whose security story is that
it serves only its own machine. Giving it remote reach would put an aggregator inside that story.
The fleet already owns the parts the hub needs: topology as data, the probe and its machine
identity, and the tunnel supervisor with its orphan guard. Its page of tabs is what is replaced. Its
formula and command keep their name, so nobody installs a new tool; a rename can come later. The
shell's code comes from the monitor crate, as served today (`ui::asset()`, public in its library),
so the hub never carries a copy that drifts. The fleet crate gains a dependency on the monitor's
library for it.

### D3 — Reaching each backend

A backend is a monitor the hub relays to, reached one of two ways, both data in the hub's config:

- **Tailnet**: the backend's `tailscale serve` name and port, the way a phone reaches it. The hub
  sends `Host` as that name, no `Origin`, and the backend's token as a bearer, so the backend's
  `origin_ok` holds and its writes work. Never a public address: the hub refuses a backend address
  outside the tailnet, and never via Funnel.
- **ssh tunnel**: the fleet's tunnel, unchanged, for machines off the tailnet. Corp machines keep
  this one path (fleet-pairing D1).

The hub's own machine, when it runs a monitor, is a backend like any other, reached on the loopback.
The hub never reads a transcript itself.

### D4 — Credentials: the hub pairs with each backend as a phone does

**Tailnet backend**: run `--pair-phone` there (a QR and a code, as today), and enter the code in the
hub's "Add a device" form (or `agent-monitor-fleet pair <address>`). The hub redeems it with
`POST /api/pair` and keeps the token the answer's cookie carries, 0600 in its own state directory.
No backend route is added. **ssh backend**: through a tunnel the backend sees a loopback peer, so
what it asks depends on its platform, as today (fleet-pairing D3b and §4.2). A Linux backend admits
the tunnel's owner with no token. An unpaired Mac backend admits the loopback outright. A paired Mac
backend needs its token, which the hub reads over ssh as the same user, who could read it anyway.
**Revoke**: forget the device at the hub. A leaked hub token
store is every backend's token, and backends have no rotate today; adding `--pair --rotate` is a
follow-up the owner may want before writes cross devices (D9).

An alternative is a per-hub credential on each backend (a second token with its own scope). It
changes every backend's auth file and gate (the owner's), and buys revoking the hub without
re-pairing phones. Not recommended for the first stages.

### D5 — Stage 0, shared with #s406: the data source and the backend handle

Two layers, agreed with `design/embedding.md` (#s406) so neither shell change is done twice. That
design adds the stage after this one, the views mounting inside a host's element.

- **The backend handle** (layer B): what the shell needs to reach one monitor. A *base* (empty for
  today's same origin; a prefix such as `/d/<device>` at the hub; an absolute origin for an embed);
  how credentials ride (the ambient cookie, or a bearer); whether the reader is *local* to that
  monitor (reveal or download; windowed reading); and where to load that monitor's mdrev bundle from
  (each backend may pin a different mdrev). Every one of the 74 root-absolute paths goes through it.
- **The data source** (layer A): what the views consume. Index rows; a session's records, snapshot
  plus growth, with the #165 rewrite rule; the turn index; search; files; writes as optional
  capabilities (send, dialog, consent). Three implementations: a *monitor source* over a handle
  (today's behaviour), a *merged source* over several (this design), and a host-fed source (#s406).

Stage 0 changes no route, no JSON and nothing a reader sees. It is page code, so it is held by the
browser suite on both surfaces and walked on the iOS Simulator before it ships.

### D6 — Merging the session list

- The hub polls each backend's `/api/sessions` on its own clock (each answer at most a poll old,
  per-backend timeout, the last good answer kept and marked stale), so the browser makes one list
  poll whatever N is (R6).
- The merged answer is the hub's own route. Each backend's rows are **tagged by the hub** with their
  device, and ids and group keys are namespaced by device (`<device>/<id>`), so the same `~/code/x`
  on two machines stays two groups. No monitor's `/api/sessions` gains a field.
- A device's identity is its tailnet name for a tailnet backend (one per device on a tailnet, and
  stable), and the probe's machine id for an ssh backend. It is labelled with the name the person
  gave it at pairing. The probe also reports the machine's tailnet name when it has one, so a
  machine configured both ways is refused as a duplicate at pairing rather than listed twice.
- The session filter's buckets and counts sum across devices. Recency compares each device's own
  clock; a tailnet's machines keep time well enough for "the last six hours".
- A device that does not answer keeps its last rows, greyed, under a head saying since when. One that
  never answered is a head saying why.

### D7 — The by-device arrangement

A new Arrange choice, "By device", beside "By agent and project" and the app arrangements: an outer
head per device, the device's sessions under it, arranged as the reader chose for the rest of the
list. An app arrangement (like the Claude app, the Codex app, Herdr) is read per device from that
device's `/api/layout`, since each machine has its own app. Every other choice shows one merged
list, each row wearing its device as a small label.

### D8 — Everything about a session goes to its own backend

The merged source routes every per-session call (records, growth, the turn index, search, the panes'
lists) to the row's monitor source. Files, reveal and mdrev carry stamps signed by the backend that
offered them, so the page remembers, per record, which backend it came from, and asks that one. The
mdrev guest loads that backend's bundle. **Locality is per device**: a reader is local only to the
monitor on the machine they are sitting at (the hub's own machine, opened on the loopback). There
they may reveal in the file manager; for every other device they download, and long sessions read
windowed, as a phone does today.

### D9 — Writes across devices

The compose box, dialogs and pane grants work on any device's session under both gates: the hub's
(the browser is paired with the hub, its `Host` and `Origin` are the hub's own) and the backend's
(its token, held by the hub; its consent, kept per pane and pid on that machine). The hub forwards
no browser cookie, `Origin` or `Authorization` to a backend. **Choice for the owner**: writes in the
first release with the rest (parity, R3), or a read-only first release with writes a stage later,
after `--rotate` exists.

### D10 — The phone: a web app, no native app

One origin is exactly today's phone story: pair once in Safari, save to the Home Screen, and the
Home Screen app takes Safari's cookies at that moment (#s111). The merged view needs no native app.
A native app is needed only for widgets, Live Activities, answering from the lock screen,
background refresh without a push, or keeping tokens in the Keychain. Web Push (iOS 16.4+, a Home
Screen app with a service worker and a permission granted from a tap) could later say "a session
needs you". Its notifications travel through Apple's push service over the internet, so their
payload would carry no session content. That is a separate design.

### D11 — The fleet's tabs retire

Once the merged page covers what the tabs did, it replaces them in the same binary. The config file,
the probe and the tunnels stay. A person with a fleet config sees the merged list where the tabs
were, after pairing each device (ssh backends need no pairing step). The integration's Skill and
slash command are updated with it.

## 4. Security

- **No monitor exposes more than today.** A backend sees the hub as one more paired client, on the
  same tailnet or ssh path a phone or the fleet already uses.
- **The hub is a paired, gated server**: the shared `AuthGate`, its own token and its own
  `--pair-phone`. Its relay routes, its merged list and its pairing form are behind it.
- **It relays only to configured backends**, by device id, never to an address a request names, and
  normalizes the path before relaying, so a request cannot reach a host or route that was not
  configured (no SSRF).
- **Writes are checked at the hub before they leave it**: `origin_ok` on the browser's request, the
  route's own method, and nothing that writes on a GET relayed for a cross-site request
  (`/api/ignore` is a GET that writes today, with no origin check).
- **Reveal is never relayed.** `/__reveal` opens the file manager on the machine that serves it, a
  side effect on that machine's desktop. The hub refuses it for every backend but the one on its own
  machine, opened from the loopback, whatever the page asks.
- **Masking is the hub's too.** A backend masks secrets for a client whose `Host` is not the
  loopback. Through an ssh tunnel, or for the hub's own machine on the loopback, a backend sends its
  content unmasked to the hub. So the hub applies the mask policy itself to everything it serves a
  client across a network, exactly as a monitor does (#365).
- **Tokens stay at the hub**: 0600, never sent to a browser, never in a log, never in a URL.
- **What crosses the network**: browser to hub, as today's phone to its monitor (WireGuard on the
  tailnet); hub to backend over the tailnet, or over ssh for corp machines. Nothing goes to a
  third-party relay.

## 5. Contracts this creates or changes, and who relies on them

Each is the owner's to approve:

- **The fleet's page and config**: the tabs go; the config gains a backend's address, transport and
  label. Relied on by whoever runs the fleet today and by the `monitor-fleet` integration.
- **New hub routes**: the merged list, the device list, the relay under `/d/<device>/`, and the
  pairing form. Nothing else relies on them yet.
- **New CLI on the fleet binary**: `agent-monitor-fleet pair <address>` to pair with a backend, and
  its own `--pair-phone`, which finds the `tailscale serve` entry relaying to the hub's port (the
  monitor's finds its own) and pairs a browser with the hub.
- **A new dependency**: the fleet crate on the monitor's library, for the shell's bytes (D2).
- **The app shell's backend handle (stage 0)**: internal to the page, no route or JSON change. It
  becomes a contract when #s406's embed ships.
- **On the monitor, at most one optional flag**: `--pair --rotate`, to mint a new token (D4), in
  phase 3 if the owner wants it before writes cross devices. No route, field or gate change. That is
  the point of D1 and D2.

## 6. Phases

| Phase | Ships | Accept |
|---|---|---|
| 0 | The backend handle and the data source in the shell (shared with #s406) | No change a reader can see: every browser case green on both surfaces, a Simulator walk |
| 1 | The hub: config, pairing with tailnet backends, the relay (read-only), the merged list with device labels | A desktop and a phone, each paired once with the hub, list sessions from two machines; an offline machine costs only its own rows |
| 2 | "By device", per-device locality, the session view, files and mdrev routed per row | Opening, searching and reading a session on another machine behaves as on that machine, files downloading rather than revealing |
| 3 | Writes across devices (or in 1, by the owner's choice), `--rotate` on the monitor | The compose box, a dialog and a pane grant on another machine's session, each refused where today's rules refuse it |
| 4 | ssh backends folded in; the tabs retired; the integration updated | A corp machine reached only by ssh appears in the merged list |

## 7. What this is not

- **Not a change to the monitor.** No route, field or gate; the one optional flag, `--rotate`, is
  §5's.
- **Not a sync service.** The hub stores no session content: it relays and merges what it is
  answered, and keeps only each backend's last list answer.
- **Not a relay off the tailnet.** No third-party service carries anything.
- **Not a cluster manager.** It starts no monitor and installs nothing on a backend.
- **Not a native app.** See D10.

## 8. Open questions for the owner

1. **The architecture** (D1, D2): the hub as the fleet rebuilt (recommended), a static client with
   CORS on every monitor, or keeping the fleet's tabs.
2. **Writes** (D9): in the first release with the rest, or read-only first and writes after
   `--rotate`.
3. **Credentials** (D4): the hub keeps each backend's own token (recommended for now), or each
   backend mints a separate hub credential.
