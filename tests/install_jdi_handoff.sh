#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
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
    canonical_dir=$(CDPATH= cd -- "$agents_dir/jdi-handoff" && pwd -P)
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

sh "$installer" --help | grep -q -- '--agents-dir PATH' || fail "help omits --agents-dir"
if sh "$installer" --unknown >/dev/null 2>&1; then
    fail "unknown arguments must be rejected"
fi
if sh "$installer" --agents-dir >/dev/null 2>&1; then
    fail "missing option values must be rejected"
fi

printf 'shared jdi-handoff installer: ok\n'
