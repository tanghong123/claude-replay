# Preserve Codex Permissions Across JDI Handoff

## Problem

`agent-jdi handoff` is intended to transfer the current interactive session to an
unattended worker without changing which session is running or what work remains.
For Codex, the resumed invocation currently hard-codes:

```text
approval_policy="never"
sandbox_mode="workspace-write"
```

That changes the execution boundary during the transfer. A Codex turn that started
with `danger-full-access` resumes with network disabled and protected `.git`
metadata, so remote access, commits, pushes, and writes to neighboring repositories
fail after handoff.

The detached watcher inherits the parent process environment. The regression is
therefore not caused by detachment or lost proxy variables; it is caused by the
Codex adapter replacing the active sandbox policy.

## Goals

- Preserve the active Codex turn's effective sandbox mode when
  `agent-jdi handoff` transfers that turn to the unattended worker.
- Preserve the workspace-write network setting when it is present.
- Never grant more filesystem or network access than the interactive turn had.
- Refuse to terminate the interactive session when its active policy cannot be
  read, rather than silently running the worker with different permissions.
- Persist the chosen policy across supervisor retries and backlog drains.
- Show the effective unattended policy before the interactive session exits and
  in tracked-session status.
- Keep Claude behavior unchanged.
- Keep `agent-jdi start` and an externally launched `agent-jdi resume` on their
  current safe defaults.

## Non-goals

- Reconstruct arbitrary beta named permission profiles from transcript data.
- Change Codex's approval policy for unattended work; it remains `never`.
- Make `workspace-write` able to modify protected `.git` metadata.
- Enable network globally in the user's Codex configuration.
- Broaden filesystem access to neighboring repositories that the interactive turn
  could not already write.
- Push the implementation to PR #4 before local tests and user review.

## Security invariant

Handoff may preserve or reduce the current execution boundary, but it must not
increase it.

The supported mappings are:

| Interactive turn | Unattended turn |
|---|---|
| `danger-full-access` | `danger-full-access` |
| `workspace-write`, network enabled | `workspace-write`, network enabled |
| `workspace-write`, network disabled | `workspace-write`, network disabled |
| `read-only` | `read-only` |
| Missing, malformed, or unsupported context | Abort handoff; keep the interactive session running |

The transfer remains an explicit unattended action: the user invoked the handoff
Skill, and the command prints the policy it will preserve before terminating the
interactive process.

## Design

### 1. Represent the captured Codex policy

Add a small Codex-specific value type:

```rust
enum CodexSandboxMode {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

struct CodexPermissionSnapshot {
    sandbox: CodexSandboxMode,
    workspace_network: Option<bool>,
    source: PermissionSource,
}
```

`workspace_network` is meaningful only for `WorkspaceWrite`. `PermissionSource`
records that the policy came from the pinned turn so dry-run, handoff output,
metadata, and tests can explain the decision.

The type owns three operations:

- parse the latest usable `turn_context` from a Codex rollout;
- render the exact Codex config arguments needed by the worker;
- render a stable human-readable status summary.

It stays in the JDI Codex adapter rather than the generic replay model because it
is an execution-policy decision specific to unattended Codex invocation.

### 2. Capture at handoff time

`cmd_handoff` already pins the exact Codex thread before launching the watcher.
After resolving that thread, it resolves the thread's rollout path and scans the
JSONL for `turn_context` events. The last syntactically valid `turn_context` event
is authoritative because it represents the permission selection active for the
turn that invoked handoff. If that event contains an unsupported or malformed
policy, handoff aborts; it must not reuse an older, potentially more permissive
turn.

The parser reads:

```text
payload.sandbox_policy.type
payload.sandbox_policy.network_access
```

Accepted sandbox values are `read-only`, `workspace-write`, and
`danger-full-access`. The network value must be a JSON boolean. Unknown values,
missing fields, or an unreadable rollout abort Codex handoff before the watcher is
spawned or the interactive process is signaled.

Claude skips this capture and keeps its current invocation.

### 3. Carry the snapshot through the detached watcher

The public `handoff` process passes a normalized sandbox value and optional
workspace-network value to the internal `__handoff` watcher. It does not pass raw
JSON or arbitrary shell text.

The watcher passes the normalized snapshot to `cmd_resume`. `cmd_resume` persists:

- normalized Codex config arguments in the tracked session's existing `cargs`
  file, one argument per line;
- a human-readable policy summary and source in session metadata.

The supervisor already reloads `cargs` for every invocation. Consequently the
policy survives:

- the initial resume;
- recoverable retries;
- backlog drains launched from the same tracked session.

Public `start` and external `resume` supply no captured snapshot and retain the
adapter's existing safe default. An external resume also clears stale `cargs`
from an earlier handoff before launching its worker, so full access cannot leak
from a tracked handoff into a later, independently requested resume. Backlog
drains do not clear `cargs` because they are continuations of the same tracked
unattended run.

### 4. Build one unambiguous Codex invocation

The Codex adapter continues to set `approval_policy="never"`.

For a handoff with persisted permission arguments, the invocation uses the
captured sandbox arguments instead of also emitting the hard-coded
`workspace-write` value. For all other runs it emits the existing safe default:

```text
sandbox_mode="workspace-write"
sandbox_workspace_write.network_access=false
```

For workspace-write with network enabled, it emits:

```text
sandbox_mode="workspace-write"
sandbox_workspace_write.network_access=true
```

For read-only or danger-full-access, it emits only the sandbox mode because the
workspace network setting does not apply.

The implementation must not rely on duplicate `-c sandbox_mode=...` arguments and
last-value-wins behavior.

### 5. User-visible evidence

`agent-jdi handoff --dry-run` and a real handoff print the captured policy:

```text
permissions: preserving danger-full-access from the current Codex turn
approvals:   never (unattended)
```

If capture fails, handoff returns an error without spawning a watcher or
terminating the current process:

```text
handoff aborted: cannot preserve the current Codex permission context
```

Tracked session metadata stores the same normalized summary. `agent-jdi status`
shows it so a completed or retrying run can be audited without reopening the
transcript.

No secrets, raw permission-profile JSON, or environment values are written to the
state directory.

## Error handling

- Missing rollout: abort before watcher spawn and leave the current session alive.
- Malformed JSON line: skip the line and continue scanning.
- Malformed or unsupported policy in the latest `turn_context`: abort rather than
  reusing an older policy or guessing.
- Failure to persist worker arguments or metadata: abort before spawning the
  supervisor, because running with a different policy would violate the transfer
  contract.
- Admin policy rejecting the preserved mode follows the supervisor's existing
  Codex failure classification; changing that taxonomy is outside this fix.

## Testing strategy

### Unit tests

- Parse `danger-full-access` from a realistic `turn_context`.
- Parse workspace-write with network `true` and `false`.
- Parse `read-only`.
- Select the latest `turn_context` and reject an invalid latest policy rather than
  reusing an older, more permissive one.
- Ignore malformed unrelated JSONL lines.
- Fail closed for missing, malformed, and unsupported policy data.
- Render each policy into one non-duplicated Codex invocation.
- Preserve `approval_policy="never"`.
- Keep fresh `start` and external `resume` on workspace-write/network-disabled
  defaults.

### Handoff and supervisor tests

- The public handoff pins the session and forwards a normalized policy.
- The internal watcher persists the policy before supervisor launch.
- Retry reuses the same `cargs`.
- Backlog drain reuses the same `cargs`.
- External resume clears stale handoff `cargs`.
- `status` reports the stored policy and source.
- Claude handoff arguments and behavior are unchanged.

### Repository verification

Run:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
env -u AGENT_JDI_CLAUDE_BIN cargo test --all
bash tests/install_jdi_handoff.sh
bash -n integrations/install-jdi-handoff.sh tests/install_jdi_handoff.sh
git diff --check
```

### Real Codex smoke test

Use a disposable temporary Git repository and a dedicated Codex session. Compare
the transcript's effective `turn_context` before and after handoff for:

1. workspace-write/network-disabled;
2. danger-full-access.

The smoke test checks:

- the same thread ID resumes;
- the sandbox mode is preserved;
- DNS and a read-only TCP probe behave consistently before and after handoff;
- a temporary repository can create and remove `.git/index.lock` only when the
  original turn could;
- no production repository, remote, branch, or pull request is modified.

The implementation remains on the local stacked branch until the complete diff and
test evidence have been reviewed and explicitly approved by the user.

## Delivery

The work is based on the local PR #4 branch but isolated on
`fix/jdi-preserve-codex-permissions`. No commit is pushed or added to PR #4 during
implementation and verification.

After review approval, the implementation can be folded into PR #4 as focused
commits because it corrects the handoff behavior introduced and documented there.
If the owner prefers a smaller PR, the same commits can remain as a stacked
follow-up without changing their content.
