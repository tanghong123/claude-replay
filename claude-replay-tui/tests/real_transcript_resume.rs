//! A durable resume against a REAL transcript, not a synthetic fixture.
//!
//! `durable_cache.rs` builds transcripts that exercise the shapes the design names. This one
//! points the same machinery at whatever the developer actually has on disk — the shapes nobody
//! thought to write down. `#[ignore]`d: it needs real sessions, so it runs on request.
//!
//! `cargo test -p claude-replay-tui --test real_transcript_resume -- --ignored --nocapture`

use claude_replay_core::engine::meta_stream::Versions;
use claude_replay_core::model::Block;
use claude_replay_core::{discover, parse_session_as, Transcript};
use claude_replay_present::cache::{admit::Origin, Admission, Presentation, SessionCache};
use claude_replay_tui::store::ArcLog;
use std::path::{Path, PathBuf};

type Cache = SessionCache<ArcLog, ()>;

fn open(c: &Cache, src: &Path) -> (Vec<Block>, Origin) {
    let agent = discover::detect_agent(src);
    c.register("s", Transcript::open(agent, src.to_path_buf()));
    let (session, origin) = match c.admit("s", |dir| ArcLog::open_append(&dir.join("blocks.jsonl")))
    {
        Admission::Owned { session, origin } => (session, origin),
        Admission::Denied(_) => panic!("a private root must be Owned"),
    };
    let d = c.poll_view("s", ArcLog::memory).unwrap().unwrap();
    let mut blocks: Vec<Block> = session
        .committed_arcs()
        .iter()
        .map(|a| a.as_ref().clone())
        .collect();
    blocks.extend(d.provisional.iter().map(|a| a.as_ref().clone()));
    (blocks, origin)
}

/// Every real transcript on this machine, biggest first, resumes to a block-identical session.
#[test]
#[ignore]
fn real_transcripts_resume_identically() {
    let mut found: Vec<PathBuf> = Vec::new();
    for c in discover::candidates_all(None) {
        found.push(c.path);
    }
    found.sort_by_key(|p| std::cmp::Reverse(std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)));
    found.truncate(8);
    assert!(!found.is_empty(), "no real transcripts discovered");

    for src in &found {
        let root = std::env::temp_dir().join(format!(
            "cr-real-{}-{}",
            std::process::id(),
            src.file_stem().unwrap().to_string_lossy()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let agent = discover::detect_agent(src);
        let want = parse_session_as(agent, src).unwrap().blocks();

        let cold_origin = {
            let c = Cache::durable(Presentation::Tui, root.clone(), Versions::current(None));
            let (got, o) = open(&c, src);
            assert_eq!(got, want, "cold run differs from a cold parse: {src:?}");
            c.release_all();
            o
        };
        assert!(matches!(cold_origin, Origin::Cold(_)));

        let c = Cache::durable(Presentation::Tui, root.clone(), Versions::current(None));
        let (got, origin) = open(&c, src);
        assert_eq!(got, want, "RESUMED run differs from a cold parse: {src:?}");
        match origin {
            Origin::Resumed {
                committed,
                replay_from,
            } => {
                let len = std::fs::metadata(src).unwrap().len();
                println!(
                    "{:>7} blocks  resumed at {committed} committed, byte {replay_from}/{len} \
                     ({:.0}% of the file skipped)  {}",
                    want.len(),
                    100.0 * replay_from as f64 / len as f64,
                    src.file_name().unwrap().to_string_lossy()
                );
                assert!(replay_from > 0, "{src:?} resumed at byte 0 — no work saved");
            }
            other => panic!("{src:?} did not resume: {other:?}"),
        }
        drop(c);
        let _ = std::fs::remove_dir_all(&root);
    }
}
