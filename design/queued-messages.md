# Mid-turn queued messages — trace observations & algorithm

When you type a message **while Claude Code is still working**, it does not land in
the transcript as a normal `user` event. It goes through a *queue*, and for the vast
majority of prompts the only durable record is an **attachment** that CC writes at the
moment the agent picks the prompt up. claude-replay historically ignored attachments,
so those messages silently vanished from the replay. This document records exactly what
the transcript shows and the algorithm `model.rs` now uses to recover them.

All row numbers and counts below come from the reference session
`094539f2-…` (a real, settled claude-replay working session).

---

## 1. The four event types involved

A mid-turn message touches up to three different event `type`s:

| `type`             | `operation` / `attachment.type`     | Role |
|--------------------|-------------------------------------|------|
| `queue-operation`  | `enqueue`                           | You submitted a message while the agent was busy — it enters the queue. Carries the full text in `content`. |
| `queue-operation`  | `remove` / `dequeue`                | The queue changed. **Content-less** → a FIFO *front pop* (the oldest queued item was consumed). **Content-named** → that exact entry left the queue (consumed, or you edited/retracted it). |
| `attachment`       | `queued_command`                    | The **authoritative record** of a consumed queued command, emitted at the consumption point. Carries `prompt` (the text), `commandMode` (`"prompt"` = human-typed, `"task-notification"` = background), and `origin.kind` (`"human"`). |

Note `queue-operation` events are *not* `user` events — that is the whole reason the
messages went missing before: a parser that only looks at `type == "user"` never sees
them.

---

## 2. The lifecycle, as seen in the trace

Here is a verbatim window (rows 674–688) showing two messages you typed mid-turn:

```
674  queue-operation  enqueue   "queue this up as a todo as well"   ← you type, mid-turn
675  queue-operation  enqueue   "show me the queued tasks"          ← you type again
     … agent keeps emitting tool_use / thinking …
683  queue-operation  remove    (content-less front pop)            ← agent picks up #1
684  queue-operation  remove    (content-less front pop)            ← agent picks up #2
685  user             (carries tool_results, no prose)              ← the turn that consumes them
686  attachment       queued_command  "queue this up as a todo…"    ← authoritative record #1
687  attachment       queued_command  "show me the queued tasks"    ← authoritative record #2
688  assistant        … now responds to both …
```

So the lifecycle of one mid-turn message is:

```
enqueue ─────► (sits in the queue while the agent works) ─────► remove/dequeue ─────► queued_command attachment
 (has text)                                                      (front pop, no text)  (has text, at consumption point)
```

The `queued_command` attachment is grouped with the tool-result-carrying `user` event
of the *running* turn — CC surfaces the message "within the running turn, alongside the
next tool result", not as a separate conversation turn.

### The key finding: the attachment is usually the ONLY record

Of the **81** human-typed queued commands in the reference session
(`commandMode == "prompt"`), **80 never appear as a standalone `user` event anywhere in
the transcript.** They exist *only* as `queued_command` attachments. Examples that were
being dropped entirely: `"show me the queued tasks"`, `"write tool block should by
default be folded."`, `"hold on to do those edits…"`, `"queue this up as a todo as
well"`.

(The lone exception was the word `"continue"`, which also happened to be typed as a
normal turn elsewhere — a coincidence, not a duplicate. See §5 on doubles.)

### Two flavors of consumption

Content-less pops split two ways in the trace, and both are legitimate:

- **`remove` / `dequeue` → `queued_command` attachment** (the common case above): a
  human prompt consumed mid-turn. Recovered from the attachment.
- **`enqueue` → `dequeue` → a full standalone `user` event** (e.g. the agent-jdi
  `"You are running UNATTENDED…"` supervisor prompts at rows 194–196): these *do*
  materialize as their own `user` turn, and have **no** `queued_command` attachment.
  They render through the normal `user`-event path.

The op name (`remove` vs `dequeue`) is **not** a reliable signal for which flavor you
have — in the reference data all content-named removals use `remove`, and content-less
pops are an arbitrary 38 `remove` / 39 `dequeue` split. The reliable signal is the
`attachment` itself and its `commandMode`.

---

## 3. Reference counts (settled session)

```
queue-operation:  143 enqueue · 104 remove · 39 dequeue
attachment.type:  queued_command ×105  (81 commandMode="prompt", 24 "task-notification")
                  task_reminder ×331 · deferred_tools_delta ×59 · edited_text_file ×90 · … (ignored)

human queued_command prompts:                 81
  …that ALSO appear as a standalone user turn:  1   ("continue", coincidental)
  …recorded ONLY as the attachment:            80   ← these were being lost

❯ user turns rendered:  188 (before) → 269 (after)   (+81, exactly the queued_commands)
```

---

## 4. The algorithm (`model.rs`, `parse_main`)

The parser streams the JSONL once, in order, pushing render blocks as it goes. Turn
grouping never reorders user blocks, so **anything pushed in file order stays in
chronological order.** Two independent mechanisms cover queued messages:

### (a) Render consumed prompts from the attachment — the primary fix

```rust
Some("attachment") => {
    let a = v.get("attachment");
    let is_prompt = a…get("type")        == Some("queued_command")
                 && a…get("commandMode") == Some("prompt");
    if is_prompt {
        if let Some(p) = a…get("prompt") { out.push(Block::UserText(p)); }
    }
}
```

- Fires at the attachment's stream position → the recovered turn lands exactly where the
  agent consumed it (correct chronological order).
- `commandMode == "prompt"` keeps human messages and drops `"task-notification"`
  background noise — no string heuristics, CC labels it for us.
- **No dedup.** Every `queued_command` is a distinct message (see §5).

### (b) A `⧗ queued:` marker at submit time — the two-tier model

The `queue-operation` events drive a second, faithful view: **when you submitted** each
prompt (its `enqueue`), separate from **when the agent picked it up** (its attachment
turn). A prose `enqueue` emits a dim `Block::QueueEvent { text }` marker in place; the
whole queue (prose + background `<task-notification>`s) is tracked so a content-less FIFO
front pop lands on the right entry:

```rust
enqueue(prose)        → out.push(QueueEvent{text});  queue.push(item{marker_idx, content_at_enqueue})
enqueue(notification) → queue.push(item{marker_idx: None, …})   // tracked, no marker
remove/dequeue named  → pop matching item
remove/dequeue empty  → pop queue.front()                       // FIFO front pop
```

**Two-tier collapse.** Showing a marker *and* a turn for every message is redundant when
the agent grabs the prompt instantly. So each item snapshots a `content_seq` counter (the
running number of agent content blocks — assistant text / thinking / tool_use) at
`enqueue`; when it's popped, if `content_seq` is unchanged **no agent work happened in
between → the pickup was immediate → the marker is suppressed** (the `❯` turn alone
conveys it). If work *did* happen, the marker survives:

| pickup | renders as |
|--------|------------|
| **immediate** (no agent work between submit & pickup) | just the `❯` turn |
| **delayed** (agent worked in between) | `⧗ queued: …` at submit **+** the `❯` turn at pickup |
| **never** (still in flight, live `-f`) | `⧗ queued: …` only |

Measured on the reference session: **53%** immediate, **47%** delayed (gap of 1–3 agent
steps). Suppression is a post-loop filter over the block list — safe because `tool_slot`
is finished by then, and it runs before turn grouping so surviving markers keep their
positions. This also **replaces** the old end-of-stream "pending residual": an
unfinished prompt now shows its own inline marker rather than being appended at the tail.

### Live `-f`: collapsing a marker already on screen

The batch parser sees the whole sequence at once. The live tail does not:

- **HTML (`--html -f`)** re-parses the whole file each poll (`parse_path_timed_for`) and
  diffs block-lines against the previous frame, emitting a `reset` for the changed suffix
  — so an immediate pickup's marker is dropped and replaced by the turn automatically.
- **TUI (`-f`)** appends parsed blocks per poll (byte-offset tail), so `View::ingest` does
  the collapse by hand: when a `UserText` turn arrives and a matching `QueueEvent` sits in
  the **trailing run of markers** (nothing emitted since), it removes that marker so the
  turn replaces it. Once agent work has landed after the marker, the run is empty and the
  marker stays (delayed).

### Why nothing double-counts

The marker (submit) and the turn (pickup) are *different moments*, deliberately both
shown for delayed prompts. For immediate prompts the marker is suppressed, so only the
turn remains. On a settled transcript the queue nets to empty, so no stray markers linger.

---

## 5. Doubles are expected — do not dedup

If you interrupt a running prompt and retype it, you legitimately produce **two** real
messages with similar or identical text. The algorithm therefore never de-duplicates
queued commands: each `queued_command` attachment is rendered on its own. This is the
correct behavior — collapsing them would *lose* a message, which is the exact failure
mode this whole mechanism exists to prevent.

The two hard guarantees:

- **No message is lost** — every human `queued_command` becomes a turn (verified: 0 of
  81 missing from the render).
- **Turns stay in chronological order** — every turn (standalone or recovered) is pushed
  at its stream position and never reordered.

---

## 6. History (why earlier versions were wrong)

- **v0.25.0** modeled the queue but *ignored* content-less pops, so consumed prompts
  piled up and were appended at the end → ~53 phantom old messages at the bottom.
- **v0.25.1** added FIFO front pops, so the residual correctly netted to empty — but the
  consumed prompts then had *no* render path at all → 80 real messages silently dropped.
- **v0.26 (attachment render)** stopped treating the `queue-operation` stream as the
  render source and rendered from the authoritative `queued_command` attachment at its
  consumption point. Recovered all 80, no phantoms, correct order.
- **This version (two-tier)** adds the `⧗ queued:` submit-time marker (`Block::QueueEvent`)
  on top, collapsing it into the turn when the pickup was immediate (53%) and keeping both
  when there was a gap (47%) — so the submit-vs-pickup lag is visible without redundant
  repetition. Handled in the TUI, `--dump`, `--dump-html`/`--html`, and live `-f`.
