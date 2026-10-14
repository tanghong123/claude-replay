# AGENTS.md — claude-replay

Read [CLAUDE.md](CLAUDE.md) for the existing project architecture, behavior,
coding conventions, and verification requirements. Apply the additional rules
below alongside that guidance; explicit user instructions take precedence.

## UI impact assessment

Every UI change requires an explicit assessment of affected pages and output
paths before implementation, updated against the final diff before delivery.

- Trace changed renderers, shared JavaScript, CSS, assets, routes, and cached
  records to every consumer. Include indirect effects such as export bytes,
  bundle size, network requests, and cache invalidation.
- Check each applicable entry: `agent-monitor` and `agent-monitor-v2` app shells
  (`?ui=app`), their classic shells (`?ui=classic`) while supported, standalone
  served sessions (`--html` and `/session`), single-file HTML exports
  (`--dump-html`), and directory HTML exports (`--dump-all-html`). Also assess
  preview panes, detached document/image pages, fleet views, print output, and
  terminal/plain-text/JSON exports when the changed components reach them.
  Confirm the current entrypoints from source instead of treating this list
  as exhaustive or assuming a retired page is still supported.
- Record a compact impact table in the MR/PR description: page or output,
  affected or excluded (with a reason), expected behavior, verification evidence
  and source revision, and remaining gaps or failures. Use concrete files,
  routes, and commands to explain which component supplies each behavior.
- Verify each affected entry against its expected behavior with appropriate
  checks or a reproducible demonstration. Exercise relevant light/dark themes,
  desktop/mobile layouts, live updates, navigation/search, controls, failure
  states, and offline behavior. A shared module or a passing check on one page
  does not establish that every consuming page works correctly.
- Reconcile the impact table with the final diff and actual page/route inventory
  to find omitted consumers, missing CSS or assets, and untested paths. State
  intentional differences explicitly. Fix unintended omissions and regressions;
  disclose unresolved findings and missing evidence before delivery, without
  describing code-path inspection as a successful runtime check.
- Preserve the repository's applicable output and browser checks. Use synthetic
  data for shared demonstrations and MR screenshots; do not publish real
  conversation content.
