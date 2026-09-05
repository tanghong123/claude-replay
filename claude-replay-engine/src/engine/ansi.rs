//! Terminal styling out of the text a viewer shows (#130).
//!
//! Agents record what a command printed to a terminal, escape codes and all: Claude Code writes
//! `<local-command-stdout>` with the dim/undim pair around it, a build tool that forces colour
//! writes them into a tool result. Nothing downstream renders them — the browser drops the ESC
//! byte and shows the rest, so a reader sees `[2mCompacted (ctrl+o to see full summary) [22m`
//! where the agent meant dim text, and the TUI has its own styling to apply. So they come out
//! here, once, where transcript text becomes block content — before the html wire, the TUI and
//! `--dump` ever see it.
//!
//! Removed: CSI sequences (`ESC [ … final`), OSC strings (`ESC ] … BEL` or `ESC \`), and the
//! two-byte escapes. A lone ESC with nothing recognisable after it is dropped with the byte
//! that follows, which is what a terminal would do with it.

use std::borrow::Cow;

/// The text without its terminal styling. Borrowed unchanged when there is no escape at all —
/// the overwhelming case, and this runs over every line of every output.
pub fn strip_ansi(s: &str) -> Cow<'_, str> {
    if !s.contains('\u{1b}') {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.char_indices().peekable();
    while let Some((_, c)) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.peek().map(|&(_, n)| n) {
            // CSI: parameters and intermediates, then one final byte in @…~.
            Some('[') => {
                chars.next();
                for (_, n) in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&n) {
                        break;
                    }
                }
            }
            // OSC: a string terminated by BEL or ST (ESC \).
            Some(']') => {
                chars.next();
                while let Some((_, n)) = chars.next() {
                    if n == '\u{7}' {
                        break;
                    }
                    if n == '\u{1b}' {
                        if matches!(chars.peek().map(|&(_, m)| m), Some('\\')) {
                            chars.next();
                        }
                        break;
                    }
                }
            }
            // Anything else is a two-byte escape (or a stray ESC at the end).
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::strip_ansi;

    #[test]
    fn plain_text_is_borrowed_unchanged() {
        assert!(matches!(
            strip_ansi("nothing to strip"),
            std::borrow::Cow::Borrowed(_)
        ));
        assert_eq!(strip_ansi("nothing to strip"), "nothing to strip");
    }

    #[test]
    fn the_dim_pair_a_slash_command_writes_comes_out() {
        // What the owner saw: the ESC byte is invisible in HTML, so the codes read as text.
        let seen = "\u{1b}[2mCompacted (ctrl+o to see full summary) \u{1b}[22m";
        assert_eq!(strip_ansi(seen), "Compacted (ctrl+o to see full summary) ");
    }

    #[test]
    fn colours_hyperlinks_and_cursor_moves_come_out_together() {
        let seen = "\u{1b}[1;31merror\u{1b}[0m: \u{1b}]8;;https://example.test\u{7}link\u{1b}]8;;\u{7}\u{1b}[2K done";
        assert_eq!(strip_ansi(seen), "error: link done");
    }

    #[test]
    fn an_osc_terminated_by_st_ends_there() {
        assert_eq!(strip_ansi("a\u{1b}]0;title\u{1b}\\b"), "ab");
    }

    #[test]
    fn text_that_merely_mentions_brackets_is_untouched() {
        assert_eq!(
            strip_ansi("[2m is not an escape without ESC"),
            "[2m is not an escape without ESC"
        );
    }
}
