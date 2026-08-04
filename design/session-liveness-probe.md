# Session liveness, terminal attachment, and input injection (#99)

> **Prototype findings.** Instrument: `examples/session_probe.rs` — an *example*, never wired
> into the CLI. Measured on macOS 26 (Darwin 25.5.0) against 9–11 live agent processes.
> Read §4 before building anything on §3.

## 1. Is the session running?

Four signals. Two are necessary, two are not.

| signal | how | verdict |
|---|---|---|
| **live process** | `ps` scan, matched on the basename of **argv[0]** | **necessary** — nothing else distinguishes *finished* from *idle* |
| **tree mtime** | `jdi::latest_tree_activity` — root transcript + every child under `<stem>/subagents/` | **necessary** — the only continuous progress signal |
| **in-flight tool** | `jdi::inflight_tool_in_tail` — a `tool_use` with no `tool_result` in the last 256 KiB | **necessary in principle**, unobserved here (see below) |
| root-transcript mtime alone | `stat` | **redundant** — strictly weaker than the tree mtime |

Resolution: no process ⇒ `Finished`. Process + (fresh tree write **or** tool in flight) ⇒
`Running`. Process, quiet ⇒ `IdleAlive`. Measured, the three-way split is real: one session
read 17 s (`Running`), three read 3472 s (`IdleAlive`) — the same `ps`-visible state, told
apart only by the tree clock.

**Why the in-flight signal must stay even though it never fired here.** It exists for the
case the clock cannot cover: an agent blocked in a long tool call writes *nothing* anywhere in
its tree, so mtime ages past any threshold while the session is maximally busy. It is a
correctness signal for a rare state, not a hot-path optimisation, and dropping it because a
sample did not hit it would be a mistake.

### The two process-matching traps

Both are easy to get wrong, and getting either wrong loses agents silently.

- **Do not match argv anywhere.** An agent's own tool shells carry `claude` in their argv;
  matching broadly makes every shell look like an agent. (jdi documents this.)
- **Do not match `comm` from a bulk `ps` listing.** The multi-column form pads and
  **truncates** comm to a fixed width — `/Users/hong/.local/bin/claude` arrives as
  `/Users/hong/.loc` — so every agent launched by absolute path is dropped. This cost the
  prototype 2 of 4 resolvable sessions before it was found. **jdi is not affected**: it reads
  `ps -o comm= -p <pid>` per pid, which does not truncate. Verified both forms.

Matching the **basename of argv[0]** avoids both: it is the executable path (not truncated),
and a tool shell's argv[0] is its shell.

### Mapping a process to its session — the real gap

`--resume <uuid>` in argv is the reliable link, and jdi's `session_id_from_argv` reads it. But
it only exists for **resumed** sessions: **7 of 11 live agents carried no id**, having been
started fresh. Two fallbacks were measured:

- **open file descriptor** (`lsof`): works for **Codex** (holds its rollout `.jsonl` open),
  fails for **Claude** (no `.jsonl` fd — it appends and closes).
- **cwd → project slug**: the process cwd (`lsof -d cwd`) maps to Claude's store directory by
  replacing `/` with `-` (`/Users/hong/code/knack` → `-Users-hong-code-knack`); the newest
  transcript there is the session. Verified correct on three processes.

Neither is exact on its own. A monitor (#98) should use argv when present, then cwd+recency,
and **cross-check the transcript's recorded `session_id`** against the argv id — the probe
prints `[id UNCONFIRMED]` when a filename matched but the head did not.

## 2. Is it attached to a terminal, and which?

`ps -o tty=` gives the controlling terminal (`ttys006`, …; `??` when detached). That alone
does **not** answer the question that matters, because a tmux pane and a bare terminal both
present a real tty. The multiplexer is only visible in the process **environment**
(`ps eww -o command=`):

| marker | meaning |
|---|---|
| `TMUX_PANE=%N` | inside tmux; `%N` is the injection target |
| `STY=` | inside GNU screen |
| tty, neither var | bare terminal emulator |
| no tty | detached / daemonised |

Measured: **all 11 live agents were inside tmux**, none bare. That is this host's habit, not a
law — the bare-tty row below is the one that decides whether a feature is possible in general.

## 3. Can input be injected from outside? — capability matrix

Every row **tested**, not inferred.

| host shape | mechanism | result |
|---|---|---|
| **tmux** | `tmux send-keys -t <pane> '<text>' Enter` | ✅ **works** — target received the text |
| **GNU screen** | `screen -S <name> -X stuff '<text>\n'` | ✅ **works** (screen 4.00.03) — needs a literal newline; `\r` in the shell string silently delivered nothing |
| **bare tty** | `TIOCSTI` on `/dev/ttysNNN` | ❌ **denied** — `EPERM`, same user, same uid |
| bare tty | `echo … > /dev/ttysNNN` | ❌ **not input** — reaches the *display*; the process's stdin never sees it |

**The TIOCSTI result needs stating precisely, because a careless test says the opposite.** A
process calling `TIOCSTI` on **its own** stdin is *accepted* on this macOS. Calling it on
**another** process's tty — the actual question — is denied with `EPERM`. The first probe
measured the former and looked like a green light; only opening `/dev/ttysNNN` from a separate
process showed the truth. Any future work must test the external case specifically.

**Consequence:** injection is a property of the **multiplexer**, not of the terminal or the OS.
Inside tmux or screen there is a supported control channel; on a bare tty there is none, and
the modern kernel restriction is deliberate — TIOCSTI was a privilege-escalation vector
(disabled by default on Linux ≥ 6.2 and restricted on macOS). Do not attempt to defeat it.

## 4. Safety boundary — read before productising

Pushing input into a live agent session is **executing instructions as that user**, in a
session with their tools, credentials and permissions — including sessions started with
`--dangerously-skip-permissions`, as several observed here were. The capability is not made
safe by being convenient.

Anything built on §3 must, at minimum:

1. **Be explicitly authorised per target session**, by that session's owner, at the time — not
   by a global setting, and not inferred from filesystem ownership. "I can write to the tty"
   is a permission fact, not consent.
2. **Be visible in the target.** The receiving session should show that input arrived from
   outside, and from where. Silent injection is indistinguishable from the user typing, which
   is exactly what makes it dangerous.
3. **Be local-only.** No network-reachable path to §3, ever, without a separate and much
   harder security review than this note.
4. **Refuse by default.** The prototype prints a capability, never performs an injection —
   that asymmetry is deliberate and should survive productisation.

This note deliberately reports what is *possible* and stops there. `#98`'s monitor answers
"what sessions exist and what have they done"; this one answers "which are alive and could be
interacted with". Only the first needs no consent story.
