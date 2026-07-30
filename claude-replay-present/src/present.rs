//! Shared viewer TEXT formatters — the plain-string summaries the TUI renderer and the
//! HTML exporter both need (spawn chips, activity/turn summaries, tool display names, edit
//! summaries, …). Pure text over `crate::model::{Block, SubAgent}`; no ratatui/theme.

use crate::model::{Block, SubAgent};

/// Direct tool calls in a sub-agent's child transcript (activity tools absorbed into a
/// `Thinking` turn are counted too, since grouping folds Bash/Read/… into it).
pub fn tool_count(sa: &SubAgent) -> usize {
    sa.blocks
        .iter()
        .map(|b| match b {
            Block::ToolUse { .. } | Block::SubAgent(_) => 1,
            Block::Thinking { tools, .. } => tools.len(),
            _ => 0,
        })
        .sum()
}

/// The collapsed spawn's chip: `<N> tools · launched` (or just `launched`). The spawn is
/// the *launch* event and always reads "launched" — the terminal status shows on the
/// separate `AgentDone` completion event, not here.
pub fn spawn_chip(sa: &SubAgent) -> String {
    let tools = tool_count(sa);
    if tools > 0 {
        format!(
            "{tools} tool{} · launched",
            if tools == 1 { "" } else { "s" }
        )
    } else {
        "launched".to_string()
    }
}

/// Claude Code shows only the first `WRITE_PREVIEW` lines of a file write, then a
/// `… +N lines` marker (the full content isn't dumped into the transcript view).
pub const WRITE_PREVIEW: usize = 10;

/// `Added N lines[, removed M lines]` (singular/plural; "removed" omitted at 0) —
/// the Edit/MultiEdit result summary, matching Claude Code.
pub fn edit_summary(adds: usize, dels: usize) -> String {
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    let a = format!("Added {adds} line{}", plural(adds));
    if dels == 0 {
        a
    } else {
        format!("{a}, removed {dels} line{}", plural(dels))
    }
}

/// The display name Claude Code shows for a tool — it labels Edit/MultiEdit as
/// `Update`; everything else keeps its tool name.
pub fn display_name(name: &str) -> &str {
    match name {
        "Edit" | "MultiEdit" => "Update",
        other => other,
    }
}

// The span-summarization vocabulary lives in the CORE since #68 (see the #58 study:
// `design/fold-coalesce-summarize-extensibility.md`) — re-exported here so both
// frontends keep importing one place.
pub use crate::summary::{thinking_summary, turn_summary};

/// A Write/NotebookEdit's body: the first non-empty *new-side* text across its diffs (the
/// transcript records a Write as a diff whose new side is the whole file). Shared so the TUI
/// and HTML agree on which diff supplies the content.
pub fn write_content(diffs: &[(String, String)]) -> &str {
    diffs
        .iter()
        .map(|(_, n)| n.as_str())
        .find(|n| !n.is_empty())
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `tool_count` for a spawn is **node-scoped**: it tallies the child's own tools (here 2
    /// Reads, coalesced into an activity list), not the parent's Bash. Exercises the
    /// present-side counter over a `model`-parsed sub-agent tree — an integration point that
    /// lived in `model`'s tests until the parser core was split into `claude-replay-core`.
    #[test]
    fn child_scoped_tool_count() {
        use std::io::Write;
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let base = std::env::temp_dir().join(format!(
            "cr-present-subagent-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&base);
        let sess = base.join("proj").join("sid.jsonl");
        let sadir = base.join("proj").join("sid").join("subagents");
        std::fs::create_dir_all(&sadir).unwrap();
        // Parent: one Agent spawn; its own transcript has a Bash the child must NOT be
        // credited with.
        let parent = concat!(
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"toolu_P\",\"name\":\"Bash\",\"input\":{\"command\":\"ls\"}}]}}\n",
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"toolu_A\",\"name\":\"Agent\",\"input\":{\"subagent_type\":\"general-purpose\",\"description\":\"child\",\"prompt\":\"go\"}}]}}\n",
            "{\"type\":\"user\",\"toolUseResult\":{\"agentId\":\"achild01\",\"status\":\"completed\"},\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"toolu_A\",\"content\":\"done\"}]}}\n"
        );
        std::fs::File::create(&sess)
            .unwrap()
            .write_all(parent.as_bytes())
            .unwrap();
        // Child transcript: two Read tools.
        let child = concat!(
            "{\"type\":\"user\",\"message\":{\"content\":\"go\"}}\n",
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"c1\",\"name\":\"Read\",\"input\":{\"file_path\":\"/a\"}}]}}\n",
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"c2\",\"name\":\"Read\",\"input\":{\"file_path\":\"/b\"}}]}}\n"
        );
        std::fs::File::create(sadir.join("agent-achild01.jsonl"))
            .unwrap()
            .write_all(child.as_bytes())
            .unwrap();

        // Parse through the public entry point (enriched = loads the sub-agent tree), the
        // same way a library consumer would — no reach into the core's per-agent internals.
        let blocks = claude_replay_core::parse_session_enriched_as(crate::Agent::CLAUDE, &sess)
            .unwrap()
            .blocks();
        let Some(crate::model::Block::SubAgent(sa)) = blocks
            .iter()
            .find(|b| matches!(b, crate::model::Block::SubAgent(_)))
        else {
            panic!("no SubAgent: {blocks:?}")
        };
        assert_eq!(
            tool_count(sa),
            2,
            "node-scoped tool count (child's 2 Reads, not the parent's Bash)"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
