//! Tier-b: **on-disk (or off-heap) block storage**. A [`BlockStore`] whose `Bv` is a small
//! [`Deferred`] locator, so a `Session<Deferred>` holds only the **O(N) offset table** while each
//! block's *content* lives in an append-only backing — an off-heap byte buffer
//! ([`TierBStore::new`]) or a real on-disk file ([`TierBStore::file`]). Reading a block is a
//! positional read + a `serde_json` decode ([`TierBSession`] implements [`BlockAccess`] over the
//! backing) — the raw [`Block`] is re-materialized on demand and dropped after use.
//!
//! Put-once holds by construction: the [`SessionAccumulator`](crate::engine::SessionAccumulator)
//! `put`s each block exactly once as it crosses the durability frontier (drained per `advance`),
//! so the append-only backing never sees duplicates — batch, repeated-snapshot, and live paths
//! are all safe. On a source truncation the accumulator's reset also
//! [`reset`](BlockStore::reset)s the store, so a rebuilt session starts on a fresh backing.

use crate::engine::{BlockAccess, BlockStore, Session};
use crate::model::{Block, BlockIndex, ByteOffset};
use std::borrow::Cow;
use std::io;
use std::path::{Path, PathBuf};

/// A locator into a tier-b backing: the block's serialized bytes are
/// `backing[offset .. offset + size]`. `Vec<Deferred>` (a `Session<Deferred>`'s `blocks`) **is** the
/// tier-b index — O(N) tiny locators, no content resident.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Deferred {
    /// Byte offset of this block's record in the backing.
    pub offset: ByteOffset,
    /// Length in bytes of this block's serialized record (excludes the framing newline).
    pub size: u32,
}

/// An append-only tier-b block store. `put` serializes the block (`serde_json`, newline-framed)
/// into the backing and returns its [`Deferred`] locator. The backing is either an **off-heap
/// byte buffer** ([`new`](Self::new) — tests / batch) or a real **on-disk file**
/// ([`file`](Self::file) — the live server's residents, so committed content never sits in RAM).
/// Hand the finished [`into_backing`](Self::into_backing) to a [`TierBSession`] to read blocks
/// back, or read them through the store's own [`get`](crate::engine::BlockRead::get).
pub struct TierBStore {
    backing: Backing,
}

/// Where the serialized block records live.
enum Backing {
    /// An in-memory append-only buffer.
    Mem(Vec<u8>),
    /// An on-disk append-only file: the open handle, the current length (the next record's
    /// offset), and the path (so a reset can recreate it).
    File {
        file: std::fs::File,
        len: u64,
        path: PathBuf,
    },
}

impl Default for TierBStore {
    fn default() -> Self {
        Self::new()
    }
}

impl TierBStore {
    /// A fresh, empty buffer-backed store.
    pub fn new() -> Self {
        Self {
            backing: Backing::Mem(Vec::new()),
        }
    }

    /// A fresh **file-backed** store at `path` (created/truncated): committed block content goes
    /// straight to disk, and [`get`](crate::engine::BlockRead::get) reads it back positionally — nothing
    /// resident beyond the locators the caller holds.
    pub fn file(path: &Path) -> io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        Ok(Self {
            backing: Backing::File {
                file,
                len: 0,
                path: path.to_path_buf(),
            },
        })
    }

    /// Re-open an **existing** file backing without truncating — the reload half of a
    /// persist/restore cycle: locators recorded earlier keep pointing at the same bytes, and any
    /// later `put` appends after them. The tracked length resumes from the file's current size.
    pub fn open(path: &Path) -> io::Result<Self> {
        use std::io::Seek;
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)?;
        let len = file.seek(std::io::SeekFrom::End(0))?;
        Ok(Self {
            backing: Backing::File {
                file,
                len,
                path: path.to_path_buf(),
            },
        })
    }

    /// Consume the store, returning the append-only backing bytes (pair with a
    /// `Session<Deferred>` in a [`TierBSession`] to read blocks). For a file-backed store this
    /// reads the file back — used by the persist path, not the live hot path.
    pub fn into_backing(self) -> Vec<u8> {
        match self.backing {
            Backing::Mem(buf) => buf,
            Backing::File { path, .. } => std::fs::read(&path).unwrap_or_default(),
        }
    }

    /// The current backing length (next block's offset) — the total bytes written so far.
    pub fn len(&self) -> usize {
        match &self.backing {
            Backing::Mem(buf) => buf.len(),
            Backing::File { len, .. } => *len as usize,
        }
    }

    /// Whether nothing has been stored yet.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Positional read of `[offset, offset+size)` — `&self`, no seek state (works under the
    /// accumulator's shared borrows).
    fn read_at(&self, offset: u64, size: usize) -> Vec<u8> {
        match &self.backing {
            Backing::Mem(buf) => {
                let start = offset as usize;
                buf[start..start + size].to_vec()
            }
            Backing::File { file, .. } => {
                let mut out = vec![0u8; size];
                #[cfg(unix)]
                {
                    use std::os::unix::fs::FileExt;
                    file.read_exact_at(&mut out, offset)
                        .expect("tier-b: backing read");
                }
                #[cfg(windows)]
                {
                    use std::os::windows::fs::FileExt;
                    let mut done = 0usize;
                    while done < size {
                        let n = file
                            .seek_read(&mut out[done..], offset + done as u64)
                            .expect("tier-b: backing read");
                        assert!(n > 0, "tier-b: unexpected EOF");
                        done += n;
                    }
                }
                out
            }
        }
    }
}

impl BlockStore for TierBStore {
    type Bv = Deferred;
    fn put(
        &mut self,
        b: Block,
        _at: BlockIndex,
        _user_times: &[Option<crate::model::EpochSeconds>],
    ) -> Deferred {
        // `serde_json` over the `Block` model (see the model's serde derives). Serialization is
        // total for the block vocabulary (only Strings/ints/Options/Vecs/plain enums), so this never
        // fails in practice; a corrupt/older backing surfaces at read time (reset ⇒ rebuild).
        let mut json = serde_json::to_vec(&b).expect("tier-b: Block is serializable");
        let size = json.len() as u32;
        json.push(b'\n');
        match &mut self.backing {
            Backing::Mem(buf) => {
                let offset = buf.len() as ByteOffset;
                buf.extend_from_slice(&json);
                Deferred { offset, size }
            }
            Backing::File { file, len, .. } => {
                use std::io::Write;
                let offset = *len;
                // One write per committed block (framed record). A locator is returned only
                // after the bytes are down, so `get` on a returned locator always succeeds.
                file.write_all(&json).expect("tier-b: backing append");
                *len += json.len() as u64;
                Deferred { offset, size }
            }
        }
    }
    fn reset(&mut self) {
        match &mut self.backing {
            Backing::Mem(buf) => buf.clear(),
            Backing::File { file, len, .. } => {
                use std::io::Seek;
                // Truncate AND rewind: `set_len` does not move the write cursor, so without the
                // seek the next append would land at the old position (a hole of zeros before it)
                // while its locator claims the offset from the fresh `len` counter.
                let _ = file.set_len(0);
                let _ = file.seek(std::io::SeekFrom::Start(0));
                *len = 0;
            }
        }
    }
}

impl crate::engine::session::BlockRead for TierBStore {
    fn get<'a>(&'a self, d: &'a Deferred) -> Cow<'a, Block> {
        let bytes = self.read_at(d.offset, d.size as usize);
        Cow::Owned(serde_json::from_slice(&bytes).expect("tier-b: valid block record"))
    }
}

/// A `Session<Deferred>` paired with its tier-b backing — the client-side handle that can actually
/// read block content. Implements [`BlockAccess`] by seeking to each locator and decoding on demand;
/// the [`SessionIndex`](crate::engine::SessionIndex) / metrics / `sub_agents` come free from the
/// session and never touch the backing.
pub struct TierBSession {
    /// The offset-table session (`blocks: Vec<Deferred>`) + the `Bv`-free index/metrics/sub_agents.
    pub session: Session<Deferred>,
    /// The append-only backing the locators point into.
    backing: Vec<u8>,
}

impl TierBSession {
    /// Pair a `Session<Deferred>` (its `blocks` are locators) with the backing they index.
    pub fn new(session: Session<Deferred>, backing: Vec<u8>) -> Self {
        Self { session, backing }
    }

    /// Read + decode the raw bytes for locator `d` (no caching).
    fn read(&self, d: Deferred) -> Block {
        let start = d.offset as usize;
        let end = start + d.size as usize;
        serde_json::from_slice(&self.backing[start..end]).expect("tier-b: valid block record")
    }
}

impl BlockAccess for TierBSession {
    /// Materialize the block at flat index `i`: a decode from the backing for a committed block, a
    /// borrow for the resident provisional tail.
    fn block(&self, i: BlockIndex) -> Cow<'_, Block> {
        let c = self.session.committed.len();
        if i < c {
            Cow::Owned(self.read(self.session.committed[i]))
        } else {
            Cow::Borrowed(&self.session.provisional[i - c])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::session::{BlockRead, BlockStore};
    use crate::model::{AgentStatus, Attachment, AttachmentContent, AttachmentKind, SubAgent};

    fn sample_blocks() -> Vec<Block> {
        vec![
            Block::UserText("hello **world**".into()),
            Block::AssistantText("hi there".into()),
            Block::Thinking {
                text: "pondering".into(),
                duration_secs: Some(3),
                tools: vec![Block::ToolUse {
                    name: "Bash".into(),
                    target: "ls -la".into(),
                    diffs: vec![],
                    output: Some("a\nb".into()),
                    patch: None,
                    read_lines: None,
                }],
            },
            Block::ToolUse {
                name: "Edit".into(),
                target: "src/x.rs".into(),
                diffs: vec![("old".into(), "new".into())],
                output: None,
                patch: None,
                read_lines: Some(42),
            },
            Block::SubAgent(SubAgent {
                agent_id: "aXYZ".into(),
                tool_use_id: "toolu_1".into(),
                agent_type: "code-reviewer".into(),
                description: "review it".into(),
                prompt: "please review".into(),
                status: AgentStatus::AsyncLaunched,
                result: None,
                output_file: Some("/t/aXYZ.output".into()),
                blocks: vec![],
                subtree_cost: Some(0.0123),
            }),
            Block::AgentDone {
                agent_id: "aXYZ".into(),
                agent_type: "code-reviewer".into(),
                description: "review it".into(),
                status: AgentStatus::Completed,
                result: Some("two gaps".into()),
            },
            Block::Attachment(Attachment {
                kind: AttachmentKind::Image,
                name: "shot.png".into(),
                path: Some("/t/shot.png".into()),
                content: AttachmentContent::Deferred { at: 99, index: 1 },
            }),
            Block::Command {
                name: "/compact".into(),
                args: "".into(),
                output: vec!["done".into()],
            },
        ]
    }

    // A single-snapshot batch parse routed through a tier-b store must reproduce the in-memory
    // parse block-for-block — the on-disk locators + backing decode back to identical `Block`s, and
    // the `Bv`-free index/metrics/user_times are unchanged (they never touch the store).

    /// The file-backed mode: puts append to a real file, gets read back positionally, `reset`
    /// truncates (fresh offsets), and the tracked length equals the file's.
    #[test]
    fn file_backed_store_round_trips_and_resets() {
        let path = std::env::temp_dir().join(format!("tierb-file-{}.blocks", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut store = TierBStore::file(&path).unwrap();
        let blocks = sample_blocks();
        let locs: Vec<Deferred> = blocks
            .iter()
            .enumerate()
            .map(|(at, b)| store.put(b.clone(), at, &[]))
            .collect();
        for (d, b) in locs.iter().zip(&blocks) {
            assert_eq!(&*store.get(d), b, "file round-trip");
        }
        assert_eq!(
            store.len() as u64,
            std::fs::metadata(&path).unwrap().len(),
            "tracked length == file length"
        );

        store.reset();
        assert!(store.is_empty());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0, "truncated");
        let d = store.put(blocks[0].clone(), 0, &[]);
        assert_eq!(d.offset, 0, "offsets restart after reset");
        assert_eq!(&*store.get(&d), &blocks[0]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn tier_b_put_then_read_round_trips_every_block() {
        let blocks = sample_blocks();
        let mut store = TierBStore::new();
        let locators: Vec<Deferred> = blocks
            .iter()
            .enumerate()
            .map(|(at, b)| store.put(b.clone(), at, &[]))
            .collect();
        let backing = store.into_backing();

        // Locators are non-overlapping, in order, and cover the buffer densely (records + newlines).
        let mut cursor = 0u64;
        for d in &locators {
            assert_eq!(d.offset, cursor, "records are append-only, in order");
            cursor += d.size as u64 + 1; // + framing newline
        }
        assert_eq!(cursor as usize, backing.len(), "no gaps in the backing");

        // Every block decodes byte-for-byte back to what was stored (via BlockAccess).
        let session = Session {
            agent: crate::Agent::CLAUDE,
            cwd: None,
            committed: locators,
            provisional: vec![],
            user_times: vec![],
            metrics: Default::default(),
            index: Default::default(),
            sub_agents: Default::default(),
            tasks: Default::default(),
        };
        let tb = TierBSession::new(session, backing);
        for (i, original) in blocks.iter().enumerate() {
            assert_eq!(
                &*tb.block(i),
                original,
                "block {i} round-trips through tier-b"
            );
        }
    }
}
