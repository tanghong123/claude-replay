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

## Quoted user replies

**Status: proposed validation case; no parser or renderer change is claimed here.** Use the
[annotation-reply RFC's acceptance contract](../design/monitor-response-annotations.md#when-special-presentation-is-allowed)
and [additive presentation contract](../design/monitor-response-annotations.md#parse-once-add-presentation-without-breaking-existing-readers)
as the expected behavior. This case study supplies synthetic source examples and a validation
recipe; recognition rules, shared interfaces, and delivery scope belong in that RFC.

### Observed clients and synthetic input

The inspected clients are Codex App `26.1007.21159` (build `20052`) and Claude Desktop
`2.31226.1`. The inspected Claude Code session records version `2.1.295` and
`entrypoint: "claude-desktop"`, identifying a Claude Desktop Code-session envelope. A separate
Claude Code CLI capture has not been verified for this envelope.

The following examples substitute synthetic prose into the observed envelopes. They show the
text extracted from native user-input records, not complete JSONL records or real conversations.
The message id and offsets are also synthetic; no real assistant message is referenced. When
building fixtures, retain each native record's measured envelope and put these synthetic strings
in its user-input text field. Keep real captures outside git as in the workflow above.

The Codex example begins with one LF before `# Response annotations:` and uses the observed
compact, single-line array. The fence delimiters are not input:

```text

# Response annotations:
Each item contains text selected from an earlier Codex response and may include a user comment. Treat items as Annotation 1, Annotation 2, and so on in array order. Use every selection as context and address every comment. For every annotation you address, include its inline directive `:codex-annotation{index="N"}`, where N is its one-based array position (for example, `:codex-annotation{index="1"}`). Do not use unstructured annotation labels.
<response-annotations>
[{"text":"This is the first excerpt from Codex's response.","source":{"messageId":"msg_synthetic_response","startOffset":100,"endOffset":146}},{"text":"This is the second excerpt from Codex's response.","annotation":"Please clarify this point."}]
</response-annotations>

## My request:
Please provide a detailed example covering both excerpts.
```

For reading only, the same array pretty-printed is:

```json
[
  {
    "text": "This is the first excerpt from Codex's response.",
    "source": {
      "messageId": "msg_synthetic_response",
      "startOffset": 100,
      "endOffset": 146
    }
  },
  {
    "text": "This is the second excerpt from Codex's response.",
    "annotation": "Please clarify this point."
  }
]
```

Keep the compact fixture distinct from this readable copy. Replacing the compact array with the
pretty copy is a focused mutation test whose expected outcome comes from the acceptance contract.

The first observed Claude layout has one `reply 1` block:

```text
<!-- reply 1 -->
> This is the first excerpt from Claude Code's response.

Please explain this point.
```

The other observed layout begins with `reply 2` and has a second, bare `reply` marker:

```text
<!-- reply 2 -->
> This is the first excerpt from Claude Code's response.

Please explain this point.

<!-- reply -->
> This is the second excerpt from Claude Code's response.

Please clarify what this means.


Please provide a detailed example covering both excerpts.
```

Preserve the sample's two LF before the bare marker and three LF before the final sentence when
constructing the fixture. Assert the final sentence's mapping using the RFC; do not let a fixture
writer's interpretation of its prose erase the spacing or separate it into another source record.

This additional quote-only control is **proposed synthetic input**, not a verified native Claude
capture. The JSON string makes the two LF before EOF explicit; expected separator handling and
shared output are specified in the RFC's acceptance contract:

```json
"<!-- reply 1 -->\n> This is the first excerpt from Claude Code's response.\n\n"
```

### Validation recipe

Start with a synthetic Codex native record and a synthetic Claude native record for the same
quoted-reply case. Feed both through their Rust adapters, then compare the shared `Message`/`Block`
facts before examining rendered output. Compare the facts both source formats encode, preserving
any distinctions required by the RFC. Check quote text, associated text, ordering, turn count,
and preserved context against that contract. This prevents a convincing screenshot from concealing an
incorrect parser result. Use a single binary and fixed options for the existing default/`--full`
dump comparison, then inspect HTML wire records and the applicable browser views separately.

For preservation checks, retain the fixture's original user-input bytes before Codex
`desktop_prompt` or Claude `strip_caveat` cleanup. Compare raw display against those bytes; current
`src` alone is not an independent oracle because it contains the text that reached the shared
block. Add sentinel text, files, and images around the quote envelope and request, then check that
all expected content remains visible exactly once in the renderer and its readable fallback.

Generate negative controls by mutating one aspect of an example at a time. Derive expected
classification and fallback from the RFC rather than making the test parser define its own rules:

| Control group | What to inspect |
|---|---|
| Codex and Claude source examples; proposed quote-only input | Shared facts, quote/associated-text order, one turn, and byte-preserved following text |
| Changed preamble, tags, marker numbers, prefixes, separators, case, or LF/CRLF boundaries | Acceptance decision and complete ordinary-input fallback against the RFC |
| Pretty/reordered JSON, alternate escapes, unknown fields, empty present values, invalid source, or malformed array | Contract-defined classification; distinguish absent optional fields from invalid present values |
| Ordinary Markdown quotes, marker-like prose, quoted/fenced markers, and JSON strings containing an envelope | No unintended conversion or recursive recognition |
| Multiline code/tables, repeated quotes, source offsets, and all request/context/attachment combinations | Selected text comes from the fixture, with preserved order and no hidden or duplicated content |
| An exact hand-typed copy of an envelope | Presentation and any provenance claim agree with the RFC's stated text-only limitation |

Finally, exercise regressions through the same new wire record with the adapted view and the
unchanged consumers identified by the RFC. Include old records with no metadata and invalid or
unsupported metadata; check readable body fallback, raw display, search, copying, and Turns
navigation. Test the existing TUI/dump path for content preservation without assuming that every
consumer receives new styling.

Compare a cold parse with incremental parsing at meaningful envelope boundaries, then with durable
resume and a pre-change cache. Inspect whether the required `FOLD_VERSION` and HTML
`RECORD_SCHEMA` boundaries rebuild incompatible output under the RFC's additive contract. Use
`--no-cache` for initial comparisons so stale rendered records cannot produce a false result;
re-enable caching for the explicit regression controls. Run the repository gates and review any
intentional baseline changes as in the workflow above. These are implementation checks to perform,
not checks reported as passed by this proposed case study.

`--unknown` inventories record/content types and keys; it does not detect changed prose inside a
known user-text field. Include a preamble/marker mutation that leaves the surrounding record shape
unchanged, and inspect adapter output and full fallback directly instead of treating an unchanged
`--unknown` report as evidence that the quote recognizer still works.

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
