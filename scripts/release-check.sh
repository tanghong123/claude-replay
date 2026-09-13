#!/bin/sh
# scripts/release-check.sh — the release's version guard (#200).
#
# Reads the [workspace.package] version — the SECTION, never a naive `^version` match:
# `version.workspace = true` sits above it in Cargo.toml, and a release run that matched the
# first `^version` once moved main from 1.259.0 back to 1.258.0 — compares it with every `v*`
# tag (local; origin's too with --fetch), and refuses what history could not otherwise tell
# from a good release:
#
#   --next V    the release about to be cut: V must be above every existing tag and above the
#               workspace version, and must not be a tag already (a version is released once);
#   (no --next) the commit at hand: the workspace version must not be below the highest tag,
#               and a release commit (subject `release: vX …`) must name the workspace version,
#               with tag vX either absent or at HEAD.
#
# Exit 0 ok, 2 refused (the reason on stdout, prefixed REFUSED), 1 usage. --allow-backwards
# turns a refusal of the monotonic rules into a printed OVERRIDE and exit 0 — the explicit
# override the rule requires; nothing else bypasses it.
set -u
next=""; fetch=0; allow=0
while [ $# -gt 0 ]; do
  case "$1" in
    --next) next="${2:-}"; shift 2 ;;
    --fetch) fetch=1; shift ;;
    --allow-backwards) allow=1; shift ;;
    -h|--help) sed -n '2,19p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "usage: $0 [--next <version>] [--fetch] [--allow-backwards]" >&2; exit 1 ;;
  esac
done
root=$(git rev-parse --show-toplevel 2>/dev/null) || { echo "not a git repository" >&2; exit 1; }
cd "$root" || exit 1

# The [workspace.package] version and nothing else.
workspace_version() {
  awk '/^\[workspace\.package\]/{s=1;next} /^\[/{s=0} s && /^version *= *"/{sub(/^version *= *"/,""); sub(/".*$/,""); print; exit}' Cargo.toml
}
current=$(workspace_version)
[ -n "$current" ] || { echo "no [workspace.package] version in Cargo.toml" >&2; exit 1; }

# Dotted-number compare: -1, 0 or 1.
vercmp() {
  awk -v a="$1" -v b="$2" 'BEGIN{n=split(a,x,".");m=split(b,y,".");k=(n>m?n:m);for(i=1;i<=k;i++){p=(i<=n?x[i]+0:0);q=(i<=m?y[i]+0:0);if(p<q){print -1;exit}if(p>q){print 1;exit}}print 0}'
}

tags=$(git tag -l 'v*' | sed 's/^v//')
if [ $fetch = 1 ]; then
  remote=$(git ls-remote --tags origin 'refs/tags/v*' 2>/dev/null | sed -n 's#.*refs/tags/v\([0-9][0-9.]*\)$#\1#p') || remote=""
  tags=$(printf '%s\n%s\n' "$tags" "$remote")
fi
highest=""
for t in $tags; do
  case "$t" in ""|*[!0-9.]*) continue ;; esac
  if [ -z "$highest" ] || [ "$(vercmp "$t" "$highest")" = 1 ]; then highest=$t; fi
done
tagged() { printf '%s\n' $tags | grep -qx -- "$1"; }
refuse() {
  if [ $allow = 1 ]; then echo "OVERRIDE (--allow-backwards): $1"; exit 0; fi
  echo "REFUSED: $1"; exit 2
}

if [ -n "$next" ]; then
  printf '%s' "$next" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' || { echo "REFUSED: '$next' is not a MAJOR.MINOR.PATCH version"; exit 2; }
  if tagged "$next"; then refuse "v$next is already a tag — a version is released once"; fi
  if [ -n "$highest" ] && [ "$(vercmp "$next" "$highest")" != 1 ]; then
    refuse "$next is not above the highest tag v$highest — a release only moves forward"
  fi
  if [ "$(vercmp "$next" "$current")" != 1 ]; then
    refuse "$next is not above the workspace version $current"
  fi
  echo "OK: next $next is above the highest tag v${highest:-none} and the workspace version $current"
  exit 0
fi

if [ -n "$highest" ] && [ "$(vercmp "$current" "$highest")" = -1 ]; then
  refuse "the workspace version $current is below the highest tag v$highest — a commit moved it backwards"
fi
subject=$(git log -1 --format=%s 2>/dev/null || true)
case "$subject" in
  "release: v"*)
    named=$(printf '%s' "$subject" | sed -E 's/^release: v([0-9.]+).*$/\1/')
    [ "$named" = "$current" ] || refuse "the release commit names v$named but the workspace version is $current"
    at=$(git rev-parse -q --verify "refs/tags/v$named^{commit}" 2>/dev/null || true)
    if [ -n "$at" ] && [ "$at" != "$(git rev-parse HEAD)" ]; then
      refuse "v$named is already a tag at $at — this commit claims a version released elsewhere"
    fi
    ;;
esac
echo "OK: workspace version $current, highest tag v${highest:-none}"
exit 0
