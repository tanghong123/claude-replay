//! Session metrics parsed from the transcript: token totals, wall-clock
//! duration, model, and a best-effort USD cost estimate.

use crate::Agent;

#[derive(Debug, Default, PartialEq, Clone)]
pub struct Metrics {
    /// Genuinely-new input tokens (excludes cached content — see the two cache
    /// fields below). Small on cache-heavy sessions.
    pub input_tokens: u64,
    /// Tokens written to the prompt cache the first time content is seen.
    pub cache_creation_tokens: u64,
    /// Cached tokens re-read on later turns. This dominates a long session (the
    /// whole context is re-read every turn), so it's tallied separately from
    /// `input_tokens` rather than lumped in.
    pub cache_read_tokens: u64,
    pub output_tokens: u64,
    pub model: String,
    pub duration_secs: i64,
    pub cost_usd: Option<f64>,
}

/// Parse an RFC3339-ish timestamp ("2026-06-28T13:54:10.106Z") to unix seconds
/// (integer — sub-second precision is dropped, matching the old byte-offset parser).
/// Shares the one epoch-seconds converter with the parse layer (`engine::time`).
pub(crate) fn parse_ts(s: &str) -> Option<i64> {
    crate::engine::time::epoch_secs(s).map(|secs| secs as i64)
}

/// Rough USD/1M-token (input, output) list prices for cost estimation.
/// Best-effort — rates are approximate and drift over time.
fn price(model: &str) -> Option<(f64, f64)> {
    let m = model.to_lowercase();
    if m.contains("opus") {
        Some((15.0, 75.0))
    } else if m.contains("sonnet") {
        Some((3.0, 15.0))
    } else if m.contains("haiku") {
        Some((1.0, 5.0))
    } else if m.contains("codex") || m.contains("gpt-5") || m.contains("gpt5") {
        // OpenAI GPT-5 family (Codex uses these), best-effort list price.
        Some((1.25, 10.0))
    } else {
        None
    }
}

/// Best-effort USD cost from a model name and its token tiers. Cache writes bill
/// at ~1.25× base input, cache reads at ~0.1× (prompt-caching discount). Returns
/// `None` when the model isn't in the price table.
pub(crate) fn estimate_cost(
    model: &str,
    input: u64,
    cache_creation: u64,
    cache_read: u64,
    output: u64,
) -> Option<f64> {
    price(model).map(|(pi, po)| {
        (input as f64 + cache_creation as f64 * 1.25 + cache_read as f64 * 0.10) / 1e6 * pi
            + output as f64 / 1e6 * po
    })
}

/// Metrics via the reader for `agent` — dispatches to each agent's adapter
/// (`claude_metrics` / `codex_metrics`); the returned [`Metrics`] shape is shared.
pub(crate) fn parse_reader_for<R: std::io::BufRead>(agent: Agent, mut reader: R) -> Metrics {
    crate::adapter::adapter(agent).parse_reader(&mut reader)
}

pub(crate) fn human_tokens(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1e9)
    } else if n >= 1_000_000 {
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

/// "claude-opus-4-8" -> "opus4.8". Non-Claude models (e.g. Codex "gpt-5.6") are
/// shown verbatim.
fn short_model(model: &str) -> String {
    if !model.starts_with("claude-") {
        return model.to_string();
    }
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
    /// Footer metric parts as `(text, shed_priority)` — the viewer's fit-and-shed drops
    /// the highest priority first when the footer can't fit its width. Order matches the
    /// spec: cached(1) → model(3) → in(4) → out(5) → duration(6) → cost(7). (`%`, at
    /// priority 2, is scroll-derived and added by the view.)
    pub fn footer_segments(&self) -> Vec<(String, u8)> {
        let mut segs = Vec::new();
        let cached = self.cache_creation_tokens + self.cache_read_tokens;
        if cached > 0 {
            segs.push((format!("{} cached", human_tokens(cached)), 1));
        }
        if !self.model.is_empty() {
            segs.push((short_model(&self.model).to_string(), 3));
        }
        segs.push((format!("{} in", human_tokens(self.input_tokens)), 4));
        segs.push((format!("{} out", human_tokens(self.output_tokens)), 5));
        if self.duration_secs > 0 {
            segs.push((human_dur(self.duration_secs), 6));
        }
        if let Some(c) = self.cost_usd {
            segs.push((format!("~${c:.2}"), 7));
        }
        segs
    }

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
        // Show the cache tier only when there is one — cache-less transcripts keep
        // the plain "in / out" shape.
        let cached = self.cache_creation_tokens + self.cache_read_tokens;
        let cached = if cached > 0 {
            format!("{} cached · ", human_tokens(cached))
        } else {
            String::new()
        };
        format!(
            "{model}{} in · {cached}{} out · {}{cost}",
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
    fn short_model_formats() {
        assert_eq!(short_model("claude-opus-4-8"), "opus4.8");
        assert_eq!(short_model("claude-sonnet-4-6"), "sonnet4.6");
    }
}
