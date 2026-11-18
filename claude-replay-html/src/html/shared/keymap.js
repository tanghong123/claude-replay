// SHARED between the app shell (served as an ES module at /monitor-ui/shared/…) and the html
// crate's pages (inlined by html_export/shared.rs). Conventions the inliner relies on: no
// imports, exactly one trailing `export { … };` line.
// The app shell's keyboard, as one table (parity #11) — the classic view's keys, so muscle
// memory transfers, in one place rather than handlers scattered across modules. DOM-free: the
// contract test checks the table (no key bound twice, every classic key present) and the guard
// under node; app.js supplies the actions.
//
// `when` scopes a binding: "view" applies while nothing in the session list has focus, "list"
// only while a session-tree row does, "any" always. A binding never fires while the focus is in
// a text field — typing is typing — nor inside a guest the page mounted (`inGuest`, #270), nor
// with a platform modifier held (⌘K stays the shell's own).
//
// A `command` binding is the one exception (#s509, Hong's ruling on #s508: "⌘F opens the session's
// own search (a second ⌘F goes to the browser)"). It is pressed WITH the platform's command key
// (`COMMAND_KEY`: ⌘ on macOS and iPadOS, Ctrl elsewhere, the key the browser's own find answers
// to) and nothing else held, and it fires from a text field only inside an element the page marks
// `data-command-keys` (the compose box, where a reader who has just sent a prompt still has the
// focus). The search field itself carries no mark, so the second press there is the browser's.
const APPLE = typeof navigator !== "undefined" && /Mac|iPhone|iPad|iPod/.test(navigator.platform || "");
const COMMAND_KEY = APPLE ? "metaKey" : "ctrlKey";
const KEYMAP = Object.freeze([
  { key: "/", when: "any", action: "search", hint: "/" },
  { key: "f", command: true, when: "any", action: "search", hint: APPLE ? "⌘F" : "Ctrl+F" },
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
  // The sidebar's collapse into its icon rail, and back (#54) — from anywhere, since the rail
  // is what a reader reaches for when the list is in the way. Pages without a rail ignore it.
  { key: "\\", when: "any", action: "sidebar-toggle", hint: "\\" },
  // The outline (navigator) pane beside the transcript, hidden and back (#55): with the
  // sidebar on its rail, the transcript then has the whole window.
  { key: "o", when: "any", action: "navigator-toggle", hint: "o" },
  // The tasks pane centered on what is running (#57) — or on the done/pending boundary.
  { key: "c", when: "view", action: "tasks-center", hint: "c" },
  // Up to the parent session from a sub-agent's transcript (#82).
  { key: "u", when: "view", action: "parent", hint: "u" },
]);

// The full-width brackets, read as the bracket keys (#s576). A Chinese keyboard can give [ and ] as
// 【 and 】 (U+3010, U+3011) even with nothing editable focused: the iPad Simulator under Pinyin gave
// one [ as U+3010 (#s544), and a Mac typing Chinese punctuation may do the same. No binding uses
// them, so nothing else can fire. The keys are still read by `key`, never by `code`: on a layout
// other than US the bracket keys sit elsewhere.
const WIDE_KEYS = new Map([["\u3010", "["], ["\u3011", "]"]]);

const isEditable = target => !!target && (/^(INPUT|TEXTAREA|SELECT)$/.test(target.tagName || "") || target.isContentEditable === true);

/** The GUEST region an element is in, if any — mdrev's viewer in the app shell's preview pane
 *  (#270), which the host marks with `data-guest-keys` because only the host knows where it mounted
 *  one. A guest has keys of its own (`a`, `n`, `p`, `[`, `]`, `/`, Escape) that act while the reader
 *  is engaged with it, and a binding here answering the same press would move the page under the
 *  reader: `n` would step the transcript's search AND mdrev's next change. */
const guestOf = target => (target && typeof target.closest === "function" && target.closest("[data-guest-keys]")) || null;
/** A key pressed on something inside a guest is the guest's — `when: "any"` bindings included,
 *  which no context value can switch off. */
const inGuest = target => !!guestOf(target);
/** A text field a `command` binding may fire from: one inside an element marked
 *  `data-command-keys` (#s509). */
const commandField = target => !!(target && typeof target.closest === "function" && target.closest("[data-command-keys]"));

/** The binding an event resolves to, or null. `context` is "list" while a tree row has focus,
 *  else "view"; a button or link with focus takes Space/Enter itself, so Space is left alone there. */
function resolveKey(event, context, target = null) {
  // The command key alone: ⌘⇧F, ⌘⌥F and, on a Mac, Ctrl+F (a text field's forward-a-character)
  // stay the browser's and the field's. Caps Lock makes the letter a capital.
  const other = COMMAND_KEY === "metaKey" ? "ctrlKey" : "metaKey";
  const command = !!event[COMMAND_KEY] && !event[other] && !event.altKey && !event.shiftKey;
  if (!command && (event.metaKey || event.ctrlKey || event.altKey)) return null;
  if (isEditable(target) && !(command && commandField(target))) return null;
  if (inGuest(target)) return null;
  if (event.key === " " && target && /^(BUTTON|A|SUMMARY)$/.test(target.tagName || "")) return null;
  const pressed = WIDE_KEYS.get(event.key) || event.key;
  const key = command ? String(pressed || "").toLowerCase() : pressed;
  for (const binding of KEYMAP) {
    if (!!binding.command !== command) continue;
    if (binding.key !== key) continue;
    if (!!binding.shift !== !!event.shiftKey && binding.key === " ") continue;
    if (binding.when !== "any" && binding.when !== context) continue;
    return binding;
  }
  return null;
}

/** The hint to show beside a control for an action, e.g. "n" — or "" when it has none. An action
 *  with two keys names both ("/ or ⌘F"); `{ command: false }` leaves out a command binding's, for
 *  a control with little room on a screen that seldom has a hardware keyboard (a phone's search). */
const hintFor = (action, { command = true } = {}) => KEYMAP.filter(binding => binding.action === action && binding.hint && (command || !binding.command)).map(binding => binding.hint).join(" or ");

function bindKeymap(root, contextOf, dispatch) {
  // ENGAGEMENT, as the guest itself defines it: the last click or focus landed inside it (mdrev's
  // `scopeToElement` is exactly this), so the page yields exactly where the guest takes. The
  // keydown's target alone cannot say it — a click on a guest's prose leaves the focus on <body>,
  // and making the guest focusable to change that is worse (#270: a focusable mdrev host turned one
  // key press into thousands of trusted keydowns, measured in Chrome).
  let engaged = null;
  const engage = event => { engaged = guestOf(event.target); };
  root.addEventListener("mousedown", engage, true);
  root.addEventListener("focusin", engage, true);
  root.addEventListener("keydown", event => {
    if (engaged && engaged.isConnected !== false) return;
    const target = event.target;
    const binding = resolveKey(event, contextOf(target), target);
    if (!binding) return;
    if (dispatch(binding.action, event) !== false) event.preventDefault();
  });
}

export { KEYMAP, COMMAND_KEY, isEditable, inGuest, resolveKey, hintFor, bindKeymap };
