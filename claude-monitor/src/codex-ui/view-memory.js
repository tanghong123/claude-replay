// What the viewport remembers about a session between page loads — DOM-free, so the contract
// test can run it under node. The memory is the same anchor the viewport already keeps while
// a session is open (a unit key plus its offset from the viewport top), or the fact that the
// reader was following the tail; following IS the position, so a followed session comes back
// following rather than at a stale offset. Unit keys are the record ids the server assigns,
// stable across reloads, which is what makes remembering by key work at all.
export const viewMemoryKey = session => `am-view:${session}`;

export function parseViewMemory(raw) {
  try {
    const value = JSON.parse(raw || "");
    if (!value || typeof value !== "object") return null;
    if (value.following === true) return { following: true };
    if (typeof value.key === "string" && value.key && Number.isFinite(value.top)) return { following: false, key: value.key, top: value.top };
    return null;
  } catch (_) { return null; }
}

export const serializeViewMemory = value => JSON.stringify(value.following ? { following: true } : { following: false, key: value.key, top: Math.round(value.top) });
