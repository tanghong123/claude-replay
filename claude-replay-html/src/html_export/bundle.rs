//! Offline HTML bundles: the `--dump-html` single self-contained file and the
//! `--dump-all-html` directory bundle (one `<id>.jsonl` per sub-agent, cross-linked).
//! Thin orchestration — parse each transcript via the library `parse_session*`, render
//! through `super`'s block/stream helpers, and write the files.

use super::{
    block_lines, build_html, build_shell, child_info, display_title, render_agent_stream,
    render_snapshot, serve, session_id, AgentInfo, AssetSink, ChildRef,
};
use crate::fold::FoldPolicy;
use crate::{discover, Agent, Args};
use anyhow::{Context, Result};
use std::path::Path;

/// The whole append-only stream for `path` right now: the `meta` line followed by
/// one line per block. Re-run each poll cycle in live mode; the loop appends only
/// the lines that are new since the previous cycle.
fn build_stream(
    agent: Agent,
    path: &Path,
    fold: &FoldPolicy,
    reveal: bool,
) -> Result<(String, Vec<(String, String)>)> {
    // One parse yields blocks + per-turn times + metrics + cwd (design §3.3 / Phase 4).
    let s = crate::parse_session_as(agent, path)
        .with_context(|| format!("read transcript {}", path.display()))?;
    let cwd = s
        .cwd
        .clone()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let blocks = s.blocks();
    let tasks = crate::engine::tasks::merged(&s.tasks, crate::discover::session_tasks(agent, path));
    Ok(render_snapshot(
        agent,
        path,
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
    let fold = args.fold_policy();
    let reveal = false;
    let (jsonl, turns) = build_stream(agent, path, &fold, reveal)?;
    // The page title identifies the session in a browser tab; files are named by session id.
    let title = display_title(agent, path);

    // `--dump-html -` streams the page to stdout (pipes / tests); never live.
    let stem = match args.dump_html.as_ref().and_then(|o| o.as_deref()) {
        Some("-") => {
            print!("{}", build_html(&title, &jsonl, &turns, None));
            return Ok(());
        }
        Some(s) => s.to_string(),
        None => crate::sys::deduce_stem(path, None),
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
        agent,
        path,
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
    agent: Agent,
    fold: &FoldPolicy,
    cwd: &str,
    reveal: bool,
    info: &AgentInfo,
    assets: Option<&mut AssetSink>,
) -> Result<(String, Vec<ChildRef>)> {
    // Parse via the canonical `parse_session_as` — the same entry `build_stream` uses, so both
    // HTML paths go through one place. `cwd` stays the caller-supplied session cwd (every agent
    // in a tree shares the root's, which a sub-agent transcript may not itself record).
    let s = crate::parse_session_as(agent, &info.source)
        .with_context(|| format!("read transcript {}", info.source.display()))?;
    let blocks = s.blocks();
    // The task panel's state (#15): op-log from the transcript, overlaid by the live
    // task files when they still exist — so an offline dump carries the task state.
    let tasks = crate::engine::tasks::merged(
        &s.tasks,
        crate::discover::session_tasks(agent, &info.source),
    );
    let (jsonl, mut children) = render_agent_stream(
        agent,
        fold,
        cwd,
        reveal,
        info,
        &blocks,
        &s.user_times,
        &s.metrics,
        &tasks,
        assets,
    );
    for child in &mut children {
        child.source = s
            .sub_agents
            .get(&child.id)
            .and_then(|meta| meta.transcript.clone());
    }
    Ok((jsonl, children))
}

/// `--dump-all-html`: write an offline **directory bundle** — a shared `index.html` shell
/// plus one `<id>.jsonl` per agent reachable from the root, cross-linked via `child:` so
/// the whole tree is navigable offline. Serve the dir with any static file server.
///
/// **Completeness contract (#64):** every EMBEDDED, non-inline-rendered object in the
/// transcripts — prompt images, tool-result images, `file` attachments,
/// `plan_file_reference` plans, ExitPlanMode plans (any `AttachmentContent::Deferred`)
/// — materializes into `assets/` with an `att_href` link on its record (pinned by
/// `bundle_materializes_every_embedded_object`). Path-only attachments (e.g.
/// `edited_text_file`) carry no embedded bytes — nothing to dump. The portable
/// single-file `--dump-html` deliberately stays name-only (no embedded objects).
///
/// Unlike
/// the lazy served path, this walks EVERY reachable agent eagerly (blocking is fine for a
/// one-shot export) and materializes embedded attachments into `assets/`.
pub fn dump_all_html(args: &Args, path: &Path) -> Result<()> {
    let agent = discover::detect_agent(path);
    let fold = args.fold_policy();
    let out_dir = match args.dump_all_html.as_ref().and_then(|o| o.as_deref()) {
        Some(s) => std::path::PathBuf::from(s),
        None => std::path::PathBuf::from(crate::sys::deduce_stem(path, None)),
    };
    std::fs::create_dir_all(&out_dir).with_context(|| format!("create {}", out_dir.display()))?;
    let cwd = discover::session_cwd(path)
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let title = display_title(agent, path);
    let root_id = session_id(path);
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
        let (jsonl, children) = agent_stream(agent, &fold, &cwd, false, &info, Some(&mut sink))?;
        std::fs::write(
            out_dir.join(format!("{}.jsonl", info.id)),
            format!("{jsonl}\n"),
        )
        .with_context(|| format!("write stream {}", info.id))?;
        count += 1;
        for c in children {
            if let Some(ci) = child_info(agent, path, &info, c) {
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
