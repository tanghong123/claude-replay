//! Shared syntect syntax-highlighter. One process-wide `SyntaxSet` plus a
//! hand-built **Claude-Code "subtle" theme** (see `cc_theme`), reused by both
//! `markdown` (fenced code) and `render` (Write/Edit tool bodies) so we don't
//! load syntect twice.

use ratatui::style::{Color, Style};
use ratatui::text::Span;
use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::{
    Color as SynColor, ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSettings,
};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

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
pub fn highlight_spans(code: &str, token: &str) -> Vec<Vec<Span<'static>>> {
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
                Span::styled(
                    text.trim_end_matches('\n').to_string(),
                    Style::default().fg(cc_index(c.r, c.g, c.b)),
                )
            })
            .collect();
        out.push(spans);
    }
    out
}

/// Map the hand-built syntect palette (RGB) onto Claude Code's 256-colour
/// indices, so peek emits the same `38;5;N` sequences CC does instead of
/// truecolor. Unknown colours fall back to the near-white default (231).
fn cc_index(r: u8, g: u8, b: u8) -> Color {
    match (r, g, b) {
        (229, 229, 229) => Color::Indexed(231), // default text
        (129, 213, 251) => Color::Indexed(81),  // keyword / storage
        (184, 215, 69) => Color::Indexed(148),  // function / macro
        (216, 216, 146) => Color::Indexed(186), // string
        (170, 138, 248) => Color::Indexed(141), // number / constant
        (234, 52, 99) => Color::Indexed(197),   // self / language variable
        (106, 106, 106) => Color::Indexed(242), // comment
        _ => Color::Indexed(231),
    }
}

/// Highlight a single line into styled spans (fg only). Convenience for diff
/// rows; empty input yields no spans.
pub fn highlight_one(line: &str, token: &str) -> Vec<Span<'static>> {
    highlight_spans(line, token)
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

    /// fg color of the first span whose text contains `needle`.
    fn fg_of(spans: &[Span<'static>], needle: &str) -> Color {
        spans
            .iter()
            .find(|s| s.content.contains(needle))
            .unwrap_or_else(|| panic!("no span with {needle:?} in {spans:?}"))
            .style
            .fg
            .expect("span has fg")
    }

    #[test]
    fn subtle_palette_colors_rust_tokens() {
        // Colours map to Claude Code's 256-colour indices (not truecolor).
        let spans = highlight_one("let x = Some(2); // c", "rs");
        assert_eq!(fg_of(&spans, "let"), Color::Indexed(81), "keyword");
        assert_eq!(fg_of(&spans, "2"), Color::Indexed(141), "number");
        assert_eq!(fg_of(&spans, "//"), Color::Indexed(242), "comment");
        // Plain identifiers / operators use the near-white default fg (231).
        assert_eq!(fg_of(&spans, "x"), Color::Indexed(231), "identifier");
    }
}
