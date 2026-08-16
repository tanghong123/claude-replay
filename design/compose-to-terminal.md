# Design #133 — a compose box that sends input to a session's terminal

**Status: BUILT (2026-08-15), both transports shipped — see the §7 step log for the trail.**
Owner sign-off was given on the §6 sentence — *the monitor may send input into a live agent
session* — with the direction: build BOTH transports (idle-resume and live tmux), tmux
prioritized as the highest value. Two owner constraints held throughout: (1) tmux injection is
offered ONLY for a PROVEN session→pane link (§3.1), never a cwd guess; (2) an idle session's
resume affordance is SUPPRESSED when the same project has an ACTIVE session — resuming an old
idle continuation while live work is in flight would fork a divergent branch. #196's auth is
the §3.2 CSRF fix this waited on; the whole feature is runtime-gated behind pairing (§7.1).

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
the idle path; the resume-forks-a-new-sid assumption (#195) is RESOLVED by reading agent-jdi (no agent
spawn): `claude --resume <sid> -p` **APPENDS** to `<sid>.jsonl` (same id). Proof — jdi's
`pins_session_id()==true`, its `fresh_vs_resume_invocation` test drives `--session-id sid`
then `--resume sid` with the SAME id, `transcript_path→transcript_by_id(sid)` reads that
one transcript on every turn, and `capture_session_id` is Codex-only; jdi's whole Claude
supervision would break on turn 2 if resume minted a new id. Consequence for step 3: the
idle row simply goes ACTIVE (no #142 fork grouping), but the append MUTATES the finished
transcript in place (a later human `--resume` sees the injected turn) — sharpening both the
suppress-when-active rule and a new decision: a headless `-p` resume needs a permission
posture (`--dangerously-skip-permissions`, or it stalls on the first prompt), so idle-send
is an autonomous agent turn, not a chat — OWNER decision: (a) use skip-permissions (chosen). **Step 3a (idle-send backend) BUILT, v1.82.0:** `POST /api/send?target=<sid>` (body = prompt), gated by `deny_write` (POST + same-origin + a token — unpaired binaries can't reach it) then by `resolve_send` (target finished, project quiet, claude/codex only), spawning `claude --resume <sid> --dangerously-skip-permissions -p` (or `codex exec resume`) detached in the session's cwd; the row goes active as it grows. **Step 3b (idle-send UI) BUILT, v1.83.0:** a `✎` compose button on FINISHED claude/codex
rows (paired only — the `{{PAIRED}}` flag) opens a fixed compose bar (separate from the
polled list, so a re-render never clobbers the input); ⌘/Ctrl+Enter POSTs the prompt to
`/api/send` with the ambient cookie, shows the refusal reason inline, and re-polls on
success so the row goes active. The idle transport is now end-to-end usable. **Step 4 (the
tmux slice — the highest-value transport) BUILT, v1.84.0.** A LIVE, PROVEN
(`confirmed`, §3.1 — never a cwd guess), in-tmux claude/codex row is marked `injectable` and
gets the same `✎` compose button; sending POSTs `/api/send`, which the backend dispatches by
liveness — a finished session resumes (3a), a live one injects. Injection requires standing
CONSENT (§3.4, the owner's model-2b expiring GRANT): consent is keyed by the
`(socket, pane, sid, pid)` quadruple — the pid load-bearing, so a restart (new pid) drops the
grant and the owner re-grants — with an 8 h wall-clock backstop, stored 0600 at the state root
(`consent.json`, beside `auth-token`/`ignored.json`) and revocable from the rail. An
unconsented pane returns `code:"no-consent"`, which makes the Send button read "Grant & send"
(one click: `POST /api/consent` then `/api/send`); a consented pane shows a "revoke" link. The
injection itself (verified from a foreign process against a foreign pane) is `load-buffer -`
(prompt on STDIN, never an argv), `paste-buffer -d -p` (BRACKETED — a multi-line prompt is one
pasted block, not N Enter-submitted lines), a single `send-keys Enter` to submit, and
`display-message` on the pane's STATUS LINE (§3.3 — announces the send WITHOUT entering stdin,
so injection is never silent). The whole feature is runtime-gated behind pairing (§7.1 owner
decision 1b): a stock unpaired binary has the code but no token, so every write route 401s.
The §3.1–§3.4 blockers are now all cleared and both transports are usable; #133 is complete.

**Post-ship refinements, v1.85.0** (owner review of the live feature):
- **Constraint 2 now covers BOTH paths.** Injection was suppressed only for the resume path;
  it now also refuses `ProjectHasActiveSession` when the target's project has ANOTHER live
  session — with two live sessions in one cwd we can't tell which drives the project, so we
  refuse rather than pick (consistent with §3.1's never-guess). The shared, pure
  `project_has_other_live` gates both `resolve_send` and `resolve_tmux_send`, and the rail now
  emits `projActive` per row so the `✎` affordance is HIDDEN where the route would refuse it
  (the resume path previously showed `✎` on active-project siblings and only refused on send).
- **The grant step stays, reframed as risk-awareness.** The owner initially read "Grant &
  send" as confusing two-step wording; on reflection its PURPOSE is to make the risk explicit
  — injected text is input to a live agent, run with its tools and permissions (some sessions
  skip permission prompts). So it stays, and the first-time message now says exactly that
  ("Runs in the LIVE agent with its permissions…") rather than only describing the grant's
  lifetime.
- **Enter sends.** Plain Enter submits the prompt (chat convention); Shift+Enter (or
  ⌘/Ctrl+Enter) inserts a newline for a multi-line prompt.

**Optional grant passcode, v1.86.0** (owner proposal). The auth token gates every write, but
on a paired monitor it rides in the browser COOKIE — so at an unlocked, unattended machine
with the rail tab open, the "something you have" is already present and a walk-up could arm a
pane. An OPT-IN passcode adds a "something you know" at the one consequential moment: GRANTING
consent.
- **Set from the terminal only:** `claude-monitor --set-passcode` (no-echo prompt via `stty`,
  confirmed twice; empty clears). Stored as a salted, iterated SHA-256 hash (`sha2`) `0600` at
  `state_dir/passcode` — never the passcode. CLI-only ON PURPOSE: setting it needs shell
  access, so someone with just the open browser cannot RESET the gate it defends.
- **Gates the grant, not the send:** `POST /api/consent` resolves the target first (an
  ungrantable one never prompts), then — when a passcode is set — requires it in the request
  BODY (never the query string). Wrong/absent → `bad-passcode`/`passcode-required` (the UI
  reveals a passcode field and, on `passcode-required`, waits); five misses arm a 30 s
  `locked` window (blunts browser brute-forcing a short code). Once granted, sends within the
  window/pid are unaffected.
- **Honest scope, stated in the code and `--help`:** a speed bump against an OPPORTUNISTIC
  walk-up, not proof against a same-user shell (which can read the token, brute a short code
  offline, or drive tmux directly). It does not cover a pane you already granted before
  stepping away — revoke it, or let the grant expire. Pure `passcode_verdict` (lockout, time
  injected) and `Passcode` (set/verify/clear, no-plaintext) are unit-tested; the `--set-passcode`
  CLI is smoke-tested.