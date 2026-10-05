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

## The monitor's two shells
`agent-monitor` and `agent-monitor-v2` each serve **two** frontends and both are supported: the
**app shell** (`claude-monitor/src/ui.rs` + `src/codex-ui/*`, the default) and the **classic**
page (v1's rail, v2's splice shell). A button in each switches and REMEMBERS the choice at
`<state_dir>/ui.json`, shared by both binaries; `?ui=classic` / `?ui=app` override for one
request without disturbing it, which is what makes side-by-side comparison possible. The classic
page is not deprecated — it goes when the app shell has been validated, and not before.
**The sidebar head is TWO LINES by construction** (#223): the brand takes the first, the controls
take the second and wrap within it. They never share a row, at any width — the shell switch is a
word rather than a glyph, and beside the brand it met the theme button. Below `SIDEBAR_TIGHT`
(272px) the shell wears `sidebar-tight`, which squeezes the head's padding, the gaps and the glyphs
so all six stay inside the sidebar at its 232px minimum (#210); the switch is exempt from the 30px
glyph width there, since shaving a word is what clipped it. Nothing is hidden at any width; a new
control in that row is measured against the minimum, not the default, and the case hit-tests the
switch against the brand and the theme glyph rather than eyeballing it.
**On a phone (≤760px, #310 then #313) the app shell is a reader and a native-feeling app**, the
owner's rules: the transcript runs edge to edge (a 13px gutter), controls may FLOAT, and controls of
little use on a phone are hidden. The session list is a DRAWER over part of the view (a scrim over the
rest closes it on a tap; `mobile-detail` = shut), opened and closed by a handle fixed at the top left —
a sibling of the drawer in `#app`, because the workspace is isolated under the scrim and anything in
the top bar would sit under the drawer. The top bar is two rows: handle slot, title, Info, right pane;
then Turns, Tasks, Agents, search/filter, Aa; the turn header is the third. That second row never
wraps (#352): the search takes at most what the four 44px glyphs leave, and the CLOSED box with a
query is its count and steps only — its chips (a scope chip is its letters alone, a tool chip wears
a wrench; the word visually hidden, #353) and the clear control (×, every width, whenever there is a
query) wait in the open box. There the chips and the field are ONE strip (`.search-field`, wrapped at
load) that scrolls as a whole, the field as wide as its text, so the end being typed stays in view;
the count hangs as a tag under the box (the open box stacks above the turn's sticky bar, z 27 > 26);
and the step arrows are SVG, centred. A tool chip is its wrench and the tools' own single-character
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
exist there: each pane opens from its icon as a drop-down that holds the outline's OWN live list (moved
in while open, back after), so one renderer and one click handler (`navigatorClick`) serve both. Every
sheet hangs at `--phone-top` (the bar's measured bottom). Inputs are 16px (iOS zooms into anything
smaller), targets 44px. Choosing a session shuts the drawer at once and a veil (`sessionLoading`, every
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
local files requires pairing". Both binaries now hand the gate `pair_phone::trusted_hosts(port)`: the
names this machine's `tailscale serve` relays to THIS port (read lazily, again on a miss at most every
30 s), plus any `AGENT_MONITOR_TRUSTED_HOSTS` lists — never a `*.ts.net` wildcard. On Linux the gate
also admits a VERIFIED same-user loopback peer without the cookie (D3b, `/proc/net/tcp`); macOS cannot
verify one. `AGENT_MONITOR_TOKEN_ONLY=1` turns that leg off for a paired gate — stricter only — and
the browser suite sets it on every paired monitor, so a case's unpaired browser is refused on Linux
as on macOS (#366). The pairing
cookie is named for the token it carries — `cmauth-` and eight hex digits of the token's SHA-256
(`cookie_name`, #350) — because a cookie is scoped to the HOST, not the port: while every gate wrote
`cmauth`, agent-metrics' `serve --phone` (this gate since #317, a token of its own) replaced the
monitor's cookie and the owner was told the monitor was not paired. The two monitors share one token
and so one cookie; the legacy `cmauth` is still read (admitting only on this gate's token) and is
answered with the gate's own. It is `SameSite=Strict`, which a browser WITHHOLDS from a navigation
that did not start on the site (a link in another app; the owner met it on a phone after saving a
file, #341), so a refused PAGE load gets `HttpResponse::refusal_page`: it reloads once from itself
— same-site, so the cookie comes along — bounded by a 10 s sessionStorage stamp, then shows the
host's refusal words; fetches keep the plain 401, and each refused page load is one stderr line
(never the token), which names a legacy `cmauth` that did not match apart from the gate's own.
**The page saves to a Home Screen as itself** (#320): the tab icon's green mark, square and opaque
(`claude-monitor/src/icons/`, rendered from `app-icon.svg` by `scripts/render-app-icon.sh`), named by
the app shell's and v1's classic heads with a manifest and the label "Agent Monitor"; iOS asks the root
for `apple-touch-icon.png` on its own, which covers v2's classic page. Those files are the ONLY paths
the gate answers unpaired (`ui::PUBLIC_ASSETS` → `AuthGate::with_public`, GET only), because a phone
fetches the icon itself and may not carry the pairing cookie. The manifest says `display: browser` on
purpose: an iOS standalone Home Screen app keeps cookies of its own and would open unpaired.
**Sessions sort into three buckets** (#202, `design/agent-states.md` §10): active (busy),
blocked (a wait, or an idle reason that cut the work short — this is what "needs attention"
means and counts) and idle (`done`, `exited`); `sessionBucket` in `shared/state-labels.js` is
the one definition, held to the tracker's enum by a test, and a turn that ended with an answer
is idle, not blocked. The app shell's session filter (a glyph in the sidebar's head-actions row,
between Expand every group and the sidebar collapse, opening Active recently / Blocked / Idle /
Include hidden checkboxes; Active recently and Blocked by default, remembered, never an empty
set; "Everything" checks the three and leaves Include hidden alone) filters by a COVER of that partition: Active recently is an hour of
activity or busy now and overlaps Blocked by design (`sessionFilterBuckets`). It replaced the
Needs attention and Show Hidden controls; the classic rail keeps its All / Active / Idle pills on
the legacy `state`.

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
fork's engine (`--fork-session --resume <parent.jsonl>`) is the only marker of a Claude fork
anywhere, and `note_forks_from_argv` is how one joins its #142 family. Diagnose against the
machine, not the API: `term`/`injectable` describe the process the row was LINKED to, so a false
link reads exactly like a correct refusal — `tmux -L <sock> list-panes -a` and `ps -axww` first.

**Markdown in the preview pane is mdrev's viewer, as a guest** (#270,
`design/mdrev-in-the-preview-pane.md`), and **mdrev is PINNED** (#274, the owner: "only depend on
static version of mdrev, similar to how agent-monitor depends on crates in claude-replay. Future
upgrades will be triggered explicitly and manually"). `vendor/mdrev` is a crate holding ONE public
mdrev release's embedding kit, unmodified (`release/`: `bundle/`, `mdrev-cli.js` + `package.json`,
`docs/*.md`); its version IS the release's (1.1.20, #s6), `release.sha256` is checked by its tests (never
hand-edit a vendored file), and its build script embeds `bundle/` as a table. Only `claude-monitor`
depends on it, and `routes::handler` — the one constructor both binaries go through — installs it
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
The pinned `mdrev-cli conform` against a live monitor is the definition of done
(`mdrev_contract_passes_mdrev_cli_conform`); the mdrev browser cases need nothing installed and run in
CI too. Keys: `bindKeymap` tracks ENGAGEMENT as mdrev does — the last click or focus inside
`[data-guest-keys]` — and yields every key while it holds; the host element is NOT focusable. The
request parser REFUSES (413) a body over its route's bound instead of silently cutting it to 64 KB;
`hold` gets the artifact cap.
**The pane offers no SHARED review** (#s6, mdrev 1.1.18+: threads in a review store, pairing, a
Push): every `review` route and a thread's `hide` answer the contract's 404, after which the guest
draws no Share control and no Push, and the host never passes mdrev's viewer key or `--viewer`,
without which `mdrev-cli` does nothing shared. The pane reads and takes this machine's local notes;
shared threads are mdrev's own viewer's business. Wiring them in is a decision for the owner.

`src/codex-ui/{reference.css,reference-shell.html,icons.js}` are **generated**, extracted
byte-for-byte from `design/agent-monitor-codex-demo.html` by
`scripts/extract-agent-monitor-demo.mjs` and checked by two tests. Never hand-edit them: change
the demo and re-run the script. Production-only chrome (the shell switch) is layered on at
runtime from `app.js` so the extraction stays exact.

**Shared frontend modules live in `claude-replay-html/src/html/shared/`** (seam 0 of
`design/monitor-shell-duplication.md`, v1.140.0): ONE source, consumed two ways — the monitor
serves each unchanged as an ES module at `/monitor-ui/shared/<name>.js` (`ui::asset()`), and the
html crate INLINES each into its self-contained pages ahead of `export.js`
(`html_export/shared.rs`), where export.js reads it as `window.__shared.<name>`. The inliner is
textual, so a shared module keeps two conventions, held by tests: no imports, and exactly one
trailing `export { … };` line. A new module is one row in `SHARED` (`shared.rs`); the monitor's
import-closure test and the html crate's convention test cover the rest. The classic rail
(`claude-monitor/src/rail.html`) and the v2 splice shell (`claude-monitor-v2/src/shell.html`) take the
same inlined block at serve time through their `{{SHARED}}` placeholder
(`claude_replay_html::shared_inline_all()`), so `window.__shared` there is a template substitution,
not an import (#43). `claude-monitor/src/codex-ui/shared` is a SYMLINK to that directory, there so
node (the contract test imports `components.js`, which imports `./shared/…`) and editors resolve the
relative paths; the served bytes still come from the html crate through `ui::asset()`.

`claude-monitor-v2/tests/ui_contract.mjs` holds the frontend contract that only JS can answer.
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
PNG, measured), so the image gets reveal and the app shell's lightbox opens the original, falling back
to the embedded copy if the original is gone. A sub-agent gets its root's uploads. A refusal says
which (`Unservable`, #s7): 410 for a file that is gone, 403 with the ceiling's words for one held
back, 404 only for a path the page never offered; the preview pane shows them as they are.

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
keys, JWTs, bearer tokens, a private key's body, a URL's password) replacing the secret with `*` of
the SAME length — `/records` range-reads by byte offset, and a JSON string stays one — never
touching a backslash or the byte after it, nor a `data:` URI or a long base64 run (a pasted image).
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
process-surface card did. Not on a phone (#335): its reader is not at the machine a Finder window
would open on, so `revealHere()` (`shared/capabilities.js`, the 760px breakpoint) withholds every
reveal — a reveal-only file is COPIED instead — and the pane's ↗, whose page has no way back.
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
  `cargo build --release -p claude-monitor -p claude-monitor-v2 && cargo test -p claude-replay-browser-tests -- --ignored --skip known_red`
  (a local Chrome; CI runs the same in its `browser` job). `tests/harness/mod.rs` is the kit
  every case uses: `Stores` (a hermetic world of agent stores under the case's scratch root —
  nothing a case measures comes from this machine's sessions), record builders and
  `long_session`, `Monitor::spawn` (v1 or v2 on a fixed port, reaped on drop; a missing binary
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
  structural cases (the html server's viewport contract; `the_app_shell_*` on ports 2831–2836;
  `the_classic_rail_*` on 2837–2838 against v1; `the_v2_shell_*`, the compose affordance on
  2841–2842). `tests/files.rs` holds #272's file affordances on the app shell (2811–2814) and #374's handed
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
  grew while the reader moved: the deltas replay as timed appends).
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
session, which the sweep prunes by the LIVENESS of the pid in the name, not by age (#205: the
age rule had kept 43 GB of them after one day's runs); a running suite's roots are never in
range, so the sweep is safe beside a live run. Everything else goes by age.
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

## Merging external PRs
CI must run and pass BEFORE the merge — a fork PR from a first-time contributor
needs its workflow runs approved in the GitHub UI, and until they run, the email
guard has never seen the commits. Never `gh pr merge --admin` past pending
checks: the pre-push hook cannot see a server-side merge, so CI's guard job is
the ONLY identity check on that path (#16 leaked a work email exactly this way;
the accepted commits are sha-allowlisted in ci.yml).

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
page is a session-list rail beside the html crate's session view in an iframe; scan/state/
cards in `src/index.rs`, the rail in `src/rail.html`; lazy population — a session's durable
entry (at the monitor's OWN root, `~/.cache/claude-monitor`) is written by VISITING it,
never by a sweep. It is **lib + bin**: `src/lib.rs` exposes `index` (the scan and the send
DECISIONS), `consent` (grants + the passcode), `cost`, `state`, `control` (the pairing
token and the two send transports, #133), and `routes` (THE route table, #47: the five API
arms, the assets and the service fallthrough, with each binary's classic page and v1's
`chrome=embed` session arm as named parameters), so `claude-monitor-v2/` reuses them rather than
forking them — one implementation of "may this prompt be injected into that pane", two
front-ends. Both monitors share the state dir (token, passcode, consent) because the
`cmauth` cookie is host-scoped, not port-scoped; they keep separate CACHE roots. Each
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
emits a JSON block stream, the JS renders it).

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
from `touchstart` until `whenStill`, a correction is absorbed into the top pad (`padDebt`, carried by
`contentTop`) or, if the pad cannot hold it, yielded; estimates and the tail wait, and so does a
head landing (`updateRecords` holds on `viewport.touchGlide` as well as the scroll clock, which hears a
glide only as fast as its events arrive — `a_phone_fling_heard_late_still_holds_the_head`); one
`settle` write pays the debt once still. The owner's export (`fixtures/history/phone-jitter-app.json`, the turn
swinging 444↔405) replays in `sandbox_phone_jitter_app_replays`; a touch gesture replays as an
iOS-style glide (`harness::touch_glide`: offset writes from the glide's own position with a scroll
event per frame — without that event a headless tab hears the glide lazily, which invents failures).
While the head is still coming the phone's Turns count is the highest turn NUMBER drawn (#342). The
classic page never asks for a tail and is unchanged.

**`claude-replay`** (root) — the thin assembly crate: clap CLI (`run_viewer`), `jdi/` the
**`agent-jdi`** binary (unattended-run supervisor; see `src/jdi/DESIGN.md`), and compat
re-exports so `claude_replay::model`, `claude_replay::tui::app`, … keep their old paths.

The viewer's phased plan (P0–P8) is **built** — see `DESIGN.md` for the design
notes and the open backlog. Borrowed ideas are credited in `ATTRIBUTION.md`.
