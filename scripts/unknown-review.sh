#!/bin/bash
# scripts/unknown-review.sh [--dry-run]
#
# The daily review (#276), run by the LaunchAgent com.hong.unknown-review. The owner, 2026-09-25:
# "set up a period job (e.g. one day) to review the agent-monitor log to see there is any unknown
# dropped messages. If so, analyze them and see whether they are worthwhile to be included in
# agent-monitor. And queue up tasks correspondingly for execution accordingly. Also use the dws
# tool to send me a message when new work is found" — and in the same job, "check latest model
# pricing info, and update them if different. Also scan for new model strings that are not in our
# database, and retrieve prices for them".
#
#   1. scan — `agent-replay --unknown --since <window> --json`, machine-wide: every shape the
#      adapters (the same ones agent-monitor renders with) did not recognise, and every model that
#      produced tokens with no price in pricing.json. The monitor itself keeps no such log.
#   2. filter — rows already handed to a task (state/triaged.tsv) are not analysed again.
#   3. analyse — a headless `claude -p` in this repo, briefed by scripts/unknown-review.md, with a
#      read-only tool allowlist plus taskq and the web: it judges each new row, checks the known
#      prices against their official sources (every run — a price can move with no new row), and
#      QUEUES tasks tagged origin=unknown-review. It edits nothing.
#   4. record — the rows the queued tasks name become triaged; a row no task names is looked at
#      again tomorrow, which is right only when the analysis could not decide.
#   5. message — ONE dws message to the owner naming the queued tasks, only when there are any
#      (and one when the job itself fails, so a broken job is never silent). The recipient is the
#      account dws is signed in as, resolved at run time: no identifier is stored or committed.
#
# State and logs: ~/.local/state/claude-replay/unknown-review/ (runs.log, triaged.tsv, the last
# scan and analysis); the LaunchAgent's own stdout/stderr go to /tmp/unknown-review.{out,err}.log.
# Network: ~/.config/claude-replay/unknown-review.env (the proxy and DWS_CHANNEL launchd lacks).
# --dry-run scans and prints the brief it would send, then stops: no analysis, no message.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STATE="${XDG_STATE_HOME:-$HOME/.local/state}/claude-replay/unknown-review"
WINDOW="${UNKNOWN_REVIEW_WINDOW:-26h}"
# launchd hands a job /usr/bin:/bin; everything this uses lives elsewhere.
export PATH="$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
# ...and none of the network an interactive session inherits: the local proxy every agent here goes
# through (without it the API answers "403 Request not allowed") and dws's channel. Measured on the
# first run. The file is machine-local (0600), written from a session's environment.
CONFIG="${UNKNOWN_REVIEW_ENV:-$HOME/.config/claude-replay/unknown-review.env}"
if [ -f "$CONFIG" ]; then set -a; . "$CONFIG"; set +a; fi
AGENT_REPLAY="${AGENT_REPLAY:-agent-replay}"
CLAUDE="${CLAUDE_BIN:-claude}"
TASKQ="${TASKQ:-$HOME/.claude/skills/agentdev/skills/taskq/scripts/taskq}"
DRY=0
[ "${1:-}" = "--dry-run" ] && DRY=1

mkdir -p "$STATE"
touch "$STATE/triaged.tsv"
log() { printf '%s %s\n' "$(date -u +%FT%TZ)" "$*" | tee -a "$STATE/runs.log"; }

# The one message this job sends, to the account dws is signed in as.
notify() {
  local title="$1" text="$2" me
  me=$(dws contact user get-self --format json --jq '.result[0].orgEmployeeModel.userId' 2>/dev/null | tr -d '"')
  if [ -z "$me" ]; then log "dws: could not resolve the recipient (is dws signed in?)"; return 1; fi
  dws chat message send --user "$me" --title "$title" --text "$text" \
    --uuid "unknown-review-$(date +%F)-$(printf '%s' "$text" | shasum | cut -c1-12)" \
    --format json --yes >/dev/null 2>>"$STATE/dws.err" \
    || { log "dws: the message was not sent (see $STATE/dws.err)"; return 1; }
}

fail() {
  log "FAILED: $1"
  [ $DRY = 1 ] || notify "agent-monitor daily review failed" "The daily unknown-shape and pricing review failed: $1. Log: $STATE/runs.log" || true
  exit 1
}

# 1. scan
"$AGENT_REPLAY" --unknown --since "$WINDOW" --json > "$STATE/scan.jsonl" 2> "$STATE/scan.err" \
  || fail "the scan did not run ($(tail -1 "$STATE/scan.err"))"
scanned=$(grep -o 'scanning [0-9]* transcript' "$STATE/scan.err" | grep -o '[0-9]*' || echo "?")

# 2. filter
python3 - "$STATE/scan.jsonl" "$STATE/triaged.tsv" > "$STATE/new.jsonl" <<'PY'
import json, sys
scan, triaged = sys.argv[1], sys.argv[2]
done = {tuple(line.rstrip("\n").split("\t")[:3]) for line in open(triaged) if line.strip()}
for line in open(scan):
    row = json.loads(line)
    if (row["agent"], row["where"], row["name"]) not in done:
        print(line.rstrip("\n"))
PY
new=$(wc -l < "$STATE/new.jsonl" | tr -d ' ')
log "scanned $scanned transcript(s) in $WINDOW: $(wc -l < "$STATE/scan.jsonl" | tr -d ' ') row(s), $new new"

# 3. analyse — every run: the price check needs no new row to find a moved price.
started=$(date -u +%FT%TZ)
python3 - "$REPO/scripts/unknown-review.md" "$STATE/new.jsonl" "$TASKQ" > "$STATE/brief.md" <<'PY'
import sys
brief, rows, taskq = sys.argv[1], sys.argv[2], sys.argv[3]
text = open(brief).read().replace("{{TASKQ}}", taskq)
shapes = open(rows).read().strip() or "(none — only the price check today)"
print(text.replace("{{SHAPES}}", shapes))
PY
if [ $DRY = 1 ]; then cat "$STATE/brief.md"; exit 0; fi
( cd "$REPO" && "$CLAUDE" -p "$(cat "$STATE/brief.md")" \
    --allowedTools "Read" "Grep" "Glob" "WebFetch" "WebSearch" \
      "Bash(agent-replay:*)" "Bash($TASKQ:*)" "Bash(git log:*)" "Bash(git show:*)" \
      "Bash(git grep:*)" "Bash(grep:*)" "Bash(rg:*)" "Bash(head:*)" "Bash(sed -n:*)" "Bash(wc:*)" \
    > "$STATE/analysis.md" 2> "$STATE/analysis.err" ) \
  || fail "the analysis did not finish ($(tail -1 "$STATE/analysis.err" 2>/dev/null))"

# 4. record, and 5. message. The queue goes through a file: `python3 -` reads its SCRIPT from stdin,
# so a pipe would be shadowed by the heredoc and the task list would never arrive.
"$TASKQ" list --json --all 2>/dev/null | head -1 > "$STATE/queue.json" || fail "reading the queue failed"
queued=$(python3 - "$started" "$STATE/triaged.tsv" "$STATE/queue.json" <<'PY'
import json, sys
started, triaged, queue = sys.argv[1], sys.argv[2], sys.argv[3]
tasks = [t for t in json.load(open(queue))
         if (t.get("metadata") or {}).get("origin") == "unknown-review" and t.get("created_at", "") >= started]
with open(triaged, "a") as out:
    for t in tasks:
        for shape in str((t.get("metadata") or {}).get("shapes") or "").split(","):
            parts = shape.strip().split("/", 2)
            if len(parts) == 3:
                out.write("\t".join(parts + [t["id"], started]) + "\n")
for t in tasks:
    print(f'#{t["id"]} [{(t.get("metadata") or {}).get("kind", "?")}] {t["subject"]}')
PY
) || fail "reading back what the analysis queued failed"
count=$(printf '%s' "$queued" | grep -c . || true)
log "analysis queued $count task(s)"
if [ "$count" -gt 0 ]; then
  notify "agent-monitor: $count new task(s) from the daily review" \
    "The daily review of claude-replay found new work and queued $count task(s):
$queued

Analysis: $STATE/analysis.md" || true
fi
