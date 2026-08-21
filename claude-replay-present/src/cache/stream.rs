//! **The durable meta stream** (#96 §2) — the on-disk half of the record format.
//!
//! One JSONL file: a [`StreamHeader`] on line 1, then one [`MetaRecord`] per committing drain.
//! Append-only, so **every crash leaves a prefix** — which is what makes the recovery space
//! enumerable rather than hopeful, and what the crash-consistency harness enumerates.
//!
//! Reading is **iterative**. Unlike the committed `BV` vector — resident by definition, being
//! the committed index itself — records are consumed one at a time and never all held, so a
//! long-lived session's stream never has to fit in memory.

use crate::engine::meta_stream::{crc32, MetaRecord, StreamHeader};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

// The window CRC moved down into the engine (#14): the metrics cursor pairs it with a byte
// offset exactly as `Resume` does, so both resumable folds now share one implementation.
// The public path here survives as a re-export.
pub use crate::engine::meta_stream::{window_at, WINDOW_BYTES};

/// The meta stream file within a session's durable directory.
pub fn meta_path(dir: &Path) -> PathBuf {
    dir.join("meta.jsonl")
}

/// CRC32 of a transcript's first line — the stream's identity check, so a cache is never
/// matched against a different file that happens to sit at the same path.
pub fn anchor_of(src: &Path) -> std::io::Result<u32> {
    // #193: the identity read runs the primitive with elision OFF — a CRC over elided
    // text would make cache identity a function of the elision constants — and gains the
    // ceiling bound `.read_line` never had. UTF-8 is enforced as `read_line` enforced it,
    // so the anchor of every existing cache is unchanged.
    let mut out = Vec::new();
    let mut r = BufReader::new(File::open(src)?);
    use claude_replay_core::engine::{read_line_elided, Elision};
    read_line_elided(&mut r, &mut out, 0, Elision::None)?;
    let first = std::str::from_utf8(&out)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok(crc32(first.trim_end().as_bytes()))
}

/// An append-only writer over one session's meta stream.
pub struct MetaWriter {
    file: File,
    src: PathBuf,
}

impl MetaWriter {
    /// Create the stream, writing its header. Truncates any existing file — the caller has
    /// already decided the old one is unusable (a rejected cache, or a fold reset).
    pub fn create(
        dir: &Path,
        src: &Path,
        versions: crate::engine::meta_stream::Versions,
    ) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let mut file = File::create(meta_path(dir))?;
        let header = StreamHeader {
            anchor: anchor_of(src)?,
            versions,
        };
        writeln!(file, "{}", serde_json::to_string(&header)?)?;
        Ok(Self {
            file,
            src: src.to_path_buf(),
        })
    }

    /// Re-open an existing stream for appending, cut to the first `keep` records.
    ///
    /// The cut is the meta stream's half of I6. A resumed writer re-folds from `replay_from` and
    /// writes records for those commits again; anything the previous run left above the
    /// alignment point would then be folded a second time, and the reader's turn/tool/token
    /// totals would come out high. The content stream is cut for the same reason — this is the
    /// other half, and it is easy to forget precisely because blocks stay correct without it.
    pub fn open_append(dir: &Path, src: &Path, keep: usize) -> std::io::Result<Self> {
        let path = meta_path(dir);
        let raw = std::fs::read(&path)?;
        // Header + `keep` records, counted in framing newlines.
        let mut end = 0usize;
        for (n, line) in raw.split_inclusive(|b| *b == b'\n').enumerate() {
            if n > keep {
                break;
            }
            end += line.len();
        }
        if end < raw.len() {
            OpenOptions::new()
                .write(true)
                .open(&path)?
                .set_len(end as u64)?;
        }
        let file = OpenOptions::new().append(true).open(&path)?;
        Ok(Self {
            file,
            src: src.to_path_buf(),
        })
    }

    /// Re-open an existing stream for appending, cutting **nothing**.
    ///
    /// [`open_append`](Self::open_append)'s cut exists to drop records a resume is about to
    /// re-author. A *retained* entry (#109) re-authors nothing — its fold never stopped where the
    /// stream stops, it simply stopped writing — so the same cut would discard real history and
    /// the next reader would resume further back than it needs to.
    pub fn reattach(dir: &Path, src: &Path) -> std::io::Result<Self> {
        let file = OpenOptions::new().append(true).open(meta_path(dir))?;
        Ok(Self {
            file,
            src: src.to_path_buf(),
        })
    }

    /// Append one record, filling the resume window the engine deliberately left unset.
    ///
    /// The engine authors records but cannot compute the window: the CRC covers **source
    /// bytes**, which the persistence layer owns. Filling it here keeps the engine free of file
    /// access, which is what lets its alignment stay a pure function.
    pub fn append(&mut self, rec: &MetaRecord) -> std::io::Result<()> {
        let mut rec = rec.clone();
        if let Some(r) = rec.resume.as_mut() {
            r.window = window_at(&self.src, r.replay_from)?;
        }
        writeln!(self.file, "{}", serde_json::to_string(&rec)?)?;
        Ok(())
    }

    /// Flush to the OS. Not an fsync: a torn tail costs at most one commit by construction, so
    /// paying a device round-trip per commit would buy nothing this design needs.
    pub fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

/// Read a stream's header, then its records one at a time.
///
/// A **torn trailing line** — the writer died mid-append — is dropped rather than misread. That
/// is not defensive: it is the ordinary outcome of a crash, and the harness enumerates it.
pub struct MetaReader {
    lines: std::io::Lines<BufReader<File>>,
}

impl MetaReader {
    /// Open and consume the header. `Ok(None)` when the file is absent or has no header — a
    /// cache that was never written, which is a cold start rather than an error.
    pub fn open(dir: &Path) -> std::io::Result<Option<(StreamHeader, Self)>> {
        let f = match File::open(meta_path(dir)) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut lines = BufReader::new(f).lines();
        let Some(Ok(head)) = lines.next() else {
            return Ok(None);
        };
        let Ok(header) = serde_json::from_str::<StreamHeader>(&head) else {
            return Ok(None); // an unreadable header ⇒ the whole stream is unusable
        };
        Ok(Some((header, Self { lines })))
    }
}

impl Iterator for MetaReader {
    type Item = MetaRecord;
    fn next(&mut self) -> Option<MetaRecord> {
        // A record that does not parse ends the stream: it is the torn tail, and everything
        // after it (if a later write somehow landed) is not corroborated by a clean prefix.
        match self.lines.next()? {
            Ok(l) => serde_json::from_str(&l).ok(),
            Err(_) => None,
        }
    }
}

/// Drop the records a checkpoint makes redundant (#96 §6.6). Returns how many were dropped.
///
/// It needs no fold: a checkpoint is an absolute view, so every record before it is already
/// summarized. Pick the NEWEST checkpoint the content stream corroborates
/// (`resume.id <= committed_len`) and keep the stream from there.
///
/// `committed_len` counts committed **blocks**, not records — the unit `Resume::id` speaks, and
/// the same number [`claim`](super::admit::claim) hands to alignment. A record count would be far
/// too small and quietly find no base at all.
///
/// **Rewrite, not truncate-in-place.** A checkpoint replaces a *prefix* of an append-only file,
/// so this writes a new file and renames over the old one — the rename is the commit point, and a
/// crash mid-rewrite leaves the original untouched. The caller runs it under the same
/// `<presentation, session>` lock as every other write, which is what makes "any time" safe
/// rather than a race.
///
/// **Never past `n`.** Records above `committed_len` are exactly the ones alignment rejected — a
/// torn tail, or drains the content stream does not support — and keeping one as the new base
/// would launder unverified data into absolute state.
pub fn compact(dir: &Path, committed_len: usize, compact_after: usize) -> std::io::Result<usize> {
    let Some((header, reader)) = MetaReader::open(dir)? else {
        return Ok(0);
    };
    let records: Vec<MetaRecord> = reader.collect();
    let Some(at) = records.iter().rposition(|r| {
        r.checkpoint.is_some() && r.resume.as_ref().is_some_and(|res| res.id <= committed_len)
    }) else {
        return Ok(0);
    };
    if at < compact_after {
        return Ok(0); // not enough behind it to be worth a rewrite
    }
    let tmp = dir.join("meta.jsonl.compact");
    {
        let mut f = File::create(&tmp)?;
        writeln!(f, "{}", serde_json::to_string(&header)?)?;
        for r in &records[at..] {
            writeln!(f, "{}", serde_json::to_string(r)?)?;
        }
        f.flush()?;
    }
    std::fs::rename(&tmp, meta_path(dir))?; // the commit point
    Ok(at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::meta_stream::{MetaRecord, Resume, Versions};

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cr-stream-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
    fn versions() -> Versions {
        Versions {
            format: 1,
            fold: 1,
            flavor: None,
        }
    }
    fn rec(id: usize, replay_from: u64) -> MetaRecord {
        MetaRecord {
            turns: Some(1),
            resume: Some(Resume {
                id,
                replay_from,
                window: 0,
                prev_ts: None,
                pending_ts: None,
                pre: serde_json::Value::Null,
                metrics_state: serde_json::Value::Null,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn round_trips_header_and_records() {
        let d = tmp("rt");
        let src = d.join("t.jsonl");
        std::fs::write(&src, "{\"a\":1}\nsecond line\nthird\n").unwrap();

        let mut w = MetaWriter::create(&d, &src, versions()).unwrap();
        for i in 1..=3 {
            w.append(&rec(i, 8 * i as u64)).unwrap();
        }
        w.flush().unwrap();

        let (h, r) = MetaReader::open(&d).unwrap().unwrap();
        assert_eq!(h.versions, versions());
        assert_eq!(h.anchor, anchor_of(&src).unwrap());
        let got: Vec<_> = r.collect();
        assert_eq!(got.len(), 3);
        assert_eq!(got[2].resume.as_ref().unwrap().id, 3);
    }

    /// The writer fills the window the engine left unset — the engine cannot, since the CRC
    /// covers source bytes it deliberately never touches.
    #[test]
    fn the_writer_fills_the_resume_window() {
        let d = tmp("win");
        let src = d.join("t.jsonl");
        std::fs::write(&src, "x".repeat(500)).unwrap();
        let mut w = MetaWriter::create(&d, &src, versions()).unwrap();
        w.append(&rec(1, 400)).unwrap();
        w.flush().unwrap();

        let got = MetaReader::open(&d).unwrap().unwrap().1.next().unwrap();
        let win = got.resume.unwrap().window;
        assert_ne!(win, 0, "the writer must fill it");
        assert_eq!(win, window_at(&src, 400).unwrap());
        assert_ne!(win, window_at(&src, 300).unwrap(), "offset-specific");
    }

    /// A crash mid-append leaves half a line. It must be DROPPED, never misread — and the
    /// clean prefix before it must still load.
    #[test]
    fn a_torn_trailing_line_is_dropped_not_misread() {
        let d = tmp("torn");
        let src = d.join("t.jsonl");
        std::fs::write(&src, "line one\nline two\n").unwrap();
        let mut w = MetaWriter::create(&d, &src, versions()).unwrap();
        for i in 1..=3 {
            w.append(&rec(i, i as u64)).unwrap();
        }
        w.flush().unwrap();

        // Chop the file mid-record.
        let raw = std::fs::read_to_string(meta_path(&d)).unwrap();
        std::fs::write(meta_path(&d), &raw[..raw.len() - 20]).unwrap();

        let got: Vec<_> = MetaReader::open(&d).unwrap().unwrap().1.collect();
        assert_eq!(
            got.len(),
            2,
            "the torn record is dropped, the prefix survives"
        );
        assert_eq!(got[1].resume.as_ref().unwrap().id, 2);
    }

    fn ckpt(id: usize, replay_from: u64, turns: usize) -> MetaRecord {
        let mut r = rec(id, replay_from);
        let mut m = crate::engine::meta_stream::MaterializedMeta::default();
        m.session_meta.turns = turns;
        r.checkpoint = Some(m);
        r
    }

    /// Compaction keeps the newest corroborated checkpoint and everything after it, and the
    /// result loads like an uncompacted stream — the header survives, the records line up.
    #[test]
    fn compaction_keeps_the_newest_corroborated_checkpoint() {
        let d = tmp("compact");
        let src = d.join("t.jsonl");
        std::fs::write(&src, "x".repeat(400)).unwrap();
        let mut w = MetaWriter::create(&d, &src, versions()).unwrap();
        for i in 1..=10 {
            // checkpoints at 4 and 8; the newest wins
            if i == 4 || i == 8 {
                w.append(&ckpt(i, i as u64, i)).unwrap();
            } else {
                w.append(&rec(i, i as u64)).unwrap();
            }
        }
        w.flush().unwrap();

        let dropped = compact(&d, 10, 1).unwrap();
        assert_eq!(
            dropped, 7,
            "records 1..=7 go; the checkpoint at 8 is index 7"
        );

        let (h, r) = MetaReader::open(&d).unwrap().unwrap();
        assert_eq!(h.versions, versions(), "the header survives the rewrite");
        let got: Vec<_> = r.collect();
        assert_eq!(got.len(), 3, "the checkpoint plus records 9 and 10");
        assert!(got[0].checkpoint.is_some(), "it opens ON the checkpoint");
        assert_eq!(got[0].resume.as_ref().unwrap().id, 8);
        assert_eq!(got[2].resume.as_ref().unwrap().id, 10);
    }

    /// **Never past `n`.** A checkpoint the content stream cannot corroborate must not become
    /// the new base — those are exactly the records alignment rejects, and keeping one would
    /// launder unverified data into absolute state.
    #[test]
    fn compaction_never_bases_on_an_uncorroborated_checkpoint() {
        let d = tmp("compact-n");
        let src = d.join("t.jsonl");
        std::fs::write(&src, "x".repeat(400)).unwrap();
        let mut w = MetaWriter::create(&d, &src, versions()).unwrap();
        for i in 1..=10 {
            if i == 3 || i == 9 {
                w.append(&ckpt(i, i as u64, i)).unwrap();
            } else {
                w.append(&rec(i, i as u64)).unwrap();
            }
        }
        w.flush().unwrap();

        // Only 5 blocks are corroborated: the checkpoint at 9 is off-limits, 3 is not.
        assert_eq!(compact(&d, 5, 1).unwrap(), 2);
        let got: Vec<_> = MetaReader::open(&d).unwrap().unwrap().1.collect();
        assert_eq!(got[0].resume.as_ref().unwrap().id, 3, "based on 3, not 9");
    }

    /// Below the threshold, and with no checkpoint at all, compaction is a no-op that leaves the
    /// stream byte-for-byte alone — it must never be a rewrite for its own sake.
    #[test]
    fn compaction_is_a_no_op_when_it_would_not_pay() {
        let d = tmp("compact-noop");
        let src = d.join("t.jsonl");
        std::fs::write(&src, "x".repeat(400)).unwrap();
        let mut w = MetaWriter::create(&d, &src, versions()).unwrap();
        for i in 1..=6 {
            w.append(&if i == 5 {
                ckpt(i, i as u64, i)
            } else {
                rec(i, i as u64)
            })
            .unwrap();
        }
        w.flush().unwrap();
        let before = std::fs::read(meta_path(&d)).unwrap();

        assert_eq!(compact(&d, 6, 100).unwrap(), 0, "too few records behind it");
        assert_eq!(std::fs::read(meta_path(&d)).unwrap(), before, "untouched");

        let d2 = tmp("compact-none");
        let mut w = MetaWriter::create(&d2, &src, versions()).unwrap();
        for i in 1..=6 {
            w.append(&rec(i, i as u64)).unwrap();
        }
        w.flush().unwrap();
        let before = std::fs::read(meta_path(&d2)).unwrap();
        assert_eq!(compact(&d2, 6, 1).unwrap(), 0, "no checkpoint to base on");
        assert_eq!(std::fs::read(meta_path(&d2)).unwrap(), before, "untouched");
    }

    /// No stream at all is a cold start, not an error.
    #[test]
    fn a_missing_stream_is_none() {
        assert!(MetaReader::open(&tmp("none")).unwrap().is_none());
    }

    /// An unreadable header condemns the whole stream: without versions there is nothing to
    /// validate the records against.
    #[test]
    fn an_unreadable_header_rejects_the_stream() {
        let d = tmp("badhdr");
        std::fs::write(meta_path(&d), "not json\n{}\n").unwrap();
        assert!(MetaReader::open(&d).unwrap().is_none());
    }
}
