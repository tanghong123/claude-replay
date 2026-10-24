# Codex native permissions and process lifetime

The optional `codex-native` feature wires `claude-monitor-codex` into
`agent-monitor`. The crate is a default workspace test member so ordinary Cargo
gates exercise it. Construction, session reading and ordinary Write mode remain
inert until explicit native activation. Configuring an endpoint does not start a
runtime or enable pairing. Retiring Monitor entrypoints do not add native controls.

## Caller wiring contract

The HTTP host must install Monitor's `AuthGate` before reading request bodies and
register `with_body_limit("api/codex/send", SEND_BODY_LIMIT)` for encoded images.
The constant alone does not override the HTTP server's ordinary 64 KiB limit.
`Gateway::route` trusts the gate's `Request.authenticated` and `origin_ok` facts;
it checks them and the method before parsing. Never synthesize either fact from
browser input. Every read requires pairing and Origin; mutations require POST too.

The host must apply the same gate before `subscribe`, `activate`, `deactivate` or
`force_deactivate`. These Rust methods are not HTTP authentication boundaries.
Authorize the event subscription before connecting it to an SSE writer. Slow
subscribers are disconnected at 128 events; reconnect requires an authoritative
snapshot, not `Last-Event-ID` replay. Thread-less native notifications are denied
by default. Resolution of a pending request already owned by this gateway forwards
only its request ID. No account, login, search or standalone-command data is broadcast.

Before installing any host logging backend, cap the `tungstenite` target and all
its child targets at DEBUG or below, regardless of a global TRACE setting. Version
0.29 logs the serialized handshake at TRACE, including Authorization; marking the
header sensitive does not redact those bytes. Reject logging configuration that
can override this target cap, or filter those records before any sink. The wiring
stage must verify this with a synthetic credential and captured logs before
enabling native connections. This library installs no logger and cannot enforce
the host's logging configuration; sanitized gateway errors alone are insufficient.

## Authority

The owner pairs a browser using Monitor's existing credential. Every paired
client has the same native authority; there are no per-device roles. Native sends
need no additional passcode. The switches express intent, not another credential.
A paired client can operate only the configured canonical project roots and
attached thread/request identities: browse, create/resume, send text/images, change
advertised settings, manage queued input, interrupt, and answer pending requests.

The host chooses the executable, Codex home, endpoint and allowed roots. Browsers
cannot replace them. Monitor canonicalizes execution directories, including
symlinks, before native project operations.
Without `--codex-allowed-root`, the allowed root defaults to `$HOME`.
A home-directory root grants broad project scope. Pairing does not grant arbitrary shell access or bypass another
thread's pending-request identity checks.

## Monitor-to-server authentication: loopback WebSocket

On first explicit activation, Monitor generates an independent random 256-bit
credential. It publishes `codex-server.token` atomically beside the receipt
database in its private state directory (normally `~/.local/state/agent-monitor`).
The directory is created with mode 0700 even under umask 002; an existing real
directory is tightened to 0700 only after checking its owner. The file is 0600.
On Unix, the directory is opened with O_DIRECTORY and O_NOFOLLOW. Ownership
inspection and permission changes use that descriptor; token reads, temporary
creation, linking, removal and directory sync are relative to the same handle.
Replacing its pathname with a symlink cannot redirect these operations. The host
must still choose a trusted state directory and ancestors for SQLite and the
server's later token-path lookup; this is not a sandbox for arbitrary paths.
Temporary names use an independent random nonce, never credential bytes. The
fully written file and the containing directory are synced before publication
returns, including when a concurrent creator supplied the winning hard link.
Directory fsync returning EINVAL or ENOTSUP is tolerated for filesystems without
that operation; other errors and all token-file sync failures remain fatal.
Such filesystems do not provide a confirmed power-loss durability guarantee for
the directory entry. A crash can leave a private 0600 temporary token file.
Automatic stale-file cleanup remains deferred: an unlocked sweep could delete
another creator's live temporary file. The wiring stage must establish exclusive
initialization/cleanup ownership and compatibility with older writers before
adding cleanup; never remove the canonical token or rotate it as cleanup.
Concurrent starts reuse the published value. Existing malformed, symlinked or
non-private credentials fail closed. The credential is not the pairing token.
Gateway status/UI responses and its own diagnostic strings omit it; dependency
logs also require the host logging cap above. It is never a setup step or user
prompt. Unpairing does not rotate it: app-server loads it at startup.

Monitor starts app-server with `--ws-auth capability-token --ws-token-file <path>`.
Every WebSocket connection, including reconnect, first requires a 401 for a
request without credentials, then sends `Authorization: Bearer <credential>`.
An open server is refused before native initialization. TCP listener probes do
not establish authentication. Handshake failures report status without headers
or response bodies, which could echo the credential.

The 401 challenge alone does not authenticate the server to Monitor: any account
may bind a loopback port above 1023, so another user's listener holding the
configured port could answer it. So the credential goes out only on a connection
whose server end belongs to Monitor's own OS user, read for that connection after
it is made and before the request is written (#s145). On Linux this is the `uid`
of the server's row in `/proc/net/tcp` or `tcp6` (a dual-stack server's v4
connection is in `tcp6`); on macOS it is the process that `/usr/sbin/lsof`, run as
this user, finds holding the server end, and as this user `lsof` sees no other
account's sockets. Until the server accepts, no process holds its end: `lsof`
does not list it, and Linux lists it with inode 0, whatever uid the row shows,
which counts as no owner. So the owner is read again for up to 5 seconds, until a
process holds the end; the first holder found is the answer, since an accepted
socket's owner does not change. Another user's or an unreadable owner is refused
without sending anything, and a platform with no lookup refuses at once. After a
refusal Monitor waits 15 seconds before it connects again (a server that is not up
yet is tried again after 250 ms), so a port another account holds costs a lookup
now and then. A server root holds is another user's unless Monitor itself
runs as root. Same-user processes with access to this credential remain trusted;
on macOS that includes one of this user's processes holding a server end another
account passed it. Codex App sharing through this
transport requires an App that can supply the Bearer header; an endpoint override
alone does not establish that support. This authenticated WebSocket is the default
transport. App-sharing setup is offered only for an explicitly selected Unix
socket; neither the native credential nor its file path is part of a setup prompt.
Authentication failure never triggers an unauthenticated retry.

## Monitor-to-server authentication: private Unix socket

On macOS and Linux, explicitly selecting `unix://` uses
`$CODEX_HOME/app-server-control/app-server-control.sock`; an absolute
`unix:///path.sock` selects a different socket. An explicit CLI endpoint takes
precedence over `CODEX_NATIVE_SERVER_URL`; existing WebSocket configurations remain
valid. Unix failure never selects WebSocket implicitly. This carries WebSocket
HTTP Upgrade and frames over a Unix stream, retaining the same RPC, receipts,
approvals and reconnect rules. It needs
a Codex CLI that supports Unix listeners. Monitor does not weaken WebSocket
authentication when Unix is selected and does not create a native Token file.

Before every connection, Monitor checks the advertised endpoint belongs to its
effective OS user and is a socket or symlink. Codex's macOS rendezvous alias may
resolve to a short physical path. The resolved socket and its physical parent
must belong to the same user and have no group/other permissions (normally 0600
and 0700). Canonical ancestors must be owned by that user or root and must not be
writable by other users, except root-owned sticky directories such as `/tmp`.
The connected peer UID must match via `getpeereid` or `SO_PEERCRED`. A second path
inspection checks device/inode identity after connecting. An invalid, replaced,
public or dangling endpoint is refused, not unlinked or chmodded by Monitor.

This authorizes an OS user, not particular executables. Other processes belonging
to that user and privileged processes remain trusted. A native App connecting
directly to the socket has Codex's native authority; Monitor's allowed-root scope
only governs requests through its HTTP gateway. Browser pairing, Origin checks,
method checks, project scope and approval identity checks remain mandatory for
both native transports. Browsers never connect directly to the Unix socket.

Monitor can attach to an existing native daemon. Explicit **Start and enable**
uses the existing owned foreground-listener lifecycle with `--listen unix://...`,
without TCP auth flags. It does not stop or manage another daemon. App transport
selection is version/configuration dependent; verify actual attachment instead
of assuming that opening the App connected it to this socket. No automatic App
launcher or permission-changing migration is included.

The setup prompt asks the coding agent to prepare a user-run terminal command
or local launcher, without executing it or stopping any App/server from the chat.
The generated launcher must let the user save work and confirm any restart,
preserve unrelated instances, and verify the actual shared connection. A copied
prompt is preparation guidance, not proof that a compatible launcher was produced
or that Codex App has switched transports.

## Codex policy and process context

`codex app-server` owns execution, sandbox and approval policy. Monitor does not
install a second sandbox or relax Codex policy. Answering an approval is a paired
owner's explicit action. The ordinary writer retains its existing terminal-consent
and passcode rules.

Startup selects the first configured allowed project root as cwd and explicitly
sets `CODEX_HOME`, `CODEX_APP_SERVER_FORCE_CLI=1` and Standard service tier. The
child environment is cleared and retains only HOME/USER/LOGNAME/PATH/TMPDIR,
SSH_AUTH_SOCK/SHELL/TERM,
LANG/LC_ALL, SSL_CERT_FILE/SSL_CERT_DIR, HTTP(S)/ALL/NO_PROXY (both spellings), and
OPENAI_API_KEY when set. Custom provider environment variables are not implicitly
inherited; their support needs explicit host configuration before enablement.
Preserving SSH_AUTH_SOCK grants app-server access to the user's SSH agent,
including its available signing/authentication operations; it is an explicit
capability of the host execution context, not merely a display preference.
Both `NativeRuntime::new` and `Gateway::start` require an absolute `state_path`
and reject relative paths before filesystem operations. The host must select a
dedicated private state directory, not infer one from the current working
directory or point it at a project checkout. Existing callers supplying relative
paths must choose that explicit location; there is no automatic cwd migration.

## Disconnect and recovery

Only attached threads are restricted from ordinary sending. The native toggle's
`deactivate` transition fences new requests first, closes the gateway worker and
releases its subscriptions with one three-second deadline. In-flight delivered
RPCs become uncertain; they are never replayed. The transition then drains local
response handlers before clearing ownership. Background reads, pending approvals
and active sessions cannot veto the user's explicit close. Worker/handler drain
is bounded at 15 seconds; a failed drain keeps the fence and can be retried.

If this runtime holds the child handle from activation, it stops the owned process
group with TERM (three seconds), then KILL (three seconds), and reaps the child.
Cleanup failure retains the handle for retry and is reported. History and already
delivered messages are preserved. Stopping this shared process interrupts its work
and disconnects other clients. Turning off Native, or Write with Native enabled,
refreshes ownership and requires an explicit shutdown confirmation for a
Monitor-owned process. The gateway status includes known running thread/turn
identities from its canonical snapshots and notifications; confirmation names
those turns. Active or pending gateway work also requires confirmation before
detaching an external server, whose work keeps running. This observation covers
threads attached to Monitor, not unrelated work invisible to its connection.
Cancel and Escape leave the server and both modes on.
An unavailable ownership check never silently proceeds with shutdown.
Monitor handles SIGTERM, SIGINT and SIGHUP on its main thread. After successful
activation, its server stays running across host exit and upgrades, including
active turns. Before activation succeeds, Monitor atomically saves and syncs a
0600 ownership receipt beside its state database: process ID/group, start identity,
executable diagnostics and endpoint/home/scope configuration. It holds a private file
lease while managing that process, so another Monitor cannot claim it concurrently.
On the next explicit activation, the configured transport must authenticate first
(the existing bearer token for WebSocket; private Unix permissions and peer UID
for explicit Unix mode). The receipt, live PID/group/UID/start identity (and boot
identity on Linux) and configuration must also match before the new Monitor reports
the server as owned. Executable paths and file metadata are diagnostic only: a
package upgrade can unlink or replace the old binary while its process and turns
continue. Missing executable paths do not discard a verified live ownership lease;
process identity and endpoint authentication remain required.
Missing/unsafe receipts, reused PIDs and mismatches leave the server external; they
never authorize a signal. Records contain no token contents.

Explicit mode-off and abnormal runtime destruction still stop the owned server;
startup failures still clean up their child. Orderly host shutdown only closes the
gateway and releases the ownership lease. After the first terminal signal, default
signal behavior is restored for installed handlers, so a second signal can end a
slow drain. Inherited ignored signals remain ignored, preserving nohup and
background-shell behavior across installation, cleanup and handler removal. No fixed
20-second exit deadline is promised. SIGKILL cannot run cleanup, but an already
persisted receipt can be verified on restart; interrupted startup before its receipt
was persisted still needs operator recovery. Old versions leave this new receipt
unused and see a surviving process as external. Do not remove the state directory
while a server is retained; losing its receipt/token prevents safe reacquisition.
Independent servers remain external: Monitor only closes its own connection,
never signals them, removes writer locks, or edits Codex session storage.

The standalone Gateway's `disconnect` and `force_disconnect` APIs retain their
non-interrupting contract for other hosts. Runtime startup/shutdown transitions
are serialized separately from status reads. Failed startup still cleans the
entire newly owned process group, including launcher-shim descendants.

## Durable receipts and compatibility

Receipt schema `user_version=1` stores bounded status/IDs and error summaries,
never conversation snapshots or queued input. Initial action responses may still
include fresh snapshots; retries return compact receipts and the browser must
fetch current history using the retained thread ID. Claims use an immediate SQLite
transaction and a busy timeout so concurrent Monitors cannot deliver one action
ID twice. Different payload/endpoint fingerprints still conflict.

Terminal receipts are retained for 30 days from completion, then deleted from the
active table. Their action ID/fingerprint tombstones remain: an expired retry
returns `receipt_expired` (HTTP 409), never a new delivery. Uncertain receipts and
queue-steer bindings are not expired automatically. Each identity store is capped
at 100,000; new claims fail closed at capacity. Do not automatically rotate/delete
this database to bypass the cap. Reconcile native history before any operator-led
retirement. This bounds growth without weakening duplicate-delivery protection.

`Gateway::receipt_usage()` returns a typed `ReceiptUsage` with action identities
(active plus retired), queue-steer identities, the per-store limit of 100,000,
and `warning_at=90,000`. `should_warn()` is true at or above that threshold in
either store. Counts come from one read-only SQLite snapshot; they do not prune,
claim, or issue native RPC. This explicit method may wait for a receipt operation;
ordinary `status()` remains independent of database I/O. The host must call this
method after activation and completed actions and periodically while activated,
report observation failures rather than inventing remaining capacity, and show
a persistent warning whenever `should_warn()` is true. Counts can change after
the snapshot; transactional claim limits remain authoritative.

Warning UI and capacity recovery are deferred to the HTTP/UI wiring stage, not
implemented in this library revision. Before exposing native writes, that stage
must consume the usage interface above and provide a tested operator
recovery path that preserves rejection of old action IDs and queued submissions.
Stop all writers and reconcile uncertain deliveries before any operator operation;
deleting tombstones, replacing the database or extending the cap without such a
protocol is not a supported recovery path. The current hard cap remains a known
operational limit. No automatic expiry is added here because it could turn an old
retry into a second native delivery.

Opening a legacy version-0 database transactionally compacts its response payloads,
adds retention timestamps, sets the version and reclaims old snapshot pages.
Future schema versions are refused. Stop older Monitor writers before migration;
version-0 binaries lack a version guard and must not share or reopen the upgraded
database. Same-version concurrent writers are supported. Before enablement, retain
a consistent backup for rollback with all writers stopped; restoring old receipt
state after new native actions risks duplicate delivery and requires reconciliation.
No production Codex store is read or migrated by this library.


## Browser delivery recovery

The native controller stores drafts (including ready images), exact send payloads,
action IDs and outgoing previews in this browser origin's IndexedDB before a send.
It scopes records by the configured endpoint, Codex home and allowed roots; each
page session's draft is separate. Browser Duplicate Tab may copy that page-session
identity and its draft; independent tabs receive different identities. A single
read/write transaction makes identical unresolved
sends from two tabs share one action identity. Storage failure prevents a new send.
Clearing site data or using another browser loses this local recovery evidence;
it does not undo a delivered message or remove the gateway's SQLite receipts.

On reload, an in-flight send is shown as uncertain. Checking its receipt uses the
paired, same-origin GET `/api/codex/receipt?actionId=...`; this lookup never claims
an identity, connects to native RPC, or executes a send. Missing or expired receipts
remain unresolved, including after replacement of the gateway's receipt database.
Matching text in newly observed JSONL history is presented as evidence to review,
not proof that this specific action succeeded. For uncertain attempts, image-only
and initially unread history remain explicit review cases. After inspecting the conversation, the user
can confirm retirement of an uncertain attempt without sending anything; a later
intentional send gets a new ID and may duplicate a previously delivered message.
The server never replays its stored uncertain receipt.

Accepted previews are retired after history or queue reconciliation, or oldest
first once 32 accepted previews are retained. This includes image-only messages
and conversations that are no longer open. Only unresolved attempts count toward
the 64-attempt send limit; their recovery identities are never silently evicted.
Drafts from other page sessions expire after seven days without an edit when the
journal is next used. Legacy drafts receive that grace period on first read. The
current page session keeps its draft; expiry never deletes unresolved attempts.
Browser storage may contain message text and images; it contains no native or browser pairing tokens.
This storage is native-only and does not change the read-only export format.

## Native phone layout

On sideways phones the composer starts as one input line. Tapping it expands the
model, attachment and send controls; a successful send or outside tap folds it
again without discarding the draft. Portrait keeps the full resident composer.

The resident native composer keeps reading tools behind a 44px **Reader tools**
button in the top bar. The existing dock is hidden until that button is opened;
its search, filter and pane controls are reused rather than duplicated. Focusing
the composer closes those tools. This differs from the ordinary phone reader's
always-visible dock and remains a product design choice for maintainer acceptance.
Portrait and landscape Chrome tests cover the composer, touch targets and dock;
these do not establish iOS Safari keyboard, sticky-hover or Home Screen behavior.
Pinch zoom leaves viewport panning to the browser; keyboard compensation applies
only at scale 1 (within the existing 0.01 tolerance).
