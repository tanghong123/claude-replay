#!/usr/bin/env bash
# scripts/sweep-scratch.sh <target-dir> [--dry-run] — the test-scratch half of scripts/sweep.sh.
#
# `.cargo/config.toml` points TMPDIR at target/, so every test's scratch lands here, keyed by
# the pid of the process that made it. Two rules (#164, #205):
#
#   1. LIVENESS, for the browser harness's roots — `cr-browser-follow-<pid>-<case>` (a case's
#      hermetic stores, ~0.3 GB for a walk over a long session), `cr-browser-state-<pid>` and
#      `cr-browser-chrome-<pid>-<n>` (a Chrome profile). A root is dead the moment the test
#      process that made it is gone, however young: one per case per run, a suite several times
#      a day, and the age rule below kept 43 GB of them on 2026-09-13. A running suite's roots
#      are never in range — their pids are alive — so this is safe beside a live run.
#   2. AGE, for every other scratch name (`cr-*`, `sc-*`, `qw-*`, `chrome-search-profile*`):
#      older than a day, since a name without a pid says nothing about who still needs it.
#
# Prints one line per rule in sweep.sh's "dropped N (X GB)" style. `--dry-run` deletes nothing.
set -euo pipefail
TARGET="${1:-}"
[[ -n "$TARGET" && -d "$TARGET" ]] || { echo "usage: $0 <target-dir> [--dry-run]" >&2; exit 1; }
DRY=""
[[ "${2:-}" == "--dry-run" ]] && DRY=1
verb() { [[ -n "$DRY" ]] && echo "would drop" || echo "dropped"; }

# 1. Liveness.
n=0; freed=0
for d in "$TARGET"/cr-browser-follow-[0-9]*-* "$TARGET"/cr-browser-state-[0-9]* "$TARGET"/cr-browser-chrome-[0-9]*-*; do
  [[ -e "$d" ]] || continue
  pid=$(basename "$d" | sed -E 's/^cr-browser-(follow|state|chrome)-([0-9]+).*$/\2/')
  [[ "$pid" =~ ^[0-9]+$ ]] || continue
  if ps -p "$pid" > /dev/null 2>&1; then continue; fi   # its test process is alive — never in range
  size=$(du -sk "$d" 2>/dev/null | awk '{print $1}')
  [[ -n "$DRY" ]] || rm -rf "$d"
  n=$((n + 1)); freed=$((freed + ${size:-0}))
done
printf "    harness roots of dead test processes: %s %d (%.1f GB)\n" "$(verb)" "$n" "$(awk -v k="$freed" 'BEGIN{printf "%.3f", k/1024/1024}')"

# 2. Age.
scratch=(-name 'cr-*' -o -name 'chrome-search-profile*' -o -name 'sc-*' -o -name 'qw-*')
old=$(find "$TARGET" -maxdepth 1 -mindepth 1 \( "${scratch[@]}" \) -mtime +1 | wc -l | tr -d ' ')
[[ -n "$DRY" ]] || find "$TARGET" -maxdepth 1 -mindepth 1 \( "${scratch[@]}" \) -mtime +1 -exec rm -rf {} + 2>/dev/null || true
echo "    other scratch older than a day: $(verb) $old"
