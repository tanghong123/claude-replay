//! The browser harness's kit (#53): what every real-Chrome case needs and none should
//! re-spell — scratch roots, record builders, hermetic stores, a monitor under test, Chrome,
//! and the small vocabulary of actions and probes over a tab.
//!
//! It is an integration-test module (`mod harness;` from each test file), not library code,
//! so the crate's dev-dependencies stay where they are and nothing here reaches
//! `cargo doc --workspace`. Every case in this crate is `#[ignore]`d and runs only under
//! `cargo test -p claude-replay-browser-tests -- --ignored`, which needs a local Chrome and,
//! for the monitor cases, `cargo build --release -p claude-monitor -p claude-monitor-v2`.
//!
//! Conventions the cases rely on:
//! - A case takes [`serial`] first: every case binds fixed loopback ports and shares
//!   `CLAUDE_MONITOR_STATE`, so two cannot overlap.
//! - A case builds its world under [`base`] and points every store env var into it
//!   ([`Stores`]); nothing a case measures comes from this machine's own sessions.
//! - A monitor under test is a [`Monitor`]: it is reaped on drop, its token is read from its
//!   scratch state dir, and [`Monitor::pair`] is the first navigation of every tab.
//! - Probes go through [`probe`] (a JSON value) or [`eval`] (a primitive); waits go through
//!   [`until`], which panics with the last thing it saw — never a vacuous return.

#![allow(dead_code)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

// ── scratch, serialization, reaping ─────────────────────────────────────────────────────────

/// A fresh scratch directory for one case, under the workspace's own temp dir.
pub fn base(name: &str) -> PathBuf {
    hermetic_state();
    let d = std::env::temp_dir().join(format!("cr-browser-follow-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The html server's state dir, once per process: a render policy that offers files, and
/// `CLAUDE_MONITOR_STATE` pointed away from the machine's real one.
pub fn hermetic_state() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let d = std::env::temp_dir().join(format!("cr-browser-state-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        let _ = std::fs::write(d.join("render-policy.json"), b"{\"mode\":\"offered\"}");
        std::env::set_var("CLAUDE_MONITOR_STATE", &d);
    });
}

/// Every case holds this: fixed ports and a shared state dir mean one case at a time.
pub fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// A child process that dies with its guard — a panicking case never strands a server on
/// its port for the next one.
pub struct Reap(pub std::process::Child);
impl Drop for Reap {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

// ── records ─────────────────────────────────────────────────────────────────────────────────
// Claude-format lines. `s` is the minute of a fixed day, so a fixture's clock is stable and
// the index reads these sessions as finished (state derives from the CONTENT clock, not the
// file's mtime). A growth scenario that must read as live uses [`user_at`] & co with a
// now-relative timestamp instead.

const DAY: &str = "2026-08-21T10";

fn stamp(s: u32) -> String {
    format!("{DAY}:{:02}:00Z", s % 60)
}

/// A user turn.
pub fn user(t: &str, s: u32) -> String {
    user_at(t, &stamp(s))
}
/// An assistant text block.
pub fn assistant(t: &str, s: u32) -> String {
    assistant_at(t, &stamp(s))
}
/// A tool call opening (a `Bash` head the transcript renders as a fold header).
pub fn tool_open(id: &str, s: u32) -> String {
    tool_open_at(id, &stamp(s))
}
/// The matching tool result.
pub fn tool_result(id: &str, s: u32) -> String {
    tool_result_at(id, &stamp(s))
}
/// A thinking block.
pub fn thinking(t: &str, s: u32) -> String {
    thinking_at(t, &stamp(s))
}

pub fn user_at(t: &str, ts: &str) -> String {
    format!(
        "{{\"type\":\"user\",\"cwd\":\"/r\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"{t}\"}}]}},\"timestamp\":\"{ts}\"}}\n"
    )
}
/// A prompt the user submitted while the agent was busy (`queue-operation` / `enqueue`), not
/// yet picked up — both pages render it as the in-flight "queued" marker with its text.
pub fn queued_at(t: &str, ts: &str) -> String {
    format!("{{\"type\":\"queue-operation\",\"operation\":\"enqueue\",\"timestamp\":\"{ts}\",\"content\":\"{t}\"}}\n")
}
/// A context compaction (Claude's `system` / `compact_boundary` record), as the client writes it.
pub fn compaction_at(ts: &str) -> String {
    format!("{{\"type\":\"system\",\"subtype\":\"compact_boundary\",\"timestamp\":\"{ts}\",\"content\":\"Conversation compacted\",\"compactMetadata\":{{\"trigger\":\"auto\",\"preTokens\":594718,\"postTokens\":8617}}}}\n")
}
/// A one-pixel PNG, base64 — enough for a browser to decode to real dimensions.
pub const TINY_PNG_B64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";
/// A tool result carrying an embedded image (what a Read of a PNG records).
pub fn image_result_at(call_id: &str, ts: &str) -> String {
    format!(
        "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"{call_id}\",\"content\":[{{\"type\":\"image\",\"source\":{{\"type\":\"base64\",\"media_type\":\"image/png\",\"data\":\"{TINY_PNG_B64}\"}}}}]}}]}},\"timestamp\":\"{ts}\"}}\n"
    )
}
/// An artifact publish, as Claude Code records it: the `Artifact` call and its
/// "Published … at <url>" result.
pub fn artifact_publish_at(
    call_id: &str,
    path: &str,
    title: &str,
    icon: &str,
    url: &str,
    ts: &str,
) -> String {
    format!(
        "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"{call_id}\",\"name\":\"Artifact\",\"input\":{{\"file_path\":\"{path}\",\"title\":\"{title}\",\"favicon\":\"{icon}\",\"description\":\"a page\"}}}}]}},\"timestamp\":\"{ts}\"}}\n{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"{call_id}\",\"content\":\"Published {path} at {url}\"}}]}},\"timestamp\":\"{ts}\"}}\n"
    )
}
pub fn assistant_at(t: &str, ts: &str) -> String {
    format!(
        "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"{t}\"}}],\"usage\":{{\"input_tokens\":10,\"output_tokens\":20}}}},\"timestamp\":\"{ts}\"}}\n"
    )
}
pub fn tool_open_at(id: &str, ts: &str) -> String {
    format!(
        "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"{id}\",\"name\":\"Bash\",\"input\":{{\"command\":\"echo {id}\"}}}}]}},\"timestamp\":\"{ts}\"}}\n"
    )
}
/// A `Workflow` call and its result (#38/#119): the result text carries `Transcript dir:`, whose
/// trailing component IS the run id — the only place a transcript names the fleet it launched.
pub fn workflow_call_at(id: &str, run: &str, ts: &str) -> String {
    format!(
        "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"{id}\",\"name\":\"Workflow\",\"input\":{{\"script\":\"export const meta = {{ name: 'fan-out' }}\"}}}}]}},\"timestamp\":\"{ts}\"}}\n{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"{id}\",\"content\":\"Workflow launched in background. Task ID: t-{run}\\nTranscript dir: /w/.claude/runs/{run}\\n\"}}]}},\"timestamp\":\"{ts}\"}}\n"
    )
}

/// A tool call by NAME — for a session that used many different tools, which is what fills the
/// filter's tool-type list (#139).
pub fn named_tool_at(id: &str, name: &str, target: &str, ts: &str) -> String {
    format!(
        "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"{id}\",\"name\":\"{name}\",\"input\":{{\"command\":\"{target}\",\"file_path\":\"{target}\",\"pattern\":\"{target}\"}}}}]}},\"timestamp\":\"{ts}\"}}\n{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"{id}\",\"content\":\"ok\"}}]}},\"timestamp\":\"{ts}\"}}\n"
    )
}

/// A file-acting tool call (`Read` on `path`): the page offers the path with its stamps.
pub fn read_tool_at(id: &str, path: &str, ts: &str) -> String {
    format!(
        "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"{id}\",\"name\":\"Read\",\"input\":{{\"file_path\":\"{path}\"}}}}]}},\"timestamp\":\"{ts}\"}}\n"
    )
}
/// A tool result of `n` numbered lines ("line 1" … "line n") — long enough to be capped.
pub fn tool_result_lines(call_id: &str, n: usize, ts: &str) -> String {
    let body: String = (1..=n).map(|k| format!("line {k}\\n")).collect();
    format!(
        "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"{call_id}\",\"content\":\"{body}\"}}]}},\"timestamp\":\"{ts}\"}}\n"
    )
}
/// A slash command as Claude Code records typed input: a plain STRING content (the adapter
/// classifies commands on that path, not on text-block arrays), with its args and its local
/// stdout inline (a standalone stdout message's attachment to the command is #124's question).
pub fn command_at(name: &str, args: &str, stdout: &str, ts: &str) -> String {
    // TWO messages, as Claude Code really records a slash command: the command itself, then its
    // output as a standalone `<local-command-stdout>` user message (#124). The fold attaches the
    // second to the first, so the pair is ONE turn — which is the thing worth testing.
    let call = format!(
        "{{\"type\":\"user\",\"cwd\":\"/r\",\"message\":{{\"role\":\"user\",\"content\":\"<command-message>{name}</command-message>\\n<command-name>/{name}</command-name>\\n<command-args>{args}</command-args>\"}},\"timestamp\":\"{ts}\"}}\n"
    );
    if stdout.is_empty() {
        return call;
    }
    call + &format!(
        "{{\"type\":\"user\",\"cwd\":\"/r\",\"message\":{{\"role\":\"user\",\"content\":\"<local-command-stdout>{stdout}</local-command-stdout>\"}},\"timestamp\":\"{ts}\"}}\n"
    )
}
/// A tool result carrying the given text (already JSON-escaped: `\\n` for a newline).
pub fn tool_result_text(call_id: &str, text: &str, ts: &str) -> String {
    format!(
        "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"{call_id}\",\"content\":\"{text}\"}}]}},\"timestamp\":\"{ts}\"}}\n"
    )
}
pub fn tool_result_at(id: &str, ts: &str) -> String {
    format!(
        "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"{id}\",\"content\":\"out line\\nout line\\nout line\\n\"}}]}},\"timestamp\":\"{ts}\"}}\n"
    )
}
pub fn thinking_at(t: &str, ts: &str) -> String {
    format!(
        "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"thinking\",\"thinking\":\"{t}\"}}]}},\"timestamp\":\"{ts}\"}}\n"
    )
}

/// An `Edit` call on `path` — the tool the pages DISPLAY as "Update", which is why a scope that
/// selects edits must be read from the record's kind and not from what the head is called.
pub fn edit_tool_at(id: &str, path: &str, ts: &str) -> String {
    format!(
        "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"{id}\",\"name\":\"Edit\",\"input\":{{\"file_path\":\"{path}\",\"old_string\":\"before\",\"new_string\":\"after\"}}}}]}},\"timestamp\":\"{ts}\"}}\n"
    )
}

/// An agent asking the reader a question through its own client (#121): the `request_user_input`
/// call the server projects into `head.interaction`. Unanswered on its own; pair it with
/// `input_request_answer` for the resolved card.
pub fn input_request_at(id: &str, question: &str, ts: &str) -> String {
    format!(
        "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"{id}\",\"name\":\"request_user_input\",\"input\":{{\"question\":\"{question}\"}}}}]}},\"timestamp\":\"{ts}\"}}\n"
    )
}

/// The answer that came back through the agent's client, as the tool's own output.
pub fn input_request_answer(id: &str, field: &str, label: &str, ts: &str) -> String {
    format!(
        "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"{id}\",\"content\":\"{{\\\"answers\\\":{{\\\"{field}\\\":{{\\\"answers\\\":[\\\"{label}\\\"]}}}}}}\"}}]}},\"timestamp\":\"{ts}\"}}\n"
    )
}

/// A sub-agent spawn: the `Agent` tool call the parent makes (the spawn chip).
pub fn agent_spawn(call_id: &str, subagent_type: &str, s: u32) -> String {
    format!(
        "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"{call_id}\",\"name\":\"Agent\",\"input\":{{\"subagent_type\":\"{subagent_type}\",\"description\":\"look around\",\"prompt\":\"look around\"}}}}]}},\"timestamp\":\"{}\"}}\n",
        stamp(s)
    )
}
/// The spawn's result, naming the child `agent_id` whose transcript lives at
/// `<sid>/subagents/agent-<agent_id>.jsonl` — what links a parent to its child.
pub fn agent_result(call_id: &str, agent_id: &str, subagent_type: &str, s: u32) -> String {
    format!(
        "{{\"type\":\"user\",\"toolUseResult\":{{\"kind\":\"agent-result\",\"agentId\":\"{agent_id}\",\"agentType\":\"{subagent_type}\",\"content\":\"done\"}},\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"{call_id}\",\"content\":\"done\"}}]}},\"timestamp\":\"{}\"}}\n",
        stamp(s)
    )
}

/// An ISO timestamp `secs_ago` seconds before now — for records that must read as live.
pub fn now_minus(secs_ago: u64) -> String {
    let t = std::time::SystemTime::now() - Duration::from_secs(secs_ago);
    let secs = t.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    // Civil time from the epoch, UTC — enough for a timestamp the parsers accept.
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, mo, d) = civil_from_days(days as i64);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Append to a transcript and flush — a live tail, as an agent writes it.
pub fn append(path: &Path, s: &str) {
    let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    f.write_all(s.as_bytes()).unwrap();
    f.flush().unwrap();
}

/// A long session: `turns` user/assistant pairs, every `shape.tool_every`th turn carrying a
/// tool call + result (a fold header the keys walk), every `shape.think_every`th a thinking
/// block. Long enough to scroll on every surface from a few dozen turns.
#[derive(Clone, Copy)]
pub struct Shape {
    pub tool_every: u32,
    pub think_every: u32,
    pub prose_repeat: usize,
}
impl Default for Shape {
    fn default() -> Self {
        Shape {
            tool_every: 3,
            think_every: 5,
            prose_repeat: 6,
        }
    }
}
pub fn long_session(turns: u32, shape: Shape) -> String {
    let mut out = String::new();
    for i in 0..turns {
        out += &user(
            &format!(
                "question {i}: {}",
                "lorem ipsum dolor sit amet, consectetur. ".repeat(shape.prose_repeat / 2 + 1)
            ),
            i,
        );
        if shape.think_every > 0 && i % shape.think_every == 0 {
            out += &thinking(
                &format!(
                    "deliberation {i}: {}",
                    "weighing the options carefully. ".repeat(shape.prose_repeat)
                ),
                i,
            );
        }
        if shape.tool_every > 0 && i % shape.tool_every == 0 {
            out += &tool_open(&format!("t{i}"), i);
            out += &tool_result(&format!("t{i}"), i);
        }
        out += &assistant(
            &format!(
                "answer {i}: {}",
                "sed do eiusmod tempor incididunt ut labore. ".repeat(shape.prose_repeat)
            ),
            i,
        );
    }
    out
}

// ── stores ──────────────────────────────────────────────────────────────────────────────────

/// A hermetic world of agent stores under a case's scratch root. Every store env var the
/// monitors and the adapters honour points into it, so a monitor under test sees ONLY what
/// the case wrote (the same knobs claude-monitor's index tests use).
pub struct Stores {
    pub root: PathBuf,
}

impl Stores {
    pub fn new(base: &Path) -> Stores {
        let root = base.join("stores");
        for d in ["claude", "qoderwork", "qoder", "codex"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        Stores { root }
    }

    /// The env a monitor (or an adapter) needs to see only these stores.
    pub fn envs(&self) -> Vec<(&'static str, PathBuf)> {
        vec![
            ("CLAUDE_PROJECTS_DIR", self.root.join("claude")),
            ("QODERWORK_PROJECTS_DIR", self.root.join("qoderwork")),
            ("QODER_PROJECTS_DIR", self.root.join("qoder")),
            ("CODEX_HOME", self.root.join("codex")),
            ("CLAUDE_JDI_TASKS_ROOT", self.root.join("claude-tasks")),
        ]
    }

    /// A Claude session `sid` under the project slug `-r` (the builders' cwd), with `jsonl`.
    /// Returns the transcript path — a live-growth scenario appends to it.
    /// A Claude session's LIVE task store (`<tasks root>/<sid>/<n>.json`, one file per task, the
    /// shape Claude Code writes), filed in the order given — which is the order the pane must
    /// NOT rely on.
    pub fn claude_tasks(&self, sid: &str, tasks: &[(&str, &str, &str)]) -> PathBuf {
        let dir = self.root.join("claude-tasks").join(sid);
        std::fs::create_dir_all(&dir).unwrap();
        for (n, (id, subject, status)) in tasks.iter().enumerate() {
            let json = format!(
                "{{\"id\":\"{id}\",\"subject\":\"{subject}\",\"description\":\"{subject}\",\"activeForm\":\"{subject}\",\"status\":\"{status}\",\"blocks\":[],\"blockedBy\":[]}}"
            );
            std::fs::write(dir.join(format!("{}.json", n + 1)), json).unwrap();
        }
        dir
    }
    /// A task file with the LIFE a queue records (#125): who holds it, when it moved, what it
    /// asked for, what came of it, and the worklog written along the way.
    pub fn claude_task_file(&self, sid: &str, n: usize, json: &str) -> PathBuf {
        let dir = self.root.join("claude-tasks").join(sid);
        std::fs::create_dir_all(&dir).unwrap();
        serde_json::from_str::<serde_json::Value>(json).expect("a task file is JSON");
        let path = dir.join(format!("{n}.json"));
        std::fs::write(&path, json).unwrap();
        path
    }

    /// One workflow run's journal beside a Claude session — `<session>/subagents/workflows/
    /// <run>/journal.jsonl`, the file the roster is read from. Each member is `(id, result)`:
    /// a member with a result is finished and titled by its first line, one without is still
    /// running.
    pub fn claude_workflow_run(&self, sid: &str, run: &str, members: &[(&str, &str)]) -> PathBuf {
        let dir = self
            .root
            .join("claude")
            .join("-r")
            .join(sid)
            .join("subagents")
            .join("workflows")
            .join(run);
        std::fs::create_dir_all(&dir).unwrap();
        let mut journal = String::new();
        for (id, _) in members {
            journal += &format!("{{\"type\":\"started\",\"agentId\":\"{id}\"}}\n");
        }
        for (id, result) in members {
            if !result.is_empty() {
                journal += &format!(
                    "{{\"type\":\"result\",\"agentId\":\"{id}\",\"result\":\"{result}\"}}\n"
                );
            }
        }
        let path = dir.join("journal.jsonl");
        std::fs::write(&path, journal).unwrap();
        path
    }

    pub fn claude_session(&self, sid: &str, jsonl: &str) -> PathBuf {
        let proj = self.root.join("claude").join("-r");
        std::fs::create_dir_all(&proj).unwrap();
        let path = proj.join(format!("{sid}.jsonl"));
        std::fs::write(&path, jsonl).unwrap();
        path
    }

    /// A Codex rollout `id` under `codex/sessions/2026/08/21/rollout-<id>.jsonl` (the store
    /// `CODEX_HOME` points at). Every line must parse: a malformed fixture line is skipped by
    /// the adapter in silence, and the case would then chase a missing head.
    pub fn codex_session(&self, id: &str, jsonl: &str) -> PathBuf {
        for line in jsonl.lines() {
            serde_json::from_str::<serde_json::Value>(line)
                .unwrap_or_else(|e| panic!("fixture line {line}: {e}"));
        }
        let dir = self.root.join("codex/sessions/2026/08/21");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("rollout-{id}.jsonl"));
        std::fs::write(&path, jsonl).unwrap();
        path
    }

    /// A sub-agent transcript of `parent_sid`: `<slug>/<sid>/subagents/agent-<agent>.jsonl`.
    /// Lineage is the PATH alone (the adapter reads no file to know the parent).
    pub fn claude_child(&self, parent_sid: &str, agent: &str, jsonl: &str) -> PathBuf {
        let dir = self
            .root
            .join("claude")
            .join("-r")
            .join(parent_sid)
            .join("subagents");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("agent-{agent}.jsonl"));
        std::fs::write(&path, jsonl).unwrap();
        path
    }

    /// A QoderWork fork FAMILY (#142): a root session and one forked from it, related through
    /// the `<sid>-session.json` sidecar's `fork_from`, both past the adapter's junk-size gate.
    /// Returns `(root_id, fork_id)`.
    pub fn qoderwork_family(&self) -> (&'static str, &'static str) {
        let qw = self.root.join("qoderwork").join("-r");
        std::fs::create_dir_all(&qw).unwrap();
        let root_id = "aaaaaaaa-0000-4000-8000-000000000001";
        let fork_id = "aaaaaaaa-0000-4000-8000-000000000002";
        let transcript = |salt: &str| -> String {
            let mut out = String::new();
            for i in 0..30u32 {
                out += &user(
                    &format!("prompt {salt} {i} — a line long enough to matter"),
                    i,
                );
                out += &assistant(
                    &format!("reply {salt} {i} — a line long enough to matter"),
                    i,
                );
            }
            assert!(
                out.len() > 4096,
                "past QoderWork's MIN_TRANSCRIPT_BYTES gate"
            );
            out
        };
        std::fs::write(qw.join(format!("{root_id}.jsonl")), transcript("root")).unwrap();
        std::fs::write(
            qw.join(format!("{root_id}-session.json")),
            r#"{"title":"Family root","updated_at":1756800000000}"#,
        )
        .unwrap();
        std::fs::write(qw.join(format!("{fork_id}.jsonl")), transcript("fork")).unwrap();
        std::fs::write(
            qw.join(format!("{fork_id}-session.json")),
            format!(r#"{{"title":"Family root (Fork)","fork_from":"{root_id}","updated_at":1756800100000}}"#),
        )
        .unwrap();
        (root_id, fork_id)
    }

    /// One FINISHED Claude session (old timestamps, no process) — the shape the compose
    /// affordance resumes. Returns its id.
    pub fn claude_finished(&self) -> &'static str {
        let sid = "bbbbbbbb-0000-4000-8000-000000000001";
        let mut out = String::new();
        for i in 0..12u32 {
            out += &user(&format!("prompt {i} — a line long enough to matter"), i);
            out += &assistant(&format!("reply {i} — a line long enough to matter"), i);
        }
        self.claude_session(sid, &out);
        sid
    }
}

// ── the monitor under test ──────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// `agent-monitor` — the v1 binary: its classic page is the rail.
    V1,
    /// `agent-monitor-v2` — its classic page is the splice shell.
    V2,
}

impl Kind {
    fn binary(self) -> &'static str {
        match self {
            Kind::V1 => "agent-monitor",
            Kind::V2 => "agent-monitor-v2",
        }
    }
    fn package(self) -> &'static str {
        match self {
            Kind::V1 => "claude-monitor",
            Kind::V2 => "claude-monitor-v2",
        }
    }
}

/// A monitor binary running on a fixed loopback port over a scratch state dir, reaped on
/// drop. Missing binary → a PANIC naming the build, never a silent skip: a skipped case
/// reads as green, and a blank shell has passed as 13/16 that way (#53).
pub struct Monitor {
    pub kind: Kind,
    pub port: u16,
    pub state: PathBuf,
    child: Reap,
}

impl Monitor {
    pub fn spawn(
        kind: Kind,
        port: u16,
        base: &Path,
        stores: Option<&Stores>,
        paired: bool,
    ) -> Monitor {
        let bin = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("target/release")
            .join(kind.binary());
        assert!(
            bin.is_file(),
            "{} is not built — run `cargo build --release -p {}` first",
            bin.display(),
            kind.package()
        );
        let state = base.join(format!("state-{port}"));
        std::fs::create_dir_all(&state).unwrap();
        let mut cmd = std::process::Command::new(&bin);
        cmd.args(["--port", &port.to_string()])
            .env("XDG_CACHE_HOME", base)
            .env("CLAUDE_MONITOR_CACHE", base.join(format!("cache-{port}")))
            .env("CLAUDE_MONITOR_STATE", &state)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if paired {
            cmd.arg("--pair");
        }
        if kind == Kind::V1 {
            cmd.arg("--no-open");
        }
        if let Some(stores) = stores {
            for (k, v) in stores.envs() {
                cmd.env(k, v);
            }
        }
        let child = Reap(
            cmd.spawn()
                .unwrap_or_else(|e| panic!("{} starts: {e}", kind.binary())),
        );
        std::thread::sleep(Duration::from_millis(1500));
        Monitor {
            kind,
            port,
            state,
            child,
        }
    }

    pub fn url(&self, path_and_query: &str) -> String {
        format!(
            "http://127.0.0.1:{}/{}",
            self.port,
            path_and_query.trim_start_matches('/')
        )
    }

    /// The token `--pair` minted, or `None` when unpaired.
    pub fn token(&self) -> Option<String> {
        std::fs::read_to_string(self.state.join("auth-token"))
            .ok()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
    }

    /// The first navigation of every tab: `?token=` sets the cookie and redirects to a bare
    /// `/`, dropping every other query — so pair first, then ask for a page.
    pub fn pair(&self, tab: &headless_chrome::Tab) {
        let token = self
            .token()
            .map(|t| format!("?token={t}"))
            .unwrap_or_default();
        tab.navigate_to(&self.url(&token)).unwrap();
        tab.wait_until_navigated().unwrap();
    }

    /// Navigate the tab to a page of this monitor and wait for the navigation.
    pub fn open(&self, tab: &headless_chrome::Tab, path_and_query: &str) {
        tab.navigate_to(&self.url(path_and_query)).unwrap();
        tab.wait_until_navigated().unwrap();
    }
}

// ── chrome, actions, probes ─────────────────────────────────────────────────────────────────

/// Headless Chrome with timer throttling off — a throttled background tab misses polls and
/// reads exactly like a positioning bug.
/// The browser this suite drives — NEVER the developer's own Google Chrome (#u21).
///
/// `headless_chrome` with no `path` calls its `default_executable()`, which on macOS falls
/// through to `/Applications/Google Chrome.app`: the same bundle id (`com.google.Chrome`) the
/// developer browses with. macOS registers one app per bundle id, so launching a second
/// instance reconciles to the one application — it can take the developer's window — and mints
/// a copy-on-write clone of the whole bundle under `$TMPDIR/../X/com.google.Chrome.code_sign_clone/`
/// that nothing reaps. One clone per launch, one launch per case: measured at 4,405 clones on
/// this machine, 481 of them in three hours of running these suites.
///
/// So the path is explicit, and any browser whose bundle id differs will do: `CLAUDE_REPLAY_CHROME`
/// when set, else a Chrome for Testing (`com.google.chrome.for.testing`) where one is installed.
/// Neither present, the crate's own detection stands — CI has its own Chrome and no developer
/// window to lose — so this is a local hygiene rule, not a new dependency.
fn browser_path() -> Option<std::path::PathBuf> {
    if let Some(explicit) = std::env::var_os("CLAUDE_REPLAY_CHROME") {
        let path = std::path::PathBuf::from(explicit);
        assert!(
            path.exists(),
            "CLAUDE_REPLAY_CHROME points at {path:?}, which does not exist"
        );
        return Some(path);
    }
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from)?;
    let testing = |dir: std::path::PathBuf| {
        let binary =
            dir.join("Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing");
        binary.exists().then_some(binary)
    };
    if let Some(found) = testing(std::path::PathBuf::from("/Applications")) {
        return Some(found);
    }
    // Playwright keeps one under its cache; the newest install wins.
    let cache = home.join("Library/Caches/ms-playwright");
    let mut candidates: Vec<_> = std::fs::read_dir(&cache)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("chromium"))
        })
        .collect();
    candidates.sort();
    candidates.into_iter().rev().find_map(|dir| {
        testing(dir.join("chrome-mac-arm64")).or_else(|| testing(dir.join("chrome-mac")))
    })
}

pub fn chrome() -> headless_chrome::Browser {
    headless_chrome::Browser::new(
        headless_chrome::LaunchOptions::default_builder()
            .path(browser_path())
            .headless(true)
            .window_size(Some((1400, 900)))
            .args(vec![
                std::ffi::OsStr::new("--disable-background-timer-throttling"),
                std::ffi::OsStr::new("--disable-backgrounding-occluded-windows"),
                std::ffi::OsStr::new("--disable-renderer-backgrounding"),
            ])
            .build()
            .unwrap(),
    )
    .expect("chrome launches")
}

/// A JS expression's PRIMITIVE result (string, number, bool) — objects come back Null; use
/// [`probe`] for those. A promise is awaited.
pub fn eval(tab: &headless_chrome::Tab, js: &str) -> serde_json::Value {
    tab.evaluate(js, true)
        .ok()
        .and_then(|r| r.value)
        .unwrap_or(serde_json::Value::Null)
}

/// A JS expression's result as JSON — the way an OBJECT crosses the CDP boundary by value.
pub fn probe(tab: &headless_chrome::Tab, js: &str) -> serde_json::Value {
    serde_json::from_str(
        eval(tab, &format!("JSON.stringify({js})"))
            .as_str()
            .unwrap_or("null"),
    )
    .unwrap_or(serde_json::Value::Null)
}

/// Poll a boolean JS predicate until true, or PANIC with `what` and a diagnostic — never a
/// vacuous return. `diag` is a JS expression evaluated on timeout (a string).
pub fn until(tab: &headless_chrome::Tab, js: &str, what: &str, timeout: Duration, diag: &str) {
    let t0 = Instant::now();
    while t0.elapsed() < timeout {
        if eval(tab, js).as_bool() == Some(true) {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let seen = eval(tab, diag);
    panic!("timed out waiting for {what}; seen: {seen}");
}

/// A key press on the document (the app shell's and the classic page's handlers both listen
/// there); `shift` for the ⇧ variants.
pub fn key(tab: &headless_chrome::Tab, k: &str, shift: bool) {
    eval(tab, &format!("document.dispatchEvent(new KeyboardEvent('keydown', {{key: {k:?}, shiftKey: {shift}, bubbles: true, cancelable: true}})); 'ok'"));
}

/// The reader's intent, then a scroll: a programmatic scroll alone reads as the renderer's
/// own and the follow logic re-pins the tail; a wheel event first is what unpins.
pub fn wheel_scroll(tab: &headless_chrome::Tab, scroller: &str, to: &str) {
    eval(tab, &format!("(function(){{ var s = {scroller}; s.dispatchEvent(new WheelEvent('wheel', {{deltaY: -1, bubbles: true}})); {to}; return 'ok'; }})()"));
}

/// The app shell's transcript scroller.
pub const APP_SCROLLER: &str = "document.querySelector('.transcript')";

/// The app shell: the `data-unit-from` of the first mounted unit at (or within a line above)
/// the viewport top — the unit the reader is "at".
pub fn app_unit_index(tab: &headless_chrome::Tab) -> i64 {
    eval(tab, "(function(){ var s=document.querySelector('.transcript'); if (!s) return -1; var top=s.getBoundingClientRect().top; for (var c of document.querySelector('.virtual-window').children) { if (c.getBoundingClientRect().top >= top - 24) return Number(c.dataset.unitFrom); } return -1; })()")
        .as_i64()
        .unwrap_or(-1)
}

/// The app shell: whether the transcript sits at its tail (within 2px).
pub fn app_at_tail(tab: &headless_chrome::Tab) -> bool {
    eval(tab, "(function(){ var s=document.querySelector('.transcript'); if (!s || !document.querySelector('.virtual-window').children.length) return false; return s.scrollHeight - s.clientHeight - s.scrollTop <= 2; })()")
        .as_bool()
        .unwrap_or(false)
}

/// The classic page (the html server's, or a monitor's classic view): the viewport's state —
/// scrollY, document height, the gap to the bottom, whether it follows, the pill's text.
pub fn classic_view_state(tab: &headless_chrome::Tab) -> serde_json::Value {
    probe(
        tab,
        r#"({
            y: Math.round(window.scrollY),
            h: document.body.scrollHeight,
            gap: Math.round(document.body.scrollHeight - window.innerHeight - window.scrollY),
            following: document.body.classList.contains("following"),
            badge: (document.getElementById("newbadge") || {}).textContent || "",
            badgeOn: /\bon\b/.test((document.getElementById("newbadge") || {className:""}).className),
            blocks: (document.getElementById("stream") || {childElementCount:-1}).childElementCount
        })"#,
    )
}

// ── the two surfaces, one vocabulary ────────────────────────────────────────────────────────
// A scenario is written once and run against both pages. The classic page (the html server's
// `export.js`, the reference) scrolls the DOCUMENT and marks turns with `data-turn` on the
// turn card; the app shell scrolls `.transcript`, virtualizes units and marks user turns with
// `data-turn` on `.turn.user`. The probes below speak in USER-TURN ORDINALS, which both name,
// never in pixels or DOM indexes.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Surface {
    /// `export.js` on the html server: the reference.
    Classic,
    /// The monitor's app shell (`?ui=app`).
    AppShell,
}

impl Surface {
    /// The scroller's JS expression.
    pub fn scroller(self) -> &'static str {
        match self {
            Surface::Classic => "document.scrollingElement",
            Surface::AppShell => "document.querySelector('.transcript')",
        }
    }
    /// The element whose children carry the turns.
    fn turns_root(self) -> &'static str {
        match self {
            Surface::Classic => "document.getElementById('stream')",
            Surface::AppShell => "document.querySelector('.virtual-window')",
        }
    }
    /// The fold headers a reader opens: the classic page's `.fold-h`, the app shell's
    /// interactive `button.renderer-head`.
    pub fn fold_head(self) -> &'static str {
        match self {
            Surface::Classic => ".fold-h",
            Surface::AppShell => "button.renderer-head",
        }
    }
}

/// The user-turn ordinal at the viewport top: the first `[data-turn]` element at (or within a
/// line above) the top edge of the scroller. -1 when nothing is mounted there yet.
pub fn turn_at_top(tab: &headless_chrome::Tab, surface: Surface) -> i64 {
    let (scroller, root) = (surface.scroller(), surface.turns_root());
    let top = match surface {
        Surface::Classic => "0".to_string(),
        Surface::AppShell => format!("{scroller}.getBoundingClientRect().top"),
    };
    eval(tab, &format!("(function(){{ var root = {root}; if (!root) return -1; var top = {top}; var els = root.querySelectorAll('[data-turn]'); for (var e of els) {{ if (e.getBoundingClientRect().top >= top - 24) return Number(e.dataset.turn); }} return -1; }})()"))
        .as_i64()
        .unwrap_or(-1)
}

/// Whether the scroller sits at its tail (within 2px).
pub fn at_tail(tab: &headless_chrome::Tab, surface: Surface) -> bool {
    let s = surface.scroller();
    eval(tab, &format!("(function(){{ var s = {s}; if (!s) return false; return s.scrollHeight - s.clientHeight - s.scrollTop <= 2; }})()"))
        .as_bool()
        .unwrap_or(false)
}

/// The reader's scroll: a wheel event first (intent — a bare programmatic scroll reads as the
/// renderer's own and the follow logic re-pins), then a scroll by `dy` pixels.
pub fn scroll_by(tab: &headless_chrome::Tab, surface: Surface, dy: i64) {
    let s = surface.scroller();
    let target = match surface {
        Surface::Classic => "window",
        Surface::AppShell => s,
    };
    // The reader's intent, then the scroll: a programmatic scroll alone reads as the renderer's
    // own, and a page that is following heals it straight back to the tail.
    //
    // Dispatched TWICE, around the move, and re-applied once if the position did not stick. The
    // page classifies in its SCROLL handler, which is asynchronous: if that handler lands more
    // than the intent window (300ms) after the wheel — a heavy page, a slower engine, a busy
    // machine — it reads the reader's own scroll as displacement and undoes it. One retry is
    // enough for a race and does not hide a page that genuinely refuses: a second refusal
    // leaves the position where the page put it, and the case fails as it should.
    // `s.scrollTop = y` on the DOCUMENT scroller fires no scroll event on every engine —
    // measured: Chrome for Testing 151 stays silent where stable 152 reports two — and a page
    // that classifies in its scroll handler then never hears the reader, so it heals the scroll
    // straight back. `scrollTo` is the same movement and is reported by both.
    let move_it = format!(
        "(function(){{ var s = {s}; var want = Math.max(0, s.scrollTop + ({dy})); {target}.dispatchEvent(new WheelEvent('wheel', {{deltaY: {dy}, bubbles: true}})); s.scrollTo({{ top: want, behavior: 'instant' }}); {target}.dispatchEvent(new WheelEvent('wheel', {{deltaY: {dy}, bubbles: true}})); return [want, s.scrollTop]; }})()"
    );
    let before = eval(tab, &format!("{s}.scrollTop")).as_f64().unwrap_or(0.0);
    let asked = probe(tab, &move_it);
    let want = asked
        .get(0)
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0);
    std::thread::sleep(Duration::from_millis(140));
    let now = eval(tab, &format!("{s}.scrollTop"))
        .as_f64()
        .unwrap_or(want);
    // Retry only an outright REFUSAL — the view came back to where it started although we asked
    // it to move. A page that merely lands somewhere else is CORRECTING (it held an anchor
    // through growth, it converged on the tail), and re-applying our number would fight the
    // very rule the case is watching.
    if (now - before).abs() <= 2.0 && (want - before).abs() > 2.0 {
        eval(
            tab,
            &format!("(function(){{ var s = {s}; {target}.dispatchEvent(new WheelEvent('wheel', {{deltaY: {dy}, bubbles: true}})); s.scrollTo({{ top: {want}, behavior: 'instant' }}); return 'ok'; }})()"),
        );
    }
}

/// Jump to the end the way the page offers it: the classic page's pill / a scroll to the
/// bottom with intent; the app shell's jump-to-bottom control.
pub fn jump_to_end(tab: &headless_chrome::Tab, surface: Surface) {
    let s = surface.scroller();
    match surface {
        Surface::Classic => {
            eval(tab, "(function(){ window.dispatchEvent(new WheelEvent('wheel', {deltaY: 120})); window.scrollTo(0, document.scrollingElement.scrollHeight); var b = document.getElementById('newbadge'); if (b) b.click(); return 'ok'; })()");
        }
        Surface::AppShell => {
            eval(tab, &format!("(function(){{ var b = document.getElementById('jumpToBottom'); if (b) b.click(); var s = {s}; s.scrollTo({{ top: s.scrollHeight, behavior: 'instant' }}); return 'ok'; }})()"));
        }
    }
}

/// The largest user-turn ordinal mounted right now — grows as the transcript grows.
pub fn last_mounted_turn(tab: &headless_chrome::Tab, surface: Surface) -> i64 {
    let root = surface.turns_root();
    eval(tab, &format!("(function(){{ var r = {root}; if (!r) return -1; var m = -1; r.querySelectorAll('[data-turn]').forEach(function(e){{ m = Math.max(m, Number(e.dataset.turn)); }}); return m; }})()"))
        .as_i64()
        .unwrap_or(-1)
}

/// Open the LAST fold header currently in the DOM (near the end after a jump), preferring a
/// THINKING block — the owner's sequence (#51) opens one — over any other fold. Returns the
/// turn it belongs to, -2 when the fold carries no turn of its own, -1 when none is mounted.
pub fn open_last_fold(tab: &headless_chrome::Tab, surface: Surface) -> i64 {
    let (thinking, any) = match surface {
        Surface::Classic => (".fold[data-kind=\"think\"] .fold-h", ".fold-h"),
        Surface::AppShell => (
            ".renderer-turn[data-kind=\"thinking\"] button.renderer-head",
            "button.renderer-head",
        ),
    };
    eval(tab, &format!("(function(){{ var hs = document.querySelectorAll('{thinking}'); if (!hs.length) hs = document.querySelectorAll('{any}'); if (!hs.length) return -1; var h = hs[hs.length - 1]; var t = h.closest('[data-turn]'); h.click(); return t ? Number(t.dataset.turn) : -2; }})()"))
        .as_i64()
        .unwrap_or(-1)
}

/// The number of user turns mounted right now (the app shell mounts a window; the classic
/// page mounts everything it has rendered).
pub fn mounted_turns(tab: &headless_chrome::Tab, surface: Surface) -> i64 {
    let root = surface.turns_root();
    eval(tab, &format!("(function(){{ var r = {root}; return r ? r.querySelectorAll('[data-turn]').length : -1; }})()"))
        .as_i64()
        .unwrap_or(-1)
}

// ── live growth ─────────────────────────────────────────────────────────────────────────────

/// A transcript growing while a page watches it: a thread appends the script's records one
/// per `interval`. The interval must exceed the slower consumer's poll (the app shell's
/// record store polls every 1 s; the classic page's tick is its POLL_MS) or assertions race
/// the apply. The thread stops on drop — drop the driver before the next case takes
/// [`serial`], or it appends into a store another case is measuring.
pub struct LiveGrowth {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    pub appended: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl LiveGrowth {
    pub fn start(path: PathBuf, script: Vec<String>, interval: Duration) -> LiveGrowth {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let appended = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (stop2, appended2) = (stop.clone(), appended.clone());
        let thread = std::thread::spawn(move || {
            for record in script {
                if stop2.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(interval);
                if stop2.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                append(&path, &record);
                appended2.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        });
        LiveGrowth {
            stop,
            thread: Some(thread),
            appended,
        }
    }

    /// How many records have been appended so far.
    pub fn count(&self) -> usize {
        self.appended.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Wait until the whole script has been appended (or `timeout`).
    pub fn finish(mut self, timeout: Duration) -> usize {
        let t0 = Instant::now();
        while self.thread.as_ref().is_some_and(|t| !t.is_finished()) && t0.elapsed() < timeout {
            std::thread::sleep(Duration::from_millis(100));
        }
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        self.count()
    }
}

impl Drop for LiveGrowth {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The turn the PANE says the reader is at: the classic page's sticky bar ("Turn N — …", the
/// scroll spy's verdict, mirrored by `.side-item.active`); the app shell's outline row marked
/// current, if it marks one (a leading number in its text). -1 when the pane names none.
pub fn pane_focus_turn(tab: &headless_chrome::Tab, surface: Surface) -> i64 {
    let js = match surface {
        Surface::Classic => "(function(){ var t = (document.getElementById('stickytext') || {}).textContent || ''; var m = t.match(/Turn (\\d+)/); if (m) return Number(m[1]); var a = document.querySelector('.side-item.active'); if (!a) return -1; var n = (a.textContent || '').match(/^(\\d+)/); return n ? Number(n[1]) : -1; })()",
        Surface::AppShell => "(function(){ var r = document.querySelector('#navigatorTurns .outline-turn-row.is-current, #navigatorTurns .outline-turn-row[aria-current=\"true\"], #navigatorTurns .outline-turn-row.current'); if (!r) return -1; if (r.dataset.turn) return Number(r.dataset.turn); var n = (r.textContent || '').match(/(\\d+)/); return n ? Number(n[1]) : -1; })()",
    };
    eval(tab, js).as_i64().unwrap_or(-1)
}

/// The turn bar's reading (#123): `None` while it is off, else the turn it names and its whole
/// text. Both surfaces mark a live bar with `on` and read `Turn N — <label>`; only the id
/// differs — the classic page's `#stickybar` and the app shell's `#turnStickyBar`.
pub fn sticky_turn(tab: &headless_chrome::Tab, surface: Surface) -> Option<(i64, String)> {
    let bar = match surface {
        Surface::Classic => "document.getElementById('stickybar')",
        Surface::AppShell => "document.getElementById('turnStickyBar')",
    };
    let text = eval(tab, &format!("(function(){{ var b = {bar}; if (!b || !b.classList.contains('on')) return ''; return b.innerText.replace(/\\s+/g, ' ').trim(); }})()"));
    let text = text.as_str().unwrap_or("").to_string();
    if text.is_empty() {
        return None;
    }
    let turn = text
        .split("Turn ")
        .nth(1)
        .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|digits| digits.parse::<i64>().ok())?;
    Some((turn, text))
}

/// Click the turn bar — the way a reader returns to the turn it names.
pub fn click_sticky_turn(tab: &headless_chrome::Tab, surface: Surface) {
    let bar = match surface {
        Surface::Classic => "document.getElementById('stickybar')",
        Surface::AppShell => "document.getElementById('turnStickyBar')",
    };
    eval(
        tab,
        &format!("(function(){{ {bar}.click(); return 'ok'; }})()"),
    );
}

/// Type a search query the way the page takes it (its own box), and return the hit count the
/// page reports ("N hits" / "N matches").
pub fn search(tab: &headless_chrome::Tab, surface: Surface, query: &str) -> i64 {
    let (input, count) = match surface {
        Surface::Classic => ("q", "qcount"),
        Surface::AppShell => ("transcriptSearchInput", "transcriptSearchCount"),
    };
    eval(tab, &format!("(function(){{ var i = document.getElementById('{input}'); i.focus(); i.value = {query:?}; i.dispatchEvent(new Event('input', {{bubbles: true}})); return 'ok'; }})()"));
    std::thread::sleep(Duration::from_millis(600));
    search_count(tab, surface, count)
}

/// The TOTAL the page reports: "N hits" / "N matches", or the total of a "k/N" navigation
/// display (the classic page shows the current hit's position once the reader steps).
fn search_count(tab: &headless_chrome::Tab, _surface: Surface, count_id: &str) -> i64 {
    eval(tab, &format!("(function(){{ var t = (document.getElementById('{count_id}') || {{}}).textContent || ''; var nav = t.match(/(\\d+)\\s*\\/\\s*(\\d+)/); if (nav) return Number(nav[2]); var m = t.match(/(\\d+)/); return m ? Number(m[1]) : -1; }})()"))
        .as_i64()
        .unwrap_or(-1)
}

/// The hit count the page reports right now.
pub fn search_hits(tab: &headless_chrome::Tab, surface: Surface) -> i64 {
    let count = match surface {
        Surface::Classic => "qcount",
        Surface::AppShell => "transcriptSearchCount",
    };
    search_count(tab, surface, count)
}

/// How many search highlights are mounted (`mark.hl` / `mark.search-mark`).
pub fn search_marks(tab: &headless_chrome::Tab, surface: Surface) -> i64 {
    let sel = match surface {
        Surface::Classic => "mark.hl",
        Surface::AppShell => "mark.search-mark",
    };
    eval(tab, &format!("document.querySelectorAll('{sel}').length"))
        .as_i64()
        .unwrap_or(-1)
}

/// Step to the next search hit the way the page offers it: Enter in the classic box, the
/// app shell's "next" control.
pub fn search_next(tab: &headless_chrome::Tab, surface: Surface) {
    match surface {
        Surface::Classic => {
            eval(tab, "(function(){ var q = document.getElementById('q'); q.focus(); q.dispatchEvent(new KeyboardEvent('keydown', {key: 'Enter', bubbles: true, cancelable: true})); return 'ok'; })()");
        }
        Surface::AppShell => {
            eval(tab, "(function(){ var b = document.getElementById('findNext'); if (b) b.click(); return 'ok'; })()");
        }
    }
    std::thread::sleep(Duration::from_millis(500));
}

/// Jump to user turn `n` the way the page offers it: a click on the pane's entry for it (the
/// classic sidebar's `.side-item` "N · …", the app shell's outline row "NN ·"). Returns
/// whether an entry was found.
pub fn jump_to_turn(tab: &headless_chrome::Tab, surface: Surface, n: u32) -> bool {
    let js = match surface {
        Surface::Classic => format!("(function(){{ var e = [...document.querySelectorAll('.side-item')].find(function(x){{ return (x.textContent || '').trim().startsWith('{n} ·'); }}); if (!e) return false; e.click(); return true; }})()"),
        Surface::AppShell => format!("(function(){{ var want = String({n}).padStart(2, '0') + ' ·'; var e = [...document.querySelectorAll('#navigatorTurns .outline-turn-row')].find(function(x){{ var num = x.querySelector('.outline-number'); return num && num.textContent.trim() === want; }}); if (!e) return false; e.click(); return true; }})()"),
    };
    eval(tab, &js).as_bool().unwrap_or(false)
}

/// Resize the window (the classic case's idiom): every measured height was taken at the old
/// width and is now a guess.
pub fn resize(tab: &headless_chrome::Tab, width: f64, height: f64) {
    tab.set_bounds(headless_chrome::types::Bounds::Normal {
        left: None,
        top: None,
        width: Some(width),
        height: Some(height),
    })
    .expect("resize");
}

/// The scroller's scrollTop, in pixels — for the assertions that must be exact.
pub fn scroll_top(tab: &headless_chrome::Tab, surface: Surface) -> f64 {
    let s = surface.scroller();
    eval(
        tab,
        &format!("(function(){{ var s = {s}; return s ? s.scrollTop : -1; }})()"),
    )
    .as_f64()
    .unwrap_or(-1.0)
}

/// A session whose LAST turn is long and still open: `head_turns` ordinary turns, then one user
/// turn followed by `tail_steps` tool call/result pairs with thinking between — the shape of a
/// working agent's tail, where a reader scrolled back a few screens is still inside the open
/// turn. Growth appended to it stays inside that turn (no new user turn).
pub fn long_open_turn_session(head_turns: u32, tail_steps: u32) -> String {
    let mut out = long_session(head_turns, Shape::default());
    out += &user(
        &format!(
            "the long last question: {}",
            "please do the whole thing. ".repeat(6)
        ),
        50,
    );
    for k in 0..tail_steps {
        out += &thinking(
            &format!(
                "step {k} deliberation: {}",
                "considering the next move carefully. ".repeat(10)
            ),
            50 + (k % 9),
        );
        out += &tool_open(&format!("tail{k}"), 50 + (k % 9));
        out += &tool_result(&format!("tail{k}"), 50 + (k % 9));
        out += &assistant(
            &format!(
                "step {k} note: {}",
                "what the result means and what comes next. ".repeat(8)
            ),
            50 + (k % 9),
        );
    }
    out
}

/// More of the same open turn: `steps` further tool call/result pairs with notes, stamped now.
pub fn open_turn_growth(from_step: u32, steps: u32) -> Vec<String> {
    let mut script = Vec::new();
    for k in from_step..from_step + steps {
        script.push(thinking_at(
            &format!(
                "late step {k} deliberation: {}",
                "still weighing it. ".repeat(10)
            ),
            &now_minus(40),
        ));
        script.push(tool_open_at(&format!("late{k}"), &now_minus(35)));
        script.push(tool_result_at(&format!("late{k}"), &now_minus(30)));
        script.push(assistant_at(
            &format!("late step {k} note: {}", "and on it goes. ".repeat(8)),
            &now_minus(25),
        ));
    }
    script
}

/// What the reader SEES: the first visible content element and its offset from the viewport
/// top — `{ key, top }`. A scroll offset moves legitimately whenever content above the viewport
/// changes height (an estimate replaced by a real height, a rewrite re-measured); the reader's
/// view has moved only when the same element sits at a different offset, or another element
/// took its place without the reader scrolling. Assert on this, never on scrollTop.
pub fn view_anchor(tab: &headless_chrome::Tab, surface: Surface) -> (String, f64) {
    let js = match surface {
        Surface::Classic => "(function(){ var els = document.querySelectorAll('#stream [data-idx]'); for (var e of els) { var r = e.getBoundingClientRect(); if (r.bottom > 0) return JSON.stringify({key: 'idx:' + e.dataset.idx, top: r.top}); } return JSON.stringify({key: '', top: 0}); })()",
        Surface::AppShell => "(function(){ var s = document.querySelector('.transcript'); var top = s.getBoundingClientRect().top; for (var c of document.querySelector('.virtual-window').children) { var r = c.getBoundingClientRect(); if (r.bottom > top + 1) return JSON.stringify({key: c.dataset.unitKey || '', top: r.top - top}); } return JSON.stringify({key: '', top: 0}); })()",
    };
    let v: serde_json::Value =
        serde_json::from_str(eval(tab, js).as_str().unwrap_or("{}")).unwrap_or_default();
    (
        v["key"].as_str().unwrap_or("").to_string(),
        v["top"].as_f64().unwrap_or(0.0),
    )
}

/// The "new messages" pill: how many it says, or -1 when it is not shown. The classic page's
/// `#newbadge` ("↓ N new messages" while on); the app shell's jump control once it widens.
pub fn new_messages_pill(tab: &headless_chrome::Tab, surface: Surface) -> i64 {
    let js = match surface {
        Surface::Classic => "(function(){ var b = document.getElementById('newbadge'); if (!b || !/\\bon\\b/.test(b.className)) return -1; var m = (b.textContent || '').match(/(\\d+) new message/); return m ? Number(m[1]) : 0; })()",
        Surface::AppShell => "(function(){ var b = document.getElementById('jumpToBottom'); if (!b || !b.classList.contains('show')) return -1; var m = (b.textContent || '').match(/(\\d+) new message/); return m ? Number(m[1]) : 0; })()",
    };
    eval(tab, js).as_i64().unwrap_or(-1)
}

/// Wait for the pill to reach `want`, or fail saying what it reached. A live-growth case reads
/// a number the page arrives at ASYNCHRONOUSLY — the driver appends on its own cadence, the
/// page folds and paints on its own — so a flat sleep and a single read is a coin toss on a
/// busy machine (#131). This waits for the answer and only then insists on it.
pub fn await_pill(tab: &headless_chrome::Tab, surface: Surface, want: i64, what: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut seen = new_messages_pill(tab, surface);
    while std::time::Instant::now() < deadline {
        if seen == want {
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
        seen = new_messages_pill(tab, surface);
    }
    panic!("{what}: the pill reached {seen}, not {want}");
}

/// Click the pill / jump control.
pub fn click_pill(tab: &headless_chrome::Tab, surface: Surface) {
    let id = match surface {
        Surface::Classic => "newbadge",
        Surface::AppShell => "jumpToBottom",
    };
    eval(tab, &format!("(function(){{ var b = document.getElementById('{id}'); if (b) b.click(); return 'ok'; }})()"));
}

/// The text of the in-flight "queued" marker, or an empty string when no marker is shown. The
/// classic page's `.qmarker .qmd`; the app shell's `.renderer-queue-text`.
pub fn queued_text(tab: &headless_chrome::Tab, surface: Surface) -> String {
    let js = match surface {
        Surface::Classic => "(function(){ var m = document.querySelectorAll('.qmarker .qmd'); return m.length ? (m[m.length-1].textContent || '').trim() : ''; })()",
        Surface::AppShell => "(function(){ var m = document.querySelectorAll('.renderer-queue-text'); return m.length ? (m[m.length-1].textContent || '').trim() : ''; })()",
    };
    eval(tab, js).as_str().unwrap_or("").to_string()
}

/// Where the page shows the session id: the classic page's `#sid` (the short form); the app
/// shell's title menu, which carries the full id (#83 dropped the chip). "" when none shows.
pub fn session_id_chip(tab: &headless_chrome::Tab, surface: Surface) -> String {
    let js = match surface {
        Surface::Classic => "(function(){ var e = document.getElementById('sid'); return e && !e.hidden ? (e.textContent || '').trim() : ''; })()",
        Surface::AppShell => "(function(){ var e = document.querySelector('[data-session-copy-value=\"id\"]'); return e ? (e.textContent || '').trim() : ''; })()",
    };
    eval(tab, js).as_str().unwrap_or("").to_string()
}

/// Replace the page's clipboard with a recorder, so a copy can be read back without the
/// permission a real clipboard needs; `copied_text` returns what was last written.
pub fn stub_clipboard(tab: &headless_chrome::Tab) {
    eval(tab, "window.__copied = null; Object.defineProperty(navigator, 'clipboard', { value: { writeText: function (t) { window.__copied = String(t); return Promise.resolve(); } }, configurable: true }); 'ok'");
}
pub fn copied_text(tab: &headless_chrome::Tab) -> String {
    eval(
        tab,
        "window.__copied == null ? '' : String(window.__copied)",
    )
    .as_str()
    .unwrap_or("")
    .to_string()
}

/// Copy the transcript path the way each page offers it: the classic page's `#sid` click; the
/// app shell's title menu item.
pub fn click_session_id(tab: &headless_chrome::Tab, surface: Surface) {
    let js = match surface {
        Surface::Classic => "(function(){ var e = document.getElementById('sid'); if (e) e.click(); return 'ok'; })()",
        Surface::AppShell => "(function(){ var e = document.querySelector('[data-copy-session=\"path\"]'); if (e) e.click(); return 'ok'; })()",
    };
    eval(tab, js);
}

/// The reader's view by POSITION: the record index of the first visible element (the classic
/// page's `data-idx`, the app shell's `data-unit-from`) and its offset from the viewport top.
/// Record indices survive a tail rewrite that re-emits the same positions with new block ids —
/// which element KEYS do not — so this is the metric for "did the view hold through a rewrite".
pub fn view_anchor_index(tab: &headless_chrome::Tab, surface: Surface) -> (i64, f64) {
    // On the app shell a unit may hold many records (a process); descend to the innermost row
    // holding the viewport top so the metric is a RECORD on both pages.
    let js = match surface {
        Surface::Classic => "(function(){ var els = document.querySelectorAll('#stream [data-idx]'); for (var e of els) { var r = e.getBoundingClientRect(); if (r.bottom > 1) return JSON.stringify({ i: Number(e.dataset.idx), top: r.top }); } return JSON.stringify({ i: -1, top: 0 }); })()",
        Surface::AppShell => "(function(){ var s = document.querySelector('.transcript'); var top = s.getBoundingClientRect().top; var hit = null; for (var e of document.querySelectorAll('.virtual-window > [data-unit-from]')) { var r = e.getBoundingClientRect(); if (r.bottom > top + 1) { hit = e; break; } } if (!hit) return JSON.stringify({ i: -1, top: 0 }); for (;;) { var inner = null; for (var c of hit.querySelectorAll('[data-block-index]')) { var rc = c.getBoundingClientRect(); if (rc.bottom > top + 1 && rc.height > 0) { inner = c; break; } } if (!inner) break; hit = inner; } var rh = hit.getBoundingClientRect(); return JSON.stringify({ i: Number(hit.dataset.blockIndex != null ? hit.dataset.blockIndex : hit.dataset.unitFrom), top: rh.top - top }); })()",
    };
    let v: serde_json::Value = serde_json::from_str(eval(tab, js).as_str().unwrap_or("{}"))
        .unwrap_or(serde_json::Value::Null);
    (
        v["i"].as_i64().unwrap_or(-1),
        v["top"].as_f64().unwrap_or(0.0),
    )
}

/// A session whose LAST turn is open and long: `turns` finished turns, then one prompt followed
/// by `tools` tool calls with results and no closing answer — the shape of an agent still at
/// work, whose records the server re-emits (new block ids, same positions) on every rewrite.
pub fn open_turn_session(turns: u32, tools: u32) -> String {
    let mut out = long_session(turns, Shape::default());
    let span = 30 * tools as u64 + 200;
    out += &user_at(
        "question open: keep working on the long task",
        &now_minus(span + 100),
    );
    // A word of progress before each call, as a working agent writes — a bare run of identical
    // calls would fold into one block on both pages.
    for k in 0..tools as u64 {
        out += &assistant_at(
            &format!("progress {k}: checking the next file"),
            &now_minus(span - k * 30),
        );
        out += &tool_open_at(&format!("open-{k}"), &now_minus(span - k * 30 - 10));
        out += &tool_result_at(&format!("open-{k}"), &now_minus(span - k * 30 - 20));
    }
    out
}

/// Chrome's console for a tab, from now on: log entries and uncaught exceptions, collected
/// across navigations (the listener stays on the tab). Read it before diagnosing a shell that
/// "timed out waiting for …" — a blank shell is usually one uncaught error at load.
pub fn tap_console(tab: &headless_chrome::Tab) -> std::sync::Arc<std::sync::Mutex<Vec<String>>> {
    use headless_chrome::protocol::cdp::types::Event;
    let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = lines.clone();
    let _ = tab.enable_log();
    let _ = tab.enable_runtime();
    let _ = tab.call_method(headless_chrome::protocol::cdp::Network::Enable {
        max_total_buffer_size: None,
        max_resource_buffer_size: None,
        max_post_data_size: None,
        enable_durable_messages: None,
        report_direct_socket_traffic: None,
    });
    tab.add_event_listener(std::sync::Arc::new(move |event: &Event| {
        let text = match event {
            Event::LogEntryAdded(e) => Some(format!(
                "{:?}: {}",
                e.params.entry.level, e.params.entry.text
            )),
            Event::RuntimeExceptionThrown(e) => Some(format!(
                "EXCEPTION: {}",
                e.params
                    .exception_details
                    .exception
                    .as_ref()
                    .and_then(|x| x.description.clone())
                    .unwrap_or_else(|| e.params.exception_details.text.clone())
            )),
            Event::NetworkResponseReceived(e) if e.params.response.status >= 400 => Some(format!(
                "HTTP {} {}",
                e.params.response.status, e.params.response.url
            )),
            Event::RuntimeConsoleAPICalled(e) => Some(format!(
                "console: {}",
                e.params
                    .args
                    .iter()
                    .filter_map(|a| a.value.as_ref().map(|v| v.to_string()))
                    .collect::<Vec<_>>()
                    .join(" ")
            )),
            _ => None,
        };
        if let Some(text) = text {
            sink.lock().unwrap().push(text);
        }
    }))
    .unwrap();
    lines
}

/// A real drag selection: the pointer goes down at (x1, y1), moves to (x2, y2) with the button
/// held, and comes up — what a reader does to copy a message. Synthetic Range selections
/// ignore `user-select`, which is exactly the rule a copy case has to exercise.
pub fn drag_select(tab: &headless_chrome::Tab, x1: f64, y1: f64, x2: f64, y2: f64) {
    use headless_chrome::protocol::cdp::Input::{
        DispatchMouseEvent, DispatchMouseEventTypeOption, MouseButton,
    };
    let event =
        |kind: DispatchMouseEventTypeOption, x: f64, y: f64, buttons: u32| DispatchMouseEvent {
            Type: kind,
            x,
            y,
            modifiers: None,
            timestamp: None,
            button: Some(MouseButton::Left),
            buttons: Some(buttons),
            click_count: Some(1),
            force: None,
            tangential_pressure: None,
            tilt_x: None,
            tilt_y: None,
            twist: None,
            delta_x: None,
            delta_y: None,
            pointer_Type: None,
        };
    tab.call_method(event(DispatchMouseEventTypeOption::MouseMoved, x1, y1, 0))
        .unwrap();
    tab.call_method(event(DispatchMouseEventTypeOption::MousePressed, x1, y1, 1))
        .unwrap();
    let steps = 6;
    for i in 1..=steps {
        let t = i as f64 / steps as f64;
        tab.call_method(event(
            DispatchMouseEventTypeOption::MouseMoved,
            x1 + (x2 - x1) * t,
            y1 + (y2 - y1) * t,
            1,
        ))
        .unwrap();
    }
    tab.call_method(event(
        DispatchMouseEventTypeOption::MouseReleased,
        x2,
        y2,
        0,
    ))
    .unwrap();
}

/// The text a copy would take: the live selection as a string.
pub fn selection_text(tab: &headless_chrome::Tab) -> String {
    eval(
        tab,
        "String(window.getSelection ? window.getSelection().toString() : '')",
    )
    .as_str()
    .unwrap_or("")
    .to_string()
}

/// A Codex rollout of `turns` turns — each a prompt, then (narrated, so no call folds into an
/// activity) a failing `cargo test --lib` (exit 1, 2.50s), a long `cargo build --release`
/// (exit 0, 1m 5s), a declined `cargo fmt` (42ms) and an update of README.md: the heads whose
/// chips carry an exit code, a duration and a status word (#117). Claude's own format records
/// neither an exit code nor a duration, so these heads need a Codex store.
pub fn codex_tool_session(id: &str, turns: u32) -> String {
    fn at(k: u32, i: u32) -> String {
        format!("{DAY}:{:02}:{:02}Z", k % 60, i % 60)
    }
    fn user(k: u32, i: u32, text: &str) -> String {
        format!(
            "{{\"timestamp\":\"{}\",\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"{text}\"}}]}}}}\n",
            at(k, i)
        )
    }
    fn assistant(k: u32, i: u32, phase: &str, text: &str) -> String {
        format!(
            "{{\"timestamp\":\"{}\",\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"assistant\",\"phase\":\"{phase}\",\"content\":[{{\"type\":\"output_text\",\"text\":\"{text}\"}}]}}}}\n",
            at(k, i)
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn command(
        k: u32,
        i: u32,
        cid: &str,
        cmd: &str,
        status: &str,
        exit: Option<i32>,
        secs: u64,
        nanos: u64,
        output: &str,
    ) -> String {
        let exit = exit
            .map(|e| format!(",\"exit_code\":{e}"))
            .unwrap_or_default();
        format!(
            "{{\"timestamp\":\"{}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"item_completed\",\"item\":{{\"type\":\"CommandExecution\",\"id\":\"{cid}\",\"command\":[\"/bin/zsh\",\"-lc\",\"{cmd}\"],\"status\":\"{status}\"{exit},\"duration\":{{\"secs\":{secs},\"nanos\":{nanos}}},\"formatted_output\":\"{output}\"}}}}}}\n",
            at(k, i)
        )
    }
    fn edit(k: u32, i: u32, cid: &str) -> String {
        format!(
            "{{\"timestamp\":\"{}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"item_completed\",\"item\":{{\"type\":\"FileChange\",\"id\":\"{cid}\",\"status\":\"completed\",\"changes\":{{\"/r/README.md\":{{\"type\":\"update\",\"unified_diff\":\"@@ -1 +1 @@\\n-old readme\\n+new readme\\n\"}}}}}}}}}}\n",
            at(k, i)
        )
    }
    let mut out = format!(
        "{{\"timestamp\":\"{}\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"cwd\":\"/r\",\"originator\":\"codex-tui\",\"cli_version\":\"0.147.0\"}}}}\n",
        at(0, 0)
    );
    for k in 1..=turns {
        out += &user(
            k,
            0,
            &format!("Turn {k}: run the checks and patch the readme"),
        );
        out += &assistant(k, 1, "commentary", "Running the unit tests first.");
        out += &command(
            k,
            2,
            &format!("exec-{k}-fail"),
            "cargo test --lib",
            "failed",
            Some(1),
            2,
            500_000_000,
            "error: test failed, to rerun pass `--lib`\\n",
        );
        out += &assistant(
            k,
            3,
            "commentary",
            "That failed; a release build takes a while.",
        );
        out += &command(
            k,
            4,
            &format!("exec-{k}-long"),
            "cargo build --release",
            "completed",
            Some(0),
            65,
            0,
            "    Finished release [optimized] target(s)\\n",
        );
        out += &assistant(
            k,
            5,
            "commentary",
            "The formatter was declined by the sandbox.",
        );
        out += &command(
            k,
            6,
            &format!("exec-{k}-declined"),
            "cargo fmt",
            "declined",
            None,
            0,
            42_000_000,
            "",
        );
        out += &assistant(k, 7, "commentary", "Patching the readme now.");
        out += &edit(k, 8, &format!("edit-{k}"));
        out += &assistant(k, 9, "final", &format!("Done with turn {k}."));
    }
    out
}
