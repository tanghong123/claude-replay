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

use crate::engine::tasks::{TaskItem, TaskList, TaskOp};
use serde_json::Value;
use std::path::{Path, PathBuf};

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
fn shell_words(cmd: &str) -> (Vec<String>, bool) {
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
    (out, !dq && !sq)
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
    let (words, balanced) = shell_words(cmd);
    // Unclosed quoting means the tokenization is WRONG, not merely incomplete, and a wrong
    // draft is worse than none: it lands under another task's id and takes that task's row
    // with it. Measured on the migration that filed this repo's queue — a heredoc body
    // containing "It's" flipped the single-quote state, hiding the second `create` boundary,
    // so one draft was recovered where two were written; it paired with the FIRST record and
    // task #10 never appeared at all. Returning nothing costs a description, which the
    // record's own stub and `queue_tasks` both cover.
    if !balanced {
        return Vec::new();
    }
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
            let value = if unexpanded(&words[i + 1]) {
                String::new()
            } else {
                words[i + 1].clone()
            };
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

/// Whether a recovered value is a bare shell EXPANSION rather than text — `$D`, `${DESC}`,
/// `$1`, `$(cat …)`. [`shell_words`] is deliberately not a shell, so it hands back the
/// reference verbatim; recording that puts the literal string `$D` in the panel where the
/// description belongs (observed on two unrelated sessions, 2026-08-30, both from agents
/// that built the text in a variable and passed `--description "$D"`).
///
/// Empty is the honest answer: the panel says "no recorded description", which is true,
/// instead of showing a variable name that means nothing to a reader.
///
/// Only a WHOLE-value reference counts. A description that merely CONTAINS a `$` is left
/// alone — "costs $5" is prose someone single-quoted, and dropping it would lose real text
/// to catch a case that is already ambiguous.
fn unexpanded(value: &str) -> bool {
    let Some(rest) = value.strip_prefix('$') else {
        return false;
    };
    // `$(…)` — command substitution, whose output no transcript records.
    if rest.starts_with('(') {
        return value.ends_with(')');
    }
    let name = rest
        .strip_prefix('{')
        .and_then(|n| n.strip_suffix('}'))
        .unwrap_or(rest);
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A `taskq` task's id as the PANEL must key it — `q` before a repo-tier number.
///
/// Two independent queues share the panel and both number from 1: the harness's own
/// `TaskCreate`/`TaskUpdate` tools, and taskq. Keyed by the bare number they collide, and
/// whichever resolved last owns the row outright. Measured on the session that reported this
/// (115 native creates beside a 34-task taskq queue): 7 of taskq's 34 survived and 27 were
/// wearing a native task's subject.
///
/// The prefix is taskq's own convention, not an invention: a user-tier task is `u12` on every
/// surface taskq has, "so a record is self-describing without its command line" (design §9,
/// rev 4). This extends that to the repo tier, which taskq leaves bare because within taskq
/// there is nothing to disambiguate from. So the panel reads `#27` native, `#q27` taskq repo,
/// `#u12` taskq user.
///
/// Already-prefixed ids pass through: a user-tier record arrives as `u12` and stays that way.
fn queue_id(task: &str) -> String {
    match task.as_bytes().first() {
        Some(b) if b.is_ascii_digit() => format!("q{task}"),
        _ => task.to_string(),
    }
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
                    task_id: queue_id(&call.task),
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
        // The sentinel must START the line, with no leading whitespace. taskq PRINTS its
        // record (`print(SENTINEL + " " + json)`), so a real one always begins a line;
        // anything indented is a QUOTATION — a doc's code block, a diff's context line, a
        // pretty-printed log — and replaying a quotation as a mutation invents tasks.
        //
        // Measured across every local transcript carrying the sentinel (2026-08-31): 786
        // occurrences at column 0 and 19 indented, and of the 377 distinct record ids seen,
        // exactly ONE appears only indented — `m2k9x1-ab3f`, the sample record printed in
        // `taskq-DESIGN.md` §9. Two unrelated sessions that had merely READ that document
        // were showing its example ("Activity pane: humanize ETA") as a real task. Every
        // indented occurrence of a genuine id also appears at column 0, so nothing real is
        // lost. An earlier `trim_start()` here was leniency with no evidence behind it.
        let Some(rest) = line.strip_prefix(TASKQ_SENTINEL) else {
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
                // rev 5 publishes the description (and active form) a create was given,
                // bounded at 8 KB rather than the 120 chars everything else gets. That is
                // the ONLY copy for text the shell ate before the transcript saw it — a
                // `--description "$D"`, a heredoc, a `$(cat …)` — and, since rev 5 also made
                // repo queues machine-local, the only copy at all when the transcript is read
                // anywhere but the machine that owns the checkout.
                //
                // The COMMAND still wins when it produced text: it is unbounded and exact,
                // while this is bounded. The record is the floor, not the authority.
                // The record ALONE is enough to put the task on the list, correctly titled:
                // it names its task, unlike a native "Created task #12". Emitted first so a
                // draft that does exist replaces this stub wholesale (`join` retains the id
                // out and pushes its own item), and so a create whose draft was lost — an
                // echoed record, a `--with-history` tail, a command this decoder would not
                // tokenize — still appears with its subject instead of vanishing.
                out.push(TaskOp::Update {
                    task_id: queue_id(task),
                    status: to("status"),
                    subject: Some(str_of("subject").to_string()).filter(|s| !s.is_empty()),
                    description: to("description"),
                    active_form: to("activeForm"),
                    add_blocks: Vec::new(),
                    add_blocked_by: Vec::new(),
                });
                // Then resolve the draft the COMMAND emitted, by ordinal within this call
                // (`taskq_create_ops`) — that draft carries the description, which no record
                // does.
                out.push(TaskOp::Resolve {
                    tool_use_id: format!("taskq:{call_id}#{nth_create}"),
                    id: Some(queue_id(task)),
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
                    task_id: queue_id(task),
                    status,
                    subject,
                    // rev 5: an edit publishes its new text at 8 KB. Before it, the record
                    // truncated to 120 and the command was the only full copy.
                    description: to("description"),
                    active_form: to("activeForm"),
                    add_blocks: Vec::new(),
                    add_blocked_by: Vec::new(),
                });
            }
            _ => {}
        }
    }
    out
}

/// The `taskq` QUEUE on disk, for the repository `anchor` sits in — `tasks/*.json` at the git
/// root, plus `tasks/archive/*.json` for tasks that have been archived out of the active set
/// (taskq's own reader does the same, with an active file shadowing an archived one of the
/// same id).
///
/// Discovery-side on purpose: this reads files beside the transcript and is never part of a
/// fold, the same rule `session_tasks`/`session_runs` follow. The fold stays pure over the
/// transcript; what a queue currently says is a fact about now, read where the answer is
/// served.
///
/// The file schema is the one [`task_from_json`](crate::engine::tasks::task_from_json)
/// already parses — `{id, subject, description, activeForm, status, blockedBy}` — because
/// taskq writes the same shape Claude's own task files use.
///
/// `None` when there is no repository above `anchor`, no `tasks/` in it, or nothing parseable
/// there. Never shells out to `git`: it looks for a `.git` ENTRY, which is a directory in a
/// normal clone and a FILE in a worktree or submodule.
pub fn queue_tasks(anchor: &Path) -> Option<TaskList> {
    let root = git_root(anchor)?;
    let dir = root.join("tasks");
    let mut items: Vec<TaskItem> = Vec::new();
    // Archive first, so an active file of the same id overwrites it below.
    for (d, archived) in [(dir.join("archive"), true), (dir.clone(), false)] {
        let Ok(entries) = std::fs::read_dir(&d) else {
            if !archived {
                return None; // no `tasks/` at all — not a queue
            }
            continue;
        };
        for e in entries.flatten() {
            // taskq's own `TASKFILE_RE`: `<n>.json` and nothing else, so `journal.ndjson`
            // and any stray file in the directory are skipped rather than guessed at.
            let name = e.file_name();
            let Some(stem) = name.to_str().and_then(|n| n.strip_suffix(".json")) else {
                continue;
            };
            if stem.is_empty() || !stem.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let Some(mut t) = std::fs::read_to_string(e.path())
                .ok()
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .as_ref()
                .and_then(crate::engine::tasks::task_from_json)
            else {
                continue;
            };
            // These files ARE the repo tier, so they key the same way the records do.
            t.id = queue_id(&t.id);
            match items.iter_mut().find(|x| x.id == t.id) {
                Some(slot) => *slot = t,
                None => items.push(t),
            }
        }
    }
    (!items.is_empty()).then_some(TaskList { items })
}

/// The repository root at or above `dir` — the first ancestor holding a `.git` entry.
///
/// Bounded rather than unbounded: a pathological path cannot turn one lookup into an
/// unbounded walk, and no real repository is 64 levels deep. Stops at the filesystem root,
/// NOT at `$HOME` — a checkout under `/opt` or `/srv` is an ordinary case.
///
/// Honours `GIT_CEILING_DIRECTORIES` the way git's own discovery does: the walk stops when it
/// reaches a listed directory, without examining it or anything above. That is not decoration
/// — this workspace points `TMPDIR` at its own `target/` and sets a ceiling there precisely so
/// a fixture under it is outside a repository, exactly as it was in the system temp. Without
/// this, every scratch directory a test builds would sit inside claude-replay and inherit
/// claude-replay's queue.
fn git_root(dir: &Path) -> Option<PathBuf> {
    let ceilings: Vec<PathBuf> = std::env::var("GIT_CEILING_DIRECTORIES")
        .unwrap_or_default()
        .split(':')
        .filter(|p| !p.is_empty() && Path::new(p).is_absolute())
        .map(PathBuf::from)
        .collect();
    dir.ancestors()
        .take(64)
        .take_while(|d| !ceilings.iter().any(|c| c == *d))
        .find(|d| d.join(".git").exists())
        .map(Path::to_path_buf)
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
                // Each create record first stands the task up on its own — titled, so a
                // create whose draft never arrived is a row rather than a hole — then
                // resolves the draft at ordinal 0 and 1 within this Bash call.
                "update(q1, pending, Scaffold the monorepo, desc=)".to_string(),
                "resolve(taskq:b1#0 -> q1)".to_string(),
                "update(q2, pending, Engine: inline diffs, desc=)".to_string(),
                "resolve(taskq:b1#1 -> q2)".to_string(),
                // State ops read their destination from `changes.status.to`, and carry the
                // subject from the record's TOP-LEVEL field so a stub is never blank.
                "update(q1, in_progress, Scaffold the monorepo, desc=)".to_string(),
                "update(q1, completed, Scaffold the monorepo, desc=)".to_string(),
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
                "update(q1, , , desc=a longer, edited brief)".to_string(),
            ],
            "`gh issue create` is another tool's verb and must not become a task"
        );

        // Re-seeing a result is harmless: an echoed create record resolves an ordinal this
        // call never created, which lands nothing (`join` returns on an unknown id).
        let again: Vec<String> = taskq_ops("b1", &text).iter().map(expect).collect();
        assert_eq!(again.len(), 6, "records re-read identically");
    }

    /// The queue read: `tasks/<n>.json` at the git root, ARCHIVE included, everything else in
    /// the directory ignored.
    ///
    /// Archive matters more than it looks: taskq moves finished tasks there, and a finished
    /// task with no recoverable description is exactly the population this exists to fill.
    /// An active file shadows an archived one of the same id, matching taskq's own reader.
    #[test]
    fn the_queue_is_read_from_the_git_root_including_archive() {
        let base = std::env::temp_dir().join(format!("tq-queue-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("repo");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("tasks/archive")).unwrap();
        let write = |p: std::path::PathBuf, id: &str, subject: &str, desc: &str| {
            std::fs::write(
                p,
                serde_json::json!({
                    "id": id, "subject": subject, "description": desc,
                    "activeForm": "Doing it", "status": "pending", "blockedBy": []
                })
                .to_string(),
            )
            .unwrap()
        };
        write(
            root.join("tasks/1.json"),
            "1",
            "active one",
            "from the active file",
        );
        write(
            root.join("tasks/archive/2.json"),
            "2",
            "archived one",
            "from the archive",
        );
        // Same id in both: the ACTIVE file is the answer.
        write(
            root.join("tasks/archive/1.json"),
            "1",
            "stale",
            "stale copy",
        );
        // Neither of these is a task file.
        std::fs::write(root.join("tasks/journal.ndjson"), "##taskq/v1 {}\n").unwrap();
        std::fs::write(root.join("tasks/notes.json"), "{\"id\":\"99\"}").unwrap();

        // Found from a SUBDIRECTORY — the walk goes up to the repository root.
        let deep = root.join("crates/engine/src");
        std::fs::create_dir_all(&deep).unwrap();
        let q = queue_tasks(&deep).expect("a queue above this directory");
        let mut rows: Vec<(String, String)> = q
            .items
            .iter()
            .map(|t| (t.id.clone(), t.description.clone()))
            .collect();
        rows.sort();
        assert_eq!(
            rows,
            [
                ("q1".to_string(), "from the active file".to_string()),
                ("q2".to_string(), "from the archive".to_string()),
            ],
            "`notes.json` is not `<n>.json`, and the journal is not a task"
        );

        // A directory with no repository above it, and a repository with no queue.
        assert!(queue_tasks(&base).is_none(), "no .git above `base`");
        let bare = base.join("bare");
        std::fs::create_dir_all(bare.join(".git")).unwrap();
        assert!(
            queue_tasks(&bare).is_none(),
            "a repo with no tasks/ has no queue"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A description the caller built in a shell VARIABLE is not recoverable, and must not be
    /// recorded as the variable's NAME. Reported twice on 2026-08-30 from unrelated sessions,
    /// both of which had run `--description "$D"` after a heredoc: every task in the panel
    /// showed its description as the literal `$D`.
    ///
    /// Empty is the honest answer — "no recorded description" is true, `$D` is noise. What
    /// this must NOT do is get greedy: prose that merely mentions a dollar sign is text
    /// someone quoted, and dropping it would lose real content.
    #[test]
    fn a_description_left_as_a_shell_variable_is_dropped_not_recorded() {
        let desc = |cmd: &str| {
            taskq_create_ops("b1", cmd)
                .into_iter()
                .find_map(|op| match op {
                    TaskOp::Create { description, .. } => Some(description),
                    _ => None,
                })
                .expect("a create")
        };
        for form in [
            "\"$D\"",
            "$D",
            "\"${DESCRIPTION}\"",
            "\"$1\"",
            "\"$(cat brief.txt)\"",
        ] {
            assert_eq!(
                desc(&format!("taskq create --subject S --description {form}")),
                "",
                "{form} is a reference, not text"
            );
        }
        // …and the same guard leaves real prose alone, dollar sign and all — including
        // prose that OPENS with one, which is the case a looser check would eat.
        for prose in [
            "'costs $5 a month, $ANNUAL yearly'",
            "'$5 a month, billed yearly'",
            "'${} is not a name'",
        ] {
            let text = prose.trim_matches('\'');
            assert_eq!(
                desc(&format!("taskq create --subject S --description {prose}")),
                text,
                "a description that merely contains a $ is still a description"
            );
        }
        // The subject travels the same path, so it gets the same treatment.
        let ops = taskq_create_ops("b1", "taskq create --subject \"$S\" --description real");
        assert!(matches!(
            &ops[0],
            TaskOp::Create { subject, description, .. }
                if subject.is_empty() && description == "real"
        ));
    }

    /// **Two queues, one panel: taskq ids may not collide with the harness's own.**
    ///
    /// The reported session ran 115 native `TaskCreate` calls beside a 34-task taskq queue.
    /// Both number from 1, `TaskFold` keys on the id string, and `join` retains the id out
    /// before pushing — so whichever resolved last owned the row. 7 of taskq's 34 survived;
    /// 27 were wearing a native task's subject, including the two the owner asked about.
    #[test]
    fn a_taskq_task_cannot_shadow_a_native_one_of_the_same_number() {
        let mut fold = crate::engine::tasks::TaskFold::default();
        // The harness's own tools: a create, then the result that names its id.
        fold.apply(&TaskOp::Create {
            tool_use_id: "native-1".into(),
            subject: "Weave plan() candidates into plain migrate".into(),
            description: "the native one".into(),
            active_form: String::new(),
            blocked_by: Vec::new(),
        });
        fold.on_tool_result("native-1", "Created task #27: Weave plan()…");
        // taskq's #27, arriving the way a record does.
        for op in taskq_ops(
            "b1",
            "##taskq/v1 {\"rid\":\"r1\",\"op\":\"create\",\"task\":\"27\",\
             \"subject\":\"Evolution plan: settle the remaining steps\",\
             \"changes\":{\"status\":{\"from\":null,\"to\":\"pending\"}}}",
        ) {
            fold.apply(&op);
        }
        let rows: Vec<(&str, &str)> = fold
            .snapshot()
            .items
            .iter()
            .map(|t| (t.id.as_str(), t.subject.as_str()))
            .collect();
        assert_eq!(
            rows,
            [
                ("27", "Weave plan() candidates into plain migrate"),
                ("q27", "Evolution plan: settle the remaining steps"),
            ],
            "both rows survive, and the bare number stays the harness's"
        );
        // taskq's own user tier already carries its prefix, so it passes through untouched
        // rather than becoming `qu12`.
        for op in taskq_ops(
            "b2",
            "##taskq/v1 {\"rid\":\"r2\",\"op\":\"done\",\"task\":\"u12\",\"subject\":\"a user task\",\
             \"tier\":\"user\",\"changes\":{\"status\":{\"from\":null,\"to\":\"completed\"}}}",
        ) {
            fold.apply(&op);
        }
        assert!(
            fold.snapshot().items.iter().any(|t| t.id == "u12"),
            "a user-tier id is already namespaced by taskq itself"
        );
        // Ordering is by (prefix, NUMBER): `q10` must not file between `q1` and `q2`.
        for n in [1u32, 2, 10] {
            for op in taskq_ops(
                "b3",
                &format!(
                    "##taskq/v1 {{\"rid\":\"r{n}x\",\"op\":\"create\",\"task\":\"{n}\",\
                     \"subject\":\"t{n}\",\"changes\":{{\"status\":{{\"from\":null,\"to\":\"pending\"}}}}}}"
                ),
            ) {
                fold.apply(&op);
            }
        }
        let ids: Vec<&str> = fold
            .snapshot()
            .items
            .iter()
            .map(|t| t.id.as_str())
            .collect();
        assert_eq!(
            ids,
            ["27", "q1", "q2", "q10", "q27", "u12"],
            "numbers sort as numbers inside each queue's prefix"
        );
    }

    /// **rev 5: the record publishes the description, so a description the shell ate is
    /// recoverable from the transcript alone.**
    ///
    /// taskq rev 5 (agentdev 2d74e99) puts `changes.description` on a create and an update,
    /// bounded at 8 KB where everything else stays at 120 — and made repo queues
    /// machine-local, so `queue_tasks` can no longer be the answer for a transcript read
    /// anywhere but the machine that owns the checkout. The record is now the portable copy.
    ///
    /// The join rule: the COMMAND wins where it produced text (exact and unbounded), the
    /// record is the floor. That works because a create record stands its task up BEFORE its
    /// draft resolves, and resolving fills rather than erases.
    #[test]
    fn a_rev5_record_supplies_the_description_the_shell_ate() {
        let rec = |desc: &str| {
            format!(
                "##taskq/v1 {{\"rid\":\"r1\",\"op\":\"create\",\"task\":\"1\",\"subject\":\"Ship it\",\
                 \"changes\":{{\"status\":{{\"from\":null,\"to\":\"pending\"}},\
                 \"description\":{{\"from\":null,\"to\":\"{desc}\"}},\
                 \"activeForm\":{{\"from\":null,\"to\":\"Shipping\"}}}}}}"
            )
        };
        let run = |cmd: &str, record: &str| {
            let mut fold = crate::engine::tasks::TaskFold::default();
            for op in taskq_create_ops("b1", cmd) {
                fold.apply(&op);
            }
            for op in taskq_ops("b1", record) {
                fold.apply(&op);
            }
            let t = fold.snapshot().items[0].clone();
            (t.subject, t.description, t.active_form)
        };

        // The reported case: the shell ate the description, so the command carries none.
        assert_eq!(
            run(
                "taskq create --subject \"Ship it\" --description \"$D\"",
                &rec("from the record")
            ),
            (
                "Ship it".to_string(),
                "from the record".to_string(),
                "Shipping".to_string()
            ),
            "the record is the floor when the command half is gone"
        );
        // When the command DID carry it, that text wins — it is exact and unbounded, while
        // the record's copy is capped.
        assert_eq!(
            run(
                "taskq create --subject \"Ship it\" --description 'the full brief, uncapped' \
                 --active-form 'Shipping it'",
                &rec("the capped copy")
            ),
            (
                "Ship it".to_string(),
                "the full brief, uncapped".to_string(),
                "Shipping it".to_string()
            ),
            "the command is the authority where it has text"
        );
    }

    /// **A record QUOTED in a document is not a mutation.** taskq prints its record, so a
    /// real one begins a line; an indented one is a quotation.
    ///
    /// This is not hypothetical: `taskq-DESIGN.md` §9 prints a sample record inside an
    /// indented code block, and two unrelated sessions that had merely READ that file were
    /// showing its example — "Activity pane: humanize ETA", a task belonging to nobody — as a
    /// real row in their panel.
    #[test]
    fn a_record_quoted_in_a_document_is_not_a_mutation() {
        // The real §9 sample, as the document indents it.
        let doc = "Record format\n\n    ##taskq/v1 {\"rid\":\"m2k9x1-ab3f\",\"ts\":\
                   \"2026-08-29T06:12:33Z\",\"repo\":\"crux-web\",\"op\":\"claim\",\"task\":\"12\",\
                   \"subject\":\"Activity pane: humanize ETA\",\"changes\":{\"status\":{\"from\":\
                   \"pending\",\"to\":\"in_progress\"}}}\n\nThe sentinel contains no quotes.\n";
        assert!(
            taskq_ops("b1", doc).is_empty(),
            "an indented sample is a quotation, not something that happened"
        );
        // A diff of the journal quotes them too — with a marker column, which already fails
        // the prefix test, and with a context space, which now does.
        for quoted in [" ", "+", "-", "> ", "\t"] {
            let line = format!(
                "{quoted}##taskq/v1 {{\"rid\":\"r1\",\"op\":\"done\",\"task\":\"7\",\
                 \"subject\":\"s\",\"changes\":{{\"status\":{{\"from\":null,\"to\":\"completed\"}}}}}}"
            );
            assert!(
                taskq_ops("b1", &line).is_empty(),
                "a record behind {quoted:?} was quoted by something"
            );
        }
        // The same record, printed rather than quoted, still decodes.
        let printed =
            "##taskq/v1 {\"rid\":\"r1\",\"op\":\"done\",\"task\":\"7\",\"subject\":\"s\",\
                       \"changes\":{\"status\":{\"from\":null,\"to\":\"completed\"}}}";
        assert_eq!(
            taskq_ops("b1", printed).len(),
            1,
            "column 0 is the CLI's own output"
        );
    }

    /// **A command this tokenizer cannot read must produce NO drafts, not wrong ones.**
    ///
    /// [`shell_words`] is not a shell. An apostrophe inside a quoted heredoc — "It's" in a
    /// task's own prose — flips its single-quote state and hides every boundary after it, so
    /// a command that wrote two `create`s yields one. That draft then pairs with the FIRST
    /// record by ordinal, which puts the second task's text under the first task's id and
    /// drops the second task entirely. Measured on the migration that filed this repo's
    /// queue: task #9 wore #10's subject and #10 was missing from the panel.
    ///
    /// Unclosed quoting at the end of tokenization is the signal, and it is exact: it means
    /// the reader is still inside a string it never left.
    #[test]
    fn a_command_that_did_not_tokenize_yields_no_drafts() {
        // One ODD apostrophe in a heredoc body is all it takes: `Cursor'd` opens a
        // single-quote state that swallows the next `create` boundary whole.
        let broken = "TQ=/x/taskq\nD=$(cat <<'EOF'\n\
                      Parked: needs an explicit go-ahead.\n\
                      EOF\n)\n\
                      \"$TQ\" create --subject \"Sunset the compat symlinks\" --description \"$D\"\n\
                      D=$(cat <<'EOF'\n\
                      Cursor'd resume for --dump --json.\n\
                      EOF\n)\n\
                      \"$TQ\" create --subject \"Cursor resume\" --description \"$D\"";
        assert!(
            taskq_create_ops("b1", broken).is_empty(),
            "one unbalanced quote makes every boundary after it a guess"
        );
        // The records still stand both tasks up, correctly titled — which is the whole point
        // of refusing the drafts rather than trusting them.
        let recs = [1, 2]
            .map(|n| {
                format!(
                    "##taskq/v1 {{\"rid\":\"r{n}\",\"op\":\"create\",\"task\":\"{n}\",\
                     \"subject\":\"task {n}\",\"changes\":{{\"status\":{{\"from\":null,\
                     \"to\":\"pending\"}}}}}}"
                )
            })
            .join("\n");
        let mut fold = crate::engine::tasks::TaskFold::default();
        for op in taskq_ops("b1", &recs) {
            fold.apply(&op);
        }
        let rows: Vec<(&str, &str)> = fold
            .snapshot()
            .items
            .iter()
            .map(|t| (t.id.as_str(), t.subject.as_str()))
            .collect();
        assert_eq!(
            rows,
            [("q1", "task 1"), ("q2", "task 2")],
            "no draft, no hole: the record names its own task"
        );
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
        assert_eq!(task_id, "q47");
        assert_eq!(status.as_deref(), Some("completed"));
        assert_eq!(
            subject.as_deref(),
            Some("Daemon: one server, many documents"),
            "a stub built from this op is not blank"
        );
    }
}
