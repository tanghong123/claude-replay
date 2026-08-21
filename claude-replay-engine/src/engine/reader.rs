//! Byte-offset incremental **line reader** for a JSONL file. Poll-driven (no threads).
//!
//! Two capabilities:
//! - **Tail** — `poll()` returns the complete lines appended since the last poll, buffering a
//!   trailing partial until its newline arrives, and recovering from truncation/rewrite
//!   (compaction) by detecting a shrunk file and re-reading from 0 (`reset`).
//! - **Resume** — [`open_at_offset`](LineReader::open_at_offset) starts reading at a byte offset,
//!   so a restored fold reads only the bytes above its resume point. Validating that offset is
//!   deliberately **not** the reader's job (#96): the durable cache checks the source window
//!   before it ever constructs a reader, and a reader that re-hashed the prefix on every resume
//!   would spend exactly what the resume exists to save.
//!
//! Adapted in spirit from claude-code-scrollback (MIT, © 2026 pjh4993): buffer a trailing
//! partial line until its newline arrives, and recover from truncation/rewrite by detecting a
//! shrunk file and re-reading.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

pub struct LineReader {
    path: PathBuf,
    offset: crate::model::ByteOffset,
    pending: String,
}

#[derive(Default)]
pub struct Poll {
    /// Complete new lines (no trailing newline) since the last poll.
    pub lines: Vec<String>,
    /// The **start byte offset** in the file of each line in `lines` (parallel, same length) —
    /// so the follower can stamp attachment locators without re-deriving positions.
    pub offsets: Vec<crate::model::ByteOffset>,
    /// True if a truncation/rewrite was detected and we re-read from 0 — `lines` then holds the
    /// whole current file.
    pub reset: bool,
}

impl LineReader {
    /// Start reading new bytes written *after* the current end of the file. (A general tailing
    /// primitive; production follows from the start via `FollowParser`, but this remains for a
    /// "tail only new output" caller and the tail tests.)
    #[allow(dead_code)]
    pub fn open_at_end(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let offset = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        Self {
            path,
            offset,
            pending: String::new(),
        }
    }

    /// Start reading from the BEGINNING — the first `poll` returns the whole current file, then
    /// subsequent polls return only appends. Used by the incremental follower (M16), which folds
    /// the file once through a persistent `Replayer` and then only the delta.
    pub fn open_at_start(path: impl Into<PathBuf>) -> Self {
        Self::open_at_offset(path, 0)
    }

    /// Start reading at `offset` — the resume entry point (#96). The first `poll` returns the
    /// lines at or above it, stamped with their true file offsets, so attachment locators in a
    /// resumed session match a cold parse's exactly.
    ///
    /// The caller vouches for `offset`. A file that has since shrunk below it is still caught —
    /// `poll` sees the shrink and resets — but a file whose bytes were *rewritten* in place is
    /// not, which is what the durable cache's window CRC is for.
    pub fn open_at_offset(path: impl Into<PathBuf>, offset: crate::model::ByteOffset) -> Self {
        Self {
            path: path.into(),
            offset,
            pending: String::new(),
        }
    }

    /// Reset to a from-scratch read (on a detected truncation/rewrite).
    fn reset_state(&mut self) {
        self.offset = 0;
        self.pending.clear();
    }

    /// Fold `buf` (the bytes just read, starting at the current `offset`) into the running
    /// state: advance the offset + rolling hash, split off complete lines into `out`, keep a
    /// trailing partial in `pending`.
    fn consume(&mut self, buf: &[u8], out: &mut Poll) {
        // File offset of `pending`'s first byte: bytes consumed so far, minus the held partial.
        // (Assumes valid UTF-8 — `pending` is built via lossy decode; JSONL transcripts are UTF-8.)
        let base = self.offset - self.pending.len() as u64;
        self.offset += buf.len() as u64;
        self.pending.push_str(&String::from_utf8_lossy(buf));
        let ends_newline = self.pending.ends_with('\n');
        let combined = std::mem::take(&mut self.pending);
        let mut parts: Vec<&str> = combined.split('\n').collect();
        let rest = if ends_newline {
            parts.pop(); // trailing "" after the final newline
            String::new()
        } else {
            parts.pop().unwrap_or("").to_string()
        };
        let mut pos = 0u64; // byte position of the current part's start within `combined`
        for p in parts {
            if !p.is_empty() {
                out.offsets.push(base + pos);
                out.lines.push(p.to_string());
            }
            pos += p.len() as u64 + 1; // + the '\n' that split consumed
        }
        self.pending = rest;
    }

    /// Read any bytes appended since the last poll, returning complete lines.
    pub fn poll(&mut self) -> std::io::Result<Poll> {
        let mut out = Poll::default();
        let mut f = match File::open(&self.path) {
            Ok(f) => f,
            Err(_) => return Ok(out),
        };
        let len = f.metadata()?.len();
        if len < self.offset {
            // File shrank → truncation/rewrite. Re-read from the top.
            self.reset_state();
            out.reset = true;
        }
        if len == self.offset {
            return Ok(out);
        }
        f.seek(SeekFrom::Start(self.offset))?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        self.consume(&buf, &mut out);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("peekv2-reader-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn reads_appended_lines_and_buffers_partials() {
        let p = tmp("t.jsonl");
        std::fs::write(&p, b"{\"a\":1}\n").unwrap();

        let mut t = LineReader::open_at_end(&p); // start at end → nothing yet
        assert!(t.poll().unwrap().lines.is_empty());

        // Append one whole line + one partial.
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        write!(f, "{{\"b\":2}}\n{{\"c\"").unwrap();
        let r = t.poll().unwrap();
        assert_eq!(r.lines, vec!["{\"b\":2}".to_string()]); // partial held back

        // Complete the partial.
        writeln!(f, ":3}}").unwrap();
        let r = t.poll().unwrap();
        assert_eq!(r.lines, vec!["{\"c\":3}".to_string()]);

        std::fs::remove_file(&p).ok();
    }

    /// A reader resumed at a byte offset sees exactly the delta an uninterrupted reader sees —
    /// and stamps the SAME absolute file offsets, which is what keeps a resumed session's
    /// attachment locators identical to a cold parse's.
    #[test]
    fn resuming_at_an_offset_equals_an_uninterrupted_read() {
        let p = tmp("resume.jsonl");
        std::fs::write(&p, b"{\"a\":1}\n{\"b\":2}\n").unwrap();

        let mut r1 = LineReader::open_at_start(&p);
        assert_eq!(r1.poll().unwrap().lines, vec!["{\"a\":1}", "{\"b\":2}"]);
        let at = std::fs::metadata(&p).unwrap().len();

        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        write!(f, "{{\"c\":3}}\n{{\"d\":4}}\n").unwrap();

        let mut r2 = LineReader::open_at_offset(&p, at);
        let got = r2.poll().unwrap();
        assert!(!got.reset, "a resume is not a reset");
        assert_eq!(got.lines, vec!["{\"c\":3}", "{\"d\":4}"]);

        let cont = r1.poll().unwrap();
        assert_eq!(got.lines, cont.lines);
        assert_eq!(got.offsets, cont.offsets, "absolute offsets, not relative");

        std::fs::remove_file(&p).ok();
    }

    /// A file that SHRANK below the resume offset is still caught here — the one rewrite shape
    /// the reader can see for free. An in-place rewrite that keeps the length is deliberately
    /// NOT this layer's job: the durable cache's window CRC covers it, and re-hashing the prefix
    /// on every resume would cost exactly what the resume saves.
    #[test]
    fn a_source_shrunk_below_the_resume_offset_resets() {
        let p = tmp("shrunk.jsonl");
        std::fs::write(&p, b"{\"a\":1}\n{\"b\":2}\n{\"c\":3}\n").unwrap();
        let at = std::fs::metadata(&p).unwrap().len();

        std::fs::write(&p, b"{\"x\":9}\n").unwrap(); // compacted away

        let mut r = LineReader::open_at_offset(&p, at);
        let got = r.poll().unwrap();
        assert!(got.reset, "a shrunk source must reset");
        assert_eq!(got.lines, vec!["{\"x\":9}"]);

        std::fs::remove_file(&p).ok();
    }
}

// ─── The batch driver over the eliding primitive (#193) ─────────────────────────────────

use crate::engine::elide::{read_line_elided, Elision, LineOutcome};
use std::io::BufRead;

/// What to do with a final line that has no newline yet.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TornTail {
    /// A live file may be mid-append: stop before the incomplete line and leave the cursor
    /// on the last complete one. (A durable cursor — the metrics fold's, the follower's —
    /// requires this.)
    Stop,
    /// The last line is all there is: yield it, cursor advanced past it. (The one-shot
    /// whole-file folds.)
    Yield,
}

/// The elision gauges (`design/bounded-line-reads.md` §11.3) — reported per fold; the
/// block/metrics paths bank them into `Metrics::extra`.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ElisionCounts {
    /// Lines that had at least one value elided.
    pub elided_lines: u64,
    /// Total bytes dropped by elision.
    pub elided_bytes: u64,
    /// Lines consumed and dropped whole by the [`ELIDE_CEILING`](crate::engine::elide::ELIDE_CEILING)
    /// — genuine data loss, and the gauge that should read loudest.
    pub skipped_lines: u64,
}

/// One line source for every whole-file loop (#193): raw-offset accounting, torn-tail
/// policy, blank skipping and elision — each written once, wrapping the one byte-touching
/// primitive. The offset invariant is structural here: `offset` advances by the raw
/// `read_line_elided` count inside the source, before any caller sees the line, and the
/// line handed out is already elided — a caller cannot elide first and count second,
/// because a caller never counts at all.
pub struct LineSource<R> {
    reader: R,
    offset: crate::model::ByteOffset,
    tail: TornTail,
    policy: Elision,
    out: Vec<u8>,
    /// The gauges, accumulated across the source's lifetime.
    pub elided: ElisionCounts,
}

impl<R: BufRead> LineSource<R> {
    /// A source whose next byte is at absolute file offset `at` — the caller aligns the
    /// reader and the offset (a fresh open at 0, or a seek + resume offset).
    pub fn new(reader: R, at: crate::model::ByteOffset, tail: TornTail, policy: Elision) -> Self {
        LineSource {
            reader,
            offset: at,
            tail,
            policy,
            out: Vec::new(),
            elided: ElisionCounts::default(),
        }
    }

    /// The next non-blank line as `(start offset, elided body)` — the body without its
    /// trailing newline — or `None` at EOF, or at a torn tail under [`TornTail::Stop`]
    /// (the cursor then stays on the last complete line). Ceiling-skipped lines are
    /// consumed and counted, never yielded. A lending iterator: the `&str` borrows until
    /// the next call — which is why this is not `Iterator::next` (that trait cannot lend).
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> std::io::Result<Option<(crate::model::ByteOffset, &str)>> {
        loop {
            let start = self.offset;
            match read_line_elided(&mut self.reader, &mut self.out, start, self.policy)? {
                LineOutcome::Eof => return Ok(None),
                LineOutcome::Torn { raw_len } => match self.tail {
                    TornTail::Stop => return Ok(None),
                    TornTail::Yield => {
                        self.offset += raw_len;
                        return Ok(Some((start, self.body())));
                    }
                },
                LineOutcome::Complete {
                    raw_len,
                    elided,
                    skipped,
                } => {
                    self.offset += raw_len;
                    if skipped {
                        self.elided.skipped_lines += 1;
                        continue;
                    }
                    if elided > 0 {
                        self.elided.elided_lines += 1;
                        self.elided.elided_bytes += elided;
                    }
                    if self.trimmed_len() == 0 {
                        continue; // blank — every consumer skips these (or excuses them)
                    }
                    return Ok(Some((start, self.body())));
                }
            }
        }
    }

    /// The offset of the next unread line — the durable cursor.
    pub fn offset(&self) -> crate::model::ByteOffset {
        self.offset
    }

    /// Reposition the underlying reader to [`offset`](Self::offset). Needed only by the
    /// live tails (the metrics fold, the follower), which keep their reader open across
    /// polls: discovering a torn tail consumes its bytes, so before the next poll the
    /// reader must be walked back to the cursor — the same seek today's metrics fold does
    /// inline, under a name.
    pub fn rewind_to_cursor(&mut self) -> std::io::Result<()>
    where
        R: std::io::Seek,
    {
        self.reader.seek(std::io::SeekFrom::Start(self.offset))?;
        Ok(())
    }

    fn trimmed_len(&self) -> usize {
        let mut b: &[u8] = &self.out;
        if b.last() == Some(&b'\n') {
            b = &b[..b.len() - 1];
        }
        if b.last() == Some(&b'\r') {
            b = &b[..b.len() - 1];
        }
        b.iter().filter(|c| !c.is_ascii_whitespace()).count()
    }

    /// The current line's body, newline-stripped. Invalid UTF-8 (never produced by an
    /// agent, but the reader must not panic on a corrupt store) is patched lossily in
    /// place; the happy path borrows without copying.
    fn body(&mut self) -> &str {
        let mut end = self.out.len();
        if end > 0 && self.out[end - 1] == b'\n' {
            end -= 1;
        }
        if end > 0 && self.out[end - 1] == b'\r' {
            end -= 1;
        }
        self.out.truncate(end);
        if std::str::from_utf8(&self.out).is_err() {
            let patched = String::from_utf8_lossy(&self.out).into_owned();
            self.out = patched.into_bytes();
        }
        std::str::from_utf8(&self.out).expect("just patched")
    }
}

#[cfg(test)]
mod line_source_tests {
    use super::*;
    use std::io::{Cursor, Seek, SeekFrom};

    #[test]
    fn offsets_count_raw_bytes_across_lines() {
        let data = "{\"a\":1}\n\n{\"b\":2}\n";
        let mut src = LineSource::new(
            Cursor::new(data.as_bytes().to_vec()),
            0,
            TornTail::Yield,
            Elision::Aggressive,
        );
        let (at, line) = src.next().unwrap().unwrap();
        assert_eq!((at, line), (0, "{\"a\":1}"));
        // The blank line is skipped but its bytes are counted.
        let (at, line) = src.next().unwrap().unwrap();
        assert_eq!((at, line), (9, "{\"b\":2}"));
        assert!(src.next().unwrap().is_none());
        assert_eq!(src.offset(), data.len() as u64);
    }

    #[test]
    fn stop_leaves_the_cursor_on_the_last_complete_line() {
        let data = "{\"a\":1}\n{\"torn\":";
        let mut src = LineSource::new(
            Cursor::new(data.as_bytes().to_vec()),
            0,
            TornTail::Stop,
            Elision::Aggressive,
        );
        assert!(src.next().unwrap().is_some());
        assert!(src.next().unwrap().is_none(), "torn tail is not yielded");
        assert_eq!(src.offset(), 8, "cursor stays at the torn line's start");
    }

    /// The live-tail pattern: Stop at the torn line, the writer finishes it, rewind to the
    /// cursor, and the next poll reads the whole line — never its second half.
    #[test]
    fn rewind_then_repoll_reads_the_completed_line_whole() {
        let mut src = LineSource::new(
            Cursor::new(b"{\"a\":1}\n{\"torn\":".to_vec()),
            0,
            TornTail::Stop,
            Elision::Aggressive,
        );
        assert!(src.next().unwrap().is_some());
        assert!(src.next().unwrap().is_none());
        // The writer finishes the line.
        let pos = src.reader.stream_position().unwrap();
        src.reader.get_mut().extend_from_slice(b"2}\n");
        src.reader.seek(SeekFrom::Start(pos)).unwrap();
        src.rewind_to_cursor().unwrap();
        let (at, line) = src.next().unwrap().unwrap();
        assert_eq!((at, line), (8, "{\"torn\":2}"));
    }

    #[test]
    fn yield_delivers_the_torn_line_and_advances() {
        let data = "{\"a\":1}\n{\"torn\":tr";
        let mut src = LineSource::new(
            Cursor::new(data.as_bytes().to_vec()),
            0,
            TornTail::Yield,
            Elision::Aggressive,
        );
        assert!(src.next().unwrap().is_some());
        let (at, line) = src.next().unwrap().unwrap();
        assert_eq!((at, line), (8, "{\"torn\":tr"));
        assert_eq!(src.offset(), data.len() as u64);
        assert!(src.next().unwrap().is_none());
    }

    #[test]
    fn the_gauges_accumulate() {
        use crate::engine::elide::{ELIDE_STRING_BYTES, SCAN_THRESHOLD};
        let blob = "g".repeat(SCAN_THRESHOLD + ELIDE_STRING_BYTES);
        let data = format!("{{\"big\":\"{blob}\"}}\n{{\"small\":1}}\n");
        let mut src = LineSource::new(
            Cursor::new(data.into_bytes()),
            0,
            TornTail::Yield,
            Elision::Aggressive,
        );
        while src.next().unwrap().is_some() {}
        assert_eq!(src.elided.elided_lines, 1);
        assert!(src.elided.elided_bytes > ELIDE_STRING_BYTES as u64);
        assert_eq!(src.elided.skipped_lines, 0);
    }

    /// A nonzero starting offset bases both the yielded offsets and the markers.
    #[test]
    fn a_resumed_source_reports_absolute_offsets() {
        let whole = "{\"a\":1}\n{\"b\":2}\n";
        let resume_at = 8u64;
        let mut cur = Cursor::new(whole.as_bytes().to_vec());
        cur.seek(SeekFrom::Start(resume_at)).unwrap();
        let mut src = LineSource::new(cur, resume_at, TornTail::Yield, Elision::Aggressive);
        let (at, line) = src.next().unwrap().unwrap();
        assert_eq!((at, line), (8, "{\"b\":2}"));
    }
}
