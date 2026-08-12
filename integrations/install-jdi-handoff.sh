#!/bin/sh
# Installs the jdi-handoff Skill and Claude command by running `install-skill.sh`, which is the
# installer for every integration here. This path is kept working because it is already published —
# `agent-jdi install-skill` and the README name it — and not because an integration needs a script
# of its own; a new one is `install-skill.sh <name>`.
set -eu
script_dir=$(CDPATH= cd "$(dirname "$0")" && pwd -P)
exec sh "$script_dir/install-skill.sh" jdi-handoff "$@"
