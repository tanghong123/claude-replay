// Reading preferences (parity #7): code size, wrapped long lines, a wide transcript. DOM-free —
// the contract test runs it under node. They apply as CSS custom properties and two classes on
// the app root, so production.css sizes and wraps through variables the reference rules never
// set, instead of per-element overrides. The classic view's ranges: 10–16 px in half steps.
export const DEFAULT_READING = Object.freeze({ size: 12, wrap: false, wide: false });
export const SIZE_MIN = 10, SIZE_MAX = 16, SIZE_STEP = 0.5;

export const clampSize = value => Math.min(SIZE_MAX, Math.max(SIZE_MIN, Math.round((Number(value) || DEFAULT_READING.size) * 2) / 2));

export function parseReading(raw) {
  try {
    const value = JSON.parse(raw || "");
    if (!value || typeof value !== "object") return { ...DEFAULT_READING };
    return { size: clampSize(value.size), wrap: value.wrap === true, wide: value.wide === true };
  } catch (_) { return { ...DEFAULT_READING }; }
}

/** The custom properties the app root carries for these preferences. */
export const readingVars = prefs => ({ "--code-size": `${clampSize(prefs.size)}px`, "--measure": prefs.wide ? "1240px" : "820px" });
