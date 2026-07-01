//! Minimal markdown -> ratatui Lines renderer, with syntect-highlighted code
//! fences. Supports headings, paragraphs, bold/italic, inline & fenced code,
//! bullet/ordered lists, blockquotes, rules, and pipe tables. HTML is flattened.

use crate::{highlight, theme};
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

#[derive(Default)]
struct Builder {
    lines: Vec<Line<'static>>,
    cur: Vec<Span<'static>>,
    bold: u32,
    italic: u32,
    heading: bool,
    quote: bool,
    list: Vec<Option<u64>>, // ordered counter or None for bullets
}

impl Builder {
    fn style(&self) -> Style {
        let mut s = Style::default();
        if self.heading {
            return theme::heading();
        }
        // Bold/italic keep the default fg (Claude Code only adds the modifier);
        // only inline `code` is recoloured (see Event::Code).
        if self.bold > 0 {
            s = s.add_modifier(Modifier::BOLD);
        }
        if self.italic > 0 {
            s = s.add_modifier(Modifier::ITALIC);
        }
        if self.quote {
            s = theme::dim();
        }
        s
    }
    /// Inline-code style: light-blue emphasis fg, keeping any active bold/italic
    /// (Claude Code renders inline `code` inside **bold** as bold + light blue).
    fn code_style(&self) -> Style {
        let mut s = Style::default().fg(theme::emphasis());
        if self.bold > 0 {
            s = s.add_modifier(Modifier::BOLD);
        }
        if self.italic > 0 {
            s = s.add_modifier(Modifier::ITALIC);
        }
        s
    }
    fn push_text(&mut self, t: &str) {
        if t.is_empty() {
            return;
        }
        self.cur.push(Span::styled(t.to_string(), self.style()));
    }
    fn flush(&mut self) {
        // Always emit (even empty) so paragraph spacing survives.
        let spans = std::mem::take(&mut self.cur);
        self.lines.push(Line::from(spans));
    }
    fn blank(&mut self) {
        if self.lines.last().map(|l| l.width()).unwrap_or(1) != 0 {
            self.lines.push(Line::from(""));
        }
    }
    fn indent(&self) -> String {
        "  ".repeat(self.list.len().saturating_sub(1))
    }
}

/// One table cell: its inline-styled runs (so light-blue inline code and bold
/// emphasis survive into the grid, not flattened to plain text).
type Cell = Vec<Span<'static>>;

/// Accumulates pipe-table cells until the table closes, then renders an aligned
/// grid. Cells keep their inline styling; alignment uses display width.
#[derive(Default)]
struct TableBuf {
    rows: Vec<Vec<Cell>>,
    cur_row: Vec<Cell>,
    cur_cell: Cell,
    header_rows: usize,
    in_head: bool,
}

impl TableBuf {
    fn push_span(&mut self, span: Span<'static>) {
        self.cur_cell.push(span);
    }
    fn end_cell(&mut self) {
        let cell = trim_cell(std::mem::take(&mut self.cur_cell));
        self.cur_row.push(cell);
    }
    fn end_row(&mut self) {
        let row = std::mem::take(&mut self.cur_row);
        self.rows.push(row);
    }
    fn render(self, out: &mut Vec<Line<'static>>, width: usize) {
        render_table(&self.rows, self.header_rows, table_budget(width), out);
    }
}

/// Trim leading/trailing whitespace at the cell's edge spans, dropping any that
/// become empty (pulldown rarely emits edge whitespace, but be safe).
fn trim_cell(mut spans: Cell) -> Cell {
    if let Some(first) = spans.first_mut() {
        let t = first.content.trim_start().to_string();
        first.content = t.into();
    }
    if let Some(last) = spans.last_mut() {
        let t = last.content.trim_end().to_string();
        last.content = t.into();
    }
    spans.retain(|s| !s.content.is_empty());
    spans
}

/// Total display width of a cell's styled runs.
fn cell_width(cell: &[Span<'static>]) -> usize {
    cell.iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum()
}

/// Blank columns reserved beside a table so it doesn't run edge-to-edge: 2 on the
/// left (supplied by the assistant body indent) plus 3 on the right = 5 total.
const TABLE_MARGIN: usize = 5;
/// Fallback budget when the real terminal width isn't known yet (`width == 0`, raw
/// lines built before the first layout). The view re-lays-out once the true width
/// arrives, so this only affects pre-layout raw lines.
const TABLE_FALLBACK_WIDTH: usize = 100;

/// The width budget a table may occupy at terminal `width`: the full terminal width
/// minus the side margins (no upper cap — wide terminals get wide tables). A `width`
/// of 0 falls back to `TABLE_FALLBACK_WIDTH`.
fn table_budget(width: usize) -> usize {
    if width == 0 {
        return TABLE_FALLBACK_WIDTH;
    }
    width.saturating_sub(TABLE_MARGIN).max(4)
}

/// Word-wrap a styled cell to `width` display columns, preserving each run's
/// style across the wrap (reuses the main span-aware wrapper). Always returns at
/// least one physical line.
fn wrap_cell_spans(cell: &[Span<'static>], width: usize) -> Vec<Vec<Span<'static>>> {
    if width == 0 {
        return vec![Vec::new()];
    }
    let line = Line::from(cell.to_vec());
    let wrapped = crate::wrap::wrap_line(&line, width);
    if wrapped.is_empty() {
        return vec![Vec::new()];
    }
    wrapped.into_iter().map(|l| l.spans).collect()
}

/// Allocate `natural` column widths within `budget` total content columns. If the
/// natural widths already fit (`sum ≤ budget`), they're returned unchanged — a
/// narrow table stays narrow, it is not padded to fill the width. When they're too
/// wide, max-min fair sharing shrinks them: each column gets a quota of
/// `remaining_budget / remaining_cols`; any column narrower than its quota is fixed
/// at its natural width and removed from the pool (freeing budget for the rest),
/// repeating until only over-quota columns remain — those split the leftover budget
/// evenly (and wrap to it). Every column stays ≥1, and the total never exceeds budget.
fn fair_share_widths(natural: &[usize], budget: usize) -> Vec<usize> {
    if natural.is_empty() || natural.iter().sum::<usize>() <= budget {
        return natural.to_vec();
    }
    let mut out = vec![0usize; natural.len()];
    let mut fixed = vec![false; natural.len()];
    // Repeatedly fix columns that fit under the current fair quota.
    loop {
        let used: usize = (0..natural.len())
            .filter(|&i| fixed[i])
            .map(|i| out[i])
            .sum();
        let remaining_cols = fixed.iter().filter(|f| !**f).count();
        if remaining_cols == 0 {
            break;
        }
        let quota = budget.saturating_sub(used) / remaining_cols;
        let mut changed = false;
        for i in 0..natural.len() {
            if !fixed[i] && natural[i] <= quota {
                out[i] = natural[i];
                fixed[i] = true;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    // The over-quota columns that remain split the leftover budget evenly.
    let used: usize = (0..natural.len())
        .filter(|&i| fixed[i])
        .map(|i| out[i])
        .sum();
    let wide: Vec<usize> = (0..natural.len()).filter(|&i| !fixed[i]).collect();
    if !wide.is_empty() {
        let leftover = budget.saturating_sub(used);
        let base = (leftover / wide.len()).max(1);
        let mut extra = leftover.saturating_sub(base * wide.len());
        for &i in &wide {
            out[i] = base
                + if extra > 0 {
                    extra -= 1;
                    1
                } else {
                    0
                };
        }
    }
    out
}

/// Render a pipe table as a closed box-drawing grid, capped to `max_width`
/// (columns shrink and cells wrap to fit). Header cells are bolded on top of their
/// inline styling; borders use the higher-contrast table-border color.
fn render_table(
    rows: &[Vec<Cell>],
    header_rows: usize,
    max_width: usize,
    out: &mut Vec<Line<'static>>,
) {
    if rows.is_empty() {
        return;
    }
    let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if cols == 0 {
        return;
    }

    // Natural column widths (≥1) by display width, then shrink to the budget.
    let mut widths = vec![1usize; cols];
    for r in rows {
        for (i, cell) in r.iter().enumerate() {
            widths[i] = widths[i].max(cell_width(cell).max(1));
        }
    }
    // Per the box layout, total width = sum(widths) + 3*cols + 1 (borders+padding).
    let overhead = 3 * cols + 1;
    let budget = max_width.saturating_sub(overhead).max(cols);
    let widths = fair_share_widths(&widths, budget);

    let border = theme::table_border();
    let rule = |left: &str, mid: &str, right: &str| -> Line<'static> {
        let mut s = String::from(left);
        for (i, w) in widths.iter().enumerate() {
            s.push_str(&"─".repeat(w + 2));
            s.push_str(if i + 1 < cols { mid } else { right });
        }
        Line::from(Span::styled(s, border))
    };

    out.push(rule("┌", "┬", "┐"));
    for (ri, r) in rows.iter().enumerate() {
        let is_header = ri < header_rows;
        // Wrap each styled cell to its column width (span-aware), then emit as
        // many physical lines as the tallest cell.
        let empty: Cell = Vec::new();
        let wrapped: Vec<Vec<Cell>> = (0..cols)
            .map(|i| wrap_cell_spans(r.get(i).unwrap_or(&empty), widths[i]))
            .collect();
        let height = wrapped.iter().map(|c| c.len()).max().unwrap_or(1);
        for k in 0..height {
            let mut spans = vec![Span::styled("│", border)];
            for (i, w) in widths.iter().enumerate() {
                let line = wrapped[i].get(k).cloned().unwrap_or_default();
                let used = cell_width(&line);
                // Header cells are centered (like Claude Code); data cells are
                // left-aligned. Split the slack into a leading/trailing pad.
                let pad = w.saturating_sub(used);
                let (lead, trail) = if is_header {
                    let l = pad / 2;
                    (l, pad - l)
                } else {
                    (0, pad)
                };
                spans.push(Span::raw(format!(" {}", " ".repeat(lead)))); // left pad + centering
                for s in line {
                    // Claude Code does NOT bold header cells (only centres them) — keep
                    // each run's own inline style, header or not.
                    spans.push(Span::styled(s.content, s.style));
                }
                // right padding to the column width, plus the trailing space.
                spans.push(Span::raw(format!("{} ", " ".repeat(trail))));
                spans.push(Span::styled("│", border));
            }
            out.push(Line::from(spans));
        }
        // Claude Code draws a horizontal rule between *every* row (header and
        // data alike), not just under the header — the bottom border closes the
        // last row.
        if ri + 1 < rows.len() {
            out.push(rule("├", "┼", "┤"));
        }
    }
    out.push(rule("└", "┴", "┘"));
}

fn highlight_code(code: &str, lang: &str) -> Vec<Line<'static>> {
    // No extra code indent: Claude Code renders a code block at the message's body
    // indent (the 2-space hang added by `render_one`), letting the syntax colour —
    // not extra whitespace — set it apart. Emitting our own indent here double-inset
    // it to 6 columns.
    highlight::highlight_spans(code, lang)
        .into_iter()
        .map(Line::from)
        .collect()
}

/// Render markdown text into styled lines, laying tables out for terminal `width`.
pub fn render(text: &str, width: usize) -> Vec<Line<'static>> {
    let mut b = Builder::default();
    let mut code_buf = String::new();
    let mut code_lang = String::new();
    let mut in_code = false;
    let mut table: Option<TableBuf> = None;

    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    for ev in Parser::new_ext(text, opts) {
        match ev {
            // --- pipe tables: buffer cells, render the aligned grid on close ---
            Event::Start(Tag::Table(_)) => {
                b.flush();
                b.blank();
                table = Some(TableBuf::default());
            }
            Event::End(TagEnd::Table) => {
                if let Some(t) = table.take() {
                    t.render(&mut b.lines, width);
                }
                b.blank();
            }
            Event::Start(Tag::TableHead) => {
                if let Some(t) = table.as_mut() {
                    t.in_head = true;
                }
            }
            Event::End(TagEnd::TableHead) => {
                if let Some(t) = table.as_mut() {
                    t.end_row();
                    t.header_rows = 1;
                    t.in_head = false;
                }
            }
            Event::Start(Tag::TableRow) => {} // cells accumulate; row closes below
            Event::End(TagEnd::TableRow) => {
                if let Some(t) = table.as_mut() {
                    t.end_row();
                }
            }
            Event::Start(Tag::TableCell) => {
                if let Some(t) = table.as_mut() {
                    t.cur_cell.clear();
                }
            }
            Event::End(TagEnd::TableCell) => {
                if let Some(t) = table.as_mut() {
                    t.end_cell();
                }
            }
            // While inside a table, inline text/code feed the current cell —
            // keeping their styling (bold/italic emphasis, light-blue inline code).
            Event::Text(t) if table.is_some() => {
                let style = b.style();
                table
                    .as_mut()
                    .unwrap()
                    .push_span(Span::styled(t.to_string(), style));
            }
            Event::Code(t) if table.is_some() => {
                let st = b.code_style();
                table
                    .as_mut()
                    .unwrap()
                    .push_span(Span::styled(t.to_string(), st));
            }
            Event::SoftBreak if table.is_some() => {
                table.as_mut().unwrap().push_span(Span::raw(" "));
            }
            Event::Start(Tag::Heading { .. }) => {
                // Conditional flush: an unconditional one emits an empty first line
                // when the heading opens the block, which then steals the `⏺` marker
                // and pushes the heading text to an indented second line.
                if !b.cur.is_empty() {
                    b.flush();
                }
                b.heading = true;
            }
            Event::End(TagEnd::Heading(_)) => {
                b.heading = false;
                b.flush();
                b.blank();
            }
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => {
                b.flush();
                // Inside a list, don't add a blank between items (avoids the
                // over-spaced loose-list look); paragraphs elsewhere still gap.
                if b.list.is_empty() {
                    b.blank();
                }
            }
            Event::Start(Tag::Strong) => b.bold += 1,
            Event::End(TagEnd::Strong) => b.bold = b.bold.saturating_sub(1),
            Event::Start(Tag::Emphasis) => b.italic += 1,
            Event::End(TagEnd::Emphasis) => b.italic = b.italic.saturating_sub(1),
            Event::Start(Tag::BlockQuote(_)) => {
                b.flush();
                b.quote = true;
                b.push_text("│ ");
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                b.quote = false;
                b.flush();
            }
            Event::Start(Tag::List(start)) => b.list.push(start),
            Event::End(TagEnd::List(_)) => {
                b.list.pop();
            }
            Event::Start(Tag::Item) => {
                // Only flush pending content — an unconditional flush here emits
                // an empty line per item, which over-spaces tight lists.
                if !b.cur.is_empty() {
                    b.flush();
                }
                let indent = b.indent();
                let marker = match b.list.last_mut() {
                    Some(Some(n)) => {
                        let m = format!("{n}. ");
                        *n += 1;
                        m
                    }
                    _ => "- ".to_string(),
                };
                b.cur.push(Span::raw(format!("{indent}{marker}")));
            }
            Event::End(TagEnd::Item) => {
                if !b.cur.is_empty() {
                    b.flush();
                }
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                // Flush only pending inline content — an unconditional flush emits an
                // empty line (spurious blank before the block) when a list item's
                // paragraph already flushed, which CC doesn't show.
                if !b.cur.is_empty() {
                    b.flush();
                }
                in_code = true;
                code_buf.clear();
                code_lang = match kind {
                    CodeBlockKind::Fenced(l) => l.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
            }
            Event::End(TagEnd::CodeBlock) => {
                in_code = false;
                for l in highlight_code(&code_buf, &code_lang) {
                    b.lines.push(l);
                }
                b.blank();
            }
            Event::Text(t) => {
                if in_code {
                    code_buf.push_str(&t);
                } else {
                    b.push_text(&t);
                }
            }
            Event::Code(t) => {
                // No literal back-ticks; inline code reads light blue (emphasis),
                // keeping any active bold/italic so `code` inside **bold** stays bold.
                let st = b.code_style();
                b.cur.push(Span::styled(t.to_string(), st));
            }
            Event::SoftBreak => {
                if in_code {
                    code_buf.push('\n');
                } else {
                    b.push_text(" ");
                }
            }
            Event::HardBreak => b.flush(),
            Event::Rule => {
                b.flush();
                b.lines.push(Line::styled("───", theme::dim()));
                b.blank();
            }
            _ => {}
        }
    }
    b.flush();
    // Trim a trailing blank line.
    while b.lines.last().map(|l| l.width() == 0).unwrap_or(false) {
        b.lines.pop();
    }
    b.lines
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Flatten a rendered line back to its plain text.
    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// One plain-text table cell (single unstyled run).
    fn cell(s: &str) -> Cell {
        vec![Span::raw(s.to_string())]
    }

    #[test]
    fn table_budget_subtracts_margin_and_is_uncapped() {
        assert_eq!(table_budget(80), 75, "width − 5 margin");
        // No 100 cap: a very wide terminal yields a proportionally wide budget.
        assert_eq!(table_budget(300), 295, "no MAX_TABLE_WIDTH cap");
        assert_eq!(table_budget(0), 100, "width 0 → fallback");
    }

    #[test]
    fn fair_share_keeps_natural_widths_when_under_budget() {
        // sum = 18 < budget 20: a narrow table stays at natural widths (no padding).
        assert_eq!(fair_share_widths(&[3, 5, 10], 20), vec![3, 5, 10]);
    }

    #[test]
    fn fair_share_keeps_widths_when_exactly_budget() {
        assert_eq!(fair_share_widths(&[4, 6, 10], 20), vec![4, 6, 10]);
    }

    #[test]
    fn fair_share_gives_narrow_cols_their_max_and_shares_rest() {
        // budget 20, three cols. Quota starts at 6: the narrow cols (2, 3) fit and
        // are fixed at their natural width; the wide col takes the remaining 15.
        assert_eq!(fair_share_widths(&[2, 3, 40], 20), vec![2, 3, 15]);
        // Total never exceeds the budget.
        assert!(fair_share_widths(&[2, 3, 40], 20).iter().sum::<usize>() <= 20);
    }

    #[test]
    fn fair_share_splits_evenly_when_all_wide() {
        // No column fits under the quota (avg 5) → split 16 evenly, remainder
        // distributed to the leading columns; every column stays ≥1.
        let w = fair_share_widths(&[40, 40, 40], 16);
        assert_eq!(w, vec![6, 5, 5]);
        assert!(w.iter().all(|&x| x >= 1));
        assert!(w.iter().sum::<usize>() <= 16);
    }

    #[test]
    fn renders_table_as_aligned_grid_no_raw_pipes() {
        let md = "\
| Name | Role |
|------|------|
| Ann | Dev |
| Bob | PM |
";
        let lines = render(md, 100);
        let joined: Vec<String> = lines.iter().map(text).collect();
        let all = joined.join("\n");

        // No raw markdown pipe-table syntax leaks through.
        assert!(!all.contains("|------|"), "separator pipes leaked:\n{all}");
        assert!(!all.contains("| Name |"), "header pipes leaked:\n{all}");

        // Header, a ─ rule, and aligned columns joined by │.
        let header = joined.iter().find(|l| l.contains("Name")).expect("header");
        assert!(header.contains('│'), "header missing column join: {header}");
        assert!(
            joined.iter().any(|l| l.contains('─')),
            "missing separator rule:\n{all}"
        );

        // Columns are padded to a common width: "Ann" and "Bob" cells align so
        // the ` │ ` separator lands at the same display column on both rows.
        let r_ann = joined.iter().find(|l| l.contains("Ann")).expect("ann row");
        let r_bob = joined.iter().find(|l| l.contains("Bob")).expect("bob row");
        let col = |s: &str| UnicodeWidthStr::width(&s[..s.find('│').unwrap()]);
        assert_eq!(
            col(r_ann),
            col(r_bob),
            "columns not aligned:\n{r_ann}\n{r_bob}"
        );
    }

    #[test]
    fn table_caps_width_and_wraps_long_cells() {
        let rows = vec![
            vec![cell("Name"), cell("Description")],
            vec![
                cell("x"),
                cell("a very long description that absolutely must wrap across several lines"),
            ],
        ];
        let mut out = Vec::new();
        super::render_table(&rows, 1, 30, &mut out);
        let lines: Vec<String> = out.iter().map(text).collect();

        // Closed box grid.
        assert!(lines.first().unwrap().starts_with('┌'), "no top border");
        assert!(lines.last().unwrap().starts_with('└'), "no bottom border");
        assert!(lines.iter().any(|l| l.starts_with('├')), "no header rule");

        // Never exceeds the width budget.
        for l in &lines {
            assert!(
                UnicodeWidthStr::width(l.as_str()) <= 30,
                "line too wide ({}): {l}",
                UnicodeWidthStr::width(l.as_str())
            );
        }
        // The long cell wrapped onto continuation rows (more than the 5 lines a
        // single-row table would have).
        assert!(
            lines.len() > 5,
            "long cell did not wrap: {} lines",
            lines.len()
        );
    }

    /// A table that fits keeps each cell on one line; a narrow terminal forces it
    /// to wrap. Verifies width is threaded through (not the old fixed cap).
    /// Inline styling survives into table cells: `` `code` `` renders light-blue
    /// (emphasis fg) and `**bold**` keeps the bold modifier.
    #[test]
    fn table_cells_keep_inline_styling() {
        let md = "| Hash | Note |\n|--|--|\n| `deadbeef` | **important** |\n";
        let lines = render(md, 100);
        let spans: Vec<&Span> = lines.iter().flat_map(|l| &l.spans).collect();

        assert!(
            spans
                .iter()
                .any(|s| s.content.contains("deadbeef") && s.style.fg == Some(theme::emphasis())),
            "inline code cell not light-blue"
        );
        assert!(
            spans.iter().any(|s| s.content.contains("important")
                && s.style.add_modifier.contains(Modifier::BOLD)),
            "bold cell not bold"
        );
    }

    /// A horizontal rule (`├…┤`) separates every row, header and data alike.
    #[test]
    fn table_has_rule_between_every_row() {
        let md = "| H |\n|--|\n| a |\n| b |\n| c |\n";
        let lines: Vec<String> = render(md, 100).iter().map(text).collect();
        // 4 rows (H,a,b,c) → 3 interior rules (H│a, a│b, b│c).
        let rules = lines.iter().filter(|l| l.starts_with('├')).count();
        assert_eq!(
            rules,
            3,
            "expected a rule between every row:\n{}",
            lines.join("\n")
        );
    }

    #[test]
    fn table_wraps_only_when_width_demands() {
        let md = "| Key | Value |\n|--|--|\n| id | 0123456789abcdef0123456789abcdef |\n";
        let val = "0123456789abcdef0123456789abcdef";

        let wide: Vec<String> = render(md, 120).iter().map(text).collect();
        assert!(
            wide.iter().any(|l| l.contains(val)),
            "value should stay unwrapped at width 120:\n{}",
            wide.join("\n")
        );

        // The 32-char value + "id"/headers + borders need ~47 cols to fit; at width
        // 40 the budget (40 − 5 margin − 7 overhead = 28) forces the value to wrap.
        let narrow: Vec<String> = render(md, 40).iter().map(text).collect();
        assert!(
            !narrow.iter().any(|l| l.contains(val)),
            "value should wrap at width 40:\n{}",
            narrow.join("\n")
        );
    }

    #[test]
    fn inline_code_keeps_bold_when_nested_in_emphasis() {
        // CC renders inline `code` inside **bold** as bold + light blue.
        let lines = render("**a `b` c**", 80);
        let b_span = lines
            .iter()
            .flat_map(|l| &l.spans)
            .find(|s| s.content.as_ref() == "b")
            .expect("inline code span 'b'");
        assert_eq!(
            b_span.style.fg,
            Some(theme::emphasis()),
            "inline code is light blue"
        );
        assert!(
            b_span.style.add_modifier.contains(Modifier::BOLD),
            "inline code keeps the surrounding bold"
        );
    }

    #[test]
    fn inline_code_has_no_backticks() {
        let lines = render("call `foo_bar()` now", 100);
        let all: String = lines.iter().map(text).collect::<Vec<_>>().join("\n");
        assert!(all.contains("foo_bar()"), "code text missing:\n{all}");
        assert!(!all.contains('`'), "back-ticks leaked:\n{all}");
    }

    /// Inline code renders in the light-blue emphasis fg; bold keeps the default
    /// fg and only adds the BOLD modifier (matching Claude Code).
    #[test]
    fn inline_code_is_light_blue_bold_is_default_fg() {
        let emph = Some(theme::emphasis());

        let code = render("call `foo()` now", 100);
        assert!(
            code.iter()
                .flat_map(|l| &l.spans)
                .any(|s| s.content.contains("foo()") && s.style.fg == emph),
            "inline code not light-blue"
        );

        let bold = render("this is **strong** text", 100);
        let strong = bold
            .iter()
            .flat_map(|l| &l.spans)
            .find(|s| s.content.contains("strong"))
            .expect("bold span");
        assert!(
            strong.style.add_modifier.contains(Modifier::BOLD),
            "bold missing BOLD modifier"
        );
        assert_ne!(strong.style.fg, emph, "bold should not be recoloured");
    }

    #[test]
    fn bullets_use_dash_and_tight_lists_have_no_blank_between_items() {
        let lines = render("- one\n- two\n- three\n", 100);
        let t: Vec<String> = lines.iter().map(text).collect();
        assert!(
            t.iter().any(|l| l.starts_with("- one")),
            "no dash bullet:\n{t:?}"
        );
        assert!(
            !t.iter().any(|l| l.contains('•')),
            "old bullet glyph:\n{t:?}"
        );
        // No blank line separating the items.
        assert!(
            !t.iter().any(|l| l.is_empty()),
            "tight list has blank lines:\n{t:?}"
        );
    }

    /// A code block inside a list item renders at the body indent (no extra code
    /// inset) with no spurious blank line between the item text and the code —
    /// matching Claude Code.
    #[test]
    fn code_block_in_list_has_no_extra_indent_or_blank() {
        let lines = render("1. Do this:\n\n       let x = 1;\n", 100);
        let t: Vec<String> = lines.iter().map(text).collect();
        let item = t.iter().position(|l| l.contains("Do this")).unwrap();
        let code = t.iter().position(|l| l.contains("let x = 1;")).unwrap();
        // Code follows the item text directly — no blank between them.
        assert_eq!(code, item + 1, "blank inserted before code block:\n{t:?}");
        // Code carries no leading indent of its own (the body hang adds the 2).
        assert!(
            t[code].starts_with("let x = 1;"),
            "code block double-indented:\n{:?}",
            t[code]
        );
    }

    /// A heading that opens the message is the block's first line (no empty line
    /// before it), so `render_one` puts the `⏺` marker on the heading itself.
    #[test]
    fn leading_heading_is_first_line() {
        let lines = render("## Diagnosis\n\nBody text.\n", 100);
        let t: Vec<String> = lines.iter().map(text).collect();
        assert_eq!(t[0], "Diagnosis", "heading not first line:\n{t:?}");
    }
}
