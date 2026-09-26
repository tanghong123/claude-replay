# The ⌘K search: a jump-to, sharing the in-session grammar

> **Status: DECIDED (owner, 2026-09-26), not built.** Task #138. §2 is the owner's decisions; §4
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

- **Tabs**: All / Agent / Project / Session.
- **Placeholder** says what it does: "Go to an agent, project or session…" (today's reads
  "Search agents、Projects、Session or transcripts…", with a Chinese comma and a promise it does
  not keep).
- **Grammar**: plain words match names, as today. Qualifiers narrow the list — `agent:codex`,
  `project:knack` — and are the same `key:value` token shape as the in-session `tool:`. The
  in-session scope prefix and `tool:` mean nothing here and are ignored rather than matched
  as text.
- **Open, for the owner** (not decided): ranking by recency with the open session's project
  first; choosing a project filtering the sidebar instead of opening its first session;
  whether the sidebar's session filter (Active recently / Blocked / Idle) applies here — the
  recommendation is that it does not, and that each row shows its bucket; folding the Search
  button and the mini search into one entry point.

## 5. Implementation (filed as a task)

Remove the Transcript tab and its note, reword the placeholder, parse qualifiers through the
shared grammar module. A scenario on the app shell (the classic rail has no such dialog): the
tab is gone, a name query jumps, `project:` narrows.
