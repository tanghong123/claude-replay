# Embedding agent-monitor's session view and session list

*agent-monitor kit {{version}} · for a team putting agent-monitor's parts inside an app of its own*

agent-monitor's page is two parts: the **session list** (the sessions on a machine, their state,
project and agent, with a filter) and the **session view** (one session's transcript, its outline
and artifact panes, and the compose box). This kit is those two parts as a host mounts them in a
page of its own, from one release it pins and upgrades on purpose, as hosts do with mdrev's kit.

## 1. What is in the box

```text
agent-monitor-kit-{{version}}/
  package.json             the release's version, and the type that makes node read .js as modules
  bundle/                  what the reader's browser runs, served by you as static files
    index.js                 the entry: mountSessionView, mountSessionList, backendHandle,
                             useBackend and persist
    index.d.ts               what each takes and answers
    session-view.css         the view's stylesheet, every rule under #sessionView
    session-list.css         the list's, every rule under #sessionList
    *.js, shared/*.js        the modules index.js imports, byte for byte as agent-monitor
                             {{version}} serves them at /monitor-ui/
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

Run the conformance check against it before you mount anything (section 7).

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
that start with `am-`. Two more are ours: `mdrev-held:` keys in `sessionStorage` carry a document
the artifact pane opens in a tab of its own, and a `localStorage` key named `viewportTrace`, if you
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
  its keys), `history` (the viewport history's Save), `detach` (the artifact pane's ↗).
- **`write`**: `{ on(), confirm(ask) }`, for writing to a session. Left out, the view offers no
  writes. `on()` says whether the reader switched writing on (call `writeChanged()` when it
  changes); `confirm({ title, text, passcode })` asks before the server types into a live pane,
  and answers the passcode, `""` where none was asked, to go on.
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
through the backend the view was given, or the one `useBackend(handle)` names: call it before you
mount the list on its own. Its options, each optional, are in `index.d.ts`: `theme`, `show`
(`filter`, `hiding`, `arrangements`, each shown unless `false`), `persist`, `onOpen`, `onIndex`
and `onToast`. Its keys, like the view's, answer only while the reader works in it. Pass the
kit's `persist` so the list remembers the reader's folds, filter and read marks.

The list hands you the reader's choice (`onOpen`) and moves its selection only when you call
`select(id)`, so you can keep the reader where they are.

With both parts mounted, give the view the list's answers: `selectedId: () => list.selected()`,
`sessionRow: id => list.row(id)`, `sessionWorkers: id => list.workers(id)`, `renderList: () =>
list.redraw()`, `refreshIndex: () => list.refresh()`, `pauseIndex: () => list.stop()` and
`resumeIndex: () => list.start()`, as agent-monitor's own page does, and call the view's
`indexChanged()` from the list's `onIndex`.

## 7. The conformance check

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

## 8. Upgrading

Pin one release: its kit, and a server that speaks the contract version this guide names. A new
release that only adds to the contract keeps its version; one that removes or changes something
moves it, and its guide says so. To upgrade, read the new release's notes, swap `bundle/`, run
the conformance check again against your server, and load your page.

Not in this kit yet: a page for a document or an image in a tab of its own (the ↗ opens the
server's `/markdown` and `/image` pages), and a mapping of your sign-in to ours.
