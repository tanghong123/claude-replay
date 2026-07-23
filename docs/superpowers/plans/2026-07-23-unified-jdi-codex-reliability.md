# Unified JDI/Codex Reliability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the two open pull requests with one latest-main-based change that installs the shared handoff Skill, fixes Codex picker labels, preserves tracked-session resume, and pins/validates real Codex sessions reliably.

**Architecture:** Keep PR #4's installation layer as the integration branch. Port only the behavior from PR #3 that is still missing on v0.17.0, then add small pure helpers around Codex session identity and process selection so they can be test-driven without spawning or killing real sessions.

**Tech Stack:** Rust 2021, clap, serde_json, shell integration tests, GitHub CLI

## Global Constraints

- Use `feat/shared-jdi-handoff-skill` as the only integration branch and PR #4 as the only final pull request.
- Preserve v0.17.0's explicit raw `resume --session <session-id>` behavior.
- Add tracked slot selection as the distinct `resume --id <slot-id>` interface.
- Session pinning order for handoff is explicit `--session`, `CODEX_THREAD_ID`, process argv, then cwd-scoped discovery.
- Never choose or kill an arbitrary process when more than one same-cwd candidate remains.
- Use `gh` only for GitHub network mutations.
- Do not modify or remove the dirty `wiki-init-ignore` worktree or root `.delivery/` files.
- Do not merge or deploy.

---

### Task 1: Port the reviewed picker behavior

**Files:**
- Modify: `src/codex_discover.rs`
- Modify: `src/codex_model.rs`
- Test: `src/codex_discover.rs`

**Interfaces:**
- Consumes: Codex `response_item` user messages and `session_meta` subagent metadata.
- Produces: a human prompt, `↳ subagent <name>`, or `(no user prompt)` as the picker snippet.

- [ ] **Step 1: Apply the two previously test-driven commits**

```bash
git cherry-pick 9eceb7c 32e8240
```

Expected: both commits apply without changing v0.17.0's cwd/ancestor scoping.

- [ ] **Step 2: Run focused tests**

```bash
cargo test codex_discover::tests::picker_snippet_skips_host_context_messages -- --exact --nocapture
cargo test codex_discover::tests::picker_snippet_labels_subagent_without_user_prompt -- --exact --nocapture
cargo test codex_discover::tests::picker_snippet_labels_regular_session_without_user_prompt -- --exact --nocapture
```

Expected: each command runs one passing test.

### Task 2: Adapt tracked-slot resume and test isolation to v0.17.0

**Files:**
- Modify: `src/jdi/mod.rs`
- Modify: `src/jdi/claude.rs`
- Modify: `README.md`
- Test: `src/jdi/mod.rs`

**Interfaces:**
- Consumes: `resume --id <slot>`, slot metadata, and the existing `resume --session <session-id>`.
- Produces: a resolved cwd, agent, session id, and transcript while keeping raw session pinning intact.

- [ ] **Step 1: Add failing CLI and slot-resolution tests**

```rust
#[test]
fn resume_accepts_a_tracked_slot_id_alongside_raw_session_id() {
    assert!(Cli::try_parse_from(["agent-jdi", "resume", "--id", "repo-123"]).is_ok());
    assert!(Cli::try_parse_from(["agent-jdi", "resume", "--session", "thread-123"]).is_ok());
}
```

Add tests that reject path-shaped slot ids and resolve cwd/agent/session/transcript from metadata.

- [ ] **Step 2: Verify the new focused test fails**

```bash
cargo test jdi::tests::resume_accepts_a_tracked_slot_id_alongside_raw_session_id -- --exact --nocapture
```

Expected: failure because v0.17.0 has `--session` but not `--id`.

- [ ] **Step 3: Implement the minimal adaptation**

Add `id: Option<String>` to `Command::Resume`. Reject simultaneous `--id` and `--session`, validate the slot as a single normal path component, and resolve tracked metadata before the existing resume flow. Keep cwd-scoped discovery for calls without either flag.

- [ ] **Step 4: Make the Claude takeover unit test self-contained**

Set `AGENT_JDI_CLAUDE_BIN` to the current test executable under a scoped environment lock, then restore the previous value. Do not alter production binary lookup.

- [ ] **Step 5: Correct log/follow help text and run focused tests**

```bash
cargo test jdi::tests::resume_accepts_a_tracked_slot_id_alongside_raw_session_id -- --exact --nocapture
cargo test jdi::tests::explicit_session_id_rejects_paths -- --exact --nocapture
cargo test jdi::tests::explicit_resume_target_comes_from_tracked_slot_metadata -- --exact --nocapture
cargo test jdi::tests::start_and_resume_expose_the_follow_flag -- --exact --nocapture
```

Expected: each command runs one passing test.

### Task 3: Pin and validate the real Codex session

**Files:**
- Modify: `src/jdi/codex.rs`
- Modify: `src/jdi/mod.rs`
- Modify: `src/jdi/DESIGN.md`
- Test: `src/jdi/codex.rs`
- Test: `src/jdi/mod.rs`

**Interfaces:**
- Consumes: `CODEX_THREAD_ID`, Codex JSONL stdout, `codex login status`, and process-table candidates.
- Produces: an exact thread id or an explicit ambiguity/error; never a guessed live-process target.

- [ ] **Step 1: Add failing `thread_id` capture test**

```rust
#[test]
fn captures_thread_id_from_real_codex_json_event() {
    let output = r#"{"type":"thread.started","thread_id":"thread-123"}"#;
    assert_eq!(CodexAdapter.capture_session_id(output, Path::new("/tmp"), "nonce"), Some("thread-123".into()));
}
```

- [ ] **Step 2: Verify it fails, then add `/thread_id` to capture pointers**

```bash
cargo test jdi::codex::tests::captures_thread_id_from_real_codex_json_event -- --exact --nocapture
```

Expected: fail before implementation and pass afterward.

- [ ] **Step 3: Add failing handoff-priority tests**

Extract a pure helper whose inputs are explicit id, agent, `CODEX_THREAD_ID`, argv id, and discovered id. Test the order:

```text
explicit --session > CODEX_THREAD_ID for Codex > argv > cwd-scoped discovery
```

- [ ] **Step 4: Verify failure, implement the helper, and use it from `cmd_handoff`**

```bash
cargo test jdi::tests::codex_handoff_prefers_thread_environment_over_discovery -- --exact --nocapture
```

Expected: fail before implementation and pass afterward.

- [ ] **Step 5: Add a failing preflight test with a fake executable**

The fake executable records arguments and returns a controlled status. Assert that preflight invokes exactly `login status` and reports a non-zero result.

- [ ] **Step 6: Implement `codex login status` preflight**

Resolve the configured Codex binary, run `login status` without inheriting interactive stdin, and include stderr/stdout context on failure.

- [ ] **Step 7: Add failing process-selection ambiguity tests**

Extract a pure candidate selector and test zero, one exact match, and multiple same-cwd candidates. Multiple candidates must return an ambiguity error with all candidate PIDs.

- [ ] **Step 8: Implement fail-closed unmanaged takeover selection**

Use the selector before any kill/launch action. A forced takeover must still refuse when the target cannot be identified exactly.

- [ ] **Step 9: Remove obsolete Codex CLI `TODO(verify)` notes**

Document the verified CLI 0.145.0 contract: `codex exec`, `codex exec resume`, `codex resume`, `login status`, and the `thread.started/thread_id` event.

### Task 4: Verify and consolidate the pull requests

**Files:**
- Verify: every changed file in `upstream/main...HEAD`
- Update: PR #4 title/body
- Close: PR #3 after PR #4 readback succeeds

**Interfaces:**
- Consumes: fixed base/head diff and local verification output.
- Produces: one open PR targeting `main`.

- [ ] **Step 1: Run repository and integration gates**

```bash
cargo fmt --check
cargo clippy --all-targets
cargo test --all
bash tests/install_jdi_handoff.sh
bash -n integrations/install-jdi-handoff.sh tests/install_jdi_handoff.sh
git diff --check upstream/main...HEAD
```

Expected: every command exits 0.

- [ ] **Step 2: Run real local Codex contract checks**

```bash
codex login status
agent-jdi --dry-run --agent codex handoff
```

Expected: login is reported and dry-run prints the current `CODEX_THREAD_ID` as the pinned session without mutating session state.

- [ ] **Step 3: Review the exact fixed diff**

```bash
git status --short
git log --oneline upstream/main..HEAD
git diff --stat upstream/main...HEAD
```

Expected: only the unified installer, picker, JDI reliability, tests, and plan/docs changes are present.

- [ ] **Step 4: Push PR #4 with GitHub CLI-backed Git Data writes**

Upload the verified commit/tree to `feat/shared-jdi-handoff-skill`, read back the remote SHA, then update PR #4's title/body with `gh pr edit`.

- [ ] **Step 5: Close superseded PR #3**

```bash
gh pr close 3 --repo tanghong123/claude-replay --comment "Superseded by #4, which includes these fixes on v0.17.0 plus the shared handoff integration."
```

Expected: PR #4 is open with the verified head SHA; PR #3 is closed.

## Rollback

- Close PR #4 without merging if the integrated diff is rejected.
- Keep local worktrees and remote branches until the user explicitly asks to remove them.
- Revert only the integration commits; do not reset or delete user files.

## Self-Review

- Spec coverage: shared install, picker labels, tracked resume, exact Codex identity, auth preflight, takeover ambiguity, verification, and PR consolidation are each assigned to a task.
- Placeholder scan: no deferred implementation placeholder is part of the plan.
- Type consistency: raw Codex IDs use `--session`; tracked agent-jdi slots use `--id`; thread identity is a string throughout.
