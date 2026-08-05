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
use crate::cache::DurableStore;
use crate::engine::{BlockStore, SessionMeta};
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

/// What `put` needs to render: the serve run's presentation parameters. The resumable
/// [`EmitState`] (anchors/turn numbering follow on across ranges) sits beside it on the store.
struct RenderCx {
    fold: FoldPolicy,
    cwd: String,
    transcript: Transcript,
}

/// The wire-record log backing. Reads open the path per call (stateless), so a range read never
/// contends with the append handle.
struct Log {
    path: PathBuf,
    file: std::fs::File,
    len: u64,
}

pub struct RecordStore {
    log: Log,
    cx: RenderCx,
    emit: EmitState,
}

impl RecordStore {
    /// Open the log, keeping what is there.
    ///
    /// There is no truncating constructor: a caller that wants a fresh log calls
    /// [`reset`](BlockStore::reset), and a durable open MUST be able to read the existing log
    /// before deciding whether to keep it (#96) — a constructor that truncated would destroy the
    /// evidence the decision rests on.
    pub(crate) fn open_append(
        path: &Path,
        fold: FoldPolicy,
        cwd: String,
        transcript: Transcript,
    ) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        let len = file.metadata()?.len();
        Ok(Self {
            log: Log {
                path: path.to_path_buf(),
                file,
                len,
            },
            cx: RenderCx {
                fold,
                cwd,
                transcript,
            },
            emit: EmitState::default(),
        })
    }

    /// Cut the log to `len` bytes and leave the handle at the new end.
    fn cut_to(&mut self, len: u64) -> std::io::Result<()> {
        self.log.file.set_len(len)?;
        self.log.file.seek(SeekFrom::End(0))?;
        self.log.len = len;
        Ok(())
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
        let cx = &self.cx;
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
        let _ = self.log.file.write_all(rec.as_bytes());
        let _ = self.log.file.write_all(b"\n");
        self.log.len += rec.len() as u64 + 1;
        locator
    }

    /// A source truncation rebuilt the session: restart the log (truncate + rewind) and the
    /// render continuation. The epoch bump that accompanies the reset resyncs every client, so
    /// no outstanding pointer can reference the discarded bytes.
    fn reset(&mut self) {
        let _ = self.cut_to(0);
        self.emit = EmitState::default();
    }
}

impl DurableStore for RecordStore {
    type Note = HtmlNote;

    /// Rebuild the committed locator table by walking the log's framing newlines.
    ///
    /// A **torn trailing record** is dropped *and* cut, so `log_len` stays an honest append
    /// offset: appending after a fragment would splice a new record onto half an old one and
    /// every locator past it would address garbage.
    fn load(&mut self) -> std::io::Result<Vec<RecordLocator>> {
        let mut buf = Vec::new();
        match std::fs::File::open(&self.log.path) {
            Ok(mut f) => f.read_to_end(&mut buf)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut out = Vec::new();
        let mut at = 0u64;
        for line in buf.split_inclusive(|b| *b == b'\n') {
            if !line.ends_with(b"\n") {
                break; // the writer died mid-append
            }
            out.push(RecordLocator {
                offset: at,
                len: line.len() as u32 - 1, // the locator excludes the framing newline
            });
            at += line.len() as u64;
        }
        if at != self.log.len || at != buf.len() as u64 {
            self.cut_to(at)?;
        }
        Ok(out)
    }

    /// Cut the log to `n` records and **derive** the render continuation from the prefix that
    /// survives (#96 §4.3: no presentation state is persisted).
    ///
    /// All three counters are facts about the prefix, so deriving them cannot go stale against
    /// it the way a persisted copy could: one record per committed block makes `next_block` the
    /// block count, and both turn counters advance once per `UserText`/`Command`, which is
    /// exactly what the header's `turns` counts. The sidebar accumulator starts empty because
    /// the live path never reads it — a served page builds its turn index from the records
    /// themselves; only the static bundle, which never resumes, consumes it.
    fn adopt(&mut self, n: usize, meta: &SessionMeta) -> std::io::Result<()> {
        let end = self.load()?.get(n).map(|l| l.offset);
        if let Some(end) = end {
            self.cut_to(end)?;
        }
        self.emit = EmitState::resumed(n, meta.turns);
        Ok(())
    }
}

/// What a second server finds when it discovers this one already holds a session (#96 §8.4):
/// where to go instead. `None` until the listener binds — a real state, not a defensive option.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HtmlNote {
    pub port: u16,
}
