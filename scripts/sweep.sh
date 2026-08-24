#!/usr/bin/env bash
# Prune orphaned build artifacts from target/.
#
# WHY THIS EXISTS. Cargo never garbage-collects `target/`: every artifact it has ever built
# stays until something deletes it. A workspace-wide version bump changes the metadata hash of
# every crate and of every test, example and bin target, so it mints a COMPLETE new set of
# artifacts and orphans the previous one — permanently. This repo bumps the version on every
# completed task, which is what turned ~200 MB of live artifacts into a 64 GB `target/`: 241
# hash-variants of the engine rlib, 345 of the root test binary, 118 of the `parse` example.
# The size settings in Cargo.toml (`debug = "line-tables-only"`, deps at `debug = false`) were
# working the whole time — per-artifact sizes are tiny. Retention was the problem.
#
# WHAT IT KEEPS. Not a guess and not an mtime heuristic: it asks cargo. Two builds with
# `--message-format=json` report the `filenames` of every unit the real gates need — dev with
# `--all-targets` (what `cargo test` and `cargo clippy --all-targets` build) and release (what
# `scripts/gate/gate.sh` builds). Everything else in `deps/` and `examples/` is an orphan by
# construction. So a sweep costs NO rebuild: run it right after a build and the next build is
# still fully warm.
#
# Also removed: `incremental/` (the global cargo config sets `incremental = false` for sccache's
# sake, so anything there is legacy), stale `.fingerprint` entries, the headless-Chrome profiles
# the browser harness leaves behind, and test scratch older than a day — `.cargo/config.toml`
# points TMPDIR at `target/`, so pid-keyed scratch dirs collect here in the tens of thousands
# (#164 moved them out of macOS's unsweepable /var/folders precisely so this could reach them).
#
# Usage:  scripts/sweep.sh [--dry-run]
set -euo pipefail

cd "$(dirname "$0")/.."
TARGET="$PWD/target"
DRY=""
[[ "${1:-}" == "--dry-run" ]] && DRY=1

[[ -d "$TARGET" ]] || { echo "no target/ — nothing to sweep"; exit 0; }

usage() { du -sk "$TARGET" 2>/dev/null | awk '{print $1}'; }
BEFORE=$(usage)

LIVE=$(mktemp)
trap 'rm -f "$LIVE"' EXIT

echo "==> asking cargo which artifacts are live"
{
  cargo build --all-targets --message-format=json 2>/dev/null || true
  cargo build --release --message-format=json 2>/dev/null || true
} | python3 -c '
import json, sys
for line in sys.stdin:
    try:
        m = json.loads(line)
    except ValueError:
        continue
    if m.get("reason") == "compiler-artifact":
        for f in m.get("filenames") or []:
            print(f)
' | sort -u > "$LIVE"

live_count=$(wc -l < "$LIVE" | tr -d ' ')
if [[ "$live_count" -lt 20 ]]; then
  echo "refusing to sweep: cargo reported only $live_count live artifacts (a failed build?)" >&2
  exit 1
fi
echo "    $live_count live artifact files"

# Orphans in deps/ and examples/: on disk, absent from the live set. One pass in Python rather
# than a `grep` per file — there are six figures of them.
echo "==> pruning orphaned artifacts"
DRY="$DRY" LIVE="$LIVE" TARGET="$TARGET" python3 -c '
import os

dry = bool(os.environ.get("DRY"))
target = os.environ["TARGET"]
with open(os.environ["LIVE"]) as fh:
    live = {line.rstrip("\n") for line in fh}

for rel in ("debug/deps", "debug/examples", "release/deps", "release/examples"):
    d = os.path.join(target, rel)
    if not os.path.isdir(d):
        continue
    dropped = freed = 0
    with os.scandir(d) as it:
        for e in it:
            if not e.is_file(follow_symlinks=False) or e.path in live:
                continue
            try:
                size = e.stat(follow_symlinks=False).st_size
                if not dry:
                    os.unlink(e.path)
                dropped += 1
                freed += size
            except OSError:
                pass
    kept = sum(1 for f in live if f.startswith(d + os.sep))
    verb = "would drop" if dry else "dropped"
    print("    %-18s %s %d orphans (%.1f GB), kept %d live"
          % (rel + ":", verb, dropped, freed / 2 ** 30, kept))
'

# Legacy incremental state: the global cargo config disables incremental compilation (sccache
# cannot cache incremental units), so nothing here is ever read again.
for d in debug/incremental release/incremental; do
  if [[ -d "$TARGET/$d" ]]; then
    if [[ -n "$DRY" ]]; then
      echo "    would drop $d ($(du -sh "$TARGET/$d" | awk '{print $1}'))"
    else
      rm -rf "$TARGET/$d"
    fi
  fi
done

# Fingerprints for units whose artifacts are gone. They only make cargo re-check freshness once,
# so mtime is a good enough rule here.
for d in debug/.fingerprint release/.fingerprint; do
  [[ -d "$TARGET/$d" ]] || continue
  if [[ -n "$DRY" ]]; then
    echo "    $d: $(find "$TARGET/$d" -maxdepth 1 -mindepth 1 -mtime +7 | wc -l | tr -d ' ') stale"
  else
    find "$TARGET/$d" -maxdepth 1 -mindepth 1 -mtime +7 -exec rm -rf {} + 2>/dev/null || true
  fi
done

# Test scratch (TMPDIR lives here) and the browser harness's Chrome profiles.
echo "==> pruning test scratch"
scratch=(-name 'cr-*' -o -name 'chrome-search-profile*' -o -name 'sc-*' -o -name 'qw-*')
if [[ -n "$DRY" ]]; then
  echo "    $(find "$TARGET" -maxdepth 1 -mindepth 1 \( "${scratch[@]}" \) -mtime +1 | wc -l | tr -d ' ') entries older than a day"
else
  find "$TARGET" -maxdepth 1 -mindepth 1 \( "${scratch[@]}" \) -mtime +1 -exec rm -rf {} + 2>/dev/null || true
fi

AFTER=$(usage)
awk -v b="$BEFORE" -v a="$AFTER" -v dry="${DRY:-}" 'BEGIN {
  printf "%s: %.1f GB -> %.1f GB (%.1f GB %s)\n",
    (dry == "" ? "SWEPT" : "DRY RUN (nothing deleted)"),
    b/1024/1024, a/1024/1024, (b-a)/1024/1024,
    (dry == "" ? "reclaimed" : "would be reclaimed")
}'
