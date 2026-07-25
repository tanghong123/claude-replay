# Sub-agent activity — UX design

Design record for the `DESIGN.md` backlog item *"Track sub-agent activity (spawned from
the main session)"*. Discovery is already verified there; this document covers **model,
navigation, and render** for the TUI and the HTML export.

Companion mocks in this directory (open in a browser, both are self-contained):

| File | What it shows |
|---|---|
| `tui-mock.html` | The TUI as proposed — real palette and line shapes, working keys and mouse |
| `html-export-mock.html` | The HTML export as proposed — drill-in views, node-scoped filter and usage |

They are interactive: click the frame, then use the keys. Where this prose and a mock
disagree, the mock is the artifact that was reviewed.

---

## 1. The model: a tree, navigated like a filesystem

A session's sub-agents form a **tree**. The session is the root; each `Agent` spawn is a
child; a sub-agent that spawns its own agents has children of its own (`spawnDepth ≥ 2`).

Navigation is deliberately restricted, exactly like `cd`:

- **down** — into one of the current node's *direct* children
- **up** — to the current node's parent
- nothing else. No sideways jump to an arbitrary node, no global flat list of every agent
  in the session.

Everything scoped follows the current node: the transcript shown, the tool-use filter and
its counts, the token/cost panel, the step outline, and the active-agent list.

**Invariant.** Every node except the root has exactly **one** spawn block in its parent's
transcript, and that block is where *up* returns you. This is free in the implementation
(the node is derived *from* that spawn record) but it is the thing to assert in tests — an
early draft of the mock had an agent whose `from` pointed at an unrelated Bash call, and
the result was silently plausible.

**Why not the alternatives.** Inlining a child's blocks into the parent's flow destroys the
parent's rhythm and is unbounded at depth ≥ 2. A persistent tree pane costs permanent
space for a shallow fan-out. A flat "all agents" list invites exactly the sideways jump the
model excludes — and it was the first thing built, then removed, because in a session with
many agents it stops answering "what did *this* agent spawn?".

### 1.1 Agent ids are URL-like targets

An agent id behaves like a **file path or an attachment**: a target you activate. Focus it
and press `⏎`, or click it — the same two gestures that already reveal a path or download
an attachment. This is the one design decision that keeps the feature from needing new
interaction vocabulary.

It follows that ids are targets **only in events specifically about sub-agents**:

- the spawn event (`Agent` tool_use) and its result rows
- lifecycle state changes (launched / completed / failed)
- messages between the session and a sub-agent

An id that merely *appears* in tool output, in prose, or inside a file path is **plain
text** — or, if it is a path, an ordinary path target. `Read(subagents/agent-9d41c7f2.meta.json)`
reveals a file; it does not navigate to an agent. Both cases are in `tui-mock.html`: the
lifecycle line's id navigates, while the same id inside `tool-results/agent-6f2c1b9e.output`
on that very row is a path.

Because activation is generic, no affordance text is needed — no inline "open", no
per-target wording in the hint row.

---

## 2. TUI

Minimalist by intent, and an **intentional deviation from the HTML version**: no breadcrumb
bar, no peek box, no meta panel. The TUI is a full-screen transcript plus a one-line
footer, with overlays drawn over it — the feature adds nothing to that structure.

### 2.1 Spawn block

Collapsed, it is an ordinary tool line in the existing `Name(arg)` shape, so it needs no
new render path — only a hue:

```
⏺ Agent(code-reviewer: Review the command_name rewrite)  4 tools · 22s
```

Marker and `Agent` take a new **agent hue** (`Indexed(141)`), deliberately not the tool
green, so a spawn reads as a different class of event while scrolling. Parallel spawns
coalesce as they do today:

```
⏺ Agent ×2 (parallel)  6 tools · 1m 12s · running
```

Expanded, it adds the prompt and result exactly as a `tool_use`/`tool_result` pair renders,
plus **one selectable row per agent, keyed by id**:

```
⏺ Agent(general-purpose: Audit panic-safety on malformed input)
  ⎿  Audit the three render modules for panic-safety against malformed JSONL…
  ⎿  ⏺ agent-9d41c7f2   general-purpose   6 tools · 1m 12s
  ⎿  Audited model.rs / render.rs / markdown.rs against truncated JSONL, invalid
     UTF-8, and 100MB lines. No panics; two unwraps hardened.
```

Expanding pre-selects the first agent, so the promised second `⏎` descends with no
hunting. `]`/`[` already walk foldables, so agent rows simply join that walk — **selection
needs no new mechanism**. A running agent's row marker animates; a completed one is `⏺`.

### 2.2 Descending and returning

`⏎` on an agent row opens that agent's transcript in place. A click is focus + `⏎`, so one
click on an id opens it.

`esc` (or `⌫`) returns, landing the cursor **on the spawn block** rather than at a
remembered scroll offset — that is what makes descending feel free, since you always resume
where the decision was made. Two properties matter and were both bugs before they were
features:

- **Returning must not mutate fold state.** A spawn block you left collapsed comes back
  collapsed. The expand-ancestors walk has to start at the target's *parent*.
- **Controls inside a header own their click.** The header's fold-toggle branch must skip
  clicks originating on an agent id and return, because `stopPropagation` cannot undo a
  branch that already ran in the same listener.

Note `Esc` currently means `Outcome::Back` (return to the session picker). Inside a
sub-agent it must ascend first, and only fall through to that at the root — a rebind of an
existing key, not a new one.

### 2.3 Active sub-agents

A hotkey (`a`) opens a one-line-per-agent popup of **active** sub-agents — spawned, no
result yet — **of the current node**. Not of the session: the list is relative, like
everything else.

The hotkey and its label are **disabled and absent** when the current node has no running
children, which for a finished replay is every node. Precedent: `can_open_picker()` already
gates `s` and its `?` help line on `--latest`.

### 2.4 The footer, and why nothing was added to the hint row

The real footer is one dim dot-separated line whose key hints are **glyphs only** —
`?·[ ]·↵/·n·g·q` — and it is already full. So sub-agent navigation puts **nothing** in the
glyph run. Both affordances are *status*, and live in the left run that already carries
location · live · model · tokens:

```
↑ esc back · a active 1 · agent-9d41c7f2 · general-purpose · live · opus4.8 · …   ?·[ ]·↵/·n·g·q
```

- `esc back` — appears only when descended; clicking it ascends.
- `a active 1` — appears only when this node has a running child; clicking it opens the
  popup.

Each label is **both the click target and the hotkey hint** — key then words — which is
precisely why neither needs a slot in the glyph run. Both are dim with a dotted underline,
so the row gains no second colour. Descending also *shortens* the left run (a short agent
id replaces the 36-char session uuid), which pays for the labels.

**The footer must fit its width exactly.** A terminal footer is `width` columns and cannot
overflow, so when the left run does not fit it **sheds segments by priority** — `cached`,
then `%`, model, `in`, `out`, duration, cost — and as a last resort truncates the session
uuid. Location, live-state, and the two navigation labels never drop, and **the key-hint run
is never what loses.** `tui-mock.html` measures its real column count and re-fits on
resize; narrow the window to watch the shedding.

Depth is deliberately *not* displayed. The up label names the parent, so "back to the
session" is depth 1 and "back to agent-9d41c7f2" is depth 2 — a `d1`/`d2` badge was tried
and removed as opaque and redundant.

### 2.5 Architecture — the machinery already exists

`app.rs` has the two pieces this needs:

- **`Outcome::Switch(PathBuf)` + `run_view_loop`** already re-views a different transcript.
  Descend/ascend is that pattern with a **stack**. Keep the parent `View` alive rather than
  re-parsing: `view_session` rebuilds from scratch, so a re-parse would lose both scroll
  offset and fold state, which are exactly what §2.2 promises to restore. Memory is bounded
  by depth, not breadth (only the ancestor chain is retained).
- **`activate_focused()` / `click_at()`** already return an action from the focused block
  (fold, reveal path, download attachment). Widen the return from `Option<PathBuf>` to an
  action enum and add a `Descend(agent)` variant. That is the honest integration point, and
  it is what makes §1.1 true rather than aspirational.

Also needed: a `"agent"` fold key in `FOLD_KEYS` / `--fold` / `--unfold` (the `"skill"` key
is the exact precedent), and `theme::agent()` + an `agent_expanded_bg()` slotted into the
existing background-tier ladder — keeping `background_tiers_are_ordered` green.

`focus_bg` applies to a focused spawn block like any other foldable. `theme.rs` documents it
as "the universal focus cue (colours alone don't suffice)"; excluding spawn blocks and
relying on a text-colour swap was tried, and it was invisible.

### 2.6 Live tail

Children grow in their own files, so the tailer follows each open child by `agentId`.

- At the parent with a child growing: the spawn line's stats update in place; a transient
  `+3` when steps land is enough to notice without stealing attention.
- At the child: normal live-tail behaviour.
- Parent finishes while you are inside a child: reflect it in the footer state. **Never
  auto-navigate** — a view change you did not ask for loses your place.
- `status: async_launched` with a `toolUseResult.outputFile`: the result slot reads
  `awaiting async result · tool-results/agent-<id>.output` until the file appears.

Scope cut: poll the current node and its ancestor chain; do not tail unopened children.
Their spawn line shows the parent's own status until you descend.

### 2.7 Degradation

- Child file missing (older session, a `.jsonl` copied without its `subagents/` dir) → no
  agent rows, no descent, parent summary only. Never a dead affordance.
- `spawnDepth ≥ 2` is unconfirmed on disk. If a grandchild resolves by the same rule it
  becomes a child node and navigation recurses; if not, render depth 1 as today. Nested
  support must not block shipping.

---

## 3. Dump modes: no change

`--dump` and `--dump-html` render the `Agent` event **exactly as it appears in the
transcript** — no drill-down, no child sections, no `⇱` affordances.

This is a decision, not an omission. Those outputs are portable artifacts; a shared
`--dump-html` has no access to the `subagents/` directory, so any affordance it emitted
would be dead on arrival. Sub-agent navigation is a property of the interactive viewer
(TUI and served `--html`), which can read the child files.

---

## 4. HTML export

`html-export-mock.html` mirrors §1 with browser-native affordances. It is the richer of the
two by design — a browser has room the terminal does not.

- **`↓ Children N`** in the top bar: the picker, scoped to the current node, disabled on a
  leaf.
- Descending swaps the transcript for that node's section **in place**, so the export stays
  one file. A sticky bar carries `▲ up`, the clickable path, and the child count.
- Each node remembers its own scroll offset; the sidebar swaps its turn list for the node's
  step outline.
- The **tool-use filter, its counts, and the usage panel are node-scoped**; the root
  additionally aggregates own cost + descendants + total.
- **Open in a new tab** (`⧉`, or `⇧⏎`): opens `#agent=<id>` with a stable window name, so a
  second click re-focuses the existing tab instead of duplicating it. A tab with an
  `opener` returns focus to it and closes rather than rendering a second copy of the
  parent.
- Deep links: `#agent=<id>` lands on a node; a block link resolves its owning node first.

---

## 5. Build order

1. **Model** — `Block::SubAgent { agent_id, agent_type, description, status, blocks, result,
   children }`; discovery + join via `toolUseResult.agentId`; `parse_main` reuse for
   children; subtree cost rollup.
2. **TUI render** — spawn block collapsed/expanded with agent rows; `theme::agent()`;
   `"agent"` fold key. No navigation yet.
3. **TUI navigation** — the View stack, `⏎` descend, `esc`/`⌫` ascend with
   cursor-restore-on-return, the action enum from `activate_focused()`/`click_at()`.
4. **TUI footer** — the two labels, fit-and-shed, clickable regions (note `click_at()`
   currently early-returns for rows past `content_rows()`).
5. **Active-agents popup** — `a`, node-scoped, gated like `can_open_picker()`.
6. **Live tail** for children.
7. **HTML export** — node sections, `↓ Children`, node-scoped filter/usage, `⧉` tabs, hash
   routing.
