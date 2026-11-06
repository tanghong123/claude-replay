# 让 session 列表按 Claude app、Codex app 或 Herdr 的方式排列

> 本文是 `design/layout-sync.md` 的中文副本；以英文版为准，两份描述同一个设计。
>
> **DRAFT（草案）**（#s382，2026-11-06）。这是一个提案；同一分支上有一个本地原型（prototype，#s387），
> 按修订后的 §2 规则实现；都还没有发布。§3 中关于各个 app 的事实，是 2026-11-06 在一台 macOS 机器上
> 读取的，版本号见各节。demo 是 `design/layout-sync-demo.html`（只含虚构的 session），早于 §2 的修订。

## 1. 问题

同时运行很多 agent 的人，会在运行它们的地方整理它们：

- 在 **Claude app**（Claude 桌面应用）的 Code tab 里：有顺序的自定义 group（分组，例如 "Planning"、
  "Reviews"）、一个 Pinned（置顶）section、其余按项目分组，side session（旁支会话）嵌在启动它的
  session 下面，还有归档；
- 在 **Codex app**（OpenAI 的 Codex 桌面应用）里：置顶的 thread（会话线程）、自定义 section、按自己顺序
  排列的 project、归入 project 的 thread，还有归档；
- 在 **Herdr**（面向 coding agent 的终端复用器）里：有名字的 workspace，里面是有名字的 tab，每个 tab 的
  pane（窗格）里运行着 agent。

Agent Monitor 用自己的方式列出同样这些 session：先按 agent，再按 project，再按 session family
（会话家族，即 tree 视图），或者按活动时间排成一个列表（flat list，#s206）。这个人的整理里只有一处
能传到 monitor：从 #s340 起，Claude 桌面应用的 side session 会嵌在它的启动者下面，而且只在两者属于
同一个 project 时才嵌。所以这个人花心思做好的整理，一打开 monitor 就不见了，而 monitor 正是他同时
查看所有 session 的地方，手机上也是。

举一个具体的例子。Claude app 里的一个协调者 session 在四个仓库里各启动了一个 worker。Claude app
把协调者显示在上面，四个 worker 嵌在它下面。monitor 却把协调者放在它自己的 project 下，把每个
worker 放在各自的仓库下，分散在四个地方，看不出它们是一组。于是这个人每次打开都要用眼睛重新拼出
这张图。

## 2. 提案

给 session 列表加一个选择：**Arrange**（排列方式）。选项如下：

| 选项 | 列表显示什么 |
|---|---|
| By agent and project | 今天的 tree。默认值。 |
| One list by activity | 今天的 flat list（#s206），从它原来单独的复选框移到这里。 |
| Like the Claude app | Claude app 的 section 和 group，按它的顺序，带它的置顶和嵌套。 |
| Like the Codex app | Codex app 的置顶 thread、section，以及按 project 分组的 thread，按它的顺序。 |
| Like Herdr | 每个 Herdr workspace 一个 section，它的 tab 作为小标题，按 Herdr 的顺序。 |

选一个选项，就是那"一键"。从那以后，列表会**跟随**（follow）这个 app：index 每次刷新（目前每 5 秒
一次）都按这个 app 当前的排列来画列表，直到这个人选别的选项。monitor 不复制任何东西，所以不会过时，
也不会往 app 里写回任何东西。

三条规则让列表显示和 app 相同的 session，这样这个人在 app 和 monitor 之间切换时，不用再去找某个 session：

1. **由 app 决定列出哪些 session。** app 显示的每个 session 都会出现，恰好一次，位于 app 的 section
   里、按 app 的顺序，不管 session filter（#202）或已隐藏的集合怎么说。app 列出、但 monitor 没有对应
   行的 session，会以变暗的样子画出来，带 app 的标题和 "Not in this monitor's list"，所以 app 列出的
   东西一个都不少。
2. **其余的都放进最后一组。** app 里没有的 session（在 "Like the Claude app" 下的 Codex session、在
   普通终端里运行的 Claude Code、Herdr 已经关掉的 pane）放进 **Not in the Claude app**（或 Codex
   app、或 Herdr），默认折叠，按今天的顺序画。session filter 和已隐藏的集合对这一组的收窄效果，与对
   tree 完全一样。
3. **app 列出的每个 session 都是单独一行，用 app 的标题。** app 把每个 fork 当作独立的 session 列出，
   所以 app 的 section 里不合并 fork family（#142）；在 "Not in …" 下，family 和今天一样保持完整。

这些规则在 2026-11-06 修订过。初稿让筛选和隐藏作用于整个排列后的列表，并保持 fork family 完整，
结果显示的 session 比 app 少，标题也和 app 不同。

在显示某个 app 的排列时，列表顶部有一行说明（"Arranged like the Claude app · follows it"），带一个
回到 "By agent and project" 的控件。这个选择按浏览器记住，和今天的 flat list 一样（`localStorage`），
所以手机和桌面可以用不同的排列。

**section 内的排序和分组由 monitor 自己决定**（2026-11-06 修订）。app 决定一个 section 包含哪些
session、放在哪些小标题下；它们在那里怎么排序、怎么分组，由每个 section 头上的一个菜单决定，选项与
Claude app 的 section 菜单一致。菜单是 section 头右端、折叠箭头旁的 ≡ 控件，与 Claude app 放置它自己菜单
的位置相同；section 头不显示数量。在手机上，菜单以 sheet 的形式从屏幕底部弹出，背后有一层变暗的遮罩，每行
高 44px；点击遮罩会关闭菜单，这次点击不会落到下面的任何东西上。横屏时它和桌面上一样，是控件旁边的弹层，
背后的 scrim（遮罩层）是透明的，但仍会接住关闭菜单的那次点击。

| 选择 | 选项 |
|---|---|
| Group by | As in the app（app 的小标题）、None、Project、Date（Today、Yesterday、Previous 7 days、Previous 30 days、Older）、State（Active、Blocked、Idle，按 session filter 的定义，#202） |
| Sort by | As in the app、Last updated、Date created、Name |
| Order | Descending、Ascending（排序不是 "As in the app" 时） |
| Reset | 回到 app 的顺序 |

每个 section 的选择按浏览器记住，从不写回 app；两项的默认值都是 "As in the app"，所以开箱即与 app 一致。
这样划分的原因：排序或分组的改动不在任何 transcript 里；monitor 只能在 app 自己的文件里看到它，而且要等
app 把它保存进去之后（§3.1、§3.2），它也只覆盖那个 app 自己的 session。哪些 session 放在哪里，是这个人花
力气整理出来的，而且能读到；section 内的顺序点一下就能改，monitor 自己的菜单立即生效，在手机上也一样，
对任何 section 都适用。app 一保存它的排序，"As in the app" 就会跟随。

## 3. 每个 app 存了什么，monitor 怎么读

每个 app 都是 2026-11-06 在一台 Mac 上读取的：把它的文件复制到一个临时目录后读副本（从不读正在使用的
文件），或者用它自己的只读命令。数量来自那台机器，只说明结构，不代表一般情况。

### 3.1 Claude app

Claude 桌面应用的 Code tab，在被读取的机器上版本是 4.16336.0（内带 Claude Code 2.1.309 和 2.1.317）。

**这个人整理了什么。** 侧边栏是一串 section，顺序由这个人决定：一个内置的 Pinned section、一个内置的
Routines section、每个自定义 group 一个 section，以及一个内置的 Sessions section 装其余所有
session，并分组（在那台机器上按 project）和排序（在那台机器上按最近活动）。每个 section 可以折叠或
关闭。session 被归入 group；置顶的 session 保留它的 group。side session 画在启动它的 session 下面，
位于那个 session 的 group 里，不管它自己属于哪个 project。归档的 session 离开侧边栏。这个人可以给
session 改名。

**存在哪里。** 在两处另一个进程可以按普通 JSON 读取的地方：

| 文件 | 存了什么 |
|---|---|
| `~/Library/Application Support/Claude/claude-code-sessions/<account>/<org>/local_<uuid>.json`，每个 session 一个（那台机器上有 78 个） | `cliSessionId`（transcript 的 id，即 monitor 的行 id）、`title` 和 `titleSource`（`user`、`auto` 或 `tool`）、`isStarred`（置顶）、`isArchived`、`cwd`、`lastActivityAt`、`spawnedFrom`（启动者）。monitor 从 #s340 起已经在读 |
| `~/Library/Application Support/Claude/claude_desktop_config.json` 的 `preferences.epitaxyPrefs` 下（app 的设置 schema 把它标为 UI 状态） | `dframe-code-sections["<account>/<org>"].sections[]`：每个 section 的 `id`、`kind`（`pinned`、`routines`、`manual`、`sessions`）、`name`、`order`、`sortBy`、`ascending`、`collapsed`、`shown`。`dframe-group-scopes["<account>/<org>"]`：各个 group（`cg-<uuid>`、`name`）、每个 session 属于哪个 group（`assignments`，键为 `code:local_<uuid>`）以及每个 group 内的顺序。`starred-local-code-sessions` 和 `dframe-local-slice.pinnedOrder`：置顶及其顺序 |

同一份状态的实时副本在渲染进程的 `localStorage` 里，是 `Local Storage/leveldb/` 下的一个 Chromium
LevelDB 存储，app 会不断压缩它。为了核对 JSON 文件，从一份副本解码了它。两者在全部 15 个 group 归属
及其顺序上一致，在 section 列表、名字、顺序和折叠状态上也一致，并且与当时 app 自己的侧边栏报告的相符。
唯一的差别是一个刚创建、还是空的 group：它在文件的 section 列表里，但不在文件的 group 列表里。app
报告的 group 大小（12、2 和 31）等于分配进去的 session 加上它们的 side session（3 + 9、2 + 0、
10 + 21），这证实了 side session 跟随其启动者的 group。

只在 `localStorage` 里、也不需要读的：未读标记（monitor 有自己的已读标记）。没有存储 group 颜色。
分屏 pane 在同一个 `epitaxyPrefs` 里，不在本提案范围内（§7）。

**排序和分组设置。** section 的 `sortBy` 是 `recency`、`alpha` 或 `created`（app 自己的 sidebar 工具这样
命名），另有 `ascending`。它的分组方式（`date`、`project`、`state`、`custom` 或 `none`）是 section 的 `groupBy`；分组从未改过的
section 没有这个 key，表示 app 的默认值（Sessions 按 project，自定义 section 为 none）。2026-11-06 观察到，
在 app 里做的改动一分钟内就写进了文件（把一个自定义 section 的排序设为 date created：文件里是
`sortBy: created` 和 `groupBy: none`，写入时间比读取早 40 秒）。原型跟随 `sortBy` 和 `ascending`；对
`groupBy` 只读 Sessions 是否按 project 分组（其他值都把 Sessions 画成一个列表），所以在 app 里选的按日期或
按状态分组不会被 "As in the app" 跟随，需要用 monitor 自己的菜单设置。

**映射。** 记录的 `cliSessionId` 就是 monitor 的行 id；全部 78 条记录都对应到一个 transcript。group
归属用的是桌面 id（`local_<uuid>`），由记录映射到行。

**app 实际画出什么，对照它自己的 sidebar 列表核对过**（app 的 `list_sections` 和 `list_sessions`
回答，2026-11-06）：置顶的 session 只画在 Pinned 下，但它保留自己的 group；side session 画在它的
starter 下，并归在 starter 的 group 里；Sessions section 按 project 分组（`group_by: project`），
按最近活动排序；被关掉的 section（`shown: false`）不画；归档的 session 不画。原型对那台机器上 60 个
可见 session 的排列，与这个列表逐个 section 一致，而且每一个都是 monitor 的行。

**评估。** 今天就能从两个普通 JSON 文件读到，monitor 已经读了其中一半；没有受支持的接口，也没有格式
版本。有一件事尚未确定：这个人移动一个 session 之后，app 多快重写 config 文件（读取时它已有 45 分钟没
更新，而 session 记录只有几分钟）。在动手实现之前要先检查：把一个 session 在 group 之间移动，观察这个
文件。如果它很慢，替代办法是从 LevelDB 存储的副本里读同样的键，这个读取器更重、更脆弱。

### 3.2 Codex app

OpenAI 的 Codex 桌面应用；在被读取的机器上是 `/Applications/ChatGPT.app`（bundle id
`com.openai.codex`，版本 26.1103.31037，内带 `codex-cli` 0.167.0-alpha.3.2）。

**这个人整理了什么。** project：每个 project 是一个名字加一个或多个文件夹，顺序由这个人决定。thread：
每个 thread 由这个人归入某个 project，归属不是从文件夹推出来的（有 15 个 thread 在其 project 的文件夹
之外，另有 81 个在某个 project 的文件夹里却没有 project）。自定义侧边栏 section 装着 thread，有顺序，
有的折叠。置顶 thread，有顺序。归档的 thread。改过名的标题。（sub-agent thread 和 `codex exec` 运行在它的
数据库里，但不在它的 sidebar 里：见下面的"app 列出哪些 thread"。）

**存在哪里。** 在三个存储里，而它们之间的迁移只完成了一半：

| 存储 | 存了什么 | 状态 |
|---|---|---|
| `~/.codex/.codex-global-state.json` | app 自己的窗口状态：project 顺序、侧边栏的 section 列表、顺序和折叠集合、每个 thread 的 project、四个置顶列表 | 私有。没有 schema（键带 `-v1`、`-v3` 后缀），2.5 MB，每隔几分钟重写一次 |
| `~/.codex/state_5.sqlite` | app-server 的数据库：`projects`、`project_roots`、`threads`（`project_id`、`thread_section_id`、`archived`、`name`、`is_pinned`）、`thread_sections`、`thread_spawn_edges` | 有版本化的 migration（当天为 62 个）。monitor 已经以只读方式读它来取 thread 标题 |
| `~/.codex/sqlite/codex.db` | 桌面应用自己的目录 | 随每个 app 版本迁移 |

前两个存储里的 project 和 section 一致。**thread 归属不一致**：数据库为 588 个 thread 指定了 project，
有 1 个不一致，655 个为空；状态文件自己的标志也写着 thread 归属尚未迁移。**置顶尚未查清**：状态文件里
有四个置顶列表，分别为 7、13、11 和 10 个 thread，彼此不一致；数据库一个都没标；app 实际显示哪个列表
尚未确定。

**一条受支持的入口。** `codex app-server`（标为 experimental）响应 JSON-RPC 方法 `project/list`、
`threadSection/list` 和 `thread/list`，并发送 `project/changed`、`thread/project/updated` 和
`thread/section/changed` 通知。monitor 的 `claude-monitor-codex` crate 已经用这个协议调用
`project/list` 和 `thread/list`。没有找到置顶相关的方法。

**映射。** thread 的 `rollout_path` 指向它的 rollout；rollout 文件名去掉扩展名
（`rollout-<timestamp>-<id>`）才是 monitor 的行 id，单独的 thread id 不是。sub-agent 的父 thread 写在
`thread_spawn_edges` 里，也写在它的 rollout 的 `session_meta` 里，monitor 会读。数据库没有保存 section
的顺序；原型按 section 的创建顺序来画。

**压缩的 rollout。** Codex 会把 7 天没动过的 rollout 压缩成 `rollout-….jsonl.zst`；app 放进某个 section
或 project 的 612 个未归档 thread 中，有 611 个是这样存的。monitor 从 #s383 起能读这种文件；在那之前，
"Like the Codex app" 把它们画成变暗的行，也就是 app 列出、但 monitor 没有对应行的 session。

**app 列出哪些 thread。** app 自己有一份它所列 thread 的目录（catalogue，`~/.codex/sqlite/codex.db` 里的
`local_thread_catalog`）。在那台机器上测得：81 个 `codex exec` 运行和 sub-agent thread（971 个 reviewer
sub-agent，以及所有派生出来的子 thread）都在 threads 表里，但不在这份目录里，也没有一个被放进 section 或
project。所以原型只画目录里有的 thread。

app 自己的代码说明了它怎样填这份目录（它的目录构建代码，2026-11-07 在 app 26.1103.31037 里读到）：它排除
ephemeral（临时）的 thread、有父 thread 的 thread、`threadSource` 是三个内部值之一的 thread，以及 `source` 为
`exec` 或不是字符串的 thread（`subagent` 对象，或除 `{"custom": "<name>"}` 以外的任何对象）。其他 source 都会
列出：app 自己的 thread（`vscode`）、CLI 的（`cli`）、app-server 或 MCP 方式的运行，以及 `unknown`。

目录不存在或读不了时（评审时读到的一台机器上就没有 `codex.db`），原型只应用这条规则里 threads 表能回答的
部分——`source`——并在列表上方的说明行里说明这份列表是 monitor 的推测。ephemeral 的 thread 和有父 thread 的
thread 单凭这张表分辨不出来：2026-11-07 在第一台机器上测得，推测得到 2,791 个未归档的 thread，目录列出 2,772
个。多出的 19 个都是 `vscode` thread：9 个是 fork 出来的 thread，1 个在表里写明了父 thread；app 为什么不列出另外
9 个，没有查清。没有 `source` 列的数据库（较旧的 app）仍然能读；
这时如果没有目录，每个 thread 都算，note 里会说明。目录忙时，和数据库忙时一样，保留上一次读到的结果。

**sidebar 本身**只存在状态文件里，位于 `electron-persisted-atom-state` 下，2026-11-06 对照 app 自己的
sidebar 核对过：

| Key | sidebar 从中显示什么 |
|---|---|
| `sidebar-custom-sections-v3`（按账号） | `sectionOrder`（`pinned`、`orbit`、`agents`、`custom:<id>`…、`threads`、`chats`）；每个自定义 section 的 `name` 和 `itemKeys`，一项是一个 project（`codex:project:<id>`）或一个 thread（`codex:thread:<host>:<id>`）；`collapsedSectionIds` |
| `sidebar-collapsed-sections-v1` | Pinned、Projects（`threads`）和 Chats 是否折叠 |
| `sidebar-project-expanded-v1-codex:<project id>` | 一个 project 文件夹是否展开 |
| `pinned-thread-ids`（顶层） | Pinned 显示的置顶 |
| `project-order`、`local-projects`（顶层） | Projects 列表的顺序和名字，用状态文件自己的 project id，`app-server-project-id-by-legacy-project-id-by-host` 把它映射到数据库的 id |
| `thread-project-assignments`、`projectless-thread-ids`（顶层） | app 还没把 thread 迁进数据库时它所属的 project，以及 Chats 显示的 thread |

app 的排序设置也在同一个文件里：`flat-project-sidebar-preferences-v1` 包含 `chatSortMode`（文件夹内和
Chats 里的 thread：`manual`、`updated_at` 或 `created_at`）和 `projectSortMode`（`manual`、`priority`、
`created_at` 或 `updated_at`，取自 app 自己的代码）。在那台机器上 `projectSortMode` 是 `updated_at`，但
app 画出的 Projects 列表遵循 `project-order`，所以原型保留 `project-order`，只读取 `chatSortMode`。

一个自定义 section 可以包含 project 文件夹，而不只是 thread：在那台机器上，"Platform" 包含 5 个
project，Projects 列表按 app 的顺序包含其余 9 个。数据库自己的 `thread_sections` 里有一个叫 "Pinned"
的 section，含 11 个 thread，但那不是 Pinned 显示的内容。

**评估。** sidebar 只能从私有的状态文件读到；app 每隔几分钟重写一次这个文件，而且它没有 schema（key
带版本后缀）。原型以只读方式读它，只在它变化时重读，标题、最近活动和归档取自数据库；没有
状态文件时，退回到数据库里的 section，并说明这一点。等 app-server 提供 sidebar 的 section、置顶和文件夹
之后，改为通过 app-server 读取。

### 3.3 Herdr

Herdr 0.9.5，socket 协议 22。Herdr 发布了自己的 schema（`herdr api schema --json`）。

**这个人整理了什么。** server（Herdr 称之为 session），每个 server 有 workspace，每个 workspace 有
tab，每个 tab 有 pane；一个 pane 里可以运行一个 agent。workspace 和 tab 有可由这个人重命名的 `label`
和一个 `number`；workspace 和 tab 可以移动（`insert_index`），这说明 `number` 是显示位置。

**存在哪里，怎么读。** 在运行中的 Herdr server 里，通过它受支持的 CLI：

- `herdr session list --json` 列出各个 server（`name`、`running`、`socket_path`）；
- `herdr --session <name> api snapshot` 一次调用返回一个 JSON 对象，含每个 workspace（`workspace_id`、
  `number`、`label`）、tab（`tab_id`、`workspace_id`、`number`、`label`）和 pane（`pane_id`、`tab_id`、
  `agent_session`、`agent_status`）。

socket API 还提供 `events.subscribe`，可订阅 workspace、tab 和 pane 的变化；CLI 没有暴露它，也没有
试过它的推送。在 index 刷新时轮询 snapshot 就够了。

**映射。** pane 的 `agent_session.value` 是 agent 的 session id，由 agent 的 Herdr hook 在启动时上报；
它就是 monitor 的行 id。没有 agent session 的 pane（一个 shell，或没装 hook 的 agent）不是一行，跳过。hook
只在启动时上报（#s349），所以 session 换了新 id 之后（#s294），pane 仍然指着旧的、已结束的那个。因此每个
entry 还带上它的 pane（`<server>|<pane id>`），页面优先用 monitor 自己关联到这个 pane 的行（行上的
`term: "herdr"`、`target` 和 `herdrSession`），只有 monitor 没有关联任何行时才用 hook 上报的 id。

**monitor 已经在做的。** 它从 agent 进程的环境变量（`HERDR_PANE_ID`、`HERDR_SOCKET_PATH`）找到一个
session 所在的 Herdr pane，并对每个 server 每 2 秒轮询一次 `herdr agent list` 来取 agent 的状态
（`state.rs`、`HerdrReader`）。它从不读 workspace 或 tab。

**注意事项。** pane id 在不同 server 之间会重复（每个 server 上都有 `w1:p1`），所以运行着多个 server
时，排列多出一层：先 server，再 workspace。没有字段说明 label 是这个人输入的还是 Herdr 的默认值
（workspace 默认为文件夹名，tab 默认为编号），所以默认值就按 Herdr 显示的样子显示。在被读取的机器上
只有一个 server 在运行，且没有 agent pane，因此没有观察到实际的 pane 到行的映射；Claude hook（v11）
和 Codex hook（v10）都已安装。

### 3.4 什么映射到列表上

| 在 app 里 | 在 monitor 排列后的列表里 |
|---|---|
| Claude app：Pinned | 第一个 section，按 app 的置顶顺序 |
| Claude app：一个自定义 group | 一个带它名字的 section，按 app 的 section 顺序，折叠状态与 app 相同；这个人关闭的 section 不画 |
| Claude app：Sessions，按 project 分组 | app 的最后一个 section，每个 project 一个小标题，按 app 的方式排序 |
| Claude app：一个 side session | 嵌在启动者下面，位于启动者的 section 里，可以跨 project |
| Claude app：一个归档的 session | 一个折叠的 Archived section，在 "Not in the Claude app" 之前 |
| Claude app：session 的标题（这个人输入的，或 app 自己起的） | 在这个视图里作为该行的名字 |
| Claude app：Routines | 不显示：routine 不是 session 行 |
| Codex app：一个置顶 thread | Pinned，按 app 的置顶顺序，折叠状态与 app 相同 |
| Codex app：一个自定义 section | 一个带它名字的 section，按 app 的顺序，折叠状态与 app 相同；其中的 project 文件夹作为小标题，thread 作为行 |
| Codex app：一个 project | 一个文件夹小标题，放在它的自定义 section 里，或按 app 的 project 顺序放在 Projects 下；除非在 app 里展开过，否则是关闭的；即使为空也画出来 |
| Codex app：一个没有 project 的 thread | Chats |
| Codex app：一个归档的 thread | 不画，因为 app 也不画 |
| Codex app：一个 sub-agent thread，或一个 `codex exec` 运行 | 不画，因为 app 不列出它 |
| Herdr：一个 workspace | 一个带 workspace label 的 section，按 Herdr 的顺序 |
| Herdr：一个 tab | 它里面的一个小标题 |
| Herdr：一个 agent pane | 该 session 的行 |
| Herdr：一个 shell pane | 不显示 |
| monitor 列出的其他所有 session | "Not in …"，按今天的顺序 |

## 4. 模型

**layout**（排列）是 server 为一个来源交给页面的东西：

- `source`：`claude-app`、`codex-app` 或 `herdr`；
- `available`：是否读到了这个来源；没读到时，`reason` 用页面会显示的话说明原因（例如 "The Claude
  app's session list was not found on this Mac"）；
- `sections`：一个有序列表。每个 section 有一个在刷新之间保持稳定的 `id`、一个原样取自 app 的
  `label`、app 是否把它显示为 `collapsed`、有序的 `entries`，以及有序的 `groups`（小标题，带自己的
  `label` 和 `entries`：一个 project、一个 Herdr tab）。一个 entry 指向一个 monitor 行 id，在 app 保存了
  标题时带上 app 显示的 `title`，可以包含子 entry（嵌套）；
- `note`：这个人需要知道的、关于列表有多忠实的说明（例如 app 数据库里还有多少 Codex thread 没有
  project）；
- `pending: true`，不带 section：这个来源的第一次读取还没完成；
- `stale: true`：这份结果是这个来源的读取线程上一次停止之前读到的（手机隔一小时回到页面时，它可能已经是
  一小时前的）；列表照画，但说明行写 "updating…"，直到新的读取完成。

headless worker（monitor 标了 `startedBy` 的行，#s53）只出现在它的发起者的 Agents 面板里，这里从不画它，
即使某个 app 列出了它。

然后页面应用 §2 的三条规则：每个 entry 只画一次，画在第一个指向它的 entry 处，作为那个 session 的行
（monitor 没有这一行时画成变暗的行）；把子 entry 嵌在父 entry 下；把没有任何 entry 指向的行放进最后一组，
受筛选和隐藏约束。server 不知道页面的筛选、隐藏或 family，所以从不替页面决定这些。Herdr 的 hook 可能
上报单独的 session id，而 monitor 用 rollout 名称命名 Codex 行；页面会把 entry 匹配到 id 以它结尾的行。

来自 app 的嵌套可以跨 project。这正是 §1 中协调者那个例子的要点；在这里这样做是安全的，因为这个视图里
每一行都已经写明它的 project，和 flat list 的行一样。在 "By agent and project" 下什么都不变：tree 继续
遵守 #s340 的同一 project 规则。

## 5. 接口

**一个新的 route：`GET api/layout?source=<name>`**，以 JSON 返回一个来源的 layout。只有在选中某个 app
的排列时才请求它，并跟随 index 自己的刷新。`api/sessions` 的行不变。

**请求从不读取 app。** 每个来源有自己的读取线程，由第一次请求启动，只在有页面跟随这个来源时运行：每 3 秒
读一次，最后一次请求之后 30 秒结束。route 返回这个线程最近一次读到的结果；第一次读取完成之前返回
`pending: true`；读取线程上一次停止之前留下的结果带 `stale: true` 返回。两种情况页面都在一秒后再问，用的是
同一个定时器，每次回复都重新设置它，所以重试不会越积越多。读取来源时不持有任何锁，所以慢的 herdr 或忙的数据库不会耽误任何请求；
herdr 按读取线程自己的节奏去问，和 monitor 的 `HerdrReader` 一样（#s238）。Codex 读取器给 SQLite 250 ms，
而不是默认的 5 秒，数据库忙时保留上一次读到的结果。**不带 `source` 的 `GET api/layout`** 在打开筛选面板时
请求，只检查文件——Claude app 的存储和设置文件、Codex app 的数据库、herdr 程序——不读任何 app。只有这项
检查决定一个来源能不能选：读取失败会作为 `lastError` 显示在选项旁边，但从不让它变成不可选，因为选中它
正是重新读取它的方式。从不选择 app 的人，不会触发任何一次读取。

两种回复的确切格式：

| 字段 | 类型 | 含义 |
|---|---|---|
| `sources` | 对象数组 | 每个来源一项，顺序为 `claude-app`、`codex-app`、`herdr`（请求不带 `source` 时） |
| `sources[].source` | `"claude-app"` \| `"codex-app"` \| `"herdr"` | 来源 |
| `sources[].label` | 字符串 | 选项的名字 |
| `sources[].available` | 布尔 | 这个来源的文件是否在读取器查找的位置 |
| `sources[].reason` | 字符串或 null | `available` 为 false 时的原因 |
| `sources[].lastError` | 字符串或 null | `available` 为 true 而上一次读取失败时，读取遇到了什么 |
| `source`、`label` | 同上 | 这份 layout 的来源（请求带 `source` 时） |
| `available` | 布尔 | 上一次读取是否成功 |
| `reason` | 字符串或 null | 没成功的原因 |
| `note` | 字符串或 null | 列表有多忠实 |
| `sections` | 数组 | §4 的 section；`available` 为 false 或带 `pending` 时为空 |
| `pending` | `true`，或没有这个字段 | 还没有任何一次读取完成 |
| `stale` | `true`，或没有这个字段 | 这份结果来自读取线程上一次停止之前 |

不是这三个来源之一的 `source` 返回 404。

由 server 而不是页面来读这些存储：页面可能是通过 tailnet 访问的手机，读不到 Mac 的磁盘；而且 server 已经
为其中一个存储保留了带缓存的读取器（#s340 的 `desktop_sessions.rs`，只在一条记录的修改时间或长度变化时
才重读）。每个读取器：

- 只读，从不对 app 的文件持有写锁，自己也不创建任何文件。读 WAL 模式的 SQLite 数据库时，如果旁边没有
  `-shm` 和 `-wal`，读取者会创建它们，关闭后也不会删除（用 SQLite 3.54 测过）；所以没有 `-wal` 的 WAL 数据库
  （没有写入者打开它，所有已提交的页都在主文件里）以 immutable（不可变）方式打开，不创建任何文件；有写入者
  打开时，通过那个写入者自己的 `-wal` 读取。immutable 读取不持锁，所以读取过程中如果有写入者碰到了这个文件
  （出现了 `-wal`，或文件变了），这次读取按 busy 处理，保留上一次的结果。实际上很少走到 immutable：monitor
  自己的 Codex 标题读取器（在 agents crate 里，不属于这次改动）以普通只读方式打开同一个数据库，所以在运行
  monitor 的机器上，它的 `-wal` 和 `-shm` 通常已经在了；
- 从不把列表的一部分当成整个列表：某一行的值类型不对时，计入 note，其余行照读；扫描中途出现的其他错误会让
  这次读取失败；
- 跳过无法解析的记录，忽略不认识的键，和 #s340 的读取器一样；
- 有一个环境变量可以改它读取的位置，让 browser case 在隔离的存储上运行，和这里其他每个存储一样；
- 遇到不认识的格式时，在选项的原因说明和 monitor 的日志里说出来，这样 app 更新挪动了排列时会被注意到，
  而不是被画错。格式巡检（`agent-replay --unknown`，#264）只读 transcript；它是否也该读这些存储，留给
  实现时决定。

## 6. 来源缺失或变化时

| 情况 | 这个人看到什么 |
|---|---|
| app 没有安装，或它的存储不存在 | 选项显示为不可用，原因作为它的提示和第二行文字。 |
| 存储存在但读不了，或格式不认识 | 仍然可选，因为只有文件缺失才让来源不可选；第二行写 "Last read: …"，说明读取遇到了什么。选中后会重新读取，读不到时按下一行的方式退回。 |
| 选中的来源在之后某次刷新时失败 | 列表退回被 app 替换掉的那种排列（"By agent and project" 或 "One list by activity"），列表顶部用琥珀色写着 "Showing by agent and project: the Claude app's list could not be read"，并给出原因。选择会被保留；下一次能读到来源的刷新会重新按它来画。读取线程上一次停止之前留下的失败结果显示为 "Reading …"，因为这个来源正在重新读取。 |
| Codex app 没有它的 thread 目录，或目录读不了 | 列表照画，按 `source` 排除 `codex exec` 运行和 sub-agent（§3.2），说明行写明这份列表是 monitor 的推测。 |
| app 写到一半的记录 | 这次刷新跳过；下一次再读。 |
| 在手机或另一台桌面上看的人 | 看到的是 monitor 所在机器上的排列，也就是 session 运行的那台机器。 |

## 7. 范围

需要：

- 在 monitor 运行的机器上，以只读方式读取三个 app 的排列；
- session 列表中的 Arrange 选择，在桌面和手机的抽屉里都有；
- §6 的退回行为和 §2 的三条规则。

不需要，也不提议：

- **写入任何 app。** monitor 从不修改 Claude app、Codex app 或 Herdr 的排列或存储。把 monitor 自己的
  排列复制到某个 app 里是另一个功能。
- **窗口布局**：app 并排显示哪些 session、窗口大小、Herdr pane 的几何形状。monitor 一次只显示一个
  session。
- **在 monitor 里编辑排列**（重命名 group、在那里置顶 session）。
- **其他机器。** fleet 视图（`agent-monitor-fleet`）显示每台机器自己的 monitor，每个 monitor 按它自己
  机器上的 app 来排列。

**语言。** Agent Monitor 的界面只有英文，并保持这样：新的文字（"Arrange"、"Like the Claude app"、
"Not in the Claude app"、退回提示）都是英文，不新增本地化。group、section、workspace 和 tab 的名字按
这个人在 app 里写的样子显示。

## 8. 备选方案

- **复制一次，之后归 monitor 所有。** 点击时把 app 的排列复制成一份 monitor 自己的排列，之后可以编辑。
  这需要 monitor 目前没有的 group 和置顶模型；而且这个人下一次在 app 里重新整理时，副本就过时了，他还是
  得再点一次。跟随（follow）是同样的一次点击，却没有这两项代价。
- **双向同步。** 在 monitor 里重新排列，也会移动 app 里的 session。Claude app 不提供从外部写入其排列的
  方式，而写它的文件会和正在运行的 app 竞争，因为 app 自己也会重写这些文件。Codex app-server
  （`thread/section/move`、`project/update`）和 Herdr（移动和重命名）确实接受写入，但用它们会让
  monitor 成为这个人排列的第二个编辑者，带来它自己的冲突。这是另一个功能，本文不提议。
- **只加标签。** 保留今天的 tree，给每行加一个显示 app 中 group 名的标签。更小，但协调者那个例子仍然
  分散在各个 project 里。
- **只放宽 #s340。** 让 `nestSideSessions` 在 tree 里跨 project 嵌套。这是解决协调者例子的最小改动，
  但也只解决这一个：group、置顶、Codex 和 Herdr 仍然看不到。即使不采纳本提案，它也值得做，而且它是本
  提案的一个子集。
- **侧边栏顶部加一个图标**，而不是放在 filter sheet 里的一行。更快够到，但顶部那一行控件是按侧边栏
  232px 的最小宽度来衡量的（#210、#223），已经满了；而 sheet 里本来就放着 flat list 的选择，这个提案正
  替换它。

## 9. 代价与风险

- **没有文档的格式。** Claude app 的文件和 Codex app 的状态文件是私有的，没有格式版本；Codex app 的
  数据库有版本化的 migration，但属于 app-server 自己；只有 Herdr 发布了 schema 和带版本的协议。每一个
  都可能随 app 更新而变化，不另行通知。退回行为让列表仍然可用，对不认识格式的报告让变化可见，而一次格式
  变化的代价是更新一个读取器，而不是重新设计。
- **读取开销。** Claude app 的记录已经由 #s340 读取并缓存；config 文件每次刷新多一次 `stat`，只在它
  变化时才重读。Herdr 每次刷新、每个运行中的 server 多一次 `api snapshot`，与 monitor 已经每 2 秒运行
  一次的 `agent list` 并列。Codex app 只在其存储变化时才多读一次它的数据库或 app-server。
- **要维护三个读取器**，每个 app 一个，各有一个隔离的 fixture。
- **远程查看的人会看到 group 名。** group 和 workspace 的名字会像 session 名一样传到配对的手机上；
  mask policy（#365）对它们和对任何 JSON 响应一样适用。

## 10. 评审者如何判断它有效

**在原型（#s387）上运行过的，一台 Mac，2026-11-06：**

- Rust 单元测试（`claude-monitor/src/layout.rs`），每个读取器都用虚构的存储：Claude app 从磁盘上的
  设置和记录读出 section、group、置顶、side session 和归档；Codex app 从按实测结构建的状态文件读出
  sidebar（置顶、包含 project 文件夹或 thread 的自定义 section、Projects 列表、Chats、折叠的 section 和关闭
  的文件夹，thread 的 project 先看状态文件再看数据库），从按实测结构建的数据库读出 thread，结构不同的状态
  文件或数据库会被拒绝；Herdr 从一个和两个 server 的 snapshot
  读出 workspace、tab 和 agent pane。还有一个 `api/layout` 的 route 测试。
- 一个关于页面放置逻辑的 node contract case：每个 entry 一次，隐藏的行和 index 里没有的 entry 都保留，
  重复的丢掉，空 section 不画，单独的 session id 能匹配到用 rollout 命名的行。
- 在这台机器的 app 上本地启动一个 monitor，用内置浏览器："Like the Codex app" 按 app 显示的样子画出了它的
  sidebar（Pinned 折叠，"Platform" 带 5 个文件夹，第二个自定义 section 折叠，Projects 按顺序带其余 9 个
  文件夹，全部关闭，然后是 Chats）。合入 #s383 对压缩 rollout 的读取之后，它在顶层放置的 1,276 个
  thread 全部是 monitor 的行（合入之前是 85 个）。
- 在同一个 monitor 里试 section 菜单：把 Ongoing 按名字排序、按日期分组、按状态分组，再 Reset 回 app
  的顺序；刷新页面后选择仍在，Reset 会清除它；选中的选项有勾选标记。在 Claude app 里把 Ongoing 设为
  date created 后，"As in the Claude app" 按记录里的创建时间顺序列出它。
- 在 375 × 812（竖握的手机）下：菜单以 sheet 的形式从底部弹出，距两侧各 8 px，每行 44 px，44 px 高的
  section 头里是 40 px 的控件；点击遮罩会关闭菜单，抽屉和打开的 session 保持不变。
- 在同一个 monitor 里：每个选项都画出对应 app 的列表；Claude app 的
  与它自己的 sidebar 列表一致（§3.1）；读不到的来源显示为不可用并给出原因；读取失败时带琥珀色说明行
  退回，之后恢复；说明行的 ✕ 回到 tree；刷新页面后选择仍在；手机抽屉在 375 px 下显示说明行和列表，
  不会横向滚动。

**第一轮评审后补上的**（browser case，`claude-replay-browser-tests/tests/layout_sync.rs`，在隔离存储上运行：
每个 case 自己写 Claude app 的记录和设置文件，harness 给每个 monitor 一个 herdr 替身，所以没有 case 读这台
机器的 app 或运行它的 herdr）：

- "Like the Claude app" 画出 fixture 的 group 和 Sessions，用 app 的标题，side session 嵌在下面；一个 idle
  和一个被隐藏的 session 仍画在它们的 group 里；其余的放在折叠的 "Not in the Claude app" 下；刷新页面后选择
  仍在，它背后的 "One list by activity" 也在，说明行的 ✕ 会回到它。
- 文件缺失的来源显示为不可用并给出原因；选中后它的设置文件被删除，列表带琥珀色说明行退回，文件回来后恢复。
- "Like Herdr" 通过替身画出一个 workspace 和它的 tab，以及 agent pane 对应的行。
- 在 390 × 844 下，section 菜单从 44 px 的控件以 sheet 弹出，每行 44 px；点击遮罩关闭菜单，不选中任何东西；
  说明行的 ✕ 是 44 px；Arrange 的两行选项能容下原因说明。在 844 × 390 下菜单是控件旁边的弹层。

**第二轮评审后补上的：**

- 单元测试：读取失败成为来源的 `lastError`，来源仍然可选；只有文件缺失才让它不可选。读取线程停止之前留下
  的结果带 `stale` 返回，直到新线程的第一次读取完成。读取线程的用例让每次读取等一个闸门放行，而不是给请求
  计时，所以负载高的机器上不会不稳（#s300）。Codex 读取器读 WAL 数据库时不在旁边创建 `-shm` 或 `-wal`（关掉
  immutable 打开时，用例失败并列出这两个文件）；没有目录时，按 `source` 排除 `exec` 和 sub-agent thread，并
  说明这一点。
- Claude app 的 browser case 继续往下：设置文件里没有 sidebar 排列时，列表退回；用说明行的 ✕ 离开后，选项
  仍然可选，并显示 "Last read: …"；再次选中，就重新读取。换回旧规则（读取失败就让来源不可选）时，用例在这一步
  失败。
- 一个 Codex app 的 browser case（只在 macOS 上，数据库用系统自带的 `sqlite3` 建）：app 的文件夹和 Chats，用
  app 的标题；数据库里的一个 `codex exec` 运行放在 "Not in the Codex app" 下——没有目录时按 `source` 排除（说明
  行写明），目录出现后由目录单独决定；两个数据库旁边都不会多出 `-shm` 或 `-wal`。关掉按 `source` 排除的规则时
  用例失败。
- 在 844 × 390 下，section 菜单的 scrim 不再让屏幕变暗，但仍会接住关闭菜单的那次点击。

**第三轮评审后补上的：**

- 单元测试：写入者碰到过的 immutable 读取（出现了 `-wal`）按 busy 处理，普通读取从不这样处理；只有某一行的
  值类型不对才计数，`SQLITE_CORRUPT` 这类错误不算；没有 `source` 列的数据库仍然能读，并说明这一点；判断
  headless 的规则与 app 一致（MCP 或 app-server 方式的运行、`unknown` 和 custom source 会列出；`exec` 和其他
  对象不会）。
- Codex 的 browser case 用 `-init /dev/null` 运行 `sqlite3`，不会读到 `~/.sqliterc`。

没有运行的：iOS 模拟器（竖屏、横屏、浅色、深色）——这台机器上没有模拟器工具；Linux。Herdr 的 browser case
里没有 monitor 关联到 pane 的行，所以"先按 pane 匹配"只由 node contract 覆盖。Codex 读取器的单元测试和它的
browser case 只在 macOS 上编译，和 agents crate 的 Codex 读取器一样（Linux 构建不编译 C 依赖），所以 Linux 上
的 CI 不运行它们。

**发布前拟议的检查：**

- iOS 模拟器上走一遍：竖屏、横屏、浅色、深色。
- 一个协调者的 side session 在别的 project 里：在 "Like the Claude app" 下嵌在它下面，而 tree 保持不变。
- 一个 node contract case：给定行和一个 layout，每一行恰好出现一次。

## 11. 待定问题

1. **跟随，还是复制一次？** 建议：跟随（§8）。如果跟随的读取开销过大，复制一次是备选。
2. **先做哪些来源？** 建议：先做 Claude app（读取器已经有了，协调者例子也最常见），然后 Herdr（有发布的
   schema 和受支持的 CLI，monitor 也已经在轮询每个 server），最后是 Codex app：它的 sidebar 只能从一个私有的
   状态文件读到，app 在任何一次发布中都可能改动它（§3.2）。原型三个都提供；Codex 的读取器最可能在 app
   发布后需要更新，当文件结构不是它认识的样子时，会在原因里说明。
3. **新的 route，还是给 `api/sessions` 的行加字段？** 建议：route（§5）。这是一个 fleet 视图和
   attention report 目前都不读的公共接口，所以对它们没有影响；但和 #s342 一样，仍需维护者决定。
4. **按浏览器记住，还是所有浏览器共用一个选择？** 建议：按浏览器，和 flat list 一样。共用的选择需要一个
   server 端偏好设置，而自 shell 偏好（`ui.json`、`api/ui`）随 classic 视图退役（#s98）以来，monitor
   已经没有这样的设置。
5. **tree 是否也应该跨 project 嵌套**（即"只放宽 #s340"这个备选），与本提案无关地单独做？
6. **Claude app 的 config 文件够新吗？** 在实现之前测量（§3.1）：把一个 session 在 group 之间移动，
   记录这个文件被重写的时间。如果滞后几分钟，就要在接受这个滞后（顶部那行可以写明排列是何时读取的）和
   一个 LevelDB 读取器之间做选择。

## 12. Demo

`design/layout-sync-demo.html` 是一个基于虚构 session 的可点击 mockup：左边是 monitor 的侧边栏，旁边是
所选 app 自己的列表以便对照，包含 Arrange 选择、每个来源、"Not in …" 组、筛选作用于排列后的列表、来源
缺失的情况，以及手机宽度。它做于 §2 修订之前，仍然让筛选作用于整个排列后的列表；原型（#s387）按修订后的
规则实现。
