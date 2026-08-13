//! **Qoder CLI's transcript discovery** — the qodercli half of the shared `discover`
//! interface. Qoder CLI is a Claude-Code-format terminal agent whose store layout is
//! identical (`~/.qoder/projects/<slug>/<id>.jsonl`, same slug convention), so this is a
//! thin wrapper over `claude_discover`'s root-parameterized internals — only the root (and
//! the agent tag on candidates) differ. Parsing likewise delegates to the Claude
//! implementations (see the `QoderAdapter` in `adapters.rs`); the format-level tells are the
//! `runtime-config` head line carrying `reasoningEffort`/`contextWindow` (detection) and
//! `usage.credits` billing (metrics), both handled on the shared Claude paths.

use claude_replay_engine::seam::{Agent, Candidate, CardMemo, CardOutcome};
use std::path::{Path, PathBuf};

/// Root under which Qoder CLI writes per-project transcript dirs.
pub(crate) fn projects_dir() -> PathBuf {
    if let Ok(p) = std::env::var("QODER_PROJECTS_DIR") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    Path::new(&home).join(".qoder").join("projects")
}

/// Qoder sessions scoped strictly to `cwd` or its nearest ancestor that has sessions —
/// the same no-global-fallback scoping as the Claude store.
pub fn candidates_scoped(cwd: &Path) -> Vec<Candidate> {
    crate::agents::claude::discover::candidates_scoped_in(
        &projects_dir(),
        Agent::QODER,
        cwd,
        claude_replay_engine::seam::home_dir().as_deref(),
    )
}

/// Every main transcript in the Qoder store, newest first — the store walk is Claude's
/// (`<root>/<slug>/<id>.jsonl`; sub-agent transcripts live one level deeper under
/// `<slug>/<sid>/subagents/` and are reached through their parent, never listed here).
pub(crate) fn store_transcripts() -> Vec<PathBuf> {
    crate::agents::claude::discover::store_transcripts_in(&projects_dir())
}

/// Find a Qoder transcript by session id (`<id>.jsonl`) anywhere under its projects dir.
pub fn transcript_by_id(id: &str) -> Option<PathBuf> {
    crate::agents::claude::discover::transcript_by_id_in(&projects_dir(), id)
}

/// Every `subagents/` dir that may hold this session's children, in probe order: beside the
/// transcript first (Claude's layout), then `<root>/<any-slug>/<sid>/subagents` across the
/// store. The second form is real, not defensive: a mid-session `cwd` change (entering a
/// worktree) makes qodercli file the companion dir under the NEW cwd's slug while the main
/// transcript stays under the original one — observed on a live store, where the beside-
/// the-transcript probe alone finds nothing.
pub(crate) fn subagents_dirs(path: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
        return out;
    };
    if let Some(parent) = path.parent() {
        let d = parent.join(stem).join("subagents");
        if d.is_dir() {
            out.push(d);
        }
    }
    if let Ok(projects) = std::fs::read_dir(projects_dir()) {
        for proj in projects.flatten() {
            let d = proj.path().join(stem).join("subagents");
            if d.is_dir() && !out.contains(&d) {
                out.push(d);
            }
        }
    }
    out
}

/// The on-disk transcript for `agent_id` reached from the session at `session_path` —
/// Claude's beside-the-transcript shape plus the drifted-slug dirs `subagents_dirs` probes.
pub(crate) fn subagent_file(session_path: &Path, agent_id: &str) -> Option<PathBuf> {
    subagents_dirs(session_path)
        .into_iter()
        .map(|d| d.join(format!("agent-{agent_id}.jsonl")))
        .find(|f| f.is_file())
}

/// Qoder's `session_card` — wholly Claude's tail scanner: Qoder writes the same
/// `ai-title`/`custom-title`/`last-prompt` lines into the transcript itself, so there is
/// no external title store to consult (unlike QoderWork's sidecar/database).
pub(crate) fn session_card(path: &Path, memo: Option<&CardMemo>) -> CardOutcome {
    crate::agents::claude::discover::session_card(path, memo)
}

/// The live on-disk task list for a Qoder session — the same `<root>/<sessionId>/<n>.json`
/// layout as Claude's `~/.claude/tasks`, under `~/.qoder/tasks` (`QODER_TASKS_ROOT`
/// overrides for tests).
pub(crate) fn load_tasks(path: &Path) -> Option<claude_replay_engine::seam::TaskList> {
    let root = std::env::var_os("QODER_TASKS_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            Path::new(&home).join(".qoder").join("tasks")
        });
    crate::agents::claude::discover::load_tasks_in(&root, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// `QODER_PROJECTS_DIR` is a PROCESS-global override and cargo tests share one process,
    /// so env-scoped tests serialize on this (the same discipline as the QoderWork suite).
    static STORE_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A minimal Qoder-shaped transcript: the runtime-config head that drives detection
    /// (with the `reasoningEffort`/`contextWindow` keys QoderWork's head lacks), then
    /// Claude-shaped conversation lines with credits-bearing usage.
    fn write_fixture(path: &Path) {
        let mut f = std::fs::File::create(path).unwrap();
        writeln!(f, r#"{{"type":"runtime-config","sessionId":"abc123","model":"cmodel","reasoningEffort":null,"contextWindow":null,"timestamp":1786606218598}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"hello qoder"}}]}},"timestamp":"2026-08-13T07:32:00Z","isSidechain":false}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","model":"cmodel","content":[{{"type":"thinking","thinking":"pondering","signature":""}}],"usage":{{"input_tokens":0,"output_tokens":0,"credits":1.25}}}},"timestamp":"2026-08-13T07:32:05Z","isSidechain":false}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","model":"cmodel","content":[{{"type":"text","text":"hi","citations":null}}],"usage":{{"input_tokens":0,"output_tokens":0,"credits":0.75}}}},"timestamp":"2026-08-13T07:32:06Z","isSidechain":false}}"#).unwrap();
    }

    /// Discovery over a fake Qoder store: candidates come back tagged `QODER`, scoped to the
    /// cwd's slug, and a bare id resolves to its transcript — the picker/`--latest`/bare-id
    /// surface the Claude store already has, on the Qoder root.
    #[test]
    fn discovers_and_resolves_from_the_qoder_store() {
        let _env = STORE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("qd-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let cwd = Path::new("/Users/dev/proj");
        let slug = "-Users-dev-proj";
        std::fs::create_dir_all(root.join(slug)).unwrap();
        write_fixture(&root.join(slug).join("abc123.jsonl"));

        std::env::set_var("QODER_PROJECTS_DIR", &root);
        let by_id = transcript_by_id("abc123");
        let walked = store_transcripts();
        std::env::remove_var("QODER_PROJECTS_DIR");

        let cands = crate::agents::claude::discover::candidates_scoped_in(
            &root,
            Agent::QODER,
            cwd,
            Some(Path::new("/Users/dev")),
        );
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].agent, Agent::QODER);
        assert!(cands[0].cwd_affinity, "scoped to the cwd's own slug");
        assert_eq!(
            by_id.as_deref(),
            Some(root.join(slug).join("abc123.jsonl").as_path())
        );
        assert_eq!(walked.len(), 1, "the store walk sees the one transcript");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The delegation guarantee (the same one QoderWork's suite pins): parsing a Qoder
    /// transcript AS Qoder is byte-identical to parsing it as Claude — the adapter adds
    /// detection and a store, never a format fork. Credits land in the metrics bag either
    /// way, because the fold is Claude's.
    #[test]
    fn qoder_parse_is_byte_identical_to_claude_parse() {
        let f = std::env::temp_dir().join(format!("qd-equiv-{}.jsonl", std::process::id()));
        write_fixture(&f);
        let qd = claude_replay_core::parse_session_as(Agent::QODER, &f).unwrap();
        let cl = claude_replay_core::parse_session_as(Agent::CLAUDE, &f).unwrap();
        assert_eq!(
            format!("{:?}", qd.blocks()),
            format!("{:?}", cl.blocks()),
            "blocks identical under delegation"
        );
        assert_eq!(qd.user_times, cl.user_times);
        assert_eq!(qd.metrics, cl.metrics);
        assert_eq!(qd.agent, Agent::QODER, "identity is the only difference");
        // The credits fold: 1.25 + 0.75 = 2.00 credits, surfaced by the shared footer.
        assert_eq!(qd.metrics.extra.get("credits_micro"), Some(&2_000_000));
        assert!(
            qd.metrics.footer().contains("~2.00 credits"),
            "footer: {}",
            qd.metrics.footer()
        );
        let _ = std::fs::remove_file(&f);
    }

    /// The session card comes from the transcript's own title/prompt lines — Qoder writes
    /// `ai-title` and `last-prompt` exactly as Claude Code does.
    #[test]
    fn session_card_reads_the_transcripts_own_title_lines() {
        let d = std::env::temp_dir().join(format!("qd-card-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("abc123.jsonl");
        std::fs::write(
            &p,
            concat!(
                "{\"type\":\"ai-title\",\"sessionId\":\"abc123\",\"aiTitle\":\"配置 qoder 适配\"}\n",
                "{\"type\":\"last-prompt\",\"sessionId\":\"abc123\",\"lastPrompt\":\"do the thing\"}\n",
            ),
        )
        .unwrap();
        match session_card(&p, None) {
            CardOutcome::Fresh { card, .. } => {
                assert_eq!(card.title.as_deref(), Some("配置 qoder 适配"));
                assert_eq!(card.last_prompt.as_deref(), Some("do the thing"));
            }
            other => panic!("expected a fresh card, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The main/sub-agent chain, end to end on Qoder's real on-disk layout: a parent
    /// `Agent` spawn whose `tool_result` carries `toolUseResult.agentId`, and the child at
    /// `<sid>/subagents/agent-<id>.jsonl` — the same flat dir Claude's enrichment walks.
    /// The child's `redacted_thinking` decodes to the placeholder, never the ciphertext.
    #[test]
    fn subagent_chain_enriches_from_the_qoder_layout() {
        use claude_replay_engine::seam::Block;
        let d = std::env::temp_dir().join(format!("qd-tree-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let sadir = d.join("sess1").join("subagents");
        std::fs::create_dir_all(&sadir).unwrap();

        let parent = d.join("sess1.jsonl");
        std::fs::write(&parent, concat!(
            r#"{"type":"runtime-config","sessionId":"sess1","model":"cmodel","reasoningEffort":null,"contextWindow":null,"timestamp":1786606218598}"#, "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"research this"}]},"timestamp":"2026-08-13T08:00:00Z"}"#, "\n",
            r#"{"type":"assistant","message":{"role":"assistant","model":"cmodel","content":[{"type":"tool_use","id":"tu1","name":"Agent","input":{"description":"调研架构","prompt":"go","subagent_type":"Explore"}}],"usage":{"input_tokens":0,"output_tokens":0,"credits":2.0}},"timestamp":"2026-08-13T08:00:10Z"}"#, "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu1","content":"done"}]},"toolUseResult":{"agentId":"aExplore-1234","status":"completed"},"timestamp":"2026-08-13T08:01:00Z"}"#, "\n",
        )).unwrap();
        std::fs::write(sadir.join("agent-aExplore-1234.jsonl"), concat!(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"go"}]},"timestamp":"2026-08-13T08:00:11Z","isSidechain":true,"agentId":"aExplore-1234","parent_tool_use_id":"tu1"}"#, "\n",
            r#"{"type":"assistant","message":{"role":"assistant","model":"cmodel","content":[{"type":"redacted_thinking","data":"QE:c2VjcmV0","reasoning_item":{"id":"ri1"}}],"usage":{"input_tokens":0,"output_tokens":0,"credits":0.5}},"timestamp":"2026-08-13T08:00:20Z","isSidechain":true}"#, "\n",
            r#"{"type":"assistant","message":{"role":"assistant","model":"cmodel","content":[{"type":"text","text":"child answer"}],"usage":{"input_tokens":0,"output_tokens":0,"credits":0.25}},"timestamp":"2026-08-13T08:00:30Z","isSidechain":true}"#, "\n",
        )).unwrap();

        let s = claude_replay_core::parse_session_enriched_as(Agent::QODER, &parent).unwrap();
        let blocks = s.blocks();
        let sa = blocks
            .iter()
            .find_map(|b| match b {
                Block::SubAgent(sa) => Some(sa),
                _ => None,
            })
            .expect("the Agent spawn folds to a SubAgent block");
        assert_eq!(
            sa.agent_id, "aExplore-1234",
            "joined via toolUseResult.agentId"
        );
        assert!(
            !sa.blocks.is_empty(),
            "child transcript attached from subagents/"
        );
        let child_dump = format!("{:?}", sa.blocks);
        assert!(
            child_dump.contains("[redacted thinking]"),
            "redacted reasoning keeps its place in the work span"
        );
        assert!(
            !child_dump.contains("QE:"),
            "the ciphertext is never surfaced"
        );
        assert!(child_dump.contains("child answer"));
        // The parent's own credits folded through the shared accumulator.
        assert_eq!(s.metrics.extra.get("credits_micro"), Some(&2_000_000));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The DRIFTED layout observed on a live store: the session enters a worktree
    /// mid-run, so qodercli files `subagents/` under the NEW cwd's slug while the main
    /// transcript stays under the original slug. `subagents_dirs` finds the drifted dir
    /// through the store root, and enrichment attaches the child from there.
    #[test]
    fn subagents_are_found_across_a_mid_session_cwd_change() {
        use claude_replay_engine::seam::Block;
        let _env = STORE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("qd-drift-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let orig = root.join("-Users-dev-proj");
        let drifted = root.join("-Users-dev-proj--qoder-worktrees-wt");
        std::fs::create_dir_all(&orig).unwrap();
        let sadir = drifted.join("sess2").join("subagents");
        std::fs::create_dir_all(&sadir).unwrap();

        let parent = orig.join("sess2.jsonl");
        std::fs::write(&parent, concat!(
            r#"{"type":"runtime-config","sessionId":"sess2","model":"cmodel","reasoningEffort":null,"contextWindow":null,"timestamp":1786606218598}"#, "\n",
            r#"{"type":"assistant","message":{"role":"assistant","model":"cmodel","content":[{"type":"tool_use","id":"tu1","name":"Agent","input":{"description":"survey","prompt":"go","subagent_type":"Explore"}}],"usage":{"input_tokens":0,"output_tokens":0,"credits":1.0}},"timestamp":"2026-08-13T09:00:10Z"}"#, "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu1","content":"done"}]},"toolUseResult":{"agentId":"aExplore-9999","status":"completed"},"timestamp":"2026-08-13T09:01:00Z"}"#, "\n",
        )).unwrap();
        std::fs::write(
            sadir.join("agent-aExplore-9999.jsonl"),
            concat!(
                r#"{"type":"assistant","message":{"role":"assistant","model":"cmodel","content":[{"type":"text","text":"drifted child"}],"usage":{"input_tokens":0,"output_tokens":0,"credits":0.1}},"timestamp":"2026-08-13T09:00:30Z","isSidechain":true}"#,
                "\n",
            ),
        )
        .unwrap();

        std::env::set_var("QODER_PROJECTS_DIR", &root);
        let dirs = subagents_dirs(&parent);
        let by_id = subagent_file(&parent, "aExplore-9999");
        let s = claude_replay_core::parse_session_enriched_as(Agent::QODER, &parent).unwrap();
        std::env::remove_var("QODER_PROJECTS_DIR");

        assert_eq!(
            dirs,
            vec![sadir.clone()],
            "the drifted dir is the only candidate"
        );
        assert_eq!(
            by_id.as_deref(),
            Some(sadir.join("agent-aExplore-9999.jsonl").as_path())
        );
        let blocks = s.blocks();
        let sa = blocks
            .iter()
            .find_map(|b| match b {
                Block::SubAgent(sa) => Some(sa),
                _ => None,
            })
            .expect("SubAgent block");
        assert!(
            format!("{:?}", sa.blocks).contains("drifted child"),
            "the child attaches from the drifted slug"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The live task list reads from `~/.qoder/tasks/<sessionId>/<n>.json` — Claude's
    /// sidecar layout on the Qoder root, so the TUI/HTML task panel works unchanged.
    #[test]
    fn load_tasks_reads_the_qoder_tasks_store() {
        let root = std::env::temp_dir().join(format!("qd-tasks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sess9")).unwrap();
        std::fs::write(
            root.join("sess9").join("1.json"),
            r#"{"id":"1","subject":"Implementing Slice 4","status":"in_progress"}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("sess9").join("2.json"),
            r#"{"id":"2","subject":"done thing","status":"completed"}"#,
        )
        .unwrap();
        let transcript = root.join("sess9.jsonl");

        std::env::set_var("QODER_TASKS_ROOT", &root);
        let tasks = load_tasks(&transcript);
        std::env::remove_var("QODER_TASKS_ROOT");

        let tasks = tasks.expect("task list loads");
        assert_eq!(tasks.items.len(), 2);
        assert_eq!(tasks.items[0].subject, "Implementing Slice 4");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// PROVENANCE-FIRST detection (#20 follow-up): a session inside `~/.qoder/projects` is
    /// Qoder's even though its `runtime-config` head is in-band IDENTICAL to a real
    /// QoderWork head — the store decides, before any sniff. The same bytes outside any
    /// store honestly label as QoderWork: the shared head is the qwork-family signature,
    /// and nothing in-band is distinctively Qoder's.
    #[test]
    fn detection_attributes_the_qoder_store_by_provenance() {
        let _env = STORE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("qd-prov-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let slug = root.join("-Users-dev-proj");
        std::fs::create_dir_all(&slug).unwrap();
        let inside = slug.join("abc123.jsonl");
        write_fixture(&inside);

        std::env::set_var("QODER_PROJECTS_DIR", &root);
        let claimed = claude_replay_core::discover::detect_agent_claimed(&inside);
        std::env::remove_var("QODER_PROJECTS_DIR");
        assert_eq!(
            claimed,
            (Agent::QODER, true),
            "the store attributes the session, marker-free"
        );

        let outside =
            std::env::temp_dir().join(format!("qd-prov-out-{}.jsonl", std::process::id()));
        write_fixture(&outside);
        assert_eq!(
            claude_replay_core::discover::detect_agent_claimed(&outside),
            (Agent::QODERWORK, true),
            "out-of-store, the shared runtime-config head sniffs as QoderWork"
        );
        let _ = std::fs::remove_file(&outside);
        let _ = std::fs::remove_dir_all(&root);
    }
}
