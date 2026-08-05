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
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// The window a resume validates: the bytes immediately below `replay_from`. Everything a
/// resume restores derives from below that offset, so it is the only region a rewrite can
/// silently corrupt — bytes at or after it are re-read and folded fresh.
pub const WINDOW_BYTES: u64 = 64 * 1024;

/// The meta stream file within a session's durable directory.
pub fn meta_path(dir: &Path) -> PathBuf {
    dir.join("meta.jsonl")
}

/// CRC32 of a transcript's first line — the stream's identity check, so a cache is never
/// matched against a different file that happens to sit at the same path.
pub fn anchor_of(src: &Path) -> std::io::Result<u32> {
    let mut first = String::new();
    BufReader::new(File::open(src)?).read_line(&mut first)?;
    Ok(crc32(first.trim_end().as_bytes()))
}

/// CRC32 of the (up to) [`WINDOW_BYTES`] ending at `offset`.
///
/// A short file, or an offset below the window size, hashes what is there — the length check in
/// [`admit`](super::admit()) is what catches a truncated source, so this need not also.
pub fn window_at(src: &Path, offset: u64) -> std::io::Result<u32> {
    let mut f = File::open(src)?;
    let start = offset.saturating_sub(WINDOW_BYTES);
    f.seek(SeekFrom::Start(start))?;
    let mut buf = vec![0u8; (offset - start) as usize];
    read_exact_or_short(&mut f, &mut buf)?;
    Ok(crc32(&buf))
}

fn read_exact_or_short(f: &mut File, buf: &mut Vec<u8>) -> std::io::Result<()> {
    use std::io::Read;
    let mut filled = 0;
    while filled < buf.len() {
        match f.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    buf.truncate(filled);
    Ok(())
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

    /// Re-open an existing stream for appending, after a load validated it.
    pub fn open_append(dir: &Path, src: &Path) -> std::io::Result<Self> {
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
