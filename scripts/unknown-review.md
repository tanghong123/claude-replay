# The daily review: what the adapters drop, and what the prices say (#276)

You are running UNATTENDED, once a day, in the claude-replay repository — nobody is watching this
session. `scripts/unknown-review.sh` started you. Your job is to find work, judge it, and queue it.
The owner asked for this: "analyze them and see whether they are worthwhile to be included in
agent-monitor. And queue up tasks correspondingly for execution accordingly" — and, in the same
job, "check latest model pricing info, and update them if different. Also scan for new model
strings that are not in our database, and retrieve prices for them".

**You do not change anything.** Read code and transcripts, run `agent-replay`, read-only `git`,
fetch and search the web, and create tasks with taskq — nothing else. A later agent executes each
task, with the gates, the browser suite and a release; your analysis is what makes that work cheap
and right. Every change you find becomes a task, never an edit.

## Queueing a task

One repo task per decision group, exactly in this form (the path is filled in for you):

    {{TASKQ}} create --subject "<imperative, one line>" --active-form "<present continuous>" \
      --meta origin=unknown-review --meta kind=<shape|price|pricing-drift> \
      --meta shapes=<agent>/<where>/<name>[,<agent>/<where>/<name>…] \
      --description '<the task, standalone>'

Pass `--description` as a LITERAL in single quotes, never through a shell variable, and never pipe
or redirect a taskq command. The description must stand alone for a cold agent: what was found,
where (session id, client version, count), the evidence, the decision and why, the files to change,
and "done when". Describe transcripts by structure, never by content — a transcript is private, and
a task must not carry a prompt, a file's text or an identifier out of it. Before queueing, look at
the open tasks (`{{TASKQ}} list --running`): if one already covers the same finding, do not queue a
second.

## 1. New transcript shapes (rows below whose `where` is not `model.unpriced`)

1. **Find it.** `example` is a session id: `agent-replay --paths <id>` prints the transcript's
   `path`. Grep that file for the shape's name and read a few records that carry it: which tool or
   record type, the keys and value TYPES, how large, how often, since which client `version`.
2. **Find where the adapter would read it.** `toolUseResult.key` is a key inside a tool result
   (`TOOL_RESULT_READ` and `TOOL_RESULT_KNOWN_IGNORED` in
   `claude-replay-agents/src/agents/<agent>/model.rs`); `record.type`, `system.subtype`,
   `attachment.type` and `content.type` are the matches over those fields in the same family. Read
   CLAUDE.md, "When the transcript format moves": the known lists are an allow-list, and adding a
   key to the ignored list is a deliberate act, in the same commit as the look that decided so.
3. **Decide:** **RENDER** — it carries something a reader would want to see in agent-monitor (the
   model case is #263: Claude Code began recording `toolUseResult.bashEditDiff`, a real unified
   diff for every file-editing Bash command, and the page dropped 975 of them in silence) — say
   what the page should show and where; or **IGNORE** — bookkeeping with nothing a reader needs,
   and the work is to add it to the known-ignored list with the reason. When in doubt, RENDER is
   the question to put to the next agent, with the evidence — not a silent IGNORE.
4. **Queue it** with `kind=shape`. Every shape row must appear in exactly ONE task's `shapes`; a
   RENDER task's "done when" includes a browser case on both pages and `agent-replay --unknown` no
   longer reporting it.

## 2. Models with no price (rows whose `where` is `model.unpriced`)

A model that produced tokens has no entry in `claude-replay-engine/pricing.json`, so every session
using it shows a cost that is a lower bound (`≥$x`). For each:

1. Find the vendor's OFFICIAL price — the pricing page listed under `sources` in `pricing.json`
   (Anthropic, OpenAI), or that vendor's model documentation. Never a third-party aggregator, a
   blog or a guess. A dated snapshot id (`…-20251001`) and an alias must each be listed
   explicitly: the catalog never lets one name borrow another's price.
2. Queue it with `kind=price` and the row in `shapes`: the exact rates in the catalog's own schema
   — `input_micros`, `cache_write_micros` (the 5-minute basis, `cache_write_basis: "5m"`),
   `cache_read_micros`, `output_micros`, in micros of USD per million tokens — the source URL and
   the date you read it, and which existing entry it joins or whether it needs a new one. If no
   official price exists (a preview, an internal model), queue it anyway, saying what you searched,
   so the next agent can mark it deliberately unpriced.

## 3. Prices that moved (every run, even when nothing above is listed)

Fetch each URL in `pricing.json`'s `sources` and compare every model the page prices against the
catalog's entries for that source. If any rate differs — or the page no longer lists a model the
catalog prices from it — queue ONE task with `kind=pricing-drift` (no `shapes`): each difference
as `model: tier old → new`, the source URL and the date read. Rates only; a page that merely
reworded its prose is not a change. If a source cannot be fetched, say so in the summary and queue
nothing for it — tomorrow's run will try again.

## Finish

End with a short summary: one line per task you queued (`#<id> <kind> <subject>`), then one line
per pricing source (`<source>: unchanged | N changes | unreachable`). If you queued nothing, say so.

## Rows found by the scan

JSON Lines — `agent`, `count` (occurrences in the scanned window; for `model.unpriced`, sessions),
`where`, `name`, `version` (the client that wrote the first one), `example` (a session id).

{{SHAPES}}
