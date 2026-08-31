//! **`taskq` transcript decoding** — turning a shared work queue's CLI traffic into the
//! engine's own [`TaskOp`] vocabulary, for any agent whose transcript records a shell call
//! and its output.
//!
//! `taskq` (the `agentdev:taskq` skill) is a repo-rooted, cross-agent work queue —
//! `tasks/*.json` at the git root — that exists because the native `TaskCreate`/`TaskUpdate`
//! tools are harness-private and absent from some builds. A session that manages its work
//! through it therefore renders an EMPTY task panel while doing everything in a queue the
//! transcript records in full. Measured on the session that motivated this: 47 creates,
//! 36 claims, 45 dones, 14 logs, and a panel showing nothing.
//!
//! This module lives in the ENGINE, not in an agent family, because nothing in it is any
//! agent's: it reads `taskq`'s own contract — a `##taskq/v1 {json}` audit line printed last
//! on stdout, and the `--subject`/`--description`/`--active-form` flags on the command that
//! printed it. An adapter supplies two things and gets the ops: a stable per-call id, and
//! the two halves of one shell call.
//!
//! ```text
//! // at the tool CALL, where the command line is visible:
//! for op in taskq_create_ops(call_id, command) { … }
//! // at the tool RESULT, where the records are:
//! for op in taskq_ops(call_id, stdout) { … }
//! ```
//!
//! Both halves are needed because they carry different things — the record has the id, and
//! only the command has the DESCRIPTION. An adapter that wires only the result half still
//! gets correctly-titled tasks; it just loses their briefs.
//!
//! Wired for Claude Code (and so QoderWork, which delegates to that family) and for Codex.

use crate::engine::tasks::TaskOp;
use serde_json::Value;

/// The `taskq` audit-record sentinel (agentdev `docs/taskq-DESIGN.md` §9). A mutating
/// `taskq` command prints one such line per mutation as the LAST line of its stdout, and the
/// harness captures that into the transcript — so the queue's history is already in every
/// transcript, in a form designed to be extracted. The sentinel carries no quotes,
/// backslashes or non-ASCII precisely so it survives JSON string escaping verbatim.
pub const TASKQ_SENTINEL: &str = "##taskq/v1";

/// Split a shell command into words, honouring the three quoting forms a `taskq` invocation
/// actually uses: double quotes, single quotes, and backslash-escapes (including the
/// line-continuation `\<newline>` these multi-line calls are written with).
///
/// Deliberately not a shell: no expansion, no substitution, no operator parsing. It exists to
/// recover the literal text of a `--description` argument, and anything it cannot read
/// literally it leaves alone — a wrong description is worse than none.
fn shell_words(cmd: &str) -> Vec<String> {
    let (mut out, mut cur, mut has) = (Vec::new(), String::new(), false);
    let mut chars = cmd.chars().peekable();
    let (mut dq, mut sq) = (false, false);
    while let Some(c) = chars.next() {
        match c {
            '\\' if !sq => match chars.next() {
                Some('\n') => {} // line continuation: joins, adds nothing
                Some(n) => {
                    cur.push(n);
                    has = true;
                }
                None => {}
            },
            '"' if !sq => {
                dq = !dq;
                has = true;
            }
            '\'' if !dq => {
                sq = !sq;
                has = true;
            }
            c if c.is_whitespace() && !dq && !sq => {
                if has {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            c => {
                cur.push(c);
                has = true;
            }
        }
    }
    if has {
        out.push(cur);
    }
    out
}

/// One `taskq` subcommand recovered from a Bash command line: its verb, the task id when the
/// verb takes one, and the `--subject`/`--description`/`--active-form` values it was given.
#[derive(Default)]
struct TaskqCall {
    verb: String,
    task: String,
    subject: String,
    description: String,
    active_form: String,
}

/// The `taskq` calls in one Bash command, in order — subject, description, active form.
///
/// The RECORDS a create prints carry no description (measured: their `changes` hold only
/// `status` and `blockedBy`), because the record is an audit line, not a store. The text the
/// panel wants is in the COMMAND, which is why this reads both halves. Pairing is by ORDINAL
/// within the one Bash call: the k-th `create` in the command produced the k-th create record
/// in its output. Verified across the motivating session — 31 batched calls, every one
/// matching in count.
///
/// The `create` must be TASKQ's: it is counted only when the immediately preceding word is the
/// taskq program itself. A shell block routinely mixes tools — the motivating session ran
/// `a1 repo create`, `gh issue create` and `gh pr create` in blocks that also mention taskq —
/// and a whole-command "does it say taskq" guard admitted all three, inventing three tasks and
/// leaving one stuck in-progress by shifting every later ordinal. The program word is matched
/// by BASENAME, so `taskq`, `$TQ`, `"$TQ"` and an absolute path to the script all count.
fn taskq_calls(cmd: &str) -> Vec<TaskqCall> {
    let words = shell_words(cmd);
    let is_taskq = |w: &String| {
        let base = w.rsplit('/').next().unwrap_or(w);
        base == "taskq" || base == "$TQ" || base == "${TQ}"
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < words.len() {
        // The verb must be TASKQ's — the word before it is the program itself.
        if i == 0 || !is_taskq(&words[i - 1]) {
            i += 1;
            continue;
        }
        let verb = words[i].clone();
        if !matches!(verb.as_str(), "create" | "update") {
            i += 1;
            continue;
        }
        let mut call = TaskqCall {
            verb,
            ..Default::default()
        };
        i += 1;
        // `update` takes the id positionally, before its flags.
        if call.verb == "update" && i < words.len() && !words[i].starts_with("--") {
            call.task = words[i].clone();
            i += 1;
        }
        while i + 1 < words.len() && words[i].starts_with("--") {
            let value = words[i + 1].clone();
            match words[i].as_str() {
                "--subject" => call.subject = value,
                "--description" => call.description = value,
                "--active-form" => call.active_form = value,
                _ => {}
            }
            i += 2;
        }
        out.push(call);
    }
    out
}

/// The `taskq create` calls in a shell command line, as [`TaskOp::Create`]s awaiting their ids.
///
/// Mirrors the native flow exactly: the create is emitted from the CALL (only L1 sees a tool's
/// input), and the id arrives in the result. The synthetic `tool_use_id` is the shell call's own
/// id plus the create's ordinal, which is what [`taskq_ops`] resolves against — so `call_id`
/// must be the same value the adapter later hands [`taskq_ops`] for that call's output.
///
/// Takes the command LINE, not a tool input: adapters disagree about where it lives
/// (Claude's `input.command` is a string, Codex's is `input.cmd` or an argv array whose
/// `-lc` payload is the script), and unwrapping that is the adapter's job, not this one's.
pub fn taskq_create_ops(call_id: &str, cmd: &str) -> Vec<TaskOp> {
    let mut out = Vec::new();
    let mut nth_create = 0usize;
    for call in taskq_calls(cmd) {
        match call.verb.as_str() {
            "create" => {
                out.push(TaskOp::Create {
                    tool_use_id: format!("taskq:{call_id}#{nth_create}"),
                    subject: call.subject,
                    description: call.description,
                    active_form: call.active_form,
                    blocked_by: Vec::new(),
                });
                nth_create += 1;
            }
            // An `update --description` carries the FULL new text; the record it prints
            // truncates it to ~120 chars with an ellipsis (design §9 — it is an audit line,
            // not a store), so the command is the only place the real text exists. Applied
            // here rather than from the record for exactly that reason.
            "update" if !call.task.is_empty() => {
                let field = |s: String| (!s.is_empty()).then_some(s);
                let (description, subject, active_form) = (
                    field(call.description),
                    field(call.subject),
                    field(call.active_form),
                );
                if description.is_none() && subject.is_none() && active_form.is_none() {
                    continue; // a status-only update; the RECORD states that transition
                }
                out.push(TaskOp::Update {
                    task_id: call.task,
                    status: None, // the record owns status; the command owns the text
                    subject,
                    description,
                    active_form,
                    add_blocks: Vec::new(),
                    add_blocked_by: Vec::new(),
                });
            }
            _ => {}
        }
    }
    out
}

/// Task ops carried by a shell tool's RESULT, from `taskq` records.
///
/// The queue this reads is not the harness's. `taskq` is a repo-rooted, cross-agent work queue
/// (`tasks/*.json` at the git root) that exists because the native `TaskCreate`/`TaskUpdate`
/// tools are harness-private and absent from some builds — the sessions using it therefore
/// render an EMPTY task panel while doing all their work through a queue the transcript
/// records in full. Measured on the session that motivated this: 47 creates, 36 claims, 45
/// dones, 14 logs, and a panel showing nothing.
///
/// Every record becomes ONE op, and the mapping is deliberately onto the vocabulary the fold
/// already has rather than a new one — the ops are the same ops:
///
/// * `create` → a [`TaskOp::Resolve`] for the draft the COMMAND side emitted
///   ([`taskq_create_ops`]), paired by ordinal within the same Bash call. The two halves are
///   read because they carry different things: the record has the id, and only the command has
///   the DESCRIPTION — a record's `changes` hold `status` and `blockedBy` and nothing else
///   (measured across every create in the motivating session), the record being an audit line
///   rather than a store.
/// * `claim`/`done`/`cancel`/`release`/`update` → [`TaskOp::Update`], with the status taken
///   from `changes.status.to` — the record states the transition rather than implying it.
/// * `log` → a no-op here: a progress note changes no field the panel shows, and mapping it to
///   an `Update` would be recording a change that did not happen.
/// * `archive`/`delete`/`renumber` → left alone for now; they mean "leave the list", which the
///   append-only fold cannot express (the same reason `Snapshot` exists for `TodoWrite`).
///
/// Re-seeing a record is HARMLESS and needs no dedupe state, which is what keeps this a
/// per-line decode like every other: `taskq list --with-history` echoes other agents' records
/// into this transcript and the journal's tail can be printed, so the same mutation
/// legitimately appears more than once — but a repeated `Update` sets the same status twice,
/// and an echoed `create` record resolves an ordinal its Bash call never created, which lands
/// nothing at all (`join` returns on an unknown `tool_use_id`).
pub fn taskq_ops(call_id: &str, text: &str) -> Vec<TaskOp> {
    let mut out = Vec::new();
    let mut nth_create = 0usize;
    for line in text.lines() {
        let Some(rest) = line.trim_start().strip_prefix(TASKQ_SENTINEL) else {
            continue;
        };
        let Ok(rec) = serde_json::from_str::<Value>(rest.trim()) else {
            continue; // a mangled or truncated line is skipped, never guessed at
        };
        let str_of = |k: &str| rec.get(k).and_then(|v| v.as_str()).unwrap_or_default();
        // The record id namespaces the synthetic `tool_use_id` below, so a create always
        // pairs with its OWN resolve. A record without one is not trusted.
        let rid = str_of("rid");
        if rid.is_empty() {
            continue;
        }
        let task = str_of("task");
        if task.is_empty() {
            continue; // nothing to address
        }
        // `changes.<field>.to` — where a record states the value a mutation moved a field TO.
        let to = |field: &str| {
            rec.pointer(&format!("/changes/{field}/to"))
                .and_then(|v| v.as_str())
                .map(String::from)
        };
        match str_of("op") {
            "create" => {
                // Resolve the create the COMMAND emitted, by ordinal within this Bash call
                // (`taskq_create_ops`) — that draft carries the description, which no record
                // does. A create the command side did not produce (an echoed record, a
                // `--with-history` tail) resolves nothing and is simply not in the list; its
                // task still appears the moment any state op touches it, titled from the
                // record.
                out.push(TaskOp::Resolve {
                    tool_use_id: format!("taskq:{call_id}#{nth_create}"),
                    id: Some(task.to_string()),
                });
                nth_create += 1;
            }
            // Every other state-moving op says where it moved to, so one arm reads them all.
            "claim" | "done" | "cancel" | "release" | "update" => {
                let status = to("status");
                if status.is_none() {
                    continue; // e.g. a description-only edit — nothing the panel renders
                }
                // The subject comes from the record's TOP-LEVEL field, which every record
                // carries, not from `changes.subject.to`, which appears only when the subject
                // itself was edited. That distinction is load-bearing: an update for a task
                // this transcript never saw created materializes a stub (#125), and for the
                // native tools that stub is necessarily titleless — "Updated task #5 status"
                // says nothing more. A taskq record DOES name its task, so passing the subject
                // on every state op is what keeps a resumed or mid-session view from rendering
                // a row with no content.
                let subject = Some(str_of("subject").to_string()).filter(|s| !s.is_empty());
                out.push(TaskOp::Update {
                    task_id: task.to_string(),
                    status,
                    subject,
                    description: None,
                    active_form: None,
                    add_blocks: Vec::new(),
                    add_blocked_by: Vec::new(),
                });
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `taskq` records ride a Bash result's stdout and become the same task ops the native
    /// tools produce — the queue exists precisely because those tools are absent from some
    /// builds, so a session using it rendered an empty panel while doing all its work there.
    ///
    /// Pinned here: a create lands under the id the RECORD carries (no result to wait for),
    /// state ops read their status from `changes.status.to`, a `log` note changes nothing,
    /// and a mangled or rid-less line is skipped rather than guessed at.
    #[test]
    fn taskq_records_in_a_bash_result_become_task_ops() {
        let rec = |rid: &str, op: &str, task: &str, subject: &str, changes: &str| {
            format!(
                "##taskq/v1 {{\"rid\":\"{rid}\",\"ts\":\"2026-08-29T23:20:35Z\",\"repo\":\"mdviewer\",\
                 \"op\":\"{op}\",\"task\":\"{task}\",\"subject\":\"{subject}\",\
                 \"by\":\"claude-code/hong@mac\",\"changes\":{changes}}}"
            )
        };
        let text = [
            "some ordinary command output first",
            &rec(
                "r1",
                "create",
                "1",
                "Scaffold the monorepo",
                r#"{"status":{"from":null,"to":"pending"}}"#,
            ),
            &rec(
                "r2",
                "create",
                "2",
                "Engine: inline diffs",
                r#"{"status":{"from":null,"to":"pending"}}"#,
            ),
            &rec(
                "r3",
                "claim",
                "1",
                "Scaffold the monorepo",
                r#"{"status":{"from":"pending","to":"in_progress"}}"#,
            ),
            &rec(
                "r4",
                "done",
                "1",
                "Scaffold the monorepo",
                r#"{"status":{"from":"in_progress","to":"completed"}}"#,
            ),
            // A progress note moves nothing the panel shows.
            &rec(
                "r5",
                "log",
                "2",
                "Engine: inline diffs",
                r#"{"log":{"to":"a note"}}"#,
            ),
            // Not trusted: no rid to namespace the create's join, and unparsable JSON.
            "##taskq/v1 {\"op\":\"create\",\"task\":\"9\",\"subject\":\"no rid\"}",
            "##taskq/v1 {this is not json",
        ]
        .join("\n");

        // The RESULT half yields only resolves — the drafts they land come from the command.
        let ops = taskq_ops("b1", &text);
        let expect = |op: &TaskOp| -> String {
            match op {
                TaskOp::Create {
                    tool_use_id,
                    subject,
                    description,
                    ..
                } => format!("create({tool_use_id}, {subject}, desc={description})"),
                TaskOp::Resolve { tool_use_id, id } => {
                    format!(
                        "resolve({tool_use_id} -> {})",
                        id.clone().unwrap_or_default()
                    )
                }
                TaskOp::Update {
                    task_id,
                    status,
                    subject,
                    description,
                    ..
                } => format!(
                    "update({task_id}, {}, {}, desc={})",
                    status.clone().unwrap_or_default(),
                    subject.clone().unwrap_or_default(),
                    description.clone().unwrap_or_default()
                ),
                _ => "other".into(),
            }
        };
        assert_eq!(
            ops.iter().map(expect).collect::<Vec<_>>(),
            vec![
                // Ordinal 0 and 1 within this Bash call — the ids the records announce.
                "resolve(taskq:b1#0 -> 1)".to_string(),
                "resolve(taskq:b1#1 -> 2)".to_string(),
                // State ops read their destination from `changes.status.to`, and carry the
                // subject from the record's TOP-LEVEL field so a stub is never blank.
                "update(1, in_progress, Scaffold the monorepo, desc=)".to_string(),
                "update(1, completed, Scaffold the monorepo, desc=)".to_string(),
            ],
            "the log note, the rid-less record and the mangled line contribute nothing"
        );

        // The COMMAND half supplies what no record carries: the description, in full. The
        // record truncates to ~120 chars by design, so the command is the only source.
        let cmd = "TQ=/x/taskq\n\"$TQ\" create --subject \"Scaffold the monorepo\" \
                   --active-form \"Scaffolding\" --description \"npm workspaces at root\"\n\
                   \"$TQ\" create --subject \"Engine: inline diffs\" --description \"the engine\"\n\
                   gh issue create --title \"not a task\"\n\
                   \"$TQ\" update 1 --description \"a longer, edited brief\"";
        assert_eq!(
            taskq_create_ops("b1", cmd)
                .iter()
                .map(expect)
                .collect::<Vec<_>>(),
            vec![
                "create(taskq:b1#0, Scaffold the monorepo, desc=npm workspaces at root)"
                    .to_string(),
                "create(taskq:b1#1, Engine: inline diffs, desc=the engine)".to_string(),
                // An `update --description` carries the FULL text; the record's copy is
                // truncated, so this is where an edited brief comes from.
                "update(1, , , desc=a longer, edited brief)".to_string(),
            ],
            "`gh issue create` is another tool's verb and must not become a task"
        );

        // Re-seeing a result is harmless: an echoed create record resolves an ordinal this
        // call never created, which lands nothing (`join` returns on an unknown id).
        let again: Vec<String> = taskq_ops("b1", &text).iter().map(expect).collect();
        assert_eq!(again.len(), 4, "records re-read identically");
    }

    /// The reported symptom, reproduced at the op level: a view that starts PART-WAY through a
    /// session (a durable resume, or a client that joined late) sees a `done` whose `create`
    /// lies in bytes it will never read. The fold materializes a stub for it (#125) — that is
    /// correct and deliberate — and the stub must still carry a TITLE, because unlike the
    /// native tools' "Updated task #5 status", a taskq record names its task on every op.
    ///
    /// Without the top-level subject this rendered exactly what the session showed: one task,
    /// no content.
    #[test]
    fn a_state_op_alone_still_names_its_task() {
        let done = "##taskq/v1 {\"rid\":\"r9\",\"ts\":\"2026-08-29T23:59:00Z\",\"op\":\"done\",\
                    \"task\":\"47\",\"subject\":\"Daemon: one server, many documents\",\
                    \"changes\":{\"status\":{\"from\":\"in_progress\",\"to\":\"completed\"},\
                    \"outcome\":{\"from\":null,\"to\":\"shipped\"}}}";
        let ops = taskq_ops("b9", done);
        assert_eq!(ops.len(), 1, "{ops:#?}");
        let TaskOp::Update {
            task_id,
            status,
            subject,
            ..
        } = &ops[0]
        else {
            panic!("{:#?}", ops[0])
        };
        assert_eq!(task_id, "47");
        assert_eq!(status.as_deref(), Some("completed"));
        assert_eq!(
            subject.as_deref(),
            Some("Daemon: one server, many documents"),
            "a stub built from this op is not blank"
        );
    }
}
