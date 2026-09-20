//! What an adapter dropped because it did not KNOW about it (#264).
//!
//! Most of what an adapter skips it skips on purpose. One session carries 1,515
//! `total_tokens_reminder` attachments, 140 `task_reminder`s and 120 `queued_command`s, and
//! none of them is a record. A log that cannot tell those from something new is noise, and
//! noise is what nobody reads — so an adapter says nothing here about a shape it recognises
//! and ignores, and everything it does say is, by construction, something it has never seen.
//!
//! The case this exists for is #263. Claude Code began recording `toolUseResult.bashEditDiff`
//! on 2026-09-13: every Bash command that edits a file carries a real unified diff, and no
//! line of this codebase read the field. Nothing failed, no test went red, and by the time the
//! owner noticed from a screenshot, 975 records across thirteen sessions were carrying a diff
//! the page had dropped in silence. The upstream format moves and we find out by eye, weeks
//! later. This is the instrument that would have said so on day one.
//!
//! **It costs nothing when there is nothing new.** A recognised shape never reaches this
//! module — the adapter's own `match` answers it — so the only work on the hot path is the
//! match that was already there. An unknown shape takes a lock once and is then counted; by
//! definition there are few of them, and a shape that repeats a million times is one map
//! bump per occurrence.
//!
//! **It never holds content.** A report is a kind, a name, a count, the client version that
//! wrote it and where one example was — enough to go and look, and safe to paste into an
//! issue. The value itself stays in the transcript.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

/// Which part of a transcript an unknown shape turned up in. Closed, because a new category
/// is a decision about what the adapters watch, not something to add in passing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Where {
    /// A top-level record `type` no arm matched.
    RecordType,
    /// A `system` record's `subtype`.
    SystemSubtype,
    /// An `attachment.type`.
    AttachmentType,
    /// A `message.content[].type`.
    ContentType,
    /// A key inside `toolUseResult` that no code reads. This is the one that catches the next
    /// `bashEditDiff`: the others are new NAMES in places we already look, and this is a new
    /// place to look inside something we already parse.
    ToolResultKey,
}

impl Where {
    /// The word used in the printed table and in the report's key.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RecordType => "record.type",
            Self::SystemSubtype => "system.subtype",
            Self::AttachmentType => "attachment.type",
            Self::ContentType => "content.type",
            Self::ToolResultKey => "toolUseResult.key",
        }
    }
}

/// One shape, and what is known about it. Counts and identifiers only — never a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    /// Which agent family met it.
    pub agent: &'static str,
    /// Where in the transcript.
    pub at: Where,
    /// The unrecognised name itself (`bashEditDiff`, `some_new_record`, …).
    pub name: String,
    /// How many times it has been met since this process started.
    pub count: u64,
    /// The client version that wrote the first one, when the record said.
    pub version: Option<String>,
    /// Where one of them is: the session id and, when the caller knows it, the line.
    pub example: Option<String>,
}

type Registry = BTreeMap<(&'static str, Where, String), Seen>;

fn registry() -> &'static Mutex<Registry> {
    static REG: OnceLock<Mutex<Registry>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Report a shape the adapter did not recognise.
///
/// `version` is the `version` field of the record that carried it (the client that wrote the
/// transcript) and `example` is a locator — `"<session id>:<line>"` is the useful shape. Both
/// are recorded from the FIRST sighting only: the point is to be able to go and look, and the
/// first one is as good as any.
pub fn note(
    agent: &'static str,
    at: Where,
    name: &str,
    version: Option<&str>,
    example: Option<&str>,
) {
    // A poisoned lock must not take the parse down with it: this is an observation, and an
    // observation that fails is worth less than the parse it would abort.
    let Ok(mut reg) = registry().lock() else {
        return;
    };
    reg.entry((agent, at, name.to_string()))
        .and_modify(|s| s.count += 1)
        .or_insert_with(|| Seen {
            agent,
            at,
            name: name.to_string(),
            count: 1,
            version: version.map(str::to_string),
            example: example.map(str::to_string),
        });
}

/// Everything met so far, most frequent first — the answer to "has the format moved?".
pub fn snapshot() -> Vec<Seen> {
    let Ok(reg) = registry().lock() else {
        return Vec::new();
    };
    let mut out: Vec<Seen> = reg.values().cloned().collect();
    out.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
    out
}

/// Forget everything. For tests, which share a process and would otherwise see each other's
/// reports.
pub fn reset() {
    if let Ok(mut reg) = registry().lock() {
        reg.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shape_is_counted_once_per_sighting_and_described_from_the_first() {
        reset();
        note(
            "claude",
            Where::ToolResultKey,
            "bashEditDiff",
            Some("2.1.270"),
            Some("abc:12"),
        );
        note(
            "claude",
            Where::ToolResultKey,
            "bashEditDiff",
            Some("2.1.999"),
            Some("zzz:99"),
        );
        note("claude", Where::RecordType, "brand-new", None, None);
        let seen = snapshot();
        assert_eq!(seen.len(), 2, "two distinct shapes: {seen:?}");
        assert_eq!(
            seen[0].name, "bashEditDiff",
            "most frequent first: {seen:?}"
        );
        assert_eq!(seen[0].count, 2);
        assert_eq!(
            (seen[0].version.as_deref(), seen[0].example.as_deref()),
            (Some("2.1.270"), Some("abc:12")),
            "the FIRST sighting describes it — a locator only has to be good enough to go and \
             look at: {seen:?}"
        );
        reset();
        assert!(snapshot().is_empty());
    }
}
