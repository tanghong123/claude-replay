# claude-replay — HTML export spec

Design reference: `Session Export.dc.html` (interactive mockup). This doc specifies what the
Rust dumper (`--dump-html <stem>` proposed flag) must emit: a **single self-contained .html**,
no network, no external assets, working folding without any framework.

---

## 1. Element inventory

Every block type the TUI models, with its HTML treatment. Terms: *fold* = a
collapsible block; *open/closed* = default state (mirrors the TUI `FoldPolicy`).

| TUI block (`model.rs`) | HTML element | Default | Notes |
|---|---|---|---|
| UserText | `.uturn` card | open (not foldable) | `❯` caret, tinted card, right-aligned timestamp, `#tN` anchor. Sidebar + sticky-bar entry. |
| Command (slash cmd) | `.uturn.fold` card variant | **closed** | `/name` mono badge + first arg line + line-count chip in header; body renders the full multi-line args as markdown (fixes the single-`Line` collapse bug). |
| Injected skill/command instructions | same as Command | **closed** | keyed off the JSONL wrapper marker, never genuine prose. |
| Assistant text | `.ablock` | open (not foldable) | small dot marker + native-markdown prose (§3). |
| Thinking | `.fold[data-kind=think]` | **closed** | header `✻ Thought for Ns`; body = thinking text, muted, thin left rule. |
| Grouped turn activity | `.fold[data-kind=act]` | **closed** | header = CC summary line (`✻ Thought for 5s · read 1 file, ran 3 shell commands (zsh, pwd, ls)`); body nests the absorbed tool folds. |
| Bash (non-mutating) | `.fold[data-kind=bash]` | **closed** | header: green `Bash` + command (ellipsized, mono) + output-line-count chip; body: `⎿` + `<pre>` output. |
| Bash (mutating) / Edit / Write / MultiEdit | `.fold[data-kind=edit|write]` | **open** | header: tool name + path + diff-stat chips (`+19` / `−15`); body: `⎿ Added N lines…` note + diff table (§4). Write shows a 10-line numbered preview + `⋯ +N more lines` expander. |
| Read | `.fold[data-kind=read]` | **closed** | path + line-count chip; body = numbered lines + expander. |
| Skill | `.fold[data-kind=skill]` | **closed** | `Skill` + name; body = `⎿` base-dir line + folded file reads nested inside. |
| Agent / Task | `.fold[data-kind=agent]` | **closed** | description + `N tool uses` chip; body = nested tool folds or result summary. |
| Generic tool / tool_result | `.fold[data-kind=tool]` | **closed** | name + args preview; body = `⎿` result pre. |
| `⋯ N more lines` cap | `.morebtn` | — | inline text button; reveals the hidden tail (all content IS in the file, just `display:none`). |

## 2. DOM structure

```html
<body>
  <header id="topbar">brand · search(input+count) · expand-all · collapse-all · theme</header>
  <div class="layout">                 <!-- flex; max-width 1160px centered -->
    <nav id="sidebar">                 <!-- sticky; width 240px -->
      turn list (.side-item[data-t]) + keyboard legend
    </nav>
    <main>                             <!-- max-width 820px -->
      <section class="session-header"> title + mono meta row </section>
      <div id="stickybar"> ❯ Turn N — label </div>   <!-- position:sticky; JS-updated -->
      ...blocks in transcript order...
    </main>
  </div>
</body>
```

Fold skeleton (the only JS contract that matters):

```html
<div class="fold" id="b7" data-kind="bash" data-open="0">
  <div class="fold-h" tabindex="0" role="button" aria-expanded="false">
    <span class="chev">▸</span>
    ...marker / tool name / mono args / chips...
    <a class="alink" href="#b7">#</a>
  </div>
  <div class="fold-b" style="display:none"> ...body... </div>
</div>
```

Rules:
- Every block gets a stable id: `t{n}` for user turns, `b{n}` for others (deep links).
- Nested folds are legal to any depth (activity → Bash → result); the toggle uses
  `:scope > .fold-h` / `:scope > .fold-b` so children are unaffected.
- User turns carry `data-turn="N"` + `data-label="first ~80 chars"` for the
  sidebar/sticky-bar scroll spy.
- Open folds get `background:var(--openbg); border:1px solid var(--border)` on the wrapper —
  the "distinct background for expanded foldables" backlog item, solved in HTML.

## 3. Markdown → native HTML (assistant prose, user text, command bodies)

Rendered from the markdown AST the TUI already parses (`markdown.rs`) — do **not**
re-wrap to a column; the browser wraps.

| md | HTML | Style |
|---|---|---|
| paragraph | `<p>` | `var(--fs)` (15.5px) / 1.6, sans |
| heading 1–3 | `<div>` styled | 21/17/15.5px bold (no `<h*>` jump-scale; transcripts need calm hierarchy) |
| bold / italic | `<strong>/<em>` | — |
| inline code | `<code>` | mono .88em, `--icbg` bg, `--icfg` text, 4px radius |
| link | `<a>` | `--link`, underline on hover |
| ul / ol | native `<ul>/<ol>` | 22px indent, 3px vertical li margin |
| blockquote | `<blockquote>` | 3px left rule `--border`, muted italic |
| table | native `<table>` | full-width, collapsed 1px `--border`, header row `--panel`, 14px — this **replaces** the fair-share width algorithm; HTML tables solve it natively |
| fenced code | `.fence` | panel card: header row (lang label + copy button) + `<pre>` 13px/1.65 mono, `overflow-x:auto`; syntax colors via `--kw --str --fn --com` spans (map from syntect's classes) |

## 4. Diff rendering

Per line: `flex` row of gutter (42px right-aligned line number) + 16px marker + `pre-wrap` code.

- context: default fg `--muted`, gutter `--gut`, marker blank
- added: row bg `--addbg`, text `--addfg`, gutter+marker `--addgut`, marker `+`
- removed: row bg `--delbg`, text `--delfg`, gutter+marker `--delgut`, marker `−`
- container: 1px `--border`, 6px radius, `overflow:hidden`, bg `--bg`, mono `var(--ms)` / 1.75
- `user-select:none` on gutters/markers so copied text is clean code.

## 5. Design tokens (CSS custom properties on `:root`)

| Token | Light | Dark | Role |
|---|---|---|---|
| `--bg` | `#faf9f7` | `#191817` | page |
| `--fg` | `#26241f` | `#e7e2d8` | prose |
| `--muted` | `#716a5c` | `#a69a86` | secondary text, fold summaries (dark = CC's fold-header rgb(166,154,134)) |
| `--faint` | `#a69e8e` | `#6e675c` | chips, chevrons, timestamps |
| `--border` | `#e6e1d5` | `#33312b` | all rules |
| `--card` | `#f0ede3` | `#2c2a25` | user-turn block (dark ≈ CC user bg 237) |
| `--panel` | `#f4f2ec` | `#222120` | fences, table headers |
| `--openbg` | `#f5f3ed` | `#201f1d` | expanded-fold wrapper |
| `--hover` | `#edeade` | `#282623` | header hover |
| `--tool` | `#2e7d55` | `#7fcd90` | tool names/dots (dark ≈ CC 114) |
| `--link` | `#3a6ea5` | `#8fbde8` | links, focus outline |
| `--icfg/--icbg` | `#28567e` / `#eae7db` | `#a5cdf0` / `#252b31` | inline code (dark ≈ CC 153) |
| `--addbg/--addfg/--addgut` | `#e5f1e2/#255c33/#57a06b` | `#20351f/#a8dcae/#5cae64` | diff add (dark ≈ CC 22/77) |
| `--delbg/--delfg/--delgut` | `#f9e7e3/#963a2b/#c07a6c` | `#3b211c/#e3a99e/#b06a5c` | diff del (dark ≈ CC 52/167) |
| `--gut` | `#b3ab9b` | `#5c574c` | line numbers |
| `--kw/--str/--fn/--com` | `#6d4fa1/#8f5a22/#28567e/#a69e8e` | `#b79ae8/#d8a86a/#a5cdf0/#6e675c` | syntax |
| `--fs` / `--ms` | `15.5px` / `12.5px` | same | prose / mono size |

Fonts (no webfonts — self-contained):
- sans: `-apple-system, BlinkMacSystemFont, 'Segoe UI', Helvetica, Arial, sans-serif`
- mono: `ui-monospace, 'SF Mono', SFMono-Regular, Menlo, Consolas, monospace`

Theme switch = swap the variable set on `:root` (+ persist in
`localStorage['claude-replay-export-theme']`). Emit light values as the stylesheet default.

## 6. Behavior (single inline `<script>`, ~150 lines, no deps)

- **Fold toggle**: click on `.fold-h` (delegated), or Space/Enter when focused.
  Sets `data-open`, chevron `rotate(90deg)`, body `display`, wrapper open-bg.
- **Expand/collapse all**: toolbar buttons iterate `.fold`.
- **Search**: substring match over `.blk` text; count label; Enter cycles hits —
  auto-expands ancestor folds, smooth-scrolls (offset −120px), 1s `--flash` box-shadow.
- **Keyboard**: `j/k` move focus across fold headers · `Space/Enter` toggle ·
  `[ ]` prev/next user turn · `/` focus search · `Esc` blur.
- **Scroll spy** (rAF-throttled): last `[data-turn]` above y=130 → sticky bar text +
  visibility (shown once the turn card scrolls past) + sidebar active item.
- **Deep links**: `#id` on load expands ancestors and scrolls; `#` header anchors use
  `history.replaceState` (no jump).
- **Copy**: `.cpy` copies the sibling `<pre>` textContent (clipboard API; fences only).

## 7. Rust emission notes

- One pass over the existing `Block` list; reuse `fold_key`/`FoldPolicy` for `data-kind`
  and `data-open`. The CC-style collapsed summary strings you already build become the
  fold-header text verbatim.
- HTML-escape everything (`&<>"`); the mockup's entity handling shows the expected output.
- Emit CSS classes for the repeated primitives (`.fold-h`, diff rows, chips) rather than
  the mockup's inline styles — the mockup inlines only because of its authoring format.
- Unlike `--dump`, do **not** cap long bodies: emit full content wrapped in a hidden
  `<div>` behind the `⋯ N more lines` button (grep-ability of the file is a feature).
- Syntax highlighting: keep syntect, map its 256-color theme indices → the four `--syn`
  vars (or emit `style="color:var(--kw)"` spans directly).
- Size guard: a 33k-line session ≈ 2–4 MB of HTML — fine. Beyond ~20 MB consider
  `--dump-html --split-turns`.
- Suggested CLI: `claude-replay <id> --dump-html [stem]` → `<stem>.html`, honoring the
  same `--fold/--unfold/--full` flags as `--dump`.


---

## 8. Revision 2 — polish pass (supersedes conflicting details above)

Reference: `design/html-dump/session-export-mockup.html` (open it; it implements all of this).

### 8.1 Fold header alignment
`.fold-h` is `align-items: flex-start` (was `center`) so a wrapping target keeps the
chevron / dot / tool name / chips / `#` on line 1. The comment in the current exporter CSS
asserting center-alignment is intentional must be removed.

One shared line box is what actually makes them align: `.fold-h { line-height: 20px }`,
and every child pinned to it — the `font:` shorthand resets `line-height` to `normal`, so
targets must be `font: 12px/20px var(--mono)` and chips `font: 10.5px/16px var(--mono)`
with `margin-top: 2px`; the tool-name span needs explicit `line-height: 20px`; the 8px dot
`margin-top: 6px`.

### 8.2 Collapsed vs expanded header target
- collapsed: `white-space: nowrap; overflow: hidden; text-overflow: ellipsis; min-width: 0`
- expanded: `white-space: pre-wrap; overflow: visible; text-overflow: clip; overflow-wrap: anywhere`

Emit the expanded form **inline for blocks authored `data-open="1"`** so it is correct
before JS runs, and have the toggle sync it on every state change. Any init pass must run
over *all* folds, not only the ones the user touches.

### 8.3 Per-pane code controls (replaces any global font control)
Each `.numbered`/`.diff` pane is wrapped in `.codewrap` and followed by a `.codefoot` row:
the `⋯ N more lines` expander on the left, controls right-aligned — `A−`, the current size,
`A+`, the wrap/scroll toggle, `copy`. It shares the row the expander already occupies, so it
costs no extra line, and it is never visible when no code is on screen.

- `A−`/`A+` step `--ms` 8→16px in 0.5 increments; **global and persisted**
  (`localStorage['claude-replay-export-ms']`) — adjusting one pane resizes all.
- wrap toggle: `⤶` = `.code { white-space: pre-wrap }`; `↔` = `white-space: pre` +
  `overflow-x: auto` on the pane, so long diff lines keep their shape and pan horizontally.
  Persisted in `localStorage['claude-replay-export-wrap']`.
- `copy` copies only the `.code` column (no gutters or markers).
- Keys: `-` / `+` size, `w` wrap.
- **All static button styling belongs in the stylesheet** (`.codebar button{…}`,
  `:hover`, `:focus-visible`), never inline — an inline `color`/`background` beats author
  rules and kills hover feedback. State uses classes (`.on`, `.bad`). Rest at
  `opacity:.8`, `1` on `.codewrap:hover`/`:focus-within` — a declarative rule, zero JS.

### 8.4 Deep-link anchors copy, they do not navigate
Clicking `#` must **not** scroll, change the hash, or toggle the fold
(`preventDefault` + `stopPropagation`). It copies the absolute link
(`location.href.split('#')[0] + '#id'`), flips to a green `✓` for ~1.4s, and carries
`title="Copy a link to this spot"`. Loading a URL that already has a hash still scrolls
and expands as before.

### 8.5 Clipboard must never lie
Exports are normally opened from `file://`, where `navigator.clipboard` is refused. One
helper used by all four call sites (anchors, fence copy, pane copy, session id): try the
async API, fall back to a hidden-textarea `execCommand('copy')`, and only report success
when one resolves. On failure show a distinct state (`⚠` / `blocked`, title
"press ⌘C") — never a `✓`. No unhandled rejections.

### 8.6 Top bar must not clip its trailing control
The bar is `position: fixed` and cannot scroll, so nothing may overflow. Make the search
box the flexible element (`flex: 1 1 auto; min-width: 0`, input `width: auto`), keep no
extra spacer div, and shed chrome progressively as width tightens (replaces the stale
900px breakpoint): hide the code-size readout < 1080px, swap "Expand all"/"Collapse all"
to `⌄`/`⌃` icons with `title` < 1000px, hide the brand suffix < 820px.

### 8.7 Focus rings follow their corner
`.uturn.fold > .fold-h` needs `border-radius: 10px` — an outline follows its own
element's radius, so without it the focus ring draws square inside the rounded turn card.

### 8.8 Width & density (was the open backlog — all agreed and implemented)

**Wide mode.** A `⇔ Wide` toggle beside the theme button drops both width caps
(`.layout` 1160px → none, `main` 820px → none) so code panes get the whole window; the
button turns accent-colored and reads `⇔ Narrow` while active. Persisted in
`localStorage['claude-replay-export-wide']`. Not the default — prose pays for it.

**Rail indent (replaces §2's 28px-per-level padding).** Fold bodies indent with
`padding-left: 14px; margin-left: 11px; border-left: 1px solid var(--border)` instead of
24–28px of padding. Same hierarchy, half the horizontal cost, and a visible rail tying a
body to its header. (The slash-command card body keeps its own padding — no rail.)

**Pane bleed.** `.fold-b > .codewrap { margin-left: -10px; margin-right: -6px }` — code
panes reach past the prose column on both sides.

**Per-kind keylines.** Every fold header carries `box-shadow: inset 2px 0 0 <hue>` in
*both* states, so a collapsed session is scannable: edit/write `--tool`, bash/tool
`--link`, read `--gut`, skill/agent `--kw`, think/act `--faint`, command none (the card
carries its own identity). A filter hit overrides with `inset 3px 0 0 var(--tool)` — apply
it as a **class** (`.fold-h.filter-hit`), never an inline box-shadow, or the two fight.

**Diff tint scope.** Tint the **code column only** (`.diff .nrow.add .code { background:
var(--addbg) }`), not the whole row — gutters stay neutral and the `+`/`−` markers carry
the signal. This is a deliberate divergence from the TUI's full-row tint; expect it when
diffing against the golden capture.

**Diff density.** `line-height: 1.75` → `1.5` on `.numbered`/`.diff`.

**Fold transition + scroll anchoring.** Expanding plays a 160ms
`@keyframes foldin` (opacity + `translateY(-3px)`) on the body. The toggle measures the
header's viewport top before and after the state change and `scrollBy`s the delta, so the
clicked row does not move; if it would land behind the sticky bars (< 96px) it eases to
104px. Bulk expand/collapse and programmatic navigation skip anchoring.

**Scrollbar reservation.** In scroll mode the pane gets a `.scrollx` class →
`padding-bottom: 10px`. Without it an overlay scrollbar (which consumes no layout space)
paints inside the padding box and strikes through the last 18.75px row. Wrap mode keeps
zero padding.

**Expander labels.** `⋯ 126 more lines · to line 132` — name the range, not just a count.

**Session header.** 19px title, `padding: 18px 0 12px` — it should not claim the first
screen.

### 8.9 Still open (nothing agreed)
Nothing outstanding. Future ideas would start from: printing/PDF of an export, splitting
very large sessions across files, and a compare-two-sessions view.
