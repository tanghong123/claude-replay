#!/bin/sh
# Installs a shared Skill and its Claude Code slash command:
#
#   ./integrations/install-skill.sh monitor-fleet
#
# One implementation for every integration, because what is delicate here is not the copying but
# the refusals: a managed directory that is a symlink, a managed file that is a symlink to
# somewhere else, two destinations that collapse to one path, a local edit that must be preserved
# exactly once. Those are properties of installing a Skill, not of any particular Skill, and a
# second copy of them would drift from the first the day one of them is fixed.
# `tests/install_jdi_handoff.sh` exercises them.
#
# Adding an integration therefore adds no script: it is this one plus a name. `install-jdi-handoff.sh`
# stays only because that path is already published — `agent-jdi install-skill` and the README name
# it — and not because an entry point that just re-invokes this file earns its place.
set -eu

self=install-skill
name=
agents_dir=
claude_dir=

usage() {
    subject=${name:-<name>}
    cat <<EOF
Install the shared $subject Skill for Codex and Claude Code.

Usage:
  $self.sh $subject [--agents-dir PATH] [--claude-dir PATH]

Options:
  --agents-dir PATH  Agent Skills root (default: ~/.agents/skills)
  --claude-dir PATH  Claude configuration root (default: ~/.claude)
  -h, --help         Show this help
EOF
}

die() {
    printf '%s: %s\n' "$self" "$*" >&2
    exit 2
}

case "${1:-}" in
    -h|--help)
        usage
        exit 0
        ;;
    '') die "a Skill name is required" ;;
    -*) die "a Skill name must come first: $1" ;;
esac
name=$1
shift
# The name selects both the sources to read and the directories to write, so it stays a name.
case "$name" in
    */*|.*) die "not a Skill name: $name" ;;
esac

absolute_path() {
    case "$1" in
        /*) printf '%s\n' "$1" ;;
        *) printf '%s/%s\n' "$(pwd -P)" "$1" ;;
    esac
}

ensure_managed_dir() {
    path=$1
    label=$2
    [ ! -L "$path" ] || die "$label must not be a symbolic link: $path"
    if [ -e "$path" ] && [ ! -d "$path" ]; then
        die "$label is not a directory: $path"
    fi
    mkdir -p "$path"
}

install_managed_file() {
    source_file=$1
    target_file=$2
    target_dir=${target_file%/*}

    if [ -d "$target_file" ] && [ ! -L "$target_file" ]; then
        die "managed file target is a directory: $target_file"
    fi
    temporary=$(mktemp "$target_dir/.$name-install.XXXXXX")
    if ! cp "$source_file" "$temporary"; then
        rm -f "$temporary"
        die "cannot copy managed file: $target_file"
    fi
    chmod 644 "$temporary"
    if [ -L "$target_file" ] && ! rm "$target_file"; then
        rm -f "$temporary"
        die "cannot replace managed symlink: $target_file"
    fi
    if ! mv -f "$temporary" "$target_file"; then
        rm -f "$temporary"
        die "cannot replace managed file: $target_file"
    fi
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --agents-dir)
            [ "$#" -ge 2 ] || die "--agents-dir requires a value"
            [ -n "$2" ] || die "--agents-dir cannot be empty"
            agents_dir=$2
            shift 2
            ;;
        --claude-dir)
            [ "$#" -ge 2 ] || die "--claude-dir requires a value"
            [ -n "$2" ] || die "--claude-dir cannot be empty"
            claude_dir=$2
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            die "unknown argument: $1"
            ;;
    esac
done

if [ -z "$agents_dir" ] || [ -z "$claude_dir" ]; then
    user_home=${HOME:-}
    [ -n "$user_home" ] || die "HOME is not set; pass both destination options"
    [ -n "$agents_dir" ] || agents_dir="$user_home/.agents/skills"
    [ -n "$claude_dir" ] || claude_dir="$user_home/.claude"
fi

# This script lives beside the sources it installs, whichever entry point invoked it.
script_dir=$(CDPATH= cd "$(dirname "$0")" && pwd -P)
skill_source="$script_dir/shared/skills/$name/SKILL.md"
command_source="$script_dir/claude/commands/$name.md"
[ -f "$skill_source" ] || die "shared Skill source is missing: $skill_source"
[ -f "$command_source" ] || die "Claude command source is missing: $command_source"

agents_dir=$(absolute_path "$agents_dir")
claude_dir=$(absolute_path "$claude_dir")
mkdir -p "$agents_dir" "$claude_dir"
agents_dir=$(CDPATH= cd "$agents_dir" && pwd -P)
claude_dir=$(CDPATH= cd "$claude_dir" && pwd -P)

canonical_dir="$agents_dir/$name"
claude_skills_dir="$claude_dir/skills"
claude_skill_dir="$claude_dir/skills/$name"
claude_command_dir="$claude_dir/commands"
canonical_skill="$canonical_dir/SKILL.md"
claude_skill="$claude_skill_dir/SKILL.md"
claude_command="$claude_command_dir/$name.md"

if [ "$canonical_skill" = "$claude_skill" ]; then
    die "canonical and Claude Skill targets overlap: $canonical_skill"
fi

ensure_managed_dir "$canonical_dir" "canonical Skill directory"
ensure_managed_dir "$claude_skills_dir" "Claude skills directory"
ensure_managed_dir "$claude_skill_dir" "Claude $name Skill directory"
ensure_managed_dir "$claude_command_dir" "Claude commands directory"

install_managed_file "$skill_source" "$canonical_skill"

if [ -L "$claude_skill" ]; then
    rm "$claude_skill"
elif [ -e "$claude_skill" ]; then
    backup="$claude_skill.pre-shared-backup"
    if [ -e "$backup" ] || [ -L "$backup" ]; then
        die "cannot preserve existing Claude Skill; backup already exists: $backup"
    fi
    mv "$claude_skill" "$backup"
    printf 'Preserved previous Claude Skill: %s\n' "$backup"
fi
ln -s "$canonical_skill" "$claude_skill"

install_managed_file "$command_source" "$claude_command"

printf 'Installed shared Skill: %s\n' "$canonical_skill"
printf 'Linked Claude Skill:   %s -> %s\n' "$claude_skill" "$canonical_skill"
printf 'Installed Claude command: %s\n' "$claude_command"
printf 'Open a new session, then use $%s in Codex or /%s in Claude Code.\n' "$name" "$name"
