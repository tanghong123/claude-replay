// SHARED between the app shell (served as an ES module at /monitor-ui/shared/…), the classic
// rail and the v2 splice (inlined at serve time through {{SHARED}}) and the html crate's pages
// (inlined by html_export/shared.rs). Conventions the inliner relies on: no imports, exactly
// one trailing `export { … };` line.
// The app shell's keyboard, as one table (parity #11) — the classic view's keys, so muscle
// memory transfers, in one place rather than handlers scattered across modules. DOM-free: the
// contract test checks the table (no key bound twice, every classic key present) and the guard
// under node; app.js supplies the actions.
//
// `when` scopes a binding: "view" applies while nothing in the session list has focus, "list"
// only while a session-tree row does, "any" always. A binding never fires while the focus is in
// a text field — typing is typing — nor with a platform modifier held (⌘K stays the shell's own).
const KEYMAP = Object.freeze([
  { key: "/", when: "any", action: "search", hint: "/" },
  { key: "]", when: "view", action: "turn-next", hint: "]" },
  { key: "[", when: "view", action: "turn-prev", hint: "[" },
  { key: "j", when: "view", action: "head-next", hint: "j" },
  { key: "k", when: "view", action: "head-prev", hint: "k" },
  { key: "n", when: "view", action: "hit-next", hint: "n" },
  { key: "N", when: "view", action: "hit-prev", hint: "N" },
  { key: "w", when: "view", action: "wrap", hint: "w" },
  { key: "-", when: "view", action: "size-down", hint: "-" },
  { key: "_", when: "view", action: "size-down" },
  { key: "+", when: "view", action: "size-up", hint: "+" },
  { key: "=", when: "view", action: "size-up" },
  { key: " ", when: "view", action: "page-down", hint: "Space" },
  { key: " ", shift: true, when: "view", action: "page-up", hint: "⇧Space" },
  { key: "ArrowDown", when: "list", action: "list-next", hint: "↓" },
  { key: "ArrowUp", when: "list", action: "list-prev", hint: "↑" },
]);

const isEditable = target => !!target && (/^(INPUT|TEXTAREA|SELECT)$/.test(target.tagName || "") || target.isContentEditable === true);

/** The binding an event resolves to, or null. `context` is "list" while a tree row has focus,
 *  else "view"; a button or link with focus takes Space/Enter itself, so Space is left alone there. */
function resolveKey(event, context, target = null) {
  if (event.metaKey || event.ctrlKey || event.altKey) return null;
  if (isEditable(target)) return null;
  if (event.key === " " && target && /^(BUTTON|A|SUMMARY)$/.test(target.tagName || "")) return null;
  for (const binding of KEYMAP) {
    if (binding.key !== event.key) continue;
    if (!!binding.shift !== !!event.shiftKey && binding.key === " ") continue;
    if (binding.when !== "any" && binding.when !== context) continue;
    return binding;
  }
  return null;
}

/** The hint to show beside a control for an action, e.g. "n" — or "" when it has none. */
const hintFor = action => KEYMAP.find(binding => binding.action === action && binding.hint)?.hint || "";

function bindKeymap(root, contextOf, dispatch) {
  root.addEventListener("keydown", event => {
    const target = event.target;
    const binding = resolveKey(event, contextOf(target), target);
    if (!binding) return;
    if (dispatch(binding.action, event) !== false) event.preventDefault();
  });
}

export { KEYMAP, isEditable, resolveKey, hintFor, bindKeymap };
