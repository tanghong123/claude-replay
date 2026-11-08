# Mounting agent-monitor's backend in another server

> **DRAFT 1** (#s426, 2026-11-08). Step 3 of `design/embedding.md` ("The order of the work").
> Nothing in it is built yet.

## What it is for

A host that links this repository's crates into its own Rust web service should be able to serve
the session list and the session view from the same route table the monitor serves. It mounts the
table under a path of its own, names the sessions to serve, and puts its own sign-in in front.
`docs/backend-contract.md` (contract version 1) is what the table answers, and it does not change.

Today only the `agent-monitor` binary hosts the table. Some things hold only because that binary,
or its listener, makes them true. An inventory of them, made at main `d83e36b8`, found about forty.
This note groups them into what a host can already do, what it needs, and the order to build it in.

## What already works

- **The table is a plain function.** `routes::handler` returns `Fn(&Request) -> HttpResponse`, with
  no socket. A host strips its prefix from the path and builds a `Request`, whose fields are public.
- **The routes take their request facts from the `Request`:** whether it is paired, whether its
  origin is the page's own, and whether its replies are masked. No route looks at the port or the
  `Host` header itself.
- **The session service takes its cache root and scratch directory as values,** and serves any
  transcript of a built-in agent once it is registered.
- **The page joins every server path to its backend handle's base** (step 1), Mermaid's runtime
  included (#s423).
- **Under the single-writer cache** a pull never redirects to another process.

## What a host needs

### 1. Sessions it names

The index lists every transcript in the stores the process's environment points at
(`CLAUDE_PROJECTS_DIR`, `CODEX_HOME` and the others, or `$HOME`). A host cannot name its own, and
changing process-wide environment variables is not safe once threads run. The adapters already have
a way to list one chosen store (`store_transcripts_in`); the index does not use it yet.

**Build:** the index takes the stores to list: pairs of an agent and a store root. With none given,
it keeps today's machine-wide default, so the monitor is unchanged.

### 2. Its own machine left alone

Every scan runs `ps` and `lsof`, reads the Claude desktop app's records and asks herdr, to tell
which sessions are live and which pane each runs in. For a host whose sessions do not run on its
own server, those facts are wrong, and the probes cost CPU on every scan.

**Build:** a switch on the index that turns the process probes off, the desktop store and herdr
with them. With it off, a row has no `pid`, `term` or `target`, and its state is what the transcript
alone shows. Those fields stay in the contract; only their values change. The Arrange choice's
readers (`api/layout`) are left out the same way, which the contract already allows.

### 3. Its own state

Most state is read from the directory the environment names, whatever the host passed: pane grants,
the passcode, the file-stamp key, and the render and mask policies. Beside a monitor run by the same
user, a host would share the monitor's grants and key. mdrev's notes key is read, and minted, in the
machine's `~/.mdrev`.

**Build:** one state directory, the one the index is already given, used for all of it. mdrev's
state directory is given too, or none (then the pane files no notes, and the shared-review prefix
is not served). The relayed-message key source is given, or none.

The key and the two policies stay one per process for now: a host mounts one table per process.
Two tables in one process is a later change.

### 4. A ceiling on the routes it serves

One fact, "paired", opens three things at once: reading local files, every write (sending a prompt,
granting a pane, answering a dialog) and Codex native mode. A host that lets its signed-in readers
open files would also let them start an agent turn on the host's machine, as the host's OS user,
with permissions bypassed. `design/embedding.md` already says a host declares which routes it serves
and that the declaration is a ceiling.

**Build:** the table takes the routes it serves: reading (always), files, reveal, writes, shared
review, Codex native. A route outside the ceiling answers 404, as a route a host never serves does.
The monitor binary serves them all, as today. A host's table serves reading and files unless it
says otherwise. The page learns the ceiling in step 4, from its backend handle; until then a host
whose table serves no writes says `paired=false` in its page.

### 5. Replies finished as the listener finishes them

The monitor's listener does work the table does not: it masks secrets in replies to a client the
mask policy covers, adds `Cache-Control: no-store` where a reply sets none, decodes the gzipped
Mermaid runtime for a client that does not accept gzip, enforces each route's body limit, and writes
a large file out from disk. A host calling the table directly would send replies unmasked, and a
large download empty.

**Build:** an exported step that finishes a reply for a request (masking, the default cache header,
decoding where needed), the same code the listener runs, and an exported table of body limits by
route. A host honours a reply's file to stream; the contract names it.

### 6. Its own path in the page

The monitor's page writes root paths into its HTML: its stylesheets, its first module, the icons
and the manifest. Under a host's prefix they load from the host's root.

**Build:** the page takes a mount base. It writes it into those links and into the page as
`<body data-base>`, and the default backend handle reads it. That adds to the contract, so its
version stays 1. A host that mounts the components (steps 4 and 5) instead of serving the page does
not need it.

### 7. A host's sign-in refuses requests from other sites, GETs included

Two GET routes have side effects: `api/ignore` changes the hide list, and a stamped `/__reveal` opens
a file manager on the serving machine. A request another site makes, an image say, sends no
`Origin` header, and the table's origin fact counts a missing `Origin` as the page's own. So the
table cannot tell such a request from the page's. On the monitor the pairing cookie is
`SameSite=Strict`, which a browser withholds from a request another site starts, and the gate
refuses it. Behind a host whose sign-in sends its cookie on such a request, either route could be
reached from another site.

**Build:** nothing in the table. The contract says a host's sign-in refuses a request another site
started, whatever its method (a `SameSite=Strict` cookie, or `Sec-Fetch-Site`), and the route
ceiling (4) leaves reveal out of a host's table unless it says otherwise.

### 8. One writer per cache root

The monitor holds a lock on its cache root, in `main.rs`, and publishes its port beside it so a
second monitor hands off to it. A host given the monitor's root would corrupt the record logs, or
be handed off to.

**Build:** the claim moves into a library call both use, and its note says whether the holder is a
monitor that can be handed off to. A host takes a root of its own.

## The order

Each lands on its own, with the monitor working at every step:

1. The rule for a host's sign-in (7), in the contract.
2. The finished reply and the body limits (5), used by the monitor's own listener first.
3. The route ceiling (4), the monitor serving every route.
4. One configuration for a mounted table: the stores (1), the probes switch (2), the state (3) and
   the root claim (8). The monitor builds its own from today's defaults.
5. A test host that mounts the table under `/agents/monitor/`, with stores of its own, the probes
   off, its own request facts and reads only. The contract tests (the route index, every documented
   row answered) run against it.
6. The mount base in the page (6).

## Left for later

- **Several readers in one table.** A request names no person: one hide list, one set of grants and
  one lockout per table, and a file's stamp works for any reader of it. A host serving several people
  needs a filter on who sees which session, and hides per reader.
- **A host's own transcript format.** The adapters are a fixed registry, so a format this
  repository does not read cannot join without a change to the facade crate.
- **Writes for a host whose agents run elsewhere.** They act on the serving machine. Reaching agents
  on another machine is a relay to a monitor beside them, as the native app does
  (`design/monitor-hub.md`).
