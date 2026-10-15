//! Exact recognition of the observed Codex Desktop quotation envelope.
use claude_replay_engine::seam::{ReplyContext, ReplyItem, ReplyResponse};
use serde::Deserialize;

/// Literal client preamble; no trimming or newline normalization is permitted.
pub(crate) const PREFIX: &str = "\n# Response annotations:\nEach item contains text selected from an earlier Codex response and may include a user comment. Treat items as Annotation 1, Annotation 2, and so on in array order. Use every selection as context and address every comment. For every annotation you address, include its inline directive `:codex-annotation{index=\"N\"}`, where N is its one-based array position (for example, `:codex-annotation{index=\"1\"}`). Do not use unstructured annotation labels.\n<response-annotations>\n";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    text: String,
    annotation: Option<String>,
    source: Option<Source>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Source {
    message_id: String,
    start_offset: f64,
    end_offset: f64,
}

/// Returns metadata only if the entire JSON/envelope matches the native spelling.
/// The input itself remains the canonical fallback body and raw source.
pub(crate) fn parse(text: &str) -> Option<ReplyContext> {
    let rest = text.strip_prefix(PREFIX)?;
    let (array, tail) = rest.split_once("\n</response-annotations>")?;
    if array.contains(['\n', '\r']) || (!tail.is_empty() && !tail.starts_with('\n')) {
        return None;
    }
    let selections: Vec<Selection> = serde_json::from_str(array).ok()?;
    if selections.is_empty() {
        return None;
    }
    // Rebuild with the client's key order and ECMAScript number spelling. Serde's
    // object-map order / float spelling are not JSON.stringify's byte contract.
    let string = |s: &str| serde_json::to_string(s).expect("string serialization");
    let mut canonical = String::from("[");
    let mut items = Vec::with_capacity(selections.len());
    for selection in selections {
        if selection.text.is_empty() || selection.annotation.as_ref().is_some_and(String::is_empty)
        {
            return None;
        }
        if !items.is_empty() {
            canonical.push(',');
        }
        canonical.push_str(&format!("{{\"text\":{}", string(&selection.text)));
        if let Some(annotation) = &selection.annotation {
            canonical.push_str(&format!(",\"annotation\":{}", string(annotation)));
        }
        if let Some(source) = selection.source {
            let start = source.start_offset;
            let end = source.end_offset;
            if source.message_id.is_empty()
                || !start.is_finite()
                || !end.is_finite()
                || start < 0.0
                || start.fract() != 0.0
                || end.fract() != 0.0
                || end <= start
            {
                return None;
            }
            let mut buffer = ryu_js::Buffer::new();
            let start = buffer.format(start).to_string();
            let end = buffer.format(end);
            canonical.push_str(&format!(
                ",\"source\":{{\"messageId\":{},\"startOffset\":{},\"endOffset\":{}}}",
                string(&source.message_id),
                start,
                end
            ));
        }
        canonical.push('}');
        items.push(ReplyItem {
            quote: selection.text,
            response: selection
                .annotation
                .map(|text| ReplyResponse::Comment { text }),
        });
    }
    canonical.push(']');
    if canonical != array {
        return None;
    }
    // A request boundary is explicit only outside fenced content and when unique.
    // Otherwise preserve the complete tail as context without guessing its scope.
    let boundary = "\n## My request:\n";
    let (context, request) = match tail.split_once(boundary) {
        Some((before, request))
            if !before.contains("```")
                && !before.contains("~~~")
                && !request.contains(boundary)
                && !request.is_empty() =>
        {
            (before.to_string(), Some(request.to_string()))
        }
        _ => (tail.to_string(), None),
    };
    Some(ReplyContext {
        version: 1,
        items,
        request,
        context,
    })
}

/// A preservation guard, never a relaxed recognizer.
pub(crate) fn candidate(text: &str) -> bool {
    text.to_ascii_lowercase().contains("<response-annotations")
        || text.to_ascii_lowercase().contains("# response annotations")
}

/// Several native text fragments lack a measured, lossless quotation partition.
/// Keep one ordinary turn with every input character in source order, and leave
/// image/tool records on their existing paths. Do not recognize a later fragment.
pub(crate) fn message_fallback(items: &[serde_json::Value]) -> Option<String> {
    let texts: Vec<&str> = items
        .iter()
        .filter(|item| item.get("type").and_then(serde_json::Value::as_str) == Some("input_text"))
        .filter_map(|item| item.get("text").and_then(serde_json::Value::as_str))
        .collect();
    if texts.len() < 2 {
        return None;
    }
    let joined = texts.concat();
    (candidate(&joined) || texts.iter().any(|text| candidate(text))).then_some(joined)
}

/// Only an opening native package is a drift candidate; later mentions stay literal.
pub(crate) fn rejected(parts: &[&str]) -> bool {
    let joined;
    let text = match parts {
        [] => return false,
        [text] => *text,
        _ => {
            joined = parts.concat();
            &joined
        }
    };
    let source = text.trim_start_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
    ["# Response annotations", "<response-annotations"]
        .iter()
        .any(|prefix| {
            source
                .get(..prefix.len())
                .is_some_and(|s| s.eq_ignore_ascii_case(prefix))
        })
        && (parts.len() != 1 || parse(text).is_none())
}
