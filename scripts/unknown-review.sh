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
#   2. filter — rows already handed to a task (state/triaged.tsv) are not analysed again, and nor
#      are QoderWork's: the owner is sunsetting it (2026-09-26, "the app is no[w] in the process of
#      sunsetting so no more investment"). Its transcripts are Claude-shaped and read by the Claude
#      family, so its rows arrive labelled `claude`; a row whose example session is a QoderWork
#      transcript is QoderWork's, and is counted in the log instead of analysed.
#      …and `agent-replay --field-coverage` over COVERAGE_WINDOW (14d): a field the newest client
#      version writes clearly less often than the ones before it joins the scan as a row (#363).
#      …and scripts/version-canary.sh: a newly installed Claude Code or Codex run once in a throwaway
#      home with your own login, its transcript swept the same day (#364).
#   3. analyse — a headless `claude -p` in this repo, briefed by scripts/unknown-review.md, with a
#      read-only tool allowlist plus taskq and the web: it judges each new row, checks the known
#      prices against their official sources (every run — a price can move with no new row), and
#      QUEUES tasks tagged origin=unknown-review. It edits nothing.
#   4. record — the rows the queued tasks name become triaged; a row no task names is looked at
#      again tomorrow, which is right only when the analysis could not decide.
#   5. message — ONE dws message to the owner naming the queued tasks, only when there are any
#      (and one when the job itself fails, so a broken job is never silent). The recipient is the
#      account dws is signed in as, resolved at run time: no identifier is stored or committed.
#      A message dws cannot deliver is KEPT in state/outbox/ and sent at the start of the next
#      run (or by --flush), and the Mac shows a notification saying why — a lapsed dws login
#      once lost a day's message with nothing but a log line to show for it (2026-10-02).
#
# State and logs: ~/.local/state/claude-replay/unknown-review/ (runs.log, triaged.tsv, the last
# scan and analysis); the LaunchAgent's own stdout/stderr go to /tmp/unknown-review.{out,err}.log.
# Network: ~/.config/claude-replay/unknown-review.env (the proxy and DWS_CHANNEL launchd lacks).
# --dry-run scans and prints the brief it would send, then stops: no canary, no analysis, no message.
# --flush sends what the outbox holds, then stops — run it after `dws auth login`.
# Installed (and checked: --status) by scripts/unknown-review-setup.sh.
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
# dws's channel, where an interactive shell gets it: aries-black's ~/.config/dws/env, or the knack
# one-liner's alibaba-env.sh on a box it set up (alilang-watch reads the same two). A launchd job
# reads no shell profile, and without the channel every call is ENTERPRISE_NOT_AUTHORIZED.
for f in "$HOME/.config/dws/env" "$HOME/.config/knack/alibaba-env.sh"; do
  [ -n "${DWS_CHANNEL:-}" ] && break
  # Not this job's files: an unset variable or a failing line in one must not end the run.
  if [ -f "$f" ]; then set +u; . "$f" || true; set -u; fi
done
[ -n "${DWS_CHANNEL:-}" ] && export DWS_CHANNEL
AGENT_REPLAY="${AGENT_REPLAY:-agent-replay}"
CLAUDE="${CLAUDE_BIN:-claude}"
TASKQ="${TASKQ:-$HOME/.claude/skills/agentdev/skills/taskq/scripts/taskq}"
DRY=0 FLUSH=0
[ "${1:-}" = "--dry-run" ] && DRY=1
[ "${1:-}" = "--flush" ] && FLUSH=1

mkdir -p "$STATE" "$STATE/outbox"
touch "$STATE/triaged.tsv"
log() { printf '%s %s\n' "$(date -u +%FT%TZ)" "$*" | tee -a "$STATE/runs.log"; }

# Who dws is signed in as, or — on stderr, for the log — why it cannot say. `auth status` is no
# answer: on 2026-10-02 it read "authenticated" with a refresh token good for a month while every
# call was refused as not logged in, so the call that sends is the call that is asked.
dws_me() {
  local out me
  out=$(dws contact user get-self --format json 2>&1) || true
  me=$(printf '%s' "$out" | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
except ValueError:
    sys.exit(0)
r = d.get("result")
if isinstance(r, list) and r:
    print((r[0].get("orgEmployeeModel") or {}).get("userId") or "")
' 2>/dev/null)
  if [ -n "$me" ]; then printf '%s' "$me"; return 0; fi
  case "$out" in
    *not_authenticated*) echo "the dws login has lapsed: run \`dws auth login\` at this Mac, then \`$REPO/scripts/unknown-review.sh --flush\`" >&2 ;;
    *ENTERPRISE_NOT_AUTHORIZED*) echo "dws has no DWS_CHANNEL (looked in $CONFIG, ~/.config/dws/env, ~/.config/knack/alibaba-env.sh)" >&2 ;;
    "") echo "dws is not installed or printed nothing" >&2 ;;
    *) echo "dws could not say who it is signed in as: $(printf '%s' "$out" | tr '\n' ' ' | cut -c1-160)" >&2 ;;
  esac
  return 1
}

# A notification on this Mac, for when DingTalk is the thing that is broken.
desktop() {
  [ "${UNKNOWN_REVIEW_DESKTOP:-1}" = 1 ] || return 0
  osascript -e 'on run argv' -e 'display notification (item 2 of argv) with title (item 1 of argv)' \
    -e 'end run' "$1" "$2" >/dev/null 2>&1 || true
}

# Send one held message; it leaves the outbox only once dws has taken it. Its uuid is fixed when it
# is written, so a retry of a message dws did take is not a second message.
send_held() {
  local file="$1" me why title text uuid
  title=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["title"])' "$file")
  text=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["text"])' "$file")
  uuid=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["uuid"])' "$file")
  if ! me=$(dws_me 2>"$STATE/dws.why"); then
    why=$(cat "$STATE/dws.why")
    log "dws: not sent, kept in the outbox — $why"
    desktop "agent-monitor review: a message is waiting" "$why"
    return 1
  fi
  if dws chat message send --user "$me" --title "$title" --text "$text" --uuid "$uuid" \
       --format json --yes >/dev/null 2>>"$STATE/dws.err"; then
    /bin/rm -f "$file"
    return 0
  fi
  log "dws: the send failed, kept in the outbox (see $STATE/dws.err)"
  desktop "agent-monitor review: a message is waiting" "dws could not send it; see $STATE/dws.err"
  return 1
}

# Send whatever earlier runs could not, oldest first. Stops at the first failure: the rest would
# fail for the same reason, and one notification says it.
flush_outbox() {
  local f sent=0
  for f in "$STATE"/outbox/*.json; do
    [ -f "$f" ] || continue
    send_held "$f" || return 1
    sent=$((sent + 1))
  done
  [ "$sent" = 0 ] || log "dws: delivered $sent message(s) held from an earlier run"
}

# The one message this job sends, to the account dws is signed in as: written to the outbox first,
# so a message dws cannot take now is sent by a later run instead of lost.
notify() {
  local title="$1" text="$2" uuid file
  uuid="unknown-review-$(date +%F)-$(printf '%s' "$text" | shasum | cut -c1-12)"
  file="$STATE/outbox/$(date -u +%Y%m%dT%H%M%SZ)-$uuid.json"
  python3 -c 'import json,sys; json.dump({"title": sys.argv[1], "text": sys.argv[2], "uuid": sys.argv[3]}, open(sys.argv[4], "w"))' \
    "$title" "$text" "$uuid" "$file"
  send_held "$file"
}

if [ $FLUSH = 1 ]; then
  flush_outbox || true
  left=$(find "$STATE/outbox" -name '*.json' | wc -l | tr -d ' ')
  echo "outbox: $left message(s) waiting"
  [ "$left" = 0 ] && exit 0 || exit 1
fi
# What an earlier run could not send goes first, so it arrives before today's. A failure here is
# logged and notified inside; the review itself still runs.
[ $DRY = 1 ] || flush_outbox || true

fail() {
  log "FAILED: $1"
  [ $DRY = 1 ] || notify "agent-monitor daily review failed" "The daily unknown-shape and pricing review failed: $1. Log: $STATE/runs.log" || true
  exit 1
}

# 1. scan
"$AGENT_REPLAY" --unknown --since "$WINDOW" --json > "$STATE/scan.jsonl" 2> "$STATE/scan.err" \
  || fail "the scan did not run ($(tail -1 "$STATE/scan.err"))"
scanned=$(grep -o 'scanning [0-9]* transcript' "$STATE/scan.err" | grep -o '[0-9]*' || echo "?")
# 1b. field coverage (#363): a KNOWN field our cost or cards read going empty after a client update —
# which no new shape reveals. Over a longer window than the shapes, since the newest version is
# judged against the ones before it. A drop joins the scan as a `field.dropped` row named
# `<field>@<version>`, so it is triaged once per version through the same path as a shape.
COVERAGE_WINDOW="${COVERAGE_WINDOW:-14d}"
if ! "$AGENT_REPLAY" --field-coverage --since "$COVERAGE_WINDOW" --json > "$STATE/coverage.jsonl" 2> "$STATE/coverage.err"; then
  # An installed agent-replay older than the release that brought the mode: skip, say so, go on.
  grep -q -- "unexpected argument '--field-coverage'" "$STATE/coverage.err" \
    || fail "the coverage scan did not run ($(tail -1 "$STATE/coverage.err"))"
  log "coverage: this agent-replay predates --field-coverage (#363); skipped until it is upgraded"
  : > "$STATE/coverage.jsonl"
fi
python3 - "$STATE/coverage.jsonl" >> "$STATE/scan.jsonl" <<'PY'
import json, sys
for line in open(sys.argv[1]):
    r = json.loads(line)
    if r.get("dropped"):
        print(json.dumps({"agent": r["agent"], "where": "field.dropped", "name": f'{r["field"]}@{r["version"]}',
                          "count": r["records"], "version": r["version"], "example": None,
                          "rate": r["rate"], "usual": r["usual"]}))
PY

# 1c. the version canary (#364): a Claude Code or Codex version installed since the last run is run
# once in a throwaway home and its transcript swept; what it finds joins the scan as rows with
# `canary: true`. Its failure is logged, never the job's: tomorrow tries again.
# A dry run spends no model call, so it leaves the canary alone.
if [ $DRY = 1 ]; then
  echo "canary: skipped (--dry-run)" >&2
else
  "$REPO/scripts/version-canary.sh" >> "$STATE/scan.jsonl" 2>> "$STATE/canary.log" \
    || log "canary: a run failed (see $STATE/canary.log)"
fi

# 2. filter
QODERWORK_STORE="${QODERWORK_PROJECTS_DIR:-$HOME/.qoderwork/projects}"
python3 - "$STATE/scan.jsonl" "$STATE/triaged.tsv" "$QODERWORK_STORE" \
  > "$STATE/new.jsonl" 2> "$STATE/filter.err" <<'PY'
import json, os, sys
scan, triaged, qoderwork = sys.argv[1], sys.argv[2], sys.argv[3]
done = {tuple(line.rstrip("\n").split("\t")[:3]) for line in open(triaged) if line.strip()}
# QoderWork is being sunset: its sessions, by the transcripts in its own store.
sunset = {
    name[: -len(".jsonl")]
    for _, _, files in os.walk(qoderwork)
    for name in files
    if name.endswith(".jsonl")
}
skipped = 0
for line in open(scan):
    row = json.loads(line)
    if (row["agent"], row["where"], row["name"]) in done:
        continue
    if row.get("example") in sunset:
        skipped += 1
        continue
    print(line.rstrip("\n"))
print(skipped, file=sys.stderr)
PY
new=$(wc -l < "$STATE/new.jsonl" | tr -d ' ')
sunset_skipped=$(tail -1 "$STATE/filter.err")
log "scanned $scanned transcript(s) in $WINDOW: $(wc -l < "$STATE/scan.jsonl" | tr -d ' ') row(s), $new new, $sunset_skipped QoderWork row(s) skipped (sunset)"

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
