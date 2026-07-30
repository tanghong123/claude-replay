//! The **agent-neutral task/todo model** (#15): the [`TaskItem`]/[`TaskList`] vocabulary,
//! the op-log fold (replaying `TaskCreate`/`TaskUpdate` calls from the transcript into
//! point-in-time state), and the live/op-log merge. Per-agent knowledge stays thin and
//! elsewhere: the Claude L1 emits `Message::TaskOp`s
//! (only the tokenizer sees tool inputs), and the Claude adapter's `load_tasks` hook reads
//! the on-disk `~/.claude/tasks/<session-id>/*.json` files — the same
//! `TranscriptAdapter`-seam pattern as `subagent_source`. Codex has neither and simply
//! never produces either source.

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
#[derive(Debug, Clone, PartialEq)]
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
    Update {
        task_id: String,
        status: Option<String>,
        subject: Option<String>,
        description: Option<String>,
        active_form: Option<String>,
        add_blocks: Vec<String>,
        add_blocked_by: Vec<String>,
    },
}

/// The op-log reducer: replays [`TaskOp`]s (and watches tool results for the create→id
/// join) into a point-in-time [`TaskList`]. Maintained by the accumulator as messages
/// fold — live sessions grow it; a finished transcript yields its final state.
#[derive(Debug, Clone, Default)]
pub struct TaskFold {
    list: TaskList,
    /// `TaskCreate` calls whose id hasn't arrived yet: `tool_use_id` → draft item.
    pending: Vec<(String, TaskItem)>,
}

impl TaskFold {
    pub fn apply(&mut self, op: &TaskOp) {
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
        }
    }

    /// Feed every tool result through here: a pending `TaskCreate`'s result text
    /// ("Created task #12: …") assigns the draft its id and lands it in the list.
    pub fn on_tool_result(&mut self, tool_use_id: &str, text: &str) {
        let Some(pos) = self.pending.iter().position(|(id, _)| id == tool_use_id) else {
            return;
        };
        let (_, mut item) = self.pending.remove(pos);
        let id = text
            .split('#')
            .nth(1)
            .map(|r| {
                r.chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect::<String>()
            })
            .unwrap_or_default();
        if id.is_empty() {
            return; // the create failed — no id, no task
        }
        item.id = id;
        // A re-created id replaces the older item (shouldn't happen; be idempotent).
        self.list.items.retain(|t| t.id != item.id);
        self.list.items.push(item);
        self.list.sort();
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
