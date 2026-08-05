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
