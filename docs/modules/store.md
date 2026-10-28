# `store` 模块

**Crate**：`crates/strata-store` · **设计来源**：design.md §5（模块表）、§6.1、§6.2、§6.4、§6.6、§7、§8、§9、§11 第 8 条；module-rulings.md 的「T6 store」、标了 T6 与 Everyone 的增补，以及增补 2、3、4（增补 4 把全部持久数据的 schema 交给本文） · **矩阵**：test-matrix 里目标模块为 `store` 的行（3a 45 行、3c 4 行、3d 5 行、3f 2 行，另加裁定改归 store 的 3c L139），以及理由点名 store 的 15 条 `—` 行

## 1. 职责与边界

store 负责 git-strata 全部的持久元数据和内容存储，落在 `[paths] root`（config.md，默认 `/Volumes/GWD/inter/.strata`）之下。按增补 4，**只有一份 schema，全部在本文 §4.3**：别的模块不执行 SQL，store 也不提供通用的写事务，每个写都是 §3 里一个具名方法、一个事务。

- **元数据库**（`<root>/store/store.db`；引擎 Turso，SQLite 文件格式）：基线 generation 与基线清单；依赖集、依赖集的认领与冷装尝试、依赖变更提议、主干当前的依赖集；会话、工作区、种子文件、`move_workspace` 的意图；checkpoint 与 overlay 条目；blob 登记；发布记录与 ref 变动审计；gc 记录（design §5 模块表、§7，增补 3、4）。
- **按 BLAKE3 寻址的 blob 存储**（`<root>/store/blobs/`）：一文件一 blob，存为普通文件；写入 = 从工作区 `clonefile` 进来（零拷贝；工作区里的文件经 `put_blob_rooted`，每一级都不跟随符号链接，增补 8），读出 = 反向克隆（design §7；写进会话树的经 `get_blob_rooted`，同样每一级都不跟随，增补 8）。
- **`<root>` 的布局**：基线、依赖集与依赖集暂存目录的路径由 store 按键算出（§4.1）。目录由 session、deps 建，由 store 登记；目录离开活路径只有一条路：`retire_dir` 把它改名进 store 的 trash（增补 3），之后只由 gc 删除。
- **回收**：`gc` 与 `gc_repo` 按引用与保留期删除行与目录，是全模块唯一发出 `DELETE` 的路径（裁定 T6）。
- **状态图的守卫**：每个模块拥有自己状态的词汇、转换图和含义；store 用自己定义的行类型镜像它们（从不依赖 deps、session、publish 的类型，由所有者把行映射到自己 §3 的类型），并用触发器守住转换图（增补 4）。
- **持久与并发规则**：钉死的 SQLite 设置、一个写者、读者永不被写者阻塞、校验策略、序号规则、崩溃之后留下什么（§4–§6）。

不负责的事和它们的去处：

- 计算 overlay、决定哪些文件进 checkpoint、写竞态规则（哈希之后重读 gen count）、restore 与 `move_workspace` 的投影、由基线目录建出 `BaseManifest`（`overlay::build_manifest`，增补 3）：overlay（overlay.md）。store 只存 overlay 交来的条目、blob 与清单。
- 导出基线树、克隆工作区、种子文件、`move_workspace` 的目录交换、会话生命周期与现场（scene）的含义、什么时候写意图行、暂存放在哪里：session（session.md）。store 存会话、工作区、种子、意图的行，守住它们的转换图。
- 依赖集的认领、冷装、封存，`inputs_hash` 与 `DepsSetId` 的计算，提议何时登记、提升、取代、批准：deps（deps.md）。store 存 deps 的四张表（增补 4 把它们从 deps.md 收进本文），分配 `ProposalId` 与尝试序号。
- 批次与发布组的合成、三方合并、检查、发布链与推送：publish（publish.md）与 gitio。store 只记发布行、批次路径和 ref 变动。发布的阶段词汇 `Running`、`Done`、`Unknown` 是暂定的，publish.md 采用它或在它的 §2 列出要改的地方（增补 4，T15 核对）。
- 什么时候跑 gc（`recover` 之后，一次一个 repo，持该 repo 的写锁）、保留期 `keep` 取多少、按 repo 的写锁与 `Busy` 的等待上限、重启之后报出 `Unknown` 的发布与中毒的写者：daemon（daemon.md，增补 4 的 T13 一条）。
- 签发与校验 `ConfirmToken`：confirm。store 只在 ref 审计里存核对过的 `Action`，从不存凭据（types.md §4）。
- 写工作区之前先 checkpoint（不变量 1）：调用方的事。store 从不覆盖工作区里已有的文件，`get_blob` 与 `get_blob_rooted` 只新建 `dst`。

## 2. 依赖

Workspace crate：

- `strata-types`：全部标识（`BlobId`、`CheckpointId`、`ProposalId`、`SessionId`、`RepoName`、`RepoPath`、`TreeOid`、`CommitOid`、`DepsSetId`），结构（`BaseGen`、`DepsSet`、`BaseManifest`、`ManifestEntry`、`OverlayEntry`、`EntryKind`、`Checkpoint`、`CheckpointReason`、`SessionManifest`、`WorkspaceState`、`ReleaseGroup`、`Batch`、`Action`），`StrataError` 与 `Code`，以及 `serde_rfc3339` 适配模块。
- `strata-apfs`：`clone_file`（`put_blob` 与 `get_blob`）、`same_volume` 与 `nearest_existing_ancestor`（`open` 的同卷检查）、`classify_io_error`（`io::Error` 的归类），以及在根之下不跟随符号链接的操作：`attrs_of` 与 `clone_from_rooted`（`put_blob_rooted`）、`clone_file_rooted` 与 `open_rooted_file`（`get_blob_rooted`）、`set_file_meta`（规整 put 的克隆体与 get、`get_blob_rooted` 刚克隆出的文件）、`open_rooted` 与 `make_removable`（准备 generation 的根目录）、`rename_rooted`（`retire_dir` 与 gc 改名进 trash）、`remove_tree_rooted`（gc 删 trash 与 `store/tmp/` 的每个条目，不论种类）、`remove_rooted`（删清扫之后变空的目录）、`list_dir` 与 `Attrs::last_changed`（gc 清扫活路径与基线暂存，列出 trash 与 tmp 的条目）、`full_fsync`（改名之后同步 trash）。增补 8：store 在会话树、generation 树与 trash 上不自己发 `*at` 调用，那里的打开、克隆、改名与删除全经 apfs；store 自己的 `store/` 之下（`tmp/`、`blobs/`、库文件）照常用 libc，只是 gc 删 `tmp/` 的条目也经 `remove_tree_rooted`；store 自己从不调 `fchflags`、`fchmod`、`futimens`：get 出来的文件与 put 的克隆体都经 `set_file_meta` 改元数据，从不按路径改标志（module-rulings 的"remove_tree_rooted takes any entry kind""Metadata changes through a handle go through apfs::set_file_meta"）。这条规则管的是别的进程摸得到的文件（会话树、暂存、基线 generation、工作区）；store 的私有区（`store/blobs`、`store/tmp`）里增补 8 的 libc 许可照旧：put 的克隆体经 `set_file_meta` 是选择，put 第 7 步按路径刷新已存 blob 的时间（`utimensat(AT_SYMLINK_NOFOLLOW)`）也留着，能经根之外的硬链接摸到一个 blob，说明有人动了 strata 的私有区，能变的也只有它的时间（module-rulings 的"set_file_meta scope, corrected"）。types.md §8 第 1 层只允许 store 依赖 apfs 这一个同层例外；依赖表以 types.md 为准（裁定 T6）。

apfs.md（@1e723f2，module-rulings 的"apfs names committed at sdd/task-4 1e723f2"）里 store 用到的签名：

- `clone_file(src: &Path, dst: &Path) -> Result<()>`：符号链接克隆成符号链接、从不跟随，`dst` 必须不存在（已存在 → `Exists`，从不覆盖），原子。`src` 按路径解析，上级里的符号链接照常跟随，所以 `put_blob` 只给 strata 自己的文件用。
- `same_volume(a: &Path, b: &Path) -> io::Result<bool>`：两个路径都跟随符号链接，路径不存在是 `io::ErrorKind::NotFound`，store 用 `classify_io_error` 归类（`NotFound` → `NoSuchPath`）。
- `nearest_existing_ancestor(path: &Path) -> io::Result<PathBuf>`：从 `path` 往上找第一个存在的路径（`path` 自己存在就是它自己），即 config.md §4.3 的"最近的已存在上级"。`open` 的同卷检查就是 `same_volume(&nearest_existing_ancestor(root)?, checkout)`：这条规则只在 apfs 写一份（M17），store 不自己往上找。
- `classify_io_error(err: &io::Error, side: IoSide, path: &Path) -> StrataError`：types.md §6.3 的 errno 归类表加 apfs 的细化；`IoSide` 分读、写、已经写了一部分之后三种，`detail` 带 `errno` 与 `path`。store 对 apfs 交来的句柄、对自己 `store/` 之下的文件做 I/O 失败时都用它，不另写一张表（§6.2）。
- `open_rooted(root: &Path, path: &RepoPath) -> Result<File>`：逐级 `openat(O_NOFOLLOW)`；能打开普通文件、目录与 FIFO，最后一级是符号链接、socket 或设备 → `NotFile`，上级里有符号链接 → `NotDirectory`（`reason = symlink`）。store 把它返回的目录句柄交给 `make_removable`（§5.5），或拿来做 `full_fsync`。
- `open_rooted_file(root: &Path, path: &RepoPath) -> Result<File>`：同 `open_rooted`，另要求是普通文件：目录 → `IsDirectory`，FIFO、设备 → `NotFile`。`get_blob_rooted` 把它返回的句柄交给 `set_file_meta`。
- `clone_file_rooted(src: &Path, root: &Path, dst: &RepoPath, mode: Option<u32>) -> Result<()>`：`dst` 的上级逐级以 `O_DIRECTORY | O_NOFOLLOW` 打开并钉住，任何一级是文件或符号链接 → `NotDirectory`（符号链接时 `reason = symlink`），上级不存在 → `NoSuchPath`，都在写任何东西之前；然后在钉住的父目录上只用最后一个名字 `clonefileat`，`dst` 已存在（含悬空符号链接）→ `Exists`，从不覆盖。`src` 按路径解析，store 给的是自己的 blob 文件。克隆带着源的 BSD 标志，所以 store 传 `None`，清标志与设 mode 都交给之后的 `set_file_meta`（§5.4）。
- `clone_from_rooted(src_root: &Path, src: &RepoPath, dst: &Path) -> Result<()>`：以不跟随符号链接的方式打开 `src`（每一级都不跟随，最后一级也不：符号链接、socket、设备、FIFO → `NotFile`），再从这个描述符克隆（`fclonefileat`），所以克隆的就是此刻在 `src` 上的那个对象，绝不是根之外的东西；`dst` 必须不存在（`Exists`，从不覆盖）；其余错误同 `clone_file`。在 1e723f2 里，`src` 是目录时它克隆整棵树（2026-10-22 实测：对目录的描述符，`fclonefileat` 克隆整棵树）；store 要的 `kind` 参数见下面的上游要求。
- `rename_rooted(root: &Path, from: &RepoPath, to: &RepoPath, mode: RenameMode) -> Result<()>`，`RenameMode { Excl, Swap }`：store 只用 `Excl`。单次 `renameatx_np(RENAME_EXCL)`，两边的上级都钉住、不跟随；`to` 已存在 → `Exists`；0555 的目录改到别的父目录 → `ReadOnly`（`reparent_needs_write`），带 `uchg` → `ReadOnly`（`immutable`），所以 store 改名之前先准备根目录（§5.5）。它不同步，要扛住断电的调用方自己对父目录 `full_fsync`。
- `remove_rooted(root: &Path, path: &RepoPath, kind: RemoveKind) -> Result<()>`，`RemoveKind { File, EmptyDir }`：单次 `unlinkat`，不 chmod、不清标志。`EmptyDir` 删一个空目录（不空 → `NotEmpty`）；`File` 删一个文件或符号链接本身（是目录 → `IsDirectory`）。store 只用 `EmptyDir`。
- `remove_tree_rooted(root: &Path, path: &RepoPath) -> Result<RemoveReport>`：删一棵目录树，在 1e723f2 里 `path` 必须是目录（是符号链接或文件 → `NotDirectory`）；树里的符号链接只删链接本身、不跟随，不跨设备，为删掉它清掉条目的 `UF_IMMUTABLE` 与 `UF_APPEND`、给打不开的目录补属主权限，自底向上。store 要它也接受不是目录的目标，以及没删干净时怎样返回，见下面的上游要求。
- `attrs_of(root: &Path, path: &RepoPath) -> Result<Attrs>`：上级逐级不跟随，最后一级是 lstat 语义；`Attrs.kind` 是 `FileKind`，符号链接带着目标的原始字节；`Attrs::last_changed()` 是 mtime 与 ctime 中较晚的那个。
- `list_dir(root: &Path, dir: Option<&RepoPath>) -> Walk<'static>`：列一个目录的直接子项，每项带 `Attrs`，不跟随符号链接。
- `full_fsync(file: &File) -> Result<()>`：`F_FULLFSYNC`。

下面五项由 apfs 的第 2 轮加上（module-rulings 的"clone_from_rooted states its source kind""make_removable""remove_tree_rooted reports, it does not fail""remove_tree_rooted takes any entry kind""Metadata changes through a handle go through apfs::set_file_meta"），是 store 对 apfs 的上游要求：

- `clone_from_rooted` 多一个参数 `kind: CloneKind`，`CloneKind { File, Dir }`：apfs `fstat` 钉住的描述符，种类与 `kind` 不符就在克隆之前拒绝。store 只传 `CloneKind::File`，靠的是：是目录 → `IsDirectory`，不再克隆整棵树；是 FIFO、socket 或设备 → `NotFile`（`fclonefileat` 对 FIFO 的描述符返回 `EINVAL`，所以这一条同样要在克隆之前判）。
- `make_removable(file: &File) -> Result<()>`：在调用方用 `open_rooted` 打开的句柄上，先清掉 `UF_IMMUTABLE` 与 `UF_APPEND`，再 `fchmod u+w`。`retire_dir` 与 `gc_repo` 准备根目录都用它（§5.5、§5.11），store 不再自己发这对 `fchflags`、`fchmod`；apfs 从不自己放宽任何东西（A10）。不是目录、又有别的名字（`st_nlink > 1`）的条目，它原样拒绝（`ReadOnly`，`reason = "hard_link"`）；store 只对目录调用它。
- `remove_tree_rooted` 的返回：动手之前的拒绝是 `Err`，什么都没动（`NoSuchPath`、`NotDirectory`、父目录不可写的 `ReadOnly`、目标是挂载点的 `CrossVolume`（`reason = "mount_point"`））；一旦开始遍历就返回 `Ok(RemoveReport)`，没删掉的条目带原因列在 `stuck` 里（不确定的 unlink 是 `unknown_outcome`），`#[must_use]`，`is_complete()` 说明删干净没有。没删干净的结果是确定的，store 把条目留到下一轮再删，不当成 `Unknown`（§5.11）。
- `remove_tree_rooted` 的目标可以是任何种类的条目（module-rulings 的"remove_tree_rooted takes any entry kind"）：目录照上面删整棵树；文件或符号链接（从不跟随）先清标志、再删它本身，返回同样的 `RemoveReport`。标志只在没有别的名字的 inode 上清（module-rulings 的"Flags are cleared only on an unshared inode"）：带着 `UF_IMMUTABLE` 或 `UF_APPEND`、又有别的名字（`st_nlink > 1`）的非目录原样留下，记进 `stuck`（`Immutable`，`detail = "hard_link"`）；`SF_*` 标志从不清，只报出来。store 的 gc 用这一个调用删 tmp 与 trash 的每个条目（§5.11）。
- `set_file_meta(file: &File, meta: &FileMeta) -> Result<()>`，`FileMeta { clear_flags: bool, mode: Option<u32>, mtime: Option<SystemTime> }`（module-rulings 的"Metadata changes through a handle go through apfs::set_file_meta"）：先 `fstat` 句柄：目录句柄接受，`st_nlink` 核对跳过目录（module-rulings「set_file_meta accepts a directory handle」）；别的非普通文件 → `NotFile`；普通文件要求 `st_nlink` 为 1（否则 `ReadOnly`，`reason = "hard_link"`），核对不过就什么都不改；然后依次清掉写回的文件不该带的用户标志（`UF_COMPRESSED` 保留；`SF_*` 从不清，挡住改动时 `ReadOnly`，`reason = "immutable"`）、设 mode、设 mtime（atime 不动）；第一处改动之后失败 → `Unknown`（`partial`）。句柄上的改动作用在 inode 上，有别的名字时会连带改到那个名字（§5.4 的实测），所以 store 规整刚克隆出的文件（get 出来的文件、put 的克隆体）都经它，自己不调 `fchflags`、`fchmod`、`futimens`。

外部 crate：

- `rusqlite`，开 `bundled` [我的决定]：SQLite 的版本与编译选项随 crate 固定，§4.2 的回读、STRICT 表与 JSON 函数都依赖它；不链接系统 libsqlite3，免得系统更新改变持久语义。
- 【引擎变更，2026-10-23 用户决定】元数据引擎已改为 Turso（crate `turso`，钉 `=0.9.0-pre.1`，原为 `=0.8.3`；SQLite 文件格式兼容，开多进程 WAL）；上一条的 rusqlite 已不再使用，本文其余写 SQLite、PRAGMA、`rusqlite::ffi` 的地方按 Turso 的实际行为读，实现差异与缺口见 docs/impl-progress/store.md「Engine: Turso」。
- 选这个引擎是为了日后的项目向量化：嵌入可以放进同一个 store.db，用 `vector32` / `vector_distance_cos` 查询（现在不建向量表）。
- `blake3`，开 `mmap` 与 `rayon`：算 `BlobId`；16 MiB 及以上的文件走 mmap 加多线程 [我的决定]。
- `libc`：`flock`、`open(O_CLOEXEC)`、`fcntl(F_BARRIERFSYNC)` 与 `fcntl(F_FULLFSYNC)`、`renamex_np(RENAME_EXCL)`（只用在 store 自己的 `tmp/` 到 `blobs/`、临时库到 `store.db`）、`flistxattr`/`fremovexattr`、`utimensat(AT_SYMLINK_NOFOLLOW)`（刷新已存 blob 的时间）、`arc4random_buf`（design §11 第 2 条：macOS API 走 libc）。
- `serde`、`serde_json`：JSON 列（§4.4）与 `GcReport`。
- 开发依赖：`tempfile`（测试根目录）。§7 的 VFS 计数与故障注入用 `rusqlite::ffi`，不另加 crate。

依赖方向：store 在第 1 层，只依赖第 0 层和 apfs，符合 types.md §8。

谁调用 store（逐个方法的调用方见 §3 末尾的表）：

| 调用方 | 用到的 |
| --- | --- |
| overlay | blob 的方法（按路径的 `get_blob` 除外）、`record_checkpoint`、`checkpoint`、`latest_checkpoint`、`session`（overlay.md @77e2e0c §2） |
| deps | `deps_dir`、`deps_staging_dir`、增补 4 定名的八个写方法与五个读方法 |
| session | `base_dir`、`record_base_gen`、`has_base_gen`、`manifest`、`retire_dir`（含第 7 行的基线暂存）；会话、工作区、种子、意图的写与读（§3 的"会话与工作区"一组）。投影写工作区经 overlay（增补 3"Projection"）。种子从主 checkout 克隆（session.md §5.3），携带经 `clone_from_rooted`（module-rulings 的"Carry is not a capture"），都不经 blob 存储，所以 session 不调 `put_blob_rooted` 与按路径的 `get_blob`；往会话树里写 blob 内容时用 `get_blob_rooted`（增补 8，session.md @853fdf8 里没有这样的地方） |
| publish | `record_publish`、`set_publish_phase`、`record_ref_change`、`ref_changes`、`publish_record`、checkpoint 的读、`read_blob` |
| daemon | `open`、`close`、`is_poisoned`、`quick_check`、`manifest`（组成 overlay 的 `Base`，overlay.md §2）、`gc`、`gc_repo`、`last_gc_report`、`open_publishes`，以及 status 要的读 |
| cli | daemon 不在时用 `open_read_only` 加读（design §5） |

search 不直接调用，它从 overlay 拿 `Overlay`；层次上允许，search.md 需要时可以读。

## 3. 公开接口（Rust）

跨模块类型全部来自 types.md。本节另外定义 store 自有的公开类型：会话一组 `Lifecycle`、`Health`、`SessionRecord`、`WorkspaceRecord`、`SeedRow`、`MoveIntent`、`MoveState`；依赖一组 `AttemptKind`、`AttemptState`、`DepsAttempt`、`DepsOrigin`、`DepsSetRecord`、`NewProposal`、`ProposalRecord`、`ProposalStatus`、`ProposalDecision`、`DepsMain`；以及 `CheckpointInfo`、`PublishRecord`、`PublishPhase`、`RefChange`、`RefOp`、`RefOutcome`、`GcReport`，和只在测试里出现的 `Seam`、`SyncCounts`。它们是按增补 4 镜像所有者词汇的行类型：deps 把它们映射成 deps.md §3 的 `Proposal`、`BuildAttempt` 等，session 映射成 session.md §3 的类型，store 从不反过来依赖那些类型。`Checkpoint` 用的是 Task 15 加进 types.md 的形状：多一个 `base: BaseGen` 字段，记下这个 checkpoint 对着哪个基线（增补裁定 T7/T6/T15）。

```rust
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use serde::{Deserialize, Serialize};
use strata_types::{
    Action, BaseGen, BaseManifest, BlobId, Checkpoint, CheckpointId, CheckpointReason, CommitOid,
    DepsSet, DepsSetId, ProposalId, ReleaseGroup, RepoName, RepoPath, SessionId, SessionManifest,
    StrataError, TreeOid,
};

/// 本模块所有函数的返回类型（types.md §8 的写法）。
pub type Result<T> = std::result::Result<T, StrataError>;

/// 一个打开的 Store。Send + Sync，daemon 用 Arc<Store> 在线程间共享。
/// 字段私有，列出来只为说明结构（计划里的 `Store { root, db }` 保留这两个名字）：
///   root: PathBuf                    [paths] root；Store 本体在 root/store/
///   db: Mutex<Connection>            L2：唯一的写连接（只读打开时是只读连接）
///   readers: Mutex<Vec<Connection>>  只读连接池；读从不取 L2
///   writer_lock: Option<File>        L1：root/store/LOCK 上的 flock，以 O_CLOEXEC 打开；只读打开时为 None
///   blob_gate: RwLock<()>            L3：put、get、read、verify、record_checkpoint 持共享锁；gc 的 blob 与 tmp 清扫持独占锁
///   gc_active: Arc<AtomicBool>       SQL 函数 strata_gc_active() 的返回值
///   poisoned: AtomicBool             提交结果不确定之后为 true，此后写一律拒绝
pub struct Store { /* 私有 */ }

impl Store {
    // ---- 打开 ----

    /// 以写者身份打开（只有 daemon 调用）。root 是 [paths] root，checkouts 是每张 [repo.<name>] 的 path。
    /// 第一次打开时建布局与 store.db（§5.1）；之后核对文件头、身份、PRAGMA 与 schema，
    /// 上次没有正常关闭（没有 clean-shutdown 标记）时再跑 quick_check（§5.2）。
    /// 同卷检查：每个 checkout 与 apfs::nearest_existing_ancestor(root) 各比一次 apfs::same_volume（config.md §4.3）。
    /// 参数比计划多一个 checkouts：增补 4 的 T13 一条定为 `Store::open(root, checkouts)`。
    /// 码：InvalidInput（root 不是绝对路径）、CrossVolume、NotEmpty、NoSuchPath（某个 checkout 不存在）、
    /// Busy（另一个写者持有 LOCK）、Corrupt（文件头、身份、版本、日志模式、schema 不对，丢了 store.db，
    /// SQLite 报库坏）、Unsupported（卷不支持 clonefile，apfs 透传；PRAGMA 回读不符）、Unknown（I/O）。
    pub fn open(root: &Path, checkouts: &[&Path]) -> Result<Store>;

    /// 以只读身份打开（daemon 不在时的 cli）。不取 LOCK，不改 store.db 的内容；之后每个写函数返回 ReadOnly。
    /// WAL 协议下 SQLite 可能建出缺失的 -wal 与 -shm 文件，这不改变库的内容（§5.2）。
    /// 码：NoSuchPath（还没有 Store）、Corrupt（同 open）、
    /// ReadOnly（WAL 需要恢复而只读连接做不了，detail.reason = "recovery_needed"）、Unknown。
    pub fn open_read_only(root: &Path) -> Result<Store>;

    /// [paths] root。
    pub fn root(&self) -> &Path;

    /// 是否以只读身份打开。
    pub fn is_read_only(&self) -> bool;

    /// 写者是否已中毒（§6.4）。daemon 把它并进 status；为 true 时只有丢掉这个 Store 重新 open 才能再写。
    pub fn is_poisoned(&self) -> bool;

    /// 正常关闭（daemon 有序退出时调用；它拿走 Store 本身，daemon 先停掉其它用它的线程）：等进行中的写事务结束，
    /// 关掉全部连接；写者没有中毒时写下
    /// root/store/clean-shutdown 标记并 F_FULLFSYNC；最后放开 LOCK（§5.2）。没有 close 就丢掉的 Store、
    /// 被杀的进程、中毒的写者都不留标记，下一次 open 因此会跑 quick_check。只读的 Store 只关连接。
    /// 码：Unknown（写标记失败；连接已经关了，下一次 open 照常跑 quick_check）。
    pub fn close(self) -> Result<()>;

    /// 按需的完整性检查：在一个只读连接上跑 PRAGMA quick_check，供 daemon 的校验命令用（命令名由 daemon.md 定）。
    /// 码：Corrupt（结果不是 ok，detail 带前十条消息）、Unknown。
    pub fn quick_check(&self) -> Result<()>;

    // ---- blob ----

    /// 把 src 此刻的内容存成 blob，返回它的 BlobId（§5.3）。不碰 SQLite：blob 的行由 record_checkpoint 登记。
    /// 只给 strata 自己的文件用（合并结果的临时文件、核对过的基线字节）：src 按路径解析，上级里的符号链接照常跟随，
    /// 所以工作区里的文件一律用 put_blob_rooted（增补 8）。
    /// 普通文件经 clonefile 进 tmp/；符号链接存它的目标文本（mode 120000 记在条目里，裁定 T7）。
    /// 克隆之后只看克隆体，所以返回的 id 就是存下的那份字节的 BLAKE3；调用方用这个返回值，不用自己事先算的哈希。
    /// 同一内容已存在时不新增文件、不回读已存副本、不做屏障，只核对长度并刷新它的时间（§5.3 第 7 步）。
    /// 新 blob 返回之前，数据和名字都已用 F_BARRIERFSYNC 排在之后任何写入之前。
    /// 码：ReadOnly；apfs 透传的 NoSuchPath、IsDirectory、NotFile、CrossVolume、Unsupported、ReadOnly、InvalidInput（名字过长）；
    /// Corrupt（已存同名 blob 不是普通文件或长度不对）、Unknown（I/O，带 detail.outcome）。
    pub fn put_blob(&self, src: &Path) -> Result<BlobId>;

    /// 从工作区捕获：把 src_root 之下 src 此刻的内容存成 blob（§5.3，增补 8）。overlay 的 checkpoint 用它；session 的携带
    /// 不是捕获，不经 blob 存储（module-rulings 的 "Carry is not a capture"）。
    /// src 的每一级都不跟随符号链接：扫描之后某一级目录被换成指向工作区之外的符号链接，捕获就失败，从不存下工作区之外的文件。
    /// 普通文件经 apfs::clone_from_rooted（CloneKind::File）克隆进 tmp/，再哈希克隆体；检查之后 src 被换成目录、特殊文件或符号链接，
    /// apfs 在克隆之前就拒绝。符号链接存它的目标文本（apfs::attrs_of 读出）；其余同 put_blob。
    /// src_root 要是规范路径（apfs 以 O_NOFOLLOW 打开它的最后一级）。
    /// 码：ReadOnly；apfs 透传的 NoSuchPath、NotDirectory（某一级上级不是目录或是符号链接，detail.reason = "symlink"）、IsDirectory、
    /// NotFile、CrossVolume、Unsupported、ReadOnly、InvalidInput（名字过长）；Corrupt、Unknown（同 put_blob）。
    pub fn put_blob_rooted(&self, src_root: &Path, src: &RepoPath) -> Result<BlobId>;

    /// 把一段字节存成 blob（三方合并的结果、符号链接的目标）。规则同 put_blob。
    /// 码：ReadOnly、Corrupt、Unknown。
    pub fn put_bytes(&self, bytes: &[u8]) -> Result<BlobId>;

    /// 把 blob 克隆成 dst。按文件工作（增补 4"Blob protection"）：blob 文件在就能取，不要求有登记行；
    /// 有登记行时核对长度。dst 不能存在；clonefile 要么建出完整的文件，要么不建。
    /// 受信任读：不重算哈希（裁定 T6）。产物像 git checkout 出来的文件：mode 0644（可执行位由调用方
    /// 按条目的 mode 设），没有拷来的 BSD 标志与扩展属性，mtime 为当前时刻。
    /// dst 按路径解析，上级里的符号链接照常跟随，所以只给 strata 自己的、不在会话树（<root>/sessions/）之下的目的地用；
    /// 写进工作区、会话根或 staging 树的一律用 get_blob_rooted（增补 8）。
    /// 符号链接条目不用它（得到的是普通文件），用 read_blob 取目标文本。
    /// 码：Corrupt（文件不在、不是普通文件、长度与登记行不符）、Exists（dst 已存在）、NoSuchPath（dst 的上级不存在）、
    /// NotDirectory（dst 的某一级上级不是目录）、ReadOnly（dst 所在目录不可写）、InvalidInput（名字过长）、CrossVolume（apfs 透传）、
    /// Unknown（dst 建出之后的步骤失败：detail.outcome = "partial"，dst 留在原处）。
    pub fn get_blob(&self, id: &BlobId, dst: &Path) -> Result<()>;

    /// 同 get_blob，但 dst 是 root 之下的 RepoPath（§5.4，增补 8）：dst 的上级逐级以 O_NOFOLLOW 打开、钉住，再只用最后一个名字
    /// 克隆（apfs::clone_file_rooted）；上级里有符号链接就在写任何东西之前拒绝，从不写到 root 之外。之后在 apfs::open_rooted_file
    /// 交来的句柄上经 apfs::set_file_meta 做 get_blob 的规整。overlay 的 restore 临时目录与投影临时名用它；往会话树里写 blob 内容的别的调用方也用它。
    /// root 要是规范路径（apfs 以 O_NOFOLLOW 打开它的最后一级）。
    /// 码：Corrupt（同 get_blob）、Exists（dst 已存在，含悬空符号链接）、NoSuchPath（dst 的上级不存在）、NotDirectory（dst 的某一级上级
    /// 不是目录或是符号链接，detail.reason = "symlink"；什么都没写）、ReadOnly、InvalidInput（名字过长）、CrossVolume（apfs 透传）、
    /// Unknown（克隆之后的步骤失败，含 dst 或它的上级在克隆之后被换掉、dst 被换成有别的名字的硬链接：detail.outcome = "partial"，
    /// 克隆出的文件留在原处，根之外什么都不改）。
    pub fn get_blob_rooted(&self, id: &BlobId, root: &Path, dst: &RepoPath) -> Result<()>;

    /// 读出 blob 的全部字节；超过 max 字节就不读。按文件工作、受信任读，同 get_blob。
    /// store 自己没有大文件阈值：max 就是调用方的阈值（64 MiB 的默认值在 config 与 search 一侧，§9）。
    /// 码：Corrupt、TooLarge（大小超过 max）、Unknown。
    pub fn read_blob(&self, id: &BlobId, max: u64) -> Result<Vec<u8>>;

    /// 校验读：重算 blob 文件的 BLAKE3 并与 id 比较；有登记行时也核对长度。
    /// restore --verify 与 gc 的完整性扫描用它，从不修改或删除任何东西。
    /// 码：Corrupt（不符、文件不在、不是普通文件、长度与登记行不符；detail 带期望与实际）、Unknown。
    pub fn verify_blob(&self, id: &BlobId) -> Result<()>;

    // ---- <root> 布局 ----

    /// 基线 generation 目录的路径 root/bases/<repo>/<tree>/<deps>（§4.1，增补 3），纯计算。
    pub fn base_dir(&self, gen: &BaseGen) -> PathBuf;

    /// 依赖集目录的路径 root/deps/<repo>/<id>（§4.1），纯计算。
    pub fn deps_dir(&self, repo: &RepoName, id: &DepsSetId) -> PathBuf;

    /// 一次依赖集尝试的暂存目录 root/deps/<repo>/.staging/<attempt>（§4.1），纯计算；attempt 是 begin_deps_attempt 的返回值。
    pub fn deps_staging_dir(&self, repo: &RepoName, attempt: u64) -> PathBuf;

    /// 把一个已经没有活记录的目录原子地改名进 root/store/trash/，返回它在 trash 里的路径；自己不删任何东西，
    /// 过了 1 小时由 gc 删除（§5.11）。这是目录离开活路径的唯一途径（增补 3）。只接受 §5.5 表里的七种路径，
    /// 前六种按行核对：已登记的 generation、活会话的根与影子仓库、当前代的管理目录、仍在交换中或已成现场的意图
    /// 与尝试点名的路径都不接受；第七种是基线暂存，没有行可以核对，只看形状。改名之前在 open_rooted 交来的句柄上
    /// 调 apfs::make_removable（清掉 UF_IMMUTABLE、UF_APPEND，再给属主补上写权限，增补 6）；
    /// 打开与改名都经 apfs（open_rooted，rename_rooted 的 RenameMode::Excl），每一级都不跟随符号链接（增补 8）。
    /// 码：ReadOnly（只读 Store；准备或改名得到 EPERM、EACCES、EROFS）、InvalidInput（路径不是这七种、含符号链接
    /// 或 . 与 ..、对应的行不允许）、NotDirectory（路径不是目录）、NoSuchPath、Unknown。
    pub fn retire_dir(&self, path: &Path) -> Result<PathBuf>;

    // ---- 基线 generation 与清单 ----

    /// 登记一个基线 generation 和它 tree 的清单，一个事务（增补 3、§5.5）。清单随 generation 一起登记，不另设 put_manifest
    /// [我的决定：增补 3 让 store 在两者之间选；一起登记，清单就不会没有 generation 而单独存在]。调用方是 session 的 ensure_base 与
    /// ensure_base_for：导出、依赖集的认领或冷装与清单都在 repo 写锁之外，只有登记那一段（把依赖集克隆进去、改名到
    /// base_dir(gen)、再调本方法）在该 repo 的写锁里；move_workspace 在锁里只找已建好的基线，从不建
    /// （module-rulings 的"Base generations are built outside the lock"）。
    /// 同一 (repo, tree) 的清单已登记时逐条比较：相同就不再写，不同就拒绝。generation 已登记时什么都不写。
    /// 码：ReadOnly、InvalidInput（manifest.tree 与 gen.tree 不同；依赖集没有登记；与已登记的清单不同）、Busy、Unknown。
    pub fn record_base_gen(&self, gen: &BaseGen, manifest: &BaseManifest, at: SystemTime) -> Result<()>;

    /// 码：Corrupt、Unknown。
    pub fn has_base_gen(&self, gen: &BaseGen) -> Result<bool>;

    /// 没有登记过返回 None；空 tree 返回 entries 为空的清单，两者能区分。码：Corrupt、Unknown。
    pub fn manifest(&self, repo: &RepoName, tree: &TreeOid) -> Result<Option<BaseManifest>>;

    // ---- 依赖集：deps 的写（名字由增补 4 定，语义在 deps.md，每个一个写事务）----

    /// 插入一条 Running 的尝试，返回 store 分配的尝试序号；暂存目录是 deps_staging_dir(repo, 序号)，同时记进行里。
    /// deps 在拿到该依赖集的建造锁、算出 set 之后调用它（T8 的 I3 裁定），所以 set 必填。
    /// 码：ReadOnly、Exists（同一 (repo, set) 已有 Running 的尝试，detail.attempt 是它的序号）、TooLarge、Busy、Unknown。
    pub fn begin_deps_attempt(&self, repo: &RepoName, set: &DepsSetId, kind: AttemptKind, at: SystemTime) -> Result<u64>;

    /// 记下冷装进程（进程组组长）的 pid。比较并交换：尝试仍是 Running 且还没有 pid 时写入并返回 true；
    /// 尝试已经结束返回 false。码：ReadOnly、InvalidInput（没有这个尝试；pid 不是正数；已经记过 pid）、Busy、Unknown。
    pub fn set_deps_attempt_pid(&self, attempt: u64, pid: i32) -> Result<bool>;

    /// 结束一个尝试：Running → Failed 或 Refused，error 以 JSON 存下。比较并交换：不是 Running 时返回 false，什么都不写。
    /// 码：ReadOnly、InvalidInput（没有这个尝试；to 不是 Failed 或 Refused）、Busy、Unknown。
    pub fn end_deps_attempt(&self, attempt: u64, to: AttemptState, error: &StrataError, at: SystemTime) -> Result<bool>;

    /// 登记一个已封存的依赖集并把它的尝试记为 Sealed，一个事务。origin 为 Empty 时 attempt 必须是 None、
    /// set.dirs 为空；Adopt、Build 时 attempt 必须是同一 (repo, set)、同一种类、仍为 Running 的尝试。
    /// 码：ReadOnly、Exists（这个依赖集已经登记过）、InvalidInput（origin 与 attempt 对不上）、Busy、Unknown。
    pub fn record_deps_set(&self, set: &DepsSet, origin: DepsOrigin, attempt: Option<u64>, at: SystemTime) -> Result<()>;

    /// 插入一条提议，并在同一个事务里取代它占掉的位置上原来的那条：有目标（tree 与 head）的新行是 Pending，
    /// 位置是"该 repo 的 Pending"；没有目标的新行是 AwaitingPublish，位置是"该 repo 该提出会话的 AwaitingPublish"。
    /// replaces 是调用方读到的、该位置上现有的那条（None 表示位置是空的）：与实际不符时什么都不写，返回 Ok(None)
    /// [我的决定：deps.md 原来在一个写事务里先读后写，换成具名方法之后，由这个参数守住读与写之间的变化]，
    /// 调用方重读之后再决定。符合时原来那条变成 Superseded（superseded_by = 新号），返回 Some(新号)；号由 store 分配。
    /// 码：ReadOnly、InvalidInput（from 与 to 相同；tree 与 head 不是同有同无；没有目标又没有提出会话）、TooLarge、Busy、Unknown。
    pub fn record_proposal(&self, new: &NewProposal, replaces: Option<ProposalId>, at: SystemTime) -> Result<Option<ProposalId>>;

    /// 提升：AwaitingPublish → Pending，填上 tree 与 head（比较并交换：不是 AwaitingPublish 时返回 false）。
    /// 同一个事务里，该 repo 现有的 Pending，以及该 repo 里 to 相同的其它 AwaitingPublish，都变成 Superseded（superseded_by = id）。
    /// 码：ReadOnly、InvalidInput（没有这条提议）、Busy、Unknown。
    pub fn promote_proposal(&self, id: ProposalId, tree: &TreeOid, head: &CommitOid, at: SystemTime) -> Result<bool>;

    /// 决定：Pending → Applied，或 Pending → Superseded { by }（比较并交换：不是 Pending 时返回 false）。
    /// 码：ReadOnly、InvalidInput（没有这条提议；by 不存在或就是它自己）、Busy、Unknown。
    pub fn decide_proposal(&self, id: ProposalId, to: ProposalDecision, at: SystemTime) -> Result<bool>;

    /// 比较并交换该 repo 主干当前的依赖集（增补 7"Main deps record"）。expected 是调用方读到的那一行
    /// （None 表示预期还没有行）：现有的行与它逐列相等（§5.6）时写入 row，返回 (true, 现有的行)；
    /// 不相等时什么都不写，返回 (false, 现有的行)，调用方重读之后再决定。row.set 可以还没有封存
    /// （observe_main 只记它算得出的，T8 裁定）。
    /// 码：ReadOnly、InvalidInput（row 与 expected 的 repo 不同）、Busy、Unknown。
    pub fn set_deps_main(&self, row: &DepsMain, expected: Option<&DepsMain>) -> Result<(bool, Option<DepsMain>)>;

    /// 没有登记过返回 None。码：Corrupt、Unknown。
    pub fn deps_set(&self, repo: &RepoName, id: &DepsSetId) -> Result<Option<DepsSetRecord>>;

    /// 全部尝试（任何状态），按序号递增；repo 为 None 时列出全部 repo。码：Corrupt、Unknown。
    pub fn deps_attempts(&self, repo: Option<&RepoName>) -> Result<Vec<DepsAttempt>>;

    /// 任何状态都返回；没有返回 None，由 deps 给出 NoSuchProposal。码：Corrupt、Unknown。
    pub fn proposal(&self, id: ProposalId) -> Result<Option<ProposalRecord>>;

    /// 按号递增。open_only 为 true 时只列 Pending，以及提出会话还在、而且不是 Removed 的 AwaitingPublish（增补 4）。
    /// 码：Corrupt、Unknown。
    pub fn proposals(&self, repo: Option<&RepoName>, open_only: bool) -> Result<Vec<ProposalRecord>>;

    /// 码：Corrupt、Unknown。
    pub fn deps_main(&self, repo: &RepoName) -> Result<Option<DepsMain>>;

    // ---- 会话与工作区：session 的写（每个一个写事务；含义在 session.md）----

    /// new 的第一次对外可见的写（增补 6）：一个事务写入会话行（Creating）和每个 WorkspaceState 一行工作区（代数 1，健康 Ok）。
    /// 码：ReadOnly、Exists（会话 id 已有行，任何生命周期；root 或某个工作区路径已被占用）、
    /// InvalidInput（repos 为空；root 不是 root/sessions/<id>；工作区路径不是 root/<repo>；某个基线没有登记或 repo 对不上）、Busy、Unknown。
    pub fn create_session(&self, manifest: &SessionManifest, at: SystemTime) -> Result<()>;

    /// 记下 session 正在做的阶段名（session 的词汇，store 不解释）。只在 Creating、Removing 时可写
    /// [我的决定：增补 4 只给意图行定了 phase；new 与 rm 也是多步操作，recover 要知道它们停在哪一步]。
    /// 码：ReadOnly、NoSuchSession、InvalidInput（生命周期不是 Creating 或 Removing）、Busy、Unknown。
    pub fn set_session_phase(&self, id: &SessionId, phase: &str) -> Result<()>;

    /// 比较并交换生命周期：当前是 from 时改成 to 并返回 true；当前不是 from 返回 false，什么都不写。
    /// 只允许增补 4 的六条边；进入 Unknown 时必须带 scene（现场的 JSON，形状归 session），其它转换不能带；
    /// 进入 Removed 时记下 at。Unknown → Removing 只给 rm --discard 用，这一条由 session 守（store 见不到凭据）。
    /// 码：ReadOnly、NoSuchSession、InvalidInput（不允许的转换；scene 与 to 不配）、Busy、Unknown。
    pub fn set_lifecycle(&self, id: &SessionId, from: Lifecycle, to: Lifecycle,
                         scene: Option<&serde_json::Value>, at: SystemTime) -> Result<bool>;

    /// 记下一个工作区的种子文件，每个路径一行，只能写一次；只在会话为 Creating 时可写 [我的决定：种子只在 new 时拷进来]。
    /// 码：ReadOnly、NoSuchSession、NoSuchRepo、InvalidInput（会话不是 Creating；路径重复或已经记过；路径不是 UTF-8）、Busy、Unknown。
    pub fn record_seeds(&self, id: &SessionId, repo: &RepoName, seeds: &[SeedRow]) -> Result<()>;

    // ---- dev 端口租约（§4.4 `dev_ports`，dev-ports 设计）[实现时补的决定]（2026-10-28） ----
    // DevPortLease { session, repo, base }：web = base、connector = base+1、connector_mcp = base+2、aux = base+3。
    // DEV_PORT_BLOCK = 4。DevPortsPolicy::probing(blocks) 由 daemon 按 config [dev_ports] 设进来；
    // 没设策略的句柄（cli 的只读 open）不分配：create_session / add_workspaces 不建租约，ensure / reassign → Unsupported(no_policy)。

    /// daemon 启动时设一次。
    pub fn set_dev_ports_policy(&self, policy: Option<DevPortsPolicy>);
    /// 全部租约（给了 session 就只要它的），按会话、repo 排序。库还是 v1（只读句柄，未迁移）时返回 None：每个工作区都算没租约。
    pub fn dev_port_leases(&self, session: Option<&SessionId>) -> Result<Option<Vec<DevPortLease>>>;
    /// Active 会话的工作区的租约；没有就分配一个（表之前建的会话）。幂等。
    /// 码：ReadOnly、Unsupported(no_policy)、NoSuchSession(not_active：creating / removing / removed)、
    /// InvalidInput(session_unknown：Unknown 的会话保留已有租约，但不发新的)、NoSuchRepo、Busy(ports_exhausted)、Unknown。
    pub fn ensure_dev_ports(&self, id: &SessionId, repo: &RepoName) -> Result<DevPortLease>;
    /// 把租约挪到另一个可用块（从旧块之后找起）；没有租约时等于 ensure。找不到时旧租约不动，Busy(ports_exhausted)。码同 ensure。
    pub fn reassign_dev_ports(&self, id: &SessionId, repo: &RepoName) -> Result<DevPortLease>;

    /// move_workspace 的意图行：写在建任何暂存之前，状态 Preparing，返回 store 分配的代数 n
    /// （该工作区当前代数与它所有意图里最大的 n 再加 1，永不复用）。staging 是 session 选的暂存路径，必须是
    /// root/sessions/<id>/<name> 这样会话根下的一级名字，而且不是任何工作区的目录名（§5.5 第 5 行的形状）。
    /// 码：ReadOnly、NoSuchSession（不存在或已 Removed）、NoSuchRepo、
    /// InvalidInput（会话不是 Active；工作区 tracked = false 或健康为 Unknown；to 没有登记或 repo 对不上；staging 不是上面的形状）、
    /// Exists（该工作区已有一条 Preparing 或 Swapping 的意图）、TooLarge（代数到了 i64::MAX）、Busy、Unknown。
    pub fn begin_move(&self, id: &SessionId, repo: &RepoName, to: &BaseGen, head: &CommitOid,
                      staging: &Path, at: SystemTime) -> Result<u64>;

    /// 记下意图正在做的阶段名（session 的词汇）。只对 Preparing、Swapping 的意图。
    /// 码：ReadOnly、InvalidInput（没有这条意图或它已结束）、Busy、Unknown。
    pub fn set_move_phase(&self, id: &SessionId, repo: &RepoName, n: u64, phase: &str) -> Result<()>;

    /// 意图的比较并交换，只用于 Preparing → Swapping（第一次对外可见的写之前，会话须为 Active）、
    /// Preparing → Refused（具名拒绝，暂存已拿走）、Preparing → Abandoned（recover 发现它没走到对外可见的写）。
    /// 进入 Swapping 时必须带 conflicts（rebase 算出的冲突路径），同一条比较并交换把它原样记在意图行上；别的边不能带
    /// （module-rulings 的"Move conflicts persist on the intent"，session.md 的 D46）[我的决定：给这条边加参数、不另开方法，同 set_lifecycle 的 scene]。
    /// 当前不是 from 返回 false。码：ReadOnly、InvalidInput（没有这条意图；不是这三条边；会话不是 Active；conflicts 与 to 不配）、
    /// Busy、Unknown。
    pub fn set_move_state(&self, id: &SessionId, repo: &RepoName, n: u64, from: MoveState, to: MoveState,
                          conflicts: Option<&[RepoPath]>, at: SystemTime) -> Result<bool>;

    /// 交换已经做完：一个事务里把工作区的 base（tree 与 deps 一起）、head、代数换成意图的 to、head、n，
    /// 记下冲突路径，意图 Swapping → Done（不变量 8）。conflicts 必须与意图进入 Swapping 时记下的那份相同
    /// （同样的路径、同样的顺序）[我的决定：保留这个参数，session 的调用不变；两份不同说明调用方拿错了 rebase 的结果]。
    /// 码：ReadOnly、InvalidInput（意图不是 Swapping；conflicts 与意图行上的不同，reason = "conflicts_mismatch"）、
    /// Corrupt（意图的目标基线没有登记，base_gen_missing）、Busy、Unknown。
    pub fn finish_move(&self, id: &SessionId, repo: &RepoName, n: u64, conflicts: &[RepoPath], at: SystemTime) -> Result<()>;

    /// 交换开始之后失败，或 recover 发现意图停在 Swapping：一个事务里意图 Swapping → Unknown，
    /// 工作区的健康 Ok → Unknown 并记下 scene（增补 4）。意图行上的冲突留着，给 resolve_move 的 Accept。
    /// 码：ReadOnly、InvalidInput（意图不是 Swapping）、Busy、Unknown。
    pub fn fail_move(&self, id: &SessionId, repo: &RepoName, n: u64, scene: &serde_json::Value, at: SystemTime) -> Result<()>;

    /// 人看过一次失败的移动之后，把它记回 Ok（module-rulings 的"Resolving a failed move"与"Move conflicts persist on the intent"；session.md §5.9）。
    /// 守卫：会话是 Active；工作区健康是 Unknown；n 是这个工作区 n 最大的意图，状态是 Unknown。一个事务里：
    /// accept = false（Abandon）时意图 Unknown → Abandoned，工作区的 base、head、代数与冲突不变；
    /// accept = true（Accept）时意图 Unknown → Done、old_tree 记成它的 staging，工作区的 base、head、代数换成意图的
    /// to、head、n，冲突换成意图进入 Swapping 时记下的那份。两种都把工作区健康 Unknown → Ok，现场留作历史；
    /// 意图的 ended 记成 at [我的决定]。store 只核对行，磁盘上的现场由 session 在调用之前核对。
    /// 码：ReadOnly、NoSuchSession（不存在或已 Removed）、NoSuchRepo、
    /// InvalidInput（守卫不过：reason 是 "lifecycle"、"not_failed" 或 "no_failed_move"，什么都不写）、
    /// Corrupt（Accept 时意图的目标基线没有登记，base_gen_missing）、Busy、Unknown。
    pub fn resolve_move(&self, id: &SessionId, repo: &RepoName, n: u64, accept: bool, at: SystemTime) -> Result<()>;

    /// 交换之后旧树被保留时记下它的位置（session.md 的 OldTreeKept）。只对 Done 的意图，只写一次。
    /// 码：ReadOnly、InvalidInput（意图不是 Done；已经记过；路径不是会话根下的一级名字，或就是某个工作区的目录）、Busy、Unknown。
    pub fn set_old_tree(&self, id: &SessionId, repo: &RepoName, n: u64, path: &Path) -> Result<()>;

    /// 码：Corrupt、Unknown。
    pub fn session(&self, id: &SessionId) -> Result<Option<SessionRecord>>;

    /// 全部会话，含 Removed 而尚未被 gc 回收的，按创建时间排序。码：Corrupt、Unknown。
    pub fn sessions(&self) -> Result<Vec<SessionRecord>>;

    /// 该会话的全部意图，按 (repo, n) 排序。码：Corrupt、Unknown。
    pub fn move_intents(&self, id: &SessionId) -> Result<Vec<MoveIntent>>;

    /// 全部 Preparing 与 Swapping 的意图，给 recover 用。码：Corrupt、Unknown。
    pub fn open_move_intents(&self) -> Result<Vec<MoveIntent>>;

    // ---- checkpoint ----

    /// 记一个 checkpoint（§5.8），返回 store 分配的 CheckpointId。
    /// 输入的 cp.id 不读 [我的决定]：序号在同一个写事务里分配为该会话已提交的最大值加 1，
    /// 写进行里并返回，调用方以返回值为准。cp.base 必须已登记且属于 cp.repo；cp.entries 引用的 blob 文件
    /// 必须已由 put_blob 或 put_bytes 存好，本方法在同一个事务里登记它们。reason 是 ToolCall { id }、而同一会话同一 repo
    /// 已经记过这个工具调用 id 时，什么都不写，返回已有的序号。checkpoint 行、全部 overlay 条目与 blob 登记
    /// 在一个事务里提交；提交即持久（synchronous = FULL）。
    /// 码：ReadOnly、NoSuchSession（不存在或已 Removed）、NoSuchRepo、
    /// InvalidInput（条目不一致，例如同一路径两条；工作区 tracked = false；基线没有登记或属于别的 repo）、
    /// Corrupt（引用的 blob 文件不在、不是普通文件或长度与登记行不符）、TooLarge（序号到了 i64::MAX）、
    /// Busy（外部进程持有 SQLite 写锁）、Unknown（I/O；detail.outcome 为 "not_written" 或 "uncertain"；写者已中毒）。
    pub fn record_checkpoint(&self, cp: Checkpoint) -> Result<CheckpointId>;

    /// 读回一个完整的 checkpoint：含 base 与全部条目，条目按 path 排序。读到的条目行数必须等于 checkpoints.entries，
    /// 否则是 Corrupt，从不交出缺了条目的快照（module-rulings 的"A Removed session takes no new pins"）。
    /// 码：NoSuchCheckpoint、Corrupt（读回的标识或序号不合法；条目行数不符，entries_mismatch）、Unknown。
    pub fn checkpoint(&self, session: &SessionId, id: CheckpointId) -> Result<Checkpoint>;

    /// 该会话、该 repo 序号最大的 checkpoint；一个都没有时返回 None。条目行数的核对同 checkpoint。码：Corrupt、Unknown。
    pub fn latest_checkpoint(&self, session: &SessionId, repo: &RepoName) -> Result<Option<Checkpoint>>;

    /// checkpoint 摘要（不含条目），按序号递增；repo 为 None 时列出全部 repo。码：Corrupt、Unknown。
    pub fn checkpoints(&self, session: &SessionId, repo: Option<&RepoName>) -> Result<Vec<CheckpointInfo>>;

    // ---- 发布与 ref 审计 ----

    /// 记一次发布的开始：写 publishes 行（phase = Running），每个批次的每个路径写一行 publish_items，
    /// 返回 store 分配的 ReleaseGroup.id（输入的 group.id 不读）。路径原样照抄，不展开改名。
    /// 批次点名的会话必须还在、不是 Removed（module-rulings 的"A Removed session takes no new pins"）。
    /// 码：ReadOnly、NoSuchSession（某个批次的会话不存在或已 Removed）、NoSuchCheckpoint（某个批次的 (session, checkpoint) 不存在）、
    /// InvalidInput（batches 为空或某个批次的 paths 为空）、TooLarge、Busy、Unknown。
    pub fn record_publish(&self, group: &ReleaseGroup, at: SystemTime) -> Result<u64>;

    /// 比较并交换发布的阶段：只允许 Running→Done、Running→Unknown、Unknown→Done [我的决定，与阶段词汇一起暂定]。
    /// outcome 是 publish.md 规定的 JSON 摘要。每次转换把 at 记为 ended。
    /// 码：ReadOnly、InvalidInput（不允许的转换）、Busy、Unknown。
    pub fn set_publish_phase(&self, id: u64, from: PublishPhase, to: PublishPhase,
                             outcome: Option<&serde_json::Value>, at: SystemTime) -> Result<bool>;

    /// 码：Corrupt、Unknown。
    pub fn publish_record(&self, id: u64) -> Result<Option<PublishRecord>>;

    /// 阶段为 Running 或 Unknown 的发布，供 daemon 重启时报出（从不重放）。码：Corrupt、Unknown。
    pub fn open_publishes(&self) -> Result<Vec<PublishRecord>>;

    /// 追加一条 ref 变动审计（发布链的比较并交换、推送）。UpdateRef 只接受 refs/strata/ 下的 ref
    /// （不变量 2，schema 也拦）。confirmed 只存核对过的 Action，从不存 ConfirmToken。
    /// 码：ReadOnly、InvalidInput（ref 名或 outcome 与 op 不配）、Busy、Unknown。
    pub fn record_ref_change(&self, change: &RefChange) -> Result<()>;

    /// 某次发布的全部 ref 变动，按时间顺序。码：Corrupt、Unknown。
    pub fn ref_changes(&self, publish: u64) -> Result<Vec<RefChange>>;

    // ---- 回收 ----

    /// 全局回收（§5.11）：删除过了保留期 keep 的会话行（含它们的工作区、种子、意图、checkpoint 与条目）、
    /// 已决定的提议、已结束的尝试、旧的 gc 记录；删除没有引用的 blob；清扫 tmp/ 与 trash/；再做一段完整性扫描，
    /// 把报告追加进 gc_runs 并返回。不碰任何 repo 的 generation 目录（那是 gc_repo）。
    /// 码：ReadOnly、Busy、Unknown（中途 I/O：detail.outcome = "partial"，已提交的批次有效，下一轮接着做）。
    pub fn gc(&self, keep: Duration) -> Result<GcReport>;

    /// 一个 repo 的 generation 回收（§5.11）：删除过了 keep、没有引用的基线 generation、基线清单与依赖集的行，
    /// 把它们的目录与活路径上没有登记的目录改名进 trash。改名之前先准备根目录（清掉 UF_IMMUTABLE、UF_APPEND，
    /// 给属主补上写权限，增补 6）；准备不成的 generation 连行一起留到下一轮，记进报告的 stuck。
    /// 调用方（daemon）必须持有该 repo 的写锁（增补 4）。
    /// 码：ReadOnly、Busy、Unknown（同 gc）。
    pub fn gc_repo(&self, repo: &RepoName, keep: Duration) -> Result<GcReport>;

    /// 最近一次 gc（scope 为 None）或某个 repo 的 gc_repo 的报告，含中途停下的那一轮。status 用它报出损坏的 blob。
    /// 码：Corrupt、Unknown。
    pub fn last_gc_report(&self, scope: Option<&RepoName>) -> Result<Option<GcReport>>;
}

/// 会话的生命周期（sessions.lifecycle）。词汇、转换图与含义归 session（增补 4）；store 镜像它，
/// 用触发器只放行这六条边：Creating→Active、Creating→Unknown、Active→Removing、Removing→Removed、
/// Removing→Unknown、Unknown→Removing。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lifecycle { Creating, Active, Removing, Removed, Unknown }

/// 工作区的健康（workspaces.health）：move_workspace 过了第一次对外可见的写却没做完时是 Unknown（增补 4）。
/// Missing 不存，它看文件系统。Ok → Unknown 经 fail_move；Unknown → Ok 只经 resolve_move，带着这个工作区 n 最大、
/// 状态为 Unknown 的意图（module-rulings 的"Resolving a failed move"）；没人处理的 Unknown 工作区随会话一起删除。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Health { Ok, Unknown }

/// 从 store 读回的一个会话。这些读回类型是纯数据，不派生 serde [我的决定]：JSON 形状由 session.md 与 cli.md 规定。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRecord {
    pub manifest: SessionManifest,
    pub lifecycle: Lifecycle,
    pub created: SystemTime,
    /// 进入 Removed 的时刻。
    pub removed: Option<SystemTime>,
    /// session 最近记下的阶段名。
    pub phase: Option<String>,
    /// 最近一次进入 Unknown 时记下的现场；之后离开 Unknown 也留着，作为历史 [我的决定]。
    pub scene: Option<serde_json::Value>,
    /// SessionManifest 之外的工作区列，键与 manifest.repos 相同。
    pub workspaces: BTreeMap<RepoName, WorkspaceRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceRecord {
    /// 工作区 git 管理目录的代数：new 时为 1，每次 finish_move 或 resolve_move 的 Accept 换成意图的 n。
    pub generation: u64,
    pub health: Health,
    /// 最近一次进入 Unknown 时记下的现场；resolve_move 记回 Ok 之后也留着，作为历史。
    pub scene: Option<serde_json::Value>,
    /// 最近一次 finish_move 或 resolve_move 的 Accept 记下的冲突路径。
    pub conflicts: Vec<RepoPath>,
    /// 种子文件，按 path 排序。
    pub seeds: Vec<SeedRow>,
}

/// 一个种子文件（seeds 表的一行），与 session.md 的 SeedRecord 一一对应。路径存 UTF-8 文本。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeedRow {
    pub path: RepoPath,
    pub source: PathBuf,
    pub link: Option<PathBuf>,
    pub size: u64,
    pub source_mtime: SystemTime,
    pub copied: SystemTime,
}

/// move_workspace 的意图（move_intents 表的一行）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MoveIntent {
    pub session: SessionId,
    pub repo: RepoName,
    /// 新的代数，也是新管理目录的名字。
    pub n: u64,
    pub to: BaseGen,
    pub head: CommitOid,
    pub staging: PathBuf,
    pub phase: Option<String>,
    pub state: MoveState,
    /// 进入 Swapping 时记下的冲突路径（session.md 的 D46）；Preparing、Refused，以及从 Preparing 收走的 Abandoned 是 None。
    pub conflicts: Option<Vec<RepoPath>>,
    pub old_tree: Option<PathBuf>,
    pub started: SystemTime,
    pub ended: Option<SystemTime>,
}

/// 意图的状态 [我的决定：增补 4 只定了"意图行在、关闭不删"，状态词汇由本文给出，session.md 采用或在它的 §2 列出要改的]。
/// Preparing：只建了内部暂存；Swapping：即将或已经做第一次对外可见的写；其余四个是终态：
/// Done（交换完成）、Refused（具名拒绝，暂存已拿走）、Abandoned（recover 收走的、没走到对外可见写的意图，
/// 或人对失败的移动选了 Abandon）、Unknown（交换开始之后失败，现场在工作区行上）。终态里只有 Unknown 还能再走一步：
/// 人看过现场之后经 resolve_move 变成 Done（Accept）或 Abandoned（Abandon），module-rulings 的"Resolving a failed move"。
/// 转换图见 §4.3。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MoveState { Preparing, Swapping, Done, Refused, Abandoned, Unknown }

/// 依赖集尝试的种类与状态。状态词汇归 deps（增补 4：running、sealed、failed、refused）；种类取自 deps.md §4.2。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AttemptKind { Adopt, Build }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AttemptState { Running, Sealed, Failed, Refused }

/// deps_attempts 表的一行。deps 把它映射成 deps.md 的 BuildAttempt。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DepsAttempt {
    pub id: u64,
    pub repo: RepoName,
    pub set: DepsSetId,
    pub kind: AttemptKind,
    pub state: AttemptState,
    /// deps_staging_dir(repo, id)。
    pub staging: PathBuf,
    pub pid: Option<i32>,
    pub started: SystemTime,
    pub ended: Option<SystemTime>,
    /// Failed、Refused 时的错误。
    pub error: Option<StrataError>,
}

/// 依赖集是怎么来的（取自 deps.md §4.2 的 origin 列）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DepsOrigin { Adopt, Build, Empty }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DepsSetRecord {
    pub set: DepsSet,
    pub origin: DepsOrigin,
    pub sealed: SystemTime,
}

/// record_proposal 的输入。tree 与 head 同有同无；有目标的是 Pending，没有的是 AwaitingPublish。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewProposal {
    pub repo: RepoName,
    /// 提出变更的会话；None 表示主干（deps.md 的 ProposalOrigin::Main）。
    pub origin: Option<SessionId>,
    pub from: DepsSetId,
    pub from_inputs: String,
    pub to: DepsSetId,
    pub to_inputs: String,
    pub tree: Option<TreeOid>,
    pub head: Option<CommitOid>,
}

/// deps_proposals 表的一行。deps 把它映射成 deps.md §3 的 Proposal。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProposalRecord {
    pub id: ProposalId,
    pub repo: RepoName,
    pub origin: Option<SessionId>,
    pub from: DepsSetId,
    pub from_inputs: String,
    pub to: DepsSetId,
    pub to_inputs: String,
    pub tree: Option<TreeOid>,
    pub head: Option<CommitOid>,
    pub status: ProposalStatus,
    pub superseded_by: Option<ProposalId>,
    pub created: SystemTime,
    /// 最近一次状态变化的时刻。
    pub updated: SystemTime,
}

/// 提议的状态（词汇与转换图归 deps，增补 4）：AwaitingPublish→Pending（提升）、AwaitingPublish→Superseded、
/// Pending→Applied、Pending→Superseded。每个 repo 至多一条 Pending，每个 (repo, 提出会话) 至多一条 AwaitingPublish。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProposalStatus { AwaitingPublish, Pending, Applied, Superseded }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProposalDecision { Applied, Superseded { by: ProposalId } }

/// deps_main 表的一行：某个 repo 主干当前的依赖集（observe_main 记下的）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DepsMain {
    pub repo: RepoName,
    pub tree: TreeOid,
    pub head: CommitOid,
    pub inputs_hash: String,
    /// 可以还没有封存。
    pub set: DepsSetId,
    pub observed: SystemTime,
}

/// checkpoint 摘要，给 status 与列表用。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckpointInfo {
    pub id: CheckpointId,
    pub repo: RepoName,
    pub base: BaseGen,
    pub at: SystemTime,
    pub reason: CheckpointReason,
    /// 条目数。
    pub entries: u64,
}

/// 一次发布的记录。group.id 是 store 分配的号，batches 与每批的 paths 保持记录时的顺序。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishRecord {
    pub group: ReleaseGroup,
    pub phase: PublishPhase,
    pub started: SystemTime,
    pub ended: Option<SystemTime>,
    pub outcome: Option<serde_json::Value>,
}

/// 发布的阶段。暂定（增补 4）：publish.md 采用它，或在它的 §2 列出要改的地方，T15 核对。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PublishPhase { Running, Done, Unknown }

/// refs_audit 的一行。每个 repo 的发布结果（新 commit、推送结果）都记在这里，不另设表。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefChange {
    pub at: SystemTime,
    pub repo: RepoName,
    pub op: RefOp,
    /// UpdateRef：本地 ref 的全名，必须以 refs/strata/ 开头；Push：远端 ref 的全名，如 refs/heads/main。
    pub ref_name: String,
    pub old: Option<CommitOid>,
    pub new: CommitOid,
    pub outcome: RefOutcome,
    pub publish: Option<u64>,
    /// 推送之前 daemon 核对过的确认动作；只存 Action，从不存 ConfirmToken（types.md §4）。
    pub confirmed: Option<Action>,
    /// 远端原话等，给人看。
    pub detail: Option<String>,
}

/// ref 审计的操作与结果 [我的决定]。UpdateRef 只取 Applied、HeadMoved；Push 只取 Pushed、Refused、Unknown，
/// 与 types.md 的 PushOutcome 三个变体一一对应。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RefOp { UpdateRef, Push }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RefOutcome { Applied, HeadMoved, Pushed, Refused, Unknown }

/// 一次 gc 或 gc_repo 的报告，以 JSON 存进 gc_runs.report，也返回给调用方。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GcReport {
    /// None：gc；Some(repo)：gc_repo。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<RepoName>,
    #[serde(with = "strata_types::serde_rfc3339")]
    pub started: SystemTime,
    #[serde(with = "strata_types::serde_rfc3339")]
    pub ended: SystemTime,
    pub keep_secs: u64,
    /// 下面是各类被删掉的行数。
    pub sessions: u64,
    pub checkpoints: u64,
    pub overlay_entries: u64,
    pub proposals: u64,
    pub attempts: u64,
    pub base_gens: u64,
    pub base_trees: u64,
    pub deps_sets: u64,
    pub blobs: u64,
    /// 被删的 blob 的逻辑字节数；CoW 之下实际释放的空间可能更少。
    pub blob_bytes: u64,
    /// 改名进 trash 的目录数。
    pub dirs_retired: u64,
    pub trash_removed: u64,
    pub tmp_removed: u64,
    /// 没删干净、留在原处的 trash 与 tmp 条目，以及根目录准备不成、这一轮没回收的 generation（§5.11）。
    pub stuck: Vec<PathBuf>,
    /// 本轮完整性扫描核对过的 blob 数。
    pub verified: u64,
    /// 扫描发现的损坏 blob（重算的 BLAKE3 不符、文件不在或长度不对）。从不删除，status 报出。
    pub corrupt: Vec<BlobId>,
    /// 这一轮中途停下时的错误（同一个错误也作为返回值）；完整跑完为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<StrataError>,
}

/// 测试接缝（增补 4"Test seams"）：cargo feature `test-seams`，只在测试里打开，发布构建里没有（§8）。
/// 要在两步之间做事的测试用停住类接缝，不用 errno 故障点。
#[cfg(feature = "test-seams")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seam {
    /// 下一次 put 在改名进 blobs/ 时返回这个 errno。
    BlobRenameFails { errno: i32 },
    /// 下一次 put 生成的第一个临时名视为已被占用。
    TempNameTaken,
    /// 下一次建库在临时库改名到 store.db 之前以 I/O 错误中止。
    InitBeforeRename,
    /// 下一次 record_checkpoint 在插入 checkpoint 行之后、插入条目之前以 I/O 错误失败。
    AfterCheckpointRow,
    /// 下一次 COMMIT 时，插桩的 VFS 让 WAL 的 xSync 返回 I/O 错误。
    CommitSyncFails,
    /// 停住：下一个写事务执行完全部语句之后、COMMIT 之前停住，直到 release。
    PauseBeforeCommit,
    /// 停住：下一次 put 建好临时克隆之后、改名进 blobs/ 之前停住，直到 release；这期间它一直持有 L3 共享锁。
    PauseInPut,
    /// 下一次 gc 或 gc_repo 在挪完第 n 个目录之后以 I/O 错误中止。
    GcStopAfterRetire { n: u64 },
    /// 下一次删 trash 条目时不调 apfs::remove_tree_rooted，当它返回了一份没删干净的报告（第一个文件以这个 errno 删不掉）：条目原样留着。
    GcUnlinkFails { errno: i32 },
    /// 之后 gc 的年龄下限改为这么多秒（缺省 3600）。
    AgeFloor { secs: u64 },
    /// 之后每轮 gc 的完整性扫描预算改为这么多字节。
    ScrubBudget { bytes: u64 },
    /// 下一次准备 generation 根目录（清标志、补属主写权限，§5.5、§5.11）时返回这个 errno。
    RetirePrepFails { errno: i32 },
    /// 之后 gc 每批删的行数上限改为这么多（缺省 10 000），用来造出跨批的情形。
    GcBatchRows { rows: u64 },
    /// 停住：下一次 gc 或 gc_repo 算完保留集（gc_repo 还已准备好根目录）之后、第一批删行之前停住，直到 release。
    GcPauseBeforeRows,
    /// 停住：下一次 gc 或 gc_repo 提交完第 n 批删行之后、开始下一批之前停住，直到 release。
    GcPauseAfterBatch { n: u64 },
}

/// 插桩 VFS 与 store 自己的同步计数（test-seams）。
#[cfg(feature = "test-seams")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SyncCounts {
    /// WAL 文件上标志为 SQLITE_SYNC_FULL 的 xSync 次数（fullfsync = 1 时即 F_FULLFSYNC）。
    pub wal_full: u64,
    /// WAL 文件上标志为 SQLITE_SYNC_NORMAL 的 xSync 次数，应恒为 0。
    pub wal_normal: u64,
    /// put 对 blob 文件做的 F_BARRIERFSYNC 次数。
    pub blob_barriers: u64,
    /// put 对扇出目录做的 F_BARRIERFSYNC 次数。
    pub dir_barriers: u64,
}

#[cfg(feature = "test-seams")]
impl Store {
    /// 装上一个接缝；故障点用一次就失效，AgeFloor 与 ScrubBudget 一直有效。
    pub fn arm(&self, seam: Seam);
    /// 阻塞到某个停住类接缝真的停住为止（由测试的另一个线程调用）。
    pub fn wait_paused(&self);
    /// 放行停住的那一步。
    pub fn release(&self);
    pub fn sync_counts(&self) -> SyncCounts;
}
```

每个方法的调用方（增补 4 要求点名 session 与 deps 用到的每个方法）：

| 方法 | 调用方 | 语义 |
| --- | --- | --- |
| `open`、`open_read_only`、`close` | daemon；cli（只读） | 打开 Store；正常关闭并留下 clean-shutdown 标记 |
| `is_poisoned` | daemon（status） | 写者是否中毒 |
| `quick_check` | daemon（校验命令） | 按需的 `PRAGMA quick_check` |
| `put_blob_rooted` | overlay（checkpoint） | 从工作区捕获 blob，每一级都不跟随符号链接（增补 8）；不碰 SQLite。session 的携带不是捕获，不经 blob 存储（module-rulings 的"Carry is not a capture"） |
| `put_blob`、`put_bytes` | overlay（rebase 的合并结果、核对过的基线字节） | 存 strata 自己的文件或字节，不碰 SQLite |
| `get_blob_rooted` | overlay（restore 取进会话根之下的临时目录 T，root 是会话根；投影取进 dst 正下方的临时名，root 是 dst；之后都经 `apfs::rename_rooted` 换到位置上，增补 3、7）；往会话树里写 blob 内容的别的调用方（session.md @853fdf8 里没有） | 按文件克隆出 blob，dst 的每一级上级都不跟随符号链接（增补 8） |
| `get_blob` | strata 自己的、不在会话树（`<root>/sessions/`）之下的目的地（增补 8）。目前各模块文档里没有这样的调用方：overlay 的 restore 与投影改用 `get_blob_rooted`，由 overlay 的修订轮落实；store 自己的测试用它 | 按文件克隆出 blob，dst 按路径解析 |
| `read_blob` | overlay（符号链接目标）、publish（三方合并的输入） | 读出 blob 字节，有上限 |
| `verify_blob` | overlay（`restore --verify`）、store 自己的 gc 扫描 | 重算哈希 |
| `base_dir`、`has_base_gen`、`record_base_gen` | session（`ensure_base`、`ensure_base_for` 在锁外建基线，只有登记持 repo 写锁；`move_workspace` 在锁里只找基线） | 基线目录的路径；登记基线与清单 |
| `manifest` | daemon（从 store 取清单组成 overlay 的 `Base`，overlay.md §2）、session | 读基线清单 |
| `deps_dir`、`deps_staging_dir` | deps | 依赖集与暂存的路径 |
| `begin_deps_attempt`、`set_deps_attempt_pid`、`end_deps_attempt`、`record_deps_set` | deps（认领、冷装、封存） | 尝试的意图行与封存登记 |
| `record_proposal`、`promote_proposal`、`decide_proposal` | deps（`propose`、`observe_main`、`record_rollout`、approve 的替换提议） | 提议的写 |
| `set_deps_main` | deps（`observe_main`：先读，再登记提议，最后比较并交换） | 主干当前的依赖集，比较并交换 |
| `deps_set`、`deps_attempts`、`proposal`、`proposals`、`deps_main` | deps；session 与 daemon 经 deps | 读 |
| `create_session`、`set_session_phase`、`set_lifecycle`、`record_seeds` | session（`new`、`remove`、`recover`） | 会话行（`new` 的第一次对外可见的写）、阶段、生命周期、种子 |
| `set_dev_ports_policy`、`ensure_dev_ports`、`reassign_dev_ports`；`dev_port_leases` | daemon（启动、`ports_ensure`、`ports_reassign`）；cli 读 | dev 端口租约（§4.4 `dev_ports`） |
| `begin_move`、`set_move_phase`、`set_move_state`、`finish_move`、`fail_move`、`set_old_tree` | session（`move_workspace`、`recover`） | 移动意图与交换结果 |
| `resolve_move` | session（`resolve_failed_move`，session.md §5.9） | 人选了 Abandon 或 Accept 之后，把失败的移动记回 Ok |
| `session`、`sessions`、`move_intents`、`open_move_intents` | session（`status`、`resolve`、`select`、`list`、`recover`）；overlay、deps 读会话 | 读 |
| `retire_dir` | session（会话根、影子仓库、暂存与旧树、旧管理目录、基线暂存）、deps（暂存，可选） | 把目录改名进 trash |
| `record_checkpoint`、`checkpoint`、`latest_checkpoint`、`checkpoints` | overlay；publish、session、daemon 读 | checkpoint |
| `record_publish`、`set_publish_phase`、`publish_record`、`open_publishes`、`record_ref_change`、`ref_changes` | publish；daemon（`open_publishes`） | 发布记录与 ref 审计 |
| `gc`、`gc_repo`、`last_gc_report` | daemon（`recover` 之后；`gc_repo` 持该 repo 的写锁） | 回收 |

与上一版相比，这一轮改名或新增、供 deps 与 session 的修订轮使用的方法：`record_base_gen` 改为连清单一起登记（删去 `record_manifest`）；`set_workspace_base` 由 `begin_move`、`set_move_phase`、`set_move_state`、`finish_move`、`fail_move`、`set_old_tree` 取代；`set_lifecycle` 多了 `scene`；新增 `set_session_phase`、`move_intents`、`open_move_intents`、`deps_staging_dir`、`is_poisoned`、`gc_repo`；`record_seeds` 改为按路径的行；deps 的写与读用增补 4 定的名字；`set_deps_main` 改为比较并交换，多一个 `expected`，返回是否写成和现有的行（增补 7）；新增 `put_blob_rooted`，工作区里的捕获都改用它（增补 8）；新增 `get_blob_rooted`，写进会话树的 blob 都改用它（增补 8"get_blob_rooted"）；`retire_dir` 多了第 7 行（基线暂存，module-rulings 的"Base generations are built outside the lock"）；新增 `resolve_move`，`set_move_state` 多了 `conflicts`，`MoveIntent` 多了 `conflicts`（module-rulings 的"Move conflicts persist on the intent"）。

接口上的三条通则：

- **store 分配的序号**：checkpoint、发布组、提议、依赖集尝试的序号与意图的代数由 store 在写事务里分配。输入结构里的序号字段（`Checkpoint.id`、`ReleaseGroup.id`）不读，调用方以返回值为准 [我的决定]。占位值随便填，例如 `CheckpointId::from(0)`。
- **读回不存在的东西**：types.md 把 `NoSuchCheckpoint` 记在 store 名下，所以 `checkpoint` 返回这个码；其余"没有"一律返回 `None` 或 `false`，由调用方给出自己的码（`NoSuchSession`、`NoSuchProposal`……）。写方法点名了不存在的尝试、提议或意图时返回 `InvalidInput`：这是调用方的错，types.md 没有对应的码。
- **线程**：所有方法都是 `&self`。写事务在 L2 里串行，读从连接池借独立的只读连接。比较并交换的方法返回 `false` 时什么都没写，调用方重读之后再决定；store 从不替调用方重试。

## 4. 数据与不变量

### 4.1 目录布局

```text
<root>/                                [paths] root（config.md），默认 /Volumes/GWD/inter/.strata
  store/
    store.db  store.db-wal  store.db-shm       SQLite（§4.2、§4.3）
    LOCK                                        L1 写者守卫：open 以 O_CLOEXEC 打开并持有它的 flock，进程退出即由内核释放
    clean-shutdown                              正常关闭的标记：close 写下，写者 open 时删掉；open 时不在，就说明上次没有正常关闭，跑 quick_check（§5.2）
    blobs/00/ … blobs/ff/                       256 个扇出目录，建库时一次建好
    blobs/<id 前两位>/<id>                      一个 blob：内容就是文件内容；mode 0444，没有扩展属性，没有 BSD 标志
    tmp/                                        store 自己的临时文件：put 的克隆体（put-<32 位十六进制>）、put_bytes 的临时文件、建库用的临时库
    trash/<unix 毫秒>-<32 位十六进制>/          retire_dir 与 gc 改名进来的目录，过了 1 小时由 gc 删除
  bases/<repo>/<tree>/<deps>/                   基线 generation 目录：源码导出加上匹配的依赖集克隆；session 建，record_base_gen 登记，gc_repo 回收
  bases/<repo>/.staging/<32 位十六进制>/        基线 generation 的暂存：session 在 repo 写锁之外建，登记时把其中的 tree/ 改名到 base_dir；retire_dir 第 7 行拿走，gc_repo 收走过了年龄下限的
  deps/<repo>/<deps>/                           依赖集目录：deps 建，record_deps_set 登记，gc_repo 回收
  deps/<repo>/.staging/<attempt>/               deps 的暂存，路径由 deps_staging_dir 给出；deps.md 的 .locks/ 也在 deps/<repo>/ 下
  sessions/<session-id>/<repo>/                 工作区：session 拥有，gc 从不进入
  sessions/.repos/<session-id>/<repo>.git/      会话的影子仓库（增补 2）：session 拥有，gc 从不进入
```

- `<repo>` 是 `RepoName`，`<tree>` 是 `TreeOid`，`<deps>` 是 `DepsSetId`（types.md §3）。三者的合法字符都不含 `/`，路径与键一一对应（design §7"键在路径里"）。
- 基线 generation 目录按 `BaseGen` 的三个键分目录：`base_dir(&BaseGen) = bases/<repo>/<tree>/<deps>/`，里面是源码导出加上匹配的依赖集克隆，`new` 一次 `clone_tree` 把整个目录克隆进工作区（增补 3"Base generation layout"）。同一个 tree 可以配不止一个依赖集（工具链升级、重新解析出的 `@inter/*` SHA 都会改变 `DepsSetId`），多一级 `<deps>` 才不会互相覆盖。
- 建目录的模块先在别处建好、封存，再在该 repo 的写锁里改名到 `base_dir`、`deps_dir` 给出的路径并登记（增补 4"gc guards"与"Locks"；基线 generation 在锁外建好，只有改名与登记在锁里，module-rulings 的"Base generations are built outside the lock"）。store 不规定暂存的名字和位置，只保证 gc 不碰进行中的暂存：有意图行或尝试行点名的受守卫 (a) 保护；没有行的基线暂存 `bases/<repo>/.staging/<32 位十六进制>`（名字由 session 定，`retire_dir` 第 7 行认它）受年龄下限保护，过了下限才被 `gc_repo` 挪进 trash（§5.11）。
- 基线文件不在 blob 存储里（增补 4"Base files"）：基线的内容就在基线目录里，restore 与投影从那里把没有覆盖的路径克隆回去（design §6.2）；`BaseManifest` 条目的 `BlobId` 是从基线目录读出的文件内容的 BLAKE3，store 只在 `manifest_entries` 里记 blob、mode 与大小，不保证 `blobs/` 里有这个文件；overlay 条目引用的 blob 一定在（§4.5 S3）。基线目录里的文件在 overlay 或 session 要用时不见了，是 `Corrupt`，从不当成"新文件"（3c:85，由 overlay.md、session.md 落实）。
- 数据库只有一个文件：一台机器一个 daemon，按 repo 分命名空间（design §5），各 repo 的行用 `repo` 列区分。

### 4.2 SQLite 设置

钉死的五项（裁定 T6）。每个写连接打开后设置，并逐项回读，对不上就拒绝打开：

| 设置 | 值 | 回读不符时 |
| --- | --- | --- |
| `journal_mode` | `WAL`；建库时设一次，持久在文件头里 | `Corrupt`：从不转换已有库的日志模式 |
| `synchronous` | `FULL`（回读为 2） | `Unsupported` |
| `fullfsync` | 1 | `Unsupported` |
| `checkpoint_fullfsync` | 1 | `Unsupported` |
| `foreign_keys` | 1 | `Unsupported` |

下面几项是 [我的决定]：

| 设置 | 值 | 理由 |
| --- | --- | --- |
| `recursive_triggers` | 1（写连接） | `INSERT OR REPLACE` 的隐式删除也会触发 §4.3 的删除守卫，REPLACE 绕不过"只有 gc 删除"（2026-10-22 用 SQLite 3.50.6、3.54.1 实测）；store 自己从不写 REPLACE：`set_deps_main` 在写事务里先读，再 `INSERT` 或 `UPDATE`（§5.6），blob 登记用 `INSERT … ON CONFLICT (id) DO NOTHING`（§5.8），都不删行 |
| `trusted_schema` | 0（每个连接） | 不让 schema 里的触发器调用没有声明无害的应用函数；`strata_gc_active` 注册时带 `SQLITE_INNOCUOUS`（不带 `SQLITE_DIRECTONLY`，否则触发器调不到它）。实测：没有这个标志的函数在 `trusted_schema = 0` 下被触发器调用，报 `unsafe use of strata_gc_active()` |
| `busy_timeout` | 写连接 0，读连接 2000 ms | 外部进程持锁时写立即失败、不重试（3a:620、3a:659）；读只在 WAL 恢复这种极短的独占期里等，普通写事务从不让读等 |
| `SQLITE_DBCONFIG_DEFENSIVE` | 每个连接都开 | 关掉 `writable_schema` 一类改 schema 的后门（沿用 LayerFS 的做法） |
| `query_only` | 1（读连接） | 读连接写不了 |
| `application_id` | 1398035009，即 0x53545241（"STRA"） | 建库时写入；在文件头偏移 68，open 靠它认出这是 Store |
| `user_version` | 2（`schema::LATEST`） | 建库时写入；在文件头偏移 60；v1 的库由写者 open 迁移到 2，规则见 §4.4a |

`wal_autocheckpoint` 保持 SQLite 的缺省值（1000 页），写者提交时自动做；读事务都很短，不会让 WAL 无限增长。

### 4.3 完整 DDL

下面是 v1 的 DDL（`schema::BASE_DDL`，冻结不改）。当前版本的 schema 是它加上 §4.4a 的迁移（`schema::ddl(v)`）；新库直接按 `ddl(LATEST)` 建，与迁移出来的库逐行相同（`new_and_migrated_stores_have_the_same_schema`）。

这是 git-strata 唯一的一份 schema（增补 4）：deps.md §4.2 原来的四张表在这里，按 T8 的裁定调整过（尝试行在拿锁、算出 set 之后才写，所以 `set_id` 必填；`ProposalId` 由 store 分配；AwaitingPublish → Pending 的提升允许；`deps_main` 多了 `inputs_hash`，它的 `set_id` 可以还没有封存）；session 要的会话、工作区、种子与移动意图也在这里。各表的词汇、转换图归所有者（会话生命周期归 session，提议与尝试的状态归 deps，发布阶段暂定，见 §3），触发器只是守住它们。

建库时在一个事务里按顺序执行（§5.1 第 8 步）。open 时把同一段文本在 `:memory:` 库里执行一遍，再与 store.db 的 `sqlite_schema` 逐行比对（type、name、tbl_name、sql），任何差别都是 `Corrupt`（§5.2 第 5 步）。触发器里的 `strata_gc_active()` 是 store 在每个写连接上注册的 SQL 函数，只在 gc 运行时返回 1；没有注册它的连接（例如人手开的 `sqlite3`）执行 DELETE 会因为找不到这个函数而失败，同样删不掉。2026-10-22 实测：这段 DDL 在 SQLite 3.54.1 与 3.50.6 上都建得出库（18 张表、42 个触发器），用一组 104 个正反断言逐条核对了下面的 CHECK、外键、部分唯一索引与触发器（转换图的每条边、不允许的边、代数单调、提议的延迟外键与显式号、没有级联时先删子行），以及 3.50.6 的 `sqlite3` 命令行删行时报 `no such function: strata_gc_active`。

```sql
PRAGMA application_id = 1398035009;
PRAGMA user_version = 1;

CREATE TABLE deps_sets (
    repo        TEXT    NOT NULL,
    id          TEXT    NOT NULL CHECK (length(id) = 64 AND id NOT GLOB '*[^0-9a-f]*'),
    inputs_hash TEXT    NOT NULL CHECK (length(inputs_hash) = 64 AND inputs_hash NOT GLOB '*[^0-9a-f]*'),
    platform    TEXT    NOT NULL,
    toolchain   TEXT    NOT NULL,
    dirs        TEXT    NOT NULL CHECK (json_valid(dirs) AND json_type(dirs) = 'array'),
    origin      TEXT    NOT NULL CHECK (origin IN ('adopt', 'build', 'empty')),
    sealed_ns   INTEGER NOT NULL,
    PRIMARY KEY (repo, id),
    CHECK (origin <> 'empty' OR json_array_length(dirs) = 0)
) STRICT;

CREATE TABLE deps_attempts (
    id         INTEGER PRIMARY KEY AUTOINCREMENT CHECK (id >= 1),
    repo       TEXT    NOT NULL,
    set_id     TEXT    NOT NULL CHECK (length(set_id) = 64 AND set_id NOT GLOB '*[^0-9a-f]*'),
    kind       TEXT    NOT NULL CHECK (kind IN ('adopt', 'build')),
    state      TEXT    NOT NULL CHECK (state IN ('running', 'sealed', 'failed', 'refused')),
    staging    TEXT    NOT NULL,
    pid        INTEGER CHECK (pid IS NULL OR pid > 0),
    started_ns INTEGER NOT NULL,
    ended_ns   INTEGER,
    error      TEXT    CHECK (error IS NULL OR json_valid(error)),
    CHECK ((state = 'running') = (ended_ns IS NULL)),
    CHECK ((state IN ('failed', 'refused')) = (error IS NOT NULL))
) STRICT;
CREATE UNIQUE INDEX deps_attempts_one_running ON deps_attempts (repo, set_id) WHERE state = 'running';
CREATE INDEX deps_attempts_by_set ON deps_attempts (repo, set_id, id);

CREATE TABLE deps_proposals (
    id             INTEGER PRIMARY KEY AUTOINCREMENT CHECK (id >= 1),
    repo           TEXT    NOT NULL,
    origin_session TEXT,
    from_set       TEXT    NOT NULL CHECK (length(from_set) = 64 AND from_set NOT GLOB '*[^0-9a-f]*'),
    from_inputs    TEXT    NOT NULL CHECK (length(from_inputs) = 64 AND from_inputs NOT GLOB '*[^0-9a-f]*'),
    to_set         TEXT    NOT NULL CHECK (length(to_set) = 64 AND to_set NOT GLOB '*[^0-9a-f]*'),
    to_inputs      TEXT    NOT NULL CHECK (length(to_inputs) = 64 AND to_inputs NOT GLOB '*[^0-9a-f]*'),
    tree           TEXT    CHECK (tree IS NULL OR (length(tree) = 40 AND tree NOT GLOB '*[^0-9a-f]*')),
    head           TEXT    CHECK (head IS NULL OR (length(head) = 40 AND head NOT GLOB '*[^0-9a-f]*')),
    state          TEXT    NOT NULL CHECK (state IN ('awaiting_publish', 'pending', 'applied', 'superseded')),
    superseded_by  INTEGER REFERENCES deps_proposals (id) DEFERRABLE INITIALLY DEFERRED,
    created_ns     INTEGER NOT NULL,
    updated_ns     INTEGER NOT NULL,
    CHECK (from_set <> to_set),
    CHECK ((tree IS NULL) = (head IS NULL)),
    CHECK (state <> 'awaiting_publish' OR tree IS NULL),
    CHECK (state NOT IN ('pending', 'applied') OR tree IS NOT NULL),
    CHECK ((state = 'superseded') = (superseded_by IS NOT NULL)),
    CHECK (superseded_by IS NULL OR superseded_by <> id),
    CHECK (origin_session IS NOT NULL OR tree IS NOT NULL)
) STRICT;
CREATE UNIQUE INDEX deps_proposals_one_pending ON deps_proposals (repo) WHERE state = 'pending';
CREATE UNIQUE INDEX deps_proposals_one_awaiting ON deps_proposals (repo, origin_session) WHERE state = 'awaiting_publish';
CREATE INDEX deps_proposals_by_superseded_by ON deps_proposals (superseded_by);
CREATE INDEX deps_proposals_by_origin ON deps_proposals (origin_session);

CREATE TABLE deps_main (
    repo        TEXT    NOT NULL PRIMARY KEY,
    tree        TEXT    NOT NULL CHECK (length(tree) = 40 AND tree NOT GLOB '*[^0-9a-f]*'),
    head        TEXT    NOT NULL CHECK (length(head) = 40 AND head NOT GLOB '*[^0-9a-f]*'),
    inputs_hash TEXT    NOT NULL CHECK (length(inputs_hash) = 64 AND inputs_hash NOT GLOB '*[^0-9a-f]*'),
    set_id      TEXT    NOT NULL CHECK (length(set_id) = 64 AND set_id NOT GLOB '*[^0-9a-f]*'),
    observed_ns INTEGER NOT NULL
) STRICT;

CREATE TABLE base_trees (
    repo       TEXT    NOT NULL,
    tree       TEXT    NOT NULL CHECK (length(tree) = 40 AND tree NOT GLOB '*[^0-9a-f]*'),
    files      INTEGER NOT NULL CHECK (files >= 0),
    created_ns INTEGER NOT NULL,
    PRIMARY KEY (repo, tree)
) STRICT;

CREATE TABLE manifest_entries (
    repo TEXT    NOT NULL,
    tree TEXT    NOT NULL,
    path TEXT    NOT NULL,
    blob TEXT    NOT NULL CHECK (length(blob) = 64 AND blob NOT GLOB '*[^0-9a-f]*'),
    mode INTEGER NOT NULL CHECK (mode IN (33188, 33261, 40960)),
    size INTEGER NOT NULL CHECK (size >= 0),
    PRIMARY KEY (repo, tree, path),
    FOREIGN KEY (repo, tree) REFERENCES base_trees (repo, tree)
) STRICT;

CREATE TABLE base_gens (
    id         INTEGER PRIMARY KEY CHECK (id >= 1),
    repo       TEXT    NOT NULL,
    tree       TEXT    NOT NULL,
    deps       TEXT    NOT NULL,
    created_ns INTEGER NOT NULL,
    UNIQUE (repo, tree, deps),
    UNIQUE (repo, id),
    FOREIGN KEY (repo, tree) REFERENCES base_trees (repo, tree),
    FOREIGN KEY (repo, deps) REFERENCES deps_sets (repo, id)
) STRICT;
CREATE INDEX base_gens_by_deps ON base_gens (repo, deps);
CREATE INDEX base_gens_by_age ON base_gens (repo, created_ns);

CREATE TABLE sessions (
    id         TEXT    NOT NULL PRIMARY KEY CHECK (id GLOB 's-[a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7]'),
    ticket     TEXT,
    root       TEXT    NOT NULL UNIQUE,
    lifecycle  TEXT    NOT NULL CHECK (lifecycle IN ('creating', 'active', 'removing', 'removed', 'unknown')),
    phase      TEXT,
    scene      TEXT    CHECK (scene IS NULL OR json_valid(scene)),
    created_ns INTEGER NOT NULL,
    removed_ns INTEGER,
    CHECK ((lifecycle = 'removed') = (removed_ns IS NOT NULL)),
    CHECK (lifecycle <> 'unknown' OR scene IS NOT NULL)
) STRICT;

CREATE TABLE workspaces (
    session    TEXT    NOT NULL REFERENCES sessions (id),
    repo       TEXT    NOT NULL,
    base_gen   INTEGER NOT NULL,
    head       TEXT    NOT NULL CHECK (length(head) = 40 AND head NOT GLOB '*[^0-9a-f]*'),
    path       TEXT    NOT NULL UNIQUE,
    tracked    INTEGER NOT NULL CHECK (tracked IN (0, 1)),
    generation INTEGER NOT NULL CHECK (generation >= 1),
    health     TEXT    NOT NULL CHECK (health IN ('ok', 'unknown')),
    scene      TEXT    CHECK (scene IS NULL OR json_valid(scene)),
    conflicts  TEXT    NOT NULL CHECK (json_valid(conflicts) AND json_type(conflicts) = 'array'),
    PRIMARY KEY (session, repo),
    FOREIGN KEY (repo, base_gen) REFERENCES base_gens (repo, id),
    CHECK (health <> 'unknown' OR scene IS NOT NULL)
) STRICT;
CREATE INDEX workspaces_by_base_gen ON workspaces (repo, base_gen);

CREATE TABLE seeds (
    session         TEXT    NOT NULL,
    repo            TEXT    NOT NULL,
    path            TEXT    NOT NULL,
    source          TEXT    NOT NULL,
    link            TEXT,
    size            INTEGER NOT NULL CHECK (size >= 0),
    source_mtime_ns INTEGER NOT NULL,
    copied_ns       INTEGER NOT NULL,
    PRIMARY KEY (session, repo, path),
    FOREIGN KEY (session, repo) REFERENCES workspaces (session, repo)
) STRICT;

CREATE TABLE move_intents (
    session    TEXT    NOT NULL,
    repo       TEXT    NOT NULL,
    n          INTEGER NOT NULL CHECK (n >= 2),
    to_tree    TEXT    NOT NULL CHECK (length(to_tree) = 40 AND to_tree NOT GLOB '*[^0-9a-f]*'),
    to_deps    TEXT    NOT NULL CHECK (length(to_deps) = 64 AND to_deps NOT GLOB '*[^0-9a-f]*'),
    head       TEXT    NOT NULL CHECK (length(head) = 40 AND head NOT GLOB '*[^0-9a-f]*'),
    staging    TEXT    NOT NULL,
    phase      TEXT,
    state      TEXT    NOT NULL CHECK (state IN ('preparing', 'swapping', 'done', 'refused', 'abandoned', 'unknown')),
    conflicts  TEXT    CHECK (conflicts IS NULL OR (json_valid(conflicts) AND json_type(conflicts) = 'array')),
    old_tree   TEXT,
    started_ns INTEGER NOT NULL,
    ended_ns   INTEGER,
    PRIMARY KEY (session, repo, n),
    FOREIGN KEY (session, repo) REFERENCES workspaces (session, repo),
    CHECK ((state IN ('preparing', 'swapping')) = (ended_ns IS NULL)),
    CHECK (old_tree IS NULL OR state = 'done'),
    CHECK (state = 'abandoned' OR (state IN ('preparing', 'refused')) = (conflicts IS NULL))
) STRICT;
CREATE UNIQUE INDEX move_intents_one_open ON move_intents (session, repo) WHERE state IN ('preparing', 'swapping');

CREATE TABLE checkpoints (
    session     TEXT    NOT NULL,
    id          INTEGER NOT NULL CHECK (id >= 1),
    repo        TEXT    NOT NULL,
    base_gen    INTEGER NOT NULL,
    at_ns       INTEGER NOT NULL,
    reason      TEXT    NOT NULL CHECK (reason IN ('tool_call', 'manual', 'before_publish', 'before_restore', 'before_rollout', 'before_remove')),
    reason_text TEXT,
    entries     INTEGER NOT NULL CHECK (entries >= 0),
    PRIMARY KEY (session, id),
    FOREIGN KEY (session, repo) REFERENCES workspaces (session, repo),
    FOREIGN KEY (repo, base_gen) REFERENCES base_gens (repo, id),
    CHECK ((reason IN ('tool_call', 'manual')) = (reason_text IS NOT NULL))
) STRICT;
CREATE UNIQUE INDEX checkpoints_by_tool_call ON checkpoints (session, repo, reason_text) WHERE reason = 'tool_call';
CREATE INDEX checkpoints_by_repo ON checkpoints (session, repo, id);
CREATE INDEX checkpoints_by_base_gen ON checkpoints (repo, base_gen);

CREATE TABLE blobs (
    id         TEXT    NOT NULL PRIMARY KEY CHECK (length(id) = 64 AND id NOT GLOB '*[^0-9a-f]*'),
    size       INTEGER NOT NULL CHECK (size >= 0),
    created_ns INTEGER NOT NULL
) STRICT;

CREATE TABLE overlay_entries (
    session    TEXT    NOT NULL,
    checkpoint INTEGER NOT NULL,
    path       TEXT    NOT NULL,
    kind       TEXT    NOT NULL CHECK (kind IN ('added', 'modified', 'deleted', 'renamed', 'untracked')),
    blob       TEXT    REFERENCES blobs (id),
    from_path  TEXT,
    mode       INTEGER NOT NULL,
    PRIMARY KEY (session, checkpoint, path),
    FOREIGN KEY (session, checkpoint) REFERENCES checkpoints (session, id),
    CHECK ((kind = 'deleted') = (blob IS NULL)),
    CHECK ((kind = 'renamed') = (from_path IS NOT NULL)),
    CHECK ((kind = 'deleted' AND mode = 0) OR (kind <> 'deleted' AND mode IN (33188, 33261, 40960)))
) STRICT;
CREATE UNIQUE INDEX overlay_entries_by_from ON overlay_entries (session, checkpoint, from_path) WHERE from_path IS NOT NULL;
CREATE INDEX overlay_entries_by_blob ON overlay_entries (blob);

CREATE TABLE publishes (
    id         INTEGER PRIMARY KEY CHECK (id >= 1),
    phase      TEXT    NOT NULL CHECK (phase IN ('running', 'done', 'unknown')),
    started_ns INTEGER NOT NULL,
    ended_ns   INTEGER,
    outcome    TEXT    CHECK (outcome IS NULL OR json_valid(outcome)),
    CHECK ((phase = 'running') = (ended_ns IS NULL))
) STRICT;

CREATE TABLE publish_items (
    publish    INTEGER NOT NULL REFERENCES publishes (id),
    batch      INTEGER NOT NULL CHECK (batch >= 0),
    item       INTEGER NOT NULL CHECK (item >= 0),
    session    TEXT    NOT NULL,
    repo       TEXT    NOT NULL,
    checkpoint INTEGER NOT NULL CHECK (checkpoint >= 1),
    path       TEXT    NOT NULL,
    PRIMARY KEY (publish, batch, item)
) STRICT;
CREATE INDEX publish_items_by_checkpoint ON publish_items (session, checkpoint);

CREATE TABLE refs_audit (
    id        INTEGER PRIMARY KEY CHECK (id >= 1),
    at_ns     INTEGER NOT NULL,
    repo      TEXT    NOT NULL,
    op        TEXT    NOT NULL CHECK (op IN ('update_ref', 'push')),
    ref_name  TEXT    NOT NULL,
    old_oid   TEXT    CHECK (old_oid IS NULL OR (length(old_oid) = 40 AND old_oid NOT GLOB '*[^0-9a-f]*')),
    new_oid   TEXT    NOT NULL CHECK (length(new_oid) = 40 AND new_oid NOT GLOB '*[^0-9a-f]*'),
    outcome   TEXT    NOT NULL CHECK (outcome IN ('applied', 'head_moved', 'pushed', 'refused', 'unknown')),
    publish   INTEGER REFERENCES publishes (id),
    confirmed TEXT    CHECK (confirmed IS NULL OR json_valid(confirmed)),
    detail    TEXT,
    CHECK (op <> 'update_ref' OR (ref_name GLOB 'refs/strata/*' AND outcome IN ('applied', 'head_moved'))),
    CHECK (op <> 'push' OR outcome IN ('pushed', 'refused', 'unknown'))
) STRICT;
CREATE INDEX refs_audit_by_publish ON refs_audit (publish);

CREATE TABLE gc_runs (
    id          INTEGER PRIMARY KEY CHECK (id >= 1),
    scope       TEXT,
    started_ns  INTEGER NOT NULL,
    ended_ns    INTEGER NOT NULL,
    outcome     TEXT    NOT NULL CHECK (outcome IN ('complete', 'partial')),
    report      TEXT    NOT NULL CHECK (json_valid(report)),
    scrub_after TEXT    CHECK (scrub_after IS NULL OR (length(scrub_after) = 64 AND scrub_after NOT GLOB '*[^0-9a-f]*')),
    CHECK (scope IS NULL OR scrub_after IS NULL)
) STRICT;
CREATE INDEX gc_runs_by_scope ON gc_runs (scope, id);

CREATE TRIGGER deps_sets_no_update        BEFORE UPDATE ON deps_sets        BEGIN SELECT RAISE(ABORT, 'immutable: deps_sets'); END;
CREATE TRIGGER base_trees_no_update       BEFORE UPDATE ON base_trees       BEGIN SELECT RAISE(ABORT, 'immutable: base_trees'); END;
CREATE TRIGGER manifest_entries_no_update BEFORE UPDATE ON manifest_entries BEGIN SELECT RAISE(ABORT, 'immutable: manifest_entries'); END;
CREATE TRIGGER base_gens_no_update        BEFORE UPDATE ON base_gens        BEGIN SELECT RAISE(ABORT, 'immutable: base_gens'); END;
CREATE TRIGGER seeds_no_update            BEFORE UPDATE ON seeds            BEGIN SELECT RAISE(ABORT, 'immutable: seeds'); END;
CREATE TRIGGER checkpoints_no_update      BEFORE UPDATE ON checkpoints      BEGIN SELECT RAISE(ABORT, 'immutable: checkpoints'); END;
CREATE TRIGGER blobs_no_update            BEFORE UPDATE ON blobs            BEGIN SELECT RAISE(ABORT, 'immutable: blobs'); END;
CREATE TRIGGER overlay_entries_no_update  BEFORE UPDATE ON overlay_entries  BEGIN SELECT RAISE(ABORT, 'immutable: overlay_entries'); END;
CREATE TRIGGER publish_items_no_update    BEFORE UPDATE ON publish_items    BEGIN SELECT RAISE(ABORT, 'immutable: publish_items'); END;
CREATE TRIGGER refs_audit_no_update       BEFORE UPDATE ON refs_audit       BEGIN SELECT RAISE(ABORT, 'immutable: refs_audit'); END;
CREATE TRIGGER gc_runs_no_update          BEFORE UPDATE ON gc_runs          BEGIN SELECT RAISE(ABORT, 'immutable: gc_runs'); END;

CREATE TRIGGER sessions_insert_guard BEFORE INSERT ON sessions
WHEN NEW.lifecycle <> 'creating' OR NEW.scene IS NOT NULL OR NEW.removed_ns IS NOT NULL
BEGIN SELECT RAISE(ABORT, 'illegal insert: sessions'); END;
CREATE TRIGGER sessions_update_guard BEFORE UPDATE ON sessions
WHEN NEW.id IS NOT OLD.id OR NEW.ticket IS NOT OLD.ticket OR NEW.root IS NOT OLD.root OR NEW.created_ns IS NOT OLD.created_ns
  OR (NEW.phase IS NOT OLD.phase AND NEW.lifecycle NOT IN ('creating', 'removing'))
  OR (NEW.scene IS NOT OLD.scene AND NOT (NEW.lifecycle = 'unknown' AND OLD.lifecycle <> 'unknown'))
  OR NOT ((NEW.lifecycle = OLD.lifecycle AND OLD.lifecycle IN ('creating', 'removing'))
       OR (OLD.lifecycle = 'creating' AND NEW.lifecycle IN ('active', 'unknown'))
       OR (OLD.lifecycle = 'active'   AND NEW.lifecycle = 'removing')
       OR (OLD.lifecycle = 'removing' AND NEW.lifecycle IN ('removed', 'unknown'))
       OR (OLD.lifecycle = 'unknown'  AND NEW.lifecycle = 'removing'))
BEGIN SELECT RAISE(ABORT, 'illegal update: sessions'); END;

CREATE TRIGGER workspaces_insert_guard BEFORE INSERT ON workspaces
WHEN NEW.generation <> 1 OR NEW.health <> 'ok' OR NEW.scene IS NOT NULL OR json_array_length(NEW.conflicts) <> 0
BEGIN SELECT RAISE(ABORT, 'illegal insert: workspaces'); END;
CREATE TRIGGER workspaces_update_guard BEFORE UPDATE ON workspaces
WHEN NEW.session IS NOT OLD.session OR NEW.repo IS NOT OLD.repo OR NEW.path IS NOT OLD.path OR NEW.tracked IS NOT OLD.tracked
  OR ((NEW.base_gen IS NOT OLD.base_gen OR NEW.head IS NOT OLD.head OR NEW.generation IS NOT OLD.generation OR NEW.conflicts IS NOT OLD.conflicts)
      AND NOT (NEW.generation > OLD.generation AND OLD.tracked = 1 AND NEW.health = 'ok'
               AND NEW.generation IS (SELECT MAX(n) FROM move_intents WHERE session = NEW.session AND repo = NEW.repo)))
  OR (NEW.health IS NOT OLD.health
      AND NOT (OLD.health = 'ok' AND NEW.health = 'unknown')
      AND NOT (OLD.health = 'unknown' AND NEW.health = 'ok'
               AND (SELECT lifecycle FROM sessions WHERE id = NEW.session) IS 'active'
               AND (SELECT state FROM move_intents WHERE session = NEW.session AND repo = NEW.repo ORDER BY n DESC LIMIT 1) IS 'unknown'))
  OR (NEW.scene IS NOT OLD.scene AND NOT (OLD.health = 'ok' AND NEW.health = 'unknown'))
BEGIN SELECT RAISE(ABORT, 'illegal update: workspaces'); END;

CREATE TRIGGER move_intents_insert_guard BEFORE INSERT ON move_intents
WHEN NEW.state <> 'preparing' OR NEW.old_tree IS NOT NULL OR NEW.ended_ns IS NOT NULL
  OR NEW.n <= (SELECT generation FROM workspaces WHERE session = NEW.session AND repo = NEW.repo)
  OR NEW.n <= (SELECT COALESCE(MAX(n), 0) FROM move_intents WHERE session = NEW.session AND repo = NEW.repo)
BEGIN SELECT RAISE(ABORT, 'illegal insert: move_intents'); END;
CREATE TRIGGER move_intents_update_guard BEFORE UPDATE ON move_intents
WHEN NEW.session IS NOT OLD.session OR NEW.repo IS NOT OLD.repo OR NEW.n IS NOT OLD.n OR NEW.to_tree IS NOT OLD.to_tree
  OR NEW.to_deps IS NOT OLD.to_deps OR NEW.head IS NOT OLD.head OR NEW.staging IS NOT OLD.staging OR NEW.started_ns IS NOT OLD.started_ns
  OR (NEW.phase IS NOT OLD.phase AND OLD.state NOT IN ('preparing', 'swapping'))
  OR (NEW.conflicts IS NOT OLD.conflicts AND NOT (OLD.state = 'preparing' AND NEW.state = 'swapping'))
  OR (NEW.old_tree IS NOT OLD.old_tree AND NOT (OLD.old_tree IS NULL AND OLD.state IN ('done', 'unknown') AND NEW.state = 'done'))
  OR NOT ((NEW.state = OLD.state AND OLD.state IN ('preparing', 'swapping'))
       OR (NEW.state = OLD.state AND OLD.state = 'done' AND NEW.ended_ns IS OLD.ended_ns)
       OR (OLD.state = 'preparing' AND NEW.state IN ('swapping', 'refused', 'abandoned'))
       OR (OLD.state = 'swapping'  AND NEW.state IN ('done', 'unknown'))
       OR (OLD.state = 'unknown'   AND NEW.state IN ('done', 'abandoned')
           AND NEW.n IS (SELECT MAX(n) FROM move_intents WHERE session = NEW.session AND repo = NEW.repo)
           AND (SELECT lifecycle FROM sessions WHERE id = NEW.session) IS 'active'
           AND EXISTS (SELECT 1 FROM workspaces
                       WHERE session = NEW.session AND repo = NEW.repo AND health = 'ok'
                         AND (generation = NEW.n) = (NEW.state = 'done'))))
BEGIN SELECT RAISE(ABORT, 'illegal update: move_intents'); END;

CREATE TRIGGER deps_attempts_insert_guard BEFORE INSERT ON deps_attempts
WHEN NEW.state <> 'running' OR NEW.pid IS NOT NULL OR NEW.error IS NOT NULL
BEGIN SELECT RAISE(ABORT, 'illegal insert: deps_attempts'); END;
CREATE TRIGGER deps_attempts_update_guard BEFORE UPDATE ON deps_attempts
WHEN NEW.id IS NOT OLD.id OR NEW.repo IS NOT OLD.repo OR NEW.set_id IS NOT OLD.set_id OR NEW.kind IS NOT OLD.kind
  OR NEW.staging IS NOT OLD.staging OR NEW.started_ns IS NOT OLD.started_ns
  OR (NEW.pid IS NOT OLD.pid AND NOT (OLD.pid IS NULL AND NEW.state = 'running'))
  OR OLD.state <> 'running'
BEGIN SELECT RAISE(ABORT, 'illegal update: deps_attempts'); END;

CREATE TRIGGER deps_proposals_insert_guard BEFORE INSERT ON deps_proposals
WHEN NEW.state NOT IN ('awaiting_publish', 'pending') OR NEW.superseded_by IS NOT NULL
BEGIN SELECT RAISE(ABORT, 'illegal insert: deps_proposals'); END;
CREATE TRIGGER deps_proposals_update_guard BEFORE UPDATE ON deps_proposals
WHEN NEW.id IS NOT OLD.id OR NEW.repo IS NOT OLD.repo OR NEW.origin_session IS NOT OLD.origin_session
  OR NEW.from_set IS NOT OLD.from_set OR NEW.from_inputs IS NOT OLD.from_inputs OR NEW.to_set IS NOT OLD.to_set
  OR NEW.to_inputs IS NOT OLD.to_inputs OR NEW.created_ns IS NOT OLD.created_ns
  OR ((NEW.tree IS NOT OLD.tree OR NEW.head IS NOT OLD.head) AND NOT (OLD.state = 'awaiting_publish' AND NEW.state = 'pending'))
  OR NOT ((OLD.state = 'awaiting_publish' AND NEW.state IN ('pending', 'superseded'))
       OR (OLD.state = 'pending' AND NEW.state IN ('applied', 'superseded')))
BEGIN SELECT RAISE(ABORT, 'illegal update: deps_proposals'); END;

CREATE TRIGGER deps_main_update_guard BEFORE UPDATE ON deps_main
WHEN NEW.repo IS NOT OLD.repo
BEGIN SELECT RAISE(ABORT, 'illegal update: deps_main'); END;

CREATE TRIGGER publishes_insert_guard BEFORE INSERT ON publishes
WHEN NEW.phase <> 'running'
BEGIN SELECT RAISE(ABORT, 'illegal insert: publishes'); END;
CREATE TRIGGER publishes_update_guard BEFORE UPDATE ON publishes
WHEN NEW.id IS NOT OLD.id OR NEW.started_ns IS NOT OLD.started_ns
  OR NOT ((OLD.phase = 'running' AND NEW.phase IN ('done', 'unknown')) OR (OLD.phase = 'unknown' AND NEW.phase = 'done')
       OR (OLD.phase IN ('unknown', 'done') AND NEW.phase = OLD.phase AND NEW.ended_ns IS OLD.ended_ns
           AND NEW.outcome IS NOT OLD.outcome))
BEGIN SELECT RAISE(ABORT, 'illegal update: publishes'); END;

CREATE TRIGGER deps_sets_delete_gc_only        BEFORE DELETE ON deps_sets        WHEN NOT strata_gc_active() BEGIN SELECT RAISE(ABORT, 'delete outside gc: deps_sets'); END;
CREATE TRIGGER deps_attempts_delete_gc_only    BEFORE DELETE ON deps_attempts    WHEN NOT strata_gc_active() BEGIN SELECT RAISE(ABORT, 'delete outside gc: deps_attempts'); END;
CREATE TRIGGER deps_proposals_delete_gc_only   BEFORE DELETE ON deps_proposals   WHEN NOT strata_gc_active() BEGIN SELECT RAISE(ABORT, 'delete outside gc: deps_proposals'); END;
CREATE TRIGGER deps_main_delete_gc_only        BEFORE DELETE ON deps_main        WHEN NOT strata_gc_active() BEGIN SELECT RAISE(ABORT, 'delete outside gc: deps_main'); END;
CREATE TRIGGER base_trees_delete_gc_only       BEFORE DELETE ON base_trees       WHEN NOT strata_gc_active() BEGIN SELECT RAISE(ABORT, 'delete outside gc: base_trees'); END;
CREATE TRIGGER manifest_entries_delete_gc_only BEFORE DELETE ON manifest_entries WHEN NOT strata_gc_active() BEGIN SELECT RAISE(ABORT, 'delete outside gc: manifest_entries'); END;
CREATE TRIGGER base_gens_delete_gc_only        BEFORE DELETE ON base_gens        WHEN NOT strata_gc_active() BEGIN SELECT RAISE(ABORT, 'delete outside gc: base_gens'); END;
CREATE TRIGGER sessions_delete_gc_only         BEFORE DELETE ON sessions         WHEN NOT strata_gc_active() BEGIN SELECT RAISE(ABORT, 'delete outside gc: sessions'); END;
CREATE TRIGGER workspaces_delete_gc_only       BEFORE DELETE ON workspaces       WHEN NOT strata_gc_active() BEGIN SELECT RAISE(ABORT, 'delete outside gc: workspaces'); END;
CREATE TRIGGER seeds_delete_gc_only            BEFORE DELETE ON seeds            WHEN NOT strata_gc_active() BEGIN SELECT RAISE(ABORT, 'delete outside gc: seeds'); END;
CREATE TRIGGER move_intents_delete_gc_only     BEFORE DELETE ON move_intents     WHEN NOT strata_gc_active() BEGIN SELECT RAISE(ABORT, 'delete outside gc: move_intents'); END;
CREATE TRIGGER checkpoints_delete_gc_only      BEFORE DELETE ON checkpoints      WHEN NOT strata_gc_active() BEGIN SELECT RAISE(ABORT, 'delete outside gc: checkpoints'); END;
CREATE TRIGGER blobs_delete_gc_only            BEFORE DELETE ON blobs            WHEN NOT strata_gc_active() BEGIN SELECT RAISE(ABORT, 'delete outside gc: blobs'); END;
CREATE TRIGGER overlay_entries_delete_gc_only  BEFORE DELETE ON overlay_entries  WHEN NOT strata_gc_active() BEGIN SELECT RAISE(ABORT, 'delete outside gc: overlay_entries'); END;
CREATE TRIGGER gc_runs_delete_gc_only          BEFORE DELETE ON gc_runs          WHEN NOT strata_gc_active() BEGIN SELECT RAISE(ABORT, 'delete outside gc: gc_runs'); END;
CREATE TRIGGER publishes_no_delete     BEFORE DELETE ON publishes     BEGIN SELECT RAISE(ABORT, 'append-only: publishes'); END;
CREATE TRIGGER publish_items_no_delete BEFORE DELETE ON publish_items BEGIN SELECT RAISE(ABORT, 'append-only: publish_items'); END;
CREATE TRIGGER refs_audit_no_delete    BEFORE DELETE ON refs_audit    BEGIN SELECT RAISE(ABORT, 'append-only: refs_audit'); END;
```

表的说明：

- **没有 `ON DELETE CASCADE`**：外键都是默认的 NO ACTION，父行还有子行就删不掉。gc 因此总是先删子行、分批删（§5.11），一条 DELETE 不会悄悄级联出几百万行，也不会超过每批的上限。
- **复合外键保证 repo 一致**：`workspaces` 与 `checkpoints` 用 `(repo, base_gen)` 引用 `base_gens (repo, id)`，所以一个工作区或 checkpoint 钉住的基线一定属于同一个 repo；store 在写之前也先核对，给出 `InvalidInput` [我的决定：两者都做，外键兜底，核对给出能区分的码]。
- **意图与尝试只记键，不引用基线** [我的决定]：`move_intents` 用 `to_tree`、`to_deps` 两列记目标，不是外键。结束了的意图是历史，不该把基线钉到会话被回收为止；还在进行或成了现场的意图钉住目标，由 gc 的保留集负责（§5.11）。
- **提议的 `superseded_by` 是延迟外键**：`record_proposal` 先把位置上的旧行改成 Superseded（指向新号），再以这个显式的号插入新行，外键在 COMMIT 时检查。新号取 `sqlite_sequence` 里的值加 1（没有这一行时取 1），这是 store 内部的事，deps 从不读 `sqlite_sequence`（增补 4）。以显式的号插入之后，`AUTOINCREMENT` 照样把 `sqlite_sequence` 推到这个号，被 gc 删掉的号不会再发出去（实测）。
- **两张部分唯一索引守住提议的位置**：`deps_proposals_one_pending`（每个 repo 至多一条 Pending）与 `deps_proposals_one_awaiting`（每个 (repo, 提出会话) 至多一条 AwaitingPublish）。`deps_attempts_one_running` 让同一依赖集同时至多一条 Running 的尝试；`move_intents_one_open` 让一个工作区同时至多一条没有结束的意图。
- **插入守卫**规定每种行的初始状态：会话从 Creating 开始，工作区从代数 1、健康 Ok、没有冲突开始，意图从 Preparing 开始且 `n` 必须大于该工作区的当前代数和它所有意图里最大的 `n`，尝试从 Running 开始，提议从 AwaitingPublish 或 Pending 开始，发布从 Running 开始。
- **更新守卫**就是各个转换图：
  - 会话：`creating → active | unknown`、`active → removing`、`removing → removed | unknown`、`unknown → removing`（增补 4）；`phase` 只在 Creating、Removing 时可改；`scene` 只在进入 Unknown 时改写；`removed_ns` 由 CHECK 绑在 Removed 上。
  - 工作区：base、head、代数、冲突列表只能一起变，代数必须变大、等于这个工作区 n 最大的意图的 n，只对 tracked 工作区，变完健康必须是 Ok（`finish_move`，或者 `resolve_move` 的 Accept 从 Unknown 回来）；`ok → unknown` 时同时写入现场；`unknown → ok` 只在会话是 Active、这个工作区 n 最大的意图还是 Unknown 时放行，所以 `resolve_move` 先改工作区、再改意图；现场只在进入 Unknown 时改写，回到 Ok 之后留作历史。
  - 意图：`preparing → swapping | refused | abandoned`、`swapping → done | unknown`、`unknown → done | abandoned`（只给 `resolve_move`：它是这个工作区 n 最大的意图，会话是 Active，工作区已经回到 Ok；Done 时工作区的代数已是 n，Abandoned 时不是）；`phase` 只在没结束时可改；`conflicts` 只在 `preparing → swapping` 时写一次（CHECK 另外要求 Swapping、Done、Unknown 有冲突，Preparing、Refused 没有）；`old_tree` 只能在 Done 上、或在 `unknown → done` 时从空写成值一次。
  - 尝试：`running → sealed | failed | refused`；`pid` 只能在 Running 时从空写成值一次；Failed、Refused 必须带错误。
  - 提议：`awaiting_publish → pending | superseded`、`pending → applied | superseded`；`tree` 与 `head` 只在提升时写入；其余列不可改。
  - 发布：`running → done | unknown`、`unknown → done`（暂定，见 §3）。
  - 其余表写入后整行不可改；`deps_main` 只有 `repo` 不可改，整行由 `set_deps_main` 比较并交换（一个写事务里先读、逐列比较、再写，§5.6）。
- **删除守卫**：除 `publishes`、`publish_items`、`refs_audit` 外，每张表的 DELETE 都要 `strata_gc_active()` 为 1；这三张表连 gc 也删不掉（§4.5 S2）。

### 4.4 列与序号的约定

- **时间**：`*_ns` 列是相对 Unix 纪元的纳秒数（UTC），有符号 INTEGER [我的决定]：早于纪元的时刻（例如种子文件的源文件 mtime）存成负数。读回的 `SystemTime` 与写入时逐纳秒相同；超出 i64 纳秒能表示的范围（约 1677 年到 2262 年）的时间返回 `InvalidInput`。
- **标识**：十六进制标识存成小写 TEXT，长度与字符由 CHECK 约束。`RepoName`、`RepoPath`、`SessionId` 存它们的字符串形式（types.md §3 的 `Display`）。读回时经 `TryFrom<String>` 重新校验，失败就是 `Corrupt`（`bad_row`）。`PathBuf` 存 UTF-8 文本；不是 UTF-8 的路径在写入时返回 `InvalidInput` [我的决定]。
- **枚举值**一律存小写 snake_case [我的决定]，包括 `CheckpointReason`（`tool_call`、`manual`、`before_publish`……）和 `EntryKind`（`added`、`modified`……）：库里只有一种写法；读回时按固定的对照表转换，对不上就是 `Corrupt`（`bad_row`）。
- **JSON 列**有九个，形状由各自的所有者规定，store 只检查是 JSON：`deps_sets.dirs`（`DepsSet.dirs` 的字符串数组）、`deps_attempts.error`（`StrataError`）、`sessions.scene` 与 `workspaces.scene`（session 的现场，增补 4）、`workspaces.conflicts` 与 `move_intents.conflicts`（`RepoPath` 的字符串数组）、`publishes.outcome`（publish.md 的结果摘要）、`refs_audit.confirmed`（types.md `Action` 的 JSON）、`gc_runs.report`（`GcReport`）。
- **序号**：
  - `CheckpointId`：每个会话一个序列，在 `record_checkpoint` 的写事务里取该会话已提交的最大值加 1。回滚的尝试不占号，下一个提交的 checkpoint 拿紧接着的号，所以序列没有空洞 [我的决定]。这与 3a:589 原文的"失败留下空洞"不同：LayerFS 的预留先发给调用方再使用，strata 的号与 checkpoint 行在同一个事务里出现，任何人都看不到一个没提交的号（增补 4 的 T15 一条把 3a:589 改成"没有空洞"）。提交过的号永不复用，重开之后接着递增。会话的行只会被 gc 整个删除，删除之后没有人再引用它的号。
  - `ProposalId` 与依赖集尝试的序号：`AUTOINCREMENT` [我的决定]，由 store 在写事务里分配并返回（types.md：`ProposalId` 由 Store 分配）。gc 会删掉旧的提议与尝试，`AUTOINCREMENT` 保证最大号被删之后也不会再发出去：`git-strata deps approve <id>` 永远点不到另一个提议，`.staging/<attempt>/` 的名字永远不会撞上一个被删的尝试留下的目录。
  - 意图的代数 `n`：每个 (会话, repo) 一个序列，`begin_move` 取工作区当前代数与它全部意图的 `n` 里最大的加 1；插入守卫再核一次。被拒绝或被收走的意图也占号，所以一个 `n` 从不复用（增补 4），管理目录 `worktrees/<n>/` 的名字因此不会撞上一次失败的移动留下的东西。
  - `ReleaseGroup.id`：`publishes` 的行永不删除，取最大值加 1。
  - 这些号都只写 1 到 `i64::MAX`（types.md §3）。下一个号会超过 `i64::MAX` 时，写操作在写任何东西之前返回 `TooLarge`（`detail.limit` 指明是哪种号），已存的状态不变，不回绕（3a:590、3a:596）；`AUTOINCREMENT` 的表在分配之前先查 `sqlite_sequence`，因为 SQLite 自己到顶时报的是 `SQLITE_FULL`，不是一个能区分的错误。读回小于 1 的值说明库坏了：store 先检查，返回 `Corrupt`（`bad_row`），不把它交给 `From<i64>`（types.md §3）。
  - `base_gens.id`、`refs_audit.id`、`gc_runs.id` 只在 store 内部用，不出现在公开接口上。
- **可变的列**只有这些，其余每张表的每一列写入后都不可改（§4.3 的触发器拦）：`sessions` 的 `lifecycle`、`phase`、`scene`、`removed_ns`；`workspaces` 的 `base_gen`、`head`、`generation`、`conflicts`、`health`、`scene`；`move_intents` 的 `state`、`phase`、`conflicts`、`old_tree`、`ended_ns`；`deps_attempts` 的 `state`、`pid`、`ended_ns`、`error`；`deps_proposals` 的 `state`、`tree`、`head`、`superseded_by`、`updated_ns`；`deps_main` 除 `repo` 以外的列；`publishes` 的 `phase`、`ended_ns`、`outcome`（`unknown` / `done` 的行可以只改 `outcome`，阶段与 `ended_ns` 不变，供 `set_publish_outcome` 与 publish.md §5.7 的 `assign`；`running` 的行不能只改 `outcome`）。状态列只能沿 §4.3 的转换图前进。
- **schema 版本**：`user_version` 取 `BASE_VERSION`（1）到 `LATEST`（2）。比 `LATEST` 新的（新版 git-strata 写的）、比 1 小的、`application_id` 不对、日志模式不是 WAL、schema 文本与该版本的 `ddl(v)` 有任何差别的库一律拒绝打开（`Corrupt`），不修补、不转换；版本号大于 `LATEST` 时消息提示安装写它的那个版本。旧版本由写者 open 按 §4.4a 前进迁移；只读 open 不迁移，按库自己的版本核对 schema 并照常读（v1 的会话 `agent` 读成 `None`）。
- **`sessions.agent`**（v2）：创建会话的 AI agent 名，types.md `AgentName`（1 到 64 个 `[A-Za-z0-9._-]`，首字符是字母或数字），`NULL` 表示不知道。CHECK 与 `AgentName` 逐字符一致：Turso 的 `GLOB` 与 `length()` 在内嵌的 NUL 处停下，所以另加 `length(CAST(agent AS BLOB)) = length(agent)`，它同时挡住非 ASCII；两边的一致性由 `agent_column_matches_agent_name_and_is_immutable` 用一张表逐条核对。写入后不可改（触发器 `sessions_agent_guard`，包括 `NULL` → 值）。

- **`dev_ports`**（v2）[实现时补的决定]（2026-10-28，dev-ports 设计）：每个会话工作区一行租约，`base..base+3` 四个端口（web、connector、connector_mcp、aux）。`UNIQUE(base)` 加上 4 对齐，两条租约不可能重叠。分配：在 daemon 设的策略（config.md §2.5 的块）里，从 `fnv1a64("<session>/<repo>")` 对块数取模的位置起环形找（reassign 从旧块之后起），跳过已租的块与旧块，每块的 4 个端口都通过绑定探测（`0.0.0.0`、`[::]`、`127.0.0.1`，`SO_REUSEADDR`；`EAFNOSUPPORT`、`EADDRNOTAVAIL`、`EPROTONOSUPPORT` 当作通过）才用；一次最多探 64 块。写入时机：`create_session` 与 `add_workspaces` 在同一个写事务里给每个新工作区分一块（找不到就不建，从不让 `new` / `add` 失败）；`ensure_dev_ports`、`reassign_dev_ports`。删除时机：`reassign` 删旧行再插新行；`set_lifecycle` 到 `Removed` 时在同一事务里删掉该会话的全部租约。这是 S2"`DELETE` 只来自 gc"的例外：租约是可回收的分配，不是历史；表上没有 gc 守卫触发器，gc 的删除顺序里它排在 `workspaces` 之前（子行先删）。

### 4.4a schema 迁移

迁移只前进、只在写者 open 里跑，整段在一个事务里：

- **登记**：`schema::MIGRATIONS` 是 `Migration { to, name, sql }` 的有序表，第 i 条把库从 `to - 1` 带到 `to`，`sql` 的最后一句设 `PRAGMA user_version = to`。`LATEST` 是最后一条的 `to`。已发布的迁移不改；改 schema 就加一条。
- **v1 → v2（`sessions.agent`）**：

  ```sql
  ALTER TABLE sessions ADD COLUMN agent TEXT CHECK (agent IS NULL OR (length(agent) BETWEEN 1 AND 64 AND length(CAST(agent AS BLOB)) = length(agent) AND agent GLOB '[A-Za-z0-9]*' AND agent NOT GLOB '*[^A-Za-z0-9._-]*'));
  CREATE TRIGGER sessions_agent_guard BEFORE UPDATE ON sessions WHEN NEW.agent IS NOT OLD.agent BEGIN SELECT RAISE(ABORT, 'immutable: sessions.agent'); END;
  PRAGMA user_version = 2;
  ```

  已有的会话 `agent` 为 `NULL`，其余行一字不变（`writer_open_migrates_v1_and_keeps_every_row` 用 v1 夹具逐行比对）。

  同一条迁移（名字 `sessions.agent+dev_ports`，dev-ports 设计与 agent-id 合成一个 v2，peilin 2026-10-28）还建 `dev_ports` 表，写在 `PRAGMA user_version = 2;` 之前：

  ```sql
  CREATE TABLE dev_ports (
      session TEXT    NOT NULL REFERENCES sessions (id),
      repo    TEXT    NOT NULL,
      base    INTEGER NOT NULL UNIQUE CHECK (base BETWEEN 1024 AND 65532 AND base % 4 = 0),
      PRIMARY KEY (session, repo),
      FOREIGN KEY (session, repo) REFERENCES workspaces (session, repo)
  ) STRICT;
  ```

  迁移不给已有的工作区租约：它们在第一次 `session ports` / `session exec`（daemon 的 `ports_ensure`）时拿到。
- **步骤**（§5.2 写者第 9 步）：
  1. 库不是干净关闭的就先跑 `quick_check`（干净关闭的也补跑一次：迁移之前要知道库是好的）。
  2. 备份：`PRAGMA wal_checkpoint(TRUNCATE)` 必须返回 busy = 0 且 `-wal` 长度为 0，最多等 5 秒（每 100 ms 再试）；做不到（有读者持着旧快照）→ `Busy`（`migration_backup`，`not_written`），什么都没改，读者结束后重启 daemon 即可。然后建 `store/backups/`（新建时同步 `store/`），`clonefile` 出 `store/backups/store-v<from>-<ns>.db`，对它与目录做 `F_FULLFSYNC`，再不经 SQLite 核对它：魔数、`application_id`、`user_version = from`、页大小 × 页数等于文件长度。任一步失败就删掉这个克隆，`not_written`。
  3. `BEGIN IMMEDIATE`，依次执行待做的迁移，在事务里回读 `user_version` 必须是 `LATEST`，并按 `ddl(LATEST)` 比对 schema；任一不对 → `ROLLBACK`，`not_written`，库仍是旧版本，下次 open 再试。
  4. `COMMIT`。确定的失败（约束、`Busy`）→ `not_written`。其它失败 → `Unknown`（`reason = "migration_commit"`，`outcome = "uncertain"`，`detail` 带 `from`、`to`、`backup`）：新 schema 是否落盘不知道，而 Turso 只在新进程里重跑 WAL 恢复，所以调用方必须退出；daemon 不在进程内重试这个错误（waiting.rs `retryable` 排除它），launchd 重启后的 open 读到的要么是完整的旧版本（再迁移一次），要么是完整的新版本（`uncertain_commit_is_resolved_by_a_fresh_process`）。
  5. 提交之后尽力做一次 TRUNCATE 检查点，并在 stderr 写一行迁移与备份路径。这一步只整理文件，不是版本的依据：所有 open 都以 PRAGMA 读到的 `user_version`（事务里写下的）为准；检查点没做成时文件头还是旧版本号，那也是 §5.2 第 2 步接受的值。提交之后进程立刻死掉，下次 open 看到的是 v2，不再迁移（`crash_right_after_commit_keeps_v2`）。
- **读者与迁移**：已经打开的只读 `Store`（同一进程或另一个进程，例如正在跑的 cli）跨过迁移照常工作。另一个进程迁移之后，Turso 的旧连接在每个显式读事务的第一句碰表的语句上报 `Database schema changed`，直到这个连接执行一句自动提交的语句重新载入 schema；所以 `read_txn` 遇到它时先执行一句自动提交的 `SELECT count(*) FROM sqlite_schema`，再把整个读事务重做一次（只一次）。读函数因此是可重放的（`Fn`）。
- **恢复到旧版本**：迁移不可逆，旧版 git-strata 拒绝 v2 的库（`Corrupt`，`user_version`）。要回到旧版：停 daemon（`launchctl bootout`），把 `store/store.db` 与它的 `-wal`、`-shm`、`-tshm` 挪开，把 `store/backups/store-v1-<ns>.db` 复制成 `store/store.db`，装回旧版本再启动。迁移之后新建的会话不在备份里（它们的目录还在 `sessions/` 下，旧版会把库里没有的目录当成陌生东西）；所以只在迁移后马上发现问题时这样做。

### 4.5 不变量

| # | 不变量 | 落实 design §9 |
| --- | --- | --- |
| S1 | blob 按内容寻址、只增不改：`blobs/<aa>/<id>` 一旦改名到位，字节永不改写（3a:103）；同一内容只存一份；文件名就是内容的 BLAKE3，不另存哈希。 | 不变量 1（恢复依赖它）、反目标 1 |
| S2 | 写入过的行只按 §4.4 列出的可变列改动。`DELETE` 只来自 gc（触发器加 `strata_gc_active()`）；`publishes`、`publish_items`、`refs_audit` 连 gc 也不删。文件系统上，store 只在两处删除东西：gc，以及丢弃本次调用自己刚建、从未登记的临时文件（put 的重复克隆、符号链接的克隆、建库的探测文件与空目录）。目录离开活路径只经 `retire_dir` 或 gc 改名进 trash。 | 不变量 5、6 |
| S3 | checkpoint 行提交之前，它引用的每个 blob 都已改名到位、已用 F_BARRIERFSYNC 排在提交之前、并在同一个事务里登记；不存在悬空引用（3a:475、3a:617、3a:672）。 | 不变量 1 |
| S4 | 一个 Store 只有一个写者：写连接只有一个，进程之间由 `LOCK` 的 flock 保证。读走独立的只读连接，WAL 之下从不等写者。 | —（design §5"daemon 不在时 CLI 只读"与 §8"并发"的性质，不对应 §9 的某一条） |
| S5 | 序号在 1 到 `i64::MAX` 之间、只增、提交过的永不复用；意图的代数从不复用（§4.4）。 | —（types.md §3 与增补 4 的性质，不对应 §9 的某一条） |
| S6 | 工作区行的 `base_gen` 指向一个 (repo, tree, deps) 组合，`finish_move` 与 `resolve_move` 的 Accept 都用一个事务同时换掉 tree、deps、head 与代数，不存在只换一半的状态；基线一定属于工作区的 repo（复合外键）。 | 不变量 8 |
| S7 | 基线与依赖集的行、目录永不原地修改；登记过的目录只有 gc_repo 能拿走：先把根目录准备好（清掉不可变标志、给属主补上写权限），准备不成就连行一起留着；准备好了才删行、再改名进 trash，过了 1 小时才删。 | 不变量 5 |
| S8 | `refs_audit` 里 `update_ref` 的行只能是 `refs/strata/` 下的 ref（CHECK 约束）。store 自己不写任何 git 文件；它的全部路径都在 `<root>` 下，而 config.md 的 V3 保证 `<root>` 不在任何主 checkout 里。 | 不变量 2 |
| S9 | 不确定的结果保留现场：结果不确定的提交让写者中毒，等重开（§6.4）；阶段为 Running 或 Unknown 的发布钉住它批次里的会话；Unknown 的会话带着现场，只随会话一起删除；Unknown 的工作区带着现场，直到 `resolve_move` 带着匹配的意图把它记回 Ok（现场留作历史），否则随会话一起删除；gc 从不碰非终态意图与尝试点名的路径、1 小时以内的东西（按 mtime 与 ctime 中较晚的那个）；损坏的 blob 只报不删。 | 不变量 6 |
| S10 | 每个 checkpoint 记下它对着的 `BaseGen`（`checkpoints.base_gen`，即 `Checkpoint.base`），读回时原样给出；每个留着的 checkpoint 都钉住它的基线（增补 5），跨基线的 restore 因此总能找到目标基线目录。restore 到基线不同的工作区由 overlay 走 `overlay::rebase`，从不按条目原样回放（增补裁定 T7/T6/T15）。 | 不变量 1 |
| S11 | 每个状态列只沿所有者定义的转换图走（§4.3 的触发器）：会话生命周期、工作区健康、意图、尝试、提议、发布。一个操作自己的意图行只会被关进终态，从不删除（增补 4"Error timing, refinement 2"）。 | 不变量 6 |

### 4.6 限制

- **扩展属性**：strata 不记录也不恢复 xattr（与 git 相同）。put 把克隆体上的扩展属性全部剥掉，blob 只含内容（增补裁定 T6/T7，来自 3c）。例外是内核托管的 `com.apple.provenance`：2026-10-22 本机实测，删除它的调用返回成功而属性仍在；它不是内容，store 不把它当失败。透明压缩用来存数据的 `com.apple.decmpfs`、`com.apple.ResourceFork` 不碰，剥掉会毁掉内容。
- **空目录**：overlay 只有文件，checkpoint 与 restore 从不携带空目录（与 git 相同，增补裁定 T6/T7）。store 的 blob 都是文件，没有目录 blob。
- **mode 不在 blob 里**：blob 文件一律 0444；条目的 mode 记在 `overlay_entries.mode`，取值同 git。同一路径的类型变化（文件 ↔ 符号链接）或 mode 变化是**一条** `Modified`（或 `Added`）条目，带新的 mode（120000 即符号链接），从不在同一路径上记 `Deleted` 加 `Added`：键 (session, checkpoint, path) 本来就不允许，`record_checkpoint` 在写之前就以 `InvalidInput` 拒绝（增补 4"Same-path type change"，overlay 与 publish 一致）。
- **断电**：提交时请求 `F_FULLFSYNC`，它要求设备把缓存写到介质（fcntl(2)）；设备或 USB 桥是否照做，store 管不了。§7 证明的是同步调用确实发出了，不声称断电级持久（3a:670）。
- **基线文件**不在 blob 存储里（§4.1）。基线目录里文件的完整性，由读基线目录的模块核对（overlay.md、session.md）。
- **只靠时间窗保护的 blob**（增补 4"Blob protection"）：blob 的行只由 `record_checkpoint` 登记。put 之后、登记之前，或者只被一次操作临时使用（例如三方合并的结果先 put 再 get）的 blob，只有 1 小时的年龄下限保护它。某个模块的现场若要让一个 blob 活得更久，就要经一个登记它的 store 方法；目前这样的方法只有 `record_checkpoint`，需要别的就在该模块的 §2 列出，store.md 再加。

## 5. 流程与算法

store 里的锁（daemon 按 repo 的写锁在 store 之外）：

- **L1 写者守卫**：`<root>/store/LOCK` 以 `O_CREAT | O_RDWR | O_CLOEXEC` 打开，`flock(LOCK_EX | LOCK_NB)`。`open` 取得，`Store` 析构时放开；进程被杀时由内核释放。`O_CLOEXEC` 让 deps 的冷装、publish 的检查这些子进程拿不到这个描述符，daemon 死了锁就放开（增补 4）。
- **L2 写锁**：`db` 的 `Mutex`，包住每个写事务。事务里只做 SQL；哈希、克隆、同步、对 blob 文件的 `lstat` 都在锁外。同一进程里的第二个写事务在这把 Mutex 上等，不返回 `Busy`：`Busy` 只来自另一个进程，它持有 LOCK（`open`）或 SQLite 的写锁（写事务）。
- **L3 blob 闸门**：`RwLock<()>`。put（从建临时克隆起到最后的改名）、get、read、verify、`record_checkpoint`（从核对 blob 文件起到提交）持共享锁；gc 清扫 `store/tmp/` 与 `blobs/`、删除 blob 文件的那几段持独占锁（增补 4"gc guards"）。一个调用只取一次，持有时从不再取：std 的 `RwLock` 读锁不可重入，前面排着等独占锁的 gc 时，第二次取读锁会死锁。
- **顺序**：repo 写锁在 L3 之前，L3 在 L2 之前（增补 4）。L1 在 Store 的整个生命里持有，不参与这个顺序。持 L2 时从不去取 L3，持 L3 时从不去取 repo 写锁，所以不会死锁。
- SQLite 自己的锁：写事务一律 `BEGIN IMMEDIATE`。读不取 L2：每个读函数从连接池借一个只读连接，在一个读事务里读完。

store 不覆盖工作区里已有的文件，所以本模块没有"先做 checkpoint"这一步，不变量 1 由调用方落实。原子替换只用在 store 自己的区域：tmp 到 blobs、临时库到 store.db 用 `renamex_np(RENAME_EXCL)`，目录到 trash 用 `apfs::rename_rooted(…, RenameMode::Excl)`（增补 8）；都是目标已存在时失败，从不覆盖。

### 5.1 `open`：第一次建库

1. `root` 必须是绝对路径，否则 `InvalidInput`。
2. 同卷检查，不写任何东西：对每个 checkout 调 `apfs::same_volume(&apfs::nearest_existing_ancestor(root)?, checkout)`（config.md §4.3；M17 之后"最近的已存在上级"由 apfs 的这个助手找，store 不自己往上找）。任一为假 → `CrossVolume`；checkout 不存在 → `NoSuchPath`；其余 `io::Error` 经 `apfs::classify_io_error`（读的一侧）归类。
3. 看 `<root>`：不存在、是空目录、或只含 `store`、`bases`、`deps`、`sessions`、`daemon` 这几个名字，就继续（`daemon` 是 daemon 的暂存与保留的推送现场；不收它的话，单独推送或 approve 之后 daemon 再也起不来。review M6 时发现，[实现时补的决定]）；有任何别的条目 → `NotEmpty`（`detail.entries` 列出前五个）。
4. `<root>/store/store.db` 已存在 → 走 §5.2。不存在、而 `bases/`、`deps/`、`sessions/` 里有条目 → `Corrupt`（`store_db_missing`）[我的决定]：这是丢了库，不是第一次打开；悄悄建一个空库会让已有的会话失去记录。
5. 建 `<root>`、`store/`、`store/tmp/`，打开 `LOCK` 并取得 L1（拿不到 → `Busy`，同 §5.2 第 1 步）。取得之后再看一次 `store.db`：另一个进程可能刚建完。
6. 探测 clonefile：在 `store/tmp/` 建一个探测文件，用 `apfs::clone_file` 克隆一次，随后删掉这两个文件。克隆返回 `Unsupported`（卷不是 APFS，或不支持克隆）时，store 用 `rmdir` 删掉本次调用自己刚建的空目录（只删空目录，从不递归），放开 L1，原样返回 `Unsupported` [我的决定]。这样类型化的拒绝之后没有净写入，符合"类型化拒绝只发生在写任何东西之前"的裁定。
7. 建 `store/blobs/00` 到 `store/blobs/ff`、`store/trash/`、`bases/`、`deps/`、`sessions/`，已存在不算错。
8. 在 `store/tmp/init-<32 位十六进制>.db` 建临时库：设 `journal_mode = WAL` 并回读，设 §4.2 的连接级设置，在一个事务里执行 §4.3 的 DDL（含 `application_id` 与 `user_version`），`PRAGMA wal_checkpoint(TRUNCATE)`，关闭连接（最后一个连接关闭时 SQLite 删掉它的 -wal 与 -shm），对临时库做 `F_FULLFSYNC`。
9. `renamex_np(临时库, store/store.db, RENAME_EXCL)`，再对 `store/` 目录做 `F_FULLFSYNC`。改名是原子的：要么完整的库在位，要么没有库。
10. 从 §5.2 写者第 2 步起打开它。

中途崩溃时，第 9 步之前留下的只有空目录和 `store/tmp/` 里的临时库。下次 `open` 从第 3 步重来（只含那四个名字的根目录算部分建好的布局），临时库由 gc 的 tmp 清扫收走。

### 5.2 `open`：已有的 Store；`open_read_only`

写者（`open`）：

1. 取 L1：`flock` 返回 `EWOULDBLOCK` → `Busy`（`lock = "store-writer"`），什么都不写。
2. 不经 SQLite，直接读 `store.db` 的文件头：魔数 `SQLite format 3\0`；偏移 18 与 19 的值为 2（WAL）；偏移 60 的 `user_version` 在 1 到 `LATEST` 之间；偏移 68 的 `application_id` 为 1398035009。任一不对 → `Corrupt`（`reason` 为 `application_id`、`user_version` 或 `journal_mode`）。这一步保证对不是 Store 的文件不建连接、不产生 -wal 与 -shm（3a:661）。建库时做过 TRUNCATE 检查点；`application_id` 之后再也不改。`user_version` 只由迁移改（§4.4a），迁移之后文件头可能暂时还是旧值，所以这一步只做范围检查，版本以第 3 步 PRAGMA 读到的为准。
3. 建写连接（`SQLITE_OPEN_READ_WRITE | SQLITE_OPEN_NO_MUTEX`，不带 CREATE），开 DEFENSIVE，`busy_timeout = 0`；用 PRAGMA 再核一次 `application_id`、`user_version`（1 到 `LATEST`，记下它作为库的版本 v）、`journal_mode`（不对同样是 `Corrupt`）。
4. 设 §4.2 的连接级设置（含 `trusted_schema = 0`）并逐项回读；不符 → `Unsupported`，`detail` 带设置名、期望值与实际值。
5. schema 比对：把版本 v 的 DDL（`ddl(v)`，§4.3 加上 §4.4a 里 v 之前的迁移）在 `:memory:` 库里执行一遍，两边各取 `SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY type, name`，必须逐行相同；否则 `Corrupt`（`schema`，`detail` 带第一处差别）。不修补、不迁移（3a:597、3a:610）。
6. 看 `store/clean-shutdown`（T6 裁定，fix round 1 之后）。在，说明上一次是 `close` 正常关闭的：删掉它并对 `store/` 做 `F_FULLFSYNC`（这一次若异常结束，下次打开就看不到它），不跑 quick_check；删不掉 → `Unknown`。不在（上次被杀、崩溃、写者中毒，或没调 `close` 就退出）：`PRAGMA quick_check` 必须返回 `ok`，否则 `Corrupt`（`sqlite`）。它的成本与库的大小成正比（§5.12），所以只在异常退出之后付；平时由 `quick_check()` 按需跑。标记是 `store/` 下的一个空文件，由 `close` 在关掉全部连接之后写下 [我的决定]。
7. 注册 SQL 函数 `strata_gc_active()`，带 `SQLITE_INNOCUOUS`（§4.2）。读连接池先空着，用到时再开。
8. 取 `<root>` 的 realpath，作为 store 在根之下的 apfs 调用（`open_rooted`、`attrs_of`、`list_dir`、`rename_rooted`、`remove_tree_rooted`、`remove_rooted`）的 `root` [我的决定：apfs 以 `O_NOFOLLOW` 打开根的最后一级，`[paths] root` 本身是符号链接时这些调用一律 `NotDirectory`；路径的拼接与 `retire_dir` 的前缀核对仍用配置里的 root]。下文 apfs 调用里写的 `<root>` 都指它。
9. v < `LATEST`：按 §4.4a 迁移（之前没跑过 quick_check 就先跑）。迁移失败时 open 返回那个错误，不交出 Store。

第 3 到 6 步里 SQLite 报 `SQLITE_CORRUPT` 或 `SQLITE_NOTADB` → `Corrupt`（`sqlite`）。对崩溃后的库，第 3 步会先做 WAL 恢复；这是 SQLite 自己的行为，不改变已提交的内容。

只读（`open_read_only`）：

1. 不取 L1。`store.db` 不存在 → `NoSuchPath`。
2. 同写者第 2 步。
3. 建只读连接（`SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NO_MUTEX`），开 DEFENSIVE，`trusted_schema = 0`，`query_only = 1`，`busy_timeout = 2000`；在一个读事务里核身份、按库自己的版本比 schema（同写者第 3、5 步）。不迁移：v1 的库照常读。只读连接做不了 WAL 恢复时 SQLite 报 `SQLITE_READONLY_RECOVERY` → `ReadOnly`（`recovery_needed`：先启动 daemon）[我的决定]。
4. 不改 store.db 的内容。WAL 协议下，`-shm`（以及 `-wal`）不在而目录可写时，只读连接会建出它们（2026-10-22 用 SQLite 3.54.1 实测）；那是 SQLite 的共享内存索引与空日志，不改变库的内容，§7 的只读测试在打开之后才取文件快照。此后每个写函数第一步就返回 `ReadOnly`，不碰 SQLite 和文件系统（3a:659）。只读打开不看、也不动 clean-shutdown 标记。

`close`：取 L2，等进行中的写事务结束；关闭读连接池与写连接（最后一个连接关闭时 SQLite 做检查点，并删掉 -wal 与 -shm）。写者没有中毒时，以 `O_CREAT | O_EXCL | O_WRONLY | O_CLOEXEC`、权限 0600 建 `store/clean-shutdown`，对它和 `store/` 做 `F_FULLFSYNC`；标记已经在（不该发生）就留着。最后关掉 LOCK，放开 L1。写标记失败返回 `Unknown`，下一次 `open` 照常跑 quick_check。只读的 Store 只关连接。

`quick_check()`：借一个只读连接跑 `PRAGMA quick_check`；结果不是 `ok` → `Corrupt`（`sqlite`，`detail` 带前十条消息）。它只读，写者与只读的 Store 都能跑。

### 5.3 `put_blob`、`put_blob_rooted` 与 `put_bytes`

`put_blob(src)` 不进 SQLite：

1. 只读 → `ReadOnly`。
2. 取 L3 共享锁，一直持到第 9 步结束（增补 4：put 从临时克隆起到最后的改名都持 L3 共享锁，而 gc 清扫 `store/tmp/` 与 `blobs/` 时持独占锁，所以清扫从来看不到进行中的 put）。
3. 取临时名 `store/tmp/put-<arc4random 生成的 128 bit，十六进制>`，`apfs::clone_file(src, tmp)`（不跟随 src 的符号链接）。临时名已存在（`Exists`）就换一个名字重试，最多 8 次，仍不行 → `Unknown`。新写入从不采用或截断已存在的临时文件（3a:228）。`clone_file` 的其它错误原样透传。测试接缝 `PauseInPut` 停在这一步之后。
4. 此后只看克隆体，不再碰 src：克隆是 src 那一刻的快照，哈希期间 src 被改写的竞态在这里不存在（overlay 的写竞态规则管的是它自己的判断，见 overlay.md）。`lstat(tmp)`：是符号链接 → `readlink(tmp)` 得到目标的字节，删掉自己的 tmp，转到第 10 步写临时文件的那一段存目标文本（符号链接的 blob 内容就是目标，mode 120000 记在条目里，裁定 T7）。这一转仍在第 2 步取得的 L3 共享锁之下，不再取一次（L3 不可重入，见本节开头）。是普通文件 → 以 `O_RDONLY | O_NOFOLLOW | O_CLOEXEC` 打开 tmp，第 5 到 9 步在 tmp 上的改动、读与屏障都用这个句柄。
5. 规整。克隆会把 src 的 mode、扩展属性、BSD 标志一起带过来，2026-10-22 实测连 `uchg` 也带。依次：`apfs::set_file_meta(&tmp, &FileMeta { clear_flags: true, mode: Some(0o600), mtime: None })` 清掉标志（`UF_COMPRESSED` 保留）并设 0600（只读文件上删扩展属性会 `EACCES`，同日实测）；`flistxattr(XATTR_NOFOLLOW)` 列出的每个名字都 `fremovexattr`，跳过 `com.apple.provenance`、`com.apple.decmpfs`、`com.apple.ResourceFork`（§4.6）。删扩展属性也是句柄上的改动，不另做核对：它夹在两次 `set_file_meta` 之间、用的是同一个句柄，第一次调用已经核对过这个 inode 只有 tmp 一个名字，之后才出现的别名指向的也只是 store 自己的克隆体（module-rulings 的"set_file_meta callers, clarified"）。
6. 哈希：读 tmp 的全部字节算 BLAKE3，16 MiB 及以上走 mmap 加多线程。代价与文件大小成正比；稀疏文件和大文件一样整份读，空洞读成零，不特殊处理（裁定 T7）。结果是 `id`，读到的字节数是 `size`。
7. 先看目标：`lstat(blobs/<id 前两位>/<id>)`。已存在，即同一内容已经存过：
   - 不是普通文件 → `Corrupt`（`blob_type`）；大小不等于 `size` → `Corrupt`（`blob_size`）。两种都把 `detail.path` 指向 tmp：tmp 是正确的一份，留给人修复，gc 过了年龄下限收走（3a:506）。
   - 否则 `utimensat(AT_FDCWD, 已存的 blob, UTIME_NOW, AT_SYMLINK_NOFOLLOW)` 刷新它的时间（私有区里的 libc 许可，§2），删掉自己的 tmp，返回 `id`。按路径设时间只要求是文件的属主，已存副本被改成 000 权限时 `open` 会 `EACCES`，这一步照样成功（2026-10-22 实测）。重复内容的 put 不做任何屏障，也不回读、不重算已存副本（3a:505、3a:507，裁定 T6）。
8. `apfs::set_file_meta(&tmp, &FileMeta { clear_flags: false, mode: Some(0o444), mtime: Some(now) })`：blob 文件的 mtime 就是存入的时刻。第 5 步与这一步的 `set_file_meta` 失败时 `blobs/` 里没有多出东西：拒绝原样返回（例如源带着 `SF_*` 标志，`ReadOnly`，`reason = "immutable"`），其余是 `Unknown`（`outcome = "not_written"`）；tmp 留在原处，gc 过了年龄下限收走。gc 按 mtime 与 ctime 中较晚的那个判断年龄（§5.11）。
9. `fcntl(tmp, F_BARRIERFSYNC)`：tmp 的数据与属性排在之后的任何 I/O 之前落盘（fcntl(2)：屏障之前用 fsync 刷出去的 I/O，保证先于屏障之后的 I/O 持久化）。然后 `renamex_np(tmp, blobs/<id 前两位>/<id>, RENAME_EXCL)`：
   - 成功 → 对扇出目录做 `fcntl(F_BARRIERFSYNC)`，让新名字也排在之后的写入之前；放开 L3，返回 `id`。
   - `EEXIST`，即另一个线程刚把同一内容存进来 → 同第 7 步的已存在分支（这次的屏障已经做了，无害）。
   - 其它错误 → `Unknown`（`outcome = "not_written"`：`blobs/` 里没有多出东西，tmp 留在原处）。
10. `put_bytes(bytes)`：先取 L3 共享锁（从第 4 步或 `put_blob_rooted` 的第 2 步转来时已经持有，不再取），在 `store/tmp/` 用 `O_CREAT | O_EXCL | O_WRONLY | O_NOFOLLOW | O_CLOEXEC`、权限 0600 建临时文件（重名处理同第 3 步），写入全部字节，从内存里的字节算 BLAKE3，然后同第 7 到 9 步。

返回之后，blob 已在 `blobs/` 里，名字与数据都排在之后的任何写入之前；下一次 SQLite 提交的 `F_FULLFSYNC` 排空设备队列，于是"checkpoint 行持久"蕴含"它引用的 blob 持久"（S3）。并发：两个线程 put 同一内容，各自克隆、哈希，`RENAME_EXCL` 只让一个改名成功，另一个走 `EEXIST` 分支；两边返回同一个 id，只落一份（3a:582）。put 不登记 blob：行由 `record_checkpoint` 在同一个事务里写；在那之前只有 1 小时的年龄下限保护它（增补 4"Blob protection"，§4.6）。

`put_blob_rooted(src_root, src)`（增补 8）与 `put_blob` 只差取得克隆体的那一段：

1. 同第 1、2 步：只读 → `ReadOnly`；取 L3 共享锁，一直持到最后。
2. `apfs::attrs_of(src_root, src)`：上级逐级不跟随，最后一级是 lstat 语义。上级里有符号链接（`NotDirectory`，`reason = "symlink"`）、不存在（`NoSuchPath`）等错误原样透传。再看种类 [我的决定：`clone_from_rooted` 以不跟随的方式打开源，打不开符号链接本身；`attrs_of` 经同样钉住的上级读出链接的目标，也不出根；目录与特殊文件在这里就拒绝，不必再开一次]：
   - `Dir` → `IsDirectory`；`Fifo`、`Socket`、设备 → `NotFile`。什么都不建。
   - `Symlink { target }` → 把目标的原始字节转到第 10 步存成 blob（符号链接的 blob 就是目标文本，裁定 T7），不克隆。
   - `File` → 下一步。
3. 取临时名（同第 3 步），`apfs::clone_from_rooted(src_root, src, tmp, CloneKind::File)`。apfs 在根之下逐级不跟随地打开 `src`，`fstat` 钉住的描述符，再从它克隆，所以存下的是这一刻 `src` 上的那个对象，绝不是根之外的文件。第 2 步之后 `src` 被换掉，apfs 在克隆之前拒绝、原样透传，`tmp` 不存在，`store/tmp/` 里什么都没多：换成目录 → `IsDirectory`；换成 FIFO、socket、设备或符号链接 → `NotFile`；某一级上级换成符号链接 → `NotDirectory`（`reason = "symlink"`）。
4. 克隆体是普通文件（apfs 按 `CloneKind::File` 核对过钉住的描述符）：从第 5 步一直做到第 9 步（规整、哈希、看目标并核对长度、0444 与 mtime、屏障、改名），L3、长度与 mtime 的规则都和 `put_blob` 一样。

工作区为什么不能用 `put_blob(ws.root/path)`：按路径克隆时，内核照常跟随上级里的符号链接。overlay 扫描之后、put 之前，工作区里某一级目录被换成指向工作区之外的符号链接，按路径的 put 就会把外面同名的文件存进 store（2026-10-22 实测：`a/b` 换成指向别处的符号链接之后，对 `a/b/f` 的 `clonefile` 克隆出的是别处的 `f`），之后的 restore 或发布再把它带进工作区或 git。

### 5.4 `get_blob`、`get_blob_rooted`、`read_blob`、`verify_blob`

四者都持 L3 共享锁，按文件工作（增补 4"Blob protection"）：

1. `lstat(blobs/<id 前两位>/<id>)`：不存在 → `Corrupt`（`blob_missing`，`detail.path`）；不是普通文件 → `Corrupt`（`blob_type`）。
2. 用一个只读连接查 `blobs` 行：有行而大小与文件不符 → `Corrupt`（`blob_size`）；没有行就不比，blob 文件在就能用。所以 put 之后、还没有任何 checkpoint 时，put → get 照样可以往返（3a:141），restore 与 rebase 也能取刚 put 的合并结果。不重算哈希：get 与 read 是受信任读（裁定 T6）。

`get_blob(id, dst)` 接着：

3. `apfs::clone_file(blob, dst)`：`dst` 已存在 → `Exists`，dst 不动；上级目录不存在 → `NoSuchPath`；上级不是目录 → `NotDirectory`；不可写 → `ReadOnly`；名字过长 → `InvalidInput`；跨卷 → `CrossVolume`。clonefile 要么建出完整的 dst，要么不建（3a:148）。
4. 把 dst 弄成 git checkout 出来的样子 [我的决定]：以 `O_RDONLY | O_NOFOLLOW | O_CLOEXEC` 打开 dst，把句柄交给 `apfs::set_file_meta(&file, &FileMeta { clear_flags: true, mode: Some(0o644), mtime: Some(now) })`（module-rulings 的"Metadata changes through a handle go through apfs::set_file_meta"）：它先核对句柄是普通文件且 `st_nlink` 为 1，核对不过就什么都不改；再清掉克隆带来的 BSD 标志（`UF_COMPRESSED` 保留）、设 0644、把 mtime 设为当前时刻（atime 不动）。句柄上的改动作用在 inode 上，会连带改到这个 inode 在别处的名字；克隆刚建出的 dst 只有这一个名字，`st_nlink` 不是 1 说明 dst 在克隆之后被换掉了。先清标志再设 mode：put 存下的 blob 不带标志（§5.3 第 5 步），但 blob 文件被人加上 `uchg` 时，克隆会把它带过来，带着它改 mode 会 `EPERM`（2026-10-22 实测）。blob 文件是 0444，mtime 是当初 put 的时刻；把旧 mtime 带进工作区，增量构建（cargo、tsc、Turbopack）会把内容变了的文件当成没变。可执行位由调用方按条目的 mode 设。打不开 dst、`set_file_meta` 拒绝或中途失败 → `Unknown`（`outcome = "partial"`，`detail.path` 是 dst：dst 已经建出，留在原处）。
5. 不写 SQLite，不碰 dst 之外的任何东西。

`get_blob_rooted(id, root, dst)`（增补 8）做完第 1、2 步之后，与 `get_blob` 只差落地的两步：

3. `apfs::clone_file_rooted(blob, root, dst, None)`：dst 的上级逐级以 `O_DIRECTORY | O_NOFOLLOW` 打开并钉住，再在钉住的父目录上只用最后一个名字克隆。任何一级是符号链接或文件 → `NotDirectory`（符号链接时 `reason = "symlink"`），上级不存在 → `NoSuchPath`，都在写任何东西之前；dst 已存在（含悬空符号链接）→ `Exists`，链接指向的地方不被碰；不可写 → `ReadOnly`；名字过长 → `InvalidInput`；跨卷 → `CrossVolume`。传 `None`：克隆会带来 blob 文件的标志，mode 留到下一步、清掉标志之后再设。
4. `apfs::open_rooted_file(root, dst)`：每一级重新以 `O_NOFOLLOW` 打开，要求是普通文件；把句柄交给 `get_blob` 第 4 步的同一个 `apfs::set_file_meta` 调用。克隆之后 dst 或它的某一级上级被换掉：换成符号链接、目录或特殊文件，`open_rooted_file` 拒绝；换成根之外某个文件的硬链接，`set_file_meta` 的 `st_nlink` 核对拒绝（2026-10-22 实测：克隆之后把 dst 换成根之外一个 0600 文件的硬链接，`open(O_NOFOLLOW)` 照样打开它，按句柄的 `fchmod 0644` 把那个文件改成了 0644；带 `uchg` 的文件连硬链接都建不成，`EPERM`）。两种都是 `Unknown`（`outcome = "partial"`，`detail.path` 是 dst：克隆已经落地，或者已被换走），根之外的文件不会被改标志、mode 或时间。`set_file_meta` 中途失败同 `get_blob` 第 4 步。
5. 同 `get_blob` 第 5 步。

按路径的 `get_blob` 为什么不能写进会话树：内核按路径解析 dst 时照常跟随上级里的符号链接。工作区、会话根与 staging 树是 agent 能写的地方，检查之后、get 之前某一级目录被换成指向外面的符号链接，按路径的 get 就把文件建在了外面（2026-10-22 实测：`a/b` 换成指向别处 X 的符号链接之后，对 `a/b/f` 的 `clonefile` 在 X 里建出了 `f`；同样的替换下，以 `O_DIRECTORY | O_NOFOLLOW` 打开 `b` 得到 `ENOTDIR`，什么都没写）。所以写进会话树的 blob 一律经 `get_blob_rooted`（增补 8"get_blob_rooted"）：overlay 的 restore 把 blob 取进会话根之下的临时目录 T（root 是会话根），投影取进 dst 正下方的临时名（root 是 dst），之后都用 `apfs::rename_rooted` 换到位置上（overlay.md §5.7、§5.12，增补 7、8），store 不参与改名。session 的种子与携带都不经 blob 存储（session.md §5.3、§5.7）。按路径的 `get_blob` 只给 strata 自己的、不在会话树之下的目的地。

`read_blob(id, max)`：第 1、2 步之后，`size > max` → `TooLarge`（`limit = "read_max"`）；否则读出全部字节返回，不重算哈希。overlay 用它取符号链接的目标，publish 用它取三方合并的输入，`max` 由调用方给（大文件阈值不在 store，§9）。

`verify_blob(id)`：第 1、2 步之后重算 BLAKE3；与 `id` 不符 → `Corrupt`（`blob_mismatch`，`detail` 带期望与实际）。`restore --verify` 和 gc 的完整性扫描用它；它从不修改或删除任何东西。

3a 有几行（3a:198、3a:555、3a:507）原文写的是"读出时发现内容与 id 不符就失败"。在 strata 里这由校验读承担，普通的 get 不承担：这是裁定 T6 的校验策略，§7 对应的测试按这个口径断言（增补 4 的 T15 一条把 types.md `Corrupt` 行的"读 blob 时 BLAKE3 不符"改成只指 `verify_blob`）。

### 5.5 基线 generation 与 `retire_dir`

`record_base_gen(gen, manifest, at)` 一个 `BEGIN IMMEDIATE` 写事务，持 L2：

1. `manifest.tree` 必须等于 `gen.tree`，否则 `InvalidInput`（`field = "tree"`）。
2. 依赖集 (gen.repo, gen.deps) 必须已登记，否则 `InvalidInput`（`reason = "deps_unregistered"`）。
3. 清单：同一 (repo, tree) 已登记 → 按 path 顺序逐条比较 (path, blob, mode, size) 与条数；相同就不写，不同 → `InvalidInput`（`reason = "manifest_mismatch"`，`detail.path` 是第一处差别）[我的决定：同一个 tree 导出了不同的字节，多半是检出过滤的配置变了，要人来看]。没登记 → 插 `base_trees` 一行（`files` = 条目数）和全部 `manifest_entries`。有 `base_trees` 这一行，空 tree（0 条）和没登记才能区分。
4. generation 已登记 → 不写；否则插 `base_gens` 一行（`created_ns = at`）。
5. `COMMIT`。

调用方是 session 的 `ensure_base` 与 `ensure_base_for`（session.md §5.2）：在 repo 写锁之外把源码导出进基线暂存 `bases/<repo>/.staging/<hex>`、备好依赖集、用 `overlay::build_manifest` 建出清单；清单一建好，就在该 repo 的写锁里登记：把依赖集克隆进去、改名到 `base_dir(gen)`、再调本方法（增补 3"Base manifest"、增补 4"Locks"，module-rulings 的"Base generations are built outside the lock"与"Base staging is registered right after its manifest"）。`move_workspace` 在锁里只找已建好的基线，从不建。store 不看目录本身。改名之后、登记之前崩溃，活路径上会留下一个没登记的目录：下一次 `gc_repo` 过了年龄下限把它挪进 trash（§5.11），或者 session 下一次发现它没登记时自己用 `retire_dir` 拿走再重建。

`retire_dir(path)`：

1. 只读 → `ReadOnly`。`path` 必须是绝对路径、在 `<root>` 之下、没有空组件和 `.`、`..`，去掉 `<root>/` 之后是一个合法的 `RepoPath`，否则 `InvalidInput`。文件系统上的核对（符号链接、存在、是不是目录）在第 3 步经 apfs 做（增补 8）。
2. 去掉 `<root>/` 之后，路径必须恰好匹配下表的一行，并在一个读快照里通过这一行的核对；不匹配任何一行、或核对不过 → `InvalidInput`（`reason` 写明是哪一行、哪一项没过）。`<repo>`、`<tree>`、`<deps>`、`<session-id>` 按 types.md 的规则解析，解析不成就不算匹配；`<attempt>`、`<m>` 是不带前导零的十进制正整数；`<name>` 是一个组件；`<hex>` 是 32 位小写十六进制 [我的决定：前六行就是 session 与 deps 在增补 2、3 之下会拿走的全部目录；第 7 行是 module-rulings 的"Base generations are built outside the lock"加的]。

   | # | 路径（相对 `<root>`） | 接受的条件 | 用它的 |
   | --- | --- | --- | --- |
   | 1 | `bases/<repo>/<tree>/<deps>` | (repo, tree, deps) 没有 `base_gens` 行。已登记的 generation 只由 `gc_repo` 回收 | session：建基线中途失败，或发现没登记的残留 |
   | 2 | `deps/<repo>/<deps>` | (repo, deps) 没有 `deps_sets` 行 | deps |
   | 3 | `deps/<repo>/.staging/<attempt>` | 尝试 `<attempt>` 属于 `<repo>`，状态是 Running、Sealed 或 Refused；或者它的行已被 gc 删掉（号不大于 `sqlite_sequence` 里已经发出的最大号）。Failed 的尝试的暂存是现场，status 经 deps 的 `attempts` 报出（deps.md D8），不接受 | deps（可选，也可以照 deps.md 自己删） |
   | 4 | `sessions/<session-id>`，或 `sessions/.repos/<session-id>`（该会话全部影子仓库所在的目录，增补 2） | 会话是 Removing 或 Removed，或者已经没有行 [我的决定：被 gc 回收之后剩下的目录也能拿走]。Creating、Active、Unknown 的会话不接受：Unknown 的会话先经 `rm --discard` 进入 Removing | session：`remove` |
   | 5 | `sessions/<session-id>/<name>` | 会话是 Active；`<name>` 不是该会话任何工作区的目录名；该会话有一条意图，它的 `staging` 就是这个路径而状态是 Preparing、Refused、Abandoned 或 Done，或者它的 `old_tree` 就是这个路径。交换（`RENAME_SWAP`）之后，换下来的旧工作区树就在 Done 意图的 `staging` 上；被保留并改了名时在 `old_tree` 上。Swapping 与 Unknown 的意图点名的路径是现场，不接受 | session：`move_workspace` 拿走换下来的旧树或具名拒绝时的暂存，`recover` 拿走没走到对外可见写的暂存 |
   | 6 | `sessions/.repos/<session-id>/<repo>.git/worktrees/<m>` | 会话是 Active；工作区 (session, repo) 存在；`m` 小于工作区的当前代数（交换之后被换下的旧管理目录），或者 (session, repo, m) 是一条 Preparing、Refused 或 Abandoned 的意图（没有换入的下一代管理目录）。当前代数的管理目录、Swapping 与 Unknown 意图的 `m`、没有任何意图的更大的 `m` 都不接受 | session：`move_workspace` 之后，以及具名拒绝、`recover` |
   | 7 | `bases/<repo>/.staging/<hex>` | 形状对就接受：基线暂存是缓存填充（增补 6），没有意图行或尝试行可以核对。谁拿走它由调用方保证：建它的那次调用，以及 daemon 启动时的 `recover`（那时没有建造在跑）；`gc_repo` 第 5 步把过了年龄下限的挪进 trash（§5.11） | session：建基线失败，或并发的建造者先登记了同一个 tree 时拿走自己的暂存；`recover` 拿走全部残留（session.md §5.2、§5.8） |

   接受 Preparing 的意图与 Running 的尝试，是为了让操作先拿走自己的暂存、再把行关进终态（增补 4"Error timing, refinement 2"）：两步之间崩溃，行还是 Preparing 或 Running，`recover` 或下一个持锁者会把它关掉。第 5、6 行要求会话是 Active：意图只在 Active 的会话上开始（`begin_move`），会话离开 Active 之后，这些目录随第 4 行的整个会话根与影子仓库一起离开。
3. 打开并准备（增补 6、8）：`apfs::open_rooted(<root>, 相对路径)`，每一级都以 `O_NOFOLLOW` 打开。任何一级是符号链接（apfs 的 `NotDirectory` 或 `NotFile`，`reason = symlink`）→ `InvalidInput`（`reason = "symlink"`）；不存在 → `NoSuchPath`；最后一级不是目录（打开之后 `fstat` 看到的，或 apfs 因为它是 socket、设备而拒绝）→ `NotDirectory`。然后把这个句柄交给 `apfs::make_removable`（apfs 第 2 轮加的，§2）：它先清掉 `UF_IMMUTABLE` 与 `UF_APPEND`（其它标志不动），再 `fchmod` 给属主补上写权限（`u+w`，其它位不动）。顺序不能反：带 `uchg` 的目录连 `chmod` 都会 `EPERM`；把目录挪到另一个父目录要改它自己的 `..`，需要对它本身有写权限（2026-10-22 实测：0555 的目录改到别的父目录 `EACCES`，在同一父目录里改名不受影响；带 `uchg` 的 `EPERM`；清掉标志、补上写权限之后都成功）。deps 把封存的根设成 0555（deps.md §4.6），clonefile 会把 `uchg` 带过来，所以这一步对七行一律做。准备失败原样返回 apfs 的码（`EPERM`、`EACCES` → `ReadOnly`），目录留在原处；`retire_dir` 从不删行，所以失败之后行与目录都和调用之前一样（标志可能已经清掉），调用方可以重试。
4. `apfs::rename_rooted(<root>, 相对路径, store/trash/<unix 毫秒>-<随机>, RenameMode::Excl)`，两边的上级都钉住、不跟随符号链接（增补 8）；再对 `apfs::open_rooted(<root>, store/trash)` 交来的句柄做 `apfs::full_fsync`（`F_FULLFSYNC`，增补 3"Durability"）[我的决定]：改名在返回之前持久，调用方随后提交的行（例如会话进入 Removed）不会比它先落盘。apfs.md 要调用方同步改名的两个父目录，store 只同步 trash 这一侧：源父目录那一侧保证的是"原路径上已经没有它"，store 用不着这个保证，因为掉电之后目录无论在原路径还是在 trash，对 store 都合法（§6.3 的"`retire_dir`：改名之后、同步之前"一行：还在原路径就再 `retire_dir` 一次，在 trash 里就由 gc 删）。只改名，不删除；trash 由 gc 在 1 小时之后删（§5.11）。返回 trash 里的路径。

session 用它拿走会话根、影子仓库、交换之后换下来的旧树、被拒绝的暂存、旧代的管理目录和基线暂存（增补 3：目录只经 `retire_dir` 离开，没有 session 私有的 `.trash`）；deps 可以用它拿走自己的暂存（第 3 行），也可以照 deps.md 自己删。

### 5.6 deps 的写

每个方法一个 `BEGIN IMMEDIATE` 写事务，持 L2，提交即返回。哪一步什么时候调用、调用之前读什么，是 deps.md 的事；这里只规定每个方法在一个事务里做什么。

- `begin_deps_attempt(repo, set, kind, at)`：同一 (repo, set) 已有 Running 的尝试 → `Exists`（`detail.attempt`）。取号 N = `sqlite_sequence` 里 `deps_attempts` 的值加 1（没有这一行取 1），到顶 → `TooLarge`（`limit = "attempt_id"`）。以显式的 N 插入 Running 的行，`staging = deps_staging_dir(repo, N)`。返回 N。deps 在拿到该依赖集的建造锁、算出 set 之后才调用它（T8 的 I3 裁定）；锁的持有者看到同一依赖集还有一条 Running 的行，说明那个建造者已经死了，deps 先用 `end_deps_attempt` 把它关成 Failed（`reason = "interrupted"`）再开始自己的，部分唯一索引让"先开始再关"失败 [我的决定：同一依赖集至多一条 Running 的尝试，由 `deps_attempts_one_running` 守住]。
- `set_deps_attempt_pid(attempt, pid)`：`pid > 0`，否则 `InvalidInput`。`UPDATE … WHERE id = ? AND state = 'running' AND pid IS NULL`：改了一行 → true；没改到时区分"没有这个尝试"（`InvalidInput`）、"已经记过 pid"（`InvalidInput`）与"已不是 Running"（false）。
- `end_deps_attempt(attempt, to, error, at)`：`to` 只能是 Failed 或 Refused（否则 `InvalidInput`）。`UPDATE … SET state, ended_ns = at, error = <StrataError 的 JSON> WHERE id = ? AND state = 'running'`：改了一行 → true，否则 false（没有这个尝试 → `InvalidInput`）。
- `record_deps_set(set, origin, attempt, at)`：origin 为 Empty 时 `attempt` 必须是 None、`set.dirs` 为空；Adopt、Build 时 `attempt` 必须是同一 (repo, set)、种类对应、仍为 Running 的尝试；否则 `InvalidInput`。(repo, id) 已登记 → `Exists`。插 `deps_sets` 一行（`sealed_ns = at`），把尝试改成 Sealed（`ended_ns = at`）。deps 在该 repo 的写锁里把暂存改名到 `deps_dir` 并 `chmod 0555` 之后立刻调用（T8 的 I1 裁定、增补 4"Locks"）。
- `record_proposal(new, replaces, at)`：先校验（`InvalidInput`）：`from ≠ to`；`tree` 与 `head` 同有同无；没有目标时 `origin` 必须有。位置：有目标 → 该 repo 的 Pending；没有 → 该 repo 该提出会话的 AwaitingPublish。读出位置上现有的那条，与 `replaces` 不符 → `ROLLBACK`，返回 `Ok(None)`。取号 N（同 `begin_deps_attempt`，到顶 → `TooLarge`，`limit = "proposal_id"`）；位置上有旧行就把它改成 Superseded（`superseded_by = N`，`updated_ns = at`）；以显式的 N 插入新行（Pending 或 AwaitingPublish，`created_ns = updated_ns = at`）；`COMMIT` 时检查延迟外键。返回 `Some(N)`。要不要登记、相同的 `to` 已经在了就直接返回它（幂等），这些判断在 deps 读 `proposals` 的时候做（deps.md §5.9）。
- `promote_proposal(id, tree, head, at)`：没有这条 → `InvalidInput`；不是 AwaitingPublish → false。把该 repo 现有的 Pending，以及该 repo 里 `to` 相同的其它 AwaitingPublish，都改成 Superseded（`superseded_by = id`）；再把 `id` 改成 Pending，写入 `tree`、`head`、`updated_ns`。返回 true。这照搬 deps.md §5.9（ccef28f）的提升规则；deps 的修订轮若要别的取代范围，在它的 §2 列出。
- `decide_proposal(id, to, at)`：没有这条 → `InvalidInput`；`Superseded { by }` 的 `by` 必须存在且不是 `id`（否则 `InvalidInput`）。`UPDATE … WHERE id = ? AND state = 'pending'`：改了一行 → true，否则 false。approve 发现封存出的 id 与 `P.to` 不同时（T8 的 I5 裁定）不走这里，而是 `record_proposal(替换提议, replaces = Some(P.id))`。
- `set_deps_main(row, expected)`：`row` 与 `expected` 的 repo 必须相同，否则 `InvalidInput`。在这个写事务里读出该 repo 的现有行，与 `expected` 按写入时的列值逐列比较：六列都比，含 `observed_ns` [我的决定：任何一列不同都说明读与写之间有别的观察写过；只比 set，会把 set 相同而 tree 或 head 更新了的一行当成没变，再用过时的观察盖掉它]；一边有行、一边没有也算不等。不等 → 什么都不写，返回 `(false, 现有行)`；相等 → 预期没有行就 `INSERT`，有就 `UPDATE … WHERE repo = ?` 换成 `row`，返回 `(true, 现有行)`。读、比较与写在同一个写事务里，中间插不进别的写。`row.set` 可以还没有封存（`observe_main` 只记它算得出的，T8 裁定）。增补 7"Main deps record"：deps 的 `observe_main` 先读 `deps_main`、登记提议，最后以读到的那一行作 `expected` 写，所以中途被打断或先后颠倒的一次观察由下一次观察重做；没写成之后怎么办由 deps.md 定。

### 5.7 会话、工作区与移动意图

每个方法一个 `BEGIN IMMEDIATE` 写事务，持 L2。

- `create_session(manifest, at)`：`repos` 不能为空；`root` 必须等于 `<root>/sessions/<id>`；每个工作区的 `path` 必须等于 `root/<repo>`；每个 `base` 必须已登记且 `base.repo` 等于它的键（以上 `InvalidInput`）。这个 id 已有行（任何生命周期）→ `Exists`（`reason = "store_row"`）；`root` 或某个 `path` 已被别的会话占用 → `Exists`。插 `sessions` 一行（`creating`，`created_ns = at`）和每个 `WorkspaceState` 一行 `workspaces`（代数 1、健康 `ok`、冲突 `[]`）。增补 6：`create_session` 就是 `new` 的第一次对外可见的写，`new` 的类型化拒绝全在它之前（它之前允许做缓存填充，增补 6 细化 4）；之后任何失败都是 Creating → Unknown，带现场。
- `set_session_phase(id, phase)`：没有这个会话 → `NoSuchSession`；生命周期不是 Creating 或 Removing → `InvalidInput`。写入 `phase`。
- `set_lifecycle(id, from, to, scene, at)`：(from, to) 不在增补 4 的六条边里、或 `scene` 与 `to` 不配（进入 Unknown 必须带，其它不能带）→ `InvalidInput`，什么都不碰。`UPDATE … WHERE id = ? AND lifecycle = from`：改了一行 → true；没改到时区分"没有这个会话"（`NoSuchSession`）与"当前不是 from"（false）。进入 Removed 时写 `removed_ns = at`，进入 Unknown 时写 `scene`。触发器是兜底。
- `record_seeds(id, repo, seeds)`：会话不存在 → `NoSuchSession`；不是 Creating → `InvalidInput`；没有这个工作区 → `NoSuchRepo`；这一批里路径重复、或某个路径已经记过 → `InvalidInput`（`reason = "seed_exists"`），整批不写；路径不是 UTF-8 → `InvalidInput`。每个种子一行。
- `begin_move(id, repo, to, head, staging, at)`：会话不存在或已 Removed → `NoSuchSession`；不是 Active → `InvalidInput`；没有这个工作区 → `NoSuchRepo`；`tracked = 0` 或健康为 Unknown → `InvalidInput`（tracked = false 的仓库从不移动，T9 增补）；`to` 没有登记或 `to.repo ≠ repo` → `InvalidInput`；`staging` 不是 `<root>/sessions/<id>/<name>`（`<name>` 是一个组件，而且不是该会话任何工作区的目录名）→ `InvalidInput` [我的决定：暂存的名字归 session；store 要求它是会话根下的一级目录，这样它不会落进工作区或 store 的区域，`retire_dir` 的第 5 行也总能把它拿走]；已有一条没结束的意图 → `Exists`（`detail.n`）。n = max(工作区代数, 该工作区全部意图的 n) + 1，超过 `i64::MAX` → `TooLarge`（`limit = "move_generation"`）；插一行 Preparing（`started_ns = at`）。返回 n。
- `set_move_phase(id, repo, n, phase)`：意图不存在或已结束 → `InvalidInput`。写入 `phase`。
- `set_move_state(id, repo, n, from, to, conflicts, at)`：只接受 Preparing → Swapping、Refused、Abandoned（否则 `InvalidInput`）；进入 Swapping 时会话必须仍是 Active，而且必须带 `conflicts`，别的边不能带（否则 `InvalidInput`）。进入 Swapping 的那条 UPDATE 同时把冲突路径原样写进意图行（session.md 的 D46：交换之后失败、进程又崩溃时，冲突只能从这里找回）。`UPDATE … WHERE … AND state = from`：改了一行 → true，否则 false（意图不存在 → `InvalidInput`）。进入终态时写 `ended_ns = at`。
- `finish_move(id, repo, n, conflicts, at)`：意图必须是 Swapping（否则 `InvalidInput`）；`conflicts` 必须与意图行上的那份相同（否则 `InvalidInput`，`reason = "conflicts_mismatch"`，什么都不写）。取 (repo, to_tree, to_deps) 的 `base_gens.id`：没有 → `Corrupt`（`reason = "base_gen_missing"`；没结束的意图钉住它的目标，正常情况下不会发生）。一个事务里：工作区的 `base_gen`、`head`、`generation = n`、`conflicts` 一起改（S6），意图改成 Done（`ended_ns = at`）。
- `fail_move(id, repo, n, scene, at)`：意图必须是 Swapping（否则 `InvalidInput`）。一个事务里：意图改成 Unknown（`ended_ns = at`，冲突留着），工作区健康改成 Unknown、写入 `scene`（换掉上一次留作历史的那份）。
- `resolve_move(id, repo, n, accept, at)`（module-rulings 的"Resolving a failed move"与"Move conflicts persist on the intent"；session.md §5.9）：在同一个写事务里、改任何行之前依次核对：会话不存在或已 Removed → `NoSuchSession`；不是 Active → `InvalidInput`（`reason = "lifecycle"`）；没有这个工作区 → `NoSuchRepo`；健康不是 Unknown → `InvalidInput`（`reason = "not_failed"`）；n 不是这个工作区 n 最大的意图，或它的状态不是 Unknown → `InvalidInput`（`reason = "no_failed_move"`）。码与 reason 同 session.md §5.9 第 1、2 步的核对。守卫不过就回滚，什么都不写。Accept 时再取 (repo, to_tree, to_deps) 的 `base_gens.id`：没有 → `Corrupt`（`base_gen_missing`；Unknown 的意图钉住它的目标，§5.11，正常情况下不会发生）。然后先改工作区、再改意图（§4.3 的触发器按这个顺序核对）：Abandon 只把健康改成 Ok，意图改成 Abandoned；Accept 把健康改成 Ok，`base_gen`、`head`、`generation = n`、`conflicts`（意图行上的那份）一起改（S6），意图改成 Done、`old_tree` 记成它的 `staging`。两种都写 `ended_ns = at`，都不动工作区的 `scene`：它留作历史，同会话行离开 Unknown 之后的做法。store 只核对行；磁盘上的现场是"没换"还是"已换"、pin 在哪，由 session 在调用之前核对（session.md §5.9 第 3、4 步）。
- `set_old_tree(id, repo, n, path)`：意图必须是 Done 且 `old_tree` 为空，`path` 的形状同 `begin_move` 的 `staging`（否则 `InvalidInput`）。

session 怎样串这些方法由 session.md 定。按增补 3、4 的错误时机，预期的用法是：

- `new`：（类型化拒绝与缓存填充都在这之前）`create_session` → 每个阶段开始时 `set_session_phase` → `record_seeds` → `set_lifecycle(Creating → Active)`；`create_session` 之后任何失败 → `set_lifecycle(Creating → Unknown, scene)`。
- `move_workspace`：`begin_move`（在建任何暂存之前）→ 建暂存 → 具名拒绝时先 `retire_dir(暂存)` 再 `set_move_state(Preparing → Refused)` → 否则在第一次对外可见的写之前 `set_move_state(Preparing → Swapping, 冲突)` → 交换、pin → `finish_move`；交换开始之后失败 → `fail_move(scene)`；旧树 → `retire_dir(staging)`，或保留并 `set_old_tree`。
- `resolve_failed_move`：人选了 Abandon 或 Accept；session 先核对磁盘上的现场（Accept 还要先移 pin），再 `resolve_move(accept)`；Abandon 之后 `retire_dir(staging)` 与 `retire_dir(<影子仓库>/worktrees/<n>)`，§5.5 的第 5、6 行接受 Abandoned 的意图（session.md §5.9）。
- `remove`：`set_lifecycle(Active → Removing)`（`--discard` 时也可以是 Unknown → Removing）→ `set_session_phase` → `retire_dir(会话根)`、`retire_dir(影子仓库)` → `set_lifecycle(Removing → Removed)`；中途失败 → `set_lifecycle(Removing → Unknown, scene)`。
- `recover`：`sessions()` 里 Creating、Removing 的 → `set_lifecycle(… → Unknown, scene)`；`open_move_intents()` 里 Preparing 的 → `retire_dir(暂存)` 再 `set_move_state(Preparing → Abandoned)`，Swapping 的 → `fail_move(scene)`（增补 3、增补 4"Error timing, refinement 2"）。

### 5.8 `record_checkpoint`

1. 纯校验，不碰任何东西（失败 → `InvalidInput`）：条目的 `path` 两两不同；没有哪条的 `path` 等于另一条 `Renamed` 的 `from`；`Renamed` 的 `from` 不等于自己的 `path`；`Deleted` 的 mode 为 0 且没有 blob，其余种类的 mode 是 0o100644、0o100755 或 0o120000 并带 blob；`at` 在 §4.4 的范围内。同一路径的类型或 mode 变化只能是一条 `Modified`（或 `Added`）条目（§4.6，增补 4）：同一路径上的 `Deleted` 加 `Added` 在这一步就被"path 两两不同"拒绝。
2. 只读 → `ReadOnly`；写者已中毒 → `Unknown`（`reason = "poisoned"`，`outcome = "not_written"`）。
3. 取 L3 共享锁。对每个被引用的、不同的 blob，在锁 L2 之外 `lstat` 它的文件：不存在 → `Corrupt`（`blob_missing`）；不是普通文件 → `Corrupt`（`blob_type`）；记下大小。L3 共享锁一直持到第 10 步，gc 删不了这些文件，所以第 6 步不用再看文件系统。
4. 取 L2，`BEGIN IMMEDIATE`。外部进程持着 SQLite 写锁 → `Busy`（`lock = "sqlite"`），不重试，什么都没写（3a:620、3a:659）。
5. 事务内核对：会话存在且不是 `removed`（否则 `NoSuchSession`）；工作区 (session, repo) 存在（否则 `NoSuchRepo`）且 `tracked = 1`（否则 `InvalidInput`：`tracked = false` 的仓库没有 overlay，T9 增补）；`cp.base` 已登记且属于 `cp.repo`，取得它的 `base_gens.id`（否则 `InvalidInput`）。钩子重放：`reason` 是 `ToolCall { id }`，而同一会话、同一 repo 已有这个工具调用 id 的 checkpoint → `ROLLBACK`，返回已有的 `CheckpointId`。唯一索引 `checkpoints_by_tool_call` 兜底。
6. 登记 blob：`INSERT … ON CONFLICT (id) DO NOTHING`，已有的行按先到者为准（3a:657）；已有行的大小与第 3 步看到的文件大小不符 → `Corrupt`（`blob_size`）。引用由 store 按条目自己推导、核对，不信任调用方自报（3a:617）。
7. 分配序号：该会话的 `COALESCE(MAX(id), 0) + 1`；已到 `i64::MAX` → `ROLLBACK`，`TooLarge`（3a:590、3a:596）。
8. 插 `checkpoints` 一行（`entries` = 条目数），每个条目插 `overlay_entries` 一行（`Renamed` 的 `from` 写进 `from_path`）。
9. `COMMIT`。`synchronous = FULL` 加 `fullfsync = 1`：提交时在 WAL 上做 `F_FULLFSYNC`，返回即持久。提交之前的任何失败，SQLite 回滚整个事务，返回 `Unknown`（`outcome = "not_written"`）或上面各步的码；提交本身的 I/O 错误 → `Unknown`（`outcome = "uncertain"`），写者中毒（§6.4）。
10. 放开 L2、L3，返回序号。

整个单元在一个事务里：checkpoint 行、全部条目与 blob 登记要么一起出现，要么都不出现（3a:657、3a:658）。L2 里只有 SQL，几毫秒；put 的哈希与同步、对 blob 文件的 `lstat` 都在 L2 之外完成。

### 5.9 发布与 ref 审计

每个函数一个写事务，持 L2：

- `record_publish`：`batches` 不能为空，每批的 `paths` 不能为空（否则 `InvalidInput`）；每个 `Batch` 的会话必须存在且不是 Removed（否则 `NoSuchSession`），(session, checkpoint) 必须存在（否则 `NoSuchCheckpoint`）；取号（最大值加 1，到顶 → `TooLarge`，`limit = "release_group_id"`）；插 `publishes` 一行（`phase = 'running'`），每个（批次下标，路径下标）插 `publish_items` 一行。路径原样照抄，不展开改名：批次怎么解释归 publish.md。返回号。会话的核对与别的点名会话的写一样（module-rulings 的"A Removed session takes no new pins"）：gc 分批回收 Removed 的会话，批与批之间放开 L2，一个落在两批之间的发布若能钉住这个会话，它的 checkpoint 行就会留下来、条目却已经删了，发布读到的是一个空的快照（§5.11）。
- `set_publish_phase`：比较并交换，只允许 Running→Done、Running→Unknown、Unknown→Done，其余 → `InvalidInput`（触发器兜底）。每次转换把 `at` 写进 `ended_ns`，`outcome` 可选。daemon 启动时把残留的 Running 改成 Unknown 并报出，从不重放（daemon.md）。
- `record_ref_change`：追加一行 `refs_audit`。`UpdateRef` 只能配 `Applied` 或 `HeadMoved`，ref 名必须以 `refs/strata/` 开头；`Push` 只能配 `Pushed`、`Refused` 或 `Unknown`；不配 → `InvalidInput`（CHECK 约束也拦）。`confirmed` 存 `Action` 的 JSON，从不存 `ConfirmToken`。

### 5.10 读

每个读函数从池里借一个只读连接，在一个读事务里读完，再归还连接（另一个进程刚迁移过 schema 时重做一次，§4.4a）；例如 `checkpoint` 读到的行与全部条目、`session` 读到的会话行、工作区行与种子都来自同一个快照。读从不取 L2，所以写事务进行时读照常完成（3a:620、3d:165）；WAL 之下，新开的读事务看得见全部已提交的写。读回的标识经 `TryFrom` 重新校验，序号先查是否不小于 1，枚举值按对照表转换；任何一项不对 → `Corrupt`（`bad_row`）。`proposals(_, open_only = true)` 在同一个快照里排除提出会话已不存在或已 Removed 的 AwaitingPublish（增补 4）。`checkpoint` 与 `latest_checkpoint` 读到的条目行数不等于 `checkpoints.entries` → `Corrupt`（`entries_mismatch`，`detail` 带期望与实际的条数）：缺了条目的 checkpoint 从不当成完整的快照交出去（module-rulings 的"A Removed session takes no new pins"里的兜底）。

### 5.11 `gc(keep)` 与 `gc_repo(repo, keep)`

只有写者能跑（只读 → `ReadOnly`）。daemon 在 `recover` 之后调用：先 `gc`，再对每个 repo 持该 repo 的写锁调 `gc_repo`，一次一个（增补 4 的 T13 一条）。`now` 每轮只取一次。

三条守卫（增补 4"gc guards"），两个方法都遵守：

- **(a) 有主的路径**：gc 从不删除、也不挪走被这些行点名的路径，以及它们的上级和下级：Preparing、Swapping、Unknown 的意图的 `staging`，Running 与 Failed 的尝试的 `staging` [我的决定：增补 4 写的是"非终态"；Unknown 的意图与 Failed 的尝试的暂存是现场（不变量 6），一并保护]。导出暂存（增补 6"Export staging"）：`gitio::export_tree` 把临时目录 `.export-<dst 的名字>-<nonce>/` 建在 `dst` 的父目录里，成功时把其中的 `tree/` 改名成 `dst` 再删掉它，失败时原样保留（gitio.md 的 `export_tree`）。调用方把 `dst` 放在自己意图行或尝试行点名的暂存目录之下（不是暂存目录本身），这个临时目录就是暂存目录的下级，同样受本条保护；`dst` 若就是暂存目录本身，临时目录落在暂存目录旁边，本条护不住它。没有意图行的缓存填充：基线 generation 在 repo 写锁之外建（module-rulings 的"Base generations are built outside the lock"），建造中的暂存 `bases/<repo>/.staging/<hex>` 靠守卫 (b) 的年龄下限保护，年龄按 B 自己的 mtime 与 ctime 算，也就是它的直接条目最后一次变动的时刻。session 在清单建好之后马上登记，从不跨过依赖安装或别的长步骤持着没登记的暂存，所以下限只要盖过导出加清单（module-rulings 的"Base staging is registered right after its manifest"）；过了下限的由 `gc_repo` 第 5 步挪进 trash，暂存里中断的导出留下的临时目录随它一起离开（缓存填充的残留；gitio 的"中断的导出保留临时目录"只管导出到对外可见的目的地）。登记（把其中的 `tree/` 改名进 `base_dir` 再 `record_base_gen`）在 repo 写锁里，`gc_repo` 持着同一把锁，所以登记从不和清扫交错。清扫只改名、不就地删：一个过了下限还在跑的建造者，登记时找不到自己的暂存（`NoSuchPath`），不会登记一棵删了一半的树；session 把登记或 `retire_dir(B)` 得到的 `NoSuchPath` 当作"已被收走"，重建一次，再落空就以 `Busy` 拒绝（module-rulings 的同一条）。
- **(b) 年龄**：没有行的东西（`store/tmp/` 的条目、没有行的 blob 文件、活路径上没有登记的目录、基线暂存）以及要删的 blob 文件，只有年龄超过下限 1 小时才动；年龄按 mtime 与 ctime 中较晚的那个算（`Attrs::last_changed`）。只看 mtime 是错的：clonefile 复制源文件的 mtime，改名只更新 ctime（2026-10-22 实测：克隆体带着 2020 年的 mtime、ctime 是当下；目录改名之后 mtime 不变、ctime 更新）。trash 的条目按名字里的时刻（改名进来的时间）算。下限与 `keep` 无关 [我的决定]；测试用 `Seam::AgeFloor` 改小它。
- **(c) 有引用的 generation**：登记过、仍被引用的基线 generation 与依赖集不删（下面的保留集）。

删行一律分批：每批一个写事务，至多 10 000 行 [我的决定]（测试用 `Seam::GcBatchRows` 改小），先删子行再删父行（schema 没有级联，§4.3）；每批开始时打开 `strata_gc_active`，提交后关掉；批与批之间放开 L2，checkpoint 照常进行。每批在自己的写事务里重新核对（裁定 T6）：这一批要删的每一行，第 1 步的保留条件按事务里读到的当前行重算一次，仍然可删才删。第 1 步的快照之后才写下的引用因此在删之前就被看到。`gc_repo` 这边，不持 repo 写锁也能写下钉子的只有 `create_session`（session 遇到 `InvalidInput` 会重试）与 deps 在 `observe_main` 和落地之外的提议写（例如会话的草稿提议）；`begin_move`、`set_deps_main` 与 `promote_proposal` 都在 repo 写锁里跑，`gc_repo` 持着同一把锁，它们不会和它交错。`gc` 这边，R 里的会话都已 Removed：往会话里添行的写（`record_checkpoint`、`begin_move`、`record_seeds`）点名 Removed 的会话都被拒绝，唯一能钉住会话的 `record_publish` 也返回 `NoSuchSession`（module-rulings 的"A Removed session takes no new pins"），所以 R 在快照之后既不会多出行，也不会多出钉子：一个会话的行跨几批删（先条目、再 checkpoint 行……），没有哪个发布能落在两批之间、钉住一个条目已经删掉的会话。重新核对留着兜底：重算之后被钉住的行这一轮不删，它的目录也不挪，下一轮从头算；测试用 `Seam::GcPauseBeforeRows` 停在第一批之前、`Seam::GcPauseAfterBatch` 停在两批之间，在那里写下这样的引用。批提交之后才写下的引用不会指向已删的行：`create_session`、`begin_move` 与 `record_checkpoint` 都要求基线已登记，否则 `InvalidInput`；提议的 `to_set` 与 `deps_main` 不查登记，指向的依赖集若已回收，由 deps 按 deps.md 处理（`deps_set` 读到 `None`）。锁的顺序见本节开头：repo 写锁（`gc_repo`，daemon 持有）→ L3（只在 blob 与 tmp 那几段）→ L2（每批）。

`gc(keep)`：

1. **保留集**，在一个读快照里算：
   - 可回收的会话 R：`lifecycle = 'removed'` 且 `removed_ns < now − keep`，并且没有阶段为 Running 或 Unknown 的发布在 `publish_items` 里点名它（S9）。阶段为 Done 的发布不钉任何东西 [我的决定]：Done 之后，发布的内容已经在 git 里（commit 与 `refs_audit` 记着），store 里没有它还要的东西；Running 与 Unknown 的发布是现场，钉住它的会话。design §7 把"发布记录"列为引用之一，这一处由 T15 按增补 4 的 T15 一条改写。
   - 可删的提议 D：Applied 或 Superseded、且 `updated_ns < now − keep` 的；以及提出会话属于 R 或已经没有行的 AwaitingPublish（增补 4：随会话一起删）。再做闭包：只要 D 里某条还被 D 外的行经 `superseded_by` 引用，就把它移出 D，直到不动为止。
   - 可删的尝试：Sealed、Refused 且 `ended_ns < now − keep` 的；Failed 且 `ended_ns < now − keep`、而它的 `staging` 已经不在的 [我的决定：暂存还在的 Failed 是现场]。
   - 可删的 gc 记录：`ended_ns < now − keep`、而且不是本 scope 最新的那一行 [我的决定：`last_gc_report` 只要每个 scope 最新的一行]。
2. **删行**，分批：R 的 `overlay_entries` → `checkpoints` → `seeds` → `move_intents` → `workspaces` → `sessions`；然后 D；然后尝试；然后 gc 记录。D 按引用的顺序删，从不按号：一条提议只在已经没有未删的行经 `superseded_by` 指向它时才删（指向它的行要么已经删了，要么在同一批里），所以每批先挑 D 里没有被剩下的行指向的那些，一条取代链从被取代的一头删起。号的大小说明不了方向：`promote_proposal` 之后，号大的 Pending 可以被号小的行取代，号小的行之后又可以被更新的行取代。同一批里互相引用的行一起删没有问题，外键在 COMMIT 时才检查（延迟外键）。
3. **blob**，持 L3 独占锁，每批至多 10 000 个：(i) 没有 overlay 条目引用、文件已经不在或年龄超过下限的 `blobs` 行：先删行并提交，再删文件；外键保证被引用的 blob 行删不掉（3a:600）。(ii) 清扫 `blobs/<aa>/`：没有行、年龄超过下限的文件删掉，它们是 put 之后、`record_checkpoint` 之前崩溃留下的孤儿（3a:293）。持独占锁时没有进行中的 put、get 与 `record_checkpoint`，所以一个刚 put、马上要被登记的 blob 不会在这一段里消失；刚 put 完的 blob 又因为年龄（ctime）太小而留着。
4. **tmp**，持 L3 独占锁：`store/tmp/` 里年龄超过下限的条目删掉（死掉的 put 的克隆体、建库剩下的临时库与探测文件）。每个条目一次 `apfs::remove_tree_rooted(<root>, store/tmp/<名字>)`，不论种类（module-rulings 的"remove_tree_rooted takes any entry kind"）：put 死在第 5 步规整之前，克隆体还带着源的 `uchg`，由 apfs 在删之前清掉；store 自己从不按路径改标志（增补 8、A10）。报告没删干净，或者调用在动手之前就被拒绝，条目留在原处、记进报告的 `stuck`，下一轮再删。
5. **删 trash**：名字里的时刻早于 `now` 减去年龄下限（缺省 1 小时，与守卫 (b) 同一个值）的条目逐个删 [我的决定：上一版是过了 `keep` 才删；进 trash 的目录都已经没有活记录，留 1 小时，让还在按路径读它的进程（例如对旧树的扫描）有时间读完，也给人一个找回的窗口]。条目用 `apfs::list_dir` 列出，每个条目一次 `apfs::remove_tree_rooted(<root>, store/trash/<名字>)`（增补 8；不论种类，module-rulings 的"remove_tree_rooted takes any entry kind"）。目录自底向上删整棵树：树里的符号链接只删链接本身、不跟随，不跨设备，删之前清掉 `UF_IMMUTABLE` 与 `UF_APPEND`（封存若用了 `chflags uchg`，克隆会把它带过来）、让目录对属主可写（deps 封存的根是 0555，deps.md §4.6）。不是目录的条目（`gc_repo` 第 2 步只挪走、没有准备的符号链接或文件）只删它本身，符号链接从不跟随。返回的报告没删干净（`is_complete()` 为假），或者调用在动手之前就被拒绝，条目都留在 trash 里、路径记进报告的 `stuck`，不让整轮失败，下一轮再删（module-rulings 的"remove_tree_rooted reports, it does not fail"：没删干净的结果是确定的，不是 `Unknown`）。删之前按 §5.11a 的引用集核对（review GS-09，2026-10-27）：行（`sessions.root`、`workspaces.path`、`deps_attempts.staging`、`move_intents.staging`）或调用方经 `gc_with(keep, extra)` 传入的路径（及其规范化形式）落在条目之内的，不删，记进报告的 `trash_refused`。`gc(keep)` 即 `gc_with(keep, &[])`。
6. **完整性扫描**，持 L3 共享锁：从上一次全局 gc 记录的 `scrub_after` 之后，按 `blobs.id` 的顺序逐个 `verify_blob`，累计读满 2 GiB 就停 [我的决定]，记下最后一个 id；走到末尾则游标回到 NULL，下一轮从头开始。不符、文件不在或长度不对的 blob 进报告的 `corrupt`：从不删除、从不覆盖（3f:557），由 status 报出、人来处理。
7. 追加一行 `gc_runs`（`scope = NULL`、`outcome = 'complete'`、报告的 JSON 与新游标），返回报告。

注（2026-10-26）：实现上 daemon 从来只调 `gc_repo`（启动、对账循环、`gc` 方法），`gc(keep)` 没有调用方，所以第 5 步从未运行，`store/trash` 只进不出。补上的是下面的 `gc_trash`，只由 daemon 的全局 `gc` 方法调用。2026-10-27（review GS-09）：`gc_with` 由 daemon 的 `gc` 方法在 `store = true` 时调用（`git-strata gc --store`，保留期 30 天，daemon.md D33），从不自动跑；先用 §5.11b 的 `gc_preview` 看会删什么。

### 5.11b `gc_preview(keep, extra)`

只读，不要求写者，不拿 `gc_running`、不拿 L3、不写 `gc_runs`。在一个读快照里算第 1 步的保留集（与 `gc` 共用同一个函数 `retention`），返回 `GcPreview { keep_secs, floor_secs, sessions, proposals, attempts, gc_runs, blobs, blob_bytes, trash_eligible, trash_young, trash_refused }`：`sessions` 是 R 的 id；`blobs` 与 `blob_bytes` 是只被 R 的条目（或没有条目）引用、文件不在或年龄过了下限的 blob 行；trash 三项按 §5.11a 的规则数。数字是上限：真跑时每批还要重新核对。daemon 的 `gc` 方法在 `store = true, dry_run = true` 时用它（`git-strata gc --store --dry-run`）。测试：`gc_preview_counts_and_writes_nothing`、`gc_trash_step_spares_referenced_trash`。

### 5.11a `gc_trash(extra)`

只有写者能跑；与 `gc`、`gc_repo` 共用 `gc_running` 互斥。只删 `store/trash/` 的条目，不删行、不写 `gc_runs`。年龄按名字里的时刻（`<ms>-<hex32>`；解析不了就按 mtime 与 ctime 中较晚的）算，下限与守卫 (b) 相同（1 小时，`Seam::AgeFloor` 可改小）；不到下限的计入 `young`。引用集：`sessions.root`、`workspaces.path`、`deps_attempts.staging`、`move_intents.staging`（全部行，不论状态），加上调用方传入的 `extra`（daemon 传 git 的 alternates 与 worktree 链接），以及它们规范化（解析符号链接）之后的形式。任何引用落在条目之内（等于条目或是它的下级），条目都不删，记进 `refused`。其余条目一次 `apfs::remove_tree_rooted`，没删干净进 `stuck`。返回 `TrashReport { removed, young, refused, stuck }`。测试：`gc_trash_removes_only_old_unreferenced_trash`。

`gc_repo(repo, keep)`，调用方持有该 repo 的写锁：

1. **保留集**，在一个读快照里算：
   - 被钉住的基线 generation：任何工作区行引用的；任何留着的 checkpoint 行引用的，包括 removed 而还没过 keep 的会话的 checkpoint（S10；增补 5：每个留着的 checkpoint 的 `BaseGen` 都算引用，跨基线的 restore 因此总能找到目标基线目录，design §7）；Preparing、Swapping、Unknown 的意图的目标 (to_tree, to_deps)；该 repo 里 `created_ns` 最新的那一个 [我的决定：下一个 `new` 多半要用它，回收掉就得重新导出、重新认领依赖]。`gc` 先跑过，已经回收的会话不再钉住任何东西。
   - 被钉住的依赖集：留下的基线 generation 引用的；Pending 提议的 `to_set`；提出会话还有行的 AwaitingPublish 提议的 `to_set`；`deps_main.set_id`（deps.md §4.6）。`from_set` 不钉：仍钉着它的会话已经由工作区行保住了。
2. **候选与准备**：没被钉住、且 `created_ns < now − keep` 的基线 generation 是候选。改名进 trash 之前先准备每个候选的根目录（增补 6，同 `retire_dir` 第 3 步）：`apfs::open_rooted(<root>, 相对路径)`，不存在（`NoSuchPath`）就只删行；是目录就把这个句柄交给 `apfs::make_removable`（清掉 `UF_IMMUTABLE` 与 `UF_APPEND`，再给属主补上写权限）；最后一级不是目录（例如被换成了符号链接，apfs 不打开它）就不准备，改名只挪这个条目本身；上级里有符号链接（`NotDirectory`，`reason = symlink`）算准备失败。准备失败的候选这一轮不回收：行与目录都留着，路径记进报告的 `stuck`，下一轮再试；这不算中途失败。基线这一步做完，再定依赖集的候选：没被钉住（留下的基线 generation，含准备失败的，引用的仍被钉住）、且 `sealed_ns < now − keep` 的依赖集，同样先准备根目录（deps 封存的根是 0555，deps.md §4.6）。
3. **删行**，分批，只删第 2 步准备好了的：基线 generation 行；然后已没有 generation 引用、且 `created_ns < now − keep` 的基线清单（先删 `manifest_entries` 再删 `base_trees`）；然后依赖集行。每批照上面的规则在事务里重新核对第 1 步的钉子：快照之后有工作区、checkpoint 或意图指向的 generation，有 Pending 或 AwaitingPublish 提议的 `to_set`、`deps_main` 或留下的 generation 指向的依赖集，这一轮不删行，第 4 步也不挪它的目录；它的根目录已经准备过，照 §6.3 留着可写，不还原。
4. **挪目录**：第 3 步删了行的每个目录用 `apfs::rename_rooted(<root>, 相对路径, store/trash/<now 毫秒>-<随机>, RenameMode::Excl)` 改名进 trash，再对 `store/trash/` 的句柄做 `apfs::full_fsync`（只同步 trash 一侧，理由同 §5.5 第 4 步：掉电之后目录若还在原路径，它就是一个没有登记的目录，第 5 步在下一轮收走）；目录已经不在（`NoSuchPath`）就跳过。先准备、再删行、再改名：准备失败时行没删，不会有目录因为权限留在活路径上而行已经没了。准备之后改名仍然失败（例如 I/O 错误），或在删行与改名之间崩溃，留下的是一个准备好了、没有登记的目录：第 5 步在下一轮过了年龄下限收走它（准备时的 chmod 刷新了它的 ctime）。
5. **清扫活路径**：用 `apfs::list_dir` 逐级列出 `bases/`、`bases/<repo>/`、`bases/<repo>/<tree>/`、`bases/<repo>/.staging/`、`deps/`、`deps/<repo>/`（每项带 `Attrs`，不跟随符号链接）。形状是 `bases/<repo>/<tree>/<deps>` 或 `deps/<repo>/<deps>`、每一级都能解析成对应的键、却没有登记、年龄超过下限、又不受守卫 (a) 保护的目录 → 照第 2 步准备（失败记进 `stuck`），再照第 4 步改名进 trash；之后变空的 `bases/<repo>/<tree>/` 用 `apfs::remove_rooted(<root>, 相对路径, RemoveKind::EmptyDir)` 删掉，不空就留着。基线暂存同样清扫：`bases/<repo>/.staging/<hex>`（`retire_dir` 第 7 行的形状）、年龄超过下限、又不受守卫 (a) 保护的目录 → 照第 2 步准备，再照第 4 步改名进 trash；`bases/<repo>/.staging/` 本身从不删（建造者在锁外往里建新的暂存）。别的名字解析不成键的条目（`deps/<repo>/.staging`、`.locks`、gitio 导出留下的 `.export-*` 临时目录、别的模块以 `.` 开头的临时目录）一概不进，符号链接一律不进；`<root>/sessions/` 从来不进（裁定 T5：中断的导出保留它的临时目录，由 status 报出）。
6. 追加一行 `gc_runs`（`scope = repo`），返回报告。

为什么分 repo、为什么持锁：session 与 deps 都在该 repo 的写锁里把目录改名进 `bases/`、`deps/` 并登记，`gc_repo` 在同一把锁里做决定、挪目录，两者不会交错（增补 4）；年龄守卫是第二道防线。基线在锁外建，建造中的暂存只有年龄守卫护着（守卫 (a) 末尾）。

中途失败：任何一步 I/O 失败就停下。已提交的批次有效，已经挪进 trash 的目录留在 trash 里；store 尽力追加一行 `outcome = 'partial'` 的 `gc_runs`（报告里带 `error`），然后返回 `Unknown`（`outcome = "partial"`）。写者已中毒或库本身写不进去时，就只有返回值。daemon 把返回的错误留在内存里并在 status 里报出；重启之后 `last_gc_report` 读得到记下的那一轮（M9）。下一轮从头算保留集、接着做，不回滚、不重放已删的东西。

### 5.12 复杂度与验收数字

| 操作 | 复杂度 | 说明 |
| --- | --- | --- |
| `put_blob` | O(文件大小) 的哈希加常数次系统调用 | 新内容两次 `F_BARRIERFSYNC`（文件与扇出目录）；重复内容只多一次克隆、一次哈希、一次 `lstat`，没有屏障 |
| `put_blob_rooted` | 同 `put_blob`，另加两次 O(深度) 的逐级打开（`attrs_of` 与 `clone_from_rooted` 各一次） | 每一级一次 `openat`；符号链接不克隆，直接存目标文本 |
| `get_blob` | O(1) | 零拷贝克隆，不读内容；一次只读查询 |
| `get_blob_rooted` | O(dst 的深度) | 同 `get_blob`，另加两次逐级打开（`clone_file_rooted` 与 `open_rooted_file` 各一次） |
| `record_checkpoint` | O(条目数) 次插入、O(不同 blob 数) 次 `lstat`（L2 之外）加 1 次 `F_FULLFSYNC` | L2 只持几毫秒 |
| `record_base_gen` | O(清单条目数) | 一个事务；tool 量级（约 1 万个文件）几十毫秒，只在建新基线时 |
| `checkpoint` 读 | O(条目数) | 一个读事务 |
| `gc`、`gc_repo` | O(行数 + blob 文件数 + 扫描预算) | 分批，不长时间挡住写；blob 与 tmp 两段持 L3 独占锁，每批至多 10 000 个文件 |
| `open`（已有的库） | O(schema)；上次没有正常关闭时加 `quick_check` 的 O(库大小) | 只在 daemon 启动时 |
| `quick_check()` | O(库大小) | 按需 |

design §8 的验收数字里与 store 有关的是"checkpoint 改 50 个文件 < 200 ms"。store 的份额是 50 次 put（每次一次克隆、一次哈希、两个屏障）加一次 `record_checkpoint`（一次 `F_FULLFSYNC`）。屏障与 `F_FULLFSYNC` 是主要成本，在 USB SSD 上更慢，首个 spike 要实测这个数字。超出预算时回到用户那里决定，store 不悄悄去掉屏障：它们是断电时"行持久则 blob 持久"的唯一保证。put 是线程安全的，overlay 可以并行 put 多个文件。"新建会话 < 5 s"里 store 的份额只是几次登记和一个 `create_session` 事务，毫秒级。

**store.db 会长多大**（估算，首个 spike 实测）：checkpoint 是完整快照（types.md `Checkpoint`），每个 checkpoint 把整个 overlay 写一遍。`overlay_entries` 一行连同它的主键索引与 `by_blob` 索引约 300 字节（会话 id 10 字节、path 约 40 字节、blob 的 64 位十六进制两份）。一个活跃会话每天约 1 000 次工具调用、overlay 平均 50 个文件，就是每天 5 万行、约 15 MB；五个这样的会话，加上保留期内已删除会话的行，store.db 大约在 0.5 到 2 GB 之间。`quick_check` 读整个库：按 USB SSD 约 400 MB/s 估，2 GB 约 5 秒；按 T6 的裁定（fix round 1 之后），它只在上次没有正常关闭时和按需时跑（§5.2），不加在每次启动上。剩下的 spike 项 [我的决定]：首个 spike 用真实会话记录库的增长速度；一周增长超过 1 GB，就回到用户决定要不要把条目按内容去重（`overlay_entries` 拆成去重的条目表，加上每个 checkpoint 到条目的成员表，一条成员行约 20 字节）。去重要升 `user_version`，所以要在第一次发布之前定。

## 6. 失败与恢复

### 6.1 store 返回的码

"types.md 已列"一栏说的是 types.md §6.2"谁返回"里有没有 store。写"否"的几项，是本文把 store 的行为映射到最接近的已有码；增补 4 的 T15 一条让 Task 15 在 types.md 的"谁返回"里给它们补上 store，码的集合不变（仍是 39 个）。

| 码 | 何时（函数） | 留下的现场 | 恢复 | types.md 已列 |
| --- | --- | --- | --- | --- |
| `CrossVolume` | open：root 与某个 checkout 不同卷；put、get（含两个 rooted 变体）由 apfs 透传 | 无 | 改 `[paths] root` | 是 |
| `NotEmpty` | open 建库时 root 里有不属于 strata 的条目 | 无 | 换一个 root，或清空它 | 是 |
| `Corrupt` | open：文件头、身份、版本、日志模式、schema、quick_check（上次没有正常关闭时）不对，或丢了 store.db；`quick_check()`：结果不是 ok；读：标识、序号或枚举值不合法，`checkpoint`、`latest_checkpoint` 读到的条目行数与 `checkpoints.entries` 不符；blob：文件不在、不是普通文件、长度与登记行不符、重算的哈希不符（只有 `verify_blob` 与 gc 扫描重算）；finish_move 或 resolve_move 的 Accept 找不到意图的目标基线 | 一字不动；put 的长度不符会留下正确的 tmp | 人工检查（status 报出）；store 从不自动修补 | 是 |
| `Busy` | open：另一个写者持有 LOCK；任何写：外部进程持有 SQLite 写锁 | 无，`outcome = "not_written"` | 稍后重试 | 否（types.md 只列了 daemon 的按 repo 写锁） |
| `ReadOnly` | 只读 Store 上的任何写；只读打开时需要 WAL 恢复；get、`get_blob_rooted` 的 dst 所在目录不可写；retire_dir 的准备或改名得到 `EPERM`、`EACCES` | 无 | 启动 daemon；修正目标 | 否（列的是 cli 与写封存目录的模块） |
| `Unsupported` | open：卷不支持 clonefile（apfs 透传）；PRAGMA 回读不符 | 无；建库探测失败时删掉自己刚建的空目录 | 换 APFS 卷；换支持这些设置的 SQLite | 否（列的是 apfs、gitio、confirm；回读不符这一项是新增） |
| `NoSuchPath` | open：checkout 不存在；open_read_only：还没有 Store；put、`put_blob_rooted`：src 不存在；get、`get_blob_rooted`：dst 的上级不存在；retire_dir：路径不存在 | 无 | 修正路径 | 否 |
| `NotDirectory` | get：dst 的某一级上级不是目录（apfs 透传）；`get_blob_rooted` 的 dst、`put_blob_rooted` 的 src：某一级上级不是目录或是符号链接（apfs 透传，`reason = "symlink"`，什么都没写）；retire_dir：路径不是目录 | 无 | 修正路径 | 否（列的是 apfs、session、deps） |
| `Exists` | get、`get_blob_rooted`：dst 已存在（含悬空符号链接）；create_session：会话 id、root 或工作区路径已有行；begin_deps_attempt：同一依赖集已有 Running 的尝试；record_deps_set：依赖集已登记；begin_move：该工作区已有没结束的意图 | 无 | 换目标；读出已有的那条再决定 | 否（列的是 apfs、session） |
| `IsDirectory` | put：src 是目录（apfs 透传）；`put_blob_rooted`：src 是目录，或检查之后被换成目录（apfs 按 `CloneKind::File` 在克隆之前拒绝） | 无 | — | 是 |
| `NotFile` | put、`put_blob_rooted`：src 是 FIFO、socket 或设备文件（apfs 透传）；`put_blob_rooted` 的 src 在检查之后被换成这些或符号链接（apfs 在克隆之前拒绝） | 无 | — | 透传 |
| `NoSuchCheckpoint` | `checkpoint` 读；record_publish 的批次点名了不存在的 checkpoint | 无 | — | 是 |
| `NoSuchSession` | 写操作点名的会话不存在或已 Removed（含 `record_publish` 的批次，module-rulings 的"A Removed session takes no new pins"）；set_lifecycle、set_session_phase、record_seeds 的会话不存在 | 无 | — | 否（列的是 session） |
| `NoSuchRepo` | 会话里没有这个 repo 的工作区 | 无 | — | 否 |
| `InvalidInput` | root 不是绝对路径；条目不一致（含同一路径的 Deleted 加 Added）；工作区 tracked = false 或健康为 Unknown；基线、清单、依赖集没登记或属于别的 repo；与已登记的清单不同；不允许的状态转换；点名的尝试、提议、意图不存在；`set_move_state` 的 conflicts 与 to 不配；`finish_move` 的冲突与意图行上的不同；`resolve_move` 的守卫不过；retire_dir 的路径不合规或对应的行不允许；种子写第二次；路径不是 UTF-8；时间超出范围 | 无 | 调用方的错 | 否（列的是 types、cli、daemon） |
| `TooLarge` | 序号或代数到了 `i64::MAX`；read_blob 的大小超过 max | 无 | — | 否（列的是 publish、search、daemon） |
| `Unknown` | types.md §6.3 归为 `Unknown` 的 I/O 错误（`ENOSPC`、`EIO` 等）；提交结果不确定；写者中毒；gc 中途停下；get 与 `get_blob_rooted` 克隆之后规整不成（dst 被换掉或有别的名字，`outcome = "partial"`）；触发器拦下了 store 自己发出的语句（store 的 bug） | 看 `outcome` | §6.4 | 是（任何模块） |

### 6.2 `detail` 的形状

types.md 把 `detail` 的形状交给返回它的模块。store 的 `detail` 都是 JSON 对象，不适用的键省略：

- I/O 类（按 types.md §6.3 归类的错误；`io::Error` 一律经 `apfs::classify_io_error` 归类，store 再补上 `op` 与 `outcome`）：`{"errno": 28, "path": "...", "op": "commit", "outcome": "not_written"}`。
- `outcome` 是本文规定的"写到哪一步"标记（3c:30 要求由 store.md 规定）：
  - `"not_written"`：没有任何外部可见的改变。事务回滚了或根本没开始，也没有文件改名进 store。可以直接重试，没有现场要保留。
  - `"uncertain"`：一次 `COMMIT` 的结果不确定（提交时同步或写失败）。那一行在恢复之后可能在、也可能不在。写者中毒（§6.4），status 报出，不重放。
  - `"partial"`：一个多步操作停在中间，已完成的步骤有效（gc 已提交的批次、已挪进 trash 的目录、get 已建出的 dst）。现场保留，status 报出。
  - 同样是 `ENOSPC`：写临时文件时满了是 `not_written`，提交时满了是 `uncertain`。两者都是 `Unknown`，调用方按 `outcome` 区分，永远不把不确定的结果当成确定的容量失败去重放或清理（3c:30）。
- `Corrupt`：`{"reason": "...", "blob": "...", "expected": "...", "actual": "...", "path": "..."}`。`reason` 取 `application_id`、`user_version`（含比 `LATEST` 新的库）、`journal_mode`、`schema`、`sqlite`、`store_db_missing`、`bad_row`、`entries_mismatch`、`blob_missing`、`blob_type`、`blob_size`、`blob_mismatch`、`base_gen_missing` 之一。
- `Busy`：`{"lock": "...", "path": "..."}`，`lock` 是 `store-writer`（LOCK）或 `sqlite`（SQLite 写锁）。迁移前折叠不了 WAL 时是 `{"reason": "migration_backup", "op": "migrate_backup", "outcome": "not_written", "checkpoint": "...", "wal_bytes": n}`（§4.4a）。
- 迁移（§4.4a）：`op` 是 `migrate` 或 `migrate_backup`；备份核对失败是 `Unknown`（`migration_backup`，`not_written`，带 `path`）；提交结果不确定是 `Unknown`（`migration_commit`，`uncertain`，带 `from`、`to`、`backup`），调用方要退出进程。
- `Exists`：`{"reason": "...", "path": "...", "attempt": 7, "n": 3}`，按需取用；`reason = "store_row"` 表示撞上的是已有的行。
- `CrossVolume`：`{"root": "...", "checkout": "..."}`。`NotEmpty`：`{"root": "...", "entries": ["..."]}`。
- `TooLarge`：`{"limit": "...", "max": 9223372036854775807}`，`limit` 取 `checkpoint_id`、`proposal_id`、`attempt_id`、`release_group_id`、`move_generation` 或 `read_max`。
- `Unsupported`：`{"setting": "fullfsync", "expected": 1, "actual": 0}`；apfs 透传的保持 apfs 的形状。
- `ReadOnly`：`{"reason": "read_only_handle"}` 或 `{"reason": "recovery_needed"}`；get 的 dst 不可写时保持 apfs 的形状。
- `InvalidInput`：`{"reason": "...", "path": "...", "field": "..."}`，按需取用；`reason` 的取值有 `deps_unregistered`、`manifest_mismatch`、`symlink`、`seed_exists`、`illegal_transition`、`not_found`、`conflicts_mismatch`，以及 `resolve_move` 的 `lifecycle`、`not_failed`、`no_failed_move`（同 session.md §5.9）等。

### 6.3 崩溃会留下什么

| 崩溃发生在 | 留下 | 下一次 |
| --- | --- | --- |
| 建库第 9 步之前 | 空目录，`store/tmp/` 里的临时库 | open 从头建（§5.1）；临时库由 gc 过了年龄下限收走 |
| put：克隆之后、改名之前 | `store/tmp/` 里的克隆体（mtime 可能是源文件的旧值，ctime 是克隆的时刻；死在第 5 步之前的还带着源的 BSD 标志） | 不影响任何写入，新写入换新名字（3a:228）；gc 过了年龄下限（按 mtime 与 ctime 中较晚的那个）经 `apfs::remove_tree_rooted` 删，带着的标志由 apfs 清掉 |
| put：改名之后、`record_checkpoint` 提交之前 | `blobs/` 里一个没有行的 blob | 之后同内容的 put 复用它；没人复用就由 gc 过了下限收走（3a:293）；不存在 checkpoint 行 |
| `record_checkpoint`：提交之前 | 什么都没有，SQLite 回滚 | checkpoint 不存在；同一工具调用的重放会重新记 |
| `record_checkpoint`：提交之后、回复之前 | 完整、持久的 checkpoint | 同一工具调用的重放返回同一个号；`checkpoint_write_is_durable_after_crash` 证明 |
| session 写下 `create_session` 或 `begin_move` 的行、或进入 Removing 之后 | 这些行，以及可能建了一半的暂存或目录 | recover 按 §5.7 收尾：Preparing 的意图（没走到第一次对外可见的写）关成 Abandoned，暂存经 `retire_dir` 拿走；Swapping 的意图进入 Unknown，进入 Swapping 时记下的冲突留在意图行上，人选 Accept 时由 `resolve_move` 交给工作区（session.md 的 D46）；Creating 与 Removing 的会话进入 Unknown，现场保留（增补 3、4）。`create_session` 本身就是 `new` 的第一次对外可见的写，它之后没有"还没对外可见"的窗口，所以 Creating 的会话一律进入 Unknown（增补 6） |
| deps 的尝试还是 Running 时 daemon 死了 | Running 的行与它的暂存 | 下一个拿到该依赖集建造锁的 deps 把它关成 Failed（`interrupted`，T8 裁定）；gc 从不碰 Running 与 Failed 的暂存 |
| 目录改名进 `bases/` 或 `deps/` 之后、登记之前 | 活路径上一个没有登记的目录 | 建它的模块下一次发现没登记，就 `retire_dir` 拿走再重建；否则 `gc_repo` 过了年龄下限把它挪进 trash |
| session 建基线的中途（暂存还在） | `bases/<repo>/.staging/<hex>`，可能带着 gitio 的导出临时目录 | daemon 启动时的 `recover` 经 `retire_dir` 第 7 行拿走（session.md §5.8）；漏下的由 `gc_repo` 第 5 步过了年龄下限挪进 trash |
| `retire_dir`：改名之后、同步之前 | 掉电之后目录在原路径或在 trash 里 | 两处都合法：还在原路径就再 `retire_dir` 一次；在 trash 里就由 gc 删 |
| `gc_repo`：准备好根目录之后、删行之前（不崩溃也会留下同样的东西：第 3 步中途停下，或者删行那一批重新核对时它已被快照之后的引用钉住，§5.11） | 根目录已经可写、没有不可变标志，行还在 | 下一轮重算：没被钉住就照样回收；被钉住就留下来，根目录这一层保持可写 [我的决定：不还原；里面的文件与目录不受影响] |
| `gc_repo`：删行之后、挪目录之前 | 活路径上一个准备好了、没有登记的目录 | 下一轮 `gc_repo` 第 5 步过了年龄下限收走 |
| gc：删 trash 中途 | trash 里删了一半的目录 | 下一轮接着删 |
| 进程持着 LOCK 与 SQLite 写锁时被杀 | 没有陈旧的锁：flock 与 SQLite 的 POSIX 锁都随进程消失由内核释放；LOCK 以 `O_CLOEXEC` 打开，子进程手里没有它 | 下一个写者立刻能打开、能写（3a:692）；WAL 恢复由 SQLite 做 |

### 6.4 `Unknown` 之后

按不变量 6 与错误时机的裁定（增补 3、4）：store 只在写任何东西之前给出类型化的拒绝；写开始之后的失败一律 `Unknown`，现场保留，不清理，不回滚已经持久的东西，由 status 报出。移动意图与依赖集尝试只会被关进终态，会话出错时进入 Unknown；它们都不因为失败而被删除，只由 gc 回收（增补 4"Error timing, refinement 2"）。会话行也不是内部的意图行：`create_session` 就是 `new` 的第一次对外可见的写（增补 6）。

- 增补 5（错误时机，第 3 条细化）：操作动手之前对当前、未改动的状态记下的 Before* 安全 checkpoint（BeforeRestore、BeforeRollout、BeforeRemove 等）不算对外可见的写，之后操作仍可给出类型化的拒绝，checkpoint 留着，是一个有效的恢复点。store 不区分这类 checkpoint：它们是普通的 checkpoint 行，只由 gc 按保留期回收，从不因为后面的拒绝而删除。
- 增补 6（错误时机，第 4 条细化）：建出并登记一个以后任何操作都会复用的、不可变、按内容寻址的 generation（基线 generation、封存的依赖集、blob）是缓存填充，不算对外可见的写，之后操作仍可给出类型化的拒绝。store 这一侧：`record_base_gen`、`record_deps_set` 与 put 写下的东西不因为后面的拒绝而撤销，只由 gc 回收。

- `outcome = "uncertain"` 的提交之后，写者中毒：此后每个写函数返回 `Unknown`（`reason = "poisoned"`），`is_poisoned()` 返回 true，直到 daemon 丢掉这个 `Store` 重新 `open` [我的决定]。daemon 的做法是退出进程让 launchd 重启（daemon D31）：Turso 每个进程每个文件只留一个 `Database`，进程内重开不会重跑 WAL 恢复。daemon 把 `is_poisoned()` 并进 status（增补 4 的 T13 一条）。重开时 SQLite 的 WAL 恢复给出真实结果：checkpoint 行在，就是提交了；不在，就是没提交，钩子的重放会重新记。中毒期间读照常工作。理由：同步失败之后继续写，可能把其实没落盘的状态当成已提交（PostgreSQL 在 fsync 失败后也不再继续）。
- gc 的 `partial`：`gc` 或 `gc_repo` 返回 `Unknown`（`outcome = "partial"`），store 尽力把这一轮写进 `gc_runs`（`outcome = 'partial'`，报告里带 `error`）。daemon 把返回的错误留在内存里并在 status 里报出；重开之后 `last_gc_report` 读得到写进库的那一轮。不需要人处理，下一轮会接着做；`stuck` 里的条目要人看。
- `Corrupt` 从不自动处理：status 报出它（gc 报告里的 `corrupt` 也由 status 报出），由人决定。

## 7. 测试清单

带 `#[ignore]` 的测试需要 `hdiutil` 建的磁盘映像（不需要 root），用 `cargo test -p strata-store --features test-seams -- --ignored` 跑。子进程测试照 LayerFS 的做法：父测试以 `--exact <子测试名>` 重新执行本测试二进制并设环境变量，子测试函数在环境变量不在时直接返回。测试用到的 `Seam`、`SyncCounts` 只在 `test-seams` 下编译（§3、§8）。store 的 `test-seams` 同时打开 `strata-apfs/test-seams`：要在 apfs 的两次系统调用之间换掉目录的测试用 apfs 的 `seams::set_before_open`。

| 测试名 | 断言 | 来源（矩阵行 或 新增） |
| --- | --- | --- |
| `open_creates_layout_on_empty_root` | 空的或不存在的 root 上，`open` 建出 §4.1 的布局：256 个扇出目录，`store.db` 的文件头 `application_id = 1398035009`、`user_version = 1`、WAL；`close` 之后再 `open`，除了删掉 `store/clean-shutdown` 之外不改任何文件 | 新增 |
| `interrupted_creation_is_resumed` | 用 `InitBeforeRename` 让建库在改名之前中止：root 下只有空目录和 `store/tmp/` 里的临时库；再次 `open` 成功建库，不报 `NotEmpty` | 新增 |
| `open_refuses_nonempty_foreign_root` | root 里有一个别的文件：`open` 返回 `NotEmpty`，`detail.entries` 列出它，root 下没有多出任何东西 | 新增 |
| `open_refuses_lost_store_db` | 建好的 Store 删掉 `store.db`，`sessions/` 里还有会话目录：`open` 返回 `Corrupt`（`store_db_missing`），不建新库 | 新增 |
| `open_refuses_cross_volume_root` | root 在 `hdiutil` 建的另一个 APFS 卷上：`open` 在写任何东西之前返回 `CrossVolume`，两个卷上都没有新文件（`#[ignore]`） | layerfs-project/tests/init_sqlite.rs::an_unsupported_platform_refuses_the_store_before_any_file_exists |
| `open_refuses_volume_without_clonefile` | root 与 checkout 同在一个 FAT 磁盘映像上：`open` 返回 `Unsupported`，本次建的空目录被删掉，没有净写入（`#[ignore]`） | layerfs-project/tests/init_sqlite.rs::an_unsupported_platform_refuses_the_store_before_any_file_exists |
| `open_pins_durable_pragmas` | 写连接回读 `journal_mode = wal`、`synchronous = 2`、`fullfsync = 1`、`checkpoint_fullfsync = 1`、`foreign_keys = 1`、`recursive_triggers = 1`、`trusted_schema = 0`、`busy_timeout = 0`；读连接 `query_only = 1`、`trusted_schema = 0`。strata 只有这一种持久配置，没有 Disposable 档。这是 `checkpoint_write_is_durable_after_crash` 的前置 | layerfs-persistence/tests/sqlite_publication.rs::wal_full_settings_and_unavailable_engine_are_explicit<br>layerfs-persistence/tests/serverless_store.rs::disposable_create_publish_seal_and_reopen_all_layouts<br>layerfs-daemon/tests/r5_10_producer_profile.rs::r5_10_each_mounted_commit_has_one_producer_on_the_one_open_disposable_writer_session |
| `open_refuses_non_wal_store` | 用原始连接把库改成 `journal_mode = delete`：`open` 返回 `Corrupt`（`journal_mode`），文件字节不变，日志模式仍是 delete | layerfs-persistence/tests/serverless_store.rs::non_wal_store_is_refused_without_conversion |
| `open_refuses_schema_drift` | 分别删一张表、去掉一个 CHECK 约束、改一列的类型、删一个触发器：`open` 都返回 `Corrupt`（`schema`），文件字节不变，不修补 | layerfs-persistence/tests/history_remediation.rs::reopen_refuses_each_independent_schema_or_binding_corruption<br>layerfs-persistence/tests/old_format_open.rs::a_store_with_any_other_schema_difference_is_still_refused<br>layerfs-project/tests/init_sqlite.rs::a_store_without_acquisition_tables_refuses_init |
| `open_refuses_unknown_user_version` | `user_version` 改成 0 和 3：`open` 都返回 `Corrupt`（`user_version`），不迁移；版本策略见 §4.4 | layerfs-persistence/tests/sqlite_inode_values.rs::a_store_of_the_value_group_format_is_refused_at_open<br>layerfs-persistence/tests/old_format_open.rs::an_old_format_store_opens_and_records_hints_from_then_on |
| `open_leaves_unrelated_database_untouched` | `store.db` 的位置上放一个 delete 日志模式的无关 SQLite 库：`open` 返回 `Corrupt`（`application_id`），文件字节与日志模式不变，没有产生 -wal 或 -shm | layerfs-persistence/tests/sqlite_publication.rs::writable_open_does_not_migrate_an_unrelated_database_journal |
| `second_writer_open_is_busy` | 已有一个写者 `Store`（同一进程里，以及子进程里）时，第二个 `open` 返回 `Busy`（`store-writer`），什么都不写；`open_read_only` 照常成功：Store 只有 daemon 一个写者，cli 只读，没有直连写路径 | layerfs-store-service/tests/kernel_guard.rs::direct_access_is_unsupported_off_linux<br>layerfs-store-service/tests/kernel_guard.rs::direct_access_is_unsupported_on_a_filesystem_outside_the_allowlist |
| `read_only_open_refuses_every_write` | `open_read_only` 之后取 store 目录的文件列表与字节快照（打开时 SQLite 可能建出 -wal 与 -shm，所以快照在打开之后取，§5.2）；之后每个写函数（put、`record_checkpoint`、`create_session`、`begin_deps_attempt`、`retire_dir`、gc 等）都返回 `ReadOnly`，快照不变；读函数照常 | layerfs-persistence/tests/sqlite_publication.rs::readonly_refusal_and_external_writer_busy_do_not_retry |
| `quick_check_runs_only_after_unclean_exit` | 夹具用原始连接写满几页数据，`close` 之后改坏一个只属于某张表的数据页：再 `open` 成功（标记在，不跑 quick_check），标记随即被删掉，按需的 `quick_check()` 返回 `Corrupt`（`sqlite`）；不经 `close` 丢掉这个 Store 之后再 `open`，返回 `Corrupt`（`sqlite`）；只读打开既不看也不动标记 | 新增 |
| `lock_is_not_inherited_by_child_processes` | 写者 Store 起一个长时间运行的子进程（`sleep 60`）之后被丢掉：另一个 `open` 立即成功，而子进程还活着，证明 LOCK 以 `O_CLOEXEC` 打开（增补 4） | 新增 |
| `committed_rows_are_immutable` | 用注册了 `strata_gc_active`（返回 0）的原始连接：每张不可变表上的 UPDATE 都被触发器拦下（`immutable`）；`INSERT OR REPLACE` 撞到已有行时被删除守卫拦下；一批插入里有一行冲突时整批回滚，不留半批；已存的基线与依赖集行不被改写 | layerfs-persistence/tests/inode_branch_delta_custody.rs::a_branch_base_cannot_be_dropped_and_survives_reclamation<br>layerfs-persistence/tests/sqlite_inode_values.rs::a_stored_value_is_never_replaced_and_a_refused_batch_leaves_no_row<br>layerfs-persistence/tests/sqlite_publication.rs::failure_after_body_insert_rolls_back_the_entire_unit_and_published_body_is_immutable<br>layerfs-workspace/tests/operation_scope.rs::scoped_views_share_original_install_serials_and_keep_original_provider |
| `delete_outside_gc_is_refused` | gc 以外的 DELETE（store 的原始连接、没有注册函数的 `sqlite3` 连接）都失败，行不变；gc 模式下先删子行再删父行成功，父行还有子行时被外键拦下（schema 没有级联）；`publishes`、`publish_items`、`refs_audit` 连 gc 也删不掉 | 新增 |
| `state_graphs_are_enforced_by_triggers` | 用注册了 `strata_gc_active` 的原始连接逐表试 §4.3 的转换图：会话生命周期、工作区健康、意图、尝试、提议、发布阶段的每条允许的边都成功，其余每条都被触发器拦下（`illegal update`），行不变；意图的 n 不大于工作区代数或已有的 n 时插入被拦下；工作区的 base、head、代数只能换到它 n 最大的意图的 n 上；健康 Unknown → Ok 只在会话 Active、它 n 最大的意图是 Unknown 时放行，现场只在进入 Unknown 时改写；意图 Unknown → Done、Abandoned 只在会话 Active、它是 n 最大的意图、工作区已经回到 Ok 时放行（Done 要工作区的代数已是 n，Abandoned 要不是）；意图的冲突只在 Preparing → Swapping 时写一次，进入 Swapping 不带冲突被 CHECK 拦下 | 新增 |
| `blob_put_is_idempotent` | 同一内容 put 两次（同一个文件；两个不同路径的文件；一次 `put_blob` 一次 `put_bytes`）得到同一个 BlobId，`blobs/` 里只有一个文件；把已存副本改成 000 权限之后再 put 照样成功，证明不回读已存副本 | layerfs-content/tests/file_complete.rs::construction_is_deterministic_for_the_same_input<br>layerfs-content/tests/streaming.rs::root_identity_is_stable_across_repeated_streaming_runs<br>layerfs-storage/tests/exact_reuse_trust.rs::an_exact_reuse_reads_no_stored_bytes |
| `blob_id_ignores_delivery_shape` | 同样的字节，一次写成的文件、分块乱序写成的文件、经 mmap 写成的文件，以及 `put_bytes`，得到同一个 BlobId：身份只取决于内容 | layerfs-content/tests/file_complete.rs::known_length_and_streaming_inputs_agree_exactly<br>layerfs-content/tests/streaming.rs::unknown_length_and_known_length_agree_object_by_object<br>layerfs-workspace/tests/captured_namespace_fragments.rs::fragmented_writes_of_a_base_file_give_one_root |
| `blob_known_answer_vectors` | 空串的 BlobId 为 `af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262`，`abc` 的为 `6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85`（2026-10-22 用 BLAKE3 参考实现核过）；一百万个 `a` 的值写测试时用 `b3sum` 算出后固定。三者经 `put_blob` 与 `put_bytes` 都得到这些 id。原测试的 SHA-256 向量不保留 | layerfs-storage/tests/object_key.rs::sealed_keys_keep_standard_sha256_bytes_for_empty_short_and_many_blocks |
| `empty_file_is_a_valid_blob` | 0 字节的文件可以 put，id 是空串的 BLAKE3；get 回来是 0 字节的文件 | layerfs-content/tests/edit_single.rs::complete_deletion_returns_the_empty_representation |
| `stored_blob_is_never_rewritten` | 之后存任何别的内容，已存 blob 的 inode 与字节都不变；同内容再 put 只刷新它的 atime 与 mtime | layerfs-content/tests/edit_single.rs::the_base_objects_are_never_rewritten |
| `blob_identity_changes_with_any_byte` | 一份内容在首、中、尾各改一个字节，得到三个互不相同、也不同于原来的 BlobId | layerfs-content/tests/object_identity.rs::identity_of_a_damaged_canonical_object_changes |
| `blob_roundtrip_is_byte_exact` | 空、1 字节、1 MiB 的文件 put 之后、不经任何 checkpoint（`blobs` 里没有行）直接 get，字节逐一相同：get 按文件工作（增补 4"Blob protection"） | layerfs-content/tests/file_read.rs::full_read_returns_exact_bytes_for_every_representation |
| `large_and_sparse_blobs_roundtrip` | 500 000 000 字节的稠密文件，与带空洞的稀疏文件，经 put、get 字节相同（空洞读成零）；get 是零拷贝克隆，不读内容 | layerfs-daemon/tests/complete_installed_roots.rs::dense_500000000_kernel_bytes_survive_install<br>layerfs-daemon/tests/complete_installed_roots.rs::sparse_chunk_bytes_and_holes_survive_install |
| `same_bytes_new_path_or_mode_reuse_blob` | 同样字节的文件换路径、换 inode（复制一份）、改 mode 之后 put，得到同一个 blob，`blobs/` 里只有一份；差一个字节就是新 blob | layerfs-daemon/tests/file_digests.rs::a_relinked_file_with_saved_bytes_reuses_the_object<br>layerfs-project/tests/init_counters.rs::init_counts_every_entry_and_each_file_identity_once |
| `blob_files_are_content_only` | 源文件带 `com.apple.quarantine`、一个用户扩展属性、`uchg` 标志、mode 0755：blob 文件没有这些扩展属性（只剩内核托管的 `com.apple.provenance`），没有 BSD 标志，mode 0444；get 出来的文件 mode 0644、mtime 在当前时刻附近，没有拷来的扩展属性 | layerfs-workspace/tests/captured_namespace_metadata.rs::a_metadata_patch_keeps_every_other_key_and_its_value_root |
| `symlink_blob_stores_target_text` | put 一个指向 `../a/b` 的符号链接：blob 是内容为 `../a/b` 的普通文件，`read_blob` 读回目标文本；链接本身不被跟随；`put_blob_rooted` 对工作区里的同一个链接得到同一个 id，目标取自 `attrs_of`，不克隆 | 新增 |
| `put_blob_refuses_directory_and_special_files` | src 是目录 → `IsDirectory`；FIFO → `NotFile`，而且打开 FIFO 不会挂住；不存在 → `NoSuchPath`；三种情况下 `blobs/` 与 `tmp/` 都没有新条目；`put_blob_rooted` 对同样三种 src 返回同样的码，在克隆之前就拒绝 | 新增 |
| `get_blob_refuses_existing_dst` | dst 已存在：`get_blob` 返回 `Exists`，dst 的字节与 inode 不变 | 新增 |
| `get_blob_missing_is_corrupt_and_leaves_no_dst` | 删掉一个已 put 的 blob 的文件：`get_blob` 返回 `Corrupt`（`blob_missing`），dst 不存在；`read_blob` 与 `verify_blob` 同样返回 `Corrupt`（`blob_missing`） | layerfs-content/tests/file_read.rs::a_provider_failure_is_returned_once_and_stops_the_read |
| `missing_or_mismatched_blob_is_corrupt_never_new` | 文件不在、不是普通文件（换成一个目录）、长度与登记行不符（夹具用原始连接插一行 `blobs`）、内容与 id 不符（`verify_blob`）四种情况都返回 `Corrupt` 并带原因；只有文件、没有登记行时 get 与 read 照常成功；没有哪个函数返回会被当成"这是新文件"或"没变"的结果。3c:85 里"基线文件不见了"的那一半不在 store：基线文件不在 blob 存储里，由 overlay.md 与 session.md 报 `Corrupt`（增补 4"Base files"） | layerfs-workspace/tests/captured_file_edits.rs::first_provider_absence_denial_identity_and_length_mismatch_never_prove_newness |
| `get_blob_is_trusted_verify_detects_flipped_bit` | 翻转 blob 文件的一个比特（长度不变）：`get_blob` 照常克隆（受信任读，不重算）；`verify_blob` 返回 `Corrupt`（`blob_mismatch`，带期望与实际），坏字节不会以"已校验"的名义交给调用方 | layerfs-content/tests/filesystem_failure.rs::a_malformed_or_unauthenticated_demanded_page_is_rejected_without_a_root<br>layerfs-storage/tests/selected_group_reads.rs::accessed_record_corruption_fails_canonical_authentication |
| `put_reuse_does_not_reread_existing_copy` | 已存副本被改坏（长度不变）：同内容再 put 照常返回同一个 id，不回读；之后 `verify_blob` 返回 `Corrupt`（`blob_mismatch`）。gc 扫描报出它由 `gc_scrub_reports_corrupt_and_resumes_from_cursor` 证明 | layerfs-storage/tests/exact_reuse_trust.rs::a_corrupt_stored_copy_is_accepted_at_save_and_fails_at_its_next_read |
| `put_reuse_with_length_mismatch_is_corrupt` | 已存的同名 blob 被截短：同内容再 put 返回 `Corrupt`（`blob_size`），已存副本不被替换，`detail.path` 指向留在 tmp 里的正确副本 | layerfs-storage/tests/exact_reuse_trust.rs::a_role_or_length_mismatch_is_still_refused_at_save |
| `put_restores_missing_blob_file` | blob 有登记行（夹具用原始连接插入）而文件被删：同内容再 put 重新建出文件，不信任悬空的行；之后 `verify_blob` 通过 | layerfs-daemon/tests/file_digests.rs::a_recorded_root_the_store_does_not_hold_is_not_reused |
| `stale_temp_files_do_not_block_put` | `store/tmp/` 里放满旧的临时文件和无关文件，再用 `TempNameTaken` 让第一个临时名已被占用：put 换名成功，既不采用也不截断已有文件，那些文件原样留着 | layerfs-content/tests/filesystem_ordering.rs::a_stale_run_file_does_not_block_a_later_backing |
| `concurrent_puts_of_same_content_store_one_blob` | 8 个线程同时 put 同一份新内容（相当于几个会话同时 checkpoint 同一个文件）：都成功，都返回同一个 id，`blobs/` 里只有一个文件，读回完整 | layerfs-persistence/tests/concurrent_first_installs.rs::two_writers_saving_the_same_new_content_both_complete |
| `put_blob_orders_blob_before_later_writes` | `sync_counts`：每个新 blob 在 put 返回之前有一次文件屏障和一次扇出目录屏障；重复内容的 put 先看到已存的 blob，一次屏障都不做（§5.3 第 7 步） | 新增 |
| `put_rename_failure_is_not_written` | `BlobRenameFails { errno: EIO }`：put 返回 `Unknown`（`not_written`），`blobs/` 里没有新文件，克隆体留在 `store/tmp/`；之后同内容的 put 成功 | 新增 |
| `put_blob_rooted_never_follows_a_swapped_parent` | 工作区 W 里 `a/b/f` 是普通文件，W 之外的目录 X 里有内容不同的同名文件 `f`：`put_blob_rooted(W, a/b/f)` 存下 W 里的内容；把 `a/b` 换成指向 X 的符号链接之后再调，返回 `NotDirectory`（`reason = "symlink"`），`blobs/` 与 `tmp/` 都没有新条目，X 里的内容没有进 store；用 apfs 的 `seams::set_before_open` 在 `attrs_of` 之后、`clone_from_rooted` 打开 `a` 之前做同样的替换，结果相同。对照：按路径的 `put_blob(W/a/b/f)` 跟着这个符号链接存下了 X 里的内容（2026-10-22 实测的内核行为），所以工作区的捕获不用它 | 新增 |
| `put_blob_rooted_refuses_a_swapped_kind_before_cloning` | 用 apfs 的 `seams::set_before_open` 在 `attrs_of` 之后、`clone_from_rooted` 打开 src 之前，把普通文件 src 换成一个装着文件的目录：返回 `IsDirectory`，`store/tmp/` 与 `blobs/` 都没有新条目（apfs 按 `CloneKind::File` 在克隆之前拒绝，从来没有克隆树），工作区里的那个目录原样；换成没有写端的 FIFO → `NotFile`，不挂住；换成符号链接 → `NotFile`（`reason = "symlink"`） | 新增 |
| `get_blob_rooted_roundtrip_matches_get_blob` | 同一个 blob 用 `get_blob(id, P)` 与 `get_blob_rooted(id, W, a/f)` 各取一份：字节与 blob 逐一相同，两份都是 mode 0644、没有 BSD 标志、mtime 为当前时刻；blob 文件被人加上 `uchg` 时也一样（先清标志、再设 mode）；`blobs` 里没有行时同样成功。`get_blob_rooted` 的 dst 已存在（含悬空符号链接）→ `Exists`，原样，链接指向的地方没有被碰；blob 文件被删 → `Corrupt`（`blob_missing`），dst 不存在；dst 的上级不存在 → `NoSuchPath` | 新增 |
| `get_blob_rooted_never_writes_through_a_swapped_parent` | 工作区 W 里 `a/b/` 是目录，W 之外的目录 X 里有文件 `keep`：把 `a/b` 换成指向 X 的符号链接之后，`get_blob_rooted(id, W, a/b/f)` 返回 `NotDirectory`（`reason = "symlink"`），X 里没有新文件；用 apfs 的 `seams::set_before_open` 在 `clone_file_rooted` 打开 `b` 之前做同样的替换，结果相同；在克隆之后、`open_rooted_file` 打开 `b` 之前替换，返回 `Unknown`（`partial`），克隆出的文件随换走的目录留着，X 里的 `keep` 的 mode 与 mtime 都没变；用同一个接缝在 `open_rooted_file` 打开 `f` 之前，把 `f` 换成 W 之外一个 0600 文件 `Z` 的硬链接：返回 `Unknown`（`partial`），`Z` 仍是 0600，标志与 mtime 都没变（`set_file_meta` 的 `st_nlink` 核对）。对照：按路径的 `get_blob(id, W/a/b/f)` 跟着这个符号链接在 X 里建出了 `f`（2026-10-22 实测的内核行为），所以会话树里的目的地不用它 | 新增 |
| `deps_attempt_is_an_intent_row` | `begin_deps_attempt` 返回从 1 起的序号，行为 Running，`staging` 等于 `deps_staging_dir`；同一 (repo, set) 再 begin 返回 `Exists`（`detail.attempt`）；`set_deps_attempt_pid` 只写一次，第二次返回 `InvalidInput`，尝试结束之后返回 false；`end_deps_attempt` 把它关成 Failed 或 Refused，读回的 `StrataError` 与写入的相等，再 end 返回 false；关掉之后同一依赖集再 begin 拿到新号；`deps_attempts` 按号列出全部 | 新增 |
| `deps_set_seals_its_attempt` | `record_deps_set` 登记依赖集，并在同一个事务里把尝试记为 Sealed；origin 与尝试不配（Empty 带尝试、Build 配 Adopt 的尝试、尝试已结束、Empty 的 dirs 非空）返回 `InvalidInput`，什么都不写；已登记的依赖集再登记返回 `Exists`；`deps_set` 读回 origin 与封存时刻 | 新增 |
| `proposal_slots_supersede_in_one_transaction` | `record_proposal` 插入 Pending 时取代该 repo 原有的 Pending（`superseded_by` 是新号），插入 AwaitingPublish 时只取代同一 (repo, 提出会话) 的那条；`replaces` 与位置上实际的那条不符时返回 `None`，什么都不写；from = to、tree 与 head 只给一个、没有目标又没有提出会话都返回 `InvalidInput`；任何时刻每个 repo 至多一条 Pending | 新增 |
| `promote_and_decide_are_compare_and_swap` | `promote_proposal` 把 AwaitingPublish 变成 Pending 并填上 tree 与 head，同一个事务里取代该 repo 原有的 Pending 和 to 相同的其它 AwaitingPublish；对不是 AwaitingPublish 的返回 false；`decide_proposal` 只对 Pending 生效，`Superseded { by }` 的 by 必须存在且不是它自己；Applied 与 Superseded 之后不再变 | 新增 |
| `proposal_ids_never_reused` | 记三个提议，夹具用 gc 模式的原始连接删掉号最大的那个：新提议拿到的号仍比被删的大（`AUTOINCREMENT`）；依赖集尝试的序号同样不复用 | 新增 |
| `deps_main_is_compare_and_swap` | 预期没有行时，`set_deps_main(r1, None)` 写入并返回 `(true, None)`；以读到的 r1 作预期写 r2，返回 `(true, Some(r1))`；再以过时的 r1 作预期写 r3：不写，返回 `(false, Some(r2))`，`deps_main` 读回仍是 r2；预期与现有行只差 `observed` 时同样不写；已经有行时预期 `None` 也不写；`row` 与 `expected` 的 repo 不同返回 `InvalidInput`；set 可以是还没登记的依赖集 | 新增 |
| `generation_rows_register_and_read_back` | `record_base_gen` 写入的 generation 与清单读回相等（`has_base_gen`、`manifest`）；同样内容再登记一次不写；同一 (repo, tree) 的清单不同时返回 `InvalidInput`（`manifest_mismatch`，`detail.path` 是第一处差别），什么都不写 | 新增 |
| `manifest_absent_and_empty_are_distinct` | 没登记的 (repo, tree) 读回 `None`；登记过的空 tree 读回 entries 为空的清单；同一个 tree 在两个 repo 下各有一份 | 新增 |
| `base_gen_requires_registered_deps` | 依赖集没登记时 `record_base_gen` 返回 `InvalidInput`（`deps_unregistered`）；`manifest.tree` 与 `gen.tree` 不同返回 `InvalidInput`；两种都什么都不写；没有依赖目录的仓库用 `dirs = []` 的空依赖集（origin Empty）登记成功 | 新增 |
| `generation_paths_derive_from_keys` | `base_dir`、`deps_dir` 与 `deps_staging_dir` 给出 `<root>/bases/<repo>/<tree>/<deps>`、`<root>/deps/<repo>/<deps>` 与 `<root>/deps/<repo>/.staging/<attempt>`；同一个 tree 配两个依赖集得到两个不同的目录 | 新增 |
| `session_manifest_roundtrip` | `create_session` 之后，`session` 读回的 `SessionManifest` 与写入的相等，lifecycle 为 Creating，created 等于传入的时刻，每个工作区代数 1、健康 Ok；`sessions` 按创建时间列出；会话 id、root 或工作区路径重复返回 `Exists`；基线没登记或属于别的 repo、root 或工作区路径不合布局返回 `InvalidInput` | 新增 |
| `lifecycle_moves_forward_by_compare_and_swap` | 增补 4 的六条边都成功；当前不是 from 时返回 false，什么都不写；Removed→Active、Active→Unknown、Unknown→Active 之类返回 `InvalidInput`；进入 Unknown 不带 scene、别的转换带 scene 都返回 `InvalidInput`；进入 Removed 时记下时刻；`set_session_phase` 只在 Creating、Removing 时成功 | 新增 |
| `unknown_session_keeps_its_scene` | Creating→Unknown 带一段 scene：`session` 读回同一段 JSON；重开 Store 之后读回仍然相同（daemon 重启之后 status 报出同一个现场，T9 裁定）；Unknown→Removing→Removed 之后 scene 作为历史留着 | 新增 |
| `seeds_are_per_path_rows_written_once` | `record_seeds` 每个路径一行，读回按 path 排序、各字段相等（含早于纪元的 `source_mtime`）；同一路径第二次（同一批里重复，或之后再记）返回 `InvalidInput`（`seed_exists`），原值不变；会话不是 Creating 时返回 `InvalidInput` | 新增 |
| `move_intent_generation_never_reused` | `begin_move` 返回工作区代数加 1；已有没结束的意图时再 begin 返回 `Exists`；Preparing→Refused 之后再 begin 拿到更大的 n，不复用；会话不是 Active、工作区 tracked = false 或健康为 Unknown、目标没登记或属于别的 repo、staging 在 `<root>/store` 之下都返回 `InvalidInput` | 新增 |
| `finish_move_swaps_base_head_and_generation_together` | Swapping 的意图 `finish_move` 之后，读回的 base 的 tree 与 deps、head、代数、冲突列表都是新值，意图为 Done；用 `PauseBeforeCommit` 停在提交之前时，另一个读看到的全部是旧值，放行之后全部是新值（不变量 8）；`set_old_tree` 只对 Done 的意图写一次 | 新增 |
| `failed_move_marks_workspace_unknown` | `fail_move` 之后意图为 Unknown，工作区健康为 Unknown 并带 scene；之后对这个工作区 `begin_move` 返回 `InvalidInput`；`open_move_intents` 列出 Preparing 与 Swapping 的意图，不列已结束的；Preparing→Abandoned 成功 | 新增 |
| `move_conflicts_are_recorded_at_swapping` | `set_move_state(Preparing → Swapping, Some(c))` 把 c 原样记在意图行上，`move_intents` 读回 `conflicts = Some(c)`（空列表读回 `Some([])`）；Preparing、Refused 的意图读回 None；进入 Swapping 不带 conflicts、别的边带 conflicts 都返回 `InvalidInput`，什么都不写；`fail_move` 之后意图行上的冲突还在；`finish_move` 带另一组路径（少一条，或顺序不同）返回 `InvalidInput`（`conflicts_mismatch`），工作区与意图都不变；带同样的路径成功，工作区读回这组冲突 | 新增 |
| `resolve_move_abandon_returns_health_to_ok` | Swapping 之后 `fail_move`：`resolve_move(…, n, false, at)` 之后意图为 Abandoned、`ended` 为 at、冲突还在；工作区健康为 Ok，base、head、代数与冲突都不变，scene 原样留着；之后 `retire_dir(staging)` 与 `retire_dir(sessions/.repos/<id>/<repo>.git/worktrees/<n>)` 都成功（§5.5 第 5、6 行接受 Abandoned 的意图）；对这个工作区再 `begin_move` 拿到 n + 1 | 新增 |
| `resolve_move_accept_keeps_the_conflicts_recorded_at_swapping` | `set_move_state(Preparing → Swapping, Some([a, b]))` 之后丢掉这个 Store、不调 `close`（进程在交换中崩溃），再 `open`：`open_move_intents` 读回这条 Swapping 的意图，冲突是 [a, b]；`fail_move`（recover 的做法）之后 `resolve_move(…, n, true, at)`：工作区的 base 的 tree 与 deps、head、代数是意图的 to、head、n，冲突是 [a, b]，健康为 Ok，scene 留着；意图为 Done、`old_tree` 等于它的 staging、`ended` 为 at；用 `PauseBeforeCommit` 停在提交之前时，另一个读看到的全部是旧值，放行之后全部是新值（不变量 8） | 新增 |
| `resolve_move_guards_refuse_and_write_nothing` | 每条守卫各试一次，之后读回的会话、工作区与意图行都与之前相同：会话不存在或已 Removed 返回 `NoSuchSession`；会话是 Removing 返回 `InvalidInput`（`lifecycle`）；没有这个工作区返回 `NoSuchRepo`；健康为 Ok 返回 `InvalidInput`（`not_failed`）；n 不存在、n 是一条更早的 Done 意图都返回 `InvalidInput`（`no_failed_move`）；Abandon 之后再 `resolve_move` 返回 `InvalidInput`（`not_failed`）；夹具用 gc 模式的原始连接删掉意图的目标基线行之后，Accept 返回 `Corrupt`（`base_gen_missing`） | 新增 |
| `open_proposals_skip_removed_sessions` | 提出会话已 Removed 或已经没有行的 AwaitingPublish 不出现在 `proposals(_, open_only = true)` 里，Pending 照常出现；`open_only = false` 时全部出现 | 新增 |
| `retire_dir_moves_only_dirs_without_live_rows` | §5.5 表的第 1 到 3 行：没登记的 generation 目录与依赖集目录、Running、Sealed、Refused 尝试的暂存、行已被 gc 删掉的尝试的暂存，都被改名进 `store/trash/`，内容完好；已登记的 generation 与依赖集、Failed 尝试的暂存返回 `InvalidInput`，原样不动。表外的路径、含符号链接或 `..` 的路径也返回 `InvalidInput`；指向文件的路径返回 `NotDirectory`；不存在的路径返回 `NoSuchPath` | 新增 |
| `retire_dir_takes_dirs_of_removed_sessions` | §5.5 表的第 4 行：会话处在 Removing、Removed，以及行已被 gc 回收时，`sessions/<session-id>` 与 `sessions/.repos/<session-id>`（里面是各 repo 的影子仓库）都被改名进 trash，内容完好；同样的两个路径在 Creating、Active、Unknown 的会话上返回 `InvalidInput`，原样不动 | 新增 |
| `retire_dir_takes_swapped_out_trees` | §5.5 表的第 5 行，会话 Active：`finish_move` 之后换下来的旧工作区树在 Done 意图的 `staging` 上，被改名进 trash；另一次移动保留了旧树并 `set_old_tree`，那个路径也被接受；Preparing、Refused、Abandoned 意图的暂存被接受；Swapping、Unknown 意图的暂存、工作区目录本身、不属于任何意图的一级目录、别的会话的暂存返回 `InvalidInput`；会话进入 Removing 之后第 5 行不再接受（整个会话根走第 4 行） | 新增 |
| `retire_dir_takes_superseded_admin_dirs` | §5.5 表的第 6 行，会话 Active、工作区代数为 3：`worktrees/1`、`worktrees/2` 与一条 Refused 意图的 `worktrees/4` 被改名进 trash；当前代的 `worktrees/3`、Swapping 意图的 `worktrees/5`、没有意图的 `worktrees/9`、带前导零的 `worktrees/02` 返回 `InvalidInput`，原样不动；会话不是 Active 时一律不接受 | 新增 |
| `retire_dir_takes_base_staging` | §5.5 表的第 7 行：`bases/<repo>/.staging/<32 位小写十六进制>`（里面有导出的 `tree/` 与 gitio 的 `.export-tree-<nonce>/`）被改名进 trash，内容完好；根是 0555 的同样挪得走；31 位、带大写字母或不是十六进制的名字、`bases/<repo>/.staging` 本身、`bases/<repo>/.staging/<hex>/tree` 返回 `InvalidInput`，原样不动；指向文件的返回 `NotDirectory`，不存在的返回 `NoSuchPath` | 新增 |
| `record_checkpoint_roundtrip_keeps_base` | 五种条目（含 `Renamed` 的 from 与 `Untracked`）记下之后，`checkpoint` 读回相等，`base` 是记录时对着的 `BaseGen`；输入里的 id 被忽略，返回值是分配的号 | 新增 |
| `checkpoint_ids_monotone_per_session` | 两个会话交替记 checkpoint，各自从 1 起连续递增，互不共用 | layerfs-persistence/tests/history_allocation.rs::reservations_are_monotone_half_open_and_per_scope |
| `failed_record_consumes_no_id` | 一次 record 失败（`AfterCheckpointRow`）之后，失败的那次没有留下任何行；下一次成功拿到上一个已提交的号加 1；提交过的号从不复用。与 3a:589 原文"失败留下空洞"不同：strata 的号与 checkpoint 行在同一个事务里出现，回滚的尝试不占号（§4.4） | layerfs-persistence/tests/history_allocation.rs::a_consumed_reservation_survives_a_later_failure |
| `checkpoint_id_refuses_past_i64_max` | 用原始连接放进一行 id 为 `i64::MAX` 的 checkpoint：下一次 record 返回 `TooLarge`（`checkpoint_id`），所有表的行不变，不回绕 | layerfs-persistence/tests/history_allocation.rs::counts_are_checked_and_the_terminal_endpoint_is_refused<br>layerfs-persistence/tests/history_remediation.rs::last_representable_reservation_is_consumed_and_terminal_refusal_is_atomic |
| `ids_survive_reopen` | 写入 blob 与 checkpoint，关闭再打开：都能读回；同内容再 put 计为复用；下一个号紧接着上一个 | layerfs-persistence/tests/sqlite_publication.rs::canonical_save_read_reuse_and_persisted_reservations_survive_reopen |
| `negative_id_read_back_is_corrupt` | 用原始连接（`PRAGMA ignore_check_constraints = 1`）放进一行负 id 的 checkpoint：`checkpoints` 与 `checkpoint` 读到它时返回 `Corrupt`（`bad_row`），不把负数交给 `CheckpointId::from` | 新增 |
| `checkpoint_read_with_missing_entries_is_corrupt` | 记一个三条条目的 checkpoint，夹具用 gc 模式的原始连接删掉其中一条 `overlay_entries` 行：`checkpoint` 与 `latest_checkpoint` 都返回 `Corrupt`（`entries_mismatch`，`detail` 带期望与实际的条数），不交出少了一条的快照；`checkpoints` 的摘要照常列出它，条目数仍是 3（摘要不读条目行） | 新增 |
| `tool_call_checkpoint_recorded_once` | 同一会话、同一 repo、同一个 `ToolCall { id }` 记两次：第二次什么都不写，返回第一次的号；换一个 repo 或换一个工具调用 id 各记一个新的 | 新增 |
| `checkpoint_unit_is_one_transaction` | checkpoint 行、全部条目与新 blob 的登记在一个事务里：用 `PauseBeforeCommit` 停在提交之前，另一个读看不到其中任何一项；放行之后同时看到全部；已登记过的 blob 按先到者为准，不报错 | layerfs-persistence/tests/sqlite_publication.rs::final_body_multirow_locators_and_first_wins_have_one_atomic_ack |
| `record_checkpoint_failure_rolls_back_whole_unit` | 在插入 checkpoint 行之后注入失败：没有 checkpoint 行，没有条目，没有新的 blob 登记；之前的 checkpoint 与 blob 仍可读；失败只报告一次，store 不自己重试 | layerfs-content/tests/filesystem_failure.rs::a_read_failure_at_each_boundary_publishes_no_root_and_keeps_the_old_one<br>layerfs-content/tests/filesystem_failure.rs::a_refusing_consumer_ends_the_operation_once_without_a_root<br>layerfs-persistence/tests/sqlite_publication.rs::failure_after_body_insert_rolls_back_the_entire_unit_and_published_body_is_immutable |
| `late_source_failure_writes_no_checkpoint` | 一组文件 put 到第 k 个时源文件出错：错误原样传出，不调用 record，没有 checkpoint 行；已 put 的 k−1 个 blob 留在 `blobs/` 里，gc 过了年龄下限收走 | layerfs-content/tests/filesystem_sorted.rs::a_late_source_error_propagates_and_publishes_nothing<br>layerfs-workspace/tests/captured_namespace_custody.rs::a_consumer_that_rejects_a_file_root_leaves_the_file_custody |
| `checkpoint_refuses_reference_to_missing_blob` | 条目引用一个从没 put 过的 BlobId：record 返回 `Corrupt`（`blob_missing`），什么都不写；引用由 store 按条目自己推导、核对；提交成功的 checkpoint 引用的 blob 都已在 `blobs/` 里并已登记 | layerfs-content/tests/streaming.rs::children_are_emitted_before_their_parents<br>layerfs-persistence/tests/semantic_admission.rs::omitted_wire_references_cannot_hide_an_inode_dependency_from_save<br>layerfs-persistence/tests/sqlite_reference_acquisition.rs::missing_logical_child_is_still_refused |
| `interleaved_sessions_do_not_see_failed_checkpoints` | 三个会话的 put 与 record 交错进行，其中一个注入失败：成功的两个可读，失败的那个不可见，互不影响 | layerfs-persistence/tests/interleaved_saves.rs::scoped_registry_interleaves_pending_reads_finish_abort_and_slot_reuse |
| `checkpoint_refused_for_untracked_or_removed` | `tracked = false` 的工作区返回 `InvalidInput`；基线没登记或属于别的 repo 返回 `InvalidInput`；会话已 Removed 返回 `NoSuchSession`；会话里没有这个 repo 返回 `NoSuchRepo`；都不写 | 新增 |
| `same_path_type_change_is_one_entry` | 同一路径上一条 Deleted 加一条 Added 返回 `InvalidInput`，什么都不写；文件变成符号链接记成一条 Modified、mode 120000，读回相同（增补 4"Same-path type change"） | 新增 |
| `publish_record_roundtrip_and_phase_machine` | `record_publish` 读回的 `ReleaseGroup` 与写入的相同，批次与路径的顺序不变，id 是分配的号；Running→Unknown→Done 成功，Done→Running 返回 `InvalidInput`；批次点名不存在的 checkpoint 返回 `NoSuchCheckpoint`；批次点名的会话已 Removed 或已经没有行时返回 `NoSuchSession`，`publishes` 与 `publish_items` 都不多出行 | 新增 |
| `open_publishes_listed_for_restart` | 阶段为 Running 与 Unknown 的发布出现在 `open_publishes` 里，Done 的不出现；重开 Store 之后结果相同 | 新增 |
| `ref_audit_only_strata_refs_for_update_ref` | `UpdateRef` 配 `refs/heads/main` 返回 `InvalidInput`，配 `refs/strata/publish/main` 成功；`Push` 配 `HeadMoved` 返回 `InvalidInput`；`confirmed` 读回的 `Action` 与写入的相同 | 新增 |
| `external_writer_is_busy_without_side_effects` | 子进程 `lock_child` 开原始连接并持着 `BEGIN IMMEDIATE`：本进程的 record 立即返回 `Busy`（`sqlite`），不重试，没有副作用；子进程放手之后，同样的调用成功 | layerfs-persistence/tests/serverless_store.rs::lock_child<br>layerfs-persistence/tests/serverless_store.rs::second_process_busy_has_no_effect_and_readers_keep_progressing<br>layerfs-persistence/tests/sqlite_publication.rs::readonly_refusal_and_external_writer_busy_do_not_retry<br>layerfs-daemon/tests/installed_store.rs::direct_reads_finish_under_another_process_writer_and_busy_stays_typed |
| `readers_never_block_on_writer` | 外部进程持写锁时，以及本进程的写事务停在 `PauseBeforeCommit` 时，另一个线程的读（`session`、`checkpoint`）都在 100 ms 内完成，读到提交之前的状态；只读打开的 Store 同样如此 | layerfs-persistence/tests/serverless_store.rs::second_process_busy_has_no_effect_and_readers_keep_progressing<br>layerfs-daemon/tests/installed_store.rs::direct_reads_finish_under_another_process_writer_and_busy_stays_typed |
| `killed_lock_holder_leaves_no_stale_lock` | 子进程以写者身份打开 Store、开着写事务时被 SIGKILL：父进程随即 `open` 成功并能写，没有陈旧的 LOCK 或 SQLite 锁 | layerfs-persistence/tests/writer_admission.rs::a_holder_process_that_exits_hands_the_lock_to_the_waiter |
| `checkpoint_write_is_durable_after_crash` | 子进程 `crash_child` put 两个 blob、record 一个 checkpoint，打印确认后等待；父进程收到确认即 SIGKILL 它，然后重开：checkpoint 行、全部条目、两个 blob 的文件与行都在，下一个号紧接着。这是 `synchronous = FULL` 的验证方法的一部分：SIGKILL 只证明"确认等于已提交"（进程被杀后页缓存仍在，`synchronous = OFF` 也能通过），FULL 本身由 `open_pins_durable_pragmas`（回读）与 `commit_issues_full_fsync`（同步调用确实发出）证明；不声称断电级持久 | layerfs-persistence/tests/sqlite_recovery.rs::crash_child<br>layerfs-persistence/tests/sqlite_recovery.rs::sigkill_preserves_acknowledged_body_locator_and_reservation |
| `commit_issues_full_fsync` | 插桩的 VFS 计数：每次 record 提交，在 record 返回之前，WAL 上至少有一次 `SQLITE_SYNC_FULL` 标志的 xSync（`fullfsync = 1` 时即 `F_FULLFSYNC`），`SQLITE_SYNC_NORMAL` 标志的 xSync 为 0 | 新增 |
| `commit_io_error_is_unknown_and_poisons_writer` | `CommitSyncFails`：record 返回 `Unknown`（`uncertain`）；`is_poisoned()` 返回 true，此后的写返回 `Unknown`（`poisoned`），读照常；中毒的写者 `close` 不写 clean-shutdown 标记，重开时因此跑 quick_check；重开之后 `is_poisoned()` 为 false，读到的就是真实结果，同一工具调用的重放要么返回已有的号、要么记一个新的；不确定的结果从不被当成确定的失败去重放或清理 | layerfs-overlay/tests/error_identity.rs::a_real_sqlite_full_identity_never_unwraps_an_uncertain_outcome |
| `disk_full_during_checkpoint_keeps_previous_state` | root 放在 64 MiB 的 APFS 磁盘映像上并写满：put 返回 `Unknown`（`not_written`），record 返回 `Unknown`（`not_written` 或 `uncertain`），都作为错误上报；之前的 checkpoint 与 blob 原样可读，store 不重放、不清理；腾出空间之后新的 checkpoint 成功（`#[ignore]`） | layerfs-overlay/tests/error_identity.rs::a_real_sqlite_full_identity_never_unwraps_an_uncertain_outcome<br>layerfs-overlay/tests/orphans.rs::a_failed_release_keeps_exact_custody_until_terminal_file_disposal<br>layerfs-telemetry/tests/full_disk.rs::actual_enospc_preserves_the_original_result<br>layerfs-daemon/tests/mounted_sqlite_full.rs::known_sqlite_full_is_enospc_and_normal_drain_completes |
| `gc_keeps_referenced_generations` | 基线 G1 被活会话的 checkpoint 引用，之后又建了基线 G2：`gc` 与 `gc_repo`（keep = 0）之后 G1 的行与目录仍在，它上面的 checkpoint 可读，blob 都在；引用它的会话 removed 并过了保留期之后，G1 被回收（行删掉、目录进 trash），G2（最新）留着；被引用的 blob 行删不掉。checkpoint 行不可变、被引用的 blob 一直留着，所以 strata 不需要"读者持有层"那样的机制；回收按引用，不分页 | layerfs-persistence/tests/inode_branch_delta_custody.rs::a_branch_base_cannot_be_dropped_and_survives_reclamation<br>tgrep-core/tests/generations.rs::retained_views_and_checkpoints_outlive_managers_and_new_generations<br>layerfs-overlay/tests/payload_runs.rs::a_maintenance_page_is_fourteen_cells_of_bytes_whatever_the_rows<br>layerfs-overlay/tests/orphans.rs::captured_reader_preserves_sealed_bytes_across_install_and_blocks_failed_fold |
| `gc_keeps_base_of_every_retained_checkpoint` | 会话从 G1 移到 G2 之后，它在 G1 上的 checkpoint 还留着：`gc_repo`（keep = 0）不回收 G1，行与目录都在，跨基线的 restore 找得到 `base_dir(G1)`；会话 removed 而还没过 keep 时，它的 checkpoint 同样钉住 G1；会话被 gc 回收之后，下一轮 `gc_repo` 才回收 G1（增补 5） | 新增 |
| `gc_reclaims_removed_sessions_after_keep` | removed 未满 keep 的会话全部保留，含它的 BeforeRemove checkpoint；满了之后，它的工作区、种子、意图、checkpoint、条目与它提出的 AwaitingPublish 提议都被删，只被它引用的 blob 在过了下限之后被删 | 新增 |
| `gc_keeps_newest_base_per_repo` | 一个 repo 最新的基线即使没有任何引用也不被回收；更旧的、没有引用、过了 keep 的被回收 | 新增 |
| `gc_keeps_pending_proposal_deps` | Pending 提议的 to、提出会话还在的 AwaitingPublish 的 to、`deps_main` 的 set 都不被 `gc_repo` 回收，from 不钉（deps.md §4.6）；提议被决定且过了 keep 之后被 gc 删掉，仍被留下的提议经 `superseded_by` 引用的行留着；之后不再被引用的依赖集被回收 | 新增 |
| `gc_reclaims_closed_attempts_keeps_failed_scenes` | Sealed、Refused 且过了 keep 的尝试被删，之后的号仍不复用；Failed 而暂存还在的尝试与它的暂存都留着，暂存被拿走之后下一轮才删这一行 | 新增 |
| `gc_never_deletes_young_blobs` | keep = 0 时，刚 put、还没被 record 引用的 blob 不被删（年龄下限 1 小时，按 mtime 与 ctime 中较晚的那个）；把它的 mtime 改到 2020 年也不被删（ctime 是当下）；用 `AgeFloor` 把下限改成 1 秒、等过之后它被删；之后 record 引用这个已被删的 blob 返回 `Corrupt`，不会写出悬空引用 | 新增 |
| `gc_spares_in_flight_put_with_old_mtime` | 源文件的 mtime 是 2020 年；put 用 `PauseInPut` 停在临时克隆之后（克隆体的 mtime 也是 2020 年），另一个线程跑 gc：gc 等到 put 放开 L3 才清扫 `store/tmp/` 与 `blobs/`；put 成功，blob 在，报告里 `tmp_removed` 与 `blobs` 都是 0 | 新增 |
| `gc_spares_freshly_renamed_generation_dir` | 一个 mtime 是 2020 年的目录刚刚改名到 `bases/<repo>/<tree>/<deps>`、还没登记：`gc_repo`（keep = 0）不动它（ctime 是当下）；用 `AgeFloor` 缩短下限并等过之后，它被挪进 trash | 新增 |
| `gc_skips_paths_of_open_intents_and_attempts` | `AgeFloor` 为 0：Preparing、Swapping、Unknown 意图与 Running、Failed 尝试点名的路径（其中一个放在 `bases/` 下、名字合乎键的形状、没有登记）在 gc 与 `gc_repo` 之后原样留着；那条意图关成 Refused 之后，下一轮 `gc_repo` 把那个目录挪进 trash | 新增 |
| `retiring_a_0555_root_reparents_it` | 一个依赖集照 deps 的封存方式把根设成 0555，没被引用、过了 keep：`gc_repo` 先给根补上属主写权限，再删行、挪进 trash，过了年龄下限之后 trash 删得干净；一个没登记的 0555 根，`retire_dir` 同样挪得走 | 新增 |
| `retiring_a_uchg_root_clears_the_flag_first` | 一个基线 generation 的根带 `uchg`（clonefile 会把它带过来）：`gc_repo` 先清标志、再补写权限、再挪进 trash；用 `RetirePrepFails { errno: EPERM }` 让准备失败时，这个 generation 的行与目录都留着、路径进 `stuck`、整轮不失败，下一轮回收它；`retire_dir` 对一个没登记的 `uchg` 根同样先清标志，准备失败返回 `ReadOnly`，目录原样 | 新增 |
| `gc_rechecks_pins_written_after_its_snapshot` | 两个不是最新的基线 generation G1、G2 与一个依赖集 S 都没被引用、过了 keep；`GcPauseBeforeRows` 让 `gc_repo` 停在准备好根目录之后、第一批删行之前，另一个线程这时用 `create_session` 建一个基于 G1 的工作区，用 `begin_move` 写一条目标是 G2 的意图，用 `set_deps_main` 把主干指向 S（后两者在真实调用里持 repo 写锁，不会与 `gc_repo` 交错；这里直接调用，测的是重新核对这道兜底）；放行之后这一轮照常完成（`outcome = 'complete'`、没有错误），G1、G2、S 的行与目录都在、没有进 trash；G1 的根原先是 0555，之后对属主可写（准备过的根目录不还原）；意图关成 Refused、主干改指别处之后，下一轮回收 G2 与 S | 新增 |
| `gc_refuses_publish_pins_between_batches` | 会话 S 已 Removed、过了 keep，有一个两条条目的 checkpoint；`GcBatchRows { rows: 2 }` 与 `GcPauseAfterBatch { n: 1 }` 让 `gc` 删完 S 的两条条目那一批之后停住：另一个线程这时 `record_publish` 一个点名 (S, 1) 的发布，返回 `NoSuchSession`，`publishes` 与 `publish_items` 都没有多出行；停在同一处时 `checkpoint(S, 1)` 返回 `Corrupt`（`entries_mismatch`），不读成一个空的快照；放行之后这一轮照常完成（`outcome = 'complete'`），S 的 checkpoint、工作区与会话行都不在了 | 新增 |
| `gc_retires_stale_base_staging` | `bases/<repo>/.staging/` 下放一个过了年龄下限的暂存（里面有 `tree/` 与 gitio 的 `.export-tree-<nonce>/`）和一个刚建的暂存：`gc_repo` 把旧的挪进 trash（`dirs_retired` 加 1），刚建的原样；名字不是 32 位小写十六进制的条目、`deps/<repo>/.staging/` 下的东西都原样；`bases/<repo>/.staging/` 变空之后也不删。暂存被挪走之后，用它的路径做的改名（建造者登记时的那一步）得到 `NoSuchPath`，`bases/<repo>/<tree>/` 下没有出现任何东西 | 新增 |
| `gc_sweeps_stale_temp_entries` | `store/tmp/` 里放三个过了年龄下限的条目：死掉的 put 留下的克隆体（带着源的 `uchg`：put 停在第 5 步规整之前）、建库剩下的临时库、一个不是 store 建的目录（里面有文件）；`gc` 之后这三个都不在，`tmp_removed` 是 3；没过下限的克隆体留着。第四个条目是一个在 `store/tmp/` 之外还有一个硬链接、又带着 `uchg` 的文件（先建链接、再加标志）：它留在原处，`stuck` 里只有它，两个名字上的 `uchg` 都还在（apfs 只在没有别的名字的 inode 上清标志） | 新增 |
| `gc_deletes_supersede_chains_across_batches` | `promote_proposal` 让一条号小的提议取代号大的 Pending，再由一条更新的 Pending 取代这条号小的，链里两个方向的引用都有；都决定了、过了 keep，`GcBatchRows { rows: 1 }` 让每批只删一行：gc 删完整条链，没有一批在 COMMIT 时撞上外键；被留下的提议经 `superseded_by` 引用的行留着 | 新增 |
| `symlink_put_never_retakes_the_blob_gate` | put 一个符号链接，`PauseInPut` 停在临时克隆之后；另一个线程开始 gc，在 L3 独占锁上排队；放行之后 put 在 1 秒内返回目标文本的 id，gc 随后完成：符号链接那一支没有第二次取 L3 | 新增 |
| `gc_keeps_open_publish_scenes` | removed 并过了 keep 的会话，只要有 Running 或 Unknown 的发布引用它的 checkpoint，就整个保留；发布变成 Done 之后，下一轮回收它 | 新增 |
| `gc_retires_dirs_through_trash_and_resumes` | 回收的 generation 目录先进 trash，过了年龄下限才删（用 `AgeFloor` 缩短）；用 `GcStopAfterRetire` 让 `gc_repo` 在挪目录中途失败：返回 `Unknown`（`partial`），已提交的批次有效，下一轮收走活路径上没登记的目录并把 trash 删完；trash 里带 `uchg` 的文件、0555 的目录照样删掉（`remove_tree_rooted`）；trash 里一个指向 root 之外目录的符号链接条目（`gc_repo` 第 2 步挪进来的）只删链接本身，链接指向的目录与里面的东西原样；用 `GcUnlinkFails` 让一个 trash 条目删不掉：它进 `stuck`，整轮不失败，下一轮删掉它 | 新增 |
| `gc_touches_nothing_outside_its_areas` | `sessions/` 下的一切、`bases/` 下以 `.` 开头的临时目录、解析不成键的名字、指向 root 之外的符号链接，gc 之后都原样存在，链接的目标没有被删；`bases/<repo>` 被换成指向 root 之外目录的符号链接时，`gc_repo` 不进去，那个目录与里面的东西原样 | 新增 |
| `gc_never_reclaims_live_session_with_missing_dirs` | 活会话的工作区目录被人手删掉：gc 不回收它的任何行，checkpoint 仍可读；这样的现场由 status 报出，不由 gc 清理 | sdk/js/native/tests/unit/reclaim.rs::a_gone_container_is_reclaimed_by_removing_its_records |
| `gc_scrub_reports_corrupt_and_resumes_from_cursor` | 用 `ScrubBudget` 把预算设为 4 KiB：每轮核对一段，游标前进，走到末尾回到开头；改坏的 blob 在扫到它的那一轮进 `corrupt` | 新增 |
| `corrupt_blob_is_reported_never_overwritten` | 已存 blob 被改坏（长度不变）：gc 的完整性扫描把它列进 `corrupt` 并原样保留；同内容再 put 不覆盖它；人把正确的字节写回之后，`verify_blob` 通过，下一轮扫描不再报 | tgrep-core/tests/generations.rs::malformed_metadata_is_rejected_without_overwriting_generation |
| `gc_report_persisted_for_status` | `gc` 与 `gc_repo` 返回的报告与 `last_gc_report(scope)` 读回的相同，重开之后仍能读到；中途停下的一轮（`GcStopAfterRetire`）以 `outcome = 'partial'`、带 `error` 写进 `gc_runs`，`last_gc_report` 读得到它；旧的 gc 记录过了 keep 被删，每个 scope 最新的一行留着 | 新增 |

覆盖：目标模块为 store 的 56 行（3a 45、3c 4、3d 5、3f 2）加裁定改归 store 的 3c L139，共 57 行，全部出现在上表的来源栏里；另引用理由点名 store 的 `—` 行 15 条（3a 2、3b 4、3c 6、3d 2、3e 1），其中 3c:47（`captured_reader_preserves_sealed_bytes_across_install_and_blocks_failed_fold`，理由写"被引用的 blob 由 store 引用计数 gc 保留"）在本轮补引到 `gc_keeps_referenced_generations`。上表共 122 个测试，其中新增 76 个。3b 的 `layerfs-telemetry/tests/platform.rs::retention_expiry_open_reader_and_deletion_failure` 理由写明"不以此为依据"，所以 gc 保留期的测试记为新增。上一版的 `base_gen_requires_registered_tree_and_deps` 改为 `base_gen_requires_registered_deps`：清单不再单独登记，而是随 `record_base_gen` 一起写（M13）。

## 8. 实现任务（TDD）

各任务共用的约定：

- `Cargo.toml` 在 `[features]` 里声明 `test-seams = []`，并在 `[dev-dependencies]` 里写 `strata-store = { path = ".", features = ["test-seams"] }`：测试总能用到 §3 的 `Seam`、`SyncCounts` 与 `arm`、`wait_paused`、`release`、`sync_counts`，发布构建里没有它们（增补 4"Test seams"）。
- `tests/support/mod.rs` 提供：同卷的临时 root 与假 checkout；登记一个空依赖集、一个基线并建一个会话的夹具；按 §4.2 设好连接设置、注册了 `strata_gc_active`（返回 0 或 1 由参数定）的原始连接；把一个文件或目录的 mtime 设到 2020 年的助手；按名字重新执行本测试二进制的子进程助手；`hdiutil` 磁盘映像（APFS、FAT，Drop 时卸载）。
- 每个任务的测试放在自己的测试文件里，Run 行用 `--test <文件名>` 跑整个文件，并带 `--features test-seams`。

### Task 1: 打开、布局、schema 与守卫

**Files:**
- Create: `crates/strata-store/Cargo.toml`、`crates/strata-store/src/lib.rs`、`crates/strata-store/src/schema.rs`、`crates/strata-store/src/open.rs`、`crates/strata-store/src/errors.rs`、`crates/strata-store/src/seams.rs`
- Test: `crates/strata-store/tests/open.rs`、`crates/strata-store/tests/guards.rs`、`crates/strata-store/tests/support/mod.rs`

**Interfaces:**
- Consumes: `strata_types::{StrataError, Code}`、`apfs::{same_volume, nearest_existing_ancestor, classify_io_error, IoSide, clone_file}`
- Produces: `Store::open`、`Store::open_read_only`、`Store::close`、`Store::quick_check`、`Store::root`、`Store::is_read_only`；clean-shutdown 标记；§4.3 的 DDL 文本（全部表、索引与触发器）；§4.2 的设置与回读；带 `SQLITE_INNOCUOUS` 的 `strata_gc_active`；以 `O_CLOEXEC` 打开的 LOCK；`detail.outcome` 的构造；`Seam::InitBeforeRename` 与 `Store::arm`

- [ ] **Step 1: 写失败的测试**：`open_creates_layout_on_empty_root`、`interrupted_creation_is_resumed`、`open_refuses_nonempty_foreign_root`、`open_refuses_lost_store_db`、`open_refuses_cross_volume_root`、`open_refuses_volume_without_clonefile`、`open_pins_durable_pragmas`、`open_refuses_non_wal_store`、`open_refuses_schema_drift`、`open_refuses_unknown_user_version`、`open_leaves_unrelated_database_untouched`、`second_writer_open_is_busy`、`read_only_open_refuses_every_write`、`quick_check_runs_only_after_unclean_exit`、`lock_is_not_inherited_by_child_processes`、`committed_rows_are_immutable`、`delete_outside_gc_is_refused`、`state_graphs_are_enforced_by_triggers`。守卫三项用夹具的原始连接直接插入和改写行，不经 store 的写方法
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-store --features test-seams --test open --test guards`
Expected: FAIL（`Store::open` 还不存在，编译失败）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.1 的建库顺序（同卷检查是 `same_volume(&nearest_existing_ancestor(root)?, checkout)`，同卷与非空检查在任何写入之前，`O_CLOEXEC` 的 LOCK，clonefile 探测失败时 rmdir 自己的空目录，临时库加 `RENAME_EXCL`）；§5.2 的文件头预检、身份核对、设置回读、`:memory:` schema 比对、函数注册，看 clean-shutdown 标记决定要不要跑 `quick_check`，取 root 的 realpath 给 apfs 在根之下的调用，`close` 写标记，按需的 `quick_check()`；只读打开与 `ReadOnly` 的统一入口；`io::Error` 经 `apfs::classify_io_error`、SQLite 错误由 store 自己归类到 `StrataError`（types.md §6.3 加 §6.2 的 `detail`）；§4.3 的 DDL 原文
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-store --features test-seams --test open --test guards`（磁盘映像的两个再加 `-- --ignored` 跑一遍）
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-store/Cargo.toml crates/strata-store/src/lib.rs crates/strata-store/src/schema.rs crates/strata-store/src/open.rs crates/strata-store/src/errors.rs crates/strata-store/src/seams.rs crates/strata-store/tests/open.rs crates/strata-store/tests/guards.rs crates/strata-store/tests/support/mod.rs
git commit -m "feat(store): open, layout, pinned schema and guards"
```

### Task 2: blob 存储

**Files:**
- Create: `crates/strata-store/src/blob.rs`
- Modify: `crates/strata-store/src/seams.rs`
- Test: `crates/strata-store/tests/blob.rs`

**Interfaces:**
- Consumes: `apfs::{clone_file, attrs_of, FileKind, clone_file_rooted, open_rooted_file, clone_from_rooted}` 与 apfs 第 2 轮的 `CloneKind`、`set_file_meta`、`FileMeta`（§2 的上游要求）、测试里 `apfs::seams::set_before_open`；`strata_types::{BlobId, RepoPath}`
- Produces: `Store::put_blob`、`Store::put_blob_rooted`、`Store::put_bytes`、`Store::get_blob`、`Store::get_blob_rooted`、`Store::read_blob`、`Store::verify_blob`；L3 的共享一侧；`Seam::{BlobRenameFails, TempNameTaken}`；`Store::sync_counts` 的 blob 与目录屏障计数

- [ ] **Step 1: 写失败的测试**：`blob_put_is_idempotent`、`blob_id_ignores_delivery_shape`、`blob_known_answer_vectors`、`empty_file_is_a_valid_blob`、`stored_blob_is_never_rewritten`、`blob_identity_changes_with_any_byte`、`blob_roundtrip_is_byte_exact`、`large_and_sparse_blobs_roundtrip`、`same_bytes_new_path_or_mode_reuse_blob`、`blob_files_are_content_only`、`symlink_blob_stores_target_text`、`put_blob_refuses_directory_and_special_files`、`get_blob_refuses_existing_dst`、`get_blob_missing_is_corrupt_and_leaves_no_dst`、`missing_or_mismatched_blob_is_corrupt_never_new`、`get_blob_is_trusted_verify_detects_flipped_bit`、`put_reuse_does_not_reread_existing_copy`、`put_reuse_with_length_mismatch_is_corrupt`、`put_restores_missing_blob_file`、`stale_temp_files_do_not_block_put`、`concurrent_puts_of_same_content_store_one_blob`、`put_blob_orders_blob_before_later_writes`、`put_rename_failure_is_not_written`、`put_blob_rooted_never_follows_a_swapped_parent`、`put_blob_rooted_refuses_a_swapped_kind_before_cloning`、`get_blob_rooted_roundtrip_matches_get_blob`、`get_blob_rooted_never_writes_through_a_swapped_parent`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-store --features test-seams --test blob`
Expected: FAIL（`put_blob` 等还不存在）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.3 的 L3 共享锁、克隆、只看克隆体、规整（标志、mode、扩展属性的跳过表）、BLAKE3（16 MiB 起 mmap 加 rayon）、先看目标再决定（已存在就 `utimensat` 刷新、不做屏障）、两次 `apfs::set_file_meta`（0600 与 0444）、两个 `F_BARRIERFSYNC`、`RENAME_EXCL` 与 `EEXIST` 分支；`put_blob_rooted`（`attrs_of` 先看种类：目录与特殊文件在克隆之前拒绝，符号链接直接存目标；`clone_from_rooted(…, CloneKind::File)` 克隆进 tmp，检查之后被换掉的由 apfs 在克隆之前拒绝；`test-seams` 同时打开 `strata-apfs/test-seams`）；§5.4 的按文件工作（有登记行才比长度）、受信任 get（把 dst 的句柄交给 `apfs::set_file_meta`：清标志、0644、mtime 为当前时刻，被拒绝或中途失败就是 `Unknown`（`partial`））、`get_blob_rooted`（`clone_file_rooted` 传 `None`，再把 `open_rooted_file` 的句柄交给同一个 `set_file_meta`，打不开或被拒绝就是 `Unknown`（`partial`））、read 的上限、verify 的重算
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-store --features test-seams --test blob`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-store/src/blob.rs crates/strata-store/src/seams.rs crates/strata-store/tests/blob.rs
git commit -m "feat(store): content-addressed blob put, get and verify"
```

### Task 3: 依赖集的四张表

**Files:**
- Create: `crates/strata-store/src/txn.rs`、`crates/strata-store/src/deps.rs`
- Test: `crates/strata-store/tests/deps.rs`

**Interfaces:**
- Consumes: `strata_types::{DepsSet, DepsSetId, ProposalId, SessionId, RepoName, TreeOid, CommitOid, StrataError}`
- Produces: `Store::deps_dir`、`Store::deps_staging_dir`、`Store::begin_deps_attempt`、`Store::set_deps_attempt_pid`、`Store::end_deps_attempt`、`Store::record_deps_set`、`Store::record_proposal`、`Store::promote_proposal`、`Store::decide_proposal`、`Store::set_deps_main`、`Store::deps_set`、`Store::deps_attempts`、`Store::proposal`、`Store::proposals`、`Store::deps_main`；`AttemptKind`、`AttemptState`、`DepsAttempt`、`DepsOrigin`、`DepsSetRecord`、`NewProposal`、`ProposalRecord`、`ProposalStatus`、`ProposalDecision`、`DepsMain`；写事务助手（L2、`BEGIN IMMEDIATE` 与 `Busy`、从 `sqlite_sequence` 取号与到顶检查）；读连接池与读事务

- [ ] **Step 1: 写失败的测试**：`deps_attempt_is_an_intent_row`、`deps_set_seals_its_attempt`、`proposal_slots_supersede_in_one_transaction`、`promote_and_decide_are_compare_and_swap`、`proposal_ids_never_reused`、`deps_main_is_compare_and_swap`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-store --features test-seams --test deps`
Expected: FAIL（这些函数还不存在）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.6 的每个方法一个写事务；尝试与提议的显式取号；`record_proposal` 的位置核对（`replaces`）、先取代再插入与延迟外键；`promote_proposal` 的取代范围；比较并交换返回 false 与 `InvalidInput` 的区分；`set_deps_main` 在一个写事务里先读、逐列比较、再写的比较并交换；JSON 列（`dirs`、`error`）；§5.10 的读与 `TryFrom` 校验
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-store --features test-seams --test deps`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-store/src/txn.rs crates/strata-store/src/deps.rs crates/strata-store/tests/deps.rs
git commit -m "feat(store): deps sets, attempts, proposals and main"
```

### Task 4: 基线 generation 与清单

**Files:**
- Create: `crates/strata-store/src/gens.rs`
- Test: `crates/strata-store/tests/gens.rs`

**Interfaces:**
- Consumes: `strata_types::{BaseGen, BaseManifest, ManifestEntry, RepoName, TreeOid, DepsSetId}`；Task 3 的 `record_deps_set`
- Produces: `Store::base_dir`、`Store::record_base_gen`、`Store::has_base_gen`、`Store::manifest`

- [ ] **Step 1: 写失败的测试**：`generation_rows_register_and_read_back`、`manifest_absent_and_empty_are_distinct`、`base_gen_requires_registered_deps`、`generation_paths_derive_from_keys`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-store --features test-seams --test gens`
Expected: FAIL（这些函数还不存在）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.5 的 `record_base_gen`（tree 一致、依赖集已登记、清单逐条比较、相同不写、不同 `InvalidInput`、空 tree 也有 `base_trees` 行）；按键算路径
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-store --features test-seams --test gens`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-store/src/gens.rs crates/strata-store/tests/gens.rs
git commit -m "feat(store): base generations with their manifests"
```

### Task 5: 会话、工作区、移动意图与 `retire_dir`

**Files:**
- Create: `crates/strata-store/src/sessions.rs`、`crates/strata-store/src/retire.rs`
- Modify: `crates/strata-store/src/txn.rs`、`crates/strata-store/src/seams.rs`
- Test: `crates/strata-store/tests/sessions.rs`

**Interfaces:**
- Consumes: `strata_types::{SessionManifest, WorkspaceState, SessionId, RepoPath, CommitOid, BaseGen}`；`apfs::{open_rooted, full_fsync, rename_rooted, RenameMode}` 与 apfs 第 2 轮的 `make_removable`（§2 的上游要求）；Task 3 的提议读；Task 4 的 `record_base_gen`
- Produces: `Store::create_session`、`Store::set_session_phase`、`Store::set_lifecycle`、`Store::record_seeds`、`Store::begin_move`、`Store::set_move_phase`、`Store::set_move_state`、`Store::finish_move`、`Store::fail_move`、`Store::resolve_move`、`Store::set_old_tree`、`Store::session`、`Store::sessions`、`Store::move_intents`、`Store::open_move_intents`、`Store::retire_dir`；`Lifecycle`、`Health`、`SessionRecord`、`WorkspaceRecord`、`SeedRow`、`MoveIntent`、`MoveState`；`Seam::PauseBeforeCommit` 与 `Store::wait_paused`、`Store::release`

- [ ] **Step 1: 写失败的测试**：`session_manifest_roundtrip`、`lifecycle_moves_forward_by_compare_and_swap`、`unknown_session_keeps_its_scene`、`seeds_are_per_path_rows_written_once`、`move_intent_generation_never_reused`、`finish_move_swaps_base_head_and_generation_together`、`failed_move_marks_workspace_unknown`、`move_conflicts_are_recorded_at_swapping`、`resolve_move_abandon_returns_health_to_ok`、`resolve_move_accept_keeps_the_conflicts_recorded_at_swapping`、`resolve_move_guards_refuse_and_write_nothing`、`open_proposals_skip_removed_sessions`、`retire_dir_moves_only_dirs_without_live_rows`、`retire_dir_takes_dirs_of_removed_sessions`、`retire_dir_takes_swapped_out_trees`、`retire_dir_takes_superseded_admin_dirs`、`retire_dir_takes_base_staging`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-store --features test-seams --test sessions`
Expected: FAIL（这些函数还不存在）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.7 的会话事务、比较并交换的生命周期与意图（区分不存在与不是 from）、按路径的种子行、意图代数的分配、进入 Swapping 时把冲突原样记在意图行上、`finish_move` 核对冲突并在一个事务里换掉 base、head、代数与冲突、`fail_move` 记下现场、`resolve_move` 的守卫与两条边（先改工作区、再改意图，现场留作历史）；`proposals(_, true)` 的会话过滤；§5.5 的 `retire_dir`（七种路径：前六种按行核对，第 7 种基线暂存只看形状；经 `apfs::open_rooted` 打开，符号链接给 `InvalidInput`、不是目录给 `NotDirectory`；把这个句柄交给 `apfs::make_removable`；`apfs::rename_rooted(…, RenameMode::Excl)` 进 trash，只对 trash 一侧 `apfs::full_fsync`）；写事务助手里提交之前的停住点
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-store --features test-seams --test sessions`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-store/src/sessions.rs crates/strata-store/src/retire.rs crates/strata-store/src/txn.rs crates/strata-store/src/seams.rs crates/strata-store/tests/sessions.rs
git commit -m "feat(store): sessions, workspaces, move intents and retire_dir"
```

### Task 6: checkpoint

**Files:**
- Create: `crates/strata-store/src/checkpoints.rs`
- Modify: `crates/strata-store/src/seams.rs`
- Test: `crates/strata-store/tests/checkpoints.rs`

**Interfaces:**
- Consumes: `strata_types::{Checkpoint, CheckpointId, CheckpointReason, OverlayEntry, EntryKind, BaseGen}`；Task 2 的 blob 文件布局与 L3；Task 5 的会话与工作区行
- Produces: `Store::record_checkpoint`、`Store::checkpoint`、`Store::latest_checkpoint`、`Store::checkpoints`、`CheckpointInfo`；`Seam::AfterCheckpointRow`

- [ ] **Step 1: 写失败的测试**：`record_checkpoint_roundtrip_keeps_base`、`checkpoint_ids_monotone_per_session`、`failed_record_consumes_no_id`、`checkpoint_id_refuses_past_i64_max`、`ids_survive_reopen`、`negative_id_read_back_is_corrupt`、`checkpoint_read_with_missing_entries_is_corrupt`、`tool_call_checkpoint_recorded_once`、`checkpoint_unit_is_one_transaction`、`record_checkpoint_failure_rolls_back_whole_unit`、`late_source_failure_writes_no_checkpoint`、`checkpoint_refuses_reference_to_missing_blob`、`interleaved_sessions_do_not_see_failed_checkpoints`、`checkpoint_refused_for_untracked_or_removed`、`same_path_type_change_is_one_entry`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-store --features test-seams --test checkpoints`
Expected: FAIL（`record_checkpoint` 还不存在）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.8 的十步：纯校验（含同一路径只有一条）、L3 共享锁下在 L2 之外 `lstat` 被引用的 blob、`BEGIN IMMEDIATE` 与 `Busy`、事务内核对（含基线属于这个 repo）、工具调用的幂等、先到者为准的 blob 登记、`MAX + 1` 与 `TooLarge`、批量插入；读回时的 `TryFrom`、负 id 检查与条目行数的核对（`entries_mismatch`）
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-store --features test-seams --test checkpoints`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-store/src/checkpoints.rs crates/strata-store/src/seams.rs crates/strata-store/tests/checkpoints.rs
git commit -m "feat(store): atomic checkpoint records with store-assigned ids"
```

### Task 7: 发布与 ref 审计

**Files:**
- Create: `crates/strata-store/src/records.rs`
- Test: `crates/strata-store/tests/records.rs`

**Interfaces:**
- Consumes: `strata_types::{ReleaseGroup, Batch, Action, CommitOid}`；Task 6 的 checkpoint 行
- Produces: `Store::record_publish`、`Store::set_publish_phase`、`Store::publish_record`、`Store::open_publishes`、`Store::record_ref_change`、`Store::ref_changes`、`PublishRecord`、`PublishPhase`、`RefChange`、`RefOp`、`RefOutcome`

- [ ] **Step 1: 写失败的测试**：`publish_record_roundtrip_and_phase_machine`、`open_publishes_listed_for_restart`、`ref_audit_only_strata_refs_for_update_ref`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-store --features test-seams --test records`
Expected: FAIL（这些函数还不存在）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.9：批次点名的会话的核对（不存在或已 Removed → `NoSuchSession`）、发布行与批次路径原样照抄、`MAX + 1` 的发布号、阶段的比较并交换、ref 审计的 op 与 outcome 配对、`Action` 的 JSON
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-store --features test-seams --test records`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-store/src/records.rs crates/strata-store/tests/records.rs
git commit -m "feat(store): publish records and ref audit"
```

### Task 8: 并发、持久与中毒

**Files:**
- Create: `crates/strata-store/src/vfs.rs`
- Modify: `crates/strata-store/src/txn.rs`、`crates/strata-store/src/seams.rs`
- Test: `crates/strata-store/tests/concurrency.rs`、`crates/strata-store/tests/crash.rs`

**Interfaces:**
- Consumes: Task 1 的 LOCK 与写连接、Task 6 的 `record_checkpoint`；`rusqlite::ffi`（插桩 VFS）
- Produces: `Store::is_poisoned`、写者中毒；提交错误分成 `not_written` 与 `uncertain`；`Seam::CommitSyncFails`；`Store::sync_counts` 的 WAL 计数

- [ ] **Step 1: 写失败的测试**：`external_writer_is_busy_without_side_effects`、`readers_never_block_on_writer`、`killed_lock_holder_leaves_no_stale_lock`、`checkpoint_write_is_durable_after_crash`、`commit_issues_full_fsync`、`commit_io_error_is_unknown_and_poisons_writer`、`disk_full_during_checkpoint_keeps_previous_state`（含子进程 `lock_child` 与 `crash_child`）
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-store --features test-seams --test concurrency --test crash`
Expected: FAIL（插桩 VFS、`is_poisoned` 与中毒还不存在；同步计数为 0）

- [ ] **Step 3: 写让测试通过的最少实现**：`test-seams` 下注册包住 SQLite 缺省 `unix` VFS 的插桩 VFS，按文件后缀与 `flags & 0x0F` 分别计数 xSync，并能让下一次 WAL 的 xSync 返回 `SQLITE_IOERR`；提交错误为 `uncertain` 时置 `poisoned`，此后写一律 `Unknown`，`close` 也不写 clean-shutdown 标记；子进程助手（重新执行测试二进制、读确认行、SIGKILL）
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-store --features test-seams --test concurrency --test crash`（磁盘满那个再加 `-- --ignored` 跑一遍）
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-store/src/vfs.rs crates/strata-store/src/txn.rs crates/strata-store/src/seams.rs crates/strata-store/tests/concurrency.rs crates/strata-store/tests/crash.rs
git commit -m "feat(store): busy, reader isolation, crash durability and poisoning"
```

### Task 9: gc

**Files:**
- Create: `crates/strata-store/src/gc.rs`
- Modify: `crates/strata-store/src/blob.rs`、`crates/strata-store/src/seams.rs`
- Test: `crates/strata-store/tests/gc.rs`

**Interfaces:**
- Consumes: `apfs::{open_rooted, attrs_of, Attrs, list_dir, full_fsync, rename_rooted, RenameMode, remove_tree_rooted, RemoveReport, remove_rooted, RemoveKind}` 与 apfs 第 2 轮的 `make_removable`、`RemoveReport::is_complete` 以及接受任何种类条目的 `remove_tree_rooted`（§2 的上游要求）；Task 2 的 `verify_blob` 与 L3；Task 3–7 的全部表
- Produces: `Store::gc`、`Store::gc_repo`、`Store::last_gc_report`、`GcReport`；L3 的独占一侧；`strata_gc_active` 的开关；`Seam::{PauseInPut, GcStopAfterRetire, GcUnlinkFails, AgeFloor, ScrubBudget, RetirePrepFails, GcBatchRows, GcPauseBeforeRows, GcPauseAfterBatch}`

- [ ] **Step 1: 写失败的测试**：`gc_keeps_referenced_generations`、`gc_keeps_base_of_every_retained_checkpoint`、`gc_reclaims_removed_sessions_after_keep`、`gc_keeps_newest_base_per_repo`、`gc_keeps_pending_proposal_deps`、`gc_reclaims_closed_attempts_keeps_failed_scenes`、`gc_never_deletes_young_blobs`、`gc_spares_in_flight_put_with_old_mtime`、`gc_spares_freshly_renamed_generation_dir`、`gc_skips_paths_of_open_intents_and_attempts`、`retiring_a_0555_root_reparents_it`、`retiring_a_uchg_root_clears_the_flag_first`、`gc_rechecks_pins_written_after_its_snapshot`、`gc_refuses_publish_pins_between_batches`、`gc_retires_stale_base_staging`、`gc_sweeps_stale_temp_entries`、`gc_deletes_supersede_chains_across_batches`、`symlink_put_never_retakes_the_blob_gate`、`gc_keeps_open_publish_scenes`、`gc_retires_dirs_through_trash_and_resumes`、`gc_touches_nothing_outside_its_areas`、`gc_never_reclaims_live_session_with_missing_dirs`、`gc_scrub_reports_corrupt_and_resumes_from_cursor`、`corrupt_blob_is_reported_never_overwritten`、`gc_report_persisted_for_status`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-store --features test-seams --test gc`
Expected: FAIL（`gc` 还不存在）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.11：三条守卫（有主的路径、按 mtime 与 ctime 中较晚者的年龄、有引用的 generation）；`gc` 的保留集（R、提议的闭包、尝试、gc 记录）与先子后父的分批删除，每批在自己的写事务里重新核对钉子（被钉住的行不删、目录不挪），提议按 `superseded_by` 的引用顺序删、从不按号；blob 与 tmp 两段持 L3 独占；tmp 与 trash 的每个条目都经 `apfs::remove_tree_rooted` 删，不论种类，store 不按路径改标志，没删干净或被拒绝的进 `stuck`、下一轮再删；带游标的完整性扫描；`gc_repo` 的保留集，挪目录之前先准备根目录（`apfs::make_removable`，准备失败不删行、记进 `stuck`），再删行、再经 `apfs::rename_rooted` 挪目录，经 `apfs::list_dir` 按键解析的活路径清扫（含过了年龄下限的基线暂存，`.staging` 本身不删），变空的目录经 `apfs::remove_rooted` 删；`gc_runs` 的追加（含 `partial`）；put 里临时克隆之后的停住点、gc 删行之前与两批之间的停住点，以及符号链接那一支不再取 L3 的验证
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-store --features test-seams --test gc`，再跑一遍 `cargo test -p strata-store --features test-seams` 确认全部测试文件都绿
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-store/src/gc.rs crates/strata-store/src/blob.rs crates/strata-store/src/seams.rs crates/strata-store/tests/gc.rs
git commit -m "feat(store): gc with guards, trash and integrity scan"
```

## 9. 构建者决定记录

design.md 里标 [交给构建者]、落在本模块的事项：

| 事项（design 里标 [交给构建者]、落在本模块） | 选择 | 理由 | 日期 |
| --- | --- | --- | --- |
| generation 保留期（design §7、§12） | 由 `gc` 与 `gc_repo` 的参数 `keep` 决定，默认值由 daemon.md 定，本文建议 14 天。没有行的东西（tmp、没登记的 blob 与目录）与 trash 另有固定的年龄下限 1 小时，年龄按 mtime 与 ctime 中较晚的那个算 | 保留期是调度策略，daemon 管调度；下限保护 put 与登记之间、改名与登记之间的窗口（增补 4"gc guards"） | 2026-10-22；默认值待定（daemon.md） |
| `rm` 之前最后一次 checkpoint 保留 N 天（design §6.6） | 不单独设 N：removed 会话的全部行（含 BeforeRemove checkpoint）保留到 `removed_ns + keep`，之后由 gc 一起回收 | 一个参数管所有回收，行为可以预期；增补 3 也把这份保留交给 store 的 gc | 2026-10-22 |
| 封存目录的只读手段（design §7、§12），gc 这一侧 | gc 删 trash 用 `apfs::remove_tree_rooted`，它先让目录对属主可写、清掉 `UF_IMMUTABLE` 与 `UF_APPEND` 再删（增补 8），`chflags uchg`、0555 与"仅靠约定"都删得掉；手段本身由 session.md、deps.md 定 | clonefile 会把 `uchg` 一起复制（2026-10-22 实测），无论选哪种手段，gc 都必须删得掉 | 2026-10-22；手段本身待定（session.md、deps.md） |
| 大文件阈值（design §7、§12） | store 没有阈值：put 对任何大小都整份哈希，`read_blob(id, max)` 的 `max` 就是调用方的阈值；64 MiB 的默认值在 config 与 search 一侧 | 阈值是"要不要读内容"的策略，属于读内容的模块；store 只负责存与取（审阅 M8 的裁定） | 2026-10-22 |

本文自己的 [我的决定]，每条在正文里都有标注：

| 事项（design 里标 [交给构建者]、落在本模块） | 选择 | 理由 | 日期 |
| --- | --- | --- | --- |
| SQLite 的来源（§2） | `rusqlite` 开 `bundled` | 版本与编译选项随 crate 固定，系统更新改不了持久语义 | 2026-10-22 |
| 哈希大文件（§2） | `blake3` 开 `mmap` 与 `rayon`，16 MiB 起走 mmap 加多线程 | 大文件的哈希是 checkpoint 的主要成本之一 | 2026-10-22 |
| 清单怎么登记（§3、§5.5） | `record_base_gen(gen, manifest)` 一个事务登记两者，删去单独的 `record_manifest`，不设 `put_manifest` | 增补 3 让 store 选；清单不会没有 generation 而单独存在，调用方只有 session 一个 | 2026-10-22 |
| 清单重登记不同（§5.5） | 同一 (repo, tree) 的清单不同时返回 `InvalidInput`（`manifest_mismatch`） | 同一个 tree 导出了不同的字节，多半是检出过滤的配置变了，要人来看 | 2026-10-22 |
| `record_proposal` 的位置核对（§3、§5.6） | 多一个 `replaces` 参数，位置上的那条与它不符时什么都不写、返回 `Ok(None)` | deps.md 原来在一个写事务里先读后写；换成具名方法之后，靠它守住读与写之间的变化 | 2026-10-22；待 deps.md 修订轮确认 |
| `set_deps_main` 的"相等"（§5.6） | 现有行与 `expected` 按写入时的列值六列逐列相等，含 `observed_ns` | 任何一列不同都说明读与写之间有别的观察写过；只比 set 会把 set 相同而 tree 或 head 更新了的一行当成没变，再用过时的观察盖掉它 | 2026-10-22；待 deps.md 修订轮确认 |
| 一个依赖集至多一条 Running 的尝试（§4.3、§5.6） | 部分唯一索引 `deps_attempts_one_running`；第二次 begin 返回 `Exists` | T8 裁定让下一个持锁者先把死掉的尝试关成 Failed；索引让"先开始再关"失败 | 2026-10-22 |
| 会话行的阶段（§3） | `sessions.phase` 文本列，只在 Creating、Removing 时可写 | 增补 4 只给意图行定了 phase；new 与 rm 也是多步操作，recover 要知道它们停在哪一步 | 2026-10-22；待 session.md 修订轮确认 |
| 种子何时可写（§3、§5.7） | 只在会话为 Creating 时，每个路径只写一次 | 种子只在 new 时拷进来 | 2026-10-22 |
| 会话的现场（§3） | 离开 Unknown 之后 `scene` 作为历史留着 | 现场是 status 与人排查的依据，覆盖掉就没了 | 2026-10-22 |
| 意图的状态（§3、§4.3） | `MoveState`：Preparing、Swapping、Done、Refused、Abandoned、Unknown；Preparing→Swapping、Refused、Abandoned，Swapping→Done、Unknown；Unknown→Done、Abandoned 只经 `resolve_move`（module-rulings 的"Resolving a failed move"） | 增补 4 只定了"意图行在、关闭不删"与 refused、abandoned 两个终态；session.md 采用或在它的 §2 列出要改的 | 2026-10-22；待 session.md 修订轮确认 |
| 意图的暂存放在哪（§3、§5.7） | `staging` 与 `old_tree` 都必须是 `<root>/sessions/<id>/<name>`：`<name>` 是一个组件，不是该会话任何工作区的目录名 | 名字归 session；这样它们不会落进工作区或 store 的区域，`retire_dir` 的第 5 行总能拿走它们 | 2026-10-22；待 session.md 修订轮确认 |
| 意图上的冲突怎样写（§3、§5.7） | `set_move_state` 进入 Swapping 时带 `conflicts`（别的边不能带），原样写进意图行；不另开方法 | session.md 的 D46 把"加参数还是另开方法"交给 store；同 `set_lifecycle` 的 scene，一次比较并交换就把状态与冲突一起写下 | 2026-10-22 |
| `finish_move` 的冲突（§3、§5.7） | 保留 `conflicts` 参数，它必须与意图行上的那份相同（同样的路径、同样的顺序），否则 `InvalidInput`（`conflicts_mismatch`）；写进工作区的是意图行上的那份 | session.md @cec336b 照这个签名调用，两次传的是同一份 `rebase` 结果；Accept 用的是意图行上的那份，不核对的话两条路记下的冲突可能不同 | 2026-10-22 |
| `resolve_move` 的时刻（§3、§5.7） | 意图离开 Unknown 时 `ended_ns` 改成 `at` | 同 `set_publish_phase` 的 Unknown → Done：每次转换把 `at` 写进 `ended_ns`；失败时的情形留在工作区的现场里 | 2026-10-22 |
| 意图不引用基线（§4.3） | `move_intents` 只记 `to_tree`、`to_deps` 两列，不是外键；Preparing、Swapping、Unknown 的意图由 gc 的保留集钉住目标 | 结束了的意图是历史，不该把基线钉到会话被回收为止；Unknown 的意图还要留给 `resolve_move` 的 Accept | 2026-10-22 |
| 基线属于同一个 repo（§4.3） | 复合外键 `(repo, base_gen)` 加写之前的核对，两者都做 | 外键兜底；核对给出能区分的 `InvalidInput` | 2026-10-22 |
| 发布阶段的转换（§3） | Running→Done、Running→Unknown、Unknown→Done | 与阶段词汇一起暂定（增补 4）；publish.md 采用或在它的 §2 列出要改的 | 2026-10-22；待 publish.md（T10）与 T15 |
| ref 审计的操作与结果（§3） | `RefOp`：UpdateRef、Push；`RefOutcome`：Applied、HeadMoved（UpdateRef），Pushed、Refused、Unknown（Push，对应 `PushOutcome`） | 审计要区分本地链的比较并交换与远端推送 | 2026-10-22 |
| 读回类型不派生 serde（§3） | `SessionRecord` 等读回类型是纯数据 | JSON 形状由 session.md 与 cli.md 规定 | 2026-10-22 |
| 输入里的序号（§3） | `Checkpoint.id`、`ReleaseGroup.id` 不读，以返回值为准 | 号由 store 在写事务里分配 | 2026-10-22 |
| 连接的其它设置（§4.2） | `recursive_triggers = 1`、`trusted_schema = 0` 加 `SQLITE_INNOCUOUS`、`busy_timeout` 写 0 读 2000 ms、DEFENSIVE、读连接 `query_only`、`application_id`、`user_version = 1` | REPLACE 绕不过删除守卫；schema 里只能调声明无害的函数；外部写者让写立即失败；认出自己的库 | 2026-10-22 |
| 时间的存法（§4.4） | 有符号的 Unix 纳秒，早于纪元存负数，超出范围 `InvalidInput` | 逐纳秒往返；种子文件的源 mtime 可能早于纪元 | 2026-10-22 |
| 非 UTF-8 的路径（§4.4） | 写入时返回 `InvalidInput` | 路径存 UTF-8 文本，types.md 的路径也都是 UTF-8 | 2026-10-22 |
| 枚举的存法（§4.4） | 一律小写 snake_case，读回按对照表 | 库里只有一种写法 | 2026-10-22 |
| `CheckpointId` 没有空洞（§4.4） | 号与 checkpoint 行在同一个事务里分配，回滚不占号 | 任何人都看不到没提交的号；增补 4 的 T15 一条改写 3a:589 | 2026-10-22 |
| 提议与尝试的号（§4.4） | `AUTOINCREMENT` | gc 删掉最大号之后也不复用，approve 点不到别的提议，暂存目录名不撞 | 2026-10-22 |
| 丢了 store.db（§5.1） | 布局里有条目而没有库时返回 `Corrupt`（`store_db_missing`） | 悄悄建空库会让已有会话失去记录 | 2026-10-22 |
| 建库的 clonefile 探测（§5.1） | 失败时 rmdir 自己刚建的空目录，再返回 `Unsupported` | 类型化的拒绝之后没有净写入 | 2026-10-22 |
| clean-shutdown 标记（§3、§5.2） | `close` 在关掉全部连接之后写下空文件 `store/clean-shutdown` 并 `F_FULLFSYNC`；写者 `open` 时删掉它；中毒的写者不写 | T6 裁定只定了"异常退出之后才跑 quick_check"；标记要在最后一个写之后才出现、在下一个写之前消失，才能可靠地说明"上次正常关闭" | 2026-10-22 |
| 只读打开需要恢复（§5.2） | `ReadOnly`（`recovery_needed`） | 只读连接做不了 WAL 恢复，先启动 daemon | 2026-10-22 |
| apfs 调用的根（§5.2） | `open` 取一次 `<root>` 的 realpath，作为 store 在根之下的 apfs 调用的 `root`；路径的拼接与 `retire_dir` 的前缀核对仍用配置里的 root | apfs 以 `O_NOFOLLOW` 打开根的最后一级，`[paths] root` 本身是符号链接时这些调用一律 `NotDirectory` | 2026-10-22 |
| `put_blob_rooted` 的种类判断（§5.3） | 先 `apfs::attrs_of` 看种类：目录与特殊文件在克隆之前拒绝；符号链接存 `attrs_of` 读出的目标，不克隆；之后才被换掉的，由 `clone_from_rooted` 的 `CloneKind::File` 在克隆之前拒绝 | `clone_from_rooted` 以不跟随的方式打开源，打不开符号链接本身；`attrs_of` 经同样钉住的上级读出链接的目标，也不出根；目录与特殊文件不必再开一次 | 2026-10-22 |
| get 出来的文件（§5.4） | 在 dst 的句柄上调 `apfs::set_file_meta`：先核对是普通文件且 `st_nlink` 为 1（不是就什么都不改，store 返回 `Unknown`），再清 BSD 标志（`UF_COMPRESSED` 除外）、mode 0644、mtime 为当前时刻（atime 不动）；`get_blob` 与 `get_blob_rooted` 一样 | 像 git checkout 出来的文件；旧 mtime 会骗过增量构建；克隆带来的 `uchg` 会让改 mode 得到 `EPERM`（2026-10-22 实测）；句柄上的改动作用在 inode 上，克隆之后 dst 可能已被换成根之外文件的硬链接（2026-10-22 实测，module-rulings 的"Metadata changes through a handle go through apfs::set_file_meta"） | 2026-10-22 |
| `retire_dir` 接受的路径（§5.5） | §5.5 表里的七行。前六行按行核对，第 7 行（基线暂存，module-rulings 的"Base generations are built outside the lock"）只看形状。前六行：会话根与 `sessions/.repos/<session-id>` 在 Removing、Removed 或已没有行时；换下来的旧树、暂存、旧代与没换入的管理目录只在会话 Active 时；被 gc 删掉的尝试的暂存也可拿走 | 前六行就是 session 与 deps 在增补 2、3 之下会拿走的全部目录，第 7 行是锁外建基线留下的暂存；被 gc 回收之后剩下的目录也要能拿走 | 2026-10-22 |
| `retire_dir` 的持久（§5.5） | 改名之后只对 trash 目录 `F_FULLFSYNC`，源父目录不同步；`gc_repo` 挪目录同样 | 增补 3"Durability"：要扛住断电的目录改名用 F_FULLFSYNC，调用方之后提交的行不会比改名先落盘。源父目录那一侧保证的是"原路径上已经没有它"，store 用不着：掉电之后目录在原路径或在 trash 都合法（§6.3） | 2026-10-22 |
| gc 守卫 (a) 的范围（§5.11） | 除非终态的意图与尝试外，Unknown 的意图与 Failed 的尝试点名的路径也不碰 | 它们是现场（不变量 6） | 2026-10-22 |
| 年龄下限与 keep 无关（§5.11） | 固定 1 小时，测试用 `AgeFloor` 改 | 它保护的是 put、改名与登记之间的窗口，与保留期无关 | 2026-10-22 |
| gc 分批（§5.11） | 每批一个写事务，至多 10 000 行或 10 000 个文件，先子后父 | 不长时间挡住 checkpoint；schema 没有级联 | 2026-10-22 |
| Done 的发布不钉东西（§5.11） | 只有 Running 与 Unknown 的发布钉住它批次里的会话 | Done 之后内容已在 git 里（commit 与 `refs_audit` 记着）；design §7 的"发布记录"由 T15 按增补 4 改写 | 2026-10-22；待 T15 |
| Failed 的尝试（§5.11） | 暂存还在时，行与暂存都留着 | 暂存是现场 | 2026-10-22 |
| 旧的 gc 记录（§5.11） | 过了 keep 就删，每个 scope 最新的一行留着 | `last_gc_report` 只要每个 scope 最新的一行 | 2026-10-22 |
| trash 何时删（§5.11） | 过了年龄下限（1 小时）就删，不再等 keep | 进 trash 的目录都已没有活记录；留 1 小时让按路径读它的进程读完，也给人一个找回的窗口 | 2026-10-22 |
| 完整性扫描的预算（§5.11） | 每轮读满 2 GiB 就停，游标存进 `gc_runs` | 不让一轮 gc 读遍整个 blob 存储 | 2026-10-22 |
| 每个 repo 最新的基线（§5.11） | `gc_repo` 不回收 `created_ns` 最新的那个 generation | 下一个 `new` 多半要用它 | 2026-10-22 |
| store.db 的增长（§5.12） | spike 项：首个 spike 量出每周增长；超过 1 GB 每周就回到用户，决定要不要把条目按内容去重（升 `user_version`）。`quick_check` 已按 T6 裁定改为只在异常退出之后与按需时跑 | checkpoint 是完整快照，估算在 0.5 到 2 GB 之间；去重要在第一次发布之前定 | 2026-10-22；待 spike |
| 准备过的根目录不还原（§5.11、§6.3） | `gc_repo` 准备好根目录、却没有删它的行时（删行之前崩溃；第 3 步中途停下；删行那一批重新核对时它已被快照之后的引用钉住），根目录的写权限与清掉的标志不还原 | 这几个窗口都小，下一轮重算之后照样回收或者留下；还原要再记一份原来的标志与权限，换不来什么；根目录可写不改变里面任何文件的 mode 与标志 | 2026-10-22 |
| dev 端口租约（§4.4 `dev_ports`、§4.4a） | 租约放在 store 的 `dev_ports` 表，随 agent-id 的 v1 → v2 一起迁移；分配在 store 的写事务里，探测策略由 daemon 注入；会话 Removed 时释放 | 租约要跨进程唯一、随会话生命周期走；放在同一个库里，`UNIQUE(base)` 与写事务就是锁，不需要另一份文件 | 2026-10-28 |
| 写者中毒（§6.4） | 提交结果不确定之后，写一律 `Unknown` 直到重开；`is_poisoned()` 报给 daemon | 同步失败之后继续写，可能把没落盘的状态当成已提交 | 2026-10-22 |
| 根目录里的 `search` 与已有 store 旁的其它条目（§3，事故 2026-10-27） | `Store::open` 先看 `store/store.db`：在就直接打开已有 store，不管根目录里还有什么；只有新建时才查陌生条目，`search`（daemon 的搜索索引）也算布局里的名字 | 原来陌生条目检查在 store.db 之前，daemon 建了 `search/` 之后重启就拒绝打开自己的 store | 2026-10-27 |
