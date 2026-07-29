//! **The HTML frontend's [`BlockStore`] (#74)** — the presentation layer deciding what lives in
//! the `Session`'s `BV`: here, a [`RecordLocator`] into `<id>.records`, the append-only log of
//! **rendered wire-format JSON records**. `put` renders each committed block to its record
//! exactly once *as it commits* (the render-once side effect) and returns the locator — so the
//! `Session` itself captures the session in the form this presentation serves, `/pull` answers
//! committed zones as `{offset, len}` pointers clients range-read directly, and nothing is
//! stored twice (this replaces the former tier-b `.blocks` + `PullRender` double log).
//!
//! Read-back is deliberately impossible: a wire record is a one-way projection of its `Block`,
//! so this store implements [`BlockStore`] but NOT `BlockRead` — the type system then keeps
//! every committed consumer on the pointer path.

use super::{render_blocks, EmitState};
use crate::cache::PersistentStore;
use crate::engine::BlockStore;
use crate::fold::FoldPolicy;
use crate::model::{Block, BlockIndex, EpochSeconds};
use crate::Transcript;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Locator of one rendered record in the log: the record's bytes are
/// `log[offset .. offset + len]` (excluding the framing newline).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RecordLocator {
    pub offset: u64,
    pub len: u32,
}

/// What `put` needs to render: the serve run's presentation parameters plus the resumable
/// [`EmitState`] (anchors/turn numbering follow on across ranges). Absent on a
/// [`reopen`](PersistentStore::reopen)ed store until [`restore_state`] rehydrates the emit
/// state — a hibernated body never `put`s, but its emit state still seeds the open-turn render.
struct RenderCx {
    fold: FoldPolicy,
    cwd: String,
    transcript: Transcript,
}

/// The wire-record log backing. `file` is `None` on a reopened (hibernated) store — reads open
/// the path per call, so a read-only store holds no handle.
struct Log {
    path: PathBuf,
    file: Option<std::fs::File>,
    len: u64,
}

pub struct RecordStore {
    log: Log,
    cx: Option<RenderCx>,
    emit: EmitState,
}

impl RecordStore {
    /// Create (truncating) the record log at `path` with this serve run's render parameters.
    pub(crate) fn create(
        path: &Path,
        fold: FoldPolicy,
        cwd: String,
        transcript: Transcript,
    ) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?;
        Ok(Self {
            log: Log {
                path: path.to_path_buf(),
                file: Some(file),
                len: 0,
            },
            cx: Some(RenderCx {
                fold,
                cwd,
                transcript,
            }),
            emit: EmitState::default(),
        })
    }

    /// Current log length (EOF) — the end bound of a committed `{offset, len}` pointer.
    pub(crate) fn log_len(&self) -> u64 {
        self.log.len
    }

    /// The render continuation as of the committed frontier — the open turn renders each poll
    /// from a CLONE of this (its ephemeral anchors never pollute the committed state).
    pub(crate) fn emit_snapshot(&self) -> EmitState {
        self.emit.clone()
    }

    /// `[start, end)` bytes off the log — the `/records` range read. Opens the path per call
    /// (stateless read), so live and reopened stores serve identically; empty on any I/O error
    /// or empty range (the committed zone is then simply absent from that reply).
    pub(crate) fn read_range(&self, start: u64, end: u64) -> Vec<u8> {
        let end = end.min(self.log.len);
        if end <= start {
            return Vec::new();
        }
        match std::fs::File::open(&self.log.path) {
            Ok(mut f) => {
                let mut buf = vec![0u8; (end - start) as usize];
                if f.seek(SeekFrom::Start(start)).is_ok() && f.read_exact(&mut buf).is_ok() {
                    buf
                } else {
                    Vec::new()
                }
            }
            Err(_) => Vec::new(),
        }
    }
}

impl BlockStore for RecordStore {
    type Bv = RecordLocator;

    /// Render-once TO DISK: one wire record per committed block, appended with its framing
    /// newline; the locator is the `Bv` the `Session` holds. `user_times` is the session's
    /// per-turn clock — `EmitState::seen_turns` indexes into it exactly as the whole-session
    /// render does, so the record bytes are identical to a batch export's.
    fn put(
        &mut self,
        b: Block,
        _at: BlockIndex,
        user_times: &[Option<EpochSeconds>],
    ) -> RecordLocator {
        let cx = self
            .cx
            .as_ref()
            .expect("a reopened record store never puts");
        let lines = render_blocks(
            &[b],
            user_times,
            &cx.fold,
            &cx.cwd,
            true,
            true,
            None,
            Some(&cx.transcript),
            &mut self.emit,
        );
        debug_assert_eq!(lines.len(), 1, "one wire record per block");
        let rec = &lines[0];
        let locator = RecordLocator {
            offset: self.log.len,
            len: rec.len() as u32,
        };
        if let Some(f) = &mut self.log.file {
            let _ = f.write_all(rec.as_bytes());
            let _ = f.write_all(b"\n");
        }
        self.log.len += rec.len() as u64 + 1;
        locator
    }

    /// A source truncation rebuilt the session: restart the log (truncate + rewind) and the
    /// render continuation. The epoch bump that accompanies the reset resyncs every client, so
    /// no outstanding pointer can reference the discarded bytes.
    fn reset(&mut self) {
        if let Some(f) = &mut self.log.file {
            let _ = f.set_len(0);
            let _ = f.seek(SeekFrom::Start(0));
        }
        self.log.len = 0;
        self.emit = EmitState::default();
    }
}

impl PersistentStore for RecordStore {
    fn backing_len(&self) -> u64 {
        self.log.len
    }

    /// Read-only reopen for a hibernated body: no write handle, no render context — it serves
    /// range reads and (after [`restore_state`](PersistentStore::restore_state) rehydrates the
    /// emit state) seeds the open-turn render. It never `put`s: a changed source refolds fresh.
    fn reopen(backing: &Path) -> std::io::Result<Self> {
        let len = std::fs::metadata(backing)?.len();
        Ok(Self {
            log: Log {
                path: backing.to_path_buf(),
                file: None,
                len,
            },
            cx: None,
            emit: EmitState::default(),
        })
    }

    fn hibernate_state(&self) -> serde_json::Value {
        serde_json::to_value(&self.emit).unwrap_or(serde_json::Value::Null)
    }

    fn restore_state(&mut self, v: serde_json::Value) {
        if let Ok(emit) = serde_json::from_value(v) {
            self.emit = emit;
        }
    }
}
