// agent-monitor's kit: what the session view and the session list take and answer. The doc comments
// over `mountSessionView` (session-view.js) and `mountSessionList` (session-list-view.js) are the
// reference; these declarations follow them, and a test holds them to each component's
// MOUNT_OPTIONS and OPTIONAL_CONTROLS. docs/embedding-guide.md walks a host through them.
//
// They declare what a host is offered: what the owner ruled a host may change (light or dark, the
// optional controls, the events, keys only while the reader works in a part, and the room the view
// may ask for) and what mounting a part needs. The options agent-monitor's own page and the native
// app use beside those (the view's frame, back-to-the-list and pane-layout controls; the list's
// native-mode projects and its placing of its parts in a host's bar) are the components' own and
// not offered here.

export type Theme = "light" | "dark";

/** "engaged" (the default): the component binds its own keys and answers them only while the
 *  reader works inside it. "page": it binds none, and the host calls the handle's `keys`. */
export type KeyboardMode = "engaged" | "page";

// ------------------------------------------------------------------------------- the backend

export interface BackendHandleOptions {
  /** "" for the page's own origin (the default), a same-origin prefix ("/agents/monitor"), or an
   *  absolute origin, with or without a path. A trailing slash is dropped. */
  base?: string;
  /** How a fetch carries credentials: "same-origin" (the default) sends the page origin's cookie. */
  credentials?: RequestCredentials;
  /** Whether the reader is at the server's machine: it decides between "show in the file manager"
   *  and a download. By default, for the page's own origin, whether the page loaded from a loopback
   *  host; for an absolute base, whether that base is one. */
  local?: boolean;
  /** The base that serves the server's pinned mdrev (`/mdrev/<version>/`, `/markdown`,
   *  `/api/mdrev/`); the handle's own base unless the host serves mdrev elsewhere. */
  mdrev?: string;
  /** The mdrev release that server pins ("" for none), or a function that reads it. */
  mdrevVersion?: string | (() => string);
  /** The server was found serving another mdrev release than the page was given. */
  onMdrevMoved?: (version: string) => void;
  /** Whether the server gates its readers behind pairing: it decides whether writes are offered and
   *  whether an agent's HTML page may run in the artifact pane. */
  paired?: boolean;
  /** The server's own version, when it says. */
  version?: string | null;
}

export interface BackendHandle {
  readonly base: string;
  readonly local: boolean;
  readonly paired: boolean;
  readonly version: string | null;
  mdrevVersion(): string;
  mdrevMoved(version: string): void;
  /** The URL of a server path (it must start with "/"). */
  url(path: string): string;
  fetch(path: string, init?: RequestInit): Promise<Response>;
  mdrevUrl(path: string): string;
  mdrevFetch(path: string, init?: RequestInit): Promise<Response>;
}

/** A handle on one server that answers the backend contract (docs/backend-contract.md). */
export function backendHandle(options?: BackendHandleOptions): BackendHandle;

/** Point both components at a server; call it before mounting them. The view's `backend` option
 *  does the same. Until either is called, they read the page's own origin. */
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
  /** The artifact pane's ↗, which opens a document or an image in a tab of its own. */
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
  backend?: BackendHandle;
  /** The optional controls, each shown unless it is `false`. */
  show?: SessionViewControls;
  keyboard?: KeyboardMode;
  /** Left out, the view offers no writes. */
  write?: WriteOptions;
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
  /** The view's key actions, by name, for a host in "page" mode. */
  readonly keys: Readonly<Record<string, (event?: KeyboardEvent) => unknown>>;
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
  keyboard?: KeyboardMode;
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
  /** The list's key actions, `"list-next"` and `"list-prev"`, each `false` when it moves nothing. */
  readonly keys: Readonly<Record<string, () => boolean>>;
  setTheme(theme: Theme): void;
  /** Take the list away; every other member then does nothing. */
  unmount(): void;
}

/** Draw the session list in `element`, which must be empty and have no id or the id "sessionList". */
export function mountSessionList(element: HTMLElement, options?: SessionListOptions): SessionListHandle;

/** Remember the list's choices in this origin's storage: the `persist` the list's option takes. */
export function persist(): void;
