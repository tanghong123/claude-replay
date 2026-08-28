# CLAUDE.md — claude-replay

A Rust + ratatui terminal UI viewer (**binaries: `agent-replay` / `agent-monitor` /
`agent-monitor-fleet` since v1.101.0** — crate and repo names keep the `claude-` prefix;
brew installs symlink the old command names). It is **fully testable headless (no TTY)** —
never skip, stub, or defer a feature "because it needs a terminal."

## Work tracking
**`BACKLOG.md` is the state of record for pending work** — read it before picking up
a task, and update it in the same commit that changes an item's state (started,
decided, shipped, parked). Design docs argue, issues discuss, BACKLOG.md tracks;
don't trust a `design/*.md` status header alone — the tracker exists because those
drift.

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
  Scroll/viewport changes to `export.js` must extend this harness. The crate sits OUTSIDE
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
