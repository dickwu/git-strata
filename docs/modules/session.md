# `session` 模块

**Crate**：`crates/strata-session` · **设计来源**：design.md §3、§4、§5、§6.1、§6.3、§6.4、§6.5 第 7 步、§6.6、§6.7、§6.8、§6.9、§7、§8、§9 · **矩阵**：目标模块为 `session` 的行，加上理由点名 session 的 `—` 行，共 51 行：3b 13 行、3c 4 行、3d 29 行（含裁定改归 session 的 `:176`、`:178`）、3e 5 行；3a、3f 没有。逐条见 §7

> 本文按 module-rulings.md 写，与 design.md 的旧措辞不同的地方以裁定为准：工作区路径按会话 id 取，即 `<root>/sessions/<session-id>/<repo>/`，ticket 只是标签；不变量 4 优先于 design §6.4 的最后一句；写入一旦开始，任何失败都是 `Unknown`。标 [我的决定] 的地方是 design 和裁定都没有定、由本文决定的，报告里有汇总。

## 1. 职责与边界

session 负责：

- **会话生命周期**：`new` 建会话，`status` 报状态，`remove` 删会话（design §6.1、§6.3、§6.6）。
- **兄弟布局**：配置里的每个仓库在会话根下都有一个工作区目录，目录名就是 `RepoName`，所以 `../quick-start`、`../release-notes`、`../node_modules` 这类相对路径在会话里照常成立（design §3）。
- **基线的取得与钉住**：每个仓库取发布链顶或 `origin/<default_branch>` 的 commit，导出基线树、备好依赖集，再克隆进工作区（design §6.1、§6.7）。
- **种子缓存与种子文件**：从主 checkout 克隆或复制，记录来源与时间（design §4）。
- **工作区里的 git**：HEAD detached、没有 upstream、装 `pre-push` 钩子；工作区的 git 不写主 checkout（design §6.8、不变量 2）。
- **会话识别**：按 cwd 识别会话（`resolve`），以及会话选择器（id 或 ticket，`select`）。
- **`move_workspace`**：基线与依赖一起移动的原子交换，并定义它的结果 `MoveReport`（types.md §2.1、§8；design §6.4、§6.5 第 7 步）。
- **自己操作留下的现场**：`new`、`move_workspace`、`remove` 的 `Unknown` 现场由 session 记录、在 daemon 重启后认领（`recover`），并在 `status` 里报出（不变量 6）。

session 不负责的事，以及各归哪个模块：

- overlay 的计算、checkpoint 的写入、restore、整文件三方投影：overlay。session 只在移动和删除之前调它记 checkpoint、取 overlay。
- 依赖集的安装输入哈希、认领、冷装、漂移判定、变更提议、rollout 计划，以及枚举打开的文件：deps。session 调用这些函数，从不自己判断依赖该不该换。
- git 子进程、tree 导出、ignore 判定、ref 的比较并交换：gitio。
- clonefile、目录遍历、同卷判定：apfs。
- SQLite 表结构、blob、generation 的引用计数与 gc，包括 `rm` 前那个 checkpoint 的保留期：store。
- 发布、发布链，以及发布后要不要移动、移动不了时记提议：publish。它是 `move_workspace` 的调用方之一。
- 锁、排队、监听、RPC、启动时机：daemon。人类确认的弹框：confirm。TTY 检查：cli。
- 配置的解析与静态校验：types 与 config.md。其中 V3（`paths.root` 与主 checkout 互不嵌套，包括经符号链接别名的写法）在 daemon 加载配置时检查，§7 把这两条测试列为 config 级。

## 2. 依赖

| 依赖 | 用途 |
| --- | --- |
| `strata-types` | 标识、`BaseGen`、`SessionManifest`、`WorkspaceState`、`CheckpointReason`、`ConfirmToken`、`Action`、`Config`、`RepoConfig`、`StrataError`、`Code` |
| `strata-apfs` | `clone_tree`、`clone_file`（工作区、依赖目录、种子缓存、种子文件、投影时的 blob），`same_volume`（`new` 的同卷预检），`walk`（`move_workspace` 的携带遍历），`open_rooted`（删除时逐级 `O_NOFOLLOW`） |
| `strata-gitio` | `Git::fetch`、`Git::tree_of`、`Git::export_tree`、`Git::is_ignored`、`Git::update_ref`（pin ref 的比较并交换），以及 `Git::command`：经它的干净环境跑 `read-tree`、`update-index --refresh`、`ls-tree`、`rev-parse --verify`、`merge-base`、`init --bare`、`config` |
| `strata-store` | `Store`：会话、工作区、种子、现场的读写（字段见 §4.3），最新 checkpoint 与依赖集的读取，`get_blob` |
| `strata-overlay` | `checkpoint`（`BeforeRollout`、`BeforeRemove`），`reconcile_full`（`remove` 的未发布改动检查、移动后对旧树的核对），`rebase` 与 `Overlay`（移动时的三方投影） |
| `strata-deps` | `inputs_hash`、`adopt`、`build`（`new` 的依赖集），`open_files`（`remove` 与 `move_workspace` 的打开文件检查），`Proposal` 与待确认提议的列出（`status`） |
| `serde`、`serde_json` | §3 数据类型的序列化，以及 `StrataError.detail` |
| `libc` | `statfs`（卷类型）、`fcntl(F_GETPATH)`（`resolve` 的规范路径）、`renamex_np` 加 `RENAME_SWAP`（原子交换）、`openat`/`unlinkat`（删除）、`arc4random_buf`（会话 id 的随机数） |

依赖符合 types.md §8：session 在第 3 层，只依赖第 0–2 层，不依赖 publish、search、daemon、confirm、cli。上面写的上游函数名取自计划的 Interfaces 与 module-rulings.md；参数列表以各模块 §3 为准，报告列出了 session 对它们的要求。

谁调用本模块：

- daemon：调用全部公开函数。处理 `deps approve` 时，它逐会话调用 `move_workspace`；启动时调用一次 `recover`。
- publish：`publish::rebase_session` 调用 `move_workspace`。
- search：调用 `resolve`（计划 Task 11）。
- cli：只经 daemon 的 RPC 间接调用。daemon 不在时，cli 可以只读地调用 `resolve`、`select`、`list`、`status`，它们只读 Store、文件和 git。

## 3. 公开接口（Rust）

```rust
// crates/strata-session/src/lib.rs
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use strata_deps::Proposal;
use strata_store::Store;
use strata_types::{
    BaseGen, CheckpointId, CommitOid, Config, ConfirmToken, RepoConfig, RepoId, RepoName, RepoPath,
    SessionId, SessionManifest, StrataError,
};

/// types.md §8 的 `Result<T>`。
pub type Result<T> = std::result::Result<T, StrataError>;

/// session 的句柄：借用配置、RepoName → RepoId 映射与 Store。
/// 不持有锁，不缓存会话状态，每次调用都读 Store。它不是数据，所以不 derive serde。
pub struct Sessions<'a> { /* config、repo_ids、store：私有 */ }

impl<'a> Sessions<'a> {
    pub fn open(
        config: &'a Config,
        repo_ids: &'a BTreeMap<RepoName, RepoId>,
        store: &'a Store,
    ) -> Result<Sessions<'a>>;

    pub fn new(&self, ticket: Option<&str>, repos: &[RepoName]) -> Result<SessionManifest>;
    pub fn resolve(&self, cwd: &Path) -> Result<Option<(SessionId, RepoName)>>;
    pub fn select(&self, selector: &str) -> Result<SessionId>;
    pub fn list(&self) -> Result<Vec<SessionId>>;
    pub fn status(&self, id: &SessionId) -> Result<SessionStatus>;
    pub fn remove(
        &self,
        id: &SessionId,
        discard: bool,
        confirm: Option<ConfirmToken>,
    ) -> Result<RemoveReport>;
    pub fn move_workspace(
        &self,
        session: &SessionId,
        repo: &RepoId,
        new_base: BaseGen,
        head: CommitOid,
    ) -> Result<MoveReport>;
    pub fn repo_name(&self, repo: &RepoId) -> Result<RepoName>;
    pub fn recover(&self) -> Result<Vec<SessionId>>;
}

/// 把 `repo_cfg.seed_files` 从主 checkout `from` 复制到工作区 `to`，返回复制了的那些。
pub fn seed_files(repo_cfg: &RepoConfig, from: &Path, to: &Path) -> Result<Vec<SeedRecord>>;

/// 一个会话的状态（design §6.3）。JSON 形状由 cli.md 引用。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionStatus {
    pub manifest: SessionManifest,
    pub state: SessionState,
    #[serde(with = "strata_types::serde_rfc3339")]
    pub created: SystemTime,
    pub workspaces: BTreeMap<RepoName, WorkspaceStatus>,
    /// 这个会话上记下的未决现场（不变量 6）。
    pub scenes: Vec<Scene>,
}

/// 会话的状态（§4.3 的状态机）。序列化为变体名字符串。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionState { Creating, Active, Removing, Removed, Unknown }

/// 一个工作区的健康状况。Missing 直接看文件系统；Unknown 来自中途失败的 move_workspace 或 recover。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspaceHealth { Ok, Missing, Unknown }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceStatus {
    pub health: WorkspaceHealth,
    /// 最新的 checkpoint；tracked = false 的仓库没有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<CheckpointId>,
    /// 最新 checkpoint 的 overlay 里的路径；改名只列 path。
    pub changed: Vec<RepoPath>,
    /// 工作区算出的安装输入哈希，只在它与所钉 DepsSet.inputs_hash 不同时出现（漂移是数据，不是错误）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drift: Option<String>,
    /// 这个 repo 下 from 等于本工作区依赖集的待确认提议（design §6.4）。
    pub pending: Vec<Proposal>,
    /// 最近一次 move_workspace 记下、仍在 overlay 里的冲突路径。
    pub conflicts: Vec<RepoPath>,
    pub seeds: Vec<SeedRecord>,
    pub warnings: Vec<StatusWarning>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum StatusWarning {
    /// tracked = false：这里的编辑不进 checkpoint，rm 需要 --discard。
    NotCheckpointed,
    /// 主 checkout 的源文件在复制之后变过，或已不在。
    SeedStale { path: RepoPath },
    /// 工作区的 ignore 规则已不再 ignore 这个种子文件，它会以 Untracked 进 overlay。
    SeedNotIgnored { path: RepoPath },
    /// move_workspace 交换后保留的旧树。changed：交换后有人改过它。
    OldTreeKept { path: PathBuf, pids: Vec<u32>, changed: bool },
}

/// 一个未决现场：哪个操作、停在哪个阶段、留下了什么、当时返回的错误。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scene {
    pub op: SceneOp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<RepoName>,
    pub phase: String,
    #[serde(with = "strata_types::serde_rfc3339")]
    pub at: SystemTime,
    pub kept: Vec<PathBuf>,
    pub error: StrataError,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SceneOp { New, Move, Remove }

/// 一个复制过来的种子文件。只有来源与时间，从不含内容（design §4、config.md §2.3）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeedRecord {
    pub path: RepoPath,
    /// 主 checkout 里的绝对路径。
    pub source: PathBuf,
    /// 种子文件本身是符号链接时，链接的目标（原样复制，从不跟随）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<PathBuf>,
    pub size: u64,
    #[serde(with = "strata_types::serde_rfc3339")]
    pub source_mtime: SystemTime,
    #[serde(with = "strata_types::serde_rfc3339")]
    pub copied: SystemTime,
}

/// move_workspace 的结果（types.md §2.1：所有者 session，使用者 daemon、publish）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveReport {
    pub session: SessionId,
    pub repo: RepoName,
    pub from: BaseGen,
    pub from_head: CommitOid,
    /// 等于调用参数 new_base 与 head。
    pub to: BaseGen,
    pub head: CommitOid,
    /// 交换之前记的 BeforeRollout checkpoint；from == to 时什么都没做，为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<CheckpointId>,
    /// 离开 overlay 的路径：新基线里已经是会话的内容（已发布，或两边改成了一样）。
    pub dropped: Vec<RepoPath>,
    /// 三方投影有冲突、保留了会话版本的路径（design §6.5 第 7 步"冲突标出"）。
    pub conflicts: Vec<RepoPath>,
    /// 旧工作区因仍被使用或交换后被改过而保留时，它现在的位置。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_tree: Option<PathBuf>,
}

/// remove 的结果。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveReport {
    pub session: SessionId,
    pub discarded: bool,
    /// 每个 tracked 仓库删除前记的 BeforeRemove checkpoint，按保留期留存（design §6.6）。
    pub checkpoints: BTreeMap<RepoName, CheckpointId>,
}

/// 只在测试里打开的注入口。
#[cfg(feature = "test-seams")]
impl<'a> Sessions<'a> {
    /// 下一次 new 用这个 id，不随机生成（用来测 Exists 与 id 不复用）。
    pub fn set_next_id(&self, id: SessionId);
    /// 在名为 phase 的阶段开始时生效：Some(errno) 注入一个该 errno 的 I/O 错误；
    /// None 让当前操作就地返回、不更新任何状态，模拟进程在这里崩溃。
    pub fn set_failpoint(&self, phase: &str, errno: Option<i32>);
}
```

types.md §8 写的 `session::move_workspace(session: &SessionId, repo: &RepoId, new_base: BaseGen, head: CommitOid) -> Result<MoveReport>` 就是上面的 `Sessions::move_workspace`：参数和返回类型逐字一致，接收者 `&self` 带着配置和 Store。计划里不带 `Result` 的签名（`new`、`resolve`、`status`、`remove`、`seed_files`）在这里都加上了 `Result`，因为它们都会读写 Store 或文件。

各函数的语义与可能返回的 `Code`：

- **`open`**：校验 `repo_ids` 的键与 `config.repos` 的键完全相同，不做 I/O。映射由 daemon 在加载配置时算出（§4.4）。码：`InvalidInput`。
- **`new`**：建一个会话。配置里的每个仓库都克隆成兄弟布局里的工作区，`repos` 点名的那些标为 tracked；返回 `SessionManifest`。流程见 §5.1。写入之前的具名拒绝：`InvalidInput`、`ReservedName`、`NoSuchRepo`、`NoSuchPath`、`NotDirectory`、`IsDirectory`、`NotFile`、`CrossVolume`、`Unsupported`、`Exists`。gitio、deps、store 在写入之前给出的码原样透传，例如 `NoSuchBranch`、`NoSuchCommit`、`Unsupported`、`NotText`。写入开始之后的任何失败：`Unknown`。
- **`resolve`**：cwd 在某个会话的某个工作区里（含子目录）时返回 `Some((id, repo))`，否则 `Ok(None)`。会话根本身、根下不属于任何仓库的条目、已删除的会话，都返回 `Ok(None)`。Store 读不出来时原样透传（如 `Corrupt`）。命令默认作用于"本会话"而得到 None 时，由调用方返回 `NoSuchSession`。
- **`select`**：选择器是会话 id 就按 id 找，否则当作 ticket 找。码：`NoSuchSession`、`AmbiguousMatch`（`detail.candidates`）、`InvalidInput`（既不是 id 也不是合法的 ticket）。
- **`list`**：没有删除的会话，包括 `Creating` 与 `Unknown` 现场，按创建时间排序。只透传 Store 的码。
- **`status`**：见 §5.4。工作区缺失时报 `Missing`，本身不失败。码：`NoSuchSession`（不存在或已删除）；透传 Store 的码。
- **`remove`**：删除会话，删前记 `BeforeRemove` checkpoint。见 §5.5。写入之前的具名拒绝：`NoSuchSession`、`NotConfirmed`、`OpenFiles`、`Dirty`；写入之前 overlay 或 store 给出的码原样透传；写入开始之后：`Unknown`。`confirm` 按值收下，核对 `action == Action::RemoveDiscard { session: id }`（types.md §4）。
- **`move_workspace`**：把一个工作区的基线与依赖一起移到 `new_base`，HEAD 移到 `head`。调用方持有该 repo 的写锁。见 §5.6。写入之前的具名拒绝：`NoSuchSession`、`NoSuchRepo`、`InvalidInput`、`NoSuchPath`、`NoMergeBase`、`OpenFiles`；工作区已是未决现场时以 `Unknown` 拒绝，什么都不做；写入之前 gitio、overlay 给出的码原样透传；交换开始之后：`Unknown`。
- **`repo_name`**：RepoId → RepoName。码：`NoSuchRepo`。
- **`recover`**：daemon 启动时调用一次。把上次进程中断时仍在 `Creating`/`Removing` 的会话、有未完成移动意图的工作区记成 `Unknown`，返回受影响的会话 id；不碰文件系统。见 §5.7。只透传 Store 的码。
- **`seed_files`**：按 §5.2 的规则判定并复制种子文件。先判定全部条目，再复制：判定阶段的拒绝有 `IsDirectory`、`NotFile`、`InvalidInput`；复制开始之后的失败（含目标已存在的 `Exists`）一律是 `Unknown`。

## 4. 数据与不变量

### 4.1 目录布局

```text
<paths.root>/
  bases/<repo>/<tree>/                 基线源码树：gitio::export_tree 导出，只读，由 store 的 gc 回收 [我的决定：只放源码]
  deps/…                               封存的依赖集，布局归 deps.md
  sessions/
    .repos/<repo>.git/                 影子仓库：每个 repo 一个，所有会话共用（§4.2）
    .trash/<session-id>-<unix 秒>/      remove 先把会话根改名到这里，再删
    <session-id>/                      会话根 = SessionManifest.root
      <repo>/                          工作区 = WorkspaceState.path；配置里的每个 repo 都有
      .strata-tmp-move-<repo>-<n>/     move_workspace 正在准备的新树（私有临时）
      .strata-old-<repo>-<n>/          交换后因仍被使用或被改过而保留的旧树
      node_modules/…                   tool 与 front 的 dev-link 在布局根自己建的，不归 strata
每个主 checkout 的 git common dir：
  refs/strata/pins/<session-id>        该会话该 repo 的 head，防主 checkout 的 git gc（§4.2）
```

- 工作区里只有被克隆或复制进来的东西，没有 strata 建的符号链接：依赖目录、种子缓存、种子文件都用 clonefile（design §3：Turbopack 拒绝 symlink 的 `node_modules`）。基线树里本来就有的符号链接原样保留。
- 一个工作区 = 基线源码树的克隆，加上依赖集 `dirs` 每一项的克隆；依赖集为空时只有前者。design §6.1 第 1 步说"把匹配的依赖集克隆进"基线缓存；本文把这一步放到工作区层面做 [我的决定]。原因是同一个 tree 可以配不止一个依赖集（平台、工具链都进依赖集的键），而 generation 永不原地修改（不变量 5），所以 `bases/<repo>/<tree>/` 只放源码，供这个 tree 的所有依赖集共用。基线 generation 的键仍是 (repo, tree, 依赖集)，由 Store 记录与计数。
- 种子缓存只克隆给 tracked 仓库 [我的决定]。种子缓存只是让第一次构建不从零开始（design §4）。`tracked = false` 的兄弟库是被相对路径读取的钉住副本，不在这里构建；而每个 Tauri 应用的 `src-tauri/target` 可达几十到 200 GB，给每个兄弟库都克隆一份，会让 `new` 慢上几分钟。依赖目录则照 design §6.1 第 1 步，给每个仓库都克隆，否则基线与依赖集就配不成对（不变量 8）。

### 4.2 工作区里的 git：影子仓库 [我的决定]

design §6.1 第 2 步要求工作区的 `.git` "以 `git worktree add --detach` 等价的方式"接到主 checkout 的 object store；design §12 把接法（worktree，还是独立的 `.git` 文件指向 common dir）交给构建者。本文选择**每个 repo 一个影子仓库**，工作区是它的 linked worktree，它借主 checkout 的对象库读对象：

- 位置 `<paths.root>/sessions/.repos/<repo>.git/`，`git init --bare` 建成，`core.bare = true`，`core.repositoryformatversion = 0`，不设任何 `extensions.*`（SHA-256 仓库早被 gitio 拒绝）。首次用到时建立，此后每次 `new` 刷新一次镜像，内容不变就不写。写它要持有该 repo 的写锁。
- `objects/info/alternates` 写主 checkout 的 git common dir 下 `objects` 的规范路径：读对象经它，不复制对象。工作区里新建的对象（会话自己的 `git add`、`git commit`）写进影子仓库自己的 `objects/`，不进主 checkout 的对象库。
- `config` 只从主 checkout 的仓库级 config 复制这些键：`core.ignorecase`、`core.precomposeunicode`、`core.autocrlf`、`core.eol`、`core.safecrlf`、`core.symlinks`、`core.filemode`、`core.checkRoundtripEncoding`，以及 `filter.*`、`diff.*`、`merge.*` 整节。从不复制 `remote.*`、`branch.*`、`core.hooksPath`、`core.worktree`、`core.bare`、`include.*`、`includeIf.*`、`extensions.*`。用户级与系统级 config 由 git 照常读取。
- `info/exclude` 与 `info/attributes` 逐字节镜像主 checkout 的同名文件，主 checkout 没有就不建。linked worktree 的这两个文件取自 common dir；不镜像的话，只靠主 checkout 的 `info/exclude` 被 ignore 的文件（例如某个仓库这样 ignore 了 `.env`）在工作区里会变成未跟踪，进而进入 overlay。
- `hooks/pre-push`：环境里没有 `STRATA_PUBLISH=1` 就以非零退出，并往 stderr 打一行 `git-strata: push refused in a strata workspace; publish with git-strata publish`。这个钩子在影子仓库里，只对 strata 的工作区生效，主 checkout 的 `hooks/` 一字不动。
- 管理目录 `worktrees/<session-id>.<n>/`：每个会话的每个 repo 一个。n 是代数，`new` 时为 1，每次 `move_workspace` 加 1，这样 git 的状态能随目录一起原子交换（§5.6）。目录里有：`HEAD`（一行 commit id，即 detached HEAD）、`commondir`（`../..`）、`gitdir`（工作区 `.git` 的规范路径）、`locked`（内容 `git-strata session <session-id>`，让 `git worktree prune` 永远不清理它）、`index`（由 `read-tree` 建）。
- 工作区根下的 `.git` 是一行 gitfile：`gitdir: <管理目录的规范路径>`。overlay 不看 `.git`，search 的视图排除这个 gitfile（module-rulings T11）。
- 每个主 checkout 里每个会话一个 pin ref `refs/strata/pins/<session-id>`，指向该会话在这个 repo 的 head：主 checkout 的 `git gc` 看不到影子仓库的 HEAD，有了 pin，被引用的 commit 与 tree 就不会被回收。这是 session 对主 checkout 的唯一写入，落在不变量 2 允许的 `refs/strata/*` 之内。

为什么不直接在主 checkout 上 `git worktree add`：

1. linked worktree 的 hooks 与 config 取自 common dir（gitrepository-layout 对 `hooks` 的说明："This directory is ignored if $GIT_COMMON_DIR is set"）。接在主 checkout 上，`pre-push` 只能装进主 checkout 的 `hooks/`，或者改它的 config（打开 `extensions.worktreeConfig`）。两者都违反不变量 2，前者还会挡住人类自己的 push。
2. refs 是共用的：会话里的 `git stash`、`git branch` 会出现在主 checkout 里。
3. front 的 `dev-link.sh` 会对每个干净、能 `fetch origin`、又没有未推送 commit 的兄弟库执行 `git reset --hard origin/main`（`front/dev-link.sh:13-28`）。如果接在主 checkout 上，兄弟工作区能解析 `origin/main`，在会话里跑一次 `bun run dev`，钉住的兄弟工作区就被悄悄挪到 `origin/main`，违反不变量 8。影子仓库没有 remote，`fetch origin` 失败，dev-link 打印 "offline / no remote — using local copy" 后跳过。
4. `git worktree add` 要求目标目录不存在或为空，而工作区是先用 clonefile 克隆出来的。

2026-10-22 在 `/tmp` 用 git 实测过这套接法：用影子仓库加 alternates、手写的管理目录，跑完 `read-tree` 和 `update-index --refresh` 之后 `git status --porcelain` 为空，被 ignore 的 `node_modules` 和 `.env` 不出现。HEAD 是 detached 的，`git log` 经 alternates 读到主 checkout 的历史。`git fetch origin` 失败，`origin/main` 也解析不到。影子仓库的 `pre-push` 拒绝了推送，而主 checkout 的 `hooks/` 里只有样例文件。工作区里新 commit 的对象在影子仓库里，主 checkout 的对象库里没有。主 checkout 的 `git worktree list` 看不到这个工作区。作为对照，主 checkout 上的普通 linked worktree 里，`git rev-parse --git-path hooks` 指向主 checkout 的 `.git/hooks`。

别的模块需要知道的后果：

- 在工作区里跑 `git rev-parse --git-common-dir`，得到的是影子仓库，不是主 checkout 的 common dir。strata 从不由工作区里的 git 推出 `RepoId`（§4.4）。
- tgrep-core 的仓库身份是 `--git-common-dir` 规范路径的哈希（`tgrep-core/src/generations/mod.rs`），而工作区视图要求和基线 generation 是同一个身份（`tgrep-core/src/worktrees.rs`："root must be a worktree root in the pinned repository"）。所以 search 必须以影子仓库为仓库来建共享基线索引。所有会话共用一个影子仓库，基线索引照样跨会话共享。
- 工作区里引用远端分支的命令（`git log origin/main`、`git pull`、`build.sh` 开头的 `git pull`）都会失败，这是有意的：发布只经 strata。

### 4.3 Store 里的会话数据

DDL 由 store.md 定，下面是 session 读写的字段：

- **会话**：id（永不复用）、ticket、root、state（`SessionState`）、创建时间、删除时间、phase（最近记下的阶段名）、scene（未决时的 `Scene`，JSON）、plan（`new` 写意图时每个仓库的 head、tree、依赖集、tracked）。
- **工作区**（每个会话每个 repo 一行）：base 的 tree 与依赖集、head、path、tracked、管理目录代数 n、health（`Unknown` 时附带 `Scene`）、移动意图（JSON：新旧 base、head、临时路径、n）、冲突路径、保留的旧树（路径与 `changed`）。
- **种子**：(会话, repo, path) → `SeedRecord` 的其余字段。
- **状态机**：
  - `Creating` → `Active`；中途失败 → `Unknown`。
  - `Active` → `Removing` → `Removed`；中途失败 → `Unknown`。
  - `Unknown` 只能经 `remove` 走到 `Removing` → `Removed`。
  - 工作区的 health 只在 `move_workspace` 和 `recover` 里变成 `Unknown`，也只有 `remove` 能清掉它。`Removed` 的行永久保留，它的 id 永不复用。
- `SessionManifest` 由会话行和工作区行拼成。按 types.md §4 末段，创建时间与状态不在清单里，由 `status` 报出。
- 引用计数（design §7）：没有删除的会话，其工作区行引用 (repo, tree, 依赖集)；`BeforeRemove` checkpoint 引用的部分由 store 按保留期处理。

### 4.4 RepoId 与 RepoName

- `RepoName` 是配置的表名，也是兄弟布局里的目录名；`BaseGen.repo`、清单的键、工作区路径都用它。`RepoId` 是主 checkout 的 git common dir 的规范路径（types.md §3）。
- 二者经配置一一对应。daemon 加载配置时，对每张 `[repo.<name>]` 的 `path` 解析出 git common dir 并取 realpath，得到 `RepoId`；两张表解析到同一个 `RepoId` 时整份配置以 `NameTaken` 拒绝。这份映射传给 `Sessions::open`。
- `move_workspace` 收 `RepoId`，因为它的调用方（daemon 的 `deps approve`、publish 的 rebase）以仓库身份识别仓库。session 先映射成 `RepoName` 再查清单：映射不到，或者清单里没有这个仓库 → `NoSuchRepo`；`new_base.repo` 与映射出的名字不同 → `InvalidInput`。
- 工作区的 `RepoId` 只经清单得到：工作区路径 → `RepoName` → 映射。session 从不在工作区里跑 git 推出它，因为在 §4.2 的接法下那会得到影子仓库。

### 4.5 不变量

| # | 不变量 | 落实 design §9 |
| --- | --- | --- |
| S1 | session 对主 checkout 的唯一写入是 `refs/strata/pins/<session-id>`。工作区的 refs、对象、hooks、config 都在影子仓库里；种子文件和种子缓存只从主 checkout 读出 | 不变量 2 |
| S2 | 对每个工作区：`base.deps` 就是克隆进来的依赖集；`gitio::tree_of(head) == base.tree`；base、head、目录内容三者只经 `move_workspace` 的一次交换一起改变 | 不变量 8 |
| S3 | `bases/<repo>/<tree>/` 和封存的依赖集永不被修改；工作区是 CoW 克隆，在工作区里写不会改到它们 | 不变量 5 |
| S4 | 改写一个已有工作区之前先记 checkpoint：`move_workspace` 记 `BeforeRollout`，`remove` 记 `BeforeRemove`（只对 tracked 仓库；`new` 建的是全新的工作区） | 不变量 1 |
| S5 | 写入开始之后的失败一律是 `Unknown`：现场原样保留、记下来，`status` 报出，daemon 重启后照样报出；session 从不重放、清理或回滚它 | 不变量 6 |
| S6 | `remove(…, discard = true, …)` 只接受 `action` 为 `RemoveDiscard { session: 本会话 }` 的凭据；没有任何参数、环境变量或配置能代替它 | 不变量 7 |
| S7 | 依赖目录、种子缓存、种子文件从不以符号链接的形式进入工作区 | design §3、§4（为不变量 4、8 的"依赖目录归会话所有"服务） |
| S8 | 依赖目录只在 `move_workspace` 里被替换，而且替换前查过依赖目录上打开的文件；调用方只在 `deps approve` 之后、或者提出变更的会话自己要求时（design §6.4 第 3 步）才会传入新的依赖集 | 不变量 4 |
| S9 | 会话 id 永不复用（已删除会话的行永久保留）；ticket 只是标签，不进任何路径 | design §11 第 18 条 |
| S10 | 配置里的每个仓库在会话根下都有 `<repo>/`；工作区的 HEAD 是 detached 的，没有 upstream，`pre-push` 在没有 `STRATA_PUBLISH=1` 时拒绝 | design §3、§6.8 |

## 5. 流程与算法

下文用【锁 repo】标出要求调用方持有该 repo 写锁的步骤，【写】标出对外可见的写入，【rename】标出原子改名。session 自己不取锁、不等锁，`Busy` 由 daemon 返回（module-rulings T13）。最简单的持锁方式是：daemon 在调用 `new`、`remove` 之前，按 `RepoName` 顺序取得涉及的全部仓库的写锁，返回后释放；按顺序取锁是为了防死锁。具体由 daemon.md 定。

### 5.1 `new(ticket, repos)`

**阶段 A：校验与解析。** 只读，这一阶段的失败都是具名拒绝，什么都不写。

1. ticket 必须匹配 `^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`，否则 `InvalidInput`（`detail = {field: "ticket", value}`）。形如会话 id（`^[sS]-[A-Za-z2-7]{8}$`，不分大小写）的 ticket → `ReservedName`。这条白名单是 [我的决定]：ticket 不再是目录名，但它会出现在选择器里，`publish --from <session>:<path>` 要求它不含 `:`；`../foreign` 这类写法因此被拒（3e `packaging::lazy`）。`repos` 为空 → `InvalidInput`；名字不在配置里 → `NoSuchRepo`；重复的名字合并为一个。
2. 对配置里的**每个**仓库（不只是 `repos`）`lstat(path)`：不存在 → `NoSuchPath`（`detail = {repo, field: "path", path, errno}`）；是符号链接 → `NotDirectory`（`reason: "symlink"`）；是别的非目录 → `NotDirectory`。`paths.root` 存在时按同样的规则检查。符号链接一律拒绝是 [我的决定]：同卷、互不嵌套、对主 checkout 只读这几条保证都是按配置里写的路径检查的，经符号链接，真实位置可以在别的卷上，或落进 `paths.root` 里（3b `:15`、3e `link` 把这个选择留给本文）。
3. 卷：对每个仓库，`apfs::same_volume(<paths.root 或它最近的已存在上级>, path)` 必须为真，否则 `CrossVolume`（`detail = {root, repo, path}`）。`statfs` 的 `f_fstypename` 不是 `apfs` → `Unsupported`（`detail.fs_type`）。两条都在任何克隆之前判定，从不退化成复制（design §7）。
4. 对每个仓库（有界并行）：`gitio::fetch("origin", default_branch)` 得到 `o`；按下面的规则选 head；`gitio::tree_of(head)` 得到 tree。gitio 的错误原样透传（`NoSuchBranch`、`NoSuchCommit`、`Unsupported`……；3b `:14`）。fetch 在主 checkout 里写什么由 gitio.md 定（module-rulings T5：gitio 只写 `refs/strata/*`），不算本会话的写入；它失败时 session 没有现场。
   - `pin_unpushed = true` 且 `publish.branch == default_branch` 时，读主 checkout 的 `refs/strata/publish/<default_branch>`：它存在、不等于 `o`、且 `o` 是它的祖先（`merge-base --is-ancestor`），即链领先 origin，就钉链顶；与 origin 相同、落后或分叉，都钉 `o`。这与 publish 选合并基线的规则相同（design §6.7）。
   - `publish.branch != default_branch`，或者 `pin_unpushed = false`：一律钉 `o`（config.md §2.3 的 `publish` 与 `pin_unpushed` 两条，T2 裁定）。
5. 种子：按 §5.2 判定每个仓库的 `seed_files`。这时工作区还不存在，被跟踪看钉住的基线 tree（`ls-tree`），被 ignore 看主 checkout 的规则。可能的拒绝：`InvalidInput`、`IsDirectory`、`NotFile`。tracked 仓库的 `seed_caches` 里已存在的条目必须是目录、且不是符号链接，否则 `NotDirectory`（`detail = {repo, field: "seed_caches", index, path, reason}`）。
6. 用 `arc4random_buf` 取 40 bit，生成 `s-` 加 8 位 base32 的会话 id。Store 里已有这个 id（任何状态，含已删除），或者 `<sessions>/<id>` 已存在 → `Exists`（`detail = {session, path, reason: "store_row" | "directory"}`）。不换个 id 重试 [我的决定]：40 bit 随机数碰撞的概率可以忽略，真撞上了说明有 bug 或有人手工建了目录，报出来比悄悄绕开好；调用方再调一次 `new` 就会得到新 id。

**阶段 B：填缓存。** 写的都是幂等、按内容取键的共享缓存，不算本会话的对外写入；失败原样透传（§6.1）。

7. 【锁 repo】影子仓库不存在就建，存在就按 §4.2 刷新镜像。
8. 【锁 repo】`bases/<repo>/<tree>/` 不存在 → `gitio::export_tree(tree, 该目录)`。导出中断时目标不会出现，临时目录由 gitio 保留并报出（module-rulings T5）。
9. 【锁 repo】依赖集：向 deps 要一个与该基线安装输入匹配的依赖集。已封存的直接复用；没有就用 `deps::adopt` 认领主 checkout 现成的依赖目录；adopt 返回 `Drift` 时，把基线克隆到 staging 目录，由 `deps::build` 冷装（design §6.1 第 1 步）。查找与 set id 的计算都在 deps 里。`deps_dirs` 为空的仓库（附录 A 里多数 `@inter/*` 库与纯 Rust 仓库）得到的是空的依赖集：`dirs` 为空，id 照常由 deps 给出。结果得到 `BaseGen { repo, tree, deps }`。

**阶段 C：写意图。** 第 10 步写成之后，任何失败都是 `Unknown`。

10. 【写】在 Store 插入会话行：`state = Creating`，记下 id、ticket、root、创建时间与计划。这一行本身写失败时什么都没留下（SQLite 事务是原子的），store 的码原样透传。

**阶段 D：建工作区。** 每个阶段开始时先在 Store 里记下 phase，崩溃后 `recover` 报出的就是正在做的那一步。

11. 【写】`root`：`mkdir <sessions>/<id>`，带 `O_EXCL` 语义；目录已存在 → `Unknown`。然后对每个仓库依次：
    1. `workspace`：`apfs::clone_tree(bases/<repo>/<tree>, <root>/<repo>)`。
    2. `deps`：对依赖集 `dirs` 的每一项，`apfs::clone_tree(<依赖集目录>/<dir>, <root>/<repo>/<dir>)`。
    3. `git`【锁 repo】：按 §4.2 写管理目录 `worktrees/<id>.1/` 与工作区的 gitfile；在工作区里跑 `git read-tree <tree>` 和 `git update-index -q --refresh`。
    4. `pin`【锁 repo】：`gitio::update_ref("refs/strata/pins/<id>", head, None)`，期望这个 ref 原本不存在。
    5. `seed_caches`：只对 tracked 仓库（§4.1）。每个已存在的条目 `apfs::clone_tree(<path>/<dir>, <root>/<repo>/<dir>)`，缺的上级目录先建；`node_modules/.cache` 因此落进刚克隆的依赖目录里。
    6. `seed_files`：调 `seed_files(repo_cfg, path, <root>/<repo>)`（§5.2），得到 `SeedRecord`。

**阶段 E：定稿。**

12. 【写】在 Store 的一个事务里写入工作区行（base、head、path、tracked、代数 1）与种子行，并把 `state` 设为 `Active`。返回 `SessionManifest`。随后由 daemon 为 tracked 仓库开始监听（design §6.1 第 3 步）。

复杂度：

- 阶段 A：O(仓库数) 次 stat，加一轮并行的网络 fetch。
- 阶段 B：缓存温热时是 O(仓库数) 次查找。
- 阶段 D：每个仓库几次目录 clonefile（在内核里，按条目数线性），加一次 index 刷新（O(源码文件数) 次 stat）。
- 阶段 E：O(仓库数 + 种子数) 行。

验收数字：design §8 的"新建会话 < 5 s（tool + 全部兄弟库）"，指基线与依赖集都已在缓存里的情况（`new_warm_under_5s`）。第一次遇到某个依赖集时的认领（秒级）或冷装（分钟级，design §6.1）不在此列。风险：每个兄弟库都要克隆依赖目录，十几个 `node_modules` 的目录 clonefile 可能超过 5 s，要等首个 spike 实测后修正（design §8）。

### 5.2 种子文件：`seed_files(repo_cfg, from, to)`

判定规则（module-rulings T9 from T2；config.md §2.3、§4.3）：

1. **两类条目**：条目表由 `RepoConfig::seed_files()` 给出，它返回 `(条目, 是否显式)`（config.md §3）。配置里没写 `seed_files` 时，条目是 design §4 的默认四项（`CLAUDE.md`、`.env`、`.env.local`、`.claude/settings.local.json`），每一项都是**候选**。写了时，表里的每一项都是**显式**的硬要求，即使它的名字与默认四项之一相同。
2. **类型**：在 `from`（主 checkout）里 `lstat`。不存在 → 静默跳过；目录 → `IsDirectory`；FIFO、socket、设备文件 → `NotFile`（`detail.file_type`）；普通文件或符号链接 → 继续。判定只用 `lstat`，从不打开文件，所以 FIFO 不会让 `new` 阻塞（3e `fifo`）。
3. **被跟踪与被 ignore**：被跟踪，指路径在钉住的基线 tree 里；被 ignore，由 `gitio::is_ignored` 判定，含 git 的全局排除文件 `core.excludesFile`（module-rulings T5）。候选被跟踪或未被 ignore → 静默不算，不报错、不告警。显式条目被跟踪 → `InvalidInput`（`detail = {repo, field: "seed_files", index, rule: "seed_files", reason: "tracked"}`）；未被 ignore → `InvalidInput`（`reason: "not_ignored"`）。
4. **复制**：`apfs::clone_file(from/<p>, to/<p>)`，缺的上级目录先建。符号链接按链接本身克隆，目标字节不变，从不跟随 [我的决定]。复制出来的文件与主 checkout 的互相独立（CoW）。每复制一个记一条 `SeedRecord`：`source = from/<p>`；是链接时 `link` 记目标；`size` 与 `source_mtime` 取自复制前的 `lstat`；`copied` 是复制完成的时刻。
5. **时机**：先判定全部条目，再开始复制。判定阶段的失败是具名拒绝，什么都没写；复制开始之后的任何失败是 `Unknown`。

`new` 里分两步用 [我的决定]：阶段 A 第 5 步只判定，那时被 ignore 看主 checkout 的规则；阶段 D 的 `seed_files` 调用再按工作区自己的 index（等于基线 tree）和工作区的 ignore 规则判定一遍。两次判定只在主 checkout 的 `.gitignore` 与基线不同时才会不一致：这时候选静默不算；显式条目的失败已在写入之后，按 §6.1 是 `Unknown`。

种子文件不进 checkpoint，靠的是它们被 ignore，而 overlay 跳过被 ignore 的文件。会话如果改了 `.gitignore`，让某个种子文件不再被 ignore，它就会以 `Untracked` 进 overlay；`status` 每次都对种子文件重查 ignore，这时给出 `SeedNotIgnored` 告警 [我的决定]。

### 5.3 `resolve`、`select`、`list`

`resolve(cwd)`：

1. 以 `O_DIRECTORY` 打开 cwd，用 `fcntl(F_GETPATH)` 取规范路径；这样符号链接已解析，大小写按卷上的写法。打不开 → `Ok(None)`。
2. 规范路径不以规范化的 `<paths.root>/sessions/` 开头 → None。
3. 下一段不能经 `SessionId::try_from` 解析 → None。`.repos`、`.trash` 在这一步落空。
4. 再下一段不是该会话清单里的某个 `RepoName` → None。目录名就是 `RepoName`，逐字节比较。会话根本身、dev-link 建的 `node_modules/`、`.strata-old-*`、`.strata-tmp-*` 在这一步落空。
5. 查 Store：会话存在、没有删除、有这个仓库的工作区行 → `Some((id, repo))`，否则 None。Store 读不出来 → `Err`，原样透传。

`resolve` 的复杂度是 O(1)，daemon 不在时也能用。返回类型从计划里的 `Option` 改为 `Result<Option<…>>`，是为了不把 Store 的错误吞成"不在会话里" [我的决定]。

`select(selector)`：

1. 能经 `SessionId::try_from` 解析 → 按 id 找；不存在或已删除 → `NoSuchSession`（`detail.selector`）。
2. 否则按 §5.1 第 1 步的 ticket 规则校验，不合格 → `InvalidInput`。合格时，找 ticket 逐字节相等且没有删除的会话：0 个 → `NoSuchSession`；1 个 → 它；多个 → `AmbiguousMatch`（`detail = {ticket, candidates}`，按创建时间排序）。

ticket 不会和 id 混淆，因为 `new` 以 `ReservedName` 拒绝会话 id 形式的 ticket。

`list()`：没有删除的会话，按创建时间排序。

### 5.4 `status(id)`

1. 从 Store 读会话行、工作区行、种子行和现场。读的时候不加锁，WAL 的读者不排在写锁后面（module-rulings T13）。不存在或已删除 → `NoSuchSession` [我的决定：已删除会话的 id 对所有调用都过期，3d `:219`]。
2. 每个工作区：`lstat <root>/<repo>` 与 `<root>/<repo>/.git`，任一缺失 → `Missing`。这一项直接看文件系统，不按记录推断（3d `:317`–`:319`）。工作区行有未完成的移动意图或已记成未决 → `Unknown`。否则 `Ok`。
3. tracked 工作区：从 Store 取最新的 checkpoint，填 `checkpoint` 与 `changed`。`tracked = false` 的工作区：给 `NotCheckpointed` 告警（module-rulings T9）。
4. 有依赖目录的工作区：`deps::inputs_hash(repo_cfg, 工作区)` 与所钉 `DepsSet.inputs_hash` 不同时，`drift` 取前者。按 types.md 的 `Drift` 行，这是数据，不是错误。
5. `pending`：deps 记录的、这个 repo 下 `from` 等于本工作区依赖集的待确认提议。发布后 rebase 因为需要别的依赖集而没有移动时记的那条提议也在其中（design §6.4 最后一段，不变量 4 优先）。
6. `conflicts`：最近一次 `move_workspace` 记下的冲突路径里，仍在 overlay 中的那些。
7. `seeds`：每条 `SeedRecord`。主 checkout 里源文件的 mtime 与 `source_mtime` 不同、或源文件已不在 → `SeedStale`。工作区的规则不再 ignore 它（每个仓库一次批量的 `check-ignore`）→ `SeedNotIgnored`。
8. 保留的旧树 → `OldTreeKept`，`pids` 现场用 `deps::open_files` 查，`changed` 取自记录。
9. 现场：会话行和工作区行上记下的 `Scene`。

复杂度是 O(仓库数 × (几次 stat + 一次安装输入哈希 + 一次批量 ignore 查询))，不遍历工作区。overlay 的特殊 mode 位告警（module-rulings T7）和 publish 的未决推送不在 `SessionStatus` 里，由 daemon 组装 status 回复时并入（daemon.md）。

### 5.5 `remove(id, discard, confirm)`

**阶段 A：检查。** 都是具名拒绝，什么都不写。

1. 会话存在且没有删除，否则 `NoSuchSession`。
2. `discard = true` 时，`confirm` 必须是 `Some`，且 `token.action == Action::RemoveDiscard { session: id }`，否则 `NotConfirmed`（`detail = {expected, reason: "missing" | "mismatch"}`）。凭据是否用过、是否过期，由 daemon 在交给 session 之前经 confirm 校验（types.md §4）。`discard = false` 时不看 `confirm`，凭据随函数返回被丢弃。
3. 对会话根（含保留的旧树）调 `deps::open_files`。有进程打开其中的文件、cwd 在其中、或 mmap 了其中的文件 → `OpenFiles`（`detail.open` 列出 pid 与路径）。session 不发信号、不等待。`discard` 不能绕过这一条 [我的决定]，`remove` 也没有 force 参数（3d `fp32` 三行）。
4. `discard = false` 时，以下任一成立 → `Dirty`，`detail` 分别列出：
   1. 某个 tracked 工作区的 overlay 非空：用 `overlay::reconcile_full`（冷扫，以它为真相）对比所钉基线，只读，不写 checkpoint（`detail.repos`，每个仓库最多列 100 个路径，超出时 `truncated = true`）。
   2. 会话含 `tracked = false` 的仓库：那里的编辑没有 checkpoint，strata 无从知道删了会不会丢东西（module-rulings T9；`detail.untracked_repos`）。
   3. 会话曾经 Active，而现在处于 `Unknown` 或 `Removing`，或者某个工作区是 `Unknown`、`Missing`。越过未决状态，只能 `--discard` 加人类确认（3d `:144`；`detail.unresolved`）。

   从未成为 Active 的会话（`new` 留下的 `Creating` 或 `Unknown` 现场）不受第 2、3 项约束，因为它的路径从未交给调用方 [我的决定]。

**阶段 B：记 checkpoint。** checkpoint 是 append-only 的历史，不算对外写入。

5. 对每个存在的 tracked 工作区：`overlay::checkpoint(工作区, 所钉基线, CheckpointReason::BeforeRemove)`，记下 `CheckpointId`。保留期由 store 的 gc 管（design §6.6）。失败 → 原样透传，什么都没删。

**阶段 C：删。** 第 6 步写成之后，任何失败都是 `Unknown`。

6. 【写】Store：`state = Removing`。这一行本身写失败时什么都没留下，store 的码原样透传。
7. 【rename】把会话根改名为 `<sessions>/.trash/<id>-<unix 秒>/`。这是同卷 `rename`，会话目录在这一刻整体消失 [我的决定]。
8. 【写】【锁 repo】对每个仓库：用比较并删除去掉 `refs/strata/pins/<id>`（期望值是记录的 head）；删掉影子仓库里本会话的管理目录（先删 `locked`）。
9. 【写】递归删除回收站里的目录：按 `apfs::open_rooted` 的规则逐级 `openat(O_NOFOLLOW)`，`unlinkat` 先删子后删父。从不跟随符号链接，遇到 `st_dev` 变化就停下（不跨卷）。会话根里指向外面的链接（例如 dev-link 建的 `node_modules/tailwindcss`）只删链接本身。
10. 【写】Store：`state = Removed`，记删除时间。返回 `RemoveReport`。

复杂度：阶段 A 的冷扫是 O(各 tracked 工作区的源码文件数)（design §8：tool 全量对账 < 3 s）。删除是 O(会话根里的条目数)；种子缓存里的 `target` 可能有几十万个条目，要删几十秒，但会话目录在第 7 步就已经消失。

### 5.6 `move_workspace(session, repo, new_base, head)`

调用方持有该 repo 的写锁，并保证同一会话没有进行中的 checkpoint：daemon 按会话排队，从不取消进行中的 checkpoint（module-rulings T7、T13）。

**阶段 A：检查。** 都是具名拒绝，什么都不写。

1. 会话存在且处于 `Active`，否则 `NoSuchSession`。
2. `repo_name(repo)`，以及 §4.4 的不匹配规则：`NoSuchRepo` 或 `InvalidInput`。
3. 工作区：`tracked = false` → `InvalidInput`（`detail.reason = "untracked_repo"`）。module-rulings T9 规定这类仓库从不移动或 rebase；没有现成的码表达"这个仓库不能移动"，按共同规则映射到最接近的 `InvalidInput` [我的决定]。工作区 `Missing` → `NoSuchPath`。工作区 `Unknown` → 以 `Unknown` 拒绝，`detail` 带现有的现场，什么都不做 [我的决定]。
4. 目标与当前相同（base 与 head 都相等）→ 返回 `from == to` 的 `MoveReport`，什么都不写。
5. `gitio::tree_of(head) != new_base.tree` → `InvalidInput`，因为 `WorkspaceState` 要求 `tree_of(head) == base.tree`（types.md §4）。Store 里没有 `new_base.deps` 这个依赖集 → `InvalidInput`。
6. 旧 head 与新 head 没有共同祖先（`git merge-base` 无输出）→ `NoMergeBase`（types.md §6.2）。
7. 预检：对旧工作区的每个依赖目录调 `deps::open_files`，有打开的文件 → `OpenFiles`（design §6.4 第 5 步：在 dev server 脚下不换）。

**阶段 B：填缓存。** 与 `new` 的阶段 B 相同，失败原样透传。

8. `bases/<repo>/<new tree>/` 不存在 → `gitio::export_tree`。

**阶段 C：checkpoint。** 这是不变量 1 要求的那次。

9. `overlay::checkpoint(工作区, 旧基线, CheckpointReason::BeforeRollout)`，得到 `CheckpointId`。它的 entries 就是要投影的 overlay。

**阶段 D：在旁边准备新树。** 准备用的目录是私有临时 `<root>/.strata-tmp-move-<repo>-<n>/`，n 是管理目录的下一代数。

10. `apfs::clone_tree(bases/<repo>/<new tree>, 临时)`；再把新依赖集的每个目录克隆进去。
11. 投影：`overlay::rebase(第 9 步的 overlay, 旧基线, 新基线)` 得到新的 overlay 与冲突（design §6.5 第 7 步）。新 overlay 的每一条：Added、Modified、Untracked 把 blob 放到临时树里的 `path`（`store::get_blob`）；Renamed 放 `path`、删 `from`；Deleted 删掉临时树里的该路径。冲突路径放会话的版本，即"保留会话版本并标出"。新基线已经等于会话内容的路径不再出现在 overlay 里，记入 `dropped`。
12. 携带：用 `apfs::walk` 遍历旧工作区，跳过 `.git` 和依赖目录；被 ignore 的目录整块处理，不往下走。既不在旧基线清单里、也不在 overlay 里的条目，即种子文件、种子缓存、其它被 ignore 的文件和空目录，按以下规则放进临时树 [我的决定]：临时树里没有 → 整块 `clone_tree` 或 `clone_file`；两边都是目录 → 递归；临时树里已有同名条目（新基线新增了一个被跟踪的同名文件）→ 保留会话的那份，记为冲突。
13. git：写新的管理目录 `worktrees/<id>.<n>/`，其中 `HEAD` 为新 head，有 `commondir`、`locked`，`gitdir` 指向最终的 `<root>/<repo>/.git`；临时树的 `.git` 指向它。在临时树里跑 `read-tree <new tree>` 和 `update-index -q --refresh`。
14. 复查打开的文件，同第 7 步；module-rulings T8 要求交换之前复查。有打开的文件 → 删掉临时树和新管理目录，返回 `OpenFiles`。这时工作区一字不动，Store 不变，第 9 步的 checkpoint 留作历史。

**阶段 E：交换。** 第 15 步写成之后，任何失败都是 `Unknown`。

15. 【写】在 Store 的工作区行上记移动意图：新旧 base、head、临时路径、n。这一行本身写失败时什么都没留下（事务是原子的）：删掉临时树和新管理目录，store 的码原样透传。
16. 【rename】`renamex_np(临时, 工作区, RENAME_SWAP)`：工作区路径原子地换成新树，旧树换到临时路径。然后 `fsync` 会话根。新树的 gitfile 指向新管理目录，所以 git 的 HEAD 与 index 随目录一起换了过来。
17. 【写】pin：`gitio::update_ref("refs/strata/pins/<id>", 新 head, Some(旧 head))`。
18. 【写】在 Store 的一个事务里写入 base、head、n、冲突列表，并清掉移动意图。`synchronous=FULL` 落盘之后，这次交换才算持久。

**阶段 F：处理旧树。** 交换已经持久，这一阶段的失败不改变移动的结果。

19. 旧树现在在临时路径。用 `overlay::reconcile_full`（对旧基线，经旧管理目录）检查它与第 9 步的 checkpoint 一致，并且 `deps::open_files` 对整棵旧树为空 → 删除旧树和旧管理目录。否则把旧树改名为 `<root>/.strata-old-<repo>-<n>/` 保留，记下 `changed`（是否与 checkpoint 不一致），`status` 给 `OldTreeKept` 告警。删到一半出错，同样改为保留并记录。
20. 返回 `MoveReport`。

复杂度：

- 第 9 步：O(改动文件数)。
- 第 10 步：几次目录 clonefile。
- 第 11 步：O(overlay 条目数)，冲突文件各做一次三方合并。
- 第 12 步：O(旧工作区里不在被 ignore 目录下的文件数)。
- 第 13 步：O(源码文件数) 次 stat。
- 第 16 步：O(1)。
- 第 19 步：O(旧树文件数)。

验收数字：design §8 的"依赖 rollout = 克隆 + rename，秒级"（`move_workspace_seconds_warm`）。

### 5.7 `recover()`

daemon 启动时调用一次，这时没有进行中的操作：

1. 会话行处于 `Creating` → 改为 `Unknown`，记 `Scene { op: New, phase: 最后记下的 phase, error: Unknown 且 detail.reason = "interrupted" }`。处于 `Removing` → 改为 `Unknown`，`op: Remove`。
2. 工作区行有移动意图 → 该工作区改为 `Unknown`，`Scene { op: Move }` 列出临时路径和代数。
3. 不碰文件系统：不删除，不重放，不替上次完成交换（不变量 6）。返回被改成 `Unknown` 的会话 id。

## 6. 失败与恢复

### 6.1 错误时机

这里落实共同裁定"写入一旦开始，任何失败都是 `Unknown`"，对 session 精确规定"对外可见的写入"：

- **算对外可见的写入**：工作区目录和会话根的内容、Store 里的会话行与工作区行、pin ref、影子仓库里的管理目录。第一次这样的写入成功之前，失败都是具名拒绝或原样透传，不留痕迹；这一次写入本身是 Store 里的一行，失败时事务原子地什么都不留。这次写入成功之后，任何失败都是 `Unknown`：现场保留并记下，`status` 报出。
- **不算对外可见的写入**：
  - 填共享缓存：`bases/` 的导出、依赖集的认领与冷装、影子仓库的建立和镜像刷新。它们幂等、按内容取键、不改变任何已有状态；中途失败留下的东西由 gitio、deps 自己保留并报出，session 原样透传它们的码。
  - `BeforeRollout`、`BeforeRemove` checkpoint：append-only 的历史。
  - 操作进行中的私有临时目录：在具名拒绝之前由操作自己删掉；进程崩溃时它会留下来，由 `recover` 认领为现场。

### 6.2 失败表

`new`：

| 失败 | 码 | 现场 | 恢复 |
| --- | --- | --- | --- |
| ticket、`repos`、配置路径、卷、种子判定、id 冲突 | 具名码（§3） | 无 | 改正后重试；重试会得到新 id |
| fetch、链顶判定、`tree_of` | gitio 的码，原样透传 | session 无 | 重试；fetch 本身留下什么由 gitio.md 定 |
| 填缓存：导出、认领、冷装、影子仓库 | gitio、deps、apfs 的码，原样透传 | session 无；导出的临时目录、staging 克隆由 gitio、deps 保留并报出 | 修好后重试 |
| 写意图那一行 | store 的码 | 无（事务是原子的） | 重试 |
| 写意图之后的任何失败：`ENOSPC`、`EIO`、克隆中途、git 失败、显式种子条目在阶段 D 才不过、进程崩溃 | `Unknown` | 会话行 `Unknown` 加 phase；会话根以及已建的工作区、管理目录、pin 都原样保留 | 看 `status`；用 `remove` 清掉现场，不需要 discard；重新 `new` |

`remove`：

| 失败 | 码 | 现场 | 恢复 |
| --- | --- | --- | --- |
| 会话不存在或已删除 | `NoSuchSession` | 无 | — |
| `discard` 没有凭据，或凭据与本会话不符 | `NotConfirmed` | 无 | 经 cli 加 TTY 与 LocalAuthentication 重来 |
| 有进程打开会话里的文件、cwd 在其中、或 mmap 了其中的文件 | `OpenFiles` | 无；目录一字不动，不发信号 | 停掉 `detail.open` 里的进程后重试 |
| 有未发布改动、含 `tracked = false` 仓库、或有未决现场 | `Dirty` | 无 | 先发布或恢复；或用 `--discard` 加确认 |
| `BeforeRemove` checkpoint 失败 | overlay 或 store 的码，原样透传 | 无 | 重试 |
| 改名之后的任何失败 | `Unknown` | 状态 `Unknown` 加 phase；回收站里剩下的部分、剩下的 pin 和管理目录都原样保留 | 看 `status`；之后的 `remove` 需要 `--discard` 加确认 |

`move_workspace`：

| 失败 | 码 | 现场 | 恢复 |
| --- | --- | --- | --- |
| 会话、仓库、不匹配、`tracked = false`、head 与 tree 不符、依赖集未知 | `NoSuchSession`、`NoSuchRepo`、`InvalidInput` | 无 | 调用方改参数 |
| 工作区缺失 | `NoSuchPath` | 无 | 看 `status` |
| 工作区已是未决现场 | `Unknown`（拒绝，什么都不做） | 原有现场不变 | 人工处理后 `remove` |
| 新旧 head 没有共同祖先 | `NoMergeBase` | 无 | — |
| 依赖目录上有打开的文件（预检或交换前复查） | `OpenFiles` | 无；工作区一字不动；复查时删掉自己的临时树；checkpoint 留作历史 | 多会话的 `deps approve` 里按条目返回，其余会话照常移动（types.md §6.2）；停掉 dev server 后重试 |
| 导出、checkpoint、准备新树时失败（交换之前） | 透传，或者在第 15 步之后才发生时为 `Unknown` | 交换之前：删掉临时树后透传 | 重试 |
| 交换开始之后（第 15–18 步） | `Unknown` | 工作区 `Unknown`；新旧两棵树、两个管理目录、移动意图都保留 | 看 `status`；用 `BeforeRollout` checkpoint 恢复，或 `remove --discard` |
| 旧树仍被使用或被改过 | 不是错误；`MoveReport.old_tree` 带路径，`status` 给 `OldTreeKept` | 旧树保留 | 停掉进程后由人删除，或随 `remove` 一起删 |

`status`、`resolve`、`select`、`list` 只读，失败时原样透传 Store 的码，不留任何东西。

### 6.3 `Unknown` 的 `detail` 与 `message`

`new` 留下现场时返回的 `Unknown`（3b `:13` 留给本文定）：

```json
{
  "op": "new",
  "session": "s-k3q7xm2a",
  "phase": "workspace",
  "repo": "tool",
  "path": "/Volumes/GWD/inter/.strata/sessions/s-k3q7xm2a/tool",
  "cause": { "errno": 28, "message": "No space left on device (os error 28)" },
  "kept": ["/Volumes/GWD/inter/.strata/sessions/s-k3q7xm2a"]
}
```

对应的 `message` 是：`new s-k3q7xm2a: workspace failed after writing began; kept /Volumes/GWD/inter/.strata/sessions/s-k3q7xm2a; nothing was rolled back, cleaned up or retried; see git-strata status s-k3q7xm2a`。

- `phase` 的取值固定：`new` 有 `root`、`workspace`、`deps`、`git`、`pin`、`seed_caches`、`seed_files`、`finalize`；`move_workspace` 有 `swap`、`pin`、`record`；`remove` 有 `trash`、`unpin`、`delete`、`record`。
- `cause` 在下层给出的是 `StrataError` 时就是那个错误（`code`、`message`、`detail`），是 I/O 错误时是 `errno` 加原文。
- `move_workspace` 的 `detail` 另带 `from`、`to`、`temp`、`n`。`remove` 的 `detail` 另带回收站路径。
- 同一个 `StrataError` 存进 `Scene.error`，所以 `status`，以及 daemon 重启后的 `status`，报出的是与当初完全相同的错误。

### 6.4 重启

daemon 重启后先调 `recover`（§5.7），再接受请求。被中断的 `new`、`remove`、`move_workspace` 都在 `status` 里以 `Scene` 报出，phase 是中断前最后记下的那个；文件系统保持中断时的样子，不重放，不清理。

### 6.5 已知限制

- `pre-push` 只拦住习惯性的 `git push`。agent 与用户是同一个 uid，可以设 `STRATA_PUBLISH=1`，或直接绕过钩子，所以它不是安全边界；真正的人类确认在 publish 的推送路径上（design §6.9）。工作区没有 remote，`git push` 不带 URL 本来就会失败。
- 交换前只对依赖目录检查打开的文件（design §6.4 第 5 步）。cwd 在工作区里的进程，例如在工作区里启动的 Claude Code 或 shell，交换后会留在旧树上：继承这个 cwd 的命令会在旧树里读写。旧树因此被保留，并在 `status` 里告警；从那以后写进旧树的东西不进 checkpoint。按路径重新进入工作区的进程不受影响。
- 阶段 A 判定种子文件时，被 ignore 看的是主 checkout 的规则（§5.2）；`status` 之后按工作区的规则重查。
- 交换之后，会话的暂存状态和本地 commit 不随工作区走：新管理目录的 index 等于新基线。overlay 里的 `Added` 条目因此变成 `Untracked`，两者发布与检索的待遇相同（types.md `EntryKind`）。本地 commit 的对象还留在影子仓库里，直到被 gc。

## 7. 测试清单

测试用真实的 git 仓库（file:// 的裸仓库当 origin）和真实的 Store（WAL + `synchronous=FULL`），都放在测试机 APFS 卷上的临时目录里；跨卷与非 APFS 的情形用 `hdiutil` 挂上的磁盘映像。持有文件的进程由同一个测试二进制以 `SESSION_TEST_HOLD=fd|cwd|mmap:<path>` 重新执行自己。测试从不碰 `/Volumes/GWD/inter`。

| 测试名 | 断言 | 来源（矩阵行 或 新增） |
| --- | --- | --- |
| `new_clones_entire_sibling_layout` | 配置三个仓库：`tool` 的脚本按 `../quick-start/src` 读兄弟库，另有 `release-notes`。`new(Some("9246"), [tool])` 返回的清单含全部三个仓库，只有 `tool` 的 `tracked = true`。`<paths.root>/sessions/<sid>/` 下恰好是三个仓库目录。在 `tool` 工作区里按 `../quick-start/src/index.ts` 读到的字节等于 quick-start 钉住基线里的同一文件。`root = <paths.root>/sessions/<sid>`，`path = root/<repo>`，路径里没有 ticket | 新增（design §3 兄弟布局、§10 新增项） |
| `new_workspace_equals_base_tree` | 100 与 1000 个文件两档（参数化）。每个工作区去掉 `.git`、依赖目录、种子缓存和种子文件之后，路径集合与 `git ls-tree -r <head>` 完全相同，逐文件字节、可执行位、符号链接目标都相同。会话根旁和 `bases/` 里没有遗留的临时目录。用真实 Store，不用内存假实现 | `layerfs-project/tests/init_memory.rs::init_100_and_1000_over_memory_ports_pass_the_full_namespace_oracle`；`layerfs-project/tests/init_sqlite.rs::selected_profiles_init_100_and_1000_pass_the_full_namespace_oracle`；`layerfs-workspace/tests/captured_namespace.rs::the_model_of_the_fixture_is_the_base_root` |
| `new_workspace_keeps_seed_bytes_and_link_targets` | 基线 tree 里有 5 个符号链接：指向依赖目录、断链、自指、指向仓库外、目标是非 UTF-8 字节；还有二进制文件。主 checkout 里有被 ignore 的种子文件（含二进制字节），以及一个不在种子清单里的被 ignore 文件。断言：工作区里的链接目标逐字节等于 tree 里的，从不被跟随；种子文件字节相同；不在清单里的被 ignore 文件不被带入；之后改写、删除主 checkout 的种子文件，工作区里的副本不变；`.git` 是 gitfile，对象没有被复制。用真实 Store | `layerfs-project/tests/complete_root.rs::host_import_keeps_ignored_git_dependencies_outputs_and_exact_link_targets`；`layerfs-project/tests/complete_root.rs::real_host_profiles_acquire_and_reuse_the_exact_complete_root` |
| `new_typed_refusals_leave_no_trace` | 参数化：未配置的 repo → `NoSuchRepo`；主 checkout 的 `path` 不存在 → `NoSuchPath`；`seed_caches` 条目是普通文件 → `NotDirectory`；种子文件是目录 → `IsDirectory`；种子文件是 FIFO → `NotFile`；`path` 是符号链接 → `NotDirectory`；`origin/<default_branch>` 不存在 → `NoSuchBranch`。每种都返回该码，`detail` 点名是哪一项检查、哪个条目。之后 `<paths.root>/sessions/` 下没有新目录，Store 没有新会话行，影子仓库和 `refs/strata/pins/*` 不变，主 checkout 每个条目的 mtime/ctime 都不变 | `layerfs-project/tests/acquisition_custody.rs::a_definite_failure_removes_its_rows_and_its_record`；`layerfs-daemon/tests/mounted_serial_admission.rs::busy_initial_reservation_has_no_kernel_mount_effect` |
| `new_passes_lower_codes_through_unchanged` | gitio 返回的 `NoSuchBranch`、`Unsupported`（tree 里有 gitlink；仓库是 SHA-256 格式）经 `new` 原样到达调用方，`code`、`message`、`detail` 与 gitio 给出的逐字相同；什么都没写 | `layerfs-project/tests/acquisition_custody.rs::a_backing_read_failure_is_definite_and_is_cleaned_up` |
| `new_validates_ticket` | `../foreign`、空串、65 字节、含空格、含 `:` → `InvalidInput`（`detail.field = "ticket"`）。`s-k3q7xm2a`、`S-K3Q7XM2A` → `ReservedName`。`9246`、`gwd-9246`、`a.b_c-d` 通过 ticket 校验（随后的检查照常进行）。ticket 不出现在任何路径里。`RepoName` 的格式由 types 自己的测试负责 | `sdk/js/test/packaging.mjs::lazy`；`layerfs-sandbox/tests/docker_naming.rs::container_names_follow_the_engine_rule` |
| `new_refuses_cross_volume_root` | `paths.root` 在 `hdiutil` 挂上的另一个 APFS 卷上；`paths.root` 还不存在时，取它最近的已存在上级所在的卷。断言：`CrossVolume`，`detail` 含 root 与 repo 路径；在任何克隆之前返回，两个卷上都没有新目录，Store 没有新行，从不退化成复制。这是计划 Review Focus 1 的 session 一半，apfs 一半是 `clone_dir_refuses_cross_volume` | `layerfs-project/tests/init_sqlite.rs::an_unsupported_platform_refuses_the_store_before_any_file_exists` |
| `new_refuses_non_apfs_root` | `paths.root` 在 HFS+ 磁盘映像上：`Unsupported`（`detail.fs_type = "hfs"`），什么都没写 | `layerfs-project/tests/init_sqlite.rs::an_unsupported_platform_refuses_the_store_before_any_file_exists` |
| `new_refuses_existing_session_dir_before_any_effect` | 用 `set_next_id` 固定会话 id。两种情形：预先建好 `<paths.root>/sessions/<sid>/` → `Exists`，`detail.reason = "directory"`；Store 里已有该 id 的行（含已删除的会话）→ `Exists`，`detail.reason = "store_row"`。预先存在的目录内容一字不动，没有导出、没有 pin、影子仓库没有改动 | `sdk/js/test/reinit.mjs::entry`；`layerfs-api/tests/sandbox_operations.rs::create_mismatched_existing_project_refuses_before_any_deployment_or_control_effect`；`layerfs-daemon/tests/kernel_mount.rs::attach_refusals_have_no_effect_and_a_failed_attach_returns_to_unattached` |
| `new_refuses_explicit_seed_tracked_or_unignored` | 显式条目 `src-tauri/src/secrets.rs`：在基线 tree 里被跟踪 → `InvalidInput`，`detail = {repo, field: "seed_files", index, rule: "seed_files", reason: "tracked"}`；未被 ignore → 同样的码，`reason: "not_ignored"`。显式写出的 `CLAUDE.md` 在跟踪它的仓库里同样被拒（显式条目不按候选处理）。三种都在写入之前返回 | 新增（config.md §2.3、§4.3，module-rulings T9） |
| `new_refuses_symlinked_repo_path` | 主 checkout 的 `path` 是指向真实仓库目录的符号链接：`NotDirectory`，`detail = {repo, field: "path", path, reason: "symlink"}`，没有残留；把 `path` 换成真实目录后，同一次 `new` 成功 | `layerfs-project/tests/acquisition_custody.rs::a_symlinked_source_is_refused_by_name_before_anything_begins`；`sdk/js/test/init_refusals.mjs::link` |
| `new_refuses_fifo_seed_before_writing` | 主 checkout 里的显式种子文件是 FIFO（`mkfifo`）：`NotFile`，在任何写入之前返回，FIFO 从未被打开（调用不阻塞），没有残留；换成普通文件后重试成功，得到一个新的会话 id，不返回 `Exists` | `sdk/js/test/init_refusals.mjs::fifo` |
| `new_pins_chain_top_when_ahead` | `pin_unpushed = true` 且 `publish.branch == default_branch`：`refs/strata/publish/main` 领先 `origin/main` → `head` 是链顶；链与 origin 相同、落后或分叉 → `head` 是 `origin/main` 的 tip；`pin_unpushed = false` → 一律 origin | 新增（design §6.7） |
| `new_pins_default_branch_when_publish_branch_differs` | `publish.branch = "release"`、`default_branch = "main"`，`refs/strata/publish/release` 领先：新会话仍钉 `origin/main`，`pin_unpushed` 不起作用 | 新增（config.md §2.3 的 `publish` 条，T2 裁定） |
| `new_seeds_default_names_only_when_untracked_and_ignored` | 配置里没写 `seed_files`，缺省四项都是候选：`CLAUDE.md` 被跟踪 → 不复制，不报错也不告警，工作区里是基线版本；`.env` 未被 ignore → 不复制；`.env.local` 被 ignore 且未被跟踪 → 复制；`.claude/settings.local.json` 只靠 `core.excludesFile` 被 ignore → 复制；主 checkout 里不存在的条目静默跳过 | 新增（module-rulings T9 from T2，config.md §2.3） |
| `new_clones_seed_caches_as_real_directories` | tracked 仓库的 `src-tauri/target`、`.next/cache`、`node_modules/.cache` 从主 checkout 克隆为真实目录：`lstat` 不是符号链接；改写工作区里的文件，主 checkout 的同一文件不变。`node_modules/.cache` 落在依赖目录里。`tracked = false` 的仓库不克隆种子缓存。主 checkout 的 `target` 是符号链接时返回 `NotDirectory`，在写入之前 | 新增（design §3 symlink 陷阱、§4） |
| `workspace_git_is_detached_and_prepush_refuses` | 每个工作区：`git symbolic-ref -q HEAD` 失败（detached）；`git rev-parse HEAD` 等于清单的 `head`，且 `tree_of(head) == base.tree`；`git remote` 输出为空，没有 upstream；新建后 `git status --porcelain` 为空（种子文件与依赖目录被 ignore）。`git push <裸仓库 URL> HEAD:refs/heads/x` 被 `pre-push` 拒绝：非零退出，stderr 含 strata 的提示，远端没有 `x`。带 `STRATA_PUBLISH=1` 时钩子放行（design §6.8 的约定） | 新增（design §6.8） |
| `manifest_round_trips_through_store` | `new` 返回的清单（id、ticket、root，以及每个 repo 的 base、head、path、tracked）与重开 Store 之后的 `status(id).manifest` 完全相等，JSON 往返不变 | `layerfs-sandbox/tests/container_labels.rs::labels_are_sent_at_create_and_found_in_any_order_beside_the_image_s` |
| `new_warm_under_5s` | 合成一套规模接近 tool 加 21 个兄弟库的配置，基线与依赖集都已在缓存里：`new` 的墙钟时间小于 5 s。标 `#[ignore]`，在 spike 里跑，并据此修正目标 | 新增（design §8 验收数字） |
| `new_same_ticket_creates_another_session` | 同一 ticket 连续 `new` 两次：得到两个不同的会话 id 和两个独立的会话根；第二次不复用第一次的目录，也不重建已有的基线与依赖集（Store 里 generation 的行数不变）。`select("9246")` 返回 `AmbiguousMatch`，`detail.candidates` 恰为这两个 id | `sdk/js/test/reinit.mjs::entry`；`layerfs-api/tests/sandbox_operations.rs::create_matching_existing_project_keeps_one_original_hello_and_does_not_reinstall` |
| `new_lost_reply_is_found_not_replayed` | 调用方没有拿到 `new` 的返回值（测试丢弃它）：会话照常处于 Active，`list` 和按 ticket 的 `select` 都能找到它；没有任何重放的路径，再调一次 `new` 得到的是另一个会话 | `layerfs-daemon/tests/kernel_mount_routes.rs::lost_bind_and_attach_acknowledgements_are_located_without_replay` |
| `resolve_by_cwd_inside_workspace` | cwd 为 `<root>/tool/src/app`，包括大小写不同的拼写、经符号链接到达的同一目录 → `Some((sid, tool))`。cwd 为会话根本身、会话根下 dev-link 建的 `node_modules/`、`.strata-old-*`、主 checkout、不存在的路径 → `None`。已删除会话的工作区路径 → `None` | `layerfs-sandbox/tests/container_labels.rs::a_missing_or_different_label_or_another_mount_is_a_topology_mismatch` |
| `select_by_id_or_ticket` | 会话 id → 该会话；已删除 → `NoSuchSession`。ticket 只有一个会话 → 该会话；没有会话 → `NoSuchSession`；两个会话 → `AmbiguousMatch` 加 `candidates`。不合格的选择器 → `InvalidInput` | 新增（types.md §6.2 的 `AmbiguousMatch`、`NoSuchSession`） |
| `sessions_on_one_base_are_isolated` | 两个会话钉同一基线：在 A 里写、删、改名文件，B 的工作区、清单与 checkpoint 都不变；一次被拒绝的 `new`（`NoSuchRepo`）不改变 A 或 B 的任何状态 | `layerfs-daemon/tests/mounted_cycles.rs::simultaneous_mounts_over_related_roots_are_isolated_and_a_stale_token_never_redirects` |
| `seed_files_listed_in_status_and_never_checkpointed` | `new` 之后，`status` 的每个工作区都列出每个种子文件的 `path`、`source`、`size`、`source_mtime`、`copied`，没有任何内容字段。改写工作区里的 `.env` 后做 checkpoint：entries 里没有任何种子文件路径。之后改写主 checkout 的 `.env` → `SeedStale`；会话把 `.env` 从 `.gitignore` 里删掉 → `SeedNotIgnored` | 新增（design §4 种子文件） |
| `status_warns_untracked_repo_edits_not_checkpointed` | `tracked = false` 的每个工作区都带 `NotCheckpointed` 告警，`changed` 为空，`checkpoint` 为 None；tracked 的工作区没有这条告警 | 新增（module-rulings T9） |
| `status_reads_workspace_presence_from_filesystem` | 不经 Store 删掉一个工作区目录（或它的 `.git`）：`status` 报 `Missing`，Store 里的行没有被改写；把目录放回去后，`status` 回到 `Ok` | `layerfs-sandbox/tests/engine_lifecycle.rs::only_the_engines_no_such_container_answer_is_a_gone_container`；`layerfs-sandbox/tests/engine_lifecycle.rs::a_stop_the_engine_answers_with_no_such_container_is_a_gone_container`；`layerfs-sandbox/tests/engine_lifecycle.rs::only_the_exact_intact_container_not_running_is_a_stopped_container`；`layerfs-sandbox/tests/container_labels.rs::a_missing_or_different_label_or_another_mount_is_a_topology_mismatch` |
| `status_reports_drift_and_pending_proposals` | 在工作区里改 `package.json` 的依赖：`drift` 为工作区算出的安装输入哈希，与所钉依赖集的不同；只改 `version` 字段不产生 drift（依 deps 的 `inputs_hash` 规则）。deps 记下一条 `from` 等于该工作区依赖集的提议之后，`pending` 列出它 | 新增（design §6.3、§6.4；types.md `Drift` 行） |
| `new_failure_after_writing_began_is_unknown` | failpoint 在 `workspace` 阶段（克隆第二个仓库时）注入 `ENOSPC`：返回 `Unknown`。会话根和已克隆的仓库目录原样留下，不删除、不重试、不回滚。Store 里该会话为 `Unknown`，`phase = "workspace"`。`status` 报出这个现场（`Scene.op = New`）；`resolve` 对它的路径返回 None。用同一 ticket 再 `new`，得到新 id，旧现场不受影响 | 新增；理由出处 `layerfs-project/tests/acquisition_custody.rs::a_failed_cleanup_reports_its_cause_and_the_rows_still_held` |
| `new_unknown_detail_names_the_kept_scene` | 同上的失败：`detail` 恰好有 `op = "new"`、`session`、`phase`、`repo`、`path`、`cause`（`errno = 28` 与原文）、`kept`（会话根的绝对路径）；`message` 符合 §6.3 的模板；清单从未返回给调用方 | `layerfs-project/tests/acquisition_custody.rs::an_unknown_outcome_leaves_the_operation_untouched` |
| `unknown_scene_survives_daemon_restart` | 两种情形。（a）上一条的 `Unknown` 之后，丢弃 `Sessions` 与 `Store` 再重新打开；`recover()` 之后，`status` 报出同一个现场（同 id、phase、kept、error）。（b）failpoint 以 `None` 在 `seed_files` 阶段就地返回，不更新状态，模拟崩溃；重开后 `recover()` 把仍为 `Creating` 的行记为 `Unknown`，`status` 报出最后记下的 phase。两种情形下，文件系统在重开前后逐字节相同：不重放，不清理 | 新增（module-rulings T9）；理由出处 `layerfs-project/tests/acquisition_custody.rs::a_failed_cleanup_after_a_failure_keeps_both_causes` |
| `base_generation_never_modified_in_place` | 在工作区里改写、删除、chmod 文件，`bases/<repo>/<tree>/` 与依赖集目录的字节和 mode 都不变；`move_workspace` 移到新基线之后，旧的 `bases/<repo>/<tree>/` 仍然不变 | `layerfs-workspace/tests/operation_scope.rs::scoped_views_share_original_install_serials_and_keep_original_provider` |
| `move_workspace_validates_before_writing` | 参数化：RepoId 不在配置里 → `NoSuchRepo`；`new_base.repo` 与 RepoId 映射出的名字不同 → `InvalidInput`；`tracked = false` 的仓库 → `InvalidInput`；`tree_of(head) != new_base.tree` → `InvalidInput`；依赖集不在 Store 里 → `InvalidInput`；新旧 head 没有共同祖先 → `NoMergeBase`；工作区缺失 → `NoSuchPath`。每种都不写任何东西：没有 checkpoint，没有临时目录 | 新增（types.md §8、module-rulings T9） |
| `move_workspace_swaps_base_and_deps_atomically` | 移动之前先有一个 `BeforeRollout` checkpoint。移动之后，工作区的源码等于新基线加上投影后的 overlay，依赖目录是新依赖集的克隆；`WorkspaceState.base` 与 `head` 一起更新，HEAD detached 在新 head，pin 移到新 head。failpoint 以 `None` 分别在交换前、交换后、记录前各中止一次：重开并 `recover()` 之后，要么完全是旧状态（交换前），要么工作区为 `Unknown` 且新旧两棵树都保留（交换后），从不出现新源码配旧依赖 | 新增（design §6.4"依赖与源码一起移动"、不变量 8） |
| `move_workspace_rechecks_open_files_before_swap` | 准备完成之后、交换之前，测试的持有进程打开依赖目录里的一个文件：返回 `OpenFiles`。工作区目录一字不动（inode 与内容都不变）；准备用的临时目录和新管理目录已经删掉；Store 里的 base 与 head 不变；`BeforeRollout` checkpoint 留作历史 | 新增（module-rulings T8：交换前复查） |
| `move_workspace_projects_overlay_and_marks_conflicts` | 从旧基线移到新基线（模拟一次发布）：会话已发布的路径离开 overlay，记入 `dropped`；未发布、与新基线不重叠的改动经三方合并后写入；同一 hunk 两边都改的路径保留会话版本，列在 `conflicts` 里，`status` 也列出它；被 ignore 的本地文件与新基线新增的同名被跟踪文件相撞时，保留会话的文件并记为冲突 | 新增（design §6.5 第 7 步） |
| `move_workspace_carries_ignored_content` | 移动之后，种子文件、种子缓存（`target/` 里的内容）、其它被 ignore 的文件和空目录都原样出现在新工作区里，内容相同，不经过 overlay | 新增（design §4 种子文件、种子缓存） |
| `move_workspace_keeps_old_tree_while_in_use` | 移动时有进程的 cwd 在旧工作区里：移动成功，`MoveReport.old_tree` 为 `.strata-old-<repo>-<n>/`，旧树没有被删除，`status` 带 `OldTreeKept`（含 pid）。交换后有人往旧树里写文件 → 告警的 `changed = true`，旧树不会被自动删除 | 新增（design §2 反目标"丢失未发布改动"；module-rulings T9 的交换规则） |
| `move_workspace_seconds_warm` | 与 `new_warm_under_5s` 同样的规模，新基线与依赖集都已在缓存里：`move_workspace` 的墙钟时间在秒级（目标小于 5 s）。标 `#[ignore]` | 新增（design §8"依赖 rollout = 克隆 + rename，秒级"） |
| `workspace_git_never_writes_main_checkout` | 在 `new`、工作区里的 `git add`、`git commit`、`git stash`、`git branch`，以及 `remove` 之后：主 checkout 的工作树、index、`config`、`hooks/`、`info/`、`worktrees/` 逐字节不变，`refs/` 只多出 `refs/strata/pins/<sid>`（`new` 时出现，`remove` 后消失）。工作区里新建的 commit 对象在影子仓库里，不在主 checkout 的对象库里。工作区里 `git fetch origin` 失败、`origin/main` 解析不到，所以 front dev-link 那段"干净就 `reset --hard origin/main`"不会在兄弟工作区里发生 | 新增（design §9 不变量 2、§6.8；`front/dev-link.sh`） |
| `removed_session_id_never_resolves_or_reissued` | 删除会话 A 之后：`select(A)`、`status(A)` → `NoSuchSession`；在 A 的旧工作区路径新建一个同名目录，`resolve` 对它返回 None；`set_next_id` 再给出 A 的 id 时，`new` → `Exists`，id 永不复用，A 的目录也不会被新会话复用 | `sdk/js/test/verify/exec_mount_lookup_real.mjs`；`layerfs-daemon/tests/mounted_cycles.rs::simultaneous_mounts_over_related_roots_are_isolated_and_a_stale_token_never_redirects` |
| `remove_refuses_unpublished_without_discard` | 全部仓库都是 tracked，`tool` 有未发布改动（overlay 非空）：`remove(id, false, None)` → `Dirty`，`detail.repos.tool` 列出路径；会话目录、清单、checkpoint 一字不动。`remove(id, true, Some(RemoveDiscard{id} 的凭据))` 成功，`RemoveReport.checkpoints` 含 `BeforeRemove` checkpoint，其 entries 等于删除前的 overlay | `layerfs-overlay/tests/orphans.rs::logical_close_refuses_new_captured_owners_and_keeps_existing_consumers`；`layerfs-overlay/tests/piece_lifetimes.rs::a_closed_workspace_retains_its_pieces_until_file_disposal` |
| `remove_untracked_repo_requires_discard` | 会话含 `tracked = false` 的仓库，tracked 仓库没有改动：不带 discard → `Dirty`，`detail.untracked_repos` 列出这些仓库；带 discard 和匹配的凭据 → 成功 | 新增（module-rulings T9） |
| `remove_discard_checks_the_token` | `discard = true` 而 `confirm` 为 None，或者凭据的 action 是另一个会话的 `RemoveDiscard`、或是 `PushDeploy` → `NotConfirmed`，什么都不删；凭据按值消费 | 新增（types.md §4"谁消费"、design §6.9） |
| `remove_after_mtime_only_change_needs_no_discard` | 全部仓库都是 tracked，只 `touch` 过文件，内容与 mode 都没变：overlay 为空，`remove(id, false, None)` 成功 | `layerfs-daemon/tests/mounted_commit.rs::an_unmount_without_discard_succeeds_after_an_mtime_only_commit` |
| `remove_refuses_when_a_process_holds_fd_cwd_or_mmap` | 参数化三种持有：持有进程打开会话里的一个文件、cwd 在工作区里、mmap 了依赖目录里的一个文件，其中一个变体让持有进程阻塞在写满的管道上。每种都返回 `OpenFiles`，`detail.open` 含该 pid 与路径；目录一字不动；持有进程不被发信号，之后仍能读写；持有释放后 `remove` 成功 | `layerfs-api/tests/observed_control.rs::unmount_force_selects_one_original_request_without_unknown_relinquishment`；`layerfs-daemon/tests/f046_fork_reference.rs::a_descriptor_copied_by_another_threads_fork_keeps_the_plain_unmount_busy`；`layerfs-daemon/tests/forced_unmount.rs::fp33_a_held_working_directory_leaves_the_mount_aborted_and_retained`；`layerfs-daemon/tests/forced_unmount.rs::fp33_a_held_descriptor_leaves_the_mount_aborted_and_retained`；`layerfs-daemon/tests/mounted_cycles.rs::a_caller_blocked_on_a_full_pipe_neither_blocks_commit_nor_is_touched_by_unmount` |
| `remove_open_files_in_one_session_never_block_another` | 会话 A 被进程占用，B 没有：`remove(A)` → `OpenFiles`；`remove(B)` 照常成功；B 的删除不影响 A | `layerfs-daemon/tests/forced_unmount.rs::fp22_a_reference_in_a_never_holds_b_and_force_on_a_signals_no_process` |
| `remove_has_no_force_path` | 即使 `discard = true` 并带匹配的凭据，进程占用时仍然返回 `OpenFiles`；`remove` 从不返回 `Busy`，排队是 daemon 的事；API 上没有 force 参数 | `layerfs-daemon/tests/forced_producers.rs::fp32_force_is_busy_while_a_commit_is_held_before_save_finish`；`layerfs-daemon/tests/forced_producers.rs::fp32_force_is_busy_while_a_commit_is_held_before_publication`；`layerfs-daemon/tests/forced_producers.rs::fp32_force_is_busy_while_a_commit_is_held_before_its_local_install` |
| `remove_unresolved_scene_requires_discard_confirmation` | 会话的某个工作区是 `Unknown`（`move_workspace` 中途失败留下的现场）或 `Missing`：不带 discard → `Dirty`，`detail.unresolved` 列出它；带 discard 和匹配的凭据 → 成功。从未成为 Active 的会话（`new` 留下的 `Unknown` 现场）不需要 discard | `layerfs-daemon/tests/forced_producers.rs::fp32_unknown_commit_custody_is_forced_only_when_relinquished_and_stays_unknown` |
| `remove_failure_midway_is_unknown_and_keeps_scene` | failpoint 在 `delete` 阶段注入 `EIO`：返回 `Unknown`；已删和未删的部分（回收站目录、剩下的 pin）都原样留下；状态为 `Unknown`，`phase = "delete"`；不重试；`status` 报出这个现场；之后的 `remove` 需要 discard 和凭据 | `layerfs-api/tests/sandbox_operations.rs::failed_stop_preserves_original_owner_channel_inputs_and_attempt_without_delete`；`layerfs-api/tests/sandbox_operations.rs::failed_delete_retains_acknowledged_stop_and_exact_delete_failure` |
| `remove_deletes_only_its_own_session` | 删除 A 之后：A 的会话根、A 在每个影子仓库里的管理目录、A 的 pin 都不在了；B 的工作区、B 的管理目录与 pin、`bases/`、依赖集、主 checkout 逐字节不变。会话根里指向会话外的符号链接（dev-link 建的 `node_modules/tailwindcss`）只删链接本身，目标不受影响 | `layerfs-api/tests/sandbox_operations.rs::close_stops_then_deletes_the_original_owner_without_deleting_its_borrowed_volume`；`layerfs-daemon/tests/workspace_databases.rs::terminal_unmount_deletes_only_its_file_after_retained_requests_release`；`layerfs-sandbox/tests/engine_lifecycle.rs::exited_daemon_is_stopped_by_the_engines_not_modified_then_deleted`；`layerfs-sandbox/tests/engine_lifecycle.rs::a_stopped_container_is_deleted_once_and_never_forced` |

### config 级：只列出，不由 session 实现

module-rulings 规定：V3 的拒绝（含符号链接别名的写法）在 daemon 加载配置时由 `Config::validate` 做，session.md 只把这两条列为 config 级测试。它们由 strata-types 的测试（静态嵌套）和 daemon 加载配置的测试（别名）承担，不进 §8 的任务。

| 测试名 | 断言 | 来源（矩阵行 或 新增） |
| --- | --- | --- |
| `config_v3_root_and_repo_paths_never_nest` | 三种写法：`paths.root` 位于某个 `path` 之内；某个 `path` 位于 `paths.root` 之内；Store 落在主 checkout 里（`paths.root = <path>/.strata`）。每种都由 `Config::validate` 返回 `InvalidInput`（`detail.rule = "V3"`），daemon 加载配置时整份拒绝：不会有会话、没有目录，主 checkout 的 mtime/ctime 不变。`..` 绕行的写法在 V2 就被拒 | `layerfs-project/tests/acquisition_custody.rs::backing_inside_source_is_refused_before_an_operation_begins`；`layerfs-project/tests/init_sqlite.rs::a_store_inside_the_source_is_refused_before_an_operation_begins` |
| `config_v3_symlink_alias_refused_at_load` | `paths.root` 经符号链接别名落进某个主 checkout：字面路径不嵌套，解析后嵌套。daemon 加载配置时拒绝（`InvalidInput`，`detail.rule = "V3"`；按解析后的路径做 V3 检查，解析由 daemon 做）；session 不重复这条检查 | 新增；理由出处 `layerfs-project/tests/acquisition_custody.rs::backing_inside_source_is_refused_before_an_operation_begins` |

## 8. 实现任务（TDD）

六个任务按顺序做，每个任务都先写失败的测试。§7 主表里的每个测试都落在下面某个任务的第一步；config 级的两条按 §7 的说明不在这里。测试的公共脚手架放在 `crates/strata-session/tests/support/mod.rs`，包括：建裸仓库和主 checkout、写配置、算 `RepoId`、开 Store、`hdiutil` 卷、持有进程。它在 Task 1 建立，之后各任务按需扩充。

### Task 1: `new` 的写前拒绝

**Files:**
- Create: `crates/strata-session/src/lib.rs`、`crates/strata-session/src/ticket.rs`、`crates/strata-session/src/new/validate.rs`
- Test: `crates/strata-session/tests/new_refusals.rs`、`crates/strata-session/tests/support/mod.rs`

**Interfaces:**
- Consumes: types.md 的 `Config`、`RepoConfig`、`RepoName`、`RepoId`、`SessionId`、`StrataError`、`Code`；`apfs::same_volume`；`gitio::Git` 的 `fetch`、`tree_of`、`is_ignored`、`command`；`store::Store`
- Produces: `Sessions::open`、`Sessions::new` 的阶段 A（§5.1 第 1–6 步）、`set_next_id`

- [ ] **Step 1: 写失败的测试**：`new_typed_refusals_leave_no_trace`、`new_passes_lower_codes_through_unchanged`、`new_validates_ticket`、`new_refuses_cross_volume_root`、`new_refuses_non_apfs_root`、`new_refuses_existing_session_dir_before_any_effect`、`new_refuses_explicit_seed_tracked_or_unignored`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-session --features test-seams --test new_refusals`
Expected: FAIL（`Sessions::new` 还不存在）

- [ ] **Step 3: 写让测试通过的最少实现**：ticket 白名单与保留形式；`repos` 校验；每个仓库的 `lstat` 检查（符号链接按 `NotDirectory`）；同卷检查与 `statfs` 检查；fetch 与链顶规则；`tree_of`；种子判定（`ls-tree` 判被跟踪、`is_ignored` 判被 ignore、`lstat` 判类型）；生成会话 id 并检查 `Exists`。各码的 `detail` 按 §5.1。这时阶段 A 通过之后直接返回一个占位错误，测试只覆盖拒绝的路径。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-session --features test-seams --test new_refusals`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-session/src/lib.rs crates/strata-session/src/ticket.rs crates/strata-session/src/new/validate.rs crates/strata-session/tests/new_refusals.rs crates/strata-session/tests/support/mod.rs
git commit -m "feat(session): refuse invalid new requests before writing"
```

### Task 2: `new` 建出整套兄弟布局

**Files:**
- Create: `crates/strata-session/src/new/mod.rs`、`crates/strata-session/src/gitwire.rs`、`crates/strata-session/src/seed.rs`
- Test: `crates/strata-session/tests/new_layout.rs`

**Interfaces:**
- Consumes: `gitio::Git` 的 `export_tree`、`update_ref`、`command`；`deps::inputs_hash`、`deps::adopt`、`deps::build`；`apfs::clone_tree`、`apfs::clone_file`；`store::Store`
- Produces: `Sessions::new` 的阶段 B–E（§5.1 第 7–12 步）、`seed_files`、影子仓库与管理目录（§4.2）

- [ ] **Step 1: 写失败的测试**：`new_clones_entire_sibling_layout`、`new_workspace_equals_base_tree`、`new_workspace_keeps_seed_bytes_and_link_targets`、`new_refuses_symlinked_repo_path`、`new_refuses_fifo_seed_before_writing`、`new_pins_chain_top_when_ahead`、`new_pins_default_branch_when_publish_branch_differs`、`new_seeds_default_names_only_when_untracked_and_ignored`、`new_clones_seed_caches_as_real_directories`、`workspace_git_is_detached_and_prepush_refuses`、`manifest_round_trips_through_store`、`new_warm_under_5s`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-session --features test-seams --test new_layout`
Expected: FAIL（阶段 A 之后还没有实现，返回的是占位错误）

- [ ] **Step 3: 写让测试通过的最少实现**：影子仓库的建立与镜像刷新（alternates、选定的 config 键、`info/exclude`、`info/attributes`、`pre-push`）；基线导出；向 deps 要依赖集（复用、认领、冷装）；写意图行；克隆基线和依赖目录；写管理目录 `<sid>.1`、gitfile，再跑 `read-tree` 与 `update-index --refresh`；pin；只给 tracked 仓库克隆种子缓存；`seed_files` 的两类条目与复制；定稿事务。`new_warm_under_5s` 标 `#[ignore]`。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-session --features test-seams --test new_layout`（性能测试另跑：`cargo test -p strata-session --features test-seams --test new_layout new_warm_under_5s -- --ignored`）
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-session/src/new/mod.rs crates/strata-session/src/gitwire.rs crates/strata-session/src/seed.rs crates/strata-session/tests/new_layout.rs
git commit -m "feat(session): build the full sibling layout with shadow-repo git"
```

### Task 3: 识别与状态

**Files:**
- Create: `crates/strata-session/src/resolve.rs`、`crates/strata-session/src/status.rs`
- Test: `crates/strata-session/tests/resolve_status.rs`

**Interfaces:**
- Consumes: `store::Store`（会话、工作区、种子行，最新 checkpoint，`DepsSet`）；`deps::inputs_hash`、`deps::open_files`、待确认提议的列出与 `Proposal`；`gitio::Git::is_ignored`；`overlay::checkpoint`（只在测试里用来造 checkpoint）
- Produces: `Sessions::resolve`、`Sessions::select`、`Sessions::list`、`Sessions::status`、`SessionStatus`、`WorkspaceStatus`、`StatusWarning`

- [ ] **Step 1: 写失败的测试**：`new_same_ticket_creates_another_session`、`new_lost_reply_is_found_not_replayed`、`resolve_by_cwd_inside_workspace`、`select_by_id_or_ticket`、`sessions_on_one_base_are_isolated`、`seed_files_listed_in_status_and_never_checkpointed`、`status_warns_untracked_repo_edits_not_checkpointed`、`status_reads_workspace_presence_from_filesystem`、`status_reports_drift_and_pending_proposals`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-session --features test-seams --test resolve_status`
Expected: FAIL（`resolve`、`select`、`list`、`status` 还不存在）

- [ ] **Step 3: 写让测试通过的最少实现**：`F_GETPATH` 取规范路径，再按 §5.3 逐段匹配；按 id 或 ticket 选择；`list`；`status` 的九步（§5.4），其中 `Missing` 直接看文件系统，种子的 ignore 每个仓库批量查一次。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-session --features test-seams --test resolve_status`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-session/src/resolve.rs crates/strata-session/src/status.rs crates/strata-session/tests/resolve_status.rs
git commit -m "feat(session): resolve sessions by cwd or selector and report status"
```

### Task 4: `Unknown` 现场与重启认领

**Files:**
- Create: `crates/strata-session/src/scene.rs`
- Modify: `crates/strata-session/src/new/mod.rs`（每个阶段记 phase，接上 failpoint）
- Test: `crates/strata-session/tests/new_unknown.rs`

**Interfaces:**
- Consumes: `store::Store`
- Produces: `Scene`、`SceneOp`、`Sessions::recover`、`set_failpoint`、§6.3 的 `detail` 与 `message`

- [ ] **Step 1: 写失败的测试**：`new_failure_after_writing_began_is_unknown`、`new_unknown_detail_names_the_kept_scene`、`unknown_scene_survives_daemon_restart`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-session --features test-seams --test new_unknown`
Expected: FAIL（还没有 failpoint、现场记录和 `recover`）

- [ ] **Step 3: 写让测试通过的最少实现**：用一个"已开始写"标志按 §6.1 分类错误；每个阶段开始时在 Store 记 phase；`Unknown` 的 `detail` 与 `message` 按 §6.3；把 `Scene` 存进会话行；`recover` 按 §5.7 处理，不碰文件系统；failpoint 在 `test-seams` 下注入 errno，或就地返回。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-session --features test-seams --test new_unknown`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-session/src/scene.rs crates/strata-session/src/new/mod.rs crates/strata-session/tests/new_unknown.rs
git commit -m "feat(session): keep and report Unknown scenes across restarts"
```

### Task 5: `move_workspace`

**Files:**
- Create: `crates/strata-session/src/move_workspace.rs`
- Modify: `crates/strata-session/src/gitwire.rs`（管理目录的代数 n）、`crates/strata-session/src/scene.rs`（移动意图）
- Test: `crates/strata-session/tests/move_workspace.rs`

**Interfaces:**
- Consumes: `overlay::checkpoint`（`CheckpointReason::BeforeRollout`）、`overlay::rebase` 与 `Overlay`、`overlay::reconcile_full`；`deps::open_files`；`gitio::Git` 的 `tree_of`、`export_tree`、`update_ref`、`command`（`merge-base`、`read-tree`、`update-index`）；`apfs::clone_tree`、`apfs::clone_file`、`apfs::walk`；`store::Store` 与 `get_blob`
- Produces: `Sessions::move_workspace`、`Sessions::repo_name`、`MoveReport`

- [ ] **Step 1: 写失败的测试**：`base_generation_never_modified_in_place`、`move_workspace_validates_before_writing`、`move_workspace_swaps_base_and_deps_atomically`、`move_workspace_rechecks_open_files_before_swap`、`move_workspace_projects_overlay_and_marks_conflicts`、`move_workspace_carries_ignored_content`、`move_workspace_keeps_old_tree_while_in_use`、`move_workspace_seconds_warm`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-session --features test-seams --test move_workspace`
Expected: FAIL（`move_workspace` 还不存在）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.6 的 20 步。要点：RepoId 到 RepoName 的映射与不匹配规则；先 checkpoint 再在旁边准备新树；投影与携带的规则；新管理目录 `<sid>.<n>`；交换前复查打开的文件；先记移动意图再 `renamex_np(RENAME_SWAP)`；移 pin；用一个事务记录；旧树核对一致且没人用时才删除，否则保留。`move_workspace_seconds_warm` 标 `#[ignore]`。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-session --features test-seams --test move_workspace`（性能测试另跑：`cargo test -p strata-session --features test-seams --test move_workspace move_workspace_seconds_warm -- --ignored`）
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-session/src/move_workspace.rs crates/strata-session/src/gitwire.rs crates/strata-session/src/scene.rs crates/strata-session/tests/move_workspace.rs
git commit -m "feat(session): move base and deps together with an atomic directory swap"
```

### Task 6: `remove`

**Files:**
- Create: `crates/strata-session/src/remove.rs`、`crates/strata-session/src/remove_tree.rs`
- Test: `crates/strata-session/tests/remove.rs`

**Interfaces:**
- Consumes: types.md 的 `ConfirmToken`、`Action`；`overlay::reconcile_full`、`overlay::checkpoint`（`CheckpointReason::BeforeRemove`）；`deps::open_files`；`gitio::Git::update_ref` 与 `command`（删 pin）；`apfs::open_rooted`；`store::Store`
- Produces: `Sessions::remove`、`RemoveReport`

- [ ] **Step 1: 写失败的测试**：`workspace_git_never_writes_main_checkout`、`removed_session_id_never_resolves_or_reissued`、`remove_refuses_unpublished_without_discard`、`remove_untracked_repo_requires_discard`、`remove_discard_checks_the_token`、`remove_after_mtime_only_change_needs_no_discard`、`remove_refuses_when_a_process_holds_fd_cwd_or_mmap`、`remove_open_files_in_one_session_never_block_another`、`remove_has_no_force_path`、`remove_unresolved_scene_requires_discard_confirmation`、`remove_failure_midway_is_unknown_and_keeps_scene`、`remove_deletes_only_its_own_session`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-session --features test-seams --test remove`
Expected: FAIL（`remove` 还不存在）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.5 的十步。要点：核对凭据的 `action`；对整个会话根查打开的文件，`discard` 不绕过；用冷扫判断未发布改动，加上 `tracked = false` 与未决现场两条规则；`BeforeRemove` checkpoint；改名到回收站；删 pin 与管理目录；按 `O_NOFOLLOW` 和同一 `st_dev` 递归删除；记 `Removed`，行永久保留。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-session --features test-seams --test remove`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-session/src/remove.rs crates/strata-session/src/remove_tree.rs crates/strata-session/tests/remove.rs
git commit -m "feat(session): remove sessions behind discard confirmation and open-file checks"
```

## 9. 构建者决定记录

| 事项 | 选择 | 理由 | 日期 |
| --- | --- | --- | --- |
| 工作区 `.git` 的接法（design §12） | 每个 repo 一个影子仓库（裸仓库，alternates 指向主 checkout 的对象库），工作区是它的 linked worktree；管理目录 `worktrees/<session-id>.<n>/` 由 session 手写，带 `locked`；主 checkout 里只写 `refs/strata/pins/<session-id>` | linked worktree 的 hooks、config、refs 取自 common dir，接在主 checkout 上就装不了只对工作区生效的 `pre-push`，还会让 front 的 dev-link 把兄弟工作区 `reset --hard origin/main`（§4.2）；已在 `/tmp` 实测 | 2026-10-22 |
| index 的 stat 刷新 | `new` 和移动时都在工作区里跑 `update-index -q --refresh`。把 `core.checkStat=minimal` 加预制 index 作为优化，留待 spike 实测后再定 | 刷新之后 `git status` 不再重新哈希整棵树；先要正确，再求快 | 2026-10-22 |
| `rm` 前最后一个 checkpoint 的保留期（design §6.6） | 待定。由 store 的 gc 保留期决定（store.md、daemon.md）；session 只在 `RemoveReport` 里给出 `CheckpointId` | — | — |
| fetch、克隆、index 刷新的并发度 | 待定：按仓库有界并行，上限由首个 spike 实测决定（design §8） | 22 个仓库串行 fetch 很可能超过 5 s | — |
| 测试注入机制 | `test-seams` feature 下手写的 `set_failpoint` 与 `set_next_id`，不引入 failpoint crate | 只需按阶段名注入一种 errno 或就地返回；手写的开关几十行就够 | 2026-10-22 |
| 会话 id 的随机源 | `arc4random_buf` 取 40 bit，编码成小写 base32 | macOS 自带，不加依赖；types.md 规定了格式 | 2026-10-22 |
