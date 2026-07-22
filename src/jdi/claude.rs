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
        args.push(self.prompt_for(ctx.mode, ctx.brief));
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
                    return match self.task_queue().and_then(|q| q.open_count(ctx.session_id)) {
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

    fn prompt_for(&self, mode: Mode, brief: &Brief) -> String {
        match mode {
            Mode::Start => format!("{START_PREAMBLE}\n\n{}", brief.text),
            Mode::ResumeDump | Mode::BacklogDump => DUMP_PROMPT.to_string(),
            _ => EXECUTE_PROMPT.to_string(),
        }
    }

    fn task_queue(&self) -> Option<&dyn TaskQueue> {
        Some(&ClaudeTaskQueue)
    }

    fn unattended_note(&self) -> &'static str {
        "--dangerously-skip-permissions (unattended)"
    }

    /// Hand the session to a human: `claude --resume <id>` — interactive, with
    /// permissions ON (no `-p`, no skip-permissions).
    fn interactive_invocation(&self, session_id: &str, _cwd: &Path) -> Option<Invocation> {
        if session_id.is_empty() {
            return None;
        }
        let program = self.resolve_binary().ok()?;
        Some(Invocation {
            program,
            args: vec!["--resume".into(), session_id.to_string()],
        })
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

const START_PREAMBLE: &str = "You are running UNATTENDED and headless — the human has stepped away and cannot answer. Do NOT ask for input. Use your task-management tools to plan and complete the task below, committing per task, until everything is done. The task:";

const DUMP_PROMPT: &str = "You are running UNATTENDED and headless. Enqueue the agreed work using your task-management tools (TaskCreate; set blockedBy where needed), write a one-line receipt, then STOP. Do not begin executing yet.";

const EXECUTE_PROMPT: &str = "You are running UNATTENDED and headless — do NOT ask for input. Drain your task list to completion: pick the next actionable task (respect blockedBy), mark it in_progress, complete it, and commit the work per task. Keep going until every task is completed; nothing left pending or in_progress. If a task is genuinely blocked by something only a human can resolve, note it and move on.";

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
        entries.sort(); // stable task order across files
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
/// `{tasks:[…]}`, or a single task object). Title = the first present of
/// `content`/`description`/`title`/`activeForm`.
fn collect_tasks(v: &Value, out: &mut Vec<(String, String)>) {
    match v {
        Value::Array(items) => items.iter().for_each(|it| collect_tasks(it, out)),
        Value::Object(map) => {
            if let Some(Value::Array(items)) = map.get("tasks") {
                items.iter().for_each(|it| collect_tasks(it, out));
            } else if let Some(status) = map.get("status").and_then(Value::as_str) {
                let title = ["content", "description", "title", "activeForm"]
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

    #[test]
    fn interactive_takeover_is_a_plain_resume_no_autonomy_flags() {
        let a = ClaudeAdapter;
        let inv = a
            .interactive_invocation("sid", Path::new("/tmp/repo"))
            .expect("claude resumes interactively");
        assert_eq!(inv.args, vec!["--resume".to_string(), "sid".to_string()]);
        // A human is present: no unattended `-p` / skip-permissions.
        assert!(!inv.args.iter().any(|x| x == "-p"));
        assert!(!inv
            .args
            .iter()
            .any(|x| x == "--dangerously-skip-permissions"));
        // No id yet → nothing to resume.
        assert!(a
            .interactive_invocation("", Path::new("/tmp/repo"))
            .is_none());
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
            backlog: vec![],
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
