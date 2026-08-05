//! **The frontend API** (#96 §8) — one call, an exhaustive outcome.
//!
//! Everything below this is invisible to a frontend: lock acquisition, validity checking,
//! alignment, truncation and the cold-rebuild decision all happen inside [`admit`]. What a
//! frontend writes is a `match`.
//!
//! Admission has **two** outcomes, not three. A cache entry is never shared, so you either own
//! it or you do not — and on denial *nothing was opened*. Falling back to a cache-less session
//! is a separate, explicit call, so "we gave up on caching" is visible at the call site rather
//! than hidden in a third variant that would suggest a session might be handed out while
//! another process owns it.

use super::lock::{self, Holder, Taken};
use super::stream::{anchor_of, window_at, MetaReader, MetaWriter};
use crate::engine::meta_stream::{align, Aligned, Versions};
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

/// The outcome of asking for a session.
#[derive(Debug)]
pub enum Admission<N> {
    /// Exclusive owner. Durable, and resumed when the cache was valid.
    Owned {
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
    /// The durable root could not be created or written.
    UnwritableRoot,
    /// No liveness check on this platform, so a lock cannot be reclaimed safely (§9).
    NoLivenessCheck,
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
}

/// Take exclusive ownership of a session's durable entry, or say why not.
///
/// `committed_len` is how many committed blocks the frontend's own loader recovered — **the
/// sole authority** on how far the content stream reaches (I1). Passing it in is what keeps
/// this function free of `BV` decoding, and therefore one implementation for every
/// presentation (R5).
pub fn admit<N: serde::Serialize + serde::de::DeserializeOwned + Clone>(
    root: Option<&Path>,
    p: Presentation,
    session: &str,
    src: &Path,
    versions: Versions,
    committed_len: usize,
    alive: impl Fn(&Holder<N>) -> bool,
) -> Admission<N> {
    let Some(root) = root else {
        return Admission::Denied(Denial::Unavailable(Unavailable::NoCacheFlag));
    };
    if !lock::liveness_decidable() {
        // Assuming a lock is stale would fail INTO concurrent writers — the one outcome the
        // lock exists to prevent. Better to serve cache-less.
        return Admission::Denied(Denial::Unavailable(Unavailable::NoLivenessCheck));
    }
    let dir = root.join(p.dir_name()).join(session);
    if std::fs::create_dir_all(&dir).is_err() {
        return Admission::Denied(Denial::Unavailable(Unavailable::UnwritableRoot));
    }
    match lock::acquire::<N>(&dir, alive) {
        Ok(Taken::Held(h)) => return Admission::Denied(Denial::Held(h)),
        Ok(Taken::Owned) => {}
        Err(_) => return Admission::Denied(Denial::Unavailable(Unavailable::UnwritableRoot)),
    }

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
    Admission::Owned {
        dir,
        origin,
        resumed,
    }
}

/// Validate and align a durable entry. `Err(reason)` is a diagnosable rejection; `Ok(None)`
/// means the stream was readable but nothing in it was corroborated by the content stream.
fn recover(
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
    let Some(a) = align(&records, committed_len) else {
        return Ok(None);
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

/// Open the writer for an entry `admit` granted, truncating the stream when the cache was not
/// reusable.
pub fn writer_for(
    dir: &Path,
    src: &Path,
    versions: Versions,
    resumed: bool,
) -> std::io::Result<MetaWriter> {
    if resumed {
        MetaWriter::open_append(dir, src)
    } else {
        MetaWriter::create(dir, src, versions)
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
    fn admit_at(root: &Path, src: &Path, committed: usize) -> Admission<Note> {
        admit(
            Some(root),
            Presentation::Tui,
            "s1",
            src,
            versions(),
            committed,
            |_| true,
        )
    }

    #[test]
    fn no_cache_flag_denies_without_touching_anything() {
        let a = admit::<Note>(
            None,
            Presentation::Tui,
            "s",
            Path::new("/nope"),
            versions(),
            0,
            |_| true,
        );
        assert!(matches!(
            a,
            Admission::Denied(Denial::Unavailable(Unavailable::NoCacheFlag))
        ));
    }

    #[test]
    fn a_first_run_is_cold_with_no_prior_cache() {
        let root = tmp("first");
        let src = root.join("t.jsonl");
        std::fs::write(&src, "x\n").unwrap();
        match admit_at(&root, &src, 0) {
            Admission::Owned {
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
            Admission::Owned {
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
            Admission::Owned { origin, .. } => assert_eq!(
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
            Admission::Owned {
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
            Admission::Owned { origin, .. } => {
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
        match admit::<Note>(Some(&root), Presentation::Tui, "s1", &src, newer, 3, |_| {
            true
        }) {
            Admission::Owned { origin, .. } => {
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
            Admission::Denied(Denial::Held(h)) => assert_eq!(h.pid, 999_999),
            _ => panic!("a live holder must deny"),
        }
    }

    /// The two presentations never contend: the directory and the lock are namespaced.
    #[test]
    fn presentations_do_not_contend() {
        let root = tmp("ns");
        let src = root.join("t.jsonl");
        std::fs::write(&src, "x\n").unwrap();
        let tui = admit::<Note>(
            Some(&root),
            Presentation::Tui,
            "s",
            &src,
            versions(),
            0,
            |_| true,
        );
        let html = admit::<Note>(
            Some(&root),
            Presentation::Html,
            "s",
            &src,
            versions(),
            0,
            |_| true,
        );
        assert!(matches!(tui, Admission::Owned { .. }));
        assert!(
            matches!(html, Admission::Owned { .. }),
            "a peer presentation must not be blocked"
        );
    }
}
