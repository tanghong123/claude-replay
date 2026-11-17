# The web app as one mount, for the native app

> **DRAFT 1** (#s551, 2026-11-17). Not built. Hong's choice of 2026-11-17, closing
> agent-monitor-app #s9: *"I want the same experience as the current web app on desktop and
> mobile, but need the native integration on ios devices"*, then "Yes" to doing it this way, then
> *"I'd like to have a single session list that combines sessions from multiple devices"*. It revises
> #s415 (`design/monitor-hub.md`, D2) and adds a step 9 to `design/embedding.md`. It builds on #s540
> (the list reads a server of its own) and #s541 (the merged source over several machines).

## What it is for

The native app (`tanghong123/agent-monitor-app`) should be the web app: the same session list with
its drawer and rail, the same session view, the same search, filter and theme, laid out as the
monitor's own page lays them out, the phone's on an iPhone and the desktop's on an iPad. What it adds
is what only an app on the device can do: pairing with each machine, the relay, the share sheet, the
sheet before a link leaves the app, and one list over every machine.

Today the app gets less. It mounts the kit's two parts, the session list and the session view, in
chrome of its own (#s415): one machine at a time, read-only, a layout of its own (list and session
side by side from 993px, else the list, then the session with a back control). The web app's chrome
around the two parts lives in `app.js`, which the kit does not ship. Rebuilding that chrome in the
app would make two copies of one interface, drifting apart. And the app reaches past the kit's guide
in four places to get what it has (agent-monitor-app #s9): the view's `frame` and `paneLayout`
options, the handle's `url`, `mdrevUrl` and `local` members, and two of the view's CSS rules.

So the kit ships the web app itself as one mount. The app mounts it, and its native integration goes
through options the kit names for it, each held by the kit's tests.

## What the web app is today

- **Script:** `app.js` (683 lines), a module that runs as the monitor's page loads. It mounts the
  list (`mountSessionList`) and the view (`mountSessionView`) as a host would, and keeps what is the
  app's: the brand and version, the theme toggle, write mode with its switch and confirm dialog, the
  session search (⌘K, a jump-to), the toast, the list's layout from the window (the drawer, the rail
  or the list at a width, `arrangeApp`), the phone's drawer handle and scrim, a wide phone's column
  switch, the address (`?session=`) and its history, the keymap, and native mode's installer.
- **Markup:** the page's shell (`app-shell.html`, from the generated reference shell): `#app` with
  the sidebar's head, the list's element, the view's element, ⌘K's layer, the confirm dialog, the
  toast.
- **Page-wide:** `app-shell.css` puts its tokens on `:root` (16 rules, 11 more for dark) and has
  rules for `html` and `body`; `app.js` listens on `window` (`popstate`, `keydown`) and reads
  `location` and `localStorage`.
- **One server:** the view takes `backend()`, the page's one handle, and write mode reads whether
  the page is paired.

## Decisions

### One entry point that takes the whole page

`mountApp(element, options)` draws the web app into `element` and returns a handle (`unmount`, and
the members below). It draws the app's markup itself, from the same shell the monitor's page
carries, which a test holds byte for byte. It mounts the list and the view inside, as `app.js`
does today. And it owns the page: the document's address, the root's theme, the page-wide styles and
the page's keymap.

So there is one per document, for a host whose whole page is the web app. The native app is that
host. A host that wants the session in a pane of its own page (the collaboration app) keeps mounting
the two parts, as `design/embedding.md` has it.

Two other shapes were weighed:
- **The app's chrome as a component a host places in a pane.** It would mean re-scoping every
  page-wide rule and every window-sized layout rule to an element, and no host has asked for it.
- **The kit ships the page itself**, an HTML entry that the host configures through globals. That
  is an untyped contract: nothing for the kit's declaration test to hold, and nothing to refuse a
  wrong name.

### The monitor's page is its first host

`app.js` becomes the monitor's page calling `mountApp` with its own server, then installing native
mode as now. So the web app and the native app run the same code, and every browser case of the app
shell tests the mount. This is embedding's "The standalone app becomes a host", one level up.

### One list over every machine

The mount takes either `backend`, one handle (the monitor's page), or `machines`, a list of
`{machine, label, handle}` (the native app). With `machines`:

- the list reads #s541's merged source over them, so each row and project head names its machine,
  a machine that stops answering keeps its rows greyed under a head saying since when, and hiding a
  row hides it on its own machine only;
- opening a row mounts the view again on that row's machine, with that machine's handle (#s541's
  `locate` and `forView`, guide section 6), so the view's every call goes to the machine the
  session runs on (D8);
- ⌘K's rows name their machine;
- write mode asks the row's machine whether it is paired, never the page;
- the address names the merged id (`<machine>/<id>`);
- a Machines control calls `onMachines()`, the app's own screen for pairing, forgetting and each
  machine's state. It is drawn only where `onMachines` is given. In the sidebar's head it takes the
  theme toggle's slot, which the app leaves free (`show.theme`, below), so the head keeps the count
  of controls #223 and #210 measured at its 232px minimum. On the rail it is a glyph after search;
  in the drawer and a wide phone's column it is where the head's controls are there. Each place is
  hit-tested at the list's minimum width, as `the_app_shell_resizes_the_session_list` does for the
  head today.

The "By device" arrangement (D7) comes with phase 2, in the list.

### What only the native app is given

Each option below is declared in the kit, held by its declaration test and a contract case, and
offered to the native app only, as `frame` is today (#s436: the owner controls both sides). Which of
them the plan Hong agreed to already named, and which need his ruling, is in "What needs a ruling".
Each is an option of the mount, which hands it to the list or the view inside; none is a handle's,
so no other host is offered it.

- **`frame`**: an agent's HTML page in a frame on the app's own scheme, passed to the view as now.
  With it, `onFrameGone(id)`: the view says when no frame shows a page any longer, so the app can
  drop that page (agent-monitor-app #s9's note).
- **`assets`**: where mdrev's bundle and the Mermaid runtime load from, apart from each machine's
  routes. The app bundles the releases the kit pins (D3: no code from a machine runs in the page).
  Today the view takes both from its handle: mdrev's bundle and its routes through one base
  (`mdrevUrl`, in `mdrev-pane.js`), and the Mermaid runtime through `url` from the page's
  `mermaid-runtime` meta (`session-view.js`). So the app wraps each handle to send those few paths
  to its bundle. With `assets` the mount passes the view where the code loads from, and every
  route, mdrev's included, still goes through the row's handle. The wrapped `url` and `mdrevUrl`
  members go.
- **The theme**: the app gives it, `theme: "system"` to follow the device's appearance, or
  `"light"` or `"dark"` from a setting of its own and `setTheme` as that changes. It switches the
  mount's own toggle off (`show.theme`, below).
- **Scripts**: the mount imports only the modules beside it in the kit and loads nothing from a
  machine. A contract case mounts it under a policy that allows scripts from the kit's directory
  alone.
- **Saving a file and links out**: no option. The page does what it does in a browser, a download
  link or a link with an address, and the app's navigation rules take each: the save through the
  relay into the share sheet (agent-monitor-app #s3), and the sheet that asks before a link leaves
  the app (#s12). The app's walk holds both. An option is added only if a measurement shows the
  page's own path does not reach those rules.
- **Machines and pairing**: `machines` and `onMachines`, above.
- **Locality**: `backendHandle` takes `local` already but the kit does not declare it, so the app
  passes it from outside the guide (`local: false`: nothing on a phone is local, D8). Declaring it
  would offer it to every host, which is why it is listed for a ruling.
- **Native mode**: the mount's handle carries no installer for Codex native mode in the kit. The
  monitor's page installs it as now, and the app goes without it (D9).

So each of the app's four reaches past the guide goes:
- `frame` becomes the mount's declared option.
- `paneLayout` is no longer needed: the mount lays out its parts itself, as `app.js` does.
- The wrapped `url` and `mdrevUrl` members give way to `assets`, and `local` is declared.
- Of the two CSS rules, the back control's room goes with the app's own layout, and Review hidden
  becomes `show.review`.

### Controls a host may switch off

The mount takes `show`, as the view does: the view's optional controls (`reading`, `history`,
`detach`), and two more:
- `review`: shared review's full-screen sheet on a phone, which writes to colleagues' threads. The
  writes Hong chose for the app (agent-monitor-app #s14) are the compose box, dialog answers and pane
  grants; shared review reaches other people, so the app passes `review: false` until he asks for it.
- `theme`: the theme toggle, off where the app gives the theme.

It passes `detach: false` too, until detached windows come (phase 4). Switching an optional control
off is a choice #s436 already gives every host.

### Writes

The mount takes `write` as the view does. Hong chose on agent-monitor-app #s14 (2026-11-17) that the
app writes from the moment it mounts the web app: the compose box, dialog answers and pane grants, as
on the web app, each under that machine's own gate and grant. Write mode's switch asks the row's
machine whether it is paired (above). The relay passes the page's write routes and still refuses
every frame's (D3). A host that passes no `write` gets no write switch and no compose box.

### What stays the native app's

Pairing and the Keychain, on its own screens; the relay and its key; the share sheet; the sheet
before a link leaves the app; running only bundled code; and building each kit release into the app
and onto TestFlight (agent-monitor-app #s13). The layout, the top bar and where the list goes are
the mount's, as in a browser.

## What needs a ruling

hd-replay pointed out that the kit offers a host only what Hong has ruled it may change
(`VIEW_WITHHELD` and `LIST_WITHHELD` in `claude-monitor/src/kit.rs`). The plan Hong agreed to on
2026-11-17 named the web app as one mount, from the same code the monitor serves, and these
native-app options: the app's frame for agent pages, saving a file to the share sheet, links out
through a sheet, the scripts loading from the bundle, the machines (pairing, the one list, By
device), and the theme. Anything more comes to him as one decision before it is built (his "sounds
good" to that, logged on #s551).

Writes are decided: they come with the mount (agent-monitor-app #s14, D9).

**Named in the plan he agreed to:**

| Item | Where in the plan |
|---|---|
| `mountApp`, with the monitor's page its first host | the web app as one mount, from the same code |
| `machines` and the merged list | the machines: the one list (and #s539 A) |
| `frame` | the app's frame for agent pages (and #s436) |
| `assets` | the scripts loading from the bundle |
| The theme from the app (`theme: "system"`, `setTheme`) | the theme |
| Saves and links out, with no option | the share sheet; links out through a sheet |

**For his ruling, each with a recommendation:**

| Item | What it adds | Recommended |
|---|---|---|
| `onMachines` and the Machines control | a control in the list's chrome that only the app draws | yes: pairing is in the plan, and the app has no other way into its own screens over a full-page mount |
| `onFrameGone` | an event out, for the frame only | yes: without it the app keeps every agent page a session ever framed |
| `show.review` | Review made an optional control | yes: shared review writes to colleagues' threads, beyond the writes Hong chose for the app |
| `show.theme` | the theme toggle made an optional control | yes, with the theme from the app; else two controls set one theme |
| `local` declared on `backendHandle` | a handle option every host is offered | yes: it only says what the handle already reads, and a host that frames the parts on another origin needs it too |

These go to him as one decision once this design has been reviewed, filed in this queue.

## Details for the stages

- **The address on the app's scheme.** The mount writes `?session=` and pushes history, as the web
  app does. Whether a `WKWebView` page on a custom scheme keeps both is measured now, in the app's current
  page, before stage 2 starts: if it does not, the mount takes the address as an option there.
  The app today also remembers the last session in storage and opens it at launch, since its page
  always loads with no address. Where the address names no session, the mount opens the first, as
  the web app does; whether it should open the remembered one instead is settled with that
  measurement.
- **Storage keys.** The mount keeps the web app's keys. On the app's scheme they are the app's
  alone. A key that holds a session id holds the merged id there.
- **mdrev's pin.** The app bundles the mdrev release the kit names (`pins.js` today), so the pin
  moves with the kit's.

## The order

1. **#s540 and #s541 land** (in flight).
2. **The mount.** `app.js` becomes `app-mount.js` with `mountApp`, its teardown and element-rooted
   lookups; the monitor's page is its host. The whole browser suite and a Simulator walk show
   nothing changed. Released.
3. **Machines in the mount**, on #s541: `machines`, `onMachines`, ⌘K, write mode per machine, the
   view remounted per row. A browser case with two hermetic monitors. Released.
4. **The native options**: `frame` with `onFrameGone`, `assets`, the theme from the app,
   `show.review` and `show.theme`, `local` declared, the Machines control, each once Hong has
   ruled on it.
   The kit's declarations, a guide section, and kit.rs's list of what only the native app is given.
   A kit page case mounts it under the app's policy. Released.
5. **agent-monitor-app #s13** mounts it and drops its own chrome.

Each stage lands on its own, with the monitor's page working at every step.

## Testing

- **The monitor's page:** its whole browser suite, unchanged, at every stage. That is the proof the
  mount is the web app.
- **The kit page:** `mountApp` from the kit alone, over two hermetic monitors: one merged list,
  each machine's rows opening on their own machine, ⌘K naming machines, a stopped machine's rows
  greyed, scripts allowed from the kit only.
- **The Machines control:** hit-tested in the head at the list's 232px minimum, on the rail, in the
  drawer and in a wide phone's column, as the head's other controls are.
- **The width study** (#s517) run on the kit page as on the monitor's.
- **The app's walk** on the Simulator: an iPhone upright and sideways, and an iPad, light and dark.
