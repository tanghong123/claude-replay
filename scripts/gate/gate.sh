#!/bin/bash
# Build the working tree, dump the fixed set, diff against $SC_GATE_DIR/BASE.
# Prints BYTE-IDENTICAL: PASS/FAIL.
#
# HTML outputs embed the producing build's version (#55: the topbar brand + the meta
# record's "version" field), which would churn every release — those two tight patterns
# are NORMALIZED on both sides before diffing (text dumps stay version-free and raw).
# Data (BASE, NOW, the frozen inputs) lives in $SC_GATE_DIR — see gate-dir.sh / README.md.
# A missing BASE *or* a missing frozen input means rebaseline first.
set -u
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
. "$SCRIPT_DIR/gate-dir.sh"
cd "$SCRIPT_DIR/../.."
if [ ! -d "$GATE_DIR/BASE" ]; then
  echo "NO BASE at $GATE_DIR/BASE — run scripts/gate/rebaseline.sh <known-good-binary> first."
  echo "BYTE-IDENTICAL: FAIL"
  exit 1
fi
# Guard the INPUTS too, symmetrically. Without this the dumps below write 0 bytes and the
# run reports a whole-file DIFF — a missing fixture masquerading as a huge regression.
# ALL of them are frozen copies (#147): the gate measures the binary, never the machine.
for f in frozen_self frozen_claude_sa frozen_codex; do
  if [ ! -f "$GATE_DIR/$f.jsonl" ]; then
    echo "NO INPUT at $GATE_DIR/$f.jsonl — the frozen fixture is gone."
    echo "  re-freeze + regenerate BASE from a known-good binary:"
    echo "  scripts/gate/rebaseline.sh \"\$(command -v claude-replay)\" <a-transcript.jsonl>"
    echo "BYTE-IDENTICAL: FAIL"
    exit 1
  fi
done
cargo build --release 2>&1 | tail -1
BIN=./target/release/claude-replay
OUT="$GATE_DIR/NOW"; rm -rf "$OUT"; mkdir -p "$OUT"
"$SCRIPT_DIR/verify.sh" "$BIN" "$OUT"
"$BIN" "$GATE_DIR/frozen_self.jsonl" --dump - --width 120    >| "$OUT/self.dump.txt" 2>/dev/null
"$BIN" "$GATE_DIR/frozen_self.jsonl" --dump-html - --width 120 >| "$OUT/self.html"     2>/dev/null
# Normalize ONLY the two version carriers: the brand span and the top-level meta field.
norm() {
  sed -E 's|<span class="brand-sub">v[0-9]+\.[0-9]+\.[0-9]+|<span class="brand-sub">vNORM|g; s|"version":"[0-9]+\.[0-9]+\.[0-9]+"|"version":"NORM"|g' "$1"
}
cmp_norm() { # $1=base file, $2=now file — normalized for html/jsonl, raw otherwise
  case "$1" in
    *.html|*.jsonl) diff <(norm "$1") <(norm "$2") >/dev/null 2>&1 ;;
    *)              diff -q "$1" "$2" >/dev/null 2>&1 ;;
  esac
}
FAIL=0
for f in "$GATE_DIR"/BASE/*.txt "$GATE_DIR"/BASE/*.html; do
  b=$(basename "$f")
  cmp_norm "$f" "$OUT/$b" || { echo "DIFF: $b"; FAIL=1; }
done
for d in "$GATE_DIR"/BASE/*.bundle; do
  b=$(basename "$d")
  if ! diff <(cd "$d" && find . -type f | sort) <(cd "$OUT/$b" && find . -type f | sort) >/dev/null 2>&1; then
    echo "DIFF bundle (file set): $b"; FAIL=1; continue
  fi
  while IFS= read -r rel; do
    cmp_norm "$d/$rel" "$OUT/$b/$rel" || { echo "DIFF bundle: $b/$rel"; FAIL=1; }
  done < <(cd "$d" && find . -type f | sort)
done
[ $FAIL -eq 0 ] && echo "BYTE-IDENTICAL: PASS" || echo "BYTE-IDENTICAL: FAIL"
