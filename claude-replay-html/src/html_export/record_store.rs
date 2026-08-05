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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::{Admission, Holder, Presentation, SessionCache};
    use crate::engine::meta_stream::Versions;
    use crate::Agent;
    use std::sync::atomic::{AtomicUsize, Ordering};

    type Cache = SessionCache<RecordStore, ()>;

    fn tmp(n: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let d = std::env::temp_dir().join(format!(
            "cr-recdur-{}-{n}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn user(t: &str, s: u32) -> String {
        format!("{{\"type\":\"user\",\"cwd\":\"/r\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"{t}\"}}]}},\"timestamp\":\"2026-07-26T10:00:{s:02}Z\"}}\n")
    }
    fn asst(t: &str, s: u32) -> String {
        format!("{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"{t}\"}}],\"usage\":{{\"input_tokens\":5,\"output_tokens\":8}}}},\"timestamp\":\"2026-07-26T10:00:{s:02}Z\"}}\n")
    }
    fn tool(id: &str, s: u32) -> String {
        format!("{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"{id}\",\"name\":\"Bash\",\"input\":{{\"command\":\"ls\"}}}}]}},\"timestamp\":\"2026-07-26T10:00:{s:02}Z\"}}\n")
    }
    fn result(id: &str, s: u32) -> String {
        format!("{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"{id}\",\"content\":\"out\"}}]}},\"timestamp\":\"2026-07-26T10:00:{s:02}Z\"}}\n")
    }

    fn write_transcript(p: &Path, turns: usize) {
        let mut s = String::new();
        for i in 0..turns {
            let t = (i * 4) as u32;
            s.push_str(&user(&format!("ask {i}"), t));
            s.push_str(&tool(&format!("b{i}"), t + 1));
            s.push_str(&result(&format!("b{i}"), t + 2));
            s.push_str(&asst(&format!("reply {i}"), t + 3));
        }
        std::fs::write(p, s).unwrap();
    }

    fn append(p: &Path, s: &str) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(p).unwrap();
        f.write_all(s.as_bytes()).unwrap();
    }

    fn cache(root: &Path) -> Cache {
        Cache::durable(
            Presentation::Html,
            root.to_path_buf(),
            Versions::current(Some(7)),
        )
    }

    /// Open through the real API and fold to EOF; returns the record log's bytes.
    fn open_and_fold(c: &Cache, root: &Path, src: &Path) -> (String, crate::cache::admit::Origin) {
        c.register(
            "s",
            crate::Transcript::open(Agent::CLAUDE, src.to_path_buf()),
        );
        let fold = crate::fold::FoldPolicy::default();
        let srcp = src.to_path_buf();
        let origin = match c.admit(
            "s",
            move |dir| {
                RecordStore::open_append(
                    &dir.join("records.jsonl"),
                    fold.clone(),
                    "/r".into(),
                    crate::Transcript::open(Agent::CLAUDE, srcp.clone()),
                )
            },
            |_: &Holder<HtmlNote>| false,
        ) {
            Admission::Owned { session, origin } => {
                let _ = session.advance();
                origin
            }
            Admission::Denied(_) => panic!("a free entry must be Owned"),
        };
        let log =
            crate::cache::admit::entry_dir(root, Presentation::Html, "s").join("records.jsonl");
        (std::fs::read_to_string(log).unwrap_or_default(), origin)
    }

    /// **The HTML half of the resume oracle** (#96 R5). The wire records a resumed server
    /// writes must be byte-identical to a cold run's — which is really a test of the derived
    /// `EmitState`: `next_block` (the `b{n}` anchors) and the two turn counters are *computed*
    /// from the restored prefix rather than persisted with it, and nothing else would catch a
    /// wrong derivation. The byte gate cannot: its corpus never resumes.
    #[test]
    fn a_resumed_record_log_is_byte_identical_to_a_cold_one() {
        // Cold reference: one run over the whole transcript.
        let root_a = tmp("cold");
        let src_a = root_a.join("t.jsonl");
        write_transcript(&src_a, 6);
        let cold = {
            let c = cache(&root_a);
            let (log, origin) = open_and_fold(&c, &root_a, &src_a);
            assert!(matches!(origin, crate::cache::admit::Origin::Cold(_)));
            c.release_all();
            log
        };
        assert!(cold.lines().count() > 5, "the fixture must commit records");

        // Split run: fold 4 turns, drop the process, then resume and fold the rest.
        let root_b = tmp("resumed");
        let src_b = root_b.join("t.jsonl");
        write_transcript(&src_b, 4);
        {
            let c = cache(&root_b);
            open_and_fold(&c, &root_b, &src_b);
            c.release_all();
        }
        for i in 4..6 {
            let t = (i * 4) as u32;
            append(&src_b, &user(&format!("ask {i}"), t));
            append(&src_b, &tool(&format!("b{i}"), t + 1));
            append(&src_b, &result(&format!("b{i}"), t + 2));
            append(&src_b, &asst(&format!("reply {i}"), t + 3));
        }
        let c = cache(&root_b);
        let (resumed, origin) = open_and_fold(&c, &root_b, &src_b);
        assert!(
            matches!(origin, crate::cache::admit::Origin::Resumed { .. }),
            "the second run must resume, got {origin:?}"
        );
        assert_eq!(
            resumed, cold,
            "a resumed record log must equal a cold one, byte for byte"
        );
    }

    /// A changed render flavor (a different fold policy) must REBUILD, not resume: the records
    /// already on disk were rendered under the old one, and splicing the two would leave a page
    /// whose halves disagree about what is folded.
    #[test]
    fn a_changed_render_flavor_rebuilds() {
        let root = tmp("flavor");
        let src = root.join("t.jsonl");
        write_transcript(&src, 3);
        {
            let c = cache(&root);
            open_and_fold(&c, &root, &src);
            c.release_all();
        }
        let c = Cache::durable(
            Presentation::Html,
            root.clone(),
            Versions::current(Some(99)), // a different render fingerprint
        );
        let (_, origin) = open_and_fold(&c, &root, &src);
        assert_eq!(
            origin,
            crate::cache::admit::Origin::Cold(crate::cache::ColdReason::VersionChanged)
        );
    }
}
