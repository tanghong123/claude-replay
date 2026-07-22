#!/bin/sh
set -eu

usage() {
    cat <<'EOF'
Install the shared jdi-handoff Skill for Codex and Claude Code.

Usage:
  install-jdi-handoff.sh [--agents-dir PATH] [--claude-dir PATH]

Options:
  --agents-dir PATH  Agent Skills root (default: ~/.agents/skills)
  --claude-dir PATH  Claude configuration root (default: ~/.claude)
  -h, --help         Show this help
EOF
}

die() {
    printf 'install-jdi-handoff: %s\n' "$*" >&2
    exit 2
}

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
    temporary=$(mktemp "$target_dir/.jdi-handoff-install.XXXXXX")
    if ! cp "$source_file" "$temporary"; then
        rm -f "$temporary"
        die "cannot copy managed file: $target_file"
    fi
    chmod 644 "$temporary"
    if ! mv -f "$temporary" "$target_file"; then
        rm -f "$temporary"
        die "cannot replace managed file: $target_file"
    fi
}

agents_dir=
claude_dir=
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

script_dir=$(CDPATH= cd "$(dirname "$0")" && pwd -P)
skill_source="$script_dir/shared/skills/jdi-handoff/SKILL.md"
command_source="$script_dir/claude/commands/jdi-handoff.md"
[ -f "$skill_source" ] || die "shared Skill source is missing: $skill_source"
[ -f "$command_source" ] || die "Claude command source is missing: $command_source"

agents_dir=$(absolute_path "$agents_dir")
claude_dir=$(absolute_path "$claude_dir")
mkdir -p "$agents_dir" "$claude_dir"
agents_dir=$(CDPATH= cd "$agents_dir" && pwd -P)
claude_dir=$(CDPATH= cd "$claude_dir" && pwd -P)

canonical_dir="$agents_dir/jdi-handoff"
claude_skills_dir="$claude_dir/skills"
claude_skill_dir="$claude_dir/skills/jdi-handoff"
claude_command_dir="$claude_dir/commands"
canonical_skill="$canonical_dir/SKILL.md"
claude_skill="$claude_skill_dir/SKILL.md"
claude_command="$claude_command_dir/jdi-handoff.md"

if [ "$canonical_skill" = "$claude_skill" ]; then
    die "canonical and Claude Skill targets overlap: $canonical_skill"
fi

ensure_managed_dir "$canonical_dir" "canonical Skill directory"
ensure_managed_dir "$claude_skills_dir" "Claude skills directory"
ensure_managed_dir "$claude_skill_dir" "Claude jdi-handoff Skill directory"
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
printf 'Open a new session, then use $jdi-handoff in Codex or /jdi-handoff in Claude Code.\n'
