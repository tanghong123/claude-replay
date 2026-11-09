# The session view as a component

> **DRAFT 1** (#s429, 2026-11-08). Step 4 of `design/embedding.md` ("The order of the work").
> Stages 1 and 2 are built and shipped in v1.496.0. Stage 3 is built.

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
   looks.
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
5. **Styles apart.** The view's rules are scoped to its root, and a static test checks that none
   reaches outside it. The app's chrome rules are kept apart from the view's.
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
