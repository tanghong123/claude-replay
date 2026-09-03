// SHARED between the app shell (served as an ES module at /monitor-ui/shared/…), the classic
// rail and the v2 splice (inlined at serve time through {{SHARED}}) and the html crate's pages
// (inlined by html_export/shared.rs). Conventions the inliner relies on: no imports, exactly
// one trailing `export { … };` line.
// Reading preferences (parity #7): code size, wrapped long lines, a wide transcript. DOM-free —
// the contract test runs it under node. In the app shell they apply as CSS custom properties
// and two classes on the app root; the classic page applies them its own way (#45) — both
// persist under ONE key, `am-prod-reading`, so a viewer's choice follows them across shells.
// The range is the classic page's: 8–16 px in half steps. Each page keeps its own defaults
// (the classic page: 12.5 px, wrapped; the app shell: 12 px, unwrapped).
const READING_KEY = "am-prod-reading";
const DEFAULT_READING = Object.freeze({ size: 12, wrap: false, wide: false });
const SIZE_MIN = 8, SIZE_MAX = 16, SIZE_STEP = 0.5;

const clampSize = value => Math.min(SIZE_MAX, Math.max(SIZE_MIN, Math.round((Number(value) || DEFAULT_READING.size) * 2) / 2));

function parseReading(raw, defaults = DEFAULT_READING) {
  try {
    const value = JSON.parse(raw || "");
    if (!value || typeof value !== "object") return { ...defaults };
    return {
      size: clampSize(value.size ?? defaults.size),
      wrap: typeof value.wrap === "boolean" ? value.wrap : defaults.wrap === true,
      wide: typeof value.wide === "boolean" ? value.wide : defaults.wide === true
    };
  } catch (_) { return { ...defaults }; }
}

/**
 * The preferences a page starts from, through `get(key)`/`set(key, value)` (a page passes its
 * try/catch-wrapped localStorage accessors). Reads `am-prod-reading`; when it is absent and
 * `legacy` names the page's pre-#45 keys ({ size, wrap, wide } → key names, the classic page's
 * "claude-replay-export-ms" / "-wrap" / "-wide"), their values are folded in ONCE, written under
 * the one key, and the legacy keys removed — so a reader keeps what they had chosen.
 */
function loadReading(get, set, defaults = DEFAULT_READING, legacy = null, remove = null) {
  const raw = get(READING_KEY);
  if (raw != null && raw !== "") return parseReading(raw, defaults);
  const prefs = { ...defaults };
  let migrated = false;
  if (legacy) {
    const size = parseFloat(get(legacy.size));
    if (Number.isFinite(size)) { prefs.size = clampSize(size); migrated = true; }
    const wrap = get(legacy.wrap);
    if (wrap != null) { prefs.wrap = wrap !== "0"; migrated = true; }
    const wide = get(legacy.wide);
    if (wide != null) { prefs.wide = wide === "1"; migrated = true; }
  }
  if (migrated) {
    set(READING_KEY, JSON.stringify(prefs));
    if (remove) for (const key of [legacy.size, legacy.wrap, legacy.wide]) remove(key);
  }
  return prefs;
}

/** The custom properties the app root carries for these preferences. */
const readingVars = prefs => ({ "--code-size": `${clampSize(prefs.size)}px`, "--measure": prefs.wide ? "1240px" : "820px" });

export { READING_KEY, DEFAULT_READING, SIZE_MIN, SIZE_MAX, SIZE_STEP, clampSize, parseReading, loadReading, readingVars };
