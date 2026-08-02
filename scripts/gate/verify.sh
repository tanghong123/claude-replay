#!/bin/bash
# Byte-identical gate dumper: render the fixed transcript set with $1 into $2/.
# Fixture inputs live in $SC_GATE_DIR — see gate-dir.sh / README.md.
set -u
BIN="$1"; OUT="$2"
. "$(cd "$(dirname "$0")" && pwd)/gate-dir.sh"
mkdir -p "$OUT"
CLAUDE_SA=/Users/hong/.claude/projects/-Users-hong-code-crux/19432415-3f0e-434e-8255-fa84407109db.jsonl
CLAUDE_SELF=/Users/hong/.claude/projects/-Users-hong-personal-claude-replay/094539f2-40d7-4703-a510-8c3ee69657a4.jsonl
CODEX=/Users/hong/.codex/sessions/2026/07/20/rollout-2026-07-20T22-37-46-019f7ff6-2664-7263-99dd-af17d013015b.jsonl
W=120
for name in claude_sa claude_self codex; do
  case $name in
    claude_sa) T=$CLAUDE_SA;;
    claude_self) T=$CLAUDE_SELF;;
    codex) T=$CODEX;;
  esac
  # These fixtures are live agent-store transcripts, so they can disappear (a store
  # pruned, a machine changed). Fail loudly: rendering a MISSING input writes 0 bytes,
  # which the gate would otherwise report as a whole-file content regression.
  if [ ! -f "$T" ]; then
    echo "MISSING FIXTURE: $name -> $T" >&2
    echo "  the gate cannot measure anything against it; re-point verify.sh or re-baseline." >&2
    exit 1
  fi
  "$BIN" "$T" --dump - --width $W          > "$OUT/${name}.dump.txt"   2>"$OUT/${name}.dump.err"
  "$BIN" "$T" --dump - --width $W --full   > "$OUT/${name}.full.txt"   2>"$OUT/${name}.full.err"
  "$BIN" "$T" --dump-html - --width $W      > "$OUT/${name}.html"       2>"$OUT/${name}.html.err"
  rm -rf "$OUT/${name}.bundle"
  "$BIN" "$T" --dump-all-html "$OUT/${name}.bundle" --width $W >/dev/null 2>"$OUT/${name}.bundle.err"
done
