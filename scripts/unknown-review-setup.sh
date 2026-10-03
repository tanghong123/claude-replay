#!/bin/bash
# scripts/unknown-review-setup.sh [--at HH:MM] [--dry-run] [--no-load]
# scripts/unknown-review-setup.sh --status
# scripts/unknown-review-setup.sh --uninstall
#
# Installs the daily review (#276, scripts/unknown-review.sh) on this Mac, from wherever this
# checkout is: the machine-local env file the job reads, and the LaunchAgent that runs it. Safe to
# re-run — it is also how the job is repaired or moved to another hour. It never overwrites the env
# file (it may hold CLAUDE_CODE_OAUTH_TOKEN); delete the file to have it written again.
#
#   --at HH:MM   when the job runs each day (default 09:30; launchd runs a missed one at the next wake)
#   --dry-run    print the files it would write and change nothing
#   --no-load    write the files but leave launchd alone
#   --status     check an installed job, end to end, and exit 1 if it cannot do its work
#   --uninstall  unload the job and move its plist aside (the env file and the state stay)
#
# Logins are not this script's: `claude` (the analysis), `dws auth login` (the message) and, for the
# version canary, `claude setup-token` and `codex login`. --status says which are missing.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
JOB="$REPO/scripts/unknown-review.sh"
LABEL="${UNKNOWN_REVIEW_LABEL:-com.$(id -un).unknown-review}"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
CONFIG="${UNKNOWN_REVIEW_ENV:-$HOME/.config/claude-replay/unknown-review.env}"
STATE="${XDG_STATE_HOME:-$HOME/.local/state}/claude-replay/unknown-review"
TASKQ="${TASKQ:-$HOME/.claude/skills/agentdev/skills/taskq/scripts/taskq}"
# rowt's local proxy, the same port on every box (agentdev-setup 2.1). launchd gives a job no proxy,
# and without it the analysis's API calls answer "403 Request not allowed".
ROWT_PROXY="http://127.0.0.1:7890"
ROWT_SOCKS="socks5h://127.0.0.1:7890"
export PATH="$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"

AT="09:30" MODE=install LOAD=1
while [ $# -gt 0 ]; do
  case "$1" in
    --at) AT="${2:?--at needs HH:MM}"; shift 2 ;;
    --dry-run) MODE=dry; shift ;;
    --no-load) LOAD=0; shift ;;
    --status) MODE=status; shift ;;
    --uninstall) MODE=uninstall; shift ;;
    -h|--help) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1 (see --help)" >&2; exit 2 ;;
  esac
done
case "$AT" in
  [0-2][0-9]:[0-5][0-9]) HOUR=$((10#${AT%%:*})) MINUTE=$((10#${AT##*:})) ;;
  *) echo "--at wants HH:MM, got $AT" >&2; exit 2 ;;
esac
[ "$HOUR" -lt 24 ] || { echo "--at wants HH:MM, got $AT" >&2; exit 2; }

ok() { printf '  ok    %s\n' "$*"; }
warn() { printf '  WARN  %s\n' "$*"; }
bad() { printf '  FAIL  %s\n' "$*"; FAILED=1; }
FAILED=0

plist_text() {
  cat <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>$LABEL</string>

    <!-- The daily review of claude-replay (#276): what the adapters drop, which models have no
         price, and whether the known prices moved. The script (in the repo, versioned with the
         adapters it reviews) scans with \`agent-replay --unknown\`, runs a headless \`claude -p\`
         that QUEUES tasks for anything new, and sends the owner one dws message when it did.
         Invoked directly so launchctl and Background Items name it, not "bash".
         Written by scripts/unknown-review-setup.sh; re-run that to change it. -->
    <key>ProgramArguments</key>
    <array>
        <string>$JOB</string>
    </array>

    <key>WorkingDirectory</key>
    <string>$REPO</string>

    <!-- Daily. Asleep then? launchd runs it at the next wake (StartCalendarInterval),
         rather than skipping the day. -->
    <key>StartCalendarInterval</key>
    <dict>
        <key>Hour</key><integer>$HOUR</integer>
        <key>Minute</key><integer>$MINUTE</integer>
    </dict>

    <!-- Never wake the Mac for it. -->
    <key>ProcessType</key>
    <string>Background</string>

    <key>StandardOutPath</key>
    <string>/tmp/unknown-review.out.log</string>
    <key>StandardErrorPath</key>
    <string>/tmp/unknown-review.err.log</string>
</dict>
</plist>
EOF
}

# The proxy this box's shells use, or rowt's when none is set and rowt answers.
proxy_lines() {
  local https="${HTTPS_PROXY:-${https_proxy:-}}" http="${HTTP_PROXY:-${http_proxy:-}}"
  local all="${ALL_PROXY:-${all_proxy:-}}" no="${NO_PROXY:-${no_proxy:-}}"
  if [ -z "$https" ] && curl -s -o /dev/null -m 5 -x "$ROWT_PROXY" https://api.anthropic.com/ 2>/dev/null; then
    https="$ROWT_PROXY" http="$ROWT_PROXY" all="$ROWT_SOCKS"
  fi
  [ -n "$https" ] || return 0
  [ -n "$http" ] || http="$https"
  [ -n "$no" ] || no="localhost,127.0.0.1,::1,.local"
  printf 'HTTPS_PROXY=%s\nhttps_proxy=%s\nHTTP_PROXY=%s\nhttp_proxy=%s\n' "$https" "$https" "$http" "$http"
  [ -z "$all" ] || printf 'ALL_PROXY=%s\nall_proxy=%s\n' "$all" "$all"
  printf 'NO_PROXY=%s\nno_proxy=%s\n' "$no" "$no"
}

env_text() {
  echo "# The daily review's network and logins (#276), which launchd does not provide. Machine-local,"
  echo "# 0600, read by scripts/unknown-review.sh and scripts/version-canary.sh. Written $(date +%F) by"
  echo "# scripts/unknown-review-setup.sh, which never overwrites it."
  local proxies
  proxies=$(proxy_lines)
  if [ -n "$proxies" ]; then echo "$proxies"; else echo "# No proxy found (none in this shell, rowt not answering on 127.0.0.1:7890)."; fi
  # The channel is read from dws's or knack's own file when one exists; only a box with neither
  # needs it here.
  if [ ! -f "$HOME/.config/dws/env" ] && [ ! -f "$HOME/.config/knack/alibaba-env.sh" ] && [ -n "${DWS_CHANNEL:-}" ]; then
    echo "DWS_CHANNEL=$DWS_CHANNEL"
  fi
  echo "# For the version canary's Claude run: a long-lived token from \`claude setup-token\`."
  echo "# CLAUDE_CODE_OAUTH_TOKEN="
}

# Who dws is signed in as — the call the job itself makes (`auth status` can say "authenticated"
# while every call is refused).
dws_check() {
  command -v dws >/dev/null || { warn "dws is not installed: the job queues tasks but cannot message you"; return; }
  local out
  out=$( (
    set +u
    [ -f "$CONFIG" ] && { set -a; . "$CONFIG"; set +a; }
    for f in "$HOME/.config/dws/env" "$HOME/.config/knack/alibaba-env.sh"; do
      [ -n "${DWS_CHANNEL:-}" ] && break
      [ -f "$f" ] && . "$f"
    done
    [ -n "${DWS_CHANNEL:-}" ] && export DWS_CHANNEL
    dws contact user get-self --format json 2>&1
  ) || true)
  case "$out" in
    *orgEmployeeModel*) ok "dws: signed in, and the channel is found" ;;
    *not_authenticated*) bad "dws: the login has lapsed — run \`dws auth login\` at this Mac, then \`$JOB --flush\`" ;;
    *ENTERPRISE_NOT_AUTHORIZED*) bad "dws: no DWS_CHANNEL in $CONFIG, ~/.config/dws/env or ~/.config/knack/alibaba-env.sh" ;;
    *) bad "dws: $(printf '%s' "$out" | tr '\n' ' ' | cut -c1-140)" ;;
  esac
}

status() {
  echo "unknown-review on $(scutil --get LocalHostName 2>/dev/null || hostname), from $REPO"
  # The job.
  if [ -f "$PLIST" ]; then
    local prog
    prog=$(plutil -extract ProgramArguments.0 raw "$PLIST" 2>/dev/null || true)
    [ "$prog" = "$JOB" ] && ok "plist runs $JOB" || bad "plist runs ${prog:-nothing} — re-run this script from the checkout it should use"
    ok "daily at $(printf '%02d:%02d' "$(plutil -extract StartCalendarInterval.Hour raw "$PLIST")" "$(plutil -extract StartCalendarInterval.Minute raw "$PLIST")")"
  else
    bad "no $PLIST — run this script without --status"
  fi
  if launchctl print "gui/$(id -u)/$LABEL" >/dev/null 2>&1; then
    ok "launchd has $LABEL ($(launchctl print "gui/$(id -u)/$LABEL" | awk -F' = ' '/^\truns =/{r=$2} /last exit code/{e=$2} END{printf "runs %s, last exit %s", r, e}'))"
  else
    bad "launchd has not loaded $LABEL"
  fi
  [ -x "$JOB" ] || bad "$JOB is not executable"
  # The env file: names only, never values.
  if [ -f "$CONFIG" ]; then
    local mode keys
    mode=$(stat -f '%Lp' "$CONFIG")
    keys=$(grep -oE '^[A-Za-z_]+=' "$CONFIG" | tr -d '=' | sort -u | paste -sd' ' -)
    [ "$mode" = 600 ] && ok "$CONFIG (0600): ${keys:-no keys}" || warn "$CONFIG is mode $mode, not 600: ${keys:-no keys}"
  else
    bad "no $CONFIG — run this script without --status"
  fi
  local proxy
  proxy=$( (set +u; [ -f "$CONFIG" ] && . "$CONFIG"; printf '%s' "${HTTPS_PROXY:-}") )
  if [ -n "$proxy" ]; then
    local code
    code=$(curl -s -o /dev/null -w '%{http_code}' -m 10 -x "$proxy" https://api.anthropic.com/ 2>/dev/null || true)
    case "$code" in 000|403|"") bad "the proxy in the env file does not reach api.anthropic.com (HTTP ${code:-none})" ;; *) ok "the proxy reaches api.anthropic.com (HTTP $code)" ;; esac
  else
    warn "no proxy in the env file: the analysis needs one where api.anthropic.com is not reachable directly"
  fi
  # The tools.
  local t
  for t in agent-replay claude python3 git; do
    command -v "$t" >/dev/null && ok "$t: $(command -v "$t")" || bad "$t is not on the job's PATH ($HOME/.local/bin, /opt/homebrew/bin, /usr/local/bin)"
  done
  if command -v agent-replay >/dev/null; then
    agent-replay --help 2>/dev/null | grep -q -- '--field-coverage' \
      || warn "agent-replay $(agent-replay --version 2>/dev/null | awk '{print $NF}') predates --field-coverage: the coverage check is skipped until it is upgraded"
  fi
  [ -x "$TASKQ" ] && ok "taskq: $TASKQ" || bad "no taskq at $TASKQ (the agentdev skills)"
  dws_check
  # The canary's logins: optional, each skipped and logged when missing.
  if grep -qE '^CLAUDE_CODE_OAUTH_TOKEN=.+' "$CONFIG" 2>/dev/null; then ok "canary: Claude token present"
  else warn "canary: no CLAUDE_CODE_OAUTH_TOKEN in $CONFIG — new Claude Code versions are not canaried (\`claude setup-token\`, then add the line)"; fi
  if command -v codex >/dev/null; then
    [ -f "$HOME/.codex/auth.json" ] && ok "canary: Codex login present" || warn "canary: Codex has no login (\`codex login\`)"
  fi
  # The last run, and anything waiting to be sent.
  [ -f "$STATE/runs.log" ] && ok "last log line: $(tail -1 "$STATE/runs.log")" || warn "the job has not run yet"
  local held
  held=$(find "$STATE/outbox" -name '*.json' 2>/dev/null | wc -l | tr -d ' ')
  [ "$held" = 0 ] || warn "$held message(s) wait in the outbox: \`$JOB --flush\` once dws works"
  return $FAILED
}

case "$MODE" in
  status) status; exit ;;
  uninstall)
    launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null && echo "unloaded $LABEL" || echo "$LABEL was not loaded"
    if [ -f "$PLIST" ]; then
      /bin/mv -f "$PLIST" "$PLIST.disabled-$(date +%Y%m%d)"
      echo "moved the plist to $PLIST.disabled-$(date +%Y%m%d)"
    fi
    echo "kept: $CONFIG and $STATE"
    exit 0 ;;
  dry)
    echo "# would write $PLIST:"; plist_text
    if [ -f "$CONFIG" ]; then echo "# $CONFIG exists and would be left as it is"
    else echo "# would write $CONFIG (0600), with these keys:"; env_text | grep -oE '^[A-Za-z_]+=' | tr -d '=' | sed 's/^/#   /'; fi
    exit 0 ;;
esac

# install
[ -x "$JOB" ] || chmod +x "$JOB"
mkdir -p "$(dirname "$CONFIG")" "$(dirname "$PLIST")" "$STATE"
if [ -f "$CONFIG" ]; then
  echo "env file: $CONFIG exists, left as it is"
else
  (umask 077; env_text > "$CONFIG")
  echo "env file: wrote $CONFIG (0600)"
fi
changed=1
if [ -f "$PLIST" ] && [ "$(plist_text)" = "$(cat "$PLIST")" ]; then
  changed=0
  echo "plist: $PLIST unchanged"
else
  if [ -f "$PLIST" ]; then
    /bin/cp -f "$PLIST" "$PLIST.bak-$(date +%Y%m%d%H%M%S)"
    echo "plist: backed up the old one to $PLIST.bak-$(date +%Y%m%d%H%M%S)"
  fi
  plist_text > "$PLIST.tmp" && /bin/mv -f "$PLIST.tmp" "$PLIST"
  plutil -lint "$PLIST" >/dev/null
  echo "plist: wrote $PLIST (daily at $AT)"
fi
if [ "$LOAD" = 1 ]; then
  if [ "$changed" = 1 ] || ! launchctl print "gui/$(id -u)/$LABEL" >/dev/null 2>&1; then
    launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
    launchctl bootstrap "gui/$(id -u)" "$PLIST"
    echo "launchd: loaded $LABEL"
  else
    echo "launchd: $LABEL already loaded"
  fi
else
  echo "launchd: left alone (--no-load)"
fi
echo
status || {
  echo
  echo "Installed, with the FAILs above still to fix. Re-check with: $0 --status"
  exit 1
}
echo
echo "Installed. A scan without the analysis or a message: $JOB --dry-run"
