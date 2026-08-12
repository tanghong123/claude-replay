#!/bin/sh
# The installer installs monitor-fleet's own files, in the layout the README documents, when it is
# handed monitor-fleet's name. The refusals shared by every integration (managed symlinks,
# overlapping destinations, preserved local edits) live in `install-skill.sh` and are exercised by
# `tests/install_jdi_handoff.sh`; what is worth checking twice is that a second integration is
# actually a second, independent installation and not a rename of the first.
set -eu

repo_root=$(CDPATH= cd "$(dirname "$0")/.." && pwd -P)
installer="$repo_root/integrations/install-skill.sh"
skill_source="$repo_root/integrations/shared/skills/monitor-fleet/SKILL.md"
command_source="$repo_root/integrations/claude/commands/monitor-fleet.md"

fixture_root=$(mktemp -d "${TMPDIR:-/tmp}/monitor-fleet-install.XXXXXX")
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
    sh "$installer" monitor-fleet --agents-dir "$agents_dir" --claude-dir "$claude_dir"
}

canonical="$agents_dir/monitor-fleet/SKILL.md"
claude_skill="$claude_dir/skills/monitor-fleet/SKILL.md"
claude_command="$claude_dir/commands/monitor-fleet.md"

assert_install() {
    canonical_dir=$(CDPATH= cd "$agents_dir/monitor-fleet" && pwd -P)
    resolved="$canonical_dir/SKILL.md"

    test -f "$resolved" || fail "canonical Skill is missing"
    cmp -s "$skill_source" "$resolved" || fail "canonical Skill differs from its source"
    test -L "$claude_skill" || fail "Claude Skill is not a symbolic link"
    test "$(readlink "$claude_skill")" = "$resolved" || fail "Claude Skill points at the wrong target"
    cmp -s "$command_source" "$claude_command" || fail "Claude command differs from its source"
}

install_fixture >/dev/null
assert_install
install_fixture >/dev/null
assert_install

# The Skill must tell an agent to discover rather than to assume, since a shipped host name or port
# would be someone else's machine (the same rule `Fleet::default()` enforces in code).
grep -Fq "claude-monitor-fleet discover" "$canonical" || fail "Skill omits discovery"
grep -Fq "never assume a port" "$canonical" || fail "Skill omits the no-assumed-port rule"

# Two integrations, two installations: installing one must leave the other's files alone.
sh "$installer" jdi-handoff --agents-dir "$agents_dir" --claude-dir "$claude_dir" >/dev/null
assert_install
test -f "$agents_dir/jdi-handoff/SKILL.md" || fail "jdi-handoff Skill is missing"
install_fixture >/dev/null
test -f "$agents_dir/jdi-handoff/SKILL.md" || fail "monitor-fleet install removed jdi-handoff"
test -f "$claude_dir/commands/jdi-handoff.md" || fail "monitor-fleet install removed the jdi command"

# The integration is now an argument, so it is also an argument that can be missing or wrong.
sh "$installer" monitor-fleet --help | grep -q 'install-skill.sh monitor-fleet' ||
    fail "help names the wrong command"
sh "$installer" monitor-fleet --help | grep -q 'agents-dir PATH' || fail "help omits --agents-dir"
if sh "$installer" >/dev/null 2>&1; then
    fail "a missing Skill name must be rejected"
fi
if sh "$installer" monitor-fleet --unknown >/dev/null 2>&1; then
    fail "unknown arguments must be rejected"
fi
if sh "$installer" monitor-fleet --agents-dir >/dev/null 2>&1; then
    fail "missing option values must be rejected"
fi
if sh "$installer" ../monitor-fleet --agents-dir "$agents_dir" --claude-dir "$claude_dir" \
    >/dev/null 2>&1; then
    fail "a name that is a path must be rejected"
fi
sh "$installer" monitor-fleet --unknown 2>&1 | grep -q '^install-skill:' ||
    fail "errors must be reported under the installer's own name"

printf 'shared monitor-fleet installer: ok\n'
