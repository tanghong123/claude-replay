//! **QoderWork's transcript discovery** — the QoderWork half of the shared `discover`
//! interface. QoderWork is a Claude-Code-format client whose store layout is identical
//! (`~/.qoderwork/projects/<slug>/<id>.jsonl`, same slug convention), so this is a thin
//! wrapper over `claude_discover`'s root-parameterized internals — only the root (and the
//! agent tag on candidates) differ. Parsing likewise delegates to the Claude implementations
//! (see the `QoderWorkAdapter` in `adapter.rs`); the one format difference is the
//! `runtime-config` head line, which drives detection.

use crate::engine::seam::{Agent, Candidate};
use std::path::{Path, PathBuf};

/// Root under which QoderWork writes per-project transcript dirs.
pub(crate) fn projects_dir() -> PathBuf {
    if let Ok(p) = std::env::var("QODERWORK_PROJECTS_DIR") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    Path::new(&home).join(".qoderwork").join("projects")
}

/// QoderWork sessions scoped strictly to `cwd` or its nearest ancestor that has sessions —
/// the same no-global-fallback scoping as the Claude store.
pub fn candidates_scoped(cwd: &Path) -> Vec<Candidate> {
    crate::engine::seam::candidates_scoped_in(
        &projects_dir(),
        Agent::QODERWORK,
        cwd,
        crate::engine::seam::home_dir().as_deref(),
    )
}

/// Find a QoderWork transcript by session id (`<id>.jsonl`) anywhere under its projects dir.
pub fn transcript_by_id(id: &str) -> Option<PathBuf> {
    crate::engine::seam::transcript_by_id_in(&projects_dir(), id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// The delegation guarantee: parsing a QoderWork transcript AS QoderWork is byte-identical
    /// to parsing it as Claude (same blocks, times, metrics) — the adapter adds detection and a
    /// store, never a format fork. Fixture mirrors the real shape: runtime-config head,
    /// multi-text user line, a tool call + result.
    #[test]
    fn qoderwork_parse_is_byte_identical_to_claude_parse() {
        let f = std::env::temp_dir().join(format!("qw-equiv-{}.jsonl", std::process::id()));
        std::fs::write(&f, concat!(
            r#"{"type":"runtime-config","sessionId":"s","model":"qwork-ultimate","timestamp":1785068132048}"#, "
",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"<system-reminder>env</system-reminder>"},{"type":"text","text":"do it"}]},"timestamp":"2026-07-26T12:15:33Z"}"#, "
",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}],"model":"qwork-ultimate"},"timestamp":"2026-07-26T12:15:40Z"}"#, "
",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]},"timestamp":"2026-07-26T12:15:41Z"}"#, "
",
        )).unwrap();
        let qw = crate::engine::seam::parse_session_as(Agent::QODERWORK, &f).unwrap();
        let cl = crate::engine::seam::parse_session_as(Agent::CLAUDE, &f).unwrap();
        assert_eq!(
            format!("{:?}", qw.blocks()),
            format!("{:?}", cl.blocks()),
            "blocks identical under delegation"
        );
        assert_eq!(qw.user_times, cl.user_times);
        assert_eq!(qw.metrics, cl.metrics);
        assert_eq!(
            qw.agent,
            Agent::QODERWORK,
            "identity is the only difference"
        );
        let _ = std::fs::remove_file(&f);
    }

    /// Discovery over a fake QoderWork store: candidates come back tagged `QoderWork`, scoped
    /// to the cwd's slug, and a bare id resolves to its transcript — the picker/`--latest`/
    /// bare-id surface the Claude store already has, on the QoderWork root.
    #[test]
    fn discovers_and_resolves_from_the_qoderwork_store() {
        let root = std::env::temp_dir().join(format!("qw-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let cwd = Path::new("/Users/dev/proj");
        let slug = "-Users-dev-proj";
        std::fs::create_dir_all(root.join(slug)).unwrap();
        let mut f = std::fs::File::create(root.join(slug).join("abc123.jsonl")).unwrap();
        writeln!(f, r#"{{"type":"runtime-config","sessionId":"abc123","model":"qwork-ultimate","timestamp":1785068132048}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"hello qoderwork"}}]}},"timestamp":"2026-07-26T12:15:33Z"}}"#).unwrap();

        // Env-scoped: the override is process-global, so serialize against other env users.
        std::env::set_var("QODERWORK_PROJECTS_DIR", &root);
        // #69: through the PUBLIC surface (env $HOME), a cwd outside the real home
        // discovers nothing — even though its slug exists in the store.
        assert!(
            candidates_scoped(cwd).is_empty(),
            "cwd outside $HOME must not auto-discover"
        );
        let by_id = transcript_by_id("abc123");
        std::env::remove_var("QODERWORK_PROJECTS_DIR");
        // With the matching home bound, the store discovers and scopes normally.
        let cands = crate::engine::seam::candidates_scoped_in(
            &root,
            Agent::QODERWORK,
            cwd,
            Some(Path::new("/Users/dev")),
        );

        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].agent, Agent::QODERWORK);
        assert!(cands[0].cwd_affinity, "scoped to the cwd's own slug");
        assert_eq!(
            by_id.as_deref(),
            Some(root.join(slug).join("abc123.jsonl").as_path())
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
