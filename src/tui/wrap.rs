//! Wrap styled lines to a display width, preserving span styles. Word-aware
//! with hard-break fallback for over-long tokens. Unicode-width correct.

use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Tab stop for expanding `\t` in rendered content.
const TAB_STOP: usize = 8;

/// Make a line safe to write to the terminal: expand tabs to spaces (to the next
/// tab stop) and drop other C0 control bytes. A raw `\t` desyncs the terminal
/// cursor from ratatui's column accounting (the terminal jumps to a tab stop while
/// ratatui counts width 1), which corrupts the display with stale-cell fragments —
/// seen on Codex `cat -n`-style tool output. Runs once, before wrapping/measuring.
fn sanitize_line(line: &Line<'static>) -> Line<'static> {
    let needs = line
        .spans
        .iter()
        .any(|s| s.content.chars().any(|c| c.is_control()));
    if !needs {
        return line.clone();
    }
    let mut col = 0usize;
    let mut out: Vec<Span<'static>> = Vec::with_capacity(line.spans.len());
    for sp in &line.spans {
        let mut s = String::with_capacity(sp.content.len());
        for ch in sp.content.chars() {
            match ch {
                '\t' => {
                    let n = TAB_STOP - (col % TAB_STOP);
                    s.push_str(&" ".repeat(n));
                    col += n;
                }
                c if c.is_control() => {} // drop stray \r, \0, ESC, etc.
                c => {
                    s.push(c);
                    col += UnicodeWidthChar::width(c).unwrap_or(0);
                }
            }
        }
        out.push(Span::styled(s, sp.style));
    }
    Line::from(out)
}

/// Wrap one line's spans to `width` columns, returning one or more display lines.
/// Preserves each span's style across the wrap (used for styled table cells too).
///
/// Continuation rows carry a **hanging indent** equal to the line's leading indent
/// (its leading spaces, or a leading marker glyph + space, e.g. `⏺ `/`❯ `), so a
/// wrapped paragraph stays block-aligned under its first line, matching Claude Code.
pub(crate) fn wrap_line(line: &Line<'static>, width: usize) -> Vec<Line<'static>> {
    // Expand tabs / drop control bytes before anything measures or writes them.
    let sanitized = sanitize_line(line);
    let line = &sanitized;
    if width == 0 {
        return vec![line.clone()];
    }
    let indent = leading_indent(line).min(width.saturating_sub(1));
    // Carry only the first span's *background* onto the hang indent, so a block
    // background (user / shell tiers) extends across it — but a plain-fg paragraph
    // (assistant text) leaves the indent unstyled, as Claude Code does.
    let indent_style = line
        .spans
        .first()
        .and_then(|s| s.style.bg)
        .map(|bg| ratatui::style::Style::default().bg(bg))
        .unwrap_or_default();
    let hang = || {
        let mut v: Vec<Span<'static>> = Vec::new();
        if indent > 0 {
            v.push(Span::styled(" ".repeat(indent), indent_style));
        }
        v
    };

    let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
    let mut cur: Vec<Span<'static>> = Vec::new();
    // `floor` is the column a (continuation) row starts at — 0 on the first row,
    // `indent` after the first wrap. Guarding wraps on `col > floor` (not `col > 0`)
    // stops an over-wide token from re-wrapping forever on an already-indented row.
    let mut col = 0usize;
    let mut floor = 0usize;

    for span in &line.spans {
        let style = span.style;
        // Split into words while keeping spaces, so we can break on whitespace.
        for word in split_keep_spaces(&span.content) {
            let w = word.width();
            if col + w > width && col > floor {
                rows.push(std::mem::take(&mut cur));
                cur = hang();
                col = indent;
                floor = indent;
                if word.trim().is_empty() {
                    continue; // drop a space that would start a wrapped row
                }
            }
            if w > width.saturating_sub(floor) {
                // Hard-break an over-long token across rows.
                for ch in word.chars() {
                    let cw = ch.to_string().width();
                    if col + cw > width && col > floor {
                        rows.push(std::mem::take(&mut cur));
                        cur = hang();
                        col = indent;
                        floor = indent;
                    }
                    cur.push(Span::styled(ch.to_string(), style));
                    col += cw;
                }
            } else {
                cur.push(Span::styled(word.to_string(), style));
                col += w;
            }
        }
    }
    rows.push(cur);
    rows.into_iter().map(Line::from).collect()
}

/// The hanging indent for a line's continuation rows: its leading spaces, or — if
/// the line opens with a marker glyph followed by a space (`⏺ `, `❯ `, `✻ `, `⎿ `)
/// — the two columns that glyph+space occupy, so continuations align under the text.
fn leading_indent(line: &Line<'static>) -> usize {
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    let spaces = text.chars().take_while(|c| *c == ' ').count();
    if spaces > 0 {
        return spaces;
    }
    let mut it = text.chars();
    match (it.next(), it.next()) {
        (Some(c), Some(' ')) if is_marker(c) => 2,
        _ => 0,
    }
}

/// Leading glyphs Claude Code uses to open a turn/tool line (marker + space).
fn is_marker(c: char) -> bool {
    matches!(c, '⏺' | '❯' | '✻' | '⎿' | '※' | '•' | '●' | '○')
}

fn split_keep_spaces(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut in_space = false;
    for ch in s.chars() {
        let sp = ch == ' ';
        if buf.is_empty() {
            in_space = sp;
            buf.push(ch);
        } else if sp == in_space {
            buf.push(ch);
        } else {
            out.push(std::mem::take(&mut buf));
            in_space = sp;
            buf.push(ch);
        }
    }
    if !buf.is_empty() {
        out.push(buf);
    }
    out
}

/// Wrap a whole transcript's lines to `width`.
/// Wrap lines while carrying a per-line tag (e.g. the source block index) onto
/// each produced wrapped line — for fold hit-testing and click mapping.
pub fn wrap_all_tagged(
    lines: &[Line<'static>],
    tags: &[usize],
    width: usize,
) -> (Vec<Line<'static>>, Vec<usize>) {
    let mut out_lines = Vec::new();
    let mut out_tags = Vec::new();
    for (l, &tag) in lines.iter().zip(tags) {
        for wl in wrap_line(l, width) {
            out_lines.push(wl);
            out_tags.push(tag);
        }
    }
    (out_lines, out_tags)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_within_width() {
        let line = Line::from("the quick brown fox jumps over the lazy dog");
        let rows = wrap_line(&line, 12);
        assert!(rows.len() > 1);
        for r in &rows {
            assert!(r.width() <= 12, "row too wide: {:?}", r.width());
        }
    }

    #[test]
    fn hard_breaks_overlong_token() {
        let line = Line::from("supercalifragilisticexpialidocious");
        let rows = wrap_line(&line, 10);
        assert!(rows.len() >= 4);
        for r in &rows {
            assert!(r.width() <= 10);
        }
    }

    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn expands_tabs_and_drops_control_bytes() {
        // Codex `cat -n` style: "    35\t// code" — the tab must become spaces so no
        // raw \t reaches the terminal (which would desync the cursor).
        let line = Line::from("    35\t// code");
        let rows = wrap_line(&line, 100);
        let out = text(&rows[0]);
        assert!(!out.contains('\t'), "tab not expanded: {out:?}");
        // "    35" is 6 cols → next tab stop is 8 → 2 spaces before the code.
        assert!(out.starts_with("    35  // code"), "bad tab stop: {out:?}");

        // Other control bytes (stray ESC / CR) are dropped, not written raw.
        let line = Line::from("ab\x1b[31mcd\re");
        let out = text(&wrap_line(&line, 100)[0]);
        assert_eq!(out, "ab[31mcde", "control bytes not stripped: {out:?}");
    }

    /// A marker-led line (`⏺ …`) keeps its continuation rows indented two columns,
    /// so a wrapped turn stays block-aligned under the marker like Claude Code.
    #[test]
    fn marker_line_hangs_continuations_by_two() {
        let line = Line::from("⏺ the quick brown fox jumps over the lazy dog again");
        let rows = wrap_line(&line, 16);
        assert!(rows.len() > 1);
        for r in &rows[1..] {
            assert!(
                text(r).starts_with("  ") && !text(r).starts_with("   "),
                "continuation not hung by 2: {:?}",
                text(r)
            );
            assert!(r.width() <= 16);
        }
    }

    /// A line already indented by spaces keeps that indent on continuation rows.
    #[test]
    fn space_indent_carries_to_continuations() {
        let line = Line::from("    alpha beta gamma delta epsilon zeta eta theta");
        let rows = wrap_line(&line, 14);
        assert!(rows.len() > 1);
        for r in &rows[1..] {
            assert!(text(r).starts_with("    "), "lost indent: {:?}", text(r));
            assert!(r.width() <= 14);
        }
    }
}
