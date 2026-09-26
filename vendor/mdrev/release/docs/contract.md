# The Document Contract

*mdrev · reference · 2026-09-02 · the routes a host implements to carry mdrev's guest*

A host application that serves Markdown over HTTP can carry mdrev inside its own pages by serving the guest bundle as static files and answering the routes below under a prefix of its choosing. Everything else — the redline, anchors, diagrams, math, highlighting — happens in the reader's browser. The design and its reasons are in [embedding.md](embedding.md); this page is only what to build, and `mdrev-cli conform` checks it.

Two things to hold on to. **Texts and revisions come from the host's own store**, however it keeps them, in whatever language it is written in. **Notes are mdrev's**: they live in mdrev's sidecar in the checkout, and the host writes them by calling `mdrev-cli` rather than by reimplementing the format, so that a note filed in the host's page is a note in `mdrev` on any clone that carries the file.

## What needs a checkout, and what does not

The two halves of this contract do not ask the same thing of a host, and the
difference is worth knowing before you build.

**Reading is store-agnostic.** `/text`, `/revisions`, `/asset`, `/changed`,
`/recent-changes`, `/recents`, `/stat`, `/documents` and `/events` want two texts, an opaque
revision id, and a list or two. A host can answer every one of them out of a
database with no files and no git in sight — the redline, the anchors, the
diagrams, the maths and the highlighting all happen in the reader's browser
either way. Nothing below is asking for a filesystem; where the text below
says `git …`, it is showing how a checkout answers, not what an answer must
be.

**Reviewing is written in git's terms, today.** A note records the revision it
was taken on and a snapshot of the text it saw, and it is placed on a later
version by diffing from that snapshot. So `/annotations` and `/snapshot` mean
what they mean because of the store underneath: `rev` must be a commit id for
notes to travel, the snapshot is a blob, and a host stores notes by calling
`mdrev-cli` against a checkout rather than reimplementing the record, so that
one format stays one format.

A host with no checkout is not shut out. Answer `GET /annotations` with `[]`
and refuse the writes, and the reader gets the document, its history, its
redline and every list — everything except notes. That is a real and useful
viewer, and it is what most embedders will want first.

## A document is (collection, path), and a reader moves without a page load

Every route below takes an optional `root`, and a host that holds more than one
folder must send it: a path stops identifying a document as soon as two
projects each have a README.md. That much has always been here. What was not
written down, and what cost three bugs on 2026-09-10, is the LIFETIME that goes
with it.

The reader moves between collections **without a page load** — the host calls
`mountMdrev(…).open(path, range, {root, cap})` and the viewer re-renders in
place, which is the point: their scroll, their open notes and the tree's
expanded branches all survive. So a fact the host states about a collection
must not outlive that move, and the viewer's mount options are split by
lifetime to say which is which:

| | | |
|---|---|---|
| **Host facts** | `contract`, `credentials`, `trustHtml` | true of the application; survive every move |
| **Collection facts** | `root`, `isGit`, `caps`, `review`, `annotate` | true of ONE folder; **replaced** on every move, never inherited |
| **Opening facts** | `path`, `range`, `rangeAsked` | `path` and `range` are set at mount and **replaced** by `open()`; `rangeAsked` is read once, at mount, and never again |

`rangeAsked` is the third lifetime, and the only one worth a paragraph. mdrev
remembers the mode each document was last read in, and that memory outranks the
range a page opens with — which is right for a reload, a bookmark or a shared
link, and wrong for a range the reader named a second ago. `mdrev doc.md
--last 1` came up clean for anyone who had last left that document clean, headed
AS OF with the redline control unlit, and read as "nothing changed". The viewer
cannot tell those apart, because both reach it as nothing but a range. So the
host says which it has: set `rangeAsked` when the reader has just asked for this
range in so many words — a command line, a "what changed since Tuesday" control
— and leave it alone for a page that is merely showing a range it was holding.
It is read once, at mount; a move to another document goes back to that
document's own preference. mdrev's own command lines write `asked=1` into the
address for exactly this, and never write it back, so the claim belongs to the
command that made it and to nothing after.

`isGit` is the one that bit. A file opened outside a checkout mounted with
`isGit: false`; the reader walked into a repository through the switcher; the
client for the new collection was built by spreading the old options, so it
carried `false`, and the redline switch, the range picker and the changed rail
stayed gone for the life of the page while the title followed the document
perfectly. State a collection fact for the collection being MOVED TO, or state
none and let the viewer ask.

`review` and `annotate` are the host's LEAVE, and each is a ceiling, never a
floor: `effective.review = review ≠ false ∧ served revisions ∧ isGit`, and
`effective.annotate = annotate ≠ false ∧ the notes route answers` — and a refusal is whole: `annotate: false` draws no notes and never asks the annotation routes, and neither refusal reserves the room the thing would have taken. `isGit` is
the fact; `review` is the choice — a host with no checkout says `isGit: false`
and needs no `review`; a host WITH a checkout that does not want redlines on a
page says `review: false`, which is the one power the routes cannot express.
Declaring a part the routes cannot serve gets nothing. Left out, both are
discovered, exactly as today; a host that says nothing is unchanged. And they
are collection facts for the same reason `isGit` is: say them for the
collection being moved to, or say none. mdrev's own two hosts — the daemon and
mdrev-v2 — declare both, and are the worked example. (`docs/what-a-host-takes.md` is the
design.)

For a host writing against this contract the rule is short: **anything you tell
the viewer about a folder, tell it again when the reader changes folder.**

## The routes

All under one prefix, say `/docs/api/mdrev`. Every request carries the host's own session (`credentials: 'same-origin'`), and the host decides what a session may read and write; mdrev never sees a filesystem. Errors are JSON `{"error": "…"}` with the status that fits: 400 for a bad request, 403 for a session that may not, 404 for a document or note that is not there.

### Capabilities — optional, and the shape of "may not"

A host may require that every route about a document carry a **`cap`**: an opaque string it minted for that path and can check. The viewer then cannot name a document it was not given one for, which is the point — a session says the reader may ask, a capability says what they may ask about.

Three pieces, and a host that wants none of this can ignore all three:

- The host mints one for the document it decides to serve and passes it to `mountMdrev` as `cap` (§4 of the [guide](embedding-guide.md)).
- The viewer sends it as `&cap=…` on every route about that document — `text`, `revisions`, `asset`, `snapshot`, `stat`, `events`, `annotations` and the note routes.
- Anything the document points at is asked for through `POST {prefix}/resolve`, below, because only the viewer knows what a document references and only the host knows what this reader may have.

A path with no capability, or the wrong one, is **403** — including a path that does not exist, since "you were not offered that" is the truthful answer either way and telling the two apart is how a caller maps a disk. `mdrev-cli conform --cap …` checks a host this way.

### `POST {prefix}/resolve` — required only with capabilities

```json
{"from": {"path": "docs/a.md", "cap": "…"}, "targets": ["pic.png", "../shared/b.md"]}
→ {"caps": ["…", null]}
```

The viewer has parsed a document it holds and found what that document points at; the host says which of those this reader may have, in order, `null` for each refusal. `targets` are already root-relative, resolved against the referring document. A refused reference is drawn as refused and never requested; a capability that comes back is also what opens that path as a document, so following a link needs no second conversation. Refuse the whole call with 403 if `from` is not held.

### `GET {prefix}/text?path=&rev=`

The document's Markdown, as `text/plain; charset=utf-8`. `rev` is a revision id the host issued through `/revisions`, or `current` for the document as it stands. A `path` that names nothing at that revision is 404; a `path` that tries to leave the host's document root (`../`) is refused. Send an `ETag` if you can: a viewer without push polls this route with `If-None-Match`, and a `304` costs nothing.

### `GET {prefix}/revisions?path=`

The revisions that touched the document, **newest first**, as a JSON array:

```json
[{"rev": "e74f9c7…", "date": "2026-09-02T10:32:34-07:00", "author": "Hong Tang", "subject": "mdrev-bundle: a build script…", "path": "docs/embedding.md", "body": "dist-release/mdrev-bundle-<version>.tar.gz — …"}]
```

`rev` is an opaque string to the viewer but **must be the git commit id** when the host wants its notes shared with the mdrev application, because a note records the revision it was taken on. `date` is ISO 8601. `body`, the full commit message, is optional and shows in the blame detail. An empty array means the document has no history; the viewer is then a reader with notes.

`path` is the name the document had at that revision. A host that follows renames (docs/renames.md) lists the revisions of the *document* — under every name it has had — and its `/text` reads a revision under the name listed for it; `mdrev-cli revisions --path P` answers exactly this, and `mdrev-cli text --path P --rev R` reads a revision the same way. A host that lists the history of the *name* omits `path`, and its readers lose the history and the notes across a rename.

**With no `path`, the COLLECTION's own revisions** — optional, and the viewer
degrades without it. Like `/changed` it is a range question rather than a
document's, so it carries no `cap`; a path still needs the capability that
opens it. `body` is not wanted here — this listing is arithmetic, not reading —
and a host may bound how far back it goes.

What it answers is **whether this collection has any history at all**, which is
the one thing the per-document call cannot say: an empty array there means
"nothing has touched this document", and that is true both of a folder with no
history and of a draft nobody has committed yet inside a checkout. The viewer
asks this when it needs to tell those apart — a host that answers `[]` or does
not answer is taken at its word, and the reader gets a viewer with no history
controls, which is the honest reading of two empty answers.

(It measured time windows too, until 2026-09-10. A window is now read against
the document's own commits: a `past 4h` offered because some other file in the
repository had just been committed drew a redline with nothing in it.)

### `GET {prefix}/asset?path=`

A file the document refers to — an image, mostly — with its media type. The same root and the same refusal to leave it as `/text`.

### `GET {prefix}/annotations?path=`

The document's notes, as a JSON array of records in mdrev's shape (below). The host answers by running `mdrev-cli notes list --path P --all --records --root <checkout>` and returning its output as it is — or by reading the sidecar, `.mdrev/annotations/<path>.jsonl`, one record per line. Without `--records` the CLI judges every note against the file first — where its text is now, for an agent reading `--notes` — which the viewer does not use and which costs a word diff per snapshot: seconds, on a document with a few dozen notes, after every note and reply and on every poll. An older host that runs it that way and returns each item's `annotation` still works, slowly.

### `POST {prefix}/annotations?path=`

File a note. The body is the record the viewer built: `{body, type, anchor}`, with `anchor` in source space, and the `rev` on screen — plus `fromRev` for a note on struck text; pass the body through as it is. The host adds what it knows and stores it, and returns the record as stored, with 201: run `mdrev-cli notes add --path P --root <checkout>` with the body on stdin, and return its output. The CLI resolves the revision, writes the snapshot `blob`, places an anchor whose offsets were only a guess, and appends the line.

### `PATCH {prefix}/annotations/{id}?path=` · `POST …/{id}/replies` · `DELETE …/{id}` · `DELETE …/{id}/replies/{at}`

Close, reopen, answer, remove — and take one reply back out of a thread. `PATCH` carries `{status: "resolved" | "wontfix" | "open", resolvedBy?: {note}}`; the reply body is `{body}`. A reply has no id of its own, so it is named by the note and its `at` (URL-encoded), which every listing carries. Each returns the record as it now is (204 for deleting a note). With the CLI: `notes resolve|wontfix|reopen ID --note "…"`, `notes reply ID --body "…"`, `notes delete ID`, `notes delete-reply ID --at "…"`; and, with no route of its own, `notes follow-renames [--path P] [--from OLD --to NEW]` moves records into the sidecars of the documents they are about after a rename (docs/renames.md §5). A host that offers reading and filing only may answer these with 501; the viewer's close and reply controls then fail, and everything else works.

### `GET {prefix}/snapshot?blob=` — optional

The text a note was taken on, by the `blob` in its record, as `text/plain`. With it the viewer places a note by walking the diff from that text to the one on screen — exact, and the same answer `mdrev-cli notes list` gives an agent — instead of searching for the quote. `git cat-file -p <blob>` in the checkout answers it; a host without the object store answers 404 and the viewer searches.

### `GET {prefix}/documents` — optional

Every document the host holds, as `{files: [{path, cap?}]}`, so the viewer can
offer a switcher. Not what changed and not what has been opened — the viewer
has those two lists already; this is the one a reader needs to reach a document
nobody touched in the range they are looking at and they have never opened,
which otherwise means typing a path. `git ls-files -- '*.md' '**/*.md'` answers
it in a checkout.

It is about the host, not about a document, so it carries no `cap` of its own —
the key is the whole of the authorisation, as for `health`. But a host that
gates its reads must mint one **per row**, or every row is a document the
viewer may name and not open. A host that does not answer it leaves the viewer
without that section, which is what every viewer had before it existed.

### `GET {prefix}/tree?node=` — optional

The documents you hold, arranged the way your reader thinks of them, one level
at a time: `{nodes: [{id, label, kind, root?, path?, cap?, count?, removable?}]}`. Without
`node`, the top level; with one, that node's children.

`kind` is `branch` for something that opens — a project, a folder, a
collection — or `doc` for a document, which then carries the `root` and `path`
the viewer hands back to open it, and a `cap` if you gate reads. `id` is yours
and opaque: the viewer shows `label`, and passes `id` back when the reader
opens a branch.

`count` on a branch is how many documents are under it, all the way down: a
folder is worth opening or it is not, and the number is what says which, so it
rides with the branch rather than costing a round trip per folder to find out.
Leave it out and the row has no badge.

**Leave it out when knowing it would cost more than it is worth.** A checkout
answers this in milliseconds — git already has the list. A plain folder does
not: the only way to know is to walk it, and one of the folders a reader opens
is their home directory. mdrev's own host gives such a walk a quarter of a
second and, if it does not finish, sends no `count` at all rather than a
partial one. A number that is expensive to get and out of date once you have it
is not worth a badge; no badge says "unknown", which is true.

And a listing you truncate is not a count. mdrev's host bounds the LIST it will
send at two thousand rows and counts separately, because those are different
questions — sending the cap as the count reported a home directory as holding
exactly 2000 documents, which looked exactly like the truth.

`removable` on a TOP-LEVEL branch says the reader may take that collection out
of this listing — see `POST {prefix}/forget`. Absent means no, which is the
right answer for every nested folder and for a shell that keeps no such list.

**A row's `root` is the collection, not the branch it was listed from.** The
viewer compares a row's root with the one it has mounted to decide whether a
click is a move to another collection — a document listed under a folder but
rooted at that folder would make every sibling look foreign, and reaching it
would tear the viewer down and build it again.

A BRANCH is asked for only when it is opened, and asked again on a slow timer
while it stays open, so a large store costs nothing until someone looks and a
stale listing does not persist. THE TOP LEVEL — the call with no `node` — is
different: it is asked for ONCE per collection whether or not the reader opens
the section, because the section's header says how many documents are under it
and a number nobody fetched is no number at all. Once, and then not again until
the section is opened; a shut rail does not poll. So the top level is the one
listing to keep cheap: it is a row per collection with a count, not a walk of
anything. Nothing says a branch must be a directory: a
collection, a tag, a query are all a `branch` with children, and mdrev's own
host answers with the folders its reader has opened and the documents inside
each.

This is the richer sibling of `/documents`, which is one flat list. Answer
whichever suits your store; a host that answers neither leaves the viewer with
the changed-files list alone.

### `GET {prefix}/changed?from=&to=&since=` — optional

The documents a range touched, as `{files: [{path, status, cap?}]}`, so the
viewer can offer the changed-files rail. `from` and `to` are revisions as
`/revisions` names them, `to` omitted meaning the working copy; `EMPTY` as
`from` means before the first revision, and a host that keeps history from a
fixed beginning answers it with everything it holds. `status` is one of `A`
added, `M` modified, `D` deleted, `R` renamed — a renamed document is named by
where it ended up, and a deleted one is listed but cannot be opened.

Like `/documents` it is about the range and not about one document, so it
carries no `cap` of its own — the key is the whole of the authorisation — and a
host that gates its reads mints one **per row**, or the rail is a list of names
that refuse to open. `git diff --name-status --find-renames <from> <to> --
'*.md'` answers it in a checkout.

**A window is answered as a window.** `since` is optional and carries an
instant — ISO-8601, or `@<epoch>` — set only when the reader picked a span of
time ("past 1w") rather than two revisions. `from` and `to` still come with it,
because the document beside the list is still rendered between two revisions;
`since` says the LIST is to be answered by time. The two are different
questions: "what differs between that commit and now" parts company with "what
changed this week" wherever history is not linear in commit date — a branch
merged later, a rebase, a commit landing with an older date — and a reader who
asked for a week should be told about the week. In a checkout the set is `git
log --since-as-filter=<t> --name-status --find-renames -- '*.md'` plus what the
working copy has changed — `--since-as-filter` and not `--since`, because
`--since` PRUNES the walk and gives up on the very history this exists for.
Each row's `status` is read from the diff against `from` where that diff
carries the path, and otherwise from what the window itself did to it: `A` born
inside it, `R` arrived under this name, `D` ended inside it, `M` anything else.
That case is not rare — the boundary a window resolves to can be NEWER than the
cutoff wherever an old-dated commit sits at the tip, and then the diff is empty
and the window is not. A document born and buried inside the window is not
listed at all: it does not exist to open and never existed to the reader.
**Do not resolve it yourself and pass approxidate a phrase**: git's
date parser never fails, so a cutoff that is not a timestamp reads as "now" and
answers with an empty list the reader will take for "nothing changed". Refuse
it instead. A host that does not implement `since` ignores it and answers
exactly as it does without it — the viewer sends it either way.

**Newest first.** The list is ordered by when each document last changed inside
the range — a rail in path order buries the one that just changed under twenty
that did not. A document changed only in the working copy is newer than
anything committed and comes first; ties fall back to the path, so two
identical questions get one answer. `git log --name-only` over the range says
it in a checkout, in one pass rather than one per file.

### `GET {prefix}/recent-changes?at=` — optional

What changed **lately**, as `{files: [{path, status, at, side, cap?}], skipped?}`
— the same pane as `/changed`, answering the question that is left when there
is no range to answer. The viewer asks it in exactly those cases: reading one
version cleanly, and reading a document in a collection with no history at all.

`at` is the revision on screen, omitted when that is the newest — and it is the
MIDDLE of the question rather than its end. At the tip the answer is the ten
documents changed most recently, reaching back no further than a week. From an
older revision it is five changed **before** it and five changed **after** it,
each reaching no further than a week in that direction: a reader standing in
the past is asking what was going on around them, not what has happened since.
`at` is seconds since the epoch — when that document was last touched on that
side — and `side` is `before` or `after`, which the viewer marks, because news
from ahead of the reader is a different kind of fact from news behind them. The
revision named by `at` is not itself listed: that is what the reader is
reading. `status` is `/changed`'s own vocabulary, and rows carry a `cap` each
for the same reason.

**Newest first**, across both sides — every `after` row is newer than every
`before` one, so one order says both things.

**`skipped` is a host declining to look.** A collection with no history can
only be asked by reading the disk, and one of the folders a reader opens is
their home directory: a walk that costs more than the answer is worth reports
`{files: [], skipped: true}` rather than the part of the tree it reached. The
viewer draws nothing at all for that — not an empty list, which says "nothing
changed" and is a different statement. In a checkout the answer is one `git log
--since-as-filter=@<t> --until=@<t> --name-status --find-renames -- '*.md'`
around the anchor, plus the working copy when the reader is at the tip.

### `GET {prefix}/recents` — optional

What this reader has opened lately, as `[{root, path, at, cap?}]`, newest
first, so the viewer can offer a switcher back to a document they were in
before. `root` is the host's own name for the collection a document belongs to
— a checkout, a workspace, a project — and it is opaque: the viewer hands it
back and never parses it. `at` is milliseconds since the epoch. A `cap` per row
again, for the same reason.

Send a `label` too, if the collection has a name worth showing. Without one the
viewer takes the last slash-separated segment of `root`, which is a guess that
only means anything when the collection happens to be a directory — and which
tells two of them both called `docs` apart not at all.

This is the reader's list, not the collection's, so it may span every `root`
the host holds — and should. A switcher filtered to the collection on screen is
a switcher that cannot do the one thing it is for.

> **How a host learns what the reader is reading.** Nothing in this contract
> tells you: the reader moves between documents without a page load, which is
> the point of mounting a guest, so your page route sees the first document and
> nothing after it. There is no "now reading" call and this contract does not
> add one — it would be a promise every host had to keep for a list that is
> optional. Infer it instead: a `GET /text?rev=current` with a capability that
> opens it IS the reader reading that document, and it names the collection
> too. mdrev's own host does exactly that, guarded by the last one recorded,
> because that route is also the poll that watches the file.

### `GET {prefix}/stat?path=&cap=` — optional

When the document was last written, as `{mtimeMs}` — milliseconds since the
epoch, or `null` for a host that has no such notion. The viewer shows it beside
the document's name, and shows nothing without it.

### `POST {prefix}/forget` — optional

```json
{"path": "<a top-level node id>"}
→ {"ok": true}
```

Stop offering one collection in `/tree`'s top level — the rail's "Remove
folder". It removes **nothing from disk**: it drops one entry from whatever
list the shell keeps of the collections it has been asked to serve, and after
it the shell no longer serves that collection either.

**Say which rows can be removed in the listing, not by refusing afterwards.**
A shell can usually forget some of its collections and not others — the folder
it was started for, a set it was configured with — so `/tree`'s top-level nodes
carry `removable: true` where this route will work, and the viewer draws the
entry only there. A menu entry that appears and then fails is worse than one
that never appears.

Three refusals are worth spelling out, because each is the shell declining to
promise what it cannot deliver: **501** from a shell with no such list at all;
**403** for a collection the shell cannot stop serving whatever it writes down;
and **403** when removing it would leave the requesting page's own root
unserved — a page can be mounted on a directory that is merely *inside* a
top-level root, so comparing the target with the mounted root is not enough,
and the question to ask is whether that root would still be servable with the
target gone. Stranding it would answer 403 to everything the page did next
while the article stayed on screen looking healthy. **404** for a collection
the shell is not listing.

And one thing not to do: `/tree` must keep answering **403** (never 404 or 501)
for a node id it has just stopped listing. The viewer reads 404/501 on that
route as "this shell has no tree", and would take the whole section away.

### `POST {prefix}/reveal` — optional

```json
{"path": "docs/a.md", "cap": "…", "root": "…"}   a document, by the capability it was listed with
{"path": "<a node id>", "folder": true}         a folder, by the id `/tree` gave it
→ {"ok": true}
```

Show this file to the person at the machine — the rail's right-click menu asks
for it. **The viewer cannot do this and never will**: it knows a path inside a
collection and nothing about the machine it is on, and a guest that shelled out
would be a guest running commands on its host. So it asks, and the host does
whatever "show me this file" means where it runs: `open -R` on macOS,
`explorer /select,` on Windows, the enclosing folder elsewhere.

The gate is the row that was clicked, and no wider. A document carries the
capability its listing minted and is checked exactly as reading it would be; a
folder carries none — nothing mints a capability for a folder — so it is named
by the opaque node id `/tree` handed out, and the host reads it the way `/tree`
does. Neither can name anything the reader was not already being shown.

**`root` is the ROW's collection, and it is not always the one in the query.**
The tree lists every collection the reader has open, so a row can belong to
another one — and its capability was minted for THAT collection, so checking it
against the page's would refuse a row the reader was plainly offered, or, where
both collections hold that name, reveal the wrong file. Admit the named root on
the same terms you admit the one in the query — the reader must already have it
— and check the capability against it. Absent means "the collection in the
query", which is what a host serving one collection always means.

**The three answers, and why they are three.** `200` did it. **`501`, or no
route at all, means this host can NEVER reveal anything** — a server with no
desktop, a deployment where opening things is not wanted — and the viewer drops
the entry from the menu for good. Anything else is about THIS ROW: `403` a
capability that does not open it, `410` a document that is no longer on disk,
`5xx` a revealer that failed. The viewer reports those and keeps offering the
entry on every other row.

Do not answer `404` for a document that has gone. `404` is how an optional
route says it does not exist, so the viewer reads it as the host's verdict on
itself and takes the entry away from every row — which is a menu that vanishes
because one file was deleted. `410` is the status for "it was here and is not".

### `POST {prefix}/log` — optional

```json
{"lines": [{"kind": "page.gone", "was": "2026-09-25T03:45:31.000Z", "road": "listing", "root": "/srv/docs"}]}
→ 204
```

The viewer's account of what went wrong in the page — the messages it showed
the reader about a problem, a note that did not save, the host going away and
coming back, an error nobody caught — for the host to keep beside its own
logs, so a post-mortem can read what the reader was told. mdrev's own hosts
write these into the machine's event log, `mdrev --events` (docs/events.md).

It is rare by construction: nothing is sent per request, per render or per
poll, a page sends at most twenty lines a minute, and a batch is at most
twenty lines in 16 KB. `kind` always starts `page.`; `was` is when it happened
in the page, which can be well before it arrives — a page that lost its host
keeps what it could not send and sends it after a reload. The other fields are
short strings, numbers and booleans: paths, collections, statuses and
messages, never a note's text.

**A message names the URL that failed, and a URL carries its capability**:
strip every URL's query before you keep a line. A host that keeps no such log
answers `404` or `501` and is not asked again by that page; `204` for a batch
taken, `400` for one that is not a batch of page lines.

### `GET {prefix}/events?path=` — optional

Live reload. Either a `text/event-stream` that sends an event named `change` when the document changes, or **204**, which tells the viewer to poll `/text` every few seconds while its tab is visible. Answer 204 unless you already have push.

## The note record

One JSON object per note, the same on the wire and in the sidecar:

```json
{
  "id": "ann-mtk9c8ad-p4hk",
  "path": "docs/embedding.md",
  "rev": "6db2f14e91ebdf07a003dc5a98998c4f1ef17767",
  "blob": "61fb8f68e449fa77ffceea7a91e1011d8bf3c417",
  "created": "2026-09-02T15:33:12.101Z",
  "author": "hong@aries-black",
  "type": "comment",
  "status": "open",
  "body": "Yes, it does",
  "anchor": {
    "exact": "Does",
    "prefix": "e host team, before the spike\n\n- ",
    "suffix": " the host have revision history for",
    "start": 16395,
    "end": 16399,
    "trail": ["One Origin, No Iframe", "8. Questions for the host team, before the spike"],
    "space": "source",
    "side": "to"
  },
  "replies": [{"author": "agent@host", "body": "…", "at": "…"}],
  "resolvedBy": {"note": "what was done", "commit": "795b26b…"}
}
```

- `id` — the host may mint one; the CLI does.
- `rev` — the git commit the note was taken on; `WORKTREE` for an uncommitted file. `blob` — a content snapshot of the document as the reviewer saw it, written to the git object store; with it a note is mapped *exactly* through later edits, without it every reader falls back to searching for the quote. The CLI writes both.
- `fromRev` — only on a note on struck text (`anchor.side: "from"`): the older revision of the redline it was taken on. Its offsets index that revision's text, and `blob` is that text; `rev` stays the revision on screen. The viewer sends it and the CLI snapshots it.
- `anchor` — `exact` is the quoted text; `start`/`end` are offsets into the Markdown **source** (`space: "source"`); `prefix`/`suffix` are the 32 characters around it; `trail` is the heading path; `side` is `to` for text in the newer revision of a redline and `from` for struck text, which exists only in the older one. A host filing a note by hand may send `exact` alone with zero offsets; the CLI locates it.
- `status` — `open`, `resolved`, `wontfix`. `resolvedBy.note` is what the closer said; `resolvedBy.commit` is the commit that answered it, which is where the reasoning lives.

## The sidecar, and whether it travels

`<checkout>/.mdrev/annotations/<path-to-doc>.md.jsonl`, one record per line: append on create, rewrite on change. mdrev writes a self-ignoring `.mdrev/.gitignore` beside it because a local review is private; a host whose notes are meant to be shared with the mdrev application on other machines **deletes that marker and commits the sidecars** — that is the switch, and the marker says so. Two writers of one file at the same moment are the host's per-file lock; two clones are git's ordinary merge of a line-per-record file.

**A rename, and which hosts follow it.** The sidecar sits at the name the document had when its notes were filed, and `git mv` leaves it there. A host that answers `GET {prefix}/annotations` by running `mdrev-cli notes list` follows for free: the CLI walks every record forward through the checkout's rename history and answers with the notes of the *document*, whatever it was called when each was filed (`docs/renames.md`). A host that reads the sidecar by path does not, and its readers lose their notes on a rename until it follows the history itself. mdrev's own two hosts follow. Not a conform check: the runner cannot rename a document in a host's checkout to find out.

## The policy to send with the page

A host that serves other people's Markdown sanitises it — the guest does, by default, showing raw HTML as code and dropping executable link schemes — and sends a Content-Security-Policy so that anything that slipped through could not run. The guest needs this much and no more:

```
Content-Security-Policy: default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline';
  img-src 'self' data: blob: https: http:; font-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'none'
```

Scripts only from the host's origin — the bundle ships as files, and the host's own mount script must be a file too, not inline. `style-src 'unsafe-inline'` is the one allowance: mermaid, KaTeX and shiki all write inline styles. `mdrev-v2` sends exactly this header, and its page, diagrams, formulas and code render under it with no violation.

## Checking a host

```
mdrev-cli conform --url http://localhost:8080/docs/api/mdrev --path docs/guide.md --root /srv/docs
```

Every route, every shape, the refusals, and the round trip: it files a note through the host's routes, finds it in the sidecar with the same code `mdrev --notes` uses, closes it, and deletes it. Optional parts a host lacks are reported as skipped, never as failures. Exit 0 conforms.

`--root` names the COLLECTION, and a host holding more than one needs it: it is
sent as `root` on every request, exactly as the section above requires, and
without it every route asks for `--path` in whichever folder the host happened
to start with. That is the one failure worth recognising on sight, because it
does not look like itself — a wrong (or missing) collection is refused, not
404ed, so a checker that cannot name the collection reports a wall of 403s and
reads as though the host were broken. `conform` now says so at the bottom of
such a run. A host that gates its reads needs `--cap` as well, and a capability
is minted for one `(collection, path)` pair, so the two have to agree:

```
mdrev-cli conform --url https://wiki.example.com/docs/api/mdrev \
  --path team/guide.md --root /srv/collections/team \
  --cap "$(the capability that host minted for team/guide.md)" \
  --header "cookie: session=…"
```

`--root` doubles as the checkout whose sidecar the round trip reads, which only
a host running beside this process has; when it names nothing on this machine
that one check reports as skipped, and the contract checks are unaffected.
