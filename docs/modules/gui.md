# gui 模块（设计提案）

**Crate**：`crates/strata-client`（新，从 cli 抽出的纯传输层）＋ `apps/desktop/src-tauri`（新，crate `git-strata-desktop`，lib `strata_desktop_lib`，不在核心 workspace 里）· **前端**：`apps/desktop/`（Next.js 静态导出）· **设计来源**：daemon.md §3–§6、confirm.md §1 §3 §5、cli.md §1 §3、config.md、types.md §8、README「Install and daemon」「Confirmation gate」 · **参照工程**：R2（github.com/dickwu/r2，已确认的技术栈）· **UI 方法**：frontend-design skill

> 状态：提案，未实现。第三版（2026-10-27，r3.4）。第二版（ed7a85a）按 peilin 的回答（t49u）定下 D1–D10；Codex 与 Claude 复核了第二版（review-notes/gui-merged.md），peilin 10:28 回答了四个问题（D11–D14）。本版逐条处理复核结论：每条结论在哪一节落地，见文末「复核对照」表。第三版的确认复核（review-notes/gui-r3-*.md）又提出 4 条阻断与若干小项，已在 r3.1 就地修正；r3.1 的再复核（gui-r3.1-*.md）指出停机栅栏仍有检查与停机之间的空档，r3.2 改为 daemon 内原子封口；r3.2 的再复核（gui-r3.2-*.md）指出封口后的 60 s 自退会被 KeepAlive 拉起一个未封口的新进程、旧 daemon 迁移挡不住已建立的连接，r3.3 改为「封口即退出 + 退出码决定是否拉起」与「旧 daemon 无客户端连接才迁移」，r3.3 的再复核指出「客户端已断开、处理线程还在跑」的请求核验不到，peilin 选 (a)（11:31），r3.4 改为按副本数核验连接线程，并按 peilin 11:32（t51u）把迁移收窄到他本机这一套旧安装，见「复核对照」的 r3 部分。本文只写设计，不含代码；daemon 侧要加的东西全部列在 §3.4，分期写进 §8.1。

### 决定表

| # | 决定 | 来源 | 落在 |
|---|---|---|---|
| D1 | GUI 是项目主入口：放在本仓库 `apps/desktop`，README 与引导先讲 app；cli 保留，主要给 agent 用 | t49u | §1、§5.6 |
| D2 | 实时看到后端分配（会话、工作区、基线 / 依赖集、克隆、存储）：daemon 新增只读 `watch` 事件流；轮询保留为回退 | t49u | §5.3 |
| D3 | 没有 Apple Developer ID：只做自签名（稳定身份），不公证；引导里写 Gatekeeper 首次打开步骤 | t49u | §6 |
| D4 | daemon 与 cli 打进 `.app`（不走 Homebrew）；LaunchAgent 首选 SMAppService.agent，**是否可行由 P-pack 实测决定**，不行就用 plist.rs 的传统 plist（D13） | t49u、D13 | §5.7、§8.1 |
| D5 | 界面只用英文 | t49u | §5.2 |
| D6 | 关窗不退出，常驻菜单栏 | t49u | §5.8 |
| D7 | 强调色深青石蓝：浅色 `#2E6F8E`，深色 `#6FB3D2`（对比度 ≥ 4.5:1，§8.2） | 按建议，r3 修正 | §8.2 |
| D8 | 同意新增 daemon 只读方法（清单与分期见 §3.4） | 按建议 | §3.4 |
| D9 | `com.gwd.*` 改为 `io.github.dickwu.git-strata`（app），LaunchAgent label `io.github.dickwu.git-strata.daemon`；默认数据根与 plist PATH 去个人化 | 按建议 | §5.7 |
| ~~D10~~ | ~~gc 需要 Touch ID（`Action::Gc`）~~ —— **撤销**，见 D11 | — | — |
| D11 | **gc 不走 Touch ID**。删掉 `Action::Gc` 和 gc 门；GUI 用普通确认对话框，文案如实说 gc 做什么（每仓库 `gc_repo`，全局 gc 另加 `gc_trash`），并说明 daemon 启动时与每小时 reconcile 时本来就会自动 gc | peilin 10:28 | §5.5 |
| D12 | **推送批准绑定目标 URL**：由 impl/v1 的另一位 worker 在核心 daemon 里实现——门之前解析出实际 push URL，写进计划与 Touch ID 文案，锁内复核，不一致返回 `remote_changed`。GUI 依赖这一点，不自己读 remote | peilin 10:28 | §4 G2、§5.4 |
| D13 | **打包验证作为并行轨道 P-pack**，和 UI 工作同时进行：SMAppService.agent + 自签名、TCC 与 Touch ID 框显示的名字、内部二进制的 Gatekeeper / quarantine、plist.rs 回退 | peilin 10:28 | §8.1 |
| D14 | 工期 6–7 周（一人），接受 | peilin 10:28 | §8.1 |

## 1. 职责与边界

gui 是 git-strata 的**主入口**（D1），给人用的桌面 app：看 daemon 健康与告警、看各会话在各仓库的状态和改动、看后端分配（基线、依赖集、克隆、磁盘）、在需要时批准（发布 / 推送 / 依赖提议 / 丢弃会话 / 放宽配置），以及首次使用的引导、升级与迁移。目标用户是一个开发者，同时看着几个 agent 在 git-strata 会话里干活；他打开窗口要回答的问题只有三个：**谁改了什么、什么在等我、磁盘还够不够**。

- **做**：
  - Tauri v2 外壳；业务在 Rust 侧，经 `strata-client` 调 daemon 的 RPC，JS 只做展示与交互状态。
  - 订阅 daemon 的 `watch`（§5.3），把「某物变了」转成 Tauri 事件；前端据此刷新对应查询。连不上 watch 时按 §5.3 的统一表轮询。
  - 调确认类方法**之前**，把 daemon 给出的计划原样展示出来，并把 `shown` / `plan` / `planned` 原样带回 daemon（§5.4）。计划全部由 daemon 算，GUI 不在本地读 git 或 store（G5）。
  - 首次使用引导、daemon 的安全重启（drain，§5.9）、升级与从旧安装迁移、回滚（§5.7）。
  - 系统通知（daemon 掉线、待批提议、`Unknown` 结果、告警）。
- **不做**：
  - 不弹 LocalAuthentication、不签发也不缓存任何「已确认」状态（确认只在 daemon 的 `Confirmer::require`，confirm I7）。
  - 不打开 `paths.root` 之下和任何仓库工作树里的文件（G5，理由见 §6 TCC）。daemon 不在时不走 cli 的本地只读路径，只显示「daemon 未运行」和启动按钮。
  - 不提供 cli 没有的写操作，不提供「跳过确认」「全部批准」「自动批准被取代的提议」。
  - 不在 JS 里拼 RPC、不在 JS 里判断能否推送。
- **与 cli 的关系**：cli 随 app 分发（bundle 内 `Contents/MacOS/git-strata`，引导里链接进 PATH），主要给 agent 和钩子用。GUI 与 cli 是同一组 RPC 的两个客户端，共用 `strata-client` 的传输层。cli 的 `require_tty` 只是 cli 自己的前置过滤，不是安全边界（confirm.md §1：agent 同 uid，可以直接连 socket）；真正的门在 daemon。
- **design.md 的修订**：design.md §2「不做」里「图形界面。v1 只有 CLI」与 §10 第 5 条「v1 无 GUI」在本版同一提交里改为「v1 先出 CLI；GUI 按 docs/modules/gui.md 在 v1 之后做，daemon 仍是唯一的权威与确认门」。

## 2. 依赖

### 2.1 目录与 workspace 布局

```
git-strata/
├─ Cargo.toml                 # 核心 workspace：members 增加 crates/strata-client；
│                             # exclude 增加 apps/desktop/src-tauri（tauri 不进核心 Cargo.lock、cargo test、clippy）
├─ crates/
│  ├─ strata-client/          # 新：纯传输层与线上数据类型（§3.1），只依赖 strata-types、serde、serde_json
│  ├─ git-strata/             # src/rpc.rs 变成对 strata-client 的 re-export；本地计划读取留在 cli
│  └─ strata-daemon/          # client.rs 在 P0 不动（语义不同，见 §3.1）；P2 起用 strata-client 的数据类型序列化回答
└─ apps/desktop/              # 照 R2 的仓库根布局，挪到 apps/desktop 下
   ├─ package.json            # bun；scripts 与 R2 同名：dev / build / tauri:dev / check / test
   ├─ next.config.ts          # output:'export', distDir:'dist'
   ├─ public/fonts/           # 自托管 woff2 + OFL（R2 规则）
   ├─ src/app/ src/components/ src/lib/ src/stores/ src/hooks/
   └─ src-tauri/              # 自己的 Cargo.lock；path 依赖 ../../../crates/strata-client 与 strata-types
      ├─ Cargo.toml  tauri.conf.json  capabilities/default.json  Info.plist
      ├─ src/{main.rs, lib.rs, commands/, watch.rs, poller.rs, events.rs, tray.rs, lifecycle/, onboarding/}
      └─ LaunchAgents/io.github.dickwu.git-strata.daemon.plist   # 仅当 P-pack 选定 SMAppService 时打包
```

打出来的 bundle：

```
Git Strata.app/Contents/
├─ MacOS/git-strata-desktop        # GUI
├─ MacOS/git-stratad               # daemon
├─ MacOS/git-strata                # cli（引导把它链接到 ~/.local/bin/git-strata）
├─ Library/LaunchAgents/io.github.dickwu.git-strata.daemon.plist   # SMAppService 方案：BundleProgram = Contents/MacOS/git-stratad
└─ Info.plist                      # 不设 LSUIElement（窗口模式有 Dock 图标，关窗后切 Accessory，§5.8）
```

三个可执行文件用同一个签名身份签（§6），identifier 分别是 `io.github.dickwu.git-strata`、`…git-strata.daemon`、`…git-strata.cli`。

desktop 放在核心 workspace 之外（`exclude`）的理由：核心的 `cargo test --workspace` / clippy / CI 时间与 Cargo.lock 都不受 tauri 依赖树影响；desktop 只经 path 依赖用 `strata-client` 与 `strata-types`，改动两边仍在编译期同步（desktop 的 CI 单独跑）。

### 2.2 技术栈（照 R2，已定）

| 层 | 选择 | 理由 / 与 R2 的关系 |
|---|---|---|
| 外壳 | Tauri 2.12 | R2 同款 |
| 前端 | Next.js 16 静态导出、React 19、TypeScript | R2 同款；产物是 `dist/` |
| 包管理与测试 | Bun（`bun test`） | R2 规则：只用 Bun；tauri CLI 用 devDependency `@tauri-apps/cli` |
| 组件 | AntD 6 + @ant-design/icons 6 | R2 同款；`App.useApp()` 取 message/notification/modal |
| 样式 | Tailwind 4 + R2 的 CSS 变量 | 共用 R2 的 token 与壳 |
| 状态 | Zustand（UI 状态）、TanStack Query 5（daemon 数据） | R2 同款；Query 不设定时刷新，由 Rust 事件按 §5.3 的失效表触发 refetch |
| 长列表 | react-virtuoso | 会话 × 仓库、文件、检查点 |
| 代码 / diff | Monaco（DiffEditor，只读） | R2 同款 |
| 动效 | framer-motion | 只用在 §8.2 那一处 |
| Rust↔前端 | 普通 `#[tauri::command]` + `generate_handler!`；流式用事件 | R2 同款；不用 tauri-specta，类型两边手写 |
| 插件 | opener、os、notification、updater、process、dialog、**single-instance**；tauri `tray-icon` feature | R2 子集加 notification 与 single-instance；**不要 fs、http 插件**（G5）；SMAppService 经 `objc2-service-management` |

与 R2 的约定差异：命令返回 `Result<T, GuiError>`，`GuiError = { code, message, detail }` 与 cli 信封的 `error` 同形，界面按 `code` 与 `detail.reason` 分支（cli.md：不看 message）。

### 2.3 crate 依赖

- `strata-client` → `strata-types`、`serde`、`serde_json`。**不依赖** store、gitio、deps、session、daemon。types.md §8 在 P0 增加一层：`strata-client` 与 `strata-types` 之上、其余 crate 之下，只依赖第 0 层；daemon、cli、desktop 可以依赖它。
- `git-strata-desktop` → `strata-client`、`strata-types`、`tauri` 与插件、`tokio`。不依赖 store / session / overlay / confirm / daemon。
- 本地计划读取（cli 的 `show::push_plan`、`confirm.rs` 的 `planned_moves`）**留在 cli**，不进 strata-client；GUI 用 daemon 的计划方法（§3.4）。等 daemon 有了计划方法后 cli 是否也改用，是 cli 自己的事（§9 Q-cli-plan）。

## 3. 公开接口（Rust）

### 3.1 strata-client 的契约

**纯**：只有线协议、错误映射和数据类型，没有文件系统、没有 store、没有 git。分两步长出来：

**P0（机械抽取，行为零变化）**：把 `crates/git-strata/src/rpc.rs` 原样搬过去，cli 的 `rpc.rs` 改成一行 re-export，cli 其它文件一个字不改（rebase 友好）。

```rust
// crates/strata-client/src/lib.rs —— P0 的全部公开项，与今天 git_strata::rpc 完全相同
pub enum Kind { Read, Write }
pub enum CallError { Down(StrataError), Failed(StrataError) }
impl CallError { pub fn into_error(self) -> StrataError; }
impl From<CallError> for StrataError;
pub fn daemon_down(socket: &Path) -> StrataError;           // Busy, detail.reason = daemon_down
pub fn listening(socket: &Path) -> bool;                    // 只 connect，不发数据
/// 一次连接一个请求；字符串 id 自增并校验；ok → data；StrataError 原样；
/// rpc 层的 UnknownMethod / Failed → InvalidInput，detail.reason = unknown_method / rpc_failed。
/// Write 发出后连接断 → Unknown socket_closed（不重发）；Read → Busy daemon_down。无读超时。
pub fn call(socket: &Path, method: &str, params: Value, kind: Kind) -> Result<Value, CallError>;
```

- `strata_daemon::client`（测试与 daemon 自用，语义不同：写方法 socket 不在 → `Busy daemon_down`，回答丢失 → `Unknown reply_lost`，id 固定为 1）P0 **不动**，避免改 daemon 的测试期望；P2 再决定是否换成 strata-client（§9 Q-daemon-client）。
- 「方法不存在」的判断是 `code == InvalidInput && detail.reason == "unknown_method"`，**不是** `Code::Unsupported`；GUI 的「旧 daemon → 退回轮询 / 功能置灰」按这个判断。

**P2 起（加法）**：

- **回答类型**：GUI 用到的回答结构（`DaemonStatus`、`SessionSummary`、`AllocationInventory`、`PushPlan`、`PublishPlanDetail`、`DiffText`、`ConfigEffective`、`WatchAck`、`Event`）定义在 strata-client，daemon 用同一个类型序列化（serialize 端是 daemon，deserialize 端是 GUI）。新字段一律 `Option` 或带默认值；反序列化忽略未知字段。
- **参数**：参数结构仍在 `strata_daemon::rpc`（`deny_unknown_fields`，types.md 现行规定不动）。strata-client 只提供参数**构造函数**（返回 `Value`），契约测试（§7）在 strata-daemon 的测试里把每个构造函数的输出喂给 daemon 的参数结构解析，保证两边一致。
- **版本**：`daemon_status.api: u32`（缺省视为 0 = 今天的 daemon）。GUI 按 api 开关功能，不靠试错。
- **类型化包装**：`Client { socket }` 上每个方法一个函数，内部就是 `call(..)` + 反序列化。

### 3.2 Tauri 命令（`src-tauri/src/commands/`）

一个 RPC 方法对应一个命令，参数只收标识符与 daemon 给过的计划值，不收「已确认」之类的布尔：

| 文件 | 命令 | RPC | 类别 |
|---|---|---|---|
| health.rs | `daemon_status` | daemon_status | 只读 |
| lifecycle/ | `daemon_start`、`daemon_restart`、`daemon_stop`、`drain_set` | 本机 launchctl / SMAppService + `drain` | 见 §5.9 |
| sessions.rs | `sessions_summary`、`session_status`、`session_changes`、`checkpoints` | status（summary / 单会话）、`changes` | 只读 |
| diff.rs | `diff_text` | `diff_text`（session vs base、session vs session） | 只读 |
| storage.rs | `allocations`、`du`、`gc` | `allocations`、du、gc | 只读 / 普通确认的写（D11） |
| deps.rs | `deps_list`、`deps_approve` | 同名（`planned` 绑定，§5.4） | 写，Touch ID |
| publish.rs | `push_plan`、`push`、`publish_prepare`、`publish`、`chain_resolve`、`resolve_move`、`restore` | 同名；`publish_prepare` = checkpoint + 本地组装 batches + publish_plan detail（§5.4；`assign` 只在 Conflict 对话框） | 写，按 pins 需 Touch ID |
| sessions.rs | `rm` | rm（`discard` 时 Touch ID） | 写 |
| config.rs | `config_effective`、`config_relax`、`config_tighten` | `config_effective`、config_relax；tighten 写 config.toml 后走 §5.9 重启 | 写 |
| onboarding/ | `onboarding_state`、`write_initial_config`、`register_agent`、`unregister_agent`、`link_cli`、`migrate_legacy`、`rollback_legacy`、`capture_login_path`、`open_privacy_pane` | 本机，参数全是固定值或经 dialog 选的路径（§4 G3） | 见 §5.6、§5.7 |
| tray.rs | 无命令；托盘菜单只打开主窗口到对应项 | — | — |

写命令是 `async`，内部 `spawn_blocking(client.call(..))`，开始和结束各发一次 `strata://op`。中间进度来自 daemon 的 watch（`op.*`、`confirm.*`），不是 GUI 猜的。

### 3.3 Tauri 事件

| 事件 | 负载 | 来源 |
|---|---|---|
| `strata://link` | `{state: watching / polling / down, since, last_heartbeat}` | watch.rs / poller.rs |
| `strata://invalidate` | `{queries: [QueryKey], reason: event kind / resync / poll}` | §5.3 的失效表 |
| `strata://progress` | `{op, phase, repo?, session?}`（来自 daemon 的 progress 类事件） | watch.rs |
| `strata://op` | `{op_id, method, phase: started / done / failed, result?}` | 写命令 |

前端照 R2 的规矩：先 `listen` 再 `invoke`，组件卸载时取消。

### 3.4 daemon 需要新增或修改的接口（按期）

daemon.md §5 的方法表与「共 21 个方法」那句、`tests/rpc.rs::daemon_md_method_table_matches_rpc` 每期随实现一起改，同一提交里写 daemon.md 增补。下表是全部改动；不在表里的，GUI 不用。

| 期 | 方法 / 改动 | 只读？ | 内容 | 文档增补 |
|---|---|---|---|---|
| P2 | `status` 加参数 `summary: bool`（缺省 false） | 是 | 只读 store：每会话的 ticket、仓库、每仓库 `changes_at_checkpoint{count, checkpoint, at}`（该会话该仓库**最新检查点**的 overlay 条目数——历史值，不含检查点之后的编辑；界面写 "as of last checkpoint, <time>"，不当作当前改动数）、最后检查点、最后活动时间；**不跑** drift / shadow_drift / open_files | daemon.md §5 status 行 |
| P2 | `changes {session, repo}`（新） | 是 | session **当前工作区**相对其基线的改动路径与类型（与 cli `diff` 今天的算法相同，cmds.rs:592 起，读工作区现状，不写检查点）；回答带 `computed_at`；算不了（工作区不可读、跟踪器降级）→ `fresh: false` + `reason`，界面显示 "unknown"，不显示旧数 | 方法表 +1 |
| P2 | `diff_text {session, repo, path, against: base / {session}, max_bytes}`（新） | 是 | 两侧文本或 `binary / missing / too_large / symlink{target}`，各带版本标识；`path` 走 RepoPath 校验 | 方法表 +1 |
| P2 | `allocations {repo?}`（新） | 是 | 基线代（`base_gens`）、依赖集（`deps_sets`）、工作区引用（`workspaces`）、进行中的依赖构建（`deps_attempts`）、数据根所在卷的 free / total；大小取 du 缓存（带 `as_of`） | 方法表 +1 |
| P2 | `push_plan {repo}`（新） | 是 | daemon 读出的 chain 顶（= 要传回的 `shown`）、要推的提交（oid + 标题）、分支、**解析后的目标 URL**（D12，来自 impl/v1）、pins 是否要确认、`deploy_on_push`、Touch ID 框将显示的文字 | 方法表 +1；confirm.md §5 文案含 URL |
| P2 | `publish_plan` 加参数 `detail: bool` | 是 | 每仓库要发布的提交、chain 顶、pins、目标 URL、已发布跳过项、`waiting_proposals`、Touch ID 文字 | daemon.md §5 publish_plan 行 |
| P2 | `daemon_status` 加字段 | 是 | `version`、`api`、`epoch`、`draining`、`inflight[]`（§5.9）、`pending_prompt`（见下）、`confirm`（仅 `probe: true` 时探测，结果缓存 60 s） | daemon.md §5 daemon_status 行 |
| P2 | `Confirmer::pending() -> Option<PendingPrompt{action_kind, method, reason_text, started_at}>`（新，在 **strata-confirm** 的 confirmer.rs） | — | 今天的门是 `Mutex<()>`，`prompt_in_progress` 靠 `try_lock`（confirmer.rs:146-158），里面没有框的信息。新增 `Mutex<Option<PendingPrompt>>`：拿到门之后写入，用 guard 的 Drop 在**每条**退出路径清空；只读，不影响门 | confirm.md §3 |
| P2 | `config_effective`（新） | 是 | 三份分开给：**daemon 启动时读入并正在执行的**配置与 pins（`Pins.file`，confirm_flow.rs:79-104）、磁盘上现在的 config.toml、待处理的放宽；`revision` = **已读入文本**的哈希（config_relax 用的就是这份，confirm_flow.rs:210-229），另给 `on_disk_differs: bool`，界面写 "Edited since the daemon started" | 方法表 +1 |
| P3 | `deps_list` 回答里每个提议加 `planned_moves[]` | 是 | daemon 用与锁内复核**同一个函数**算出的移动（今天 cli 在 confirm.rs 本地算）；方法数不变 | daemon.md §5 deps_list 行、types.md PlannedMove |
| P3 | `deps_approve` 加参数 `planned: Option<Vec<PlannedMove>>` | 否 | 与 `shown` 同理：daemon 锁内重算 planned moves，不等 → `NotConfirmed plan_changed` | daemon.md §5 deps_approve 行、types.md PlannedMove |
| P3 | `config_relax` 加参数 `revision: Option<String>` | 否 | 绑定人看过的**已读入**内容（不是磁盘上的字节），不等 → `NotConfirmed config_changed` | daemon.md §5 config_relax 行 |
| P3 | push / publish 的 URL 绑定 | 否 | **由 impl/v1 实现**（D12），GUI 只处理 `remote_changed` | impl/v1 自带增补 |
| P4 | `watch`（新） | 是 | §5.3：连接专用模式、单一事件序列、重放与快照协议 | 方法表 +1；daemon.md 新增 §5.x「watch 线协议」；rpc I1 增加 `Busy too_many_watchers` |
| P5 | `drain {on: bool, ttl_s}` / `drain {seal: true, lease_id, then: restart / stop}`（新） | 控制 | §5.9：可续租的拒收新写请求；准入与 inflight 同一临界区；原子封口后 daemon 立即自己退出，退出码由 `then` 决定（restart 75 → launchd 拉起；stop 0 → 留在停止状态）；写 `last_shutdown.json` | 方法表 +1；daemon.md §5.9 停机步骤 |
| P5 | cli `daemon start` | — | 服务已加载但没在跑（stop 封口以 0 退出后的状态）时 `kickstart`（不带 `-k`），不再回 `already: true`（v1:crates/git-strata/src/launch.rs:57 今天只看「已加载」）；已在跑才回 `already: true` | cli.md `daemon start` 行 |
| P5 | plist、退出码与日志 | — | plist 加 `ExitTimeOut 900`，`KeepAlive` 改为 `{SuccessfulExit = false}`；main.rs 退出码按封口模式（restart 75 / stop 与 SIGTERM 0，close 出错不改退出码，记进 `last_shutdown.json`）；daemon 自己把 stderr 重定向到 `~/Library/Logs/git-strata/daemon.log`（10 MiB 轮转一份）；PATH 从旁路文件读（§5.7） | daemon.md §5.9、config.md 新增 daemon-env.toml |
| P6 | label 改名 | — | cli 的 `launch.rs` 一个版本周期内两个 label 都认；新安装不再有默认数据根 | cli.md、config.md |

方法表从 21 个变为 28 个：P2 新增 5 个（changes、diff_text、allocations、push_plan、config_effective）→ 26；P4 加 watch → 27；P5 加 drain → 28。`gc` 不变（D11）。

## 4. 数据与不变量

GUI 不拥有持久业务数据。它写的东西都由人按按钮触发：首次的 `config.toml`（文件不存在时）、迁移时对 `config.toml` 的显式化改写（§5.7，先备份）、「Require confirmation」改写的 `confirm` 字段（§5.4）、旁路文件 `daemon-env.toml`（§5.7）、LaunchAgent 注册、`~/.local/bin/git-strata` 符号链接、前端偏好（WebView localStorage：主题、强调色、侧栏）。

不变量：

- **G1 无绕过**：GUI 进程里没有能让 daemon 认为「已确认」的东西：没有 token、没有缓存的 LAContext、没有「本次已批准」。（confirm I7，design §9 不变量 7。）
- **G2 所见即所批**：确认类调用携带的 `shown`、`plan`、`planned`、`revision` 只能来自刚展示给人的那份 daemon 计划。daemon 锁内复核：chain 顶（`chain_moved`）、发布组（`plan_changed`）、planned moves（`plan_changed`）、配置内容（`config_changed`）、**推送目标 URL（`remote_changed`，D12，impl/v1）**。对话框打开期间收到相关失效事件（§5.3）就作废重取。
- **G3 人手触发（界面规则，不是安全边界）**：确认类命令只能由主窗口里的一次点击发起；不绑全局快捷键、命令面板只能打开对话框、不注册 URL scheme、不响应 AppleScript。这只约束 GUI 自己；同 uid 的 agent 可以直接连 socket，所以安全只靠 daemon 的门（G1、G2）。引导类命令（§3.2 onboarding）只收固定值或经系统对话框选出的路径，不收任意路径或命令。
- **G4 无自动化面**：发布构建里没有 `tauri-plugin-connector`、MCP、WebSocket 端口。
- **G5 不读数据卷**：GUI 进程不打开 `paths.root` 之下和任何仓库 `path` 之下的文件，计划、diff、分配都经 daemon。唯一例外是引导时用系统对话框选目录（只拿路径，不读内容）。
- **G6 不信任仓库内容**：diff、路径、提交信息、ticket、提议理由都是 agent 写的，按纯文本渲染（Monaco 只读、AntD 文本），不渲染 HTML / Markdown，严格 CSP。
- **G7 托盘不批准、不显示 agent 文本**：托盘只显示种类、编号与仓库名（如 "Deps proposal 12 · tool"），不显示 ticket、提交标题、提议理由；点击只打开主窗口到对应对话框；Quit 只退 GUI。
- **G8 bundle 内 cli 与外部 cli 同权**：bundle 里的 `git-strata` 没有额外能力，确认仍由 daemon 弹框。
- **G9 不在有写操作时重启 daemon**：GUI 发起的任何 daemon 停止 / 重启 / 升级 / 迁移都先走 §5.9 的 drain；不用 `launchctl kickstart -k`，不发 SIGKILL。

## 5. 流程与算法

### 5.1 界面与线框

壳照 R2：36 px 标题栏（⌘K）+ 264 / 56 px 侧栏 + 主区（52 px 工具栏 + 内容 + 30 px 状态栏），右侧可选 Inspector。

```
┌ titlebar ─────────────────────────────────────────── ⌘K Search sessions ─┐
├ sidebar ──────────┬ Waiting for you ────────────────────────────────────── ┤
│ Waiting for you 2 │  Deps proposal 12 · tool         [Review]             │
│ Sessions        5 │  Push staff-api (needs Touch ID) [Review]             │
│  ● fix login      │                                                        │
│  ● deps bump      │  Active sessions                                       │
│  ○ spike (idle)   │  fix login   ▮▮▮▮▮  tool 3 files · staff-api clean     │
│ Allocations 41 GB │  deps bump   ▮▮▮▯▯  tool: waiting for deps 12          │
│ Daemon    healthy │                                                        │
│ Settings          │                                                        │
├───────────────────┴───────────────────────────────────────────────────────┤
│ statusbar: Live · daemon 0.4.0 · du as of 11:40                           │
└──────────────────────────────────────────────────────────────────────────┘
```

| 屏 | 内容 | 数据 |
|---|---|---|
| **Home = Waiting for you + Active sessions** | 上半：所有需要人的事（待批依赖提议、需确认的推送 / 发布、`ConfigRelaxPending`、`Quarantine`、`UnknownPublish`、`Conflict` 待选、`prompt_in_progress`）；每项一个 Review 按钮。下半：每个活动会话一行横向小地层条（§8.2），点开进详情。空态："Nothing needs you right now. Agents can keep working." | daemon_status、status summary、deps_list |
| **Daemon** | 运行状态（§5.9 的五种状态分开显示）、版本与 api、`store_unavailable`（附 TCC 引导）、告警（按 DaemonWarning kind 写人话）、进行中的操作、Restart / Stop | daemon_status |
| **Sessions** 列表 / 详情 | 列表：ticket、id、仓库数、改动数、最后活动；详情：左侧地层柱，右侧每仓库状态、改动文件、检查点、restore | status summary、status（单会话）、changes |
| **Diff** | 左文件树，右 Monaco DiffEditor 只读；session vs base、session vs session | changes、diff_text |
| **Publish / push** | §5.4 | push_plan、publish_plan detail |
| **Dependencies** | 提议、影响的会话、planned moves | deps_list、deps_approve |
| **Allocations** | 基线代与依赖集（谁引用、大小、创建时间）、进行中的克隆与依赖构建、卷剩余空间、du 的 logical / allocated / private / 可回收 trash 与 `as_of`；gc 按钮（§5.5） | allocations、du、watch |
| **Settings** | 引导各步状态、daemon PATH、Open at login、迁移与回滚、日志位置 | 本机 |

### 5.2 写作规则

- 界面只用英文（D5），文案集中在 `src/lib/copy.ts`。
- 动作名一致：`Publish` → `Published`；`Approve` → `Approved`；`Push` → `Pushed`；`Discard session` → `Discarded`。
- 错误说发生了什么、怎么修：`Daemon isn't running. Start it to see live sessions.`；`git-stratad can't read /Volumes/…: allow Removable Volumes for it in Privacy & Security.`（授权对象名以 P-pack 实测为准，§8.1）
- 句首大写，不用全大写标签。
- 对话框先告诉人 Touch ID 框会说什么：`macOS will ask for Touch ID. The prompt says: "push staff-api main to git@github.com:…"`。文字取自 daemon 计划里的 `prompt_text`，不是 GUI 自己拼的。弹框的进程名以 P-pack 实测为准。

### 5.3 实时更新：`watch`（D2）

#### 5.3.1 daemon 侧

1. **连接专用模式**：`watch` 只能是一个连接上的**第一个**请求。`serve_connection` 解析出 `watch` 后不再进入请求循环，把连接交给订阅者循环，这个连接之后只出不进（客户端再写数据被忽略，读到 EOF 结束）。同一连接上先发别的请求再发 `watch` → `rpc Failed`。实现上 `serve_connection` 的处理函数返回 `Reply | Stream(Subscription)`，两种分支各自写。
2. **订阅者循环**（就在原来的连接线程里，不另起写线程）：
   - 循环 `recv_timeout(250 ms)` 等「有新 seq」的通知；每轮先查**停机谓词**，为真就写一行 `{"kind":"bye"}`（尽力）后关闭。daemon 停机时作用域线程的 join 因此最多等 250 ms + 一次写超时。**停机谓词必须包含外部 SIGTERM**：今天 SIGTERM 只置 main.rs 的 `TERM`（main.rs:11-14），作为 `external` 传给 `serve`；accept 循环看 `stopping() || external` 退出（startup.rs:447-453），但不置 `self.stop`，而 `request_stop` 只是测试钩子。P4 让 `serve` 在因 `external` 退出 accept 循环时先调 `request_stop()`，再进入 join，订阅者循环只看 `stopping()` 即可（与 reconcile 循环 handlers.rs:356 的组合谓词等价）。store-wait 服务（waiting.rs:240）也调 `serve_connection`，P4 的 `Reply | Stream` 改动同样覆盖它（它不接受 watch，回 `rpc Failed`）。
   - socket 设写超时 2 s。**任何写失败或写超时都直接关闭连接**，不在同一连接上补发 `resync`（半行之后再写会破坏分帧）。客户端带着游标重连，由握手决定补发还是快照。
   - 每 15 s 无事件时写 `{"kind":"heartbeat","seq":<head>}`，供客户端判活。
3. **单一事件序列（hub）**：所有生产者调 `hub.publish(class, kind, key, data)`。hub 内一个分发线程按到达顺序处理：`state` 类先进入按 `(kind,key)` 合并的待发表（100 ms 窗口，最多 4096 个 key，满了立即冲刷）；`progress` 类与 `health` 不合并，但发出前先把待发表冲刷掉，保证总序不乱。冲刷时在 hub 锁内分配 `seq`（epoch 内从 1 单调递增，无空洞）并追加到环形日志。
4. **环形日志与重放**：日志保留最近 4096 条且不超过 4 MiB（单条 ≤ 16 KiB，`data` 只放摘要）。订阅者不持有自己的队列，只持有游标 `next_seq`，直接从日志读——不存在「重放量大于队列」的问题。订阅者的游标落到日志最旧一条之前（慢客户端）→ 关闭连接，客户端重连后会拿到 `resync: true`。
5. **握手**：请求 `{"method":"watch","params":{"v":1,"since":{"epoch":E,"seq":N}|null}}`；回答一行 `{"ok":true,"data":{"v":1,"epoch":E',"head":H,"resync":bool}}`。规则：`since` 为 null、epoch 不同、`N > H`、或 `N+1` 早于日志最旧一条 → `resync: true`，之后从 `H+1` 开始推；否则 `resync: false`，先补发 `N+1..=H`，再接实时。握手在 hub 锁内取 `H` 并登记订阅，保证 `H` 之后的事件一条不漏。
6. **epoch**：daemon 启动时生成 128 位随机数（十六进制）。
7. **事件信封**：`{"v":1,"epoch":E,"seq":n,"class":"state"|"progress"|"health","kind":"…","key":"…","at":"rfc3339","data":{…}}`。事件是「某物变了」加小摘要，全量仍用只读方法取。
8. **订阅者上限** 8 个，超出 → `Busy`，`detail.reason = too_many_watchers`（rpc I1 增补）。只读，不进会话队列，不拿仓库锁。
9. **版本**：`v` 只做加法；客户端忽略未知 kind 与字段。

#### 5.3.2 事件从哪里来（生产者表）

daemon 不拥有领域逻辑（基线、依赖集、克隆写在 session / deps / store 里），所以 daemon 不在这些 crate 里插钩子，而是在**自己能看到的边界**上产生事件：

| kind | class | 产生点 | 何时算「已发生」 |
|---|---|---|---|
| `session.created` / `session.removed` | state | `new` / `rm` 处理函数返回 Ok 之后 | store 事务已提交 |
| `workspace.changed`（key session/repo） | state | daemon 跑 `compute` 处理完跟踪器 inbox 之后（checkpoint、reconcile handlers.rs:212-219、search handlers.rs:265-281），以及 restore / resolve_move 返回之后。**没有** FSEvents 驱动的即时处理（inbox 属于 strata-overlay 的 Tracker，tracker.rs:48-67），所以这是「检查点 / reconcile 驱动」，不是逐次编辑实时；界面的改动数照 §3.4 标时间 | overlay 已更新 |
| `base.created` / `base.removed`、`deps_set.created` / `deps_set.removed`、`deps_build.started` / `deps_build.finished` | state | `deps_build.started` 由 **daemon 处理函数**在调用 strata-deps 构建（handlers.rs:863-871 调 `ensure_target`）**之前**发，含义是「已请求构建」；领域 crate 不知道 hub，不加回调。`deps_build.finished` / `base.created` 在该调用返回后发。其余（`*.removed`、`deps_build.finished`、`deps_set.created`）用**分配差分**：每个写处理函数前后、reconcile 与自动 gc 前后，各读一次 store 的 `base_gens`、`deps_sets`、`deps_attempts` 的 id 与状态，做差——差分只当作「终态清单失效」，不承诺看见中间态 | 行已提交 |
| `clone.started` / `clone.finished` | progress | FSEvents 监视 `<root>/bases` 与 `<root>/deps`（复用 strata-apfs 的 watch 包装，track.rs 同款），看到暂存目录出现 / 改名；不依赖 store | 文件系统事件，仅作进度 |
| `proposal.created` / `proposal.changed` | state | 写处理函数与 reconcile 前后对 open proposals（id、状态）做差 | 行已提交 |
| `op.started` / `op.finished`（method、repo、session、结果 kind） | progress | 每个写处理函数入口 / 出口 | — |
| `publish.finished` / `push.finished` | state | 处理函数返回之后（含部分成功的逐仓库结果摘要） | 已提交 |
| `confirm.prompting` / `confirm.resolved` | progress | confirm_flow 调 `Confirmer::require` 前后（Action 种类、方法、仓库 / 提议；不含任何可复用的东西） | — |
| `du.updated` / `gc.finished` | state | du 扫描结束；gc（手动与自动）结束 | — |
| `health` | health | hub 分发线程每秒比较一次告警集合、`store_unavailable`、`draining`、inflight 数 | — |
| `config.changed` | state | 不产生（配置只在启动时读，§5.4），GUI 靠 `config_effective` | — |

分配差分只读 id 与状态列，每次几毫秒，写处理函数本来就是串行的，开销可接受；progress 类事件同时反映在 `daemon_status.inflight` 里，重连的客户端能从快照恢复「正在进行」的状态。

#### 5.3.3 GUI 侧

- **watch.rs**：一个线程读流。重连退避 0.5 s → 10 s。45 s 收不到任何行（含 heartbeat）判定失联，关掉重连。旧 daemon（`unknown_method`）或 api 不够 → 切到 poller。
- **快照协议**：收到 `resync: true` 时：记下 `H`；对失效表里的「全部」查询发 refetch；期间到达的事件照常处理（它们只会再触发一次 refetch）。因为事件只用于失效、数据都从只读方法取，「快照与事件交错」最多导致多取一次，不会丢更新。游标 = 最后处理的 seq。
- **失效表**（事件 → 前端查询键）：

| 事件 | 失效的查询 |
|---|---|
| `session.*` | sessionsSummary、waiting |
| `workspace.changed` | sessionsSummary、session(id)、changes(id, repo)、打开中的 diffText(id, repo, *) |
| `base.*`、`deps_set.*`、`deps_build.*`、`clone.*` | allocations |
| `proposal.*` | depsList、waiting；打开中的 deps 对话框作废重取 |
| `publish.finished`、`push.finished` | session(id)、pushPlan(repo)、publishPlan(*)、waiting；打开中的发布 / 推送对话框作废重取 |
| `op.*`、`confirm.*` | progress（不走 Query，直接更新 zustand） |
| `du.updated`、`gc.finished` | du、allocations |
| `health` | daemonStatus、waiting |
| `resync` / 重连 | 全部 |

- **轮询（统一表，§5.8 也用这张）**：

| 状态 | daemon_status | status summary | deps_list | du | allocations |
|---|---|---|---|---|---|
| watch 正常，窗口可见 | 30 s（兜底） | 不轮 | 不轮 | 不轮 | 不轮 |
| watch 正常，窗口隐藏 | 60 s | 不轮 | 不轮 | 不轮 | 不轮 |
| 轮询回退，窗口可见 | 2 s | 5 s | 10 s | 60 s | 10 s |
| 轮询回退，窗口隐藏（托盘） | 10 s | 30 s | 30 s | 不轮 | 不轮 |

完整的单会话 `status`（含 drift 等昂贵检查）只在该会话详情可见时取，10 s 一次，外加窗口获得焦点时一次。从托盘打开任何对话框前先强制取一次新数据。

### 5.4 确认类流程

共同规则：计划全部由 daemon 给（G5）；对话框里展示的东西和传回 daemon 的绑定值来自同一份回答；主按钮不设默认回车；等待态显示「Look for the Touch ID prompt」；daemon 返回 `prompt_in_progress` 时显著提示（见 6）。

1. **push**：`push_plan{repo}` → 展示仓库、分支、**目标 URL**（D12）、要推的提交（oid 短码 + 标题，等宽）、`deploy_on_push`、pins 是否要确认、Touch ID 文字 → 人点 `Push` → `push{repo, shown}`。pins 不要求确认时明说 "This repo pushes without confirmation"，并给 "Require confirmation…"（见 7）。
2. **publish**（照 cli confirm.rs 的顺序）：`publish_prepare` = ① 对组内每个 (session, repo) 做 `checkpoint{session, repo, reason: BeforePublish, paths}`，参数取法与 cli confirm.rs 完全相同；② 人在对话框里按路径分批时，GUI 在本地组装草稿 `ReleaseGroup.batches`（检查点 id + 选中的路径，与 cli confirm.rs:537-555 的检查点 → batches 组装同款，数据来自 `changes` 与 ① 的检查点 id），不调任何写方法；**`assign` 不是分批调用**——它给一个**已存在的**发布的冲突路径指定解决会话（rebase.rs:339 起，路径不是该发布的冲突 → `NoSuchPath`），只在 Conflict 对话框里用（cli `publish --assign` 同款，confirm.rs:309-327）；③ `publish_plan{group, detail: true}`。展示每仓库的提交、目标 URL、跳过项、`waiting_proposals` → 人点 `Publish` → `publish{group, shown: {repo → chain 顶}, plan: <计划里的仓库集合>}`。`plan` 必须发，daemon 用它判 `plan_changed`。
3. **deps approve**：`deps_list` 的提议里带 daemon 算的 `planned_moves`（§3.4 P3 行）→ 展示 → `deps_approve{proposal, sessions, planned}`（线上字段名是 `proposal`，rpc.rs:154）。`sessions` 的语义：`null` = 所有受影响的会话，列表 = 只这些；GUI **从不发 `[]`**（`[]` 表示一个也不移，界面上没有这个选项）；全选时发 `null`，部分选择时发列表，一个都没选时按钮置灰。
4. **rm --discard**：展示会话、各仓库**当前**未发布的改动（对每个仓库现调 `changes`；`fresh: false` 时写 "Couldn't read current changes"，仍允许人决定，但不显示数字）→ `rm{session, discard: true}`。
5. **config relax**：`config_effective` 展示**正在执行的**值（放宽作用于它）；磁盘文件与之不同时另起一行 "Edited since the daemon started" 并列出差异 → `config_relax{repo, revision}`（revision 是已读入文本的哈希）。
6. **结果处理**：一律按 `code` 与 `detail.reason` 分支：

| 结果 | 界面 |
|---|---|
| ok | `Pushed` / `Published` / `Approved`… |
| `NotConfirmed` + `cancelled` / `timeout` | `Not pushed`，非错误样式，可再点 |
| `NotConfirmed` + `prompt_in_progress` | 醒目横幅："Another Touch ID prompt is already open: "<pending_prompt.reason_text>" (from <method>, <time>). If you didn't start it, cancel it in the prompt." 数据取 `daemon_status.pending_prompt`；不自动重试 |
| `chain_moved` / `plan_changed` / `config_changed` / `remote_changed` | "This changed since you looked. Review it again."，作废并重取计划；`remote_changed` 额外写明 URL 变了，从哪个变到哪个（来自 detail） |
| `Unsupported`（无 GUI 会话、无密码） | 指向系统设置 |
| `Unknown`（`push_unknown`、`socket_closed`、`interrupted`） | 不重试，标为「结果未知」，引导看 status 与 README |
| 发布部分成功 | 逐仓库一行：Published（提交）/ Pushed / Already published（跳过）/ Refused（reason）/ Unknown / Not started；整体标题按最差的一项写 |
| 其它 reason | 显示 message 与 reason 原文，不猜 |

7. **Require confirmation（收紧）**：config 与 pins 只在 daemon 启动时读（startup.rs、confirm_flow.rs），所以收紧**不是**立即生效。GUI 的做法：改写 config.toml 的该仓库 `confirm` 字段（先备份）→ 提示 "Takes effect after the daemon restarts. Restart now?" → 走 §5.9 的安全重启。重启之前，界面同时显示「configured: confirm」与「enforced: no confirm」，不假装已生效。是否让 daemon 热加载收紧，记为 §9 Q-reload。

### 5.5 gc（D11）

- 不走 Touch ID，不新增 Action。普通 AntD 确认对话框，标题 `Run garbage collection`。
- 文案如实写：
  - "For each repo, removes base generations and dependency sets older than 14 days that no session uses (moved to trash first)."（`gc_repo(GC_KEEP)`，GC_KEEP = 14 天，在仓库锁内）
  - 全局 gc 另加："Then empties trash entries older than 1 hour that nothing points to."（`gc_trash`，TRASH_FLOOR = 1 小时）
  - "The daemon already removes old base generations and dependency sets on its own at startup and every hour. Trash is only emptied when you run this."（自动的只有 `gc_repo`：startup.rs:392-396、reconcile handlers.rs:228-236；`gc_trash` 只在手动全局 gc 里跑，handlers.rs:587-594）。单仓库入口只用第一句的前半，不提 trash。
  - 预计可回收：du 的 `reclaim_min`–`reclaim_max` 与 `as_of`；写明是估计。
- 单仓库 gc（`gc{repo}`）与全局 gc 分两个入口；结果展示 daemon 返回的回收量与逐仓库结果。

### 5.6 首次使用引导（D1）

README 第一节改成「下载 app → 首次打开 → 引导」，cli 用法挪到 "For agents"。每步可检测、可重做，状态由 `onboarding_state` 计算：

0. **Gatekeeper 首次打开**（app 外，写在 README、DMG 背景与发布说明里）：macOS 15 起右键「打开」不能绕过未公证 app：双击 → 被拦 → 系统设置 › 隐私与安全性 › **Open Anyway** → 管理员密码。非管理员账户做不了这一步，README 写明。终端替代：`xattr -dr com.apple.quarantine "/Applications/Git Strata.app"`。
1. **位置检查**：app 若在 DMG 卷里（`/Volumes/…` 且卷是只读映像）或被 App Translocation 运行（路径含 `/AppTranslocation/`），**不允许**注册 daemon、链接 cli，只显示 "Move Git Strata to Applications, then open it again."。记录已注册的 bundle 路径；以后启动发现路径变了，提示重新注册与重建链接。
2. **内部二进制的 quarantine**：Open Anyway 只放行主程序；daemon 与 cli 若仍带 `com.apple.quarantine`，在注册前由 app 从**自己的 bundle** 去掉这个属性（P-pack 验证是否必要、bundle 不可写时怎么办）。
3. **迁移检测**：发现旧安装（§5.7）先走迁移。
4. **配置**：没有 config.toml 时用系统对话框选数据根、添加仓库，按 config.md V1–V3 校验；建议「与第一个仓库同卷的 `<volume>/.strata`」，人确认。根与仓库是否同一 APFS 卷由 daemon 启动时校验，失败的 InvalidInput 原样显示。
5. **daemon 的 PATH**：`capture_login_path` 运行一次 `$SHELL -ilc 'printf "\n__GS_PATH__%s\n" "$PATH"'`：stdin 为 /dev/null、5 s 超时（到时杀掉整个进程组）、输出上限 64 KiB、只取标记后那一行；rc 文件打印的杂讯被忽略。结果给人看、可编辑，写入 `daemon-env.toml`（§5.7）。失败或人选择不设时**不写** daemon-env.toml，由 daemon 用自己编进去的默认值（`install_path_for_home`，lib.rs:52-59，`~/.bun/bin` 在最前，依赖安装靠它，startup.rs:289）；GUI 不另造一份默认 PATH。
6. **注册 daemon**：按 P-pack 选定的方案（§5.7）。SMAppService 返回 `requiresApproval` 时说明并打开系统设置的登录项页。注册后等 socket 出现并 `daemon_status` 成功，才算完成。
7. **权限（TCC）**：数据根在外置卷时，需要「可移除宗卷」授权。授权对象（git-stratad 还是 Git Strata）以 P-pack 实测为准，文案照实写。检测 `store_unavailable`，`open_privacy_pane`。
8. **确认能力**：先发 `daemon_status{}` 取 `api`；`api ≥ 1` 才发 `daemon_status{probe: true}`（`DaemonStatusParams` 是 `deny_unknown_fields`，rpc.rs:246-251，旧 daemon 收到 `probe` 会回 `rpc_failed`）；不可用时说明要设登录密码 / Touch ID。
9. **命令行工具**：`~/.local/bin/git-strata` → bundle 内 cli 的符号链接（免 sudo）。目标已存在且不是我们的链接 → 不覆盖，提示。之后用第 5 步拿到的 PATH 检查 `git-strata` 解析到哪里；若 `~/.cargo/bin` 的旧版排在前面，提示改名或调整顺序（删文件由用户自己做）。
10. **Claude Code 钩子**：给出 `SessionStart` / `PostToolUse` 片段，命令写**绝对路径**（复制时展开为 `/Users/<you>/.local/bin/git-strata`），因为钩子不一定继承交互 shell 的 PATH；只提供 `Copy`，不改 `~/.claude`。

### 5.7 LaunchAgent、升级与迁移（D4、D9、D13）

**两套方案，P-pack 实测后选一套**（§8.1）：

| | A：SMAppService.agent（首选） | B：传统 plist（回退，复用 plist.rs） |
|---|---|---|
| 注册 | bundle 内 `Contents/Library/LaunchAgents/…plist`，`BundleProgram` 相对路径 | app 写 `~/Library/LaunchAgents/io.github.dickwu.git-strata.daemon.plist`，绝对路径 |
| app 被移动 | 跟着走 | 每次启动自检，路径变了就提示重写（需 drain 重启） |
| 状态 | `enabled` / `requiresApproval` / `notFound` | 读 `launchctl print` |
| 风险（待 P-pack） | 自签名能否注册 agent；TCC 与 Touch ID 框归到哪个名字 | 登录项里显示「身份不明开发者」 |
| 最低系统 | macOS 13 | 任意（仍按 13 发布） |

**plist 内容**（两套共同）：`KeepAlive = { SuccessfulExit = false }`（退出码 0 = 有意停止，留在停止状态；非 0 = 崩溃或要求重启，launchd 拉起；§5.9；plist.rs:73-74 现在是无条件 `<true/>`，P5 改，main.rs:2 的注释与新行为一致）、`RunAtLoad`、`ProcessType Interactive`、**`ExitTimeOut 900`**（launchd 默认 20 s 后 SIGKILL，不够发布或推送收尾）。**不写** `StandardErrorPath` / `StandardOutPath` / `EnvironmentVariables PATH`——bundle 里的 plist 是只读且所有用户共用的，放不进每个用户的路径：

- **日志**：daemon 启动时自己打开 `~/Library/Logs/git-strata/daemon.log`（追加，10 MiB 轮转一份），`dup2` 到 stderr / stdout。旧 plist 的 StandardErrorPath 继续有效（daemon 只在 stderr 不是 TTY 且环境变量 `GIT_STRATA_SELF_LOG=1` 时自重定向，新 plist 设这个变量）。
- **PATH**：daemon 启动时读旁路文件 `~/Library/Application Support/git-strata/daemon-env.toml`（`path = "…"`），没有就用编进二进制的默认值。**不**在 config.toml 里加 `[daemon]` 表：Config 是 `deny_unknown_fields`，旧版 daemon 读到新表会拒绝启动，回滚就坏了；旁路文件旧版不读，回滚无害。

**daemon 状态的五种情况分开处理**（Daemon 屏与引导都按这个显示）：未注册 / 已注册待批准（requiresApproval）/ 已注册但被禁用（`launchctl print-disabled` 里为 true）/ 已加载未运行（launchd 退避中，显示最后的退出码与日志位置）/ 运行中（socket 可连、daemon_status 成功）。

**启动是幂等的**：socket 能连就什么都不做；否则按 `launchctl print gui/<uid>/<label>` 判断：服务在域里 → `kickstart`（**不带 `-k`**）；不在域里 → A 方案 `unregister()` 后 `register()`（bootout 之后 SMAppService 仍认为「已注册」，单调 register() 不会把它放回域里——P-pack 第 1 项实测确认），B 方案 `bootstrap`。

**升级**（Tauri updater）：
1. `update.download()` 下载并校验签名（§6），**不安装**。
2. §5.9 的 drain：等进行中的操作结束（显示列表，人可以取消升级）。
3. 停 daemon：按 §5.9 第 4 条「升级 / 迁移」的顺序——封口 `then: stop` → 等进程以 0 退出并**停稳**（无 pid、last exit 0、socket 消失、daemon.lock 可拿）→ `launchctl disable` → 复查停稳 → A：`unregister()`；B：`bootout`。
4. `update.install()`，重启 app。
5. 新 app 首次启动：注册 / 启动新 daemon，核对 `daemon_status.version` 等于自己的版本；不等或 60 s 内起不来 → 显示 "The new daemon didn't start" 与日志位置，提供 `Roll back`（重新安装上一版需要用户手动下载，README 写明；数据不受影响）。

**从旧安装迁移**（`~/.cargo/bin/git-strata{,d}` + `~/Library/LaunchAgents/com.gwd.git-stratad.plist`）：

**范围（peilin 11:32，t51u）**：只覆盖 peilin 这台 Mac 上现有的这一套——用本仓库 impl/v1 编出、`cargo install` 到 `~/.cargo/bin`、由 `com.gwd.git-stratad` LaunchAgent 起的 daemon。**不做通用的旧版兼容**：不支持别的安装位置、别的 label、别的版本区间。开源用户一律是全新安装，走 §5.6 的引导，看不到迁移入口。
1. **检测**（每次启动都做，不只首次）：旧 plist 存在、`launchctl print gui/<uid>/com.gwd.git-stratad` 有记录、`~/.cargo/bin` 下有旧二进制、PATH 里排前面的 `git-strata` 不是 bundle 的。两个 label 同时加载时显示警告（两个 daemon 会争 daemon.lock）。检测到的只要不是下面「身份核对」认得的那一套，迁移入口不出现，只显示 "An older git-strata install was found that this app can't migrate automatically" 与 README 的手动步骤。
2. **显式化配置**：旧 config.toml 没有 `[paths]` 时，旧 daemon 用的是默认根 `/Volumes/GWD/inter/.strata`（`Paths::default_root()`）。新版不再有默认根，所以迁移先把**旧 daemon 实际使用的根**写成显式的 `[paths] root = "…"`（备份为 `config.toml.bak-<时间>`，改写前后给人看 diff）。这一步对旧版也合法，回滚无害。
3. **旧 daemon 的身份核对、准入栅栏与静止核验（不需要改旧二进制）**：旧 daemon 没有 `drain` 与 `inflight`（api 0）。已建立的连接还能随时再发请求（v1:rpc.rs:520-535 按帧循环分派）；更麻烦的是客户端已经断开、处理线程还在跑的请求——旧 daemon 先跑完处理函数，写回答失败才发现客户端没了（v1:rpc.rs:535），这样的 push 可能刚过确认、正要拿 repo 锁（v1:handlers.rs:1184 之后），此刻既没有客户端、没有持锁、没有子进程，也没有确认框。所以核验的对象是**旧 daemon 里还活着的连接线程**，不是客户端。下面行号带 `v1:` 的指 impl/v1 分支（peilin 装的就是它；impl/gui 落后它 18 个提交，连接处理已经不同）。
   ⓪ **身份核对（单一检查，不认得就拒绝）**：label 是 `com.gwd.git-stratad`；`launchctl print` 的 `program` 是 `~/.cargo/bin/git-stratad`（解析符号链接后）；经 socket 读到的 `daemon_status` 没有 `api` 字段，且同时有 `holds`、`repo_waiting`、`warnings`、`verify`、`reconcile` 这几个键（v1:startup.rs:570-578；`reconcile` 由 d9e0a8a、`repo_waiting` 由本仓库的单实例提交引入，都是本仓库 daemon 这一条线上才有的形状）。`version` 在这条线上一直是 `0.1.0`（crates/strata-daemon/Cargo.toml），区分不了构建，所以不按版本号区间匹配，只要求它等于 `0.1.0`。任何一项不符 → 不迁移（见 ③）。
   ① **摘掉 socket 路径**：把 `daemon.sock` 改名为 `daemon.sock.migrating-<随机后缀>`（同目录 rename；目录 0700、socket 0600，v1:startup.rs:584-594，只有同 uid 与 root 能连）。新连接一律连不上（cli 得到 `Busy daemon_down`，不会自动拉起 daemon，rpc.rs:57），这是准入栅栏；改名后的路径只有 GUI 知道。旧 daemon 只在 bind 与退出时碰 socket 路径，服务中不再检查，所以已有连接与 GUI 经新路径的连接照常工作。
   ② **静止核验**（全部满足，**连续两次、间隔 ≥ 2 s**；任一次不满足就从头等）：
      - **连接线程按副本数核验**：`/usr/sbin/lsof -nP -a -p <旧 pid> -U -F pfdn`，把旧 daemon 的 unix socket fd **按 DEVICE（内核 socket 地址，`d` 字段）分组**。同一个连接的几个 fd 是 `try_clone`（dup）出来的，地址相同。旧 daemon 每接受一个连接：一份 clone 放进 `conns`，线程里再有读端与写端两份（v1:startup.rs:498-513；impl/v1 1060b72 之前的构建见 startup.rs:458-468，同样三份）；线程跑完，读写两份随线程释放——1060b72 之后 `conns` 那份也一起删（v1:startup.rs:511-512），之前的构建留着它直到停机（startup.rs:479）。**GUI 自己的连接也算**：先经 socket 读完 `daemon_status` / `status` 并关掉连接，再跑 lsof，所以每次采样时 GUI 没有连接开着。所以规则是：**除监听 socket（`n` 字段是 socket 路径的那一个）以外，每个地址至多 1 份**。有地址 ≥ 2 份 = 还有连接线程活着（无论客户端在不在、请求在哪个阶段，包括已过确认、未拿锁的 push）→ 不通过。
      - **没有客户端连接**：同一次 `lsof -nP -U -F pfdn`（本 uid 所有进程）里，没有别的进程的 socket 对端（`->0x…`）指向旧 daemon 的任何地址；GUI 自己的 `daemon_status` 连接每次用完即关，核验时排除 GUI 自己的 pid。它与上一条互相印证；impl/v1 最多同时服务 128 个连接，满了才让新连接留在 listen backlog（v1:startup.rs:37、491-497），连接数不满时接受循环每 20 ms 轮询一次（v1:startup.rs:515），所以改名前已进 backlog 的连接在 2 s 内一定会被接受、出现在副本计数里。
      - `daemon_status.holds` 为空、`repo_waiting` 全为 0；`status{all:true}` 每个会话的 `in_flight` 为空、`queued` 为 0（v1:handlers.rs:744-745；holds 只列 repo 锁，只拿会话队列的写，例如 `checkpoint`，要看这两项）。
      - 旧 daemon 没有子进程（`git push`、依赖安装等，按 ppid 查）。
      - **reconcile 余量**：reconcile 每 `RECONCILE_EVERY` = 3600 s 一轮（v1:lib.rs:61），下一轮的时刻从上一轮结束时算起（v1:handlers.rs:373-391），`daemon_status.reconcile.last.at` 就是上一轮结束的时间（`runs` 为 0 时用旧 pid 的启动时间，`ps -o lstart`）。要求最后一次采样时离下一轮 ≥ 300 s；不够就等这一轮做完（`runs` 加 1）再从头采样。这样 bootout 落在两轮之间，reconcile 的 `gc_repo` 不会在迁移窗口里开始。
      - 屏幕上没有 git-strata 的 Touch ID 框：对话框仍要求勾选 "No Touch ID prompt from git-strata is open"（副本计数已经覆盖等确认的线程，这一项只是多一道人眼）。
      通过之后，旧 daemon 里没有活着的连接线程、新连接进不来，接下来的 bootout 不会打断 RPC 写操作。
   ③ **核验不了就拒绝**：⓪ 不符、pid 取不到、lsof 不存在 / 退出非 0 / 输出里缺 `d` 或 `n` 字段、找不到监听 socket、有 root 进程可能连着（同 uid 的 lsof 看不到 root 的 fd：lsof 报权限不足即算）→ 不迁移，把 socket 改回原名，提示 "Close agents using git-strata and retry"，并给出 README 的手动迁移步骤。核验不通过（有线程、有客户端、有持锁或排队、有子进程）→ 列出看得到的（客户端 pid 与进程名、holds、in_flight），同一条提示，人可以 `Retry`；不超时强迁，没有「强制」按钮。
   ④ **旧 daemon 在迁移中被拉起**：旧 plist 是无条件 KeepAlive，旧 daemon 若崩溃，launchd 会重启它，新进程在**原路径**重新 bind `daemon.sock`。GUI 在每次采样前、以及 bootout 前一刻检查：pid 变了或 `daemon.sock` 重新出现 → 不继续；删掉已死进程留下的 `daemon.sock.migrating-*`（先确认那个 pid 已不存在），把新的 `daemon.sock` 重新改名，从 ⓪ 重新开始。`daemon.sock.migrating-*` 连不上也按这一条处理，不当作「旧 daemon 已退出、可以继续」。
   ⑤ **残余风险（非 RPC 的后台写）**：静止核验只管 RPC。旧 daemon 还有两种后台写，bootout 时可能正在跑：reconcile 的 `gc_repo`（② 的余量已让它不在窗口里开始）与每 2 s 一次的种子刷新（v1:lib.rs:64、v1:handlers.rs:399-409，`try_session` 拿得到会话队列才做）。两者被中断都不丢已提交的数据：种子刷新先在布局根克隆到 `.strata-tmp-seed-<hex>`，再一次 rename 换进去（v1:seed.rs:379-388），中断只会留下一个暂存文件或一份没刷新的种子（`status` 照旧报 `SeedStale`）；`gc_repo` 只把目录改名进 `store/trash/`（v1:gc.rs 的 `retire`，`RenameMode::Excl`），store.md §6.3 的崩溃表写明改名前后中断目录在原路径或 trash 里都合法（v1:docs/modules/store.md:1466、1662），SQLite 的改动按事务提交，没有正常关闭时下次 `open` 做 WAL 恢复并跑 `quick_check`（store.md:1359-1363）。旧 plist 若是 impl/v1 6381f45 之后生成的，带 `ExitTimeOut 900`；更早生成的没有，SIGTERM 后 launchd **20 s** 强杀——文案按 `launchctl print` 读到的值写；SIGTERM 时后台 pass 会在锁关闭后尽快结束（v1:handlers.rs:365-368）。
   ⑥ 放弃迁移 → `daemon.sock` 不存在时把 socket 改回原名（存在说明旧 daemon 已重启并自己 bind 了，删掉改名的那个即可），一切照旧。
4. **停旧的**：`launchctl bootout gui/<uid>/com.gwd.git-stratad`（等退出，最多 ExitTimeOut）→ `launchctl disable gui/<uid>/com.gwd.git-stratad`（防止下次登录被拉起）→ 旧 plist 移到 `~/Library/Application Support/git-strata/legacy/`（不删）。
5. **起新的**（§5.6 第 5–7 步）。新 daemon 用同一个 `~/Library/Application Support/git-strata/`（config.toml、pins.json、socket 路径不变）、同一个数据根；会话、store、基线原样继续，不做数据迁移。TCC 授权要重新给一次。
6. **回滚**（Settings › `Restore previous daemon`）：按 §5.9 升级流程停新 daemon（封口 `then: stop` → 停稳 → disable → A：unregister；B：bootout）→ 旧 plist 移回 → `launchctl enable` + `bootstrap` 旧 label。config.toml 只多了显式的 `[paths]`，daemon-env.toml 旧版不读，pins.json 未改，所以旧版能直接起来。
7. **label**：cli 的 `launch.rs` 一个版本周期内两个 label 都认；cli 与 daemon 版本不同时 cli 在 stderr 提醒（来自 `daemon_status.version`）。

### 5.8 菜单栏常驻（D6）

- **单实例**：`tauri-plugin-single-instance`；第二次启动只把已有实例的窗口带到前台。
- 关窗（⌘W、红点）→ 隐藏窗口并切 `ActivationPolicy::Accessory`；点 Dock / Finder 里的 app（`RunEvent::Reopen`）或托盘 `Open Git Strata` → 显示窗口并切回 `Regular`。⌘Q / 托盘 `Quit Git Strata` 才退出 GUI；退出 GUI 不影响 daemon。
- 托盘图标：模板图（三道地层横线），状态靠形状：正常 / 有待处理（小圆点）/ daemon 不可达或数据过期（斜杠）。
- **判活**：watch 有 heartbeat（§5.3）；45 s 没有任何行即重连；数据超过 60 s 未更新时托盘状态行写 "Updated 2 min ago"，不显示过期的「healthy」。
- 托盘菜单：状态行（"Daemon running · 5 sessions"）；"Waiting for you" 下最多 5 项，只含种类、编号、仓库名（G7）；"Open Git Strata"；"Pause notifications for 1 hour"；"Quit Git Strata"。点待处理项 → 先强制取新数据，再打开主窗口到对应对话框。
- 窗口隐藏时保持 watch；回退轮询按 §5.3 的统一表（隐藏行）。
- "Open at login"（`SMAppService.mainApp`，默认关）。登录启动时不弹窗口（识别方式由 P-pack 验证；识别不了就提供 "Start hidden" 偏好）。

### 5.9 安全停机、重启与 drain（G9）

daemon 侧（P5）：

1. `drain{on: true, ttl_s}`：daemon 进入 draining：**新的写请求**一律回 `Busy`，`detail.reason = draining`（只读请求与 watch 照常）；已在排队或执行的写请求照常完成。drain 是**租约**：`ttl_s` 默认 120 s、上限 600 s；再发一次 `drain{on: true, ttl_s}` 就从现在起续期，总时长不设上限（依赖安装可达 60 分钟，strata-deps lib.rs:76）。租约到期自动解除，daemon 重启也解除——GUI 崩溃或睡眠后 daemon 不会永远拒写。`drain{on: false}` 立即解除。回答带 `lease_id`，`daemon_status.draining` 给出 `{lease_id, expires_at, sealed}`。**准入原子性**：写请求的「查 draining」与「登记进 inflight」在同一个临界区里做（同一把 inflight 锁）；drain 的开启也在这把锁里，所以不存在「已解析、未登记」而同时躲过 `Busy draining` 与 inflight 列表的请求。
1a. `drain{seal: true, lease_id, then: "restart" | "stop"}`（**封口**）：在同一临界区里原子地核对 `lease_id` 是当前的、未过期、`inflight` 为空；不满足 → `Busy`，`detail.reason = not_idle / lease_lost`，什么都不改。满足 → drain 变为 sealed：**不再过期、不能解除**（`drain{on:false}` 回 `InvalidInput sealed`），回答之后 daemon **立即**走自己的 §5.9 停机路径（等同收到 SIGTERM）。**没有「封口后等别人来停」的窗口**，r3.2 的 60 s 自退兜底删除。
1b. **退出码由封口决定，与收尾是否出错无关**（plist 是 `KeepAlive = { SuccessfulExit = false }`）：`then: restart` → 以 **75**（EX_TEMPFAIL）退出，launchd 拉起新进程；`then: stop` → 以 **0** 退出，launchd 不拉起，服务停在「已加载、未运行」，直到下次登录（RunAtLoad）或幂等启动的 `kickstart`。`core.close()` 出错（main.rs:92 今天是 `result.and(closed)`，会把退出码变成非 0）时：错误写进 err.log 与 `~/Library/Application Support/git-strata/last_shutdown.json`（`{mode, epoch, close_error, at}`），**退出码仍按 `then`**——所以 stop 不会因为收尾出错被拉起一个未封口的进程；restart 本来就要拉起，下一次启动照常走 daemon.md §5.1 的恢复。未封口的 SIGTERM（登出、bootout、人手 `launchctl kill`）同样以 0 退出并记录 close 错误。只有 panic / 被信号杀死 / 启动失败是非 0（launchd 按节流拉起），这是要的行为。main.rs:95-99 的 `ExitCode` 映射在 P5 改。
2. `daemon_status.inflight[]`：每个已接受但未完成的写请求一项：`{method, session?, repo?, phase: queued / confirming / locked / running, started_at}`，覆盖今天 holds / waiters 看不到的阶段（确认框等待中、依赖构建中）。
3. 收到 SIGTERM：照 daemon.md §5.9 停止 accept、在跑的请求做完、**还在排队的以 `ReadOnly shutting_down` 失败**、订阅者收到 `bye`；plist 的 `ExitTimeOut 900` 给足时间。所以 GUI 必须等 inflight（含 queued）为空才发停机。

GUI 侧 `daemon_restart` / `daemon_stop` / 升级 / 迁移 / 收紧配置共用一个流程：

1. `drain{on: true}`；旧 daemon（api 不够，没有 drain）→ 走 §5.7 迁移第 3 条的做法（socket 栅栏 + 核验无客户端连接 + 人确认，核验不了就拒绝）。
2. 显示 inflight 列表，等它变空；等待期间**每 30 s 续租**，一直保持到 daemon 进程退出；人可以 `Cancel`（`drain{on: false}`），也可以等。确认框等待中的项写明 "Waiting for someone to answer a Touch ID prompt"。**没有「强制」按钮。**
3. **封口**：`drain{seal: true, lease_id, then: <restart 时 restart，其余 stop>}`。检查与封口由 daemon 在一个临界区里完成，GUI 侧没有「查完再停」的空档；回 `not_idle` / `lease_lost`（例如电脑睡眠导致租约过期、期间有新写请求进来）→ 回到第 1 步，不停机。
4. 停：
   - restart：封口带 `then: restart`，daemon 自己优雅退出，以 75 退出，launchd 按 `SuccessfulExit = false` 拉起新进程；服务留在域里，GUI **不调 launchctl**。
   - stop：封口带 `then: stop`，daemon 自己以 0 退出，服务停在「已加载、未运行」；GUI **不调 launchctl**（A 仍注册着，下次登录会再起，见下）。
   - 升级 / 迁移：同 stop 封口；等进程退出后，GUI 先确认**停稳**——`launchctl print` 显示没有 pid、`last exit code = 0`（0 退出在 `SuccessfulExit = false` 下不会被拉起；非 0 说明 launchd 会再拉起，等它起来后从第 1 步重来）、`daemon.sock` 已消失、daemon.lock 可拿——再 `launchctl disable gui/<uid>/<label>`（挡住 agent 的 `git-strata daemon start` 在此刻 bootstrap / kickstart，launch.rs:60），再复查一次停稳，然后 A 方案 `unregister()`、B 方案 `bootout`，装好新版后 `enable` 再启动。这些 launchctl 调用都发生在 daemon 已退出、且不会被拉起之后，不在任何时间窗口里；GUI 在其间暂停多久都不会误停一个新进程（复查发现 pid 出现 → 不继续，从第 1 步重来）。`disable` 对 SMAppService 注册的 label 是否生效由 P-pack 第 1 项实测；不生效时 A 方案的残余窗口只剩「复查到 unregister」之间人手 `daemon start`，写进 P-pack 结论。
   封口后 drain 不会过期、进程随即退出，所以停机过程中没有新的 **RPC 写请求**能进来，退出后也没有未封口的同代进程。注意 inflight 只覆盖 RPC：reconcile（handlers.rs:205-238，compute + 锁内 `gc_repo`）不在其中，可能在封口后开始；停机时它作为持锁者照常做完（ExitTimeOut 900 足够），不丢数据。等 socket 消失与 daemon.lock 释放。
5. 起：restart 只需等新进程出现；stop 之后的启动走 §5.7 的幂等启动（服务在域里 → `kickstart`，不带 `-k`）。两者都核对 `daemon_status.epoch` 变了、`version` 符合预期，并读 `last_shutdown.json`：有 `close_error` → 显示 "The daemon reported an error while shutting down" 与日志位置（数据由启动恢复处理）。
6. 任一步失败 → 显示原因与日志位置，不自动重试。

`daemon_stop` 的文案写明 "The daemon will start again at your next login."（agent 仍注册着）；彻底停用是 Settings 里的 `Unregister daemon`。

## 6. 失败与恢复

| 情况 | 表现 | 恢复 |
|---|---|---|
| socket 不在 / 拒绝连接（`CallError::Down`） | 全局横幅 "Daemon isn't running"，数据冻结显示并标灰、标注时间；按 §5.7 的五种状态说明原因 | `Start daemon`（幂等启动） |
| watch 断开 / 45 s 无心跳 | 状态栏从 "Live" 变 "Reconnecting…"，切到轮询 | 自动重连；重连后按握手补发或快照 |
| `store_unavailable`（daemon 在退避重试） | Daemon 屏说明正在重试与原因，附 TCC 引导 | 授权后自动恢复 |
| `Busy draining` | "The daemon is finishing work before a restart." | drain 结束或到期后再试 |
| `Busy too_many_watchers` | 降级为轮询，状态栏写明 | 无需操作 |
| 写调用 `Unknown` | 不重试，标记结果未知 | 照 README "Error codes and what to do" |
| Touch ID 取消 / 超时 / `prompt_in_progress` | §5.4 第 6 条 | 可再点一次 |
| 计划过期（G2，含 `remote_changed`） | 对话框作废并重取 | 重新确认 |
| 部分成功 | 逐仓库结果（§5.4） | 按每行的 reason 处理 |
| 新 daemon 起不来（升级 / 迁移后） | 显示退出码、日志 `~/Library/Logs/git-strata/daemon.log` | §5.7 回滚 |
| GUI 崩溃 | daemon 不受影响；若崩在 drain 期间，`ttl_s` 到期自动解除 | 重开 |

**打包与签名**（D3）：

- **ad-hoc**（`codesign -s -`）：designated requirement 就是 cdhash，每次更新都变，TCC 授权每次更新都丢。只给从源码自己构建的开发者用。
- **稳定的自签名身份（选）**：项目生成一个长期自签名代码签名证书，私钥只放在 GitHub Actions secret，release.yml 用它签三个可执行文件。非 Apple 证书的 DR 形如 `identifier "…" and certificate leaf = H"…"`，identifier 与证书不变时 TCC 授权预期跨更新保留——**这一条与「授权记在哪个名字下」都由 P-pack 实测**，不当作已知事实。代价：Gatekeeper 仍当它是身份不明开发者（§5.6 第 0 步）；证书泄露 = 别人能签出 TCC 认作同一程序的二进制，所以私钥只在 CI；轮换会让所有用户重新授权一次。从源码构建的用户用 `scripts/make-signing-identity.sh` 生成自己的身份。
- **updater**：tauri-plugin-updater 用 minisign 签**下载的更新包**（`.app.tar.gz`），app 内嵌公钥，下载后先验签再安装。`latest.json` 清单本身**不签名**，靠 HTTPS（GitHub Releases）取得；清单里的版本号、说明、下载地址可被能篡改清单的人改，但包的签名对不上就不会安装；updater 只接受比当前更高的版本，不会被引导降级。minisign 私钥只在 CI；丢失私钥 = 已安装的用户只能手动下载新版。minisign 与 codesign 互不相干。
- app 内下载的更新是否还带 quarantine、是否触发 Gatekeeper，由 P-pack 验证。
- **分发**：只走 GitHub Releases（`.dmg` + `latest.json`），照 R2 的 publish.sh / release.yml / ci.yml。
- **GUI 自身不需要 TCC 授权**（G5）。
- **以后有了 Developer ID**：换证书 = DR 变 = 所有用户重新授权一次；公证后第 0 步可删。

## 7. 测试清单

| 测试名 | 断言 | 来源（矩阵行 或 新增） |
| --- | --- | --- |
| `client::call_maps_errors_like_cli` | P0：strata-client 的 `call` 对 ok / StrataError / UnknownMethod / 断连 / id 不符的映射与原 cli rpc.rs 一致 | 新增（cli rpc.rs 今天没有单元测试，映射只在端到端里跑；测试写在 strata-client 里，cli 文件不动） |
| `cli_tests_unchanged_after_extract` | P0：git-strata 全部既有测试不改一行通过 | 新增（验收） |
| `client_builders_parse_as_daemon_params` | 每个参数构造函数的输出能被 daemon 的 `deny_unknown_fields` 参数结构解析 | 新增 |
| `client_replies_ignore_unknown_fields` | 回答类型对多出来的字段不报错，对缺少的必需字段报错 | 新增 |
| `daemon_md_method_table_matches_rpc` | 每期更新：方法表与 rpc 一致（21 → 26 → 27 → 28） | 既有，按期修改 |
| `status_summary_skips_drift` | `status{summary:true}` 不调用 drift / shadow_drift / open_files（test-seams 计数为 0） | 新增 |
| `changes_matches_cli_local_diff` | `changes` 与 cli 本地 session-vs-base 结果相同 | 新增 |
| `diff_text_limits_and_kinds` | 超过 `max_bytes` → too_large；二进制、缺失、符号链接各自的形态；非法路径 → InvalidInput | 新增 |
| `allocations_lists_refs` | 新建两个会话共用一个基线：allocations 显示一个 base_gen 被两个工作区引用 | 新增 |
| `push_plan_includes_dest_url` | push_plan 的 URL 与 shown 和 impl/v1 门内复核用的一致；改 pushurl 后 push → `remote_changed` | 新增（依赖 impl/v1） |
| `publish_plan_detail_lists_commits` | detail 里的提交与 publish 实际发布的一致；不同的 `plan` → `plan_changed` | 新增 |
| `pending_prompt_visible_during_require` | 脚本化 Confirmer 挂起时 daemon_status 显示 pending_prompt；第二个确认 → `prompt_in_progress` | 新增 |
| `deps_approve_planned_mismatch` | `planned` 与锁内重算不同 → `plan_changed`；`sessions: null` 与列表语义不变 | 新增 |
| `config_relax_revision_mismatch` | revision 不同 → `config_changed` | 新增 |
| `watch_first_request_only` | 同一连接先发 status 再发 watch → rpc Failed | 新增 |
| `watch_handshake_replay_contiguous` | since 在日志内：补发 N+1..H 再接实时，seq 无空洞无重复 | 新增 |
| `watch_resync_cases` | since 为 null / epoch 不同 / seq 超前 / 早于日志 → `resync: true` | 新增 |
| `watch_slow_reader_closed_not_blocking` | 不读的订阅者被关闭，写处理函数的耗时不受影响 | 新增 |
| `watch_write_timeout_closes` | 写超时后连接关闭，没有半行之后的数据 | 新增 |
| `watch_shutdown_joins_fast` | 有订阅者时 SIGTERM，daemon 在 1 s + 写超时内退出 | 新增 |
| `watch_stops_on_external_term` | 有订阅者时只置 `external`（模拟 SIGTERM，不调 request_stop），订阅者收到 `bye`，`serve` 在 1 s + 写超时内返回 | 新增 |
| `drain_lease_renew_and_expire` | 续租延长到期；不续则到期解除；`lease_id` 不符时 GUI 流程回到第 1 步 | 新增 |
| `drain_seal_atomic` | 并发：写请求与 `seal` 竞争，要么写被 `Busy draining` 拒、要么 seal 回 `not_idle`，不存在两者都通过；sealed 后 `drain{on:false}` 被拒、不过期；seal 成功后 daemon 立即退出，没有等待窗口 | 新增 |
| `plist_keepalive_successful_exit_false` | 生成与打包的 plist 的 `KeepAlive` 是 `{SuccessfulExit = false}` 字典 | 新增 |
| `seal_exit_code_by_mode` | `then: restart` → 75、`then: stop` → 0、未封口 SIGTERM → 0；用测试缝让 `close()` 出错，退出码不变，`last_shutdown.json` 记下 `close_error` | 新增 |
| `gui_stop_settled_before_launchctl` | 假 launchctl：升级流程只在「无 pid + last exit 0 + socket 消失」后 disable，复查后才 unregister / bootout；GUI 暂停后假 launchd 拉起了新 pid → 不调 bootout，回到第 1 步；restart / stop 不调 launchctl | 新增 |
| `legacy_migration_socket_fence` | 假旧 daemon：socket 改名后新连接失败、已有连接完成；放弃迁移改回原名后恢复 | 新增 |
| `legacy_lsof_copy_count` | 解析器单测，用录下的 `lsof -F pfdn` 样本：监听 socket 被排除；同地址 3 份（线程活着）→ 不通过；1 份（1060b72 之前的残留 clone）与 0 份 → 通过；缺 `d` 或 `n` 字段 → 拒绝 | 新增 |
| `legacy_migration_refuses_with_open_client` | 用 impl/v1 的 daemon 当旧 daemon：① 一个开着不发请求的客户端 → 列出其 pid，不 bootout；② 客户端发一个用测试缝卡在处理函数里的请求后自己退出（模拟「已断开、线程还在」）→ 副本计数不通过，不 bootout；③ 都结束后两次采样为空才继续；lsof 不可用 → 拒绝并改回 socket 名 | 新增 |
| `legacy_identity_gate` | 假 `daemon_status` / 假 `launchctl print`：program 不在 `~/.cargo/bin`、有 `api` 字段、缺 `repo_waiting` 或 `reconcile`、version 不是 `0.1.0`、label 不同 → 都不出现迁移入口、提示手动步骤；本仓库形状 → 出现 | 新增 |
| `legacy_reconcile_headroom` | 假时钟：离下一轮 < 300 s 时等 `runs` 加 1 再采样；≥ 300 s 时继续 | 新增 |
| `cli_daemon_start_kickstarts_stopped` | 假 launchctl：已加载未运行 → `kickstart`（无 `-k`）；在跑 → `already: true`；未加载 → `bootstrap` | 新增 |
| `legacy_migration_revived_daemon_rerename` | 迁移中杀掉假旧 daemon，假 launchd 拉起后在原路径 bind：GUI 发现 pid 变化 / `daemon.sock` 重现，重新改名并从头等待，不 bootout | 新增 |
| `deps_build_started_emitted_during_build` | 脚本化的慢依赖构建：`deps_build.started` 在处理函数返回前到达订阅者 | 新增 |
| `gui_publish_never_calls_assign_when_batching` | GUI 分批只组装 `batches`，不发 `assign`；`assign` 只出现在 Conflict 流程 | 新增 |
| `changes_reports_uncheckpointed_edits` | 检查点之后改一个文件，`changes` 看得到，`changes_at_checkpoint` 不变 | 新增 |
| `daemon_status_probe_only_when_api` | 对 api 0 的假 daemon，GUI 不发 `probe` | 新增 |
| `watch_coalesce_keeps_order` | 同 key 的 state 事件被合并；其后的 progress 事件 seq 更大 | 新增 |
| `watch_alloc_diff_emits_base_created` | `new` 建出新基线后收到 `base.created`，且此时 allocations 已能看到它 | 新增 |
| `watch_too_many_watchers` | 第 9 个订阅者 → `Busy too_many_watchers` | 新增 |
| `drain_refuses_new_writes` | draining 时写请求 → `Busy draining`，只读与 watch 照常；ttl 到期自动解除 | 新增 |
| `inflight_reports_confirming_phase` | 确认框挂起时 inflight 有一项 phase = confirming | 新增 |
| `plist_has_exit_timeout` | 生成与打包的 plist 都有 `ExitTimeOut 900`，打包的那份没有 StandardErrorPath / PATH | 新增 |
| `daemon_self_log_redirect` | `GIT_STRATA_SELF_LOG=1` 时 stderr 写进日志文件 | 新增 |
| `daemon_env_toml_optional` | 没有 daemon-env.toml 用默认 PATH；有就用它；config.toml 不含新表 | 新增 |
| `gui_restart_never_kickstart_k` | lifecycle 的假 launchctl 记录：没有 `-k`，没有 SIGKILL，停机前 inflight 为空 | 新增 |
| `gui_migration_materializes_root` | 没有 `[paths]` 的旧配置被改写为显式 root（值 = 旧默认），且备份存在 | 新增 |
| `gui_migration_disables_old_label` | 迁移序列为 bootout → disable → 移走 plist；回滚序列为移回 → enable → bootstrap | 新增 |
| `gui_onboarding_blocks_translocated` | 路径在 DMG / AppTranslocation 下时 register / link 被拒 | 新增 |
| `gui_login_path_capture_bounded` | 不退出的 shell 在 5 s 被杀；杂讯输出只取标记行；超 64 KiB 截断 | 新增 |
| `gui_deps_approve_never_sends_empty` | 无论怎么选，发出的 sessions 是 null 或非空列表 | 新增 |
| `gui_publish_sends_plan_and_shown` | publish 的 `plan` 与 `shown` 等于对话框展示的计划 | 新增 |
| `gui_dialog_invalidated_on_event` | 对话框打开期间收到相关事件 → 作废重取 | 新增 |
| `gui_partial_publish_rows` | 部分成功按仓库逐行显示，标题取最差一项 | 新增 |
| `gui_tray_no_agent_text` | 托盘菜单项不含 ticket、提交标题、提议理由 | 新增 |
| `gui_single_instance` | 第二次启动只聚焦已有窗口 | 新增 |
| `gui_release_has_no_connector` | 发布构建的 `cargo tree -e features` 没有 tauri-plugin-connector；CSP 非 null；无 URL scheme | 新增（CI） |
| `ui_accent_contrast` | 浅深两套主题里强调色文字对背景 ≥ 4.5:1，按钮白字对强调色 ≥ 4.5:1 | 新增（bun test） |

所有 launchctl、SMAppService、文件写入、系统设置跳转都在 trait 后面，测试用假实现；测试绝不碰真实 LaunchAgent、`/Volumes/GWD/inter` 或正在运行的 daemon。

## 8. 实现任务（TDD）

### 8.1 分期、依赖与工作量（D13、D14）

主线（一人，按顺序）：

| 期 | 内容 | 估计 | 依赖 | 改 daemon |
|---|---|---|---|---|
| P0 | 抽 `strata-client` 传输层（§3.1 P0）；types.md §8 加层；cli rpc.rs 变 re-export | 1 天 | — | 否 |
| P1 | 照 R2 搭 `apps/desktop`（壳、token、主题、⌘K、单实例、托盘）；Daemon 屏、会话列表 / 详情（**只用现有方法**：daemon_status、status 单会话、du、deps_list；会话列表先用低频 `status` 并标注「待 P2 换 summary」） | 4 天 | P0 | 否 |
| P2 | daemon 只读增补（§3.4 P2 行：status summary、changes、diff_text、allocations、push_plan、publish_plan detail、daemon_status 字段、`Confirmer::pending`、config_effective）＋ strata-client 回答类型与契约测试；GUI 接上 Allocations、Diff、Home | 6 天 | P0；push_plan / publish_plan 的 URL 字段依赖 impl/v1 的 URL 绑定已合并 | 是 |
| P3 | 确认类对话框：push、publish（checkpoint + 本地 batches + plan；assign 只用于 Conflict）、deps approve（planned）、rm --discard、config relax（revision）、chain resolve / restore、gc（普通确认）；部分成功与 reason 分支；prompt_in_progress 横幅 | 6 天 | P1、P2；impl/v1 的 `remote_changed` | 是（deps_approve、config_relax 两个参数） |
| P4 | `watch`：hub、连接专用模式、分配差分、FSEvents、失效表、GUI watch.rs 与轮询表 | 5 天 | P2；impl/v1 合并到本分支之后（改动面与 v1 的 handlers / startup 重叠最大） | 是 |
| P5 | drain 与 inflight、封口与退出码、SuccessfulExit plist、ExitTimeOut、自重定向日志、daemon-env.toml；GUI 的 lifecycle（重启 / 停止 / 收紧配置 / 停稳核验）；cli `daemon start` 对已加载未运行的服务 kickstart | 3 天 | P2 | 是 |
| P6 | bundle、注册（按 P-pack 结论选 A / B）、cli 链接、引导、迁移（只针对 peilin 本机的旧安装，含身份核对与 lsof 副本计数）与回滚、通知 | 3 天 | P5、P-pack 结论 | label |
| P7 | release CI、自签名、updater（download → drain → install）、DMG 说明、README 改写 | 2 天 | P6、P-pack | 否 |

主线合计 30 天。

**并行轨道 P-pack**（4–5 天，从 P0 完成起与 P1–P3 交替进行，需要一台干净的 macOS 虚拟机）。产出一页实测记录，P6 之前必须有结论：

1. 自签名（以及 ad-hoc）的 app 能否用 `SMAppService.agent` 注册并启动内嵌的 `git-stratad`；移动 app、重签后是否仍有效。
2. TCC「可移除宗卷」授权记在哪个名字下（git-stratad 还是 Git Strata），授权后更新（同一证书重签）是否保留。
3. LocalAuthentication 框显示的发起者名字（影响 §5.2 文案）。
4. Open Anyway 之后内部二进制是否仍被 quarantine / Gatekeeper 拦；app 自己去掉属性是否可行；app 内 updater 下载的包是否带 quarantine。
5. 登录项启动能否识别（§5.8）。
6. 若 1 失败：方案 B（plist.rs 生成的传统 plist，绝对路径指向 bundle 内 daemon）在同样条件下的 1–4。

合计 30 + 4–5 = **34–35 天，约 7 周**；若 P-pack 一次通过、P4 与 v1 冲突小，约 6 周。每期先写测试再实现，每期一个可审的提交序列；涉及 daemon 的期，daemon.md / types.md / confirm.md 增补与实现同一提交。

**与 impl/v1 的冲突面**：P0 只碰 workspace `Cargo.toml`、`Cargo.lock`、`crates/git-strata/Cargo.toml`、`crates/git-strata/src/rpc.rs`、新目录 `crates/strata-client/`、types.md §8。P2 起会碰 `strata-daemon/src/rpc.rs`（参数）、`handlers.rs`、`startup.rs`、`confirm_flow.rs`、`waiting.rs`、`crates/strata-confirm/src/confirmer.rs`（与 confirm.md）、`strata-publish`（无改动，只读其 batches 组装约定）、cli 的 `confirm.rs` / `show.rs`——这些 impl/v1 正在改（`new` 提速、P0/P1 修复、push URL 绑定），所以 P2 开始前先把 impl/v1 的最新提交 rebase 进来。

### 8.2 视觉方向（frontend-design skill，在 R2 的 AntD 体系内）

**先做 token 计划，再按 brief 复查，再动手**（skill 的两遍法）。

**题材**：git-strata 把每个会话做成叠在基线之上的一层（base → deps → session overlay → 已发布未推送的链 → origin）。辨识度来自**地层柱（strata column）**：会话详情左侧的竖向柱，自下而上画出这几层；Home 的「Active sessions」每行一条横向的小地层条（同样五段）。这是全应用唯一大胆的地方，其余是安静的 AntD 表格、列表与表单。

**层的高度不表示数量**：五层等高（分类图，不是条形图）；每层旁边用文字写量：base「tree 3f2a1c · shared by 3 sessions」、deps「set 12 · 4 dirs」、session「3 files changed」、chain「2 commits not pushed」、origin「main」。某层为空时画成虚线描边的空层并写 "none"，不把高度压成 0。这样颜色和高度都不承担信息，色弱用户与读屏都能读。

**交互与无障碍**：地层柱是一个纵向的 `radiogroup`，每层是一个 `radio`（↑↓ 切换，Enter 选中），`aria-label` 读出层名与量（"Session layer, 3 files changed"）；选中层用 2 px accent 外框 + 文字加粗，不只靠颜色。

**沿用 R2（不改）**：表面与文字 token（light `--bg-app #f6f5f2`、`--bg-panel #fff`、`--bg-sunken #efece6`、`--text #181613`、muted `#6b665e`；dark `#0e0d0c / #17171a / #0a0a0a / #ecebe7`）、`--danger #d4493a`、圆角、壳尺寸、ConfigProvider 算法、accent 多选机制、自托管字体、`App.useApp()`、⌘K、主题 store。

**skill 加上的（与 R2 的差异）**：

| token | light | dark | 用途与对比度 |
|---|---|---|---|
| `--accent` | `#2E6F8E` | `#6FB3D2` | 主按钮、焦点环、选中、链接。light 对 `#f6f5f2` 5.09:1；dark 对 `#0e0d0c` 8.37:1（原 `#2E6F8E` 在深色只有 3.5:1，不达标，r3 改） |
| `--on-accent` | `#FFFFFF` | `#0E0D0C` | 主按钮文字：白对 `#2E6F8E` 5.55:1；深字对 `#6FB3D2` 8.37:1 |
| `--layer-base` | `#B8AE9C` | 同 | 地层：基线 |
| `--layer-deps` | `#8C9A7A` | 同 | 地层：依赖层 |
| `--layer-session` | = accent | = accent | 地层：会话改动，唯一高饱和层 |
| `--layer-chain` | `#C98A3D` | 同 | 地层：已发布未推送 |
| `--mount-live` | `#1f9d55`（R2） | 同 | 会话活动中的小圆点 |

层色只作填充，层上的文字用 `--text`，不压在色块上。R2 的 accent 列表保留，用户可切回橙 `#F38020`。避开 skill 点名的默认套路：不用奶油底 + 衬线 + 赤陶；不用近黑底 + 荧光绿；不用整套同半径同阴影的 SaaS 卡片墙。

**字体角色**：沿用 R2 的三款。Inter Tight 只用于屏标题与会话 ticket（20 / 28 px）；Inter 正文 13 / 14 px；JetBrains Mono 只用于真正的代码内容（diff、oid、路径、URL）。字号阶梯 12 / 13 / 14 / 16 / 20 / 28。

**R2 偏差记录**：R2 的 `.tag` 是 uppercase + 字距；这里用句首大写、不加字距。不用 eyebrow 小标题、不用 `A · B · C` 式标题、按钮不加 `→`。

**动效**：只有一处编排——会话详情首次打开时地层柱自下而上逐层出现（< 400 ms）；其余只有响应动作的过渡。`prefers-reduced-motion` 时全部关闭。

**质量底线**：所有可点元素有可见键盘焦点；颜色之外另有文字或图形区分；浅深两套各自过对比度（`ui_accent_contrast`）；Touch ID 等待态有文字。

**复查（第二遍，对照 brief）**：首页是 "Waiting for you"——brief 的核心是人批准 agent 的动作；地层条放在首页下半，让「谁改了什么」也在首页能看到，而不是只在详情里。

## 9. 构建者决定记录

已定：技术栈照 R2；业务在 Rust；UI 用 frontend-design skill 并与 R2 一致；D1–D14（D10 撤销）。

我的决定（可推翻）：
- strata-client 只做纯传输层与数据类型；P0 是对 cli rpc.rs 的原样搬迁，daemon 的 client.rs 不动。参数结构留在 daemon，strata-client 只给构造函数加契约测试，避免改 types.md 对参数类型归属的规定。
- desktop 不进核心 workspace（`exclude`），自己的 Cargo.lock。
- 事件只用于失效，数据都从只读方法取（§5.3）；分配事件用 daemon 边界上的 store 差分 + FSEvents，不在领域 crate 里插钩子。
- 订阅者不设队列，只有游标；慢了就断开，靠重连的握手补发或快照。
- 配置收紧走「改文件 + 安全重启」，不做热加载。
- PATH 用旁路文件 `daemon-env.toml`，不在 config.toml 里加表。
- daemon 自己写日志到 `~/Library/Logs/git-strata/`。
- drain 带 ttl，同 uid 可调用。
- 错误类型用 `{code,message,detail}`；CSP 不跟 R2 的 null。

剩余问题（给 peilin）：
1. **Q-reload**：收紧确认（`confirm = true`）目前要重启 daemon 才生效。要不要让 daemon 定时重读 config.toml，**只对收紧**立即生效？（放松仍走 config_relax。）
2. **Q-cert**：自签名证书私钥由谁持有（只在 GitHub secret，还是本机也备份一份）？有效期多长（建议 10 年）？minisign 私钥同问。
3. **Q-macos**：最低 macOS 13 可以吗？
4. **Q-cli-link**：cli 链接到 `~/.local/bin`（免 sudo，选）还是 `/usr/local/bin`？
5. **Q-cli-plan**：daemon 有了 push_plan / publish_plan detail 之后，cli 是否也改用（去掉本地计划读取，两边文案同源）？不影响 GUI。
6. **Q-daemon-client**：`strata_daemon::client` 语义与 cli 不同，P2 时是否统一到 strata-client？
7. **Q-drain-agent**：drain 期间 agent 的写请求收到 `Busy draining`。cli 是否要等待重试（像 daemon_down 时那样提示），还是直接报错？
8. **Q-legacy**：迁移只针对你本机这一套（§5.7「范围」），身份核对认不出就不迁移、给手动步骤；认得出时按 lsof 副本计数 + 静止核验自动迁移，核验不了就拒绝并提示关掉 agent。迁移前是否要先把 `~/.cargo/bin` 的旧 daemon 更新到 impl/v1 最新再迁（不是必须：副本规则对 1060b72 前后都成立）？

### 复核对照（gui-merged.md → 本版）

| 复核结论 | 本版处理 |
|---|---|
| P0-1 推送目标未绑定 | D12：impl/v1 实现 URL 绑定与 `remote_changed`；G2、§5.4、§3.4 push_plan |
| P0-2 重启 / 升级 / 迁移会杀写操作；`kickstart -k`；ExitTimeOut 默认 20 s | G9、§5.9 drain + inflight；`ExitTimeOut 900`；启动幂等无 `-k`；五种状态分开 |
| watch 无流式交接、停机阻塞 | §5.3.1 第 1–2 条：连接专用模式、recv_timeout + stopping()、bye |
| seq / 合并 / 重放 / 快照未定义；epoch 易撞 | §5.3.1 第 3–6 条、§5.3.3 快照协议；128 位随机 epoch |
| 写超时后同连接 resync 破坏分帧；1024 > 256 | 写失败即关连接；订阅者只有游标、无队列 |
| 先写后报对基线 / 依赖 / 克隆做不到 | §5.3.2 生产者表：store 差分 + FSEvents |
| 3 s 轮询 status --all 太贵 | `status{summary}`；§5.3.3 统一轮询表 |
| daemon.md 21 个方法冻结 | §3.4 逐期列出与文档增补；测试按期修改 |
| §5.4 用本地 plan::push_plan | 改为 daemon `push_plan`（§5.4 第 1 条），本地计划留在 cli |
| 对话框缺 API；publish 的 checkpoint + assign；发 `plan` | §3.4、§5.4 第 2–3 条 |
| 视图缺数据源（分配、改动计数、session-vs-base diff） | `allocations`、`status summary`、`changes`、`diff_text` |
| strata-client 拆分问题 | §3.1：纯传输层，P0 原样搬迁，daemon client.rs 不动 |
| prompt_in_progress 与 pending 访问器 | `Confirmer::pending()`；§5.4 第 6 条横幅 |
| gc 门 | D11：撤销 Action::Gc，§5.5 如实文案 |
| 「Require confirmation」并非立即生效 | §5.4 第 7 条：改文件 + 安全重启，界面显示两种值；Q-reload |
| 迁移：缺 `[paths]`；`[daemon]` 表破坏回滚 | §5.7 迁移第 2 条显式化 root；daemon-env.toml 旁路文件 |
| bundle plist 放不了 StandardErrorPath / PATH | §5.7：daemon 自重定向日志；PATH 旁路文件 |
| SMAppService + 自签名未验证；TCC 名字 | D13 P-pack 第 1–3、6 条；方案 B 回退 |
| 内部二进制 quarantine | §5.6 第 2 条；P-pack 第 4 条 |
| updater 声称签了 latest.json | §6：只签包，清单靠 HTTPS，只升不降 |
| 多仓库部分成功；按 detail.reason 分支 | §5.4 第 6 条结果表 |
| 引导：DMG / 转移位置；PATH 采集卡住 | §5.6 第 1、5 条 |
| 托盘：轮询不一致、判活、单实例 | §5.3.3 统一表、§5.8 heartbeat、single-instance |
| G3 不是人手证明；限制引导命令 | G3 改写 |
| 分期依赖缺 API；工期偏乐观 | §8.1：依赖列明，34–35 天 |
| P2：deps_approve null vs [] | §5.4 第 3 条 |
| P2：UnknownMethod ≠ Unsupported | §3.1 |
| P2：probe 成本 | `daemon_status{probe:true}`，缓存 60 s |
| P2：深色强调色对比度 | §8.2：深色 `#6FB3D2`，`ui_accent_contrast` |
| P2：层高语义；首页地层视图 | §8.2：等高分类层 + 文字量；Home 小地层条 |
| P2：launchctl disable 旧 label；每次启动检测 | §5.7 迁移第 1、4 条 |
| P2：托盘不显示 agent 文本 | G7 |
| P2：钩子片段绝对路径 | §5.6 第 10 条 |
| P2：design.md「无 GUI」 | §1 末条，同一提交修改 design.md |
| P2：§7 应为具名测试表 | §7 |
| P2：src-tauri 移出 workspace | §2.1 `exclude` |
| P2：§8.2 悬空的「§9 Q3」 | 已删 |

### 复核对照（gui-r3-codex.md / gui-r3-claude.md → r3.1）

| 复核结论 | r3.1 处理 |
|---|---|
| 阻断（Codex B1，r3.1 再复核仍未解决：检查与停机之间有空档；旧 daemon 无准入栅栏） | §5.9：租约 + 准入与 inflight 同一临界区 + daemon 内原子封口（sealed 不过期、失败方向是退出）；r3.3：封口即退出、退出码由 `then` 决定、`SuccessfulExit = false`；§5.7 迁移第 3 条：socket 改名 + lsof 核验无客户端连接 + 等空 + 人确认无 Touch ID 框；§9 Q-legacy；`drain_seal_atomic`、`seal_exit_code_by_mode`、`legacy_migration_socket_fence`、`legacy_migration_refuses_with_open_client` |
| r3.2 再复核（Codex 阻断 1）：旧 daemon 已建立的连接在「查空」之后还能发请求 | §5.7 迁移第 3 条 ②③：socket 层核验无对端（不数 fd，conns 留着 clone），连续两次为空；核验不了就拒绝 "Close agents using git-strata and retry" |
| r3.2 再复核（Codex 阻断 2 / Claude 非阻断 1）：60 s 自退后无条件 KeepAlive 拉起未封口的进程，迟到的 bootout 误停它 | §5.9 第 1a、1b、4 条：删掉 60 s 兜底，封口即退出；`SuccessfulExit = false` + 退出码按模式（restart 75 / stop 0）；升级 / 迁移只在「停稳」后 disable → 复查 → unregister / bootout；`gui_stop_settled_before_launchctl` |
| r3.3 再复核（Codex 阻断）：客户端已断开、处理线程还在跑的请求（rpc.rs:502-506，push 过了确认未拿锁）核验不到 | §5.7 迁移第 3 条 ②：lsof `-F pfdn` 按地址数副本，每个已接受的连接至多 1 份才算没有活着的连接线程；⓪ 身份核对只认本仓库这条线；③ 认不出 / 核验不了就拒绝；⑤ reconcile 余量与后台写的崩溃容忍依据；`legacy_lsof_copy_count`、`legacy_identity_gate`、`legacy_reconcile_headroom` |
| r3.3 再复核（Codex / Claude 非阻断）：stop 后 cli `daemon start` 回 already 不起；lsof 缺 `d` 字段；holds 不含只拿会话队列的写；§5.7 升级第 3 条旧顺序 | §3.4 P5 cli `daemon start` 行与 `cli_daemon_start_kickstarts_stopped`；`-F pfdn`；`status{all:true}` 的 in_flight / queued；§5.7 升级第 3 条改按 §5.9 第 4 条 |
| peilin 11:32（t51u）：只迁本机 | §5.7「范围」：只认 `~/.cargo/bin` + `com.gwd.git-stratad` + 本仓库形状；开源用户全新安装 |
| r3.2 再复核（Claude 非阻断 2）：迁移中旧 daemon 崩溃被拉起 | §5.7 迁移第 3 条 ④：pid 变化 / `daemon.sock` 重现 → 重新改名、从头等；`legacy_migration_revived_daemon_rerename` |
| r3.2 再复核（Claude 非阻断 3）：close 出错时退出码非 0 | §5.9 第 1b 条：退出码按 `then`，close 错误记进 `last_shutdown.json`，GUI 启动后显示 |
| r3.1 再复核（Claude 非阻断 1–5）：KeepAlive 须无条件；§3.2 / §8.1 残留「assign」；deps 事件由谁发；准入原子性；inflight 不含 reconcile | §5.9 第 1b 条与 `plist_keepalive_successful_exit_false`（r3.3 由无条件改为 SuccessfulExit = false）；§3.2 publish 行、§8.1 P3 行；§5.3.2 由 daemon 处理函数在调用前后发；§5.9 第 1 条；§5.9 第 4 条说明 |
| 阻断（Codex B2 / Claude 1）：watch 停机谓词漏了生产 SIGTERM | §5.3.1 第 2 条：`serve` 因 external 退出时先 request_stop；waiting.rs 同样覆盖；`watch_stops_on_external_term` |
| 阻断（Codex B3 / Claude 2）：`assign` 不是分批调用 | §5.4 第 2 条：本地组装 batches，assign 只在 Conflict 对话框；`gui_publish_never_calls_assign_when_batching` |
| 阻断（Codex B4）：摘要计数是历史值 | §3.4：`changes_at_checkpoint` 标明历史；`changes` 读当前工作区并带 fresh；§5.4 第 4 条 rm 用 `changes` |
| Codex N1：deps planned moves 缺 API；字段名 `proposal` | §3.4 P3 `deps_list.planned_moves`；§5.4 第 3 条 `deps_approve{proposal,…}` |
| Codex N2 / Claude 10：分配差分看不到构建中；workspace.changed 不是实时 | §5.3.2：构建路径自己发 started；差分只作终态失效；workspace.changed 写明检查点 / reconcile 驱动 |
| Claude 3：全局 gc 文案不准 | §5.5：自动的只有 gc_repo，trash 只在手动全局 gc 清 |
| Claude 4：bootout 后 kickstart / register 起不来 | §5.7 幂等启动按 launchctl print 分支；§5.9 restart 用封口 `then: restart`（退出码 75）+ `SuccessfulExit = false`，不调 launchctl |
| Claude 5：Confirmer 在 strata-confirm，需要新的 pending 状态 | §3.4 `Confirmer::pending` 行；§8.1 冲突面 |
| Claude 6：revision 应绑定已读入的文本 | §3.4 config_effective / config_relax；§5.4 第 5 条 |
| Claude 7：回退 PATH 与 daemon 默认不同 | §5.6 第 5 条：不写旁路文件，用 daemon 默认 |
| Claude 8：probe 对旧 daemon 会失败 | §5.6 第 8 条：api ≥ 1 才发 probe；`daemon_status_probe_only_when_api` |
| Claude 9：「cli 既有测试整体搬来」不成立 | §7 第一行改写 |
| Claude 11：旧 plist 无 ExitTimeOut，20 s 强杀 | §5.7 迁移第 3 条 |
