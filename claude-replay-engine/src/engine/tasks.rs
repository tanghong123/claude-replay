//! The **agent-neutral task/todo model** (#15): the [`TaskItem`]/[`TaskList`] vocabulary,
//! the op-log fold (replaying `TaskCreate`/`TaskUpdate` calls from the transcript into
//! point-in-time state), and the live/op-log merge. Per-agent knowledge stays thin and
//! elsewhere: Claude task calls and Codex plan/goal calls emit `Message::TaskOp`s
//! (only the tokenizer sees tool inputs), while the Claude adapter's `load_tasks` hook reads
//! the on-disk `~/.claude/tasks/<session-id>/*.json` files — the same
//! `TranscriptAdapter`-seam pattern as `subagent_source`. Codex has no independent side store;
//! its task panel is reconstructed from transcript snapshots.

use serde::{Deserialize, Serialize};

/// A task's lifecycle state. Unknown strings map to [`TaskStatus::Pending`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TaskStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
}

impl TaskStatus {
    pub fn parse(s: &str) -> TaskStatus {
        match s {
            "in_progress" => TaskStatus::InProgress,
            "completed" => TaskStatus::Completed,
            _ => TaskStatus::Pending,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            TaskStatus::Pending => "pending",
            TaskStatus::InProgress => "in_progress",
            TaskStatus::Completed => "completed",
        }
    }
}

/// One task — the neutral shape both sources map onto (the op-log fold and the
/// on-disk task files share it; disk adds nothing the type doesn't carry).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TaskItem {
    /// The queue id (numeric-in-a-string in Claude's scheme, e.g. `"52"`).
    pub id: String,
    pub subject: String,
    pub description: String,
    /// The in-progress spinner form ("Fixing the parser"), when recorded.
    pub active_form: String,
    pub status: TaskStatus,
    /// Ids this task blocks / is blocked by (dependency edges).
    pub blocks: Vec<String>,
    pub blocked_by: Vec<String>,
}

/// The session's task list, ordered by numeric id (falling back to string order).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TaskList {
    pub items: Vec<TaskItem>,
}

impl TaskList {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
    fn sort(&mut self) {
        self.items
            .sort_by(|a, b| match (a.id.parse::<u64>(), b.id.parse::<u64>()) {
                (Ok(x), Ok(y)) => x.cmp(&y),
                (Ok(_), Err(_)) => std::cmp::Ordering::Less,
                (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
                (Err(_), Err(_)) => a.id.cmp(&b.id),
            });
    }
}

/// One task operation, as the L1 tokenizer saw it in the transcript — the unit the
/// [`TaskFold`] replays. Field options are "absent from the call input".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TaskOp {
    /// A `TaskCreate` call. The assigned id arrives later, in the RESULT text
    /// ("Created task #12: …"), joined by `tool_use_id`.
    Create {
        tool_use_id: String,
        subject: String,
        description: String,
        active_form: String,
        blocked_by: Vec<String>,
    },
    /// A `TaskUpdate` call — `task_id` from the input; only present fields change.
    /// The create→id join, recorded as an OP (#96). The id arrives in the tool RESULT
    /// ("Created task #12: …"), which is transcript data rather than a task op — so without
    /// this a replay strands every create in `pending` and rebuilds an EMPTY list, since
    /// `Update{task_id}` targets items that never landed. `None` ⇒ the create failed and its
    /// draft is dropped, which is what `on_tool_result` already does on an unparsable id.
    Resolve {
        tool_use_id: String,
        id: Option<String>,
    },
    Update {
        task_id: String,
        status: Option<String>,
        subject: Option<String>,
        description: Option<String>,
        active_form: Option<String>,
        add_blocks: Vec<String>,
        add_blocked_by: Vec<String>,
    },
    /// A `TodoWrite` call — the whole list, replacing whatever was there (#126).
    ///
    /// A SNAPSHOT, not an increment, and that is the point: `TodoWrite` carries no ids, so
    /// item *k* of one call has no relation to item *k* of the next, and the list must be
    /// able to SHRINK when a todo is dropped. An append-only op-log can express neither, so
    /// forcing it onto `Create`/`Update` would either strand everything in `pending` (no
    /// `Resolve` ids exist) or leave deleted todos behind forever.
    Snapshot { todos: Vec<Todo> },
}

/// One entry of a [`TaskOp::Snapshot`] — the neutral shape a `TodoWrite` item maps onto.
///
/// A struct rather than a tuple because this is a PERSISTED schema (#96): the meta stream
/// carries these, and a named field can be added later without the reader guessing which
/// slot moved.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct Todo {
    /// The todo's text. The tool spells this `description` or `content` depending on the
    /// caller's version — measured across 6323 real items, 5285 vs 1048, never both.
    pub text: String,
    pub status: String,
    /// The in-progress phrasing ("正在读取…"), when the caller sends one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub active_form: String,
}

/// The op-log reducer: replays [`TaskOp`]s (and watches tool results for the create→id
/// join) into a point-in-time [`TaskList`]. Maintained by the accumulator as messages
/// fold — live sessions grow it; a finished transcript yields its final state.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TaskFold {
    list: TaskList,
    /// `TaskCreate` calls whose id hasn't arrived yet: `tool_use_id` → draft item. **Derived,
    /// never persisted** (#96): it is exactly the creates with no matching `Resolve`.
    pending: Vec<(String, TaskItem)>,
    /// Ops applied since the last [`drain_recorded`](Self::drain_recorded) — the meta record's
    /// delta. Not part of the fold's value, so it is skipped by serde and by `PartialEq`.
    #[serde(skip)]
    recorded: Vec<TaskOp>,
}

impl TaskFold {
    /// Apply an op, and RECORD it for the meta stream (#96) — the caller drains
    /// `recorded` at each committing drain. Separated from [`apply_recorded`](Self::apply_recorded) so a replay of
    /// the persisted log does not re-record what it is replaying.
    pub fn apply(&mut self, op: &TaskOp) {
        // A `Snapshot` that says nothing new is not recorded (#126). `TodoWrite` is rewritten
        // constantly — 268 calls in one measured session, most of them the identical list —
        // and every recorded op is persisted into the meta stream (#96). Dropping the no-ops
        // keeps the stream proportional to what actually CHANGED. Applying it is still fine
        // (it is idempotent), so only the recording is skipped.
        if let TaskOp::Snapshot { .. } = op {
            let mut probe = self.clone();
            probe.apply_recorded(op);
            if probe.list == self.list {
                return;
            }
        }
        self.recorded.push(op.clone());
        self.apply_recorded(op);
    }

    /// Apply without recording — the replay path.
    pub fn apply_recorded(&mut self, op: &TaskOp) {
        match op {
            TaskOp::Create {
                tool_use_id,
                subject,
                description,
                active_form,
                blocked_by,
            } => {
                self.pending.push((
                    tool_use_id.clone(),
                    TaskItem {
                        id: String::new(),
                        subject: subject.clone(),
                        description: description.clone(),
                        active_form: active_form.clone(),
                        status: TaskStatus::Pending,
                        blocks: Vec::new(),
                        blocked_by: blocked_by.clone(),
                    },
                ));
            }
            TaskOp::Resolve { tool_use_id, id } => self.join(tool_use_id, id.as_deref()),
            TaskOp::Update {
                task_id,
                status,
                subject,
                description,
                active_form,
                add_blocks,
                add_blocked_by,
            } => {
                let items = &mut self.list.items;
                let item = match items.iter_mut().find(|t| &t.id == task_id) {
                    Some(t) => t,
                    None => {
                        // An update for a task we never saw created (created before
                        // this transcript / by another session) — materialize a stub
                        // so its status still shows.
                        //
                        // The subject stays EMPTY on purpose (#125). It is not recoverable:
                        // the tool result carries only "Updated task #5 status", and the
                        // on-disk store is keyed by session with per-queue integer ids that
                        // COLLIDE — measured, `#8` is "P1: agent-agnostic applications" in
                        // one queue and "Fix pre-existing docscroll fixture regression" in
                        // another. Scanning sibling task directories for a matching id would
                        // attach a confidently WRONG title, which is worse than none. The
                        // frontends render the absence honestly instead.
                        items.push(TaskItem {
                            id: task_id.clone(),
                            ..TaskItem::default()
                        });
                        items.last_mut().unwrap()
                    }
                };
                if let Some(s) = status {
                    item.status = TaskStatus::parse(s);
                }
                if let Some(s) = subject {
                    item.subject = s.clone();
                }
                if let Some(s) = description {
                    item.description = s.clone();
                }
                if let Some(s) = active_form {
                    item.active_form = s.clone();
                }
                for b in add_blocks {
                    if !item.blocks.contains(b) {
                        item.blocks.push(b.clone());
                    }
                }
                for b in add_blocked_by {
                    if !item.blocked_by.contains(b) {
                        item.blocked_by.push(b.clone());
                    }
                }
                self.list.sort();
            }
            TaskOp::Snapshot { todos } => {
                // Replace-all — but only over a list this op is entitled to own. A session
                // mixing `TaskCreate` with `TodoWrite` would otherwise have its op-log list
                // wiped by the first snapshot. Measured across 133 Claude transcripts: none
                // use `TodoWrite`, and none mix the two — so this guard is for the future,
                // and it degrades by ignoring the snapshot rather than by losing tasks.
                if !Self::is_snapshot_built(&self.list) {
                    return;
                }
                self.list.items = todos
                    .iter()
                    .enumerate()
                    .map(|(i, t)| TaskItem {
                        id: i.to_string(),
                        subject: t.text.clone(),
                        active_form: t.active_form.clone(),
                        status: TaskStatus::parse(&t.status),
                        ..TaskItem::default()
                    })
                    .collect();
                // Already in index order, and `sort` would reorder "10" before "2" only if
                // the ids were non-numeric — they are indices, so sorting is a no-op that
                // keeps the invariant honest if that ever changes.
                self.list.sort();
            }
        }
    }

    /// Whether `list` is one a [`Snapshot`](TaskOp::Snapshot) may replace: empty, or built by
    /// an earlier snapshot.
    ///
    /// DERIVED rather than tracked, on purpose. A flag would have to survive the checkpoint
    /// path, which serializes `TaskFold` wholesale — a `#[serde(skip)]` field would silently
    /// come back `false` after a checkpoint and quietly stop the todo list updating. The
    /// shape of the ids answers the question with no state at all: a snapshot numbers its
    /// items `0..n`, while op-log ids are server-assigned and start at **1**, so any real
    /// op-log list fails at index 0.
    fn is_snapshot_built(list: &TaskList) -> bool {
        list.items
            .iter()
            .enumerate()
            .all(|(i, t)| t.id == i.to_string())
    }

    /// Feed every tool result through here: a pending `TaskCreate`'s result text
    /// ("Created task #12: …") assigns the draft its id and lands it in the list.
    ///
    /// The join is RECORDED as a [`TaskOp::Resolve`] (#96), because the id arrives in
    /// transcript data rather than in an op — without it a replay of the log strands every
    /// create in `pending` and rebuilds an empty list.
    pub fn on_tool_result(&mut self, tool_use_id: &str, text: &str) {
        if !self.pending.iter().any(|(id, _)| id == tool_use_id) {
            return; // not a create we are waiting on — record nothing
        }
        let id = text
            .split('#')
            .nth(1)
            .map(|r| {
                r.chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect::<String>()
            })
            .filter(|s| !s.is_empty());
        self.apply(&TaskOp::Resolve {
            tool_use_id: tool_use_id.to_string(),
            id,
        });
    }

    /// Land a pending create under `id`, or drop it when the create failed (`None`).
    fn join(&mut self, tool_use_id: &str, id: Option<&str>) {
        let Some(pos) = self.pending.iter().position(|(k, _)| k == tool_use_id) else {
            return;
        };
        let (_, mut item) = self.pending.remove(pos);
        let Some(id) = id else {
            return; // the create failed — no id, no task
        };
        item.id = id.to_string();
        // A re-created id replaces the older item (shouldn't happen; be idempotent).
        self.list.items.retain(|t| t.id != item.id);
        self.list.items.push(item);
        self.list.sort();
    }

    /// Take the ops applied since the last call — the meta record's `task_ops` (#96).
    pub fn drain_recorded(&mut self) -> Vec<TaskOp> {
        std::mem::take(&mut self.recorded)
    }

    pub fn snapshot(&self) -> &TaskList {
        &self.list
    }
    pub fn is_empty(&self) -> bool {
        self.list.items.is_empty() && self.pending.is_empty()
    }
}

/// Merge the live on-disk state over the op-log reconstruction: disk wins per task id
/// (it is the queue's current truth and carries full detail); op-log items fill in for
/// pruned/gc'd files. The result keeps id order.
/// **This viewer MIRRORS the agent's task store; it is not the archive of record.**
///
/// Recorded because the tempting fix keeps presenting itself (owner decision, #155). Claude
/// Code DELETES a task's JSON when it completes — the queue directory holds only the open
/// ones — so a task completed in an earlier session has no subject in any local source: no
/// `Create` op in this transcript, no file on disk, nothing in the transcript text. It is
/// therefore easy to propose that the durable meta stream (#96) retain every subject it has
/// ever seen, so titles survive the agent pruning them.
///
/// Don't. That would make the viewer a second source of truth for data its subject
/// deliberately discards, and quietly commit it to a durability promise it never made. When a
/// title is gone, the honest render is [`TaskItem::subject`] left empty and the frontends
/// saying so (#125) — a visible gap, not an invented one.
pub fn merged(oplog: &TaskList, disk: Option<TaskList>) -> TaskList {
    let Some(disk) = disk else {
        return oplog.clone();
    };
    let mut out = disk;
    for t in &oplog.items {
        if !out.items.iter().any(|d| d.id == t.id) {
            out.items.push(t.clone());
        }
    }
    out.sort();
    out
}

/// Fill the gaps a `taskq` task's text left in the op-log, from the queue's own files
/// ([`queue_tasks`](crate::engine::taskq::queue_tasks)) — **enrich only**: this never adds a
/// task, never removes one, and never overwrites text the transcript already carried.
///
/// Why the transcript has gaps at all: a `taskq create`'s record carries no description, so
/// the fold reads it from the COMMAND — and a description built in a shell variable, a
/// heredoc or `$(cat …)` was consumed by the shell before taskq ran. There is nothing to
/// recover from the transcript in that case; the queue's file has it.
///
/// Deliberately NOT [`merged`]'s "disk wins, and disk may add". That contract fits a
/// SESSION-scoped store (`~/.claude/tasks/<session-id>/`), where every file belongs to the
/// session being viewed. A taskq queue is REPO-scoped and shared across sessions and agents,
/// so letting it add rows would show every session in a repo the whole team's queue, and
/// letting it win would let one queue's text overwrite another's on a bare numeric-id
/// collision.
///
/// The id is therefore not enough to match on, and the SUBJECT is the discriminator: a queue
/// item enriches an op-log item only when their subjects agree. Records truncate a subject to
/// ~120 chars with a trailing `…`, so a truncated op-log subject matches a disk subject it is
/// a prefix of.
///
/// An op-log item with an EMPTY subject is never enriched, on purpose. That is the #125 stub
/// — a task this transcript saw only a state op for — and it carries no evidence of WHICH
/// task it is beyond a bare id. Filling it from whatever holds that id in the repo's queue
/// would be inventing the identification, which is the same line [`merged`]'s doc draws.
pub fn enrich_from_queue(list: &mut TaskList, queue: &TaskList) {
    for item in &mut list.items {
        if item.subject.is_empty() || (!item.description.is_empty() && !item.active_form.is_empty())
        {
            continue;
        }
        let Some(disk) = queue
            .items
            .iter()
            .find(|d| d.id == item.id && same_subject(&item.subject, &d.subject))
        else {
            continue;
        };
        if item.description.is_empty() {
            item.description = disk.description.clone();
        }
        if item.active_form.is_empty() {
            item.active_form = disk.active_form.clone();
        }
    }
}

/// Whether an op-log subject and a disk subject name the same task, allowing for the record's
/// ~120-char truncation (`…` is one char, so `strip_suffix` is exact).
fn same_subject(oplog: &str, disk: &str) -> bool {
    oplog == disk
        || oplog
            .strip_suffix('…')
            .is_some_and(|head| !head.is_empty() && disk.starts_with(head))
}

/// Parse ONE on-disk task file's JSON (Claude's `<n>.json` schema: `{id, subject,
/// description, activeForm, status, blocks, blockedBy}`) into a [`TaskItem`]. Shared by
/// the Claude adapter's `load_tasks`; agent-neutral in shape (an agent with a different
/// store maps its own format onto [`TaskItem`] in its adapter).
pub fn task_from_json(v: &serde_json::Value) -> Option<TaskItem> {
    let s = |k: &str| {
        v.get(k)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let list = |k: &str| -> Vec<String> {
        v.get(k)
            .and_then(serde_json::Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    let id = s("id");
    if id.is_empty() {
        return None;
    }
    Some(TaskItem {
        id,
        subject: s("subject"),
        description: s("description"),
        active_form: s("activeForm"),
        status: TaskStatus::parse(&s("status")),
        blocks: list("blocks"),
        blocked_by: list("blockedBy"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create(id: &str, subject: &str) -> (TaskOp, String, String) {
        let tuid = format!("toolu_{id}");
        (
            TaskOp::Create {
                tool_use_id: tuid.clone(),
                subject: subject.into(),
                description: format!("{subject} — details"),
                active_form: String::new(),
                blocked_by: Vec::new(),
            },
            tuid,
            format!("Created task #{id}: {subject}"),
        )
    }

    /// The op-log round trip: create (id joined from the result text) → status
    /// updates → dependency edges; an update for a never-created id materializes a
    /// stub; the list stays in numeric id order.
    #[test]
    fn oplog_replays_create_update_and_stubs() {
        let mut f = TaskFold::default();
        let (op, tuid, result) = create("12", "fix the parser");
        f.apply(&op);
        assert!(f.snapshot().is_empty(), "no id yet — still a draft");
        f.on_tool_result(&tuid, &result);
        assert_eq!(f.snapshot().items.len(), 1);
        assert_eq!(f.snapshot().items[0].id, "12");
        assert_eq!(f.snapshot().items[0].status, TaskStatus::Pending);

        f.apply(&TaskOp::Update {
            task_id: "12".into(),
            status: Some("in_progress".into()),
            subject: None,
            description: None,
            active_form: Some("Fixing the parser".into()),
            add_blocks: vec![],
            add_blocked_by: vec!["9".into()],
        });
        let t = &f.snapshot().items[0];
        assert_eq!(t.status, TaskStatus::InProgress);
        assert_eq!(t.active_form, "Fixing the parser");
        assert_eq!(t.blocked_by, vec!["9"]);

        // An update for an unseen id → stub, sorted numerically before 12.
        f.apply(&TaskOp::Update {
            task_id: "9".into(),
            status: Some("completed".into()),
            subject: None,
            description: None,
            active_form: None,
            add_blocks: vec![],
            add_blocked_by: vec![],
        });
        let ids: Vec<&str> = f.snapshot().items.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["9", "12"], "numeric id order");
        assert_eq!(f.snapshot().items[0].status, TaskStatus::Completed);
    }

    /// Disk wins per id; op-log items survive for pruned files; order holds.
    /// The repo queue fills GAPS, and nothing else. A `taskq create` whose description was
    /// built in a shell variable leaves the op-log with a subject and no text; the queue's
    /// file has the text. Everything else about the row stays the transcript's.
    ///
    /// The subject is the discriminator, not the id: a taskq queue is REPO-scoped, so a bare
    /// numeric id collides across queues and across a session's own native tasks. Each guard
    /// below is a way that could go wrong.
    #[test]
    fn the_queue_fills_empty_text_and_never_more() {
        let item = |id: &str, subject: &str, desc: &str, active: &str| TaskItem {
            id: id.into(),
            subject: subject.into(),
            description: desc.into(),
            active_form: active.into(),
            ..Default::default()
        };
        let mut list = TaskList {
            items: vec![
                // The reported case: subject survived, description was eaten by the shell.
                item("1", "Land the seam move", "", ""),
                // Half a row: the description survived, the active form did not. Pins
                // FILL rather than OVERWRITE — the queue supplies the missing half and
                // leaves the half the transcript carried.
                item("2", "Wire codex", "from the command line", ""),
                // Same id, different task — another queue, or a native task. Not ours.
                item("3", "Something else entirely", "", ""),
                // A #125 stub: a state op with no create. No evidence of WHICH task —
                // and the queue below holds a subjectless row at that id, so "both empty"
                // must not read as "the same task".
                item("4", "", "", ""),
                // A subject the record truncated; disk has it in full.
                item(
                    "5",
                    "A subject long enough that the record cut it o…",
                    "",
                    "",
                ),
            ],
        };
        let queue = TaskList {
            items: vec![
                item("1", "Land the seam move", "the brief, in full", "Moving"),
                item("2", "Wire codex", "the queue's copy", "Queued form"),
                item(
                    "3",
                    "A completely different task",
                    "not this one's text",
                    "No",
                ),
                item("4", "", "not attributable", "No"),
                item(
                    "5",
                    "A subject long enough that the record cut it off here",
                    "matched through the truncation",
                    "",
                ),
                // Present in the queue, absent from the transcript: never added.
                item("9", "Someone else's task", "not this session's", ""),
            ],
        };
        enrich_from_queue(&mut list, &queue);
        let seen: Vec<(&str, &str, &str)> = list
            .items
            .iter()
            .map(|t| {
                (
                    t.id.as_str(),
                    t.description.as_str(),
                    t.active_form.as_str(),
                )
            })
            .collect();
        assert_eq!(
            seen,
            [
                ("1", "the brief, in full", "Moving"),
                ("2", "from the command line", "Queued form"),
                ("3", "", ""),
                ("4", "", ""),
                ("5", "matched through the truncation", ""),
            ],
            "gaps filled where the subject agrees; nothing overwritten, nothing added"
        );
        assert_eq!(
            list.items.len(),
            5,
            "the queue's own task #9 is not this session's"
        );
    }

    #[test]
    fn merged_prefers_disk_and_backfills_from_oplog() {
        let mut f = TaskFold::default();
        let (op, tuid, result) = create("3", "from the log");
        f.apply(&op);
        f.on_tool_result(&tuid, &result);
        let (op2, tuid2, result2) = create("7", "pruned from disk");
        f.apply(&op2);
        f.on_tool_result(&tuid2, &result2);

        let disk = TaskList {
            items: vec![TaskItem {
                id: "3".into(),
                subject: "from disk (richer)".into(),
                status: TaskStatus::Completed,
                ..TaskItem::default()
            }],
        };
        let m = merged(f.snapshot(), Some(disk));
        assert_eq!(m.items.len(), 2);
        assert_eq!(m.items[0].subject, "from disk (richer)", "disk wins for #3");
        assert_eq!(m.items[0].status, TaskStatus::Completed);
        assert_eq!(m.items[1].id, "7", "op-log fills the pruned file");
    }

    /// #125: an UPDATE for a task this transcript never created yields a stub whose
    /// subject stays EMPTY — the fold must never invent one, because the only place a
    /// title could come from is another queue whose integer ids mean different tasks.
    /// A real disk record for the SAME session still wins through `merged`.
    #[test]
    fn an_update_without_a_create_keeps_an_empty_subject() {
        let mut f = TaskFold::default();
        f.apply(&TaskOp::Update {
            task_id: "5".into(),
            status: Some("in_progress".into()),
            subject: None,
            description: None,
            active_form: None,
            add_blocks: vec![],
            add_blocked_by: vec![],
        });
        let list = f.snapshot();
        assert_eq!(list.items.len(), 1);
        assert_eq!(list.items[0].id, "5");
        assert_eq!(
            list.items[0].subject, "",
            "no title is honest; a borrowed one would be wrong"
        );
        assert_eq!(list.items[0].status, TaskStatus::InProgress, "status shows");

        // The session's OWN store is the one safe source, and it still fills the title.
        let disk = TaskList {
            items: vec![TaskItem {
                id: "5".into(),
                subject: "the real subject".into(),
                ..TaskItem::default()
            }],
        };
        let m = merged(f.snapshot(), Some(disk));
        assert_eq!(m.items[0].subject, "the real subject");
    }

    fn snap(todos: &[(&str, &str)]) -> TaskOp {
        TaskOp::Snapshot {
            todos: todos
                .iter()
                .map(|(d, s)| Todo {
                    text: d.to_string(),
                    status: s.to_string(),
                    ..Todo::default()
                })
                .collect(),
        }
    }

    /// #126: `TodoWrite` is a replace-all. The list must be able to SHRINK — the thing an
    /// append-only op-log cannot express, and the reason this is its own op.
    #[test]
    fn a_snapshot_replaces_the_whole_list() {
        let mut f = TaskFold::default();
        f.apply(&snap(&[
            ("读取配置文件内容", "in_progress"),
            ("读取模板文件内容", "pending"),
            ("对比分析并输出结论报告", "pending"),
        ]));
        let l = f.snapshot();
        assert_eq!(l.items.len(), 3);
        assert_eq!(l.items[0].id, "0", "synthetic index ids");
        assert_eq!(l.items[0].subject, "读取配置文件内容");
        assert_eq!(l.items[0].status, TaskStatus::InProgress);

        // A later snapshot with FEWER todos shrinks the list rather than leaving orphans.
        f.apply(&snap(&[("读取配置文件内容", "completed")]));
        let l = f.snapshot();
        assert_eq!(l.items.len(), 1, "the dropped todos are gone");
        assert_eq!(l.items[0].status, TaskStatus::Completed);
    }

    /// An unchanged snapshot is applied but NOT recorded: 268 identical rewrites must not
    /// each cost a line in the persisted meta stream (#96).
    #[test]
    fn an_unchanged_snapshot_is_not_recorded() {
        let mut f = TaskFold::default();
        f.apply(&snap(&[("a", "pending")]));
        assert_eq!(f.drain_recorded().len(), 1);

        f.apply(&snap(&[("a", "pending")]));
        f.apply(&snap(&[("a", "pending")]));
        assert!(
            f.drain_recorded().is_empty(),
            "no-op rewrites add nothing to the stream"
        );

        f.apply(&snap(&[("a", "completed")]));
        assert_eq!(f.drain_recorded().len(), 1, "a real change still records");
    }

    /// The guard: a snapshot must never wipe a list the op-log built. Measured as
    /// unreachable today (no Claude transcript mixes the two), so it degrades by IGNORING
    /// the snapshot — losing a todo view is recoverable, losing the task list is not.
    #[test]
    fn a_snapshot_never_clobbers_an_op_log_list() {
        let mut f = TaskFold::default();
        let (op, tuid, _) = create("12", "a real task");
        f.apply(&op);
        f.on_tool_result(&tuid, "Created task #12: a real task");
        assert_eq!(f.snapshot().items.len(), 1);

        f.apply(&snap(&[("a todo", "pending")]));
        let l = f.snapshot();
        assert_eq!(l.items.len(), 1, "the op-log list survives");
        assert_eq!(l.items[0].id, "12");
        assert_eq!(l.items[0].subject, "a real task");

        // …and a list built only by snapshots is still replaceable.
        let mut g = TaskFold::default();
        g.apply(&snap(&[("x", "pending")]));
        g.apply(&snap(&[("y", "pending"), ("z", "pending")]));
        assert_eq!(g.snapshot().items.len(), 2);
    }

    /// The file-schema parser maps Claude's task JSON onto the neutral shape.
    #[test]
    fn task_file_json_parses() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"id":"52","subject":"s","description":"d","activeForm":"doing s",
                "status":"in_progress","blocks":["57"],"blockedBy":["54"]}"#,
        )
        .unwrap();
        let t = task_from_json(&v).unwrap();
        assert_eq!(t.id, "52");
        assert_eq!(t.status, TaskStatus::InProgress);
        assert_eq!(t.blocks, vec!["57"]);
        assert_eq!(t.blocked_by, vec!["54"]);
    }
}
