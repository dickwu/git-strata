# `publish` 模块

**Crate**：`crates/strata-publish` · **设计来源**：design.md §4、§6.3、§6.5、§6.7、§6.9、§8、§9 不变量 3、4、6、7 · **矩阵**：目标模块为 `publish` 的 43 行（3a 1、3c 7、3d 34、3e 1），以及理由点名 publish 的 5 行 `—`（3c 2、3d 3）；3b `:26` 里的 Publish 是 LayerFS 的步骤名，不是本模块，不引用 · **邻居版本**：gitio.md @1cd5ac5，deps.md @dafdefc，session.md @a324f9c，confirm.md @229f4e3，overlay.md 与 store.md @08ffa53，types.md 与 config.md 同 `docs/module-docs`（08ffa53）

> 本文按 module-rulings.md 写：「T10 publish」，以及标了 T10 或 Everyone 的增补。后写的、标了 amended 的裁定覆盖先写的。发布链的判定不在本文重写：`deps::publish_chain` 是唯一一份（"The chain predicate lives in deps"）。锚的前移按 "The anchor follows origin along the chain — amended"。fetch、push 与链/锚点事务都在 repo 写锁里（"Every update of a ref main_tip reads runs under the repo lock"）。建造依赖集在占队列和拿锁之前，或在放开它们之后（"Lock order, extended"）。第一次可见的写之后的等锁不受 `ctx.env.timeout` 截断（"Waits after a call's first visible write are not bounded"）。`observe_main` 失败只是警告 `ObserveFailed`，发布照常成功。`ConfirmToken` 只引用 types.md §4，不重新定义，也不调用 confirm。不加错误码。`PushOutcome` 只引用 types.md，JSON 是 `kind` + `message`。

## 1. 职责与边界

publish 把一个 `ReleaseGroup` 变成真实 git 仓库里的一个 commit，写在发布链 `refs/strata/publish/<branch>` 上，按配置推送，然后把参与的会话接到新基线。

| 负责 | 对应 design / 裁定 |
| --- | --- |
| 按路径、hunk 级的三方合并；一次发布组里每个 repo 至多一个 commit | §6.5 第 3 步、§6.7 |
| 合并基线 B：只在 `publish_chain` 为 `Leads` 时取链顶，否则取 origin 的 tip | §6.7；"The chain predicate lives in deps" |
| 锚点 `refs/strata/anchor/<branch>` 与链在同一段 repo 写锁里一起改 | "The publish chain carries an anchor" |
| 真实目录上的只读检查；会改文件的命令在跑之前拒绝 | §6.5 第 4 步、config.md §2.3 |
| `commit_tree` 与比较并交换地前进发布链；按配置推送 | §6.5 第 5 步、§6.7 |
| 推送前核对 daemon 交来的 `ConfirmToken.action` | §6.9、不变量 7 |
| 冲突时停下整组，`assign` 点名解决者 | §6.5 第 3 步 |
| 发布后 `rebase_session`；依赖集必须变时经 `deps::propose` 等待 | §6.5 第 7 步、不变量 4 |
| `diff_between`：两边都碰的路径给出三方合并预览 | §6.3 |

明确不负责的事，以及交给谁：

| 事项 | 归谁 |
| --- | --- |
| 链领先、相等、落后、回退、分叉的判定 | deps（`publish_chain`）。本文只调用 |
| 锚点 ref 的比较并交换、`--index-output`、clean 过滤器、`attach_staging` | gitio。本文不另写一份 index 配方 |
| 签发、核销凭据；TTY | confirm 签发与核销，cli 做 `require_tty`。本文只核对 `action` |
| 工作区目录的原子交换 | session（`move_workspace`） |
| overlay 的计算、`.lww` 规则文件的语法、rebase 投影 | overlay |
| 发布记录的表 | store。本文采用它暂定的 `Running` / `Done` / `Unknown`，不改词汇（增补 4） |
| 占队列、有界等待、重启后报出 `Unknown` | daemon。本文给出它必须守的顺序 |
| 接受 origin 或把链原样推回去的显式命令 | cli（T14）。本文不自动做（D2）。接受 origin 重放的范围是锚点前移之后的 `a..c` 接到 o 上，不是 `o..c` |

## 2. 依赖

直接依赖的 workspace crate：

| crate | 用到的 | 理由 |
| --- | --- | --- |
| `strata-types` | `ReleaseGroup`、`Batch`、`PublishTarget`、`PushOutcome`、`ConfirmToken`、`Action`、`Code`、`StrataError`、`RepoConfig`、`RepoPath`、`CommitOid`、`TreeOid`、`BaseGen`、`SessionId`、`RepoName`、`RepoId` | types.md 是跨模块名字的唯一来源。`PushOutcome` 与 `ConfirmToken` 在 types.md 定义，本文不重新定义（"Everyone (from the T1 re-review)"） |
| `strata-deps` | `publish_chain`、`PublishChain`、`ChainState`、`main_tip`、`propose`、`ProposeArgs`、`key_of`、`is_input`、`open_proposals`、`observe_main`、`ensure`、`Ctx` | 链的判定只有一份。落地与等待按 deps.md @dafdefc §5.12 |
| `strata-gitio` | `fetch`、`read_ref`、`is_ancestor`、`merge_base`、`remote_ref`、`tree_of`、`export_tree`、`attach_staging`、`stage`、`commit_tree`、`merge3`、`is_binary`，以及还没有的一次 ref 事务（§2 第 3 条）、`blob_bytes`（第 5 条）、记录 `Pushed` 时同时写远端跟踪 ref 和锚点的那一次调用（第 6 条）。不用两次 `update_ref` 写链和锚点 | gitio.md @1cd5ac5 的调用表；`blob_bytes` 在 @f16bb0d 仍没有。git 子进程只经这些函数 |
| `strata-overlay` | `compute`、`rebase`、`RebaseOptions::for_move`、`lww_matches`、`LwwRules::parse`、`Overlay`、`MERGE_MAX_BYTES`。`LwwRules::load` 只在 rebase 经 `for_move` 用，发布时的规则不从导出目录读 | 发布后投影。批次内容来自已记录的 checkpoint，不来自现场目录 |
| `strata-session` | `move_workspace`、`ensure_base_for`、`status` 读到的 `WorkspaceState` | 交换由 session 做。`ensure_base_for` 在锁外 |
| `strata-store` | `record_publish`、`set_publish_phase`、`publish_record`、`open_publishes`、`record_ref_change`、`checkpoint`、`get_blob` | 发布行与 ref 审计。本文不写 SQL |
| `strata-apfs` | `mkdir_rooted`、`open_rooted`、`rename_rooted`、`remove_rooted`、`symlink_rooted`、`get` 侧经 store 的 `get_blob_rooted` | 写 staging 不跟随符号链接（增补 8）。本文不自己调 `*at` |

不依赖 confirm、search、cli、daemon 的 crate。daemon 把锁与队列交给本文，本文不实现它们。

**谁调用本模块**

- **daemon**（daemon.md 还没写）：`publish`、`push`、`assign`。它在调用前做这些事，本文把它们当契约：
  1. 需要确认时，cli 已做 `require_tty`；daemon 已让 confirm 签发并核销 `Action::PushDeploy { repo, branch }`，再把凭据按值交来。本文不再调用 confirm。
  2. 先按 `SessionId` 升序占住本组要移动的会话的队列位置，再拿 repo 写锁。持着 repo 写锁时从不等队列（"Lock order: session queue before repo lock"）。
  3. 同一会话的 `rm` 或 checkpoint 若在本次发布进行中到达，排在 repo 写锁后面，发布完成之后再跑，不取消本次发布，没有 `InFlight` 这个码（增补 1 的 T10/T13 条）。
  4. 重启时读 `open_publishes`，把 `Running` 改成 `Unknown` 并报出，从不重放（store.md §5.9；T13）。
  5. 任何让 `main_tip` 移动的 fetch 之后调 `observe_main`；观察失败再观察一次。那次锚点前移由 daemon 在同一段锁里做（deps.md §2 第 6 条）。本文只规定自己这次调用里的前移。
- **cli**（cli.md 还没写，T14）：`publish`、`push`、`assign`、`diff --between`。被回退或分叉的链由显式命令处理（接受 origin，或恢复）。接受 origin 把锚点前移之后的 `a..c` 重放到 o 上，不是 `o..c`，所以不会把 origin 已经有的提交再放一遍。恢复只在 `is_ancestor(o, c)` 时是快进；gitio 从不强推。本文不实现那两条命令。`pin_unpushed` 只在 `publish.branch` 等于 `default_branch` 时有意义，本文不钉新会话。

**对 deps 的契约**（deps.md @dafdefc §2 对 publish 的六条，加上 §5.12）：带目标的 `propose` 在 repo 写锁里调用。落地由本文判定（§5.8）。`observe_main` 在放开锁之后调用，失败只报 `ObserveFailed`。链与锚点的写、fetch、push 都在 repo 写锁里。`ensure` / `build` 在占队列之前，或在放开队列与锁之后。

**对 gitio 的用法**（gitio.md @1cd5ac5 §5.9，不另写 index 配方）：冲突在内存里判定，B 上的字节与 `.lww` 经 §2 第 5 条的单路径读取，不导出。通过之后才 `export_tree(B 的 tree, staging)` → `attach_staging(staging, gitdir, B)` → 写入合并后的文件 → `stage(staging, 批次路径)` → `commit_tree` → 一次 ref 事务 → 记录 `Pushed` 的那一次调用（§2 第 6 条）。index 只经 `attach_staging` 与 `stage`。不传第二套 `GIT_INDEX_FILE` 或 `--index-output`。链与锚点不拆成两次 `update_ref`。publish 不起 git。

**对 store**：采用 `PublishPhase { Running, Done, Unknown }`，不要求改词汇（增补 4）。`record_publish` 在第一次可见的写之前调用，它是本次操作自己的意图行，不算可见的写（增补 3）。Removed 的会话被 `record_publish` 拒绝（`NoSuchSession`）；本文在那之前就占住队列，所以 gc 拆批的窗口里插不进这次发布（"A Removed session takes no new pins"）。

**上游要求**（对方模块还没有、本文需要的）：

1. daemon 的会话队列：按 `SessionId` 升序一次占多把，持着 repo 写锁时不能再等。本文不定义这个类型。
2. daemon 在 `attach_staging` 或 `push` 返回 `Unknown` 且 `detail.reason` 为 `staging-changed` 或 `index-read-back-refused` 时，向人告警并点名 `<main>/.git/index`（增补 8 的拼写终稿是连字符）。本文把这个 reason 原样放进自己的 `Unknown.detail`。
3. gitio 提供一次 ref 事务，在一次调用里比较并交换地写下链和锚点（`update-ref --stdin` 或等价物）。gitio @1cd5ac5 还没有这个调用。publish 不起 git，也不用两次 `update_ref` 代替。本地提交时链的前进和锚点的写走这一次调用。崩溃留下的链与锚点不一致，不靠把过期的锚点读成 `Leads` 来补上；status 报出这个不一致。记录 `Pushed` 的锚点不走这之后的另一次调用（第 6 条）。
4. `assign` 写在这次发布的 `publishes` 行上（`outcome.assigned`）。store 若不能在 `Done` 之后改这一列，由 store 加这一笔写。不另建文件。
5. gitio 提供 `blob_bytes(tree, path) -> Result<Option<TreeBlob>>`，`TreeBlob { bytes, mode }`。从一个 tree 读一个路径，不建目录。路径不在 tree 里为 `None`，不是目录。`mode` 是 git 的 `100644`、`100755` 或 `120000`；符号链接是 `120000`，字节是目标，不是一个内容碰巧是目标的普通文件。路径在 tree 里是目录时不返回文件字节，也不在磁盘上建目录。gitio @f16bb0d 没有这个调用。publish 用它读 B 上的文件字节、可执行位和符号链接，以及 `.strata/lww`（含 `%include`，最多 16 个文件，然后 `LwwRules::parse`）。不用 `export_tree` 来判定冲突或取 mode，也不用 `LwwRules::load` 去读一份导出。
6. 记录 `Pushed` 的那一次 gitio 调用，同时比较并交换远端跟踪 ref（`refs/strata/remotes/origin/<branch>`）和锚点，都设成推上去的 commit。publish 不起 git。这次调用提交之前，推送没有完成，不返回 `Pushed`。远端已经收下、这次调用还没提交就崩溃：结果是 `Unknown`；之后的 fetch 若显示锚点不在 origin 上，是 `Rewound`，不是 `Leads`。§5.4 的前移不修补这个窗口，不把 a 设成 o。

## 3. 公开接口（Rust）

```rust
// crates/strata-publish/src/lib.rs —— 签名清单（不是实现）
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use strata_deps::{self as deps, ChainState, PublishChain};
use strata_gitio::{self as gitio, Git};
use strata_store::{PublishPhase, Store};
use strata_types::{
    Action, BaseGen, CommitOid, ConfirmToken, PushOutcome, ReleaseGroup, RepoConfig, RepoName,
    RepoPath, SessionId, StrataError, TreeOid,
};

pub type Result<T> = std::result::Result<T, StrataError>;

/// 一次发布调用的上下文。各字段由 daemon 持有，按引用传入。
pub struct Ctx<'a> {
    pub store: &'a Store,
    pub git: &'a Git,
    pub cfg: &'a RepoConfig,
    pub repo: &'a RepoName,
    /// 该 repo 的 deps 上下文。`rebase_session` 用它调 `key_of`、`propose`、`observe_main`。
    pub deps: &'a deps::Ctx<'a>,
    /// 占队列与 repo 写锁。见 §5.1。
    pub locks: &'a dyn PublishLocks,
}

/// daemon 提供的两把资源。顺序由 §5.1 规定，trait 本身不强制。
/// 闭包把结果放进它捕获的槽里。外层只区分「还没进入闭包就 `Busy`」和「闭包跑过」。
/// 返回值不经 trait 传出，与 deps 的 `RepoWriteLock` 相同。
pub trait PublishLocks: Sync {
    /// 按升序占住这些会话的队列位置，跑完 `f` 再放开。
    /// 外层 Err 只有占锁前的 `Busy`（有界等待，缺省 30 s）。`f` 没跑。
    fn with_sessions(&self, sessions: &[SessionId], f: &mut dyn FnMut() -> Result<()>) -> Result<Result<()>>;
    /// 拿到 repo 写锁后跑 `f`。占锁前等不到 → 外层 `Busy`。已有可见的写之后，实现不得用 `ctx` 的超时截断这次等待
    /// （"Waits after a call's first visible write are not bounded"）。
    fn with_repo(&self, repo: &RepoName, f: &mut dyn FnMut() -> Result<()>) -> Result<Result<()>>;
}

/// `refs/strata/publish/<branch>`。`branch` 来自 `RepoConfig.publish.branch`，不来自 `default_branch`。
pub fn chain_ref(branch: &str) -> String;

/// `refs/strata/anchor/<branch>`。
pub fn anchor_ref(branch: &str) -> String;

/// 计划里的 `merge_base`。薄包装，判定只在 deps [我的决定 D1]：
/// `publish_chain` 为 `Leads` 时返回链顶，其余返回 origin 的 tip。
/// `None`（还没 fetch）→ `NoSuchBranch`。不写 ref。
pub fn merge_base(git: &Git, cfg: &RepoConfig) -> Result<CommitOid>;

/// 只放本组选中的路径。冲突在内存里判定，不建目录，不写 commit。
pub fn stage(ctx: &Ctx, group: &ReleaseGroup, base: &CommitOid) -> Result<Staged>;

/// `validate_commands` 在建 staging、`attach_staging`、`gitio::stage` 之前就跑。会改文件 → `CheckMutates`，一条都不跑，不建目录。
pub fn check(cfg: &RepoConfig, staged: &Path) -> Result<ChecksRan>;

/// `commit_tree` 之后用 gitio 的一次 ref 事务前进链和锚点。`commit_tree` 已把对象写进主对象库之后，对外不返回 `HeadMoved`：比较并交换失败是 `Unknown`，`HeadMoved` 只在 `detail` 里。不重新合并、不重试。
pub fn commit(git: &Git, staged: &Staged, parent: &CommitOid, expected_chain: Option<&CommitOid>, message: &str) -> Result<CommitOid>;

/// 核对凭据的 `action`，再调 §2 第 6 条记录 `Pushed` 的那一次调用。不调用 confirm。
/// 不需要确认时 `confirm` 为 `None`。需要而没有、或 action 不符 → `NotConfirmed`，那次调用不启动。
pub fn push(git: &Git, cfg: &RepoConfig, staging: &Path, confirm: Option<ConfirmToken>) -> Result<PushOutcome>;

/// 把解决者记在这次发布的 `publishes` 行上（`outcome.assigned`）。不写文件，不合并、不推送。
pub fn assign(store: &Store, id: u64, path: &RepoPath, session: SessionId) -> Result<()>;

/// 发布后把一个会话接到新基线。依赖集必须变时登记提议并等待，会话留在旧基线上。
pub fn rebase_session(ctx: &Ctx, session: SessionId, repo: &RepoName, new_base: BaseGen, head: CommitOid) -> Result<RebaseReport>;

/// 只读。两边钉的基线不同时，先把旧的一方按 overlay 的 rebase 投影到新基线（design §6.3）。
pub fn diff_between(ctx: &Ctx, a: SessionId, b: SessionId, repo: &RepoName) -> Result<BetweenDiff>;

/// 一次发布组的入口。`confirm` 只在这次调用自己会推送时才用。
pub fn publish(ctx: &Ctx, group: &ReleaseGroup, confirm: Option<ConfirmToken>) -> Result<Published>;
```

模块内部的类型（types.md §2.1：定义在所有者的 §3，别人按名字用）：

```rust
/// staging 工作树、合成后的 tree、这次真正写入的路径。
pub struct Staged {
    /// `<布局根>/<repo>`，已由 `attach_staging` 接上。
    pub worktree: PathBuf,
    /// `attach_staging` 的私有 git 目录，不在主 checkout 里。
    pub gitdir: PathBuf,
    pub tree: TreeOid,
    /// 进了 index 的路径，按 `RepoPath` 升序。
    pub paths: Vec<RepoPath>,
}

/// 跑完的检查条数。没配置时为 0（D3）。
pub struct ChecksRan {
    pub ran: u32,
}

/// 一次发布的结果。commit 已在链上就是成功，后面的 rebase 失败不把它改成失败（§6）。
pub struct Published {
    pub id: u64,
    pub commit: CommitOid,
    pub checks_ran: u32,
    /// 这次没推时为 `None`。
    pub push: Option<PushOutcome>,
    pub rebases: Vec<(SessionId, RebaseReport)>,
    pub warnings: Vec<PublishWarning>,
}

/// 不是 `Code`。发布已经成功。
pub struct PublishWarning {
    /// 固定为 `ObserveFailed`。
    pub kind: WarningKind,
    pub detail: serde_json::Value,
}

pub enum WarningKind {
    ObserveFailed,
}

pub enum RebaseReport {
    /// 工作区已换到新基线。`dropped` 是离开 overlay 的路径。
    Moved { dropped: Vec<RepoPath> },
    /// 新基线要另一个依赖集。会话仍钉旧基线。`proposal` 是登记或提升后的那一条。
    Waiting { proposal: deps::Proposal },
    /// `move_workspace` 在交换前发现依赖目录上有打开的文件。会话不动。发布已经成功。
    OpenFiles { detail: serde_json::Value },
}

pub struct BetweenDiff {
    pub only_a: Vec<RepoPath>,
    pub only_b: Vec<RepoPath>,
    /// 两边都有的路径。预览合不上时 `preview` 为 `Err(Conflict)`，不是整次调用失败。
    pub both: Vec<(RepoPath, Result<Merge3Preview>)>,
}

pub struct Merge3Preview {
    pub merged: bool,
    /// 冲突时逐 hunk 的文本，受 §4 的上限约束。
    pub hunks: Vec<HunkPreview>,
}

pub struct HunkPreview {
    pub start: u32,
    pub ours: String,
    pub theirs: String,
    pub truncated: bool,
}
```

每个函数可能返回的码（透传的不改写）：

| 函数 | 自己返回的码 | 透传 |
| --- | --- | --- |
| `merge_base` | `NoSuchBranch`（还没 fetch） | `publish_chain` 里 gitio 的码 |
| `stage` | `Conflict`、`NoCommit`、`NoMatch`、`NoMergeBase`、`Binary`（只出现在 `diff_between` 的条目上）、`TooLarge`（预览条目）、`InvalidInput`（类型转换缺一半、非 UTF-8 链接目标） | `NoSuchCheckpoint`、`NoSuchSession`、`Corrupt` |
| `check` | `CheckMutates`、`CheckFailed` | `validate_commands` 的码 |
| `commit` | `InvalidInput`（`git-identity`，对象还没写）。对象已写入之后只有 `Unknown`，`HeadMoved` 只在 `detail` | `NoSuchCommit`、`Unknown` |
| `push` | `NotConfirmed`、`PushRefused`、`NoCommit`（链不领先，这次没有可推的） | `Unknown`（超时）、gitio 在子进程起来之前的码 |
| `assign` | `NoSuchPath`（没有这次发布的行，或路径不在那次 `Conflict.detail` 里）、`IsDirectory` | — |
| `rebase_session` | 不把 `OpenFiles` 变成整次发布的失败 | `propose`、`move_workspace`、`ensure_base_for` 的码 |
| `publish` | 上表在第一次可见的写之前的那些 | 写了之后只有 `Unknown` |

`Code` 仍是 39 个。`chain_rewound`、`chain_diverged`、`main_moved`、`publish_open`、`publish_unknown`、`link_target`、`git-identity` 都放在已有码的 `detail.reason` 里，不新增变体。

```rust
/// 测试接缝。只在 `test-seams` 下编译，不进发布的二进制（增补 4）。
/// 要在两步之间做事的测试用停住，不用 errno 故障点。
#[cfg(feature = "test-seams")]
pub enum PublishFault {
    /// `commit_tree` 之前停住。
    PauseBeforeCommit,
    /// `commit_tree` 成功、链与锚点的 ref 事务之前停住。此时对象已在主对象库，算可见的写。此后对外只有 `Unknown`。
    PauseAfterCommit,
    /// 记录 `Pushed` 的调用里，子进程起来之后、那次调用提交之前停住，用来造超时。
    PauseDuringPush,
    /// 记录 `Pushed` 的那一次调用提交之前停住，用来造「远端已收下、锚点还没一起写上」。结果不是 `Pushed`，也不是 `Leads`。
    PauseBeforePushRecorded,
    /// 每个会话的发布后 rebase 之前停住：队列和 repo 锁都已放下，行仍是 `Running`（`publishing` 间隙）。review M5 的测试用。[实现时补的决定]
    PauseBeforeRebase,
}
```

## 4. 数据与不变量

### 4.1 目录

一次发布的布局根是 `<paths.root>/sessions/<sid>/.strata-publish-<repo>-<release-id>/`。`sid` 是本组里这个 repo 的批次中最小的 `SessionId`，这样多会话组也有一个确定的位置 [我的决定 D4]。它是真实目录。路径上任何一截是符号链接，或者这个名字本身是符号链接，都在写之前拒绝（`NotDirectory`），不跟随。

布局根里面：

| 路径 | 是什么 |
| --- | --- |
| `<repo>/` | 被发布的仓库。只在内存合并与凭据闸门都过了之后，`export_tree` 创建，`attach_staging` 接上。检查的 cwd 是这里 |
| `<sibling>/` | 配置里其余每个 repo 一份，是该会话所钉基线的 `clonefile`，不是活的工作区。这样 `../<sibling>` 与工作区里的相对路径相同，检查不会写进活的工作区 |
| `.git-<repo>/` | `attach_staging` 的私有 git 目录。不在主 checkout 里。gitfile 的 commondir 是主 checkout |

具名返回不留下这个目录，也不留下能进主 ref 库的 gitfile。冲突在建目录之前就返回，hunk 在 `Conflict.detail` 里，没有目录可留。目录一旦建了，具名返回之前必须 `apfs::remove_tree_rooted` 完成；删不完则这次返回 `Unknown`，发布行保持 `Unknown`，status 报出。成功并完成 rebase 之后同样删掉它。删的时候不跟随符号链接。

`includeIf` 的限制照 gitio 再写一遍（增补 6 的 T10 条）：主 checkout 的 `gitdir:` 与 `onbranch:` 条件包含不作用于 staging 的 `stage` 与 `push`；`hasconfig:remote.*.url:` 在 staging 里生效。本文不另做补救。

### 4.2 Store

`record_publish` 写下 `publishes` 行（`phase = Running`）和 `publish_items`。路径原样照抄，不展开改名；展开是本文在调用之前做的（§5.3）。`set_publish_phase` 只走 `Running → Done`、`Running → Unknown`、`Unknown → Done`。

`outcome` 的形状由本文规定：

```json
{
  "repo": "tool",
  "commit": "<40 hex>",
  "checks_ran": 0,
  "push": { "kind": "Pushed" },
  "warnings": [{ "kind": "ObserveFailed", "detail": {} }],
  "rebase": [{ "session": "s-k3q7xm2a", "kind": "Moved" }],
  "stopped": "Conflict",
  "assigned": [{ "path": "src/a.ts", "session": "s-k3q7xm2a" }]
}
```

`push` 没有推时为 `null`。推了就嵌 types.md 的 `PushOutcome`（`kind` + `message`）。`stopped` 只在具名拒绝把行收成 `Done` 时出现，值是那个码的名字（D16）。`assigned` 由 `assign` 写上，不写在工作树里。`record_ref_change` 的 `confirmed` 只存 `Action`，不存 `ConfirmToken`。

### 4.3 不变量

| # | 不变量 | 落实 |
| --- | --- | --- |
| P1 | 一次 `ReleaseGroup` 里每个 repo 至多一个 commit。同一 repo 的批次按 `batches` 的顺序做三方合并 | design §6.5、§4 |
| P2 | 只提交选中的路径。未选的路径不进 staging 的 index，也不进 commit | 不变量 3 |
| P3 | 合并按路径，不按 inode。特殊 mode 位不进 git。可执行位可以 | T7、T10 |
| P4 | 链与锚点的写、fetch、push 都在 repo 写锁里。持锁时不 `build`、不 `ensure` | 锁顺序裁定 |
| P5 | 推送需要确认时（`deploy_on_push` 或 `publish.confirm`），没有匹配的 `Action::PushDeploy` 就不推。没有 flag、环境变量或配置能绕过 | 不变量 7 |
| P6 | 推送超时留下链，记 `Unknown`，重启只报出，不重放 | 不变量 6 |
| P7 | 新基线要换依赖集时，经 `deps::propose` 登记，会话留在旧基线上，等 `deps approve` | 不变量 4，压过 design §6.4 末句 |
| P8 | 具名拒绝不得留下指向主 checkout 的 gitfile。`validate_commands`、凭据核对、内存里的冲突判定都在 `attach_staging` 与 `gitio::stage` 之前。冲突不建目录，hunk 在 `Conflict.detail`。目录一旦建了，具名返回之前 `remove_tree_rooted` 必须完成，gitfile 不再是进主 ref 库的路径。删不完则返回 `Unknown`，发布行保持 `Unknown`，status 报出。`commit_tree` 把对象写进主对象库之后，对外只有 `Unknown`，`HeadMoved` 只在 `detail` 里 | 增补 3；本轮修订 |
| P9 | `.lww`：组内后应用的批次胜；同一会话里较大的 `CheckpointId` 胜。不按 mtime | T10 |

发布时用的 `.lww` 规则从 B 的 tree 读，经 `gitio::blob_bytes`（§2 第 5 条）取 `.strata/lww` 和它的 `%include`，再 `LwwRules::parse`。不导出，不用 `LwwRules::load`。rebase 仍用新基线目录里的规则（`RebaseOptions::for_move`）。两边只在这次发布改了 `.strata/lww` 时不同，那是该有的差别（增补 5 的 T10 条）。

单个 hunk 的 ours / theirs 文本上限是 4096 字节，按 UTF-8 字符边界截断 [我的决定 D5]。超限的 hunk 带 `truncated: true`。同一冲突里没超限的 hunk 保持原文。空目录不进 tree。

## 5. 流程与算法

### 5.1 锁

全序：建造锁 → 会话队列（`SessionId` 升序）→ repo 写锁（"Lock order, extended"）。

本文的两次临界区都短，除了检查与推送。检查与推送留在第二段锁里，因为合并与比较并交换必须是同一段锁里的一次合并（3d `lane_merge` 与 `concurrent_commits`：取锁前落地的别人的发布并进这一次合并；链顶在读过之后又被推进则不重新合并、不重试，对外是 `Unknown`，`HeadMoved` 只在 `detail` 里）。放开锁再合并会让并发的不相交发布停在那一次失败上，与那两行矛盾。检查不是 `build`。`ensure` / `build` 从不在持锁时调用。

占锁前的等待到时是 `Busy`，什么都没写。`commit_tree` 之后的等锁一直等到拿到。

### 5.2 `publish(ctx, group, confirm)`

按 repo 分组。一个调用只处理 `ctx.repo` 那一组批次；别的 repo 由 daemon 各调一次。批次为空，或没有这个 repo 的批次 → `InvalidInput`。

1. **锁外，任何 ref 写之前。** 读已有的 `publish_chain`。`None` → 第 2 步先 fetch。`Rewound` → `InvalidInput`（`chain_rewound`），不前移锚点，这一步还没写行。`Diverged` 不在锁外拒绝：前移要在锁里的事务里做完，再拒绝。不自动重放（D2）。然后 `ensure` 会话正钉着的依赖集（锁外）。新 tree 的集合还不知道，放到第 8 步、放开队列与锁之后。
2. **占队列，再记行，再拿 repo 写锁。** 队列是本组这个 repo 的会话，按 `SessionId` 升序。占住之后 `rm` 进不了这个会话，然后才 `record_publish`（"A Removed session takes no new pins"）。会话已 Removed → `NoSuchSession`；checkpoint 不存在 → `NoSuchCheckpoint`。这一行是意图行。锁里：`git.fetch("origin", publish.branch)`；按 §5.4 前移锚点；再读 `publish_chain`。`Rewound` → `InvalidInput`（`chain_rewound`），这次没有锚点写，按 D16 把行收成 `Done`。`Diverged` → 同样的码（`chain_diverged`），但若第 5.4 节的前移已经完成，那是做完的副作用，这次拒绝不是「什么都没写」。两种都不 `commit_tree`，记录 `Pushed` 的那一次调用不启动，链 ref 的字节不变。`merge_base` 得到 B，用的是锁里这一次读到的链，不是锁外的计划（D17）。
3. **闸门，在任何 staging 写之前。** `validate_commands`。会改文件 → `CheckMutates`，不建目录，检查进程不启动。这次要推（`publish.push`）且 `cfg.needs_human_confirm()` 时，凭据必须在场，且 `action` 等于 `PushDeploy { repo: ctx.repo.clone(), branch: cfg.publish.branch.clone() }`。否则 `NotConfirmed`，不建目录，不 `attach_staging`，不 `gitio::stage`，不 `commit_tree`，记录 `Pushed` 的那一次调用不启动。凭据按值移走。不核对 nonce、不过期：那是 daemon 核销时 confirm 做的。`publish.push` 为假时这次不看凭据。然后在内存里做 §5.3。B 上每个路径的字节和 `mode`，以及 `.lww`，都经 `blob_bytes` 读取，不调用 `export_tree`。可执行位和 `120000` 来自这个 `mode`。会话所钉基线的字节来自已有的基线目录，checkpoint 的字节来自 blob 存储。冲突 → `Conflict`，hunk 在 `detail` 里，不建目录。合成结果与 B 相同 → `NoCommit`，不建目录。这些具名拒绝按 D16 收成 `Done`。没有 gitfile。
4. **staging。** 闸门过了才建目录。导出在锁里做，对象是锁里刚读到的 B：`export_tree(tree_of(B), <布局根>/<repo>)`。B 的 tree 很大时这段锁会拉长；这是为了后面的一次合并，不在锁外导出再碰运气 [我的决定 D12]。然后 `attach_staging`。兄弟目录用 `clonefile` 放进布局根，源是各会话所钉的基线 generation，不是活的工作区。本 repo 自己的依赖集（布局会话所钉的那份 sealed 集）用 `deps::clone_into` 克隆进 `<布局根>/<repo>/`，检查才找得到 `node_modules`/`vendor` 里的可执行文件；这是私有克隆，检查写不到 sealed 集；锁里不安装，所钉的集已 sealed，不在则 `Corrupt`；导出里已有（被 git 跟踪）的依赖目录保留跟踪内容、不覆盖；依赖目录不进 tree，`gitio::stage` 只暂存计划里的路径（review H3）[实现时补的决定]。写入内存里已经合并好的文件，再 `gitio::stage`。从 `attach_staging` 起，gitfile 的 commondir 是主 checkout；`gitio::stage` 会把对象写进主对象库。这两步都在凭据闸门之后。这一步之后若要具名返回，先按 §4.1 删完目录。
5. **检查。** §5.5。仍在同一段锁里，仍在 `commit_tree` 之前。非零退出要返回 `CheckFailed` 时，先 `remove_tree_rooted`。删完、gitfile 不再是进主 ref 库的路径，才返回 `CheckFailed`。删不完 → `Unknown`，发布行保持 `Unknown`，status 报出，不返回 `CheckFailed`。
6. **提交。** `commit_tree` 把对象写进主对象库，这是不可收回的可见写。此后任何失败对外都是 `Unknown`，现场留下，不删链、不删对象、不重放。链与锚点用 gitio 的一次 ref 事务写下（§2 第 3 条），`expected` 是锁里读到的链顶，没有链时为不存在。事务失败 → `Unknown`，`detail` 里保留 `HeadMoved` 的期望值与实际值（3d `:197`）。不返回 `HeadMoved` 作为对外的码。不重新合并、不重试。
7. **推送。** `publish.push` 为假 → 停在本地，`push` 为 `None`。要推：同一段锁里调用记录 `Pushed` 的那一次 gitio 调用（§2 第 6 条）。它提交时，远端跟踪 ref 和锚点一起变成推上去的 commit。提交之前推送没有完成，不返回 `Pushed`，§5.4 不把这个窗口前移成 `Leads`。`Refused` → 链留在本地；这发生在可见的写之后，所以对外是 `Unknown`，`detail.kind = "PushRefused"`，远端原话留在 `detail.message`。单独的 `push` 调用还没写过，仍返回 `PushRefused`。超时、连接中断，或远端已收下而这次调用还没提交 → `Unknown`，链留下，行记 `Unknown`，不重放。之后的 fetch 若显示锚点不在 origin 上，是 `Rewound`，不是 `Leads`。
8. **放开锁，再放开队列。** 然后 `observe_main`（它自己取锁）。失败 → 警告 `ObserveFailed`，发布仍成功。新基线的依赖集与会话当前的不同时，在这里、锁外 `ensure` 那个集合（"Lock order, extended"）。然后再按升序占队列、拿锁，对每个参与会话做 §5.8。rebase 失败不把已成功的发布改成失败。
9. **收尾。** 全部会话都 `Moved` 或明确 `Waiting` / `OpenFiles` 时，`set_publish_phase(Running, Done, outcome)`。有会话的 rebase 在可见的写之后失败 → 行记 `Unknown`，commit 仍在结果里。成功之后删掉布局根；删不完则行记 `Unknown`，status 报出这个目录。

`publish.push` 为假时，之后的 `push(repo)` 是另一次调用：链已在。凭据在记录 `Pushed` 的那一次调用之前核对，没有则 `NotConfirmed`，链不动。那次调用提交之前推送没有完成，锚点不另写。

### 5.3 合并

每个路径的三边：base 是该会话所钉基线上的版本，ours 是该会话选中的 checkpoint 里的版本，theirs 是当前结果，起点是 B 上的版本（design §6.5 第 3 步；gitio 的约定：ours 是会话，theirs 是上游）。多个批次碰同一路径时按 `batches` 的顺序依次合并。内容来自 checkpoint 的 blob 与基线目录，不读活的工作区，所以选中之后的修改不进这次 commit。

选中规则：

- 批次路径对不上该会话这个 checkpoint 的任何条目 → `NoMatch`。
- 点了 `Renamed` 的 `path` 或 `from` 的一半 → 两条都算选中（增补 1）。
- `Untracked` 与 `Added` 一样发布。
- 文件与符号链接的互换、或同一路径上的 mode 变化，是一条 `Modified`（或 `Added`），mode `120000` 表示符号链接。不是同一路径上的 `Deleted` + `Added`（增补 4）。
- 目录与文件的互换仍是一对：该路径的新条目，加上旧树下每条被删的子路径。一对必须在同一批次里。缺一半 → `Conflict`，`detail.reason = "type_pair"`。
- 符号链接目标不是 UTF-8 → `InvalidInput`，`detail.reason = "link_target"`，在 `commit_tree` 之前。

字节规则：

- 两边相对 base 都没改 → 保持。
- 只有一边改 → 取那边。权限（可执行位）与内容分开比：一边只改可执行位、另一边改内容，结果两者都要。B 一侧的可执行位和符号链接来自 `blob_bytes` 的 `mode`（`100755` 与 `120000`），在任何导出之前。git 只记录 `100644` / `100755`，`700` 这类完整 mode 不进 commit。符号链接不是普通文件。
- 两边都改、内容哈希相同 → 不冲突，二进制也一样。
- 两边都改、哈希不同：任一边是 `is_binary`，或任一边超过 `overlay::MERGE_MAX_BYTES`（64 MiB）→ 整文件 `Conflict`，不读超限文件的内容，不做逐字节 diff3。没超限的文本交给 `gitio::merge3`。干净 → 取合并结果。冲突 → 整组停下。
- 一边删除、另一边修改同一路径 → `Conflict`。
- 两边都删、其中一边把内容放到新路径（改名）→ 旧路径删除，新路径保留，无冲突。不追随 inode：目录改名之后，另一边对旧路径的修改是「删对改」，冲突，不跟着到新路径。
- 两边把同一文件改成不同的新名字 → 旧名消失，两个新名都留，内容各自保留。不是硬链接。
- 删除与新增的路径不相交 → 都留。
- 两边都新增、字节相同 → 留一份。字节不同 → `Conflict`。
- 空目录不写入 tree。

`.lww`（规则经 `blob_bytes` 从 B 读，不导出）：只有两边都改了的命中路径才决议。组内后应用的批次整文件胜出，不试 `merge3`，不算冲突。同一会话的多个批次，较大的 `CheckpointId` 胜，不管它在 `batches` 里的位置。不按 mtime。没被命中的路径照常三方合并。没有 unit，依赖目录不参与合并。B 上参与合并的文件字节和 `mode` 同样经 `blob_bytes`，不经 `export_tree`。路径不在 tree 里是 `None`，不是目录。

13 个路径级结果（3d 的 `merge_root_oracle`，inode 语义不保留）：

| 用例 | 路径级结果 |
| --- | --- |
| disjoint | 两边的路径都在，无冲突 |
| text-merge | hunk 不重叠，一个合并结果 |
| text-conflict | 同一 hunk → `Conflict`，整组停下 |
| modify-delete | `Conflict`，清单里有该路径 |
| move-directory | 旧路径的删除加上新路径的新增，无冲突，两个方向一样 |
| move-and-edit-inside | 不追随 inode。一边改名（旧路径删除 + 新路径新增），另一边改旧路径下的文件 → `Conflict`（删对改）。这是重新裁定，与 LayerFS 的「编辑跟着走」不同 |
| revive-deleted-parent | 按路径：父路径被删又被加回，是删除与新增，不相交的子路径留着 |
| directory-becomes-file | 必须成对。一对齐 → 旧树的子路径删除、该路径变成文件。缺一半 → `Conflict`。不按 inode 把目录「变成」文件 |
| add-add-same | 字节相同，留一份，无冲突 |
| add-add-different | `Conflict` |
| 排序边界名 `n1` / `n1.y` / `n1-z` / `n1x.txt` | 都进 tree，顺序是 git 的 tree 序（字节序），不是语言的字典序 |
| delete-directory-against-delete-inside | 两边都是删除，结果这些路径都不在 |
| both-move-elsewhere | 无冲突：旧路径两边都删，两个新路径都留。丢掉的只是硬链接组 |

合成后的 tree 与 B 的 tree 相同 → `NoCommit`，不产生空提交，不推。

### 5.4 锚点

o = `publish_chain.origin`，c = 链顶，a = 锚点。前移只在 repo 写锁里、经 §2 第 3 条的那一次 ref 事务做，对旧的 a 比较并交换。只改锚点时，同一次事务里链保持原值，不另写一次。

`publish_chain` 为 `Equal`、`Leads`、`Behind` 或 `Diverged` 时，取 `m = git.merge_base(o, c)`（gitio 的函数，不是本文的 `merge_base`）。`is_ancestor(a, m)` 且 `m != a` 时把 a 设成 m。`Equal` 与 `Leads` 的 m 是 o，`Behind` 的 m 是 c，`Diverged` 的 m 是分叉点。`Absent` 与 `Rewound` 不前移。`Behind` 时不把 a 设成 o：那会让 a 超过 c，之后只丢掉 c 之上的人的提交就会读成 `Rewound`。这个前移不用于「远端已收下、记录 `Pushed` 的调用还没提交」的窗口。那个窗口是 `Unknown`，或在锚点已经不在 origin 上时是 `Rewound`，不是 `Leads`，这里不把 a 设成 o。

本次发布自己的写：B 是 origin 的 tip 时，同一次事务里把链设成新 commit、把 a 设成 B。推送的锚点不在 `Pushed` 返回之后另写。记录 `Pushed` 的那一次调用同时比较并交换远端跟踪 ref 和锚点（§2 第 6 条）。那次调用提交之前，推送没有完成。远端已经收下、这次调用还没提交就崩溃：结果是 `Unknown`；之后的 fetch 若显示锚点不在 origin 上，是 `Rewound`，不是 `Leads`。上面的前移不修补这个窗口。不拆成两次 `update_ref`。D11 不能覆盖这条裁定，已撤回。

### 5.5 检查

`RepoConfig::validate_commands` 在建目录、`attach_staging`、`gitio::stage` 之前就跑。argv 里有 `--write`，或 `cs-fix` 没有 `--dry-run` → `CheckMutates`，检查进程不启动，不建目录。闸门过了之后，在 `<布局根>/<repo>` 里按序跑 `checks`，cwd 是该目录，不经 shell（config.md §2.4）。非零退出 → 先删掉布局根（§5.2 第 5 步），删完才返回 `CheckFailed`，`detail` 带命令、退出码、输出末尾 8 KiB。不提交。删不完则不是 `CheckFailed`。

配置了 0 条 → 不失败，`checks_ran = 0`（D3）。结果与 `outcome` 里都写这个数。

兄弟布局是给 tool 的 `bun dev-link.mjs --sync-only` 与 admin 的 `./dev-link.sh` 用的（config.md §2.3 第 5 条）。front 的 `dev-link.sh` 不在 `checks` 里，本文也不跑它。staging 带 `.git`，`bun.lock` 在导出里是被跟踪的，所以 admin 的 self-heal 不会删它。

### 5.6 `push` 与确认

`needs_human_confirm` 为真时，凭据的 `action` 必须是 `PushDeploy { repo, branch }`，`branch` 等于 `cfg.publish.branch`。不符 → `NotConfirmed`，记录 `Pushed` 的那一次调用不启动。人的 `pre-push` 钩子由 gitio 在那次调用里跑，cwd 是 staging 根，环境里有 `STRATA_PUBLISH=1`。钩子拒绝按 gitio 的 porcelain 归类，本文不另解析。那次调用提交之前不是 `Pushed`。

链不领先 origin（`publish_chain` 不是 `Leads`）→ `NoCommit`，不推。

### 5.7 `assign`

把 `{ "path", "session" }` 写进这次发布行的 `outcome.assigned`。不写文件，不在会话根下留工作树。行不存在，或路径不在那次 `Conflict.detail` 里 → `NoSuchPath`。`path` 是目录 → `IsDirectory`。不合并、不推送、不占 repo 写锁。下一次 `publish` 若仍选这条路径，批次的会话必须是被点名的那一个，否则 `Conflict`，`detail.reason = "assigned"`。这只是点名，不代替人把文件改好。

### 5.8 `rebase_session`

只对本次 `ReleaseGroup` 里这个 repo 的参与会话。别的会话不动：先落地的会话保持它自己那次 commit 的内容；后合并落地的会话才在 rebase 之后同时含两边的文件。

deps.md §5.12 的顺序，不改：

1. 锁外 `(N_inputs, N) = key_of(ctx.deps, 新基线目录)`。目录还没有就先 `ensure_base_for`（锁外，缓存填充）。
2. repo 写锁里：s 有开着的草稿，且 overlay 里 `is_input` 为真的路径这次都发布了、发布之后也没再改 → `propose(Session { s }, 带目标 (tree, head))`。只发布了一部分 → 不调，草稿留着。`D.from == N` → 也不调。
3. `N == s.base.deps` → 占队列（若还没占）、拿锁、`move_workspace`。交换前 session 自己复查打开的文件。
4. 否则会话留在旧基线上，锁里 `propose(Main, 带目标)`。返回 `Waiting`。
5. `main_tip` 等于新的 head 时，放开锁之后 `observe_main`。失败只报 `ObserveFailed`。

`move_workspace` 的 `head` 是这次的 commit。已发布且与会话当前内容一致的路径离开 overlay（`rebase` 的 `dropped`）。选中之后才有的修改留着，进入下一次 checkpoint。新基线上别人发布的、本会话没有的路径，出现在换过的工作区里。

`OpenFiles` 按会话放进 `RebaseReport`，不失败这次发布。人关掉进程之后再移，不重放 commit。

### 5.9 `diff_between`

只读，不占 repo 写锁。两边 `overlay::compute`。只在一边有的路径进 `only_a` / `only_b`。两边都有的用与 §5.3 相同的三边做预览。钉的基线不同时，先把旧的一方用 `overlay::rebase`（`RebaseOptions::for_move`）投影到新基线，再比。超过 64 MiB 的文件该条目返回 `TooLarge`，不读内容。二进制该条目返回 `Binary`，不给 hunk。整次调用不因此失败。

预览的是两边工作区的当前内容，checkpoint 过的与没 checkpoint 的修改一样。没投影的一方从工作区按路径读字节（不跟随符号链接），读出的 BLAKE3 必须等于扫描时的值，不等或文件已不在 → 该次调用返回 `Busy`（`detail.reason = "workspace_changed"`），重试即可；超过 64 MiB 的不读，照上面返回 `TooLarge`。要投影的一方，先用 `put_blob_rooted` 把 overlay 里的 blob 存进 store（overlay §5.8 的前提），这是缓存填充，不算对外写入（overlay.md Addendum 6 refinement 4），存出来的 id 与扫描不符同样是 `Busy`（`workspace_changed`）；store 只读时这一步透传 `ReadOnly`。从不因为修改还没 checkpoint 而返回 `Corrupt`。[实现时补的决定]

### 5.10 复杂度

| 操作 | 复杂度 | 目标 |
| --- | --- | --- |
| `merge_base`、锚点前移 | 三次 ref 读 + 一次 `merge-base` | 小于 100 ms，不含 fetch |
| 合并 | 只对两边都改的文件调用 `merge3`，O(行数 × 改动) | 两个会话、各改 50 个不重叠文件，锁内合并小于 2 s（不含检查与 fetch） |
| 检查 | 命令自己的用时 | 不另加目标。锁被它占着；占锁前的等待因此可能 `Busy` |
| 推送 | 网络用时 | 超时由调用方传入，缺省与 gitio 的推送超时相同 |
| rebase | 一次 `key_of` + 一次 `move_workspace` | 热路径（依赖集没变）小于 1 s，不含克隆 |

design §8 没有给发布单独的秒数。上表是本文的验收数字，首个 spike 之后可以改。

## 6. 失败与恢复

具名拒绝不得留下指向主 checkout 的 gitfile（P8）。`validate_commands`、凭据核对、内存里的冲突判定都在 `attach_staging` 与 `gitio::stage` 之前，这些拒绝不建目录。目录一旦建了，具名返回之前 `remove_tree_rooted` 必须完成；删不完则返回 `Unknown`，发布行保持 `Unknown`，status 报出，不把这次收成 `Done`。删得完的具名拒绝才按 D16 收成 `Done`，`outcome.stopped` 记下那个码。人从 `detail` 看到的仍是那个具名码。重启不会重放一条 `Done`。意图行本身不是可见的写。本文不删行（store 的行只由 gc 删，而 `publishes` 永不删）。

`commit_tree` 把对象写进主对象库之后，对外只有 `Unknown`。`HeadMoved` 只出现在 `detail` 里，不是对外的码。

| 情形 | 对外的码 | 留下什么 | 之后 |
| --- | --- | --- | --- |
| 链与锚点的那一次事务失败 | `Unknown`，`detail` 保留 `HeadMoved` | commit 对象在。事务是一次调用，不留下「链已前进、锚点没写」的半截 | 不重试、不重合并。status 报出。已有的不一致不读成 `Leads` 来补 |
| 同一次调用里，commit 已写入之后推送被拒 | `Unknown`，`detail.kind = "PushRefused"` | 链在本地 | 人处理后再 `push`。不自动重推。单独的 `push` 调用还没写过，仍返回草稿表里的 `PushRefused` |
| commit 已写入之后的任何错误（review-2 N3） | `Unknown`，`detail.commit`；另带 `detail.push`（已知的推送结果，`PushOutcome` 的 JSON，含推送成功后 rebase / 关行失败时的 `Pushed`；没推为 null）和 `detail.checks_ran`；链 ref 事务失败（commit 已写、还没推）也带 `push: null` 和 `checks_ran`（review-3 R3）；行关不上（`row_moved` / `row_not_closed`）时这些键从原错误拷过来 [实现时补的决定] | 不变 | daemon 把它们放进 `detail.landed`，信封里不丢已落地的结果 |
| 推送超时或连接中断 | `Unknown` | 链在本地，行是 `Unknown` | 重启只报出，不重放 |
| 推送之后的 I/O、rebase 写到一半 | `Unknown` | 现场留下 | 不重放。已进链的 commit 由 status 报出，不当成没发生 |
| `observe_main` 失败 | 无（警告 `ObserveFailed`） | 发布已成功 | daemon 再观察 |

具名拒绝（目录已删完，或还没建）：

| 情形 | 码 | 写了什么 |
| --- | --- | --- |
| 等队列或等 repo 锁超时 | `Busy` | 无 |
| `Rewound` | `InvalidInput`（`chain_rewound`） | 无。不前移锚点 |
| `Diverged` | `InvalidInput`（`chain_diverged`） | 锚点前移若已完成，那是做完的副作用，不是「什么都没写」。没有 commit，链 ref 不变 |
| 同一 hunk、类型转换缺一半、删对改 | `Conflict` | 无目录。hunk 在 `detail` |
| 检查会改文件 | `CheckMutates` | 无进程，无目录 |
| 检查非零 | `CheckFailed` | 无 commit。目录已删完，gitfile 不在。删不完则不是这一行，是 `Unknown`，行保持 `Unknown` |
| 要确认而没有匹配的凭据 | `NotConfirmed` | 这次调用没推。链上已有的 commit 只可能来自早先 `push = false` 的成功发布 |
| 合成结果与 B 相同 | `NoCommit` | 无 commit |
| 这个 repo 已有 `Running` 或 `Unknown` 的发布行 | `Busy`（`publish_open` 或 `publish_unknown`） | 无。不覆盖那一行，不重放 |
| 会话已 Removed | `NoSuchSession` | 无 |

`Unknown` 的行挡住这个 repo 的下一次发布，直到人看过 status。不挡住别的 repo。会话删除是否被挡由 session 定，本文不代它拒绝 `rm`。

善后自己又失败（清理 staging 时出错）时，外层仍是 `Unknown`，原始 `detail` 留在 `detail.cause` 里。不因为内层是 `Busy` 之类的具名码就把外层改回去。

## 7. 测试清单

| 测试名 | 断言 | 来源（矩阵行 或 新增） |
| --- | --- | --- |
| `merge_base_prefers_local_chain_when_ahead` | 链 `Leads` 且锚在 origin 上 → B 是链顶。`Absent`、`Equal`、`Behind`、`Rewound`、`Diverged` → B 是 origin。没 fetch → `NoSuchBranch`。函数只调 `deps::publish_chain`，不自己比祖先 | 新增（D1；"The chain predicate lives in deps"） |
| `anchor_advances_to_merge_base_not_origin_when_behind` | `Behind`：下一次锁里的事务把 a 设成 `git.merge_base(o, c)`，即 c，不是 o。`Equal` 且 a ≠ o 时 a 变成 o。`Diverged` 时 a 变成分叉点。比较并交换的期望值是旧的 a | 新增（"The anchor follows origin along the chain — amended"） |
| `anchor_does_not_advance_when_absent_or_rewound` | `Absent`（没有链）与 `Rewound`（a 不在 o 上，或没有 a）不写锚点 | 新增（同一条裁定） |
| `chain_rewound_or_diverged_refuses_before_write` | 两种状态都返回 `InvalidInput`，`detail.reason` 分别是 `chain_rewound`、`chain_diverged`。没有 `commit_tree`，没有 `push`，链 ref 的字节不变。`Rewound` 不写锚点。`Diverged` 若已做完锚点前移，那是副作用，这次拒绝不是「什么都没写」 | 新增（D2） |
| `publish_branch_is_not_default_branch` | `publish.branch ≠ default_branch` 时，B 与推送目标都是 `origin/<publish.branch>`。`pin_unpushed` 在这时不起作用。本文不钉新会话 | 新增（增补 1 的 T9/T10 条；config.md 的 `publish`） |
| `stage_only_selected_paths` | 未选路径不进 `gitio::stage` 的参数，也不进 commit 的 tree。选中 `Renamed` 的一半时，两条路径都在 | 新增（不变量 3）；`layerfs-workspace/tests/captured_namespace_rebound.rs` 的成对要求由 `type_transition_pair_required` 承担 |
| `two_sessions_nonoverlapping_hunks_merge_clean` | 两个会话改同一文件的不同 hunk，中间至少隔一行 → 一个 commit，两处改动都在 | 新增（计划）；`layerfs-api/tests/merge_root_oracle.rs::merged_roots_of_every_rule_case` 的 text-merge |
| `same_hunk_conflict_stops_whole_group` | 同一 hunk 两边都改 → `Conflict`，`detail` 里有 hunk 的 ours / theirs。无目录，无 gitfile，无 `attach_staging`，无 `gitio::stage`，无 commit、无 push | 新增（计划）；`layerfs-daemon/tests/concurrent_commits.rs::stale_workspace_commits_of_one_file_give_one_conflict` |
| `same_bytes_do_not_conflict` | 两个会话把同一文件写成相同字节，含 400 KiB 二进制 → 无冲突，commit 里是那份字节 | `layerfs-api/tests/sdk_written_back.rs::a_merge_of_two_sides_that_wrote_the_same_bytes_does_not_conflict` |
| `rename_both_sides_keeps_both_names_not_hardlinks` | 两边把同一文件改成不同名字 → 旧名不在，两个新名都在，内容各自一份。`lstat` 的 nlink 都是 1 | `layerfs-api/tests/merge_by_serial.rs::a_file_renamed_on_both_sides_keeps_both_names_as_links` |
| `delete_against_edit_conflicts` | 一边删掉路径，另一边改它 → `Conflict`，`detail` 列出该路径 | `layerfs-api/tests/merge_by_serial.rs::a_deleted_directory_against_an_edit_below_conflicts` |
| `delete_directory_against_move_keeps_moved` | 一边删 `/d1`，另一边把它改名为 `/moved`，两个方向都试 → `/d1` 不在，`/moved` 下的文件在，内容不变 | `layerfs-api/tests/merge_by_serial.rs::a_deleted_directory_against_a_move_keeps_the_moved_directory` |
| `delete_against_disjoint_add_keeps_add` | 一边删 `/d1` 下原有文件，另一边新增 `/d1/new.txt` → 结果里只有 `new.txt` | `layerfs-api/tests/merge_by_serial.rs::a_deleted_directory_against_an_add_below_keeps_only_the_add` |
| `one_sided_exec_bit_kept` | 一边只把可执行位置上，另一边改别的文件 → 两个改动都在。mode `0700` 进 commit 时是 `100755`，不是 `0700` | `layerfs-api/tests/merge_by_serial.rs::a_one_sided_mode_change_is_kept` |
| `chmod_and_content_merge` | 同一文件一边只改可执行位、另一边改内容，两个方向 → 无冲突，新内容与新可执行位都在 | `layerfs-api/tests/merge_metadata_only.rs::a_chmod_survives_a_content_edit_of_an_inline_file` |
| `merge_root_oracle_path_level` | §5.3 的 13 行各有一个 fixture，断言与那张表一致。`move-and-edit-inside` 是 `Conflict`，不是编辑跟着走。`directory-becomes-file` 成对时成功，缺一半时 `Conflict`。`both-move-elsewhere` 两个新名都在、nlink 为 1。边界名在 tree 里的顺序是 git 的字节序。不比较跨构建的根哈希 | `layerfs-api/tests/merge_root_oracle.rs::merged_roots_of_every_rule_case` |
| `lww_later_batch_wins` | 命中 `.lww` 的文件两边都改 → 不冲突，组内后应用的批次整文件胜出。只有一边改 → 取那边，不决议 | `layerfs-content/tests/merge_units.rs::only_units_both_sides_changed_are_decided_and_ours_wins_a_tie`；`layerfs-api/tests/merge_units.rs::units_come_before_lww_rules_which_still_apply_outside` |
| `lww_later_checkpoint_wins_within_session` | 同一会话两个批次，`CheckpointId` 较大的那份胜，即使它在 `batches` 里靠前。不读 mtime | 新增（T10 的 `.lww` 条；3d `:45` 写明不按 mtime） |
| `deps_dirs_are_not_merge_units` | 没有 unit 参数。依赖目录里的文件不进合并。空的、重叠的、写坏的 unit 概念都不存在，发布不因此 `InvalidInput` | `layerfs-api/tests/merge_units.rs::a_unit_both_sides_changed_comes_whole_from_the_later_side`；`layerfs-api/tests/merge_units.rs::ours_is_kept_on_a_tie`；`layerfs-api/tests/merge_units.rs::deleting_or_retyping_the_unit_root_is_a_change`；`layerfs-api/tests/merge_units.rs::renames_across_and_within_a_unit`；`layerfs-api/tests/merge_units.rs::a_directory_moved_out_of_a_lost_unit_is_restored_beside_its_move`；`layerfs-api/tests/merge_units.rs::overlapping_units_belong_to_the_first_listed`；`layerfs-api/tests/merge_units.rs::malformed_units_are_refused_before_any_work`；`layerfs-api/tests/merge_units.rs::units_that_decide_nothing_leave_the_root_byte_identical` |
| `type_transition_pair_required` | 目录换成文件时，批次只有新文件、没有子路径的删除 → `Conflict`，`detail.reason = "type_pair"`。两半都在 → 一个 commit，旧子路径不在，新文件在 | 新增（T10；3c `captured_namespace_rebound` 的改写指向本文） |
| `file_to_symlink_is_one_modified` | 同一路径从文件变成符号链接，checkpoint 里是一条 mode `120000` 的 `Modified` → commit 里该路径是符号链接，目标是 blob 的字节。不要求第二条 `Deleted` | 新增（增补 4 "Same-path type change"） |
| `untracked_publishes_as_added` | `Untracked` 条目进 commit，与 `Added` 相同 | 新增（增补 1） |
| `empty_directory_absent_from_tree` | 批次只含一个新建的空目录 → 该目录不在 tree 里。工作区里的空目录不动 | `layerfs-workspace/tests/captured_namespace_states.rs::fresh_directories_get_headers_even_with_no_name` |
| `binary_and_oversize_conflict_without_byte_diff` | 两边都改、哈希不同：前 8000 字节有 NUL，或文件超过 64 MiB → `Conflict`，一次整文件 hunk，不调用会读完全部内容的 diff。持锁期间读的字节数有上限（超限文件只读哈希） | `layerfs-daemon/tests/lane_merge.rs::the_lane_does_not_compare_a_large_file_byte_by_byte` |
| `hunk_text_cap_keeps_small_hunk_exact` | 两处冲突，一处超过 4096 字节、一处只有一行：小的 ours / theirs 与输入逐字节相同，大的 `truncated = true`，长度不超过 4096。`detail` 里 hunk 的个数是 2 | `layerfs-bridge/tests/sdk_merge_values.rs::oversized_hunks_are_explicit_and_small_conflicts_keep_exact_text` |
| `no_commit_when_identical_to_base` | 选中的路径与 B 无差异 → `NoCommit`，无 commit，链 ref 不变 | `layerfs-workspace/tests/captured_namespace.rs::an_empty_capture_returns_the_base_root_identity` |
| `special_mode_bits_never_written` | 条目带着 setuid 位进入（不该发生）时，commit 里该 blob 的 mode 是 `100644` 或 `100755`，没有 setuid / setgid / sticky。目录 mode 不写、不警告 | 新增（T7 / T10） |
| `staging_refuses_to_follow_a_symlink` | 布局根的位置是符号链接，或任一上级是符号链接 → `NotDirectory`，链接目标的 mtime 不变，没有 `commit_tree` | 新增（任务：不跟随符号链接写 staging） |
| `stage_goes_through_gitio_stage` | `take_git_calls` 里写 index 的命令只有 gitio 的 `attach_staging` 与 `stage`。没有第二份 `--index-output` 由本文拼出 | 新增（gitio.md §5.9 的用法） |
| `check_mutating_command_is_config_error` | `--write`，或 `cs-fix` 没有 `--dry-run` → `CheckMutates`。检查进程的 argv 没有出现在记录里。不建目录，不 `attach_staging` | 新增（计划；config.md §4.2） |
| `check_failed_does_not_commit` | 检查退出码非零，且 `remove_tree_rooted` 完成 → `CheckFailed`，`detail` 含命令与退出码，无 commit，布局根不在，gitfile 不在。删不完 → `Unknown`，发布行保持 `Unknown`，不返回 `CheckFailed` | 新增（design §6.5 第 4 步） |
| `zero_checks_reports_zero_ran` | `checks` 为空 → 成功，`checks_ran = 0`，`outcome` 里也是 0 | 新增（D3；config.md §2.3 第 2 条） |
| `staging_has_the_publishing_repos_sealed_deps` | 检查跑只在 sealed 依赖集里的 `node_modules/.bin/lint` 能成功；staging 里改依赖文件不影响 sealed 集；依赖目录不进 staged tree | 新增（review H3） |
| `sibling_layout_matches_workspace` | tool 的检查 cwd 的 `../<sibling>` 解析在布局根里，不在活的会话目录里。admin 的 staging 有 `.git`，`bun.lock` 是已跟踪文件 | 新增（config.md §2.3 第 5 条；D4） |
| `commit_cas_failure_is_not_retried` | 读过链顶之后、ref 事务之前，另一边推进了链 → 不重新合并。`commit_tree` 已把对象写进主对象库之后，对外是 `Unknown`，`HeadMoved` 只在 `detail` 里，带期望值与实际值。不把 `HeadMoved` 当作对外的码 | `layerfs-daemon/tests/lane_merge.rs::a_commit_landing_before_the_lane_is_merged_in_the_lane`；`layerfs-daemon/tests/mount_control.rs::an_unresolved_moved_head_failure_is_answered_and_not_dropped` |
| `deploy_on_push_without_confirm_stops_before_push` | `deploy_on_push` 或 `publish.confirm` 为真、凭据缺失或 `action` 不是 `PushDeploy` 的这个 repo 与分支 → `NotConfirmed`。`attach_staging`、`gitio::stage`、`commit_tree` 都没被调用。记录 `Pushed` 的那一次调用不启动。没有 gitfile | 新增（计划；不变量 7） |
| `push_timeout_keeps_chain_reports_unknown` | 推送超时后链 ref 仍是新 commit，结果 `Unknown`，`publishes` 行是 `Unknown`。重启后 status 报出同一行，不调用 `push` | 新增（Review Focus 5）；`layerfs-daemon/tests/mounted_commit_failures.rs::f3_unknown_history_outcome_keeps_custody_and_the_mount_still_serves`；`layerfs-daemon/tests/store_commit.rs::unknown_history_acknowledgement_retains_original_intent_and_capture` |
| `push_refused_keeps_local_chain` | 远端拒绝 → 链仍指向新 commit，对外 `Unknown`，`detail.kind = "PushRefused"`，`detail.message` 是远端原话 | 新增（types.md `PushRefused`；写了之后不交具名码） |
| `push_false_leaves_chain_local` | `publish.push = false` → 记录 `Pushed` 的那一次调用不启动，链指向新 commit，`Published.push` 为 `None` | 新增（design §6.7） |
| `pushed_sets_anchor_to_pushed_commit` | `Pushed` 只在同一次 gitio 调用提交之后返回：那次调用比较并交换远端跟踪 ref 和锚点，锚点等于推上去的 commit。提交之前推送没有完成。远端已收下而这次调用没提交就崩溃 → `Unknown`；之后 fetch 显示锚点不在 origin 上 → `Rewound`，不是 `Leads`，§5.4 不把 a 设成 o | 新增（"The publish chain carries an anchor"） |
| `no_flag_or_env_bypass` | 设置任意环境变量或在配置里把 `confirm` 写成假、但 `deploy_on_push` 仍为真 → 仍要凭据。本文不读绕过用的 flag | 新增（不变量 7） |
| `post_publish_rebase_drops_published_paths` | 发布并 rebase 之后，参与会话的 overlay 不再含已发布且内容一致的路径。B 先发布与 A 不相交的 `from-b`，A 再发布 `from-a` 并 rebase 之后，A 的目录里既有 `from-b`（内容 bravo）也有 `from-a`（内容 alpha） | `layerfs-daemon/tests/negative_entries.rs::a_name_another_workspace_added_shows_after_the_merged_install`；`layerfs-overlay/tests/kept_live.rs::folding_a_failed_capture_into_a_kept_row_counts_it` |
| `later_edits_survive_rebase` | 选中 checkpoint 之后又写的字节不进这次 commit，rebase 之后仍在工作区里，下一次 checkpoint 看得到 | `layerfs-workspace/tests/captured_namespace.rs::mutations_after_the_capture_are_absent_from_its_root_and_present_in_the_next`；`layerfs-daemon/tests/store_commit.rs::later_mutation_survives_capture_publication_and_known_install`；`layerfs-workspace/tests/namespace_view.rs::known_install_folds_a_capture_and_keeps_later_namespace_changes` |
| `rebase_waits_when_deps_must_change` | 新基线的 `key_of` 与会话当前依赖集不同，且没有可提升的、已全部发布的草稿 → `Waiting`，会话的 `base` 不变，锁里有一次 `propose(Main, …)` | 新增（不变量 4；deps.md §5.12） |
| `landed_draft_promoted_under_repo_lock` | 草稿的安装输入路径这次都发布了 → 锁里 `propose(Session { s }, 带目标)`。`to` 与新 id 相同则提升；不同则先改登记再提升。提升之后该会话不再因这条草稿 `OwnChange` | 新增（deps.md §5.12 第 2 步；"T10 publish (from the deps r2 re-review)"） |
| `partial_input_publish_does_not_promote` | 安装输入路径只发布了一部分 → 不调用 `propose(Session { s })`，草稿还在 | 新增（deps.md §5.12） |
| `first_lander_keeps_exact_tree` | 两个会话并发，先后落地。先落地的会话 rebase 到它自己的 commit，目录里没有后一个会话的文件。后一个会话 rebase 到合并 commit，目录里两边的文件都有 | `layerfs-daemon/tests/mounted_parallel_commits.rs::parallel_workspace_commits_keep_distinct_captures_and_one_admission_each` |
| `open_files_does_not_fail_a_landed_publish` | `move_workspace` 返回 `OpenFiles` → `Published` 仍成功，该会话的 `RebaseReport::OpenFiles`，链上有 commit | 新增（§5.8） |
| `observe_failed_is_a_warning` | `observe_main` 返回 `Busy` 或 `Unknown` → `Published` 成功，`warnings` 里有一条 `kind = ObserveFailed`，会话生命周期不变 | 新增（"observe_main is best-effort for its callers"） |
| `publish_does_not_build_while_holding_repo_lock` | 测试锁记下每一段持锁。`ensure` / `build` 的调用时刻全部在占队列之前，或在两段锁都放开之后。持锁期间的调用列表里没有它们 | 新增（"Lock order, extended"） |
| `failed_publish_keeps_overlay` | `Conflict` 或 `CheckFailed` 之后，会话 overlay 与发布前相同，`status` 仍报有未发布改动 | `layerfs-overlay/tests/kept_live.rs::folding_a_failed_capture_into_a_kept_row_counts_it` |
| `injected_failure_after_commit_is_unknown` | 在 `commit_tree` 之后的第一步、中间一步、最后一步各注入一次失败 → 每次都是 `Unknown`，之后的步骤不再调用，已写入的 commit 对象还在，overlay 不动 | `layerfs-workspace/tests/captured_namespace_custody.rs::the_first_middle_and_last_failure_of_each_call_kind_lands_in_its_expected_slot` |
| `second_attempt_does_not_overwrite` | 行还是 `Running` 或 `Unknown` 时再调用 `publish` → `Busy`，`detail.reason` 为 `publish_open` 或 `publish_unknown`。原行的 `outcome` 不变，没有第二次 `commit_tree` | `layerfs-workspace/tests/captured_namespace_custody.rs::a_second_attempt_in_the_same_scope_is_refused_by_its_deciding_record`；`layerfs-daemon/tests/mount_control.rs::an_original_unknown_blocks_another_commit_and_normal_unmount`；`layerfs-daemon/tests/store_commit.rs::nested_storage_unknown_keeps_original_capture_and_blocks_another_commit` |
| `rebase_failure_still_reports_the_commit` | commit 已在链上，rebase 没跑成（注入失败）→ status 读到的记录里有这个 commit，不把它显示成没发生 | `layerfs-daemon/tests/mount_control.rs::status_retains_known_publication_when_the_engine_is_unavailable`；`layerfs-daemon/tests/store_commit.rs::known_publication_survives_an_unattempted_local_install` |
| `uncertain_cleanup_stays_unknown` | 具名失败之后，清理布局根又失败 → 对外 `Unknown`，`detail.cause` 是原来的码，目录还在 | `layerfs-daemon/tests/mount_control.rs::a_definite_commit_failure_whose_capture_is_not_resolved_answers_unknown` |
| `restart_reports_unknown_never_replays` | 行是 `Unknown`，进程重启 → `open_publishes` 含这一行，没有任何代码路径调用 `push` 或 `commit_tree` | 新增（T10；T13） |
| `disjoint_concurrent_publishes_all_land` | 四个会话各改不同文件，同时发布：repo 写锁内串行，四个文件都在最后的链顶里，每个会话各一个成功结果 | `layerfs-daemon/tests/concurrent_commits.rs::stale_workspace_commits_of_different_files_all_publish` |
| `conflict_already_landed_stops_the_group` | 取锁之后的那一次合并遇到冲突 → 整组 `Conflict`，已在链上的前一个 commit 不被覆盖 | `layerfs-daemon/tests/lane_merge.rs::a_conflict_landing_before_the_lane_follows_the_conflict_rules` |
| `concurrent_publish_matches_serial_oracle` | 两个会话对同一 repo 并发发布，含一次合并与一次同路径冲突：成功的次数等于链上新增的 commit 数，冲突那次是 `Conflict`，最终 tree 等于按结果重放的串行顺序。期间的 status 读不取 repo 写锁 | `sdk/js/test/verify/direct_locking_real.mjs` |
| `pre_write_wait_is_busy_post_write_wait_is_not` | 占锁前等待超过设定的上界 → `Busy`，无 commit。`commit_tree` 之后持锁者再占 60 s → 这次调用等到拿到，不返回 `Busy` | 新增（"Waits after a call's first visible write are not bounded"） |
| `assign_names_resolver_and_does_not_push` | `assign` 把 `{ path, session }` 写进这次发布行的 `outcome.assigned`。不写文件，不调用 `push`，不调用 `commit_tree`。下一次别的会话选这条路径 → `Conflict`，`detail.reason = "assigned"` | 新增（design §6.5 第 3 步） |
| `diff_between_previews_both_sides` | 只在 A 的路径进 `only_a`，只在 B 的进 `only_b`，两边都有的带 hunk 预览。不占 repo 写锁，不写 ref | 新增（design §6.3） |
| `non_utf8_link_target_refused_before_commit` | 符号链接目标含非 UTF-8 字节 → `InvalidInput`，`detail.reason = "link_target"`，无 commit 对象 | 新增（"Recorded paths and link targets are UTF-8"） |
| `removed_session_is_nosuchsession` | 批次里的会话已 `Removed` → `NoSuchSession`，无 `publishes` 新行 | 新增（"A Removed session takes no new pins"） |

不保留与替代、理由点名本模块的 `—` 行。断言列写结果，不把 inode 语义写成要实现的行为。

| 测试名 | 断言 | 来源（矩阵行 或 新增） |
| --- | --- | --- |
| `no_per_job_kv` | 不保留。staging 是目录加 `gitio::merge3`，没有 per-job KV，也没有按角色预热的对象缓存 | `layerfs-overlay/tests/indexed_operation_record.rs::complete_keys_and_full_file_scopes_never_alias_other_custody`；`layerfs-workspace/tests/shared_cache_client.rs::constructed_objects_are_staged_by_role_within_a_bound_and_placed_without_a_fetch` |
| `edit_does_not_follow_directory_by_inode` | 不保留，理由：按路径，不按 inode。一边把目录改名，另一边改旧路径下的文件 → `Conflict`，编辑不出现在新路径上。千文件规模的「改名后编辑仍跟随」同样不保留，文本合并由 `two_sessions_nonoverlapping_hunks_merge_clean` 承担 | `layerfs-api/tests/merge_by_serial.rs::an_edit_follows_a_moved_directory`；`layerfs-api/tests/sdk_merge_scale.rs::m1_both_sides_change_1000_moved_files` |
| `unknown_is_not_replayed_without_a_container` | 替代。没有上传与 start。不确定的失败留下现场、不重放，由 `push_timeout_keeps_chain_reports_unknown` 与 `restart_reports_unknown_never_replays` 落实；daemon 负责重启时的报出 | `layerfs-sandbox/tests/engine_lifecycle.rs::lost_upload_and_start_keep_original_owner_and_refuse_replay` |

覆盖：目标模块为 `publish` 的矩阵行 43 行（3a 1、3c 7、3d 34、3e 1），加上理由点名 publish 的 `—` 行 5 行（3c 2、3d 3），共 48 行，全部出现在上面两张表的来源栏里。3b `:26` 的 Publish 是 LayerFS 步骤名，不计入。主表 63 个测试，其中来源栏以「新增」开头的 37 个；不保留表 3 个，不单独实现。

## 8. 实现任务（TDD）

六个任务按顺序做。§7 主表里的每个测试恰好落在下面某个任务的第一步。不保留表的三行不实现。公共脚手架在 `crates/strata-publish/tests/support/mod.rs`：裸仓库、`Git`、`Store`、一个记下持锁区间的 `PublishLocks`、可注入的 `publish_chain`。Task 1 建立它。

### Task 1: 链、锚点与合并基线

**Files:**
- Create: `crates/strata-publish/src/chain.rs`
- Test: `crates/strata-publish/tests/chain.rs`

**Interfaces:**
- Consumes: `deps::publish_chain`、`gitio::merge_base`、`gitio::is_ancestor`、§2 第 3 条的一次 ref 事务
- Produces: `chain_ref`、`anchor_ref`、`merge_base`、锚点前移

- [ ] **Step 1: 写失败的测试**：`merge_base_prefers_local_chain_when_ahead`、`anchor_advances_to_merge_base_not_origin_when_behind`、`anchor_does_not_advance_when_absent_or_rewound`、`chain_rewound_or_diverged_refuses_before_write`、`publish_branch_is_not_default_branch`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-publish --test chain`
Expected: FAIL（函数未定义）

- [ ] **Step 3: 写让测试通过的最少实现**：`merge_base` 只调 `deps::publish_chain`。前移用 gitio 的 `merge_base`，不用本文的包装。`Behind` 不写 `a = o`。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-publish --test chain`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-publish/src/chain.rs crates/strata-publish/tests/chain.rs crates/strata-publish/tests/support/mod.rs
git commit -m "feat(publish): chain ref and anchor advance"
```

### Task 2: staging 与路径级合并

**Files:**
- Create: `crates/strata-publish/src/stage.rs`
- Test: `crates/strata-publish/tests/stage.rs`

**Interfaces:**
- Consumes: `gitio::export_tree`、`attach_staging`、`stage`、`merge3`、`is_binary`；`overlay::lww_matches`；`store::checkpoint`
- Produces: `stage`、`Staged`

- [ ] **Step 1: 写失败的测试**：`stage_only_selected_paths`、`two_sessions_nonoverlapping_hunks_merge_clean`、`same_hunk_conflict_stops_whole_group`、`same_bytes_do_not_conflict`、`rename_both_sides_keeps_both_names_not_hardlinks`、`delete_against_edit_conflicts`、`delete_directory_against_move_keeps_moved`、`delete_against_disjoint_add_keeps_add`、`one_sided_exec_bit_kept`、`chmod_and_content_merge`、`merge_root_oracle_path_level`、`lww_later_batch_wins`、`lww_later_checkpoint_wins_within_session`、`deps_dirs_are_not_merge_units`、`type_transition_pair_required`、`file_to_symlink_is_one_modified`、`untracked_publishes_as_added`、`empty_directory_absent_from_tree`、`binary_and_oversize_conflict_without_byte_diff`、`hunk_text_cap_keeps_small_hunk_exact`、`no_commit_when_identical_to_base`、`special_mode_bits_never_written`、`staging_refuses_to_follow_a_symlink`、`stage_goes_through_gitio_stage`、`non_utf8_link_target_refused_before_commit`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-publish --test stage`
Expected: FAIL（函数未定义）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.3。index 只经 gitio。符号链接不跟随。超限文件不读内容。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-publish --test stage`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-publish/src/stage.rs crates/strata-publish/tests/stage.rs
git commit -m "feat(publish): path-level stage and merge"
```

### Task 3: 检查

**Files:**
- Create: `crates/strata-publish/src/check.rs`
- Test: `crates/strata-publish/tests/check.rs`

**Interfaces:**
- Consumes: `RepoConfig::validate_commands`、`checks`
- Produces: `check`、`ChecksRan`

- [ ] **Step 1: 写失败的测试**：`check_mutating_command_is_config_error`、`check_failed_does_not_commit`、`zero_checks_reports_zero_ran`、`sibling_layout_matches_workspace`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-publish --test check`
Expected: FAIL（函数未定义）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.5。先 `validate_commands`。cwd 是布局根下的仓库目录。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-publish --test check`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-publish/src/check.rs crates/strata-publish/tests/check.rs
git commit -m "feat(publish): read-only checks"
```

### Task 4: 提交、推送与不确定结果

**Files:**
- Create: `crates/strata-publish/src/push.rs`
- Test: `crates/strata-publish/tests/push.rs`

**Interfaces:**
- Consumes: `gitio::commit_tree`、§2 第 3 条的一次 ref 事务、`push`；`ConfirmToken`；`store::set_publish_phase`、`record_ref_change`
- Produces: `commit`、`push`、`Published`

- [ ] **Step 1: 写失败的测试**：`commit_cas_failure_is_not_retried`、`deploy_on_push_without_confirm_stops_before_push`、`push_timeout_keeps_chain_reports_unknown`、`push_refused_keeps_local_chain`、`push_false_leaves_chain_local`、`pushed_sets_anchor_to_pushed_commit`、`no_flag_or_env_bypass`、`injected_failure_after_commit_is_unknown`、`second_attempt_does_not_overwrite`、`uncertain_cleanup_stays_unknown`、`restart_reports_unknown_never_replays`、`pre_write_wait_is_busy_post_write_wait_is_not`、`removed_session_is_nosuchsession`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-publish --test push`
Expected: FAIL（函数未定义）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.2 第 3 步的凭据闸门在 `attach_staging` 之前；第 6–7 步与 §5.6、§6。凭据只核对 `action`。超时留链。链与锚点一次事务。记录 `Pushed` 的调用同时写远端跟踪 ref 和锚点，提交之前不是 `Pushed`。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-publish --test push`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-publish/src/push.rs crates/strata-publish/tests/push.rs
git commit -m "feat(publish): commit, push, and unknown"
```

### Task 5: 发布后 rebase、assign 与 diff

**Files:**
- Create: `crates/strata-publish/src/rebase.rs`
- Test: `crates/strata-publish/tests/rebase.rs`

**Interfaces:**
- Consumes: `deps::key_of`、`propose`、`is_input`、`observe_main`；`session::move_workspace`、`ensure_base_for`；`overlay::rebase`
- Produces: `rebase_session`、`assign`、`diff_between`、`RebaseReport`

- [ ] **Step 1: 写失败的测试**：`post_publish_rebase_drops_published_paths`、`later_edits_survive_rebase`、`rebase_waits_when_deps_must_change`、`landed_draft_promoted_under_repo_lock`、`partial_input_publish_does_not_promote`、`first_lander_keeps_exact_tree`、`open_files_does_not_fail_a_landed_publish`、`observe_failed_is_a_warning`、`publish_does_not_build_while_holding_repo_lock`、`failed_publish_keeps_overlay`、`rebase_failure_still_reports_the_commit`、`assign_names_resolver_and_does_not_push`、`diff_between_previews_both_sides`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-publish --test rebase`
Expected: FAIL（函数未定义）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.7、§5.8、§5.9。`ensure` 在锁外。`propose` 在锁里。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-publish --test rebase`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-publish/src/rebase.rs crates/strata-publish/tests/rebase.rs
git commit -m "feat(publish): rebase, assign, and diff"
```

### Task 6: 并发

**Files:**
- Test: `crates/strata-publish/tests/concurrent.rs`

**Interfaces:**
- Consumes: Task 1–5 的入口 `publish`
- Produces: 无新签名

- [ ] **Step 1: 写失败的测试**：`disjoint_concurrent_publishes_all_land`、`conflict_already_landed_stops_the_group`、`concurrent_publish_matches_serial_oracle`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-publish --test concurrent`
Expected: FAIL（锁还没把两次发布串起来时，同路径的两次会同时写链）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.1 的持锁范围。合并与比较并交换在同一段锁里，只合并一次。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-publish --test concurrent`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-publish/tests/concurrent.rs crates/strata-publish/src/lib.rs
git commit -m "feat(publish): one merge under the repo lock"
```

## 9. 构建者决定记录

| 决定 | 选择 | 理由 | 日期 |
| --- | --- | --- | --- |
| D1 `merge_base` 的形状 | 薄包装。只在 `Leads` 时返回链顶，其余返回 origin 的 tip。没 fetch → `NoSuchBranch` | 计划要这个名字。判定只有 deps 一份，两份会在锚上分叉 | 2026-10-23 |
| D2 不自动重放回退或分叉的链 | `InvalidInput`，`chain_rewound` / `chain_diverged`。显式命令归 T14 | 自动重放会把被人拿掉的提交推回去。裁定把「是否自动」交给 T10 | 2026-10-23 |
| D3 零条检查 | 成功，并写出 `checks_ran: 0` | config.md 把这件事交给本文。静默跳过会让人以为检查跑过了 | 2026-10-23 |
| D4 staging 的位置 | 最小 `SessionId` 的会话根下，`.strata-publish-<repo>-<release-id>/`。仓库在其中的 `<repo>/`，兄弟是所钉基线的克隆，不是活的工作区 | 草稿要求会话根、真实目录、兄弟布局。多会话组没有唯一的根，取最小 id。克隆兄弟是为了检查的 `../` 不写进活的工作区 | 2026-10-23 |
| D5 hunk 文本上限 | 4096 字节，UTF-8 字符边界。超限的带 `truncated`，同冲突里的小 hunk 保持原文 | 裁定要求本文定上限。3d 的那一行要求小 hunk 精确 | 2026-10-23 |
| D12 导出在锁里 | `export_tree` 的对象是锁里刚读到的 B，在同一段锁里导出再合并。导出在凭据闸门和内存冲突判定之后 | 锁外导出再合并，会让并发的不相交发布得到 `HeadMoved`，与 3d 的「全部落地」矛盾。闸门之前不建目录 | 2026-10-23 |
| D16 具名拒绝的相位 | 目录已删完、或还没建目录时，返回具名码之前把 `Running` 收成 `Done`，`outcome.stopped` 记下那个码。删不完则行保持 `Unknown`，不收成 `Done` | `publishes` 只允许进 `Done` 或 `Unknown`。留着 `Running`，重启会把一次干净的 `Conflict` 报成不确定。删不完的 gitfile 不能靠收成 `Done` 假装没有 | 2026-10-23 |
| D17 合并基线取锁里的读 | 锁外的计划与锁里的 `publish_chain` 不一致时，用锁里的 B 做这一次合并，不因此 `HeadMoved`。对象已写入之后，对外是 `Unknown`，`HeadMoved` 只在 `detail` 里 | 草稿写「与计划不一致就 `HeadMoved`」。3d 的并发行要求取锁前落地的不相交发布并进这一次合并、全部落地。两处不能同时成立，矩阵行的断言取胜。草稿的「不重新合并、不重试」留在比较并交换失败上 | 2026-10-23 |

草稿里的 D1、D2、D3 保持原选择。D11（两次 `update_ref`，失败的那次靠下一次前移补上）撤回：它覆盖了「链与锚点同一次 ref 事务」的裁定。上游要求在 §2 第 3 条。
