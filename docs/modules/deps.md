# `deps` 模块

**Crate**：`crates/strata-deps` · **设计来源**：design.md §3（dev-link 写依赖目录、锁文件不一定在 git 里、symlink 陷阱）、§4（Deps set、Seed cache）、§6.1 第 1 步、§6.4、§6.5 第 7 步、§7、§8、§9 不变量 2、4、5、6、8；module-rulings.md 的「T8 deps」，以及 Addendum 1–4 里标了 T8 或 Everyone 的条目 · **矩阵**：目标模块为 `deps` 的 5 行（3d 两行、3f 三行），以及理由点名 deps 的 1 行 `—`（3b）

## 1. 职责与边界

deps 负责依赖集（design §4 的 Deps set）里"算"的那一半：算键、建 generation、判漂移、记提议、出 rollout 计划。

| 负责 | 对应 design |
| --- | --- |
| 安装输入哈希 `inputs_hash` 与依赖集 id `set_id`，包括 `platform`、`toolchain` 的采集，以及进入键的依赖配置（`DepsConfigKey`） | §3"锁文件不一定在 git 里"、§4 |
| 依赖集 generation 的建立与封存。`adopt` 认领主 checkout 现成的依赖目录；认领不了时，`build` 在 staging 克隆里冷装；`ensure` 按"已有 → 认领 → 冷装"的顺序把两者串起来。封存时排除嵌套在依赖目录里的种子缓存 | §6.1 第 1 步、§6.4 第 2 步、§7 |
| `clone_into`：把封存的依赖集克隆进调用方新建的目录。只用 `clonefile`，从不 symlink | §3 symlink 陷阱、§8 |
| `drift`：判漂移。只比安装输入哈希，不比目录内容 | §3"dev-link 写依赖目录"、§4 |
| `observe_main`：记录主干当前的 (tree, head, 安装输入哈希, 依赖集 id)，id 变了就登记提议。它只计算，从不封存、从不冷装 | §6.4 第 1 步 |
| `Proposal`：依赖变更提议的登记、状态、去重、提升与取代；`ensure_target`：批准之前把提议的目标依赖集封存好 | §6.4 第 4–5 步、§6.5 第 7 步 |
| `RolloutPlan`：对每个会话给出"移到哪个基线"或"为什么跳过"。依赖目录上打开的文件由 `open_files` 检查 | §6.4 第 5 步、不变量 4、8 |

明确不负责的事，以及交给谁：

- **移动工作区**：基线与依赖目录一起交换的原子动作是 `session::move_workspace(session, repo, new_base, head) -> Result<MoveReport>`（types.md §8）。它在交换前记一个 `CheckpointReason::BeforeRollout` checkpoint，交换前再查一次打开的文件，有就返回 `OpenFiles`（module-rulings Addendum 1 T8）。deps 只出计划：从不写工作区，也不调用 session 或 publish，依赖方向也不允许它这样做。
- **人类确认**：`deps approve` 用的 `ConfirmToken`（`Action::DepsApprove`）由 confirm 签发与校验，由 daemon 核对 action（types.md §4"谁消费"）；TTY 检查在 cli。deps 的任何函数都不收凭据。
- **种子缓存与种子文件的克隆**：由 session::new 负责（design §4、§6.1 第 4 步）。deps 只保证封存时排除嵌套在依赖目录里的种子缓存，并用 `nested_seed_caches` 告诉调用方是哪几个。
- **基线导出与基线 generation 目录**：由 session 负责。它经 `gitio::export_tree` 导出源码树，调 `ensure` 取依赖集，用 `clone_into` 把依赖集克隆进它自己新建的目录，再改名到 `Store::base_dir` 并登记（module-rulings Addendum 3"Base generation layout"）。deps 只读调用方交来的导出目录。
- **发布后 rebase**：由 publish::rebase_session 负责。新基线需要另一个依赖集时，它调 `deps::propose` 登记提议，会话停在旧基线上等 `deps approve`（module-rulings 公共条 3：不变量 4 优先于 design §6.4 的最后一句）。deps 对它的要求见 §5.12。
- **repo 写锁与 `Busy`**：由 daemon 负责（module-rulings T13）。daemon 经 `Ctx.locks`（`RepoWriteLock`，§3）把它交给 deps；deps 只在封存的临界区（rename、chmod、同步、登记）里持有它，从不跨过 install（Addendum 3/4"Locks"）。deps 自己另有"每个依赖集一把建造锁"（§4.1）。
- **持久化**：表、索引、触发器与全部 SQL 都在 store（Addendum 4"Schema ownership"）。deps 拥有自己四类记录的词汇、转换图与含义（§4.2、§4.5），经 store 的具名方法读写，从不执行 SQL。
- **generation 的回收**：由 store 的 gc 负责（design §7"空间回收"）。依赖集的引用规则与封存目录的回收要求写在 §4.6。
- **依赖目录的内容**：dev-link 每次 dev/build 都把兄弟库镜像进 `node_modules/@inter/*`，并在布局根建 `../node_modules/tailwindcss` 软链（`tool/dev-link.mjs`、`front/dev-link.sh`）；bun 和 composer 也会改写依赖目录。工作区里的依赖目录是会话自己的可写 CoW 克隆，deps 不解释、不监听、不 checkpoint 它的内容（design §6.1"依赖目录的归属"）。

## 2. 依赖

| 依赖 | 理由 |
| --- | --- |
| `strata-types` | 标识与结构（`DepsSet`、`DepsSetId`、`BaseGen`、`WorkspaceState`、`ProposalId`、`SessionId`、`TreeOid`、`CommitOid`、`RepoName`、`RepoPath`）、`StrataError` 与 `Code`、`RepoConfig` 与 `Paths`（config.md §3），以及 config.md §2.4 的命令行切分：`install` 按同一规则切成 argv [我的决定，§9] |
| `strata-apfs` | `clone_tree`：认领、staging 与 `clone_into`，跨卷时返回 `CrossVolume`，从不退化成复制；`clone_file`：把会话的安装输入放进 staging；`walk` 与 `Attrs`：展开安装输入目录、认领前后的两次遍历、封存候选的校验与同步；`open_rooted`：每级 `O_NOFOLLOW` 地读安装输入，FIFO 不阻塞；`same_volume`：预检；rooted、no-follow 的目标操作（module-rulings Addendum 3"Rooted destinations"）：往 staging 与调用方的目录里删、放、克隆 |
| `strata-gitio` | `Git::is_ignored(worktree, path)`：在 git 工作树（主 checkout 或会话工作区）里展开安装输入目录时跳过被 ignore 的成员。`Git::tree_of(commit)`：核对提议与主干记录的 head 与 tree 相符 |
| `strata-store` | 依赖集、尝试、提议、主干四类记录的读写，方法名由 module-rulings Addendum 4 定（下面"对 store"）；`Store::deps_dir` 与 `Store::deps_staging_dir` 给出封存目录与暂存目录的路径；`Store::retire_dir`（Addendum 3）把不再需要的暂存原子地挪进 store 的 trash |
| `blake3` | `inputs_hash`、`set_id` 与文件摘要 |
| `serde`、`serde_json` | `Proposal`、`RolloutPlan`、`Target` 等类型的 JSON，以及 package manifest 的规范化（§4.4） |
| `libc` | libproc（`proc_listallpids`、`proc_pidinfo`、`proc_pidfdinfo`、`proc_name`）、`fcntl`（`F_GETPATH`、`F_BARRIERFSYNC`、`F_FULLFSYNC`）、`flock`、`renamex_np`、`setpgid`、`killpg`。design §11 第 2 条规定 macOS API 走 `libc` |

依赖方向：deps 在第 2 层（types.md §8），只依赖第 0–1 层。它不依赖 overlay，因为漂移比的是安装输入哈希；也不依赖 session、publish、daemon、confirm。daemon 的写锁经 deps 自己定义的 trait `RepoWriteLock` 注入，不需要反向依赖。

调用 deps 的模块：

| 调用方 | 用到的接口 |
| --- | --- |
| session | `new`：`ensure`、`find_set`、`clone_into`、`nested_seed_caches`、`observe_main`；`move_workspace`：`find_set`，交换前复查用 `open_files`；`remove`：`open_files`；`status`：`drift`、`open_proposals`、`attempts` |
| publish | `rebase_session`：`key_of`、`propose`、`observe_main`（§5.12） |
| daemon | `deps propose`：`drift` → `build` → `propose`，提出者自换用 `plan_switch`；`deps approve`：`get_proposal` → 人类确认 → `ensure_target` → `plan_rollout` → 逐个 Move 调 `session::move_workspace` → `record_rollout`；checkpoint 碰到安装输入（`is_input`）后重算 `drift`；status 与 stderr 提示用 `open_proposals`；实现 `RepoWriteLock`，给出 `InstallEnv` |
| cli | 只用类型：`Proposal`、`RolloutPlan`、`Target` 的 JSON，经 daemon lib 的 RPC 类型 |

### 上游要求

下面是 deps 依赖、而邻居文档还没写或正在修订的名字与行为。

**对 store**（store.md 的修订轮已提交：sdd/task-6 223f9d3。名字与签名都取自它的 §3，语义见本文）：

1. deps 调用的方法。写与读的名字由 Addendum 4 定，每个写方法一个写事务；`retire_dir` 的名字由 Addendum 3 定，`deps_staging_dir` 是 store 自己加的：
   - 写：`begin_deps_attempt(repo, set, kind: AttemptKind, at) -> u64`、`set_deps_attempt_pid(attempt, pid) -> bool`、`end_deps_attempt(attempt, to: AttemptState, error, at) -> bool`、`record_deps_set(set, origin: DepsOrigin, attempt: Option<u64>, at)`、`record_proposal(new: &NewProposal, replaces: Option<ProposalId>, at) -> Option<ProposalId>`、`promote_proposal(id, tree, head, at) -> bool`、`decide_proposal(id, to: ProposalDecision, at) -> bool`、`set_deps_main(row: &DepsMain) -> Option<DepsMain>`（返回同一事务里读到的旧记录）。
   - 读：`deps_set(repo, id) -> Option<DepsSetRecord>`、`deps_attempts(repo) -> Vec<DepsAttempt>`、`proposal(id) -> Option<ProposalRecord>`、`proposals(repo, open_only) -> Vec<ProposalRecord>`。同样定名的 `deps_main` deps 用不着：`set_deps_main` 已经返回旧记录。
   - 路径与目录：`deps_dir(repo, id)`、`deps_staging_dir(repo, attempt)`、`retire_dir(path)`（store.md §5.5 表第 3 行：Running、Sealed、Refused 的尝试的暂存都收，Failed 的是现场，不收）。
   - 行类型到 §3 类型的映射由 deps 做（Addendum 4）：`DepsSetRecord.set` → `DepsSet`；`DepsAttempt` → `BuildAttempt`，其中 `AttemptKind` → `BuildKind`，`AttemptState` 加上建造锁的探测与暂存是否还在 → `BuildState`；`ProposalRecord` → `Proposal`，其中 `origin: Option<SessionId>` → `ProposalOrigin`，`status` 与 `superseded_by` → `ProposalState`。
2. store 选的两条语义，deps 都确认：
   - `record_proposal` 返回 `Option<ProposalId>`，`None` 表示 `replaces` 已经不是该位置上的那一条，store 什么都没写：确认。这正是 §5.9 的比较并交换。deps 收到 `None` 就重读、重新决定，最多 3 次，之后返回 `Busy`（reason proposal_race）；`ensure_target` 收到 `None`，说明 P 已被取代，返回 `NoSuchProposal`，不登记替换（§5.10）。Addendum 4 的"返回 ProposalId"由 `Some` 兑现。
   - `promote_proposal` 同时取代该 repo 当前的 Pending，以及 to 相同的别的 AwaitingPublish：确认，§4.5 与 §5.9 依赖它。它对当前的 Pending 不做比较并交换，deps 也接受：要不要提升不取决于当前是哪一条 Pending，后写者胜，与 `record_proposal` 的取代规则一致。
3. store 还缺、deps 需要的：`gc_repo` 第 3 步（挪走回收的依赖集目录）与第 4 步（清扫没登记的 `deps/<repo>/<deps>`）在 `renamex_np` 之前，先对要挪的目录本身 `chmod u+w`。封存根目录是 0555（D1），把 0555 的目录改名到另一个父目录得到 `EACCES`（2026-10-22 本机实测）；store.md 223f9d3 只在删 trash 时处理 0555（§5.11 第 5 步），挪进 trash 这一步还没有（§4.6）。
4. 已经照写、不再是要求：Empty 的依赖集不收尝试；提议与主干记录对依赖集没有外键（`to_set` 可以还没封存，§4.2）；`proposals(open_only = true)` 去掉提出会话已被删除的 AwaitingPublish；gc 不进 `.staging/`、`.locks/`，也不碰 Running 与 Failed 的尝试的暂存（守卫 (a)）。

**对 session**（session.md 的修订轮；module-rulings Addendum 3"Deps API"）：

1. 取依赖集只调 `ensure`：不自己调 `adopt`、`build`，也不自己算 set id。
2. 调 `ensure`、`adopt`、`build`、`ensure_target` 时不持有任何 repo 写锁：冷装在锁外，deps 在封存时自己取锁（D6）。
3. 建基线 generation 目录：先把源码树导出到自己新建的目录；`ensure` 之后，持该 repo 写锁时先用 `find_set` 复查（gc 可能在两者之间回收一个没人引用的旧依赖集；复查落空就放开锁、再 `ensure`），再 `clone_into` 进那个目录，然后改名到 `Store::base_dir` 并登记。`clone_into` 只往这种新建的目录里写。
4. `new` 钉住一个 tip 之后，调 `observe_main(ctx, tree, head, 该 tree 的基线目录)`。
5. `move_workspace` 交换之前，对工作区的依赖目录（新旧依赖集 `dirs` 的并集）复查 `open_files`，有就返回 `OpenFiles`。
6. `status`：漂移用 `drift`，待确认的提议用 `open_proposals`，现场用 `attempts`；"在等 deps approve"按 §4.5 的规则判定。

**对 gitio**：用到的两个名字 gitio.md 的修订轮（sdd/task-5，ca2c280）已经写明，这里只记 deps 依赖的那几条。`Git::is_ignored(worktree, path)` 收工作区的根，经它的 gitfile 找到会话的影子仓库（module-rulings Addendum 2），对主 checkout 与工作区都能回答，含镜像的 `info/exclude` 与全局排除文件；已跟踪的文件永不被 ignore；没有 `.git` 时返回 `NoSuchPath`，所以 deps 只对有 `.git` 的根调用它（§5.1）。`Git::tree_of(commit)` 对不在库里的 commit 返回 `NoSuchCommit`，另有 `Unsupported`、`NotText`、`InvalidInput`、`Unknown`，deps 都原样透传。

**对 apfs**（apfs.md 还没写）：计划 Task 4 定名的 `clone_tree`、`clone_file`、`same_volume`、`walk` 与 `Attrs`；module-rulings T4 的 `open_rooted`；`walk` 报告符号链接的目标、从不跟随（T4）；有可写 mmap 时 gen count 报告为未知（Addendum 1 T4）；Addendum 3 的 rooted 目标操作，名字由 apfs.md 定。

**对 daemon**（daemon.md 还没写）：实现 `RepoWriteLock`（有界等待之后 `Busy`）并放进 `Ctx`；给出 `InstallEnv`（显式 PATH 与时限）；按 §5.12 的顺序执行 `deps approve`，`Target::Replaced` 时什么都不移动、把替换提议报给人。

**对 publish**（publish.md 还没写）：`rebase_session` 按 §5.12 的契约，用 `key_of` 算新基线的依赖集，决定直接移动还是等待、以哪个 origin 调 `propose`；推进了新会话会钉的 tip 之后调 `observe_main`。

## 3. 公开接口（Rust）

```rust
// crates/strata-deps/src/lib.rs —— 签名清单（不是实现）
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use strata_gitio::Git;
use strata_store::Store;
use strata_types::{
    BaseGen, CommitOid, DepsSet, DepsSetId, Paths, ProposalId, RepoConfig, RepoName, RepoPath,
    SessionId, StrataError, TreeOid, WorkspaceState,
};

pub type Result<T> = std::result::Result<T, StrataError>;

/// 一次 deps 操作的上下文。各字段都由 daemon 持有，按引用传入。
pub struct Ctx<'a> {
    pub store: &'a Store,
    pub paths: &'a Paths,
    pub repo: &'a RepoName,
    /// `repo` 的配置（config.md 的 `[repo.<name>]`）。
    pub cfg: &'a RepoConfig,
    /// `repo` 的 git，打开在主 checkout 上。
    pub git: &'a Git,
    pub env: &'a InstallEnv,
    /// daemon 的 per-repo 写锁；deps 只在封存的临界区里取它（§5.3）。
    pub locks: &'a dyn RepoWriteLock,
}

/// daemon 的 per-repo 写锁（design §8；module-rulings T13、Addendum 3/4"Locks"）。
/// daemon 实现它并经 Ctx 交给 deps [我的决定]：封存必须持有它，install 又必须在它外面跑，调用方没法替 deps 拿。
pub trait RepoWriteLock: Sync {
    /// 拿到 `repo` 的写锁后执行一次 `f`，`f` 返回就释放。
    /// 外层的 Err 只有 `Busy`：有界等待（daemon 定，缺省 30 s）之后仍没拿到锁，`f` 没有运行。Ok 里是 `f` 的结果。
    /// deps 从不在持有它时再调它，也从不在持有它时等建造锁（D6）。
    fn with_write(&self, repo: &RepoName, f: &mut dyn FnMut() -> Result<()>) -> Result<Result<()>>;
}

/// 运行 `install` 和 `<程序> --version` 的环境。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallEnv {
    /// 显式的 PATH，由 daemon 按 daemon.md 给出。launchd 的环境里没有用户的 PATH（config.md §2.4 第 4 条）。
    pub path: String,
    /// 一次 `install` 的时限，缺省 60 分钟 [我的决定]。超时就杀掉整个进程组。
    pub timeout: Duration,
}

// ---- 键（§4.3、§4.4、§5.1、§5.2）----
/// 进入依赖集 id 的依赖配置（§4.3）：改了它们，装出来的东西或封存的范围就可能变。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DepsConfigKey {
    /// `deps_dirs`，按字节序。
    pub deps_dirs: Vec<RepoPath>,
    /// `nested_seed_caches(cfg)`，按字节序。
    pub nested_seed_caches: Vec<RepoPath>,
    /// `install` 按 config.md §2.4 切出的 argv，保持顺序；没有 `install` 时为空。
    pub install: Vec<String>,
}

impl DepsConfigKey {
    pub fn of(cfg: &RepoConfig) -> Result<DepsConfigKey>;
}

pub fn inputs_hash(cfg: &RepoConfig, git: &Git, root: &Path) -> Result<String>;
pub fn set_id(inputs_hash: &str, platform: &str, toolchain: &str, config: &DepsConfigKey) -> DepsSetId;
/// `tree` 的 (inputs_hash, set id)。ensure、observe_main 与 publish 的 rebase_session 都用它，免得各自拼键 [我的决定]。
pub fn key_of(ctx: &Ctx, tree: &Path) -> Result<(String, DepsSetId)>;
pub fn platform() -> String;
pub fn toolchain(cfg: &RepoConfig, env: &InstallEnv) -> Result<String>;
pub fn is_input(cfg: &RepoConfig, path: &RepoPath) -> bool;
pub fn nested_seed_caches(cfg: &RepoConfig) -> Vec<RepoPath>;

// ---- 依赖集 generation（§4.1、§4.2、§5.3–§5.7）----
pub fn find_set(store: &Store, repo: &RepoName, id: &DepsSetId) -> Result<Option<DepsSet>>;
pub fn adopt(ctx: &Ctx, tree: &Path) -> Result<DepsSet>;
pub fn build(ctx: &Ctx, tree: &Path, inputs_from: Option<&Path>) -> Result<DepsSet>;
pub fn ensure(ctx: &Ctx, tree: &Path) -> Result<DepsSet>;
pub fn clone_into(store: &Store, set: &DepsSet, dst_root: &Path) -> Result<()>;
pub fn attempts(store: &Store, paths: &Paths, repo: Option<&RepoName>) -> Result<Vec<BuildAttempt>>;

/// 一次认领或冷装的尝试，对应 store 的一条尝试记录（§4.2）。status 用它报告失败与中断的现场（不变量 6）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildAttempt {
    /// store 分配的尝试序号；暂存目录是 `Store::deps_staging_dir(repo, id)`，即 `.staging/<id>/`。
    pub id: u64,
    pub repo: RepoName,
    /// 这次要封存的依赖集；尝试开始之前就已算出（§5.3）。
    pub set: DepsSetId,
    pub kind: BuildKind,
    pub state: BuildState,
    /// 暂存目录的绝对路径。
    pub staging: PathBuf,
    /// 冷装时 install 进程的 pid，它也是进程组组长。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<i32>,
    #[serde(with = "strata_types::serde_rfc3339")]
    pub started: SystemTime,
    /// 失败的原因。detail 的形状见 §6。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<StrataError>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BuildKind {
    Adopt,
    Build,
}

/// `attempts` 只报下面三种状态；已封存的、被具名拒绝的、暂存已被人删掉的尝试都不报。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BuildState {
    /// 建造锁仍被持有：正在建。
    Running,
    /// 记录还是 running，但建造锁没人持有，说明持有者死了（daemon 崩溃或被杀）。读的时候推出来，不写回；
    /// 下一个拿到这把建造锁的调用把它关成 failed（reason interrupted，§5.3）。
    Interrupted,
    /// install 失败或超时、封存前的校验没过、封存时出错，或者持有者死后被关闭。现场保留。
    Failed,
}

// ---- 漂移（§5.8）----
/// 漂移（design §4）：在某个目录里算出的安装输入哈希，与所钉依赖集记录的哈希不同。
/// 它是数据，不是错误；types.md 的 `Drift` 码只用于拒绝（§6）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Drift {
    /// 所钉的依赖集。
    pub pinned: DepsSetId,
    /// 所钉依赖集记录的安装输入哈希。
    pub pinned_inputs: String,
    /// 在工作区（或主干 tip 的导出目录）里算出的安装输入哈希。
    pub found_inputs: String,
}

pub fn drift(cfg: &RepoConfig, git: &Git, ws: &Path, pinned: &DepsSet) -> Result<Option<Drift>>;

// ---- 提议（§4.5、§5.9）----
/// 依赖变更提议（design §6.4 第 4 步）。所有者是 deps（types.md §2.1），session、daemon、cli、publish 按名字使用。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    pub id: ProposalId,
    pub repo: RepoName,
    pub origin: ProposalOrigin,
    /// 被替换的依赖集，以及它记录的安装输入哈希。
    pub from: DepsSetId,
    pub from_inputs: String,
    /// 新的依赖集，以及它的安装输入哈希。提议可以先于这个依赖集封存而存在（§5.9）。
    pub to: DepsSetId,
    pub to_inputs: String,
    /// 目标基线的 tree 与 commit。批准后，会话移到 BaseGen { repo, tree, deps: to }，HEAD 指向 head。
    /// 由会话发起、改动还没发布时，两者都是 None（state 为 AwaitingPublish）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree: Option<TreeOid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<CommitOid>,
    pub state: ProposalState,
    #[serde(with = "strata_types::serde_rfc3339")]
    pub created: SystemTime,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ProposalOrigin {
    /// 新的安装输入已经在一个 git tree 里：主干的 tip（`observe_main`）或发布链顶（`rebase_session`）。
    Main,
    /// 会话改了自己的安装输入（design §6.4 第 1 步"某会话改了锁文件"），由 `deps propose` 登记为草稿；
    /// 它的改动落地时，publish 的 `rebase_session` 以同一个 origin 带目标调 `propose`，把草稿提升为 Pending。
    Session { session: SessionId },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ProposalState {
    /// 由会话发起、改动还没发布：没有目标 tree，只有提出者自己可以先换（`plan_switch`）。
    /// 对它调 `deps approve` 返回 NoSuchProposal；提出会话被删除后不再列出（`open_proposals`）。
    AwaitingPublish,
    /// 有目标 tree，待确认，`deps approve` 可以用它。每个 repo 至多一条。
    Pending,
    /// 某次 approve 覆盖了该 repo 的全部会话之后，再没有会话需要它（见 `record_rollout`）。
    Applied,
    /// 被更新的提议取代：同一会话的新草稿、该 repo 新登记或提升的 Pending、批准时的替换提议；
    /// 草稿被提升时，该 repo 里 to 相同的别的草稿也被它取代（§4.5）。
    Superseded { by: ProposalId },
}

/// `propose` 的参数；repo 取自 `Ctx`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProposeArgs {
    pub origin: ProposalOrigin,
    pub from: DepsSetId,
    pub from_inputs: String,
    pub to: DepsSetId,
    pub to_inputs: String,
    /// 两者同为 Some，或同为 None。同为 None 只允许 origin = Session。
    pub tree: Option<TreeOid>,
    pub head: Option<CommitOid>,
}

pub fn propose(ctx: &Ctx, args: ProposeArgs) -> Result<Proposal>;
pub fn observe_main(ctx: &Ctx, tree: &TreeOid, head: &CommitOid, export: &Path) -> Result<Option<Proposal>>;
pub fn get_proposal(store: &Store, id: ProposalId) -> Result<Proposal>;
/// Pending，以及提出会话还在的 AwaitingPublish；提出会话已被删除的不列出（module-rulings Addendum 4），
/// 这一层过滤由 store 的 `proposals(repo, open_only = true)` 在读的时候做（§2 对 store 的要求第 1 条）。
pub fn open_proposals(store: &Store, repo: Option<&RepoName>) -> Result<Vec<Proposal>>;

// ---- 批准与 rollout 计划（§5.10）----
/// `ensure_target` 的结果。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Target {
    /// 提议的 `to` 已封存：daemon 接着拿 repo 写锁、调 `plan_rollout`。
    Ready { proposal: Proposal },
    /// 封存出的依赖集不是 `to`（提议之后工具链或依赖配置变了）：`superseded` 已被 `replacement` 取代，
    /// 这次 approve 不移动任何会话，人去批准 replacement（D10：`to` 从不改写）。
    Replaced { superseded: ProposalId, replacement: Proposal },
}

pub fn ensure_target(ctx: &Ctx, id: ProposalId, tree_dir: &Path) -> Result<Target>;

/// 一次 `deps approve`（或一次提出者自换）对一组会话的计划：每个会话一步，要么移动，要么跳过。
/// 所有者是 deps（types.md §2.1）。daemon 按 steps 的顺序，把每个 Move 交给 `session::move_workspace`。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RolloutPlan {
    pub proposal: ProposalId,
    pub repo: RepoName,
    /// true 表示计划覆盖了该 repo 的全部会话（approve 没有用 --sessions 收窄）。只有这样的计划能让提议变成 Applied。
    pub complete: bool,
    pub steps: Vec<RolloutStep>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum RolloutStep {
    /// 原样交给 `session::move_workspace(session, <repo 的 RepoId>, new_base, head)`，基线与依赖一起移动（不变量 8）。
    Move { session: SessionId, new_base: BaseGen, head: CommitOid },
    /// 不动这个会话。
    Skip { session: SessionId, reason: SkipReason },
}

/// 跳过的原因。按 §5.10 的顺序判定，第一个成立的生效。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum SkipReason {
    /// 该 repo 在这个会话里 tracked = false：只在 new 时克隆，从不移动（session.md）。
    Untracked,
    /// base.deps 已经是提议的 to，依赖不用换。
    AlreadyThere,
    /// 提议的 to 还没封存：approve 先经 `ensure_target` 封存它；自换时可能还没建，也可能上次没建成（见 `attempts`）。
    NotReady,
    /// 这个会话是另一条 AwaitingPublish 提议的提出者、那条提议的 to 又不是这次的 to：
    /// 它有自己的、与目标不同的依赖改动，换过去会丢掉它。
    OwnChange { proposal: ProposalId },
    /// 会话改了自己的安装输入：工作区算出的哈希既不等于所钉依赖集记录的，也不等于提议的 to_inputs；换过去会与它的改动错位。
    Drift { drift: Drift },
    /// 依赖目录里有进程打开着文件（dev server 等）。跳过并报告，目录一字不动（不变量 4）。
    OpenFiles { files: Vec<OpenFile> },
    /// 判定这个会话时出错，例如 open_files 枚举失败。无法证明安全，就不动它。
    Error { error: StrataError },
}

pub fn plan_rollout(
    ctx: &Ctx,
    id: ProposalId,
    sessions: &[(SessionId, WorkspaceState)],
    complete: bool,
) -> Result<RolloutPlan>;
pub fn plan_switch(ctx: &Ctx, id: ProposalId, session: &SessionId, ws: &WorkspaceState) -> Result<RolloutPlan>;
pub fn record_rollout(store: &Store, plan: &RolloutPlan, moved: &[SessionId]) -> Result<Proposal>;

// ---- 打开的文件（§5.11）----
/// 一个进程对某个目录下文件的持有。types.md 的 `OpenFiles` 要求 detail 列出 pid 和路径。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenFile {
    pub pid: i32,
    /// 进程名，取自 proc_name，给人看的。
    pub command: String,
    /// 被持有的文件或目录的绝对路径，按内核报告的写法。
    pub path: PathBuf,
    pub hold: Hold,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Hold {
    /// 打开的文件描述符。
    Fd,
    /// 映射进内存的文件，例如 dlopen 加载的 `.node` 原生模块，或 mmap。
    Mapped,
    /// 进程的当前目录或根目录在里面。
    Cwd,
}

pub fn open_files(dir: &Path) -> Result<Vec<OpenFile>>;

// ---- 测试注入口 ----
/// 只在测试里打开（module-rulings Addendum 4"Test seams"），从不编进发行版。
/// 按线程生效 [我的决定]：并行跑的测试互不干扰。
#[cfg(feature = "test-seams")]
pub mod test_seams {
    use std::path::Path;
    use strata_types::StrataError;

    /// 当前线程之后的打开文件扫描（`open_files`，以及 `plan_rollout`、`plan_switch` 的批量扫描）
    /// 遇到落在 `prefix` 之下的目录时返回 `error`（failpoint）。
    pub fn fail_open_files_under(prefix: &Path, error: StrataError);
    /// 当前线程每次 `begin_deps_attempt` 之后、往暂存里放东西之前调用 `hook`（hook seam，不是 errno 注入）。
    pub fn after_attempt_begins(hook: Box<dyn FnMut()>);
    /// 当前线程封存时做过的同步：(做了 F_BARRIERFSYNC 的普通文件数, 目录数, F_FULLFSYNC 次数)。
    pub fn sync_counts() -> (u64, u64, u64);
    /// 清掉当前线程的全部注入与计数。
    pub fn reset();
}
```

各函数的语义与可能返回的 `Code`（具名拒绝都发生在第一次对外可见的写入之前，规则见 §6 开头）：

| 函数 | 语义 | 可能的 `Code` |
| --- | --- | --- |
| `inputs_hash` | 按 §4.3 的编码，对 `root` 下 `cfg.install_inputs` 的内容求 64 位小写十六进制哈希。`root` 可以是基线导出、基线 generation 目录、staging、主 checkout 或工作区。条目缺失不报错，按"缺失"编码 | `NotFile`；读失败按 types.md §6.3 归类 |
| `DepsConfigKey::of` | 取 `deps_dirs` 与 `nested_seed_caches`（各按字节序），把 `install` 按 config.md §2.4 切成 argv | `InvalidInput`（`install` 切不开，即 config.md V9） |
| `set_id` | 按 §4.3 计算，纯计算 | 无 |
| `key_of` | `inputs_hash(tree)`，加上 `platform()`、`toolchain` 与 `DepsConfigKey::of` 算出的 id；只读 | 同 `inputs_hash`、`toolchain`、`DepsConfigKey::of` |
| `platform` | 编译期的目标三元组，例如 `aarch64-apple-darwin` | 无 |
| `toolchain` | `<install 的程序名> <它的 --version 输出的第一行>`，没有 `install` 时为空串（§5.2） | `NoSuchPath`（程序找不到）、`InvalidInput`（`install` 切不开）、`Unknown`（`--version` 超时或非零退出） |
| `is_input` | `path` 等于某个 `install_inputs` 条目，或位于某个目录条目之下 | 无 |
| `nested_seed_caches` | 位于某个 `deps_dirs` 条目之内的 `seed_caches` 条目（config.md V7 的唯一例外，如 `node_modules/.cache`），按字节序 | 无 |
| `find_set` | `store.deps_set`；只有已登记的才是依赖集（不变量 D2） | store 的码 |
| `adopt` | 主 checkout 的安装输入与基线导出 `tree` 相同时，把它现成的依赖目录克隆、校验、同步、封存为依赖集（§5.4） | `InvalidInput`、`Drift`、`NoSuchPath`、`NotDirectory`、`CrossVolume`、`IsDirectory`（锁）、`Busy`（repo 写锁）、`Unknown` |
| `build` | 在 staging 里克隆 `tree`、放入安装输入、跑 `install`，校验、同步后封存（§5.5） | `InvalidInput`、`NoSuchPath`、`Drift`（复制期间输入变了）、`CrossVolume`、`IsDirectory`（锁）、`Unknown` |
| `ensure` | 依次尝试：已封存 → 认领 → 冷装；`deps_dirs` 为空时封存空依赖集（§5.6） | 同 `adopt` 与 `build`；`adopt` 的 `Drift`、`NoSuchPath`、`NotDirectory` 不外传，改走冷装 |
| `clone_into` | 把 `set.dirs` 逐项从封存目录克隆到 `dst_root/<dir>`；`dst_root` 必须是调用方新建的目录，不能是活的工作区（§5.7） | `Exists`、`NotDirectory`、`Corrupt`、`CrossVolume`、`Unknown` |
| `attempts` | 列出暂存目录还在的 Running、Interrupted、Failed 尝试 | store 的码 |
| `drift` | `inputs_hash(ws)` 与 `pinned.inputs_hash` 不同时返回 `Some` | 同 `inputs_hash` |
| `propose` | 校验参数后按 §5.9 登记、提升或去重 | `InvalidInput`、`tree_of` 的码原样透传（`NoSuchCommit` 等，§2）、`Busy`（并发改写，重读 3 次仍冲突）、store 的码 |
| `observe_main` | 算出 `export` 的键，记下主干当前的 (tree, head, 安装输入哈希, 依赖集 id)；id 变了就以 origin = Main 调 `propose`。不封存、不冷装 | `InvalidInput`（`tree_of(head) ≠ tree`）、`tree_of` 的码原样透传、同 `key_of`、同 `propose` |
| `get_proposal` | 读一条提议，任何状态都返回 | `NoSuchProposal` |
| `open_proposals` | 列出 Pending，以及提出会话还在的 AwaitingPublish，按 id 排 | store 的码 |
| `ensure_target` | 批准前的第一步（§5.10）：`to` 已封存 → Ready；否则对 `tree_dir` 调 `ensure`，id 等于 `to` → Ready，不等 → 登记取代它的替换提议，返回 Replaced | `NoSuchProposal`、`InvalidInput`、同 `ensure` |
| `plan_rollout` | 对一条 Pending 提议和调用方给的会话算出计划，不写任何东西（§5.10） | `NoSuchProposal`、`InvalidInput`；单个会话的失败记成 `Skip { Error }`，不让整个调用失败 |
| `plan_switch` | 只给提出者用：它可以在确认之前先换到自己的新依赖集（design §2 目标 3、§6.4 第 3 步 [用户]） | `NoSuchProposal`、`InvalidInput` |
| `record_rollout` | 计划完整、且每个 Move 都成功时，把提议记为 Applied；否则不改状态 | store 的码 |
| `open_files` | 列出 `dir` 下被任何同 uid 进程以文件描述符、内存映射或当前目录持有的路径（§5.11）。枚举失败是错误，不会返回空列表 | `Unknown` |

与实施计划 Task 8 的 Produces 相比，有这几处差异，理由见 §9：

- `inputs_hash` 返回 `String`（module-rulings Addendum 1 T8），多了 `cfg` 与 `git` 两个参数。
- `set_id` 是本模块的函数。除了 inputs_hash、platform、toolchain 三段，它还覆盖依赖配置（`DepsConfigKey`，T8 修订轮的裁定）；`key_of` 把这几步合在一起。
- `adopt` 只从 `cfg.path` 认领，所以没有 `from` 参数。
- `build` 自己建 staging，参数是要安装的源码树与可选的安装输入来源。
- `drift` 与 `open_files` 都返回 `Result`。
- 计划里的 `rollout(session, proposal, confirm)` 拆成了 `ensure_target`、`plan_rollout`、`plan_switch`、`record_rollout` 四个函数：移动由 `session::move_workspace` 做，凭据由 daemon 校验，deps 都不碰。
- `Ctx` 带着 daemon 的 repo 写锁（`RepoWriteLock`），因为封存必须持有它，而 install 必须在它外面跑。

## 4. 数据与不变量

### 4.1 目录布局

```text
<root>/deps/                          Paths::deps()（config.md §3）
  <repo>/
    <set-id>/                         封存的依赖集：Store::deps_dir(repo, id)；根目录 mode 0555（D1）
      node_modules/                   DepsSet.dirs 的每一项，按仓库内相对路径排列；mode 照旧（0755）
      public/pdfium/                  例如 tool 的 postinstall 产物（config.md 附录 A）
    .staging/
      <attempt>/                      一次尝试的暂存：Store::deps_staging_dir(repo, attempt)，attempt 是 begin_deps_attempt 返回的序号
        tree/                         冷装：源码树的克隆，install 在这里跑
        seal/                         封存候选：只含依赖目录；封存时整个 rename 成 <set-id>/
        install.log                   冷装：install 的 stdout 与 stderr
    .locks/
      <set-id>.lock                   建造锁：普通文件，flock(LOCK_EX)，fd 带 FD_CLOEXEC
```

- 封存目录与暂存目录的路径都由 store 按键算出（`Store::deps_dir`、`Store::deps_staging_dir`，store.md §4.1）。`.staging/` 与 `.locks/` 是 deps 的私有区：名字以 `.` 开头，解析不成 `DepsSetId`，store 的 gc 从不进入（§4.6）。不再需要的暂存由 deps 经 `Store::retire_dir` 原子地改名进 store 的 trash，gc 在 1 小时之后真正删除 [我的决定]；下文的"拿走暂存"都指这一步。
- 只有已登记（`store.deps_set` 读得到）的 `<set-id>/` 才是依赖集。`.staging/` 下的任何东西，以及没有记录的 `<set-id>/`，都不可发现、不被引用（不变量 D2）。
- 依赖集的键是 `(repo, id)` [我的决定]：`set_id` 不含 repo，两个 repo 的安装输入与依赖配置恰好逐字节相同时，id 会相同。所以目录与记录都按 repo 分开（types.md `DepsSet` 的注释："design §4 的键 … 就是这里的 (repo, id)"）。
- 空依赖集 [我的决定]：`deps_dirs` 为空的仓库（release-notes、四个 Rust 仓库、多数 `@inter/*` 库）也有依赖集，这样 `BaseGen.deps` 永远有值。它的安装输入为空，toolchain 为空串，依赖配置为空，`dirs = []`，封存目录是空的；它不建尝试、不拿建造锁，也没有暂存，在 repo 写锁里直接建出、登记（§5.6）。

### 4.2 持久记录（deps 拥有词汇，store 拥有 schema）

按 module-rulings Addendum 4，表、索引、触发器与全部 SQL 都在 store.md §4.3 的 DDL 里。deps 定下面四类记录的词汇、转换图与含义；store 用自己的行类型镜像它们，用触发器守住转换图；deps 把行映射成本文 §3 的类型。deps 需要的列、状态与唯一性如下。

1. **依赖集**（每个封存的依赖集一条）：`repo`、`id`、`inputs_hash`、`platform`、`toolchain`（没有 install 时为空串）、`dirs`（JSON 数组：封存时实际存在的依赖目录，按字节序）、`origin` ∈ {`adopt`, `build`, `empty`}、`sealed_at`。(repo, id) 唯一，写入之后不改。只经 `record_deps_set` 写入：origin 为 adopt、build 时，它在同一事务里把这次的尝试关成 `sealed`，尝试不是 `running`、不指名同一 (repo, id)、种类对不上，或 (repo, id) 已登记时，什么都不写并报冲突；origin 为 empty 时没有尝试，`dirs` 必须为空。
2. **尝试**（每次认领、冷装一条；它就是这次操作的意图记录）：`id`（store 分配，1..=`i64::MAX`，永不复用，也是暂存目录名）、`repo`、`set_id`（非空：尝试开始之前 id 已算出）、`kind` ∈ {`adopt`, `build`}、`state` ∈ {`running`, `sealed`, `failed`, `refused`}、`staging`（绝对路径）、`pid`（install 的进程组组长，只在 running 时由 `set_deps_attempt_pid` 写一次）、`started_at`、`ended_at`（running 时为空，否则非空）、`error`（`StrataError` 的 JSON；failed 与 refused 时非空）。转换：`running → sealed`（`record_deps_set`），`running → failed | refused`（`end_deps_attempt`，比较并交换）；三种终态不再改。每个 (repo, set_id) 至多一条 `running`：建造锁保证，唯一索引兜底。`deps_attempts` 按 repo（可选）列出全部状态，按 id 递增；按 set_id 筛选由 deps 自己做。
3. **提议**：`id`（`ProposalId`，`record_proposal` 插入时由 store 分配并返回，1..=`i64::MAX`；gc 删掉旧记录之后也不复用，types.md:129）、`repo`、`origin_session`（空表示 origin = Main）、`from_set`、`from_inputs`、`to_set`、`to_inputs`、`tree`、`head`（同为空或同非空）、`state` ∈ {`AwaitingPublish`, `Pending`, `Applied`, `Superseded`}、`superseded_by`（state = Superseded 时非空，指向另一条提议）、`created_at`、`updated_at`。约束：AwaitingPublish 时 tree 为空、origin_session 非空；Pending 与 Applied 时 tree 非空。`from_set`、`to_set` 不要求已登记：`observe_main` 只计算、不封存，`ensure_target` 才封存（§5.10）。唯一性：每个 repo 至多一条 Pending；每个 (repo, origin_session) 至多一条 AwaitingPublish。转换（Addendum 4，store 用触发器守）：AwaitingPublish → Pending（只经 `promote_proposal`；tree 与 head 只在这一步从空变成非空）、AwaitingPublish → Superseded、Pending → Applied、Pending → Superseded。除了 state、superseded_by、updated_at 与那一次 tree/head，任何列写入后不改。
4. **主干**（每个 repo 一条）：`repo`、`tree`、`head`、`inputs_hash`、`set_id`（不要求已登记）、`observed_at`；`set_deps_main` 整条覆盖（upsert），并在同一事务里返回旧记录。

deps 从不删任何记录：记录只由 store 的 gc 删除（module-rulings T6）。关闭的尝试是历史，不是状态（Addendum 4"Error timing, refinement 2"）。提出会话已被删除的 AwaitingPublish 不进 `open_proposals`（store 的 `proposals(repo, open_only = true)` 在读的时候按会话记录过滤），随会话一起被 gc 删除，不另设状态（Addendum 4）。

### 4.3 键与编码

- **`inputs_hash`** [我的决定]：哈希器是 `blake3::Hasher::new_derive_key("git-strata install-inputs v1")`。§5.1 产出一组记录，按路径的 UTF-8 字节序排好，逐条写入：路径的字节长度（u64 小端）、路径字节、一个标签字节，以及 32 字节摘要（缺失的条目没有摘要）。标签与摘要如下：

  | 标签 | 含义 | 摘要 |
  | --- | --- | --- |
  | 0 `Absent` | 条目不存在；或者某个中间级不是目录；或者是没有成员的目录 | 无 |
  | 1 `File` | 普通文件，原始字节 | BLAKE3(内容) |
  | 2 `Symlink` | 符号链接本身，不跟随 | BLAKE3(readlink 的字节) |
  | 3 `Manifest` | 能规范化的 package manifest（§4.4） | BLAKE3(规范化字节) |

  输出 64 位小写十六进制。没有任何安装输入时，哈希器什么都不写，得到一个固定常量（空依赖集用它）。
- **`set_id`** [我的决定]：哈希器是 `blake3::Hasher::new_derive_key("git-strata deps-set-id v2")`。依次写入 inputs_hash、platform、toolchain 三段，每段先写字节长度（u64 小端）再写字节；然后写 `DepsConfigKey` 的三个列表 `deps_dirs`、`nested_seed_caches`、`install`，每个列表先写元素个数（u64 小端），再逐个按"长度 + 字节"写元素。两个路径列表先按字节序排好，argv 保持原顺序。输出 64 位小写十六进制（types.md 要求拼接方式由本文规定，同样的输入必定得到同样的 id）。长度前缀保证 `("ab","c","")` 与 `("a","bc","")` 不会相撞，元素个数保证列表之间的边界不会挪动。
- **依赖配置进入键**：module-rulings T8 修订轮裁定 `deps_dirs` 与 `install` 的 argv 进入键，它们决定装出来、封存进去的是什么。嵌套的种子缓存也进入键 [我的决定]：它决定封存时排除什么；不进键的话，改了 `seed_caches` 之后旧依赖集里还带着 `node_modules/.cache`，session 再克隆种子缓存就会撞上已存在的目录。键里多了无关的东西，代价只是改配置之后每个仓库重新认领或冷装一次；漏掉的代价是同一个 id 下装着按旧配置封存的东西。为此 `set_id` 的编码版本从 v1 升到 v2。types.md 的 `DepsSetId` 注释（"BLAKE3(inputs_hash ∥ platform ∥ toolchain)"）要在 T15 补上这一段。
- **`platform`** [我的决定]：`format!("{}-apple-darwin", std::env::consts::ARCH)`，在 Apple Silicon 上是 `aarch64-apple-darwin`。
- **`toolchain`**：见 §5.2。工具链版本进入键（types.md §4 末段），所以 `bun upgrade` 之后同一棵树对应新的 id。
- **不改编码版本就不能改规则**：§4.3、§4.4 的任何改动都会改变所有 id，必须换 derive_key 的版本串；所有依赖集随后各重新认领或冷装一次。

### 4.4 package manifest 忽略的字段

module-rulings T8 规定，每次发布的版本号变化不得产生新的依赖集。规则如下 [我的决定：哪些文件、哪些字段]：文件名（最后一级）在下表里、内容能解析为顶层是对象的 JSON 时，删掉表里的字段，把对象的键递归地按 UTF-8 字节序排序，紧凑输出（无空白），再对结果求摘要（标签 3）。键序要显式排序，不依赖 serde_json 的 `preserve_order` 特性是否被别的 crate 打开（Cargo 的特性合并会把它悄悄打开）。解析失败、或顶层不是对象时，按原始字节处理（标签 1），不报错。

| 文件名 | 删掉的字段 | 为什么 |
| --- | --- | --- |
| `package.json` | 顶层 `version` | 各仓库的 `build.sh` 每次发布都用 `jq ".version = …"` 改它，且 jq 会重排格式。顶层 `version` 是包自己的版本，不影响装出来的依赖 |
| `composer.json` | 顶层 `version` | 同上（staff-api 目前没有这个字段，规则照样成立） |
| `package-lock.json`、`npm-shrinkwrap.json` | 顶层 `version`；`packages[""].version` | 这两处是根包版本的镜像，`npm version` 会同时改它们。front 被跟踪的 `package-lock.json` 带着它们，而且已经与 `package.json` 不同步（0.1.7 对 0.1.18） |

其它字段一律参与哈希，包括 `scripts`（`postinstall` 会改变装出来的结果）与 `name`。嵌套位置的 `version`，例如 `packages["node_modules/x"].version`、`dependencies.version`，都不删。`bun.lock` 的根工作区条目不带版本，`composer.lock` 也没有根包版本，两者都按原始字节处理。

### 4.5 提议的状态机

```text
   propose(Session{s}, 无目标)              propose(Main 或 Session{s}, 带目标)，或 ensure_target 的替换
            │ record_proposal                                │ record_proposal
            ▼                                                ▼
  ┌──────────────────┐ propose(Session{s}, 带目标, 同一个 to) ┌─────────┐ record_rollout(完整且全部移过) ┌─────────┐
  │ AwaitingPublish  │ ─────────────────────────────────────► │ Pending │ ─────────────────────────────► │ Applied │
  └──────────────────┘       promote_proposal（提升）         └─────────┘        decide_proposal         └─────────┘
            │                                                      │
            │ 同一会话又提了不同的 to（record_proposal 取代它）；  │ 该 repo 登记或提升了另一条 Pending，
            │ 或该 repo 提升了别的、to 相同的草稿                  │ 或批准时的替换提议（record_proposal、promote_proposal）
            │（promote_proposal 取代它）                           │
            ▼                                                      ▼
     Superseded { by }                                      Superseded { by }
```

- 每个 repo 至多一条 Pending；每个 (repo, 提出会话) 至多一条 AwaitingPublish（§4.2）。
- 只有提出会话自己的改动落地时，它的 AwaitingPublish 才被提升：publish 的 `rebase_session` 以 origin = `Session { s }` 带目标调 `propose`（§5.12）[我的决定]。提升在同一事务里取代该 repo 当前的 Pending，以及该 repo 里 to 相同的别的 AwaitingPublish（沿用 ccef28f 的取代范围，store.md 223f9d3 照此实现，§2 对 store 的第 2 条）：那些草稿要的依赖集已经有了一条 Pending，approve 会把它们的会话一起移过去（§5.10 对 to 相同的草稿不判 OwnChange）。origin = Main 的登记不提升、也不取代任何草稿。
- 目标的 tree 与 head 设定之后不再改。主干之后又前进了、而依赖集没变时，Pending 的目标仍是登记时的那个 tree；会话下一次发布时，发布后 rebase 会把它接到链顶。
- 一个会话正在等待一条 Pending 提议 P 的条件是：它 tracked，且 `base.deps ≠ P.to`。status 显示这些会话"在等 deps approve"（module-rulings 公共条 3）。
- 提出会话被删除后，它的 AwaitingPublish 不再列出，也不再让任何会话 OwnChange（它不在任何计划的会话里）；gc 随会话删除它（Addendum 4）。

### 4.6 依赖集的引用与回收（给 store 的 gc）

一个依赖集 (repo, id) 在下列任何一处被引用时都不能回收（design §6.4 第 6 步："旧依赖集保留到没有会话或基线引用它"）：

1. 某个现存会话的 `WorkspaceState.base.deps`；
2. 某个基线 generation（store 记录的 (repo, tree, deps)）的 deps；
3. 某条 Pending 提议的 `to_set`；某条 AwaitingPublish 提议的 `to_set`，前提是它的提出会话的记录还在（gc 删会话记录时一并删掉这条草稿，Addendum 4）。`from_set` 只是给人看的记录，不保活：仍钉着它的会话已经由第 1 条保住了；
4. 主干记录的 `set_id`。

提议与主干记录里的 id 可能还没封存（§4.2），没有对应的目录，这时什么都不用保。不被引用、且超过保留期之后才回收，保留期由 store.md 规定。回收的要求：

- gc 在持有该 repo 写锁时决定并挪走这个 repo 的封存目录（Addendum 4"gc guards"）：先删记录，使它不可发现；再对封存根目录 `chmod u+w`；再 `renamex_np` 进 store 的 trash。封存根目录是 0555，而把目录改名到另一个父目录要求对它本身有写权限（它的 `..` 要改）：没有写权限时 `rename` 返回 `EACCES`（2026-10-22 本机实测）。根目录以下的 mode 本来就可写。
- gc 不进入 `deps/<repo>/` 下以 `.` 开头的条目，也不碰 Running 与 Failed 的尝试的暂存（Addendum 4 gc guards (a)）。封存的 rename、chmod、登记与 gc 的决定都在同一把 repo 写锁里，gc 看不到"目录在、记录还没写"的中间状态；崩溃留下的孤儿（`deps_dir(repo, id)` 在、记录不在，§5.3）按 store 的清扫规则在 1 小时之后挪进 trash（守卫 (b)）。
- deps 自己从不删封存目录，也从不删记录。

### 4.7 不变量

| 编号 | 不变量 | 落实 design §9 |
| --- | --- | --- |
| D1 | 封存的依赖集永不原地修改。deps 对 `deps_dir(repo, id)` 只有一次写：在 repo 写锁之下把它建出来（认领与冷装把暂存里 mode 0755 的 `seal/` rename 过去，空依赖集直接 mkdir），再把根目录 chmod 成 0555，然后登记；之后只读（约定加根目录 0555）。安装结果变了，就是新的 id、新的目录 | 第 5 条 |
| D2 | 暂存不可发现。只有已登记的记录才是依赖集；`.staging/` 下的目录、没有记录的 `<set-id>/` 都不会被 `find_set` 返回，也不会被克隆进任何地方 | 第 5、6 条 |
| D3 | 键是忠实的。登记的 `inputs_hash` 等于封存前在暂存树（冷装）或基线导出（认领）上算出的值，冷装时 install 前后这个值相同；`id = set_id(inputs_hash, platform, toolchain, 依赖配置)`。改写后的不变量 8 读作"工作区的依赖集与工作区当前内容（基线 tree + overlay）的安装输入哈希相符；除了自换过的提出者（design §6.4 第 3 步），这就等于基线 tree 的安装输入"（module-rulings Addendum 1）：D3 保证依赖集的 `inputs_hash` 就是它要匹配的那组输入 | 第 8 条 |
| D4 | 依赖集是自包含的。封存的依赖目录里只有目录、属主可读的普通文件，以及相对目标在仓库布局里经词法解析不出仓库根的符号链接（module-rulings T8 I2 裁定）。这样的链接可以在封存目录里悬空，例如 insurance-check 的 `sidecar/node_modules/cloakbrowser → ../vendor/cloakbrowser`，克隆进工作区后指向工作区自己的那份；绝对链接与出仓库根的链接一律不收。所以依赖集克隆到哪个工作区，都不会经符号链接指回主 checkout 或别的会话 | 第 2 条 |
| D5 | 种子缓存不封存。`nested_seed_caches` 里的路径永远不在封存的依赖集里（design §4："不同步、不 checkpoint、不封存"） | — |
| D6 | 每个 (repo, id) 同一时刻至多一个建造者（flock）。尝试记录在拿到建造锁、算出 id 之后才写，所以同一 (repo, id) 的别的 running 记录一定属于死掉的建造者。锁的顺序固定为建造锁 → repo 写锁，repo 写锁只包住封存，从不包住 install。拿不到建造锁就等；锁打不开就报错，从不无锁建造。空依赖集没有什么可建，只靠 repo 写锁串行化（§5.6） | 第 5 条 |
| D7 | deps 产出的移动只有 `RolloutStep::Move`，它带一整个 `BaseGen` 和匹配的 head：基线与依赖一起移动（不变量 8"两者只会一起移动"）。Move 只出现在三处：Pending 提议的 `plan_rollout`（daemon 在人类确认之后调用，tree 与 deps 都取自那条提议）；提出者对 Pending 提议的 `plan_switch`（同上，只是不需要确认）；提出者对 AwaitingPublish 提议的 `plan_switch`（tree 不动、deps 换成 `to`，前提是工作区当前的安装输入哈希等于 `to_inputs`，即改写后的不变量 8 所说"依赖集与工作区当前内容相符"）。依赖目录上有打开的文件就是 Skip | 第 4、8 条 |
| D8 | 失败的冷装不重放。同一依赖集最近一次冷装尝试失败、而且暂存还在时，deps 不再跑 install，直接返回 `Unknown`（reason previous_attempt）。唯一的例外是持有者死掉、被关成 failed（reason interrupted）的尝试：下一个建造者重来一次；连续两次 interrupted 也拒绝。status 经 `attempts` 报出现场 | 第 6 条 |
| D9 | 对主 checkout 只读。认领只从它往外克隆，只遍历和读安装输入；从不写它的工作树、索引或 refs | 第 2 条 |
| D10 | 提议的状态只前进（§4.5）；目标设定之后不改，`to` 从不改写：批准时封存出别的 id，就登记替换提议（§5.10）；deps 不删记录 | 第 4 条：approve 移到的就是登记时、人看到的那个目标 |

`plan_switch` 有两支（§5.10）。对 AwaitingPublish 提议，提出者换成 `BaseGen { tree: 原 tree, deps: to }`，这是唯一一种不移动 tree 的依赖替换；design §2 目标 3 与 §6.4 第 3 步 [用户] 明确允许提出者先用自己的新依赖。`to` 是用这个工作区自己的安装输入（tree 加上它的 overlay）建出来的，§5.10 也要求工作区当前的哈希等于 `to_inputs`，所以改写后的不变量 8 仍然成立：依赖集与工作区当前内容相符。会话发布这次改动之后，新 tree 的安装输入就是 `to_inputs`，发布后 rebase 不需要换依赖集，基线回到"tree + 匹配的依赖集"的常态。对 Pending 提议（提出者的改动已经发布、草稿已被提升）[我的决定：保留这一支]，提出者换成 `BaseGen { tree: P.tree, deps: P.to }`、head 为 `P.head`：基线与依赖一起移动，与 approve 对别的会话做的完全一样，只是不需要人类确认。

## 5. 流程与算法

记号：【建造锁】是 deps 的建造锁（flock）；【repo 写锁】是 daemon 的 per-repo 写锁，经 `ctx.locks.with_write` 取得；【rename】是原子替换；【同步】是 `F_BARRIERFSYNC` 或 `F_FULLFSYNC`。【checkpoint】不会出现在本模块里：deps 从不写工作区，交换前的 checkpoint 在 `session::move_workspace` 里。

### 5.1 `inputs_hash(cfg, git, root)`

1. 判断 `root` 的种类：`root/.git` 存在（gitfile 或目录）就是 git 工作树（主 checkout 或工作区），目录成员要过 ignore 过滤；否则是导出树（基线导出、基线 generation 目录或 staging），不过滤，因为导出里只有被跟踪的文件。
2. 对每个 `install_inputs` 条目 e，按配置里的写法、用 `apfs::open_rooted` 的规则逐级解析（每一级都带 `O_NOFOLLOW`，不跟随符号链接）：
   - 不存在，或某个中间级不是目录（包括符号链接）：记 `(e, Absent)`。
   - 普通文件：记 `(e, File)`；文件名在 §4.4 的表里、且能规范化时记 `(e, Manifest)`。
   - 符号链接：记 `(e, Symlink)`。从不打开它指向的东西。
   - 目录：用 `apfs::walk` 遍历其下全部条目，不跟随符号链接。普通文件与符号链接成为成员；在 git 工作树里，被 `git.is_ignored(root, 成员)` 判为 ignore 的成员跳过（已跟踪的文件永不被 ignore，这是 gitio 的规则）；子目录本身不记录。成员为零时记 `(e, Absent)`：git 不跟踪空目录，导出树里不存在的目录与工作区里的空目录必须得到同一个哈希。
   - 其它类型（FIFO、socket、设备文件），或者目录里出现了这类成员：返回 `NotFile`，`detail = { "path": <仓库内路径> }`。用 lstat 式的属性判断类型，FIFO 从不被打开。
3. 按 §4.3 的编码求哈希。

[我的决定] config.md §2.3 写的是"目录 = 其下被 git 跟踪的全部文件"；这里取"其下未被 ignore 的全部文件"。在导出树里两者相同，因为导出里全是被跟踪的文件。在工作区里，会话新写、还没 `git add` 的补丁是 `Untracked`，可以发布，`patch-package` 也会应用 `patches/` 下的每一个文件；只数被跟踪的文件会漏掉这种漂移。被 ignore 的杂物（`.DS_Store`）仍然排除。

复杂度：O(安装输入的总字节数 + 目录成员数)。tool 的输入约 1.3 MB（`bun.lock` 357 KB，`patches/` 约 920 KB）。

### 5.2 `toolchain(cfg, env)`

1. `cfg.install` 为 `None` 时返回 `""`。
2. 按 config.md §2.4 把 `install` 切成 argv，切不开返回 `InvalidInput`（V9）。取 argv[0]，按 `env.path` 解析成程序文件；找不到返回 `NoSuchPath`，`detail = { "program": …, "path": … }`。
3. 用解析出的程序文件的身份做缓存键：realpath、inode、大小、mtime。命中缓存就直接返回。缓存是进程内的，daemon 活多久它就活多久。
4. 否则运行 `<程序> --version` [我的决定：采集方式]：环境同 §5.5 第 7 步，时限 10 秒。取 stdout 第一行非空行并去掉首尾空白；stdout 为空时取 stderr 的。结果是 `<程序文件名> <那一行>`，例如 `bun 1.3.3`。非零退出或超时返回 `Unknown`。

### 5.3 建造锁、尝试与封存（`adopt` 与 `build` 共用）

一次建造分七段。第 1 段由调用它的函数做（§5.4–§5.6），后六段共用：

1. **预检**（确定性的，什么都不写）：安装输入在 `tree` 里存在、同卷、工具链、依赖配置，算出 (H, id)（module-rulings T8 I4 裁定）。
2. **【建造锁】** [我的决定：锁的形式]：打开 `.locks/<id>.lock`（`O_CREAT | O_CLOEXEC`），`flock(LOCK_EX)` 阻塞等待。持有者的 install 有时限，所以等待有界。打不开时按 types.md §6.3 归类（例如锁的位置被换成了目录，就是 `EISDIR` → `IsDirectory`），从不在无锁状态下继续。锁 fd 带 `FD_CLOEXEC`，不被 install 继承：daemon 死了锁就释放，残留的 install 只能写进它自己那个不可发现的暂存。
3. **拿到建造锁之后、写任何东西之前**，依次：
   1. `store.deps_set(repo, id)` 已有：释放锁，返回它（别的建造者刚封存了）。
   2. 关掉死掉的尝试：同一 (repo, id) 的每条 `running` 尝试都属于已经死掉的建造者（D6），用 `end_deps_attempt(该尝试, Failed, 错误, 当前时间)` 把它关掉，错误为 `Unknown`、`detail.reason = "interrupted"`（module-rulings T8 I3 裁定）；返回 false 说明它已经不是 running，跳过。这只是给别人的意图记录补上终态，不是对外可见的写入：关闭的意图记录是历史（Addendum 4 refinement 2）。
   3. 孤儿：`deps_dir(repo, id)` 存在而没有记录时，【repo 写锁】之下再查一次：gc 在持有 repo 写锁时删记录、挪目录，锁外可能正好看到删了一半。复查时记录出现了 → 返回它；目录不见了 → 继续；仍是"有目录、没记录" → 释放锁，返回 `Unknown`，`detail = { "reason": "orphan", "path" }`，不动它 [我的决定]。
   4. 只对冷装，D8 [我的决定：interrupted 重来一次]：取同一 (repo, id) 最近的一条 `build` 尝试 A（不算 refused）。A 是 failed、reason 不是 interrupted、暂存还在 → 返回 `Unknown`，`detail = { "reason": "previous_attempt", "attempt", "staging", "error" }`，不跑 install。A 是 failed、reason interrupted、暂存还在：A 之前紧挨着的那条 build 尝试也是 interrupted（A 已经是重来的那一次）→ 同样 previous_attempt；否则继续，重来一次。A 的暂存已被人删掉 → 继续。
4. **尝试**：`begin_deps_attempt(repo, id, kind, 当前时间)`（kind 为 `AttemptKind::Adopt` 或 `Build`）插入 `running` 记录，返回尝试序号 a；暂存目录是 `Store::deps_staging_dir(repo, a)`，即 `.staging/<a>/`。冷装起 install 之后用 `set_deps_attempt_pid` 记下 pid。从这里起（Addendum 3 与 Addendum 4 refinement 2）：
   - install 开始之前的具名拒绝：先拿走暂存（`retire_dir`，这时尝试还是 running），再 `end_deps_attempt(a, Refused, 错误, 当前时间)`，释放建造锁，返回那个码。两步之间崩溃，留下的是一条没有暂存的 running 记录：下一个拿到这把建造锁的调用把它关成 failed（reason interrupted），它的暂存已经不在，D8 不拦。先关记录、后拿暂存的话，崩溃会留下一个 refused 尝试的暂存，gc 不进 `.staging/`，就再没人收走它。
   - install 开始之后的失败：`end_deps_attempt(a, Failed, 错误, 当前时间)`，暂存原样保留，释放建造锁，返回 `Unknown`（module-rulings 公共条：错误时序）。install 在 strata 之外有副作用（`~/.bun/install/cache`、网络），它的产出和日志是只此一份的证据。
5. **校验与同步**：对封存候选 `seal/` 做一次 `apfs::walk`（不跟随符号链接）：
   - 只允许目录、属主可读的普通文件，以及按下面规则判定的符号链接 [我的决定：特殊文件与打不开的文件不收]。FIFO、socket、设备文件是 special_file；打不开的普通文件是 unreadable_file。
   - 符号链接（D4，module-rulings T8 I2 裁定）：取链接在仓库布局里的位置 L，从 L 的父目录出发对目标逐段做词法解析：`.` 跳过，`..` 退一级，其它名字进一级。目标是绝对路径，或某一步退到了仓库根之上 → symlink_escapes。没有退出仓库根的一律允许，指向的东西在封存里不存在也允许（悬空）。例如 `node_modules/.bin/tsc → ../typescript/bin/tsc` 解析为 `node_modules/typescript/bin/tsc`，允许；`sidecar/node_modules/cloakbrowser → ../vendor/cloakbrowser` 解析为 `sidecar/vendor/cloakbrowser`，在封存里悬空，在工作区里指向被跟踪的那个目录，允许；`node_modules/x/y → ../../../z` 在第三步退到仓库根之上，拒绝。
   - 【同步】同一遍里，对每个普通文件和目录以 `O_RDONLY | O_NOFOLLOW` 打开，做 `fcntl(F_BARRIERFSYNC)`：候选的数据与元数据排在之后的 rename 与登记之前落盘 [我的决定，§9]。符号链接不打开，它们的元数据由所在目录的屏障与第 6 段的 `F_FULLFSYNC` 带上。
6. **封存**，【repo 写锁】`ctx.locks.with_write(repo, …)` 里依次做（Addendum 4"Locks"：rename、chmod、登记是一个短临界区）：
   1. 【rename】`renamex_np(.staging/<a>/seal, deps_dir(repo, id), RENAME_EXCL)`。`seal/` 这时是 0755：把目录改名到另一个父目录要求对它本身有写权限，0555 的目录这样改名会得到 `EACCES`（2026-10-22 本机实测；module-rulings T8 I1 裁定）。`EEXIST`：第 3 段查过之后才冒出来的目录只能来自外力，尝试记为 failed、候选留在暂存，返回 `Unknown`（reason orphan）。
   2. `chmod 0555 deps_dir(repo, id)`。只改根目录：依赖目录本身与里面的 mode 不动。clonefile 带着 mode，0555 的 `node_modules` 克隆进工作区后不可写（同日实测）。
   3. 【同步】对 `<root>/deps/<repo>/` 做 `fcntl(F_FULLFSYNC)`：rename 与第 5 段的屏障一起落到介质上（Addendum 3"Durability"）。
   4. `record_deps_set(set, Adopt 或 Build, Some(a), 当前时间)`：登记依赖集，并在同一事务里把尝试 a 关成 `sealed`。

   rename 之后，这几步里任何一步失败都返回 `Unknown`（reason seal_failed）：目录在、记录不在，不可发现（D2）；尽力把 a 关成 failed，关不上的由下一个建造者当成死掉的关掉。rename 之后 deps 不再给具名拒绝。`with_write` 返回 `Busy`（repo 写锁在 daemon 的等待上限内没拿到）时什么都没 rename：认领按 install 之前的具名拒绝处理（拿走暂存、记 refused），返回 `Busy`；冷装已经跑过 install，重试 `with_write` 最多 10 次（缺省等待上限下约 5 分钟）[我的决定]，仍不行就返回 `Unknown`（reason seal_busy），候选留在暂存里。
7. 尽力拿走 `.staging/<a>/` 剩下的部分（冷装的 `tree/` 与 `install.log`；尝试这时已是 sealed，`retire_dir` 收它），拿不走只记日志，不影响已封存的依赖集；释放建造锁；返回 `DepsSet`。

### 5.4 `adopt(ctx, tree)`

`tree` 是基线导出目录。只从 `cfg.path`（主 checkout）认领（design §6.1 第 1 步）。

1. `cfg.deps_dirs` 为空：转 §5.6 第 1 步（空依赖集）。
2. 预检，什么都不写：每个 `install_inputs` 条目都在 `tree` 里存在，否则 `InvalidInput`（config.md §4.3），`detail = { "repo", "field": "install_inputs", "index", "rule": "tracked_in_base", "path" }`；`(H, id) = key_of(ctx, tree)`；`apfs::same_volume(cfg.path, <Paths::deps() 或它最近的已存在上级>)` 为假 → `CrossVolume`。
3. `M = inputs_hash(cfg, git, cfg.path)`。`M ≠ H` 时返回 `Drift`，`detail = { "reason": "inputs", "base_inputs": H, "main_inputs": M }`。
4. 列出主 checkout 里存在的依赖目录：对每个 `d ∈ deps_dirs` 做 lstat。不存在的跳过；存在但不是目录的（包括符号链接）返回 `NotDirectory`；一个都不存在时返回 `NoSuchPath`，表示没有可认领的。
5. §5.3 第 2、3 段：【建造锁】与拿锁后的检查（认领不做 D8：认领不重放 install）。
6. 第一次遍历 W1（拿着建造锁，还没有尝试记录）：`apfs::walk` 每个存在的依赖目录，跳过 `nested_seed_caches`，记下 (路径, 类型, 大小, gen count, mtime, 链接目标)。出现 FIFO、socket 或设备文件 → `Drift`（reason special_file）；符号链接按 §5.3 第 5 段的规则出了仓库根 → `Drift`（reason symlink_escapes）；两者都带 path，释放锁返回。到这一步为止还什么都没写。
7. §5.3 第 4 段：`begin_deps_attempt`，kind 为 `Adopt`。对每个存在的 d，`apfs::clone_tree(cfg.path/d, .staging/<a>/seal/d)`，父目录按需创建；然后删掉 `seal/` 里的 `nested_seed_caches`。
8. 第二次遍历 W2，规则同第 6 步。W1 与 W2 逐条比较（路径集合、类型、大小、gen count、mtime、链接目标）；某条的 gen count 读不到（有可写的 mmap，见 apfs.md 的 gen count 约定）也算不同。有不同 → install 之前的具名拒绝（拿走暂存、记 refused），返回 `Drift`（reason changed_during_clone，带第一个不同的路径）。W1 与 W2 相同，说明克隆期间主 checkout 的依赖目录没有被写过，克隆是一个一致的快照。
9. §5.3 第 5 段的校验与同步。打不开的普通文件 → 具名拒绝，返回 `Drift`（reason unreadable_file）。
10. §5.3 第 6、7 段：封存，origin = adopt，`dirs` 取存在的依赖目录（按字节序）。返回 `DepsSet`。

[我的决定] W1 放在拿到建造锁之后，与 W2 隔得最近，少一些假的 changed_during_clone。第 8、9 步的拒绝拿走自己的暂存：暂存只是主 checkout 的 CoW 克隆，没有独有的数据，也从未可发现，按 Addendum 3 这是第一次对外可见的写入之前的具名拒绝。special_file、symlink_escapes、unreadable_file、changed_during_clone 借用 `Drift` 码，因为它们对调用方的含义与 reason inputs 相同：这次认领不成，改走冷装。`Code` 是封闭的，types.md 的 `Drift` 行要在 T15 补上这些 reason。

复杂度：三次遍历（W1、W2、同步）加一次 `clone_tree`，都是 O(依赖目录的条目数)。认领只在一个依赖集的第一代发生一次。

### 5.5 `build(ctx, tree, inputs_from)`

`tree` 是要安装的源码树：基线导出，或者会话所钉基线 generation 的目录。`inputs_from` 是会话的工作区：会话发起的依赖变更，要从这里取安装输入（design §6.4 第 2 步）。

1. `cfg.deps_dirs` 为空：转 §5.6 第 1 步。
2. 预检，什么都不写：`DepsConfigKey::of(cfg)` 切开 `install`（`InvalidInput`）；`toolchain`（`NoSuchPath`）；输入的来源 src 是 `inputs_from`，没给时是 `tree`，这时每个条目都必须在 `tree` 里存在（`InvalidInput`，rule tracked_in_base）；`H = inputs_hash(cfg, git, src)`，`id = set_id(H, platform(), toolchain, config)`；`apfs::same_volume(tree, <Paths::deps() 或它最近的已存在上级>)` 为假 → `CrossVolume`。
3. §5.3 第 2、3 段：【建造锁】、拿锁后的检查，含 D8。
4. §5.3 第 4 段：`begin_deps_attempt`，kind 为 `Build`，暂存是 `.staging/<a>/`。
5. 准备源码树：`apfs::clone_tree(tree, .staging/<a>/tree)`，记作 S。删掉 S 里的 `deps_dirs` 与 `seed_caches`（它们是自己的克隆，可以删）。给了 `inputs_from` 时，对每个安装输入条目 e：先删掉 S/e，再按 §5.1 对 `inputs_from` 的解析结果，把文件、符号链接（链接本身）和目录成员用 `apfs::clone_file` 放进 S，缺失的保持缺失。往 S 里删和放都走 rooted、no-follow 的目标操作（Addendum 3）：S 是基线树的克隆，被跟踪的符号链接可能出现在中间级，写入不能经它们落到 S 之外。这一步自己出错（例如 `ENOSPC`）：install 之前的具名拒绝（拿走暂存、记 refused），按 types.md §6.3 归类返回。
6. `H' = inputs_hash(cfg, git, S)`。S 没有 `.git`，所以不过滤。`H' ≠ H` 说明复制期间会话又改了安装输入（或者基线目录被改过）→ install 之前的具名拒绝，返回 `Drift`，`detail = { "reason": "inputs_changed", "expected": H, "found": H' }`。键因此永远是真正装进去的那组输入（D3）。
7. 运行 install。从这一步起，失败一律 `Unknown`、现场保留（§5.3 第 4 段）：
   - argv 来自第 2 步，cwd 是 S，stdin 是 `/dev/null`。
   - 环境取 daemon 的环境，删掉全部 `GIT_*`，PATH 换成 `env.path` [我的决定]。
   - 用 `setpgid` 自成一个进程组；stdout 与 stderr 都写进 `.staging/<a>/install.log`；用 `set_deps_attempt_pid` 把 pid 写进尝试记录。
   - 到 `env.timeout` 时，先 `killpg(SIGTERM)`，10 秒后再 `killpg(SIGKILL)`。
8. 非零退出、启动失败或超时：返回 `Unknown`，`detail = { "reason": "install_failed" 或 "timeout", "attempt", "staging", "command", "exit_code", "log_tail" }`，其中 `log_tail` 是日志最后 8 KiB。
9. 重算 `inputs_hash(cfg, git, S)`，它必须仍等于 H；不等返回 `Unknown`（reason inputs_modified）。这说明 install 改写了安装输入，比如漏了 `--frozen-lockfile`，装出来的东西不属于这个键。
10. 组装封存候选 `seal/`：对每个 `d ∈ deps_dirs`，逐级 `O_NOFOLLOW` 解析 S/d；存在就 rename 进 `seal/d`（父目录按需创建）。S/d 不是目录，或某个中间级是符号链接 → `Unknown`（reason not_directory）；一个依赖目录都没有产出 → `Unknown`（reason no_deps_dirs）。然后删掉 `seal/` 里的 `nested_seed_caches`。
11. §5.3 第 5 段的校验与同步。不合规 → `Unknown`（reason special_file、unreadable_file 或 symlink_escapes）。
12. §5.3 第 6、7 段：封存，origin = build，`dirs` 取产出的依赖目录。返回 `DepsSet`。

复杂度：时间主要花在 install 上（tool 的 `bun install` 要几分钟）。deps 自己的开销是一次 `clone_tree`、三次 `inputs_hash`、一次校验加同步的遍历。

### 5.6 `ensure(ctx, tree)`

1. `cfg.deps_dirs` 为空时：`id = set_id(空输入常量, platform(), "", 空配置)`。`find_set` 命中就返回。否则在【repo 写锁】`ctx.locks.with_write(repo, …)` 里 [我的决定]：再查一次 `deps_set`，有了就返回它；`deps_dir(repo, id)` 已存在而没有记录 → `Unknown`（reason orphan），不动它；否则 `mkdir`（0755，父目录按需创建）→ `chmod 0555` → 对 `<root>/deps/<repo>/` 做 `F_FULLFSYNC` → `record_deps_set(set, Empty, None, 当前时间)`。没有建造锁、尝试和暂存，也不跑任何命令：并发的调用由 repo 写锁串行化，后到的那个在锁里看到记录。`with_write` 返回 `Busy` 时什么都没写，返回 `Busy`；`mkdir` 之后的失败返回 `Unknown`（reason seal_failed），空目录在、记录不在，同 §5.3 第 6 段。
2. 预检：每个条目都在 `tree` 里存在，否则 `InvalidInput`；`(H, id) = key_of(ctx, tree)`。
3. `find_set` 命中就返回。这是热路径，不拿锁。
4. 调 `adopt(ctx, tree)`。成功就返回。遇到 `Drift { reason: changed_during_clone }` 时再认领，连同第一次最多 3 次 [我的决定]；3 次都是它，就转第 5 步冷装。遇到其它 `Drift`、`NoSuchPath` 或 `NotDirectory` 时转第 5 步。其它错误原样返回。
5. 调 `build(ctx, tree, None)` 并返回它的结果。

并发：两个 `ensure` 同时为同一个 id 走到第 4 或第 5 步时，由建造锁串行化；后到的那个拿到锁后，在 §5.3 第 3 段看到已封存的依赖集，直接返回它，从不开始自己的尝试。

### 5.7 `clone_into(store, set, dst_root)`

1. `src = store.deps_dir(set.repo, set.id)`。`src` 不存在，或某个 `src/d` 不存在：返回 `Corrupt`（记录在而封存目录坏了；types.md 的 `Corrupt` 原指 store 数据损坏，这里把它扩展到 deps 的封存区，T15 补进表里）。
2. 先检查全部 `dst_root/d`，逐级 `O_NOFOLLOW` 解析：任何一个已存在 → `Exists`；某个中间级是符号链接或不是目录 → `NotDirectory`。两种都什么都不写。
3. 对每个 `d ∈ set.dirs` 依次执行 `apfs::clone_tree(src/d, dst_root/d)`（rooted 目标），父目录按需创建。克隆出来的目录 mode 保持 0755，归调用方所有、可写。第一次克隆之后的任何失败都返回 `Unknown`，已克隆的部分保留（types.md §6.3 末段）。

`clone_into` 只往调用方新建的目录里写 [我的决定：克隆归 deps]：session 建基线 generation 时的私有目录（§5.12），从不往活的工作区里写。调用方在持有该 repo 写锁、并用 `find_set` 复查过之后调用它（§2 对 session 的要求第 3 条），所以 gc 不会在克隆途中挪走源目录。种子缓存由 session 在这之后另行从主 checkout 克隆（`nested_seed_caches` 告诉它是哪些）。

### 5.8 `drift(cfg, git, ws, pinned)`

`H = inputs_hash(cfg, git, ws)`。`H == pinned.inputs_hash` 时返回 `None`，否则返回 `Some(Drift { pinned: pinned.id, pinned_inputs: pinned.inputs_hash, found_inputs: H })`。

- 只比安装输入，不比依赖目录的内容（design §3），所以 dev-link 写 `node_modules/@inter/*`、`bun` 往 `.bin` 加东西都不算漂移。也不比 toolchain、platform 与依赖配置：它们变了，主干的依赖集 id 就变，`observe_main` 会登记一条 from_inputs 等于 to_inputs 的提议。
- 对主干 tip 的导出目录调用（没有 `.git`），就是"会话的依赖集与主干当前的依赖集比较"（module-rulings T8）。主干记录里存着它的 `inputs_hash`，status 也可以直接拿它与所钉依赖集的 `inputs_hash` 比，不必遍历。
- 调用时机由调用方决定：status 时调用，或者 daemon 在 checkpoint 之后、overlay 里有路径让 `is_input` 为真时调用。

### 5.9 `propose(ctx, args)` 与 `observe_main(ctx, tree, head, export)`

`propose`：

1. 校验：`to ≠ from`；`tree` 与 `head` 同为 Some 或同为 None；同为 None 时 origin 必须是 Session；同为 Some 时 `git.tree_of(head)` 必须等于 `tree`。前几项不满足返回 `InvalidInput`；`tree_of` 自己的码（head 不在 object store 里的 `NoSuchCommit` 等）原样透传。
2. 用 `proposals(repo, open_only = true)` 读出该 repo 当前的 Pending C 与各会话的 AwaitingPublish，按参数分三种情况决定写什么。每次写都是 store 的一个写事务，带比较并交换（见下）：读到的状态已经变了（别的调用先写了），store 什么都不写，`record_proposal` 返回 `None`、`promote_proposal` 返回 false；deps 重新读、重新决定，最多 3 次，仍冲突就返回 `Busy`（`detail.reason = "proposal_race"`）[我的决定]。
   - **会话的草稿**（origin = `Session { s }`，没有目标）：s 在这个 repo 已有 AwaitingPublish D，且 `D.to == to` → 返回 D（幂等）。否则 `record_proposal(新草稿, replaces = D 的 id 或 None, 当前时间)`：插入 AwaitingPublish，取代 D（如果有）。
   - **会话的改动已落地**（origin = `Session { s }`，带目标；publish 的 `rebase_session` 调用）：s 有 AwaitingPublish D 且 `D.to == to` → `promote_proposal(D, tree, head, 当前时间)`：D 变成 Pending，填上目标，origin 保持 `Session { s }`，取代 C，以及该 repo 里 to 相同的别的 AwaitingPublish（§4.5）。否则 C 的 (to, tree) 与这次相同 → 返回 C（幂等：已经提升过的 D 走到这里）。否则 `record_proposal(新 Pending, replaces = C 的 id 或 None, 当前时间)`，origin `Session { s }`。先看自己的草稿 [我的决定]：C 即使恰好是同一个目标，也让 D 取代它；否则 D 作为一条没人提升的草稿留下来，到 s 下一次发布时才被提升成一条大家都已用上的 Pending。
   - **主干**（origin = `Main`，带目标；`observe_main` 与 `rebase_session` 调用）：C 的 (to, tree) 与这次相同 → 返回 C。否则 `record_proposal(新 Pending, replaces = C 的 id 或 None, 当前时间)`，origin Main。不提升、也不取代任何 AwaitingPublish。
3. 返回提议。`ProposalId` 由 store 在 `record_proposal` 插入时分配并返回（types.md:129），deps 从不自己取号。

比较并交换的语义（store.md §5.6 照此实现）：`record_proposal` 的 `replaces` 是 deps 读到的、新记录要占的那个位置上现有的那一条（插 Pending 时是该 repo 的 Pending，插 AwaitingPublish 时是同一 (repo, 会话) 的 AwaitingPublish，位置空着时为 `None`）。位置上已经换了别的，store 什么都不写、返回 `None`；否则在同一事务里把它记为 `Superseded { by: 新 id }` 再插入，返回 `Some(新 id)`。`promote_proposal` 要求 D 仍是 AwaitingPublish（否则返回 false），并在同一事务里取代该 repo 当时的 Pending，以及该 repo 里 to 与 D 相同的别的 AwaitingPublish；它对当时是哪一条 Pending 不做比较，后写者胜（§2 对 store 的第 2 条）。`decide_proposal` 要求提议仍是 Pending（否则返回 false）。

`propose` 不建依赖集：`to` 可以还没封存，这时 `plan_rollout` 把会话标成 NotReady，批准时 `ensure_target` 封存它。

`observe_main`：调用方传入新会话此刻会钉住的那个 tip，即 session.md 按 design §6.7 和 `pin_unpushed` 算出的 (tree, head)，以及这个 tree 的导出或基线目录 `export` [我的决定：由调用方给目录]。它只计算，从不封存、从不冷装，所以 publish 里调它不会跑几分钟的 install（module-rulings T8 修订轮）。

1. `git.tree_of(head)` 必须等于 `tree`，否则 `InvalidInput`；`tree_of` 自己的码原样透传。
2. `(H, id) = key_of(ctx, export)`。
3. 用 `set_deps_main(&DepsMain { repo, tree, head, inputs_hash: H, set: id, observed: 当前时间 })` 写入，取回它在同一事务里读到的旧记录：并发的两次观察各自拿到的都是自己写入之前的那一条，不会两次都拿同一条旧记录去比。
4. 没有旧记录（第一次观察），或者旧记录的 set_id 等于 id：返回 `None`。
5. 否则以 `ProposalOrigin::Main` 调 `propose`：from 取旧记录的 (set_id, inputs_hash)，to 取 (id, H)，目标是 (tree, head)。返回这条提议，它可能是新登记的，也可能是已有的同一条。

### 5.10 `ensure_target`、`plan_rollout`、`plan_switch` 与 `record_rollout`

`ensure_target(ctx, id, tree_dir)`：`deps approve` 在人类确认之后、拿 repo 写锁之前调用（§5.12）。`tree_dir` 是 `P.tree` 的导出或基线目录，由调用方准备。

1. `P = get_proposal(id)`。不是 Pending → `NoSuchProposal`，`detail = { "proposal", "state" }`；`P.repo ≠ ctx.repo` → `InvalidInput`。
2. `find_set(P.repo, P.to)` 命中 → `Ready { proposal: P }`。
3. `set = ensure(ctx, tree_dir)`（可能要冷装几分钟，在 repo 写锁之外）。`set.id == P.to` → `Ready`。
4. 否则（提议之后工具链、依赖配置或编码版本变了）：`record_proposal(替换提议, replaces = Some(P.id), 当前时间)`。替换提议是 Pending，origin、from、from_inputs、tree、head 取 P 的 [我的决定]，to = set.id，to_inputs = set.inputs_hash。返回 `None`（P 在这期间已被取代）→ `NoSuchProposal`，不登记替换。返回 `Some(Q)` → `Replaced { superseded: P.id, replacement: Q }`。这次 approve 不移动任何会话；人看到替换提议之后再批准它（module-rulings T8 I5 裁定；D10）。

`plan_rollout(ctx, id, sessions, complete)` 只读，什么都不写：

1. `P = get_proposal(id)`。`P.state` 不是 Pending 时返回 `NoSuchProposal`，`detail = { "proposal", "state" }`。`P.repo` 不等于 `ctx.repo`，或某个会话的 `base.repo` 不等于 `P.repo`，返回 `InvalidInput`。
2. 目标是 `BaseGen { repo: P.repo, tree: P.tree, deps: P.to }`，head 是 `P.head`。`ready = find_set(P.repo, P.to).is_some()`。用 `proposals(repo, open_only = true)` 读出该 repo 未关闭的 AwaitingPublish 提议（提出会话已被删除的不在其中）。
3. 对 `sessions` 里的每个 (s, ws)，按顺序判定，第一个成立的条件决定这一步：
   1. `!ws.tracked` → `Skip Untracked`
   2. `ws.base.deps == P.to` → `Skip AlreadyThere`
   3. `!ready` → `Skip NotReady`
   4. s 是另一条 AwaitingPublish 提议 Q 的提出者，且 `Q.to ≠ P.to` → `Skip OwnChange { proposal: Q.id }`。`Q.to == P.to` 的会话往下判定 [我的决定]：它自己的改动要的正是这次的目标，第 5 小步会让它移动（origin = Main 的 Pending 不取代任何草稿，§4.5）
   5. 先 `pinned = find_set(ws.base.deps)`：缺失时记 `Skip Error { Corrupt }`。再算 `found = inputs_hash(cfg, git, ws.path)`：出错记 `Skip Error`；`found` 既不等于 `pinned.inputs_hash`、也不等于 `P.to_inputs` → `Skip Drift { drift }`。`found == P.to_inputs` 的会话照常移动 [我的决定]：它自己的安装输入改动就是目标，移过去之后依赖集与工作区内容相符；提出者的草稿被提升之后，正是靠这一条接到自己的提议
   6. 查 `ws.path` 下 `cfg.deps_dirs`、`pinned.dirs`、`to` 的 dirs 三者并集里每个存在的目录上打开的文件。枚举出错记 `Skip Error`；有打开的文件 → `Skip OpenFiles { files }`
   7. 否则 → `Move { session: s, new_base: 目标, head: P.head }`
4. 第 3 步第 6 小步对所有会话只做一次进程扫描（§5.11 的批量形式），按目录前缀分给各会话。
5. 返回 `RolloutPlan { proposal: id, repo, complete, steps }`。`complete` 由调用方给出：没有 `--sessions` 收窄、`sessions` 就是该 repo 的全部会话时，取 true。

`plan_switch(ctx, id, s, ws)` 只给提出者用，不需要人类确认（§4.7 末段；design §2 目标 3 [用户]）：

1. `P = get_proposal(id)`。`P.origin` 不是 `Session { s }`，或 P 的状态既不是 AwaitingPublish 也不是 Pending：返回 `NoSuchProposal`，`detail = { "proposal", "state", "reason": "not_origin" 或 "not_open" }`。从这个会话的角度看，它手里没有这样一条可以自换的提议，所以借用这个码。
2. 目标：AwaitingPublish 时是 `BaseGen { tree: ws.base.tree, deps: P.to }`，head 用 `ws.head`（tree 不动）；Pending 时是 `BaseGen { tree: P.tree, deps: P.to }`，head 用 `P.head`（基线与依赖一起移动）。
3. 按顺序判定：tracked、AlreadyThere（`ws.base` 已等于目标）、NotReady；然后要求 `inputs_hash(ws) == P.to_inputs`，否则 `Skip Drift { pinned: P.to, pinned_inputs: P.to_inputs, found_inputs: 实际值 }`，说明工作区已经不是这个依赖集当初对应的那组输入；再查打开的文件；都通过就 Move。
4. 返回一步的计划，`complete = false`。

这不给 agent 任何新的权力：会话本来就拥有自己依赖目录的克隆，可以直接在里面 `bun install`。`plan_switch` 只是让它换成封存好、别人也会用的那一份，并且同样不在 dev server 脚下换。

`record_rollout(store, plan, moved)`：

1. `P = get_proposal(plan.proposal)`。`P.state` 不是 Pending（自换时的 AwaitingPublish，或者在计划与记录之间被取代了）：原样返回 P。
2. 同时满足三个条件时，用 `decide_proposal(P.id, ProposalDecision::Applied, 当前时间)` 把 P 从 Pending 记为 Applied：`plan.complete` 为真；每个 Move 的会话都在 `moved` 里；每个 Skip 的原因都只是 Untracked 或 AlreadyThere。否则不改。返回 false（P 刚被取代）时同样不改。
3. 返回 P 的当前状态。

### 5.11 `open_files(dir)`

1. 用 `O_RDONLY | O_DIRECTORY | O_NOFOLLOW` 打开 `dir`。`ENOENT`、`ENOTDIR`、`ELOOP` 时返回空列表：不存在的目录不会被交换；符号链接被交换时，它指向的东西不受影响。打开成功后，用 `fcntl(F_GETPATH)` 取内核给出的规范路径 C，然后关闭。
2. 用 `proc_listallpids` 列出全部 pid；缓冲区满时加倍重试。
3. 对每个 pid：`ESRCH`（进程已退出）和 `EPERM`（别的 uid 的进程，看不到）跳过；其它错误整个调用返回 `Unknown`，`detail` 带 errno。
   - `proc_pidinfo(PROC_PIDVNODEPATHINFO)` 给出 cwd 与根目录，在 C 下的记 `Cwd`。
   - `proc_pidinfo(PROC_PIDLISTFDS)` 列出 fd；对每个 vnode 类型的 fd，`proc_pidfdinfo(PROC_PIDFDVNODEPATHINFO)` 给出路径，在 C 下的记 `Fd`。
   - 从地址 0 开始，逐段调用 `proc_pidinfo(PROC_PIDREGIONPATHINFO, 地址)`，带 vnode 路径、且路径在 C 下的段记 `Mapped`。dlopen 加载的 `.node` 原生模块不占 fd，只能这样看到。
4. "在 C 下"的定义：路径等于 C，或者以 `C + "/"` 开头，按字节比较。两边都来自内核，大小写一致。`node_modules2/x` 不算在 `node_modules` 下。
5. 按 (pid, path, hold) 去重，按 (pid, path) 排序，用 `proc_name` 填 command。

[我的决定] 用 libproc 扫 fd、映射段与 cwd；枚举失败返回错误，不返回空列表。局限：别的 uid 的进程看不到；dev server 都以用户身份运行，满足需要。`plan_rollout` 用的批量形式扫描一次进程，同时对多个目录前缀做匹配。

### 5.12 调用方的顺序（契约）

- **`deps approve <id> [--sessions …]`**，由 daemon 执行：
  1. `get_proposal`：不是 Pending 就返回 `NoSuchProposal`，不弹认证框。
  2. 人类确认：cli 先 `require_tty`；daemon 推出 `Action::DepsApprove { proposal: id }`，经 confirm 签发与校验后，再核对 action。
  3. 在 repo 写锁之外：`find_set(P.to)` 没有时，经 session 与 gitio 备好 `P.tree` 的导出，再调 `ensure_target`（已封存时它立刻返回 Ready）。冷装要几分钟，不能让同一 repo 的 checkpoint 因此 `Busy`。返回 `Replaced` 时什么都不移动，把替换提议的 id 报给人；批准它是一次新的确认，因为凭据的 action 指名的是 P。
  4. 拿 repo 写锁，再 `get_proposal` 一次：认证或冷装期间它可能已被取代，这时返回 `NoSuchProposal`。
  5. `plan_rollout`。
  6. 对每个 Move 调 `session::move_workspace(session, <RepoId>, new_base, head)`；它先 checkpoint，再在旁边准备，最后 rename 交换，交换前复查打开的文件。
  7. `record_rollout(plan, 成功移过的会话)`。
  8. 返回每个会话的结果。Skip 的 `kind` 与某个 `Code` 同名时（`OpenFiles`、`Drift`），它就是该条目在输出里的码（types.md §7："多会话 deps approve 里某个会话的 OpenFiles … 出现在 data 里对应的条目上"）；形状由 cli.md 规定。
- **`deps propose`**（由会话发起），由 daemon 执行：
  1. 先算 `drift`：没有漂移就不必提议。
  2. 有漂移就调 `build(ctx, 会话所钉基线 generation 的目录, Some(工作区))`，在 repo 写锁之外做。
  3. `propose(Session { s }, from: base.deps, to: 建好的 id, 无目标)`。
  4. 提出者想先用新依赖时调 `plan_switch`，再 `move_workspace`。cli 怎样暴露这一步由 cli.md 决定。
- **发布后 rebase**，即 publish 的 `rebase_session`，对每个参与的会话 s，新基线是 (T′, H′) [我的决定：这份契约]：
  1. `(N_inputs, N) = key_of(ctx, T′ 的导出或基线目录)`。
  2. s 是一条 AwaitingPublish D 的提出者、且 `D.to == N`：它的依赖改动落地了，调 `propose(Session { s }, to = N, 目标 = (T′, H′))` 把 D 提升为 Pending。
  3. `N == s.base.deps` → 直接把 s 移到 (T′, N)（自换过的提出者就属于这种）。
  4. 否则 s 原地等待，调 `propose(Main, from = s.base.deps, to = N, 目标 = (T′, H′))`；第 2 步已经提升了同一个 (to, tree) 时，它幂等地返回那一条。
  5. 新会话会钉的 tip 前进了，再调 `observe_main`。
- **`session::new`**：对每个兄弟仓库，钉住 tip，把 tree 导出到自己新建的目录，`ensure`（不持有锁），然后持 repo 写锁 `find_set` 复查、`clone_into`、改名并登记基线 generation，最后 `observe_main(ctx, tree, head, 基线目录)`。种子缓存（包括 `nested_seed_caches`）之后再从主 checkout 克隆进工作区。
- **status 与 stderr 提示**：用 `open_proposals`，每次 CLI 调用的 stderr 只提示 Pending 的提议（design §6.4 第 4 步）；用 `attempts` 报出现场（不变量 6）；用 `drift` 报出各工作区的漂移；等待中的会话按 §4.5 的规则显示。

### 5.13 复杂度与验收数字

| 操作 | 复杂度 | 目标（首个 spike 实测后修正，同 design §8） |
| --- | --- | --- |
| `inputs_hash`、`drift`、`key_of` | O(输入字节 + 目录成员) | tool 小于 20 ms |
| `ensure` 热路径（依赖集已封存） | 一次 `inputs_hash`、一次缓存命中的 `toolchain`、一次 store 读 | 每个 repo 小于 50 ms，这样 design §8 的"新建会话 < 5 s（tool + 全部兄弟库）"才能成立 |
| `adopt` | 三次遍历（W1、W2、同步）加一次 `clone_tree`，O(依赖目录条目数) | 不超过同一目录 `cp -Rc` 用时的 1.5 倍加三次遍历的用时；只在第一代发生一次。同步遍历在本机内置 SSD 上约 18 µs 一个文件（5000 个克隆文件 0.09 s，2026-10-22），tool 的 `node_modules` 约 10.9 万个文件，约 2 s；工作区所在的 USB SSD 待实测 |
| `build` | install 用时，加上 O(条目数) | 由 install 决定；deps 自己的开销小于同一目录 `cp -Rc` 用时的 2 倍加一次同步遍历 |
| 封存的临界区 | 一次 rename、一次 chmod、一次 `F_FULLFSYNC`、一个 store 写事务 | 小于 50 ms：repo 写锁只被它占这么久 |
| `open_files` | O(进程数 + fd 数 + 映射段数)，扫描一次 | 本机约 800 个进程时小于 1 s |
| `plan_rollout` | O(会话数 × `inputs_hash`) 加一次进程扫描 | 10 个会话小于 2 s。design §8 的"依赖 rollout = 克隆 + rename，秒级"由 `move_workspace` 兑现 |
| `ensure_target` | `to` 已封存时一次 store 读；否则同 `ensure` | — |
| `propose`、`observe_main`、`record_rollout` | 每次一个 store 写事务（冲突时最多再读写 3 次）；`observe_main` 另加一次 `key_of` | 小于 10 ms，`observe_main` 另加 `key_of` 的用时 |

## 6. 失败与恢复

错误时序（module-rulings 公共条、Addendum 3"Error timing, refined"、Addendum 4 refinement 2；design §9 不变量 6）。deps 对外可见的写入只有三种：

1. 依赖集记录（`record_deps_set`），封存的最后一步。它之前的 rename 把目录放到 `deps_dir(repo, id)`；没有记录，那个目录不可发现（D2），但 rename 是不回头的一步：rename 之后 deps 不再给具名拒绝。
2. 提议与主干记录（`record_proposal`、`promote_proposal`、`decide_proposal`、`set_deps_main`）：每次一个事务，要么写成，要么什么都没写。
3. `clone_into` 写进调用方目录的克隆。

暂存目录、尝试记录（deps 自己的意图记录）、锁文件、没有记录的 `deps_dir(repo, id)` 都是内部的，从不可发现。具名拒绝只发生在两处：预检（什么都没写），以及冷装 install 开始之前（经 `retire_dir` 拿走自己的暂存，再把尝试记为 refused）。冷装的 install 一旦开始，此后的失败一律是 `Unknown`、尝试记为 failed、现场保留：install 在外面有副作用，日志又是只此一份的证据。

| 失败 | 在哪一步 | Code | 留下的现场 | 恢复 |
| --- | --- | --- | --- | --- |
| 安装输入是 FIFO、socket 或设备文件 | `inputs_hash` | `NotFile` | 无写入 | 人修正 |
| 安装输入在基线 tree 里不存在 | 预检（`ensure`、`adopt`、`build`） | `InvalidInput`（rule tracked_in_base） | 无写入：没有锁文件、尝试记录、暂存 | 改配置，或提交那个文件 |
| `install` 切不开，或程序在 PATH 里找不到 | 预检（`DepsConfigKey::of`、`toolchain`） | `InvalidInput` 或 `NoSuchPath` | 无写入 | 修配置或 daemon 的 PATH |
| 跨卷 | 预检（`apfs::same_volume`）；`apfs::clone_tree` 兜底 | `CrossVolume` | 无写入 | 人把 `paths.root` 放到同一卷 |
| 主 checkout 的安装输入与基线不符 | `adopt` 第 3 步 | `Drift`（reason inputs） | 无写入 | `ensure` 改走冷装 |
| 主 checkout 没有依赖目录，或依赖目录不是目录 | `adopt` 第 4 步 | `NoSuchPath`、`NotDirectory` | 无写入 | `ensure` 改走冷装 |
| 主 checkout 的依赖目录里有特殊文件或出仓库根的符号链接 | `adopt` 第 6 步（W1） | `Drift`（reason special_file、symlink_escapes） | 只有锁文件 | `ensure` 改走冷装 |
| 克隆期间主 checkout 的依赖目录被写 | `adopt` 第 8 步 | `Drift`（reason changed_during_clone） | 暂存已拿走，尝试为 refused | `ensure` 最多再认领两次，仍不行就冷装 |
| 认领的候选里有打不开的普通文件 | `adopt` 第 9 步 | `Drift`（reason unreadable_file） | 暂存已拿走，尝试为 refused | `ensure` 改走冷装 |
| 建造锁打不开 | §5.3 第 2 段 | 按 §6.3 归类（如 `IsDirectory`） | 无写入 | 人修锁的位置 |
| 上一个建造者死了（daemon 或建造进程被杀） | 下一个拿到建造锁的调用，§5.3 第 3 段 | 不报错：它的尝试被关成 failed（reason interrupted），冷装重来一次 | 死掉那次的暂存保留，status 报出；残留的 install 只能写自己的暂存 | 人看过之后可以删掉那个暂存 |
| 连续两次 interrupted，或上次冷装失败的现场还在 | §5.3 第 3 段（D8） | `Unknown`（reason previous_attempt） | 原现场不动；本次没跑 install | 人看现场，删掉那个暂存目录之后重来 |
| 有封存目录、没有记录（孤儿） | §5.3 第 3 段（repo 写锁下复查过），或封存时 rename 遇到 `EEXIST` | `Unknown`（reason orphan） | 不动 | 人检查后删除（先 `chmod u+w`）；或者等 gc 在 1 小时之后把它挪进 trash（§4.6） |
| 复制期间会话又改了安装输入 | `build` 第 6 步 | `Drift`（reason inputs_changed） | 暂存已拿走，尝试为 refused | 调用方重来 |
| 准备暂存时 I/O 失败 | `build` 第 5 步 | 按 §6.3 归类（如 `Unknown`） | 暂存已拿走，尝试为 refused | 人腾出空间后重来 |
| install 非零退出，或启动失败 | `build` 第 8 步 | `Unknown`（reason install_failed） | `tree/` 与 `install.log` 保留，尝试为 failed | 人看日志，删暂存后重来 |
| install 超时 | `build` 第 7–8 步 | `Unknown`（reason timeout） | 进程组被杀，暂存保留 | 同上 |
| install 改写了安装输入 | `build` 第 9 步 | `Unknown`（reason inputs_modified） | 保留 | 改 `install`（例如加 `--frozen-lockfile`） |
| install 没产出依赖目录，或产出的不是目录 | `build` 第 10 步 | `Unknown`（reason no_deps_dirs、not_directory） | 保留 | 改 `deps_dirs` 或 `install` |
| 冷装的候选里有特殊文件、打不开的文件或出仓库根的符号链接 | `build` 第 11 步 | `Unknown`（reason special_file、unreadable_file、symlink_escapes） | 保留 | 人看 |
| repo 写锁拿不到 | §5.3 第 6 段；§5.6 第 1 步 | 认领、空依赖集：`Busy`；冷装重试 10 次之后：`Unknown`（reason seal_busy） | 认领：暂存已拿走，尝试为 refused；空依赖集：什么都没写；冷装：候选留在暂存，尝试为 failed | 稍后重来；冷装的现场同"上次失败" |
| rename（空依赖集是 mkdir）之后 chmod、同步或登记失败，或 daemon 在其间崩溃 | §5.3 第 6 段；§5.6 第 1 步 | `Unknown`（reason seal_failed），或下一次被当成孤儿 | 封存目录在、记录不在，不可发现 | 同"孤儿" |
| 磁盘满或其它 I/O 错误（install 开始之后） | install、组装、封存 | `Unknown`（types.md §6.3） | 保留 | 人腾出空间，删暂存后重来 |
| 记录在，封存目录却不在 | `clone_into` 第 1 步 | `Corrupt` | 不动 | 人检查：封存区被外力改过 |
| `clone_into` 的目标已存在，或中间级是符号链接 | `clone_into` 第 2 步 | `Exists`、`NotDirectory` | 无写入 | 调用方换一个目标 |
| `clone_into` 写到一半失败 | `clone_into` 第 3 步 | `Unknown` | 已克隆的部分保留 | 调用方按自己的现场规则报出 |
| `propose` 的参数自相矛盾；`observe_main` 的 head 与 tree 不符 | `propose`、`observe_main` 第 1 步 | `InvalidInput` | 无写入 | 调用方修正 |
| head 不在 object store 里 | 同上 | `NoSuchCommit`（gitio） | 无写入 | — |
| 提议被并发改写，重读 3 次仍冲突 | `propose` 第 2 步 | `Busy`（reason proposal_race） | 每次都没写成 | 稍后重来 |
| 提议不存在，或不在可用状态；批准前提议已被取代 | `get_proposal`、`ensure_target`、`plan_rollout`、`plan_switch` | `NoSuchProposal` | 无写入 | — |
| 批准时封存出的依赖集不是提议的 `to` | `ensure_target` 第 4 步 | 不报错：`Target::Replaced` | 一条新的 Pending 取代原提议；没有会话被移动 | 人批准替换提议 |
| repo 不在配置里 | daemon 建 `Ctx` 时（`Config::repo`） | `NoSuchRepo` | 无写入 | — |
| 判定某个会话时出错 | `plan_rollout` 第 3 步 | 该会话为 `Skip { Error }` | 无写入；其余会话照常 | 修好后再 approve |
| 依赖目录里有打开的文件 | `plan_rollout`、`plan_switch` | 该会话为 `Skip { OpenFiles }` | 目录一字不动 | 停掉 dev server 后再 approve |
| 计划之后、交换之前有文件被打开 | `session::move_workspace` 复查 | `OpenFiles`（session） | 按 session 的规则 | 再 approve |
| store 写事务失败 | `propose`、`observe_main`、`record_rollout`、尝试与封存的记录 | store 的码 | 事务没写成；封存时见 seal_failed 一行 | — |

借用的码 [我的决定]：`adopt` 的 special_file、symlink_escapes、unreadable_file、changed_during_clone 与 `build` 的 inputs_changed 用 `Drift`；"不是提出者""还没发布"用 `NoSuchProposal`；"有记录、没目录"用 `Corrupt`；提议的并发冲突与认领时拿不到 repo 写锁用 `Busy`；`clone_into` 目标的中间级是符号链接用 `NotDirectory`。`Code` 是封闭的，都取最接近的码、用 `detail.reason` 区分；T15 要在 types.md §6.2 的"谁返回"里补上 deps。

断电：封存前对候选逐项做了屏障，rename 之后对父目录 `F_FULLFSYNC`，登记随 store 的 `fullfsync` 提交。设备或 USB 桥是否真的把缓存写到介质，deps 管不了（同 store.md §4.6"断电"）。

## 7. 测试清单

测试都在临时目录里进行：临时 Store、临时主 checkout（真实的 git 仓库）、`PATH` 上的假 `bun` 与假 install 脚本（它们把自己看到的 cwd、环境、调用次数和起止时刻写进文件），以及记录调用的 `RepoWriteLock` 替身。测试从不读写 `/Volumes/GWD/inter`。需要第二个卷的断言（`CrossVolume`）用 `hdiutil` 建磁盘映像，带 `#[ignore]`，用 `cargo test -p strata-deps -- --ignored` 跑。注入口都在 §3 的 `test-seams` 特性下。

| 测试名 | 断言 | 来源（矩阵行 或 新增） |
| --- | --- | --- |
| `inputs_hash_ignores_manifest_version` | 同一棵树里，只把 `package.json` 顶层 `version` 从 1.0.87 改成 1.0.88：哈希不变；`composer.json` 顶层 `version` 同理；`package-lock.json` 的顶层 `version` 与 `packages[""].version` 一起改：不变；用 `jq` 重写（缩进、键序、末尾换行都变了）：不变；`dependencies` 加一项，或 `packages["node_modules/x"].version` 改了：变；解析不了、或顶层不是对象的 `package.json` 按原始字节处理，改一个字节就变，且不报错 | 新增 |
| `inputs_hash_covers_files_dirs_and_absence` | `bun.lock` 改一个字节：变；在工作区的 `patches/` 里加一个未跟踪、未被 ignore 的补丁：变；加一个被 ignore 的 `.DS_Store`：不变；只改文件 mode：不变；条目缺失不报错，哈希与存在时不同；空目录与不存在的目录哈希相同；没有 `.git` 的导出树与内容相同的 git 工作树哈希相同 | 新增 |
| `inputs_hash_never_follows_symlinks` | `bun.lock` 是指向工作区外文件的符号链接：哈希只取链接目标字符串，改被指向文件的内容不变，改链接目标就变；中间一级 `sidecar` 是符号链接时，`sidecar/package.json` 记为缺失；把被指向的文件 `chmod 000` 之后，`inputs_hash` 照样成功、结果不变，说明 deps 从不打开链接指向的东西 | 新增 |
| `inputs_hash_rejects_special_files` | 安装输入本身是 FIFO：返回 `NotFile`，`detail.path` 指向它；`patches/` 里有 FIFO 或 socket：返回 `NotFile`；FIFO 没有写端时，调用在 1 秒内返回，FIFO 从未被打开 | 新增 |
| `set_id_is_deterministic_and_unambiguous` | 同样的输入在两个进程里得到同一个 64 位小写十六进制 id；`("ab","c","")` 与 `("a","bc","")` 不同；只改 toolchain 或只改 platform 都不同；`deps_dirs` 换个顺序 id 不变，多一项就变；`install` 加一个参数（`--frozen-lockfile`）就变；嵌套的种子缓存多一项（`node_modules/.cache`）就变；`DepsConfigKey::of` 对切不开的 `install` 返回 `InvalidInput`；空安装输入的 `inputs_hash` 与空配置的 id 都是固定常量 | 新增 |
| `toolchain_is_install_program_version` | PATH 上的假 `bun` 打印 `1.3.3`，`install = "bun install --frozen-lockfile"`：toolchain 是 `bun 1.3.3`；程序找不到：`NoSuchPath`，`detail` 带程序名与 PATH；没有 `install`：空串；把假 `bun` 换成打印 `1.3.4` 的新文件后，下一次调用得到 `bun 1.3.4`（缓存按程序文件的身份失效）；stdout 为空时取 stderr 的第一行 | 新增 |
| `is_input_matches_entries_and_members` | `install_inputs = [package.json, bun.lock, patches]`：`package.json`、`patches`、`patches/a.patch`、`patches/sub/b.patch` 为真；`patches2/a.patch`、`sub/package.json`、`bun.lockb`、`node_modules/x/package.json` 为假；`install_inputs` 为空时任何路径都为假 | 新增 |
| `open_files_reports_fd_mapping_and_cwd` | 在 dir 下起三个子进程，一个打开 `dir/a/x.js` 不关，一个 mmap `dir/b/y.node`，一个当前目录在 `dir/c`：`open_files(dir)` 恰好返回这三条，hold 依次为 `Fd`、`Mapped`、`Cwd`，pid 与路径正确，command 非空；打开同前缀的 `dir2/x.js` 的进程不出现；子进程退出后返回空；dir 不存在，或 dir 是符号链接：返回空列表 | 新增 |
| `build_seals_install_output_without_seed_caches` | 假 install 在 staging 里写出 `node_modules/x/index.js`、`node_modules/.cache/y`、`public/pdfium/z.wasm`：`build` 返回的 `DepsSet` 的 `dirs` 为 `[node_modules, public/pdfium]`，`inputs_hash` 等于 `inputs_hash(树)`，toolchain 是假程序的版本；封存目录里有 `x/index.js` 与 `z.wasm`，没有 `node_modules/.cache`；install 的 cwd 是 staging 树的仓库根，PATH 等于 `env.path`，stdin 是 `/dev/null`，环境里没有 `GIT_*`；`.staging/<a>/` 已不在原处，进了 store 的 trash（`retire_dir`）；`find_set` 能找到它；`deps_dir` 根目录的 mode 是 0555，`node_modules` 是 0755；该依赖集有一条 kind 为 build 的尝试，状态 sealed；`test_seams::sync_counts()` 等于（候选里的普通文件数, 目录数, 1） | 新增 |
| `build_failure_is_unknown_and_keeps_scene` | install 以 3 退出：返回 `Unknown`，`detail` 含 reason install_failed、attempt、staging、command、exit_code 3、log_tail；`tree/` 与 `install.log` 都在；`find_set` 为 None；`attempts` 把它列为 Failed；对同一依赖集再调 `build`，返回 reason previous_attempt 的 `Unknown`（带原 attempt），假 install 的调用次数不变；删掉暂存目录后再 `build`，install 重新运行 | 新增 |
| `build_timeout_kills_install_group` | `timeout` 为 1 s，install 先起一个睡 60 s 的孙进程，自己也睡：约 1 s 后返回 `Unknown`（reason timeout）；install 和孙进程都已不存在（`kill(pid, 0)` 得到 `ESRCH`）；暂存保留，`attempts` 为 Failed | 新增 |
| `seal_refuses_when_staging_does_not_match_key` | 四种 install 各配一棵自己的树（四棵树的 `package.json` 各不相同，所以是四个依赖集，互不触发 D8），各跑一次：改写 `bun.lock`（安装输入）；一个依赖目录都不产出；在 `node_modules` 里放一个指向绝对路径的符号链接；在 `node_modules` 里放一个 FIFO。四次都返回 `Unknown`，reason 依次为 inputs_modified、no_deps_dirs、symlink_escapes、special_file；四个 `deps_dir` 都没有出现，`find_set` 都为 None；四个暂存都保留，四条尝试都是 failed，由 `attempts` 报出 | tgrep-core/src/generations/mod.rs::invalid_staged_generation_is_never_published |
| `killed_builder_leaves_undiscoverable_staging_and_free_lock` | 测试二进制以 worker 模式在子进程里对树 A 跑 `build`，假 install 写出就绪标记后睡眠；看到标记就 SIGKILL worker 的进程组，再按尝试记录里的 pid SIGKILL install 自己的进程组（install 用 setpgid 自成一组，杀 worker 的组杀不到它）。之后：非阻塞 flock 立即拿到建造锁；`find_set` 为 None，`deps_dir` 不存在；`attempts` 报出一条 Interrupted，带暂存路径。父进程再对树 A `build`：那条记录被关成 failed（reason interrupted），install 重新运行一次，依赖集封存、`find_set` 找得到，死掉那次的暂存仍在、被 `attempts` 列为 Failed。对树 B 连杀两次（第二个 worker 先关掉第一条、开始重来，再被杀）：父进程的 `build` 返回 reason previous_attempt 的 `Unknown`，install 调用次数不变；删掉最新那次的暂存之后，`build` 重建成功 | tgrep-core/tests/generations.rs::killed_starter_releases_lock_and_leaves_only_undiscoverable_staging |
| `failed_or_lockless_builds_never_publish` | 手放一个没有记录的 `.staging/999/` 垃圾目录：`find_set` 为 None，`attempts` 不列它；install 以 2 退出：没有依赖集；把 `.locks/<id>.lock` 换成目录：`build` 在跑 install 之前返回 `IsDirectory`，没有尝试记录，也没有暂存；另一个已封存的依赖集前后遍历逐条相同 | tgrep-core/tests/generations.rs::interrupted_and_failed_publications_are_not_discoverable |
| `orphan_sealed_dir_is_unknown_not_overwritten` | 手建一个没有记录的 `deps_dir(repo, id)/node_modules/q`：`build` 返回 `Unknown`（reason orphan，带路径）；返回之前在 repo 写锁之下复查过一次（锁替身被进入一次）；该目录和 q 都不变；install 没有运行；没有依赖集记录 | 新增 |
| `clone_into_copies_set_dirs_privately` | `clone_into` 到新目录 dst：`set.dirs` 的每一项都在，内容逐字节相同，inode 不同；在 `dst/node_modules` 里写文件、删文件之后，封存目录前后遍历相同，另一份 `clone_into` 的结果也不变；`dst/node_modules` 已存在：返回 `Exists`，什么都没写；`dst/public` 是指向外部目录的符号链接而 `set.dirs` 含 `public/pdfium`：返回 `NotDirectory`，外部目录前后相同；记录在而封存目录被删：返回 `Corrupt` | 新增 |
| `seal_runs_under_repo_write_lock` | 记录调用的 `RepoWriteLock` 替身：每次封存恰好进入一次；进入时 `.staging/<a>/seal` 在、mode 0755，`deps_dir` 不在，没有依赖集记录；退出时 `deps_dir` 在、根目录 0555、`node_modules` 0755，记录已在、尝试为 sealed；假 install 记下的结束时刻早于进入时刻（install 从不在锁里跑）。替身先返回 3 次 `Busy`：第 4 次封存成功；一直返回 `Busy`：重试 10 次之后返回 `Unknown`（reason seal_busy），候选留在 `.staging/<a>/seal`，尝试为 failed。另外核对 gc 的前提：把封存根目录 rename 到另一个父目录得到 `EACCES`，先 `chmod u+w` 就成功 | 新增 |
| `build_preflight_and_staging_refusals` | PATH 上没有假 `bun`：`NoSuchPath`；`paths.root` 在另一个卷上（磁盘映像，`#[ignore]`）：`CrossVolume`；两种都没有锁文件、尝试记录与暂存。`build(…, Some(工作区))` 时用 `test_seams::after_attempt_begins` 在尝试开始之后改写工作区的 `bun.lock`：返回 `Drift`，reason inputs_changed，带 expected 与 found；`.staging/<a>/` 已拿走（进了 trash），尝试为 refused，install 没有运行 | 新增 |
| `adopt_matches_inputs_hash_else_refuses` | 主 checkout 的 `package.json`、`bun.lock` 与基线导出相同（只差 `package.json` 的 `version`），有 `node_modules/a/index.js`：`adopt` 成功，`inputs_hash` 等于 `inputs_hash(基线导出)`，`dirs` 为 `[node_modules]`，封存的 `a/index.js` 与主 checkout 的逐字节相同、inode 不同，主 checkout 依赖目录的遍历（含 gen count）前后相同，有一条 kind 为 adopt 的尝试，sealed。主 checkout 的 `bun.lock` 有一行未提交的改动：返回 `Drift`，`detail` 为 reason inputs 加两个哈希；没有尝试记录、没有暂存、没有依赖集记录。主 checkout 没有 `node_modules`：返回 `NoSuchPath`。锁替身对封存返回 `Busy`：`adopt` 返回 `Busy`，暂存已拿走，尝试为 refused | 新增 |
| `adopt_refuses_when_main_changes_during_clone` | 用 `test_seams::after_attempt_begins` 在尝试开始之后、克隆之前改写主 checkout 的 `node_modules/a/index.js`：返回 `Drift`，reason 为 changed_during_clone，path 为 `node_modules/a/index.js`；没有依赖集记录，暂存已拿走，尝试为 refused；主 checkout 里留着测试写入的新内容，说明 deps 没有碰它。钩子改的是 `node_modules/.cache/…` 时，认领照常成功。去掉钩子后再 `adopt`：成功 | layerfs-project/tests/acquisition_custody.rs::a_changed_row_in_the_window_is_refused_by_its_path |
| `adopt_refuses_special_files_in_main` | 主 checkout 的 `node_modules/a/` 里有一个没有写端的 FIFO：`adopt` 在 1 秒内返回 `Drift`（reason special_file，path 指向它），FIFO 从未被打开；没有尝试记录，也没有暂存；主 checkout 前后遍历相同 | 新增 |
| `symlinks_inside_repo_root_are_sealed` | `deps_dirs = [node_modules, sidecar/node_modules]`，仓库跟踪 `sidecar/vendor/cloakbrowser/`。主 checkout 有 `node_modules/.bin/tsc → ../typescript/bin/tsc` 与 `sidecar/node_modules/cloakbrowser → ../vendor/cloakbrowser`：`adopt` 成功，两条链接原样封存（readlink 相同），后者在封存目录里悬空；`clone_into` 到基线导出之后，它解析到导出自己的 `sidecar/vendor/cloakbrowser`，不是主 checkout 的。主 checkout 再加 `node_modules/x/abs → /tmp/elsewhere`，或 `node_modules/x/up → ../../../z`：`adopt` 返回 `Drift`（reason symlink_escapes，path 指向它），没有尝试记录。冷装时假 install 造出这两种链接之一：`Unknown`（reason symlink_escapes），现场保留；只造前两种：封存成功 | 新增 |
| `seed_cache_is_cloned_not_sealed` | `deps_dirs = [node_modules]`，`seed_caches = [node_modules/.cache, .next/cache]`，主 checkout 有 `node_modules/.cache/babel/x.json`：认领后封存的依赖集里没有 `node_modules/.cache`；`nested_seed_caches` 返回 `[node_modules/.cache]`，`.next/cache` 不嵌套所以不列；主 checkout 的 `.cache/babel/x.json` 字节与 gen count 都不变；`clone_into` 的结果里也没有 `.cache`，种子缓存由 `session::new` 从主 checkout 克隆（session.md）；冷装时 install 产出的 `node_modules/.cache` 同样不进封存 | layerfs-api/tests/merge_units.rs::two_patterns_make_one_unit |
| `ensure_prefers_sealed_then_adopt_then_build` | 依赖集已封存：`ensure` 不克隆、不跑 install、不建尝试；没有封存而主 checkout 一致：认领，install 调用次数为 0；主 checkout 不一致、没有依赖目录、依赖目录不是目录、或依赖目录里有特殊文件：冷装，install 调用次数为 1；钩子每次都在克隆期间改主 checkout：认领 3 次都是 changed_during_clone，然后冷装，install 调用次数为 1；每种情况返回的 id 都等于 `set_id(inputs_hash(树), platform(), toolchain, 依赖配置)` | 新增 |
| `ensure_refuses_input_missing_from_base_tree` | `install_inputs` 含 `bunfig.toml` 而基线导出里没有：返回 `InvalidInput`，`detail` 为 repo、field install_inputs、index、rule tracked_in_base、path `bunfig.toml`；没有锁文件、没有尝试记录、没有暂存 | 新增 |
| `concurrent_ensure_builds_once` | 两个线程同时对同一棵树 `ensure`，主 checkout 没有依赖目录、必须冷装：install 只运行一次；两边拿到相等的 `DepsSet`；该依赖集只有一条尝试记录，状态 sealed；后到的线程从不开始自己的尝试（没有 refused、failed，也没有 previous_attempt） | 新增 |
| `empty_config_yields_empty_set` | `deps_dirs = []`：`ensure` 返回的依赖集 `dirs` 为空，toolchain 为空串，`inputs_hash` 为空输入常量；不跑任何命令；封存目录存在、为空、根目录 0555；记录的 origin 为 empty；没有尝试记录、锁文件和暂存，锁替身恰好被进入一次；两个线程同时 `ensure`，或再调一次，都得到同一个 id，只有一条依赖集记录；`clone_into` 什么都不克隆 | 新增 |
| `deps_dir_writes_are_not_drift` | 工作区的 `node_modules` 是封存依赖集经 `clone_into` 得到的克隆。模拟 dev-link 写入 `node_modules/@inter/hr-core/src/a.ts`，在 `node_modules/.bin` 下加文件，再删掉一个包：`drift` 为 None，封存目录前后遍历相同；随后改 `package.json` 的 `dependencies`：`drift` 为 Some。依赖目录整个归会话所有，不存在按路径的合并 | layerfs-api/tests/merge_units.rs::a_unit_one_side_changed_merges_as_today |
| `drift_compares_install_inputs_only` | 工作区的 `bun.lock` 改了：返回 `Some(Drift)`，`pinned` 为所钉 id，`pinned_inputs` 为它记录的哈希，`found_inputs` 为新哈希；假 `bun` 换了版本而输入没变：None；对主干 tip 的导出目录（没有 `.git`）调用，结果与在内容相同的工作区上调用一致 | 新增 |
| `observe_main_files_proposal_when_main_set_changes` | 第一次 `observe_main`：返回 None，主干记录为 (tree, head, inputs_hash, id)；新 tip 的依赖集 id 相同：None，只更新 tree 与 head；新 tip 改了 `bun.lock`：一条 Pending，origin 为 Main，from 与 from_inputs 取上一条主干记录，to、to_inputs、tree、head 取新的；to 还没封存（`find_set` 为 None），全程 install 调用次数为 0、没有尝试记录；再来一个不同的依赖集：新的一条 Pending，前一条变成 `Superseded { by: 新 id }`；任何时刻该 repo 只有一条 Pending；只换 PATH 上的假 `bun` 版本（输入不变）：也登记一条 Pending，from_inputs 等于 to_inputs | 新增 |
| `observe_main_validates_head_and_tree` | `tree_of(head)` 不等于给的 tree：`InvalidInput`；head 是另一个仓库的 commit、不在本 repo 的 object store 里：`NoSuchCommit`；两种都不写主干记录、不登记提议。修订前的"依赖集属于别的 repo"一项已经无从发生：`observe_main` 不再收依赖集，repo 只取自 `Ctx` | 新增 |
| `propose_validates_and_is_idempotent` | 同样的参数调两次：同一个 `ProposalId`，只有一条记录；from 等于 to：`InvalidInput`；只给 tree 不给 head，或 origin 为 Main 却没有目标：`InvalidInput`；`tree_of(head)` 不等于 tree：`InvalidInput`；head 不在 object store 里：`NoSuchCommit`；同一会话再提一个不同的 to：它之前的 AwaitingPublish 变成 `Superseded { by: 新 id }`；`ProposalId` 是 `record_proposal` 的返回值，从 1 起递增 | 新增 |
| `publish_promotes_awaiting_proposal` | 会话 S 有一条 AwaitingPublish 提议 P（to 为 D），会话 S3 也有一条 to 为 D 的 AwaitingPublish P3，会话 S5 有一条 to 为 E 的 AwaitingPublish P5，该 repo 另有一条 Pending C。以 origin = `Session { S }` 带目标 (T2, H2)、to 为 D 调 `propose`（模拟 S 发布之后的 `rebase_session`）：返回 P 的 id，状态变为 Pending，origin 仍是 `Session { S }`，tree 与 head 已填上；C 与 P3 都变成 `Superseded { by: P }`；P5 原样仍是 AwaitingPublish。再以 origin = `Session { S }`、同样的参数调一次，以及以 origin = Main、同样的 (to, tree) 调一次：都返回 P，不新增记录。另一个 repo 里，主干先以 origin = Main 登记了目标 (to D, T2) 的 Pending C2，会话 S6 有 to 为 D 的草稿 P6；以 origin = `Session { S6 }` 带同一目标调 `propose`：P6 被提升，C2 变成 `Superseded { by: P6 }`，S6 没有草稿留下。之后会话 S4 登记一条 to 为 D 的草稿 P4，再以 origin = Main 带新的 tree T3 调：新的一条 Pending 取代 P，P4 与 P5 仍是 AwaitingPublish（origin = Main 的登记不动任何草稿） | 新增 |
| `open_proposals_lists_open_and_live_only` | 用 store 的会话写方法建出会话 S1（Active）与 S2（Removed）。repo tool 有 Pending P1、S1 的 AwaitingPublish D1、S2 的 AwaitingPublish D2、一条 Applied、一条 Superseded；repo front 有 Pending P2。`open_proposals(Some(tool))` 恰好是 [P1, D1]，按 id 排；`open_proposals(None)` 另含 P2；D2 的提出会话已被删除，两次都不列；`get_proposal(D2)` 照样返回它 | 新增 |
| `rollout_skips_session_with_open_files` | 准备：Pending 提议 P，目标 `BaseGen { tool, T2, D2 }`，head H2，D2 已封存；会话 S1、S2 都钉 (T1, D1)、tracked，工作区的 `node_modules` 都是 D1 经 `clone_into` 得到的克隆，安装输入未改；起一个子进程打开 S1 的 `node_modules/next/dist/server/next.js` 不关。断言：`plan_rollout(P, [S1, S2], complete = true)` 的 steps 依次为 `Skip { S1, OpenFiles { files: [{ pid: 子进程, path: …/node_modules/next/dist/server/next.js, hold: Fd }] } }` 与 `Move { S2, BaseGen { tool, T2, D2 }, H2 }`；Skip 的 JSON 带 `"kind":"OpenFiles"` 以及 pid 与 path；计划前后，S1 依赖目录的遍历（路径、大小、inode、gen count）逐条相同，store 也前后相同；把计划交给测试里记录调用的 mover（daemon 循环的替身），只有 S2 被调用、且只调用一次；`record_rollout(plan, moved = [S2])` 之后 P 仍是 Pending。子进程退出后，`plan_rollout(P, [S1])` 对 S1 给出 Move。把持有方式换成 mmap（Mapped）和当前目录在 `node_modules` 里（Cwd），各跑一遍，结论相同。交换前的复查由 `session::move_workspace` 负责（session.md 的测试） | 新增 |
| `rollout_moves_base_and_deps_atomically` | 准备同上，但没有打开的文件。断言：`plan_rollout` 的每个 Move 带的 `new_base` 恰好是 `BaseGen { repo: tool, tree: T2, deps: D2 }`，tree 与 deps 取自同一条提议；head 为 H2，且 `gitio::tree_of(H2)` 等于 T2；计划里只有 Move 与 Skip 两种步骤，不存在只换依赖或只换 tree 的步骤；记录调用的 mover 对每个 Move 恰好收到一次 (session, new_base, head)，对 Skip 从不被调用；mover 对 S2 报失败（模拟 `move_workspace` 返回 `OpenFiles`）时，`record_rollout(moved = [S1])` 让 P 保持 Pending，再 approve 时 S1 为 AlreadyThere、S2 为 Move；全部移过之后 P 为 Applied。交换本身的原子性（先 checkpoint、在旁边准备、rename 交换）由 session.md 的 `move_workspace` 测试断言 | 新增 |
| `proposing_session_may_use_new_deps_before_approval` | 准备：会话 S 钉 (T1, D1)，在工作区的 `package.json` 里加了一个依赖；会话 S2 也钉 (T1, D1)。断言：`drift(S)` 为 Some；`build(S 所钉基线 generation 的目录, Some(S 的工作区))` 封存出 D_new，其 `inputs_hash` 等于 S 工作区的哈希；`propose(origin = Session { S }, from = D1, to = D_new, 无目标)` 得到 AwaitingPublish；`plan_switch(P, S)` 恰好是一步 `Move { S, BaseGen { tool, T1, D_new }, S 的 head }`，全程没有 `ConfirmToken` 参与（`plan_switch` 的签名不收凭据）；`plan_switch(P, S2)` 返回 `NoSuchProposal`（不是提出者）；`plan_rollout(P, [S2])` 返回 `NoSuchProposal`，`detail.state` 为 AwaitingPublish，别的会话在发布并确认之前不动；S 的依赖目录被打开时，`plan_switch` 给出 Skip OpenFiles；S 又改了 `bun.lock` 后，`plan_switch` 给出 Skip Drift，其中的哈希不等于 to_inputs。发布后，以 origin = `Session { S }` 带目标调 `propose` 把 P 提升为 Pending，这时 `plan_rollout(P, [S2])` 对 S2 给出 Move | 新增 |
| `plan_switch_on_pending_moves_base_and_deps` | S 提了草稿 P（to 为 D_new）但没有自换，仍钉 (T1, D1)；S 发布后，以 origin = `Session { S }` 带目标 (T2, H2) 调 `propose`，P 变成 Pending。`plan_switch(P, S)` 恰好是一步 `Move { S, BaseGen { tool, T2, D_new }, H2 }`，`complete = false`；`record_rollout` 这份计划之后 P 仍是 Pending；S2 调 `plan_switch(P)`：`NoSuchProposal`（reason not_origin）。`plan_rollout(P, [S, S2])`：S 的工作区哈希等于 to_inputs，所以是 Move 而不是 Drift；S2 也是 Move | 新增 |
| `plan_rollout_skip_reasons_in_order` | 对同一条 Pending 提议（origin = Main）构造九个会话：tracked = false；`base.deps` 已是 to；是另一条 AwaitingPublish 的提出者、那条的 to 不是这次的 to；是另一条 AwaitingPublish 的提出者、那条的 to 正是这次的 to，工作区的安装输入也正是 to 的；工作区改了 `bun.lock`、改成的不是 to 的输入；工作区改了 `bun.lock`、改成的正是 to 的输入；依赖目录被打开；`test_seams::fail_open_files_under` 对它的工作区注入了失败；什么都没有。依次得到 Untracked、AlreadyThere、`OwnChange { 那条提议 }`、Move、Drift、Move、OpenFiles、`Error { error }`（`error.code` 为 `Unknown`）、Move；同时满足多个条件的会话按 §5.10 的顺序取第一个；to 没封存时，前两种以外的会话都是 NotReady；`plan_rollout` 什么都不写，store 与各目录前后相同 | 新增 |
| `record_rollout_applies_only_complete_successful_plans` | `complete = true`、每个 Move 都在 moved 里、其余 Skip 只有 Untracked 或 AlreadyThere：提议变为 Applied。出现 OpenFiles、Drift、NotReady、Error 的 Skip，或某个 Move 不在 moved 里，或 `complete = false`：仍是 Pending。`plan_switch` 的计划（`complete = false`）从不改状态；提议已被取代时，`record_rollout` 原样返回它 | 新增 |
| `approve_of_non_pending_proposal_is_no_such_proposal` | 不存在的 id：`NoSuchProposal`；AwaitingPublish、Applied、Superseded 的提议交给 `plan_rollout` 或 `ensure_target`：`NoSuchProposal`，`detail.state` 是当前状态；`get_proposal` 对存在的提议，任何状态都返回它 | 新增 |
| `approve_files_replacement_when_sealed_id_differs` | origin 为 Main 的 Pending P，to 是用打印 `1.3.3` 的假 `bun` 算出的 D_a，还没封存。把假 `bun` 换成打印 `1.3.4` 的，`ensure_target(P, P.tree 的导出)`：封存出 D_b（≠ D_a，install 调用次数为 1），返回 `Replaced { superseded: P, replacement: Q }`；Q 是 Pending，to 为 D_b，to_inputs 等于 P 的，tree、head、from、origin 都取 P 的；P 变成 `Superseded { by: Q }`；记录调用的 mover 从未被调用；`plan_rollout(P)` 返回 `NoSuchProposal`；`ensure_target(Q)` 直接返回 Ready，不再 install。P.to 已封存时，`ensure_target` 立刻返回 Ready，install 调用次数不变。在 P 被一条新的 Pending 取代之后才走到第 4 步：返回 `NoSuchProposal`，不登记替换 | 新增 |

矩阵覆盖：目标模块为 deps 的 5 行全部落在上表（3d `merge_units.rs` 的两行，3f `generations` 的三行）；理由点名 deps 的 `—` 行有 1 行（3b `acquisition_custody.rs`），也落在上表。上表共 42 条：6 条引用矩阵行，36 条新增。

## 8. 实现任务（TDD）

`tests/common/mod.rs` 放共用的夹具：

- 临时 Store；
- 临时主 checkout，是一个真实的 git 仓库，带提交好的 `package.json`、`bun.lock`、`patches/`；
- 基线导出；
- 假 `bun` 与假 install 脚本，可以控制退出码、睡眠、输出，并把调用次数、cwd、环境、起止时刻写进文件；
- 记录调用的 `RepoWriteLock` 替身，可以按次数返回 `Busy`，记下进入与退出的时刻；
- 记录调用的 mover。

全部放在同一个临时目录下，保证同卷。`Cargo.toml` 声明 `test-seams` 特性；集成测试经本 crate 自身的 dev-dependency 打开它，发行构建不带。

### Task 1: 安装输入哈希与依赖集 id

**Files:**
- Create: `crates/strata-deps/Cargo.toml`、`crates/strata-deps/src/lib.rs`、`crates/strata-deps/src/inputs.rs`、`crates/strata-deps/tests/common/mod.rs`
- Test: `crates/strata-deps/tests/inputs.rs`

**Interfaces:**
- Consumes: `RepoConfig`（config.md）、`RepoPath`、`DepsSetId`、`StrataError`；`apfs::{walk, open_rooted}`；`gitio::Git::is_ignored`
- Produces: `inputs_hash`、`DepsConfigKey`、`set_id`、`platform`、`toolchain`、`InstallEnv`、`is_input`、`nested_seed_caches`

- [ ] **Step 1: 写失败的测试**：`inputs_hash_ignores_manifest_version`、`inputs_hash_covers_files_dirs_and_absence`、`inputs_hash_never_follows_symlinks`、`inputs_hash_rejects_special_files`、`set_id_is_deterministic_and_unambiguous`、`toolchain_is_install_program_version`、`is_input_matches_entries_and_members`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-deps --test inputs`
Expected: FAIL（这些函数还不存在，编译失败）

- [ ] **Step 3: 写让测试通过的最少实现**：
  - §4.3 的两种编码（`set_id` 为 v2，含依赖配置的三个列表），以及 §4.4 的规范化：显式地递归排序键，不依赖 `preserve_order`；
  - §5.1 的解析：每级 `O_NOFOLLOW`，用属性判断类型，从不打开 FIFO，目录成员过 ignore 过滤，空目录等同缺失；
  - §5.2 的 toolchain：按程序文件的身份缓存；
  - `nested_seed_caches` 与 `is_input` 用 `RepoPath` 的前缀关系判断（前缀止于 `/`）。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-deps --test inputs`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-deps/Cargo.toml crates/strata-deps/src/lib.rs crates/strata-deps/src/inputs.rs crates/strata-deps/tests/common/mod.rs crates/strata-deps/tests/inputs.rs
git commit -m "feat(deps): install-input hash and deps set id"
```

### Task 2: 依赖目录上打开的文件

**Files:**
- Create: `crates/strata-deps/src/open_files.rs`
- Test: `crates/strata-deps/tests/open_files.rs`

**Interfaces:**
- Consumes: `libc` 的 libproc 与 `fcntl(F_GETPATH)`
- Produces: `open_files`、`OpenFile`、`Hold`，供 `plan_rollout` 用的批量扫描（crate 内部），以及 `test_seams::fail_open_files_under`

- [ ] **Step 1: 写失败的测试**：`open_files_reports_fd_mapping_and_cwd`。mmap 的子进程用测试二进制的 worker 模式（`std::env::current_exe()` 加一个环境变量），不依赖外部解释器
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-deps --test open_files`
Expected: FAIL（`open_files` 还不存在，编译失败）

- [ ] **Step 3: 写让测试通过的最少实现**：按 §5.11 扫描 fd、映射段和 cwd，用 `F_GETPATH` 取前缀，按字节比较前缀边界；`ESRCH`、`EPERM` 跳过，其它错误返回 `Unknown`；批量形式只扫描一次进程；按线程的失败注入
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-deps --test open_files`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-deps/src/open_files.rs crates/strata-deps/src/lib.rs crates/strata-deps/tests/open_files.rs
git commit -m "feat(deps): open-file scan over fds, mappings and cwd"
```

### Task 3: 封存区、建造锁与冷装

**Files:**
- Create: `crates/strata-deps/src/layout.rs`、`crates/strata-deps/src/seal.rs`、`crates/strata-deps/src/build.rs`、`crates/strata-deps/src/attempts.rs`
- Test: `crates/strata-deps/tests/build.rs`

**Interfaces:**
- Consumes: Task 1；`apfs::{clone_tree, clone_file, walk, same_volume}` 与 rooted 目标操作；store 的 `begin_deps_attempt`、`set_deps_attempt_pid`、`end_deps_attempt`、`record_deps_set`、`deps_set`、`deps_attempts`、`Store::deps_dir`、`Store::deps_staging_dir`、`Store::retire_dir`
- Produces: `Ctx`、`RepoWriteLock`、`find_set`、`build`、`clone_into`、`attempts`、`BuildAttempt`、`BuildKind`、`BuildState`、`test_seams::{after_attempt_begins, sync_counts, reset}`

- [ ] **Step 1: 写失败的测试**：`build_seals_install_output_without_seed_caches`、`build_failure_is_unknown_and_keeps_scene`、`build_timeout_kills_install_group`、`seal_refuses_when_staging_does_not_match_key`、`killed_builder_leaves_undiscoverable_staging_and_free_lock`、`failed_or_lockless_builds_never_publish`、`orphan_sealed_dir_is_unknown_not_overwritten`、`clone_into_copies_set_dirs_privately`、`seal_runs_under_repo_write_lock`、`build_preflight_and_staging_refusals`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-deps --test build`
Expected: FAIL（`build`、`clone_into` 等还不存在，编译失败）

- [ ] **Step 3: 写让测试通过的最少实现**：
  - §4.1 的布局，封存目录的路径取自 `Store::deps_dir`；
  - §5.3 的七段：预检之后拿建造锁（`FD_CLOEXEC`，打不开就报错）；关掉死掉的 running 尝试；孤儿在 repo 写锁之下复查；D8，interrupted 重来一次；`begin_deps_attempt`；install 之前的拒绝先经 `retire_dir` 拿走暂存、再记 refused；
  - 校验与同步一遍完成（I2 的符号链接规则、属主可读、`F_BARRIERFSYNC`）；`with_write` 里 rename → chmod 0555 → `F_FULLFSYNC` → `record_deps_set`；冷装遇到 `Busy` 重试 10 次；
  - §5.5 的冷装：预检，staging 里的删与放走 rooted 目标，复制后核对哈希，环境删 `GIT_*`，自成进程组并记下 pid，超时先 SIGTERM 再 SIGKILL，日志尾部 8 KiB，install 前后核对哈希；
  - §5.7 的 `clone_into`；
  - `attempts` 用非阻塞 flock 推出 Interrupted，只报暂存还在的尝试。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-deps --test build`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-deps/src/layout.rs crates/strata-deps/src/seal.rs crates/strata-deps/src/build.rs crates/strata-deps/src/attempts.rs crates/strata-deps/src/lib.rs crates/strata-deps/tests/build.rs
git commit -m "feat(deps): sealed generations, build lock and cold install"
```

### Task 4: 认领主 checkout 的依赖目录

**Files:**
- Create: `crates/strata-deps/src/adopt.rs`
- Test: `crates/strata-deps/tests/adopt.rs`

**Interfaces:**
- Consumes: Task 1、Task 3；`apfs::{clone_tree, walk, same_volume}`（gen count）
- Produces: `adopt`

- [ ] **Step 1: 写失败的测试**：`adopt_matches_inputs_hash_else_refuses`、`adopt_refuses_when_main_changes_during_clone`、`adopt_refuses_special_files_in_main`、`symlinks_inside_repo_root_are_sealed`、`seed_cache_is_cloned_not_sealed`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-deps --test adopt`
Expected: FAIL（`adopt` 还不存在，编译失败）

- [ ] **Step 3: 写让测试通过的最少实现**：按 §5.4，具名拒绝都在第一次对外可见的写入之前；W1 在拿到建造锁之后、尝试开始之前；两次遍历都跳过嵌套的种子缓存；W1 与 W2 不同时删掉自己的暂存、把尝试记为 refused，返回 `Drift`（reason changed_during_clone）；然后按 §5.3 校验、同步、封存
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-deps --test adopt`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-deps/src/adopt.rs crates/strata-deps/src/lib.rs crates/strata-deps/tests/adopt.rs
git commit -m "feat(deps): adopt the main checkout's deps dirs as the first generation"
```

### Task 5: `ensure` 与空依赖集

**Files:**
- Create: `crates/strata-deps/src/ensure.rs`
- Test: `crates/strata-deps/tests/ensure.rs`

**Interfaces:**
- Consumes: Task 3、Task 4
- Produces: `ensure`、`key_of`

- [ ] **Step 1: 写失败的测试**：`ensure_prefers_sealed_then_adopt_then_build`、`ensure_refuses_input_missing_from_base_tree`、`concurrent_ensure_builds_once`、`empty_config_yields_empty_set`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-deps --test ensure`
Expected: FAIL（`ensure` 还不存在，编译失败）

- [ ] **Step 3: 写让测试通过的最少实现**：按 §5.6：热路径不拿锁；changed_during_clone 时最多认领 3 次，再冷装；其余认领拒绝改走冷装；空依赖集不建尝试、不拿建造锁，在 repo 写锁里复查、建空目录、chmod、同步、登记
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-deps --test ensure`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-deps/src/ensure.rs crates/strata-deps/src/lib.rs crates/strata-deps/tests/ensure.rs
git commit -m "feat(deps): ensure a base's deps set by reuse, adoption or cold install"
```

### Task 6: 漂移

**Files:**
- Create: `crates/strata-deps/src/drift.rs`
- Test: `crates/strata-deps/tests/drift.rs`

**Interfaces:**
- Consumes: Task 1、Task 3（夹具里用 `clone_into` 做工作区的依赖目录）
- Produces: `drift`、`Drift`

- [ ] **Step 1: 写失败的测试**：`deps_dir_writes_are_not_drift`、`drift_compares_install_inputs_only`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-deps --test drift`
Expected: FAIL（`drift` 还不存在，编译失败）

- [ ] **Step 3: 写让测试通过的最少实现**：按 §5.8，只比 `inputs_hash`
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-deps --test drift`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-deps/src/drift.rs crates/strata-deps/src/lib.rs crates/strata-deps/tests/drift.rs
git commit -m "feat(deps): drift as an install-input hash comparison"
```

### Task 7: 提议

**Files:**
- Create: `crates/strata-deps/src/proposal.rs`
- Test: `crates/strata-deps/tests/proposals.rs`

**Interfaces:**
- Consumes: Task 5（`key_of`）；store 的 `record_proposal`、`promote_proposal`、`set_deps_main`、`proposal`、`proposals`；`gitio::Git::tree_of`
- Produces: `Proposal`、`ProposalOrigin`、`ProposalState`、`ProposeArgs`、`propose`、`observe_main`、`get_proposal`、`open_proposals`

- [ ] **Step 1: 写失败的测试**：`observe_main_files_proposal_when_main_set_changes`、`observe_main_validates_head_and_tree`、`propose_validates_and_is_idempotent`、`publish_promotes_awaiting_proposal`、`open_proposals_lists_open_and_live_only`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-deps --test proposals`
Expected: FAIL（提议的类型与函数还不存在，编译失败）

- [ ] **Step 3: 写让测试通过的最少实现**：
  - §5.9 的校验，以及三种情况下的去重、登记、提升与取代；
  - 每次写带比较并交换，冲突就重读重来，3 次仍冲突返回 `Busy`；
  - `ProposalId` 用 `record_proposal` 的返回值；
  - `observe_main` 只算键、不封存，旧记录取自 `set_deps_main` 的返回值；`open_proposals` 读 `proposals(repo, open_only = true)`。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-deps --test proposals`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-deps/src/proposal.rs crates/strata-deps/src/lib.rs crates/strata-deps/tests/proposals.rs
git commit -m "feat(deps): deps change proposals and main tracking"
```

### Task 8: 批准与 rollout 计划

**Files:**
- Create: `crates/strata-deps/src/rollout.rs`
- Test: `crates/strata-deps/tests/rollout.rs`

**Interfaces:**
- Consumes: Task 2（批量扫描）、Task 3、Task 5、Task 6、Task 7；store 的 `decide_proposal`、`record_proposal`；`WorkspaceState`、`BaseGen`
- Produces: `Target`、`ensure_target`、`RolloutPlan`、`RolloutStep`、`SkipReason`、`plan_rollout`、`plan_switch`、`record_rollout`

- [ ] **Step 1: 写失败的测试**：`rollout_skips_session_with_open_files`、`rollout_moves_base_and_deps_atomically`、`proposing_session_may_use_new_deps_before_approval`、`plan_switch_on_pending_moves_base_and_deps`、`plan_rollout_skip_reasons_in_order`、`record_rollout_applies_only_complete_successful_plans`、`approve_of_non_pending_proposal_is_no_such_proposal`、`approve_files_replacement_when_sealed_id_differs`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-deps --test rollout`
Expected: FAIL（计划的类型与函数还不存在，编译失败）

- [ ] **Step 3: 写让测试通过的最少实现**：
  - `ensure_target`：已封存就 Ready；否则 `ensure`，id 不符就登记替换提议并返回 Replaced；
  - §5.10 的判定顺序，工作区哈希等于 `to_inputs` 的不算漂移；
  - 对所有会话只做一次进程扫描；单个会话出错记 `Skip { Error }`；
  - `plan_switch` 只给提出者用，两支各取自己的目标，并要求工作区的哈希等于 `to_inputs`；
  - `record_rollout` 只在计划完整、且全部成功时经 `decide_proposal` 记 Applied；
  - 计划不写任何东西。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-deps --test rollout`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-deps/src/rollout.rs crates/strata-deps/src/lib.rs crates/strata-deps/tests/rollout.rs
git commit -m "feat(deps): approval targets and rollout plans with open-file skips"
```

## 9. 构建者决定记录

design.md 标为 [交给构建者]、并且落在本模块的决定：

| 事项 | 选择 | 理由 | 日期 |
| --- | --- | --- | --- |
| 依赖集键是否含工具链版本（design §4、§12） | 含（types.md 已定）。采集方式：`<install 的程序名> <它的 --version 输出的第一行>`；没有 `install` 时为空串；按程序文件的身份缓存 | 程序自己报的版本最便宜，人也看得懂；对整个二进制求哈希太脆，换一次签名就变 | 2026-10-22 |
| 封存目录的只读手段（design §7、§12） | 靠约定，外加封存根目录 mode 0555；在 repo 写锁之下先 rename、再 chmod（module-rulings T8 I1 裁定）；只改根目录；不加 `chflags uchg` | rename 一个 0555 的目录到另一个父目录得到 `EACCES`（2026-10-22 本机实测），chmod 只能在 rename 之后；clonefile 带着 mode 与 BSD 标志（同日本机实测，store.md §5.3 也测到 `uchg` 随克隆过去），改了里面的目录或加了 uchg，工作区里的 `node_modules` 就不可写，dev-link 与 `bun` 都会坏 | 2026-10-22 |
| generation 保留期（design §7、§12） | 不在本模块：由 store 的 gc 规定。deps 只给出引用规则与回收要求（§4.6） | 回收属于 store | 待定（store.md） |

本模块自己的决定。下表每一行都是 [我的决定]，正文在作出它的地方也标了出来：

| 事项 | 选择 | 理由 | 日期 |
| --- | --- | --- | --- |
| package manifest 忽略的字段（module-rulings T8） | 见 §4.4：`package.json`、`composer.json` 的顶层 `version`；`package-lock.json`、`npm-shrinkwrap.json` 的顶层 `version` 与 `packages[""].version`；规范化 JSON 后再求摘要 | 发布时改版本号不应产生新依赖集；`jq` 与 prettier 改格式也不应 | 2026-10-22 |
| 安装输入目录的成员 | 未被 ignore 的文件（不只是被跟踪的） | 见 §5.1；与 config.md §2.3 的措辞要在 T15 对齐 | 2026-10-22 |
| 两种哈希的编码 | 见 §4.3：derive_key 的上下文串，每段加长度前缀，列表加元素个数；`set_id` 是 v2 | 不同输入不会拼出同一串；改规则就换版本串 | 2026-10-22 |
| 依赖集 id 里的依赖配置 | `deps_dirs`（排序）、嵌套的种子缓存（排序）、`install` 的 argv（保持顺序）。前一类与最后一类是 T8 修订轮的裁定，嵌套的种子缓存是本文加的 | 嵌套的种子缓存决定封存时排除什么，不进键的话改了 `seed_caches` 会让 session 撞上已存在的 `.cache`；键里多了无关的东西，代价只是改配置之后重建一次 | 2026-10-22 |
| `platform` 的取法 | 编译期三元组 | strata 只在 macOS 上跑，不需要运行期探测 | 2026-10-22 |
| install 的时限 | 60 分钟，超时杀进程组 | 冷装要几分钟；挂死的 install 不能让建造锁永远被占着 | 2026-10-22 |
| install 的环境 | daemon 的环境删掉 `GIT_*`，PATH 取 `InstallEnv.path`，stdin 是 `/dev/null`，自成进程组 | 同 gitio 的净化理由；daemon 没有 TTY | 2026-10-22 |
| 失败的冷装（D8） | 不自动重来：现场在就返回 previous_attempt，人删掉现场才会重建；唯一的例外是持有者死掉的尝试（reason interrupted），下一个建造者重来一次，连续两次 interrupted 也拒绝 | 不变量 6"不重放"：失败的 install 是只此一份的证据；持有者被杀多半是 daemon 重启，不是 install 本身的问题，重来一次就够，再死一次就该人看（矩阵 3f:554"重启后可重来或清理"） | 2026-10-22 |
| 建造锁与尝试的顺序 | 预检 → 算 id → 每个 (repo, id) 一个 flock（`FD_CLOEXEC`）→ 写尝试记录；拿锁后先关掉死掉的 running 记录；锁打不开就报错 | 尝试记录写在锁里，同一键别的 running 记录一定属于死掉的建造者（T8 I3 裁定）；锁随 daemon 的死亡释放（3f killed_starter 行）；从不无锁建造（3f interrupted 行） | 2026-10-22 |
| 封存的临界区 | daemon 的写锁经 `Ctx` 里的 `RepoWriteLock` 注入；只包住 rename（空依赖集是 mkdir）、chmod、`F_FULLFSYNC`、登记；认领与空依赖集拿不到就以 `Busy` 拒绝，冷装重试 10 次再 `Unknown` | Addendum 4 要求封存持 repo 写锁，Addendum 3 要求冷装在锁外，只有 deps 自己能在两者之间取锁；跑了几分钟的 install 不该因为一次慢的 fetch（gitio 的 fetch 时限 120 s，长于 daemon 的 30 s 等待上限）就作废 | 2026-10-22 |
| 封存前同步候选（review 的断电撕裂一条） | 选"同步整棵树"：校验遍历里对每个普通文件与目录做 `F_BARRIERFSYNC`，rename 之后对父目录 `F_FULLFSYNC`；属主打不开的普通文件不收 | Addendum 3"Durability"；撕裂的依赖集会被每个新工作区克隆，而登记过的依赖集只有 gc 能拿走。成本：5000 个克隆出来的文件 0.09 s（2026-10-22 本机内置 SSD 实测，约 18 µs 一个；tool 的 `node_modules` 约 10.9 万个文件，约 2 s；工作区所在的 USB SSD 待首个 spike 实测）。打不开的文件做不了屏障（`O_RDONLY` 与 `O_EVTONLY` 都是 `EACCES`，同日实测） | 2026-10-22 |
| 孤儿 | `deps_dir` 在、记录不在 → `Unknown`（reason orphan），不动；先在 repo 写锁之下复查 | gc 在持有 repo 写锁时删记录、挪目录，锁外可能看到删了一半；真的孤儿来自封存中途崩溃或外力，交给人，或交给 gc 的 1 小时清扫 | 2026-10-22 |
| 认领前后的一致性检查 | W1 在拿到建造锁之后、尝试开始之前，W2 在克隆之后，按 gen count 等逐条比较；不同就拿走自己的暂存、返回 `Drift`；`ensure` 最多认领 3 次，再冷装 | 目录级的 `clonefile` 不是快照；主 checkout 的 dev-link 随时在写 | 2026-10-22 |
| 依赖集必须自包含 | 符号链接的规则是 T8 I2 裁定（仓库根以内的相对链接照收，可以悬空）；本文另加：特殊文件与属主打不开的普通文件不收 | 克隆进工作区后不会指回主 checkout（不变量 2）或已删除的 staging；insurance-check 的 sidecar 链接必须能封存，否则每个会话的 `new` 都失败 | 2026-10-22 |
| 提议的写法 | 四种状态（§4.5，Addendum 4）；经 store 的具名方法写，每次带比较并交换，冲突就重读重来，3 次仍冲突返回 `Busy`；只有提出会话自己的改动落地时，才提升它的草稿，提升时一并取代 to 相同的别的草稿；带目标的会话调用先看自己的草稿，当前的 Pending 恰好是同一个目标也照样提升；origin = Main 的登记不动任何草稿 | ccef28f 按"最早的同 to 草稿"提升，会把还没发布的会话的草稿当成落地的那一条，提升出的 Pending 挂在错的提出者名下（自换的权利跟着它走）；Addendum 4 的 `promote_proposal` 由调用方指名提升哪一条。取代范围沿用 ccef28f，store.md 223f9d3 照此实现。先看自己的草稿，是为了不留下一条没人提升、等 s 下一次发布才冒出来的草稿 | 2026-10-22 |
| 提出者自换 | `plan_switch` 不需要确认，要求工作区的哈希等于 `to_inputs`，并检查打开的文件。AwaitingPublish 时 tree 不动；Pending 时（提出者的改动已发布、草稿已被提升）基线与依赖一起移到 (P.tree, P.to) | design §2 目标 3、§6.4 第 3 步 [用户]；不给 agent 新的权力；Pending 那一支与 approve 对别人做的完全一样 | 2026-10-22 |
| approve 的候选与跳过顺序 | tracked 且 `base.deps ≠ to` 的会话都是候选，不限于钉着 `from` 的；跳过的判定顺序固定（§5.10）；工作区的安装输入哈希等于 `to_inputs` 的不算漂移；自己草稿的 to 与这次相同的不判 OwnChange | 落后不止一代的会话也该能一步追上主干；有自己依赖改动的会话（OwnChange、Drift）换过去会错位；改动正是目标的会话换过去不会错位，提出者的草稿被提升之后正是这样接到它；origin = Main 的 Pending 不取代草稿，草稿的 to 又与它相同时，跳过只会让这个会话白等 | 2026-10-22 |
| 批准时封存出的 id 与 `to` 不符 | 替换提议本身是裁定（T8 I5）；本文定：`ensure_target` 返回 `Target::Replaced`，替换提议沿用原提议的 origin、from 与目标；原提议在这之前已被取代就返回 `NoSuchProposal`，不登记替换 | 人批准的是他看到的那条（D10）；不在别人刚登记的新提议之上再叠一条 | 2026-10-22 |
| 主干的跟踪 | 主干记录每个 repo 一条，`observe_main` 只算键、不封存，id 变了就登记 origin = Main 的提议；调用方传入那个 tree 的目录 | design §6.4 第 1 步"主干的锁文件变了"要有人发现；T8 修订轮要求 publish 里不跑冷装；deps 从 git 对象直接读安装输入就得另写一套导出 | 2026-10-22 |
| 发布后 rebase 的契约 | §5.12：`key_of` 算新基线的 id N；提出者的草稿 `to == N` 就以 `Session { s }` 提升它；`N == base.deps` 直接移动；否则等待，并以 Main 登记 | 不变量 4、8；不变量 4 优先于 design §6.4 的最后一句（公共条 3）；`key_of` 让 publish 不必知道键怎么拼 | 2026-10-22 |
| 空依赖集 | `deps_dirs` 为空的仓库也有一个依赖集：输入为空、toolchain 为空串、依赖配置为空、目录为空；不建尝试、不拿建造锁，在 repo 写锁里复查、建空目录、chmod、登记 | `BaseGen.deps` 不是 `Option`，所有仓库走同一条路，每个登记的依赖集都有目录；没有东西要装或克隆，repo 写锁足以串行化；store.md 223f9d3 的 `record_deps_set` 对 Empty 也不收尝试 | 2026-10-22 |
| 拿走暂存 | 不再需要的暂存经 `Store::retire_dir` 原子地挪进 store 的 trash，不就地删；具名拒绝时先拿走暂存、再记 refused | 一次 rename，崩溃不会留下删了一半、看着像现场的暂存；冷装的暂存有十万量级的文件，就地删要几秒。先拿走、后关记录，崩溃留下的 running 记录由下一个建造者关掉；反过来会留下没人收的暂存（gc 不进 `.staging/`） | 2026-10-22 |
| 依赖集的键与布局 | 记录与目录都按 (repo, id) 分开；封存目录与暂存目录的路径由 `Store::deps_dir`、`Store::deps_staging_dir` 给出，`.staging/`、`.locks/` 是 deps 的私有区 | `set_id` 不含 repo（types.md），不同仓库的 id 可能相同；store 拥有 `<root>` 的布局（store.md §4.1） | 2026-10-22 |
| `clone_into` 归 deps | 封存区怎么用只有 deps 知道，克隆进新目录的事也由它做；只写调用方新建的目录，中间级逐级 `O_NOFOLLOW` | session 不必复制这部分知识；deps 仍然从不写活的工作区 | 2026-10-22 |
| `key_of` | 一个函数给出 (inputs_hash, set id) | ensure、observe_main、publish 都要同一个键，只能有一处拼法 | 2026-10-22 |
| `open_files` 的实现 | libproc 扫描 fd、映射段与 cwd；失败返回错误，不返回空列表 | 返回空就是错误地放行，会违反不变量 4；dlopen 的原生模块只能从映射段看到 | 2026-10-22 |
| 测试注入口 | `test-seams` 特性下的 `test_seams` 模块，按线程生效：打开文件扫描的失败注入、尝试开始之后的钩子、同步计数 | Addendum 4"Test seams"；按线程生效，并行的测试互不干扰 | 2026-10-22 |
| 借用的错误码 | 见 §6 末段 | `Code` 是封闭的，取最接近的码，用 `detail.reason` 区分；T15 要在 types.md §6.2 补上 | 2026-10-22 |
| `install` 的切分 | 用 config.md §2.4 的切分函数（`validate_commands` 用的同一个） | 只能有一份切分规则；config.md 还没公开这个函数的名字，T15 要补上 | 待定（config.md） |

需要 T15 改 types.md 的地方：`DepsSetId` 的注释补上依赖配置一段（§4.3）；`Drift` 行补上 special_file、symlink_escapes、unreadable_file、changed_during_clone、inputs_changed 这些 reason；§6.2 的"谁返回"在 `Busy`、`NoSuchProposal`、`Corrupt`、`NotDirectory` 补上 deps。
