#!/bin/sh
# scripts/vendor-mdrev.sh <version> [--sha256 <hex>] [--from <tarball>]
#
# Move the mdrev pin (#274) — the ONLY way it moves. The monitors build ONE released mdrev into
# their binaries (vendor/mdrev, the `mdrev` crate) and look at nothing installed on the machine.
# The owner, 2026-09-25: "only depend on static version of mdrev, similar to how agent-monitor
# depends on crates in claude-replay. Future upgrades will be triggered explicitly and manually".
# So this is run BY HAND, for a version a person chose, and what it leaves is reviewed and
# committed as a change of its own.
#
# The source is mdrev's PUBLIC release — the kit alone, as its release notes call it:
#   https://github.com/tanghong123/homebrew-tap/releases/download/mdrev-<v>/mdrev-embed-<v>.tar.gz
# It is checked against the sha256 digest the release publishes for that asset (GitHub's API, which
# keeps one for every asset of every release), or against --sha256, before anything is read out of
# it. --from takes an already-downloaded tarball through the same check. (The application tarball
# beside it carries the same kit; 1.1.12's matched it byte for byte.)
#
# The steps, in order:
#   1. the tarball — downloaded (or --from), its sha256 checked against the release's digest;
#   2. the kit — bundle/mdrev.js and mdrev.css, mdrev-cli.js, and a package.json naming <version>,
#      at or above the floor the pane's mount options need (1.1.6: `toolbar`, `review`,
#      `annotate`); where node is present, the CLI must run and report the same version, since
#      bundle and CLI must come from one release (mdrev's guide, §11);
#   3. vendor/mdrev/release — replaced WHOLE with what a host uses: bundle/, mdrev-cli.js,
#      package.json, and docs/*.md (the guide and the contract, pinned with the code they describe);
#   4. vendor/mdrev/release.sha256 — every file's checksum, which a test holds the tree to, so a
#      hand edit fails `cargo test`;
#   5. the crate's version and README's provenance rows, rewritten by PATTERN (never against the old
#      value, which silently no-ops on a file that drifted) and each checked afterwards;
#   6. cargo check -p mdrev — refreshes Cargo.lock and proves the table builds.
#
# Then, by hand: read the contract's diff, run the gates and the FULL browser suite (the mdrev
# cases and `mdrev-cli conform` run against the new pin), and commit. Exit 0, or 2 stopped.
#
# set -e is deliberately NOT used (as in release.sh and corp-publish.sh): every command is checked
# with an explicit `|| stop`, and every value a later step depends on goes through need().
set -u

V=""; want=""; from=""
usage() { echo "usage: $0 <version> [--sha256 <hex>] [--from <tarball>]" >&2; exit 1; }
while [ $# -gt 0 ]; do
  case "$1" in
    --sha256) want="${2:-}"; shift 2 ;;
    --from) from="${2:-}"; shift 2 ;;
    -h|--help) sed -n '2,34p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    -*) usage ;;
    *) [ -z "$V" ] || usage; V="$1"; shift ;;
  esac
done
V=${V#v}
[ -n "$V" ] || usage
printf '%s' "$V" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$' || { echo "not a version: $V" >&2; exit 1; }

say()  { echo "### $*"; }
stop() { echo "### STOP: $*" >&2; exit 2; }
need() { [ -n "${2:-}" ] || stop "$1 is empty — refusing to continue"; }

# at_least <version> <floor>: semver precedence on the three numbers, and a pre-release OF the
# floor is still below it (1.1.6-dev11 predates the options 1.1.6 shipped).
at_least() {
  printf '%s %s\n' "$1" "$2" | awk '{
    pre = index($1, "-") > 0; split($1, parts, "-"); split(parts[1], v, "."); split($2, f, ".")
    for (i = 1; i <= 3; i++) { if (v[i] + 0 > f[i] + 0) exit 0; if (v[i] + 0 < f[i] + 0) exit 1 }
    exit pre ? 1 : 0 }'
}

ROOT=$(git rev-parse --show-toplevel 2>/dev/null) || stop "run this inside the claude-replay checkout"
DEST="$ROOT/vendor/mdrev"
[ -f "$DEST/Cargo.toml" ] && [ -f "$DEST/README.md" ] || stop "$DEST is not the mdrev crate (Cargo.toml, README.md)"
ASSET="mdrev-embed-$V.tar.gz"
URL="https://github.com/tanghong123/homebrew-tap/releases/download/mdrev-$V/$ASSET"
RELEASE_API="https://api.github.com/repos/tanghong123/homebrew-tap/releases/tags/mdrev-$V"
FLOOR=1.1.6
if command -v shasum >/dev/null 2>&1; then SHA="shasum -a 256"; else SHA="sha256sum"; fi
for t in curl tar awk sed find cargo; do command -v "$t" >/dev/null 2>&1 || stop "needs $t"; done

# 1. The tarball, and the checksum it must have: the digest the release publishes for this asset —
# the `"digest"` that follows the asset's `"name"` in the API's listing. The JSON is cut at its
# punctuation first, so the parse does not care how it is laid out; no asset's fields contain any.
if [ -z "$want" ]; then
  listing=$(curl -fsSL -H "Accept: application/vnd.github+json" "$RELEASE_API") \
    || stop "could not read the release mdrev-$V ($RELEASE_API) — pass --sha256"
  want=$(printf '%s\n' "$listing" | tr ',{}' '\n\n\n' | tr -d ' \t' | awk -v asset="\"name\":\"$ASSET\"" '
    $0 == asset { found = 1; next }
    found && /^"name":/ { exit }
    found && /^"digest":"sha256:[0-9a-f]+"$/ { print substr($0, 18, length($0) - 18); exit }')
  source_of_sha="the release's published digest"
else
  source_of_sha="--sha256"
fi
need "the expected sha256" "$want"
printf '%s' "$want" | grep -qE '^[0-9a-f]{64}$' || stop "not a sha256: $want"

work=$(mktemp -d "${TMPDIR:-/tmp}/vendor-mdrev.XXXXXX") || stop "mktemp failed"
trap 'rm -rf "$work"' EXIT
tarball="$work/$ASSET"
if [ -n "$from" ]; then
  cp "$from" "$tarball" || stop "cannot read $from"
else
  say "downloading $URL"
  curl -fsSL -o "$tarball" "$URL" || stop "download failed: $URL"
fi
got=$($SHA "$tarball" | awk '{print $1}')
need "the tarball's sha256" "$got"
[ "$got" = "$want" ] || stop "sha256 mismatch: the tarball is $got, $source_of_sha says $want"
say "sha256 $got — matches $source_of_sha"

# 2. The kit, read out of the tarball and checked before anything in the repository moves.
tree="$work/mdrev-embed-$V"
top="mdrev-embed-$V"
tar xzf "$tarball" -C "$work" "$top/bundle" "$top/mdrev-cli.js" "$top/package.json" "$top/docs" \
  || stop "the tarball does not hold $top/{bundle,mdrev-cli.js,package.json,docs}"
for f in bundle/mdrev.js bundle/mdrev.css mdrev-cli.js package.json docs/contract.md docs/embedding-guide.md; do
  [ -f "$tree/$f" ] || stop "the release has no $f"
done
[ -z "$(find "$tree" -type l)" ] || stop "the release holds symlinks — the build embeds files and directories only"
pv=$(sed -n 's/.*"version" *: *"\([^"]*\)".*/\1/p' "$tree/package.json" | head -1)
[ "$pv" = "$V" ] || stop "its package.json says ${pv:-no version}, not $V"
at_least "$V" "$FLOOR" || stop "mdrev $V is older than $FLOOR, whose guest first understood the options the pane declares (toolbar, review, annotate)"
if command -v node >/dev/null 2>&1; then
  cv=$(node "$tree/mdrev-cli.js" --version 2>&1) || stop "node could not run the release's mdrev-cli.js: $cv"
  [ "$cv" = "mdrev-cli $V" ] || stop "the CLI reports '$cv', not 'mdrev-cli $V' — bundle and CLI must come from one release"
  say "$cv runs under $(node --version)"
else
  say "no node here: the CLI's own version check is skipped (the browser suite runs it)"
fi

# 3. release/, replaced whole. Modes normalised: to this repository every file here is data.
rel="$DEST/release"
rm -rf "$rel" || stop "cannot remove $rel"
mkdir -p "$rel/docs" || stop "cannot create $rel"
cp -R "$tree/bundle" "$rel/bundle" || stop "copying bundle/"
cp "$tree/mdrev-cli.js" "$tree/package.json" "$rel/" || stop "copying the CLI"
for d in "$tree"/docs/*.md; do cp "$d" "$rel/docs/" || stop "copying $d"; done
find "$rel" -type f -exec chmod 644 {} + || stop "chmod"

# 4. The manifest a test holds the tree to.
( cd "$rel" && find . -type f | LC_ALL=C sort | sed 's|^\./||' | while IFS= read -r f; do $SHA "$f" || exit 1; done ) \
  > "$DEST/release.sha256" || stop "writing release.sha256"
n=$(wc -l < "$DEST/release.sha256" | tr -d ' ')
[ "$n" -gt 0 ] || stop "release.sha256 lists nothing"

# 5. The crate's version and the README's provenance rows — by pattern, each one checked.
sed -i.bak -E "s/^version = \"[^\"]*\"/version = \"$V\"/" "$DEST/Cargo.toml" && rm -f "$DEST/Cargo.toml.bak" || stop "rewriting Cargo.toml"
grep -q "^version = \"$V\"" "$DEST/Cargo.toml" || stop "the crate's version was not rewritten"
today=$(date +%Y-%m-%d)
sed -i.bak \
  -e "s#^| release | .*|\$#| release | mdrev $V |#" \
  -e "s#^| source | .*|\$#| source | <$URL> |#" \
  -e "s#^| sha256 | .*|\$#| sha256 | \`$got\` ($source_of_sha) |#" \
  -e "s#^| vendored | .*|\$#| vendored | $today |#" \
  "$DEST/README.md" && rm -f "$DEST/README.md.bak" || stop "rewriting README.md"
for row in "| release | mdrev $V |" "| source | <$URL> |" "| sha256 | \`$got\`" "| vendored | $today |"; do
  grep -qF "$row" "$DEST/README.md" || stop "README.md's provenance row was not rewritten: $row"
done

# 6. Cargo.lock and the build.
(cd "$ROOT" && cargo check -q -p mdrev) || stop "cargo check -p mdrev failed"

say "vendor/mdrev now pins mdrev $V: $n files, sha256 $got"
git -C "$ROOT" status --short -- vendor/mdrev Cargo.lock | awk '{print $1}' | sort | uniq -c | sed 's/^/    /'
cat <<EOF
Next, by hand:
  git diff vendor/mdrev/release/docs/contract.md     # what the host must now honour
  cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test && scripts/gate/gate.sh
  cargo build --release -p claude-monitor -p claude-monitor-v2 \\
    && cargo test -p claude-replay-browser-tests -- --ignored --skip known_red
  then commit it as its own change: "mdrev: pin $V"
EOF
