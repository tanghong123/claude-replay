//! The **discovery vocabulary** — the agent-free half of locating sessions (#87 step 3):
//! the shared [`Candidate`] type, the cwd-ancestor scoping helpers, and the format-neutral
//! transcript readers `first_cwd`/`session_id` (the head) and `latest_cwd` (the end) (and the
//! disk-grounded `project_path`). The REGISTRY half — `detect_agent`,
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
/// agent-generated one and the most recent prompt; Codex exposes its first genuine prompt as
/// the stable fallback label because current rollouts do not persist the generated task title.
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

/// How much of a first prompt a [`Candidate::snippet`] keeps.
///
/// A **memory** bound, not a display width. Every discovered session holds one of these, and a
/// first prompt can be an entire pasted file, so the cap has to exist — but a frontend is what
/// knows how much room it has. This was 72, which is roughly one 80-column row minus the
/// picker's fixed columns, and the result was that a session title stopped ~107 columns in
/// however wide the terminal was. Set it well past any real terminal and let the picker's own
/// width-fitting do the truncating it already does correctly (double-width glyphs included).
pub const SNIPPET_CHARS: usize = 512;

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
    /// **first genuine user prompt**, whitespace-collapsed and capped at [`SNIPPET_CHARS`]
    /// (e.g. `"add a --width flag to the CLI"`). Host-context / boilerplate messages are
    /// skipped; empty when the session has no user prompt yet.
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

/// The cwd recorded in one transcript record, in either shape: Claude's top-level `cwd` or
/// Codex's `payload.cwd` on a `session_meta`. The shared extractor behind [`first_cwd`] /
/// [`latest_cwd`].
fn cwd_in_record(v: &Value) -> Option<PathBuf> {
    if let Some(cwd) = v.get("cwd").and_then(Value::as_str) {
        return Some(PathBuf::from(cwd));
    }
    if v.get("type").and_then(Value::as_str) == Some("session_meta") {
        if let Some(cwd) = v.pointer("/payload/cwd").and_then(Value::as_str) {
            return Some(PathBuf::from(cwd));
        }
    }
    None
}

/// The **first** working directory recorded in the transcript — the session's launch/anchor
/// directory, **not** necessarily its current one: a session that `cd`s away keeps reporting
/// this. Pure over the first 50 lines (cheap). `None` when none is recorded. Named for what it
/// is; see [`latest_cwd`] for the most-recent one and [`project_path`] for the disk-grounded
/// repository the session belongs to. (This is the old `session_cwd`, renamed — "cwd" means
/// *current*, which this value is not.)
pub fn first_cwd(path: &Path) -> Option<PathBuf> {
    crate::engine::bounded_lines(path, crate::engine::Elision::None)
        .take(50)
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        .find_map(|v| cwd_in_record(&v))
}

/// How much of a transcript's END [`latest_cwd`] reads before it reads the whole file. Measured
/// over this machine's 141 transcripts (2026-09-26, #10): every Claude and QoderWork session
/// records its last cwd within 1 MiB of EOF (132 of them within 64 KiB), because nearly every
/// line they write carries one. A Codex rollout records it only in its head `session_meta`, so
/// those always fall back — 1 MiB on top of the scan they paid before.
const LATEST_CWD_TAIL: u64 = 1 << 20;

/// The **last** working directory recorded in the transcript — the session's most-recent cwd.
/// Pure. Accepts both Claude and Codex shapes. `None` when none is recorded.
///
/// The last cwd can be anywhere, so the answer is a whole-file question, but it is almost always
/// on one of the last few lines: the END is read first, and the whole file only when the end
/// records none (#10). That is exact rather than a heuristic — if any line in the tail records a
/// cwd, every line after the last such one was read too, so it IS the file's last. It was a
/// whole-file scan of every transcript `--paths --all` returned: 3.6 s of a daily sweep that then
/// folded the same bytes again.
pub fn latest_cwd(path: &Path) -> Option<PathBuf> {
    latest_cwd_within(path, LATEST_CWD_TAIL)
}

/// [`latest_cwd`] with the tail's size as a parameter, so the tests can move the window across
/// every position of a small file.
fn latest_cwd_within(path: &Path, tail: u64) -> Option<PathBuf> {
    let len = std::fs::metadata(path).ok()?.len();
    if len > tail {
        if let Some(cwd) = last_cwd_from(path, len - tail) {
            return Some(cwd);
        }
    }
    last_cwd_from(path, 0)
}

/// The last cwd among the lines that START at or after `from`, each read through the bounded
/// source the whole-file scan uses (#193: `Aggressive` elision, a torn final line yielded) — so a
/// line reads the same whichever of the two scans reaches it. A `from` inside a line moves on to
/// the next line start, and so does a `from` that IS one: telling the two apart costs a read
/// behind `from`, and whatever the skip passes over the whole-file fallback still reads.
fn last_cwd_from(path: &Path, from: u64) -> Option<PathBuf> {
    use std::io::{BufRead, Seek, SeekFrom};
    let mut reader = std::io::BufReader::new(std::fs::File::open(path).ok()?);
    let mut at = from;
    if from > 0 {
        reader.seek(SeekFrom::Start(from)).ok()?;
        // To the end of the line `from` lands in, through the buffer — never collected.
        loop {
            let buf = reader.fill_buf().ok()?;
            if buf.is_empty() {
                return None; // no line starts in the tail
            }
            match buf.iter().position(|&b| b == b'\n') {
                Some(i) => {
                    reader.consume(i + 1);
                    at += i as u64 + 1;
                    break;
                }
                None => {
                    let n = buf.len();
                    reader.consume(n);
                    at += n as u64;
                }
            }
        }
    }
    let mut src = crate::engine::LineSource::new(
        reader,
        at,
        crate::engine::TornTail::Yield,
        crate::engine::Elision::Aggressive,
    );
    let mut last = None;
    while let Ok(Some((_, line))) = src.next() {
        if let Some(cwd) = serde_json::from_str::<Value>(line)
            .ok()
            .and_then(|v| cwd_in_record(&v))
        {
            last = Some(cwd);
        }
    }
    last
}

/// The directory a session's transcript belongs to — DISK-GROUNDED, unlike the pure
/// [`first_cwd`]/[`latest_cwd`] facts. Normally the recorded cwd, but a project that was moved
/// or renamed leaves that cwd DEAD in the immutable transcript; then follow the transcript to
/// where it now LIVES: the store directory it sits in (Claude files a session under its cwd,
/// every `/` turned to `-`), decoded and kept only if it exists. Falls back to the (dead)
/// recorded cwd when neither resolves.
///
/// Consumers are the ones that need a REAL location on disk: grouping (the monitor,
/// agent-metrics) and the viewer's **reveal-in-file-manager action** (a clicked path must open
/// where the file actually is, even after a move). What must NOT use it is the viewer's
/// *rendered* output — the relativized paths in the dump/HTML stream — which stays a pure
/// function of transcript content (`first_cwd`/`latest_cwd`) so it is deterministic and
/// byte-identical across machines. Reveal is an action, not rendered bytes, so it's on this side
/// of that line. Keeping the filesystem dependency HERE, out of `first_cwd`/`latest_cwd`, is what
/// holds that boundary.
pub fn project_path(path: &Path) -> Option<PathBuf> {
    let recorded = first_cwd(path);
    if recorded.as_deref().is_some_and(Path::is_dir) {
        return recorded;
    }
    if let Some(decoded) = decode_store_dir(path).filter(|d| d.is_dir()) {
        return Some(decoded);
    }
    recorded
}

/// The git repository ROOT the session belongs to — disk-grounded, for GROUPING (the monitor,
/// agent-metrics) that wants one entry per repo rather than one per working *sub*directory.
/// Starts from [`project_path`] (the session's live cwd location) and walks up to the nearest
/// ancestor holding a `.git` — a directory for a normal clone, a FILE for a worktree/submodule,
/// both counted. Falls back to `project_path` when the session sits under no repo. `None` only
/// when `project_path` is.
///
/// The split from [`project_path`] is deliberate and load-bearing: `project_path` is the
/// session's EXACT cwd on disk — what the reveal action opens, and what a live process's cwd is
/// matched against (subdir-precise) — while `repo_root` normalizes a subdir `cd` (whid's
/// `.whid/run-…`) up to the one repo. A consumer that keys off a running process's cwd must use
/// `project_path`; a consumer that groups by project uses this. A plain `.git` stat-walk (no
/// `git` shell-out), so a monitor scan over many sessions stays cheap — no cache needed.
pub fn repo_root(path: &Path) -> Option<PathBuf> {
    let start = project_path(path)?;
    // `project_path`'s last resort is the DEAD recorded cwd (the unsolved plain-`mv` case: the
    // recorded cwd is gone AND the store dir doesn't decode). Never walk a dead path's ancestors
    // — they can be live and unrelated (a moved repo whose old parent now sits under a `~/.git`
    // dotfiles repo would wrongly collapse into it). Return it as-is; both live branches of
    // `project_path` already proved `is_dir`, so this only guards that dead fallback.
    if !start.is_dir() {
        return Some(start);
    }
    let mut cur: &Path = &start;
    loop {
        if cur.join(".git").exists() {
            return Some(cur.to_path_buf());
        }
        match cur.parent() {
            Some(parent) => cur = parent,
            None => break,
        }
    }
    // No `.git` anywhere above: the session is outside a repo — group it under its own dir.
    Some(start)
}

/// Decode a Claude project store-dir name back to its absolute cwd. The encoding replaces every
/// `/` with `-`, which is LOSSY — a literal `-` in a directory name is indistinguishable from a
/// separator (`agent-metrics`, or this repo's own `claude-replay`, would mis-split under a naive
/// `-`→`/`). So decode by WALKING the real filesystem: at each level take the LONGEST run of
/// `-`-joined tokens that names an existing directory. `None` if any level fails to resolve.
/// Guarded on the leading `-` so a non-Claude store layout is never mistaken for a slug.
fn decode_store_dir(path: &Path) -> Option<PathBuf> {
    let slug = path.parent()?.file_name()?.to_str()?;
    if !slug.starts_with('-') {
        return None;
    }
    let tokens: Vec<&str> = slug[1..].split('-').collect();
    let mut cur = PathBuf::from("/");
    let mut i = 0;
    while i < tokens.len() {
        let mut best: Option<usize> = None;
        let mut comp = String::new();
        for (k, tok) in tokens[i..].iter().enumerate() {
            if k > 0 {
                comp.push('-');
            }
            comp.push_str(tok);
            if cur.join(&comp).is_dir() {
                best = Some(i + k);
            }
        }
        let j = best?;
        cur.push(tokens[i..=j].join("-"));
        i = j + 1;
    }
    Some(cur)
}

/// The session id recorded in the transcript head — Claude's top-level `sessionId` or
/// Codex's `payload.id` of `session_meta`. `None` when absent (a caller then falls back to
/// the file stem). Agent-neutral, mirroring [`first_cwd`].
pub fn session_id(path: &Path) -> Option<String> {
    for line in crate::engine::bounded_lines(path, crate::engine::Elision::None).take(50) {
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

#[cfg(test)]
mod cwd_tests {
    use super::*;

    fn write(path: &Path, body: &str) {
        std::fs::write(path, body).unwrap();
    }

    /// `first_cwd` returns the EARLIEST recorded cwd, `latest_cwd` the LAST — the whole reason
    /// they're two functions with honest names.
    #[test]
    fn first_and_latest_cwd_differ_when_a_session_moves() {
        let dir = std::env::temp_dir().join(format!("cr-cwd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let t = dir.join("s.jsonl");
        write(
            &t,
            concat!(
                r#"{"type":"user","cwd":"/a/start","sessionId":"s"}"#,
                "\n",
                r#"{"type":"user","cwd":"/a/middle","sessionId":"s"}"#,
                "\n",
                r#"{"type":"assistant","cwd":"/a/end","sessionId":"s"}"#,
                "\n",
            ),
        );
        assert_eq!(first_cwd(&t), Some(PathBuf::from("/a/start")));
        assert_eq!(latest_cwd(&t), Some(PathBuf::from("/a/end")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #10: reading the END first changes the cost of `latest_cwd`, never its answer. The oracle
    /// is the whole-file scan it replaced, verbatim; every fixture is checked with the tail
    /// window at EVERY size from one byte past the whole file, so the window's edge lands on
    /// every byte — mid-line, on a line start, on a newline, inside a torn tail.
    #[test]
    fn reading_the_tail_first_agrees_with_the_whole_file_at_every_window() {
        let oracle = |p: &Path| {
            crate::engine::bounded_lines(p, crate::engine::Elision::Aggressive)
                .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
                .filter_map(|v| cwd_in_record(&v))
                .last()
        };
        let long = "x".repeat(300);
        let fixtures: Vec<(&str, String)> = vec![
            (
                "the session moved, and says so on every line",
                concat!(
                    r#"{"type":"user","cwd":"/a/start"}"#,
                    "\n",
                    r#"{"type":"assistant","cwd":"/a/middle"}"#,
                    "\n",
                    r#"{"type":"user","cwd":"/a/end"}"#,
                    "\n",
                )
                .to_string(),
            ),
            (
                "Codex: the cwd lives only in the head record",
                format!(
                    "{}\n{}\n{}\n{}\n",
                    r#"{"type":"session_meta","payload":{"id":"c","cwd":"/codex/repo"}}"#,
                    r#"{"type":"response_item","payload":{"type":"message"}}"#,
                    r#"{"type":"event_msg","payload":{"type":"token_count"}}"#,
                    r#"{"type":"response_item","payload":{"type":"message"}}"#,
                ),
            ),
            (
                "the last line is longer than most windows, and carries the answer",
                format!(
                    "{}\n{{\"type\":\"user\",\"cwd\":\"/big/last\",\"t\":\"{long}\"}}\n",
                    r#"{"type":"user","cwd":"/before"}"#
                ),
            ),
            (
                "records after the last cwd carry none",
                format!(
                    "{}\n{}\n{}\n{}\n",
                    r#"{"type":"user","cwd":"/only"}"#,
                    r#"{"type":"summary","summary":"s"}"#,
                    r#"{"type":"custom-title","customTitle":"t"}"#,
                    r#"{"type":"last-prompt","lastPrompt":"p"}"#,
                ),
            ),
            (
                "a torn final line that parses is a record awaiting its newline",
                format!(
                    "{}\n{}",
                    r#"{"type":"user","cwd":"/complete"}"#, r#"{"type":"user","cwd":"/torn"}"#
                ),
            ),
            (
                "a torn final line that does not parse is a write in progress",
                format!(
                    "{}\n{}",
                    r#"{"type":"user","cwd":"/complete"}"#, r#"{"type":"user","cwd":"/tor"#
                ),
            ),
            (
                "blank lines and CRLF endings",
                "{\"type\":\"user\",\"cwd\":\"/crlf/a\"}\r\n\r\n\n{\"type\":\"user\",\"cwd\":\"/crlf/b\"}\r\n\n"
                    .to_string(),
            ),
            ("nothing records a cwd", "{\"type\":\"summary\"}\n{}\n".to_string()),
            ("an empty file", String::new()),
        ];
        let dir = std::env::temp_dir().join(format!("cr-cwd-tail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (i, (what, body)) in fixtures.iter().enumerate() {
            let p = dir.join(format!("f{i}.jsonl"));
            write(&p, body);
            let want = oracle(&p);
            for tail in 1..=body.len() as u64 + 1 {
                assert_eq!(
                    latest_cwd_within(&p, tail),
                    want,
                    "{what}: a {tail}-byte tail disagrees with the whole-file scan"
                );
            }
            assert_eq!(latest_cwd(&p), want, "{what}: the production window");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `project_path` is the DISK-GROUNDED one: a session whose recorded cwd was moved/deleted
    /// follows its store dir to where it now lives (the decode walks the filesystem, so a dash
    /// IN a directory name — the temp path here runs through `claude-replay` — is rejoined, not
    /// split). A recorded cwd that still exists wins outright.
    #[test]
    fn project_path_follows_the_store_dir_when_the_recorded_cwd_is_dead() {
        let pid = std::process::id();
        let live = std::env::temp_dir().join(format!("crmoved{pid}"));
        std::fs::create_dir_all(&live).unwrap();
        // The Claude store-dir slug for `live` (absolute path, every '/' → '-').
        let slug = live.display().to_string().replace('/', "-");
        let store = std::env::temp_dir()
            .join(format!("crstore{pid}"))
            .join(&slug);
        std::fs::create_dir_all(&store).unwrap();

        let dead = store.join("dead.jsonl");
        write(
            &dead,
            "{\"type\":\"user\",\"cwd\":\"/no/such/place-zzz\"}\n",
        );
        assert_eq!(project_path(&dead), Some(live.clone()));

        let alive = store.join("alive.jsonl");
        write(
            &alive,
            &format!("{{\"type\":\"user\",\"cwd\":\"{}\"}}\n", live.display()),
        );
        assert_eq!(project_path(&alive), Some(live.clone()));

        let _ = std::fs::remove_dir_all(&live);
        let _ = std::fs::remove_dir_all(store.parent().unwrap());
    }

    /// `repo_root` walks up from the session's cwd to the nearest `.git` (grouping), while
    /// `project_path` stays at the exact cwd (reveal / process matching) — the two are distinct
    /// on purpose. A `.git` FILE (worktree/submodule) counts the same as a directory.
    #[test]
    fn repo_root_climbs_to_the_git_marker_while_project_path_stays_put() {
        let pid = std::process::id();
        let base = std::env::temp_dir().join(format!("crreporoot{pid}"));
        let _ = std::fs::remove_dir_all(&base);

        // A normal clone with a subdir; the session's recorded cwd IS the subdir.
        let repo = base.join("myrepo");
        let sub = repo.join("crate").join("src");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let t = base.join("s.jsonl");
        write(
            &t,
            &format!("{{\"type\":\"user\",\"cwd\":\"{}\"}}\n", sub.display()),
        );
        // project_path is the exact subdir; repo_root normalizes up to the repo.
        assert_eq!(project_path(&t), Some(sub.clone()));
        assert_eq!(repo_root(&t), Some(repo.clone()));

        // A `.git` FILE (a worktree/submodule checkout) is a repo root too.
        let wt = base.join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        write(&wt.join(".git"), "gitdir: /elsewhere/.git/worktrees/wt\n");
        let t2 = base.join("s2.jsonl");
        write(
            &t2,
            &format!("{{\"type\":\"user\",\"cwd\":\"{}\"}}\n", wt.display()),
        );
        assert_eq!(repo_root(&t2), Some(wt.clone()));

        // A DEAD recorded cwd (the unsolved plain-`mv` case) is returned as-is — never walked
        // into a live ancestor. Deterministic regardless of any `.git` above the temp dir,
        // because the guard returns before the walk. The parent isn't a store slug, so
        // `project_path` can only hand back the dead cwd.
        let t3 = base.join("s3.jsonl");
        write(&t3, "{\"type\":\"user\",\"cwd\":\"/no/such/moved-repo\"}\n");
        assert_eq!(repo_root(&t3), Some(PathBuf::from("/no/such/moved-repo")));

        let _ = std::fs::remove_dir_all(&base);
    }
}
