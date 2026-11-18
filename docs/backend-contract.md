# agent-monitor's backend contract

Contract version: 1

**What this is for.** agent-monitor's page is built from two parts: the session list (the left pane,
with its filter) and the session view (the right pane: the transcript, its outline and artifact
panes, and the compose box). Both read everything they show from a set of HTTP routes. This document
is that set: every route, what it takes, what it answers, and the record format the transcript
arrives in. A host that wants to show the same parts serves these routes, or relays them from an
agent-monitor beside its sessions (`design/embedding.md`, "The backend contract").

Today one server answers it: agent-monitor's own (`claude-monitor/src/routes.rs`, the route table).
Where this document and that server disagree, the server is right and this document has a bug.

## The version and its rule

The contract has one number, `CONTRACT_VERSION` in `claude-monitor/src/routes.rs`. Version 1 is the
contract as it was first written down, in November 2026; the routes themselves are older.

- **A change that only adds keeps the version.** A new route, a new field in a reply or a record, a
  new record kind, a new body part, or a new value in a set this document calls open. A reader
  ignores what it does not know.
- **A change that removes, renames, or changes what an answer means moves the version.** That
  includes a field changing type, a status code meaning something else, or a route moving.
- **The page can read the version its server speaks.** The monitor's page carries it as
  `<body data-contract="1">`. Step 4 of `design/embedding.md` has each backend's handle carry it
  instead, so one page can hold backends that speak different versions.

## The route index

The tests read this table: every route the page calls must match a row, and every row must be one
the route table answers. A path that ends in `/` is a prefix: the row covers everything under it.
`<mdrev>` stands for the pinned mdrev release, and `<mermaid>` for the pinned Mermaid runtime's path.

The tiers say what a host must serve:

- **core**: the parts show nothing without it;
- **optional**: a feature goes away (each section says which);
- **write**: sending, granting and answering; a host that serves none of these says it is not paired;
- **page**: the detached tabs, which `design/embedding.md` moves to a later kit;
- **native**: Codex native mode, which is outside contract 1 (see "Codex native mode").

<!-- route-index -->
| Method | Path | Tier | Probe | What for |
|---|---|---|---|---|
| GET | `/api/sessions` | core | `/api/sessions` | the session list |
| GET | `/api/ignore` | optional | `/api/ignore` | hide and restore a session, project or agent |
| GET | `/api/layout` | optional | `/api/layout` | another app's arrangement of the list |
| GET | `/pull` | core | `/pull` | one session's feed: what changed since the reader's cursor |
| GET | `/records` | core | `/records` | a byte range of a session's record log |
| GET | `/turn-index` | optional | `/turn-index` | the map of a session's turns, for a long session over a slow link |
| GET | `/search` | optional | `/search` | search in the records the reader has not read yet |
| GET | `/file` | optional | `/file` | a file the transcript offers or hands over |
| GET | `/__reveal` | optional | `/__reveal` | show that file in the server machine's file manager |
| POST | `/api/send` | write | `/api/send` | send a prompt to a session |
| POST | `/api/consent` | write | `/api/consent` | grant or revoke sending into a live pane |
| GET | `/api/dialog` | write | `/api/dialog` | read the dialog a live session is waiting on |
| POST | `/api/answer` | write | `/api/answer` | answer that dialog |
| GET | `/monitor-ui/` | core | `/monitor-ui/app.js` | the page's modules and stylesheets |
| GET | `/mdrev/<mdrev>/` | optional | `/mdrev/<mdrev>/mdrev.js` | mdrev's viewer, for Markdown and code in the artifact pane |
| ANY | `/api/mdrev/` | optional | `/api/mdrev/hold` | mdrev's routes for the pane |
| GET | `/mermaid/` | optional | `<mermaid>` | the Mermaid runtime, for diagrams in the transcript |
| GET | `/image` | page | `/image` | an image in a tab of its own |
| GET | `/markdown` | page | `/markdown` | a Markdown document in a tab of its own |
| ANY | `/api/mdrev-review/` | page | `/api/mdrev-review/review` | mdrev's routes for that tab, with shared review |
| ANY | `/api/codex/` | native | `/api/codex/status` | Codex native mode's gateway |
| GET | `/codex-workspace/` | native | `/codex-workspace/api.js` | Codex native mode's modules |

## How a request is made

- **Paths are relative to the backend's base.** The page joins every path to its backend handle's
  base (`codex-ui/backend.js`): the monitor's own origin, a prefix a host chose, another origin,
  or a native app's own scheme. Nothing in this document assumes the routes sit at the root:
  the Mermaid runtime's path, which the page head names, is joined to the base too.
- **Credentials.** The page sends its cookies with every request (`credentials: "same-origin"` by
  default; a handle may say otherwise) and asks for `cache: "no-store"` on almost every read.
- **Pairing belongs to the listener, not to the routes.** The monitor's listener checks every
  request before a route sees it. A request it refuses gets 401: an HTML page load gets a page
  that reloads itself once and then says how to pair; anything else gets plain text. The page reads
  a 401 as "not paired". `/pair` and `/api/pair` are the listener's own and not part of this
  contract. A host puts its own sign-in in front instead. That sign-in must refuse a request
  another site started, whatever its method: `/api/ignore` and a stamped `/__reveal` act on a
  GET, and a request another site makes (an image, say) carries no `Origin` for the routes' own
  origin check to see. The monitor's `SameSite=Strict` cookie is what refuses it there.
- **Three facts per request.** Routes decide by what the listener tells them: whether the request is
  paired, whether its origin is the page's own (no foreign `Host` or `Origin`), and whether its
  replies must have secrets masked (a client across a network when the owner chose `remote`
  masking, every client when they chose `always`).
- **The write gate.** Every write, and the dialog read, checks in this order: 405 for the wrong
  method, 403 for a foreign origin, 401 when not paired.
- **A host serves a ceiling.** A route of an optional tier that a host does not serve answers 404
  (the monitor's table says `not served by this host`); reading the sessions and their records is
  always served. The monitor serves every route.
- **Bodies** are at most 64 KiB, except `/api/send` (12 MiB) and mdrev's `hold` (8 MiB plus 64
  KiB, one limit). A larger body gets 413, except from a client that is not paired, which gets
  401: the listener reads no more of it.
- **Compression and masking are transparent.** A client across a network may get a gzipped reply;
  `fetch` decodes it. A masked reply replaces each secret with `*` of the same length, so byte
  offsets and record boundaries do not move.

## The session list

### `GET /api/sessions`

The sessions to show, grouped by project. No parameters. The page polls it every 5 seconds.

The session routes answer for the ids this list shows. A reader need not ask for the list first:
a session route asked for an id the server has not seen scans as this route does (at most every
5 seconds) and then answers. An id the list would never show stays unknown.

Reply: `{groups: Group[], ignoredCount: number}`.

A `Group`:

| Field | Type | Meaning |
|---|---|---|
| `kind` | `"project"` or `"agent"` | a project, or a desktop agent with no workspace |
| `label` | string | the project's last path segment |
| `secondary` | string | the full path with `~`, or `"desktop agent · no workspace"` |
| `metaLine` | string | cost and count, shown as is (`"~$1.23 · 4"`, `"≥$… · N"`, `"unpriced · N"`, `"N"`) |
| `hasTerm` | bool | some row runs in a terminal the server can name (a pane or a screen session), whether or not a send may type into it |
| `growing`, `idle`, `total` | number | row counts |
| `ignoreKey` | string | the key that hides the whole group (`p:<path>` or `a:<label>`) |
| `hidden` | bool | the group is hidden |
| `cost` | number, optional | the known cost in US dollars |
| `costPartial` | `true`, optional | `cost` is a lower bound, or the usage is unpriced |
| `rows` | Row[] | the sessions |

A `Row` (the fields the page reads; the server sends a few more for other readers, listed after):

| Field | Type | Meaning |
|---|---|---|
| `id` | string | the session's id, the key every other route takes |
| `name` | string | its title |
| `agent` | string, open set | `claude`, `codex`, `qoderwork`, `qoder`, `qwenwork`, … |
| `state` | `"growing"`, `"idle"` or `"finished"` | whether its process runs and its transcript grows |
| `agentState` | `"busy"`, `"wait"` or `"idle"` | what the agent is doing |
| `stateReason` | string, open set | why: `thinking`, `tool`, `question`, `plan-approval`, `permission`, `queued-prompt`, `done`, `exited`, `exited-mid-work`, `ended-question`, `error`, `failed`, `starting`, `fleet`, `stalled`, and `idle` |
| `stateDetail` | string | a line for a person. Until the server's first verdict on a session it sends `exited`, `starting` or `idle` with the detail `state derivation is stabilizing` |
| `stateConfidence` | `"observed"` or `"inferred"` | |
| `stateSince` | RFC 3339 string or null | |
| `activityTs` | number, epoch seconds | the last activity |
| `activity` | string | its age for a person (`"3m"`, `"just now"`, `"—"`) |
| `startTs` | number, optional | the first event |
| `ignoreKey` | string | `s:<id>` |
| `hidden` | bool | hidden by its own key or its group's |
| `family` | string | the root of its fork family (the row's own id when it has none) |
| `isFork` | `true`, optional | |
| `sideOf` | string, optional | the session a desktop side session was started from |
| `startedBy` | string, optional | the session that started a headless worker; the page lists the worker under it, not in the list |
| `brief` | string, optional | a worker's first prompt line |
| `projActive` | `true`, optional | another live session runs in the project |
| `term` | string, optional, open set | `tmux`, `herdr`, `screen`, `tty`, `detached` |
| `target` | string, optional | the pane |
| `herdrSession` | string, optional | a named herdr session |
| `injectable` | `true`, optional | a send may type into the pane |
| `sendsImages` | `true`, optional | the pane takes images |
| `consented` | bool, optional | a standing grant covers the pane |
| `dialog` | `"question"`, `"permission"` or `"plan"`, optional | a dialog the page can answer |
| `cost`, `costPartial`, `costOwn`, `costSubs` | number or `true`, optional | the cost, its own and its sub-agents' |

Also sent, for the attention report and the fleet page rather than the parts: `conf`, `visited`,
`continuedIn`, `ambig`, `pid`, `pidSource`, `sock`, `turns`, `tools`, `subs`, `child`,
`costSubsPartial` and `stateSource`. They are part of the contract all the same: removing one
moves the version.

### `GET /api/ignore?add=<key>` or `?remove=<key>`

Hide or restore. The key is `s:<id>`, `p:<path>` or `a:<label>`, percent-encoded. Reply:
`{ok: true, ignored: <count>}`, or `{ok: false}` with no key. Always 200.

It is a GET because hiding is the reader's own preference kept beside the list, not a change to any
session. **Without it:** the hide and restore controls do nothing.

### `GET /api/layout` and `GET /api/layout?source=<name>`

Another app's own arrangement of the same sessions, for the list's Arrange choice. `source` is
`claude-app`, `codex-app` or `herdr` (an open set).

- Without `source`: `{sources: [{source, label, available, reason, lastError}]}`.
- With it: `{source, label, available, reason, note, sections, pending?, stale?}`, where a section
  is `{id, label, collapsed, entries, groups: [{id, label, collapsed, keep, entries}]}` and an entry
  is `{id: <session id>, title, pane: "<server>|<pane>" or null, children: Entry[]}`. The first ask
  may answer `pending: true` with no sections.
- 404 for a source it does not know.

**Without it:** the list offers only its own arrangements.

## One session's records

A session's transcript reaches the page as **records**: one JSON object per block the reader sees (a
prompt, a reply, a tool call, a compaction; "The record format" below). The server keeps them in a
**record log**, one record per line, and serves the log by byte range. Four routes read it, and
all take `session=<id>`. An id holding `/` or `..` gets 404.

**Two ways of counting, and the bridge between them.** `/pull`'s pointers and `/records` count
BYTES of the log. `/turn-index` and `/search`, and every record index in their replies, count
RECORDS. The turn index gives each turn both: its first record's index and its byte span. A host
that answers these routes itself needs a log it can address by byte with one record per line, or
offsets of its own that agree across all four routes.

### `GET /pull?session=<id>&cursor=<epoch>.<committed>.<gen>.<index>[&tail=<bytes>]`

What changed since the reader's cursor. The page asks every second.

**The cursor is the reader's.** It holds the log's epoch, how many committed records the reader
has, and the generation and index of the open turn. A fresh reader sends epoch 0, which never
matches, so its first pull gets everything.

**Two zones.** Records the transcript will not rewrite are **committed**; the turn still being
written is **provisional**. A reply is one of:

- `{t: "pull", ...}`, the envelope:

  | Field | Type | Meaning |
  |---|---|---|
  | `epoch` | number | the log's epoch; a new one means start again |
  | `committed_from` | record index | where the committed records this reply points at begin |
  | `committed_ext` | `{offset, len}` in bytes, or null | where they are in the log; the reader reads them with `/records` |
  | `provisional_gen` | number | the open turn's generation |
  | `provisional_from` | number | where the provisional records below begin, within that zone |
  | `provisional` | record[] | the open turn's records, inline |
  | `meta` | object or null | the session's facts ("The session's facts"); null when nothing changed |

- `{t: "redirect", url}`: another process owns this session; the page goes to `url`. A host need
  not send it.
- `{t: "error", message}` with 404: the session is unknown, its file is gone, or it cannot be
  served.

**How the reader applies a reply.** For each zone: cut what it holds back to that zone's `from`,
then append what came. The committed records come from reading `committed_ext` with `/records`.
The first index whose record may differ is the smaller of the two cuts; the server does not send
it.

**A record's id is its position, not its identity.** Ids are `b<n>`, and `t<n>` for a record that
opens turn `n`, counted as the records are written. When a queued prompt is picked up, its queue
marker goes and every marker behind it moves up one, so after a cut the same id can name a
different record. A reader may rely on a record's position and nothing else. Within one epoch the
committed zone only grows.

**The tail first.** On a fresh open the page sends `tail=524288`. The server honours it only on a
pull that would send everything (a fresh open or a resync), only when the committed log is more
than twice that, and only when some record but not every record starts inside its last `tail`
bytes; otherwise the reply is the usual one. When it is honoured, the reply points only at the
records from the first one starting inside those bytes: `committed_from` is that record's index,
above 0. The reader takes records 0 up to `committed_from` as owed
and draws a placeholder for them. Those records are the bytes from 0 up to `committed_ext.offset`,
which it reads with `/records` in ranges, under the same epoch. A 409, a count that differs, or a
record that does not parse starts the session over. The indices never move when they arrive.

Sub-agents: a record that starts a sub-agent names it (`head.child`, `head.child_id`), and its
records are pulled the same way under the sub-agent's id, which resolves once its parent has been
pulled.

### `GET /records?session=<id>&from=<byte>&len=<bytes>&epoch=<epoch>`

Whole records from the log, one per line. The reply's type says `application/json`, but its body is
JSON Lines: split it on newlines. An end inside a record is carried on to that record's end.

- 409 `stale epoch`: the log was made again since the pointer was given, or the server holds no
  log for this session now (it was never pulled, or it left the server's cache). The reader drops
  the reply and keeps its cursor, and its next pull starts again.
- 400 when `from` is not where a record starts: masking needs whole records.
- 404 for an empty id, or one holding `/` or `..`.

### `GET /turn-index?session=<id>&epoch=<epoch>&upto=<n>`

The map of the records below `upto` (the records the reader is still waiting for). The page asks
for it beside a tail-first open, and uses it only until the head has arrived.

Reply `{t: "turn-index", epoch, upto, records, bytes, turns, compactions, tools, artifacts, files, facts, agents}`:

| Field | Meaning |
|---|---|
| `upto` | as asked, at most the record count |
| `records`, `bytes` | the log's record count and length now |
| `turns` | every turn below `upto`: `{kind, id, turn, label, ts?, reply_context?, head?: {from, mode}, r, last, o, end}`. `r` is the opener's record index and `last` its turn's last record; `o` and `end` are the turn's byte span |
| `compactions` | `{r, head: {compact_trigger, compact_pre?, compact_post?}}` |
| `tools` | `{<tool name>: count}`, counted as the page counts, nested calls included |
| `artifacts` | `{url, name, icon, desc, at}`, `at` a record index |
| `files` | `{path, fsig, sig, r}`: the best offer of each path |
| `facts` | one `[kind, id]` per record below `upto`, `[kind, id, 1]` for an assistant's commentary: enough to group records into the units the view draws before they arrive |
| `agents` | `{id, r}`: the sub-agents started |

409 on a stale epoch, or when the server holds no log for the session now, as `/records`; 404
for an empty id, or one holding `/` or `..`. **Without it:** the page reads the head whole, from
the top, and a reader across a slow link waits for it.

### `GET /search?session=<id>&epoch=<epoch>&upto=<n>&q=<query>`

Search in the records below `upto`, for a reader whose earlier turns have not arrived. `q` is
URL-encoded JSON: `{lc, whole, wanted, tools}`, where `lc` is the lower-cased needle, `whole` asks
for whole words, `wanted` is a mask of the scopes (1 the person's turns, 2 replies, 4 thinking, 8
tools; 0 all), and `tools` is `[{name, prefix}]`. The page turns its own query box into this; the
box's grammar is the page's, not the server's.

Reply `{t: "search", epoch, upto, records: [[recordIndex, hits], ...], counts: {u, a, t, o}, total}`.
It quotes no text. An empty `lc` with `tools` is the tool filter: each record it keeps comes back
with 1 hit, and `counts` stay 0.

409 on a stale epoch, or when the server holds no log for the session now; 400 for a query it
cannot read; 503 when the log cannot be read; 404 for an empty id, or one holding `/` or `..`. **Without it:** the page searches what it holds and says "earlier turns not searched".

## Files

A record offers a path as `{path, sig?, fsig?}`: `sig` is a **reveal stamp**, `fsig` a **file
stamp**. The server mints both when it renders the record, and acts only on a path carrying the
stamp for that action. A host that answers `/file` must mint and check its own.

### `GET /file?path=<path>&sig=<fsig>`

| Status | What |
|---|---|
| 200 `image/png`, `jpeg`, `gif`, `webp`, `bmp`, `x-icon` or `avif` | a raster image |
| 200 `text/plain; charset=utf-8` | a file whose bytes are UTF-8, never served as active content |
| 200 `application/octet-stream` with `Content-Disposition: attachment` | anything else, and any file over 8 MiB, streamed |
| 401 | not paired, or a foreign origin |
| 403 | held back by the server's policy, in words for a person |
| 404 | no path, or a path the page was never offered |
| 410 | gone; `X-Gone-Worktree: <name>` when it went with a removed worktree |
| 413 | a large text file for a masked client, which cannot be streamed masked |
| 415 | not a file |

Every 200 carries `X-Content-Type-Options: nosniff` and `Content-Security-Policy: sandbox`. The
page shows a refusal's words as they are. **Without it:** a path can only be copied, and an image
the transcript carries in its record still draws.

### `GET /__reveal?path=<path>&sig=<sig>`

Opens the file manager **on the server's machine**: 200 `revealed`, or 404. The page offers it
only when its backend is local to the reader. **Without it:** no reveal.

## Markdown and diagrams

### mdrev: `/mdrev/<mdrev>/` and `/api/mdrev/`

The artifact pane shows Markdown and code through mdrev's viewer. Its bundle is served under
`/mdrev/<version>/`, immutable; the page learns the version from `<body data-mdrev>`, and its
`/markdown` page (below) answers with the version the server carries now. Its routes are mdrev's
own contract (`vendor/mdrev/release/docs/contract.md`) under `/api/mdrev/`, plus three of the
monitor's:

- `POST hold` with `{name, text}`: keeps text the transcript carries and answers `{root: "held",
  path, cap, name}`.
- `GET open?path=&sig=`: opens an offered file, answering `{root, path, cap, isGit, name, review,
  annotate}`. 401 when not paired, 404 for a path the page was not offered, and 415 when the file
  is not text (a Markdown path is taken whatever its bytes).
- `POST resolve?root=` with `{from: {path, cap}, targets}`: answers `{caps}`.

Every route needs the page's own origin (403). A file's routes also need pairing (401); held text
does not. Shared review answers 404 here. **Without it:** the pane shows text plainly.

### `/mermaid/<version>.js`

The pinned Mermaid runtime. The page head names its path (`<meta name="mermaid-runtime">`).
**Without it:** a diagram shows as its source.

## Writes

Each write passes the write gate. A refusal is HTTP 200 with `{ok: false, error, code?}`, except
`shared_runtime_required`, which is 409. `shared/control-protocol.js` is the page's reading of
every answer.

| Route | Query | Body | Success | Refusal codes |
|---|---|---|---|---|
| `POST /api/send` | `target=<id>`, `images=1` with images | the prompt as text (64 KiB at most), or with `images=1` `{prompt, images: [data URL, ...]}`: PNG, JPEG, WebP or GIF, at most 4 of 4 MiB and 8 MiB in all | `{ok: true, sid, via: "resume", "tmux" or "herdr"}` | `no-consent`, `held`, `shared_runtime_required`; otherwise words in `error` |
| `POST /api/consent` | `target=<id>`, `op=revoke` to revoke | the passcode as text, or nothing | `{ok: true, sid, granted}` | `passcode-required`, `bad-passcode`, `locked`, and the send's |
| `GET /api/dialog` | `target=<id>` | | `{ok: true, sid, open, dialog, why?}` | `no-consent`, `held` |
| `POST /api/answer` | `target=<id>` | `{fingerprint, choice}`, `{fingerprint, choices}` or `{fingerprint, text}` (one line, 2,000 characters at most) | `{ok: true, sid, via: "herdr"}` | `held`, `no-consent` |

A `held` refusal's `error` says what the reader should do, and the page shows it as is.

A `dialog` is `{kind: "question", "permission" or "plan", fingerprint, title, text, detail, note,
multi, choices: [{n, label, description, checked?, freeText?}], tabs?: [{label, done}], plan?}`.
An answer names the fingerprint, and is refused when the pane shows another dialog.

The page offers writes only when its server says it is paired (`<body data-paired="true">`), and
only for a row whose facts allow it (`injectable`, `state`, `projActive`, `agent`). **A host that
serves no writes says it is not paired**, and the page then draws no compose box and no answer
buttons.

## The detached tabs

`/image?name=&path=&sig=&held=` and `/markdown?root=&path=&cap=&from=&to=&since=` are static
pages: an image, or a Markdown document, in a tab of its own. Nothing from the address is written
into them; their script reads the address and goes through the routes above. Text carried in the
transcript reaches the tab through `sessionStorage`. `/api/mdrev-review/` is the Markdown tab's
mdrev prefix, the same routes as `/api/mdrev/` plus shared review. **Without them:** no tab of its
own; the pane still shows everything.

## Codex native mode

Native mode, for Codex sessions only, is a second API that a server built with it offers beside
this one: `/api/codex/` (status, start and stop, the gateway's reads and writes, and an event stream
at `/api/codex/events`) and the modules under `/codex-workspace/`. It appears only where the server
puts `native-entry.css` and `native-entry.js` in the page head. Its replies are mostly the Codex
app-server's own results, and the gateway (`claude-monitor-codex`) defines them. **It is outside
contract 1**: a host that does not offer it serves none of these routes, and the page never shows
the mode. The route index lists its two prefixes so the tests can hold the page's calls to them.

## The session's facts (`meta`)

A pull's `meta` is `{t: "meta", ...}`:

| Field | Meaning |
|---|---|
| `title`, `agent`, `sid`, `cwd` | the session's title, agent, id and directory |
| `turns`, `tools` | counts |
| `version` | the server build's version, not the contract's |
| `usage` | `{input, output, cache_read}` as strings for a person, `cost` (a label or null), `model`, `duration_secs`, and optionally `cost_partial`, `compacted`, `credits`, `reported`, `runtime` (below) |
| `tasks` | the session's tasks: `{id, subject, description, active_form, status, blocks, blocked_by}`, optionally with `owner`, `created`, `claimed`, `completed`, `updated`, `outcome`, `accept`, `checks`, `deferred`, `deferred_reason`, and `log: [{ts, by, msg}]`. `status` is `"Pending"`, `"InProgress"` or `"Completed"` |
| `agent_type` | a sub-agent's type |
| `ancestors` | `[{id, title}]`, a sub-agent's parents |
| `children` | `[{id, title, type, running}]`, its sub-agents |
| `path` | the transcript's path on the server machine |
| `runs` | `[{run, members: [{id, title, running, phase}]}]`, workflow runs, optional |

Inside `usage`:

- `reported` is the cost the agent itself reported: `{cost, complete, epochs, from, through,
  unknown_model}`;
- `runtime` is what the agent's client said about its session, each field optional: `context_left`,
  `context_used_tokens`, `context_window_tokens`, `effort`, `approvals`, `sandbox`, `permission`,
  `mode`, `tier`, `client`, `recorded`, `plan`, `reached`, `primary`, `secondary`.

## The record format

### Every record

| Field | Meaning |
|---|---|
| `t` | `"block"` |
| `kind` | what it is, below |
| `id` | `t<n>` for a record that opens turn `n`, `b<n>` otherwise; nested records have their own |
| `turn`, `label` | the turn number and a label of 80 characters at most, on a record that opens a turn |
| `ts`, `dur` | epoch seconds and duration in seconds, on a record that opens a turn |
| `src` | the source as written, on a person's prompt and on another session's message |
| `reply_context` | `{version: 1, items: [{quote, response?: {kind: "comment" or "following_text", text}}], request?, context?}` on a prompt that replies to quoted text. The page drops the quoted view for any other `version` |
| `inbox` | a relayed message's facts: `{from, via?, kind?, at?, to?, task?, guess?, about?, reply_to?, do?}` with `from` never empty |
| `phase` | `"commentary"` or `"final"` on an assistant's reply that carries one, with `phaseInferred` when guessed |
| `tool` | the tool's name as shown (`Edit` shows as `Update`) on a tool call |
| `run` | the workflow run a call started |
| `epoch` | `true` on a compaction |
| `fold`, `open` | for the offline page's folding; the parts do not read them |
| `head` | the record's header fields, by kind; left out when empty |
| `body` | its parts, below |

### The kinds

An open set: the page draws a kind it does not know as "Unknown".

| `kind` | What | `head` | `body` |
|---|---|---|---|
| `user` | the person's prompt, or a relayed message of theirs (with `inbox`) | | `md`, and `raw` for pasted art |
| `peer` | a message from another agent session | `from`, `mode?` | one `md` |
| `command` | a slash command | `badge` (`/name`), `preview`, `chips?` | `md` (its arguments), then a `pre` per output, or `ctx` for `/context` |
| `assistant` | a reply | `presentation?: "proposed_plan"` | `md`; a cut-off reply adds a `note` |
| `queue` | a prompt typed while the agent worked | | `md` |
| `attachment` | a file or image attached to a prompt | `att_kind` (`file`, `plan`, `edited`, `ref`, `image`), `att_name`, `att_dl`, `chips?`; `att_path`, `att_sig` and `att_fsig` when the path is offered; and `att_text` or `att_datauri` (`data:<type>;base64,...`) whenever the server could read the content, offered or not | empty |
| `agent` | a sub-agent, started or finished | `badge: "Agent"`, `preview`, `chips` (the launch, or the word it finished with), `child: "?session=<id>"`, `child_id` | started: `md` (its prompt), `note`, `md` (its result); finished: `note`, then `md` |
| `compaction` | the context was compacted | `summary`, `compact_trigger` (`auto` or `manual`), `compact_pre?`, `compact_post?` | `md` |
| `think` | thinking with no tool calls | `summary`, `chips?` | `think` |
| `act` | thinking with the tool calls it made | `summary`, `chips?` | `blocks` (the calls, as records), then `think` |
| `bash`, `read`, `write`, `edit`, `skill`, `tool` | a tool call, by family: `bash` for Bash; `edit` for Edit and MultiEdit; `write` for Write and NotebookEdit; `read` for Read, Grep, Glob, LS and NotebookRead; `skill` for Skill; `tool` for any other | `name`, `target`, `dot` (a bool the parts do not read), `chips?`, `interaction?`, `artifact?`, the offered path flattened (`path`, `sig`, `fsig`), `files?` for more than one delivered file | by tool: `edit` a `note` and a `diff` (with any output as `pre`), or its failure as `pre`; `write` a `note` and numbered lines (a new file) or a `diff`; Read and NotebookRead numbered lines; a message sent to another session `md` then a `note`; anything else its output as `pre`, with a `note` naming any file it saved, and a `note` and a `diff` per file a command edited |
| `tool` with no `tool` field | a result with no call, or a safety stop | `name`, `target` | `pre` |

`pending` is the page's own placeholder for records still owed; a server never sends it.

How the page groups records into what it draws: `user`, `command` and `peer` each open a unit; an
`attachment` right after a prompt joins it; an `assistant` that is not commentary is a unit of its
own; everything else joins the turn's process unit. The turn index's `facts` carry exactly what
that grouping needs.

### Body parts

Each part is `{p, ...}`; an open set.

| `p` | Fields | Meaning |
|---|---|---|
| `md` | `h` | **HTML the server rendered**, fences highlighted and diagrams marked; the page inserts it as is |
| `think` | `h` | the same, for thinking |
| `raw` | `x` | text shown as is |
| `pre` | `x`, `cap` | output; `cap` lines are shown before an expander |
| `num` | `rows: [[line, html]]`, `cap` | numbered, highlighted lines |
| `diff` | `rows: [[kind, line or null, text]]`, `cap` | a diff; `kind` is `ctx`, `add` or `del` |
| `note` | `x`, `file?` | a short line; `file` is an offered path with `name` |
| `ctx` | `model`, `totals`, `rows: [{label, tokens, percent}]`, `extras: [{label, value}]` | `/context`'s table |
| `blocks` | `items` | nested records, only inside `act` |

A host that writes its own records must render `md` and `think` to HTML itself, and keep it safe:
the page does not sanitize it.

### Chips carry facts in their words

A chip is `{x, c?}`, `c` one of `add`, `del`, `fail` or `sandbox`. The page reads some words
back as facts (`shared/tool-head.js`), so a host writing records uses exactly these:

- `exit <n>` and a duration (`42ms`, `2.50s`, `1m 5s`) are the call's exit and time;
- `<n> lines` is a count;
- a chip whose `c` is `fail` marks the call failed, and so do the words `failed`, `killed`,
  `declined` and `cancelled`; an `exit <n>` alone is a fact, not a failure;
- `completed`, `done`, `finished`, `stopped` and `launched` mean it ended;
- pieces in one chip are joined with ` · `.

### Smaller shapes

- **An offered path:** `{path, sig?, fsig?}`, flattened into a tool call's `head`, and an object in
  `head.files` and in a `note`'s `file` (with `name`).
- **A question card** (`head.interaction`): `{kind: "request_user_input", resolved, answers:
  [{id, label}], asked?: [{header, question, multi, options: [{label, description, chosen,
  preview?}], typed?, notes?}], unanswered?: {why: "timeout", "declined", "failed" or "none",
  seconds?}}`.
- **An artifact** (`head.artifact`): `{url, name, icon, desc}`.
- **A sub-agent link:** `head.child` is a relative `?session=<id>`; `head.child_id` names it
  directly.

## What the server puts in the page

A host that serves the monitor's page, or mounts its parts, supplies these with it:

| What | Read for | Without it |
|---|---|---|
| `<body data-paired>` | whether writes are offered, and whether an HTML file is framed in the pane | no compose box, no answers, no framed pages |
| `<body data-contract>` | the contract version | the page cannot say which version it reads |
| `<body data-version>` | the build, shown in the brand | `vdev` |
| `<body data-mdrev>` | the pinned mdrev release | Markdown shows as text |
| `<meta name="mermaid-runtime">` | the Mermaid runtime's path | diagrams show as source |
| `<meta name="viewport">` | Codex native mode keeps and restores it | native mode does not start |
| `native-entry.css` and `native-entry.js` in the head | Codex native mode | no native mode |

The page itself (`/`, `/index.html`) and its icons and manifest (`/favicon.svg`,
`/apple-touch-icon.png`, `/manifest.webmanifest`, which the listener answers unpaired) are the
monitor's own page, not something the parts read, so the route index leaves them out. The page's
head links (`monitor-ui/…`, the icons and the manifest) are written relative to the page, and the
page's own backend handle is based at the directory the page was served from. So a host that
serves the page at a prefix of its own, at a path ending in a slash (`/agents/monitor/`), has it
load its parts and reach its routes under that prefix, its Markdown and image tabs too, with
nothing to rewrite. Step 4 of `design/embedding.md` moves
what the page reads about its backend (paired, version, mdrev) from the head to the backend handle.

## How this document is held to the code

- `claude-monitor/tests/request_layer.rs`: every server path the page's modules name matches a row
  of the route index.
- `claude-monitor/src/routes.rs`: every row is a route the table answers (asked as the table is
  asked, a refusal counts; the static fallback's answer does not), and this document's version
  line is `CONTRACT_VERSION`, the one the page carries.
- `claude-monitor/src/codex_workspace.rs`: the native rows, answered by a server built with
  Codex native mode.
