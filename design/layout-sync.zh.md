# 让 session 列表按 Claude app、Codex app 或 Herdr 的方式排列

> 本文是 `design/layout-sync.md` 的中文副本；以英文版为准，两份描述同一个设计。
>
> **DRAFT（草案）**（#s382，2026-11-06）。这是一个提案：文中内容都还没有实现。§3 中关于各个 app 的事实，
> 是 2026-11-06 在一台 macOS 机器上读取的，版本号见各节。demo 是 `design/layout-sync-demo.html`
> （只含虚构的 session）。

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

无论 app 里有什么，三条规则保证列表如实：

1. **每个 session 恰好出现一次。** app 里没有的 session（在 "Like the Claude app" 下的 Codex
   session、在普通终端里运行的 Claude Code、Herdr 已经关掉的 pane）放进最后一组 **Not in the Claude
   app**（或 Codex app、或 Herdr），按今天的顺序画。
2. **筛选和隐藏照样生效。** session filter（Active recently、Blocked、Idle，#202）和已隐藏的集合，
   对排列后的列表的收窄效果，与对 tree 完全一样。被筛空的 group 不画。
3. **family 保持完整。** fork（#142）和它的代表 session 待在一起，不管 app 把其中哪一个放在哪里。

在显示某个 app 的排列时，列表顶部有一行说明（"Arranged like the Claude app · follows it"），带一个
回到 "By agent and project" 的控件。这个选择按浏览器记住，和今天的 flat list 一样（`localStorage`），
所以手机和桌面可以用不同的排列。

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

**映射。** 记录的 `cliSessionId` 就是 monitor 的行 id；全部 78 条记录都对应到一个 transcript。group
归属用的是桌面 id（`local_<uuid>`），由记录映射到行。

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
有的折叠。置顶 thread，有顺序。归档的 thread。改过名的标题。sub-agent thread 嵌在派生它的 thread 下。

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

**映射。** thread id 就是 rollout 文件名（`rollout-<timestamp>-<id>.jsonl`）里的 Codex session id，
也就是 monitor 的行 id。sub-agent 的父 thread 已经写在它的 rollout 的 `session_meta` 里，monitor 会读。

**评估。** 今天只能从私有的状态文件读到，而 app 不断重写它，格式也正在迁移中。等 app-server 的存储
写入每个 thread 的 project 之后，再通过 app-server（或以只读方式读它的数据库）来读；在那之前，大约一半
的 thread 会被放错位置。在弄清 app 显示哪个置顶列表之前，不处理置顶。

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
它就是 monitor 的行 id。没有 agent session 的 pane（一个 shell，或没装 hook 的 agent）不是一行，跳过。

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
| Claude app：这个人输入的标题 | 在这个视图里作为该行的名字 |
| Claude app：Routines | 不显示：routine 不是 session 行 |
| Codex app：一个置顶 thread | 第一个 section，Pinned，等置顶列表查清之后 |
| Codex app：一个自定义 section | 一个带它名字的 section，按 app 的顺序，折叠状态与 app 相同 |
| Codex app：一个 project | Threads 下的一个小标题，按 app 的 project 顺序 |
| Codex app：一个归档的 thread | 最后一个折叠的 Archived section，在 "Not in the Codex app" 之前 |
| Codex app：一个 sub-agent thread | 嵌在它的父 thread 下面 |
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
  `label`、app 是否把它显示为 `collapsed`，以及有序的 `entries`。一个 entry 指向一个 monitor 行 id，
  可以包含子 entry（嵌套）。

然后页面应用 §2 的三条规则：把每个 family 放在第一个指向它某个成员的 entry 处，把子 entry 嵌在父 entry
下，丢掉指向此处没有的行的 entry，并把没有任何 entry 指向的行放进最后一组。server 不知道页面的筛选、
隐藏或 family，所以从不替页面决定这些；由页面用它画 tree 时已有的代码来决定。

来自 app 的嵌套可以跨 project。这正是 §1 中协调者那个例子的要点；在这里这样做是安全的，因为这个视图里
每一行都已经写明它的 project，和 flat list 的行一样。在 "By agent and project" 下什么都不变：tree 继续
遵守 #s340 的同一 project 规则。

## 5. 接口

**一个新的 route：`GET api/layout?source=<name>`**，以 JSON 返回一个来源的 layout。只有在选中某个 app
的排列时才请求它，并跟随 index 自己的刷新，所以对从不选择的人，monitor 不读任何 app 的存储。
`api/sessions` 的行不变。

由 server 而不是页面来读这些存储：页面可能是通过 tailnet 访问的手机，读不到 Mac 的磁盘；而且 server 已经
为其中一个存储保留了带缓存的读取器（#s340 的 `desktop_sessions.rs`，只在一条记录的修改时间或长度变化时
才重读）。每个读取器：

- 只读，从不对 app 的文件持有写锁；
- 跳过无法解析的记录，忽略不认识的键，和 #s340 的读取器一样；
- 有一个环境变量可以改它读取的位置，让 browser case 在隔离的存储上运行，和这里其他每个存储一样；
- 遇到不认识的格式时，在选项的原因说明和 monitor 的日志里说出来，这样 app 更新挪动了排列时会被注意到，
  而不是被画错。格式巡检（`agent-replay --unknown`，#264）只读 transcript；它是否也该读这些存储，留给
  实现时决定。

## 6. 来源缺失或变化时

| 情况 | 这个人看到什么 |
|---|---|
| app 没有安装，或它的存储不存在 | 选项显示为不可用，原因作为它的提示和第二行文字。 |
| 存储存在但读不了，或格式不认识 | 同上，并在能读到时附上 app 版本。 |
| 选中的来源在之后某次刷新时失败 | 列表退回 "By agent and project"，列表顶部写着 "Showing by project: the Claude app's list could not be read"。选择会被保留；下一次能读到来源的刷新会重新按它来画。 |
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

拟议的检查（尚未运行；这是草案）：

- 每个来源一个隔离的 fixture 存储和一个 browser case：选中该选项后，按 fixture 的顺序画出它的 group，
  置顶在前，保留嵌套，其余每一行在 "Not in …" 下各出现一次。
- 同一个 case 收窄 session filter：被筛空的 group 不画；被筛掉的置顶 session 不画。
- 存储缺失时选项不可用并给出原因；选中时存储被移除，列表带提示退回，存储回来时排列也回来。
- 一个协调者的 side session 在别的 project 里：在 "Like the Claude app" 下嵌在它下面，而 tree 保持不变。
- 手机抽屉在 390×844 下显示排列后的列表和 Arrange 选择。
- 一个 node contract case：给定行和一个 layout，每一行恰好出现一次。

## 11. 待定问题

1. **跟随，还是复制一次？** 建议：跟随（§8）。如果跟随的读取开销过大，复制一次是备选。
2. **先做哪些来源？** 建议：先做 Claude app（读取器已经有了，协调者例子也最常见），然后 Herdr（有发布的
   schema 和受支持的 CLI，monitor 也已经在轮询每个 server），最后在 Codex app 的 app-server 存储写入
   每个 thread 的 project 之后再做它（§3.2）。在那之前，"Like the Codex app" 显示为不可用并说明原因。
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
缺失的情况，以及手机宽度。
