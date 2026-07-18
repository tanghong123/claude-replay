use crate::metrics::{parse_ts, Metrics};
use serde_json::Value;
use std::io::BufRead;

pub(crate) fn parse_codex_reader<R: BufRead>(reader: R) -> Metrics {
    let mut input = 0;
    let mut cached = 0;
    let mut output = 0;
    let mut model = String::new();
    let mut first = None;
    let mut last = None;

    for line in reader.lines().map_while(|line| line.ok()) {
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(timestamp) = value
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_ts)
        {
            first = Some(first.map_or(timestamp, |seen: i64| seen.min(timestamp)));
            last = Some(last.map_or(timestamp, |seen: i64| seen.max(timestamp)));
        }
        if value.get("type").and_then(Value::as_str) == Some("turn_context") {
            if let Some(next) = value.pointer("/payload/model").and_then(Value::as_str) {
                model = next.to_string();
            }
        }
        if value.get("type").and_then(Value::as_str) == Some("event_msg")
            && value.pointer("/payload/type").and_then(Value::as_str) == Some("token_count")
        {
            let Some(total) = value.pointer("/payload/info/total_token_usage") else {
                continue;
            };
            let field = |name: &str| total.get(name).and_then(Value::as_u64).unwrap_or(0);
            let total_input = field("input_tokens");
            cached = field("cached_input_tokens");
            input = total_input.saturating_sub(cached);
            output = field("output_tokens");
        }
    }

    Metrics {
        input_tokens: input,
        cache_creation_tokens: 0,
        cache_read_tokens: cached,
        output_tokens: output,
        model,
        duration_secs: match (first, last) {
            (Some(start), Some(end)) => (end - start).max(0),
            _ => 0,
        },
        cost_usd: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_newest_cumulative_usage_and_keeps_cached_input_separate() {
        let jsonl = r#"
{"timestamp":"2026-07-18T01:00:00Z","type":"turn_context","payload":{"model":"gpt-5.6"}}
{"timestamp":"2026-07-18T01:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":50,"output_tokens":20}}}}
{"timestamp":"2026-07-18T01:01:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":300,"cached_input_tokens":200,"output_tokens":80}}}}
"#;
        let metrics = parse_codex_reader(std::io::Cursor::new(jsonl));
        assert_eq!(metrics.input_tokens, 100);
        assert_eq!(metrics.cache_read_tokens, 200);
        assert_eq!(metrics.output_tokens, 80);
        assert_eq!(metrics.model, "gpt-5.6");
        assert_eq!(metrics.duration_secs, 60);
        assert_eq!(metrics.cost_usd, None);
        let footer = metrics.footer();
        assert!(footer.contains("gpt-5.6"), "footer: {footer}");
        assert!(footer.contains("100 in"), "footer: {footer}");
        assert!(footer.contains("200 cached"), "footer: {footer}");
    }
}
