# Sub-agent drill-down in the HTML export — with the live feed

Design proposal for build-order step 7 in `README.md` §5 (HTML export). The TUI
(steps 1-6) is shipped; this covers the HTML exporter only: `--dump-html` (static
file), `--html` (served), and the `-f` live companion feed. The reviewed target UX is
`html-export-mock.html`; where this doc and the mock disagree, **the mock wins**.

Two facts frame everything below:

1. The mock (`tree()`, `navigate`, per-node `.agentview` sections) renders **all
   nodes' sections up-front in the DOM** and just toggles `display`. Our page is
   **JS-rendered from an append-only JSONL stream** — nothing is in the DOM until a
   record for it arrives.
2. The live `-f` path **re-parses the whole tree every poll** (`snapshot` →
   `parse_path_timed_for` → `parse_path` → `enrich_subagents`, `html_export.rs:779`,
   `model.rs:618-693`) and reconciles it against the page via a **positional diff +
   `{t:"reset",from:N}`** protocol (`follow_and_append`, `html_export.rs:941-981`;
   `resetFrom`, `export.js:406-413`). Node navigation has to ride on top of that
   without a full re-render and without losing the viewer's node + scroll.

So the whole job is: **turn the mock's static node tree into records on our stream,
and make the reset protocol node-aware.**

---

## 1. How the mock's node model maps onto our stream + reset architecture

| Mock (static React DOM) | Our port (streamed) |
|---|---|
| `tree()` — hardcoded JS object | a `{t:"tree", nodes:[…]}` **stream record** emitted by Rust, refreshed each poll |
| node id `a1`, `a1a` … | the real **`agent_id`** (root = `"root"`) — so `#agent=<id>` is a real target |
| `#transcript` + N pre-rendered `.agentview` divs | one lazily-created **`#node-<id>` container** per node; root's is today's `#stream` |
| every block already in its section | each **block record carries a `node` field**; JS routes it into `#node-<node>` |
| `node.from` = spawn block id | the parent **spawn block's id**, emitted on the child `NodeMeta` |
| `node.usage` literal | **per-node usage** rolled from the child transcript's metrics, on `NodeMeta` |
| `navigate/up/openTab/applyHash/countTools/renderUsage/renderSteps` | **ported almost verbatim** — they already scope every query to `#<view>`; only the container id changes (`#node-<id>`) and `this.T` is fed by the `tree` record instead of a literal |

The mock's navigation code is essentially reusable. The work is (a) Rust emitting the
`tree` record + `node`-tagged blocks in a stable, diff-friendly way, and (b) making
`consume`/`reset` route and reconcile per node.

### Block ids must become per-node (the load-bearing change)

Today `block_id()` is a single global counter in emission order (`html_export.rs:393`).
That is safe only because live transcripts **append at the end**. With sub-agents, a
**still-running child sits in the middle of pre-order** and grows every poll — which
under a global counter shifts the id of every block after it, breaking deep-link
anchors and forcing needless re-renders.

**Decision: id = `"<node>-<n>"`** (`root-7`, `agent-9d41c7f2-3`), `n` monotonic
*within the node*. A node's own transcript only appends (its tail may rewrite — that's
the reset's job, scoped to that node), so these ids are stable across polls even while
a sibling or child grows. This is what makes both the deep links and the per-node reset
(below) correct.

---

## 2. Rust emitter changes (`html_export.rs`)

### 2.1 The node-tree record

```rust
/// One node of the sub-agent tree — streamed as a `{t:"tree", nodes:[…]}` record and
/// refreshed each live poll. The JS builds `this.T` and each node's section header
/// (title, agent-file, depth, prompt-from-parent card, ↓ Children button) from these.
#[derive(serde::Serialize)]
struct NodeMeta {
    id: String,               // agent_id; the root node is "root"
    parent: Option<String>,   // None only for root
    from: Option<String>,     // parent's spawn block id — the ⌫/esc return target
    label: String,            // description (root: the session title)
    agent_type: String,       // "general-purpose" … (root: "")
    status: &'static str,     // AgentStatus::label(): "running"|"done"|"failed"|…
    depth: usize,             // 1 for a direct child (for "depth 1 · sync")
    async_launched: bool,     // "· async" vs "· sync" in the header
    agent_file: Option<String>, // basename agent-<id>.jsonl (served: full path for copy)
    prompt: String,           // spawn prompt → the "Prompt from parent" card
    turns: usize,             // child turn / tool counts for the section header
    tools: usize,
    kids: Vec<String>,        // direct children, in spawn order
    usage: NodeUsage,         // node-scoped in/out/cache/cost (own; JS rolls up subtree)
}

#[derive(serde::Serialize)]
struct NodeUsage { input: String, output: String, cache_read: String, cost: f64 }
```

`usage`/`turns`/`tools` come from the same `metrics::parse_reader_for` +
block-count logic `snapshot` already runs for the root (`html_export.rs:781-806`),
applied to each child transcript. `subtree_cost` already exists on `SubAgent`
(`model.rs:141`) — the JS aggregates descendants from `NodeUsage.cost`, matching the
mock's `subCost`.

### 2.2 Pre-order flatten + node tagging

`block()` returns one `Value`; children need many lines. Replace the per-block push in
`build_jsonl` (`html_export.rs:659-668`) with a recursive emitter:

```rust
struct Emitter<'a> {
    // …existing fields…
    node: String,                          // current owning node ("root" | agent_id)
    per_node_seq: HashMap<String, usize>,  // stable per-node id counter
    nodes: Vec<NodeMeta>,                  // tree, in first-seen (pre-order) order
    node_stack: Vec<(String, usize)>,      // (id, depth) — for `depth`/`parent`
}

impl Emitter<'_> {
    fn block_id(&mut self) -> String {
        let n = self.per_node_seq.entry(self.node.clone()).or_insert(0);
        *n += 1;
        format!("{}-{}", self.node, n)          // "root-7", "agent-9d41c7f2-3"
    }

    /// Emit `b` as one line into `out` (tagged `node`), then — for a SubAgent —
    /// record its child NodeMeta and recurse over `sa.blocks` under the child node id.
    /// A pre-order flatten of the whole agent tree into the append-only line stream.
    fn emit_block(&mut self, b: &Block, ts: Option<f64>, out: &mut Vec<String>) {
        let mut v = self.block(b, ts);                 // existing per-block JSON
        v.as_object_mut().unwrap()
            .insert("node".into(), json!(self.node));  // ← route tag
        let this_id = v["id"].as_str().unwrap().to_string();
        out.push(v.to_string());

        if let Block::SubAgent(sa) = b {
            if sa.agent_id.is_empty() { return; }      // degraded: no child file
            let (parent, depth) = (self.node.clone(),
                                   self.node_stack.last().map_or(1, |(_, d)| d + 1));
            self.nodes.push(NodeMeta {
                id: sa.agent_id.clone(),
                parent: Some(parent),
                from: Some(this_id),                   // the spawn block to return to
                label: sa.description.clone(),
                agent_type: sa.agent_type.clone(),
                status: sa.status.label(),
                depth,
                async_launched: sa.status == AgentStatus::AsyncLaunched,
                agent_file: /* basename; full path when reveal */,
                prompt: sa.prompt.clone(),
                turns: /* count over sa.blocks */, tools: /* … */,
                kids: sa.blocks.iter().filter_map(child_id).collect(),
                usage: node_usage(sa),
            });
            let saved = std::mem::replace(&mut self.node, sa.agent_id.clone());
            self.node_stack.push((sa.agent_id.clone(), depth));
            for cb in &sa.blocks { self.emit_block(cb, None, out); }
            self.node_stack.pop();
            self.node = saved;
        }
    }
}
```

The **existing `Block::SubAgent` arm of `block()` stays** (`html_export.rs:426-443`) —
it still renders the collapsed spawn fold (prompt, one-level peek, result) in the
*parent's* section. We only append the `.agent-open`/`.agent-tab` controls to that
fold's header (a `head` field the JS turns into buttons carrying `data-agent =
sa.agent_id`) and then recurse to emit the child's own section.

`build_jsonl` returns the same joined string, now with a `tree` record after `meta`:

```rust
fn build_jsonl(...) -> (String, Vec<(String,String)>) {
    // …emit_block loop over top-level `blocks`…
    let tree = json!({ "t": "tree", "nodes": em.nodes }).to_string();
    let lines = [vec![meta.to_string(), tree], block_lines].concat();
    (lines.join("\n"), em.turns)
}
```

### 2.3 Keeping `block_lines` / the reset correct — go node-aware

`block_lines` (`html_export.rs:856`) still means "everything after the header records"
— now it skips **two** leading records (`meta`, `tree`), or better, filters to
`"t":"block"` lines so record count is order-independent.

The positional reset (`follow_and_append`) becomes **per node**. Because every block
line carries `node`, the differ groups fresh lines by node and diffs each group
independently:

```rust
/// Group block lines by their `node` field, preserving per-node emission order.
fn lines_by_node(block_lines: &[String]) -> BTreeMap<String, Vec<String>>;
```

```rust
fn follow_and_append(
    …,
    mut prev: BTreeMap<String, Vec<String>>,   // was Vec<String>
    …,
) -> Result<()> {
    loop {
        // …sleep, re-snapshot (unchanged: whole-tree re-parse)…
        let fresh = lines_by_node(&block_lines(&fresh_str));
        let mut out = String::new();
        out.push_str(&tree_line);  out.push('\n');   // refreshed tree every cycle
        for (node, flines) in &fresh {
            let plines = prev.get(node).map(Vec::as_slice).unwrap_or(&[]);
            let d = plines.iter().zip(flines).take_while(|(a, b)| a == b).count();
            if d < plines.len() {                     // this node's tail rewrote
                out.push_str(&json!({ "t":"nreset", "node":node, "from":d }).to_string());
                out.push('\n');
            }
            for l in &flines[d..] { out.push_str(l); out.push('\n'); }  // l carries node
        }
        for node in prev.keys().filter(|k| !fresh.contains_key(*k)) {   // agent vanished (rare)
            out.push_str(&json!({ "t":"nreset", "node":node, "from":0 }).to_string());
            out.push('\n');
        }
        out.push_str(&meta);  out.push('\n');         // refreshed usage/cost/counts
        append_line(companion, out.trim_end())?;
        prev = fresh;
    }
}
```

The old whole-stream `{t:"reset",from:N}` is replaced by the per-node `nreset`. The
common case is still a pure append (no node diverges → no `nreset`). A running child
whose tail coalesces re-emits **only that child's** tail, not the parent's later blocks
— which the old global reset would have thrown away.

---

## 3. JS changes (`export.js`)

### 3.1 Per-node containers + routing

```js
var nodes = {};                     // this.T, fed by the tree record
var rendered = {};                  // node id -> [block els] in emission order
function nodeContainer(id) {        // #node-<id>; root is the existing #stream
  if (id === "root") return stream;
  var c = document.getElementById("node-" + id);
  if (!c) { c = el("div", "agentview"); c.id = "node-" + id; c.style.display = "none";
            document.getElementById("main").appendChild(c); rendered[id] = []; }
  return c;
}
```

`consume` (`export.js:379-402`) gains two dispatch arms and routes `block` by `node`:

```js
if (obj.t === "meta")  { renderMeta(obj);   continue; }
if (obj.t === "tree")  { updateTree(obj);   continue; }     // new
if (obj.t === "nreset"){ nresetFrom(obj.node, obj.from); continue; } // replaces reset
if (obj.t !== "block") continue;
var c = nodeContainer(obj.node);
c.appendChild(renderBlock(obj));
(rendered[obj.node] || (rendered[obj.node] = [])).push(/* the appended el */);
if (obj.node === "root" && obj.turn != null) addTurn(obj);   // sidebar is root-only
```

```js
function nresetFrom(node, from) {                            // per-node reset
  var arr = rendered[node] || [];
  while (arr.length > from) { var last = arr.pop();
    var si = turnlist.querySelector('.side-item[data-t="' + last.id + '"]');
    if (si) si.remove();
    last.remove();
  }
}
```

After each `consume`, refresh the **current node's** derived UI only (its scope
changed): `countTools(); renderUsage(); renderSteps();` — all already scope to the
node's container (§3.3), so they recompute node-scoped counts live for free.

### 3.2 `updateTree`, `navigate`, `up`, `⧉` tabs, breadcrumb, `#agent=` routing

Ported from the mock (`mock_template.txt` `navigate/up/openTab/applyHash/renderKids`),
with three deltas:

- `this.T` is **merged from the `tree` record**, not a literal: `updateTree` upserts
  each `NodeMeta`, creates any missing `#node-<id>` container, updates the current
  node's `↓ Children N` button + breadcrumb + usage if the current node's kids/status
  changed, and **flips the spawn fold's status pip** (running → done) by id.
- `navigate(id)` toggles `#node-<id>` visibility (mock: `.agentview` by `t.view`);
  root shows `#stream` + the turn sidebar, a child shows its step outline. Scroll
  memory (`this.scrollMemo[node]`) and `history.replaceState('#agent='+id)` are
  unchanged from the mock.
- **Deferred hash** (the spawn-later case): `applyHash` stores `pendingHash = id` when
  `!this.T[id]`; `updateTree` calls `applyHash()` again after a merge, so a shared
  `#agent=<id>` opened before that agent spawns lands the moment it appears. A static
  page with no such node simply never resolves (matches the mock).

`openTab`/satellite/opener logic (`mock_template.txt` `openTab`, `up`, `applyHash`) is
verbatim: `window.open(url, 'claude-replay-agent-'+id)` for one-tab-per-agent, opener
focus-and-close on `up` at the entry node.

### 3.3 Node-scoped filter / usage / steps (already node-shaped in the mock)

`countTools`, `renderUsage`, `renderSteps`, `setFilter` all scope their queries to
`document.getElementById(this.T[this.node].view)` — swap that for
`nodeContainer(this.node)`. `buildToolMenu` (`export.js:418`) still enumerates the
**union** of tools across the page (so the menu is stable), but the per-item counts and
the applied filter are node-scoped exactly as the mock does it (`countTools`,
`setFilter`). No new logic — just the container swap.

---

## 4. The live-feed update strategy (the central section)

Data flow for one poll cycle, live `-f`:

```
transcript.jsonl (+ subagents/agent-*.jsonl grow on disk)
      │  POLL_MS
      ▼
snapshot(): parse_path_timed_for → parse_path → enrich_subagents      [model.rs:618]
      │  whole tree re-parsed; each SubAgent.blocks re-filled from its child file
      ▼
build_jsonl(): pre-order emit_block → flat lines, each tagged `node`,  [§2.2]
               + refreshed `tree` record (per-node usage/status/kids)
      ▼
follow_and_append(): group by node, per-node positional diff,          [§2.3]
      │  emit: tree line · {nreset,node,from}? · node tail lines · meta
      ▼ append to companion .jsonl
page fetch(src) every POLL_MS  →  consume(text)                        [export.js:563]
      │  meta→renderMeta · tree→updateTree · nreset→nresetFrom · block→route to #node-<id>
      ▼  then countTools/renderUsage/renderSteps for the CURRENT node only
DOM: current node visible & scroll preserved; other nodes updated off-screen
```

The three live events, and how each flows through without a full re-render or losing
place:

- **A new agent spawns mid-session.** The whole-tree re-parse produces a new `SubAgent`
  block in the parent + a new child subtree. `emit_block` gives the parent one new
  spawn-fold line (a pure append to the parent node → no `nreset`) and a new
  `NodeMeta`. `updateTree` creates the `#node-<id>` container off-screen and, if the
  viewer is *on the parent*, bumps its `↓ Children N` and adds the agent to the `a`
  menu. The viewer's node and scroll are untouched. If the viewer is *inside* the new
  agent's parent looking at an earlier block, nothing moves.

- **An open/earlier child's transcript grows.** That child's re-parsed `blocks` produce
  new (or rewritten-tail) lines under its node. The per-node diff catches divergence
  **only in that node's group** → an `nreset(node, d)` + the child's tail, appended to
  `#node-<child>`. Crucially this does **not** disturb the parent's later blocks (the
  old global reset would have). If the viewer is *in that child*, `nresetFrom` drops
  only its rewritten tail and re-renders it; the "follow the bottom" logic
  (`export.js:569-578`, `atBottom`) keeps them pinned to the live end if they were
  there, else shows the `↓ N new` badge. If the viewer is *elsewhere*, the growth lands
  off-screen and only the badge/usage update.

- **An agent completes (status flips).** `enrich_subagents` already promotes
  `AsyncLaunched → Completed` (`model.rs:686-688`) and the completion notification sets
  the terminal status (`model.rs:1200-1206`). The refreshed `tree` record carries the
  new `status`; `updateTree` flips the spawn fold's pulse pip to a solid dot by block id
  and updates the breadcrumb/`a`-menu row — **no navigation, ever** (README §2.6: "a
  view change you did not ask for loses your place"). The child's `result` block arrives
  as a normal appended line in the child node.

**Why per-node reset, not the global one.** With a flat pre-order stream, a running
child in the *middle* diverges every poll; a single global `from` would sit at that
child and force re-render of everything after it (later siblings, parent tail). The
per-node diff localizes the churn to the node that actually changed. It also composes
with the stable per-node ids (§1): a block's id never shifts because a *sibling* grew.

**Does the whole-tree re-parse fight the diff?** No — it's the same "re-parse, re-emit,
positional-diff" contract the flat design already relies on (`follow_and_append`
docstring, `html_export.rs:934-940`); we've only partitioned the diff by node. Cost is
one extra grouping pass (`lines_by_node`) over lines we already produce.

**Node-scoped counts/usage recompute** by re-running `countTools`/`renderUsage` after
each consume against the current node's container; the numbers themselves ride the
`tree` record and the `data-tool` attributes already in the DOM, so no server-side
recount is needed.

---

## 5. Phased build plan (each phase shippable)

- **Phase A — static drill-down (`--dump-html`, `--html` without `-f`).** Rust:
  per-node ids, `node` tag, `emit_block` recursion, the `tree` record, `NodeMeta`
  (usage/status/from). JS: `updateTree`, `nodeContainer`, block routing, `navigate`/
  `up`/breadcrumb/`↓ Children`/`a`-menu, node-scoped `countTools`/`renderUsage`/
  `renderSteps`, `#agent=` hash routing, `⧉` tabs. **No reset changes** — a static
  snapshot never resets. This alone delivers the full mock UX for finished sessions and
  for a live-served page's initial load. Gate: `TestBackend` has no bearing here;
  extend the existing `html_export.rs` unit tests (`stream(...)`) to assert the `node`
  tag, per-node ids, and the `tree` record shape.

- **Phase B — live feed (`-f`).** Replace the global reset with the per-node diff:
  `prev: BTreeMap<String,Vec<String>>`, `lines_by_node`, `nreset`. JS: `nresetFrom`,
  the deferred-hash resolve in `updateTree`, refresh current-node derived UI per poll.
  Gate: a unit test that spawns a child mid-"session" (two `build_jsonl` snapshots) and
  asserts the diff emits an `nreset` scoped to the grown node only; the `tmux`/served
  smoke path stays green.

- **Phase C — polish / efficiency (optional).** Lazy child loading for served `--html`
  (a `/__node?id=` endpoint so unopened children aren't inlined — see §6); the
  transient `+N` spawn-line badge for a growing unopened child (README §2.6); async
  `outputFile` "awaiting async result" slot in the child section (README §2.6, mock
  shows it).

---

## 6. Open questions to resolve before building

1. **Eager vs lazy child inlining (memory).** Phase A inlines *every* child transcript
   into the one self-contained file — total size ≈ sum of all transcripts in the tree
   (the same bytes the TUI reads, but all resident in one page and, at load, in
   `#session-data`). For a deep swarm that can be large. Options: (a) **eager** —
   simplest, keeps `--dump-html` fully portable and offline, matches the "one file"
   promise (my recommendation for A); (b) **lazy for served `--html` only** — emit
   child sections on first descend via a new loopback `/__node?id=` endpoint
   (`serve_connection`, `html_export.rs:1110`), keeping `--dump-html` eager. Which
   matters more for your sessions — portable size, or served responsiveness?

2. **Per-node reset vs. a simpler global reset for Phase B.** Per-node (§2.3/§4) is more
   code but is the correct model once a running child sits mid-stream — it stops a
   growing child from re-rendering unrelated parent/sibling blocks and keeps deep-link
   ids stable. The alternative (keep one global positional reset over the flat
   pre-order stream) is a smaller diff but re-renders a large tail whenever any
   non-last node grows. I recommend per-node; confirm you're happy paying that
   complexity in Phase B rather than shipping the cheaper global reset first.

3. **`--dump-html` scope — README §3 says "no drill-down".** README §3 currently states
   the *dump* modes render the Agent event flat with **no** child sections, because a
   shared file "has no access to the `subagents/` directory." But the exporter parses
   children into `SubAgent.blocks` *before* export (`parse_path`), so a `--dump-html`
   file **can** carry the whole tree inline and be fully navigable offline — which the
   mock's own design (one self-contained file) assumes. This is a direct contradiction
   between README §3 and README §4 + the mock. **Which wins?** I've assumed §4/the mock
   (drill-down in `--dump-html` too, since the data is already inlined and self-
   contained); if §3 is the real intent, Phase A ships drill-down for served `--html`
   only and `--dump-html` keeps the flat spawn fold.
</content>
</invoke>
