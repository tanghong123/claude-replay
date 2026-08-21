//! The engine's line-reading machinery (#193): [`LineSource`] — the ONE driver over the
//! eliding primitive — and [`bounded_lines`], its owned-line iterator for discovery.
//!
//! The old `LineReader` (poll-batching tail reader, adapted in spirit from
//! claude-code-scrollback, MIT © 2026 pjh4993) dissolved into `FollowParser` + `LineSource`
//! at #193 §9.1: its resume constructors only ever computed a starting offset, its
//! cross-poll pending-partial buffer is replaced by `TornTail::Stop` + `rewind_to_cursor`,
//! and truncation detection is one metadata check in the follower. Offset validation stays
//! deliberately NOT the reader's job (#96): the durable cache checks the source window
//! before it constructs a source.

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
    last_torn: bool,
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
            last_torn: false,
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
        self.last_torn = false;
        loop {
            let start = self.offset;
            match read_line_elided(&mut self.reader, &mut self.out, start, self.policy)? {
                LineOutcome::Eof => return Ok(None),
                LineOutcome::Torn { raw_len } => match self.tail {
                    TornTail::Stop => return Ok(None),
                    TornTail::Yield => {
                        self.last_torn = true;
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

    /// Did [`next`](Self::next) most recently yield a TORN line (under [`TornTail::Yield`])?
    /// A consumer that diagnoses malformed lines excuses a torn one — it is a write in
    /// progress, not schema drift. Query after the yielded body's borrow ends.
    pub fn last_was_torn(&self) -> bool {
        self.last_torn
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

/// Every discovery `.lines()` loop, replaced by the one bounded reader (#193): yields
/// owned, newline-stripped, non-blank lines through the eliding primitive. `.lines()` grew
/// a line without cap on a newline-less file — the trap the audit named: `take(N)` bounds
/// the line COUNT, not the SIZE, and `detect_agent`'s five-line sniff runs on every
/// candidate discovery sees. Sniffs pass [`Elision::None`] (ceiling-bounded, zero
/// behavioral change — their point is the bound, not elision); a whole-file field scan
/// passes [`Elision::Aggressive`]. An unopenable path yields nothing, exactly as the
/// `.lines()` idiom's `ok()?` did.
pub fn bounded_lines(path: &std::path::Path, policy: Elision) -> impl Iterator<Item = String> {
    let mut src = std::fs::File::open(path)
        .ok()
        .map(|f| LineSource::new(std::io::BufReader::new(f), 0, TornTail::Yield, policy));
    std::iter::from_fn(move || match src.as_mut()?.next() {
        Ok(Some((_, line))) => Some(line.to_string()),
        _ => {
            src = None;
            None
        }
    })
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
