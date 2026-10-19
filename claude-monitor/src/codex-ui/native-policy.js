/** Codex is the only ordinary agent whose controls may be replaced by native mode. */
export const isCodexSession = row => String(row?.agent || "").toLowerCase() === "codex" || !!row?.nativeThreadId;

/**
 * @param {"off"|"enabling"|"on"} mode Explicit user-selected mode.
 * @param {boolean} write The original master Write switch.
 * @param {{agent?:string,nativeThreadId?:string}|null} row Current reader selection.
 * @param {"none"|"create"} intent A deliberate new Codex session operation.
 * @returns {boolean} Whether native controls own the visible composer.
 */
export const usesNativeControls = (mode, write, row, intent = "none") => mode === "on" && write === true && (intent === "create" || row == null || isCodexSession(row));
