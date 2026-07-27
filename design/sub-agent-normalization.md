# Design: normalize sub-agents into a Session metadata map

> **Status:** proposed (not built). A public data-model change, output-preserving, gated on
> byte-identical diffs. Tracked as task #20. Pairs with the lazy **session cache** (task #21,
> [`session-cache.md`](line-reader-and-session-builder.md#layer-3--sessioncache)) that owns
> the parsed child transcripts. Supersedes `SessionIndex.agents` / `AgentEntry`.

## Problem

A spawned sub-agent is one **entity** with an identity (`agent_id`), but the transcript
expresses it through several **events**, and today the entity's data is smeared across three
places:

- `Block::SubAgent(SubAgent)` — the *spawn* event, which also **owns** the child's parsed
  `blocks: Vec<Block>` (when enriched) plus every attribute.
- `Block::AgentDone { agent_id, agent_type, description, status, result }` — the *completion*
  event, which **duplicates** id/type/description/status/result.
- `SessionIndex.agents: Vec<AgentEntry>` — a third, derived copy.

So the entity has no single owner; one block variant owns a whole recursive sub-tree; and the
same fields live in three shapes.

## Proposal

Two moves, kept strictly separate:

1. **A `Session` corresponds to exactly one transcript — flat, no nesting.** It never embeds a
   child `Session`. This keeps it cheap to clone (important for the live `snapshot()` path) and
   makes "one transcript = one Session" a clean invariant.
2. **Model a sub-agent as an entity stored once, keyed by id, holding only *metadata* + the
   *paths* to its external artifacts** (its transcript, its async output file). The block
   stream carries only references (ids). **Parsing a child is not the map's job** — it's the
   [session cache](line-reader-and-session-builder.md#layer-3--sessioncache)'s, lazily, on
   request (task #21).

```rust
/// Identity of a spawned sub-agent within a session (a transparent alias for now; a newtype
/// later if we want the compiler to stop it mixing with other id strings).
pub type AgentId = String;

pub struct Session {
    pub agent: Agent,
    pub cwd: Option<PathBuf>,
    pub blocks: Vec<Block>,
    pub user_times: Vec<Option<EpochSeconds>>,
    pub metrics: Metrics,
    pub index: SessionIndex,
    /// The sub-agents spawned in this session — **metadata only**, one entry per id. The
    /// single owner of every sub-agent's attributes and the *pointers* to its artifacts; it
    /// does **not** hold the child's parsed transcript. Blocks reference these by id.
    pub sub_agents: BTreeMap<AgentId, SubAgent>,   // NEW
}

/// A spawned sub-agent — the entity's **metadata**, stored once per id in
/// [`Session::sub_agents`]. Purely descriptive + the paths a cache needs to load the child on
/// demand; it never contains the parsed child (that lives in the session cache, task #21).
pub struct SubAgent {
    pub agent_type: String,          // Claude's free-form `subagent_type`
    pub description: String,
    pub prompt: String,
    pub status: AgentStatus,         // the terminal-or-running truth
    pub result: Option<String>,
    pub subtree_cost: Option<UsdCost>,
    /// Index of this sub-agent's `SubAgentSpawn` block in the parent's `blocks` — the jump
    /// target (replaces `AgentEntry.at`).
    pub spawn_at: BlockIndex,
    // ── pointers to external artifacts (for the cache to load lazily; never parsed here) ──
    /// The child transcript on disk (`agent-<id>.jsonl`), if it exists.
    pub transcript: Option<PathBuf>,
    /// The async result sidecar (`tasks/agent-<id>.output`), if any.
    pub output_file: Option<PathBuf>,
}

pub enum Block {
    // …
    /// Marks where a sub-agent was launched; renders by looking the entity up in
    /// `Session.sub_agents[agent_id]`. (Was `SubAgent(SubAgent)`.)
    SubAgentSpawn { agent_id: AgentId },
    /// Marks where a sub-agent's completion notification arrived; renders by looking up the
    /// entity. (Was `AgentDone { agent_id, agent_type, … }`.)
    AgentDone { agent_id: AgentId },
    // …
}
```

This **supersedes `SessionIndex.agents`**: the map is the source of truth; `active_agents()`
and `agent(id)` become map operations, and `spawn_at` is the jump target. `SessionIndex` keeps
the purely-positional indices (turns / tools / attachments / counts).

## How a client descends into a child (the split of duties)

The map gives the client the child's *identity and path*; the [session
cache](line-reader-and-session-builder.md#layer-3--sessioncache) turns that into a live
`Session` **only when asked**:

```text
parse root ─▶ Session { …, sub_agents: {id → SubAgent{ transcript, … }} }
client wants child `id`:
   cache.register(id, &sub_agents[id])   // cheap: id + artifact paths, no I/O
   cache.get(id)                         // 1st call: parse transcript → Session
   cache.get(id)  (later)                // tail new events, update, return current Session
```

So the data model stays a flat value; materialization, liveness, and residency are the cache's
concern (task #21) — which is exactly today's `html_export::serve.rs` behaviour, lifted to a
reusable primitive.

## Payoff

- **One owner** per sub-agent — no attributes duplicated across spawn / done / index.
- **`Session` is flat and cheap to clone** — the recursion/clone-cost problem disappears; a
  `Session` is one transcript, always.
- **Lazy + live children** — the client pays to parse a child only when it opens it, and gets
  incremental updates on re-open, via the cache.
- **O(1) id lookup**; the map replaces the linear `Vec<AgentEntry>`.

## Design decisions to settle before building

1. **Keying + late ids.** Async spawns assign `agent_id` *after* the spawn; the fold joins via
   `tool_use_id` and back-patches `agent_id` in `apply_completions_and_suppress` — the map is
   finalized there. Decide the key for a spawn whose id never resolves (fall back to
   `tool_use_id` / a synthetic key); blocks must reference whatever key the map uses.
2. **Renderers consult the map.** `render.rs`, `html_export`, and the viewer render a
   `SubAgentSpawn`/`AgentDone` by looking up `session.sub_agents[id]`, so their signatures gain
   access to the map (or the `&Session`). Byte-identical: same output, different source.
3. **`subtree_cost`: stored, not derived.** Without an embedded child `Session` it can't be
   derived locally; the enricher/cache computes it (summing the child's metrics + descendants)
   and stores it on the meta, or leaves `None` when the child isn't loaded.
4. **Discovery of the paths.** `transcript` comes from `discover::subagent_source` (Claude's
   flat `subagents/` layout); `output_file` from the spawn's `toolUseResult`. Codex has no
   sub-agents → empty map.

## Migration

Output-preserving but broad: `model` (block variants + the meta entity + the `Session` field)
→ parse (`Shaping`/`Replayer` + `claude_model` build the meta map; **stop** enriching child
blocks into the model) → `index` (drop `agents`) → `render`/`html_export`/`view`/`app` (look up
the map; descend via the cache) → `jdi`. Implement as a dedicated milestone with task #21
(the cache), gated on the `--dump`/`--dump-html` byte-identical diffs (both agents) and the
sub-agent unit tests.

See [`docs/architecture.md`](../docs/architecture.md) §4 for the current data model this
refines, and [`line-reader-and-session-builder.md`](line-reader-and-session-builder.md) for the
`LineReader` → `SessionBuilder` → `SessionCache` stack the cache sits atop.
