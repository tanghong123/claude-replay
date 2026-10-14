//! Exact recognition of the two measured Claude Desktop quotation layouts.
use claude_replay_engine::seam::{ReplyContext, ReplyItem, ReplyResponse};

/// Preserve comment-marker candidates even when case, spacing, or marker wording changes.
/// This is only a non-destructive fallback guard; exact recognition remains in `parse`.
pub(crate) fn candidate(text: &str) -> bool {
    let trimmed = text.trim_start();
    let marker_with_selection = trimmed.starts_with("<!--")
        && trimmed
            .split_once('\n')
            .is_some_and(|(_, rest)| rest.starts_with('>'));
    let lowercase = text.to_ascii_lowercase();
    let reply_comment = lowercase
        .split("<!--")
        .skip(1)
        .any(|comment| comment.trim_start().starts_with("reply"));
    marker_with_selection || reply_comment
}

/// Split only unfenced standalone markers. A marker inside selected text or a
/// fenced code block is content; an unsupported or ambiguous outside marker fails.
pub(crate) fn parse(text: &str) -> Option<ReplyContext> {
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
            } else if fence.is_none()
                && bare.to_ascii_lowercase().starts_with("<!--")
                && bare.to_ascii_lowercase().contains("reply")
            {
                if bare != "<!-- reply -->" || !line.ends_with('\n') || boundary.is_some() {
                    return None;
                }
                boundary = Some(offset);
            }
            offset += line.len();
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

/// Several native text fragments lack a measured, lossless quotation partition.
/// Keep one ordinary turn with every input character in source order, and leave
/// image/tool records on their existing paths. Do not recognize a later fragment.
pub(crate) fn message_fallback(items: &[serde_json::Value]) -> Option<String> {
    let texts: Vec<&str> = items
        .iter()
        .filter(|item| item.get("type").and_then(serde_json::Value::as_str) == Some("text"))
        .filter_map(|item| item.get("text").and_then(serde_json::Value::as_str))
        .collect();
    (texts.len() > 1 && texts.iter().any(|text| candidate(text))).then(|| texts.concat())
}
