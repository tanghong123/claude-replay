//! Tier-b: **on-disk (or off-heap) block storage**. A [`BlockStore`] whose `Bv` is a small
//! [`Deferred`] locator, so a `Session<Deferred>` holds only the **O(N) offset table** while each
//! block's *content* lives in an append-only backing buffer. Reading a block is a seek + a
//! `serde_json` decode ([`TierBSession`] implements [`BlockAccess`] over the backing) — the raw
//! [`Block`] is re-materialized on demand and dropped after use.
//!
//! Correctness note (put-once): [`SessionAccumulator::snapshot`](crate::engine::SessionAccumulator)
//! currently maps *every* block through [`BlockStore::put`] on *every* snapshot (identity for the
//! in-memory default, so harmless there). For an append-only tier-b backing that means a repeated
//! snapshot would append duplicate copies — so tier-b is correct today only for a **single-snapshot
//! batch** parse (put each block exactly once). The Stage-3 emit-and-drop rewrite makes `put`
//! fire once at the durability frontier, which is what unlocks tier-b for the live/repeated-snapshot
//! path. Until then, drive it via one `advance_reader(..)` + one `snapshot()`.

use crate::engine::session::{BlockAccess, BlockStore, Session};
use crate::engine::SessionIndex;
use crate::metrics::Metrics;
use crate::model::{AgentId, Block, BlockIndex, ByteOffset, SubAgentMeta};
use crate::Agent;
use std::borrow::Cow;
use std::collections::BTreeMap;
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

/// An append-only tier-b block store backed by an in-memory byte buffer. `put` serializes the block
/// (`serde_json`, newline-framed — compact enough since this lives off the resident heap / on disk)
/// and returns its [`Deferred`] locator. Hand the finished [`into_backing`](Self::into_backing) to a
/// [`TierBSession`] to read blocks back.
#[derive(Default)]
pub struct TierBStore {
    buf: Vec<u8>,
}

impl TierBStore {
    /// A fresh, empty tier-b store.
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// Consume the store, returning the append-only backing bytes (pair with a
    /// `Session<Deferred>` in a [`TierBSession`] to read blocks).
    pub fn into_backing(self) -> Vec<u8> {
        self.buf
    }

    /// The current backing length (next block's offset) — the total bytes written so far.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Whether nothing has been stored yet.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}

impl BlockStore for TierBStore {
    type Bv = Deferred;
    fn put(&mut self, b: Block, _at: BlockIndex) -> Deferred {
        let offset = self.buf.len() as ByteOffset;
        // `serde_json` over the `Block` model (see the model's serde derives). Serialization is
        // total for the block vocabulary (only Strings/ints/Options/Vecs/plain enums), so this never
        // fails in practice; a corrupt/older backing surfaces at read time (reset ⇒ rebuild).
        let json = serde_json::to_vec(&b).expect("tier-b: Block is serializable");
        self.buf.extend_from_slice(&json);
        self.buf.push(b'\n');
        Deferred {
            offset,
            size: json.len() as u32,
        }
    }
    fn get<'a>(&'a self, d: &'a Deferred) -> Cow<'a, Block> {
        let start = d.offset as usize;
        Cow::Owned(
            serde_json::from_slice(&self.buf[start..start + d.size as usize])
                .expect("tier-b: valid block record"),
        )
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

    /// **Persist** this session to `dir` so it can be reloaded without re-folding the transcript
    /// (restart / `SessionCache` re-admit survival). Writes two files: `blocks.tierb` (the
    /// append-only content backing — the bulk) and `session.json` (a small sidecar: agent, cwd,
    /// `user_times`, `metrics`, `sub_agents`, and the `Vec<Deferred>` offset table).
    ///
    /// The [`SessionIndex`] is deliberately **not** written — it is fully derivable from the blocks
    /// plus `user_times`, so it's rebuilt on [`load`](Self::load). (That also sidesteps serializing
    /// the index's `&'static str` count keys.) `Metrics` can't be re-derived from blocks — its token
    /// tallies come from the raw transcript — so it is persisted.
    pub fn persist(&self, dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(dir)?;
        std::fs::write(dir.join(BLOCKS_FILE), &self.backing)?;
        let sidecar = Sidecar {
            agent: self.session.agent,
            cwd: self.session.cwd.clone(),
            user_times: self.session.user_times.clone(),
            metrics: self.session.metrics.clone(),
            sub_agents: self.session.sub_agents.clone(),
            committed: self.session.committed.clone(),
            provisional: self.session.provisional.clone(),
        };
        let json = serde_json::to_vec(&sidecar).map_err(to_io)?;
        std::fs::write(dir.join(SIDECAR_FILE), json)?;
        Ok(())
    }

    /// **Reload** a session persisted by [`persist`](Self::persist): read the backing + sidecar and
    /// rebuild the [`SessionIndex`] by folding the blocks back through [`SessionIndex::push`] (which
    /// equals a batch `build`). The result is byte-identical to the session that was persisted —
    /// same blocks, index, metrics, `user_times`, `sub_agents` — for the cost of one pass over the
    /// backing instead of a full transcript re-fold.
    pub fn load(dir: &Path) -> io::Result<Self> {
        let backing = std::fs::read(dir.join(BLOCKS_FILE))?;
        let sidecar: Sidecar =
            serde_json::from_slice(&std::fs::read(dir.join(SIDECAR_FILE))?).map_err(to_io)?;

        // Rebuild the index incrementally over the committed blocks (re-decoded from the backing)
        // then the resident provisional tail — advancing the user-turn cursor exactly as
        // `SessionIndex::build` does. Committed content is never held all-resident.
        let mut index = SessionIndex::default();
        let mut turn_i = 0usize;
        let mut at = 0usize;
        let mut push = |index: &mut SessionIndex, b: &Block| {
            let turn_time = if matches!(b, Block::UserText(_) | Block::Command { .. }) {
                let t = sidecar.user_times.get(turn_i).copied().flatten();
                turn_i += 1;
                t
            } else {
                None
            };
            index.push(at, b, turn_time);
            at += 1;
        };
        for d in &sidecar.committed {
            let start = d.offset as usize;
            let b: Block =
                serde_json::from_slice(&backing[start..start + d.size as usize]).map_err(to_io)?;
            push(&mut index, &b);
        }
        for b in &sidecar.provisional {
            push(&mut index, b);
        }

        let session = Session {
            agent: sidecar.agent,
            cwd: sidecar.cwd,
            committed: sidecar.committed,
            provisional: sidecar.provisional,
            user_times: sidecar.user_times,
            metrics: sidecar.metrics,
            index,
            sub_agents: sidecar.sub_agents,
        };
        Ok(Self { session, backing })
    }
}

/// File names inside a persisted tier-b session directory.
const BLOCKS_FILE: &str = "blocks.tierb";
const SIDECAR_FILE: &str = "session.json";

/// The persisted metadata beside the content backing. Everything that is **not** re-derivable from
/// the blocks (so the index is absent — rebuilt on load).
#[derive(serde::Serialize, serde::Deserialize)]
struct Sidecar {
    agent: Agent,
    cwd: Option<PathBuf>,
    user_times: Vec<Option<crate::model::EpochSeconds>>,
    metrics: Metrics,
    sub_agents: BTreeMap<AgentId, SubAgentMeta>,
    committed: Vec<Deferred>,
    provisional: Vec<Block>,
}

/// Map a `serde_json` error into an `io::Error` (persist/load surface `io::Result`).
fn to_io(e: serde_json::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e)
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
    use crate::engine::session::BlockStore;
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
    #[test]
    fn tier_b_session_matches_in_memory_parse() {
        use crate::engine::session::BlockAccess;
        use crate::engine::SessionAccumulator;
        use crate::Agent;
        use std::io::Cursor;

        let jsonl = r##"
{"type":"user","message":{"content":[{"type":"text","text":"hi"}]}}
{"type":"assistant","message":{"content":[{"type":"text","text":"hello"}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_A","name":"Bash","input":{"command":"ls"}}]}}
{"type":"user","toolUseResult":{"stdout":"a\nb"},"message":{"content":[{"type":"tool_result","tool_use_id":"toolu_A","content":"a\nb"}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_B","name":"Agent","input":{"subagent_type":"code-reviewer","description":"review","prompt":"review it"}}]}}
{"type":"user","toolUseResult":{"agentId":"aXYZ1234","status":"async_launched","outputFile":"/t/aXYZ1234.output"},"message":{"content":[{"type":"tool_result","tool_use_id":"toolu_B","content":"async_launched"}]}}
{"type":"queue-operation","operation":"enqueue","content":"<task-notification>\n<task-id>aXYZ1234</task-id>\n<tool-use-id>toolu_B</tool-use-id>\n<status>completed</status>\n<summary>Agent \"review\" finished</summary>\n<result>Two gaps.</result>\n</task-notification>"}
"##;

        let mut mem = SessionAccumulator::new(Agent::Claude);
        mem.advance_reader(&mut Cursor::new(jsonl.as_bytes()))
            .unwrap();
        let mem_session = mem.snapshot(); // Session<Block>

        let mut tb = SessionAccumulator::with_store(Agent::Claude, TierBStore::new());
        tb.advance_reader(&mut Cursor::new(jsonl.as_bytes()))
            .unwrap();
        let tb_session = tb.snapshot(); // Session<Deferred>
        let backing = tb.into_store().into_backing();
        let tb_sess = TierBSession::new(tb_session, backing);

        assert_eq!(
            mem_session.block_count(),
            tb_sess.session.block_count(),
            "same block count"
        );
        assert!(
            mem_session.block_count() >= 5,
            "fixture produced real blocks"
        );
        for i in 0..mem_session.block_count() {
            assert_eq!(
                mem_session.block(i),
                tb_sess.block(i),
                "block {i} matches in-memory"
            );
        }
        // The Bv-free metadata is store-independent.
        assert_eq!(mem_session.user_times, tb_sess.session.user_times);
        assert_eq!(mem_session.sub_agents, tb_sess.session.sub_agents);
    }

    // Persist a tier-b session to disk and reload it: every block, the rebuilt index, metrics,
    // user_times, and sub_agents must match the pre-persist session exactly — a restart survives
    // without re-folding the transcript.
    #[test]
    fn persist_then_load_reconstructs_the_session() {
        use crate::engine::session::BlockAccess;
        use crate::engine::SessionAccumulator;
        use crate::Agent;
        use std::io::Cursor;

        let jsonl = r##"
{"type":"user","message":{"content":[{"type":"text","text":"start"}]}}
{"type":"assistant","message":{"content":[{"type":"text","text":"ok"}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_A","name":"Agent","input":{"subagent_type":"code-reviewer","description":"review","prompt":"review it"}}]}}
{"type":"user","toolUseResult":{"agentId":"aXYZ1234","status":"async_launched","outputFile":"/t/aXYZ1234.output"},"message":{"content":[{"type":"tool_result","tool_use_id":"toolu_A","content":"async_launched"}]}}
{"type":"queue-operation","operation":"enqueue","content":"<task-notification>\n<task-id>aXYZ1234</task-id>\n<tool-use-id>toolu_A</tool-use-id>\n<status>completed</status>\n<summary>Agent \"review\" finished</summary>\n<result>Two gaps.</result>\n</task-notification>"}
{"type":"user","message":{"content":[{"type":"text","text":"thanks"}]}}
"##;

        let mut acc = SessionAccumulator::with_store(Agent::Claude, TierBStore::new());
        acc.advance_reader(&mut Cursor::new(jsonl.as_bytes()))
            .unwrap();
        let session = acc.snapshot();
        let backing = acc.into_store().into_backing();
        let before = TierBSession::new(session, backing);

        let dir = std::env::temp_dir().join(format!("tierb-persist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        before.persist(&dir).unwrap();
        let after = TierBSession::load(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(before.session.block_count(), after.session.block_count());
        assert!(before.session.block_count() >= 5);
        for i in 0..before.session.block_count() {
            assert_eq!(
                before.block(i),
                after.block(i),
                "block {i} survives persist→load"
            );
        }
        assert_eq!(before.session.agent, after.session.agent);
        assert_eq!(before.session.cwd, after.session.cwd);
        assert_eq!(before.session.user_times, after.session.user_times);
        assert_eq!(before.session.metrics, after.session.metrics);
        assert_eq!(before.session.sub_agents, after.session.sub_agents);
        // The index is rebuilt on load — must equal the original (compared via Debug; no PartialEq).
        assert_eq!(
            format!("{:?}", before.session.index),
            format!("{:?}", after.session.index),
            "rebuilt index equals the persisted session's"
        );
    }

    #[test]
    fn tier_b_put_then_read_round_trips_every_block() {
        let blocks = sample_blocks();
        let mut store = TierBStore::new();
        let locators: Vec<Deferred> = blocks
            .iter()
            .enumerate()
            .map(|(at, b)| store.put(b.clone(), at))
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
            agent: crate::Agent::Claude,
            cwd: None,
            committed: locators,
            provisional: vec![],
            user_times: vec![],
            metrics: Default::default(),
            index: Default::default(),
            sub_agents: Default::default(),
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
