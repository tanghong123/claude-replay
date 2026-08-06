# Design: the `session_card` seam — cheap, repeatable title derivation

> **v2 — BUILT** (the Claude half; QoderWork is §9). Revises the interface shipped in v1.40.0
> (#106), which derives a title correctly but pays the full cost every time. The addition is an
> **opaque, adapter-owned memo** the caller stores and hands back, so the second derivation costs
> almost nothing.
>
> Measured on 18 real transcripts (714 MB): **1.707 ms → 3.8 µs per session, 446×**, all 18
> answering `Unchanged`. §8 Q2 is settled — `Unchanged` carries a **required** memo.
>
> Driven by #98 (`claude-monitor`), which re-derives titles for every session on the machine on a
> ~2 s cadence — but the interface belongs to `claude-replay` and every frontend benefits.

---

## 1. The problem, measured

`session_card` today reads a bounded 256 KiB tail and parses its lines, every call. Measured on
this machine (release build, warm page cache, 18 Claude transcripts totalling 714 MB):

| | cost |
|---|---|
| **`session_card` today** | **0.96 ms / session** |
| — of which, JSON parsing the tail | 0.15 ms |
| — the rest is the 256 KiB read | ~0.8 ms |
| **one `stat` (the "did anything change" question)** | **1.3 µs / session** |

Two things follow:

- The cost is **flat in transcript size** (the tail is bounded) but **linear in session count**.
  100 sessions ≈ 96 ms per refresh; 1000 ≈ 0.96 s. On a 2 s cadence that is ~5–50% of a core,
  spent almost entirely on sessions where nothing happened.
- The floor is **three orders of magnitude lower**. Nearly all of the work is re-reading bytes
  that were already read and re-deriving a value that did not change.

The fix is not a smaller window or a faster parser. It is **not doing the work again**.

## 2. Why the caller cannot solve this itself

The obvious move — the caller stats the transcript and skips the adapter when `(len, mtime)` are
unchanged — is **wrong**, and the investigation into QoderWork is what proves it:

> QoderWork keeps its session titles in SQLite —
> `~/Library/Application Support/QoderWork/data/agents.db`, `sub_chats.name`, joined on
> `session_id` = the transcript stem. **A QoderWork title can change while the transcript is
> untouched**, because renaming a chat writes the database, not the log.

A framework-level mtime cache would pin such a session's title forever. Symmetrically, a
framework that always re-derived would pay Claude's 0.96 ms for nothing.

**Only the adapter knows what its answer depends on.** So the staleness decision has to move to
the adapter — and to decide cheaply, the adapter needs somewhere to keep what it learned last
time. That is the memo.

## 3. The interface

```rust
/// What an agent calls a session (unchanged from v1.40.0).
pub struct SessionCard {
    pub title: Option<String>,
    pub last_prompt: Option<String>,
}

/// **Opaque, adapter-owned, JSON.** Whatever this adapter needs to answer faster next time — a
/// byte offset it scanned to, a row version, a chat id it already resolved. The caller stores it
/// and hands it back; it never looks inside.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CardMemo(serde_json::Value);

/// The three answers, distinguished because a caller cannot tell them apart otherwise.
pub enum CardOutcome {
    /// Nothing this adapter depends on has changed — **keep the card you already have.**
    ///
    /// The memo is REQUIRED, not optional: a cursor advances even when the answer does not
    /// (Claude's scan offset moves with every append), so a caller free to drop it would
    /// silently restart from a stale position every call and undo the memoization.
    Unchanged { memo: CardMemo },
    /// A card, plus the memo for next time.
    Fresh { card: SessionCard, memo: Option<CardMemo> },
    /// This agent names nothing here — **drop any card you cached.**
    Absent,
}

pub trait TranscriptAdapter {
    /// What this agent calls the session at `path`.
    ///
    /// `memo` is whatever this adapter returned last time for this same path, or `None` on a
    /// first call, after a cache eviction, or when the stored memo could not be read.
    ///
    /// Same class as `load_tasks`: takes a path, may do I/O, and **the fold never calls it**.
    fn session_card(&self, _path: &Path, _memo: Option<&CardMemo>) -> CardOutcome {
        CardOutcome::Absent
    }

    /// Many at once, for adapters where the per-call setup dominates — a database open, a
    /// directory scan. Provided: the obvious loop. Mirrors `subagent_sources`, which exists for
    /// exactly this reason.
    fn session_cards(&self, items: &[(&Path, Option<&CardMemo>)]) -> Vec<CardOutcome> {
        items
            .iter()
            .map(|(p, m)| self.session_card(p, *m))
            .collect()
    }
}
```

### 3.1 Why three outcomes and not `Option<SessionCard>`

Because two of the cases are indistinguishable otherwise, and confusing them is a visible bug:

| the adapter means | with `Option` the caller sees | what would go wrong |
|---|---|---|
| "unchanged, keep yours" | `None` | the title **disappears** on the next poll |
| "nothing here, forget it" | `None` | a deleted title **lingers forever** |

`Unchanged` also lets the adapter answer without constructing anything — which is the whole
point, since it is the common case.

### 3.2 Why an opaque JSON blob is right here, having been rejected in #96

#96 rejected opaque `serde_json::Value` payloads in the meta record, and the rejection said why:

> *inherited from a **trait** seam, where the trait cannot name every impl's state; a file format
> is not that situation.*

This **is** that situation. `TranscriptAdapter` cannot name what a future adapter needs to memoize
— a byte offset, a row version, a database path, an ETag — and the caller has no use for it.

The distinguishing property is not the encoding but the **discardability**: the memo is a *cache*
its owner may throw away at any moment, whereas the meta record is a *format* readers depend on.
Which gives the contract's hardest rule:

> **A memo must always be optional.** An adapter must treat a missing, unreadable, foreign, or
> stale-format memo exactly as `None` — fall back to the cold path. It must never error, and it
> must never trust a memo it does not recognise. An adapter that changes its memo format bumps a
> version *inside its own JSON* and ignores anything else; the framework does not police this,
> because it cannot.

## 4. The contract

**The caller must:**
1. hand back the memo it received, unmodified, for the **same path and same agent**;
2. never interpret it;
3. cope with losing it (first run, eviction, corruption) — that is a cost, never an error;
4. on `Absent`, drop the cached card **and** the memo.

**The adapter must:**
1. be correct with `memo: None` — the memo is an optimisation, never a requirement;
2. be correct with a *wrong* memo — stale, truncated, or from another version;
3. return `Unchanged` only when it has actually checked something, not by assumption;
4. keep the memo small. It is stored per session and read on every refresh.

**Neither may:** use the memo to carry the card itself. The caller already has the card; a memo
that duplicates it doubles the store and invites the two to disagree.

## 5. Worked examples

### 5.1 Claude — an incremental tail scan

```jsonc
// memo
{ "v": 1, "at": 41_238_912, "title": "…", "last_prompt": "…" }
```

| situation | check | work |
|---|---|---|
| nothing appended (`len == at`) | one `stat` | **`Unchanged`** — 1.3 µs |
| grew (`len > at`) | scan `[at, len)` only | proportional to the *append*, not the file |
| shrank (`len < at`) | compaction or a different file | cold path: rescan the 256 KiB tail |
| no memo | — | cold path (today's behaviour) |

The incremental scan keeps the previous title when the new bytes contain no title line, which is
the normal case — Claude re-stamps `ai-title`/`last-prompt` in pairs, so an append that contains
neither leaves both standing.

This is #96's resume principle at a much smaller scale, and the same two failure modes: the
partition must be a byte offset the adapter itself produced, and a shrunk file means *rebuild*,
never *trust*.

### 5.2 QoderWork — a batched database lookup

```jsonc
// memo
{ "v": 1, "sub_chat_updated_at": 1785062938883 }
```

The transcript is irrelevant here; the title lives in `sub_chats`. Per-session cost is dominated
by **opening the database**, which a memo cannot fix — so QoderWork overrides `session_cards`:
one read-only open (`file:…?mode=ro`, so a running QoderWork is undisturbed), one
`WHERE session_id IN (…)`, then per row compare `updated_at` against the memo and answer
`Unchanged` or `Fresh`.

That turns N opens into one, which is the difference the batch method exists to buy.

### 5.3 Codex — nothing to do

No title anywhere; the default `Absent` costs nothing and needs no memo. An agent opts in by
implementing one method, and pays nothing until it does.

## 6. Where the memo is stored

The monitor keeps it beside the card, in its own `cards.json` (#98 §4.1) — one JSON object per
session:

```jsonc
{ "<session-id>": { "title": "…", "last_prompt": "…", "memo": { … }, "derived_at": … } }
```

Discardable by construction: deleting `cards.json` costs one cold derivation per session and
nothing else. The TUI and HTML do **not** store a memo today — they derive a title once per open,
where 0.96 ms is invisible. The interface does not require them to.

## 7. What this changes

| | |
|---|---|
| `SessionCard` | unchanged |
| `session_card` | gains a `memo` parameter, returns `CardOutcome` instead of `Option<SessionCard>` |
| `session_cards` | **new**, provided default |
| `Transcript::card()` | keeps today's simple shape — `Option<SessionCard>`, no memo — so the two existing frontends do not change at all |
| Claude adapter | gains the incremental path |
| QoderWork adapter | gains its first implementation (SQLite; see #98's investigation) |
| Codex adapter | unchanged (default) |

The v1.40.0 signature was shipped four commits ago and has two in-tree callers, both of which
route through `Transcript::card()` — so the breaking part is contained to the facade.

## 8. Open questions

1. **Should `Transcript::card()` also expose the memo form?** Keeping the simple one is what
   spares the frontends; but a future frontend that polls (a live TUI header?) would want the
   memo. Adding `card_memo()` later is additive, so this can wait — but not adding it now means
   the monitor calls the adapter through a lower-level path than the frontends do, which is a
   small asymmetry worth naming.
2. ~~Does `Unchanged` need to carry a memo?~~ **Settled: required.** A caller free to drop it
   would silently restart Claude's scan from a stale offset on every call.
3. **A cheap/expensive budget hint.** The index refresh wants cheap; the sweep can afford
   expensive. Today the memo makes the common case cheap enough that a hint looks like premature
   generality — but if an adapter's cold path is ever *very* expensive (a network call), the
   caller has no way to say "not now".
4. **Batch granularity.** `session_cards` takes a flat list. QoderWork would rather receive them
   grouped by store, and derives that itself from the paths. Fine at these sizes; worth revisiting
   if an adapter ever needs an index over the batch.
5. **Should the framework version the memo envelope?** Today each adapter versions its own JSON.
   A framework-level `{adapter, version, payload}` wrapper would make skew impossible to get
   wrong, at the cost of a concept every adapter must understand. Leaning no — the discard rule
   makes the failure mode harmless.

## 9. Status

| | |
|---|---|
| `CardMemo` / `CardOutcome` / the two hooks | **built** |
| Claude's incremental scan | **built** — 16 tests, including `incremental_equals_cold` |
| `Transcript::card()` unchanged for frontends | **built** — the TUI and HTML did not change |
| `session_card_memo` / `session_cards` on the facade | **built** |
| **QoderWork's SQLite implementation** | **not built** — needs a dependency decision (§9.1) |

### 9.1 The QoderWork dependency question

Its titles live in `~/Library/Application Support/QoderWork/data/agents.db`
(`sub_chats.name`, joined on `session_id` = the transcript stem). Reading that needs SQLite, and
`claude-replay-agents` has no database dependency today. Three options, none obviously right:

- **`rusqlite`** — correct and safe, but pulls a C library into a workspace that currently builds
  with pure Rust, affecting build time and cross-compilation. Biggest hammer; also the only one
  that reads WAL correctly.
- **Shell out to `sqlite3`** — no dependency, but a runtime requirement and a parsing surface;
  fragile in exactly the way this codebase avoids elsewhere.
- **Defer** — QoderWork sessions fall back to `Candidate::snippet`, exactly as Codex does, and the
  interface is ready when the answer is.

Leaning `rusqlite` behind an **optional feature**, off by default, so the cost lands only on a
build that wants QoderWork titles. Wants a decision rather than a guess.

## Rejected

| shape | why |
|---|---|
| Caller-side `(len, mtime)` cache, no memo | wrong for QoderWork, whose title changes without the transcript changing (§2) |
| Always re-derive (today) | 0.96 ms × every session × every refresh, almost all of it re-reading unchanged bytes |
| `Box<dyn Any>` memo | cannot be persisted, and the monitor's whole point is that the memo survives a restart |
| Memo carries the card | duplicates state the caller already holds and invites the two to disagree (§4) |
| A framework-wide title cache with per-agent TTLs | a TTL is a guess about staleness; the adapter *knows*, and the memo lets it say so cheaply |
| `Option<SessionCard>` return with the memo bolted on | cannot distinguish "unchanged" from "nothing here" — one makes titles vanish, the other makes them linger (§3.1) |
