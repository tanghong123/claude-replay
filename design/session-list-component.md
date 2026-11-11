# The session list as a component

> **DRAFT 1** (#s430, 2026-11-10). Step 5 of `design/embedding.md` ("The order of the work"). Not
> built. It starts once #s429's stage 7 is on main: stage 7 settles `state.js` (the view keeps its
> own state; `indexState` and `persist` become the app's alone) and changes `app.js`, which this
> step mostly moves.

## What it is for

A host shows the session list in a pane of its own: the sessions to show, their state, project and
agent, the filter and the arrangements. Choosing a session tells the host, which opens it in a
session view (`design/session-view-component.md`). Today the list exists only inside the app
shell's page, written into `app.js` beside the app's own chrome. This step makes it one component
with one entry point, and rebuilds the app shell to mount it the way a host would. As in step 4,
the app's browser suite runs against the app at every stage and stays green: that is the proof
nothing changed for the app.

## What the session list is today

- **Script:** about three quarters of `app.js` (1,217 lines): the session index store and its
  refresh, the tree and its rows (by project and agent, a flat list by activity, an arrangement
  read from another app), side sessions and workers, the session filter and its sheet, the
  arrangement strip and the section menus, expand and collapse all, hiding and restoring a
  session, read marks, the collapsed rail's agent marks and their flyout, and the list keys
  (`list-next`, `list-prev`). `session-list.js` already holds the arrangement helpers, with no
  DOM.
- **Markup:** `aside.sidebar` in `app-shell.html`, its head shared with the app (the brand, the
  theme toggle, the session search), plus controls created at runtime: the filter button and
  sheet, expand all, the arrangement strip, the resizer.
- **State:** `indexState` (the groups and rows, the selection, folds, filter buckets, the
  arrangement and section views, the sidebar, read marks), written by `persist()` with the app's
  other keys.
- **Styles:** in `app-shell.css`, hung on `#app` and `.sidebar`; the flyout and the section menu
  sit in `body`. `#app` carries the list's classes (`sessions-flat`, `sessions-arranged`) beside
  the app's (`sidebar-off`, `sidebar-tight`, `mobile-detail`) and the view's copies.
- **Opening a session:** every path goes through the app's `selectSession(id, push)`: it closes
  the phone's drawer, marks the row selected and read, redraws the tree, tells the view and
  writes the history.

## Decisions

### One entry point

`mountSessionList(element, options)` in a module of its own, `session-list-view.js`
(`session-list.js` keeps the helpers it already holds). It draws the list inside `element` and
returns a handle:

- the rows: `rows()`, `row(id)`, `workers(id)` and `selected()`;
- `select(id)`: mark a session selected without telling the host, for a selection the host made
  (the history, the view's parent control, the session search);
- `take(row, group)`: add a session the index does not hold yet. Codex native mode's workspace
  (`codex-workspace/monitor-controls.js`) does this today by writing into `indexState` for a
  native thread the index has not listed;
- `refresh()`, `resized()` and `unmount()`.

### The list owns the index and the selection

The session index store (`session-index-store.js`, which reads and merges into `indexState`) moves
into the list. It reads the session list route through the backend handle, as the view reads its
routes, so a host's list fills itself. Native mode's workspace stops importing `state.js`: it reads
the selected row and adds a row through the list's handle.

**The keys are written at the same moments as now.** The app's `persist()` writes every key on
every app action, so a reader who only folds a group or changes the filter also stores the view's
outline choice; #s429's stage 7 kept that by having the app write the view's choices with its own.
The list's store keeps those moments: an action that writes today still writes every key it wrote,
the list's and the view's alike. The folds, the filter, the
arrangement and the read marks go with it, each under its key as today, written by the list's own
store; the app's `persist` stops writing them. The app asks the list's handle for what the view's
options need (`selectedId`, `sessionRow`, `sessionWorkers`) and for the session search's rows.

### Opening a session is an event

The list raises `onOpen({id, push})` when the reader chooses a session: a row, the rail's flyout,
the list keys. It has already marked the row selected and read. The app answers it as
`selectSession` does now: closes the drawer, opens the session in the view and writes the history.
A selection that starts elsewhere (the history, the session search, the view's parent control)
goes the other way, through `select(id)`.

### What stays the app's

As `design/embedding.md` gives it ("The standalone app becomes a host"): the brand and the
version, pairing, the theme toggle, the session search layer, the toast, the drawer and its handle
on a phone, and placing the two components. Placing includes the sidebar's frame: its width and
the resizer, and collapsing it to the rail (`sidebar-off` and `sidebar-tight` stay on `#app`,
whose grid they change). The wide phone's column stays the app's too: it borrows the view's panes
beside the list.

### The rail is the list's compact form

The collapsed rail's agent marks and their flyout are the list drawn small. The app decides when
the sidebar is collapsed and tells the list (`compact(on)`); the list draws its marks and flyout.
The rail's other buttons (expand, the session search, write mode) are the app's and the view's,
placed in the rail by the app. Step 6 makes compact a matter of the pane's width.

### The list's controls are a strip the host places

The sidebar's head is two lines by construction (#223): the brand, then one row of controls that
mixes the app's (the theme, the session search, collapsing the sidebar) with the list's (the
filter, expand all, collapse all). The list draws its controls as one strip and the handle offers
it (`controls()`); the app moves that strip into its head's row, so the head looks as it does now.
A host that does not place it gets the strip at the top of the list. `the_app_shell_resizes_the_session_list`,
which hit-tests every control of that row at the sidebar's minimum, holds the app unchanged.

### Keys, theme and styles, as the view's

The list answers its keys only while the reader works in it (the view's `keyboard` option, #s436);
the app keeps `sidebar-toggle` and the session search. It takes the same `theme` option as the
view, light or dark, so a host sets both from one value and the two never disagree. The list's
rules hang on its own element, `#sessionList`, the way the view's hang on `#sessionView`, and a
class the collaboration app styles in its own page is renamed with a prefix, as the view's were
(#s438).

### Lookups stay inside the list, and a late call finds nothing

`view-root.js` looks ids up in the document, so after an unmount and a second mount in the same
element, the old view's late callbacks found the new view's nodes; #s429's stage 7 fences that
(work only while mounted, a handle that does nothing after unmount). Two components with fixed
ids on one page make it likelier. So the list looks its elements up inside its own element from
stage 1, and fences its timers, listeners and handle the same way.

## The order

Each landing is page code: the whole local browser suite and an iOS Simulator walk (the drawer,
the rail and its flyout, the filter, the arrangements, a wide phone's column) before it lands.

1. **The list's state on its own element.** `sessions-flat` and `sessions-arranged` move from
   `#app` to the list's element, with the rules that read them and the cases that look for them.
   For now that element is the sidebar, given the id `sessionList` (an id, so each rule weighs what
   it did on `#app`): a wrapper around the tree would break the generated reference rules that name
   the tree as the sidebar's child (`.sidebar>.tree`), and the list's own markup (stage 3) carves
   the element out of the sidebar anyway. **Built** (#s430), not landed: it lands with stage 2.
2. **The split, with lookups from the list's element.** The list's code moves to
   `session-list-view.js` behind `mountSessionList`, with the index store, the selection, the
   list's state and `onOpen`, and finds its elements inside its element, never the document. The
   app mounts it and answers the event. The contract's assertions on the list's source keep their
   words and move to the new module in the same commit. A new browser case mounts the list alone in
   a host page, chooses a session and hears `onOpen`. Lands with stage 1. **Built** (#s430), not
   landed. As built, the handle's redraw is `redraw()` (a ban on `.render()` in the shell's sources
   keeps that name for the engine), and three more names came out of the code: the option
   `newSession` (native mode's project buttons in the tree) and the handle's `setBuckets` and
   `filtered()` (the session search clears the filter for a row it is hiding). `select(id)` takes no
   `push`, which only the app's half uses; `onIndex` is raised before the list draws the new rows, so
   the app's half (a selection gone, the view told, a first session chosen) runs ahead of the draw as
   it did; and the mount sets `#sessionList` on its element itself.
3. **Markup and styles apart.** The list builds its markup from a template of its own, and its
   rules hang on its element in a stylesheet of its own, generated as the view's is. Lands as one.
4. **Options and events as the app uses them.** `keyboard`, `theme`, `show` (the filter, the
   arrangements, hiding), `onOpen`, `compact`, `controls()` and `unmount`; unknown option names are
   refused, as the view's are, and a contract check holds the names. The unmount case follows the
   view's (stage 7): the page's listeners the same before and after, no late work, the handle
   fenced, the host's values put back, and a second mount that works. Lands alone.

To check once stage 7 is on main: `session-index-store.js` imports `indexState`; the rail's write
button is bound in `native-mode.js` through `page` and stays with the app and native mode, though
`take()` touches that module; and the contract blocks this step moves sit beside stage 7's new ones
(its `write` and `frame` options).

Left for step 6: compact rows when the pane is narrow, and the rail chosen by the pane's width.
