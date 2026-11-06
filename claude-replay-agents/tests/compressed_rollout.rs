//! A Codex rollout compressed to `rollout-….jsonl.zst` reads as the JSONL it stands for (#s383).
//! Codex compresses a rollout once it has gone seven days untouched, keeps its mtime, and
//! decompresses it back to plain `.jsonl` when the thread is resumed (codex-rs
//! `rollout/src/compression.rs`). Every reader works in the JSONL's byte offsets, so each case
//! holds a compressed copy to the plain one: the fold, the follower, the metrics cursor and the
//! discovery facts, and a file that changes form under a reader that holds it.
//!
//! The rollout is hand-written; the compressed copies are made by the engine's own encoder.

use claude_replay_agents::CodexAdapter;
use claude_replay_core::{parse_session_as, Agent};
use claude_replay_engine::metrics_fold::{FoldStart, MetricsFold};
use claude_replay_engine::{discover, source, FollowParser};
use std::path::{Path, PathBuf};

/// A main-thread rollout with every kind of line a reader keeps: the head, a prompt, a call and
/// its output, cumulative token counts across a model switch, and an answer.
const LINES: &[&str] = &[
    r#"{"timestamp":"2026-10-01T09:00:00.000Z","type":"session_meta","payload":{"id":"0193a5c0-0000-7000-8000-00000000s383","cwd":"/repo","originator":"codex_cli_rs","source":"cli","cli_version":"0.167.0"}}"#,
    r#"{"timestamp":"2026-10-01T09:00:01.000Z","type":"turn_context","payload":{"cwd":"/repo","model":"gpt-5.6"}}"#,
    r#"{"timestamp":"2026-10-01T09:00:02.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"List the crates"}]}}"#,
    r#"{"timestamp":"2026-10-01T09:00:03.000Z","type":"response_item","payload":{"type":"function_call","name":"shell","call_id":"call_1","arguments":"{\"command\":[\"ls\"],\"workdir\":\"/repo\"}"}}"#,
    r#"{"timestamp":"2026-10-01T09:00:04.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_1","output":"{\"output\":\"engine\\nagents\\n\",\"metadata\":{\"exit_code\":0,\"duration_seconds\":0.1}}"}}"#,
    r#"{"timestamp":"2026-10-01T09:00:05.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1200,"cached_input_tokens":200,"output_tokens":80}}}}"#,
    r#"{"timestamp":"2026-10-01T09:00:06.000Z","type":"turn_context","payload":{"cwd":"/repo/engine","model":"gpt-5.6-mini"}}"#,
    r#"{"timestamp":"2026-10-01T09:00:07.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":2000,"cached_input_tokens":500,"output_tokens":150}}}}"#,
    r#"{"timestamp":"2026-10-01T09:00:08.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Two crates: engine and agents."}]}}"#,
];

const NAME: &str = "rollout-2026-10-01T09-00-00-0193a5c0-0000-7000-8000-00000000s383.jsonl";

fn scratch(case: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cr-zst-{case}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn body(lines: &[&str]) -> String {
    lines.iter().map(|l| format!("{l}\n")).collect()
}

/// The same rollout twice, under the same name: plain in `plain/`, compressed in `zst/`.
fn both(dir: &Path) -> (PathBuf, PathBuf) {
    let plain = dir.join("plain").join(NAME);
    std::fs::create_dir_all(plain.parent().unwrap()).unwrap();
    std::fs::write(&plain, body(LINES)).unwrap();
    let copy = dir.join("zst").join(NAME);
    std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
    std::fs::copy(&plain, &copy).unwrap();
    let z = source::compress_in_place_for_tests(&copy).unwrap();
    assert!(z.to_str().unwrap().ends_with(".jsonl.zst") && !copy.exists());
    (plain, z)
}

/// Blocks, per-turn times, metrics and the discovery facts of a compressed rollout are those of
/// the plain one; the old readers saw zstd bytes, which hold no line of JSON.
#[test]
fn a_compressed_rollout_folds_and_is_described_as_its_jsonl() {
    let dir = scratch("fold");
    let (plain, z) = both(&dir);

    let p = parse_session_as(Agent::CODEX, &plain).unwrap();
    let c = parse_session_as(Agent::CODEX, &z).unwrap();
    assert!(!p.blocks().is_empty(), "the fixture folds to blocks");
    assert_eq!(format!("{:?}", c.blocks()), format!("{:?}", p.blocks()));
    assert_eq!(c.user_times, p.user_times);
    assert!(p.metrics.cost_usd.is_some_and(|usd| usd > 0.0), "priced");
    assert_eq!(c.metrics, p.metrics);

    assert_eq!(
        discover::session_id(&z).as_deref(),
        Some("0193a5c0-0000-7000-8000-00000000s383")
    );
    assert_eq!(discover::first_cwd(&z), discover::first_cwd(&plain));
    assert!(discover::latest_cwd(&z).is_some());
    assert_eq!(discover::latest_cwd(&z), discover::latest_cwd(&plain));
    assert_eq!(
        source::stem(&z),
        Some(NAME.trim_end_matches(".jsonl")),
        "one session id in either form"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A follower on a compressed rollout reads it whole once and then idles: the decoded length,
/// not the smaller file's, is what its cursor is held to, or every poll read as a truncation and
/// folded the session again.
#[test]
fn a_follower_on_a_compressed_rollout_reads_it_once_and_then_idles() {
    let dir = scratch("follow");
    let (plain, z) = both(&dir);
    let want = parse_session_as(Agent::CODEX, &plain).unwrap();

    let mut follow = FollowParser::open(&CodexAdapter, &z);
    let (blocks, _, metrics) = follow
        .poll()
        .unwrap()
        .expect("the first poll reads the file");
    assert_eq!(format!("{blocks:?}"), format!("{:?}", want.blocks()));
    assert_eq!(metrics, want.metrics);
    for _ in 0..3 {
        assert!(follow.poll().unwrap().is_none(), "a .zst never grows: idle");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Codex compresses a rollout under a reader that holds its plain path, and decompresses it under
/// one that holds the `.zst`: the follower keeps its session and its cursor either way. The old
/// follower read the vanished plain file as empty, reset, and showed nothing.
#[test]
fn a_rollout_that_changes_form_under_a_follower_keeps_its_session() {
    let dir = scratch("flip");
    let (plain, _) = both(&dir);
    // A pasted image the follower elides (Codex's `image_url`, over the scan threshold): the
    // reader that reopens counts its elisions afresh, which must not underflow the banked ones.
    let image = format!(
        r#"{{"timestamp":"2026-10-01T09:00:02.500Z","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_image","image_url":"data:image/png;base64,{}"}}]}}}}"#,
        "A".repeat(300 * 1024)
    );
    let mut lines: Vec<&str> = LINES.to_vec();
    lines.insert(3, &image);
    std::fs::write(&plain, body(&lines)).unwrap();
    let want = format!(
        "{:?}",
        parse_session_as(Agent::CODEX, &plain).unwrap().blocks()
    );

    let mut follow = FollowParser::open(&CodexAdapter, &plain);
    let (blocks, _, _) = follow.poll().unwrap().expect("read");
    assert_eq!(format!("{blocks:?}"), want);
    let z = source::compress_in_place_for_tests(&plain).unwrap();
    assert!(
        follow.poll().unwrap().is_none(),
        "compressed: no reset, nothing new"
    );

    // A fresh follower on the plain path (as a stale row holds it) reads the .zst.
    let mut late = FollowParser::open(&CodexAdapter, &plain);
    let (blocks, _, _) = late.poll().unwrap().expect("read through the other form");
    assert_eq!(format!("{blocks:?}"), want);

    // Resumed: Codex writes the plain file back (and appends), removing the .zst; a follower
    // holding the .zst path goes on from its cursor.
    let mut on_z = FollowParser::open(&CodexAdapter, &z);
    assert!(on_z.poll().unwrap().is_some());
    std::fs::write(&plain, body(&lines)).unwrap();
    std::fs::remove_file(&z).unwrap();
    assert!(
        on_z.poll().unwrap().is_none(),
        "decompressed, unchanged: idle"
    );
    let more = r#"{"timestamp":"2026-10-08T09:00:00.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"And the tests?"}]}}"#;
    std::fs::write(&plain, body(&lines) + more + "\n").unwrap();
    let (blocks, _, _) = on_z.poll().unwrap().expect("the appended prompt");
    assert!(format!("{blocks:?}").contains("And the tests?"));
    // The first follower held the plain file Codex removed when it compressed it: it reads the
    // file written back, from its cursor, not the removed one for ever.
    let (blocks, _, _) = follow
        .poll()
        .unwrap()
        .expect("the first follower sees it too");
    assert!(format!("{blocks:?}").contains("And the tests?"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A metrics cursor banked while the rollout was plain resumes once it is compressed — its offset
/// and the CRC window below it are the JSONL's — and folds to what a cold fold gives.
#[test]
fn a_metrics_cursor_from_the_plain_rollout_resumes_on_the_compressed_one() {
    let dir = scratch("cursor");
    let (plain, z) = both(&dir);
    let cold = {
        let mut f = MetricsFold::open(&CodexAdapter, &plain, None).unwrap();
        while f.next_event().unwrap().is_some() {}
        f.metrics()
    };
    let prefix = dir.join("prefix").join(NAME);
    std::fs::create_dir_all(prefix.parent().unwrap()).unwrap();
    std::fs::write(&prefix, body(&LINES[..6])).unwrap();
    let cursor = {
        let mut f = MetricsFold::open(&CodexAdapter, &prefix, None).unwrap();
        while f.next_event().unwrap().is_some() {}
        f.cursor().unwrap()
    };

    let mut resumed = MetricsFold::open(&CodexAdapter, &z, Some(&cursor)).unwrap();
    assert_eq!(resumed.start(), FoldStart::Resumed);
    while resumed.next_event().unwrap().is_some() {}
    assert_eq!(resumed.metrics(), cold);
    assert_eq!(
        resumed.offset(),
        source::len(&z).unwrap(),
        "folded to the JSONL's end"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
