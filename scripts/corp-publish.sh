#!/bin/sh
# scripts/corp-publish.sh <version> [--dry-run] [--verify-only] [--no-upgrade] [--no-wait]
#                         [--work <dir>]
#
# The corp tap publish, mechanically — the other half of scripts/release.sh (#267). The tag push
# bumps the PUBLIC tap (tanghong123/tap) on its own; this is the manual half, and it used to be an
# inline shell block rewritten from CLAUDE.md each release. On 2026-09-22 that block was found to
# have published nothing for five releases while printing success each time: `git -C "$TAP" add
# Formula/agent-*.rb` was run from the claude-replay root, where the glob matches nothing, so zsh
# aborted the line; the commit then said "no changes added to commit", the push said "Everything
# up-to-date", and the banner printed anyway. The tap served 1.287.0 while this machine ran
# 1.292.0 — brew installs from the tap clone's WORKING TREE, so the local upgrade kept working and
# hid it. Hence the two rules this script exists to hold:
#
#   * NOTHING GLOBS ACROSS DIRECTORIES. Paths are listed explicitly, and every staged set is
#     compared against the exact list expected — never a count, never a pattern. Another team's
#     formulae live in that tap (agent-metrics); we touch four files and assert we touched four.
#   * THE BANNER IS EARNED. verify_published() re-reads the formulae from the tap's origin/main and
#     the artifacts from the pushed commit, asserts they name this version and each other, and is
#     the ONLY thing that prints the success line. --verify-only runs it alone, against whatever is
#     published now, and is the ten-second answer to "is the corp tap actually current?".
#
# set -e is deliberately NOT used (as in release.sh): it is silent inside pipelines and
# conditionals, which is precisely how a no-op passed for a no-op five times. Every command is
# checked with an explicit `|| stop`, and every value a later step depends on goes through need().
#
# The steps, in order:
#   1. preflight — the tools, a clean tap clone not ahead of its remote, and a complete release;
#   2. the tarballs — all 16 downloaded from the GitHub release and each verified against the
#      .sha256 published beside it, before anything is republished anywhere;
#   3. alibrew/artifacts — a blob:none, cone-sparse clone of exactly the 16 NEW directories (a
#      plain clone pulls every binary ever published; cone mode materializes nothing deeper than
#      two levels), the LFS guard, the already-published guard, then commit, rebase, push, and the
#      sha read back FROM THE REMOTE;
#   4. the four formulae — version, revision and only_path rewritten by pattern (never against the
#      old version, which silently no-ops), ruby -c, brew style and brew audit --strict;
#   5. the tap commit and push — four explicit paths, the staged set asserted, rebased on a fresh
#      origin/main and pushed;
#   6. verify_published — the proof, then `alibrew upgrade` for this machine.
#
# The corporate identity is read at RUNTIME off the tap's own history for a formula it already
# owns (never the tap's last commit, which belongs to whoever published last) and set repo-locally
# in both corp clones. It is never printed and never written into this repo — .githooks/pre-push
# refuses it in the diff as well as in the metadata.
#
# --dry-run does every read-only step — the release check, the 16 downloads and their checksums, the
# artifacts clone, the LFS guard and the already-published guards — and stops before the first write:
# no commit, no push, and no edit of the real tap. --verify-only skips to step 6. Exit 0, or 2 stopped.
set -u

V=""; dry=0; verify_only=0; upgrade=1; wait_assets=1; work=""
usage() { echo "usage: $0 <version> [--dry-run] [--verify-only] [--no-upgrade] [--no-wait] [--work <dir>]" >&2; exit 1; }
while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) dry=1; shift ;;
    --verify-only) verify_only=1; shift ;;
    --no-upgrade) upgrade=0; shift ;;
    --no-wait) wait_assets=0; shift ;;
    --work) work="${2:-}"; shift 2 ;;
    -h|--help) sed -n '2,48p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    -*) usage ;;
    *) [ -z "$V" ] || usage; V="$1"; shift ;;
  esac
done
V=${V#v}
[ -n "$V" ] || usage
printf '%s' "$V" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+$' || { echo "not a version: $V" >&2; exit 1; }

say()  { echo "### $*"; }
stop() { echo "### STOP: $*" >&2; exit 2; }
need() { [ -n "${2:-}" ] || stop "$1 is empty — refusing to continue (a blank value once published blank revisions)"; }

GH_REPO=tanghong123/claude-replay
TOOLS="agent-replay agent-monitor agent-monitor-fleet agent-jdi"
PLATFORMS="darwin-arm64 darwin-amd64 linux-arm64 linux-amd64"
triple_for() {
  case "$1" in
    darwin-arm64) echo aarch64-apple-darwin ;;
    darwin-amd64) echo x86_64-apple-darwin ;;
    linux-arm64)  echo aarch64-unknown-linux-musl ;;
    linux-amd64)  echo x86_64-unknown-linux-musl ;;
    *)            echo "" ;;
  esac
}
# The four formula paths, and nothing else in that tap, ever.
formulae_rel() { for t in $TOOLS; do echo "Formula/$t.rb"; done; }

TAP=$(brew --repository alibrew/core 2>/dev/null)
need "the alibrew/core tap path" "$TAP"
[ -d "$TAP/.git" ] || stop "alibrew/core is not a git clone at $TAP — run: alibrew tap alibrew/core"

T=$(mktemp -d "${TMPDIR:-/tmp}/corp-publish.XXXXXX") || exit 1
if [ -n "$work" ]; then mkdir -p "$work" || stop "cannot create --work $work"; KEEP=1; else work="$T/work"; mkdir -p "$work"; KEEP=0; fi
trap 'rm -rf "$T"' EXIT

# ---------------------------------------------------------------- the proof
# Reads ONLY from the remotes: the tap's origin/main and the artifacts commit the formulae name.
# Sets VERIFIED_SHA. Never consults the local working tree for the answer, only for "is it clean".
VERIFIED_SHA=""
verify_published() {
  v=$1
  say "verify — reading the formulae back from the tap's origin/main"
  git -C "$TAP" fetch -q origin main || stop "verify: git fetch origin main failed in the tap"
  R=$(git -C "$TAP" rev-parse FETCH_HEAD 2>/dev/null)
  need "the tap's origin/main sha" "$R"

  sha=""
  for t in $TOOLS; do
    f=$(git -C "$TAP" show "$R:Formula/$t.rb" 2>/dev/null)
    [ -n "$f" ] || stop "verify: origin/main has no Formula/$t.rb"
    n=$(printf '%s\n' "$f" | grep -c "^  version \"$v\"\$")
    if [ "$n" != "1" ]; then
      serves=$(printf '%s\n' "$f" | grep -m1 '^  version "' | sed 's/.*"\(.*\)".*/\1/')
      stop "verify: the tap's origin/main serves $t ${serves:-(no version line)}, not $v — it is NOT serving $v"
    fi
    s=$(printf '%s\n' "$f" | grep -oE 'revision: *"[0-9a-f]{40}"' | grep -oE '[0-9a-f]{40}' | sort -u)
    [ "$(printf '%s\n' "$s" | wc -l | tr -d ' ')" = "1" ] || stop "verify: $t names more than one artifacts revision"
    need "$t's artifacts revision" "$s"
    if [ -z "$sha" ]; then sha=$s; elif [ "$sha" != "$s" ]; then stop "verify: the four formulae name different artifacts revisions"; fi
    n=$(printf '%s\n' "$f" | grep -c "only_path: \"$t/$v-")
    [ "$n" = "4" ] || stop "verify: $t has $n only_path lines for $v, expected 4"
    for p in $PLATFORMS; do
      printf '%s\n' "$f" | grep -q "only_path: \"$t/$v-$p\"" || stop "verify: $t is missing only_path $t/$v-$p"
    done
  done
  VERIFIED_SHA=$sha

  # Our commit must have LANDED on origin/main — not "HEAD equals origin/main", which another
  # team's push into this shared tap would falsify a second after ours landed.
  git -C "$TAP" merge-base --is-ancestor HEAD "$R" || stop "verify: the local tap clone holds a commit that is NOT on origin/main — the push did not land; inspect $TAP"
  [ -z "$(git -C "$TAP" status --porcelain)" ] || { git -C "$TAP" status --short; stop "verify: the tap working tree is dirty — uncommitted formula edits are exactly how five releases went unpublished"; }

  say "verify — reading the artifacts back from the commit the formulae name"
  A="$T/verify-artifacts"
  rm -rf "$A"
  git clone -q --filter=blob:none --no-checkout https://code.alibaba-inc.com/alibrew/artifacts.git "$A" 2>/dev/null || stop "verify: cannot clone alibrew/artifacts"
  git -C "$A" merge-base --is-ancestor "$sha" origin/master 2>/dev/null || stop "verify: $sha is not on alibrew/artifacts master — the formulae point at a commit the remote does not have"
  missing=0
  for t in $TOOLS; do
    for p in $PLATFORMS; do
      tri=$(triple_for "$p"); need "the target triple for $p" "$tri"
      found=$(git -C "$A" ls-tree --name-only -r "$sha" -- "$t/$v-$p/" 2>/dev/null)
      case "$found" in
        *"$t-$tri.tar.gz") : ;;
        *) echo "### missing: $t/$v-$p/$t-$tri.tar.gz" >&2; missing=$((missing+1)) ;;
      esac
    done
  done
  [ "$missing" = "0" ] || stop "verify: $missing of 16 artifact paths are not in $sha"

  echo
  say "PROOF"
  echo "    tap origin/main      $R  (our commit is an ancestor, tree clean)"
  echo "    formulae             agent-replay, agent-monitor, agent-monitor-fleet, agent-jdi all version \"$v\""
  echo "    artifacts            $sha  on master, all 16 of 4 tools x 4 targets present"
  echo
}

if [ $verify_only = 1 ]; then
  verify_published "$V"
  say "CORP TAP $V AT $VERIFIED_SHA"
  exit 0
fi

# ---------------------------------------------------------------- 1. preflight
say "preflight"
for c in git gh ruby shasum tar brew; do command -v "$c" >/dev/null 2>&1 || stop "preflight: $c is not on PATH"; done
[ -z "$(git -C "$TAP" status --porcelain)" ] || { git -C "$TAP" status --short; stop "preflight: the tap working tree is dirty before we start — a previous publish edited the formulae and never committed them. Inspect $TAP and commit or discard before publishing."; }
git -C "$TAP" fetch -q origin main || stop "preflight: git fetch origin main failed in the tap"
TAP_REMOTE=$(git -C "$TAP" rev-parse FETCH_HEAD); need "the tap's origin/main sha" "$TAP_REMOTE"
git -C "$TAP" merge-base --is-ancestor HEAD "$TAP_REMOTE" || stop "preflight: the local tap clone is AHEAD of origin/main — it holds an unpushed commit. Push or drop it first; that is the state five unpublished releases left behind."
[ "$(git -C "$TAP" rev-parse HEAD)" = "$TAP_REMOTE" ] && say "  tap clone is at origin/main" || {
  git -C "$TAP" merge --ff-only -q "$TAP_REMOTE" || stop "preflight: cannot fast-forward the tap clone to origin/main"
  say "  tap clone fast-forwarded to origin/main"
}
# The identity, read off a formula this tap already owns — never `log -1` on the whole tap, whose
# last commit belongs to whichever team published most recently.
EMAIL=$(git -C "$TAP" log -1 --format='%ae' -- Formula/agent-replay.rb)
NAME=$(git -C "$TAP" log -1 --format='%an' -- Formula/agent-replay.rb)
need "the corporate author address on the tap's own history" "$EMAIL"
need "the corporate author name on the tap's own history" "$NAME"
git -C "$TAP" config --local user.email "$EMAIL" || stop "preflight: cannot set user.email in the tap clone"
git -C "$TAP" config --local user.name  "$NAME"  || stop "preflight: cannot set user.name in the tap clone"
say "  identity pinned repo-locally from the tap's own history (not printed; never enters this repo)"

gh release view "v$V" -R "$GH_REPO" --json tagName >/dev/null 2>&1 || stop "preflight: there is no release v$V on $GH_REPO — cut it first with scripts/release.sh"
n=0; waited=0
while :; do
  n=$(gh release view "v$V" -R "$GH_REPO" --json assets -q '.assets|length' 2>/dev/null || echo 0)
  [ "$n" -ge 32 ] && break
  [ $wait_assets = 1 ] || stop "preflight: release v$V has $n of 32 assets and --no-wait was given"
  [ $waited -ge 1200 ] && stop "preflight: release v$V still has $n of 32 assets after 20 minutes — the Release workflow is stuck or failed"
  say "  waiting for the Release workflow: $n of 32 assets (${waited}s)"
  sleep 30; waited=$((waited+30))
done
say "  release v$V is complete ($n assets)"

# ---------------------------------------------------------------- 2. the tarballs
say "tarballs — download and verify against the published .sha256"
D="$work/rel$V"; mkdir -p "$D" || stop "cannot create $D"
have=0
for t in $TOOLS; do for p in $PLATFORMS; do
  tri=$(triple_for "$p"); need "the target triple for $p" "$tri"
  [ -f "$D/$t-$tri.tar.gz" ] && [ -f "$D/$t-$tri.sha256" ] && have=$((have+1))
done; done
if [ "$have" = "16" ]; then say "  reusing 16 tarballs already in $D"; else
  ( cd "$D" && gh release download "v$V" -R "$GH_REPO" -p '*.tar.gz' -p '*.sha256' --clobber >/dev/null 2>&1 ) || stop "tarballs: gh release download failed"
fi
ok=0
for t in $TOOLS; do for p in $PLATFORMS; do
  tri=$(triple_for "$p")
  [ -f "$D/$t-$tri.tar.gz" ] || stop "tarballs: the release has no $t-$tri.tar.gz"
  [ -f "$D/$t-$tri.sha256" ] || stop "tarballs: the release has no $t-$tri.sha256"
  ( cd "$D" && shasum -a 256 -c "$t-$tri.sha256" >/dev/null 2>&1 ) || stop "tarballs: CHECKSUM FAILED for $t-$tri.tar.gz — do not republish it"
  ok=$((ok+1))
done; done
[ "$ok" = "16" ] || stop "tarballs: verified $ok of 16"
say "  16 of 16 checksums verified"

# ---------------------------------------------------------------- 3. alibrew/artifacts
say "artifacts — cone-sparse clone of the 16 new directories"
W="$work/artifacts$V"; rm -rf "$W"
git clone -q --filter=blob:none --no-checkout https://code.alibaba-inc.com/alibrew/artifacts.git "$W" 2>"$T/clone.log" || { grep -v "post-quantum\|store now\|openssh.com/pq" "$T/clone.log" >&2; stop "artifacts: clone failed"; }
[ -d "$W/.git" ] || stop "artifacts: clone produced no repository"
DIRS=""
for t in $TOOLS; do for p in $PLATFORMS; do DIRS="$DIRS $t/$V-$p"; done; done
git -C "$W" sparse-checkout init --cone >/dev/null 2>&1 || stop "artifacts: sparse-checkout init failed"
# Exactly two levels: brew writes a single-line cone pattern and cone mode materializes nothing
# deeper than that.
git -C "$W" sparse-checkout set $DIRS >/dev/null 2>&1 || stop "artifacts: sparse-checkout set failed"
git -C "$W" checkout -q master 2>/dev/null || stop "artifacts: checkout master failed"
git -C "$W" config --local user.email "$EMAIL" || stop "artifacts: cannot set user.email"
git -C "$W" config --local user.name  "$NAME"  || stop "artifacts: cannot set user.name"

# The LFS guard: metadata only, never a blob read. An LFS-tracked artifact installs as a pointer.
if grep -q 'filter=lfs' "$W/.gitattributes" 2>/dev/null; then stop "artifacts: .gitattributes LFS-tracks something — that repo must never LFS-track files"; fi

say "guards — has this version already been published?"
already=0
for t in $TOOLS; do for p in $PLATFORMS; do
  [ -n "$(git -C "$W" ls-tree --name-only -r origin/master -- "$t/$V-$p/" 2>/dev/null)" ] && already=$((already+1))
done; done
[ "$already" = "0" ] || stop "guards: alibrew/artifacts already holds $already of 16 directories for $V — this version was published before. Use --verify-only to check it, or publish a new version."
tap_now=$(git -C "$TAP" show "$TAP_REMOTE:Formula/agent-replay.rb" 2>/dev/null | grep -m1 '^  version "' | sed 's/.*"\(.*\)".*/\1/')
need "the version the tap currently serves" "$tap_now"
[ "$tap_now" != "$V" ] || stop "guards: the tap's origin/main already serves $V — nothing to publish. Use --verify-only."
say "  artifacts hold nothing for $V; the tap serves $tap_now"

if [ $dry = 1 ]; then
  say "dry run — the read-only half passed: release complete, 16 checksums verified, clone and guards clean"
  say "dry run — NOT committing, NOT pushing, NOT touching $TAP/Formula"
  [ "$KEEP" = "1" ] && say "dry run — the downloads are in $D"
  exit 0
fi

say "artifacts — staging the 16 tarballs"
for t in $TOOLS; do for p in $PLATFORMS; do
  tri=$(triple_for "$p"); need "the target triple for $p" "$tri"
  mkdir -p "$W/$t/$V-$p" || stop "artifacts: cannot create $t/$V-$p"
  /bin/cp -f "$D/$t-$tri.tar.gz" "$W/$t/$V-$p/$t-$tri.tar.gz" || stop "artifacts: cannot copy $t-$tri.tar.gz"
  git -C "$W" add -- "$t/$V-$p/$t-$tri.tar.gz" || stop "artifacts: git add failed for $t/$V-$p/$t-$tri.tar.gz"
done; done
# The staged SET, not its size: a count of 16 would pass on sixteen wrong paths.
expected="$T/art-expected"; : > "$expected"
for t in $TOOLS; do for p in $PLATFORMS; do
  tri=$(triple_for "$p"); echo "$t/$V-$p/$t-$tri.tar.gz" >> "$expected"
done; done
sort -o "$expected" "$expected"
git -C "$W" diff --cached --name-only > "$T/art-staged" || stop "artifacts: cannot read the staged set"
sort -o "$T/art-staged" "$T/art-staged"
cmp -s "$expected" "$T/art-staged" || { echo "--- expected ---"; cat "$expected"; echo "--- staged ---"; cat "$T/art-staged"; stop "artifacts: the staged set is not the 16 expected paths"; }
say "  16 of 16 paths staged, exactly as expected"

git -C "$W" commit -q -m "agent-replay / agent-monitor / agent-monitor-fleet / agent-jdi $V" || stop "artifacts: commit failed"
[ "$(git -C "$W" log -1 --format='%ae')" = "$EMAIL" ] || stop "artifacts: the commit is not authored from the tap's own corporate address — the corp remote would reject it"
# The repo is SHARED: another tool may have published between the clone and the push.
git -C "$W" fetch -q origin master || stop "artifacts: fetch before push failed"
git -C "$W" rebase -q FETCH_HEAD >/dev/null 2>&1 || { git -C "$W" rebase --abort >/dev/null 2>&1; stop "artifacts: cannot rebase onto origin/master — another publish conflicts with ours"; }
git -C "$W" push -q origin HEAD:master 2>"$T/push-artifacts.log" || { grep -v "post-quantum\|store now\|openssh.com/pq" "$T/push-artifacts.log" >&2; stop "artifacts: push to master failed — nothing has been written to the tap"; }
# The sha comes from the REMOTE, after the push. A local rev-parse names a commit nobody else has.
git -C "$W" fetch -q origin master || stop "artifacts: fetch after push failed"
ART_SHA=$(git -C "$W" rev-parse FETCH_HEAD)
need "the pushed artifacts sha" "$ART_SHA"
[ "$(git -C "$W" rev-parse HEAD)" = "$ART_SHA" ] || stop "artifacts: origin/master is not the commit we pushed — someone pushed on top; re-run"
say "  pushed alibrew/artifacts master at $ART_SHA"

# ---------------------------------------------------------------- 4. the four formulae
say "formulae — rewriting version, revision and only_path by pattern"
for t in $TOOLS; do
  p="$TAP/Formula/$t.rb"
  [ -f "$p" ] || stop "formulae: $p does not exist"
  # By PATTERN, never against the old version: a sed keyed on $OLD silently no-ops on a formula
  # that has drifted, and a no-op here is what ships a stale tap.
  /usr/bin/sed -i '' \
    -e "s/^  version \"[0-9][0-9.]*\"/  version \"$V\"/" \
    -e "s/revision: *\"[0-9a-f]\{40\}\"/revision:  \"$ART_SHA\"/g" \
    -e "s|only_path: \"$t/[0-9][0-9.]*-|only_path: \"$t/$V-|g" \
    "$p" || stop "formulae: sed failed on $t"
  ruby -c "$p" >/dev/null 2>&1 || stop "formulae: $t is not valid ruby after the rewrite"
  [ "$(grep -c "^  version \"$V\"\$" "$p")" = "1" ]      || stop "formulae: $t does not say version \"$V\" exactly once"
  [ "$(grep -c "$ART_SHA" "$p")" = "4" ]                 || stop "formulae: $t names the artifacts revision $(grep -c "$ART_SHA" "$p") times, expected 4"
  [ "$(grep -c "only_path: \"$t/$V-" "$p")" = "4" ]      || stop "formulae: $t has $(grep -c "only_path: \"$t/$V-" "$p") only_path lines for $V, expected 4"
  [ "$(grep -c 'only_path: "' "$p")" = "4" ]             || stop "formulae: $t has an only_path line left on another version"
  for pl in $PLATFORMS; do grep -q "only_path: \"$t/$V-$pl\"" "$p" || stop "formulae: $t is missing only_path $t/$V-$pl"; done
done
say "  four formulae rewritten and asserted"
say "style and audit"
( cd "$TAP" && brew style --except-cops=FormulaAudit/Urls Formula/agent-replay.rb Formula/agent-monitor.rb Formula/agent-monitor-fleet.rb Formula/agent-jdi.rb >"$T/style.log" 2>&1 ) || { tail -20 "$T/style.log"; stop "style: brew style refused the formulae"; }
say "  brew style clean"
for t in $TOOLS; do
  brew audit --strict "alibrew/core/$t" >"$T/audit-$t.log" 2>&1 || { tail -20 "$T/audit-$t.log"; stop "audit: brew audit --strict refused $t"; }
done
say "  brew audit --strict clean on all four"

# ---------------------------------------------------------------- 5. the tap commit and push
say "tap — committing the four formulae"
# cd IN, and name the paths. The glob that started all this was expanded by the caller's shell in
# the caller's directory, where Formula/ does not exist, and zsh killed the line before git ran.
cd "$TAP" || stop "tap: cannot cd into $TAP"
git add -- Formula/agent-replay.rb Formula/agent-monitor.rb Formula/agent-monitor-fleet.rb Formula/agent-jdi.rb || stop "tap: git add failed"
formulae_rel | sort > "$T/tap-expected"
git diff --cached --name-only > "$T/tap-staged" || stop "tap: cannot read the staged set"
sort -o "$T/tap-staged" "$T/tap-staged"
cmp -s "$T/tap-expected" "$T/tap-staged" || { echo "--- expected ---"; cat "$T/tap-expected"; echo "--- staged ---"; cat "$T/tap-staged"; stop "tap: the staged set is not exactly our four formulae (another team's formulae live in this tap)"; }
say "  four of four formulae staged, exactly as expected"
{
  echo "agent-replay / agent-monitor / agent-monitor-fleet / agent-jdi $V"
  echo
  echo "Artifacts at alibrew/artifacts $ART_SHA, each tarball verified against the sha256"
  echo "published beside it upstream."
} > "$T/tapmsg"
git commit -q -F "$T/tapmsg" || stop "tap: commit failed"
[ "$(git log -1 --format='%ae')" = "$EMAIL" ] || stop "tap: the commit is not authored from this tap's own corporate address — the corp remote would reject it"
git fetch -q origin main || stop "tap: fetch before push failed"
git rebase -q FETCH_HEAD >/dev/null 2>&1 || { git rebase --abort >/dev/null 2>&1; stop "tap: cannot rebase onto origin/main — someone else has touched our four formulae"; }
git push -q origin HEAD:main 2>"$T/push-tap.log" || { grep -v "post-quantum\|store now\|openssh.com/pq" "$T/push-tap.log" >&2; stop "tap: push to main failed — the formula commit is local only, which is exactly the state this script exists to prevent"; }
say "  pushed alibrew/homebrew-core main at $(git rev-parse --short HEAD)"

# ---------------------------------------------------------------- 6. the proof, then this machine
echo
verify_published "$V"

if [ $upgrade = 1 ]; then
  say "alibrew upgrade (this machine)"
  alibrew upgrade $TOOLS >"$T/upgrade.log" 2>&1
  grep -E '\->' "$T/upgrade.log" || tail -3 "$T/upgrade.log"
fi

say "CORP TAP $V AT $VERIFIED_SHA"
say "the paired monitor still runs the old binary until it is restarted"
exit 0
