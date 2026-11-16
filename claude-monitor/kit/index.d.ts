// agent-monitor's kit: what the session view and the session list take and answer. The doc comments
// over `mountSessionView` (session-view.js) and `mountSessionList` (session-list-view.js) are the
// reference; these declarations follow them, and a test holds them to each component's
// MOUNT_OPTIONS and OPTIONAL_CONTROLS. docs/embedding-guide.md walks a host through them.
//
// They declare what a host is offered: what the owner ruled a host may change (light or dark, the
// optional controls, the events, and the room the view may ask for), what mounting a part needs, and
// nothing more. Each part binds its own keys and answers them only while the reader's last press or
// focus is inside it, as ruled; a key it has no action for stays the host's. The options
// agent-monitor's own page and the native app use beside those (the view's frame, back-to-the-list
// and pane-layout controls; the list's native-mode projects and its placing of its parts in a host's
// bar; the app's own keymap, which takes every key and calls the parts' key actions) are the
// components' own and not offered here, and nor is what the backend handle holds for the parts
// beyond its base and pairing.

export type Theme = "light" | "dark";

// ------------------------------------------------------------------------------- the backend

export interface BackendHandleOptions {
  /** Where the server's routes answer: "" for the page's origin, a same-origin prefix
   *  ("/agents/monitor"), or the page's origin with a path. A trailing slash is dropped. */
  base?: string;
  /** Whether the server gates its readers behind pairing (false by default): it decides whether
   *  writes are offered and whether an agent's HTML page may run in the artifact pane. */
  paired?: boolean;
  /** The mdrev release the server pins ("" for none, the default, and Markdown shows as plain
   *  text), or a function that reads it. */
  mdrevVersion?: string | (() => string);
  /** The base that serves that mdrev (`/mdrev/<version>/`, `/markdown`, `/api/mdrev/`): the
   *  handle's own base unless the host serves mdrev elsewhere. */
  mdrev?: string;
}

/** A handle on one server. What else it holds is the parts' own. */
export interface BackendHandle {
  readonly base: string;
  readonly paired: boolean;
}

/** A handle on one server that answers the backend contract (docs/backend-contract.md). */
export function backendHandle(options?: BackendHandleOptions): BackendHandle;

/** Point both components at a server; call it before mounting them. The view's `backend` option
 *  does the same. Until either is called they read the server at the directory of the page's
 *  address, as unpaired unless the page's body says `data-paired="true"`: pass a handle. */
export function useBackend(handle: BackendHandle): void;

/** A row of `GET /api/sessions` (docs/backend-contract.md, "The session list"). */
export interface SessionRow {
  id: string;
  [field: string]: unknown;
}

// ------------------------------------------------------------------------------- the session view

export interface SessionViewControls {
  /** The Reading menu (Aa) and its keys. */
  reading?: boolean;
  /** The viewport history's Save, a row of the Reading menu. */
  history?: boolean;
  /** Every ↗ that opens something in a tab of its own: the artifact pane's (a document or an image)
   *  and, where `detachTo` is given, the top bar's (the session's transcript). */
  detach?: boolean;
}

/** What writing to a session needs from the host, which owns write mode. */
export interface WriteOptions {
  /** Whether the reader has writing switched on; call the handle's `writeChanged()` when it changes. */
  on(): boolean;
  /** Ask the reader before the server types into a live pane. Answer the passcode they gave ("" where
   *  none was asked) to go on, and anything else to cancel. */
  confirm(ask: { title: string; text: string; passcode: boolean }): unknown | Promise<unknown>;
}

export interface SessionViewOptions {
  theme?: Theme;
  /** The box around the view: a phone's sheets are measured against it, and it is moved back over
   *  iOS's page scroll while the keyboard is up. */
  grid: HTMLElement;
  /** The server the view reads; see `useBackend` for what it reads without one. */
  backend?: BackendHandle;
  /** The optional controls, each shown unless it is `false`. */
  show?: SessionViewControls;
  /** Left out, the view offers no writes. */
  write?: WriteOptions;
  /** Where a tab of its own opens: the address of your page that runs `mountDetached`, to which each
   *  ↗ adds its query (`?session=…`, `?root=…`, `?path=…`), or a function asked at each ↗ (`href` the
   *  server's own page for it) that answers the address to open (anything else once it opened a
   *  window itself). Left out, the artifact
   *  pane's ↗ opens the server's own `/markdown` and `/image`, and the transcript has no ↗. */
  detachTo?: string | ((tab: { kind: DetachedKind; query: string; href: string }) => unknown);
  /** The session the host has selected ("" or null for none). */
  selectedId(): string | null;
  sessionRow(id: string): SessionRow | null;
  /** The headless workers a session started, as rows. */
  sessionWorkers(id: string): SessionRow[];
  /** The view asks the host to select a session (its parent control, a descent). */
  selectSession(id: string, push?: boolean): void;
  renderList(): void;
  refreshIndex(): void;
  pauseIndex(): void;
  resumeIndex(): void;
  onTitle?(event: { session: string; title: string }): void;
  onTurn?(event: { session: string; turn: number | null; label: string }): void;
  onInfo?(event: { session: string }): void;
  onFile?(event: { name: string; path: string; text: string | null }): void;
  onLinkCopied?(event: { url: string; record: string }): void;
  onToast?(message: string): void;
  onOutline?(event: { wanted: boolean }): void;
  /** How many more pixels the host could give the view by narrowing what it lays beside it. */
  room?(): number;
}

export interface SessionViewHandle {
  /** A session was chosen: the view takes it… */
  select(id: string): void;
  /** …and reads its records. */
  open(id: string): void;
  /** The session whose records the view reads ("" for none). */
  session(): string;
  /** The selected session is gone: the view stops reading it and says why. */
  close(title: string, detail?: string): void;
  /** The host's rows changed: the top bar, the compose box and the Agents pane follow. */
  indexChanged(): void;
  /** The host's write mode changed. */
  writeChanged(): void;
  resized(): void;
  setTheme(theme: Theme): void;
  closePane(): void;
  /** Remember the reader's choices of the view now. */
  persist(): void;
  /** Take the view away; every other member then does nothing. */
  unmount(): void;
}

/** Draw the session view in `element`, which must be empty and have no id or the id "sessionView". */
export function mountSessionView(element: HTMLElement, options: SessionViewOptions): SessionViewHandle;

// ------------------------------------------------------------------------------- the session list

export interface SessionListControls {
  /** The session filter's states and its Everything. */
  filter?: boolean;
  /** Hiding and restoring a session, a project or an agent, and the filter's Include hidden. */
  hiding?: boolean;
  /** The filter's Arrange choices and an arranged list's section menus. */
  arrangements?: boolean;
}

export interface SessionListOptions {
  theme?: Theme;
  show?: SessionListControls;
  /** Remember the list's choices, at each moment the list changes one: pass the kit's `persist`. */
  persist?(): void;
  /** The reader chose a session; the host opens it and calls the handle's `select(id)`. */
  onOpen?(event: { id: string; push: boolean }): void;
  /** The index has new rows, said before the list draws them. */
  onIndex?(): void;
  onToast?(message: string): void;
}

export interface SessionListHandle {
  redraw(): void;
  /** Read the index now. */
  refresh(): unknown;
  /** Read the index, and again every 5 s… */
  start(): void;
  /** …until this. */
  stop(): void;
  rows(): SessionRow[];
  row(id: string): SessionRow | null;
  workers(id: string): SessionRow[];
  /** The selected session ("" for none). */
  selected(): string;
  /** A selection the host made: the list takes it, marks it read and draws it, and says nothing. */
  select(id: string): void;
  setTheme(theme: Theme): void;
  /** Take the list away; every other member then does nothing. */
  unmount(): void;
}

/** Draw the session list in `element`, which must be empty and have no id or the id "sessionList". */
export function mountSessionList(element: HTMLElement, options?: SessionListOptions): SessionListHandle;

/** Remember the list's choices in this origin's storage: the `persist` the list's option takes. */
export function persist(): void;

// ------------------------------------------------------------------------------- a tab of its own

/** What a detached tab's address shows: a session's transcript, a Markdown document, an image, or
 *  nothing it can name. */
export type DetachedKind = "session" | "markdown" | "image" | "none";

export interface DetachedOptions {
  /** The server to read; it becomes the page's (`useBackend`), the tab being the page's one part. */
  backend?: BackendHandle;
  theme?: Theme;
  /** The query that says what to show. Left out, the page's own `location.search`, which the tab
   *  then keeps current as the reader moves. */
  address?: string;
}

export interface DetachedHandle {
  /** What the address named. */
  readonly kind: DetachedKind;
  setTheme(theme: Theme): void;
  /** Take the tab's content away and leave the element empty. */
  unmount(): void;
}

/** Draw a tab of its own in `element` (no id, or the id "detachedView"; empty), from the address's
 *  query: `session=<id>` a transcript, `root=…&path=…&cap=…` a Markdown document, `path=…&sig=…` or
 *  `held=…` an image. */
export function mountDetached(element: HTMLElement, options?: DetachedOptions): DetachedHandle;
