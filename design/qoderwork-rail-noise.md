# Study #149 — why the QoderWork group has 68 rows when the app shows 11

**The premise "lots of very short sessions" is not what the data says, and the fix is not a
size floor.** Only 1 of the 68 transcripts is under 16 KB; 35 have more than 20 assistant
replies. Raising `MIN_TRANSCRIPT_BYTES` would delete real work and barely dent the count.

**The 68 are dominated by machine-generated maintenance runs, not sessions anyone had.**

## What the 68 actually are

Census of every transcript the monitor shows (`>= 4096 B`, non-mount slug):

| cohort | n | median user turns | median replies | median size |
|---|---|---|---|---|
| cwd = `$HOME` | **31** | 6 | 9 | 84 KB |
| cwd = a QoderWork workspace | 37 | **70** | **128** | **910 KB** |

An order of magnitude apart on every axis. And the `$HOME` cohort is not a mixed bag — it is
three stereotyped prompts:

| n | opening prompt |
|---|---|
| 24 | `Target file this round: MEMORY.md  Current usage: …` |
| 6 | `Target file this round: USER.md  Current usage: …` |
| 1 | `You are the Memory Reflection worker. Your only job is to reorganize MEMORY.md…` |

These are a memory-reflection worker running on a schedule — four of them are byte-identical
36.3 KB repeats of the same prompt. They are automation output that happens to be shaped like
a session. **30 of the 31 have no QoderWork sidecar at all**, i.e. QoderWork itself keeps no
per-session record of them.

## Why they all pile into one group

`QoderWorkAdapter::workspace_anchored()` returns `false`, so every QoderWork session — all 68 —
is filed under a single `qoderwork` desktop-agent group ("desktop agent · no workspace"). The
37 real ones are spread across **27 distinct workspace directories** and get no benefit from
that structure. Claude sessions, by contrast, land in per-project groups.

So the crowding is two independent faults compounding: 31 non-sessions are admitted, and the
remaining 37 are denied the grouping that would spread them out.

## What is NOT a reliable filter

- **Size.** Shown above: the distribution runs the wrong way.
- **Prompt text.** Matching `Target file this round:` would work today and is exactly the kind
  of rule that breaks silently when the user renames their own tooling. It also encodes one
  person's automation into a general viewer.
- **The sidecar.** It looked ideal — cheap, cross-platform, QoderWork's own record — until the
  owner mentioned deleting sessions from the UI. **Deleting a session removes its database row
  and leaves BOTH the transcript and the sidecar on disk**: 11 orphaned transcripts still carry
  a `-session.json`. A sidecar therefore proves a session once existed, not that it still does,
  and requiring one would keep showing exactly the sessions the user deliberately deleted.
- **Archival or recency in the database.** `chats` holds 30 rows, all in one project, **none
  archived**, all updated within 7 days. Neither explains the app's 11, and I did not
  reverse-engineer its exact rule — guessing at another app's list logic is a bug waiting to
  happen. (The owner's "okay to have a few more" makes matching 11 exactly unnecessary anyway.)

## Where the 68 come from, exactly

Reconciling the store against QoderWork's own database settles it:

| | n |
|---|---|
| session ids the database knows | 30 |
| transcripts on disk | 75 |
| transcripts the monitor shows | 68 |
| — shown **and** in the database (live) | **27** |
| — shown but orphaned: `$HOME` automated workers | **31** |
| — shown but orphaned: in a workspace dir → **UI-deleted** | **10** |

So the rail is showing 27 live sessions, 31 pieces of automation output, and 10 sessions the
user explicitly deleted. Two different problems with two different fixes.

## Recommendation

**1. Exclude sessions whose cwd is `$HOME` (structural, no text matching).** QoderWork's real
sessions live under `~/.qoderwork/workspace/<id>`; a session rooted at `$HOME` is by
construction not in a workspace. This removes all 31 — the whole automated-worker cohort — on
a property of where the session ran, not what it said. **68 → 37.**

**2. Group QoderWork by workspace.** The 37 survivors span 27 directories. Either make the
adapter workspace-anchored or group on the sidecar's `working_dir`. Combined with #142's fork
families (which collapse a 10-session family to one row), the largest remaining group falls to
a handful. **37 → ~27 groups, biggest ≈ 2 rows.**

**3. Make it a filter, not a deletion.** Same contract as #113's hide list: excluded rows stay
reachable behind a toggle, because "automated worker output" is a judgement and the user may
want to inspect a run. A count ("31 hidden") keeps the omission honest rather than silent.

**4. Deleted sessions need the database — and only the database.** The 10 UI-deleted rows have
no file-level tell: transcript and sidecar both survive deletion. Excluding them means checking
`sub_chats.session_id` in the SQLite reader (compiled in on macOS via `cfg(target_os = "macos")`;
it was a `qoderwork-titles` feature that #143 deliberately stopped requiring, until QoderWork
dropped the sidecar and the gate was removed). Reasonable shape: on macOS, treat "transcript
present, database row absent" as deleted and filter it; elsewhere, show them. It is 10 rows out
of 68, so it is a refinement, not the fix.

Steps 1 and 2 are independent and can ship separately; 1 is the large win and the simpler
change. Together they take the group from 68 rows in one pile to ~37 spread across 27 groups;
adding 4 reaches the 27 live sessions.

## Caveat worth recording

The chat titles in QoderWork's store are **real work content** — this store's include
individuals' names in a recruitment context. They already flow into the rail as session titles
via #143. That is correct for a local, loopback, single-user tool, but it means QoderWork
titles must never be pasted into shared artifacts, issues, or test fixtures. The repo was
sanitized of exactly this class of content in `478cbd0`; the store itself is full of it.
