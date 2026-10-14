//! Exact recognition of the two measured Claude Desktop quotation layouts.
use claude_replay_engine::seam::{ReplyContext, ReplyItem, ReplyResponse};

/// Preserve comment-marker candidates even when case, spacing, or marker wording changes.
/// This is only a non-destructive fallback guard; exact recognition remains in `parse`.
pub(crate) fn candidate(text: &str) -> bool {
    if super::model::is_skill_body(text) {
        return false;
    }
    let lowercase = text.to_ascii_lowercase();
    let mut offset = 0;
    loop {
        let cursor = &lowercase[offset..];
        let source = &text[offset..];
        let marker = ["<", "!--"]
            .into_iter()
            .filter_map(|opening| cursor.find(opening).map(|at| (at, opening)))
            .min_by_key(|(at, _)| *at);
        // Native pasted/command-output payloads can themselves contain reply examples.
        // Their contents stay under the existing literal/command normalization.
        if let Some((at, end)) = opaque_span(source) {
            if marker.is_none_or(|(marker_at, _)| at <= marker_at) {
                offset += end;
                continue;
            }
        }
        let Some((at, opening)) = marker else {
            return false;
        };
        let comment = &cursor[at + opening.len()..];
        // Treat punctuation changes in a comment opener as unsupported reply input.
        // Ordinary tags start with their name, so their prefix contains no comment punctuation.
        let prefix_end = comment
            .find(|ch: char| !ch.is_ascii_punctuation() && !ch.is_whitespace())
            .unwrap_or(comment.len());
        let (prefix, body) = comment.split_at(prefix_end);
        let comment_opener = opening.contains('-') || prefix.contains('-');
        if body.starts_with("reply")
            || comment_opener
                && (comment
                    .split_once('\n')
                    .is_some_and(|(_, rest)| rest.starts_with('>'))
                    || comment.split_once("-->").is_some_and(|(_, rest)| {
                        rest.strip_prefix('\r').unwrap_or(rest).starts_with("\n>")
                    }))
        {
            return true;
        }
        offset += at + opening.len();
    }
}

/// The earliest complete native payload whose contents are literal during reply detection.
fn opaque_span(source: &str) -> Option<(usize, usize)> {
    let pasted = source.find("<pasted_content").and_then(|at| {
        super::model::pasted_parts(&source[at..]).map(|(_, after)| (at, source.len() - after.len()))
    });
    let wrappers = [
        ("<local-command-stdout>", "</local-command-stdout>"),
        ("<local-command-caveat>", "</local-command-caveat>"),
        ("<command-message>", "</command-message>"),
        ("<command-name>", "</command-name>"),
        ("<command-args>", "</command-args>"),
        ("<task-notification>", "</task-notification>"),
    ]
    .into_iter()
    .filter_map(|(open, close)| {
        let at = source.find(open)?;
        let tail = &source[at + open.len()..];
        let end = super::model::native_tag_at(tail, close)?;
        Some((at, at + open.len() + end + close.len()))
    });
    pasted.into_iter().chain(wrappers).min_by_key(|(at, _)| *at)
}

/// Whether a fragment's marker belongs to quoted text or a literal native payload.
fn literal_position(text: &str, position: usize) -> bool {
    let line_prefix = text[..position].rsplit('\n').next().unwrap_or("");
    if line_prefix.trim_start().starts_with('>') {
        return true;
    }
    let mut offset = 0;
    while let Some((at, end)) = opaque_span(&text[offset..]) {
        if position < offset + at {
            return false;
        }
        if position < offset + end {
            return true;
        }
        offset += end;
    }
    false
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
/// Keep one ordinary turn in source order, leaving image/tool records on their existing paths.
/// Reply candidates retain raw text; complete native pasted wrappers retain their normalization.
/// Do not recognize a later fragment or assemble metadata from fragmented markers.
pub(crate) fn message_fallback(items: &[serde_json::Value]) -> Option<String> {
    let texts: Vec<&str> = items
        .iter()
        .filter(|item| item.get("type").and_then(serde_json::Value::as_str) == Some("text"))
        .filter_map(|item| item.get("text").and_then(serde_json::Value::as_str))
        .collect();
    if texts.len() < 2 {
        return None;
    }
    let joined = texts.concat();
    // A whole Skill body stays on its existing reference path. Reply candidates own
    // their complete message, even if a selected literal spans a Skill-looking fragment.
    if super::model::is_skill_body(&joined) {
        return None;
    }
    let mut text_offset = 0;
    for text in &texts {
        let marker = text_offset + text.len() - text.trim_start().len();
        if super::model::is_skill_body(text) && !literal_position(&joined, marker) {
            return None;
        }
        text_offset += text.len();
    }
    if candidate(&joined) {
        return Some(joined);
    }
    match super::model::fence_pasted(&joined) {
        std::borrow::Cow::Owned(pasted) => Some(pasted),
        std::borrow::Cow::Borrowed(_) if texts.iter().any(|text| candidate(text)) => Some(joined),
        _ => None,
    }
}
