//! Shared viewer TEXT formatters — the plain-string summaries the TUI renderer and the
//! HTML exporter both need (spawn chips, activity/turn summaries, tool display names, edit
//! summaries, …). Pure text over `crate::model::{Block, SubAgent}`; no ratatui/theme.

use crate::model::{Block, CompactTrigger, SubAgent, ToolDuration, ToolExecution, ToolStatus};

/// The compaction divider's one-line text — `context compacted · auto · 725k → 7.0k` (#108).
/// Shared so the TUI rule, the HTML divider, and `--dump` say the same thing; each surface
/// adds its own decoration (the TUI's `──` rule, the HTML's hairline).
pub fn compaction_summary(trigger: CompactTrigger, pre_tokens: u64, post_tokens: u64) -> String {
    // Sizes only when the transcript recorded them — a boundary with no token figures still
    // deserves its divider, and `0 → 0` would be a claim the record never made.
    if pre_tokens == 0 && post_tokens == 0 {
        return format!("context compacted · {trigger}");
    }
    format!(
        "context compacted · {trigger} · {} → {}",
        crate::metrics::human_tokens(pre_tokens),
        crate::metrics::human_tokens(post_tokens),
    )
}

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

/// The collapsed spawn's chip: `<N> tools · launched` (or just `launched`). An ASYNC
/// spawn is the *launch* event and always reads "launched" — its terminal status shows
/// on the separate `AgentDone` completion event. A SYNCHRONOUS spawn (the inline
/// `result` is right here, and no completion event will ever follow — QoderWork's
/// `agent-result` shape, #95) shows its terminal verb instead: `<N> tools · done`.
pub fn spawn_chip(sa: &SubAgent) -> String {
    use crate::model::AgentStatus;
    let state = if sa.result.is_some() {
        match sa.status {
            AgentStatus::Completed => "done",
            AgentStatus::Failed => "failed",
            AgentStatus::Killed => "killed",
            AgentStatus::Stopped => "stopped",
            _ => "launched",
        }
    } else {
        "launched"
    };
    let tools = tool_count(sa);
    if tools > 0 {
        format!(
            "{tools} tool{} · {state}",
            if tools == 1 { "" } else { "s" }
        )
    } else {
        state.to_string()
    }
}

/// Claude Code shows only the first `WRITE_PREVIEW` lines of a file write, then a
/// `… +N lines` marker (the full content isn't dumped into the transcript view).
pub const WRITE_PREVIEW: usize = 10;

/// Why an edit changed nothing (#s22): the call FAILED (or was interrupted), or the reader
/// REJECTED it. A not-applied edit used to read exactly like an applied one — its diff and
/// `Added 1 line, removed 1 line` — with only a chip on its head to say otherwise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotApplied {
    /// `Error: <the client's reason>` — drawn INSTEAD of the diff, as Claude Code draws it: the
    /// change never happened, and what it would have been is not what the reader needs.
    Failed(String),
    /// `User rejected update to <file>` — drawn ABOVE the proposed diff, as Claude Code draws it:
    /// the reader saw it and said no, and what they said no to is the point.
    Rejected(String),
}

/// What a file-changing call says in place of its summary when it changed nothing — `None` when
/// it was applied, or recorded no outcome. `reason` is the result text the adapter kept for a
/// failed edit (Claude's `<tool_use_error>`, a patch Codex rejected).
pub fn not_applied(
    name: &str,
    target: &str,
    overwrite: bool,
    execution: Option<&ToolExecution>,
    reason: Option<&str>,
) -> Option<NotApplied> {
    let reason = reason.map(str::trim).filter(|r| !r.is_empty());
    match execution.and_then(|e| e.status)? {
        ToolStatus::Declined => {
            let verb = if matches!(name, "Write" | "NotebookEdit") && !overwrite {
                "write"
            } else {
                "update"
            };
            Some(NotApplied::Rejected(format!(
                "User rejected {verb} to {target}"
            )))
        }
        ToolStatus::Failed | ToolStatus::Cancelled => Some(NotApplied::Failed(match reason {
            Some(r) if r.starts_with("Error") => r.to_string(),
            Some(r) => format!("Error: {r}"),
            None => "Error: not applied".to_string(),
        })),
        ToolStatus::Completed | ToolStatus::Unknown => None,
    }
}

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

/// The same, for ONE FILE of a Bash command's edit diff (#263). A Bash call's header names the
/// COMMAND, so the file has to name itself: `CHANGELOG.md · Added 3 lines, removed 2 lines`. A
/// file the transcript named but recorded no hunks for — a tarball, or one past the five-file
/// cap `bashEditDiff` applies — says only that it changed, because that is all that is known.
pub fn file_edit_summary(file: &str, adds: usize, dels: usize) -> String {
    let name = file.rsplit('/').next().unwrap_or(file);
    if adds == 0 && dels == 0 {
        format!("{name} · changed")
    } else {
        format!("{name} · {}", edit_summary(adds, dels))
    }
}

/// How many names [`removed_files_note`] spells out before it counts the rest.
pub const REMOVED_FILES_SHOWN: usize = 10;

/// #s155: the chip an Artifact publish wears when it removed files from the artifact, in a
/// change's own words: `−4 files`, `−1 file`.
pub fn removed_files_chip(n: usize) -> String {
    format!("−{n} file{}", if n == 1 { "" } else { "s" })
}

/// #s155: the files an Artifact publish removed, for the terminal and the pages alike —
/// `Removed from the artifact: data/a.json, data/b.json`. Each path as the result recorded it,
/// relative to the artifact, since two removed files may share a name; past
/// [`REMOVED_FILES_SHOWN`] the rest are counted (`… and 3 more`).
pub fn removed_files_note(files: &[String]) -> String {
    let shown = files
        .iter()
        .take(REMOVED_FILES_SHOWN)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    match files.len().saturating_sub(REMOVED_FILES_SHOWN) {
        0 => format!("Removed from the artifact: {shown}"),
        rest => format!("Removed from the artifact: {shown} and {rest} more"),
    }
}

/// #s182: what an Artifact edit's card says when its result applied FEWER edits than the call
/// sent — `Applied 1 of 3 edits` — for the terminal and the pages alike. `None` when they agree,
/// when nothing was sent, or when the result gave no count: the card then says nothing extra, and
/// it never claims an edit the result does not count.
pub fn edits_applied_note(applied: Option<usize>, sent: usize) -> Option<String> {
    let applied = applied?;
    (applied < sent).then(|| {
        format!(
            "Applied {applied} of {sent} edit{}",
            if sent == 1 { "" } else { "s" }
        )
    })
}

/// #s83: a SendMessage receipt as the reader should see it, for the terminal and the pages alike.
/// The client writes it as the JSON of its result (`{"success":true,"message":"…","msg_id":"…"}`),
/// whose `message` is the sentence that says where the message went ("… in that session's inbox,
/// not yet read", or "No agent named 'x' is reachable"); a receipt that is not that JSON is kept
/// as it is.
pub fn send_receipt(out: &str) -> String {
    serde_json::from_str::<serde_json::Value>(out.trim())
        .ok()
        .and_then(|v| {
            v.get("message")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| out.trim().to_string())
}

/// The display name Claude Code shows for a tool — it labels Edit/MultiEdit as
/// `Update`; everything else keeps its tool name.
pub fn display_name(name: &str) -> &str {
    match name {
        "Edit" | "MultiEdit" => "Update",
        other => other,
    }
}

/// Compact status/duration text shared by the terminal and HTML tool headers.
pub fn tool_execution_summary(execution: &ToolExecution) -> String {
    let mut parts = Vec::new();
    if let Some(exit_code) = execution.exit_code {
        parts.push(format!("exit {exit_code}"));
    } else if let Some(status) = execution.status {
        parts.push(
            match status {
                // A bare success badge on every Claude tool is noise; exit code or duration
                // still renders when present. Failures remain meaningful without either.
                ToolStatus::Completed => "",
                ToolStatus::Failed => "failed",
                ToolStatus::Declined => "declined",
                ToolStatus::Cancelled => "cancelled",
                ToolStatus::Unknown => "finished",
            }
            .to_string(),
        );
        parts.retain(|part| !part.is_empty());
    }
    if let Some(duration) = execution.duration {
        parts.push(format_tool_duration(duration));
    }
    parts.join(" · ")
}

/// Whether the structural outcome should use failure presentation.
///
/// A recorded Completed wins over a non-zero exit (#s126): Qwenwork marks an exit it does not call
/// a failure (a grep's exit 1, "No matches found") and the adapter records it Completed, so the
/// head reads `exit 1` without failure presentation. Codex records an exit code beside its own
/// status, and on 2026-10-16 every Codex command on aries-black agreed with the exit (943
/// completed, all exit 0; 70 failed, all non-zero), so no Codex head changes.
pub fn tool_execution_failed(execution: &ToolExecution) -> bool {
    match execution.status {
        Some(ToolStatus::Failed | ToolStatus::Declined | ToolStatus::Cancelled) => true,
        Some(ToolStatus::Completed) => false,
        _ => execution.exit_code.is_some_and(|code| code != 0),
    }
}

fn format_tool_duration(duration: ToolDuration) -> String {
    if duration.secs >= 60 {
        return format!("{}m {}s", duration.secs / 60, duration.secs % 60);
    }
    if duration.secs > 0 {
        let centis = duration.nanos / 10_000_000;
        return if centis == 0 {
            format!("{}s", duration.secs)
        } else {
            format!("{}.{centis:02}s", duration.secs)
        };
    }
    let millis = duration.nanos / 1_000_000;
    if millis > 0 {
        return format!("{millis}ms");
    }
    let micros = duration.nanos / 1_000;
    if micros > 0 {
        return format!("{micros}µs");
    }
    format!("{}ns", duration.nanos)
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

/// The lines a call ADDED and REMOVED, when it changed a file — `None` for every other call, and
/// for an edit that changed nothing (failed, rejected, interrupted: #s22). A fresh-file write
/// adds its whole content; an Edit, an overwrite and a shell command that recorded a diff count
/// their diff rows, as the diff part draws them. One count for the TUI's run line and the
/// html stream's chips (#s23).
pub fn change_counts(b: &Block) -> Option<(usize, usize)> {
    let Block::ToolUse {
        name,
        diffs,
        patch,
        execution,
        ..
    } = b
    else {
        return None;
    };
    if execution.as_ref().and_then(|e| e.status).is_some_and(|s| {
        matches!(
            s,
            ToolStatus::Failed | ToolStatus::Declined | ToolStatus::Cancelled
        )
    }) {
        return None;
    }
    let hunks = patch.as_deref().filter(|h| !h.is_empty());
    let edit = crate::model::is_edit_tool(name);
    if !edit && hunks.is_none() {
        return None;
    }
    if matches!(name.as_str(), "Write" | "NotebookEdit") && hunks.is_none() {
        return Some((write_content(diffs).lines().count(), 0));
    }
    let (mut adds, mut dels) = (0, 0);
    for row in crate::diff::diff_row_groups(diffs, hunks)
        .into_iter()
        .flat_map(|g| g.rows)
    {
        match row.kind {
            crate::diff::DiffKind::Add => adds += 1,
            crate::diff::DiffKind::Del => dels += 1,
            crate::diff::DiffKind::Ctx => {}
        }
    }
    Some((adds, dels))
}

/// What a RUN changed (#s23): the sum of its calls' [`change_counts`], `None` when none of them
/// changed a file.
pub fn run_changes(tools: &[Block]) -> Option<(usize, usize)> {
    tools
        .iter()
        .filter_map(change_counts)
        .reduce(|(a, d), (x, y)| (a + x, d + y))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #s182: an Artifact edit's card speaks only when the result applied fewer edits than sent.
    #[test]
    fn edits_applied_note_only_when_short() {
        assert_eq!(
            edits_applied_note(Some(1), 3).as_deref(),
            Some("Applied 1 of 3 edits")
        );
        assert_eq!(
            edits_applied_note(Some(0), 1).as_deref(),
            Some("Applied 0 of 1 edit")
        );
        assert_eq!(
            edits_applied_note(Some(2), 2),
            None,
            "all applied: nothing extra"
        );
        assert_eq!(edits_applied_note(None, 2), None, "no count: no claim");
        assert_eq!(edits_applied_note(Some(1), 0), None, "nothing sent");
    }

    /// #s155: a publish's removed files in a change's words — one, several, and past the names
    /// it spells out, where the rest are counted.
    #[test]
    fn removed_files_chip_and_note() {
        assert_eq!(removed_files_chip(1), "−1 file");
        assert_eq!(removed_files_chip(4), "−4 files");
        let files = |n: usize| (1..=n).map(|i| format!("old/{i}.json")).collect::<Vec<_>>();
        assert_eq!(
            removed_files_note(&files(1)),
            "Removed from the artifact: old/1.json"
        );
        assert_eq!(
            removed_files_note(&files(2)),
            "Removed from the artifact: old/1.json, old/2.json"
        );
        let many = removed_files_note(&files(REMOVED_FILES_SHOWN + 3));
        assert!(
            many.ends_with(&format!("old/{REMOVED_FILES_SHOWN}.json and 3 more")),
            "{many}"
        );
        assert!(!many.contains(&format!("old/{}.json", REMOVED_FILES_SHOWN + 1)));
    }

    /// #s23: what a call changed, and a run: a fresh Write adds its lines, an Edit counts its
    /// diff, a shell command counts the diff it recorded, a failed edit and any other call count
    /// nothing, and a run sums its calls.
    #[test]
    fn change_counts_and_a_runs_sum() {
        let call = |name: &str,
                    diffs: Vec<(&str, &str)>,
                    patch: Option<Vec<crate::model::Hunk>>| Block::ToolUse {
            name: name.into(),
            target: "x".into(),
            diffs: diffs
                .into_iter()
                .map(|(o, n)| (o.into(), n.into()))
                .collect(),
            output: None,
            patch,
            read_lines: None,
            cwd: String::new(),
            execution: None,
            published: None,
            asked: None,
            delivered: Vec::new(),
            sent: None,
            saved_output: None,
        };
        let fresh = call("Write", vec![("", "a\nb\nc\n")], None);
        let edit = call("Edit", vec![("one\ntwo", "one\nTWO\nthree")], None);
        let bash = call(
            "Bash",
            vec![],
            Some(vec![crate::model::Hunk {
                old_start: 1,
                new_start: 1,
                lines: vec!["-x".into(), "+y".into(), "+z".into()],
                file: Some("f.py".into()),
            }]),
        );
        let read = call("Bash", vec![], None);
        assert_eq!(change_counts(&fresh), Some((3, 0)));
        assert_eq!(change_counts(&edit), Some((2, 1)));
        assert_eq!(change_counts(&bash), Some((2, 1)));
        assert_eq!(change_counts(&read), None);
        let mut failed = edit.clone();
        if let Block::ToolUse { execution, .. } = &mut failed {
            *execution = Some(ToolExecution {
                status: Some(ToolStatus::Failed),
                exit_code: None,
                duration: None,
            });
        }
        assert_eq!(change_counts(&failed), None, "an edit that changed nothing");
        assert_eq!(
            run_changes(&[fresh, read.clone(), edit, bash, failed]),
            Some((7, 2))
        );
        assert_eq!(run_changes(&[read]), None);
    }

    /// #s22: what a file-changing call says when it changed nothing.
    #[test]
    fn a_not_applied_edit_names_why() {
        let ran = |status| ToolExecution {
            status: Some(status),
            exit_code: None,
            duration: None,
        };
        let failed = ran(ToolStatus::Failed);
        assert_eq!(
            not_applied(
                "Edit",
                "x.rs",
                false,
                Some(&failed),
                Some("Error: not found")
            ),
            Some(NotApplied::Failed("Error: not found".into()))
        );
        assert_eq!(
            not_applied(
                "Edit",
                "x.rs",
                false,
                Some(&failed),
                Some("File has not been read yet.")
            ),
            Some(NotApplied::Failed(
                "Error: File has not been read yet.".into()
            ))
        );
        assert_eq!(
            not_applied(
                "Write",
                "x.md",
                false,
                Some(&ran(ToolStatus::Cancelled)),
                None
            ),
            Some(NotApplied::Failed("Error: not applied".into()))
        );
        let declined = ran(ToolStatus::Declined);
        assert_eq!(
            not_applied("Edit", "x.rs", false, Some(&declined), None),
            Some(NotApplied::Rejected("User rejected update to x.rs".into()))
        );
        assert_eq!(
            not_applied("Write", "x.md", false, Some(&declined), None),
            Some(NotApplied::Rejected("User rejected write to x.md".into()))
        );
        assert_eq!(
            not_applied("Write", "x.md", true, Some(&declined), None),
            Some(NotApplied::Rejected("User rejected update to x.md".into()))
        );
        assert_eq!(not_applied("Edit", "x.rs", false, None, None), None);
        assert_eq!(
            not_applied(
                "Edit",
                "x.rs",
                false,
                Some(&ran(ToolStatus::Completed)),
                None
            ),
            None
        );
    }

    #[test]
    fn execution_summary_preserves_exit_status_and_subsecond_duration() {
        let execution = ToolExecution {
            status: Some(ToolStatus::Failed),
            exit_code: Some(7),
            duration: Some(ToolDuration {
                secs: 1,
                nanos: 230_000_000,
            }),
        };
        assert_eq!(tool_execution_summary(&execution), "exit 7 · 1.23s");
        assert!(tool_execution_failed(&execution));
    }

    /// #s126: an exit the client calls no failure is recorded Completed beside its exit code, and
    /// keeps the code on its head without failure presentation; a Failed exit 1 is still a failure,
    /// and so is a bare non-zero exit with no status.
    #[test]
    fn tool_execution_failed_yields_to_an_explicit_completed() {
        let ran = |status, exit_code| ToolExecution {
            status,
            exit_code,
            duration: None,
        };
        let interpreted = ran(Some(ToolStatus::Completed), Some(1));
        assert!(!tool_execution_failed(&interpreted));
        assert_eq!(tool_execution_summary(&interpreted), "exit 1");
        assert!(tool_execution_failed(&ran(
            Some(ToolStatus::Failed),
            Some(1)
        )));
        assert!(tool_execution_failed(&ran(None, Some(1))));
        assert!(!tool_execution_failed(&ran(None, Some(0))));
        assert!(tool_execution_failed(&ran(
            Some(ToolStatus::Cancelled),
            None
        )));
    }

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
