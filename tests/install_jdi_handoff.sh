#!/bin/sh
set -eu

repo_root=$(CDPATH= cd "$(dirname "$0")/.." && pwd -P)
installer="$repo_root/integrations/install-jdi-handoff.sh"
shared_source="$repo_root/integrations/shared/skills/jdi-handoff/SKILL.md"
command_source="$repo_root/integrations/claude/commands/jdi-handoff.md"

fixture_root=$(mktemp -d "${TMPDIR:-/tmp}/jdi-handoff-install.XXXXXX")
trap 'rm -rf "$fixture_root"' EXIT HUP INT TERM
fixture="$fixture_root/path with spaces"
agents_dir="$fixture/.agents/skills"
claude_dir="$fixture/.claude"
mkdir -p "$fixture"

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}

test -x "$installer" || fail "installer is not executable"

install_fixture() {
    sh "$installer" \
        --agents-dir "$agents_dir" \
        --claude-dir "$claude_dir"
}

assert_install() {
    canonical_dir=$(CDPATH= cd "$agents_dir/jdi-handoff" && pwd -P)
    canonical="$canonical_dir/SKILL.md"
    claude_skill="$claude_dir/skills/jdi-handoff/SKILL.md"
    claude_command="$claude_dir/commands/jdi-handoff.md"

    test -f "$canonical" || fail "canonical Skill is missing"
    cmp -s "$shared_source" "$canonical" || fail "canonical Skill differs from its source"
    test -L "$claude_skill" || fail "Claude Skill is not a symbolic link"
    test "$(readlink "$claude_skill")" = "$canonical" || fail "Claude Skill points at the wrong target"
    cmp -s "$command_source" "$claude_command" || fail "Claude command differs from its source"
}

# Fresh install and repeated install both produce the same topology.
install_fixture
assert_install
install_fixture
assert_install

# Migrate the layout documented before the shared installer existed. Preserve
# the regular file once, then replace it with the canonical link.
claude_skill="$claude_dir/skills/jdi-handoff/SKILL.md"
rm "$claude_skill"
cp "$shared_source" "$claude_skill"
install_fixture
assert_install
backup="$claude_skill.pre-shared-backup"
test -f "$backup" || fail "previous Claude Skill was not backed up"
cmp -s "$shared_source" "$backup" || fail "Claude Skill backup content changed"
install_fixture
assert_install
cmp -s "$shared_source" "$backup" || fail "reinstall changed the preserved backup"

# Replacing a managed command symlink must replace the link itself, never write
# through it to an arbitrary file.
claude_command="$claude_dir/commands/jdi-handoff.md"
victim="$fixture/victim.txt"
printf 'do not overwrite\n' >"$victim"
rm "$claude_command"
ln -s "$victim" "$claude_command"
install_fixture
test "$(cat "$victim")" = "do not overwrite" || fail "installer followed a command symlink"
test ! -L "$claude_command" || fail "managed Claude command remained a symlink"
cmp -s "$command_source" "$claude_command" || fail "Claude command was not safely refreshed"

# Installer-owned directory components must not be symlinks. Reject them before
# a Skill or command can be written outside the selected roots.
assert_rejects_managed_dir_link() {
    label=$1
    linked_path=$2
    outside=$3
    attack_agents=$4
    attack_claude=$5

    mkdir -p "$(dirname "$linked_path")" "$outside"
    ln -s "$outside" "$linked_path"
    if sh "$installer" --agents-dir "$attack_agents" --claude-dir "$attack_claude" >/dev/null 2>&1; then
        fail "$label managed-directory symlink was accepted"
    fi
    test -z "$(find "$outside" -mindepth 1 -print -quit)" || fail "$label link was followed outside its root"
}

attack="$fixture_root/managed-link-attacks"
assert_rejects_managed_dir_link \
    "canonical Skill" \
    "$attack/one/.agents/skills/jdi-handoff" \
    "$attack/one/outside" \
    "$attack/one/.agents/skills" \
    "$attack/one/.claude"
assert_rejects_managed_dir_link \
    "Claude skills root" \
    "$attack/two/.claude/skills" \
    "$attack/two/outside" \
    "$attack/two/.agents/skills" \
    "$attack/two/.claude"
assert_rejects_managed_dir_link \
    "Claude command root" \
    "$attack/three/.claude/commands" \
    "$attack/three/outside" \
    "$attack/three/.agents/skills" \
    "$attack/three/.claude"
assert_rejects_managed_dir_link \
    "Claude jdi-handoff Skill" \
    "$attack/four/.claude/skills/jdi-handoff" \
    "$attack/four/outside" \
    "$attack/four/.agents/skills" \
    "$attack/four/.claude"

# Public destination overrides may share parents, but their managed files must
# never collapse to the same path and become a self-referential link.
overlap="$fixture_root/overlapping-targets"
mkdir -p "$overlap"
if sh "$installer" \
    --agents-dir "$overlap/skills" \
    --claude-dir "$overlap" >/dev/null 2>&1; then
    fail "overlapping canonical and Claude Skill targets were accepted"
fi
test ! -e "$overlap/skills/jdi-handoff/SKILL.md" || fail "overlap rejection happened after writing a Skill"

# Relative destinations beginning with '-' are valid paths, not tool options.
leading="$fixture_root/leading-dash"
mkdir -p "$leading"
(
    cd "$leading"
    sh "$installer" --agents-dir -agents --claude-dir -claude
)
test -f "$leading/-agents/jdi-handoff/SKILL.md" || fail "leading-dash agents path was not installed"
test -L "$leading/-claude/skills/jdi-handoff/SKILL.md" || fail "leading-dash Claude path was not linked"

sh "$installer" --help | grep -q 'agents-dir PATH' || fail "help omits --agents-dir"
if sh "$installer" --unknown >/dev/null 2>&1; then
    fail "unknown arguments must be rejected"
fi
if sh "$installer" --agents-dir >/dev/null 2>&1; then
    fail "missing option values must be rejected"
fi

printf 'shared jdi-handoff installer: ok\n'
