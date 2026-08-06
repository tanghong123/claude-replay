//! The **registry half of discovery** (#87 step 3): auto-detection, id/path resolution,
//! and the cross-agent candidate sweep — every function here consults the wired adapter
//! registry. The agent-free vocabulary ([`Candidate`], the ancestor scoping, the
//! transcript-head readers) lives in `claude-replay-engine::discover` and is re-exported
//! here, so `crate::discover::*` keeps presenting the ONE discovery module.

pub use claude_replay_engine::discover::*;

use crate::Agent;
use anyhow::{anyhow, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Sessions for the current directory across **every** agent, filtered to `only`
/// when set (else all agents), sorted cwd-matches-first then most-recent.
pub fn candidates_all(only: Option<Agent>) -> Vec<Candidate> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut out: Vec<Candidate> = Vec::new();
    // Each agent is scoped to cwd-or-nearest-ancestor-with-sessions, with no global
    // fallback — so a session for an unrelated directory never shows here.
    for a in crate::adapter::adapters() {
        if only.is_none() || only == Some(a.agent()) {
            out.extend(a.candidates_scoped(&cwd));
        }
    }
    out.sort_by(|a, b| {
        b.cwd_affinity
            .cmp(&a.cwd_affinity)
            .then(b.mtime.cmp(&a.mtime))
    });
    out
}

/// Auto-detect which agent wrote a transcript by sniffing its first lines — asking
/// each registered adapter's `sniff` claim (#59). An `Owns`
/// claim (a distinctive head: Codex's `session_meta`, QoderWork's `runtime-config`)
/// wins immediately; a mere `CanParse` (Claude's adapter can parse any Claude-format
/// lines, including derived agents') is remembered and only wins if NO adapter owns
/// any of the sniffed lines — so a new Claude-format agent is labeled by its owner
/// marker, never mislabeled by the first can-parse adapter, and Claude's sniff needs
/// no per-agent carve-outs. Defaults to Claude.
pub fn detect_agent(path: &Path) -> Agent {
    detect_agent_claimed(path).0
}

/// [`detect_agent`] plus whether the label is OWNERSHIP-PROVEN (#66): `true` when a
/// distinctive marker decided it, `false` for a merely-compatible (can-parse)
/// fallback — an unknown Claude-format-derived agent's transcript parses fine but
/// is honestly "compatible", not known to be Claude's. Presenters may badge the
/// unowned case; parse dispatch uses the agent either way.
pub fn detect_agent_claimed(path: &Path) -> (Agent, bool) {
    use crate::adapter::SniffClaim;
    use std::io::BufRead;
    let Ok(file) = std::fs::File::open(path) else {
        return (Agent::CLAUDE, false);
    };
    let mut can_parse: Option<Agent> = None;
    for line in std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .take(5)
    {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        for a in crate::adapter::adapters() {
            match a.sniff(&v) {
                SniffClaim::Owns => return (a.agent(), true),
                SniffClaim::CanParse => {
                    if can_parse.is_none() {
                        can_parse = Some(a.agent());
                    }
                }
                SniffClaim::No => {}
            }
        }
    }
    (can_parse.unwrap_or(Agent::CLAUDE), false)
}

/// The **transcript file path** of sub-agent `child_id` spawned under the session at `root`,
/// for the given `agent`. Resolves a path; it does **not** parse. `None` if the adapter has no
/// resolvable child transcript. Routes to the agent adapter's `subagent_source` hook.
///
/// This is the **lazy, on-demand** route — the presentation layer uses it to open a child from
/// its own file (descend-and-live-tail in the TUI, the HTML server's deep links, the
/// `--dump-all-html` BFS). It's distinct from the **eager**
/// [`parse_session_enriched`](crate::parse_session_enriched), which asks the same adapter to
/// load each child's blocks at parse time.
pub fn subagent_source(agent: Agent, root: &Path, child_id: &str) -> Option<PathBuf> {
    crate::adapter::adapter(agent).subagent_source(root, child_id)
}

/// [`subagent_source`] for MANY children in ONE operation-scoped call — results in `ids`
/// order. An adapter backed by a relationship store (Codex) scans that store once for the
/// whole batch instead of once per child; for the rest it is exactly the per-child loop.
/// Use this whenever a parent's children are resolved together (the live server registering
/// a pull's child list, the enriched parse's sub-agent meta).
pub fn subagent_sources(agent: Agent, root: &Path, ids: &[&str]) -> Vec<Option<PathBuf>> {
    crate::adapter::adapter(agent).subagent_sources(root, ids)
}

/// The LIVE on-disk task list for the session at `path` (#15) — the agent-neutral
/// facade over each adapter's `load_tasks` hook (Claude reads
/// `~/.claude/tasks/<session-id>/*.json`; agents with no task store return `None`).
/// Complements the transcript-derived op-log state in
/// [`Session::tasks`](crate::Session); merge with
/// [`tasks::merged`](crate::engine::tasks::merged) — disk wins per id, the op-log
/// backfills pruned files.
pub fn session_tasks(agent: Agent, path: &Path) -> Option<crate::engine::tasks::TaskList> {
    crate::adapter::adapter(agent).load_tasks(path)
}

/// What `agent` calls the session at `path` — its name and its most recent prompt (see
/// [`SessionCard`]). `None` for an agent that names nothing, or a
/// session not yet named.
///
/// Discovery-side: this reads a file (or an agent's own store) and is never part of a fold.
pub fn session_card(agent: Agent, path: &Path) -> Option<crate::discover::SessionCard> {
    crate::adapter::adapter(agent).session_card(path)
}

/// Is the `agent` label for `path` OWNERSHIP-PROVEN (#66)? True when the sniff
/// owned it (a distinctive in-band marker) OR the file sits inside that agent's own
/// transcript store (provenance — a normal `~/.claude/projects` session is Claude's
/// without any marker). False = merely-compatible: an arbitrary path whose format
/// parses but whose author is honestly unknown.
pub fn detection_owned(agent: Agent, path: &Path) -> bool {
    detect_agent_claimed(path).1 || crate::adapter::adapter(agent).store_contains(path)
}

/// Resolve which transcript to open, across agents, and return its path.
///
/// - `target` — an explicit selection, **either a filesystem path to a `.jsonl` transcript OR
///   a bare session id**, tried in that order: if the string names an existing file, that file
///   is used (and its agent is auto-detected on open); otherwise it's looked up as a session id
///   in each in-scope agent's store. `None` means "no explicit target — fall back to `latest`".
/// - `only` — restrict the search to a single agent's store; `None` searches every agent.
/// - `latest` — used only when `target` is `None`: pick the most-recent transcript for the
///   current directory (or its nearest ancestor that has sessions; cwd-matches first, no global
///   fallback) instead of erroring.
///
/// Precedence: `target` (as a path, then as a session id) → else `latest` → else: when the
/// cwd-scoped session set has exactly ONE candidate it is auto-selected (#51 — selection is
/// only demanded on genuine ambiguity; the non-interactive `--dump*` paths land here, while
/// the interactive default opens the picker and never reaches this branch), several
/// candidates `Err` listing them, zero `Err` with the no-session message.
pub fn resolve_any(only: Option<Agent>, target: Option<&str>, latest: bool) -> Result<PathBuf> {
    if let Some(t) = target {
        let as_path = PathBuf::from(t);
        if as_path.is_file() {
            return Ok(as_path);
        }
        // Session id: look in each in-scope agent's store via its adapter.
        for a in crate::adapter::adapters() {
            if only.is_none() || only == Some(a.agent()) {
                if let Some(hit) = a.resolve_id(t) {
                    return Ok(hit);
                }
            }
        }
        return Err(anyhow!(
            "no transcript found for '{t}' (not a file, and no session id match)"
        ));
    }
    // Scoped to the cwd or its nearest ancestor that has sessions — NOT the
    // global newest. `candidates_all` sorts cwd-matches first, then most-recent,
    // with no global fallback, so an unrelated directory's session never wins.
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut cands = candidates_all(only);
    if latest {
        return cands
            .into_iter()
            .next()
            .map(|c| c.path)
            .ok_or_else(|| anyhow!("no session found for {} or its ancestors", cwd.display()));
    }
    resolve_lone(&mut cands, &cwd)
}

/// The no-target-no-`--latest` fallback (#51): exactly one cwd-scoped candidate is
/// unambiguous — use it; several demand a selection (the error names them, newest
/// first); zero keeps the no-session message. Split from [`resolve_any`] so the
/// three-way rule is unit-testable without a real store.
fn resolve_lone(cands: &mut Vec<Candidate>, cwd: &Path) -> Result<PathBuf> {
    match cands.len() {
        1 => Ok(cands.remove(0).path),
        0 => Err(anyhow!(
            "no session found for {} or its ancestors — give a session id or a path",
            cwd.display()
        )),
        n => {
            let mut msg =
                format!("{n} sessions match this directory — give a session id or use --latest:");
            for c in cands.iter().take(10) {
                let id = c
                    .path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("<unnamed>");
                let age = match c.mtime.elapsed() {
                    Ok(d) if d.as_secs() < 3600 => format!("{}m ago", d.as_secs() / 60),
                    Ok(d) if d.as_secs() < 86_400 => format!("{}h ago", d.as_secs() / 3600),
                    Ok(d) => format!("{}d ago", d.as_secs() / 86_400),
                    Err(_) => String::new(),
                };
                let snippet: String = c.snippet.chars().take(60).collect();
                msg.push_str(&format!("\n  {id}  [{}] {age}  {snippet}", c.agent.label()));
            }
            if n > 10 {
                msg.push_str(&format!("\n  … and {} more", n - 10));
            }
            Err(anyhow!(msg))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    /// The no-target-no-`--latest` rule (#51): one cwd-scoped candidate is
    /// unambiguous and auto-selects; several error NAMING each (id, agent, snippet)
    /// so the user can pick; zero keeps the no-session error.
    #[test]
    fn lone_candidate_auto_selects_and_ambiguity_names_the_choices() {
        let cand = |stem: &str, snippet: &str| Candidate {
            path: PathBuf::from(format!("/store/{stem}.jsonl")),
            mtime: SystemTime::now(),
            project: "proj".into(),
            snippet: snippet.into(),
            cwd_affinity: true,
            agent: Agent::CLAUDE,
        };
        let cwd = PathBuf::from("/w/proj");
        // Exactly one → auto-selected.
        let mut one = vec![cand("aaaa-1111", "fix the parser")];
        assert_eq!(
            resolve_lone(&mut one, &cwd).unwrap(),
            PathBuf::from("/store/aaaa-1111.jsonl")
        );
        // Two → error names both ids and the snippets.
        let mut two = vec![
            cand("aaaa-1111", "fix the parser"),
            cand("bbbb-2222", "add the exporter"),
        ];
        let err = resolve_lone(&mut two, &cwd).unwrap_err().to_string();
        assert!(err.contains("2 sessions match"), "{err}");
        assert!(
            err.contains("aaaa-1111") && err.contains("bbbb-2222"),
            "{err}"
        );
        assert!(
            err.contains("fix the parser") && err.contains("--latest"),
            "{err}"
        );
        // Zero → the no-session error.
        let err = resolve_lone(&mut Vec::new(), &cwd).unwrap_err().to_string();
        assert!(err.contains("no session found"), "{err}");
    }

    #[test]
    fn detect_agent_sniffs_transcript_shape() {
        let dir = std::env::temp_dir();
        let codex = dir.join(format!("detect-codex-{}.jsonl", std::process::id()));
        std::fs::write(
            &codex,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\",\"cwd\":\"/x\"}}\n",
        )
        .unwrap();
        let claude = dir.join(format!("detect-claude-{}.jsonl", std::process::id()));
        std::fs::write(
            &claude,
            "{\"sessionId\":\"abc\",\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
        )
        .unwrap();

        // QoderWork: Claude-format lines under a `runtime-config` head — the head also
        // carries `sessionId`, so Claude's adapter CAN parse it, but QoderWork OWNS the
        // distinctive head and ownership outranks can-parse (#59) — no carve-out in
        // Claude's sniff, and detection stays order-independent.
        let qoderwork = dir.join(format!("detect-qw-{}.jsonl", std::process::id()));
        std::fs::write(
            &qoderwork,
            "{\"type\":\"runtime-config\",\"sessionId\":\"abc\",\"model\":\"qwork-ultimate\",\"timestamp\":1785068132048}\n",
        )
        .unwrap();

        assert_eq!(detect_agent(&codex), Agent::CODEX);
        assert_eq!(detect_agent(&claude), Agent::CLAUDE);
        assert_eq!(detect_agent(&qoderwork), Agent::QODERWORK);
        // The claim levels behind that: Claude claims CanParse on QW's head; QW Owns it.
        {
            use crate::adapter::{adapter, SniffClaim};
            let head: Value =
                serde_json::from_str("{\"type\":\"runtime-config\",\"sessionId\":\"abc\"}")
                    .unwrap();
            assert_eq!(adapter(Agent::CLAUDE).sniff(&head), SniffClaim::CanParse);
            assert_eq!(adapter(Agent::QODERWORK).sniff(&head), SniffClaim::Owns);
        }
        // A missing/empty file falls back to Claude.
        assert_eq!(detect_agent(Path::new("/nonexistent.jsonl")), Agent::CLAUDE);

        std::fs::remove_file(&codex).ok();
        std::fs::remove_file(&claude).ok();
        std::fs::remove_file(&qoderwork).ok();
    }

    #[test]
    fn ancestors_start_at_cwd_are_parent_chain_and_stop_before_home() {
        let dirs = ancestors_below(&std::env::current_dir().unwrap(), home_dir().as_deref());
        let cwd = std::env::current_dir().unwrap();
        let home = home_dir();
        // #69: outside $HOME (or at it, or with no $HOME) there is NO auto-discovery.
        let inside_home = home
            .as_deref()
            .is_some_and(|h| cwd != h && cwd.starts_with(h));
        if !inside_home {
            assert!(
                dirs.is_empty(),
                "cwd outside $HOME must not probe: {dirs:?}"
            );
            return;
        }
        assert!(!dirs.is_empty(), "should include at least the cwd");
        assert_eq!(dirs[0], cwd, "nearest first = cwd");
        // Each entry is the parent of the previous.
        for w in dirs.windows(2) {
            assert_eq!(w[1], w[0].parent().unwrap(), "not a parent chain: {w:?}");
        }
        // $HOME itself is never probed; the chain's last entry sits directly under it.
        let home = home.unwrap();
        assert!(!dirs.contains(&home), "must stop BEFORE $HOME: {dirs:?}");
        assert_eq!(dirs.last().unwrap().parent().unwrap(), home);
    }

    /// #66: ownership is sniff-marker OR store-provenance. An arbitrary-path
    /// Claude-format file is merely compatible (parse dispatch unaffected); the
    /// same bytes inside Claude's own store are ownership-proven; a Codex head is
    /// sniff-owned anywhere.
    #[test]
    fn detection_ownership_combines_sniff_and_provenance() {
        let dir = std::env::temp_dir();
        let stray = dir.join(format!("own-stray-{}.jsonl", std::process::id()));
        std::fs::write(
            &stray,
            "{\"sessionId\":\"abc\",\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
        )
        .unwrap();
        let (agent, sniff_owned) = detect_agent_claimed(&stray);
        assert_eq!(agent, Agent::CLAUDE);
        assert!(!sniff_owned, "claude format alone proves nothing");
        assert!(
            !detection_owned(Agent::CLAUDE, &stray),
            "arbitrary path: merely compatible"
        );
        let codex = dir.join(format!("own-codex-{}.jsonl", std::process::id()));
        std::fs::write(
            &codex,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\",\"cwd\":\"/x\"}}\n",
        )
        .unwrap();
        assert!(
            detection_owned(Agent::CODEX, &codex),
            "codex head is sniff-owned anywhere"
        );
        // Store provenance: the same Claude-format bytes inside the Claude store.
        let in_store = claude_replay_agents::claude_discover::projects_dir()
            .join("own-test-proj")
            .join("own-in-store.jsonl");
        if std::fs::create_dir_all(in_store.parent().unwrap()).is_ok()
            && std::fs::copy(&stray, &in_store).is_ok()
        {
            assert!(
                detection_owned(Agent::CLAUDE, &in_store),
                "store provenance proves ownership"
            );
            let _ = std::fs::remove_dir_all(in_store.parent().unwrap());
        }
        std::fs::remove_file(&stray).ok();
        std::fs::remove_file(&codex).ok();
    }

    /// #69 (supersedes #62's bare-root guard): auto-discovery is scoped strictly
    /// inside the home directory. A cwd outside `$HOME`, at `$HOME` itself, or
    /// with no `$HOME` probes NOTHING — and a probed chain never includes `$HOME`,
    /// so a store dir recorded AT home (QoderWork's `-Users-<name>`) or at `/`
    /// (its `-` dir, #62) can never match.
    #[test]
    fn ancestors_are_scoped_strictly_inside_home() {
        let home = Some(Path::new("/Users/hong"));
        // Outside home → nothing (this cwd used to leak `/`-recorded sessions, #62).
        assert_eq!(
            ancestors_below(Path::new("/private/tmp/some/scratch"), home),
            Vec::<PathBuf>::new()
        );
        // Home itself → nothing (QoderWork records some sessions' project dir AS home).
        assert_eq!(
            ancestors_below(Path::new("/Users/hong"), home),
            Vec::<PathBuf>::new()
        );
        // The bare root and no-home are equally barren.
        assert_eq!(ancestors_below(Path::new("/"), home), Vec::<PathBuf>::new());
        assert_eq!(
            ancestors_below(Path::new("/Users/hong/w/repo"), None),
            Vec::<PathBuf>::new()
        );
        // Inside home → cwd up to (never including) home, nearest first.
        assert_eq!(
            ancestors_below(Path::new("/Users/hong/w/repo"), home),
            vec![
                PathBuf::from("/Users/hong/w/repo"),
                PathBuf::from("/Users/hong/w")
            ]
        );
    }
}
