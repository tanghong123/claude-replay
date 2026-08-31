# BACKLOG — moved to the task queue

> **The state of record for pending work is now `tasks/` — the taskq queue** (owner,
> 2026-08-30). This file is a pointer, kept so that links to it still land somewhere useful.

```sh
taskq list            # in_progress + recent pending + recent finished
taskq list --ready    # what is actually claimable now
taskq list --all      # includes the deferred/parked items
taskq get <id>        # one task in full
```

The `taskq` CLI ships with the `agentdev:taskq` skill; every mutation goes through it, never
through an editor — the CLI is what makes concurrent agents safe (one flock + check-and-set)
and what journals each change to `tasks/journal.ndjson`. Reading `tasks/*.json` directly is
fine.

## Why the move

This file existed because status scattered across 30+ design docs let an agreed refactor
(#167) slip out of view. The queue keeps that property and adds the ones a Markdown list
cannot have: a claim is atomic, so two agents cannot start the same item; `--next` picks work
without a human arbitrating; a `pass` records *why* someone declined an item instead of
silently skipping it; and the mutation journal makes the history of a decision readable after
the fact. It is also shared — the same queue is visible to whichever agent product opens the
repo next.

Everything this file listed on 2026-08-30 was migrated verbatim; run `taskq list --all` to
see it. The one entry that was mostly *record* rather than pending work — the Monitor v2
narrative — became [design/monitor-v2.md](design/monitor-v2.md), with only its two live
threads filed as tasks.

## The division of labour is unchanged

Design docs under `design/` carry the **arguments**; GitHub issues carry the **discussion**;
the queue carries the **state** — what is waiting, on whom, and why. Don't trust a
`design/*.md` status header alone; those drift, which is what the tracker is for.

**Update the queue in the same commit that changes an item's state** (started, decided,
shipped, parked) — the same discipline this file asked for, and the reason `tasks/` is
committed with the repo rather than kept aside. Parking is `--meta deferred=true --meta
deferred_reason="…"`, which hides an item from `list` and from `--next` while leaving it
explicitly claimable; that is the queue's equivalent of this file's old "Parked" section.
