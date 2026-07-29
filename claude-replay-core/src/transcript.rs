//! The [`Transcript`] source handle — the canonical way to name a transcript file and decode
//! it.
//!
//! A `Transcript` is the cheap, clonable handle (agent + path) that identifies one transcript
//! source. It is the single object the rest of the API threads instead of a bare `(agent,
//! path)` pair: from it a caller can [`parse`](Transcript::parse) a whole [`Session`],
//! [`parse_enriched`](Transcript::parse_enriched) the sub-agent tree, open an incremental
//! [`follow`](Transcript::follow)er, or resolve a deferred attachment locator with
//! [`load_attachment`](Transcript::load_attachment).
//!
//! It holds no open file and caches no parsed content. Clones in one operation share the
//! agent-specific relationship resolver used to locate child transcripts and normalize their
//! stable ids.
//!
//! ## Attachment loading
//! A resident [`Session`] holds only a locator per attachment
//! ([`AttachmentContent::Deferred`](crate::model::AttachmentContent::Deferred)) — never the
//! embedded bytes. [`Transcript::load_attachment`] turns such a locator into content on demand:
//! it seeks to the recorded byte offset, reads that ONE line, and re-runs the same extraction
//! the parser uses to yield a [`LoadedAttachment`]. It reads on demand each call and returns
//! owned bytes, so at most one attachment is resident at a time. Caching, if ever wanted, is a
//! presentation-layer concern.

use std::io::{self, BufRead, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::engine::session::{
    replace_session_blocks, resolve_session_relationships, BlockStore, Session,
};
use crate::follow::FollowParser;
use crate::model::{Block, ByteOffset, LoadedAttachment, UsdCost};
use crate::{Agent, SessionGraph};

/// A cheap, clonable handle to a transcript file — the canonical source object the API threads
/// instead of a bare `(agent, path)` pair. Holds only the agent + path; loads nothing until
/// asked, caches nothing between calls.
#[derive(Debug, Clone)]
pub struct Transcript {
    agent: Agent,
    path: PathBuf,
    graph: SessionGraph,
}

impl Transcript {
    /// A handle to the transcript at `path`, decoded as `agent` (typically `session.agent`).
    pub fn open(agent: Agent, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        Self {
            agent,
            graph: SessionGraph::open(agent, &path),
            path,
        }
    }

    /// A handle to the transcript at `path`, auto-detecting the agent from the file's head
    /// (see [`detect_agent`](crate::discover::detect_agent)).
    pub fn detect(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let agent = crate::discover::detect_agent(&path);
        Self {
            agent,
            graph: SessionGraph::open(agent, &path),
            path,
        }
    }

    /// The agent this transcript is decoded as.
    pub fn agent(&self) -> Agent {
        self.agent
    }

    /// The transcript path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A related transcript in the same operation. The returned handle shares this
    /// operation's relationship cache and anchored tree boundary.
    pub fn related(&self, path: impl Into<PathBuf>) -> Self {
        Self {
            agent: self.agent,
            path: path.into(),
            graph: self.graph.clone(),
        }
    }

    /// Resolve a sub-agent by stable id inside this operation's anchored tree.
    pub fn subagent(&self, child_id: &str) -> Option<Self> {
        self.graph
            .subagent_source(&self.path, child_id)
            .map(|path| self.related(path))
    }

    /// Parse the whole file into a [`Session`] — blocks + per-turn times + folded metrics + the
    /// derived index + cwd — from one streaming pass. This is the real implementation behind
    /// [`parse_session_as`](crate::parse_session_as); the free functions are thin wrappers over
    /// it. Sub-agent child transcripts are NOT loaded (`SubAgent.blocks` stays empty); use
    /// [`parse_enriched`](Transcript::parse_enriched) for the whole tree.
    pub fn parse(&self) -> io::Result<Session> {
        // Parsing ignores CLI flags (fold is a view-layer concern), so the parse API takes no
        // `Args` — that keeps clap out of the core. Derived from the incremental fold: feed a
        // `SessionAccumulator` line-by-line (one line resident, so a multi-gigabyte transcript never
        // balloons into memory) — blocks + per-turn times + metrics fold in the SAME pass (M10),
        // one file read.
        let mut b = crate::engine::builder::SessionAccumulator::new(self.agent);
        let mut reader = io::BufReader::new(std::fs::File::open(&self.path)?);
        b.advance_reader(&mut reader)?; // one line resident, byte offsets tracked
        let mut s = b.snapshot();
        s.cwd = crate::discover::session_cwd(&self.path); // the builder leaves cwd None; fill it
        resolve_session_relationships(&mut s, &self.graph, &self.path);
        Ok(s)
    }

    /// Like [`parse`](Transcript::parse), but also loads the **sub-agent tree** — each
    /// `SubAgent`'s child transcript (recursively) into its `blocks`. Only the nested
    /// `SubAgent.blocks` change — the top-level `blocks`/`index`/`metrics` are identical to
    /// [`parse`](Transcript::parse). The real implementation behind
    /// [`parse_session_enriched_as`](crate::parse_session_enriched_as).
    pub fn parse_enriched(&self) -> io::Result<Session> {
        self.parse_enriched_inner(&mut std::collections::HashSet::new())
    }

    /// Open an incremental [`FollowParser`] over this source (folds the whole current file on
    /// the first poll, then only appended bytes). The constructor
    /// [`FollowParser::open`](crate::FollowParser::open) is equivalent; this is the handle-based
    /// entry point.
    pub fn follow(&self) -> FollowParser {
        FollowParser::open_with_graph(self.agent, &self.path, self.graph.clone())
    }

    /// Follow this operation source using an explicit committed-block storage policy while
    /// preserving the operation-scoped relationship resolver.
    pub fn follow_with_store<S: BlockStore>(&self, store: S) -> FollowParser<S> {
        FollowParser::with_store_and_graph(self.agent, &self.path, store, self.graph.clone())
    }

    /// Re-apply relationship normalization to blocks restored from the workspace presenter's
    /// persisted cache. Normal parse/follow consumers receive normalized blocks directly.
    #[cfg(feature = "presenter-internals")]
    #[doc(hidden)]
    pub fn resolve_persisted_blocks(&self, blocks: &mut [Block]) {
        self.graph.resolve_relationships(&self.path, blocks);
    }

    /// Load the content embedded at byte offset `at`, `index`-th content-bearing attachment on
    /// that line (see [`Deferred`](crate::model::AttachmentContent::Deferred)). Opens the file,
    /// seeks to `at`, reads that ONE line, and re-runs the agent's attachment extraction —
    /// O(1) memory (one line, one attachment). Returns `Ok(None)` when the line holds no such
    /// loadable attachment (a stale locator / a non-content-bearing line).
    pub fn load_attachment(
        &self,
        at: ByteOffset,
        index: usize,
    ) -> io::Result<Option<LoadedAttachment>> {
        let file = std::fs::File::open(&self.path)?;
        let mut reader = io::BufReader::new(file);
        reader.seek(SeekFrom::Start(at))?;
        let mut line = String::new();
        reader.read_line(&mut line)?;
        Ok(crate::adapter::adapter(self.agent).load_attachment(&line, index))
    }

    fn parse_enriched_inner(
        &self,
        seen: &mut std::collections::HashSet<PathBuf>,
    ) -> io::Result<Session> {
        let mut session = self.parse()?;
        let identity = std::fs::canonicalize(&self.path).unwrap_or_else(|_| self.path.clone());
        if !seen.insert(identity) {
            return Ok(session);
        }
        let mut blocks = session.blocks();
        self.enrich_blocks(&mut blocks, seen);
        replace_session_blocks(&mut session, blocks, &self.graph, &self.path);
        Ok(session)
    }

    fn enrich_blocks(&self, blocks: &mut [Block], seen: &mut std::collections::HashSet<PathBuf>) {
        for block in blocks {
            let Block::SubAgent(agent) = block else {
                continue;
            };
            if agent.agent_id.is_empty() {
                continue;
            }
            let Some(child) = self.subagent(&agent.agent_id) else {
                continue;
            };
            let identity =
                std::fs::canonicalize(child.path()).unwrap_or_else(|_| child.path().to_path_buf());
            if seen.contains(&identity) {
                continue;
            }
            let Ok(child_session) = child.parse_enriched_inner(seen) else {
                continue;
            };
            let child_blocks = child_session.blocks();
            agent.subtree_cost = subtree_cost(&child_session.metrics, &child_blocks);
            agent.blocks = child_blocks;
        }
    }
}

fn subtree_cost(metrics: &crate::Metrics, blocks: &[Block]) -> Option<UsdCost> {
    let descendants: UsdCost = blocks
        .iter()
        .filter_map(|block| match block {
            Block::SubAgent(agent) => agent.subtree_cost,
            _ => None,
        })
        .sum();
    match metrics.cost_usd {
        Some(own) => Some(own + descendants),
        None if descendants > 0.0 => Some(descendants),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Attachment, AttachmentContent, Block};
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn tmp(body: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "cr-att-{}-{}.jsonl",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::File::create(&p)
            .unwrap()
            .write_all(body.as_bytes())
            .unwrap();
        p
    }

    fn deferred(b: &Block) -> Option<(ByteOffset, usize)> {
        match b {
            Block::Attachment(Attachment {
                content: AttachmentContent::Deferred { at, index },
                ..
            }) => Some((*at, *index)),
            _ => None,
        }
    }

    /// A parsed `file`/`plan`/`image` attachment carries a `Deferred { at }` locator whose `at`
    /// is the byte offset of exactly its own transcript line — and `Transcript::load_attachment`
    /// round-trips that locator back to the embedded bytes. No content is ever held resident.
    #[test]
    fn deferred_offsets_point_at_the_right_line_and_load_roundtrips() {
        // Three content-bearing attachments across three lines (file, image-in-prompt, plan),
        // plus a path-only `edited_text_file` (→ `None`, no offset).
        let l0 = r##"{"type":"attachment","timestamp":"2026-06-30T03:00:00.000Z","attachment":{"type":"file","filename":"/w/backlog.md","displayPath":"backlog.md","content":{"type":"text","file":{"filePath":"/w/backlog.md","content":"# Backlog\nitem"}}}}"##;
        let l1 = r##"{"type":"user","timestamp":"2026-06-30T03:00:01.000Z","message":{"content":[{"type":"text","text":"see"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"Zm9v"}}]}}"##;
        let l2 = r##"{"type":"attachment","timestamp":"2026-06-30T03:00:02.000Z","attachment":{"type":"plan_file_reference","planFilePath":"/p/plan.md","planContent":"# Plan"}}"##;
        let l3 = r##"{"type":"attachment","timestamp":"2026-06-30T03:00:03.000Z","attachment":{"type":"edited_text_file","filename":"/w/x.rs","snippet":"1\tx"}}"##;
        let body = format!("{l0}\n{l1}\n{l2}\n{l3}\n");
        let path = tmp(&body);

        let s = crate::engine::parse_session_as(Agent::Claude, &path).unwrap();
        let blocks = s.blocks();
        let atts: Vec<&Block> = blocks
            .iter()
            .filter(|b| matches!(b, Block::Attachment(_)))
            .collect();
        assert_eq!(atts.len(), 4, "{blocks:?}");

        // Expected byte offsets = the start of each line in `body`.
        let off0 = 0u64;
        let off1 = (l0.len() + 1) as u64;
        let off2 = off1 + (l1.len() + 1) as u64;
        assert_eq!(deferred(atts[0]), Some((off0, 0)), "file locator");
        assert_eq!(deferred(atts[1]), Some((off1, 0)), "image locator");
        assert_eq!(deferred(atts[2]), Some((off2, 0)), "plan locator");
        assert_eq!(deferred(atts[3]), None, "edited is path-only (None)");

        // Round-trip each locator through the Transcript loader → the embedded bytes.
        let t = Transcript::open(Agent::Claude, &path);
        assert_eq!(
            t.load_attachment(off0, 0).unwrap(),
            Some(LoadedAttachment::Text("# Backlog\nitem".into()))
        );
        assert_eq!(
            t.load_attachment(off1, 0).unwrap(),
            Some(LoadedAttachment::Base64 {
                mime: "image/png".into(),
                b64: "Zm9v".into()
            })
        );
        assert_eq!(
            t.load_attachment(off2, 0).unwrap(),
            Some(LoadedAttachment::Text("# Plan".into()))
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Two images pasted into ONE user message get distinct within-line indices (0, 1), and the
    /// loader returns each one's own bytes for its index — the multi-attachment-per-line case.
    #[test]
    fn multiple_images_on_one_line_get_distinct_indices() {
        let line = r##"{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":[{"type":"image","source":{"type":"base64","media_type":"image/png","data":"AAA="}},{"type":"image","source":{"type":"base64","media_type":"image/jpeg","data":"BBB="}}]}}"##;
        let path = tmp(&format!("{line}\n"));
        let s = crate::engine::parse_session_as(Agent::Claude, &path).unwrap();
        let blocks = s.blocks();
        let locs: Vec<(ByteOffset, usize)> = blocks.iter().filter_map(deferred).collect();
        assert_eq!(locs, vec![(0, 0), (0, 1)], "{blocks:?}");

        let t = Transcript::open(Agent::Claude, &path);
        assert_eq!(
            t.load_attachment(0, 0).unwrap(),
            Some(LoadedAttachment::Base64 {
                mime: "image/png".into(),
                b64: "AAA=".into()
            })
        );
        assert_eq!(
            t.load_attachment(0, 1).unwrap(),
            Some(LoadedAttachment::Base64 {
                mime: "image/jpeg".into(),
                b64: "BBB=".into()
            })
        );
        let _ = std::fs::remove_file(&path);
    }

    /// `detect` sniffs the agent from the file head, and `parse`/`parse_enriched`/`follow`
    /// on the handle equal the free-function entry points they back.
    #[test]
    fn handle_methods_match_the_free_functions() {
        let body = concat!(
            r#"{"type":"user","cwd":"/repo","message":{"role":"user","content":[{"type":"text","text":"hi"}]},"timestamp":"2026-07-26T10:00:00Z"}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hello"}],"usage":{"input_tokens":3,"output_tokens":5}},"timestamp":"2026-07-26T10:00:01Z"}"#,
            "\n",
        );
        let path = tmp(body);

        let detected = Transcript::detect(&path);
        assert_eq!(detected.agent(), Agent::Claude);
        assert_eq!(detected.path(), path.as_path());

        let via_handle = Transcript::open(Agent::Claude, &path).parse().unwrap();
        let via_free = crate::engine::parse_session_as(Agent::Claude, &path).unwrap();
        assert_eq!(format!("{via_handle:?}"), format!("{via_free:?}"));

        let enriched_handle = detected.parse_enriched().unwrap();
        let enriched_free = crate::engine::parse_session_enriched(&path).unwrap();
        assert_eq!(format!("{enriched_handle:?}"), format!("{enriched_free:?}"));

        // `follow`'s first poll folds the whole file → equals a full parse.
        let mut f = detected.follow();
        let s = f.poll_session().unwrap().expect("content");
        assert_eq!(
            format!("{:?}", s.blocks()),
            format!("{:?}", via_free.blocks())
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn codex_transcript_resolves_subagent_map_source_and_eager_content() {
        static N: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "cr-transcript-codex-tree-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let sessions = root.join("sessions/2026/07/28");
        std::fs::create_dir_all(&sessions).unwrap();
        let parent = sessions.join("rollout-parent.jsonl");
        let child = sessions.join("rollout-child.jsonl");
        std::fs::write(
            &parent,
            concat!(
                r#"{"type":"session_meta","payload":{"id":"parent","cwd":"/repo","source":"cli"}}"#,
                "\n",
                r#"{"type":"response_item","payload":{"type":"function_call","name":"spawn_agent","namespace":"collaboration","call_id":"spawn-1","arguments":"{\"task_name\":\"review\",\"message\":\"hidden\"}"}}"#,
                "\n",
                r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"spawn-1","output":"{\"task_name\":\"/root/review\"}"}}"#,
                "\n",
                r#"{"type":"response_item","payload":{"type":"agent_message","author":"/root/review","content":[{"type":"input_text","text":"Message Type: FINAL_ANSWER\nPayload:\nPASS"}]}}"#,
                "\n",
            ),
        )
        .unwrap();
        std::fs::write(
            &child,
            concat!(
                r#"{"type":"session_meta","payload":{"id":"child","cwd":"/repo","source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent","agent_path":"/root/review","agent_nickname":"Nash"}}}}}"#,
                "\n",
                r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"child body"}]}}"#,
                "\n",
            ),
        )
        .unwrap();

        let transcript = Transcript::open(Agent::Codex, &parent);
        let flat = transcript.parse().unwrap();
        let meta = flat.sub_agents.get("child").expect("resolved child entity");
        assert_eq!(meta.status, crate::AgentStatus::Completed);
        assert_eq!(meta.transcript.as_deref(), Some(child.as_path()));
        assert!(matches!(
            &flat.blocks()[meta.spawn_at],
            Block::SubAgent(agent)
                if agent.agent_id == "child"
                    && agent.agent_type == "Nash"
                    && agent.status == crate::AgentStatus::Running
                    && agent.blocks.is_empty()
        ));

        let enriched = transcript.parse_enriched().unwrap();
        assert!(enriched.blocks().iter().any(|block| matches!(
            block,
            Block::SubAgent(agent)
                if agent.agent_id == "child"
                    && agent.blocks.iter().any(
                        |child_block| matches!(child_block, Block::UserText(text) if text == "child body")
                    )
        )));

        std::fs::remove_dir_all(root).unwrap();
    }
}
