//! `agent-replay --dump - --json --cache` through the real binary (#10): the flag's contract as a
//! shell-out consumer meets it. The stream is the plain dump's bytes; the plain dump still writes
//! nothing and takes no lock; and the flag is refused where it would mean nothing. The cache home
//! is the case's scratch, so no entry on this machine is read, written or reclaimed.

use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cr-dump-cache-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `agent-replay <args>`, its cache home under `root`.
fn run(root: &Path, args: &[&str]) -> (Vec<u8>, String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_agent-replay"))
        .args(args)
        .env("CLAUDE_REPLAY_CACHE", root.join("cache"))
        .output()
        .unwrap();
    (
        out.stdout,
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

/// A short working session: two turns, each a prompt, a tool call, its result and an answer.
fn transcript(dir: &Path) -> PathBuf {
    let path = dir.join("0f0e0d0c-0000-4000-8000-000000000010.jsonl");
    let mut body = String::new();
    for t in 0..2 {
        body.push_str(&format!(
            concat!(
                r#"{{"type":"user","cwd":"/w","timestamp":"2026-09-26T10:0{t}:00Z","message":{{"role":"user","content":[{{"type":"text","text":"turn {t}"}}]}}}}"#,
                "\n",
                r#"{{"type":"assistant","timestamp":"2026-09-26T10:0{t}:01Z","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"t{t}","name":"Bash","input":{{"command":"ls"}}}}]}}}}"#,
                "\n",
                r#"{{"type":"user","timestamp":"2026-09-26T10:0{t}:02Z","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t{t}","content":"a.rs"}}]}}}}"#,
                "\n",
                r#"{{"type":"assistant","timestamp":"2026-09-26T10:0{t}:03Z","message":{{"role":"assistant","content":[{{"type":"text","text":"answer {t}"}}]}}}}"#,
                "\n",
            ),
            t = t
        ));
    }
    std::fs::write(&path, body).unwrap();
    path
}

/// The plain dump writes nothing — not an entry, not the cache home itself — which is what lets a
/// sweep run beside live viewers. `--cache` writes the entry, and both write the same bytes, the
/// second time from the entry too.
#[test]
fn the_cached_stream_is_the_plain_one_and_only_the_flag_writes() {
    let dir = scratch("stream");
    let path = transcript(&dir);
    let p = path.to_str().unwrap();

    let (plain, err, ok) = run(&dir, &["--dump", "-", "--json", p]);
    assert!(ok, "{err}");
    assert!(!plain.is_empty());
    assert!(
        !dir.join("cache").exists(),
        "a plain dump wrote under the cache home"
    );

    let entry = dir.join("cache/sessions/json/0f0e0d0c-0000-4000-8000-000000000010");
    for run_no in 1..=2 {
        let (cached, err, ok) = run(&dir, &["--dump", "-", "--json", "--cache", p]);
        assert!(ok, "{err}");
        assert_eq!(
            String::from_utf8_lossy(&cached),
            String::from_utf8_lossy(&plain),
            "cached dump {run_no}"
        );
        assert!(entry.join("blocks.jsonl").is_file(), "the entry's blocks");
        assert!(
            entry.join("meta.jsonl").is_file(),
            "the entry's meta stream"
        );
        assert!(!entry.join("LOCK").exists(), "the lock is given back");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The flag is refused where it would do nothing, rather than ignored — and never combined with
/// the one that says the opposite.
#[test]
fn the_flag_goes_with_a_json_dump_only() {
    let dir = scratch("refused");
    let path = transcript(&dir);
    let p = path.to_str().unwrap();
    for args in [
        vec!["--dump", "-", "--cache", p],
        vec!["--paths", "--cache", p],
        vec!["--dump", "-", "--json", "--cache", "--no-cache", p],
    ] {
        let (out, err, ok) = run(&dir, &args);
        assert!(
            !ok,
            "{args:?} was accepted: {}",
            String::from_utf8_lossy(&out)
        );
        assert!(
            err.contains("--cache") || err.contains("--no-cache"),
            "{args:?}: {err}"
        );
    }
    assert!(!dir.join("cache").exists(), "a refused run wrote nothing");
    let _ = std::fs::remove_dir_all(&dir);
}

/// #s500: a reply the client withdrew for a broken tool call folds under its mark on the pages
/// and in the text dump, and the `--json` stream states it as before the mark: byte for byte the
/// stream of the same transcript with no `retractedMessageUuids` on its note (a client before
/// 2.1.325). Through both parse paths (the plain dump's whole parse, the cached dump's fold) and
/// a fold resumed at every line. Shapes where the withdrawn thinking joined the run before it
/// (Claude's shaping merges thinking into a Bash, Read or Edit run) are the ones that differed.
#[test]
fn a_withdrawn_reply_streams_as_before_the_mark_on_every_path() {
    use serde_json::{json, Value};
    let ts = |s: u32| format!("2026-11-14T17:00:{s:02}.000Z");
    let user = |uuid: &str, s: u32, content: Value| json!({"type":"user","cwd":"/w","uuid":uuid,"timestamp":ts(s),"message":{"role":"user","content":content}});
    let asst = |uuid: &str, s: u32, msg: &str, stop: &str, content: Value| json!({"type":"assistant","uuid":uuid,"timestamp":ts(s),"message":{"id":msg,"role":"assistant","stop_reason":stop,"content":content}});
    let think = |t: &str| json!([{"type":"thinking","thinking":t,"signature":"x"}]);
    let bash = |id: &str, cmd: &str| json!([{"type":"tool_use","id":id,"name":"Bash","input":{"command":cmd}}]);
    let edit = |id: &str| json!([{"type":"tool_use","id":id,"name":"Edit","input":{"file_path":"/w/nests.md","old_string":"a","new_string":"b"}}]);
    let result =
        |id: &str, out: &str| json!([{"type":"tool_result","tool_use_id":id,"content":out}]);
    let note = |ids: &[&str], marked: bool| {
        let mut n = json!({"type":"user","uuid":"n-1","timestamp":ts(30),"isMeta":true,"turnCompanion":true,
            "retractedMessageUuids":ids,
            "message":{"role":"user","content":"The previous response failed to produce a valid tool call. Please retry the tool call now."}});
        if !marked {
            n.as_object_mut().unwrap().remove("retractedMessageUuids");
        }
        n
    };
    let prompt = || user("u-1", 0, json!("count the nests"));
    let retry = || {
        vec![
            asst(
                "r-1",
                31,
                "msg-r",
                "tool_use",
                bash("toolu_r", "wc -l nests.md"),
            ),
            user("t-r", 32, result("toolu_r", "40 nests.md")),
            asst(
                "e-1",
                33,
                "msg-e",
                "end_turn",
                json!([{"type":"text","text":"Forty nests."}]),
            ),
        ]
    };
    let bash_run = || {
        vec![
            asst(
                "a-1",
                1,
                "msg-a",
                "tool_use",
                think("First list the files."),
            ),
            asst("a-2", 2, "msg-a", "tool_use", bash("toolu_a", "ls")),
            user("t-a", 3, result("toolu_a", "nests.md")),
        ]
    };
    type Shape = (&'static str, Vec<Value>, Vec<&'static str>);
    let shapes: Vec<Shape> = vec![
        (
            "straight after the prompt",
            vec![
                asst(
                    "w-1",
                    10,
                    "msg-w",
                    "tool_use",
                    think("The count needs the file."),
                ),
                asst(
                    "w-2",
                    11,
                    "msg-w",
                    "tool_use",
                    json!([{"type":"text","text":"Counting the nests now."}]),
                ),
            ],
            vec!["w-1", "w-2"],
        ),
        (
            "two thinkings straight after the prompt",
            vec![
                asst("w-1", 10, "msg-w", "tool_use", think("Find the log.")),
                asst(
                    "w-2",
                    11,
                    "msg-w",
                    "tool_use",
                    think("Then count its lines."),
                ),
            ],
            vec!["w-1", "w-2"],
        ),
        (
            "thinking after a Bash run",
            [
                bash_run(),
                vec![asst(
                    "w-1",
                    10,
                    "msg-w",
                    "tool_use",
                    think("Now count the lines."),
                )],
            ]
            .concat(),
            vec!["w-1"],
        ),
        (
            "thinking after a run that ended on an image",
            vec![
                asst("a-1", 1, "msg-a", "tool_use", think("Take a look.")),
                asst("a-2", 2, "msg-a", "tool_use", bash("toolu_a", "shot")),
                user(
                    "t-a",
                    3,
                    json!([{"type":"tool_result","tool_use_id":"toolu_a","content":"saved"},
                        {"type":"image","source":{"type":"base64","media_type":"image/png","data":"iVBORw0KGgo="}}]),
                ),
                asst("w-1", 10, "msg-w", "tool_use", think("It shows the nests.")),
            ],
            vec!["w-1"],
        ),
        (
            "thinking after two edits",
            vec![
                asst("a-1", 1, "msg-a", "tool_use", edit("toolu_e1")),
                user("t-1", 2, result("toolu_e1", "ok")),
                asst("a-2", 3, "msg-b", "tool_use", edit("toolu_e2")),
                user("t-2", 4, result("toolu_e2", "ok")),
                asst("w-1", 10, "msg-w", "tool_use", think("Both changed.")),
            ],
            vec!["w-1"],
        ),
        (
            "thinking, prose and thinking after a Bash run",
            [
                bash_run(),
                vec![
                    asst("w-1", 10, "msg-w", "tool_use", think("Now count.")),
                    asst(
                        "w-2",
                        11,
                        "msg-w",
                        "tool_use",
                        json!([{"type":"text","text":"Counting now."}]),
                    ),
                    asst("w-3", 12, "msg-w", "tool_use", think("With wc.")),
                ],
            ]
            .concat(),
            vec!["w-1", "w-2", "w-3"],
        ),
        (
            "redacted thinking after a Bash run",
            [
                bash_run(),
                vec![asst(
                    "w-1",
                    10,
                    "msg-w",
                    "tool_use",
                    json!([{"type":"redacted_thinking","data":"x"}]),
                )],
            ]
            .concat(),
            vec!["w-1"],
        ),
    ];
    let dir = scratch("withdrawn");
    for (n, (shape, withdrawn, ids)) in shapes.into_iter().enumerate() {
        let lines = |marked: bool| -> Vec<String> {
            [
                vec![prompt()],
                withdrawn.clone(),
                vec![note(&ids, marked)],
                retry(),
            ]
            .concat()
            .iter()
            .map(|v| v.to_string() + "\n")
            .collect()
        };
        let file = |k: usize, body: &[String]| -> PathBuf {
            let p = dir.join(format!(
                "0f0e0d0c-0000-4000-8000-00000000{n:02}{k:02}.jsonl"
            ));
            std::fs::write(&p, body.concat()).unwrap();
            p
        };
        let (marked, plain) = (lines(true), lines(false));
        let mp = file(1, &marked);
        let pp = file(2, &plain);
        let (m, p) = (mp.to_str().unwrap(), pp.to_str().unwrap());

        // The premise: the note naming the reply folds it under a mark; without the key it is the
        // result line it always was.
        let (mtext, err, ok) = run(&dir, &["--dump", "-", m]);
        assert!(ok, "{shape}: {err}");
        let (ptext, err, ok) = run(&dir, &["--dump", "-", p]);
        assert!(ok, "{shape}: {err}");
        let mark = "Retried: the reply gave no usable tool call";
        assert!(
            String::from_utf8_lossy(&mtext).contains(mark)
                && !String::from_utf8_lossy(&ptext).contains(mark),
            "{shape}: premise, only the marked transcript draws the mark:\n{}",
            String::from_utf8_lossy(&mtext)
        );

        let (want, err, ok) = run(&dir, &["--dump", "-", "--json", p]);
        assert!(ok, "{shape}: {err}");
        let want = String::from_utf8(want).unwrap();
        let (got, err, ok) = run(&dir, &["--dump", "-", "--json", m]);
        assert!(ok, "{shape}: {err}");
        assert_eq!(
            String::from_utf8(got).unwrap(),
            want,
            "{shape}: the whole parse"
        );

        // The fold, cold and then resumed after each line: a cache home of its own each time, the
        // file grown to the whole transcript between the two dumps.
        for cut in 0..=marked.len() {
            let home = dir.join(format!("home-{n}-{cut}"));
            if cut > 0 {
                std::fs::write(&mp, marked[..cut].concat()).unwrap();
                let (_, err, ok) = run(&home, &["--dump", "-", "--json", "--cache", m]);
                assert!(ok, "{shape}, first {cut} lines: {err}");
            }
            std::fs::write(&mp, marked.concat()).unwrap();
            let (got, err, ok) = run(&home, &["--dump", "-", "--json", "--cache", m]);
            assert!(ok, "{shape}, resumed after {cut} lines: {err}");
            assert_eq!(
                String::from_utf8(got).unwrap(),
                want,
                "{shape}: the fold, resumed after {cut} of {} lines",
                marked.len()
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
