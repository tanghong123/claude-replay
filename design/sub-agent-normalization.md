# Design: normalize sub-agents into a Session metadata map

> **Status:** proposed (not built). A public data-model change, output-preserving, gated on
> byte-identical diffs. Tracked as task #20. Pairs with **lazy session loading** (task #21) —
> the existing `SessionStore` + `FollowParser`, no new type — which materializes the parsed
> child transcripts on demand. Supersedes `SessionIndex.agents` / `AgentEntry`.

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
   stream carries only references (ids). **Parsing a child is not the map's job** — it's done
   by [lazy session loading](line-reader-and-session-builder.md) (task #21: `SessionStore` +
   `FollowParser`), on request.

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
/// [`Session::sub_agents`]. Purely descriptive + the paths a lazy loader needs to load the child on
/// demand; it never contains the parsed child (that's materialized on request via
/// `SessionStore` + `FollowParser`, task #21).
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
    // ── pointers to external artifacts (for lazy loading; never parsed here) ──
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

The map gives the client the child's *identity and path*; [lazy session
loading](line-reader-and-session-builder.md) (`SessionStore` + `FollowParser`, task #21) turns
that into a live `Session` **only when asked**:

```text
parse root ─▶ Session { …, sub_agents: {id → SubAgent{ transcript, … }} }
client wants child `id`:
   FollowParser::open(agent, &sub_agents[id].transcript)   // 1st: parse transcript → Session
   .poll()  (again, later)                                 // tail new events, current Session
   (keep the follower in a SessionStore keyed by id → residency + TTL, if desired)
```

So the data model stays a flat value; materialization, liveness, and residency are a caller
concern handled with the core `FollowParser` (+ the optional `SessionStore` helper) — exactly
today's `html_export::serve.rs` composition, not a new abstraction (task #21).

## The eager path: `parse_session_enriched` returns root + a flat agent map

The lazy path above suits interactive/live use. A caller that wants the **whole tree at once**
(offline `--dump-all-html`, programmatic analysis) shouldn't have to drive a loader — so
`parse_session_enriched[_as]` eagerly parses every reachable child and returns them in a **flat
side-map**, never nested:

```rust
#[non_exhaustive]
pub struct SessionTree {
    pub root: Session,
    /// Every agent reachable from the root, **at any depth**, keyed by id — each a flat
    /// one-transcript `Session`. Claude's `subagents/` dir is a single flat namespace (ids are
    /// unique session-wide), so one map holds the whole tree; the parent→child **edges** live
    /// in each `Session`'s `sub_agents` metadata map, while this map holds the parsed
    /// **content**. Contains only agents whose transcript was found (a spawn with a missing
    /// child file is in the metadata but not here).
    pub agents: BTreeMap<AgentId, Session>,
}

pub fn parse_session(path: &Path) -> io::Result<Session>;              // flat: root only
pub fn parse_session_enriched(path: &Path) -> io::Result<SessionTree>; // eager whole tree
// (+ the `_as` known-agent variants)
```

Structure vs content, cleanly split — and no embedded `Session`, so the flat-and-cheap-to-clone
invariant holds. This makes `subtree_cost` **derivable** from the tree (sum a node's `metrics`
plus its descendants' via the `sub_agents` edges), so it needn't be stored on the eager path.

`parse_session` stays flat (root only; `sub_agents` metadata populated, no children parsed).
The two entry points thus differ by return type — `Session` vs `SessionTree` — which states the
intent. Both are the same flat `Session` value underneath; `SessionTree` is eager, the lazy
`FollowParser`/`SessionStore` path (task #21) is on-demand + live — coexisting over the one
metadata backbone.

## Payoff

- **One owner** per sub-agent — no attributes duplicated across spawn / done / index.
- **`Session` is flat and cheap to clone** — the recursion/clone-cost problem disappears; a
  `Session` is one transcript, always.
- **Lazy + live children** — the client pays to parse a child only when it opens it, and gets
  incremental updates on re-open, via lazy loading (task #21).
- **O(1) id lookup**; the map replaces the linear `Vec<AgentEntry>`.

## Design decisions to settle before building

1. **Keying + when the id is known.** The spawn `tool_use` does **not** carry the agent id — it
   arrives on the spawn's **`tool_result`** (`toolUseResult.agentId`, the `user` message closing
   the `Task`/`Agent` call), where the fold back-patches it onto the `SubAgent` (join by
   `tool_use_id`). So the id is available *immediately after the spawn* — well before the
   completion notification — and the map can key by it then. The only id-less case is a spawn
   whose `tool_result` never arrived (interrupted / still-launching): key those by
   `tool_use_id` (or a synthetic key). Blocks reference whatever key the map uses.
   (`apply_completions_and_suppress` runs later and only back-patches the terminal *status* /
   resolves `AgentDone` — it does not assign the id.)
2. **Renderers consult the map.** `render.rs`, `html_export`, and the viewer render a
   `SubAgentSpawn`/`AgentDone` by looking up `session.sub_agents[id]`, so their signatures gain
   access to the map (or the `&Session`). Byte-identical: same output, different source.
3. **`subtree_cost`: derived, not stored (where possible).** On the eager path it's derivable
   from the [`SessionTree`](#the-eager-path-parse_session_enriched-returns-root--a-flat-agent-map)
   (sum a node's `metrics` + its descendants' via the `sub_agents` edges), so it needn't live on
   the meta at all. The lazy path computes it as children load. Keep a stored `Option<UsdCost>`
   on the meta only if a consumer needs the number before any child is parsed (then `None` until
   known) — decide whether that case is real or the field can be dropped entirely.
4. **Discovery of the paths.** `transcript` comes from `discover::subagent_source` (Claude's
   flat `subagents/` layout); `output_file` from the spawn's `toolUseResult`. Codex has no
   sub-agents → empty map.

## Migration

Output-preserving but broad: `model` (block variants + the meta entity + the `Session` field)
→ parse (`Shaping`/`Replayer` + `claude_model` build the meta map; **stop** enriching child
blocks into the model) → `index` (drop `agents`) → `render`/`html_export`/`view`/`app` (look up
the map; descend via lazy loading) → `jdi`. Implement as a dedicated milestone with task #21
(lazy loading, task #21), gated on the `--dump`/`--dump-html` byte-identical diffs (both agents) and the
sub-agent unit tests.

See [`docs/architecture.md`](../docs/architecture.md) §4 for the current data model this
refines, and [`line-reader-and-session-builder.md`](line-reader-and-session-builder.md) for the
`LineReader` → `SessionBuilder` stack (+ the `SessionStore`-based lazy loading, task #21) this
descends through.
