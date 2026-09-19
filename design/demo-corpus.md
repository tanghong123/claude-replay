# The demo corpus (#248)

`scripts/demo-corpus.mjs` generates a sanitised session that exercises the viewer's features,
and replays it live so the follower can be filmed following.

## Why a generator rather than a transcript

Anything under `design/` is public, and `.gitignore` catches `*.jsonl` but cannot catch a demo
page that embeds one — a page carrying a real prompt timeline reached review that way once.

So the corpus is **derived**: the script is the committed artefact, every word in it is
invented, and a reviewer confirms the corpus is safe by **reading the script** rather than by
auditing generated output. The generated `.jsonl` is scratch and is never committed.

The SHAPE comes from real sessions — turn lengths, how often a turn thinks, the ratio of calls
to prose, how deep result bodies run. The CONTENT is a fictional project: `lumen`, a link
checker for static sites. The only hosts are `example.invalid` (RFC 2606's reserved TLD) and
`localhost`; the only paths are under `/home/dev/lumen`.

## Running it

```
node scripts/demo-corpus.mjs --out <dir>                    # the whole session at once
node scripts/demo-corpus.mjs --out <dir> --live             # append it on a timer
node scripts/demo-corpus.mjs --out <dir> --live --speed 4   # …four times faster
```

`--out` gets `<sid>.jsonl` plus `<sid>/subagents/workflows/<run>/journal.jsonl`, because a
workflow's roster lives beside the session rather than inside it (#241).

`--live` is the half the video needs. The page follows a growing file, so a recording needs a
writer that appends while it runs. Each scene waits its own transcript span divided by
`--speed`, so the replay keeps the session's rhythm instead of a fixed tick. Under `--live` the
timestamps are recent, because a fixture stamped in the past sorts into the Idle bucket and the
app shell's default filter hides it (#202).

## What the fourteen scenes cover

| scene | shows |
| --- | --- |
| a prose answer | markdown table, fenced code, CJK |
| a run of calls | activity coalescing, and a failure pill naming the failure (#234) |
| an Edit and a Write | diffs, and a numbered body |
| a Read with offset+limit | the gutter as a claim about a file |
| a screenshot mid-run | an image attachment where the tool took it (#228/#256) |
| `/context` | the report, parsed rather than shown as terminal art (#235) |
| an API error and a hook | both failure kinds (#236) |
| a question to the reader | the ask card with every question and option (#237/#255) |
| a queued prompt picked up | the rewrite that is not append-only (#165) |
| a compaction | the boundary, and the files it carried over (#254) |
| tasks | the task panel, one task opened and closed |
| a sub-agent | spawn and finish |
| a workflow run | members grouped by phase (#241) |
| cost, and a prompt still waiting | the client's own figure beside ours (#240), and a live `⧗` marker |

Turn durations (#257) ride most turns, so the chip appears throughout.

## Verifying it

Render it and read the features off the wire:

```
node scripts/demo-corpus.mjs --out /tmp/demo
agent-replay /tmp/demo/*.jsonl --dump-html /tmp/demo/page
```

The last check before recording is the one the corpus exists for: every scene in the table above
should be visible on the page. A scene that renders nothing is usually a record shape that moved
— the `api_error` record, for instance, carries an `error` OBJECT and ignores a `content`
string, which is exactly the kind of mistake that shows up as a silently missing scene.

## Still to do

The video itself. The owner asked to review the corpus and the driver first, and scene order and
length are their call rather than a detail to guess.
