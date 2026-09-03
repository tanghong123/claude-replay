#!/usr/bin/env node

// Copy the immutable design reference into production assets without hand-editing it.
// Run from anywhere; paths are resolved from this script. The generated files are tracked.
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const sourcePath = resolve(root, "design/agent-monitor-codex-demo.html");
const out = resolve(root, "claude-monitor/src/codex-ui");
const source = await readFile(sourcePath, "utf8");

function between(start, end) {
  const from = source.indexOf(start);
  const to = source.indexOf(end, from + start.length);
  if (from < 0 || to < 0) throw new Error(`reference marker missing: ${start} … ${end}`);
  return source.slice(from + start.length, to);
}

const css = between("<style>\n", "\n</style>") + "\n";
const shell = between("<body>\n", '<script src="sample-transcript-data.js"></script>');
const iconStart = source.indexOf("const icons=");
const iconEnd = source.indexOf("const shared={};", iconStart);
if (iconStart < 0 || iconEnd < 0) throw new Error("reference icon registry markers missing");
let icons = source.slice(iconStart, iconEnd);
icons = icons.replace(/document\.querySelectorAll\([\s\S]*?function agentLogo/, "function agentLogo");
icons += "\nexport { icons, svg, agentLogo };\n";

await mkdir(out, { recursive: true });
await writeFile(resolve(out, "reference.css"), css);
await writeFile(resolve(out, "reference-shell.html"), shell);
await writeFile(resolve(out, "icons.js"), icons);
