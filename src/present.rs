//! Shared viewer TEXT formatters — the plain-string summaries the TUI renderer and the
//! HTML exporter both need (spawn chips, activity/turn summaries, tool display names, edit
//! summaries, …). Pure text over `crate::model::{Block, SubAgent}`; no ratatui/theme.

use crate::model::{Block, SubAgent};

/// Direct tool calls in a sub-agent's child transcript (activity tools absorbed into a
/// `Thinking` turn are counted too, since grouping folds Bash/Read/… into it).
pub(crate) fn tool_count(sa: &SubAgent) -> usize {
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
pub(crate) fn spawn_chip(sa: &SubAgent) -> String {
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
pub(crate) const WRITE_PREVIEW: usize = 10;

/// `Added N lines[, removed M lines]` (singular/plural; "removed" omitted at 0) —
/// the Edit/MultiEdit result summary, matching Claude Code.
pub(crate) fn edit_summary(adds: usize, dels: usize) -> String {
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
pub(crate) fn display_name(name: &str) -> &str {
    match name {
        "Edit" | "MultiEdit" => "Update",
        other => other,
    }
}

/// A summary of a grouped tool turn: `<activities>, thought for Xs` — the activities lead
/// (the tool calls ran first, feeding the thinking) then the thinking. A bare turn is just
/// `Thought for 8s`. The duration (`Xs` / `Xm Ys`) is omitted when unknown.
pub(crate) fn turn_summary(duration_secs: Option<u64>, tools: &[Block]) -> String {
    let thought = match duration_secs {
        Some(d) if d >= 60 => format!("thought for {}m {}s", d / 60, d % 60),
        Some(d) => format!("thought for {d}s"),
        None => "thought".to_string(),
    };
    let acts = activities(tools);
    if acts.is_empty() {
        capitalize(&thought)
    } else {
        format!("{}, {thought}", capitalize(&acts))
    }
}

/// A Write/NotebookEdit's body: the first non-empty *new-side* text across its diffs (the
/// transcript records a Write as a diff whose new side is the whole file). Shared so the TUI
/// and HTML agree on which diff supplies the content.
pub(crate) fn write_content(diffs: &[(String, String)]) -> &str {
    diffs
        .iter()
        .map(|(_, n)| n.as_str())
        .find(|n| !n.is_empty())
        .unwrap_or("")
}

/// The collapsed one-line summary of a `Thinking` turn — shared by the TUI and the HTML
/// exporter (each prepends its own glyph) so the wording can't drift between them. A pure
/// coalesced-activity run (no thinking text, no duration) is just the activities; otherwise
/// `<activities>, thought for Xs`; a bare thinking block with neither falls back to a line count.
pub(crate) fn thinking_summary(text: &str, duration_secs: Option<u64>, tools: &[Block]) -> String {
    if text.trim().is_empty() && duration_secs.is_none() && !tools.is_empty() {
        capitalize(&activities(tools))
    } else if duration_secs.is_some() || !tools.is_empty() {
        turn_summary(duration_secs, tools)
    } else {
        format!("Thought ({} lines)", text.lines().count())
    }
}

/// Uppercase the first character of `s` (ASCII-friendly; leaves the rest as-is).
pub(crate) fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// The representative program name of a (possibly compound) shell command, e.g.
/// `echo "==="; PROFILE=1 zsh -i -c exit | tail` → `zsh`. Splits on shell
/// separators and, per segment, skips leading `VAR=value` assignments; a whole
/// segment whose command is pure preamble (`echo`/`cd`/`for`/…) is skipped, and
/// wrapper prefixes (`sudo`/`time`/`do`/…) are stepped over to the real command.
/// Falls back to the first token's basename. `None` only for an empty command.
fn command_name(cmd: &str) -> Option<String> {
    // Whole segment is noise (its arguments aren't commands). Includes the shell
    // block-closer keywords (`fi`/`done`/`esac`/`in`) so a compound script's control
    // structure isn't mistaken for a command.
    const SKIP_SEGMENT: &[&str] = &[
        "echo", "printf", "cd", "true", "false", ":", "set", "export", "unset", "source", ".",
        "for", "while", "until", "if", "case", "test", "[", "[[", "return", "eval", "fi", "done",
        "esac", "in",
    ];
    // Prefix wrapper: the real command is the next token.
    const SKIP_PREFIX: &[&str] = &[
        "do", "then", "else", "elif", "time", "env", "sudo", "command", "builtin", "exec", "nohup",
        "xargs", "{", "(", "!",
    ];
    let is_env = |t: &str| {
        t.split_once('=').is_some_and(|(k, _)| {
            !k.is_empty()
                && k.chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        })
    };
    // A token that actually looks like a command word — a program/function name,
    // not shell punctuation. Rejects block terminators (`}`), case labels
    // (`completion)`), function-definition headers (`run_wire()`), comments (`#`),
    // and `var=value` — all of which flattened heredoc scripts scatter into
    // separators, and none of which should surface as a command name.
    let plausible = |t: &str| {
        let mut cs = t.chars();
        cs.next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '/')
            && t.chars()
                .all(|c| c.is_ascii_alphanumeric() || "_.-+/@".contains(c))
    };
    let base = |t: &str| t.rsplit(['/', '\\']).next().unwrap_or(t).to_string();
    for seg in cmd.split([';', '|', '&', '\n']) {
        let mut toks = seg.split_whitespace().filter(|t| !is_env(t));
        let Some(mut name) = toks.next().map(base) else {
            continue;
        };
        if SKIP_SEGMENT.contains(&name.as_str()) {
            continue;
        }
        while SKIP_PREFIX.contains(&name.as_str()) {
            match toks.next() {
                Some(t) => name = base(t),
                None => break,
            }
        }
        // Only accept a real command word; otherwise this segment is structural
        // noise (a brace, case label, comment, …) — move on to the next.
        if plausible(&name) {
            return Some(name);
        }
    }
    cmd.split_whitespace()
        .map(base)
        .find(|t| plausible(t))
        .or_else(|| cmd.split_whitespace().next().map(base))
}

/// Summarize grouped tool calls as `listed N directories, searched for N patterns,
/// read N files, ran N shell commands (name, …), used N tools` (each clause omitted
/// at 0). Extends Claude Code's turn line with the shell program names.
pub(crate) fn activities(tools: &[Block]) -> String {
    let s = |n: usize| if n == 1 { "" } else { "s" };
    let (mut dir, mut pat, mut file, mut other) = (0, 0, 0, 0);
    let mut shell_names: Vec<String> = Vec::new();
    for t in tools {
        if let Block::ToolUse { name, target, .. } = t {
            match name.as_str() {
                "Bash" => shell_names.push(command_name(target).unwrap_or_else(|| "sh".into())),
                "Read" | "NotebookRead" => file += 1,
                "Grep" | "Glob" => pat += 1,
                "LS" => dir += 1,
                _ => other += 1,
            }
        }
    }
    let mut parts = Vec::new();
    if dir > 0 {
        parts.push(format!(
            "listed {dir} director{}",
            if dir == 1 { "y" } else { "ies" }
        ));
    }
    if pat > 0 {
        parts.push(format!("searched for {pat} pattern{}", s(pat)));
    }
    if file > 0 {
        parts.push(format!("read {file} file{}", s(file)));
    }
    if !shell_names.is_empty() {
        let n = shell_names.len();
        // Distinct program names, first-seen order — 9 `git`s read as "(git)".
        let mut seen = std::collections::HashSet::new();
        let uniq: Vec<&str> = shell_names
            .iter()
            .map(String::as_str)
            .filter(|nm| seen.insert(*nm))
            .collect();
        parts.push(format!(
            "ran {n} shell command{} ({})",
            s(n),
            uniq.join(", ")
        ));
    }
    if other > 0 {
        parts.push(format!("used {other} tool{}", s(other)));
    }
    parts.join(", ")
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
        let blocks = crate::engine::parse_session_enriched_as(crate::Agent::Claude, &sess)
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

    #[test]
    fn command_name_extracts_the_real_program() {
        let c = |s: &str| command_name(s).unwrap();
        assert_eq!(c("ls -la"), "ls");
        assert_eq!(c("/usr/bin/time zsh -i -c exit"), "zsh"); // step over the `time` wrapper
        assert_eq!(c("echo \"=== hi ===\"; grep -n foo bar"), "grep"); // skip the echo header
        assert_eq!(c("PROFILE=1 zsh -i -c exit | tail -1"), "zsh"); // env assign + pipe filter
        assert_eq!(c("git status | grep modified"), "git");
        assert_eq!(c("{ zmodload zsh/zprof; exit; }"), "zmodload"); // step into the brace group

        // A flattened heredoc script that defines functions/case blocks must not
        // surface shell punctuation as the "program" — it lands on the first real
        // invocation instead of a bare `}` / `completion)` / `run_wire()`.
        let script = "cd /tmp # note  run_wire() { info() { printf '%s' \"$*\"; }  \
            rowt() { case \"$1\" in shell-init) echo x; return 0;; completion) return 0;; esac; } }  \
            rc=fresh.zshrc; : > \"$rc\" run_wire \"$rc\"; run_wire \"$rc\"";
        let got = command_name(script).unwrap();
        assert_eq!(got, "run_wire", "leaked shell punctuation: {got:?}");
        // Direct: a segment that is only a block terminator yields no name.
        assert_eq!(c("} ; grep -n x y"), "grep");
    }
}
