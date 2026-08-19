# Validating agent adapters with synthetic transcripts

This is the playbook for improving how a non-Claude transcript looks in `claude-replay`
without adding agent-specific behavior to a presenter. It records the method used to map
Codex 0.147's orchestrated tool events onto the existing Claude vocabulary in v1.94.0.

The method is deliberately useful even when the native Claude Code client cannot run on the
development host.

## What the oracle is

There are three distinct things that are easy to conflate:

1. **A native agent transcript** records whatever that agent chose to persist. It may contain
   model-facing wrappers, UI-facing semantic events, receipts, or several copies of one action.
2. **The adapter's canonical output** is the shared `Message`/`Block` vocabulary consumed by the
   fold and both presenters: `Bash`, `Edit`, `Write`, `TodoWrite`, `WebSearch`, and so on.
3. **A frontend rendering** is what `claude-replay --dump`, the TUI, or the HTML viewer makes
   from those canonical blocks.

The primary oracle is the output of the **same `claude-replay` binary and options** for a small,
synthetic Claude-shaped transcript describing the equivalent actions. This isolates the one
thing under test: the target agent's adapter.

This is a semantic comparison, not a byte-for-byte claim that `claude-replay` is the Claude Code
TUI. For example, the shared default fold may say `Ran 1 shell command`, while `--full` shows the
actual command. Both the reference and target should make that decision in shared code.

## The workflow

### 1. State the behavioral case before editing code

Write down the action and the expected canonical mapping. For the Codex work, the useful matrix
was:

| user-visible action | canonical vocabulary | normal full rendering |
|---|---|---|
| execute a command | `Bash` | command, cwd, output, exit status |
| inspect files/search/list directories | activity span containing `Read` / `Grep` / `LS` | action targets plus the composite command/output detail |
| change an existing file | `Edit` | `Update(path)` plus the diff |
| replace the current plan | `TodoWrite` + task snapshot | plan panel/update |
| search the web | `WebSearch` | query and search results |
| open a web result | `WebFetch` | URL and fetched result |

Do not start with the source agent's tool names. Start with what happened from the user's point
of view. The target vocabulary is the closest existing Claude-shaped action, not a transliteration
of transport internals.

### 2. Inventory the target transcript

Use a real transcript to identify every record involved in one action. Useful first passes are:

```sh
rg -n 'tools\.|CommandExecution|FileChange|item_completed|tool_call' "$TARGET"
jq -r '[.type, .payload.type, .payload.name, .payload.item.type] | map(. // "") | @tsv' \
  "$TARGET" | sort | uniq -c
```

For each action, classify records as:

- **semantic event** — the record carrying what the user saw, such as the actual command or diff;
- **transport wrapper** — model/orchestrator plumbing used to initiate it;
- **receipt** — acknowledgement that the wrapper completed;
- **independent event** — similar-looking activity that must remain visible.

Codex 0.147, for example, writes a JavaScript `custom_tool_call` named `exec`, an
`event_msg/item_completed` carrying `CommandExecution` or `FileChange`, and a matching wrapper
result. The middle record is the semantic source. Rendering all three duplicates the action and
leaks JavaScript implementation detail.

Check whether the same pattern exists in older transcript versions. Codex 0.144.6 had the wrapper
but not the semantic mirror, so suppressing wrappers unconditionally erased real commands. The
adapter therefore enables semantic-event normalization only for the newer schema selected from
`session_meta.payload.cli_version`.

### 3. Build a minimal Claude-shaped reference

Make the smallest JSONL transcript that expresses the intended actions using Claude's native tool
vocabulary. Synthetic records are easier to reason about than a large real session and contain no
private session data. A command plus edit reference can be as small as:

```jsonl
{"type":"user","cwd":"/repo","timestamp":"2026-08-18T22:00:00Z","message":{"role":"user","content":"make the change"}}
{"type":"assistant","cwd":"/repo","timestamp":"2026-08-18T22:00:01Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"git diff --check"}}]}}
{"type":"user","timestamp":"2026-08-18T22:00:02Z","toolUseResult":{"stdout":"","stderr":"","interrupted":false},"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"b1","content":""}]}}
{"type":"assistant","cwd":"/repo","timestamp":"2026-08-18T22:00:03Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"e1","name":"Edit","input":{"file_path":"/repo/README.md","old_string":"old","new_string":"new"}}]}}
{"type":"user","timestamp":"2026-08-18T22:00:04Z","toolUseResult":{"filePath":"/repo/README.md","structuredPatch":[{"oldStart":1,"oldLines":1,"newStart":1,"newLines":1,"lines":["-old","+new"]}]},"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"e1","content":"The file has been updated successfully."}]}}
```

Add one focused pair per behavior. Existing executable examples live in the Claude adapter tests,
especially `claude-replay-agents/src/agents/claude/model.rs` and
`claude-replay-agents/tests/engine_integration.rs`; copy their measured field shapes rather than
inventing a plausible schema.

Build one target-agent fixture with equivalent actions. Initially it can be a short, frozen slice
of a real transcript. Once the raw schema is understood, reduce it to a synthetic fixture that
keeps only the records necessary to reproduce the behavior.

### 4. Render both sides through exactly the same pipeline

Build once, disable the durable cache, fix the width, and capture both default and fully expanded
views:

```sh
cargo build --release --bin claude-replay
BIN=target/release/claude-replay

"$BIN" "$REFERENCE" --no-cache --dump - --width 100 > reference.default.txt
"$BIN" "$TARGET"    --no-cache --dump - --width 100 > target.default.txt
"$BIN" "$REFERENCE" --no-cache --dump - --width 100 --full > reference.full.txt
"$BIN" "$TARGET"    --no-cache --dump - --width 100 --full > target.full.txt

diff -u reference.default.txt target.default.txt
diff -u reference.full.txt target.full.txt
```

The files need not be byte-identical: timestamps, prose, paths, and agent metadata legitimately
differ. Review the action boundaries, names, targets, output, ordering, and duplication. The full
view proves the data survived; the default view proves shared folding and summaries classify it
the same way.

Also render HTML with the same binary when the change affects presentation metadata or browser
behavior:

```sh
"$BIN" "$TARGET" --no-cache --dump-html /tmp/target-adapter
open /tmp/target-adapter.html
```

`--no-cache` is essential during iteration. If canonical folded output changes, bump
`FOLD_VERSION` so already-cached sessions cannot splice old and new block shapes.

For Codex `CommandExecution`, keep the real event envelope and replace all correlated fields with
sentinels: the command string, every `parsed_cmd` entry, aggregate output, status, exit code, and
`duration.secs`/`duration.nanos`. Default output should classify a command whose complete
`parsed_cmd` list consists of `read`/`search`/`list_files` as a shared activity span; `--full`
must retain every target plus the composite command/output. One unknown parsed action is the
negative control: the whole command remains `Bash`, because partially classifying a compound
command would hide work. Status, exit code, and duration are canonical `ToolExecution` metadata,
not strings injected into the adapter's output.

### 5. Normalize only in the target adapter

The shared engine and presenters should receive the vocabulary they already understand. For an
adapter this usually means:

- translate semantic native events into canonical `Message`s;
- preserve ids long enough to join calls and results;
- suppress a wrapper only when its semantic replacement is known to exist;
- serialize any state needed across durable-cache resume boundaries;
- version-gate materially different source schemas;
- leave unknown/new records visible or harmless rather than silently discarding them.

Do not teach the HTML or TUI that `CommandExecution` means `Bash`. If a second frontend would need
the same condition, it belongs in the adapter.

### When the shared vocabulary is genuinely missing a fact

Sometimes normalization cannot be lossless because no canonical field exists. Extend the shared
vocabulary when the fact is persisted, agent-neutral, and useful to more than one frontend:

- Assistant progress/final distinctions become structured assistant phase data. Codex normalizes
  `commentary` and `final_answer`; the TUI and HTML independently choose quieter progress styling.
- Command outcome and elapsed time become `ToolExecution { status, exit_code, duration }` on a
  tool block. Presenters format the same structure as terminal text or an HTML chip.
- Context usage, limits, and execution settings are a latest session runtime snapshot, not fake
  timeline blocks. The adapter folds `turn_context`, `thread_settings_applied`, and `token_count`
  records; the TUI footer and HTML Runtime panel expose the shared snapshot.

Do not invent historical blocks for status-bar state. Preserve the most recent persisted snapshot
and label it as runtime/session metadata. Keep absence distinct from zero, success, or a default
setting. Include the new private accumulator fields in its cursor state, and bump `FOLD_VERSION`
when either canonical blocks or cached metrics output changes.

The focused synthetic controls for these additions should cover:

```text
assistant commentary -> structured Commentary -> quiet • / phase-commentary
assistant final_answer -> structured Final -> strong ⏺ / phase-final
failed CommandExecution + exit 7 + 42ms -> one failure header chip/suffix
50k input in a 200k context -> 75% context left
7-day limit at 12.5% -> typed window with reset timestamp retained
```

### 6. Turn the discovery into tests

Use several layers, each with a different job:

1. **Adapter unit fixture:** a minimal native transcript produces the intended canonical blocks.
2. **Negative/version fixture:** a superficially similar older schema retains its original data.
3. **Resume/follow test:** adapter state survives a split between wrapper, semantic event, and
   result; incremental output equals a cold parse.
4. **Same-renderer comparison:** synthetic Claude reference and target fixture have equivalent
   visible actions under default and `--full` dumps.
5. **Real-transcript smoke test:** exercise one frozen real session to catch unmodeled record
   combinations.
6. **Repository byte gate:** verify all pre-existing frozen sessions and HTML/bundle outputs; review
   intentional diffs line by line before rebaselining.

The standard gates remain:

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
scripts/gate/gate.sh            # must print BYTE-IDENTICAL: PASS
```

Keep real transcripts outside git. The byte gate's frozen inputs live under `$SC_GATE_DIR`
(default `~/.cache/claude-replay-gate`) for that reason.

## Second oracle: a format-preserving native Codex capture

For Codex, strengthen the same-renderer comparison with the source client's own TUI. Start from a
**real Codex session shape** and substitute distinctive synthetic values into its content fields.
Do not invent a plausible rollout: keeping the real envelopes, ordering, ids, lifecycle, and
duplicate mirrors is the point of this test.

Codex 0.147 has an important complication. `codex resume` does not reconstruct the display from
the rollout JSONL alone. It joins at least these parts of `CODEX_HOME`:

- `sessions/.../rollout-...jsonl` — raw response, event, wrapper, and receipt records;
- `state_5.sqlite` — the thread row and `rollout_path`;
- `thread_history_1.sqlite` — projected turns/items and byte offsets into the rollout.

A JSONL copied into the ordinary sessions directory may appear to work while silently omitting
command and file-change cards. That is not evidence that the native TUI hides them; it means the
fixture is missing or disagrees with its projection.

Use this procedure:

1. Freeze the real rollout while the source session is idle. Create a mode-0700 temporary
   `CODEX_HOME`, copy the rollout into its `sessions/` layout, and use SQLite's `.backup` command
   to snapshot `state_5.sqlite` and `thread_history_1.sqlite` consistently. Copy authentication
   only if the installed client requires it to start, never include it in a capture, and delete
   the entire temporary home afterward.
2. Keep the original thread UUID inside the isolated home, or replace **every** identity carrier:
   `session_meta.payload.id`, `session_meta.payload.session_id`, each event's
   `payload.thread_id`, the state row, and the history tables. Changing only one id can resume the
   original thread or hydrate the wrong cwd.
3. Substitute synthetic values without changing record kinds, call counts, ids, or ordering. For
   a tool action, rewrite all correlated copies consistently: the JavaScript wrapper, semantic
   `item_completed` event, wrapper receipt, and projected `thread_items.item_json`. A command's
   raw cwd remains a `file://` value when that is what the real schema used. Preserve array versus
   string result shapes.
4. If replacement changes line lengths, recompute `thread_turns.rollout_byte_offset`,
   `rollout_end_byte_offset`, and `thread_history_projection_state.next_rollout_byte_offset` from
   the rewritten JSONL. A stale offset can make Codex reject the thread or page unrelated bytes.
   A byte-length-preserving sanitizer avoids this bookkeeping.
5. Start the client with the isolated environment in a private, fixed-size terminal:

   ```sh
   tmux -L codex-oracle new-session -d -x 140 -y 50 \
     "env CODEX_HOME=$SANDBOX_CODEX_HOME codex resume $THREAD_ID --no-alt-screen"
   tmux -L codex-oracle capture-pane -p -e -S -4000
   ```

   Do not submit a prompt. Capture the initial `--no-alt-screen` scrollback, then open Codex's
   transcript view with `Ctrl-T`, jump to the top, and capture that too. The two surfaces together
   expose expanded command/file output, messages, reasoning summaries, and web activity.
6. Give every visible field a unique sentinel and make an inventory before comparing, for example
   `SYNTHETIC COMMAND OUTPUT`, `SYNTHETIC OLD LINE`, `SYNTHETIC PLAN EXPLANATION`, and
   `SYNTHETIC WEB QUERY`. Render the rewritten rollout with `claude-replay --no-cache --full` and
   assert that every native-visible sentinel appears once in the corresponding canonical action,
   with no JavaScript transport duplicate.
7. Kill only the private tmux server and remove the exact temporary home. Never point the native
   client at a rewritten file under the real `CODEX_HOME`.

The Codex 0.147 experiment reproduced synthetic user, assistant, reasoning, and web content from
the isolated state-backed clone, and reproduced command/file cells when all original correlated
records were retained. It also established two useful failure controls: a JSONL-only clone loses
tool cells, and content edits with stale projection offsets can make the thread disappear.

One class needs a different native oracle: `Updated Plan` is delivered to the TUI as a live
`TurnPlanUpdated` notification and is not projected as a replayable `ThreadItem` in these stores.
A resume fixture can validate that the adapter retains the persisted `update_plan` wrapper, but it
cannot prove the native live cell. For plan presentation, use a disposable live app-server
notification fixture or the native renderer's focused test as the Codex-side oracle, then use the
same-renderer reference to verify the adapter's `TodoWrite` block.

A private tmux server gives the client a real terminal without sharing the developer's current
session, and a fixed pane size makes wrapping reproducible. Treat native captures as evidence about
labels, grouping, ordering, and visibility—not as an everlasting byte oracle. Native clients and
their state schemas change. During this work Claude Code could not run on the host, so
`claude-replay`'s rendering of the equivalent Claude vocabulary remained the best approximation of
the Claude side; native Codex was the source-side visibility oracle.

## Review checklist

- Does one user-visible action render exactly once?
- Is the most semantic source record used instead of a model-facing wrapper?
- Does `--full` retain command text, cwd, output/error, diff, URL/query, and status?
- Does the default view use the same summary/fold behavior as the Claude-shaped reference?
- Are unrelated and unknown events still visible?
- Is a schema/version boundary explicit and covered on both sides?
- Is preprocessor/join state serialized for durable resume?
- Was `FOLD_VERSION` bumped if canonical output changed?
- Do TUI, HTML, `claude-monitor`, and dumps all improve without agent branches?
- Are frozen real fixtures private and stable during the gate?

## Where this can improve next

The current method intentionally leaves human judgment in the semantic diff. Good follow-ups are:

- checked-in privacy-safe microfixtures for each canonical action and each agent version;
- a structural comparator that ignores timestamps/prose but compares block kind, target, result,
  and ordering;
- a fixture generator that emits equivalent Claude/agent-native pairs from one small scenario
  description;
- optional native-client capture drivers behind a common private-tmux interface;
- a version matrix that makes unsupported schema transitions fail visibly instead of falling back
  by accident.

Those additions should automate this playbook, not weaken its central invariant: presenters stay
agent-neutral, and adapters translate native records into one shared semantic vocabulary.
