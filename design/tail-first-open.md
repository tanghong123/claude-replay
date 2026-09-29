# Opening a session tail-first (#314)

**Status: proposal — the owner decides before any engine code.** Filed by the agent, not the owner,
while measuring #313; it adds one seam to the viewport engine (`design/virtual-window-framework.md`),
and after #313's gzip what it buys is modest. This note is the question: build it (option A below),
build the smaller option B, or leave it.

## 1. What it would fix, and what it would not

Opening a session downloads **every committed record it has** in one `/records` reply before
anything renders. Measured on 2026-09-29 against a monitor with the sessions warm:

| transcript | `/records` reply | gzipped at level 6 |
|---|---|---|
| 8 MB | 3.0 MB | 0.8 MB |
| 70 MB | 23.6 MB | 10.1 MB |
| 107 MB | 32.3 MB | 8.8 MB |

(The server gzips at level 3, measured on the 32.3 MB reply at 9.1 MB in 256 ms: a few percent
larger than level 6 for half the CPU.)

The page itself renders a 23 MB reply in about 0.9 s at a 4× CPU throttle; the time a phone spends
is the transfer. Since #313 a client across a network gets the reply gzipped (a quarter to 43 % of
the bytes) and a veil that says how much has arrived. Tail-first would render the END of the
session — where every open lands — from its last few hundred KB, and fetch the rest behind it.

**Not a goal: the first open of a session the monitor has never read.** `/pull` does not answer
until the server has folded the transcript (1–8 s measured; 86 s for 400 MB), and no committed
record exists before that. Tail-first starts after the fold, so it does nothing for "takes a while"
on a first visit. What remains for it: a warm open of a large session over a slow link — a few
seconds, after gzip, on the largest sessions.

## 2. The protocol: one query parameter, nothing new in the reply

A first pull (a fresh cursor) may carry `tail=<bytes>`. The server honours it only when the
committed log is larger than about twice that budget (a small session is sent whole, so it never
shows a pending unit), and then picks the committed index `k` whose record starts at or after
`log_len − bytes`. The reply is an ordinary pull reply:

- `committed_from = k` and `committed_ext = {offset: offset(k), len: log_len − offset(k)}`.

Everything the client needs is already there: the HEAD is `[0, committed_ext.offset)` of the log and
holds `k` records. The shared reducer (`shared/record-stream.js` `reducePull`) needs no change: its
plan truncates the (empty) array to `k` and appends the tail behind it, leaving `k` empty slots,
which the store fills with placeholders after applying the plan. So the reducer, and the classic
page that shares it, stay byte-identical; the classic page opts out simply by never sending `tail`.

The head is then read in the background with the existing `/records?from=0&len=offset(k)&epoch=E`
(streamed, with progress, as #313's veil reads a first open). A 409 on it, or a head whose record
count is not `k`, is a resync — never a splice.

## 3. The page (app shell only)

- **The placeholders become ONE unit.** The projection maps the run of placeholders `[0, k)` to a
  single `pending` unit (key `pending:head`), extended over any leading non-user records of the
  tail, so the first real unit on screen is a user turn with its own `record.turn` — turn numbers
  come from the records (`buildUnits` reads `record.turn`), so the sticky bar is true in phase 1.
- **Its height** is `k × the applied estimate`, so `prefix[count]` barely moves when the head lands;
  and it has its own `kindOf`, which the estimator never learns from (it would be a sample of the
  wrong shape). It draws "Loading earlier turns — 42 %".
- **Search, the outline and the filters** work on what is loaded: in phase 1 the Turns drop-down
  and the outline list the tail's turns, and they gain the head's when it lands. Stated, not hidden.

## 4. The engine: the head landing is a PREPEND

When the head lands, the placeholders are replaced in place — record indices do not move — and the
projection rebuilds from 0: one `pending` unit becomes `m` units. The ENGINE's items are units, so
every tail unit's INDEX shifts by `m − 1` while its IDENTITY (the key, built from the server's record
id) stays the same.

Today that reads as the #165 case: the anchor's key is found at another index, which the engine
takes to mean "this id now names a different record" and falls back to the model form captured
before the change (I12) — which after a prepend is `m − 1` units wrong. So:

- **Option A (recommended): `recordsChanged` learns a shift.** The page's `mutate` may return
  `{ from, shift }`; the transaction translates `P0` — the anchor's index, its fallback's index, a
  model index — by `shift` before `rangeFor` and `place`. The reader did not move; the index space
  did. I12 then passes on its own terms (the translated index names the same key), I1 holds (no one
  but the reader sets `P`; a translation is not a move), I11 (`rangeFor` ranges around the
  translated index) and I7 (the placement lands in the same task) as they do for any records change.
  The history's `pendingDelta` records the shift, or a saved export would replay the prepend as a
  rewrite of everything; the sandbox kit gains a prepend builder so a bug here reproduces from a
  synthetic transcript, as every viewport bug must.
- **Option B (no engine change):** apply the head as an ordinary rewrite from 0, then have the page
  re-place by key with `jumpTo` (`rangeFor` already resolves a target's key). Two writes in one task,
  no visible flicker — but the history records a `jump` the reader never made, and the first write
  lands on the (wrong) fallback.

Either way the common case is untouched: a fresh open lands at the tail (`P = tail`), and a
following reader stays at the end through the prepend, with `following` unchanged (I13).

## 5. Cases (app shell, a throttled link, the check mode on — `window.__viewportViolations` empty)

1. The tail is on screen before the head has arrived, and its sticky-bar turn number is the true one.
2. A reader at the tail is still at the tail after the head lands, still following.
3. A reader scrolled up inside the tail keeps the same record at the same offset, ±1 px, across the
   landing.
4. The outline and the Turns drop-down gain the head's turns when it lands.
5. A head that comes back with the wrong record count, or a 409, resyncs instead of splicing.
6. A sandbox replay of a saved prepend history.

The classic page is unchanged and held by its existing scenarios.

## 6. The question

Build A, build B, or leave it at #313's gzip and veil. The agent's recommendation is A if the phone
still waits noticeably on warm opens of large sessions after 1.326.0, and to leave it otherwise.
