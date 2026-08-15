# Design #133 — a compose box that sends input to a session's terminal

**Status: AUTHORIZED (2026-08-15). Owner sign-off given on the §6 sentence — *the monitor
may send input into a live agent session* — with the direction: build BOTH transports
(idle-resume and live tmux), tmux prioritized as the highest value.** Two owner
constraints now stand: (1) tmux injection is offered ONLY for a PROVEN session→pane link
(§3.1), never a cwd guess; (2) an idle session's resume affordance is SUPPRESSED when the
same project has an ACTIVE session — resuming an old idle continuation while live work is
in flight would fork a divergent branch. Building proceeds once the §7 sub-decisions below
are settled; #196's auth is the §3.2 CSRF fix this waited on.

The request: click the terminal icon on a rail row, type a message, and have it arrive in the
tmux session that row's agent is running in.

## 1. What this would cost

The monitor's founding constraint (R8) is that it is **read-only**: it NAMES a controllable
target (`design/session-liveness-probe.md` §3 established that a tmux pane id is a usable
address) and never uses it. Everything else in the monitor — the scan, the counters, the hide
list — is a read or a local UI preference. This feature is the single change that would end
that property.

What arrives in the pane is not a message. It is **input to an agent**, executed with that
user's tools, credentials and permissions. Several sessions on this machine run with
`--dangerously-skip-permissions`; for those, injected text is arbitrary command execution with
no further prompt. The probe's §4 says this plainly and stops there. This document does not
re-argue it; it works out what would have to be true first.

## 2. The mechanism is settled

From the probe's §3, every row tested rather than inferred:

| host | mechanism | works |
|---|---|---|
| tmux | `tmux send-keys -t <pane> '<text>' Enter` | ✅ |
| GNU screen | `screen -S <name> -X stuff '<text>\n'` | ✅ (literal newline; `\r` silently delivers nothing) |
| bare tty | `TIOCSTI` on another process's tty | ❌ `EPERM` |
| bare tty | `echo > /dev/ttysNNN` | ❌ reaches the display, not stdin |

So the feature is only ever offered for multiplexed sessions, and "no terminal badge" already
means "not addressable". Nothing here is the hard part.

One caveat the probe flags and this design inherits: **all 11 agents it measured were inside
tmux, and so were all 8 of mine — but that is "this host's habit, not a law."** On a machine
where agents run in bare terminals the feature is not merely gated, it is impossible: TIOCSTI
against another process's tty is `EPERM`, and the restriction is a deliberate kernel hardening
that must not be worked around. Any UI must therefore treat "no compose here" as a normal,
permanent state rather than an error.

## 3. Four blockers, in the order they must be cleared

### 3.1 The target is a GUESS for most sessions — this is the new one

Measured while building #145/#146, and corroborated by the #99 probe's independent sample:

- **5 of 8 live agents carry no session id in argv** (the probe measured **7 of 11** on the
  same host, months earlier). Launching `claude` without one is ordinary, not exotic — and
  the probe already called this "the real gap".
- Claude **holds no fd to its transcript** (0 `.jsonl` fds across every live agent), so
  there is no exact link to fall back on.
- `claude --resume` with no id opens a **picker** — the user may resume any session in that
  directory, so "the newest session of this cwd" is a tie-break, not a fact.
- Process start time does **not** separate candidates either: in the one genuinely ambiguous
  directory here, *both* sessions had activity postdating the process.

The consequence for this feature is severe and specific: **the pane you would be typing into
is, for a majority of rows, chosen by a heuristic.** Send-keys to the wrong pane does not
produce a harmless error — it delivers your text as instructions to a *different live agent*,
possibly one running without permission prompts, in a different repository.

#146 gives the only sound footing: when a directory has exactly one growing session and one
candidate process, the pairing is forced and then banked. That is a real proof, and it is
already computed. **A compose affordance must be offered only where the link is proven** —
`confirmed: true`, i.e. session id in argv, transcript held open, or growth-proved. Never for
a cwd-heuristic link, no matter how likely. `ambig > 1` must hard-disable it.

This inverts the current UI: the terminal badge is shown wherever a target exists, but compose
would be shown only where the *identity* is established. Those are different questions and the
rail does not currently distinguish them visually.

### 3.2 The wire surface cannot carry a mutation of this weight

`serve_connection` (claude-replay-html/src/html_export/serve.rs) reads **exactly one line** —
the request line — and never parses headers. Therefore, today:

- there is **no `Origin` check** and **no `Host` check** (so no DNS-rebinding defence);
- there is **no CSRF token**;
- the method is not even inspected, so `GET` and `POST` route identically;
- mutations are consequently GET with query params — that is what `/api/ignore?add=` is (#113).

For the hide list that shape is acceptable: the worst a forged request achieves is hiding a
row, and it is reversible. For injection it is not. **Any web page the user visits could fire
`<img src="http://127.0.0.1:2727/api/inject?target=%25 0&text=...">`** and execute text in a
live agent. No amount of UI consent in the monitor's own page prevents that, because the
request never came from that page.

So this feature cannot be added to the current server. It requires, first, as its own task:
a real request parser (method, headers), rejection of any request whose `Origin`/`Host` is not
the monitor's own, a per-session CSRF token minted into the page, and mutations moved off GET.
That work is worth doing on its merits — but it is a prerequisite, not a detail.

### 3.3 Visibility in the target (§4.2) is unimplemented

Injected text is indistinguishable from typed text once it reaches the pane, which is exactly
what makes silent injection dangerous. The receiving session must show that input arrived from
outside and from where.

`tmux display-message -t <pane>` writes to the target's status line **without entering its
stdin**, so it can announce the injection without contaminating the agent's input — the one
mechanism that gives visibility for free. It is **unverified**, and the probe's own history is
the reason to take that word seriously: its first TIOCSTI test measured a process calling the
ioctl on *its own* stdin, which succeeded and read like a green light; only testing from a
separate process revealed the `EPERM` that actually governs. Verify `display-message` from a
*different* process against a *foreign* pane, or do not claim it.

### 3.4 Per-target consent (§4.1) is unimplemented

"I can write to the pane" is a permission fact, not consent. Required: authorisation by that
session's owner, for that target, at that time — not a global setting, not inferred from
filesystem ownership.

A workable shape, if this proceeds: consent is granted per `(tmux socket, pane id, session
id)` triple, expires (both by wall-clock and when the pid changes), is stored at the monitor's
own root beside `ignored.json`, and is revocable from the rail. The pid must be part of the
identity — a pane outlives the process in it, and consenting to "pane %0" must not silently
transfer to whatever occupies it next.

## 4. What is NOT in question

- **Local-only** (§4.3) already holds: the monitor binds loopback. It must stay that way, and
  §3.2's Origin checking is what keeps "loopback" from meaning "any local browser tab".
- **Refuse by default** (§4.4): the default build should not include this at all. A cargo
  feature that is off by default, so a stock binary has no injection code compiled in, is the
  honest expression of "the prototype prints a capability and never performs one".

## 5. Smaller alternatives that get most of the value

Worth weighing before funding the full thing — each is compatible with R8:

1. **Copy the attach command.** Click the badge, get `tmux -L knack attach -t %0` on the
   clipboard. Zero new capability; the user pastes it and is in the session with full context.
   This is most of the ergonomic win for none of the risk.
2. **Focus the pane.** `tmux switch-client`/`select-window` moves the user's own terminal to
   that session. It commands the *viewer's* terminal, not the target's stdin — a materially
   smaller claim, though still a write and still needing §3.2's Origin work.
3. **Deep link.** A `tmux://` style handoff to the terminal emulator, if one is configured.

(1) is implementable today, needs no consent story, and I would ship it first regardless of
what is decided about the rest.

## 6. Recommendation and the sign-off this needs

**Do not build the compose box yet.** Not because the mechanism is unproven — it is proven —
but because two of its prerequisites are unbuilt and one of them (§3.2) is a live weakness in
the current server that injection would turn from "a row got hidden" into "a web page executed
commands in your agent".

Ordered:

1. Ship §5.1 (copy the attach command) — today, R8-safe, most of the ergonomics.
2. Harden the wire surface (§3.2) as its own task, on its own merits.
3. Verify `display-message` visibility (§3.3), tested not inferred.
4. Design and build the consent store (§3.4).
5. Only then, and only for `confirmed` links (§3.1), consider the compose box — behind a
   default-off feature flag.

**Owner sign-off required before step 5**, and specifically on this sentence: *the monitor may
send input into a live agent session.* That is the decision; everything else is engineering.
Until it is given, this document is the whole deliverable.


## 7. Sub-decisions still open (post-authorization)

The dangerous decision (§6) is made. These shape HOW, and each is recorded here as it is
answered:

1. **Feature gating (§4.4).** Compile-time `--features inject` off by default (a stock
   Homebrew binary has NO injection code — strongest "refuse by default", but the owner
   must build/ship an inject-enabled binary to use it) vs runtime-gated in the released
   binary (present but inert until auth + a proven link + consent). *Owner decision.*
2. **Consent model (§3.4).** Per-SEND confirmation (the compose action names the exact
   `pane/sid/pid` and is itself the consent — no persistent store) vs a per-session
   expiring GRANT (grant once, send freely until pid-change/timeout — needs a consent
   store + revocation UI). *Owner decision; recommend per-send for v1.*
3. **Visibility mechanism (§3.3).** `tmux display-message` on the target's status line
   (announces the injection without touching stdin — must be VERIFIED from a foreign
   process first) alone, vs additionally prefixing the injected prompt itself (visible to
   the agent, but pollutes its input). *Recommend status-line-only; owner confirm.*
4. **Build order.** tmux-first (highest value, but needs the most scaffolding: wire
   hardening + visibility + consent) vs idle-first (quicker, fewer prerequisites) then
   tmux. *Owner decision.*

**Step 1 (wire hardening) BUILT, v1.81.0.** The shared HTTP server now parses the METHOD
and reads a bounded POST body, and a route consults a `Request` carrying two write
verdicts: `authenticated` (a valid TOKEN was presented — same-user loopback admits a read
but NEVER a write, so a stock binary cannot inject until `--pair`) and `origin_ok` (the
`Host`/`Origin` are the monitor's own loopback origin — a foreign Host is DNS rebinding, a
foreign Origin a cross-site fetch, both refused). `Request::deny_write()` gates a write on
POST + same-origin + authenticated. No write route exists yet — this is the safe surface
one can be added to (steps 2–4). Applied by default unless overridden: the wire hardening
(§3.2, now built) is a non-negotiable prerequisite; "active session in the
project" (constraint 2) means any session in the same cwd with a LIVE attributed process
(#194 non-idle); both `claude` (`--resume`) and `codex` (`exec resume`) are in scope for
the idle path; the resume-forks-a-new-sid assumption (#195) is verified as the first build
step, not assumed.