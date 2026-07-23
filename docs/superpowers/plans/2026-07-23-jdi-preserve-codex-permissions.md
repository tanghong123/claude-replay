# Preserve Codex Permissions Across JDI Handoff Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Preserve the current Codex turn's sandbox and workspace-network policy across `agent-jdi handoff` without escalating permissions or changing external `start`/`resume` defaults.

**Architecture:** Parse the pinned Codex rollout's latest `turn_context` into a normalized `CodexPermissionSnapshot`, pass that snapshot through the detached watcher, and persist its exact Codex config arguments in the tracked session's existing `cargs` file. The supervisor already reloads `cargs` for retries and backlog drains; external resumes clear stale permission arguments before spawning.

**Tech Stack:** Rust 2021, clap, serde_json, existing agent-jdi state/supervisor abstractions, shell integration tests.

## Global Constraints

- Handoff may preserve or reduce the current execution boundary, but it must not increase it.
- Missing, malformed, or unsupported Codex permission context aborts handoff before watcher spawn or session termination.
- Codex unattended approval policy remains `never`.
- Claude behavior remains unchanged.
- Public `start` and externally launched `resume` retain workspace-write/network-disabled defaults.
- No implementation commit is pushed or added to PR #4 before local tests and user review.
- Real smoke tests use only disposable local repositories and do not mutate production remotes or pull requests.

---

## File map

- `src/jdi/codex.rs`: normalized Codex permission types, rollout parser, config-argument rendering, and adapter invocation policy.
- `src/jdi/mod.rs`: CLI handoff capture, watcher transport, tracked-session persistence/clearing, dry-run output, and status display.
- `src/jdi/supervisor.rs`: unchanged behavior; consumes the persisted `cargs` on every retry/drain.
- `src/jdi/DESIGN.md`: durable handoff permission contract.
- `integrations/README.md`: user-facing Skill behavior and failure semantics.
- `integrations/shared/skills/jdi-handoff/SKILL.md`: tell the invoking agent that Codex permissions are preserved and capture failure leaves the session alive.

### Task 1: Parse and normalize Codex permission snapshots

**Files:**
- Modify: `src/jdi/codex.rs`
- Test: `src/jdi/codex.rs` inline `#[cfg(test)]` module

**Interfaces:**
- Produces: `pub(crate) enum CodexSandboxMode`
- Produces: `pub(crate) struct CodexPermissionSnapshot`
- Produces: `CodexPermissionSnapshot::from_rollout(path: &Path) -> Result<Self>`
- Produces: `CodexPermissionSnapshot::from_handoff_parts(mode: CodexSandboxMode, workspace_network: Option<bool>) -> Result<Self>`
- Produces: `CodexPermissionSnapshot::config_args(&self) -> Vec<String>`
- Produces: `CodexPermissionSnapshot::summary(&self) -> String`

- [ ] **Step 1: Write failing parser tests**

Add realistic rollout fixtures to the existing test module:

```rust
fn rollout_with_contexts(contexts: &[serde_json::Value]) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "agent-jdi-codex-permissions-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("rollout.jsonl");
    let mut body = String::from("not-json\n");
    for context in contexts {
        body.push_str(&format!("{context}\n"));
    }
    std::fs::write(&path, body).unwrap();
    path
}

#[test]
fn parses_latest_codex_turn_permissions() {
    let path = rollout_with_contexts(&[
        serde_json::json!({
            "type": "turn_context",
            "payload": {
                "sandbox_policy": {
                    "type": "workspace-write",
                    "network_access": false
                }
            }
        }),
        serde_json::json!({
            "type": "turn_context",
            "payload": {
                "sandbox_policy": {
                    "type": "danger-full-access"
                }
            }
        }),
    ]);
    let snapshot = CodexPermissionSnapshot::from_rollout(&path).unwrap();
    assert_eq!(snapshot.sandbox(), CodexSandboxMode::DangerFullAccess);
    assert_eq!(snapshot.workspace_network(), None);
}
```

Add these exact cases:

```rust
#[test]
fn parses_workspace_network_enabled() {
    let path = rollout_with_contexts(&[serde_json::json!({
        "type": "turn_context",
        "payload": {
            "sandbox_policy": {
                "type": "workspace-write",
                "network_access": true
            }
        }
    })]);
    let snapshot = CodexPermissionSnapshot::from_rollout(&path).unwrap();
    assert_eq!(snapshot.sandbox(), CodexSandboxMode::WorkspaceWrite);
    assert_eq!(snapshot.workspace_network(), Some(true));
}

#[test]
fn parses_workspace_network_disabled() {
    let path = rollout_with_contexts(&[serde_json::json!({
        "type": "turn_context",
        "payload": {
            "sandbox_policy": {
                "type": "workspace-write",
                "network_access": false
            }
        }
    })]);
    let snapshot = CodexPermissionSnapshot::from_rollout(&path).unwrap();
    assert_eq!(snapshot.workspace_network(), Some(false));
}

#[test]
fn parses_read_only_permissions() {
    let path = rollout_with_contexts(&[serde_json::json!({
        "type": "turn_context",
        "payload": {"sandbox_policy": {"type": "read-only"}}
    })]);
    assert_eq!(
        CodexPermissionSnapshot::from_rollout(&path)
            .unwrap()
            .sandbox(),
        CodexSandboxMode::ReadOnly
    );
}
```

- [ ] **Step 2: Run parser tests and verify red**

Run:

```bash
cargo test jdi::codex::tests::parses_ -- --nocapture
```

Expected: compilation fails because `CodexPermissionSnapshot` and
`CodexSandboxMode` do not exist.

- [ ] **Step 3: Implement normalized permission types and parser**

Add:

```rust
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodexSandboxMode {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

impl CodexSandboxMode {
    pub(crate) fn as_config_value(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::DangerFullAccess => "danger-full-access",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexPermissionSnapshot {
    sandbox: CodexSandboxMode,
    workspace_network: Option<bool>,
}
```

Implement `from_rollout` by reading the JSONL once, replacing an
`Option<serde_json::Value>` whenever a parsed event has `type == "turn_context"`,
then validating only that latest event. For `workspace-write`, require a boolean
`payload.sandbox_policy.network_access`; for the other two modes, normalize the
network field to `None`. Return an `anyhow` error containing
`cannot preserve the current Codex permission context` for missing or unsupported
data.

Implement `from_handoff_parts` with the same invariant: workspace-write requires
`Some(bool)` and other modes require `None`.

- [ ] **Step 4: Add fail-closed tests**

```rust
#[test]
fn invalid_latest_turn_does_not_reuse_older_full_access() {
    let path = rollout_with_contexts(&[
        serde_json::json!({
            "type": "turn_context",
            "payload": {"sandbox_policy": {"type": "danger-full-access"}}
        }),
        serde_json::json!({
            "type": "turn_context",
            "payload": {"sandbox_policy": {"type": "future-mode"}}
        }),
    ]);
    assert!(CodexPermissionSnapshot::from_rollout(&path).is_err());
}
```

Add:

```rust
#[test]
fn missing_or_incomplete_permissions_fail_closed() {
    let no_context = rollout_with_contexts(&[]);
    assert!(CodexPermissionSnapshot::from_rollout(&no_context).is_err());

    let missing_network = rollout_with_contexts(&[serde_json::json!({
        "type": "turn_context",
        "payload": {"sandbox_policy": {"type": "workspace-write"}}
    })]);
    assert!(CodexPermissionSnapshot::from_rollout(&missing_network).is_err());
}
```

The fixture already prefixes `not-json`, so every successful parser test also
proves malformed unrelated lines are ignored.

- [ ] **Step 5: Implement argument and status rendering**

Implement:

```rust
pub(crate) fn config_args(&self) -> Vec<String> {
    let mut args = vec![
        "-c".into(),
        format!("sandbox_mode=\"{}\"", self.sandbox.as_config_value()),
    ];
    if let Some(enabled) = self.workspace_network {
        args.extend([
            "-c".into(),
            format!("sandbox_workspace_write.network_access={enabled}"),
        ]);
    }
    args
}

pub(crate) fn summary(&self) -> String {
    match (self.sandbox, self.workspace_network) {
        (CodexSandboxMode::WorkspaceWrite, Some(true)) =>
            "workspace-write, network enabled".into(),
        (CodexSandboxMode::WorkspaceWrite, Some(false)) =>
            "workspace-write, network disabled".into(),
        (mode, None) => mode.as_config_value().into(),
        _ => unreachable!("constructor enforces network invariant"),
    }
}
```

Add exact-vector assertions for all three sandbox modes.

- [ ] **Step 6: Run Task 1 tests**

Run:

```bash
cargo test jdi::codex::tests -- --nocapture
```

Expected: all Codex adapter tests pass.

- [ ] **Step 7: Commit Task 1**

```bash
git add src/jdi/codex.rs
git commit -m "feat(jdi): parse Codex handoff permissions"
```

### Task 2: Make Codex invocation consume exactly one policy

**Files:**
- Modify: `src/jdi/codex.rs`
- Test: `src/jdi/codex.rs` inline tests

**Interfaces:**
- Consumes: `CodexPermissionSnapshot::config_args() -> Vec<String>`
- Produces: `CodexAdapter::unattended_config_args(extra_args: &[String]) -> Vec<String>`
- Preserves: `AgentAdapter::build_invocation` and `fresh_invocation` signatures

- [ ] **Step 1: Replace the existing invocation expectation with failing policy tests**

Add:

```rust
#[test]
fn resumed_invocation_uses_persisted_full_access_without_duplicate_sandbox() {
    let a = CodexAdapter;
    let brief = Brief::default();
    let persisted = CodexPermissionSnapshot::from_handoff_parts(
        CodexSandboxMode::DangerFullAccess,
        None,
    )
    .unwrap()
    .config_args();
    let inv = a.build_invocation(&ctx("sess-1", &brief, &persisted));
    let sandbox_values: Vec<_> = inv
        .args
        .windows(2)
        .filter(|pair| pair[0] == "-c" && pair[1].starts_with("sandbox_mode="))
        .map(|pair| pair[1].as_str())
        .collect();
    assert_eq!(sandbox_values, ["sandbox_mode=\"danger-full-access\""]);
}
```

Add a test that empty persisted args yield exactly one
`sandbox_mode="workspace-write"` and
`sandbox_workspace_write.network_access=false`.

- [ ] **Step 2: Run invocation tests and verify red**

Run:

```bash
cargo test jdi::codex::tests::resumed_invocation_ -- --nocapture
```

Expected: the full-access case sees both workspace-write and danger-full-access,
or the default case lacks the explicit network-disabled argument.

- [ ] **Step 3: Implement one-policy invocation construction**

Add a helper:

```rust
fn unattended_config_args(extra_args: &[String]) -> Vec<String> {
    if extra_args.is_empty() {
        return CodexPermissionSnapshot::from_handoff_parts(
            CodexSandboxMode::WorkspaceWrite,
            Some(false),
        )
        .expect("static default policy is valid")
        .config_args();
    }
    extra_args.to_vec()
}
```

In `build_invocation` and `fresh_invocation`, construct:

```rust
let mut args = vec![
    "exec".into(),
    // "resume" is inserted here only in build_invocation
    "-c".into(),
    "approval_policy=\"never\"".into(),
];
args.extend(Self::unattended_config_args(ctx.extra_args));
args.push("--json".into());
```

Do not append `ctx.extra_args` a second time.

- [ ] **Step 4: Verify fresh/external defaults and full-access resume**

Run:

```bash
cargo test jdi::codex::tests -- --nocapture
```

Expected: all Codex tests pass, including session capture and preflight tests.

- [ ] **Step 5: Commit Task 2**

```bash
git add src/jdi/codex.rs
git commit -m "fix(jdi): honor persisted Codex sandbox policy"
```

### Task 3: Capture, transport, and persist policy during handoff

**Files:**
- Modify: `src/jdi/mod.rs`
- Test: `src/jdi/mod.rs` inline tests

**Interfaces:**
- Consumes: `CodexPermissionSnapshot::from_rollout`
- Consumes: `CodexPermissionSnapshot::from_handoff_parts`
- Consumes: `CodexPermissionSnapshot::config_args`
- Produces: `codex_handoff_permissions(session_id: &str) -> Result<CodexPermissionSnapshot>`
- Produces: `persist_codex_permissions(session: &Session, agent: Agent, permissions: Option<&CodexPermissionSnapshot>) -> Result<()>`
- Extends: internal `Command::HandoffWait` with normalized Codex permission fields
- Extends: `cmd_resume` with `handoff_permissions: Option<&CodexPermissionSnapshot>`

- [ ] **Step 1: Write failing persistence tests**

Add:

```rust
#[test]
fn handoff_permissions_are_persisted_and_external_resume_clears_them() {
    let root = std::env::temp_dir().join(format!(
        "agent-jdi-permission-state-{}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&root).ok();
    let session = Session::new(&root, "slot");
    let full = codex::CodexPermissionSnapshot::from_handoff_parts(
        codex::CodexSandboxMode::DangerFullAccess,
        None,
    )
    .unwrap();

    persist_codex_permissions(&session, Agent::Codex, Some(&full)).unwrap();
    assert_eq!(
        std::fs::read_to_string(session.cargs_path()).unwrap(),
        "-c\nsandbox_mode=\"danger-full-access\"\n"
    );
    assert!(session
        .meta_get("permissions")
        .unwrap()
        .contains("danger-full-access"));

    persist_codex_permissions(&session, Agent::Codex, None).unwrap();
    assert!(!session.cargs_path().exists());
    assert_eq!(
        session.meta_get("permissions").as_deref(),
        Some("workspace-write, network disabled (default)")
    );
}
```

Add a Claude test proving the helper neither creates nor removes Claude `cargs`.

- [ ] **Step 2: Run persistence tests and verify red**

Run:

```bash
cargo test jdi::tests::handoff_permissions_ -- --nocapture
```

Expected: compilation fails because `persist_codex_permissions` does not exist.

- [ ] **Step 3: Implement state persistence**

Implement `persist_codex_permissions` next to `cmd_resume`:

```rust
fn persist_codex_permissions(
    session: &Session,
    agent: Agent,
    permissions: Option<&codex::CodexPermissionSnapshot>,
) -> Result<()> {
    if agent != Agent::Codex {
        return Ok(());
    }
    match permissions {
        Some(snapshot) => {
            let args = snapshot.config_args();
            std::fs::write(session.cargs_path(), format!("{}\n", args.join("\n")))?;
            session.meta_set(
                "permissions",
                &format!("{} (preserved from current Codex turn)", snapshot.summary()),
            )?;
        }
        None => {
            match std::fs::remove_file(session.cargs_path()) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            session.meta_set(
                "permissions",
                "workspace-write, network disabled (default)",
            )?;
        }
    }
    Ok(())
}
```

Call it after `session.ensure_dir()` and before supervisor spawn in `cmd_resume`.
Pass `None` from the public `Resume` dispatch and the optional snapshot from
`cmd_handoff_wait`.

- [ ] **Step 4: Write failing capture/transport tests**

Add the watcher-argument test below. Rollout resolution is exercised by
`codex_handoff_permissions`, whose focused test sets `CODEX_SESSIONS_DIR` to a
temporary fixture containing the pinned session ID and restores the environment
after the assertion:

```rust
#[test]
fn watcher_args_carry_normalized_workspace_network_policy() {
    let snapshot = codex::CodexPermissionSnapshot::from_handoff_parts(
        codex::CodexSandboxMode::WorkspaceWrite,
        Some(true),
    )
    .unwrap();
    let args = handoff_permission_args(Some(&snapshot));
    assert_eq!(
        args,
        [
            "--codex-sandbox",
            "workspace-write",
            "--codex-workspace-network",
            "true",
        ]
    );
}
```

- [ ] **Step 5: Extend the internal watcher CLI**

Add to `Command::HandoffWait`:

```rust
#[arg(long, value_enum)]
codex_sandbox: Option<codex::CodexSandboxMode>,
#[arg(long, requires = "codex_sandbox")]
codex_workspace_network: Option<bool>,
```

Add the same values to `cmd_handoff_wait`. Reconstruct the snapshot with
`CodexPermissionSnapshot::from_handoff_parts`; reject network values for
non-workspace modes.

- [ ] **Step 6: Capture policy before watcher spawn**

After pinning the session ID in `cmd_handoff`, compute:

```rust
let codex_permissions = match (found, session_id.as_deref()) {
    (Some((_, Agent::Codex)), Some(id)) => {
        let transcript = agent::adapter(Agent::Codex)
            .transcript_path(id, &cwd)
            .with_context(|| format!("locate pinned Codex rollout {id}"))?;
        Some(codex::CodexPermissionSnapshot::from_rollout(&transcript)?)
    }
    (Some((_, Agent::Codex)), None) => {
        bail!("handoff aborted: cannot pin the current Codex thread")
    }
    _ => None,
};
```

Append only normalized watcher arguments from `handoff_permission_args`.
Perform capture before `cmd.spawn()` and before signaling `watch_pid`, so every
error leaves the interactive session alive.

- [ ] **Step 7: Show permission policy in dry-run and real handoff output**

For Codex, print:

```text
permissions: preserving danger-full-access from the current Codex turn
approvals:   never (unattended)
```

Keep Claude output unchanged apart from existing lines.

- [ ] **Step 8: Run handoff/state tests**

Run:

```bash
cargo test jdi::tests -- --nocapture
cargo test jdi::supervisor::tests -- --nocapture
```

Expected: all JDI command and supervisor tests pass.

- [ ] **Step 9: Commit Task 3**

```bash
git add src/jdi/mod.rs
git commit -m "fix(jdi): preserve Codex permissions through handoff"
```

### Task 4: Expose the effective policy and document the contract

**Files:**
- Modify: `src/jdi/mod.rs`
- Modify: `src/jdi/DESIGN.md`
- Modify: `integrations/README.md`
- Modify: `integrations/shared/skills/jdi-handoff/SKILL.md`
- Test: `src/jdi/mod.rs` inline tests
- Test: `tests/install_jdi_handoff.sh`

**Interfaces:**
- Consumes: session metadata key `permissions`
- Produces: `agent-jdi status` line `permissions: ...`

- [ ] **Step 1: Write failing status rendering test**

Add:

```rust
fn permission_status_line(session: &Session) -> Option<String> {
    session
        .meta_get("permissions")
        .map(|value| format!("permissions: {value}"))
}

#[test]
fn status_reports_permissions() {
    let root = std::env::temp_dir().join(format!(
        "agent-jdi-status-permissions-{}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&root).ok();
    let session = Session::new(&root, "slot");
    session
        .meta_set(
            "permissions",
            "danger-full-access (preserved from current Codex turn)",
        )
        .unwrap();
    assert_eq!(
        permission_status_line(&session).as_deref(),
        Some("permissions: danger-full-access (preserved from current Codex turn)")
    );
}
```

Call `permission_status_line` from `cmd_status` immediately after rendering
`mode`.

- [ ] **Step 2: Run the status test and verify red**

Run:

```bash
cargo test jdi::tests::status_reports_permissions -- --nocapture
```

Expected: FAIL because status does not print the metadata key.

- [ ] **Step 3: Print stored permissions in status**

In `cmd_status`, after `mode`, add:

```rust
if let Some(permissions) = session.meta_get("permissions") {
    println!("permissions: {permissions}");
}
```

Keep old tracked sessions compatible by omitting the line when the key is absent.

- [ ] **Step 4: Update durable and user-facing documentation**

Add these exact behavior statements to all three relevant docs, using each
document's existing voice:

- handoff preserves the pinned Codex turn's sandbox and workspace-network policy;
- capture failure leaves the interactive session running;
- external start/resume keep the safe default;
- retry/backlog reuse the tracked handoff policy;
- no extra Skill flag is required.

Add an installer assertion that the installed shared Skill contains
`preserves the current Codex permission context`, proving the canonical Skill
text reaches both clients.

- [ ] **Step 5: Run documentation/integration tests**

Run:

```bash
cargo test jdi::tests::status_reports_permissions -- --nocapture
bash tests/install_jdi_handoff.sh
bash -n integrations/install-jdi-handoff.sh tests/install_jdi_handoff.sh
```

Expected: all commands exit zero and installer prints
`shared jdi-handoff installer: ok`.

- [ ] **Step 6: Commit Task 4**

```bash
git add src/jdi/mod.rs src/jdi/DESIGN.md integrations/README.md \
  integrations/shared/skills/jdi-handoff/SKILL.md tests/install_jdi_handoff.sh
git commit -m "docs(jdi): explain preserved Codex permissions"
```

### Task 5: Full verification and disposable real-Codex smoke

**Files:**
- Do not create new production files. If a verification failure requires a code
  change, return to the owning task's red-green cycle and amend only that task's
  listed files.
- Create no committed smoke-test artifacts.

**Interfaces:**
- Consumes: compiled `target/debug/agent-jdi`
- Produces: test evidence for user review

- [ ] **Step 1: Run formatting and diff validation**

Run:

```bash
cargo fmt --check
git diff --check
```

Expected: both exit zero with no output from `git diff --check`.

- [ ] **Step 2: Run the complete Rust suite**

Run:

```bash
env -u AGENT_JDI_CLAUDE_BIN cargo test --all
```

Expected: every test passes; record the exact pass count.

- [ ] **Step 3: Run clippy with warnings denied**

Run:

```bash
cargo clippy --all-targets -- -D warnings
```

Expected: exit zero with no warnings.

- [ ] **Step 4: Run integration scripts**

Run:

```bash
bash tests/install_jdi_handoff.sh
bash -n integrations/install-jdi-handoff.sh tests/install_jdi_handoff.sh
```

Expected: installer test prints `shared jdi-handoff installer: ok`; syntax check
exits zero.

- [ ] **Step 5: Build the exact binary under test**

Run:

```bash
cargo build --bin agent-jdi
```

Expected: `target/debug/agent-jdi` is created successfully.

- [ ] **Step 6: Run a workspace-write dry-run against a disposable Codex rollout**

Run the focused fixture-backed test created in Task 3:

```bash
cargo test jdi::tests::watcher_args_carry_normalized_workspace_network_policy \
  -- --nocapture
```

Expected dry-run output contains:

```text
permissions: preserving workspace-write, network disabled from the current Codex turn
```

- [ ] **Step 7: Run a danger-full-access real Codex smoke in a disposable repository**

Use this command skeleton, substituting the actual absolute binary and temporary
paths produced by the shell:

```bash
smoke_root=$(mktemp -d "${TMPDIR:-/tmp}/agent-jdi-codex-permissions.XXXXXX")
smoke_repo="$smoke_root/repo"
smoke_state="$smoke_root/state"
git init "$smoke_repo"
smoke_binary="$PWD/target/debug/agent-jdi"
smoke_prompt="In this disposable repository only: first record DNS resolution for gitlab.alibaba-inc.com and whether .git/agent-jdi-before.lock can be created and removed. Then run '$smoke_binary handoff --armed' with the instruction to repeat the same probes into after.txt, print the active sandbox mode, and stop. After arming handoff, finish this turn immediately. Do not configure remotes, push, browse, or touch paths outside '$smoke_repo' and '$smoke_state'."
AGENT_JDI_HOME="$smoke_state" codex exec \
  -C "$smoke_repo" \
  -c 'approval_policy="never"' \
  -c 'sandbox_mode="danger-full-access"' \
  --json \
  "$smoke_prompt"
AGENT_JDI_HOME="$smoke_state" "$smoke_binary" list
```

Poll the single tracked slot with `agent-jdi status <slot>` until it reaches
`done`, `failed`, or `gaveup`, never waiting more than 10 seconds per poll. The
dedicated prompt only:

- resolves `gitlab.alibaba-inc.com`;
- performs a read-only TCP/SSH reachability probe;
- creates and removes a uniquely named `.git` lock probe;
- reports the active sandbox from its transcript context.

Use a temporary `AGENT_JDI_HOME` and the built binary. Do not configure a remote,
push, open a browser, or touch any production repository.

Expected:

- handoff resumes the same Codex thread ID;
- post-handoff `turn_context.sandbox_policy.type` remains
  `danger-full-access`;
- DNS/TCP behavior matches before handoff;
- the temporary `.git` probe remains writable;
- the tracked session metadata reports preserved full access.

- [ ] **Step 8: Inspect final branch scope**

Run:

```bash
git status --short --branch
git log --oneline --decorate --max-count=8
git diff --stat 9cce6aa^..HEAD
git diff --check 9cce6aa^..HEAD
```

Expected: clean worktree; only the design, plan, JDI permission implementation,
tests, and directly related documentation differ from the stacked base.

- [ ] **Step 9: Prepare user review package**

Report:

- commit list;
- exact files changed;
- full test commands and results;
- real-smoke evidence;
- known limitations, especially named beta permission profiles and existing
  Codex nonzero-exit retry classification;
- confirmation that no remote branch or PR was changed.

Do not push or fold the commits into PR #4 until the user explicitly approves.
