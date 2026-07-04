//! Session metrics parsed from the transcript: token totals, wall-clock
//! duration, model, and a best-effort USD cost estimate.

use serde_json::Value;

#[derive(Debug, Default, PartialEq)]
pub struct Metrics {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub model: String,
    pub duration_secs: i64,
    pub cost_usd: Option<f64>,
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Parse an RFC3339-ish timestamp ("2026-06-28T13:54:10.106Z") to unix seconds.
fn parse_ts(s: &str) -> Option<i64> {
    if s.len() < 19 {
        return None;
    }
    let y: i64 = s.get(0..4)?.parse().ok()?;
    let mo: i64 = s.get(5..7)?.parse().ok()?;
    let d: i64 = s.get(8..10)?.parse().ok()?;
    let h: i64 = s.get(11..13)?.parse().ok()?;
    let mi: i64 = s.get(14..16)?.parse().ok()?;
    let se: i64 = s.get(17..19)?.parse().ok()?;
    Some(days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + se)
}

/// Rough USD/1M-token (input, output) prices for cost estimation.
fn price(model: &str) -> Option<(f64, f64)> {
    let m = model.to_lowercase();
    if m.contains("opus") {
        Some((15.0, 75.0))
    } else if m.contains("sonnet") {
        Some((3.0, 15.0))
    } else if m.contains("haiku") {
        Some((1.0, 5.0))
    } else {
        None
    }
}

/// Stream the metrics pass straight from a reader, so a large transcript never
/// has to be fully resident as a `String` (one line at a time).
pub fn parse_reader<R: std::io::BufRead>(reader: R) -> Metrics {
    parse_from_lines(reader.lines().map_while(Result::ok))
}

fn parse_from_lines(lines: impl Iterator<Item = String>) -> Metrics {
    let mut input = 0u64;
    let mut output = 0u64;
    let mut model = String::new();
    let mut tmin: Option<i64> = None;
    let mut tmax: Option<i64> = None;

    for line in lines {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(u) = v.pointer("/message/usage") {
            // Only genuinely-new tokens: cache_read re-counts the whole context
            // every turn, so summing it across a session wildly over-counts.
            input += u.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
            output += u.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
        }
        if let Some(m) = v.pointer("/message/model").and_then(|x| x.as_str()) {
            model = m.to_string();
        }
        if let Some(ts) = v.get("timestamp").and_then(|x| x.as_str()) {
            if let Some(secs) = parse_ts(ts) {
                tmin = Some(tmin.map_or(secs, |a| a.min(secs)));
                tmax = Some(tmax.map_or(secs, |a| a.max(secs)));
            }
        }
    }
    let duration_secs = match (tmin, tmax) {
        (Some(a), Some(b)) => (b - a).max(0),
        _ => 0,
    };
    let cost_usd = price(&model).map(|(pi, po)| input as f64 / 1e6 * pi + output as f64 / 1e6 * po);
    Metrics {
        input_tokens: input,
        output_tokens: output,
        model,
        duration_secs,
        cost_usd,
    }
}

fn human_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

fn human_dur(secs: i64) -> String {
    if secs <= 0 {
        return "—".into();
    }
    let (h, m) = (secs / 3600, (secs % 3600) / 60);
    if h > 0 {
        format!("{h}h{m}m")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{secs}s")
    }
}

/// "claude-opus-4-8" -> "opus4.8".
fn short_model(model: &str) -> String {
    let m = model.strip_prefix("claude-").unwrap_or(model);
    let mut parts = m.split('-');
    let name = parts.next().unwrap_or(m);
    let ver: Vec<&str> = parts
        .filter(|p| p.chars().all(|c| c.is_ascii_digit()))
        .collect();
    if ver.is_empty() {
        name.to_string()
    } else {
        format!("{name}{}", ver.join("."))
    }
}

impl Metrics {
    /// Compact one-line footer text.
    pub fn footer(&self) -> String {
        let model = if self.model.is_empty() {
            String::new()
        } else {
            format!("{} · ", short_model(&self.model))
        };
        let cost = self
            .cost_usd
            .map(|c| format!(" · ~${c:.2}"))
            .unwrap_or_default();
        format!(
            "{model}{} in / {} out · {}{cost}",
            human_tokens(self.input_tokens),
            human_tokens(self.output_tokens),
            human_dur(self.duration_secs),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tokens_model_duration_cost() {
        let jsonl = r#"
{"type":"assistant","timestamp":"2026-06-28T10:00:00.000Z","message":{"model":"claude-opus-4-8","usage":{"input_tokens":1000,"output_tokens":500}}}
{"type":"assistant","timestamp":"2026-06-28T10:02:00.000Z","message":{"model":"claude-opus-4-8","usage":{"input_tokens":2000,"output_tokens":1500}}}
"#;
        let m = parse_reader(std::io::Cursor::new(jsonl));
        assert_eq!(m.input_tokens, 3000);
        assert_eq!(m.output_tokens, 2000);
        assert_eq!(m.duration_secs, 120);
        assert!(m.cost_usd.unwrap() > 0.0);
        let f = m.footer();
        assert!(f.contains("opus4.8"), "footer: {f}");
        assert!(f.contains("2m"), "footer: {f}");
    }

    #[test]
    fn short_model_formats() {
        assert_eq!(short_model("claude-opus-4-8"), "opus4.8");
        assert_eq!(short_model("claude-sonnet-4-6"), "sonnet4.6");
    }
}
