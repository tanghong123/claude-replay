//! Byte-offset incremental **line reader** for a JSONL file. Poll-driven (no threads).
//!
//! Two capabilities:
//! - **Tail** — `poll()` returns the complete lines appended since the last poll, buffering a
//!   trailing partial until its newline arrives, and recovering from truncation/rewrite
//!   (compaction) by detecting a shrunk file and re-reading from 0 (`reset`).
//! - **Resume** — `tell()` yields an opaque [`Position`]; `open_at(path, pos)` resumes reading
//!   there. The first poll validates `pos` against the current file (identity anchor + a hash of
//!   the consumed prefix) and either returns only the delta or, if the position is
//!   stale/foreign/rewritten, resets to 0 and returns the whole file with `reset: true` — the
//!   same signal a truncation already emits, so the consumer's loop is uniform.
//!
//! Adapted in spirit from claude-code-scrollback (MIT, © 2026 pjh4993): buffer a trailing
//! partial line until its newline arrives, and recover from truncation/rewrite by detecting a
//! shrunk file and re-reading.

use std::collections::hash_map::DefaultHasher;
use std::fs::File;
use std::hash::Hasher;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

pub struct LineReader {
    path: PathBuf,
    offset: crate::model::ByteOffset,
    pending: String,
    /// Rolling hash of the consumed bytes `[0, offset)`, updated on each `poll`. `tell()`
    /// snapshots it as `Position::consumed_hash` (O(1), no re-read).
    hasher: DefaultHasher,
    /// Hash of the file's first non-empty line (a stable identity — Claude's `sessionId` /
    /// Codex's `session_meta` line). Set the first time a line is produced from offset 0;
    /// carried in `Position::anchor` to reject a position from a different file.
    anchor: Option<u64>,
    /// A position awaiting validation, set by [`open_at`](Self::open_at) and consumed on the
    /// first `poll`.
    resume: Option<Position>,
}

/// An opaque, serializable read position. Encodes where reading stopped (`offset`) plus enough
/// identity to reject a position from a different file (`anchor` = first-line hash) and to
/// detect that the consumed region was rewritten (`consumed_hash` = hash of `[0, offset)`).
/// Round-trips via [`encode`](Self::encode) / [`decode`](Self::decode) with a version tag; the
/// fields are private, so it is genuinely opaque.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Position {
    offset: u64,
    consumed_hash: u64,
    anchor: u64,
    len_hint: u64,
}

// The resume API (`Position::{encode,decode}`, `LineReader::{tell,open_at}`) is test-wired now;
// its production consumer is `SessionBuilder::checkpoint`/`resume` (#19) + restart persistence
// (#11), per the roadmap's decision A. The `allow(dead_code)` goes away when #19 wires it in.
impl Position {
    /// Serialize to a versioned string (`crrd1:…`). A garbage or wrong-version token is
    /// rejected by [`decode`](Self::decode), not misread.
    #[allow(dead_code)]
    pub fn encode(&self) -> String {
        format!(
            "crrd1:{:x}:{:x}:{:x}:{:x}",
            self.offset, self.consumed_hash, self.anchor, self.len_hint
        )
    }

    /// Parse a token from [`encode`](Self::encode); `None` if the prefix/version/shape is wrong.
    #[allow(dead_code)]
    pub fn decode(s: &str) -> Option<Position> {
        let rest = s.strip_prefix("crrd1:")?;
        let mut it = rest.split(':');
        let mut next = || u64::from_str_radix(it.next()?, 16).ok();
        let offset = next()?;
        let consumed_hash = next()?;
        let anchor = next()?;
        let len_hint = next()?;
        if it.next().is_some() {
            return None;
        }
        Some(Position {
            offset,
            consumed_hash,
            anchor,
            len_hint,
        })
    }
}

#[derive(Default)]
pub struct Poll {
    /// Complete new lines (no trailing newline) since the last poll.
    pub lines: Vec<String>,
    /// The **start byte offset** in the file of each line in `lines` (parallel, same length) —
    /// so the follower can stamp attachment locators without re-deriving positions.
    pub offsets: Vec<crate::model::ByteOffset>,
    /// True if a truncation/rewrite (or a stale `open_at` position) was detected and we re-read
    /// from 0 — `lines` then holds the whole current file.
    pub reset: bool,
}

fn hash_bytes(b: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    h.write(b);
    h.finish()
}

/// Hash of the first non-empty line of `bytes`, split exactly as [`LineReader::consume`] does
/// (lossy UTF-8, split on `\n`, skip empties) so the anchor matches on both the read and the
/// validation path.
fn first_line_hash(bytes: &[u8]) -> Option<u64> {
    String::from_utf8_lossy(bytes)
        .split('\n')
        .find(|p| !p.is_empty())
        .map(|p| hash_bytes(p.as_bytes()))
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
            hasher: DefaultHasher::new(),
            anchor: None,
            resume: None,
        }
    }

    /// Start reading from the BEGINNING — the first `poll` returns the whole current file, then
    /// subsequent polls return only appends. Used by the incremental follower (M16), which folds
    /// the file once through a persistent `Replayer` and then only the delta.
    pub fn open_at_start(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            offset: 0,
            pending: String::new(),
            hasher: DefaultHasher::new(),
            anchor: None,
            resume: None,
        }
    }

    /// Resume reading at `pos`. Optimistic: the next `poll` validates `pos` against the current
    /// file and either resumes (returning only the delta after `pos`) or, if the position is
    /// stale/foreign/rewritten, resets to 0 and returns the whole file with `reset: true`.
    #[allow(dead_code)]
    pub fn open_at(path: impl Into<PathBuf>, pos: Position) -> Self {
        Self {
            path: path.into(),
            offset: 0,
            pending: String::new(),
            hasher: DefaultHasher::new(),
            anchor: None,
            resume: Some(pos),
        }
    }

    /// An opaque, serializable position at the current read point. O(1) — snapshots the rolling
    /// consumed-hash; no file access.
    #[allow(dead_code)]
    pub fn tell(&self) -> Position {
        Position {
            offset: self.offset,
            consumed_hash: self.hasher.finish(),
            anchor: self.anchor.unwrap_or(0),
            len_hint: self.offset,
        }
    }

    /// Reset to a from-scratch read (on a detected truncation/rewrite or a failed resume).
    fn reset_state(&mut self) {
        self.offset = 0;
        self.pending.clear();
        self.hasher = DefaultHasher::new();
        self.anchor = None;
    }

    /// Fold `buf` (the bytes just read, starting at the current `offset`) into the running
    /// state: advance the offset + rolling hash, split off complete lines into `out`, keep a
    /// trailing partial in `pending`, and set the identity `anchor` from the first line if unset.
    fn consume(&mut self, buf: &[u8], out: &mut Poll) {
        // File offset of `pending`'s first byte: bytes consumed so far, minus the held partial.
        // (Assumes valid UTF-8 — `pending` is built via lossy decode, the same assumption the
        // anchor/consumed-hash logic already relies on; JSONL transcripts are UTF-8.)
        let base = self.offset - self.pending.len() as u64;
        self.offset += buf.len() as u64;
        self.hasher.write(buf);
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
                if self.anchor.is_none() {
                    self.anchor = Some(hash_bytes(p.as_bytes()));
                }
                out.offsets.push(base + pos);
                out.lines.push(p.to_string());
            }
            pos += p.len() as u64 + 1; // + the '\n' that split consumed
        }
        self.pending = rest;
    }

    /// Read any bytes appended since the last poll, returning complete lines.
    pub fn poll(&mut self) -> std::io::Result<Poll> {
        if let Some(pos) = self.resume.take() {
            return self.poll_resume(pos);
        }
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

    /// First poll after [`open_at`](Self::open_at): validate `pos` against the current file and
    /// either resume from `pos.offset` (delta only) or fall back to a full re-read (`reset`).
    fn poll_resume(&mut self, pos: Position) -> std::io::Result<Poll> {
        let mut out = Poll::default();
        let mut f = match File::open(&self.path) {
            Ok(f) => f,
            Err(_) => return Ok(out), // file missing → next poll starts fresh from 0
        };
        let len = f.metadata()?.len();
        // Re-read + hash the claimed consumed prefix to validate (hashing only — never parse).
        let read_len = pos.offset.min(len);
        let mut prefix = vec![0u8; read_len as usize];
        f.seek(SeekFrom::Start(0))?;
        f.read_exact(&mut prefix)?;
        let valid = len >= pos.offset
            && hash_bytes(&prefix) == pos.consumed_hash
            && first_line_hash(&prefix) == Some(pos.anchor);
        if valid {
            // Resume: seed the rolling hash with the validated prefix, then read the delta.
            self.offset = pos.offset;
            self.hasher = DefaultHasher::new();
            self.hasher.write(&prefix);
            self.anchor = Some(pos.anchor);
            self.pending.clear();
            if len > self.offset {
                f.seek(SeekFrom::Start(self.offset))?;
                let mut buf = Vec::new();
                f.read_to_end(&mut buf)?;
                self.consume(&buf, &mut out);
            }
        } else {
            // Stale/foreign/rewritten → full re-read, same signal as a truncation.
            self.reset_state();
            out.reset = true;
            if len > 0 {
                f.seek(SeekFrom::Start(0))?;
                let mut buf = Vec::new();
                f.read_to_end(&mut buf)?;
                self.consume(&buf, &mut out);
            }
        }
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

    #[test]
    fn resume_from_a_saved_position_equals_uninterrupted_read() {
        let p = tmp("resume.jsonl");
        std::fs::write(&p, b"{\"a\":1}\n{\"b\":2}\n").unwrap();

        // Reader 1 consumes the current file, then we snapshot its position.
        let mut r1 = LineReader::open_at_start(&p);
        assert_eq!(r1.poll().unwrap().lines, vec!["{\"a\":1}", "{\"b\":2}"]);
        let pos = r1.tell();

        // Position round-trips through encode/decode opaquely.
        let pos = Position::decode(&pos.encode()).unwrap();

        // Append more, then resume a fresh reader at the saved position.
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        write!(f, "{{\"c\":3}}\n{{\"d\":4}}\n").unwrap();

        let mut r2 = LineReader::open_at(&p, pos);
        let got = r2.poll().unwrap();
        assert!(!got.reset, "valid resume must not reset");
        assert_eq!(got.lines, vec!["{\"c\":3}", "{\"d\":4}"]);

        // Reader 1 continued from where it stopped sees exactly the same delta.
        assert_eq!(r1.poll().unwrap().lines, vec!["{\"c\":3}", "{\"d\":4}"]);

        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn stale_position_after_rewrite_falls_back_to_full_reread() {
        let p = tmp("rewrite.jsonl");
        std::fs::write(&p, b"{\"a\":1}\n{\"b\":2}\n").unwrap();
        let mut r1 = LineReader::open_at_start(&p);
        r1.poll().unwrap();
        let pos = r1.tell();

        // Rewrite the consumed region (compaction): different bytes at the same offsets.
        std::fs::write(&p, b"{\"x\":9}\n{\"y\":8}\n{\"z\":7}\n").unwrap();

        let mut r2 = LineReader::open_at(&p, pos);
        let got = r2.poll().unwrap();
        assert!(got.reset, "rewritten prefix must be detected → reset");
        assert_eq!(got.lines, vec!["{\"x\":9}", "{\"y\":8}", "{\"z\":7}"]);

        std::fs::remove_file(&p).ok();
    }
}
