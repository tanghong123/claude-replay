import { composeCapability } from "./shared/control-protocol.js";
import { backend } from "./backend.js";

const json = (key, fallback) => {
  try { return JSON.parse(localStorage.getItem(key) || "") || fallback; }
  catch (_) { return fallback; }
};

// The app's stores (#s429, design/session-view-component.md, stages 3 and 7). The app is app.js (the
// session list, the drawer, the chrome) with session-list.js and the session index; native mode, the
// monitor's own, reads them too (native-entry.js, codex-workspace/monitor-controls.js). The session
// view (session-view.js and the modules under it) imports none of this: it makes its own stores as it
// mounts (view-state.js, and control-store.js's compose state), and asks the app for the rest through
// its options.

/** The session index and the list's choices. The view asks for the selection, a row and a session's
 *  workers through `selectedId`, `sessionRow` and `sessionWorkers`. */
export const indexState = {
  groups: [], rows: new Map(), selected: "",
  collapsed: new Set(json("am-demo-collapsed", [])), ignoredCount: 0, showHidden: false,
  // The session filter (#202): which of the three buckets — recent (active in the last 6 hours) /
  // blocked / idle — the tree shows. The owner's default is Active recently and Blocked (a new
  // key, so the earlier release's remembered set does not override it); remembered like the
  // other view choices; every bucket is "no filter". Include hidden stays a view state (not
  // remembered, like classic), so a reload starts with hidden rows hidden.
  buckets: new Set(json("am-prod-session-filter", ["recent", "blocked"])),
  // How the list is arranged (#s387, design/layout-sync.md): "tree" (by agent and project),
  // "flat" (#s206: ONE list by activity, with no agent or project groups) or another app's list —
  // "claude-app", "codex-app" or "herdr", followed at every refresh. Remembered like the buckets:
  // the app chosen under its own key, tree or flat under #s206's, so a page that only knows #s206's
  // key still reads it.
  arrange: (() => {
    const app = localStorage.getItem("am-prod-session-arrange");
    if (["claude-app", "codex-app", "herdr"].includes(app)) return app;
    return localStorage.getItem("am-prod-session-flat") === "1" ? "flat" : "tree";
  })(),
  get flat() { return this.arrange === "flat"; },
  // The arrangement an app replaced (#s387): the list falls back to it while the app's list
  // cannot be read, and the line's ✕ returns to it. Kept under #s206's key, so "One list by
  // activity" survives a reload while an app is chosen (the review of #s387).
  arrangeBefore: localStorage.getItem("am-prod-session-flat") === "1" ? "flat" : "tree",
  // #s387: each arranged section's own order and grouping (the reader's, beside the app's), keyed
  // `L:<source>:<section id>` — the reader's, never the app's — remembered like the arrangement.
  sectionViews: json("am-prod-section-views", {}),
  // Fork families opened in the tree (view state, like classic famOpen), and whether the
  // selected id was a LIST row when chosen — a sub-agent child never is, and must not be
  // declared gone by the index poll for that reason.
  openFamilies: new Set(), selectedWasRow: false,
  expandedProjects: new Set(json("am-prod-expanded-projects", [])),
  sidebarOpen: localStorage.getItem("am-demo-sidebar") !== "0",
  read: json("am-prod-read", {})
};

/** The session search layer's (⌘K) results and the one chosen. */
export const searchState = { globalResults: [], globalIndex: 0 };

/** The compose box (native-entry.js reads `write`). */
export const controlState = {
  get paired() { return backend().paired; }, write: false,
  row: null, mode: "resume", consented: false, pending: ""
};

/** Remember the app's choices: it calls this after changing one. The view remembers its own
 *  (view-state.js `persistView`). */
export function persist() {
  localStorage.setItem("am-demo-collapsed", JSON.stringify([...indexState.collapsed]));
  localStorage.setItem("am-prod-expanded-projects", JSON.stringify([...indexState.expandedProjects]));
  localStorage.setItem("am-demo-sidebar", indexState.sidebarOpen ? "1" : "0");
  localStorage.setItem("am-prod-read", JSON.stringify(indexState.read));
  localStorage.setItem("am-prod-session-filter", JSON.stringify([...indexState.buckets]));
  const appChosen = ["claude-app", "codex-app", "herdr"].includes(indexState.arrange);
  localStorage.setItem("am-prod-session-flat", indexState.flat || (appChosen && indexState.arrangeBefore === "flat") ? "1" : "0");
  localStorage.setItem("am-prod-section-views", JSON.stringify(indexState.sectionViews));
  if (["claude-app", "codex-app", "herdr"].includes(indexState.arrange)) localStorage.setItem("am-prod-session-arrange", indexState.arrange);
  else localStorage.removeItem("am-prod-session-arrange");
}

export const selectedRow = () => indexState.rows.get(indexState.selected) || null;
// Who may be composed to is the shared protocol's rule (shared/control-protocol.js, #48).
export const canInject = row => composeCapability(row, controlState.paired).inject;
export const canResume = row => composeCapability(row, controlState.paired).resume;
export const canCompose = row => canInject(row) || canResume(row);

