# The byte-identical output gate

`gate.sh` builds the working tree, renders a fixed fixture set (text dumps, single-file
HTML, bundles) and diffs it against a known-good `BASE`, printing
`BYTE-IDENTICAL: PASS/FAIL`. Every change is gated on PASS (alongside
`cargo fmt --check` / `clippy` / `cargo test`).

- **Scripts** live here (in git). **Data** — `BASE/`, `NOW/`, and the frozen inputs —
  lives in `$SC_GATE_DIR` (default `~/.cache/claude-replay-gate`, set in one place by
  `gate-dir.sh`), out of git: the fixtures embed real session content.
- **Every input is FROZEN** (`frozen_self`, `frozen_claude_sa`, `frozen_codex`,
  `frozen_codex_desktop`), never a
  path into a live agent store (#147). The gate must measure the BINARY; an input that can
  take a turn mid-run measures the machine instead. Observed before this rule: `claude_sa`
  pointed at a session still in use, and one turn landing during a run reported the dumps,
  the HTML *and* the bundle's whole file set as regressions — reproduced with the working
  tree stashed, so no code change was involved.
- **Freezing a Claude session means freezing its sub-agents too.** They resolve as
  `<dir>/<stem>/subagents/agent-<id>.jsonl` *relative to the transcript*, so a lone `.jsonl`
  copy silently renders without them — the sub-agent tool counts just vanish, which for the
  `claude_sa` fixture is the very thing it exists to cover. Copy the `subagents/` directory
  alongside, named for the **new** stem.
- **Version normalization:** HTML outputs embed the build's version (#55); the two
  carriers (topbar brand span, meta `"version"` field) are normalized on both sides
  before diffing so release bumps stay PASS. Text dumps are compared raw.
- **Intentional output changes** legitimately FAIL: verify the diff line-by-line
  (only the intended change — structural jsonl comparison by record kind helps),
  re-baseline the changed files into `BASE` with `/bin/cp -f` (bundles: `cp -Rf`),
  re-run to PASS, and document the verified diff in the commit message.
- **Lost BASE** (e.g. `/tmp` cleared): `rebaseline.sh <known-good-binary>` — use the
  released binary of the last shipped version, not the working tree, so the gate
  still measures your changes. If a frozen input is also gone, re-freezing changes the
  fixture content — the source path and the session id derived from it are IN the rendered
  output — so regenerate BASE in the same step and note it.
- **Re-freezing an input, end to end:** copy the transcript to
  `$SC_GATE_DIR/frozen_<name>.jsonl` (plus its `subagents/` tree as
  `$SC_GATE_DIR/frozen_<name>/subagents/` for a Claude session), run the gate, confirm the
  only diffs are the path/title/id, then re-baseline those files.
- **`frozen_codex_desktop` is synthetic** — the Codex Desktop prompt-envelope fixture from
  `docs/adapter-rendering-validation.md`, tracked at
  `claude-replay-agents/tests/fixtures/codex-desktop.jsonl` and copied into `$SC_GATE_DIR`
  by `gate.sh` when missing. It is the one input that CAN be regenerated from the repo.
