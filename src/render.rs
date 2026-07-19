//! Blocks -> styled ratatui lines. Each emitted line is tagged with its source
//! block index so the viewer can fold/expand and hit-test mouse clicks.

use crate::model::Block;
use crate::{highlight, markdown, theme};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

/// Rendered lines plus a parallel "which block produced this line" vector.
pub struct Rendered {
    pub lines: Vec<Line<'static>>,
    pub block_of: Vec<usize>,
}

/// Blocks whose body can be collapsed to a one-line placeholder.
pub fn foldable(b: &Block) -> bool {
    matches!(
        b,
        Block::ToolUse { .. }
            | Block::ToolResult(_)
            | Block::Thinking { .. }
            | Block::Command { .. }
    )
}

/// The `❯ /command [args]` header line for a slash-command block — styled like a
/// user turn (dim `❯` caret + near-white text on the grey block), as Claude Code.
fn command_header(name: &str, args: &str) -> Line<'static> {
    let base = Style::default().fg(theme::user_fg()).bg(theme::user_bg());
    // Single-line summary (collapsed / header): first arg line only, with an `…`
    // when the args span more lines (the full body shows when expanded).
    let body = if args.is_empty() {
        name.to_string()
    } else {
        let mut lines = args.lines();
        let first = lines.next().unwrap_or("");
        if lines.next().is_some() {
            format!("{name} {first}…")
        } else {
            format!("{name} {first}")
        }
    };
    Line::from(vec![
        Span::styled("❯ ", base.fg(theme::user_marker())),
        Span::styled(body, base),
    ])
}

/// The full multi-line `❯ /command <args>` header for an *expanded* command block:
/// one styled line per source line of `args`, so embedded newlines aren't lost.
/// The first line carries the `❯` caret; continuation lines indent two spaces under
/// it (mirroring multi-line `UserText`), all on the user-tier block bg.
fn command_header_lines(name: &str, args: &str) -> Vec<Line<'static>> {
    let base = Style::default().fg(theme::user_fg()).bg(theme::user_bg());
    let caret = base.fg(theme::user_marker());
    if args.is_empty() {
        return vec![Line::from(vec![
            Span::styled("❯ ", caret),
            Span::styled(name.to_string(), base),
        ])];
    }
    args.lines()
        .enumerate()
        .map(|(i, line)| {
            if i == 0 {
                Line::from(vec![
                    Span::styled("❯ ", caret),
                    Span::styled(format!("{name} {line}"), base),
                ])
            } else {
                Line::from(Span::styled(format!("  {line}"), base))
            }
        })
        .collect()
}

/// One step of a line-level diff.
enum LineOp<'a> {
    Eq(&'a str),
    Del(&'a str),
    Ins(&'a str),
}

/// Line-level LCS → an ordered op sequence (unchanged lines stay as context,
/// only genuinely changed runs become -/+). Avoids the old index-zip that
/// mispaired every line after an insertion/deletion.
fn line_diff<'a>(ol: &[&'a str], nl: &[&'a str]) -> Vec<LineOp<'a>> {
    let (n, m) = (ol.len(), nl.len());
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if ol[i] == nl[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut ops = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if ol[i] == nl[j] {
            ops.push(LineOp::Eq(ol[i]));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            ops.push(LineOp::Del(ol[i]));
            i += 1;
        } else {
            ops.push(LineOp::Ins(nl[j]));
            j += 1;
        }
    }
    while i < n {
        ops.push(LineOp::Del(ol[i]));
        i += 1;
    }
    while j < m {
        ops.push(LineOp::Ins(nl[j]));
        j += 1;
    }
    ops
}

/// Added/removed line counts from a line-level diff (for the `└ Updated` header).
fn diff_counts(old: &str, new: &str) -> (usize, usize) {
    let ol: Vec<&str> = old.lines().collect();
    let nl: Vec<&str> = new.lines().collect();
    let (mut adds, mut dels) = (0usize, 0usize);
    for op in line_diff(&ol, &nl) {
        match op {
            LineOp::Ins(_) => adds += 1,
            LineOp::Del(_) => dels += 1,
            LineOp::Eq(_) => {}
        }
    }
    (adds, dels)
}

/// Claude Code shows only the first `WRITE_PREVIEW` lines of a file write, then a
/// `… +N lines` marker (the full content isn't dumped into the transcript view).
const WRITE_PREVIEW: usize = 10;

/// Render a whole-new-file write as syntax-highlighted, line-numbered code (no
/// `+` gutter), capped to a preview like Claude Code: `{6 spaces}{num right-aligned}
/// {code}` for the first `WRITE_PREVIEW` lines, then `     … +N lines`.
fn write_numbered(content: &str, token: &str, out: &mut Vec<Line<'static>>) {
    let lines: Vec<&str> = content.lines().collect();
    let shown = lines.len().min(WRITE_PREVIEW);
    // Gutter width from the largest *shown* number (min 2), as CC does.
    let gutter = shown.to_string().len().max(2);
    let hl = highlight::highlight_spans(content, token);
    for (i, l) in lines.iter().take(shown).enumerate() {
        // 6-space margin + right-aligned number + one space, then the code.
        let mut spans = vec![
            Span::raw(" ".repeat(crate::view::INSET)),
            Span::styled(format!("{:>gutter$} ", i + 1), theme::dim()),
        ];
        match hl.get(i) {
            Some(line_spans) if !line_spans.is_empty() => spans.extend(line_spans.iter().cloned()),
            _ => spans.push(Span::raw(l.to_string())),
        }
        out.push(Line::from(spans));
    }
    if lines.len() > shown {
        out.push(Line::styled(
            format!("     … +{} lines", lines.len() - shown),
            theme::dim(),
        ));
    }
}

/// One diff row: `  <gutter> <marker> <syntax-highlighted code>`. `bg`, when set,
/// fills the whole row (gutter + marker + code) so added/removed lines read as
/// colored blocks like Claude Code; context rows pass `bg = None`.
fn diff_row(
    gw: usize,
    num: Option<usize>,
    marker: char,
    text: &str,
    token: &str,
    bg: Option<Color>,
) -> Line<'static> {
    let gutter = match num {
        Some(n) => format!("{n:>gw$}"),
        None => " ".repeat(gw),
    };
    let patch = |s: Style| match bg {
        Some(c) => s.bg(c),
        None => s,
    };
    let marker_style = match marker {
        '+' => theme::diff_add(),
        '-' => theme::diff_del(),
        _ => theme::dim(),
    };
    let mut spans = Vec::new();
    // Context rows (no bg) never reach `fill_bg`'s inset, so indent them here by
    // the same INSET that `fill_bg` applies to the +/− rows — keeps gutters aligned.
    if bg.is_none() {
        spans.push(Span::raw(" ".repeat(crate::view::INSET)));
    }
    // CC layout: `{gutter} {marker}{code}` — one space after the gutter, the
    // marker (+/-/space) directly before the code (the code keeps its own indent).
    // CC colours the whole gutter+marker run with the marker colour (green/red on
    // +/- rows, dim on context).
    spans.push(Span::styled(format!("{gutter} "), patch(marker_style)));
    spans.push(Span::styled(marker.to_string(), patch(marker_style)));
    for mut sp in highlight::highlight_one(text, token) {
        sp.style = patch(sp.style);
        spans.push(sp);
    }
    Line::from(spans)
}

/// How many lines `diff_lines` will emit for this (old,new) pair — computed
/// cheaply (no `Line` allocation) so a collapsed block's `⋯ N folded` count is
/// exact without building the body. Mirrors `diff_lines`' pairing rule.
fn diff_rendered_len(old: &str, new: &str) -> usize {
    let ol: Vec<&str> = old.lines().collect();
    let nl: Vec<&str> = new.lines().collect();
    let ops = line_diff(&ol, &nl);
    let (mut n, mut k) = (0usize, 0usize);
    while k < ops.len() {
        match ops[k] {
            LineOp::Eq(_) => {
                n += 1;
                k += 1;
            }
            _ => {
                let mut dels = 0;
                while let Some(LineOp::Del(_)) = ops.get(k) {
                    dels += 1;
                    k += 1;
                }
                let mut inss = 0;
                while let Some(LineOp::Ins(_)) = ops.get(k) {
                    inss += 1;
                    k += 1;
                }
                let pairs = dels.min(inss);
                n += pairs * 2 + (dels - pairs) + (inss - pairs);
            }
        }
    }
    n
}

fn diff_lines(old: &str, new: &str, token: &str, out: &mut Vec<Line<'static>>) {
    let ol: Vec<&str> = old.lines().collect();
    let nl: Vec<&str> = new.lines().collect();
    let ops = line_diff(&ol, &nl);

    // Local hunk numbering over the NEW side. The transcript's Edit payload has
    // no absolute file line numbers, so this numbers 1..N within the hunk — an
    // intentional approximation (context + additions get numbers; deletions
    // don't exist on the new side, so they show a blank gutter).
    let new_total = ops
        .iter()
        .filter(|o| matches!(o, LineOp::Eq(_) | LineOp::Ins(_)))
        .count();
    let gw = new_total.to_string().len().max(1);

    // No truncation — the whole hunk is emitted (folding controls cost).
    let mut n = 0usize;
    for op in &ops {
        match op {
            LineOp::Eq(l) => {
                n += 1;
                out.push(diff_row(gw, Some(n), ' ', l, token, None));
            }
            LineOp::Del(l) => {
                out.push(diff_row(
                    gw,
                    None,
                    '-',
                    l,
                    token,
                    Some(theme::diff_del_bg()),
                ));
            }
            LineOp::Ins(l) => {
                n += 1;
                out.push(diff_row(
                    gw,
                    Some(n),
                    '+',
                    l,
                    token,
                    Some(theme::diff_add_bg()),
                ));
            }
        }
    }
}

/// Render Edit/MultiEdit hunks from the transcript's `structuredPatch`, which
/// carries **real file line numbers** (`new_start`). Context/added rows are
/// numbered on the new side; deletions get a blank gutter. Add/del rows fill with
/// the diff bg; code is syntax-highlighted by `token`.
fn render_patch(hunks: &[crate::model::Hunk], token: &str, out: &mut Vec<Line<'static>>) {
    for h in hunks {
        // Gutter width from the largest line number in this hunk — CC numbers both
        // sides (added/context on the new side, removed on the old side).
        let new_lines = h.lines.iter().filter(|l| !l.starts_with('-')).count();
        let old_lines = h.lines.iter().filter(|l| !l.starts_with('+')).count();
        let new_last = h.new_start + new_lines.saturating_sub(1);
        let old_last = h.old_start + old_lines.saturating_sub(1);
        let gw = new_last.max(old_last).to_string().len().max(1);
        let mut n = h.new_start;
        let mut o = h.old_start;
        for line in &h.lines {
            let marker = line.chars().next().unwrap_or(' ');
            let text = line.get(marker.len_utf8()..).unwrap_or("");
            match marker {
                '+' => {
                    out.push(diff_row(
                        gw,
                        Some(n),
                        '+',
                        text,
                        token,
                        Some(theme::diff_add_bg()),
                    ));
                    n += 1;
                }
                '-' => {
                    out.push(diff_row(
                        gw,
                        Some(o),
                        '-',
                        text,
                        token,
                        Some(theme::diff_del_bg()),
                    ));
                    o += 1;
                }
                _ => {
                    out.push(diff_row(gw, Some(n), ' ', text, token, None));
                    n += 1;
                    o += 1;
                }
            }
        }
    }
}

/// Render a single block's content lines (no trailing blank separator). `width`
/// is the terminal width, used for width-aware table layout in assistant text.
fn render_one(b: &Block, width: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    match b {
        Block::UserText(t) => {
            // A full-width grey block like Claude Code: a dim `❯` caret on the
            // first line (continuation lines indent two spaces to align under it),
            // near-white text; `fill_bg` extends the background across the row.
            let base = Style::default().fg(theme::user_fg()).bg(theme::user_bg());
            let caret = base.fg(theme::user_marker());
            for (i, line) in t.lines().enumerate() {
                if i == 0 {
                    out.push(Line::from(vec![
                        Span::styled("❯ ", caret),
                        Span::styled(line.to_string(), base),
                    ]));
                } else {
                    out.push(Line::from(Span::styled(format!("  {line}"), base)));
                }
            }
        }
        Block::AssistantText(t) => {
            let mut md = markdown::render(t, width);
            if md.is_empty() {
                md.push(Line::from(Span::styled("⏺", theme::assistant_marker())));
            } else {
                // First line carries the ⏺ marker (flush left); every other
                // non-blank body line is indented two spaces so the whole turn
                // aligns under the marker. Blank lines stay empty so they still
                // collapse as separators.
                for (i, line) in md.iter_mut().enumerate() {
                    if i == 0 {
                        // Marker glyph is coloured; the following space is plain
                        // (Claude Code resets before it).
                        line.spans.insert(0, Span::raw(" "));
                        line.spans
                            .insert(0, Span::styled("⏺", theme::assistant_marker()));
                    } else if line.width() > 0 {
                        line.spans.insert(0, Span::raw("  "));
                    }
                }
            }
            out.extend(md);
        }
        Block::Thinking {
            text,
            tools,
            duration_secs,
        } => {
            // Expanded turn: the tool calls that ran (chronological), then the
            // thinking they informed. A summary line heads it when tools ran.
            if !tools.is_empty() {
                out.push(Line::from(Span::styled(
                    format!("✻ {}", turn_summary(*duration_secs, tools)),
                    theme::thinking(),
                )));
                for t in tools {
                    out.extend(render_one(t, width));
                }
            }
            // The faintest tier: dimmest fg on the faintest bg, ✻ glyph (CC's
            // thinking marker).
            let base = Style::default()
                .fg(theme::thinking_fg())
                .bg(theme::thinking_bg());
            for (i, line) in text.lines().enumerate() {
                let prefix = if i == 0 { "✻ " } else { "  " };
                out.push(Line::from(Span::styled(format!("{prefix}{line}"), base)));
            }
        }
        Block::ToolUse {
            name,
            target,
            diffs,
            patch,
            output,
            ..
        } => {
            let token = highlight::token_for_target(target);
            // A whole-new-file write (Write/NotebookEdit → a single ("", content)
            // pair) reads better as numbered code than as an all-`+` diff wall.
            if matches!(name.as_str(), "Write" | "NotebookEdit") {
                out.push(tool_header(name, target, None));
                let content = diffs
                    .iter()
                    .map(|(_, n)| n.as_str())
                    .find(|n| !n.is_empty())
                    .unwrap_or("");
                let n = content.lines().count();
                out.push(Line::styled(
                    format!("  ⎿ \u{a0}Wrote {n} lines to {target}"),
                    theme::result(),
                ));
                write_numbered(content, token, &mut out);
            } else if matches!(name.as_str(), "Edit" | "MultiEdit") {
                out.push(tool_header(name, target, None));
                let (adds, dels) = diffs
                    .iter()
                    .map(|(o, n)| diff_counts(o, n))
                    .fold((0usize, 0usize), |(a, d), (x, y)| (a + x, d + y));
                out.push(Line::styled(
                    format!("  ⎿ \u{a0}{}", edit_summary(adds, dels)),
                    theme::result(),
                ));
                // Prefer the transcript's structuredPatch (real file line numbers);
                // fall back to our own line-diff (local numbering) when absent.
                if let Some(hunks) = patch {
                    render_patch(hunks, token, &mut out);
                } else {
                    for (old, new) in diffs {
                        if old.is_empty() && new.is_empty() {
                            continue;
                        }
                        diff_lines(old, new, token, &mut out);
                    }
                }
            } else {
                // Bash / Read / other tools — header + (capped) output, on the
                // expanded shell/read background block (medium-dark gray, full
                // row width via `fill_bg`).
                let bg = theme::shell_expanded_bg();
                out.extend(tool_header_lines(name, target, Some(bg)));
                if let Some(o) = output {
                    push_capped_output(o, bg, theme::shell_fg(), &mut out);
                }
            }
        }
        Block::ToolResult(t) => {
            // Expanded foldable: the whole result reads as one block on the
            // tool-output background tier (`fill_bg` extends it full width).
            let base = theme::result().bg(theme::shell_expanded_bg());
            for (i, line) in t.lines().enumerate() {
                let prefix = if i == 0 { "⎿ " } else { "  " };
                // Span-level style (not `Line::styled`) so the bg survives wrapping
                // and `fill_bg` extends it across the full row.
                out.push(Line::from(Span::styled(format!("  {prefix}{line}"), base)));
            }
        }
        Block::Command { name, args, output } => {
            // `❯ /compact` header + dim `⎿ <stdout>` lines, like Claude Code — the
            // whole block shares the user-tier background so it reads as one region.
            out.extend(command_header_lines(name, args));
            for line in command_output_lines(output) {
                out.push(line);
            }
        }
    }
    out
}

/// The dim `⎿`-prefixed stdout lines beneath a command header (each stdout chunk
/// may be multi-line; only the first visual line gets the `⎿` elbow).
fn command_output_lines(output: &[String]) -> Vec<Line<'static>> {
    let base = theme::result().bg(theme::user_bg());
    let mut out = Vec::new();
    for chunk in output {
        for (i, line) in chunk.lines().enumerate() {
            let prefix = if i == 0 { "⎿ " } else { "  " };
            // Span-level style so the block bg survives wrapping and `fill_bg`.
            out.push(Line::from(Span::styled(format!("  {prefix}{line}"), base)));
        }
    }
    out
}

/// Max output lines shown for an expanded tool block before "… N lines remaining".
const OUTPUT_CAP: usize = 15;

/// `Added N lines[, removed M lines]` (singular/plural; "removed" omitted at 0) —
/// the Edit/MultiEdit result summary, matching Claude Code.
fn edit_summary(adds: usize, dels: usize) -> String {
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
fn display_name(name: &str) -> &str {
    match name {
        "Edit" | "MultiEdit" => "Update",
        other => other,
    }
}

/// The `⏺ Name(target)` header line, optionally with a background fill applied to
/// every span (so an expanded shell/read block reads as a solid block).
fn tool_header(name: &str, target: &str, bg: Option<Color>) -> Line<'static> {
    let patch = |s: Style| match bg {
        Some(c) => s.bg(c),
        None => s,
    };
    Line::from(vec![
        Span::styled("⏺", patch(theme::tool())),
        Span::styled(" ", patch(Style::default())),
        Span::styled(display_name(name).to_string(), patch(theme::tool())),
        Span::styled(format!("({target})"), patch(Style::default())),
    ])
}

/// Like `tool_header`, but preserves a multi-line `target` (a multi-line shell
/// command) across rows instead of flattening its newlines — matching Claude Code:
/// `⏺ Bash(<line 1>` then each further line indented, the closing `)` on the last.
/// A single-line target is unchanged (one `⏺ Name(target)` row).
fn tool_header_lines(name: &str, target: &str, bg: Option<Color>) -> Vec<Line<'static>> {
    let cmd: Vec<&str> = target.lines().collect();
    if cmd.len() <= 1 {
        return vec![tool_header(name, target, bg)];
    }
    let patch = |s: Style| match bg {
        Some(c) => s.bg(c),
        None => s,
    };
    let last = cmd.len() - 1;
    let mut out = Vec::with_capacity(cmd.len());
    for (i, line) in cmd.iter().enumerate() {
        if i == 0 {
            out.push(Line::from(vec![
                Span::styled("⏺", patch(theme::tool())),
                Span::styled(" ", patch(Style::default())),
                Span::styled(display_name(name).to_string(), patch(theme::tool())),
                Span::styled(format!("({line}"), patch(Style::default())),
            ]));
        } else {
            // Continuation rows are indented two columns; the last one closes `)`.
            let text = if i == last {
                format!("  {line})")
            } else {
                format!("  {line}")
            };
            out.push(Line::from(Span::styled(text, patch(Style::default()))));
        }
    }
    out
}

/// Push a tool's output, capped at `OUTPUT_CAP` lines (then "… N lines
/// remaining"), each line on the `bg`/`fg` tier.
fn push_capped_output(text: &str, bg: Color, fg: Color, out: &mut Vec<Line<'static>>) {
    let lines: Vec<&str> = text.lines().collect();
    let base = Style::default().fg(fg).bg(bg);
    // Span-level style (not `Line::styled`) so the bg survives `wrap::wrap_line`
    // and `view::fill_bg` extends it across the row — matching the header's block.
    for (i, l) in lines.iter().take(OUTPUT_CAP).enumerate() {
        let prefix = if i == 0 { "  ⎿ " } else { "    " };
        out.push(Line::from(Span::styled(format!("{prefix}{l}"), base)));
    }
    if lines.len() > OUTPUT_CAP {
        out.push(Line::from(Span::styled(
            format!("    … {} lines remaining", lines.len() - OUTPUT_CAP),
            base,
        )));
    }
}

/// A turn's summary label, in **natural (chronological) order**: the grouped tool
/// calls ran first (their results fed the thinking), so the activities lead and the
/// thinking closes — `Ran 1 shell command (ls), thought for 8s`. This matches the
/// expanded body (tools then thinking). A bare turn is just `Thought for 8s`. The
/// duration (`Xs` / `Xm Ys`) is omitted when unknown.
fn turn_summary(duration_secs: Option<u64>, tools: &[Block]) -> String {
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

/// Uppercase the first character of `s` (ASCII-friendly; leaves the rest as-is).
fn capitalize(s: &str) -> String {
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
fn activities(tools: &[Block]) -> String {
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
        parts.push(format!(
            "ran {n} shell command{} ({})",
            s(n),
            shell_names.join(", ")
        ));
    }
    if other > 0 {
        parts.push(format!("used {other} tool{}", s(other)));
    }
    parts.join(", ")
}

/// The collapsed representation of a foldable block. Bash/Read get a faint
/// one-line summary in the consistent fold-header color; everything else shows
/// its header plus a `⋯ N folded` placeholder.
fn render_collapsed(b: &Block) -> Vec<Line<'static>> {
    let header = Style::default().fg(theme::fold_header());
    match b {
        Block::Thinking {
            text,
            duration_secs,
            tools,
        } => {
            // `<activities>, thought for Xs` (natural order) — falls back to a line
            // count when the duration isn't derivable and no tools ran.
            let summary = if duration_secs.is_some() || !tools.is_empty() {
                turn_summary(*duration_secs, tools)
            } else {
                format!("Thought ({} lines)", text.lines().count())
            };
            // No `✻` glyph on the collapsed summary — a plain 2-space-indented line.
            vec![Line::from(Span::styled(format!("  {summary}"), header))]
        }
        Block::ToolUse { name, .. } if name == "Bash" => {
            vec![Line::from(Span::styled("  Ran 1 shell command", header))]
        }
        Block::ToolUse {
            name,
            target,
            read_lines,
            ..
        } if name == "Read" => {
            let suffix = read_lines
                .map(|n| format!(" ({n} lines)"))
                .unwrap_or_default();
            vec![Line::from(Span::styled(
                format!("  Read {target}{suffix}"),
                header,
            ))]
        }
        Block::Command { name, args, output } => {
            // Header + first `⎿` stdout line (like CC); deeper output is folded.
            let mut v = vec![command_header(name, args)];
            let lines = command_output_lines(output);
            let total = lines.len();
            if let Some(first) = lines.into_iter().next() {
                v.push(first);
            }
            if total > 1 {
                v.push(Line::styled(
                    format!("  ⋯ {} folded (space / click to expand)", total - 1),
                    theme::dim(),
                ));
            }
            v
        }
        _ => {
            let hidden = body_len(b);
            let mut v = vec![render_header(b)];
            if hidden > 0 {
                v.push(Line::styled(
                    format!("  ⋯ {hidden} folded (space / click to expand)"),
                    theme::dim(),
                ));
            }
            v
        }
    }
}

/// The one-line header for a block — the line shown both as the first line of an
/// expanded block and as the sole line (plus `⋯ N folded`) of a collapsed one.
/// Built without rendering the body, so collapsing a huge block is cheap.
/// Must match `render_one`'s first emitted line for foldable blocks.
fn render_header(b: &Block) -> Line<'static> {
    match b {
        Block::UserText(t) => {
            let base = Style::default().fg(theme::user_fg()).bg(theme::user_bg());
            Line::from(vec![
                Span::styled("❯ ", base.fg(theme::user_marker())),
                Span::styled(t.lines().next().unwrap_or("").to_string(), base),
            ])
        }
        Block::AssistantText(t) => Line::from(vec![
            Span::styled("⏺", theme::assistant_marker()),
            Span::raw(format!(" {}", t.lines().next().unwrap_or(""))),
        ]),
        Block::Thinking { text, .. } => Line::styled(
            format!("✻ {}", text.lines().next().unwrap_or("")),
            theme::thinking(),
        ),
        Block::ToolUse { name, target, .. } => Line::from(vec![
            Span::styled("⏺ ", theme::tool()),
            Span::styled(display_name(name).to_string(), theme::tool()),
            Span::raw(format!("({target})")),
        ]),
        Block::ToolResult(t) => Line::styled(
            format!("  ⎿ {}", t.lines().next().unwrap_or("")),
            theme::result(),
        ),
        Block::Command { name, args, .. } => command_header(name, args),
    }
}

/// How many lines `render_one` emits *after* the header — computed cheaply (no
/// `Line` allocation) so a collapsed block's `⋯ N folded` count is exact without
/// building the body. Must agree with `render_one`'s output length.
fn body_len(b: &Block) -> usize {
    match b {
        Block::ToolResult(t) | Block::UserText(t) => t.lines().count().saturating_sub(1),
        // A turn collapses to its one-line `✻ Thought for…` summary (handled in
        // `render_collapsed`), so this count isn't consumed; approximate anyway.
        Block::Thinking { text, .. } => text.lines().count().saturating_sub(1),
        Block::AssistantText(_) => 0, // not foldable; never collapsed
        Block::ToolUse {
            name,
            diffs,
            patch,
            output,
            ..
        } => match name.as_str() {
            "Write" | "NotebookEdit" => {
                let content = diffs
                    .iter()
                    .map(|(_, n)| n.as_str())
                    .find(|n| !n.is_empty())
                    .unwrap_or("");
                let n = content.lines().count();
                let shown = n.min(WRITE_PREVIEW);
                1 + shown + usize::from(n > shown) // ⎿ header + preview + "… +N lines"
            }
            "Edit" | "MultiEdit" => {
                let body: usize = if let Some(hunks) = patch {
                    hunks.iter().map(|h| h.lines.len()).sum()
                } else {
                    diffs
                        .iter()
                        .filter(|(o, n)| !(o.is_empty() && n.is_empty()))
                        .map(|(o, n)| diff_rendered_len(o, n))
                        .sum()
                };
                1 + body // └ header + diff
            }
            // Bash/Read use a custom collapsed summary (this count is unused for
            // them); other tools show their capped output beneath the header.
            _ => output.as_deref().map_or(0, |o| {
                let n = o.lines().count();
                n.min(OUTPUT_CAP) + usize::from(n > OUTPUT_CAP)
            }),
        },
        // Command uses its own collapsed summary; this count is unused for it.
        Block::Command { output, .. } => output.iter().map(|c| c.lines().count()).sum(),
    }
}

/// Render all blocks, honoring per-block collapse state, tagging each line with
/// its block index. A collapsed foldable block shows its first line + a
/// one-line placeholder.
/// Render a single block's body: its one-line summary when `is_collapsed`, else its
/// full expanded lines. This is the syntax-highlighting-heavy part, so the viewer
/// caches it per block (keyed by collapsed state) to keep fold toggles cheap.
pub fn block_body(b: &Block, is_collapsed: bool, width: usize) -> Vec<Line<'static>> {
    if is_collapsed {
        render_collapsed(b)
    } else {
        render_one(b, width)
    }
}

/// Assemble per-block bodies (`bodies[i]` = block `i`) into the final tagged line
/// list: a blank separator after each non-empty block, then collapse runs of ≥2
/// blank lines into one (markdown spacing + separators otherwise stack up).
pub fn assemble(bodies: Vec<Vec<Line<'static>>>) -> Rendered {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut block_of: Vec<usize> = Vec::new();
    for (i, body) in bodies.into_iter().enumerate() {
        for l in body {
            lines.push(l);
            block_of.push(i);
        }
        if lines.last().map(|l| l.width() != 0).unwrap_or(false) {
            lines.push(Line::from(""));
            block_of.push(i);
        }
    }
    let mut out_lines: Vec<Line<'static>> = Vec::with_capacity(lines.len());
    let mut out_tags: Vec<usize> = Vec::with_capacity(block_of.len());
    let mut prev_blank = false;
    for (l, t) in lines.into_iter().zip(block_of) {
        let blank = l.width() == 0;
        if blank && prev_blank {
            continue;
        }
        prev_blank = blank;
        out_lines.push(l);
        out_tags.push(t);
    }
    Rendered {
        lines: out_lines,
        block_of: out_tags,
    }
}

/// Convenience wrapper (block_body → assemble) used by tests; the viewer drives
/// `block_body`/`assemble` directly so it can cache bodies across fold toggles.
#[cfg(test)]
pub fn render_blocks_folded(blocks: &[Block], collapsed: &[bool], width: usize) -> Rendered {
    let bodies = blocks
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let is_collapsed = collapsed.get(i).copied().unwrap_or(false) && foldable(b);
            block_body(b, is_collapsed, width)
        })
        .collect();
    assemble(bodies)
}

/// Width `--dump` falls back to when there's no terminal to measure.
pub const DUMP_WIDTH: usize = 100;

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    fn texts(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    /// True if any span on the line carries this background color.
    fn has_bg(line: &Line, bg: Color) -> bool {
        line.spans.iter().any(|s| s.style.bg == Some(bg))
    }

    /// A multi-line shell command keeps its line breaks in the `⏺ Bash(...)`
    /// header instead of being reflowed into one line (the newline-flatten bug).
    #[test]
    fn multiline_bash_command_header_preserves_line_breaks() {
        let b = Block::ToolUse {
            name: "Bash".into(),
            target: "cd /x\ncargo test\ngit status".into(),
            diffs: vec![],
            output: Some("ok".into()),
            patch: None,
            read_lines: None,
        };
        let lines = render_one(&b, 200);
        let t = texts(&lines);
        let all = t.join("\n");

        // Header opens on the first command line; the last closes the paren.
        assert!(
            t.iter()
                .any(|l| l.contains("⏺") && l.contains("Bash(cd /x")),
            "header should open with the first command line:\n{all}"
        );
        assert!(
            t.iter().any(|l| l.trim_end().ends_with("git status)")),
            "last command line should close the paren:\n{all}"
        );
        // The middle line stands on its own — not merged with a neighbor.
        assert!(
            t.iter()
                .any(|l| l.contains("cargo test") && !l.contains("cd /x")),
            "cargo test should be its own row:\n{all}"
        );
        assert!(
            !t.iter()
                .any(|l| l.contains("cd /x") && l.contains("cargo test")),
            "command lines must not be flattened onto one row:\n{all}"
        );
    }

    /// A line inserted in the middle must keep the surrounding lines as context,
    /// not mispair them into bogus -/+ rows (the old index-zip bug).
    #[test]
    fn inserted_line_keeps_others_as_context() {
        let mut out = Vec::new();
        diff_lines("a\nb\nc", "a\nb\nX\nc", "", &mut out);
        let t = texts(&out);
        let all = t.join("\n");

        // `c` was never deleted — it appears as a context row (no `- ` marker).
        assert!(
            t.iter()
                .any(|l| l.contains("c") && !l.contains("- ") && !l.contains("+ ")),
            "c not context:\n{all}"
        );
        assert!(!all.contains("- c"), "c wrongly deleted:\n{all}");
        // The genuine insertion shows as a `+` row (marker directly before code).
        assert!(t.iter().any(|l| l.contains("+X")), "X not added:\n{all}");
    }

    /// A changed line shows a `-` (red bg) row then a `+` (green bg) row.
    #[test]
    fn changed_line_shows_del_then_add_with_bg() {
        let mut out = Vec::new();
        diff_lines("hello world", "hello brave world", "txt", &mut out);
        let del = out
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content == "-"))
            .expect("a del row");
        let add = out
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content == "+"))
            .expect("an add row");
        assert!(has_bg(del, theme::diff_del_bg()), "del row lacks red bg");
        assert!(has_bg(add, theme::diff_add_bg()), "add row lacks green bg");
        let add_text: String = add.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(add_text.contains("brave"), "change missing: {add_text:?}");
    }

    /// Edit diffs carry a gutter + green/red bg + syntect fg on the code.
    #[test]
    fn edit_diff_has_gutter_bg_and_syntax() {
        let block = Block::ToolUse {
            name: "Edit".into(),
            target: "src/x.rs".into(),
            diffs: vec![("let a = 1;".into(), "let a = 2;".into())],
            output: None,
            patch: None,
            read_lines: None,
        };
        let lines = render_one(&block, 80);
        let add = lines
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content == "+"))
            .expect("add row");
        let del = lines
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content == "-"))
            .expect("del row");
        // Background fills.
        assert!(has_bg(add, theme::diff_add_bg()), "no green bg on add");
        assert!(has_bg(del, theme::diff_del_bg()), "no red bg on del");
        // Gutter number on the add (new-side) row.
        let gutter = add.spans[0].content.to_string();
        assert!(
            gutter.chars().any(|c| c.is_ascii_digit()),
            "no gutter number: {gutter:?}"
        );
        // Syntax highlighting: some span has a concrete Rgb fg (e.g. `let`).
        assert!(
            add.spans
                .iter()
                .any(|s| matches!(s.style.fg, Some(Color::Indexed(..)))),
            "no syntect fg color on the add row"
        );
    }

    /// Write renders as a `⎿ Wrote N lines` header + a capped numbered preview
    /// (first WRITE_PREVIEW lines) then `… +N lines`, like Claude Code.
    #[test]
    fn write_shows_capped_numbered_preview() {
        let content = (1..=25)
            .map(|i| format!("row{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let block = Block::ToolUse {
            name: "Write".into(),
            target: "src/x.rs".into(),
            diffs: vec![(String::new(), content)],
            output: None,
            patch: None,
            read_lines: None,
        };
        let t = texts(&render_one(&block, 80));
        let all = t.join("\n");
        assert!(
            t.iter()
                .any(|l| l.contains("⎿ \u{a0}Wrote 25 lines to src/x.rs")),
            "no header:\n{all}"
        );
        // numbered, not `+`-prefixed
        assert!(t.iter().any(|l| l.contains(" 1 ") && l.contains("row1")));
        assert!(!all.contains("+ row1"), "should not be a +diff:\n{all}");
        // capped at WRITE_PREVIEW: last preview line shown, the rest summarised.
        assert!(
            t.iter().any(|l| l.contains("row10")),
            "preview tail missing:\n{all}"
        );
        assert!(!all.contains("row11"), "should cap at 10:\n{all}");
        assert!(
            t.iter().any(|l| l.contains("… +15 lines")),
            "no cap marker:\n{all}"
        );
    }

    /// A collapsed foldable block emits only its header + a `⋯ N folded`
    /// placeholder (lazy — the body is never built), with a true line count;
    /// expanding shows every line.
    #[test]
    fn collapsed_block_is_header_only_with_true_count() {
        let big = (0..50)
            .map(|i| format!("out {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let blocks = vec![Block::ToolResult(big)];

        // Expanded: all 50 lines render.
        let exp = render_blocks_folded(&blocks, &[false], 80);
        let exp_nonblank = exp.lines.iter().filter(|l| l.width() > 0).count();
        assert_eq!(exp_nonblank, 50, "expanded should show every line");

        // Collapsed: header + placeholder only (2 non-blank lines), count = 49.
        let col = render_blocks_folded(&blocks, &[true], 80);
        let t: Vec<String> = col
            .lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(
            t.len(),
            2,
            "collapsed should be header + placeholder: {t:?}"
        );
        assert!(t[0].contains("⎿ out 0"), "header wrong: {t:?}");
        assert!(t[1].contains("49 folded"), "true count wrong: {t:?}");
    }

    /// Edit shows an `⏺ Update(...)` header + `⎿ Added/removed` summary + -/+ rows.
    #[test]
    fn edit_shows_update_header_and_diff() {
        let block = Block::ToolUse {
            name: "Edit".into(),
            target: "src/y.rs".into(),
            diffs: vec![("let a = 1;".into(), "let a = 2;".into())],
            output: None,
            patch: None,
            read_lines: None,
        };
        let lines = render_one(&block, 80);
        let t = texts(&lines);
        let all = t.join("\n");
        assert!(
            t.iter().any(|l| l.contains("⏺ Update(src/y.rs)")),
            "no Update header:\n{all}"
        );
        assert!(
            t.iter()
                .any(|l| l.contains("⎿ \u{a0}Added 1 line, removed 1 line")),
            "no summary:\n{all}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.spans.iter().any(|s| s.content == "-")),
            "no del row:\n{all}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.spans.iter().any(|s| s.content == "+")),
            "no add row:\n{all}"
        );
    }

    /// With a structuredPatch, an Edit numbers rows from the real `new_start`
    /// (not 1..N), fills add/del rows with bg, and syntax-highlights the code.
    #[test]
    fn edit_patch_uses_absolute_line_numbers_bg_and_syntax() {
        use crate::model::Hunk;
        let block = Block::ToolUse {
            name: "Edit".into(),
            target: "src/x.rs".into(),
            diffs: vec![("let a = 1;".into(), "let a = 2;".into())],
            output: None,
            patch: Some(vec![Hunk {
                old_start: 49,
                new_start: 49,
                lines: vec![
                    " let x = 0;".into(),
                    "-let a = 1;".into(),
                    "+let a = 2;".into(),
                ],
            }]),
            read_lines: None,
        };
        let lines = render_one(&block, 80);
        let add = lines
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content == "+"))
            .expect("add row");
        // Real new-side line number 50 (49 = context, 50 = the added line).
        assert!(
            add.spans[0].content.contains("50"),
            "gutter: {:?}",
            add.spans[0].content
        );
        let ctx = lines
            .iter()
            .find(|l| {
                let txt: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
                txt.contains("let x")
            })
            .expect("context row");
        // spans[0] is now the INSET indent; the gutter is in the next span(s).
        let ctx_gutter: String = ctx
            .spans
            .iter()
            .take(2)
            .map(|s| s.content.as_ref())
            .collect();
        assert!(ctx_gutter.contains("49"), "context gutter: {ctx_gutter:?}");
        // Background fill + syntect fg on the added code.
        assert!(add
            .spans
            .iter()
            .any(|s| s.style.bg == Some(theme::diff_add_bg())));
        assert!(add
            .spans
            .iter()
            .any(|s| matches!(s.style.fg, Some(Color::Indexed(..)))));
    }

    fn bash(cmd: &str, output: Option<&str>) -> Block {
        Block::ToolUse {
            name: "Bash".into(),
            target: cmd.into(),
            diffs: vec![],
            output: output.map(String::from),
            patch: None,
            read_lines: None,
        }
    }

    /// Collapsed: a Bash block is a faint one-liner; a Read block names the file
    /// and line count.
    #[test]
    fn collapsed_shell_and_read_summaries() {
        let bash_lines = render_collapsed(&bash("ls -la", Some("a\nb")));
        let bt = texts(&bash_lines).join("\n");
        assert!(bt.contains("Ran 1 shell command"), "bash summary: {bt}");
        assert!(
            bash_lines[0]
                .spans
                .iter()
                .any(|s| s.style.fg == Some(theme::fold_header())),
            "summary not in fold-header color"
        );

        let read = Block::ToolUse {
            name: "Read".into(),
            target: "src/x.rs".into(),
            diffs: vec![],
            output: Some("...".into()),
            patch: None,
            read_lines: Some(42),
        };
        let rt = texts(&render_collapsed(&read)).join("\n");
        assert!(
            rt.contains("Read src/x.rs (42 lines)"),
            "read summary: {rt}"
        );
    }

    /// Expanded: a Bash block shows the command header + output capped at 15 lines
    /// with a remainder note, all on the shell background.
    #[test]
    fn expanded_shell_caps_output_and_has_bg() {
        let out: String = (1..=20)
            .map(|i| format!("out{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let lines = render_one(&bash("ls", Some(&out)), 80);
        let t = texts(&lines);
        let all = t.join("\n");
        assert!(t.iter().any(|l| l.contains("out1")), "first output line");
        assert!(!all.contains("out16"), "should cap at 15: {all}");
        assert!(
            t.iter().any(|l| l.contains("… 5 lines remaining")),
            "no remainder: {all}"
        );
        // The output rows carry the expanded-shell background.
        assert!(
            lines.iter().any(|l| l
                .spans
                .iter()
                .any(|s| s.style.bg == Some(theme::shell_expanded_bg()))),
            "no expanded-shell bg on expanded block"
        );
        // The command header is also on the background block.
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|s| s.style.bg == Some(theme::shell_expanded_bg())),
            "command header not on bg block"
        );
    }

    /// No two adjacent blank lines survive in the assembled output.
    #[test]
    fn consecutive_blank_lines_collapse() {
        // Assistant text with intentional double blank lines + several blocks
        // (each adds a separator) — none should stack.
        let blocks = vec![
            Block::AssistantText("para one\n\n\n\npara two".into()),
            Block::AssistantText("another".into()),
            Block::AssistantText("third".into()),
        ];
        let r = render_blocks_folded(&blocks, &[], 80);
        let blanks: Vec<bool> = r.lines.iter().map(|l| l.width() == 0).collect();
        assert!(
            !blanks.windows(2).any(|w| w[0] && w[1]),
            "found adjacent blank lines"
        );
    }

    /// A user message gets a `❯` caret on the first line (continuation lines
    /// align two spaces under it), and every line carries the user background.
    #[test]
    fn user_message_has_caret_and_block_bg() {
        let lines = render_one(&Block::UserText("hello\nworld".into()), 80);
        let t = texts(&lines);
        assert!(t[0].starts_with("❯ hello"), "first line caret: {:?}", t[0]);
        assert!(
            t[1].starts_with("  ") && !t[1].contains('❯'),
            "continuation aligns under caret: {:?}",
            t[1]
        );
        for line in &lines {
            assert!(
                line.spans
                    .iter()
                    .any(|s| s.style.bg == Some(theme::user_bg())),
                "user line missing bg: {line:?}"
            );
        }
    }

    /// User text gets the user-tier bg; expanded thinking is the faintest tier
    /// (bg fainter and fg dimmer than user) with the ∴ glyph.
    #[test]
    fn user_and_thinking_background_tiers() {
        let user = render_one(&Block::UserText("hello".into()), 80);
        assert!(
            user[0]
                .spans
                .iter()
                .any(|s| s.style.bg == Some(theme::user_bg())),
            "user has no user bg"
        );

        let think = render_one(
            &Block::Thinking {
                text: "a\nb".into(),
                duration_secs: None,
                tools: vec![],
            },
            80,
        );
        let t0 = &think[0];
        assert!(
            t0.spans.iter().any(|s| s.content.contains('✻')),
            "thinking missing ✻ glyph"
        );
        assert!(
            t0.spans
                .iter()
                .any(|s| s.style.bg == Some(theme::thinking_bg())),
            "thinking has no thinking bg"
        );
        assert!(
            t0.spans
                .iter()
                .any(|s| s.style.fg == Some(theme::thinking_fg())),
            "thinking has no thinking fg"
        );
    }

    /// Assistant body: the first line carries the ● marker flush-left; every
    /// following non-blank line is indented two spaces to align under it.
    #[test]
    fn assistant_body_lines_indented_two_spaces() {
        let lines = render_one(&Block::AssistantText("- a\n- b\n- c".into()), 80);
        let t = texts(&lines);
        assert!(
            t[0].starts_with("⏺ "),
            "first line lacks marker: {:?}",
            t[0]
        );
        assert!(
            t[1].starts_with("  ") && !t[1].starts_with("⏺ "),
            "line 2 not indented: {:?}",
            t[1]
        );
        assert!(t[2].starts_with("  "), "line 3 not indented: {:?}", t[2]);
    }

    /// A Command block renders `❯ /compact` + a dim `⎿`-prefixed stdout line.
    #[test]
    fn command_block_renders_caret_header_and_elbow_output() {
        let block = Block::Command {
            name: "/compact".into(),
            args: String::new(),
            output: vec!["Compacted (ctrl+o to see full summary)".into()],
        };
        let lines = render_one(&block, 80);
        let t = texts(&lines);
        assert_eq!(t[0], "❯ /compact", "header: {:?}", t[0]);
        assert!(
            t[1].contains("⎿ Compacted (ctrl+o to see full summary)"),
            "stdout line: {:?}",
            t[1]
        );
        // The stdout line is dim (result tier), not full-bright, on the user-tier
        // block bg. Style lives on the span (so it survives wrapping / `fill_bg`).
        let st = lines[1].spans[0].style;
        assert_eq!(st.fg, theme::result().fg, "stdout not in result/dim color");
        assert_eq!(st.bg, Some(theme::user_bg()), "stdout not on the block bg");
    }

    /// A slash command with multi-line args keeps its line breaks when expanded
    /// (one line per source line, continuation indented under the caret), but
    /// collapses to a single `…`-suffixed line in the header/collapsed form.
    #[test]
    fn multiline_command_args_preserve_line_breaks() {
        let block = Block::Command {
            name: "/loop".into(),
            args: "drive parity\nWORKING DIR:\n/tmp/peek".into(),
            output: vec![],
        };
        let t = texts(&render_one(&block, 80));
        assert_eq!(t[0], "❯ /loop drive parity", "first line: {:?}", t[0]);
        assert_eq!(t[1], "  WORKING DIR:", "continuation 1: {:?}", t[1]);
        assert_eq!(t[2], "  /tmp/peek", "continuation 2: {:?}", t[2]);
        // No run-together: the second source line never glues onto the first.
        assert!(
            !t[0].contains("WORKING DIR"),
            "lines ran together: {:?}",
            t[0]
        );

        // Collapsed/header form is one line, first arg line + ellipsis.
        let ch = texts(&[render_header(&block)]);
        assert_eq!(
            ch[0], "❯ /loop drive parity…",
            "collapsed header: {:?}",
            ch[0]
        );
        let coll = texts(&render_collapsed(&block));
        assert_eq!(coll[0], "❯ /loop drive parity…", "collapsed: {:?}", coll[0]);
    }

    /// A collapsed Command block shows the header + first `⎿` line.
    #[test]
    fn collapsed_command_keeps_header_and_first_line() {
        let block = Block::Command {
            name: "/compact".into(),
            args: String::new(),
            output: vec!["Compacted (ctrl+o to see full summary)".into()],
        };
        let t = texts(&render_collapsed(&block));
        assert_eq!(t[0], "❯ /compact");
        assert!(t[1].contains("⎿ Compacted"), "first line: {:?}", t[1]);
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

    /// A grouped turn collapses to `Thought for Xs, <activities>` (no `✻` glyph),
    /// counting its absorbed tools; expanded, it shows the tools then the thinking.
    #[test]
    fn turn_collapses_to_thought_for_summary() {
        let tools = vec![
            Block::ToolUse {
                name: "Bash".into(),
                target: "ls".into(),
                diffs: vec![],
                output: None,
                patch: None,
                read_lines: None,
            },
            Block::ToolUse {
                name: "Read".into(),
                target: "a.rs".into(),
                diffs: vec![],
                output: None,
                patch: None,
                read_lines: None,
            },
        ];
        let turn = Block::Thinking {
            text: "reasoning".into(),
            duration_secs: Some(72),
            tools,
        };
        let coll = texts(&render_collapsed(&turn));
        assert_eq!(
            coll[0], "  Read 1 file, ran 1 shell command (ls), thought for 1m 12s",
            "collapsed summary: {:?}",
            coll[0]
        );
        // Expanded shows the two tool headers and the thinking text.
        let exp = texts(&render_one(&turn, 80)).join("\n");
        assert!(
            exp.contains("Bash") && exp.contains("Read"),
            "tools missing:\n{exp}"
        );
        assert!(exp.contains("reasoning"), "thinking missing:\n{exp}");
    }

    /// A `.md` edit syntax-highlights as markdown (token from the extension).
    #[test]
    fn md_edit_is_syntax_highlighted() {
        use crate::model::Hunk;
        let block = Block::ToolUse {
            name: "Edit".into(),
            target: "notes.md".into(),
            diffs: vec![(String::new(), "# Title".into())],
            output: None,
            patch: Some(vec![Hunk {
                old_start: 1,
                new_start: 1,
                lines: vec!["+# Title".into()],
            }]),
            read_lines: None,
        };
        let lines = render_one(&block, 80);
        let add = lines
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content == "+"))
            .expect("add row");
        assert!(
            add.spans
                .iter()
                .any(|s| matches!(s.style.fg, Some(Color::Indexed(..)))),
            "no markdown syntect color"
        );
    }
}
