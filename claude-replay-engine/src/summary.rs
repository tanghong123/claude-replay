//! The **span summarization vocabulary** — Claude Code's exact clause phrasing for a
//! coalesced work span (#57, `design/cc-activity-coalescing.md`), shared by every
//! presenter so wording cannot drift between frontends. Moved into the core from the
//! viewer's `present` module per the #58 extensibility study (relocation only — the
//! per-agent `summarize` hook stays gated on that study's trigger conditions). Pure
//! over [`Block`]; no presentation deps, so the core's boundary holds.

use crate::model::Block;

/// A summary of a coalesced work span: `Thought for Xs, <activities>` — Claude Code's
/// exact clause order (#57, `design/cc-activity-coalescing.md`): the thought leads,
/// then the activity clauses. A bare span is just `Thought for 8s`. The duration
/// (`Xs` / `Xm Ys`) clause degrades to bare "Thought" when unknown.
pub fn turn_summary(duration_secs: Option<u64>, tools: &[Block]) -> String {
    let thought = match duration_secs {
        Some(d) if d >= 60 => format!("thought for {}m {}s", d / 60, d % 60),
        Some(0) => "thought for <1s".to_string(),
        Some(d) => format!("thought for {d}s"),
        None => "thought".to_string(),
    };
    let acts = activities(tools);
    if acts.is_empty() {
        capitalize(&thought)
    } else {
        format!("{}, {acts}", capitalize(&thought))
    }
}

/// The collapsed one-line summary of a `Thinking` turn — shared by the TUI and the HTML
/// exporter (each prepends its own glyph) so the wording can't drift between them. A pure
/// coalesced-activity run (no thinking text, no duration) is just the activities; otherwise
/// `<activities>, thought for Xs`; a bare thinking block with neither falls back to a line count.
pub fn thinking_summary(text: &str, duration_secs: Option<u64>, tools: &[Block]) -> String {
    if text.trim().is_empty() && duration_secs.is_none() && !tools.is_empty() {
        capitalize(&activities(tools))
    } else if duration_secs.is_some() || !tools.is_empty() {
        turn_summary(duration_secs, tools)
    } else {
        format!("Thought ({} lines)", text.lines().count())
    }
}

/// Uppercase the first character of `s` (ASCII-friendly; leaves the rest as-is).
pub fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// How one Bash command folds into a span's activity clauses — Claude Code's semantic
/// classes (#57, `design/cc-activity-coalescing.md`).
enum BashClass {
    /// `git commit`/`git push` whose OUTPUT parsed: commit short-hashes and pushed-to
    /// branch names ("Committed 1a2b3c", "Pushed to main"). Not counted as "ran".
    Git {
        hashes: Vec<String>,
        branches: Vec<String>,
    },
    /// A single search command (grep/rg/find/…) → "searched for N patterns".
    Search,
    /// A single read command (cat/head/tail/…) → dedupes into "read N files".
    ReadFiles(Vec<String>),
    /// A single `ls`-alike → "listed N directories".
    List,
    /// Anything else → "ran N shell commands".
    Ran,
}

/// Classify a Bash command the way Claude Code does: git phrases parse the OUTPUT
/// (a `-q` commit or failed push yields nothing and falls through; compounds DO
/// phrase — an `add && commit && push` chain reads "committed …, pushed to …").
/// Otherwise only a SINGLE simple command classifies semantically by its first word;
/// any compound — pipes, newlines, `;`, `&` — is a plain shell command (evidenced on
/// the dev session: CC counts its `cd X\ngrep … | head` compounds under "ran N shell
/// commands", never as searches/reads).
fn classify_bash(cmd: &str, output: Option<&str>) -> BashClass {
    // Git phrases first: `git commit` / `git push` anywhere in the (possibly
    // compound) command, evidence taken from the output. git writes push branch
    // lines to stderr; Bash blocks carry stdout+stderr combined.
    if cmd.contains("git commit") || cmd.contains("git push") {
        let out = output.unwrap_or("");
        let mut hashes = Vec::new();
        let mut branches = Vec::new();
        if cmd.contains("git commit") {
            // "[branch 1a2b3c] msg" (root commits: "[branch (root-commit) 1a2b3c]").
            for line in out.lines() {
                if let Some(inner) = line.strip_prefix('[').and_then(|r| r.split(']').next()) {
                    if let Some(h) = inner.split_whitespace().last() {
                        if h.len() >= 4 && h.chars().all(|c| c.is_ascii_hexdigit()) {
                            hashes.push(h.to_string());
                        }
                    }
                }
            }
        }
        if cmd.contains("git push") {
            // "   abc..def  main -> main" / " * [new branch]  feat -> feat". Tag
            // pushes (" * [new tag]  v1.1.0 -> v1.1.0") are NOT branches — CC's line
            // for a commit+push-with-tag compound reads "pushed to main" alone.
            for line in out.lines() {
                if line.contains("[new tag]") {
                    continue;
                }
                if let Some((_, rest)) = line.split_once("-> ") {
                    if let Some(b) = rest.split_whitespace().next() {
                        if !branches.iter().any(|x| x == b) {
                            branches.push(b.to_string());
                        }
                    }
                }
            }
        }
        if !hashes.is_empty() || !branches.is_empty() {
            return BashClass::Git { hashes, branches };
        }
    }
    const SEARCH: &[&str] = &["grep", "egrep", "fgrep", "rg", "ag", "ack", "find", "fd"];
    const READ: &[&str] = &["cat", "head", "tail", "less", "more", "bat"];
    const LIST: &[&str] = &["ls", "tree", "eza", "exa"];
    let cmd = cmd.trim();
    if cmd.contains(['\n', ';', '|', '&']) {
        return BashClass::Ran;
    }
    let mut toks = cmd.split_whitespace();
    let Some(first) = toks
        .next()
        .map(|t| t.rsplit(['/', '\\']).next().unwrap_or(t))
    else {
        return BashClass::Ran;
    };
    if SEARCH.contains(&first) {
        BashClass::Search
    } else if READ.contains(&first) {
        // The file argument (skip flags and bare numbers), for unique-file dedup
        // against `Read` tool targets.
        match toks.find(|t| !t.starts_with('-') && !t.chars().all(|c| c.is_ascii_digit())) {
            Some(f) => BashClass::ReadFiles(vec![f.to_string()]),
            None => BashClass::Ran,
        }
    } else if LIST.contains(&first) {
        BashClass::List
    } else {
        BashClass::Ran
    }
}

/// Summarize a span's tool calls in Claude Code's exact clause vocabulary and order
/// (#57): `committed <hashes>, pushed to <branches>, searched for N patterns, read N
/// files, listed N directories, called <mcp-server> N times, ran N shell commands` —
/// each clause omitted at zero (the MCP clause per server, first-seen order; #90).
/// Files count UNIQUE paths (a bash `cat` of a file dedupes against a `Read` of it);
/// searches/listings/plain shells count occurrences; git-phrased commands aren't
/// double-counted as "ran". CC names no shell programs, so neither do we.
pub fn activities(tools: &[Block]) -> String {
    let s = |n: usize| if n == 1 { "" } else { "s" };
    let (mut pat, mut dir, mut ran) = (0usize, 0usize, 0usize);
    // MCP calls count per SERVER (`mcp__<server>__<tool>`), clause per server in
    // first-seen order: CC renders `called claude-in-chrome 4 times` (#90).
    let mut mcp: Vec<(String, usize)> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    let mut file_seen = std::collections::HashSet::new();
    let mut hashes: Vec<String> = Vec::new();
    let mut branches: Vec<String> = Vec::new();
    let mut push_file = |f: String, seen: &mut std::collections::HashSet<String>| {
        if seen.insert(f.clone()) {
            files.push(f);
        }
    };
    for t in tools {
        if let Block::ToolUse {
            name,
            target,
            output,
            ..
        } = t
        {
            match name.as_str() {
                "Bash" => match classify_bash(target, output.as_deref()) {
                    BashClass::Git {
                        hashes: h,
                        branches: b,
                    } => {
                        hashes.extend(h);
                        for br in b {
                            if !branches.contains(&br) {
                                branches.push(br);
                            }
                        }
                    }
                    BashClass::Search => pat += 1,
                    BashClass::ReadFiles(fs) => {
                        for f in fs {
                            push_file(f, &mut file_seen);
                        }
                    }
                    BashClass::List => dir += 1,
                    BashClass::Ran => ran += 1,
                },
                "Read" | "NotebookRead" => push_file(target.clone(), &mut file_seen),
                "Grep" | "Glob" => pat += 1,
                "LS" => dir += 1,
                n if n.starts_with("mcp__") => {
                    let server = n.split("__").nth(1).unwrap_or(n).to_string();
                    match mcp.iter_mut().find(|(sv, _)| *sv == server) {
                        Some((_, c)) => *c += 1,
                        None => mcp.push((server, 1)),
                    }
                }
                _ => ran += 1,
            }
        }
    }
    let mut parts = Vec::new();
    if !hashes.is_empty() {
        parts.push(format!("committed {}", hashes.join(", ")));
    }
    if !branches.is_empty() {
        parts.push(format!("pushed to {}", branches.join(", ")));
    }
    if pat > 0 {
        parts.push(format!("searched for {pat} pattern{}", s(pat)));
    }
    if !files.is_empty() {
        let n = files.len();
        parts.push(format!("read {n} file{}", s(n)));
    }
    if dir > 0 {
        parts.push(format!(
            "listed {dir} director{}",
            if dir == 1 { "y" } else { "ies" }
        ));
    }
    for (server, n) in &mcp {
        parts.push(format!("called {server} {n} time{}", s(*n)));
    }
    if ran > 0 {
        parts.push(format!("ran {ran} shell command{}", s(ran)));
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str, target: &str, output: Option<&str>) -> Block {
        Block::ToolUse {
            name: name.into(),
            target: target.into(),
            diffs: Vec::new(),
            output: output.map(String::from),
            patch: None,
            read_lines: None,
        }
    }

    /// The CC bash classes (#57, `design/cc-activity-coalescing.md`): single-segment
    /// commands classify semantically; a `cd`-led newline compound is ONE segment whose
    /// first word is noise, so the whole command stays a plain shell command; git
    /// commit/push phrases require parseable OUTPUT and otherwise fall through.
    /// #90: MCP calls coalesce and phrase per server — observed on CC 2.x:
    /// `called claude-in-chrome 4 times, ran 3 shell commands` (called before ran;
    /// servers in first-seen order; singular "1 time").
    #[test]
    fn mcp_calls_phrase_per_server() {
        let tools = vec![
            tool("mcp__claude-in-chrome__javascript_tool", "", None),
            tool("Bash", "cargo build", None),
            tool("mcp__claude-in-chrome__navigate", "", None),
            tool("mcp__okr__authenticate", "", None),
            tool("mcp__claude-in-chrome__computer", "", None),
        ];
        assert_eq!(
            activities(&tools),
            "called claude-in-chrome 3 times, called okr 1 time, ran 1 shell command"
        );
    }

    #[test]
    fn bash_classifies_like_claude_code() {
        let act = |cmd: &str, out: Option<&str>| activities(&[tool("Bash", cmd, out)]);
        assert_eq!(act("grep -n foo notes.txt", None), "searched for 1 pattern");
        assert_eq!(act("cat notes.txt", None), "read 1 file");
        assert_eq!(act("ls -la", None), "listed 1 directory");
        assert_eq!(act("cargo build", None), "ran 1 shell command");
        // ANY compound — newlines, pipes, `;`, `&` — is a plain shell command
        // (matches CC: the dev session's `cd X\ngrep … | head` compounds all counted
        // under "ran N shell commands" in its own display, never as searches/reads).
        assert_eq!(act("cd /x\ngrep -n pat file", None), "ran 1 shell command");
        assert_eq!(act("grep foo | head -5", None), "ran 1 shell command");
        assert_eq!(act("grep foo | python3 x.py", None), "ran 1 shell command");
        // Git phrases parse the output; branch names come from the push output.
        assert_eq!(
            act("git commit -m x", Some("[main 1a2b3c] x")),
            "committed 1a2b3c"
        );
        assert_eq!(
            act(
                "git push origin main",
                Some("To github.com:x/y.git\n   aa..bb  dev -> dev")
            ),
            "pushed to dev"
        );
        // Compound add+commit+push yields both clauses from one command.
        assert_eq!(
            act(
                "git add -A\ngit commit -m x\ngit push origin main",
                Some("[main 3c4d5e] x\nTo github.com:x/y.git\n   cc..dd  main -> main")
            ),
            "committed 3c4d5e, pushed to main"
        );
        // Failed push (no `->` line) and a `-q` commit (no output) fall back to ran.
        assert_eq!(
            act(
                "git push origin main",
                Some("error: failed to push some refs")
            ),
            "ran 1 shell command"
        );
        assert_eq!(act("git commit -q -m x", Some("")), "ran 1 shell command");
    }

    /// Clause vocabulary, fixed order, and dedup — the S4/S3 probe renders verbatim:
    /// committed → pushed → searched → read → listed → ran; files dedupe by path
    /// (bash `cat` against `Read` too); push branches merge; listings count occurrences.
    #[test]
    fn activities_matches_cc_vocabulary_and_order() {
        let tools = [
            tool("Read", "a.txt", None),
            tool("Bash", "grep -n pat a.txt", Some("1:pat")),
            tool("Bash", "ls", Some("a.txt")),
            tool("Bash", "git commit -m msg", Some("[main 1a2b3c] msg")),
            tool(
                "Bash",
                "git push origin main",
                Some("To github.com:x/y.git\n   aa..bb  main -> main"),
            ),
            tool("Bash", "cargo build", Some("ok")),
        ];
        assert_eq!(
            activities(&tools),
            "committed 1a2b3c, pushed to main, searched for 1 pattern, read 1 file, \
             listed 1 directory, ran 1 shell command"
        );
        // Unique-file dedup: Read a, Read a, cat a, Read b → 2 files; ls twice → 2 dirs.
        let tools = [
            tool("Read", "a.txt", None),
            tool("Read", "a.txt", None),
            tool("Bash", "cat a.txt", Some("x")),
            tool("Read", "b.txt", None),
            tool("Bash", "ls /tmp", Some("f")),
            tool("Bash", "ls /tmp", Some("f")),
        ];
        assert_eq!(activities(&tools), "read 2 files, listed 2 directories");
        // Two pushes merge into one clause, first-seen order.
        let tools = [
            tool("Bash", "git push origin dev", Some("   ee..ff  dev -> dev")),
            tool(
                "Bash",
                "git push origin main",
                Some("   gg..hh  main -> main"),
            ),
        ];
        assert_eq!(activities(&tools), "pushed to dev, main");
    }

    /// The span summary leads with the thought — CC's order — and formats ≥60s as XmYs.
    #[test]
    fn turn_summary_leads_with_thought() {
        let tools = [tool("Bash", "cargo test", Some("ok"))];
        assert_eq!(
            turn_summary(Some(20), &tools),
            "Thought for 20s, ran 1 shell command"
        );
        assert_eq!(turn_summary(Some(75), &[]), "Thought for 1m 15s");
        assert_eq!(
            thinking_summary("", None, &tools),
            "Ran 1 shell command",
            "activity-only span capitalizes the first clause"
        );
    }

    #[test]
    fn zero_second_duration_renders_as_subsecond() {
        assert_eq!(turn_summary(Some(0), &[]), "Thought for <1s");
    }
}
