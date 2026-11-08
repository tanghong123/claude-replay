#!/usr/bin/env node
// agent-monitor's backend conformance check, for contract version 1 (docs/backend-contract.md).
//
// A host that answers the backend contract from a server of its own runs this against its base
// URL, the address its routes sit under, and learns rule by rule where its replies differ:
//
//   node conform.mjs <base-url> [--cookie name=value]... [--header "Name: value"]...
//                    [--origin <page-origin>] [--session <id>] [--json]
//
// --cookie and --header carry the reader's sign-in (a cookie, or `Authorization: Bearer <token>`
// for a paired agent-monitor); neither is ever printed. --origin is the origin of the page the
// components are mounted in, sent as `Origin` on every request but a GET (the base's own origin
// unless given). --session names the session to read (else a finished one with the fewest turns,
// which is least likely to grow while it is read). --json prints the results as one JSON object
// instead of lines.
//
// Each rule prints as passed (✓), failed (✗) or skipped (–), under the contract section it comes
// from. A route of an optional tier the host does not serve answers 404, and its rules are skipped,
// never failed. Exit 0 conforms, 1 when a rule fails, 2 for a usage error.
//
// What it never does, because a host runs it against a real server: send a prompt (every write it
// makes is refused before it names a session), grant or revoke a pane, answer a dialog, file a note,
// or reveal a file (`/__reveal` only ever gets a stamp nobody minted, for a path that does not
// exist). Nor does it send the reader's sign-in anywhere but under the base URL. What it does
// change on the server:
//   - the hide list: a key no session has, added and removed again (if the removal fails, it says
//     which key to remove);
//   - mdrev's held store: it holds two small documents there, among the most recent the store
//     keeps (agent-monitor keeps 64);
//   - the session it reads, and one of its sub-agents: both are pulled (the session several times,
//     from a fresh cursor and its own), and agent-monitor writes a durable cache entry for each,
//     which marks the row visited. No other session is pulled.
//
// No dependencies: node 20 or later.

const CONTRACT = 1;

const USAGE = `usage: node conform.mjs <base-url> [--cookie name=value]... [--header "Name: value"]...
                         [--origin <page-origin>] [--session <id>] [--json]`;

// ------------------------------------------------------------------------------- the contract

// The contract's sections, by their headings: each rule names the one it comes from.
const S = {
  version: "The version and its rule",
  index: "The route index",
  request: "How a request is made",
  sessions: "The session list › GET /api/sessions",
  ignore: "The session list › GET /api/ignore",
  layout: "The session list › GET /api/layout",
  records: "One session's records",
  pull: "One session's records › GET /pull",
  log: "One session's records › GET /records",
  turnIndex: "One session's records › GET /turn-index",
  search: "One session's records › GET /search",
  file: "Files › GET /file",
  reveal: "Files › GET /__reveal",
  mdrev: "Markdown and diagrams › mdrev",
  mermaid: "Markdown and diagrams › /mermaid/<version>.js",
  writes: "Writes",
  tabs: "The detached tabs",
  meta: "The session's facts (meta)",
  every: "The record format › Every record",
  kinds: "The record format › The kinds",
  parts: "The record format › Body parts",
  chips: "The record format › Chips carry facts in their words",
  shapes: "The record format › Smaller shapes",
};

// The route index, row for row. `served` is learned as the rules run: true, false (404, a ceiling)
// or null (not told apart from outside).
const ROUTES = [
  ["GET", "/api/sessions", "core"],
  ["GET", "/api/ignore", "optional"],
  ["GET", "/api/layout", "optional"],
  ["GET", "/pull", "core"],
  ["GET", "/records", "core"],
  ["GET", "/turn-index", "optional"],
  ["GET", "/search", "optional"],
  ["GET", "/file", "optional"],
  ["GET", "/__reveal", "optional"],
  ["POST", "/api/send", "write"],
  ["POST", "/api/consent", "write"],
  ["GET", "/api/dialog", "write"],
  ["POST", "/api/answer", "write"],
  ["GET", "/monitor-ui/", "core"],
  ["GET", "/mdrev/<mdrev>/", "optional"],
  ["ANY", "/api/mdrev/", "optional"],
  ["GET", "/mermaid/", "optional"],
  ["GET", "/image", "page"],
  ["GET", "/markdown", "page"],
  ["ANY", "/api/mdrev-review/", "page"],
  ["ANY", "/api/codex/", "native"],
  ["GET", "/codex-workspace/", "native"],
].map(([method, path, tier]) => ({ method, path, tier, served: null }));

function served(path, value) {
  const row = ROUTES.find((r) => r.path === path);
  if (row && value !== null) row.served = value;
}

// Shapes, field by field, as the contract's tables give them. A key ending in `?` is optional; a
// type is a name below, several joined by `|`, or `oneOf(...)` for a closed set. `any` is a field
// the contract names without a type: it must be there, and nothing more is asked of it.
const oneOf = (...values) => ({ oneOf: values });
const RFC3339 = /^\d{4}-\d{2}-\d{2}[Tt ]\d{2}:\d{2}:\d{2}(\.\d+)?([Zz]|[+-]\d{2}:\d{2})$/;
const TYPES = {
  string: (v) => typeof v === "string",
  number: (v) => typeof v === "number" && Number.isFinite(v),
  boolean: (v) => typeof v === "boolean",
  true: (v) => v === true,
  array: (v) => Array.isArray(v),
  object: (v) => v !== null && typeof v === "object" && !Array.isArray(v),
  null: (v) => v === null,
  any: () => true,
  rfc3339: (v) => typeof v === "string" && RFC3339.test(v) && !Number.isNaN(Date.parse(v)),
};

const GROUP = {
  kind: oneOf("project", "agent"),
  label: "string",
  secondary: "string",
  metaLine: "string",
  hasTerm: "boolean",
  growing: "number",
  idle: "number",
  total: "number",
  ignoreKey: "string",
  hidden: "boolean",
  "cost?": "number",
  "costPartial?": "true",
  rows: "array",
};

const ROW = {
  id: "string",
  name: "string",
  agent: "string",
  state: oneOf("growing", "idle", "finished"),
  agentState: oneOf("busy", "wait", "idle"),
  stateReason: "string",
  stateDetail: "string",
  stateConfidence: oneOf("observed", "inferred"),
  stateSince: "rfc3339|null",
  activityTs: "number",
  activity: "string",
  "startTs?": "number",
  ignoreKey: "string",
  hidden: "boolean",
  family: "string",
  "isFork?": "true",
  "sideOf?": "string",
  "startedBy?": "string",
  "brief?": "string",
  "projActive?": "true",
  "term?": "string",
  "target?": "string",
  "herdrSession?": "string",
  "injectable?": "true",
  "sendsImages?": "true",
  "consented?": "boolean",
  "dialog?": oneOf("question", "permission", "plan"),
  "cost?": "number",
  "costPartial?": "true",
  "costOwn?": "number",
  "costSubs?": "number",
};

const PULL = {
  t: oneOf("pull"),
  epoch: "number",
  committed_from: "number",
  committed_ext: "object|null",
  provisional_gen: "number",
  provisional_from: "number",
  provisional: "array",
  meta: "object|null",
};

const EXT = { offset: "number", len: "number" };

const META = {
  t: oneOf("meta"),
  title: "string",
  agent: "string",
  sid: "string",
  cwd: "string",
  turns: "number",
  tools: "number",
  version: "string",
  usage: "object",
  tasks: "array",
  agent_type: "any",
  ancestors: "array",
  children: "array",
  path: "any",
  "runs?": "array",
};

const USAGE_FIELDS = {
  input: "string",
  output: "string",
  cache_read: "string",
  cost: "string|null",
  model: "any",
  duration_secs: "any",
  "reported?": "object",
  "runtime?": "object",
};

const REPORTED = {
  cost: "any",
  complete: "any",
  epochs: "any",
  from: "any",
  through: "any",
  unknown_model: "any",
};

const TASK = {
  id: "any",
  subject: "any",
  description: "any",
  active_form: "any",
  status: oneOf("Pending", "InProgress", "Completed"),
  blocks: "any",
  blocked_by: "any",
  "log?": "array",
};

const TURN = {
  kind: "string",
  id: "string",
  turn: "any",
  label: "any",
  "reply_context?": "object",
  "head?": "object",
  r: "number",
  last: "number",
  o: "number",
  end: "number",
};

const RECORD = {
  t: oneOf("block"),
  kind: "string",
  id: "string",
  "turn?": "number",
  "label?": "string",
  "ts?": "number",
  "dur?": "number",
  "src?": "string",
  "reply_context?": "object",
  "inbox?": "object",
  "phase?": oneOf("commentary", "final"),
  "tool?": "string",
  "epoch?": "true",
  "head?": "object",
  "body?": "array",
};

const TOOL_KINDS = ["bash", "read", "write", "edit", "skill"];
const CALL_HEAD = { name: "any", target: "any", dot: "boolean", "chips?": "array" };
const KIND_HEADS = {
  peer: { from: "any" },
  command: { badge: "any", preview: "any", "chips?": "array" },
  assistant: { "presentation?": oneOf("proposed_plan") },
  attachment: {
    att_kind: oneOf("file", "plan", "edited", "ref", "image"),
    att_name: "any",
    att_dl: "any",
    "chips?": "array",
  },
  agent: { badge: oneOf("Agent"), preview: "any", chips: "array", child: "string", child_id: "string" },
  compaction: { summary: "any", compact_trigger: oneOf("auto", "manual") },
  think: { summary: "any", "chips?": "array" },
  act: { summary: "any", "chips?": "array" },
  ...Object.fromEntries(TOOL_KINDS.map((k) => [k, CALL_HEAD])),
};

const PARTS = {
  md: { h: "string" },
  think: { h: "string" },
  raw: { x: "string" },
  pre: { x: "string", cap: "any" },
  num: { rows: "array", cap: "any" },
  diff: { rows: "array", cap: "any" },
  note: { x: "string", "file?": "object" },
  ctx: { model: "any", totals: "any", rows: "array", extras: "array" },
  blocks: { items: "array" },
};

const CHIP = { x: "string", "c?": oneOf("add", "del", "fail", "sandbox") };
const OFFER = { path: "string", "sig?": "string", "fsig?": "string" };
const INTERACTION = {
  kind: oneOf("request_user_input"),
  resolved: "any",
  answers: "array",
  "asked?": "array",
  "unanswered?": "object",
};
const ARTIFACT = { url: "any", name: "any", icon: "any", desc: "any" };

// The write routes and the refusal codes each may name (the Writes table).
const WRITES = [
  { path: "/api/send", method: "POST", codes: ["no-consent", "held", "shared_runtime_required"] },
  {
    path: "/api/consent",
    method: "POST",
    codes: ["passcode-required", "bad-passcode", "locked", "no-consent", "held", "shared_runtime_required"],
  },
  { path: "/api/dialog", method: "GET", codes: ["no-consent", "held"] },
  { path: "/api/answer", method: "POST", codes: ["held", "no-consent"], body: "{}" },
];

// ---------------------------------------------------------------------------------- arguments

function usage(why) {
  process.stderr.write(`conform: ${why}\n${USAGE}\n`);
  process.exit(2);
}

function parseArgs(argv) {
  const o = { base: null, cookies: [], headers: [], origin: null, session: null, json: false };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    const value = () => {
      if (i + 1 >= argv.length) usage(`${a} takes a value`);
      return argv[++i];
    };
    if (a === "--cookie") o.cookies.push(value());
    else if (a === "--header") o.headers.push(value());
    else if (a === "--session") o.session = value();
    else if (a === "--origin") o.origin = value();
    else if (a === "--json") o.json = true;
    else if (a === "--help" || a === "-h") {
      process.stdout.write(`${USAGE}\n`);
      process.exit(0);
    } else if (a.startsWith("--")) usage(`unknown option ${a}`);
    else if (o.base === null) o.base = a;
    else usage("one base URL only");
  }
  if (!o.base) usage("no base URL");
  return o;
}

const major = Number(process.versions.node.split(".")[0]);
if (major < 20) usage(`node ${process.versions.node}: the check needs node 20 or later`);

const ARGS = parseArgs(process.argv.slice(2));
let BASE;
try {
  BASE = new URL(ARGS.base);
} catch {
  usage("the base is not a URL");
}
if (BASE.protocol !== "http:" && BASE.protocol !== "https:") usage("the base URL must be http or https");
// A sign-in belongs in --header or --cookie, which are never printed; the base is printed.
if (BASE.username || BASE.password) usage("put the sign-in in --header or --cookie, not in the base URL");
BASE.search = "";
BASE.hash = "";
if (!BASE.pathname.endsWith("/")) BASE.pathname += "/";
// The page's own origin, as a browser names it on a request that is not a GET.
let ORIGIN = BASE.origin;
if (ARGS.origin) {
  try {
    ORIGIN = new URL(ARGS.origin).origin;
  } catch {
    usage("--origin takes a URL, such as https://app.example");
  }
}

// The reader's sign-in: sent with every request but the ones that ask what happens without it.
// Checked once here, and never quoted: a header's value is the reader's credential.
const HEADER_NAME = /^[!#$%&'*+.^_`|~0-9A-Za-z-]+$/;
// eslint-disable-next-line no-control-regex
const CONTROL = /[\u0000-\u0008\u000a-\u001f\u007f]/;
const AUTH = {};
for (const [i, line] of ARGS.headers.entries()) {
  const at = line.indexOf(":");
  const name = at > 0 ? line.slice(0, at).trim() : "";
  if (!HEADER_NAME.test(name)) usage(`--header ${i + 1} is not "Name: value" (its text is not shown)`);
  const value = line.slice(at + 1).trim();
  if (CONTROL.test(value)) usage(`--header ${i + 1} holds a line break or another control character (its value is not shown)`);
  AUTH[name.toLowerCase()] = value;
}
for (const [i, c] of ARGS.cookies.entries()) {
  const at = c.indexOf("=");
  if (at <= 0 || !HEADER_NAME.test(c.slice(0, at).trim())) usage(`--cookie ${i + 1} is not name=value (its text is not shown)`);
  if (CONTROL.test(c) || c.includes(";")) usage(`--cookie ${i + 1} holds a control character or a ";" (its value is not shown)`);
}
if (ARGS.cookies.length) AUTH.cookie = [AUTH.cookie, ...ARGS.cookies].filter(Boolean).join("; ");
try {
  new Headers(AUTH);
} catch {
  usage("a --header or --cookie is not one a request can carry (its value is not shown)");
}
const SIGNED_IN = Object.keys(AUTH).length > 0;

// Every value the sign-in carries, to keep out of anything printed.
const SECRETS = Object.values(AUTH)
  .flatMap((v) => [v, ...v.split(/[;,\s=]+/)])
  .map((v) => v.trim())
  .filter((v) => v.length >= 6);

/** A line fit to print: no part of the sign-in, and no stamp a URL or a reply carried. */
function redact(text) {
  let s = String(text);
  for (const secret of SECRETS) s = s.split(secret).join("<sign-in>");
  return s.replace(/((?:^|[?&\s"'{,])(?:att_)?(?:f?sig|cap)["']?\s*[=:]\s*["']?)[^&\s"',}]+/g, "$1<stamp>");
}

// -------------------------------------------------------------------------------------- HTTP

const TIMEOUT_MS = 60_000;
const enc = encodeURIComponent;
// What a host is asked for when the check means a value nobody minted or no session has.
const NONCE = `${Date.now().toString(36)}${Math.random().toString(36).slice(2, 8)}`;
const NO_SUCH_SESSION = `conform-no-such-session-${NONCE}`;
const FOREIGN = "https://conform-check.invalid";

class Reply {
  constructor(what, status, headers, bytes) {
    this.what = what;
    this.status = status;
    this.headers = headers;
    this.bytes = bytes;
  }
  header(name) {
    return this.headers.get(name);
  }
  text() {
    return this.bytes.toString("utf8");
  }
  json() {
    try {
      return JSON.parse(this.text());
    } catch {
      fail(`${this.what} answered ${this.status} with a body that is not JSON: ${brief(this.text())}`);
    }
  }
}

/** `path` joined to the base, or null where it leaves it (an absolute URL, `//host`, `\\host`, or
 * `..` past the base): nothing is ever sent there, the reader's sign-in least of all. */
function underBase(path) {
  let url;
  try {
    url = new URL(String(path).replace(/^\/+/, ""), BASE);
  } catch {
    return null;
  }
  return url.origin === BASE.origin && url.href.startsWith(BASE.href) ? url : null;
}

/** A request as a line fit to print: the path under the base, its stamps left out. */
function described(method, url) {
  return redact(`${method} /${url.href.slice(BASE.href.length)}`);
}

/** One request to the base, as the page would make it: relative to the base, cookies and the
 * reader's sign-in sent, and the page's own origin on anything but a GET. With `noDownload`, a
 * reply that is a download (`application/octet-stream`) is not read: its headers are kept and its
 * body cancelled. */
async function ask(method, path, { body, headers = {}, signedIn = true, noDownload = false } = {}) {
  const url = underBase(path);
  if (!url) fail(`${redact(String(path).slice(0, 60))} is outside the base: nothing is sent there`);
  const h = new Headers(signedIn ? AUTH : {});
  for (const [k, v] of Object.entries(headers)) h.set(k, v);
  if (method !== "GET" && method !== "HEAD" && !h.has("origin")) h.set("origin", ORIGIN);
  const what = described(method, url);
  let res;
  try {
    res = await fetch(url, { method, headers: h, body, redirect: "manual", signal: AbortSignal.timeout(TIMEOUT_MS) });
  } catch (e) {
    fail(`${what}: no answer (${e.cause?.code ?? e.name})`);
  }
  if (noDownload && mediaType(res.headers.get("content-type")) === "application/octet-stream") {
    await res.body?.cancel().catch(() => {});
    return new Reply(what, res.status, res.headers, Buffer.alloc(0));
  }
  let bytes;
  try {
    bytes = Buffer.from(await res.arrayBuffer());
  } catch (e) {
    fail(`${what}: the reply was cut off (${e.cause?.code ?? e.name})`);
  }
  return new Reply(what, res.status, res.headers, bytes);
}

/** A content type's media type, lower-cased, without its parameters. */
function mediaType(type) {
  return String(type ?? "").split(";")[0].trim().toLowerCase();
}

/** The page at the base, loaded as a browser loads it: a redirect within the base is followed, at
 * most twice, with the cookies it set (a sign-in that turns a presented token into a cookie, and
 * then reads the cookie rather than the token). */
async function loadPage() {
  let path = "";
  const jar = [];
  for (let hop = 0; hop < 3; hop++) {
    const cookie = [AUTH.cookie, ...jar].filter(Boolean).join("; ");
    const { authorization, ...rest } = AUTH;
    const reply = jar.length
      ? await ask("GET", path, { signedIn: false, headers: { ...rest, cookie } })
      : await ask("GET", path);
    if (reply.status < 300 || reply.status >= 400) return reply;
    const to = reply.header("location");
    if (!to) return reply;
    const next = new URL(to, new URL(path, BASE));
    if (next.origin !== BASE.origin || !next.href.startsWith(BASE.href)) return reply;
    for (const line of reply.headers.getSetCookie?.() ?? []) jar.push(line.split(";")[0].trim());
    path = next.href.slice(BASE.href.length);
  }
  return null;
}

/** A request with a body the server is told the length of but sent only if it waits for it, over
 * a socket of its own: a server that refuses a body by its length answers before it is sent, and
 * one that reads first gets it after a few seconds. `fetch` sends the body at once, and a server
 * that refused it unread may reset the connection before its answer is read. */
async function askWithLength(method, path, length) {
  const { connect } = await import(BASE.protocol === "https:" ? "node:tls" : "node:net");
  const url = underBase(path);
  if (!url) fail("a path outside the base: nothing is sent there");
  const what = described(method, url);
  const port = Number(url.port || (url.protocol === "https:" ? 443 : 80));
  return new Promise((resolve) => {
    let got = Buffer.alloc(0);
    let done = false;
    const sock = connect({ host: url.hostname, port, servername: url.hostname });
    const timers = [];
    const end = (why) => {
      if (done) return;
      done = true;
      timers.forEach(clearTimeout);
      sock.destroy();
      const m = got.toString("latin1").match(/^HTTP\/1\.[01] (\d{3})/);
      resolve({ what, status: m ? Number(m[1]) : null, why });
    };
    sock.once(url.protocol === "https:" ? "secureConnect" : "connect", () => {
      const lines = [`${method} ${url.pathname}${url.search} HTTP/1.1`, `Host: ${url.host}`, `Origin: ${ORIGIN}`];
      for (const [k, v] of Object.entries(AUTH)) lines.push(`${k}: ${v}`);
      lines.push(`Content-Length: ${length}`, "Connection: close", "", "");
      sock.write(lines.join("\r\n"));
      timers.push(setTimeout(() => !done && sock.write(Buffer.alloc(length, 0x20), () => {}), 3000));
    });
    sock.on("data", (chunk) => {
      got = Buffer.concat([got, chunk]);
      if (got.includes("\r\n")) end();
    });
    sock.on("error", (e) => end(e.code ?? e.message));
    sock.on("close", () => end("closed"));
    timers.push(setTimeout(() => end("no answer"), TIMEOUT_MS));
  });
}

// ------------------------------------------------------------------------------------ results

const results = [];
// Whether a session's records were read, and if not why: the last line says what went unchecked.
const READ = { session: false, why: "the session list did not answer" };
const SKIP = Symbol("skip");
const skip = (why) => ({ [SKIP]: why });
class Fail extends Error {}
function fail(why) {
  throw new Fail(why);
}

/** The verdict over a list of problems: a pass with `ok`, or a fail naming the first few. */
function verdict(problems, ok) {
  if (problems.length) {
    const more = problems.length > 4 ? `; and ${problems.length - 4} more` : "";
    fail(problems.slice(0, 4).join("; ") + more);
  }
  return ok;
}

let printedSection = null;
const MARK = { pass: "✓", fail: "✗", skip: "–" };

/** Run one rule: `fn` returns a detail (a pass), `skip(why)`, or throws (a fail). */
async function rule(id, section, says, fn) {
  let status;
  let detail;
  try {
    const out = await fn();
    if (out && typeof out === "object" && SKIP in out) {
      status = "skip";
      detail = out[SKIP];
    } else {
      status = "pass";
      detail = out ?? "";
    }
  } catch (e) {
    status = "fail";
    detail = e instanceof Fail ? e.message : `${e.name}: ${e.message}`;
  }
  detail = redact(detail);
  results.push({ id, section, rule: says, status, detail });
  if (!ARGS.json) {
    if (section !== printedSection) {
      process.stdout.write(`\n${section}\n`);
      printedSection = section;
    }
    process.stdout.write(`  ${MARK[status]} ${id}  ${says}${detail ? ` — ${detail}` : ""}\n`);
  }
  return status === "pass";
}

// ------------------------------------------------------------------------------------- shapes

function show(v) {
  if (v === undefined) return "missing";
  const s = JSON.stringify(v);
  return s.length > 40 ? `${s.slice(0, 37)}...` : s;
}

function brief(text) {
  const s = String(text).replace(/\s+/g, " ").trim();
  return JSON.stringify(s.length > 80 ? `${s.slice(0, 77)}...` : s);
}

function typeOk(spec, v) {
  if (typeof spec === "object") return spec.oneOf.includes(v);
  return spec.split("|").some((t) => TYPES[t](v));
}

function describe(spec) {
  if (typeof spec === "object") return spec.oneOf.map((v) => JSON.stringify(v)).join(" or ");
  return spec.split("|").join(" or ");
}

/** Check `obj` against `spec`, adding a line per problem to `out`. */
function fields(obj, spec, where, out) {
  if (!TYPES.object(obj)) {
    out.push(`${where} is ${show(obj)}, not an object`);
    return false;
  }
  const before = out.length;
  for (const [key, type] of Object.entries(spec)) {
    const optional = key.endsWith("?");
    const name = optional ? key.slice(0, -1) : key;
    if (!(name in obj) || obj[name] === undefined) {
      if (!optional) out.push(`${where}.${name} is missing`);
    } else if (!typeOk(type, obj[name])) {
      out.push(`${where}.${name} is ${show(obj[name])}, not ${describe(type)}`);
    }
  }
  return out.length === before;
}

const plural = (n, one, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

// ------------------------------------------------------------------------------- the records

/** The log's bytes as lines, without their newlines; a last line with none is a line too. */
function splitLines(bytes) {
  const lines = [];
  let at = 0;
  while (at < bytes.length) {
    const nl = bytes.indexOf(0x0a, at);
    const end = nl < 0 ? bytes.length : nl;
    lines.push(bytes.subarray(at, end));
    at = end + 1;
  }
  return lines;
}

/** Every record, nested ones included, with where it sits. */
function* everyRecord(records, where = "records") {
  for (const [i, rec] of records.entries()) {
    const here = `${where}[${i}]`;
    yield [rec, here];
    const body = TYPES.object(rec) && Array.isArray(rec.body) ? rec.body : [];
    for (const [j, part] of body.entries()) {
      if (TYPES.object(part) && part.p === "blocks" && Array.isArray(part.items)) {
        yield* everyRecord(part.items, `${here}.body[${j}].items`);
      }
    }
  }
}

/** Every offered path in a record and the records nested in it: `{path, sig, fsig}`. */
function offers(rec) {
  const out = [];
  for (const [r] of everyRecord([rec])) {
    const head = TYPES.object(r.head) ? r.head : {};
    if (typeof head.path === "string") out.push({ path: head.path, sig: head.sig, fsig: head.fsig });
    if (typeof head.att_path === "string") out.push({ path: head.att_path, sig: head.att_sig, fsig: head.att_fsig });
    for (const f of Array.isArray(head.files) ? head.files : []) {
      if (TYPES.object(f) && typeof f.path === "string") out.push(f);
    }
    for (const part of Array.isArray(r.body) ? r.body : []) {
      if (part?.p === "note" && TYPES.object(part.file) && typeof part.file.path === "string") out.push(part.file);
    }
  }
  return out;
}

function textOf(html) {
  return String(html).replace(/<[^>]*>/g, " ").replace(/&[a-z#0-9]+;/gi, " ");
}

// -------------------------------------------------------------------------------- the page

function tagAttrs(tag) {
  const attrs = {};
  for (const m of tag.matchAll(/([\w:-]+)\s*=\s*"([^"]*)"/g)) attrs[m[1].toLowerCase()] = m[2];
  return attrs;
}

function readPage(html) {
  const body = html.match(/<body\b[^>]*>/i);
  const facts = body ? tagAttrs(body[0]) : {};
  let mermaid = null;
  for (const m of html.matchAll(/<meta\b[^>]*>/gi)) {
    const a = tagAttrs(m[0]);
    if (a.name === "mermaid-runtime") mermaid = a.content ?? null;
  }
  return {
    contract: facts["data-contract"],
    paired: facts["data-paired"],
    mdrev: facts["data-mdrev"],
    mermaid,
  };
}

// --------------------------------------------------------------------------------- the check

async function main() {
  // A session named on the command line is read before the list is asked, as a reader that
  // comes back to a session does.
  let early = null;
  if (ARGS.session) {
    early = await ask("GET", `pull?session=${enc(ARGS.session)}&cursor=0.0.0.0`).catch((e) => e);
  }

  // ----------------------------------------------------------------------- the session list
  let list = null;
  let listReply = null;
  const signedIn = await rule(
    "request.signed-in",
    S.request,
    "the reader's sign-in is accepted (the page reads a 401 as not paired)",
    async () => {
      listReply = await ask("GET", "api/sessions");
      if (listReply.status === 401) {
        fail(
          SIGNED_IN
            ? "401: the server refused the sign-in given with --cookie or --header"
            : "401: the server wants a sign-in; pass it with --cookie or --header",
        );
      }
      return `GET /api/sessions answered ${listReply.status}`;
    },
  );
  if (!signedIn) return finish();

  await rule("sessions.reply", S.sessions, "the reply is {groups: Group[], ignoredCount: number}", async () => {
    if (listReply.status !== 200) fail(`${listReply.what} answered ${listReply.status}, not 200`);
    const body = listReply.json();
    const problems = [];
    fields(body, { groups: "array", ignoredCount: "number" }, "the reply", problems);
    verdict(problems);
    list = body;
    served("/api/sessions", true);
    const rows = body.groups.flatMap((g) => (Array.isArray(g?.rows) ? g.rows : []));
    return `${plural(body.groups.length, "group")}, ${plural(rows.length, "row")}`;
  });
  const groups = list?.groups ?? [];
  const rows = groups.flatMap((g) => (TYPES.object(g) && Array.isArray(g.rows) ? g.rows : []));

  await rule("sessions.group", S.sessions, "every group carries the Group table's fields", async () => {
    if (!list) return skip("no list to read");
    if (!groups.length) return skip("the list holds no group");
    const problems = [];
    for (const [i, g] of groups.entries()) {
      if (!fields(g, GROUP, `groups[${i}]`, problems)) continue;
      if (!/^[pa]:/.test(g.ignoreKey)) problems.push(`groups[${i}].ignoreKey is ${show(g.ignoreKey)}, not p:<path> or a:<label>`);
    }
    return verdict(problems, plural(groups.length, "group"));
  });

  await rule("sessions.row", S.sessions, "every row carries the Row table's fields", async () => {
    if (!rows.length) return skip("the list holds no row");
    const problems = [];
    for (const [i, g] of groups.entries()) {
      for (const [j, r] of (Array.isArray(g?.rows) ? g.rows : []).entries()) fields(r, ROW, `groups[${i}].rows[${j}]`, problems);
    }
    return verdict(problems, plural(rows.length, "row"));
  });

  await rule("sessions.row-key", S.sessions, "a row's ignoreKey is s:<id>", async () => {
    if (!rows.length) return skip("the list holds no row");
    const problems = rows
      .filter((r) => TYPES.object(r) && r.ignoreKey !== `s:${r.id}`)
      .map((r) => `row ${show(r.id)} has ignoreKey ${show(r.ignoreKey)}`);
    return verdict(problems, plural(rows.length, "row"));
  });

  await rule("sessions.hidden", S.sessions, "a row is hidden when its group is (hidden by its own key or its group's)", async () => {
    const hiddenGroups = groups.filter((g) => g?.hidden === true);
    if (!hiddenGroups.length) return skip("no group is hidden");
    const problems = [];
    for (const g of hiddenGroups) {
      for (const r of g.rows ?? []) if (r?.hidden !== true) problems.push(`row ${show(r?.id)} of hidden group ${show(g.ignoreKey)} is not hidden`);
    }
    return verdict(problems, plural(hiddenGroups.length, "hidden group"));
  });

  // The session to read: the one named, else the one least likely to grow while it is read, a
  // finished session with the fewest turns (a worker last, since the page lists it elsewhere).
  const listed = rows.filter((r) => TYPES.object(r) && typeof r.id === "string");
  const quietFirst = (r) => [
    r.state === "finished" ? 0 : r.state === "idle" ? 1 : 2,
    r.startedBy ? 1 : 0,
    TYPES.number(r.turns) ? r.turns : Number.MAX_SAFE_INTEGER,
  ];
  const order = (a, b) => {
    const [x, y] = [quietFirst(a), quietFirst(b)];
    return x[0] - y[0] || x[1] - y[1] || x[2] - y[2];
  };
  const sid = ARGS.session ?? [...listed].sort(order)[0]?.id ?? null;
  // Whether the named session is one the list shows (taken as so when the list could not be read).
  const named = !ARGS.session || !list || listed.some((r) => r.id === ARGS.session);
  const unlisted = named ? "" : ` (--session ${ARGS.session.slice(0, 40)} is not among the list's ${plural(listed.length, "row")})`;

  await rule(
    "sessions.before-list",
    S.sessions,
    "a session route answers an id before the list is asked for it",
    async () => {
      if (!ARGS.session) return skip("pass --session <id> to check it, against a server that has not listed it yet");
      if (!named) return skip(`--session ${ARGS.session.slice(0, 40)} is not among the list's rows, and an id the list would never show stays unknown`);
      if (early instanceof Error) throw early;
      if (early.status !== 200) fail(`${early.what} answered ${early.status} before the list was asked`);
      const body = early.json();
      if (body.t !== "pull" && body.t !== "redirect") fail(`${early.what} answered t ${show(body.t)}`);
      return `t ${body.t}`;
    },
  );

  // ------------------------------------------------------------------------ the page itself
  let page = null;
  {
    const reply = await loadPage().catch(() => null);
    if (reply && reply.status === 200 && /text\/html/i.test(reply.header("content-type") ?? "")) page = readPage(reply.text());
  }

  await rule("page.contract", S.version, "the page the server puts out names contract 1 (<body data-contract>)", async () => {
    if (!page) return skip("the base serves no page: a host that mounts the components carries the version in its handle");
    if (page.contract === undefined) return skip("the page names no contract version");
    if (page.contract !== String(CONTRACT)) fail(`data-contract is ${show(page.contract)}, not "${CONTRACT}"`);
    return `data-contract="${page.contract}"`;
  });

  await rule("ui.modules", S.index, "the page's modules are served under /monitor-ui/ (a core row)", async () => {
    const reply = await ask("GET", "monitor-ui/app.js");
    if (reply.status !== 200) fail(`${reply.what} answered ${reply.status}`);
    if (!reply.bytes.length) fail(`${reply.what} answered an empty body`);
    served("/monitor-ui/", true);
    return `${reply.what}: ${reply.bytes.length} bytes`;
  });

  // ------------------------------------------------------------------------- one session
  await rule("session.bad-id", S.records, "an id holding / or .. gets 404, on all four routes", async () => {
    const problems = [];
    for (const route of ["pull", "records", "turn-index", "search"]) {
      for (const id of ["a/b", ".."]) {
        const reply = await ask("GET", `${route}?session=${id}&cursor=0.0.0.0&from=0&len=1&epoch=1&upto=1&q=%7B%7D`);
        if (reply.status !== 404) problems.push(`${reply.what} answered ${reply.status}`);
      }
    }
    return verdict(problems, "8 requests, each 404");
  });

  await rule("session.empty-id", S.records, "an empty id gets 404 from /records, /turn-index and /search", async () => {
    const problems = [];
    for (const route of ["records", "turn-index", "search"]) {
      const reply = await ask("GET", `${route}?session=&from=0&len=1&epoch=1&upto=1&q=%7B%7D`);
      if (reply.status !== 404) problems.push(`${reply.what} answered ${reply.status}`);
    }
    return verdict(problems, "3 requests, each 404");
  });

  await rule("pull.unknown", S.pull, "an unknown session gets 404 {t: \"error\", message}", async () => {
    const reply = await ask("GET", `pull?session=${NO_SUCH_SESSION}&cursor=0.0.0.0`);
    if (reply.status !== 404) fail(`${reply.what} answered ${reply.status}`);
    const body = reply.json();
    const problems = [];
    fields(body, { t: oneOf("error"), message: "string" }, "the reply", problems);
    return verdict(problems, `message ${brief(body.message)}`);
  });

  await rule(
    "session.no-log",
    S.records,
    "a session the server holds no log for gets 409 from /records, /turn-index and /search",
    async () => {
      const problems = [];
      const notServed = [];
      for (const route of ["records", "turn-index", "search"]) {
        const q = enc(JSON.stringify({ lc: "tern", whole: false, wanted: 0, tools: [] }));
        const reply = await ask("GET", `${route}?session=${NO_SUCH_SESSION}&from=0&len=1&epoch=1&upto=1&q=${q}`);
        if (reply.status === 404 && route !== "records") notServed.push(route);
        else if (reply.status !== 409) problems.push(`${reply.what} answered ${reply.status}`);
      }
      return verdict(problems, notServed.length ? `409 where served (${notServed.join(", ")}: 404, not served)` : "each 409");
    },
  );

  // The walk: one fresh pull, its committed records read, and what later rules compare against.
  const walk = { ok: false };
  await rule("pull.reply", S.pull, "a fresh pull answers the envelope {t: \"pull\", epoch, committed_from, committed_ext, provisional_gen, provisional_from, provisional, meta}", async () => {
    if (!sid) return skip("the list holds no session to read");
    const reply = await ask("GET", `pull?session=${enc(sid)}&cursor=0.0.0.0`);
    if (reply.status !== 200) {
      // A 404 the route answers says {t: "error"}; any other is no route at all.
      let t = null;
      try {
        t = JSON.parse(reply.text())?.t ?? null;
      } catch {}
      if (reply.status === 404 && t !== "error") served("/pull", false);
      fail(`${reply.what} answered ${reply.status}: ${brief(reply.text())}${unlisted}`);
    }
    const body = reply.json();
    if (body.t === "redirect") {
      walk.redirect = body.url;
      return skip(`the server sent {t: "redirect", url}: another process owns ${sid}, and the session's rules are not checked`);
    }
    const problems = [];
    fields(body, PULL, "the reply", problems);
    if (TYPES.object(body.committed_ext)) fields(body.committed_ext, EXT, "committed_ext", problems);
    verdict(problems);
    Object.assign(walk, { ok: true, sid, pull: body, epoch: body.epoch, ext: body.committed_ext });
    served("/pull", true);
    return `epoch ${body.epoch}, committed ${body.committed_ext ? `${body.committed_ext.len} bytes from record ${body.committed_from}` : "none"}, ${plural(body.provisional.length, "provisional record")}`;
  });
  const noWalk = () => skip(walk.redirect ? "the session is served elsewhere" : sid ? "the pull did not answer" : "the list holds no session to read");
  READ.session = walk.ok;
  READ.why = walk.redirect ? "the session is served elsewhere" : sid ? `the pull of ${sid.slice(0, 40)} did not answer${unlisted}` : "the list holds no session";

  await rule("sessions.each-pulls", S.sessions, "the session routes answer for the ids this list shows", async () => {
    if (!sid) return skip("the list holds no row");
    if (!named) return skip(`--session ${ARGS.session.slice(0, 40)} is not among the list's rows`);
    if (!walk.ok && !walk.redirect) fail(`the list shows ${sid.slice(0, 40)}, and its pull did not answer`);
    return `${sid.slice(0, 40)}, as listed, pulled (the one session the check reads)`;
  });

  // Whether the session grew since the walk's pull: asked at the reader's own cursor before a rule
  // that compares two requests calls a difference a failure.
  const grew = async () => {
    const p = walk.pull;
    const cursor = `${p.epoch}.${committedCount()}.${p.provisional_gen}.${p.provisional_from + p.provisional.length}`;
    const reply = await ask("GET", `pull?session=${enc(sid)}&cursor=${cursor}`).catch(() => null);
    if (!reply || reply.status !== 200) return false;
    let body;
    try {
      body = JSON.parse(reply.text());
    } catch {
      return false;
    }
    return body.epoch !== p.epoch || body.committed_ext !== null || (body.provisional?.length ?? 0) > 0 || body.provisional_gen !== p.provisional_gen;
  };
  const GREW = "the session grew between the requests, so the two cannot be compared";

  await rule("pull.fresh", S.pull, "a fresh reader (epoch 0, which never matches) gets everything, and the session's facts", async () => {
    if (!walk.ok) return noWalk();
    const p = walk.pull;
    const problems = [];
    if (p.epoch === 0) problems.push("epoch is 0, which a fresh cursor would match");
    if (p.committed_from !== 0) problems.push(`committed_from is ${p.committed_from}, not 0`);
    if (p.provisional_from !== 0) problems.push(`provisional_from is ${p.provisional_from}, not 0`);
    if (p.committed_ext && p.committed_ext.offset !== 0) problems.push(`committed_ext.offset is ${p.committed_ext.offset}: record 0 is byte 0`);
    if (!TYPES.object(p.meta)) problems.push(`meta is ${show(p.meta)}: a fresh pull changes everything`);
    return verdict(problems, "from record 0, with meta");
  });

  await rule("records.reply", S.log, "the committed pointer reads as whole records, one JSON object per line, application/json", async () => {
    if (!walk.ok) return noWalk();
    walk.committed = [];
    walk.lines = [];
    walk.offsets = [];
    walk.logLen = 0;
    const ext = walk.ext;
    if (!ext) {
      walk.read = true;
      return skip("the pull pointed at no committed records");
    }
    const reply = await ask("GET", `records?session=${enc(sid)}&from=${ext.offset}&len=${ext.len}&epoch=${walk.epoch}`);
    if (reply.status === 404) served("/records", false);
    if (reply.status !== 200) fail(`${reply.what} answered ${reply.status}: ${brief(reply.text())}`);
    const problems = [];
    if (mediaType(reply.header("content-type")) !== "application/json") problems.push(`its type is ${show(reply.header("content-type"))}`);
    if (reply.bytes.length !== ext.len) problems.push(`it is ${reply.bytes.length} bytes where the pointer said ${ext.len}`);
    const lines = splitLines(reply.bytes);
    let at = ext.offset;
    for (const [i, line] of lines.entries()) {
      walk.offsets.push(at);
      at += line.length + 1;
      try {
        const rec = JSON.parse(line.toString("utf8"));
        if (!TYPES.object(rec)) problems.push(`line ${i + 1} is ${show(rec)}, not an object`);
        walk.committed.push(rec);
      } catch {
        problems.push(`line ${i + 1} is not JSON: ${brief(line.toString("utf8"))}`);
      }
      walk.lines.push(line);
    }
    walk.logLen = ext.offset + ext.len;
    walk.read = true;
    served("/records", true);
    return verdict(problems, `${plural(lines.length, "record")} in ${ext.len} bytes`);
  });
  const records = () => [...(walk.committed ?? []), ...(walk.ok ? walk.pull.provisional : [])];
  // The committed records' count: those before the pointer and those it points at.
  const committedCount = () => (walk.ok ? walk.pull.committed_from + (walk.committed?.length ?? 0) : 0);
  const haveLog = () => walk.ok && walk.ext && walk.committed?.length > 0 && walk.pull.committed_from === 0;

  await rule("records.end-carried", S.log, "an end inside a record is carried on to that record's end", async () => {
    if (!haveLog()) return walk.ok ? skip("no committed record to read") : noWalk();
    const reply = await ask("GET", `records?session=${enc(sid)}&from=0&len=1&epoch=${walk.epoch}`);
    if (reply.status !== 200) fail(`${reply.what} answered ${reply.status}`);
    const got = splitLines(reply.bytes);
    if (got.length !== 1 || !got[0].equals(walk.lines[0])) {
      fail(`${reply.what} answered ${plural(got.length, "line")}, not exactly record 0 (${walk.lines[0].length} bytes)`);
    }
    return `1 byte asked, record 0 (${walk.lines[0].length} bytes) answered`;
  });

  await rule("records.not-a-start", S.log, "a from that is not where a record starts gets 400", async () => {
    if (!haveLog()) return walk.ok ? skip("no committed record to read") : noWalk();
    const reply = await ask("GET", `records?session=${enc(sid)}&from=1&len=16&epoch=${walk.epoch}`);
    if (reply.status !== 400) fail(`${reply.what} answered ${reply.status}`);
    return "from=1: 400";
  });

  await rule("records.stale", S.log, "a pointer from another epoch gets 409", async () => {
    if (!haveLog()) return walk.ok ? skip("no committed record to read") : noWalk();
    const reply = await ask("GET", `records?session=${enc(sid)}&from=0&len=1&epoch=${walk.epoch + 1}`);
    if (reply.status !== 409) fail(`${reply.what} answered ${reply.status}`);
    return `epoch ${walk.epoch + 1}: 409`;
  });

  await rule("pull.own-cursor", S.pull, "a pull at the reader's own cursor keeps the epoch, cuts nothing committed, and sends no meta when nothing changed", async () => {
    if (!walk.ok || !walk.read) return noWalk();
    const p = walk.pull;
    const cursor = `${p.epoch}.${committedCount()}.${p.provisional_gen}.${p.provisional_from + p.provisional.length}`;
    const reply = await ask("GET", `pull?session=${enc(sid)}&cursor=${cursor}`);
    if (reply.status !== 200) fail(`${reply.what} answered ${reply.status}`);
    const body = reply.json();
    const problems = [];
    if (!fields(body, PULL, "the reply", problems)) return verdict(problems);
    if (body.epoch !== p.epoch) return skip(`the epoch moved (${p.epoch} to ${body.epoch}): the log was made again between the pulls`);
    if (body.committed_from !== committedCount()) problems.push(`committed_from is ${body.committed_from} where the reader holds ${committedCount()}: within one epoch the committed zone only grows`);
    const quiet = body.committed_ext === null && body.provisional.length === 0 && body.provisional_gen === p.provisional_gen;
    if (!quiet) return problems.length ? verdict(problems) : skip("the session grew between the two pulls");
    if (body.meta !== null) problems.push(`meta is ${show(body.meta)}, not null, with nothing changed`);
    return verdict(problems, `cursor ${cursor}: nothing new, meta null`);
  });

  await rule("pull.tail", S.pull, "tail=<bytes> is honoured only on a log over twice that, from the first record starting inside it, and the head reads back as the records before it", async () => {
    if (!haveLog()) return walk.ok ? skip("no committed log to take a tail of") : noWalk();
    const n = walk.committed.length;
    const L = walk.logLen;
    if (n < 2) return skip("the log holds one record");
    const s = walk.offsets;
    // One budget under half the log, one at half: the first may be honoured, the second never.
    const cases = [Math.floor((L - 1) / 2), Math.ceil(L / 2)];
    if (L - s[n - 1] < L / 2) cases.push(L - s[n - 1]);
    const problems = [];
    const said = [];
    for (const tail of cases) {
      const k = s.findIndex((o) => o >= L - tail);
      const honoured = L > 2 * tail && k > 0 && k < n;
      const reply = await ask("GET", `pull?session=${enc(sid)}&cursor=0.0.0.0&tail=${tail}`);
      if (reply.status !== 200) {
        problems.push(`${reply.what} answered ${reply.status}`);
        continue;
      }
      const body = reply.json();
      if (body.epoch !== walk.epoch) return skip("the epoch moved between the pulls");
      if (!honoured) {
        if (body.committed_from !== 0) problems.push(`tail=${tail} on a ${L}-byte log should not be honoured, but committed_from is ${body.committed_from}`);
        said.push(`tail=${tail}: whole`);
        continue;
      }
      const ext = body.committed_ext;
      if (body.committed_from !== k) problems.push(`tail=${tail}: committed_from is ${body.committed_from}, not ${k} (the first record starting in the last ${tail} bytes)`);
      if (!ext || ext.offset !== s[k] || ext.offset + ext.len !== L) problems.push(`tail=${tail}: committed_ext is ${show(ext)}, not {offset: ${s[k]}, len: ${L - s[k]}}`);
      const head = await ask("GET", `records?session=${enc(sid)}&from=0&len=${s[k]}&epoch=${walk.epoch}`);
      const lines = head.status === 200 ? splitLines(head.bytes) : [];
      if (head.status !== 200) problems.push(`${head.what} answered ${head.status}`);
      else if (lines.length !== k || lines.some((line, i) => !line.equals(walk.lines[i]))) problems.push(`the head ${head.what} read ${plural(lines.length, "record")}, not records 0 to ${k - 1}`);
      said.push(`tail=${tail}: from record ${k}`);
    }
    if (!said.some((x) => x.includes("from record"))) said.push("no budget the log allows was honoured");
    if (problems.length && (await grew())) return skip(GREW);
    return verdict(problems, said.join(", "));
  });

  // ------------------------------------------------------------------------ the turn index
  const ti = { served: null };
  await rule("turn-index.reply", S.turnIndex, "the reply is {t: \"turn-index\", epoch, upto, records, bytes, turns, compactions, tools, artifacts, files, facts, agents}", async () => {
    if (!haveLog()) return walk.ok ? skip("no committed log to map") : noWalk();
    const upto = committedCount();
    const reply = await ask("GET", `turn-index?session=${enc(sid)}&epoch=${walk.epoch}&upto=${upto}`);
    if (reply.status === 404) {
      ti.served = false;
      served("/turn-index", false);
      return skip("404: not served (the page reads the head whole)");
    }
    if (reply.status !== 200) fail(`${reply.what} answered ${reply.status}: ${brief(reply.text())}`);
    const body = reply.json();
    const problems = [];
    fields(
      body,
      {
        t: oneOf("turn-index"),
        epoch: "number",
        upto: "number",
        records: "number",
        bytes: "number",
        turns: "array",
        compactions: "array",
        tools: "object",
        artifacts: "array",
        files: "array",
        facts: "array",
        agents: "array",
      },
      "the reply",
      problems,
    );
    verdict(problems);
    if (body.epoch !== walk.epoch) problems.push(`epoch is ${body.epoch}, not ${walk.epoch}`);
    if (body.upto !== upto) problems.push(`upto is ${body.upto}, not ${upto} as asked`);
    for (const [name, count] of Object.entries(body.tools)) if (!TYPES.number(count)) problems.push(`tools.${name} is ${show(count)}, not a count`);
    ti.served = true;
    ti.body = body;
    served("/turn-index", true);
    // The log "now": the walk's, unless the session grew since.
    const now = [];
    if (body.records !== committedCount()) now.push(`records is ${body.records} where the log holds ${committedCount()}`);
    if (body.bytes !== walk.logLen) now.push(`bytes is ${body.bytes} where the log is ${walk.logLen} bytes`);
    if (now.length && !problems.length && (await grew())) {
      ti.grew = true;
      return skip(GREW);
    }
    return verdict([...problems, ...now], `${plural(body.turns.length, "turn")} below ${upto}`);
  });

  await rule("turn-index.facts", S.turnIndex, "facts holds one [kind, id] per record below upto, [kind, id, 1] for an assistant's commentary", async () => {
    if (!ti.body) return skip("no turn index to read");
    const facts = ti.body.facts;
    const problems = [];
    if (facts.length !== ti.body.upto) problems.push(`${plural(facts.length, "fact")} for ${ti.body.upto} records`);
    for (const [i, f] of facts.entries()) {
      const rec = walk.committed[i];
      if (!Array.isArray(f) || f.length < 2 || f.length > 3) {
        problems.push(`facts[${i}] is ${show(f)}`);
        continue;
      }
      if (!rec) continue;
      const commentary = rec.kind === "assistant" && rec.phase === "commentary";
      if (f[0] !== rec.kind || f[1] !== rec.id) problems.push(`facts[${i}] is ${show(f)} where record ${i} is ${show([rec.kind, rec.id])}`);
      if (commentary !== (f[2] === 1)) problems.push(`facts[${i}] is ${show(f)} for ${commentary ? "" : "no "}commentary`);
      if (f.length === 3 && f[2] !== 1) problems.push(`facts[${i}][2] is ${show(f[2])}, not 1`);
    }
    return verdict(problems, plural(facts.length, "fact"));
  });

  await rule("turn-index.turns", S.turnIndex, "every turn below upto, each with its opener's index (r, last) and its byte span (o, end) in the log", async () => {
    if (!ti.body) return skip("no turn index to read");
    const turns = ti.body.turns;
    const problems = [];
    const s = walk.offsets;
    const L = walk.logLen;
    const n = walk.committed.length;
    const opens = walk.committed.map((rec, i) => (typeof rec.id === "string" && rec.id.startsWith("t") ? i : -1)).filter((i) => i >= 0);
    const listed = [];
    for (const [i, t] of turns.entries()) {
      if (!fields(t, TURN, `turns[${i}]`, problems)) continue;
      listed.push(t.r);
      const rec = walk.committed[t.r];
      if (!rec) {
        problems.push(`turns[${i}].r is ${t.r}, past the log's ${n} records`);
        continue;
      }
      if (t.kind !== rec.kind || t.id !== rec.id || t.turn !== rec.turn) {
        problems.push(`turns[${i}] says ${show([t.kind, t.id, t.turn])} where record ${t.r} is ${show([rec.kind, rec.id, rec.turn])}`);
      }
      if (t.last < t.r || t.last >= n) problems.push(`turns[${i}].last is ${t.last} for r ${t.r} in ${n} records`);
      if (t.o !== s[t.r]) problems.push(`turns[${i}].o is ${t.o} where record ${t.r} starts at byte ${s[t.r]}`);
      const end = t.last + 1 < n ? s[t.last + 1] : L;
      if (t.end !== end) problems.push(`turns[${i}].end is ${t.end} where record ${t.last} ends at byte ${end}`);
    }
    const missing = opens.filter((r) => !listed.includes(r));
    if (missing.length) problems.push(`records ${missing.join(", ")} open turns the index does not list`);
    // The bridge: a turn's byte span reads back as its records.
    const first = turns.find((t) => TYPES.object(t) && TYPES.number(t.o) && TYPES.number(t.end) && t.end > t.o);
    if (first && !problems.length) {
      const reply = await ask("GET", `records?session=${enc(sid)}&from=${first.o}&len=${first.end - first.o}&epoch=${walk.epoch}`);
      const lines = reply.status === 200 ? splitLines(reply.bytes) : [];
      const want = walk.lines.slice(first.r, first.last + 1);
      if (reply.status !== 200 || lines.length !== want.length || lines.some((l, i) => !l.equals(want[i]))) {
        problems.push(`turn ${first.turn}'s span ${reply.what} did not read back as records ${first.r} to ${first.last}`);
      }
    }
    // The last turn runs to the log's end, which moves when the session grows.
    if (problems.length && (ti.grew || (await grew()))) return skip(GREW);
    return verdict(problems, `${plural(turns.length, "turn")}, each at its records and bytes`);
  });

  await rule("turn-index.parts", S.turnIndex, "compactions, artifacts, files and agents carry their fields, each at a record below upto", async () => {
    if (!ti.body) return skip("no turn index to read");
    const b = ti.body;
    const problems = [];
    const below = (r, where) => {
      if (!TYPES.number(r) || r < 0 || r >= b.upto) problems.push(`${where} is ${show(r)}, not a record below ${b.upto}`);
    };
    for (const [i, c] of b.compactions.entries()) {
      if (fields(c, { r: "number", head: "object" }, `compactions[${i}]`, problems)) {
        fields(c.head, { compact_trigger: "any" }, `compactions[${i}].head`, problems);
        below(c.r, `compactions[${i}].r`);
        if (walk.committed[c.r]?.kind !== "compaction") problems.push(`compactions[${i}].r is record ${c.r}, a ${show(walk.committed[c.r]?.kind)}`);
      }
    }
    for (const [i, a] of b.artifacts.entries()) if (fields(a, { ...ARTIFACT, at: "number" }, `artifacts[${i}]`, problems)) below(a.at, `artifacts[${i}].at`);
    for (const [i, f] of b.files.entries()) if (fields(f, { path: "string", fsig: "any", sig: "any", r: "number" }, `files[${i}]`, problems)) below(f.r, `files[${i}].r`);
    for (const [i, a] of b.agents.entries()) if (fields(a, { id: "any", r: "number" }, `agents[${i}]`, problems)) below(a.r, `agents[${i}].r`);
    const counted = b.compactions.length + b.artifacts.length + b.files.length + b.agents.length;
    return verdict(problems, counted ? `${counted} entries` : "none in this session");
  });

  await rule("turn-index.upto", S.turnIndex, "upto is as asked, at most the record count", async () => {
    if (!ti.body) return skip("no turn index to read");
    const asked = ti.body.records + 5;
    const reply = await ask("GET", `turn-index?session=${enc(sid)}&epoch=${walk.epoch}&upto=${asked}`);
    if (reply.status !== 200) fail(`${reply.what} answered ${reply.status}`);
    const body = reply.json();
    // Read against the record count in the same reply, so a session that grew meanwhile still reads.
    const most = TYPES.number(body.records) ? Math.min(asked, body.records) : ti.body.records;
    if (body.upto !== most) fail(`asked for ${asked}, upto is ${show(body.upto)}, not the record count ${most}`);
    return `asked ${asked}, answered ${body.upto}`;
  });

  await rule("turn-index.stale", S.turnIndex, "another epoch gets 409", async () => {
    if (!ti.body) return skip("no turn index to read");
    const reply = await ask("GET", `turn-index?session=${enc(sid)}&epoch=${walk.epoch + 1}&upto=1`);
    if (reply.status !== 409) fail(`${reply.what} answered ${reply.status}`);
    return "409";
  });

  // ----------------------------------------------------------------------------- search
  const se = {};
  const searchFor = (q, epoch = walk.epoch, upto = committedCount()) =>
    ask("GET", `search?session=${enc(sid)}&epoch=${epoch}&upto=${upto}&q=${enc(typeof q === "string" ? q : JSON.stringify(q))}`);

  await rule("search.reply", S.search, "the reply is {t: \"search\", epoch, upto, records: [[recordIndex, hits]], counts: {u, a, t, o}, total}", async () => {
    if (!haveLog()) return walk.ok ? skip("no committed log to search") : noWalk();
    const prompt = walk.committed.find((r) => r.kind === "user");
    const words = textOf((prompt?.body ?? []).map((p) => p?.h ?? p?.x ?? "").join(" ")).toLowerCase().match(/[a-z]{4,}/g);
    const needle = words?.[0] ?? "the";
    const reply = await searchFor({ lc: needle, whole: false, wanted: 0, tools: [] });
    if (reply.status === 404) {
      se.served = false;
      served("/search", false);
      return skip("404: not served (the page searches what it holds)");
    }
    if (reply.status !== 200) fail(`${reply.what} answered ${reply.status}: ${brief(reply.text())}`);
    const body = reply.json();
    const problems = [];
    fields(body, { t: oneOf("search"), epoch: "number", upto: "number", records: "array", counts: "object", total: "number" }, "the reply", problems);
    verdict(problems);
    fields(body.counts, { u: "number", a: "number", t: "number", o: "number" }, "counts", problems);
    for (const [i, hit] of body.records.entries()) {
      if (!Array.isArray(hit) || hit.length !== 2 || !TYPES.number(hit[0]) || !TYPES.number(hit[1])) problems.push(`records[${i}] is ${show(hit)}, not [recordIndex, hits]`);
      else if (hit[0] < 0 || hit[0] >= body.upto) problems.push(`records[${i}] names record ${hit[0]}, not one below ${body.upto}`);
    }
    se.body = body;
    served("/search", true);
    return verdict(problems, `"${needle}": ${plural(body.records.length, "record")}, total ${body.total}`);
  });

  await rule("search.no-text", S.search, "the reply quotes no text", async () => {
    if (!se.body) return skip("no search reply to read");
    const strings = [];
    const walkJson = (v) => {
      if (typeof v === "string") strings.push(v);
      else if (Array.isArray(v)) v.forEach(walkJson);
      else if (TYPES.object(v)) Object.values(v).forEach(walkJson);
    };
    walkJson(se.body);
    const quoted = strings.filter((x) => x !== "search");
    if (quoted.length) fail(`it carries ${plural(quoted.length, "string")}: ${quoted.slice(0, 3).map(brief).join(", ")}`);
    return "no string but its t";
  });

  await rule("search.tool-filter", S.search, "an empty lc with tools is the tool filter: each record kept has 1 hit, and counts stay 0", async () => {
    if (!se.body) return skip("no search reply to read");
    const name = Object.keys(ti.body?.tools ?? {})[0] ?? records().map((r) => r.tool).find((t) => typeof t === "string");
    if (!name) return skip("the session holds no tool call to filter on");
    const reply = await searchFor({ lc: "", whole: false, wanted: 0, tools: [{ name, prefix: false }] });
    if (reply.status !== 200) fail(`${reply.what} answered ${reply.status}: ${brief(reply.text())}`);
    const body = reply.json();
    const problems = [];
    if (!Array.isArray(body.records)) fail(`records is ${show(body.records)}`);
    for (const hit of body.records) if (!Array.isArray(hit) || hit[1] !== 1) problems.push(`record ${show(hit?.[0])} has ${show(hit?.[1])} hits, not 1`);
    for (const k of ["u", "a", "t", "o"]) if (body.counts?.[k] !== 0) problems.push(`counts.${k} is ${show(body.counts?.[k])}, not 0`);
    return verdict(problems, `${name}: ${plural(body.records.length, "record")} kept`);
  });

  await rule("search.bad-query", S.search, "a query it cannot read gets 400", async () => {
    if (!se.body) return skip("no search reply to read");
    const reply = await searchFor("not a query");
    if (reply.status !== 400) fail(`${reply.what} answered ${reply.status}`);
    return "400";
  });

  await rule("search.stale", S.search, "another epoch gets 409", async () => {
    if (!se.body) return skip("no search reply to read");
    const reply = await searchFor({ lc: "tern", whole: false, wanted: 0, tools: [] }, walk.epoch + 1, 1);
    if (reply.status !== 409) fail(`${reply.what} answered ${reply.status}`);
    return "409";
  });

  // ---------------------------------------------------------------------- the record format
  const all = walk.ok ? [...everyRecord(records())] : [];
  const noRecords = () => (walk.ok ? skip("the session holds no record") : noWalk());

  await rule("record.fields", S.every, "every record is {t: \"block\", kind, id, ...} with each field of the table its type", async () => {
    if (!all.length) return noRecords();
    const problems = [];
    for (const [rec, where] of all) {
      if (!fields(rec, RECORD, where, problems)) continue;
      if (!/^[bt]\d+$/.test(rec.id)) problems.push(`${where}.id is ${show(rec.id)}, not b<n> or t<n>`);
      if (TYPES.object(rec.head) && !Object.keys(rec.head).length) problems.push(`${where}.head is empty: it is left out when empty`);
      if (typeof rec.label === "string" && [...rec.label].length > 80) problems.push(`${where}.label is ${[...rec.label].length} characters, over 80`);
      if (rec.kind === "pending") problems.push(`${where} is a pending record, which a server never sends`);
      if (TYPES.object(rec.reply_context) && !TYPES.number(rec.reply_context.version)) problems.push(`${where}.reply_context.version is ${show(rec.reply_context.version)}`);
      if (TYPES.object(rec.inbox) && !(typeof rec.inbox.from === "string" && rec.inbox.from)) problems.push(`${where}.inbox.from is ${show(rec.inbox.from)}, never empty`);
    }
    return verdict(problems, plural(all.length, "record"));
  });

  await rule("record.turns", S.every, "a record that opens turn n has the id t<n>, its turn and its label", async () => {
    if (!all.length) return noRecords();
    const problems = [];
    let opened = 0;
    for (const [rec, where] of all) {
      if (!TYPES.object(rec) || typeof rec.id !== "string") continue;
      const opens = rec.id.startsWith("t");
      if (opens) {
        opened++;
        if (rec.id !== `t${rec.turn}`) problems.push(`${where} is ${rec.id} with turn ${show(rec.turn)}`);
        if (typeof rec.label !== "string") problems.push(`${where} opens a turn with label ${show(rec.label)}`);
      } else if ("turn" in rec) problems.push(`${where} is ${rec.id} but carries turn ${show(rec.turn)}`);
    }
    if (!opened) return verdict(problems, skip("no record opens a turn"));
    return verdict(problems, plural(opened, "turn"));
  });

  await rule("record.kinds", S.kinds, "each kind carries the head the kinds table gives it", async () => {
    if (!all.length) return noRecords();
    const problems = [];
    const seen = new Set();
    for (const [rec, where] of all) {
      if (!TYPES.object(rec)) continue;
      seen.add(rec.kind);
      let spec = KIND_HEADS[rec.kind];
      if (rec.kind === "tool") spec = "tool" in rec ? CALL_HEAD : { name: "any", target: "any" };
      if (TOOL_KINDS.includes(rec.kind) && typeof rec.tool !== "string") problems.push(`${where} is a ${rec.kind} call with no tool name`);
      if (spec && Object.keys(spec).some((k) => !k.endsWith("?"))) fields(rec.head, spec, `${where}.head`, problems);
      else if (spec && TYPES.object(rec.head)) fields(rec.head, spec, `${where}.head`, problems);
      const body = Array.isArray(rec.body) ? rec.body : [];
      if (rec.kind === "attachment" && body.length) problems.push(`${where} is an attachment with ${plural(body.length, "body part")}: its body is empty`);
      if (rec.kind === "act") {
        // "blocks (the calls, as records), then think": the calls come first.
        const blocks = body.findIndex((p) => p?.p === "blocks");
        const think = body.findIndex((p) => p?.p === "think");
        if (blocks < 0) problems.push(`${where} is an act with no blocks part`);
        else if (think >= 0 && think < blocks) problems.push(`${where} is an act whose think comes before its blocks`);
      }
    }
    return verdict(problems, `kinds ${[...seen].sort().join(", ")}`);
  });

  await rule("record.parts", S.parts, "every body part is {p, ...} with the fields its row gives, and blocks only inside act", async () => {
    if (!all.length) return noRecords();
    const problems = [];
    let count = 0;
    for (const [rec, where] of all) {
      for (const [j, part] of (Array.isArray(rec?.body) ? rec.body : []).entries()) {
        count++;
        const at = `${where}.body[${j}]`;
        if (!fields(part, { p: "string" }, at, problems)) continue;
        const spec = PARTS[part.p];
        if (!spec || !fields(part, spec, at, problems)) continue;
        if (part.p === "blocks" && rec.kind !== "act") problems.push(`${at} is blocks inside a ${show(rec.kind)}`);
        if (part.p === "num") {
          for (const row of part.rows) if (!Array.isArray(row) || row.length !== 2 || typeof row[1] !== "string") problems.push(`${at} has row ${show(row)}, not [line, html]`);
        }
        if (part.p === "diff") {
          for (const row of part.rows) {
            if (!Array.isArray(row) || row.length !== 3 || !["ctx", "add", "del"].includes(row[0]) || !(row[1] === null || TYPES.number(row[1])) || typeof row[2] !== "string") problems.push(`${at} has row ${show(row)}, not [ctx|add|del, line or null, text]`);
          }
        }
        if (part.p === "ctx") {
          for (const [k, r] of part.rows.entries()) fields(r, { label: "any", tokens: "any", percent: "any" }, `${at}.rows[${k}]`, problems);
          for (const [k, r] of part.extras.entries()) fields(r, { label: "any", value: "any" }, `${at}.extras[${k}]`, problems);
        }
        if (part.p === "note" && "file" in part) fields(part.file, { ...OFFER, name: "any" }, `${at}.file`, problems);
      }
    }
    return verdict(problems, plural(count, "part"));
  });

  await rule("record.chips", S.chips, "a chip is {x, c?}, c one of add, del, fail or sandbox", async () => {
    if (!all.length) return noRecords();
    const problems = [];
    let count = 0;
    for (const [rec, where] of all) {
      const chips = rec?.head?.chips;
      if (chips === undefined) continue;
      if (!Array.isArray(chips)) {
        problems.push(`${where}.head.chips is ${show(chips)}`);
        continue;
      }
      for (const [k, chip] of chips.entries()) {
        count++;
        fields(chip, CHIP, `${where}.head.chips[${k}]`, problems);
      }
    }
    if (!count && !problems.length) return skip("no record carries a chip");
    return verdict(problems, plural(count, "chip"));
  });

  await rule("record.shapes", S.shapes, "offered paths, question cards, artifacts and sub-agent links carry their fields", async () => {
    if (!all.length) return noRecords();
    const problems = [];
    let count = 0;
    for (const [rec, where] of all) {
      const head = rec?.head;
      if (!TYPES.object(head)) continue;
      const at = `${where}.head`;
      if ("sig" in head || "fsig" in head || "path" in head) {
        count++;
        fields(head, OFFER, at, problems);
      }
      if ("att_sig" in head || "att_fsig" in head) {
        count++;
        if (typeof head.att_path !== "string") problems.push(`${at} stamps att_path ${show(head.att_path)}`);
      }
      if ("files" in head) {
        if (!Array.isArray(head.files)) problems.push(`${at}.files is ${show(head.files)}`);
        else head.files.forEach((f, k) => (count++, fields(f, OFFER, `${at}.files[${k}]`, problems)));
      }
      if ("interaction" in head) {
        count++;
        if (fields(head.interaction, INTERACTION, `${at}.interaction`, problems)) {
          head.interaction.answers.forEach((a, k) => fields(a, { id: "any", label: "any" }, `${at}.interaction.answers[${k}]`, problems));
          if ("unanswered" in head.interaction) fields(head.interaction.unanswered, { why: oneOf("timeout", "declined", "failed", "none") }, `${at}.interaction.unanswered`, problems);
        }
      }
      if ("artifact" in head) {
        count++;
        fields(head.artifact, ARTIFACT, `${at}.artifact`, problems);
      }
      if ("child" in head || "child_id" in head) {
        count++;
        const child = typeof head.child === "string" ? head.child : "";
        let named = null;
        try {
          named = child.startsWith("?session=") ? decodeURIComponent(child.slice("?session=".length)) : null;
        } catch {}
        if (named === null || named !== head.child_id) problems.push(`${at}.child is ${show(head.child)} for child_id ${show(head.child_id)}, not ?session=<child_id>`);
      }
    }
    if (!count && !problems.length) return skip("no record carries one");
    return verdict(problems, plural(count, "shape"));
  });

  await rule("pull.sub-agent", S.pull, "a sub-agent's records are pulled the same way under its id, once its parent has been pulled", async () => {
    if (!all.length) return noRecords();
    const ids = [...new Set(all.map(([r]) => r?.head?.child_id).filter((x) => typeof x === "string"))];
    if (!ids.length) return skip("the session starts no sub-agent");
    // One sub-agent of the session read, and no other session.
    const reply = await ask("GET", `pull?session=${enc(ids[0])}&cursor=0.0.0.0`);
    const body = reply.status === 200 ? reply.json() : null;
    if (!body || (body.t !== "pull" && body.t !== "redirect")) fail(`${reply.what} answered ${reply.status}`);
    return `the first of ${plural(ids.length, "sub-agent")} pulled`;
  });

  // ----------------------------------------------------------------------- the facts (meta)
  await rule("meta.fields", S.meta, "the session's facts carry the meta table's fields, for this session", async () => {
    if (!walk.ok) return noWalk();
    const m = walk.pull.meta;
    if (!TYPES.object(m)) return skip("the fresh pull carried no meta");
    const problems = [];
    if (!fields(m, META, "meta", problems)) return verdict(problems);
    if (m.sid !== sid) problems.push(`meta.sid is ${show(m.sid)}, not ${show(sid)}`);
    if (fields(m.usage, USAGE_FIELDS, "meta.usage", problems)) {
      if ("reported" in m.usage) fields(m.usage.reported, REPORTED, "meta.usage.reported", problems);
    }
    m.tasks.forEach((t, i) => fields(t, TASK, `meta.tasks[${i}]`, problems));
    m.ancestors.forEach((a, i) => fields(a, { id: "any", title: "any" }, `meta.ancestors[${i}]`, problems));
    m.children.forEach((c, i) => fields(c, { id: "any", title: "any", type: "any", running: "any" }, `meta.children[${i}]`, problems));
    for (const [i, run] of (m.runs ?? []).entries()) {
      if (fields(run, { run: "any", members: "array" }, `meta.runs[${i}]`, problems)) {
        run.members.forEach((x, k) => fields(x, { id: "any", title: "any", running: "any", phase: "any" }, `meta.runs[${i}].members[${k}]`, problems));
      }
    }
    return verdict(problems, `${show(m.title)}, ${m.turns} turns, ${m.tools} tools, ${plural(m.tasks.length, "task")}`);
  });

  // -------------------------------------------------------------------------------- files
  const offered = all.flatMap(([rec]) => offers(rec)).filter((o) => typeof o.fsig === "string" && o.fsig);
  // The path /file served whole, which mdrev's open must then open too.
  let fileOpened = null;
  await rule("file.offered", S.file, "an offered path with its file stamp answers a status the table lists, and every 200 carries nosniff and the sandbox policy", async () => {
    if (!walk.ok) return noWalk();
    if (!offered.length) return skip("the session offers no path with a file stamp");
    const o = offered[0];
    // A download is not read: its headers say what the rule asks, and it may be large.
    const reply = await ask("GET", `file?path=${enc(o.path)}&sig=${enc(o.fsig)}`, { noDownload: true });
    if (reply.status === 404) {
      served("/file", false);
      return skip("404: not served (a path can only be copied), or the path was never offered");
    }
    served("/file", true);
    if ([401, 403, 410, 413, 415].includes(reply.status)) return `${reply.status}: ${brief(reply.text())}`;
    if (reply.status !== 200) fail(`${reply.what} answered ${reply.status}`);
    fileOpened = o.path;
    const raw = reply.header("content-type") ?? "";
    const type = mediaType(raw);
    const problems = [];
    const image = /^image\/(png|jpeg|gif|webp|bmp|x-icon|avif)$/.test(type);
    const text = type === "text/plain" && /;\s*charset\s*=\s*"?utf-8"?\s*(;|$)/i.test(raw);
    const download = type === "application/octet-stream";
    if (!image && !text && !download) problems.push(`its type is ${show(raw)}`);
    if (download && !/^attachment\b/i.test(reply.header("content-disposition") ?? "")) problems.push("a download without Content-Disposition: attachment");
    if (text && !isUtf8(reply.bytes)) problems.push("served as text but its bytes are not UTF-8");
    if ((reply.header("x-content-type-options") ?? "").toLowerCase() !== "nosniff") problems.push(`X-Content-Type-Options is ${show(reply.header("x-content-type-options"))}`);
    if (!/(^|[;,]\s*)sandbox\b/i.test(reply.header("content-security-policy") ?? "")) problems.push(`Content-Security-Policy is ${show(reply.header("content-security-policy"))}`);
    return verdict(problems, `200 ${raw}`);
  });

  await rule("file.not-offered", S.file, "no path, or a path with a stamp nobody minted, gets 404 (401 when not paired)", async () => {
    const problems = [];
    const path = offered[0]?.path ?? "/conform-check/no-such-file";
    for (const q of ["", `?path=${enc(path)}&sig=conform-forged`]) {
      const reply = await ask("GET", `file${q}`);
      if (reply.status !== 404 && reply.status !== 401) problems.push(`${reply.what} answered ${reply.status}`);
    }
    return verdict(problems, "refused");
  });

  await rule("reveal.forged", S.reveal, "a reveal without a stamp this server minted gets 404", async () => {
    // Never a real stamp: a reveal opens the file manager on the server's machine.
    const problems = [];
    for (const q of ["", `?path=${enc(`/conform-check/no-such-file-${NONCE}`)}&sig=conform-forged`]) {
      const reply = await ask("GET", `__reveal${q}`);
      if (reply.status !== 404) problems.push(`${reply.what} answered ${reply.status}`);
    }
    return verdict(problems, "404 (a stamp nobody minted, for a path that does not exist)");
  });

  // --------------------------------------------------------------------------------- mdrev
  let mdrevVersion = page?.mdrev || null;
  if (!mdrevVersion) {
    const md = await ask("GET", "markdown").catch(() => null);
    if (md?.status === 200) mdrevVersion = readPage(md.text()).mdrev || null;
  }
  await rule("mdrev.bundle", S.mdrev, "mdrev's bundle is served under /mdrev/<version>/, the version the page names", async () => {
    if (!mdrevVersion) return skip("neither the page nor /markdown names an mdrev version (data-mdrev)");
    const reply = await ask("GET", `mdrev/${enc(mdrevVersion)}/mdrev.js`);
    if (reply.status === 404) {
      served("/mdrev/<mdrev>/", false);
      return skip("404: not served (Markdown shows as text)");
    }
    if (reply.status !== 200 || !reply.bytes.length) fail(`${reply.what} answered ${reply.status}`);
    served("/mdrev/<mdrev>/", true);
    return `mdrev ${mdrevVersion}`;
  });

  const held = {};
  await rule("mdrev.hold", S.mdrev, "POST hold with {name, text} answers {root: \"held\", path, cap, name}", async () => {
    const reply = await ask("POST", "api/mdrev/hold", {
      body: JSON.stringify({ name: "conform.md", text: "# Conformance\n\nA document the check holds.\n" }),
      headers: { "content-type": "application/json" },
    });
    if (reply.status === 404) {
      served("/api/mdrev/", false);
      return skip("404: not served (the pane shows text plainly)");
    }
    if (reply.status !== 200) fail(`${reply.what} answered ${reply.status}: ${brief(reply.text())}${originHint(reply)}`);
    const body = reply.json();
    const problems = [];
    fields(body, { root: oneOf("held"), path: "string", cap: "string", name: "string" }, "the reply", problems);
    verdict(problems);
    Object.assign(held, body);
    served("/api/mdrev/", true);
    return `held as ${show(body.path)}`;
  });

  await rule("mdrev.origin", S.mdrev, "every mdrev route needs the page's own origin (403)", async () => {
    if (!held.path) return skip("hold is not served");
    // No body: a server that refuses before reading one may reset a connection that still holds it.
    const reply = await ask("POST", "api/mdrev/hold", { headers: { origin: FOREIGN } });
    if (reply.status !== 403) fail(`${reply.what} from ${FOREIGN} answered ${reply.status}`);
    return "403";
  });

  await rule("mdrev.resolve", S.mdrev, "POST resolve?root= with {from: {path, cap}, targets} answers {caps}", async () => {
    if (!held.path) return skip("hold is not served");
    const reply = await ask("POST", `api/mdrev/resolve?root=${enc(held.root)}`, {
      body: JSON.stringify({ from: { path: held.path, cap: held.cap }, targets: ["other.md"] }),
      headers: { "content-type": "application/json" },
    });
    if (reply.status === 404) return skip(`404: not served (${brief(reply.text())})`);
    if (reply.status !== 200) fail(`${reply.what} answered ${reply.status}: ${brief(reply.text())}`);
    const body = reply.json();
    if (!("caps" in body)) fail(`the reply has no caps: ${brief(reply.text())}`);
    return "caps";
  });

  await rule("mdrev.open", S.mdrev, "GET open?path=&sig= for an offered file answers {root, path, cap, isGit, name, review, annotate}, or 401, 404 or 415", async () => {
    if (!held.path) return skip("mdrev's routes are not served");
    if (!offered.length) return skip("the session offers no path with a file stamp");
    const o = offered[0];
    const reply = await ask("GET", `api/mdrev/open?path=${enc(o.path)}&sig=${enc(o.fsig)}`);
    if ([401, 404, 415].includes(reply.status)) return skip(`${reply.status}: ${brief(reply.text())}`);
    if (reply.status !== 200) fail(`${reply.what} answered ${reply.status}: ${brief(reply.text())}`);
    const body = reply.json();
    const problems = [];
    fields(body, { root: "any", path: "any", cap: "any", isGit: "any", name: "any", review: "any", annotate: "any" }, "the reply", problems);
    return verdict(problems, `opened ${show(body.name)}`);
  });

  await rule("mdrev.open-forged", S.mdrev, "open for a path the page was not offered gets 404 (401 when not paired)", async () => {
    if (!held.path) return skip("mdrev's routes are not served");
    const reply = await ask("GET", `api/mdrev/open?path=${enc(offered[0]?.path ?? "/conform-check/no-such-file.md")}&sig=conform-forged`);
    if (reply.status !== 404 && reply.status !== 401) fail(`${reply.what} answered ${reply.status}`);
    return `${reply.status}`;
  });

  await rule("mdrev.no-review", S.mdrev, "shared review answers 404 under /api/mdrev/", async () => {
    const reply = await ask("GET", "api/mdrev/review");
    if (reply.status !== 404) fail(`${reply.what} answered ${reply.status}`);
    return "404";
  });

  await rule("mermaid.runtime", S.mermaid, "the Mermaid runtime the page head names is served, joined to the base", async () => {
    if (!page) return skip("the base serves no page to name the runtime");
    if (!page.mermaid) return skip("the page names no runtime (<meta name=\"mermaid-runtime\">)");
    // The page joins the path to its base; one that leaves the base is never asked for, since the
    // reader's sign-in would go with it.
    if (!underBase(page.mermaid)) fail("the page names a runtime outside the base: nothing was sent there");
    const reply = await ask("GET", page.mermaid);
    if (reply.status === 404) {
      served("/mermaid/", false);
      return skip("404: not served (a diagram shows as its source)");
    }
    if (reply.status !== 200 || !reply.bytes.length) fail(`${reply.what} answered ${reply.status}`);
    served("/mermaid/", true);
    return `${reply.what}: ${reply.bytes.length} bytes`;
  });

  // ------------------------------------------------------------------- hide and arrange
  await rule("ignore.no-key", S.ignore, "with no key the reply is 200 {ok: false}", async () => {
    const reply = await ask("GET", "api/ignore");
    if (reply.status === 404) {
      served("/api/ignore", false);
      return skip("404: not served (hide and restore do nothing)");
    }
    if (reply.status !== 200) fail(`${reply.what} answered ${reply.status}`);
    const body = reply.json();
    if (body.ok !== false) fail(`the reply is ${brief(reply.text())}`);
    served("/api/ignore", true);
    return "{ok: false}";
  });

  await rule("ignore.add-remove", S.ignore, "add and remove answer 200 {ok: true, ignored: <count>}", async () => {
    const row = ROUTES.find((r) => r.path === "/api/ignore");
    if (row.served !== true) return skip("not served");
    const plain = `s:conform-check-${NONCE}`;
    const key = enc(plain);
    const problems = [];
    const check = (reply, op) => {
      if (reply.status !== 200) return problems.push(`${reply.what} answered ${reply.status}`);
      let body;
      try {
        body = JSON.parse(reply.text());
      } catch {
        return problems.push(`the ${op} reply is not JSON: ${brief(reply.text())}`);
      }
      return fields(body, { ok: oneOf(true), ignored: "number" }, `the ${op} reply`, problems);
    };
    try {
      check(await ask("GET", `api/ignore?add=${key}`), "add");
    } catch (e) {
      problems.push(e.message);
    }
    // Whatever the add said, or if it went unanswered, the key is removed again; and if that
    // fails, the host is told which key to remove.
    let removed = false;
    try {
      const reply = await ask("GET", `api/ignore?remove=${key}`);
      removed = reply.status === 200;
      check(reply, "remove");
    } catch (e) {
      problems.push(e.message);
    }
    if (!removed) problems.unshift(`remove the key ${plain} from the host's hide list by hand`);
    return verdict(problems, "a key no session has, added and removed");
  });

  await rule("layout.sources", S.layout, "without source the reply is {sources: [{source, label, available, reason, lastError}]}", async () => {
    const reply = await ask("GET", "api/layout");
    if (reply.status === 404) {
      served("/api/layout", false);
      return skip("404: not served (the list offers only its own arrangements)");
    }
    if (reply.status !== 200) fail(`${reply.what} answered ${reply.status}`);
    const body = reply.json();
    const problems = [];
    if (fields(body, { sources: "array" }, "the reply", problems)) {
      body.sources.forEach((s, i) => fields(s, { source: "string", label: "any", available: "any", reason: "any", lastError: "any" }, `sources[${i}]`, problems));
    }
    verdict(problems);
    served("/api/layout", true);
    return `${plural(body.sources.length, "source")}: ${body.sources.map((s) => s.source).join(", ")}`;
  });

  await rule("layout.source", S.layout, "with source the reply is {source, label, available, reason, note, sections, pending?, stale?}", async () => {
    const row = ROUTES.find((r) => r.path === "/api/layout");
    if (row.served !== true) return skip("not served");
    const listReply2 = (await ask("GET", "api/layout")).json();
    const names = listReply2.sources.map((s) => s.source).slice(0, 3);
    if (!names.length) return skip("no source listed");
    const problems = [];
    const entry = (e, where) => {
      if (fields(e, { id: "any", title: "any", pane: "string|null", children: "array" }, where, problems)) e.children.forEach((c, k) => entry(c, `${where}.children[${k}]`));
    };
    for (const name of names) {
      const reply = await ask("GET", `api/layout?source=${enc(name)}`);
      if (reply.status !== 200) {
        problems.push(`${reply.what} answered ${reply.status}`);
        continue;
      }
      const body = reply.json();
      const pending = body.pending === true;
      const spec = { source: "any", label: "any", available: "any", reason: "any", note: "any", [pending ? "sections?" : "sections"]: "array" };
      if (!fields(body, spec, name, problems) || !Array.isArray(body.sections)) continue;
      body.sections.forEach((s, i) => {
        const at = `${name}.sections[${i}]`;
        if (!fields(s, { id: "any", label: "any", collapsed: "any", entries: "array", groups: "array" }, at, problems)) return;
        s.entries.forEach((e, k) => entry(e, `${at}.entries[${k}]`));
        s.groups.forEach((g, k) => {
          if (fields(g, { id: "any", label: "any", collapsed: "any", keep: "any", entries: "array" }, `${at}.groups[${k}]`, problems)) g.entries.forEach((e, m) => entry(e, `${at}.groups[${k}].entries[${m}]`));
        });
      });
    }
    return verdict(problems, names.join(", "));
  });

  await rule("layout.unknown", S.layout, "a source it does not know gets 404", async () => {
    const row = ROUTES.find((r) => r.path === "/api/layout");
    if (row.served !== true) return skip("not served");
    const reply = await ask("GET", `api/layout?source=conform-no-such-source-${NONCE}`);
    if (reply.status !== 404) fail(`${reply.what} answered ${reply.status}`);
    return "404";
  });

  // ------------------------------------------------------------------------------- writes
  // Every write below is refused before it names a session: no target, no prompt, no answer.
  const writes = { served: null, paired: null, why: null };
  try {
    const reply = await ask("POST", "api/consent", { body: "" });
    if (reply.status === 404) writes.served = false;
    else if (reply.status === 401) Object.assign(writes, { served: true, paired: false });
    else Object.assign(writes, { served: true, paired: true });
    for (const w of WRITES) served(w.path, writes.served);
  } catch (e) {
    writes.why = `the write probe (POST /api/consent, no target) got no answer: ${e.message}`;
  }
  const noWrites = () => skip(writes.why ?? "404: not served (the page draws no compose box and no answer buttons)");

  await rule("writes.method", S.request, "the write gate: the wrong method gets 405", async () => {
    if (!writes.served) return noWrites();
    const problems = [];
    for (const w of WRITES) {
      const reply = await ask(w.method === "GET" ? "POST" : "GET", w.path);
      if (reply.status !== 405) problems.push(`${reply.what} answered ${reply.status}`);
    }
    return verdict(problems, "each 405");
  });

  await rule("writes.origin", S.request, "the write gate: a foreign origin gets 403", async () => {
    if (!writes.served) return noWrites();
    const problems = [];
    for (const w of WRITES) {
      const reply = await ask(w.method, w.path, { headers: { origin: FOREIGN }, body: w.method === "GET" ? undefined : "" });
      if (reply.status !== 403) problems.push(`${reply.what} from ${FOREIGN} answered ${reply.status}`);
    }
    return verdict(problems, "each 403");
  });

  await rule("writes.unpaired", S.request, "the write gate: a request that is not paired gets 401", async () => {
    if (!writes.served) return noWrites();
    if (writes.paired === false) return "the reader is not paired: 401";
    if (!SIGNED_IN) return skip("the reader is paired with no sign-in to leave out");
    const problems = [];
    for (const w of WRITES) {
      const reply = await ask(w.method, w.path, { signedIn: false, body: w.method === "GET" ? undefined : "" });
      if (reply.status !== 401) problems.push(`${reply.what} without the sign-in answered ${reply.status}`);
    }
    return verdict(problems, "each 401 without the sign-in");
  });

  await rule("writes.refusal", S.writes, "a refusal is 200 {ok: false, error, code?} (409 for shared_runtime_required), its code one the route names", async () => {
    if (!writes.served) return noWrites();
    if (!writes.paired) return skip("the reader is not paired, so no write reaches its route");
    const problems = [];
    for (const w of WRITES) {
      const reply = await ask(w.method, w.path, { body: w.method === "GET" ? undefined : (w.body ?? "") });
      if (reply.status !== 200 && reply.status !== 409) {
        problems.push(`${reply.what} answered ${reply.status}${originHint(reply)}`);
        continue;
      }
      const body = reply.json();
      if (!fields(body, { ok: oneOf(false), error: "string", "code?": "string" }, `${w.path}'s reply`, problems)) continue;
      if ("code" in body && !w.codes.includes(body.code)) problems.push(`${w.path} names code ${show(body.code)}`);
      if ((reply.status === 409) !== (body.code === "shared_runtime_required")) problems.push(`${w.path} answered ${reply.status} with code ${show(body.code)}`);
    }
    return verdict(problems, "each refused with no target named");
  });

  await rule("request.body-limit", S.request, "a body over 64 KiB gets 413 (401 from a client that is not paired)", async () => {
    // A route the contract gives a body, asked about nothing: an answer to no session's dialog, or
    // the links of a held document.
    const route = writes.served ? `api/answer?target=${NO_SUCH_SESSION}` : held.path ? "api/mdrev/resolve?root=held" : null;
    if (!route) return skip("neither the writes nor mdrev's routes are served, and no other route takes a body");
    const reply = await askWithLength("POST", route, 64 * 1024 + 1);
    if (reply.status === 404) return skip(`${reply.what} answered 404: not served`);
    if (reply.status === 401) return `${reply.what}: 401, the reader is not paired`;
    if (reply.status !== 413) fail(`${reply.what} with ${64 * 1024 + 1} bytes answered ${reply.status ?? `nothing (${reply.why})`}`);
    return `${reply.what} with 65,537 bytes: 413`;
  });

  await rule("request.hold-limit", S.request, "mdrev's hold takes a body over 64 KiB (its limit is 8 MiB plus 64 KiB)", async () => {
    if (!held.path) return skip("hold is not served");
    const text = "tern ".repeat(14_000);
    const reply = await ask("POST", "api/mdrev/hold", { body: JSON.stringify({ name: "large.md", text }), headers: { "content-type": "application/json" } });
    if (reply.status === 413) fail(`${reply.what} with ${text.length} bytes answered 413`);
    if (reply.status !== 200) fail(`${reply.what} answered ${reply.status}: ${brief(reply.text())}${originHint(reply)}`);
    return `${text.length.toLocaleString("en")} bytes held`;
  });

  await rule("request.cross-site", S.request, "the sign-in refuses a request another site started, whatever its method", async () => {
    const reply = await ask("GET", "api/sessions", { headers: { "sec-fetch-site": "cross-site" } });
    if (reply.status >= 400 && reply.status < 500) return `Sec-Fetch-Site: cross-site: ${reply.status}`;
    return skip(
      `${reply.status} to a request that says it came from another site: this host may rely on a SameSite=Strict cookie, which a browser withholds and this check cannot; confirm it by hand`,
    );
  });

  await rule("page.paired", S.writes, "a host that serves no writes says it is not paired (<body data-paired>)", async () => {
    if (writes.served !== false) return skip("the host serves writes");
    if (!page) return skip("the base serves no page");
    if (page.paired === "true") fail("the page says data-paired=\"true\" while every write answers 404");
    return `data-paired=${show(page.paired ?? null)}`;
  });

  // ------------------------------------------------------------------------ detached tabs
  await rule("tabs.static", S.tabs, "/image and /markdown are static pages: nothing from the address is written into them", async () => {
    const marker = `conform${NONCE}marker`;
    const asks = [
      ["/image", `image?name=${marker}&path=${marker}&sig=${marker}&held=${marker}`],
      ["/markdown", `markdown?root=${marker}&path=${marker}&cap=${marker}&from=${marker}&to=${marker}&since=${marker}`],
    ];
    const problems = [];
    const said = [];
    for (const [route, q] of asks) {
      const reply = await ask("GET", q);
      if (reply.status === 404) {
        served(route, false);
        said.push(`${route} not served`);
        continue;
      }
      if (reply.status !== 200) {
        problems.push(`${reply.what} answered ${reply.status}`);
        continue;
      }
      served(route, true);
      if (reply.text().includes(marker)) problems.push(`${route} wrote its address into the page`);
      said.push(`${route} static`);
    }
    if (said.every((x) => x.endsWith("not served"))) return verdict(problems, skip("404: neither is served (no tab of its own)"));
    return verdict(problems, said.join(", "));
  });

  await rule("tabs.review-prefix", S.tabs, "/api/mdrev-review/ answers the same routes as /api/mdrev/: the held document reads back there", async () => {
    if (!held.path) return skip("mdrev's routes are not served, so there is no held document to read");
    const reply = await ask("GET", `api/mdrev-review/text?root=${enc(held.root)}&path=${enc(held.path)}&cap=${enc(held.cap)}`);
    if (reply.status === 404) {
      served("/api/mdrev-review/", false);
      return skip("404: not served (the Markdown tab has no shared review)");
    }
    served("/api/mdrev-review/", true);
    if (reply.status !== 200) fail(`${reply.what} answered ${reply.status}: ${brief(reply.text())}`);
    if (!reply.text().includes("A document the check holds.")) fail(`${reply.what} answered ${brief(reply.text())}, not the held text`);
    return "the held text";
  });

  // ---------------------------------------------------------------------- the route index
  await rule("index.core", S.index, "every core row is served", async () => {
    const core = ROUTES.filter((r) => r.tier === "core");
    const missing = core.filter((r) => r.served === false).map((r) => r.path);
    if (missing.length) fail(`not served: ${missing.join(", ")}`);
    const unknown = core.filter((r) => r.served === null).map((r) => r.path);
    if (unknown.length) return skip(`served: ${core.filter((r) => r.served).map((r) => r.path).join(", ")}; not reached without a session: ${unknown.join(", ")}`);
    return core.map((r) => r.path).join(", ");
  });

  return finish();
}

/** What a 403 to a write of the page's own may mean, when no --origin was given. */
function originHint(reply) {
  return reply.status === 403 && !ARGS.origin
    ? ` (a 403 here can mean the page is on another origin than the routes: name it with --origin)`
    : "";
}

function isUtf8(bytes) {
  try {
    new TextDecoder("utf-8", { fatal: true }).decode(bytes);
    return true;
  } catch {
    return false;
  }
}

function finish() {
  const counts = { pass: 0, fail: 0, skip: 0 };
  for (const r of results) counts[r.status]++;
  const conforms = counts.fail === 0;
  // What a run that read no session could not say anything about.
  const note = READ.session
    ? null
    : `no session was read (${READ.why}), so the routes that read one (/pull, /records, /turn-index, /search, /file) and the record format went unchecked`;
  if (ARGS.json) {
    const report = { contract: CONTRACT, base: `${BASE.origin}${BASE.pathname}`, conforms, counts, note, results, routes: ROUTES };
    process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
  } else {
    process.stdout.write("\nRoutes: ");
    process.stdout.write(
      ROUTES.filter((r) => r.tier !== "native")
        .map((r) => `${r.path} ${r.served === true ? "served" : r.served === false ? "not served" : "?"}`)
        .join(" · "),
    );
    process.stdout.write("\nCodex native mode (/api/codex/, /codex-workspace/) is outside contract 1 and not checked.\n\n");
    if (conforms) {
      process.stdout.write(`conforms to contract ${CONTRACT}: ${counts.pass} rules passed, ${counts.skip} skipped\n`);
    } else {
      process.stdout.write(`does not conform to contract ${CONTRACT}: ${plural(counts.fail, "rule")} failed, ${counts.pass} passed, ${counts.skip} skipped\n`);
      for (const r of results.filter((x) => x.status === "fail")) process.stdout.write(`  ✗ ${r.id} (${r.section})\n`);
    }
    if (note) process.stdout.write(`Not checked: ${note}.\n`);
  }
  process.exitCode = conforms ? 0 : 1;
}

main().catch((e) => {
  process.stderr.write(`conform: the check stopped: ${redact(e instanceof Fail ? e.message : (e.stack ?? e))}\n`);
  process.exitCode = 2;
});
