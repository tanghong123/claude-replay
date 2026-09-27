# The ⌘K search: a jump-to, sharing the in-session grammar

> **Status: DECIDED (owner, 2026-09-26; amended 2026-09-27), built in #294.** Task #138. §2 is the owner's decisions; §4
> works them out and is open to amendment. Related: `design/in-session-search.md` (#137).

## 1. What it is today

⌘K, the sidebar's Search button and its mini search all open one dialog (`#searchLayer`) with
tabs All / Agent / Project / Session / Transcript (`globalRows` / `renderGlobalSearch`, app.js).
It matches agent, project and session NAMES by substring, in tree order, capped at 80 rows; an
empty query lists everything. The Transcript tab searches only the session already open — plain
substring, no scopes, no stepping — and says so in a note ("Transcript search covers the current
session only"). So the one box that looks global is a name switcher plus a weaker copy of the top
bar's search.

## 2. The decisions

1. **⌘K is a jump-to** over agents, projects and sessions. Finding text inside the open session is
   the top bar's job.
2. **The Transcript tab is removed**, with its note.
3. **Cross-session content search is on hold.** When it comes, it is a mode of its own whose
   results land in a session with the same query already in the top bar.
4. **It shares the in-session grammar** (`in-session-search.md` §3): a query typed here means
   what it means there, and the session-level qualifiers below extend it rather than invent a
   second syntax.

## 3. Why a jump-to and a find are different tools

Every tool this resembles keeps them apart: VS Code's ⌘P (go to a file by name) and ⌘F (find in
this file); Slack's ⌘K (jump to a channel or person) and its Search (message content). A jump-to
answers "where do I go", ranks by what you are likely to want, and is done in one keystroke; a
find answers "where does this occur" and steps through occurrences. One box doing both, badly,
is what the Transcript tab was.

## 4. Working it out

- **No tabs** (owner, 2026-09-27: tabs on this box are "pointless"). The qualifiers below narrow
  the list, and each row names its kind.
- **Placeholder** says what it does: "Go to an agent, project or session…" (today's reads
  "Search agents、Projects、Session or transcripts…", with a Chinese comma and a promise it does
  not keep).
- **Grammar**: plain words match names, as today. Qualifiers narrow the list — `agent:codex`,
  `project:knack` — and are the same `key:value` token shape as the in-session `tool:`. The
  in-session scope prefix and `tool:` mean nothing here and are ignored rather than matched
  as text.
- **Ranking: by recency** (owner, 2026-09-26). The most recently active sessions first, so the
  list opens on what the reader was just doing.
- **A project row opens its own most recent session** (owner): the project is the address, its
  newest session is where the reader lands. It does not filter the sidebar.
- **The sidebar's session filter does not narrow this list** (owner), but a row the sidebar is
  currently not showing is drawn in a **dimmed shade** and **says why** in the sidebar's own words
  (owner, 2026-09-27): `hidden` for a session the reader hid (or whose project they hid), and the
  filter bucket they have unticked (`idle`, `blocked`, `active recently`) — both when both apply.
  Choosing it **clears what hides it**: the bucket filter to All, and Include hidden on for a
  hidden one — otherwise the reader would be dropped into a session the list beside them does not
  show. That clearing is the reader's own act (they picked the row), so it is not a filter changing
  underneath them.
- **The two entry points stay two** (measured 2026-09-26): `searchBtn` belongs to the expanded
  sidebar and `sidebarMiniSearch` to the collapsed 64px rail (`.app.sidebar-off`), so they are one
  control in two sidebar states, not a duplicate.

## 5. Implementation (filed as a task)

Remove the tabs, reword the placeholder, parse qualifiers through the shared grammar module, rank
by recency, land a project row on its newest session, and dim the rows the sidebar is not showing
with the reason — choosing one clears what hides it. A scenario on the app shell (the classic rail
has no such dialog): no tabs, a name query jumps, `project:` narrows, a filtered-out session reads
as dimmed with `idle` and a hidden one with `hidden`, and picking each leaves the sidebar showing it.
