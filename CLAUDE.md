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
import-closure test and the html crate's convention test cover the rest.

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
- **Browser (HTML live page):** `claude-replay-browser-tests/tests/browser_follow.rs` drives
  the real `--html` server in headless Chrome over CDP — the follow/anchor viewport contract
  lives in renderer-fired scroll events, layout clamping and native scroll anchoring, which
  only a real engine has. `#[ignore]`d (needs a local Chrome); run
  `cargo test -p claude-replay-browser-tests --test browser_follow -- --ignored`.
  Scroll/viewport changes to `export.js` must extend this harness. The same file holds the
  APP SHELL's cases (`the_app_shell_*`: layout, hide/restore, child→parent, scroll memory, the
  keymap) against `agent-monitor-v2 --release` on ports 2831–2836 with scratch state; a
  behaviour change in `codex-ui/` extends those the same way, and a served module the shell
  imports must be registered in `ui::asset()` (an import-closure test walks the graph). The crate sits OUTSIDE
  `default-members` — its `headless_chrome` dep is the heaviest thing the workspace compiles,
  so the LOCAL root gates (`cargo test`, `cargo clippy --all-targets`) never resolve it and
  never compile-check it. CI's `cargo test --all` does span every member, so a break in the
  harness surfaces there rather than under you; build it explicitly (`--no-run` is enough) if
  you would rather not learn that from CI.
- **Quick plain check:** `agent-replay <path|--latest> --dump -` renders to stdout
  (no TUI) — good for verifying parsing/markdown/diffs in a pipe. (`--dump <stem>` or
  bare `--dump` instead write `<stem>.txt` + `<stem>.ansi` at the terminal width or
  `--width N`; bare `--dump` deduces the stem.) `--dump` renders through the View
  pipeline and applies the TUI's default fold policy (add `--full` to expand all).
  `--dump - --json` instead emits the structured block stream (#34): JSON Lines, `kind`
  from the shared classification, per-TURN timestamps, tool `status`/`exit`/`ms` — the
  content half of the shell-out vocabulary (`--paths --all` is the discovery half).

## Test scratch
Tests build their scratch under `std::env::temp_dir()` — ~100 call sites across the
crates — and `.cargo/config.toml` points `TMPDIR` at the workspace's own `target/`,
so all of it stays inside the repo and `cargo clean` (or `scripts/sweep.sh`) clears it
(#164). It used to land in macOS's opaque `/var/folders/…`, which nothing sweeps: 8,014
directories and 267 MB had accumulated there. A full run leaves ~3.4 MB.
Scratch inside the repo is scratch inside a GIT repo, so the same file sets
`GIT_CEILING_DIRECTORIES=target` — a fixture that shells out to `git` sees no
repository, exactly as it did in the system temp. A test that spawns a `tmux`
server must hold the `Server` Drop guard (`tests/tmux_smoke.rs`), or a failed
assertion strands the server and whatever runs inside it.

## Releasing
After each completed CODE task (docs/design-only changes need no release): bump
`[workspace.package] version` in the root Cargo.toml, `cargo build` to refresh
Cargo.lock, commit, annotated signed tag (`git tag -a vX.Y.Z -m "..."`), push
`origin main` then the tag — the tag push triggers the Release workflow, which
publishes binaries and bumps the Homebrew tap. Verify the commit really landed
before tagging — a failed commit with the tag commands still running once
shipped a tag pointing at the wrong commit.

**Publish to BOTH taps** (owner, 2026-08-29). The tag push bumps the public Homebrew tap
(`tanghong123/tap`) on its own; the corp tap is a separate, manual step and does not happen
by itself:

1. Download the release's `agent-replay` / `agent-monitor` / `agent-monitor-fleet` /
   `agent-jdi` tarballs (four tools — the tap's history publishes all four) for all four
   targets and **verify each against its published `.sha256`** (`shasum -a 256 -c`) before
   republishing anything.
2. Copy them into a clone of `alibrew/artifacts` at `<tool>/<version>-<os>-<arch>/`
   (`darwin|linux` × `arm64|amd64`), keeping the release's own filename. Clone it
   `--filter=blob:none --no-checkout`, then `sparse-checkout init --cone` + `set` the sixteen
   NEW directories and check out `master` — a plain clone pulls every binary ever published.
   In that clone, never run anything that needs blob SIZES or contents outside the cone —
   `git lfs ls-files`, `ls-tree -l`, `git show HEAD:<big file>` — each missing blob is lazily
   fetched over one ssh round-trip (measured: 178 MB / 51 packs in 14 minutes before it was
   killed). The LFS guard is `grep filter=lfs .gitattributes` plus `git check-attr` on the new
   files; both read metadata only. The repo is SHARED (other tools publish to it): `git fetch
   origin master && git rebase origin/master` right before the push, or it is rejected as
   non-fast-forward — a knack release landed between clone and push on 2026-09-03. **Exactly two levels** — brew writes a single-line cone
   sparse-checkout pattern and cone mode materializes NOTHING deeper. Never LFS-track that
   repo. Push to `master` and take the commit sha.
3. Update `Formula/<tool>.rb` in `alibrew/homebrew-core` (branch `main`) — the installed tap
   at `$(brew --repository alibrew/core)` IS that clone, already on `main` with the corporate
   identity set, and `brew audit` reads it — with the new version (it is also inside each
   `only_path:`) and that sha as `revision:` — a git url takes no `sha256` and no `using: :git`. Verify with
   `ruby -c`, `brew style --except-cops=FormulaAudit/Urls`, then `brew audit --strict
   alibrew/core/<name>` (audit takes a NAME, not a path).

Both corp repos **reject a commit authored from a non-corporate email** — set the owner's
corporate address repo-locally in those clones only. Do not guess it: read it off the
existing commits in `alibrew/homebrew-core` (`git log --format='%an <%ae>'`). `a1 staff list
--query hongtang` does NOT report it — measured 2026-09-03, it returns five other people whose
nicknames romanize the same way, and `a1 staff get <mr-assignee-id>` is a platform id, not an
employee id, and names someone else again. It must never appear in THIS repo — not in a commit and not in a file, which
`.githooks/pre-push` enforces on the diff as well as the metadata (it caught this very
paragraph naming it outright). The two rules point opposite ways on purpose: one history is
public and the other is not.

**Then sweep: `scripts/sweep.sh`.** A version bump changes the metadata hash of every
crate and every test/example/bin target, so it mints a COMPLETE new set of artifacts and
orphans the previous one — and cargo never garbage-collects `target/`. Releasing per task
without sweeping is what grew `target/` to 64 GB (241 hash-variants of the engine rlib,
345 of the root test binary) against ~200 MB of live artifacts. The script asks cargo which
artifacts the real gates need (`--message-format=json`, dev `--all-targets` + release) and
deletes only what is in neither set, so it costs no rebuild — run it right after the build
and the next one is still warm. `--dry-run` first if in doubt.

`origin` (GitHub) is where the code is developed, where releases are cut, and
where issues are filed. `alibaba` (git@code.alibaba-inc.com:project-h/
claude-replay.git) is a MIRROR the owner asked restored (2026-08-21): push
`main` and tags to BOTH remotes — `git push origin main && git push alibaba
main` (and the tag to both on releases). It holds code only; issues and
releases stay on GitHub.

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
**`claude-monitor/`** — the machine-wide session index (#98): a loopback web service whose
page is a session-list rail beside the html crate's session view in an iframe; scan/state/
cards in `src/index.rs`, the rail in `src/rail.html`; lazy population — a session's durable
entry (at the monitor's OWN root, `~/.cache/claude-monitor`) is written by VISITING it,
never by a sweep. It is **lib + bin**: `src/lib.rs` exposes `index` (the scan and the send
DECISIONS), `consent` (grants + the passcode), `cost`, `state`, and `control` (the pairing
token and the two send transports, #133), so `claude-monitor-v2/` reuses them rather than
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
  (`block_kind`/`fold_key`, `relativize`).

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
emits an append-only JSON block stream, the JS renders it).

**`claude-replay`** (root) — the thin assembly crate: clap CLI (`run_viewer`), `jdi/` the
**`agent-jdi`** binary (unattended-run supervisor; see `src/jdi/DESIGN.md`), and compat
re-exports so `claude_replay::model`, `claude_replay::tui::app`, … keep their old paths.

The viewer's phased plan (P0–P8) is **built** — see `DESIGN.md` for the design
notes and the open backlog. Borrowed ideas are credited in `ATTRIBUTION.md`.
