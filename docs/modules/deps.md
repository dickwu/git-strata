# `deps` 模块

**Crate**：`crates/strata-deps` · **设计来源**：design.md §3（dev-link 写依赖目录、锁文件不一定在 git 里、symlink 陷阱）、§4（Deps set、Seed cache）、§6.1 第 1 步、§6.4、§6.5 第 7 步、§7、§8、§9 不变量 2、4、5、6、8 · **矩阵**：目标模块为 `deps` 的 5 行（3d 两行、3f 三行），以及理由点名 deps 的 1 行 `—`（3b）

## 1. 职责与边界

deps 负责依赖集（design §4 的 Deps set）里"算"的那一半：算键、建 generation、判漂移、记提议、出 rollout 计划。

| 负责 | 对应 design |
| --- | --- |
| 安装输入哈希 `inputs_hash` 与依赖集 id `set_id`，包括 `platform`、`toolchain` 的采集 | §3"锁文件不一定在 git 里"、§4 |
| 依赖集 generation 的建立与封存。`adopt` 认领主 checkout 现成的依赖目录；认领不了时，`build` 在 staging 克隆里冷装；`ensure` 按"已有 → 认领 → 冷装"的顺序把两者串起来。封存时排除嵌套在依赖目录里的种子缓存 | §6.1 第 1 步、§6.4 第 2 步、§7 |
| `clone_into`：把封存的依赖集克隆进调用方给的新目录。只用 `clonefile`，从不 symlink | §3 symlink 陷阱、§8 |
| `drift`：判漂移。只比安装输入哈希，不比目录内容 | §3"dev-link 写依赖目录"、§4 |
| `observe_main`：记录主干当前的依赖集，并在它变化时登记提议 | §6.4 第 1 步 |
| `Proposal`：依赖变更提议的登记、状态、去重与取代 | §6.4 第 4 步、§6.5 第 7 步 |
| `RolloutPlan`：对每个会话给出"移到哪个基线"或"为什么跳过"。依赖目录上打开的文件由 `open_files` 检查 | §6.4 第 5 步、不变量 4、8 |

明确不负责的事，以及交给谁：

- **移动工作区**：基线与依赖目录一起交换的原子动作是 `session::move_workspace(session, repo, new_base, head) -> Result<MoveReport>`（types.md §8）。它在交换前记一个 `CheckpointReason::BeforeRollout` checkpoint，交换前再查一次打开的文件，有就返回 `OpenFiles`。deps 只出计划：从不写工作区，也不调用 session 或 publish，依赖方向也不允许它这样做。
- **人类确认**：`deps approve` 用的 `ConfirmToken`（`Action::DepsApprove`）由 confirm 签发与校验，由 daemon 核对 action（types.md §4"谁消费"）；TTY 检查在 cli。deps 的任何函数都不收凭据。
- **种子缓存与种子文件的克隆**：由 session::new 负责（design §4、§6.1 第 4 步）。deps 只保证封存时排除嵌套在依赖目录里的种子缓存，并用 `nested_seed_caches` 告诉调用方是哪几个。
- **基线导出与基线缓存**：由 session 经 `gitio::export_tree` 负责。deps 只读调用方交来的导出目录。
- **发布后 rebase**：由 publish::rebase_session 负责。新基线需要另一个依赖集时，它调 `deps::propose` 登记提议，会话停在旧基线上等 `deps approve`（module-rulings 公共条 3：不变量 4 优先于 design §6.4 的最后一句）。
- **repo 写锁与 `Busy`**：由 daemon 负责。deps 自己只有"每个依赖集一把建造锁"（§4.1）。
- **generation 的回收**：由 store 的 gc 负责（design §7"空间回收"）。依赖集的引用规则写在 §4.6，供 gc 使用。
- **依赖目录的内容**：dev-link 每次 dev/build 都把兄弟库镜像进 `node_modules/@inter/*`，并在布局根建 `../node_modules/tailwindcss` 软链（`tool/dev-link.mjs`、`front/dev-link.sh`）；bun 和 composer 也会改写依赖目录。工作区里的依赖目录是会话自己的可写 CoW 克隆，deps 不解释、不监听、不 checkpoint 它的内容（design §6.1"依赖目录的归属"）。

## 2. 依赖

| 依赖 | 理由 |
| --- | --- |
| `strata-types` | 标识与结构（`DepsSet`、`DepsSetId`、`BaseGen`、`WorkspaceState`、`ProposalId`、`SessionId`、`TreeOid`、`CommitOid`、`RepoName`、`RepoPath`）、`StrataError` 与 `Code`、`RepoConfig` 与 `Paths`（config.md §3），以及 config.md §2.4 的命令行切分：`install` 按同一规则切成 argv（§9） |
| `strata-apfs` | `clone_tree` 用于认领、staging 与 `clone_into`，跨卷时返回 `CrossVolume`，从不退化成复制；`clone_file` 把会话的安装输入放进 staging；`walk` 用于展开安装输入目录，以及认领前后的两次遍历；`open_rooted` 每级都带 `O_NOFOLLOW`，用来读安装输入，FIFO 不阻塞 |
| `strata-gitio` | `Git::is_ignored`：在 git 工作树里展开安装输入目录时跳过被 ignore 的文件。`Git::tree_of`：核对提议的 head 与 tree 相符 |
| `strata-store` | 单写者的 SQLite。存 deps 的四张表（§4.2），分配 `ProposalId`。读写经 store.md §3 的写事务 |
| `blake3` | `inputs_hash`、`set_id` 与文件摘要 |
| `serde`、`serde_json` | `Proposal`、`RolloutPlan` 等类型的 JSON，以及 package manifest 的规范化（§4.4） |
| `libc` | libproc（`proc_listallpids`、`proc_pidinfo`、`proc_pidfdinfo`、`proc_name`）、`fcntl(F_GETPATH)`、`flock`、`killpg`。design §11 第 2 条规定 macOS API 走 `libc` |

依赖方向：deps 在第 2 层（types.md §8），只依赖第 0–1 层。它不依赖 overlay，因为漂移比的是安装输入哈希；也不依赖 session、publish、daemon、confirm。

调用 deps 的模块：

| 调用方 | 用到的接口 |
| --- | --- |
| session | `ensure`、`clone_into`、`sealed_dir`、`nested_seed_caches`、`find_set`；钉住新的主干 tip 时调 `observe_main`；`move_workspace` 交换前复查与 `rm` 前检查用 `open_files`；`status` 用 `drift`、`open_proposals`、`attempts` |
| publish | `rebase_session` 用 `inputs_hash`、`set_id`、`find_set` 判断新基线要不要换依赖集，要换就 `propose`。推进了新会话会钉住的 tip 之后调 `observe_main` |
| daemon | `deps propose`：`drift` → `build` → `propose`。`deps approve`：`get_proposal` → 人类确认 → `ensure` → `plan_rollout` → 逐个 Move 调 `session::move_workspace` → `record_rollout`。提出者自换：`plan_switch`。checkpoint 碰到安装输入（`is_input`）后重算 `drift` |
| cli | 只用类型：`Proposal`、`RolloutPlan` 的 JSON，经 daemon lib 的 RPC 类型 |

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
    /// `repo` 的 git，指向主 checkout 的 common dir。
    pub git: &'a Git,
    pub env: &'a InstallEnv,
}

/// 运行 `install` 和 `<程序> --version` 的环境。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallEnv {
    /// 显式的 PATH，由 daemon 按 daemon.md 给出。launchd 的环境里没有用户的 PATH（config.md §2.4 第 4 条）。
    pub path: String,
    /// 一次 `install` 的时限，缺省 60 分钟（§9）。超时就杀掉整个进程组。
    pub timeout: Duration,
}

// ---- 键（§4.3、§4.4、§5.1、§5.2）----
pub fn inputs_hash(cfg: &RepoConfig, git: &Git, root: &Path) -> Result<String>;
pub fn set_id(inputs_hash: &str, platform: &str, toolchain: &str) -> DepsSetId;
pub fn platform() -> String;
pub fn toolchain(cfg: &RepoConfig, env: &InstallEnv) -> Result<String>;
pub fn is_input(cfg: &RepoConfig, path: &RepoPath) -> bool;
pub fn nested_seed_caches(cfg: &RepoConfig) -> Vec<RepoPath>;

// ---- 依赖集 generation（§4.1、§4.2、§5.3–§5.7）----
pub fn sealed_dir(paths: &Paths, repo: &RepoName, id: &DepsSetId) -> PathBuf;
pub fn find_set(store: &Store, repo: &RepoName, id: &DepsSetId) -> Result<Option<DepsSet>>;
pub fn adopt(ctx: &Ctx, tree: &Path) -> Result<DepsSet>;
pub fn build(ctx: &Ctx, tree: &Path, inputs_from: Option<&Path>) -> Result<DepsSet>;
pub fn ensure(ctx: &Ctx, tree: &Path) -> Result<DepsSet>;
pub fn clone_into(paths: &Paths, set: &DepsSet, dst_root: &Path) -> Result<()>;
pub fn attempts(store: &Store, paths: &Paths, repo: Option<&RepoName>) -> Result<Vec<BuildAttempt>>;

/// 一次认领或冷装的尝试，对应表 `deps_builds` 的一行。status 用它报告失败与中断的现场（不变量 6）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildAttempt {
    /// 尝试序号，也是暂存目录 `.staging/<id>/` 的名字。
    pub id: u64,
    pub repo: RepoName,
    /// 这次要建的依赖集。冷装在准备好 staging 之前还不知道 id，那时是 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set: Option<DepsSetId>,
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

/// `attempts` 只报下面三种状态；已封存或已被具名拒绝的尝试不报。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BuildState {
    /// 建造锁仍被持有：正在建。
    Running,
    /// 行还是 running，但建造锁没人持有，说明持有者死了（daemon 崩溃或被杀）。读的时候推出来，不写回表里。
    Interrupted,
    /// install 失败或超时，或者封存前的校验没过。现场保留。
    Failed,
}

// ---- 漂移（§5.8）----
/// 漂移（design §4）：在某个目录里算出的安装输入哈希，与所钉依赖集记录的哈希不同。
/// 它是数据，不是错误；types.md 的 `Drift` 码只用于 `adopt` 的拒绝。
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
    /// 会话改了自己的安装输入（design §6.4 第 1 步"某会话改了锁文件"），由 `deps propose` 登记。
    Session { session: SessionId },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ProposalState {
    /// 由会话发起、改动还没发布：没有目标 tree，只有提出者自己可以先换（`plan_switch`）。
    /// 对它调 `deps approve` 返回 NoSuchProposal。
    AwaitingPublish,
    /// 有目标 tree，待确认，`deps approve` 可以用它。每个 repo 至多一条。
    Pending,
    /// 某次 approve 覆盖了该 repo 的全部会话之后，再没有会话需要它（见 `record_rollout`）。
    Applied,
    /// 被更新的提议取代。
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
pub fn observe_main(ctx: &Ctx, tree: &TreeOid, head: &CommitOid, set: &DepsSet) -> Result<Option<Proposal>>;
pub fn get_proposal(store: &Store, id: ProposalId) -> Result<Proposal>;
pub fn open_proposals(store: &Store, repo: Option<&RepoName>) -> Result<Vec<Proposal>>;

// ---- rollout 计划（§5.10）----
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
    /// 提议的 to 还没封存：可能还没建，也可能上次没建成（见 `attempts`）。
    NotReady,
    /// 这个会话是另一条 AwaitingPublish 提议的提出者，正用着自己的依赖集；换过去会丢掉它自己的依赖改动。
    OwnChange { proposal: ProposalId },
    /// 会话改了自己的安装输入，工作区算出的哈希与所钉依赖集记录的不同；换过去会与它的改动错位。
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
```

各函数的语义与可能返回的 `Code`（具名拒绝都发生在第一次对外可见的写入之前，规则见 §6 开头）：

| 函数 | 语义 | 可能的 `Code` |
| --- | --- | --- |
| `inputs_hash` | 按 §4.3 的编码，对 `root` 下 `cfg.install_inputs` 的内容求 64 位小写十六进制哈希。`root` 可以是基线导出、staging、主 checkout 或工作区。条目缺失不报错，按"缺失"编码 | `NotFile`；读失败按 types.md §6.3 归类 |
| `set_id` | 按 §4.3 计算 BLAKE3(inputs_hash ∥ platform ∥ toolchain)，纯计算 | 无 |
| `platform` | 编译期的目标三元组，例如 `aarch64-apple-darwin` | 无 |
| `toolchain` | `<install 的程序名> <它的 --version 输出的第一行>`，没有 `install` 时为空串（§5.2） | `NoSuchPath`（程序找不到）、`InvalidInput`（`install` 切不开，即 config.md V9）、`Unknown`（`--version` 超时或非零退出） |
| `is_input` | `path` 等于某个 `install_inputs` 条目，或位于某个目录条目之下 | 无 |
| `nested_seed_caches` | 位于某个 `deps_dirs` 条目之内的 `seed_caches` 条目（config.md V7 的唯一例外，如 `node_modules/.cache`），按字节序 | 无 |
| `sealed_dir` | `<root>/deps/<repo>/<id>/`（§4.1），纯计算 | 无 |
| `find_set` | 读 `deps_sets` 的行；只有有行的才是依赖集（不变量 D2） | store 的码 |
| `adopt` | 主 checkout 的安装输入与基线导出 `tree` 相同时，把它现成的依赖目录克隆、校验、封存为依赖集（§5.4） | `InvalidInput`、`Drift`、`NoSuchPath`、`NotDirectory`、`NotFile`、`CrossVolume`、`IsDirectory`（锁）、`Unknown` |
| `build` | 在 staging 里克隆 `tree`、放入 `inputs_from` 的安装输入、跑 `install`，校验后封存（§5.5） | `InvalidInput`、`NoSuchPath`、`NotFile`、`CrossVolume`、`IsDirectory`（锁）、`Unknown` |
| `ensure` | 依次尝试：已封存 → 认领 → 冷装；`deps_dirs` 为空时返回空依赖集（§5.6） | 同 `adopt` 与 `build`；`adopt` 的 `Drift`、`NoSuchPath`、`NotDirectory` 不外传，改走冷装 |
| `clone_into` | 把 `set.dirs` 逐项从封存目录克隆到 `dst_root/<dir>`；`dst_root` 必须是调用方新建的目录，不能是活的工作区（§5.7） | `Exists`、`Corrupt`、`CrossVolume`、`Unknown` |
| `attempts` | 列出暂存目录还在的 Running、Interrupted、Failed 尝试 | store 的码 |
| `drift` | `inputs_hash(ws)` 与 `pinned.inputs_hash` 不同时返回 `Some` | 同 `inputs_hash` |
| `propose` | 校验参数后登记提议：去重、提升、取代（§5.9） | `InvalidInput`、`NoSuchCommit`（gitio 透传）、store 的码 |
| `observe_main` | 记下主干当前的 (tree, head, 依赖集)；依赖集变了就以 origin = Main 调 `propose` | 同 `propose` |
| `get_proposal` | 读一条提议，任何状态都返回 | `NoSuchProposal` |
| `open_proposals` | 列出 AwaitingPublish 与 Pending 的提议，按 id 排 | store 的码 |
| `plan_rollout` | 对一条 Pending 提议和调用方给的会话算出计划，不写任何东西（§5.10） | `NoSuchProposal`、`InvalidInput`；单个会话的失败记成 `Skip { Error }`，不让整个调用失败 |
| `plan_switch` | 只给提出者用：它可以在确认之前先换到自己的新依赖集（design §2 目标 3、§6.4 第 3 步 [用户]） | `NoSuchProposal`、`InvalidInput` |
| `record_rollout` | 计划完整、且每个 Move 都成功时，把提议记为 Applied；否则不改状态 | store 的码 |
| `open_files` | 列出 `dir` 下被任何同 uid 进程以文件描述符、内存映射或当前目录持有的路径（§5.11）。枚举失败是错误，不会返回空列表 | `Unknown` |

与实施计划 Task 8 的 Produces 相比，有这几处差异，理由见 §9：

- `inputs_hash` 返回 `String`（module-rulings T8 附录 1），多了 `cfg` 与 `git` 两个参数。
- `set_id` 是本模块的函数。
- `adopt` 只从 `cfg.path` 认领，所以没有 `from` 参数。
- `build` 自己建 staging，参数是要安装的源码树。
- `drift` 与 `open_files` 都返回 `Result`。
- 计划里的 `rollout(session, proposal, confirm)` 拆成了 `plan_rollout`、`plan_switch`、`record_rollout` 三个函数：移动由 `session::move_workspace` 做，凭据由 daemon 校验，deps 都不碰。

## 4. 数据与不变量

### 4.1 目录布局

```text
<root>/deps/                          Paths::deps()（config.md §3）
  <repo>/
    <set-id>/                         封存的依赖集；根目录 mode 0555，只读靠约定（§9）
      node_modules/                   DepsSet.dirs 的每一项，按仓库内相对路径排列
      public/pdfium/                  例如 tool 的 postinstall 产物（config.md 附录 A）
    .staging/
      <attempt>/                      一次尝试的暂存，<attempt> = deps_builds.id
        tree/                         冷装：源码树的克隆，install 在这里跑
        seal/                         封存候选：只含依赖目录；成功时整个 rename 成 <set-id>/
        install.log                   冷装：install 的 stdout 与 stderr
    .locks/
      <set-id>.lock                   建造锁：普通文件，flock(LOCK_EX)，fd 带 FD_CLOEXEC
```

- 只有 `deps_sets` 里有行的 `<set-id>/` 才是依赖集。`.staging/` 下的任何东西，以及没有行的 `<set-id>/`，都不可发现、不被引用（不变量 D2）。
- 依赖集的键是 `(repo, id)`：`set_id` 不含 repo，两个 repo 的安装输入恰好逐字节相同时，id 会相同。所以目录与表都按 repo 分开（types.md `DepsSet` 的注释："design §4 的键 … 就是这里的 (repo, id)"）。
- 空依赖集：`deps_dirs` 为空的仓库（release-notes、四个 Rust 仓库、多数 `@inter/*` 库）也有依赖集，这样 `BaseGen.deps` 永远有值。它的安装输入为空，toolchain 为空串，`dirs = []`，封存目录是空的。

### 4.2 表（deps 拥有，DDL 收进 store 的 schema）

```sql
-- 封存的依赖集：一行就是一个可发现的 generation（types.md 的 DepsSet）。
CREATE TABLE deps_sets (
  repo        TEXT NOT NULL,              -- RepoName
  id          TEXT NOT NULL,              -- DepsSetId，64 位小写十六进制
  inputs_hash TEXT NOT NULL,              -- 64 位小写十六进制
  platform    TEXT NOT NULL,
  toolchain   TEXT NOT NULL,              -- 没有 install 时为空串
  dirs        TEXT NOT NULL,              -- JSON 数组：封存时实际存在的依赖目录（RepoPath，按字节序）
  origin      TEXT NOT NULL CHECK (origin IN ('adopt', 'build', 'empty')),
  sealed_at   TEXT NOT NULL,              -- RFC 3339 UTC
  PRIMARY KEY (repo, id)
);

-- 认领与冷装的尝试：status 据此报告失败与中断的现场。
CREATE TABLE deps_builds (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,  -- 尝试序号，也是 .staging/<id>/ 的目录名；永不复用
  repo       TEXT NOT NULL,
  set_id     TEXT,                        -- 冷装在 staging 准备好之前还不知道 id
  kind       TEXT NOT NULL CHECK (kind IN ('adopt', 'build')),
  state      TEXT NOT NULL CHECK (state IN ('running', 'sealed', 'failed', 'refused')),
  staging    TEXT NOT NULL,               -- 暂存目录的绝对路径
  pid        INTEGER,                     -- install 进程（进程组组长）
  started_at TEXT NOT NULL,
  ended_at   TEXT,
  error      TEXT                         -- JSON：失败或拒绝时的 StrataError
);
CREATE INDEX deps_builds_by_set ON deps_builds (repo, set_id);

-- 依赖变更提议。
CREATE TABLE deps_proposals (
  id             INTEGER PRIMARY KEY AUTOINCREMENT,  -- ProposalId，1..=i64::MAX（types.md §3）；gc 删行之后也不复用
  repo           TEXT NOT NULL,
  origin_session TEXT,                    -- NULL 表示 origin = Main
  from_set       TEXT NOT NULL,
  from_inputs    TEXT NOT NULL,
  to_set         TEXT NOT NULL,
  to_inputs      TEXT NOT NULL,
  tree           TEXT,
  head           TEXT,
  state          TEXT NOT NULL CHECK (state IN ('AwaitingPublish', 'Pending', 'Applied', 'Superseded')),
  superseded_by  INTEGER REFERENCES deps_proposals (id) DEFERRABLE INITIALLY DEFERRED,
  created_at     TEXT NOT NULL,
  updated_at     TEXT NOT NULL,
  CHECK ((tree IS NULL) = (head IS NULL)),
  CHECK (state <> 'AwaitingPublish' OR tree IS NULL),             -- 还没发布就没有目标
  CHECK (state NOT IN ('Pending', 'Applied') OR tree IS NOT NULL), -- 可批准的、已批准的都有目标；Superseded 两种都可能
  CHECK ((state = 'Superseded') = (superseded_by IS NOT NULL)),
  CHECK (origin_session IS NOT NULL OR tree IS NOT NULL)
);
CREATE UNIQUE INDEX deps_proposals_one_pending ON deps_proposals (repo) WHERE state = 'Pending';
CREATE UNIQUE INDEX deps_proposals_one_draft ON deps_proposals (repo, origin_session) WHERE state = 'AwaitingPublish';

-- 主干当前的依赖集，每个 repo 一行，由 observe_main 更新。
CREATE TABLE deps_main (
  repo        TEXT PRIMARY KEY,
  tree        TEXT NOT NULL,
  head        TEXT NOT NULL,
  set_id      TEXT NOT NULL,
  observed_at TEXT NOT NULL
);
```

deps 对这四张表只做 INSERT 与 UPDATE，行只由 store 的 gc 删除（module-rulings T6）。一次操作要改的几行放在 store 的同一个写事务里。

### 4.3 键与编码

- **`inputs_hash`**：哈希器是 `blake3::Hasher::new_derive_key("git-strata install-inputs v1")`。§5.1 产出一组记录，按路径的 UTF-8 字节序排好，逐条写入：路径的字节长度（u64 小端）、路径字节、一个标签字节，以及 32 字节摘要（缺失的条目没有摘要）。标签与摘要如下：

  | 标签 | 含义 | 摘要 |
  | --- | --- | --- |
  | 0 `Absent` | 条目不存在；或者某个中间级不是目录；或者是没有成员的目录 | 无 |
  | 1 `File` | 普通文件，原始字节 | BLAKE3(内容) |
  | 2 `Symlink` | 符号链接本身，不跟随 | BLAKE3(readlink 的字节) |
  | 3 `Manifest` | 能规范化的 package manifest（§4.4） | BLAKE3(规范化字节) |

  输出 64 位小写十六进制。没有任何安装输入时，哈希器什么都不写，得到一个固定常量（空依赖集用它）。
- **`set_id`**：哈希器是 `blake3::Hasher::new_derive_key("git-strata deps-set-id v1")`，依次写入 inputs_hash、platform、toolchain 三段，每段先写它的字节长度（u64 小端），再写字节。输出 64 位小写十六进制（types.md 要求三段的拼接方式由本文规定，同样的输入必定得到同样的 id）。长度前缀保证 `("ab","c","")` 与 `("a","bc","")` 不会相撞。
- **`platform`**：`format!("{}-apple-darwin", std::env::consts::ARCH)`，在 Apple Silicon 上是 `aarch64-apple-darwin`。
- **`toolchain`**：见 §5.2。工具链版本进入键（types.md §4 末段），所以 `bun upgrade` 之后同一棵树对应新的 id。
- **不改编码版本就不能改规则**：§4.3、§4.4 的任何改动都会改变所有 id，必须换 derive_key 的版本串（`v1` → `v2`）；所有依赖集随后各重新认领或冷装一次。

### 4.4 package manifest 忽略的字段

module-rulings T8 规定，每次发布的版本号变化不得产生新的依赖集。规则如下：文件名（最后一级）在下表里、内容能解析为顶层是对象的 JSON 时，删掉表里的字段，把对象的键递归地按 UTF-8 字节序排序，紧凑输出（无空白），再对结果求摘要（标签 3）。键序要显式排序，不依赖 serde_json 的 `preserve_order` 特性是否被别的 crate 打开（Cargo 的特性合并会把它悄悄打开）。解析失败、或顶层不是对象时，按原始字节处理（标签 1），不报错。

| 文件名 | 删掉的字段 | 为什么 |
| --- | --- | --- |
| `package.json` | 顶层 `version` | 各仓库的 `build.sh` 每次发布都用 `jq ".version = …"` 改它，且 jq 会重排格式。顶层 `version` 是包自己的版本，不影响装出来的依赖 |
| `composer.json` | 顶层 `version` | 同上（staff-api 目前没有这个字段，规则照样成立） |
| `package-lock.json`、`npm-shrinkwrap.json` | 顶层 `version`；`packages[""].version` | 这两处是根包版本的镜像，`npm version` 会同时改它们。front 被跟踪的 `package-lock.json` 带着它们，而且已经与 `package.json` 不同步（0.1.7 对 0.1.18） |

其它字段一律参与哈希，包括 `scripts`（`postinstall` 会改变装出来的结果）与 `name`。嵌套位置的 `version`，例如 `packages["node_modules/x"].version`、`dependencies.version`，都不删。`bun.lock` 的根工作区条目不带版本，`composer.lock` 也没有根包版本，两者都按原始字节处理。

### 4.5 提议的状态机

```text
                 propose(会话, 无目标)
                        │
                        ▼
              ┌──────────────────┐  propose(带目标, 同一个 to)  ┌─────────┐ record_rollout(完整且全部移过) ┌─────────┐
              │ AwaitingPublish  │ ───────────────────────────► │ Pending │ ─────────────────────────────► │ Applied │
              └──────────────────┘          （提升）            └─────────┘                                └─────────┘
                        │                                        ▲   │
                        │ 同一会话又提了不同的 to               │   │ 该 repo 有更新的 Pending
                        ▼                                        │   ▼
                  Superseded { by }        propose(Main, 带目标)─┘  Superseded { by }
```

- 每个 repo 至多一条 Pending，由唯一索引保证；每个 (repo, 提出会话) 至多一条 AwaitingPublish。
- 目标的 tree 与 head 设定之后不再改。主干之后又前进了、而依赖集没变时，Pending 的目标仍是登记时的那个 tree；会话下一次发布时，发布后 rebase 会把它接到链顶。
- 一个会话正在等待一条 Pending 提议 P 的条件是：它 tracked，且 `base.deps ≠ P.to`。status 显示这些会话"在等 deps approve"（module-rulings 公共条 3）。

### 4.6 依赖集的引用（给 store 的 gc）

一个依赖集 (repo, id) 在下列任何一处被引用时都不能回收（design §6.4 第 6 步："旧依赖集保留到没有会话或基线引用它"）：

1. 某个现存会话的 `WorkspaceState.base.deps`；
2. 某个基线 generation（store 的 `base_gens`）的 deps；
3. 某条 Pending 提议的 `to_set`；某条 AwaitingPublish 提议的 `to_set`，前提是它的提出会话还存在。`from_set` 只是给人看的记录，不保活：仍钉着它的会话已经由第 1 条保住了；
4. `deps_main` 的 `set_id`。

不被引用、且超过保留期之后才回收。保留期由 store.md 规定。回收顺序是：先删 `deps_sets` 的行，使它不可发现；再把封存根目录的 mode 改回可写；最后删目录。中途崩溃只会留下一个没有行的目录，下次 gc 扫到它再删。deps 自己从不删封存目录。

### 4.7 不变量

| 编号 | 不变量 | 落实 design §9 |
| --- | --- | --- |
| D1 | 封存的依赖集永不原地修改。deps 对 `<root>/deps/<repo>/<id>/` 只有一次写，即把暂存目录 rename 过去；之后只读（约定加根目录 0555）。安装结果变了，就是新的 id、新的目录 | 第 5 条 |
| D2 | 暂存不可发现。只有 `deps_sets` 里的行才是依赖集；`.staging/` 下的目录、没有行的 `<set-id>/` 都不会被 `find_set` 返回，也不会被克隆进任何地方 | 第 5、6 条 |
| D3 | 键是忠实的。一行 `deps_sets` 的 `inputs_hash`，等于封存前在暂存树（冷装）或基线导出（认领）上算出的值；冷装时 install 前后这个值相同；`id = set_id(inputs_hash, platform, toolchain)` | 第 8 条（基线 = tree + 匹配的依赖集） |
| D4 | 依赖集是自包含的。封存的依赖目录里只有目录、普通文件，以及不逃出所在依赖目录的相对符号链接。所以克隆到哪里都能用，也不会经符号链接指回主 checkout | 第 2 条 |
| D5 | 种子缓存不封存。`nested_seed_caches` 里的路径永远不在封存的依赖集里（design §4："不同步、不 checkpoint、不封存"） | — |
| D6 | 每个 (repo, id) 同一时刻至多一个建造者（flock）。拿不到锁就等；锁打不开就报错，从不无锁建造 | 第 5 条 |
| D7 | deps 产出的移动只有 `RolloutStep::Move`，它带一整个 `BaseGen`（tree 与 deps 取自同一条提议）和匹配的 head；没有只换依赖或只换源码的步骤。Move 只出现在两处：Pending 提议的 `plan_rollout`（daemon 在人类确认之后调用），以及提出者自己的 `plan_switch`。依赖目录上有打开的文件就是 Skip | 第 4、8 条 |
| D8 | 失败的冷装不重放。同一依赖集最近一次尝试失败或中断、而且现场还在时，deps 不再跑 install，直接返回那次的 `Unknown`；status 经 `attempts` 报出现场 | 第 6 条 |
| D9 | 对主 checkout 只读。认领只从它往外克隆，只遍历和读安装输入；从不写它的工作树、索引或 refs | 第 2 条 |
| D10 | 提议的状态只前进（§4.5）；目标设定之后不改；deps 不删行 | 第 4 条：approve 移到的就是登记时、人看到的那个目标 |

`plan_switch` 让提出者换成 `BaseGen { tree: 原 tree, deps: to }`，是唯一一种不移动 tree 的依赖替换。design §2 目标 3 与 §6.4 第 3 步 [用户] 明确允许提出者先用自己的新依赖。`to` 是用这个工作区自己的安装输入（tree 加上它的 overlay）建出来的，§5.10 也要求工作区当前的哈希等于 `to_inputs`，所以不变量 8 要防的"源码与依赖错位"不会发生。会话发布这次改动之后，新 tree 的安装输入就是 `to_inputs`，发布后 rebase 不需要换依赖集，基线回到"tree + 匹配的依赖集"的常态。

## 5. 流程与算法

记号：【锁】是 deps 的建造锁（flock），【rename】是原子替换。【checkpoint】不会出现在本模块里：deps 从不写工作区，交换前的 checkpoint 在 `session::move_workspace` 里。repo 写锁属于 daemon（§5.12）。

### 5.1 `inputs_hash(cfg, git, root)`

1. 判断 `root` 的种类：`root/.git` 存在（gitfile 或目录）就是 git 工作树（主 checkout 或工作区），目录成员要过 ignore 过滤；否则是导出树（基线导出或 staging），不过滤，因为导出里只有被跟踪的文件。
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
4. 否则运行 `<程序> --version`：环境同 §5.5 第 6 步，时限 10 秒。取 stdout 第一行非空行并去掉首尾空白；stdout 为空时取 stderr 的。结果是 `<程序文件名> <那一行>`，例如 `bun 1.3.3`。非零退出或超时返回 `Unknown`。

### 5.3 建造锁与封存（`adopt`、`build`、空依赖集共用）

- **【锁】**：打开 `.locks/<id>.lock`，带 `O_CREAT | O_CLOEXEC`，然后 `flock(LOCK_EX)` 阻塞等待。持有者的 install 有时限，所以等待有界。打不开时按 types.md §6.3 归类（例如锁的位置被换成了目录，就是 `EISDIR` → `IsDirectory`），并且从不在无锁状态下继续。锁 fd 带 `FD_CLOEXEC`，不被 install 继承：daemon 死了锁就释放，而残留的 install 进程只能写进它自己那个不可发现的暂存。
- **拿到锁之后的三项检查**，都在写任何东西之前：
  - `find_set` 已有这个依赖集：直接返回它（别的建造者刚封存了）。
  - `sealed_dir` 存在但没有行：返回 `Unknown`，`detail.reason = "orphan"`。不动它，等人检查。
  - 同一 (repo, id) 除本次以外最近的一次尝试是 failed，或者是 running，并且它的暂存目录还在：返回 `Unknown`，`detail.reason = "previous_attempt"`，附上那次的 attempt、staging 和 error（D8）。此刻锁在本次手里，所以别的 running 行的持有者一定已经死了，也就是 Interrupted。
- **封存**，在暂存里的候选目录 `seal/` 已经校验通过之后：
  1. `chmod 0555 seal/`。
  2. 【rename】把 `seal/` rename 成 `sealed_dir(repo, id)`；然后对 `<root>/deps/<repo>/` 做 `fcntl(F_FULLFSYNC)`。rename 遇到 `EEXIST` 或 `ENOTEMPTY` 时返回 `Unknown`（reason orphan）。
  3. 在一个 store 写事务里插入 `deps_sets` 的行，并把尝试记为 sealed。这一步失败返回 `Unknown`（reason seal_failed）：目录在、行不在，所以不可发现，status 经 `attempts` 报出路径。
  4. 删掉 `.staging/<attempt>/` 剩下的部分。尽力而为，删不掉只记日志，不影响已封存的依赖集。
  5. 释放锁。

### 5.4 `adopt(ctx, tree)`

`tree` 是基线导出目录。只从 `cfg.path`（主 checkout）认领（design §6.1 第 1 步）。

1. `cfg.deps_dirs` 为空：返回空依赖集，同 §5.6 第 1 步。
2. `H = inputs_hash(cfg, git, tree)`。每个 `install_inputs` 条目都必须在 `tree` 里存在；缺了返回 `InvalidInput`（config.md §4.3），`detail = { "repo", "field": "install_inputs", "index", "rule": "tracked_in_base", "path" }`。
3. `M = inputs_hash(cfg, git, cfg.path)`。`M ≠ H` 时返回 `Drift`，`detail = { "reason": "inputs", "base_inputs": H, "main_inputs": M }`。
4. 用 `toolchain` 算出 `id = set_id(H, platform(), toolchain)`。
5. 【锁】，然后做 §5.3 的三项检查。
6. 列出主 checkout 里存在的依赖目录：对每个 `d ∈ deps_dirs` 做 lstat。不存在的跳过；存在但不是目录的（包括符号链接）返回 `NotDirectory`；一个都不存在时返回 `NoSuchPath`，表示没有可认领的。
7. 第一次遍历 W1：`apfs::walk` 每个存在的依赖目录，跳过 `nested_seed_caches`，记下 (路径, 类型, 大小, gen count, mtime, 链接目标)。出现 FIFO、socket 或设备文件时返回 `Drift`（reason special_file）；出现绝对路径的符号链接，或相对目标经词法解析后逃出所在依赖目录的，返回 `Drift`（reason symlink_escapes）。到这一步为止还什么都没写。
8. 插入一条尝试行（adopt，running）。对每个存在的 d，`apfs::clone_tree(cfg.path/d, .staging/<attempt>/seal/d)`，父目录按需创建；然后删掉 `seal/` 里的 `nested_seed_caches`。
9. 第二次遍历 W2，规则同第 7 步。W1 与 W2 逐条比较（路径集合、类型、大小、gen count、mtime、链接目标）；某条的 gen count 读不到（例如有可写的 mmap，见 apfs.md 的 gen count 约定）也算不同。只要有不同：删掉 `.staging/<attempt>/`，把尝试记为 refused，释放锁，返回 `Drift`（reason changed_during_clone，带第一个不同的路径）。W1 与 W2 相同，说明克隆期间主 checkout 的依赖目录没有被写过，所以克隆是一个一致的快照。
10. 按 §5.3 封存：origin = adopt，`dirs` 取存在的依赖目录（按字节序）。返回 `DepsSet`。

[我的决定] 第 9 步的拒绝会删掉自己的暂存。这个暂存只是主 checkout 的 CoW 克隆，没有独有的数据，也从未可发现；失败本身也是确定的，所以不属于不变量 6 的"不确定的失败"。第 7、9 步的 special_file、symlink_escapes、changed_during_clone 借用 `Drift` 码，因为它们对调用方的含义与 reason inputs 相同：这次认领不成，改走冷装。`Code` 是封闭的，types.md 的 `Drift` 行要在 T15 补上这三个 reason。

复杂度：两次遍历加一次 `clone_tree`，都是 O(依赖目录的条目数)。认领只在一个依赖集的第一代发生一次。

### 5.5 `build(ctx, tree, inputs_from)`

`tree` 是要安装的源码树：基线导出，或者会话的基线缓存。`inputs_from` 是会话的工作区：会话发起的依赖变更，要从这里取安装输入（design §6.4 第 2 步）。

1. `cfg.deps_dirs` 为空：返回空依赖集。把 `install` 切成 argv，切不开返回 `InvalidInput`。用 `toolchain` 取工具链，可能返回 `NoSuchPath`。
2. 插入一条尝试行（build，running，`set_id` 为 NULL）。`apfs::clone_tree(tree, .staging/<attempt>/tree)`，记作 S。删掉 S 里的 `deps_dirs` 与 `seed_caches`（它们是自己的克隆，可以删）。
3. 如果给了 `inputs_from`，对每个安装输入条目 e：先删掉 S/e，再按 §5.1 对 `inputs_from` 的解析结果，把文件、符号链接（链接本身）和目录成员用 `apfs::clone_file` 放进 S，缺失的保持缺失。没给 `inputs_from` 时，每个条目都必须在 S 里存在，否则返回 `InvalidInput`（rule tracked_in_base）。
4. `H = inputs_hash(cfg, git, S)`。S 没有 `.git`，所以不过滤。`id = set_id(H, platform(), toolchain)`，回写到尝试行。键取 S 而不是 `inputs_from`，所以会话在复制过程中又改了文件也没关系：键永远是真正装进去的那组输入（D3）。
5. 【锁】，然后做 §5.3 的三项检查。命中其中任何一项时，先删 `.staging/<attempt>/`（还没跑 install，里面没有独有的数据），把尝试记为 refused，再按检查结果返回。第 2–4 步自己失败时（例如 `clone_tree` 返回 `CrossVolume`，或缺了安装输入），同样删暂存、记 refused，返回那个码。
6. 运行 install：
   - argv 来自第 1 步，cwd 是 S，stdin 是 `/dev/null`。
   - 环境取 daemon 的环境，删掉全部 `GIT_*`，PATH 换成 `env.path`。
   - 用 `setpgid` 自成一个进程组；stdout 与 stderr 都写进 `.staging/<attempt>/install.log`；把 pid 回写到尝试行。
   - 到 `env.timeout` 时，先 `killpg(SIGTERM)`，10 秒后再 `killpg(SIGKILL)`。
7. 非零退出、启动失败或超时：把尝试记为 failed，现场保留，释放锁，返回 `Unknown`。`detail = { "reason": "install_failed" 或 "timeout", "attempt", "staging", "command", "exit_code", "log_tail" }`，其中 `log_tail` 是日志最后 8 KiB。
8. 重算 `inputs_hash(cfg, git, S)`，它必须仍等于 H；不等返回 `Unknown`（reason inputs_modified）。这说明 install 改写了安装输入，比如漏了 `--frozen-lockfile`，装出来的东西不属于这个键。
9. 组装封存候选 `seal/`：对每个 `d ∈ deps_dirs`，若 S/d 存在，就 rename 进 `seal/d`（父目录按需创建）。S/d 不是目录返回 `Unknown`（reason not_directory）；一个依赖目录都没有产出返回 `Unknown`（reason no_deps_dirs）。然后删掉 `seal/` 里的 `nested_seed_caches`。
10. 校验 `seal/`：用 `apfs::walk` 遍历，只允许目录、普通文件和不逃出所在依赖目录的相对符号链接。否则返回 `Unknown`（reason special_file 或 symlink_escapes）。
11. 按 §5.3 封存：origin = build，`dirs` 取产出的依赖目录。返回 `DepsSet`。

第 6 步一旦开始，此后的任何失败都是 `Unknown`、现场保留（module-rulings 公共条：错误时序）。install 在 strata 之外有副作用（`~/.bun/install/cache`、网络），它的产出和日志是只此一份的证据。

复杂度：时间主要花在 install 上（tool 的 `bun install` 要几分钟）。deps 自己的开销是一次 `clone_tree`、两次 `inputs_hash`、一次遍历封存候选。

### 5.6 `ensure(ctx, tree)`

1. `cfg.deps_dirs` 为空时：`id = set_id(空输入常量, platform(), "")`。`find_set` 命中就返回；否则【锁】，再查一次，然后建一个空的封存目录与行（origin = empty），不建尝试行，也不跑任何命令。
2. `H = inputs_hash(cfg, git, tree)`，并检查每个条目都在 `tree` 里存在，否则 `InvalidInput`。`id = set_id(H, platform(), toolchain(cfg, env)?)`。
3. `find_set` 命中就返回。这是热路径，不拿锁。
4. 调 `adopt(ctx, tree)`。成功就返回。遇到 `Drift { reason: changed_during_clone }` 时再认领，连同第一次最多 3 次。遇到其它 `Drift`、`NoSuchPath` 或 `NotDirectory` 时转第 5 步。其它错误原样返回。
5. 调 `build(ctx, tree, None)` 并返回它的结果。

并发：两个 `ensure` 同时为同一个 id 走到第 4 或第 5 步时，由锁串行化；后到的那个拿到锁后会在 §5.3 的检查里看到已封存的依赖集，直接返回它。

### 5.7 `clone_into(paths, set, dst_root)`

1. `src = sealed_dir(paths, set.repo, set.id)`。`src` 不存在，或某个 `src/d` 不存在：返回 `Corrupt`（表里有行而封存目录坏了；types.md 的 `Corrupt` 原指 store 数据损坏，这里把它扩展到 deps 的封存区，T15 补进表里）。
2. 先检查全部 `dst_root/d`：任何一个已存在就返回 `Exists`，什么都不写。
3. 对每个 `d ∈ set.dirs` 依次执行 `apfs::clone_tree(src/d, dst_root/d)`，父目录按需创建。克隆出来的目录 mode 保持 0755，归调用方所有、可写。第一次克隆之后的任何失败都返回 `Unknown`，已克隆的部分保留（types.md §6.3 末段）。

`clone_into` 只往调用方新建的目录里写：新会话的基线缓存，或者 `move_workspace` 在工作区旁边准备的临时目录，从不往活的工作区里写。种子缓存由 session 在这之后另行从主 checkout 克隆（`nested_seed_caches` 告诉它是哪些）。

### 5.8 `drift(cfg, git, ws, pinned)`

`H = inputs_hash(cfg, git, ws)`。`H == pinned.inputs_hash` 时返回 `None`，否则返回 `Some(Drift { pinned: pinned.id, pinned_inputs: pinned.inputs_hash, found_inputs: H })`。

- 只比安装输入，不比依赖目录的内容（design §3），所以 dev-link 写 `node_modules/@inter/*`、`bun` 往 `.bin` 加东西都不算漂移。也不比 toolchain 与 platform：工具链变了会在 `observe_main` 里变成一条 from_inputs 等于 to_inputs 的提议。
- 对主干 tip 的导出目录调用（没有 `.git`），就是"会话的依赖集与主干当前的依赖集比较"（module-rulings T8）。
- 调用时机由调用方决定：status 时调用，或者 daemon 在 checkpoint 之后、overlay 里有路径让 `is_input` 为真时调用。

### 5.9 `propose(ctx, args)` 与 `observe_main(ctx, tree, head, set)`

`propose`：

1. 校验：`to ≠ from`；`tree` 与 `head` 同为 Some 或同为 None；同为 None 时 origin 必须是 Session；同为 Some 时 `git.tree_of(head)` 必须等于 `tree`。前几项不满足返回 `InvalidInput`；head 不在 object store 里时，`NoSuchCommit` 原样透传。
2. 在一个 store 写事务里完成，顺序要紧：先取代，再插入或提升，免得撞上 §4.2 的两个唯一索引。要插入新行时，先从 `sqlite_sequence` 取下一个 id N（表用 `AUTOINCREMENT`，不能用 `MAX(id) + 1`，否则 gc 删掉最大的行之后 id 会被复用）。被取代的行写 `superseded_by = N`，外键延迟到提交时检查；最后以显式的 id N 插入新行。
   - **会话发起、没有目标**：如果该会话在这个 repo 已有 AwaitingPublish，且 to 相同，就返回它（幂等）；to 不同，就把旧的记为 `Superseded { by: N }`。然后插入一条新的 AwaitingPublish。
   - **带目标**：
     - 如果已有 Pending 的 (to, tree) 与这次相同，就返回它（幂等）。
     - 否则，如果有 to 相同的 AwaitingPublish，取最早的那条提升：填上 tree 与 head，状态改为 Pending，origin 保持 Session。其余 to 相同的 AwaitingPublish 记为被它取代。
     - 否则插入一条新的 Pending，origin 取参数里的。
     - 该 repo 原有的另一条 Pending 记为 `Superseded { by: 本条 }`。
3. 返回提议。`ProposalId` 由 store 分配，从 1 起递增，不超过 `i64::MAX`。

`propose` 不建依赖集：`to` 可以还没封存，这时 `plan_rollout` 把会话标成 NotReady，等 `ensure` 建好它。

`observe_main`：调用方传入新会话此刻会钉住的那个 tip，即 session.md 按 design §6.7 和 `pin_unpushed` 算出的 (tree, head)，以及 `ensure` 为它找到的依赖集。

1. `set.repo` 必须等于 `ctx.repo`，否则返回 `InvalidInput`。
2. 在同一个写事务里读出 `deps_main` 的旧行，然后写入新的 (tree, head, set.id, 当前时间)。
3. 没有旧行（第一次观察），或者旧行的 set_id 等于 `set.id`：返回 `None`。
4. 否则以 `ProposalOrigin::Main` 调用 `propose`：from 取旧依赖集与它的 inputs_hash，to 取 set，目标是 (tree, head)。返回这条提议，它可能是新插入的，也可能是被提升的。

### 5.10 `plan_rollout`、`plan_switch` 与 `record_rollout`

`plan_rollout(ctx, id, sessions, complete)` 只读，什么都不写：

1. `P = get_proposal(id)`。`P.state` 不是 Pending 时返回 `NoSuchProposal`，`detail = { "proposal", "state" }`。`P.repo` 不等于 `ctx.repo`，或某个会话的 `base.repo` 不等于 `P.repo`，返回 `InvalidInput`。
2. 目标是 `BaseGen { repo: P.repo, tree: P.tree, deps: P.to }`，head 是 `P.head`。`ready = find_set(P.repo, P.to).is_some()`。读出该 repo 全部 AwaitingPublish 提议的提出会话。
3. 对 `sessions` 里的每个 (s, ws)，按顺序判定，第一个成立的条件决定这一步：
   1. `!ws.tracked` → `Skip Untracked`
   2. `ws.base.deps == P.to` → `Skip AlreadyThere`
   3. `!ready` → `Skip NotReady`
   4. s 是另一条 AwaitingPublish 提议 Q 的提出者 → `Skip OwnChange { proposal: Q.id }`
   5. 先 `pinned = find_set(ws.base.deps)`：缺失时记 `Skip Error { Corrupt }`。再算 `drift(cfg, git, ws.path, pinned)`：出错记 `Skip Error`；为 `Some(d)` → `Skip Drift { drift: d }`
   6. 查 `ws.path` 下 `cfg.deps_dirs`、`pinned.dirs`、`to` 的 dirs 三者并集里每个存在的目录上打开的文件。枚举出错记 `Skip Error`；有打开的文件 → `Skip OpenFiles { files }`
   7. 否则 → `Move { session: s, new_base: 目标, head: P.head }`
4. 第 3 步第 6 小步对所有会话只做一次进程扫描（§5.11 的批量形式），按目录前缀分给各会话。
5. 返回 `RolloutPlan { proposal: id, repo, complete, steps }`。`complete` 由调用方给出：没有 `--sessions` 收窄、`sessions` 就是该 repo 的全部会话时，取 true。

`plan_switch(ctx, id, s, ws)` 只给提出者用，不需要人类确认（D7 之后的说明；design §2 目标 3 [用户]）：

1. `P = get_proposal(id)`。`P.origin` 不是 `Session { s }`，或 P 的状态既不是 AwaitingPublish 也不是 Pending：返回 `NoSuchProposal`，`detail = { "proposal", "state", "reason": "not_origin" 或 "not_open" }`。从这个会话的角度看，它手里没有这样一条可以自换的提议，所以借用这个码。
2. 目标：AwaitingPublish 时是 `BaseGen { tree: ws.base.tree, deps: P.to }`，head 用 `ws.head`；Pending 时是 `BaseGen { tree: P.tree, deps: P.to }`，head 用 `P.head`。
3. 按顺序判定：tracked、AlreadyThere（`ws.base` 已等于目标）、NotReady；然后要求 `inputs_hash(ws) == P.to_inputs`，否则 `Skip Drift { pinned: P.to, pinned_inputs: P.to_inputs, found_inputs: 实际值 }`，说明工作区已经不是这个依赖集当初对应的那组输入；再查打开的文件；都通过就 Move。
4. 返回一步的计划，`complete = false`。

这不给 agent 任何新的权力：会话本来就拥有自己依赖目录的克隆，可以直接在里面 `bun install`。`plan_switch` 只是让它换成封存好、别人也会用的那一份，并且同样不在 dev server 脚下换。

`record_rollout(store, plan, moved)`：

1. `P = get_proposal(plan.proposal)`。`P.state` 不是 Pending（自换时的 AwaitingPublish，或者在计划与记录之间被取代了）：原样返回 P。
2. 同时满足三个条件时，在写事务里把 P 记为 Applied：`plan.complete` 为真；每个 Move 的会话都在 `moved` 里；每个 Skip 的原因都只是 Untracked 或 AlreadyThere。否则不改。
3. 返回 P。

### 5.11 `open_files(dir)`

1. 用 `O_RDONLY | O_DIRECTORY | O_NOFOLLOW` 打开 `dir`。`ENOENT`、`ENOTDIR`、`ELOOP` 时返回空列表：不存在的目录不会被交换；符号链接被交换时，它指向的东西不受影响。打开成功后，用 `fcntl(F_GETPATH)` 取内核给出的规范路径 C，然后关闭。
2. 用 `proc_listallpids` 列出全部 pid；缓冲区满时加倍重试。
3. 对每个 pid：`ESRCH`（进程已退出）和 `EPERM`（别的 uid 的进程，看不到）跳过；其它错误整个调用返回 `Unknown`，`detail` 带 errno。
   - `proc_pidinfo(PROC_PIDVNODEPATHINFO)` 给出 cwd 与根目录，在 C 下的记 `Cwd`。
   - `proc_pidinfo(PROC_PIDLISTFDS)` 列出 fd；对每个 vnode 类型的 fd，`proc_pidfdinfo(PROC_PIDFDVNODEPATHINFO)` 给出路径，在 C 下的记 `Fd`。
   - 从地址 0 开始，逐段调用 `proc_pidinfo(PROC_PIDREGIONPATHINFO, 地址)`，带 vnode 路径、且路径在 C 下的段记 `Mapped`。dlopen 加载的 `.node` 原生模块不占 fd，只能这样看到。
4. "在 C 下"的定义：路径等于 C，或者以 `C + "/"` 开头，按字节比较。两边都来自内核，大小写一致。`node_modules2/x` 不算在 `node_modules` 下。
5. 按 (pid, path, hold) 去重，按 (pid, path) 排序，用 `proc_name` 填 command。

局限：别的 uid 的进程看不到；dev server 都以用户身份运行，满足需要。`plan_rollout` 用的批量形式扫描一次进程，同时对多个目录前缀做匹配。

### 5.12 调用方的顺序（契约）

- **`deps approve <id> [--sessions …]`**，由 daemon 执行：
  1. `get_proposal`：不是 Pending 就返回 `NoSuchProposal`，不弹认证框。
  2. 人类确认：cli 先 `require_tty`；daemon 推出 `Action::DepsApprove { proposal: id }`，经 confirm 签发与校验后，再核对 action。
  3. 如果 `to` 还没封存，就对目标 tree 的基线导出调 `ensure`。这一步在 repo 写锁之外做，冷装要几分钟，不能让同一 repo 的 checkpoint 因此 `Busy`。
  4. 拿 repo 写锁，再 `get_proposal` 一次：认证期间它可能已被取代，这时返回 `NoSuchProposal`。
  5. `plan_rollout`。
  6. 对每个 Move 调 `session::move_workspace(session, <RepoId>, new_base, head)`；它先 checkpoint，再在旁边准备，最后 rename 交换，交换前复查打开的文件。
  7. `record_rollout(plan, 成功移过的会话)`。
  8. 返回每个会话的结果。Skip 的 `kind` 与某个 `Code` 同名时（`OpenFiles`、`Drift`），它就是该条目在输出里的码（types.md §7："多会话 deps approve 里某个会话的 OpenFiles … 出现在 data 里对应的条目上"）；形状由 cli.md 规定。
- **`deps propose`**（由会话发起），由 daemon 执行：
  1. 先算 `drift`：没有漂移就不必提议。
  2. 有漂移就调 `build(ctx, 会话的基线缓存, Some(工作区))`，在 repo 写锁之外做。
  3. `propose(Session { s }, from: base.deps, to: 建好的 id, 无目标)`。
  4. 提出者想先用新依赖时调 `plan_switch`，再 `move_workspace`。cli 怎样暴露这一步由 cli.md 决定。
- **发布后 rebase**，即 publish 的 `rebase_session`：
  1. 对新基线的导出调 `inputs_hash`，再用 `set_id` 算出新基线需要的依赖集 id。
  2. id 等于会话当前的 `base.deps` 就直接移动（提出者自换过的就属于这种）。
  3. 不等时调 `propose(Main 或提升, 目标 = (链顶 tree, head))`，会话原地等待。
  4. 新会话会钉的 tip 前进了，再调 `observe_main`。
- **`session::new`**：对每个兄弟仓库，导出 tip，然后 `ensure`，再 `clone_into` 基线缓存，最后 `observe_main`。种子缓存（包括 `nested_seed_caches`）之后再从主 checkout 克隆进工作区。
- **status 与 stderr 提示**：用 `open_proposals`，每次 CLI 调用的 stderr 只提示 Pending 的提议（design §6.4 第 4 步）；用 `attempts` 报出现场（不变量 6）；用 `drift` 报出各工作区的漂移；等待中的会话按 §4.5 的规则显示。

### 5.13 复杂度与验收数字

| 操作 | 复杂度 | 目标（首个 spike 实测后修正，同 design §8） |
| --- | --- | --- |
| `inputs_hash`、`drift` | O(输入字节 + 目录成员) | tool 小于 20 ms |
| `ensure` 热路径（依赖集已封存） | 一次 `inputs_hash`、一次缓存命中的 `toolchain`、一次 store 读 | 每个 repo 小于 50 ms，这样 design §8 的"新建会话 < 5 s（tool + 全部兄弟库）"才能成立 |
| `adopt` | 两次遍历加一次 `clone_tree`，O(依赖目录条目数) | 不超过同一目录 `cp -Rc` 用时的 1.5 倍加两次遍历的用时；只在第一代发生一次 |
| `build` | install 用时，加上 O(条目数) | 由 install 决定；deps 自己的开销小于同一目录 `cp -Rc` 用时的 2 倍 |
| `open_files` | O(进程数 + fd 数 + 映射段数)，扫描一次 | 本机约 800 个进程时小于 1 s |
| `plan_rollout` | O(会话数 × `inputs_hash`) 加一次进程扫描 | 10 个会话小于 2 s。design §8 的"依赖 rollout = 克隆 + rename，秒级"由 `move_workspace` 兑现 |
| `propose`、`observe_main`、`record_rollout` | 每次一个 store 写事务 | 小于 10 ms |

## 6. 失败与恢复

错误时序（module-rulings 公共条，design §9 不变量 6）：deps 对外可见的写入只有三种：

1. 封存时的 rename，以及 `deps_sets` 的行；
2. `deps_proposals`、`deps_main` 的行；
3. `clone_into` 写进调用方目录的克隆。

暂存目录、尝试行和锁文件是内部的，从不可发现。具名拒绝发生在对外可见的写入之前。冷装的 install 一旦开始，此后的失败一律是 `Unknown`，现场保留，因为 install 在外面有副作用，日志又是只此一份的证据。认领的 changed_during_clone 是唯一会删掉自己暂存的失败，理由见 §5.4。

| 失败 | 在哪一步 | Code | 留下的现场 | 恢复 |
| --- | --- | --- | --- | --- |
| 安装输入是 FIFO、socket 或设备文件 | `inputs_hash` | `NotFile` | 无写入 | 人修正 |
| 安装输入在基线 tree 里不存在 | `ensure`、`adopt`、`build` 的第一步 | `InvalidInput`（rule tracked_in_base） | 无写入 | 改配置，或提交那个文件 |
| `install` 切不开，或程序在 PATH 里找不到 | `toolchain` | `InvalidInput` 或 `NoSuchPath` | 无写入 | 修配置或 daemon 的 PATH |
| 主 checkout 的安装输入与基线不符 | `adopt` 第 3 步 | `Drift`（reason inputs） | 无写入 | `ensure` 改走冷装 |
| 主 checkout 没有依赖目录，或依赖目录不是目录 | `adopt` 第 6 步 | `NoSuchPath`、`NotDirectory` | 无写入 | `ensure` 改走冷装 |
| 主 checkout 的依赖目录里有特殊文件或逃出的符号链接 | `adopt` 第 7 步 | `Drift`（reason special_file、symlink_escapes） | 无写入 | `ensure` 改走冷装 |
| 克隆期间主 checkout 的依赖目录被写 | `adopt` 第 9 步 | `Drift`（reason changed_during_clone） | 暂存已删，尝试行为 refused | `ensure` 最多再认领两次，仍不行就冷装 |
| 跨卷 | `apfs::clone_tree` | `CrossVolume` | 克隆前就拒绝；冷装时只留下一条 refused 尝试行，暂存已删 | 人把 `paths.root` 放到同一卷 |
| 建造锁打不开 | §5.3【锁】 | 按 §6.3 归类（如 `IsDirectory`） | 无写入 | 人修锁的位置 |
| 同一依赖集上次失败或中断的现场还在 | §5.3 拿锁后的检查 | `Unknown`（reason previous_attempt） | 原现场不动；本次没跑 install | 人看现场，删掉那个暂存目录之后重来 |
| 有封存目录、没有行（孤儿） | §5.3 拿锁后的检查，或 rename 时 | `Unknown`（reason orphan） | 不动 | 人检查后删除 |
| install 非零退出，或启动失败 | `build` 第 7 步 | `Unknown`（reason install_failed） | `tree/` 与 `install.log` 保留，尝试行为 failed | 人看日志，删暂存后重来 |
| install 超时 | `build` 第 6–7 步 | `Unknown`（reason timeout） | 进程组被杀，暂存保留 | 同上 |
| install 改写了安装输入 | `build` 第 8 步 | `Unknown`（reason inputs_modified） | 保留 | 改 `install`（例如加 `--frozen-lockfile`） |
| install 没产出依赖目录，或产出的不是目录 | `build` 第 9 步 | `Unknown`（reason no_deps_dirs、not_directory） | 保留 | 改 `deps_dirs` 或 `install` |
| 封存候选里有特殊文件或逃出的符号链接 | `build` 第 10 步 | `Unknown`（reason special_file、symlink_escapes） | 保留 | 人看 |
| rename 之后写行失败，或 daemon 在两者之间崩溃 | §5.3 封存第 3 步 | `Unknown`（reason seal_failed），或下次 status 报 Interrupted | 封存目录在、行不在，不可发现 | 人删掉那个封存目录（status 给出路径） |
| daemon 或建造进程被杀 | 任何建造步骤 | 下次 status 报 Interrupted | 暂存保留；锁随持有者的死亡释放；残留的 install 只能写自己的暂存 | 同"上次失败的现场" |
| 磁盘满或其它 I/O 错误 | 克隆、install、封存 | `Unknown`（types.md §6.3） | 保留 | 人腾出空间，删暂存后重来 |
| 表里有行，封存目录却不在 | `clone_into` | `Corrupt` | 不动 | 人检查：封存区被外力改过 |
| `clone_into` 的目标已存在 | `clone_into` 第 2 步 | `Exists` | 无写入 | 调用方换一个目标 |
| `clone_into` 写到一半失败 | `clone_into` 第 3 步 | `Unknown` | 已克隆的部分保留 | 调用方按自己的现场规则报出 |
| `propose` 的参数自相矛盾 | `propose` 第 1 步 | `InvalidInput` | 无写入 | 调用方修正 |
| head 不在 object store 里 | `propose` 第 1 步 | `NoSuchCommit`（gitio） | 无写入 | — |
| 提议不存在，或不在可用状态 | `get_proposal`、`plan_rollout`、`plan_switch` | `NoSuchProposal` | 无写入 | — |
| repo 不在配置里 | daemon 建 `Ctx` 时（`Config::repo`） | `NoSuchRepo` | 无写入 | — |
| 判定某个会话时出错 | `plan_rollout` 第 3 步 | 该会话为 `Skip { Error }` | 无写入；其余会话照常 | 修好后再 approve |
| 依赖目录里有打开的文件 | `plan_rollout`、`plan_switch` | 该会话为 `Skip { OpenFiles }` | 目录一字不动 | 停掉 dev server 后再 approve |
| 计划之后、交换之前有文件被打开 | `session::move_workspace` 复查 | `OpenFiles`（session） | 按 session 的规则 | 再 approve |
| store 写事务失败 | `propose`、`observe_main`、`record_rollout`、封存 | store 的码 | 事务回滚，表不变；封存时见 seal_failed 一行 | — |

## 7. 测试清单

测试都在临时目录里进行：临时 Store、临时主 checkout（真实的 git 仓库）、`PATH` 上的假 `bun` 与假 install 脚本（它们把自己看到的 cwd、环境、调用次数写进文件）。测试从不读写 `/Volumes/GWD/inter`。

| 测试名 | 断言 | 来源（矩阵行 或 新增） |
| --- | --- | --- |
| `inputs_hash_ignores_manifest_version` | 同一棵树里，只把 `package.json` 顶层 `version` 从 1.0.87 改成 1.0.88：哈希不变；`composer.json` 顶层 `version` 同理；`package-lock.json` 的顶层 `version` 与 `packages[""].version` 一起改：不变；用 `jq` 重写（缩进、键序、末尾换行都变了）：不变；`dependencies` 加一项，或 `packages["node_modules/x"].version` 改了：变；解析不了、或顶层不是对象的 `package.json` 按原始字节处理，改一个字节就变，且不报错 | 新增 |
| `inputs_hash_covers_files_dirs_and_absence` | `bun.lock` 改一个字节：变；在工作区的 `patches/` 里加一个未跟踪、未被 ignore 的补丁：变；加一个被 ignore 的 `.DS_Store`：不变；只改文件 mode：不变；条目缺失不报错，哈希与存在时不同；空目录与不存在的目录哈希相同；没有 `.git` 的导出树与内容相同的 git 工作树哈希相同 | 新增 |
| `inputs_hash_never_follows_symlinks` | `bun.lock` 是指向工作区外文件的符号链接：哈希只取链接目标字符串，改被指向文件的内容不变，改链接目标就变；中间一级 `sidecar` 是符号链接时，`sidecar/package.json` 记为缺失；把被指向的文件 `chmod 000` 之后，`inputs_hash` 照样成功、结果不变，说明 deps 从不打开链接指向的东西 | 新增 |
| `inputs_hash_rejects_special_files` | 安装输入本身是 FIFO：返回 `NotFile`，`detail.path` 指向它；`patches/` 里有 FIFO 或 socket：返回 `NotFile`；FIFO 没有写端时，调用在 1 秒内返回，FIFO 从未被打开 | 新增 |
| `set_id_is_deterministic_and_unambiguous` | 同样的三段在两个进程里得到同一个 64 位小写十六进制 id；`("ab","c","")` 与 `("a","bc","")` 不同；只改 toolchain 或只改 platform 都不同；空安装输入的 `inputs_hash` 是固定常量 | 新增 |
| `toolchain_is_install_program_version` | PATH 上的假 `bun` 打印 `1.3.3`，`install = "bun install --frozen-lockfile"`：toolchain 是 `bun 1.3.3`；程序找不到：`NoSuchPath`，`detail` 带程序名与 PATH；没有 `install`：空串；把假 `bun` 换成打印 `1.3.4` 的新文件后，下一次调用得到 `bun 1.3.4`（缓存按程序文件的身份失效）；stdout 为空时取 stderr 的第一行 | 新增 |
| `open_files_reports_fd_mapping_and_cwd` | 在 dir 下起三个子进程，一个打开 `dir/a/x.js` 不关，一个 mmap `dir/b/y.node`，一个当前目录在 `dir/c`：`open_files(dir)` 恰好返回这三条，hold 依次为 `Fd`、`Mapped`、`Cwd`，pid 与路径正确，command 非空；打开同前缀的 `dir2/x.js` 的进程不出现；子进程退出后返回空；dir 不存在，或 dir 是符号链接：返回空列表 | 新增 |
| `build_seals_install_output_without_seed_caches` | 假 install 在 staging 里写出 `node_modules/x/index.js`、`node_modules/.cache/y`、`public/pdfium/z.wasm`：`build` 返回的 `DepsSet` 的 `dirs` 为 `[node_modules, public/pdfium]`，`inputs_hash` 等于 `inputs_hash(树)`，toolchain 是假程序的版本；封存目录里有 `x/index.js` 与 `z.wasm`，没有 `node_modules/.cache`；install 的 cwd 是 staging 树的仓库根，PATH 等于 `env.path`，stdin 是 `/dev/null`，环境里没有 `GIT_*`；`.staging/<attempt>/` 已删除；`find_set` 能找到它；封存根目录的 mode 是 0555 | 新增 |
| `build_failure_is_unknown_and_keeps_scene` | install 以 3 退出：返回 `Unknown`，`detail` 含 reason install_failed、attempt、staging、command、exit_code 3、log_tail；`tree/` 与 `install.log` 都在；`find_set` 为 None；`attempts` 把它列为 Failed；对同一依赖集再调 `ensure` 或 `build`，返回 reason previous_attempt 的 `Unknown`（带原 attempt），假 install 的调用次数不变；删掉暂存目录后再 `build`，install 重新运行 | 新增 |
| `build_timeout_kills_install_group` | `timeout` 为 1 s，install 先起一个睡 60 s 的孙进程，自己也睡：约 1 s 后返回 `Unknown`（reason timeout）；install 和孙进程都已不存在（`kill(pid, 0)` 得到 `ESRCH`）；暂存保留，`attempts` 为 Failed | 新增 |
| `seal_refuses_when_staging_does_not_match_key` | 四种 install 各跑一次：改写 `bun.lock`（安装输入）；一个依赖目录都不产出；在 `node_modules` 里放一个指向绝对路径的符号链接；在 `node_modules` 里放一个 FIFO。四次都返回 `Unknown`，reason 依次为 inputs_modified、no_deps_dirs、symlink_escapes、special_file；四次都没有出现 `<root>/deps/<repo>/<id>/`，`deps_sets` 没有行，`find_set` 为 None；暂存都保留并由 `attempts` 报出 | tgrep-core/src/generations/mod.rs::invalid_staged_generation_is_never_published |
| `killed_builder_leaves_undiscoverable_staging_and_free_lock` | 测试二进制以 worker 模式在子进程里跑 `build`，假 install 写出就绪标记后睡眠；看到标记就 SIGKILL 子进程的进程组。之后：非阻塞 flock 立即拿到建造锁；`find_set` 为 None，封存目录不存在；`attempts` 报出一条 Interrupted，带暂存路径；`ensure` 返回 reason previous_attempt 的 `Unknown`，不跑 install；删掉那个暂存目录后，`ensure` 重建成功，`find_set` 找得到 | tgrep-core/tests/generations.rs::killed_starter_releases_lock_and_leaves_only_undiscoverable_staging |
| `failed_or_lockless_builds_never_publish` | 手放一个没有行的 `.staging/999/` 垃圾目录：`find_set` 为 None，`attempts` 不列它；install 以 2 退出：没有依赖集；把 `.locks/<id>.lock` 换成目录：`build` 在跑 install 之前返回 `IsDirectory`，没有尝试行，也没有暂存；另一个已封存的依赖集前后遍历逐条相同 | tgrep-core/tests/generations.rs::interrupted_and_failed_publications_are_not_discoverable |
| `orphan_sealed_dir_is_unknown_not_overwritten` | 手建一个没有 `deps_sets` 行的 `<root>/deps/<repo>/<id>/node_modules/q`：`ensure` 与 `build` 都返回 `Unknown`（reason orphan，带路径）；该目录和 q 都不变；install 没有运行；`deps_sets` 仍然没有行 | 新增 |
| `clone_into_copies_set_dirs_privately` | `clone_into` 到新目录 dst：`set.dirs` 的每一项都在，内容逐字节相同，inode 不同；在 `dst/node_modules` 里写文件、删文件之后，封存目录前后遍历相同，另一份 `clone_into` 的结果也不变；`dst/node_modules` 已存在：返回 `Exists`，什么都没写；有行而封存目录被删：返回 `Corrupt` | 新增 |
| `adopt_matches_inputs_hash_else_refuses` | 主 checkout 的 `package.json`、`bun.lock` 与基线导出相同（只差 `package.json` 的 `version`），有 `node_modules/a/index.js`：`adopt` 成功，`inputs_hash` 等于 `inputs_hash(基线导出)`，`dirs` 为 `[node_modules]`，封存的 `a/index.js` 与主 checkout 的逐字节相同、inode 不同，主 checkout 依赖目录的遍历（含 gen count）前后相同。主 checkout 的 `bun.lock` 有一行未提交的改动：返回 `Drift`，`detail` 为 reason inputs 加两个哈希；没有尝试行、没有暂存、没有 `deps_sets` 行。主 checkout 没有 `node_modules`：返回 `NoSuchPath` | 新增 |
| `adopt_refuses_when_main_changes_during_clone` | 用 adopt 的测试钩子，在克隆之后、第二次遍历之前改写主 checkout 的 `node_modules/a/index.js`：返回 `Drift`，reason 为 changed_during_clone，path 为 `node_modules/a/index.js`；没有 `deps_sets` 行，暂存已删，尝试行为 refused；主 checkout 里留着测试写入的新内容，说明 deps 没有碰它。钩子改的是 `node_modules/.cache/…` 时，认领照常成功。`ensure` 遇到一次这种拒绝后再认领，第二次成功，install 调用次数为 0 | layerfs-project/tests/acquisition_custody.rs::a_changed_row_in_the_window_is_refused_by_its_path |
| `seed_cache_is_cloned_not_sealed` | `deps_dirs = [node_modules]`，`seed_caches = [node_modules/.cache, .next/cache]`，主 checkout 有 `node_modules/.cache/babel/x.json`：认领后封存的依赖集里没有 `node_modules/.cache`；`nested_seed_caches` 返回 `[node_modules/.cache]`，`.next/cache` 不嵌套所以不列；主 checkout 的 `.cache/babel/x.json` 字节与 gen count 都不变；`clone_into` 的结果里也没有 `.cache`，种子缓存由 `session::new` 从主 checkout 克隆（session.md）；冷装时 install 产出的 `node_modules/.cache` 同样不进封存 | layerfs-api/tests/merge_units.rs::two_patterns_make_one_unit |
| `ensure_prefers_sealed_then_adopt_then_build` | 依赖集已封存：`ensure` 不克隆、不跑 install、不建尝试行；没有封存而主 checkout 一致：认领，install 调用次数为 0；主 checkout 不一致、没有依赖目录、或依赖目录不是目录：冷装，install 调用次数为 1；每种情况返回的 id 都等于 `set_id(inputs_hash(树), platform(), toolchain)` | 新增 |
| `ensure_refuses_input_missing_from_base_tree` | `install_inputs` 含 `bunfig.toml` 而基线导出里没有：返回 `InvalidInput`，`detail` 为 repo、field install_inputs、index、rule tracked_in_base、path `bunfig.toml`；没有锁文件、没有尝试行、没有暂存 | 新增 |
| `concurrent_ensure_builds_once` | 两个线程同时对同一棵树 `ensure`，主 checkout 没有依赖目录、必须冷装：install 只运行一次；两边拿到相等的 `DepsSet`；只有一条 sealed 尝试 | 新增 |
| `empty_config_yields_empty_set` | `deps_dirs = []`：`ensure` 返回的依赖集 `dirs` 为空，toolchain 为空串，`inputs_hash` 为空输入常量；不跑任何命令；封存目录存在且为空；再调一次得到同一个 id，`deps_sets` 只有一行；`clone_into` 什么都不克隆 | 新增 |
| `deps_dir_writes_are_not_drift` | 工作区的 `node_modules` 是封存依赖集经 `clone_into` 得到的克隆。模拟 dev-link 写入 `node_modules/@inter/hr-core/src/a.ts`，在 `node_modules/.bin` 下加文件，再删掉一个包：`drift` 为 None，封存目录前后遍历相同；随后改 `package.json` 的 `dependencies`：`drift` 为 Some。依赖目录整个归会话所有，不存在按路径的合并 | layerfs-api/tests/merge_units.rs::a_unit_one_side_changed_merges_as_today |
| `drift_compares_install_inputs_only` | 工作区的 `bun.lock` 改了：返回 `Some(Drift)`，`pinned` 为所钉 id，`pinned_inputs` 为它记录的哈希，`found_inputs` 为新哈希；假 `bun` 换了版本而输入没变：None；对主干 tip 的导出目录（没有 `.git`）调用，结果与在内容相同的工作区上调用一致 | 新增 |
| `observe_main_files_proposal_when_main_set_changes` | 第一次 `observe_main`：返回 None，`deps_main` 记下 (tree, head, set)；新 tip 的依赖集相同：None，只更新 tree 与 head；新 tip 的依赖集不同：一条 Pending，origin 为 Main，from 与 from_inputs 取上一个主干依赖集，to、to_inputs、tree、head 取新的；再来一个不同的依赖集：新的一条 Pending，前一条变成 `Superseded { by: 新 id }`；任何时刻该 repo 都只有一条 Pending | 新增 |
| `propose_validates_and_is_idempotent` | 同样的参数调两次：同一个 `ProposalId`，只有一行；from 等于 to：`InvalidInput`；只给 tree 不给 head，或 origin 为 Main 却没有目标：`InvalidInput`；`tree_of(head)` 不等于 tree：`InvalidInput`；head 不在 object store 里：`NoSuchCommit`；同一会话再提一个不同的 to：它之前的 AwaitingPublish 变成 Superseded；`ProposalId` 从 1 起递增 | 新增 |
| `publish_promotes_awaiting_proposal` | 会话 S 有一条 AwaitingPublish 提议 P（to 为 D）。带目标 tree 与 head、to 同为 D 的 `propose`（模拟 `rebase_session` 或 `observe_main` 的调用）返回 P 的 id：状态变为 Pending，origin 仍是 `Session { S }`，tree 与 head 已填上；原有的 Pending 被它取代 | 新增 |
| `approve_of_non_pending_proposal_is_no_such_proposal` | 不存在的 id：`NoSuchProposal`；AwaitingPublish、Applied、Superseded 的提议交给 `plan_rollout`：`NoSuchProposal`，`detail.state` 是当前状态；`get_proposal` 对存在的提议，任何状态都返回它 | 新增 |
| `rollout_skips_session_with_open_files` | 准备：Pending 提议 P，目标 `BaseGen { tool, T2, D2 }`，head H2，D2 已封存；会话 S1、S2 都钉 (T1, D1)、tracked，工作区的 `node_modules` 都是 D1 经 `clone_into` 得到的克隆，安装输入未改；起一个子进程打开 S1 的 `node_modules/next/dist/server/next.js` 不关。断言：`plan_rollout(P, [S1, S2], complete = true)` 的 steps 依次为 `Skip { S1, OpenFiles { files: [{ pid: 子进程, path: …/node_modules/next/dist/server/next.js, hold: Fd }] } }` 与 `Move { S2, BaseGen { tool, T2, D2 }, H2 }`；Skip 的 JSON 带 `"kind":"OpenFiles"` 以及 pid 与 path；计划前后，S1 依赖目录的遍历（路径、大小、inode、gen count）逐条相同，store 也前后相同；把计划交给测试里记录调用的 mover（daemon 循环的替身），只有 S2 被调用、且只调用一次；`record_rollout(plan, moved = [S2])` 之后 P 仍是 Pending。子进程退出后，`plan_rollout(P, [S1])` 对 S1 给出 Move。把持有方式换成 mmap（Mapped）和当前目录在 `node_modules` 里（Cwd），各跑一遍，结论相同。交换前的复查由 `session::move_workspace` 负责（session.md 的测试） | 新增 |
| `rollout_moves_base_and_deps_atomically` | 准备同上，但没有打开的文件。断言：`plan_rollout` 的每个 Move 带的 `new_base` 恰好是 `BaseGen { repo: tool, tree: T2, deps: D2 }`，tree 与 deps 取自同一条提议；head 为 H2，且 `gitio::tree_of(H2)` 等于 T2；计划里只有 Move 与 Skip 两种步骤，不存在只换依赖或只换 tree 的步骤；记录调用的 mover 对每个 Move 恰好收到一次 (session, new_base, head)，对 Skip 从不被调用；mover 对 S2 报失败（模拟 `move_workspace` 返回 `OpenFiles`）时，`record_rollout(moved = [S1])` 让 P 保持 Pending，再 approve 时 S1 为 AlreadyThere、S2 为 Move；全部移过之后 P 为 Applied。交换本身的原子性（先 checkpoint、在旁边准备、rename 交换）由 session.md 的 `move_workspace` 测试断言 | 新增 |
| `proposing_session_may_use_new_deps_before_approval` | 准备：会话 S 钉 (T1, D1)，在工作区的 `package.json` 里加了一个依赖；会话 S2 也钉 (T1, D1)。断言：`drift(S)` 为 Some；`build(S 的基线缓存, Some(S 的工作区))` 封存出 D_new，其 `inputs_hash` 等于 S 工作区的哈希；`propose(origin = Session { S }, from = D1, to = D_new, 无目标)` 得到 AwaitingPublish；`plan_switch(P, S)` 恰好是一步 `Move { S, BaseGen { tool, T1, D_new }, S 的 head }`，全程没有 `ConfirmToken` 参与（`plan_switch` 的签名不收凭据）；`plan_switch(P, S2)` 返回 `NoSuchProposal`（不是提出者）；`plan_rollout(P, [S2])` 返回 `NoSuchProposal`，`detail.state` 为 AwaitingPublish，别的会话在发布并确认之前不动；S 的依赖目录被打开时，`plan_switch` 给出 Skip OpenFiles；S 又改了 `bun.lock` 后，`plan_switch` 给出 Skip Drift，其中的哈希不等于 to_inputs。发布后，带目标的 `propose` 把 P 变成 Pending，这时 `plan_rollout(P, [S2])` 对 S2 给出 Move | 新增 |
| `plan_rollout_skip_reasons_in_order` | 对同一条 Pending 提议构造七个会话：tracked = false；`base.deps` 已是 to；是另一条 AwaitingPublish 的提出者；工作区改了 `bun.lock`；依赖目录被打开；`open_files` 被注入失败；什么都没有。依次得到 Untracked、AlreadyThere、`OwnChange { 那条提议 }`、Drift、OpenFiles、`Error { code: Unknown }`、Move；同时满足多个条件的会话按 §5.10 的顺序取第一个；to 没封存时，前两种以外的会话都是 NotReady；`plan_rollout` 什么都不写，store 与各目录前后相同 | 新增 |
| `record_rollout_applies_only_complete_successful_plans` | `complete = true`、每个 Move 都在 moved 里、其余 Skip 只有 Untracked 或 AlreadyThere：提议变为 Applied。出现 OpenFiles、Drift、NotReady、Error 的 Skip，或某个 Move 不在 moved 里，或 `complete = false`：仍是 Pending。`plan_switch` 的计划（`complete = false`）从不改状态；提议已被取代时，`record_rollout` 原样返回它 | 新增 |

矩阵覆盖：目标模块为 deps 的 5 行全部落在上表（3d `merge_units.rs` 的两行，3f `generations` 的三行）；理由点名 deps 的 `—` 行有 1 行（3b `acquisition_custody.rs`），也落在上表。上表共 33 条：6 条引用矩阵行，27 条新增。

## 8. 实现任务（TDD）

`tests/common/mod.rs` 放共用的夹具：

- 临时 Store；
- 临时主 checkout，是一个真实的 git 仓库，带提交好的 `package.json`、`bun.lock`、`patches/`；
- 基线导出；
- 假 `bun` 与假 install 脚本，可以控制退出码、睡眠、输出，并把调用次数、cwd、环境写进文件；
- 记录调用的 mover。

全部放在同一个临时目录下，保证同卷。

### Task 1: 安装输入哈希与依赖集 id

**Files:**
- Create: `crates/strata-deps/Cargo.toml`、`crates/strata-deps/src/lib.rs`、`crates/strata-deps/src/inputs.rs`、`crates/strata-deps/tests/common/mod.rs`
- Test: `crates/strata-deps/tests/inputs.rs`

**Interfaces:**
- Consumes: `RepoConfig`（config.md）、`RepoPath`、`DepsSetId`、`StrataError`；`apfs::{walk, open_rooted}`；`gitio::Git::is_ignored`
- Produces: `inputs_hash`、`set_id`、`platform`、`toolchain`、`InstallEnv`、`is_input`、`nested_seed_caches`

- [ ] **Step 1: 写失败的测试**：`inputs_hash_ignores_manifest_version`、`inputs_hash_covers_files_dirs_and_absence`、`inputs_hash_never_follows_symlinks`、`inputs_hash_rejects_special_files`、`set_id_is_deterministic_and_unambiguous`、`toolchain_is_install_program_version`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-deps --test inputs`
Expected: FAIL（这些函数还不存在，编译失败）

- [ ] **Step 3: 写让测试通过的最少实现**：
  - §4.3 的两种编码，以及 §4.4 的规范化：显式地递归排序键，不依赖 `preserve_order`；
  - §5.1 的解析：每级 `O_NOFOLLOW`，用属性判断类型，从不打开 FIFO，目录成员过 ignore 过滤，空目录等同缺失；
  - §5.2 的 toolchain：按程序文件的身份缓存；
  - `nested_seed_caches` 用 `RepoPath` 的前缀关系判断。
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
- Produces: `open_files`、`OpenFile`、`Hold`，以及供 `plan_rollout` 用的批量扫描（crate 内部）

- [ ] **Step 1: 写失败的测试**：`open_files_reports_fd_mapping_and_cwd`。mmap 的子进程用测试二进制的 worker 模式（`std::env::current_exe()` 加一个环境变量），不依赖外部解释器
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-deps --test open_files`
Expected: FAIL（`open_files` 还不存在，编译失败）

- [ ] **Step 3: 写让测试通过的最少实现**：按 §5.11 扫描 fd、映射段和 cwd，用 `F_GETPATH` 取前缀，按字节比较前缀边界；`ESRCH`、`EPERM` 跳过，其它错误返回 `Unknown`；批量形式只扫描一次进程
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
- Consumes: Task 1；`apfs::{clone_tree, clone_file, walk}`；store 的写事务；§4.2 的 `deps_sets` 与 `deps_builds`
- Produces: `Ctx`、`sealed_dir`、`find_set`、`build`、`clone_into`、`attempts`、`BuildAttempt`、`BuildKind`、`BuildState`

- [ ] **Step 1: 写失败的测试**：`build_seals_install_output_without_seed_caches`、`build_failure_is_unknown_and_keeps_scene`、`build_timeout_kills_install_group`、`seal_refuses_when_staging_does_not_match_key`、`killed_builder_leaves_undiscoverable_staging_and_free_lock`、`failed_or_lockless_builds_never_publish`、`orphan_sealed_dir_is_unknown_not_overwritten`、`clone_into_copies_set_dirs_privately`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-deps --test build`
Expected: FAIL（`build`、`clone_into` 等还不存在，编译失败）

- [ ] **Step 3: 写让测试通过的最少实现**：
  - §4.1 的布局；
  - §5.3 的锁（`FD_CLOEXEC`，锁打不开就报错）、拿锁后的三项检查、`0555` + rename + `F_FULLFSYNC` + 写行的顺序；
  - §5.5 的冷装：环境删 `GIT_*`，自成进程组，超时先 SIGTERM 再 SIGKILL，日志尾部 8 KiB，install 前后核对哈希，校验封存候选；
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
- Consumes: Task 1、Task 3；`apfs::{clone_tree, walk}`（gen count）
- Produces: `adopt`；一个 `#[doc(hidden)]` 的测试钩子入口，在克隆之后、第二次遍历之前回调

- [ ] **Step 1: 写失败的测试**：`adopt_matches_inputs_hash_else_refuses`、`adopt_refuses_when_main_changes_during_clone`、`seed_cache_is_cloned_not_sealed`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-deps --test adopt`
Expected: FAIL（`adopt` 还不存在，编译失败）

- [ ] **Step 3: 写让测试通过的最少实现**：按 §5.4，具名拒绝都在写入之前；两次遍历都跳过嵌套的种子缓存；W1 与 W2 不同时删掉自己的暂存、把尝试记为 refused，返回 `Drift`（reason changed_during_clone）；然后按 §5.3 封存
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
- Produces: `ensure`

- [ ] **Step 1: 写失败的测试**：`ensure_prefers_sealed_then_adopt_then_build`、`ensure_refuses_input_missing_from_base_tree`、`concurrent_ensure_builds_once`、`empty_config_yields_empty_set`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-deps --test ensure`
Expected: FAIL（`ensure` 还不存在，编译失败）

- [ ] **Step 3: 写让测试通过的最少实现**：按 §5.6：热路径不拿锁；changed_during_clone 时最多认领 3 次；其余认领拒绝改走冷装；空依赖集在锁内建空目录与行
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
- Consumes: store 的写事务；§4.2 的 `deps_proposals` 与 `deps_main`；`gitio::Git::tree_of`
- Produces: `Proposal`、`ProposalOrigin`、`ProposalState`、`ProposeArgs`、`propose`、`observe_main`、`get_proposal`、`open_proposals`

- [ ] **Step 1: 写失败的测试**：`observe_main_files_proposal_when_main_set_changes`、`propose_validates_and_is_idempotent`、`publish_promotes_awaiting_proposal`、`approve_of_non_pending_proposal_is_no_such_proposal`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-deps --test proposals`
Expected: FAIL（提议的类型与函数还不存在，编译失败）

- [ ] **Step 3: 写让测试通过的最少实现**：
  - §5.9 的校验，以及去重、提升、取代；
  - 在一个写事务里先取代再插入；
  - 两个唯一索引；
  - `ProposalId` 的上限按 types.md §3。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-deps --test proposals`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-deps/src/proposal.rs crates/strata-deps/src/lib.rs crates/strata-deps/tests/proposals.rs
git commit -m "feat(deps): deps change proposals and main tracking"
```

### Task 8: rollout 计划

**Files:**
- Create: `crates/strata-deps/src/rollout.rs`
- Test: `crates/strata-deps/tests/rollout.rs`

**Interfaces:**
- Consumes: Task 2（批量扫描）、Task 3、Task 6、Task 7；`WorkspaceState`、`BaseGen`
- Produces: `RolloutPlan`、`RolloutStep`、`SkipReason`、`plan_rollout`、`plan_switch`、`record_rollout`

- [ ] **Step 1: 写失败的测试**：`rollout_skips_session_with_open_files`、`rollout_moves_base_and_deps_atomically`、`proposing_session_may_use_new_deps_before_approval`、`plan_rollout_skip_reasons_in_order`、`record_rollout_applies_only_complete_successful_plans`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-deps --test rollout`
Expected: FAIL（计划的类型与函数还不存在，编译失败）

- [ ] **Step 3: 写让测试通过的最少实现**：
  - §5.10 的判定顺序；
  - 对所有会话只做一次进程扫描；
  - 单个会话出错记 `Skip { Error }`；
  - `plan_switch` 只给提出者用，并要求工作区的哈希等于 `to_inputs`；
  - `record_rollout` 只在计划完整、且全部成功时记 Applied；
  - 计划不写任何东西。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-deps --test rollout`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-deps/src/rollout.rs crates/strata-deps/src/lib.rs crates/strata-deps/tests/rollout.rs
git commit -m "feat(deps): rollout plans with open-file skips"
```

## 9. 构建者决定记录

design.md 标为 [交给构建者]、并且落在本模块的决定：

| 事项 | 选择 | 理由 | 日期 |
| --- | --- | --- | --- |
| 依赖集键是否含工具链版本（design §4、§12） | 含（types.md 已定）。采集方式：`<install 的程序名> <它的 --version 输出的第一行>`；没有 `install` 时为空串；按程序文件的身份缓存 | 程序自己报的版本最便宜，人也看得懂；对整个二进制求哈希太脆，换一次签名就变 | 2026-10-22 |
| 封存目录的只读手段（design §7、§12） | 靠约定，外加封存根目录 mode 0555；不加 `chflags uchg` | 依赖目录要被克隆成会话可写的副本。`clonefile` 会不会把 `UF_IMMUTABLE` 一起克隆过去，还没有实测；如果会，工作区里的 `node_modules` 就不可写，dev-link 与 `bun` 都会坏。spike 证实不继承之后，可以改成 uchg | 2026-10-22 |
| generation 保留期（design §7、§12） | 不在本模块：由 store 的 gc 规定。deps 只给出引用规则（§4.6） | 回收属于 store | 待定（store.md） |

本模块自己的决定。下表每一行都是 [我的决定]；正文只在最容易引起争议的两处（§5.1、§5.4）另外标出：

| 事项 | 选择 | 理由 | 日期 |
| --- | --- | --- | --- |
| package manifest 忽略的字段（module-rulings T8） | 见 §4.4：`package.json`、`composer.json` 的顶层 `version`；`package-lock.json`、`npm-shrinkwrap.json` 的顶层 `version` 与 `packages[""].version`；规范化 JSON 后再求摘要 | 发布时改版本号不应产生新依赖集；`jq` 与 prettier 改格式也不应 | 2026-10-22 |
| 安装输入目录的成员 | 未被 ignore 的文件（不只是被跟踪的） | 见 §5.1；与 config.md §2.3 的措辞要在 T15 对齐 | 2026-10-22 |
| 两种哈希的编码 | 见 §4.3：derive_key 的上下文串，加长度前缀 | 不同输入不会拼出同一串；改规则就换版本串 | 2026-10-22 |
| `platform` 的取法 | 编译期三元组 | strata 只在 macOS 上跑，不需要运行期探测 | 2026-10-22 |
| install 的时限 | 60 分钟，超时杀进程组 | 冷装要几分钟；挂死的 install 不能让建造锁永远被占着 | 2026-10-22 |
| install 的环境 | daemon 的环境删掉 `GIT_*`，PATH 取 `InstallEnv.path`，stdin 是 `/dev/null`，自成进程组 | 同 gitio 的净化理由；daemon 没有 TTY | 2026-10-22 |
| 失败的冷装 | 不自动重来；现场在就返回 previous_attempt，人删掉现场才会重建 | 不变量 6"不重放"；也免得每次 `new` 都重跑一次要几分钟、还会失败的 install | 2026-10-22 |
| 建造锁 | 每个 (repo, id) 一个 flock，带 `FD_CLOEXEC`，不被 install 继承；锁打不开就报错 | 锁随 daemon 的死亡释放（3f 的 killed_starter 行）；从不无锁建造（3f 的 interrupted 行） | 2026-10-22 |
| 认领前后的一致性检查 | 两次遍历，按 gen count 等逐条比较；不同就删掉自己的暂存，返回 `Drift`，`ensure` 最多认领 3 次 | 目录级的 `clonefile` 不是快照；主 checkout 的 dev-link 随时在写 | 2026-10-22 |
| 依赖集必须自包含 | 不允许特殊文件和逃出的符号链接 | 否则克隆进工作区后会指回主 checkout（不变量 2），或者指向已删除的 staging | 2026-10-22 |
| 提议的状态与取代 | 四种状态（§4.5）；每个 repo 一条 Pending；目标设定之后不改 | 只有最新的主干目标值得批准；"批准的就是看到的那一条" | 2026-10-22 |
| 提出者自换 | `plan_switch` 不需要确认，但要求工作区的哈希等于 `to_inputs`，并检查打开的文件 | design §2 目标 3、§6.4 第 3 步 [用户]；不给 agent 新的权力 | 2026-10-22 |
| approve 的候选与跳过顺序 | tracked 且 `base.deps ≠ to` 的会话都是候选，不限于钉着 `from` 的；跳过的判定顺序固定（§5.10） | 落后不止一代的会话也该能一步追上主干；有自己依赖改动的会话（OwnChange、Drift）换过去会错位；顺序固定，报告才可预期 | 2026-10-22 |
| 主干的跟踪 | `deps_main` 每个 repo 一行，由 `observe_main` 更新，依赖集变了就登记 origin = Main 的提议 | design §6.4 第 1 步"主干的锁文件变了"要有人发现；module-rulings T8 要求漂移与"主干当前的依赖集"比较 | 2026-10-22 |
| 空依赖集 | `deps_dirs` 为空的仓库也有一个依赖集：输入为空、toolchain 为空串、目录为空 | `BaseGen.deps` 不是 `Option`，所有仓库走同一条路 | 2026-10-22 |
| 依赖集的键与布局 | 表与目录都按 (repo, id) 分开 | `set_id` 不含 repo（types.md），不同仓库的 id 可能相同 | 2026-10-22 |
| `clone_into` 归 deps | 封存区的布局只有 deps 知道，克隆进新目录的事也由它做；只写调用方新建的目录 | session 不必复制布局知识；deps 仍然从不写活的工作区 | 2026-10-22 |
| deps 不收 `ConfirmToken` | 凭据由 daemon 校验并核对 `Action::DepsApprove` | types.md §4"谁消费"；`move_workspace` 也不收 | 2026-10-22 |
| `open_files` 的实现 | libproc 扫描 fd、映射段与 cwd；失败返回错误，不返回空列表 | 返回空就是错误地放行，会违反不变量 4；dlopen 的原生模块只能从映射段看到 | 2026-10-22 |
| 借用的错误码 | `adopt` 的 special_file、symlink_escapes、changed_during_clone 用 `Drift`；"不是提出者"、"还没发布"用 `NoSuchProposal`；"有行没目录"用 `Corrupt` | `Code` 是封闭的，取最接近的码，用 `detail.reason` 区分；T15 要在 types.md §6.2 补上 | 2026-10-22 |
| `install` 的切分 | 用 config.md §2.4 的切分函数（`validate_commands` 用的同一个） | 只能有一份切分规则；config.md 还没公开这个函数的名字，T15 要补上 | 待定（config.md） |
