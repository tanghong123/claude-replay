#!/usr/bin/env bash
# The version canary (#364): when a NEW Claude Code or Codex version is installed, run it ONCE in a
# throwaway home with one fixed prompt that makes a tool call, sweep that transcript with
# `agent-replay --unknown` and `--field-coverage`, and print what the sweep found — the day a
# version lands. The format moved under us before and we found out a week later (#263).
#
# The owner's decisions (2026-10-02): run daily from the unknown-review job, and only when an
# installed version is new; reuse the owner's own logins, never touching ~/.claude or ~/.codex.
#   - Claude Code: a long-lived token from the owner's own login (`claude setup-token`), as
#     CLAUDE_CODE_OAUTH_TOKEN in the machine-local 0600 env file. Copying the Keychain login instead
#     would raise a Keychain prompt in an unattended job, and a refresh could rotate the real login
#     out. Without the token the Claude canary is skipped, and says so.
#   - Codex: a copy of ~/.codex/auth.json and config.toml, made only while the login is fresh
#     enough that Codex will not refresh it (under 7 days); if it refreshes anyway, the fresh login
#     is written back — only if ~/.codex/auth.json did not change meanwhile.
# The throwaway home is deleted after every run. A copy of each canary transcript (a fixed prompt,
# nothing private) is kept for the reviewer in the state dir.
#
# Output: one JSON row per finding on stdout — the scan's own shape (agent, where, name, count,
# version, example) plus `canary: true` and `canary_transcript` — so the unknown-review job triages
# them with everything else. Log lines go to stderr. `--force` runs even when no version is new.
set -euo pipefail

STATE="${XDG_STATE_HOME:-$HOME/.local/state}/claude-replay/unknown-review/canary"
CONFIG="${UNKNOWN_REVIEW_ENV:-$HOME/.config/claude-replay/unknown-review.env}"
if [ -f "$CONFIG" ]; then set -a; . "$CONFIG"; set +a; fi
export PATH="$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
AGENT_REPLAY="${AGENT_REPLAY:-agent-replay}"
PROMPT='Run the shell command `echo canary-ok` and reply with exactly what it printed.'
FORCE=0
[ "${1:-}" = "--force" ] && FORCE=1
mkdir -p "$STATE"
umask 077
log() { printf '%s canary: %s\n' "$(date -u +%FT%TZ)" "$*" >&2; }

# Every store the sweep could read, pointed into the throwaway home: nothing real is read.
hermetic() {
  local root="$1"; shift
  CLAUDE_PROJECTS_DIR="$root/claude-projects" CODEX_HOME="$root/codex" \
    QODER_PROJECTS_DIR="$root/none" QODER_TASKS_ROOT="$root/none" \
    QODERWORK_PROJECTS_DIR="$root/none" QODERWORK_DB="$root/none.db" \
    QWENWORK_PROJECTS_DIR="$root/none" QWENWORK_DB="$root/none.db" \
    CLAUDE_REPLAY_CACHE="$root/cache" "$@"
}

# Sweep one canary transcript and print its findings as scan rows.
sweep() {
  local agent="$1" version="$2" transcript="$3" root="$4" keep
  keep="$STATE/$agent-$version.jsonl"
  cp "$transcript" "$keep"
  {
    hermetic "$root" "$AGENT_REPLAY" "$transcript" --unknown --json 2>/dev/null || true
    hermetic "$root" "$AGENT_REPLAY" "$transcript" --field-coverage --json 2>/dev/null || true
  } | python3 - "$agent" "$version" "$keep" <<'PY'
import json, sys
agent, version, keep = sys.argv[1], sys.argv[2], sys.argv[3]
for line in sys.stdin:
    try:
        r = json.loads(line)
    except ValueError:
        continue
    if "field" in r:
        # A coverage row: one session judges no drop, but a field it never wrote at all is news.
        if r.get("records", 0) and r.get("present", 0) == 0:
            r = {"agent": r["agent"], "where": "field.empty", "name": f'{r["field"]}@{r["version"]}',
                 "count": r["records"], "version": r["version"], "example": None}
        else:
            continue
    r["canary"] = True
    r["canary_transcript"] = keep
    print(json.dumps(r))
PY
}

canary_claude() {
  command -v claude >/dev/null || { log "claude is not installed; skipped"; return 0; }
  local version root transcript
  version=$(claude --version 2>/dev/null | awk '{print $1}')
  if [ "$FORCE" = 0 ] && [ "$version" = "$(cat "$STATE/last-claude" 2>/dev/null)" ]; then return 0; fi
  if [ -z "${CLAUDE_CODE_OAUTH_TOKEN:-}" ]; then
    log "claude $version is new, but no CLAUDE_CODE_OAUTH_TOKEN in $CONFIG (run \`claude setup-token\` once); skipped"
    return 0
  fi
  root=$(mktemp -d "${TMPDIR:-/tmp}/version-canary.XXXXXX")
  mkdir -p "$root/claude-projects" "$root/cfg" "$root/work"
  # The config dir IS the throwaway home's ~/.claude; its projects are the store the sweep reads.
  ln -s "$root/claude-projects" "$root/cfg/projects"
  if ! (cd "$root/work" && CLAUDE_CONFIG_DIR="$root/cfg" timeout 300 claude -p "$PROMPT" \
        --allowedTools "Bash(echo:*)" --max-turns 4 > "$root/out.txt" 2>&1); then
    log "claude $version: the run failed ($(tail -1 "$root/out.txt"))"; rm -rf "$root"; return 1
  fi
  transcript=$(find "$root/claude-projects" -name '*.jsonl' -type f | head -1)
  if [ -z "$transcript" ]; then log "claude $version: no transcript was written"; rm -rf "$root"; return 1; fi
  sweep claude "$version" "$transcript" "$root"
  echo "$version" > "$STATE/last-claude"
  log "claude $version: swept $(basename "$transcript")"
  rm -rf "$root"
}

canary_codex() {
  command -v codex >/dev/null || { log "codex is not installed; skipped"; return 0; }
  local version root transcript before age
  version=$(codex --version 2>/dev/null | awk '{print $2}')
  if [ "$FORCE" = 0 ] && [ "$version" = "$(cat "$STATE/last-codex" 2>/dev/null)" ]; then return 0; fi
  [ -f "$HOME/.codex/auth.json" ] || { log "codex $version is new, but there is no ~/.codex/auth.json; skipped"; return 0; }
  age=$(python3 - "$HOME/.codex/auth.json" <<'PY'
import datetime, json, sys
lr = json.load(open(sys.argv[1])).get("last_refresh") or ""
try:
    t = datetime.datetime.fromisoformat(lr.replace("Z", "+00:00"))
    print(int((datetime.datetime.now(datetime.timezone.utc) - t).total_seconds() // 86400))
except ValueError:
    print(999)
PY
)
  if [ "$age" -ge 7 ]; then
    log "codex $version is new, but its login is $age days old and the run would refresh it; skipped until your own Codex refreshes it"
    return 0
  fi
  root=$(mktemp -d "${TMPDIR:-/tmp}/version-canary.XXXXXX")
  mkdir -p "$root/codex" "$root/work"
  cp "$HOME/.codex/auth.json" "$root/codex/auth.json"
  [ -f "$HOME/.codex/config.toml" ] && cp "$HOME/.codex/config.toml" "$root/codex/config.toml"
  before=$(shasum "$HOME/.codex/auth.json" | cut -d' ' -f1)
  if ! (cd "$root/work" && CODEX_HOME="$root/codex" timeout 300 codex exec --skip-git-repo-check \
        --sandbox read-only "$PROMPT" > "$root/out.txt" 2>&1); then
    log "codex $version: the run failed ($(tail -1 "$root/out.txt"))"
  fi
  # A refresh during the run rotated the login: keep the owner's copy current, if it is untouched.
  if ! cmp -s "$root/codex/auth.json" "$HOME/.codex/auth.json"; then
    if [ "$(shasum "$HOME/.codex/auth.json" | cut -d' ' -f1)" = "$before" ]; then
      cp "$root/codex/auth.json" "$HOME/.codex/auth.json.canary-new" && mv -f "$HOME/.codex/auth.json.canary-new" "$HOME/.codex/auth.json"
      log "codex $version refreshed its login during the run; the fresh login was written back to ~/.codex/auth.json"
    else
      log "codex $version refreshed its login during the run, and ~/.codex/auth.json changed meanwhile; left as it is"
    fi
  fi
  transcript=$(find "$root/codex/sessions" -name '*.jsonl' -type f 2>/dev/null | head -1)
  if [ -z "$transcript" ]; then log "codex $version: no transcript was written ($(tail -1 "$root/out.txt"))"; rm -rf "$root"; return 1; fi
  sweep codex "$version" "$transcript" "$root"
  echo "$version" > "$STATE/last-codex"
  log "codex $version: swept $(basename "$transcript")"
  rm -rf "$root"
}

status=0
canary_claude || status=1
canary_codex || status=1
exit $status
