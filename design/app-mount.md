# The web app as one mount, for the native app

> **DRAFT 5** (#s551, 2026-11-17). Stage 2 (the mount for one server) is built, and stage 3 (machines in
> the mount, #s595); the rest is not.
> Reviewed on #s551. It carries Hong's rulings of the
> same day: #s553 (a Machines row), #s554 (shared review in the app) and #s555 (Review on an iPad).
>
> Hong's choice of 2026-11-17, closing agent-monitor-app #s9: *"I want the same experience as the
> current web app on desktop and mobile, but need the native integration on ios devices"*, then
> "Yes" to doing it this way, then *"I'd like to have a single session list that combines sessions
> from multiple devices"*. It revises #s415 (`design/monitor-hub.md`, D2) and adds a step 9 to
> `design/embedding.md`. It builds on #s540 (the list reads a server of its own) and #s541 (the
> merged source over several machines).

## What it is for

The native app (`tanghong123/agent-monitor-app`) should be the web app: the same session list with
its drawer and rail, the same session view, the same search, filter, theme and writes, laid out as
the monitor's own page lays them out, the phone's on an iPhone and the desktop's on an iPad. What it
adds is what only an app on the device can do: pairing with each machine, the relay, the share sheet,
the sheet before a link leaves the app, and one list over every machine.

Today the app gets less. It mounts the kit's two parts, the session list and the session view, in
chrome of its own (#s415): one machine at a time, read-only, a layout of its own (list and session
side by side from 993px, else the list, then the session with a back control). The web app's chrome
around the two parts lives in `app.js`, which the kit does not ship. Rebuilding that chrome in the
app would make two copies of one interface, drifting apart. And the app reaches past the kit's guide
in four places to get what it has (agent-monitor-app #s9): the view's `frame` and `paneLayout`
options, the handle's `url`, `mdrevUrl` and `local` members, and two of the view's CSS rules.

So the kit ships the web app itself as one mount, and the app mounts it. Its native integration goes
through options the kit's contract cases hold, kept for the native app as `frame` is kept today.

## What the web app is today

- **Script:** `app.js` (683 lines), a module that runs as the monitor's page loads. It mounts the
  list (`mountSessionList`) and the view (`mountSessionView`) as a host would, and keeps what is the
  app's: the brand and version, the theme toggle, write mode with its switch, help and confirm
  dialog, the session search (⌘K, a jump-to), the toast, the list's layout from the window (the
  drawer, the rail or the list at a width, `arrangeApp`), the phone's drawer handle and scrim, a
  wide phone's column switch, the address (`?session=`) and its history, the keymap, and the hook
  native mode's entry wraps (`native-entry.js` wraps the write switch as it loads).
- **Markup:** the page's shell, which `ui::page` puts into the page (`APP_SHELL`): `#app` with the
  sidebar, the list's element and the view's element, and beside `#app`, at the body's level, three
  fixed layers: ⌘K's (`#searchLayer`), the confirm dialog (`#confirmLayer`) and the toast.
- **The page's head and body:** the viewport meta (with `viewport-fit=cover`, which the safe-area
  insets need), `color-scheme`, the three stylesheets in order, the `mermaid-runtime` meta, and
  `data-version`, `data-paired` and `data-mdrev` on the body.
- **Page-wide styles:** `app-shell.css` owns the document: `*`, `html, body {height: 100%}`,
  `body {overflow: hidden}`, the button resets, `.app {width: 100vw; height: 100dvh}`, its tokens on
  `:root` (55 rules name it) with the theme read from `:root[data-theme]`, and media queries on the
  window.
- **One server:** the view takes `backend()`, the page's one handle, and write mode reads whether
  that handle is paired.

## Decisions

### One entry point, once per page

`mountApp(element, options)` draws the web app into `element` and returns nothing: no member
changes a mounted app. It is called once per document and never unmounted: a second call throws.
That is not a choice of style: the web app is built on things a page has one of, and the design
names them so the refusal is deliberate:

- the stores in `state.js` (`indexState`, `searchState`, `controlState`), module singletons;
- the page-wide backend handle (`backend()`), which each view mount sets;
- mdrev's bundle, imported once per page (`mdrev-pane.js`);
- the elements found by fixed ids (`#app`, `#sessionView` through `viewRoot`, `#themeBtn`,
  `#drawerHandle`, `#columnSwitch`, the resizer, the three layers);
- the root's custom properties (`--sidebar-user`, and the view's) and the root's theme;
- the document's keymap (`bindKeymap(document, …)`, which cannot be unbound), the window's
  listeners, the address and history, and the storage keys;
- the phone queries, read against the window.

So nothing needs a teardown, and `app.js`'s listeners need no `signal`. A host that wants the web
app gone drops the page; the native app reloads its web view (below).

**What `element` is.** A wrapper holding `#app` and the three layers as siblings, as the body holds
them today. The layers cannot go inside `#app`: the view sets a `transform` on `#app` while a
phone's keyboard is up, which makes `#app` the containing block of anything fixed inside it, and the
confirm dialog, which a pane grant raises while the reader composes, would be misplaced exactly
then. The shell becomes a generated module (as `session-view-markup.js` is), from the same shell
the monitor's page carries, held byte for byte by a test; the monitor's page carries an empty
wrapper.

**What the host's page carries.** The mount takes the whole viewport (`app-shell.css` owns `html`
and `body`), so the page is the mount's: the viewport meta with `viewport-fit=cover`, a
`color-scheme` meta, the three stylesheets in order, and the `mermaid-runtime` meta on the
monitor's page, where the view reads the runtime's path (under `assets` the mount names it
instead). The body's data move into options: `version` (the brand), and per machine, `paired` and
mdrev's release, which each handle already carries. In the app the brand shows the kit's release,
since that is the code the app runs. A machine's own version is not in the list: neither the merged
source nor `/api/sessions` carries one, and #s541 draws a machine's head only while it is not live.
The app's Machines screen shows each machine's version, read as D2's "Versions" has it.

### The monitor's page is its first host

The mount's code moves to `app-mount.js`. `app.js` stays the monitor page's host script: it calls
`mountApp` as it loads, with the page's own server, so `native-entry.js`, which imports `app.js` and
wraps the write switch as it is evaluated, finds the switch bound, as now. The web app and the native
app then run the same code, and every browser case of the app shell tests the mount. This is
embedding's "The standalone app becomes a host", one level up.

### One list over every machine

The mount takes either `backend`, one handle (the monitor's page), or `machines`, a list of
`{machine, label, handle}` (the native app), never empty. With `machines`:

- **The list** reads #s541's merged source over them, so each row and project head names its
  machine, a machine that stops answering keeps its rows greyed under a head saying since when, and
  hiding a row hides it on its own machine only.
- **The view** is mounted on the first machine's handle until a row opens, showing nothing until
  then. Opening a row whose machine differs from the view's mounts the view again on that
  machine's handle (#s541's `locate` and `forView`, the guide's section 6), so every call the view
  makes goes to the machine the session runs on (D8). A row on the same machine is selected as
  today, with no remount.
- **What a remount loses**, as the view's unmount does: the compose draft and its attached images,
  the pane's tabs, the search, and a send still in flight (whose result is fenced). The pane's open
  state comes back by itself. This is accepted for a first stage; keeping the draft per session in
  the mount comes after, if the app's walk shows it matters.
- **The ids that cross** between the list and the view are mapped each way: the view gets the
  machine's own id (`forView`), and every id it gives back is mapped to the list's merged one
  (`<machine>/<id>`, `forView`'s `listId`, or `splitMachineId` the other way). That covers
  `selectSession` (with `push`), `parentShown`, `session()` against the list's selection, and
  `selectedId`. The address holds the merged id; the view's own storage keys stay on the machine's
  id (`am-view:<id>`), as `design/monitor-hub.md` D3 already has them.
- **The keymap** looks up each of the view's keys on the view mounted now, at the moment of the key,
  never a copy taken at the first mount. The wide phone's column gives back the pane list it
  borrowed from the view, and shows the Sessions tab, before the view is unmounted.
- **Write mode** reads `paired` from the open row's machine (`locate`), and repaints on each remount;
  with no row open, the switch is off and says to open a session. The help that names
  `agent-monitor --pair` shows only for an unpaired machine, of which iOS has none.
- **⌘K's rows** name their machine.
- **mdrev** loads once, from `assets` (below), which `machines` requires: the mount refuses
  `machines` without `assets`, since otherwise the first machine's bundle would serve every machine.
  `mdrevVersion` is then the bundled release, and #s352's fallback, which fetches `/markdown` from a
  machine to load the release it serves, is off.
- **Machines paired or forgotten later.** The app reloads its web view with the new list. The page's
  address keeps the open session, so the reader comes back where they were. No member changes the
  machines of a mounted app.
- **A first launch with no machine** never mounts: the app shows its pairing screen first.

The "By device" arrangement (D7) comes with phase 2, in the list.

### Kept for the native app, as `frame` is

The kit offers a host only what Hong has ruled it may change, and the declaration test refuses a
withheld name in `index.d.ts` and the guide (`VIEW_WITHHELD`, `LIST_WITHHELD`, `kit.rs`). The kit
has no category "declared for the native app only": anything declared is offered to every host.

So `mountApp` and every option below are **withheld**: exported from a second entry of the kit's
bundle, `native-app.js`, which the kit's walk takes in beside `index.js`, absent from `index.d.ts`
and the guide, held by contract cases, and described here and in the app's own `docs/kit-page.md`.
`index.js` and its declarations stay exactly what hosts are offered. This is how `frame` is kept
today (#s436: an arrangement between the components and the native app, which are built together,
not offered to hosts). It also keeps
the page's keymap (`keyboard: "page"`, withheld today) unoffered: only `mountApp` binds it, and no
host is offered `mountApp`. A new kit category is not needed; whether `mountApp` should one day be
offered to other hosts is a later question.

That the kit carries the mount is Hong's "Yes." on agent-monitor-app #s9, to the proposal that the
kit ship the whole web app as one mount for the native app, its native integration through options
the kit names for the native app under its contract tests: one artifact, one pin, the kit's tests
holding every byte. The second entry is how the kit carries it while keeping those options
withheld. Two shapes outside the kit were weighed and set aside for that reason: the app building
the mount from claude-replay's source tree at the pinned tag (its build would need this repository
and its generated files, and the kit's tests would no longer hold what it ships), and a separate
tarball beside the kit (a second artifact to publish, check and pin in step). The kit's
`package.json` keeps `exports` naming `index.js` alone, so a host resolving the package reaches only
what it is offered; the app loads `native-app.js` by its path in the bundle, as its page loads the
kit's files under `/kit/` today.

- **`frame`**: an agent's HTML page in a frame on the app's own scheme, passed to the view as now.
  No "frame gone" event is added. The view asks for a page's address once per tab and keeps it for
  the tab's life, so the app may drop an address only when no tab can still hold it, which it knows
  only when the web view reloads. A remount on another machine (below) tells the app nothing, so the
  addresses the old view asked for stay in the app's store until that reload, within the store's
  caps (1,024 pages or 128 MiB), as a long session's pages do today (agent-monitor-app #s9, whose
  rule this keeps). A signal for it would be a new withheld option, and Hong's ruling.
- **`assets`**: where mdrev's bundle and the Mermaid runtime load from, apart from each machine's
  routes. The app bundles the releases the kit's release pins (the app's `pins.js`; D3: no code from
  a machine runs in the page). Today the view takes both from its handle: mdrev's bundle and its
  routes through one base (`mdrevUrl`, in `mdrev-pane.js`), and the Mermaid runtime through `url`
  from the page's `mermaid-runtime` meta (`session-view.js`), so the app wraps each handle to send
  those few paths to its bundle. `assets` is an option of the view, withheld from hosts
  (`VIEW_WITHHELD`), which the mount passes on (#s595 gave it its shape: `{ mdrev, mdrevVersion,
  mermaid }`, the base the bundle is under as `/mdrev/<mdrevVersion>/mdrev.js` and `mdrev.css`, the
  release that bundle is, and the runtime's address; the mount writes the runtime's meta into a page
  that names none): the view loads mdrev's bundle and the Mermaid
  runtime from it, and every route, mdrev's included, still goes through the row's handle, whose
  `mdrev` base names the routes alone. The wrapped `url` and `mdrevUrl` members go. A machine whose
  monitor pins a newer mdrev than the bundled one reads with the older guest, the direction #s352
  never had to be safe in; mdrev's conformance check is run with the bundled guest against the
  newest monitor release as well as the oldest (D8), on the review prefix as well as the pane's,
  since a write a mismatched guest makes there reaches colleagues, and a machine the guest fails is
  listed with a note to update the app.
- **The theme**: the web app's own toggle is the app's theme setting, kept in the page's storage on
  the app's scheme. `theme` names the theme to start in when nothing is stored, which the app sets
  from the device's appearance. No other theme control is added.
- **Scripts**: the mount imports only the modules beside it in the kit and loads nothing from a
  machine. A contract case mounts it under a policy that allows scripts from the kit's directory
  alone.
- **Saving a file and links out**: no option. The page does what it does in a browser, a download
  link or a link with an address, and the app's navigation rules take each: the save through the
  relay into the share sheet (agent-monitor-app #s3), and the sheet that asks before a link leaves
  the app (#s12). The app's walk holds both. An option is added only if a measurement shows the
  page's own path does not reach those rules.
- **`show`**: passed on to the view's and the list's optional controls as each declares them. The
  app passes `detach: false` until detached windows come (phase 4).

So each of the app's four reaches past the guide goes:
- `frame` stays what it is, a withheld option, now the mount's.
- `paneLayout` is no longer needed: the mount lays out its parts itself, as `app.js` does.
- The wrapped `url` and `mdrevUrl` members give way to `assets`. `local` is not needed on iOS: an
  `app://monitor/…` base already reads as not local (`backend.js`); only This Mac, in phase 4, needs
  a local handle, and is settled then.
- Of the two CSS rules, the back control's room goes with the app's own layout, and Review hidden
  goes too: Hong chose shared review in the app (#s554), so the app hides nothing of the web app.

### Writes

Hong chose on agent-monitor-app #s14 (2026-11-17) that the app writes from the moment it mounts the
web app: the compose box, dialog answers and pane grants, as on the web app, each under that
machine's own gate and grant. The mount draws its own write switch, help and confirm dialog, as
`app.js` does, so it takes no `write` option. The relay passes the write routes of the compose box,
dialogs and pane grants, and still refuses every frame's request and Codex native mode's
(`/api/codex/*`), whatever the page shows (D3).

**Shared review comes too** (Hong on #s554, 2026-11-17: "In, as on the web app"). Review on a phone
opens the full-screen sheet over `api/mdrev-review/`, as on the web app, and the relay passes every
route that prefix answers to the row's machine: the threads (`review`), `review/fetch` and
`review/push`, a note's body edit (`PATCH`) and Share (`annotations/{id}/share`), `hide`, the
reader's notes with `--viewer`, draft 8's `review/subscribers`, `review/lookup`,
`review/subscribe`, `review/unsubscribe`, `review/confirm` and `review/read`, and `POST review/pair`.
That last one changes the machine's own setup, pairing its mdrev with a review store; the relay passes
it on purpose, as a paired phone reaches it today. It is all per machine: each write goes out under
that machine's mdrev viewer key, to the review store its checkout names, as the person that machine's
mdrev is paired as, and a machine with no viewer key answers the prefix as the pane does (no Share,
no Push drawn). The relay is the app's code: its refusal of the prefix, the tests that hold it and
`docs/kit-page.md` change in agent-monitor-app (#s13).

### What stays the native app's

Pairing and the Keychain, on its own screens; the relay and its key; the share sheet; the sheet
before a link leaves the app; running only bundled code; reloading its web view when the machines
change; and building each kit release into the app and onto TestFlight (agent-monitor-app #s13).
The layout, the top bar, where the list goes and the theme toggle are the mount's, as in a browser.

## What needs a ruling

The plan Hong agreed to on 2026-11-17 named the web app as one mount, from the same code the monitor
serves, and these native-app options: the app's frame for agent pages, saving a file to the share
sheet, links out through a sheet, the scripts loading from the bundle, the machines (pairing, the one
list, By device), and the theme. Anything more comes to him as one decision before it is built (his
"sounds good", logged on #s551).

**Named in that plan, no ruling needed:** `mountApp` with the monitor's page its first host, carried
by the kit from a second entry (his yes on #s9, above); `machines` and the merged list; `frame`;
`assets`; the theme (the toggle as the app's setting, `theme` as the start); saves and links out with
no option. Each is withheld, as above, so no host is offered more than today. So are three options
that carry what the web app already has, adding nothing a host is not offered: `backend`, the
monitor page's own server; `version`, the brand the page's body carries today; and `show`, passed on
to the controls the view and the list already let any host turn off.

**Decided by Hong:**
- **Writes** (agent-monitor-app #s14): with the mount, as above.
- **A way into the app's own screens** (#s553: "A Machines row in the sidebar (native app only)"): a
  "Machines" row in the sidebar's nav, beside Search, and a glyph on the rail after the filter, drawn
  only where the app passes `onMachines`, a withheld option of `mountApp` whose function the row
  calls. A nav row fits at every width the nav shows, the drawer and a wide phone's column included,
  so it takes no slot in the head and needs no other control to give way. The nav is hidden on the
  rail and in a column showing a pane, which is why the rail has a glyph of its own. Both are drawn
  by script (production-only chrome, as the session filter is), their classes added to
  `scripts/session-view-css.mjs`'s lists. The alternatives were a native control over the web view
  and none at all.
- **Shared review** (#s554: "In, as on the web app"): as above, under Writes. The alternatives were
  out, with Review hidden through a withheld control, or out with Review drawn and failing.
- **Review on an iPad** (#s555: "The phone's Review sheet on an iPad too (native app only)"):
  `reviewSheet`, a withheld option of `mountApp` beside `onMachines`, which the app passes and the
  mount passes on to the view, as it does `assets` (the view's side in `VIEW_WITHHELD`). With it
  the view offers the phone's Review and its sheet at every width. The app passes it while it
  opens no window of its own, and stops passing it once it does, on each device by its own
  release, with no kit release needed. No other host is offered it, and what `detach: false` does
  for every other host is unchanged.
  - Why: on the web app, shared review lives in two places. A phone (the view's phone query) gets
    the Review sheet, and the view draws Review there only. A desktop gets the detached tab, the
    pane's ↗, which opens the document on a page of its own. In the app an iPad takes the desktop's
    layout with its ↗ off (`detach: false`, since the app opens no second window yet), and the
    detached page is not in the kit until embedding's step 8. Without the option, an iPad at full
    width had no shared review, while an iPhone and an iPad in a narrow Split View did.
  - The Mac app has the same gap until it opens windows of its own (agent-monitor-app
    `docs/mac-app.md`, slice 3), and passes the same option. Hong's words name the iPad, and the
    Mac app is the same native app: a note on #s555 before his answer said the answer would apply
    to a full-width Mac window too.
  - The alternatives were no kit change, leaving a full-width iPad without shared review until the
    detached page comes, and (not offered) the view drawing Review wherever `detach` is off, for
    every host, which would change what a host that turns ↗ off gets.

Nothing else is offered. If a later review finds an item none of these covers, it goes to him the
same way.

**What became of what he asked to see** (his "sounds good" covered a switch to hide Review, a
signal to free agent pages, `paneLayout` and the keyboard mode):
- hiding Review is not needed: he chose shared review in the app (#s554);
- the frame-done signal is dropped: the app frees the pages' addresses when the web view reloads,
  and its store's caps bound them meanwhile (`frame`, above);
- `paneLayout` is not needed: the mount lays its parts out itself;
- the keyboard mode stays withheld: only `mountApp` binds the page's keymap, and no host is offered
  `mountApp`.

## Details for the stages

- **The address on the app's scheme.** The mount writes `?session=` and pushes history, as the web
  app does. Whether a `WKWebView` page on a custom scheme keeps both, across a reload too, is
  measured now, in the app's current page, before stage 2 starts. If it does not, the mount takes
  the address as an option there. The app today remembers the last session in storage and opens it
  at launch; where the address names no session, the mount opens the first, as the web app does, and
  the measurement settles whether the app's memory stays.
- **Copy link.** The view's Copy link copies the page's address, which in the app is an `app://`
  address, useless outside it. What it copies there (the machine's own address for the session, or
  nothing) is settled in stage 4.
- **Storage keys.** The mount keeps the web app's keys. On the app's scheme they are the app's
  alone. The list's keys that hold a session id hold the merged id; the view's hold the machine's own.
- **Read marks as the sheet closes.** mdrev sends `review/read` with `{now: true}` and `keepalive`
  as a page leaves. Whether the app's scheme handler receives a keepalive request at all, at a
  reload or as the app goes to the background, is measured in stage 4; if not, the marks wait for
  the next `review/fetch`, which sends what is due.
- **Renaming.** `app-mount.js` joins `ui::asset`, and the class checks in
  `scripts/session-view-css.mjs` read it as they read `app.js` today.

## The order

1. **#s540 and #s541 land** (#s540 shipped in v1.529.0; #s541 in flight).
2. **The mount, for one server.** `app-mount.js` with `mountApp`, the shell as a generated module,
   the wrapper with the three layers, `backend` and `version` as options; `app.js` its host on the
   monitor's page. In the kit (`kit.rs`): a second entry, `native-app.js`, left out of
   `package.json`'s `exports`; the walk starting from both entries; `app-shell.css` added to the
   stylesheets; the bundle test widened to that set, and still asserting `app.js` is not in the
   kit. The guards ship with the entry: `mountApp` refuses any option it does not name, from a list
   of its own as the view's `MOUNT_OPTIONS` is, and a second call throws; before it draws it also
   refuses options that are no object, a wrapper that is not on the page and a page that already
   holds an `#app`; the declaration test refuses `mountApp` in `index.d.ts` and the guide, and the
   names only the mount takes in the components' declarations (a list beside `VIEW_WITHHELD`), and
   each later stage adds its options to both lists. The whole browser
   suite and a Simulator walk show nothing changed. Released.
3. **Machines in the mount**, on #s541: `machines`, the view remounted per machine with the ids
   mapped, the keymap read at the key, write mode per machine, ⌘K's machines, `assets` and the
   fallback off. Contract cases: `machines` without `assets` throws, and `assets` reaches mdrev's
   bundle and the Mermaid runtime while every route goes through the row's handle. A browser case
   with two hermetic monitors. No control into the app's screens yet. Released.
4. **The rest for the native app**: the start theme, `show` passed on, the Machines row and its
   rail glyph (`onMachines`, #s553), and `reviewSheet` passed on to the view (#s555). Contract
   cases: the start theme yields to a stored one, the Machines row is drawn only where the app
   passes `onMachines`, and Review is drawn at a desktop width only where the app passes
   `reviewSheet`. A kit page case under the app's policy. Released.
5. **agent-monitor-app #s13** mounts it and drops its own chrome.

Each stage lands on its own, with the monitor's page working at every step.

## Testing

- **The monitor's page:** its whole browser suite, unchanged, at every stage. That is the proof the
  mount is the web app.
- **The kit page:** `mountApp` from the kit alone, over two hermetic monitors: one merged list,
  each machine's rows opening on their own machine, ⌘K naming machines, a stopped machine's rows
  greyed, the view's keys working after a switch of machine, and scripts allowed from the kit only.
- **Writes on each machine:** on that page, the compose box sending to a session of each machine, a
  dialog answered and a pane grant asked on each, each under that machine's own gate.
- **The Machines row**: hit-tested in the nav and on the rail at the list's minimum width, in the
  drawer and in a wide phone's column; absent wherever `onMachines` is not passed.
- **Shared review on each machine**: on the kit page, a hermetic review store per machine, as
  `the_detached_tab_offers_shared_review_and_the_pane_does_not` builds one, with a push, a share and
  a subscription from the sheet reaching each machine's own store; mdrev's `conform` on each
  machine's review prefix with the bundled guest.
- **Review at every width**: on the kit page at a desktop width, Review drawn and its sheet opened,
  a note shared and the sheet closed where the app passes `reviewSheet`; no Review there where it
  does not; the monitor's page unchanged
  (`the_detached_tab_offers_shared_review_and_the_pane_does_not` still passes).
- **The width study** (#s517) run on the kit page as on the monitor's.
- **The app's walk** on the Simulator: an iPhone upright and sideways, and an iPad, light and dark,
  with the Review sheet opened, a note shared and the sheet closed on each.
