# Design: normalize sub-agents into an entity map on `Session`

> **Status:** proposed (not built). A public data-model change, output-preserving, gated on
> byte-identical diffs. Tracked as task #20. Supersedes the current `SessionIndex.agents`
> (`Vec<AgentEntry>`) and the recently-added `AgentEntry` field docs.

## Problem

A spawned sub-agent is one **entity** with an identity (`agent_id`), but the transcript
expresses it through several **events**, and today the entity's data is smeared across three
places:

- `Block::SubAgent(SubAgent)` — the *spawn* event, which also **owns** the child's parsed
  `blocks: Vec<Block>` (when enriched) plus every attribute (agent_type, description, prompt,
  status, result, output_file, subtree_cost).
- `Block::AgentDone { agent_id, agent_type, description, status, result }` — the *completion*
  event, which **duplicates** id/type/description/status/result.
- `SessionIndex.agents: Vec<AgentEntry>` — a third, derived copy of the same attributes.

So the entity has no single owner; one block variant owns a whole recursive sub-tree; and the
same fields live in three shapes. That's a normalization failure.

## Proposal

Model a sub-agent as an **entity stored once**, keyed by id, on `Session`. The block stream
carries only **references** (foreign keys) to it.

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
    /// The sub-agents spawned in this session, one entry per id — the single owner of every
    /// sub-agent's attributes and (optionally) its parsed transcript. Blocks reference these
    /// by id; grandchildren live in each child's own `sub_agents`.
    pub sub_agents: BTreeMap<AgentId, SubAgent>,   // NEW
}

/// A spawned sub-agent — the entity, stored once per id in [`Session::sub_agents`].
pub struct SubAgent {
    pub agent_type: String,          // Claude's free-form `subagent_type`
    pub description: String,
    pub prompt: String,
    pub status: AgentStatus,         // the terminal-or-running truth
    pub result: Option<String>,
    pub output_file: Option<String>, // async result sidecar
    pub transcript: Option<PathBuf>, // the child transcript on disk (agent-<id>.jsonl)
    pub subtree_cost: Option<UsdCost>,
    /// Index of this sub-agent's `SubAgentSpawn` block in the parent's `blocks` — the jump
    /// target (replaces `AgentEntry.at`).
    pub spawn_at: BlockIndex,
    /// The child's **parsed transcript**, recursively a whole `Session` (its own blocks,
    /// index, metrics, cwd, and `sub_agents`). `None` until enriched. Replaces the old
    /// `SubAgent.blocks: Vec<Block>` — so grandchildren, child metrics, and the child index
    /// all come for free.
    pub session: Option<Session>,
}

pub enum Block {
    // …
    /// Marks where a sub-agent was launched. Renders by looking the entity up in
    /// `Session.sub_agents[agent_id]`. (Was `SubAgent(SubAgent)`.)
    SubAgentSpawn { agent_id: AgentId },
    /// Marks where a sub-agent's completion notification arrived, at its point in the stream.
    /// Renders by looking up the entity. (Was `AgentDone { agent_id, agent_type, … }`.)
    AgentDone { agent_id: AgentId },
    // …
}
```

This **supersedes `SessionIndex.agents`**: the map is the source of truth; `active_agents()`
and `agent(id)` become trivial map operations (moved onto `Session`, or kept on the index over
a borrow of the map), and `spawn_at` + an ordered-iteration helper cover "jump to the spawn"
and "in spawn order." `SessionIndex` keeps the purely-positional indices (turns / tools /
attachments / counts).

## Payoff

- **One owner** per sub-agent — no attributes duplicated across spawn / done / index.
- **The child is a `Session`** (recursive) — grandchildren, the child's own metrics and index
  fall out for free; `subtree_cost` can even be *derived* from `session.metrics` instead of
  stored (a dedup to decide below).
- **O(1) id lookup**; the map replaces the linear `Vec<AgentEntry>`.

## Design decisions to settle before building

1. **Keying + late ids.** Async spawns assign `agent_id` *after* the spawn; the fold currently
   joins via `tool_use_id` and back-patches `agent_id` in `apply_completions_and_suppress`.
   The map must key by the **resolved** `agent_id`, so that back-patch pass is where entities
   get finalized. Decide the key for a spawn whose id never resolves (fall back to
   `tool_use_id`, or a synthetic key) — and blocks must reference whatever key the map uses.
2. **Renderers consult the map.** `render.rs`, `html_export`, and the viewer render a
   `SubAgentSpawn`/`AgentDone` by looking up `session.sub_agents[id]`, so their signatures gain
   access to the map (or the whole `&Session`). Byte-identical: same output, different source.
3. **Descend + bundle.** Descending into a child (app.rs) becomes
   `session.sub_agents[id].session` when enriched, else a lazy load from `.transcript` (the
   `discover::subagent_source` path stays for the un-enriched/live case).
4. **Incremental / live.** `FollowParser::poll` returns `(blocks, times, metrics)` with no map
   today. Decide whether the live path maintains a minimal `sub_agents` map (spawn/done update
   it; child sessions not loaded live) or leaves the map a batch-enrichment concern. This ties
   directly into `SessionBuilder` (#19), which should build the map.
5. **Recursion & clone cost.** `Option<Session>` inside a `BTreeMap` value is heap-backed —
   no `Box`, no infinite size. But cloning a `Session` now deep-clones child `Session`s, and
   the live `snapshot()` clones each poll. Consider `Arc<Session>` for the child (cheap clone,
   shared), or keep children out of the live snapshot.
6. **`subtree_cost`: stored vs derived.** With a child `Session`, `subtree_cost` is derivable
   from `session.metrics` (+ descendants). Prefer deriving (one source) unless the un-enriched
   case needs a stored value.

## Migration

Output-preserving but broad: `model` (block variants + the entity + the `Session` field) →
parse (`Shaping`/`Replayer` + `claude_model` enrich builds the map and nested `Session`s;
Codex has no sub-agents → empty map) → `index` (drop `agents`) → `render`/`html_export`/`view`
/`app` (look up the map) → `jdi`. Implement as a dedicated milestone, gated on the
`--dump`/`--dump-html` byte-identical diffs (both agents) and the sub-agent unit tests
(`render.rs`'s SubAgent test, `claude_model`'s enrich/spawn-and-completion tests).

See [`docs/architecture.md`](../docs/architecture.md) §4 (the data model) for the current
shape this refines.
