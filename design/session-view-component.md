# The session view as a component

> **DRAFT 1** (#s429, 2026-11-08). Step 4 of `design/embedding.md` ("The order of the work").
> Stages 1 and 2 are built and shipped in v1.496.0, stage 3 in v1.497.0. Stage 4 and stage 5 (5a and
> 5b) are built.

## What it is for

A host such as the collaboration app or the native app (#s407) mounts one session: the transcript
with its turn bar and search, the outline panes, the artifact pane and the compose box. Today they
exist only inside the app shell's one page. This step makes them one component with one entry
point, and rebuilds the app shell to mount it the same way. The app's browser suite runs against
the app at every stage, and it stays green. That proves nothing changed for the app.

What a host may change is the owner's decision (#s436): light or dark, which optional controls
show, the events it gets, and its keys kept to the component. No colour settings for now. The
native app alone frames an agent's page from a source of its own. This step builds those as
options the app shell uses, and the kit (step 7) publishes them.

## What the session view is today

- **Markup:** three children of `#app` in the generated reference shell:
  - `main.workspace`, with the top bar, the outline, the transcript and the compose box;
  - the resizer;
  - `aside.preview`, the artifact pane.

  Their siblings belong to the app: the session list (`aside.sidebar`), the session search layer,
  the confirm dialog and the toast.
- **Script:** `app.js`, about 4,900 lines, runs the list and the view together, over shared state
  in `state.js`. The parts below it are already modules of their own: the record store, the
  viewport, the artifact pane, the compose box, the view model, the cards, the dialog card and the
  mdrev pane.
- **Styles:**
  - `reference.css`, generated from the demo, and `production.css` are written for the whole page,
    with rules like `#app.composing …`, `#app:has(…)`, and media queries on the window.
  - The view keeps its own state as classes on `#app`: `composing`, `pane-open`, `keyboard-up`,
    `wrap-code`, `wide` and `resizing`. The app's state sits beside them: `mobile-detail` (the
    drawer), `sidebar-off`, `sidebar-tight`, `sessions-flat`, `sessions-arranged`,
    `phone-column-off` and `column-pane-on`. Some of those are the view's on a wide phone held
    sideways, and stage 2 sorts them out case by case.
- **Lookups:** the view finds its elements by id in the document (`byId`, and about forty
  `document.querySelector` calls across the modules).
- **What it knows about its backend:** since step 1, mostly from its handle. A few facts still come
  from the page: mdrev's version (`data-mdrev`), the build's version, and Codex native mode's mark
  on `body`.
- **The theme** is the page's too: `:root[data-theme="dark"]`, read by the view, the mdrev pane and
  the image page. Such a rule does not reach inside a shadow tree, and Safari has no
  `:host-context()`. So the view's root carries the theme itself, set from its options.

## Decisions

### One entry point

`mountSessionView(element, options)` draws the view inside `element` and returns a handle:
- open a session;
- close it;
- tell the view its pane changed size;
- unmount.

Its options name the backend handle, and the handlers for what the view tells its host:
- its title;
- its current turn;
- a request to open the session's Info;
- a file opened;
- a link copied;
- a toast.

The rest of the options are light or dark and the optional controls to switch off (#s436).

### The shadow root, and how the app reaches it

Keeping a host's CSS out of the view takes a shadow root. Without one, a host's reset or utility
framework reaches every element the view draws. So the view draws inside an open shadow root
attached to `element`, and adopts its styles there. mdrev's stylesheet is a file scoped under
`.mdrev-host` that a host links (`vendor/mdrev/release/docs/embedding-guide.md`), so the view puts
that link inside its shadow root. Mermaid writes its `<style>` into its own SVG, which then applies
where the SVG is.

The app shell mounts the view the same way, in the end. Otherwise the whole browser suite and the
iOS Simulator walk would never cover the one mode a host uses: events retargeted at the shadow
root, `document.activeElement`, the keys' engagement, mdrev's guest, and Safari's own handling of
shadow trees.

But the suite reads the page through `document` about 3,500 times, and a shadow root hides what is
inside it from those reads. The cases' text must stay as it is: an unchanged suite passing against
the app is what proves the refactor kept everything. So the harness does the reaching:
- A script it installs before the page loads, as it installs `track_idle` today, makes the
  document's lookups (`getElementById`, `querySelector`, `querySelectorAll`) search open shadow
  roots too, when the document has no match.
- Its own CDP lookups (`find_element`, 37 places) go through the same search.
- The app's chrome stays in the page, so `document.body` and `document.documentElement`, read about
  200 times, mean what they mean today.

Until that is built, the entry point takes `isolate: false` and draws into the page. This is a
staging step only. The last stage switches the app shell to the shadow root, so that the last
landing's suite and Simulator walk cover a host's path.
### The grid stays the app's

In the app, the view's root element is `display: contents`. Its three parts stay columns of
`#app`'s grid, and nothing in the generated `reference.css` changes. In a host the root is a box of
its own, and the view lays its parts out inside it. Step 6 makes that layout follow the pane's
size rather than the window's.

### The phone top bar

On a phone, the top bar's title, turn and Info belong to the app (`design/embedding.md`, "The
standalone app becomes a host"). In this step the view keeps drawing its top bar, and the events
are added beside it. The app's own phone bar comes with step 6, which decides by pane size which
bar a phone shows, and is walked on the Simulator then.

On a phone the dock and its sheets are fixed to the window. In a host's pane they would escape the
pane, so step 6 also makes the view's root what they are placed against.

## The order

1. **Lookups from a root.** Every module of the view finds its elements from a root it is given,
   the document for now. No `document.` lookup is left in the view's code, and a static test holds
   it, as `no_page_writes_a_root_path` holds the page's links. The page's backend facts (mdrev's
   version, native mode's mark) come from the handle, and the theme from the view's own root.
2. **The view's state on its own root.** The classes the view sets for itself move from `#app` and
   `body` to the view's root element, and the CSS rules that read them move with them. The few
   browser cases that read those classes change in the same commit. Each assertion stays as it is,
   reading another element. This is the one place the suite's text changes, and only in where it
   looks. Codex native mode's own stylesheet and script were missed: they read `composing` and
   `keyboard-up` on `#app` until #s442, and a static test (`ui.rs`,
   `native_mode_reads_the_view_s_states_on_the_view`) now holds native mode to the view's element.
3. **The split of `app.js`.** The view's code moves to `session-view.js`, behind the entry point.
   `app.js` keeps what belongs to the app: the list, the drawer, pairing, the theme, the session
   search and placing the two. It calls `mountSessionView(…, {isolate: false})`, and shared state
   is passed in, not imported.

   **Built** (#s429):
   - `session-view.js` holds the transcript and its viewport, the turn bar and search, the outline
     panes, the artifact pane, the compose box and its dialogs, and the phone's dock and sheets.
   - `app.js` holds the session list with its filter and arrangements, the drawer and a wide
     phone's column, the rail, the brand, the theme, the session search layer, the toast, and the
     mount. The view asks the app for its selection, rows and workers through options and raises
     its events there; the app calls the handle to select, open and close a session and to say the
     view was resized.

   The function's body is the view's old top-level code moved line for line, so it sits at the
   margin. A whitespace-only commit can indent it once stages 4 to 6 stop rewriting it.

   Not yet passed in: `state.js`'s stores. The view still imports `persist` (which writes the
   app's settings with its own), `uiState` (which also holds the session search's results) and
   `controlState`; `control-store.js` reads `selectedRow()` and native mode `indexState.selected`
   from there. `state.js` says which side owns each field. Each moves behind an option when the
   kit's host needs it, at stage 7 at the latest. The names the two sides call each other by are
   held by a contract check (`#s429 mount interface`), since most options are optional callbacks
   a rename would silence.
4. **The view's markup from its own template.** The entry point builds its three parts inside the
   element, so a host page carries no markup of ours. The extraction script cuts the template from
   the demo, so the generated files stay exact.

   **Built** (#s429): the view builds its markup from `session-view-markup.js`, and the page serves
   `app-shell.html`, whose `#sessionView` is empty.
5. **Styles apart.** The view's rules are scoped to its root, and a static test checks that none
   reaches outside it. The app's chrome rules are kept apart from the view's.

   **5a, built** (#s429): the view's rules in `production.css` hang on its element. Nothing a
   reader sees changed. A throwaway probe compared every element's computed style (every
   longhand, the custom properties, `::before`, `::after` and `::placeholder`) under the old
   stylesheet and the new one, in the same page, and found no difference. It walked 58 states:
   desktops at 1440, 1100 and 820px, light and dark, with the pane open or shut, the compose box,
   the search and its filter, Reading's menu, wrap and wide; a phone upright with the drawer, each
   pane's sheet, the reply box, the search, Info and Aa; a phone held sideways; and a wide phone's
   column with each pane borrowed into it. Those states exercised 200 of the 272 rewritten
   selectors. The rest style what a hermetic page cannot show (a herdr dialog or images in the
   compose box, a live pane, native mode, a slow link's placeholders, Codex's quoted replies) or
   only a touch's hover; each of those elements is drawn inside the view.
   - The rules that hung on a plain `#app ` descendant and style only what the view draws now hang
     on `#sessionView ` (249 selectors, id for id, so no specificity moved). That includes the ones
     behind native mode's mark on `body`, and one that read the view's own state through
     `#app:has(…)`: the jump to the latest stepping aside while the dock's find pill is open.
   - What a wide phone's column borrows (`PHONE_PANES`: the Turns, Tasks and Agents lists, moved
     into the app's `#columnPane`), and what the app's chrome shares with the view (`.iconbtn`,
     `button`, the session filter's `.navigator-options` rows), hang on `:is(#sessionView,#app)`
     (23 selectors). In the app both match, in a host only the first.
   - What the view appends to `viewRoot.layer()` lands in `body`, outside `#app` and `#sessionView`
     alike: the task card and its scrim, the copy field, a download's link. No `#app` rule ever
     reached them, and none does now. The app's own rail flyout and native mode's menu sit there
     too.
   - A static test, `ui.rs` `the_view_s_styles_hang_on_its_own_element`, fails on any selector
     that names `#app`, `.app`, `body`, `html` or `:root` and is not in one of its lists.

   **Left for later**, each listed in that test:
   - **The app placing or styling the view from outside** (stage 6 lays the view out by its pane's
     size instead): the rules conditioned on an app state on `#app`. Those are `sidebar-off`,
     `sidebar-tight`, `mobile-detail`, `phone-column-off`, `column-pane-on`, `sessions-flat` and
     `sessions-arranged`, and the pane's `preview-off` and `preview-full`, copied onto `#app` for
     its grid. Also the app reading the view's state for its own control: the drawer handle steps
     under the dock while a dock sheet is open (`#app:has(.reading-cluster>…)>.drawer-handle`).
   - **The view's tokens on the app's grid:** `#app{--code-size;--measure}`, which `applyReading`
     writes on `grid`, and the phone's `#app{--phone-dock-h;--phone-glass}`. They move to the
     view's root with the reading options (stage 7) and the pane-sized layout (stage 6).
   - **The theme and the page's tokens:** `:root`, `html` and `[data-theme]` rules, the view's
     included (`:root[data-theme="dark"] .fence`). In a shadow root the view's root carries the
     theme (stage 6).
   - **Native mode's mark** on `body` (`body[data-shared-codex]`), which `backend.js` reads. The
     rest of each such selector hangs on `#sessionView`.
   - **The app's own chrome rules** stay on `#app`: the sidebar, the tree, the drawer, the rail,
     the arrangement strip and the section menu.

   **5b, built** (#s429): the session view has ONE stylesheet, `session-view.css`, and every rule
   in it hangs on the view's element. Nothing in it styles the page around the view: no `:root`,
   `html`, `body`, `*`, bare element or unscoped class. The app shell loads `app-shell.css`, its own
   chrome and the page's, and then `session-view.css`. The owner chose prefixing over a shadow root
   for the collaboration app (#s438), as mdrev's styles hang under `.mdrev-host`.
   - **Both files are generated.** `scripts/extract-agent-monitor-demo.mjs` derives them from
     `reference.css` and `production.css` through `scripts/session-view-css.mjs`, and never edits
     those two, which stay the sources: a rule is still written in `production.css`, and the script
     run after. The frontend contract derives both again and requires a byte match, as it does for
     the view's markup. Comments are not carried; the sources keep them.
   - **How a selector is written in the view's file.** A plain selector gets the prefix
     `:where(#sessionView) `, which weighs nothing. One that may match the view's element itself
     (it leads with `*` or a class the element carries: `.preview-off`, `.composing`…) is also
     written on the element. Stage 5a's rules keep their `#sessionView`, and its shared hook
     `:is(#sessionView,#app)` becomes `#sessionView`, an id as before. Where a source named the
     page's root, a stand-in on the view's element weighs the same: `.session-view` (the element's
     own class, which `mountSessionView` adds) for `:root`, `:is(*,div)` for `html` or `body`, and
     the conditions they carried move onto the element, which carries both: the theme
     (`[data-theme="dark"]`) and native mode's mark (`[data-shared-codex]`). So every rule keeps its
     weight exactly; the script checks each selector it writes against the one it read. A rule on
     the page's root becomes a rule on the view's element that carries only what the view inherits
     from the page: the tokens (56 custom properties) and `color`, `font`, `font-size`,
     `letter-spacing`, `color-scheme` and `text-size-adjust`. Not `--sidebar` or `--preview`, which
     the app's grid declares or a script writes on the page, and which the view inherits.
   - **Whose a rule is.** What `app-shell.html`, `app.js` and `session-list.js` draw is the app's;
     every other class or id is the view's. The classifier keeps the app's names in lists, each
     commented, and the contract checks the lists against what the app draws, both ways:
     - `APP_CHROME`: the session list and its tree, the rail and its flyout, the arrangement strip
       and the section menu, the session filter, the brand, the drawer, a wide phone's column, the
       session search layer (⌘K), the confirm layer and the toast. A rule naming one is the app's.
     - `APP_STATES`: the app's states on `#app` (stage 5a's groups). A rule reading one is the
       app's; where what it styles is the view's, it reaches into the view from outside (34
       selectors: the rail, the pane's state and a wide phone's column placing the view).
     - `SHARED`: what both draw (`.iconbtn`, `.label`, `.on`, the popover rows the session filter
       and the section menu borrow). A rule naming only these, or no name (`button`, `*`), is in
       both files.
     - `ROOT_CLASSES`: the classes the view's element carries.
     - `OUTSIDE`: what the view draws that the page shows outside `#sessionView`, so the app's copy
       of its rule styles it where it is: the task card and its scrim, which `viewRoot.layer()`
       appends to `body`, and the Turns, Tasks and Agents lists with their rows, which a wide
       phone's column borrows into the app's `#columnPane`. Read off the page in those states.
     - Everything else is the view's alone.
   - **The cascade.** The app's file loads first. A rule in both files is the same rule twice,
     its later copy in the view's file, so for an element of the view the view's file alone
     decides, in the order and at the weight the sources gave. An app rule that reaches into the
     view now comes before every rule of the view's; the script lists any that weighs the same as a
     view rule it used to follow, on a shared name and property, and the contract requires none.
   - **The theme on the view's element.** `#sessionView` carries `data-theme`, `light` or `dark`.
     `mountSessionView` takes `theme` (#s436) and its handle has `setTheme`; the app passes its own
     theme and calls `setTheme` when its toggle flips, and still marks the page's root for its own
     chrome. `viewRoot.theme()` reads the view's element (a page with no view, mdrev's own tab,
     reads its root), mdrev's pane remounts when the view's element changes theme, and Mermaid's
     diagrams follow the view's element (`useMermaidTheme`; the offline page keeps the root).
   - **Native mode's mark** is set on the view's element as well as on `body`, which the app, the
     compose box and native mode's own stylesheet read.
   - **The reset**, first in the file: the page's own rules come along by the derivation
     (`box-sizing` on the view's element and all within it, `button,input,textarea{font:inherit;
     color:inherit}`, `button{border:0}`), and it adds a `select` beside them and a `code` as the
     browser draws it (`monospace`, no border or background). Each value is what the app's page
     already computes. A button's background is not pinned: five of the view's buttons keep the
     browser's own (the top bar's Tasks and Agents, a phone pane's close, the dock's ✎ and its
     search's ✕; measured), so pinning one would change them.
   - **Keyframes** are global by name, as they always are: the view's file carries `pulse`, `pop`,
     `source-flash`, `primary-pulse`, `fleet-pulse`, `session-loading-in` and
     `session-loading-spin`; the app's carries `pop`, `status-breathe` and `session-loading-spin`.
   - The split: `session-view.css` 2,148 rules (2,419 selectors), `app-shell.css` 609 rules (679
     selectors), from 1,458 and 1,057 in the sources. By selector of the sources: 1,882 the view's
     alone, 270 the view's already on `#sessionView`, 162 the view's shown outside it, 42 with no
     name of the view's own and 23 through the shared hook (in both files), 36 on the page's root,
     381 the app's, 34 an app state on the view and one the app placing the view.
   - A static test, `ui.rs` `the_view_s_stylesheet_names_nothing_outside_its_element`, fails on any
     selector of `session-view.css` that does not start at the view's element or names the page or
     the app.
   - **Nothing a reader sees changed.** A throwaway probe served the same hermetic sessions from
     the build before this stage and the build after it, and compared every element's computed
     style (every longhand, every custom property, `::before`, `::after` and `::placeholder`) in
     each state: the old build, the new, and the old again for the method's own noise, matched by
     each element's path. It walked stage 5a's 56 states; the theme flipped after load, each way,
     at every width (`setTheme`); Mermaid's diagram and mdrev's pane in the pane, each across a
     flip; and the app's own states and layers: the rail and its flyout, a narrow sidebar
     (`sidebar-tight`), the flat list, ⌘K, the session filter, the confirm layer and the task card
     on a desktop, a phone and a wide phone's column. In 86 states, 153,683 elements compared:
     no difference. The method's noise was 12 values in two states, mdrev's toolbar measuring its
     relative time ("1 min ago") a pixel or two differently between loads; the new build matched
     one of the old loads there. Seven loads were made again because the transcript's virtual
     window had mounted a unit more or fewer. The theme followed every flip in both builds:
     Mermaid's fill and mdrev's own theme match, old and new. A positive control, the top bar's
     base rule broken in the view's file (`:where(#sessionView) .topbar` made a child of the
     element, which the top bar is not), showed in every state it ran: 51 values on a desktop (the
     bar's display, border and alignment, and what moved inside it), about 6,000 on a phone. The
     probe cannot draw native mode
     (its rules depend on the mark the view's element now carries too) or the app's arranged list.

   **Left for later:**
   - The app placing the view from outside (the 34 selectors that reach in), the window's media
     queries, and the tokens on the app's grid (`--code-size`, `--measure`, `--phone-dock-h`,
     `--phone-glass`) with `--sidebar` and `--preview`: stage 6 lays the view out by its pane's
     size, and stage 7 moves the reading options onto the view.
   - What the view draws outside its element is styled by the app's copy of its rules: in a host,
     the task card and its scrim need a layer under the view's element (stage 6).
6. **The shadow root:**
   - the open shadow root, with the view's styles adopted into it and mdrev's linked there;
   - keys and focus followed through the event's composed path;
   - the harness's lookup into open shadow roots;
   - the hostile test host page (`design/embedding.md`, the isolation test).
7. **Events out and the options,** as the app shell uses them: the title, the turn, Info, a file, a
   link and a toast; light or dark; the optional controls switched off. Also the native app's own
   frame for an agent's page, which is not offered to other hosts (#s436).
8. **The app shell in the shadow root.** The app passes `isolate: true` like any host, and the whole
   suite and the Simulator walk run against it that way. Then the `isolate` option goes.

Every stage is page code. So before each landing, the whole local browser suite runs, about 85
minutes, and the iOS Simulator walk is done. To keep that to three runs, the stages land in three
groups: 1 and 2, then 3 and 4, then 5 to 8.
