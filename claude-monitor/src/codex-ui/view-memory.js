// What the viewport remembers about a session between page loads — DOM-free, so the contract
// test can run it under node. The memory is the same anchor the viewport already keeps while
// a session is open (a unit key plus its offset from the viewport top), or the fact that the
// reader was following the tail; following IS the position, so a followed session comes back
// following rather than at a stale offset. Unit keys are the record ids the server assigns,
// stable across reloads, which is what makes remembering by key work at all.
export const viewMemoryKey = session => `am-view:${session}`;

// The reader's CHOICES ride with the position (#114): which folds, processes, prompts and
// images they opened, which turns they read raw, which caps they expanded — keyed by record
// id or unit key, both stable across reloads — so a reload or a switch away and back shows
// the session as they left it, not as the server authored it.
const VIEW_FIELDS = ["folds", "processFolds", "processExpanded", "promptExpanded", "rawTurns", "capOpen", "openImages"];

/** The reader state → a plain object (Maps as objects, Sets as arrays); empty ones omitted. */
export function viewChoices(state) {
  const out = {};
  for (const field of VIEW_FIELDS) {
    const value = state?.[field];
    if (value instanceof Map && value.size) out[field] = Object.fromEntries(value);
    else if (value instanceof Set && value.size) out[field] = [...value];
  }
  return out;
}

/** Put remembered choices back into the reader state (missing fields leave it untouched). */
export function applyViewChoices(state, choices) {
  if (!state || !choices || typeof choices !== "object") return;
  for (const field of VIEW_FIELDS) {
    const value = choices[field];
    if (state[field] instanceof Map && value && typeof value === "object" && !Array.isArray(value)) state[field] = new Map(Object.entries(value));
    else if (state[field] instanceof Set && Array.isArray(value)) state[field] = new Set(value.map(String));
  }
}

export function parseViewMemory(raw) {
  try {
    const value = JSON.parse(raw || "");
    if (!value || typeof value !== "object") return null;
    const view = value.view && typeof value.view === "object" ? { view: value.view } : {};
    if (value.following === true) return { following: true, ...view };
    if (typeof value.key === "string" && value.key && Number.isFinite(value.top)) return { following: false, key: value.key, top: value.top, ...view };
    return null;
  } catch (_) { return null; }
}

export const serializeViewMemory = value => JSON.stringify({ ...(value.following ? { following: true } : { following: false, key: value.key, top: Math.round(value.top) }), ...(value.view && Object.keys(value.view).length ? { view: value.view } : {}) });
