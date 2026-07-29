#!/bin/bash
# Build the working tree, dump the fixed set, diff against $SC_GATE_DIR/BASE.
# Prints BYTE-IDENTICAL: PASS/FAIL.
#
# HTML outputs embed the producing build's version (#55: the topbar brand + the meta
# record's "version" field), which would churn every release — those two tight patterns
# are NORMALIZED on both sides before diffing (text dumps stay version-free and raw).
# Data (BASE, NOW, the frozen inputs) lives in $SC_GATE_DIR (default /tmp/sc-gate);
# a missing BASE means rebaseline first — see README.md / rebaseline.sh.
set -u
cd "$(dirname "$0")/../.."
GATE_DIR="${SC_GATE_DIR:-/tmp/sc-gate}"
if [ ! -d "$GATE_DIR/BASE" ]; then
  echo "NO BASE at $GATE_DIR/BASE — run scripts/gate/rebaseline.sh <known-good-binary> first."
  echo "BYTE-IDENTICAL: FAIL"
  exit 1
fi
cargo build --release 2>&1 | tail -1
BIN=./target/release/claude-replay
OUT="$GATE_DIR/NOW"; rm -rf "$OUT"; mkdir -p "$OUT"
"$(dirname "$0")/verify.sh" "$BIN" "$OUT"
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
  case "$b" in claude_self.*) continue;; esac
  cmp_norm "$f" "$OUT/$b" || { echo "DIFF: $b"; FAIL=1; }
done
for d in "$GATE_DIR"/BASE/*.bundle; do
  b=$(basename "$d")
  case "$b" in claude_self.*) continue;; esac
  if ! diff <(cd "$d" && find . -type f | sort) <(cd "$OUT/$b" && find . -type f | sort) >/dev/null 2>&1; then
    echo "DIFF bundle (file set): $b"; FAIL=1; continue
  fi
  while IFS= read -r rel; do
    cmp_norm "$d/$rel" "$OUT/$b/$rel" || { echo "DIFF bundle: $b/$rel"; FAIL=1; }
  done < <(cd "$d" && find . -type f | sort)
done
[ $FAIL -eq 0 ] && echo "BYTE-IDENTICAL: PASS" || echo "BYTE-IDENTICAL: FAIL"
