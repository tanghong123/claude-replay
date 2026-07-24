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
