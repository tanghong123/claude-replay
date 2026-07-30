//! Layer 2 output — the [`Session`]: a fully-parsed transcript as one value.
//!
//! This is the public, agent-neutral shape a library consumer builds on (design
//! `parser-engine.md` §3.3), without pulling in the TUI / HTML / syntect / clap layers.
//!
//! `parse_session` produces the whole `Session` — blocks + per-turn times + folded metrics
//! (M10, one file read) + the derived `SessionIndex` (M4) — from one streaming pass. Per-turn
//! timestamps still ride `user_times` directly (mirrored onto `index.turns`) until consumers
//! migrate off the field.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use crate::engine::SessionIndex;
use crate::metrics::Metrics;
use crate::model::{AgentId, Block, BlockIndex, EpochSeconds, SubAgentMeta};
use crate::Agent;

/// The per-block storage policy — the seam between the fold and where a block's content lives.
/// **The presentation layer decides what `Bv` is**: the `Block` itself, auxiliary in-memory
/// per-block state the presentation needs, an explicit pointer into a file the presentation
/// maintains (the HTML wire-record log), a deferred `Block` locator (tier-b), or any
/// combination — the `Session` then captures the whole session in the form its consumer uses
/// most efficiently.
/// `put` receives a finalized [`Block`] at its flat [`BlockIndex`] and returns the value the
/// session actually stores (`Self::Bv`). The default [`InMemoryStore`] is identity (holds the
/// `Block` in RAM ⇒ today's behavior); a later on-disk store returns a locator instead. The
/// [`SessionIndex`]/metrics stay `Bv`-free, so only `Session::blocks`' element type varies.
pub trait BlockStore {
    /// The stored block value — `Block` for the in-memory default. `Clone` so the accumulator can
    /// hand out the committed `Vec<Bv>` in a snapshot (identity `Block` clone in RAM; a cheap
    /// `Copy` for an on-disk locator).
    type Bv: Clone;
    /// Store `b` (finalized, at flat index `at`) and return its `Bv` representation. Called **once**
    /// per committed block as the accumulator drains it from the replayer. `user_times` is the
    /// session's per-turn timestamps so far (one per user turn, committed turns final) — a
    /// PROJECTION store rendering its presentation form at put time (#74) indexes into it; the
    /// lossless stores ignore it.
    fn put(
        &mut self,
        b: Block,
        at: BlockIndex,
        user_times: &[Option<crate::model::EpochSeconds>],
    ) -> Self::Bv;
    /// Discard everything stored — called when the accumulator rebuilds from scratch (a source
    /// truncation/rewrite), so an append-only backing doesn't accrete dead content across resets.
    /// Default no-op (the in-memory identity store has nothing of its own to discard).
    fn reset(&mut self) {}
}

/// Read a stored value back to its [`Block`] — the capability split out of [`BlockStore`] (#74):
/// LOSSLESS stores implement it (identity for the in-memory default, a serde decode for tier-b);
/// a one-way projection store (e.g. the HTML wire-record store, whose `Bv` locates rendered JSON)
/// deliberately cannot, and the methods that reconstruct `Block`s bound on this instead.
pub trait BlockRead: BlockStore {
    /// Materialize `bv` back to its [`Block`] — a borrow for RAM, a positional read + decode for
    /// an on-disk backing.
    fn get<'a>(&'a self, bv: &'a Self::Bv) -> std::borrow::Cow<'a, Block>;
}

/// The default, zero-footprint [`BlockStore`]: hold the [`Block`] in RAM. `put`/`get` are identity,
/// so `Session<Block>` keeps exactly today's `Vec<Block>` residency and byte-for-byte output.
#[derive(Default)]
pub struct InMemoryStore;

impl BlockStore for InMemoryStore {
    type Bv = Block;
    fn put(
        &mut self,
        b: Block,
        _at: BlockIndex,
        _user_times: &[Option<crate::model::EpochSeconds>],
    ) -> Block {
        b
    }
}

impl BlockRead for InMemoryStore {
    fn get<'a>(&'a self, bv: &'a Block) -> std::borrow::Cow<'a, Block> {
        std::borrow::Cow::Borrowed(bv)
    }
}

/// The shared-ownership store (#84): committed blocks live behind `Arc` — the accumulator
/// RETAINS the authoritative copy (the cache-owned source of truth every consumer reads),
/// and handing a block to a reader is a refcount bump, never a content copy. Readers get
/// `&Block` through the `Arc` (immutable by construction); "mutation" is building a new
/// block and swapping the slot — though committed blocks never mutate (put-once) and the
/// open turn is replaced per finalize, so that stays theoretical. This is the store behind
/// the principles: ONE full in-memory presentation copy per client instance (it exists to
/// make search fast), owned by the cache, with views keeping only rendered windows and
/// small derived indexes.
#[derive(Default)]
pub struct ArcStore;

impl BlockStore for ArcStore {
    type Bv = std::sync::Arc<Block>;
    fn put(
        &mut self,
        b: Block,
        _at: BlockIndex,
        _user_times: &[Option<crate::model::EpochSeconds>],
    ) -> std::sync::Arc<Block> {
        std::sync::Arc::new(b)
    }
}

impl BlockRead for ArcStore {
    fn get<'a>(&'a self, bv: &'a std::sync::Arc<Block>) -> std::borrow::Cow<'a, Block> {
        std::borrow::Cow::Borrowed(bv)
    }
}

/// Content access gated behind a trait, so most code works on the `Bv`-free [`SessionIndex`] and
/// never names `BV`. Trivial (a borrow) for the in-memory `Session<Block>`; a disk read for a
/// future on-disk store.
pub trait BlockAccess {
    /// The block at flat index `i` — borrowed when it's already resident.
    fn block(&self, i: BlockIndex) -> std::borrow::Cow<'_, Block>;
}

impl BlockAccess for Session<Block> {
    fn block(&self, i: BlockIndex) -> std::borrow::Cow<'_, Block> {
        let c = self.committed.len();
        std::borrow::Cow::Borrowed(if i < c {
            &self.committed[i]
        } else {
            &self.provisional[i - c]
        })
    }
}

/// A fully-parsed session — everything a consumer needs to render or analyze a transcript
/// without touching the presentation layers. Produced by one streaming parse.
///
/// **Opinionated about the durability frontier:** the block stream is split into `committed` (past
/// the frontier — grouped, immutable, `put` through the [`BlockStore`] policy `BV`) and
/// `provisional` (the still-open turn — finalized for display but not yet committed, so never
/// stored; always raw [`Block`]s, O(turn)). The full display stream is `committed ++ provisional`
/// (see [`blocks`](Session::blocks) for `Session<Block>`). Parameterized over `BV` (default `Block`):
/// `committed`'s element type varies (RAM `Block`, or an on-disk locator); `provisional`, the index,
/// metrics, and sub-agent map are `BV`-free.
#[derive(Debug, Clone)]
pub struct Session<BV = Block> {
    /// Which agent produced the transcript.
    pub agent: Agent,
    /// The session working directory, when the transcript recorded it.
    pub cwd: Option<PathBuf>,
    /// Durable blocks past the commit frontier — grouped, immutable, `put` through the store `BV`.
    pub committed: Vec<BV>,
    /// The open turn: finalized for display but **not committed** (may still change), so never
    /// stored — always raw [`Block`]s, O(turn). The display stream is `committed ++ provisional`.
    pub provisional: Vec<Block>,
    /// One timestamp per user turn, in order. Mirrored onto `index.turns[*].time`; kept as
    /// a field until consumers migrate off it.
    pub user_times: Vec<Option<EpochSeconds>>,
    /// Token / cost tally for the session.
    pub metrics: Metrics,
    /// Derived within-session indices — turns / tools / attachments (§7).
    pub index: SessionIndex,
    /// The per-session sub-agent entity map, keyed by [`AgentId`]: the single lookup-owner of
    /// each spawned sub-agent's attributes + pointers to its two lifecycle blocks (`spawn_at` /
    /// `done_at`) and its on-disk artifacts (`transcript` / `output_file`). Built as a post-pass
    /// over the finished blocks; `transcript` is filled by the path-aware parse. Empty for an
    /// agent with no sub-agents (Codex). Replaces the retired `SessionIndex.agents`.
    pub sub_agents: BTreeMap<AgentId, SubAgentMeta>,
    /// The session's task/todo state reconstructed from the transcript's op-log
    /// (`TaskCreate`/`TaskUpdate` calls, #15) — point-in-time as of the fold's end.
    /// Empty for agents with no task tools (Codex). The LIVE on-disk state (richer,
    /// possibly newer) comes from the adapter facade `discover::session_tasks`; merge
    /// with [`tasks::merged`](crate::engine::tasks::merged).
    pub tasks: crate::engine::tasks::TaskList,
}

impl<BV> Session<BV> {
    /// Total block count (`committed + provisional`) — the length of the display stream.
    pub fn block_count(&self) -> usize {
        self.committed.len() + self.provisional.len()
    }
}

impl Session<Block> {
    /// The full display stream `committed ++ provisional` as one owned `Vec<Block>` — the
    /// compatibility view for consumers that want the flat block list (the in-memory `Block` case).
    pub fn blocks(&self) -> Vec<Block> {
        let mut v = Vec::with_capacity(self.block_count());
        v.extend(self.committed.iter().cloned());
        v.extend(self.provisional.iter().cloned());
        v
    }
}

/// Build the sub-agent entity map as a **post-pass over the finished top-level blocks** (the
/// fold is untouched): one [`SubAgentMeta`] per spawn [`Block::SubAgent`] with a non-empty
/// `agent_id`, with `done_at` back-filled from the matching completion [`Block::AgentDone`].
/// `transcript` is left `None` here — the path-aware parse fills it (see
/// `populate_sub_agent_transcripts`). An unmatched `AgentDone` (no spawn) is ignored, mirroring
/// the retired `SessionIndex.agents`.
/// Build the per-session sub-agent index from a block list: one entry per spawn, keyed by
/// agent id, with status derived from the two durable events (spawn → running/async; a later
/// `AgentDone` → terminal). The authoritative status source for renderers (they compute this
/// from their own blocks) — the step off reading a mutated spawn block.
pub fn build_sub_agents<B: std::borrow::Borrow<Block>>(
    blocks: &[B],
) -> BTreeMap<AgentId, SubAgentMeta> {
    let mut map: BTreeMap<AgentId, SubAgentMeta> = BTreeMap::new();
    for (at, b) in blocks.iter().enumerate() {
        push_sub_agent(&mut map, at, b.borrow());
    }
    map
}

/// Fold ONE block (at its flat [`BlockIndex`] `at`) into a sub-agent map — the incremental unit
/// [`build_sub_agents`] loops over. A spawn `SubAgent` (non-empty id) inserts an entry; a later
/// `AgentDone` back-fills `done_at` and supersedes the status with the terminal one (two durable
/// events). Lets the emit-and-drop / tier-b accumulator maintain the map as durable blocks emit,
/// so the full `Vec<Block>` need never be resident. Proven equal to `build_sub_agents`.
pub(crate) fn push_sub_agent(map: &mut BTreeMap<AgentId, SubAgentMeta>, at: BlockIndex, b: &Block) {
    match b {
        Block::SubAgent(sa) if !sa.agent_id.is_empty() => {
            map.insert(
                sa.agent_id.clone(),
                SubAgentMeta {
                    agent_type: sa.agent_type.clone(),
                    status: sa.status,
                    subtree_cost: sa.subtree_cost,
                    transcript: None,
                    output_file: sa.output_file.clone(),
                    spawn_at: at,
                    done_at: None,
                },
            );
        }
        Block::AgentDone {
            agent_id, status, ..
        } => {
            if let Some(m) = map.get_mut(agent_id) {
                m.done_at = Some(at);
                // Two-durable-events status derivation: the finish event supersedes the spawn's
                // launch status with the terminal one. Same value the fold's spawn-block back-patch
                // produced (both from the one completion notification), so byte-identical — but it
                // makes the map the authoritative status source, derived from the events rather than
                // a mutated block (immutable spawn/finish blocks).
                m.status = *status;
            }
        }
        _ => {}
    }
}

/// Live-header facts maintained by the accumulator as the tail advances: turn / tool counts +
/// the spawned children (in launch order, each flagged running). The map-free complement to
/// [`SessionIndex`] / [`build_sub_agents`] — a cheap, always-current header a live consumer
/// reads without rescanning the session. Counts match `count_turns` / `count_tools` over the
/// display stream; `children` matches a block walk (order + duplicates), with `running` derived
/// from the same two lifecycle events the sub-agent map uses.
#[derive(Clone, Default, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SessionMeta {
    /// User turns — `#(UserText | Command)`.
    pub turns: usize,
    /// Tool invocations — `#ToolUse + Σ Thinking.tools.len()` (a spawn's `tool_use` is a child,
    /// **not** a tool). Invariant under activity coalescing.
    pub tools: usize,
    /// Spawned sub-agents in launch (block) order, each flagged `running`.
    pub children: Vec<ChildMeta>,
}

/// One spawned sub-agent in a [`SessionMeta`] — the header/menu view of a child.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ChildMeta {
    pub id: AgentId,
    pub description: String,
    pub agent_type: String,
    /// `!spawn_status.is_terminal()`, cleared when a matching [`Block::AgentDone`] folds.
    pub running: bool,
}

impl SessionMeta {
    /// Fold ONE finalized block into the running meta — the incremental unit the accumulator
    /// applies as each committed block drains and as the open turn is re-folded. Counting
    /// **finalized blocks** (not raw messages) is what makes it exact and byte-identical to the
    /// block-scan presenters: a spawn's `tool_use` is a [`Block::SubAgent`] (a child, scored 0 by
    /// `count_tools`), an activity-grouped run is one `Thinking{tools}` (its nested count), and a
    /// `tool_result`-bearing user message is a back-patch, not a turn. Proven equal to
    /// `count_turns` / `count_tools` / `collect_child_refs` over the same blocks (see the tests).
    pub fn push(&mut self, b: &Block) {
        match b {
            Block::UserText(_) | Block::Command { .. } => self.turns += 1,
            Block::ToolUse { .. } => self.tools += 1,
            Block::Thinking { tools, .. } => self.tools += tools.len(),
            Block::SubAgent(sa) if !sa.agent_id.is_empty() => self.children.push(ChildMeta {
                id: sa.agent_id.clone(),
                description: sa.description.clone(),
                agent_type: sa.agent_type.clone(),
                running: !sa.status.is_terminal(),
            }),
            Block::AgentDone { agent_id, .. } if !agent_id.is_empty() => {
                // Linear scan (children are few) so a duplicate id keeps today's block-walk
                // behavior; a map lookup would collapse duplicates.
                for c in self.children.iter_mut().filter(|c| &c.id == agent_id) {
                    c.running = false;
                }
            }
            _ => {}
        }
    }

    /// Build over a whole block list — one [`push`](Self::push) per block. The batch/oracle path
    /// (the accumulator maintains it incrementally instead; this proves the two equal).
    pub fn build(blocks: &[Block]) -> Self {
        let mut m = SessionMeta::default();
        for b in blocks {
            m.push(b);
        }
        m
    }
}

/// Fill each entry's `transcript` with its child transcript file (`subagents/agent-<id>.jsonl`)
/// via the agent's discovery hook. A no-op for an agent without sub-agents, or where the file is
/// absent (leaves `None`). Called by the path-aware parse, which alone knows the transcript path.
pub(crate) fn populate_sub_agent_transcripts(
    agent: Agent,
    path: &Path,
    map: &mut BTreeMap<AgentId, SubAgentMeta>,
) {
    for (id, m) in map.iter_mut() {
        m.transcript = crate::discover::subagent_source(agent, path, id);
    }
}

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
    crate::Transcript::detect(path).parse()
}

/// Like [`parse_session`], but also loads the **sub-agent tree** — each `SubAgent`'s child
/// transcript (recursively) into its `blocks`, so a consumer can descend into spawned agents
/// or roll up subtree cost. `parse_session` leaves `SubAgent.blocks` empty (cheaper, flat);
/// use this when you need the whole tree. Only the nested `SubAgent.blocks` change — the
/// top-level `blocks`/`index`/`metrics` are identical to `parse_session`.
pub fn parse_session_enriched(path: &Path) -> io::Result<Session> {
    crate::Transcript::detect(path).parse_enriched()
}

/// [`parse_session_enriched`] for a **known** agent (skips detection).
pub fn parse_session_enriched_as(agent: Agent, path: &Path) -> io::Result<Session> {
    crate::Transcript::open(agent, path).parse_enriched()
}

/// Parse for a **known** agent, skipping detection — for a caller that already sniffed.
///
/// A thin wrapper over [`Transcript::parse`](crate::Transcript::parse), which holds the real
/// streaming-fold logic. Kept as a documented, widely-called free-function entry point.
pub fn parse_session_as(agent: Agent, path: &Path) -> io::Result<Session> {
    crate::Transcript::open(agent, path).parse()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn tmp(name: &str, body: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "cr-session-{}-{}-{name}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::File::create(&p)
            .unwrap()
            .write_all(body.as_bytes())
            .unwrap();
        p
    }

    /// `parse_session` must return exactly what the existing entry points return — same
    /// blocks, same per-turn times, same metrics, same cwd — just bundled. This is the
    /// byte-identical gate for the wrapper.
    #[test]
    fn parse_session_matches_the_existing_entry_points() {
        let body = concat!(
            r#"{"type":"user","cwd":"/repo","message":{"role":"user","content":[{"type":"text","text":"hi"}]},"timestamp":"2026-07-26T10:00:00Z"}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hello"}],"usage":{"input_tokens":3,"output_tokens":5}},"timestamp":"2026-07-26T10:00:01Z"}"#,
            "\n",
        );
        let path = tmp("claude.jsonl", body);

        let s = parse_session(&path).unwrap();
        assert_eq!(s.agent, Agent::Claude);

        let (blocks, times, folded_metrics) =
            crate::engine::replay::parse_path_timed_for(Agent::Claude, &path).unwrap();
        // The retired separate metrics pass, as the byte-identical reference for the fold.
        let ref_metrics = crate::metrics::parse_reader_for(
            Agent::Claude,
            io::BufReader::new(std::fs::File::open(&path).unwrap()),
        );
        // `Block` isn't `PartialEq` (like the other equivalence tests, compare via Debug).
        assert_eq!(
            format!("{:?}", s.blocks()),
            format!("{:?}", blocks),
            "blocks match the existing parse"
        );
        assert_eq!(s.user_times, times, "user_times match");
        assert_eq!(
            folded_metrics, ref_metrics,
            "folded metrics (M10) == the separate parse_reader_for pass"
        );
        assert_eq!(
            s.metrics, ref_metrics,
            "session metrics == the reference pass"
        );
        assert_eq!(s.cwd, crate::discover::session_cwd(&path), "cwd matches");
        assert_eq!(s.cwd.as_deref(), Some(Path::new("/repo")));

        let _ = std::fs::remove_file(&path);
    }

    /// `build_sub_agents` keys the map by agent id, its `spawn_at`/`done_at` point at the
    /// `SubAgent` spawn and the `AgentDone` completion, and a spawn without a completion has
    /// `done_at == None`. An empty-id spawn and an unmatched `AgentDone` produce no entry.
    #[test]
    fn sub_agents_map_points_at_lifecycle_blocks() {
        use crate::model::{AgentStatus, SubAgent};
        let spawn = |id: &str, status| {
            Block::SubAgent(SubAgent {
                agent_id: id.into(),
                tool_use_id: format!("t_{id}"),
                agent_type: "gp".into(),
                description: format!("do {id}"),
                prompt: "go".into(),
                status,
                result: None,
                output_file: None,
                blocks: Vec::new(),
                subtree_cost: Some(2.5),
            })
        };
        let done = |id: &str| Block::AgentDone {
            agent_id: id.into(),
            agent_type: "gp".into(),
            description: format!("do {id}"),
            status: AgentStatus::Completed,
            result: None,
        };
        let blocks = vec![
            Block::UserText("hi".into()),        // 0
            spawn("a1", AgentStatus::Completed), // 1
            spawn("a2", AgentStatus::Running),   // 2 — never completes
            spawn("", AgentStatus::Running),     // 3 — empty id: skipped
            done("a1"),                          // 4 — completes a1
            done("ghost"),                       // 5 — no spawn: ignored
        ];
        let map = build_sub_agents(&blocks);

        assert_eq!(map.len(), 2, "a1 + a2 only (empty id + ghost dropped)");

        let a1 = &map["a1"];
        assert_eq!(a1.spawn_at, 1);
        assert!(matches!(blocks[a1.spawn_at], Block::SubAgent(_)));
        assert_eq!(a1.done_at, Some(4));
        assert!(matches!(
            blocks[a1.done_at.unwrap()],
            Block::AgentDone { .. }
        ));
        assert_eq!(a1.agent_type, "gp");
        assert_eq!(a1.subtree_cost, Some(2.5));

        let a2 = &map["a2"];
        assert_eq!(a2.spawn_at, 2);
        assert_eq!(a2.done_at, None, "a2 never completed");
        assert_eq!(a2.status, AgentStatus::Running);
        assert!(
            a2.transcript.is_none(),
            "no path-aware parse → no transcript"
        );
    }

    // The incremental `push_sub_agent` fold must reproduce the batch `build_sub_agents` exactly —
    // the property the emit-and-drop / tier-b accumulator relies on to maintain the sub-agent map
    // without the full blocks resident.
    #[test]
    fn incremental_push_sub_agent_equals_batch_build() {
        use crate::model::{AgentStatus, SubAgent};
        let spawn = |id: &str, status| {
            Block::SubAgent(SubAgent {
                agent_id: id.into(),
                tool_use_id: format!("t_{id}"),
                agent_type: "gp".into(),
                description: format!("do {id}"),
                prompt: "go".into(),
                status,
                result: None,
                output_file: Some(format!("/t/{id}.out")),
                blocks: Vec::new(),
                subtree_cost: Some(2.5),
            })
        };
        let done = |id: &str, status| Block::AgentDone {
            agent_id: id.into(),
            agent_type: "gp".into(),
            description: format!("do {id}"),
            status,
            result: Some("r".into()),
        };
        let blocks = vec![
            Block::UserText("hi".into()),
            spawn("a1", AgentStatus::AsyncLaunched),
            spawn("a2", AgentStatus::Running),
            spawn("", AgentStatus::Running),
            done("a1", AgentStatus::Completed),
            done("ghost", AgentStatus::Failed),
            spawn("a3", AgentStatus::Running),
            done("a3", AgentStatus::Killed),
        ];

        let batch = build_sub_agents(&blocks);

        let mut incr: BTreeMap<AgentId, SubAgentMeta> = BTreeMap::new();
        for (at, b) in blocks.iter().enumerate() {
            push_sub_agent(&mut incr, at, b);
        }

        assert_eq!(batch, incr, "incremental push must equal batch build");
    }

    /// `SessionMeta::build` counts exactly as the presenters' `count_turns` / `count_tools` /
    /// `collect_child_refs` do: a tool absorbed into a `Thinking` still counts, a spawn's
    /// `tool_use` is a child (not a tool), children keep block order, and `running` clears on a
    /// terminal spawn status or a matching `AgentDone`.
    #[test]
    fn session_meta_counts_turns_tools_children() {
        use crate::model::{AgentStatus, SubAgent};
        let spawn = |id: &str, status| {
            Block::SubAgent(SubAgent {
                agent_id: id.into(),
                tool_use_id: format!("t_{id}"),
                agent_type: "gp".into(),
                description: format!("do {id}"),
                prompt: "go".into(),
                status,
                result: None,
                output_file: None,
                blocks: Vec::new(),
                subtree_cost: None,
            })
        };
        let tool = || Block::ToolUse {
            name: "Bash".into(),
            target: "ls".into(),
            diffs: vec![],
            output: None,
            patch: None,
            read_lines: None,
        };
        let blocks = vec![
            Block::UserText("hi".into()), // turn 1
            tool(),                       // tool 1
            Block::Thinking {
                text: String::new(),
                duration_secs: None,
                tools: vec![tool(), tool()],
            }, // +2 nested tools ⇒ 3
            spawn("a1", AgentStatus::Running), // child a1, running
            spawn("a2", AgentStatus::Completed), // child a2, terminal spawn ⇒ not running
            spawn("", AgentStatus::Running), // empty id ⇒ skipped
            Block::AgentDone {
                agent_id: "a1".into(),
                agent_type: "gp".into(),
                description: "do a1".into(),
                status: AgentStatus::Completed,
                result: None,
            }, // completes a1 ⇒ not running
            Block::Command {
                name: "/clear".into(),
                args: String::new(),
                output: vec![],
            }, // turn 2
        ];
        let m = SessionMeta::build(&blocks);
        assert_eq!(m.turns, 2);
        assert_eq!(m.tools, 3, "1 ToolUse + 2 Thinking-absorbed");
        assert_eq!(m.children.len(), 2, "a1 + a2 (empty id skipped)");
        assert_eq!(m.children[0].id, "a1");
        assert!(!m.children[0].running, "a1 completed via AgentDone");
        assert_eq!(m.children[1].id, "a2");
        assert!(!m.children[1].running, "a2 spawned terminal");

        // Incremental push, block by block, equals the batch build.
        let mut incr = SessionMeta::default();
        for b in &blocks {
            incr.push(b);
        }
        assert_eq!(incr, m, "incremental push == batch build");
    }

    /// An enriched, path-aware parse fills `sub_agents[*].transcript` with the child's on-disk
    /// transcript (`<session>/subagents/agent-<id>.jsonl`).
    #[test]
    fn enriched_parse_populates_transcript() {
        let base = std::env::temp_dir().join(format!(
            "cr-session-tx-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let sess = base.join("proj").join("sid.jsonl");
        let sadir = base.join("proj").join("sid").join("subagents");
        std::fs::create_dir_all(&sadir).unwrap();
        let parent = concat!(
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_A","name":"Agent","input":{"subagent_type":"general-purpose","description":"child","prompt":"go"}}]}}"#,
            "\n",
            r#"{"type":"user","toolUseResult":{"agentId":"achild01","status":"completed"},"message":{"content":[{"type":"tool_result","tool_use_id":"toolu_A","content":"done"}]}}"#,
            "\n",
        );
        std::fs::File::create(&sess)
            .unwrap()
            .write_all(parent.as_bytes())
            .unwrap();
        let child = concat!(
            r#"{"type":"user","message":{"content":"go"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"ok"}]}}"#,
            "\n",
        );
        std::fs::File::create(sadir.join("agent-achild01.jsonl"))
            .unwrap()
            .write_all(child.as_bytes())
            .unwrap();

        let s = parse_session_enriched_as(Agent::Claude, &sess).unwrap();
        let meta = s
            .sub_agents
            .get("achild01")
            .expect("achild01 in sub_agents map");
        assert_eq!(
            meta.transcript.as_deref(),
            Some(sadir.join("agent-achild01.jsonl").as_path()),
            "child transcript path resolved"
        );
        assert!(matches!(s.blocks()[meta.spawn_at], Block::SubAgent(_)));

        let _ = std::fs::remove_dir_all(&base);
    }
}
