# Phase C completion — the cache as the universal data layer, both frontends thin

**Scope (locked with the user):** #31 (SessionCache as the universal data layer, owns
tier-b, both frontends) + #35 (all HTML paths through one accumulator) + #11 (memory —
the TUI render-model windowing) + **Part 2 of `pull-render-delta.md`** (committed served
as pointers; client range-reads). Then bugs #37/#38/#32/#33. Then a **think-backwards
audit**: no app code that needs the engine explained to it, nothing app-side the shared
libraries should own, no pure CPU/memory wins left on the table.

**Gates (every increment):** `cargo fmt --check` · `cargo clippy --all-targets` (no new
warnings) · `cargo test` · `/tmp/sc-gate/gate.sh` → `BYTE-IDENTICAL: PASS` · a dedicated
test for any behavior the gate doesn't drive (the `/pull` wire, the TUI interactions via
`TestBackend`). Commit per increment.

## Ground truth (what's already done vs the stale doc notes)

- `streaming-core-memory.md` stage-3 note "(A) emit-and-drop restructure — deferred" is
  **stale**: #30 landed it (Replayer drains committed per `advance_at`; put fires once).
  Likewise `tier_b.rs`'s header "snapshot maps every block through put ⇒ single-snapshot
  only" is stale — tier-b is already safe for the live path. Fix both notes in C-2.
- `parse_session_as` already rides the one `SessionAccumulator`, so `--dump-html` /
  `--dump-all-html` already share the spine (#35 is mostly *verification + de-bespoking*,
  not a rebuild).
- `TierBStore` is an in-memory byte buffer; the live win needs a **file-backed** store.
- The TUI already lazy-loads sub-agents (#36) but with its own `Frame.follower` residency
  logic — the cache should own it. `raw` is gone; `wrapped` + `body_cache` are the two
  remaining O(N) render caches.

## Increments

### P2 — pull Part 2: committed as pointers, client range-reads (`pull-render-delta.md` §6)

Server: the non-idle `/pull` reply replaces the spliced `committed:[…]` array with a
**pointer** `committed_ext: { offset, len, epoch }` into `<id>.records` (empty/absent when
no committed delta). A new route `/records?session=<id>&from=<off>&len=<n>&epoch=<e>`
serves exactly those bytes off the log; **409** on an epoch mismatch (the log was
recreated) — the client's stored cursor then re-pulls and the epoch bump resyncs it.
`pull_response` keeps computing `(cf, pf)` as today but returns byte-range coordinates
(`pr.offsets[cf] .. pr.len`) instead of reading them. Server splice RAM for committed → 0.

Client (`export.js`): two-phase apply — on a reply with `committed_ext.len > 0`, fetch the
range, split lines → records, then apply **atomically in the existing order** (committed
append, then provisional truncate/extend, then adopt the cursor). The `inflight` guard
spans both fetches; the cursor advances only after both succeed; any fetch/409 failure
drops the reply whole (the next poll re-pulls with the old cursor — idempotent by the
protocol). Provisional + meta stay inline (small, O(turn)).

Verify: serve-module test driving `pull_response` + `records_bytes` (the range read) —
committed records fetched by pointer equal the previously-spliced ones; 409 on stale
epoch. Manual browser e2e.

### C-1 — the cache owns the pull residents (#31)

Move `Live.shared: HashMap<id, (Instant, Arc<SharedSession>)>` into `SessionCache`:
`shared(id, open: impl FnOnce() -> SharedSession) -> Arc<SharedSession>` + the TTL reap
covering both resident kinds. `Live` keeps zero session-domain state for the pull path
(it keeps only presentation: titles, parents, render logs). One resident set, one policy.

### C-2 — file-backed tier-b; the live server's committed content off-heap (#31)

- `TierBStore` gains a **file-backed** mode (`TierBStore::file(path)` — append via a
  buffered writer + `flush` per drain batch; `get` reads `[offset, offset+size)` back).
  Fix the stale header notes (put-once landed; buffer vs file).
- `FollowParser<S: BlockStore = InMemoryStore>`: the follower goes generic; the light
  streaming surface (`advance_stream`/`stream_read`/`committed_tail`/counters/meta) works
  for any `S`; `poll`/`poll_session`/`poll_shared` stay on the default `Session<Block>`
  impl. `SharedSession` likewise `SharedSession<S = InMemoryStore>`.
- The live server opens pull residents as `SharedSession<TierBStore>` with the backing at
  `<bundle>/<id>.blocks`. Resident per followed session: O(turn) fold window + locator
  table + offsets — **no committed block content in RAM**. (`committed_tail(from)` decodes
  the delta once for its render-to-log; an epoch reset re-reads the backing, not the
  transcript.)

### C-3 — evict → persist → reload-from-materialization (#31)

On pull-resident TTL reap: persist the accumulator's state via the tier-b sidecar
(committed locators + provisional + times + metrics + sub_agents + maintained meta + the
source's byte length; the `.blocks` backing is already on disk). On re-admit: if the
source file's byte length is unchanged → reload (no re-fold, O(sidecar + on-demand
blocks)); if it grew/shrank → discard and re-fold from 0 (today's behavior — correct via
the epoch-resync protocol, which a fresh follower triggers anyway). The `.records` render
log + its offset table follow the same policy (offsets rebuilt by scanning the log's
lines on reload). Policy documented on the method: resuming a *grown* source from a
persisted fold-state is deliberately out (the replayer's open-window state is not
persistable); unchanged-source reload covers the actual case (revisiting finished/idle
sessions).

### C-4 — TUI adopts the cache for sub-agents; #35 closeout

- `App` owns a `SessionCache`; `Frame` drops its own `follower`/`last_used` — descend
  registers the child in the cache and polls through it; the residency budget
  (`MAX_RESIDENT_SUBAGENTS`) becomes cache policy (`reap_over_budget(keep, pinned=root)`),
  so the TUI stops hand-rolling eviction. Re-descend of an evicted child re-materializes
  from the registry (and from C-3's materialization when present).
- #35 closeout: verify each HTML path (`--dump-html`, `--dump-all-html`, `--html`,
  `-f --html`, `/pull`) assembles via the one accumulator/cache spine; remove any
  remaining bespoke assembly (e.g. `bundle.rs` re-parsing where the cache could serve).
  Expected small; the gate pins the outputs.

### C-5 — TUI render-model windowing (#11)

Per `streaming-core-memory.md`'s worked example (blocks stay resident — deliberate):
- Drop `wrapped: Vec<Line>` + `wrapped_tag` + `body_cache` (the O(N) styled caches).
- Add a **per-block display-height index** (width- and fold-keyed `Vec<u16>` + prefix
  sums): resize/fold recompute heights by cheap wrap-measure (no markdown/syntect); scroll
  math (scroll position, page/jump targets, scrollbar, mouse hit-testing, search-match
  line mapping) moves onto (block, row-in-block) coordinates via the prefix sums.
- Render **only the viewport** each frame through a bounded LRU of per-block styled lines
  (visible + margin); search scans block text (not rendered lines).
- `TestBackend` tests: scroll/fold/search/selection behavior unchanged on a fixture
  session; `--dump` (which renders everything once, streaming) stays byte-identical.

This is the largest, riskiest increment — it lands last in Phase C, in its own series of
small commits (height index first behind the existing render, then the cutover).

## Then: bugs

- **#37** HTML live-feed: clearing a filter restores the pre-filter scroll. Fix: on
  filter apply/clear, re-anchor to the block the user last navigated/scrolled to while
  filtered (track the anchor at navigation time, not a saved scroll offset).
- **#38** HTML export: implement the interactive mock's missing styling (blue focus
  border on the clicked block header; sweep `design/subagents/html-export-mock.html` for
  other gaps) — visual diff against the mock.
- **#32** JDI kills sessions actively working via subagents: staleness must consider
  child-transcript activity (the engine's `sub_agents` map + child transcript mtimes),
  not just the root transcript.
- **#33** JDI kills+restarts an idle agent blocked on an uncompletable external task:
  distinguish "blocked on external" from "stalled" (e.g. repeated identical failure output
  → back off / surface, don't restart-loop).

## Final: think-backwards audit — RESULT (2026-07-29)

Everything above is **built** (P2, C-1..C-5, bugs #37/#38/#32/#33 — commits a3cf48b,
005c427, 1a4107d, 90aa6dd, 347495d, a57bc2e, 81da145, 7596861, b5296d6, c2f0575).
The three questions, asked of every non-library file:

**1. Does any app code need the engine explained to it?** No. The full inventory of
engine touches outside the shared libraries: `bundle.rs` → `parse_session_as` (one call
per export); `tui/app.rs` → `parse_session_enriched_as` (dump/descend) + the
`SessionCache` (register/poll/poll_delta); `jdi` → `parse_session_enriched_as` (status);
`serve.rs` → the cache (`reap`/`shared_session`/`shared_peek`/`remove_pull`) + the
`SharedSession` surface (`advance`/`counters`/`pull_delta`/`hibernate`/`restore`/
`session_meta`). The deepest consumer — the `/pull` handler — reads top-to-bottom as
the protocol it implements. No app constructs a Replayer/accumulator or re-derives
engine state.

**2. Does app code do work a shared library should own?** Not anymore — that was most
of Phase C's content: residency/eviction/persistence moved into the cache (C-1/C-3/C-4);
committed-content storage into tier-b in core (C-2); the live header into the
accumulator (the maintained `SessionMeta`); child-title derivation rides the parent's
maintained meta. The one known remainder is the jdi supervisor's per-agent conditionals
— queued separately as task #17 (agent-agnostic spine), outside Phase C.

**3. Pure CPU/memory wins left?** None found at the pure-win bar. Landed this phase:
the 22 MB/poll block clone (gone), the per-poll O(N) index/sub-agent/meta scans (gone —
maintained state), committed block content off-heap (a 16 MB `.blocks` file for the
reference session), the reply body (25.2 MB → 2.7 MB via `/records` pointers), eviction
re-folds (hibernate/restore), and the TUI render model (359 MB → 162 MB RSS measured,
styled lines now O(window)). The remaining O(N) residents are deliberate, documented
choices: TUI blocks stay in RAM (`streaming-core-memory.md`'s worked example) and the
plain-text search index is the explicitly-allowed content-sized index.

**End state vs the two principles.** The engine/cache owns parsing, folding, residency,
storage tiers, persistence, and the live header; both frontends are thin protocol/
presentation shells over the same `SessionCache`, and each demonstrates a different
consumption style (batch entries, delta polls, the pull protocol) in a few calls each.
