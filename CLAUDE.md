# CLAUDE.md — claude-replay

A Rust + ratatui terminal UI viewer (**binaries: `agent-replay` / `agent-monitor` /
`agent-monitor-fleet` since v1.101.0** — crate and repo names keep the `claude-` prefix;
brew installs symlink the old command names). It is **fully testable headless (no TTY)** —
never skip, stub, or defer a feature "because it needs a terminal."

## Work tracking
**The `tasks/` queue is the state of record for pending work** (owner, 2026-08-30; it
replaced `BACKLOG.md`, now a pointer). Design docs argue, issues discuss, the queue tracks
— don't trust a `design/*.md` status header alone, since the tracker exists because those
drift. How the queue is driven is a local-agent concern and lives in the machine-level
`CLAUDE.md`, not here.

**Two habits this repo cares about, because it is the thing that RENDERS the result.**
Both are unrecoverable after the fact, and both were learned from real damage here:

- **Never pipe or redirect a mutation.** `taskq` prints its `##taskq/v1` record as the last
  line of stdout, and that record is how the work reaches the task panel of every agent's
  transcript. `| tail`, `| head`, `> /dev/null` and `2>&1 |` all destroy it. Measured on one
  session: 17 `done` commands, every one piped, 6 records surviving — and 12 tasks still
  rendering as pending when the owner looked. v1.128.0 recovers much of it from the command
  line and from taskq's own prose, but recovery is guesswork where the record was fact.
- **Pass `--description` as a literal, never `"$D"`.** The shell expands the variable before
  taskq sees it, so the task file is right and the TRANSCRIPT holds `$D`. Multi-paragraph
  text in single quotes is fine; a heredoc into a variable is the trap.

## The monitor's page
`agent-monitor` serves ONE frontend, the **app shell** (`claude-monitor/src/ui.rs` +
`src/codex-ui/*`). The classic rail-and-iframe page it once switched with, its `ui.json`
preference, `/api/ui` and `agent-monitor-v2` were retired by #s98 (design/classic-view-deprecation.md
stage 3, after the v1.376.0 notice): `/` and `?ui=classic` both serve the app shell, a `ui.json` left
behind is ignored, and `/api/ui` answers 404. Stage 4 (#s99) took `/session`, the session view
the classic hosts framed, which answers 404 now, with its `PageChrome` options and export.js's
artifacts mode. "The classic page" below means export.js, the offline viewer's page (`agent-replay --html`,
`--dump-html`), which stays and is the browser suite's reference.
**The sidebar head is TWO LINES by construction** (#223): the brand takes the first, the controls
take the second and wrap within it. They never share a row, at any width. Below `SIDEBAR_TIGHT`
(272px) the shell wears `sidebar-tight`, which squeezes the head's padding, the gaps and the glyphs
so all five stay inside the sidebar at its 232px minimum (#210). Nothing is hidden at any width; a
new control in that row is measured against the minimum, not the default, and the case
(`the_app_shell_resizes_the_session_list`) hit-tests every control there rather than eyeballing it.
**On a phone (≤760px, #310 then #313 — or a finger on a phone-sized short side, a phone held sideways,
#s32) the app shell is a reader and a native-feeling app**, the owner's rules: upright the transcript keeps a 24px margin either side (#s55; the Reading menu's Wide
transcript gives it the whole width, a 13px gutter), controls may FLOAT, and controls of
little use on a phone are hidden. The session list is a DRAWER over part of the view (a scrim over the
rest closes it on a tap; `mobile-detail` = shut), opened and closed by a handle fixed at the top left —
a sibling of the drawer in `#app`, because the workspace is isolated under the scrim and anything in
the top bar would sit under the drawer. **The top bar is ONE row and never hides** (#s37,
`design/phone-landscape.md`, the owner): handle slot, the title with the current turn as its SUBTITLE
(`#turnSubtitle`; the transcript's sticky strip is hidden on a phone but still updated, which
`harness::sticky_turn` reads), Info, right pane. The title and its subtitle are ONE target — two
stacked in a 48px row would each be half a finger — opening the session's menu; turns are chosen in
the Turns pane. **The tools float at the foot in
a DOCK** (#s37, the owner's idea, iOS 26's own pattern): Turns, Tasks, Agents and Aa in a glass pill
above the home indicator, search beside it as a circle — the SAME reading and search clusters,
moved between the top bar and the dock across the breakpoint (`placeToolbar`). The pill's glass is
its `::before`: a backdrop-filter on the pill would make it the containing block of Aa's fixed sheet.
While the search is open or holds a query the find pill takes the dock (CSS `:has`, no script)
with a glass ✕ circle beside it that ENDS the search (`#phoneSearchDone`: the query and its marks
go, the keyboard goes down, the toolbar pill returns); the field's own small ✕ only clears it to type
again — one control, one meaning, as an iOS 26 search closes beside its field; it closes the filter
with it. **The filter and the keyboard never share a phone's screen** (#s49, the owner's rule): the
filter opening puts the keyboard down and keeps the box open (`setPopover`, which also reopens a box
closed with its query held); the field taking focus closes the filter; the box closing closes it too;
and a tap outside dismisses the TOP layer only — the filter first, the box on the next tap — so
closing the menu never refolds the pill. The funnel is lit (`.on`, the selected tint) only while a
filter narrows the search, never because its menu is open (the owner). On iOS a tapped button keeps
focus off it, so the cases click the funnel by script, which leaves focus where it was as iOS does;
Chrome would move focus to the button and hide the bug (`a_phone_s_filter_and_keyboard_never_share_the_screen`). While the field has the keyboard the magnifier gives way and the FUNNEL NEVER DOES (#s41:
iOS focuses the field as the search opens, so hiding the funnel while typing hid it always — the
owner found no filter in either orientation). The dock rides the keyboard through `visualViewport`
(`--phone-kb` = the app's height less the visible one; iOS never lifts a fixed bottom element above
it), and iOS also SCROLLS THE PAGE to bring the focused field above its keyboard (322px upright on
iOS 26), which took the top bar and a wide phone's column head off the screen (#s41): pinning the page
back with `scrollTo` fights iOS and jitters, so the app is moved back over the scroll
(`translateY(visualViewport.pageTop)`) and `--phone-top`/`--phone-bottom` are measured against the
app's box — iOS reports an `innerHeight` that shrinks as it scrolls, and a `visualViewport.offsetTop`
equal to the page offset. Held by `a_phone_find_pill_rides_the_keyboard`, which shadows
`visualViewport.height` and `pageTop`. Held sideways in Safari with its tab bar the keyboard leaves
about 16px of the page (#s287, measured on iOS 26.4: Safari's bars 110pt of 402, the keyboard with
its candidate and accessory bars 276), less than the pill, which sits under Safari's bars; the Home
Screen app keeps 126px, the pill and no line above it. Where no line of the transcript shows between
the column's top and the pill, Return also puts the keyboard down (`noLineAboveFindPill`), so the hit,
the count and the steps show; elsewhere the keyboard stays
(`a_phone_held_sideways_puts_the_keyboard_down_on_return_…`). An Enter that commits an input method's
composition does not search.
The transcript's foot is padded by the dock's static height
(`--phone-dock-h`, never a measured value a keyboard would move under a glide), and the search's
landing treats the dock as an obstacle. **The compose box is the dock's third mode** (#s39,
`design/phone-landscape.md` §3.5c, the owner approved the mockup): with write mode on and a writable
session, a ✎ circle (`#dockCompose`) joins the dock beside search in place of the floating compose
button; a tap turns the dock into the reply box — the SAME `#composer` and `ControlStore`, restyled on
a phone (its head and foot `display:contents` in a grid: ✕ at the left, the field growing with its
text to five lines with ↑ inside it, a `.compose-note` above with the box's one line) — riding the
keyboard by `--phone-kb` like the find pill, the toolbar, search and the jump stepping aside
(`#app.composing`). Its ✕ keeps the draft and a dot on ✎ says one is waiting; on a wide phone held
sideways it spans the session's column (`a_phone_writes_from_the_dock_and_keeps_the_draft`).
**The box says where a message goes in ONE line and stays open after a send** (#s236, Hong: "keep the
entry dialog open", "too much info", and it said tmux for a herdr pane): `composeTargetLine`
(`control-store.js`) gives "Types into herdr" (no pane id, #s297, Hong: "just show herdr is enough"),
"Types into tmux pane %3" or "Resumes this session", the desktop's caption (amber for a live pane, beside the session's name alone) and the
phone's note alike, and the reference shell's "Write to this session ·", static "· writes to live
tmux" and inert "＋" are hidden in `production.css` (the generated files untouched). A send that goes
empties the field and keeps the box (and, on the desktop, the focus); one that is refused keeps the
draft; only ✕ closes it. **A resume is watched as it starts** (#s247, `control::watch_resume`): the
agent's stderr is drained and its first 3 s watched (a session it cannot find fails in 0.7 s for
Claude Code, 2 s for codex, measured), so one that stops is the send's answer, its exit status and
the end of what it said, where the page used to say "Resuming the session"; one still running is
left to run and reaped by a thread of its own, its late failure one stderr line (the status, never
its output); the prompt is never sent twice (`a_resume_that_fails_as_it_starts_tells_the_page_why`,
a fake `claude` on the monitor's PATH).
**On a desktop the open box keeps to the session's reading column** (#s245, Hong: it was centred on the
whole workspace, so with the right pane open it lay over the outline, and stood 169px tall over the
transcript's end): `placeDeskCompose` (`app.js`) sets its edges from the transcript's text width inside
its column; it is one caption line (name, target, a pane grant's revoke, ✕; the name gives way first)
over ONE line of field that grows with the draft, at the box's current width, to a cap (a share of the
column) and then scrolls; the transcript's end is padded above it (`--compose-room`), a reader following
the end is kept there (`convergeBottom`) and one scrolled up is not moved, the jump to the end rides
above it. Native mode lays out its own
(`a_desktop_compose_box_keeps_to_the_session_column_and_grows_with_its_draft`).
**A search step knows what covers the transcript's foot** (`searchFloor`, #s248, #s257): the phone's
dock, an open compose box (native mode's is resident) and native mode's runtime panel above it, each
while it is drawn, at any width. A match under one is neither counted as already seen nor left there
by a landing, except at the view's end, where nothing can lift it (a phone's keyboard, a tall
panel): there it counts as seen, or the steps bounce between the last two hits and never wrap.
A landing also keeps the match below the turn bar sticking over a desktop's transcript top, drawn
or faded (`searchCeiling`, #s263): it puts the match a third of the way down the space between the
bar and the cover, at most 120px down. Measured from the column's top, a cover within 93px of it (a
short window with the box open) left the match behind the bar. A step's "already seen" keeps the
column's top: a hit the reader scrolled up behind the bar is behind them, not ahead. Inside a
process group the landing also keeps the match below the group's head (#s267), which sticks at the
top once a match deep inside is revealed: a phone held sideways with the keyboard up leaves about
75px above the find pill, and a third of it put the match behind the 40px head. And the landing never
puts the match so far down that it crosses the cover: in a short space the third is capped at the
space less the match's own height.
A step remembers the hit it landed, and the next step goes on from THAT hit when the current one
is not in sight, while its record still crosses the view, until the reader moves: a scroll within
the intent window of input that came after the landing, or a new query (#s259). The jump stamps
intent itself, and its own scroll used to clear that memory; and a step re-entering from the top
started at the first hit of the unit there. So where a cover reaches the transcript's top (native
mode in a short window) every step landed the same hit again, or fell back inside a unit holding
several. The record's being in view is what catches a move the engine never hears as input (a key
the browser scrolls with natively, the browser's own find): that reader enters at the hit nearest
them. Both read RECORDS, not units (#s264): the re-entry starts at the last record row drawn at or
above the spy's line inside the unit there (`recordIndexAtTop`, the unit's first where none is
drawn), and the landed hit is in view by its own row (its unit's where the row is not drawn). A
process unit groups a turn's calls and can
be thousands of pixels tall, so the unit sent a reader deep inside one back to its first hit, and
kept going on from a hit they had scrolled away from inside it. A filter's landing reads the same
place. A step goes from record to record: a Codex unit holds a record per command, while Claude's
consecutive Bash, Read and thinking are ONE act record (each call a sub-row under the record's own
row; Edit, Write and Skill stand alone), whose hits are one match.
The cases are `a_desktop_search_step_never_leaves_a_match_under_the_compose_box`,
`a_desktop_search_first_step_lands_on_a_first_hit_under_the_compose_box`,
`a_phone_search_with_the_keyboard_up_wraps_past_a_last_hit_under_the_dock`,
`native_desktop_search_step_never_leaves_a_match_under_the_composer`,
`native_search_steps_move_on_when_the_composer_covers_the_whole_column`,
`a_search_step_after_an_unheard_move_enters_at_the_nearest_hit`,
`a_search_step_in_a_short_space_leaves_the_match_below_the_turn_bar`,
`a_search_step_deep_inside_a_tall_unit_enters_at_the_record_there` and
`a_phone_search_held_sideways_lands_a_match_below_its_process_head`.
**Codex native mode's composer is resident** (the owner): once native mode is
enabled, its compact composer stays visible on desktop and phone with no close action;
thread text and image drafts belong to the selected thread. The ordinary writer keeps the
dock's close and reopen, and the generated reference shell is untouched.
**Held sideways the right pane is a second COLUMN** (#s49, the owner): with a wide phone's list shown
it covers the session's area beside the list (the session keeps its width beneath, so nothing
remeasures; the dock goes under it; the handle by the title stays above it, the pane's head making
room), with the list hidden or on a narrower phone the session and the pane sit side by side (the
session at half the width, remeasured once through `paneLayoutChanged`; the dock, its sheets, Info
and the reply box keep to that half), and ⤢ in the pane's head (`.preview-expand`, sideways only)
takes the whole screen and back without disturbing the session beneath
(`a_phone_held_sideways_opens_the_pane_as_a_second_column`). Upright the pane is the whole screen.
**The shell's phone rules for its own top bar are scoped to `.workspace>.topbar`** (#s49): mdrev's
toolbar in the pane is a `header.topbar` too, and `#app .topbar` outranked mdrev's own rules there
(no wrap, the row spread, the shell's padding) — the owner saw the toolbar wrap beside a cut title,
fixed by a later re-measure, upright and sideways (`a_phone_pane_leaves_mdrevs_toolbar_its_own_layout`).
A rule for the shell's chrome that names a generic class (`.topbar`, `.iconbtn`) inside `#app` must
not reach into the guest. **The breakpoint is ONE query**
(#s32, `PHONE_QUERY` in `shared/capabilities.js`: `(max-width:760px), (pointer:coarse) and
(max-height:500px)`, mdrev's own phone test), written in the demo's and production's CSS and read by every
JS check, so a landscape phone is a phone. Its complement is just as strict (#s33): every `min-width` rule means "not a phone", so each of
its queries also carries `(min-height:501px)` or `(pointer:fine)` — a bare `min-width:761px` put a
932–956px landscape phone's drawer back into the flow and pushed the session view off the screen
(`every_media_query_agrees_on_what_a_phone_is` in `ui.rs` reads both stylesheets) — unless it is a
rule FOR a phone held sideways, which is then `WIDE_PHONE_QUERY` exactly (#s37); and the desktop's
collapsed rail (`sidebar-off`) never goes on a phone (`applySidebar`), where its rules emptied the
drawer. **The phone redesign of `design/phone-landscape.md`** (#s37, the owner approved it with
mockups in `design/phone-landscape-mockups.html`) lands in stages. So far: every control and line
keeps inside `env(safe-area-inset-*)` (the island and the rounded corners sideways, the status bar
over the drawer's head upright; `harness::safe_area` emulates them through Chrome's
`setSafeAreaInsetsOverride`, `a_phone_keeps_its_controls_and_lines_inside_the_safe_area`); held
sideways the transcript is a centred reading column (500px, about 75 characters of prose); and a
WIDE phone held sideways (`WIDE_PHONE_QUERY`, ≥900px: the Plus and Max iPhones) shows the session
list as a 280px COLUMN beside the session, its head ONE row of glyphs and no brand (the owner), never
shrunk by a long pane under it (`flex:none`; #s41, it was cut to 11px): the
switcher (Sessions / Turns / Tasks / Agents, borrowing the outline's live lists as the sheets do —
Sessions wears a conversation glyph, never the sidebar glyph, which every desktop reads as "hide the
sidebar": the owner tapped it to hide the column and called it dead, #s41), then
the sidebar's own (Collapse all, Expand all and the session filter go while a pane is shown: they act
on the list alone); the pill keeps only Aa beside search — and takes the panes back while the column
is hidden, or nothing would reach them. The handle sits at the session's top left, by
the title (as iPad apps keep their sidebar button, and wearing its sidebar glyph there; ☰ is the
drawer's), and hides and shows the column under its own key
(`am-phone-column`; never the rail's, never `mobile-detail`); hidden, the session stays at its
reading width, and the Reading menu's Wide transcript (offered upright too since #s55) widens it. Between the phone and 1180px an open preview covers the whole session area beside the sidebar
(it was a 72vw overlay over a sliver nobody could read); `a_phone_held_sideways_gets_the_phone_layout_in_one_row`
and `a_narrow_window_s_preview_covers_the_session_rather_than_leaving_a_sliver` hold both. The dock
never wraps (#352, `a_phone_dock_keeps_one_row_with_a_query_and_a_chip`): the CLOSED box with a query
is its count, steps and ✕ — its chips (a scope chip is its letters alone, a tool chip wears a wrench;
the word visually hidden, #353) wait in the open box. There the chips and the field are ONE strip
(`.search-field`, wrapped at load) that scrolls as a whole, the field as wide as its text, so the end
being typed stays in view; the count hangs as a tag ABOVE the find pill at its left; and the step
arrows are SVG, centred. A tool chip is its wrench and the tools' own single-character
selectors (`o(B)` reads "B", #355), and beside chips the field shows no placeholder. **The query is
ONE prefix at the start of the box** (#367, `design/in-session-search.md` §9): letters u a t o w
then a colon, `o(…)` naming tools by letter (`o(BR)`) or name (`o(Bash,Read)`); `scope:`, `tool:` and
the letters b, r, e stop working (owner). The letters are a UNION and `o(…)` narrows only the tools
class (`uo(B): x` is your turns or Bash calls), so nothing can contradict — #356's greying, clash
label and filter intersection are gone. Common tools have RESERVED letters (`shared/search.js`
`RESERVED_TOOLS`: B Bash, R Read, W Write, E Update, A Agent, S Skill, Q AskUserQuestion, F WebFetch)
never given to another tool; the rest are per session. The filter popover's tools are checkboxes
nested under its Tools row (the list moved up at load; the Tool types heading hidden). A step brings
the match into view on BOTH axes (#354): outputs do not wrap on a phone, so a hit far along a line
sat past its block's right edge while the vertical landing was exact (the owner's export: every
step inside one 175,539 px agent process, at the right offset) — `landOnCurrentMark` now scrolls
the mark's own horizontally scrolling block (its `scrollLeft`, never the transcript's offset). The outline column does not
exist there: each pane opens from its icon as a SHEET rising from the pill that holds the outline's OWN
live list (moved in while open, back after), so one renderer and one click handler (`navigatorClick`)
serve both; while one is open the session DIMS behind it (#s55, `.pane-scrim` under the dock, so the
dock's icons still switch panes; a tap on the dim closes the sheet and is swallowed, never reaching a
link beneath — the dim keeps catching until that tap's click, counted from the LIFT, since on Linux the
harness supplies the click half a second late and a slow phone may too). Info hangs at `--phone-top` (the bar's measured bottom) and ends above the dock; the
dock's sheets (panes, Aa, the filter, the suggestions) rise from `--phone-bottom` (the dock's measured
top) and stop 72px short of the bar upright, the band `phone_tap_outside` taps to dismiss them. Held
sideways each is a POPOVER at its control's side (#s41), never a band across the screen — a band ran
under a wide phone's column and hid every label ("Aa menu in landscape shows a black page"): the
toolbar pill's (Aa, the panes) at the pill's end (left on a narrow phone, right on a wide one), the
find pill's at its right by the funnel, Info under Info; there the height is what is short (330px in
Safari with its tab bar), so they may rise over the top bar
(`a_wide_phone_held_sideways_keeps_its_sheets_beside_the_column_and_its_head_whole`). The jump to the
latest sits at the bottom CENTRE of the session (#332): just above the dock upright, ON the dock's
row sideways, where the middle of that row is free (the owner: "should move to the bottom"), and
gone while the find pill holds the dock (#s41). Inputs are 16px (iOS zooms into anything
smaller), targets 44px — a reading row's whole width flips its switch, a facet suggestion is a 44px
row, a delivered file a 36px chip (at 14px WebKit's tap adjustment gave its tap to the row's head,
which folded the row), and the jump is offered only where something is below. **On a touch screen hover reveals nothing** (#s41): WebKit spends a tap on a hover that SHOWS
content — a session row's action fading in from nothing — and sends no click, so choosing a session
from an iPhone's drawer took two taps (the first tap's `touchend` came and its click never did); the
action stays hidden there (`@media(hover:none)`), and since iOS leaves `:hover` on whatever was tapped
last, a glyph button takes its tint from `aria-expanded` rather than hover, and every control a phone
keeps on screen after a tap whose resting background is clear (the search box's steps, clear and chip
✕, the funnel, fold heads and toggles, Aa's steps, the preview's tabs, outline and sidebar rows, the
session title) has its hover tint cleared on a touch screen (the owner saw ↓ keep a grey shade). Chrome's touch emulation
models neither, so these are walked on the iOS Simulator. Choosing a session shuts the drawer at once and a veil (`sessionLoading`, every
width) dims the session area with how much of the records has arrived. The rules are `production.css`
under `#313`; the cases are `a_phone*` in `browser_follow.rs` and the preview pinch in `files.rs`,
driven by `harness::phone` (emulated BEFORE the first load, since the page stores its outline
default), `harness::pinch` and `harness::chrome_with`. `harness::finger_tap` delivers what a phone
does for a tap — touch, then the compatibility mouse events and a click — on Linux too, where
Chrome's synthesized tap stops at `touchend` (#366: two cases were red on every CI run for it). **Gzip** (#313, `serve.rs` `gzip_for`) is for a
client ACROSS A NETWORK only — a `Host` that is not the loopback, as `tailscale serve` hands the
monitor — so the local desktop is byte-for-byte what it was; the case
`a_phone_over_the_tailnet_reads_the_session_gzipped` reaches it as `phone.test`. That same `Host` is
why the gate must know the tailnet name (#331): `origin_ok` (the DNS-rebinding and cross-site guard
every file read and every write asks) knew only the loopback, so a PAIRED phone was told "reading
local files requires pairing". The monitor hands the gate `pair_phone::trusted_hosts(port)`: the
names this machine's `tailscale serve` relays to THIS port, plus any `AGENT_MONITOR_TRUSTED_HOSTS`
lists — never a `*.ts.net` wildcard, and only a relay the tailnet alone reaches that serves the monitor
alone (#s170, the owner: no setup opens it to Funnel, and nothing else is served at its name and port,
across the background and every foreground serve; another app there would share the monitor's origin). No reply waits on `tailscale` (#s151): the names are read in the
background from startup and again on a miss at most every 30 s, each read bounded at 3 s and a failed or
timed-out one keeping the names before it; only a check in the first 2 s waits, for the first read. And the
verdict is asked only for a request a route answers — a refusal or a public file goes out without it. On Linux the gate
also admits a VERIFIED same-user loopback peer without the cookie (D3b, `/proc/net/tcp`); macOS cannot
verify one. `AGENT_MONITOR_TOKEN_ONLY=1` turns that leg off for a paired gate — stricter only — and
the browser suite sets it on every paired monitor, so a case's unpaired browser is refused on Linux
as on macOS (#366). The pairing
cookie is named for the token it carries — `cmauth-` and eight hex digits of the token's SHA-256
(`cookie_name`, #350) — because a cookie is scoped to the HOST, not the port: while every gate wrote
`cmauth`, agent-metrics' `serve --phone` (this gate since #317, a token of its own) replaced the
monitor's cookie and the owner was told the monitor was not paired. The legacy `cmauth` is still read (admitting only on this gate's token) and is
answered with the gate's own. It is `SameSite=Strict`, which a browser WITHHOLDS from a navigation
that did not start on the site (a link in another app; the owner met it on a phone after saving a
file, #341), so a refused PAGE load gets `HttpResponse::refusal_page`: it reloads once from itself
— same-site, so the cookie comes along — bounded by a 10 s sessionStorage stamp, then shows the
host's refusal words; fetches keep the plain 401, and each refused page load is one stderr line
(never the token), which names a legacy `cmauth` that did not match apart from the gate's own.
**The page saves to a Home Screen as itself** (#320): the tab icon's green mark, square and opaque
(`claude-monitor/src/icons/`, rendered from `app-icon.svg` by `scripts/render-app-icon.sh`), named by
the app shell's head with a manifest and the label "Agent Monitor"; iOS also asks the root
for `apple-touch-icon.png` on its own. Those files are the ONLY paths
the gate answers unpaired (`ui::PUBLIC_ASSETS` → `AuthGate::with_public`, GET only), because a phone
fetches the icon itself and may not carry the pairing cookie. The manifest says `display: browser`,
which on iOS 26 decides nothing (#s111, measured on the 26.4 Simulator, a `browser` and a `standalone`
manifest each on two hosts): Add to Home Screen offers "Open as Web App", on by default, for either;
the app gets a COPY of Safari's cookies for its host at that moment (none on `localhost`) and keeps
its own from then on. So a phone paired in Safari before the page is saved opens the app paired, and
one saved first, or whose cookie later goes, pairs inside the app, where `/pair` is in its scope.
**Sessions sort into three buckets** (#202, `design/agent-states.md` §10): active (busy),
blocked (a wait, or an idle reason that cut the work short — this is what "needs attention"
means and counts) and idle (`done`, `exited`); `sessionBucket` in `shared/state-labels.js` is
the one definition, held to the tracker's enum by a test, and a turn that ended with an answer
is idle, not blocked. **A wait the transcript cannot show is read from the client** (#s76): Claude Code
2.1.296 keeps a pending call — a question, a command awaiting permission, a plan awaiting approval —
out of the transcript until it resolves, so the pending-tool rules read such a session as Thinking;
it records the wait in `<claude home>/sessions/<pid>.json` (`status: waiting`, `waitingFor`: `input
needed` or `permission prompt`), which `client_wait` reads for a session with a live process, found
by the `sessionId` it names (the newest file speaks), a permission prompt in plan mode being the
plan's approval. **herdr's own status decides where herdr can see** (#s238, Hong: "herdr's status is very
reliable, since the agent's process runs inside it"): herdr reads each pane's agent off the screen
it draws, by the rules of its detection manifest (the title's spinner is `working`, a dialog's form
`blocked`; `herdr agent explain <pane>` names the rule that matched), and its Claude hook only names
the session, at SessionStart. For a live session in a herdr pane that `herdr agent list` names as that pane's
session, its status sets the STATE over the derivation's inferences, and the derivation keeps only
the reason and detail that agree (`herdr_decides` in `state.rs`: a tool stays a tool; a derived
stall under herdr's idle is done; a workflow fleet stays busy, since its members run off herdr's
screen). A wait the derivation OBSERVED (a pending question or plan, the client's own wait, #s76)
stands whatever herdr says, so a dialog herdr's rules miss never hides a session that needs a
person; `unknown`, no entry, or another session named there leaves the derivation alone. herdr is
asked on a clock of its own (`HerdrReader`: one asker thread per server, an `agent list` every 2 s
while a tick has wanted that server in the last 30 s, an answer standing for 10 s), so a tick never
waits on herdr and an answer is never a page poll old; an answer asked BEFORE the transcript's last
write is not used, since the newer evidence wins (a question answered, a prompt submitted after
herdr looked). A wait herdr saw publishes at once, and the row's `stateSource` says `herdr` or
`derived`. The app shell's session filter (a glyph in the sidebar's head-actions row,
between Expand every group and the sidebar collapse, opening Active recently / Blocked / Idle /
Include hidden checkboxes; Active recently and Blocked by default, remembered, never an empty
set; "Everything" checks the three and leaves Include hidden alone) filters by a COVER of that partition: Active recently is six hours of
activity or busy now (`RECENT_SECS`; #202 set an hour, and #s253, the owner, "make it 6 hours", after
an idle session an hour and five minutes old dropped out of view; the checkbox reads "Active in the
last 6 hours") and overlaps Blocked by design (`sessionFilterBuckets`). It replaced the
Needs attention and Show Hidden controls.

**Which process a session is linked to** (#269, `index.rs` `link()`) decides liveness AND whether
the compose box appears, and Claude Code 2.1.278 changed the shape it has to read: the session's
engine runs detached in a `claude --bg-pty-host … -- … --session-id <uuid>` and what sits in the
tmux pane is a `claude attach <8-char prefix>` CLIENT; `claude daemon run` supervises and INHERITS
the client's `TMUX_PANE`, and `bg-spare` ptys wait pre-warmed. The rules, each measured: a session
is named by a whole argv TOKEN (`--session-id`, `--resume <uuid|path>`, `attach <prefix>` when
exactly one known session starts with it) and never by a substring — a background job's scratchpad
path carries its session's uuid, and `argv.contains(sid)` once "confirmed" a session onto that
`bash`, detached, while the agent sat in a pane; among processes naming one session the
best-HOSTED wins, so the pane's client beats the engine; helpers (`daemon`, `bg-pty-host`,
`bg-spare`) are never directory candidates and read as detached whatever pane they inherited; and
**a lone agent process in a session's directory is paired, confirmed** — the owner's rule
(2026-09-23), amending the probe's "never a cwd guess": with one process there is one pane, and the
label self-corrects on the next append. Two processes in one directory stay a pick (`ambig`). A
process is a candidate only for sessions of its OWN agent (#s59, `serves`): a Claude pane beside a Codex
pane in one repo had the Codex process (the lower pid) linked to the Claude session, unconfirmed and
unwritable; an exe the monitor cannot place (an extra pattern) stays a candidate for any agent. The send
gate's "another live session in this project" counts the same agent only, for the same reason.
**A one-shot (`claude -p`/`--print`) is headless and pairs by TIME** (#s50): knack's coordinator
starts workers from its Bash, all in one repo, each inheriting the coordinator's `TMUX_PANE`, and the
directory rule gave a process to the newest session only — so a worker deep in a long command lost
its process and read "exited with Bash pending" until its next write. A one-shot starts a NEW
session (entrypoint `sdk-cli` in its head) whose first record lands within a second of the process
(measured 0.7–0.9 s; `ps -o etime=` gives the start), so `pair_one_shots` gives each the earliest
unclaimed one-shot session of its cwd that began in its window (3 s before, 60 s after), confirmed
unless another one-shot could have taken it instead; a paired session is neither the directory's
newest nor a sibling, so the coordinator keeps its own pane. A one-shot's prompt is in its argv, so
only `--session-id`/`--resume` name it; and whatever linked it, its terminal is Detached — a send
into the pane it inherited would type into the coordinator. **A `-p` in an interactive launch's
prompt makes no one-shot** (#s301): `ps` joins argv with spaces, so `claude … "… mkdir -p counts …"`
read as one, and its session was never linked (no liveness, no send, no dialog). The process's own
record decides where it exists (`apply_session_files`): Claude Code writes `<claude home>/sessions/
<pid>.json` (the home from the process's `CLAUDE_CONFIG_DIR`, else `~/.claude`) with `entrypoint`
`cli` for an interactive launch and `sdk-cli` for `claude -p` (measured, 2.1.312; both say `kind:
interactive`), believed only when it names the pid and began no earlier than the process. A one-shot
writes it only once under way, so until then the argv decides, the safe way.
**A headless worker lives in its starter's Agents pane, not in the session list** (#s53, the owner:
"keep them under agents outline pane, and don't put them on main session list"). Claude Code exports
its own session id into every Bash command (`CLAUDE_CODE_SESSION_ID`, with
`CLAUDE_CODE_CHILD_SESSION=1`), so a worker's ENVIRONMENT (`apply_env`, `ps eww`) names its starter
while it runs; its transcript never does. `bank_started_by` (before `assemble`) records the edge for
a ONE-SHOT session only — an interactive session started from an agent's Bash is the owner's own —
and keeps it in `<cache_root>/started-by.json`, so a restart or a finished worker keeps it; a one-shot
session in another session's scratch (#373) is that session's worker with no sighting at all. A row
carries `startedBy` only when the starter is a row here, and `brief` (the first line of its first
prompt: a worker's own title is its project's). The app shell leaves such rows out of the tree and
its counts, lists them in the starter's Agents pane after its sub-agents (`data-worker-session`, the
same descent, so the parent control leads back; a linked worker falls back to `startedBy`), and the
send gate does not count a session's own worker as "another active session". A fork's engine (`--fork-session --resume <parent.jsonl>`) is the only marker of a Claude fork
anywhere, and `note_forks_from_argv` is how one joins its #142 family. Diagnose against the
machine, not the API: `term`/`injectable` describe the process the row was LINKED to, so a false
link reads exactly like a correct refusal — `tmux -L <sock> list-panes -a` and `ps -axww` first.
**The scan's probes are bounded and read paths whole** (#s266, from collab #s298): each `ps` and
`lsof` gets 15 s (`run_bounded`), after which the scan goes on without that source — `lsof -p` once
sat 210 s in the kernel on Chromium's network service and the page on "Scanning sessions…" — and
while one program's abandoned probe still runs no other starts, since a child stuck in the kernel
cannot die; and `ps` joins argv with spaces, so an absolute first word naming no file is joined
back until it names one (`exe_path`): ChatGPT's `…/Codex Framework.framework/…/Codex (Service)`
had read as `codex`, an agent.
**A session in a herdr pane is sent to through herdr** (#s226, Hong's move from tmux to herdr in
stages, agentdev-setup-hong#s127; herdr 0.9.5 measured on aries-black by its steward). The agent's own
environment names the pane (`HERDR_PANE_ID`, like `w1:p2`) and its server (`HERDR_SOCKET_PATH`, which
herdr sets in every pane; `~/.config/herdr/sessions/<name>/herdr.sock` for a named session). One
multiplexer inside the other's pane hands its children the OUTER pane's markers too, in BOTH
directions (herdr started from a tmux pane gives every herdr pane `TMUX_PANE`; tmux started in a herdr
pane gives every tmux pane `HERDR_PANE_ID`), and typing at the outer pane reaches whatever sits there
now — so a herdr marker is believed only when herdr IS the nearest multiplexer up the agent's parent
chain (`apply_nesting`, one `ps -axo pid=,ppid=,ucomm=` when any agent carries a herdr marker: a
pane's shell is the herdr server's direct child, measured; `ucomm` because `comm` is the start path
cut at 16 characters). The tmux server is known by the pid in `TMUX`, screen or zellij nearer than
herdr is a pane we do not drive, and with no answer the agent is in no pane we may drive. The row says `term: "herdr"`, `target` the pane, `herdrSession` a named session
(the attention report reads `term`, agreed with Aries). The send (`PaneAddr`, `resolve_pane_send`)
keeps every tmux rule — a proven link, the project's only live session, consent keyed to the pane and
pid (a herdr key's socket half is `herdr:<socket>`, so a tmux grant can never count) — and then
one rule tmux does not have: **a grant follows a herdr RESTORE of the same session** (#s234, Hong's
option 1 on #s233; herdr starts it again as `claude --resume <id>`, a new pid): `consent::carried`
keeps it when only the pid changed, the server and pane (read from the new process's own
environment, believed only with herdr nearest) and the session id are the same, the process names
the session in its own argv (`names_exactly`: `--resume <id>` or `--session-id <id>`, never a bare
id, an `attach` client, a fork, a one-shot or a directory pairing), and the original expiry has not
passed (never extended). The carry is worked out on every send and never written back, so a send
only reads `consent.json`, and the row's `consented` asks the same (`covers`) — and then
`control::herdr_send` checks what tmux never did, for a CLAUDE session only (herdr's hook reports
Claude's session; a Codex row in herdr is not offered the send, `HerdrClaudeOnly`): the pane id must
be herdr's shape and the socket a socket of this user (both come from another process's
environment); every herdr step addresses that one server by `HERDR_SOCKET_PATH` alone (the monitor's
own `HERDR_*` removed), within 5 s; `herdr agent get` must name THIS
session (no hook: the refusal says `herdr integration install claude`); `agent_status: blocked` or
the client's own wait (#s76) holds it; and so does the input box read from `herdr pane read --ansi`
— the LAST pair of `─` rules with `❯`+NO-BREAK SPACE on its first line — unless everything in it after
the prompt is blank or DIM (the suggestion an empty input shows; a draft, its second line included,
is not dim, and a colour is not dim: `sgr_dim` skips `38;2;r;g;b` and its kin whole; a permission
dialog replaces the box, so no box holds too). The agent is asked once more right before typing; the
prompt goes over the socket (`pane.send_text`, one JSON line, its reply's id matched: `pane
send-text` takes the text only as an argv), with a CR made a newline and every other control
character dropped (`herdr_text`: tmux's bracketed paste keeps them inert, herdr's was not measured); the pane is read AGAIN (still this session, no dialog,
text in the box, or no Enter — a dialog takes Enter as its first choice); and one `herdr pane
send-keys <pane> enter` submits. Whether every character in the box is ours is not read back (a long
prompt may show as a paste), and a dialog can still open in the moment before typing, as with tmux; a multi-line prompt arrives as one
draft. A held send answers `{"ok": false, "code": "held", "error": <what to do>}`, which the page
shows as is. **Images go to a Claude session in a herdr pane, and nowhere else** (#s237, Hong: "if
agent-monitor detects that the session is managed under herdr, we should also allow sending images
to the session"). herdr carries no image itself (its socket has no image, paste or clipboard
method), so the monitor does what a terminal paste does, measured on herdr 0.9.5 and Claude Code
2.1.310: an image file's path sent as a BRACKETED paste (`ESC[200~<path>ESC[201~` over
`pane.send_text`) becomes `[Image #N]` in Claude's input, and on Enter an image block in the
prompt itself, followed by a meta `[Image: source: <path>]` (a path typed plainly stays text, and
the model can only Read it). The row says `sendsImages` (an injectable herdr pane, so Claude); the
compose box then offers an attach control (inside the field at its left, on a desktop as on a
phone, #s245), a strip of what is attached with a ✕ each, and an image pasted into the field.
Codex native mode, which shares `#composer` and sends its own images, never shows these
(`body[data-shared-codex]`). `/api/send?images=1` takes `{"prompt", "images": [data URL…]}` (PNG,
JPEG, WebP or GIF by their bytes, at most 4 of 4 MiB and 8 MiB in all, Codex native mode's bounds;
the route's body limit is 12 MiB, a prompt alone keeps its 64 KiB plain body), and a resume or a
tmux pane refuses images, the tmux pane before any consent step.
The herdr send saves each image under `<cache root>/sent-images/<session>/` (0600 in 0700; kept,
since the transcript names that file as the image's source) only once every check has passed,
pastes each path, waits for one `[Image #N]` per image (counted over the box's lines joined, since
the box wraps; else it is held, with no Enter), types the text after a space, and goes on as for
text. A step that fails once a paste has gone out is held too, saying what is left, unsent, in the
session's input. A real send was checked by hand through a scratch
monitor into a separate herdr server (`a_herdr_session_takes_images_from_the_compose_box` holds the
page). Unlike tmux's status line, nothing announces the send in the pane: no herdr command for it
was measured, and the typed text sits in the box before it is submitted. Caveat from the trial: herdr
restores a session as `claude --resume <id>`, dropping its start flags, so one started with
`--dangerously-skip-permissions` comes back in `settings.json`'s mode.
**A dialog in a herdr pane is answered from the compose box** (#s298, Hong chose Way 1 on #s299, so
Remote Control is not needed for it): a question (`AskUserQuestion`), a permission prompt or a plan's
approval is drawn in the pane and answered by keys, and the transcript does not carry a question or
a plan while it waits (measured, Claude Code 2.1.312), so the monitor reads the dialog off the screen
(`dialog.rs`, the screens as fixtures in `tests/fixtures/dialogs/`). A row says `dialog`
(`question`, `permission`, `plan`) for a session the page can answer (`sendsImages`) while herdr or
the client saw the wait; `GET /api/dialog` reads it and `POST /api/answer` answers it, both behind
the send's gates and the pane grant (a grant to send now also answers dialogs). The parser takes the
choices only where they are the dialog's own (the review of #s298 found the agent's text read as
one): the last run numbered from 1, the dialog's foot right after it (a narrow pane may wrap the
rest), under a full-width rule outside any `╌` preview box, with no numbered line between but inside
such a box (a command's own, as a commit message's list); a question's ☐ or tabs decide its kind
before a "Do you want …?". An answer names the dialog's fingerprint (what is asked, the note,
the choices and their descriptions, a plan's file and text) and is refused if the pane shows
another; one answer at a time per pane; a digit picks at once and is never followed by Enter (a
stray one is a draft in the input box), ticks are toggled where they differ, seen, then Tab, a
written answer is its choice's digit, the text, and the only Enter once the pane shows it; and the
answer is done once the screen has moved on from the one before its last key (a dialog just like it
may stand in its place), else held after about three seconds; gone, the input box must hold no
draft, which a dialog that closed in the moment before the key would leave there. A permission offers Yes and No only: "don't ask
again" and "switch to auto mode" change settings; a plan never offers to bypass permissions. A
plan's text is read by its file's NAME from the plans of the client home whose `sessions/` records
the session. A layout the parser does not know is no dialog, and the card says to answer it in the
session. The card sits in the compose box above its field (`dialog-card.js`), and held sideways
rises over the top bar with its choices beside what is asked; ✎ and the compose button wear an
amber dot while Claude asks
(`a_herdr_dialog_is_answered_from_the_compose_box`). Hooks were measured and not used: a question's
PreToolUse hook holds the terminal while it waits.

**Text in the preview pane is mdrev's viewer, as a guest** (#270,
`design/mdrev-in-the-preview-pane.md`; every text file since #s13), and **mdrev is PINNED** (#274, the owner: "only depend on
static version of mdrev, similar to how agent-monitor depends on crates in claude-replay. Future
upgrades will be triggered explicitly and manually"). `vendor/mdrev` is a crate holding ONE public
mdrev release's embedding kit, unmodified (`release/`: `bundle/`, `mdrev-cli.js` + `package.json`,
`docs/*.md`); its version IS the release's (1.1.65, #s289), `release.sha256` is checked by its tests (never
hand-edit a vendored file), and its build script embeds `bundle/` as a table. Only `claude-monitor`
depends on it, and `routes::handler` — the one constructor the monitor goes through — installs it
into the html crate (`install_mdrev`), so `agent-replay` carries none of it. Nothing installed on
the machine is looked at. **The pin moves only by `scripts/vendor-mdrev.sh <version>`** (the public
release's `mdrev-embed-<version>.tar.gz`, checked against the digest the release publishes; refuses
< 1.1.6, whose guest ignores the `toolbar`/`review`/`annotate` options), then gates, the full browser
suite and a commit of its own. mdrev's source repository is private: only its public release may
enter this repository. The bundle is served from memory at `/mdrev/<version>/` (immutable, since the prefix
names the version) and the page learns the version from `data-mdrev`. The CLI is written into the
monitor's scratch and run with node ≥ 20 (`MDREV_NODE`, final when set; else `PATH`; else the brew
prefixes). node gates NOTES only: without it `open` declares `annotate: false` and history comes from
the name's `git log`. Both modes use mdrev's HTTP contract at `api/mdrev/` (`html_export/mdrev.rs`),
never its in-process `client` (that seam needs renderer internals the release does not export): text
a transcript CARRIES is handed to the monitor (`POST hold`), kept content-addressed in memory under a
`Cap::Held` stamp, and mounted as a plain reader with no toolbar; a FILE opens through
`GET open?path=&sig=`, and every route re-applies `/file`'s four guards with the file's own
`Cap::File` stamp as mdrev's `cap`, so the viewer reads nothing the page was not offered. Notes are
`mdrev-cli notes …` behind `deny_mutation` (PATCH/DELETE included), one document at a time. The
pane's ↗ opens the document in a tab of its own (#271): `/markdown` is a STATIC page
(`ui::markdown_page` + `markdown-page.js`) that reads root, path, cap and the reader's range from its
own address and goes through the same routes; held text reaches the tab through sessionStorage,
which `window.open` copies into the tab it makes — so that control must never use `noopener`.
**An image opens in the pane on ONE click, as a delivered file's name does** (#s223, Hong: "if both
are images, why not giving them a similar experience?"): the app shell draws each image as a small
thumbnail (from `att_datauri` only — a path-only image shows its name, so Expand all still fetches
nothing, #80/#228) and no longer has the Show step or the lightbox; the pane's image stage carries
the lightbox's zoom bar instead (− fit% + 1:1, 40px on a phone). Consecutive images are ONE block
(#s221, `groupImageRuns` in `components.js`): "N images", a strip of thumbnails each its record's own
row (the `data-block-index` a `#b…` link lands on), and **Open all** (#s222) — every item a tab in
the pane, in the block's order, the first selected, an open tab moved into place, the tab cap raised
to fit the set (`preview.openAll`), a group's tabs numbered ("image.png 2/4": its images share one
name). A multi-file `SendUserFile` carries Open all in its card's HEAD beside "first.png +2", since
the card starts folded; its grid gives the state its own width there, or "4 lines" spilled over the
chip in a narrow head and took its click.
**An image gets a tab of its own too** (#s11, the owner, for the desktop): the pane's ↗ opens
`/image` (`ui::image_page` + `image-page.js`), the image whole with the shared zoom
(`shared/image-view.js`); a FILE by the path and stamp it was offered under (`/file` reads it, under
#s7's rules; a pasted image's saved original at full size), an EMBEDDED image by its bytes through
sessionStorage (`imageTabHref` in `preview.js`), which is also the fallback where the file has gone.
Hidden on a phone, as the Markdown ↗ is (#335); text and HTML pages have none yet.
The pinned `mdrev-cli conform` against a live monitor is the definition of done
(`mdrev_contract_passes_mdrev_cli_conform`); the mdrev browser cases need nothing installed and run in
CI too. Keys: `bindKeymap` tracks ENGAGEMENT as mdrev does — the last click or focus inside
`[data-guest-keys]` — and yields every key while it holds; the host element is NOT focusable. The
request parser REFUSES (413) a body over its route's bound instead of silently cutting it to 64 KB;
`hold` gets the artifact cap.
**Every text file goes to mdrev, not only Markdown** (#s13, mdrev 1.1.21): mdrev renders a Markdown
path as a document and ANY other as source code — highlighted by its language, its lines numbered
(drawn from `data-n`, never in copied text), no notes and so no Review control — deciding by the
path alone, which is why held text keeps its real name. Here only the BYTES decide: `open` accepts
what `/file` would show as text (`shows_as_text`: under the viewer cap, not a raster image, UTF-8)
and refuses the rest with 415, whose mount failure falls back to the pane's own view (a download, an
image); and the pane (`mdrevTakes` in `preview.js`) never sends an HTML page, which renders as a
page (the owner, 2026-10-06), nor an image it draws itself. The cases are
`mdrev_shows_a_local_code_file_as_highlighted_numbered_source` and
`the_pane_hands_mdrev_held_code_and_keeps_its_own_view_of_the_rest`. Since mdrev 1.1.23 (#s26) the
display menu's text size and density reach the source view too; the first case presses its larger
step in the pane, so a host rule that stood in the way would fail it.
**The pane prints a Markdown document where mdrev offers no print** (#s30, the owner: "print still
makes sense"): mdrev's own toolbar prints — on a phone from its Aa menu since 1.1.24 (#s31), which also
made the print start from the tap in Safari — but a held reader has no toolbar, so there the pane head
carries a print control (`.preview-print`, hit-tested at 390px) that calls mdrev's own
`mounted.print()` — the document alone on the paper; never for code. **mdrev carries every print**
(#s58, mdrev 1.1.29): its isolation (`#mdrev-print-isolation`, everything outside the mount hidden
through `:has()`, the page printed white) comes back at each `beforeprint` until the reader next
touches, clicks or types (1.1.38, #s75: iOS lays the page out once per print-sheet preview, each in
its own `beforeprint`…`afterprint`, and a first print that gave the page back at the first
`afterprint` put the monitor's page on the paper), since iOS Safari's print() returns before it
captures the page, and on a phone its Aa menu hangs from its
toolbar — since 1.1.31 (#s67, mdviewer #s36) capped below the bar and scrolling where a sideways
screen is short, and a second print in one Safari page load says Safari is asking (#s37). The pane carried both for #s57 (mdviewer #s34) and carries neither now: `print()` is
`mounted.print()`. `a_phone_prints_the_document_alone_and_keeps_mdrevs_menu_on_screen` makes print()
return at once, as iOS does, so a pin that loses either fails it. **SHARED review is the detached tab's, never the pane's** (#s6, then #s10, the owner: "maybe not in
the main interface, but in the full detached view"; mdrev 1.1.18+: threads in a review store,
pairing, a Push). The split is by PREFIX: the pane mounts the guest on `api/mdrev/`, where every
`review` route and a thread's `hide` answer the contract's 404 (the guest then draws no Share and no
Push), a note filed `shared: true` or naming a thread (`shr-`) is refused, and `mdrev-cli` runs with
the viewer key REMOVED from its environment — it carries mdrev's NOTES key instead (#s231, mdrev
1.1.61, Hong's choice on mdviewer #s235: `MDREV_NOTES_KEY`, file `notes-token` beside `token`, made
by the monitor the contract's way when mdrev has a state directory and none is there), with
`--viewer` on every call, so mdrev-cli takes the reader's own notes as theirs (filed, edited,
closed, deleted; on a machine joined to a team's mdrev service too, where a keyless pane was refused
every new note as an agent's, #s230) and turns away everything shared itself. `shares()` in
`mdrev.rs` is the one test for shared review: the viewer key, never the notes key. The detached tab (`markdown-page.js` → `mountStandalone`) mounts it on
`api/mdrev-review/`, the same handler with mdrev's viewer key (`MDREV_STATE_DIR` or `~/.mdrev`, file
`token`, read per request; without one the prefix is the pane's), which answers `review`,
`review/fetch|push|pair`, `hide`, a body-only `PATCH` edit and Share (`POST annotations/{id}/share`,
#s232: an agent's drafts on the reader's note become their shared records, `{edits}` on stdin; the pane
answers it 404, as a host without it does) through `mdrev-cli review|notes …`, and
passes `--viewer` on every note write and listing. The page's own calls (`hold`, `open`, `resolve`)
stay on `api/mdrev/`. Every review route goes through the document's stamp like the note routes, and
the writes through `deny_mutation`; a paired client can reach the prefix directly (the tab is
withheld on a phone, the routes are not), which is the note routes' bar too. mdrev's "Set up shared
review" form (1.1.32+) sits behind two OPTIONAL routes, `GET review/pair` and `POST review/pair/request`
(#s71); neither prefix offers them, and the unpaired notice, which asks the first as it opens (1.1.34),
shows the terminal instructions on their 404. `POST review/pair` passes the reader's `keepName` tick on
as `--keep-name` (1.1.33). The cases are
`the_detached_tab_offers_shared_review_and_the_pane_does_not` (unit, and the browser case with a
hermetic store, state dir and pairing, which also runs `conform` on the review prefix).
**The detached tab offers mdrev's draft 8** (#s54, mdrev 1.1.26+): per-reader read marks, subscriptions
and notifications. Answering `review/subscribers` is how the viewer learns a host offers it (a 404
there and it draws nothing new and no subscribing), so the review prefix answers it with
`review/lookup`, `review/subscribe|unsubscribe` (`{doc}`: the document, else the project),
`review/confirm` (a code an email was sent) and `review/read` (`{ids}`, or `{now: true}` with
keepalive as the page leaves), each `mdrev-cli review …` with the viewer key; `review/fetch` passes
`--viewer` (it sends the marks and subscriptions due, and writes the reader's starting mark — a host
from before draft 8 must not), and `review/push` passes `--with-code` when the reader ticked pushing
their branch too. The pane answers 404 to all of it. Notifications go through
`mdrev-notify-<channel>` commands the vendored `mdrev-cli` finds on the MONITOR's PATH (the paired
monitor's launchd PATH has `/opt/homebrew/bin`, where mdrev-plugin-alibaba puts them). The browser case
`the_detached_tab_reads_subscribes_and_notifies_through_draft_8` runs mdrev's own check's shape with
a fake channel on that PATH.
**On a phone the review prefix is a full-screen SHEET** (#s12, the owner's choice: a tab has no way
back there): the pane's Markdown from a FILE shows a Review control on a phone only; `openReviewSheet`
(`preview.js`) mounts the document with `mountMarkdown(…, {review: true})` in `.review-sheet` over the
whole app (z 95: above the drawer, below the toasts), and its 44px close control unmounts it — no
navigation, so the transcript's offset is unchanged (`a_phone_reviews_in_a_full_screen_sheet_and_…`).
The mount element is a flex ITEM with a height, never a flex container: as one it squeezed mdrev into
a shrink-to-fit column, which the case now catches by hit-testing the heading.

`src/codex-ui/{reference.css,reference-shell.html,icons.js}` are **generated**, extracted
byte-for-byte from `design/agent-monitor-codex-demo.html` by
`scripts/extract-agent-monitor-demo.mjs` and checked by two tests. Never hand-edit them: change
the demo and re-run the script. Production-only chrome (the session filter, the compose box's image
controls) is layered on at runtime by the shell's own scripts and `production.css`, so the
extraction stays exact.

**Shared frontend modules live in `claude-replay-html/src/html/shared/`** (seam 0 of
`design/monitor-shell-duplication.md`, v1.140.0): ONE source, consumed two ways — the monitor
serves each unchanged as an ES module at `/monitor-ui/shared/<name>.js` (`ui::asset()`), and the
html crate INLINES each into its self-contained pages ahead of `export.js`
(`html_export/shared.rs`), where export.js reads it as `window.__shared.<name>`. The inliner is
textual, so a shared module keeps two conventions, held by tests: no imports, and exactly one
trailing `export { … };` line. A new module is one row in `SHARED` (`shared.rs`); the monitor's
import-closure test and the html crate's convention test cover the rest. `claude-monitor/src/codex-ui/shared` is a SYMLINK to that directory, there so
node (the contract test imports `components.js`, which imports `./shared/…`) and editors resolve the
relative paths; the served bytes still come from the html crate through `ui::asset()`.

`claude-monitor/tests/ui_contract.mjs` holds the frontend contract that only JS can answer.
It runs from `cargo test` (via `tests/ui_contract.rs`, which SKIPS when node is missing) and as
its own mandatory CI step.

**Anything under `design/` is PUBLIC.** `.gitignore` keeps real session content out via `*.jsonl`
and it cannot enforce that on an `.html` — a demo page carrying a real prompt timeline reached
review this way. Design fixtures are written by hand.

## Serving local files
A served page's file links are **capability-stamped**: the renderer signs each offered path
with an HMAC (key at `<state>/file-sig-key`, 0600), and `/file` and `/__reveal` act only on a
path carrying the stamp for THAT capability. So a route acts only on what was offered, and a
reveal link cannot be edited into a byte read. `claude-replay-html/src/html_export/sig.rs` is
the whole story.

**Which paths may RENDER is a setting**, `<state>/render-policy.json`:
`{"mode": "allowlist", "dirs": ["~/personal", "~/code"]}` — `mode` is `never`, `offered` (the
default when the file is absent) or `allowlist`. It governs rendering bytes into the page, not
revealing in the file manager: reveal hands over nothing and is the only thing a click can do
on the pages that render nothing inline. The effective policy is folded into `render_flavor`,
so changing it re-renders rather than leaving cached pages stamped under the old one.
The policy decides which paths get a FILE stamp, and **under an allowlist the stamp is the
authorization and the allowlist the ceiling** (#s7, the owner: "we should allow access to anything
explicitly mentioned in a transcript (and within a ceiling)"): `/file` serves a stamped path that is
still there and still under the allowlist — re-asked at serve time, since a stamp carries no policy —
wherever the session ran. Containment used to be required too, and refused the owner a file a session
in `~/code/knack` had just edited in its linked worktree `~/code/knack-work/wt-b40`. It is now the
ceiling only under `offered` (no allowlist, the default, which therefore does not widen) and the
relocation fallback (`remap_reveal`). Containment asks whether a hosted session
EXPLAINS the path (containment: its cwd, its project, its transcript's directory, and — #283 — the
agent's own scratch for that project, `TranscriptAdapter::scratch_dirs`: for Claude
`/tmp/claude-<uid>/<project slug>/`, where a session's spawned agents keep theirs too; `<uid>` is
the transcript owner's, `CLAUDE_SCRATCH_ROOT` moves it for tests). **A background session also
explains its own job workspace** (#291): Claude Code gives every daemon-hosted session
`<claude home>/jobs/<first 8 chars of its id>/tmp`, and an agent uses it as a scratch — one session
had built git worktrees and logs there, named on 749 of its transcript's lines. The eight
characters are a PREFIX, so the job's `state.json` must name that session (`sessionId` or
`resumeSessionId`) before its tmp counts; a sub-agent gets its ROOT session's, which is where its
parent works. The home comes from the transcript's own path — the parent of the store holding the
project slugs — never from `$HOME`, so a relocated store is followed and a test's store stays
hermetic. **A session explains its pasted images' originals too** (#324): Claude Code 2.1.283 saves
each to `<claude home>/uploads/<whole session id>/` and names the paths in an `inlined_image_paths`
record a record or two after the prompt; the fold puts each path on its image, in order and only when
the counts agree (the inline copy is a re-encoded, downscaled JPEG — 921×2000 against a 1320×2868
PNG, measured), so the image gets reveal and the app shell's pane opens the original, falling back
to the embedded copy, with a note saying so, if the original is gone. A sub-agent gets its root's uploads. A refusal says
which (`Unservable`, #s7): 410 for a file that is gone, 403 with the ceiling's words for one held
back, 404 only for a path the page never offered; the preview pane shows them as they are. A path that is gone
when the page is drawn is still offered under an allowlist, judged by its nearest existing ancestor
(`sig::resolved`, #s24): it used to get no file stamp, so a phone could only copy it. A file that went
with a Claude worktree removed after the session (`<repo>/.claude/worktrees/<name>/…`, the usual end
of a worktree whose work merged) is answered 410 naming the worktree (`X-Gone-Worktree`), and the pane
says so; nothing is shown in its place (the owner: "just say it's gone").

**A file the transcript HANDS to the reader is served wherever it lives** (#374, the owner): what an
agent sent with `SendUserFile` (unless the send failed or was declined) and what the user attached to
a prompt (`TranscriptAdapter::hands_over` — Claude: an `@`-mentioned file, a pasted image's original;
Codex: an attached image, a file declared with the prompt) carry a `Cap::Handed` stamp in
`fsig`/`att_fsig`, minted under every render policy but `never` — an allowlist narrows what a
transcript MENTIONS, and sending a file is not a mention. `/file` and mdrev's routes share one rule,
`SessionService::serve_decision`: a `File` stamp under the ceiling, or a `Handed` stamp alone, which is its
own authorization as a reveal stamp is (#79). Measured before it on the owner's machine: 35 of 88
delivered files (a film in `~/Movies`, a demo repo the session never worked in) and 10 of 10 pasted
originals (`uploads` was not allowlisted) could only be revealed or copied, which a phone cannot use.
And `/file` no longer refuses a file over its 8 MB viewer cap: it STREAMS it from disk as a download
(`HttpResponse::stream`), never shown — except that a client the mask policy covers is refused one
that reads as text (its first 64 KB decode as UTF-8), since a stream cannot be masked.

**What leaves the machine can have its secrets masked** (#365, opt-in, `html_export/mask.rs`):
`<state>/mask-policy.json` beside `render-policy.json` — `{"mode": "remote"}` masks every JSON and
plain-text response to a client whose `Host` is not the loopback (the phone over the tailnet, the
same test as gzip), `always` masks the desktop too, `off` is the default — and `--mask-secrets`
masks a `--dump-html`/`--dump-all-html` file. Applied to what is SERVED, never to the transcript
or the cache, so the desktop and the phone read one cache. The rules are prefix scans (provider
keys, JWTs, bearer tokens, a private key's body, a URL's password), each at a word's start — which in
JSON's raw bytes includes the end of an escape (`\n`, `\\n` in JSON a tool printed, `\uXXXX`), of a
terminal code (`\u001b[32m`, a progress bar's `\r\u001b[K`, `ESC ( B`, ESC printed as `\033` or `\e`) and
a diff's `-`, or a key alone on a line went out whole (#s288) — replacing the secret with `*` of
the SAME length — `/records` range-reads by byte offset, and a JSON string stays one — never
touching an escape (a backslash and the byte after it, or a whole `\uXXXX`: a colour code inside a
private key's span once read `\u****`, which is not JSON, #s286), nor a `data:` URI or a long base64
run (a pasted image).
The cases are `a_phone_over_the_tailnet_sees_secrets_masked_and_the_desktop_does_not` and
`tests/dump_html_mask.rs`; their fake keys are assembled at run time because CI runs gitleaks over
every line.

**Every file view offers BOTH halves** (#272, the owner: "offering both for now"): showing the file
in the page — or downloading it, for bytes the page does not show (`/file`'s `Content-Disposition:
attachment`) — AND revealing it in the file manager, wherever the server offered each stamp
(`canReveal` in `shared/capabilities.js` is the one rule: a path and the REVEAL stamp, never the
file stamp standing in). In the app shell the preview pane — where every "show me the file" click
lands — carries ONE reveal control in its head for whatever it shows (image, page, Markdown, text,
a download, an error), and a prompt card carries a reveal beside its own action, as the
Only for a reader at THIS machine (#s29, the owner: "only offer reveal in file manager when
accessing locally and offer download the file when accessing remotely"): `revealHere()`
(`shared/capabilities.js`) is LOCALITY — the page loaded from a loopback host, the server's own test
for gzip and masking — and `besideAction` puts, beside a file's own action, the file manager for a
reader here and the file as a download for one elsewhere (a phone over the tailnet, another
desktop), each where the server offered what it needs; a reveal-only path is COPIED elsewhere. It
was the 760px breakpoint (#335), which missed a remote desktop and a narrow local window both; the
width still decides what a phone's LAYOUT withholds (`onPhone()`: the pane's ↗, whose page has no
way back) and offers (the review sheet). **A download on a touch-first device is the system share sheet** (#s36, the
owner: Photos, Files, or an app, where Safari showed a preview box): `attachment-viewer.js` hands the
file to `navigator.share` on `pointer: coarse` (a phone or a tablet; never a desktop, whose share sheet
is not a download), keeps it ready for the next tap when iOS says the tap expired during the fetch,
and streams anything over 64 MB (`?inMemoryMax=` for a case) to the browser's own download from
`/file`; a desktop saves to Downloads as before
(`a_phone_download_opens_the_share_sheet_and_a_desktop_saves`). `a_phone_is_never_offered_the_file_manager` is the audit:
a remote phone clicks every kind of offered path in the app shell and meets no reveal and sends no
`/__reveal`; a local narrow window keeps its reveal. A case about what a REMOTE reader is offered
opens the monitor as one (`Monitor::spawn_remote`, `harness::remote_phone`, `open_remote`).
Nothing reveals automatically, not even on a refusal (v2's fallback did):
a reveal is a side effect on the reader's desktop. Reveal is interim — a web file browser will
replace it — so no new reveal-only path is added (mdrev's `/reveal` answers 501 for this reason).
Three rules keep the offer honest (#275). A PATH is offered as an image exactly when `/file` serves
it as one — `RASTER_FILE` in `shared/capabilities.js` is held to `raster_type` by a test, and an
SVG is never one from this origin (its source is shown as text; embedded SVG bytes still draw).
A path offered with NO stamp (a server with no usable key) is COPIED on both pages, never sent to
`/__reveal` unsigned, which is refused every time. And every file a multi-file `SendUserFile`
delivered is offered (`ToolUse::delivered` → `head.files`), not only the first the header names.

## Test the TUI without a TTY
- **Deterministic (preferred):** drive `view::View` under ratatui **`TestBackend`**
  — render to an in-memory buffer, call the view's methods, assert cells. See the
  `#[cfg(test)]` tests in `src/view.rs` for the pattern. All viewer state lives in
  `View`, separate from the terminal wiring in `app.rs`, precisely so it's testable
  this way. Add a `TestBackend` test for any new interactive behavior.
- **End-to-end:** `tests/tmux_smoke.rs` runs the real binary inside a private
  `tmux -L` server with no controlling TTY and drives it via `send-keys` /
  `capture-pane` (`tmux new-session -d` works without a TTY). `#[ignore]`d; run
  `cargo test --test tmux_smoke -- --ignored`.
- **Browser (real Chrome):** `claude-replay-browser-tests/` drives the pages in headless Chrome
  over CDP — the follow/anchor viewport contract lives in renderer-fired scroll events, layout
  clamping and native scroll anchoring, which only a real engine has, and twice in one day a
  shell that failed to LINK passed every static gate and was caught only here. Everything is
  `#[ignore]`d; the gate is
  `cargo build --release -p claude-monitor --features claude-monitor/codex-native && cargo test -p claude-replay-browser-tests -- --ignored --skip known_red`
  (a local Chrome; CI runs the same in its `browser` job). `tests/harness/mod.rs` is the kit
  every case uses: `Stores` (a hermetic world of agent stores under the case's scratch root —
  nothing a case measures comes from this machine's sessions), record builders and
  `long_session`, `Monitor::spawn` (`agent-monitor` on a fixed port, reaped on drop; a missing binary
  PANICS naming the build — never a silent skip, which is how a blank shell once passed as
  13/16), `chrome_tab()` — a browser with timer throttling off AND its first tab, the one way a
  case opens one (#343: `first_tab` launches again, up to four times with a stderr line each,
  when a fresh browser gives no tab — ~220 of ~230 failures across fifteen suite runs were that
  one error, "The event waited for never came", in runs of up to 22 cases; such a browser is
  dead, not slow, so waiting longer on it cannot help) — `track_idle` (#351, installed by every
  scenario page before its scripts: it knows every timer, interval, frame callback, fetch and body
  read the page has pending, and `window.__settle(ms)` resolves once nothing is due before the
  deadline AND two frames have passed since the last change; scenarios' `settle()` waits on it,
  capped at the old 700 ms — validated over 1,488 settles with no change after an early return; a
  tab rendering lazily, #204, gives no frames and so waits the full 700 ms as before;
  `CR_SETTLE_CHECK=<file>` re-runs that validation, `CR_PROFILE=<file>` times the settles) — `until`
  (panics with what it saw), and the
  two-surface vocabulary (`Surface::{Classic, AppShell}`, `turn_at_top`, `at_tail`, `scroll_by`,
  `jump_to_end`, `open_last_fold`, `LiveGrowth`). `tests/browser_follow.rs` holds the
  structural cases (the html server's viewport contract; `the_app_shell_*` on ports 2831–2836). `tests/files.rs` holds #272's file affordances on the app shell (2811–2814) and #374's handed
  files on a phone (2933), and
  `tests/unsigned.rs` the unstamped-path case on both pages (2815) — its own binary, because the
  signing key is read once per process and that case needs none; a case
  that clicks a reveal wraps the page's `fetch` so `/__reveal` is recorded, never sent — `open -R`
  must never run on the machine the suite runs on. `tests/scenarios.rs` holds scenarios written ONCE and run against BOTH pages —
  the classic page (the html server's `export.js`) is the reference, the app shell is held to
  the same assertions (ports 2851+). `tests/rendering_audit.rs` is the #174 rendering-control
  audit (ports 2951+), and it is a different instrument from a scenario: it serves the DERIVED
  corpus (`html_export::audit::audit_jsonl`, every `BlockKind` variant and every body part),
  snapshots EVERY longhand computed property of EVERY element under a record root, toggles one
  control, and diffs — so a control's reach is MEASURED rather than read off the stylesheet, and
  a rendering nobody remembered is inside the quantifier by construction. Its claims are written
  as predicates ("`−` changes `font-size` on every element inside a `[data-code]` container and
  on no element outside one"), never as selector lists, and each is verified by mutation in both
  directions. It also compares the two pages as a SET DIFFERENCE over cells keyed
  `<record id>|<group>` (a cell both pages draw where only one page's control reaches it is an
  equivalence gap, and the failure names the record, the role and the page that missed it), and
  asserts that a press whose effect set is EMPTY does not move the reader — at two widths and two
  themes, because cascade bugs hide at a narrow window and colour rules hide in the other theme.
  Arming PROVES the page is quiet first: a page still settling moves properties on its own, and
  the instrument cannot tell that from a control. Run it alone with `cargo test -p
  claude-replay-browser-tests --test rendering_audit -- --ignored`. A case on EITHER surface whose failure is a QUEUED bug carries
  `known_red_<task>` in its name: the gate skips it, the fix removes the marker, and the case
  is never weakened (the classic page is the reference, not an oracle — #71 is a classic-page
  bug the harness found). Scroll/viewport changes to `export.js` or `codex-ui/` extend the
  scenarios; a served module the shell imports must be registered in `ui::asset()` (an
  import-closure test walks the graph and checks every named import exists). The crate sits
  OUTSIDE `default-members` — its `headless_chrome` dep is the heaviest thing the workspace
  compiles, so the LOCAL root gates (`cargo test`, `cargo clippy --all-targets`) never resolve
  it; CI's `cargo test --all` compiles it, its clippy step lints it with `--all` (#318), and the
  `browser` job runs it (visible, not yet a required check — it becomes one once it has been
  stable for a while). Read Chrome's console
  before diagnosing a "timed out waiting for …" on a shell:
  `chrome --headless=new --enable-logging=stderr --v=0 <url>` prints `CONSOLE … Uncaught …`.
  **The viewport trace (#192)** is how a scroll/blank-space report carries its own geometry:
  open either page with `?trace=viewport` (or set `localStorage.viewportTrace = "1"` to keep it
  across reloads) and the shared engine records every decision it makes — one entry per
  TRANSACTION under its cause (`update`, `records`, `converge`, `measure`, `displaced`, `grown`,
  `estimates`, `remeasure`, `render`, and the moves the reader asked for — `jump`, `move`, `reveal`,
  `hold`, each carrying whether the page stamped intent and whether it was smooth: the position it
  started from, whether and how it placed, the range it mounted and how it chose it; #196) and the
  seams inside them: each `reconciled` mount, every `place` (the source it wrote from, the offset,
  the correction, the reader's drift, and `smooth` for the browser's own animation) or
  `place:unmounted`, the scroll verdict (`scroll`, and `scroll:own` for the engine's own write coming
  back as an event — every event of a smooth write until `arrived`), estimate application (`estimates:pending` / `estimates:applied` — the sums take
  a new estimate only at rest, #194), a tail placement waiting for rest (`tail:deferred`) and the
  `rest` that runs what waited, `reshaped` and `measured`, and a `violation` for an invariant the
  check mode (framework §4.12; always on, `window.__viewportViolations`) found broken — with the
  geometry it saw: the mounted
  range and count, scrollTop and scrollHeight, both pad heights, the stored position and what is
  pending, the applied and live estimate, and the ms since the reader's last input. It lands in a 500-entry ring at `window.__viewportTrace`
  (`copy(window.__viewportTrace)` in the console pastes it into a bug) and as one `console.debug`
  line per entry under the `[viewport]` prefix (filter the console on it). Off, no entry is
  built — `trace()` returns on one boolean — though each seam still evaluates the fields it
  passes (an object literal and a few rounded reads; negligible beside the reconcile that called
  it, not zero). `scenario_the_trace_records_what_the_engine_did` holds it on both surfaces.
  **The viewport history (#197)** is what the engine keeps WITHOUT being asked: the last hour of
  three streams, always on, in `window.__viewportHistory` — `actions` (what the reader did: each
  wheel gesture with its summed deltaY, each key, a drag, every commanded move with its target,
  and what only the page knows through `noteAction` — a fold by key, a control by id), `states`
  (the engine after every transaction: the trace's summary plus the window, the count, the pads
  and sums it wrote, its estimates, its BELIEF of the offset — `P`'s own, or what it last wrote,
  or what its scroll handler last read, never a fresh layout read — and the turn under `P`) and
  `deltas` (the shape of every records change: the count before and after, the first rewritten
  index, the kinds it brought in, the last record before and after with its measured height).
  `window.__viewportHistory.export()` — the ⧗ button on the classic top bar, "Viewport history —
  Save" in the shell's Reading popover — is the bug report: the frame's parameters, the session's
  SHAPE (each engine item's kind, measured height, turn and record range, and the record-level
  kinds where a shell unit spans records), the three streams and the violations; kinds, heights,
  indices, turns and timings, never content. Bounded by time (`historyMs`, an hour; `?historyMs=<n>`
  for a case) with a count cap per stream as the backstop. `design/viewport-history.md` is the
  design; `scenario_the_history_records_what_happened` holds it on both surfaces. **The standing
  discipline** (owner, #197): a viewport bug is reproduced from the SHORTEST synthetic transcript
  that shows it before it is fixed — an export names the height profile and the sequence of
  events to rebuild it from, and the harness's builders (`long_session`, `user_at`,
  `tool_result_lines`, …) are what the repro is written in. **The sandbox** (#197 stage B,
  `claude-replay-browser-tests/tests/harness/history.rs`, cases in `tests/sandbox.rs`) is that path
  made mechanical: `Export::load` a saved history, `calibrate` the surface (prose models, folded
  heights and the pasted-image scale measured through the page's own export), `synthetic` writes a
  transcript with the same record kinds and heights (each kind maps to a builder whose tool NAME
  the engine shapes into the same kind — Bash/Read/thinking coalesce into `act`, Edit/Write/Skill
  stand alone, WebFetch is `tool`; an assistant's `commentary` phase is in its kind), `growth`
  replays the deltas, `steps` replays the actions (a wheel by the offset the states saw, not its
  `dy`) and the diff names the first step whose turn under `P` left the recording's by more than
  the tolerance. The synthetic session is surface-neutral, so an export recorded on one page replays
  on the other: `sandbox_walk_classic_export_replays_on_the_shell` is the parity instrument (after
  a commanded move both pages show the same turn; over the same wheels the shell runs ahead by a
  measured, pinned number, because its rows are shorter). To reproduce a report: save the history
  from the page, put the JSON under `tests/fixtures/history/` (kinds, heights, indices, turns and
  timings — `history_fixtures_carry_no_content` refuses a uuid, a path or prose), and write a case
  like `sandbox_walk_classic_replays` (or `sandbox_runaway_classic_live_replays` for a tail that
  grew while the reader moved: the deltas replay as timed appends; a records-by-turn open, #s277, opens
  records by turn and lets in a piece of the head per recorded fill: `sandbox_fill_walk_app_replays`).
  **The engine's design as a framework** — the model, its fourteen numbered invariants with what
  holds each today (construction, a timer, or only a case), the seams a page implements, and the
  #196 refactors — is `design/virtual-window-framework.md` (#195); the history that led to it is
  `design/virtual-window.md` and `design/one-engine-two-pages.md`.
  **A renderer stall is not an engine bug (#204).** On 2026-09-12, with the machine in distress
  (dozens of stranded Chromes), the walk probe saw the classic page stop delivering animation
  frames and scroll events mid-walk while `scrollTop` kept advancing and rects kept moving: the
  main thread answered layout, nothing was painted or dispatched, and the engine was never told
  about the scroll. It did not reproduce in 24 walk runs on a quiet machine (the current and the
  stage-5 engine, a second Chrome, held memory pressure to 27 % free, eight idle Chromes). What
  did show, at small scale: a fresh headless tab on a healthy static page ticks lazily — most
  300 ms samples read 1 frame and 0 scroll events even as `scrollTop` was written, one in ten
  read 24 and 21 (`harness::renderer_activity`) — so headless frame production is lazy per tab
  and the stall is that laziness at the scale of a walk. No page-side watchdog exists for it;
  `until` ends a timed-out wait with `harness::renderer_verdict`, two samples verbatim, so the
  renderer is read before the engine. The owner asked that it not be chased further.
  **The outline drawers' gesture** (#157, amended by #206): a run of wheel events with no pause is
  ONE gesture and it owns WHERE IT BEGAN — inside a pane's body it scrolls that pane's list and is
  spent at its end (no handover to the drawers), anywhere else it works the chain and keeps it even
  if the pointer lands in a pane; stopping ends it. A closing push never collapses a run of cards
  at the bottom that is already wholly visible, because nothing under them is asking for room. The
  REMAINDER answers the same question (#214): it moves the column only while there is still card
  below the fold — never into the 90px of breathing room the column ends with, which is padding and
  reveals nothing — and whatever closing a drawer makes empty is given straight back, since that
  padding holds the browser's own clamp too high. An offset the drawers did not pay for is what
  breaks the gap: the cards are sticky at their slots with a z-index rising downward, so one that
  has caught its slot holds still while the next keeps coming, and the owner photographed a pane
  resting 37px inside the body of the pane above it. The gaps are rigid at every openness AND at
  every offset, which makes a slot's step the card's SHUT height rather than its head's.
  **The column holds only the offset the CHAIN sold it** (#260). A push is not a scroll: it is a
budget spent closing drawers from the top, and `nav.scrollTop` moves only with what is left once
they are shut — so the owner's one requirement ("the column needs to fit all the header portion of
the panes + some gap space between them") is the model's own invariant: if the heads fit, the column
never rests scrolled, and a column that never rests scrolled cannot slide a sticky head over the body
above it. `scrollTop` has writers the chain never sees, though — `scrollIntoView`, a focus ring, a
scrollbar drag — and that is where #260 came from: the demo tape framed each pane with
`scrollIntoView` and the column took an offset no drawer had paid for (measured: `scrollTop 66`, a
−58px gap, Tasks inside the Turns body). So `chainOffsetLimit` is the overflow of the SHUT column
(invariant under openness) and `holdColumnToTheChain` gives back anything above it on EVERY scroll,
the column is `overflow-y:hidden` so no reader can write the offset directly, and `.heads-tight`
drops the 90px of breathing room at a window too short for the shut chain — a corner case that stays
one (owner). What does NOT work: making the open panes share the column so it can never overflow —
if the bodies always fit, the chain has nothing to close, a push does nothing and toggling one drawer
resizes the others (#159). Six drawer cases catch that in one run.
`design/outline-drawers.md` has the model.
  **Each pane's filter is a row in the outline's drop-down** (#186, moved there by #215): the panes
  menu under the word "Outline" carries the choices indented beneath the pane they belong to, on by
  default, remembered, and saying how much they hold back. Tasks has one row per STATE — Running,
  Pending, Completed, keyed by the pane's own group keys so the menu and the grouping cannot drift
  (#218) — running and pending checked, and unchecking the last one puts every state back the way
  the session filter does; a state the vocabulary does not cover (`other`) is always shown, since no
  box would ever bring it back. Agents keeps a single "Active only" box, because a sub-agent is
  running or it is finished. **A task the session recorded no title for is not listed** in that pane
  at all (#217, the owner: "it is pointless to show those tasks"); the pane says how many it is
  holding, and the classic page's task PANEL still lists it and still opens the card that explains
  the absence (#187/#188), because a board that dropped a task would be lying about what the session
  holds. It
  used to be a dot on the card's head, where it was both unfindable and unclickable — a head action
  is absolutely positioned over a head that outranked it, so it reported a perfect 24×24 rectangle
  and answered no click at all; `.outline-card-action` is `z-index:3` now, which is what makes the
  Tasks centring control work as well. `harness::show_every_pane_row` drives the menu.
  **A case that reads the session TREE must say so**: the builders stamp a fixture in a fixed past
  hour, so it lands in the Idle bucket and the app shell's default filter (Active recently +
  Blocked, #202) leaves it out — `harness::show_every_session(&tab, url)` writes the shell's own
  remembered set and reloads. Fixtures that must be placed relative to now use `harness::at`,
  `rfc3339` and `rfc3339_secs_ago`; `CR_FIXTURE_DAY=YYYY-MM-DDTHH` pins the clock for a bisect.
  Waiting on the outline's drawers is `harness::until_drawers_settle` — the app's own settled
  signal (each open body painted at the height it declared), because `drawers-animating` clears
  260 ms after a toggle while the paint can land a second later, which reads exactly like a
  drawer that never opened.
  **A case that narrows the window past 1180px waits for `harness::until_preview_parked`** before
  it hit-tests anything: below that width the preview panel stops being a grid column and becomes
  a fixed overlay at z-index 50 that parks itself off-screen over a .22s transition, and a resize
  can start that transition late — measured, 600 ms after a resize to 820px the panel still sat at
  `translateX(0)` across the middle of the window, so a control under it (the transcript filter
  popover is z-index 48) failed its hit test and read as a control something had painted over
  (#211). Wait for the window to reach the new width first, or the wait answers about the old
  layout.
  **A CDP key press the page does not `preventDefault` never comes back up** (#270): with this
  headless Chrome one unhandled `x` became 2,177 trusted keydowns in 300 ms and kept repeating into
  fields typed later, whatever the keyUp carried and with no page code involved. A case asserting a
  key did NOTHING calls `harness::quiet_keys` first; a case typing INTO a field asserts `contains`.
  A killed run used to leave its browsers behind — a SIGKILL runs no `Drop` and macOS has no
  PDEATHSIG — and sixty such processes once exhausted the machine and took the session's
  background jobs with them. `chrome()` now names each profile `cr-browser-chrome-<launching
  pid>-<n>` under the workspace scratch and reaps, at launch, any marked process whose owner pid
  is gone. Both halves of the mark are required, so a suite running in parallel is never in range
  and the developer's own Chrome cannot be (#141).
- **Quick plain check:** `agent-replay <path|--latest> --dump -` renders to stdout
  (no TUI) — good for verifying parsing/markdown/diffs in a pipe. (`--dump <stem>` or
  bare `--dump` instead write `<stem>.txt` + `<stem>.ansi` at the terminal width or
  `--width N`; bare `--dump` deduces the stem.) `--dump` renders through the View
  pipeline and applies the TUI's default fold policy (add `--full` to expand all).
  `--dump - --json` instead emits the structured block stream (#34): JSON Lines, `kind`
  from the shared classification, per-TURN timestamps, tool `status`/`exit`/`ms` — the
  content half of the shell-out vocabulary (`--paths --all` is the discovery half).
  `--cache` (#10, `src/dump_cache.rs`) makes that dump resumable for a collector's daily sweep:
  the same bytes, from a `json` entry in the durable cache, folding only what was appended since
  the last cached dump. The entry holds BLOCKS in the TUI's format and the stream is projected at
  emission, so the one version that can make it stale is `FOLD_VERSION` — which therefore has a
  PROGRAM consumer now: an un-bumped change to what a block carries reaches a script as silently
  as it reaches a page. Opt-in, because the plain dump is read-only and lock-free; a held entry,
  an unwritable cache or a transcript caught mid-line gets the plain dump.

## When the transcript format moves
**`agent-replay --unknown` is the ten-second check** (#264). It parses transcripts and prints
every shape the adapters did not recognise — a top-level record `type`, a `message.content[]`
type, or a key inside `toolUseResult` — one row per shape with a count, the client version that
wrote the first one and a session to open. Silence is the good answer.

It exists because the format moved and we found out by eye, a week late: Claude Code began
recording `toolUseResult.bashEditDiff` on 2026-09-13 (client 2.1.270) — a real unified diff for
every file-editing Bash command — and `git log -S bashEditDiff` was empty, so 975 records across
thirteen sessions carried a diff the page dropped in silence (#263).

**The reporting rule is an ALLOW-LIST, and that is the whole design.** A census of the twelve
largest sessions found **125 distinct top-level `toolUseResult` keys**; the Claude adapter read
eight. "Report any key no code reads" would have fired on 117 on its first run — `isImage` 80,791
times — and a log nobody can read is a log nobody reads. So `TOOL_RESULT_READ` and
`TOOL_RESULT_KNOWN_IGNORED` (`agents/claude/model.rs`) are a snapshot of the vocabulary as of
2026-09-20, and only a key outside both is reported. **Adding a key to the ignored list is a
deliberate act** — it says "looked at it, it carries nothing we render" — and belongs in the same
commit as the look that decided so, never in a sweep to quieten the output. That claim can be
wrong: `answers` and `annotations` sat on the ignored list while the `AskUserQuestion` card read
the reply out of the result's prose, which cannot carry a typed answer or a note, and the owner
found an answer missing from the card (#280). They are read now, and so is `afkTimeoutMs` (#281:
the card called a question the client had stopped waiting on "waiting"), so the list holds eleven.

**A new top-level KEY on a record we read is a category too** (#361, `record.key`, named
`<type>.<key>`): Claude Code's `promptId` sat on nearly every user record for weeks and no category
could see it. The census is `RECORD_KEYS_COMMON`/`RECORD_KEYS_BY_TYPE` in the Claude adapter, scoped
per record type (many keys are generic words), and the QoderWork family's own keys are known to its
adapters alone (`FAMILY_RECORD_KEYS`). It walks every record's keys, so it runs only while the sweep
asks (`unknown::watch_keys`): an ordinary parse pays nothing. (`promptId` itself was measured as a
turn key and refused, #362: it groups the client's prompt cycles, not the reader's prompts.)

**Text the client wraps in tags is read from the record, not shown raw** (#s79, the owner: "claude
now differentiates messages from another session, or including copy/pasted contents"). A message
from ANOTHER Claude Code session is a `user` record (`isMeta`, `turnOrigin: peer`) — or, picked up
mid-turn, a `queued_command` attachment — with a structured `origin` (`kind: peer`, `name`,
`fromMode`, `body`), its text wrapping the body in `<cross-session-message>` between a preamble and
a paragraph addressed to the model. It folds to `Block::PeerText` (`peer` on the wire): a turn — the
Turns pane, the sticky strip and every turn count include it (`Block::opens_turn` is the ONE turn
predicate) — but never the person's: no `❯`, no right-hand bubble, not in `u:` (`directMask`
claims no class for it), drawn as a card from the LEFT under `⇄` and the sender's name (the owner's
choice), the message alone as Markdown. In the app shell each sender has a hue of its own (#s138,
Hong: tell agent turns from the person's and one agent from another, in the Turns pane): `peerHue`
(`view-model.js`, FNV-1a over the name, so stable everywhere) colours the card's rule and name and
the sender's chip on its Turns pane row, the label going on after the name (`peerLabelRest`).
`origin.body` is read, never the prose; the tag is read
only for a meta record with no `origin` (none measured), so a person QUOTING the tag keeps their
prompt. The mid-turn one used to render as the person's own prompt. A peer enqueue draws no `⧗
queued` marker (that marker means the person typed it) but stays in the queue, popped at pickup by
its raw text. Text the person PASTED is `<pasted_content id="X">…</pasted_content id="X">`; the
adapter rewrites it as a fence labelled `pasted` (one backtick longer than any run inside), applied
identically to the prompt, its queued attachment and the queue operation so the #52 content match
holds: a labelled verbatim card on both pages, a `┌ pasted` box in the TUI, `[pasted]` in a turn's
label. Cases: `another_sessions_message_is_its_own_turn_and_pasted_text_a_fence` (both parse
paths), the `#s79` contract block, `*_shows_another_sessions_message_as_its_own_turn`.

**Hong's messages relayed into the Aries session are his, and shown so, only when signed** (#s272,
Hong, 2026-10-26: they all read alike, and text that only looks like one must never pass for his).
aries-inbox types his DingTalk messages, dashboard replies and card taps into the Aries pane as one
element, `<aries-inbox v="1" from via kind at id [about to task guess] sig>`, then an optional
`<reply-to>` line, an optional `<do>` line (Aries' instructions to itself), his words and a closing
tag; `sig` is HMAC-SHA256 under the Mac's key `~/.local/state/aries/inbox-key`. The contract is
project-h/aries `docs/inbox-format.md` (aries#s33), and `aries-inbox verify` is its reference
check. The Claude adapter (`agents/claude/inbox.rs`) takes one as his only when it opens the user
turn at its first character, its sig verifies, and its id is new to the session; anything else stays
the text it is. It reads the element as `verify` does, and refuses where `verify` would read two
ways (an attribute given twice), a sub-agent's record (`isSidechain`) and a queued pickup whose
`origin` is not a person's. aries-inbox types a bracketed paste, and Claude Code 2.1.310 records a paste of four
lines or about 800 characters inside `<pasted_content>` (measured in Aries' transcript: 8 of 193
prompts before v1, and every v1 message is that long), so a turn that is WHOLLY one paste counts from
the paste's first character (Aries' amendment). The paste also changes some characters (a tab, a
CR, zero-width ones); aries-inbox writes the text pane-safe before signing, so the recorded bytes are
the signed bytes and nothing is normalized here. A verified one is a `UserInbox` block, the person's
turn (the `u:` class, the right-hand bubble, the Turns pane), its words in `text` and the rest in
`inbox`; its `[pasted image: <path>]` lines naming a file under aries-inbox's own `uploads/` (by a
plain path) become image attachments after it, offered as a prompt's pasted originals are (a handed
stamp, #374): only a picture aries-inbox saved, in a verified message, is a file handed over, and any
other such line (a path he typed or forwarded) is his words. `<reply-to>` and `<do>` are one line
each, so a quote holding `</reply-to>` stays a quote. The card has no "show as typed": what was
typed is the element, which the transcript keeps. Most of
them arrive mid-turn, so the queued pickup is read too. The first-id rule is the fold's
(`Replayer::inbox_ids`), persisted through the meta stream (`MetaRecord::inbox_ids`) so a resumed
fold still reads a later copy as text. The fold reads no files: `agent-monitor`'s route handler and
`agent-replay` install the key's source (`$ARIES_STATE_DIR`, else `~/.local/state/aries`; a key file
others may write, or not 32 bytes, is refused with one stderr line), and with none installed nothing
verifies. The verdict is part of the cached fold, which nothing ties to the key: replacing the key
leaves cards already folded as they were until the cache is cleared or `FOLD_VERSION` moves. Both pages draw his card with the relay's chips (via, kind, issues, a
relay's agent and task, when sent; `shared/inbox.js`), the words he answers quoted above his, and
the instructions folded until asked; `--json` keeps `kind: "user"` and adds `inbox`. The fleet
needs nothing: its tabs are each Mac's own monitor, which holds that Mac's key. Cases:
`agents::claude::inbox::tests` (samples checked by `aries-inbox verify`, good and bad),
`a_relayed_message_is_the_persons_turn_once_and_only_where_it_opens_the_turn` (both parse paths),
`tests/relayed_messages.rs` (every resume point, a later poll) and
`*_shows_a_relayed_message_as_his_card_and_a_copy_as_text`; the browser harness points
`ARIES_STATE_DIR` into each case's stores, so no case reads this Mac's key.

**`agent-replay --field-coverage` is the other half** (#363): `--unknown` catches a NEW shape, and
cannot catch a KNOWN field going empty after a client update — a usage block that stops arriving
makes the cost a silent lower bound. Each adapter declares the short list its cost and cards read
(`COVERAGE_FIELDS`; Claude Code and Codex), and the report gives each field's fill rate per client
version, flagging the newest version that writes one clearly (`coverage::DROP`, 20 points) less
often than the best version before it. Run over a window (`--since 14d`), a drop is the news: a client update stopped writing
something the cost or a card reads.

The channel is `claude-replay-engine/src/unknown.rs`, re-exported through `engine/seam.rs` as
`note_unknown`/`UnknownAt` so all three families and any third-party adapter report the same way.
It costs nothing when nothing is new: a recognised shape never reaches it, because the adapter's
own `match` answers first. It never holds content — a kind, a name, a count, a version and one
locator, safe to paste into an issue.

**It sweeps every store, whatever directory it runs from** (#276 — it used the viewer's cwd-scoped
discovery and scanned nothing from `/tmp`): the newest 200 transcripts machine-wide, or with
`--since 1d` every one modified in that window, and `--json` gives one object per row
(`agent, count, where, name, version, example`). It also names every MODEL that produced tokens
with no price in `claude-replay-engine/pricing.json` (`where: model.unpriced`, counted in sessions):
that session's cost silently becomes a lower bound.

An internal daily review reads both sweeps and queues what it finds; it is part of the owner's
process, not of this repository (its notes are imported at the end of *Releasing*).

## Test scratch
Tests build their scratch under `std::env::temp_dir()` — ~100 call sites across the
crates — and `.cargo/config.toml` points `TMPDIR` at the workspace's own `target/`,
so all of it stays inside the repo and `cargo clean` (or `scripts/sweep.sh`) clears it
(#164). It used to land in macOS's opaque `/var/folders/…`, which nothing sweeps: 8,014
directories and 267 MB had accumulated there. A full run leaves ~3.4 MB — except the browser
harness's roots (`cr-browser-follow-<pid>-<case>`, `cr-browser-state-<pid>`,
`cr-browser-chrome-<pid>-<n>`), one per case per run and ~0.3 GB for a walk over a long
session. A case that PASSES removes its own as its `serial()` guard goes (#s142: a day's suites
had left 741 of them, 13.6 GB); a failed case keeps its root for diagnosis, as `CR_KEEP_SCRATCH=1`
keeps every root. What is left the sweep prunes by the LIVENESS of the pid in the name, not by
age (#205: the age rule had kept 43 GB of them after one day's runs); a running suite's roots are
never in range, so the sweep (or `scripts/sweep-scratch.sh` alone) is safe beside a live run. Everything else goes by age.
Scratch inside the repo is scratch inside a GIT repo, so the same file sets
`GIT_CEILING_DIRECTORIES=target` — a fixture that shells out to `git` sees no
repository, exactly as it did in the system temp. A test that spawns a `tmux`
server must hold the `Server` Drop guard (`tests/tmux_smoke.rs`), or a failed
assertion strands the server and whatever runs inside it.

## Releasing
After each completed CODE task (docs/design-only changes need no release), release with
**`scripts/release.sh <version> --subject "<what shipped>" [--message-file <body>]`** — the
one mechanical path (#200): a clean tree on main that origin/main is an ancestor of; the
version guard (`scripts/release-check.sh --next <version> --fetch`: refuses a version at or
below the highest tag on either side, or one already tagged, unless `--allow-backwards`, which
is printed); the bump of `[workspace.package] version` and nothing else (the SECTION — a naive
`^version` match hits `version.workspace = true` first, which is how a release once moved main
from 1.259.0 back to 1.258.0 while another tree was releasing); `cargo build` for Cargo.lock;
the gates; the release commit and the signed annotated tag, the commit verified before the tag
(a failed commit with the tag commands still running once shipped a tag pointing at the wrong
commit); the push to `origin main`, then the tag — the tag push triggers the Release workflow,
which publishes binaries and bumps the Homebrew tap (`tanghong123/tap`) — then the post-release
hook when this machine has one (below). `--dry-run` stops after the gates. CI's `version guard`
job runs the same check on every push to main (the workspace version never below the highest
tag; a release commit names its own version and owns its tag), and the Release workflow refuses a
tag that does not name the workspace version, so a release cut by hand is still refused where it
went wrong before.

**Then sweep: `scripts/sweep.sh`.** A version bump changes the metadata hash of every
crate and every test/example/bin target, so it mints a COMPLETE new set of artifacts and
orphans the previous one — and cargo never garbage-collects `target/`. Releasing per task
without sweeping is what grew `target/` to 64 GB (241 hash-variants of the engine rlib,
345 of the root test binary) against ~200 MB of live artifacts. The script asks cargo which
artifacts the real gates need (`--message-format=json`, dev `--all-targets` + release) and
deletes only what is in neither set, so it costs no rebuild — run it right after the build
and the next one is still warm. `--dry-run` first if in doubt.

`origin` (GitHub) is where the code is developed, where releases are cut, and where issues are
filed. Steps specific to the owner's environment are not in this repository: `release.sh` runs
`~/.claude/claude-replay/release-hook.sh` when it exists, and their notes are imported below. On
a machine without them, a release ends with GitHub and the public tap.

@~/.claude/claude-replay/internal.md

## Teammates' pull requests land without the owner
Most of this repository is the owner's; teammates contribute now and then by pull request (or by a
merge request on the owner's mirror, which the internal notes cover). **The owner is not in the
merge path** (owner, 2026-10-16): the owner's agent reviews a teammate's change and, when it is low
risk, approves and lands it under the owner's account without asking, once all three hold:

1. **Reviewed twice.** The agent's own review: the gates below, the browser cases the change
   touches, the whole local browser suite for a change to page code, and an iOS Simulator walk for a
   phone UI change. Then an independent review of the exact head commit by a fresh subagent or
   another agent, never the session that lands it. Every thread on the change is answered.
2. **Green.** CI passed on that exact head: `fmt · clippy · test`, `secrets · blocked emails`,
   `version guard` and `browser (real Chrome)`.
3. **Current.** main is merged in (or the branch rebased onto it), and the result is still green.

Then approve, land it fast-forward only under the landing lock, post a note on the change naming the
reviewed commit and the checks, and close its review task with what landed. One review task carries
the change from first review to landing, every task about it tagged `mr-<n>`: see the taskq skill's
`references/merge-requests.md`. Requested changes go to the author as findings on the change, never
to the owner; the owner hears of a disagreement only when a second round still has one. A change
that cannot land fast-forward gets main merged into its branch first, then lands fast-forward: never
a server-side merge commit.

**Still the owner's.** Review it, then bring it to the owner in one line instead of landing:
- releases, version numbers and the Homebrew formulas;
- CI, workflow and gate configuration (`.github/`, `scripts/gate/`, `.gitleaks.toml`);
- authentication, pairing, the file gate and its capability stamps, secret masking, and anything
  that opens a port or a network path;
- vendored releases (`vendor/`) and new dependencies;
- the public contract: CLI flags, the `--json` streams, the on-disk formats other tools read;
- removing a feature, or a design change beyond one component;
- a UI change not yet walked on the iOS Simulator.

**A first-time contributor's** fork PR needs its workflow runs approved in the GitHub UI: approve
them only after reading the diff for changes to workflows or anything that reaches secrets, which is
part of the review. CI must run and pass BEFORE the merge. Never `gh pr merge --admin` past pending
checks: the pre-push hook cannot see a server-side merge, so CI's guard job is the ONLY identity
check on that path (#16 leaked a work email exactly this way; the accepted commits are
sha-allowlisted in ci.yml).

## Gate every change on
`cargo fmt --check`, `cargo clippy --all-targets` (no new warnings), `cargo test`
(runs BOTH crates via workspace default-members; deterministic — no terminal needed;
the tmux e2e is opt-in), and `scripts/gate/gate.sh` printing `BYTE-IDENTICAL: PASS`
(fixture data lives in `$SC_GATE_DIR`, default `~/.cache/claude-replay-gate`; intentional output
changes are verified line-by-line then re-baselined — see `scripts/gate/README.md`).

For adapter event-mapping changes, also follow
`docs/adapter-rendering-validation.md`: render a minimal Claude-shaped reference and the target
agent transcript through the same binary/options, compare their semantics under default and
`--full`, and keep all agent-specific normalization inside the adapter.

## Layout
A Cargo **workspace** with eight library/binary crates, layered for multi-level reuse
(#71, #87): engine → agents → core (facade) → present → {tui, html} → the root binary
crate — plus `claude-replay-browser-tests/`, a member deliberately kept OUT of
`default-members` so its headless-Chrome dep never reaches an ordinary build — plus
`vendor/mdrev/`, the PINNED mdrev release (#274): data only, versioned as the release it holds,
and a dependency of `claude-monitor` alone — plus
**`claude-monitor/`** — the machine-wide session index (#98): a loopback web service whose
page is the app shell; scan/state/cards in `src/index.rs`; lazy population — a session's durable
entry (at the monitor's OWN root, `~/.cache/claude-monitor`) is written by VISITING it,
never by a sweep. It is **lib + bin**: `src/lib.rs` exposes `index` (the scan and the send
DECISIONS), `consent` (grants + the passcode), `cost`, `state`, `control` (the pairing
token and the two send transports, #133), and `routes` (THE route table, #47: the API arms,
the assets and the service fallthrough). Each
crate re-exports the lower layers' modules at its root (`crate::model`, `crate::present`,
…), so moved code reads unchanged. One shared version: bump `[workspace.package] version`
in the root Cargo.toml — the single spot per release.

**`claude-replay-engine/`** — the agent-FREE machinery (#87 step 3): the data model, the
fold, sessions/stores, the follower, the discovery vocabulary, the `TranscriptAdapter`
trait, and `seam` — the audited contract adapter code builds on. A third party adds an
agent against this crate alone.

**`claude-replay-agents/`** — the built-in adapter families (`agents/{claude,codex,
qoderwork}/{model,metrics,discover}.rs`), their `TranscriptAdapter` impls, and the
`REGISTRY` slice. The crate boundary + the `agents_import_only_the_seam` audit keep
family code on the seam. The machinery-with-real-adapters integration tests live in
`tests/engine_integration.rs` (a dev-dep cycle would compile two engines inside engine).

**`claude-replay-core/`** — the FACADE: engine wired to the agents' registry, presenting
the same API core always had (`adapter()`/`adapters()`, registry-driven discovery
(`detect_agent`/`resolve_any`/`candidates_all`), `Transcript`, the `parse_session*`
dispatchers; everything else re-exported from the engine). **No** TUI/HTML/CLI deps
(only `serde`/`serde_json` + `anyhow`); the crate boundary enforces "core is
presentation-agnostic".
- **Shared engine** (agent-neutral): `model.rs` the `Block` data-model vocabulary + block
  classification (`block_kind`/`fold_key`) · `engine/` `replay` (the L2 `Replayer`/`Shaping`
  fold + `parse_stream` driver that *builds* blocks) · `message` (L1↔L2 log) ·
  `session`/`index` (`Session<BV>`/`BlockStore`/`BlockAccess`/`SessionIndex`) · `tier_b`
  (off-heap/on-disk block backing) · `tasks` · `builder` (`SessionAccumulator`) · `path`/`time` ·
  `metrics.rs` the `Metrics` value + pricing · `discover.rs` the `Candidate` type +
  `detect_agent`/`session_cwd`/`session_id`/`subagent_source`/`resolve_any` ·
  `follow.rs` incremental `FollowParser` (drives `engine/elide.rs` + `LineSource` — the bounded eliding reader, #193) · `tail.rs` byte-offset tail · `agent.rs` the `Agent` enum ·
  `adapter.rs` the `TranscriptAdapter` trait + `adapter()`/`adapters()` registry (the one per-agent seam)
- **Per-agent adapter families** (`agents/<agent>/`, symmetric, each feeds the shared engine):
  `agents/{claude,codex}/model.rs` (tokenizer + `Shaping`) · `agents/{claude,codex}/metrics.rs`
  (token/cost folding) · `agents/{claude,codex,qoderwork}/discover.rs` (that agent's transcript
  store). A new agent = a `model`/`metrics`/`discover` family under `agents/<agent>/` + one
  `impl TranscriptAdapter` row in `adapter.rs`; the shared engine is never touched. Agent code
  may reach the rest of the crate ONLY through `engine/seam.rs` — the audited adapter contract
  (`agents_import_only_the_seam`); anything an adapter newly needs is added to the seam
  deliberately (#87). Each adapter owns its test suite (the byte-identical equivalence gates
  live in the `model` families); `model`'s tests are the agent-neutral ones only
  (`block_kind`/`fold_key`, `relativize`). **The QoderWork family serves an agent DERIVED from
  it** (#358): Qwenwork (QwenWorkCN, `~/.qwenworkcn`) writes the same store layout, sidecars and
  title database under its own names, so `agents/qoderwork/discover.rs` takes a `Home` (agent,
  dot-dir, App Support name, `*_PROJECTS_DIR`/`*_DB` overrides) and one macro defines both
  adapters. The family's `runtime-config` head is QoderWork's alone at the sniff level, so
  Qwenwork is Qwenwork only by provenance (a file under its store; one outside reads as
  QoderWork), and its own head rows (`runtime-config`, `workspace-directories`,
  `worktree-state`, `active-leaf`) are known to the family's adapters only (`decode_line_known`,
  #359). A hermetic test points `QWENWORK_PROJECTS_DIR` (and `QWENWORK_DB`) at its scratch like
  every other store, or it reads this machine's real sessions.

Also in core (beside the vocabulary they index): `fold.rs` the `FoldPolicy` (clap-free
`from_flags`; the CLI bridge is `Args::fold_policy`) and the agent-neutral diff-row model
`diff` (`DiffKind`/`DiffRow`/`DiffGroup`/`diff_row_groups`/`line_diff` + `base64_decode`).

**`claude-replay-present/`** — presentation SUPPORT, frontend-agnostic: `cache/` the session
residency cache (`SessionCache` over a client-built `Entries` provider (#167) — `PerSession` /
`SingleWriter` / `Transient` (`--no-cache`) — registry + TTL reaping, `SharedSession` + the
cursor-pull protocol, tier-b spill wiring) · `present.rs` the plain-text summary formatters (spawn
chips, tool display names, edit summaries; re-exports core's `summary` phrasing) · `highlight.rs`
the syntect highlighter (returns ratatui `Span`s — the shared span vocabulary; ratatui is a
types-only dep here, no terminal backend) · `sys.rs` (`deduce_stem`, `reveal_in_file_manager`, and
where a RUN puts its own directories — `run_dir`/`reclaim`, client-side on purpose: the cache owns
only the SHARED root) ·
`args.rs` the shared `Args` options type (plain data; the `cli` feature adds the clap derive —
library consumers stay clap-free).

**`claude-replay-tui/`** — the terminal frontend: `view.rs` state machine + draw
(TestBackend-testable) · `app.rs` terminal + input · `render.rs` blocks → styled ratatui lines ·
`markdown.rs` md → ratatui lines · `wrap.rs` wrapping · `theme.rs` styles · `picker.rs` fuzzy
session picker · `clipboard.rs`. Only `app`/`view` are public.

**`claude-replay-html/`** — the HTML frontend, no terminal deps: `html_export/` (`mod.rs`
render core · `bundle.rs` the `--dump-html`/`--dump-all-html` offline writers · `serve.rs` the
`--html` live server, which always tails; it serves over a loopback HTTP server since a
`file://` page can't `fetch`) → one self-contained `.html` (fixed shell + `html/export.{css,js}` embedded; Rust
emits a JSON block stream, the JS renders it). Every page reaches the browser's stores only through `crPageStore`
(`html/page-store.js`, emitted ahead of `export.js`); `--no-storage` swaps in `page-store-memory.js`
for a frame sandboxed without its origin, where every store throws and a host's lint flags the names,
and `a_no_storage_page_names_no_browser_store` holds every other page source to naming none (#s124).

The stream is **append-only almost always, and the exception matters** (#165). A live delta
carries `changed_from` — the index from which records were REWRITTEN — and the one thing that
routinely makes it non-zero is a QUEUED prompt being picked up: the marker for it disappears and
every marker still queued behind it shifts up a slot, so `[…, queue"second", queue"third"]`
becomes `[…, queue"third", user"second"]`. Record ids are `b{n}` from a positional counter, so
after that rewrite the SAME id names a DIFFERENT record — an anchor held across it resolves, to
the wrong thing, rather than failing loudly. Anything that assumes the tail only grows (a
viewport anchor, a height cache, an incremental index) has to survive this.

**A tail-first open is the other way the records change** (#314, `design/tail-first-open.md`, the
owner chose option A): the app shell's first pull asks for the last 512 KB of the committed log
(`tail=`, honoured on a log over twice that), draws it, and reads the head behind it. Until it lands,
the head's slots hold the one `PENDING` placeholder and the projection shows them as a short "Loading
earlier turns" card — the page goes no further up, as a chat app's history does. The head replaces
the placeholders IN PLACE (record indices never move), but the engine's items are UNITS, so the
landing is a PREPEND: `recordsChanged` takes `{from, shift}` and translates the reader's `P0` to the
moved index space (a reader on the card re-anchors on the turn below it). Without that, the anchor's
key is found at a new index, which reads as the #165 identity shift and falls back to a position
`shift` units wrong. The history records the shift, and the sandbox counts the landing as part of the
open. **The landing waits for a STILL view** (#340): on iOS a fling glides on for seconds after the
finger lifts — scroll events, no input — and an offset written into the glide does not stick (it
carries on from its old offset, which in the landed page is a turn near the start; the owner's export
read turn 4, then bounced to 82). So `updateRecords` holds a prepend, and every later snapshot with
it, until `Viewport.whenStill` (no scroll event for 200 ms, no finger down, and two reads 50 ms apart
agreeing); `a_phone_fling_keeps_its_turn_when_the_head_lands` emulates the glide on a 16 ms timer.
**The engine writes nothing into a touch glide** (#372, `design/virtual-window-framework.md` §4.3):
from the finger's first `touchmove` (#s25: a TAP is not a glide — it began at `touchstart`, and a tap
that opened the phone pane re-anchored the reader turns back) until `whenStill`, a correction is absorbed into the top pad (`padDebt`, carried by
`contentTop`) or, if the pad cannot hold it, yielded; estimates and the tail wait, and so does a
head landing (`updateRecords` holds on `viewport.touchGlide` as well as the scroll clock, which hears a
glide only as fast as its events arrive — `a_phone_fling_heard_late_still_holds_the_head`); one
`settle` write pays the debt once still. The owner's export (`fixtures/history/phone-jitter-app.json`, the turn
swinging 444↔405) replays in `sandbox_phone_jitter_app_replays`; a touch gesture replays as an
iOS-style glide (`harness::touch_glide`: offset writes from the glide's own position with a scroll
event per frame — without that event a headless tab hears the glide lazily, which invents failures).
The preview pane remeasures the transcript only when it changes the transcript's WIDTH (#s25,
`paneLayoutChanged` in `app.js`): on a phone and below 1180px it is an overlay, and a remeasure there
only threw every measured height away (`a_phone_reader_who_opens_a_file_and_closes_the_pane_…`).
While the head is still coming the phone's Turns count is the highest turn NUMBER drawn (#342). The
classic page never asks for a tail and is unchanged. **The head comes in ranges** (#s274, Hong on a
phone over the tailnet: the head climbed to 48% and fell back to 1%, again and again, because one
`/records` read of it restarted from the top after every drop): `RecordStore.loadHead` reads at most
`HEAD_PIECE_BYTES` (1 MiB; `?headPiece=` for a case) a range, parses each record as it completes and
starts the next range at the first record it has not finished, so the progress only climbs. **A
`/records` reply is always whole records** (`RecordStore::read_records`): an end inside a record is
carried on to its end and a start inside one is refused (400), because secret masking (#365) reads a
JSON string's quotes from the first byte of a reply; a reply cut inside a record carried a private
key's body to the phone unmasked (`a_masked_piece_of_the_record_log_shows_no_part_of_a_secret`). A
409, a head that does not parse or one whose count differs starts the session over
(`app_shell_head_cut_mid_transfer_goes_on_from_the_record_it_was_reading`). **The turn index**
(#s276, `design/slow-links.md` stage 1) is the log's map: a sidecar `records.index` beside
`records.jsonl`, one JSON line per committed record (its offset and a fingerprint; a turn opener's
number, kind, label, time, sender and quoted reply; its tool calls by name, nested ones included;
a compaction's head; the paths it offers with their stamps; its artifacts), read off the RENDERED
record by the page's own rules (`turn_index.rs`). It grows in `RecordStore::put` and is cut in
`cut_to`, the one seam every cut of the log goes through; a log from before it, or one that grew
while it lagged, is indexed from the log at the next read (117 MB in 0.22 s, measured). `/turn-index
?session=&epoch=&upto=` answers for the records BELOW `upto` exactly, since a turn can straddle the
head's boundary (409 on a stale epoch, masked and gzipped like `/records`). The page asks for it
beside the head and uses it only while the head is out: the Turns pane lists every turn and
compaction (those still coming marked `outline-turn-pending`, a tap on one held until it arrives,
unless the reader moves first), and the turn count, the tool letters and counts, the artifact
roster and a link to a file the head offered are the whole session's
(`app_shell_lists_the_whole_session_before_the_head_lands` reads them held, then landed, and
compares). Search still reads the records (stage 3). Without the index (a refusal, five failed
reads) the page reads the tail as before (`a_phone_tail_first_open_counts_the_whole_session`
refuses it). **Records by turn** (#s277, `design/slow-links.md` §6, Hong's #s279: stage 2) is a
reader across a network's path (`WINDOWED`: not `revealHere()`; `?windowed=1|0` for a case; the
desktop keeps stage 1). When the index lands, the card gives way to the units every earlier turn will
make, keyed as they will be: the index reply carries each record's kind, commentary bit and id
(`facts`), and the page's own `buildUnits` groups them (`Projection.stub`); a unit owes (`owed`)
until its last record is here, and is a PLACEHOLDER (`placeholderUnit`, `placeholderAt`), which the
engine draws at its height in the sums (margins included), never measures and never learns from,
refitting the mounted ones wherever the applied estimate moves — except a group of steps that reaches
into the tail (`live`: a tail that starts mid-turn holds no prompt), the live turn's, drawn with what
has arrived; the landing tells the engine its new key (`renamed`), found by its first record. The store reads the head turn by turn from the index's map
(`startReads`): a jump's turn first, then what the window shows (`wantWindow`, after every
transaction), then the rest from the tail backwards, in pieces of `headPiece` whose records must be the
ones the facts describe (else the session starts over), each waiting until the page takes it
(`takeFills`). A fill is a rewrite in place of its own units (`Projection.fill`; a records change
naming its run, `{from, to}`, redraws only that run); one that would redraw a mounted unit at or above
the reader waits for a still view and no thumb held (D5). A turn tap lands at once on its placeholder;
a record jump (a task, an agent, an artifact, a link) reads its turn and lands when its unit is whole,
unless the reader moves first (`jumpOrFetch`, `owedRecordFor`). While the head is out the index answers
for the records below `upto` and the scans only for those above it (`scanFrom`). The pending card is
an estimator kind of its own (it used to teach the process mean). A measure the observer delivers late,
after the reader scrolled into a pad, mounts the window where they are. **A reader keeps about 16 MB
of the head** (#s285, §6.6; Hong on #s290: "Read up to about 16 MB, then only what I visit"): the
background read stops before the turn that would take the head's reads past `KEEP_BYTES`
(`?keepBytes=` for a case), every ask lapses after each transaction except what the window shows and
the jump still waited on (`unwant`), a read starts once every ask made with it is in, and after each
fill the read turns farthest from the reader go back to `PENDING` (`dropFar`, the store's `drop`),
placeholders again under their keys at their measured heights: the sums do not move and nothing
mounted is redrawn. Never a turn within twice the window's reach, the read in flight, one with a piece
waiting to go in, or the head's last turn when the tail goes on with it. The counts, search and the
filter answer for those records from the index and the server, so a drop changes none of them; the
filter remembers what it opened (`filterSeen`). A task's "Go to the turn" trusts a held record only in
the tail and otherwise asks the server's search for the latest record naming it, by its subject or
`#<id>` (`findTaskRecord`; a TaskUpdate carries only the id). A
head under the budget is read whole, as before. The cases are `tests/records_by_turn.rs` (ports
2770–2779, 2782–2784, 2785–2789 and 2790, with a shim that queues the head's reads). **Search on
the server** (#s278, `design/slow-links.md` §7, stage 3): while a records-by-turn head is out, search
runs on Enter and the server answers for the records below `upto` (`/search?session&epoch&upto&q`,
the page's structured query — needle, whole words, class mask, resolved tools — never the box's
grammar, which stays the page's); the page counts the rest and sums (`remoteFound`: text and the
filter keep an answer each, dropped with the index; "Searching…" until it lands, and after five failed
asks, a 409 included, "earlier turns not searched"), the filter's matches include records still on
their way, a step onto an unfetched hit reads its turn and lands on its mark (`landHeldJump`), and a
step while the answer is out waits for that answer unless the reader moves or starts another search
(`stepWhenFound`); any search a control starts submits the box as it stands. The server's matcher
(`html_export/search_match.rs`) is `shared/search.js`'s matching half ported literally, and
`parity_with_the_page` runs both (node) over the rendering audit's corpus and edge records for 4,000+
queries; the reply quotes no text, and a client the mask policy covers (#365) is searched over masked
records (`Request::masked`), or its counts would read a masked secret back. The rest of the slow-link
work is `design/slow-links.md`.

**`claude-replay`** (root) — the thin assembly crate: clap CLI (`run_viewer`), `jdi/` the
**`agent-jdi`** binary (unattended-run supervisor; see `src/jdi/DESIGN.md`), and compat
re-exports so `claude_replay::model`, `claude_replay::tui::app`, … keep their old paths.

The viewer's phased plan (P0–P8) is **built** — see `DESIGN.md` for the design
notes and the open backlog. Borrowed ideas are credited in `ATTRIBUTION.md`.
