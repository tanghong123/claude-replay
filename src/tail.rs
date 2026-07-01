//! Byte-offset incremental tail of a JSONL file. Poll-driven (no threads).
//!
//! Adapted in spirit from claude-code-scrollback (MIT, © 2026 pjh4993):
//! buffer a trailing partial line until its newline arrives, and recover from
//! truncation/rewrite (compaction) by detecting a shrunk file and re-reading.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

pub struct TailReader {
    path: PathBuf,
    offset: u64,
    pending: String,
}

#[derive(Default)]
pub struct Poll {
    /// Complete new lines (no trailing newline) since the last poll.
    pub lines: Vec<String>,
    /// True if a truncation/rewrite was detected and we re-read from 0.
    pub reset: bool,
}

impl TailReader {
    /// Start reading new bytes written *after* the current end of the file.
    pub fn open_at_end(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let offset = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        Self {
            path,
            offset,
            pending: String::new(),
        }
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
            self.offset = 0;
            self.pending.clear();
            out.reset = true;
        }
        if len == self.offset {
            return Ok(out);
        }
        f.seek(SeekFrom::Start(self.offset))?;
        let mut buf = Vec::new();
        let n = f.read_to_end(&mut buf)? as u64;
        self.offset += n;
        let chunk = String::from_utf8_lossy(&buf);
        self.pending.push_str(&chunk);
        // Split off complete lines; keep any trailing partial in `pending`.
        let mut rest = String::new();
        let ends_newline = self.pending.ends_with('\n');
        let mut parts: Vec<&str> = self.pending.split('\n').collect();
        if !ends_newline {
            rest = parts.pop().unwrap_or("").to_string();
        } else {
            parts.pop(); // trailing "" after the final newline
        }
        for p in parts {
            if !p.is_empty() {
                out.lines.push(p.to_string());
            }
        }
        self.pending = rest;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn reads_appended_lines_and_buffers_partials() {
        let dir = std::env::temp_dir().join(format!("peekv2-tail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.jsonl");
        std::fs::write(&p, b"{\"a\":1}\n").unwrap();

        let mut t = TailReader::open_at_end(&p); // start at end → nothing yet
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

        std::fs::remove_dir_all(&dir).ok();
    }
}
