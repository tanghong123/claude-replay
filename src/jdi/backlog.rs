//! The crash-safe backlog queue: `pending/ → draining/ → drained/`. Items are
//! 4-digit zero-padded files. A port of claude-jdi's queue mechanics — agent-neutral.
//!
//! - `pending/`  — queued, not yet claimed.
//! - `draining/` — claimed for an in-flight drain; reclaimed if the run dies.
//! - `drained/`  — confirmed done, only after a clean execute turn.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

pub struct Backlog {
    root: PathBuf,
}

impl Backlog {
    /// `root` is the session's `backlog/` directory.
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn pending(&self) -> PathBuf {
        self.root.join("pending")
    }
    fn draining(&self) -> PathBuf {
        self.root.join("draining")
    }
    fn drained(&self) -> PathBuf {
        self.root.join("drained")
    }

    /// Next 4-digit sequence: one past the max across all three sub-queues, so
    /// ordering is stable even after items move between them.
    pub fn next_seq(&self) -> u32 {
        let mut max = 0u32;
        for d in [self.pending(), self.draining(), self.drained()] {
            if let Ok(entries) = fs::read_dir(&d) {
                for e in entries.flatten() {
                    if let Some(n) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) {
                        max = max.max(n);
                    }
                }
            }
        }
        max + 1
    }

    /// Enqueue `text` as a new `pending/NNNN` item; returns its path.
    pub fn add(&self, text: &str) -> Result<PathBuf> {
        let pending = self.pending();
        fs::create_dir_all(&pending).with_context(|| format!("create {}", pending.display()))?;
        let path = pending.join(format!("{:04}", self.next_seq()));
        fs::write(&path, text).with_context(|| format!("write {}", path.display()))?;
        Ok(path)
    }

    fn count_in(dir: &Path) -> usize {
        fs::read_dir(dir)
            .map(|it| it.flatten().count())
            .unwrap_or(0)
    }

    pub fn pending_count(&self) -> usize {
        Self::count_in(&self.pending())
    }
    pub fn draining_count(&self) -> usize {
        Self::count_in(&self.draining())
    }
    pub fn drained_count(&self) -> usize {
        Self::count_in(&self.drained())
    }

    /// Claim work to drain: move every `pending/*` into `draining/`, and also pick
    /// up any `draining/*` left over from an interrupted run. Returns the claimed
    /// items' contents (ordered by name). Empty ⇒ nothing to drain.
    pub fn claim(&self) -> Result<Vec<String>> {
        let (pending, draining) = (self.pending(), self.draining());
        fs::create_dir_all(&draining).with_context(|| format!("create {}", draining.display()))?;
        if let Ok(entries) = fs::read_dir(&pending) {
            for e in entries.flatten() {
                let dest = draining.join(e.file_name());
                fs::rename(e.path(), dest).ok();
            }
        }
        self.read_dir_sorted(&draining)
    }

    /// Finalize a clean drain: move every `draining/*` into `drained/`. Called only
    /// after a clean execute turn (the "confirm on success" guard).
    pub fn finalize(&self) -> Result<()> {
        let (draining, drained) = (self.draining(), self.drained());
        fs::create_dir_all(&drained).with_context(|| format!("create {}", drained.display()))?;
        if let Ok(entries) = fs::read_dir(&draining) {
            for e in entries.flatten() {
                let dest = drained.join(e.file_name());
                fs::rename(e.path(), dest).ok();
            }
        }
        Ok(())
    }

    fn read_dir_sorted(&self, dir: &Path) -> Result<Vec<String>> {
        let mut items: Vec<(String, PathBuf)> = match fs::read_dir(dir) {
            Ok(entries) => entries
                .flatten()
                .map(|e| (e.file_name().to_string_lossy().to_string(), e.path()))
                .collect(),
            Err(_) => return Ok(Vec::new()),
        };
        items.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(items
            .into_iter()
            .filter_map(|(_, p)| fs::read_to_string(p).ok())
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "agent-jdi-backlog-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::remove_dir_all(&d).ok();
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn pending_to_draining_to_drained() {
        let root = tmp().join("backlog");
        let b = Backlog::new(root.clone());
        b.add("first").unwrap();
        b.add("second").unwrap();
        assert_eq!(b.pending_count(), 2);

        let claimed = b.claim().unwrap();
        assert_eq!(claimed, vec!["first", "second"], "claimed in order");
        assert_eq!(b.pending_count(), 0);
        assert_eq!(b.draining_count(), 2);

        // A re-claim before finalize picks up the still-draining items (crash-safe).
        assert_eq!(b.claim().unwrap().len(), 2);

        b.finalize().unwrap();
        assert_eq!(b.draining_count(), 0);
        assert_eq!(b.drained_count(), 2);

        fs::remove_dir_all(root.parent().unwrap()).ok();
    }

    #[test]
    fn next_seq_increments_across_all_queues() {
        let root = tmp().join("backlog");
        let b = Backlog::new(root.clone());
        b.add("a").unwrap(); // 0001
        b.claim().unwrap(); // → draining/0001
        b.add("b").unwrap(); // 0002 (max across queues + 1)
                             // Names are stable: 0001 in draining, 0002 in pending.
        assert!(root.join("draining/0001").exists());
        assert!(root.join("pending/0002").exists());
        fs::remove_dir_all(root.parent().unwrap()).ok();
    }
}
