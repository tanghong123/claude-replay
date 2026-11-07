# Design: one Agent Monitor over many machines — a native app (#s407)

> **DRAFT 3 — the shape is decided, nothing built.** Draft 1 proposed a hub, one server that relays
> to every monitor. The owner chose a native app instead (#s411, 2026-11-07): "I have devices spread
> across the globe, and having a remote aggregation point will add more constraints." He then set
> Agent Monitor's direction as an embeddable module and a standalone app built from two components
> (`design/embedding.md`, #s408), and said yes to this app being one more host of those components
> with its own chrome (#s415). Its code lives in a private repository,
> `tanghong123/agent-monitor-app`, installed through TestFlight on his own account (#s413, #s414).
> This draft designs that app. The monitor stays a single-machine service
> (`design/claude-monitor.md`, "What this is not"); what spans machines is an app on the reader's
> own device, talking to each monitor as a phone already does. It supersedes the fleet's page of
> tabs (`design/monitor-fleet.md`) and builds on what `design/fleet-pairing.md` shipped (the gate,
> the token, the one-time pairing code).

**What it is for.** A person with agents on machines around the world wants one Agent Monitor: pair
it once with each machine, then see every session in ONE session list, each row saying where it
runs, with a new way to arrange the list by device first. Everything else reads and behaves as the
monitor does today: the session view, search, the panes, the compose box and dialogs, files and
mdrev. On a phone, and on a Mac.

**Two words.** A *machine* is a computer whose monitor the app reaches. The reader's *device* is the
phone, iPad or Mac the app runs on. (The owner's "by device" arrangement groups by machine; its label
keeps his word.)

**The design, in short.** A native app, for iPhone, iPad and Mac from one codebase. It pairs with
each machine directly, by the QR code `--pair-phone` already shows, and keeps each token in the
Keychain. In a web view it mounts the two components `design/embedding.md` builds, the session list
and the session view, from the kit it bundles. Their requests go to a custom URL scheme whose handler
is a relay on the device itself: it sends each request to the machine it is for, over the tailnet,
with that machine's token. A merged source lets the session list read every machine at once, and a
chosen session opens on its own machine. The app needs no change to any monitor.

---

## 1. Requirements

| | |
|---|---|
| **R1** | One session list over N machines' monitors; each row says which machine it runs on. |
| **R2** | An Arrange choice "By device", then each machine arranged as the reader already chose. |
| **R3** | Otherwise the same look and feel: the session view, search, panes, files, mdrev, compose and dialogs, for a session on any paired machine. |
| **R4** | Pairing is once per machine on each of the reader's devices, from the app; a machine can be forgotten. |
| **R5** | Works on a phone and on a Mac, with no server of its own anywhere: each request goes from the reader's device straight to the machine it is for. |
| **R6** | A machine that is offline or slow costs its own rows only: the list still answers. |
| **R7** | Nothing new is exposed: each monitor is reached exactly as a reader already reaches it today, a paired phone over the tailnet, the reader's own browser on This Mac, the fleet's tabs through a tunnel. |
| **R8** | No monitor is made multi-machine: its routes, its gate and its exposure rule (`design/claude-monitor.md`, "Exposure") are unchanged. |

## 2. What exists, and the facts the design rests on

- **The fleet** (`agent-monitor-fleet`, built) reads its topology from a config file, probes each
  machine over ssh for the monitor's port and a machine id, and keeps one `ssh -L` tunnel per
  machine alive, each on a local port the kernel picks. Its page is a strip of iframe tabs pointing
  at `127.0.0.1`, so it never worked from a phone. It knows nothing about sessions.
- **Pairing** (built): one 32-byte token per monitor, 0600 at `<state>/auth-token`, minted by
  `--pair` (a monitor paired before the move to the state directory still has it at
  `<cache root>/auth-token`, which is read too). `--pair-phone` shows a QR for
  `<address>/pair#code=<code>`, a single-use five-minute code; `POST /api/pair`, answered ahead of
  the gate, swaps it for a cookie whose value is the token itself. The gate also takes
  `Authorization: Bearer`, and answers a request that presents the token that way with the same
  cookie, which lasts (it has a `Max-Age`). A paired gate never lets a request through
  `tailscale serve` in without the token. An unpaired one on macOS does: it admits a loopback peer it
  cannot verify, and `tailscale serve` connects from the loopback (#s417).
- **The write and file bar**: the gate's `Host` and `Origin` check (`foreign_header`) asks that each
  be the loopback or a trusted host (the names `tailscale serve` relays privately to this port), and
  a request with no `Origin` passes. There is no CORS anywhere.
- **So a native client needs no monitor change**: it sends no `Origin`, sends `Host` as the monitor's
  tailnet name, carries the token as the gate's own cookie (D3), and reads that token from the
  pairing answer's `Set-Cookie`, which native code can see and page script cannot. CORS does not
  apply to it.
- **The app shell is bound to its own origin**: 22 distinct root-absolute routes at 74 places in 17
  files (`/file` alone 18 times; `/records`, `/pull`, `/search`, `/turn-index`, `/api/*`,
  `/markdown`, `/image`, `/__reveal`, `/mdrev/<version>/`, `/codex-workspace/*`). Whether the reader
  is at this machine (reveal or download, windowed reading) is read from the page's own origin. The
  request layer (D5, #s416) is what removes both assumptions.
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

### D2 — The app hosts the two components (decided, #s415)

The app is one more host of the components `design/embedding.md` builds ("The pieces", 2 and 3):
the **session list**, with its filter and arrangements, and the **session view**, with its turn bar,
search, outline panes, artifact pane and compose box. It mounts them in a `WKWebView`, and a
`WKURLSchemeHandler` answers every request they make (D3). Its own chrome is native, and is all it
adds:

- the machine list and pairing (a QR scanner, D4), and the Keychain;
- the relay (D3);
- placing the two components: the list in a drawer over the view on a phone, side by side on an
  iPad or a Mac, as the standalone app places them ("The standalone app becomes a host");
- on a phone, the one-row top bar, which the standalone app keeps as a host too. What it shows
  belongs to the session (its title, the current turn, Info), so the session view has to tell its
  host those and take an "open Info" from it. "Customizing" has no such events yet, for either host:
  an addition to the kit, section 5;
- on a Mac, a session or an artifact in a window of its own (D8).

Each component lays itself out from the device and the size of the pane the app gives it ("Layout
decides itself"), and takes the app's theme and its handlers for events ("Customizing"). So there is
one session list and one session view, the same the monitor's own page and the collaboration app
use, and none can drift. A fully native UI was the alternative, a second renderer to keep in step
forever, which `design/claude-monitor.md` ruled out ("Not a second renderer").

**Versions.** The app bundles one kit, pinned and upgraded on purpose ("Delivery"), while each
paired machine answers the backend contract in its own version. The kit says which contract
versions it reads ("1. The backend contract"), so a machine whose monitor is outside them is listed
with a note to upgrade it rather than read wrongly. How the app learns a monitor's contract version
is for embedding's step 2 to settle (section 5).

### D3 — The relay on the device: the scheme handler

Everything the web view loads comes from one origin, `app://monitor`:

- `app://monitor/kit/…` is the app's own page, the kit and the mdrev the app pins, served from the
  app's bundle;
- `app://monitor/k/<key>/m/<machine>/<path>` is relayed to that machine, where `<key>` is a secret
  the app makes at each launch and gives only its own page (below);
- an agent's HTML page, which the session view shows in a frame, comes from a scheme of its own
  (`app-frame://`, below).

One origin means one `localStorage` for the reader's preferences on every machine, which is what a
reader expects. What the session list keeps per session is keyed by the namespaced id (D6); the
session view keys by the session's own id, a UUID, which no two machines share.

The handler, for a relayed request:

- maps `<machine>`, the name of the machine's entry in the app, to its address (its
  `tailscale serve` name and port) and token;
- sends the request there natively, with `Host` set to that name, no `Origin`, and the page's own
  body. It forwards only the request headers on a short list (`Accept`, `Content-Type`, `Range`,
  `Last-Event-ID`, `If-None-Match` and the like), so an `Authorization`, `Cookie` or `Origin` the
  page sets never reaches the gate, and it drops a `token` query parameter, which the gate would
  read before its cookie. The token rides as the gate's own cookie, set by hand (`Cookie: cmauth-<8
  hex>=<token>`, the name the gate derives from its token), on a session with no cookie store at
  all. A bearer would be answered with a lasting `Set-Cookie` holding the token, and a bearer on `/`
  with a redirect every time; the gate's own cookie gets neither;
- relays the answer's status and body back to the page, streaming large ones, and its headers less
  any `Set-Cookie`, and less `Content-Encoding` and `Content-Length` where `URLSession` has already
  decoded a gzipped body (a monitor gzips for a tailnet `Host`);
- relays only to machines with an entry in the app, by that entry, never to an address a request
  names, with the path normalized first, so the page cannot reach anything that was not added. It
  follows no redirect: a redirect is handed back to the page as an answer. An entry usually holds a
  token; one for an unpaired monitor holds none (D8, D10);
- in a read-only release (D9), passes only what read-only allows: the contract's reads (mdrev's
  `resolve` among them, a POST that only reads, which the guest asks for every link and image in a
  document), a session's hiding (`/api/ignore`), mdrev's held text (`hold`) and the reader's own
  notes. Everything else is refused by the relay itself, among it `/api/send`, `/api/answer`,
  `/api/dialog` (which reads only to answer), `/api/consent`, Codex native mode's `/api/codex/*`
  (start, stop, send and the rest) and shared review's writes under `api/mdrev-review/`, so
  read-only does not rest on the page hiding its controls;
- answers only the contract's data routes, never the monitor's own page, scripts or stylesheets
  (`/`, its UI modules, its mdrev bundle), so the app's main frame never runs code a machine sent;
- never relays `/__reveal`, which opens the file manager on the machine that serves it, a side effect
  on that machine's desktop, except to this Mac's own monitor (D8).

So the monitor's gate passes the request as it passes a paired phone's, writes included. The token
never enters the web view: the page cannot read it and no script in it can leak it.

**Only the app's own page may use the relay.** The relay answers nothing outside `/k/<key>/` for
the current key, and takes the key off before it forwards a request. The key is part of each
handle's base (D5), so every request the page makes carries it: a fetch, an event stream, an
`<img>`, an `<a download>`, and mdrev's guest, which takes no header of its own. Nothing else knows
it:

- **How the page gets the key**: by a script the app injects into the main frame only, at document
  start, which writes it into the request layer's handles. No message handler the app adds ever
  returns the key, and every handler checks that its caller is the main frame
  (`frameInfo.isMainFrame`) on `app://monitor`, since WebKit offers the app's handlers to every
  frame, sandboxed ones included.

- **An agent's page cannot reach the relay.** The session view shows an HTML page an agent wrote in
  a frame, sandboxed without its origin, its scripts running, as today. Today that frame is a
  `srcdoc` of the page, and a `srcdoc` document takes its parent's content policy, so under the
  app's strict policy (below) its scripts would stop. So in the app the frame loads from
  `app-frame://`, a scheme the relay is not on: the app answers it from the HTML the page hands it
  (through a handler that takes HTML and returns only the frame's address), once, under today's
  frame policy (`connect-src 'none'`, `img-src data: blob:`, `form-action 'none'`) as its header.
  The frame never sees the key. The navigation delegate refuses any frame's navigation to `app://`,
  and any other navigation of a frame unless it is a link the reader tapped, which opens in the
  browser: a script in an agent's page must not open Safari or another app's URL with no tap. If the
  spike finds WebKit cannot tell a tap from a script's `click()`, the app refuses every navigation
  out of a frame, or asks first in a sheet naming the address. This
  needs the session view to let its host load that frame, an addition to the kit (section 5).
- **The main frame never loads a page under `/k/`**: the navigation delegate refuses it. A
  navigation the page asks to save (a file over 64 MB is saved through an `<a download>` link to
  `/file`) becomes a download, never a page load. A main-frame navigation out of the app's schemes
  opens in the browser only when the reader tapped a link, as for a frame: a machine's answer can
  ask the page to navigate (the record store follows a `/pull` reply of `{"t":"redirect"}` with
  `location.assign`), so the request layer refuses such an answer from a relayed machine.
- **The key stays out of what is kept**: the web view keeps no history across launches, and the
  page loads nothing from outside the app, which its policy enforces (below).

Whether WebKit behaves as described here (the frame on its own scheme with its policy as a header,
the delegate's refusals, a download leaving the web view, CORS on a custom scheme) is a phase 0a
spike.

**The app's own page is the boundary that is new.** Each monitor exposes nothing more than today,
but one page now holds the key and draws records from every paired machine. A rendering bug that
one machine's records could trigger, or a machine paired from a bad QR code, would reach every
machine through the relay, writes included, where today the same bug reaches one monitor. So the
page at `app://monitor/kit/` runs under a strict content security policy: `script-src` of
`app://monitor/kit/` only (not the whole origin, which relayed answers share) and none inline;
`default-src 'none'`; `style-src app://monitor/kit/ 'unsafe-inline'` (the components set inline
styles, and the diagram runtime writes a `<style>` into its drawings); `font-src app://monitor/kit/
data:`; `connect-src` and `img-src` the app's own origin plus `data:` and `blob:` (an embedded
image is downloaded by fetching its `data:` address); `frame-src app-frame:`; `form-action 'none'`;
`base-uri 'none'`. An agent's page is not under it, being on its own scheme. The scripts the page
loads from a monitor today come from the app's bundle instead, under `/kit/`: mdrev's guest (D8) and
the diagram runtime (`/mermaid/<version>.js`, found today through the page head's
`mermaid-runtime` meta), which section 5 places.

### D4 — Pairing and revoking

**Add a machine**: on that machine, `agent-monitor --pair-phone` shows its QR as today. The app scans
it, or takes the pairing link or code typed or pasted (a Mac has no camera pointed at its own
screen), reads the address and the code, redeems the code with `POST /api/pair` on the relay's
session with no cookie store (the answer's lasting `Set-Cookie` would otherwise be saved to disk),
takes the token from that header, and keeps it in the Keychain with the machine's name, address and
identity. Nothing is added to the monitor. The Keychain entry is kept on this device only, so each
of the reader's devices pairs each machine once. `--pair-phone` can hand out an `http://` address,
which App Transport Security refuses to a named host unless the app declares an exception; which
the app declares is a phase 0a spike. **Forget a machine**: delete it in the app.

**A lost device**: every device paired with a monitor holds that monitor's token. The quickest step
is to remove the device from the tailnet, which cuts it off from every machine reached over the
tailnet at once, though not from a monitor someone also serves through Funnel. A lost Mac also holds
the ssh keys D10's machines are reached by, and an unpaired monitor needs no token: revoke those
keys too. Replacing each token finishes the job, and works today, with no change, on that machine:
stop the monitor and whatever restarts it; delete `<state>/auth-token` and, if it is there,
`<cache root>/auth-token`; start it with `--pair`, which mints a new one; then every reader of that
machine pairs again. The order matters. `--pair` beside a running monitor writes the file and hands
off to it, and the running gate keeps the old token; a supervisor that restarts it without `--pair`
in between leaves it unpaired, which on macOS lets every tailnet device in (#s417). A
`--pair --rotate` flag would make it one step, and is optional (section 5).

**Identity**: the address a machine was paired at, its tailnet name (one per machine on a tailnet,
and stable). No monitor route says who a monitor is, so the same machine paired under two names
lists twice. A route that names the machine would be a monitor change (section 5).

### D5 — What the app builds on: the request layer and the backend contract

Both come from `design/embedding.md`'s order of the work, and are built there:

- **One request layer** (its step 1, #s416, the first thing phase 0b builds): every request the
  components make goes through one module that holds a *backend handle* per backend. A handle has a
  *base* (a prefix, an absolute origin, or the app's `app://monitor/k/<key>/m/<machine>/`); how
  credentials ride (the ambient cookie, the app's relay, or a bearer); whether the reader is *local*
  to that backend (reveal or download; windowed reading); where its mdrev bundle loads from; and
  what the page reads today from its own head for the one monitor it has (`data-paired`, which gates
  the HTML preview, `data-version` and `data-mdrev`). All of them are per handle, never page-wide. A
  handle's backend can also be in the page itself, as the merged source is (D6).
  `design/embedding.md` describes step 1 with one prefix, while #s416 carries the handle's base,
  credentials, locality and mdrev; the rest (what the page reads from its head, a backend in the
  page, and the key in the base) is asked of #s416 or a follow-up of it. Amending embedding.md to
  match is asked of its author, and is the owner's (section 5). Both call step 1 this design's
  "stage 0": it is the first step of phase 0b here.
- **The backend contract** ("1. The backend contract", its step 2): the routes the components read,
  written down and versioned. Each paired machine's monitor already answers it. The app needs nothing
  of its step 3 (a backend a host mounts): it reaches monitors that already serve the contract.

### D6 — The session list over every machine: the merged source

The session list component reads one backend. In the monitor's own page that backend is one
monitor; in the app it is the **merged source**, which answers the same session-list routes from N
handles:

- It polls each machine's session list through that machine's handle, as often as the list refreshes
  today, with a per-machine timeout, and keeps each machine's last good answer, marked stale. One
  slow machine never holds the list (R6).
- It names each row's machine in an optional `machine` field on the row, and namespaces by machine
  (`<machine>/<id>`) every field the merged list itself matches on: the session's id, its group,
  its `ignoreKey`, and the ids that nest one session under another (`startedBy`, `sideOf`,
  `continuedIn`, a fork's family). So the same `~/code/x` on two machines stays two groups, and
  nothing nests across machines. No monitor's answer gains a field: only the merged source writes
  `machine`.
- It leaves each machine's layout answers and the rows' pane fields (`target`, `herdrSession`) as
  that machine wrote them, and hands both answers on per machine: which apps that machine can follow
  (plain `/api/layout`), and each app's layout (`/api/layout?source=<app>`), keyed by machine and
  app. A layout is matched only against its own machine's
  rows, by the ids and pane keys that machine uses (the Herdr arrangement finds a row by
  `<herdrSession or default>|<target>`, a key two machines can share), so a layout never reaches a
  row of another machine.
- The list's own writes go to the row's machine with the prefix taken off: hiding a session and
  showing it again (`/api/ignore`).
- The session filter's buckets and counts sum across machines. Recency compares each machine's
  activity times with the reader's device's clock; tailnet machines keep time well enough for "the
  last six hours".
- A machine that does not answer keeps its last rows, greyed, under a head saying since when. One
  that never answered is a head saying why.
- Cost: one list answer per machine per poll (about 13 KB gzipped). The app polls only while it is
  open, as iOS suspends it otherwise.

It is page code in the kit, beside the session list, built in phase 1 once embedding's step 5 has
made the list a component: any host that lists sessions from more than one backend needs the same
thing, and the kit's own tests hold it. The component itself then gains the optional `machine`
field, read for the row's label and for D7, and layouts per machine, matched as above, beside
the one layout it reads today. These are additions to the kit's interface, the owner's to approve
(section 5).

### D7 — The by-device arrangement

A new Arrange choice in the session list, "By device", offered only when rows carry a `machine`: an
outer head per machine, its sessions under it, arranged as the reader chose for the rest of the
list. An app arrangement (like the Claude app, the Codex app, Herdr) is read per machine from that
machine's `/api/layout` (D6), since each machine has its own app, and applied to that machine's rows
alone, so with more than one machine it is offered only under By device: N apps' layouts have no one
merged order. Which app arrangements a machine offers comes from its plain `/api/layout` answer.
Every other choice (by agent and project, by activity) shows one merged list, each row wearing its
machine as a small label.

### D8 — A session opens on its own machine

Choosing a row is an event the session list sends its host ("Customizing", events out), naming the
machine and the session. The app mounts the session view with THAT machine's handle, so every call
the view makes (records, growth, the turn index, search, the panes' lists, files, mdrev, writes) goes
to the machine the session runs on, and nothing in the view has to know there are others. A file's
stamp, signed by that machine's key, goes back to the machine that minted it by construction.

**mdrev comes from the app.** The view's mdrev guest is the one the app pins and bundles, never a
machine's (D3: no code from a machine runs in the page), and it reads each machine's mdrev routes
through the handle. mdrev's guest already works with hosts that answer less (a route a host lacks
turns its feature off), so a machine whose monitor pins an older mdrev should lose only what it
lacks. That is checked, not assumed: the app's guest against the oldest monitor version the kit
reads, by mdrev's own conformance check. mdrev's guest makes its own requests with no header of the
host's, which is why the key rides in the base (D3) rather than in a header.

**Locality is per handle.** On a Mac, the app pairs "This Mac" on the loopback, with the code that
`agent-monitor --pair-phone --url http://127.0.0.1:<port>` shows: that machine alone is local, and
reveal is relayed to it alone. An unpaired This Mac lists its sessions and shows their transcripts
with no code at all, as the reader's own browser does, but `/file` needs a token, so its files,
images opened by path, mdrev on a file and the HTML preview (which `data-paired` gates) wait for
pairing, and so do writes. Every other machine is remote: its files
download, and long sessions read windowed. On a phone nothing is local. A Mac paired both ways
lists twice, as D4 says.

**Detached views** ("Detached views"): on a Mac, a session or an artifact opens in a window of its
own, the kit's detached page entry in a second web view on the same scheme and handle. Today's
detached tab gets held text through `sessionStorage`, which `window.open` copies into the tab it
makes; whether a second `WKWebView` gets it the same way is a phase 0a spike. On a phone the view's
own full-screen sheets stand in for a window, as they do in the monitor's page today.

### D9 — Writes across machines

The compose box, dialogs and pane grants work on any machine's session, under that monitor's own
gate: the token the app holds, and its consent, kept per pane and pid on that machine. The
components need nothing new for it: a handle whose backend serves the write routes gets the compose
box and the answer buttons, and one that serves none gets neither ("1. The backend contract"). Codex
native mode is the exception: its page code comes from the monitor (its entry imports
`/codex-workspace/api.js` and the monitor's stylesheet), which the relay does not pass (D3). So the
app leaves native mode out until its modules join the kit, an addition for section 5. **Choice for
the owner**: writes in the first release with the rest (parity, R3), or a read-only first release
with writes a stage later. Read-only here means no send, no answer to a dialog, no pane grant, none
of Codex native mode's writes (`/api/codex/*`) and none of shared review's writes (a Push, a Share,
pairing and subscriptions, which reach colleagues). Hiding a session, mdrev's held text (`hold`) and
the reader's own notes still reach the machine, as they do from a paired phone, since none of them
touches an agent or anyone else. The relay holds that line (D3), not only the page. Recommended:
read-only first, so the first release proves pairing, the relay and its key (D3) before anything in
the app can type into an agent. The relay enforces it (D3), not only the page. A lost device holding
writes is no worse than a lost paired phone today, and its tokens are replaced as D4 says.

### D10 — Machines reachable only by ssh

Today some machines are reached only by ssh, through the fleet's tunnels (`design/fleet-pairing.md`,
D1). An iOS app cannot run those tunnels. A Mac build sent through TestFlight runs in the App
Sandbox, which most likely cannot run `ssh` with the reader's keys and config; a phase 0a spike
settles it. So the plan: the fleet's binary stays on the Mac as the tunnel helper, outside the
sandbox, opening and re-opening the tunnels as today, and the Mac app reaches each tunnelled monitor
at the loopback port the helper opened:

- **Running the helper**: the reader installs and runs the fleet as today, on a fixed `--port` the
  app is told once; a sandboxed app cannot start it.
- **Which port is which machine**: the kernel picks the tunnels' ports, and the fleet already
  publishes each machine's name and current local address on every poll of its `/api/fleet`. The app
  reads that, names a tunnelled machine by the fleet's name for it, and follows it to its new port
  when a tunnel is re-opened. The fleet listens only on `127.0.0.1`, so it always runs on the app's
  own Mac, and every address it names is a loopback one; the app skips the entry whose address is
  This Mac's own monitor on the loopback (paired or not), already listed, and lists every other
  entry, a second local monitor included.
- **The `Host` a tunnelled monitor wants**: its gate compares `Host` with its own port, which is not
  the tunnel's local one, so files and writes would be refused. The fleet's own probe sends the
  monitor's port in `Host` for this reason (#s150), but the fleet keeps that port to itself, and
  `URLSession` reserves `Host`. So the relay speaks HTTP/1.1 itself over the Network framework for a
  tunnelled machine, and either sends the monitor's port, which the fleet would have to publish (a
  fleet change), or sends no `Host`, which the gate lets pass today. Section 5 weighs the two; a
  phase 0a spike measures both.
- **Admission**, as the fleet's tabs are admitted today: an unpaired Linux monitor admits the tunnel
  as its own user, with no token: its list and its transcripts, but not `/file` (files, images by
  path, mdrev on a file, the HTML preview) and no writes, as D8 says of an unpaired This Mac. A
  paired one takes a code that `agent-monitor --pair-phone --url http://127.0.0.1:<its port>` shows
  on it, typed into the app, and then takes writes too; `/api/pair` answers only on a paired gate.
  The app takes the code from that link and the address from `/api/fleet`, since the link names the
  monitor's own port.
- **A port that moved**: the relay sends a token to a tunnel's port only while the fleet's latest
  answer names that machine there. On a Mac shared with other users, another user could take a port
  in the moment a tunnel drops, or the fleet's own fixed port while the fleet is down, and answer as
  the fleet (its `/api/fleet` admits any local user on macOS) to steer tokens to ports of their own;
  this narrows the first, and neither is closed. The app is for a Mac its reader does not share.

So on a Mac every machine the fleet reaches today is in the list, and on a phone only the machines
on the tailnet. The alternatives: a Mac build signed outside TestFlight that runs `ssh` itself, which
changes #s413 and is the owner's; or one machine relaying for the others, the remote point the owner
ruled out.

### D11 — Notifications: the owner's choice

With no server of its own, a push needs something to send it through Apple's push service. Either
**each monitor sends** "a session on <machine> needs you" (an APNs key on every machine, and a
payload with no session content, since it leaves the tailnet through Apple's servers), or **there
are none**, and the app shows attention only while it is open. Background refresh on iOS is
opportunistic and is not a substitute. A machine reached only by ssh makes no new outbound
connections (`design/fleet-pairing.md`, D1), so it could not send one. Pushes would be a monitor
change, a new network path and a signing key spread over every machine, so they are in section 5.
Recommended for the first release: none.

### D12 — The fleet's tabs retire

Once the Mac app covers what the tabs did, the fleet's page could be retired, with a migration
note, in the way the classic view was (#s98). Its binary stays as the Mac app's tunnel helper (D10),
unless the spike finds the app can run the tunnels itself; then the binary and its formula could
retire too. Removing a feature is the owner's (section 5).

## 4. Security

- **No monitor exposes more than today.** A tailnet monitor sees the app as one more paired phone,
  over the same path; This Mac's sees it as the reader's own browser; a tunnelled one, as the fleet's
  tabs.
- **Tokens stay in the Keychain**, on the device only, and never enter the web view, a log or a URL.
  The relay strips the `Set-Cookie` that would carry one to the page (D3).
- **The relay reaches only paired machines**, by their entry in the app, with the path normalized:
  the page cannot make it reach a host or a route that was not paired.
- **No code from a machine runs in the app's main frame.** The page, the kit and mdrev come from the
  app's bundle; the relay answers data routes only, and the main frame never navigates under `/k/`
  (D3). An agent's HTML page runs only in its sandboxed frame on its own scheme.
- **Only the app's own page uses the relay**: the key is in its requests' path and nowhere else, and
  reaches the page by a main-frame script, never a handler (D3). A navigation out of the app's
  schemes, from the main frame or a frame, opens in the browser only when the reader tapped a link,
  and is refused otherwise. A download from the main frame passes without a tap when it is of a
  `blob:` address (the page saves a smaller file through a script-clicked `<a download>` of one) or
  of the relay's own `/file` (a file over 64 MB). Any other download, and every download from a
  frame, is a navigation under the tap rule.
- **No token on disk but the Keychain's**: pairing and the relay use a session with no cookie store
  (D3, D4).
- **Reveal is relayed only to this Mac's own monitor** (D8).
- **Masking is each monitor's**, and off unless its mask policy turns it on (#365). The app reaches
  a tailnet monitor by its tailnet name, so a policy that masks for clients across a network masks
  for the app. This Mac's monitor and a tunnelled one are reached on the loopback, so they mask only
  under the `always` policy, as for the fleet's tabs today. The app adds no masking of its own.
- **What crosses the network**: the reader's device to each machine, over the tailnet (WireGuard),
  and, if the owner chooses pushes, a content-free notice through Apple's push service. Nothing else
  leaves the tailnet.

## 5. Contracts this creates or changes, and who relies on them

Decided: the app itself, in the private repository `tanghong123/agent-monitor-app` (#s414). The
agent sets up the repository and the build; distribution through TestFlight is the owner's own, on
his Apple account (#s413). Each item below is the owner's to approve when it comes up, with a
recommendation:

- **The kit's components and the backend contract** are `design/embedding.md`'s, decided there. The
  app relies on the contract across monitor versions (D2), which is what versioning it is for.
- **A backend handle in the request layer, rather than one prefix** (D5, #s416): embedding's step 1
  names one contract prefix for the page, which is enough for a host with one backend and costs no
  change to its plan. The app needs more per backend: its credentials, its locality, its mdrev and
  what the page reads from its head, and a backend that can live in the page (D6). With one prefix
  the app would have to run a separate web view per machine and could not merge the list. #s416 is
  building the handle; the owner confirms it, with embedding.md amended to match. Recommended: the
  handle.
- **How the app learns a monitor's contract version** (D2), for embedding's step 2: (a) the kit maps
  a monitor's release version, which its page already carries (`data-version` on `/`, read natively
  by the app), to the contract versions it serves, with no monitor change; or (b) a version field in
  an answer the app already reads, a monitor change. Recommended: (b) if step 2 adds one for every
  host anyway, else (a).
- **The session list beyond one backend** (D6, D7): the merged source in the kit, which rewrites ids
  and group keys as `<machine>/<id>`; an optional `machine` field on a session-list row that only
  the merged source writes; and the list reading, per machine, which apps it can follow and each
  app's layout (`/api/layout` and `/api/layout?source=<app>`, keyed by machine and app), each layout
  matched against its own machine's rows, where it reads one machine's today. All of them only add.
  A larger alternative is a session list that takes N backends itself; one with no addition at all
  folds the machine into the project's label, which says where a row runs but makes "By device"
  impossible (R2). Recommended: the merged source, the field and the layouts per machine, triaged
  with embedding's step 5.
- **What the session view tells its host, and what embedding's order delivers first** (D2, phase
  0b): the session's title, its current turn and an "open Info" for the phone's top bar, which
  "Customizing" does not list; and the kit (step 8) ahead of the detached page (step 7), so the first
  kit ships without that page and a later one adds it. Both change embedding's plan, asked of its
  author with a recommendation, and the owner's. The alternative with no change: the app's top bar
  shows the machine and the session's name from the list's row only, and the Mac's windows wait for
  the kit that carries step 7. Recommended: the events, and the kit first.
- **What the app comes to rely on that a monitor does today**: the pairing QR's address form,
  `/api/pair` answering with the token in `Set-Cookie`, the gate reading its own cookie by the name
  it derives from its token, a request with no `Origin` passing the write check, the trusted-host
  rule that lets `/file` and writes through for the `tailscale serve` name (#331), `/api/layout`,
  and, if (a) above is chosen, the `data-version` in the page at `/`. Recommended: written into the
  contract in embedding's step 2, so a change to any of them is seen as a contract change.
- **The app's pinned mdrev guest against each machine's mdrev** (D8): the guest of one mdrev release,
  with a custom-scheme base and the key in its path, reads routes served by another release's
  `mdrev-cli`. If the conformance check or the spike finds it cannot, the fix is a change to mdrev,
  for its owner. The alternative with no change, loading each machine's own guest, is what D3 rules
  out (no code from a machine in the page). Recommended: check it in phase 0a, and ask mdrev's
  owner only on a failure.
- **A route that names the machine** (D4): recommended not now. The tailnet name is the identity,
  and the cost is a machine paired under two names listing twice.
- **`--pair --rotate` on the monitor** (D4): optional. Recommended not now: deleting the token file
  and restarting already replaces a token.
- **Notifications** (D11): pushes sent by each monitor would be a monitor change, a path through
  Apple's push service, a signing key on every machine, and none from a machine reached only by
  ssh. The alternative with no change is none, the
  app showing attention only while it is open. Recommended: none in the first release.
- **Codex native mode in the app** (D9): its page modules joining the kit, an addition; the
  alternative with no change is the app without native mode, which the monitor's own page keeps.
  Recommended: without it at first, since nothing in the app's first phases needs it.
- **Where the page's runtimes ship** (D3, D8): mdrev's guest and the diagram runtime, which the
  monitor serves today. The alternative with no change: the app vendors both itself, pinned like the
  kit. The other: the kit carries them, an addition to embedding's "Delivery", which every host that
  cannot load a backend's scripts would want. Recommended: the app vendors them at first, and the
  kit takes them over if a second host needs the same.
- **How an agent's page is framed** (D3): the session view letting its host load that frame from a
  scheme of its own, an addition to the kit. Without it the app either loosens its page's policy to
  allow inline scripts, which gives up the policy, or shows agents' pages with their scripts off,
  which gives up part of R3. Recommended: the addition, for embedding's step 4.
- **The fleet as the Mac app's tunnel helper** (D10), for phase 4: the app reads `/api/fleet` as it
  is, which then becomes something it relies on. For the `Host` a tunnelled monitor wants: the
  fleet adds each machine's monitor port to `/api/fleet` (a field that only adds); or the app takes
  the port from a paired monitor's pairing link (no change, but only for a paired monitor); or the
  relay sends no `Host` and relies on the gate letting that pass. Recommended: the fleet's field,
  since it covers every tunnelled monitor and leans on no leniency.
- **Retiring the fleet's page, and perhaps its binary and formula** (D12): removing a feature.
  Options: retire the page once the Mac app lists every machine the tabs did (recommended), keep it
  beside the app, or retire the binary too if the app can run the tunnels itself.

## 6. Phases

Two tracks. The kit is built in claude-replay under #s406, in `design/embedding.md`'s order; the app
is built in its own repository, and starts with what it can learn before the kit exists.

| Phase | Where | Ships | Accept |
|---|---|---|---|
| 0a | the app's repo | Spikes: a `WKURLSchemeHandler` relaying POST bodies and streaming a large `/records` range, with the gate's cookie set by hand and no cookie store (D3); a gzipped answer through the relay (D3); the key in the path reaching every kind of request the page makes, an agent's page framed from its own scheme with its policy as a header, refusing a frame's navigation, CORS on a custom scheme, and the page's strict policy (D3); a download from the page and the share sheet (D3, R3); the web view's storage for a custom scheme (the components keep their preferences in `localStorage`); whether a custom scheme is a secure context for the APIs the components use; a second web view getting `sessionStorage` from `window.open` (D8); App Transport Security and an `http://` pairing address (D4); and on a Mac, whether a sandboxed app can run `ssh`, reach a monitor on the loopback, and speak HTTP/1.1 to a tunnelled monitor with its port or with no `Host` (D10). Most can be measured with today's monitor page loaded whole through the scheme from one machine, an exception to D3 for the spike alone. | Each answered by measurement on the iOS Simulator, a device and a Mac, written down |
| 0b | claude-replay | Embedding's steps 1 (the request layer, #s416), 2 (the contract), 4 and 5 (the two components, with the app shell mounting them), 6 (layout by pane size, which the iPad and the Mac need side by side) and 8 (the kit). The app needs the kit before step 7, so 8 comes ahead of 7, a change asked of embedding's order. | Embedding's own: the app shell's browser suite green at every step, a Simulator walk |
| 1 | both | claude-replay: the merged source and the row's `machine` field (after the owner's yes, section 5). The app: the iOS app, with pairing by QR, the Keychain, the relay and its key, the session list over the merged source, and the session view on each row's machine, files and mdrev included, read-only | Paired with two machines, the app lists both; an offline machine costs only its own rows; a session on each, its files and its Markdown read as on its own machine; an agent's page runs its scripts, and its navigation to `app://`, an untapped navigation out and a call to each of the app's handlers are each refused; a page-issued `/api/send` or `/api/codex/send` is refused by the relay |
| 2 | claude-replay | "By device" in the session list | The list arranged by machine, each arranged as the reader chose, on a phone and side by side on an iPad |
| 3 | both | Writes across machines (or in 1, by the owner's choice) | The compose box, a dialog and a pane grant on another machine's session, each refused where today's rules refuse it; a frame's write refused by the relay |
| 4 | both | The Mac app: the two components side by side, This Mac on the loopback, detached windows (embedding's step 7); the fleet's answer and its tunnels as the helper; the fleet's page retired | A machine reached only by ssh appears in the Mac app's list; a session opens in a window of its own; reveal works on This Mac only |
| 5 | both | Notifications, if the owner chooses them (D11) | — |

## 7. What this is not

- **Not a change to the monitor.** No route, field or gate, beyond the optional items in section 5,
  which are the owner's.
- **Not a server.** Nothing runs anywhere but the monitors, the app on the reader's device, and on a
  Mac the fleet's tunnel helper.
- **Not a second renderer.** The app hosts the same two components the monitor's page does (D2).
- **Not a sync service.** The app stores no session content beyond each machine's last list answer
  and what the components already cache.

## 8. Decided, and still open

**Decided by the owner:**

- A native app on the reader's device, not a hub (#s411, D1).
- The app is one more host of the session list and session view components, with its own native
  chrome (#s408, #s415, D2).
- Its code in a new private repository, `tanghong123/agent-monitor-app`, installed through
  TestFlight on his account (#s413, #s414).

**Still open,** each asked when the phase it holds up comes near, not before:

1. **The backend handle rather than one prefix** (section 5, #s416, now): recommended the handle.
2. **The session list beyond one backend** (section 5, phase 1): the merged source in the kit and an
   optional `machine` field on a row (recommended), or a session list that takes N backends itself.
3. **The session view's events for the top bar, its host loading an agent's page frame, and the kit
   before the detached page** (section 5, phase 0b): recommended yes to all three, through
   embedding's author.
4. **How the app learns a monitor's contract version** (section 5, embedding's step 2).
5. **Writes** (D9, phase 3): in the first release with the rest, or read-only first (recommended).
6. **Machines reachable only by ssh** (D10, phase 4): the fleet's binary as the Mac app's tunnel
   helper (recommended, if the spike confirms the sandbox stops `ssh`), or a Mac build signed outside
   TestFlight; and with the helper, the monitor's port in `/api/fleet` (recommended) or no `Host`.
   Then whether the fleet's page retires (D12).
7. **Notifications** (D11, phase 5): none in the first release (recommended), or pushes sent by each
   monitor.
