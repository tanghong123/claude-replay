# The ONE definition of where the byte-gate's data lives. Sourced by gate.sh,
# rebaseline.sh and verify.sh so the location can never drift between them.
#
# NOT /tmp. The data (BASE + the frozen input fixtures) is deliberately out of git —
# the fixtures embed real session content — so it needs a durable home of its own.
# `/tmp` is actively hostile to it: the frozen INPUT is the only file here that is
# never rewritten, so macOS's periodic /tmp cleanup reaps it by age first, while BASE
# survives because every re-baseline touches it. The result is a gate that compares
# real output against a MISSING input and reports it as an enormous phantom content
# regression. Keeping the data under the user cache removes that failure entirely.
#
# Override with SC_GATE_DIR to point at a scratch copy.
GATE_DIR="${SC_GATE_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/claude-replay-gate}"

# The home the frozen sessions were RECORDED under, when the data names it ($GATE_DIR/recorded-home,
# one line). The renderer writes a path under the current home as `~/…`, so the same session renders
# differently on a machine whose home differs — measured 2026-10-04: 80 lines of one dump, `~/x` on
# the recording machine and `/Users/<them>/x` on another. Every render runs with HOME set to it
# (`render`), so one BASE serves every machine. Without the file, renders use the current HOME.
RENDER_HOME="$HOME"
if [ -f "$GATE_DIR/recorded-home" ]; then RENDER_HOME=$(head -1 "$GATE_DIR/recorded-home"); fi
render() { HOME="$RENDER_HOME" "$@"; }
