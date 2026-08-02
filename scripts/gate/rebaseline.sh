#!/bin/bash
# Regenerate the gate's BASE fixtures from a KNOWN-GOOD binary.
#   scripts/gate/rebaseline.sh <binary> [frozen-source.jsonl]
# Refuses to run without an explicit binary — rebaselining declares "this output is
# correct", so it must be deliberate. If frozen_self.jsonl is missing, pass the live
# transcript to re-freeze it (NOTE: a re-freeze changes the fixture content — every
# BASE entry regenerates from the new freeze, so do this only when starting over).
set -eu
BIN="${1:?usage: rebaseline.sh <known-good-binary> [frozen-source.jsonl]}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
. "$SCRIPT_DIR/gate-dir.sh"
mkdir -p "$GATE_DIR"
if [ ! -f "$GATE_DIR/frozen_self.jsonl" ]; then
  SRC="${2:?frozen_self.jsonl missing — pass a transcript to freeze}"
  cp "$SRC" "$GATE_DIR/frozen_self.jsonl"
  echo "froze $SRC -> $GATE_DIR/frozen_self.jsonl"
fi
BASE="$GATE_DIR/BASE"; rm -rf "$BASE"; mkdir -p "$BASE"
"$SCRIPT_DIR/verify.sh" "$BIN" "$BASE"
"$BIN" "$GATE_DIR/frozen_self.jsonl" --dump - --width 120    >| "$BASE/self.dump.txt" 2>/dev/null
"$BIN" "$GATE_DIR/frozen_self.jsonl" --dump-html - --width 120 >| "$BASE/self.html"     2>/dev/null
echo "BASE regenerated at $BASE from $BIN"
