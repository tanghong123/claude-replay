# Design: an agent-agnostic `agent-jdi` supervisor spine

> **Status:** BUILT (task #17, 2026-08-01, user green-light). The spine now holds only
> `Box<dyn PermissionPosture>` / trait hooks; `src/jdi/mod.rs` names no concrete agent
> type and matches no agent identity in application logic. Deltas from the sketch below:
> `capture_permissions` takes `Option<&str>` (the adapter owns the fail-closed missing-id
> error); posture RENDERING also moved behind the trait (`persisted_note`/`banner_lines`)
> so run-banner/meta strings stay byte-identical; `preserves_permissions()` was replaced
> by `default_permission_note()` (the no-posture-captured meta note doubles as the
> capability signal); and the cmdline-reverse hook landed as the smaller
> `resume_id_flags()` vocabulary hook rather than a whole-parse override.

## Problem

`agent-jdi` is structured as an **agent-neutral spine** (`src/jdi/mod.rs`,
`supervisor.rs`, `state.rs`, `lock.rs`, `backlog.rs`) plus **per-agent adapters**
(`jdi/claude.rs`, `jdi/codex.rs`) behind the [`AgentAdapter`](../src/jdi/agent.rs) trait.
The round-2 architecture review found the spine still reaches past that trait in three
places, all around the **permission handoff** and **session-id plumbing** — so a third
agent (or a second agent with a permission posture) could not be added by "one adapter impl
+ one registry row"; it would require editing the neutral spine.

The engine side (`claude-replay-core`) already meets that bar; this doc brings the supervisor
side up to it. **None of this is a regression** — it predates the engine refactor; it is the
last agent-coupling in the codebase.

### The three couplings (as of this writing)

1. **Permission handoff is hard-typed to Codex throughout the neutral spine (HIGH).**
   The takeover/handoff path threads `codex::CodexPermissionSnapshot` and
   `codex::CodexSandboxMode` through neutral functions and even the neutral CLI:
   - `persist_permissions(session, agent, Option<&codex::CodexPermissionSnapshot>)`
     (`mod.rs:642`) — writes the `cargs` file + the `permissions` meta line.
   - `handoff_permission_args(Option<&codex::CodexPermissionSnapshot>) -> Vec<String>`
     (`mod.rs:686`) — serializes the snapshot into CLI args for the detached `__run`.
   - `handoff_permission_lines(&codex::CodexPermissionSnapshot)` (`mod.rs:701`) — the
     human-readable run-summary lines.
   - `handoff_permission_snapshot(agent, session_id, transcript)` (`mod.rs:711`) —
     reconstructs the snapshot from the live rollout.
   - `cmd_handoff` / `cmd_handoff_wait` signatures (`mod.rs:518`, `1901-1902`).
   - The neutral CLI flags `--codex-sandbox` / `--codex-workspace-network`
     (`mod.rs:176-179`), typed as `codex::CodexSandboxMode`.

   Only the **gate** was abstracted: `AgentAdapter::preserves_permissions() -> bool`
   (`agent.rs:239`, `false` by default, `true` for Codex). The *payload*, its CLI
   serialization, and its rendering were not. The concrete logic itself is already cleanly
   encapsulated in `jdi/codex.rs` (`CodexPermissionSnapshot::{from_rollout, from_handoff_parts,
   config_args, summary}`); the leak is that the **spine names those concrete types**.

2. **`session_id_in_cmdline` reverse-parses agent-specific flags (MED).**
   `mod.rs:2047` does `match agent { … }` to know each agent's resume flags
   (`["--resume","--session-id"]` for Claude, `["resume"]` for Codex) so it can recover a
   session id from a running process's command line. The *forward* direction is already
   behind the trait (`build_invocation`/`interactive_invocation`/`resume_commands`); there is
   no reverse hook.

3. **`pinned_handoff_session_id` branches on `agent == Agent::Codex` (MED).**
   `mod.rs:1879` special-cases Codex to prefer its thread-id environment source as the
   "ambient session id". This is agent-identity logic in the spine.

## Proposed design

The spine should know only **agent-neutral shapes**; everything agent-specific moves behind
`AgentAdapter`. Three additions, mirroring how the engine's `TranscriptAdapter` handles
optional per-agent capabilities (default methods that return "unsupported").

### 1. A `PermissionPosture` trait object

Introduce an agent-neutral trait for "the permission/sandbox posture a run executes under",
implemented by each agent that has one (only Codex today):

```rust
// src/jdi/agent.rs
/// An agent's per-session permission/sandbox posture, captured for a takeover so the
/// resumed run keeps the exact context the unattended run had. Agents with no posture
/// (Claude) return `None` from `capture_permissions`, and the spine holds `None`.
pub trait PermissionPosture {
    /// Extra CLI `-c`/flag args that impose this posture on the agent invocation.
    fn config_args(&self) -> Vec<String>;
    /// One-line human summary for the run banner (e.g. "workspace-write, network disabled").
    fn summary(&self) -> String;
    /// Serialize into `agent-jdi __run` handoff flags (round-trips via `parse_handoff`).
    fn handoff_flags(&self) -> Vec<String>;
}

pub trait AgentAdapter {
    // … existing …

    /// Capture the live permission posture for a takeover (reads the rollout/session).
    /// Default `None` — an agent with no posture (Claude) preserves nothing; the spine
    /// then just clears stale permission state. (Replaces the `preserves_permissions()`
    /// bool: `Some(..)` *is* "preserves".)
    fn capture_permissions(
        &self,
        _session_id: &str,
        _transcript: Option<&Path>,
    ) -> Result<Option<Box<dyn PermissionPosture>>> {
        Ok(None)
    }

    /// Reconstruct a posture from the handoff flags the parent serialized (the inverse of
    /// `PermissionPosture::handoff_flags`). Default `None`.
    fn parse_handoff_permissions(&self, _flags: &HandoffFlags) -> Result<Option<Box<dyn PermissionPosture>>> {
        Ok(None)
    }
}
```

`jdi/codex.rs` implements `PermissionPosture for CodexPermissionSnapshot` (the four methods
already exist almost verbatim), and `CodexAdapter::capture_permissions` wraps
`from_rollout`. The spine then holds `Option<Box<dyn PermissionPosture>>` and the four
`handoff_permission_*` / `persist_permissions` functions lose every `codex::` mention —
they call trait methods.

`preserves_permissions()` is subsumed: "preserves" becomes "`capture_permissions` returned
`Some`". Keep a thin `preserves_permissions()` default only if a cheap pre-check is wanted.

### 2. The neutral CLI-flag problem

The handoff passes posture from the parent `handoff` process to the detached `__run` process
**as CLI args** — today the typed clap flags `--codex-sandbox`/`--codex-workspace-network`.
Those can't stay agent-typed. Two options:

- **(preferred) One opaque, repeatable neutral flag:** `--permission-arg <string>` (0+),
  carrying whatever `PermissionPosture::handoff_flags()` emitted; the chosen agent's
  `parse_handoff_permissions` interprets them. The spine's clap surface becomes a
  `Vec<String>` it never inspects. Simple, agent-count-independent, and keeps clap out of
  agent identity.
- **(alt) A single encoded blob:** `--permission-posture <base64-json>`. More opaque, but
  needs a serde contract per agent. The repeatable-flag option is lighter.

Either way the parent side is `args.extend(posture.handoff_flags())` and the child side is
`adapter.parse_handoff_permissions(&flags)`.

### 3. Two small reverse/ambient hooks

```rust
/// Recover a pinned session id from a running process's command line (the inverse of
/// `build_invocation`). Default `None`.
fn session_id_from_cmdline(&self, _cmdline: &str) -> Option<String> { None }

/// The agent's "ambient" session id for a fresh handoff, if it exposes one outside the
/// transcript (Codex's thread-id env var). Default `None`.
fn ambient_session_id(&self) -> Option<String> { None }
```

`session_id_in_cmdline` (`mod.rs:2047`) loops `agent::agents()` calling
`session_id_from_cmdline`; `pinned_handoff_session_id` (`mod.rs:1879`) calls
`ambient_session_id()` instead of `== Agent::Codex`.

## Result

After this, `src/jdi/mod.rs` contains **no `codex::`/`claude::` path and no `match agent`**
in application logic — the same guarantee the engine already provides. Adding a third agent
to the supervisor is: a new `jdi/<agent>.rs` implementing `AgentAdapter` (+ a
`PermissionPosture` if it has one) and one row in `agent::adapter`/`agent::agents`.

## Testing

- Preserve the existing jdi fixture tests (fake `claude`/`codex` binaries) — the handoff →
  detached-`__run` → resume flow must be behavior-identical.
- Add a round-trip test: `posture.handoff_flags()` → `parse_handoff_permissions` yields an
  equal posture; and `config_args()` output is unchanged for the Codex cases now covered by
  `codex_metrics`/`codex.rs` tests.
- Gate on the full suite + `cargo clippy`/`cargo fmt` as usual.

## When to build

This is **extensibility for an agent that does not yet exist**. With two agents — one of
which (Claude) has no permission posture at all — the current concrete coupling is small and
readable. Build this when either:

- a **third agent** is added (the double-registration and head-shape hooks land at the same
  time — see the engine's `adapters()` pattern), or
- a **second agent with a permission posture** appears (then `PermissionPosture` stops being
  speculative and starts removing real duplication).

Until then it is documented here so the intent is not lost, and so the coupling is a
deliberate, bounded exception rather than an oversight.
