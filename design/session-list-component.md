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

Stage 2 builds the rows, `select` and `refresh`; `take`, `resized` and `unmount` come with the
later stages (`take` with native mode's move off `state.js`, `unmount` with stage 4).

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
the list keys. It does not move its selection itself: the host does, with `select(id)`, when it
opens the session, so a host that holds the reader (a draft to keep) leaves it where it is. The app
answers with `selectSession`, as before: it closes the drawer, takes the session as the list's
selection, opens it in the view and writes the history, and the session already open does nothing
but close the drawer. A selection that starts elsewhere (the history, the session search, the view's
parent control) takes the same path.

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
filter, expand all, collapse all). The list draws its controls as one strip and hands it to the
host (`place("controls", strip)`, since stage 3; stage 4 made it one element rather than three loose
buttons, so a host can hold it and place it again); the app moves that strip into its head's row, so
the head looks as it does now.
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
   `push`, which only the app's half uses; `onIndex` is raised before the list draws the new rows, and
   the list is drawn even if it throws. So the app's half (a selection gone, the view told, a first
   session chosen, whose `select` draws the tree itself) now runs before the list's own draw, where
   the app used to draw between the first and the second; none of it reads the drawn tree. And the
   mount sets `#sessionList` on its element itself.
3. **Markup and styles apart.** The list builds its markup from a template of its own, and its
   rules hang on its element in a stylesheet of its own, generated as the view's is. Lands as one.
   **Planned** (#s430), in five commits on one landing; the carve, its four parts, the template, the
   styles and the theme are built (below):

   - **The carve.** `#sessionList` becomes an element of its own, `div.session-list`, a child of
     the sidebar where the tree stands now, holding the arrangement strip and the tree. In the app
     it is `display: contents`, as `#sessionView` is, so the strip and the tree stay items of the
     sidebar's column and nothing moves. Keeping the whole sidebar as the list's element was
     weighed and set aside: it holds the app's brand, theme toggle, write and search rows and rail
     buttons, so every shared rule copied into the list's stylesheet (`.iconbtn`, `.on`,
     `.mode-switch`) would reach the app's own controls after `app-shell.css` and could win a tie
     it lost before; a host page would still need our sidebar's frame; and stage 4 would carve it
     anyway and move the styles twice.

     Step 6 replaced the `display: contents` (#s431 stage 1, `design/pane-layout.md`, "A box for
     each component"): the list's element is a flex column of its own, in the app the sidebar's
     child that takes the height left below its head, so it lays out what it holds wherever a host
     puts it.
   - **Four parts stay where the app shows them.** The list's head controls (Collapse all, Expand
     all, the filter) are children of the head's one row, which
     `the_app_shell_resizes_the_session_list` hit-tests child by child; the filter sheet hangs from
     the head (`top: 100%` of `.side-head`); the filter's rail button and the agent marks sit in the
     rail. The list builds each and hands it to a new option, `place(part, node)`, as it asks
     `layer(kind)` where a floating layer goes: `"controls"` (inserted before the sidebar
     collapse), `"sheet"` (appended to the head), `"rail-filter"` (after the rail's search) and
     `"marks"` (appended to the rail). The DOM is node for node what it is now, and the list looks
     up nothing of the app's (today it finds `#sidebarCollapse`, `.side-head` and
     `#sidebarMiniSearch` to anchor its parts). A host that leaves `place` out gets the controls and
     the sheet in a head at the top of the list's element, which is the sheet's containing block
     (the host case opens the sheet there), and no rail. No rule of the list's own bakes in the
     sidebar's 232px minimum: step 6 measures the list's row against its own pane. The rail flyout and the
     section menu keep floating where `layer` puts them. Stage 4 turns the controls into one strip
     (handed over through `place`) and the rail into the list's compact form (`compact(on)`).
   - **The template** is cut from the demo's shell by `scripts/extract-agent-monitor-demo.mjs`, as
     the view's is: `session-list-markup.js` holds the tree, `#collapseBtn` and the marks' holder;
     the served shell keeps an empty `#sessionList` and loses them, and the demo's two attention
     controls, which the list removes today, are cut and not kept. The parts built at runtime
     (expand all, the filter and its sheet, the rail's filter, the strip, the flyout, the section
     menu) stay script-built. The list draws Collapse all's glyph itself.

     **Built** (#s430), the carve, the four parts and the template, not landed. As built: the
     template is the three pieces in one string, Collapse all, the marks' holder and the tree, which
     the list parses into a `<template>` and keeps by reference; it looks nothing up, in its element
     or the page. It calls `place` once for each part at the end of its mount, the controls as one
     fragment, and the app answers it in `placeListPart`, a function declaration, since it runs before
     anything below the mount exists. The flyout gives the sheet back where it was placed (its parent
     and next sibling, remembered as it moves the sheet in). Without `place`, the list's own head is
     `div.session-list-head`, first in its element, under `#sessionList>.session-list-head`
     (`position: relative`, a row of the controls); the rail's two parts are built and left
     unattached. Flat mode's rule is `:is(#collapseBtn,#expandBtn)[data-flat]`, which matches in
     either head: it weighs one id and one class where the old one weighed two ids and three
     classes, and the one rule it competes with is `.iconbtn`'s `display: grid` (one class), so
     nothing changes. The demo's attention controls stay named in the app's chrome lists, so their
     reference rules stay in `app-shell.css`; the contract counts their markup, which it cuts again,
     as the app's. The host page gives its pane's element a column box of its own, since it loads
     the list's stylesheet alone, which gives `.session-list` no layout of its own (step 6's).
     A host that passes no `layer` gets the list's floating layers (a section's menu and its scrim,
     the rail's flyout) in the list's own element, where the list's stylesheet reaches them; the
     page's body, the default before, is out of that stylesheet's reach. The host case opens no
     section menu yet: arrangements in a host come with stage 4's `show`, and its case with them.
     A host that places some parts and not others gets the unplaced ones unattached, the filter's
     sheet back unattached after the rail's flyout borrowed it.
   - **The styles.** `scripts/session-view-css.mjs` derives a third file, `session-list.css`,
     linked between `app-shell.css` and `session-view.css`. Its classifier splits the app's chrome
     into the app's own, the list's body (`LIST_CHROME`: the tree, its rows and labels, the strip
     and the arrangements, the state chips) and the list's parts the app places (`LIST_OUTSIDE`:
     the controls, the sheet, the rail's filter and marks, the flyout, the section menu), which go
     in both files as the view's `OUTSIDE` does. The list's rules hang on
     `:where(#sessionList)` with the view's stand-ins for the page's root (`.session-list`, its
     `[data-theme]`), keeping every weight; its keyframes take the prefix `sl-`. The two generated
     reference rules that name the tree as the sidebar's child (`.sidebar>.tree`, and under
     `sidebar-off`) are rewritten in the app's file as `>:where(.session-list)>`, the one rewrite
     that file takes; the same in `production.css`'s child rules for the strip and the wide phone's
     column. The phone rules led by `#app` that style the list's body move to `#sessionList` first,
     id for id, while `#sessionList` is still the sidebar. Flat mode's rule that hides Collapse and
     Expand all read `#sessionList.sessions-flat` around the head, which the head is no longer
     inside: the list marks the two controls it places instead (`[data-flat]`).

     **Built** (#s430), the styles, not landed. As built: `deriveStylesheets` returns `{view, list,
     app}`, and `session-view.css` is byte for byte what it was. Each selector gets the view's
     decision, unchanged, and beside it the list's (`classifyList`): the list's file takes a copy,
     and the app's file keeps its own unless the rule is the list's alone. One thing the plan did not
     see: the rail's flyout, in the page's body, draws an agent's rows with the functions the tree
     draws them with, and the bare `.tree-row`, `.tree-title`, `.session-state`… rules style them
     there. So a rule naming only those names is in both files, as the view's `OUTSIDE` are
     (`LIST_SHOWN_OUTSIDE`, which the contract holds to what the flyout's functions draw, both ways);
     what leaves `app-shell.css` is the list's body that only the list's element holds: the tree,
     the agent heads, the strip and the arranged sections, and every rule led by `#sessionList`. A
     word beside one of the list's names is a modifier of it (a state chip's `.busy`), and the rules
     with no name of the list's own (`button`, `*`, `.iconbtn`, `.icon`, the popover rows the sheet
     borrows) are in both, so a host loading the list's file alone gets its controls and its sheet.
     `.no-results` moved from the app's chrome to the list's body (the tree draws it; the flyout and
     ⌘K too, so it is in both). On the list's element: the page's tokens and inherited properties as
     the view's element takes them (`.session-list` for `:root`, `:is(*,div)` for `html` or `body`),
     and the sidebar's `--state-*` tokens (`.session-list` for `.sidebar`, so the dark one reads
     `.session-list.session-list[data-theme="dark"]`); the derivation stops on an inherited property
     or a token the sidebar sets that would be hidden, and on a copied token whose value names one an
     element above the list declares. The list's file has no reset of its own (the view's pins
     `select` and `code`, which the list draws none of). The list's tie check takes every app rule
     with no copy in the list's file whose subject could be inside `#sessionList` (the list's body or
     shared words, under the grid, its states, the sidebar or the list) against every earlier rule
     with a copy there: none. The two `>:where(.session-list)>` rules and the strip's
     `.sidebar>:where(.session-list)>.arrange-strip` (its padding) stay the app's, so in a host the
     strip has no padding and the tree no fixed width. The split: `app-shell.css` 612 rules (682
     selectors) before, 570 (636) after; `session-list.css` 227 rules (244 selectors): 46 the list's
     alone (32 by name, 14 led by `#sessionList`), 89 shown outside it too, 33 its placed parts, 43
     with no name of its own, the rest on its element (the page's root and the sidebar's tokens).
     Keyframes `sl-pop` and `sl-status-breathe`; the app keeps `pop` and `status-breathe` for the
     flyout's rows and its own. So the one computed value that differs inside the list in the app is
     `animation-name` on a busy row's dot (`sl-status-breathe`), the same frames. The host page loads
     `session-list.css` alone. `ui.rs` `the_list_s_stylesheet_names_nothing_outside_its_element`
     holds every selector to the list's element, its keyframes to `sl-`, and the three links' order.
   - **The theme comes forward from stage 4**, as the view's did in its stage 5b: the list takes
     `theme` and has `setTheme`, so its dark rules and tokens hang on its own element; the app sets
     both components from one value.

     **Built** (#s430), not landed. The mount adds `session-list` to its element's classes, as the
     view's adds `session-view`, and stamps `data-theme` on it alone. The app passes the root's theme
     as it mounts the list, and tells both components together (`tellTheme`) from its toggle and from
     its observer of the root's `data-theme`, which since ed887b59 is how the view follows any change.
     The list mounts before the stored theme is put on the root, so that first change reaches it
     through the observer, before the page is drawn.
   - **Names.** No class of the list's is styled by the collaboration app's own styles in a way
     that reaches it (measured, its vendored copy of our CSS left out: the one rule naming a class
     the list draws is scoped under that app's own navigation), so the list renames nothing,
     unlike the view (#s438).
   - **Checks.** A static test that every rule of `session-list.css` starts at `#sessionList` and
     names nothing outside it; the contract's byte match of the three derived files, the
     classifier's lists checked both ways against what the app and the list draw, and `place` and
     `theme` among the option names. Before landing, beyond the suite and the Simulator walk, the
     throwaway probe of stage 5b: every element's computed style, old build against new, on a
     desktop at three widths in both themes, the 232px sidebar, the rail with its marks and each
     flyout, the sheet, flat and arranged, the section menu, native mode's additions to the nav row
     (they share selectors with what moves), a phone's drawer shut and open and a wide phone's
     column.
4. **Options and events as the app uses them.** `keyboard`, `theme`, `show` (the filter, the
   arrangements, hiding), `onOpen`, `compact`, the controls' strip and `unmount`; unknown option names are
   refused, as the view's are, and a contract check holds the names. The unmount case follows the
   view's (stage 7): the page's listeners the same before and after, no late work, the handle
   fenced, the host's values put back, and a second mount that works. Lands alone.

   **Built** (#s430), not landed.
   - **Names.** `MOUNT_OPTIONS` (theme, show, keyboard, persist, onOpen, onIndex, onToast,
     projectTree, projectActions, newSession, layer, place), `OPTIONAL_CONTROLS` (filter, hiding,
     arrangements) and `KEYBOARD_MODES` (engaged, page), exported as the view's are. Any other name is
     refused, and so are a `show` that is not an object, a control not in the list, a `show` value
     that is not a boolean and an unknown keyboard. (The view takes a `show` of `false`, `0` or `""` as
     no `show` at all, and refuses a string or an array as naming an unknown control "0"; its guard is
     left as it is, a landed mount the collaboration app calls.) `toast` became `onToast`, the view's name for the same event. The `compact`
     option became the handle's `compact(on)` (the host says it as the sidebar collapses or opens, and
     an open flyout closes when the list is no longer drawn small), which retired `flyoutOpen` and
     `closeFlyout`: the app no longer reaches into the flyout. `step` became `keys`, by the keymap's
     names, as the view's handle has them.
   - **`show`.** A control switched off is not drawn, and the page's choice it would make is not
     applied, so a reader is never left with sessions held back by a choice they cannot see or change;
     the choice itself stays in storage for a host that shows it. This departs from the view, whose
     controls off only stop being drawn: none of the view's changes which records are shown. The
     filter off lists every state; the arrangements off draw the tree by agent and project and read no
     other app's list, whatever arrangement the reader stored. A hide is not the page's choice: the
     monitor keeps it, for every page that reads it, so hiding off draws no hide or restore and no
     Include hidden, and what the reader hid stays out, even while Include hidden is still ticked in
     the page's state. An arranged list still shows each session the other app lists, hidden or not,
     as #s387 has it, and with hiding off draws none of them dimmed, a dim it could not explain. The
     filter's glyph says what its sheet offers ("Include hidden sessions" with the filter and the
     arrangements off), in its title, its label and the rail's, and with every control shown each
     reads as it did. The sheet
     is built from the parts shown, joined as it always was, so with all three on it is byte for byte
     the sheet it was; with all three off neither its glyph nor its sheet is drawn or handed over.
   - **Keys.** Engaged, the list binds keydown on its own element and answers ↓ and ↑ only from one
     of its rows (`resolveKey` with the list's context), every other key and those keys elsewhere left
     to the page. The list holds the focus itself, so engagement is the focus, simpler than the view's
     last press: a row is focusable and nothing else in the list takes a key. The app passes
     `keyboard: "page"` and spreads the handle's `keys` into its keymap; the list then binds no list
     keys. In either mode Enter and Space on a focused row activate it, as a click does, and the list
     hears the document's keys, taking none, to know where a row's focus came from (below). A step that moves nothing (from a project's row, past the last session)
     answers `false`, so the key is not taken, engaged or in the app's keymap, and the last session is
     not chosen again.
   - **Where a row's focus came from.** A rewrite of the tree (the host's select, the index's next
     read, a fold) drops the focus to the page, where the next key is no longer the list's, so after a
     step the next arrow did nothing (in the app too, before stage 4). The rewrite gives a session row
     its focus back, but only when the keyboard gave it: a row a press focused, carried, keeps every key
     of the page's in the list, and `[`, `]`, `j` and Space do nothing until the reader clicks in the
     transcript. The list keeps which input came last, a press in the tree (a pointerdown there) or a
     key anywhere (a keydown on the document, a modifier alone not counting), and whether the focus in
     the tree is the press's:

     | How a row gets the focus | The press's | Carried by a rewrite |
     |---|---|---|
     | A press on a row without the focus (the mouse focuses it as it goes down) | yes | no |
     | A press on the row that has the focus | yes | no |
     | A finger's tap (its focus comes as it lifts, after its pointerdown) | yes | no |
     | Tab, after a press that focused nothing or none | no | yes |
     | A key's step, and the rewrite's own refocus | no | yes |
     | Enter on a row (a click with no pointer) | as it was | as it was |
     | The window giving it back (another app, a tab, a window) | as it was | as it was |

     The direction that must never fail is a press's focus carried, which leaves the page's keys in
     the list. The other, the keyboard's focus dropped at a rewrite, is what main does after every
     step. A click with the mouse also hands the page its keys as it chooses the session (a click with
     a pointer blurs the row), since the open session clicked again redraws nothing, and its row kept
     them, on main too; Enter's click leaves the focus where the keyboard put it. A click on a fork
     family's ⑂ chip hands them back too (#s460): a family holding the open session is drawn open
     whatever its chip says, so that click redraws nothing either. Four reviews shaped
     this. A refocus in the step alone held only until the next read rewrote the tree; a carry of any
     focused row kept a clicked one; a carry that left the mouse's focus to the click kept a row
     pressed and dragged off it, a press straddling a rewrite (#s72) and, in Safari, a row whose button
     was clicked; and a mark the press set and the next focus cleared went stale after a press that
     focused nothing, and missed a press on the focused row.

     The window's return was measured in #s460, with a page holding this rule in two real browsers,
     each switched to another app and back with `open -a` (the activation Cmd-Tab gives). Chrome 153
     fires `focusout` on the row as the window goes, leaving the row the active element, and `focusin`
     on it as the window comes back. It does so for another app, a tab and a second window. WebKit
     (Playwright's macOS build 2359; safaridriver could not reach Safari 27) fires only the window's
     own `blur` and `focus`, for another app and for a second window. Read again at Chrome's return, a
     press dragged off a row and then any key made the press's focus the keyboard's, which the next
     rewrite carried. So a `focusin` on the row a `focusout` left focused keeps whose the focus was. A
     blur inside the page leaves the body the active element in both browsers, so it is read as before.
     A modifier alone still does not count as the last key: it starts a chord (Cmd-Tab's Meta reaches
     the page) and moves no focus.
   - **The strip.** `div.session-list-controls`, `display: contents`, so each control is still an item
     of the head's one row; the rule that hides the three while the column shows a pane now names the
     strip. The cases that read the head's children read through it. The rule is the list's and the
     app's stylesheets', which reach the strip in the list's element and in the app; a host placing it
     anywhere else styles it, as it styles the controls it holds (stage 3's rule for every placed part).
   - **Compact.** The app says `compact(on)` from `applySidebar`, the one place it sets the collapsed
     rail, so an open flyout now closes whenever the rail goes, a phone's breakpoint crossed included,
     where it used to close only as the reader opened the sidebar.
   - **The unmount.** One abort signal on the ten listeners the list binds on the page (the section
     menu's three, bound when it first opens, the filter's three, the flyout's three and the last key's); its timers
     through names it keeps; the index's polls stopped (its store's generation drops a read still out);
     a late reply to another app's list, the apps it could follow and a hide fenced; the strip, the
     sheet (from the flyout too), the rail's parts, the flyout, the section menu, its scrim and the
     safe-area probe removed wherever the host put them; the element emptied and its attributes put
     back as they were read before the mount; and the handle fenced as the view's. A non-empty element
     is refused, so a second mount never lays a list over another. Expand all is made beside Collapse
     all at the mount's start, before anything that draws the tree reads it. A host that unmounts the
     list inside its own `onIndex` (a host that closes the list as its selection goes, say) comes back
     into the list's code, which then drew the tree, told the host and asked for another app's list
     again: the redraw and that read now do nothing once the list is unmounted. That covers `onIndex` and
     `onOpen`, the events; `projectTree` and `projectActions` are asked while the list draws, and a host
     does not unmount it from inside them.
   - **Checks.** The contract: the exports, the guards run over fakes, the `show` reads, the engaged
     binding run over a fake element, every page listener carrying the signal, the unmount's steps and
     the fence run, the glyph's name and title worked out for each set of controls. The browser:
     `the_session_list_in_a_host_switches_its_controls_off` (a hide kept with hiding off, Include
     hidden ticked or not; arranged like the Claude app, the hidden session dimmed with every control
     shown and not with hiding off; the reader's stored filter and arrangement, the Claude app's
     included, kept and not applied, through the app's own `persist`),
     `…_takes_its_keys_only_on_its_rows` (and not on a project's row or past the last session; the
     focus kept through the host's rewrite after a step and after Tab, a press that focused nothing
     before it, and not after a press dragged off a row, focused before it or not), `…_unmounts_and_mounts_again` (listeners by the
     browser's own count, the element's own included, four replies held across the unmount, the
     handle fenced, a second mount, an unmount inside `onIndex`, and a host that placed every part
     and layer outside the element) and `the_app_shell_keeps_its_transcript_keys_after_a_session_is_
     clicked` (another session clicked, the open one clicked again, and the list's keys through the
     app's keymap, there and back). Each late-reply fence, the index's stop, the focus carried by the
     rewrite, the press's mark, the click's hand-back, the arranged dim and the callback fences were checked red
     when taken out.

To check once stage 7 is on main: `session-index-store.js` imports `indexState`; the rail's write
button is bound in `native-mode.js` through `page` and stays with the app and native mode, though
`take()` touches that module; and the contract blocks this step moves sit beside stage 7's new ones
(its `write` and `frame` options).

Left for step 6: compact rows when the pane is narrow, and the rail chosen by the pane's width.
