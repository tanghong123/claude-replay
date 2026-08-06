//! **What a TUI session switch actually costs**, component by component. `#[ignore]`d — it needs
//! this machine's real transcripts.
//!
//! Written to test a hypothesis that turned out to be wrong: that the durable cache should make a
//! switch instant. The cache was never the problem — `admit` resumes a 12,469-block session in
//! ~47 ms. The cost was the View's FIRST LAYOUT, which renders every block purely to count its
//! wrapped lines and then throws the styled output away, at ~150 µs a line of syntect parsing.
//!
//! #107 cut that two ways, both output-identical (`BYTE-IDENTICAL: PASS`): a collapsed write now
//! parses only the lines it prints, and the measure pass fans out across cores once `carry_in`
//! has settled. Measured on this machine, before → after:
//!
//! | session          | blocks | first layout before | after   |
//! |------------------|--------|---------------------|---------|
//! | 094539f2 (107MB) | 12,469 | 5.9 s               | 816 ms  |
//! | 530339ac (55MB)  |  3,404 | 1.3 s               | 291 ms  |
//! | 4752d00e (7MB)   |    688 | 180 ms              |  55 ms  |
//!
//! What this bench is FOR now: it fails nothing, so read the numbers. If `admit` ever rivals the
//! layout again, the cache regressed; if the layout climbs back toward seconds, the measure pass
//! stopped fanning out (see `View::measure_parallel`).
//!
//! `cargo test -p claude-replay-tui --test switch_cost --release -- --ignored --nocapture`
use claude_replay_core::engine::meta_stream::Versions;
use claude_replay_core::{discover, Agent, Transcript};
use claude_replay_present::cache::{Admission, Holder, Presentation, SessionCache};
use claude_replay_tui::store::{ArcLog, TuiNote};
use std::time::Instant;

type Cache = SessionCache<ArcLog, ()>;

#[test]
#[ignore]
fn switch_cost() {
    let src_dir = std::path::PathBuf::from(std::env::var("HOME").unwrap())
        .join(".claude/projects/-Users-hong-personal-claude-replay");
    let mut paths: Vec<_> = std::fs::read_dir(&src_dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("jsonl"))
        .collect();
    paths.sort_by_key(|p| std::cmp::Reverse(std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)));
    let root = std::env::temp_dir().join(format!("cr-switch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);

    let open = |c: &Cache, id: &str, p: &std::path::Path| {
        c.register(id, Transcript::open(Agent::CLAUDE, p.to_path_buf()));
        let t = Instant::now();
        let sess = match c.admit(
            id,
            |d| ArcLog::open_append(&d.join("blocks.jsonl")),
            |_: &Holder<TuiNote>| false,
        ) {
            Admission::Owned { session, .. } => session,
            Admission::Denied(_) => panic!("denied"),
        };
        let admit_ms = t.elapsed();
        let t = Instant::now();
        let d = c.poll_view(id, ArcLog::memory).unwrap().unwrap();
        let poll_ms = t.elapsed();
        let t = Instant::now();
        let blocks = sess.committed_arcs();
        let arcs_ms = t.elapsed();
        (
            admit_ms,
            poll_ms,
            arcs_ms,
            blocks.len() + d.provisional.len(),
        )
    };

    // Pass 1: populate the durable cache (the "cold" case).
    for p in &paths {
        let c = Cache::durable(Presentation::Tui, root.clone(), Versions::current(None));
        let id = p.file_stem().unwrap().to_str().unwrap();
        open(&c, id, p);
        c.release_all();
    }

    println!("\n--- a WARM switch, per session (cache already populated) ---");
    for p in &paths {
        let id = p.file_stem().unwrap().to_str().unwrap();
        let mb = std::fs::metadata(p).unwrap().len() as f64 / 1e6;
        let t = Instant::now();
        let c = Cache::durable(Presentation::Tui, root.clone(), Versions::current(None));
        let make_ms = t.elapsed();
        let (a, pl, ar, n) = open(&c, id, p);
        let t = Instant::now();
        let tasks = discover::session_tasks(Agent::CLAUDE, p);
        let tasks_ms = t.elapsed();
        println!(
            "{:<14} {:>6.1} MB {:>6} blocks | make_cache(gc) {:>7.1?} | admit {:>8.1?} | poll_view {:>8.1?} | arcs {:>7.1?} | tasks {:>7.1?}",
            &id[..12.min(id.len())], mb, n, make_ms, a, pl, ar, tasks_ms
        );
        // The other half of a switch: building the View and laying it out at a real width.
        let blocks = {
            let sess = c.touch(id).unwrap();
            let mut b = sess.committed_arcs();
            b.extend(
                c.poll_view(id, ArcLog::memory)
                    .and_then(|r| r.ok())
                    .map(|d| d.provisional)
                    .unwrap_or_default(),
            );
            b
        };
        let t = Instant::now();
        let mut v = claude_replay_tui::view::View::new_shared(
            blocks,
            id,
            true,
            claude_replay_core::fold::FoldPolicy::from_flags(false, None, None),
        );
        let new_ms = t.elapsed();
        let t = Instant::now();
        {
            use ratatui::{backend::TestBackend, Terminal};
            let mut term = Terminal::new(TestBackend::new(160, 48)).unwrap();
            term.draw(|f| v.draw(f)).unwrap();
        }
        let draw_ms = t.elapsed();
        println!(
            "               └─ View::new_shared {new_ms:>8.1?} | first draw/layout {draw_ms:>8.1?}"
        );
        let _ = tasks;
        c.release_all();
    }
    let _ = std::fs::remove_dir_all(&root);
}
