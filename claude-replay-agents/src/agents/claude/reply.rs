//! Exact recognition of the two measured Claude Desktop quotation layouts.
use claude_replay_engine::seam::{ReplyContext, ReplyItem, ReplyResponse};

/// Preserve complete candidate messages even when recognition fails.
pub(crate) fn candidate(text: &str) -> bool {
    text.trim_start().starts_with("<!-- reply")
}

/// Split only unfenced standalone markers. A marker inside selected text or a
/// fenced code block is content; an unsupported or ambiguous outside marker fails.
pub(crate) fn parse(text: &str) -> Option<ReplyContext> {
    if text.contains('\r') {
        return None;
    }
    let (rest, count) = if let Some(rest) = text.strip_prefix("<!-- reply 1 -->\n") {
        (rest, 1)
    } else {
        (text.strip_prefix("<!-- reply 2 -->\n")?, 2)
    };
    let mut cursor = rest;
    let mut items = Vec::new();
    for index in 0..count {
        let mut quote = Vec::new();
        while let Some(line) = cursor.strip_prefix("> ") {
            let (selected, rest) = line.split_once('\n')?;
            // An empty quote line is allowed within a nonempty selection.
            quote.push(selected);
            cursor = rest;
        }
        if quote.is_empty() || quote.iter().all(|line| line.is_empty()) {
            return None;
        }
        cursor = cursor.strip_prefix('\n')?;
        let mut fence: Option<(u8, usize)> = None;
        let mut boundary = None;
        let mut offset = 0;
        for line in cursor.split_inclusive('\n') {
            let bare = line.strip_suffix('\n').unwrap_or(line);
            let trimmed = bare.trim_start_matches(' ');
            let indent = bare.len() - trimmed.len();
            let first = trimmed.as_bytes().first().copied();
            if indent <= 3 && matches!(first, Some(b'`' | b'~')) {
                let ch = first?;
                let width = trimmed.bytes().take_while(|&c| c == ch).count();
                if width >= 3 {
                    match fence {
                        None => fence = Some((ch, width)),
                        Some((open, n))
                            if ch == open && width >= n && trimmed[width..].trim().is_empty() =>
                        {
                            fence = None
                        }
                        _ => {}
                    }
                }
            } else if fence.is_none() && bare.starts_with("<!-- reply") {
                if bare != "<!-- reply -->" || !line.ends_with('\n') || boundary.is_some() {
                    return None;
                }
                boundary = Some(offset);
            }
            offset += line.len();
        }
        if fence.is_some() && index + 1 < count {
            return None;
        }
        let following = if index + 1 < count {
            let at = boundary?;
            let span = &cursor[..at];
            let following = if span.is_empty() {
                ""
            } else {
                span.strip_suffix("\n\n")?
            };
            if !span.is_empty() && following.is_empty() {
                return None;
            }
            cursor = &cursor[at + "<!-- reply -->\n".len()..];
            following
        } else {
            if boundary.is_some() {
                return None;
            }
            cursor
        };
        items.push(ReplyItem {
            quote: quote.join("\n"),
            response: (!following.is_empty()).then(|| ReplyResponse::FollowingText {
                text: following.to_string(),
            }),
        });
    }
    Some(ReplyContext {
        version: 1,
        items,
        request: None,
        context: String::new(),
    })
}
