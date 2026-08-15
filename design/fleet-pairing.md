# #196 — Pairing & auth for claude-monitor and the fleet

> **Partly built.** D3b (§4, same-user loopback gate) shipped in v1.78.0 — the
> smallest security-relevant slice, worth landing ahead of the rest. The proxy (D2),
> token/pairing (D3a), `--listen` and the relay remain PROPOSED. How `claude-monitor` and
> `claude-monitor-fleet`
> become reachable from a phone with paseo-grade friction (one QR, no accounts, nothing
> exposed until asked) while corp assets never make a single new outbound connection.
> ONE auth layer serves both binaries — they already share the same HTTP server
> (`claude_replay_html::spawn_listener`), so the gate is designed and built once.
> Borrowed principles from the paseo study (2026-08-15, `getpaseo/paseo`, AGPL — ideas
> only, no code); topology and constraints are this repo's own. Companion:
> `design/monitor-fleet.md` (the fleet as it exists), `design/claude-monitor.md` §11
> (the exposure rule this design amends when — and only when — enabled),
> `design/agent-states.md` §7 (the state stream a phone client would love), DESIGN.md
> #195 (write actions — deliberately OUT of scope here).

## 1. The goal, and the constraint that shapes everything

Open the fleet page from a phone, away from every desk, and see what every machine's
agents are doing. The constraint: the dev servers live on a corp network. They may not
be able to reach an arbitrary external relay, and whether they CAN is the wrong
question — session content leaving the corp boundary through a third-party relay is a
policy conversation nobody needs, because the corp leg already has an authorized
transport.

**Decision D1 — two tiers; corp assets are never touched.**

```
phone ──(tier 1: tailnet or relay)──▶ primary Mac: fleet page + tunnels
                                          └──(tier 2: existing ssh -L, unchanged)──▶ corp monitors
```

Tier 2 is today's fleet, byte for byte: ssh tunnels the fleet already owns, opens, and
re-opens (#24). Corp machines keep exactly one path — ssh — and make zero new outbound
connections; their monitors stay loopback-only, forever. Everything in this design
happens on tier 1, between PERSONAL devices: the phone, the Mac running
`claude-monitor-fleet up` — and, in the single-machine case, a personal Mac running
plain `claude-monitor` (§4.1): the same pairing and the same gate, no fleet required.

## 2. What paseo got right (the principles being borrowed)

| Principle | Their form | Our form |
|---|---|---|
| Keypair as identity, no accounts | daemon Curve25519 keypair under `~/.paseo` | fleet keypair/token under the fleet's own config dir |
| ONE artifact bootstraps everything | `paseo daemon pair` → QR/link `{serverId, pubkey, relay}` | `claude-monitor-fleet pair` → QR/link `{address, credential}` |
| Secret rides the URL **fragment** | `https://app.paseo.sh#<offer>` — the fragment never reaches any server | same trick: the pairing link opens the fleet page itself; JS reads `location.hash`, stores the credential, strips it |
| Outbound-only when relayed | daemon dials the relay on 443 | fleet dials the relay (tier 1 only, phase 2) |
| Off until enabled | relay disabled by default | fleet binds loopback exactly as today until `--listen`/`pair` |
| Hosted default, self-host escape | relay.paseo.sh + Elixir relay you can run | no hosted anything: personal tailnet (phase 1) or a self-hosted relay (phase 2) |

## 3. What actually blocks phone access today (the fleet-specific problem)

Not the bind address — the **iframes**. `fleet.html` embeds each machine's monitor as an
iframe whose `src` is `http://127.0.0.1:<tunnel-port>/` (the page's `ENVS`, plus the
live URL updates #24 added). From a phone, `127.0.0.1` is the phone. Binding the fleet
to a tailnet IP without fixing this yields a health strip over a page of dead frames.

**Decision D2 — the fleet becomes a reverse proxy for its own tabs.** New same-origin
routes:

```
/h/<idx>/<anything>   ──proxied to──▶  http://127.0.0.1:<that env's local port>/<anything>
```

- The monitor's pages use RELATIVE fetches throughout (`pull?session=…`,
  `records?…`, `api/sessions`) and poll — no WebSockets — so they work unmodified under
  a sub-path: the browser resolves `/h/2/index.html` → `pull?…` as `/h/2/pull?…`.
- `ENVS` (and the `/api/fleet` per-poll URLs from #24) are emitted as `/h/<idx>/` paths
  whenever the fleet serves a non-loopback client; loopback clients keep direct
  `127.0.0.1:<port>` URLs so the local experience (including "open this tab in its own
  window") is unchanged.
- The proxy target is the env's CURRENT local port — the same field #24's re-opened
  forwards already republish, so a reconnected tunnel needs no client action at all.
  (A nice consequence: the port-preservation dance matters less for remote clients,
  since `/h/<idx>/` never changes.)
- Bounded and boring: streamed request/response copy on the existing tiny HTTP server,
  no header rewriting beyond `Host`, no caching.

## 4. Identity, pairing, and auth — ONE gate, both binaries

**Decision D3 — the auth gate lives in the shared server.** `claude-monitor`,
`claude-monitor-fleet`, and the viewer's `--html` all serve through
`claude_replay_html::spawn_listener`. The gate is one optional configuration on that
listener:

```rust
pub struct AuthGate {
    /// The bearer credential (256-bit, random). None = loopback-only server, no gate
    /// beyond the same-user rule below — for every caller that does not opt in.
    token: Option<Secret>,
    /// The loopback rule: SAME-USER, not same-machine (D3b). Loopback requests bypass
    /// the token only when the peer's UID is provably this process's UID.
    loopback: LoopbackRule, // SameUser
}
```

Checked before routing, uniformly: `Authorization: Bearer` header, or the `paseo`-style
cookie fallback for iframe/proxy/static requests that cannot carry a header. A miss is a
plain 401 with no body worth reading. Built once, tested once, inherited by every
server — including #195's future write routes, which will REQUIRE a stricter posture on
top of it, not a parallel mechanism.

**Decision D3b — loopback bypass means SAME USER, never same machine.** Review caught
the hole in the earlier draft ("loopback always bypasses"): loopback is a MACHINE
boundary, and on a shared dev server every local user can reach `127.0.0.1:<port>` —
today that is a readable monitor, and under #195 it would be "another user sends prompts
to my session". The identity boundary on a multi-user box is the UID, so the gate
verifies the loopback peer's UID:

- **Linux** (the shared-server case — BUILT, v1.78.0): `/proc/net/tcp` carries the
  socket owner's UID per 4-tuple. The gate matches the CLIENT socket's row (its
  `local_address` is the connection's peer end, `rem_address` our listener end) and
  compares that uid to our own euid (`/proc/self/status`). One file read, no subprocess,
  no dependency. `--listen` (P1a) must add the `tcp6` read.
- **macOS / any platform without a TCP peer-cred mechanism** (BUILT): loopback is
  treated as SAME-MACHINE and admitted. macOS exposes no `SO_PEERCRED` for TCP, and the
  `pcblist_n` sysctl is fragile struct-layout FFI not worth a security check's
  correctness (and `netstat`/`lsof` per request — the server is `Connection: close` —
  is too slow). This is a single-user-machine assumption: correct for a personal Mac,
  and hardened for a multi-user Mac by the P1a token gate below. NOT a claim of
  fail-closed on macOS — stated plainly so the posture is honest per platform.
- **The token fallback is P1a, not D3b.** The earlier draft folded a token into D3b so
  the unverifiable case could fail closed everywhere; the shipped D3b is smaller — Linux
  enforces (the real threat), macOS assumes same-machine — and the token lands with
  `--listen`/pairing, where the phone needs it regardless. This keeps D3b a ~200-line
  security fix with no page-JS or cookie machinery.
- **The ssh-tunnel path still costs zero ceremony**: the remote end of `ssh -L` is
  connected by the AUTHENTICATED user's own sshd session process, so the peer UID is the
  tunnel owner's. Your tunnels pass the same-user check; another user's tunnel to your
  monitor port is refused like any other cross-user loopback connect.
- The stricter alternative, recorded for a later phase if wanted: serve on a UNIX
  socket (0600) instead of loopback TCP — the kernel enforces same-user, and
  `ssh -L <port>:<socket-path>` forwards to it directly. Not needed once peer-UID
  verification exists; unix sockets can't serve a local browser without a helper.

This hardening is worth shipping AHEAD of the rest of #196 as its own small change:
today's `claude-monitor` (and any `--html` serve) on a shared machine is a
loopback-readable surface for every local user, gate or no gate.

**Decision D3a — pairing is identical in both binaries.** On first `pair`, the binary
mints and persists the token (0600, at its OWN root: the monitor's beside `ignored.json`,
the fleet's beside its config — R5 discipline). The pairing artifact is a link/QR:

```
http://<tailnet-ip>:<port>/#pair=<token>        (phase 1)
```

The served page's JS sees `#pair=`, stores the token in `localStorage`, strips the
fragment from the address bar, and attaches the credential on every request (header for
fetches, cookie for subresources). `pair --rotate` mints a new token; old devices simply
stop authenticating. The QR pairs a phone with a FLEET or with a SINGLE monitor by the
same motion — whichever page the link points at.

Why a token and not the NaCl keypair in phase 1: on a tailnet, WireGuard already gives
transport encryption and device identity; the token only has to stop OTHER tailnet
devices (a family member's laptop on the same tailnet) from reading the pages. The
Curve25519/NaCl channel earns its complexity only when an untrusted relay sits in the
middle — phase 2, where the paseo shape (offer carries the server's public key; channel
is `nacl.box` Curve25519 + XSalsa20-Poly1305) is adopted as-is, with a vetted library.

**Decision D4 — read-only stays read-only.** Nothing in this design exposes a write.
The monitor is R8 read-only; the fleet adds no mutating routes. (One nuance: the
monitor's hide/ignore toggle IS a mutation of monitor UI state — behind the gate it is
available to a paired phone, which is fine: it touches the monitor's own root, never an
agent.) #195's send-prompt affordances must NOT ride this until they have their own
consent design — the auth here is about who may LOOK.

### 4.2 Multi-user macOS — the token becomes the portable gate (BUILT, v1.79.0)

D3b's macOS behavior (admit loopback as same-machine) assumed a personal Mac. A SHARED
Mac (a dev-team Mac mini) breaks that: every local user reaches `127.0.0.1:<port>`, and
macOS enforces nothing. macOS exposes no `SO_PEERCRED` for TCP, and the two ways to learn
a TCP peer's uid are both bad for a per-REQUEST security check (the server is
`Connection: close`, so the check runs on every poll):

- `lsof`/`netstat` subprocess — measured **~416 ms** per lookup here. A monitor polling
  every 2 s from several tabs would spend most of its time shelling out. Rejected.
- `net.inet.tcp.pcblist_n` sysctl (the no-subprocess native path) — carries the owning
  uid, but as versioned struct-layout FFI (~150 lines) where a layout bug FAILS OPEN.
  A fragile foothold for a security check, against this repo's grain (we hand-rolled
  RFC3339 rather than take a struct-heavy dependency).

Three ways to make a shared Mac safe:

| Option | Mechanism | macOS UX | Cost / risk |
|---|---|---|---|
| **A — native peer-UID** | `pcblist_n` sysctl FFI, matched by 4-tuple | transparent (no token) | ~150 lines version-fragile FFI; a layout bug fails OPEN; must track macOS releases |
| **B — token, portable** (recommended) | a 256-bit secret in a **0600** file; the OS's FILE PERMISSIONS are the same-user enforcement — identical on every platform; the token bridges to the browser | one tokenized URL to bookmark; a plain `127.0.0.1` bookmark's data fetches 401 | ~80 lines (mint/persist + cookie + fragment bootstrap) — but this is P1a's token anyway |
| **C — unix domain socket** | 0600 socket, kernel-enforced peer creds | — | a browser can't `fetch()` a unix socket; needs a per-user TCP↔socket bridge. Reject for the browser surface |

**Recommendation: B, and it SUBSUMES the platform-specific peer-UID paths.** The token's
same-user guarantee comes from `chmod 0600` on the token file — a mechanism macOS and
Linux enforce identically and correctly, with zero fragile code. It is ALSO exactly what
P1a needs for the phone. So the multi-user-Mac requirement is not a new burden; it is the
reason to pull P1a's token forward. The layered gate (already sketched in §4's `AuthGate`)
becomes:

```
admit  ⟺  request carries the valid token
       ∨  peer is verifiably same-user on loopback   (Linux /proc — a transparent convenience)
```

- On a shared Mac: no cheap peer-UID → the token is REQUIRED → a stranger's `curl` 401s.
  This flips D3b's macOS "admit same-machine" to fail-closed, which is the correct posture
  once a token exists to make fail-closed usable.
- On Linux: the v1.78.0 transparent same-user path STAYS — a plain `127.0.0.1` bookmark
  keeps working with no token; the token is the additional way in (remote, phone).
- **Bootstrap (the only real UX cost):** the binary reads the 0600 token at startup and
  prints `http://127.0.0.1:<port>/#token=<t>`. The fragment never reaches the server (nor
  any log); the page JS reads `location.hash`, stores the token, strips it, and attaches
  it (header for fetches, `SameSite=Strict` cookie for iframe/subresource requests). The
  owner bookmarks that URL once; another user who loads plain `127.0.0.1:<port>` gets the
  inert shell HTML but every data route 401s. `pair --rotate` reprints.

**Related friction to name, not solve here:** on a shared Mac the PORT is machine-global,
so two users cannot both bind 2727. Each picks a port (or the tool picks a free one and
prints it) — orthogonal to auth, but the same shared-Mac reality. The per-user cache root
is already separate (`~/.cache/claude-monitor` is per-home), so nothing else collides.

**Decided: B, and shipped (v1.79.0).** `claude-monitor pair` mints a 32-byte
`/dev/urandom` token (0600, mode set at open — no write-then-chmod race), and a paired
monitor requires it wherever the peer is unverifiable (macOS), while Linux keeps the
transparent same-user leg. The token rides `?token=`/`Authorization: Bearer`/`cmauth`
cookie, constant-time compared. **One deviation from the sketch above, and it is
strictly better here:** the bootstrap is a URL token + a one-time **302 → `/` with
`Set-Cookie`** (`SameSite=Strict; HttpOnly; Max-Age=400d`), not a `#fragment` + page JS.
A URL fragment lands in browser history exactly as a query does, so the fragment's only
real wins — not sent over the wire, not in server logs — matter for a third-party RELAY
(P2), never for localhost owner→own-server. The 302 drops the token from the address bar
after one hop, needs **zero page-JS changes** (the cookie is ambient on every same-origin
request, so the rail/session/fleet pages are untouched — the byte gate stayed PASS), and
is a fresh page-load redirect, not the cursor'd pull-loop redirect the cache design
forbids. Verified live: `pair` → 401 for a tokenless peer, 200 with the token (query or
cookie), 302+cookie on the tokened root; unpaired still v1.78.0; a paired monitor still
reads "serving" to the fleet's probe.

Note on the "read-only" framing: the monitor is read-only over agent DATA, but `/__reveal`
opens Finder on the SERVER — one more reason the pre-pair warning (below) matters on a
shared box.

Startup makes the hole un-silent: an UNPAIRED monitor on a platform that cannot verify a
TCP peer (macOS) prints a one-line warning naming `pair`; a PAIRED monitor prints the
tokened URL (the owner's browser re-pairs itself every open) and says "token required".

### 4.1 The single-machine case: `claude-monitor` exposed directly

The fleet is the aggregation story; a personal Mac that just wants ITS monitor on the
phone should not need one. With the gate in the shared server this falls out:

```
claude-monitor --listen tailscale        # bind tailnet IP in addition to loopback
claude-monitor pair                      # QR/link for the rail page, same ritual
```

Same flags, same identity file discipline, same fragment bootstrap — the phone lands on
the rail, and the rail's session view keeps working because it is served same-origin by
the same process. CORP machines never use this: their monitors stay loopback-only and
are reached exclusively through tier 2's ssh tunnels (directly, or proxied by the
fleet). The viewer's `claude-replay --html` KEEPS its loopback-only posture — a
per-session ephemeral server has no pairing story worth its complexity; it can adopt the
gate later if that ever changes.

## 5. The bind, and "off until enabled"

- Default: `127.0.0.1`, no auth — exactly today. Nothing listens beyond loopback until
  the user asks.
- `--listen tailscale` (both binaries) resolves the machine's tailnet IP (via
  `tailscale ip -4`, with `--listen <ip>` as the explicit form) and binds it **in
  addition to** loopback. A non-loopback bind REQUIRES the gate: the flag without a
  minted token refuses to start and says to run `pair` — exposed-and-open is not a state
  these tools can reach. Refuses `0.0.0.0` without an explicit
  `--listen-any-i-understand` — dev-machine monitors on a hotel LAN is not a mistake
  this tool should make easy.
- `pair` (both binaries; fleet also as `up --pair`) prints the QR/link for the current
  bind; with no non-loopback bind it says what flag to add rather than silently
  exposing.

## 6. Phase 2 — the relay (deferred, shaped)

For phone access without any VPN: the fleet dials OUT to a relay over TLS 443 and
maintains the connection; the phone reaches the relay; the relay forwards ciphertext by
server id, learning nothing (E2E NaCl channel keyed by the QR-carried public key —
paseo's exact construction).

**Where the relay lives (the hosting question, answered by D1):** only tier 1 traffic
crosses it — phone ↔ personal Mac — so it is hosted wherever the USER likes with zero
corp involvement:

- a personal VPS (any $5 box; the relay is a dumb ciphertext forwarder),
- a **Cloudflare Worker** — paseo's relay package ships a `cloudflare-adapter`, evidence
  the pattern runs serverless on a free tier,
- or not at all, which is phase 1: a personal tailnet makes the relay unnecessary, and
  is the recommended steady state.

Corp servers never know any of this exists. The one case a relay inside the corp network
would serve — phone without corp VPN reaching corp-hosted state — is exactly the case
D1 already routes through the primary Mac's ssh tunnels instead.

## 7. Phasing and acceptance

| Phase | Ships | Accept |
|---|---|---|
| P1a gate | `AuthGate` in the shared listener + token identity + `pair` QR/link + fragment bootstrap + `--listen tailscale`, wired into `claude-monitor` first (no proxy needed there) | phone on the tailnet scans once, sees THAT Mac's monitor; a second unauthed device sees 401; loopback UX unchanged |
| P1b fleet | the same gate on the fleet + the `/h/<idx>/*` reverse proxy + path-relative `ENVS` for non-loopback clients | phone sees the whole fleet through one QR; corp monitors still loopback-only behind ssh |
| P2 relay | outbound relay transport + NaCl channel + self-host docs | phone with no VPN sees the fleet through a personal relay; the relay host can read nothing |

P1a is deliberately first and monitor-only: the gate lands in the shared server with the
simplest consumer, and a single personal Mac on a tailnet is already a complete,
useful feature. P1b adds the only fleet-specific machinery (the proxy). P2 is a real
project and waits until phase 1 chafes.

## 8. Security posture, stated plainly

- Corp content crosses the corp boundary ONLY over ssh, before and after this design.
- Nothing listens beyond loopback, and nothing answers without the token, until the user
  runs the enabling commands. Pairing artifacts carry the secret in a fragment, which
  never appears in server logs — theirs or anyone's.
- The token gates READ access to session content — treat the QR like a password; rotate
  with one command. Phase 2's E2E channel removes the relay host from the trust set
  entirely.
- The phone client is the browser. No app, no store, no push channel — the #194 state
  stream stays a local file; if phone NOTIFICATIONS are ever wanted, that is a separate
  consumer of `events.jsonl` (the old ntfy pipeline's slot), not this design's problem.

## 9. Open questions for review

1. ~~Loopback bypass on shared machines~~ — **resolved by review (owner's catch): the
   bypass is same-UID, never same-machine (D3b)**; peer-UID verification on both
   platforms, fail closed, ssh tunnels unaffected.
2. ~~Cookie vs header~~ — **resolved (owner): `SameSite=Strict` cookie is the
   mechanism and is sufficient CSRF posture for this read-only surface**; the
   Authorization header stays as the CLI/scripting form.
3. ~~Multiple devices~~ — **resolved (owner): start with one shared token**; the
   identity file format leaves room for a per-device list later.
4. ~~TLS on the tailnet bind~~ — **resolved (owner): skip in P1** — WireGuard already
   encrypts; `tailscale cert` is the escape hatch if a secure context is ever needed.
5. **Base-path audit** — resolved as P1b's first task: verify the monitor pages against
   a real proxied session (absolute `/session?id=…` links in the rail are the likely
   offender — they would need to become relative).
