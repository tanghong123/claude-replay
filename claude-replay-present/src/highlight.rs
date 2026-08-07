//! Shared syntect syntax-highlighter. One process-wide `SyntaxSet` plus a
//! hand-built **Claude-Code "subtle" theme** (see `cc_theme`), reused by both
//! `markdown` (fenced code) and `render` (Write/Edit tool bodies) so we don't
//! load syntect twice.

use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::{
    Color as SynColor, ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSettings,
};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

/// One highlighted token: its text and an optional foreground as an **xterm-256 index**
/// (the shared Claude-Code palette). Toolkit-neutral (#86): the TUI maps indices onto its
/// terminal spans, the HTML exporter onto CSS token classes — a new frontend maps onto
/// whatever it renders with, and this crate names no UI toolkit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HlSpan {
    pub text: String,
    pub fg: Option<u8>,
}

struct Syn {
    ps: SyntaxSet,
    theme: Theme,
}

/// Claude Code's deliberately *subtle* code palette: only six token categories
/// are tinted; everything else (types, identifiers, operators, punctuation) is
/// the near-white default foreground. RGB values sampled from CC screenshots.
/// syntect resolves overlapping selectors by specificity (most specific wins),
/// so the broad `constant` (purple) yields to `constant.language` (light blue).
fn cc_theme() -> Theme {
    fn c(r: u8, g: u8, b: u8) -> SynColor {
        SynColor { r, g, b, a: 0xFF }
    }
    fn item(selectors: &str, fg: SynColor) -> ThemeItem {
        ThemeItem {
            scope: selectors
                .parse::<ScopeSelectors>()
                .expect("valid scope selector"),
            style: StyleModifier {
                foreground: Some(fg),
                background: None,
                font_style: None,
            },
        }
    }
    let light_blue = c(129, 213, 251); // keyword / storage / lang-const
    let lime = c(184, 215, 69); // functions & macros
    let pale_yellow = c(216, 216, 146); // strings
    let purple = c(170, 138, 248); // numbers / enum variants / constants
    let crimson = c(234, 52, 99); // self / language variable
    let gray = c(106, 106, 106); // comments
    Theme {
        name: Some("claude-code-subtle".into()),
        author: None,
        settings: ThemeSettings {
            foreground: Some(c(229, 229, 229)),
            ..Default::default()
        },
        scopes: vec![
            item(
                "keyword, storage, keyword.control, constant.language",
                light_blue,
            ),
            item(
                "entity.name.function, support.function, entity.name.macro, support.macro",
                lime,
            ),
            item("string", pale_yellow),
            item(
                "constant, constant.numeric, support.constant, variable.other.enummember",
                purple,
            ),
            item("variable.language", crimson),
            item("comment", gray),
        ],
    }
}

fn syn() -> &'static Syn {
    static S: OnceLock<Syn> = OnceLock::new();
    S.get_or_init(|| {
        let ps = SyntaxSet::load_defaults_newlines();
        Syn {
            ps,
            theme: cc_theme(),
        }
    })
}

/// Highlight `code` with the syntax for `token` (a language name OR a file
/// extension — syntect's `find_syntax_by_token` matches either; falls back to
/// plain text). Returns one `Vec<Span>` per line, with per-token `fg` colors
/// only (no background). Multi-line state (strings, comments) is preserved
/// across lines within the call.
pub fn highlight_spans(code: &str, token: &str) -> Vec<Vec<HlSpan>> {
    highlight_spans_with(code, token, Hl::Styled)
}

/// Whether to actually run syntect.
///
/// `Plain` returns each line as ONE uncoloured span carrying **exactly the text** `Styled`
/// would have split into many — same lines, same characters, same display width — without
/// parsing anything. That is what a MEASURE pass needs: syntect parsing is ~150 µs/line and
/// dominates the first layout of a large session (#107), yet a row's HEIGHT depends only on
/// its width, not on how it was coloured.
///
/// The one thing span segmentation *does* change is where a row wraps, because `wrap_line`
/// breaks words per span. So a caller measuring with `Plain` may only trust the result when
/// nothing wrapped; see the TUI's `measure_block`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hl {
    /// Render for display: always highlight.
    Styled,
    /// Measure only. `width` is the terminal width the result will be wrapped to; see
    /// [`fits_unwrapped`] for the rule that decides, per line, whether syntect runs at all.
    Measure { width: usize },
}

/// Can a line `cols` columns wide be emitted as ONE raw span without changing where it wraps?
///
/// Yes exactly when it cannot wrap at all: a line that fits the width occupies one display row
/// however it was segmented. A line that does NOT fit must be highlighted for real, because
/// `wrap_line` splits words per span, so `["abcdef"]` and `["abc","def"]` break differently.
/// A tab is treated as never fitting — `sanitize_line` expands it later, so its rendered width
/// is not the width measured here.
pub fn fits_unwrapped(text: &str, cols: usize, hl: Hl) -> bool {
    match hl {
        Hl::Styled => false,
        Hl::Measure { width } => {
            !text.contains('\t') && cols + unicode_width::UnicodeWidthStr::width(text) <= width
        }
    }
}

/// [`highlight_spans`] with the mode chosen explicitly; see [`Hl`].
pub fn highlight_spans_with(code: &str, token: &str, hl: Hl) -> Vec<Vec<HlSpan>> {
    // Measuring: emit each line that cannot wrap as ONE uncoloured span carrying exactly the
    // text the styled path would have split — same characters, same width, no syntect. Lines
    // that CAN wrap still go through the highlighter, because their span split decides where.
    if let Hl::Measure { width } = hl {
        if LinesWithEndings::from(code)
            .all(|l| fits_unwrapped(l.trim_end_matches('\n'), 0, Hl::Measure { width }))
        {
            return LinesWithEndings::from(code)
                .map(|line| {
                    vec![HlSpan {
                        text: line.trim_end_matches('\n').to_string(),
                        fg: None,
                    }]
                })
                .collect();
        }
    }
    let s = syn();
    let syntax = (!token.is_empty())
        .then(|| s.ps.find_syntax_by_token(token))
        .flatten()
        .unwrap_or_else(|| s.ps.find_syntax_plain_text());
    let mut h = HighlightLines::new(syntax, &s.theme);
    let mut out = Vec::new();
    for line in LinesWithEndings::from(code) {
        let ranges = h.highlight_line(line, &s.ps).unwrap_or_default();
        let spans = ranges
            .into_iter()
            .map(|(st, text)| {
                let c = st.foreground;
                HlSpan {
                    text: text.trim_end_matches('\n').to_string(),
                    fg: cc_index(c.r, c.g, c.b),
                }
            })
            .collect();
        out.push(spans);
    }
    out
}

/// Map the hand-built syntect palette (RGB) onto Claude Code's 256-colour
/// indices, so peek emits the same `38;5;N` sequences CC does instead of
/// truecolor. Unknown colours fall back to the near-white default (231).
fn cc_index(r: u8, g: u8, b: u8) -> Option<u8> {
    match (r, g, b) {
        (229, 229, 229) => Some(231), // default text
        (129, 213, 251) => Some(81),  // keyword / storage
        (184, 215, 69) => Some(148),  // function / macro
        (216, 216, 146) => Some(186), // string
        (170, 138, 248) => Some(141), // number / constant
        (234, 52, 99) => Some(197),   // self / language variable
        (106, 106, 106) => Some(242), // comment
        _ => Some(231),
    }
}

/// Highlight a single line into styled spans (fg only). Convenience for diff
/// rows; empty input yields no spans.
pub fn highlight_one(line: &str, token: &str) -> Vec<HlSpan> {
    highlight_one_with(line, token, Hl::Styled)
}

/// [`highlight_one`] with the mode chosen explicitly; see [`Hl`].
pub fn highlight_one_with(line: &str, token: &str, hl: Hl) -> Vec<HlSpan> {
    highlight_spans_with(line, token, hl)
        .into_iter()
        .next()
        .unwrap_or_default()
}

/// The syntect token (file extension) for a tool target path, e.g.
/// `justdoit/peek-v2/src/x.rs` -> `rs`. Empty when there's no extension.
pub fn token_for_target(target: &str) -> &str {
    let name = target.rsplit('/').next().unwrap_or(target);
    match name.rsplit_once('.') {
        Some((_, ext)) if !ext.is_empty() => ext,
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// fg index of the first span whose text contains `needle`.
    fn fg_of(spans: &[HlSpan], needle: &str) -> u8 {
        spans
            .iter()
            .find(|s| s.text.contains(needle))
            .unwrap_or_else(|| panic!("no span with {needle:?} in {spans:?}"))
            .fg
            .expect("span has fg")
    }

    #[test]
    fn subtle_palette_colors_rust_tokens() {
        // Colours map to Claude Code's 256-colour indices (not truecolor).
        let spans = highlight_one("let x = Some(2); // c", "rs");
        assert_eq!(fg_of(&spans, "let"), 81, "keyword");
        assert_eq!(fg_of(&spans, "2"), 141, "number");
        assert_eq!(fg_of(&spans, "//"), 242, "comment");
        // Plain identifiers / operators use the near-white default fg (231).
        assert_eq!(fg_of(&spans, "x"), 231, "identifier");
    }

    /// The property that lets a collapsed preview parse only what it prints (#107): syntect's
    /// state flows FORWARD only, so the spans for lines `0..n` are identical whether the parse
    /// stops at `n` or runs to the end. Checked with multi-line state in flight — an unterminated
    /// block comment and an open string — since that is where a backward dependency would show.
    #[test]
    fn a_truncated_parse_matches_the_full_one_line_for_line() {
        let code = "fn a() {}\n/* opens here\nstill inside\n*/\nlet s = \"x\";\nfn b() {}\n";
        let full = highlight_spans(code, "rs");
        for n in 1..=code.lines().count() {
            let end = code
                .match_indices('\n')
                .take(n)
                .last()
                .map_or(code.len(), |(i, _)| i + 1);
            let head = highlight_spans(&code[..end], "rs");
            assert_eq!(
                head.len(),
                n,
                "a {n}-line prefix parses to {n} lines of spans"
            );
            assert_eq!(head[..], full[..n], "prefix of {n} lines diverged");
        }
    }
}
