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
use crate::model::Block;
use crate::Agent;

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
    pub user_times: Vec<Option<f64>>,
    /// Token / cost tally for the session.
    pub metrics: Metrics,
    /// Derived within-session indices — turns / agents / tools / attachments (§7).
    pub index: SessionIndex,
}

/// Auto-detect the agent from the transcript head, then parse into a [`Session`].
/// Streaming (one line resident); no sub-agent enrichment.
pub fn parse_session(path: &Path) -> io::Result<Session> {
    parse_session_as(crate::discover::detect_agent(path), path)
}

/// Parse for a **known** agent, skipping detection — for a caller that already sniffed.
pub fn parse_session_as(agent: Agent, path: &Path) -> io::Result<Session> {
    // Parsing ignores CLI flags (fold is a view-layer concern), so the parse API takes no
    // `Args` — that keeps clap out of the core. Metrics are folded in the SAME streaming
    // pass (M10) — one file read, no separate `parse_reader_for`.
    let (blocks, user_times, metrics) = crate::model::parse_path_timed_for(agent, path)?;
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
            crate::model::parse_path_timed_for(Agent::Claude, &path).unwrap();
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
}
