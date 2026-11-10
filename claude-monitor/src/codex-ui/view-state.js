import { DEFAULT_READING, READING_KEY, parseReading } from "./shared/reading.js";
import { PHONE_QUERY } from "./shared/capabilities.js";

const json = (key, fallback) => {
  try { return JSON.parse(localStorage.getItem(key) || "") || fallback; }
  catch (_) { return fallback; }
};

// The session view's own stores (#s429, design/session-view-component.md, stage 7): made fresh by
// each `mountSessionView`, so nothing of one mount reaches the next and no host shares them. They
// were state.js's `recordState` and `uiState` until stage 7; state.js keeps what is the app's (the
// session index, the list's choices, the session search's results, write mode). The compose box's
// own state is control-store.js's.

/** The open session's records and how they are drawn. */
export const createRecordState = () => ({
  session: "", records: [], meta: null, units: [], generation: 0,
  cursor: { epoch: 0, committed: 0, gen: 0, index: 0 },
  heights: new Map(), folds: new Map(), processFolds: new Map(), processExpanded: new Set(), promptExpanded: new Set(),
  // Which process blocks the READER has spoken about (#250). `processExpanded` alone cannot say
  // whether a block is open because the reader opened it or because the page did, and the two
  // must behave differently: the page may revise its own decision, it may never revise theirs.
  // A block in here is never touched by the live-turn rule again.
  processChosen: new Set(),
  // "Expand every detail in this section" as a STANDING INTENT for that process, not a one-off
  // sweep of the records that happened to exist when it was pressed (#233). An open turn keeps
  // producing records; the reader asked for the turn to be open, so what arrives next is open
  // too. Key → true (expand) / false (collapse); absent means each record keeps its own default.
  processBulk: new Map(),
  // The heads whose TARGET is shown in full (#129): a long command is one clipped line until
  // the reader asks for it, and asking is the third step of the head's click cycle.
  fullTargets: new Set(),
  // Where each head sits in its click cycle (#129), by record id.
  headSteps: new Map(),
  following: true, newRecords: 0, search: "", matches: [], match: -1,
  // Per-turn raw overrides (#109): a value FLIPS away from what the turn shows — for a user
  // turn that is the global `rawUser` preference (the text as typed), for an assistant turn
  // the record view. Mirrored here from the reading preferences so the renderer sees one state.
  rawTurns: new Map(), rawUser: false,
  // The tool filter (#110): the ids of every record on a hit chain while a tool is selected,
  // and the fold state to put back when it is cleared.
  filterHits: null, filterDirect: null, filterSnapshot: null,
  searchMask: 0, searchWhole: false, recSizes: [], pendingSearch: false,
  capOpen: new Set(),
  // Per-block code overrides (#173): `{ size?, wrap? }` by record id, set by a code pane's own
  // A− / A+ / wrap bar. EPHEMERAL on purpose — the reading preference stays the baseline every
  // block starts from, and these are one reader's one-off look at one block, so they live here
  // and nowhere else: `persist()` never writes them and a reload starts clean.
  codeOverrides: new Map(),
  taskTargets: new Map(), agentTargets: new Map()
});

/** How the view is laid out and read: the artifact pane, the outline and its panes, the in-session
 *  search, the reading options. The reader's choices among them are read from this origin's storage
 *  as the view mounts, and `persistView` writes them back. */
export const createViewState = () => ({
  preview: false, previewTabs: [], previewId: null,
  // Until the reader chooses, the outline starts open — except on a phone (#310), where it is an
  // overlay over most of the transcript rather than a column beside it.
  navigatorOpen: (stored => stored === null ? !matchMedia(PHONE_QUERY).matches : stored !== "0")(localStorage.getItem("am-demo-navigator")),
  navCards: new Set(json("am-prod-nav-cards", ["turns"])),
  // Which panes EXIST in the outline at all (#159). Distinct from `navCards`, which is whether a
  // pane that exists is open: a pane turned off here costs nothing — no body, no head, no slot,
  // and no part in the drawer budget. The owner's reason for the two being different controls is
  // that "shut and stays shut" is not a drawer state, it is not caring about that information.
  navPanes: new Set(json("am-prod-nav-panes", ["turns", "tasks", "agents", "session"])),
  // Which info subsections the reader has folded away (#89). Per VIEWER, not per session: a
  // reader who does not care about Usage does not care about it in the next session either.
  infoFolds: new Set(json("am-prod-info-folds", [])),
  // Which outline panes show only their LIVE rows (#186). ON for tasks and agents by default,
  // because a long session's finished work is a wall nobody reads — the owner's words were that
  // showing everything "renders it useless". Per VIEWER, like the other outline choices, and the
  // pane's own header keeps showing both counts so what is held back is never a secret.
  liveOnly: new Set(json("am-prod-live-only", ["tasks", "agents"])),
  // …and for TASKS, which of the three states the pane shows (#218, the owner: "for tasks, I think
  // we will need checkboxes for running and pending (not active)"). Live work is running work and
  // pending work, and a reader watching a queue separates them; a single "active" box could not.
  // The keys are the pane's own group keys, so the filter and the grouping cannot drift. Running
  // and pending by default, which is exactly what the one box used to mean.
  taskGroupsShown: new Set(json("am-prod-task-groups", ["in_progress", "pending"])),
  searchScopes: new Set(["u", "a", "t", "o", "b", "r", "e"]), searchWhole: false, toolFilters: new Set(),
  // #303: the search box's frozen facets (design/in-session-search.md §8) — one scope chip (its
  // letters) and one tools chip (tool NAMES or `family*` keys, never the per-session letters).
  chips: { scope: "", tools: [] },
  // #293: which branches of the tool tree are expanded (`mcp`, `mcp/<server>`). Survives the
  // rebuilds a live session forces, as the classic page's `mcpOpen` does.
  toolTreeOpen: new Set(),
  reading: parseReading(localStorage.getItem(READING_KEY)) || { ...DEFAULT_READING },
  // #45: the reading key is SHARED with the classic page. It is written only once a reader has
  // chosen something (here, or on the classic page — an existing value keeps being maintained),
  // never with this shell's defaults on an unrelated persist(), which would flip the classic
  // page to them.
  readingChosen: localStorage.getItem(READING_KEY) != null
});

/** Remember the reader's choices of the view: it calls this after changing one, and as a session is
 *  chosen (which is when the app's own remembering wrote them too, before stage 7 split the two, so
 *  the outline's first state is kept from the same moment). */
export function persistView(uiState) {
  localStorage.setItem("am-demo-navigator", uiState.navigatorOpen ? "1" : "0");
  localStorage.setItem("am-prod-nav-cards", JSON.stringify([...uiState.navCards]));
  localStorage.setItem("am-prod-nav-panes", JSON.stringify([...uiState.navPanes]));
  localStorage.setItem("am-prod-info-folds", JSON.stringify([...uiState.infoFolds]));
  localStorage.setItem("am-prod-live-only", JSON.stringify([...uiState.liveOnly]));
  localStorage.setItem("am-prod-task-groups", JSON.stringify([...uiState.taskGroupsShown]));
  if (uiState.readingChosen) localStorage.setItem(READING_KEY, JSON.stringify(uiState.reading));
}
