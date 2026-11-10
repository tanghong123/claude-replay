# agent-monitor as an embeddable module and an app

> **DRAFT 2** (#s406, 2026-11-07). The owner's direction (#s408) replaced the first draft's
> options. Nothing is built yet. Its companion is `design/monitor-hub.md` (#s407, the native app
> over many monitors), which is one more host of the same components.

## What it is for

agent-monitor's value is split evenly between two ways of using it, as mdrev's is:

- **an embeddable module** that another web app mounts in its own page;
- **a standalone app**: today's monitor page.

The owner's words: *"We are not simply lending the capability we have today as-is or do a good
enough job. We will need to make changes to the UX such that it can be used for all kinds of
situations."* A team's collaboration app is the first host, but the module is built for any host.

To prove the module, the standalone app is rebuilt on top of it. The app becomes one host of the
same components, so every test of the app also tests the module.

## The pieces

There are three. The first is served and the other two are drawn.

### 1. The backend contract: what the components read

The components get all their data from a set of HTTP routes. The host says where those routes
live: a URL prefix of its own choosing, as mdrev's `contract` option works.

**Unlike mdrev's, this backend is heavy, and a host reuses it rather than writing it.** mdrev's
server side is a few routes over files, which a host can write in an afternoon. agent-monitor's
server does real work behind its routes:

- reading every agent's transcripts, and folding them into records;
- the record log, read lazily;
- the turn index;
- search on the server;
- file stamps;
- secret masking.

That work is already split into reusable pieces:

- the agent-free engine, with its seam for a new transcript format (`TranscriptAdapter`), and an
  adapter per agent;
- the html crate's session service: records, turns, search and files;
- the monitor's route table, built by one constructor (`routes::handler`).

So a host serves the contract in one of three ways:

- **Run agent-monitor's server where the sessions are.** The host relays the routes under its
  prefix and puts its own sign-in in front, as hosts of mdrev relay its routes. Where the agents
  run in the host's own machines, a monitor beside them answers for their transcripts.
- **Link the crates into its own Rust service.** It mounts the same route table under its prefix
  and gives it the sessions to serve. A transcript format we do not read yet gets an adapter
  through the engine's seam.
- **For a host whose data is already records:** answer the routes directly, with the conformance
  check to confirm.

The routes cover:

- **the session list**: the sessions to show, their state, project and agent;
- **the turns**: one session's turn index, the map that lets a long session open fast and jump
  anywhere;
- **the transcript**: one session's records, read lazily, the tail first and older blocks in
  ranges as the reader scrolls back (what the app does over slow links today);
- **files**: a file the transcript offers or hands over, behind the capability stamps the server
  mints;
- **images**: pictures carried in the transcript as base64, and the originals behind them.

Optional routes add **writes**: sending a prompt, answering a dialog, granting a pane. A host
declares which routes it serves, and that declaration is a ceiling: a host that serves no writes
gets no compose box and no answer buttons. That is mdrev's rule too.

The contract is written down: every route, its parameters, its reply, and the record format the
transcript uses, field by field. It carries a version. A change that only adds keeps the version.
A change that removes or reinterprets moves it, and the kit says which versions it reads. Today
this format is private between the monitor's server and its page; writing it down is what lets
another host serve it.

### 2. The session list: the left pane

The list of sessions with its filter and its arrangements: by agent and project, by activity, or
like the Claude app, the Codex app or Herdr. It reads the session list route. Choosing a session
tells the host, which opens it in a session view.

### 3. The session view: the right pane

One session: the transcript with its turn bar and search, the outline panes (Turns, Tasks,
Agents), the artifact pane where files and images open, and the compose box where the host serves
writes. **These stay together as one component.** The outline and artifact panes are not split out
for hosts to arrange. The view arranges them itself.

## Layout decides itself

Each component looks at two things and lays itself out from them:

- **the device**: a finger or a mouse, and whether it is a phone;
- **the size of the pane it is mounted in**: not the size of the window.

The session view, for example:

- **in a wide pane on a desktop:** the transcript, the outline panes and the artifact pane side by
  side, as the app shows them today;
- **in a narrower pane:** the artifact pane opens over the transcript, and the outline panes give
  way;
- **in a narrow pane (a host's side column, about 380px):** the transcript alone, with the outline
  and artifacts one tap away;
- **on a phone:** the phone reader the app has today, with the dock, the sheets and touch-sized
  targets. All of it is drawn inside the pane, never across the host's screen.

The session list does the same: full rows when there is room, compact rows when there is not.

Laying the two components out against each other is the host's job, since it owns the screen. On
a phone, the standalone app puts the list in a drawer over the view, as it does today. The
collaboration app puts them in its own columns.

Every rule that today asks the window's size asks the pane's instead. The phone rules that hang on
a finger stay with the device.

## Customizing

A host gets little to change, by the owner's choice (#s436): tuning a reading experience is hard,
so the components are designed to work well in every pane, on every device, rather than handing
their look to each host. What a host can do:

- **theme:** light or dark. No colour settings for now; they may come once the components have
  been used for a while;
- **which controls show:** each optional control can be switched off. The writes appear only when
  the host serves them;
- **events out:** opening a session, opening a file, copying a link and a toast go to the host's
  handlers. The session view also tells its host its title, its current turn and a request to
  open the session's Info, which a host's own top bar shows: the standalone app on a phone does
  exactly this, as a host like any other;
- **keys:** a component answers its keyboard shortcuts only while the reader is working in it,
  so they never take the host's keys.

Where the session view shows an agent's HTML page in a frame, it writes the frame from the page's
text, and such a frame takes the security policy of the page around it. The native app's policy
allows no scripts under it, so the native app frames the page from a source of its own. That is
an arrangement between the components and the native app, which are built together; it is not
offered to hosts.

Styles are kept apart by terms on both sides, with no shadow root (the owner, #s438). A component's
CSS cannot reach the host page: every rule of ours hangs on the component's own element. The host's
CSS stays out of the component on the host's side of the terms: it keeps blanket rules (`button`,
`*`, `body`) out of its page, and the component's classes that the host styles are named `sv-<name>`,
so a host's class rule cannot style ours by name (`design/session-view-component.md`, stage 5).

## Detached views

A session's transcript, or one artifact, can open in a tab of its own:

- **served by the host's own port:** at a route of the host's choosing;
- **rendered entirely by agent-monitor:** the kit carries a standalone page entry for this. The
  page reads what to show from its address, and its data from the host's contract, the same
  routes the embedded components use.

The standalone app already does this for Markdown and images (`/markdown`, `/image`). The kit
makes it available to any host, for the transcript as well.

## The standalone app becomes a host

The app shell is refactored to mount the session list and the session view. It keeps only what
belongs to an app:

- the brand and the version;
- pairing;
- the theme toggle;
- the drawer and the top bar on a phone;
- placing the two components side by side.

Its server keeps answering the contract it already answers. Its auth, pairing and the file gate
do not change.

So there is one session list and one session view, used by the app, by the collaboration app and
by the native app (#s407). None can drift from the others.

## Testing

- **Across widths, as mdrev is:** each component, mounted alone in a test host page, at widths
  from a phone's to a wide desktop's and at several heights, by mouse and by touch, light and
  dark. At every size: nothing overflows its pane, every control can be reached and clicked,
  and the right layout is chosen.
- **Isolation, both ways:** a test host page with CSS and keys of its own, as hostile as a host may
  be under the owner's terms (#s438): class rules on common names (`.primary`, `.hint`, `.active`,
  `.open`) and element rules on its own containers, but no blanket rules, which a host keeps out of
  its page (the collaboration app's agent rules forbid them, collab #s801). Nothing of the host
  reaches a record, and nothing of ours reaches the host.
- **The app's existing browser suite** keeps running unchanged against the app, which is now a
  host. That is the proof that the refactor kept everything.
- **The iOS Simulator walk** of the app as today, and of a test host with a narrow pane on a
  phone.
- **A conformance check** in the kit (as mdrev's `conform`) that a host runs against its own
  contract routes, to know its data is what the components read.

## Delivery

Each release publishes a kit beside its binaries, with its digest, as mdrev does. It holds:

- the module: the two components, and the detached page from a later kit;
- its stylesheet;
- type declarations;
- the contract and the guide;
- the conformance check.

A host pins a version and upgrades on purpose.

mdrev's viewer and the Mermaid runtime, which the session view uses for Markdown and diagrams,
are not in the first kits: a host vendors its own, as the native app does with its own pinned
mdrev. The kit takes them over if a second host needs them.

The first kit ships without the detached page; a later kit adds it (steps 7 and 8 below).

## The first hosts

- **The collaboration app** gives the session view a rectangular pane that the view's own code
  controls entirely, and may mount the list later. Its server mounts this repository's backend
  module (`design/backend-mount.md`), so the caching and the file serving are ours too, over the
  sessions it names (the owner, #s440). It takes no lower layer of ours: no renderer of its own
  fed with records. Its styles stay out of the pane (the owner, #s438: ours are prefixed under the
  view's element, and the host keeps blanket rules out of its own page). Its class rules could
  still reach ours by name, so the view's classes the host styles are named `sv-<name>`
  (`design/session-view-component.md`, stage 5). What it changes on its
  side is its lead's to schedule.
- **The native app** (#s407) mounts both components in its own window, reaching each paired
  monitor through the backend handle that `design/monitor-hub.md` describes.

## The order of the work

1. **One request layer.** Every request the page makes goes through a backend handle, one per
   backend: the server's base (a page origin, a host's prefix, another origin, or a native host's
   own scheme with whatever path it routes by), whether the reader is local to it, and where its
   mdrev loads from. A page that holds several backends' sessions holds several handles, and a
   handle may be answered by code in the page itself (the native app's merged source). This is
   the first step of phase 0b in `design/monitor-hub.md` (#s416).
2. **The contract written down and versioned.** The monitor already serves it; this step writes
   it, its version and its record format.
3. **The backend as something a host mounts.** The route table answers under any prefix, from
   sessions the host names (stores on disk, or its own adapter), with the host's sign-in in front
   and no assumption that it owns the port. `design/backend-mount.md` has what that takes and
   the order to build it in (#s426).
4. **The session view as a component**, with the app shell mounting it. It reads what it needs
   to know about its backend (paired, version, mdrev) from its handle, not the page's head, and
   gains the title, turn and Info events from "Customizing", and the native app's frame.
5. **The session list as a component**, with the app shell mounting it.
6. **Layout by device and pane size**, with the width tests.
7. **The kit**, with its guide and conformance check, ready for the collaboration app.
8. **Detached views**, in a later kit.

Each step lands on its own, with the app working at every step.

## Appendix: what the page assumes today

These are counts from main at v1.487.0, of what steps 4 and 5 have to re-scope.

- **Boot:** `app.js` (4,877 lines) runs at load, with no mount function and no teardown.
- **Ids:** 282 lookups by id and 29 `document.querySelector` calls.
- **Listeners:** about 16 on `document` and about 27 on `window`.
- **Body-appended elements:** about 8, such as menus and toasts.
- **Address and history:** about 25 uses.
- **Storage:** about 20 keys.
- **Page-wide CSS:**
  - 82 `:root` rules, plus `html` and `body` rules;
  - 377 `#app` rules;
  - 90 media queries that ask the window's size;
  - no container queries.
- **Server paths:** 74, written from the root.
