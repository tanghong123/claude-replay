# The byte-identical output gate

`gate.sh` builds the working tree, renders a fixed fixture set (text dumps, single-file
HTML, bundles) and diffs it against a known-good `BASE`, printing
`BYTE-IDENTICAL: PASS/FAIL`. Every change is gated on PASS (alongside
`cargo fmt --check` / `clippy` / `cargo test`).

- **Scripts** live here (in git). **Data** — `BASE/`, `NOW/`, and the frozen input
  `frozen_self.jsonl` — lives in `$SC_GATE_DIR` (default `~/.cache/claude-replay-gate`,
  set in one place by `gate-dir.sh`), out of git:
  the fixtures embed real session content.
- **Version normalization:** HTML outputs embed the build's version (#55); the two
  carriers (topbar brand span, meta `"version"` field) are normalized on both sides
  before diffing so release bumps stay PASS. Text dumps are compared raw.
- **Intentional output changes** legitimately FAIL: verify the diff line-by-line
  (only the intended change — structural jsonl comparison by record kind helps),
  re-baseline the changed files into `BASE` with `/bin/cp -f` (bundles: `cp -Rf`),
  re-run to PASS, and document the verified diff in the commit message.
- **Lost BASE** (e.g. `/tmp` cleared): `rebaseline.sh <known-good-binary>` — use the
  released binary of the last shipped version, not the working tree, so the gate
  still measures your changes. If `frozen_self.jsonl` is also gone, re-freezing
  changes the fixture content; regenerate BASE in the same step and note it.
