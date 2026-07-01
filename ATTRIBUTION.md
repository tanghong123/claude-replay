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
