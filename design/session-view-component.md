# The session view as a component

> **DRAFT 1** (#s429, 2026-11-08). Step 4 of `design/embedding.md` ("The order of the work").
> Stages 1 and 2 are built and shipped in v1.496.0, stage 3 in v1.497.0, stage 4 in v1.498.0, 5a in
> v1.499.0, 5b in v1.500.0, the class renames in v1.502.0, and stage 6 in v1.504.0. Stage 7 is left.
> There is no shadow root (the owner, #s438).

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
  the image page. A host's page root is not the view's to theme, and a host may theme its own
  differently. So the view's root carries the theme itself, set from its options.

## Decisions

### One entry point

`mountSessionView(element, options)` draws the view inside `element` and returns a handle:
- open a session;
- close it;
- tell the view its pane changed size;
- unmount.

Its options name the backend handle, and the handlers for what the view tells its host:
- its title (`onTitle`);
- its current turn (`onTurn`);
- a request to open the session's Info (`onInfo`);
- a file opened (`onFile`);
- a link copied (`onLinkCopied`);
- a toast (`onToast`).

The rest of the options are light or dark (`theme`), the optional controls to switch off (`show`)
and the keys kept to the view (`keyboard`, engaged by default) (#s436). Stage 6 below says what each
is.

### No shadow root: the view's styles keep to its element

This draft first planned a shadow root, to keep a host's CSS out of the view. The owner chose
otherwise for the collaboration app (#s438), as mdrev does under `.mdrev-host`: the view draws into
the host's page, and each side keeps its styles to itself.

- **Ours stay inside the view.** Every rule of the view's stylesheet hangs on its own element
  (stage 5b), so nothing of ours reaches the host's page.
- **The host keeps blanket rules out of its page.** A rule on `button`, `*` or `body` would reach
  the view's elements. The collaboration app moved its element rules into a layer that skips the
  embed, and its agent rules forbid new unscoped ones (collab #s801).
- **No class of ours shares a name with one the host styles.** A host's `.primary{…}` would
  otherwise style our `.primary`. So the view's classes the collaboration app styles are named
  `sv-<name>` (stage 5b), and a test keeps the bare names out. mdrev's own classes inside the pane
  are mdrev's; the collaboration app removed the one rule of its own that reached them (#s444).
- **A short reset** under the view's element sets what a host's rules could still change by
  inheritance or a browser default (box sizing, a button's font).

So the app shell and a host mount the view the same way: into the page. The suite and the iOS
Simulator walk already cover that path, and the harness needs no lookup into a shadow tree. The
`isolate` option, which only ever took `false`, goes with stage 6.

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
   search and placing the two. It calls `mountSessionView(…, {isolate: false})` (the option went
   with stage 6), and shared state is passed in, not imported.

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
   - **The app placing or styling the view from outside** (step 6 of `design/embedding.md` lays the
     view out by its pane's size instead): the rules conditioned on an app state on `#app`. Those are `sidebar-off`,
     `sidebar-tight`, `mobile-detail`, `phone-column-off`, `column-pane-on`, `sessions-flat` and
     `sessions-arranged`, and the pane's `preview-off` and `preview-full`, copied onto `#app` for
     its grid. Also the app reading the view's state for its own control: the drawer handle steps
     under the dock while a dock sheet is open (`#app:has(.reading-cluster>…)>.drawer-handle`).
   - **The view's tokens on the app's grid:** `#app{--code-size;--measure}`, which `applyReading`
     writes on `grid`, and the phone's `#app{--phone-dock-h;--phone-glass}`. They move to the
     view's root with the reading options (stage 6) and the pane-sized layout (embedding's step 6).
   - **The theme and the page's tokens:** `:root`, `html` and `[data-theme]` rules, the view's
     included (`:root[data-theme="dark"] .fence`). 5b gave the view's element its own theme and
     tokens.
   - **Native mode's mark** on `body` (`body[data-shared-codex]`), which `backend.js` reads. The
     rest of each such selector hangs on `#sessionView`. 5b sets the mark on the view's element too.
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
     - `COMMON`: common words the app draws as modifiers (`.dim`, `.warn`, `.hint`, `.kbd`,
       `.dialog`, the list's `is-*` marks…), which decide nothing alone, as `SHARED` does: beside a
       chrome name the rule is the app's (`.navbtn .hint`), beside a name of the view's it is the
       view's (`.process-row.warn`, `.transcript .kbd`), alone it is in both files.
     - A name inside `:not()` decides nothing (`.turn:not(.dim)` is the view's).
     - `ROOT_CLASSES`: the classes the view's element carries.
     - `OUTSIDE`: what the view draws that the page shows outside `#sessionView`, so the app's copy
       of its rule styles it where it is: the task card and its scrim, which `viewRoot.layer()`
       appends to `body`, and the Turns, Tasks and Agents lists with their rows, which a wide
       phone's column borrows into the app's `#columnPane`. Read off the page in those states.
     - Everything else is the view's alone.
   - **What cannot hang under the view stops the build**, none of it in the sources today: a
     selector going on from the view's element to its siblings (`.pane-open+.x`); one led by a
     condition the page around the view meets (`[data-shared-codex] .composer`, `:not(.x) .y`,
     `:focus-within .y`), which under the view's element would quietly match less or nothing; and
     alternatives naming the app's chrome and the view's elements together
     (`:is(.sidebar,.transcript) .label`). An attribute the view's own elements carry may lead
     (`[data-nav-card="tasks"] .x`).
   - **The cascade.** The app's file loads first. A rule in both files is the same rule twice,
     its later copy in the view's file, so for an element of the view the view's file alone
     decides, in the order and at the weight the sources gave. An app rule that reaches into the
     view, or places it, now comes before every rule of the view's; the script lists any that
     weighs the same as a view rule it used to follow, on what could be one element (a shared name
     outside `:not()`, or a subject naming none) and a property they share, a shorthand meeting its
     longhands (`padding` and `padding-left`). The contract requires none.
   - **The theme on the view's element.** `#sessionView` carries `data-theme`, `light` or `dark`.
     `mountSessionView` takes `theme` (#s436) and its handle has `setTheme`; the app passes its own
     theme, still marks the page's root for its own chrome, and calls `setTheme` whenever the root's
     `data-theme` changes, by its toggle or by a script. `viewRoot.theme()` reads the view's element (a page with no view, mdrev's own tab,
     reads its root), mdrev's pane remounts when the view's element changes theme, and Mermaid's
     diagrams follow the view's element and take their colours from its tokens (`useMermaidTheme`;
     the offline page keeps the root). `mountSessionView` gives its element the id `sessionView`
     the stylesheet hangs on, and refuses one that carries another.
   - **Native mode's mark** is set on the view's element as well as on `body`, which the app, the
     compose box and native mode's own stylesheet read. Native mode's stylesheet and controls read
     the view's own states (`composing`, `keyboard-up`) on `#sessionView`, where stage 2 moved them
     (#s442: from v1.496.0 they read `#app`, where the states no longer were).
   - **The reset**, first in the file: the page's own rules come along by the derivation
     (`box-sizing` on the view's element and all within it, `button,input,textarea{font:inherit;
     color:inherit}`, `button{border:0}`), and it adds a `select` beside them and a `code` as the
     browser draws it (`monospace`, no border or background). Each value is what the app's page
     already computes. A button's background is not pinned: five of the view's buttons keep the
     browser's own (the top bar's Tasks and Agents, a phone pane's close, the dock's ✎ and its
     search's ✕; measured), so pinning one would change them.
   - **Keyframes** are global by name, so the view's take a prefix: its file carries `sv-pulse`,
     `sv-pop`, `sv-source-flash`, `sv-primary-pulse`, `sv-fleet-pulse`, `sv-session-loading-in` and
     `sv-session-loading-spin`, and every animation there names them so; a host's keyframes of the
     same name can neither replace them nor be replaced. The app's file keeps its own names, `pop`
     and `status-breathe`. A keyframe no animation names stops the build. No script names one.
   - The split: `session-view.css` 2,152 rules (2,423 selectors), `app-shell.css` 609 rules (679
     selectors), from 1,458 and 1,057 in the sources. By selector of the sources: 1,882 the view's
     alone, 270 the view's already on `#sessionView`, 162 the view's shown outside it, 46 with no
     name of the view's own and 23 through the shared hook (in both files), 36 on the page's root,
     377 the app's, 34 an app state on the view and one the app placing the view.
   - A static test, `ui.rs` `the_view_s_stylesheet_names_nothing_outside_its_element`, fails on any
     selector of `session-view.css` that does not start at the view's element, goes on from it to its
     siblings, or names the page or the app. The contract also scans the other way: the view's
     modules and its markup draw no name of the app's chrome or states.
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
     Run again after the review's fixes (the keyframes' prefix, the classifier, Mermaid's colours,
     the id, the theme followed from the root): in 86 states, 153,701 elements compared, the one
     difference is `animation-name` taking the prefix, on the loading veil (`session-loading-in`
     to `sv-session-loading-in`) and its spinner (`session-loading-spin` to
     `sv-session-loading-spin`), in every state; the same keyframes run. The noise was mdrev's
     relative time again, 6 values in one state.

   **The names a host styles, built** (#s429, after 5b). Our rules cannot reach the host's page,
   but the host's class rules still reach ours by name: the collaboration app styles six classes
   the view used, so the view's are renamed `sv-<name>`, the prefix its keyframes took (#s438).
   - `.session-main`, the workspace's middle column, is `.sv-session-main`.
   - `.task-card` and its whole family (`-head`, `-glyph`, `-id`, `-title`, `-chips`, `-dates`,
     `-gap`, `-section`, `-label`, `-body`, `-item`, `-out`, `-log`, `-log-time`, `-log-msg`,
     `-log-by`) are `.sv-task-card…`: the names `APP_TASK` hands the shared card. The classic
     page's `tcard-*` and the module's file name stay. `OUTSIDE` names `.sv-task-card`, so the
     app's copy still styles the card in the page's layer.
   - The pane's roster tab, `.preview-tab.pinned`, is `.preview-tab.sv-pinned`.
   - `.primary` on the view's buttons (a dialog's first choice and its Next, the pane's Download)
     is `.sv-primary`. The demo's `.smallbtn.primary` rules each gained `.smallbtn.sv-primary` in
     the same rule, so their place in the cascade holds. The app's confirm button keeps
     `.smallbtn.primary`: the classifier lists `.primary` in `APP_CHROME` now, not `SHARED`, so its
     rule is the app's alone, and its `.primary` rules in `app-shell.css` did not change (that
     file's task-card rules did, taking the new names). The split above moves by three:
     the three `.smallbtn.primary` selectors are the app's alone now (380, and 43 in both), and the
     three `.smallbtn.sv-primary` beside them the view's (1,885).
   - `.crumbs` was one dead rule of the demo's (`.artifact-path .crumbs`; nothing draws either
     class), now `.sv-crumbs`. `.hint` is drawn by the app alone (⌘K beside Search), and stays.
   - The guard: `ui.rs` `the_view_s_stylesheet_names_no_class_the_collaboration_app_styles` fails
     on any selector of `session-view.css` naming one of the six, or the task card's family, inside
     `:not()` too; the contract fails on the view's modules drawing one (its scan now reads
     `dialog-card.js`'s `el(tag, classes)` and `APP_TASK` as well).
   - Nothing a reader sees changed. Both generated stylesheets, with the renames mapped back, are
     byte for byte what 5b derived. Stage 5b's probe, the 5b build against this one, matched
     elements by path with each renamed class read under its old name (a fixed map, nothing else
     stripped). Its 86 states in 5b's world: 151,609 elements compared, no difference; one state
     whose loads mounted different units (mdrev's pane flipped to light) was made again and
     compared clean. A world that adds a published Artifact (the roster tab) and a delivered binary
     (the pane's Download), in 17 states with the pane open, the task card and the Download on a
     desktop, a phone and sideways: 24,714 elements, no difference. Noise, old against old: mdrev's
     relative time, the jump to the latest fading in one load, and the transcript's window in the
     roster opened at 1440px. A positive control, one rule of each family changed in the new
     build's stylesheets, showed on exactly those elements (the middle column, the card's title, the
     roster tab, the Download). The probe cannot draw a dialog (`.dialog-choice.sv-primary`, a herdr
     pane's), which the browser case taps by its new name.

   **Left for later:**
   - The app placing the view from outside (the 34 selectors that reach in), the window's media
     queries, and the tokens on the app's grid (`--code-size`, `--measure`, `--phone-dock-h`,
     `--phone-glass`) with `--sidebar` and `--preview`: step 6 of `design/embedding.md` lays the
     view out by its pane's size, and stage 6 here moves the reading options onto the view (done:
     `--code-size` and `--measure` are the view element's).
   - What the view draws outside its element is styled by the app's copy of its rules: in a host,
     the task card and its scrim need a layer under the view's element (embedding's step 6, with
     the test host page that first mounts the view alone).
   - **The class renames, built** (#s429, after 5b's review): see 5b above. Shipped in v1.502.0.
6. **Events out and the options,** as the app shell uses them: the title, the turn, Info, a file, a
   link and a toast; light or dark; the reading options (the wide transcript, wrapped code, the text
   size) on the view's element rather than the app's grid; and the optional controls switched off.
   The `isolate` option goes, since the view always draws into the page (#s438).

   **Built** (#s429). Nothing a reader of the app sees changed: the app passes every handler, keeps
   every control shown and keeps the page's keyboard. `mountSessionView`'s doc comment is the
   reference. An independent review then found defects for a host, fixed in the same stage; each is
   said below where it belongs.
   - **The options**, each named in `MOUNT_OPTIONS`; any other name is refused, so a retired `toast`
     or a typo fails loudly. Required: `grid` and the app's index and list callbacks. Optional:
     `theme`, `backend`, `show`, `keyboard`, the six events and the app's own (`parentShown`,
     `openList`, `openSidebar`, `paneLayout`, `paneCount`). The names `show` and `keyboard` are the
     coordinator's, so neither can be taken for the handle's `controls` or `keys`.
   - **Events out.** Each is an optional handler, called with plain data (never a node), where the
     view already does that thing, through one emitter (`say`). A handler that throws is reported
     (`reportError`) and the view goes on: a throw had aborted a redraw after the record store moved
     its cursor, and skipped a transaction's placement. What the view says while it mounts (its first
     title) is held and goes out a microtask after `mountSessionView` returns, in order, so a handler
     may use the handle. The view keeps drawing its own top bar, so it still shows its title, turn and
     Info itself.
     - `onTitle({ session, title })`: the title the top bar shows, said when it changes
       (`renderHeader`, which the index's poll calls every 5 s). With no session it is the bar's
       placeholder and `session` is "".
     - `onTurn({ session, turn, label })`: the turn the reader is in, said when it changes, and only
       from where the engine placed the reader: after a transaction and after a scroll, the task's
       last reading once (`queueTurn`). Not from `afterRender`, which runs before the placement and
       had reported a wrong turn that the transaction's end then corrected. `turn` is null when there
       is none, and for a session just opened until its own records arrive (`unitsSession`): the
       loading update had reported the previous session's turn under the new id. `open` and `close`
       say none at once. The app's own subtitle keeps its old timing.
     - `onInfo({ session })`: the reader pressed for the session's Info: the phone's Info button,
       the outline footer's, or the collapsed rail's, when the press opens it. A hover's peek is not
       a request. The view still opens its own popover; a host whose bar shows Info is step 6 of
       `design/embedding.md`.
     - `onFile({ name, path, text })`: the artifact pane was asked to show a file (`Preview.open`,
       and the first of an Open all, the one it shows). `name` is the file's own, not an Open all
       tab's label ("image.png 1/4"); `path` is "" for none, and `text` is the text the transcript
       carries for it, or null. The fields are copied off the pane's tab, which also holds the file's
       stamps and an image's bytes.
     - `onLinkCopied({ url, record })`: the reader copied a link to a record (a record's `#`).
     - `onToast(message)`: say something briefly. It was the required `toast` option, renamed into
       the family. A host that leaves it out hears nothing of what the view says this way, a send the
       compose box held or the server refused among it.
     The app passes all six. It shows the toasts; the other five do nothing, since today's page does
     nothing with them (it never set `document.title` from the session). The app's phone bar takes
     the title, the turn and Info at embedding's step 6.
   - **The reading options on the view's element.** `applyReading` writes `--code-size` and
     `--measure` on `#sessionView`, and their defaults are a rule on it
     (`#sessionView{--code-size:12px;--measure:820px}`, which moved from `app-shell.css` to
     `session-view.css`). Both `var()`s already hung on `#sessionView`. The wrap and wide classes
     were on the view's element since stage 2. So a host gets every reading option without the app's
     grid.
   - **What still needs `grid`:** the phone's sheets are measured against its box (`place`, which
     sets `--phone-top` and `--phone-bottom`), and it is the box moved back over iOS's page scroll
     while the keyboard is up (`ride`: its `transform`, and its height for native mode's
     `sharedKeyboardFrame`). Embedding's step 6 makes the view's own element that box. The phone's
     `--phone-dock-h` and `--phone-glass` are still declared on `#app`, for the same step.
   - **Optional controls**, one option, `show: { <name>: false }`. Each is shown unless it is
     `false`; a name the view does not know (`OPTIONAL_CONTROLS`), or a value that is not a boolean,
     is refused. A control switched off is not drawn and takes no key. Three are on the list:
     - `reading`, the Reading menu (Aa), with its keys `w`, `-` and `+`, at every width (on a phone
       it is the dock's Aa). Its settings are the reader's own and the defaults read well everywhere,
       so a host with little room (a side column) or one that keeps its chrome to the fewest controls
       can do without it. With it off, the reader's stored preferences (this origin's
       `am-prod-reading`) still apply as they are. The per-block code bars stay: they act on one
       block, not the page.
     - `history`, the viewport history's Save, a row of the Reading menu. It is this repository's bug
       report tool: a host's readers report to the host, and the file is named for this app.
     - `detach`, the artifact pane's ↗, which opens the document or image in a tab of its own. That
       tab is the monitor's own page (`/markdown`, `/image`), which a host serves only once the kit
       carries the detached views (embedding's "Detached views", a later kit). Off, the button is
       never put in the pane's head.

     Left off the list: the outline panes' filters. They decide what a pane lists and are on by
     default (running tasks and agents only), so a host switching off their control would strand a
     reader whose lists it hides (#s356); switching them off would have to mean showing everything,
     a second path through the outline's rendering for no host's need yet. Also left: the writes,
     which appear only when the backend serves them, and the title's copy menu.
   - **Keys**, the `keyboard` option (`KEYBOARD_MODES`).
     - `"engaged"`, the default for a host. The view binds its own keymap and answers it only while
       the reader works in it (`engaged`): the last `pointerdown`, `mousedown` or focus landed inside
       its element, or inside what it floats over the page from `viewRoot.layer()` (`floating`: the
       task card and its scrim), and not inside a guest of its own (mdrev's pane), whose keys stay
       mdrev's. That is mdrev's own rule, which `bindKeymap` already applies to a guest (#270).
       `pointerdown` is heard beside `mousedown` because a host that prevents the first stops the
       browser's second, which left the view engaged. Every one of these listeners, the keydown
       included, is in the capture phase, so a host's own listener on the page, bound before the
       view's, finds the key already taken (`defaultPrevented`) rather than acting on it beside the
       view. The binding is resolved by the shared keymap (`resolveKey`), so a key in a text field or
       with a platform modifier is never taken. A key the view has no action for (the list's arrows,
       `\`, a switched-off control's) or an action that answers `false` is left to the page. The
       artifact pane's image viewer, which takes `+ = - _ 0 1` and the arrows on the document,
       keeps to the same engagement: a zoomed image no longer took the arrows the host's page scrolls
       with. And it too is heard in the capture phase in a host (`keysFirst`, through the key host
       option `image-view.js` already had), since it binds when an image opens, after a host's own
       listener. The engagement is built in the view on `resolveKey`, not in `bindKeymap`: the offline
       pages inline the shared module, and the byte gate reads them.
     - `"page"`, the app's mode. The view binds none and is always engaged, so the image viewer
       answers as it did: the host owns the page's keyboard and calls the handle's `keys[action]`.
       A host that uses it gets the action names, not the key table (`shared/keymap.js`, which the
       app binds at the document with the list's keys beside the view's and their contexts, exactly
       as before). It now leaves a key it has no action for to the page too, which changes nothing in
       the app, where every action has one.
     - Not gated: the view's Escape listeners on the page. Each closes something of the view's that
       is open (a menu, the popovers, a phone sheet, the task card, whose capture-phase listener stops
       the Escape), so each acts only after the reader opened it in the view.
   - **`isolate` is gone**, with the throw at the top of `mountSessionView` and the app's
     `isolate: false`; and so is `view-root.js`'s `useViewRoot`, which nothing called, with its
     `root?.host` branches. Every lookup of `viewRoot` is the page's, as it was in practice.
   - **Mounted alone, the view met a fault the app never had.** The artifact pane shuts itself as it
     is built. In a page whose element does not start `preview-off` (the app's markup does), that
     changed the transcript's width, and `paneLayoutChanged` remeasured before the record store
     existed: the mount threw. The pane's layout changes now remeasure only once the view is built,
     from the width the mount left (the one read before the pane shut itself was stale); the reading
     options' first application, later in the mount, measures everything anyway.
   - **What a host's page must still carry**, which the test host found: the compose box's controls
     (`control-store.js`) bind the app's write-mode switch (`writeSwitch`, `writeBtn`, `writeInfo`,
     `writeHelp`, `sidebarMiniWrite`) and its confirm layer (`confirmLayer` with its `h3` and
     `.dialog`, `confirmText`, `cancelSend`, `confirmSend`) by id, so a page without them cannot
     mount the view. The doc comment names them and the contract checks each against
     `control-store.js`; the test host carries hidden stand-ins. That is stage 3's `controlState`,
     passed in by stage 7 at the latest.
   - **Tests.** The frontend contract's mount interface cases hold the names both ways:
     `MOUNT_OPTIONS` is exactly the options the view reads and the ones its doc comment documents,
     where documented means an option bullet's head names it (not any backticked word); the required
     ones are passed by the app, the app leaves out only `show`, and each of the six handlers is
     raised through `say` by its literal name and passed by the app. It runs the emitter (a throw
     reported, the mount's events held, then in order) and pins where the turn is said. A second
     block runs the engagement and the engaged keymap as the view writes them, in both modes, over a
     fake page: a press in the view, its task card, a guest and the host, focus in, a pointer going
     down in the host, every listener in the capture phase, and the page's keyboard binding nothing.
     Three browser cases:
     - `the_app_shell_hears_what_its_session_view_tells_its_host` (port 3102) answers the app's
       request for `session-view.js` with a wrapper that records each event on its way to the app's
       handler. It asserts the title when a session opens and no title said twice in a row across an
       index poll; a hover over Info saying nothing and a press saying it once; a file opened with its
       path and text; an Open all of three images as one `onFile` under the file's own name; a link
       copied; a toast the app then shows; the turn as `]` moves it, nothing across a scroll inside
       that turn, and no turn said twice in a row; all of it plain data.
     - The other two mount the view alone in a test host page,
       `claude-replay-browser-tests/tests/fixtures/host/session-view-host.html`, whose own keydown
       listener is bound before the mount and which can open a session from the monitor's list.
       `the_session_view_in_a_host_draws_no_control_switched_off_and_takes_none_of_its_keys` (3103):
       nothing switched off is drawn and the Reading keys are not taken while engaged, and at a
       phone's width the dock's Aa is there with reading on and gone with it off.
       `the_session_view_in_a_host_takes_its_keys_only_while_the_reader_works_in_it` (3104): with a
       session open, engaged and disengaged by a press and by focus, a key without an action left to
       the page, a zoomed image in the pane panned by ArrowDown while engaged and the key reaching the
       host once disengaged, and `"page"` binding none.

     The page and the wrapper are answered by the browser itself, through CDP's Fetch domain
     (`harness::answer_in_browser`), at a single-segment address on the monitor's origin, so the
     monitor serves nothing new and no reader can reach either.

   **Left for later:**
   - The app's own phone bar, which shows the title, the turn and Info from these events and decides
     by pane size which bar a phone shows; `grid`'s two remaining roles and the phone's tokens on
     `#app`; and a host-supplied link for a record (the view copies the page's own address with the
     record's hash, which in a host is the host's address). All are embedding's step 6.
   - **The unmount.** The handle has none yet. It must remove what the view adds to the document
     for its keys, the four capture listeners of engaged mode (`pointerdown`, `mousedown`, `focusin`,
     `keydown`), beside the view's other listeners on the window and the document (its Escape
     handlers, the pane's and the task card's outside-press closers, the viewport's), its floating
     layers in `viewRoot.layer()`, its observers and its timers.
   - The compose box's page elements above, passed in with `controlState` (stage 7).
7. **The native app's own frame** for an agent's page, which is not offered to other hosts (#s436).

The shadow root this draft first planned as stage 6, and the app shell moved into it as stage 8, are
dropped (#s438, above). The isolation test of `design/embedding.md` still runs, in the terms the
owner set: a test host page whose own CSS keeps to its side (class rules on common names, element
rules on its own containers, no blanket rules) and whose keys are its own. Nothing of ours reaches
the host, and the view looks as it does in the app. It comes with step 6 there, where the view is
first mounted alone in a test page for the width tests.

Every stage is page code. So before each landing, the whole local browser suite runs, about 85
minutes, and the iOS Simulator walk is done. Stages 1 and 2 landed together, then 3, then 4, then 5a,
5b and the renames, then 6 on its own, so that embedding's steps 5 and 6 need not wait for 7; 7 lands
last.
