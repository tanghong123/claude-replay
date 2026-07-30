//! The `parse_session*` dispatchers — the facade's whole-file entry points (#87
//! step 3): detect (or accept) the agent, resolve its adapter from the wired
//! registry, and drive the engine's streaming fold via [`Transcript`].

use crate::{Agent, Session, Transcript};
use std::io;
use std::path::Path;

/// **The entry point.** Auto-detect the agent from the transcript head, then parse the file
/// into a [`Session`] (blocks + index + metrics + cwd). Streaming — one line resident, so a
/// multi-gigabyte transcript never balloons into memory. Sub-agent child transcripts are NOT
/// loaded (`SubAgent.blocks` stays empty); this is the flat top-level session.
///
/// ```no_run
/// let session = claude_replay_core::parse_session(std::path::Path::new("session.jsonl"))?;
/// println!("{} blocks, {} turns", session.block_count(), session.index.turns.len());
/// for block in session.blocks() {
///     // render / analyze `block` — see `claude_replay_core::Block`
/// }
/// # Ok::<(), std::io::Error>(())
/// ```
///
/// For a live tail (fold only appended bytes each poll), use [`FollowParser`](crate::FollowParser).
pub fn parse_session(path: &Path) -> io::Result<Session> {
    Transcript::detect(path).parse()
}

/// Like [`parse_session`], but also loads the **sub-agent tree** — each `SubAgent`'s child
/// transcript (recursively) into its `blocks`, so a consumer can descend into spawned agents
/// or roll up subtree cost. `parse_session` leaves `SubAgent.blocks` empty (cheaper, flat);
/// use this when you need the whole tree. Only the nested `SubAgent.blocks` change — the
/// top-level `blocks`/`index`/`metrics` are identical to `parse_session`.
pub fn parse_session_enriched(path: &Path) -> io::Result<Session> {
    Transcript::detect(path).parse_enriched()
}

/// [`parse_session_enriched`] for a **known** agent (skips detection).
pub fn parse_session_enriched_as(agent: Agent, path: &Path) -> io::Result<Session> {
    Transcript::open(agent, path).parse_enriched()
}

/// Parse for a **known** agent, skipping detection — for a caller that already sniffed.
///
/// A thin wrapper over [`Transcript::parse`](crate::Transcript::parse), which holds the real
/// streaming-fold logic. Kept as a documented, widely-called free-function entry point.
pub fn parse_session_as(agent: Agent, path: &Path) -> io::Result<Session> {
    Transcript::open(agent, path).parse()
}
