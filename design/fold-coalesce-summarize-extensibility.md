# Study: should folding, coalescing, and summarization be agent-extendable, or stay in the core? (#58)

**Recommendation: stay in the core, behind the seams that already exist. Extract no
new interface now.** The one per-agent decision the layers genuinely need — *whether*
to group at all — is already a per-agent hook (`Shaping::finish_turns`); everything
else keys on the canonical tool vocabulary that adapters are contractually required
to normalize onto, which makes per-agent variants redundant until an agent arrives
that can't normalize. A migration sketch for that day is at the end.

## The three layers, as built (2026-07-29, post-#57/#50/#15)

| layer | what it decides | where it lives | per-agent seam today |
|---|---|---|---|
| **Coalescing** | span structure: what folds into a `Thinking` span, what breaks it | `core::model::coalesce_spans` (agent-neutral walk over `Block`s) | `Shaping::finish_turns` — Claude wires `coalesce_spans`, Codex wires `identity` |
| **Summarization** | the span line's phrasing: bash semantic classes, git output phrases, clause vocabulary/order | `src/present.rs` (`activities`/`turn_summary`/`thinking_summary`, `classify_bash`) | none — one source for both frontends |
| **Folding** | what renders expanded vs collapsed by default | `src/fold.rs` `FoldPolicy`, keyed by `model::fold_key` | none — agent-blind, driven by block classification |

## Why staying put is right

1. **The canonical-vocabulary contract already does the extensibility work.**
   `is_activity_tool`, `block_kind`, and `fold_key` classify on Claude Code's tool
   *names* by design, and each agent's `Shaping::build_tool` normalizes its own
   names onto that vocabulary (`codex_model::normalize_tool_name`; QoderWork rides
   Claude's decoder wholesale). An agent that maps its shell tool onto `Bash` gets
   the #57 bash classes, git phrases, and span structure *correctly and for free*.
   A per-agent summarization interface would ask every adapter to re-answer a
   question the name-normalization already answered.

2. **The one genuinely per-agent decision already has its hook.** Codex sessions
   are deliberately un-grouped (`codex_finish = identity`) — that IS agent-specific
   fold/coalesce policy, expressed in one line at the existing seam. When a third
   agent wants CC-style spans, its adapter points `finish_turns` at
   `coalesce_spans`; when it wants nothing, identity. The seam carries the whole
   structural question.

3. **Summarization must not fragment.** `present.rs` is one-source-for-two-frontends
   (TUI + HTML `thinking_summary`) precisely so wording can't drift. Splitting it
   per agent multiplies the drift surface by the agent count for zero present-day
   benefit — no current agent wants different phrasing for the same canonical
   blocks.

4. **The invariant that constrains any future hook lives in the core.**
   `finish_turns` must distribute over user-turn boundaries (the committed/open
   split replays chunks independently; `assemble = durable ++ finalize_open(open)`
   is only a global finalize because spans never cross a user turn). A per-agent
   span hook would inherit this proof obligation; keeping the only span
   implementation in the core keeps the proof in one place, next to the tests that
   pin it (`spans_merge_between_visible_outputs_and_break_on_cc_breakers`,
   the split-apply and incremental equivalence gates).

5. **Cost asymmetry.** Core churn per new agent has been near zero in practice
   (QoderWork landed as a `*_discover` file + one adapter row + zero shared-engine
   edits). Widening the adapter trait with fold/summarize hooks is cheap to *write*
   but expensive to *hold*: every hook is API surface each future agent must
   consider, and most would wire the same defaults.

## What WOULD justify extraction (the trigger conditions)

- An agent whose transcripts can't normalize onto the canonical vocabulary
  (e.g. no tool-call structure at all, or semantics where "read/search/run"
  is the wrong frame), so `activities()` produces nonsense for it; or
- an agent whose OWN UI has a distinct, load-bearing summary language the user
  expects replay to mirror (the way #57 mirrors Claude Code), i.e. a second
  empirically-derived phrasing spec like `cc-activity-coalescing.md`.

Codex today satisfies neither: its adapter opts out of grouping entirely, which
the existing seam expresses.

## Migration sketch (when a trigger fires)

Small and mechanical, in dependency order:

1. **Coalescing** — nothing to do: `finish_turns` is already the hook. Add the
   new agent's span function beside `coalesce_spans` (core or its adapter file);
   the distributivity invariant must hold (document + reuse the split-apply test
   pattern).
2. **Summarization** — move `activities`/`turn_summary`/`thinking_summary` +
   `classify_bash` from `src/present.rs` into the core (they are pure over
   `Block`s; the HTML/TUI already just call them), then widen `Shaping` with
   `summarize: fn(duration, &[Block]) -> String` defaulting to the CC
   implementation. Frontends call through the session's agent shaping instead of
   the free function — a rename-level change at ~6 call sites.
3. **Folding** — `fold_key` is already the indirection; a divergent agent adds
   its mapping in its `build_tool` normalization, not in `fold.rs`. Only if an
   agent needs different DEFAULT policies per key does `FoldPolicy::default_for
   (agent)` become a function of the agent — one constructor change.

## Verdict

Keep all three layers in the shared core. The `finish_turns` seam plus the
name-normalization contract is the agent-extendable interface — it is just thinner
than a trait full of hooks, and it has carried three agents without a shared-engine
edit. Re-open this on the first trigger condition above, with the migration sketch
as the plan of record.
