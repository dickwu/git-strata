# daemon

**Crate**：`crates/strata-daemon`（lib + bin `git-stratad`）· **设计来源**：design.md §5、§6.2、§8、§9、§12 · **矩阵**：test-matrix 里目标模块为 `daemon` 的行，以及 `—` 行里把替代物写成 daemon / launchd / 本机 unix socket 的行，逐条见 §7 · **邻居版本**：session.md @a324f9c，deps.md @dafdefc，gitio.md @1cd5ac5，confirm.md @229f4e3，overlay.md 与 store.md @08ffa53；publish.md @df9f069；search.md 草案 @aa0414c（只借方法名）

> 本文按 module-rulings.md 写：「T13 daemon」，以及标了 T13 或 Everyone 且约束本模块的增补。后写的、标了 amended 的那条胜。一台机器一个 daemon。方法表是 cli 子命令的对照，名字与下面这张表一致，不改名。控制器在初稿的 16 个之外加了 `resolve_move`、`chain_resolve` 与 `config_relax`。本文不再加别的方法。`Busy` 只用于第一次可见的写之前的等锁（缺省 30 秒）。写之后的等待不受这 30 秒截断。重启报出 `Unknown` 的发布，不重放。不加 `Code`：`Failed` 与 `UnknownMethod` 是 RPC 层的回答。`Code` 停在 39 个。

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
| publish @df9f069 | `publish`（入口）、`push`、`assign`、`diff_between`、`chain_ref`、`anchor_ref`；实现 `PublishLocks` | 不自己拼 `stage` / `check` / `commit`，不自己选合并基 B。`chain_rewound` 与 `chain_diverged` 由 `publish::publish` 在 commit 与 push 之前拒绝 |
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
12. `Store::close(self)` 吃掉 `Store`。daemon 先停掉自己持有的全部 `Arc<Store>` 使用者，再 close。`Store::open` 自己看 `store/clean-shutdown`：标记不在就跑 `quick_check`，失败返回 `Corrupt`，daemon 不听 socket。daemon 不另建干净标记，启动时不调用 `quick_check`。人要的那次只走 `daemon_status` 的 `verify: true`。
13. ToolCall：Edit/Write 的路径经 `Inbox::hint`（裁定的 `Tracker::hint` 就是这个：`Tracker::inbox()` 交出的句柄；overlay @08ffa53 没有 `Tracker::hint`），没有路径清单的工具经 `Inbox::require_full`，然后在请求线程上 `Tracker::flush()`。`hint` 读属性，不在 FSEvents 回调线程上调（「T13 (daemon, from overlay r2)」）。`flush` 返回 `()`。不在回调里 `flush`，也不在回调里丢弃 `Watcher`（「Event flush, amended — corrected by measurement」：回调里对 Watcher 自己的队列 `dispatch_sync` 是 SIGTRAP，exit 133，不是死锁；`FlushSync` 单独在回调里会返回，但 daemon 仍不在回调里调它）。
14. 交换之后，以及 Accept 之后：`Tracker::reset` 再 `watch`。status 列出旧树上的 `holders`（session 的 `OldTreeKept`）。健康为 `Unknown` 的工作区给出 Abandon 与 Accept 两种处理，本文不自动选（「Resolving a failed move」的 T13 部分）。
15. 同一会话的 checkpoint、restore、`rm`、`move_workspace` 串行。进行中的 checkpoint 不取消。`resolve_failed_move` 走同一条队列（session §2 第 3 条，比初稿多这一个）。

`InstallEnv.path` 就是 D1 那条。`InstallEnv.timeout` 用 deps 的缺省 60 分钟，daemon 不另定。`RepoWriteLock` 由本文实现，经 `Ctx` 与 `Sessions::open` 交出去。种子判定是 session 的：`RepoConfig::seed_files()` 返回 `(candidates, explicit)`，daemon 不把候选列表当成显式条目再判一遍。

**上游要求**（邻居还没有的，本文不假装已经有）：

- `deps::RepoWriteLock` 没有 `note_visible_write`。本文用 §5.2 的线程局部规则区分写前与写后的等待 [我的决定 D8]。若将来有调用方在第一次 `with_write` 之前就做了可见的写，deps 应在 trait 上加这个方法，session / publish / overlay 在各自的第一次可见的写处调用。在那之前，本文的规则成立的前提写在 D8。
- gitio @1cd5ac5 没有重钉 `hooksPath` 的函数，并且写明影子仓库的漂移只报告、不修。本文选择只报告，不要求 gitio 加函数 [我的决定 D2]。
- gitio @1cd5ac5 没有把 a..c 重放到 o 上的函数，也没有一次写下链和锚点的 ref 事务。重放是 `Git::replay_commits` [我的决定 D18]。链与锚点的写是 publish.md @df9f069 §2 第 3 条要求 gitio 提供的那一次调用（`update-ref --stdin` 或等价物）。daemon 用这一次，不拆成两次 `update_ref`，不起 git。

## 3. 公开接口（Rust）

RPC 是 JSON 行，一行一个请求，一行一个回答。形状只借 tgrep `SHARED_WORKTREE_INDEXES.md` §「Daemon wire contract v1」的「一行一个 JSON」：不用它的 `jsonrpc` 信封、TCP、instance、lease 或认证。socket 是 `~/Library/Application Support/git-strata/daemon.sock`。目录 `0700`，socket `0600`，同 uid 即可，没有认证通道（design §5、§12）。

请求：`{ "id", "method", "params" }`。`id` 是 JSON 字符串或数字，原样回显。缺 `id`、`method` 或 `params`，或 `params` 对不上该方法的字段（多了、少了、类型不对）→ RPC `Failed`，不是 `Code`。一行超过 16 MiB → `TooLarge`（types.md：帧上限由 daemon 返回这个已有的码）[我的决定 D3]。

成功：`{ "id", "ok": true, "data" }`。`data` 总是有；没有结果时是 `{}`。失败且是上游的 `StrataError`：`{ "id", "ok": false, "error": { "code", "message", "detail"? } }`，`code` 是变体名。失败且是 RPC 层：`{ "id", "ok": false, "error": { "rpc", "message" } }`，`rpc` 只取 `UnknownMethod` 或 `Failed`，没有 `code` 字段。客户端靠有没有 `code` 区分，不把这两个词放进 `Code`。

未知方法 → RPC `UnknownMethod`。参数对不上 → RPC `Failed`。

方法与 cli 子命令同名。T14 按这张表绑定。名字不改。除表里这 20 个以外不加方法（`tests/rpc.rs` 的 `daemon_md_method_table_matches_rpc` 核对这个数、名字与只读列，review-3 T4）：

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
| `publish_plan` | 同一个 group 的发布会做什么：还要提交的仓库与已经落地的仓库。不写任何东西（review-2 N2）[实现时补的决定] | 是 |
| `push` | `publish::push` | 否 |
| `assign` | 点名冲突解决者 | 否 |
| `rm` | `session::remove` | 否 |
| `gc` | `store::gc_repo` | 否 |
| `resolve_move` | `session::resolve_failed_move`。不查 TTY，不收凭据 | 否 |
| `chain_resolve` | 人对 Rewound / Diverged 的显式处理。接受 origin 不推送；恢复只在快进时推 | 否 |
| `config_relax` | 显式接受一次调松。文件变了不弹框。路径能编译。调松仍要这条命令；调紧立即生效 | 否 |
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
| `publish` | `{ "group": ReleaseGroup, "shown"?: { RepoName: CommitOid \| null }, "plan"?: [RepoName] }` | 每个 repo 的 commit 与 `PushOutcome`；`group.id` 由 store 分配，客户端传来的值不算数。`shown` 是 cli 给人看过的每个要确认仓库的链顶（D21）。`plan` 是 cli 的 TTY 检查覆盖了的、会推且要确认的仓库（cli.md D23）；给了 `plan` 时，daemon 自己重算出的要提交仓库里，凡是会推且要确认、却不在 `plan` 里的（`publish_plan` 与 `publish` 之间发布历史变了），在弹框、取凭据、写任何东西之前就拒绝：`NotConfirmed`，`detail.reason = "plan_changed"`，`detail.repo`、`detail.planned`；agent 重跑同一条命令即可（重跑时 cli 会把它算进计划并查 TTY）。不给 `plan`（cli 以外的调用方）不做这项检查（review-3 T1）[实现时补的决定]。回复另带 `already_published: [{repo, publish, commit, push}]`：之前已经落地、这次跳过的仓库（D22）。失败时 `detail.repo` 是停下的仓库，`detail.published_before` 是这次已经提交的仓库。停下的仓库若已写了提交（`detail.commit`，如推送超时或被拒），`detail.landed` 是它的那条 `{repo, commit, push}`，`push` 取已知的结果（`Unknown` / `Refused`，没推为 null）（review M4）[实现时补的决定]；`push` 与 `checks_ran` 取 publish 错误 detail 里写明的值，推送成功后才失败（rebase、关行）的是 `Pushed`（review-2 N3）[实现时补的决定]。`repo` / `published_before` / `already_published` 加在该仓库的任何错误上，不只是 publish 本身的：读配置、确认失败或被取消（`require_validated`）同样带上（review-2 N6）[实现时补的决定]。`plan_changed` 的检查仍在逐仓库循环之前（还什么都没写），但它和它读配置的错误走同一套装饰：带 `repo`，以及重算计划时发现已经落地、这次跳过的 `already_published`（这时 `published_before` 必为空，不出现）（review-4 F2）[实现时补的决定] |
| `publish_plan` | `{ "group": ReleaseGroup }` | `{ "repos": [RepoName], "already_published": [{repo, publish, commit, push}] }`。与 `publish` 用同一份重跑过滤（D22）：`repos` 是这次 `publish` 还会提交、会确认的仓库，按名字排序；`already_published` 与 `publish` 会报的一致。不排队，不拿锁，不写 Store、ref、工作区，不弹框。只覆盖按内容判定的落地；会话没被 rebase 时 publish 的 `NoCommit` 回退（D22）只有真正 stage 才知道，这类仓库仍在 `repos` 里（`publish_plan_keeps_a_waiting_sessions_landed_guarded_repo`：rebase 为 `Waiting` 的会话，其要确认的仓库仍在 `repos`，cli 照样要 TTY 与确认；skills 的 reference 照此写明，review-3 T3）。group 为空 → `InvalidInput`。cli 用它把 TTY 检查和确认限制在还要做事的仓库上（cli.md §5，review-2 N2）[实现时补的决定] |
| `push` | `{ "repo": RepoName, "shown"?: CommitOid \| null }` | `PushOutcome`。`shown` 是 cli 给人看过的链顶（D21） |
| `assign` | `{ "group": ReleaseGroup, "path": RepoPath, "session": SessionId }` | `{}` |
| `rm` | `{ "session": SessionId, "discard": bool }` | `RemoveReport` |
| `gc` | `{ "repo": RepoName\|null }` | `GcReport` 的数组 |
| `resolve_move` | `{ "session": SessionId, "repo": RepoName, "abandon": bool, "accept": bool }` | `ResolutionReport`。两个布尔必须恰好一个为真，否则 RPC `Failed`，不调用 session |
| `chain_resolve` | `{ "repo": RepoName, "choice": ChainChoice, "shown"?: CommitOid \| null }` | 见 §5.5。`choice` 不是这三个变体名 → RPC `Failed` |
| `config_relax` | `{ "repo": RepoName }` | `{}`。没有凭据字段。见 §5.5 |
| `daemon_status` | `{ "verify": bool }` | 见 §5.4；`verify` 缺省 false。另带 `reconcile: { runs, last }`：§5.9 上一轮的 `scanned`（每个工作区扫出的 `changed`）、`skipped`（队列被占，`holder`）、`failed`、`gc` |

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
    /// 不再产生。启动时 Running 已改成 Unknown，按 UnknownPublish 报出。
    PublishRunning { id: u64 },
    /// 配置文件比钉住的值松，还没经 ConfigRelax 接受。
    ConfigRelaxPending { repo: RepoName },
    /// Store::is_poisoned。
    StorePoisoned,
    /// quick_check 失败，或上一轮 gc 的 partial。
    VerifyFailed { message: String },
    /// 锚点事务比较并交换两次仍没写成。下次 fetch 再试。
    AnchorNotAdvanced { repo: RepoName },
    /// 单独 `push`（或 restore）的推送结果不确定：暂存目录 `scene`（含 `push.json`）保留。review M6 [实现时补的决定]
    UnknownPush { repo: RepoName, commit: CommitOid, scene: PathBuf },
}

/// 谁持着哪把锁。status 在持有超过 LOCK_WAIT 时带上；daemon_status 总是带上。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LockHold {
    pub repo: RepoName,
    pub method: String,
    pub session: Option<SessionId>,
    pub since: SystemTime,
}

/// `chain_resolve` 的选择。序列化为变体名。恰好一个，没有第四个。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChainChoice {
    /// 把 a..c 干净重放到 origin 的 tip 上。新链，a := o。不推送。
    AcceptOriginReplay,
    /// 丢掉 a..c：链与锚点都改到 o。不推送。
    AcceptOriginDrop,
    /// 把链原样推回去。只在 `is_ancestor(o, c)` 时推。不强推。
    Restore,
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
| `pins.json` | 每个 `RepoName` 的 `PinnedFlags` | 见下。不在 store 里 |
| `daemon.err.log` | stderr | launchd |

`pins.json` 是 UTF-8 JSON：`{ "repos": { "<RepoName>": { "deploy_on_push": bool, "confirm": bool, "push": bool } } }`。第一次启动没有这个文件：把当时加载的配置写成钉，不算调松。之后：

- 调紧立即生效并写回，不要凭据：`deploy_on_push` false→true，`confirm` false→true，`push` true→false。
- 调松不生效：`deploy_on_push` true→false，`confirm` true→false，`push` false→true。有效值仍是钉住的那份。status 带 `ConfigRelaxPending`。文件变了不弹框，启动、重载、KeepAlive 重启都不弹。要生效必须人发 `config_relax`（§5.5）。

不变量：

| 编号 | 内容 | design §9 |
| --- | --- | --- |
| I1 | 一台机器一个进程。锁文件被占时第二个进程以 `Exists` 退出（`detail.reason = "daemon_running"`）。不用 `Busy`：`Busy` 只用于 repo 写锁或会话队列的写前等待 | §5 |
| I2 | 每 repo 一把写锁，不可重入。先到先得。写前等 `LOCK_WAIT`，超时 `Busy`，什么都没写。写后的等待一直等到拿到，不返回 `Busy`（「Waits after a call's first visible write are not bounded」） | §8、不变量 6 |
| I3 | 每会话一条队列，先到先得，不可重入。同一会话的 checkpoint、restore、`rm`、`move_workspace`、`resolve_failed_move` 串行。进行中的不取消，后来的排队，不返回错误码 | §8 |
| I4 | 建造锁 → 会话队列（`SessionId` 升序）→ repo 写锁（一次只拿一个 repo 的）。持有队列或 repo 锁时不等建造锁 | — |
| I5 | status、grep、files、diff、deps_list、publish_plan、daemon_status 不排队，不等写锁 | §8 |
| I6 | 启动时把残留的 `PublishPhase::Running` 改成 `Unknown`（store 允许这条边），然后由 `status` 和 `daemon_status` 报出。不重放、不删除、不调用 `git push`。留下 Running 不是不变量 6 的不确定状态 | 不变量 6 |
| I7 | 凭据不出进程。RPC 参数里没有能代表「已确认」的字段。action 由 daemon 从请求推出，不采用客户端传来的 | 不变量 7 |
| I8 | 不持 `Tracker` 去等队列或锁。回调线程只调 `Inbox::note` | — |
| I9 | 影子仓库上不跑 `git gc`、`git worktree prune`、`git commit`。`worktrees` 或 `worktrees/<n>` 不是目录时，也不把任何 git 子进程的 git-dir 或 cwd 指到那个影子仓库 | 不变量 2 |
| I10 | 主 checkout 的 index 不被 daemon 改。attach 告警只报告 | 不变量 2 |
| I11 | 请求处理器返回、返回错误或 panic 之后，写锁与队列都已释放（Drop）。锁中毒不把后来的请求卡成 `Busy`：清掉中毒，继续服务 | §8 |
| I12 | 小时对账与 gc 与请求共用同一把 repo 锁和同一条会话队列，不另开一套 | §6.2 |

可见的写沿用 Addendum 3 的定义：工作区、活的 gitfile、别人的 store 行、ref、主 checkout、主对象库。`create_session` 是 `new` 的第一次。publish 的第一次不可收回的写是 `commit_tree` 把对象写进主对象库（publish.md @df9f069 §5.2 第 6 步），不是 daemon 自己的 `update_ref`。restore 的第一次是 `mkdir` 那个暂存目录加 `plan.json`（BeforeRestore 不算，Addendum 5 细化 3）。缓存填充（基线 generation、封存的依赖集、blob）不算。锚点的比较并交换一旦调用，这次命令后面的失败是 `Unknown`，不是 `InvalidInput`，也不是「`Conflict` 且 ref 没变」。

panic：写前 → 该请求 RPC `Failed`，进程继续。写后 → 对该请求答 `Unknown`，现场留下，status 报出。两种都释放锁。

## 5. 流程与算法

### 5.1 启动

顺序固定。初稿的四步都在，前面加上还没碰 store 的检查。

1. `TEST_SEAMS` 为真 → 拒绝启动。
2. `flock` `daemon.lock`。拿不到 → stdout 一行 `Exists`（I1），退出，不 bind。
3. 读 `config.toml`，`Config::parse` 再 `validate`。TOML 失败是 `InvalidInput`，`detail.rule = "toml"`，`message` 键是 toml 的原话。`validate` 报文件顺序里的第一条，daemon 不带着部分配置往下走。
4. 每个 `path` 必须存在且等于它的 realpath（绝对、无尾斜杠、无符号链接分量）。否则 `InvalidInput`，detail 里写出 realpath。通过之后路径原样往下传。
5. 读 `pins.json`，按 §4 调紧或记下待定的调松。
6. `Store::open(paths.store(), checkouts)`，`checkouts` 是配置里的主 checkout。它自己看 `store/clean-shutdown`：标记不在就跑 `quick_check`，不是 `ok` 则返回 `Corrupt`。daemon 不另跑，返回 `Corrupt` 就不听 socket。不建 `daemon.clean`。
7. 每个 repo `Git::open(主 checkout)`。`repo_id` 撞车 → `NameTaken`，不听 socket。
8. `Sessions::open` 一次。之后不再开。
9. `session::recover`。然后才 gc：每个 repo 依次，持该 repo 的写锁调 `gc_repo(repo, GC_KEEP)`，再放开。gc 失败记进告警，不因此不听 socket。
10. `open_publishes`：每一行 `PublishPhase::Running` 用 `set_publish_phase(id, Running, Unknown, None, now)` 改成 `Unknown`。然后把 `Unknown`（含刚改的）记进内存，供 status。不调用 `git push`，不重放。
11. 隔离区非空，或 `worktrees` 不是目录 → `Quarantine` 告警，stderr 一行。
12. 然后才 bind socket、听。
13. 对已有的、tracked 的工作区 `Tracker::new(root, DEFAULT_DIRTY_CAPACITY)` 再 `watch`。

听 socket 之前不接受请求。小时计时从开始听算起。

### 5.2 等锁

每个 RPC 一个线程局部 `visible: bool`，初值 false。

- 该线程上第一次 `with_write`，以及第一次占队列，在 `visible` 为 false 时最多等 `LOCK_WAIT`。超时 → `Busy`，回调没跑，队列没占上。
- 同一 RPC 里之后的 `with_write` 和占队列，或者 `visible` 已是 true，一直等到拿到。status 在持有超过 30 秒时写 `LongHold`。
- daemon 在调用会写的函数之前把 `visible` 设上的时机：`session::new` 返回之后（它内部的第一次 `with_write` 在 `create_session` 之前，之后的由「第二次 with_write」这条盖住）。`publish::publish` 自己经 `PublishLocks::with_repo` 拿锁，第一次在 `commit_tree` 之前，放开后再拿的那次已经写过，不等待被 30 秒截断。restore 由 overlay 自己写，daemon 在调用 `restore` 之前已经占上队列，调用期间不再等锁。

前提 [我的决定 D8]：一次 RPC 的第一次 `with_write` 发生在这次 RPC 的第一次可见的写之前。`new` 的 fetch 在 `create_session` 之前取锁，符合。`move_workspace` 由调用方已经持锁再调，它自己不再 `with_write`，符合。若邻居打破这个前提，走 §2 的上游要求，不在这里猜。

队列与锁都是 Drop 释放。panic 也释放（I11）。先到先得：等待队列按到达顺序，不是按线程号。

`publishing` 旗标（按会话）不是锁。`rm` 看见它就等，自己不持有队列、repo 锁或建造锁。checkpoint 在 `publish::publish` 放开队列去 `ensure` 的那段不等它（D14，不重开）。

### 5.3 请求

1. 读一行。对不上方法 → `UnknownMethod`。参数坏 → `Failed`。超帧 → `TooLarge`。
2. 只读方法直接跑，不占队列，不等写锁。写锁被占住时，`status` 在 1 秒内返回。
3. 写方法先做不需要确认就能下的具名拒绝（会话没有、ticket 有歧义、提议不是 Pending）。注定失败的不弹框（confirm.md §5.1 第 2 步）。
4. 要确认的：cli 应已做 `require_tty`。daemon 不查 TTY，也不相信参数里的任何「已确认」。推出 `Action`，在阻塞线程上 `require` 再立刻 `validate`。此时不持有队列或 repo 锁。框开着时第二个 `require` 由 confirm 以 `NotConfirmed`（`prompt_in_progress`）拒绝。核销之后这次确认就用掉了：后面 `Busy`、`OpenFiles`、被取代，人都要重新发命令、重新认证。
5. 要建造的，在占队列之前做完，或先放开队列和 repo 锁再做。`publish` 的这段由 `publish::publish` 自己排（§5.6），daemon 不重写。
6. 占队列（要几个会话就按 `SessionId` 升序全占上，再往下），再拿 repo 锁。持锁时不等队列。`chain_resolve` 不占会话队列，只拿该 repo 的写锁。`resolve_move` 占队列，不由 daemon 拿 repo 锁。
7. 拿到锁之后按锁里重新解析出的请求再推一次 `Action`，与 `token.action` 比。不同 → `NotConfirmed`，什么都不做（confirm.md D2）。`remove` 与 `new` 自己取锁，daemon 不持锁去调它们，所以这两条的 D2 在调用前做一次，锁内的 action 核对由 `remove` 自己做（它按值收凭据）。选择器在 `remove` 取锁之后又变的窗口，本文不另开锁去堵：堵了就与 `remove` 的取锁重入 [我的决定 D9]。
8. 调上游。回答一行 JSON。连接已断：不重放。结果留在 store 里，status 看得到。

`deps_approve` 与 `push`，以及 `rm` 且 `discard`，以及 `chain_resolve` 的 `Restore` 在仓库要确认时：都走第 4 步。初稿点了前两个；`rm --discard` 是 types.md 的第三种 `Action`，同一条顺序。`Restore` 用已有的 `PushDeploy`，不新造 `Action`。`resolve_move` 与接受 origin 不走第 4 步。

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

实现（review M2）：`OverlaySource::last` 不把 Tracker 上次缓存的 overlay 原样交出去，那是上次 checkpoint 时的样子，之后新建、删除、改名的文件和改过的忽略规则都看不到。每次 attach 时：会话队列此刻空闲（`Locks::try_session`，不等、不排队，仍守 I5）就借出 Tracker 跑 `compute(.., None)`，走全树但缓存让没变的文件不重读，不写 Store 也不写工作区，不依赖 FSEvents 已经送到（flush 不是屏障）；队列被别的请求占着就冷算一次 `reconcile_full`（只读）。都失败才是 None，按 search §5 当 `overlay_not_ready`。下一次 checkpoint 仍和 Store 里最新的 checkpoint 行比，所以搜索刷新缓存不会让 checkpoint 漏记 [实现时补的决定]。

**`diff`**：不排队。`publish::diff_between`。

**`deps_list`**：不排队。`open_proposals`。

**`publish_plan`**：不排队。`publish` 开头那一步重跑过滤（`rerun_plan`，即 D22 的按选择子、按内容判定）原样跑一遍，返回还要提交的仓库与 `already_published`，不写（review-2 N2）[实现时补的决定]。

**`deps_propose`**：锁外 `drift`，没有漂移不必提议。有则锁外 `build`。然后占 repo 锁（写前 30 秒）调 `propose`。`switch` 为真：先锁外 `ensure_base_for`，再占该会话队列、拿锁、`plan_switch` 与 `move_workspace`。建造都在占队列之前。

**`deps_approve`**：按 deps.md §5.12。不是 Pending → `NoSuchProposal`，不弹框。弹框在占队列之前，action 是 `DepsApprove { proposal }`。锁外 `ensure_target`；`Target::Replaced` 则什么都不移动，把替换提议的 id 交人，批准它是一次新的确认。Ready 时锁外备好基线 generation。然后按 `SessionId` 升序占齐候选会话的队列，再拿 repo 锁。锁里再 `get_proposal`：已被取代 → `NoSuchProposal`，`detail.state` 指向替换的那条，不自动批准。`P.head` 不是 `main_tip` → `HeadMoved`（reason `main_moved`），什么都没写，放开之后 `observe_main`，把现状交人重新确认。然后 `plan_rollout`，对每个 Move 调 `move_workspace`（调用方已持锁），再 `record_rollout`。某个会话 `OpenFiles` 是 data 里的一条，不是整次失败。冷装在队列和锁之外，所以 ToolCall 不等这次冷装。移动阶段持着队列，ToolCall 等移动做完，不等待被取消。

**`publish`**：见 §5.6。

**`push`**：按钉住的值要确认才弹框，action 是 `PushDeploy { repo, branch }`。核对的链顶是 cli 展示给人、随请求发来的 `shown`，不是 daemon 自己在弹框前读的那个（D21）。不占会话队列（不碰工作区）。拿 repo 锁。锁里再对 action，并核对链顶没变；变了 → `NotConfirmed`，不推，人重新确认（confirm.md §5.1 末尾交给本文的缺口）。然后 `publish::push`。`PushOutcome::Pushed` 之后，若 `publish::push` 还没把链和锚点写成那个 commit，daemon 在同一把锁里调用 publish.md @df9f069 §2 第 3 条的那一次 ref 事务，把两者一起设上。不拆成两次 `update_ref`。放开之后 §5.7。

实现（review M6）：单独 `push` 与 restore 的暂存目录 `<root>/daemon/push/<pid>-<ns>/` 在导出之前先写 `push.json`（`repo`、`commit`），作为这次尝试的持久记录。结果确定（`Pushed`、`PushRefused` 等）就删掉目录；结果不确定（`Unknown`）就保留目录，告警 `UnknownPush { repo, commit, scene }`，错误的 `detail.scene` 给出路径；重启时扫描这些目录再报一次。不重推。之后同一 repo 一次确定的 `Pushed`，其 commit 包含现场的 commit 时，现场算结清，删目录、撤告警。（review-2 N4）[实现时补的决定] `push.json` 另记 `remote`（`origin`）和 `branch`；只有同一 repo、同一 remote、同一 branch 的确定 `Pushed`（单独 `push`，以及 restore 成功的推送）才结清，且其 commit 须包含现场的 commit。别的分支的现场、以及 N4 之前写的、没有 remote / branch 的记录不自动结清，留给人处理。（review-3 R4）[实现时补的决定] `push.json` 再记仓库身份 `repo_id`（主 checkout common dir 的 realpath，`Git::repo_id`）与推送目的地 `push_url`（推送之前经 gitio `push_url("origin")` 读的，只读本地 config；读不到记 null）。结清要求：这次确定的 `Pushed` 推送之前读到的 `repo_id`、`push_url`，在推送之后重读仍相同，且与现场记录的两项逐字节相等，再加上同一 repo 名、remote、branch 与包含关系。所以 origin 的推送地址在一次不确定的推送之后被改到别的远端，推到新地址的确定推送不结清旧现场；同名而 common dir 不同的仓库也不结清；R4 之前写的、没有这两项的记录不自动结清。地址按字符串比，同一远端的两种写法（别名、`insteadOf` 改写前后）算不同，宁可留着给人处理。（review-4 F1）[实现时补的决定] 推送本身就推到现场记下的那个 `push_url`：单独 `push` 经 publish 的 `push_to`、restore 经 gitio 的 `push_recorded_to`，gitio 不再重读 config。所以记录里的地址就是这次推送实际去的地址，即使 origin 的推送地址在读之后、推之前被改（A→B），或推完又改回（B→A）。结清因此不再在推送之后重读 config 比较：确定的 `Pushed` 就落在记下的地址上，只要求现场的 `repo_id`、`push_url` 与之逐字节相等。读不到推送地址（记 null）时照旧由 gitio 自己读，这种推送不结清任何现场。（review-5 N1）[实现时补的决定] git 对命令行给的地址还会再套 `insteadOf` 改写，所以 gitio 把地址钉住（恒等改写规则；赢不了的 config 在发送前以 `InvalidInput` `url-rewritten` / `url-is-remote-name` 拒绝，见 gitio.md §5.11 第 3 步）。推送之后的核对恢复为兜底：重读的 `PushDest` 须仍等于推送之前的，且 `Git::push_url_exact(push_url)` 仍为真，否则不结清任何现场（保留、照报，留给人处理）。（review-6 漏洞 1）记下的 `push_url` 是裸名、而 common dir 下有老式 `remotes/<名>` / `branches/<名>` 文件时，git 会把它当 remote 名推到文件里的地址；gitio 同样以 `url-is-remote-name` 拒绝，`push_url_exact` 为 false。所以 A→B→A 仍结清 A 的现场（推完已改回 A），推送期间改成 B 或加了改写规则就不结清。（review-6 漏洞 2，D25）[实现时补的决定] gitio 的发送前检查与推送是两个 git 进程，检查之后加、推完又删的改写规则，推后核对查不出来；所以 gitio 让推送进程自己核对它真正要去的地址（守卫 `pre-push`，gitio.md §5.11 第 3 步），不等于记下的 `push_url` 就什么都不发，返回 `InvalidInput`（`url-rewritten`）。单独 `push` 与 restore 因此不会推到 B，旧的 A 现场不被结清。store 的 open 因此把 `daemon` 列为 root 里允许的名字（store.md §5.1 第 3 步）。[实现时补的决定]

**`assign`**：拿该 repo 的写锁，调 `publish::assign`。不占队列，不确认，不推送。

**`rm`**：`discard` 为真则先弹框，action `RemoveDiscard { session }`，不持队列和锁。然后占该会话的队列。不持 repo 锁地调 `session::remove`（它自己取锁）。`publishing` 为真时先等旗标，不等的时候不持别的锁。进行中的 checkpoint 不取消：队列让 `rm` 排在它后面。

实现（review M5）：`rm` 和 `resolve_move` 都走 `resolve::with_queue_not_publishing`：先不持任何队列和锁地等旗标，再占队列，在队列里再查一次旗标；又为真（publish 正在提交后的无锁间隙，如逐会话 rebase 之间）就交还队列重新等。整段等待合起来以一个锁等待为上限，超时 `Busy`（`reason: "publishing"`），什么都不写。[实现时补的决定]

（review-2 N5）“整段”是一个绝对截止时刻（开始 + 锁等待），每一轮的旗标等待和每一次占队列共用它（`Locks::with_sessions_until`：这一调用里的每次占用都以截止时刻为界，不只是请求的第一次；已有可见写之后照旧不设界）。之前第二轮占队列不设界，可能无限等。在旗标等待里到期是 `Busy` `publishing`；在占队列时到期是 `Busy` `lock_wait`（带持有者）。都不写任何东西。[实现时补的决定]

（review-3 R5）拿到队列位置之后、`f` 开始之前再看一次截止时刻：旗标恰好在最后一次轮询（已过期）时清掉，或者过期后以零时长拿到空闲队列，都不再让 `f` 晚开始，而是 `Busy` `lock_wait`（`deadline: true`），什么都不写。`f` 一旦开始就不再设界。零时长的 try-lock 语义（`try_session`）不变。[实现时补的决定]

**`gc`**：一个 repo 一把锁，调用方持有，调 `gc_repo(repo, GC_KEEP)`。`repo` 为 null 则按 `RepoName` 升序逐个。写前等锁；超时跳过这个 repo，记进 `daemon_status`，不让整次失败。不占会话队列，不等建造锁。

**`resolve_move`**：不查 TTY，不返回 `NotTty`，不调用 `Confirmer`，参数里没有凭据。cli 已经查过 TTY，并且人给过选择。`abandon` 与 `accept` 必须恰好一个为真；否则 RPC `Failed`，`resolve_failed_move` 不调用。`publishing` 为真时先等旗标，等的时候不持队列或 repo 锁。然后占该会话的队列。不持 repo 锁地调 `session::resolve_failed_move(session, repo, MoveResolution::Abandon 或 Accept)`（它自己取锁）。`Accept` 之后 `Tracker::reset` 再 `watch`。`Abandon` 不换工作区树，不 reset。不自动选。

**`chain_resolve`**：人对 `ChainState::Rewound` 或 `Diverged` 的显式命令。不查 TTY。不占会话队列（不碰工作区）。branch 是 `cfg.publish.branch`。链 ref 是 `publish::chain_ref(branch)`，锚点 ref 是 `refs/strata/anchor/<branch>`（与 `publish::anchor_ref` 同一字符串）。

1. `choice` 不是 `ChainChoice` 的三个变体名 → RPC `Failed`。
2. 还没拿锁时读一次 `publish_chain`。不是 `Rewound` 也不是 `Diverged` → `InvalidInput`，`detail.reason = "not_rewound_or_diverged"`，不 fetch，不改链 [我的决定 D19]。不加码。
3. `Restore` 且钉住的值要确认（`needs_human_confirm`）：与 `push` 相同。action 用已有的 `Action::PushDeploy { repo, branch }`，不新造变体。不持锁地 `require` 再 `validate`。凭据不出现在 RPC 里。接受 origin 的两个选择不推送，不弹框。
4. 拿该 repo 的写锁（写前 30 秒）。锁里 `fetch`，再读 `publish_chain`。到这里还没有锚点的比较并交换。分类在任何比较并交换之前：
   - 不是 `Rewound` 也不是 `Diverged` → `InvalidInput`（`not_rewound_or_diverged`），不写链，不写锚点，不推。
   - `Restore` 且 `git.is_ancestor(o, c)` 为假 → `InvalidInput`（`not_fast_forward`），不写锚点，不推。
   - `AcceptOriginReplay` 且下面算不出重放范围 → `InvalidInput`（`no_replay_range`），不写锚点。
   `Diverged` 的锚点前移会写 ref。先比较并交换再拒绝，具名拒绝就落在一次可见的写之后。所以拒绝在比较并交换之前。比较并交换一旦调用，这次命令后面的失败是 `Unknown`，不是 `InvalidInput`，也不是「`Conflict` 且 ref 没变」。
5. 分类通过之后才写 ref。链和锚点用 publish.md @df9f069 §2 第 3 条的那一次 ref 事务，一次调用里一起比较并交换。不拆成两次 `update_ref`。两次调用之间崩溃就是过期锚点的窗口。daemon 不起 git。
6. `AcceptOriginDrop`：不调用 `push`，不调用 `replay_commits`。那一次事务把链和锚点都设成 o，对旧的 c 和旧的 a 比较并交换。事务没完成 → `Unknown`，不推，不声称 ref 没变。
7. `AcceptOriginReplay`：不调用 `push`。范围是前移之后的 a..c，不是 o..c。前移后的 a 在内存里算，这一步还不写：会前移时 `m = git.merge_base(o, c)`，`is_ancestor(a, m)` 且 `m != a` 则 `exclusive_base` 是 m，否则是当前的 a。`Rewound` 不前移，`exclusive_base` 就是当前的 a。`replay_commits(exclusive_base, c, o)`。`Conflict` 只在还没有 ref 事务时返回，链和锚点的字节不变。成功之后那一次事务把链设成新 tip、锚点设成 o。事务失败 → `Unknown`。
8. `Restore`：第 4 步已经确认是快进，并且还没有锚点比较并交换。锁里再对 `PushDeploy`（要确认时），并核对 c 仍是弹框前看到的那个，且就是 cli 发来的 `shown`（D21）；变了 → `NotConfirmed`，不推。然后 `git.push(staging, "origin", chain_ref, branch, timeout)`。staging 用 `attach_staging` 接到 c，推完 `retire_dir`。timeout 与 publish 的 push 相同，本文不另定。argv 由 gitio 保证没有 `--force`、`--force-with-lease` 或 `+` 开头的 refspec；daemon 不另开一条强推路径。`Refused` 且链还没改 → `PushRefused`。`Unknown` → 报出，不重推，不接着做一次把失败说成具名拒绝的比较并交换。`Pushed` 之后才调用那一次 ref 事务，把链和锚点一起设成推上去的 commit。事务失败 → `Unknown`。

   实现（review M7）：不用普通的 `git.push` 再另开事务——那样跟踪 ref 由 push 自己单独推进（失败只记日志），锚点在后一个事务里，崩溃或读者会看到一个动了另一个没动。改用 gitio 的 `push_recorded`（`push_published` 加一组只比较的更新）：远端接受之后，同一个 ref 事务里核对链仍是 c、把跟踪 ref 与锚点一起设成 c，各自对推送前读到的值比较并交换；事务不成 → `Unknown`（本地什么都没动，暂存现场照 M6 保留）。[实现时补的决定]
9. 放开锁。`main_tip` 动了就 `observe_main`，失败只是 `ObserveFailed`。第 4 步的具名拒绝不被这次观察改成 `Unknown`。ref 事务已经调用过的，失败只能是 `Unknown`。

**`config_relax`**：显式接受一次调松。params 只有 `repo`。不查 TTY。不占会话队列，不拿 repo 写锁。文件变了不弹框：启动、重载、KeepAlive 重启都不调用 `require`。调紧仍立即写回，不要凭据。

1. 名字不在配置里 → `NoSuchRepo`，不弹框。
2. 该 repo 的文件并不比钉松 → `InvalidInput`，`detail.reason = "not_relaxed"`，不弹框 [我的决定 D20]。
3. 顺序：`require(Action::ConfigRelax { repo: RepoId })`；`validate` 核对 action 就是这个 repo 的 `ConfigRelax`，并核销凭据；然后才写 `pins.json`。action 由 daemon 从 `repo` 推出，不采用客户端传来的。不符、用过、过期 → `NotConfirmed`，钉不改。confirm 不写任何文件。这条路径能编译：变体在 types.md §4，本文不定义。重载把钉放松的仍然不应用，直到这条命令走完；调紧立即写回，不要凭据。不提供绕过。

### 5.6 publish

RPC `publish` 不自己选合并基 B，也不把 `stage`、`check`、`commit`、`push` 拼成临界区。B 由 `publish::publish` 内部的 `publish::merge_base` 决定（publish.md @df9f069 §5.2 第 2 步、D17）。`Rewound` 在任何 commit 或 push 之前以 `InvalidInput`（reason `chain_rewound`）拒绝，不前移锚点。`Diverged` 以 `InvalidInput`（reason `chain_diverged`）拒绝，不 `commit_tree`、不 `push`，链 ref 的字节不变。daemon 把这两个码原样交出去。自己选链顶会把被人拿掉的提交再推上去；自己选 origin 的 tip 会丢掉 a..c。

daemon 实现 `publish::PublishLocks`（`with_sessions`、`with_repo`），放进 `publish::Ctx`。对组里每个 repo 各调一次 `publish::publish(ctx, group, confirm)`。`confirm` 是已经 `validate` 过的 `PushDeploy`；这次不推，或不需要确认，则为 `None`。daemon 不在这次调用外面再占队列或 repo 锁：trait 不可重入，锁由 `publish::publish` 自己拿。

`PublishLocks` 守已有的顺序：会话队列按 `SessionId` 升序，再 repo 写锁；持锁时不等队列；持有队列或 repo 锁时不等建造锁。写前等 `LOCK_WAIT`，写后一直等。一次只进入一个 repo 的 `with_repo`（D15）。`ensure` / `build` 仍在占队列之前或放开之后，由 `publish::publish` 自己排（它的 §5.2 第 1 步与第 8 步）。daemon 不把这段再写一遍。这一点不改（「Lock order, extended」）。建造缺口里 checkpoint 可以跑、`rm` 仍等，仍是 D14，不重开。

`publish::publish` 返回之后，工作区换过基线的，daemon 对该工作区 `Tracker::reset` 再 `watch`。`ObserveFailed` 在 `Published.warnings` 里，不把发布改成失败。

### 5.7 fetch 之后

这一节只用于 `publish::publish` 外面的 fetch（`new` 返回之后）。`publish::publish` 自己在锁里前移，daemon 不再做一次。

分类在调用事务之前。`ChainState::Absent` 或 `Rewound`，或还没 fetch 到（`None`）→ 不调用。会前移时（`Equal`、`Leads`、`Behind`、`Diverged`）在内存里算 `m = git.merge_base(o, c)`（gitio 的，不是 `publish::merge_base`）。`is_ancestor(a, m)` 且 `m != a` 才写。

写用 publish.md @df9f069 §2 第 3 条的那一次 ref 事务：链保持原值，锚点设成 m，同一次调用。不拆成两次 `update_ref`。只改锚点时链也在同一次事务里（publish.md §5.4）。事务一旦调用，失败是 `Unknown`，不是 `InvalidInput`，也不再试一次然后声称锚点没动。`new` 已经成功的，这次 `Unknown` 记进告警，不把 `new` 改成失败。

放开锁之后，若 `main_tip` 与 fetch 前不同，调 `observe_main`（它自己取锁）。失败再调一次。仍失败 → 警告 `ObserveFailed`。`new` 内部已经观察过的，daemon 只在看到 `ObserveFailed` 时再试。两边都做是幂等的。

### 5.8 调松

命令是 `config_relax`（§5.5）。方法表里有这一项。confirm 不写 `pins.json`。路径能编译。文件变松仍然不生效，直到 `config_relax`；调紧立即生效。

### 5.9 小时对账与停机

每 `RECONCILE_EVERY`：每个工作区试着占该会话的队列（不等；占不上就跳过，记进 `daemon_status`）。占上之后借 `Tracker` 做一次不依赖事件的冷扫（`unchanged_since` 作保，不是把每个文件再哈希一遍），更新缓存，不写 checkpoint 行——`CheckpointReason` 没有「小时」这一项，不发明 [我的决定 D16]。放开 Tracker，放开队列。然后 gc，规则同 `gc` 方法。与请求共用同一把锁和同一条队列。

实现（review M1）：`handlers::serve` 在 accept 循环旁边起一条对账线程，与 accept 循环同生同灭，丢 Tracker 之前先 join。线程先做 §5.1 第 13 步（给每个 Active 会话里 tracked 的工作区建 Tracker 并 watch；队列被占的跳过，由持有它的请求自己建），然后每 `Options::reconcile_every`（生产为 `RECONCILE_EVERY`，测试缩短）跑一轮 `Trackers::reconcile_once`：`Locks::try_session` 不等、不排队；占上就 `compute(.., None)`（不带脏集合 = 走全树，事件整段没来也能发现），只更新缓存。每 20 ms 看一次停机标志，停机不等间隔；正在跑的一轮做完（之后的取锁按 D17 失败）。一轮 panic 记进 `daemon_status`，不拖垮 daemon。上一轮结果写进 `daemon_status.reconcile` [实现时补的决定]。

停机（SIGTERM）：停止 accept。在跑的请求做完。还在排队的以 `ReadOnly` 失败，`detail.reason = "shutting_down"`（types.md 没有 Closed 这个码，映射到已有的「写被拒绝」）[我的决定 D17]。在请求线程上丢掉 Tracker（从而丢掉 Watcher），不在回调里丢。放开全部 `Arc<Store>` 之后 `Store::close`（它写 `store/clean-shutdown`）。不写 `daemon.clean`。unlink socket。放开 flock。退出。

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
| `resolve_move` 两个布尔都真或都假 | RPC `Failed` | 无 | 只给一个选择 |
| `chain_resolve` 的 choice 不对 | RPC `Failed` | 无 | — |
| 链不是 Rewound / Diverged 却来 `chain_resolve` | `InvalidInput`（reason `not_rewound_or_diverged`） | 链不动，锚点比较并交换还没调用 | 不必处理 |
| 接受 origin | 不调用 `push`。drop 与 replay 的链和锚点同一次 ref 事务 | replay 在事务之前 `Conflict` 时 ref 不动；事务之后只有 `Unknown` | 人再决定要不要另一次 `push` |
| 锚点事务已调用之后失败 | `Unknown` | 不声称 ref 没变 | 不重放，不改成 `InvalidInput` 或 `Conflict` |
| 恢复但 o 不是 c 的祖先 | `InvalidInput`（reason `not_fast_forward`） | 无 push | 改用接受 origin。gitio 不强推 |
| 恢复要确认而凭据不符 | `NotConfirmed` | 无 push | 重新确认 |
| 上次推送不确定 | status 报出 `UnknownPublish`，不重放、不删行 | 行还在 | 人决定 |
| 崩溃留下的 Running 发布 | 先 `set_publish_phase(Running → Unknown)`，再当 Unknown 报出，不重放 | 行变成 Unknown | 人决定。留下 Running 会让下一次 publish 一直 Busy |
| `observe_main` 失败 | 警告 `ObserveFailed`，再试一次 | 调用本身已完成的留下 | 下次观察补上 |
| attach 越界 | `Unknown` 原样透传，加告警 | 现场留下 | 人看主 checkout 的 index，daemon 不改 |
| 隔离挪失败 | `Unknown`，告警 | 链接可能还在 | 不在该影子仓库上跑 git |
| 停机时还在排队 | `ReadOnly`（reason `shutting_down`） | 无 | 重启后重试 |
| daemon 没在跑 | cli 的只读命令仍可用；`publish` 与 `push` 由 cli 以 `ReadOnly` 拒绝 | 无 | 见 §7 |
| 处理器 panic，写前 | RPC `Failed`，锁已释放 | 无 | 进程继续 |
| 处理器 panic，写后 | `Unknown`，锁已释放 | 现场留下 | status 报出 |
| 配置调松 | 不生效，直到 `config_relax` 走完 require、validate、写钉。文件变了不弹框。调紧立即生效 | 钉住的值不变 | 人发 `config_relax` |
| TOML 或校验失败 | `InvalidInput` | 不听 socket | 改配置 |
| 两个 repo 同一个 `RepoId` | `NameTaken` | 不听 socket | 改配置 |
| `path` 不是 realpath | `InvalidInput`，detail 带 realpath | 不听 socket | 改成 realpath |

> daemon 没在跑时写命令的码以 cli.md D2（2026-10-23）为准：`Busy`，`detail.reason = "daemon_down"`；上表与 §7 `cli_readonly_commands_work_without_daemon` 里的 `ReadOnly` 不再沿用。

结果不确定的按不变量 6：保留现场，不重放，不清理，status 报出。daemon 重启后同样报出。

## 7. 测试清单

| 测试名 | 断言 | 来源（矩阵行 或 新增） |
| --- | --- | --- |
| `concurrent_checkpoints_same_session_serialize` | 同一会话两个 checkpoint 同时到，第二个排队，两个都完成，顺序与到达一致，没有一个被取消；types.md 里没有 InFlight 这个码 | `layerfs-daemon/tests/store_commit.rs::a_held_producer_does_not_hold_workspace_metadata_or_read_capacity` |
| `rm_queued_behind_in_flight_checkpoint` | checkpoint 进行中收到 `rm`，`rm` 在该 checkpoint 完成之后才执行，不取消它，不返回 `Busy` | `layerfs-daemon/tests/store_commit.rs::a_held_producer_does_not_hold_workspace_metadata_or_read_capacity` |
| `unknown_publish_reported_on_restart_not_replayed` | 一行 `Unknown` 重启后仍在 status 里；一行 `Running` 被改成 `Unknown` 再报出；两种都不调用 `git push`，也不重放 | 新增 |
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
| `config_relax_does_not_prompt_on_file_change` | 只改 `pins` 所对的配置文件、不发 `config_relax`：`require` 调用次数为 0。调紧立即写回，不要凭据 | 新增 |
| `config_relax_writes_pin_after_validate` | 顺序是 `require`，然后 `validate` 核销，然后才写 `pins.json`。validate 失败则钉的字节不变。confirm 不写这个文件 | 新增 |
| `hint_and_flush_off_callback_thread` | FSEvents 回调里只有 `Inbox::note`。`hint`、`flush`、丢弃 `Watcher` 都发生在请求线程或停机线程。回调里不 `dispatch_sync` 到 Watcher 的队列 | 新增 |
| `release_daemon_has_no_test_seams` | `TEST_SEAMS` 为真的二进制拒绝启动，不听 socket | 新增 |
| `long_hold_named_in_status` | 持锁超过 30 秒时，`status` 写出方法、repo 与开始时间 | 新增 |
| `resolve_move_exclusive_choice` | `abandon` 与 `accept` 都真或都假 → RPC `Failed`，`resolve_failed_move` 没被调用。恰好一个为真才调用，选择与布尔对应 | 新增 |
| `resolve_move_skips_tty_and_confirm` | 不返回 `NotTty`，不调用 `Confirmer`。参数里多一个凭据字段 → RPC `Failed` | 新增 |
| `resolve_move_queues_and_does_not_hold_repo_lock` | 占该会话队列之后才调用；调用时 daemon 不持有 repo 写锁。`Accept` 之后 `Tracker::reset` 再 `watch` | 新增 |
| `chain_resolve_accept_origin_does_not_push` | `AcceptOriginReplay` 与 `AcceptOriginDrop` 的全程 `git.push` 调用次数为 0。drop 与 replay 各只调用一次 ref 事务，不是两次 `update_ref`。replay 的范围是前移之后的 a..c，不是 o..c。事务之前 `Conflict` 时链的字节不变 | 新增 |
| `chain_resolve_restore_pushes_only_fast_forward` | `is_ancestor(o, c)` 为假 → `InvalidInput`（reason `not_fast_forward`），`git.push` 不调用。为真才 `push` 一次，src 是链 ref，dst 是 `publish.branch`。没有第二条强推路径 | 新增 |
| `chain_resolve_restore_is_pinned_against_url_rewriting` | （review-5 N1）现场记下 A 之后（导出暂停时）加一条把 A 改写到 `alt/<名>` 的前缀 `insteadOf`：commit 落在 A、alt 不变；推后核对看到 origin 的目的地变了，原有指向 A 的现场不结清。修之前推送落在 alt | 新增 |
| `standalone_push_refuses_a_url_rewritten_while_staging` | （review-5 N1）单独 `push` 在导出暂停时加一条匹配串恰好是 A 的 `insteadOf`（指向 B）：返回 `InvalidInput`（`url-rewritten`），A、B 都不变，原有指向 A 的现场不结清。修之前推送落在 B、A 的现场被结清 | 新增 |
| `chain_resolve_restore_rule_added_between_check_and_push_is_refused` | （review-6 漏洞 2）restore 推送时，在 gitio 检查之后（`pause_push_after_check`）加一条 `url.<B>.pushInsteadOf=A`，推送之后、推后核对之前（`pause_push_before_record`）删掉：restore 失败，A、B 都不变，原有指向 A 的现场保留。去掉守卫时推送落在 B、restore 报 `pushed`，测试失败 | 新增 |
| `chain_resolve_restore_pushes_to_the_recorded_url_across_a_b_a` | （review-4 F1）现场记下 A 之后（导出暂停时）把 origin 的 `pushurl` 改成 B，推送记录之前（`pause_push_before_record`）再改回 A：commit 落在 A、B 不变，原有指向 A 的现场被结清。修之前推送落在 B、A 不变 | 新增 |
| `chain_resolve_restore_checks_push_deploy` | 钉住的值要确认时，恢复走与 `push` 相同的 `Action::PushDeploy` 核对；不符不推。接受 origin 不弹框 | 新增 |
| `chain_resolve_refs_under_repo_lock` | fetch、那一次 ref 事务、以及恢复的 `push` 都在该 repo 写锁的闭包里。分类没通过时 ref 事务的调用次数为 0 | 新增 |
| `publish_calls_publish_not_stage` | RPC `publish` 调用 `publish::publish`，不调用 `stage`、`check`、`commit`。`Rewound` / `Diverged` 原样交出 `InvalidInput`（`chain_rewound` / `chain_diverged`），daemon 不选链顶，也不选 origin 的 tip 当 B | 新增 |

`second_exporter_waits_then_busy_never_exists` 不在本表。后写的裁定把它交给 session 的登记时去重（「T13 / T10 notes (from gitio 60b1cf5)」），本文不再写。

3d `interrupted_transfer…` 的并发导出半边同样归 session，不归本文。

## 8. 实现任务（TDD）

### Task 1: RPC 信封与方法表

**Files:**

- Create: `crates/strata-daemon/src/rpc.rs`
- Test: `crates/strata-daemon/tests/rpc.rs`

**Interfaces:**

- Consumes: 无上游。`Code` 不增加
- Produces: §3 的信封、`RpcReject`、`MAX_FRAME`、表里的 19 个方法名

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
- Produces: 单例锁、§5.1 的顺序。不产生 `daemon.clean`

- [ ] **Step 1: 写失败的测试**：`unknown_publish_reported_on_restart_not_replayed`、`second_instance_refused_then_reacquired`、`quarantine_is_an_alert`、`release_daemon_has_no_test_seams`、`daemon_does_not_idle_exit`、`idle_close_is_not_ours`、`no_linux_install_path`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-daemon --test startup`

Expected: FAIL

- [ ] **Step 3: 写让测试通过的最少实现**：§5.1。gc 在 recover 之后、listen 之前。残留的 `PublishPhase::Running` 改成 `Unknown`，然后报出，不重放。
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

- [ ] **Step 1: 写失败的测试**：`config_relax_not_applied_on_restart`、`config_relax_does_not_prompt_on_file_change`、`config_relax_writes_pin_after_validate`，外加：弹框时不持有队列或 repo 锁；action 不符 → `NotConfirmed`；daemon 不返回 `NotTty`；`InstallEnv.path` 等于 D1；plist 含 `Label`、`KeepAlive`、`LimitLoadToSessionType = Aqua`、D1 的 PATH。
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-daemon --test confirm_flow`

Expected: FAIL

- [ ] **Step 3: 写让测试通过的最少实现**：§5.3 第 4 步、§4 的钉、§5.5 的 `config_relax`、§3 的 plist。不在本文定义 `ConfigRelax`；变体已在 types.md，这条路径能编译。重载把钉放松的仍然不应用，直到 `config_relax`；调紧立即生效。
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

- Consumes: `publish_chain`、`main_tip`、`observe_main`、publish.md @df9f069 §2 第 3 条的一次 ref 事务、`gitio::merge_base`、`is_ancestor`
- Produces: §5.7、`DaemonWarning::AttachOutside`、`AnchorNotAdvanced`

- [ ] **Step 1: 写失败的测试**：`anchor_advances_after_fetch`、`observe_failed_does_not_fail_new_or_publish`、`hint_and_flush_off_callback_thread`，外加 attach 两个 reason 的告警测试（不改 index），以及 `PrePushGuardDisabled` 只报告、不写影子 config。
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-daemon --test fetch_after`

Expected: FAIL

- [ ] **Step 3: 写让测试通过的最少实现**：§5.7、§5.10。锚点与链同一次 ref 事务，不拆成两次 `update_ref`。
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

### Task 7: `resolve_move` 与 `chain_resolve`

**Files:**

- Create: `crates/strata-daemon/src/resolve.rs`
- Test: `crates/strata-daemon/tests/resolve.rs`

**Interfaces:**

- Consumes: `session::resolve_failed_move`、`MoveResolution`、`publish_chain`、`gitio::is_ancestor`、`gitio::push`、`gitio::fetch`、publish.md @df9f069 §2 第 3 条的一次 ref 事务、`Git::replay_commits`（D18）、`Action::PushDeploy`、`publish::publish`
- Produces: `ChainChoice`。不加 `Code`

- [ ] **Step 1: 写失败的测试**：`resolve_move_exclusive_choice`、`resolve_move_skips_tty_and_confirm`、`resolve_move_queues_and_does_not_hold_repo_lock`、`chain_resolve_accept_origin_does_not_push`、`chain_resolve_restore_pushes_only_fast_forward`、`chain_resolve_restore_checks_push_deploy`、`chain_resolve_refs_under_repo_lock`、`publish_calls_publish_not_stage`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-daemon --test resolve`

Expected: FAIL（方法未定义）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.5 的两段。接受 origin 的路径里没有 `git.push`。恢复只在 `is_ancestor(o, c)` 时推，argv 交给 gitio，daemon 不加强推参数。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-daemon --test resolve`

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-daemon/src/resolve.rs crates/strata-daemon/tests/resolve.rs
git commit -m "feat(daemon): resolve_move and chain_resolve"
```

## 9. 构建者决定记录

| 决定 | 选择 | 理由 | 日期 |
| --- | --- | --- | --- |
| D1 显式 PATH | `/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin` | launchd 没有用户 PATH。这台机器的 bun 与 git 在 Homebrew。`~/.bun/bin` 与 `~/.cargo/bin` 不在缺省里；staff-api 的 php 在配置里写绝对路径。配置以后可以覆盖 | 2026-10-23 |
| D2 影子仓库漂移只报告 | 不重钉 `hooksPath`，不要求 gitio 加函数 | gitio @1cd5ac5 写明不修。重钉会与 husky / lefthook 对打，并且要往活的影子仓库写。status 已有 `MirrorDrift` | 2026-10-23 |
| D3 帧上限 | 16 MiB，超过 → `TooLarge` | types.md 把帧上限指给 daemon，又不许加码。16 MiB 装得下一次大的 diff，又挡得住不换行的灌入 | 2026-10-23 |
| D4 params 未知字段 | RPC `Failed` | 打错的字段若被忽略，cli 与 daemon 会各跑各的。冻结表的字段就这些 | 2026-10-23 |
| D5 gc 保留期 | 14 天 | design §7 交给构建者。store 的 `keep` 要一个值；Removed 会话与 BeforeRemove 的内容留两周 | 2026-10-23 |
| D6 残留的 Running | 启动时 `set_publish_phase(Running → Unknown)`，然后当 Unknown 报出，不重放 | store 允许这条边。留下 Running 不是不变量 6 的不确定状态，下一次 publish 会一直 Busy，没有命令能清。旧的「不改行」撤回 | 2026-10-23 |
| D7 plist 的 RunAtLoad 与 ProcessType | 都写上 | `KeepAlive` 与 `Aqua` 是裁定。没有 `RunAtLoad`，bootstrap 之后不一定起来。`Interactive` 让认证框画得出。Label、PATH、socket 路径不是这条决定 | 2026-10-23 |
| D8 写前与写后的等待 | 同一 RPC 里第一次 `with_write` / 占队列在 `visible == false` 时有界，之后无界 | trait 上没有 `note_visible_write`，邻居不能反向依赖 daemon。前提：第一次取锁在第一次可见的写之前。打破前提时走 §2 的上游要求 | 2026-10-23 |
| D9 `remove` 的锁内再核对 | 调用前核对一次；锁内由 `remove` 核对 action | session 要求调用时不持锁，锁又不可重入。confirm.md D2 的选择器窗口留着，不发明 session 没有的回调 | 2026-10-23 |
| D10 按需 quick_check | 只有 `daemon_status` 的 `verify: true` 调用 `quick_check` | 启动时的检查是 `Store::open` 看 `store/clean-shutdown`。标记不在它就跑，失败返回 `Corrupt`，daemon 不听。daemon 不再建 `daemon.clean` | 2026-10-23 |
| D11 钩子等 checkpoint | 做完再回答 | 先应答会把下一次调用的早期写入算进这一次。没有路径清单的冷扫大约 0.20 秒，超出 200 ms，阶段结束时告诉用户 | 2026-10-23 |
| D14 publish 的建造缺口 | 建造前放开队列与 repo 锁；`rm` 仍等 `publishing`；checkpoint 在缺口里可以跑 | 同时满足「不持队列等建造锁」和「不取消进行中的 publish」。3d 那条在 commit/push/move 阶段仍是排队 | 2026-10-23 |
| D15 一次一把 repo 锁 | 按批次里的 repo 顺序，做完一个再拿下一个 | 一组跨 repo 时两把锁没有裁定过的顺序。一次一把不会死锁 | 2026-10-23 |
| D16 小时对账不写 checkpoint | 只刷新 Tracker 缓存 | `CheckpointReason` 没有小时这一项。发明一种会写出人没要的检查点 | 2026-10-23 |
| D17 停机时排队的写 | `ReadOnly`，reason `shutting_down` | types.md 没有 Closed。`ReadOnly` 含「写被拒绝」。映射说在这里 | 2026-10-23 |
| 对账间隔 | 1 小时 | 初稿已定，落实 design §6.2 交给构建者的间隔 | 2026-10-23 |
| 第二个进程的码 | `Exists`，不是 `Busy` | 矩阵写明码由本文定，且 `Busy` 只用于写锁。`Exists` 是「要建的已经在」里最近的一个 | 2026-10-23 |
| D18 重放函数 | `Git::replay_commits(exclusive_base, tip, onto) -> Result<CommitOid>` | 见下。gitio @1cd5ac5 没有这个函数。daemon 不起 git | 2026-10-23 |
| D19 链处理的 reason | `not_rewound_or_diverged`、`no_replay_range`、`not_fast_forward`，码都是 `InvalidInput` | 控制器不许加码。这三种都是写链或推送之前的具名拒绝 | 2026-10-23 |
| D20 没有待调松仍叫 `config_relax` | `InvalidInput`，reason `not_relaxed`，不弹框 | 文件变了不弹框。没有待接受的调松时，显式命令也不该弹出一个空框 | 2026-10-23 |
| D21 确认绑定展示过的链顶（review H2） | `push`、`publish`、`chain_resolve` 带 cli 展示过的链顶 `shown`（没展示链 = `null`，缺省同 `null`）。要确认时，repo 锁里读到的链顶不是它 → `NotConfirmed`（reason `chain_moved`，detail `confirmed`/`now`），不暂存、不提交、不推。`publish` 在 `strata_publish::publish_bound` 里、fetch 之后核对 [实现时补的决定] | 凭据只有 `PushDeploy { repo, branch }`，不含提交；只比 daemon 自己弹框前读的链顶，盖不住"cli 展示之后、daemon 读之前"那段，人会推出没见过的提交 | 2026-10-23 |
| D23 推送去记下的地址（review-4 F1） | 单独 `push` 与 restore 先读 `PushDest`、写进 `push.json`，再把其中的 `push_url` 原样交给 gitio 推送（`push_published_to` / `push_recorded_to`）；结清只看这个地址，不在推送之后重读 config | review-4 F1：gitio 原先在发送前再读一次 config，期间地址变了（A→B，或 A→B→A）会让记录与实际目的地不一致，进而留错现场或结清错现场。一次读、一处用，记录就是真相 | 2026-10-24 |
| D24 钉住推送地址、恢复推后核对（review-5 N1） | gitio 推送时把记下的 `push_url` 钉住，不让 git 再按 `insteadOf` 改写；结清之前重读 `PushDest` 须不变、`push_url_exact` 须为真 | review-5 N1：git 对命令行地址仍套改写规则，推送可能去了 B 而记录写着 A；D23 去掉了推后重读，改写规则就会让旧的 A 现场被错误结清。钉住修根因，推后核对兜底查与推之间的窗口 | 2026-10-24 |
| D25 检查与推送用同一份 config（review-6 漏洞 2） | gitio 推送子进程带守卫 `pre-push`（经 `GIT_CONFIG_COUNT` 设 `core.hooksPath`，转给人的钩子之前撤掉）：git 读完 config、算好改写、发送之前，守卫核对它要去的地址等于记下的 `push_url`，不等就什么都不发（`InvalidInput` `url-rewritten`），等于再跑人自己的 `pre-push`。另把老式 `remotes/<名>`、`branches/<名>` 文件也算作同名 remote（review-6 漏洞 1） | review-6：检查（一个 git 进程）与推送（另一个）之间加、推完又删的规则能把推送带到 B 并让 A 的现场被结清。不可变快照做不全：仓库自己的 config 没有环境变量能换，要冻结只能换私有 git 目录，会改变钩子与凭据的环境；在推送进程内、它唯一一次 config 读取之后核对实际目的地，是同一份 config，没有窗口，且覆盖所有来源。代价：被改写时 git 已连上 B 读 ref 广告（不发对象）（review-7 起人的钩子与其中再起的 git 看不到守卫的覆盖） | 2026-10-24 |
| D22 冲突后修好重跑（review H4） | 仍按仓库逐个发布，不做跨仓库原子（用户 2026-10-23：主要用户是 agent，冲突 → 修 → 继续）。某个仓库失败，错误 detail 带 `repo`（停下的仓库）和 `published_before`（这次已提交的仓库及 commit / push）。重跑同一个批次：按选择子逐个判断，用 stage 自己的选择规则（`strata_publish::selection::selects`，只读查询，daemon 不调 stage：精确路径、改名来源、目录下的一切）（review-2 N1）。选中了当前 checkpoint 任何条目的选择子（新内容或改过的内容，目录选择子下新加的文件也算）照常交给 stage；什么都没选中的选择子（会话在那里的内容等于基线），只有在“同一会话、同一仓库、最新一条 `done` 且有 commit、提交过它之下文件（`outcome.paths`，且在那次该会话的某个选择子之下）的 publish 行”里，这些文件在那个 commit 里的 mode 和 blob 与 checkpoint 基线完全相同，才算已经落地并从批次里去掉——按内容比，不按路径名。一个片段的选择子全部去掉就跳过，写进回复的 `already_published`；部分去掉的，剩下的选择子照常发布（落地了 {a, b}、之后只改了 a，重跑只提交 a，不会因 b 而 `NoMatch`）。内容不同（例如落地后别人在上游又改了它）或从没落地的、拼错的选择子，留在批次里，stage 照常 `NoMatch` [实现时补的决定]。会话没被 rebase（Waiting）时 publish 返回 `NoCommit`，若该仓库的每个片段都被同一会话的某条 `done` 行覆盖（选择子相等或一个在另一个之下），也算 `already_published`（`NoCommit` 说明与链上内容无差别，不丢内容）。全部落地 → `ok`，`published` 为空。跳过的仓库不要确认 [实现时补的决定] | 第一次冲突后重跑，已经落地的仓库 checkpoint 已被 rebase 清空，整次 `NoMatch`，agent 卡住；或者重复提交 | 2026-10-23 |

D18 的契约，实现者不要猜：

- 范围是从 `tip` 可达、从 `exclusive_base` 不可达的提交。`chain_resolve` 的 `AcceptOriginReplay` 把 `exclusive_base` 设成前移之后的 a（内存里按锚点规则算出的那个，还没写），`tip` 是 c，`onto` 是 o。不是 o..c：那会把 origin 已经有的提交再放一遍。
- 顺序是 a..c 上从旧到新。每个新 commit 的第一父是上一个新 commit，第一个的父是 `onto`。
- 合并提交只走第一父那一条：重放成单父 commit，tree 用那个合并提交的 tree，不重新合并。这一段相对它第一父的 diff 合不上新父 → `Conflict`。
- 每个 commit 用 `user.useConfigOnly=true`。身份没配 → `InvalidInput`，`detail.reason = "git-identity"`，与 publish 造 commit 的规则相同。
- 对象进主 checkout 的对象库（`Git::open` 开在主 checkout 上的那一个）。不进影子仓库。
- 临时工作树若需要，由 gitio 自己建、自己收。daemon 不建，不起 git。
- 只造 commit。不改 ref，不推送，不强推。`Conflict` 时 ref 的字节不变。ref 事务是 daemon 在这之后另一次调用，不在这个函数里。
