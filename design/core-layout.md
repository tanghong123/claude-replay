# Core layout study: agent adapters out of core + namespace audit (#87)

**Status: SIGNED OFF 2026-07-31. Part A step 1 and Part B are EXECUTED (v1.20.0):
`agents/<agent>/{model,metrics,discover}.rs` families, `engine/seam.rs` as the audited
adapter contract (`agents_import_only_the_seam` enforces seam-only imports),
`reader` → `engine`, `cache/stream.rs` → `present::pull` (old paths aliased).
Steps 2–3 EXECUTED (v1.22.0, user-directed): `Agent` is an open interned id (constants +
`Agent::new`; serde as the label; the 4 match sites open-world), and the crate split is
real — `claude-replay-engine` (machinery + public seam + trait), `claude-replay-agents`
(families + `REGISTRY` + the engine-integration tests), `claude-replay-core` (the wired
facade, same API). Engine internals take adapters (`SessionAccumulator::new(adapter)`,
`FollowParser::open(adapter, path)`); the facade curries via `adapter(agent)`.**

The engine's goal is to be broadly usable, and its own layout is part of that surface:
a consumer (or a new-agent author) should be able to tell *where things live and why*
without a guide. Today two things fail that test:

1. The "a third party adds an agent without forking core" story is **not actually true**:
   `TranscriptAdapter` is `pub(crate)`, the `Agent` enum is closed, and the registry is a
   hard-coded match. The seam exists and is clean — but only inside the crate boundary.
2. The root-vs-`engine` module split is historical, not principled (`follow` at root,
   `tier_b` under `engine`; seven per-agent files sprawled at root), and `present::cache`
   carries stale docs plus a wire protocol that isn't caching.

---

## Part A — moving the agent adapters out of core

### A.1 What the seam is today (measured, not assumed)

The intended seam is `adapter.rs`: `TranscriptAdapter` (sniff / decode_line / shaping /
metrics_acc / enrich / discovery / load_attachment / load_tasks) + the `adapter()` /
`adapters()` registry. All `pub(crate)`. But the *actual* surface the per-agent files
consume is wider. A grep audit of `crate::` paths inside the seven per-agent files:

| Per-agent file | Reaches into |
|---|---|
| `claude_model` | `engine::{builder, message, replay, tasks, path::relativize, time::epoch_secs, build_sub_agents}`, `model::coalesce_spans`, `metrics::parse_reader_for` |
| `codex_model` | `engine::{message, replay::{parse_path_timed_for, replay, stamp_user_turns}, path::relativize, time::epoch_secs}`, `model::*` |
| `claude_metrics` / `codex_metrics` | `adapter::*`, `metrics::{estimate_cost, parse_reader_for}` |
| `claude_discover` | `discover::{ancestors_below, home_dir}`, `engine::tasks::task_from_json` |
| `codex_discover` | `discover::{ancestors_below, home_dir}`, `codex_model::is_host_context` |
| `qoderwork_discover` | **`claude_discover::{candidates_scoped_in, transcript_by_id_in}`**, `engine::parse_session_as` |

So the *real* adapter contract is: the trait, plus ~a dozen helper functions and types
(`Shaping`, `Message`, `QueueItem` via `Shaping`, `relativize`, `epoch_secs`,
`stamp_user_turns`, `coalesce_spans`, `parse_reader_for`, `estimate_cost`,
`ancestors_below`, `home_dir`, `task_from_json`, `parse_session_as`). None of this is
written down anywhere — it is discoverable only by reading the adapter sources. That is
exactly the "does consuming code need somebody to explain it?" smell, applied to the
next agent's author.

One genuine boundary violation: `qoderwork_discover` reaches into `claude_discover`'s
internals. The *capability* is legitimate (QoderWork is a derived agent that stores
Claude-format transcripts in a Claude-shaped store under a different root) — but the
mechanism should be a named, documented seam helper ("a Claude-format store rooted at
`<dir>`"), not a private cross-import between two adapters.

### A.2 What "out of core" must mean

Target shape (three crates where core is today, current consumers see **zero** change):

```
claude-replay-engine    the agent-free machinery: model, engine/*, discover (generic
                        parts), follow, metrics, fold, summary, diff, transcript,
                        + pub trait TranscriptAdapter + pub seam helpers.
                        Entry points that today consult the registry take it as a
                        parameter: detect_agent(head, &[&dyn TranscriptAdapter]) etc.

claude-replay-agents    depends on engine. The three adapters (+ their tests, incl.
                        the byte-equivalence gates). Exports
                        pub static REGISTRY: &[&dyn TranscriptAdapter].

claude-replay-core      the FACADE: re-exports engine wired with agents::REGISTRY —
                        today's exact API (parse_session, detect_agent, resolve_any …
                        with today's signatures, registry curried in). Everything
                        downstream (present/tui/html/bin/jdi) keeps importing core.
```

- **Registry mechanism: static slice through a facade.** No global mutable registration
  (`OnceLock`/`inventory`-style) — registration order and init-time global state are
  problems we don't have to buy. A third party building on `engine` passes its own slice
  (ours ± theirs) to the same parameterized entry points; the facade is just our curry.
- **Agent identity must open.** The closed `enum Agent` appears in 29 files across the
  workspace. Options: (i) keep the enum in `engine` and accept that a third-party agent
  adds a variant (fork — defeats the purpose); (ii) `Agent` becomes an interned id
  (`&'static str` newtype) with the three known constants; the places that `match` on it
  (picker labels, jdi supervisors, theme hues) become adapter-supplied metadata
  (`display_name`, etc.). (ii) is the honest open-world design and the bulk of the
  migration cost.
- **The seam becomes public.** `Shaping` (the L2 hook table), `Message`, and the helper
  list above become `pub` in `engine`. Note the calculus here: these crates are **not
  published to crates.io** — the workspace ships binaries. Publicizing the seam costs
  discipline (docs, deliberate change) but not semver freeze. Still, the fold vocabulary
  is *actively evolving* (`QueueItem` grew a field this week; `BlockStore::put` changed
  signature two tasks ago) — every such change would now be a documented-seam change.

### A.3 Cost / benefit and recommendation

| | Benefit | Cost |
|---|---|---|
| Crate-enforced contract | adapters *cannot* quietly widen the seam; the compiler is the reviewer | the seam must be curated once (write down A.1's list) |
| Third-party story becomes true | `engine` + your adapter + your registry slice = a new agent, no fork | `Agent` open-world rework across 29 files |
| Core dep bill | unchanged (both halves stay serde_json + anyhow) | — |
| Tests | equivalence gates move with their adapters — cleaner | churn in `claude_model`'s 2.8k lines (mostly tests) |

**Recommendation: adopt the target, execute in three separately-gated steps, in this
order — each is independently valuable and the byte gate must stay zero-diff at each.**

1. **Curate the seam in place (no crate split).** Group the per-agent files as
   `agents/claude/{model,metrics,discover}.rs`, `agents/codex/…`, `agents/qoderwork/…`
   inside core; route every A.1 helper through one `engine::seam` module (docs on each
   item: "part of the adapter contract"); replace the `qoderwork_discover` →
   `claude_discover` private imports with a named "Claude-format store at root"
   helper in the seam. After this step the seam is *documented and single-file
   auditable*; the grep audit in A.1 becomes a test (`agents/**` may import only
   `seam::` + std + serde_json).
2. **Open the agent identity.** The `Agent` enum → id newtype + adapter-supplied
   metadata; kill every `match agent` outside the adapters (the jdi supervisors keep
   theirs — jdi has its own adapter seam by design).
3. **Split the crates** (engine / agents / facade re-export as core). After 1+2 this is
   nearly mechanical.

Step 1 can start immediately after sign-off. Steps 2–3 are worth scheduling *after* the
fold/BlockStore vocabulary has been quiet for a few tasks — publicizing a seam mid-churn
just relabels every refactor as a contract change.

---

## Part B — namespace audit

### B.1 The rule (proposed)

> **Root = the vocabulary and entry points a consumer names. `engine` = the machinery
> that builds sessions, which consumers benefit from but never name. `agents/<agent>` =
> one family per agent.** Physical location should match the rustdoc story: if the
> developer guide names a type, its canonical path should not dive through `engine`.

`lib.rs` re-exports make location invisible to *imports*, but not to rustdoc, error
messages, or the reader forming a mental model — which is why the rule matters even
with a facade. (Evidence that paths already lag the code: the API-docs CI job has been
red since v1.18.0 on ~40 broken intra-doc links to renamed/moved items — fixed
separately from this proposal, no sign-off needed.)

### B.2 Disposition table (core)

| Module | Today | Verdict | Why |
|---|---|---|---|
| `model`, `metrics`, `diff`, `fold`, `summary`, `agent`, `discover`, `transcript` | root | **stay** | consumer-named vocabulary / entry points |
| `follow` | root | **stay** | `FollowParser` is consumer-named (present::cache holds one) |
| `adapter` | root | **stay root**, becomes the seam's front door (Part A) | the one thing a new-agent author names |
| `reader` | root (priv) | **→ `engine`** | follower plumbing nobody names |
| `claude_*`, `codex_*`, `qoderwork_*` (7 files) | root | **→ `agents/<agent>/…`** | Part A step 1; kills the root sprawl |
| `engine::{replay, builder, session, index, message, tasks}` | engine | **stay** | the pipeline machinery |
| `engine::{path, time}` | engine | **stay** | machinery utils (only the seam re-exports them) |
| `engine::tier_b` | engine | **stay**, but its *name*: `TierBStore` is consumer-visible as present's default `P` — re-export it from root beside `InMemoryStore` (already done) and keep the file where the other stores live | stores are engine machinery; the *type* is vocabulary |

Net: two physical moves (`reader` in, per-agent files grouped), zero API change.

### B.3 `present::cache` review

- **`cache/stream.rs` is not caching** — it is the pull wire protocol (`Cursor`,
  `PullReply`, `pull`, `pull_indices`, `PullClient`), the same 4-member-cursor contract
  the JS client implements. Proposal: promote to **`present::pull`** (matches the docs'
  name for it, "the pull protocol"), re-exported as today. `cache` keeps `SessionCache` +
  `SharedSession` (residency + the servable live state) — one concern per module name.
- **Stale docs to rewrite in place** (found during this audit): `cache/mod.rs`'s module
  doc still describes the retired `poll() -> Session` API and pre-#85 tier list
  ((a)/(a′)/(c)); the `#[allow(dead_code)] mod stream` attribute + "Phase C step 4/5"
  comment are leftovers — the pull path has been the only transport since #85.
- `args`/`highlight`/`present`/`sys` at present-root: correct as-is (vocabulary).

### B.4 What this does NOT change

No behavior, no frontend-visible signature, no new dependencies, byte gate zero-diff at
every step. The `jdi` supervisor seam (`jdi::agent`) is out of scope — it is a different
contract (process supervision, not transcript parsing) and already documented as such.

---

## Sign-off asks

1. Part A: agree the target (engine/agents/facade) and the 3-step sequencing? Any appetite
   to pull step 2 (open `Agent`) earlier/later?
2. Part B: agree the root-vs-engine rule + the two moves + `present::pull` rename?
3. OK to fix the red API-docs CI (broken intra-doc links) and the stale `present::cache`
   docs immediately, independent of the above?
