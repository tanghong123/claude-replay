//! Whether the fields we DEPEND on are still being written (#363).
//!
//! [`crate::unknown`] catches a NEW shape. It cannot catch a known field going empty after a
//! client update: a usage block that stops arriving, a model name that moves, a `cwd` that is
//! no longer written. Nothing fails when that happens — the cost silently becomes a lower bound,
//! a card silently loses its line — so this counts, for each adapter and each client version,
//! how often the records that should carry a field do, and flags a field the newest version
//! writes clearly less often than the versions before it did.
//!
//! The fields are DECLARED per adapter ([`crate::adapter::TranscriptAdapter::coverage_fields`]):
//! a short list of what the rendering and the cost read, not every key a record has. It never
//! holds content — field names, versions and counts.

use std::collections::BTreeMap;

use serde_json::Value;

/// One field an adapter's rendering or cost depends on.
pub struct CoverageField {
    /// How the report names it: `<record>.<path>` (`assistant.message.usage`).
    pub name: &'static str,
    /// Which records are expected to carry it.
    pub applies: fn(&Value) -> bool,
    /// Whether this record does.
    pub present: fn(&Value) -> bool,
}

/// A version needs this many records asking for a field before its rate means anything.
pub const MIN_RECORDS: u64 = 20;

/// How far below the earlier versions' best rate the newest may fall before it is flagged:
/// twenty points. A field that is optional by nature (the first token count of a Codex session
/// carries no usage yet) sits at a steady rate below one, and only a MOVE is news.
pub const DROP: f64 = 0.2;

/// One row of the report.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub agent: &'static str,
    /// The client version that wrote the records, or `?` when they did not say.
    pub version: String,
    pub field: &'static str,
    /// Records the field was asked of.
    pub records: u64,
    /// …and of those, how many carried it.
    pub present: u64,
    /// The best rate among the EARLIER versions with enough records, when there are any.
    pub usual: Option<f64>,
    /// The newest version with enough records writes it clearly less often than `usual`.
    pub dropped: bool,
}

impl Row {
    pub fn rate(&self) -> f64 {
        if self.records == 0 {
            0.0
        } else {
            self.present as f64 / self.records as f64
        }
    }
}

/// One version's counts for a field: the version, the records asked, the records that carried it.
type VersionCounts = (String, u64, u64);

/// Counts accumulated over any number of transcripts.
#[derive(Default)]
pub struct Tally {
    counts: BTreeMap<(&'static str, String, &'static str), (u64, u64)>,
}

impl Tally {
    /// Count one record of `agent`, written by `version`, against `fields`.
    pub fn record(
        &mut self,
        agent: &'static str,
        version: Option<&str>,
        fields: &[CoverageField],
        v: &Value,
    ) {
        for f in fields {
            if !(f.applies)(v) {
                continue;
            }
            let e = self
                .counts
                .entry((agent, version.unwrap_or("?").to_string(), f.name))
                .or_default();
            e.0 += 1;
            if (f.present)(v) {
                e.1 += 1;
            }
        }
    }

    /// The report: one row per agent, version and field, newest version first within a field,
    /// with the newest well-counted version of each field flagged when it fell [`DROP`] below
    /// the best of the versions before it.
    pub fn rows(&self) -> Vec<Row> {
        let mut by_field: BTreeMap<(&'static str, &'static str), Vec<VersionCounts>> =
            BTreeMap::new();
        for ((agent, version, field), (records, present)) in &self.counts {
            by_field
                .entry((agent, field))
                .or_default()
                .push((version.clone(), *records, *present));
        }
        let mut out = Vec::new();
        for ((agent, field), mut versions) in by_field {
            versions.sort_by(|a, b| version_order(&b.0, &a.0));
            // The newest version with enough records is the one judged; the rest are its past.
            let judged = versions.iter().position(|(_, n, _)| *n >= MIN_RECORDS);
            let usual = judged.and_then(|j| {
                versions[j + 1..]
                    .iter()
                    .filter(|(_, n, _)| *n >= MIN_RECORDS)
                    .map(|(_, n, p)| *p as f64 / *n as f64)
                    .reduce(f64::max)
            });
            for (i, (version, records, present)) in versions.into_iter().enumerate() {
                let rate = present as f64 / records.max(1) as f64;
                let is_judged = Some(i) == judged;
                out.push(Row {
                    agent,
                    version,
                    field,
                    records,
                    present,
                    usual: if is_judged { usual } else { None },
                    dropped: is_judged && usual.is_some_and(|u| u - rate > DROP),
                });
            }
        }
        out
    }
}

/// Newest last: dotted numbers compared as numbers (`2.1.270` < `2.1.1000`), anything that is
/// not a number compared as text after them, `?` oldest of all.
fn version_order(a: &str, b: &str) -> std::cmp::Ordering {
    if a == "?" || b == "?" {
        return (a != "?").cmp(&(b != "?"));
    }
    let parts = |s: &str| -> Vec<Result<u64, String>> {
        s.split(['.', '-', '+'])
            .map(|p| p.parse::<u64>().map_err(|_| p.to_string()))
            .collect()
    };
    let (pa, pb) = (parts(a), parts(b));
    for (x, y) in pa.iter().zip(pb.iter()) {
        let o = match (x, y) {
            (Ok(x), Ok(y)) => x.cmp(y),
            (Ok(_), Err(_)) => std::cmp::Ordering::Greater,
            (Err(_), Ok(_)) => std::cmp::Ordering::Less,
            (Err(x), Err(y)) => x.cmp(y),
        };
        if o != std::cmp::Ordering::Equal {
            return o;
        }
    }
    pa.len().cmp(&pb.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn usage() -> [CoverageField; 1] {
        [CoverageField {
            name: "assistant.message.usage",
            applies: |v| v.get("type").and_then(Value::as_str) == Some("assistant"),
            present: |v| v.pointer("/message/usage").is_some(),
        }]
    }

    fn feed(t: &mut Tally, version: &str, with: u64, without: u64) {
        let fields = usage();
        for _ in 0..with {
            t.record(
                "claude",
                Some(version),
                &fields,
                &json!({"type": "assistant", "message": {"usage": {}}}),
            );
        }
        for _ in 0..without {
            t.record(
                "claude",
                Some(version),
                &fields,
                &json!({"type": "assistant", "message": {}}),
            );
        }
    }

    #[test]
    fn a_field_the_newest_version_stopped_writing_is_flagged() {
        let mut t = Tally::default();
        feed(&mut t, "2.1.270", 100, 0);
        feed(&mut t, "2.1.300", 98, 2);
        feed(&mut t, "2.1.1000", 10, 90);
        let rows = t.rows();
        let newest = rows.iter().find(|r| r.version == "2.1.1000").unwrap();
        assert!(
            newest.dropped && newest.usual == Some(1.0),
            "2.1.1000 is the newest (numbers, not text) and writes usage on 10% where 2.1.270 did \
             on all: {rows:?}"
        );
        assert!(
            rows.iter().filter(|r| r.dropped).count() == 1,
            "only it: {rows:?}"
        );
        // A user record is never asked for an assistant's usage.
        t.record(
            "claude",
            Some("2.1.1000"),
            &usage(),
            &json!({"type": "user"}),
        );
        assert_eq!(t.rows().iter().map(|r| r.records).sum::<u64>(), 300);
    }

    #[test]
    fn a_steady_rate_below_one_and_a_thin_version_are_not_news() {
        let mut t = Tally::default();
        feed(&mut t, "0.40.0", 60, 40);
        feed(&mut t, "0.41.0", 58, 42);
        feed(&mut t, "0.42.0", 1, 9);
        let rows = t.rows();
        assert!(
            !rows.iter().any(|r| r.dropped),
            "60% then 58% is a field optional by nature; ten records of 0.42.0 judge nothing: \
             {rows:?}"
        );
        let judged = rows.iter().find(|r| r.usual.is_some()).unwrap();
        assert_eq!(
            judged.version, "0.41.0",
            "the newest version with enough records is judged"
        );
    }

    #[test]
    fn versions_order_as_numbers() {
        let mut v = ["2.1.1000", "?", "2.1.270", "2.1.99", "1.1.26"];
        v.sort_by(|a, b| version_order(a, b));
        assert_eq!(v, ["?", "1.1.26", "2.1.99", "2.1.270", "2.1.1000"]);
    }
}
