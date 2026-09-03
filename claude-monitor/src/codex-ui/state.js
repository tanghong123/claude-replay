import { DEFAULT_READING, parseReading } from "./reading.js";

const json = (key, fallback) => {
  try { return JSON.parse(localStorage.getItem(key) || "") || fallback; }
  catch (_) { return fallback; }
};

export const indexState = {
  groups: [], rows: new Map(), selected: "", attention: false,
  collapsed: new Set(json("am-demo-collapsed", [])), ignoredCount: 0, showHidden: false,
  // Fork families opened in the tree (view state, like classic famOpen), and whether the
  // selected id was a LIST row when chosen — a sub-agent child never is, and must not be
  // declared gone by the index poll for that reason.
  openFamilies: new Set(), selectedWasRow: false,
  expandedProjects: new Set(json("am-prod-expanded-projects", [])),
  sidebarOpen: localStorage.getItem("am-demo-sidebar") !== "0",
  read: json("am-prod-read", {})
};

export const recordState = {
  session: "", records: [], meta: null, units: [], generation: 0,
  cursor: { epoch: 0, committed: 0, gen: 0, index: 0 },
  heights: new Map(), folds: new Map(), processFolds: new Map(), processExpanded: new Set(), promptExpanded: new Set(),
  following: true, newRecords: 0, search: "", matches: [], match: -1,
  rawTurns: new Set(),
  taskTargets: new Map(), agentTargets: new Map()
};

export const uiState = {
  preview: false, previewTabs: [], previewId: null,
  navigatorOpen: localStorage.getItem("am-demo-navigator") !== "0",
  navCards: new Set(json("am-prod-nav-cards", ["turns"])),
  searchTab: "all", searchScopes: new Set(["u", "a", "t", "o", "b", "r", "e"]), toolFilters: new Set(),
  globalResults: [], globalIndex: 0,
  reading: parseReading(localStorage.getItem("am-prod-reading")) || { ...DEFAULT_READING }
};

export const controlState = {
  paired: document.body.dataset.paired === "true", write: false,
  row: null, mode: "resume", consented: false, pending: ""
};

export function persist() {
  localStorage.setItem("am-demo-collapsed", JSON.stringify([...indexState.collapsed]));
  localStorage.setItem("am-prod-expanded-projects", JSON.stringify([...indexState.expandedProjects]));
  localStorage.setItem("am-demo-sidebar", indexState.sidebarOpen ? "1" : "0");
  localStorage.setItem("am-demo-navigator", uiState.navigatorOpen ? "1" : "0");
  localStorage.setItem("am-prod-nav-cards", JSON.stringify([...uiState.navCards]));
  localStorage.setItem("am-prod-read", JSON.stringify(indexState.read));
  localStorage.setItem("am-prod-reading", JSON.stringify(uiState.reading));
}

export const selectedRow = () => indexState.rows.get(indexState.selected) || null;
export const canInject = row => controlState.paired && !!row?.injectable;
export const canResume = row => controlState.paired && row?.state === "finished" && !row?.projActive && ["claude", "codex"].includes(row?.agent);
export const canCompose = row => canInject(row) || canResume(row);
