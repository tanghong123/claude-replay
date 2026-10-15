# Codex native permissions and process lifetime

Codex native is an optional writer inside Agent Monitor. Compiling it into a
release makes its enabling control available. Reading sessions, enabling ordinary
Write mode, or configuring a native endpoint does not activate the gateway or
start `codex app-server`.

## Authority

The owner pairs a browser using Monitor's existing credential. Native reads
require that credential. Native mutations require an authenticated POST and the
existing same-origin check, before the request body is parsed. The Write/native
switches express user intent; a `writeMode` field is not another credential.
Every paired client has the same native authority. There are no per-device roles.

A paired client may explicitly connect to a configured loopback app-server, start
one if absent, list the allowed projects and history, create or resume threads,
send text/images, select supported model/effort/speed settings, manage queued
input, interrupt work, and answer the selected thread's approval/question
requests. Response controls are bound to the native connection and exact pending
request; durable action receipts distinguish retries from uncertain delivery.

The host chooses the executable, `CODEX_HOME`, endpoint and allowed directory
roots through trusted CLI/environment configuration. Browsers cannot replace
those values. Monitor canonicalizes execution directories, including symlinks,
and rejects directories outside the roots. A broad root such as the home
directory deliberately grants a broad project scope. Pairing does not grant an
arbitrary shell endpoint or bypass another thread's request identity checks.

## Codex policy

`codex app-server` owns execution. Its Codex configuration supplies sandbox and
approval policy. Monitor does not install a second sandbox policy or silently
relax Codex's policy. Answering an approval is an action by a paired owner within
that policy, not an automatic approval by Monitor. The ordinary writer retains
its existing terminal-consent and passcode rules.

## Disconnect and recovery

Only threads attached to Monitor's gateway are restricted from ordinary Codex
sending. Unrelated Codex threads keep their ordinary transport. An explicit
native disconnect is serialized with HTTP actions and refuses active turns,
pending requests or uncertain owned-thread recovery. Once idle, it closes the
Monitor RPC worker, releases those restrictions and restores the ordinary reader.
A second paired client observes the same shared gateway transition.

Disconnecting does not terminate an independently running shared app-server,
delete conversation history, or remove durable action receipts. On Unix, a
Monitor-started server uses `setsid` and may outlive Monitor; stopping that process
is a separate owner action. Startup failure may reap only a newly started child
that failed initialization, never an existing shared server or Codex App.

After disconnection, enabling native again creates a new RPC connection and
revalidates history and pending controls. Lost delivery is not replayed as a new
turn. Native mode uses Monitor's existing shared pairing token; it adds no per-device
revocation mechanism or public non-loopback listener.
