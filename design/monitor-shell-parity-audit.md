<!-- Tracked as queue task #31. Comment inline; the queue is the state of record, this file is the argument. -->

# Shell parity audit — what the classic shell does that the app shell does not

Method: enumerated every route, DOM control, keyboard handler and top-level function in the
classic v1 rail (`claude-monitor/src/rail.html`), the classic v2 splice shell
(`claude-monitor-v2/src/shell.html`) and the session view they embed
(`claude-replay-html/src/html/export.js`), then looked for each in the app shell
(`claude-monitor/src/codex-ui/*.js`) by route, id and behaviour. Line numbers are on `main`
at v1.129.1.

## Present only in the classic shell

| # | Feature | Classic evidence | App shell |
|---|---|---|---|
| 1 | **Hide / restore** sessions, projects and agents, and **"Show hidden"** | `rail.html:197 #hiddentoggle`, `/api/ignore`; v2 `shell.html:267 "Show hidden (n)"` | Never calls `/api/ignore`. Filters `row.hidden` out (`app.js:208-216`) and offers no way to hide, restore, or see what is hidden. **One-way trap**: anything hidden from classic vanishes here with no recovery. |
| 2 | **Reveal in file manager** | `export.js:642` — `/__reveal` with the reveal stamp; also the fallback when the render policy withholds the file stamp | Never calls `/__reveal`. Carries the stamp (`view-model.js:88 revealSig`) and does nothing with it; fallback is "copy path". |
| 3 | **Back to the parent session** from a child | `export.js:827 synthesizeBack(ancestors)` | `#sessionParent` exists in the shell (`compat-hidden`) but nothing binds it, and the shell never reads `ancestors`. A child opened directly has no way up. |
| 4 | **Family clustering** — sub-agents grouped under their parent in the list | `rail.html:422 families()`, `:415 clusterKey()` | Tree has no family notion; children appear only in the outline's agent list *inside* the parent. |
| 5 | **Agent filter chips** (all / claude / codex / …) | `rail.html #filters`, `state.filter` | Needs-attention filter and search only. |
| 6 | **Scroll position restored across reloads**, per session | `export.js:2004 saveView` / `loadView` (sessionStorage) | No `sessionStorage` anywhere; a reload lands at the tail. |
| 7 | **Reading controls**: monospace size, wrap, wide layout | `export.js:2409 setMono`, `:2415 setWrap`, `:2459 setWide` | None. |
| 8 | **Per-turn raw ↔ rendered toggle** (`{}`) | `export.js:196 rawToggle` | None (the `raw` fallback renderer is a different thing). |
| 9 | **Published-artifact roster** — `Published … at <url>` results grouped by URL, republishes collapsed | `export.js:1349 renderArtifactMenu`, `collectArtifacts` | None. The app shell's "artifacts" are preview tabs of local files. |
| 10 | **Fleet run membership** in the header (jdi fleet runs) | `export.js:1138 renderFleets` | None. |
| 11 | **Keyboard** — `/` focuses the list filter, `↑`/`↓` move the list (`rail.html:744`); in the view `j`/`k` turns, `n`/`N` search hits, `[`/`]`, `w` wrap, `+`/`-`/`=` mono size, Space | `rail.html:744`, `export.js` `onKey` | `⌘K`, `Enter`, `Esc`, and `↓` only on the title (`app.js:81, :483`). |
| 12 | **Expand all** groups | `rail.html #expandall` | Collapse-all only (`collapseBtn`). |
| 13 | **Resizable session list** | `rail.html #drag` | Only the preview pane resizes (`preview.js:22`). |
| 14 | **Step between tool-filter hits** (▲▼ on a filter) | `export.js:1770 filterNav` | Filters hide; find prev/next exists for search only. Minor. |
| 15 | **Session id in the header, click copies the transcript path** (`#sid`, owner-reported after this audit) | `export.js renderHead` (`#sid`, `snipId`) | A copy menu on the title offered both values; the id itself was not shown. Closed by #50 (v1.156.0) with an id chip; the owner then found the title's copy menu (id + path) enough and #83 dropped the chip again — the shortener stays shared (`html/shared/ids.js`) for the classic page. |

Not gaps: theme sync into the embedded view (one document now), the collapsed "strip" mode
(the mini rail is its analogue), compose / consent / passcode (`control-store.js`), tasks,
usage & cost, follow + new-records badge, jump to bottom, lightbox, search scopes, tool-type
filter, fold all, `?session=` + `#anchor` deep links — all present in both.

## Present only in the app shell (for the record)

Global search across sessions *and* transcripts with scope tabs (⌘K); the needs-attention
filter with per-row reasons (`agentState*`); the outline navigator (turns / work / agents) as a
standing panel; the preview pane with tabs and a session-scoped cache; capability-aware
attachment actions (image lightbox, download, copy) and prompt attachments; the
`request_user_input` projection; the proposed-plan surface; per-process fold surfaces; sticky
headers; spot links; a mobile layout; and the shell switch itself.

## Decisions (owner, 2026-09-03)

Yes → queued: 1 (#32), 2 (#33), 3 + 4 (#34), 6 (#35), 7 + 8 (#36), 11 (#37), 15 (#50) — each
pinned to the app shell's own surfaces and idiom, execution on the owner's go. Deferred, held on #31:
5, 9, 10, 12, 13, 14.

## Suggested order for closing the gaps before "validated"

1 (a data trap, not a convenience) → 2 (and it is the policy-withheld fallback) → 11 → 3 + 4
(sub-agent navigation) → 6 → 7 + 8 → 5 → 9 + 10 → 12, 13, 14.
