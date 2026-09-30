#!/bin/sh
# scripts/tap-clean.sh <tap clone> <where> <why dirty matters>
#
# Is the installed corp tap clone clean enough to publish into, or to prove a publish from (#327)?
# corp-publish.sh asks at preflight and at verify. Exit 0: clean. Exit 2: it is not, and it says why.
#
# The clone is SHARED with alibrew itself, which rewrites its working tree while it updates: on
# 2026-09-29 two publishes stopped at verify on an untracked `install.sh`, then on a modified
# `Formula/alibrew.rb`, each while the alibrew team was pushing — and each was clean again within
# minutes. So:
#  - a dirty path that is one of OUR formulae stops at once: uncommitted formula edits are exactly
#    how five releases went unpublished, and no one else writes them;
#  - any other dirty path is waited out, rechecked every TAP_WAIT_STEP seconds (15) for up to
#    TAP_WAIT_MAX seconds (180), saying what it waits on — and stops if it is still dirty.
# TOOLS names our formulae (Formula/<tool>.rb); corp-publish.sh passes its own list.
set -u
TAP=$1 WHERE=$2 WHY=$3
TOOLS=${TOOLS:-agent-replay agent-monitor agent-monitor-fleet agent-jdi}
STEP=${TAP_WAIT_STEP:-15} MAX=${TAP_WAIT_MAX:-180}
ours=$(printf '%s\n' $TOOLS | sed 's#.*#Formula/&.rb#')

waited=0
while :; do
  dirty=$(git -C "$TAP" status --porcelain) || { echo "### STOP: $WHERE: cannot read the tap's status at $TAP" >&2; exit 2; }
  [ -z "$dirty" ] && exit 0
  # A porcelain line is two status letters, a space and the path (`a -> b` for a rename).
  paths=$(printf '%s\n' "$dirty" | cut -c4- | sed 's/.* -> //')
  if printf '%s\n' "$paths" | grep -qxF "$ours"; then
    git -C "$TAP" status --short >&2
    echo "### STOP: $WHERE: one of our formulae is dirty in the tap — $WHY" >&2
    exit 2
  fi
  if [ "$waited" -ge "$MAX" ]; then
    git -C "$TAP" status --short >&2
    echo "### STOP: $WHERE: the tap working tree is still dirty after ${MAX}s (none of it ours) — inspect $TAP" >&2
    exit 2
  fi
  echo "### $WHERE: the tap is changing under us ($(printf '%s\n' "$paths" | tr '\n' ' ')— none of it ours; alibrew updating itself?), rechecking in ${STEP}s"
  sleep "$STEP"
  waited=$((waited + STEP))
done
