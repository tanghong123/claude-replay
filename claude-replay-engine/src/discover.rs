//! The **discovery vocabulary** — the agent-free half of locating sessions (#87 step 3):
//! the shared [`Candidate`] type, the cwd-ancestor scoping helpers, and the format-neutral
//! transcript-head readers `session_cwd`/`session_id`. The REGISTRY half — `detect_agent`,
//! `resolve_any`, `candidates_all`, the per-adapter dispatch — lives in the facade crate
//! (`claude-replay-core`), which wires the agents in; adapters build on THIS half through
//! the seam.

use crate::Agent;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// What an agent calls a session, read from wherever that agent keeps it.
///
/// **Discovery-side, never the fold.** Producing this may open a file or query an agent's own
/// store, so it belongs with `load_tasks`/`candidates_scoped` and not in the sans-io
/// accumulator — a title is a label someone chose, revisable at any time, not something the
/// transcript *did*.
///
/// Both fields are optional because agents differ: Claude records a user-set title, an
/// agent-generated one and the most recent prompt; Codex records none of them. A consumer falls
/// back to [`Candidate::snippet`] (the FIRST prompt), which always exists.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionCard {
    /// A name for the session — the user's own if they set one, else the agent's.
    pub title: Option<String>,
    /// The most recent prompt: what the session is doing *now*, as opposed to what it opened
    /// with. Worth showing beside a title, not just as a fallback for one.
    pub last_prompt: Option<String>,
}

/// **Opaque, adapter-owned memoization state** — whatever an adapter needs to answer faster next
/// time: a byte offset it already scanned to, a row version, a resolved id.
///
/// The caller stores it beside the card and hands it back on the next call for the same path. It
/// never looks inside, and it must be prepared to lose it.
///
/// **A memo is always optional and always discardable.** An adapter must treat a missing,
/// unreadable, foreign, or stale-format memo exactly as `None` and fall back to its cold path —
/// never an error, and never trusted unverified. An adapter whose format changes stamps a version
/// inside its own JSON and ignores anything it does not recognise; nothing here polices that,
/// because nothing here can.
///
/// Opaque JSON is right *here* even though #96 rejected it for the meta record, and the reason is
/// in that rejection: opacity suits a **trait** seam, whose trait cannot name every impl's state,
/// and not a **file format**, whose readers depend on it. This is the former — and the product is
/// a cache its owner may throw away, which is what makes the discard rule safe.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CardMemo(serde_json::Value);

impl CardMemo {
    /// Wrap an adapter's own state. Only the adapter that wrote it should read it back.
    pub fn new(v: serde_json::Value) -> Self {
        Self(v)
    }
    /// Read it back — for the adapter that wrote it. A caller has no use for this.
    pub fn value(&self) -> &serde_json::Value {
        &self.0
    }
    /// Decode into the adapter's own type, or `None` when it is missing/foreign/stale. The
    /// shape every adapter wants at the top of `session_card`.
    pub fn decode<T: serde::de::DeserializeOwned>(memo: Option<&Self>) -> Option<T> {
        serde_json::from_value(memo?.0.clone()).ok()
    }
    /// Encode the adapter's own state.
    pub fn encode<T: serde::Serialize>(v: &T) -> Option<Self> {
        serde_json::to_value(v).ok().map(Self)
    }
}

/// What a `session_card` call answers. Three cases, because a caller cannot tell them apart
/// otherwise — and confusing two of them is visible: "keep the card you have" reported as "no
/// card" makes a title vanish on the next refresh, while the reverse makes a deleted one linger.
#[derive(Clone, Debug, PartialEq)]
pub enum CardOutcome {
    /// Nothing this adapter depends on has changed — **keep the card you already have.**
    ///
    /// The memo is **required**, not optional: an adapter's cursor can advance even when its
    /// answer does not (Claude's scan offset moves with every append), so a caller that had the
    /// option of dropping it would silently restart from a stale position on every call and
    /// quietly undo the memoization.
    Unchanged { memo: CardMemo },
    /// A card — the first, or a changed one — plus the memo for next time. `None` from an
    /// adapter with nothing worth remembering.
    Fresh {
        card: SessionCard,
        memo: Option<CardMemo>,
    },
    /// This agent names nothing here — **drop any card and memo you cached.**
    Absent,
}

impl CardOutcome {
    /// The card this outcome carries, if it carries one. `Unchanged` yields `None` because the
    /// card it refers to is the caller's, not the outcome's.
    pub fn card(&self) -> Option<&SessionCard> {
        match self {
            CardOutcome::Fresh { card, .. } => Some(card),
            _ => None,
        }
    }
}

impl SessionCard {
    /// Whether this carries anything worth showing — a card with neither field is
    /// indistinguishable from no card, and callers should treat it as `None`.
    pub fn is_empty(&self) -> bool {
        self.title.is_none() && self.last_prompt.is_none()
    }

    /// The best single line for `path`'s session: its name, else what it was last asked, else
    /// nothing. The one-call form for a consumer that has room for exactly one string.
    pub fn label(&self) -> Option<&str> {
        self.title.as_deref().or(self.last_prompt.as_deref())
    }
}

/// A pickable session — one transcript on disk plus the metadata the fuzzy session picker
/// shows and ranks by. Produced by the facade's `candidates_all` / the per-agent discovery.
#[derive(Clone)]
pub struct Candidate {
    /// Absolute path to the transcript `.jsonl` this entry opens (what a selection resolves
    /// to, and what the facade's `detect_agent` / `parse_session` are handed).
    pub path: PathBuf,
    /// The transcript file's last-modified time — the recency key the picker sorts by
    /// (most-recent first, after `cwd_affinity`).
    pub mtime: SystemTime,
    /// Which codebase/directory the session was working in, as a short human-recognizable
    /// label — the **leaf name of the session's working directory**, derived from the cwd the
    /// transcript recorded (a session under `/Users/you/code/knack` → `"knack"`). It groups
    /// and labels rows in the picker instead of showing an opaque id or a long path. Not a
    /// path, and not guaranteed unique (two dirs can share a leaf name).
    pub project: String,
    /// A preview of *what the session was about*, so you can recognise it at a glance: its
    /// **first genuine user prompt**, whitespace-collapsed and truncated to ~one line (e.g.
    /// `"add a --width flag to the CLI"`). Host-context / boilerplate messages are skipped;
    /// empty when the session has no user prompt yet.
    pub snippet: String,
    /// Whether this session belongs to the directory you're launching from **right now** —
    /// `true` iff its `project` matches the current working directory's. It's purely a
    /// **ranking hint**: the picker lists affinity sessions first, so "the sessions for *this*
    /// repo" float to the top, above everything else sorted by recency.
    pub cwd_affinity: bool,
    /// Which agent wrote this transcript (Claude / Codex) — shown as a badge and used to
    /// dispatch to the right parser.
    pub agent: Agent,
}

/// Directories from `cwd` up to (but **never including**) `home`, nearest first —
/// the ancestors we probe for a matching project. Cwd-based auto-discovery is
/// scoped to the user's home directory (#69): a cwd that is not strictly inside
/// `home` — including `home` itself, `/tmp`, a missing `$HOME` — yields NOTHING,
/// and the probe never reaches `home`'s own slug. Both halves exist because
/// misbehaving agents record sessions against directories a probe must never
/// match: QoderWork writes some sessions' project dir as `$HOME` itself (its store
/// grows a `-Users-<name>` dir) and others as `/` (the `-` dir, #62); scoping the
/// climb strictly below home makes both unreachable. Explicit paths/ids are
/// unaffected — only cwd inference is scoped. Agent-neutral; each adapter maps
/// these to its own store layout.
pub fn ancestors_below(cwd: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let Some(home) = home else {
        return Vec::new();
    };
    if home.as_os_str().is_empty() || cwd == home || !cwd.starts_with(home) {
        return Vec::new();
    }
    let mut dirs = vec![cwd.to_path_buf()];
    let mut cur = cwd.parent();
    while let Some(d) = cur {
        if d == home {
            break; // probe strict subdirectories only — never home's own slug
        }
        dirs.push(d.to_path_buf());
        cur = d.parent();
    }
    dirs
}

/// The process's `$HOME`, if set and non-empty — the home every public scoped
/// lookup passes to [`ancestors_below`].
pub fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// The working directory a session ran in, read from the transcript head — the
/// top-level `cwd` (Claude) or `payload.cwd` of `session_meta` (Codex). Used to
/// resolve a header's relativized path back to an absolute one (for reveal-in-
/// file-manager). `None` when no cwd is recorded. Agent-neutral: it accepts both shapes.
pub fn session_cwd(path: &Path) -> Option<PathBuf> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    for line in std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .take(50)
    {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(cwd) = v.get("cwd").and_then(Value::as_str) {
            return Some(PathBuf::from(cwd));
        }
        if v.get("type").and_then(Value::as_str) == Some("session_meta") {
            if let Some(cwd) = v.pointer("/payload/cwd").and_then(Value::as_str) {
                return Some(PathBuf::from(cwd));
            }
        }
    }
    None
}

/// The session id recorded in the transcript head — Claude's top-level `sessionId` or
/// Codex's `payload.id` of `session_meta`. `None` when absent (a caller then falls back to
/// the file stem). Agent-neutral, mirroring [`session_cwd`].
pub fn session_id(path: &Path) -> Option<String> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    for line in std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .take(50)
    {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(id) = v.get("sessionId").and_then(Value::as_str) {
            return Some(id.to_string());
        }
        if v.get("type").and_then(Value::as_str) == Some("session_meta") {
            if let Some(id) = v.pointer("/payload/id").and_then(Value::as_str) {
                return Some(id.to_string());
            }
        }
    }
    None
}
