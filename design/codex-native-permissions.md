# Codex native permissions and process lifetime

This stage ships only the `claude-monitor-codex` library. Neither Monitor binary
links it or exposes an activation control yet. The crate is a default workspace
test member so ordinary Cargo gates exercise it. Later HTTP/UI wiring must keep
construction, session reading and ordinary Write mode inert until explicit native
activation. Do not start a runtime simply because its endpoint is configured.

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
symlinks, before native project operations. A home-directory root grants broad
project scope. Pairing does not grant arbitrary shell access or bypass another
thread's pending-request identity checks.

## Monitor-to-server authentication

On first explicit activation, Monitor generates an independent random 256-bit
credential. It publishes `codex-server.token` atomically beside the receipt
database in its private state directory (normally `~/.local/state/agent-monitor`).
The directory is created with mode 0700 even under umask 002; an existing real
directory is tightened to 0700 only after checking its owner. The file is 0600.
Temporary names use an independent random nonce, never credential bytes. The
fully written file and the containing directory are synced before publication
returns, including when a concurrent creator supplied the winning hard link.
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

The supported transport remains loopback WebSocket. A private Unix socket would
also prevent another account squatting the port, but requires a Unix transport
adapter and an explicit supported-runtime contract. Revisit it before supporting
mutually untrusted local accounts: TCP loopback authentication does not authenticate
the server to Monitor, and a malicious local listener could impersonate the 401
challenge. Same-user processes with access to this credential remain trusted.
Codex App sharing is not supported until its client can present this credential;
there are no unauthenticated App launcher instructions in the runtime response.

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
`NativeRuntime::new` anchors a relative receipt path to the Monitor's current
directory, so the token-file argument stays absolute when the child changes cwd.

## Disconnect and recovery

Only attached threads are restricted from ordinary sending. Normal disconnect
verifies native idleness and empty queues without holding the lifecycle lock
across RPC. A short final lock rechecks connection/event/action generations before
releasing ownership; the worker join happens outside that lock. Status and exact
ownership checks remain available during slow probes. Runtime startup/shutdown
transitions are serialized separately from its state mutex.

A deleted project directory does not prevent an idle check. If native reads fail
or the server disappears, an explicit force disconnect releases the gateway
without sending an interrupt, killing a process or replaying an action. It refuses
known active turns/approvals while connected and all in-flight HTTP actions.
When disconnected, cached active flags are stale; force disconnect accepts that
execution may still exist elsewhere. The host must explain this uncertainty and
require an explicit recovery action. It does not claim the native work stopped.

On Unix the authenticated server uses `setsid` and survives successful Monitor
disconnection/exit. Every failed startup cleans the entire newly owned process
group and reaps its direct child, including launcher-shim descendants. A server
from an earlier activation or another process is never killed by that cleanup.

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

Capacity recovery is deferred to the HTTP/UI wiring stage, not implemented in
this library revision. Before exposing native writes, that stage must surface a
warning before either identity store reaches its cap and provide a tested operator
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
