//! **On-demand attachment loading** — the [`Transcript`] source object.
//!
//! A resident [`Session`](crate::Session) holds only a locator per attachment
//! ([`AttachmentContent::Deferred`](crate::model::AttachmentContent::Deferred)) — never the
//! embedded bytes. A [`Transcript`] is the cheap handle (agent + path) a presenter keeps to
//! turn a locator into content on demand: [`Transcript::load_attachment`] seeks to the
//! recorded byte offset, reads that ONE line, and re-runs the same extraction the parser uses
//! to yield a [`LoadedAttachment`]. It is **stateless** — it caches nothing, reads on demand
//! each call, and returns owned bytes the caller drops after use, so at most one attachment is
//! resident at a time. Caching, if ever wanted, is a presentation-layer concern.

use crate::model::{ByteOffset, LoadedAttachment};
use crate::Agent;
use std::io::{self, BufRead, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// A cheap, clonable handle to a transcript file — the "source" a presenter holds to resolve
/// [`Deferred`](crate::model::AttachmentContent::Deferred) attachment locators on demand.
/// Holds only the agent + path; loads nothing until asked.
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

    /// The agent this transcript is decoded as.
    pub fn agent(&self) -> Agent {
        self.agent
    }

    /// The transcript path.
    pub fn path(&self) -> &Path {
        &self.path
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

        let s = crate::engine::parse_session_as(Agent::Claude, &path).unwrap();
        let atts: Vec<&Block> = s
            .blocks
            .iter()
            .filter(|b| matches!(b, Block::Attachment(_)))
            .collect();
        assert_eq!(atts.len(), 4, "{:?}", s.blocks);

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
        let locs: Vec<(ByteOffset, usize)> = s.blocks.iter().filter_map(deferred).collect();
        assert_eq!(locs, vec![(0, 0), (0, 1)], "{:?}", s.blocks);

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
}
