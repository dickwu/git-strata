# gui 模块（设计提案）

**Crate**：`crates/strata-client`（新，从 cli 抽出）＋ `apps/desktop/src-tauri`（新，crate `git-strata-desktop`，lib `strata_desktop_lib`）· **前端**：`apps/desktop/`（Next.js 静态导出）· **设计来源**：daemon.md §3–§5、confirm.md §1 §5、cli.md §1 §3、README「Install and daemon」「Confirmation gate」 · **参照工程**：R2（github.com/dickwu/r2，已确认的技术栈）· **UI 方法**：frontend-design skill

> 状态：提案，未实现。技术栈按 R2 已定，不再讨论；§9 列出待 peilin 决定的问题。本文只写设计，不含代码。

## 1. 职责与边界

gui 是给**人**用的桌面前端：看 daemon 健康与告警、看各会话在各仓库的状态和改动、在需要时批准（发布 / 推送 / 依赖提议 / 丢弃会话 / 放宽配置），以及首次使用的引导。目标用户是一个开发者，同时看着几个 agent 在 git-strata 会话里干活；他打开窗口要回答的问题只有三个：**谁改了什么、什么在等我、磁盘还够不够**。

- **做**：
  - Tauri v2 外壳；全部业务在 Rust 侧，经 `strata-client` 调 daemon 的 RPC（daemon.md §5），JS 只做展示与交互状态。
  - 轮询 daemon 并把变化以 Tauri 事件推给前端（§5.3）。
  - 在调用确认类方法**之前**，把 daemon 将要执行的计划原样展示出来（与 cli 打到 stderr 的内容同源），并把 `shown` / `planned` 原样带回 daemon。
  - 首次使用引导：config.toml、LaunchAgent、TCC 权限、Claude Code 钩子（§5.6）。
  - 系统通知（daemon 掉线、待批提议、`Unknown` 结果、告警）。
- **不做**：
  - 不弹 LocalAuthentication、不签发也不缓存任何"已确认"状态（confirm.md：认证只在 daemon 的 `Confirmer::require`，不变量 7 / I7）。
  - 不直接读写 store、`<root>/sessions/` 或任何仓库工作树；**所有数据都经 daemon socket**（理由见 §6 TCC）。daemon 没在跑时不走 cli 那条本地只读路径，只显示"daemon 未运行"和启动按钮。
  - 不提供 cli 没有的写操作，不提供"跳过确认""全部批准""自动批准被取代的提议"。
  - 不在 JS 里拼 RPC、不在 JS 里判断能否推送；这些判断由 daemon 返回，Rust 侧转成类型化结构。
- **与 cli 的关系**：同一组 RPC 的第二个客户端。cli 的 `require_tty` 只是 cli 自己的前置过滤，不是安全边界（confirm.md §1 威胁模型：agent 同 uid，可以直接连 socket）。GUI 不是 TTY，也不需要是：真正的门在 daemon。

## 2. 依赖

### 2.1 目录与 workspace 布局

```
git-strata/
├─ Cargo.toml                 # members 增加 crates/strata-client、apps/desktop/src-tauri
│                             # default-members 不含 desktop：cargo test 仍然只跑核心，不拉 tauri
├─ crates/
│  ├─ strata-client/          # 新：socket 路径、call、错误映射、类型化参数与回答、计划读取
│  ├─ git-strata/             # 改为依赖 strata-client，删掉自带的 rpc.rs 重复实现
│  └─ strata-daemon/          # client.rs 改为 re-export strata-client（或删除，见 §9）
└─ apps/desktop/              # 照 R2 的仓库根布局，只是挪到 apps/desktop 下
   ├─ package.json            # bun；scripts 与 R2 同名：dev / build / tauri:dev / check / test
   ├─ next.config.ts          # output:'export', distDir:'dist'，dev 用 Turbopack
   ├─ postcss.config.mjs      # @tailwindcss/postcss
   ├─ public/fonts/           # 自托管 woff2 + OFL（R2 规则：不用 next/font/google）
   ├─ src/app/                # layout.tsx / providers.tsx / page.tsx / globals.css / fonts.css
   ├─ src/components/         # Titlebar, Sidebar, Toolbar, StatusBar, Inspector, CommandPalette …
   ├─ src/lib/                # tauri 调用的薄封装（invoke + listen）、手写 TS 类型、格式化
   ├─ src/stores/             # zustand（themeStore 照 R2，uiStore：选中会话/仓库）
   ├─ src/hooks/              # useGlobalShortcuts、useStrataEvents
   └─ src-tauri/
      ├─ Cargo.toml           # crate git-strata-desktop；lib strata_desktop_lib（staticlib/cdylib/rlib）
      ├─ tauri.conf.json
      ├─ capabilities/default.json
      ├─ Info.plist           # 合并进 bundle（R2 同款做法）
      └─ src/{main.rs, lib.rs, commands/, poller.rs, events.rs, onboarding/}
```

放进同一 workspace 的理由：GUI 直接复用 `strata-types` 的 `Code`、`RepoName`、`SessionId` 与 config 校验（config.md V1–V3），改一处两边编译期同步；`default-members` 排除 desktop，核心的 `cargo test`、CI 时间不受 tauri 影响。另一种是放在 workspace 之外（`exclude`），用 path 依赖；见 §9 Q1。

### 2.2 技术栈（照 R2，已定）

| 层 | 选择 | 理由 / 与 R2 的关系 |
|---|---|---|
| 外壳 | Tauri 2.12 | R2 同款；Rust 侧直接链接本仓库 crate，满足"后端全在 Rust" |
| 前端 | Next.js 16 静态导出、React 19、TypeScript | R2 同款；没有 Node 服务端，产物是 `dist/` |
| 包管理与测试 | Bun（`bun test`） | R2 规则：只用 Bun。不要求 pnpm 或 `cargo tauri`，CLI 用 devDependency `@tauri-apps/cli` |
| 组件 | AntD 6 + @ant-design/icons 6 | R2 同款；`App.useApp()` 取 message/notification/modal，禁用静态方法（R2 规则） |
| 样式 | Tailwind 4（@tailwindcss/postcss）+ R2 的 CSS 变量 | 共用 R2 的 token 与壳；Prettier + tailwind 插件 |
| 状态 | Zustand（UI 状态）、TanStack Query 5（daemon 数据） | R2 同款；Query 的数据由 Rust 事件 `setQueryData` 喂入，不靠前端定时器 |
| 长列表 | react-virtuoso | 会话 × 仓库、文件列表、检查点列表 |
| 代码 / diff | Monaco（DiffEditor，只读） | R2 同款依赖 |
| 动效 | framer-motion | 只用在 §8 那一处 |
| Rust↔前端 | 普通 `#[tauri::command]` + `generate_handler!`，命令在 `src-tauri/src/commands/`；流式与进度用事件 | R2 同款；**不用 tauri-specta**，类型两边手写 |
| 插件 | opener、os、notification、updater、process、dialog | R2 的子集加 notification；**不要 fs、http 插件**（GUI 不读文件、不发网络请求，见 §6） |

与 R2 的唯一约定差异：R2 命令返回 `Result<T, String>`；这里返回 `Result<T, GuiError>`，`GuiError = { code, message, detail }` 与 cli 信封的 `error` 同形。因为界面要按 `error.code` 分支（types.md 的闭集 `Code`，cli.md：不看 message），字符串错误做不到。

### 2.3 crate 依赖

- `strata-client` → `strata-types`、`serde`、`serde_json`；可选 `strata-gitio`（计划读取要看 chain 顶，见 §3.1）。不依赖 `strata-daemon`：参数结构从 `strata-daemon/src/rpc.rs` 挪进 strata-client（或 strata-types），daemon 反过来依赖它们，保证两边 `deny_unknown_fields` 的结构是同一份。
- `git-strata-desktop` → `strata-client`、`strata-types`、`tauri`、tauri 插件、`tokio`（轮询与 spawn_blocking）。**不依赖** `strata-store`、`strata-session`、`strata-overlay`、`strata-confirm`。
- 调用方：cli 与 desktop 都调 strata-client。

## 3. 公开接口（Rust）

### 3.1 strata-client（从 cli 抽出）

现状：有两份几乎一样的客户端——`strata_daemon::client::call(socket, method, params) -> Result<Value>`（socket 不在给 `Busy daemon_down`，回答丢失给 `Unknown reply_lost`，无读超时）和 `git_strata::rpc::call(socket, method, params, Kind) -> Result<Value, CallError{Down, Failed}>`（字符串 id、校验 id、错误映射）。抽成一份：

```rust
pub struct Client { socket: PathBuf }
impl Client {
    pub fn default_socket() -> PathBuf;            // ~/Library/Application Support/git-strata/daemon.sock
    pub fn new(socket: PathBuf) -> Self;
    /// 一次连接一个请求（daemon.md 线协议）；Kind::Read 可以设超时，Kind::Write 不设
    /// （确认框最长 PROMPT_TIMEOUT 60 s + 宽限，发布还要跑检查）。
    pub fn call(&self, method: &str, params: Value, kind: Kind) -> Result<Value, CallError>;
    // 类型化包装，GUI 只用这些：
    pub fn daemon_status(&self) -> Result<DaemonStatus, CallError>;
    pub fn status_all(&self) -> Result<Vec<SessionStatus>, CallError>;
    pub fn du(&self, refresh: bool) -> Result<DuReport, CallError>;
    pub fn deps_list(&self, repo: Option<&RepoName>) -> Result<Vec<Proposal>, CallError>;
    pub fn publish_plan(&self, group: &[(SessionId, RepoName)]) -> Result<PublishPlan, CallError>;
    pub fn diff(&self, p: DiffParams) -> Result<DiffReport, CallError>;
    pub fn push(&self, repo: &RepoName, shown: Option<CommitOid>) -> Result<PushResult, CallError>;
    pub fn publish(&self, p: PublishParams) -> Result<PublishResult, CallError>;
    pub fn deps_approve(&self, id: ProposalId, sessions: &[SessionId]) -> Result<Value, CallError>;
    pub fn rm(&self, s: &SessionId, discard: bool) -> Result<Value, CallError>;
    pub fn config_relax(&self, repo: &RepoName) -> Result<Value, CallError>;
    pub fn gc(&self) -> Result<GcReport, CallError>;
}
/// 计划读取：cli 的 show::push_plan、confirm.rs 的 planned_moves 挪到这里，
/// cli 打到 stderr、GUI 画成对话框，用的是同一个函数的同一个结果。
pub mod plan {
    pub fn push_plan(config: &Config, repo: &RepoName) -> Option<PushPlan>;   // 含要推的 chain 顶 = shown
}
pub enum Kind { Read, Write }
pub enum CallError { Down, Failed(StrataError) }
```

回答目前是 `serde_json::Value`；strata-client 只为 GUI 用到的子集定义 `Deserialize` 结构，**未知字段忽略**（daemon 加字段不打断 GUI），缺字段报错（契约测试兜住，§7）。cli 继续用 `Value` 也可以。

### 3.2 Tauri 命令（`src-tauri/src/commands/`）

一个 RPC 方法对应一个命令，名字不改，参数只收标识符，不收"已确认"之类的布尔：

| 文件 | 命令 | RPC | 类别 |
|---|---|---|---|
| health.rs | `daemon_status`、`daemon_start`、`daemon_stop` | daemon_status / launchctl | 只读 / 本机 |
| sessions.rs | `sessions`、`session_files`、`checkpoints` | status --all、files | 只读 |
| diff.rs | `diff_paths`、`diff_text`* | diff、（新）diff_text | 只读 |
| storage.rs | `du`、`gc` | du、gc | 只读 / 写（模态确认，见 §5.5） |
| deps.rs | `deps_list`、`deps_propose`、`deps_approve` | 同名 | 写，Touch ID |
| publish.rs | `publish_plan`、`publish`、`push`、`chain_resolve`、`resolve_move` | 同名 | 写，按 pins 需 Touch ID |
| sessions.rs | `rm` | rm（`discard` 时 Touch ID） | 写 |
| config.rs | `config_get`、`config_relax`、`config_check` | 读文件 + config_relax | 写，Touch ID |
| onboarding/ | `onboarding_state`、`write_config`、`install_agent`、`open_privacy_pane` | 本机 | 见 §5.6 |

\* `diff_text` 需要 daemon 新增一个只读方法，见 §9 Q4。

全部写命令是 `async`，内部 `spawn_blocking(client.call(..))`；开始时发 `strata://op` 事件 `{op_id, method, phase:"started"}`，结束发 `"done"`/`"failed"`。daemon 不流式回报进度，所以界面上只有"等待确认（看 Touch ID 框）"与"执行中"两种中间态，按方法静态区分。

### 3.3 事件

| 事件 | 负载 | 来源 |
|---|---|---|
| `strata://health` | `DaemonStatus` + `reachable` | poller，变化才发 |
| `strata://sessions` | `Vec<SessionStatus>` | poller，变化才发 |
| `strata://du` | `DuReport` | poller / 手动刷新 |
| `strata://op` | `{op_id, method, phase, result?}` | 写命令 |

前端照 R2 `folderStream.ts` 的规矩：先 `listen` 再 `invoke`，组件卸载时取消。

## 4. 数据与不变量

GUI 不拥有持久业务数据。它只写三样东西，都在引导里、由人按按钮写：`config.toml`（仅首次、文件不存在时）、LaunchAgent plist、前端的偏好（zustand persist，存在 WebView 的 localStorage：主题、强调色、侧栏样式，照 R2 themeStore）。

不变量：

- **G1 无绕过**：GUI 进程里不存在能让 daemon 认为"已确认"的东西。没有 token 字段、没有缓存的 LAContext、没有"本次会话已批准"。（落实 confirm I7 与 design §9 不变量 7。）
- **G2 所见即所推**：确认类调用携带的 `shown` / `planned` 只能来自刚展示给人的那份计划；计划在对话框打开后若被 poller 发现变化，对话框作废并重新取。daemon 在锁内再核一次（daemon D21），这是第二道。
- **G3 人手触发**：确认类命令只能由主窗口里的一次点击发起；不绑全局快捷键、不能从命令面板直接执行（面板只能打开对话框）、不能被 URL scheme / 深链 / 命令行参数 / AppleScript 触发。
- **G4 无自动化面**：发布构建里没有 `tauri-plugin-connector`（R2 在 dev 用来让 AI 驱动界面），没有 MCP、没有 WebSocket 端口。
- **G5 不读数据卷**：GUI 进程不打开 `paths.root` 之下和任何仓库 `path` 之下的文件（§6 TCC）。
- **G6 不信任仓库内容**：diff、路径、提交信息、ticket 都是 agent 写的，按纯文本渲染（Monaco 只读、AntD 文本），不渲染 HTML / Markdown，开严格 CSP。

## 5. 流程与算法

### 5.1 界面（MVP）与线框

壳完全照 R2：`.app-shell` 网格 = 36 px 标题栏（可拖拽，⌘K 入口）+ `.app-body`（264 px 侧栏 / 56 px 收起 + 主区）；主区 = 52 px 工具栏 + 内容 + 30 px 状态栏；右侧可选 Inspector。

```
┌ titlebar ─────────────────────────────────────────── ⌘K Search sessions ─┐
├ sidebar ──────────┬ toolbar: s-k3q7xm2a  "fix login"   [Diff] [Publish…] ┤
│ Waiting for you 2 │                                                       │
│ Sessions        5 │  strata column        repos in this session          │
│  ● fix login      │  ┌──────────┐         tool       3 changed  ahead 1  │
│  ● deps bump      │  │ origin   │         staff-api  clean      —        │
│  ○ spike (idle)   │  ├──────────┤                                        │
│ Storage   41 GB   │  │ chain  2 │         checkpoints                     │
│ Daemon    healthy │  ├──────────┤         #14  12:03  after edit app.ts   │
│                   │  │ session  │         #13  11:58  before restore      │
│ Settings          │  │ deps     │                                        │
│                   │  │ base     │                                        │
├───────────────────┴──┴──────────┴────────────────────────────────────────┤
│ statusbar: daemon pid 4121 · last poll 2 s ago · du as of 11:40          │
└──────────────────────────────────────────────────────────────────────────┘
```

| 屏 | 内容 | 数据 |
|---|---|---|
| **Waiting for you**（默认首页） | 所有需要人的事：待批依赖提议、需确认的推送、`ConfigRelaxPending`、`Quarantine`、`UnknownPublish`、`Conflict` 待选。每项一个按钮，进入对应对话框。空态："Nothing needs you right now. Agents can keep working." | daemon_status、status、deps_list |
| **Daemon health** | 运行与否、`store_unavailable`（附 TCC 引导链接）、告警列表（DaemonWarning 的各 kind，按 kind 写人话与下一步）、启动 / 停止 | daemon_status |
| **Sessions** 列表 / 详情 | 列表：ticket、id、仓库数、改动数、最后活动；详情：左侧 strata column（§8），右侧每仓库状态、文件、检查点；可 restore 到检查点（daemon 已有 checkpoint 再 restore） | status --all、files |
| **Diff** | 左文件树（改动类型），右 Monaco DiffEditor 只读；支持 session vs base 与两个 session 之间 | diff、diff_text |
| **Publish / push** | 见 §5.4 | publish_plan、push、publish |
| **Dependencies** | 提议列表、影响的会话、planned moves，Approve 走 §5.4 | deps_list、deps_approve |
| **Storage** | 每组 logical / allocated / private、可回收的 trash（`reclaim_min`–`reclaim_max`）、savings、扫描状态与 `as_of`；"Empty trash older than 1 hour" | du、gc |
| **Settings / onboarding** | §5.6 | 本机 |

### 5.2 写作规则（skill「Writing」）

- 动作名一致：按钮 `Publish` → 结果 `Published`；`Approve` → `Approved`；`Push` → `Pushed`；`Discard session` → `Discarded`。
- 错误说发生了什么、怎么修，不道歉：`Daemon isn't running. Start it to see live sessions.`；`git-stratad can't read /Volumes/…: allow Removable Volumes for git-stratad in Privacy & Security.`
- 句首大写（sentence case），不用全大写标签（与 R2 `.tag` 的 uppercase 不同，见 §8.3）。
- Touch ID 框是 git-stratad 弹的，对话框里先告诉人："macOS will ask git-stratad for Touch ID. The prompt says: *push staff-api main to origin*." 文字取自 daemon 将用的 `reason`，让人核对两边一致。

### 5.3 实时更新

daemon 没有订阅流，一个连接一个请求（daemon.md）。MVP 在 Rust 侧轮询：

1. `poller.rs` 启动一个 tokio 任务；窗口可见时 `daemon_status` 每 2 s、`status --all` 每 3 s、`du` 每 60 s（`du` 走缓存，约 1 s 返回，带 `as_of/stale`）；窗口隐藏或最小化时退到 15 s / 30 s / 不轮询；菜单栏模式（§9）下只轮 daemon_status。
2. 对回答做哈希，变化才 `emit`；同类事件用 R2 `ThrottleGate` 的做法限频。
3. 只读方法不排队不加锁（daemon.md），所以轮询不会拖慢 agent；仍给 `Kind::Read` 设 5 s 超时，超时显示为 "Daemon is slow to answer"。
4. 前端：`useStrataEvents` 把事件写进 TanStack Query 缓存；Query 自身 `refetchInterval` 关掉，`refetchOnWindowFocus` 关（R2 同款），手动刷新调对应命令。
5. 后续（需 daemon 改动，§9 Q4b）：一个只读的 `watch` 方法，长连接按行推送变化，GUI 优先用它，没有就退回轮询。

### 5.4 确认类流程（以 push 为例；publish、deps approve、rm --discard、config relax、chain restore 同构）

1. 人点 `Push…`。Rust 侧 `plan::push_plan` 取出要推的 chain 顶 `shown` 与 pins（是否 `confirm`）。
2. 对话框展示：仓库、分支、远端、要推的提交（oid 短码 + 标题，等宽字体）、是否触发部署（`deploy_on_push`）、以及 Touch ID 框将显示的原文。若 pins 不要求确认，明说 "This repo pushes without confirmation"，并给 "Require confirmation" 的收紧入口（收紧立即生效、不需要认证）。
3. 人点 `Push`（主按钮，不设默认回车，避免连按）。Rust 调 `push{repo, shown}`，阶段 `awaiting_confirmation`。
4. git-stratad 在 Aqua 会话里弹 LocalAuthentication 框（DeviceOwnerAuthentication、不复用、60 s 超时、一次只一个框）。GUI 什么也不做，只显示"Look for the Touch ID prompt"。
5. 结果：成功 `Pushed`；`Conflict`/chain 顶变了 → "The chain changed since you looked. Review it again."，重新取计划；用户取消 → `Not pushed`，不重试；`Unsupported`（没有 GUI 会话或没设密码）→ 指向系统设置；`Unknown push_unknown` → 不重试，提示照 README 处理。
6. publish 额外：先 `publish_plan(group)`，展示每仓库要发的提交与 `waiting_proposals`，`shown` 为每仓库 chain 顶的映射（`PublishParams.shown: BTreeMap<RepoName, Option<CommitOid>>`）。deps approve 额外展示 `planned_moves`。

### 5.5 gc

gc 在 daemon 侧不要求 Touch ID。GUI 用 AntD 模态确认，展示 du 给出的 `trash.eligible`（条目数、可回收 `reclaim_min`–`reclaim_max`）后才调。是否加 Touch ID 见 §9 Q9。（运维规则：在真实数据上跑 gc 需 peilin 同意；GUI 的这一按钮就是那次同意。）

### 5.6 首次使用引导（开源后的易用性重点）

步骤，每步可检测、可重做，状态由 `onboarding_state` 计算：

1. **找到二进制**：`git-strata`、`git-stratad` 在不在、版本是否与 GUI 匹配（§6 打包决定它们从哪来）。
2. **配置**：没有 `~/Library/Application Support/git-strata/config.toml` 时，选数据根（`dialog` 插件选目录）、添加仓库；Rust 侧用 `strata-types` 的 Config 校验 V1–V3（绝对规范路径、根与仓库同卷、互不包含），报错按 §5.2 写法。默认值不再是 `/Volumes/GWD/inter/.strata`，改为"与第一个仓库同卷的 `<volume>/.strata`"（§9 Q6）。
3. **LaunchAgent**：展示将写入的 plist（复用 `strata_daemon::plist` 的渲染，`LimitLoadToSessionType=Aqua`），人点 `Install` 后写入并 `launchctl bootstrap gui/<uid>`；现在 README 说 cli 从不写 plist，GUI 成为第一个写它的客户端，需要确认（§9 Q5）。macOS 13+ 也可改用 `SMAppService.agent`（出现在"登录项"，用户可在系统设置里关）。
4. **权限（TCC）**：数据根在外置卷时，git-stratad 需要"可移除宗卷"授权。GUI 不能替它授权，只能检测（daemon_status 的 `store_unavailable` / EINTR 类错误）并 `open_privacy_pane` 打开对应系统设置页，写明给谁授权（git-stratad，不是 GUI）。之前 tccd 卡住造成 EINTR 的事故写进帮助。
5. **确认能力**：显示 daemon 的 `Confirmer` 是否可用（需要 daemon_status 带出 probe 结果，§9 Q4c）；不可用时说明要设登录密码 / Touch ID。
6. **Claude Code 钩子**：给出 `SessionStart` / `PostToolUse` 的配置片段，`Copy` 按钮，不替用户改 `~/.claude`。

## 6. 失败与恢复

| 情况 | 表现 | 恢复 |
|---|---|---|
| socket 不在 / 拒绝连接（`CallError::Down`） | 全局横幅 "Daemon isn't running"，数据冻结显示并标灰、标注时间 | `Start daemon`（launchctl bootstrap） |
| `store_unavailable`（82fa626，daemon 在退避重试） | 健康页说明正在重试与原因，附 TCC 引导 | 授权后自动恢复，不需重启 |
| 读超时 | 状态栏 "Daemon is slow to answer"，继续轮询 | 无需操作 |
| 写调用 `Unknown`（`reply_lost`、`push_unknown`、`socket_closed`） | 不重试，标记该操作结果未知，引导看 status | 照 README "Error codes and what to do" |
| 用户取消 Touch ID / 超时 | `Not pushed` 等，非错误样式 | 可再点一次 |
| 计划过期（G2） | 对话框作废并刷新 | 重新确认 |
| GUI 崩溃 | daemon 不受影响（GUI 无状态） | 重开 |

**打包与签名**（与 TCC 强相关）：

- **ad-hoc 签名**（`signingIdentity: "-"`）：免费，本机自用可行。缺点：下载的构建被 Gatekeeper 拦（要右键打开或去掉 quarantine）；TCC 授权按 cdhash 记，**每次更新都换二进制，可移除宗卷等授权失效、要重新给**；对 daemon 尤其痛（会再次 `store_unavailable`）。
- **Developer ID + notarization**（$99/年）：签名身份稳定，TCC 授权跨更新保留；Gatekeeper 直接放行；Tauri 在 CI 里用 `APPLE_CERTIFICATE`、`APPLE_SIGNING_IDENTITY`、`APPLE_API_KEY`/`APPLE_API_ISSUER` 自动签名并公证。R2 的 release.yml 只有 updater 的 `TAURI_SIGNING_PRIVATE_KEY`（minisign），没看到 Apple 证书变量，即 R2 目前没有公证；git-strata 想开源分发，建议补上。
- **GUI 自身不需要 TCC 授权**：它只连 `~/Library/Application Support` 下的 socket（G5）。这是坚持"所有数据经 daemon"的主要理由——否则每个新 GUI 版本都要单独申请外置卷权限，且可能复现 tccd 卡死。
- **分发**：GitHub Releases（`.dmg` + updater `latest.json`，照 R2 publish.sh / release.yml / homebrew.yml / ci.yml）；Homebrew 分两件：formula 装 `git-strata` + `git-stratad`（agent 和无 GUI 用户只要这个），cask 装 GUI。另一种是把 daemon 作为 Tauri `externalBin` 打进 `.app`，plist 指向 bundle 内路径（移动 app 会断，需要 SMAppService）；见 §9 Q7。
- **更新**：updater 换 GUI 无风险；换 daemon 前要求没有正在执行的写操作，GUI 显示 "Restart daemon to finish updating"，由人点。

## 7. 测试清单

- **strata-client**：沿用 `crates/strata-daemon/tests/daemon.rs` 的方式起一个带 `test-seams` 的进程内 daemon（临时根），覆盖每个类型化包装；契约测试：e2e 测试里录下的真实回答作为 golden JSON，类型化结构必须能解析，缺字段要失败。
- **cli 回归**：抽出后 cli 全部测试不变通过（行为零变化是 P0 的验收）。
- **Tauri 命令**：`tauri::test::mock_builder` 验证命令注册与参数形状；poller 用假 Client，验证"不变不发"、隐藏窗口降频。
- **确认流程**：假 Confirmer（daemon test-seams 的脚本化确认）下验证 GUI 发出的 `shown` 等于展示的计划；计划变化时对话框作废；用户取消时不重试。
- **安全检查（CI）**：发布构建 `cargo tree -e features` 里不得有 `tauri-plugin-connector`；capabilities 只允许主窗口、只列用到的命令；`tauri.conf.json` 的 CSP 不为 null（R2 是 null，这里不跟）；不注册 URL scheme。
- **引导**：launchctl、写 plist、打开系统设置都在 trait 后面，测试用假实现，绝不在测试里碰真实 LaunchAgent 或 `/Volumes/GWD/inter`。
- **前端**：`bun test` 测 store、事件归并、格式化（字节、相对时间、oid 短码）；`bun run check`（tsc + prettier + eslint，照 R2）。
- **界面**：dev 构建开 `connector` feature（仅 dev），用 R2 的 tauri-connector 截图做 skill 要求的视觉自查：键盘焦点、`prefers-reduced-motion`、对比度、浅深两套主题。

## 8. 实现任务（TDD）

### 8.1 分期与工作量

| 期 | 内容 | 估计 | 需改 daemon |
|---|---|---|---|
| P0 | 抽 `strata-client`，cli 与 daemon 改用；契约测试 | 1 天 | 否 |
| P1 | 照 R2 搭 `apps/desktop`（壳、token、主题、命令面板）；健康、会话列表/详情、Storage（只读）；poller 与事件 | 3 天 | 否 |
| P2 | Diff 屏（路径 + Monaco）；检查点与 restore | 2–3 天 | 是：`diff_text` 只读方法 |
| P3 | Waiting for you；publish/push/deps approve/rm --discard/config relax/chain 对话框；gc | 3 天 | 否 |
| P4 | 引导（config、LaunchAgent、TCC、钩子片段）；通知；菜单栏图标（可选） | 2–3 天 | 可选：probe 进 daemon_status |
| P5 | 签名、公证、updater、Homebrew formula + cask、CI；去掉 `com.gwd.*` 与 `/Volumes/GWD` 默认值 | 2 天 | 改 label 与默认根 |

合计约 2.5–3 周（一人）。每期先写测试再实现，按 module-rulings 的节奏，每期一个可审的提交序列。

### 8.2 视觉方向（frontend-design skill，在 R2 的 AntD 体系内）

**先做 token 计划，再按 brief 复查，再动手**（skill 的两遍法）。

**题材**：git-strata 把每个会话做成叠在基线之上的一层（base → deps → session overlay → 未推的发布链 → origin）。界面的辨识度就来自这件事：**地层柱（strata column）**——会话详情左侧一根竖向柱状图，自下而上画出这几层，每层高度对应改动量 / 提交数，点击某层就把右侧切到那一层的内容。这是全应用唯一大胆的地方；其余全部是安静的 AntD 表格、列表与表单（skill：把大胆花在一处）。

**沿用 R2（不改）**：表面与文字 token（light `--bg-app #f6f5f2`、`--bg-panel #fff`、`--bg-sunken #efece6`、`--text #181613`、muted `#6b665e`、边框 rgba(20,18,16,.08/.14/.18)；dark `#0e0d0c / #17171a / #0a0a0a / #ecebe7`）、`--danger #d4493a`、圆角 4/6/10/14/20、壳尺寸（36/264/56/52/30，行高 30/38/48）、ConfigProvider 的 `darkAlgorithm/defaultAlgorithm + colorPrimary`、accent 系统（lib/accent.ts 多选一）、self-host 字体机制、`App.useApp()`、⌘K 命令面板、主题 store。用户在 R2 和 git-strata 之间切换时应感觉是同一家族。

**skill 加上的（与 R2 的差异）**：

| token | 值 | 用途 |
|---|---|---|
| `--accent`（默认） | `#2E6F8E` 深青石蓝 | 主按钮、焦点环、选中；替代 R2 默认的 Cloudflare 橙 `#F38020`。R2 的 accent 列表保留，用户可切回橙（§9 Q3） |
| `--layer-base` | `#B8AE9C` | 地层柱：基线（沉积岩的浅褐） |
| `--layer-deps` | `#8C9A7A` | 地层柱：依赖层 |
| `--layer-session` | `#2E6F8E`（= accent） | 地层柱：会话改动，唯一高饱和层 |
| `--layer-chain` | `#C98A3D` | 地层柱：已发布未推送 |
| `--mount-live #1f9d55` | 沿用 R2 | 会话活动中的小圆点 |

共 5 个具名新色 + R2 中性色。避开 skill 点名的默认套路：不用奶油底 + 衬线 + 赤陶（R2 的 `#f6f5f2` 接近奶油，但这里不配衬线和赤陶，强调色是冷色）；不用近黑底 + 荧光绿；不用整套同半径同阴影的 SaaS 卡片（会话列表是表格行，不是卡片墙）。

**字体角色**：沿用 R2 的三款，不另引新字体（R2 一致性优先于 skill 的"挑个有个性的字体"；个性交给地层柱）。Inter Tight 只用于屏标题与会话 ticket（20/28 px）；Inter 正文 13/14 px，行宽 < 80 字符；JetBrains Mono **只用于真正的代码内容**——diff、提交 oid、路径——不用于小数据标签（skill：等宽小标签是模板味）。字号阶梯 12 / 13 / 14 / 16 / 20 / 28。

**R2 偏差记录**：R2 的 `.tag` 是 uppercase + 0.04em 字距；git-strata 的 tag 用句首大写、不加字距（skill：不要全大写标签）。不用 eyebrow 小标题、不用 `A · B · C` 式标题、按钮不加 `→`。状态栏里的 `·` 分隔只用于真正并列的读数。

**动效**：只有一处编排——会话详情首次打开时地层柱自下而上逐层长出（framer-motion，总时长 < 400 ms）；其余只有响应用户动作的过渡（展开、对话框）。`prefers-reduced-motion` 时全部关闭。

**质量底线**：所有可点元素有可见键盘焦点（accent 2 px 外环）；颜色之外另有文字或图形区分层（色弱）；浅深两套各自过对比度；Touch ID 等待态有文字，不只靠转圈。

**复查（第二遍，对照 brief）**：初稿里"仪表盘卡片 + 大数字"的首页被换成 "Waiting for you" 列表——brief 的核心是人批准 agent 的动作，首页应展示最该做的那件事，而不是统计数字（skill：hero 是最有代表性的东西）。

## 9. 构建者决定记录

已定（来自 peilin / R2）：技术栈全照 R2（§2.2）；后端全在 Rust；UI 用 frontend-design skill 并与 R2 的 AntD 风格一致；考虑开源后的易用性。

我的决定（可推翻）：
- 抽 `strata-client` 而不是让 GUI 依赖 cli 的 lib：`git_strata` 的命令函数里混着 TTY 检查和 stderr 输出，不能给 GUI 用。
- GUI 不做本地只读回退，不读数据卷（G5），换来 GUI 不需要 TCC 授权。
- GUI 不自己弹 LocalAuthentication：daemon 已是权威门，GUI 再弹一次只会教人"见框就按"，不增加安全性。
- 错误类型偏离 R2 的 `String`，用 `{code,message,detail}`。
- CSP 不跟 R2 的 null。

待 peilin 决定：
1. **Q1 位置**：`apps/desktop` 放本仓库 workspace（default-members 排除）还是 workspace 外 / 独立仓库？
2. **Q2 签名**：有没有 Apple Developer ID？决定 TCC 授权能否跨更新保留和开源分发体验。
3. **Q3 强调色**：用新的深青石蓝 `#2E6F8E` 还是保留 R2 的橙 `#F38020`？是否沿用 R2 的三款字体？
4. **Q4 daemon 增补**（都是只读）：(a) `diff_text` 返回文件两侧内容，GUI 才能不读数据卷；(b) `watch` 长连接推送，替代轮询；(c) daemon_status 带出 Confirmer probe 结果与"有确认框正在等待（由哪个方法发起）"——后者让 GUI 能提示"有一个不是你点的确认请求"，防止人对 agent 发起的 Touch ID 框随手按下。
5. **Q5 LaunchAgent**：同意 GUI 写 plist 并 bootstrap 吗？用传统 plist 还是 SMAppService？
6. **Q6 开源前去个人化**：label `com.gwd.git-stratad`、默认根 `/Volumes/GWD/inter/.strata`、plist 里的 `~/.bun/bin` PATH 改成什么？bundle id 建议 `io.github.dickwu.git-strata`（R2 是 `com.lifefarmer.r2`）。
7. **Q7 daemon 的来源**：Homebrew formula 装 CLI + daemon、cask 装 GUI（推荐），还是 daemon 打进 `.app`？
8. **Q8 语言**：界面先英文（与 cli 输出一致）还是同时中文？
9. **Q9 gc**：GUI 里的 gc 只用模态确认，还是也要 Touch ID（需 daemon 新增一个 Action）？
10. **Q10 菜单栏模式**：要不要只显示托盘图标 + 待批数（R2 已有 tray-icon 先例）？
