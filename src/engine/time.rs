//! Timestamp parsing shared by every agent parser and the metrics pass.
//!
//! One copy of the ISO-8601 → epoch-seconds conversion (was duplicated verbatim in
//! `model.rs` and `codex_model.rs`, plus a third `i64` variant in `metrics.rs`).

/// Seconds since the Unix epoch for an ISO-8601 UTC timestamp like
/// `2026-06-30T03:36:44.500Z` (we only ever use *differences*, so the absolute
/// epoch just needs to be consistent). Returns `None` if it doesn't parse.
pub fn epoch_secs(ts: &str) -> Option<f64> {
    let (date, time) = ts.split_once('T')?;
    let mut d = date.split('-');
    let y: i64 = d.next()?.parse().ok()?;
    let mo: i64 = d.next()?.parse().ok()?;
    let da: i64 = d.next()?.parse().ok()?;
    let time = time.trim_end_matches('Z');
    let mut t = time.split(':');
    let h: f64 = t.next()?.parse().ok()?;
    let mi: f64 = t.next()?.parse().ok()?;
    let s: f64 = t.next()?.parse().ok()?;
    // days_from_civil (Howard Hinnant): civil date → days since 1970-01-01.
    let yy = if mo <= 2 { y - 1 } else { y };
    let era = (if yy >= 0 { yy } else { yy - 399 }) / 400;
    let yoe = yy - era * 400;
    let mp = if mo > 2 { mo - 3 } else { mo + 9 };
    let doy = (153 * mp + 2) / 5 + da - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days as f64 * 86400.0 + h * 3600.0 + mi * 60.0 + s)
}
