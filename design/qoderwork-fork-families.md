# Investigation #135 — QoderWork's "duplicate" sessions in one workspace directory

**They are FORKS, not duplicates.** A QoderWork workspace directory can hold a dozen session
ids whose dialogs overlap almost entirely, because forking a session COPIES the conversation
up to the fork point and continues from there. The overlap is real and expected; the sessions
are not redundant, and deleting or hiding them loses work.

**Verdict: GROUP them into a fork family, do not dedup.** The grouping key is already on disk
and exact — no heuristic, no content comparison needed.

## The signal: a per-session sidecar

Beside every `<sid>.jsonl` QoderWork writes `<sid>-session.json`:

```json
{"id":"0d3b6243-…","parent_session_id":"","fork_from":"fcd5fe45-…",
 "title":"安装MCP服务器 (Fork)","created_at":1780283466227,"updated_at":1780311574726,
 "working_dir":"…","cost":0,"message_count":0, …}
```

`fork_from` names the session this one was forked from. That is the family edge.

## Measured on this machine

31 QoderWork sessions carry a sidecar:

| | count |
|---|---|
| roots (no `fork_from`) | 11 |
| forks (`fork_from` set) | **20** (65%) |
| forks whose `fork_from` is missing from the same directory | **0** |
| sidecars with a non-empty `parent_session_id` | **0** |

Two directories hold a 1-root + 9-fork family; two hold 1 + 1. Forking is the norm, not an
edge case.

The worst directory (`…mpumt1wjosm5onng`) — root `fcd5fe45` with 151 messages — against each
fork, comparing the semantic message spine (role + text; raw lines never match because every
line carries its own ids and timestamps):

| fork | messages | shared prefix with the root |
|---|---|---|
| 7b9483fe | 153 | 151 (99%) |
| fdf218c0 | 121 | 114 (94%) |
| 0d3b6243 | 110 | 107 (97%) |
| 9246c41f | 95 | 92 (97%) |
| f0d467a8 | 87 | 84 (97%) |
| 31e895ac | 78 | 75 (96%) |
| daf06044 | 69 | 67 (97%) |
| e1174f46 | 28 | 24 (86%) |
| 64906b1b | 17 | 14 (82%) |

Each fork is 82–99% a replay of the root and diverges only in its last few messages. Note
`7b9483fe`: it contains the root's entire conversation and continues past it — **the root is
not necessarily the longest or the newest member**, so "keep the biggest, drop the rest" is
wrong.

## Why the obvious rule ("one directory = one session") is wrong

The `-Users-hong` directory holds **3 roots and 0 forks** — three unrelated sessions that
happen to share a working directory. Collapsing by directory would merge them. The family, not
the directory, is the unit: group by transitive `fork_from`, and a directory may contain
several families plus unrelated singletons.

## Recommended shape (for the implementation task)

1. **Read the sidecar in the QoderWork adapter.** `agents/qoderwork/discover.rs` already owns
   the store walk; `<sid>-session.json` sits beside the transcript it walks. Family resolution
   is a per-directory pass: read the sidecars, follow `fork_from` transitively to a root
   (measured: always resolvable inside the same directory — but the code must still terminate
   on a cycle or a dangling edge and treat that session as its own root).
2. **Family identity = the root's id; family label = the root's title.** Forks are titled
   `"<root title> (Fork)"`, so the root's title names the family without string surgery.
3. **The rail shows one row per family**, expandable to its members — the same show-more
   vocabulary groups already use (#116). Its state and activity are the MAX over members (a
   family is active if any member is), which is also what makes the row stop flapping.
4. **Do not sum cost/turns across a family.** 82–99% of every fork is a replay of the root, so
   adding members would multiply the same tokens by ten. Show the family's largest member, or
   the root, and let the expanded members carry their own numbers.
5. **Never hide a member.** The divergent tail is the whole point of a fork; #113's hide list
   remains the user's own explicit choice, not something forking triggers.

## Secondary finding, relevant to #119

Every one of the 31 sidecars has a **non-empty title** (`安装MCP服务器`, `整理项目文档目录`, …).
#119 originally got QoderWork titles from `sub_chats.name` in the SQLite store, behind a
`qoderwork-titles` feature (bundled rusqlite), macOS-only via `db_path()`. #143 made the sidecar
the first source — a plain JSON file next to the transcript, no database, no platform assumption.
That held until QoderWork stopped writing the sidecar (~July 2026, new sessions carry only an
encrypted `<sid>/state.json`), which made the DB the only source again; the feature gate was then
removed in favor of `cfg(target_os = "macos")`, so every macOS build reads it.

## Conclusion

Group by `fork_from` into families, label by the root, never dedup or hide. Filed as its own
implementation task rather than done under this investigation: it changes discovery output
(a new grouping level the rail must render) and touches the QoderWork adapter's store walk.
