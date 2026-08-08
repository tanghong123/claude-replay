# Study #121 — should QoderWork's `TodoWrite` be normalized into the task model?

**Verdict: yes, it is worth surfacing — but NOT through the existing op-log path.** `TodoWrite`
is a *snapshot* tool; the task model (#15) is an *op-log*. They share the presentation
vocabulary (`TaskList`/`TaskItem`) but not the fold. Normalizing means adding a new
replace-all op, which carries three real costs — so it is filed as its own implementation task
rather than done under a study.

## What the two shapes actually are

| | `TaskCreate` / `TaskUpdate` (Claude task system, #15) | `TodoWrite` (QoderWork) |
|---|---|---|
| semantics | **op-log** — incremental events replayed into state | **snapshot** — each call is the WHOLE list, last-write-wins |
| identity | server-assigned id, arriving in the tool RESULT text (`Created task #12`), joined by `tool_use_id` (`TaskOp::Resolve`, #96) | **no ids** — a bare `todos: [{description, status}]` array |
| fields | `subject`, `description`, `active_form`, `status`, `blocks`/`blocked_by` | `description` + `status` only |
| cadence | a handful of ops over a session | **268 calls in one real session** — rewritten constantly |

Real `TodoWrite` input (QoderWork, verbatim):

```json
{"name":"TodoWrite","input":{"todos":[
  {"description":"读取简历页面内容","status":"in_progress"},
  {"description":"读取JD页面内容","status":"pending"},
  {"description":"对比分析并输出初筛结论","status":"pending"}]}}
```

## Why it matters (the panel is dead today)

Measured on this machine:

- QoderWork emits **zero** `TaskCreate`/`TaskUpdate` across every transcript, and has **no**
  on-disk task queue (nothing like `~/.claude/tasks/`). Both of the panel's feeds (#15: the
  op-log fold + the `load_tasks` disk hook) are therefore empty — **the task/todo panel is
  always blank for QoderWork**, even though the agent maintains a rich, 268-update todo list
  in-band. That live list is exactly what the panel exists to show.
- `TodoWrite` is unhandled for **every** agent: the Claude tokenizer maps only
  `TaskCreate`/`TaskUpdate` (`agents/claude/model.rs`), never `TodoWrite`.

## Why not just map it onto `Create`/`Update`

A snapshot is not a sequence of creates/updates. It has no stable ids to `Update` against
(item 0 of call N is unrelated to item 0 of call N+1), and it must be able to *shrink* the list
(a todo dropped between snapshots), which the append-only op-log cannot express. Forcing it in
would either strand every item in `pending` (no `Resolve` ids) or leave deleted todos behind.

## Recommended shape (for the implementation task)

Add a replace-all op — snapshots stay honest about being snapshots:

```rust
enum TaskOp {
    Create { … }, Resolve { … }, Update { … },   // existing op-log
    Snapshot { todos: Vec<(String /*description*/, String /*status*/)> },  // NEW
}
```

- **Tokenizer**: map `TodoWrite` → `TaskOp::Snapshot` (the only place tool inputs are seen).
- **`TaskFold::apply(Snapshot)`**: clear `list`, rebuild `TaskItem`s with synthetic index ids
  (`"0"`, `"1"`, …), `subject = description`, no `blocks`/`active_form`. Last snapshot wins.
- **Dedup**: skip recording a `Snapshot` equal to the current list — 268 calls are mostly
  no-op rewrites, and the op is persisted into the meta stream (#96); don't bloat it.
- **Presentation** already degrades gracefully: a `TaskItem` with an index id and empty
  `blocks`/`active_form` renders fine in both frontends' panels.

## The three costs — carry these into the implementation task

1. **FOLD_VERSION bump (3 → 4).** Populating a previously-empty task list changes folded
   output, so every user's durable cache must re-fold once (a one-time reparse, not data loss).
   Standard cost of any fold-output change; acceptable, but it reprices **all** caches, not just
   QoderWork's.
2. **Clobber hazard — theoretical, not observed.** A `Snapshot` replaces the whole list, so a
   session mixing `TaskCreate` AND `TodoWrite` would have its op-log list wiped. Measured:
   **0 of 133** Claude transcripts contain `TodoWrite`, and **0** mix both — modern Claude Code
   uses `TaskCreate`/`TaskUpdate` exclusively (this repo's own sessions included). Still, guard
   it: only let a `Snapshot` replace a list that is empty or was itself snapshot-built, so a
   future mixed session degrades safely instead of silently losing tasks.
3. **Shared-decoder reach.** QoderWork delegates `decode_line` to Claude's tokenizer, so mapping
   `TodoWrite` there is the DRY, agent-neutral choice and automatically covers QoderWork. It is
   inert for Claude *today* (0/133), but it couples future behavior: if Claude re-introduces
   `TodoWrite`, panels light up without a further change. That is the intended agent-neutral
   contract (a tool name means the same thing across Claude-format agents), so map it in the
   shared decoder deliberately — not with a QoderWork-only special case that would break the
   clean delegation.

## Conclusion

Normalize, via `TaskOp::Snapshot`, in a dedicated implementation task — not inline under this
study. The win (a live, 268-entry todo list where the panel is blank today) is real; the costs
(one FOLD_VERSION bump, a guarded replace-all, a deliberate shared-decoder mapping) are bounded
and understood.
