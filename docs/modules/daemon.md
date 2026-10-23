# daemon

**Crate**：`crates/strata-daemon`（lib + bin `git-stratad`）· **设计来源**：design.md §5、§6.2、§8、§9、§12 · **矩阵**：test-matrix 里目标模块为 `daemon` 的行，以及 `—` 行里把替代物写成 daemon / launchd / 本机 unix socket 的行，逐条见 §7 · **邻居版本**：session.md @a324f9c，deps.md @dafdefc，gitio.md @1cd5ac5，confirm.md @229f4e3，overlay.md 与 store.md @08ffa53；publish.md 草案 @0fde091（方法名，正文仍薄）；search.md 草案 @aa0414c（只借方法名）

> 本文按 module-rulings.md 写：「T13 daemon」，以及标了 T13 或 Everyone 且约束本模块的增补。后写的、标了 amended 的那条胜。一台机器一个 daemon。方法表是 cli 子命令的对照，名字与下面这张冻结表一致，不改名、不增方法。`Busy` 只用于第一次可见的写之前的等锁（缺省 30 秒）。写之后的等待不受这 30 秒截断。重启报出 `Unknown` 的发布，不重放。不加 `Code`：`Failed` 与 `UnknownMethod` 是 RPC 层的回答。`Code` 停在 39 个。

## 1. 职责与边界

daemon 是本机唯一的写入口。cli 和钩子经 unix socket 叫它。设计来源是 design.md §5（一台机器一个 launchd agent，daemon 不在时 CLI 只读）、§6.2（checkpoint、每小时全量对账）、§8（按 repo 一把写锁，同会话 checkpoint 串行）、§9 不变量 2、3、6、7。

- **做**：持有每 repo 一把写锁和每会话一条队列；整个生命周期只 `Sessions::open` 一次；跑检查点、发布、推送、gc；在图形会话里经 confirm 弹框；重启后报出不确定的发布；fetch 之后前移锚点、在 `main_tip` 动了之后观察主干；把隔离与 attach 越界告警给人。
- **不做**：不自己签发凭据（confirm 签发，daemon 核对 action）。不查 TTY（`require_tty` 是 cli 的）。不在监听回调里 `flush` 或丢弃任何一个 `Watcher`，也不在回调线程里调 `hint`（裁定写 `Tracker::hint`；overlay @08ffa53 的名字是 `Inbox::hint`）。不持有 `Tracker` 的同时去等队列或 repo 锁。不重放、不删除 `Unknown` 的发布。不自动改主 checkout。不在影子仓库上跑 `git gc`、`git worktree prune`，也不在那里 `git commit`（会触发 auto-maintenance）。不把 `Action::ConfigRelax` 定义进本文（Task 15 才加进 types.md）。

cli 只在连不上 socket 时自己打开 `Sessions`（只读）。daemon 在跑时，cli 不打开第二套 `Sessions`：两套进程内记录会把正在跑的移动报成 `MoveAwaitingRecover`（session.md §2）。

## 2. 依赖

依赖方向是 types.md §8：`strata-daemon` 依赖第 0–4 层，外加 `strata-confirm`。cli 依赖本 crate 的 lib，不依赖 `strata-confirm`。本 crate 是 lib + bin；RPC 的方法与参数类型在 lib 里（Addendum 1 的 T13 条）。

| 模块 | daemon 调用的 | 不调用的 |
| --- | --- | --- |
| store @08ffa53 | `Store::open(root, checkouts)`、`open_read_only`（cli 在 daemon 不在时）、`is_poisoned`、`close`、`quick_check`、`open_publishes`、`gc_repo`、`last_gc_report` | 不写 SQL，不把钉住的三个 flag 放进 store |
| session @a324f9c | `Sessions::open` 一次；`new`、`remove`、`ensure_base`、`ensure_base_for`、`move_workspace`、`resolve_failed_move`、`status`、`recover`、`leftovers`、`select`、`resolve` | 调 `new`、`remove`、`ensure_base`、`ensure_base_for`、`resolve_failed_move` 时不持有 repo 写锁（它们自己取，锁不可重入） |
| deps @dafdefc | `ensure`、`build`、`ensure_target`、`observe_main`、`publish_chain`、`main_tip`、`propose`、`plan_rollout`、`record_rollout`、`open_proposals`、`get_proposal`、`open_files`；实现 `RepoWriteLock`，给出 `InstallEnv` | 不在持有队列或 repo 锁时等建造锁 |
| gitio @1cd5ac5 | 每个 repo 一个 `Git::open`（主 checkout）；`fetch`、`read_ref`、`update_ref`、`merge_base`、`is_ancestor`、`push`、`shadow_drift`、`shadow_at` | 不调用 `Git::command`（crate 私有）。不修影子仓库 |
| overlay @08ffa53 | `checkpoint`、`restore`、`compute`；`Tracker::new`、`watch`、`reset`、`flush`、`hint` 经 `Inbox`、`require_full`；`DEFAULT_DIRTY_CAPACITY` | 不在回调里 `flush` |
| publish @0fde091 | `stage`、`check`、`commit`、`push`、`assign`、`rebase_session`、`diff_between`、`chain_ref` | 不调用 `publish::merge_base`（那是发布的合并基 B）。锚点用 `gitio::merge_base` |
| search @aa0414c | `attach`、`detach`、`refresh`、`grep`、`files` | 不加 tgrep 的 CLI 旗标 |
| confirm @229f4e3 | 进程里唯一一个 `Confirmer::new`；`require`、`validate`、`probe` | 不序列化 `ConfirmToken`。`TEST_SEAMS == true` 时拒绝启动 |
| types / config | `Config::parse`、`validate`、`RepoConfig::seed_files`、`needs_human_confirm`；`Action`、`Code`、`StrataError`、`ReleaseGroup`、`CheckpointReason`、`PushOutcome` | 不重定义这些类型 |

本文件点名 daemon 必须照做的几条。每条后面是裁定的标题，后写的胜。

1. 整个生命周期只 `Sessions::open` 一次。cli 只在 daemon 没在跑时自己打开（session §2「对 daemon」第 1 条）。
2. 要队列又要 repo 锁：先按 `SessionId` 升序占队列，再拿锁。持锁时不等队列（「Lock order: session queue before repo lock」）。
3. `ensure` / `build` / `ensure_target` / `ensure_base` / `ensure_base_for` 在占队列之前做完，或者先放开队列和 repo 锁再做。顺序是建造锁 → 会话队列 → repo 写锁。没有任何路径在持有队列位置或 repo 锁时等建造锁（「Lock order, extended」）。
4. fetch、push、链与锚点事务都在 repo 写锁里（「Every update of a ref main_tip reads runs under the repo lock」）。任何 fetch 之后用 `publish_chain` 判定要不要前移锚点（「The anchor follows origin along the chain — amended」）。让 `main_tip` 动了的 fetch 之后还调 `observe_main`。
5. `observe_main` 失败是警告 `ObserveFailed`（session 的 `NewWarning`，`kind` 为变体名），不是失败的 `new` 或 `publish`。daemon 再观察一次（「observe_main is best-effort for its callers」）。
6. `index-read-back-refused` 或 `staging-changed`（连字符，以 gitio 9985e4c 的拼写为准，「Attach alert reasons, spelling — final」）时告警，请人看主 checkout 的 `git status` 与 `git diff --cached`。不自动改主 checkout（「An attach that may have written outside its staging is raised to the human」）。
7. `<shadow>/worktrees` 或 `worktrees/<n>` 不是目录时，session 把它挪进 `<shadow>/strata-quarantine/`。daemon 告警。挪失败则结果是 `Unknown`，仍然告警。不在这种影子仓库上跑 `git gc`、`git worktree prune` 或会触发 auto-maintenance 的 `git commit`（「A symlinked worktrees/<n> is quarantined, never kept」及其 amended 条）。
8. 持锁超过 30 秒就在 `status` 里写出谁持着哪把锁、从什么时候起（「Waits after a call's first visible write are not bounded」）。
9. launchd 没有用户的 PATH。`install` 和 `checks` 用本文写明的 PATH：`/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin` [我的决定 D1]。
10. 确认：daemon 不查 TTY。它核对凭据的 action 与这次请求一致，并且 confirm 说它还没用过、没过期。不符 → `NotConfirmed`。`require_tty` 是 cli 的（「T12 confirm」、confirm.md §5.1）。
11. 钉住的 `deploy_on_push` / `publish.confirm` / `publish.push` 写在配置目录的文件里，不写进 store（Addendum 5 的 T13 条）。调松要 `Action::ConfigRelax { repo: RepoId }`，该变体由 Task 15 加进 types.md，本文不定义。
12. `Store::close(self)` 吃掉 `Store`。daemon 先停掉自己持有的全部 `Arc<Store>` 使用者，再 close。不清洁退出之后的启动才调 `quick_check`；人要的那次走 `daemon_status` 的 `verify`（Addendum 8 的 T13 条，store 第 2 轮）。
13. ToolCall：Edit/Write 的路径经 `Inbox::hint`（裁定的 `Tracker::hint` 就是这个：`Tracker::inbox()` 交出的句柄；overlay @08ffa53 没有 `Tracker::hint`），没有路径清单的工具经 `Inbox::require_full`，然后在请求线程上 `Tracker::flush()`。`hint` 读属性，不在 FSEvents 回调线程上调（「T13 (daemon, from overlay r2)」）。`flush` 返回 `()`。不在回调里 `flush`，也不在回调里丢弃 `Watcher`（「Event flush, amended — corrected by measurement」：回调里对 Watcher 自己的队列 `dispatch_sync` 是 SIGTRAP，exit 133，不是死锁；`FlushSync` 单独在回调里会返回，但 daemon 仍不在回调里调它）。
14. 交换之后，以及 Accept 之后：`Tracker::reset` 再 `watch`。status 列出旧树上的 `holders`（session 的 `OldTreeKept`）。健康为 `Unknown` 的工作区给出 Abandon 与 Accept 两种处理，本文不自动选（「Resolving a failed move」的 T13 部分）。
15. 同一会话的 checkpoint、restore、`rm`、`move_workspace` 串行。进行中的 checkpoint 不取消。`resolve_failed_move` 走同一条队列（session §2 第 3 条，比初稿多这一个）。

`InstallEnv.path` 就是 D1 那条。`InstallEnv.timeout` 用 deps 的缺省 60 分钟，daemon 不另定。`RepoWriteLock` 由本文实现，经 `Ctx` 与 `Sessions::open` 交出去。种子判定是 session 的：`RepoConfig::seed_files()` 返回 `(candidates, explicit)`，daemon 不把候选列表当成显式条目再判一遍。

**上游要求**（邻居还没有的，本文不假装已经有）：

- `deps::RepoWriteLock` 没有 `note_visible_write`。本文用 §5.2 的线程局部规则区分写前与写后的等待 [我的决定 D8]。若将来有调用方在第一次 `with_write` 之前就做了可见的写，deps 应在 trait 上加这个方法，session / publish / overlay 在各自的第一次可见的写处调用。在那之前，本文的规则成立的前提写在 D8。
- gitio @1cd5ac5 没有重钉 `hooksPath` 的函数，并且写明影子仓库的漂移只报告、不修。本文选择只报告，不要求 gitio 加函数 [我的决定 D2]。
- 冻结的方法表没有 `resolve`，也没有「应用调松」的方法。步骤写在 §5.8，入口等方法表放开。这是给 T14 / T15 的缺口，不是悄悄加一个方法。

## 3. 公开接口（Rust）

RPC 是 JSON 行，一行一个请求，一行一个回答。形状只借 tgrep `SHARED_WORKTREE_INDEXES.md` §「Daemon wire contract v1」的「一行一个 JSON」：不用它的 `jsonrpc` 信封、TCP、instance、lease 或认证。socket 是 `~/Library/Application Support/git-strata/daemon.sock`。目录 `0700`，socket `0600`，同 uid 即可，没有认证通道（design §5、§12）。

请求：`{ "id", "method", "params" }`。`id` 是 JSON 字符串或数字，原样回显。缺 `id`、`method` 或 `params`，或 `params` 对不上该方法的字段（多了、少了、类型不对）→ RPC `Failed`，不是 `Code`。一行超过 16 MiB → `TooLarge`（types.md：帧上限由 daemon 返回这个已有的码）[我的决定 D3]。

成功：`{ "id", "ok": true, "data" }`。`data` 总是有；没有结果时是 `{}`。失败且是上游的 `StrataError`：`{ "id", "ok": false, "error": { "code", "message", "detail"? } }`，`code` 是变体名。失败且是 RPC 层：`{ "id", "ok": false, "error": { "rpc", "message" } }`，`rpc` 只取 `UnknownMethod` 或 `Failed`，没有 `code` 字段。客户端靠有没有 `code` 区分，不把这两个词放进 `Code`。

未知方法 → RPC `UnknownMethod`。参数对不上 → RPC `Failed`。

方法与 cli 子命令同名。这张表是冻结的，T14 按它绑定，本文不加第 17 个方法：

| method | 作用 | 是否只读 |
| --- | --- | --- |
| `new` | `session::new` | 否 |
| `status` | 读，不排队，不等写锁 | 是 |
| `checkpoint` | 记检查点 | 否 |
| `restore` | 恢复到一个检查点 | 否 |
| `grep` | `search::grep`；零命中是空列表 | 是 |
| `files` | `search::files` | 是 |
| `diff` | `publish::diff_between` | 是 |
| `deps_propose` | 提议依赖集 | 否 |
| `deps_approve` | 确认后铺开 | 否 |
| `deps_list` | `open_proposals` | 是 |
| `publish` | `stage` 到 `commit`，按配置 `push` | 否 |
| `push` | `publish::push` | 否 |
| `assign` | 点名冲突解决者 | 否 |
| `rm` | `session::remove` | 否 |
| `gc` | `store::gc_repo` | 否 |
| `daemon_status` | 锁、队列、Unknown 发布、可选 `quick_check` | 是 |

`params` 的字段（serde，未知字段 → RPC `Failed`）[我的决定 D4]：

| method | params | data |
| --- | --- | --- |
| `new` | `{ "ticket": string\|null, "repos": [RepoName] }` | `NewReport` |
| `status` | `{ "cwd": string\|null, "session": string\|null, "all": bool }` | 见 §5.4 |
| `checkpoint` | `{ "session", "repo", "reason": CheckpointReason, "paths": [RepoPath] }` | `Checkpointed`；`paths` 可空 |
| `restore` | `{ "session", "repo", "checkpoint": CheckpointId, "paths": [RepoPath]\|null }` | `Restored` |
| `grep` | `{ "pattern", "session": string\|null, "all": bool, "ignore_case": bool }` | `Hit` 的数组；零命中是 `[]` |
| `files` | `{ "session", "repo", "glob" }` | `RepoPath` 的数组 |
| `diff` | `{ "a": SessionId, "b": SessionId, "repo" }` | `BetweenDiff` |
| `deps_propose` | `{ "session", "repo", "switch": bool }` | 提议；`switch` 为真时附带移动结果 |
| `deps_approve` | `{ "proposal": ProposalId, "sessions": [SessionId]\|null }` | 每个会话一条；`null` = 该 repo 的全部会话 |
| `deps_list` | `{ "repo": RepoName\|null }` | `open_proposals` |
| `publish` | `{ "group": ReleaseGroup }` | 每个 repo 的 commit 与 `PushOutcome`；`group.id` 由 store 分配，客户端传来的值不算数 |
| `push` | `{ "repo": RepoName }` | `PushOutcome` |
| `assign` | `{ "group": ReleaseGroup, "path": RepoPath, "session": SessionId }` | `{}` |
| `rm` | `{ "session": SessionId, "discard": bool }` | `RemoveReport` |
| `gc` | `{ "repo": RepoName\|null }` | `GcReport` 的数组 |
| `daemon_status` | `{ "verify": bool }` | 见 §5.4；`verify` 缺省 false |

设计 §6.3 的 `diff --base` 在 publish 草案里没有对应函数。本方法只做 `diff_between`。`--base` 等 publish 补了函数再加参数，不加方法名。

```rust
// crates/strata-daemon/src/lib.rs —— 签名清单（不是实现）
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use strata_confirm::Confirmer;
use strata_deps::{InstallEnv, RepoWriteLock};
use strata_session::{BeforeCheckpoint, Sessions};
use strata_store::Store;
use strata_types::{Code, RepoId, RepoName, SessionId, StrataError};

pub type Result<T> = std::result::Result<T, StrataError>;

/// 写前等锁的上限。只用于这次调用还没写任何对外可见的东西时。
pub const LOCK_WAIT: Duration = Duration::from_secs(30);

/// 一行 JSON 的上限。超过 → Code::TooLarge。
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

/// 全量对账与 gc 的间隔（design §6.2 交给构建者；初稿已定一小时）。
pub const RECONCILE_EVERY: Duration = Duration::from_secs(3600);

/// gc 的保留期（design §7 交给构建者）[我的决定 D5]。
pub const GC_KEEP: Duration = Duration::from_secs(14 * 24 * 3600);

/// D1。install 与 checks 的 PATH。配置以后可以覆盖，缺省用这一条。
pub const DEFAULT_PATH: &str = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin";

/// RPC 层的拒绝。不是 Code。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RpcReject {
    UnknownMethod,
    Failed,
}

/// 钉住的三个值。一个 repo 一行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinnedFlags {
    pub deploy_on_push: bool,
    pub confirm: bool,
    pub push: bool,
}

/// status / daemon_status 里 daemon 自己的告警。会话的告警仍用 session 的 `StatusWarning`，不在这里重定义。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DaemonWarning {
    /// 隔离区里的一条，或还没挪走的链接。
    Quarantine { path: PathBuf },
    /// attach 的 reason 是这两个之一。index 是 `<main>/.git/index`。
    AttachOutside { reason: String, index: PathBuf },
    /// 持锁已超过 LOCK_WAIT。
    LongHold { repo: RepoName, method: String, since: SystemTime },
    /// publishes 里 phase = Unknown 的一行。不重放、不删除。
    UnknownPublish { id: u64 },
    /// 崩溃留下的 Running 行，没有在跑的发布。只报告，不改行 [我的决定 D6]。
    PublishRunning { id: u64 },
    /// 配置文件比钉住的值松，还没经 ConfigRelax 接受。
    ConfigRelaxPending { repo: RepoName },
    /// Store::is_poisoned。
    StorePoisoned,
    /// quick_check 失败，或上一轮 gc 的 partial。
    VerifyFailed { message: String },
    /// 锚点事务比较并交换两次仍没写成。下次 fetch 再试。
    AnchorNotAdvanced { repo: RepoName },
}

/// 谁持着哪把锁。status 在持有超过 LOCK_WAIT 时带上；daemon_status 总是带上。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LockHold {
    pub repo: RepoName,
    pub method: String,
    pub session: Option<SessionId>,
    pub since: SystemTime,
}

pub struct Daemon { /* 私有：Store、Sessions、Confirmer、每 repo 一个 Git、Tracker、钉住文件、告警 */ }

impl Daemon {
    /// 生产构造走 bin 的 main，不从库里直接弹框。测试用 `with_seams`。
    pub fn install_agent(bin: &Path) -> Result<PathBuf>;
    pub fn pinned(&self, repo: &RepoName) -> PinnedFlags;
    /// 启动时不清洁才由 main 调用；人要的那次由 daemon_status verify 调用。
    pub fn verify(&self) -> Result<()>;
}

/// 给 Sessions::open 的 BeforeCheckpoint。不进队列，不等 repo 锁，直接 overlay::checkpoint。
pub struct DaemonBefore { /* 私有 */ }

impl BeforeCheckpoint for DaemonBefore {
    fn checkpoint(
        &self,
        ws: &strata_overlay::WorkspaceRef<'_>,
        base: &strata_overlay::Base,
        reason: strata_types::CheckpointReason,
    ) -> Result<strata_overlay::Checkpointed>;
}

/// 给 deps 的写锁。不可重入。写前等 LOCK_WAIT，写后一直等（§5.2）。
pub struct RepoLocks { /* 私有 */ }

impl RepoWriteLock for RepoLocks {
    fn with_write(
        &self,
        repo: &RepoName,
        f: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Result<()>>;
}
```

`BeforeCheckpoint::checkpoint` 在 `move_workspace` 已经持有队列和 repo 锁时被调用。实现不取这把锁、不排在它后面、不进队列。有 `Tracker` 就传 `Some`，daemon 启动后该工作区第一次扫描之前传 `None`。

launchd 的 plist 不进仓库，由 `install_agent` 写到 `~/Library/LaunchAgents/com.gwd.git-stratad.plist`。`ProgramArguments` 是调用时传来的 `bin` 的绝对路径。`StandardErrorPath` 与 `HOME` 都写成绝对路径（launchd 不展开 `~`）。内容：

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.gwd.git-stratad</string>
  <key>ProgramArguments</key>
  <array>
    <string>BIN</string>
  </array>
  <key>KeepAlive</key>
  <true/>
  <key>RunAtLoad</key>
  <true/>
  <key>LimitLoadToSessionType</key>
  <string>Aqua</string>
  <key>ProcessType</key>
  <string>Interactive</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>
  </dict>
  <key>StandardErrorPath</key>
  <string>HOME/Library/Application Support/git-strata/daemon.err.log</string>
</dict>
</plist>
```

`LimitLoadToSessionType = Aqua` 是 confirm.md §5.2 第 3 步要的：不在图形会话里，LocalAuthentication 弹不出框。`KeepAlive` 为真，空闲不退出（design §5）。`RunAtLoad` 与 `ProcessType = Interactive` 是 [我的决定 D7]：前者让 bootstrap 之后就起来，后者让认证框画得出。装上之后 `launchctl bootstrap gui/$UID <plist>`；卸下是 `bootout` 再删 plist。这是库函数，不是 RPC：daemon 没在跑时 cli 在本进程调它。

`strata_confirm::TEST_SEAMS == true` 时 main 拒绝启动，不听 socket，不弹框（confirm.md C8）。

## 4. 数据与不变量

配置目录 `~/Library/Application Support/git-strata/`（design §7 的裁定，不是 design 里旧的 `strata/`）。本模块拥有的文件：

| 文件 | 内容 | 谁写 |
| --- | --- | --- |
| `daemon.sock` | unix socket，`0600` | 听之前 bind；停机时 unlink |
| `daemon.lock` | `flock` 独占，`O_CLOEXEC` | 第二个进程拿不到就退出，不听 socket |
| `daemon.clean` | 空文件，干净停机的标记 | 停机写上；启动时删掉。没有它 = 不清洁退出 |
| `pins.json` | 每个 `RepoName` 的 `PinnedFlags` | 见下。不在 store 里 |
| `daemon.err.log` | stderr | launchd |

`pins.json` 是 UTF-8 JSON：`{ "repos": { "<RepoName>": { "deploy_on_push": bool, "confirm": bool, "push": bool } } }`。第一次启动没有这个文件：把当时加载的配置写成钉，不算调松。之后：

- 调紧立即生效并写回：`deploy_on_push` false→true，`confirm` false→true，`push` true→false。
- 调松不生效：`deploy_on_push` true→false，`confirm` true→false，`push` false→true。有效值仍是钉住的那份。status 带 `ConfigRelaxPending`。不主动弹框（KeepAlive 重启时弹框，人会顺手点过）。应用调松的步骤在 §5.8，冻结的方法表上没有入口。

不变量：

| 编号 | 内容 | design §9 |
| --- | --- | --- |
| I1 | 一台机器一个进程。锁文件被占时第二个进程以 `Exists` 退出（`detail.reason = "daemon_running"`）。不用 `Busy`：`Busy` 只用于 repo 写锁或会话队列的写前等待 | §5 |
| I2 | 每 repo 一把写锁，不可重入。先到先得。写前等 `LOCK_WAIT`，超时 `Busy`，什么都没写。写后的等待一直等到拿到，不返回 `Busy`（「Waits after a call's first visible write are not bounded」） | §8、不变量 6 |
| I3 | 每会话一条队列，先到先得，不可重入。同一会话的 checkpoint、restore、`rm`、`move_workspace`、`resolve_failed_move` 串行。进行中的不取消，后来的排队，不返回错误码 | §8 |
| I4 | 建造锁 → 会话队列（`SessionId` 升序）→ repo 写锁（一次只拿一个 repo 的）。持有队列或 repo 锁时不等建造锁 | — |
| I5 | status、grep、files、diff、deps_list、daemon_status 不排队，不等写锁 | §8 |
| I6 | publishes 里 `Unknown` 的行，重启后由 `status` 和 `daemon_status` 报出，不重放、不删除、不调用 `git push` | 不变量 6 |
| I7 | 凭据不出进程。RPC 参数里没有能代表「已确认」的字段。action 由 daemon 从请求推出，不采用客户端传来的 | 不变量 7 |
| I8 | 不持 `Tracker` 去等队列或锁。回调线程只调 `Inbox::note` | — |
| I9 | 影子仓库上不跑 `git gc`、`git worktree prune`、`git commit`。`worktrees` 或 `worktrees/<n>` 不是目录时，也不把任何 git 子进程的 git-dir 或 cwd 指到那个影子仓库 | 不变量 2 |
| I10 | 主 checkout 的 index 不被 daemon 改。attach 告警只报告 | 不变量 2 |
| I11 | 请求处理器返回、返回错误或 panic 之后，写锁与队列都已释放（Drop）。锁中毒不把后来的请求卡成 `Busy`：清掉中毒，继续服务 | §8 |
| I12 | 小时对账与 gc 与请求共用同一把 repo 锁和同一条会话队列，不另开一套 | §6.2 |

可见的写沿用 Addendum 3 的定义：工作区、活的 gitfile、别人的 store 行、ref、主 checkout、主对象库。`create_session` 是 `new` 的第一次。`commit` 的 `update_ref` 是 publish 的第一次。restore 的第一次是 `mkdir` 那个暂存目录加 `plan.json`（BeforeRestore 不算，Addendum 5 细化 3）。缓存填充（基线 generation、封存的依赖集、blob）不算。

panic：写前 → 该请求 RPC `Failed`，进程继续。写后 → 对该请求答 `Unknown`，现场留下，status 报出。两种都释放锁。

## 5. 流程与算法

### 5.1 启动

顺序固定。初稿的四步都在，前面加上还没碰 store 的检查。

1. `TEST_SEAMS` 为真 → 拒绝启动。
2. `flock` `daemon.lock`。拿不到 → stdout 一行 `Exists`（I1），退出，不 bind。
3. 删掉 `daemon.clean`（这次若崩在中途，下次算不清洁）。
4. 读 `config.toml`，`Config::parse` 再 `validate`。TOML 失败是 `InvalidInput`，`detail.rule = "toml"`，`message` 键是 toml 的原话。`validate` 报文件顺序里的第一条，daemon 不带着部分配置往下走。
5. 每个 `path` 必须存在且等于它的 realpath（绝对、无尾斜杠、无符号链接分量）。否则 `InvalidInput`，detail 里写出 realpath。通过之后路径原样往下传。
6. 读 `pins.json`，按 §4 调紧或记下待定的调松。
7. `Store::open(paths.store(), checkouts)`，`checkouts` 是配置里的主 checkout。同卷检查是 store 的。
8. 没有过 `daemon.clean`（第 3 步已删；第一次安装也没有）→ `quick_check`。失败 → 记 `VerifyFailed`，仍然继续：人要看得见，而不是静默不听。
9. 每个 repo `Git::open(主 checkout)`。`repo_id` 撞车 → `NameTaken`，不听 socket。
10. `Sessions::open` 一次。之后不再开。
11. `session::recover`。然后才 gc：每个 repo 依次，持该 repo 的写锁调 `gc_repo(repo, GC_KEEP)`，再放开。gc 失败记进告警，不因此不听 socket。
12. `open_publishes`：`Unknown` 记进内存。`PublishPhase::Running` 且没有本次启动之后的发布在跑 → 记 `PublishRunning`，不改行、不重放 [我的决定 D6]。
13. 隔离区非空，或 `worktrees` 不是目录 → `Quarantine` 告警，stderr 一行。
14. 然后才 bind socket、听。
15. 对已有的、tracked 的工作区 `Tracker::new(root, DEFAULT_DIRTY_CAPACITY)` 再 `watch`。

第 11 步之前不接受请求。小时计时从开始听算起。

### 5.2 等锁

每个 RPC 一个线程局部 `visible: bool`，初值 false。

- 该线程上第一次 `with_write`，以及第一次占队列，在 `visible` 为 false 时最多等 `LOCK_WAIT`。超时 → `Busy`，回调没跑，队列没占上。
- 同一 RPC 里之后的 `with_write` 和占队列，或者 `visible` 已是 true，一直等到拿到。status 在持有超过 30 秒时写 `LongHold`。
- daemon 在调用会写的函数之前把 `visible` 设上的时机：`session::new` 返回之后（它内部的第一次 `with_write` 在 `create_session` 之前，之后的由「第二次 with_write」这条盖住）；`publish::commit` 返回成功之后；restore 由 overlay 自己写，daemon 在调用 `restore` 之前已经占上队列，调用期间不再等锁。

前提 [我的决定 D8]：一次 RPC 的第一次 `with_write` 发生在这次 RPC 的第一次可见的写之前。`new` 的 fetch 在 `create_session` 之前取锁，符合。`move_workspace` 由调用方已经持锁再调，它自己不再 `with_write`，符合。若邻居打破这个前提，走 §2 的上游要求，不在这里猜。

队列与锁都是 Drop 释放。panic 也释放（I11）。先到先得：等待队列按到达顺序，不是按线程号。

`publishing` 旗标（按会话）不是锁。`rm` 看见它就等，自己不持有队列、repo 锁或建造锁。checkpoint 在 §5.6 说的缺口里不等它。

### 5.3 请求

1. 读一行。对不上方法 → `UnknownMethod`。参数坏 → `Failed`。超帧 → `TooLarge`。
2. 只读方法直接跑，不占队列，不等写锁。写锁被占住时，`status` 在 1 秒内返回。
3. 写方法先做不需要确认就能下的具名拒绝（会话没有、ticket 有歧义、提议不是 Pending）。注定失败的不弹框（confirm.md §5.1 第 2 步）。
4. 要确认的：cli 应已做 `require_tty`。daemon 不查 TTY，也不相信参数里的任何「已确认」。推出 `Action`，在阻塞线程上 `require` 再立刻 `validate`。此时不持有队列或 repo 锁。框开着时第二个 `require` 由 confirm 以 `NotConfirmed`（`prompt_in_progress`）拒绝。核销之后这次确认就用掉了：后面 `Busy`、`OpenFiles`、被取代，人都要重新发命令、重新认证。
5. 要建造的，在占队列之前做完（§5.6 的缺口是先放开再建造）。
6. 占队列（要几个会话就按 `SessionId` 升序全占上，再往下），再拿 repo 锁。持锁时不等队列。
7. 拿到锁之后按锁里重新解析出的请求再推一次 `Action`，与 `token.action` 比。不同 → `NotConfirmed`，什么都不做（confirm.md D2）。`remove` 与 `new` 自己取锁，daemon 不持锁去调它们，所以这两条的 D2 在调用前做一次，锁内的 action 核对由 `remove` 自己做（它按值收凭据）。选择器在 `remove` 取锁之后又变的窗口，本文不另开锁去堵：堵了就与 `remove` 的取锁重入 [我的决定 D9]。
8. 调上游。回答一行 JSON。连接已断：不重放。结果留在 store 里，status 看得到。

`deps_approve` 与 `push`，以及 `rm` 且 `discard`：都走第 4 步。初稿点了前两个；`rm --discard` 是 types.md 的第三种 `Action`，同一条顺序。

### 5.4 status 与 daemon_status

`status` 不排队，不等写锁。读到的可以与持锁者交错，这是裁定要的，不是漏了锁。

组装：`session::status`（或 `all`）的 `SessionStatus`，加上 `overlay::compute` 在读时算出的 `changed`（session §2 第 5 条，daemon 照做）、`Tracker::warnings`、`leftovers`、`deps::attempts`、`open_proposals`、每个 repo 的 `publish_chain`（Rewound / Diverged 时带上 a..c）。健康 `Unknown` 的工作区带上两个选择：`Abandon` 与 `Accept`，不执行。`OldTreeKept.holders` 原样带上。restore 暂存目录非空时，session 的 `RestoreScene` 已经含 `plan.json` 的路径，不另造。

再加上 §3 的 `DaemonWarning`：隔离、attach、超过 30 秒的 `LongHold`、`UnknownPublish`、`PublishRunning`、`ConfigRelaxPending`、`StorePoisoned`。持锁不到 30 秒的不进 `status`（进 `daemon_status`）。

`ObserveFailed` 不在 `DaemonWarning` 里重定义。它是 `NewWarning` 或这次 RPC 的 `data.warnings`。

`daemon_status` 同样不等锁。总是列出当前持锁者与队列里在等的人。`verify: true` 时调 `Store::quick_check`，这就是裁定要本文点名的那次校验命令：冻结表里没有 `verify` 方法，所以它是这个已有方法的参数 [我的决定 D10]。

stderr 只为给人看的告警打一行：隔离、attach 越界、第一次看见某个影子仓库的 `PrePushGuardDisabled`。同一条不重复打。JSON 里都有，不靠人去翻日志。

### 5.5 各方法

**`new`**：不持锁地调 `Sessions::new`。它自己 fetch、取锁、`ensure`、`observe_main`。返回的 `warnings` 里有 `ObserveFailed` 时，daemon 对该 repo 再 `observe_main` 一次。再失败仍是警告，`new` 已经成功的不改成失败。然后 §5.7。

**`checkpoint`**：先等队列，等到了再借 `Tracker`。等的时候不持有 `Tracker`。同一会话已有 checkpoint 在跑：排队，不取消，两个都做完，顺序与到达一致。同一 `ToolCall.id` 已经记过：返回那一条，不再写。路径非空 → `Inbox::hint` 再 `flush` 再 `checkpoint(Some(tracker))`。路径空且 reason 是 `ToolCall` → `require_full` 再 `flush` 再 checkpoint。`Manual` 与 `Before*` 用 Tracker 的冷扫（有 Tracker 时），不依赖事件。不拿 repo 写锁。钩子等这次 checkpoint 做完再回答，不先应答 [我的决定 D11]：先应答的话，下一次调用的早期写入会混进这一次。代价是没有路径清单的 ToolCall 在 tool 的树上大约 0.20 秒，超出 design §8 的 200 ms；这是给阶段结束时告诉用户的，不是 daemon 能靠少扫修掉的。

**`restore`**：占该会话的队列，不拿 repo 写锁（Addendum 5）。调 `overlay::restore`。它自己记 `BeforeRestore`。

**`grep` / `files`**：不排队。视图没有就 `search::attach`（缓存填充）。零命中是空列表，不是 `NoMatch`。未知视图 → `NoSuchView`。

**`diff`**：不排队。`publish::diff_between`。

**`deps_list`**：不排队。`open_proposals`。

**`deps_propose`**：锁外 `drift`，没有漂移不必提议。有则锁外 `build`。然后占 repo 锁（写前 30 秒）调 `propose`。`switch` 为真：先锁外 `ensure_base_for`，再占该会话队列、拿锁、`plan_switch` 与 `move_workspace`。建造都在占队列之前。

**`deps_approve`**：按 deps.md §5.12。不是 Pending → `NoSuchProposal`，不弹框。弹框在占队列之前，action 是 `DepsApprove { proposal }`。锁外 `ensure_target`；`Target::Replaced` 则什么都不移动，把替换提议的 id 交人，批准它是一次新的确认。Ready 时锁外备好基线 generation。然后按 `SessionId` 升序占齐候选会话的队列，再拿 repo 锁。锁里再 `get_proposal`：已被取代 → `NoSuchProposal`，`detail.state` 指向替换的那条，不自动批准。`P.head` 不是 `main_tip` → `HeadMoved`（reason `main_moved`），什么都没写，放开之后 `observe_main`，把现状交人重新确认。然后 `plan_rollout`，对每个 Move 调 `move_workspace`（调用方已持锁），再 `record_rollout`。某个会话 `OpenFiles` 是 data 里的一条，不是整次失败。冷装在队列和锁之外，所以 ToolCall 不等这次冷装。移动阶段持着队列，ToolCall 等移动做完，不等待被取消。

**`publish`**：见 §5.6。

**`push`**：按钉住的值要确认才弹框，action 是 `PushDeploy { repo, branch }`。弹框前读链顶，记在这次请求里。不占会话队列（不碰工作区）。拿 repo 锁。锁里再对 action，并核对链顶没变；变了 → `NotConfirmed`，不推，人重新确认（confirm.md §5.1 末尾交给本文的缺口）。然后 `publish::push`。`PushOutcome::Pushed` 之后锚点事务里 a := 推上去的 commit（仍在这把锁里）。放开之后 §5.7。

**`assign`**：拿该 repo 的写锁，调 `publish::assign`。不占队列，不确认，不推送。

**`rm`**：`discard` 为真则先弹框，action `RemoveDiscard { session }`，不持队列和锁。然后占该会话的队列。不持 repo 锁地调 `session::remove`（它自己取锁）。`publishing` 为真时先等旗标，不等的时候不持别的锁。进行中的 checkpoint 不取消：队列让 `rm` 排在它后面。

**`gc`**：一个 repo 一把锁，调用方持有，调 `gc_repo(repo, GC_KEEP)`。`repo` 为 null 则按 `RepoName` 升序逐个。写前等锁；超时跳过这个 repo，记进 `daemon_status`，不让整次失败。不占会话队列，不等建造锁。

### 5.6 publish 的锁

publish 草案把「占队列再拿锁」写在一步里。建造锁的顺序不允许在持有队列或 repo 锁时 `ensure` / `build`。本文把一次 `publish` 拆开，两头都满足：

1. 要确认的 repo，按钉住的值，每个 `PushDeploy` 各弹一次。不持锁。记下每个链顶。
2. 若已经知道 rebase 要的依赖集，锁外 `ensure`。通常还不知道。
3. 按 `SessionId` 升序占本组会话的队列。设 `publishing`。拿涉及的 repo 的写锁，一次一个 repo，做完一个放开再拿下一个 [我的决定 D15]：不同时持两把 repo 锁。写前 30 秒，超时 `Busy`，放开已占的队列。
4. 锁里再对 action 与链顶。然后 fetch、§5.7 的锚点事务、`stage`、`check`、`commit`、按配置 `push`。检查可以跑几分钟，这把锁就持有这么久：裁定把「publish 在跑检查」当作健康的长持有者，status 在 30 秒后点名。`commit` 成功之后 `visible = true`，之后的等锁一直等。
5. 放开 repo 锁和队列。`publishing` 仍为真，所以 `rm` 仍等。checkpoint 在下一步的建造期间不等 `publishing` [我的决定 D14]：钩子不能等一次冷装；建造锁的顺序又不许持着队列去等建造锁。commit 已经落地的不会被这次 checkpoint 取消。
6. rebase 要的 `ensure_base_for` / `ensure` 在锁外做。做完再占该会话队列、拿锁、`move_workspace`，然后放开。`move_workspace` 返回后 `Tracker::reset` 再 `watch`。
7. 清 `publishing`。`observe_main`（§5.7）。失败只报 `ObserveFailed`，发布照常成功。

同一会话的 checkpoint 或 `rm` 在第 3–4 步到达：排队，不取消进行中的 publish，不返回错误码。在第 5 步的建造缺口里，checkpoint 可以跑完，然后第 6 步的 `move_workspace` 等它；`rm` 仍等 `publishing`，在第 7 步之后才跑。这是「T10 publish / T13 daemon (from 3d)」那条的落实，建造缺口是 D14 写明的收窄。

### 5.7 fetch 之后

仍在刚才那把 repo 锁里，或为了这件事再拿一次（写后则一直等）：

1. `ch = publish_chain(git, branch)`。`None`（还没 fetch 到）→ 不动锚点。
2. `ChainState::Absent` 或 `ChainState::Rewound` → 不动锚点。
3. `ChainState::Equal`、`ChainState::Leads`、`ChainState::Behind` 或 `ChainState::Diverged`：`m = git.merge_base(&ch.origin, &ch.top)`（gitio 的，不是 `publish::merge_base`）。`m` 是旧锚点的后代且不等于旧锚点时，`git.update_ref("refs/strata/anchor/<branch>", m, Some(旧锚点))`。比较并交换失败就再读一次：已经等于 `m` 算成功；否则再试一次；仍失败 → `AnchorNotAdvanced`，不让这次 `new` 或 `publish` 失败。
4. 放开锁。若 `main_tip` 与 fetch 前不同，调 `observe_main`（它自己取锁）。失败再调一次。仍失败 → 警告 `ObserveFailed`，调用本身按它已经完成的结果返回。

`new` 内部已经观察过的，daemon 只在看到 `ObserveFailed` 时做第 4 步的再试。两边都做是幂等的：head 已经不是 tip 的观察什么都不写。

### 5.8 没有进冻结表的两步

**处理失败的移动**。status 给出 `MoveResolution::Abandon` 与 `MoveResolution::Accept`。真正调用是 `resolve_failed_move`，走该会话的队列，daemon 不持 repo 锁（它自己取）。Accept 之后 `Tracker::reset` 再 `watch`。不自动选。冻结表没有这个方法，cli 的显式命令等表放开再绑。步骤先写在这里，避免表放开之后再猜锁。

**应用调松**。人要的是 `Action::ConfigRelax { repo: RepoId }`（Task 15 才进 types.md；confirm.md §5.6 已有框上的文字）。顺序与 §5.3 第 4 步相同：不持锁地 `require` + `validate`，核对 action 就是这个 repo，然后把该 repo 的钉改成文件里的值并写 `pins.json`。在变体进 types.md 之前，这条路径编译不了，daemon 保持拒绝调松。冻结表没有方法可挂。不提供绕过。

### 5.9 小时对账与停机

每 `RECONCILE_EVERY`：每个工作区试着占该会话的队列（不等；占不上就跳过，记进 `daemon_status`）。占上之后借 `Tracker` 做一次不依赖事件的冷扫（`unchanged_since` 作保，不是把每个文件再哈希一遍），更新缓存，不写 checkpoint 行——`CheckpointReason` 没有「小时」这一项，不发明 [我的决定 D16]。放开 Tracker，放开队列。然后 gc，规则同 `gc` 方法。与请求共用同一把锁和同一条队列。

停机（SIGTERM）：停止 accept。在跑的请求做完。还在排队的以 `ReadOnly` 失败，`detail.reason = "shutting_down"`（types.md 没有 Closed 这个码，映射到已有的「写被拒绝」）[我的决定 D17]。在请求线程上丢掉 Tracker（从而丢掉 Watcher），不在回调里丢。放开全部 `Arc<Store>` 之后 `Store::close`。写 `daemon.clean`。unlink socket。放开 flock。退出。

### 5.10 告警与影子仓库

`move_workspace` 或 attach 返回的 `Unknown`，`detail.cause` 的 `reason` 是 `index-read-back-refused` 或 `staging-changed`：`AttachOutside`，stderr 一行，点名 `<main>/.git/index`，请人在那里看 `git status` 与 `git diff --cached`。不自动改，不 `git checkout`，不复位 index。

隔离区有条目，或挪隔离失败（链接可能还在）：`Quarantine`，stderr 一行。该影子仓库不作为任何 git 子进程的 git-dir 或 cwd，直到 session 把它清掉。

`shadow_drift` 的 `PrePushGuardDisabled`（reason 字符串 `pre_push_guard_disabled`）只报告：session 的 `MirrorDrift` 已经带上，daemon 再在第一次看见时打一行 stderr。不重钉，不重建影子仓库 [我的决定 D2]。重钉要往活的影子仓库写 config，gitio 没有这个函数，而且 husky / lefthook 会把它再改回去。include、`config.worktree`、`extensions.*`、gitfile、被换掉的 hooks 目录同样只报告。

旧树上的进程只列在 status 里，不杀（session 的已知限制）。

### 5.11 复杂度

design §8 的数字落在 overlay 与 session 上。daemon 加的约束：checkpoint 不拿 repo 写锁，所以不被同 repo 的 publish 检查拖进那 200 ms；无争用时分发加占队列应小于一次 `status` 的读。publish 持锁跑检查是裁定接受的长持有，不是 daemon 要压进 200 ms 的路径。新建会话的 5 秒目标是 session 的；daemon 不在 `new` 外面再串一次冷装。

## 6. 失败与恢复

| 情形 | 回答 | 写了什么 | 之后 |
| --- | --- | --- | --- |
| 未知方法 | RPC `UnknownMethod` | 无 | 调用方改方法名 |
| 参数坏、缺 id、未知字段 | RPC `Failed` | 无 | — |
| 一行超过 16 MiB | `TooLarge` | 无 | 缩小请求 |
| 第二个进程 | `Exists`（reason `daemon_running`），进程退出 | 无 | 用已在跑的那个 |
| 写前等锁或等队列超时 | `Busy` | 无 | 重试 |
| 写后等锁或等队列 | 一直等；超过 30 秒 status 点名持有者 | 已写的留下 | 人看 status |
| 会话正在做 checkpoint | 排队，不取消 | 无新的，直到轮到 | 做完再跑 |
| 凭据不符、用过、过期、action 不对 | `NotConfirmed` | 无 | 重新确认 |
| 要确认的推送，链顶在弹框之后变了 | `NotConfirmed` | 无 | 重新确认 |
| 上次推送不确定 | status 报出 `UnknownPublish`，不重放、不删行 | 行还在 | 人决定 |
| 崩溃留下的 Running 发布 | status 报出，不改行、不重放 | 行还在 | 人决定 [D6] |
| `observe_main` 失败 | 警告 `ObserveFailed`，再试一次 | 调用本身已完成的留下 | 下次观察补上 |
| attach 越界 | `Unknown` 原样透传，加告警 | 现场留下 | 人看主 checkout 的 index，daemon 不改 |
| 隔离挪失败 | `Unknown`，告警 | 链接可能还在 | 不在该影子仓库上跑 git |
| 停机时还在排队 | `ReadOnly`（reason `shutting_down`） | 无 | 重启后重试 |
| daemon 没在跑 | cli 的只读命令仍可用；`publish` 与 `push` 由 cli 以 `ReadOnly` 拒绝 | 无 | 见 §7 |
| 处理器 panic，写前 | RPC `Failed`，锁已释放 | 无 | 进程继续 |
| 处理器 panic，写后 | `Unknown`，锁已释放 | 现场留下 | status 报出 |
| 配置调松 | 不生效，`ConfigRelaxPending` | 钉住的值不变 | 等 Task 15 的变体与方法表 |
| TOML 或校验失败 | `InvalidInput` | 不听 socket | 改配置 |
| 两个 repo 同一个 `RepoId` | `NameTaken` | 不听 socket | 改配置 |
| `path` 不是 realpath | `InvalidInput`，detail 带 realpath | 不听 socket | 改成 realpath |

结果不确定的按不变量 6：保留现场，不重放，不清理，status 报出。daemon 重启后同样报出。

## 7. 测试清单

| 测试名 | 断言 | 来源（矩阵行 或 新增） |
| --- | --- | --- |
| `concurrent_checkpoints_same_session_serialize` | 同一会话两个 checkpoint 同时到，第二个排队，两个都完成，顺序与到达一致，没有一个被取消；types.md 里没有 InFlight 这个码 | `layerfs-daemon/tests/store_commit.rs::a_held_producer_does_not_hold_workspace_metadata_or_read_capacity` |
| `rm_queued_behind_in_flight_checkpoint` | checkpoint 进行中收到 `rm`，`rm` 在该 checkpoint 完成之后才执行，不取消它，不返回 `Busy` | `layerfs-daemon/tests/store_commit.rs::a_held_producer_does_not_hold_workspace_metadata_or_read_capacity` |
| `unknown_publish_reported_on_restart_not_replayed` | publishes 里有一行 `Unknown`；重启后 `status` 与 `daemon_status` 都含它；`git push` 没被调用；该行还在 | 新增 |
| `cli_readonly_commands_work_without_daemon` | daemon 没听 socket 时，`status`、`grep`、`files` 仍返回 JSON（cli 直接读）；`publish` 与 `push` 失败并说明 daemon 没在跑（`ReadOnly`） | 新增 |
| `status_does_not_wait_on_write_lock` | 写锁被占住时，`status`、`grep`、`files`、`diff`、`deps_list`、`daemon_status` 都在 1 秒内返回 | `layerfs-api/tests/sdk_reads_during_publish.rs::reads_do_not_wait_for_a_held_publication_lane` |
| `busy_only_before_first_write` | 调用还没写时，锁超过 30 秒 → `Busy`，什么都没写；已经写过的调用在同样的持锁下等到拿到，不返回 `Busy` | `layerfs-persistence/tests/writer_admission.rs::queued_writers_outlive_the_old_window_and_all_succeed` |
| `quarantine_is_an_alert` | 隔离区有一条链接时，status 的 warnings 点名它，stderr 有一行，且这次启动没有对那个影子仓库跑 `git gc`、`worktree prune` 或 `git commit` | 新增 |
| `locks_released_on_return_error_and_panic` | 处理器正常返回、返回 `Err`、panic 之后，repo 写锁与会话队列都已释放；随后的请求不是因为中毒而 `Busy`；原错误原样返回 | `layerfs-telemetry/tests/observations.rs::optional_windows_release_on_return_error_and_unwind` |
| `second_instance_refused_then_reacquired` | 持有期间第二个进程得到 `Exists`（reason `daemon_running`），不是 `Busy`；停机后 1 秒内新进程能拿到锁并 bind | `layerfs-telemetry/tests/platform.rs::concurrent_final_handles_release_the_owned_writer` |
| `concurrent_sessions_different_files_all_commit` | 多个会话同时 checkpoint，各改不同文件，全部成功，改动都在 | `layerfs-daemon/tests/concurrent_commits.rs::concurrent_fs_writes_of_different_files_all_publish` |
| `lost_rpc_reply_is_not_replayed` | 写方法已完成但连接断开：daemon 不重放；另一个客户端的 `status` 只报告现状，不替那次丢失的回复裁定 | `layerfs-daemon/tests/mount_control.rs::original_lost_control_reply_is_not_replayed_or_settled_by_an_observer` |
| `status_sees_in_flight_without_cancelling` | checkpoint 进行中时另一个调用的 `status` 能看到进行中；同一时刻的 `rm` 排队而不是在产生效果之前以 `Busy` 拒绝；checkpoint 随后完成 | `layerfs-daemon/tests/mount_control.rs::another_control_channel_observes_running_commit_and_refuses_unmount_before_effect` |
| `e2e_checkpoint_restore_matches_oracle` | 真实 `git-stratad` 与 CLI：`new`、外部进程改工作区、经 socket `checkpoint`、再 `restore` 到新目录，结果与独立 oracle 一致。没有 Linux 安装、Store 安装或认证握手 | `layerfs-daemon/tests/r5_1_actual_daemon_commit.rs::r5_1_the_actual_daemon_commits_a_change_by_external_bash_and_a_fresh_mount_is_exact` |
| `repo_write_lock_is_first_come_first_served` | 同一 repo 的写请求按到达顺序执行 | `layerfs-persistence/tests/writer_admission.rs::queued_writers_are_admitted_first_come_first_served` |
| `shutdown_fails_queued_lets_holder_finish` | 停机时在跑的请求先完成；排队的以 `ReadOnly`（reason `shutting_down`）失败；停机之后的新连接被拒 | `layerfs-persistence/tests/writer_admission.rs::closing_the_store_fails_every_queued_writer_typed_and_lets_the_holder_finish` |
| `checkpoint_during_publish_does_not_cancel` | publish 的 fetch/检查/commit/push 阶段，同一会话的 checkpoint 与 `rm` 排队，publish 不被取消，也不返回错误码；建造缺口里 checkpoint 可以跑完，`rm` 仍等到 `publishing` 清除（「T10 publish / T13 daemon (from 3d)」，缺口是 D14） | 新增 |
| `no_auth_facade_observation` | 信封没有 scope、token 或 observed 标记。同 uid 的 JSON lines 即连接 | `layerfs-api/tests/observed_control.rs::facade_observation_is_explicit_and_does_not_persist_on_channel` |
| `no_auth_reply_token` | 答复没有 token 校验阶段 | `layerfs-api/tests/observed_control.rs::observed_reply_keeps_original_exact_token_validation` |
| `no_linux_install_path` | 没有受保护配置、Store 安装或 Linux 二进制安装。未改动不由 daemon 跑安装来证明 | `layerfs-daemon/tests/kernel_application.rs::actual_daemon_install_hello_bind_status_no_constructor_and_session_end` |
| `no_auth_observation_identity` | 没有观察身份或 worker 槽。状态经 `status` | `layerfs-daemon/tests/observed_application.rs::equal_calls_and_reused_slot_have_distinct_public_observation_identity` |
| `no_auth_diagnostic_sink` | 没有诊断 sink。不重放、不替未知回复定论，由 `lost_rpc_reply_is_not_replayed` 承载 | `layerfs-daemon/tests/observed_application.rs::diagnostic_sink_failure_does_not_repeat_or_hide_successful_bind` |
| `no_auth_traced_diagnostic` | 没有 traced 诊断层。连接是普通的 unix socket | `layerfs-daemon/tests/observed_application.rs::traced_diagnostic_failure_is_reported_and_keeps_the_connection` |
| `no_auth_traced_scope` | 答复不带遥测 scope | `layerfs-daemon/tests/observed_application.rs::traced_records_share_the_scope_and_untraced_replies_stay_plain` |
| `failure_keeps_scene_no_replay` | 失败保留现场，不重放（不变量 6）。没有诊断围栏 | `layerfs-daemon/tests/observed_application.rs::resource_and_output_failure_preserve_original_status_reply` |
| `daemon_does_not_idle_exit` | 空闲不退出。`KeepAlive` 为真。没有「Workspace 绑定让进程保持」的关系 | `layerfs-daemon/tests/observed_application.rs::an_idle_daemon_exits_and_a_workspace_keeps_it` |
| `no_binary_frame` | 没有 8192 字节定长记录，也没有截断前缀校验。一行一个 JSON | `layerfs-bridge/tests/control_records.rs::commands_are_bounded_exact_and_reject_every_truncated_prefix` |
| `no_binary_reply_frame` | 没有定长答复。不确定的发布由 status 报出、不重放 | `layerfs-bridge/tests/control_records.rs::replies_preserve_scoped_status_conflict_and_known_publication` |
| `no_binary_tag_extension` | 没有二进制 tag。字段增补由 serde 承担，未知字段是 RPC `Failed` | `layerfs-bridge/tests/control_records.rs::duplex_block_is_additive_and_bounded_fields_are_refused_before_send` |
| `no_branch_page_frame` | 没有 Branch 分页。RPC 一次返回 JSON | `layerfs-bridge/tests/control_records.rs::branch_page_bounds_are_checked_before_encoding_or_allocating_records` |
| `no_tcp_nodelay_auth` | 不用 TCP，不设 `TCP_NODELAY`，没有认证握手 | `layerfs-bridge/tests/duplex.rs::successful_duplex_channels_enable_nodelay_on_both_authenticated_sockets` |
| `no_noise_window` | 没有 Noise 记录或固定密文窗 | `layerfs-bridge/tests/duplex.rs::authenticated_duplex_records_stream_beyond_one_window_with_fixed_buffers` |
| `no_nonce_record_limit` | 没有 nonce，没有记录上限。超帧是 `TooLarge`，不是 RecordLimit | `layerfs-bridge/tests/duplex.rs::oversized_unattempted_record_does_not_consume_a_nonce` |
| `confirm_is_not_channel_auth` | 没有对端静态身份。需要人的动作走 confirm，与 socket 无关 | `layerfs-bridge/tests/duplex.rs::wrong_static_identity_fails_the_actual_handshake` |
| `no_encrypted_record` | 没有加密记录帧，也没有双方向隔离 | `layerfs-bridge/tests/duplex.rs::malformed_record_quarantines_both_directions_before_body_allocation` |
| `no_tamper_replay_frame` | 没有认证记录的篡改检测。不重放由不变量 6 承担 | `layerfs-bridge/tests/duplex.rs::corrupted_authenticated_record_retains_the_crypto_failure_without_replay` |
| `no_duplex_eof_fence` | 没有 duplex 的终端 EOF 或平台 fence | `layerfs-bridge/tests/duplex.rs::terminal_reply_retains_eof_and_original_platform_fence_outcome` |
| `no_duplex_trailing_byte` | 没有 wait_peer_end，也没有终端多余字节状态机 | `layerfs-bridge/tests/duplex.rs::terminal_trailing_input_keeps_original_byte_work_and_has_no_second_read` |
| `no_duplex_read_timeout` | 没有 wait_peer_end 的读超时阶段 | `layerfs-bridge/tests/duplex.rs::terminal_read_error_keeps_phase_and_original_fence_attempt` |
| `no_observed_prefix` | 没有 observed scope 前缀。一行 JSON 就是请求 | `layerfs-bridge/tests/observed_records.rs::one_checked_scoped_layer_preserves_point_request_capacity` |
| `no_observed_wrapping` | 没有 observed 包裹，也没有内层 tag | `layerfs-bridge/tests/observed_records.rs::observed_lifecycle_and_nesting_are_rejected_before_inner_decode` |
| `no_zero_scope` | 没有全零 scope 的拒绝阶段 | `layerfs-bridge/tests/observed_records.rs::zero_scope_is_rejected_before_inner_body_decode` |
| `no_cleanup_tags` | 没有清理请求的二进制 tag 或 Live/Held/Queued/Gone 状态字节 | `layerfs-bridge/tests/observed_records.rs::cleanup_has_exact_additive_tags_and_checked_state` |
| `no_traced_prefix_tag` | 没有 traced 前缀 tag | `layerfs-bridge/tests/observed_records.rs::traced_observation_differs_only_in_its_prefix_tag` |
| `no_telemetry_beside_reply` | 答复旁边没有遥测 | `layerfs-bridge/tests/observed_records.rs::observed_reply_carries_telemetry_beside_the_unchanged_reply` |
| `concurrent_sessions_are_not_refused_for_a_sibling` | 不同会话的并发调用各答各的，不因为兄弟调用正处在两步之间就被拒 | `sdk/js/test/verify/concurrent_unmount_real.mjs` |
| `same_machine_unix_socket` | stratad 与 CLI 同机，socket 在配置目录，没有 `daemonHost` | `sdk/js/test/trace.mjs::daemon-host` |
| `no_container_daemon_host` | 没有容器回环，也没有 `LAYERFS_DAEMON_HOST` | `sdk/js/test/verify/daemon_host_real.mjs` |
| `readiness_is_socket_connect` | 就绪 = socket 可连接。没有 stderr 标记或子串标记 | `layerfs-sandbox/tests/engine_lifecycle.rs::stderr_and_substring_markers_never_satisfy_readiness` |
| `no_container_archive_of_the_binary` | daemon 是 launchd agent，不把可执行文件打进容器档案 | `layerfs-sandbox/tests/engine_lifecycle.rs::streamed_private_archive_exact_marker_endpoint_and_borrowed_cleanup` |
| `idle_close_is_not_ours` | 空闲关闭不是本 daemon 的行为；重启与对账是 §5.1，不是回收容器 | `sdk/js/test/verify/idle_close_real.mjs` |
| `anchor_advances_after_fetch` | fetch 之后、仍在 repo 锁里，`ChainState` 为 Equal、Leads、Behind 或 Diverged 且 `git.merge_base(o, c)` 是旧锚点的后代时，锚点比较并交换到该 commit；Absent 与 Rewound 不动 | 新增 |
| `observe_failed_does_not_fail_new_or_publish` | `observe_main` 返回错误时 `new` 与 `publish` 仍成功，`data.warnings` 含 `ObserveFailed`；daemon 再观察一次 | 新增 |
| `toolcall_does_not_wait_out_approve_ensure` | `deps_approve` 的 `ensure_target` 冷装期间，该会话的 ToolCall checkpoint 不等待；移动阶段才排队 | 新增 |
| `config_relax_not_applied_on_restart` | 停机期间把 `deploy_on_push` 从 true 改成 false，重启后推送仍要确认；status 含 `ConfigRelaxPending`；没有弹框 | 新增 |
| `hint_and_flush_off_callback_thread` | FSEvents 回调里只有 `Inbox::note`。`hint`、`flush`、丢弃 `Watcher` 都发生在请求线程或停机线程。回调里不 `dispatch_sync` 到 Watcher 的队列 | 新增 |
| `release_daemon_has_no_test_seams` | `TEST_SEAMS` 为真的二进制拒绝启动，不听 socket | 新增 |
| `long_hold_named_in_status` | 持锁超过 30 秒时，`status` 写出方法、repo 与开始时间 | 新增 |

`second_exporter_waits_then_busy_never_exists` 不在本表。后写的裁定把它交给 session 的登记时去重（「T13 / T10 notes (from gitio 60b1cf5)」），本文不再写。

3d `interrupted_transfer…` 的并发导出半边同样归 session，不归本文。

## 8. 实现任务（TDD）

### Task 1: RPC 信封与方法表

**Files:**

- Create: `crates/strata-daemon/src/rpc.rs`
- Test: `crates/strata-daemon/tests/rpc.rs`

**Interfaces:**

- Consumes: 无上游。`Code` 不增加
- Produces: §3 的信封、`RpcReject`、`MAX_FRAME`、冻结的 16 个方法名

- [ ] **Step 1: 写失败的测试**：`no_auth_facade_observation`、`no_auth_reply_token`、`no_binary_frame`、`no_binary_reply_frame`、`no_binary_tag_extension`、`no_branch_page_frame`、`no_tcp_nodelay_auth`、`no_noise_window`、`no_nonce_record_limit`、`no_encrypted_record`、`no_tamper_replay_frame`、`no_duplex_eof_fence`、`no_duplex_trailing_byte`、`no_duplex_read_timeout`、`no_observed_prefix`、`no_observed_wrapping`、`no_zero_scope`、`no_cleanup_tags`、`no_traced_prefix_tag`、`no_telemetry_beside_reply`、`no_auth_observation_identity`、`no_auth_diagnostic_sink`、`no_auth_traced_diagnostic`、`no_auth_traced_scope`、`confirm_is_not_channel_auth`、`same_machine_unix_socket`、`no_container_daemon_host`、`readiness_is_socket_connect`、`no_container_archive_of_the_binary`。未知方法断言 `error.rpc == "UnknownMethod"` 且没有 `code`。参数坏断言 `Failed`。超帧断言 `TooLarge`。
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-daemon --test rpc`

Expected: FAIL（方法未定义）

- [ ] **Step 3: 写让测试通过的最少实现**：§3 的一行一个 JSON。不抄 tgrep 的 jsonrpc 字段。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-daemon --test rpc`

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-daemon/src/rpc.rs crates/strata-daemon/tests/rpc.rs
git commit -m "feat(daemon): json-lines rpc envelope"
```

### Task 2: 锁、队列与停机

**Files:**

- Create: `crates/strata-daemon/src/locks.rs`
- Test: `crates/strata-daemon/tests/locks.rs`

**Interfaces:**

- Consumes: `RepoWriteLock`
- Produces: `RepoLocks`、`LOCK_WAIT`、会话队列、`LockHold`

- [ ] **Step 1: 写失败的测试**：`concurrent_checkpoints_same_session_serialize`、`rm_queued_behind_in_flight_checkpoint`、`status_does_not_wait_on_write_lock`、`busy_only_before_first_write`、`locks_released_on_return_error_and_panic`、`repo_write_lock_is_first_come_first_served`、`shutdown_fails_queued_lets_holder_finish`、`long_hold_named_in_status`、`concurrent_sessions_different_files_all_commit`、`concurrent_sessions_are_not_refused_for_a_sibling`、`status_sees_in_flight_without_cancelling`、`checkpoint_during_publish_does_not_cancel`、`toolcall_does_not_wait_out_approve_ensure`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-daemon --test locks`

Expected: FAIL

- [ ] **Step 3: 写让测试通过的最少实现**：§4 的 I2–I5、I11，§5.2。写后的等待用测试接缝把 `visible` 设上，不依赖邻居先改 trait。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-daemon --test locks`

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-daemon/src/locks.rs crates/strata-daemon/tests/locks.rs
git commit -m "feat(daemon): repo lock, session queue, busy before write"
```

### Task 3: 启动、单例、Unknown 与隔离

**Files:**

- Create: `crates/strata-daemon/src/main.rs`
- Create: `crates/strata-daemon/src/startup.rs`
- Test: `crates/strata-daemon/tests/startup.rs`

**Interfaces:**

- Consumes: `Store::open`、`quick_check`、`open_publishes`、`gc_repo`、`session::recover`、`Sessions::open`
- Produces: 单例锁、`daemon.clean`、§5.1 的顺序

- [ ] **Step 1: 写失败的测试**：`unknown_publish_reported_on_restart_not_replayed`、`second_instance_refused_then_reacquired`、`quarantine_is_an_alert`、`release_daemon_has_no_test_seams`、`daemon_does_not_idle_exit`、`idle_close_is_not_ours`、`no_linux_install_path`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-daemon --test startup`

Expected: FAIL

- [ ] **Step 3: 写让测试通过的最少实现**：§5.1。gc 在 recover 之后、listen 之前。`Unknown` 只读进内存。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-daemon --test startup`

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-daemon/src/main.rs crates/strata-daemon/src/startup.rs crates/strata-daemon/tests/startup.rs
git commit -m "feat(daemon): single instance, recover, then listen"
```

### Task 4: 确认、钉住、PATH 与 plist

**Files:**

- Create: `crates/strata-daemon/src/confirm_flow.rs`
- Create: `crates/strata-daemon/src/plist.rs`
- Test: `crates/strata-daemon/tests/confirm_flow.rs`

**Interfaces:**

- Consumes: `Confirmer::require`、`validate`、`probe`；`Action`；`RepoConfig::needs_human_confirm`
- Produces: `install_agent`、`pins.json`、`PinnedFlags`、`DEFAULT_PATH`

- [ ] **Step 1: 写失败的测试**：`config_relax_not_applied_on_restart`，外加：弹框时不持有队列或 repo 锁；action 不符 → `NotConfirmed`；daemon 不返回 `NotTty`；`InstallEnv.path` 等于 D1；plist 含 `Label`、`KeepAlive`、`LimitLoadToSessionType = Aqua`、D1 的 PATH。
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-daemon --test confirm_flow`

Expected: FAIL

- [ ] **Step 3: 写让测试通过的最少实现**：§5.3 第 4 步、§4 的钉、§3 的 plist。不定义 `ConfigRelax`。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-daemon --test confirm_flow`

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-daemon/src/confirm_flow.rs crates/strata-daemon/src/plist.rs crates/strata-daemon/tests/confirm_flow.rs
git commit -m "feat(daemon): confirm order, pinned flags, launchd plist"
```

### Task 5: fetch 之后的锚点、观察与告警

**Files:**

- Create: `crates/strata-daemon/src/fetch_after.rs`
- Test: `crates/strata-daemon/tests/fetch_after.rs`

**Interfaces:**

- Consumes: `publish_chain`、`main_tip`、`observe_main`、`gitio::update_ref`、`gitio::merge_base`、`is_ancestor`
- Produces: §5.7、`DaemonWarning::AttachOutside`、`AnchorNotAdvanced`

- [ ] **Step 1: 写失败的测试**：`anchor_advances_after_fetch`、`observe_failed_does_not_fail_new_or_publish`、`hint_and_flush_off_callback_thread`，外加 attach 两个 reason 的告警测试（不改 index），以及 `PrePushGuardDisabled` 只报告、不写影子 config。
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-daemon --test fetch_after`

Expected: FAIL

- [ ] **Step 3: 写让测试通过的最少实现**：§5.7、§5.10。锚点 ref 名 `refs/strata/anchor/<branch>`。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-daemon --test fetch_after`

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-daemon/src/fetch_after.rs crates/strata-daemon/tests/fetch_after.rs
git commit -m "feat(daemon): anchor after fetch, observe warning, attach alert"
```

### Task 6: 端到端与 daemon 不在时的只读

**Files:**

- Test: `crates/strata-daemon/tests/daemon.rs`
- Test: `crates/git-strata/tests/readonly_without_daemon.rs`

**Interfaces:**

- Consumes: 上面各任务的签名；cli 的只读路径（`Store::open_read_only`）
- Produces: 无新签名

- [ ] **Step 1: 写失败的测试**：`e2e_checkpoint_restore_matches_oracle`、`cli_readonly_commands_work_without_daemon`、`lost_rpc_reply_is_not_replayed`、`failure_keeps_scene_no_replay`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-daemon --test daemon`

Expected: FAIL

- [ ] **Step 3: 写让测试通过的最少实现**：§5 的启动与请求串起来。只读回退在 cli，断言在 cli 的测试里，契约在本文。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-daemon --test daemon`

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-daemon/tests/daemon.rs crates/git-strata/tests/readonly_without_daemon.rs
git commit -m "feat(daemon): rpc, locks, and unknown-publish report"
```

## 9. 构建者决定记录

| 决定 | 选择 | 理由 | 日期 |
| --- | --- | --- | --- |
| D1 显式 PATH | `/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin` | launchd 没有用户 PATH。这台机器的 bun 与 git 在 Homebrew。`~/.bun/bin` 与 `~/.cargo/bin` 不在缺省里；staff-api 的 php 在配置里写绝对路径。配置以后可以覆盖 | 2026-10-23 |
| D2 影子仓库漂移只报告 | 不重钉 `hooksPath`，不要求 gitio 加函数 | gitio @1cd5ac5 写明不修。重钉会与 husky / lefthook 对打，并且要往活的影子仓库写。status 已有 `MirrorDrift` | 2026-10-23 |
| D3 帧上限 | 16 MiB，超过 → `TooLarge` | types.md 把帧上限指给 daemon，又不许加码。16 MiB 装得下一次大的 diff，又挡得住不换行的灌入 | 2026-10-23 |
| D4 params 未知字段 | RPC `Failed` | 打错的字段若被忽略，cli 与 daemon 会各跑各的。冻结表的字段就这些 | 2026-10-23 |
| D5 gc 保留期 | 14 天 | design §7 交给构建者。store 的 `keep` 要一个值；Removed 会话与 BeforeRemove 的内容留两周 | 2026-10-23 |
| D6 崩溃留下的 Running 行 | 只报告，不改 phase | 裁定只要求报 `Unknown`、不重放、不删除。把 Running 改成 Unknown 是 publish / store 的状态图，草案还没写 recover。改行会冒充已经裁定的转换 | 2026-10-23 |
| D7 plist 的 RunAtLoad 与 ProcessType | 都写上 | `KeepAlive` 与 `Aqua` 是裁定。没有 `RunAtLoad`，bootstrap 之后不一定起来。`Interactive` 让认证框画得出。Label、PATH、socket 路径不是这条决定 | 2026-10-23 |
| D8 写前与写后的等待 | 同一 RPC 里第一次 `with_write` / 占队列在 `visible == false` 时有界，之后无界 | trait 上没有 `note_visible_write`，邻居不能反向依赖 daemon。前提：第一次取锁在第一次可见的写之前。打破前提时走 §2 的上游要求 | 2026-10-23 |
| D9 `remove` 的锁内再核对 | 调用前核对一次；锁内由 `remove` 核对 action | session 要求调用时不持锁，锁又不可重入。confirm.md D2 的选择器窗口留着，不发明 session 没有的回调 | 2026-10-23 |
| D10 按需 quick_check | `daemon_status` 的 `verify: true` | 裁定要本文点名校验命令。冻结表没有 `verify`。启动时只在不清洁退出之后跑，与 store 的说法一致 | 2026-10-23 |
| D11 钩子等 checkpoint | 做完再回答 | 先应答会把下一次调用的早期写入算进这一次。没有路径清单的冷扫大约 0.20 秒，超出 200 ms，阶段结束时告诉用户 | 2026-10-23 |
| D14 publish 的建造缺口 | 建造前放开队列与 repo 锁；`rm` 仍等 `publishing`；checkpoint 在缺口里可以跑 | 同时满足「不持队列等建造锁」和「不取消进行中的 publish」。3d 那条在 commit/push/move 阶段仍是排队 | 2026-10-23 |
| D15 一次一把 repo 锁 | 按批次里的 repo 顺序，做完一个再拿下一个 | 一组跨 repo 时两把锁没有裁定过的顺序。一次一把不会死锁 | 2026-10-23 |
| D16 小时对账不写 checkpoint | 只刷新 Tracker 缓存 | `CheckpointReason` 没有小时这一项。发明一种会写出人没要的检查点 | 2026-10-23 |
| D17 停机时排队的写 | `ReadOnly`，reason `shutting_down` | types.md 没有 Closed。`ReadOnly` 含「写被拒绝」。映射说在这里 | 2026-10-23 |
| 对账间隔 | 1 小时 | 初稿已定，落实 design §6.2 交给构建者的间隔 | 2026-10-23 |
| 第二个进程的码 | `Exists`，不是 `Busy` | 矩阵写明码由本文定，且 `Busy` 只用于写锁。`Exists` 是「要建的已经在」里最近的一个 | 2026-10-23 |
