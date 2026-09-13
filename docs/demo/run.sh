#!/bin/zsh
# run.sh [port] [big-mb] — rebuild the demo: a hermetic store, a monitor over it, two stand-in
# agent processes for the two states a reader triages by, the recorded tour, and the deck.
# Nothing here touches a real session: every prompt, answer and path is written by make_store.py.
set -e
D=${0:a:h}
R=$D/../..
PORT=${1:-2790}
BIG_MB=${2:-200}
STORE=${DEMO_STORE:-${TMPDIR:-/tmp}/agent-monitor-demo}

echo "### store ($BIG_MB MB) -> $STORE"
rm -rf $STORE
python3 $D/make_store.py $STORE $BIG_MB

echo "### monitor on 127.0.0.1:$PORT"
[ -x $R/target/release/agent-monitor-v2 ] || (cd $R && cargo build --release -p claude-monitor-v2)
CLAUDE_PROJECTS_DIR=$STORE/claude QODERWORK_PROJECTS_DIR=$STORE/qoderwork QODER_PROJECTS_DIR=$STORE/qoder \
CODEX_HOME=$STORE/codex CLAUDE_JDI_TASKS_ROOT=$STORE/claude-tasks \
XDG_CACHE_HOME=$STORE/home-cache CLAUDE_MONITOR_CACHE=$STORE/cache CLAUDE_MONITOR_STATE=$STORE/state \
  $R/target/release/agent-monitor-v2 --port $PORT > $STORE/monitor.log 2>&1 &
MON=$!
trap "kill $MON 2>/dev/null; pkill -f 'claude --resume aaaaaaaa' 2>/dev/null" EXIT
sleep 5

# The two live states. A stand-in process is a shell whose argv[0] is `claude` and whose argv
# carries the session id — what the monitor's probe links a row to — holding the transcript open.
for pair in "aaaaaaaa-0000-4000-8000-000000000001:-Users-demo-code-payments" "aaaaaaaa-0000-4000-8000-000000000002:-Users-demo-code-search"; do
  sid=${pair%%:*}; proj=${pair##*:}
  ( exec -a claude /bin/sh -c "while :; do sleep 5; done" claude --resume $sid < $STORE/claude/$proj/$sid.jsonl > /dev/null 2>&1 & )
done
sleep 25   # the state tracker publishes a non-immediate verdict after one stable tick

echo "### tour"
cd $D && PORT=$PORT BIG_MB=$BIG_MB node tour.mjs
ffmpeg -hide_banner -loglevel error -y -f concat -safe 0 -i $D/frames/frames.txt \
  -vf "scale=1440:-2:flags=lanczos,fps=30" -c:v libx264 -pix_fmt yuv420p -crf 20 -movflags +faststart \
  $D/agent-monitor-demo.mp4
for t in 4 12 20 35; do ffmpeg -hide_banner -loglevel error -y -ss $t -i $D/agent-monitor-demo.mp4 -frames:v 1 -vf "scale=1200:-2:flags=lanczos" -q:v 4 $D/still-$t.jpg; done
rm -rf $D/frames

echo "### deck"
python3 $D/build_deck.py $D/../agent-monitor-pitch.zh.html
echo "### done — docs/agent-monitor-pitch.zh.html"
