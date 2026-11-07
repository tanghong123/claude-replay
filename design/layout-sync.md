# Arranging the session list like the Claude app, the Codex app or Herdr

> **DRAFT** (#s382, 2026-11-06). A proposal, with a local prototype on the same branch (#s387) that
> follows the revised rules of §2; nothing is released. The facts about each app in §3 were read on
> one macOS machine on 2026-11-06, with the versions named there. The demo is
> `design/layout-sync-demo.html` (synthetic sessions only); it predates the revision of §2.

## 1. The problem

A person who runs many agents arranges them where they run them:

- in the **Claude app**'s Code tab: custom groups ("Planning", "Reviews") in an order, a Pinned
  section, the rest grouped by project, side sessions nested under the session that started them,
  and an archive;
- in the **Codex app**: pinned threads, custom sections, projects in their own order, threads filed
  under projects, and an archive;
- in **Herdr**, the terminal multiplexer for coding agents: named workspaces holding named tabs,
  each tab's panes running agents.

Agent Monitor lists the same sessions its own way: by agent, then project, then session families
(the tree), or as one list by activity (#s206). Only one piece of the person's arrangement reaches
it: since #s340 a Claude desktop side session nests under its starter, and only when both are in the
same project (`nestSideSessions`). So the arrangement the person built is lost the moment they open
the monitor, which is where they watch everything at once, including on a phone.

A concrete case. A coordinator session in the Claude app starts workers in four repositories. The
Claude app shows the coordinator with its four workers under it. The monitor shows the coordinator
under its own project and each worker under its repository, in four places, with nothing to say they
belong together. The person then rebuilds the picture by eye on every visit.

## 2. Proposal

Add one choice to the session list: **Arrange**. Its options:

| Option | What the list shows |
|---|---|
| By agent and project | Today's tree. The default. |
| One list by activity | Today's flat list (#s206), moved here from its own checkbox. |
| Like the Claude app | The Claude app's sections and groups, in its order, with its pins and nesting. |
| Like the Codex app | The Codex app's pinned threads, sections, and threads by project, in its order. |
| Like Herdr | One section per Herdr workspace, its tabs as sub-heads, in Herdr's order. |

Choosing an option is the one click. From then on the list **follows** that app: each refresh of
the index (every 5 s today) draws the list in the app's current arrangement, until the person
chooses another option. Nothing is copied into the monitor, so nothing goes stale, and nothing is
written back to the app.

Three rules make the list show the same sessions as the app, so the person can move between the
app and the monitor without hunting for a session:

1. **The app decides which sessions are listed.** Every session the app shows appears, exactly once,
   in the app's section and order, whatever the session filter (#202) or the hidden set says. A
   session the app lists that the monitor has no row for is drawn dimmed, with the app's title and
   "Not in this monitor's list", so nothing the app lists is missing.
2. **Everything else goes to one last group.** A session the app does not hold (a Codex session
   under "Like the Claude app", a Claude Code run in a plain terminal, a pane Herdr has closed) goes
   to **Not in the Claude app** (or the Codex app, or Herdr), collapsed, in today's order. The
   session filter and the hidden set narrow that group exactly as they narrow the tree.
3. **Each session the app lists is its own row, with the app's title.** The app lists each fork as
   a session of its own, so the app's sections do not merge fork families (#142); under "Not in …"
   a family stays whole as it does today.

These rules were revised on 2026-11-06. The first draft applied the filter and the hides to the
whole arranged list and kept fork families whole, which showed fewer sessions than the app did and
under different titles.

While an app's arrangement is shown, a line at the top of the list says so ("Arranged like the
Claude app · follows it") with a control that goes back to "By agent and project". The choice is
remembered per browser, as the flat list is today (`localStorage`), so a phone and a desktop can
arrange differently.

**Order and grouping inside a section are the monitor's own** (revised 2026-11-06). The app
decides which sessions a section holds and under which sub-heads; how they are sorted and grouped
there is a menu on each section's head with the Claude app's section menu's choices. The menu is
the ≡ control at the head's right end, beside its chevron, as the Claude app places its own; a
section head carries no count. On a phone the menu opens as a sheet at the foot of the screen over
a dim, with 44px rows; a tap on the dim closes it and reaches nothing beneath. Held sideways it is
a popover at its control's side, as on a desktop, over a clear scrim that still takes the closing
tap.

| Choice | Options |
|---|---|
| Group by | As in the app (its sub-heads), None, Project, Date (Today, Yesterday, Previous 7 days, Previous 30 days, Older), State (Active, Blocked, Idle, as the session filter defines them, #202) |
| Sort by | As in the app, Last updated, Date created, Name |
| Order | Descending, Ascending (for a sort other than "As in the app") |
| Reset | Back to the app's order |

Each section's choice is remembered per browser and is never written to the app; the default is
"As in the app" for both, so out of the box the list matches the app. The reason for the split: a
sort or group change is in no transcript; the monitor sees it only in the app's own files, once
the app has saved it there (§3.1, §3.2), and it covers only the sessions that app holds. Which
sessions sit where is what the person spent effort arranging, and is readable; the order inside a
section is a click away, and the monitor's own menu applies at once, on a phone too, and to any
section. "As in the app" follows the app's sort as soon as the app has saved it.

## 3. What each app holds, and how the monitor would read it

Each app was read on one Mac on 2026-11-06, by copying its files to a scratch directory and reading
the copies (never the live files), or through its own read-only commands. Counts are from that
machine and show shape, not what is typical.

### 3.1 The Claude app

The Claude desktop app's Code tab, version 4.16336.0 on the machine read (bundling Claude Code
2.1.309 and 2.1.317).

**What the person arranges.** The sidebar is a list of sections in an order the person sets: a
built-in Pinned section, a built-in Routines section, one section per custom group, and a built-in
Sessions section holding everything else, grouped (by project there) and sorted (by recency there).
Each section can be collapsed or switched off. Sessions are filed into groups; a pinned session keeps
its group. A side session is drawn under the session that started it, inside that session's group,
whatever its own project. Archived sessions leave the sidebar. The person can rename a session.

**Where it lives.** In two places a separate process can read as plain JSON:

| File | What it holds |
|---|---|
| `~/Library/Application Support/Claude/claude-code-sessions/<account>/<org>/local_<uuid>.json`, one per session (78 on that machine) | `cliSessionId` (the transcript's id, the monitor's row id), `title` and `titleSource` (`user`, `auto` or `tool`), `isStarred` (pinned), `isArchived`, `cwd`, `lastActivityAt`, `spawnedFrom` (the starter). Already read by the monitor since #s340 |
| `~/Library/Application Support/Claude/claude_desktop_config.json`, under `preferences.epitaxyPrefs` (the app's settings schema marks it UI state) | `dframe-code-sections["<account>/<org>"].sections[]`: each section's `id`, `kind` (`pinned`, `routines`, `manual`, `sessions`), `name`, `order`, `sortBy`, `ascending`, `collapsed`, `shown`. `dframe-group-scopes["<account>/<org>"]`: the groups (`cg-<uuid>`, `name`), each session's group (`assignments`, keyed `code:local_<uuid>`) and each group's order. `starred-local-code-sessions` and `dframe-local-slice.pinnedOrder`: the pins and their order |

The app's live copy of the same state is the renderer's `localStorage`, a Chromium LevelDB store
under `Local Storage/leveldb/` that the app compacts continuously. It was decoded from a copy to
check the JSON file against it. They agreed on all 15 group assignments and their order, and on the
section list, names, order and collapsed state, which matched what the app's own sidebar reported at
the time. The one difference was a group created recently and still empty. It was in the file's
section list but not in its group list. The group sizes the app reports (12, 2 and 31) are the
assigned sessions plus their side sessions (3 + 9, 2 + 0, 10 + 21), which confirms that side
sessions follow their starter's group.

Not readable outside `localStorage`, and not needed: unread marks (the monitor keeps its own). No
group colour is stored. The split-view panes are in the same `epitaxyPrefs` and are out of scope
(§7).

**Sort and group settings.** A section's `sortBy` is `recency`, `alpha` or `created` (the app's
own sidebar tool names them so), with `ascending`. Its grouping (`date`, `project`, `state`,
`custom` or `none`) is the section's `groupBy`; a section whose grouping was never changed has no
such key, which means the app's default (by project for Sessions, none for a custom section). A
change made in the app reached the file within a minute when observed on 2026-11-06 (a custom
section's sort set to date created: the file held `sortBy: created` and `groupBy: none`, written 40
seconds before it was read). The prototype follows `sortBy` and `ascending`; of `groupBy` it reads
only whether Sessions is grouped by project (any other value draws Sessions as one list), so a date
or state grouping chosen in the app is not followed by "As in the app" and is set with the
monitor's own menu instead.

**Mapping.** A record's `cliSessionId` is the monitor's row id; all 78 records mapped to a
transcript. Group assignments name the desktop id (`local_<uuid>`), which the record maps to the row.

**What the app draws, checked against its own sidebar listing** (the app's `list_sections` and
`list_sessions` answers, 2026-11-06): a pinned session is drawn under Pinned only, though it keeps
its group; a side session is drawn under its starter and is reported in its starter's group; the
Sessions section groups by project (`group_by: project`) and sorts by recency; a section switched
off (`shown: false`) is not drawn; an archived session is not drawn. The prototype's arrangement of
that machine's 60 visible sessions matched the listing section for section, and every one of them
was a monitor row.

**Assessment.** Readable today from two plain JSON files the monitor half-reads already, with no
supported interface and no format version. One fact is not established: how soon the app rewrites
the config file after the person moves a session (it was 45 minutes old when read, and the session
records were minutes old). That is checked before building, by moving a session between groups and
watching the file. If the file proves slow, the alternative is reading the same keys from a copy of
the LevelDB store, which is a heavier and more fragile reader.

### 3.2 The Codex app

OpenAI's Codex desktop app; on the machine read it is `/Applications/ChatGPT.app` (bundle id
`com.openai.codex`, version 26.1103.31037, bundling `codex-cli` 0.167.0-alpha.3.2).

**What the person arranges.** Projects, each a name over one or more folders, in an order they set.
Threads, each assigned to a project by the person: membership is not derived from the folder (15
threads sat outside their project's folders, and 81 inside a project's folder had no project). Custom
sidebar sections holding threads, in an order, some collapsed. Pinned threads, in an order. Archived
threads. Renamed titles. (Sub-agent threads and `codex exec` runs are in its database but not in
its sidebar: see "What the app lists" below.)

**Where it lives.** In three stores, with a migration between them half done:

| Store | What it holds | Status |
|---|---|---|
| `~/.codex/.codex-global-state.json` | The app's own window state: project order, the sidebar's section list, order and collapsed set, each thread's project, four pin lists | Private. No schema (keys carry `-v1`, `-v3` suffixes), 2.5 MB, rewritten every few minutes |
| `~/.codex/state_5.sqlite` | The app-server's database: `projects`, `project_roots`, `threads` (`project_id`, `thread_section_id`, `archived`, `name`, `is_pinned`), `thread_sections`, `thread_spawn_edges` | Versioned migrations (62 on that day). Already read, read-only, by the monitor for thread titles |
| `~/.codex/sqlite/codex.db` | The desktop app's own catalogue | Migrated with each app version |

Projects and sections agree between the first two stores. **Thread membership does not**: the
database names a project for 588 threads, disagrees for 1 and is empty for 655, and the state file's
own flag says thread assignments are not migrated yet. **Pins are unresolved**: the state file holds
four pin lists of 7, 13, 11 and 10 threads that disagree with one another, the database marks none,
and which list the app draws was not established.

**A supported way in.** `codex app-server` (marked experimental) answers JSON-RPC methods
`project/list`, `threadSection/list` and `thread/list`, and sends `project/changed`,
`thread/project/updated` and `thread/section/changed` notifications. The monitor's
`claude-monitor-codex` crate already speaks this protocol for `project/list` and `thread/list`. No
method for pins was found.

**Mapping.** A thread's `rollout_path` names the rollout, and the rollout's name without its
extension (`rollout-<timestamp>-<id>`) is the monitor's row id; the bare thread id is not. A
sub-agent's parent is in `thread_spawn_edges`, and also in its rollout's `session_meta`, which the
monitor reads. The database keeps no order for sections; the prototype draws them in the order they
were made.

**Compressed rollouts.** Codex compresses a rollout untouched for seven days into
`rollout-….jsonl.zst`; of the 612 live threads the app files into a section or a project, 611 were
stored that way. The monitor reads them since #s383; before that, "Like the Codex app" drew them
dimmed, as sessions the app lists that the monitor has no row for.

**What the app lists.** The app keeps its own catalogue of the threads it lists (`local_thread_catalog`
in `~/.codex/sqlite/codex.db`). Measured on that machine: the 81 `codex exec` runs and the sub-agent
threads (971 reviewer sub-agents, and every spawned child) are in the threads table but not in the
catalogue, and none of them is filed into a section or a project. The prototype therefore draws only
catalogued threads, read with the app's own filter (its `readEntries` query): the rows for this
machine (`host_id` `local`; a remote host's threads have rows of their own) that the app's last scan
still found (`missing_candidate` 0; a thread a scan no longer finds is marked so and hidden at once,
and deleted later).

The app's own code says how it fills that catalogue (its catalogue builder, read in app 26.1103.31037
on 2026-11-07): it leaves out a thread that is ephemeral, has a parent thread, or has one of three
internal `threadSource` values, and a thread whose `source` is `exec` or is not a string (a
`subagent` object, or any object other than `{"custom": "<name>"}`). Every other source is listed:
the app's own threads (`vscode`), the CLI's (`cli`), an app-server or MCP run, and `unknown`.

Where the catalogue is missing or does not read (a machine read during review had no `codex.db`),
the prototype applies the parts of that rule the database can answer — the `source`, and a parent,
which it records as a sub-agent edge (`thread_spawn_edges`) or an attached parent
(`attached_thread_parent_id`) — and the line over the list says the list is the monitor's guess.
Whether a thread is ephemeral is not recorded. Measured 2026-11-07 on the first machine, the guess
holds 2,790 live threads where the catalogue lists 2,772. The 18 more are `vscode` threads: 9
forked from another thread (`thread_source` `agent_forked_thread`; the app lists 3 others), and 9
with nothing in the database to say why the app leaves them out. A database with no `source` column
(an older app) is still read; without the catalogue it then leaves out only threads with a parent,
and the note says so. A busy catalogue, like a busy database, keeps the last read.

**The sidebar itself** is only in the state file, under `electron-persisted-atom-state`, checked
against the app's own sidebar on 2026-11-06:

| Key | What the sidebar shows from it |
|---|---|
| `sidebar-custom-sections-v3` (per account) | `sectionOrder` (`pinned`, `orbit`, `agents`, `custom:<id>`…, `threads`, `chats`); each custom section's `name` and `itemKeys`, a project (`codex:project:<id>`) or a thread (`codex:thread:<host>:<id>`); `collapsedSectionIds` |
| `sidebar-collapsed-sections-v1` | Whether Pinned, Projects (`threads`) and Chats are collapsed |
| `sidebar-project-expanded-v1-codex:<project id>` | Whether a project folder is open |
| `pinned-thread-ids` (top level) | The pins Pinned draws |
| `project-order`, `local-projects` (top level) | The Projects list's order and names, in the state file's project ids, which `app-server-project-id-by-legacy-project-id-by-host` maps to the database's |
| `thread-project-assignments`, `projectless-thread-ids` (top level) | A thread's project while the app has not moved it into the database, and the threads Chats draws |

The app's sort settings are in the same file: `flat-project-sidebar-preferences-v1` holds
`chatSortMode` (threads in a folder and in Chats: `manual`, `updated_at` or `created_at`) and
`projectSortMode` (`manual`, `priority`, `created_at` or `updated_at`, from the app's own code). On
that machine `projectSortMode` was `updated_at`, yet the Projects list the app drew followed
`project-order`, so the prototype keeps `project-order` and reads only `chatSortMode`.

A custom section can hold project folders, not only threads: on that machine "Platform" held five
projects, and the Projects list held the other nine, in the app's order. The database's own
`thread_sections` held a section named "Pinned" with 11 threads, which is not what Pinned draws.

**Assessment.** The sidebar is readable only from the private state file, which the app rewrites
every few minutes and which has no schema (its keys carry version suffixes). The prototype reads it
read-only, re-reading only when it changes, and takes titles, recency and archive from the
database; with no state file it falls back to the database's sections and says so. Read through
the app-server instead once it offers the sidebar's sections, pins and folders.

### 3.3 Herdr

Herdr 0.9.5, socket protocol 22. Herdr publishes its own schema (`herdr api schema --json`).

**What the person arranges.** Servers (Herdr calls them sessions), each holding workspaces, each
holding tabs, each holding panes; a pane may run an agent. Workspaces and tabs have a `label` the
person can rename and a `number`; workspaces and tabs can be moved (`insert_index`), which suggests
`number` is the display position.

**Where it lives, and how to read it.** In the running Herdr server, through its supported CLI:

- `herdr session list --json` lists the servers (`name`, `running`, `socket_path`);
- `herdr --session <name> api snapshot` returns one JSON object with every workspace (`workspace_id`,
  `number`, `label`), tab (`tab_id`, `workspace_id`, `number`, `label`) and pane (`pane_id`,
  `tab_id`, `agent_session`, `agent_status`), in one call.

The socket API also offers `events.subscribe` for workspace, tab and pane changes; the CLI does not
expose it, and its delivery was not tried. Polling the snapshot at the index's refresh is enough.

**Mapping.** A pane's `agent_session.value` is the agent's session id, which the agent's Herdr hook
reports at start; it is the monitor's row id. A pane with no agent session (a shell, or an agent
without the hook) is not a row and is skipped. The hook reports only at the start (#s349), so after
a session moves to a new id (#s294) the pane still names the old, finished one. Each entry therefore
also names its pane (`<server>|<pane id>`), and the page prefers the row the monitor itself links
to that pane (a row's `term: "herdr"`, `target` and `herdrSession`), using the hook's id only where
the monitor links none.

**What the monitor already does.** It finds a session's Herdr pane from the agent process's
environment (`HERDR_PANE_ID`, `HERDR_SOCKET_PATH`), and polls `herdr agent list` every 2 s per server
for the agent's state (`state.rs`, `HerdrReader`). It never reads workspaces or tabs.

**Caveats.** Pane ids repeat across servers (`w1:p1` on each), so with more than one running server
the layout gains a level: server, then workspace. No field says whether a label was typed by the
person or is Herdr's default (the folder's name for a workspace, the number for a tab), so a default
is shown as Herdr shows it. On the machine read, one server was running with no agent pane, so no
live pane-to-row mapping was observed; the Claude hook (v11) and the Codex hook (v10) were installed.

### 3.4 What maps onto the list

| In the app | In the monitor's arranged list |
|---|---|
| Claude app: Pinned | First section, in the app's pin order |
| Claude app: a custom group | A section with its name, in the app's section order, collapsed as in the app; a section the person switched off is not drawn |
| Claude app: Sessions, grouped by project | A last app section with one sub-head per project, sorted as the app sorts it |
| Claude app: a side session | Nested under its starter, in the starter's section, across projects |
| Claude app: an archived session | A collapsed Archived section, before "Not in the Claude app" |
| Claude app: a session's title (typed by the person, or the app's own) | The row's name in this view |
| Claude app: Routines | Nothing: a routine is not a session row |
| Codex app: a pinned thread | Pinned, in the app's pin order, collapsed as in the app |
| Codex app: a custom section | A section with its name, in the app's order, collapsed as in the app; its project folders as sub-heads, its threads as rows |
| Codex app: a project | A folder sub-head, in its custom section or under Projects in the app's project order, closed unless opened in the app, drawn even when empty |
| Codex app: a thread with no project | Chats |
| Codex app: an archived thread | Not drawn, as the app does not draw it |
| Codex app: a sub-agent thread, or a `codex exec` run | Not drawn, as the app does not list it |
| Herdr: a workspace | A section with the workspace's label, in Herdr's order |
| Herdr: a tab | A sub-head inside it |
| Herdr: an agent pane | The session's row |
| Herdr: a shell pane | Nothing |
| Anything else the monitor lists | "Not in …", in today's order |

## 4. The model

A **layout** is what the server hands the page for one source:

- `source`: `claude-app`, `codex-app` or `herdr`;
- `available`: whether the source was read; when it was not, `reason` says why in words the page
  shows (for example "The Claude app's session list was not found on this Mac");
- `sections`: an ordered list. Each has an `id` stable across refreshes, a `label` taken verbatim
  from the app, whether the app shows it `collapsed`, ordered `entries`, and ordered `groups` (a
  sub-head with its own `label` and `entries`: a project, a Herdr tab). An entry names a monitor row
  id, carries the `title` the app shows when it keeps one, and may hold child entries (nesting);
- `note`: anything the person should know about how faithful the list is (for example how many
  Codex threads have no project in the app's database yet);
- `pending: true`, with no sections, before the source's first read has landed;
- `stale: true` when the read is from before the source's reader last stopped (it can be an hour
  old when a phone comes back to the page); the list is drawn under "updating…" until a fresh read
  lands.

A headless worker (a row the monitor marks `startedBy`, #s53) lives in its starter's Agents pane
and is never drawn here, even where an app lists it.

The page then applies the three rules of §2: it draws each entry once, at the first entry naming it,
as that session's row (a dimmed row when the monitor has none), nests children under their parent
entry, and puts every row no entry named into the last group, under the filter and the hides. The
server does not know the page's filter, hides or families, so it never decides them. A Herdr hook
may report a bare session id where the monitor names a Codex row by its rollout; the page matches an
entry to a row whose id ends with it.

Nesting from the app may cross projects. That is the point of the coordinator case in §1, and it is
safe here because every row in this view already says its project, as rows in the flat list do.
Under "By agent and project" nothing changes: the tree keeps #s340's same-project rule.

## 5. Interface

**A new route, `GET api/layout?source=<name>`**, answers one source's layout as JSON. It is asked
only while an app layout is chosen, at the index's own refresh. `api/sessions` rows do not change.

**A request never reads an app.** Each source has a reader thread of its own, started by the first
ask and kept only while a page follows that source: it reads every 3 seconds and ends 30 seconds
after the last ask. The route answers with that thread's last read, or with `pending: true` before
its first read lands; a read kept from before the reader last stopped is answered with
`stale: true`. In both cases the page asks again a second later, through one timer that every
answer re-arms, so retries never pile up. No lock is held while a source is
read, so a slow herdr or a busy database delays nobody's request; herdr is asked at the reader's
own pace, as the monitor's `HerdrReader` asks it (#s238). The Codex reader gives SQLite 250 ms
rather than its default five seconds, and a busy database keeps the last read. **`GET api/layout`
with no `source`**, asked as the filter sheet opens, checks files only — the Claude app's store and
settings file, the Codex app's database, the herdr program — and reads no app. Only that check
decides whether a source can be chosen: a read that failed is reported beside it as `lastError` and
never disables it, since choosing it is what reads it again. A person who never chooses an app
never has one read.

The two answers, exactly:

| Field | Type | Meaning |
|---|---|---|
| `sources` | array of objects | One per source, in the order `claude-app`, `codex-app`, `herdr` (no `source` asked) |
| `sources[].source` | `"claude-app"` \| `"codex-app"` \| `"herdr"` | The source |
| `sources[].label` | string | The option's name |
| `sources[].available` | boolean | Whether the source's files are where its reader looks |
| `sources[].reason` | string or null | Why not, when `available` is false |
| `sources[].lastError` | string or null | What the source's last read met, when it failed and `available` is true |
| `source`, `label` | as above | The layout's source (`source` asked) |
| `available` | boolean | Whether the last read succeeded |
| `reason` | string or null | Why it did not |
| `note` | string or null | How faithful the list is |
| `sections` | array | §4's sections; empty when `available` is false or `pending` is set |
| `pending` | `true`, or absent | No read has landed yet |
| `stale` | `true`, or absent | The read is from before the reader last stopped |

A source that is not one of the three answers 404.

The server, not the page, reads the stores: the page may be a phone across the tailnet, which cannot
read the Mac's disk, and the server already keeps a cached reader for one of them (#s340's
`desktop_sessions.rs`, which re-reads a record only when its modification time or length changes).
Each reader:

- reads only, never holds a write lock on the app's files, and makes none of its own. A reader of
  an SQLite database in WAL mode makes its `-shm` and `-wal` beside it when they are absent, and
  they stay after it closes (measured with SQLite 3.54); so a WAL database with no `-wal` (no writer
  has it open, and every committed page is in the main file) is opened immutable, which makes
  nothing, and one a writer has open is read through that writer's own `-wal`. An immutable read
  takes no lock, so when a writer reaches the file during it (a `-wal` appears, or the file
  changes), the read is treated as busy and the last read stands, whatever the read returned, an
  error too. In practice the immutable branch
  is rare: the monitor's own Codex title reader (in the agents crate, outside this change) opens the
  same database read-only in the usual way, so on a machine running the monitor its `-wal` and
  `-shm` are usually already there;
- never keeps part of a list as the whole: a row whose value has an unexpected type, or whose text is
  not UTF-8, is counted in the note and the rest still read, but any other error part-way through a
  scan fails the read. The Codex reader reads its tables in one read transaction, so a commit
  between two queries cannot pair a thread with edges from another moment, and it asks the schema
  which optional parts (the sub-agent edges, `source`, the attached parent) an older database
  lacks rather than reading that from an error;
- skips a record it cannot parse and ignores keys it does not know, as #s340's reader does;
- has an environment override for its location, so the browser cases run on a hermetic store, as
  every other store here has;
- says when it meets a format it does not recognise, in the option's reason and in the monitor's
  log, so an app update that moves the arrangement is noticed rather than drawn wrong. The format
  sweep (`agent-replay --unknown`, #264) reads transcripts only; whether it should also read these
  stores is left to the implementation.

## 6. When a source is missing or changes

| Situation | What the person sees |
|---|---|
| The app is not installed, or its store is absent | The option is shown disabled, with the reason as its tooltip and second line. |
| The store is present but does not read, or its format is not recognised | Still offered, since only missing files disable a source; its second line says "Last read: …" with what the read met. Chosen, it is read again, and while it does not read it falls back as in the next row. |
| The chosen source fails at a later refresh | The list falls back to the arrangement the app replaced ("By agent and project", or "One list by activity") and its head says, in amber, "Showing by agent and project: the Claude app's list could not be read", with the reason. The choice is kept; the next refresh that reads the source draws it again. A failure kept from before the reader last stopped says "Reading …" instead, since the source is being read again. |
| The Codex app keeps no catalogue of its threads, or it does not read | The list is drawn, leaving out `codex exec` runs, sub-agents and threads started from another thread by their source and parent (§3.2), and the line over it says the list is the monitor's guess. |
| A record half-written by the app | Skipped for that refresh; read again at the next. |
| A reader on a phone or another desktop | Sees the arrangement on the monitor's own machine, which is the machine the sessions run on. |

## 7. Scope

Needed:

- reading the three apps' arrangements, read-only, on the machine the monitor runs on;
- the Arrange choice in the session list, on desktop and in a phone's drawer;
- the fallback of §6 and the three rules of §2.

Not needed, and not proposed:

- **writing to any app.** The monitor never changes the Claude app's, the Codex app's or Herdr's
  arrangement or storage. Copying the monitor's own arrangement into an app is a different feature.
- **window layout**: which sessions an app shows side by side, window sizes, a Herdr pane's
  geometry. The monitor shows one session at a time.
- **editing the arrangement inside the monitor** (renaming a group, pinning a session there).
- **other machines.** The fleet view (`agent-monitor-fleet`) shows each machine's own monitor, and
  each of those arranges by its own machine's apps.

**Language.** Agent Monitor's interface is English only, and stays so: the new labels ("Arrange",
"Like the Claude app", "Not in the Claude app", the fallback notice) are English, and no
localization is added. Group, section, workspace and tab names are shown as the person wrote them
in the app.

## 8. Alternatives

- **Copy once, then own it.** The click copies the app's arrangement into a monitor-owned
  arrangement the person can then edit. That needs a groups-and-pins model the monitor does not have,
  and the copy goes stale the next time the person rearranges the app, so they would click again
  anyway. Following is the same click with neither cost.
- **Two-way sync.** Rearranging in the monitor would move sessions in the app too. The Claude app
  offers no way to write its arrangement from outside, and writing its files races the running app,
  which rewrites them itself. The Codex app-server (`thread/section/move`, `project/update`) and
  Herdr (moves and renames) do accept writes, but using them makes the monitor a second editor of
  the person's arrangement, with conflicts of its own. A separate feature, not proposed here.
- **Labels only.** Keep today's tree and add a chip with the app's group name to each row. Smaller,
  but the coordinator case stays scattered across projects.
- **Only widen #s340.** Let `nestSideSessions` nest across projects in the tree. The smallest change
  that fixes the coordinator case, and nothing else: groups, pins, Codex and Herdr stay unseen. It
  is worth doing even if this proposal is not taken, and it is a subset of it.
- **A glyph in the sidebar's head** instead of a row in the filter sheet. Faster to reach, but the
  head's controls row is measured against the sidebar's 232px minimum (#210, #223) and is full; the
  sheet already holds the flat-list choice, which this replaces.

## 9. Costs and risks

- **Undocumented formats.** The Claude app's files and the Codex app's state file are private, with
  no format version; the Codex app's database has versioned migrations but is the app-server's own;
  only Herdr publishes a schema and a versioned protocol. Each can change with an app update, without
  notice. The fallback keeps the list usable, the unknown-shape report makes the change visible, and
  a format change costs a reader update, not a redesign.
- **Read cost.** The Claude app's records are already read and cached by #s340; the config file adds
  one `stat` per refresh and a re-read only when it changes. Herdr adds one `api snapshot` per running
  server per refresh, beside the `agent list` the monitor already runs every 2 s. The Codex app adds a
  read of its database or app-server only when that store changes.
- **Three readers to maintain**, one per app, each with a hermetic fixture.
- **Remote readers see group names.** Group and workspace names travel to a paired phone like
  session names already do; the mask policy (#365) applies to them as to any JSON response.

## 10. How a reviewer can tell it works

**Run on the prototype (#s387), on one Mac, 2026-11-06:**

- Rust unit tests (`claude-monitor/src/layout.rs`) for each reader on synthetic stores: the Claude
  app's sections, groups, pins, side sessions and archive from its settings and records on disk;
  the Codex app's sidebar from a state file in its measured shape (pins, custom sections holding
  project folders or threads, the Projects list, Chats, collapsed sections and closed folders, a
  thread's project from the state file before the database), its threads from a database in its
  measured shape, and a state file or database of another shape refused; Herdr's workspaces, tabs and agent panes from
  snapshots of one and of two servers. A route test for `api/layout`.
- A node contract case for the page's placement: every entry once, hidden rows and rows with no
  index entry kept, repeats dropped, empty sections not drawn, a bare session id matched to a
  rollout-named row.
- A locally started monitor on this machine's apps, in the built-in browser: "Like the Codex app"
  drew the app's sidebar as the app showed it (Pinned closed, "Platform" with its five folders,
  the second custom section closed, Projects with the other nine folders in order, all closed, then
  Chats). All 1,276 threads it placed at the top level were monitor rows once #s383's reader for
  compressed rollouts was merged in (85 before it).
- In the same monitor, the section menu: sorting Ongoing by name, grouping it by date and by
  state, and Reset back to the app's order; the choice survives a reload and Reset clears it; the
  chosen options are marked. With Ongoing set to date created in the Claude app, "As in the
  Claude app" listed it in the records' date-created order.
- At 375 × 812 (a phone upright): the menu opens as a sheet at the foot, 8 px from each side, with
  44 px rows and a 40 px control in the 44 px head; a tap on the dim closes it and leaves the drawer
  and the open session as they were.
- In the same monitor: each option draws its
  app's list; the Claude app's matched its own sidebar listing (§3.1); an unreadable source is
  offered disabled with its reason; a failing read falls back with the amber line and recovers; the
  line's ✕ returns to the tree; the choice survives a reload; the phone drawer at 375 px shows the
  line and the list without scrolling sideways.

**Added after the first review (browser cases, `claude-replay-browser-tests/tests/layout_sync.rs`,
on hermetic stores: the Claude app's records and settings written by each case, and a herdr
stand-in the harness gives every monitor, so no case reads this machine's apps or runs its herdr):**

- "Like the Claude app" draws the fixture's group and Sessions with the app's titles and a side
  session nested; an idle and a hidden session are still drawn in their group; the rest waits under a
  collapsed "Not in the Claude app"; a reload keeps the choice and "One list by activity" behind it,
  which the line's ✕ returns to.
- A source whose files are absent is offered disabled with its reason; its settings removed while
  chosen, the list falls back under an amber line, and returns when they come back.
- "Like Herdr" draws a workspace and its tab with the agent pane's row, through the stand-in.
- At 390 × 844 the section menu opens as a sheet from a 44 px glyph with 44 px rows, a tap on the
  dim closes it and chooses nothing, the line's ✕ is 44 px, and Arrange's two-line rows hold their
  reason; at 844 × 390 the menu is a popover at its glyph's side.

**Added after the second review:**

- Unit cases: a read that failed is the source's `lastError` and the source stays available; only
  missing files make it unavailable. A read from before the reader stopped is answered `stale`
  until the new reader's first read lands. The reader case holds each read on a gate rather than
  timing the request, so a loaded machine cannot flake it (#s300). The Codex reader makes no `-shm`
  or `-wal` beside a WAL database (with the immutable open turned off, the case fails naming both
  files); without a catalogue it leaves out `exec` and sub-agent threads by their source and says
  so.
- The Claude app browser case goes on: its settings hold no sidebar layout, the list falls back; left
  with the line's ✕, the option is still offered with "Last read: …"; chosen again, it reads again.
  With the old rule (a failed read disables the source) the case fails at that step.
- A Codex app browser case (macOS only, its databases built with the system's own `sqlite3`): the
  app's folder and Chats under the app's titles; a `codex exec` run the database holds waits under
  "Not in the Codex app", left out by its source while there is no catalogue (the line says so) and
  by the catalogue once one appears, which then decides alone; no `-shm` or `-wal` is made beside
  either database. With the source rule turned off the case fails.
- At 844 × 390 the section menu's scrim no longer dims the screen; it still takes the closing tap.

**Added after the third review:**

- Unit cases: an immutable read a writer reached (its `-wal` appears) is busy, and an ordinary read
  is never treated so; only a value of the wrong type in a row is counted, while an error such as
  `SQLITE_CORRUPT` is not; a database with no `source` column still reads and says so; the
  headless rule follows the app's (an MCP or app-server run, `unknown` and a custom source are
  listed; `exec` and any other object are not).
- The Codex browser case's `sqlite3` runs with `-init /dev/null`, so no `~/.sqliterc` reaches it.

**Added after the fourth review:**

- Unit cases go through the reader itself, with a writer that arrives after each file is read: a
  checkpoint (a new modification time) or a write (a `-wal`) reaching the thread database or the
  catalogue makes the read busy, a failed catalogue query too; a catalogue query that fails with no
  writer is a guess said in the note. A database whose last page is not a page fails the read
  rather than keeping the rows before it. A name that is not UTF-8 costs its row, counted. Read
  from a real database: an exec run, a sub-agent and an attached thread are left out without a
  catalogue and a custom run is listed; with one, a missing candidate and another host's row are
  not listed. Each was checked by turning its rule off: the case for it fails (nine rules: the two
  busy checks, the file-time half of the writer check, the row-error rule, UTF-8, the two catalogue
  filters, the parent rule, the attached parent).
- The Codex browser case adds a custom run (drawn without a catalogue) and a sub-agent (never
  drawn), and its catalogue holds a missing candidate and another host's row for Z, which stays
  out.
- The phone case holds the section menu, sideways, at 874 × 402 with `innerHeight` 292, as Safari
  with its tab bar has it on an iPhone 17 Pro: its vh counts the whole 402 px, the page shows 292.
  The popover is capped at `innerHeight` less 16 px, and ends on the page with its last row in
  reach. Without that cap the case fails with the walk's own numbers (top 13, 297 px tall: the 74vh
  cap of `#app .navigator-options`, which outranks the menu's own `100vh` rule).

The iOS Simulator walk was run by the reviewer on 6d7a9f98 (iPhone 17 Pro, Safari with its tab bar;
upright, sideways, light and dark): everything held but the sideways section menu, whose foot was
off a 292 px page; the fix above is for that. Dark section titles are faint for a reason older than
this change (`.agent-label` has no dark value), tracked in the queue apart from it.

Not run: Linux. The Herdr browser case has no row the monitor links to a pane, so the pane-first
matching is held by the node contract only. The Codex reader's unit cases and its browser case
compile on macOS only, as the agents crate's Codex reader does (a Linux build compiles no C
dependency), so the Linux CI does not run them.

**Proposed checks for a release:**

- The iOS Simulator walk, sideways, for the section menu's fix.
- A coordinator with side sessions in other projects nests them under "Like the Claude app" and
  leaves the tree as it is.
- A node contract case: given rows and a layout, every row appears exactly once.

## 11. Open questions

1. **Follow, or copy once?** Recommended: follow (§8). Copying once is the fallback if following
   proves too costly to read.
2. **Which sources first?** Recommended: the Claude app first (its reader exists, and the coordinator
   case is the common one), then Herdr (a published schema and a supported CLI, and the monitor
   already polls each server), then the Codex app, whose sidebar is readable only from a private
   state file that the app may change in any release (§3.2). The prototype offers all three; the
   Codex reader is the one most likely to need an update after an app release, and says so in its
   reason when the file's shape is not one it knows.
3. **A new route, or new fields on `api/sessions` rows?** Recommended: the route (§5). It is a public
   interface the fleet view and the attention report do not read today, so it changes nothing for
   them; it still needs the maintainer's decision, as #s342 did.
4. **Per browser, or one choice for every browser?** Recommended: per browser, as the flat list is. A
   shared choice would need a server-side preference, which the monitor has not had since the shell
   preference (`ui.json`, `api/ui`) was retired with the classic view (#s98).
5. **Should the tree also nest across projects** (the "Only widen #s340" alternative), independently
   of this proposal?
6. **Is the Claude app's config file fresh enough?** To be measured before building (§3.1): move a
   session between groups and time the file's rewrite. If it lags by minutes, the choice is between
   accepting that lag (the strip can say when the arrangement was read) and a LevelDB reader.

## 12. Demo

`design/layout-sync-demo.html` is a clickable mockup on invented sessions: the monitor's sidebar on
the left, the chosen app's own list beside it for comparison, the Arrange choice, each source, the
"Not in …" group, the filter applied to an arranged list, a missing source, and a phone width. It
was made before the revision of §2 and still applies the filter to the whole arranged list; the
prototype (#s387) follows the revised rules.
