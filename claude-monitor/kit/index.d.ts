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
 *  address, as unpaired unless the page's body says `data-paired="true"`: pass a handle. A list
 *  given a `backend` of its own reads that one, whatever this or the view says. */
export function useBackend(handle: BackendHandle): void;

/** A row of `GET /api/sessions` (docs/backend-contract.md, "The session list"). */
export interface SessionRow {
  id: string;
  /** The machine of a merged source's row (`MergedMachine.machine`); no server writes it. */
  machine?: string;
  [field: string]: unknown;
}

// ---------------------------------------------------------------- the session list over machines

/** A machine a merged source reads. */
export interface MergedMachine {
  /** The host's key for it, with no "/" in it: the list's ids and keys for its sessions are
   *  `<machine>/<its own id>`. */
  machine: string;
  /** The name a reader knows it by, shown on its rows. The key, when left out. */
  label?: string;
  /** Its server. */
  handle: BackendHandle;
}

/** How a machine answered the merged source's last ask: `live`; `stale`, its last answer kept,
 *  from `since`; `waiting`, no answer yet; `failed`, none, and `error` says why. */
export interface MergedMachineState {
  machine: string;
  label: string;
  state: "live" | "stale" | "waiting" | "failed";
  since: string | null;
  error: string | null;
}

/** Where a merged row's session is: its machine and that machine's own id and row. */
export interface MergedLocation {
  machine: string;
  label: string;
  handle: BackendHandle;
  id: string;
  row: SessionRow | null;
}

/** The view's row options for a view mounted on one machine's handle, in that machine's own ids,
 *  and the list's id for one of them. */
export interface MergedViewOptions {
  listId(id: string): string;
  selectedId(): string;
  sessionRow(id: string): SessionRow | null;
  sessionWorkers(id: string): SessionRow[];
}

/** The merged source: a handle the session list takes as its `backend`, answered in the page. */
export interface MergedBackend extends BackendHandle {
  machines(): MergedMachineState[];
  locate(id: string): MergedLocation | null;
  forView(list: SessionListHandle, machine: string): MergedViewOptions;
}

/** One session list over several machines' servers (design/monitor-hub.md D6): every machine's
 *  rows, each naming its machine, its ids and keys `<machine>/<id>`, a hide sent to its machine;
 *  a machine that stops answering keeps its last rows, greyed under a head saying since when.
 *  `timeout` (ms, 4000 by default) is how long one ask waits for the machines that answered in time
 *  before; a machine already late is not waited for again, and an ask still out after eight times
 *  `timeout` is given up (its fetch aborted through `init.signal`). Its handle is the one you pass,
 *  used as it is. Open a merged row's session on its machine: `locate(id)` names it, and the view is
 *  mounted with that machine's handle. */
export function mergedBackend(machines: MergedMachine[], options?: { timeout?: number }): MergedBackend;

/** A merged id or key as its machine and that machine's own; null for one that names none. */
export function splitMachineId(id: string): { machine: string; id: string } | null;

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
  /** The server the view reads; see `useBackend` for what it reads without one. */
  backend?: BackendHandle;
  /** The optional controls, each shown unless it is `false`. */
  show?: SessionViewControls;
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
  /** The server the list reads (its sessions, another app's arrangement, its hides). Left out, the
   *  list reads the page's (`useBackend`'s, which the view's `backend` option also sets); given, the
   *  list keeps to it, so it may list one server's sessions beside a view on another. */
  backend?: BackendHandle;
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
