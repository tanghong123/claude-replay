# Embedding agent-monitor's session view and session list

*agent-monitor kit {{version}} · for a team putting agent-monitor's parts inside an app of its own*

agent-monitor's page is two parts: the **session list** (the sessions on a machine, their state,
project and agent, with a filter) and the **session view** (one session's transcript, its outline
and artifact panes, and the compose box). This kit is those two parts as a host mounts them in a
page of its own, and the page a transcript, a document or an image opens in when the reader sends
it to a tab of its own, from one release it pins and upgrades on purpose, as hosts do with mdrev's
kit.

## 1. What is in the box

```text
agent-monitor-kit-{{version}}/
  package.json             the release's version, and the type that makes node read .js as modules
  bundle/                  what the reader's browser runs, served by you as static files
    index.js                 the entry: mountSessionView, mountSessionList, mountDetached,
                             backendHandle, useBackend, persist, and mergedBackend and
                             splitMachineId for one list over several machines
    index.d.ts               what each takes and answers
    session-view.css         the view's stylesheet, every rule under #sessionView
    session-list.css         the list's, every rule under #sessionList
    detached.css             a tab of its own's, every rule under #detachedView
    *.js, shared/*.js        the modules index.js imports, byte for byte as agent-monitor
                             {{version}} serves them at /monitor-ui/
    other files              agent-monitor's own apps' (app-shell.css among them): a host
                             loads none of them
  docs/
    embedding-guide.md       this guide
    backend-contract.md      the routes the parts read, contract version {{contract}}
  conform.mjs              the conformance check: run it against your server
  LICENSE                  the licence the modules are copies under
```

Check the download against the digest the release publishes beside it,
`agent-monitor-kit-{{version}}.sha256` (`sha256sum -c` reads it), before you unpack it.

## 2. The server behind the parts

The parts show nothing of their own: they read every session, record, file and image from a
server that answers the backend contract (`docs/backend-contract.md`, version {{contract}}). Two
ways to have one:

- **agent-monitor itself**, on the machine with the sessions. Its routes are the contract's
  reference.
- **Its backend mounted in your server**: the route table answering under a prefix of yours, over
  the sessions you name, behind your sign-in (`design/backend-mount.md` in the repository).

The parts fetch from that server with the page's credentials and no cross-origin requests, so your
page and the server share an origin: serve the page from it, or relay the server's routes under a
prefix of your page's origin.

Run the conformance check against it before you mount anything (section 8).

## 3. Serving the bundle

Serve `bundle/` as static files at any path of yours, for example `/static/agent-monitor/`. The
modules import each other by relative path, so the prefix is yours to choose. Serve `.js` as
`text/javascript` and `.css` as `text/css`.

Two pinned runtimes come from the server, not from the kit:

- **mdrev**, which shows Markdown and code in the artifact pane: the server pins one release and
  serves it under `/mdrev/<version>/`. agent-monitor {{version}} pins mdrev {{mdrev}}. Pass the
  version as the backend handle's `mdrevVersion`; without it, Markdown shows as plain text.
- **Mermaid**, which draws diagrams in the transcript: the server serves it at
  `{{mermaid}}`. Name that path in your page's head:

  ```html
  <meta name="mermaid-runtime" content="{{mermaid}}">
  ```

  The view loads it through the backend handle, so a prefix is added for you. Without the line,
  a diagram shows as its source.

A host whose server is its own, not agent-monitor's backend, serves both itself, at those paths:
mdrev from a release it vendors (the handle's `mdrev` base may point elsewhere), and Mermaid's
runtime at the path above, which names its release.

## 4. The page

Your page carries, for each part it mounts:

- its stylesheet, `session-view.css` or `session-list.css`;
- an empty element for it, with no id or the part's own (`sessionView`, `sessionList`); an
  element with another id is refused. The part draws everything inside it and takes that id.
  Give the element a place and a size and style nothing else of it; the part lays out its own
  insides at any size, from a phone's width to a wide desktop's, and in a narrow box it becomes a
  phone reader by its own width.

Nothing else of ours goes in your page: no markup, no element we look up by id, no store.

Our rules hang on our two ids, so they never reach the rest of your page. Yours can reach our
elements by class name or by element, though. The view draws none of the class names one host
styles for itself (`.primary`, `.hint`, `.crumbs`, `.pinned`, `.session-main`, `.task-card`), but
other common names are ours too (`.active`, `.open`, `.selected`). Keep your class rules to your
own containers (`.your-bar .active`), and keep blanket rules (`* { … }`, `div { … }`,
`button { … }`) out of your page.

The parts remember the reader's choices (reading size, outline, folds, the filter) in your
origin's `localStorage`, and the reader's place in each session in its `sessionStorage`, under keys
that start with `am-`. Three more are ours: `mdrev-held:` and `am-image:` keys in `sessionStorage`
carry a document or an image the artifact pane opens in a tab of its own (section 7), and a `localStorage` key named `viewportTrace`, if you
set it, turns on a debugging trace. mdrev's reader in the artifact pane keeps its own reading
choices under names of its own.

## 5. Mounting the session view

```html
<div id="pane"><div id="sessionView"></div></div>
```

```js
import { backendHandle, mountSessionView } from "/static/agent-monitor/index.js";

const backend = backendHandle({ base: "/agents/monitor", paired: true, mdrevVersion: "{{mdrev}}" });
const rows = new Map(); // your session rows, by id, from GET /api/sessions
let selected = "";
const view = mountSessionView(document.getElementById("sessionView"), {
  backend,
  grid: document.getElementById("pane"),
  theme: "light",
  selectedId: () => selected,
  sessionRow: id => rows.get(id) || null,
  sessionWorkers: () => [],
  selectSession: id => { selected = id; view.select(id); view.open(id); },
  renderList: () => {},
  refreshIndex: () => {},
  pauseIndex: () => {},
  resumeIndex: () => {},
  onTitle: ({ title }) => { document.title = title; },
});
selected = "<a session id>";
view.select(selected);
view.open(selected);
```

The options, each in `index.d.ts`:

- **Required:** `grid` (the box around the view: a phone's sheets are measured against it),
  `selectedId`, `sessionRow`, `sessionWorkers` (what your session index says), `selectSession`
  (the view asks you to select one), and `renderList`, `refreshIndex`, `pauseIndex`,
  `resumeIndex` (your list and its index, for the compose box's controls).
- **`theme`**: `"light"` (the default) or `"dark"`; `setTheme` changes it later.
- **`backend`**: the handle above. Pass one: without it the view reads the server
  `useBackend` last named, and with neither, the server at the directory of your page's address,
  as an unpaired one unless your page's body says `data-paired="true"` as the kit loads.
- **`show`**: the optional controls, each shown unless `false`: `reading` (the Reading menu and
  its keys), `history` (the viewport history's Save), `detach` (every ↗ to a tab of its own).
- **`detachTo`**: your page that draws a tab of its own (section 7). Left out, the artifact pane's
  ↗ opens the server's pages and the transcript has none.
- **`write`**: `{ on(), confirm(ask) }`, for writing to a session. Left out, the view offers no
  writes. `on()` says whether the reader switched writing on (call `writeChanged()` when it
  changes); `confirm({ title, text, passcode })` asks before the server types into a live pane,
  and answers the passcode, `""` where none was asked, to go on. The view writes only where the
  backend handle says this reader's writes reach the server: its `write` option is `"open"` (by
  default it follows `paired`). Where they cannot, give the handle what is missing, as
  agent-monitor's page reads it from `<body data-write>` (`docs/backend-contract.md`, "The page's
  facts"): `"monitor"` (the server is not paired), `"browser"` (this reader holds no token) or
  `"host"` (your server serves no writes). The handle answers `writes` and `writeNeed`, which your
  own Write mode control can show; and with writing on, a session the view cannot write to keeps
  its compose button, greyed, with the reason, rather than offering a send the server refuses.
- **What the view tells you**, each optional, each with plain data: `onTitle`, `onTurn`,
  `onInfo`, `onFile`, `onLinkCopied`, `onToast`, `onOutline`. A handler that throws is reported
  and the view goes on.
- **`room()`**: how many more pixels you could give the view by narrowing what you lay beside
  it; the view opens its outline only where it fits, then tells you with `onOutline`.

Any other option name is refused, so a misspelt one fails at once.

The view binds its own keys and answers them only while the reader's last press or focus is inside
it, so your page keeps its keys everywhere else; a key the view has no action for stays yours.

The handle: `select(id)` and `open(id)` to show a session, `session()`, `close(title, detail)`,
`indexChanged()` when your rows change, `writeChanged()`, `resized()`, `setTheme(theme)`,
`closePane()`, `persist()`, and `unmount()`, which stops everything the view started and leaves
your element empty. After `unmount()` every other member does nothing. The list's handle is in
`index.d.ts` too: `start()`, `stop()`, `refresh()`, `redraw()`, `rows()`, `row(id)`,
`workers(id)`, `selected()`, `select(id)`, `setTheme(theme)` and `unmount()`.

## 6. Mounting the session list

```js
import { mountSessionList, persist } from "/static/agent-monitor/index.js";

const list = mountSessionList(document.getElementById("sessionList"), {
  persist,
  onOpen: ({ id }) => { list.select(id); view.select(id); view.open(id); },
});
list.start();
```

The list reads `GET /api/sessions` every 5 seconds between `start()` and `stop()`. It reads
through its own `backend` when you pass one; otherwise through the backend the view was given, or
the one `useBackend(handle)` names (call it before you mount the list on its own). A list given its
own keeps to it, so it can list one server's sessions beside a view you point at another. Its
options, each optional, are in `index.d.ts`: `theme`, `show` (`filter`, `hiding`, `arrangements`,
each shown unless `false`), `backend`, `persist`, `onOpen`, `onIndex` and `onToast`. Its keys, like
the view's, answer only while the reader works in it. Pass the kit's `persist` so the list
remembers the reader's folds, filter and read marks.

The list hands you the reader's choice (`onOpen`) and moves its selection only when you call
`select(id)`, so you can keep the reader where they are.

### One list over several machines

To show the sessions of several machines in one list (every Mac a reader has paired, say), give
the list a merged source as its `backend`. It takes a handle on each machine's server, a key of
yours for the machine (with no `/` in it) and the name a reader knows it by:

```js
import { backendHandle, mergedBackend, mountSessionList, mountSessionView } from "/static/agent-monitor/index.js";

const merged = mergedBackend([
  { machine: "studio", label: "Studio Mac", handle: backendHandle({ base: "/machines/studio", paired: true }) },
  { machine: "laptop", label: "Laptop", handle: backendHandle({ base: "/machines/laptop", paired: true }) },
]);
const list = mountSessionList(document.getElementById("sessionList"), {
  backend: merged,
  onOpen: ({ id }) => show(id),
});
list.start();
```

What the list then shows:

- Every machine's sessions. A project's head carries its machine's name, and so does a row in a
  flat arrangement. The same folder on two machines is two projects. A `label` that is a host name
  is shown by its first part (`studio.tail1234.ts.net` shows `studio`); give the whole name, and the
  list keeps it for the label's title.
- The filter offers **Group by machine** (between Idle and Include hidden, while the list holds more
  than one machine): a head for each machine, in the order you listed them, holding that machine's
  own agents, projects and sessions, and saying how the machine answers while it does not. The
  reader's choice is remembered like the filter's others, through the `persist` you pass. It is
  part of the `filter` control: with `show: { filter: false }` the list is not grouped.
- The list's ids are `<machine>/<that machine's own id>`; `splitMachineId(id)` takes one apart.
- A hide or a restore goes to the row's own machine.
- Each read asks every machine that is not still answering the last one, and waits at most
  `timeout` (4 seconds unless you pass `{ timeout }` in ms) for those that answered in time before.
  A machine that does not answer in time, or fails, keeps its last rows, greyed, under a head over
  the list saying since when, and is not waited for again until it answers; one that has never
  answered gets a head saying why, and no rows. An ask still out after eight times `timeout` is
  given up, and the machine is asked afresh.
- Other apps' arrangements (the Arrange choices) are shown disabled, with the reason: they come
  from one machine's apps, and make no one list over several.
- The handle you pass for a machine is the one the merged source reads it through, and the one
  `locate` gives back, used as it is.

The view shows one machine's session at a time, read from that machine. `merged.locate(id)` names
a listed session's machine, its handle and the session's own id, and `merged.forView(list,
machine)` gives the view's row options in that machine's ids. When the reader opens a session of
another machine, unmount the view and mount it again with that machine's handle:

```js
let view = null, on = "";
function show(id) {
  const where = merged.locate(id);
  if (!where) return; // an id that names none of the machines
  list.select(id);
  if (where.machine !== on) {
    if (view) view.unmount();
    const rows = merged.forView(list, where.machine);
    view = mountSessionView(document.getElementById("sessionView"), {
      backend: where.handle,
      grid: document.getElementById("pane"),
      selectedId: rows.selectedId,
      sessionRow: rows.sessionRow,
      sessionWorkers: rows.sessionWorkers,
      selectSession: own => show(rows.listId(own)),
      renderList: () => list.redraw(),
      refreshIndex: () => list.refresh(),
      pauseIndex: () => list.stop(),
      resumeIndex: () => list.start(),
    });
    on = where.machine;
  }
  view.select(where.id);
  view.open(where.id);
}
```

Each machine's server is relayed under a prefix of your page's origin, as section 2 says for one.

With both parts mounted, give the view the list's answers: `selectedId: () => list.selected()`,
`sessionRow: id => list.row(id)`, `sessionWorkers: id => list.workers(id)`, `renderList: () =>
list.redraw()`, `refreshIndex: () => list.refresh()`, `pauseIndex: () => list.stop()` and
`resumeIndex: () => list.start()`, as agent-monitor's own page does, and call the view's
`indexChanged()` from the list's `onIndex`.

## 7. A tab of its own

The reader can send a session's transcript, or a document or an image from the artifact pane, to a
tab of its own. That tab is a page you serve, at a route of yours, and the kit draws it: give the
view the page's address as `detachTo`, and on that page mount `mountDetached`:

```js
mountSessionView(document.getElementById("sessionView"), { /* … */ detachTo: "/agents/detached" });
```

```html
<link rel="stylesheet" href="/static/agent-monitor/session-view.css">
<link rel="stylesheet" href="/static/agent-monitor/detached.css">
<div id="detachedView" style="position: fixed; inset: 0"></div>
```

```js
import { backendHandle, mountDetached } from "/static/agent-monitor/index.js";

const tab = mountDetached(document.getElementById("detachedView"), {
  backend: backendHandle({ base: "/agents/monitor", paired: true, mdrevVersion: "{{mdrev}}" }),
  theme: "light",
});
```

Each ↗ opens your page with a query that says what to show: `?session=<id>` a session's
transcript, `?root=…&path=…&cap=…` a Markdown document, `?path=…&sig=…` or `?held=…` an image.
`mountDetached` reads it from your page's address, draws it in the element (no id or the id
`detachedView`, empty, given a place and a size by you), and keeps the address and the page's
title current as the reader moves: another range of a document, a session they go to from this
one, which Back returns from. Its options, each optional, are in `index.d.ts`: `backend`, `theme`
and `address` (a query to read in place of your page's own, which the tab then leaves alone). The
handle has `kind`, `setTheme(theme)` and `unmount()`.

Your page's address may carry a query and a fragment of its own (`/agents/detached?team=x#top`):
the ↗ adds what to show to the query and keeps the fragment last, and the tab keeps your keys as
it rewrites the address. Its own are `session`, `root`, `path`, `cap`, `from`, `to`, `since`,
`sig`, `held` and `name`: leave those names to it, since it reads them to choose what to show and
rewrites them as the reader moves. It writes the query back in its standard form (`a+b` for
`a%20b`, `x=` for a bare `x`), so read your keys with `URLSearchParams`. A fragment reaches the
transcript's view, which reads one naming a record (`#b12`) as the place to open at.

The transcript's tab is a reader: the session view with no list, its keys bound at the page, its
index read every 5 seconds, with no writes and no ↗ of its own. Its page needs the Mermaid line
of section 3 too.

`detachTo` may also be a function, asked at each ↗ with `{ kind, query, href }` (`href` is the
server's own page for it), that answers the address to open. The view opens it with `window.open` and never `noopener`: text the transcript carries and
an image it embeds reach the tab in a copy of your page's `sessionStorage`, which a window you open
yourself does not get, and then the tab says the copy is gone. Without `detachTo` the artifact
pane's ↗ opens the server's own `/markdown` and `/image` pages and the transcript has no ↗;
`show: { detach: false }` hides every ↗.

## 8. The conformance check

`conform.mjs` checks your server against contract version {{contract}}, rule by rule. It needs
node 20 or later and nothing else:

```sh
node conform.mjs https://your.host/agents/monitor --cookie session=… --origin https://your.host
```

`--cookie` and `--header` carry the reader's sign-in and are never printed. Each rule prints as
passed, failed or skipped, under the contract section it comes from; a route of an optional tier
you do not serve answers 404 and its rules are skipped. It exits 0 when your server conforms and
1 when a rule fails. It sends no prompt, grants nothing, answers no dialog and reveals no file;
the top of the file lists the little it does change.

## 9. Upgrading

Pin one release: its kit, and a server that speaks the contract version this guide names. A new
release that only adds to the contract keeps its version; one that removes or changes something
moves it, and its guide says so. To upgrade, read the new release's notes, swap `bundle/`, run
the conformance check again against your server, and load your page.

Not in this kit yet: a mapping of your sign-in to ours.
