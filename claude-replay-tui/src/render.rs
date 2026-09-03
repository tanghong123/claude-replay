//! Blocks -> styled ratatui lines. Each emitted line is tagged with its source
//! block index so the viewer can fold/expand and hit-test mouse clicks.

use crate::diff::{diff_row_groups, line_diff, DiffKind, LineOp};
use crate::highlight::{self, Hl};
use crate::model::{AssistantPhase, Attachment, Block};
use crate::present::{
    display_name, edit_summary, spawn_chip, thinking_summary, tool_execution_failed,
    tool_execution_summary, turn_summary, write_content, WRITE_PREVIEW,
};
use crate::tui::{markdown, theme};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

/// Rendered lines plus a parallel "which block produced this line" vector. Test-only now: the
/// flat pass survives as the ORACLE the windowed `assemble_one` is proven against; production
/// renders per block and threads the blank-carry itself.
#[cfg(test)]
pub struct Rendered {
    pub lines: Vec<Line<'static>>,
    pub block_of: Vec<crate::model::BlockIndex>,
}

/// Blocks whose body can be collapsed to a one-line placeholder.
pub fn foldable(b: &Block) -> bool {
    crate::model::foldable(b)
}

/// The collapsed spawn header: `⏺ Agent(<type>: <description>)  <chip>  ↵ <agent-id>` in
/// the agent hue. The `↵ <agent-id>` is the DESCEND target — clicking it opens the child;
/// clicking anywhere else on the header just folds. `focused` brightens the arg.
fn agent_header(sa: &crate::model::SubAgent, focused: bool) -> Line<'static> {
    let mark = theme::agent();
    let arg = if focused {
        Style::default().fg(theme::fold_header_focused())
    } else {
        Style::default()
    };
    let mut spans = vec![
        Span::styled("⏺ ", mark),
        Span::styled("Agent", mark),
        Span::styled(format!("({}: {})", sa.agent_type, sa.description), arg),
        Span::styled(
            format!("  {}", spawn_chip(sa)),
            Style::default().fg(theme::fold_header()),
        ),
    ];
    // The agent id is the sole descend affordance (link-styled). Shown whenever the spawn
    // carries an id — including a still-running agent whose child transcript loads lazily
    // at descend time — so the block always visibly signals "↵ opens this agent".
    if !sa.agent_id.is_empty() {
        spans.push(Span::styled(
            "  ↵ ",
            Style::default().fg(theme::fold_header()),
        ));
        spans.push(Span::styled(
            sa.agent_id.clone(),
            theme::agent().add_modifier(Modifier::UNDERLINED),
        ));
    }
    Line::from(spans)
}

/// The column span of the descend-target agent id in the collapsed spawn header (for
/// mouse hit-testing), or `None` if there's no descendable child. Must match the prefix
/// `agent_header` renders before the id.
pub(crate) fn agent_id_span(sa: &crate::model::SubAgent) -> Option<(usize, usize)> {
    use unicode_width::UnicodeWidthStr;
    if sa.agent_id.is_empty() {
        return None;
    }
    let prefix = format!(
        "⏺ Agent({}: {})  {}  ↵ ",
        sa.agent_type,
        sa.description,
        spawn_chip(sa)
    );
    let start = UnicodeWidthStr::width(prefix.as_str());
    let end = start + UnicodeWidthStr::width(sa.agent_id.as_str());
    Some((start, end))
}

/// The `(<type>: <description>)` (or `(<description>)`) parenthetical for a completion
/// header — kept in one place so the header and its id-span hit-test agree.
fn agent_done_arg(agent_type: &str, description: &str) -> String {
    if agent_type.is_empty() {
        format!("({description})")
    } else {
        format!("({agent_type}: {description})")
    }
}

/// The completion event header: `⏺ Agent(<type>: <description>) <verb>  ↵ <agent-id>` in
/// the agent hue, where `<verb>` is completed/failed/killed/stopped. The `↵ <agent-id>`
/// is the DESCEND target (same as the spawn), so the reader can open the finished
/// agent's transcript from its completion message. This is the "different message later"
/// that pairs with the "launched" spawn.
fn agent_done_header(
    agent_type: &str,
    description: &str,
    status: crate::model::AgentStatus,
    agent_id: &str,
    focused: bool,
) -> Line<'static> {
    let mark = theme::agent();
    let arg = if focused {
        Style::default().fg(theme::fold_header_focused())
    } else {
        Style::default()
    };
    let mut spans = vec![
        Span::styled("⏺ ", mark),
        Span::styled("Agent", mark),
        Span::styled(agent_done_arg(agent_type, description), arg),
        Span::styled(
            format!("  {}", status.done_verb()),
            Style::default().fg(theme::fold_header()),
        ),
    ];
    if !agent_id.is_empty() {
        spans.push(Span::styled(
            "  ↵ ",
            Style::default().fg(theme::fold_header()),
        ));
        spans.push(Span::styled(
            agent_id.to_string(),
            theme::agent().add_modifier(Modifier::UNDERLINED),
        ));
    }
    Line::from(spans)
}

/// The column span of the descend-target agent id in a completion header (for mouse
/// hit-testing). Must match the prefix `agent_done_header` renders before the id.
pub(crate) fn agent_done_id_span(
    agent_type: &str,
    description: &str,
    status: crate::model::AgentStatus,
    agent_id: &str,
) -> Option<(usize, usize)> {
    use unicode_width::UnicodeWidthStr;
    if agent_id.is_empty() {
        return None;
    }
    let prefix = format!(
        "⏺ Agent{}  {}  ↵ ",
        agent_done_arg(agent_type, description),
        status.done_verb()
    );
    let start = UnicodeWidthStr::width(prefix.as_str());
    let end = start + UnicodeWidthStr::width(agent_id);
    Some((start, end))
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

/// Added/removed line counts from a line-level diff (for the `└ Updated` header).
pub(crate) fn diff_counts(old: &str, new: &str) -> (usize, usize) {
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

/// Render a whole-new-file write as syntax-highlighted, line-numbered code (no
/// `+` gutter): `{6 spaces}{num right-aligned} {code}`. `limit` caps the shown
/// lines (the collapsed preview shows `Some(WRITE_PREVIEW)` then `     … +N lines`,
/// like Claude Code; the expanded view passes `None` to dump the whole file).
fn write_numbered(
    content: &str,
    token: &str,
    limit: Option<usize>,
    hl: Hl,
    out: &mut Vec<Line<'static>>,
) {
    let lines: Vec<&str> = content.lines().collect();
    let shown = limit.map_or(lines.len(), |cap| lines.len().min(cap));
    // Gutter width from the largest *shown* number (min 2), as CC does.
    let gutter = shown.to_string().len().max(2);
    // Highlight only the lines that will actually be PRINTED. syntect's state flows forward
    // only, so lines `0..shown` parse to identical spans whether or not the parse continues past
    // them — while a COLLAPSED preview (`limit = WRITE_PREVIEW`) would otherwise parse a whole
    // 5,000-line write to show ten of it, and that parse is ~150 µs a line (#107).
    let head = if shown >= lines.len() {
        content
    } else {
        let end = content
            .match_indices('\n')
            .take(shown)
            .last()
            .map_or(content.len(), |(i, _)| i + 1);
        &content[..end]
    };
    let hl = highlight::highlight_spans_with(head, token, hl);
    for (i, l) in lines.iter().take(shown).enumerate() {
        // 6-space margin + right-aligned number + one space, then the code.
        let mut spans = vec![
            Span::raw(" ".repeat(crate::tui::view::INSET)),
            Span::styled(format!("{:>gutter$} ", i + 1), theme::dim()),
        ];
        match hl.get(i) {
            Some(line_spans) if !line_spans.is_empty() => spans.extend(
                line_spans
                    .iter()
                    .cloned()
                    .map(|sp| theme::hl_span(sp, |s| s)),
            ),
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
    hl: Hl,
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
        spans.push(Span::raw(" ".repeat(crate::tui::view::INSET)));
    }
    // CC layout: `{gutter} {marker}{code}` — one space after the gutter, the
    // marker (+/-/space) directly before the code (the code keeps its own indent).
    // CC colours the whole gutter+marker run with the marker colour (green/red on
    // +/- rows, dim on context).
    spans.push(Span::styled(format!("{gutter} "), patch(marker_style)));
    spans.push(Span::styled(marker.to_string(), patch(marker_style)));
    // Measuring: a row narrower than the terminal occupies one display line however the
    // highlighter split it, so skip syntect for it entirely — that is the whole optimisation
    // (edit diffs were 97% of the first layout's cost). Rows that CAN wrap are highlighted
    // exactly as the render does, because their span split is what decides the break.
    let prefix = spans
        .iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum::<usize>();
    if highlight::fits_unwrapped(text, prefix, hl) {
        spans.push(Span::raw(text.to_string()));
    } else {
        for sp in highlight::highlight_one_with(text, token, Hl::Styled) {
            spans.push(theme::hl_span(sp, patch));
        }
    }
    Line::from(spans)
}

/// How many rows the fallback diff will emit for this (old,new) pair — computed
/// cheaply (no `Line` allocation) so a collapsed block's `⋯ N folded` count is
/// exact without building the body. Mirrors `diff_row_groups`' fallback pairing rule.
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

/// Added/removed counts straight from a `structuredPatch`'s hunk lines (a Write
/// overwrite has no old/new string pair to line-diff — the patch IS the diff).
fn patch_counts(hunks: &[crate::model::Hunk]) -> (usize, usize) {
    let adds = hunks
        .iter()
        .flat_map(|h| &h.lines)
        .filter(|l| l.starts_with('+'))
        .count();
    let dels = hunks
        .iter()
        .flat_map(|h| &h.lines)
        .filter(|l| l.starts_with('-'))
        .count();
    (adds, dels)
}

/// Render an Edit's diff to styled TUI rows: classify via [`diff_row_groups`], size the
/// gutter per group (from its `max_line`), and emit one styled `diff_row` each. Add/del rows
/// fill with the diff bg; code is syntax-highlighted by `token`.
fn render_diff(
    diffs: &[(String, String)],
    patch: Option<&[crate::model::Hunk]>,
    token: &str,
    hl: Hl,
    out: &mut Vec<Line<'static>>,
) {
    for group in diff_row_groups(diffs, patch) {
        let gw = group.max_line.to_string().len().max(1);
        for r in &group.rows {
            let (marker, bg) = match r.kind {
                DiffKind::Ctx => (' ', None),
                DiffKind::Add => ('+', Some(theme::diff_add_bg())),
                DiffKind::Del => ('-', Some(theme::diff_del_bg())),
            };
            out.push(diff_row(gw, r.num, marker, &r.text, token, hl, bg));
        }
    }
}

/// A one-line marker for a surfaced attachment: `▤ <kind>  <name>`. The **name** is
/// the actionable target — clicking it (or Enter on the focused block) downloads the
/// embedded content or reveals the path in the file manager — so it's underlined like
/// a link.
fn attachment_line(a: &Attachment) -> Line<'static> {
    let mark = theme::tool();
    let name = mark.add_modifier(Modifier::UNDERLINED);
    Line::from(vec![
        Span::styled("▤ ", mark),
        Span::styled(format!("{} ", a.kind), mark),
        Span::styled(a.name.clone(), name),
    ])
}

/// Render a single block's content lines (no trailing blank separator). `width`
/// is the terminal width, used for width-aware table layout in assistant text.
fn assistant_lines(text: &str, width: usize, phase: Option<AssistantPhase>) -> Vec<Line<'static>> {
    let (marker, marker_style) = match phase {
        // Codex's in-progress commentary is deliberately quieter than a terminal answer.
        Some(AssistantPhase::Commentary) => ("•", theme::thinking()),
        Some(AssistantPhase::Final) | None => ("⏺", theme::assistant_marker()),
    };
    let mut md = markdown::render(text, width);
    if md.is_empty() {
        md.push(Line::from(Span::styled(marker, marker_style)));
    } else {
        for (i, line) in md.iter_mut().enumerate() {
            if i == 0 {
                line.spans.insert(0, Span::raw(" "));
                line.spans.insert(0, Span::styled(marker, marker_style));
            } else if line.width() > 0 {
                line.spans.insert(0, Span::raw("  "));
            }
        }
    }
    md
}

fn render_one(b: &Block, width: usize, hl: Hl) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    match b {
        Block::Attachment(a) => out.push(attachment_line(a)),
        // Expanded sub-agent spawn: the agent-hue header, then the prompt, one
        // selectable agent-id row (the descend target), and the result — all on the
        // agent background tier.
        Block::SubAgent(sa) => {
            let bg = theme::agent_expanded_bg();
            let res = theme::result().bg(bg);
            let mut head = agent_header(sa, false);
            for s in head.spans.iter_mut() {
                s.style = s.style.bg(bg);
            }
            out.push(head);
            if !sa.prompt.trim().is_empty() {
                for (i, l) in sa.prompt.lines().enumerate() {
                    let p = if i == 0 { "  ⎿  " } else { "     " };
                    out.push(Line::from(Span::styled(format!("{p}{l}"), res)));
                }
            }
            if let Some(r) = &sa.result {
                for (i, l) in r.lines().enumerate() {
                    let p = if i == 0 { "  ⎿  " } else { "     " };
                    out.push(Line::from(Span::styled(format!("{p}{l}"), res)));
                }
            }
        }
        // The completion event: the agent-hue header, then the returned result on the
        // agent background tier (the paired "launched" spawn is elsewhere, up the log).
        Block::AgentDone {
            agent_id,
            agent_type,
            description,
            status,
            result,
        } => {
            let bg = theme::agent_expanded_bg();
            let res = theme::result().bg(bg);
            let mut head = agent_done_header(agent_type, description, *status, agent_id, false);
            for s in head.spans.iter_mut() {
                s.style = s.style.bg(bg);
            }
            out.push(head);
            if let Some(r) = result {
                for (i, l) in r.lines().enumerate() {
                    let p = if i == 0 { "  ⎿  " } else { "     " };
                    out.push(Line::from(Span::styled(format!("{p}{l}"), res)));
                }
            }
        }
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
        Block::QueueEvent { text } => {
            // A dim `⧗ queued: …` marker for a mid-turn prompt still in flight (the
            // agent hadn't picked it up yet). Continuation lines align under the text.
            let style = Style::default().fg(theme::fold_header());
            for (i, line) in text.lines().enumerate() {
                let s = if i == 0 {
                    format!("⧗ queued: {line}")
                } else {
                    format!("           {line}")
                };
                out.push(Line::from(Span::styled(s, style)));
            }
        }
        Block::AssistantText(t) => out.extend(assistant_lines(t, width, None)),
        // An INFERRED phase does not change the drawing: Claude Code marks narration and final
        // answer identically, and deriving the phase from `stop_reason` is not a licence to
        // depart from that. Only a transcript that STATES the phase gets the phased treatment.
        Block::AssistantMessage {
            text,
            phase,
            inferred,
        } => out.extend(assistant_lines(text, width, (!*inferred).then_some(*phase))),
        Block::Thinking {
            text,
            tools,
            duration_secs,
        } => {
            // Expanded turn: the tool calls that ran (chronological), then the
            // thinking they informed. A summary line heads it when tools ran.
            // The whole turn shares the shell/read block background so a coalesced
            // command+thinking region reads as one shaded block (matching Claude
            // Code); thinking is set apart by a fainter font, not a darker bg.
            let turn_bg = theme::shell_expanded_bg();
            // A coalesced pure-activity run (no thinking text/duration) expands to
            // just its tool calls — no `✻` summary header.
            let pure_activity = text.trim().is_empty() && duration_secs.is_none();
            if !tools.is_empty() {
                if !pure_activity {
                    out.push(Line::from(Span::styled(
                        format!("✻ {}", turn_summary(*duration_secs, tools)),
                        theme::thinking().bg(turn_bg),
                    )));
                }
                for t in tools {
                    out.extend(render_one(t, width, hl));
                }
            }
            // Faintest font (dim thinking fg) on the shared turn bg, ✻ glyph (CC's
            // thinking marker).
            let base = Style::default().fg(theme::thinking_fg()).bg(turn_bg);
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
            execution,
            published,
            ..
        } => {
            // An artifact publish reads as the THING it published. The header already names it
            // (the adapter labels the call by the artifact); what expanding adds is the URL —
            // the only part a reader can act on — and the description. There is no output: the
            // result was instructions to the agent, dropped at fold time, and the `{}` raw
            // toggle still has it.
            if let Some(p) = published.as_deref() {
                out.push(with_execution(
                    tool_header(name, target, None),
                    execution.as_ref(),
                    None,
                ));
                out.push(Line::styled(
                    format!("  ⎿ \u{a0}{}", p.url),
                    theme::result(),
                ));
                if !p.description.is_empty() {
                    out.push(Line::styled(format!("    {}", p.description), theme::dim()));
                }
                return out;
            }
            let token = highlight::token_for_target(target);
            let write_like = matches!(name.as_str(), "Write" | "NotebookEdit");
            // A Write OVER AN EXISTING FILE carries the harness's structuredPatch
            // (computed against the pre-write disk content — the transcript never
            // holds the original); CC renders it as a diff like an Edit (#92). Only
            // a fresh-file write (empty patch) keeps the numbered-content preview.
            let overwrite = write_like && patch.as_deref().is_some_and(|h| !h.is_empty());
            if write_like && !overwrite {
                out.push(with_execution(
                    tool_header(name, target, None),
                    execution.as_ref(),
                    None,
                ));
                let content = write_content(diffs);
                let n = content.lines().count();
                out.push(Line::styled(
                    format!("  ⎿ \u{a0}Wrote {n} lines to {target}"),
                    theme::result(),
                ));
                // Expanded: the whole file (folding controls cost). The collapsed
                // preview caps at WRITE_PREVIEW — see `render_collapsed`.
                write_numbered(content, token, None, hl, &mut out);
            } else if overwrite || matches!(name.as_str(), "Edit" | "MultiEdit") {
                out.push(with_execution(
                    tool_header(name, target, None),
                    execution.as_ref(),
                    None,
                ));
                let (adds, dels) = if overwrite {
                    patch_counts(patch.as_deref().unwrap_or_default())
                } else {
                    diffs
                        .iter()
                        .map(|(o, n)| diff_counts(o, n))
                        .fold((0usize, 0usize), |(a, d), (x, y)| (a + x, d + y))
                };
                out.push(Line::styled(
                    format!("  ⎿ \u{a0}{}", edit_summary(adds, dels)),
                    theme::result(),
                ));
                // Prefer the transcript's structuredPatch (real file line numbers);
                // `diff_row_groups` falls back to our own line-diff (local numbering) when absent.
                render_diff(diffs, patch.as_deref(), token, hl, &mut out);
            } else {
                // Bash / Read / other tools — header + (capped) output, on the
                // expanded shell/read background block (medium-dark gray, full
                // row width via `fill_bg`).
                let bg = theme::shell_expanded_bg();
                let mut headers = tool_header_lines(name, target, Some(bg));
                if let Some(header) = headers.first_mut() {
                    *header = with_execution(
                        std::mem::replace(header, Line::raw("")),
                        execution.as_ref(),
                        Some(bg),
                    );
                }
                out.extend(headers);
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
        Block::Compaction { summary, .. } => {
            // The rule, then the continuation summary the agent wrote — the whole point of
            // expanding an epoch divider is reading what survived the cut. Same `⎿` body
            // idiom (and the same 4-column gutter) as every other block's body, so the prose
            // wraps where it always did; no background tier, because a seam is not a speaker.
            out.push(compaction_rule(b));
            for (i, line) in summary.lines().enumerate() {
                let prefix = if i == 0 { "⎿ " } else { "  " };
                out.push(Line::styled(format!("  {prefix}{line}"), theme::result()));
            }
        }
    }
    out
}

/// The compaction divider's rule: `── context compacted · auto · 725k → 7.0k ──`, dim, so it
/// reads as a seam in the conversation rather than as another message.
fn compaction_rule(b: &Block) -> Line<'static> {
    let Block::Compaction {
        trigger,
        pre_tokens,
        post_tokens,
        ..
    } = b
    else {
        return Line::raw("");
    };
    Line::styled(
        format!(
            "── {} ──",
            crate::present::compaction_summary(*trigger, *pre_tokens, *post_tokens)
        ),
        theme::dim(),
    )
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

/// The display-column span `[start, end)` of the `(target)` region in a tool
/// header (`⏺ Name(target)`) — so the viewer can hit-test clicks on the path.
/// Mirrors `tool_header`'s layout: `⏺`(1) + ` `(1) + display_name + `(target)`.
pub(crate) fn tool_header_target_span(name: &str, target: &str) -> (usize, usize) {
    use unicode_width::UnicodeWidthStr;
    let start = 2 + UnicodeWidthStr::width(display_name(name));
    let end = start + UnicodeWidthStr::width(format!("({target})").as_str());
    (start, end)
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

fn with_execution(
    mut line: Line<'static>,
    execution: Option<&crate::model::ToolExecution>,
    bg: Option<Color>,
) -> Line<'static> {
    let Some(execution) = execution else {
        return line;
    };
    let summary = tool_execution_summary(execution);
    if summary.is_empty() {
        return line;
    }
    let patch = |style: Style| match bg {
        Some(bg) => style.bg(bg),
        None => style,
    };
    line.spans.push(Span::styled("  ", patch(Style::default())));
    let style = if tool_execution_failed(execution) {
        theme::diff_del()
    } else {
        theme::dim()
    };
    line.spans.push(Span::styled(summary, patch(style)));
    line
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
            // Shared with the HTML exporter (see `thinking_summary`); no `✻` glyph on the
            // collapsed TUI line — a plain 2-space-indented line.
            let summary = thinking_summary(text, *duration_secs, tools);
            vec![Line::from(Span::styled(format!("  {summary}"), header))]
        }
        Block::ToolUse {
            name, execution, ..
        } if name == "Bash" => {
            vec![with_execution(
                Line::from(Span::styled("  Ran 1 shell command", header)),
                execution.as_ref(),
                None,
            )]
        }
        // A file write collapses to a Claude-Code-style preview — the header, the
        // `Wrote N lines` result, then the first `WRITE_PREVIEW` lines + `… +N lines`
        // (not a generic `⋯ N folded`). Expanding shows the whole file.
        Block::ToolUse {
            name,
            target,
            diffs,
            patch,
            execution,
            ..
        } if (name == "Write" || name == "NotebookEdit")
            && patch.as_deref().is_none_or(|h| h.is_empty()) =>
        {
            let content = write_content(diffs);
            let n = content.lines().count();
            let token = highlight::token_for_target(target);
            let mut v = vec![
                with_execution(tool_header(name, target, None), execution.as_ref(), None),
                Line::styled(
                    format!("  ⎿ \u{a0}Wrote {n} lines to {target}"),
                    theme::result(),
                ),
            ];
            write_numbered(content, token, Some(WRITE_PREVIEW), Hl::Styled, &mut v);
            v
        }
        Block::ToolUse {
            name,
            target,
            read_lines,
            execution,
            ..
        } if name == "Read" => {
            let suffix = read_lines
                .map(|n| format!(" ({n} lines)"))
                .unwrap_or_default();
            vec![with_execution(
                Line::from(Span::styled(format!("  Read {target}{suffix}"), header)),
                execution.as_ref(),
                None,
            )]
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
        Block::QueueEvent { text } => Line::styled(
            format!("⧗ queued: {}", text.lines().next().unwrap_or("")),
            Style::default().fg(theme::fold_header()),
        ),
        Block::SubAgent(sa) => agent_header(sa, false),
        Block::AgentDone {
            agent_id,
            agent_type,
            description,
            status,
            ..
        } => agent_done_header(agent_type, description, *status, agent_id, false),
        Block::Attachment(a) => attachment_line(a),
        Block::AssistantText(t) => Line::from(vec![
            Span::styled("⏺", theme::assistant_marker()),
            Span::raw(format!(" {}", t.lines().next().unwrap_or(""))),
        ]),
        Block::AssistantMessage {
            text,
            phase,
            inferred,
        } => {
            // As above: an inferred phase collapses to the plain assistant marker.
            let (marker, style) = match (inferred, phase) {
                (false, AssistantPhase::Commentary) => ("•", theme::thinking()),
                _ => ("⏺", theme::assistant_marker()),
            };
            Line::from(vec![
                Span::styled(marker, style),
                Span::raw(format!(" {}", text.lines().next().unwrap_or(""))),
            ])
        }
        Block::Thinking { text, .. } => Line::styled(
            format!("✻ {}", text.lines().next().unwrap_or("")),
            theme::thinking(),
        ),
        Block::ToolUse {
            name,
            target,
            execution,
            ..
        } => with_execution(
            Line::from(vec![
                Span::styled("⏺ ", theme::tool()),
                Span::styled(display_name(name).to_string(), theme::tool()),
                Span::raw(format!("({target})")),
            ]),
            execution.as_ref(),
            None,
        ),
        Block::ToolResult(t) => Line::styled(
            format!("  ⎿ {}", t.lines().next().unwrap_or("")),
            theme::result(),
        ),
        Block::Command { name, args, .. } => command_header(name, args),
        Block::Compaction { .. } => compaction_rule(b),
    }
}

/// How many lines `render_one` emits *after* the header — computed cheaply (no
/// `Line` allocation) so a collapsed block's `⋯ N folded` count is exact without
/// building the body. Must agree with `render_one`'s output length.
fn body_len(b: &Block) -> usize {
    match b {
        Block::ToolResult(t) | Block::UserText(t) | Block::QueueEvent { text: t } => {
            t.lines().count().saturating_sub(1)
        }
        Block::Attachment(_) => 0, // one-line marker; not foldable
        Block::SubAgent(sa) => {
            let p = if sa.prompt.trim().is_empty() {
                0
            } else {
                sa.prompt.lines().count()
            };
            let r = sa.result.as_deref().map_or(0, |r| r.lines().count());
            p + 1 + r // prompt lines + the agent row + result lines
        }
        // Header + the result body (the completion event has no prompt/agent row).
        Block::AgentDone { result, .. } => result.as_deref().map_or(0, |r| r.lines().count()),
        // A turn collapses to its one-line `✻ Thought for…` summary (handled in
        // `render_collapsed`), so this count isn't consumed; approximate anyway.
        Block::Thinking { text, .. } => text.lines().count().saturating_sub(1),
        Block::AssistantText(_) | Block::AssistantMessage { .. } => 0, // never collapsed
        Block::ToolUse {
            name,
            diffs,
            patch,
            output,
            ..
        } => match name.as_str() {
            "Write" | "NotebookEdit" if patch.as_deref().is_none_or(|h| h.is_empty()) => {
                // Expanded height: ⎿ result line + every content line (Write has its
                // own `render_collapsed` arm, so this feeds only length checks).
                let content = write_content(diffs);
                1 + content.lines().count()
            }
            // A Write overwrite renders as a diff (#92) — counted like an Edit.
            "Write" | "NotebookEdit" | "Edit" | "MultiEdit" => {
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
        // The summary prose beneath the rule; a summary-less boundary folds to nothing, so
        // `foldable`'s divider shows no `⋯ N folded` affordance it can't honour.
        Block::Compaction { summary, .. } => {
            if summary.is_empty() {
                0
            } else {
                summary.lines().count()
            }
        }
    }
}

/// Render all blocks, honoring per-block collapse state, tagging each line with
/// its block index. A collapsed foldable block shows its first line + a
/// one-line placeholder.
/// Render a single block's body: its one-line summary when `is_collapsed`, else its
/// full expanded lines. This is the syntax-highlighting-heavy part, so the viewer
/// caches it per block (keyed by collapsed state) to keep fold toggles cheap.
pub fn block_body(b: &Block, is_collapsed: bool, width: usize, hl: Hl) -> Vec<Line<'static>> {
    if is_collapsed {
        render_collapsed(b)
    } else {
        render_one(b, width, hl)
    }
}

/// Assemble per-block bodies (`bodies[i]` = block `i`) into the final tagged line
/// list: a blank separator after each non-empty block, then collapse runs of ≥2
/// blank lines into one (markdown spacing + separators otherwise stack up).
/// Test-only now — the oracle `assemble_one` (the windowed unit) is proven against.
#[cfg(test)]
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

/// ONE block's share of `assemble`'s output, computed in isolation — the windowed viewer's
/// unit (render only what's visible, measure the rest). `carry_in` is the one cross-block fact
/// the flat pass threads: whether the line before this block's first is blank — which, by
/// `assemble`'s own invariant, is exactly "any earlier block emitted at least one line" (every
/// non-empty contribution ends with a single blank: either the separator, or the body's own
/// trailing blank after the ≥2-run collapse). Proven equal to the flat pass block-for-block
/// (see `assemble_one_equals_flat_assemble`).
pub fn assemble_one(body: Vec<Line<'static>>, carry_in: bool) -> Vec<Line<'static>> {
    let mut lines = body;
    if lines.last().map(|l| l.width() != 0).unwrap_or(false) {
        lines.push(Line::from(""));
    }
    let mut out: Vec<Line<'static>> = Vec::with_capacity(lines.len());
    let mut prev_blank = carry_in;
    for l in lines {
        let blank = l.width() == 0;
        if blank && prev_blank {
            continue;
        }
        prev_blank = blank;
        out.push(l);
    }
    out
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
            block_body(b, is_collapsed, width, Hl::Styled)
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

    /// The windowing foundation: concatenating [`assemble_one`] over the blocks (threading the
    /// emitted-anything carry) reproduces the flat [`assemble`] EXACTLY — lines and per-block
    /// tags — across the tricky shapes: leading blanks, trailing blanks, all-blank bodies, empty
    /// bodies, and blank runs that straddle a block boundary.
    #[test]
    fn assemble_one_equals_flat_assemble() {
        let l = |s: &str| Line::from(s.to_string());
        let cases: Vec<Vec<Vec<Line>>> = vec![
            // Ordinary bodies.
            vec![vec![l("a")], vec![l("b"), l("c")]],
            // Leading blank at document start (kept) vs mid-document (dropped).
            vec![vec![l(""), l("a")], vec![l(""), l("b")]],
            // Trailing blanks collapse with the separator; runs collapse to one.
            vec![vec![l("a"), l(""), l("")], vec![l("b")]],
            // Empty and all-blank bodies emit nothing mid-document.
            vec![
                vec![],
                vec![l("")],
                vec![l("a")],
                vec![],
                vec![l("")],
                vec![l("b")],
            ],
            // All-blank document.
            vec![vec![l("")], vec![l(""), l("")]],
            // Blank run straddling a boundary.
            vec![vec![l("a"), l("")], vec![l(""), l(""), l("b")]],
        ];
        for bodies in cases {
            let flat = assemble(bodies.clone());
            let mut lines: Vec<Line> = Vec::new();
            let mut tags: Vec<usize> = Vec::new();
            let mut carry = false;
            for (i, body) in bodies.into_iter().enumerate() {
                let part = assemble_one(body, carry);
                carry |= !part.is_empty();
                tags.extend(std::iter::repeat_n(i, part.len()));
                lines.extend(part);
            }
            assert_eq!(
                texts(&lines),
                texts(&flat.lines),
                "lines match the flat pass"
            );
            assert_eq!(tags, flat.block_of, "tags match the flat pass");
        }
    }

    /// True if any span on the line carries this background color.
    fn has_bg(line: &Line, bg: Color) -> bool {
        line.spans.iter().any(|s| s.style.bg == Some(bg))
    }

    /// The spawn reads "launched" (never the terminal status) even after the agent is
    /// done; the separate `AgentDone` event carries the terminal verb + result.
    #[test]
    fn spawn_reads_launched_completion_reads_done() {
        use crate::model::{AgentStatus, SubAgent};
        let spawn = Block::SubAgent(SubAgent {
            agent_id: "a7436efe".into(),
            tool_use_id: "toolu_A".into(),
            agent_type: "general-purpose".into(),
            description: "Design the engine".into(),
            prompt: "go".into(),
            status: AgentStatus::Completed, // back-patched, but must NOT show as "done"
            result: None,
            output_file: None,
            blocks: vec![],
            subtree_cost: None,
        });
        let head = texts(&[render_header(&spawn)]).remove(0);
        assert!(head.contains("launched"), "spawn shows launched: {head:?}");
        assert!(!head.contains("done"), "spawn never shows done: {head:?}");
        assert!(
            head.contains("↵ a7436efe"),
            "spawn keeps descend id: {head:?}"
        );

        let done = Block::AgentDone {
            agent_id: "a7436efe".into(),
            agent_type: "general-purpose".into(),
            description: "Design the engine".into(),
            status: AgentStatus::Completed,
            result: Some("Proposal written.".into()),
        };
        let dlines = texts(&render_one(&done, 200, Hl::Styled));
        assert!(
            dlines[0].contains("Agent(general-purpose: Design the engine)")
                && dlines[0].contains("completed"),
            "completion header: {dlines:?}"
        );
        // The completion also carries the descend id, so the reader can open the
        // finished agent's transcript from its completion message.
        assert!(
            dlines[0].contains("↵ a7436efe"),
            "completion shows descend id: {dlines:?}"
        );
        assert!(
            dlines.iter().any(|l| l.contains("Proposal written.")),
            "completion carries the result: {dlines:?}"
        );
        // A failed/killed/stopped completion is treated identically — id + verb.
        for (st, verb) in [
            (AgentStatus::Failed, "failed"),
            (AgentStatus::Killed, "killed"),
            (AgentStatus::Stopped, "stopped"),
        ] {
            let d = Block::AgentDone {
                agent_id: "a7436efe".into(),
                agent_type: "gp".into(),
                description: "x".into(),
                status: st,
                result: None,
            };
            let h = texts(&[render_header(&d)]).remove(0);
            assert!(
                h.contains(verb) && h.contains("↵ a7436efe"),
                "{verb} completion shows verb + id: {h:?}"
            );
        }
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
            cwd: String::new(),
            execution: None,
            published: None,
        };
        let lines = render_one(&b, 200, Hl::Styled);
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
        render_diff(
            &[("a\nb\nc".into(), "a\nb\nX\nc".into())],
            None,
            "",
            Hl::Styled,
            &mut out,
        );
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
        render_diff(
            &[("hello world".into(), "hello brave world".into())],
            None,
            "txt",
            Hl::Styled,
            &mut out,
        );
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
            cwd: String::new(),
            execution: None,
            published: None,
        };
        let lines = render_one(&block, 80, Hl::Styled);
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

    /// #92: a Write OVER AN EXISTING FILE carries the harness's structuredPatch and
    /// renders as a diff like an Edit — `⎿ Added N lines, removed M lines` + hunks —
    /// while a fresh-file write (no patch) keeps the numbered-content preview.
    #[test]
    fn write_overwrite_renders_as_diff() {
        let block = Block::ToolUse {
            name: "Write".into(),
            target: "src/x.rs".into(),
            diffs: vec![(String::new(), "b\nc\n".into())],
            output: None,
            patch: Some(vec![crate::model::Hunk {
                old_start: 1,
                new_start: 1,
                lines: vec!["-a".into(), "+b".into(), " c".into()],
            }]),
            read_lines: None,
            cwd: String::new(),
            execution: None,
            published: None,
        };
        let e = texts(&render_one(&block, 100, Hl::Styled));
        let all = e.join("\n");
        assert!(
            all.contains("Added 1 line, removed 1 line"),
            "edit-style summary expected:\n{all}"
        );
        assert!(
            all.contains("- a") || all.contains("-a"),
            "removed row expected:\n{all}"
        );
        assert!(
            !all.contains("Wrote"),
            "no write preview on overwrite:\n{all}"
        );
    }

    /// Like Claude Code: a Write **collapses** to a `⎿ Wrote N lines` header + a
    /// capped numbered preview (first WRITE_PREVIEW lines) then `… +N lines`, and
    /// **expands** to the whole file — the inverse of the old (always-capped) render.
    #[test]
    fn write_collapses_to_preview_and_expands_to_whole_file() {
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
            cwd: String::new(),
            execution: None,
            published: None,
        };

        // Collapsed → 10-line preview + "… +15 lines".
        let c = texts(&render_collapsed(&block));
        let call = c.join("\n");
        assert!(
            c.iter()
                .any(|l| l.contains("⎿ \u{a0}Wrote 25 lines to src/x.rs")),
            "no header:\n{call}"
        );
        assert!(c.iter().any(|l| l.contains(" 1 ") && l.contains("row1")));
        assert!(!call.contains("+ row1"), "should not be a +diff:\n{call}");
        assert!(
            c.iter().any(|l| l.contains("row10")),
            "tail missing:\n{call}"
        );
        assert!(!call.contains("row11"), "collapsed caps at 10:\n{call}");
        assert!(
            c.iter().any(|l| l.contains("… +15 lines")),
            "no cap marker:\n{call}"
        );

        // Expanded → the whole file, no cap marker.
        let e = texts(&render_one(&block, 80, Hl::Styled));
        let eall = e.join("\n");
        assert!(
            e.iter().any(|l| l.contains("row11")),
            "expanded truncated:\n{eall}"
        );
        assert!(
            e.iter().any(|l| l.contains("row25")),
            "expanded truncated:\n{eall}"
        );
        assert!(
            !eall.contains("… +"),
            "expanded should not summarise:\n{eall}"
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
            cwd: String::new(),
            execution: None,
            published: None,
        };
        let lines = render_one(&block, 80, Hl::Styled);
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
            cwd: String::new(),
            execution: None,
            published: None,
        };
        let lines = render_one(&block, 80, Hl::Styled);
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
            cwd: String::new(),
            execution: None,
            published: None,
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
            cwd: String::new(),
            execution: None,
            published: None,
        };
        let rt = texts(&render_collapsed(&read)).join("\n");
        assert!(
            rt.contains("Read src/x.rs (42 lines)"),
            "read summary: {rt}"
        );
    }

    #[test]
    fn command_execution_status_and_duration_render_in_both_fold_states() {
        use crate::model::{ToolDuration, ToolExecution, ToolStatus};
        let mut block = bash("cargo test", Some("boom"));
        let Block::ToolUse { execution, .. } = &mut block else {
            unreachable!()
        };
        *execution = Some(ToolExecution {
            status: Some(ToolStatus::Failed),
            exit_code: Some(7),
            duration: Some(ToolDuration {
                secs: 0,
                nanos: 42_000_000,
            }),
        });

        let collapsed = texts(&render_collapsed(&block)).join("\n");
        assert!(collapsed.contains("Ran 1 shell command"));
        assert!(collapsed.contains("exit 7 · 42ms"), "{collapsed}");
        let expanded = texts(&render_one(&block, 100, Hl::Styled)).join("\n");
        assert!(expanded.contains("Bash(cargo test)"));
        assert!(expanded.contains("exit 7 · 42ms"), "{expanded}");
    }

    /// Expanded: a Bash block shows the command header + output capped at 15 lines
    /// with a remainder note, all on the shell background.
    #[test]
    fn expanded_shell_caps_output_and_has_bg() {
        let out: String = (1..=20)
            .map(|i| format!("out{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let lines = render_one(&bash("ls", Some(&out)), 80, Hl::Styled);
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
        let lines = render_one(&Block::UserText("hello\nworld".into()), 80, Hl::Styled);
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
        let user = render_one(&Block::UserText("hello".into()), 80, Hl::Styled);
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
            Hl::Styled,
        );
        let t0 = &think[0];
        assert!(
            t0.spans.iter().any(|s| s.content.contains('✻')),
            "thinking missing ✻ glyph"
        );
        assert!(
            t0.spans
                .iter()
                .any(|s| s.style.bg == Some(theme::shell_expanded_bg())),
            "thinking should share the shell/turn background"
        );
        assert!(
            t0.spans
                .iter()
                .any(|s| s.style.fg == Some(theme::thinking_fg())),
            "thinking has no thinking fg"
        );
    }

    /// A queued (in-flight) prompt marker: dim `⧗ queued:` prefix on the first line,
    /// continuation lines aligned under the text, and it is NOT foldable (an
    /// always-shown annotation, never collapsed to a summary).
    #[test]
    fn queue_marker_renders_dim_prefix_and_is_not_foldable() {
        let b = Block::QueueEvent {
            text: "fix the table\nsecond line".into(),
        };
        assert!(!foldable(&b), "queue marker must not be foldable");
        let lines = render_one(&b, 80, Hl::Styled);
        let t = texts(&lines);
        assert!(t[0].starts_with("⧗ queued: fix the table"), "{t:?}");
        assert!(t[1].contains("second line"), "{t:?}");
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|s| s.style.fg == Some(theme::fold_header())),
            "queue marker should use the dim fold-header color"
        );
    }

    /// Assistant body: the first line carries the ● marker flush-left; every
    /// following non-blank line is indented two spaces to align under it.
    #[test]
    fn assistant_body_lines_indented_two_spaces() {
        let lines = render_one(
            &Block::AssistantText("- a\n- b\n- c".into()),
            80,
            Hl::Styled,
        );
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

    #[test]
    fn assistant_phase_uses_quiet_commentary_and_strong_final_markers() {
        let commentary = Block::AssistantMessage {
            text: "working".into(),
            phase: AssistantPhase::Commentary,
            inferred: false,
        };
        let final_answer = Block::AssistantMessage {
            text: "done".into(),
            phase: AssistantPhase::Final,
            inferred: false,
        };
        let commentary_text = texts(&render_one(&commentary, 80, Hl::Styled)).join("\n");
        let final_text = texts(&render_one(&final_answer, 80, Hl::Styled)).join("\n");
        assert!(
            commentary_text.starts_with("• working"),
            "{commentary_text}"
        );
        assert!(final_text.starts_with("⏺ done"), "{final_text}");
    }

    /// …and an INFERRED phase draws exactly like unphased assistant prose. Claude's phase comes
    /// from `stop_reason`, which says what the turn did, not how the prose should look — Claude
    /// Code marks narration and final answer with the same `⏺`, and so do we.
    #[test]
    fn an_inferred_phase_draws_like_plain_assistant_text() {
        for phase in [AssistantPhase::Commentary, AssistantPhase::Final] {
            let inferred = Block::AssistantMessage {
                text: "working".into(),
                phase,
                inferred: true,
            };
            let plain = Block::AssistantText("working".into());
            assert_eq!(
                texts(&render_one(&inferred, 80, Hl::Styled)),
                texts(&render_one(&plain, 80, Hl::Styled)),
                "inferred {phase:?} must be indistinguishable from AssistantText"
            );
        }
    }

    /// A Command block renders `❯ /compact` + a dim `⎿`-prefixed stdout line.
    #[test]
    fn command_block_renders_caret_header_and_elbow_output() {
        let block = Block::Command {
            name: "/compact".into(),
            args: String::new(),
            output: vec!["Compacted (ctrl+o to see full summary)".into()],
        };
        let lines = render_one(&block, 80, Hl::Styled);
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

    /// #108: a compaction renders as a dim RULE, not as another message — and its collapsed
    /// header is byte-for-byte the first line it renders expanded (the invariant every
    /// foldable block owes `render_header`, since the two are drawn by different paths).
    /// Expanding adds the continuation summary.
    #[test]
    fn compaction_renders_as_a_dim_rule_expanding_to_the_summary() {
        use crate::model::CompactTrigger;
        let block = Block::Compaction {
            trigger: CompactTrigger::Auto,
            pre_tokens: 996_000,
            post_tokens: 18_000,
            summary: "This session is being continued…\nsecond line".into(),
        };
        let t = texts(&render_one(&block, 80, Hl::Styled));
        assert_eq!(
            t[0], "── context compacted · auto · 996.0k → 18.0k ──",
            "rule: {:?}",
            t[0]
        );
        assert_eq!(
            texts(&[render_header(&block)])[0],
            t[0],
            "the collapsed header must equal the expanded first line"
        );
        assert_eq!(t[1], "  ⎿ This session is being continued…");
        assert_eq!(t[2], "    second line");
        // `body_len` drives the `⋯ N folded` count, so it must match what was emitted.
        assert_eq!(body_len(&block), t.len() - 1);
        // Dim, so the seam reads as chrome between turns rather than as a speaker. (Line-level
        // style like the other bg-less markers — there is no block background to survive.)
        assert_eq!(
            render_one(&block, 80, Hl::Styled)[0].style.fg,
            theme::dim().fg
        );
    }

    /// A boundary whose summary never arrived (the transcript ended mid-compaction, or an
    /// older shape) still shows its rule — and folds to NOTHING, so no `⋯ N folded`
    /// affordance promises content that isn't there.
    #[test]
    fn a_summary_less_compaction_folds_to_nothing() {
        use crate::model::CompactTrigger;
        let block = Block::Compaction {
            trigger: CompactTrigger::Manual,
            pre_tokens: 0,
            post_tokens: 0,
            summary: String::new(),
        };
        let t = texts(&render_one(&block, 80, Hl::Styled));
        assert_eq!(
            t,
            vec!["── context compacted · manual ──"],
            "no token figures the record never made"
        );
        assert_eq!(body_len(&block), 0);
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
        let t = texts(&render_one(&block, 80, Hl::Styled));
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

    /// A coalesced span collapses to `Thought for Xs, <activities>` (no `✻` glyph) in
    /// CC's clause order (#57: thought first, `ls` classifies as a directory listing);
    /// expanded, it shows the tools then the thinking.
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
                cwd: String::new(),
                execution: None,
                published: None,
            },
            Block::ToolUse {
                name: "Read".into(),
                target: "a.rs".into(),
                diffs: vec![],
                output: None,
                patch: None,
                read_lines: None,
                cwd: String::new(),
                execution: None,
                published: None,
            },
        ];
        let turn = Block::Thinking {
            text: "reasoning".into(),
            duration_secs: Some(72),
            tools,
        };
        let coll = texts(&render_collapsed(&turn));
        assert_eq!(
            coll[0], "  Thought for 1m 12s, read 1 file, listed 1 directory",
            "collapsed summary: {:?}",
            coll[0]
        );
        // Expanded shows the two tool headers and the thinking text.
        let exp = texts(&render_one(&turn, 80, Hl::Styled)).join("\n");
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
            cwd: String::new(),
            execution: None,
            published: None,
        };
        let lines = render_one(&block, 80, Hl::Styled);
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

    /// `assemble_one`'s ONLY use of `carry_in`: a body that opens with a blank line drops it when
    /// something already emitted a line, and keeps it when nothing has. This is the contract that
    /// makes the measure pass's serial `carry_in` prefix necessary before it fans out (#107) —
    /// no block kind opens blank today, so this is where the requirement is written down.
    #[test]
    fn assemble_one_drops_a_leading_blank_only_when_something_preceded_it() {
        let opens_blank = || vec![Line::from(""), Line::from("body")];
        assert_eq!(
            assemble_one(opens_blank(), true).len(),
            2,
            "carry_in: the opening blank is a duplicate separator and goes"
        );
        assert_eq!(
            assemble_one(opens_blank(), false).len(),
            3,
            "no carry: the opening blank is the document's own and stays"
        );
        // A body that opens with content is indifferent — which is every block kind today.
        let opens_solid = || vec![Line::from("body")];
        assert_eq!(
            assemble_one(opens_solid(), true).len(),
            assemble_one(opens_solid(), false).len()
        );
    }
}
