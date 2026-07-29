//! **Layer 2 — the shared fold.** The stateful [`Replayer`] folds a stream of canonical
//! [`Message`]s (produced by any agent's Layer-1 decoder)
//! into render [`Block`]s, via the per-agent [`Shaping`] seam. It is driven incrementally by
//! [`SessionAccumulator`](crate::engine::builder::SessionAccumulator) (batch and live alike).
//! Agent-agnostic: everything that differs by agent enters
//! through `Shaping` (a `&'static` const per adapter) plus the `decode` closure. The data
//! model it produces and block classification live in [`crate::model`]; this module is the
//! machinery that builds it. Byte-identical to the retired `parse_main`/`parse_lines` oracles
//! (see the equivalence gates in `claude_model`/`codex_model`).

use crate::engine::message::{Message, QueueOpKind};
use crate::model::*;
#[cfg(test)]
use crate::Agent;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// Streaming whole-file parse for `agent`: blocks + one wall-clock timestamp per user turn +
/// folded metrics, all in a single pass (M9/M10). Derived from the incremental fold — feeds a
/// [`SessionAccumulator`](crate::engine::builder::SessionAccumulator) line-by-line (one line resident,
/// no whole-file `Vec`), the same driver the live [`FollowParser`](crate::FollowParser) uses.
/// Flat (sub-agent trees are loaded separately by
/// [`enrich`](crate::adapter::TranscriptAdapter::enrich)).
///
/// Test-only now: production goes straight through `SessionAccumulator`
/// ([`parse_session_as`](crate::parse_session_as) drives it directly). This wrapper is retained
/// as the whole-file reference the equivalence gates compare the builder-driven output against.
#[cfg(test)]
pub(crate) fn parse_path_timed_for(
    agent: Agent,
    path: &std::path::Path,
) -> std::io::Result<(
    Vec<Block>,
    Vec<Option<EpochSeconds>>,
    crate::metrics::Metrics,
)> {
    let mut b = crate::engine::builder::SessionAccumulator::new(agent);
    let mut reader = std::io::BufReader::new(std::fs::File::open(path)?);
    b.advance_reader(&mut reader)?; // one line resident
    Ok(b.fold())
}

/// The agent-specific seam of the otherwise-shared L2 fold — one of the
/// [`TranscriptAdapter`](crate::adapter) hooks, returned by its `shaping()`. Each agent
/// supplies a `&'static` const (`CLAUDE_SHAPING` / `CODEX_SHAPING`); the `Replayer` fold takes
/// it by reference on every parse (batch and live). Everything else
/// in this module is agent-agnostic; these **four** fn-pointer hooks (each documented on its
/// field below) are the only points Claude and Codex differ: `build_tool` (shape a `tool_use`
/// into a block), `join_result` (attach its result), `keep_orphan` (keep a resultless
/// result?), and `finish_turns` (final turn shaping).
pub(crate) struct Shaping {
    /// Build the block for a `tool_use` from its raw fields (`id`, `name`, `input`, `cwd`).
    /// This is the block-model lift's L2 hook (M14): the tokenizer emits raw
    /// `Message::ToolUse` fields and the fold shapes the block here, so agent-specific
    /// block construction (Claude's `Agent`/`Task`→`SubAgent`, Codex's name normalization)
    /// lives in Layer 2, not the tokenizer.
    pub build_tool: fn(&str, &str, &Value, &str) -> Block,
    /// Join a tool result onto its `ToolUse` block (Claude reads `toolUseResult` for
    /// diffs/read-count; Codex just sets the output text). Named to avoid colliding with
    /// [`Replayer::apply`], which folds a whole message batch.
    pub join_result: fn(&mut Block, &str, &Value),
    /// Keep a resultless orphan result (already non-empty)? Claude drops boilerplate; Codex
    /// keeps every non-empty output.
    pub keep_orphan: fn(&str) -> bool,
    /// Final turn shaping — Claude groups thinking + coalesces activity runs; Codex is identity.
    pub finish_turns: fn(Vec<Block>) -> Vec<Block>,
}

/// **Layer 2 — the stateful replayer** (design §3.3). `apply` folds a batch of messages
/// into the running block buffer (the `id → block index` back-patch, the thinking clock,
/// the queue lifecycle, user-turn stamping); `into_blocks` finalizes (the final user-turn
/// flush, completions, then the agent-specific `finish`). Fed all messages at once it
/// reproduces the old one-shot `replay` exactly; fed in pieces it folds **incrementally** —
/// the keystone for the streaming production path (M9) and the live `ingest` (M11).
/// Agent-agnostic: it folds the shared [`Message`]
/// vocabulary and parses **no** raw agent formats — each agent's L1 decoder maps its own
/// transcript shapes onto these structured messages (completions, commands, skill bodies,
/// injected notes, the queue lifecycle), so the fold is the same code for every agent. The
/// one agent-specific seam is `shaping` (tool-block build, result back-patch, orphan policy,
/// turn `finish`). Variants an agent doesn't produce (e.g. Codex emits no `QueueOp`/
/// `Completion`/`SkillBody`) simply never reach their arms.
///
/// Single pass: a `tool_result` whose `tool_use` hasn't been seen yet is emitted inline as an
/// orphan (forward-references — a result physically before its own tool_use — do not occur in
/// real transcripts: 0/209 scanned).
pub(crate) struct Replayer<'a> {
    shaping: &'a Shaping,
    /// The **resident window** of raw (pre-`finish_turns`) blocks: logical indices `base..`. Blocks
    /// before `base` are complete turns that have been finalized (grouped) into `durable` and dropped
    /// from `out` — so content is O(turn), not O(N). Every stored index below (`tool_slot`,
    /// `suppress`, `last_skill`, `stamped`, `queue[].marker_idx`) is **logical**; subtract `base` to
    /// index `out`.
    out: Vec<Block>,
    /// Finalized (grouped) blocks of the completed turns, in order — the durable prefix. Appended to
    /// as turns close; concatenated ahead of `finish_turns(out)` to reproduce the whole session
    /// (per-turn finalize distributes over user-turn boundaries, so this equals a global finalize).
    durable: Vec<Block>,
    /// Count of raw blocks already finalized-and-dropped from the front of `out` — the logical index
    /// of `out[0]`. Always sits **at a turn boundary** (a `UserText`/`Command`), so the durable /
    /// open split never cuts a turn.
    base: usize,
    user_times: Vec<Option<EpochSeconds>>,
    pending_ts: Option<EpochSeconds>,
    stamped: usize,
    tool_slot: HashMap<String, BlockIndex>,
    trigger_ts: Option<EpochSeconds>,
    queue: Vec<QueueItem>,
    content_seq: usize,
    suppress: Vec<BlockIndex>,
    last_skill: Option<BlockIndex>,
    // Running spawn identity map (tool_use_id **and** agent_id → (agent_id, agent_type)), fed as
    // each `SubAgent` spawn is emitted and refreshed when its result fills `agent_id`. It lets an
    // `AgentDone` resolve its spawn's id/type at emit-time from O(#agents) state instead of a
    // finalize-time scan over all blocks — so resolution survives emit-and-drop of the spawn.
    agent_ids: HashMap<String, (String, String)>,
}

/// Upsert a `SubAgent` spawn's identity into the running resolution map (keyed by both its
/// `tool_use_id` and, once known, its `agent_id`). No-op for non-`SubAgent` blocks.
fn record_agent(map: &mut HashMap<String, (String, String)>, b: &Block) {
    if let Block::SubAgent(sa) = b {
        let v = (sa.agent_id.clone(), sa.agent_type.clone());
        if !sa.tool_use_id.is_empty() {
            map.insert(sa.tool_use_id.clone(), v.clone());
        }
        if !sa.agent_id.is_empty() {
            map.insert(sa.agent_id.clone(), v);
        }
    }
}

/// Record an in-place patch of the block at raw-logical index `logical`: fold it into `floor` (the
/// running minimum) **iff** it sits below `frontier` — the count of blocks already emitted *before*
/// this fold batch. A patch at/above `frontier` touched a block appended within this same batch,
/// which no client has seen yet (it arrives as a plain append), so it is not a back-patch and is not
/// recorded. `floor` is thus the lowest already-emitted index this batch mutated: `None` ⇒ append-
/// only. See [`Replayer::apply`] for how the streaming layer turns it into a provisional-generation
/// bump (and, later, the `provisional_gen_prefix`).
fn note_patch(floor: &mut Option<usize>, logical: usize, frontier: usize) {
    if logical < frontier {
        *floor = Some(floor.map_or(logical, |p| p.min(logical)));
    }
}

impl<'a> Replayer<'a> {
    pub(crate) fn new(shaping: &'a Shaping) -> Self {
        Replayer {
            shaping,
            out: Vec::new(),
            durable: Vec::new(),
            base: 0,
            user_times: Vec::new(),
            pending_ts: None,
            stamped: 0,
            tool_slot: HashMap::new(),
            trigger_ts: None,
            queue: Vec::new(),
            content_seq: 0,
            suppress: Vec::new(),
            last_skill: None,
            agent_ids: HashMap::new(),
        }
    }

    /// Fold a batch of messages into the running state (append, back-patch, stamp).
    ///
    /// Returns the **min raw-logical index of any already-emitted block this batch back-patched**
    /// (`None` if it only appended) — the streaming layer's provisional-generation signal (§9a).
    /// `emitted_frontier` is captured at entry (blocks emitted *before* this batch); a mutation
    /// below it changed a block a prior poll may have served ⇒ a gen bump, whereas a mutation to a
    /// block appended *within* this batch is invisible to clients and doesn't count. The fold's
    /// effect on `out` is unchanged — this is a pure observation (keeps `--dump` byte-identical).
    pub(crate) fn apply(&mut self, messages: &[Message]) -> Option<usize> {
        let (join_result, keep_orphan) = (self.shaping.join_result, self.shaping.keep_orphan);
        let emitted_frontier = self.base + self.out.len();
        let mut patch_floor: Option<usize> = None;
        for m in messages {
            match m {
                Message::LineStart(ts) => {
                    // Stamp over the resident window; `stamped` is logical, so translate by `base`.
                    let mut ws = self.window_stamped();
                    stamp_user_turns(&self.out, &mut ws, self.pending_ts, &mut self.user_times);
                    self.stamped = self.base + ws;
                    self.pending_ts = *ts;
                }
                Message::Trigger(ts) => {
                    if let Some(t) = ts {
                        self.trigger_ts = Some(*t);
                    }
                }
                Message::AssistantText(t) => {
                    self.out.push(Block::AssistantText(t.clone()));
                    self.content_seq += 1;
                }
                Message::Thinking { text, ts } => {
                    let duration_secs = match (ts, self.trigger_ts) {
                        (Some(end), Some(start)) if *end >= start => Some((end - start) as u64),
                        _ => None,
                    };
                    self.out.push(Block::Thinking {
                        text: text.clone(),
                        duration_secs,
                        tools: Vec::new(),
                    });
                    self.content_seq += 1;
                }
                Message::ToolUse {
                    id,
                    name,
                    input,
                    cwd,
                } => {
                    self.out
                        .push((self.shaping.build_tool)(id, name, input, cwd));
                    self.content_seq += 1;
                    let rel = self.out.len() - 1;
                    let logical = self.base + rel;
                    if let Block::ToolUse { name, .. } = &self.out[rel] {
                        if name == "Skill" {
                            self.last_skill = Some(logical);
                        }
                    }
                    record_agent(&mut self.agent_ids, &self.out[rel]);
                    if !id.is_empty() {
                        self.tool_slot.insert(id.clone(), logical);
                    }
                }
                Message::ToolResult {
                    tool_use_id,
                    text,
                    tur,
                } => {
                    if let Some(rel) = self.tool_slot.get(tool_use_id).map(|&i| i - self.base) {
                        join_result(&mut self.out[rel], text, tur);
                        note_patch(&mut patch_floor, self.base + rel, emitted_frontier);
                        // The result may have filled the spawn's `agent_id`; refresh the map so a
                        // later `AgentDone` resolves the real id (not just the `tool_use_id`).
                        record_agent(&mut self.agent_ids, &self.out[rel]);
                    } else if !text.trim().is_empty() && keep_orphan(text) {
                        self.out.push(Block::ToolResult(text.clone()));
                    }
                }
                Message::UserText { text } => {
                    self.out.push(Block::UserText(text.clone()));
                }
                Message::SystemNote { text } => {
                    self.out.push(Block::ToolResult(text.clone()));
                }
                Message::SkillBody { text, fallback } => {
                    // L1 detected the skill body; the fold only nests it into the most recent
                    // `Skill` block (stateful), falling back to a loose result block.
                    let target = self.last_skill.map(|i| i - self.base);
                    if attach_skill_body(&mut self.out, target, text) {
                        // Nested into the existing `Skill` block — a back-patch of that block.
                        if let Some(ls) = self.last_skill {
                            note_patch(&mut patch_floor, ls, emitted_frontier);
                        }
                    } else if !fallback.is_empty() {
                        self.out.push(Block::ToolResult(fallback.clone()));
                    }
                }
                Message::Command { name, args, output } => {
                    self.out.push(Block::Command {
                        name: name.clone(),
                        args: args.clone(),
                        output: output.clone(),
                    });
                }
                Message::CommandStdout { text } => {
                    // Attach to the command it follows, else show it command-less.
                    let last_logical = (self.base + self.out.len()).checked_sub(1);
                    if let Some(Block::Command { output, .. }) = self.out.last_mut() {
                        output.push(text.clone());
                        if let Some(logical) = last_logical {
                            note_patch(&mut patch_floor, logical, emitted_frontier);
                        }
                    } else {
                        self.out.push(Block::Command {
                            name: String::new(),
                            args: String::new(),
                            output: vec![text.clone()],
                        });
                    }
                }
                Message::AttachmentPrompt { text } => {
                    self.out.push(Block::UserText(text.clone()));
                }
                Message::Attachment(att) => {
                    self.out.push(Block::Attachment(att.clone()));
                }
                Message::Completion {
                    tool_use_id,
                    task_id,
                    status,
                    description,
                    result,
                } => {
                    // L1 already parsed the notification; the fold only places the `AgentDone`
                    // block. No status back-patch onto the spawn — the terminal status is
                    // derived by the `sub_agents` index from this finish event (two durable
                    // events), so the spawn/finish blocks stay immutable. The spawn's real
                    // id/type are resolved here from the running `agent_ids` map (populated as the
                    // spawn was emitted), so no finalize-time scan over the — possibly dropped —
                    // spawn block is needed.
                    let key = if !task_id.is_empty() {
                        task_id.clone()
                    } else {
                        tool_use_id.clone()
                    };
                    let (agent_id, agent_type) = match self.agent_ids.get(&key) {
                        Some((real_id, ty)) => (real_id.clone(), ty.clone()),
                        None => (key, String::new()),
                    };
                    self.out.push(Block::AgentDone {
                        agent_id,
                        agent_type,
                        description: description.clone(),
                        status: status.unwrap_or(AgentStatus::Completed),
                        result: result.clone(),
                    });
                }
                Message::QueueOp { op, content, prose } => match op {
                    QueueOpKind::Enqueue => {
                        if let Some(c) = content {
                            let marker_idx = if *prose {
                                self.out.push(Block::QueueEvent {
                                    text: c.trim().to_string(),
                                });
                                Some(self.base + self.out.len() - 1)
                            } else {
                                None
                            };
                            self.queue.push(QueueItem {
                                content: c.trim().to_string(),
                                marker_idx,
                                content_at_enqueue: self.content_seq,
                            });
                        }
                    }
                    QueueOpKind::Remove | QueueOpKind::Dequeue => {
                        let popped = match content.as_deref().map(str::trim) {
                            Some(c) => self
                                .queue
                                .iter()
                                .position(|q| q.content == c)
                                .map(|i| self.queue.remove(i)),
                            None if !self.queue.is_empty() => Some(self.queue.remove(0)),
                            None => None,
                        };
                        if let Some(item) = popped {
                            if let Some(mi) = item.marker_idx {
                                if self.content_seq == item.content_at_enqueue {
                                    self.suppress.push(mi);
                                }
                            }
                        }
                    }
                },
            }
        }
        self.finalize_completed();
        patch_floor
    }

    /// Drop the completed turns behind the open window: finalize (group + suppress) every turn
    /// before the **last** user-turn boundary and move the result into `durable`, then discard those
    /// raw blocks from `out` and advance `base`. Runs only when the prompt queue is **empty** — while
    /// any prompt is pending, its `⧗ queued:` marker might still be suppressed at dequeue, so we keep
    /// its turn resident (the queue-marker-pinned frontier). Content stays O(turn); the durable prefix
    /// grows. A no-op until at least one full turn has closed.
    fn finalize_completed(&mut self) {
        if !self.queue.is_empty() {
            return;
        }
        // The open turn starts at the last user-turn boundary in the window; everything before it is
        // complete. Nothing to drop if there's no boundary, or the only turn is the open one.
        let Some(mut k) = self
            .out
            .iter()
            .rposition(|b| matches!(b, Block::UserText(_) | Block::Command { .. }))
        else {
            return;
        };
        // Pin the turn holding `last_skill`: a `SkillBody` can still nest into a `Skill` block from
        // an earlier turn (the back-reference persists until the next skill), so that block must stay
        // resident. Cap the drop at the turn boundary at/before it. (The queue-marker back-reference
        // is already pinned by the `queue.is_empty()` guard above.)
        if let Some(ls_rel) = self.last_skill.map(|i| i - self.base) {
            if ls_rel < k {
                k = self.out[..=ls_rel]
                    .iter()
                    .rposition(|b| matches!(b, Block::UserText(_) | Block::Command { .. }))
                    .unwrap_or(0);
            }
        }
        if k == 0 {
            return;
        }
        // Stamp any not-yet-stamped user turns BEFORE the frontier advances past them. Normally a
        // turn is stamped by its own line's `LineStart` long before it commits (the commit needs
        // the NEXT user line, whose LineStart stamps first) — but ONE line can carry several user
        // text items, so the second turn closes the first *within the same batch*, ahead of any
        // later LineStart. Without this, that turn would drain unstamped: its timestamp lost and
        // `stamped` left BEHIND `base`, wrapping the `stamped - base` window math (the QoderWork
        // panic, #56). Uses `pending_ts` — exactly the value the next LineStart would stamp with.
        let mut ws = self.window_stamped();
        stamp_user_turns(&self.out, &mut ws, self.pending_ts, &mut self.user_times);
        self.stamped = self.base + ws;
        // Drain the completed raw blocks [0..k) from the window front.
        let drained: Vec<Block> = self.out.drain(0..k).collect();
        // Apply the suppression flagged for this range (logical indices in [base, base+k)), then
        // finalize (group) and append to the durable prefix.
        let lo = self.base;
        let hi = self.base + k;
        let drop: HashSet<usize> = self
            .suppress
            .iter()
            .copied()
            .filter(|&i| (lo..hi).contains(&i))
            .collect();
        let raw: Vec<Block> = drained
            .into_iter()
            .enumerate()
            .filter(|(off, _)| !drop.contains(&(lo + off)))
            .map(|(_, b)| b)
            .collect();
        let finalized = (self.shaping.finish_turns)(raw);
        self.durable.extend(finalized);
        // Advance the frontier and retire indices that pointed into the dropped turns.
        self.base = hi;
        self.suppress.retain(|&i| i >= self.base);
        self.tool_slot.retain(|_, &mut v| v >= self.base);
        if matches!(self.last_skill, Some(i) if i < self.base) {
            self.last_skill = None;
        }
    }

    /// Assemble the full presentable block list: the finalized `durable` prefix followed by the
    /// finalized open window (suppression applied window-relative). Because `base` always sits at a
    /// user-turn boundary and `finish_turns` distributes over such boundaries, this equals a single
    /// global finalize of the whole raw stream.
    /// The whole finalized stream (`durable ++ finalize_open(open)`) — the pre-C2 combined view.
    /// Test-only now: production splits this into [`drain_committed`] + [`open_snapshot`] so the
    /// replayer never holds the durable prefix. Kept for the equivalence oracles.
    #[cfg(test)]
    fn assemble(&self, open: Vec<Block>) -> Vec<Block> {
        let mut all = self.durable.clone();
        all.extend(self.finalize_open(open));
        all
    }

    /// Finalize just the open window: apply the remaining (window-relative) suppression, then
    /// `finish_turns` — **without** the `durable` prefix. This is the committed/open split point:
    /// `assemble` = `durable ++ finalize_open(open)`, and the accumulator combines its own drained
    /// committed blocks with this.
    fn finalize_open(&self, mut open: Vec<Block>) -> Vec<Block> {
        let drop: HashSet<usize> = self
            .suppress
            .iter()
            .filter(|&&i| i >= self.base)
            .map(|&i| i - self.base)
            .collect();
        if !drop.is_empty() {
            let mut i = 0usize;
            open.retain(|_| {
                let keep = !drop.contains(&i);
                i += 1;
                keep
            });
        }
        (self.shaping.finish_turns)(open)
    }

    /// `stamped` translated into the resident window — with the invariant `stamped >= base`
    /// asserted in debug and SATURATED in release: even if a future fold path lets a turn commit
    /// unstamped again (#56), the window math degrades (re-stamps from the window start) instead
    /// of wrapping to `usize::MAX` and panicking the viewer on a foreign transcript.
    fn window_stamped(&self) -> usize {
        debug_assert!(
            self.stamped >= self.base,
            "stamped ({}) fell behind base ({}) — a turn committed unstamped",
            self.stamped,
            self.base
        );
        self.stamped.saturating_sub(self.base)
    }

    /// Take the finalized **committed** blocks accumulated since the last drain (the turns that
    /// crossed the durability frontier), removing them from the replayer — so its resident content
    /// stays O(turn). The `SessionAccumulator` drains after each `apply` and `put`s each block once.
    pub(crate) fn drain_committed(&mut self) -> Vec<Block> {
        std::mem::take(&mut self.durable)
    }

    /// The finalized **open** window (the still-provisional tail) + the full per-turn timestamps
    /// (pending stamps flushed) — the non-consuming complement to [`drain_committed`]. The
    /// accumulator's snapshot is `committed ++ open_snapshot().0`.
    pub(crate) fn open_snapshot(&self) -> (Vec<Block>, Vec<Option<EpochSeconds>>) {
        let open = self.out.clone();
        let mut user_times = self.user_times.clone();
        let mut ws = self.window_stamped();
        stamp_user_turns(&open, &mut ws, self.pending_ts, &mut user_times);
        (self.finalize_open(open), user_times)
    }

    /// Finalize (consuming): final user-turn flush + completions + the agent `finish`.
    /// Returns the grouped blocks and the per-turn timestamps. Test-only now — production
    /// finalizes non-consumingly via [`snapshot`](Self::snapshot) (the `SessionAccumulator` folds
    /// incrementally and re-snapshots), so this consuming path is exercised only by the
    /// equivalence oracles.
    #[cfg(test)]
    pub(crate) fn into_blocks(mut self) -> (Vec<Block>, Vec<Option<EpochSeconds>>) {
        let mut ws = self.window_stamped();
        stamp_user_turns(&self.out, &mut ws, self.pending_ts, &mut self.user_times);
        self.stamped = self.base + ws;
        let open = std::mem::take(&mut self.out);
        let blocks = self.assemble(open);
        (blocks, self.user_times)
    }

    /// Non-consuming finalize: the whole presentable stream + per-turn times over cloned state.
    /// Test-only now — production drives [`drain_committed`] + [`open_snapshot`] via the
    /// `SessionAccumulator` (the replayer never materializes the durable prefix). Kept for the
    /// direct-replayer incremental tests.
    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> (Vec<Block>, Vec<Option<EpochSeconds>>) {
        let open = self.out.clone();
        let mut user_times = self.user_times.clone();
        let mut ws = self.window_stamped();
        stamp_user_turns(&open, &mut ws, self.pending_ts, &mut user_times);
        let blocks = self.assemble(open);
        (blocks, user_times)
    }
}

/// Batch L2 fold — `Replayer::new(); apply(all); into_blocks()`. For Claude,
/// `replay(tokenize(x), &CLAUDE_SHAPING)` is asserted bit-identical to `parse_main(x)`; for
/// Codex, to `parse_lines(x)`. `user_times` is filled with one entry per emitted user turn.
#[cfg(test)]
pub(crate) fn replay(
    messages: &[Message],
    user_times: &mut Vec<Option<f64>>,
    shaping: &Shaping,
) -> Vec<Block> {
    let mut r = Replayer::new(shaping);
    r.apply(messages);
    let (blocks, ut) = r.into_blocks();
    user_times.extend(ut);
    blocks
}

/// One entry in the reconstructed prompt queue. `marker_idx` is the index of this
/// prompt's `⧗ queued:` marker in the block list (prose only); `content_at_enqueue`
/// snapshots `content_seq` at submit so a later pop can tell whether any agent work
/// happened in between (immediate → suppress the marker).
pub(crate) struct QueueItem {
    pub(crate) content: String,
    pub(crate) marker_idx: Option<BlockIndex>,
    pub(crate) content_at_enqueue: usize,
}

// (queue-operation handling is inlined in `parse_main`'s `Some("queue-operation")`
// arm — it needs the block list, `content_seq`, and `suppress`.)

/// Record `ts` for every user turn in `out[*stamped..]`, advancing `stamped`.
pub(crate) fn stamp_user_turns(
    out: &[Block],
    stamped: &mut usize,
    ts: Option<EpochSeconds>,
    user_times: &mut Vec<Option<EpochSeconds>>,
) {
    for b in &out[*stamped..] {
        if matches!(b, Block::UserText(_) | Block::Command { .. }) {
            user_times.push(ts);
        }
    }
    *stamped = out.len();
}
