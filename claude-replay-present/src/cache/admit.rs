//! **Claiming a durable entry** (#96 §8) — the machinery behind the frontend API.
//!
//! [`claim`] does everything that has to happen before a session can exist: take the lock, check
//! the versions and the source identity, align the record stream to what the content stream
//! corroborates, and decide resume-or-rebuild. What it does NOT do is build the session — it
//! knows nothing about `BV`, which is exactly why one implementation serves every presentation.
//!
//! [`SessionCache::admit`](crate::cache::SessionCache::admit) wraps this into the two-outcome
//! [`Admission`](crate::cache::Admission) a frontend actually matches on.

use super::lock::{self, Holder, Taken};
use super::stream::{anchor_of, meta_path, window_at, MetaReader, MetaWriter};
use crate::engine::meta_stream::{align, AlignError, Aligned, Versions};
use std::path::{Path, PathBuf};

/// Which frontend a durable entry belongs to. Namespaces the directory **and** the lock, so a
/// TUI and an HTML server on the same session never contend (R3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presentation {
    Tui,
    Html,
}

impl Presentation {
    pub fn dir_name(self) -> &'static str {
        match self {
            Presentation::Tui => "tui",
            Presentation::Html => "html",
        }
    }
}

/// The outcome of claiming a durable entry.
#[derive(Debug)]
pub enum Claim<N> {
    /// Ours exclusively. Durable, and resumed when the cache was valid.
    Ours {
        dir: PathBuf,
        origin: Origin,
        /// The recovered state, when this was a resume. `None` on a cold start.
        resumed: Option<Box<Aligned>>,
    },
    /// Not the owner. **Nothing was opened, nothing is shared.**
    Denied(Denial<N>),
}

#[derive(Debug)]
pub enum Denial<N> {
    /// Another live process holds it. `Holder` carries what a *message* needs — never a lock.
    Held(Holder<N>),
    /// No durable slot exists to compete for.
    Unavailable(Unavailable),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// `--no-cache`.
    NoCacheFlag,
    /// The durable root, or the entry's own backing, could not be created or written.
    UnwritableRoot,
    /// No liveness check on this platform, so a lock cannot be reclaimed safely (§9).
    NoLivenessCheck,
    /// The id is not registered — there is no source to open, cached or not.
    UnknownSession,
}

/// Where durable entries live: `$XDG_CACHE_HOME` (or `~/.cache`) `/claude-replay/sessions`.
///
/// Deliberately NOT the temp bundle dir a serve run wipes on startup — the whole point is to
/// survive the process. `None` when neither variable resolves, which denies durability rather
/// than guessing at a writable location.
pub fn default_root() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("claude-replay").join("sessions"))
}

/// A durable entry's directory. A pure function of the three, so the caller can name the entry
/// before deciding to claim it.
pub fn entry_dir(root: &Path, p: Presentation, session: &str) -> PathBuf {
    root.join(p.dir_name()).join(session)
}

#[derive(Clone, Debug, PartialEq)]
pub enum Origin {
    Resumed { committed: usize, replay_from: u64 },
    Cold(ColdReason),
}

/// Why a cold fold happened. Diagnosable on purpose: "the cache did not help" is a support
/// question, and the rejection tests assert on these rather than on "it rebuilt".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColdReason {
    NoPriorCache,
    SourceRewritten,
    VersionChanged,
    /// The stream held no record the content stream corroborates — a torn tail below the first
    /// resume point, or a content stream shorter than every recorded commit.
    TornStream,
    /// A checkpoint disagreed with the state folded up to it (§6.6) — the stream is corrupt, or
    /// writer and reader have drifted. Either way the cache is not to be trusted.
    CheckpointMismatch,
}

/// Take exclusive ownership of a session's durable entry, or say why not.
///
/// `committed_len` is a **callback**, not a value, and that is the whole ordering argument: it
/// opens the frontend's backing and reports how far the content stream reaches (I1, the sole
/// authority), so it must run *after* the lock is ours — otherwise a denial would leave a
/// backing open for a session another process owns, and "nothing was opened" would be a lie.
/// `None` from it means the backing could not be opened, and the lock is handed back.
///
/// The callback is also what keeps this function free of `BV` decoding, and therefore one
/// implementation for every presentation (R5).
pub fn claim<N: serde::Serialize + serde::de::DeserializeOwned + Clone>(
    root: Option<&Path>,
    p: Presentation,
    session: &str,
    src: &Path,
    versions: Versions,
    committed_len: impl FnOnce(&Path) -> Option<usize>,
    alive: impl Fn(&Holder<N>) -> bool,
) -> Claim<N> {
    let Some(root) = root else {
        return Claim::Denied(Denial::Unavailable(Unavailable::NoCacheFlag));
    };
    if !lock::liveness_decidable() {
        // Assuming a lock is stale would fail INTO concurrent writers — the one outcome the
        // lock exists to prevent. Better to serve cache-less.
        return Claim::Denied(Denial::Unavailable(Unavailable::NoLivenessCheck));
    }
    let dir = entry_dir(root, p, session);
    if std::fs::create_dir_all(&dir).is_err() {
        return Claim::Denied(Denial::Unavailable(Unavailable::UnwritableRoot));
    }
    match lock::acquire::<N>(&dir, alive) {
        Ok(Taken::Held(h)) => return Claim::Denied(Denial::Held(h)),
        Ok(Taken::Owned) => {}
        Err(_) => return Claim::Denied(Denial::Unavailable(Unavailable::UnwritableRoot)),
    }
    let Some(committed_len) = committed_len(&dir) else {
        lock::release::<N>(&dir); // we took it and cannot use it — do not pin the session
        return Claim::Denied(Denial::Unavailable(Unavailable::UnwritableRoot));
    };

    let (origin, resumed) = match recover(&dir, src, &versions, committed_len) {
        Ok(Some(a)) => (
            Origin::Resumed {
                committed: a.committed,
                replay_from: a.resume.replay_from,
            },
            Some(Box::new(a)),
        ),
        Ok(None) => (Origin::Cold(ColdReason::TornStream), None),
        Err(r) => (Origin::Cold(r), None),
    };
    Claim::Ours {
        dir,
        origin,
        resumed,
    }
}

/// Validate and align a durable entry. `Err(reason)` is a diagnosable rejection; `Ok(None)`
/// means the stream was readable but nothing in it was corroborated by the content stream.
pub(crate) fn recover(
    dir: &Path,
    src: &Path,
    versions: &Versions,
    committed_len: usize,
) -> Result<Option<Aligned>, ColdReason> {
    let Some((header, reader)) = MetaReader::open(dir).map_err(|_| ColdReason::NoPriorCache)?
    else {
        return Err(ColdReason::NoPriorCache);
    };
    if header.versions != *versions {
        return Err(ColdReason::VersionChanged);
    }
    // Identity first: a stream must never be matched against a different file at the same path.
    if anchor_of(src).map_err(|_| ColdReason::SourceRewritten)? != header.anchor {
        return Err(ColdReason::SourceRewritten);
    }
    // Feed records to the fold, stopping at what the content stream corroborates (I1).
    let records: Vec<_> = reader.collect();
    let a = match align(&records, committed_len) {
        Ok(Some(a)) => a,
        Ok(None) => return Ok(None),
        Err(AlignError::CheckpointMismatch) => return Err(ColdReason::CheckpointMismatch),
    };
    // The source must still reach the partition, and the bytes BELOW it must be unchanged —
    // everything the resume restores derives from there. Bytes at or after it are re-read and
    // folded fresh, so a rewrite there is self-correcting and needs no coverage.
    let len = std::fs::metadata(src)
        .map_err(|_| ColdReason::SourceRewritten)?
        .len();
    if len < a.resume.replay_from {
        return Err(ColdReason::SourceRewritten);
    }
    let win = window_at(src, a.resume.replay_from).map_err(|_| ColdReason::SourceRewritten)?;
    if win != a.resume.window {
        return Err(ColdReason::SourceRewritten);
    }
    Ok(Some(a))
}

/// Open the writer for an entry [`claim`] granted, starting a fresh stream when the cache was
/// not reusable.
pub fn writer_for(
    dir: &Path,
    src: &Path,
    versions: Versions,
    resumed: Option<usize>,
) -> std::io::Result<MetaWriter> {
    match resumed {
        Some(keep) => MetaWriter::open_append(dir, src, keep),
        None => MetaWriter::create(dir, src, versions),
    }
}

/// How long an untouched entry survives. Long enough that "the session I was reading last week"
/// still resumes; short enough that a machine's cache does not grow forever.
pub const KEEP_FOR: std::time::Duration = std::time::Duration::from_secs(14 * 24 * 3600);

/// When a LOCKED entry stops being taken at its word. A lock normally makes an entry untouchable
/// without probing — probing every entry on every start is exactly the cost a sweep should not
/// pay — but a crashed holder must not pin bytes forever, so past this age the lock is probed.
///
/// Necessarily longer than [`KEEP_FOR`]: the window between them is where the lock actually does
/// something, and if it were shorter the trust would never apply at all.
pub const LOCK_TRUSTED_FOR: std::time::Duration = std::time::Duration::from_secs(30 * 24 * 3600);

const _: () = assert!(
    LOCK_TRUSTED_FOR.as_secs() > KEEP_FOR.as_secs(),
    "a lock must be trusted for longer than an entry survives, or it protects nothing"
);

/// Delete durable entries nobody is using (#96 §11 step 9). Best-effort throughout: a sweep that
/// cannot read a directory simply keeps whatever is in it.
///
/// Called once per durable cache construction. It is a directory walk, not a read of any stream.
pub fn gc(root: &Path) {
    let now = std::time::SystemTime::now();
    let age = |p: &Path| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
    };
    let Ok(presentations) = std::fs::read_dir(root) else {
        return;
    };
    for p in presentations.flatten() {
        let Ok(entries) = std::fs::read_dir(p.path()) else {
            continue;
        };
        for e in entries.flatten() {
            let dir = e.path();
            if !dir.is_dir() {
                continue;
            }
            // The meta stream IS the activity signal — it is appended on every commit — so its
            // mtime is how long since anything happened here. An entry with no stream yet falls
            // back to the directory's own.
            let Some(idle) = age(&meta_path(&dir)).or_else(|| age(&dir)) else {
                continue;
            };
            if idle < KEEP_FOR {
                continue;
            }
            if lock::read::<serde_json::Value>(&dir).is_some_and(|h| {
                // Below the cap a lock is trusted outright. Above it, the holder has to prove it
                // still exists — otherwise a crash would pin these bytes permanently.
                idle < LOCK_TRUSTED_FOR || lock::pid_alive(h.pid)
            }) {
                continue;
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::meta_stream::{MetaRecord, Resume};

    #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Note;

    fn versions() -> Versions {
        Versions {
            format: 1,
            fold: 1,
            flavor: None,
        }
    }
    fn tmp(n: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cr-admit-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
    /// Backdate an entry's meta stream by `by`, so a sweep sees the entry as that idle.
    fn age_to(dir: &Path, by: std::time::Duration) {
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(meta_path(dir))
            .unwrap();
        f.set_modified(std::time::SystemTime::now() - by).unwrap();
    }

    /// A source plus a stream describing `n` commits over it.
    fn seeded(root: &Path, n: usize) -> PathBuf {
        let src = root.join("t.jsonl");
        std::fs::write(&src, "abcdefghij".repeat(50)).unwrap();
        let dir = root.join(Presentation::Tui.dir_name()).join("s1");
        let mut w = MetaWriter::create(&dir, &src, versions()).unwrap();
        for i in 1..=n {
            w.append(&MetaRecord {
                turns: Some(1),
                resume: Some(Resume {
                    id: i,
                    replay_from: (i * 10) as u64,
                    window: 0,
                    prev_ts: None,
                    pending_ts: None,
                }),
                ..Default::default()
            })
            .unwrap();
        }
        w.flush().unwrap();
        src
    }
    fn admit_at(root: &Path, src: &Path, committed: usize) -> Claim<Note> {
        claim(
            Some(root),
            Presentation::Tui,
            "s1",
            src,
            versions(),
            |_| Some(committed),
            |_| true,
        )
    }

    #[test]
    fn no_cache_flag_denies_without_touching_anything() {
        let a = claim::<Note>(
            None,
            Presentation::Tui,
            "s",
            Path::new("/nope"),
            versions(),
            |_| Some(0),
            |_| true,
        );
        assert!(matches!(
            a,
            Claim::Denied(Denial::Unavailable(Unavailable::NoCacheFlag))
        ));
    }

    #[test]
    fn a_first_run_is_cold_with_no_prior_cache() {
        let root = tmp("first");
        let src = root.join("t.jsonl");
        std::fs::write(&src, "x\n").unwrap();
        match admit_at(&root, &src, 0) {
            Claim::Ours {
                origin, resumed, ..
            } => {
                assert_eq!(origin, Origin::Cold(ColdReason::NoPriorCache));
                assert!(resumed.is_none(), "nothing to resume from");
            }
            _ => panic!("a free entry must be Owned"),
        }
    }

    #[test]
    fn a_valid_cache_resumes_at_the_last_corroborated_commit() {
        let root = tmp("resume");
        let src = seeded(&root, 3);
        match admit_at(&root, &src, 3) {
            Claim::Ours {
                origin, resumed, ..
            } => {
                assert_eq!(
                    origin,
                    Origin::Resumed {
                        committed: 3,
                        replay_from: 30
                    }
                );
                assert_eq!(resumed.unwrap().committed, 3);
            }
            _ => panic!("expected Owned"),
        }
    }

    /// The content stream is the AUTHORITY: meta describing commits it cannot corroborate is
    /// ignored, never trusted (I1).
    #[test]
    fn meta_ahead_of_content_aligns_down() {
        let root = tmp("ahead");
        let src = seeded(&root, 3);
        match admit_at(&root, &src, 2) {
            Claim::Ours { origin, .. } => assert_eq!(
                origin,
                Origin::Resumed {
                    committed: 2,
                    replay_from: 20
                },
                "must fall back to what the content stream supports"
            ),
            _ => panic!("expected Owned"),
        }
    }

    /// A rewritten source must be REJECTED, not silently resumed against — the false-accept
    /// class, which yields wrong output rather than a no-op.
    #[test]
    fn a_rewritten_source_is_rejected() {
        let root = tmp("rewrite");
        let src = seeded(&root, 3);
        std::fs::write(
            &src,
            "completely different content that is also long enough",
        )
        .unwrap();
        match admit_at(&root, &src, 3) {
            Claim::Ours {
                origin, resumed, ..
            } => {
                assert_eq!(origin, Origin::Cold(ColdReason::SourceRewritten));
                assert!(resumed.is_none());
            }
            _ => panic!("expected Owned with a cold origin"),
        }
    }

    /// A truncated source cannot reach its recorded partition.
    #[test]
    fn a_truncated_source_is_rejected() {
        let root = tmp("trunc");
        let src = seeded(&root, 3);
        std::fs::write(&src, "ab").unwrap();
        match admit_at(&root, &src, 3) {
            Claim::Ours { origin, .. } => {
                assert_eq!(origin, Origin::Cold(ColdReason::SourceRewritten))
            }
            _ => panic!("expected Owned"),
        }
    }

    /// A fold-logic bump invalidates: a resume would otherwise splice blocks built by two
    /// different folds into one session.
    #[test]
    fn a_version_change_is_rejected() {
        let root = tmp("ver");
        let src = seeded(&root, 3);
        let newer = Versions {
            format: 1,
            fold: 2,
            flavor: None,
        };
        match claim::<Note>(
            Some(&root),
            Presentation::Tui,
            "s1",
            &src,
            newer,
            |_| Some(3),
            |_| true,
        ) {
            Claim::Ours { origin, .. } => {
                assert_eq!(origin, Origin::Cold(ColdReason::VersionChanged))
            }
            _ => panic!("expected Owned"),
        }
    }

    /// A live holder denies, and NOTHING is opened — the two-outcome invariant.
    #[test]
    fn a_live_holder_denies_and_opens_nothing() {
        let root = tmp("held");
        let src = seeded(&root, 1);
        let dir = root.join(Presentation::Tui.dir_name()).join("s1");
        std::fs::write(
            lock::lock_path(&dir),
            serde_json::to_string(&Holder::<Note> {
                pid: 999_999,
                dir: dir.clone(),
                note: None,
            })
            .unwrap(),
        )
        .unwrap();
        match admit_at(&root, &src, 1) {
            Claim::Denied(Denial::Held(h)) => assert_eq!(h.pid, 999_999),
            _ => panic!("a live holder must deny"),
        }
    }

    /// A fresh entry survives the sweep, and a stale unlocked one does not. (Ages are set by
    /// touching the entry's mtime — the alternative is a test that sleeps for two weeks.)
    #[test]
    fn gc_keeps_fresh_entries_and_removes_stale_ones() {
        let root = tmp("gc");
        let fresh = entry_dir(&root, Presentation::Tui, "fresh");
        let stale = entry_dir(&root, Presentation::Tui, "stale");
        for d in [&fresh, &stale] {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(meta_path(d), "{}\n").unwrap();
        }
        age_to(&stale, KEEP_FOR * 2);

        gc(&root);
        assert!(fresh.exists(), "a fresh entry survives");
        assert!(!stale.exists(), "a stale one is swept");
    }

    /// A LOCK protects a stale entry — until it is old enough that a crashed holder would be
    /// pinning the bytes forever, at which point the holder has to prove it exists.
    #[test]
    fn gc_respects_a_lock_until_the_age_cap() {
        let root = tmp("gclock");
        let held = entry_dir(&root, Presentation::Tui, "held");
        let ancient = entry_dir(&root, Presentation::Tui, "ancient");
        for d in [&held, &ancient] {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(meta_path(d), "{}\n").unwrap();
            std::fs::write(
                lock::lock_path(d),
                serde_json::to_string(&Holder::<Note> {
                    pid: 999_999, // a pid that is not running
                    dir: d.clone(),
                    note: None,
                })
                .unwrap(),
            )
            .unwrap();
        }
        age_to(&held, KEEP_FOR * 2); // stale, but inside the lock's trust window
        age_to(&ancient, LOCK_TRUSTED_FOR * 2);

        gc(&root);
        assert!(held.exists(), "a lock is taken at its word below the cap");
        assert!(
            !ancient.exists(),
            "above the cap a dead holder no longer pins the bytes"
        );
    }

    /// The two presentations never contend: the directory and the lock are namespaced.
    #[test]
    fn presentations_do_not_contend() {
        let root = tmp("ns");
        let src = root.join("t.jsonl");
        std::fs::write(&src, "x\n").unwrap();
        let tui = claim::<Note>(
            Some(&root),
            Presentation::Tui,
            "s",
            &src,
            versions(),
            |_| Some(0),
            |_| true,
        );
        let html = claim::<Note>(
            Some(&root),
            Presentation::Html,
            "s",
            &src,
            versions(),
            |_| Some(0),
            |_| true,
        );
        assert!(matches!(tui, Claim::Ours { .. }));
        assert!(
            matches!(html, Claim::Ours { .. }),
            "a peer presentation must not be blocked"
        );
    }
}
