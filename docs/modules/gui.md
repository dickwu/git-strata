# gui 模块（设计提案）

**Crate**：`crates/strata-client`（新，从 cli 抽出）＋ `apps/desktop/src-tauri`（新，crate `git-strata-desktop`，lib `strata_desktop_lib`）· **前端**：`apps/desktop/`（Next.js 静态导出）· **设计来源**：daemon.md §3–§5、confirm.md §1 §5、cli.md §1 §3、README「Install and daemon」「Confirmation gate」 · **参照工程**：R2（github.com/dickwu/r2，已确认的技术栈）· **UI 方法**：frontend-design skill

> 状态：提案，未实现。技术栈按 R2 已定。第二版（2026-10-27）按 peilin 的回答（t49u）改写：GUI 是项目主入口、实时看到后端分配、没有 Apple ID、daemon 打进 app、界面只用英文、关窗后留在菜单栏。决定见下表，§9 记录剩余问题。本文只写设计，不含代码。

### 决定表（第二版）

| # | 决定 | 来源 | 落在 |
|---|---|---|---|
| D1 | GUI 是项目主入口：放在本仓库 `apps/desktop`，README 与引导先讲 app；cli 保留，主要给 agent 用 | t49u | §1、§5.6 |
| D2 | 实时看到后端分配（会话、工作区、基线 / 依赖集、克隆、存储）：daemon 新增只读 `watch` 事件流；轮询保留为回退 | t49u | §5.3 |
| D3 | 没有 Apple Developer ID：只做 ad-hoc 或自签名；不公证；用稳定的自签名身份让 TCC 授权跨更新保留；引导里写 Gatekeeper 首次打开步骤 | t49u | §6 |
| D4 | daemon 打进 `.app`（不走 Homebrew）；LaunchAgent 用 **SMAppService.agent + `Contents/Library/LaunchAgents` + `BundleProgram`**；cli 也打进 bundle，引导里建 PATH 符号链接；规划从 `~/.cargo/bin` + `com.gwd.git-stratad` 迁移 | t49u | §5.6、§5.7、§6 |
| D5 | 界面只用英文 | t49u | §5.2 |
| D6 | 关窗不退出，常驻菜单栏：状态 + 待处理事项速览 | t49u | §5.8 |
| D7 | 强调色深青石蓝 `#2E6F8E`，字体沿用 R2 三款 | 按建议 | §8.2 |
| D8 | 同意新增只读 daemon 方法：`diff_text`、`watch`、daemon_status 带 Touch ID 可用性与「正在等待的确认框」 | 按建议 | §3.1、§5.3 |
| D9 | `com.gwd.*` 改为 `io.github.dickwu.git-strata`（app），LaunchAgent label `io.github.dickwu.git-strata.daemon`；默认数据根与 plist PATH 去个人化（本版只写计划与迁移） | 按建议 | §5.7 |
| D10 | GUI 里的 gc 需要 Touch ID：daemon 新增 `Action::Gc` | 按建议 | §5.5（见 §9 Q-gc：daemon 分不出调用方，门只能对所有客户端生效） |

## 1. 职责与边界

gui 是 git-strata 的**主入口**（D1），给人用的桌面 app：看 daemon 健康与告警、看各会话在各仓库的状态和改动、在需要时批准（发布 / 推送 / 依赖提议 / 丢弃会话 / 放宽配置），以及首次使用的引导。目标用户是一个开发者，同时看着几个 agent 在 git-strata 会话里干活；他打开窗口要回答的问题只有三个：**谁改了什么、什么在等我、磁盘还够不够**。

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
- **与 cli 的关系**：cli 随 app 一起分发（bundle 内 `Contents/MacOS/git-strata`，引导里链接进 PATH，§5.6），主要给 agent 和钩子用。GUI 与 cli 是同一组 RPC 的两个客户端。cli 的 `require_tty` 只是 cli 自己的前置过滤，不是安全边界（confirm.md §1 威胁模型：agent 同 uid，可以直接连 socket）。GUI 不是 TTY，也不需要是：真正的门在 daemon。

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
      ├─ src/{main.rs, lib.rs, commands/, watch.rs, poller.rs, events.rs, tray.rs, onboarding/}
      └─ LaunchAgents/io.github.dickwu.git-strata.daemon.plist   # 打包时放进 Contents/Library/LaunchAgents
```

打出来的 bundle（D4）：

```
Git Strata.app/Contents/
├─ MacOS/git-strata-desktop        # GUI（Tauri 主程序）
├─ MacOS/git-stratad               # daemon（Tauri externalBin / 打包脚本拷入）
├─ MacOS/git-strata                # cli（同上；引导把它链接到 ~/.local/bin/git-strata）
├─ Library/LaunchAgents/io.github.dickwu.git-strata.daemon.plist   # BundleProgram = Contents/MacOS/git-stratad
└─ Info.plist                      # 不设 LSUIElement（窗口模式要有 Dock 图标，关窗后切 Accessory，§5.8）
```

三个可执行文件用同一个签名身份签（§6），各自 identifier：`io.github.dickwu.git-strata`、`…git-strata.daemon`、`…git-strata.cli`。

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
| 插件 | opener、os、notification、updater、process、dialog；tauri `tray-icon` feature | R2 的子集加 notification；**不要 fs、http 插件**（GUI 不读数据卷，见 §6）；SMAppService 用 `objc2-service-management` 在 Rust 里调 |

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
/// 放在 feature `local-plan` 后面：它要打开主 checkout 的 git 和 store（在数据卷上）。
/// GUI 不开这个 feature（G5），改走 daemon 新增的只读 `push_plan`（§9 Q-plan）与已有的 publish_plan。
#[cfg(feature = "local-plan")]
pub mod plan {
    pub fn push_plan(config: &Config, repo: &RepoName) -> Option<PushPlan>;   // 含要推的 chain 顶 = shown
}
/// 订阅（D2）：一条长连接，按行读事件；daemon 回 UnknownMethod 时返回 Unsupported，调用方退回轮询。
pub fn watch(&self, since: Option<Cursor>) -> Result<EventStream, CallError>;
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
| storage.rs | `du`、`gc` | du、gc（新 `Action::Gc`） | 只读 / 写，Touch ID（D10） |
| deps.rs | `deps_list`、`deps_propose`、`deps_approve` | 同名 | 写，Touch ID |
| publish.rs | `publish_plan`、`publish`、`push`、`chain_resolve`、`resolve_move` | 同名 | 写，按 pins 需 Touch ID |
| sessions.rs | `rm` | rm（`discard` 时 Touch ID） | 写 |
| config.rs | `config_get`、`config_relax`、`config_check` | 读文件 + config_relax | 写，Touch ID |
| onboarding/ | `onboarding_state`、`write_config`、`register_agent`、`unregister_agent`、`link_cli`、`migrate_legacy`、`open_privacy_pane` | 本机 | 见 §5.6、§5.7 |
| tray.rs | 无命令；托盘菜单只打开主窗口到对应项 | — | — |

\* `diff_text` 需要 daemon 新增一个只读方法，见 §9 Q4。

全部写命令是 `async`，内部 `spawn_blocking(client.call(..))`；开始时发 `strata://op` 事件 `{op_id, method, phase:"started"}`，结束发 `"done"`/`"failed"`。daemon 不流式回报进度，所以界面上只有"等待确认（看 Touch ID 框）"与"执行中"两种中间态，按方法静态区分。

### 3.3 事件

| 事件 | 负载 | 来源 |
|---|---|---|
| `strata://health` | `DaemonStatus` + `reachable` + `source`（watch / poll） | watch 或 poller，变化才发 |
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
- **G7 托盘不批准**：菜单栏菜单只显示状态与待处理数，点击只打开主窗口到对应对话框；托盘里没有任何写操作（Quit 只退 GUI，不停 daemon）。
- **G8 bundle 内 cli 与外部 cli 同权**：bundle 里的 `git-strata` 没有额外能力（不借 GUI 的签名特权、不读 GUI 状态），确认仍由 daemon 弹框。
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
| **Allocations / Storage** | 实时（D2）：基线与依赖集的克隆、哪些会话引用哪个基线、进行中的克隆 / 发布；每组 logical / allocated / private、可回收的 trash（`reclaim_min`–`reclaim_max`）、savings、扫描状态与 `as_of`；"Empty trash older than 1 hour" | du、gc |
| **Settings / onboarding** | §5.6 | 本机 |

### 5.2 写作规则（skill「Writing」）

- 界面只用英文（D5），不做 i18n 框架；文案集中在 `src/lib/copy.ts`。

- 动作名一致：按钮 `Publish` → 结果 `Published`；`Approve` → `Approved`；`Push` → `Pushed`；`Discard session` → `Discarded`。
- 错误说发生了什么、怎么修，不道歉：`Daemon isn't running. Start it to see live sessions.`；`git-stratad can't read /Volumes/…: allow Removable Volumes for git-stratad in Privacy & Security.`
- 句首大写（sentence case），不用全大写标签（与 R2 `.tag` 的 uppercase 不同，见 §8.3）。
- Touch ID 框是 git-stratad 弹的，对话框里先告诉人："macOS will ask git-stratad for Touch ID. The prompt says: *push staff-api main to origin*." 文字取自 daemon 将用的 `reason`，让人核对两边一致。

### 5.3 实时更新：`watch` 事件流（D2）与轮询回退

现在 daemon 一个连接一个请求，没有推送。新增只读方法 `watch`：

1. **连接与握手**：客户端发 `{"id","method":"watch","params":{"v":1,"since":{"epoch":E,"seq":N}|null,"topics":[...]|null}}`。daemon 先回一行普通回答 `{"id","ok":true,"data":{"v":1,"epoch":E,"seq":S,"resync":bool}}`，然后**保持连接**，之后每行一个事件。旧 daemon 回 `error.rpc = UnknownMethod` → 客户端退回轮询。
2. **事件信封**：`{"v":1,"epoch":E,"seq":n,"kind":"…","key":"…","at":"rfc3339","data":{…}}`。`epoch` 每次 daemon 启动换新（启动时间 + pid），`seq` 在 epoch 内单调递增。
3. **事件种类**（v1，均为「某物变了」的提示加小摘要，不是完整数据；客户端按需用已有只读方法取全量，数据只有一个来源）：
   - `health`：DaemonWarning 集合、`store_unavailable`、启动 / 停止中
   - `session.created` / `session.changed` / `session.removed`（key = session id；摘要：仓库数、改动数、最后检查点）
   - `workspace.changed`（key = session/repo；摘要：改动计数、检查点 id）
   - `base.created` / `base.evicted`、`deps.set.created` / `deps.set.evicted`（基线与依赖集缓存，「后端分配」的核心）
   - `clone.started` / `clone.finished`（APFS clonefile 进行中，含目标与耗时）
   - `proposal.created` / `proposal.changed`
   - `publish.started` / `publish.finished`、`push.started` / `push.finished`（含结果 kind）
   - `confirm.prompting` / `confirm.resolved`（Action 种类、方法名、仓库 / 提议；**不含**任何可复用的东西）——GUI 用它提示「有一个不是你点的确认框」
   - `du.updated`（`as_of`）、`gc.finished`
   - `resync`（见 5）
4. **合并**：daemon 内每个订阅者一个按 `(kind,key)` 去重的待发表；同一 key 在 100 ms 窗口内只发最后一次。`*.started`/`*.finished`、`confirm.*` 不合并。
5. **背压**：每个订阅者一个有界队列（256 条）和一个专用写线程，写超时 2 s；daemon 的业务线程只往队列里放，从不阻塞在慢客户端上。队列满或写超时 → 清空队列、发一条 `resync`、继续；客户端收到 `resync` 就重新取全量（daemon_status、status --all、du）。订阅者上限 8 个，超出回 `Busy`（reason `too_many_watchers`）。
6. **断线续传**：daemon 保留最近 1024 条事件的环形缓冲；`since` 的 epoch 相同且 seq 仍在缓冲内 → 补发，否则回答里 `resync: true`。
7. **版本**：`v` 只做加法（新 kind、新字段）；客户端忽略未知 kind 与字段；不兼容的改动才升 `v:2`，daemon 同时支持旧版至少一个发布周期。
8. **安全**：只读，和 status 暴露的信息相同；不进会话队列，不拿锁；socket 仍是 0600。事件在状态落盘之后发出（先写后报）。
9. **GUI 侧**：`watch.rs` 一个线程读流，断线指数退避重连（0.5 s → 10 s）；连不上或旧 daemon → 启动 `poller.rs`（窗口可见时 daemon_status 2 s、status --all 3 s、du 60 s；隐藏时 15 s / 30 s / 不轮）。两者都只在内容变化时 `emit`，同类事件按 R2 `ThrottleGate` 限频；前端 `useStrataEvents` 写 TanStack Query 缓存，Query 自身不定时刷新。cli 不订阅。

### 5.4 确认类流程（以 push 为例；publish、deps approve、rm --discard、config relax、chain restore 同构）

1. 人点 `Push…`。Rust 侧 `plan::push_plan` 取出要推的 chain 顶 `shown` 与 pins（是否 `confirm`）。
2. 对话框展示：仓库、分支、远端、要推的提交（oid 短码 + 标题，等宽字体）、是否触发部署（`deploy_on_push`）、以及 Touch ID 框将显示的原文。若 pins 不要求确认，明说 "This repo pushes without confirmation"，并给 "Require confirmation" 的收紧入口（收紧立即生效、不需要认证）。
3. 人点 `Push`（主按钮，不设默认回车，避免连按）。Rust 调 `push{repo, shown}`，阶段 `awaiting_confirmation`。
4. git-stratad 在 Aqua 会话里弹 LocalAuthentication 框（DeviceOwnerAuthentication、不复用、60 s 超时、一次只一个框）。GUI 什么也不做，只显示"Look for the Touch ID prompt"。
5. 结果：成功 `Pushed`；`Conflict`/chain 顶变了 → "The chain changed since you looked. Review it again."，重新取计划；用户取消 → `Not pushed`，不重试；`Unsupported`（没有 GUI 会话或没设密码）→ 指向系统设置；`Unknown push_unknown` → 不重试，提示照 README 处理。
6. publish 额外：先 `publish_plan(group)`，展示每仓库要发的提交与 `waiting_proposals`，`shown` 为每仓库 chain 顶的映射（`PublishParams.shown: BTreeMap<RepoName, Option<CommitOid>>`）。deps approve 额外展示 `planned_moves`。

### 5.5 gc

D10：新增 `Action::Gc { reclaim_max }`，daemon 的 gc 先 `Confirmer::require`，Touch ID 框文字写可回收上限。GUI 先展示 du 的 `trash.eligible`（条目数、`reclaim_min`–`reclaim_max`）再调。注意：daemon 分不出是 GUI 还是 cli 调的（同 uid、无握手），所以这个门对 cli 的 `gc` 同样生效——agent 将不能再无人值守地 gc（§9 Q-gc）。

### 5.6 首次使用引导（开源后的易用性重点，D1）

README 第一节改成「下载 app → 首次打开 → 引导」，cli 用法挪到 "For agents" 一节。引导每步可检测、可重做，状态由 `onboarding_state` 计算：

0. **Gatekeeper 首次打开**（D3，app 外的步骤，写在 README、DMG 背景图和发布说明里）：macOS 15 起右键「打开」不再能绕过未公证 app。步骤：双击 → 被拦 → 系统设置 › 隐私与安全性 › 找到 "Git Strata was blocked" → **Open Anyway** → 输管理员密码。DMG 本身也可能被拦一次，同样处理。终端替代：`xattr -dr com.apple.quarantine "/Applications/Git Strata.app"`。之后用 app 内 updater 更新，下载的包不带 quarantine 属性，不再触发（P5 在干净虚拟机上验证）。
1. **迁移检测**：发现旧安装（§5.7）就先走迁移。
2. **配置**：没有 config.toml 时选数据根（`dialog` 选目录）、添加仓库；Rust 侧用 `strata-types` 的 Config 校验 V1–V3。没有默认根：建议「与第一个仓库同卷的 `<volume>/.strata`」，人确认。
3. **注册 daemon**：`SMAppService.agent(plistName: "io.github.dickwu.git-strata.daemon.plist").register()`（§5.7 选型）。状态 `requiresApproval` 时说明并 `openSystemSettingsLoginItems()`。
4. **权限（TCC）**：数据根在外置卷时，git-stratad 需要「可移除宗卷」授权（授给 daemon，不是 GUI）。检测 `store_unavailable` / EINTR 类错误，`open_privacy_pane`，写明授权对象。帮助里记下 tccd 卡住导致 EINTR 的旧事故。
5. **确认能力**：daemon_status 的 Confirmer probe（D8）；不可用时说明要设登录密码 / Touch ID。
6. **命令行工具**：`Install command-line tool` → 建 `~/.local/bin/git-strata` → bundle 内 cli 的符号链接（不需要 sudo）；`~/.local/bin` 不在 PATH 时给出要加到 shell 配置的一行（不替用户改）。每次启动校验链接目标仍在（app 被移动则提示重建）。`git strata` 形式因 git 在 PATH 里找 `git-strata` 自动可用。
7. **Claude Code 钩子**：给出 `SessionStart` / `PostToolUse` 片段与 `Copy`，不改 `~/.claude`。
8. **daemon 的 PATH**：不再写死 `~/.bun/bin`；引导时运行一次用户登录 shell 取 PATH（`$SHELL -lc 'printf %s "$PATH"'`），展示给人确认后写入 config.toml 的新字段，daemon 启动时读取（bundle 内的 plist 只读，§9 Q-path）。

### 5.7 LaunchAgent 选型与迁移（D4、D9）

**选 SMAppService.agent**，不选 app 写 `~/Library/LaunchAgents` 的传统 plist。

| | SMAppService.agent（选） | 传统 plist（app 写入） |
|---|---|---|
| ad-hoc / 自签名能用吗 | 能（Apple 只要求「已签名」，ad-hoc 算；LaunchDaemon 才要求公证）。第三方在 macOS 26.6 上实测 ad-hoc 的 SMAppService 注册可用、重建后仍有效、移动 app 后仍有效（测的是 mainApp，agent 需 P4 实测） | 能 |
| app 被移动 | `BundleProgram` 是相对 bundle 的路径，跟着走 | 绝对路径失效，launchd 报 78 EX_CONFIG，系统仍显示「已启用」，需要每次启动自检重写 |
| 用户可见性 | 系统设置 › 登录项里以 app 名出现，可开关；app 能读到真实状态（`enabled`/`requiresApproval`/`notFound`） | 显示为「来自身份不明开发者的项目」，状态读不准 |
| 更新 | 可执行文件变了要 `unregister` + `register`（Apple 文档），新版首次启动时做；做之前确认没有写操作在跑 | 路径不变就继续用 |
| 卸载 | 设置里 `Unregister`；删 app 后 BTM 清理 | 留下 plist，要提供卸载按钮 |
| 代码量 | Rust 里经 `objc2-service-management` 调几行；cli 的 `daemon start/stop` 改为 `launchctl kickstart -k` / `bootout gui/<uid>/<label>`（不再 bootstrap 一个文件） | 复用现有 `plist.rs` / `launch.rs` |
| 最低系统 | macOS 13 | 任意 |

理由：开源用户会把 app 放在不同位置，传统 plist 会静默失效；SMAppService 能读到真实状态，引导能据此给出准确提示。代价是最低支持 macOS 13（`minimumSystemVersion: 13.0`），以及每次更新要重新注册。

**迁移**（从现在的 `~/.cargo/bin/git-strata{,d}` + `~/Library/LaunchAgents/com.gwd.git-stratad.plist`）：

1. 检测：旧 plist 存在、`launchctl print gui/<uid>/com.gwd.git-stratad` 有记录、`~/.cargo/bin` 下有旧二进制、PATH 里排在前面的 `git-strata` 不是 bundle 的。
2. 前置：经旧 daemon 的 daemon_status 确认没有进行中的写操作（publish / push / 克隆）；有就等，不强停。
3. 人点 `Migrate`：`launchctl bootout gui/<uid>/com.gwd.git-stratad`（等它退出，同 launch.rs 的 10 s 等待），把旧 plist 改名为 `.plist.disabled`（不删，可回滚）。
4. SMAppService 注册新 agent，新 daemon 用同一个 `~/Library/Application Support/git-strata/`（config.toml、pins.json、socket 路径不变）、同一个数据根——会话、store、基线原样继续，不需要数据迁移。
5. TCC：新 daemon 是另一个二进制、另一个签名，「可移除宗卷」要重新授权一次（之后自签名身份稳定，跨更新保留，§6）。
6. PATH：`~/.cargo/bin` 通常排在 `~/.local/bin` 前面；引导提示把旧 `git-strata` 改名或调整 PATH 顺序（删文件由用户自己做或明确同意）。新增 cli 与 daemon 的版本对比（daemon_status 带 `version`），不一致时 cli 在 stderr 提醒。
7. 回滚：设置里 `Restore previous daemon` = unregister 新 agent、把 `.disabled` 改回、`launchctl bootstrap`。
8. label：`com.gwd.git-stratad` → `io.github.dickwu.git-strata.daemon`；cli 的 `launch.rs` 在一个版本周期内两个 label 都认。默认数据根 `/Volumes/GWD/inter/.strata` 改为「无默认，必须配置」；已有 config.toml 写明了 root，不受影响。

### 5.8 菜单栏常驻（D6）

- 关窗（⌘W、红点）→ `CloseRequested` 里 `prevent_close` 并隐藏窗口，切换 `ActivationPolicy::Accessory`（Dock 图标消失）；打开窗口时切回 `Regular`。⌘Q / 托盘 `Quit Git Strata` 才退出 GUI；**退出 GUI 不影响 daemon**（daemon 由 launchd 管）。
- 托盘图标：模板图（单色三道地层横线），状态靠形状而不只靠颜色：正常 / 有待处理（小圆点）/ daemon 不可达（带斜杠）。
- 托盘菜单：状态行（"Daemon running · 5 sessions"）；"Waiting for you" 下最多 5 项（如 "Approve deps proposal #12 for tool"），点击打开主窗口并定位到那一项的对话框（G7：托盘本身不执行）；"Open Git Strata"；"Pause notifications for 1 hour"；"Quit Git Strata"。
- 数据来自同一条 watch 流；窗口隐藏时仍保持 watch（开销很小），轮询回退时只轮 daemon_status。
- 开机启动：设置里 "Open at login"（`SMAppService.mainApp`），默认关；daemon 的开机启动由它自己的 agent 负责，与 GUI 无关。

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

**打包与签名**（D3：没有 Apple Developer ID）：

- **ad-hoc**（`codesign -s -`）：designated requirement（DR）就是 cdhash，**每次更新二进制都变**，TCC 认为是新程序：daemon 的「可移除宗卷」授权每次更新都丢，会再次 `store_unavailable`。只适合从源码自己构建的开发者。
- **稳定的自签名身份（选）**：项目生成一个长期有效的自签名代码签名证书，私钥只放在 GitHub Actions secret，release.yml 用它签 GUI、daemon、cli。codesign 为非 Apple 证书生成的 DR 形如 `identifier "io.github.dickwu.git-strata.daemon" and certificate leaf = H"…"`，只要 identifier 与证书不变，TCC 授权跨更新保留（P5 在干净虚拟机上验证：授权 → 更新 → 授权仍在）。
  - 代价：Gatekeeper 依然当它是「身份不明开发者」（未公证），首次打开见 §5.6 第 0 步；证书泄露 = 别人能签出 TCC 认作同一程序的二进制并继承授权，所以私钥只在 CI；轮换会让所有用户重新授权一次；`spctl` 评估仍是 rejected，不影响运行。
  - 从源码构建的用户：`scripts/make-signing-identity.sh` 在本机登录钥匙串里生成自己的自签名身份，本机重建也保留授权（DR 与官方发布不同，换用官方包要重新授权一次）。
  - 第三方测试显示自签名对登录项没有可见区别；它的价值只在 TCC（DR 稳定）。
- **updater**：沿用 R2 的 tauri-plugin-updater（minisign 签 `latest.json` 与包，和 codesign 无关）。app 内下载的更新不带 quarantine 属性，不再触发 Gatekeeper（P5 验证）。更新 daemon：新版首次启动时确认没有写操作在跑 → `unregister` + `register` agent → daemon 重启；有写操作就显示 "Restart daemon to finish updating"，由人点。
- **分发**：只走 GitHub Releases（`.dmg` + `latest.json`），照 R2 的 publish.sh / release.yml / ci.yml；不做 Homebrew（D4）。R2 的 release.yml 也没有 Apple 证书变量。
- **GUI 自身不需要 TCC 授权**：它只连 `~/Library/Application Support` 下的 socket（G5）。
- **以后有了 Developer ID**：换证书 = DR 变 = 所有用户重新授权一次；公证后 Gatekeeper 步骤可删。

## 7. 测试清单

- **strata-client**：沿用 `crates/strata-daemon/tests/daemon.rs` 的方式起一个带 `test-seams` 的进程内 daemon（临时根），覆盖每个类型化包装；契约测试：e2e 测试里录下的真实回答作为 golden JSON，类型化结构必须能解析，缺字段要失败。
- **cli 回归**：抽出后 cli 全部测试不变通过（行为零变化是 P0 的验收）。
- **Tauri 命令**：`tauri::test::mock_builder` 验证命令注册与参数形状；poller 用假 Client，验证"不变不发"、隐藏窗口降频。
- **确认流程**：假 Confirmer（daemon test-seams 的脚本化确认）下验证 GUI 发出的 `shown` 等于展示的计划；计划变化时对话框作废；用户取消时不重试。
- **安全检查（CI）**：发布构建 `cargo tree -e features` 里不得有 `tauri-plugin-connector`；capabilities 只允许主窗口、只列用到的命令；`tauri.conf.json` 的 CSP 不为 null（R2 是 null，这里不跟）；不注册 URL scheme。
- **watch**：进程内 daemon 上订阅，验证事件在状态落盘之后发出、合并窗口、慢订阅者触发 `resync` 而不阻塞写路径、epoch 变化后 `resync: true`、旧 daemon（UnknownMethod）时客户端退回轮询。
- **托盘**：关窗只隐藏、托盘菜单项只发「打开窗口到 X」、Quit 不发任何 RPC。
- **引导**：SMAppService、launchctl、写 plist、打开系统设置都在 trait 后面，测试用假实现，绝不在测试里碰真实 LaunchAgent 或 `/Volumes/GWD/inter`。
- **前端**：`bun test` 测 store、事件归并、格式化（字节、相对时间、oid 短码）；`bun run check`（tsc + prettier + eslint，照 R2）。
- **界面**：dev 构建开 `connector` feature（仅 dev），用 R2 的 tauri-connector 截图做 skill 要求的视觉自查：键盘焦点、`prefers-reduced-motion`、对比度、浅深两套主题。

## 8. 实现任务（TDD）

### 8.1 分期与工作量

| 期 | 内容 | 估计 | 需改 daemon |
|---|---|---|---|
| P0 | 抽 `strata-client`（含 `local-plan` feature），cli 与 daemon 改用；契约测试 | 1 天 | 否 |
| P1 | 照 R2 搭 `apps/desktop`（壳、token、主题、命令面板）；健康、会话列表/详情、Allocations/Storage（只读，轮询）；菜单栏常驻 | 3–4 天 | 否 |
| P2 | daemon 增补：`watch`、`diff_text`、`push_plan`、daemon_status 的 probe / 等待中的确认 / version、`Action::Gc`；GUI 接 watch | 4–5 天 | 是 |
| P3 | Diff 屏；Waiting for you；publish / push / deps approve / rm --discard / config relax / chain / gc 对话框 | 4 天 | 否 |
| P4 | bundle（daemon + cli 进 app）、SMAppService 注册、cli 符号链接、迁移与回滚、引导、通知 | 4 天 | label 与默认根 |
| P5 | 自签名身份、release CI、updater、DMG 首次打开说明；虚拟机上验证 TCC 跨更新与 Gatekeeper 步骤 | 2–3 天 | 否 |

合计约 3.5–4 周（一人）。P2 的 watch 要在 daemon 主循环与各写路径加发出点，和 impl/v1 上并行的改动冲突面最大，建议在 v1 合并后再做。每期先写测试再实现，按 module-rulings 的节奏，每期一个可审的提交序列。

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

已定：技术栈全照 R2（§2.2）；后端全在 Rust；UI 用 frontend-design skill 并与 R2 的 AntD 风格一致；考虑开源后的易用性；第二版决定表 D1–D10。

我的决定（可推翻）：
- 抽 `strata-client` 而不是让 GUI 依赖 cli 的 lib：`git_strata` 的命令函数里混着 TTY 检查和 stderr 输出。
- GUI 不做本地只读回退，不读数据卷（G5），换来 GUI 不需要 TCC 授权；因此计划读取放在 `local-plan` feature 后，GUI 走 daemon。
- GUI 不自己弹 LocalAuthentication：daemon 已是权威门。
- 错误类型偏离 R2 的 `String`，用 `{code,message,detail}`；CSP 不跟 R2 的 null。
- LaunchAgent 用 SMAppService.agent（§5.7），最低 macOS 13。
- 签名用项目级稳定自签名身份（§6）。
- 事件是「变了」的提示 + 摘要，全量仍用只读方法取（§5.3）。

剩余问题：
1. **Q-gc**：daemon 分不出 GUI 与 cli，`Action::Gc` 会让 cli 的 gc 也要 Touch ID，agent 不能再无人值守 gc。接受吗？（另一种：gc 不加门，GUI 只用模态确认，但那不是安全边界。）
2. **Q-plan**：为了 G5，新增只读 `push_plan`（daemon 读 chain 顶与提交列表）。同意吗？
3. **Q-path**：daemon 的 PATH 改由 config.toml 新字段 `[daemon] path` 提供（bundle 内 plist 只读）。同意吗？
4. **Q-watch-timing**：watch 改动面大，是否等 impl/v1 合并后再做（P1 先用轮询）？
5. **Q-cert**：谁持有自签名证书私钥（只在 GitHub secret？本机备份？），有效期多长？
6. **Q-macos**：最低 macOS 13 可以吗？
7. **Q-cli-link**：cli 链接到 `~/.local/bin`（免 sudo）还是 `/usr/local/bin`（要管理员）？
