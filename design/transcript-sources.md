# Acquiring transcripts from web & desktop sources

> Investigation / feasibility doc. No source code changed. Researched 2026-07-25.
> Goal: teach `claude-replay` to pull transcripts from web + desktop surfaces, not just
> local `~/.claude/projects/*.jsonl` (Claude Code CLI) and `~/.codex/sessions/*.jsonl` (Codex CLI).

## TL;DR

There are **two different "Claude web" things** and they must not be conflated:

- **Claude Code on the web** (the cloud coding agent at `claude.ai/code`) — has a **usable
  private JSON API** returning transcripts in a `loglines[]` block shape that is *almost identical*
  to the Claude Code `.jsonl` we already parse. This is the best target. (Reverse-engineered and
  proven by Simon Willison's `claude-code-transcripts`, though his `web` command is currently
  broken because Anthropic changed the undocumented endpoints.)
- **claude.ai chat conversations** (the normal chatbot) — **no per-conversation API or export
  button.** Only a whole-account "Export data" ZIP (emailed, 24h link) or DOM/console scraping.
  Format is a flat `sender`/`text` message list — much thinner than our block model.

**Recommended first target: Claude Code on the web.** It is closest to what we already render,
the acquisition path is documented in working reference code, and the payload maps onto our
existing `model.rs` block pipeline with minimal new parsing. Everything else is moderate-to-hard
or needs a real logged-in account to confirm.

---

## 1. Claude web

This splits into two surfaces. Treat them separately.

### 1a. Claude Code on the web  (RECOMMENDED FIRST TARGET)

The cloud coding agent — "assign multiple coding tasks that run on Anthropic-managed cloud
infra", launched Oct 2025 as a research preview for Pro/Max/Team, lives at `claude.ai/code`.
Sessions run in isolated VMs, persist across browser close, and auto-open PRs.
([anthropic.com/news/claude-code-on-the-web](https://anthropic.com/news/claude-code-on-the-web),
[code.claude.com/docs/en/claude-code-on-the-web](https://code.claude.com/docs/en/claude-code-on-the-web))

**Acquisition path — private JSON API** (reverse-engineered by Simon Willison by de-obfuscating
Claude Code's JS with `npx prettier`; documented in his post and implemented in
`claude-code-transcripts`):

- List sessions: `GET https://api.anthropic.com/v1/sessions`
- Fetch one session (full transcript): `GET https://api.anthropic.com/v1/session_ingress/session/{session_id}`

Source (exact code):
[`src/claude_code_transcripts/__init__.py`](https://github.com/simonw/claude-code-transcripts/blob/main/src/claude_code_transcripts/__init__.py)
— `API_BASE_URL = "https://api.anthropic.com/v1"`, functions `fetch_sessions()` and `fetch_session()`.
Blog: [simonwillison.net/2025/Dec/25/claude-code-transcripts/](https://simonwillison.net/2025/Dec/25/claude-code-transcripts/).

> ⚠️ **Currently broken.** The project README states: *"The `web` commands for both listing
> Claude Code for web sessions and converting those to a transcript are both broken right now
> due to changes to the unofficial and undocumented APIs that these commands were using."*
> ([README.md](https://github.com/simonw/claude-code-transcripts/blob/main/README.md)) So the
> *shape* below is confirmed from working history, but the exact live endpoints may have moved and
> **need re-confirmation against a real logged-in account.**

**Auth:** OAuth bearer token, not an API key.
- Token: macOS Keychain item `Claude Code-credentials`, field `.claudeAiOauth.accessToken`
  (`security find-generic-password -a "$USER" -s "Claude Code-credentials" -w` → parse JSON).
- Org UUID: `~/.claude.json` → `.oauthAccount.organizationUuid`.
- Headers sent:
  ```
  Authorization: Bearer <accessToken>
  anthropic-version: 2023-06-01
  Content-Type: application/json
  x-organization-uuid: <organizationUuid>
  ```
- On non-mac, the CLI takes `--token` / `--org-uuid` manually.
- **ToS/practical:** private undocumented endpoint reusing the *user's own* logged-in Claude Code
  credentials that already sit on disk — the same trust boundary as reading the local `.jsonl`.
  Reasonable for a personal local viewer; brittle (breaks when Anthropic changes the API, as it
  already has). Not a stable integration; treat as best-effort with graceful failure.

**Format:** JSON with a top-level `loglines[]` array (confirmed from the repo's
[`tests/sample_session.json`](https://github.com/simonw/claude-code-transcripts/blob/main/tests/sample_session.json)).
Each logline:
```jsonc
{
  "type": "user" | "assistant",
  "timestamp": "2025-12-24T10:00:00.000Z",
  "message": {
    "role": "user" | "assistant",
    "content": [ { "type": "thinking" | "text" | "tool_use" | "tool_result", ... } ]
    //  first user turn's content may be a bare string instead of a block array
  }
}
```
Plus a `session_context` object (in live payloads; empty in the sample fixture) carrying
`outcomes[]` / `sources[]` with `git_info.repo` and repo URLs.

**Closeness to our model:** *very close.* This is the same
`{role, content:[{type:text|thinking|tool_use|tool_result}]}` block vocabulary as the Claude Code
`.jsonl` that `model.rs` already turns into `Block`s. The main differences: it's one JSON object
with a `loglines[]` wrapper instead of one JSON-per-line, and the outer envelope keys differ
(`loglines` vs `.message` per line). A thin adapter that iterates `loglines` and feeds each
`message` into the existing block parser should mostly work.

**Feasibility: MODERATE.** Payload maps cleanly; auth is local credential reuse. Downgraded from
"easy" only because the exact live endpoints are currently broken/moved and must be re-derived
against a real account.

### 1b. claude.ai chat conversations (the chatbot)

The normal conversational product.

**Acquisition paths (all weak):**
1. **Official account data export** — Settings → Privacy → *Export data*. Emailed ZIP with a
   ~24h expiring link, web/desktop only. Contains `conversations.json` (some reports say a
   `.jsonl`, one conversation per line). **Whole account only — no per-conversation, no on-demand
   download, no programmatic trigger.** ([XTrace guide](https://xtrace.ai/blog/export-claude-conversations),
   [promptlayer guide](https://blog.promptlayer.com/how-to-download-a-claude-chat-session/))
2. **DOM / console scrape** — e.g. `ryanschiang/claude-export`, a browser-console script that
   reads the **rendered DOM** (not an API) and emits Markdown/JSON/PNG, entirely client-side.
   ([github.com/ryanschiang/claude-export](https://github.com/ryanschiang/claude-export)) Brittle;
   breaks on UI changes; loses tool-call structure.
3. **Undocumented internal API** — the web app internally uses endpoints under
   `claude.ai/api/organizations/{org}/chat_conversations/{uuid}`. **UNCONFIRMED here** — I did not
   verify the exact path or payload against a live login; several third-party exporters are
   DOM-based precisely because this isn't a stable public surface. Do not build on this without
   first-hand confirmation.
4. **Official public API** — none. Anthropic's public API is the *model* API
   (`/v1/messages`); it does not expose your claude.ai chat history.

**Auth:** logged-in `claude.ai` session cookie (for scrape/internal-API) or the emailed export
link (for the ZIP). No API key path exists.

**Format (`conversations.json`):** array of conversations; each has `uuid`, `name`, created/updated
timestamps, `model`, and `chat_messages[]` where each message has `sender` (`human`|`assistant`),
text, and a created timestamp. Flat human/assistant turns — **no first-class tool_use / tool_result
/ thinking blocks** like our model. ([promptlayer](https://blog.promptlayer.com/how-to-download-a-claude-chat-session/),
[ai-chat-importer](https://ai-chat-importer.com/blog/how-to-export-claude-conversations))

**Closeness to our model:** *low.* It's a chat log, not an agent trace — maps to plain
user/assistant text `Block`s, none of the tool/diff richness `render.rs` specializes in.

**Feasibility: HARD** for anything automated/live (no per-conversation API, cookie + brittle
scrape). **EASY-but-manual** if we only accept a user-provided export ZIP and parse
`conversations.json` offline.

---

## 2. Claude Design

**What it is (confirmed):** an Anthropic Labs product launched to Pro/Max/Team/Enterprise —
"collaborate with Claude to create polished visual work: designs, prototypes, slides, one-pagers."
Chat + inline comments + adjustment sliders; reads your codebase/design files to build a design
system; powered by Claude Opus 4.7. ([Fast Company](https://www.fastcompany.com/91528198/anthropic-claude-design-ai-design-tool),
[VentureBeat](https://venturebeat.com/technology/anthropic-just-launched-claude-design-an-ai-tool-that-turns-prompts-into-prototypes-and-challenges-figma),
[DataCamp](https://www.datacamp.com/blog/claude-design))

**Export:** the only documented "export" is a **handoff bundle to Claude Code** ("packages
everything into a handoff bundle passed to Claude Code" — VentureBeat/Fast Company). There is
**no documented transcript/session export, no API, no share-JSON** for the Design *session* itself.

**Auth / Format:** **UNKNOWN / UNCONFIRMED.** No public docs on where a Design session is stored or
whether its conversation is retrievable. It plausibly rides the same claude.ai backend as 1b, but
I could not confirm any endpoint or on-disk format. **Needs a real Pro/Max account to inspect.**

**Feasibility: NOT CURRENTLY POSSIBLE to confirm.** No evidence of a transcript export path.
Deprioritize until (a) someone with access inspects the network traffic, or (b) the handoff bundle
turns out to contain a readable conversation log. Interesting angle to check later: *if* the
"handoff bundle" lands in `~/.claude/projects` as a normal Claude Code session, we'd get it for
free via the path we already read — **unverified, worth a 5-minute check with an account.**

---

## 3. Claude Code / "cowork" in the desktop app

The Claude **desktop** application (Mac/Windows) and its coding / agent mode.

**Where transcripts live — the key question, and it's ambiguous:**
- The **Claude Code CLI** (which the desktop app embeds/launches for coding work) stores sessions
  as **JSONL under `~/.claude/projects/<slug>/<sessionId>.jsonl`** on mac/Linux and
  `%USERPROFILE%\.claude` on Windows — *exactly what `claude-replay` already reads*. Local
  transcripts are pruned after `cleanupPeriodDays` (default 30). ([calmbu.com](https://calmbu.com/claude-code-backup-recovery/where-claude-code-stores-local-data-on-windows/),
  and Simon's tool reads `~/.claude/projects` as its default `local` source —
  [README](https://github.com/simonw/claude-code-transcripts/blob/main/README.md))
- The **desktop app's own config** lives at
  `~/Library/Application Support/Claude/claude_desktop_config.json` (mac). That's config, not
  transcripts. ([itecsonline](https://itecsonline.com/post/how-to-claude-sqlite))
- Whether the desktop **"cowork" agent** writes to `~/.claude/projects` (reuse) **or** to a
  separate store under `~/Library/Application Support/Claude/` (sqlite/leveldb, as Electron apps
  often do) is **UNCONFIRMED.** I found no authoritative doc pinning cowork's transcript store.

**Acquisition path:** if cowork reuses `~/.claude/projects` → **zero new work, already supported.**
If it uses a private Application-Support store → we'd need to locate and parse it (likely sqlite or
an Electron leveldb/IndexedDB), which is a reverse-engineering task.

**Auth:** none beyond local filesystem access (it's on the user's own machine).

**Format:** if reused → identical Claude Code `.jsonl` block model (perfect fit). If separate →
unknown.

**Feasibility: EASY *if* it reuses `~/.claude/projects` (likely, and testable in minutes on a
machine with the desktop app); MODERATE-to-HARD if it has a private store.** This is a
**"go look on a real install" item**, not a research-from-docs item — the answer is one
`ls ~/.claude/projects` + `ls ~/Library/Application\ Support/Claude/` away.

---

## 4. Codex web

The web version of OpenAI Codex (Codex cloud / Codex in ChatGPT), contrasted with the Codex CLI
rollout JSONL we already parse from `~/.codex/sessions/`.

**Acquisition paths:**
1. **Built-in share (transcript)** — you *can* share a conversation transcript **from Codex Web
   (and cloud runs shown in the Codex App)**; you **cannot** share from the Codex App-local,
   extension, or CLI conversations. ([openai/codex discussion #13251](https://github.com/openai/codex/discussions/13251))
   The exact **share-link URL and whether it exposes a fetchable JSON payload are UNCONFIRMED** —
   ChatGPT's general share format is `https://chatgpt.com/share/{id}` for chats
   ([OpenAI shared-links FAQ](https://help.openai.com/en/articles/7925741-chatgpt-shared-links-faq)),
   but I could not confirm the Codex-task share URL shape or that it yields structured JSON vs an
   HTML page. Needs a real account to inspect.
2. **Official API** — none found for exporting Codex-web task transcripts. Codex cloud is driven
   from web/GitHub/Linear/Slack but no documented transcript-download API surfaced.
   ([developers.openai.com/codex/cloud](https://developers.openai.com/codex/cloud))
3. **Local (contrast, already handled)** — CLI/extension/app conversations are stored **locally,
   not in the cloud** ("Local conversations are stored locally to the machine… They are not stored
   in the cloud" — maintainer, discussion #13251). The CLI rollout JSONL under `~/.codex/sessions/`
   is what `claude-replay`'s `codex_model.rs` / `codex_discover.rs` already parse.
4. **DOM scrape** — last resort against the Codex-web UI; brittle, unconfirmed structure.

**Auth:** logged-in ChatGPT/Codex session cookie for web share; the API-key path doesn't cover
transcript export.

**Format:** **UNCONFIRMED.** Not verified whether Codex-web exposes the same rollout event shape as
the CLI JSONL we parse, or a different chat-style payload. A shared Codex transcript's on-the-wire
format needs first-hand capture.

**Feasibility: MODERATE-to-HARD / UNCONFIRMED.** A share feature exists (good sign) but its
machine-readable payload is unverified. Cloud runs surfaced in the local Codex App *might* also
land on disk — worth checking alongside item 3.

---

## Summary comparison

| Source | Best acquisition path | Auth | Format vs our block model | Feasibility |
|---|---|---|---|---|
| **1a. Claude Code on the web** | Private API `GET /v1/sessions` + `/v1/session_ingress/session/{id}` | OAuth bearer from Keychain `Claude Code-credentials` + org UUID from `~/.claude.json` | `loglines[]` of `{role, content:[text/thinking/tool_use/tool_result]}` — **very close** | **MODERATE** (endpoints currently broken/moved, re-confirm) |
| **1b. claude.ai chat** | Account "Export data" ZIP (`conversations.json`) | Emailed 24h link / session cookie for scrape | flat `sender`+text, no tool blocks — **low** | HARD (live) / EASY (offline ZIP, manual) |
| **2. Claude Design** | none found (only Claude-Code "handoff bundle") | unknown | unknown | **NOT CONFIRMED POSSIBLE** |
| **3. Desktop "cowork"** | likely reuses `~/.claude/projects/*.jsonl` (unconfirmed) | local fs | identical Claude Code `.jsonl` **if** reused | EASY if reused / MODERATE-HARD if private store |
| **4. Codex web** | built-in "share transcript" (payload shape unconfirmed) | ChatGPT session cookie | unconfirmed | MODERATE-HARD / UNCONFIRMED |

## Recommended acquisition path per source

- **1a Claude Code on the web →** implement the private-API pull (list + fetch-by-id), reusing the
  Keychain/`~/.claude.json` credential extraction and the `loglines[]`→`Block` adapter. **Build this first.**
- **1b claude.ai chat →** support **offline parsing of a user-supplied export ZIP** (`conversations.json`)
  only; do not build live cookie scraping. Low priority.
- **2 Claude Design →** park it; open a spike ticket "does a Design handoff bundle appear as a
  normal session in `~/.claude/projects`?" to be answered by someone with an account.
- **3 Desktop cowork →** **first do the 5-minute empirical check** on a real desktop install: does
  cowork write to `~/.claude/projects`? If yes, it's already supported and just needs a docs note.
  If no, scope the private-store parse separately.
- **4 Codex web →** park until someone captures a real Codex-web share link + its response; then
  decide adapter vs scrape.

### Build order
1. **Claude Code on the web** (private API) — highest value, closest to existing pipeline.
2. **Desktop cowork empirical check** — possibly free coverage, near-zero cost to verify.
3. **claude.ai export-ZIP offline parser** — easy, self-serve, no auth automation.
4. Codex web / Claude Design — blocked on real-account reconnaissance.

## Unknowns / needs-a-real-account-to-confirm

1. **1a live endpoints** — Simon's `web` command is broken; the exact current
   `/v1/sessions` and `/v1/session_ingress/session/{id}` paths and any new headers must be
   re-derived against a logged-in Claude Code account (capture real network traffic).
2. **1a `session_context` live shape** — empty in the test fixture; real outcomes/sources fields
   (git repo, PR links, attachments) unverified.
3. **1b internal chat API** — whether `claude.ai/api/organizations/{org}/chat_conversations/{uuid}`
   (or similar) exists as a fetchable JSON endpoint, and its schema. Unconfirmed; treat as scrape-only.
4. **1b export format drift** — ZIP contains `conversations.json` vs a `.jsonl`; confirm current shape
   and whether attachments/tool calls are included.
5. **Claude Design** — does *any* transcript/session export exist? Where is a Design session stored?
   Does the handoff bundle contain a readable conversation? All unknown without access.
6. **Desktop cowork storage** — reuse of `~/.claude/projects` vs a private
   `~/Library/Application Support/Claude/` sqlite/leveldb store. Answerable by inspecting a real install.
7. **Codex web share** — the share-link URL shape and whether it yields structured JSON (vs HTML);
   whether cloud-run transcripts also land locally. Needs a real Codex-web account.
8. **ToS** — automating any cookie/OAuth-driven pull of Anthropic/OpenAI web surfaces should be
   reviewed against each provider's ToS before shipping beyond personal/local use; the local
   credential-reuse paths (1a, 3) are lowest-risk since they read the user's own machine.

## Key source URLs

- Simon Willison, "A new way to extract detailed transcripts from Claude Code" — https://simonwillison.net/2025/Dec/25/claude-code-transcripts/
- `simonw/claude-code-transcripts` README — https://github.com/simonw/claude-code-transcripts/blob/main/README.md
- `simonw/claude-code-transcripts` source (endpoints, keychain, headers) — https://github.com/simonw/claude-code-transcripts/blob/main/src/claude_code_transcripts/__init__.py
- sample session JSON (`loglines[]` shape) — https://github.com/simonw/claude-code-transcripts/blob/main/tests/sample_session.json
- Claude Code on the web (news) — https://anthropic.com/news/claude-code-on-the-web
- Claude Code on the web (docs) — https://code.claude.com/docs/en/claude-code-on-the-web
- `ryanschiang/claude-export` (DOM console scraper) — https://github.com/ryanschiang/claude-export
- Export claude.ai conversations guides — https://xtrace.ai/blog/export-claude-conversations · https://blog.promptlayer.com/how-to-download-a-claude-chat-session/ · https://ai-chat-importer.com/blog/how-to-export-claude-conversations
- Claude Design — https://www.fastcompany.com/91528198/anthropic-claude-design-ai-design-tool · https://venturebeat.com/technology/anthropic-just-launched-claude-design-an-ai-tool-that-turns-prompts-into-prototypes-and-challenges-figma · https://www.datacamp.com/blog/claude-design
- Claude Desktop config path / sqlite — https://itecsonline.com/post/how-to-claude-sqlite
- Claude Code local data on Windows / retention — https://calmbu.com/claude-code-backup-recovery/where-claude-code-stores-local-data-on-windows/
- Codex sharing discussion (web-only share; local storage) — https://github.com/openai/codex/discussions/13251
- Codex cloud docs — https://developers.openai.com/codex/cloud
- ChatGPT shared-links FAQ (share URL format) — https://help.openai.com/en/articles/7925741-chatgpt-shared-links-faq
