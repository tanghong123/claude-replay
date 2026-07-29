# Minimal Codex Subagent Replay

## Goal

Add Codex CLI subagent replay to the existing shared `Block::SubAgent` /
`Block::AgentDone` presentation flow while keeping the current Claude and
QoderWork behavior, `Transcript` API, FollowParser architecture, cache model,
and HTML/TUI UX unchanged.

## Evidence and constraints

- Codex `spawn_agent` function outputs contain only a canonical task path such
  as `/root/spec_review`; they do not contain the child rollout UUID.
- The child rollout UUID and its parent relationship are recorded separately in
  the child rollout's `session_meta.source.subagent.thread_spawn` object.
- Current `main` already provides the shared lifecycle blocks, replay fold,
  `TranscriptAdapter::enrich`, `TranscriptAdapter::subagent_source`, recursive
  HTML bundle traversal, lazy served child registration, breadcrumbs, and TUI
  child navigation.
- No Codex-specific CSS or JavaScript branch is allowed.
- Claude and QoderWork adapter behavior must remain byte-for-byte compatible.
- No operation-wide `SessionGraph`, new public `Transcript` methods, or cache
  persistence concept will be introduced.

## Chosen design

### Canonical lifecycle blocks

`codex_model` recognizes the Codex collaboration events:

- `spawn_agent` is shaped as `Block::SubAgent`.
- Its function output supplies the canonical agent task path.
- A final `agent_message` is decoded as the existing canonical
  `Message::Completion`, which produces `Block::AgentDone` and lets the shared
  replay fold update terminal status.

The block's `agent_id` is a reversible, filename-safe key derived from the full
Codex task path. The key is stable within and across replays, contains no `/`,
and can therefore be used directly by the existing HTML stream and query
conventions. The raw prompt remains in the existing `SubAgent.prompt` field and
is not used as identity.

### Adapter-owned rollout lookup

`codex_discover` implements the Codex-specific relationship lookup behind the
existing adapter seam:

1. Parse the root rollout's `session_meta.id`.
2. Decode the child key back to its canonical agent task path.
3. Scan Codex rollout metadata under the same sessions store.
4. Select the unique rollout whose `agent_path` matches and whose
   `parent_thread_id` ancestry reaches the root rollout.
5. Return that rollout path from `CodexAdapter::subagent_source`.

The lookup is stateless and operation-scoped by ancestry. It cannot link a
same-named agent from an unrelated Codex session. A child created after the
parent's initial live poll becomes discoverable on a later existing
registration attempt without adding resolver state to FollowParser or cache.

### Existing traversal paths

No presenter architecture changes are required:

- Batch parse builds the existing `sub_agents` index and fills transcript paths
  through `subagent_source`.
- `parse_enriched` delegates to a Codex-only `enrich` implementation that loads
  descendants using the same lookup.
- FollowParser emits the same lifecycle blocks incrementally.
- `dump-all-html` uses its existing BFS and produces one stream per safe child
  key.
- Served live replay uses its existing lazy child registration.
- TUI navigation uses the existing `subagent_source` entry point.

## Alternatives rejected

### Shared operation-scoped SessionGraph

This handles late rollout creation efficiently, but requires threading graph
state through `Transcript`, FollowParser, cache restoration, batch parsing, and
Claude/QoderWork adapters. It is disproportionate for a Codex-only adapter
addition and changes public API semantics.

### Presentation-only child-ID rewriting

Deriving a different ID only in HTML would leave TUI, session metadata, cache,
and block semantics inconsistent and would require JavaScript or presenter
special cases.

### Global task-path lookup

Task paths repeat across independent sessions. Looking them up without checking
the `parent_thread_id` ancestry can connect a parent to an unrelated rollout.

## Failure behavior

- A malformed spawn output leaves `agent_id` empty, so no dead child link is
  rendered.
- A missing or ambiguous child rollout returns no source and leaves the spawn
  visible but non-navigable.
- A malformed child `session_meta` is ignored.
- Cyclic or broken ancestry is rejected.
- Non-final agent messages do not emit `AgentDone`.

## Test strategy

Tests are added before implementation and must demonstrate:

1. Codex spawn output becomes a shared `SubAgent` with a safe stable key.
2. A final agent message becomes `AgentDone` and completes the matching spawn.
3. Rollout lookup resolves direct and nested children but rejects an unrelated
   same-path rollout.
4. Batch and FollowParser expose identical Codex lifecycle blocks.
5. `dump-all-html` materializes the parent and every reachable child stream
   without presenter code changes.
6. Served live registration resolves the Codex child through the existing
   source hook.
7. Existing Claude parser, follow, HTML, and full workspace tests remain green.

## Scope

Expected production changes are limited to Codex parsing/discovery and the
Codex adapter implementation. Presenter, cache, Transcript, FollowParser,
Claude, and QoderWork production code are out of scope unless a failing
integration test proves an existing generic path is defective.
