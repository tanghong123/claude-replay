# Attribution

`claude-replay` borrows design ideas (and may adapt small pieces of code) from:

- **claude-code-scrollback** by pjh4993 — MIT License, © 2026 pjh4993.
  <https://github.com/pjh4993/claude-code-scrollback>
  Borrowed concepts: byte-offset incremental tail with partial-line buffering and
  truncation/rewrite recovery; pre-rendered line cache for O(1) scrolling;
  collapse/fold model for tool & thinking blocks; directory-affinity session picker.

- **claude-code-trace** by delexw — MIT License.
  <https://github.com/delexw/claude-code-trace>
  Borrowed concepts: word-level Edit diff rendering; metric formatting
  (tokens / cost / duration / short model name).

Where code is adapted rather than merely inspired, the upstream MIT notice is
preserved in the relevant source file.

## Brand marks

- **Claude, Qoder and Qwen** — the marks drawn for Claude Code, Qoder / QoderWork and Qwenwork
  sessions in the agent monitor (`agentLogo` in `design/agent-monitor-codex-demo.html`, extracted
  into `claude-monitor/src/codex-ui/icons.js`) are the paths of the single-colour icons each vendor
  published to the **Agent Client Protocol registry** (<https://github.com/agentclientprotocol/registry>,
  Apache-2.0; entries `claude-acp`, `qoder` and `qwen-code` at commit `528db5c8`), scaled into the
  monitor's 24-unit icon box and drawn in the surrounding text colour, as the registry draws them.
  The marks themselves are Anthropic's, Qoder's and Alibaba's trademarks, used only to say which
  agent wrote a session. The Claude path was Simple Icons' (CC0-1.0, in its brand colour) before.
- **Codex** has no official mark here on purpose: Simple Icons removed OpenAI's in v16 because
  OpenAI's brand guidelines require permission (simple-icons#13944, #12739, #14096). Codex sessions
  show a plain terminal-prompt glyph drawn for this project until that permission exists.
