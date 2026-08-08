#!/bin/bash
# Byte-identical gate dumper: render the fixed transcript set with $1 into $2/.
# Fixture inputs live in $SC_GATE_DIR — see gate-dir.sh / README.md.
#
# Every input is a FROZEN copy in $SC_GATE_DIR, never a path into a live agent store
# (#147). The gate must measure the BINARY; an input that can take a turn while the
# gate runs measures the machine instead. That is not theoretical: `claude_sa` pointed
# at a session that was still being used, and a turn landing mid-run reported the dumps,
# the HTML and the bundle's whole FILE SET as regressions — reproduced with the working
# tree stashed, so no code change was involved.
#
# Re-freezing changes the fixture (the path and the derived session id are IN the
# rendered output), so BASE must be regenerated in the same step — see README.md.
set -u
BIN="$1"; OUT="$2"
. "$(cd "$(dirname "$0")" && pwd)/gate-dir.sh"
mkdir -p "$OUT"
W=120
# `claude_self` used to be rendered here from a 107 MB live transcript and then SKIPPED by
# gate.sh's comparison — four renders per run for no signal. `self` already covers a Claude
# session of this shape from a frozen copy, so the redundant one is gone (#147).
for name in claude_sa codex; do
  T="$GATE_DIR/frozen_${name}.jsonl"
  # A frozen fixture can still go missing (a cache wiped, a machine changed). Fail loudly:
  # rendering a MISSING input writes 0 bytes, which the gate would otherwise report as a
  # whole-file content regression.
  if [ ! -f "$T" ]; then
    echo "MISSING FIXTURE: $name -> $T" >&2
    echo "  re-freeze it and regenerate BASE in the same step — see scripts/gate/README.md." >&2
    exit 1
  fi
  "$BIN" "$T" --dump - --width $W          > "$OUT/${name}.dump.txt"   2>"$OUT/${name}.dump.err"
  "$BIN" "$T" --dump - --width $W --full   > "$OUT/${name}.full.txt"   2>"$OUT/${name}.full.err"
  "$BIN" "$T" --dump-html - --width $W      > "$OUT/${name}.html"       2>"$OUT/${name}.html.err"
  rm -rf "$OUT/${name}.bundle"
  "$BIN" "$T" --dump-all-html "$OUT/${name}.bundle" --width $W >/dev/null 2>"$OUT/${name}.bundle.err"
done
