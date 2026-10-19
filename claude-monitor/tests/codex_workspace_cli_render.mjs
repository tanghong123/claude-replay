import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

/** Compare independent Claude/Codex transcripts with the same renderer/options.
 * The caller must build the exact reviewed binary before invoking this probe.
 */
export function assertCanonicalRendering(binary) {
  const fixtures = join(dirname(fileURLToPath(import.meta.url)), "fixtures/codex-workspace");
  const evidence = {};
  for (const full of [false, true]) {
    const options = ["--no-cache", "--dump", "-", "--width", "120", ...(full ? ["--full"] : [])];
    const outputs = ["claude", "codex"].map(agent => execFileSync(binary,
      [join(fixtures, `${agent}-render-reference.jsonl`), ...options], { encoding: "utf8" }));
    for (const output of outputs) {
      for (const value of ["Inspect the workspace change.", "Workspace ready."]) {
        assert(output.includes(value), `${full ? "Expanded" : "Default"} rendering preserves ${value}`);
      }
      assert.equal(output.split("Inspect the workspace change.").length - 1, 1, "The input remains one turn.");
      if (full) {
        assert(output.includes("settings.txt"));
        assert(output.includes("-old value") && output.includes("+new value"), "Both diff signs are preserved.");
        assert(output.includes("/allowed/project"), "Expanded command retains execution context.");
        assert(output.includes("workspace-visible-output"), "Expanded command retains its result.");
      } else {
        assert(output.includes("Ran 1 shell command"), "The default view summarizes its command.");
        assert(output.includes("edited 1 file +1 −1") || output.includes("Update(settings.txt)") && output.includes("Added 1 line, removed 1 line"), "The default view preserves the one-file change and its line counts.");
      }
    }
    // Whitespace-only tool-result spacing is presentation; action content must agree.
    if (full) assert.equal(outputs[0].replace(/\s+/g, " ").trim(), outputs[1].replace(/\s+/g, " ").trim());
    evidence[full ? "expanded" : "default"] = { options, claudeBytes: outputs[0].length, codexBytes: outputs[1].length };
  }
  return { ...evidence, exploration: assertCanonicalExploration(binary) };
}

/** Independent persisted-native reference includes known actions and the mixed negative control. */
function assertCanonicalExploration(binary) {
  const directory = mkdtempSync(join(tmpdir(), "codex-exploration-reference-"));
  const parsed = [
    { type: "read", cmd: "cat src/native-sentinel.rs", name: "native-sentinel.rs", path: "/fixture/src/native-sentinel.rs" },
    { type: "search", cmd: "rg SEARCH_SENTINEL src", query: "SEARCH_SENTINEL", path: "src" },
    { type: "list_files", cmd: "rg --files", path: "." },
  ];
  const command = "cat src/native-sentinel.rs && rg SEARCH_SENTINEL src && rg --files";
  const evidence = {};
  try {
    for (const mode of ["known", "mixed", "repeated"]) {
      const unknown = mode === "mixed";
      const file = join(directory, `${mode}.jsonl`);
      const composite = command + (unknown ? " && OTHER_WORK_SENTINEL" : mode === "repeated" ? ` && ${command}` : "");
      const records = [
        { type: "session_meta", payload: { id: "canonical-exploration-fixture", cwd: "/fixture", cli_version: "0.162.0-alpha.17.2" } },
        { type: "response_item", payload: { type: "message", role: "user", content: [{ type: "input_text", text: "Inspect canonical exploration." }] } },
        { type: "event_msg", payload: { type: "item_completed", item: { type: "CommandExecution", id: "command-fixture", command: ["/bin/zsh", "-lc", composite], cwd: "file:///fixture", status: "completed", exit_code: 0, duration: { secs: 1, nanos: 230000000 }, parsed_cmd: unknown ? [...parsed, { type: "unknown", cmd: "OTHER_WORK_SENTINEL" }] : mode === "repeated" ? [...parsed, ...parsed] : parsed, formatted_output: "EXPLORATION_OUTPUT_SENTINEL\n" } } },
      ];
      writeFileSync(file, records.map(value => JSON.stringify(value)).join("\n") + "\n");
      const outputs = [false, true].map(full => execFileSync(binary, [file, "--no-cache", "--dump", "-", "--width", "120", ...(full ? ["--full"] : [])], { encoding: "utf8" }));
      assert(outputs[0].includes(unknown ? "Ran 1 shell command" : mode === "repeated" ? "Searched for 2 patterns, read 1 file, listed 2 directories" : "Searched for 1 pattern, read 1 file, listed 1 directory"));
      for (const value of [command, "EXPLORATION_OUTPUT_SENTINEL", "exit 0", "1.23s"]) assert(outputs[1].includes(value), `Full canonical command retains ${value}`);
      if (unknown) {
        assert(outputs[1].includes("Bash(")); assert(outputs[1].includes("OTHER_WORK_SENTINEL"));
        assert(!outputs[1].includes("Read(src/native-sentinel.rs)"));
      } else for (const value of ["Read(src/native-sentinel.rs)", "Grep(SEARCH_SENTINEL in src)", "LS(.)"]) assert(outputs[1].includes(value));
      evidence[mode] = { defaultBytes: outputs[0].length, fullBytes: outputs[1].length, defaultSummary: true, targetsCommandOutputAndMetadata: true };
    }
    return evidence;
  } finally { rmSync(directory, { recursive: true, force: true }); }
}
