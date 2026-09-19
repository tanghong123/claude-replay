#!/usr/bin/env node
// A sanitised demo corpus for the viewer, and a driver that replays it live (#248).
//
// WHY THIS IS A GENERATOR AND NOT A TRANSCRIPT. Anything committed here is public, and
// `.gitignore` catches `*.jsonl` but cannot catch a demo page that embeds one — a page carrying
// a real prompt timeline reached review that way once already. So the corpus is DERIVED: this
// script is the artefact, every word in it is invented, and a reviewer confirms the corpus is
// safe by reading this file rather than by auditing generated output.
//
// The SHAPE is taken from real sessions — turn lengths, how often a turn thinks, the ratio of
// calls to prose, how deep result bodies run — while the CONTENT is a fictional project:
// `lumen`, a link checker for static sites. No real path, host, person or repository appears.
//
// Usage:
//   node scripts/demo-corpus.mjs --out <dir>              write the whole session at once
//   node scripts/demo-corpus.mjs --out <dir> --live       append it on a timer (the tailing demo)
//   node scripts/demo-corpus.mjs --out <dir> --live --speed 4   …four times faster
//
// `--live` is the half the video needs: the page follows a growing file, so a recording needs a
// writer that appends while it runs. It writes the same records in the same order, one scene at
// a time, pausing between them.

import { mkdirSync, writeFileSync, appendFileSync, rmSync } from "node:fs";
import { join } from "node:path";

const args = process.argv.slice(2);
const flag = n => args.includes(n);
const value = (n, d) => { const i = args.indexOf(n); return i >= 0 && args[i + 1] ? args[i + 1] : d; };
const OUT = value("--out", null);
const LIVE = flag("--live");
const SPEED = Number(value("--speed", "1")) || 1;
const SID = "0d3f7a91-2c45-4e18-9b6a-7f2e5c81d430";
const CWD = "/home/dev/lumen";

if (!OUT) { console.error("usage: demo-corpus.mjs --out <dir> [--live] [--speed N]"); process.exit(2); }

// ── the clock ────────────────────────────────────────────────────────────────────────────────
// A fixed start so a regenerated corpus is byte-identical, unless `--live`, where the records
// have to look recent or the session sorts into the Idle bucket and the shell's default filter
// hides it.
let t = LIVE ? Date.now() - 20 * 60_000 : Date.parse("2026-04-07T09:12:00.000Z");
const at = (secs = 0) => { t += secs * 1000; return new Date(t).toISOString(); };

let uid = 0;
const rec = o => ({ uuid: `u${String(++uid).padStart(4, "0")}`, sessionId: SID, cwd: CWD, ...o });

const user = (text, secs = 0) => rec({ type: "user", timestamp: at(secs), message: { role: "user", content: [{ type: "text", text }] } });
const assistant = (text, secs = 0, usage) => rec({ type: "assistant", timestamp: at(secs), message: { role: "assistant", content: [{ type: "text", text }], ...(usage ? { usage, model: "claude-opus-5" } : {}) } });
const thinking = (text, secs = 0) => rec({ type: "assistant", timestamp: at(secs), message: { role: "assistant", content: [{ type: "thinking", thinking: text }] } });
const call = (id, name, input, secs = 0) => rec({ type: "assistant", timestamp: at(secs), message: { role: "assistant", content: [{ type: "tool_use", id, name, input }] } });
const result = (id, content, secs = 0, extra = {}) => rec({ type: "user", timestamp: at(secs), message: { role: "user", content: [{ type: "tool_result", tool_use_id: id, content, ...extra }] } });
const turnDuration = (ms, secs = 0) => rec({ type: "system", subtype: "turn_duration", durationMs: ms, messageCount: 6, timestamp: at(secs) });
const attachment = (a, secs = 0) => rec({ type: "attachment", timestamp: at(secs), attachment: a });
const compaction = (pre, post, secs = 0) => rec({ type: "system", subtype: "compact_boundary", timestamp: at(secs), content: "Conversation compacted", compactMetadata: { trigger: "auto", preTokens: pre, postTokens: post } });
// The record carries an `error` OBJECT — a `content` string is not read (#236), which is easy
// to get wrong and shows up as a scene that silently renders nothing.
const apiError = (message, status, attempt, maxRetries, secs = 0) => rec({ type: "system", subtype: "api_error", timestamp: at(secs), error: { message, status }, retryAttempt: attempt, maxRetries });
const queueOp = (operation, content, secs = 0, reason) => rec({ type: "queue-operation", operation, timestamp: at(secs), ...(content ? { content } : {}), ...(reason ? { reason } : {}) });

// A 1x1 PNG. The demo needs an image the page can actually decode; it carries no picture.
const TINY_PNG = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
const imageResult = (id, secs = 0) => rec({ type: "user", timestamp: at(secs), message: { role: "user", content: [{ type: "tool_result", tool_use_id: id, content: [{ type: "text", text: "screenshot captured" }, { type: "image", source: { type: "base64", media_type: "image/png", data: TINY_PNG } }] }] } });

// ── the scenes ───────────────────────────────────────────────────────────────────────────────
// One per feature worth showing. Each returns its records; the driver writes them in order and,
// under `--live`, pauses between scenes so the page is seen to grow.

const scenes = [];
const scene = (name, build) => scenes.push({ name, build });

scene("a prose answer with a table, a fence and CJK", () => [
  user("Summarise what the crawler does with redirects, and show the status table.", 0),
  thinking("Worth answering with the table from the README rather than prose alone.", 8),
  assistant([
    "`lumen` follows a redirect chain up to **five** hops and reports the LAST status it saw.",
    "",
    "| status | meaning | counted as |",
    "| --- | --- | --- |",
    "| 200 | reachable | ok |",
    "| 301/302 | redirected | ok, with a note |",
    "| 404 | missing | broken |",
    "| 429 | rate limited | retried, then skipped |",
    "",
    "The chain stops early when a hop repeats:",
    "",
    "```rust",
    "if seen.contains(&next) {",
    "    return Err(Redirect::Loop { at: next });",
    "}",
    "```",
    "",
    "文档里这一段写得不清楚,所以下面用一句话概括:重定向只记最后一跳的状态。",
  ].join("\n"), 26, { input_tokens: 4120, output_tokens: 890, cache_read_input_tokens: 38000 }),
  turnDuration(41_000, 1),
]);

scene("a run of calls, one of which fails with an exit code", () => {
  const out = [user("Run the link check over the docs directory.", 40)];
  out.push(call("c-ls", "Bash", { command: "ls docs" }, 3));
  out.push(result("c-ls", "index.md\nguide.md\nchangelog.md", 2));
  out.push(call("c-run", "Bash", { command: "lumen check docs --format json" }, 2));
  out.push(result("c-run", "checked 3 files, 118 links, 2 broken\n  guide.md:41  https://example.invalid/spec  404\n  guide.md:77  https://example.invalid/old  404", 9));
  out.push(call("c-fail", "Bash", { command: "lumen check docs --strict" }, 3));
  out.push(result("c-fail", "error: 2 broken links and --strict was given", 4, { is_error: true }));
  out.push(assistant("Two links are dead. `--strict` turns that into a non-zero exit, which is what CI uses.", 6));
  out.push(turnDuration(29_000, 1));
  return out;
});

scene("an Edit with a diff, and a Write", () => [
  user("Fix both links — they moved to /spec/v2 and /archive.", 30),
  call("c-edit", "Edit", { file_path: `${CWD}/docs/guide.md`, old_string: "https://example.invalid/spec", new_string: "https://example.invalid/spec/v2" }, 4),
  result("c-edit", "The file docs/guide.md has been updated.", 3),
  call("c-write", "Write", { file_path: `${CWD}/docs/redirects.toml`, content: "# links that moved, kept so the checker can explain a 301\n[\"/spec\"]\nto = \"/spec/v2\"\n\n[\"/old\"]\nto = \"/archive\"\n" }, 3),
  result("c-write", "File created successfully at docs/redirects.toml", 2),
  assistant("Both updated, and the moves are recorded in `docs/redirects.toml` so the checker can explain them rather than just passing.", 5),
  turnDuration(19_000, 1),
]);

scene("a Read with an offset and a limit", () => [
  user("Show me just the retry block in the fetcher.", 25),
  call("c-read", "Read", { file_path: `${CWD}/src/fetch.rs`, offset: 118, limit: 14 }, 3),
  result("c-read", ["   118\t    let mut backoff = Duration::from_millis(250);",
    "   119\t    for attempt in 0..RETRIES {",
    "   120\t        match head(url).await {",
    "   121\t            Ok(r) if r.status() != 429 => return Ok(r),",
    "   122\t            Ok(_) => tokio::time::sleep(backoff).await,",
    "   123\t            Err(e) if attempt + 1 == RETRIES => return Err(e.into()),",
    "   124\t            Err(_) => tokio::time::sleep(backoff).await,",
    "   125\t        }",
    "   126\t        backoff *= 2;",
    "   127\t    }",
    "   128\t    Err(Fetch::Exhausted { url: url.clone() })",
    "   129\t}"].join("\n"), 2),
  assistant("Five attempts, doubling from 250ms. A 429 is the only status that retries; everything else returns on the first answer.", 5),
  turnDuration(11_000, 1),
]);

scene("a screenshot taken in the middle of a run", () => [
  user("Open the report page and tell me if the summary line wraps.", 35),
  call("c-nav", "mcp__claude-in-chrome__navigate", { url: "http://localhost:4173/report.html" }, 3),
  result("c-nav", "navigated", 3),
  call("c-shot", "mcp__claude-in-chrome__computer", { action: "screenshot" }, 2),
  imageResult("c-shot", 4),
  call("c-width", "Bash", { command: "lumen report --width 72 --dry-run" }, 3),
  result("c-width", "summary fits in 68 columns", 2),
  assistant("It does not wrap at 72 columns — the summary line measures 68.", 5),
  turnDuration(22_000, 1),
]);

scene("a /context report", () => [
  rec({ type: "system", subtype: "local_command", timestamp: at(30), content: "<command-name>/context</command-name>\n<command-message>context</command-message>\n<command-args></command-args>" }),
  rec({ type: "system", subtype: "local_command", timestamp: at(1), content: "<local-command-stdout> Context Usage\n⛁ ⛶   Opus 5 (1M context)\n⛶ ⛶   142.8k/1m tokens (14%)\n⛶ ⛶   ⛁ Messages: 96.4k tokens (9.6%)\n⛶ ⛶   ⛶ Free space: 810.3k (81.0%)</local-command-stdout>" }),
]);

scene("an API error, and a hook that failed", () => [
  user("Re-run the whole check.", 20),
  call("c-again", "Bash", { command: "lumen check docs" }, 3),
  apiError("upstream connect error or disconnect/reset before headers", 500, 1, 3, 4),
  result("c-again", "checked 3 files, 118 links, 0 broken", 8),
  attachment({ type: "hook_non_blocking_error", command: "lumen fmt --check", exitCode: 1, stderr: "docs/redirects.toml is not formatted" }, 1),
  assistant("Clean now. The formatting hook failed separately — `docs/redirects.toml` needs `lumen fmt`.", 5),
  turnDuration(24_000, 1),
]);

scene("a question put to the reader, with the options it offered", () => [
  user("Decide how the checker should treat a 429.", 30),
  call("c-ask", "AskUserQuestion", {
    questions: [
      { header: "Rate limits", question: "How should a 429 be counted?", multiSelect: false,
        options: [
          { label: "Retry then skip (Recommended)", description: "Five attempts, then leave the link unjudged rather than calling it broken." },
          { label: "Treat as broken", description: "Strict, but a slow host would fail the build." },
          { label: "Ignore entirely", description: "Never report it; the quietest option and the least honest." }] },
      { header: "Reporting", question: "What should the summary show?", multiSelect: true,
        options: [
          { label: "A skipped count", description: "One number beside broken and ok." },
          { label: "The hosts that limited us", description: "Names each host once, with a count." },
          { label: "Nothing", description: "Keep the summary to two numbers." }] },
    ],
  }, 3),
  result("c-ask", "Your questions have been answered: \"How should a 429 be count\"=\"Retry then skip (Recommended)\" \"What should the summary sh\"=\"A skipped count, The hosts that limited us\". You can now continue.", 46),
  assistant("Retry then skip, and the summary will carry a skipped count plus the hosts that limited us.", 6),
  turnDuration(58_000, 1),
]);

scene("a queued prompt, picked up mid-turn", () => [
  user("Add the skipped count to the summary.", 25),
  call("c-sum", "Edit", { file_path: `${CWD}/src/report.rs`, old_string: "broken: usize,", new_string: "broken: usize,\n    skipped: usize," }, 4),
  queueOp("enqueue", "and please keep the column order stable", 6),
  result("c-sum", "The file src/report.rs has been updated.", 3),
  queueOp("remove", "and please keep the column order stable", 2, "absorbed_mid_turn"),
  user("and please keep the column order stable", 1),
  assistant("Added, and the column order is fixed by the struct's field order so it cannot drift.", 7),
  turnDuration(31_000, 1),
]);

scene("a compaction, and the files it carried over", () => [
  compaction(612_440, 9_180, 20),
  attachment({ type: "compact_file_reference", filename: `${CWD}/src/report.rs`, displayPath: "src/report.rs" }, 1),
  attachment({ type: "compact_file_reference", filename: `${CWD}/src/fetch.rs`, displayPath: "src/fetch.rs" }, 0),
  attachment({ type: "compact_file_reference", filename: `${CWD}/docs/redirects.toml`, displayPath: "docs/redirects.toml" }, 0),
  assistant("Carrying on from the summary: the skipped count is in, and the formatting hook still needs a run.", 6),
]);

scene("tasks moving through the queue", () => [
  user("Log what is left as tasks.", 22),
  call("c-task1", "Bash", { command: "taskq create --subject 'Run lumen fmt over docs'" }, 3),
  result("c-task1", "Task #4 created successfully: Run lumen fmt over docs\n##taskq/v1 {\"rid\":\"demo-0001\",\"ts\":\"2026-04-07T10:02:00Z\",\"repo\":\"lumen\",\"op\":\"create\",\"kind\":\"content\",\"task\":\"4\",\"subject\":\"Run lumen fmt over docs\",\"changes\":{\"status\":{\"from\":null,\"to\":\"pending\"}}}", 3),
  call("c-task2", "Bash", { command: "taskq done 4 --outcome 'formatted'" }, 4),
  result("c-task2", "Completed task #4: Run lumen fmt over docs\n##taskq/v1 {\"rid\":\"demo-0002\",\"ts\":\"2026-04-07T10:03:00Z\",\"repo\":\"lumen\",\"op\":\"done\",\"kind\":\"state\",\"task\":\"4\",\"subject\":\"Run lumen fmt over docs\",\"changes\":{\"status\":{\"from\":\"pending\",\"to\":\"completed\"}}}", 3),
  assistant("One task, opened and closed — it shows in the task panel with both states.", 5),
  turnDuration(18_000, 1),
]);

scene("a sub-agent, spawned and finished", () => [
  user("Have a sub-agent audit the retry constants.", 20),
  call("c-agent", "Agent", { subagent_type: "general-purpose", description: "Audit retry constants", prompt: "Check RETRIES and the backoff base against the docs." }, 3),
  result("c-agent", "Agent started (id a-retry-audit)", 2),
  queueOp("enqueue", "<task-notification>\n<task-id>a-retry-audit</task-id>\n<tool-use-id>c-agent</tool-use-id>\n<summary>Agent \"Audit retry constants\" finished</summary>\n<status>completed</status>\n<result>RETRIES is 5 in code and 3 in the docs; the docs are wrong.</result>\n</task-notification>", 52),
  assistant("The sub-agent found the docs claim three attempts where the code does five. The code is right.", 6),
  turnDuration(66_000, 1),
]);

// #241: a workflow run's roster lives BESIDE the session, in
// `<sid>/subagents/workflows/<run>/journal.jsonl`, not in the transcript. The call below starts
// it; `writeRun` writes the journal the page reads the phases from.
const RUN = "wf-linkcheck";
const runJournal = [
  { type: "launched" },
  { type: "started", agentId: "a-crawl-docs", label: "crawl:docs", phase: "Crawl" },
  { type: "started", agentId: "a-crawl-blog", label: "crawl:blog", phase: "Crawl" },
  { type: "started", agentId: "a-verify-docs", label: "verify:docs", phase: "Verify" },
  { type: "started", agentId: "a-tidy", label: "tidy", phase: "" },
  { type: "result", agentId: "a-crawl-docs", result: "118 links, 2 broken" },
  { type: "result", agentId: "a-crawl-blog", result: "64 links, 0 broken" },
  { type: "result", agentId: "a-verify-docs", result: "both confirmed dead" },
];

scene("a workflow run, grouped by its phases", () => [
  user("Fan the check out over docs and the blog.", 25),
  call("c-wf", "Workflow", { script: "export const meta = { name: 'linkcheck', phases: [{ title: 'Crawl' }, { title: 'Verify' }] }" }, 4),
  result("c-wf", `Workflow launched in background. Task ID: t-${RUN}\nTranscript dir: ${CWD}/.claude/runs/${RUN}\n`, 3),
  assistant("Four agents: two crawling in parallel, one verifying what they found, and a tidy pass outside the phases.", 8),
  turnDuration(40_000, 1),
]);

scene("the client's own cost, and a prompt still waiting", () => [
  // #240: the CLI's own running tally. One epoch per CLI process, keyed by `startTime`; the
  // page shows it beside the figure the viewer computes from the tokens.
  rec({ type: "cost-state", timestamp: at(15), totalCostUSD: 1.8342, totalAPIDuration: 214_500,
        totalAPIDurationWithoutRetries: 211_900, totalToolDuration: 96_400, totalLinesAdded: 34,
        totalLinesRemoved: 6, totalDuration: 1_284_000, startTime: Date.parse("2026-04-07T09:12:00.000Z"),
        modelUsage: { "claude-opus-5[1m]": { inputTokens: 28_400, outputTokens: 6_120, costUSD: 1.8342 } } }),
  user("What did this session cost?", 4),
  assistant("About $1.83 by the client's own tally — the panel shows that beside the figure derived from the tokens, so the two can be compared rather than guessed at.", 7),
  turnDuration(11_000, 1),
  // …and one the agent has not picked up, so the ⧗ marker stays on the page. The pickup case is
  // in an earlier scene; this is the other half of #165, the prompt still in flight.
  queueOp("enqueue", "after this, check the changelog links too", 9),
]);


// ── the driver ───────────────────────────────────────────────────────────────────────────────
// One shot writes everything and exits. `--live` writes the first scene, then appends the rest
// on a timer — which is what the page's follower needs to be SEEN following. The gaps come from
// the scene's own timestamps, compressed by `--speed`, so the replay keeps the session's real
// rhythm instead of a fixed tick.

const sleep = ms => new Promise(r => setTimeout(r, ms));
const line = r => JSON.stringify(r) + "\n";

async function main() {
  mkdirSync(OUT, { recursive: true });
  const path = join(OUT, `${SID}.jsonl`);
  rmSync(path, { force: true });
  // The run's roster, beside the session rather than inside it (#241).
  const runDir = join(OUT, SID, "subagents", "workflows", RUN);
  mkdirSync(runDir, { recursive: true });
  writeFileSync(join(runDir, "journal.jsonl"), runJournal.map(line).join(""));

  if (!LIVE) {
    const all = scenes.flatMap(s => s.build());
    writeFileSync(path, all.map(line).join(""));
    console.log(`wrote ${all.length} records to ${path}`);
    console.log(`open with: agent-replay ${path}`);
    return;
  }

  console.log(`live: appending ${scenes.length} scenes to ${path} at ${SPEED}x`);
  console.log(`follow with: agent-replay ${path} --html    (or open it in agent-monitor)`);
  for (const [i, s] of scenes.entries()) {
    const before = t;
    const records = s.build();
    // The scene's own span, compressed — a scene that took 40 transcript-seconds waits 40/SPEED
    // real ones, so the recording moves at a watchable pace without losing the shape.
    const span = Math.max(1, Math.round((t - before) / 1000 / SPEED));
    appendFileSync(path, records.map(line).join(""));
    console.log(`  [${i + 1}/${scenes.length}] ${s.name} — ${records.length} records, next in ${span}s`);
    if (i < scenes.length - 1) await sleep(span * 1000);
  }
  console.log("live replay finished; the file is complete.");
}

main().catch(e => { console.error(e); process.exit(1); });
