# Retiring the monitor's classic view

> **BUILT**: the tests moved (#s96) and the notice shipped (#s97), both in v1.376.0; the classic
> view and `agent-monitor-v2` were removed in v1.417.0 (#s98); `/session`, `PageChrome` and
> artifacts mode (#s99) and the documents (#s100) followed in v1.419.0.
>
> **DECIDED** (#s95, 2026-10-13). The owner asked to deprecate the classic view, and to
> investigate and design the process first. This is that design, with the owner's decisions in
> §8: one notice release, then removal; `agent-monitor-v2` retires; the tmux attach command is
> dropped, not ported; `/session` is removed. CLAUDE.md said the classic page "goes when the app
> shell has been validated, and not before"; §5 is what "validated" means, measured.

## 1. Two meanings of "classic"

The word means two different things in this repository, and the deprecation touches only one.

- **The monitor's classic view.** `agent-monitor`'s rail page (`claude-monitor/src/rail.html`:
  a session list beside the session view in an iframe) and `agent-monitor-v2`'s splice shell
  (`claude-monitor-v2/src/shell.html`). They are reached through the app shell's "Classic"
  switch, a remembered choice in `<state_dir>/ui.json`, or `?ui=classic`. **This is what
  retires.**
- **`export.js`, the html crate's session page.** It is what the classic view shows inside its
  iframe or splice. It is also the page of `agent-replay --html`, `--dump-html` and
  `--dump-all-html`, and the REFERENCE the browser scenarios hold the app shell to. **This
  stays.** Nothing here proposes changing how offline exports look or work.

Most uses of "classic" in the tests (`Surface::Classic`), in CLAUDE.md's harness sections and in
the viewport design docs mean the second. They survive.

## 2. What the classic view is made of

Removed with it:

| Piece | Where |
|---|---|
| v1 rail page (728 lines), its `{{VERSION}}`/`{{SHARED}}`/`{{PAIRED}}` template and Home Screen tags | `claude-monitor/src/rail.html`; `claude-monitor/src/main.rs` (`RAIL_TEMPLATE`, the substitution) |
| v1's session arm, which adds `chrome=embed` for the rail's iframe | `claude-monitor/src/main.rs`; `routes.rs` `SessionArm` |
| v2 splice shell (471 lines), `splice()`, `empty_shell()`, `stem_of()`, its on-miss store lookup | `claude-monitor-v2/src/shell.html`, `claude-monitor-v2/src/main.rs` |
| The route table's classic branch, `ClassicPage`, `/api/ui` | `claude-monitor/src/routes.rs` |
| The shell preference: `Shell`, `ui.json` read/write, `?ui=` resolution, `{{DEFAULT_UI}}` | `claude-monitor/src/ui.rs` |
| The app shell's "Classic" switch and links pinning `ui=app` | `claude-monitor/src/codex-ui/app.js`, `page-head.html`, `production.css` |
| `claude_replay_html::shared_inline_all`, called only by the two classic substitutions | `claude-replay-html/src/lib.rs` |

Used only by the classic hosts, and removable in a later clean-up (§6, stage 4):

- **Host chrome in the html crate.** `PageChrome`'s `embed` and `theme` are sent only by the v1
  rail; `artifacts`, `host_search` and `host_inset` only by the v2 splice. Their body attributes
  and CSS (`#embed-title`, `body[data-shell]`) go with them.
- **`export.js`'s artifacts mode** (`fileview`, `openArtifact`), reachable only through the v2
  splice or `/session?artifacts=1`. The app shell has its own preview pane.
- **Two shared exports with no other production reader**: `stateTip` (`shared/state-labels.js`)
  and `composeCopy` (`shared/control-protocol.js`).

Not removable, though only the classic pages read them in this repository: the `/api/sessions`
fields `pid`, `term`, `sock`, `subs`, `child` and `hasTerm`. Tools outside the repository read
that API (a pid is how one finds a session's process), so the fields stay.

## 3. What stays

- **`export.js` and every offline export**, unchanged: `--dump-html` (`build_html`),
  `--dump-all-html` and `--html` (`build_shell`). The ⧗ viewport history, the ↑ parent button and
  the Tasks panel are export.js features, not monitor features, and stay with it.
- **The test reference.** The 104 `classic_page_*` scenarios, the cases that loop over both
  surfaces, and the sandbox all open export.js on the html server, not through the monitor. They
  keep holding the app shell to the same reference. §7 lists the few cases that do reach
  export.js through the monitor.
- Not `/session`: the owner chose to remove it (§8, D5).

## 4. Who still uses the classic view

Measured on the maintainer's two machines on 2026-10-13:

- One has no `ui.json`, so it gets the default, the app shell. The other remembers `{"ui":"app"}`.
- `agent-monitor-v2` is not built by the release workflow and is installed on neither machine.
  Its own Cargo.toml calls it "a separate app, kept for side-by-side comparison".
- On a phone the app shell hides the Classic switch. The classic view has no phone layout, and
  opening it on a phone shows the desktop rail with the session pushed off the screen.
- Nothing in `scripts/`, `integrations/` or the other crates opens `?ui=classic`.

Users of the public tap cannot be measured. Anyone who chose classic has a remembered
`{"ui":"classic"}` and sees it on every visit.

## 5. What the classic view does that the app shell does not

| Gap | Where it lives | Proposal |
|---|---|---|
| Copy the tmux/screen attach command for a session's pane | `rail.html` (the row's `>_ tmux` chip) | **Dropped** (owner, D3) |
| Compose from any row in the list | `rail.html`, `shell.html` | **Drop**: the app shell composes into the session open in the view, with write mode on. That is the deliberate design |
| All / Active / Idle pills on the legacy `state` | `rail.html` | **Superseded** by the bucket filter (#202) |
| Untitled tasks listed | `export.js` task panel | **Superseded** in the monitor (#217). The panel stays in agent-replay's pages |
| Headless workers as rows | `rail.html` | **Superseded** by the starter's Agents pane (#s53) |
| A tool filter that hides non-matching blocks | `export.js` | **Superseded** in the monitor by search by kind (#367). It stays in agent-replay's pages |
| Row cap with "show N more", the "↳ live" chip, own + sub cost split, strip collapse | `rail.html` | **Drop** |
| One search box for the list and the transcript | `shell.html` (v2 only) | **Drop**: v2 is unreleased, and the app shell has ⌘K |

`design/monitor-shell-parity-audit.md` (v1.129.1) is stale: its items 9, 10, 12, 13 and 14 are
closed in the app shell. It is marked superseded by this table.

With the owner's D3, nothing in the table is ported: every row is superseded or dropped. That is
what "validated" means here.

## 6. The stages

**Stage 1: move the tests. No user-visible change.**
- Move the tests that reach export.js through the monitor (§7) to the html server.
- Move the node contract out of the v2 crate (§7).
- Ship it with an ordinary release.

**Stage 2: notice.** One release in which the classic view says it is going.
- The classic pages carry a one-line notice at the top, with a link to the app shell.
- The release notes say so too.
- The Classic switch stays, so nobody is stranded mid-release.
- The owner chose this release (D1).

**Stage 3: removal.** Everything in §2's first table goes, and `agent-monitor-v2` with it (D2):
- `ui.json`: a remembered `classic` is read as `app`.
- `?ui=classic` serves the app shell.
- `/api/ui` answers 404.
- `--help` loses its "TWO INTERFACES" paragraph.

**Stage 4: clean-up of the html crate.** The `/session` route (D5) and with it all of
`PageChrome`, artifacts mode, and the two dead shared exports (§2), each checked against
`agent-replay --html`, `--dump-html` and `--dump-all-html` before it goes. It is separate from stage 3 because it touches the page offline
exports use: the byte gate must stay byte-identical, or be re-baselined line by line.

**Stage 5: documents.**
- CLAUDE.md: "The monitor's two shells", the switch, `{{SHARED}}`, the Layout section's "rail
  beside an iframe".
- README's monitor section.
- `design/monitor-v2.md`, `design/claude-monitor.md` §6.3 and
  `design/monitor-shell-parity-audit.md` are marked superseded, not deleted, since the history
  explains the code.
- The deck under `docs/` and any copied `--help` text elsewhere.

## 7. The tests

The reference does not move (§3). What changes:

- **The #174 rendering audit's 9 reference cases open export.js through the v2 splice**
  (`rendering_audit.rs`, `open`). They move to the html server, the page the scenarios already
  use as the reference. The audit keeps measuring the same page; only its host changes.
- **Three scenarios** reach export.js through the v2 splice:
  - `classic_page_resumes_after_a_server_restart`;
  - `both_shells_open_a_file_from_the_session_s_own_scratch`;
  - `both_shells_open_a_file_from_the_session_s_job_workspace`.

  They move to the html server, or keep only their app-shell leg where the server is the point.
- **Two `files.rs` legs** run on the v2 splice: `a_file_a_shell_command_changed_opens_from_its_note_on_both_pages` and `a_phone_is_never_offered_the_file_manager_on_either_page`. Their classic legs move to the html server.
- **Classic-only cases are deleted** with what they test:
  - in `browser_follow.rs`: `the_v2_shell_keeps_the_document_scroller`, `the_compose_affordance_appears_only_once_paired`, `the_classic_rail_clusters_a_family_and_hides_and_restores_a_row`, `the_classic_rail_composes_through_the_shared_protocol`, `the_classic_lightbox_reveal_carries_the_stamp`;
  - the v1 legs of the favicon and Home Screen icon cases;
  - the harness helpers only they use (`Stores::qoderwork_family`, `Stores::claude_finished`; `classic_view_state` has no callers already).
- **Non-browser tests** that assert the switch, `ui.json` or the rail: `ui_contract.mjs`, `ui.rs`, `routes.rs`, both binaries' `main.rs` tests, and `serve.rs`'s `/session` chrome test. They change with the code they hold.

The node contract lived in the v2 crate (`claude-monitor-v2/tests/ui_contract.mjs`). It moved to
`claude-monitor/tests/` in stage 1 (#s96), in its own commit, since v2 retires (D2).

## 8. Decisions for the owner

Each has a recommendation.

- **D1. A notice release, or straight removal?** **Owner: one notice release.** *Recommendation was the same.* Public
  users cannot be measured (§4), and a notice costs one release. If the owner prefers speed,
  stages 2 and 3 merge.
- **D2. `agent-monitor-v2`: retire the binary, or keep it as an app-shell-only second binary?**
  **Owner: retire it** in stage 3. *Recommendation was the same.*
  - It is unreleased, and its role was side-by-side comparison with classic.
  - Its app shell is v1's, through the shared route table.
  - Browser cases that spawn a v2 monitor move to v1. That includes the rendering audit before
    its move to the html server, and any case written for an open merge request.
- **D3. Port the tmux attach command to the app shell?** **Owner: drop it.** *The recommendation
  was to port it as a Runtime row in the Info pane, since nothing else replaces it.*
- **D4. Compose from any list row?** *Recommendation: drop* (not put to the owner separately). The app shell composes into the
  open session under write mode. That is the deliberate model and the safer one.
- **D5. `/session`: keep or remove?** **Owner: remove it** in stage 4, with all of `PageChrome`.
  *The recommendation was to keep it plain, as a documented public route
  (`design/claude-monitor.md` §6.3) for embedding one session's view.* Removing it is a change to
  a public route, so the notice release (stage 2) names it too.
- **D6. The v1 rail's small features** (row cap, live chip, cost split, strip collapse)?
  *Recommendation: drop*, as §5 says (not put to the owner separately).

## 9. Follow-up tasks (queued with the decisions)

1. Move the rendering audit, the three splice scenarios and the two `files.rs` legs to the html
   server, and the node contract out of the v2 crate (stage 1). No product change.
2. The notice release (stage 2): the classic pages say they are going.
3. Remove the classic view and retire `agent-monitor-v2` (stage 3), one release after 2.
4. Remove `/session` and `PageChrome`, artifacts mode and the dead shared exports (stage 4),
   byte gate held.
5. Documents (stage 5).
