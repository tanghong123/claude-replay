//! Claude adapter. Reuses the viewer's `discover`; supervises `claude --resume`/
//! `--session-id` with `--dangerously-skip-permissions`, uses Claude's native task
//! queue (`~/.claude/tasks/`) for the "planned ≠ done" completion signal, and runs
//! the plan→execute two-step on resume.

use super::agent::{
    self, AgentAdapter, Brief, Invocation, Mode, ResumableSession, TaskQueue, Trigger, TurnContext,
    TurnOutcome,
};
use crate::{discover, Agent};
use anyhow::{anyhow, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub struct ClaudeAdapter;

/// Terminal errors: matching these in a turn's output marks the run failed rather
/// than retrying (port of claude-jdi's `UNRECOVERABLE_RE`).
const UNRECOVERABLE: &[&str] = &[
    "\"type\":\"authentication_error\"",
    "\"type\":\"permission_error\"",
    "\"type\":\"invalid_request_error\"",
    "\"type\":\"billing_error\"",
    "\"type\":\"not_found_error\"",
    "invalid_api_key",
    "invalid x-api-key",
    "Credit balance is too low",
    "prompt is too long",
];

impl AgentAdapter for ClaudeAdapter {
    fn id(&self) -> Agent {
        Agent::Claude
    }

    /// Claude plans then executes: a dump turn enqueues the plan and STOPs, an
    /// execute turn drains it. `start` feeds the task on the first turn.
    fn initial_mode(&self, trigger: Trigger) -> Mode {
        match trigger {
            Trigger::Start => Mode::Start,
            Trigger::Resume => Mode::ResumeDump,
            Trigger::BacklogDrain => Mode::BacklogDump,
        }
    }

    /// After the fresh/dump turn, drain the task list on relaunches.
    fn continue_mode(&self) -> Mode {
        Mode::ResumeExecute
    }

    fn resolve_binary(&self) -> Result<PathBuf> {
        if let Some(p) = std::env::var_os("AGENT_JDI_CLAUDE_BIN") {
            return Ok(PathBuf::from(p));
        }
        agent::which("claude").ok_or_else(|| anyhow!("claude CLI not found on PATH"))
    }

    fn build_invocation(&self, ctx: &TurnContext) -> Invocation {
        let program = self
            .resolve_binary()
            .unwrap_or_else(|_| PathBuf::from("claude"));
        let mut args: Vec<String> = Vec::new();
        // Resume an existing session, else pin a fresh id.
        if ctx.session_created {
            args.push("--resume".into());
        } else {
            args.push("--session-id".into());
        }
        args.push(ctx.session_id.to_string());
        args.push("--dangerously-skip-permissions".into());
        args.extend(ctx.extra_args.iter().cloned());
        args.push("-p".into());
        args.push(self.prompt_for(ctx.mode, ctx.brief, ctx.session_id));
        Invocation { program, args }
    }

    fn classify(&self, rc: i32, capture: &str, ctx: &TurnContext) -> TurnOutcome {
        // A dump turn (rc 0) advances to its execute phase — planning never "done".
        if rc == 0 {
            match ctx.mode {
                Mode::ResumeDump => return TurnOutcome::AdvanceMode(Mode::ResumeExecute),
                Mode::BacklogDump => return TurnOutcome::AdvanceMode(Mode::BacklogExecute),
                // Start/execute turns: done only if the native task queue is empty;
                // unknown ⇒ trust rc. (After the fresh turn the supervisor has already
                // moved to `continue_mode`, so `Start` here is just for exhaustiveness.)
                Mode::Start | Mode::ResumeExecute | Mode::BacklogExecute | Mode::Execute => {
                    // "planned ≠ done": prefer the native task queue, but fall back to
                    // the checklist file when this session has no task tools (they
                    // aren't always available) — otherwise an unknown count would be
                    // read as "finished" and a half-done run would stop after one turn.
                    let open = self
                        .task_queue()
                        .and_then(|q| q.open_count(ctx.session_id))
                        .or_else(|| {
                            ctx.brief
                                .checklist
                                .as_deref()
                                .and_then(open_checklist_items)
                        });
                    return match open {
                        Some(0) | None => TurnOutcome::Done,
                        Some(_) => TurnOutcome::Retry, // stopped early with work left
                    };
                }
            }
        }
        if rc == 130 || rc == 143 {
            return TurnOutcome::Stopped(rc);
        }
        if capture.contains("No conversation found") {
            return TurnOutcome::RecreateSession;
        }
        if UNRECOVERABLE.iter().any(|needle| capture.contains(needle)) {
            return TurnOutcome::Failed(rc);
        }
        TurnOutcome::Retry
    }

    fn discover_resumable(&self, cwd: &Path) -> Result<ResumableSession> {
        let (id, path, mtime) = discover::latest_for_cwd(cwd)
            .ok_or_else(|| anyhow!("no Claude session found for {}", cwd.display()))?;
        let idle_secs = mtime.elapsed().map(|d| d.as_secs()).unwrap_or(0);
        Ok(ResumableSession {
            id,
            transcript: path,
            idle_secs,
        })
    }

    fn transcript_path(&self, session_id: &str, _cwd: &Path) -> Option<PathBuf> {
        discover::transcript_by_id(session_id)
    }

    /// Claude pins the id, so the transcript path is deterministic even before the
    /// file exists.
    fn expected_transcript(&self, session_id: &str, cwd: &Path) -> Option<PathBuf> {
        Some(discover::claude_transcript_path(cwd, session_id))
    }

    fn prompt_for(&self, mode: Mode, brief: &Brief, session_id: &str) -> String {
        // `Start`'s task text is appended last (below), after the queue-file note, so
        // the prompt reads instructions → mechanics → "The task:" → the task itself.
        let mut out = match mode {
            Mode::Start => START_PREAMBLE.to_string(),
            Mode::ResumeDump | Mode::BacklogDump => DUMP_PROMPT.to_string(),
            _ => EXECUTE_PROMPT.to_string(),
        };
        // Follow-ups the human queued for this session while it ran. A dump turn is
        // what turns them into tasks, so they MUST reach the prompt — they were being
        // dropped entirely. (Port of the bash "ALSO fold in …" paragraph.)
        if matches!(mode, Mode::ResumeDump | Mode::BacklogDump) && !brief.backlog.is_empty() {
            let items = brief
                .backlog
                .iter()
                .enumerate()
                .map(|(i, b)| format!("### Backlog item {}\n{}", i + 1, b.trim()))
                .collect::<Vec<_>>()
                .join("\n\n");
            out.push_str(&format!(
                "\n\nALSO fold in the {} follow-up message(s) the human queued for THIS \
                 session while you were working. Go through them ONE BY ONE: understand \
                 each request, do a brief diagnosis of what it entails, and enqueue the \
                 concrete, fully-scoped work it implies. Anything ambiguous: do NOT \
                 enqueue it.\n\n{items}",
                brief.backlog.len()
            ));
        }
        // Adaptive: a session that demonstrably drives the native queue doesn't need
        // the fallback paragraph, so don't spend tokens on it.
        if let Some(path) = brief
            .checklist
            .as_ref()
            .filter(|_| !has_native_tasks(session_id))
        {
            out.push_str(&format!(
                "\n\nThe queue file, if this session has no task-management tools: `{}` — \
                 one `- [ ]` line per queued item in order, flipped to `- [x]` the moment \
                 that item is finished, with any blocker noted on its line. Keep it \
                 current: when the tools are absent this file is the queue, and it is how \
                 completion is tracked.",
                path.display()
            ));
        }
        // The operator's instruction applies to every mode, not just a fresh start —
        // on resume it was previously dropped on the floor. A fresh start puts the
        // same text last, as the task itself.
        if !brief.text.trim().is_empty() {
            out.push_str(&if matches!(mode, Mode::Start) {
                format!("\n\nThe task:\n\n{}", brief.text.trim())
            } else {
                format!("\n\nAdditional instruction: {}", brief.text.trim())
            });
        }
        out
    }

    fn task_queue(&self) -> Option<&dyn TaskQueue> {
        Some(&ClaudeTaskQueue)
    }

    fn unattended_note(&self) -> &'static str {
        "--dangerously-skip-permissions (unattended)"
    }

    /// Hand the session to a human: `claude --resume <id>` — a real interactive
    /// session (never `-p`). `autonomous` keeps `--dangerously-skip-permissions`, so
    /// a run that was already unattended doesn't start prompting on every tool call.
    fn interactive_invocation(
        &self,
        session_id: &str,
        _cwd: &Path,
        autonomous: bool,
    ) -> Option<Invocation> {
        if session_id.is_empty() {
            return None;
        }
        let program = self.resolve_binary().ok()?;
        let mut args = vec!["--resume".into(), session_id.to_string()];
        if autonomous {
            args.push("--dangerously-skip-permissions".into());
        }
        Some(Invocation { program, args })
    }

    fn resume_commands(&self, session_id: &str) -> Vec<(String, String)> {
        if session_id.is_empty() {
            return Vec::new();
        }
        vec![
            (
                "# autonomous — keeps running tools without asking:".into(),
                format!("claude --resume {session_id} --dangerously-skip-permissions"),
            ),
            (
                "# supervised — approve each action:".into(),
                format!("claude --resume {session_id}"),
            ),
        ]
    }
}

/// Does this session demonstrably drive Claude's native task queue? True once it has
/// written at least one task file — the harness pre-creates the dir (`.lock`,
/// `.highwatermark`) even for sessions that never get the tools, so dir existence
/// alone proves nothing. Used to drop the checklist-fallback paragraph when it's
/// dead weight.
fn has_native_tasks(session_id: &str) -> bool {
    !session_id.is_empty() && ClaudeTaskQueue::tasks(session_id).is_some_and(|t| !t.is_empty())
}

/// Unchecked `- [ ]` items in the fallback checklist. `None` when the file doesn't
/// exist (⇒ unknown, so the caller trusts the exit code rather than assuming work).
fn open_checklist_items(path: &Path) -> Option<usize> {
    let text = std::fs::read_to_string(path).ok()?;
    Some(
        text.lines()
            .filter(|l| {
                let t = l.trim_start();
                // `- [ ]` / `* [ ]`, any amount of inner space, but not `- [x]`.
                (t.starts_with("- [") || t.starts_with("* ["))
                    && t[3..].starts_with(' ')
                    && t[3..].trim_start().starts_with(']')
            })
            .count(),
    )
}

/// Fresh-run turn (`start`). Unlike resume, this one plan *and* executes in a single
/// turn, so it carries a condensed form of both disciplines — build the durable
/// queue, then work it FIFO with the same skip-on-blocked and prerequisite-now rules.
/// A fresh run has no prior conversation to re-derive from: the task text below is
/// the whole input.
const START_PREAMBLE: &str = "You are running UNATTENDED and headless — the human has stepped away and cannot answer anything. Do NOT ask for input, and do NOT stop to confirm.\n\nFirst break the task below into a durable QUEUE of fully-scoped units of work — your task-management tools if this session has them (one task per unit), otherwise the queue file named below. Give each a clear subject and a self-contained description, in the order they should be done; the queue is the source of truth for the run, so it must outlive this turn.\n\nThen work it to completion, FIFO — oldest pending task first:\n  - Mark a task in_progress before you begin, completed the moment it's done.\n  - Commit the work for each task as you finish it, so progress is durable if the run is interrupted.\n  - If a task is blocked — it needs a human decision, or something outside this run — write the blocker onto it and SKIP to the next one rather than stopping.\n  - New work you identify: if the CURRENT task genuinely needs it as a prerequisite, do it now and record it; otherwise append it to the END of the queue. Only fully-scoped work — never speculative or ambiguous items.\n\nKeep going until every task is either completed or explicitly documented as blocked. Do not end the turn early while actionable work remains.";

/// Dump turn. Ported from the bash claude-jdi's `resume_dump_prompt` — the
/// specificity (self-contained descriptions, blockedBy wiring, the anti-duplicate
/// rules, the receipt) is what actually gets a usable queue built. Two deltas on the
/// original: it leads with the durable *queue* rather than with `TaskCreate`, so a
/// session lacking the tools follows the same discipline into the fallback file
/// instead of arguing with the instruction; and it states the FIFO contract up
/// front, since ordering decided at build time is what execution relies on.
const DUMP_PROMPT: &str = "You are running UNATTENDED and headless — the human has stepped away and cannot answer anything. Do NOT ask for input or wait for confirmation.\n\nPut the work we have ALREADY discussed and agreed on in THIS conversation into a durable QUEUE — it is the source of truth for the run, so it must outlive this turn rather than staying in your reply. Record it with your task-management tools if this session has them (one task per unit of work); if it does not have them, keep the queue in the file named below. The queue is worked FIFO, so put the entries in the order they should be done.\n\nEach entry is ONE unit of work with a clear subject and a self-contained description a fresh session could act on with no extra context, status \"pending\"; where one entry must finish before another can start, wire that with blocks/blockedBy.\n\nRules for what to enqueue:\n  - ONLY fully-scoped tasks that need no human decision. Exclude anything ambiguous or still open — do not enqueue it and do not act on it.\n  - Re-derive a FRESH list of what is agreed and still OUTSTANDING right now. Skip anything already completed. Do NOT copy an earlier task dump.\n  - If tasks from this conversation are ALREADY in the queue, reconcile with them — update or reuse; do not create duplicates.\n\nWhen the queue is populated, write ONE line as a receipt: either \"queued: <N> task(s)\" with the count, or \"queued: 0 task(s) — nothing actionable\" if there was no agreed, fully-scoped work.\n\nThen STOP. Do not start implementing — execution is a separate step. Your only job this turn is to populate the queue and write that receipt.";

/// Execute turn. The bash `resume_execute_prompt` plus three deltas that keep an
/// unattended run moving: FIFO (deterministic order, no re-planning each turn);
/// skip-on-blocked promoted from an afterthought to a rule, so one blocker documents
/// itself and yields instead of stalling the queue; and new work split by kind — a
/// prerequisite of the CURRENT task must be done now, because appending it to the
/// end (correct for ordinary follow-ups) would leave the task that needs it
/// permanently unfinishable.
const EXECUTE_PROMPT: &str = "You are running UNATTENDED and headless — the human has stepped away and cannot answer anything. Do NOT ask for input, and do NOT stop to confirm.\n\nWork the queue to completion — your task-management tools if this session has them, otherwise the queue file named below:\n  - Take the oldest still-pending task first (FIFO), respecting blockedBy — never start a task whose blockers aren't completed.\n  - Mark it in_progress before you begin, completed the moment it's done.\n  - Commit the work for each task as you finish it, so progress is durable if the run is interrupted.\n\nIf a task turns out to be blocked — it needs a human decision, or something outside this run — write what the blocker is onto that task and SKIP it, then carry on with the next one. Never end the run over a single blocked task; make as much progress as the rest of the queue allows.\n\nWhen you identify NEW work along the way:\n  - if it is a prerequisite the CURRENT task genuinely needs, do it now and record it — do not push it to the back, or the task that needs it can never finish;\n  - otherwise append it to the END of the queue and carry on in order.\n  - Enqueue only fully-scoped work; never speculative or ambiguous items.\n\nKeep going until EVERY task is either completed or explicitly documented as blocked — nothing silently left pending or in_progress. Do not end the turn early while actionable work remains.";

/// Reads Claude's native task queue under `~/.claude/tasks/<session>/*.json`.
struct ClaudeTaskQueue;

impl ClaudeTaskQueue {
    fn tasks_root() -> PathBuf {
        std::env::var_os("CLAUDE_JDI_TASKS_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let home = std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("."));
                home.join(".claude").join("tasks")
            })
    }

    /// Collect `(status, title)` for each task in a session's json files, in file
    /// order. `None` if the dir is missing or nothing parses (⇒ "unknown", so the
    /// caller trusts the exit code rather than assuming zero).
    fn tasks(session_id: &str) -> Option<Vec<(String, String)>> {
        let dir = Self::tasks_root().join(session_id);
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
            .collect();
        // Claude writes one `<n>.json` per task, so order numerically — a plain
        // string sort gives 18, 19, 2, 20, 3.
        entries.sort_by_key(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(u64::MAX)
        });
        let mut out: Vec<(String, String)> = Vec::new();
        let mut parsed_any = false;
        for p in entries {
            let Ok(text) = std::fs::read_to_string(&p) else {
                continue;
            };
            let Ok(v) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            parsed_any = true;
            collect_tasks(&v, &mut out);
        }
        parsed_any.then_some(out)
    }
}

/// Pull every `(status, title)` out of a tasks document (array of tasks,
/// `{tasks:[…]}`, or a single task object). Title prefers `subject` — Claude's real
/// schema is `{id, subject, description, activeForm, status, blocks, blockedBy}`,
/// and `description` is long prose that would swamp the checklist.
fn collect_tasks(v: &Value, out: &mut Vec<(String, String)>) {
    match v {
        Value::Array(items) => items.iter().for_each(|it| collect_tasks(it, out)),
        Value::Object(map) => {
            if let Some(Value::Array(items)) = map.get("tasks") {
                items.iter().for_each(|it| collect_tasks(it, out));
            } else if let Some(status) = map.get("status").and_then(Value::as_str) {
                let title = ["subject", "content", "title", "description", "activeForm"]
                    .iter()
                    .find_map(|k| map.get(*k).and_then(Value::as_str))
                    .unwrap_or("")
                    .to_string();
                out.push((status.to_string(), title));
            }
        }
        _ => {}
    }
}

impl TaskQueue for ClaudeTaskQueue {
    fn open_count(&self, session_id: &str) -> Option<usize> {
        Self::tasks(session_id).map(|ts| ts.iter().filter(|(s, _)| s != "completed").count())
    }

    /// A live checklist: `✓`/`▶`/`·` per task with its title, then `done/total`.
    fn render(&self, session_id: &str) -> String {
        let Some(ts) = Self::tasks(session_id) else {
            return "  (no task queue for this session)".to_string();
        };
        if ts.is_empty() {
            return "  (task queue is empty)".to_string();
        }
        let mut done = 0usize;
        let mut s = String::new();
        for (i, (status, title)) in ts.iter().enumerate() {
            let marker = match status.as_str() {
                "completed" => {
                    done += 1;
                    "✓"
                }
                "in_progress" => "▶",
                _ => "·",
            };
            let t: String = title.replace('\n', " ");
            let t = if t.chars().count() > 68 {
                format!("{}…", t.chars().take(67).collect::<String>())
            } else {
                t
            };
            s.push_str(&format!("  {marker} [{}] {t}\n", i + 1));
        }
        s.push_str(&format!("  ── {done}/{} completed", ts.len()));
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `CLAUDE_JDI_TASKS_ROOT` is process-global, so tests that point it at their own
    /// fixture must not run concurrently (Rust runs tests in parallel threads).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn ctx<'a>(mode: Mode, created: bool, id: &'a str, brief: &'a Brief) -> TurnContext<'a> {
        TurnContext {
            mode,
            session_id: id,
            session_created: created,
            cwd: Path::new("/tmp/repo"),
            brief,
            extra_args: &[],
        }
    }

    #[test]
    fn fresh_vs_resume_invocation() {
        let a = ClaudeAdapter;
        let brief = Brief::default();
        let fresh = a.build_invocation(&ctx(Mode::Execute, false, "sid", &brief));
        assert!(fresh.args.windows(2).any(|w| w == ["--session-id", "sid"]));
        let resumed = a.build_invocation(&ctx(Mode::ResumeExecute, true, "sid", &brief));
        assert!(resumed.args.windows(2).any(|w| w == ["--resume", "sid"]));
        assert!(resumed
            .args
            .iter()
            .any(|x| x == "--dangerously-skip-permissions"));
    }

    /// The operator's instruction (what `handoff`/`resume` pass) must reach the agent
    /// on **resume**, not just a fresh start — it used to be written to task.md and
    /// then dropped, so a handoff message never appeared in the transcript.
    #[test]
    fn resume_prompts_carry_the_operator_instruction() {
        let a = ClaudeAdapter;
        let brief = Brief {
            text: "finish the refactor and commit".into(),
            ..Default::default()
        };
        for mode in [Mode::ResumeDump, Mode::ResumeExecute, Mode::Execute] {
            let p = a.prompt_for(mode, &brief, "");
            assert!(
                p.contains("finish the refactor and commit"),
                "{mode:?} dropped the instruction: {p}"
            );
        }
        // A fresh start already embedded it in the preamble — don't duplicate it.
        let start = a.prompt_for(Mode::Start, &brief, "");
        assert_eq!(start.matches("finish the refactor").count(), 1, "{start}");
    }

    /// Task-management tools aren't available in every session. The prompt must not
    /// hard-require them, and the done-signal must fall back to the checklist file —
    /// otherwise an unknown count reads as "finished" and a half-done run stops.
    #[test]
    fn checklist_fallback_drives_the_done_signal_without_task_tools() {
        let _g = lock_env();
        let a = ClaudeAdapter;
        let dir = std::env::temp_dir().join(format!("jdi-checklist-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let list = dir.join("checklist.md");
        let brief = Brief {
            checklist: Some(list.clone()),
            ..Default::default()
        };

        // The prompt points at the file and doesn't demand TaskCreate exist.
        let p = a.prompt_for(Mode::ResumeDump, &brief, "");
        assert!(p.contains(&list.display().to_string()), "{p}");
        assert!(
            p.contains("if this session has them"),
            "must be conditional: {p}"
        );

        // Unchecked items left → not done, even though the native queue is absent.
        std::fs::write(
            &list,
            "- [x] shipped it\n- [ ] still open\n* [ ] also open\n",
        )
        .unwrap();
        assert_eq!(open_checklist_items(&list), Some(2));
        let ctx = ctx(Mode::ResumeExecute, true, "no-such-session", &brief);
        assert_eq!(a.classify(0, "", &ctx), TurnOutcome::Retry);

        // All checked → done.
        std::fs::write(&list, "- [x] shipped it\n- [x] and this\n").unwrap();
        assert_eq!(open_checklist_items(&list), Some(0));
        assert_eq!(a.classify(0, "", &ctx), TurnOutcome::Done);

        // No file at all → unknown → trust the exit code (unchanged behavior).
        std::fs::remove_file(&list).unwrap();
        assert_eq!(open_checklist_items(&list), None);
        assert_eq!(a.classify(0, "", &ctx), TurnOutcome::Done);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Every phase that plans or executes must carry the same queue discipline —
    /// including `Start`, which does both in one turn. The prerequisite carve-out is
    /// the subtle one: appending a prereq to the END would leave the task that needs
    /// it permanently unfinishable.
    #[test]
    fn all_phases_state_the_queue_discipline() {
        let _g = lock_env();
        let a = ClaudeAdapter;
        let brief = Brief {
            text: "do the thing".into(),
            ..Default::default()
        };

        // Planning phases build a durable, ordered queue.
        for mode in [Mode::Start, Mode::ResumeDump, Mode::BacklogDump] {
            let p = a.prompt_for(mode, &brief, "");
            assert!(p.contains("QUEUE"), "{mode:?} must name the queue: {p}");
            assert!(p.contains("self-contained"), "{mode:?}: {p}");
            // Behaviour first: never demand the tools unconditionally.
            assert!(
                p.contains("if this session has them"),
                "{mode:?} must stay conditional about tools: {p}"
            );
        }

        // Executing phases work it FIFO, skip blockers, and place new work by kind.
        for mode in [
            Mode::Start,
            Mode::ResumeExecute,
            Mode::BacklogExecute,
            Mode::Execute,
        ] {
            let p = a.prompt_for(mode, &brief, "");
            assert!(p.contains("FIFO"), "{mode:?} must state FIFO order: {p}");
            assert!(
                p.contains("SKIP"),
                "{mode:?} must skip blockers, not stall: {p}"
            );
            assert!(
                p.contains("prerequisite") && p.contains("END of the queue"),
                "{mode:?} must split prerequisites from appended follow-ups: {p}"
            );
            assert!(
                p.contains("Do not end the turn early"),
                "{mode:?} must forbid stopping with work left: {p}"
            );
        }
    }

    /// Queued backlog follow-ups must reach the dump prompt — that turn is what
    /// converts them into tasks. They were being dropped on the floor entirely.
    #[test]
    fn dump_prompts_fold_in_queued_backlog_items() {
        let _g = lock_env();
        let a = ClaudeAdapter;
        let brief = Brief {
            backlog: vec![
                "also update the changelog".into(),
                "bump the version".into(),
            ],
            ..Default::default()
        };
        for mode in [Mode::ResumeDump, Mode::BacklogDump] {
            let p = a.prompt_for(mode, &brief, "");
            assert!(p.contains("2 follow-up message(s)"), "{mode:?}: {p}");
            assert!(p.contains("also update the changelog"), "{mode:?}: {p}");
            assert!(p.contains("bump the version"), "{mode:?}: {p}");
        }
        // An execute turn drains the queue; it doesn't re-triage the backlog.
        let exec = a.prompt_for(Mode::ResumeExecute, &brief, "");
        assert!(!exec.contains("also update the changelog"), "{exec}");
        // No backlog → no stray section.
        let empty = a.prompt_for(Mode::ResumeDump, &Brief::default(), "");
        assert!(!empty.contains("ALSO fold in"), "{empty}");
    }

    /// The fallback paragraph is insurance, not boilerplate: a session that
    /// demonstrably drives the native queue shouldn't pay tokens for it. The harness
    /// pre-creates the task dir (.lock/.highwatermark) even when the tools never
    /// appear, so presence is judged on actual task files.
    #[test]
    fn checklist_paragraph_is_omitted_when_the_session_uses_native_tasks() {
        let _g = lock_env();
        let root = std::env::temp_dir().join(format!("jdi-adaptive-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::env::set_var("CLAUDE_JDI_TASKS_ROOT", &root);

        let a = ClaudeAdapter;
        let brief = Brief {
            checklist: Some(root.join("checklist.md")),
            ..Default::default()
        };
        let marker = "no task-management tools";

        // A dir with only the harness's bookkeeping files ⇒ no proof of tools.
        let bare = root.join("bare-session");
        std::fs::create_dir_all(&bare).unwrap();
        std::fs::write(bare.join(".lock"), "").unwrap();
        std::fs::write(bare.join(".highwatermark"), "7").unwrap();
        assert!(!has_native_tasks("bare-session"));
        assert!(
            a.prompt_for(Mode::ResumeDump, &brief, "bare-session")
                .contains(marker),
            "fallback must be offered when tools are unproven"
        );

        // A real task file ⇒ the queue works here; drop the paragraph.
        let live = root.join("live-session");
        std::fs::create_dir_all(&live).unwrap();
        std::fs::write(
            live.join("3.json"),
            r#"{"id":"3","subject":"Ship it","status":"pending","blockedBy":[]}"#,
        )
        .unwrap();
        assert!(has_native_tasks("live-session"));
        assert!(
            !a.prompt_for(Mode::ResumeDump, &brief, "live-session")
                .contains(marker),
            "should not spend tokens on a fallback this session doesn't need"
        );

        std::env::remove_var("CLAUDE_JDI_TASKS_ROOT");
        std::fs::remove_dir_all(&root).ok();
    }

    /// Claude's real schema is `{id, subject, description, activeForm, status, …}`
    /// with one `<n>.json` per task: show `subject` (not the long `description`) and
    /// order numerically (a string sort gives 18, 19, 2, 20, 3).
    #[test]
    fn task_queue_reads_subject_in_numeric_file_order() {
        let _g = lock_env();
        let root = std::env::temp_dir().join(format!("jdi-schema-{}", std::process::id()));
        let sid = "s";
        let dir = root.join(sid);
        std::fs::create_dir_all(&dir).unwrap();
        for (n, subject) in [(2, "second"), (18, "eighteenth"), (3, "third")] {
            std::fs::write(
                dir.join(format!("{n}.json")),
                format!(
                    r#"{{"id":"{n}","subject":"{subject}","description":"long prose that should not be shown","activeForm":"doing","status":"pending"}}"#
                ),
            )
            .unwrap();
        }
        std::env::set_var("CLAUDE_JDI_TASKS_ROOT", &root);
        let out = ClaudeTaskQueue.render(sid);
        std::env::remove_var("CLAUDE_JDI_TASKS_ROOT");

        assert!(out.contains("[1] second"), "{out}");
        assert!(out.contains("[2] third"), "numeric order (2,3,18): {out}");
        assert!(out.contains("[3] eighteenth"), "{out}");
        assert!(
            !out.contains("long prose"),
            "description must not swamp it: {out}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn interactive_takeover_keeps_the_runs_permission_posture() {
        let a = ClaudeAdapter;
        let cwd = Path::new("/tmp/repo");

        // Default (autonomous): the session was running unattended, so keep
        // --dangerously-skip-permissions — otherwise takeover would start
        // prompting on every tool call. Still interactive: never `-p`.
        let auto = a
            .interactive_invocation("sid", cwd, true)
            .expect("claude resumes interactively");
        assert_eq!(
            auto.args,
            vec![
                "--resume".to_string(),
                "sid".to_string(),
                "--dangerously-skip-permissions".to_string()
            ]
        );
        assert!(
            !auto.args.iter().any(|x| x == "-p"),
            "must not be a batch turn"
        );

        // --supervised: approvals on.
        let sup = a.interactive_invocation("sid", cwd, false).unwrap();
        assert_eq!(sup.args, vec!["--resume".to_string(), "sid".to_string()]);
        assert!(!sup
            .args
            .iter()
            .any(|x| x == "--dangerously-skip-permissions"));

        // No id yet → nothing to resume.
        assert!(a.interactive_invocation("", cwd, true).is_none());
    }

    #[test]
    fn dump_advances_then_execute_is_terminal_on_clean_exit() {
        let a = ClaudeAdapter;
        let brief = Brief::default();
        // Dump turn (rc 0) → advance to execute.
        assert_eq!(
            a.classify(0, "", &ctx(Mode::ResumeDump, true, "sid", &brief)),
            TurnOutcome::AdvanceMode(Mode::ResumeExecute)
        );
        // Execute turn, no task queue for this id → unknown → trust rc → Done.
        assert_eq!(
            a.classify(0, "", &ctx(Mode::ResumeExecute, true, "no-such", &brief)),
            TurnOutcome::Done
        );
    }

    #[test]
    fn fresh_start_invocation_pins_id_and_feeds_the_task() {
        let a = ClaudeAdapter;
        let brief = Brief {
            text: "add a HELLO file".into(),
            ..Default::default()
        };
        // A pinned fresh run: session_created=false → --session-id; prompt = the task.
        let inv = a.fresh_invocation(&ctx(Mode::Start, false, "the-uuid", &brief), "n");
        assert!(inv
            .args
            .windows(2)
            .any(|w| w == ["--session-id", "the-uuid"]));
        assert!(!inv.args.iter().any(|x| x == "--resume"));
        assert!(
            inv.args.last().unwrap().contains("add a HELLO file"),
            "task fed as the prompt"
        );
    }

    #[test]
    fn classify_terminal_and_recreate() {
        let a = ClaudeAdapter;
        let brief = Brief::default();
        let c = ctx(Mode::Execute, true, "sid", &brief);
        assert_eq!(a.classify(130, "", &c), TurnOutcome::Stopped(130));
        assert_eq!(
            a.classify(1, "No conversation found", &c),
            TurnOutcome::RecreateSession
        );
        assert_eq!(
            a.classify(1, "{\"type\":\"authentication_error\"}", &c),
            TurnOutcome::Failed(1)
        );
        assert_eq!(a.classify(1, "some transient blip", &c), TurnOutcome::Retry);
    }

    #[test]
    fn task_queue_open_count_from_json() {
        let _g = lock_env();
        let dir = std::env::temp_dir().join(format!("claude-tasks-{}", std::process::id()));
        let sid = "sess-x";
        let sdir = dir.join(sid);
        std::fs::create_dir_all(&sdir).unwrap();
        std::fs::write(
            sdir.join("a.json"),
            r#"[{"status":"completed"},{"status":"in_progress"},{"status":"pending"}]"#,
        )
        .unwrap();
        std::env::set_var("CLAUDE_JDI_TASKS_ROOT", &dir);
        let q = ClaudeTaskQueue;
        assert_eq!(q.open_count(sid), Some(2)); // 2 not completed
        assert_eq!(q.open_count("missing"), None); // unknown

        // The detailed render is a per-task checklist with a completion tally.
        std::fs::write(
            sdir.join("a.json"),
            r#"[{"status":"completed","content":"Commit the guard"},
                {"status":"in_progress","content":"Install the skill"},
                {"status":"pending","content":"Interactive multi-select"}]"#,
        )
        .unwrap();
        let out = q.render(sid);
        assert!(out.contains("✓ [1] Commit the guard"), "{out}");
        assert!(out.contains("▶ [2] Install the skill"), "{out}");
        assert!(out.contains("· [3] Interactive multi-select"), "{out}");
        assert!(out.contains("── 1/3 completed"), "{out}");

        std::env::remove_var("CLAUDE_JDI_TASKS_ROOT");
        std::fs::remove_dir_all(&dir).ok();
    }
}
