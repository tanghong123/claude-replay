# Shared jdi-handoff Skill Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Install one shared `jdi-handoff` Skill for Codex and Claude Code without duplicating its source.

**Architecture:** Keep the agent-neutral Skill under `integrations/shared`, copy it to the standard user agent-skills directory, and link Claude's Skill entry to that installed copy. Keep only the Claude slash-command adapter client-specific and validate the installer in temporary directories.

**Tech Stack:** POSIX shell, GitHub Actions, Markdown, existing Rust/Cargo verification gates

## Global Constraints

- Base the branch on `upstream/main@425ed95` independently of PR #3.
- Keep exactly one repository copy of `jdi-handoff/SKILL.md`.
- Do not modify the user's real `~/.agents`, `~/.claude`, or `$HOME` during tests.
- Preserve the existing Claude `/jdi-handoff` command.
- Codex uses `$jdi-handoff`; do not claim support for a custom first-level `/jdi-handoff` command.

---

### Task 1: Specify the cross-client installer behavior

**Files:**
- Create: `tests/install_jdi_handoff.sh`
- Test: `tests/install_jdi_handoff.sh`

**Interfaces:**
- Consumes: `integrations/install-jdi-handoff.sh`, `--agents-dir`, and `--claude-dir`.
- Produces: an executable integration test that exits nonzero until the installer and shared layout exist.

- [ ] **Step 1: Write a failing POSIX shell test**

The test creates a temporary path containing spaces, invokes:

```sh
sh integrations/install-jdi-handoff.sh \
  --agents-dir "$fixture/.agents/skills" \
  --claude-dir "$fixture/.claude"
```

It asserts that the canonical file and Claude command match their repository
sources, the Claude Skill file is a symlink to the installed canonical file, a
second install is idempotent, and an old regular Claude Skill is backed up during
migration.

- [ ] **Step 2: Run the test and verify RED**

Run: `sh tests/install_jdi_handoff.sh`

Expected: FAIL because `integrations/install-jdi-handoff.sh` and the shared Skill
path do not exist.

### Task 2: Implement the shared layout and installer

**Files:**
- Move: `integrations/claude/skills/jdi-handoff/SKILL.md` → `integrations/shared/skills/jdi-handoff/SKILL.md`
- Create: `integrations/install-jdi-handoff.sh`
- Test: `tests/install_jdi_handoff.sh`

**Interfaces:**
- Consumes: repository-relative shared Skill and Claude command source files.
- Produces: `~/.agents/skills/jdi-handoff/SKILL.md`, a Claude Skill symlink, and the Claude command copy.

- [ ] **Step 1: Move the Skill without changing its workflow content**

Use the shared repository path as the only tracked `jdi-handoff/SKILL.md`.

- [ ] **Step 2: Implement argument parsing and repository path resolution**

Support exactly:

```text
--agents-dir PATH
--claude-dir PATH
-h, --help
```

Reject missing values and unknown arguments. Resolve sources relative to the
installer rather than the caller's working directory.

- [ ] **Step 3: Implement safe, idempotent installation**

Create destination directories, refresh the canonical Skill and Claude command,
replace an existing Claude symlink, and preserve a pre-existing regular Claude
Skill once as `SKILL.md.pre-shared-backup` before linking it to the canonical
file.

- [ ] **Step 4: Run the test and verify GREEN**

Run: `sh tests/install_jdi_handoff.sh`

Expected: PASS with all files confined to the temporary fixture.

- [ ] **Step 5: Verify shell syntax**

Run: `sh -n integrations/install-jdi-handoff.sh tests/install_jdi_handoff.sh`

Expected: exit 0 with no output.

### Task 3: Document and continuously verify installation

**Files:**
- Create: `integrations/README.md`
- Delete: `integrations/claude/README.md`
- Modify: `README.md`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: the installer and its integration test.
- Produces: public install/use instructions and a CI regression gate.

- [ ] **Step 1: Document automated and manual installation**

Explain the shared installed source, Claude symlink/command, Codex
`$jdi-handoff`, Claude `/jdi-handoff`, session restart requirement, and custom
destination flags.

- [ ] **Step 2: Update the root README integration link and client-specific usage**

Link to `integrations/README.md` and distinguish Claude `/jdi-handoff` from Codex
`$jdi-handoff` without changing JDI runtime behavior.

- [ ] **Step 3: Add the installer test to CI**

Add a `Shared jdi-handoff installer` step that runs:

```sh
sh tests/install_jdi_handoff.sh
```

- [ ] **Step 4: Run all final gates**

```sh
sh tests/install_jdi_handoff.sh
sh -n integrations/install-jdi-handoff.sh tests/install_jdi_handoff.sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all
```

Expected: every command exits 0; the Rust baseline remains green and the shell
test reports success.

- [ ] **Step 5: Review the fixed diff and commit**

```sh
git diff --check upstream/main...HEAD
git diff --stat upstream/main...HEAD
```

Expected: only shared integration, documentation, CI, test, design, and plan files
are present; no PR #3 Rust fixes appear.

## Rollback

Close or revert only this feature PR. Existing Claude users can continue using
their previously copied Skill and command; no runtime state or session migration
is involved.

## Self-Review

- Spec coverage: shared source, both client discovery paths, backward-compatible
  Claude command, safe migration, documentation, and CI are covered.
- Placeholder scan: no TBD/TODO/later placeholders remain.
- Interface consistency: installer flags and destination paths match the design
  and test plan.
