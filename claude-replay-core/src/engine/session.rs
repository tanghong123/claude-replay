//! Layer 2 output — the [`Session`]: a fully-parsed transcript as one value.
//!
//! This is the public, agent-neutral shape a library consumer builds on (design
//! `parser-engine.md` §3.3), without pulling in the TUI / HTML / syntect / clap layers.
//!
//! `parse_session` produces the whole `Session` — blocks + per-turn times + folded metrics
//! (M10, one file read) + the derived `SessionIndex` (M4) — from one streaming pass. Per-turn
//! timestamps still ride `user_times` directly (mirrored onto `index.turns`) until consumers
//! migrate off the field.

use std::io;
use std::path::{Path, PathBuf};

use crate::engine::SessionIndex;
use crate::metrics::Metrics;
use crate::model::{Block, EpochSeconds};
use crate::{Agent, SessionGraph};

/// A fully-parsed session — everything a consumer needs to render or analyze a transcript
/// without touching the presentation layers. Produced by one streaming parse.
#[derive(Debug, Clone)]
pub struct Session {
    /// Which agent produced the transcript.
    pub agent: Agent,
    /// The session working directory, when the transcript recorded it.
    pub cwd: Option<PathBuf>,
    /// The ordered block stream (tool results already joined onto their calls).
    pub blocks: Vec<Block>,
    /// One timestamp per user turn, in order. Mirrored onto `index.turns[*].time`; kept as
    /// a field until consumers migrate off it.
    pub user_times: Vec<Option<EpochSeconds>>,
    /// Token / cost tally for the session.
    pub metrics: Metrics,
    /// Derived within-session indices — turns / agents / tools / attachments (§7).
    pub index: SessionIndex,
}

/// **The entry point.** Auto-detect the agent from the transcript head, then parse the file
/// into a [`Session`] (blocks + index + metrics + cwd). Streaming — one line resident, so a
/// multi-gigabyte transcript never balloons into memory. Sub-agent child transcripts are NOT
/// loaded (`SubAgent.blocks` stays empty); this is the flat top-level session.
///
/// ```no_run
/// let session = claude_replay_core::parse_session(std::path::Path::new("session.jsonl"))?;
/// println!("{} blocks, {} turns", session.blocks.len(), session.index.turns.len());
/// for block in &session.blocks {
///     // render / analyze `block` — see `claude_replay_core::Block`
/// }
/// # Ok::<(), std::io::Error>(())
/// ```
///
/// For a live tail (fold only appended bytes each poll), use [`FollowParser`](crate::FollowParser).
pub fn parse_session(path: &Path) -> io::Result<Session> {
    parse_session_as(crate::discover::detect_agent(path), path)
}

/// Like [`parse_session`], but also resolves relationship metadata and loads the
/// **sub-agent tree** — each `SubAgent`'s child transcript recursively into its `blocks`,
/// so a consumer can descend into spawned agents or roll up subtree cost. `parse_session`
/// leaves `SubAgent.blocks` empty (cheaper, flat); use this when you need the whole tree.
/// Top-level transcript content and metrics are unchanged; resolved relationship ids,
/// the agent index, and nested child content may differ.
pub fn parse_session_enriched(path: &Path) -> io::Result<Session> {
    parse_session_enriched_as(crate::discover::detect_agent(path), path)
}

/// [`parse_session_enriched`] for a **known** agent (skips detection).
pub fn parse_session_enriched_as(agent: Agent, path: &Path) -> io::Result<Session> {
    let graph = SessionGraph::open(agent, path);
    let mut session = parse_session_with_graph(agent, path, graph.clone())?;
    let mut seen = std::collections::HashSet::new();
    enrich_subagent_tree(agent, path, &graph, &mut session.blocks, &mut seen);
    session.index = SessionIndex::build(&session.blocks, &session.user_times);
    Ok(session)
}

/// Parse for a **known** agent, skipping detection — for a caller that already sniffed.
pub fn parse_session_as(agent: Agent, path: &Path) -> io::Result<Session> {
    parse_session_flat(agent, path)
}

/// Parse a known-agent transcript and resolve its relationship metadata through an
/// operation graph shared with the surrounding TUI, HTML traversal, or live follower.
/// Child transcript content remains lazy; use [`parse_session_enriched_as`] for an eager tree.
pub fn parse_session_with_graph(
    agent: Agent,
    path: &Path,
    graph: SessionGraph,
) -> io::Result<Session> {
    let mut session = parse_session_flat(agent, path)?;
    graph.resolve_relationships(path, &mut session.blocks);
    session.index = SessionIndex::build(&session.blocks, &session.user_times);
    Ok(session)
}

fn parse_session_flat(agent: Agent, path: &Path) -> io::Result<Session> {
    // Parsing ignores CLI flags (fold is a view-layer concern), so the parse API takes no
    // `Args` — that keeps clap out of the core. Metrics are folded in the SAME streaming
    // pass (M10) — one file read, no separate `parse_reader_for`.
    let (blocks, user_times, metrics) = crate::engine::replay::parse_path_timed_for(agent, path)?;
    let cwd = crate::discover::session_cwd(path);
    let index = SessionIndex::build(&blocks, &user_times);
    Ok(Session {
        agent,
        cwd,
        blocks,
        user_times,
        metrics,
        index,
    })
}

fn enrich_subagent_tree(
    agent: Agent,
    root: &Path,
    graph: &SessionGraph,
    blocks: &mut [Block],
    seen: &mut std::collections::HashSet<String>,
) {
    for block in blocks {
        let Block::SubAgent(subagent) = block else {
            continue;
        };
        if subagent.agent_id.is_empty() || !seen.insert(subagent.agent_id.clone()) {
            continue;
        }
        let Some(source) = graph.subagent_source(root, &subagent.agent_id) else {
            continue;
        };
        let Ok(mut child) = parse_session_with_graph(agent, &source, graph.clone()) else {
            continue;
        };
        enrich_subagent_tree(agent, root, graph, &mut child.blocks, seen);
        subagent.subtree_cost = subtree_cost(&child.metrics, &child.blocks);
        subagent.blocks = child.blocks;
    }
}

fn subtree_cost(metrics: &Metrics, blocks: &[Block]) -> Option<crate::model::UsdCost> {
    let descendants: crate::model::UsdCost = blocks
        .iter()
        .filter_map(|block| match block {
            Block::SubAgent(subagent) => subagent.subtree_cost,
            _ => None,
        })
        .sum();
    match metrics.cost_usd {
        Some(own) => Some(own + descendants),
        None if descendants > 0.0 => Some(descendants),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn tmp(name: &str, body: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "cr-session-{}-{}-{name}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::File::create(&p)
            .unwrap()
            .write_all(body.as_bytes())
            .unwrap();
        p
    }

    /// `parse_session` must return exactly what the existing entry points return — same
    /// blocks, same per-turn times, same metrics, same cwd — just bundled. This is the
    /// byte-identical gate for the wrapper.
    #[test]
    fn parse_session_matches_the_existing_entry_points() {
        let body = concat!(
            r#"{"type":"user","cwd":"/repo","message":{"role":"user","content":[{"type":"text","text":"hi"}]},"timestamp":"2026-07-26T10:00:00Z"}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hello"}],"usage":{"input_tokens":3,"output_tokens":5}},"timestamp":"2026-07-26T10:00:01Z"}"#,
            "\n",
        );
        let path = tmp("claude.jsonl", body);

        let s = parse_session(&path).unwrap();
        assert_eq!(s.agent, Agent::Claude);

        let (blocks, times, folded_metrics) =
            crate::engine::replay::parse_path_timed_for(Agent::Claude, &path).unwrap();
        // The retired separate metrics pass, as the byte-identical reference for the fold.
        let ref_metrics = crate::metrics::parse_reader_for(
            Agent::Claude,
            io::BufReader::new(std::fs::File::open(&path).unwrap()),
        );
        // `Block` isn't `PartialEq` (like the other equivalence tests, compare via Debug).
        assert_eq!(
            format!("{:?}", s.blocks),
            format!("{:?}", blocks),
            "blocks match the existing parse"
        );
        assert_eq!(s.user_times, times, "user_times match");
        assert_eq!(
            folded_metrics, ref_metrics,
            "folded metrics (M10) == the separate parse_reader_for pass"
        );
        assert_eq!(
            s.metrics, ref_metrics,
            "session metrics == the reference pass"
        );
        assert_eq!(s.cwd, crate::discover::session_cwd(&path), "cwd matches");
        assert_eq!(s.cwd.as_deref(), Some(Path::new("/repo")));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn enriched_claude_session_loads_child_content_separately_from_resolution() {
        static N: AtomicUsize = AtomicUsize::new(0);
        let base = std::env::temp_dir().join(format!(
            "cr-session-enriched-claude-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::remove_dir_all(&base).ok();
        let parent = base.join("project").join("root.jsonl");
        let child = base
            .join("project")
            .join("root")
            .join("subagents")
            .join("agent-child.jsonl");
        std::fs::create_dir_all(child.parent().unwrap()).unwrap();
        std::fs::write(
            &parent,
            concat!(
                r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"spawn","name":"Agent","input":{"subagent_type":"Explore","description":"inspect","prompt":"go"}}]}}"#,
                "\n",
                r#"{"type":"user","toolUseResult":{"agentId":"child","status":"completed"},"message":{"content":[{"type":"tool_result","tool_use_id":"spawn","content":"done"}]}}"#,
                "\n"
            ),
        )
        .unwrap();
        std::fs::write(
            &child,
            concat!(
                r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"child turn"}]}}"#,
                "\n"
            ),
        )
        .unwrap();

        let session = parse_session_enriched_as(Agent::Claude, &parent).unwrap();
        let Some(Block::SubAgent(agent)) = session
            .blocks
            .iter()
            .find(|block| matches!(block, Block::SubAgent(_)))
        else {
            panic!("expected Claude sub-agent: {:#?}", session.blocks);
        };
        assert!(
            !agent.blocks.is_empty(),
            "the explicit eager API must load Claude child content"
        );

        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn enriched_codex_session_loads_child_tree_recursively() {
        static N: AtomicUsize = AtomicUsize::new(0);
        let base = std::env::temp_dir().join(format!(
            "cr-session-enriched-codex-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::remove_dir_all(&base).ok();
        let day = base.join("sessions").join("2026").join("07").join("28");
        std::fs::create_dir_all(&day).unwrap();
        let parent = day.join("rollout-parent.jsonl");
        let child = day.join("rollout-child.jsonl");
        let grandchild = day.join("rollout-grandchild.jsonl");
        std::fs::write(
            &parent,
            concat!(
                r#"{"type":"session_meta","payload":{"id":"parent","cwd":"/repo","source":"cli"}}"#,
                "\n",
                r#"{"type":"response_item","payload":{"type":"function_call","name":"spawn_agent","namespace":"collaboration","call_id":"spawn-child","arguments":"{\"task_name\":\"review\",\"message\":\"inspect\"}"}}"#,
                "\n",
                r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"spawn-child","output":"{\"task_name\":\"/root/review\"}"}}"#,
                "\n"
            ),
        )
        .unwrap();
        std::fs::write(
            &child,
            concat!(
                r#"{"type":"session_meta","payload":{"id":"child","cwd":"/repo","source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent","agent_path":"/root/review","agent_nickname":"Reviewer"}}}}}"#,
                "\n",
                r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"child turn"}]}}"#,
                "\n",
                r#"{"type":"response_item","payload":{"type":"function_call","name":"spawn_agent","namespace":"collaboration","call_id":"spawn-grandchild","arguments":"{\"task_name\":\"audit\",\"message\":\"audit\"}"}}"#,
                "\n",
                r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"spawn-grandchild","output":"{\"task_name\":\"/root/review/audit\"}"}}"#,
                "\n"
            ),
        )
        .unwrap();
        std::fs::write(
            &grandchild,
            concat!(
                r#"{"type":"session_meta","payload":{"id":"grandchild","cwd":"/repo","source":{"subagent":{"thread_spawn":{"parent_thread_id":"child","agent_path":"/root/review/audit","agent_nickname":"Auditor"}}}}}"#,
                "\n",
                r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"grandchild result"}]}}"#,
                "\n"
            ),
        )
        .unwrap();

        let session = parse_session_enriched_as(Agent::Codex, &parent).unwrap();
        let Some(Block::SubAgent(child_agent)) = session
            .blocks
            .iter()
            .find(|block| matches!(block, Block::SubAgent(_)))
        else {
            panic!("expected Codex child: {:#?}", session.blocks);
        };
        assert_eq!(child_agent.agent_id, "child");
        let Some(Block::SubAgent(grandchild_agent)) = child_agent
            .blocks
            .iter()
            .find(|block| matches!(block, Block::SubAgent(_)))
        else {
            panic!(
                "expected Codex grandchild in child blocks: {:#?}",
                child_agent.blocks
            );
        };
        assert_eq!(grandchild_agent.agent_id, "grandchild");
        assert!(
            !grandchild_agent.blocks.is_empty(),
            "the explicit eager API must recursively load Codex descendants"
        );

        std::fs::remove_dir_all(base).unwrap();
    }
}
