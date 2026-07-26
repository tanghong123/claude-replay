//! Layer 2 output — the [`Session`]: a fully-parsed transcript as one value.
//!
//! This is the public, agent-neutral shape a library consumer builds on (design
//! `parser-engine.md` §3.3), without pulling in the TUI / HTML / syntect / clap layers.
//!
//! Milestone status: `parse_session` currently bundles today's parse ([`parse_path_timed_for`])
//! with the (still separate) metrics pass. Two refinements land in later milestones and are
//! deliberately *not* here yet: the within-session `SessionIndex` (agents / tools /
//! attachments — §5.2 Phase 5), and folding metrics into the parse pass to drop the extra
//! file read (§5.2 Phase 4). Until the index exists, per-turn timestamps ride `user_times`
//! directly rather than `index.turns`.

use std::io;
use std::path::{Path, PathBuf};

use crate::metrics::Metrics;
use crate::model::Block;
use crate::{Agent, Args};

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
    /// One timestamp per user turn, in order. (Re-homes to `index.turns` in Phase 5.)
    pub user_times: Vec<Option<f64>>,
    /// Token / cost tally for the session.
    pub metrics: Metrics,
}

/// Auto-detect the agent from the transcript head, then parse into a [`Session`].
/// Streaming (one line resident); no sub-agent enrichment.
pub fn parse_session(path: &Path) -> io::Result<Session> {
    parse_session_as(crate::discover::detect_agent(path), path)
}

/// Parse for a **known** agent, skipping detection — for a caller that already sniffed.
pub fn parse_session_as(agent: Agent, path: &Path) -> io::Result<Session> {
    // Parsing ignores CLI flags (fold is a view-layer concern — both `parse_main` and
    // `parse_lines` take `_args`), so the default is exact and keeps `Args` out of the API.
    let args = Args::default();
    let (blocks, user_times) = crate::model::parse_path_timed_for(agent, path, &args)?;
    let metrics =
        crate::metrics::parse_reader_for(agent, io::BufReader::new(std::fs::File::open(path)?));
    let cwd = crate::discover::session_cwd(path);
    Ok(Session {
        agent,
        cwd,
        blocks,
        user_times,
        metrics,
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

        let args = Args::default();
        let (blocks, times) =
            crate::model::parse_path_timed_for(Agent::Claude, &path, &args).unwrap();
        let metrics = crate::metrics::parse_reader_for(
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
        assert_eq!(s.metrics, metrics, "metrics match the separate pass");
        assert_eq!(s.cwd, crate::discover::session_cwd(&path), "cwd matches");
        assert_eq!(s.cwd.as_deref(), Some(Path::new("/repo")));

        let _ = std::fs::remove_file(&path);
    }
}
