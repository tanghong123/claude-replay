//! The per-directory slot lock: at most one tracked supervisor per project dir.
//! A port of claude-jdi's `acquire_slot`/`release_slot` — a `mkdir`-atomic lock
//! with an owner pidfile and stale-lock reclamation. Agent-neutral.

use super::state::pid_alive;
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Result of trying to take a directory's slot.
pub enum Acquire {
    /// Got the slot; hold this until done (releases on drop).
    Acquired(SlotLock),
    /// A live supervisor already owns this session.
    AlreadyRunning,
    /// Another `resume`/`start` is mid-setup for this dir right now.
    SetupInFlight,
}

/// A held slot lock. Dropping it removes the lock directory.
pub struct SlotLock {
    dir: PathBuf,
}

impl Drop for SlotLock {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.dir).ok();
    }
}

/// Try to acquire `<session_dir>/.lock`. `session_alive` reports whether a live
/// supervisor already owns the *session* (checked separately, because a running
/// supervisor holds no lock — it releases the lock after setup). Mirrors the two
/// distinct guards in claude-jdi's `acquire_slot`.
pub fn acquire(session_dir: &Path, session_alive: impl Fn() -> bool) -> Result<Acquire> {
    fs::create_dir_all(session_dir)
        .with_context(|| format!("create session dir {}", session_dir.display()))?;
    let lock = session_dir.join(".lock");
    let owner = lock.join("owner");

    loop {
        match fs::create_dir(&lock) {
            Ok(()) => break, // won the mkdir race
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Someone holds (or is mid-writing) the lock. Read the owner pid;
                // grace once for the write-after-mkdir window.
                let mut pid = read_owner(&owner);
                if pid.is_none() {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    pid = read_owner(&owner);
                }
                match pid {
                    Some(p) if pid_alive(p) => return Ok(Acquire::SetupInFlight),
                    _ => {
                        // Stale lock — reclaim and retry.
                        fs::remove_dir_all(&lock).ok();
                        continue;
                    }
                }
            }
            Err(e) => return Err(e).context("create slot lock"),
        }
    }

    // We hold the lock. A live supervisor runs lock-free, so "already running" is
    // tested here against the session's recorded pid.
    if session_alive() {
        fs::remove_dir_all(&lock).ok();
        return Ok(Acquire::AlreadyRunning);
    }
    fs::write(&owner, std::process::id().to_string())
        .with_context(|| format!("write lock owner {}", owner.display()))?;
    Ok(Acquire::Acquired(SlotLock { dir: lock }))
}

fn read_owner(owner: &Path) -> Option<u32> {
    fs::read_to_string(owner)
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "agent-jdi-lock-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::remove_dir_all(&d).ok();
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn acquire_then_second_setup_is_in_flight_until_released() {
        let dir = tmp().join("sess");
        // First acquire (no live supervisor) → Acquired.
        let held = match acquire(&dir, || false).unwrap() {
            Acquire::Acquired(l) => l,
            _ => panic!("first acquire should succeed"),
        };
        // A concurrent setup (owner = our live pid) → SetupInFlight.
        assert!(matches!(
            acquire(&dir, || false).unwrap(),
            Acquire::SetupInFlight
        ));
        drop(held); // release
                    // After release, the slot is free again.
        assert!(matches!(
            acquire(&dir, || false).unwrap(),
            Acquire::Acquired(_)
        ));
        fs::remove_dir_all(dir.parent().unwrap()).ok();
    }

    #[test]
    fn live_supervisor_reports_already_running() {
        let dir = tmp().join("sess");
        // No lock held, but the session reports a live supervisor → AlreadyRunning.
        assert!(matches!(
            acquire(&dir, || true).unwrap(),
            Acquire::AlreadyRunning
        ));
        fs::remove_dir_all(dir.parent().unwrap()).ok();
    }

    #[test]
    fn stale_lock_with_dead_owner_is_reclaimed() {
        let dir = tmp().join("sess");
        let lock = dir.join(".lock");
        fs::create_dir_all(&lock).unwrap();
        fs::write(lock.join("owner"), "999999999").unwrap(); // dead pid
        assert!(matches!(
            acquire(&dir, || false).unwrap(),
            Acquire::Acquired(_)
        ));
        fs::remove_dir_all(dir.parent().unwrap()).ok();
    }
}
