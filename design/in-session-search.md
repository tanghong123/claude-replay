# In-session search: one query, one facet tree

> **Status: DECIDED (owner, 2026-09-26), not built.** Task #137. §2 is the owner's decisions; the
> rest — the `tool:` token, the count wording, "a tool implies its class", the sort order — works
> them out and is open to amendment until the implementation starts. The implementation is filed
> as its own tasks (see §7). Related: `design/global-search.md` (#138), the ⌘K box that shares this
> grammar.

## 1. The problem

The top bar's search grew three axes, and nothing said how they combine:

- a **text query**, narrowed by **scopes** typed as a `uatobrew:` prefix (`shared/search.js`,
  the TUI's `/` grammar) or ticked in the popover;
- **tool types** (#110), which began as "cycle through the calls of one type" and became a
  search by kind (#133): they mark and step, and hide nothing on the app shell.

The code answered the combinations by accident. `activeMatches()` (app.js) returns the text
matches when there is a query and the tool filter's matches otherwise, so **typing a query
silently replaces the tool filter**. Scopes narrow the text but never the tool filter, and a
scope with no text does nothing. In the popover that reads as one set of filters with two
opposite rules: the top half needs text, the bottom half is ignored once there is text. The
count changes words between the two ("N hits in ub", "N matches") and never says which one won.

## 2. The decisions

1. **Everything narrows.** A record matches when it satisfies the text AND the scopes AND the
   tool types. Several tool types are ORed with each other (a facet's values OR; facets AND —
   the model every log viewer uses). A query plus "Bash" means Bash calls whose text matches.
2. **Scopes and tool types are one facet tree.**
   `Messages → User, Agent, Thinking` and `Tools → Bash, Read, Edit, …, MCP → server → tool`.
   Choosing "Tools" is today's `o:`; choosing "Bash" is a refinement inside it. The MCP grouping
   (#120, row 3.16 of `rendering-parity-audit.md`) is this tree's MCP branch.
3. **One grammar, typed or clicked.** The box stays the truth (#101): every facet has a typed
   form, the popover writes it into the box, and typing it ticks the popover. The ⌘K box
   (`global-search.md`) parses the same grammar.
4. **Cross-session content search is on hold** (see `global-search.md`).

## 3. The grammar

Backward compatible with every query that parses today:

| form | meaning | notes |
|---|---|---|
| `word…` | text, case-insensitive, ≥ 2 characters (`MIN_NEEDLE`) | unchanged |
| `uatobre:` prefix | message/tool classes, as today | unchanged; the TUI keeps it |
| `w` in the prefix | whole words | unchanged in the grammar; see §5 for the control |
| `tool:Name` | calls of one tool, exact name | new; repeatable, values OR |
| `tool:mcp__server__*` | every tool of an MCP server; `tool:mcp__*` every MCP call | new; the prefix form, as the classic page's `[data-tool^=]` |
| leading `:` | escape a scope-shaped literal | unchanged |

`tool:` tokens may sit anywhere in the box; the prefix run stays first. The value is the name a
record's `tool` field carries — the server's display name (an Edit reads `Update`), the same on
both pages. A box holding only `tool:` tokens (no text) is a valid query: it matches every call
of those tools — today's tool filter, typed.

**Amended while implementing (2026-09-26):** a box holding only a scope prefix is NOT a facet
query. `auto:` alone searches the literal text `auto:`, as it always has — the TUI shares this
parser and the rule is load-bearing — so "scopes alone" is out. And text shorter than two
characters beside a `tool:` token runs nothing until the text is empty or long enough, rather
than being silently dropped.

## 4. What each combination means

| text | classes | tools | matches | count reads |
|---|---|---|---|---|
| — | — | — | nothing (no query) | empty |
| ✓ | — | — | records whose text matches | `N matches` |
| ✓ | ✓ | — | …inside the chosen classes | `N matches in ub` |
| — | ✓ | — | every record of the chosen classes | `N in ub` |
| — | — | ✓ | calls of the chosen tools | `N Bash` / `N in 3 tools` |
| ✓ | — | ✓ | calls of the chosen tools whose text matches | `N matches in Bash` |
| ✓ | ✓ | ✓ | the chosen tools' calls, within the classes, whose text matches | as above |

A tool implies its class: choosing Bash with `u:` ticked is an empty intersection, so ticking a
tool ticks Tools, and unticking Tools unticks its tools. ↑/↓ always step the rows of the combined
query, in document order, and the count is always that query's count, in one vocabulary
("matches"; "hits" goes). One count, one step list, no mode that another silently displaces.

**What a match SHOWS is still each page's own** (a deliberate divergence, #133): the app shell
marks and lands and hides nothing; the classic page's tool filter keeps its cut. The shared
modules own what MATCHES (`shared/search.js` for text and classes, `shared/filter.js` for tools
and the chain walk); the pages own the presentation.

## 5. The controls (app shell)

**Amended while implementing (2026-09-26).** §2's sketch names two sections, Messages and Tools,
and the seven scope classes do not fit in it: `b:` (bash output), `r:` (reads) and `e:` (edits) are
TEXT classes, not tools — `b:` counts what a Bash call printed, which is not the same question as
"this is a Bash call". Dropping them to make the sketch literal would remove a capability nobody
asked to lose. So the popover keeps its two existing sections — **Scope** (the seven classes, with
All) and **Tool types** (with None) — and the tree is the Tool types section: each tool with its
count, then MCP → server → tool. Whether the two sections should become one tree, with the classes
as a level above the tools, is left for the owner; nothing here forecloses it.

- **The popover is the tree**, each row with its count for the current text: Messages and its
  three classes, Tools and its tools, MCP and its servers and tools. Tools sort by count,
  most-used first, ties alphabetical; built-ins before the MCP branch. An MCP row reads
  `server/tool` (the single-tool server rule the classic page already has), the full name as its
  tooltip.
- **Whole words leaves the tree** and becomes a toggle beside the box (it is a match option, not
  a part of the transcript). The `w` letter keeps working in the typed prefix.
- **The filter button's badge counts every active facet**, classes included — today a scope
  narrows the results while the button reads as inactive.
- **The letter hints** in the scope rows stay (they teach the typed form) but read as the typed
  token, e.g. `u:`, not as keyboard shortcuts.
- The dead demo markup (`findTranscriptBtn`, `findTranscriptPanel`, `filterTranscriptPanel`)
  goes. `reference-shell.html` is generated: remove it in `design/agent-monitor-codex-demo.html`
  and re-run `scripts/extract-agent-monitor-demo.mjs`.

**Decided against, and held** (owner, 2026-09-26): active facets as chips inside the box are NOT
wanted — a filter "needs more direct exposure" than a token in a text field, which is what the
popover and its badge are for. The "only matches" toggle is on hold.

What §3 keeps is the typed FORM of a facet, not a chip: a reader may type `tool:Bash`, and ticking
a row writes that token into the box the way ticking a scope already writes `ub:` there (#101). The
popover stays the control.

## 6. The classic page

It is the reference page and parses the same grammar. The combination rule (§4) applies to it
too, through the shared modules; its tool filter remains a cut rather than a mark, and its menu
keeps its own markup. Where the two pages still differ after this, the difference is in what a
match shows, never in what matches.

## 7. Implementation (filed as tasks)

1. **The shared query model** — `tool:` in `splitQuery`, facets ANDed with text, one count and
   one step list, on both pages; the TUI's `/` keeps parsing everything it parses today.
2. **The app shell's facet tree** — the popover of §5, including the MCP branch (#120 folds into
   this), whole words beside the box, the badge, the dead markup.

Each with a scenario on both pages (`claude-replay-browser-tests/tests/scenarios.rs`), confirmed
red on the old code, and unit cases for the parser and the combination table in the node contract.
