# #196 — Pairing & auth for claude-monitor and the fleet

> **Proposal for review — nothing built.** How `claude-monitor` and `claude-monitor-fleet`
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

- **Linux** (the shared-server case): `/proc/net/tcp{,6}` carries the socket owner's
  UID per 4-tuple — match the peer's entry, compare to our own UID. One file read.
- **macOS**: `netstat -anv` exposes the owning `process:pid` per socket → UID via one
  `ps` lookup. (Or the `pcblist_n` sysctl for the no-subprocess form.)
- **Fail closed**: a peer whose UID cannot be determined is NOT same-user — it needs the
  token. The owner never notices: the binary prints its page URL WITH the `#pair=`
  fragment read from the 0600 token file on startup, so even the fail-closed path is
  one click for the same user (the file's mode is what proves same-user there) — and
  other users cannot read that file.
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
2. **Cookie vs header for the proxied iframes:** the iframe requests can't carry a
   custom header, so the token must ride a cookie for `/h/<idx>/*`. `SameSite=Strict` +
   the token cookie scoped to the fleet origin looks sufficient on a tailnet — is CSRF
   worth more than that for a read-only surface?
3. **Multiple phones/devices:** one shared token (simplest, rotate-all-or-nothing) vs
   per-device tokens (a tiny table, per-device revoke). Proposal: start shared; the
   identity file format leaves room for a list.
4. **TLS on the tailnet bind:** WireGuard already encrypts; browser features that demand
   a secure context (few, on this page) can use `tailscale cert`. Skip TLS in P1?
5. **Does the monitor page need a base-path audit?** The claim "all fetches are
   relative" is from reading `export.js`/`rail.html`; P1a's first task is verifying it
   against a real proxied session (absolute `/session?id=…` links in the rail are the
   likely offender — they would need to become relative).
