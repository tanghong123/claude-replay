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
//! It is **stateless** — it holds no open handle and caches nothing. Every method opens the
//! file, does its work, and returns owned data the caller drops after use. `Clone` is cheap
//! (an [`Agent`] tag + a [`PathBuf`]).
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

use crate::engine::session::{populate_sub_agent_transcripts, Session};
use crate::follow::FollowParser;
use crate::model::{ByteOffset, LoadedAttachment};
use crate::Agent;

/// A cheap, clonable handle to a transcript file — the canonical source object the API threads
/// instead of a bare `(agent, path)` pair. Holds only the agent + path; loads nothing until
/// asked, caches nothing between calls.
#[derive(Debug, Clone)]
pub struct Transcript {
    agent: Agent,
    path: PathBuf,
}

impl Transcript {
    /// A handle to the transcript at `path`, decoded as `agent` (typically `session.agent`).
    pub fn open(agent: Agent, path: impl Into<PathBuf>) -> Self {
        Self {
            agent,
            path: path.into(),
        }
    }

    /// A handle to the transcript at `path`, auto-detecting the agent from the file's head
    /// (see [`detect_agent`](crate::discover::detect_agent)).
    pub fn detect(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let agent = crate::discover::detect_agent(&path);
        Self { agent, path }
    }

    /// The agent this transcript is decoded as.
    pub fn agent(&self) -> Agent {
        self.agent
    }

    /// The transcript path.
    pub fn path(&self) -> &Path {
        &self.path
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
        let mut b = crate::SessionAccumulator::new(crate::adapter::adapter(self.agent));
        let mut reader = io::BufReader::new(std::fs::File::open(&self.path)?);
        b.advance_reader(&mut reader)?; // one line resident, byte offsets tracked
        let mut s = b.into_session();
        s.cwd = crate::discover::session_cwd(&self.path); // the builder leaves cwd None; fill it
                                                          // The builder can't resolve child transcript paths (it has no file path); now that we
                                                          // know `path`, fill each `sub_agents[*].transcript` so the map can locate each child.
        populate_sub_agent_transcripts(
            crate::adapter::adapter(self.agent),
            &self.path,
            &mut s.sub_agents,
        );
        Ok(s)
    }

    /// Like [`parse`](Transcript::parse), but also loads the **sub-agent tree** — each
    /// `SubAgent`'s child transcript (recursively) into its `blocks`. Only the nested
    /// `SubAgent.blocks` change — the top-level `blocks`/`index`/`metrics` are identical to
    /// [`parse`](Transcript::parse). The real implementation behind
    /// [`parse_session_enriched_as`](crate::parse_session_enriched_as).
    pub fn parse_enriched(&self) -> io::Result<Session> {
        let mut s = self.parse()?;
        // Enrich fills each `SubAgent`'s child transcript in place — over both the committed and the
        // provisional blocks (a spawn may sit in either).
        let adapter = crate::adapter::adapter(self.agent);
        adapter.enrich(&self.path, &mut s.committed);
        adapter.enrich(&self.path, &mut s.provisional);
        Ok(s)
    }

    /// Open an incremental [`FollowParser`] over this source (folds the whole current file on
    /// the first poll, then only appended bytes). The constructor
    /// [`FollowParser::open`](crate::FollowParser::open) is equivalent; this is the handle-based
    /// entry point.
    pub fn follow(&self) -> FollowParser {
        FollowParser::open(crate::adapter::adapter(self.agent), &self.path)
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

        let s = crate::parse_session_as(Agent::CLAUDE, &path).unwrap();
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
        let t = Transcript::open(Agent::CLAUDE, &path);
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
        let s = crate::parse_session_as(Agent::CLAUDE, &path).unwrap();
        let blocks = s.blocks();
        let locs: Vec<(ByteOffset, usize)> = blocks.iter().filter_map(deferred).collect();
        assert_eq!(locs, vec![(0, 0), (0, 1)], "{blocks:?}");

        let t = Transcript::open(Agent::CLAUDE, &path);
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
        assert_eq!(detected.agent(), Agent::CLAUDE);
        assert_eq!(detected.path(), path.as_path());

        let via_handle = Transcript::open(Agent::CLAUDE, &path).parse().unwrap();
        let via_free = crate::parse_session_as(Agent::CLAUDE, &path).unwrap();
        assert_eq!(format!("{via_handle:?}"), format!("{via_free:?}"));

        let enriched_handle = detected.parse_enriched().unwrap();
        let enriched_free = crate::parse_session_enriched(&path).unwrap();
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
    fn codex_batch_and_enriched_parse_resolve_child_rollout() {
        static N: AtomicUsize = AtomicUsize::new(0);
        let base = std::env::temp_dir().join(format!(
            "cr-codex-tree-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let sessions = base.join("sessions/2026/07/29");
        std::fs::create_dir_all(&sessions).unwrap();
        let parent = sessions.join("rollout-parent.jsonl");
        let child = sessions.join("rollout-child.jsonl");
        std::fs::write(
            &parent,
            concat!(
                r#"{"type":"session_meta","payload":{"id":"parent","cwd":"/repo","source":"cli"}}"#,
                "\n",
                r#"{"type":"response_item","payload":{"type":"function_call","name":"spawn_agent","namespace":"collaboration","call_id":"spawn-1","arguments":"{\"task_name\":\"review\",\"message\":\"review it\"}"}}"#,
                "\n",
                r#"{"type":"event_msg","payload":{"type":"sub_agent_activity","event_id":"spawn-1","agent_thread_id":"child","agent_path":"/root/review","kind":"started"}}"#,
                "\n",
                r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"spawn-1","output":"{\"task_name\":\"/root/review\"}"}}"#,
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
                r#"{"type":"turn_context","payload":{"model":"gpt-5.6"}}"#,
                "\n",
                r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":0,"output_tokens":80}}}}"#,
                "\n",
            ),
        )
        .unwrap();

        let transcript = Transcript::open(Agent::CODEX, &parent);
        let child_id = "child";
        let flat = transcript.parse().unwrap();
        assert_eq!(
            flat.sub_agents
                .get(child_id)
                .and_then(|meta| meta.transcript.as_deref()),
            Some(child.as_path())
        );

        let enriched = transcript.parse_enriched().unwrap();
        let agent = enriched
            .blocks()
            .into_iter()
            .find_map(|block| match block {
                Block::SubAgent(agent) if agent.agent_id == child_id => Some(agent),
                _ => None,
            })
            .expect("resolved Codex child");
        assert!(agent
            .blocks
            .iter()
            .any(|block| matches!(block, Block::UserText(text) if text == "child body")));
        assert!(
            (agent.subtree_cost.expect("child cost") - 0.000925).abs() < 1e-12,
            "{:?}",
            agent.subtree_cost
        );

        std::fs::remove_dir_all(base).unwrap();
    }
}
