//! **The single-writer lock** (#96 §9) — one per `<presentation, session>`.
//!
//! There is **exactly one writer per cache entry, always**. Two processes never share one: a
//! second is denied, and denial opens nothing. That is why [`Admission`](super::Admission) has
//! two outcomes rather than three — a cache-less session is a *separate*, explicit choice the
//! frontend makes afterwards.
//!
//! Reclaim is liveness-based. Where liveness cannot be determined the cache is **disabled**,
//! never assumed stale: guessing wrong fails *into* concurrent writers, which is the one
//! outcome this lock exists to prevent.

use serde::{de::DeserializeOwned, Serialize};
use std::path::{Path, PathBuf};

/// Who holds a lock, and what they published about themselves.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Holder<N> {
    pub pid: u32,
    pub dir: PathBuf,
    /// The holder's own note — a port for a server, a tmux pane for a TUI. `None` in the window
    /// between taking the lock and having something useful to say: a server has no port until
    /// it binds, so this is a real state, not a defensive `Option`.
    #[serde(default = "none")]
    pub note: Option<N>,
}

fn none<N>() -> Option<N> {
    None
}

pub fn lock_path(dir: &Path) -> PathBuf {
    dir.join("LOCK")
}

/// Whether a pid is alive. Unix-only: elsewhere there is no cheap check, and the caller
/// disables caching rather than guess (§9).
pub fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// Can this platform decide liveness at all? When it cannot, caching is disabled — assuming a
/// lock is stale would let two writers share an entry.
pub const fn liveness_decidable() -> bool {
    cfg!(unix)
}

/// The outcome of trying to take a session's lock.
pub enum Taken<N> {
    /// We own it. Released by [`release`], or reclaimed by the next process once this pid dies.
    Owned,
    /// A live process holds it. Nothing was opened.
    Held(Holder<N>),
}

/// Take the lock for `dir`, reclaiming it from a dead holder.
///
/// `alive` is injected so a frontend can add its own liveness signal — a server also port-probes,
/// since pids are recycled and a stale lock naming a recycled pid would otherwise look live
/// forever. Keeping it a callback is also what makes the lock testable without spawning
/// processes.
pub fn acquire<N: Serialize + DeserializeOwned + Clone>(
    dir: &Path,
    alive: impl Fn(&Holder<N>) -> bool,
) -> std::io::Result<Taken<N>> {
    std::fs::create_dir_all(dir)?;
    if let Some(h) = read::<N>(dir) {
        if h.pid != std::process::id() && alive(&h) {
            return Ok(Taken::Held(h));
        }
        // Ours already, or the holder is gone — reclaim.
    }
    write(
        dir,
        &Holder::<N> {
            pid: std::process::id(),
            dir: dir.to_path_buf(),
            note: None,
        },
    )?;
    Ok(Taken::Owned)
}

/// Publish this process's note for whoever finds the lock held. Separate from [`acquire`]
/// because the useful facts arrive later — a server has no port until it binds.
pub fn publish<N: Serialize + DeserializeOwned + Clone>(
    dir: &Path,
    note: N,
) -> std::io::Result<()> {
    let mut h = read::<N>(dir).unwrap_or(Holder {
        pid: std::process::id(),
        dir: dir.to_path_buf(),
        note: None,
    });
    h.pid = std::process::id();
    h.note = Some(note);
    write(dir, &h)
}

/// Release the lock **only if we hold it** — a process must never drop a peer's lock, which is
/// reachable when a reclaim raced.
pub fn release<N: Serialize + DeserializeOwned + Clone>(dir: &Path) {
    if let Some(h) = read::<N>(dir) {
        if h.pid == std::process::id() {
            let _ = std::fs::remove_file(lock_path(dir));
        }
    }
}

pub fn read<N: DeserializeOwned>(dir: &Path) -> Option<Holder<N>> {
    let s = std::fs::read_to_string(lock_path(dir)).ok()?;
    serde_json::from_str(&s).ok()
}

fn write<N: Serialize>(dir: &Path, h: &Holder<N>) -> std::io::Result<()> {
    std::fs::write(lock_path(dir), serde_json::to_string(h)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Note {
        port: u16,
    }

    fn tmp(n: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cr-lock-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }
    fn owned<N>(t: &Taken<N>) -> bool {
        matches!(t, Taken::Owned)
    }

    #[test]
    fn a_free_lock_is_taken_and_released() {
        let d = tmp("free");
        assert!(owned(&acquire::<Note>(&d, |_| true).unwrap()));
        assert!(read::<Note>(&d).is_some());
        release::<Note>(&d);
        assert!(read::<Note>(&d).is_none());
    }

    /// A LIVE holder denies, and nothing is opened. This is the invariant the two-outcome
    /// admission rests on.
    #[test]
    fn a_live_holder_denies() {
        let d = tmp("held");
        std::fs::create_dir_all(&d).unwrap();
        write(
            &d,
            &Holder::<Note> {
                pid: 999_999,
                dir: d.clone(),
                note: Some(Note { port: 8080 }),
            },
        )
        .unwrap();
        match acquire::<Note>(&d, |_| true).unwrap() {
            Taken::Held(h) => {
                assert_eq!(h.pid, 999_999);
                assert_eq!(h.note.unwrap().port, 8080, "the note reaches the peer");
            }
            Taken::Owned => panic!("must not steal a live holder's lock"),
        }
    }

    /// A DEAD holder's lock is reclaimed — otherwise a crashed process would pin a session
    /// forever.
    #[test]
    fn a_dead_holder_is_reclaimed() {
        let d = tmp("dead");
        std::fs::create_dir_all(&d).unwrap();
        write(
            &d,
            &Holder::<Note> {
                pid: 999_999,
                dir: d.clone(),
                note: None,
            },
        )
        .unwrap();
        assert!(owned(&acquire::<Note>(&d, |_| false).unwrap()));
        assert_eq!(read::<Note>(&d).unwrap().pid, std::process::id());
    }

    /// The note is published AFTER acquisition, because the useful facts arrive later.
    #[test]
    fn a_note_is_published_after_acquiring() {
        let d = tmp("note");
        acquire::<Note>(&d, |_| true).unwrap();
        assert!(read::<Note>(&d).unwrap().note.is_none(), "none until bound");
        publish(&d, Note { port: 4321 }).unwrap();
        assert_eq!(read::<Note>(&d).unwrap().note.unwrap().port, 4321);
    }

    /// Releasing must not drop a PEER's lock — reachable when a reclaim raced.
    #[test]
    fn release_never_drops_someone_elses_lock() {
        let d = tmp("peer");
        std::fs::create_dir_all(&d).unwrap();
        write(
            &d,
            &Holder::<Note> {
                pid: 999_999,
                dir: d.clone(),
                note: None,
            },
        )
        .unwrap();
        release::<Note>(&d);
        assert!(read::<Note>(&d).is_some(), "a peer's lock must survive");
    }
}
