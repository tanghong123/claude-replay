//! agent-replay library — the thin assembly crate for the `agent-replay` viewer and the
//! `agent-jdi` supervisor binaries.
//!
//! The layers live in sibling crates (#71): `claude-replay-core` (parser/replay engine),
//! `claude-replay-present` (cache + shared presentation helpers + `Args`),
//! `claude-replay-tui` and `claude-replay-html` (the two frontends). This crate re-exports
//! them under their long-standing module paths (so `claude_replay::model`,
//! `claude_replay::tui::app`, … keep working), owns the CLI entry point, and hosts `jdi`.

pub mod jdi;

pub use claude_replay_html::html_export;
pub use claude_replay_tui as tui;

pub use claude_replay_core::{
    claude_discover, codex_discover, diff, discover, engine, fold, follow, metrics, model,
    parse_session_as, parse_session_enriched_as, summary, Agent, Transcript,
};
pub use claude_replay_present::{cache, highlight, present, sys, Args, SessionCache};

use anyhow::Result;
use clap::Parser;

/// Entry point for the `agent-replay` viewer binary.
pub fn run_viewer() -> Result<()> {
    let args = Args::parse();
    // Take back what dead runs left behind (#165), on EVERY invocation — the frontends each do
    // this when they build a cache, which leaves the `--dump*` modes never reclaiming anything.
    // A machine used only for dumps would keep the leftovers forever, and those are exactly the
    // ones nobody is watching.
    sys::reclaim();
    // `--paths`: not a viewer at all — a shell-out entry to the `discover` path vocabulary
    // (for tools that can't link the crate, e.g. a Python collector). Resolve the same way the
    // viewer does, print the directory facts as JSON, and exit.
    if args.paths {
        return print_session_paths(&args);
    }
    // `--html`: open a browser instead of the TUI, but with the SAME session
    // selection as the terminal viewer — an explicit id/path or `--latest` resolves
    // directly (cwd-scoped for `--latest`); otherwise show the picker (like a bare
    // `-f`), so `-f --html` prompts when this dir has several sessions.
    if args.html {
        if args.target.is_some() || args.latest {
            let path = discover::resolve_any(args.agent, args.target.as_deref(), args.latest)?;
            return html_export::serve(&args, &path);
        }
        // No explicit target: the picker chooses. With SEVERAL matches, serve them ALL at
        // once and stay on the picker — each pick opens that session's browser tab while
        // the list remains, so there is a way back (the TUI has always had one). A single
        // match keeps the original direct-serve path untouched.
        let cands = discover::candidates_all(args.agent);
        if cands.is_empty() {
            anyhow::bail!("no transcripts found for any agent in this directory");
        }
        if cands.len() == 1 {
            return html_export::serve(&args, &cands[0].path);
        }
        let paths: Vec<std::path::PathBuf> = cands.iter().map(|c| c.path.clone()).collect();
        let server = html_export::start_server(&args, &paths)?;
        let by_path: std::collections::HashMap<_, _> = paths
            .iter()
            .cloned()
            .zip(server.root_ids.iter().cloned())
            .collect();
        let status = format!(
            "serving {} sessions at 127.0.0.1:{} — Enter/click opens a tab, Esc quits",
            server.root_ids.len(),
            server.port
        );
        tui::app::pick_session_loop(cands, &status, &mut |path| {
            if let Some(sid) = by_path.get(path) {
                server.open(sid);
            }
        })?;
        return Ok(());
    }
    // No id/path/--latest and not dumping → interactive picker ↔ viewer flow. The
    // picker merges sessions from every agent (filtered by --agent) for this dir.
    if args.target.is_none()
        && !args.latest
        && args.dump.is_none()
        && args.dump_html.is_none()
        && args.dump_all_html.is_none()
    {
        return tui::app::run_interactive(&args);
    }
    // Explicit path / session id / --latest: resolve across agents (honoring the
    // --agent filter). The agent for each opened file is auto-detected downstream.
    let path = discover::resolve_any(args.agent, args.target.as_deref(), args.latest)?;
    if args.dump_all_html.is_some() {
        html_export::dump_all_html(&args, &path)
    } else if args.dump_html.is_some() {
        html_export::dump_html(&args, &path)
    } else if args.dump.is_some() {
        if args.json {
            dump_json(&args, &path)
        } else {
            tui::app::dump(&args, &path)
        }
    } else {
        tui::app::run(&args, &path)
    }
}

/// `--dump --json` (#34): emit the structured block stream and exit — the CONTENT half of
/// the shell-out vocabulary (`--paths --all` is the discovery half). One JSON object per
/// block from the same normalized stream the text dump renders; the emission itself lives
/// in [`claude_replay_core::block_json`] beside the vocabulary it projects. `--dump -`
/// streams to stdout; with a stem (given or deduced), writes `<stem>.json` and prints the
/// stem last for scripting, mirroring the text dump's contract.
fn dump_json(args: &Args, path: &std::path::Path) -> Result<()> {
    let agent = claude_replay_core::discover::detect_agent(path);
    // The flat parse: top-level blocks are identical to the enriched one's — a `SubAgent`
    // emits spawn facts and its `agent_id`, and the child transcript is its own session
    // (discoverable via `--paths --all`), not an inline sub-stream.
    let session = claude_replay_core::parse_session_as(agent, path)?;
    match args.dump.as_ref().and_then(|o| o.as_deref()) {
        Some("-") => {
            let out = std::io::stdout();
            claude_replay_core::block_json::write_block_stream(&session, &mut out.lock())?;
        }
        stem => {
            let stem = match stem {
                Some(s) => s.to_string(),
                None => claude_replay_present::sys::deduce_stem(path, None),
            };
            let mut f = std::io::BufWriter::new(std::fs::File::create(format!("{stem}.json"))?);
            claude_replay_core::block_json::write_block_stream(&session, &mut f)?;
            std::io::Write::flush(&mut f)?;
            eprintln!("wrote {stem}.json ({} blocks)", session.blocks().len());
            println!("{stem}"); // last stdout line = the stem, for scripting
        }
    }
    Ok(())
}

/// `--paths`: emit the session's directory facts as one JSON object and exit. The stable
/// shell-out contract for a non-Rust consumer (whid's Python collector) — every value comes
/// straight from a `discover` function so there is one implementation of the store-dir decode
/// and the repo-root walk, not a re-implementation per language. Each field is `null` when the
/// corresponding function returns `None`. See [`discover`] for the semantics of each.
fn print_session_paths(args: &Args) -> Result<()> {
    // `--all`: the machine-wide sweep. The store registry decides what exists; `--since`
    // trims it by mtime BEFORE any transcript is opened, because the per-file facts below
    // include `latest_cwd`, a whole-file scan.
    if args.all {
        let cutoff = match args.since.as_deref() {
            Some(w) => Some(window_cutoff(w)?),
            None => None,
        };
        let rows: Vec<serde_json::Value> = discover::store_all(args.agent)
            .into_iter()
            .filter(|e| cutoff.is_none_or(|c| e.mtime >= c))
            .map(|e| session_paths_json(&e.path, Some(e.agent), Some(e.mtime)))
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    let path = discover::resolve_any(args.agent, args.target.as_deref(), args.latest)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&session_paths_json(&path, args.agent, None))?
    );
    Ok(())
}

/// `90m` / `24h` / `7d` → the epoch-second cutoff that window starts at.
fn window_cutoff(w: &str) -> Result<f64> {
    let (n, unit) = w.split_at(w.len().saturating_sub(1));
    let secs: f64 = match unit {
        "m" => 60.0,
        "h" => 3600.0,
        "d" => 86400.0,
        _ => anyhow::bail!("--since takes a window like 90m, 24h or 7d (got {w:?})"),
    };
    // Unsigned integer on purpose: `-1d` would parse as a f64 and put the cutoff in the
    // FUTURE, filtering everything out with no error — the worst failure a sweep can have,
    // because an empty result looks like an answer. Same for `inf`/`NaN`.
    let n: u64 = n
        .parse()
        .map_err(|_| anyhow::anyhow!("--since takes a window like 90m, 24h or 7d (got {w:?})"))?;
    let n = n as f64;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    Ok(now - n * secs)
}

/// The `--paths` JSON object for one transcript — split from the printing so the contract can be
/// asserted without a subprocess. Every field comes straight from a `discover` function.
///
/// One shape for both forms (single target and `--all`), so a consumer parses one object: the
/// sweep supplies `agent`/`mtime` from the store entry it already holds, and the single form
/// detects the agent when the caller did not name one. `session_key` is the canonical GROUP
/// identity from [`discover::session_key_from`] — the exact string a hide list stores, so a
/// consumer tests ONE string instead of probing `p:<cwd>` or `p:<repo_root>` and hoping.
///
/// Deliberately NOT emitted: whether the session is hidden. The hide list belongs to
/// claude-monitor; a verdict here would make the viewer a reader of another tool's state, and
/// a consumer that keeps its own list would get two answers. We emit the key it tests.
fn session_paths_json(
    path: &std::path::Path,
    agent: Option<Agent>,
    mtime: Option<f64>,
) -> serde_json::Value {
    let show = |o: Option<std::path::PathBuf>| o.map(|p| p.display().to_string());
    let agent = agent.unwrap_or_else(|| discover::detect_agent(path));
    let repo_root = discover::repo_root(path);
    let project_path = discover::project_path(path);
    let key = discover::session_key_from(agent, repo_root.as_deref(), project_path.as_deref());
    serde_json::json!({
        "path": path.display().to_string(),
        "agent": agent.label(),
        "mtime": mtime,
        "session_id": discover::session_id(path),
        "first_cwd": show(discover::first_cwd(path)),
        "latest_cwd": show(discover::latest_cwd(path)),
        "project_path": show(project_path),
        "repo_root": show(repo_root),
        "session_key": key.key,
        "key_kind": match key.kind {
            discover::SessionKeyKind::Project => "project",
            discover::SessionKeyKind::Agent => "agent",
        },
        "label": key.label,
    })
}

#[cfg(test)]
mod paths_tests {
    use super::*;

    /// `--paths` reports each `discover` fact under a stable key (the shell-out contract for a
    /// non-Rust consumer). A transcript sitting in a real repo resolves all five to that repo.
    #[test]
    fn session_paths_json_reports_the_discover_facts() {
        let pid = std::process::id();
        let base = std::env::temp_dir().join(format!("crpaths{pid}"));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let t = repo.join("s.jsonl");
        std::fs::write(
            &t,
            format!(
                "{{\"type\":\"user\",\"cwd\":\"{}\",\"sessionId\":\"sid1\"}}\n",
                repo.display()
            ),
        )
        .unwrap();

        let v = session_paths_json(&t, None, None);
        let repo_s = repo.display().to_string();
        assert_eq!(v["path"], t.display().to_string());
        assert_eq!(v["session_id"], "sid1");
        assert_eq!(v["first_cwd"], repo_s);
        assert_eq!(v["latest_cwd"], repo_s);
        assert_eq!(v["project_path"], repo_s);
        assert_eq!(v["repo_root"], repo_s);
        // The sweep's extra facts ride the SAME object, so a consumer parses one shape.
        // `session_key` is the canonical hide/group identity — one string to test, not a
        // `p:<cwd>`-or-`p:<repo_root>` guess.
        assert_eq!(v["agent"], "claude", "detected when the caller names none");
        assert_eq!(v["session_key"], format!("p:{repo_s}"));
        assert_eq!(v["key_kind"], "project");
        assert_eq!(v["label"], "repo");
        assert!(
            v["mtime"].is_null(),
            "no mtime unless the sweep supplied one"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// A non-workspace-anchored agent (QoderWork, whose cwd is noise) keys by AGENT, not
    /// project — the rule lives in `discover::session_key_from`, and `--paths` must report
    /// it rather than assuming every session groups by directory.
    #[test]
    fn session_key_follows_the_agent_anchoring_rule() {
        let pid = std::process::id();
        let base = std::env::temp_dir().join(format!("crpathskey{pid}"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let t = base.join("s.jsonl");
        std::fs::write(&t, "{\"type\":\"user\",\"sessionId\":\"sid2\"}\n").unwrap();

        let v = session_paths_json(&t, Some(Agent::QODERWORK), Some(1.5));
        assert_eq!(v["agent"], "qoderwork");
        assert_eq!(v["key_kind"], "agent");
        assert_eq!(v["session_key"], "a:qoderwork");
        assert_eq!(v["mtime"], 1.5, "the sweep's mtime rides through");

        let _ = std::fs::remove_dir_all(&base);
    }

    /// `--since` is a window back from now, and anything else is a clean error rather than a
    /// silently-empty sweep.
    #[test]
    fn window_cutoff_parses_the_three_units_and_rejects_the_rest() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let approx = |got: f64, want_back: f64| (now - got - want_back).abs() < 5.0;
        assert!(approx(window_cutoff("90m").unwrap(), 5400.0));
        assert!(approx(window_cutoff("24h").unwrap(), 86400.0));
        assert!(approx(window_cutoff("7d").unwrap(), 604800.0));
        for bad in ["7", "d", "7w", "", "abcd", "-1d"] {
            assert!(window_cutoff(bad).is_err(), "{bad:?} should not parse");
        }
    }
}
