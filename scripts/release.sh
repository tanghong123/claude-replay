#!/bin/sh
# scripts/release.sh <version> [--subject "<what shipped>"] [--message-file <body>] [--dry-run]
#                    [--allow-backwards] [--skip-gates]
#
# The release, mechanically (#200), as CLAUDE.md's "Releasing" prescribes and in one place, so
# that no working tree releases by hand-rolled steps again:
#   1. a clean tree on main that origin/main is an ancestor of (another tree may have released);
#   2. the version guard — scripts/release-check.sh --next <version> --fetch — which refuses a
#      version at or below the highest tag, or already tagged, unless --allow-backwards;
#   3. the bump of the [workspace.package] version and nothing else, then `cargo build` for
#      Cargo.lock; the diff must be exactly Cargo.toml and Cargo.lock;
#   4. the gates — cargo fmt --all --check, cargo clippy --all-targets -D warnings, cargo test,
#      scripts/gate/gate.sh printing BYTE-IDENTICAL: PASS — unless --skip-gates (printed: the
#      caller vouches that they ran on this very tree);
#   5. the release commit (`release: v<version> — <subject>`, the body from --message-file — the
#      caller's trailers included) and the signed annotated tag, both signed by the repo's own
#      config (never pass -c commit.gpgsign=false); the commit is verified before the tag;
#   6. the push: origin main, then the tag (the tag push triggers the Release workflow), then
#      the mirror (a failure there is printed and retried by hand — the host goes down).
# --dry-run stops after the gates and reverts the bump. scripts/corp-publish.sh (the corp tap) and
# scripts/sweep.sh remain the caller's (CLAUDE.md, Releasing). Exit 0 released, 2 stopped.
set -u
version=""; subject=""; msgfile=""; dry=0; allow=""; skip=0
usage() { echo "usage: $0 <version> [--subject <text>] [--message-file <path>] [--dry-run] [--allow-backwards] [--skip-gates]" >&2; exit 1; }
while [ $# -gt 0 ]; do
  case "$1" in
    --subject) subject="${2:-}"; shift 2 ;;
    --message-file) msgfile="${2:-}"; shift 2 ;;
    --dry-run) dry=1; shift ;;
    --allow-backwards) allow="--allow-backwards"; shift ;;
    --skip-gates) skip=1; shift ;;
    -h|--help) sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    -*) usage ;;
    *) [ -z "$version" ] || usage; version="$1"; shift ;;
  esac
done
[ -n "$version" ] || usage
[ -z "$msgfile" ] || [ -f "$msgfile" ] || { echo "no such message file: $msgfile" >&2; exit 1; }
say() { echo "### $*"; }
stop() { echo "### STOP: $*" >&2; exit 2; }
root=$(git rev-parse --show-toplevel 2>/dev/null) || stop "not a git repository"
cd "$root" || exit 1
workspace_version() {
  awk '/^\[workspace\.package\]/{s=1;next} /^\[/{s=0} s && /^version *= *"/{sub(/^version *= *"/,""); sub(/".*$/,""); print; exit}' Cargo.toml
}
T=$(mktemp -d "${TMPDIR:-/tmp}/release.XXXXXX") || exit 1
trap 'rm -rf "$T"' EXIT

# 1. The tree.
[ -z "$(git status --short)" ] || { git status --short; stop "the tree is not clean"; }
[ "$(git rev-parse --abbrev-ref HEAD)" = "main" ] || stop "not on main"
git fetch -q origin || stop "git fetch origin failed"
git merge-base --is-ancestor origin/main HEAD || stop "origin/main is not an ancestor of HEAD — rebase first; another tree may have released"

# 2. The guard.
say "version guard"
sh scripts/release-check.sh --next "$version" --fetch $allow || stop "the version guard refused $version"

# 3. The bump.
current=$(workspace_version)
say "bump $current -> $version"
awk -v v="$version" '
  /^\[workspace\.package\]/ { s = 1 }
  /^\[/ && !/^\[workspace\.package\]/ { s = 0 }
  s && /^version *= *"/ && !done { sub(/"[^"]*"/, "\"" v "\""); done = 1 }
  { print }
' Cargo.toml > "$T/Cargo.toml" && cp "$T/Cargo.toml" Cargo.toml
revert() { git checkout -- Cargo.toml Cargo.lock 2>/dev/null; }
[ "$(workspace_version)" = "$version" ] || { revert; stop "the bump did not land in [workspace.package]"; }
say "build (Cargo.lock)"
cargo build > "$T/build.log" 2>&1 || { tail -20 "$T/build.log"; revert; stop "cargo build failed"; }
tail -1 "$T/build.log"
changed=$(git diff --name-only | sort | tr '\n' ' ')
[ "$changed" = "Cargo.lock Cargo.toml " ] || [ "$changed" = "Cargo.toml " ] || { git status --short; revert; stop "the bump changed more than Cargo.toml and Cargo.lock"; }

# 4. The gates.
gate() { name=$1; shift; say "$name"; if "$@" > "$T/$name.log" 2>&1; then tail -1 "$T/$name.log"; else tail -30 "$T/$name.log"; revert; stop "$name failed"; fi; }
if [ $skip = 1 ]; then
  say "gates SKIPPED (--skip-gates): the caller vouches that fmt, clippy, cargo test and the byte gate ran clean on this tree"
else
  gate fmt cargo fmt --all --check
  gate clippy cargo clippy --all-targets -- -D warnings
  gate test cargo test
  gate byte-gate sh -c 'scripts/gate/gate.sh 2>&1 | tee "$0" >/dev/null; grep -q "BYTE-IDENTICAL: PASS" "$0"' "$T/gate.out"
  tail -3 "$T/gate.out"
fi
if [ $dry = 1 ]; then say "dry run — the gates passed; reverting the bump"; revert; exit 0; fi

# 5. The commit and the tag.
subj="release: v$version — ${subject:-release}"
{ echo "$subj"; if [ -n "$msgfile" ]; then echo; cat "$msgfile"; fi; } > "$T/msg"
git add Cargo.toml Cargo.lock
git commit -q -F "$T/msg" || stop "the release commit failed"
[ "$(git log -1 --format=%s)" = "$subj" ] || stop "HEAD is not the release commit — not tagging"
git tag -a "v$version" -m "v$version — ${subject:-release}" || stop "the tag failed (the release commit is on main, untagged)"
if git tag -v "v$version" >/dev/null 2>&1; then say "tag signature verifies"; else say "tag signature not verified here (allowed_signers?) — the tag is signed by config"; fi

# 6. The push.
say "push origin"
git push origin main || stop "push origin main failed — the release commit and tag are local"
git push origin "v$version" || stop "push origin v$version failed — main is pushed; push the tag: git push origin v$version"
say "push mirror"
run_timeout() { if command -v timeout >/dev/null 2>&1; then timeout 180 "$@"; else "$@"; fi; }
if run_timeout git push alibaba main && run_timeout git push alibaba "v$version"; then :; else
  say "MIRROR PUSH FAILED — retry when the host is up: git push alibaba main && git push alibaba v$version"
fi
say "released v$version at $(git rev-parse --short HEAD) — next: sh scripts/corp-publish.sh $version, then scripts/sweep.sh"
exit 0
