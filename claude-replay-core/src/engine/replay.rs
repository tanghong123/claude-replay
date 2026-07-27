//! **Layer 2 — the shared fold.** The stateful [`Replayer`] folds a stream of canonical
//! [`Message`]s (produced by any agent's Layer-1 decoder)
//! into render [`Block`]s, via the per-agent [`Shaping`] seam and the
//! streaming [`parse_stream`] driver. Agent-agnostic: everything that differs by agent enters
//! through `Shaping` (a `&'static` const per adapter) plus the `decode` closure. The data
//! model it produces and block classification live in [`crate::model`]; this module is the
//! machinery that builds it. Byte-identical to the retired `parse_main`/`parse_lines` oracles
//! (see the equivalence gates in `claude_model`/`codex_model`).

use crate::engine::message::{Message, QueueOpKind};
use crate::model::*;
use crate::Agent;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// **Pass-1 id pre-scan.** Collect the tool-call ids that a later result WILL be joined onto,
/// so a result whose `tool_use` appears further down the stream is held pending rather than
/// mis-emitted as an orphan (the streaming fold matches a whole-file batch this way). The
/// read/trim/parse/skip-non-JSON skeleton is shared; each agent supplies `extract`, which pulls
/// this line's join ids into the set (Claude reads `assistant`→`tool_use.id`; Codex reads
/// `response_item`→`call_id`).
pub(crate) fn scan_ids<S: AsRef<str>>(
    lines: impl Iterator<Item = S>,
    extract: impl Fn(&Value, &mut HashSet<String>),
) -> HashSet<String> {
    let mut ids = HashSet::new();
    for line in lines {
        if let Ok(v) = serde_json::from_str::<Value>(line.as_ref().trim()) {
            extract(&v, &mut ids);
        }
    }
    ids
}

/// **The streaming L2 driver** (M9). Feed a [`Replayer`] one line's messages at a time —
/// `decode` (the agent's per-line L1, capturing its `cwd`) turns each line into a few
/// messages that are folded immediately — so no whole-file `Vec<Message>` is built: peak
/// memory is one line + the block buffer, matching the retired `parse_main`. `tool_ids` is
/// the pass-1 id pre-scan; `reader` is a fresh pass-2 read. This equals `replay(tokenize(x))`
/// over the same input (proven by `parse_file_matches_parse_str` + the golden corpus).
pub(crate) fn parse_stream<R: std::io::BufRead>(
    reader: R,
    tool_ids: HashSet<String>,
    shaping: &Shaping,
    mut decode: impl FnMut(&str, &mut Vec<Message>),
    mut fold_metrics: impl FnMut(&Value),
    user_times: &mut Vec<Option<f64>>,
) -> std::io::Result<Vec<Block>> {
    let mut r = Replayer::new(shaping, tool_ids);
    let mut buf: Vec<Message> = Vec::new();
    for line in reader.lines() {
        let line = line?;
        buf.clear();
        decode(&line, &mut buf);
        // Fold token/cost metrics in the SAME pass (M10) — one read instead of two. The
        // metrics re-parse of the line matches the retired `parse_reader_for` exactly
        // (from the raw line, skip on parse error), so the tally is byte-identical.
        if let Ok(v) = serde_json::from_str::<Value>(&line) {
            fold_metrics(&v);
        }
        r.apply(&buf);
    }
    let (blocks, ut) = r.into_blocks();
    user_times.extend(ut);
    Ok(blocks)
}

/// Streaming whole-file parse for `agent`: blocks + one wall-clock timestamp per user turn +
/// folded metrics, all in a single pass (M9/M10). The one whole-file parse seam — batch
/// callers ([`parse_session_as`](crate::parse_session_as)) go through here; the live path
/// folds appended bytes via [`FollowParser`](crate::FollowParser). Flat (sub-agent trees are
/// loaded separately by [`enrich`](crate::adapter::TranscriptAdapter::enrich)).
pub(crate) fn parse_path_timed_for(
    agent: Agent,
    path: &std::path::Path,
) -> std::io::Result<(Vec<Block>, Vec<Option<f64>>, crate::metrics::Metrics)> {
    let mut times = Vec::new();
    let (blocks, metrics) = crate::adapter::adapter(agent).parse_path_timed(path, &mut times)?;
    Ok((blocks, times, metrics))
}

/// The small agent-specific seam of the otherwise-shared L2 fold — the embryo of the
/// `Adapter` (design §3.2). Everything else in this module is agent-agnostic; these **four**
/// fn-pointer hooks (each documented on its field below) are the only points Claude and Codex
/// differ: `build_tool` (shape a `tool_use` into a block), `join_result` (attach its result),
/// `keep_orphan` (keep a resultless result?), and `finish_turns` (final turn shaping). A
/// per-agent `&'static` const supplies them (`CLAUDE_SHAPING` / `CODEX_SHAPING`).
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
/// `tool_ids` is the L1 id pre-scan (so an orphan result is told from a not-yet-seen one);
/// the caller supplies it — from the whole message log for a batch, or a streaming pre-scan.
pub(crate) struct Replayer<'a> {
    shaping: &'a Shaping,
    tool_ids: HashSet<String>,
    out: Vec<Block>,
    user_times: Vec<Option<f64>>,
    pending_ts: Option<f64>,
    stamped: usize,
    tool_slot: HashMap<String, usize>,
    pending: HashMap<String, (String, Value)>,
    trigger_ts: Option<f64>,
    queue: Vec<QueueItem>,
    content_seq: usize,
    suppress: Vec<usize>,
    last_skill: Option<usize>,
    completions: Vec<CompletionRec>,
}

impl<'a> Replayer<'a> {
    pub(crate) fn new(shaping: &'a Shaping, tool_ids: HashSet<String>) -> Self {
        Replayer {
            shaping,
            tool_ids,
            out: Vec::new(),
            user_times: Vec::new(),
            pending_ts: None,
            stamped: 0,
            tool_slot: HashMap::new(),
            pending: HashMap::new(),
            trigger_ts: None,
            queue: Vec::new(),
            content_seq: 0,
            suppress: Vec::new(),
            last_skill: None,
            completions: Vec::new(),
        }
    }

    /// Fold a batch of messages into the running state (append, back-patch, stamp).
    pub(crate) fn apply(&mut self, messages: &[Message]) {
        let (join_result, keep_orphan) = (self.shaping.join_result, self.shaping.keep_orphan);
        for m in messages {
            match m {
                Message::LineStart(ts) => {
                    stamp_user_turns(
                        &self.out,
                        &mut self.stamped,
                        self.pending_ts,
                        &mut self.user_times,
                    );
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
                    let idx = self.out.len() - 1;
                    if let Block::ToolUse { name, .. } = &self.out[idx] {
                        if name == "Skill" {
                            self.last_skill = Some(idx);
                        }
                    }
                    if !id.is_empty() {
                        self.tool_slot.insert(id.clone(), idx);
                        if let Some((txt, tur)) = self.pending.remove(id) {
                            join_result(&mut self.out[idx], &txt, &tur);
                        }
                    }
                }
                Message::ToolResult {
                    tool_use_id,
                    text,
                    tur,
                } => {
                    if let Some(&idx) = self.tool_slot.get(tool_use_id) {
                        join_result(&mut self.out[idx], text, tur);
                    } else if self.tool_ids.contains(tool_use_id) {
                        self.pending
                            .insert(tool_use_id.clone(), (text.clone(), tur.clone()));
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
                    if !attach_skill_body(&mut self.out, self.last_skill, text)
                        && !fallback.is_empty()
                    {
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
                    if let Some(Block::Command { output, .. }) = self.out.last_mut() {
                        output.push(text.clone());
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
                    // L1 already parsed the notification; the fold only places the block and
                    // records the terminal status for the post-loop `SubAgent` back-patch.
                    self.completions.push(CompletionRec {
                        tool_use_id: tool_use_id.clone(),
                        task_id: task_id.clone(),
                        status: *status,
                    });
                    let agent_id = if !task_id.is_empty() {
                        task_id.clone()
                    } else {
                        tool_use_id.clone()
                    };
                    self.out.push(Block::AgentDone {
                        agent_id,
                        agent_type: String::new(),
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
                                Some(self.out.len() - 1)
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
    }

    /// Finalize (consuming): final user-turn flush + completions + the agent `finish`.
    /// Returns the grouped blocks and the per-turn timestamps.
    pub(crate) fn into_blocks(mut self) -> (Vec<Block>, Vec<Option<f64>>) {
        stamp_user_turns(
            &self.out,
            &mut self.stamped,
            self.pending_ts,
            &mut self.user_times,
        );
        apply_completions_and_suppress(
            &mut self.out,
            &self.tool_slot,
            &self.completions,
            self.suppress,
        );
        let blocks = (self.shaping.finish_turns)(self.out);
        (blocks, self.user_times)
    }

    /// Non-consuming finalize (M11): the current presentable blocks + per-turn times, WITHOUT
    /// consuming the Replayer — so a live follower can `apply` a delta, `snapshot` to render,
    /// then keep folding. Same output as `into_blocks`, computed over cloned working state.
    /// (Proven byte-identical vs a full re-parse — used by the live `FollowParser`, M16.)
    pub(crate) fn snapshot(&self) -> (Vec<Block>, Vec<Option<f64>>) {
        let mut out = self.out.clone();
        let mut user_times = self.user_times.clone();
        let mut stamped = self.stamped;
        stamp_user_turns(&out, &mut stamped, self.pending_ts, &mut user_times);
        apply_completions_and_suppress(
            &mut out,
            &self.tool_slot,
            &self.completions,
            self.suppress.clone(),
        );
        let blocks = (self.shaping.finish_turns)(out);
        (blocks, user_times)
    }

    /// Merge more tool_use join ids into the pre-scan set (M11): a live follower pre-scans
    /// each *delta* for its ids and extends before applying, so a result whose tool_use is
    /// later in the SAME delta is held pending (not mis-emitted as an orphan) — exactly as a
    /// batch pre-scan would. Across polls, earlier deltas' ids are already accumulated; the
    /// only remaining reorder (a result physically before its tool_use) is a rewritten tail,
    /// which the follower handles by rebuilding from scratch (a `reset`).
    pub(crate) fn extend_tool_ids(&mut self, ids: impl IntoIterator<Item = String>) {
        self.tool_ids.extend(ids);
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
    let tool_ids: HashSet<String> = messages
        .iter()
        .filter_map(|m| match m {
            Message::ToolUse { id, .. } if !id.is_empty() => Some(id.clone()),
            _ => None,
        })
        .collect();
    let mut r = Replayer::new(shaping, tool_ids);
    r.apply(messages);
    let (blocks, ut) = r.into_blocks();
    user_times.extend(ut);
    blocks
}

/// The `parse_main` post-loop: apply agent-completion notifications to their `SubAgent`
/// / `AgentDone` blocks (by tool-use-id, else task-id), then drop the `⧗ queued:` markers
/// of prompts picked up immediately. Split out so both `parse_main` and `replay` share
/// one copy. Runs before turn grouping so surviving markers keep their positions.
fn apply_completions_and_suppress(
    out: &mut Vec<Block>,
    tool_slot: &HashMap<String, usize>,
    completions: &[CompletionRec],
    suppress: Vec<usize>,
) {
    if !completions.is_empty() {
        let mut agent_slot: HashMap<String, usize> = HashMap::new();
        for (i, b) in out.iter().enumerate() {
            if let Block::SubAgent(sa) = b {
                if !sa.agent_id.is_empty() {
                    agent_slot.insert(sa.agent_id.clone(), i);
                }
            }
        }
        for rec in completions {
            let idx = (!rec.tool_use_id.is_empty())
                .then(|| tool_slot.get(&rec.tool_use_id).copied())
                .flatten()
                .or_else(|| {
                    (!rec.task_id.is_empty())
                        .then(|| agent_slot.get(&rec.task_id).copied())
                        .flatten()
                });
            if let Some(Block::SubAgent(sa)) = idx.and_then(|i| out.get_mut(i)) {
                if let Some(st) = rec.status {
                    sa.status = st;
                }
            }
        }
        let mut by_id: HashMap<String, (String, String)> = HashMap::new();
        for b in out.iter() {
            if let Block::SubAgent(sa) = b {
                let v = (sa.agent_id.clone(), sa.agent_type.clone());
                if !sa.agent_id.is_empty() {
                    by_id.insert(sa.agent_id.clone(), v.clone());
                }
                if !sa.tool_use_id.is_empty() {
                    by_id.insert(sa.tool_use_id.clone(), v);
                }
            }
        }
        for b in out.iter_mut() {
            if let Block::AgentDone {
                agent_id,
                agent_type,
                ..
            } = b
            {
                if let Some((real_id, ty)) = by_id.get(agent_id.as_str()) {
                    *agent_type = ty.clone();
                    *agent_id = real_id.clone();
                }
            }
        }
    }
    if !suppress.is_empty() {
        let drop: HashSet<usize> = suppress.into_iter().collect();
        let mut i = 0usize;
        out.retain(|_| {
            let keep = !drop.contains(&i);
            i += 1;
            keep
        });
    }
}

/// One entry in the reconstructed prompt queue. `marker_idx` is the index of this
/// prompt's `⧗ queued:` marker in the block list (prose only); `content_at_enqueue`
/// snapshots `content_seq` at submit so a later pop can tell whether any agent work
/// happened in between (immediate → suppress the marker).
pub(crate) struct QueueItem {
    pub(crate) content: String,
    pub(crate) marker_idx: Option<usize>,
    pub(crate) content_at_enqueue: usize,
}

/// A structured agent/task completion (L1-parsed from the raw notification) — the fold's
/// record for back-patching a `SubAgent`'s terminal status after the loop. `status` is
/// `None` when the source carried no explicit status (then the spawn is left untouched).
pub(crate) struct CompletionRec {
    /// The spawning `Agent`/`Task` **tool_use id** (from the notification's `<tool-use-id>`) —
    /// the *primary* key that back-patches this completion onto its `SubAgent` spawn block
    /// (which stores the same id). Empty when the notification carried no `<tool-use-id>`.
    pub(crate) tool_use_id: String,
    /// The notification's `<task-id>`. For an agent completion this **is the agent's id**
    /// (matched against `SubAgent.agent_id`) — the *fallback* join key, used when the
    /// notification keyed by task-id rather than tool-use-id. Empty when absent.
    pub(crate) task_id: String,
    /// Terminal state from the notification's `<status>`; `None` when it carried none (the
    /// spawn's status is then left untouched).
    pub(crate) status: Option<AgentStatus>,
}

// (queue-operation handling is inlined in `parse_main`'s `Some("queue-operation")`
// arm — it needs the block list, `content_seq`, and `suppress`.)

/// Record `ts` for every user turn in `out[*stamped..]`, advancing `stamped`.
pub(crate) fn stamp_user_turns(
    out: &[Block],
    stamped: &mut usize,
    ts: Option<f64>,
    user_times: &mut Vec<Option<f64>>,
) {
    for b in &out[*stamped..] {
        if matches!(b, Block::UserText(_) | Block::Command { .. }) {
            user_times.push(ts);
        }
    }
    *stamped = out.len();
}
