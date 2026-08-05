//! **The TUI's durable block store** (#96) — a resident store with a log behind it.
//!
//! `Bv` is still `Arc<Block>`, so every reader keeps the refcount-bump access the in-process
//! frontend rides. What is added is an append-only JSONL backing: each committed block is
//! serialized once as it crosses the durability frontier, so a later process can
//! [`load`](ArcLog::load) the committed prefix instead of re-folding the transcript to reach it.
//!
//! It lives here, not in the engine, because [`DurableStore::Note`] is the frontend's own type:
//! the store and the note a peer reads out of the lock are two halves of one frontend's contract.
//!
//! It is not tier-b. Tier-b exists to keep content *off* the heap and pays a positional read per
//! access; this keeps content resident and writes only so the next process can skip work. The
//! two answer different questions and deliberately do not share a type.

use claude_replay_core::engine::{BlockRead, BlockStore, SessionMeta};
use claude_replay_core::model::{Block, BlockIndex, EpochSeconds};
use claude_replay_present::cache::DurableStore;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct ArcLog {
    path: PathBuf,
    /// The append handle. `None` only when the backing could not be opened — the store then
    /// still serves the session in RAM, and the next load simply finds a short log and resumes
    /// from further back. Degrading to "slower next time" beats failing a session that works.
    file: Option<File>,
    /// Bytes written, including framing newlines.
    len: u64,
}

impl ArcLog {
    /// A store with NO backing: blocks stay resident, nothing is written. The `--no-cache` and
    /// denied-admission path — identical in behaviour to the store that preceded #96.
    pub fn memory() -> Self {
        Self {
            path: PathBuf::new(),
            file: None,
            len: 0,
        }
    }

    /// Create the backing, discarding anything already there — a cold start.
    pub fn create(path: &Path) -> std::io::Result<Self> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            file: Some(file),
            len: 0,
        })
    }

    /// Open the backing for appending, keeping what is there — a resume.
    pub fn open_append(path: &Path) -> std::io::Result<Self> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let len = file.metadata()?.len();
        Ok(Self {
            path: path.to_path_buf(),
            file: Some(file),
            len,
        })
    }

    /// The committed blocks the backing actually holds.
    ///
    /// A **torn trailing record** — the writer died mid-append — is dropped *and* cut from the
    /// file, so the log is left exactly as long as the records it yielded. Doing it here rather
    /// than leaving the fragment for the next append is what keeps `len` an honest offset: an
    /// append after a torn tail would otherwise splice a new record onto half an old one and
    /// corrupt every locator after it.
    pub fn load(&mut self) -> std::io::Result<Vec<Arc<Block>>> {
        let mut buf = Vec::new();
        match File::open(&self.path) {
            Ok(mut f) => f.read_to_end(&mut buf)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut out = Vec::new();
        let mut good = 0u64; // bytes covered by complete, parsed records
        for line in buf.split_inclusive(|b| *b == b'\n') {
            if !line.ends_with(b"\n") {
                break; // a fragment with no newline: the writer died mid-append
            }
            match serde_json::from_slice::<Block>(&line[..line.len() - 1]) {
                Ok(b) => {
                    out.push(Arc::new(b));
                    good += line.len() as u64;
                }
                Err(_) => break, // a complete line that does not parse ends the usable log
            }
        }
        if good != self.len || good != buf.len() as u64 {
            self.cut_to(good)?;
        }
        Ok(out)
    }

    /// Cut the backing to `n` committed records (#96 I6) — the content stream must never keep
    /// bytes above the resume point, or a reader would serve blocks the meta stream has no
    /// record of.
    pub fn truncate_to(&mut self, n: usize) -> std::io::Result<()> {
        let mut buf = Vec::new();
        match File::open(&self.path) {
            Ok(mut f) => f.read_to_end(&mut buf)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        let end = buf
            .split_inclusive(|b| *b == b'\n')
            .take(n)
            .map(|l| l.len() as u64)
            .sum();
        self.cut_to(end)
    }

    fn cut_to(&mut self, len: u64) -> std::io::Result<()> {
        if let Some(f) = &mut self.file {
            f.set_len(len)?;
            f.seek(SeekFrom::End(0))?;
        }
        self.len = len;
        Ok(())
    }

    /// Bytes in the backing — the durable analogue of a block count, used by the cache's
    /// length checks.
    pub fn backing_len(&self) -> u64 {
        self.len
    }
}

impl BlockStore for ArcLog {
    type Bv = Arc<Block>;

    /// Append the block's record, then hand back the resident `Arc`. A write error is swallowed
    /// exactly as tier-b's is: the session is correct either way, and a short log costs only a
    /// longer resume next time.
    fn put(
        &mut self,
        b: Block,
        _at: BlockIndex,
        _user_times: &[Option<EpochSeconds>],
    ) -> Arc<Block> {
        if let Some(f) = &mut self.file {
            if let Ok(mut rec) = serde_json::to_vec(&b) {
                rec.push(b'\n');
                if f.write_all(&rec).is_ok() {
                    self.len += rec.len() as u64;
                }
            }
        }
        Arc::new(b)
    }

    /// A source truncation rebuilt the session: restart the log, since every locator into it is
    /// now meaningless.
    fn reset(&mut self) {
        let _ = self.cut_to(0);
    }
}

impl BlockRead for ArcLog {
    fn get<'a>(&'a self, bv: &'a Arc<Block>) -> std::borrow::Cow<'a, Block> {
        std::borrow::Cow::Borrowed(bv)
    }
}

/// What a second `claude-replay` finds when it discovers this one already holds a session
/// (#96 §8.4). The refusal message names the pane, so "it's open somewhere else" becomes
/// "attach to it here" — `tmux attach` being the real sharing primitive the TUI defers to.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TuiNote {
    /// `$TMUX_PANE`, when running under tmux.
    pub pane: Option<String>,
}

impl TuiNote {
    /// This process's note, read from the environment.
    pub fn here() -> Self {
        Self {
            pane: std::env::var("TMUX_PANE").ok(),
        }
    }
}

impl DurableStore for ArcLog {
    type Note = TuiNote;

    fn load(&mut self) -> std::io::Result<Vec<Arc<Block>>> {
        ArcLog::load(self)
    }

    /// The TUI derives no render continuation from its prefix — blocks render from themselves —
    /// so adopting a prefix is exactly the truncation.
    fn adopt(&mut self, n: usize, _meta: &SessionMeta) -> std::io::Result<()> {
        self.truncate_to(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(n: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cr-arclog-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join(n)
    }
    fn blk(s: &str) -> Block {
        Block::UserText(s.into())
    }

    /// What was `put` comes back out of a reopened log, in order — the property the whole
    /// resume rests on.
    #[test]
    fn puts_reload_in_order() {
        let p = tmp("rt.jsonl");
        let mut s = ArcLog::create(&p).unwrap();
        for t in ["a", "b", "c"] {
            s.put(blk(t), 0, &[]);
        }
        let mut r = ArcLog::open_append(&p).unwrap();
        let got = r.load().unwrap();
        assert_eq!(got.len(), 3);
        assert_eq!(*got[2], blk("c"));
        assert_eq!(r.backing_len(), s.backing_len());
    }

    /// A crash mid-append leaves half a record. It must be dropped AND cut, so the next append
    /// does not splice onto the fragment.
    #[test]
    fn a_torn_tail_is_dropped_and_cut() {
        let p = tmp("torn.jsonl");
        let mut s = ArcLog::create(&p).unwrap();
        for t in ["a", "b"] {
            s.put(blk(t), 0, &[]);
        }
        let raw = std::fs::read(&p).unwrap();
        std::fs::write(&p, &raw[..raw.len() - 4]).unwrap(); // chop mid-record

        let mut r = ArcLog::open_append(&p).unwrap();
        let got = r.load().unwrap();
        assert_eq!(got.len(), 1, "the torn record is dropped");
        assert_eq!(*got[0], blk("a"));
        assert_eq!(
            r.backing_len(),
            std::fs::metadata(&p).unwrap().len(),
            "and the file is cut to match"
        );

        // The next append lands on a clean boundary, not on the fragment.
        r.put(blk("z"), 0, &[]);
        let mut r2 = ArcLog::open_append(&p).unwrap();
        let got = r2.load().unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(*got[1], blk("z"));
    }

    /// `truncate_to` cuts to a record boundary — the I6 alignment step.
    #[test]
    fn truncate_to_cuts_at_a_record_boundary() {
        let p = tmp("cut.jsonl");
        let mut s = ArcLog::create(&p).unwrap();
        for t in ["a", "b", "c", "d"] {
            s.put(blk(t), 0, &[]);
        }
        s.truncate_to(2).unwrap();
        let mut r = ArcLog::open_append(&p).unwrap();
        let got = r.load().unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(*got[1], blk("b"));

        s.truncate_to(0).unwrap();
        assert_eq!(ArcLog::open_append(&p).unwrap().load().unwrap().len(), 0);
    }

    /// A missing backing is an empty load, not an error — the cold-start path.
    #[test]
    fn a_missing_backing_loads_empty() {
        let p = tmp("gone.jsonl");
        let _ = std::fs::remove_file(&p);
        let mut s = ArcLog::open_append(&p).unwrap();
        assert!(s.load().unwrap().is_empty());
    }
}
