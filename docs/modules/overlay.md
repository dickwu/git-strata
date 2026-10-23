# `overlay` 模块

**Crate**：`crates/strata-overlay` · **设计来源**：design.md §4（Overlay、Checkpoint、`.lww`）、§6.2、§6.3（`diff --between` 的投影）、§6.5 第 7 步、§8、§9 不变量 1、2、4、5、6；tgrep `SHARED_WORKTREE_INDEXES.md` 的 "Base snapshots and worktree overlays" 与 "Implemented worktree synchronization API" 两节；module-rulings.md 的「Common」「T7 overlay」与标了 T7、Everyone 的增补 · **矩阵**：目标模块为 `overlay` 的 131 行（3c 60、3a 15、3d 27、3e 6、3f 23；3b 没有），以及理由点名 overlay 的 `—` 行 49 条，逐条见 §7

> 本文按 module-rulings.md 写，含 Addendum 1–8 与其后的「Cold scan」「Empty directories, amended」「Proven copies」「Addendum 8: get_blob_rooted」「Disk identity follows the volume, git identity follows core.ignorecase」「Mapping disk entries to tracked paths is disk identity」「Case-only pairing pairs under either fold」「Metadata changes through a handle go through apfs::set_file_meta」。与 design.md 旧措辞不同的地方以裁定为准：工作区路径是 `<root>/sessions/<session-id>/<repo>/`；`Checkpoint` 用 Task 15 加进 types.md 的形状，多一个 `base: BaseGen` 字段（增补裁定 T7/T6/T15，store.md 已按它写）；基线清单由 overlay 的 `build_manifest` 建、由 store 持久化（Addendum 3）；把条目落到目录上的投影归 overlay，写入一律经 apfs 的 rooted 操作与 store 的 `get_blob_rooted`（Addendum 3、7、8）；工作区的 git 经主 checkout 上打开的 `Git` 读（Addendum 5）；错误时机按 Addendum 3 到 7：第一次对外写入之前只有具名拒绝，之后任何失败都是 `Unknown`，现场保留，`Before*` checkpoint 不算对外写入，restore 的第一次对外写入是 `mkdir T` 与 `plan.json`。apfs 的名字取 apfs.md（1e723f2），store 的取 store.md（0062a3e，`get_blob_rooted` 已在其中）。§2 列为上游要求的有两项，都等 apfs 的下一轮：用测试钉住 `case_fold_key` 的前缀性质（module-rulings「case_fold_key prefix property」），以及句柄上改元数据的 `set_file_meta` 与 `FileMeta`（module-rulings「Metadata changes through a handle go through apfs::set_file_meta」）。标 [我的决定] 的地方是 design 和裁定都没有定、由本文决定的，§9 有汇总。文中标"实测"的 APFS 行为，是 2026-10-22 在本机 APFS 卷上用临时文件验证过的。

## 1. 职责与边界

overlay 负责：

1. **算 overlay**（design §4、§6.2）：一个工作区相对它钉住的基线的整文件差异。增、改是新内容，删是墓碑，改名是新条目兼旧路径的墓碑，未跟踪且合格的文件是 `Untracked`，改回与基线相同就撤销覆盖（tgrep "Base snapshots and worktree overlays" 一节的那张表）。比较只看内容（BLAKE3）和 git 文件模式，从不看 mtime。
2. **监听之上的增量**：把 apfs 的 FSEvents 事件变成"可能脏"的路径集合，只重扫这些路径；事件丢失或溢出时退化为全扫。**正确性不依赖监听**：`reconcile_full`（冷扫，读遍内容）是真相，全扫的结果必须等于它，增量在写入的事件送达或被 hint 之后也等于它（O7；裁定 T7，Review Focus 2）。
3. **工作区属性缓存**：gen count、inode、大小这类每个工作区各不相同的属性，是 overlay 的进程内缓存，从不进基线清单（types.md `BaseManifest`、裁定 T7）。
4. **改名认定**：精确内容配对、只改大小写的改名（按卷或按 git 折叠相等就配，Review Focus 4）、改名到同一次扫描里空出来的名字（增补裁定 T7；3c:166）、类型互换的成组记录（裁定 T7）。
5. **checkpoint**（design §6.2）：把 overlay 的 blob 存进 Store，记一行 checkpoint；没有变化时不记（LayerFS 的 UpToDate）。
6. **restore**（design §6.2）：先记 `BeforeRestore`（安全 checkpoint，不算对外写入），再把目标 checkpoint 投影回目录；所有具名拒绝都在第一次对外写入（`mkdir T` 与 `plan.json`）之前；不覆盖任何没被记下的内容；目标的基线与工作区不同时经 `rebase`（增补裁定 T7/T6/T15）；非空的暂存目录是现场，本文定它怎样了结（Addendum 7）。
7. **rebase**（design §6.5 第 7 步、§6.3）：把对着旧基线的 overlay 整文件三方投影到新基线上，冲突标出、保留会话版本；被 `.lww` 命中的路径不冲突。
8. **基线清单**：`build_manifest` 对导出的基线源码树逐文件取 BLAKE3，取导出后的字节而不是 git blob，依赖目录、种子缓存与 `.git` 不计（Addendum 3：overlay 建清单，store 持久化，建基线 generation 的 session 两个都调）。
9. **投影**（Addendum 3）：`project` 把 overlay 的条目落到一个目录上：mode 0o100644 或 0o100755 的文件，以及由 blob 内容（即目标）建出的 0o120000 符号链接。session 的 `move_workspace` 用它建新一代的树，restore 的落盘是同一个算法加上工作区的安全网。
10. **`.lww` 规则**：`.strata/lww` 的语法、读取与匹配（`lww_matches`）。

overlay 不负责的事，以及它们的去处：

| 事项 | 归谁 |
| --- | --- |
| blob 的存放、去重、校验策略、checkpoint 行的持久化与序号分配、gc | store（store.md） |
| clonefile、getattrlistbulk 遍历、gen count 的语义、FSEvents 监听本身、Unicode 大小写折叠、逐级 `O_NOFOLLOW` 打开 | apfs（apfs.md） |
| 读 index 与 config、ignore 判定、hunk 级三方合并 `merge3`、二进制判定 | gitio（gitio.md） |
| 基线 generation 的导出、清单的登记（调 `build_manifest` 与 store）、工作区的克隆、`move_workspace` 的新树准备与原子交换、种子文件与种子缓存 | session（session.md）。基线 generation 目录是 `Store::base_dir(&gen)`：导出的源码树加上克隆进来的依赖集（Addendum 3） |
| 批次与发布组、staging 树、发布时的 `.lww` 决议（发布组内后应用的批次胜，同一会话里较大的 `CheckpointId` 胜，裁定 T10）、Conflict、HeadMoved、`NoCommit` | publish（publish.md）。overlay 只提供 `lww_matches` 与 `Overlay::paired` |
| 何时 checkpoint、每小时对账的调度、同一会话的请求排队、按 repo 的写锁、status 的组装 | daemon（daemon.md） |
| 依赖目录（不进 overlay、不被 overlay 写） | deps 与 session |
| 检索视图 | search：它按名字使用 `Overlay` |

对应 design 的章节：§4 的 Overlay、Checkpoint、`.lww` 三个词条；§6.2 全节；§6.3 `diff --between` 里"把旧的一方按发布后 rebase 的算法投影到新基线"那一步；§6.5 第 7 步的重算 overlay 与三方投影；§8 的 checkpoint 与全量对账两个验收数字；§9 不变量 1、2、4、5、6。

## 2. 依赖

Workspace crate（types.md §8：overlay 在第 2 层，只依赖第 0–1 层，不依赖 deps）：

| 依赖 | 用途 |
| --- | --- |
| `strata-types` | `BaseGen`、`BaseManifest`、`ManifestEntry`、`OverlayEntry`、`EntryKind`、`Checkpoint`、`CheckpointReason`、`CheckpointId`、`BlobId`、`RepoPath`、`RepoName`、`SessionId`、`TreeOid`、`RepoConfig`、`StrataError`、`Code` |
| `strata-apfs` | `walk_with`、`WalkOptions`（全扫）、`list_dir`（列单个目录：增量扫描、拼写解析、restore 看 T）、`Attrs`（`kind`、`size`、`mode`、`gen_count`、`inode`）、`Attrs::unchanged_since`（缓存作保的唯一判法）、`Attrs::gen_count_known`、`FileKind`、`attrs_of`（点查）；`volume_caps` 与 `VolumeCaps.case_sensitive`（磁盘身份按卷折叠，§5.1）；`case_fold_key`（折叠键，§5.1，Addendum 7）；`watch_with`、`WatchOptions`（`exclude` 只放 `.git`，§5.4）、`Watcher`、`WatchEvent`（脏集合的来源）、`Watcher::flush`（经 `Tracker::flush` 由 daemon 调用，§5.6，Addendum 7、8）；`open_rooted_file`（读内容、读 `.strata/lww`，FIFO 不会挂住）、`open_rooted`（取 T 的句柄）、`open_root`（取 R 自己的句柄）、`full_fsync`（`F_FULLFSYNC`）、`classify_io_error` 与 `IoSide`（overlay 自己在句柄上做的 libc 调用失败时归类 errno）；restore 与 `project` 的写全经 apfs 的 rooted 操作（Addendum 8）：`clone_file_rooted`、`create_file_rooted`、`mkdir_rooted`、`symlink_rooted`、`rename_rooted` 与 `RenameMode { Excl, Swap }`、`remove_rooted` 与 `RemoveKind { File, EmptyDir }`（`EmptyDir` 遇到非空目录返回 `NotEmpty`，module-rulings「NotEmpty from apfs」）。上游要求（apfs 的下一轮）：用测试钉住 `case_fold_key` 的前缀性质 `case_fold_key(a + "/" + b) == case_fold_key(a) + "/" + case_fold_key(b)`（apfs.md §5.8 已写明整条路径的键就是各分量的键用 `/` 拼起来；module-rulings「case_fold_key prefix property」：受限目录与 X 的键前缀范围查找靠它，§5.1）；`set_file_meta(file, &FileMeta { clear_flags, mode, mtime })`（module-rulings「Metadata changes through a handle go through apfs::set_file_meta」：句柄指的是普通文件且 `st_nlink == 1` 才改，否则什么都不改，硬链接是 `ReadOnly`、`reason = "hard_link"`；restore 在 T 里、`project` 在 dst 里对句柄的元数据改动都经它，§5.7、§5.12） |
| `strata-gitio` | `Git::read_index(worktree)` 与 `IndexInfo`（`is_tracked` 判 `Added` 还是 `Untracked`；`tracked` 进 §5.1 按卷折叠的键，`ignorecase` 给出 §5.3 只改大小写配对用的 git 折叠；不用 `tracked_under`，它只折叠 ASCII，又跟着 `core.ignorecase`，§5.1）；`Git::is_ignored(worktree, path)`（增量点查与全扫用同一个函数，gitio.md §5.8 第 7 步）；`merge3`、`Merge3`（rebase）；测试的 fixture 还用 `Git::open`（对主 checkout）、`export_tree`、`ensure_shadow`、`attach_worktree` |
| `strata-store` | `Store` 的 `put_blob_rooted`（Addendum 8：checkpoint 从工作区存 blob，不跟随符号链接；扫描之后换成目录或特殊文件的源，store 在克隆之前就拒绝，§5.6）、`put_blob`（rebase 按 blob 不变量存基线文件）、`put_bytes`、`read_blob`、`verify_blob`、`record_checkpoint`、`checkpoint`、`latest_checkpoint`、`session`、`root`、`get_blob_rooted(id, root, dst)`（「Addendum 8: get_blob_rooted」：restore 往 T、`project` 往 dst 取 blob，从 root 起逐级不跟随符号链接，§5.7、§5.12），overlay 不用按路径写的 `get_blob`；测试的 fixture 还用 `record_deps_set`、`record_base_gen`（连清单一起登记）、`create_session`、`base_dir`、移动意图的 `begin_move` 到 `finish_move`，以及 store 的测试接缝 `Seam`、`arm` |

外部 crate：

| 依赖 | 理由 |
| --- | --- |
| `blake3` | `compute` 与 `reconcile_full` 只读不存，自己算内容哈希，与 store 的 `BlobId` 是同一种地址 |
| `libc` | 只用在文件句柄上，apfs 没有包装的：`flistxattr`、`fremovexattr`（清克隆带来的扩展属性）、`fsync`（`F_FULLFSYNC` 用 apfs 的 `full_fsync`）；BSD 标志、mode 与 mtime 一律经 apfs 的 `set_file_meta`，从不自己调 `fchflags`、`fchmod`、`futimens`；句柄来自 apfs 的 `open_root`、`open_rooted`、`open_rooted_file`、`create_file_rooted`。工作区、staging 与基线树上不做任何自己的 `*at` 调用（Addendum 8；design §11 第 2 条：macOS API 走 libc） |
| `serde` | §3 里出现在 RPC/JSON 上的类型（`Overlay`、`OverlayWarning`、`ScanStats`、`Restored`、`Rebased`） |
| `serde_json` | `T/plan.json`（§5.7）与错误的 `detail` |
| `rayon` | checkpoint 并行 `put_blob_rooted`（store.md §5.3：并发的 put 各自克隆、哈希，同一内容只落一份） |
| 开发依赖 `tempfile`、`proptest`、`rusqlite` | 临时工作区；§7 的随机改动序列；`Busy` 测试里持锁的子进程（只在测试里，overlay 的代码不跑 SQL） |

`Git` 句柄的要求：`ws.git` 是对该 repo **主 checkout** 打开的 `Git`（gitio.md §3：`Git::open` 拒绝 strata 的工作区）。overlay 读工作区的 index 与 ignore 规则时把 `ws.root` 作为 worktree 传进去：`git.read_index(ws.root)`、`git.is_ignored(ws.root, p)`。按 Addendum 2，每个会话的每个 repo 有自己的影子仓库（`<paths.root>/sessions/.repos/<session-id>/<repo>.git/`），工作区的 `.git` 是指向其管理目录 `worktrees/<n>/` 的 gitfile；gitio 经这个 gitfile 读管理目录里的 index，影子仓库算本仓库的（gitio.md §5.7 第 2 步的归属核对）。别的仓库的 `Git` 读它得到 `InvalidInput`；index 不存在得到 `Unknown`（`detail.reason = "no-index"`，Addendum 2），从不当作"什么都没跟踪"。`is_ignored` 读的是影子仓库在 `new` 时抄下、会话期间不再改写的 `info/exclude` 镜像，工作区里的 `.gitignore`，以及全局排除文件（gitio.md §5.8 第 4 步）。daemon 对每个 repo 的主 checkout 打开一个 `Git`，交给该 repo 的所有工作区。

谁调用本模块：

| 调用方 | 用到 |
| --- | --- |
| daemon | 每个 tracked 工作区一个 `Tracker`（`new`、`watch`、`inbox`、`warnings`、`reset`）；PostToolUse 钩子带来的路径交给 `Inbox::hint` 再调用 `Tracker::flush`，工具没有路径清单（Bash 等）时改调 `Inbox::require_full`，然后做 `ToolCall` checkpoint（§5.6 第 2 步）；`checkpoint -m`；每小时与读 status 时的 `compute`；`restore`；从 store 取清单（`manifest`）组成 `Base`；status 并入 `Tracker::warnings`，并把非空的 restore 暂存目录（读它的 `plan.json`）报成现场（§5.7）；session 的 `BeforeRollout`、`BeforeRemove` 用的是 daemon 交过去的这个工作区的 `Tracker`（module-rulings「Session's Before* checkpoints use the daemon's Tracker」） |
| session | `build_manifest`（建基线 generation 时，结果交给 store 登记）；`checkpoint`（`BeforeRollout`、`BeforeRemove`：带 daemon 交来的这个工作区的 `Tracker`，即不依赖事件的全扫；daemon 还没有它的 Tracker 时才不带，即冷扫，module-rulings「Session's Before* checkpoints use the daemon's Tracker」）；`reconcile_full`（`remove` 的未发布改动检查、移动后对旧树的核对）；`rebase`、`project` 与 `Overlay`（`move_workspace` 建新一代的树）；`rm` 遇到非空的 restore 暂存目录返回 `Dirty`，除非 `--discard`（Addendum 7） |
| publish | `compute`、`rebase`（发布后 rebase 经 session；`diff --between` 的投影）、`Overlay::paired`、`LwwRules`、`lww_matches`；发布前的 `BeforePublish` checkpoint 由 daemon 经本模块记 |
| search | `Overlay`（`hides`、`tombstones`、`get`），以及 `Scan`/`Checkpointed` 里的 `changed` 作刷新提示 |
| cli | 只经 daemon 的 RPC |

## 3. 公开接口（Rust）

跨模块类型全部来自 types.md。本节定义 overlay 自有的公开类型：`WorkspaceRef`、`Base`、`Overlay`（types.md §2.1 列在 overlay 名下，search 与 publish 按名字使用）、`OverlayWarning`、`ScanStats`、`Scan`、`Checkpointed`、`RestoreRequest`、`Restored`、`RebaseOptions`、`Rebased`、`LwwRules`、`Tracker`、`Inbox`，以及只在测试里出现的 `OverlayFault` 与 `ReadProbe`。`BaseManifest` 是 types.md 的定义（裁定：只记内容），本文从不重新定义它。

```rust
// crates/strata-overlay/src/lib.rs —— 签名清单（不是实现）
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use strata_apfs::WatchEvent;
use strata_gitio::Git;
use strata_store::Store;
use strata_types::{
    BaseGen, BaseManifest, BlobId, Checkpoint, CheckpointId, CheckpointReason, OverlayEntry,
    RepoConfig, RepoName, RepoPath, SessionId, StrataError, TreeOid,
};

/// types.md §8 的 `Result<T>`。
pub type Result<T> = std::result::Result<T, StrataError>;

/// 脏集合的缺省上限（§5.4）；超过就改为全扫。
pub const DEFAULT_DIRTY_CAPACITY: usize = 65_536;
/// rebase 做三方合并时读内容的上限，即 design §7 的大文件阈值 64 MiB（§9）。
pub const MERGE_MAX_BYTES: u64 = 64 * 1024 * 1024;
/// `.lww` 规则文件在仓库里的位置（design §4）。
pub const LWW_FILE: &str = ".strata/lww";
/// restore 的私有暂存目录名前缀：`<会话根>/.strata-tmp-restore-<repo>/`（§4.2）。
pub const RESTORE_TEMP_PREFIX: &str = ".strata-tmp-restore-";

/// 调用 overlay 时借用的工作区上下文。它不是数据，不 derive serde。
pub struct WorkspaceRef<'a> {
    pub session: &'a SessionId,
    pub repo: &'a RepoName,
    /// 工作区目录，即 WorkspaceState.path（<root>/sessions/<session-id>/<repo>）。
    pub root: &'a Path,
    /// 这个仓库的配置：deps_dirs 与 seed_caches 不进 overlay（§5.1）。
    pub config: &'a RepoConfig,
    /// 对该 repo 主 checkout 打开的 Git；读 index 与 ignore 规则时把 root 作为 worktree 传进去（§2）。
    pub git: &'a Git,
    pub store: &'a Store,
}

/// 一个基线：BaseGen、它的清单（store 里登记的那份，由 build_manifest 建出）和只读的基线目录。克隆很便宜。
#[derive(Clone, Debug)]
pub struct Base {
    pub gen: BaseGen,
    pub manifest: Arc<BaseManifest>,
    /// 基线 generation 目录，即 Store::base_dir(&gen)：导出的源码树加上克隆进来的依赖集（Addendum 3），只读。
    /// restore 与 rebase 从这里取基线文件的内容：基线文件不在 blob 存储里（Addendum 4）。
    pub dir: PathBuf,
}

/// 工作区相对一个基线的全部整文件差异（types.md §2.1：所有者 overlay，使用者 search、publish）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Overlay {
    /// 这份差异对着的基线，与 Checkpoint.base 同义。
    pub base: BaseGen,
    /// 按 path 升序（RepoPath 的逐字节序），满足 §4.3 的 V1–V5。
    pub entries: Vec<OverlayEntry>,
}

impl Overlay {
    pub fn empty(base: BaseGen) -> Overlay;
    /// 取 checkpoint 的 base 与 entries；store 读回时 entries 已按 path 排序。
    pub fn from_checkpoint(cp: &Checkpoint) -> Overlay;
    pub fn is_empty(&self) -> bool;
    /// path 上的那一条；只按 path 找，Renamed 的 from 不算。
    pub fn get(&self, path: &RepoPath) -> Option<&OverlayEntry>;
    /// 基线里的这个路径是否被遮住：path 上有条目（任何种类），或者它是某条 Renamed 的 from。
    pub fn hides(&self, path: &RepoPath) -> bool;
    /// 墓碑：Deleted 条目的 path 与 Renamed 条目的 from，按路径排序。
    pub fn tombstones(&self) -> Vec<RepoPath>;
    /// 必须与 path 一起发布或恢复的条目路径（含 path 自己）：改名的另一半，以及类型互换的成组路径（§5.3）。
    /// path 既不是条目的 path 也不是某条 Renamed 的 from 时返回空。
    pub fn paired(&self, path: &RepoPath) -> Vec<RepoPath>;
    /// 核对 §4.3 的 V1–V4（不需要清单）。Code：InvalidInput（detail.rule = "V1".."V4"）。
    pub fn validate(&self) -> Result<()>;
}

/// status 要报出的事实。daemon 组装 status 时并入（session.md §5.5）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum OverlayWarning {
    /// 文件带 setuid、setgid 或 sticky 位。条目里的 mode 已按 git 规范化成 0o100644/0o100755，
    /// 发布从不写出这些位。mode 是权限低 12 位的八进制文本，如 "4755"。目录上的这些位不记录、不告警。
    SpecialModeBits { path: RepoPath, mode: String },
    /// 写竞态（§5.2）：读内容期间文件两次都在变，或 gen count 不可得（有可写的 mmap）。
    /// 这一次的内容是某一刻读到的（checkpoint 存下的总是 put_blob_rooted 克隆出的完整一份），下一次扫描重扫它。
    Unstable { path: RepoPath },
    /// FIFO、socket 或设备文件：git 存不了，overlay 从不打开、不记录；它占着的基线路径记为 Deleted。
    SpecialFile { path: RepoPath },
    /// 嵌套的 git 仓库（根以外的目录里有 `.git`）：overlay 只看其中的基线路径与已跟踪路径（受限目录，§5.1），
    /// 别的不记录；strata 不管理子模块。
    EmbeddedRepo { path: RepoPath },
}

/// 一次扫描的工作量计数。§7 用计数而不是计时来断言工作量随改动增长。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanStats {
    /// 列过的目录数，每个目录一次 apfs::walk_with 或 list_dir 的批量读。
    pub dirs_listed: u64,
    /// 读过属性的条目数。
    pub entries_seen: u64,
    /// 读了内容来哈希的文件数与字节数（checkpoint 里经 put_blob_rooted 的也算）。
    pub files_hashed: u64,
    pub bytes_hashed: u64,
    /// 属性缓存直接作保、没有读内容的文件数。
    pub vouched: u64,
    /// 这次是否全扫；是否冷扫（缓存为空：reconcile_full，或 Tracker 新建、reset 之后的第一次扫描，每个候选都读内容，§5.5）。
    pub full: bool,
    pub cold: bool,
}

/// compute 的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scan {
    pub overlay: Overlay,
    /// 与 tracker.last() 相比条目有变化的路径（含 Renamed 的 from），search 据此刷新视图。
    pub changed: Vec<RepoPath>,
    pub warnings: Vec<OverlayWarning>,
    pub stats: ScanStats,
}

/// checkpoint 的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkpointed {
    /// recorded = true：本次写下的 checkpoint，id 是 store 分配的号。
    /// recorded = false：基线与条目都与最新的 checkpoint 相同，这里就是那个最新的 checkpoint，什么都没写。
    pub checkpoint: Checkpoint,
    pub recorded: bool,
    /// 与本次之前最新的 checkpoint 相比条目有变化的路径。
    pub changed: Vec<RepoPath>,
    pub warnings: Vec<OverlayWarning>,
    pub stats: ScanStats,
}

/// restore 的请求。
pub struct RestoreRequest<'a> {
    /// 要恢复到的 checkpoint：本会话的一个号，它的 repo 必须是 ws.repo。
    pub target: CheckpointId,
    /// 目标的基线与工作区当前基线不同时必须给出：目标基线的清单与目录（§5.7 第 4 步）。
    pub target_base: Option<&'a Base>,
    /// None：两边 overlay 里的全部路径；Some：这些路径（按字节比较），可以是文件，也可以是目录前缀（§5.7 第 6 步）。
    pub paths: Option<&'a [RepoPath]>,
    /// restore --verify：写之前用 store::verify_blob 重算每个要写回的 blob，并核对要克隆回去的基线文件。
    pub verify: bool,
}

/// restore 的结果。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Restored {
    /// restore 之前记下的 BeforeRestore checkpoint：恢复它就回到 restore 之前（不变量 1）。
    pub before: CheckpointId,
    pub target: CheckpointId,
    /// 写成目标内容的路径与删掉的路径，按路径排序。
    pub written: Vec<RepoPath>,
    pub removed: Vec<RepoPath>,
    /// 跨基线恢复时三方投影有冲突、写成了目标 checkpoint 版本的路径。
    pub conflicts: Vec<RepoPath>,
}

/// rebase 的选项。Default 是"不用 .lww、不做内容合并、ignorecase = false、卷不区分大小写（APFS 的默认）"。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RebaseOptions {
    /// 用哪套 .lww 规则；空规则即不用 .lww。
    pub lww: LwwRules,
    /// 仓库的 core.ignorecase（IndexInfo.ignorecase）：只改大小写的配对用的 git 折叠（§5.3 第 1 步）。
    pub ignorecase: bool,
    /// 工作区所在的卷是否区分大小写（apfs::VolumeCaps 的 case_sensitive）：只改大小写的配对用的按卷折叠（§5.3 第 1 步）。
    pub volume_case_sensitive: bool,
    /// true：两边都改了的普通文件先试 gitio::merge3，合得上就取合并结果（经 store::put_bytes 存下）。
    /// false：两边都改了一律算冲突、取会话一侧，rebase 不写任何 blob（§5.8）。
    pub merge: bool,
}

impl RebaseOptions {
    /// move_workspace 与发布后 rebase 用：merge = true，.lww 规则从新基线的目录读（LwwRules::load(&new.dir)），
    /// 卷是否区分大小写取 apfs::volume_caps(&new.dir)（新基线的 generation 目录与工作区在同一个卷上，§5.8 第 5 步）。
    /// Code：LwwRules::load 与 volume_caps 的码。
    pub fn for_move(new: &Base, ignorecase: bool) -> Result<RebaseOptions>;
    /// restore 跨基线时用：merge = false，不用 .lww，卷是否区分大小写由 restore 传入（§5.7 第 4 步）。
    pub fn for_restore(ignorecase: bool, volume_case_sensitive: bool) -> RebaseOptions;
}

/// rebase 的结果。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rebased {
    /// 对着新基线的 overlay；冲突路径上是会话的版本。
    pub overlay: Overlay,
    /// 两边都改了而合不上、保留了会话版本的路径（design §6.5 第 7 步"冲突标出"），以及 §5.8 第 4a 步前缀冲突里两边的路径。
    pub conflicts: Vec<RepoPath>,
    /// 离开 overlay 的路径：新基线已经等于会话的内容。
    pub dropped: Vec<RepoPath>,
    /// 被 .lww 命中、两边都改了的路径：整份取会话一侧（被投影的 checkpoint 是最新的那个），不试合并，不算冲突。
    pub lww: Vec<RepoPath>,
}

/// `.strata/lww` 的规则：LayerFS `/.lww` 的方言（§5.10）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LwwRules { /* 私有：有序的规则列表 */ }

impl LwwRules {
    /// 解析一份规则文本，不处理 %include。纯函数，从不失败：写坏的行（空模式、只有 `!`）跳过 [我的决定]。
    pub fn parse(text: &str) -> LwwRules;
    /// 从源码树 tree（通常是新基线的目录）读 `.strata/lww` 与它的 %include，最多 16 个文件。
    /// 文件不在、不是普通文件、不是 UTF-8，都当作没有这个文件。Code：Unknown（读失败）。
    pub fn load(tree: &Path) -> Result<LwwRules>;
    pub fn is_empty(&self) -> bool;
}

/// overlay 对一个 tracked 工作区的进程内状态：属性缓存、原始差异、脏集合、告警（§4.1）。
/// daemon 每个 tracked 工作区持有一个（Mutex<Tracker>）。扫描类函数拿 &mut Tracker，
/// 所以同一工作区的扫描在类型上就是串行的；它不是数据，不 derive serde。
pub struct Tracker { /* 私有 */ }

impl Tracker {
    /// 新的 Tracker：缓存为空，needs_full() 为 true，第一次扫描就是冷扫。capacity 是脏集合的上限。
    pub fn new(root: &Path, capacity: usize) -> Tracker;
    /// 交给监听回调的句柄。
    pub fn inbox(&self) -> Inbox;
    /// apfs::watch_with(root, &WatchOptions { exclude: 只有 .git }, …)，事件转进 inbox；排除集合 X 由 Inbox::note 按当前的 X 过滤（§5.4），
    /// 所以 X 变了不用重新 watch。Tracker 持有返回的 Watcher，丢弃 Tracker 即停止监听。Code：apfs 透传。
    pub fn watch(&mut self) -> Result<()>;
    /// 调用 Watcher::flush()（Addendum 8：FlushSync，再排空流的私有队列）：best-effort，让一部分积压的事件进 inbox，
    /// 不是屏障。daemon 在 ToolCall checkpoint 之前调用（§5.6 第 2 步）；没有监听时什么都不做。apfs 的 flush 没有返回值，
    /// 这里也不会失败。不得在监听回调里调用：apfs 在 debug 构建里 debug_assert，release 构建里直接返回、什么都不做；
    /// 回调只碰 Inbox 自己的锁，所以 daemon 持着 Tracker 调用它不会死锁。
    pub fn flush(&self);
    /// 最近一次扫描所对的基线；还没扫过时为 None。
    pub fn base(&self) -> Option<&BaseGen>;
    /// 最近一次扫描得到的 overlay。
    pub fn last(&self) -> Option<&Overlay>;
    /// 当前的告警，status 用它。
    pub fn warnings(&self) -> &[OverlayWarning];
    /// 下一次扫描是否必须全扫：新建或 reset 之后还没扫成过，或者 inbox 要求全扫（事件丢失、脏集合溢出、require_full）。
    /// 换了基线、X 或 ignorecase 变了由 compute 自己发现（§5.5 第 1、2 步）；扫描失败不改变它，快照并回 inbox（§5.5 第 7 步）。
    pub fn needs_full(&self) -> bool;
    /// 工作区被换成新一代的树之后由 daemon 调用（Addendum 3）：丢掉缓存、原始差异与监听，
    /// needs_full() 变为 true，下一次扫描是冷扫；之后再 watch()。
    pub fn reset(&mut self);
}

/// 脏集合的入口：Send + Sync，克隆很便宜。监听线程随时调用，从不等待正在进行的扫描。
#[derive(Clone)]
pub struct Inbox { /* 私有：Arc<Mutex<…>> */ }

impl Inbox {
    /// 收一个监听事件（§5.4）。
    pub fn note(&self, event: &WatchEvent);
    /// 收已知改过的路径：daemon 在 ToolCall checkpoint 之前交来 PostToolUse 钩子带来的、Edit 与 Write 等写过的文件（§5.6 第 2 步）。
    /// 路径经过符号链接时，链接指向的路径也记下；解析不了或指到根外 → 要求全扫（§5.4）。它读属性，在 daemon 的线程上调用，不在监听回调里。
    pub fn hint(&self, paths: &[RepoPath]);
    /// 要求下一次扫描全扫。daemon 在没有路径清单的工具调用（Bash 等）之后、ToolCall checkpoint 之前调用它（§5.6 第 2 步）。
    pub fn require_full(&self);
}

/// 为导出的基线源码树建清单（§5.9，Addendum 3）：逐文件取导出后字节的 BLAKE3，依赖目录、种子缓存与 `.git` 不计。
/// 纯计算，不写 Store：登记由建基线 generation 的 session 调 store 完成。
pub fn build_manifest(dir: &Path, tree: &TreeOid, config: &RepoConfig) -> Result<BaseManifest>;

/// 算 overlay（§5.5）。dirty = Some：只重扫这些路径（文件或目录）；None：全扫，属性缓存作保的文件不读内容。
/// tracker 需要全扫时不看 dirty。不写 Store，不写工作区。
pub fn compute(ws: &WorkspaceRef, base: &Base, tracker: &mut Tracker, dirty: Option<&[RepoPath]>) -> Result<Scan>;

/// 冷扫（§5.5）：不信任何缓存与监听，读遍每个文件的内容，结果就是真相。不写 Store，不写工作区。
pub fn reconcile_full(ws: &WorkspaceRef, base: &Base) -> Result<Overlay>;

/// 记一个 checkpoint（§5.6）。tracker 为 None 时冷扫（reconcile_full）；有 tracker 时 Manual、Before* 全扫，ToolCall 增量。
pub fn checkpoint(ws: &WorkspaceRef, base: &Base, tracker: Option<&mut Tracker>, reason: CheckpointReason) -> Result<Checkpointed>;

/// 把工作区（或其中一些路径）恢复到一个 checkpoint（§5.7）：先记 BeforeRestore，再投影。
pub fn restore(ws: &WorkspaceRef, base: &Base, tracker: Option<&mut Tracker>, req: &RestoreRequest) -> Result<Restored>;

/// 整文件三方投影（§5.8）：把对着 old 的 overlay 投影到 new 上，内容合并用 gitio::merge3。
/// 只写 blob（合并出来的内容经 store::put_bytes），从不写工作区。
pub fn rebase(overlay: &Overlay, old: &Base, new: &Base, store: &Store, opts: &RebaseOptions) -> Result<Rebased>;

/// 把 overlay 的条目落到 dst 上（§5.12，Addendum 3、7）。dst 必须是 overlay.base 那棵树的一份未改动的克隆
/// （session 准备的新一代的树），而且在 <store.root()>/sessions/ 之下。删掉 Deleted 的路径与 Renamed 的 from，写入其余条目；
/// 所有写都经 apfs 的 rooted 操作（rename_rooted、remove_rooted 等）与 store 的 get_blob_rooted，句柄上的元数据改动经 apfs::set_file_meta，不跟随任何符号链接，不写 dst 之外的地方，
/// 不做 F_FULLFSYNC（交换之前由调用方同步）。
pub fn project(store: &Store, overlay: &Overlay, dst: &Path) -> Result<()>;

/// path 是否被 .lww 规则命中（§5.10）。纯函数。
pub fn lww_matches(patterns: &LwwRules, path: &RepoPath) -> bool;

/// 测试接缝（cargo feature `test-seams`，只在测试里打开，从不进发布的二进制，Addendum 4）。
/// 按工作区根登记，并行的测试互不干扰。
#[cfg(feature = "test-seams")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OverlayFault {
    /// 读完 path 的内容之后、再读属性之前停住，直到 release_fault 放行（§5.2 第 4 步的写竞态）。
    PauseAfterHash { path: RepoPath },
    /// 扫描取完 inbox 快照之后停住（§5.4：扫描期间到达的事件进下一轮）。
    PauseAfterSnapshot,
    /// checkpoint 扫描完、第 5 步存 blob 之前停住（§5.6：扫描之后某一级上级被换掉）。
    PauseBeforePut,
    /// restore 在第 8 步预检之前停住（§5.7：扫描之后、写之前的并发写入）。
    PauseBeforePrecheck,
    /// restore 在第 10 步写下 plan.json 并落盘之后、在 T 里备临时文件之前停住（§5.7：T 被换成符号链接）。
    PauseAfterPlan,
    /// restore 在第 10 步把一个要改元数据的临时文件放进 T 之后、打开它改元数据之前停住（§5.7：它被换成别处文件的硬链接）。
    PauseBeforeTempMeta,
    /// restore 在阶段 C 的第 n 次改名之后停住（§5.7：并发写入与现场）。
    PauseInApply { after: u32 },
}

#[cfg(feature = "test-seams")]
pub fn inject_fault(root: &Path, fault: OverlayFault);
#[cfg(feature = "test-seams")]
pub fn release_fault(root: &Path);

/// 测试接缝：对 root 之下的一个文件跑一遍 §5.2（不用缓存），给 Task 3 的写竞态测试用。
#[cfg(feature = "test-seams")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadProbe {
    pub blob: BlobId,
    pub mode: u32,
    /// 第 5 步重来过一次。
    pub retried: bool,
    /// 两次都在变，或 gen count 不可得：这一份可能是撕裂的。
    pub unstable: bool,
}
#[cfg(feature = "test-seams")]
pub fn read_candidate(root: &Path, path: &RepoPath) -> Result<ReadProbe>;
```

**与计划签名的差别**：

- 计划写的 `struct BaseManifest`（基线每个文件的 `Attrs` + `BlobId`）拆成两半：内容一半就是 types.md 的 `BaseManifest`（blob、mode、size），本文引用、不重新定义；属性一半（gen count、inode、大小）是 `Tracker` 里的缓存（裁定 T7，types.md §4 `BaseManifest` 的注释）。
- 计划的 `ws: &Path` 换成 `WorkspaceRef`，`base` 换成 `Base`：算 overlay 要 index、ignore 规则和排除的目录，restore 与 rebase 要读基线文件的内容，而基线文件不在 blob 存储里（Addendum 4）。
- Addendum 3 写的是 `build_manifest(dir)`，这里多了 `tree` 与 `config` [我的决定]：`BaseManifest` 有 `tree` 字段，排除的依赖目录与种子缓存来自仓库配置。它不碰 Store，登记仍由 session 调 store。
- 多一个 `Tracker` 参数：属性缓存与脏集合要在调用之间保留，design §6.2 的"监听维护的可能脏路径集"住在这里。`checkpoint` 与 `restore` 可以不带它，这时冷扫：session 的 `BeforeRollout`、`BeforeRemove` 只在 daemon 还没有这个工作区的 `Tracker` 时这样调用，平时带着 daemon 交来的那一个（module-rulings「Session's Before* checkpoints use the daemon's Tracker」）。
- 全部包上 `Result`，因为它们都读文件系统或 Store。
- `checkpoint` 返回 `Checkpointed`：调用方要知道这次有没有写（UpToDate）、哪些路径变了、有哪些告警 [我的决定]。session 取 `.checkpoint.id`。
- `restore` 的目标是 `CheckpointId`（经 `RestoreRequest`）而不是 `&Checkpoint` [我的决定]：overlay 在同一次串行操作里自己读它，store 的 `NoSuchCheckpoint` 原样透传（3c `captured_namespace_custody.rs` 一行），也由 overlay 核对它属于这个工作区。
- `rebase` 多了 `store`（读会话一侧的 blob、存合并结果）与 `RebaseOptions`（`.lww`、ignorecase、卷是否区分大小写、是否做内容合并）。session.md（853fdf8）§5.7 第 13 步已按它调用，用 `RebaseOptions::for_move`。
- 测试接缝 `read_candidate` 与 `ReadProbe`（Addendum 4 的测试接缝规则）：§5.2 的写竞态规则在 Task 3 实现，那里就要有一个先失败的测试 [我的决定]。
- `lww_matches` 的 `patterns` 是解析好的 `LwwRules`；`build_manifest`、`project`（都来自 Addendum 3）、`Overlay` 的辅助方法、`Tracker`、`Inbox`、`LwwRules` 是计划之外新增的，各有调用方（§2 的表）。

各函数的语义与可能返回的 `Code`：

- **`build_manifest`**：见 §5.9。码：`NoSuchPath`（dir 不存在）、`NotText`（目录项不是 UTF-8）、`Unsupported`（dir 里有 FIFO、socket、设备文件：导出的树里不应该有）、`TooLarge`（`too_deep`）与 `Busy`（`root_changed`，Addendum 8 给 apfs 定的映射）、`Unknown`（读失败）。
- **`project`**：见 §5.12。`overlay.validate()` 不过，或 dst 不在 `<store.root()>/sessions/` 之下 → `InvalidInput`；dst 不在 → `NoSuchPath`；要删或要替换的路径在 dst 里不在、或者类型不对 → `Corrupt`（dst 不是基线的完整克隆；基线文件缺了是 `Corrupt`，从不当作新文件，Addendum 4）；上级某一级是符号链接 → `NotDirectory`（apfs 透传，dst 之外什么都没写）；store 的 `get_blob_rooted`、`read_blob` 与 apfs 的其它码原样透传。失败时 dst 停在半途：它是调用方的内部 staging，由调用方按 Addendum 3、4、7 的错误时机处理。
- **`compute`**：见 §5.5。`base.gen.repo != *ws.repo` → `InvalidInput`。工作区目录不在 → `NoSuchPath`；没有 `.git`、index 读不了 → gitio 的码原样透传（`NoSuchPath`、`InvalidInput`、`NotText`、`Unsupported`、`Unknown`，index 不存在是 `Unknown` 的 `no-index`）；目录项不是 UTF-8 → `NotText`；名字超出 macOS 的长度限制 → `InvalidInput`；遍历太深 → `TooLarge`、工作区根在遍历中被换掉 → `Busy`（apfs 的 `too_deep`、`root_changed`，Addendum 8）；`is_ignored` 读不了规则文件、读文件失败 → 按 types.md §6.3 归类（多半是 `Unknown`）。失败时 tracker 的状态不变，脏集合不丢（§6.1）。
- **`reconcile_full`**：同 `compute`，只是不带 Tracker、从不读缓存。session 用它对一棵不在原位的旧树核对（session.md §5.7 第 19 步），所以 `ws.root` 可以是任何以 gitfile 接到本仓库影子仓库的目录。
- **`checkpoint`**：见 §5.6。在 `compute` 的码之外：会话不存在或已 `Removed` → `NoSuchSession`；会话里没有这个 repo → `NoSuchRepo`；`base.gen` 不是该工作区在 Store 里记的当前基线 → `InvalidInput`（`detail.reason = "stale_base"`），什么都不写（3f `wrong_root_and_reader_swap_are_rejected` 一行）；`ws.root` 与 Store 里记的工作区路径不是同一个目录 → `InvalidInput`（`detail.reason = "wrong_root"`）；工作区 `tracked = false` → `InvalidInput`（store 也拦）；Store 的 `Busy`、`ReadOnly`、`Corrupt`、`TooLarge`、`Unknown` 原样透传，`put_blob_rooted` 的 `CrossVolume`、`Unsupported`、`InvalidInput` 也一样（它返回的 `NoSuchPath`、`IsDirectory`、`NotFile`、`NotDirectory` 不是失败：扫描之后路径变了，按写竞态重判，§5.6 第 5 步）。checkpoint 的对外写入只有最后记下的那一行，记行是一个事务：之前的失败什么都没写，不写工作区，所以它的任何失败都不留下现场；`Before*` 的行不算对外写入（Addendum 5）。
- **`restore`**：见 §5.7。第一次对外写入是第 10 步的 `mkdir T` 与 `plan.json`；`BeforeRestore` 是安全 checkpoint，不算（Addendum 5）。之前只有具名拒绝：`Exists`（T 的位置上是符号链接或文件，`reason = "restore_temp_taken"`）；`Unknown`（T 是证明不了的现场，`detail.reason = "restore_scene"`；或会话的 `lifecycle`、工作区的 `health` 为 `Unknown`，§6.3）；`NoSuchCheckpoint`（store 透传，不包装、不再读别的东西）；`InvalidInput`（目标是别的 repo 的、`ws.root` 不是这个会话的工作区、目标格式坏了、目标与当前基线不同却没给 `target_base`、`target_base` 与目标的基线不符、目标里有落在排除目录下的条目）；`NoSuchPath`（`paths` 里的路径在目标 checkpoint、两边的基线和当前 overlay 里都没有：types.md §6.2 写的是"目标 checkpoint 和基线"，多认当前 overlay 是为了能用路径限定的 restore 撤销 checkpoint 之后新建的文件 [我的决定]；按字节比较，只差大小写时 `detail.on_disk` 给出磁盘上的拼写）；`Exists`（要写的位置或它的某一级上级上有没被记下的东西，§5.7 第 8 步）；`ReadOnly`（要动的文件或目录带不可变标志）；`OpenFiles`（要动的文件有可写的 mmap，gen count 不可得）；`Corrupt`（基线文件不在或大小与清单不符；`verify` 时 blob 或基线文件的内容与地址不符）；`Busy`（`workspace_changed`：扫描之后工作区又被改了，或者第 10 步建 T 时它的位置上已有别的东西，重试即可）；记 `BeforeRestore` 时 `checkpoint` 的全部码（那时已记下的 `BeforeRestore` 留着）。第一次对外写入之后的任何失败：`Unknown`，现场保留（§6.3）。
- **`rebase`**：见 §5.8。前提：`overlay` 的 blob 都还在 store 里（来自 checkpoint 的已登记；刚 put 的受 store 的 1 小时窗口保护；get、read、verify 按文件工作，Addendum 4）。`overlay.base != old.gen`，或 `old.gen.repo != new.gen.repo` → `InvalidInput`；overlay 不满足 V1–V5 → `InvalidInput`；基线文件不在，或读到的内容与清单不符 → `Corrupt`；会话一侧的 blob 读不出来 → store 透传（`Corrupt`、`TooLarge`、`Unknown`）；合并结果存不进去 → store 透传。单个路径的"太大""二进制""类型不同"，以及目录与文件的前缀冲突（§5.8 第 4a 步），都不是错误，是冲突（记在 `conflicts` 里）。按 blob 不变量存进 store 的基线文件（§5.8 第 5a 步），id 与清单不符 → `Corrupt`。`merge = false` 时不写任何 blob。
- **`lww_matches`**、**`LwwRules::parse`**：纯函数，不失败。
- **`Overlay::validate`**：`InvalidInput`，`detail.rule` 指明违反了 §4.3 的哪一条。

`OpenFiles` 在 types.md §6.2 的"谁返回"一栏只写了 session；restore 遇到被可写 mmap 着的文件时用它，是按共同规则映射到最接近的已有码 [我的决定]，Task 15 在那一栏补上 overlay，码的集合不变。

## 4. 数据与不变量

### 4.1 进程内状态：`Tracker`

`Tracker` 只在 daemon 进程里，不持久化 [我的决定]。daemon 重启之后的第一次扫描是冷扫：tool 的已跟踪源码是 4196 个文件、42.3 MB（2026-10-22 统计），读一遍远在 design §8 的 3 s 之内。持久化属性缓存，就得证明重启期间没有漏掉任何写入，收益只是省掉这一次读，所以不做（§9）。

| 字段（私有） | 内容 | 用途 |
| --- | --- | --- |
| `cache` | `RepoPath` → `{attrs: apfs::Attrs, blob}`（`attrs` 里有 kind、inode、size、权限的低 12 位与 gen count） | 属性缓存：当前属性对缓存的满足 `Attrs::unchanged_since` 时，直接用缓存的 blob，不读内容（§5.2） |
| `raw` | `RepoPath` → `Absent`，或 `Present {blob, mode, staged}` | 原始差异：与基线不同的每个路径的现状。`Absent` 是基线里有、磁盘上没有；`staged` 是路径在 index 里。条目由它经 §5.1、§5.3 算出，是它的纯函数 |
| `last`、`base` | 上一次扫描的 `Overlay` 与它所对的 `BaseGen` | `Scan.changed`；换了基线就全扫 |
| `index` | 上一次 `read_index` 得到的 `tracked` 集合 | index 变了（`git add`、`git rm --cached`），就重判受影响的路径（§5.4） |
| `inbox` | `Inbox`：路径集合、子树集合、全扫标志、上限，以及 `note` 过滤用的当前 X 与卷是否区分大小写、`hint` 解析符号链接用的 `ws.root` | 脏集合（§5.4） |
| `fold` | M ∪ `I.tracked` 按卷折叠的键：键 → 确切路径，外加键的有序集合（卷是否区分大小写见 `x`） | §5.1 的候选资格、受限目录与 X 第 3 项；M 的部分每个基线算一次，`I.tracked` 的部分在 index 变了时重算 |
| `x` | 上一次扫描用的排除集合 X，以及卷是否区分大小写（`apfs::volume_caps(ws.root)`，第一次扫描时读，之后不变） | X 不论因为什么变了都全扫，先把新的 X 交给 inbox（§5.5） |
| `unstable` | 上一次扫描标为 `Unstable` 的路径 | 下一次扫描必重扫 |
| `warnings` | 当前的 `OverlayWarning` | status |
| `watcher` | `Option<strata_apfs::Watcher>` | 丢弃即停止监听 |

缓存只描述工作区里的文件，与基线无关：换基线时缓存照用，只是分类要按新清单全部重做。`move_workspace` 之后工作区是一棵新树，inode 全变，daemon 调用 `Tracker::reset`（§5.11）。

缓存作保依赖 gen count 的一条性质：文件内容每被写一次，gen count 就变。2026-10-22 在本机 APFS 上实测：新建的文件是 2，写 1 字节变成 3；`utimes` 恢复 mtime、`touch`、`chmod`、`rename` 都不改它；`clonefile` 的克隆体从 1 开始，inode 是新的；有可写的 mmap 时读到 0，`munmap` 之后增加；`truncate` 增加。所以缓存除了 inode、gen、size，还要比较权限位（`chmod` 不改 gen），gen count 不可得时不作保（Addendum 1：这时 apfs 把文件报告为未知，由使用者哈希）。apfs 的 `Attrs::unchanged_since` 正是这条判法：kind、inode、size、mode 相同，两边的 `gen_count` 都非 0 且相等（apfs.md §4.2 C）。

### 4.2 overlay 写的持久数据

overlay 不建库。它写的东西都经别的模块，或者只在 restore 与 `project` 里写：

| 数据 | 位置 | 怎么写 | 性质 |
| --- | --- | --- | --- |
| blob | store 的 `blobs/` | `put_blob_rooted`（checkpoint）、`put_bytes`（rebase 的合并结果）、`put_blob`（rebase 按 blob 不变量存的基线文件） | 按内容寻址，不可变；没有被 checkpoint 引用的，由 store 的 gc 在 1 小时之后收走 |
| checkpoint 行与条目 | `store.db` | `record_checkpoint` | 只追加；一行对一个 repo；entries 是完整快照，不是增量（types.md §4） |
| 基线清单 | 不由 overlay 写 | `build_manifest` 只算；session 在 repo 写锁里调 `Store::record_base_gen`，与基线 generation 一起登记（Addendum 3） | 按导出后的字节算（§5.9）；`Base.manifest` 取 store 里登记的那一份 |
| 工作区文件 | `<会话根>/<repo>/` | 只有 restore 写，经 apfs 的 rooted 操作与 store 的 `get_blob_rooted`、以会话根为根 | 写之前先记 `BeforeRestore`（安全 checkpoint） |
| 投影的目标树 | 调用方给的 dst（session 的 staging，必须在 `<store.root()>/sessions/` 之下） | 只有 `project` 写，经 apfs 的 rooted 操作与 store 的 `get_blob_rooted`、以 dst 为根 | 调用方的内部 staging；同步、交换与失败后的收拾都归调用方（§5.12） |
| restore 暂存目录 T | `<会话根>/.strata-tmp-restore-<repo>/` | 只有 restore 写 | 与工作区同卷、在工作区之外，监听看不到，也不会被当成 `Untracked`。restore 进行中时里面一直有 `plan.json`（§5.7 第 10 步）。T 是平的：restore 只往里放 `plan.json`、临时文件与挪开的文件（普通文件或符号链接），从不放目录。T 里的一个文件只在删除之前刚被证明是一个留着的 checkpoint 所指内容的副本时，才逐个 `remove_rooted(…, RemoveKind::File)`（module-rulings「Proven copies」，§5.7 的"证明"）；`plan.json` 最后删；overlay 只在 T 为空时删掉它（`remove_rooted`，`RemoveKind::EmptyDir`）。非空就是现场（Addendum 7）：status 读 `plan.json` 报出，session 的 `rm` 返回 `Dirty`（除非 `--discard`），了结的办法见 §5.7。它属于 session 的布局（Addendum 5），从来不是工作区路径，`Sessions::resolve(cwd)` 在这里照样落空 |

### 4.3 `Overlay` 的形状规则

| 规则 | 内容 | 谁查 |
| --- | --- | --- |
| V1 | `entries` 按 path 严格升序（`RepoPath` 的逐字节序），每个路径至多一条（3c:42） | `validate` |
| V2 | `Deleted` 的 mode = 0，没有 blob；其它四种的 mode ∈ {0o100644, 0o100755, 0o120000}（types.md §4） | `validate` |
| V3 | `Renamed` 的 `from ≠ path`；`from` 不是任何条目的 path；同一个 `from` 至多出现一次（与 store 的校验一致） | `validate` |
| V4 | 没有一个活路径（非 `Deleted` 条目的 path）是另一个活路径的目录前缀：真实目录里一个名字不能既是文件又是目录（3c:146） | `validate` |
| V5 | 对基线清单 M：`Modified`、`Deleted` 的 path ∈ M；`Added`、`Untracked` 的 path ∉ M；`Renamed` 的 from ∈ M；path ∈ M 的条目，其 (blob, mode) ≠ M[path] | `rebase`、`restore`（要清单） |

`compute` 的结果按构造满足 V1–V5，也没有落在排除集合 X 之下的条目。违反它们的 overlay 只可能来自损坏的 checkpoint，`restore` 与 `rebase` 在写任何东西之前拒绝它（3f:621）。条目落在 X 之下只由 restore 查（§5.7 第 3 步）：rebase 不拿 X。

### 4.4 不变量

| 编号 | 不变量 | design §9 |
| --- | --- | --- |
| O1 | restore 是 overlay 唯一写工作区的操作（`project` 只写调用方的 staging 树），它在第一次写工作区之前记下 `BeforeRestore`。恢复 `Restored.before`，就回到 restore 之前的状态，包括目标里没有的新文件 | 不变量 1 |
| O2 | overlay 只写四处：Store（经 store）、`ws.root` 之下、本 repo 的 restore 暂存目录、`project` 的 dst。checkpoint 与 restore 先核对 `ws.root` 与 Store 里记的工作区是同一个目录（dev + inode）；`project` 先核对 dst 在 `<store.root()>/sessions/` 之下；写都经 apfs 的 rooted 操作或 store 的 `get_blob_rooted`，所以写不到主 checkout，也穿不过工作区、会话根与 dst 里的符号链接；句柄上的元数据改动经 `apfs::set_file_meta`，所以也改不到经硬链接连着的别处文件 | 不变量 2 |
| O3 | 基线目录只读：只以只读方式打开，或作为 `clone_file_rooted`、`put_blob` 的源 | 不变量 5 |
| O4 | 记下的 checkpoint 永不改变；restore 与 rebase 只产生新的 checkpoint 或新的 overlay | 不变量 5 |
| O5 | 依赖目录、种子缓存、种子文件、`.git` 不进 overlay，restore 从不写它们 | 不变量 4 |
| O6 | restore 第一次对外写入（`mkdir T` 与 `plan.json`）之后的任何失败都是 `Unknown`：现场保留，不回滚、不重放、不清理。restore 替换或删除的文件先挪进暂存目录，被证明是 `BeforeRestore` 所指内容的副本才删（module-rulings「Proven copies」），所以 restore 期间别的进程写下的内容不会丢 | 不变量 6 |
| O7 | 正确性不依赖监听：全扫与冷扫的结果总等于同一磁盘状态上的 `reconcile_full`；增量 `compute` 只要扫描开始之前完成的写入的事件都已进 inbox 或已 hint，也等于它，事件丢失、溢出都落成全扫。事件还在路上的写入，落进它送达之后的第一次扫描，最迟落进下一次全扫（每小时；`Manual`、`Before*` 与没有路径清单的工具调用之后）。不被监听的全局排除文件改了，增量结果可能与冷扫不同，直到下一次全扫；排除集合不论因为什么变了（仓库配置、index），下一次扫描就全扫，inbox 也从这时起按新的 X 过滤事件（§5.4、§5.5） | design §6.2（"正确性不依赖监听"） |
| O8 | 成员资格只看路径与 git 的规则：大小、二进制、扩展名都不影响一个文件进不进 overlay。只有基线里有、磁盘上没有、且不在排除集合之下的路径才是墓碑（3f:463） | 不变量 1 的前提（design §2 反目标：不丢未发布改动） |
| O9 | 必须一起走的一组路径（改名的两半、类型互换的整组）由 `Overlay::paired` 给出，publish 用它保证批次完整 | 不变量 3 |

## 5. 流程与算法

### 5.1 成员与分类

`compute`、`reconcile_full` 与 `rebase` 的重新分类共用这一节。M 是基线清单，I 是 `read_index` 得到的 `IndexInfo`，`I.tracked` 是其中已跟踪的路径。

**排除集合 X**（仓库内的相对路径，连同其下的一切）：

1. 根下的 `.git`（工作区里它是一个 gitfile）；
2. `config.deps_dirs` 与 `config.seed_caches` 的每一项。config.md 规定依赖目录"不进 overlay、不被监听"，种子缓存"不同步、不 checkpoint"；
3. `config.seed_files()` 的候选里，在磁盘身份上（按卷折叠，见下表）既不是 M、也不是 `I.tracked` 里任何路径的那些。config.md 规定种子文件"不 checkpoint"。这样即使有人改了 `.gitignore`，让 `.env` 不再被 ignore，它也不会以 `Untracked` 进 overlay、被发布出去 [我的决定]。被跟踪的 `.ENV` 占着种子文件 `.env` 的位置时，`.env` 不进 X，那个文件照常比较。

M 里落在 X 之下的条目不参加比较，既不比较内容，也不成为墓碑：依赖目录里若有被跟踪的文件，工作区里的那一份是依赖集的克隆，不是会话的改动 [我的决定]。X 记在 `Tracker` 里；X 变了（仓库配置重新加载，或者 index 的变化让某个种子文件进出了第 3 项），下一次扫描全扫（§5.5）。

**名字的比较**：按 module-rulings「Disk identity follows the volume, git identity follows core.ignorecase」与「Mapping disk entries to tracked paths is disk identity」（即 apfs.md §4.2 B 的"两个问题"）分两种折叠，下表定下每一处用哪种。**按卷折叠**回答"磁盘上是不是同一项"，凡是判定磁盘上的哪一项对应基线或已跟踪的路径都用它：`apfs::volume_caps(ws.root).case_sensitive` 为假时比 apfs 的 `case_fold_key`，为真时按字节。**按 git 折叠**回答"git 把它当成哪个路径"：`IndexInfo.ignorecase` 为真时比 `case_fold_key`，为假时按字节，只用于只改大小写的配对（§5.3 第 1 步，那里两种折叠任一种相等就配）。git 自己的回答（gitio 的 `is_tracked`、`is_ignored`）原样照搬：它们跟着 `core.ignorecase`，只折叠 ASCII。两种都按键查（Addendum 7：`case_fold_eq(a, b)` ⇔ 两个键相等），从不两两比较。默认的设置（大小写不敏感的卷、`core.ignorecase = true`）上两者相同；两者不同时（比如大小写不敏感的卷上设了 `core.ignorecase = false`），按卷的几行照样把改建成 `.ENV` 的种子文件留在 X 里，也照样让一个只改了大小写的已跟踪目录在 ignore 规则只命中新拼写时照常比较，不会因为它被剪掉而让其下的基线文件全成 `Deleted`。

| 用处 | 比较 | 理由 |
| --- | --- | --- |
| 候选资格：磁盘上的 p 是不是 M ∪ `I.tracked` 里的路径 | 按卷折叠：p 的键在 M ∪ `I.tracked` 的键集合里 | 磁盘身份（module-rulings「Mapping disk entries to tracked paths is disk identity」）：大小写不敏感的卷上，只差大小写的磁盘拼写就是那个基线或已跟踪的文件，不论 `core.ignorecase` 怎么设；被跟踪的文件不能因为磁盘拼写换了大小写就被 ignore 规则藏起来 |
| 进不进被 ignore 的目录 d：M ∪ `I.tracked` 里有没有 d 之下的路径 | 按卷折叠：d 的键加 `/` 是不是某个键的前缀（键的有序集合上做范围查找；路径的键等于各段的键用 `/` 连起来，即 `case_fold_key` 的前缀性质，§2）；从不用 gitio 的 `tracked_under`，它只折叠 ASCII，又跟着 `core.ignorecase` | 磁盘身份（同上一行）：否则一个只改了大小写的已跟踪目录，遇上只命中新拼写的 ignore 规则就会被剪掉，它的基线文件全成墓碑，发布出假的删除。用 `tracked_under` 时非 ASCII 的大小写会这样；按 git 折叠时，大小写不敏感的卷上设了 `core.ignorecase = false` 的任何大小写都会这样 |
| 落不落在 X 之下（遍历第 2 到 4 条、`Inbox::note`、拼写解析、restore 第 3 步） | 按卷折叠：路径的键等于某个 X 项的键，或以它加 `/` 开头（同上一行的前缀性质） | 磁盘身份：大小写不敏感的卷上 `.ENV` 占的就是种子文件 `.env` 的位置，不论 `core.ignorecase` 怎么设，都不能以 `Untracked` 被发布；apfs 按 `exclude` 过滤事件也看卷（apfs.md §5.7 第 1、4 步），§5.4 把过滤挪进 `Inbox::note` 因此与原来等价 |
| X 第 3 项：种子文件的候选是不是 M 或 `I.tracked` 里的路径 | 按卷折叠：候选的键等于其中某个路径的键 | 磁盘身份：被跟踪的 `.ENV` 与种子文件 `.env` 在大小写不敏感的卷上是同一个文件，它要照常比较，不能被 X 吞掉 |
| M[p] 取值、`Absent`、配对里"n 不在 M 里" | 字节（`RepoPath` 相等） | 原始差异以磁盘上的确切拼写为键：只改大小写的改名就是旧拼写 `Absent` 加新拼写 `Present`，由 §5.3 第 1 步配成 `Renamed` |
| `staged`：`Present` 的新路径是 `Added` 还是 `Untracked` | gitio 的 `IndexInfo::is_tracked`，只折叠 ASCII，就是 git 自己的回答 [我的决定] | `Added` 与 `Untracked` 的分界照搬 git，不另立规则 |
| 只改大小写的改名（§5.3 第 1 步） | 两种折叠任一种下两个键相等（module-rulings「Case-only pairing pairs under either fold」） | 按 git 折叠保住裁定 T7（`core.ignorecase` 下只改大小写的改名记为改名，大小写敏感的卷上也一样）；按卷折叠让大小写不敏感的卷上设了 `core.ignorecase = false` 时，改名又改了内容的文件仍是一条 `Renamed`，不成为 `Deleted` 加 `Untracked`（发布可能只带走删除、不带新文件）；Review Focus 4 |
| 在磁盘上找一个路径的确切拼写：事件拼写的解析（§5.4）、restore 的"在磁盘上找 q"（§5.7） | 先按字节，再按卷折叠：在上级目录的列表里按键找 | 磁盘身份；FSEvents 报的拼写可能与磁盘上的不同 |
| restore 的 `paths` | 字节；按卷折叠只用来在 `NoSuchPath` 的 `detail` 里指出磁盘上的拼写 [我的决定] | §5.7 第 6 步 |
| `.lww` 规则 | 字节，区分大小写（§5.10） | 同一份规则在每台机器上判定相同 |

M ∪ `I.tracked` 按卷折叠的键集合在 `Tracker` 里（`fold`）：M 的部分每个基线算一次，`I.tracked` 的部分在 index 变了时重算；候选资格、受限目录与 X 第 3 项都查这一个集合。卷是否区分大小写在第一次扫描时读一次，之后不变，所以 `core.ignorecase` 变了不用重算这些键（§5.5 第 2 步）。键只在内存里，从不持久化，也从不当作路径用。

**遍历**（全扫）：从 `ws.root` 起用 `apfs::walk_with(ws.root, &WalkOptions::default(), filter)`。filter 按下面的规则决定一个条目留不留、进不进一个目录。filter 自己不能返回错误，而 `is_ignored` 可能失败（`Unknown`）：这时把错误存进这次扫描的错误格子，filter 返回 false；扫描每取一项都先看这个格子，非空就丢掉 `Walk`、返回那个错误（本模块评审的裁定：filter 出错即停止遍历，扫描返回这个错误）。`walk` 自己产出的 `Err` 项（目录不可读、`too_deep`、`root_changed`、名字不是 UTF-8）同样让扫描失败、原样返回。扫描只读，失败时 `Tracker` 不变（§5.5 第 7 步）。

1. 符号链接从不跟随，是叶子。
2. 目录 d：d 落在 X 之下（按卷折叠）→ 不进。d 不是根而 d 里有名为 `.git` 的项 → 嵌套仓库，`EmbeddedRepo` 告警，按受限目录进入 [我的决定]。`git.is_ignored(ws.root, d)` 为真 → 按上表（按卷折叠），M ∪ `I.tracked` 里有 d 之下的路径就按受限目录进入，否则剪掉（3f:468）。其余的目录正常进入。受限目录里只看键（按卷折叠）在 M ∪ `I.tracked` 里的名字，别的名字不记录、不告警。
3. 文件或符号链接 p：落在 X 之下（按卷折叠，比如第 3 项的种子文件）→ 跳过。否则，不在受限目录里的 p：键（按卷折叠）在 M ∪ `I.tracked` 的键集合里 → 候选；否则 `git.is_ignored(ws.root, p)` 为真 → 跳过；否则是候选。只认 git 的 ignore 规则，ripgrep 的 `.ignore` 不起作用（3f:175）。
4. FIFO、socket、设备文件：落在 X 之下的跳过；其余从不打开（FIFO 因此挂不住扫描），`SpecialFile` 告警，也从不成为候选，所以从不交给 `put_blob_rooted`；它占着的 M 路径按"磁盘上没有"处理 [我的决定]。
5. 目录本身从不成为条目：目录的 mode、mtime，以及空目录，都不记录（git 没有目录条目；裁定 T7：空目录不带）。

**名字**：目录项的原生名先转成 `String`，不是 UTF-8 → `NotText`，不做 U+FFFD 替换。APFS 本来就拒绝建这样的名字，2026-10-22 实测返回 `EILSEQ`。然后 `RepoPath::try_from`，它把 NFD 规整为 NFC（types.md §3；APFS 对 NFC/NFD 不敏感，与 git 的 `core.precomposeunicode` 一致）。字面的 `\` 是普通字符，`a\b.txt` 与 `a/b.txt` 是两个路径（3f:440）。

**内容**（由 §5.2 读出）：普通文件的 blob = BLAKE3(内容)；所有者可执行位（0o100）置位时 mode = 0o100755，否则 0o100644。符号链接的 blob = BLAKE3(目标的原字节)，mode = 0o120000；目标不经 `RepoPath` 校验，非 UTF-8 也原样记。setuid、setgid、sticky 不进 mode，产生 `SpecialModeBits` 告警（裁定 T7）。xattr、BSD 标志、mtime、ctime、属主一概不看（裁定 T7：xattr 不记录、不恢复）。

**原始差异**（p 都是磁盘上的确切拼写，按字节比较）：

- 候选 p，按字节 ∈ M：(blob, mode) 与 (M[p].blob, M[p].mode) 相同 → 不进原始差异（改回与基线相同即撤销覆盖）；不同 → `Present`。
- 候选 p，按字节 ∉ M：`Present`，`staged = I.is_tracked(p)`。
- M 里的路径（X 之外）按字节在磁盘上找不到，或被特殊文件占着 → `Absent`。磁盘上有一个与它只差大小写的名字也一样：那是只改大小写的改名，由 §5.3 配对。

**分类**：`Present` 且 p ∈ M → `Modified`；`Present` 且 p ∉ M → `staged` 为真是 `Added`，否则是 `Untracked`；`Absent` → `Deleted`（mode 0）。然后 §5.3 把一部分 `Absent` 与 `Present` 配成 `Renamed`。

暂存与否只决定新路径是 `Added` 还是 `Untracked`。基线路径只按内容与 mode 比较，`git rm --cached`、skip-worktree、sparse 标志都不看（3f:501）。所以会话里开了 sparse-checkout，没物化的路径就会记成墓碑，status 会把它们列出来；sparse index 则由 gitio 以 `Unsupported` 拒绝。

### 5.2 读一个候选文件

1. A1 = 当前的 `Attrs`。全扫时来自 `walk_with` 的批量读，增量时来自上级目录的单目录列表 `list_dir(ws.root, Some(上级))`。
2. 缓存作保：`cache[p]` 存在，且 `A1.unchanged_since(&cache[p].attrs)` → blob 取缓存，不读内容（计入 `ScanStats.vouched`）。
3. 否则读内容：普通文件用 `apfs::open_rooted_file(ws.root, p)` 逐级 `O_NOFOLLOW`、只读打开，流式算 BLAKE3，16 MiB 及以上走 mmap 加多线程（同 store）；符号链接的目标原字节就在 `A1.kind`（`FileKind::Symlink { target }`）里。稀疏文件与大文件一样整份读，空洞读成零，代价与逻辑大小成正比（裁定 T7）。3c 的 `new_hole_above_four_gib_uses_bounded_original_run_work` 一行留给模块文档补定的就是这一条。大文件阈值不影响成员资格（3f:463）。
4. A2 = `attrs_of(ws.root, p)`。`A2.unchanged_since(&A1)` → 写缓存。
5. 写竞态（裁定 T7）：A1 与 A2 的 kind、inode、size、mode、gen count 有一项不同 → 从第 1 步重来一次；仍然不等 → 用这一次的 blob，路径标 `Unstable`（告警、记进 `unstable`、不写缓存），下一次扫描重扫它。读的时候文件在被写，这一份可能是撕裂的，所以它只出现在 `compute` 的结果里；checkpoint 存下的，总是 `put_blob_rooted` 克隆出的某一刻的完整内容（§5.6 第 5 步）。
6. gen count 不可得（`Attrs::gen_count_known()` 为假：有可写的 mmap，apfs 报告为未知，Addendum 1）：照第 3 步读一次，不重来，标 `Unstable`，不写缓存。checkpoint 不强制写回，只保证包含已经写回的内容：`msync` 或 `munmap` 之后的下一次扫描必含（3d:203、3d:268）。

### 5.3 改名、类型互换与成组

输入是分类之后的两个集合：V（`Absent` 的路径，都在 M 里）与 N（`Present` 的路径）。配对是确定的，只依赖最终状态，与操作顺序、事件顺序无关（3c:99）。下面的配对顺序 [我的决定]：

1. **只改大小写**（module-rulings「Case-only pairing pairs under either fold」；Review Focus 4）：V 按路径升序，对每个 v，取 N 里与它的键在按卷折叠或按 git 折叠下相等（§5.1）、按字节不在 M 里、尚未配对的最小路径 n（N 按两种键各分一次组，两种折叠相同时只分一次，不两两比较），配成一对，不看内容。改名的同时改了内容，也一样配对。默认的设置上两种折叠相同，结果与只看一种无异；两种都按字节时（大小写敏感的卷、`core.ignorecase = false`），这一步什么都不配。
2. **精确内容**：剩下的 V 按路径升序，对每个 v，取 N 里 blob = M[v].blob、类型相同（普通文件对普通文件，符号链接对符号链接）、尚未配对的最小路径 n，配成一对。n 可以在 M 里：改名到同一次扫描里空出来的名字，内容等于搬过来的源就是 `Renamed`，否则是 `Deleted` 加 `Modified`（增补裁定 T7，3c:166）。mode 跟着 n 走：改名之后又改了可执行位，仍是改名。
3. 剩下的 V → `Deleted`；剩下的 N → `Modified`、`Added` 或 `Untracked`（§5.1）。

配成的一对，在 n 上记一条 `Renamed {from: v, blob}`，v 不再另记 `Deleted`（types.md：`Renamed` 兼作 from 的墓碑）。目录改名、移动、移进新建的目录，都自然展开为其下每个文件一条 `Renamed`（3c:98、3c:167）。两个目录互换名字时，每个文件看它的 blob 有没有出现在空出来的路径上，得到 `Renamed` 或 `Modified`（3c:170）。新建之后又改名的文件，只在最终路径上出现。

**类型互换**（裁定 T7）：

- 一个路径上普通文件与符号链接互换 → 一条 `Modified`，mode 在 0o100644/0o100755 与 0o120000 之间变（3a:410）。
- 目录换成文件或符号链接 → 目录下每个基线文件各一条 `Deleted`，该路径上的新内容一条 `Added` 或 `Untracked`（基线里它是目录）。
- 反过来，文件或符号链接换成目录 → 该路径一条 `Deleted`，目录下的新文件各一条 `Added` 或 `Untracked`（3c:146、3c:147、3c:149）。

**成组**：`Overlay::paired(p)` 取下面两种关系的传递闭包里 p 所在的那一组：

- 一条 `Renamed` 的 path 与它的 from；
- 两个条目路径（含 from）q、r，q 是 r 的目录前缀（q 加 `/` 是 r 的前缀）。按 V4，这只可能是类型互换。

publish 选中一个路径时必须带上整组（裁定 T7；增补裁定：批次路径点名 `Renamed` 的任何一半，就选中整个改名）。restore 的路径选择同样取闭包，在当前 overlay 与目标 overlay 的并集上算（§5.7 第 6 步）。

硬链接：每个路径各自独立，不跟踪链接关系，git 也不跟踪。经一个名字写入，所有名字的内容都变，各自记录。

### 5.4 脏集合：`Inbox`

`Inbox::note(event)` 只做常数时间的记账，不碰磁盘：

- `WatchEvent::Changed(paths)`：每个绝对路径去掉 `ws.root` 前缀；不在根之下，或落在 inbox 当前的 X 之下 → 丢弃；转 `String` 或规整成 NFC 失败 → 要求全扫；否则记进路径集合。路径的最后一段是 `.gitignore` → 要求全扫：ignore 规则变了，整棵树的成员资格都可能变 [我的决定]。
- `WatchEvent::MustRescan(dir)`：落在 X 之下 → 丢弃；否则记进子树集合（design §6.2：对该子树重扫）。
- `WatchEvent::Dropped` → 要求全扫。
- 两个集合的大小之和超过上限（缺省 `DEFAULT_DIRTY_CAPACITY`）→ 清空两个集合，要求全扫。

集合按规范化之后的 `RepoPath` 去重，同一路径改多少次都只是一项（3c:42、3f:524）。

**X 在 inbox 里过滤** [我的决定]：监听只排除 `.git`（`WatchOptions.exclude`），依赖目录、种子缓存与种子文件由 `note` 按 inbox 当前的 X 丢弃。apfs 本来就在自己的回调里按 `exclude` 过滤，比较看卷（apfs.md §5.7 第 1、4 步：大小写不敏感的卷用 `case_fold_eq`），`note` 用同样的比较（§5.1 的表：按卷折叠），挪过来代价相同，X 却总是最新的：仓库配置重新加载、X 缩小之后，重新纳入的目录里的写入从下一个事件起就进脏集合，不用重新 watch，也没有新旧两个流交接的空档。X 变了的那次扫描是全扫，它先把新的 X 交给 inbox，再取快照（§5.5 第 2 步）；在这之前按旧的 X 丢掉的事件，由这次全扫补上。往 X 里的目录洪泛写入（`bun install`、`cargo build`）照样会让流报 `Dropped`（apfs.md §5.7 给消费者的契约 (e)），由此而来的全扫同样不进 X（§5.1 的遍历规则）。

**hint**：`Inbox::hint(paths)` 在 daemon 的线程上调用，可以读属性。每个 p 记进路径集合，然后从根起逐级看 p 的前缀（`apfs::attrs_of(ws.root, 前缀)`）。某一级 s 是符号链接时，Edit、Write 经它写到的是它指向的地方（比如 `CLAUDE.md` → `AGENTS.md`）：目标是相对路径，就把它接在 s 的上级之后、再接上 p 在 s 之后的部分，按字面规整 `.` 与 `..`。按字面规整 `..` 与内核的解析一致，前提是被它抵消的那一级不是符号链接，所以 `..` 只许出现在目标的开头：它抵消的是 s 的上级里已经逐级看过、确实是目录的分量；`..` 跟在目标的别的分量之后（如 `x/../AGENTS.md`）→ 要求全扫。结果落在根之内、成得了 `RepoPath`，逐级看下去也不再经过符号链接，就把它也记进路径集合。目标是绝对路径、规整时越过了根、又经过一个符号链接、或者读属性失败 → 要求全扫 [我的决定：只解一跳，其余交给全扫]。某一级已不存在（工具写完又删了）→ 只记 p。

**快照**：每次扫描开始时，在 inbox 的锁里把三样东西整体换出来，留下空的集合继续收事件。扫描期间到达的事件进下一轮（3f:502）。扫描失败时，把快照并回 inbox（3c:43）。

**拼写**（3f:522、3f:523）：FSEvents 报出的拼写可能与磁盘上的不同，例如 `DIR/x` 对 `dir/x`，NFD 对 NFC。扫描时对每个脏路径 p（落在这次扫描的 X 之下的直接丢掉：hint 与 index 的对称差可能交来这样的路径，事件也可能在 inbox 拿到新的 X 之前进来。X 没变时原始差异里本来就没有 X 之下的路径；X 变了，这次就是全扫，§5.5 第 2 步）：

1. 从根往下逐级 `list_dir`，找每一级在磁盘上的确切拼写：字节相等，或者按卷折叠相等（大小写不敏感的卷上 `case_fold_key` 相等；这一级目录的名字按键分组）。同一轮扫描里每个目录只列一次。本文别处说的"拼写解析"就是这三步。某一级找不到 → p 已不存在，按第 3 步处理它在缓存、M、原始差异里的名字。某一级有两个名字都与它按卷折叠相等（只在卷自己的折叠表与 `case_fold_key` 不一致时才可能）→ 要求全扫。
2. p 在磁盘上是文件或符号链接 → 重列 p 的上级目录。其中与 p 的名字按卷折叠相等的每个名字，以及缓存、M、原始差异里在这个目录下、与它按卷折叠相等的每个名字，都按磁盘上的确切拼写重新判定：在磁盘上就走 §5.2；不在，就在原始差异里改成 `Absent`（在 M 里）或删掉（不在 M 里）。只改大小写的改名因此一次就看到新旧两个名字。
3. p 在磁盘上是目录，或者缓存、M、原始差异里有 p 之下的路径 → 整棵子树重判：按 §5.1 的规则遍历 p 之下，再把缓存、M、原始差异里在 p 之下（按卷折叠的前缀）却没被遍历到的路径改成"磁盘上没有"。目录事件因此总是"子树脏"的信号。第 2 步与这一步都适用时（目录被换成了符号链接或文件），两步都做：第 2 步给链接或文件本身一个判定，这一步把它遮住的路径改成磁盘上没有。

**index 与 ignore 规则**：每次扫描都调用 `git.read_index(ws.root)`。index 不在工作区里，在影子仓库的管理目录（session.md §4.2），监听看不到 `git add`。所以把它与 `Tracker` 里记的 `tracked` 比较，对称差里的每个路径都当作脏路径：`git add -f` 一个被 ignore 的文件之后，它进 overlay；`git rm --cached` 之后，它变回 `Untracked` 或被跳过。工作区里的 `.gitignore` 变了 → 全扫（见上）。影子仓库的 `info/exclude` 是 `new` 时抄下的镜像，会话期间不变（Addendum 2）；git 的全局排除文件不被监听，它的改动在下一次全扫时生效。全扫每小时一次，`Manual`、`Before*`（含发布前的 `BeforePublish`）与没有路径清单的工具调用之后也一定全扫，所以不会因此发布错文件 [我的决定]。

### 5.5 `compute` 与 `reconcile_full`

`compute(ws, base, tracker, dirty)`：

1. `base.gen.repo != *ws.repo` → `InvalidInput`。`tracker.base() != Some(&base.gen)` → 本次全扫。
2. `info = ws.git.read_index(ws.root)`，码原样透传。`info.ignorecase` 与上一次扫描的不同 → 本次全扫：git 自己的回答（`is_tracked`、`is_ignored`）与只改大小写的配对（§5.3 第 1 步）都随它变，按卷折叠的键不受影响（§5.1）。由 `ws.config`、`base.manifest` 与 `info.tracked` 算出 X（§5.1；卷是否区分大小写取 `apfs::volume_caps(ws.root)`，`Tracker` 第一次扫描时读一次，失败时码原样透传），与 `Tracker` 记下的不同 → 本次全扫 [我的决定：X 不论因为什么变了（仓库配置重新加载，或者 `git add -f`、`git rm --cached` 让一个种子文件进出 X 的第 3 项）都全扫，不去逐项删掉落进新 X 的原始差异、再把离开 X 的路径记成脏的。这种变化很少，一次全扫之后原始差异里没有过时的条目，离开 X 的路径也都看过了]。X 与 inbox 手里的不同 → 先把新的 X（连同卷是否区分大小写）交给 inbox（§5.4），再取快照。
3. 取 inbox 快照（§5.4），并上 `dirty` 与 `tracker.unstable`。`dirty` 为 `None`、快照要求全扫、或 `tracker.needs_full()` → 全扫。
4. 全扫：按 §5.1 遍历整棵树，每个候选走 §5.2（缓存可以作保），重建原始差异。增量：从 `tracker.raw` 出发，只按 §5.4 重判脏路径、脏子树与 index 有变动的路径。
5. 按 §5.1、§5.3 由原始差异算出 `Overlay { base: base.gen, entries }`。
6. 提交到 tracker：原始差异、缓存、`last`、`base`、`index`、告警、`unstable`；`needs_full` 清零。`Scan.changed` 是新旧 `last` 的条目差。
7. 第 2 到 5 步的任何失败（含 §5.1 的错误格子与 `walk` 的 `Err` 项）：tracker 不变，快照并回 inbox，错误原样返回（§6.1）。

`reconcile_full(ws, base)` 就是用一个临时的新 `Tracker`（缓存为空、要求全扫）跑 `compute`：读遍每个候选文件的内容，不信监听、mtime、size，也不信 `git status`，结果就是真相（tgrep 的 `reconcile_full()`："Always read/verify content"）。它不碰 daemon 的 `Tracker`，也不写 Store。session 用它做 `remove` 的未发布改动检查，以及 `move_workspace` 之后对旧树的核对（session.md §5.6、§5.7 第 19 步）；测试用它作参照。

design §8 的"全量对账"是带着 daemon 的热 `Tracker` 跑 `compute(…, None)`：一次 `getattrlistbulk` 遍历源码树，缓存作保的文件不读内容，"我们不读内容"说的就是它。它在每次发布前（`BeforePublish`）、其它 `Before*` 与 `Manual` checkpoint、没有路径清单的工具调用之后跑（§5.6），并且每小时一次（间隔见 §9）。监听整段失效时，daemon 退化为只做这种全扫（design §6.2）。

全扫不依赖事件：它重读每个候选的属性，一个文件只在这次遍历读到的属性对缓存满足 `Attrs::unchanged_since` 时由缓存作保（gen count 不可得从不作保，apfs.md A6），从不因为"没有收到它的事件"而作保。Addendum 8 要 `Manual` 与 `Before*` 做的 full (cold) scan 就是它：module-rulings「Cold scan」定下，cold 指不依赖事件，不是重读每个文件。本文的"冷扫"另指缓存为空的扫描：`reconcile_full`，以及 `Tracker` 新建或 `reset` 之后的第一次扫描，每个候选都读内容（`stats.cold = true`）。

`compute` 与 `reconcile_full` 的结果相等，靠的是 §4.1 的 gen count 性质：缓存只在 `Attrs::unchanged_since` 成立时作保（kind、inode、size、mode 相同，gen count 可得且相等），这几项都没变，就说明内容没被写过。

### 5.6 `checkpoint`

调用方保证同一会话的 checkpoint 串行，后到的排队、不取消（§5.11）。

1. **核对**（只读 Store）：`store.session(ws.session)` 为 `None` 或已 `Removed` → `NoSuchSession`；会话里没有 `ws.repo` → `NoSuchRepo`；记下的工作区 `tracked = false` → `InvalidInput`；它的 `base != base.gen` → `InvalidInput`（`stale_base`，3f:620：基线已被 `move_workspace` 换掉，再对旧基线存 checkpoint，会写出新旧混合的状态）；`ws.root` 与它的 `path` 的 dev + inode 不同 → `InvalidInput`（`wrong_root`）；只差大小写的写法认作同一个目录。`ws.root` 还要是规范路径：带 `.`、`..` 分量或以 `/` 结尾的根，apfs 与 store 的 rooted 操作以 `InvalidInput` 拒绝（module-rulings「Rooted roots are canonical」）；checkpoint 与 restore 用到它们都在第一次对外写入之前，所以这样的拒绝什么都没写。
2. **扫描**：没有 tracker → `reconcile_full`（只在 daemon 还没有这个工作区的 `Tracker` 时，session 的 `BeforeRollout`、`BeforeRemove` 才这样调用）。有 tracker 时按原因选（Addendum 8 的"Event flush, amended"）：
   - `Manual` 与四种 `Before*` → `compute(ws, base, tracker, None)`：全扫整个工作区的 tracked 与 untracked 集合，不依赖监听，扫描开始之前完成的写入一定在内（`BeforePublish` 的这一次就是发布前的全量对账）。这就是 Addendum 8 要的 full (cold) scan（module-rulings「Cold scan」，§5.5）：作保只看这次遍历读到的属性（`unchanged_since`；apfs.md §4.2 C 实测每次写都改 gen count），所以结果与读遍内容的 `reconcile_full` 相同（O7），只是不重读没变过的文件。
   - `ToolCall` → 增量 `compute(ws, base, tracker, Some(&[]))`。在这之前 daemon 把钩子带来的路径交给 `Inbox::hint`（Edit、Write 等工具点名写的文件；经符号链接写的，hint 把链接指向的路径也记下，解析不了就要求全扫，§5.4），再调用 `Tracker::flush()`：apfs 的 `Watcher::flush()` 让一部分积压的事件进 inbox，Addendum 8 实测它不是屏障，正确性不靠它。工具没有路径清单（Bash 等）时，daemon 改为先调用 `Inbox::require_full()`，这一次就是与 `Manual` 相同的全扫 [我的决定：Addendum 8 让 overlay 在全扫与"晚一个工具调用"之间选，理由见 §9]。

   于是一个 `ToolCall` checkpoint 含有这个工具返回之前完成的写入：Edit、Write 点名的路径（经符号链接时连同它指向的路径）由 hint 带来，Bash 等没有路径清单的由全扫看到。工具调用之外的写者（后台进程、上一个 Bash 留下的后台任务）的写入要等它们的事件送达（实测 15 ms 到 3.3 s，apfs.md §5.9），落进其后的第一个 checkpoint，最迟落进下一次全扫；restore 与发布不受影响，它们之前的 `BeforeRestore`、`BeforePublish` 是全扫。代价：全扫是一次 `getattrlistbulk` 遍历，tool 的源码树温热时 0.20 s（apfs.md §5.9），没有路径清单的 `ToolCall` 因此在 tool 上达不到 design §8 的"改 50 个文件 < 200 ms"（§5.13）；module-rulings 的 T13 一条把它列为阶段末尾交给用户的一项，钩子等不等这次 checkpoint 由 daemon 定。
3. `latest = store.latest_checkpoint(ws.session, ws.repo)`。
4. **没有变化**：`reason` 是 `ToolCall` 或 `Manual`，`latest` 存在，且它的 `base` 与 `entries` 都与这次相同 → 返回 `Checkpointed { checkpoint: latest, recorded: false, .. }`，什么都不写：不 put，也不记行（LayerFS 的 UpToDate；3d:277）。`Before*` 四种原因总是记一行 [我的决定]：调用方需要一个能指认的号（`Restored.before`、session 的 `RemoveReport.checkpoints`）。
5. **存 blob**：已被 `latest` 引用的 blob 都已登记，跳过。其余每个带 blob 的条目（可以并行）：A1 → `id = store.put_blob_rooted(ws.root, path)`（Addendum 8：从工作区存 blob 从不跟随符号链接）→ A2。`id` 等于条目的 blob 且 `A2.unchanged_since(&A1)` → 通过。否则文件在扫描之后又变了：以这次 put 的结果作为它的内容，按 §5.2 第 5 步的规则重来一次；仍然在变，就保留最后一次的 `id` 并标 `Unstable`。`put_blob_rooted` 返回 `NoSuchPath`、`IsDirectory`、`NotFile` 或 `NotDirectory`（扫描之后路径没了、变成了目录或特殊文件，或者某一级上级换成了符号链接或文件）也是写竞态，不让 checkpoint 失败。遍历从不把特殊文件当候选（§5.1 遍历第 4 条），所以 `NotFile` 只来自扫描之后才换了种类的路径，store 在克隆之前就拒绝它（store.md §5.3）。前三种按 §5.4 拼写解析的第 2、3 步重判这个路径；`NotDirectory` 时，从根起按 §5.4 拼写解析的第 1 步逐级找，取最浅的一个已不是目录的分量 s（符号链接、文件或特殊文件），按 §5.4 的第 2 步重判 s 本身、第 3 步重判 s 之下原来的路径：链接或文件本身与它遮住的基线路径于是在同一个 checkpoint 里一起记下。从工作区存 blob 从不跟随符号链接，会话之外的字节进不了 checkpoint（Addendum 8）。然后更新原始差异、重新分类：条目可能消失，配对可能变化。`put_blob_rooted` 克隆之后只看克隆体，所以记下的永远是某一刻完整的内容，即写入序列的某个前缀，不会撕裂（3d:212）。纯改名的 blob 等于一个基线文件，而基线文件不在 blob 存储里，所以第一次也要 put（是克隆，几乎不占空间）；之后的 checkpoint 因为它已被引用而跳过。
6. **记行**：`store.record_checkpoint(Checkpoint { id: 占位（store 不读）, session, repo, at: 现在, reason, entries, base: base.gen })`。返回的号不大于 `latest` 的号 → 这是 store 对同一个 `ToolCall` id 的去重：取回已有的那一行，`recorded = false`。
7. 返回 `Checkpointed`，`changed` 是相对 `latest` 的条目差。

第 5、6 步失败时，错误原样返回。`Busy` 是确定的失败：没有半个 checkpoint，工作区不动，重试得到精确的 checkpoint（3d:207、3d:208、3d:210、3d:281）。扫描到的条目路径连同快照一起并回 inbox（3c:43）。已经 put 的 blob 没有行引用，由 store 的 gc 在 1 小时之后收走（store.md §5.11）。`record_checkpoint` 返回 `Unknown` 且 `outcome = "uncertain"` 时，行可能已经写下：overlay 不重试、不探测，原样返回（不变量 6），下一次 checkpoint 由 `latest_checkpoint` 读到实情。

实现可以省掉一部分重复的读：checkpoint 里，一个缓存不能作保的候选如果不在 M 里，或大小与 M 不同，必然是条目，可以跳过 §5.2 的读，直接 put，用 put 的结果作它的 blob。

### 5.7 `restore`

调用方占用该会话的队列，不持有该 repo 的写锁（§5.11）。R 指会话根（`ws.root` 的上级目录）；T 指 R 下的 `.strata-tmp-restore-<repo>/`，restore 的私有暂存目录，放在会话根里、与工作区同卷 [我的决定]。restore 的写都以 R 为根，经 apfs 的 rooted 操作与 store 的 `get_blob_rooted`（Addendum 8 与「Addendum 8: get_blob_rooted」）：工作区里的路径 q 写作 `<repo>/q`，T 里的名字写作 `T/<n>`，都是 R 之下的 `RepoPath`，逐级不跟随符号链接。会话根是 agent 写得到的地方（兄弟布局的 `../`），T 在第 1 步之后也可能被换成符号链接，所以 T 同样只经 rooted 操作写；T 里临时文件的元数据经 `apfs::set_file_meta` 改（第 10 步）。

**错误时机**（Addendum 3、5、7）：`BeforeRestore` 是对当前、未改动状态的安全 checkpoint，不算对外写入（Addendum 5 refinement 3）。restore 的第一次对外写入是第 10 步的 `mkdir T` 与 `plan.json`（Addendum 7：从这一刻起 T 是 status 看得见的现场）；有能自动了结的旧现场时，从第 10 步清掉它的第一次删除算起。在它之前的失败都是具名拒绝，什么都没写到工作区或 T 里；`BeforeRestore` 若已记下就留着，它是合法的恢复点。在它之后的任何失败都是 `Unknown`，现场保留。

**三个记号**：

- 一份对着当前基线 M 的 overlay X，在路径 q 上的内容 content(X, q)：X 在 q 上有条目 → 条目给出的 (blob, mode)（`Deleted` 即不存在）；q 上没有条目而 `X.hides(q)`（q 是某条 `Renamed` 的 from）→ 不存在；否则 M[q]（不在 M 里即不存在）。
- "在磁盘上找 q"一律用 §5.4 的拼写解析：逐级列上级目录，先按字节、再按卷折叠（大小写不敏感的卷上比 `case_fold_key`）找出磁盘上的确切名字。从不拿一次 `lstat` 的成败当作"同名"：大小写不敏感的卷上 `Foo.ts` 与 `foo.ts` 是同一个文件。
- **证明**一个文件是留着的 checkpoint 所指内容的副本（module-rulings「Proven copies」）：紧挨着它的 `remove_rooted(…, RemoveKind::File)` 之前确认，它的字节等于某个留着的 checkpoint 在某个路径上的内容，即那个 checkpoint 的条目给出的 blob（在 blob 存储里，被这个 checkpoint 引用），或者没有条目时它钉住的基线清单里的 blob（基线随 checkpoint 留着，store.md S10）。确认的办法有两种：重算 BLAKE3；或者 `attrs_of` 对一次已经确认过内容的属性满足 `unchanged_since`（§4.1 的 gen count 性质，与缓存作保同一判法）。证明不了就不删，那个文件留在 T 里。

**阶段 A：检查。** 只读。

1. **T 与现场**：`apfs::attrs_of(R, T)`。不存在 → 继续。是符号链接或不是目录 → `Exists`（`detail.path` 是 T，`reason = "restore_temp_taken"`）。是空目录 → 照用：某次 restore 停在建好 T 与写下 `plan.json` 之间，或停在删掉 `plan.json` 与删掉 T 之间。非空（`list_dir(R, Some(T))` 列出它）→ 现场（Addendum 7），按本节末尾的"现场的了结"判它：逐个按 §5.2 的读法（以 R 为根）重算 BLAKE3，记下读时的属性；能自动了结，就把这些属性留到第 10 步动手；不能 → `Unknown`（`restore_scene`，`detail` 列出没被证明的文件和它们原本的工作区路径）。`store.session(ws.session)` 里会话的 `lifecycle` 为 `Unknown`，或这个工作区的 `WorkspaceRecord.health` 为 `Health::Unknown`（某次操作越过了第一次对外写入，Addendum 4）→ 同样 `Unknown`，`detail` 带 store 里记的现场。
2. 同 §5.6 第 1 步的核对。
3. `target = store.checkpoint(ws.session, req.target)`：`NoSuchCheckpoint` 等 store 的码原样返回，不包装，也不再读别的东西（3c:114）。`target.repo != *ws.repo` → `InvalidInput`。`Overlay::from_checkpoint(&target).validate()` 不过，或者有条目落在 X 之下（X 按 §5.1 由当前的仓库配置、基线清单与 index 算出，按卷折叠比较）→ `InvalidInput`（`reason = "excluded_path"`）；从不悄悄丢掉墓碑（3f:621）。
4. **目标的基线**：`target.base == base.gen` → 目标状态 D 就是目标的 overlay。否则必须有 `req.target_base = Some(tb)` 且 `tb.gen == target.base`，不然 `InvalidInput`。先按 V5 核对目标对 `tb.manifest`，再调用 `rebase(&目标的 overlay, tb, base, ws.store, &RebaseOptions::for_restore(ignorecase, volume_case_sensitive))`（ignorecase 取 `ws.git.read_index(ws.root)` 的 `ignorecase`，volume_case_sensitive 取第 3 步按卷折叠比较时用的 `apfs::volume_caps(ws.root)`），D 取它的 overlay，冲突路径记下（增补裁定 T7/T6/T15：跨基线走 rebase，从不原样回放）。`for_restore` 不做内容合并，也不用 `.lww`：两边都改了的路径取目标的版本，作为冲突报出 [我的决定]。restore 写回的于是都是目标里真实存在过的内容，不合成新内容，rebase 也不写任何 blob；只有基线一侧改过的路径保持当前基线的内容，不会被悄悄改回旧基线。目标基线总在：每个留着的 checkpoint 都钉住它的 `BaseGen`（Addendum 5，store.md S10）。
5. **扫描**：按 §5.6 第 2 步、以 `BeforeRestore` 为原因扫描（有 tracker 时是全扫，不依赖事件，扫描开始之前完成的写入一定在内），得到当前 overlay C；扫描读到的每个路径的 `Attrs` 留作下面比较的基准。
6. **选路径**：`paths = None` → C 与 D 的全部条目路径，加上两边 `Renamed` 的 from；其余路径在两边都等于 M，不用动。`paths = Some(ps)` → 每个 p 按字节比较 [我的决定]：p 必须等于、或者是某个候选路径的目录前缀；候选是 C、D 与目标 overlay（rebase 之前）的条目路径和 from，以及两边基线清单里的路径。都不是 → `NoSuchPath`；只有按卷折叠才对得上时，`detail.on_disk` 给出那个拼写，帮人改参数。选中 p 本身及其下的全部候选，再取 §5.3 的成组闭包，在 C 与 D 的条目路径（含 from）的并集上算：一个路径在 C、另一个在 D，一个是另一个的目录前缀，也算一组（比如当前一条 `Untracked` 的符号链接 `src` 与目标里的 `src/new.ts`）。
7. **计划**：对每个选中的路径 q，目标 = content(D, q)，当前 = content(C, q)。两者不同才动它：目标不存在 → 删除；目标存在 → 写入（只有 mode 不同，也按写入处理）。写入的路径缺的上级目录要建。这次 restore 弄空的目录（它的每一项都被这次 restore 挪进了 T，或者是同样被这次 restore 弄空的子目录），目标里又没有路径在它之下，就要删（module-rulings「Empty directories, amended」）；restore 没弄空的目录从不删。不写目录的 mode，不建空目录（裁定 T7）。改名因此恢复得对：当前是 `b.ts` 上的 `Renamed { from: "a.ts" }`，目标是改名之前 → 删 `b.ts`（目标里不存在）、写 `a.ts`（当前 `C.hides("a.ts")`，不存在；目标是 M 的内容）。只改大小写的改名同理：删 `foo.ts`、写 `Foo.ts`。
8. **预检**（按下面的顺序查，返回先遇到的那一个拒绝）：
   - 要写入的路径 q：在磁盘上找 q。找到的名字正是计划要删的路径（只改大小写的改名，旧拼写在删除之列）→ 算作已腾出：第 11 步删掉它之后，第 13 步才建 q。找到的就是 q、计划要替换它 → 继续。找到的是别的东西（不在 C 里的：被 ignore 的文件、排除集合里的目录、特殊文件，或者扫描之后才出现的）→ `Exists`（`detail.path`）：覆盖它就丢了不在任何 checkpoint 里的字节（不变量 1）。q 的每一级上级在磁盘上必须是目录、不存在，或者是计划要删的路径（类型互换）；否则（文件、符号链接或特殊文件，不论在不在 C 里）→ `Exists`（`detail.path` 是那一级）。挡路的东西一律是 `Exists`，在第 9 步记 `BeforeRestore` 之前返回 [我的决定：不分它是扫描之前就在、还是扫描之后才出现。扫描不进被剪掉的 ignore 目录、不留它们的属性，分不出这两种；两种情况都要人先挪开它]。
   - 要删的目录里有计划之外的东西 → `Exists`。
   - 要替换或删除的文件、要删的目录带 `UF_IMMUTABLE`、`SF_IMMUTABLE`、`UF_APPEND` 或 `SF_APPEND` → `ReadOnly`。
   - 要替换或删除的文件 gen count 不可得（`gen_count_known()` 为假：有可写的 mmap）→ `OpenFiles`。
   - 要克隆的基线文件不在，或大小与清单不符 → `Corrupt`（基线文件缺了是 `Corrupt`，从不当作新文件，Addendum 4）。`req.verify` 时再重算它的 BLAKE3，并对每个要写回的 blob 调 `store.verify_blob`，不符 → `Corrupt`。
   - 要替换或删除的路径，`attrs_of(ws.root, q)` 对第 5 步的 `Attrs` 不满足 `unchanged_since` → `Busy`（`workspace_changed`）：扫描之后有人写了工作区。这时什么都没写，重试即可。

**阶段 B：记 BeforeRestore。** 安全 checkpoint，不算对外写入（Addendum 5）。

9. 用第 5 步的扫描结果，按 §5.6 第 3 到 6 步记一个 `BeforeRestore` checkpoint（`Before*` 总是记一行），它的 entries 是 C。存 blob 时发现一个要替换或删除的文件在扫描之后又变了（put 的结果与扫描不同，§5.6 第 5 步）→ 不记行，返回 `Busy`（`workspace_changed`）：put 出来的 blob 没有行引用，是缓存填充（Addendum 6 refinement 4）。计划之外的文件又变了，按 §5.6 第 5 步取最后一次 put 的内容，不拒绝：restore 不动它们，一个一直在写的日志文件不该让 restore 永远 `Busy`。记行本身的失败照 §6.2 原样返回。

**阶段 C：落到工作区。** 从第 10 步的第一次写起（第 10 步开头对旧现场的再核对只读），任何失败都是 `Unknown`，现场保留（§6.3）。这是 §5.12 的投影，落在活的工作区上，外加三条安全网：内容先在 T 里备好；被替换或删除的文件先挪进 T，被证明是 `BeforeRestore` 所指内容的副本才删（上面的"证明"）；新建用 `RenameMode::Excl`，从不覆盖计划之外的东西。blob 用 `store.get_blob_rooted(id, R, T/<n>)`、基线文件用 `apfs::clone_file_rooted` 先取到 T 的正下方，再用 `apfs::rename_rooted(R, …)` 挪到位置上（Addendum 7、8 的"Rooted projection"与「Addendum 8: get_blob_rooted」）；会话根之下的任何路径都不按路径写。

10. 第 1 步判为能自动了结的现场：先再核对一遍 T 里每个文件的属性，有一个对第 1 步记下的不再满足 `unchanged_since` → `Busy`（`workspace_changed`），什么都没删，`BeforeRestore` 留着；都满足 → 按"现场的了结"逐个删掉它们：每个文件在自己的 `remove_rooted(R, T/<名字>, RemoveKind::File)` 之前再核对一次属性，这就是它的证明（第 1 步按 §5.2 的办法重算过它的 BLAKE3）；中途有一个又变了 → `Unknown`（`restore_scene`），它留在 T 里。最后删 `plan.json`，再 `remove_rooted(R, T, RemoveKind::EmptyDir)`（`plan.json` 最后删：中途崩溃时，T 里剩下的文件仍有它作证）。`apfs::mkdir_rooted(R, T, 0o700, false)`；返回 `Exists` 时，`attrs_of(R, T)` 是目录、`list_dir(R, Some(T))` 又列不出任何项，就照用，不是（第 1 步之后才出现的符号链接、文件或非空目录）→ 还没删过旧现场的文件时是 `Busy`（`workspace_changed`），什么都没写，删过之后是 `Unknown`。用 `apfs::create_file_rooted(R, T/plan.json, 0o600)` 写 `plan.json`：`before`、`target`，以及每一项操作的路径、它在 T 里的名字、那个名字下可能出现的两种内容的 (blob, mode)，即要写入的目标内容与 content(C, q)。然后 `plan.json`、T、R 逐个 `fsync`（T 的句柄取自 `open_rooted(R, T)`，R 的取自 `apfs::open_root(R)`），再对 R 的句柄做一次 `apfs::full_fsync`（`F_FULLFSYNC`，同 gitio.md §5.6 `ensure_shadow` 的第 6 步）：从这里到第 14 步 T 一直非空，崩溃之后现场也看得见。每个要写入的路径，在 T 的正下方备一个临时文件，名字是操作的序号 n：
    - blob → `store.get_blob_rooted(id, R, T/<n>)`（「Addendum 8: get_blob_rooted」：从 R 起逐级不跟随符号链接，T 在这之前被换成符号链接 → `NotDirectory`，R 之外什么都没写），得到 BSD 标志已清、mode 0644、mtime 为当前时刻的文件；可执行就对 `open_rooted_file(R, T/<n>)` 的句柄 f 调 `apfs::set_file_meta(&f, &FileMeta { clear_flags: false, mode: Some(0o755), mtime: None })`。
    - 基线文件 → `apfs::clone_file_rooted(base.dir/q, R, T/<n>, None)`，再对 `open_rooted_file(R, T/<n>)` 的句柄 f 依次：`apfs::set_file_meta(&f, &FileMeta { clear_flags: true, mode: Some(0o644 或 0o755), mtime: None })` 清掉克隆带来的 BSD 标志（封存基线时可能用了 `uchg`，clonefile 会一起复制，带着它连 chmod 都 `EPERM`）并设 mode；在同一个句柄上用 `flistxattr`、`fremovexattr` 清掉扩展属性（strata 从不恢复 xattr，Addendum 1；前一次调用已核对 `st_nlink == 1`，之后才出现的别名指的也是这份克隆）；最后 `apfs::set_file_meta(&f, &FileMeta { clear_flags: false, mode: None, mtime: Some(现在) })` 把 mtime 设为当前时刻 [我的决定]。理由同 `get_blob_rooted`：增量构建要把内容变了的文件当成变了。
    - 符号链接 → 目标字节取自 `store.read_blob(id, 1024)`，或者基线目录里那个链接的 `attrs_of(base.dir, q)`（`FileKind::Symlink { target }`），再 `apfs::symlink_rooted(R, T/<n>, target)`。

    句柄上的元数据改动一律经 `apfs::set_file_meta`（module-rulings「Metadata changes through a handle go through apfs::set_file_meta」）：`fchflags`、`fchmod`、`futimens` 改的是 inode，T 里的名字若在放进去之后被换成了别处文件的硬链接，直接改就改到了别处。`set_file_meta` 只改 `st_nlink == 1` 的普通文件，否则什么都不改（硬链接 → `ReadOnly`，`reason = "hard_link"`）。它的拒绝发生在第一次对外写入（`mkdir T` 与 `plan.json`）之后，所以不是具名拒绝，是 `Unknown`，现场保留（§6.3）；这时工作区还一字未动，只是 T 里多了一个不在任何 checkpoint 里的名字。

    用 `attrs_of(R, T/<n>)` 记下每个临时文件的属性，供第 14 步写缓存。
11. **删除**，深的先删：`rename_rooted(R, <repo>/q, T/rm-<n>, RenameMode::Excl)` 把它挪进 T；某一级上级是符号链接 → apfs 拒绝（`NotDirectory`），什么都没动，停。再看挪过来的文件：`attrs_of(R, T/rm-<n>)` 对第 5 步的属性满足 `unchanged_since`，说明自扫描起没被写过，它的字节就是 content(C, q)，即 `BeforeRestore` 在 q 上的内容：q 是 C 的条目时是它引用的 blob，否则是它钉住的基线 → 已被证明，紧接着 `remove_rooted(R, T/rm-<n>, RemoveKind::File)`。否则 → `Unknown`，现场保留：那是 restore 期间别人写下的新内容，留在 T 里，不丢。
12. **删目录**：第 7 步记下的、这次 restore 弄空的目录，深的先删，只用 `remove_rooted(R, <repo>/d, RemoveKind::EmptyDir)`（module-rulings「Empty directories, amended」）。目录不空（apfs 返回 `NotEmpty`：第 11 步之后有人在里面建了东西）是写竞态，按 module-rulings「NotEmpty from apfs」：第一次对外写入之前是 `Busy`，之后是 `Unknown`。这一步总在它之后，所以返回 `Unknown`，现场保留，那个目录与里面的新东西都不动。
13. **写入**，浅的先写：缺的上级目录用 `apfs::mkdir_rooted(R, <repo>/上级, 0o755, true)` 建（某一级是符号链接 → `NotDirectory`，什么都没写，停）。目标位置有文件 → `rename_rooted(R, T/<n>, <repo>/q, RenameMode::Swap)`，旧文件因此换到了 `T/<n>`，按第 11 步的同一规则证明之后才 `remove_rooted`，证明不了 → `Unknown`，旧内容留在 T 里。目标位置没有东西 → `RenameMode::Excl`，目标已被占（`Exists`）→ `Unknown`，不覆盖任何东西。

**阶段 D：收尾。**

14. T 里只剩 `plan.json` → `remove_rooted(R, T/plan.json, RemoveKind::File)`，再 `remove_rooted(R, T, RemoveKind::EmptyDir)`（本模块建的空目录，Addendum 7）。还有别的东西，或者删 T 时得到 `NotEmpty`（这期间有人往 T 里放了东西）→ `Unknown`，现场保留。有 tracker 时，把第 10 步记下的属性与 blob 写进缓存（改名不改 inode 与 gen count），删掉的路径移出缓存，对写过的路径做一次增量重判；这些路径的监听事件随后到达时，缓存直接作保。返回 `Restored`。

restore 只在第 10 到 14 步写会话根之下（T 与工作区），次数是 O(选中的路径数)。它不对工作区里的文件做 `F_FULLFSYNC`，与 git checkout 一样 [我的决定]：`BeforeRestore` 已经持久地记在 Store 里，`plan.json` 也已落盘，掉电之后 restore 之前的状态总能恢复，工作区由下一次冷扫重新对账。

**现场的了结**（module-rulings「Proven copies」）：非空的 T 是现场。status 读它的 `plan.json` 报出；session 的 `rm` 在它存在时返回 `Dirty`（`detail` 指出 T），除非 `--discard`，这时会话根连同 T 照常经 `store::retire_dir` 退役（Addendum 7）。了结有两条路：

- **下一次 restore 自动了结**：`plan.json` 读得出来，它记的 `before` 与 `target` 两个 checkpoint 都还在（`store.checkpoint` 读得到，于是它们引用的 blob 与钉住的基线也在），T 里除了 `plan.json` 都是文件或符号链接，而且每一个都逐个证明（上面的"证明"），不只信 `plan.json`：`plan.json` 记下这个名字原本的工作区路径 q 与它可能装着的内容：content(C, q)，以及要写入的目标内容（删除操作没有这一种）。先核对这些内容确实是那两个 checkpoint 在 q 上所指的：content(C, q) 等于 `before` 在 q 上的内容（它的条目给出的，或者没有条目时它钉住的基线清单里 q 的 blob）；目标内容等于 `target` 在 q 上的条目给出的，或者（restore 把 q 改回当前基线时）`before` 的基线清单里 q 的 blob。再按 §5.2 的读法（以 R 为根）重算这个文件的 BLAKE3，等于其中一种的 blob，记下读时的属性。第 10 步删每一个之前紧挨着再核对它的属性（`unchanged_since`），然后才 `remove_rooted`：各自核对、各自删，`plan.json` 最后删，再删掉空的 T，然后照常做这一次 restore：上一次 restore 就此放弃，工作区停在它做到的地方，这一次的 `BeforeRestore` 把那个状态记下来。
- **人工了结**：有任何一个文件不在 `plan.json` 里、字节对不上，T 里有目录，或者 `plan.json` 不在、读不了，或者那两个 checkpoint 已被回收，restore 返回 `Unknown`（`restore_scene`）。没被证明的文件装着可能不在任何 checkpoint 里的字节，多半是 restore 期间别的进程写下的。人把想留的挪回工作区（下一次 checkpoint 会记下），再删掉 T；或者 `rm --discard`。

### 5.8 `rebase`

`rebase(overlay, old, new, store, opts)` 从不写工作区。

1. `overlay.base != old.gen`，或 `old.gen.repo != new.gen.repo` → `InvalidInput`；`overlay.validate()` 或 V5（对 `old.manifest`）不过 → `InvalidInput`。
2. U = overlay 的全部条目路径，加上 `Renamed` 的 from。
3. 对 U 里的每个 p，三方都是 `Option<(blob, mode)>`：合并基 = `old.manifest[p]`；会话一侧 ours = overlay 在 p 上的内容（`Deleted` 与 from 为不存在）；新基线一侧 theirs = `new.manifest[p]`。按顺序判：
   1. ours = theirs → 取它。两边一致，这个路径离开 overlay。
   2. theirs = 合并基 → 取 ours：上游没碰它。
   3. `lww_matches(&opts.lww, p)` → 取 ours，记进 `lww`，不算冲突，也不试合并。design §4 说 `.lww` 命中的文件"最新 checkpoint 的版本胜出"；rebase 投影的 overlay 来自刚记下的 checkpoint（session 的 `BeforeRollout`），比新基线新，所以会话一侧胜出 [我的决定]。不能保证这一点的调用方（restore）传空规则。
   4. `opts.merge`，ours 与 theirs 都是普通文件，合并基不存在或者也是普通文件：
      - 内容：会话没改内容（ours 的 blob = 合并基的 blob）→ 取 theirs 的 blob；上游没改内容 → 取 ours 的 blob；两边都改了 → 三份都不超过 `MERGE_MAX_BYTES` 才合并，否则冲突。合并基与 theirs 从 `old.dir`、`new.dir` 读，文件不在或读出的 BLAKE3 与清单不符 → `Corrupt`（不在也从不当作新文件，Addendum 4）；合并基不存在时用空串（add/add）。ours 用 `store.read_blob(blob, MERGE_MAX_BYTES)` 读。`gitio::merge3(合并基, ours, theirs)` 得到 `Clean(bytes)` → 它的 BLAKE3 等于 ours 或 theirs 的 blob 就直接用那个，否则 `store.put_bytes(bytes)`：这个 blob 没有登记，只受 store 的 1 小时窗口保护（Addendum 4），调用方要在同一次操作里用 `project` 把它克隆出去，之后的 checkpoint 才登记它；得到 `Conflict` → 冲突。二进制文件由 merge3 给出整份冲突（gitio.md）。
      - mode：会话改了可执行位，就取 ours 的；否则取 theirs 的。合并基不存在时，任一边可执行就可执行。
   5. 其余情况（一边删、一边改；类型不同；符号链接两边改得不同；`merge = false`）→ 冲突。

   冲突 → 取 ours，记进 `conflicts`。
4. 最终状态：U 里的路径取上面的结果，U 之外的路径就是新基线。
4a. **前缀冲突** [我的决定]：最终状态里每个活路径带一个来历：S（取自会话一侧：ours、合并结果或 `.lww`）或 B（等于新基线：不在 U 里，或者结果等于 theirs）。两个活路径 a、b，a 是 b 的目录前缀，而两者来历不同，真实目录里就放不下：会话把目录 `d/` 换成了文件 `d`，上游在 `d/` 下加了 `d/new.ts`；或者会话加了 `lib/x.ts`，上游把 `lib` 换成了文件或符号链接。这时保留 S 一侧，B 一侧的那些路径在最终状态里改为不存在（它们都在新清单里，于是成为墓碑），a 与 b 都记进 `conflicts`。会话的改动因此一个不丢，结果满足 V4，`project` 也不会把它当成损坏。两个 S 路径之间不会有这种冲突（它们来自同一个真实工作区），两个 B 路径之间也不会（它们来自同一棵 tree）。
5. 按 §5.1 的分类与 §5.3 的配对，对 `new.manifest` 重新分类，得到新的条目。新基线里没有的 `Present` 路径，原来的条目是 `Untracked` 的仍是 `Untracked`，否则是 `Added`；改名重新配对，§5.3 第 1 步的两种折叠取 `opts.ignorecase` 与 `opts.volume_case_sensitive` [我的决定：卷是否区分大小写放进选项（`for_move` 从 `apfs::volume_caps(&new.dir)` 读，`for_restore` 由 restore 传入），不让 rebase 自己探卷：rebase 的结果只由参数决定，测试不用磁盘映像就能覆盖两种卷]。结果只按新基线的 tree OID 取键，不复用对旧基线算的任何东西（3c:205）。
5a. **blob 不变量**（Addendum 7）：新 overlay 里每条带 blob 的条目，blob 必须在 blob 存储里。取自会话一侧的（来自 checkpoint）与合并结果（`put_bytes`）已经在。条目的 blob 等于 `new.manifest[p].blob`、又不是输入 overlay 里的某个 blob 的，可能不在：内容就是新基线的那一份，只因为 mode 不同才留在 overlay 里，比如会话改了可执行位、上游改了内容（`Modified { blob: theirs 的 blob, mode: 0o100755 }`），或者合并结果恰好等于 theirs、又带着会话加的可执行位。按 blob 判，不按条目来自哪一侧判：凡是这样的条目，都用 `store.put_blob(new.dir/p)` 把那个基线文件存进去（基线是 strata 自己的目录，用普通的 `put_blob`，Addendum 8；克隆，几乎不占空间），返回的 id 就是存下的那份字节的 BLAKE3，必须等于清单里的 blob，否则 `Corrupt`：这就是 Addendum 7 要的"先按清单核对，再存"。它与合并结果一样只受 1 小时窗口保护，调用方在同一次操作里用 `project` 把它克隆出去。`merge = false` 时不会有这种条目：每个路径要么离开 overlay，要么取 ours，条目的 blob 都是输入 overlay 里的。
6. `dropped` = overlay 里原有、不在 `lww` 里、而新 overlay 里既没有条目也不是任何 from 的路径。返回 `Rebased { overlay: Overlay { base: new.gen, .. }, conflicts, dropped, lww }`，三个列表都按路径排序。

两个例子。会话把 `d/` 改名为 `x/`，上游把 `d/` 改名为 `y/`：`d/*` 两边一致（都不存在），`x/*` 上游没碰，于是会话的 `x/*` 成为 `Added`，不冲突（3d:23 一行的理由）。会话把 `f` 改了名，上游改了 `f` 的内容：`f` 一边删、一边改，冲突，保留会话的改名（3d:20 一行的理由）。

### 5.9 `build_manifest`

1. 用 `apfs::walk_with(dir, &WalkOptions::default(), …)` 遍历整棵导出的树（`Err` 项原样返回），§5.1 排除集合 X 的前两项不进：根下的 `.git`、`config.deps_dirs` 与 `config.seed_caches`（基线 generation 目录里克隆进来的依赖集就在这里，Addendum 3）。X 的第三项（种子文件）在这里不适用：导出的树里只有被跟踪的文件。
2. 普通文件 → BLAKE3(导出后的字节)，mode 按所有者可执行位取 0o100644 或 0o100755，size；符号链接 → BLAKE3(目标的原字节)，0o120000，size 是目标的长度；目录往下走；FIFO、socket、设备文件 → `Unsupported`；名字按 §5.1 的规则处理，不是 UTF-8 → `NotText`。
3. 返回 `BaseManifest { tree, entries }`。登记由建这个基线 generation 的 session 在 repo 写锁里调 `Store::record_base_gen` 完成，清单与 generation 一起登记（Addendum 3）；`compute` 用的 `Base.manifest` 是 store 里登记的那一份。

按导出后的字节而不是 git 的 blob 算，是因为工作区是这棵树的克隆：导出时做过的过滤（行尾、编码、smudge，Addendum 1 的 gitio 裁定），比较的两边都一样。代价是这个 tree 的字节数，每个基线 generation 一次：tool 是 4196 个文件、42.3 MB。

### 5.10 `.lww` 规则

规则文件是仓库里的 `.strata/lww`（design §4）。它本身是普通的仓库文件，照常进 overlay、照常发布。语法与语义取 LayerFS `/.lww` 的方言（3a:419–3a:422），路径去掉前导的 `/`：

- 一行一个模式。空行与 `#` 开头的行是注释；`!` 开头表示否定；结尾的 `/` 忽略。
- 含 `/` 的模式锚定在仓库根，不含 `/` 的匹配任意深度的名字。
- `*`、`?` 只在一个名字之内匹配；`**` 跨名字；`**/` 匹配零个或多个目录，连续的 `**/**/` 同样可以匹配零个目录。
- 一个模式匹配路径本身，或者它的任何一级上级目录。
- 最后一条匹配的规则决定结果，否定规则把匹配清掉。
- `%include <path>` 读另一个仓库内路径上的规则文件，可以嵌套，连同 `.strata/lww` 最多读 16 个文件，超出的 `%include` 忽略。文件不存在、不是普通文件、不是 UTF-8，都当作空文件 [我的决定]：读不全只会让更多文件按普通文件合并或报冲突，不会让文件被悄悄覆盖。
- 区分大小写，按 NFC 之后的字节比较，不随机器的 `core.ignorecase` 变化 [我的决定]：同一份规则在每个会话、每台机器上给出同样的判定。

规则从哪棵树读：rebase 读新基线的目录（`RebaseOptions::for_move`），与 LayerFS 在 Workspace Commit 时读 "theirs" 的规则一致；restore 不用规则；publish 决议时读哪棵树，由 publish.md 定。overlay 只提供 `LwwRules` 与 `lww_matches`，发布时的胜负（发布组内后应用的批次胜，同一会话里较大的 `CheckpointId` 胜）归 publish（裁定 T10）。

### 5.11 并发与锁

overlay 是库，自己不加锁，靠调用方遵守下面的约定（由 daemon.md 与 session.md 实现）：

- 同一工作区的扫描串行。带 `Tracker` 的调用拿 `&mut Tracker`，类型上就是串行的，session 的 `BeforeRollout`、`BeforeRemove` 带着 daemon 交来的那一个也一样；不带的调用（还没有 Tracker 时的这两种、`reconcile_full`）在该会话的队列里。
- 同一会话有 checkpoint 进行中时，后到的 checkpoint 或 rm 排队，从不取消进行中的那个（裁定 T7）；有 publish 进行中时，后到的 rm 或 checkpoint 排在该 repo 的写锁之后（增补裁定）。
- restore 占用该会话的队列，与同一会话的其他操作串行；它不持有该 repo 的写锁（Addendum 3：写锁只包短的临界区），因为它只写本会话的工作区与 T，不碰 refs、影子仓库与基线目录。当前基线被工作区行引用，gc 不会收走（Addendum 4 的 gc 守则 (c)）；跨基线时的目标基线同样钉着：每个留着的 checkpoint 都钉住它的 `BaseGen`（Addendum 5；store.md S10）。基线文件仍然读不到（被人改过）是 `Corrupt`：第 8 步时是具名拒绝；第 10 步时是 `Unknown`，这时工作区还没动。
- 工作区里的进程从不被阻塞：工作区是真实目录，读写不经过 daemon（3d:211、3d:213）。扫描与写入并发时，扫描期间的写入要么被这一次看到，要么进下一次（§5.4 的快照，§5.2 的写竞态规则）。
- `Inbox` 是 `Send + Sync`，监听线程随时调用 `note`，从不等扫描。
- `move_workspace` 交换之后，daemon 调用 `Tracker::reset` 再 `watch`（Addendum 3：交换之后做一次冷对账，属性缓存与这个工作区的 FSEvents 流都重置）：新树的 inode 全变了，旧缓存没有用。status 的改动列表在读 status 时重算。

### 5.12 投影：`project`

`project(store, overlay, dst)` 把 overlay 的条目落到 dst 上（Addendum 3）。dst 是 `overlay.base` 那棵树的一份未改动的克隆：session 的 `move_workspace` 先把新一代的基线克隆成内部 staging，再把 rebase 得到的 overlay 投影上去。dst 是调用方的内部 staging，所以这里没有 restore 的那几条安全网，失败时 dst 停在半途，由调用方按 Addendum 3、4、7 的错误时机处理。写的方式与 restore 相同（Addendum 7、8 的"Rooted projection"）：以 dst 为根，blob 先用 `store.get_blob_rooted(id, dst, <临时名>)` 取到 dst 正下方的私有临时名（「Addendum 8: get_blob_rooted」），再用 `apfs::rename_rooted(dst, …)` 挪到位置上，删除用 `apfs::remove_rooted`，句柄上的元数据改动经 `apfs::set_file_meta`；dst 之下的任何路径都不按路径写。

1. `overlay.validate()` 不过 → `InvalidInput`；dst 不在 → `NoSuchPath`；dst 的真实路径不在 `<store.root()>/sessions/` 之下 → `InvalidInput`（`reason = "dst_outside_sessions"`）：一道便宜的防线，`project` 写不到主 checkout、基线目录或 blob 存储里（不变量 2、5）[我的决定]。
2. **删除**，深的先删：每条 `Deleted` 的 path 与每条 `Renamed` 的 from，`remove_rooted(dst, path, RemoveKind::File)`。路径不在（`NoSuchPath`）或是目录（`IsDirectory`）→ `Corrupt`：dst 不是基线的完整克隆（基线文件缺了是 `Corrupt`，从不当作新文件，Addendum 4）。上级某一级是符号链接 → apfs 返回 `NotDirectory`，原样返回，dst 之外什么都没碰。
3. **删目录**：第 2 步弄空、且 overlay 里没有路径在其下的目录，深的先删，`remove_rooted(dst, d, RemoveKind::EmptyDir)`。dst 是这次操作自己的 staging，这些目录是它克隆出来的（Addendum 7：操作可以删掉自己建的空目录）。
4. **写入**，浅的先写：缺的上级目录用 `apfs::mkdir_rooted(dst, 上级, 0o755, true)` 建；上级某一级是文件 → 它必须已在第 2 步删掉，否则 `Corrupt`。每个条目先在 dst 正下方备一个临时名 `.strata-project-<序号>`（已存在就换下一个序号）[我的决定]：普通文件 → `store.get_blob_rooted(id, dst, <临时名>)`，可执行就对 `open_rooted_file(dst, <临时名>)` 的句柄 f 调 `apfs::set_file_meta(&f, &FileMeta { clear_flags: false, mode: Some(0o755), mtime: None })`（它的拒绝原样返回，dst 停在半途，由调用方按错误时机处理）；符号链接 → `store.read_blob(id, 1024)` 取目标，`apfs::symlink_rooted(dst, <临时名>, 目标)`。再 `rename_rooted(dst, <临时名>, path, RenameMode::Excl)`；目标位置已有东西（`Exists`：`Modified`，或改名、新建到一个基线路径上）→ 改用 `RenameMode::Swap`，换出来的必须是文件或符号链接（基线的那一份，字节就是基线的），随即 `remove_rooted(dst, <临时名>, RemoveKind::File)`；换出来的是目录 → `Corrupt`。
5. 不做 `F_FULLFSYNC`：交换之前由调用方按 Addendum 3 的持久性规则同步。结束时 dst 正下方没有残留的临时名。

restore 的阶段 C 是同一个算法，落在活的工作区上（§5.7）。代价是 O(条目数) 次取 blob（克隆）与改名，不读任何文件的内容。

### 5.13 复杂度与 design §8 的数字

| 操作 | 代价 | design §8 |
| --- | --- | --- |
| 增量 `compute` | O(脏路径数 × 上级目录的条目数) + O(被读文件的字节数) + O(overlay 的条目数)（分类与配对） | 热路径只读元数据 |
| `checkpoint`（`ToolCall`，钩子带来路径，有 tracker） | 一次增量扫描 + 每个新 blob 一次 `put_blob_rooted`（克隆加哈希）+ 一个 SQLite 事务 | 改 50 个文件 < 200 ms |
| `checkpoint`（`Manual`、`Before*`、没有路径清单的 `ToolCall`；有 tracker） | 一次全扫：不依赖事件，缓存作保（tool 的源码树温热 0.20 s，apfs.md §5.9）+ put + 一个事务 | 全量对账 < 3 s；没有路径清单的 `ToolCall` 在 tool 上超出 200 ms（§5.6 第 2 步） |
| 全量对账 `compute(…, None)`（热缓存） | 一次 `getattrlistbulk` 遍历源码树，剪掉的 ignore 目录不进；缓存作保的文件不读 | tool 源码树 < 3 s |
| `reconcile_full`（冷） | 遍历，加上读遍候选文件：tool 42.3 MB、staff-api 38.5 MB、admin 30.9 MB（2026-10-22） | 同上的目标同样适用 |
| `restore` | 一次全扫（`BeforeRestore`）+ O(选中的路径数) 次克隆与改名 | — |
| `rebase` | O(n log n)，n 是 U 的路径数，加上两边都改了的文件的字节数 | — |
| `build_manifest` | 每个基线 generation 一次，O(树的字节数) | 新建会话 < 5 s |
| `project` | O(条目数) 次克隆与改名，不读内容 | 新建会话 < 5 s 的一部分（move_workspace） |

内存：缓存每个文件约 100 字节，tool 约 0.5 MB；折叠键与 M、`I.tracked` 同量级。这些数字是目标，首个 spike 实测后修正（design §8）；§7 用计数而不是计时来断言工作量（3c:103、3c:104）。

## 6. 失败与恢复

共同规则（module-rulings.md「Common」与 Addendum 3 到 7）：具名拒绝只发生在第一次对外写入之前；之后的任何失败都是 `Unknown`，现场保留，不重放、不清理，由 status 报出。errno 按 types.md §6.3 映射，overlay 自己在句柄上做的 libc 调用失败时经 apfs 的 `classify_io_error` 归类（`IoSide::Read`、`Write`、`AfterWrite`），不另写一张表；写到一半的失败，无论 errno 是什么，都是 `Unknown`。落到 overlay 上：

- checkpoint 的对外写入只有最后那个事务（`ToolCall`、`Manual` 的行），之前的失败什么都没写。
- `Before*` 是对当前、未改动状态的安全 checkpoint，不算对外写入；记下它之后仍可以具名拒绝，它留着，是合法的恢复点（Addendum 5 refinement 3）。
- put 出来、还没有行引用的 blob 是缓存填充，不算对外写入（Addendum 6 refinement 4），由 gc 在 1 小时之后收走。
- restore 的第一次对外写入是 `mkdir T` 与 `plan.json`（Addendum 7）。
- `project` 只写调用方的内部 staging，失败怎么报由调用方定。

### 6.1 扫描：`compute`、`reconcile_full`，以及 checkpoint、restore 里的扫描

| 失败 | Code | 现场 | 恢复 |
| --- | --- | --- | --- |
| 工作区目录不在 | `NoSuchPath` | 无 | session 的 status 报 `Missing` |
| index 读不了、工作区没接在本仓库的影子仓库上、是 sparse index、不存在 | gitio 透传：`NoSuchPath`、`InvalidInput`、`NotText`、`Unsupported`、`Unknown`（不存在是 `no-index`） | 无 | `InvalidInput`：工作区的 gitfile 没有指向本仓库的影子仓库（gitio.md §5.7 第 2 步），或者 `ws.git` 是别的仓库的（§2） |
| `is_ignored` 失败（规则文件读不了）；`walk` 产出 `Err` 项（目录不可读、`too_deep`、`root_changed`） | 原样返回：多半是 `Unknown`；`too_deep` 是 `TooLarge`，`root_changed` 是 `Busy`（Addendum 8） | tracker 不变，快照并回 inbox | 下一次扫描重来；`root_changed` 多半是工作区刚被交换，`Tracker::reset` 之后冷扫 |
| 目录项的名字不是 UTF-8 | `NotText`（`detail.path` 是上级目录，`detail.name_hex` 是原字节） | 无 | 改名；APFS 上建不出这种名字 |
| 名字超出 macOS 的长度限制 | `InvalidInput` | 无 | 改名 |
| 读文件或属性失败（权限、I/O） | 按 types.md §6.3，多半是 `Unknown`（`detail.path`、errno） | tracker 不变，快照并回 inbox | 下一次扫描重来；一直失败由 status 报出 |
| 写竞态两次都在变、gen count 不可得 | 不是失败：`Unstable` 告警 | 这次记下的是某一刻的内容 | 下一次扫描重扫 |
| 特殊文件、嵌套仓库、特殊权限位 | 不是失败：告警 | — | 人看 status |
| 监听启动失败、事件丢失 | 启动失败由 apfs 透传；丢失不是失败 | `needs_full` | daemon 退化为定时全扫（design §6.2） |

扫描只读工作区、index 与 ignore 规则，失败从不留下任何写入。

### 6.2 checkpoint

| 失败 | Code | 现场 | 恢复 |
| --- | --- | --- | --- |
| 会话、repo、tracked、基线或根目录不对 | `NoSuchSession`、`NoSuchRepo`、`InvalidInput`（`stale_base`、`wrong_root`） | 什么都没写 | 调用方换用正确的基线或目录 |
| `put_blob_rooted` 返回 `NoSuchPath`、`IsDirectory`、`NotFile`、`NotDirectory` | 不是失败：扫描之后路径没了、成了目录或特殊文件，或者某一级上级换成了符号链接或文件；按写竞态重判（§5.6 第 5 步：`NotDirectory` 时重判从根数起最浅的那个已不是目录的分量与它遮住的路径） | — | — |
| `put_blob_rooted` 的其它失败 | store 透传：`ReadOnly`、`CrossVolume`、`Unsupported`、`InvalidInput`、`Corrupt`、`Unknown` | 已 put 的 blob 没人引用，1 小时后由 gc 收走；扫描到的路径并回 inbox | 重试 |
| `record_checkpoint` 返回 `Busy` | `Busy` | 没有行；同上 | 重试，得到精确的 checkpoint（3d:189） |
| `record_checkpoint` 返回 `Unknown`，`outcome = "not_written"` | `Unknown` | 没有行 | 重试 |
| `record_checkpoint` 返回 `Unknown`，`outcome = "uncertain"` | `Unknown` | 行可能已经写下，store 的写者已中毒 | 不重试；daemon 重开 store 之后，`latest_checkpoint` 读到实情 |
| 其它 store 码（`ReadOnly`、`Corrupt`、`TooLarge`、`InvalidInput`） | 原样透传 | 没有行 | 人工处理 |

checkpoint 从不写工作区，所以它的任何失败都不改变工作区。

### 6.3 restore

| 阶段 | 失败 | Code | 现场 | 恢复 |
| --- | --- | --- | --- | --- |
| A | T 的位置上是符号链接或文件 | `Exists`（`restore_temp_taken`） | 无 | 人挪开它 |
| A | T 是证明不了的现场，或会话的 `lifecycle`、工作区的 `health` 为 `Unknown` | `Unknown`（`restore_scene`，或带 store 里记的现场） | 原有的现场原样，这一次什么都没做 | §5.7 的"现场的了结"：人把没被证明的文件挪走、删掉 T，或者 `rm --discard` |
| A | 目标 checkpoint 不存在或已被回收 | `NoSuchCheckpoint`（store 原样） | 无 | — |
| A | 目标坏了、跨基线却缺 `target_base`、路径谁都不认识 | `InvalidInput`、`NoSuchPath` | 无 | 改参数；`detail.on_disk` 提示只差大小写的拼写 |
| A | 预检拒绝 | `Exists`、`ReadOnly`、`OpenFiles`、`Corrupt`、`Busy` | 无，工作区一字未动，也没有记 `BeforeRestore` | 挪开挡路的东西再试；`Busy` 直接重试 |
| B | 记 `BeforeRestore` 失败；存 blob 时发现要动的文件又变了；第 10 步动手之前发现旧现场又变了，或者建 T 时它的位置上已有别的东西 | §6.2 的码；`Busy`（`workspace_changed`） | 没有新行，或者只多了一条 `BeforeRestore`（合法的恢复点，Addendum 5）；没被引用的 blob 由 gc 收走 | 重试 |
| C | 第一次对外写入（第 10 步的 `mkdir T` 与 `plan.json`）之后的任何失败：清掉旧现场时删除失败、在 T 里备内容失败（含 `set_file_meta` 拒绝改一个被换成别处文件硬链接的临时名：`ReadOnly`，`hard_link`）、证明不了被挪开的文件、目标位置已被占（`Exists`）、要删的目录不空（`NotEmpty`，写竞态）、上级或 T 换成了符号链接（`NotDirectory`）、I/O | `Unknown`，`detail` 带 `before`（`BeforeRestore` 的号）、`done`（已落位或已删除的路径）、`pending`（还没做的）、`temp`（T 的路径） | 工作区可能部分恢复；T 里是 `plan.json`、没落位的临时文件和被挪开的文件；`BeforeRestore` 记着 restore 之前的全部状态 | 不回滚、不重放、不清理（不变量 6）。T 里的文件都被证明时，下一次 restore 自动了结（§5.7）；否则人工了结 |
| D | T 里还有 `plan.json` 之外的东西 | `Unknown` | 工作区已经是目标状态 | 同上 |

restore 替换或删除的文件，总是先挪进 T，只有字节被证明在 `BeforeRestore` 里才删。所以任何时刻，工作区里原有的内容要么在 `BeforeRestore` 里，要么还在 T 里。

崩溃：第 10 步之前崩溃，最多多了一条 `BeforeRestore` 历史，工作区与 T 都没动，不需要现场；之后崩溃，`plan.json` 已经落盘，一直留到第 14 步，status 与下一次 restore 都看得到现场。T 非空时，session 的 `rm` 返回 `Dirty`，除非 `--discard`（Addendum 7）。

### 6.4 rebase、build_manifest 与 project

| 失败 | Code | 现场 | 恢复 |
| --- | --- | --- | --- |
| rebase 的参数互相不符，或 overlay 不合 V1–V5 | `InvalidInput` | 无 | 调用方的错误 |
| 基线目录里的文件不在，或与清单不符（读来合并时，或按 blob 不变量存进 store 时） | `Corrupt`（`detail.path`，期望与实际） | 无 | 基线 generation 被改过，违反不变量 5；人工处理 |
| `read_blob`、`put_bytes`、`put_blob` 失败 | store 透传 | 已 put 的 blob 没人引用，gc 收走 | 重试 |
| `build_manifest` 遍历失败 | `NoSuchPath`、`NotText`、`Unsupported`、`TooLarge`、`Busy`、`Unknown` | 无，它不写任何东西 | 由 session 重新导出这棵树 |
| `project` 失败 | `InvalidInput`、`NoSuchPath`、`NotDirectory`、`Corrupt`，store 与 apfs 的码 | dst 停在半途 | 由调用方按错误时机处理（session.md）：还没有对外写入时，session 用 `store::retire_dir` 退役 staging（会话根下的一级名字，store.md §5.5 的那一行），把意图关成 `MoveState::Refused`，原样返回；之后就是 `Unknown` |

## 7. 测试清单

测试在临时目录里跑。fixture 建一个小的 git 仓库（`core.ignorecase = true`、`core.precomposeunicode = true`，与 inter 的仓库相同），用 `Git::export_tree` 导出基线；在同一个 APFS 卷上开一个临时 Store，用 store 的接口登记一个空依赖集、基线 generation（连同 `build_manifest` 建出的清单）与会话；工作区是基线的克隆，按 session 的做法接上 git：`ensure_shadow` 建影子仓库，`attach_worktree` 写管理目录与 gitfile；`Git` 对 fixture 仓库的主 checkout 打开（`Git::open` 拒绝工作区），读工作区时传工作区的路径。`Tracker` 默认不启动监听，由测试自己把 `WatchEvent` 交给 inbox，才能精确地模拟丢事件；需要真实 FSEvents 的用例单独启动监听。带 `#[ignore]` 的测试要大文件或 `hdiutil` 建的磁盘映像（不需要 root），用 `cargo test -p strata-overlay -- --ignored` 跑。测试接缝用本模块的 `OverlayFault` 与 store.md §3 的 `Seam`（经 `Store::arm`），都在 `test-seams` feature 后面（Addendum 4）；`Busy` 由子进程造出来，照 store.md 的 `lock_child`：测试二进制以 `--exact <子测试名>` 重新执行自己，子进程开一条原始连接、对 `store.db` 持着 `BEGIN IMMEDIATE`（store 的 L2 是进程内的互斥锁，同一进程里的第二个写者只会等，不会 `Busy`）；这是测试的 fixture，overlay 的代码不跑 SQL（Addendum 4）。要杀掉一个停在半途的 restore 时，用同样的子进程做法。工作量用 `ScanStats` 的计数断言，不计时。

| 测试名 | 断言 | 来源（矩阵行 或 新增） |
| --- | --- | --- |
| `overlay_validate_rejects_malformed_shapes` | 手工构造的 overlay：路径重复或乱序（V1）、`Deleted` 带非零 mode 或普通条目带 mode 0（V2）、`Renamed` 的 from 等于自己的 path、等于另一条目的 path、或被两条共用（V3）、`a` 与 `a/b` 同为活路径（V4），`validate` 都返回 `InvalidInput`，`detail.rule` 依次是 "V1" 到 "V4"。合规的 overlay 上：`hides` 对 `Deleted` 的 path 与 `Renamed` 的 from 为真；`tombstones` 恰好是这两类路径；`paired` 对改名的任一半返回两半，对类型互换组里的任一路径返回整组，对无关的条目只返回它自己，对不是条目也不是 from 的路径返回空。路径为键的 overlay 表达不出"一个目录有两个父目录"或环 | 新增<br>layerfs-workspace/tests/captured_namespace_perturbed.rs::a_second_binding_of_a_stored_directory_is_refused_as_multiple_parents<br>layerfs-workspace/tests/captured_namespace_perturbed.rs::a_stored_directory_bound_only_below_itself_is_refused_as_a_cycle |
| `lww_glob_vectors` | LayerFS 的 glob 向量去掉前导 `/` 后原样通过：`a/**` 命中 `a/b` 与 `a/b/c`（直接子项也算）；`**/b` 不命中 `ab`、`xb`（目录名必须完整）；`a/**/**/b` 命中 `a/b`（连续的 `**/` 可以是零个目录）；`*`、`?` 不跨 `/`；不含 `/` 的模式命中任意深度的同名项；命中上级目录即命中其下的路径；最后一条匹配的规则决定结果，`!` 把匹配清掉；只有大小写不同的路径不命中 | layerfs-content/tests/glob_directories.rs::recursive_directory_glob_includes_direct_children<br>layerfs-content/tests/glob_directories.rs::recursive_directory_glob_requires_complete_directory_names<br>layerfs-content/tests/glob_directories.rs::consecutive_recursive_directory_globs_include_zero_directories |
| `lww_rules_load_from_strata_lww` | 规则从树里的 `.strata/lww` 读，树根的 `.lww`（LayerFS 的位置）不起作用；`%include` 读到嵌套的文件，第 17 个文件起的 `%include` 被忽略；文件不存在、是目录、不是 UTF-8 → 空规则；注释、空行、只有 `!` 的行没有效果；`load` 读出的规则与把同样的文本交给 `parse` 的匹配结果逐条相同，`**` 的语义与 `lww_glob_vectors` 一致 | layerfs-content/tests/glob_directories.rs::last_writer_rules_share_recursive_directory_semantics<br>layerfs-content/tests/glob_directories.rs::consecutive_recursive_directory_globs_include_zero_directories |
| `build_manifest_matches_exported_tree` | 对一棵导出的树（普通文件、可执行文件、符号链接、空文件、多级目录），清单的每一项是导出后字节的 BLAKE3、按所有者可执行位得到的 mode 与 size；树里克隆进来的 `node_modules/`、`src-tauri/target/`（配置的依赖目录与种子缓存）和 `.git` 不进清单；条目集合恰好等于树里其余的文件，这是"基线清单 = 导出目录"的自检；同一棵树建两次结果相同；树里有 FIFO → `Unsupported`；`build_manifest` 不写 Store | 新增（Addendum 3）<br>layerfs-workspace/tests/captured_namespace.rs::the_model_of_the_fixture_is_the_base_root |
| `untouched_workspace_has_empty_overlay` | 刚从基线克隆出的工作区：`compute` 与 `reconcile_full` 都得到空 overlay，blob 存储的文件数不变；连续两次 `compute` 结果相同，第二次 `files_hashed = 0` | layerfs-content/tests/edit_noop.rs::an_empty_stream_returns_the_base_root_untouched<br>layerfs-content/tests/filesystem_updates.rs::a_same_name_no_op_keeps_the_root_identity<br>layerfs-api/tests/sdk_compare.rs::equal_roots_require_no_namespace_scan |
| `restored_to_base_removes_override` | 一个基线文件先改成 `Modified`，再以五种方式回到基线的字节：原地写回；写到新文件再 rename 覆盖（换了 inode）；由另一个进程写回；`cp` 一份基线副本覆盖；先删再以同样的内容与 mode 重建。每种之后该路径都没有条目，blob 存储也不新增文件。改名走再改回、加可执行位再去掉、同字节覆盖写、多个文件同时写成基线内容，同样都没有条目；只翻转最后 1 个字节则是 `Modified`。另一个会话的 overlay 在整个过程中不变 | layerfs-workspace/tests/captured_file_edits.rs::same_byte_overwrite_retains_original_root_with_one_classification<br>layerfs-workspace/tests/captured_namespace_inline.rs::an_identical_rewrite_and_a_net_zero_directory_keep_the_base_root<br>layerfs-workspace/tests/captured_namespace_rebound.rs::a_base_directory_moved_away_and_back_keeps_its_stored_values_with_no_territory_walk<br>layerfs-workspace/tests/captured_namespace_states.rs::restated_bindings_and_removals_over_nothing_change_no_name<br>layerfs-workspace/tests/captured_piece_edits.rs::a_piece_equal_to_the_base_is_no_edit<br>layerfs-content/tests/edit_noop.rs::an_equal_replacement_preserves_the_base_root<br>layerfs-content/tests/edit_noop.rs::several_equal_replacements_are_all_recognised<br>layerfs-content/tests/merge_units.rs::a_relink_that_rewrote_nothing_is_not_a_change<br>layerfs-api/tests/sdk_written_back.rs::a_file_written_away_and_back_through_the_store_is_no_content_change<br>layerfs-api/tests/sdk_written_back.rs::a_file_saved_away_and_back_by_rename_over_is_no_content_change<br>tgrep-core/tests/shared_worktrees.rs::restored_changes_can_be_reverted_to_base_without_affecting_other_worktrees<br>layerfs-api/tests/merge_mixed_forms.rs::inline_changes_merge_against_a_root_form_tree<br>layerfs-api/tests/relink_commit.rs::clean_relink_commit |
| `deleted_base_file_is_tombstone` | 删除基线里的文件与符号链接 → 各一条 `Deleted`（mode 0，没有 blob），其余条目不受影响；删除整个基线目录 → 其下每个文件一条 `Deleted`，没有目录条目；墓碑在之后的每个 checkpoint 里都还在，直到 restore 或发布；同名文件以不同内容重建、再删除，仍是墓碑，基线文件不会"复活"；把 index 里该路径标成 skip-worktree 再删文件，仍是 `Deleted`；被删的基线文件仍被打开、经描述符继续写，overlay 里只有它的 `Deleted` | layerfs-overlay/tests/orphans.rs::unowned_unlinked_payload_is_reclaimed_live_but_its_namespace_tombstone_remains<br>layerfs-workspace/tests/captured_namespace.rs::deleted_files_directories_and_symlinks_leave_the_result<br>layerfs-workspace/tests/captured_namespace_holds.rs::a_base_file_held_written_unlinked_and_written_again_is_never_constructed<br>layerfs-workspace/tests/captured_namespace_holds.rs::files_and_a_symlink_held_only_by_a_lookup_are_absent_after_their_unlink<br>layerfs-workspace/tests/captured_namespace_states.rs::a_removed_base_directory_keeps_its_whiteouts_and_has_no_value<br>layerfs-workspace/tests/namespace.rs::unlink_and_rmdir_remove_one_reference_and_keep_directory_counts_exact<br>layerfs-workspace/tests/nonfile.rs::failed_name_composition_drops_only_whiteouts_over_proven_absent_base_names<br>layerfs-content/tests/filesystem_updates.rs::removing_the_last_binding_removes_the_inode_and_keeps_the_root_readable<br>tgrep-core/src/worktrees/tests.rs::sparse_missing_and_skip_worktree_paths_never_leak_base_terms |
| `new_then_deleted_path_leaves_no_entry` | 同一个 checkpoint 周期内新建又删除的文件、符号链接、目录树 → 没有任何条目，也没有墓碑，它们的内容从未被 put（blob 存储不变）；删除一个基线里没有、也从未存在的路径 → overlay 不变；新建、打开着写、unlink、再经描述符写 → 没有条目；与一处真实改动并存时只出现那一处；基线外的路径新建又删除，加上一处等内容的重写，overlay 为空 | layerfs-workspace/tests/captured_namespace_holds.rs::a_file_created_held_written_unlinked_and_written_again_is_never_constructed<br>layerfs-workspace/tests/captured_namespace_inline.rs::an_identical_rewrite_and_a_net_zero_directory_keep_the_base_root<br>layerfs-workspace/tests/captured_namespace_perturbed.rs::a_whiteout_over_a_name_the_base_does_not_bind_changes_nothing<br>layerfs-workspace/tests/captured_namespace_states.rs::inodes_created_and_removed_inside_the_capture_are_never_constructed<br>layerfs-workspace/tests/captured_namespace_states.rs::a_fresh_removed_directory_and_whatever_is_captured_under_it_never_reach_the_update<br>layerfs-workspace/tests/namespace_view.rs::name_and_inode_counts_have_no_cap_and_every_window_stays_bounded<br>layerfs-content/tests/filesystem_topology.rs::a_dropped_directory_emits_no_page_of_its_own<br>layerfs-content/tests/filesystem_topology_backed.rs::removing_a_name_the_base_does_not_bind_changes_nothing |
| `content_change_and_new_files_classified` | 基线文件内容改变 → `Modified`，blob 是新内容的 BLAKE3；新文件 `git add` 过 → `Added`，没有 add 也没被 ignore → `Untracked`；扩大文件（中间补零）、截断、写入非 UTF-8 与二进制字节、只改行尾（CRLF 换成 LF）→ blob 恒等于磁盘上的字节，新文件不与任何虚构的基线比较；`.png`、`.wasm` 等扩展名的文件同样记录；基线文件有没有被 `git add` 不影响它的分类 | layerfs-workspace/tests/captured_file_edits.rs::length_growth_and_true_new_file_do_not_use_a_fabricated_base<br>layerfs-workspace/tests/captured_namespace.rs::new_files_directories_and_symlinks_are_constructed<br>layerfs-workspace/tests/captured_namespace_states.rs::a_base_file_with_changed_content_gets_a_new_content_root<br>sdk/js/test/binary_write.mjs::forms<br>layerfs-content/tests/filesystem_updates.rs::a_tiny_tree_is_created_and_read_back_exactly<br>tgrep-core/src/walker.rs::walk_dir_retains_binary_extensions_for_file_listing<br>tgrep-core/src/worktrees/tests.rs::checkout_decoding_can_reuse_base_despite_different_raw_bytes |
| `exec_bit_change_keeps_blob` | 基线文件 `chmod +x` → `Modified`，blob 等于基线的 blob，mode 0o100755；改回 0644 → 没有条目；0644 改成 0600 → 没有条目（git 只记可执行位）；同一批里一处内容改动加三处无变化的操作（`touch`、改成 0600、等内容重写）→ 只有那一处有条目 | layerfs-workspace/tests/captured_namespace.rs::changed_content_mode_and_time_of_each_kind<br>layerfs-workspace/tests/captured_namespace_inline.rs::a_real_change_writes_the_inline_form_beside_three_no_ops<br>layerfs-workspace/tests/captured_namespace_states.rs::a_base_file_with_only_metadata_changes_keeps_its_content_root<br>layerfs-workspace/tests/namespace.rs::chmod_and_utimens_follow_the_portable_grammar_and_keep_one_mtime<br>layerfs-overlay/tests/digests.rs::a_later_mode_change_keeps_the_digest |
| `mtime_only_change_is_no_change` | `touch`、`touch -d` 设成过去与将来的时间、`utimes` 恢复原 mtime → `compute` 没有条目，`checkpoint` 返回 `recorded = false`；之后一次真实的内容改动被记录，checkpoint 里没有任何 mtime | layerfs-workspace/tests/captured_namespace_inline.rs::an_mtime_only_capture_keeps_the_base_root_and_row<br>layerfs-api/tests/merge_metadata_only.rs::an_mtime_only_change_is_no_change_at_commit<br>layerfs-daemon/tests/mounted_commit.rs::an_mtime_only_edit_stays_live_and_a_later_real_change_commits_its_time<br>layerfs-content/tests/filesystem_attributes.rs::the_metadata_forms_compare_by_value |
| `directory_mode_bits_are_never_recorded_or_warned` | 给工作区根目录和一个子目录加 g+s、sticky，或改成 0700 → overlay 为空，`warnings` 为空；对基线符号链接所在的目录 `chmod`，符号链接没有条目 | sdk/js/test/init_refusals.mjs::sgid<br>sdk/js/test/reinit.mjs::refused<br>layerfs-workspace/tests/captured_namespace_states.rs::a_base_directory_and_symlink_keep_their_content_roots_under_new_metadata<br>layerfs-workspace/tests/captured_namespace_states.rs::the_root_is_a_patched_metadata_value_and_never_fresh<br>layerfs-api/tests/sdk_compare.rs::directory_metadata_is_emitted_and_counted_by_both_diff_routes |
| `special_mode_bits_are_normalized_and_warned` | mode 为 4755、2755、1644 的文件（新建的，以及改了权限的基线文件）→ 条目的 mode 依次是 0o100755、0o100755、0o100644，`warnings` 各有一条 `SpecialModeBits { path, mode }`，mode 文本依次是 "4755"、"2755"、"1644"；扫描不中断，同批的其它改动照常记录；restore 写回的文件不带这些位 | sdk/js/test/init_refusals.mjs::setuid<br>sdk/js/test/init_refusals.mjs::sticky |
| `symlink_entry_is_exact_target_bytes` | 新建的符号链接 → mode 0o120000，blob 是目标的原字节：含 0xff 0xfe 的非 UTF-8 目标、1023 字节的目标（1024 字节在 APFS 上建不出来，`ENAMETOOLONG`，2026-10-22 实测）、指向目录或不存在的目标，都原样记录；链接从不被跟随，指向目录的链接下的文件不出现；基线链接改指向 → `Modified`，删掉 → `Deleted`；checkpoint 再 restore 之后 `readlink` 逐字节相同；没改的链接在之后的 checkpoint 里条目不变 | layerfs-overlay/tests/captured_namespace.rs::symlink_target_is_the_exact_local_bytes_of_a_live_symlink_or_missing<br>layerfs-workspace/tests/captured_namespace_states.rs::a_fresh_symlink_takes_exactly_its_captured_target<br>layerfs-workspace/tests/namespace.rs::create_mkdir_symlink_and_link_publish_parent_and_reference_effects_atomically<br>layerfs-content/tests/filesystem_read.rs::a_symlink_reads_back_its_exact_stored_target<br>layerfs-workspace/tests/nonfile.rs::linked_symlink_targets_follow_effective_layers_through_repeated_failed_captures<br>layerfs-content/tests/filesystem_limits.rs::a_symlink_target_is_accepted_at_4096_bytes_and_refused_at_4097 |
| `file_symlink_swap_is_modified` | 同一路径上普通文件换成符号链接 → 一条 `Modified`，mode 由 0o100644 变成 0o120000，没有残留的 `Deleted` 或第二条；再换回内容不同的普通文件 → `Modified`，换回基线内容与 mode → 没有条目；只改可执行位只影响 mode | layerfs-content/tests/filesystem_updates.rs::a_type_replacement_replaces_the_binding_and_the_record<br>tgrep-core/src/worktrees/tests.rs::symlinks_executable_modes_and_materialized_gitlinks_match_walker |
| `directory_file_transitions_are_paired` | 基线目录 `d/`（含文件与符号链接）换成同名文件 → `d/` 下每一项 `Deleted`，`d` 一条 `Untracked`，`paired("d")` 与 `paired("d/x")` 都返回整组；基线文件、基线符号链接各换成带子项的目录 → 它们各一条 `Deleted`，子项各 `Untracked` 或 `Added`，各自成组；目录与符号链接互换同理；任何时刻都没有一个路径既是文件又是目录前缀（`validate` 通过） | layerfs-workspace/tests/captured_namespace_rebound.rs::a_removed_stored_directory_tree_is_replaced_by_a_file<br>layerfs-workspace/tests/captured_namespace_rebound.rs::a_removed_file_and_symlink_are_replaced_by_directories_with_children<br>layerfs-workspace/tests/captured_namespace_rebound.rs::a_directory_is_replaced_by_a_symlink_and_a_symlink_by_a_directory |
| `recreated_directory_compares_by_path` | 删掉一个基线目录，再建同名目录：内容与基线相同的文件没有条目，内容不同的 `Modified`，没有重建的 `Deleted`，基线里没有的 `Untracked` 或 `Added`；结果与新目录是不是原来的 inode 无关 | layerfs-workspace/tests/captured_namespace_holds.rs::a_base_directory_removed_while_held_is_absent_and_keeps_its_whiteout<br>layerfs-workspace/tests/captured_namespace_rebound.rs::a_directory_removed_and_made_again_under_its_name_is_a_new_inode |
| `rename_records_renamed_entry` | `mv a.ts b.ts` → `b.ts` 上一条 `Renamed { from: "a.ts", blob }`，blob 等于基线里 `a.ts` 的 blob，没有 `a.ts` 的 `Deleted`，`hides("a.ts")` 为真；第一次 checkpoint put 这个 blob 一次，之后的 checkpoint 不再 put；两个内容相同的基线文件都被改名时，按路径顺序确定地配对，重复计算结果相同；改名之后又改了内容 → `Deleted` 加 `Untracked` | layerfs-workspace/tests/captured_namespace.rs::renamed_and_moved_names_of_each_kind_keep_their_inodes<br>layerfs-workspace/tests/namespace.rs::rename_moves_replaces_and_refuses_cycles_without_descendant_rows<br>layerfs-content/tests/filesystem_hardlinks.rs::a_move_across_directories_keeps_the_count_and_the_inode<br>layerfs-content/tests/filesystem_topology.rs::a_base_resident_directory_can_be_renamed_by_unbinding_first<br>layerfs-content/tests/filesystem_topology_backed.rs::a_move_that_unbinds_first_is_accepted_and_a_restated_binding_is_no_placement |
| `directory_moves_expand_per_file` | 基线目录改名、移到深处、移进一个新建目录再嵌进另一个基线目录、一个周期里移动两次、改名覆盖一个空目录、先移出子目录再删掉父目录：结果都是每个文件在最终路径上一条 `Renamed`（from 是基线路径），中间路径与临时目录不留痕迹，父目录里剩下的文件 `Deleted`；新建的目录树整体改名，只在最终路径上出现 `Untracked`；用例用中性的目录名，不用 `.git`、`node_modules`（它们被排除） | layerfs-workspace/tests/captured_namespace.rs::a_deep_fresh_chain_ends_in_aliases_and_a_moved_base_directory<br>layerfs-workspace/tests/captured_namespace_rebound.rs::a_directory_renamed_over_an_empty_directory_replaces_it<br>layerfs-workspace/tests/captured_namespace_rebound.rs::a_base_directory_moved_twice_in_one_capture_is_bound_once<br>layerfs-workspace/tests/captured_namespace_rebound.rs::a_directory_rescued_from_a_directory_removed_in_the_same_capture_survives<br>layerfs-workspace/tests/captured_namespace_states.rs::a_moved_base_directory_has_no_row_and_no_value<br>layerfs-workspace/tests/captured_namespace_states.rs::a_moved_fresh_directory_keeps_its_serial_and_children<br>layerfs-workspace/tests/captured_namespace_states.rs::a_base_directory_moves_under_a_fresh_directory_inside_a_base_directory<br>layerfs-content/tests/filesystem_bounds.rs::a_directory_larger_than_the_former_walk_ceiling_can_be_rebound<br>layerfs-content/tests/filesystem_topology_backed.rs::one_moved_directory_under_another_ones_stored_child_is_accepted |
| `exchanged_and_inverted_directories` | 两个基线目录互换名字：内容出现在对方空出来的路径上的文件得到 `Renamed`，同名而内容不同的得到 `Modified`，结果与经哪个临时名交换无关；父子目录颠倒（`p/c` 变成 `c/p`）→ 每个文件一条 `Renamed`，再移回去 → overlay 为空 | layerfs-workspace/tests/captured_namespace_states.rs::two_base_directories_exchange_their_names<br>layerfs-workspace/tests/captured_namespace_states.rs::a_stored_parent_and_child_directory_are_inverted<br>layerfs-content/tests/filesystem_topology_backed.rs::inverting_a_parent_and_its_child_is_a_legal_move_without_a_base_walk |
| `rename_onto_freed_name_and_replacing_renames` | 删掉 `a`，再把没改过的 `b` 改名为 `a` → `a` 上一条 `Renamed { from: "b" }`；`b` 先改内容再改名为 `a` → `a` 为 `Modified`，`b` 为 `Deleted`；新文件 rename 覆盖基线文件 `victim` → `victim` 为 `Modified`，新文件的旧名没有条目；新符号链接 rename 覆盖基线符号链接 → `Modified`，目标字节是新链接的；把这些操作换个顺序做，结果相同 | layerfs-workspace/tests/captured_namespace.rs::replacing_renames_and_fresh_moves_compose_in_one_capture<br>layerfs-workspace/tests/captured_namespace_holds.rs::a_stored_file_replaced_by_rename_while_open_leaves_only_the_new_inode<br>layerfs-workspace/tests/captured_namespace_holds.rs::fresh_and_looked_up_inodes_replaced_by_rename_leave_only_the_new_inodes<br>layerfs-workspace/tests/captured_namespace_states.rs::a_removed_name_and_a_rebound_name_are_exact_changes |
| `rename_case_only_recorded_as_rename` | fixture 仓库 `core.ignorecase = true`（APFS 默认大小写不敏感）。`mv src/Foo.ts src/foo.ts` → overlay 恰好是 `src/foo.ts` 上的一条 `Renamed { from: "src/Foo.ts", blob }`，blob 等于基线的 blob，没有 `Deleted` 或 `Untracked`；改名同时改了内容 → 仍是这一条 `Renamed`，blob 是新内容；目录 `Src/` 改成 `src/` → 每个文件一条 `Renamed`；`École.ts` 改成 `école.ts`（非 ASCII 的大小写）→ 同样一条 `Renamed`；`compute` 与 `reconcile_full` 结果相同（只由事件驱动的增量那一半在 Task 6 的 `event_spellings_and_subtrees_reverify`）；把 `core.ignorecase` 设为 false（卷仍不区分大小写），改名加改内容 → 仍是这一条 `Renamed`（按卷折叠配对），blob 是新内容，没有 `Deleted` 或 `Untracked`；大小写敏感的磁盘映像上设 `core.ignorecase = true`，`mv src/Foo.ts src/foo.ts` 加改内容 → 同样一条 `Renamed`（按 git 折叠配对，裁定 T7；`#[ignore]`，要 `hdiutil`） | 新增（Review Focus 4；裁定 T7；module-rulings「Case-only pairing pairs under either fold」） |
| `non_ascii_case_renamed_ignored_tracked_dir_is_still_compared` | `core.ignorecase = true`，基线有 `Écoles/x.ts` 与 `Écoles/y.ts`；`.gitignore` 加一行 `écoles/`，再把目录改名为 `écoles` → overlay 是每个文件一条 `Renamed`（from 是 `Écoles/…`），没有 `Deleted`，目录没有因为 ignore 规则被剪掉；`compute` 与 `reconcile_full` 结果相同（只由事件驱动的增量那一半在 Task 6 的 `event_spellings_and_subtrees_reverify`）。gitio 的 `tracked_under("écoles")` 在这里答 false（只折叠 ASCII），这个测试证明 overlay 不靠它；fixture 改成 `core.ignorecase = false`（卷仍不区分大小写），基线有 `Src/a.ts` 与 `Src/b.ts`，`.gitignore` 加一行 `src/`（只命中新拼写），再把目录改名为 `src` → `src` 照样按受限目录进入，overlay 是每个文件一条 `Renamed`（from 是 `Src/…`，blob 等于基线的；§5.3 第 1 步按卷折叠配成），没有 `Deleted`（受限目录若按 git 折叠，`src` 会被剪掉，`Src/…` 全成 `Deleted`）；`compute` 与 `reconcile_full` 结果相同 | 新增（本模块评审 I5；Addendum 7 的 `case_fold_key`；module-rulings「Mapping disk entries to tracked paths is disk identity」） |
| `hard_links_are_independent_paths` | 给基线文件建一个硬链接 → 新名字是 `Untracked`，blob 与基线相同，原名字没有条目；经一个名字写入 → 两个路径都按新内容记录（原名字 `Modified`）；删掉其中一个名字 → 只有它变化；跨目录移动一个链接名，按改名处理 | 新增<br>layerfs-content/tests/filesystem_hardlinks.rs::two_aliases_reach_one_derived_count<br>layerfs-content/tests/filesystem_topology.rs::a_directory_or_symlink_cannot_have_two_parents<br>layerfs-content/tests/filesystem_topology.rs::a_base_resident_symlink_cannot_gain_a_second_parent<br>layerfs-content/tests/filesystem_topology_backed.rs::a_symlink_bound_twice_is_refused_by_its_derived_count |
| `ignored_files_skipped_tracked_never_ignored` | 被 ignore 的新文件不进 overlay；被 ignore 的目录 `build/` 里有一个已跟踪文件，改它 → `Modified`，同一目录里的其它新文件不出现；`.gitignore` 新加一条规则盖住一个已跟踪的文件，它仍按内容比较；`.ignore` 文件里的规则不起作用 | tgrep-core/src/walker.rs::a_tracked_file_keeps_its_ignored_directory_walkable<br>tgrep-core/src/gitignore.rs::builds_matcher_from_dot_ignore_without_a_git_repo |
| `point_query_and_walk_agree` | 一组路径覆盖嵌套的 `.gitignore`、`!` 反向规则、`info/exclude`、全局排除文件、被 ignore 目录里的已跟踪文件、目录规则（`dir/`）与大小写变体；对每一个路径，把它作为唯一脏路径的增量 `compute` 给出的判定，与全扫的判定逐项相同 | tgrep-core/src/walker.rs::the_point_query_matcher_hides_exactly_what_the_walk_hides<br>tgrep-core/src/walker.rs::both_walks_agree_on_what_the_tree_contains |
| `excluded_dirs_never_enter_overlay` | 在 `node_modules/`、`vendor/`、`src-tauri/target/`、`.next/cache/` 里，以及对未被跟踪的种子文件 `.env`，新建、修改、删除 → 没有条目，也不进脏集合；基线里有被跟踪文件的 `vendor/` 被换成依赖集的克隆（内容不同、少了文件）→ 没有 `Deleted`；改 `.gitignore` 让 `.env` 不再被 ignore → 它仍然不进 overlay；以 `NODE_MODULES/x` 的拼写送达的事件不进脏集合，`core.ignorecase = false`（卷仍不区分大小写）时以 `.ENV` 的拼写送达的事件也不进（inbox 按卷折叠）；仓库配置的 `deps_dirs` 加了一个目录、重新加载之后，下一次扫描 `stats.full = true`，那个目录下的文件离开 overlay；反过来把它从 `deps_dirs` 去掉、重新加载 → 下一次扫描全扫，它下面的文件进 overlay；之后在里面改一个文件、把它的事件交给 inbox → 下一次增量扫描（`stats.full = false`）含这处改动，事件按 inbox 当前的 X 过滤，不再被丢（`#[ignore]` 的一段用真实监听做同样的事，要等事件送达，apfs.md §4.2 F） | 新增（config.md：依赖目录不进 overlay、不被监听；种子缓存与种子文件不 checkpoint；本模块评审 N1） |
| `excluded_paths_follow_disk_identity` | 都用新 `Tracker` 上的全扫。种子文件 `.env` 未被跟踪，`.gitignore` 不再 ignore 它：把它删掉、改建成 `.ENV` → 不进 overlay；fixture 改成 `core.ignorecase = false`（卷仍不区分大小写）再做一遍 → 同样不进 overlay；基线与 index 跟踪的是 `.ENV`、种子配置写的是 `.env`（`core.ignorecase = false`）→ `.env` 不进 X，改那个文件 → `Modified`；大小写敏感的磁盘映像上 `.ENV` 与 `.env` 是两个文件 → `.ENV` 按普通文件分类（没被 ignore 就是 `Untracked`），`.env` 仍不进 overlay（`#[ignore]`，要 `hdiutil`） | 新增（module-rulings「Disk identity follows the volume, git identity follows core.ignorecase」；本模块评审 r2 对决定 (1) 的意见） |
| `size_threshold_never_affects_membership` | 64 MiB 加 1 字节的新文件 → `Untracked`；同样大小的基线文件被改 → `Modified`；没改的大文件没有条目，绝不因为大而被当作墓碑。`#[ignore]`：要约 130 MB 磁盘 | tgrep-core/src/walker.rs::metadata_walk_honours_a_raised_max_file_size |
| `sparse_file_is_hashed_whole` | 一个 256 MiB 的稀疏文件（开头与末尾各 1 字节，中间是洞）→ blob 等于整份逻辑内容（洞读成零）的 BLAKE3，`ScanStats.bytes_hashed` 等于逻辑大小；restore 之后逐字节相同 | 新增（裁定 T7）<br>layerfs-workspace/tests/captured_file_edits.rs::new_hole_above_four_gib_uses_bounded_original_run_work |
| `special_files_are_never_opened` | 工作区里建一个 FIFO（新路径）→ 扫描在 1 s 内返回，FIFO 没有条目，`warnings` 有 `SpecialFile`；把一个基线文件换成同名 FIFO → 该路径 `Deleted`，并告警；socket 同样 | 新增 |
| `embedded_repo_is_not_entered` | 在 `lib/sub/` 里 `git init` 并加新文件 → 新文件不进 overlay，`warnings` 有 `EmbeddedRepo { path: "lib/sub" }`；`lib/sub/` 下原有的基线文件被改 → 照常 `Modified`，没改的不产生墓碑 | 新增 |
| `xattrs_are_never_recorded_or_restored` | 只给基线文件加扩展属性与 BSD 标志 → 没有条目；一个带扩展属性的已修改文件 checkpoint 再 restore → 内容与 mode 恢复，扩展属性不恢复；基线目录里的文件带扩展属性时，restore 克隆回去的那份没有扩展属性 | 新增（裁定 T7 的局限）<br>layerfs-content/tests/filesystem_attributes.rs::reference_attribute_trees_are_reproduced_exactly<br>layerfs-content/tests/filesystem_attributes.rs::exact_sizing_partitions_match_the_encoder_and_the_tail_rebalances<br>layerfs-persistence/tests/acquisition_batching.rs::a_metadata_value_outside_the_portable_grammar_is_refused_and_never_read |
| `empty_directories_are_not_carried` | 新建空目录 → 没有条目；restore 一个"目录里的文件都被删掉"的 checkpoint 之后，不重建那个空目录；写回文件时缺的上级目录会建出来 | 新增（裁定 T7 的局限）<br>layerfs-workspace/tests/captured_namespace.rs::new_files_directories_and_symlinks_are_constructed |
| `names_backslash_nfd_and_non_utf8` | 同一目录下 `a\b.txt`（字面的反斜杠）与 `a/b.txt` 是两条不同的条目；以 NFD 拼写（e 加组合重音）新建的文件，`RepoPath` 是 NFC 拼写，与基线里 NFC 的同名文件是同一个路径，内容相同就没有条目；目录项名字的转换函数对含 0xff 的字节返回 `NotText`，不做替换。APFS 拒绝建这种名字，所以这一项直接测转换函数 | tgrep-core/src/walker.rs::index_keys_never_rename_a_native_name<br>tgrep-core/src/worktrees/tests.rs::literal_backslash_checkout_paths_cannot_alias_directory_paths |
| `workspace_git_reads_through_the_main_checkout` | fixture 用主 checkout 上打开的 `Git`，经 `ensure_shadow` 与 `attach_worktree` 建出工作区。`Git::open(工作区)` → `InvalidInput`（`strata-workspace`）；主 checkout 的 `Git` 调 `read_index(ws.root)` 读到的就是工作区的 index（与 `git -C <工作区> ls-files` 一致），`compute` 正常；把另一个仓库的 `Git` 放进 `WorkspaceRef` → `compute` 返回 gitio 的 `InvalidInput`，原样透传、不包装，`Tracker` 不变 | 新增（本模块评审 I1；Addendum 5） |
| `reconcile_after_kernel_dropped_equals_cold_scan` | `Tracker` 不启动监听，测试自己把事件交给 inbox。先 `compute` 一次让缓存热起来，然后在"事件丢失"的窗口里做下面这些改动、不送它们的事件：同大小改写并用 `utimes` 恢复 mtime、rename 覆盖、只改大小写的改名、删除后以同样内容重建、文件与目录互换、改 `.gitignore`、把一个改过的文件改回基线、`git add` 一个新文件；最后送一个 `WatchEvent::Dropped`。之后 `compute(…, Some(&[]))` 的 `stats.full = true`，它的 overlay 与一个全新 `Tracker` 的 `reconcile_full` 逐条相等，也等于把每个文件读一遍算出的期望。第二轮把 `Dropped` 换成对根目录的 `MustRescan`，结果相同。第三轮只在 `src/sub/` 下改动，送 `MustRescan(<root>/src/sub)` → 不是全扫（`stats.full = false`），`dirs_listed` 只数 `src/sub` 之下的目录，结果仍等于 `reconcile_full`。对照组：事件照常送达、不丢时，增量结果同样相等 | 新增（Review Focus 2；矩阵映射到它的行如下）<br>tgrep-core/src/walker.rs::the_point_query_matcher_hides_exactly_what_the_walk_hides<br>tgrep-core/src/walker.rs::both_walks_agree_on_what_the_tree_contains<br>tgrep-core/src/worktrees/tests.rs::hints_are_bounded_and_full_refresh_repairs_unreported_same_stat_edits<br>tgrep-core/src/worktrees/tests.rs::serving_verification_keeps_unchanged_views_ready_and_repairs_missed_changes<br>layerfs-overlay/tests/kept_live.rs::every_writer_of_a_kept_serial_counts_it_once |
| `every_change_kind_reaches_the_next_checkpoint` | 对每一种改动，即改内容、截断、扩展、改可执行位、新建、删除、改名（新旧两个路径都报事件），各自之后的下一次 checkpoint 都 `recorded = true` 并含对应的条目，从不答"无变化"；每一次的增量结果都等于 `reconcile_full`。`touch` 不在其列：它不算改动 | layerfs-overlay/tests/kept_live.rs::every_writer_of_a_kept_serial_counts_it_once |
| `random_edits_incremental_equals_cold_and_restores_exact` | proptest 生成随机的操作序列：重叠写、截断后再增长、新建、删除、改名、改可执行位、符号链接改指向、整目录移动。每一步之后增量 `compute` 等于 `reconcile_full`，并做一次 checkpoint；序列结束后依次 restore 每个旧 checkpoint，工作区逐字节等于当时读出的快照（增长部分读成零，截掉的字节不复活）。缺省跑 64 个用例，`#[ignore]` 的版本跑 2048 个 | layerfs-api/tests/history_diff_oracle.rs::change_walk_equals_the_full_walk<br>layerfs-overlay/tests/payload.rs::layered_reads_match_a_reference_model_across_writes_resizes_capture_and_install<br>layerfs-workspace/tests/captured_file_edits.rs::overlapping_writes_shrink_regrow_and_cross_cell_spans_match_final_bytes<br>layerfs-workspace/tests/captured_file_edits.rs::many_edits_use_numbered_records_after_install_and_reads_survive_close<br>layerfs-workspace/tests/captured_namespace_fragments.rs::a_truncate_between_pieces_cuts_what_was_written_before_it<br>layerfs-workspace/tests/captured_namespace_fragments.rs::shrinking_and_regrowing_across_cells_never_returns_cut_bytes<br>layerfs-workspace/tests/captured_namespace_states.rs::a_fresh_file_is_constructed_from_its_captured_runs<br>layerfs-workspace/tests/captured_new_file_route.rs::a_new_file_is_identical_on_the_direct_and_the_record_route<br>layerfs-workspace/tests/captured_piece_edits.rs::insert_delete_and_replace_are_one_edit_each<br>layerfs-workspace/tests/payload.rs::truncate_and_regrow_never_resurrect_bytes_across_capture_and_known_install<br>layerfs-workspace/tests/payload.rs::whole_windows_overwrites_and_cuts_inside_them_read_back_exactly |
| `dirty_set_dedups_by_canonical_path` | 同一路径改 100 次，inbox 里只有一项；`DIR/x` 与 `dir/x`、NFD 与 NFC 的同一个名字、`./dir/x`，归并为同一项；不同路径各一项；overlay 里每个路径至多一条；事件路径转不成 `RepoPath`（不是 UTF-8）→ 要求全扫，不静默丢弃 | layerfs-overlay/tests/kept_live.rs::a_kept_serial_is_counted_once_and_others_as_before<br>tgrep-core/src/worktrees/tests.rs::accepted_hint_spellings_have_one_canonical_queue_entry |
| `event_spellings_and_subtrees_reverify` | 磁盘上是 `dir/x`，事件只以 `DIR/x` 的拼写送达，期间做了一次同大小、恢复 mtime 的改写 → 增量 `compute` 找到这处改动；NFD 拼写的事件同理；只送一个目录事件（`Changed(["dir"])` 或 `MustRescan("dir")`）→ 整棵子树被重判，其中的同大小改写被找到；`core.ignorecase = true` 下 `mv src/Foo.ts src/foo.ts`、把 `École.ts` 改成 `école.ts`，以及 `.gitignore` 里有 `écoles/` 时把 `Écoles/` 改名为 `écoles/`，都只送旧拼写的事件 → 增量 `compute` 与 `reconcile_full` 结果相同（这几种改名怎样分类，由 Task 5 的 `rename_case_only_recorded_as_rename` 与 `non_ascii_case_renamed_ignored_tracked_dir_is_still_compared` 测）；大小写敏感的磁盘映像上设 `core.ignorecase = true`，`dir/x` 与 `DIR/x` 是两个文件：改 `DIR/x`、只送它的事件 → 拼写解析按卷（按字节）只找到 `DIR/x`，增量扫描 `stats.full = false`，结果与 `reconcile_full` 相同（`#[ignore]`，要 `hdiutil`） | tgrep-core/src/worktrees/tests.rs::subtree_hints_and_case_aliases_reverify_unchanged_metadata<br>tgrep-core/src/worktrees/tests.rs::subtree_hint_spellings_reverify_restored_mtime_edits |
| `dropped_or_overflowing_events_force_full_scan` | `Dropped` 之后 `needs_full()` 为真，下一次扫描 `stats.full = true`；把上限设为 8、送 9 个不同的路径 → 同样全扫；全扫找到窗口里没有报事件的同 stat 改写（大小与 mtime 都恢复）；全扫之后 `needs_full()` 为假 | tgrep-core/src/worktrees/tests.rs::hints_are_bounded_and_full_refresh_repairs_unreported_same_stat_edits |
| `periodic_full_scan_repairs_missed_changes` | 不送任何事件，做同 stat 改写、新增、删除、改全局排除文件（`core.excludesFile` 指向的文件）、把文件改回基线：`compute(…, None)` 把每一处都修正（新增出现，删除成为墓碑，改回的条目消失，被新规则 ignore 的 `Untracked` 消失），之后的 checkpoint 记下它们；什么都没变时，`compute(…, None)` 之后的 checkpoint 答 `recorded = false`，缓存作保的文件 `files_hashed = 0` | tgrep-core/src/worktrees/tests.rs::serving_verification_keeps_unchanged_views_ready_and_repairs_missed_changes |
| `index_changes_reclassify_without_events` | `git add` 一个 `Untracked` 文件，不送任何事件 → 下一次增量 `compute` 把它变成 `Added`；`git add -f` 一个被 ignore 的文件 → 它以 `Added` 出现；`git rm --cached` 一个 `Added` 文件 → 变回 `Untracked`；把 `core.ignorecase` 改成 false → 下一次扫描 `stats.full = true`；种子文件 `.env` 被 ignore、不在 overlay 里，`git add -f .env` → 下一次扫描 `stats.full = true`，`.env` 以 `Added` 出现；再 `git rm --cached .env` → 下一次扫描 `stats.full = true`，`.env` 离开 overlay，原始差异里也没有它，之后的增量扫描仍然没有 | 新增 |
| `quiet_workspace_stays_clean` | checkpoint 之后没有新的编辑：inbox 为空，`compute` 不读任何文件，overlay 不变，下一次 checkpoint `recorded = false`；只被读过的文件不产生事件，也不改变任何东西 | layerfs-overlay/tests/kept_live.rs::a_kept_row_is_not_counted_dirty |
| `failed_checkpoint_keeps_dirty_set` | 改 3 个文件，用 store 的 `Seam::AfterCheckpointRow` 让记行失败 → 返回 `Unknown`，没有新行，inbox 里又有这 3 个路径；重试的 checkpoint 含全部 3 处，一处不少；扫描中途读文件失败（把一个文件改成 000 权限）时同样如此 | layerfs-overlay/tests/kept_live.rs::a_failed_job_restores_the_kept_set |
| `writes_during_scan_reach_the_next_scan` | 用 `OverlayFault::PauseAfterSnapshot` 停住一次 checkpoint，期间写 50 个文件（其中一个目录下 50 个）并送出它们的事件：这次 checkpoint 不含它们或只含一部分，下一次全部含有，最后 overlay 与 `reconcile_full` 相等；一次判为"无变化"的 checkpoint 与并发写入赛跑，写入总是出现在这一次或下一次里，从不丢；定时全扫进行时到达的事件不丢，扫描失败时快照回到 inbox；`compute` 读一个文件的两次之间每次都被写（`OverlayFault::PauseAfterHash` 每次放开之前写一次）→ 这次的 `warnings` 有 `Unstable`，下一次扫描重读它，`Unstable` 消失 | layerfs-daemon/tests/mounted_install.rs::writes_during_a_commit_stay_live_and_reach_the_next_commit<br>layerfs-daemon/tests/mounted_install.rs::writes_during_a_commit_stay_live_and_reach_the_next_commit_fifty_to_a_directory<br>layerfs-daemon/tests/store_commit.rs::a_mutation_racing_an_unchanged_commit_is_never_lost<br>tgrep-core/src/worktrees/tests.rs::epoch_change_after_preparation_prevents_stale_ready_publication<br>tgrep-core/src/worktrees/tests.rs::serving_verification_yields_to_invalidations_and_fails_closed<br>layerfs-workspace/tests/captured_namespace_order.rs::mutations_published_between_producer_passes_do_not_reach_the_captured_root |
| `work_grows_with_the_change_not_the_base` | 有 tracker 的工作区里分别改 40、160、640、1600 个文件，每次 checkpoint 的 `files_hashed` 等于改动数，`entries_seen` 与 `dirs_listed` 至多按改动数线性增长；同样的 40 处改动放在 1000 个文件和 100 000 个文件的基线上，增量计数相同；全扫的 `dirs_listed` 等于源码树的目录数，被剪掉的 ignore 目录不算；只有判为改了的文件被读 | layerfs-workspace/tests/captured_namespace_cursor.rs::work_grows_linearly_with_the_change_and_pages_stay_bounded<br>layerfs-workspace/tests/captured_namespace_cursor.rs::work_follows_the_change_and_not_the_installed_base<br>layerfs-daemon/tests/file_digests.rs::a_commit_records_digests_of_followed_fresh_files_only<br>layerfs-content/tests/edit_noop.rs::a_long_equal_prefix_then_a_mismatch_stops_the_comparison_early<br>layerfs-api/tests/sdk_diff_entries.rs::the_same_changes_cost_the_same_in_a_small_and_a_large_tree |
| `write_race_retries_then_marks_unstable` | 对 `read_candidate`：`OverlayFault::PauseAfterHash` 停在读完内容之后，期间写一次 → `retried = true`、`unstable = false`，blob 是写入之后的内容；每次重读之前都写 → `retried = true`、`unstable = true`，blob 是最后一次读到的；文件有可写的 mmap（gen count 不可得）→ 只读一次，`retried = false`、`unstable = true`；没有写入 → 两个都是 false | tgrep-core/src/worktrees/tests.rs::managed_verified_read_rejects_restored_mtime_mutation_and_replacement<br>layerfs-workspace/tests/captured_namespace_order.rs::mutations_published_between_producer_passes_do_not_reach_the_captured_root |
| `mmap_writes_count_once_written_back` | 有可写 mmap 期间 gen count 不可得：扫描照读内容并标 `Unstable`，不写缓存；`msync` 之后的下一次 checkpoint 必含映射里写的字节；`munmap` 之后缓存恢复作保；checkpoint 从不调用 `msync` | layerfs-daemon/tests/mounted_commit.rs::r5_9_stores_through_shared_mappings_are_committed_once_written_back<br>layerfs-daemon/tests/r5_9_dirty_mapping.rs::r5_9_a_still_dirty_mapped_page_is_outside_the_commit_and_in_the_next_after_msync |
| `checkpoint_is_immutable_and_restorable` | checkpoint 之后继续编辑、再 checkpoint、对它的 overlay 做 rebase：读回的旧 checkpoint 的 base 与 entries 一字不变，restore 它得到当时的树；同一会话连续 5 个增量 checkpoint（包括在一个写入者每两步之间各做一次）都能完整 restore 出各自那一刻；号严格递增，后一个紧接前一个；已被 gc 回收的 checkpoint 读取得 `NoSuchCheckpoint` | layerfs-overlay/tests/captured_namespace.rs::later_active_rows_install_and_close_leave_reader_answers_until_exact_release<br>layerfs-workspace/tests/captured_file_edits.rs::prepare_after_actual_binding_install_rebinds_only_the_retained_original_root<br>layerfs-daemon/tests/mounted_commit.rs::r5_3_commit_captures_what_every_launch_published_whatever_became_of_the_process<br>layerfs-daemon/tests/mounted_commit.rs::r5_8_five_incremental_commits_on_one_mount_each_read_back_completely<br>layerfs-daemon/tests/r5_5_install_survival.rs::r5_5_a_commit_at_each_parked_step_of_a_writer_is_exact_on_the_same_and_a_fresh_mount |
| `unchanged_checkpoint_is_up_to_date` | 没有变化时，`ToolCall` 与 `Manual` 的 checkpoint 返回 `recorded = false` 和最新的那个 checkpoint，不读文件（`files_hashed = 0`）、不 put（blob 存储不变）、不记行；改一个文件 → 恰好记一行；紧接着再做 → 又是 `recorded = false`；四种 `Before*` 原因即使没有变化也记一行；同一个 `ToolCall` id 第二次 → 返回已有的号，`recorded = false` | layerfs-daemon/tests/mount_control.rs::product_control_commit_captures_the_frontier_releases_its_owners_and_is_then_up_to_date<br>layerfs-daemon/tests/mounted_commit.rs::r5_2_unchanged_is_up_to_date_and_a_change_is_committed_once<br>layerfs-daemon/tests/store_commit.rs::an_unchanged_workspace_answers_uptodate_without_capture_or_save<br>tgrep-core/tests/shared_worktrees.rs::checkpoint_contains_only_overlay_and_never_changes_base_files |
| `checkpoint_never_touches_base_or_leaves_residue` | checkpoint 前后，基线目录里每个文件的字节、mtime、inode 不变；blob 存储里新增的只有改动路径的内容；同一状态存两次，blob 存储不再新增；扫描中途失败（一个文件读不了）或记行失败时，没有半个 checkpoint，上一个 checkpoint 读回不变，工作区里没有多出任何文件，store 的 `tmp/` 里没有本次留下的临时文件 | tgrep-core/tests/shared_worktrees.rs::checkpoint_contains_only_overlay_and_never_changes_base_files<br>tgrep-core/tests/shared_worktrees.rs::invalid_live_paths_fail_before_replacing_a_checkpoint<br>tgrep-core/tests/shared_worktrees.rs::checkpoint_cannot_overwrite_base_or_leave_temporary_files_after_failure |
| `checkpoint_store_failures_are_settled` | 一个子进程对 `store.db` 持着 `BEGIN IMMEDIATE`（照 store.md 的 `lock_child`）时 checkpoint → `Busy`，没有行，工作区与它的 overlay 不变，子进程放手后重试得到精确的 checkpoint；先 put 了一个 100 MB 的文件才遇到 `Busy`，同样如此；`Seam::BlobRenameFails { errno: EIO }` 让 put 失败 → 错误原样返回，没有行；扫描中一个文件读失败 → 没有行；每种情况之后，下一次成功的 checkpoint 都与磁盘一致，没被引用的 blob 留给 gc | layerfs-daemon/tests/mount_control.rs::control_busy_keeps_the_channel_healthy_for_a_later_explicit_call<br>layerfs-daemon/tests/mounted_commit_failures.rs::f1_store_busy_before_save_is_settled_and_a_later_commit_is_exact<br>layerfs-daemon/tests/mounted_commit_failures.rs::f2_store_busy_at_publication_is_settled_and_releases_both_owners<br>layerfs-daemon/tests/mounted_commit_failures.rs::f5_store_busy_inside_the_producer_is_settled_and_releases_both_owners<br>layerfs-daemon/tests/store_commit.rs::busy_missing_dependency_and_read_failure_leave_no_stage_or_local_data_loss<br>layerfs-daemon/tests/store_commit.rs::history_busy_after_save_has_no_stage_and_counts_only_acknowledged_save_writes<br>layerfs-overlay/tests/piece_lifetimes.rs::a_failed_capture_folds_pieces_and_cells_below_the_active_layer |
| `external_writers_are_captured_exactly` | 分别由退出 0、非零退出、仍然活着（持有打开的描述符，cwd 在工作区里）的子进程做各类改动（内容、符号链接、可执行位、删除、改名）→ checkpoint 记下磁盘上的精确状态；把它 restore 到一个从基线新克隆的工作区，逐路径比较，种类、内容或链接目标、git mode 都相同；活着的进程再写并退出，下一个 checkpoint 记下新的状态 | layerfs-daemon/tests/mounted_commit.rs::r5_1_external_bash_changes_of_every_kind_are_committed_and_read_back_on_a_fresh_mount<br>layerfs-daemon/tests/mounted_commit.rs::r5_3_commit_captures_what_every_launch_published_whatever_became_of_the_process |
| `writers_are_never_blocked` | 24 个进程持续读写工作区时连续做 3 次 checkpoint：每次都返回，写入者的写调用在 checkpoint 进行期间照常完成，不等它结束；checkpoint 进行时有 20 个进程各自按序追加写入，每个记下的 blob 都等于该文件写入序列的某个前缀，不撕裂，之后的 checkpoint 补齐，最后与磁盘一致；会话 A 的 checkpoint 只含 A 的路径，会话 B 同时写自己的工作区，B 的路径不出现在 A 的条目里，B 也不等 A | layerfs-daemon/tests/mounted_concurrency.rs::r6_1_twenty_four_processes_read_and_write_through_three_commits<br>layerfs-daemon/tests/mounted_concurrency.rs::r6_2_a_commit_against_twenty_writers_holds_an_exact_prefix_of_each<br>layerfs-daemon/tests/mounted_concurrency.rs::r6_3_a_commit_on_one_workspace_while_its_sibling_writes |
| `stale_base_and_wrong_root_are_refused` | 用 store 的移动意图（`begin_move` 直到 `finish_move`）把工作区记下的基线换成另一个已登记的 generation 之后，对旧基线 checkpoint → `InvalidInput`（`stale_base`），没有行；`ws.root` 给成另一个会话的工作区 → `InvalidInput`（`wrong_root`）；同一目录写成 `root/.` 或大小写不同 → 接受；restore 到另一个会话的目录同样被拒绝，两个工作区都不动 | tgrep-core/tests/shared_worktrees.rs::wrong_root_and_reader_swap_are_rejected |
| `git_modes_roundtrip` | 0o100644、0o100755、0o120000 三种条目 checkpoint 再 restore，磁盘上的可执行位与类型都恢复；目录的 mode 与任何 mtime 都不进 checkpoint，restore 写出的文件 mtime 是当前时刻 | layerfs-content/tests/filesystem_attributes.rs::portable_fields_round_trip_and_are_checked |
| `mixed_text_and_binary_in_one_checkpoint` | 一次 checkpoint 同时记下文本文件、含 NUL 与非 UTF-8 字节的二进制文件和空文件，各自的 blob 与磁盘字节逐一相同；它们在同一个号里一起出现，不会只记下一部分 | sdk/js/test/binary_write.mjs::forms<br>sdk/js/test/binary_write.mjs::applied |
| `tool_call_checkpoints_take_hints_and_late_events` | `Tracker` 不启动监听，事件由测试交给 inbox；先记一次 checkpoint 让 `Tracker` 热起来（之后 `needs_full()` 为假）。写 `a.ts`、把它交给 `Inbox::hint`、不送事件 → 紧接着的 `ToolCall` checkpoint 含它，`stats.full = false`；`CLAUDE.md` 是指向 `AGENTS.md` 的符号链接，经它改 `AGENTS.md`、只 hint `CLAUDE.md` → 紧接着的 `ToolCall` 含 `AGENTS.md` 的改动，`stats.full = false`；链接指向工作区之外（`../x.md`），或者目标里 `..` 跟在别的分量之后（`x/../AGENTS.md`）时，hint 之后的那次 `ToolCall` 是全扫（`stats.full = true`）；写 `b.ts`、既不 hint 也不送事件，做一次 `ToolCall`（不含它，`stats.full = false`），再把它的事件交给 `Inbox::note` → 下一次 `ToolCall` 含它，`stats.full = false`；在上次扫描以来没动过的目录里写 `quiet/c.ts`、`Inbox::require_full` 之后的 `ToolCall` 含它（`stats.full = true`、`stats.cold = false`）；写完立刻做 `Manual` 与以 `BeforeRestore` 为原因的 checkpoint，不 hint、不送事件 → 都含这次写入，其中一处是同大小改写、用 `utimes` 恢复了 mtime 的文件；这两次都是 `stats.full = true`、`stats.cold = false`，没改过的候选都由缓存作保（`vouched` 等于它们的个数），只有改过的被读；最后启动真实监听，持着 `Tracker` 调 `flush()` 会返回（不死锁）；这一段只断言返回，不等任何事件送达，默认跑的各段都不靠 FSEvents 的送达时间（apfs.md §4.2 F） | 新增（本模块评审 I9、N2、N3；Addendum 8 的 Event flush, amended；「Cold scan」） |
| `checkpoint_never_captures_through_a_swapped_directory` | `d/y.ts` 改过内容，新内容不在 `latest` 里，所以第 5 步一定要 put 它。`OverlayFault::PauseBeforePut` 停在 `Manual` checkpoint 扫描之后、第 5 步之前，测试把 `d/` 换成指向会话之外一个目录的符号链接，那里有同名、同大小、内容不同的 `y.ts`，再放行 → checkpoint 成功（写竞态，不是失败），这一次就记下 `d` 是一条符号链接条目、`d/` 下的基线文件都是墓碑；两级的情形同理：改过 `a/b/y.ts`，暂停时把 `a` 换成符号链接 → 记下 `a` 一条符号链接条目，`a/` 下的基线文件都是墓碑；blob 存储里没有会话之外那些文件的字节；把这次交换的事件交给 inbox 之后，下一次 checkpoint 与 `reconcile_full` 相同 | 新增（Addendum 8：从工作区存 blob 用 `put_blob_rooted`，从不跟随符号链接） |
| `restore_is_itself_checkpointed` | checkpoint 1 之后改几个文件、新建 `new.txt`、删一个基线文件，然后 `restore(1)`：store 里多了一个 `BeforeRestore` checkpoint，号比 1 大，`Restored.before` 就是它，`written` 与 `removed` 列出动过的路径；再 `restore(before)` → 工作区逐字节回到 restore 之前，`new.txt` 回来了，被删的文件仍被删；对这一次 restore 再 restore 一次也成立；checkpoint 1 仍可读、可再 restore（只追加，不覆盖）；成功之后 T 不存在 | 新增<br>tgrep-core/tests/shared_worktrees.rs::repeated_saves_replace_existing_checkpoint_with_latest_overlay |
| `restore_before_a_rename_brings_back_the_old_name` | checkpoint 1 之后 `mv a.ts b.ts`，restore(1) → `a.ts` 回来，内容是基线的，`b.ts` 不在了，overlay 为空；改名之后又改了内容 → 同样，`BeforeRestore` 里有改过的内容；`core.ignorecase = true` 的 fixture 里 `mv src/Foo.ts src/foo.ts`，restore(1) → 没有 `Exists`，`readdir` 看到的拼写变回 `Foo.ts`；反过来，checkpoint 2 记下改名之后的状态，从改名之前 restore(2) → 拼写变成 `foo.ts`；`paths = None` 只动两边 overlay 里的路径，其余文件的 inode 不变 | 新增（本模块评审 I4） |
| `restore_paths_select_whole_groups` | 只给 `restore` 一个 `Renamed` 的 from 或 path → 两半一起恢复；给类型互换组里的任一路径 → 整组恢复；给一个目录前缀 → 其下全部；给 checkpoint 之后才新建的文件 → 它被删除；给一个哪里都不认识的路径 → `NoSuchPath`，工作区一字未动，也没有记 `BeforeRestore`；给一个只差大小写的路径（磁盘上与清单里都是 `Foo.ts`，给 `foo.ts`）→ `NoSuchPath`，`detail.on_disk` 是 `Foo.ts`；只点名 `src/new.ts`，而 `src` 现在是一条 `Untracked` 的符号链接 → `src` 随之选中：链接被删掉（它在 `BeforeRestore` 里），`src/` 建成目录，`src/new.ts` 写进去 | 新增（裁定 T7：成对携带；增补裁定：点名改名的任一半即选中整个改名；本模块评审 N6） |
| `restore_missing_checkpoint_is_store_error` | restore 一个从未存在的号，以及一个已被 gc 回收的号 → 得到 store 返回的那个 `NoSuchCheckpoint`，code 与 message 都不变、不包装；没有扫描，没有记 `BeforeRestore`，工作区与 store 都不变 | layerfs-workspace/tests/captured_namespace_custody.rs::a_released_reader_is_the_original_stale_refusal_of_the_engine<br>layerfs-overlay/tests/captured_namespace.rs::later_active_rows_install_and_close_leave_reader_answers_until_exact_release |
| `restore_refuses_malformed_checkpoint` | 手工构造的 `Checkpoint`（路径重复、`Deleted` 带 mode、from 等于 path）交给 `Overlay::from_checkpoint` 再 `validate` → `InvalidInput`；store 里一个 checkpoint 的条目落在后来才加进 `deps_dirs` 的目录下 → restore 返回 `InvalidInput`（`excluded_path`），工作区不变；没有一个墓碑被悄悄丢掉：之后 restore 一个好的 checkpoint 仍然逐字节正确。`..`、绝对路径、空段、`.` 构造不出 `RepoPath`（types.md） | tgrep-core/tests/shared_worktrees.rs::malformed_checkpoints_fail_instead_of_revealing_base_entries |
| `restore_never_overwrites_unrecorded_content` | 目标要在 `out/report.txt` 写文件，而那里现在是一个被 ignore 的文件 → `Exists`（`detail.path`），工作区不变，被 ignore 的文件字节不变，也没有记 `BeforeRestore`；目标要删的目录里有一个被 ignore 的 `.DS_Store` → `Exists`；`src/new.ts` 的上级 `src` 现在是一个被 ignore 的文件 → `Exists`（`detail.path` 是 `src`），同样没有记 `BeforeRestore`；正常的 restore 前后，被 ignore 的文件、依赖目录、种子缓存与种子文件逐字节相同，inode 不变 | 新增（不变量 1、4；本模块评审 N6） |
| `restore_concurrent_write_is_kept_in_scene` | `OverlayFault::PauseInApply { after: 0 }` 停在第一次落位之前时，store 里已有那条 `BeforeRestore`，T 里已有 `plan.json`，工作区还没动；`OverlayFault::PauseInApply { after: 1 }` 停住 restore，期间改写一个计划要替换的文件 → restore 返回 `Unknown`，`detail` 带 `before`、`done`、`pending`、`temp`，那次改写的内容完整地留在 T 里；此后的 restore 都返回 `Unknown`（`restore_scene`，`detail` 列出那个没被证明的文件和它原本的路径），直到人把它挪走；挪走之后 T 里只剩 `plan.json` 列出、字节对得上的文件 → 下一次 restore 自动了结它们、照常完成，结束时 T 不存在；做 restore 的子进程在第一次删除之后被杀掉 → T 里有 `plan.json`，下一次 restore 自动了结；T 的位置上是一个符号链接 → `Exists`（`restore_temp_taken`），链接指向的地方不被碰；用 `OverlayFault::PauseBeforePrecheck` 停住，在 T 的位置上放一个指向 R 之外的符号链接再放行 → `Busy`（`workspace_changed`）：只记下了 `BeforeRestore`，链接指向的地方与工作区都一字未动；用 `OverlayFault::PauseBeforePrecheck` 停在预检之前改写一个文件 → `Busy`，工作区一字未动，也没有记 `BeforeRestore`；目标在 `d` 是文件、当前 `d/` 是目录时，用 `OverlayFault::PauseInApply` 停在挪走 `d/` 下最后一个文件的那次改名之后，在 `d/` 里建一个新文件再放行 → `Unknown`，`d/` 与新文件都在，`d/` 原来的文件都在 `BeforeRestore` 里；不打断时 `d/` 被删掉，`d` 是目标的文件 | 新增（不变量 6；module-rulings「Proven copies」「Empty directories, amended」） |
| `restore_never_writes_through_a_symlinked_directory` | 把 `src/` 换成指向会话之外一个目录的符号链接，再 restore 一个 `src/` 还是目录的 checkpoint → 链接本身被删掉，`src/` 重建为目录，文件写进去，链接指向的目录一字未动；用 `OverlayFault::PauseBeforePrecheck` 在扫描之后才把 `src/` 换成符号链接 → `Exists`（`detail.path` 是 `src`：预检里挡路的东西一律是 `Exists`，先于 `Busy` 查），什么都没写，也没有记 `BeforeRestore`；用 `OverlayFault::PauseInApply { after: 1 }` 在落位途中才换 → `Unknown`，链接指向的目录一字未动，没落位的内容留在 T 里；用 `OverlayFault::PauseAfterPlan` 停在 `plan.json` 落盘之后、备临时文件之前，把 T 挪开、在原处放一个指向 R 之外一个空目录的符号链接，再放行 → `Unknown`（`detail.temp` 是 T），那个目录仍然是空的，R 之外没有出现任何文件（`get_blob_rooted` 与 `clone_file_rooted` 都从 R 起逐级不跟随符号链接），工作区一字未动；用 `OverlayFault::PauseBeforeTempMeta` 停在一个基线文件克隆进 T 之后、改它的元数据之前，把 T 里那个名字换成 R 之外一个 mode 0600 文件的硬链接，再放行 → `Unknown`（`detail.temp` 是 T），那个文件的 mode、mtime 与字节都不变，工作区一字未动 | 新增（本模块评审 M19；Addendum 8 与「Addendum 8: get_blob_rooted」；module-rulings「Metadata changes through a handle go through apfs::set_file_meta」） |
| `restore_across_bases_goes_through_rebase` | 在基线 T1 上 checkpoint（会话改了 `s` 与 `b`），工作区被换到 T2（上游改了 `u` 与 `b`）。不给 `target_base` 的 restore → `InvalidInput`；给了之后：`s` 是目标的内容，`u` 保持 T2 的内容（没有被改回 T1），`b` 是目标的版本，列在 `Restored.conflicts` 里；T2 的基线目录前后不变；把 T1 的基线目录删掉、在另一个位置重新导出（同一个 `BaseGen`），restore 照样成功；路径相同而内容不同的另一个 tree（不同的 tree OID）不被当作同一个基线 | tgrep-core/tests/shared_worktrees.rs::identical_base_at_another_location_can_restore_checkpoint<br>tgrep-core/tests/shared_worktrees.rs::checkpoint_rejects_changed_postings_even_with_identical_path_table<br>tgrep-core/tests/shared_worktrees.rs::wrong_root_and_reader_swap_are_rejected |
| `restore_verify_detects_corruption` | 把一个 blob 文件改掉一个字节、长度不变：不带 verify 的 restore 照写（受信任读，裁定 T6），带 verify → `Corrupt`，在写任何东西之前；把基线目录里一个要克隆的文件截短 → 不带 verify 也是 `Corrupt`（大小不符）；只改一个字节不改长度 → 只有 verify 发现 | 新增（store.md：`restore --verify` 用 `verify_blob`；基线目录的完整性由读它的模块核对） |
| `project_materializes_entries_onto_a_clone` | 把一个含各种条目（`Added`、`Untracked`、`Modified`、可执行文件、符号链接、`Deleted`、`Renamed`、目录换成文件的成组条目）的 overlay 投影到基线的一份新克隆上：结果逐路径等于原工作区里被跟踪与 `Untracked` 的内容（种类、内容或链接目标、git mode）；被删光的目录不留下；`project` 不读任何文件的内容。克隆里一个要删的基线文件缺了 → `Corrupt`，从不当作新文件；把克隆里一个上级目录换成指向 dst 之外的符号链接 → `NotDirectory`，dst 之外什么都没写；dst 不在 `<store.root()>/sessions/` 之下 → `InvalidInput`，什么都没写；结束时 dst 正下方没有 `.strata-project-*` | 新增（Addendum 3：投影归 overlay） |
| `rebase_one_sided_changes_project` | 会话改了 `a`、上游改了 `b`、两边把 `c` 改成同样的内容、两边都删了 `d`：rebase 之后 `a` 是 `Modified`（会话版本），`b` 不在 overlay 里，`c` 与 `d` 列在 `dropped` 里，`overlay.base` 是新基线；分类只看新清单：上游新增了一个与会话的 `Untracked` 同名同内容的文件 → 进 `dropped`；rebase 前后工作区不变 | layerfs-workspace/tests/kernel_visit.rs::facts_of_another_base_are_ignored_and_read_again_for_the_present_one |
| `rebase_both_changed_merge_or_conflict` | `merge = true`：两边改了同一个文本文件的不同区域 → 合并结果经 `put_bytes` 存下，条目是 `Modified`，内容等于 `git merge-file` 的结果；改了同一区域 → 冲突，取会话版本，列进 `conflicts`；一边删、一边改（两个方向）→ 冲突，取会话一侧；二进制文件、超过 64 MiB 的文件两边都改 → 冲突；会话把 `f` 改了名、上游改了 `f` → `f` 冲突，改名保留；会话加了可执行位、上游改了内容 → 条目是 `Modified { blob: 上游的 blob, mode: 0o100755 }`，这个 blob 已被存进 blob 存储（`verify_blob` 通过），`project` 到新基线的克隆上得到可执行的上游内容；`merge = false` → 两边都改的路径都是冲突，store 里不新增 blob，两边把 `g` 改成同样的内容、会话还加了可执行位的那一条也一样（它的 blob 等于新清单里的，但本来就在输入 overlay 里） | 新增<br>layerfs-api/tests/merge_by_serial.rs::an_edit_follows_a_moved_directory |
| `rebase_moves_on_both_sides_do_not_conflict` | 会话把 `d/` 改名为 `x/`，上游把 `d/` 改名为 `y/` → 没有冲突，`x/*` 变成 `Added`，`d/*` 进 `dropped`；会话把 `a` 改名为 `b`、上游没动 `a` → 仍是 `Renamed`；上游删了 `a` → `b` 变成 `Added`；会话把 `src/Foo.ts` 只改大小写地改名为 `src/foo.ts`、又改了内容，上游没动它：`RebaseOptions` 为 `ignorecase = false`、`volume_case_sensitive = false`（按卷折叠配对），或 `ignorecase = true`、`volume_case_sensitive = true`（按 git 折叠配对）→ rebase 之后仍是一条 `Renamed { from: "src/Foo.ts" }`，blob 是新内容；两种折叠都按字节（`ignorecase = false`、`volume_case_sensitive = true`）→ `src/Foo.ts` 是 `Deleted`，`src/foo.ts` 是 `Added` | 新增（只改大小写的那一段：module-rulings「Case-only pairing pairs under either fold」）<br>layerfs-api/tests/merge_by_serial.rs::a_directory_renamed_on_both_sides_conflicts |
| `rebase_prefix_collisions_keep_the_session_side` | 会话把目录 `d/` 换成文件 `d`，上游在 `d/` 下加了 `d/new.ts` → 结果里 `d` 是会话的文件，`d/new.ts` 是墓碑（对新清单的 `Deleted`），两者都在 `conflicts` 里；会话加了 `lib/x.ts`，上游把 `lib` 换成文件 → `lib/x.ts` 保留，`lib` 成为墓碑，两者都在 `conflicts` 里；上游把 `lib` 换成符号链接，同样；每种情况 `validate()` 通过，`project` 到新基线的克隆上成功、从不跟随那个符号链接，结果逐路径等于预期 | 新增（本模块评审 I7） |
| `lww_newest_wins_no_conflict` | 新基线的 `.strata/lww` 写着 `*.lock` 与 `generated/**`。两边都改了 `bun.lock` 的不同区域（`merge3` 本可以干净合并）→ 结果逐字节等于会话的版本，没有合并，不在 `conflicts` 里，列在 `lww` 里；两边都改了 `generated/api.ts` 的同一区域 → 同样取会话版本、不冲突；规则之外的 `src/a.ts` 两边改同一区域 → 冲突，保留会话版本；规则之外两边改不同区域 → 干净合并；同样的输入改用 `RebaseOptions::for_restore`（空规则）→ `bun.lock` 成为冲突 | 新增（design §4 `.lww`；发布时的平局规则归 publish，裁定 T10） |

**覆盖**：目标模块为 `overlay` 的 131 行（3a 15、3c 60、3d 27、3e 6、3f 23）每一行都出现在上表的来源栏里，至少一次。理由涉及 overlay 的 `—` 行有 49 条：其中 47 条把某项断言交给了 overlay，随相应的测试列在来源栏里。3c 的两条"潜在缺口"行在本文补定：`new_hole_above_four_gib_uses_bounded_original_run_work` 由 §5.2 第 3 步（稀疏文件整份哈希）与 `sparse_file_is_hashed_whole` 承担，`mutations_published_between_producer_passes_do_not_reach_the_captured_root` 由 §5.4 的快照、§5.2 第 5 步与 §5.6 第 5 步的写竞态规则承担。另外两条只说明 overlay 没有对应的机制，不需要测试：`layerfs-store-service/tests/kept_plans.rs::a_full_walk_answer_is_kept_like_a_plan_and_let_go_like_one`（overlay 没有计划缓存）与 `tgrep-core/src/walker.rs::no_ignore_turns_the_whole_thing_off`（overlay 与 `is_ignored` 没有总开关）。只把 overlay 当作背景提到的 `—` 行（tgrep 的 delta 建库、LayerFS 的 `overlay.sqlite` 等）不在此列；3c 的 `captured_namespace_metadata.rs::a_metadata_patch_keeps_every_other_key_and_its_value_root` 按 Task 15 归 store，由 store.md 的 `blob_files_are_content_only` 承担，本文不认领。Conflict、HeadMoved 与发布时 `.lww` 决议的行归 publish，本文只测 overlay 交出去的 `lww_matches` 与 rebase 里的 `.lww`。

共 77 个测试，其中 29 个的来源是新增。

## 8. 实现任务（TDD）

依赖的三个 crate 先要有 §2 列出的接口：apfs 的 `walk_with`、`WalkOptions`、`list_dir`、`Attrs`、`attrs_of`、`volume_caps`、`case_fold_key`（连同要由测试钉住的前缀性质）、`watch_with`、`Watcher::flush`、`open_root`、`open_rooted`、`open_rooted_file`、`clone_file_rooted`、`create_file_rooted`、`mkdir_rooted`、`symlink_rooted`、`rename_rooted`、`remove_rooted`、`full_fsync`、`classify_io_error`，以及 apfs 下一轮的 `set_file_meta` 与 `FileMeta`；gitio 的 `read_index`、`is_ignored`、`merge3`，以及测试的 fixture 用的 `Git::open`（对主 checkout）、`export_tree`、`ensure_shadow`、`attach_worktree`；store 的 blob（含 Addendum 8 的 `put_blob_rooted` 与 `get_blob_rooted`）、checkpoint、会话与登记接口，以及它的测试接缝。任务按顺序做，后面的任务用前面的产出。

### Task 1: `Overlay` 的形状规则与成组

**Files:**
- Create: `crates/strata-overlay/Cargo.toml`
- Create: `crates/strata-overlay/src/lib.rs`
- Create: `crates/strata-overlay/src/overlay.rs`
- Test: `crates/strata-overlay/tests/overlay_type.rs`

**Interfaces:**
- Consumes: `strata_types::{BaseGen, Checkpoint, OverlayEntry, EntryKind, RepoPath, StrataError, Code}`
- Produces: `Overlay` 及其 `empty`、`from_checkpoint`、`is_empty`、`get`、`hides`、`tombstones`、`paired`、`validate`；`OverlayWarning`、`ScanStats`；`Result`

- [ ] **Step 1: 写失败的测试**：`overlay_validate_rejects_malformed_shapes`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-overlay --test overlay_type`
Expected: FAIL（`strata-overlay` 还没有 `Overlay`）

- [ ] **Step 3: 写让测试通过的最少实现**：§4.3 的 V1–V4，`detail.rule` 指明违反的那一条；`paired` 用并查集合并"改名两半"与"目录前缀"两种关系（§5.3）；Cargo.toml 只依赖 §2 的 crate，`test-seams` feature 先空着
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-overlay --test overlay_type`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-overlay/Cargo.toml crates/strata-overlay/src/lib.rs crates/strata-overlay/src/overlay.rs crates/strata-overlay/tests/overlay_type.rs
git commit -m "feat(overlay): overlay shape rules and pairing"
```

### Task 2: `.lww` 规则

**Files:**
- Create: `crates/strata-overlay/src/lww.rs`
- Test: `crates/strata-overlay/tests/lww.rs`

**Interfaces:**
- Consumes: `strata_apfs::open_rooted_file`；`strata_types::RepoPath`
- Produces: `LwwRules`（`parse`、`load`、`is_empty`）、`lww_matches`、`LWW_FILE`

- [ ] **Step 1: 写失败的测试**：`lww_glob_vectors`、`lww_rules_load_from_strata_lww`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-overlay --test lww`
Expected: FAIL（`LwwRules` 还不存在）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.10 的方言：注释、`!`、结尾 `/`、锚定、`*`、`?`、`**`、`**/` 可以是零个目录、匹配上级目录、最后一条胜出；`%include` 连同主文件最多 16 个；读不到的文件当作空；区分大小写
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-overlay --test lww`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-overlay/src/lww.rs crates/strata-overlay/tests/lww.rs crates/strata-overlay/src/lib.rs
git commit -m "feat(overlay): .lww rules in the LayerFS glob dialect"
```

### Task 3: 读文件与基线清单

**Files:**
- Create: `crates/strata-overlay/src/context.rs`
- Create: `crates/strata-overlay/src/hash.rs`
- Create: `crates/strata-overlay/src/manifest.rs`
- Create: `crates/strata-overlay/src/fault.rs`
- Create: `crates/strata-overlay/tests/common/mod.rs`
- Test: `crates/strata-overlay/tests/manifest.rs`
- Test: `crates/strata-overlay/tests/read.rs`

**Interfaces:**
- Consumes: `strata_apfs::{walk_with, WalkOptions, Attrs, attrs_of, open_rooted_file}`；`strata_types::{BaseManifest, ManifestEntry, BlobId, TreeOid, RepoConfig}`；测试的 fixture 用 `strata_gitio::Git` 的 `open`（对主 checkout）、`export_tree`、`ensure_shadow`、`attach_worktree` 与 store 的登记接口
- Produces: `WorkspaceRef`、`Base`、`build_manifest`；内部的"读一个候选文件"（§5.2，含写竞态规则与名字转换）；`OverlayFault`、`inject_fault`、`release_fault`、`read_candidate`、`ReadProbe`（feature `test-seams`）；测试共用的 fixture（git 仓库、导出的基线、临时 Store、登记好的基线 generation 与会话、接好影子仓库的工作区）

- [ ] **Step 1: 写失败的测试**：`build_manifest_matches_exported_tree`、`write_race_retries_then_marks_unstable`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-overlay --features test-seams --test manifest --test read`
Expected: FAIL（`build_manifest`、`read_candidate` 还不存在）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.9（排除依赖目录、种子缓存与 `.git`）；§5.2 的第 1 到 6 步写成内部函数（读模式），`OverlayFault::PauseAfterHash` 停在第 3 步之后，`read_candidate` 包住它，Cargo.toml 的 `test-seams` feature 从这里起有内容；名字经 `String` 与 `RepoPath::try_from`（§5.1），非 UTF-8 → `NotText`；BLAKE3 在 16 MiB 及以上走 mmap 加多线程
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-overlay --features test-seams --test manifest --test read`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-overlay/Cargo.toml crates/strata-overlay/src/context.rs crates/strata-overlay/src/hash.rs crates/strata-overlay/src/manifest.rs crates/strata-overlay/src/fault.rs crates/strata-overlay/src/lib.rs crates/strata-overlay/tests/common/mod.rs crates/strata-overlay/tests/manifest.rs crates/strata-overlay/tests/read.rs
git commit -m "feat(overlay): base manifests and race-safe reads"
```

### Task 4: 成员、分类与冷扫

**Files:**
- Create: `crates/strata-overlay/src/scan.rs`
- Create: `crates/strata-overlay/src/classify.rs`
- Create: `crates/strata-overlay/src/tracker.rs`
- Create: `crates/strata-overlay/src/compute.rs`
- Test: `crates/strata-overlay/tests/classify.rs`

**Interfaces:**
- Consumes: Task 3 的读文件；`strata_apfs::{walk_with, WalkOptions, volume_caps, case_fold_key}`；`strata_gitio::{Git, IndexInfo}` 的 `read_index`、`is_ignored`、`is_tracked`（不用 `tracked_under`，§5.1）；`RepoConfig` 的 `deps_dirs`、`seed_caches`、`seed_files()`
- Produces: `Tracker::new`、`Tracker::{base, last, warnings, needs_full}`、`compute`（这一步每次都按全扫做，结果正确，只是慢）、`reconcile_full`、`Scan`；`OverlayWarning` 的 `SpecialModeBits`、`SpecialFile`、`EmbeddedRepo`

- [ ] **Step 1: 写失败的测试**：`untouched_workspace_has_empty_overlay`、`restored_to_base_removes_override`、`content_change_and_new_files_classified`、`exec_bit_change_keeps_blob`、`directory_mode_bits_are_never_recorded_or_warned`、`file_symlink_swap_is_modified`、`directory_file_transitions_are_paired`、`recreated_directory_compares_by_path`、`ignored_files_skipped_tracked_never_ignored`、`size_threshold_never_affects_membership`、`special_files_are_never_opened`、`embedded_repo_is_not_entered`、`names_backslash_nfd_and_non_utf8`、`workspace_git_reads_through_the_main_checkout`、`excluded_paths_follow_disk_identity`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-overlay --test classify`
Expected: FAIL（`compute`、`reconcile_full` 还不存在）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.1 全部：排除集合 X（按卷折叠，`volume_caps`）、名字比较的那张表（两种折叠；M ∪ `I.tracked` 的键集合按卷折叠）、受限目录、只认 git 的 ignore 规则、filter 的错误格子、特殊文件不打开、目录不成条目、原始差异与分类；属性缓存（§4.1）在这一步就写，作保条件就是 `Attrs::unchanged_since`；`reconcile_full` = 新 `Tracker` 上的 `compute`
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-overlay --test classify`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-overlay/src/scan.rs crates/strata-overlay/src/classify.rs crates/strata-overlay/src/tracker.rs crates/strata-overlay/src/compute.rs crates/strata-overlay/src/lib.rs crates/strata-overlay/tests/classify.rs
git commit -m "feat(overlay): membership, classification and the cold scan"
```

### Task 5: 改名认定

**Files:**
- Create: `crates/strata-overlay/src/rename.rs`
- Test: `crates/strata-overlay/tests/rename.rs`

**Interfaces:**
- Consumes: Task 4 的原始差异与分类；`strata_apfs::case_fold_key`
- Produces: 分类里的 `Renamed` 配对（§5.3：只改大小写、精确内容、改名到空出来的名字）

- [ ] **Step 1: 写失败的测试**：`directory_moves_expand_per_file`、`exchanged_and_inverted_directories`、`rename_onto_freed_name_and_replacing_renames`、`hard_links_are_independent_paths`、`rename_case_only_recorded_as_rename`、`non_ascii_case_renamed_ignored_tracked_dir_is_still_compared`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-overlay --test rename`
Expected: FAIL（改名还被记成 `Deleted` 加 `Untracked`）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.3 的三步配对，按路径升序、每个源只用一次；n 在 M 里时按"改名到空出来的名字"的规则判
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-overlay --test rename`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-overlay/src/rename.rs crates/strata-overlay/src/classify.rs crates/strata-overlay/src/lib.rs crates/strata-overlay/tests/rename.rs
git commit -m "feat(overlay): deterministic rename pairing"
```

### Task 6: 脏集合与增量扫描

**Files:**
- Create: `crates/strata-overlay/src/inbox.rs`
- Modify: `crates/strata-overlay/src/tracker.rs`
- Modify: `crates/strata-overlay/src/compute.rs`
- Test: `crates/strata-overlay/tests/incremental.rs`

**Interfaces:**
- Consumes: `strata_apfs::{list_dir, watch_with, WatchOptions, Watcher, WatchEvent, case_fold_key}`；Task 4、5 的分类
- Produces: `Inbox` 与 `Inbox::note`、`Tracker::{inbox, watch}`、`DEFAULT_DIRTY_CAPACITY`；`compute` 的增量路径；`OverlayWarning::Unstable`

- [ ] **Step 1: 写失败的测试**：`point_query_and_walk_agree`、`reconcile_after_kernel_dropped_equals_cold_scan`、`dirty_set_dedups_by_canonical_path`、`event_spellings_and_subtrees_reverify`、`dropped_or_overflowing_events_force_full_scan`、`index_changes_reclassify_without_events`、`excluded_dirs_never_enter_overlay`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-overlay --test incremental`
Expected: FAIL（`Inbox` 还不存在）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.4 里 `hint` 与 `require_full` 之外的全部：常数时间的 `note`（按 inbox 当前的 X、按卷折叠过滤）、上限与全扫、快照与失败时并回、拼写解析、文件与子树的重判、index 对称差；§5.5 的增量分支、X 变了就全扫（含 index 引起的）与 `needs_full` 的各个来源
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-overlay --test incremental`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-overlay/src/inbox.rs crates/strata-overlay/src/tracker.rs crates/strata-overlay/src/compute.rs crates/strata-overlay/src/lib.rs crates/strata-overlay/tests/incremental.rs
git commit -m "feat(overlay): dirty set and incremental scans equal to the cold scan"
```

### Task 7: checkpoint

**Files:**
- Create: `crates/strata-overlay/src/checkpoint.rs`
- Modify: `crates/strata-overlay/src/inbox.rs`
- Modify: `crates/strata-overlay/src/tracker.rs`
- Modify: `crates/strata-overlay/src/fault.rs`
- Test: `crates/strata-overlay/tests/checkpoint.rs`

**Interfaces:**
- Consumes: `strata_store::Store` 的 `session`、`put_blob_rooted`、`latest_checkpoint`、`record_checkpoint`、`checkpoint`，测试里还有 store 的 `Seam` 与 `arm`；`strata_apfs::attrs_of` 与 `Watcher::flush`；Task 6 的 `Inbox` 与 `Tracker`；`strata_types::{Checkpoint, CheckpointReason, CheckpointId}`
- Produces: `checkpoint`、`Checkpointed`；`Inbox::{hint, require_full}`、`Tracker::flush`；`OverlayFault::{PauseAfterSnapshot, PauseBeforePut}`

- [ ] **Step 1: 写失败的测试**：`new_then_deleted_path_leaves_no_entry`、`mtime_only_change_is_no_change`、`rename_records_renamed_entry`、`every_change_kind_reaches_the_next_checkpoint`、`periodic_full_scan_repairs_missed_changes`、`quiet_workspace_stays_clean`、`failed_checkpoint_keeps_dirty_set`、`writes_during_scan_reach_the_next_scan`、`work_grows_with_the_change_not_the_base`、`mmap_writes_count_once_written_back`、`unchanged_checkpoint_is_up_to_date`、`checkpoint_never_touches_base_or_leaves_residue`、`checkpoint_store_failures_are_settled`、`writers_are_never_blocked`、`mixed_text_and_binary_in_one_checkpoint`、`tool_call_checkpoints_take_hints_and_late_events`、`checkpoint_never_captures_through_a_swapped_directory`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-overlay --features test-seams --test checkpoint`
Expected: FAIL（`checkpoint` 还不存在）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.4 的 `Inbox::hint`（符号链接只解一跳，`..` 只许在目标开头）、`Inbox::require_full` 与 `Tracker::flush`；§5.6 的七步：核对（`stale_base`、`wrong_root` 用 dev + inode）、按原因选扫描（`Manual`、`Before*` 全扫；`ToolCall` 增量，`require_full` 之后同样全扫）、无变化不写、`put_blob_rooted` 作为存储时的哈希器与写竞态规则（`NoSuchPath`、`IsDirectory`、`NotFile` 重判路径，`NotDirectory` 重判从根数起最浅的那个已不是目录的分量，§5.4 第 2、3 步）、`PauseBeforePut` 暂停点、记行与 `ToolCall` 去重的识别、失败时并回 inbox
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-overlay --features test-seams --test checkpoint`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-overlay/src/checkpoint.rs crates/strata-overlay/src/inbox.rs crates/strata-overlay/src/tracker.rs crates/strata-overlay/src/fault.rs crates/strata-overlay/src/lib.rs crates/strata-overlay/Cargo.toml crates/strata-overlay/tests/checkpoint.rs
git commit -m "feat(overlay): checkpoints that store only what changed"
```

### Task 8: rebase 与投影

**Files:**
- Create: `crates/strata-overlay/src/rebase.rs`
- Create: `crates/strata-overlay/src/project.rs`
- Test: `crates/strata-overlay/tests/rebase.rs`

**Interfaces:**
- Consumes: `strata_gitio::{merge3, Merge3}`；`strata_store::Store` 的 `read_blob`、`put_bytes`、`put_blob`、`get_blob_rooted`、`root`；`strata_apfs::{open_rooted_file, mkdir_rooted, symlink_rooted, rename_rooted, RenameMode, remove_rooted, RemoveKind, classify_io_error, set_file_meta, FileMeta, volume_caps}`；测试里还有 `verify_blob`；Task 2 的 `lww_matches`；Task 4、5 的分类与配对
- Produces: `rebase`、`RebaseOptions`（`for_move`、`for_restore`）、`Rebased`、`MERGE_MAX_BYTES`、`project`

- [ ] **Step 1: 写失败的测试**：`rebase_one_sided_changes_project`、`rebase_both_changed_merge_or_conflict`、`rebase_moves_on_both_sides_do_not_conflict`、`lww_newest_wins_no_conflict`、`rebase_prefix_collisions_keep_the_session_side`、`project_materializes_entries_onto_a_clone`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-overlay --test rebase`
Expected: FAIL（`rebase`、`project` 还不存在）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.8 的各步：参数与 V5 核对、逐路径三方判定（`.lww` 在内容合并之前）、基线文件读出后核对 BLAKE3、合并结果等于一边时复用那一边的 blob、前缀冲突（第 4a 步）、对新清单重新分类（只改大小写的配对用 `opts` 的两种折叠）、blob 不变量（第 5a 步）、`dropped` 的定义；§5.12 的投影
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-overlay --test rebase`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-overlay/src/rebase.rs crates/strata-overlay/src/project.rs crates/strata-overlay/src/lib.rs crates/strata-overlay/tests/rebase.rs
git commit -m "feat(overlay): whole-file three-way rebase with .lww, and projection"
```

### Task 9: restore

**Files:**
- Create: `crates/strata-overlay/src/restore.rs`
- Test: `crates/strata-overlay/tests/restore.rs`

**Interfaces:**
- Consumes: `strata_store::Store` 的 `checkpoint`、`get_blob_rooted`、`read_blob`；`strata_apfs::{clone_file_rooted, create_file_rooted, open_root, open_rooted, open_rooted_file, mkdir_rooted, symlink_rooted, rename_rooted, remove_rooted, attrs_of, full_fsync, classify_io_error, set_file_meta, FileMeta}`；libc 的 `flistxattr`、`fremovexattr`、`fsync`；Task 7 的 `checkpoint`；Task 8 的投影写法
- Produces: `restore`、`RestoreRequest`、`Restored`、`RESTORE_TEMP_PREFIX`

- [ ] **Step 1: 写失败的测试**：`restore_is_itself_checkpointed`、`restore_before_a_rename_brings_back_the_old_name`、`deleted_base_file_is_tombstone`、`symlink_entry_is_exact_target_bytes`、`special_mode_bits_are_normalized_and_warned`、`git_modes_roundtrip`、`sparse_file_is_hashed_whole`、`xattrs_are_never_recorded_or_restored`、`empty_directories_are_not_carried`、`external_writers_are_captured_exactly`、`checkpoint_is_immutable_and_restorable`、`restore_paths_select_whole_groups`、`restore_missing_checkpoint_is_store_error`、`restore_refuses_malformed_checkpoint`、`stale_base_and_wrong_root_are_refused`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-overlay --features test-seams --test restore`
Expected: FAIL（`restore` 还不存在）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.7 的同基线路径：content(X, q) 与拼写解析，阶段 A 的第 2、3 步与第 5 到 7 步、第 8 步里的 `ReadOnly`、`OpenFiles` 与基线文件的 `Corrupt`，阶段 B 记 `BeforeRestore`，阶段 C 写 `plan.json`、在 T 里备内容（句柄上的元数据经 `set_file_meta`）、挪开再证明再删与 `RenameMode::Swap`，阶段 D 删 `plan.json` 与空的 T、写缓存；路径选择（`paths = None` 的定义、按字节匹配）与成组闭包
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-overlay --features test-seams --test restore`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-overlay/src/restore.rs crates/strata-overlay/src/lib.rs crates/strata-overlay/tests/restore.rs
git commit -m "feat(overlay): a restore that checkpoints first"
```

### Task 10: restore 的现场、校验与跨基线

**Files:**
- Modify: `crates/strata-overlay/src/restore.rs`
- Modify: `crates/strata-overlay/src/fault.rs`
- Test: `crates/strata-overlay/tests/restore_safety.rs`

**Interfaces:**
- Consumes: Task 8 的 `rebase` 与 `RebaseOptions::for_restore`；`strata_store::Store::verify_blob`；`strata_apfs::list_dir`（看 T）
- Produces: `restore` 的跨基线路径（`RestoreRequest.target_base`）、`verify`、`restore_scene` 现场与 `Busy`（`workspace_changed`）；`OverlayFault::{PauseBeforePrecheck, PauseAfterPlan, PauseBeforeTempMeta, PauseInApply}`

- [ ] **Step 1: 写失败的测试**：`restore_never_overwrites_unrecorded_content`、`restore_concurrent_write_is_kept_in_scene`、`restore_verify_detects_corruption`、`restore_across_bases_goes_through_rebase`、`random_edits_incremental_equals_cold_and_restores_exact`、`restore_never_writes_through_a_symlinked_directory`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-overlay --features test-seams --test restore_safety`
Expected: FAIL（跨基线的 restore 还返回 `InvalidInput`，预检还没有 `Exists` 与 `Busy`）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.7 第 1 步（T 被占时的 `Exists`，现场的判定与自动了结，`lifecycle` 或 `health` 为 `Unknown` 时的拒绝）、第 4 步（经 `rebase`）、第 8 步其余的预检（`Exists`、`Busy`、上级目录的类型、`verify`）与第 9 步的 `Busy`、第 10 步的落盘顺序与建 T 时的 `Busy`、阶段 C 失败时 `detail` 的 `before`、`done`、`pending`、`temp`；四个暂停点 `PauseBeforePrecheck`、`PauseAfterPlan`、`PauseBeforeTempMeta`、`PauseInApply`；proptest 的操作生成器放在测试文件里
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-overlay --features test-seams --test restore_safety`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-overlay/src/restore.rs crates/strata-overlay/src/fault.rs crates/strata-overlay/tests/restore_safety.rs
git commit -m "feat(overlay): restore scenes, verification and cross-base projection"
```

## 9. 构建者决定记录

| 事项（design 里标 [交给构建者]、落在本模块） | 选择 | 理由 | 日期 |
| --- | --- | --- | --- |
| 对账间隔（design §6.2、§12） | 每小时一次 `compute(…, None)`；`Manual`、`Before*`（含发布前的 `BeforePublish`）与没有路径清单的工具调用之后也全扫（§5.6 第 2 步）；调度由 daemon 做 | design 给的默认是每小时与发布前；Addendum 8 要 `Manual` 与 `Before*` 做不依赖事件的全扫（module-rulings「Cold scan」）；全扫只遍历元数据，tool 的源码树温热 0.20 s（apfs.md §5.9） | 2026-10-22；调度见 daemon.md |
| 大文件阈值（design §7、§12），overlay 这一侧 | 阈值不影响成员资格，超阈值的文件照常进 overlay、整份哈希；rebase 的内容合并上限 `MERGE_MAX_BYTES` 取同一个值 64 MiB，超过就算冲突 | 3f:463：超限的文件绝不能被当成删除；三方合并几十 MB 的文件既慢又少有意义 | 2026-10-22；阈值本身若改，两处一起改 |
| 属性缓存是否持久化 | 不持久化，daemon 重启后第一次扫描冷扫（§4.1） | 持久化就得证明重启期间没漏掉写入；冷扫 tool 是 42.3 MB，远低于 3 s | 2026-10-22 |
| 脏集合上限 | `DEFAULT_DIRTY_CAPACITY` = 65 536 项，超过即全扫 | 超过这个量，一次热缓存的全扫并不比逐项处理慢 | 2026-10-22；spike 实测后可调 |
| 哈希的并行度 | 16 MiB 以下的文件单线程流式哈希，文件之间用 rayon 并行；16 MiB 及以上 mmap 加多线程，与 store 相同 | 与 store 的 put 一致，两边算出的 BLAKE3 恒等 | 2026-10-22 |
| 每小时全扫要不要偶尔冷扫 | 不做：gen count 抓得住每一次内容写入（§4.1 实测，apfs.md §4.2 C） | design §8："我们不读内容"；冷扫只在没有缓存时发生 | 2026-10-22；spike 若发现 gen count 有漏报，改为每天冷扫一次 |

本文其余标 [我的决定] 的地方，按出现的顺序（design、Addendum 1–8 与其后的裁定都没有定）：

| 位置 | 决定 | 理由 |
| --- | --- | --- |
| §3 `LwwRules::parse` | 写坏的行（空模式、只有 `!`）跳过，解析从不失败 | 读不全只会让更多文件按普通文件合并或报冲突，不会让文件被悄悄覆盖 |
| §3 `build_manifest` | 比 Addendum 3 的 `build_manifest(dir)` 多了 `tree` 与 `config` | `BaseManifest` 有 `tree` 字段；排除的依赖目录、种子缓存要从 `config` 算 |
| §3 `checkpoint` 的返回值 | `Checkpointed`：号、有没有写、变了哪些路径、告警 | 调用方要区分 UpToDate；session 只取 `.checkpoint.id` |
| §3 `restore` 的目标 | `CheckpointId`（经 `RestoreRequest`），不是 `&Checkpoint` | overlay 在同一次串行操作里自己读它，store 的 `NoSuchCheckpoint` 原样透传 |
| §3 测试接缝 `read_candidate`、`ReadProbe` | 写竞态规则在 Task 3 实现时就有一个先失败的测试 | 它是 §5.2 的规则，不该等到 checkpoint 那一步才测 |
| §3 restore 的 `NoSuchPath` | 除了目标 checkpoint 与两边的基线，也认当前 overlay 里的路径；按字节比较，只差大小写时 `detail.on_disk` 给出磁盘上的拼写 | 路径限定的 restore 要能撤销 checkpoint 之后新建的文件 |
| §3 restore 的 `OpenFiles` | 要动的文件有可写的 mmap（gen count 不可得）时用它 | 共同规则下最接近的已有码；Task 15 在 types.md §6.2 的"谁返回"补上 overlay |
| §5.1 种子文件 | `config.seed_files()` 的候选里，按卷折叠既不是 M 也不是 index 里的路径的，进排除集合 X | `.gitignore` 改了也不会让 `.env` 以 `Untracked` 被发布出去；被跟踪的 `.ENV` 占着 `.env` 的位置时它照常比较 |
| §5.1 X 之下的基线条目 | 不比较、不成为墓碑 | 依赖目录里被跟踪的文件，工作区里的那一份是依赖集的克隆，不是会话的改动 |
| §5.1 `staged` | 用 gitio 的 `IndexInfo::is_tracked`（只折叠 ASCII） | `Added` 与 `Untracked` 的分界照搬 git 自己的回答 |
| §5.1 restore 的 `paths` | 按字节比较；按卷折叠只用来给 `NoSuchPath` 的提示 | 人给的路径就是人要的那个，不猜 |
| §5.1 嵌套仓库 | `EmbeddedRepo` 告警，按受限目录进入（只看 M 与 index 里的名字） | 不把别人的仓库当成 `Untracked` 发布，也不丢基线文件 |
| §5.1 特殊文件 | 从不打开；它占着的基线路径按"磁盘上没有"处理 | FIFO 挂不住扫描；git 也记不下特殊文件 |
| §5.3 配对顺序 | 先配只改大小写的（按卷或按 git 折叠相等），再按精确内容配；V 按路径升序，每次取最小的 n | 配对只依赖最终状态，结果确定（3c:99） |
| §5.4 `.gitignore` 的事件 | 要求全扫 | ignore 规则变了，整棵树的成员资格都可能变 |
| §5.4 全局排除文件 | 不监听，改动在下一次全扫时生效 | 发布前与 `Before*` 都全扫，所以不会因此发布错文件（O7 的例外） |
| §5.4 X 在 inbox 里过滤 | 监听只排除 `.git`；依赖目录、种子缓存与种子文件由 `Inbox::note` 按当前的 X 丢弃；X 变了的那次扫描先换 inbox 的 X，再取快照；比较按卷折叠，与 apfs 相同 | apfs 本来就在回调里按 `exclude` 过滤，代价相同；X 缩小之后，重新纳入的目录里的事件不再被丢，不用重新 watch，也没有两个流交接的空档（本模块评审 N1） |
| §5.4 hint 与符号链接 | 经符号链接写的路径，hint 把链接指向的路径也记下（相对目标、只解一跳、`..` 只在目标开头、落在根之内）；其余要求全扫 | Edit、Write 经 `CLAUDE.md` → `AGENTS.md` 这样的链接写，写到的是目标（本模块评审 N2）；多跳或指到根外的少见，全扫兜底；`..` 跟在别的分量之后时按字面规整可能与内核的解析不同 |
| §5.5 X 变了 | 不论因为什么（仓库配置、index 让种子文件进出第 3 项）都全扫 | 很少发生；一次全扫之后原始差异里没有过时的条目，离开 X 的路径也都看过，不用逐项删、逐项记脏的簿记（本模块评审 N13） |
| §5.6 没有路径清单的工具调用 | daemon 先 `Inbox::require_full`，这次 `ToolCall` 是与 `Manual` 相同的全扫 | Addendum 8 让 overlay 在全扫与"晚一个工具调用"之间选。晚一个调用更便宜，但 checkpoint N 会缺第 N 个调用里 Bash 的写入，restore 到 N 会撤掉 N 的一部分（`BeforeRestore` 仍兜底，不丢字节）；全扫让这种调用的 checkpoint 含它返回之前完成的写入，代价是 tool 上 0.20 s，超出 design §8 的 200 ms（module-rulings T13 一条：阶段末尾交给用户） |
| §5.6 `Before*` | 没有变化也记一行 | 调用方需要一个能指认的号（`Restored.before`、session 的 `RemoveReport.checkpoints`） |
| §5.7 T 的位置 | 会话根下的 `.strata-tmp-restore-<repo>/`，与工作区同卷 | 改名不跨卷；在工作区之外，监听看不到，也不会被当成 `Untracked` |
| §5.7 跨基线的 restore | `RebaseOptions::for_restore`：不合并、不用 `.lww`，两边都改的路径取目标的版本并报冲突 | restore 只写回目标里真实存在过的内容 |
| §5.7 `paths` 的选择 | 按字节；等于或是候选路径的目录前缀；再在 C 与 D 的并集上取成组闭包 | 改名、类型互换成组恢复（裁定 T7） |
| §5.7 预检里挡路的东西 | 一律 `Exists`，在记 `BeforeRestore` 之前返回 | 扫描不进被剪掉的 ignore 目录、不留它们的属性，分不出扫描之前就在还是之后才出现；两种情况都要人先挪开它（本模块评审 N6） |
| §5.7 基线文件的克隆 | 清 BSD 标志并设 mode、清 xattr、mtime 设为当前时刻；标志、mode 与 mtime 经 `apfs::set_file_meta` | 与 `get_blob_rooted` 的产物一致；增量构建要把内容变了的文件当成变了 |
| §5.7 持久性 | 工作区里的文件不做 `F_FULLFSYNC`，`plan.json` 做 | 与 git checkout 一样；`BeforeRestore` 与 `plan.json` 已落盘，掉电之后总能恢复 |
| §5.8 `.lww` | 命中的路径会话一侧胜出 | rebase 投影的 overlay 来自刚记下的 checkpoint，比新基线新；保证不了这一点的 restore 传空规则 |
| §5.8 前缀冲突 | 保留会话一侧，基线一侧的路径成为墓碑，两边都报冲突 | 会话的改动一个不丢，结果满足 V4，`project` 不会把它当成损坏 |
| §5.8 配对用的卷 | 卷是否区分大小写放进 `RebaseOptions.volume_case_sensitive`：`for_move` 从 `apfs::volume_caps(&new.dir)` 读，`for_restore` 由 restore 传入 | rebase 不自己探卷，结果只由参数决定；测试不用磁盘映像就能覆盖两种卷 |
| §5.10 `%include` | 最多读 16 个文件；读不了的当作空文件 | 读不全只会让更多文件按普通文件处理，不会让文件被悄悄覆盖 |
| §5.10 `.lww` 的大小写 | 区分大小写，按 NFC 之后的字节比较 | 同一份规则在每个会话、每台机器上给出同样的判定 |
| §5.12 dst 的位置 | 必须在 `<store.root()>/sessions/` 之下，否则 `InvalidInput` | 一道便宜的防线：`project` 写不到主 checkout、基线目录或 blob 存储里 |
| §5.12 临时名 | dst 正下方的 `.strata-project-<序号>` | Addendum 7、8：取到 strata 自己的目录正下方，再 `rename_rooted` 到位置上 |
