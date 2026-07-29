//! Offline HTML bundles: the `--dump-html` single self-contained file and the
//! `--dump-all-html` directory bundle (one `<id>.jsonl` per sub-agent, cross-linked).
//! Thin orchestration — parse each transcript via the library `parse_session*`, render
//! through `super`'s block/stream helpers, and write the files.

use super::{
    block_lines, build_html, build_shell, child_info, display_title, render_agent_stream,
    render_snapshot, serve, session_id, AgentInfo, AssetSink, ChildRef,
};
use crate::fold::FoldPolicy;
use crate::{discover, Args};
use anyhow::{Context, Result};
use std::path::Path;

/// The whole append-only stream for `path` right now: the `meta` line followed by
/// one line per block. Re-run each poll cycle in live mode; the loop appends only
/// the lines that are new since the previous cycle.
fn build_stream(
    transcript: &crate::Transcript,
    fold: &FoldPolicy,
    reveal: bool,
) -> Result<(String, Vec<(String, String)>)> {
    // One parse yields blocks + per-turn times + metrics + cwd (design §3.3 / Phase 4).
    let s = transcript
        .parse()
        .with_context(|| format!("read transcript {}", transcript.path().display()))?;
    let cwd = s
        .cwd
        .clone()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let blocks = s.blocks();
    let tasks = crate::engine::tasks::merged(
        &s.tasks,
        crate::discover::session_tasks(transcript.agent(), transcript.path()),
    );
    Ok(render_snapshot(
        transcript.agent(),
        transcript.path(),
        &blocks,
        &s.user_times,
        &s.metrics,
        &cwd,
        fold,
        reveal,
        &tasks,
    ))
}

/// Entry point for `--dump-html`. Writes a shareable file → no reveal-in-Finder
/// path links (their absolute `file://` paths don't resolve on another machine).
pub fn dump_html(args: &Args, path: &Path) -> Result<()> {
    let agent = discover::detect_agent(path);
    let fold = FoldPolicy::from_args(args);
    let reveal = false;
    let transcript = crate::Transcript::open(agent, path);
    let (jsonl, turns) = build_stream(&transcript, &fold, reveal)?;
    // The page title identifies the session in a browser tab; files are named by session id.
    let title = display_title(agent, path);

    // `--dump-html -` streams the page to stdout (pipes / tests); never live.
    let stem = match args.dump_html.as_ref().and_then(|o| o.as_deref()) {
        Some("-") => {
            print!("{}", build_html(&title, &jsonl, &turns, None));
            return Ok(());
        }
        Some(s) => s.to_string(),
        None => crate::tui::app::deduce_stem(path, None),
    };

    // Live: the page renders the inline snapshot immediately, then polls the
    // companion for appended lines — so it works standalone *and* keeps up. The
    // page references the companion by **basename** (same directory as the .html),
    // so `fetch` resolves it relative to the page's own URL.
    let companion = if args.follow {
        let cpath = format!("{stem}.jsonl");
        std::fs::write(&cpath, format!("{jsonl}\n")).with_context(|| format!("write {cpath}"))?;
        let src = Path::new(&cpath)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&cpath)
            .to_string();
        Some((cpath, src))
    } else {
        None
    };
    let html_path = format!("{stem}.html");
    std::fs::write(
        &html_path,
        build_html(
            &title,
            &jsonl,
            &turns,
            companion.as_ref().map(|(_, s)| s.as_str()),
        ),
    )
    .with_context(|| format!("write {html_path}"))?;

    let Some((cpath, _)) = companion else {
        eprintln!("wrote {html_path}");
        println!("{stem}");
        return Ok(());
    };

    // Live tail: poll the transcript, appending any block lines that appeared
    // since the last cycle. Runs until interrupted (like `claude-replay -f`).
    eprintln!("wrote {html_path} + {cpath} (live — open it and it follows; Ctrl-C to stop)");
    println!("{stem}");
    serve::follow_and_append(
        transcript,
        &fold,
        Path::new(&cpath),
        block_lines(&jsonl),
        reveal,
    )
}

/// Parse ONE agent's source transcript (NOT the whole tree) into its stream jsonl: the
/// `meta` line + one line per block, cross-linked to its direct children via `child:`.
/// The single generator both paths share — the offline dump (eager over every source) and
/// the live server (lazy, one source per *requested* agent) — so a live tailer re-parses
/// only the ONE agent being viewed, not the tree. Returns the jsonl + the direct child
/// refs (to register/queue). `cwd` is the session cwd (shared by every agent).
fn agent_stream(
    operation: &crate::Transcript,
    fold: &FoldPolicy,
    cwd: &str,
    reveal: bool,
    info: &AgentInfo,
    assets: Option<&mut AssetSink>,
) -> Result<(String, Vec<ChildRef>)> {
    // Parse via the canonical `parse_session_as` — the same entry `build_stream` uses, so both
    // HTML paths go through one place. `cwd` stays the caller-supplied session cwd (every agent
    // in a tree shares the root's, which a sub-agent transcript may not itself record).
    let transcript = operation.related(&info.source);
    let s = transcript
        .parse()
        .with_context(|| format!("read transcript {}", info.source.display()))?;
    let blocks = s.blocks();
    // The task panel's state (#15): op-log from the transcript, overlaid by the live
    // task files when they still exist — so an offline dump carries the task state.
    let tasks = crate::engine::tasks::merged(
        &s.tasks,
        crate::discover::session_tasks(transcript.agent(), transcript.path()),
    );
    Ok(render_agent_stream(
        operation.agent(),
        fold,
        cwd,
        reveal,
        info,
        &blocks,
        &s.user_times,
        &s.metrics,
        &tasks,
        assets,
    ))
}

/// `--dump-all-html`: write an offline **directory bundle** — a shared `index.html` shell
/// plus one `<id>.jsonl` per agent reachable from the root, cross-linked via `child:` so
/// the whole tree is navigable offline. Serve the dir with any static file server. Unlike
/// the lazy served path, this walks EVERY reachable agent eagerly (blocking is fine for a
/// one-shot export) and materializes embedded attachments into `assets/`.
pub fn dump_all_html(args: &Args, path: &Path) -> Result<()> {
    let agent = discover::detect_agent(path);
    let fold = FoldPolicy::from_args(args);
    let out_dir = match args.dump_all_html.as_ref().and_then(|o| o.as_deref()) {
        Some(s) => std::path::PathBuf::from(s),
        None => std::path::PathBuf::from(crate::tui::app::deduce_stem(path, None)),
    };
    std::fs::create_dir_all(&out_dir).with_context(|| format!("create {}", out_dir.display()))?;
    let cwd = discover::session_cwd(path)
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let title = display_title(agent, path);
    let root_id = session_id(path);
    let operation = crate::Transcript::open(agent, path);
    let mut sink = AssetSink::new(&out_dir).with_context(|| "create assets dir")?;

    // BFS over sources from the root; each agent's stream is parsed from its OWN source,
    // its direct children discovered and queued (grandchildren surface when a child runs).
    let mut queue = std::collections::VecDeque::from([AgentInfo {
        id: root_id.clone(),
        source: path.to_path_buf(),
        title: title.clone(),
        agent_type: String::new(),
        ancestors: Vec::new(),
    }]);
    let mut seen = std::collections::HashSet::new();
    let mut count = 0usize;
    while let Some(info) = queue.pop_front() {
        if !seen.insert(info.id.clone()) || !info.source.exists() {
            continue;
        }
        let (jsonl, children) =
            agent_stream(&operation, &fold, &cwd, false, &info, Some(&mut sink))?;
        std::fs::write(
            out_dir.join(format!("{}.jsonl", info.id)),
            format!("{jsonl}\n"),
        )
        .with_context(|| format!("write stream {}", info.id))?;
        count += 1;
        for c in children {
            if let Some(ci) = child_info(&operation, &info, c) {
                queue.push_back(ci);
            }
        }
    }
    std::fs::write(
        out_dir.join("index.html"),
        build_shell(&title, &root_id, false, false),
    )
    .with_context(|| "write index.html")?;

    eprintln!(
        "wrote {} — {count} agent stream(s) + index.html",
        out_dir.display()
    );
    eprintln!(
        "  serve it:  (cd {} && python3 -m http.server)  then open http://localhost:8000/",
        out_dir.display()
    );
    println!("{}", out_dir.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html_export::{CSS, JS};
    use serde_json::{json, Value};
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn codex_subagent_bundle_reuses_shared_navigation_contract() {
        static N: AtomicUsize = AtomicUsize::new(0);
        let base = std::env::temp_dir().join(format!(
            "cr-codex-bundle-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::remove_dir_all(&base).ok();
        let day = base.join("sessions/2026/07/27");
        std::fs::create_dir_all(&day).unwrap();
        let parent = day.join("p.jsonl");
        let child = day.join("c.jsonl");
        let grandchild = day.join("g.jsonl");

        fn meta(
            id: &str,
            parent: Option<&str>,
            path: Option<&str>,
            nickname: Option<&str>,
        ) -> Value {
            let source = parent.map(|parent| {
                json!({
                    "subagent": {
                        "thread_spawn": {
                            "parent_thread_id": parent,
                            "agent_path": path,
                            "agent_nickname": nickname
                        }
                    }
                })
            });
            json!({
                "type": "session_meta",
                "payload": {
                    "id": id,
                    "cwd": "/repo",
                    "source": source.unwrap_or_else(|| json!("cli")),
                    "agent_path": path,
                    "agent_nickname": nickname
                }
            })
        }

        fn append_spawn(file: &mut std::fs::File, call: &str, task: &str) {
            let spawn = json!({
                "type": "response_item",
                "payload": {
                    "type": "function_call",
                    "name": "spawn_agent",
                    "namespace": "collaboration",
                    "call_id": call,
                    "arguments": json!({
                        "task_name": task,
                        "message": "encrypted"
                    }).to_string()
                }
            });
            let output = json!({
                "type": "response_item",
                "payload": {
                    "type": "function_call_output",
                    "call_id": call,
                    "output": json!({ "task_name": format!("/root/{task}") }).to_string()
                }
            });
            writeln!(file, "{spawn}").unwrap();
            writeln!(file, "{output}").unwrap();
        }

        let mut p = std::fs::File::create(&parent).unwrap();
        writeln!(p, "{}", meta("p", None, None, None)).unwrap();
        append_spawn(&mut p, "spawn-c", "review");
        writeln!(
            p,
            "{}",
            json!({
                "type": "response_item",
                "payload": {
                    "type": "agent_message",
                    "author": "/root/review",
                    "content": [{
                        "type": "input_text",
                        "text": "Message Type: FINAL_ANSWER\nPayload:\nPASS"
                    }]
                }
            })
        )
        .unwrap();

        let mut c = std::fs::File::create(&child).unwrap();
        writeln!(
            c,
            "{}",
            meta("c", Some("p"), Some("/root/review"), Some("Hume"))
        )
        .unwrap();
        append_spawn(&mut c, "spawn-g", "audit");

        let mut g = std::fs::File::create(&grandchild).unwrap();
        writeln!(
            g,
            "{}",
            meta("g", Some("c"), Some("/root/review/audit"), Some("Nash"))
        )
        .unwrap();
        writeln!(
            g,
            "{}",
            json!({
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "done"}]
                }
            })
        )
        .unwrap();

        let out = base.join("bundle");
        use clap::Parser as _;
        let args = crate::Args::parse_from([
            "claude-replay",
            parent.to_str().unwrap(),
            "--dump-all-html",
            out.to_str().unwrap(),
        ]);
        dump_all_html(&args, &parent).unwrap();

        assert!(out.join("p.jsonl").is_file());
        assert!(out.join("c.jsonl").is_file());
        assert!(out.join("g.jsonl").is_file());
        let root = std::fs::read_to_string(out.join("p.jsonl")).unwrap();
        let root_records: Vec<Value> = root
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(root_records[0]["children"][0]["id"], json!("c"));
        assert_eq!(root_records[0]["children"][0]["running"], json!(false));
        assert!(root_records
            .iter()
            .any(|record| record["head"]["child"] == json!("?session=c")));

        let child_stream = std::fs::read_to_string(out.join("c.jsonl")).unwrap();
        let child_meta: Value = serde_json::from_str(child_stream.lines().next().unwrap()).unwrap();
        assert_eq!(child_meta["ancestors"][0]["id"], json!("p"));
        assert_eq!(
            child_meta["ancestors"][0]["title"],
            root_records[0]["title"]
        );
        assert_eq!(child_meta["children"][0]["id"], json!("g"));
        assert!(!CSS.to_ascii_lowercase().contains("codex"));
        assert!(!JS.to_ascii_lowercase().contains("codex"));
        std::fs::remove_dir_all(base).ok();
    }
}
