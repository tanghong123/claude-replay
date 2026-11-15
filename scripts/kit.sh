#!/usr/bin/env bash
# Packs the kit (#s432): the session view and the session list as a host mounts them, which each
# release publishes beside its binaries (design/embedding.md, "Delivery").
#
#   scripts/kit.sh <out-dir>
#
# writes <out-dir>/agent-monitor-kit-<version>.tar.gz and <out-dir>/agent-monitor-kit-<version>.sha256
# (sha256sum's format, as each binary's digest is), and prints the archive's path. With GNU tar (the
# Release workflow's, or `gtar` on macOS) the archive is the same bytes from the same commit: its
# files sorted, owned by 0, dated at the commit, gzip without a name or a time. macOS's own tar packs
# the same files, owned by 0, without the dates fixed: enough to check a kit by hand.
set -euo pipefail

out=${1:?usage: scripts/kit.sh <out-dir>}
root=$(cd "$(dirname "$0")/.." && pwd)
mkdir -p "$out"
out=$(cd "$out" && pwd)

if tar --version 2>/dev/null | grep -q 'GNU tar'; then TAR=(tar)
elif command -v gtar >/dev/null 2>&1; then TAR=(gtar)
else TAR=()
fi
if command -v sha256sum >/dev/null 2>&1; then SHA="sha256sum"; else SHA="shasum -a 256"; fi

stage=$(mktemp -d "${TMPDIR:-/tmp}/agent-monitor-kit.XXXXXX")
trap 'rm -rf "${stage:?}"' EXIT

dir=$(cd "$root" && cargo run -q --locked -p claude-monitor --example kit -- "$stage")
name=$(basename "$dir")
[ -n "$name" ] && [ -d "$stage/$name" ] || { echo "the kit was not written" >&2; exit 1; }
epoch=$(git -C "$root" log -1 --format=%ct)

if [ ${#TAR[@]} -gt 0 ]; then
  "${TAR[@]}" --sort=name --owner=0 --group=0 --numeric-owner --mtime="@$epoch" --format=ustar \
    -C "$stage" -cf - "$name" | gzip -n -9 > "$out/$name.tar.gz"
else
  # bsdtar: no extended attributes or AppleDouble files, and no owner's name.
  COPYFILE_DISABLE=1 tar --no-mac-metadata --no-xattrs --uid 0 --gid 0 --uname '' --gname '' \
    --format ustar -C "$stage" -cf - "$name" | gzip -n -9 > "$out/$name.tar.gz"
fi
(cd "$out" && $SHA "$name.tar.gz" > "$name.sha256")
echo "$out/$name.tar.gz"
