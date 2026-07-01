//! Colors/styles, tuned to read like Claude Code's transcript.

use ratatui::style::{Color, Modifier, Style};

pub fn user() -> Style {
    Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}
pub fn assistant_marker() -> Style {
    // Claude Code's `⏺` assistant marker: plain near-white (256-color 231), no bold.
    Style::default().fg(Color::Indexed(231))
}
pub fn thinking() -> Style {
    // CC's `✻` thinking summary is mid-grey (256-color 246).
    Style::default().fg(Color::Indexed(246))
}
pub fn tool() -> Style {
    // CC's `⏺` tool-call marker + tool name are green (256-color 114).
    Style::default().fg(Color::Indexed(114))
}
pub fn result() -> Style {
    Style::default().fg(Color::DarkGray)
}
pub fn diff_add() -> Style {
    // CC's added gutter/marker green (256-colour 77).
    Style::default().fg(Color::Indexed(77))
}
pub fn diff_del() -> Style {
    // CC's removed gutter/marker red (256-colour 167).
    Style::default().fg(Color::Indexed(167))
}
/// Background fills for added/removed diff lines, matching Claude Code's
/// 256-colour dark green (22) / dark red (52).
pub fn diff_add_bg() -> Color {
    Color::Indexed(22)
}
pub fn diff_del_bg() -> Color {
    Color::Indexed(52)
}

// --- beige background tiers (most → least prominent): user > shell/read > thinking.
// Each pairs a faint background block with a foreground; thinking is the faintest
// of both. (Tuned toward Claude Code; refine from screenshots.)
#[allow(dead_code)] // wired by render/view in later styling tasks
pub fn user_bg() -> Color {
    // Claude Code's user-block grey (256-colour 237 ≈ rgb 58,58,58).
    Color::Indexed(237)
}
/// Background block behind an *expanded* shell/read foldable (command + output) —
/// a neutral medium-dark gray, clearly lighter than the page. Sampled from CC.
pub fn shell_expanded_bg() -> Color {
    Color::Rgb(70, 70, 70)
}
#[allow(dead_code)] // wired by render/view in later styling tasks
pub fn user_fg() -> Color {
    // CC renders user prompt text near-white (256-color 231) on the grey block.
    Color::Indexed(231)
}
/// The dim `❯` caret CC puts before a user prompt (256-color 239).
pub fn user_marker() -> Color {
    Color::Indexed(239)
}
#[allow(dead_code)] // wired by render/view in later styling tasks
pub fn shell_bg() -> Color {
    Color::Rgb(34, 32, 28)
}
#[allow(dead_code)] // wired by render/view in later styling tasks
pub fn shell_fg() -> Color {
    Color::Rgb(180, 168, 148)
}
#[allow(dead_code)] // wired by render/view in later styling tasks
pub fn thinking_bg() -> Color {
    Color::Rgb(26, 25, 23)
}
#[allow(dead_code)] // wired by render/view in later styling tasks
pub fn thinking_fg() -> Color {
    Color::Rgb(132, 126, 114)
}
/// Consistent color for every foldable header; `_focused` is the
/// hover/keyboard-selected (brighter) variant.
#[allow(dead_code)] // wired by render/view in later styling tasks
pub fn fold_header() -> Color {
    Color::Rgb(166, 154, 134)
}
#[allow(dead_code)] // wired by render/view in later styling tasks
pub fn fold_header_focused() -> Color {
    Color::Rgb(222, 208, 182)
}
/// Inline code — Claude Code's light blue (256-colour 153).
pub fn emphasis() -> Color {
    Color::Indexed(153)
}
pub fn heading() -> Style {
    // Claude Code renders markdown headings bold in the default fg (not coloured).
    Style::default().add_modifier(Modifier::BOLD)
}
pub fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}
pub fn badge() -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(Color::Yellow)
        .add_modifier(Modifier::BOLD)
}
pub fn status() -> Style {
    Style::default().fg(Color::DarkGray)
}
/// Table box-drawing borders — Claude Code draws them in the default fg (no colour).
pub fn table_border() -> Style {
    Style::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sum(c: Color) -> u32 {
        match c {
            Color::Rgb(r, g, b) => r as u32 + g as u32 + b as u32,
            // The two grey 256-indices used as block backgrounds, by brightness.
            Color::Indexed(237) => 58 * 3,
            Color::Indexed(n) => n as u32,
            _ => 0,
        }
    }

    #[test]
    fn emphasis_is_cc_inline_code_index() {
        // Claude Code's inline-code colour: 256-colour index 153 (light blue).
        assert_eq!(emphasis(), Color::Indexed(153));
    }

    #[test]
    fn sampled_palette_values() {
        assert_eq!(emphasis(), Color::Indexed(153));
        assert_eq!(user_bg(), Color::Indexed(237));
        assert_eq!(shell_expanded_bg(), Color::Rgb(70, 70, 70));
    }

    #[test]
    fn background_tiers_are_ordered() {
        // Prominence ladder: user > shell/read > thinking (bg brightness).
        assert!(sum(user_bg()) > sum(shell_bg()), "user bg > shell bg");
        assert!(
            sum(shell_bg()) > sum(thinking_bg()),
            "shell bg > thinking bg"
        );
        // Thinking foreground is the dimmest.
        assert!(
            sum(shell_fg()) > sum(thinking_fg()),
            "shell fg > thinking fg"
        );
        // Focused fold header is brighter than the resting one.
        assert!(
            sum(fold_header_focused()) > sum(fold_header()),
            "focused header brighter"
        );
    }
}
