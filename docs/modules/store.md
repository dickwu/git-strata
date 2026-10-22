# `store` 模块

**Crate**：`crates/strata-store` · **设计来源**：design.md §5（模块表）、§6.2、§6.6、§7、§8、§9、§11 第 8 条；module-rulings.md 的「T6 store」与标了 T6、Everyone 的增补 · **矩阵**：test-matrix 里目标模块为 `store` 的行（3a 45 行、3c 4 行、3d 5 行、3f 2 行，另加裁定改归 store 的 3c L139），以及理由点名 store 的 14 条 `—` 行

## 1. 职责与边界

store 负责 git-strata 全部的持久元数据和内容存储，落在 `[paths] root`（config.md，默认 `/Volumes/GWD/inter/.strata`）之下：

- **SQLite 元数据**（`<root>/store/store.db`）：基线 generation、依赖集、基线清单、会话与工作区、checkpoint 与 overlay 条目、blob 登记、依赖变更提议、发布记录、ref 变动审计、gc 记录（design §5 模块表、§7）。
- **按 BLAKE3 寻址的 blob 存储**（`<root>/store/blobs/`）：一文件一 blob，存为普通文件；写入 = 从工作区 `clonefile` 进来（零拷贝），读出 = 反向克隆（design §7）。
- **`<root>` 的布局**：基线与依赖集目录的路径由 store 按键算出（§4.1）。目录由 session、deps 建，由 store 登记，只由 store 的 gc 回收。
- **回收**：`gc` 按引用计数与保留期删除行与目录，是全模块唯一发出 `DELETE` 的路径（裁定 T6）。
- **持久与并发规则**：钉死的 SQLite 设置、一个写者、读者永不被写者阻塞、校验策略、序号规则、崩溃之后留下什么（§4–§6）。

不负责的事和它们的去处：

- 计算 overlay、决定哪些文件进 checkpoint、写竞态规则（哈希之后重读 gen count）、restore 的投影：overlay（overlay.md）。store 只存 overlay 交来的条目和 blob。
- 从 git 导出基线树、克隆工作区、种子文件、`move_workspace` 的目录交换、会话生命周期的含义：session（session.md）。store 只存清单行、生命周期列和种子记录。
- 依赖集的认领、冷装、封存，`inputs_hash` 与 `DepsSetId` 的计算，提议的语义：deps（deps.md）。
- 批次与发布组的合成、三方合并、检查、发布链与推送：publish（publish.md）与 gitio。store 只记发布行、批次路径和 ref 变动。
- 什么时候跑 gc、保留期取多少、按 repo 的写锁与 `Busy` 的等待上限、重启之后报出 `Unknown`：daemon（daemon.md）。
- 签发与校验 `ConfirmToken`：confirm。store 只在 ref 审计里存核对过的 `Action`，从不存凭据（types.md §4）。
- 写工作区之前先 checkpoint（不变量 1）：调用方的事。store 从不覆盖工作区里已有的文件，`get_blob` 只新建 `dst`。

## 2. 依赖

Workspace crate：

- `strata-types`：全部标识（`BlobId`、`CheckpointId`、`ProposalId`、`SessionId`、`RepoName`、`RepoPath`、`TreeOid`、`CommitOid`、`DepsSetId`），结构（`BaseGen`、`DepsSet`、`BaseManifest`、`ManifestEntry`、`OverlayEntry`、`EntryKind`、`Checkpoint`、`CheckpointReason`、`SessionManifest`、`WorkspaceState`、`ReleaseGroup`、`Batch`、`Action`），`StrataError` 与 `Code`，以及 `serde_rfc3339` 适配模块。
- `strata-apfs`：`clone_file`（blob 的写入与读出，不跟随 `src` 的符号链接）、`same_volume`（`open` 的同卷检查）、`open_rooted`（gc 删除目录时逐级 `O_NOFOLLOW`，见 apfs.md）。types.md §8 第 1 层只允许 store 依赖 apfs 这一个同层例外；依赖表以 types.md 为准（裁定 T6）。

外部 crate：

- `rusqlite`，开 `bundled` [我的决定]：SQLite 的版本与编译选项随 crate 固定，§4.2 的回读、STRICT 表与 JSON 函数都依赖它；不链接系统 libsqlite3，免得系统更新改变持久语义。
- `blake3`，开 `mmap` 与 `rayon`：算 `BlobId`；16 MiB 及以上的文件走 mmap 加多线程 [我的决定]。
- `libc`：`flock`、`fcntl(F_BARRIERFSYNC)`、`renamex_np(RENAME_EXCL)`、`flistxattr`/`fremovexattr`、`fchflags`、`futimens`、`arc4random_buf`（design §11 第 2 条：macOS API 走 libc）。
- `serde`、`serde_json`：JSON 列（§4.4）与 `GcReport`。
- 开发依赖：`tempfile`（测试根目录）。§7 的 VFS 计数与故障注入用 `rusqlite::ffi`，不另加 crate。

依赖方向：store 在第 1 层，只依赖第 0 层和 apfs，符合 types.md §8。

谁调用 store：

- overlay：`put_blob`、`put_bytes`、`get_blob`、`read_blob`、`verify_blob`、`record_checkpoint`、checkpoint 的读、`manifest`。
- deps：`deps_dir`、`record_deps_set`、`deps_set`、`record_proposal`、`decide_proposal`、`proposal`、`proposals`。
- session：`base_dir`、`record_base_gen`、`has_base_gen`、`create_session`、`set_lifecycle`、`set_workspace_base`、`record_seeds`、`session`、`sessions`、`retire_dir`。
- `record_manifest`：建基线清单的模块调用，是 session 还是 overlay 由它们的文档定；store 只存。
- publish：`record_publish`、`set_publish_phase`、`record_ref_change`、`ref_changes`、checkpoint 的读、`read_blob`。
- daemon：`open`、`gc`、`last_gc_report`、`open_publishes`，以及 status 要的全部读。
- cli：daemon 不在时用 `open_read_only` 加读（design §5）。
- search 不直接调用，它从 overlay 拿 `Overlay`；层次上允许，search.md 需要时可以读。

## 3. 公开接口（Rust）

跨模块类型全部来自 types.md。本节另外定义 store 自有的公开类型：`Lifecycle`、`SessionRecord`、`CheckpointInfo`、`ProposalRow`、`ProposalStatus`、`PublishRecord`、`PublishPhase`、`RefChange`、`RefOp`、`RefOutcome`、`GcReport`，以及只在测试里出现的 `FaultPoint`、`SyncCounts`。`Checkpoint` 用的是 Task 15 加进 types.md 的形状：多一个 `base: BaseGen` 字段，记下这个 checkpoint 对着哪个基线（增补裁定 T7/T6/T15）。

```rust
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use serde::{Deserialize, Serialize};
use strata_types::{
    Action, BaseGen, BaseManifest, BlobId, Checkpoint, CheckpointId, CheckpointReason, CommitOid,
    DepsSet, DepsSetId, ProposalId, ReleaseGroup, RepoName, SessionId, SessionManifest,
    StrataError, TreeOid,
};

/// 本模块所有函数的返回类型（types.md §8 的写法）。
pub type Result<T> = std::result::Result<T, StrataError>;

/// 一个打开的 Store。Send + Sync，daemon 用 Arc<Store> 在线程间共享。
/// 字段私有，列出来只为说明结构（计划里的 `Store { root, db }` 保留这两个名字）：
///   root: PathBuf                    [paths] root；Store 本体在 root/store/
///   db: Mutex<Connection>            唯一的写连接（只读打开时是只读连接）
///   readers: Mutex<Vec<Connection>>  只读连接池；读从不取 db 的锁
///   writer_lock: Option<File>        root/store/LOCK 上的 flock；只读打开时为 None
///   blob_gate: RwLock<()>            put、get、record 持共享锁，gc 删 blob 文件时持独占锁
///   gc_active: Arc<AtomicBool>       SQL 函数 strata_gc_active() 的返回值
///   poisoned: AtomicBool             提交结果不确定之后为 true，此后写一律拒绝
pub struct Store { /* 私有 */ }

impl Store {
    // ---- 打开 ----

    /// 以写者身份打开（只有 daemon 调用）。root 是 [paths] root，checkouts 是每张 [repo.<name>] 的 path。
    /// 第一次打开时建布局与 store.db（§5.1）；之后核对文件头、身份、PRAGMA 与 schema（§5.2）。
    /// 同卷检查：root（不存在时取最近的已存在上级）与每个 checkout 各比一次（config.md §4.3）。
    /// 参数比计划多一个 checkouts [我的决定]：同卷检查要知道主 checkout 在哪。
    /// 码：InvalidInput（root 不是绝对路径）、CrossVolume、NotEmpty、NoSuchPath（某个 checkout 不存在）、
    /// Busy（另一个写者持有 LOCK）、Corrupt（文件头、身份、版本、日志模式、schema 不对，丢了 store.db，
    /// SQLite 报库坏）、Unsupported（卷不支持 clonefile，apfs 透传；PRAGMA 回读不符）、Unknown（I/O）。
    pub fn open(root: &Path, checkouts: &[&Path]) -> Result<Store>;

    /// 以只读身份打开（daemon 不在时的 cli）。不取 LOCK、不改任何文件；之后每个写函数返回 ReadOnly。
    /// 码：NoSuchPath（还没有 Store）、Corrupt（同 open）、
    /// ReadOnly（WAL 需要恢复而只读连接做不了，detail.reason = "recovery_needed"）、Unknown。
    pub fn open_read_only(root: &Path) -> Result<Store>;

    /// [paths] root。
    pub fn root(&self) -> &Path;

    /// 是否以只读身份打开。
    pub fn is_read_only(&self) -> bool;

    // ---- blob ----

    /// 把 src 此刻的内容存成 blob，返回它的 BlobId（§5.3）。普通文件经 clonefile 进 tmp/；
    /// 符号链接存它的目标文本（mode 120000 记在条目里，裁定 T7）。克隆之后只看克隆体，
    /// 所以返回的 id 就是存下的那份字节的 BLAKE3；调用方必须用这个返回值，不用自己事先算的哈希。
    /// 同一内容已存在时不新增文件、不回读已存副本，只核对长度并刷新它的 mtime。
    /// 返回之前，blob 的数据和名字都已用 F_BARRIERFSYNC 排在之后任何写入之前。
    /// 码：ReadOnly、NoSuchPath、IsDirectory、NotFile、CrossVolume、Unsupported（后四个由 apfs 透传）、
    /// Corrupt（已存同名 blob 的长度不对）、Unknown（I/O，带 detail.outcome）。
    pub fn put_blob(&self, src: &Path) -> Result<BlobId>;

    /// 把一段字节存成 blob（三方投影的结果、符号链接的目标）。规则同 put_blob。
    /// 码：ReadOnly、Corrupt、Unknown。
    pub fn put_bytes(&self, bytes: &[u8]) -> Result<BlobId>;

    /// 把 blob 克隆成 dst。dst 不能存在；clonefile 要么建出完整的文件，要么不建。
    /// 受信任读：不重算哈希（裁定 T6）。产物像 git checkout 出来的文件：mode 0644（可执行位由调用方
    /// 按条目的 mode 设），没有拷来的扩展属性，atime 与 mtime 为当前时刻。
    /// 符号链接条目不用它（得到的是普通文件），用 read_blob 取目标文本。
    /// 码：Corrupt（blob 没有登记、文件不在、长度与登记不符）、Exists（dst 已存在）、NoSuchPath（dst 的上级不存在）、
    /// CrossVolume（apfs 透传）、Unknown（dst 建出之后的步骤失败：detail.outcome = "partial"，dst 留在原处）。
    pub fn get_blob(&self, id: &BlobId, dst: &Path) -> Result<()>;

    /// 读出 blob 的全部字节；超过 max 字节就不读。受信任读，同 get_blob。
    /// 码：Corrupt、TooLarge（大小超过 max）、Unknown。
    pub fn read_blob(&self, id: &BlobId, max: u64) -> Result<Vec<u8>>;

    /// 校验读：重算 blob 文件的 BLAKE3 并与 id 比较。restore --verify 与 gc 的完整性扫描用它，
    /// 从不修改或删除任何东西。
    /// 码：Corrupt（不符、文件不在、长度与登记不符；detail 带期望与实际）、Unknown。
    pub fn verify_blob(&self, id: &BlobId) -> Result<()>;

    // ---- generation 与基线清单 ----

    /// 依赖集目录的路径 root/deps/<repo>/<id>（§4.1），纯计算。deps 在别处建好、封存，
    /// 改名到这里，再调用 record_deps_set。
    pub fn deps_dir(&self, repo: &RepoName, id: &DepsSetId) -> PathBuf;

    /// 基线 generation 目录的路径 root/bases/<repo>/<tree>/<deps>（§4.1），纯计算。session 的用法同上。
    pub fn base_dir(&self, gen: &BaseGen) -> PathBuf;

    /// 登记一个已封存的依赖集。同一 (repo, id) 已登记且各列相同时什么都不写。
    /// 码：ReadOnly、InvalidInput（与已登记的不同）、Busy、Unknown。
    pub fn record_deps_set(&self, set: &DepsSet) -> Result<()>;

    /// 没有登记过返回 None。码：Corrupt、Unknown。
    pub fn deps_set(&self, repo: &RepoName, id: &DepsSetId) -> Result<Option<DepsSet>>;

    /// 登记一个基线 tree 的清单，一个 (repo, tree) 一份，一个事务写完；已登记且相同时什么都不写。
    /// 码：ReadOnly、InvalidInput（与已登记的清单不同）、Busy、Unknown。
    pub fn record_manifest(&self, repo: &RepoName, manifest: &BaseManifest) -> Result<()>;

    /// 没有登记过返回 None；空 tree 返回 entries 为空的清单，两者能区分。码：Corrupt、Unknown。
    pub fn manifest(&self, repo: &RepoName, tree: &TreeOid) -> Result<Option<BaseManifest>>;

    /// 登记一个基线 generation；它的清单与依赖集必须已登记。已登记时什么都不写。
    /// 码：ReadOnly、InvalidInput（清单或依赖集没有登记）、Busy、Unknown。
    pub fn record_base_gen(&self, gen: &BaseGen) -> Result<()>;

    /// 码：Corrupt、Unknown。
    pub fn has_base_gen(&self, gen: &BaseGen) -> Result<bool>;

    /// 把一个没有活记录的目录原子地改名进 root/store/trash/，等 gc 按保留期删除；自己不删任何东西。
    /// 只接受三种路径：root/bases/<repo>/<tree>/<deps> 与 root/deps/<repo>/<deps>（对应的行必须不存在：
    /// 已登记的 generation 只由 gc 回收），以及 root/sessions/<session-id>（会话必须是 Removing 或 Removed）。
    /// 码：ReadOnly、InvalidInput（路径不是这三种、含符号链接、对应的行还在、会话还活着）、NoSuchPath、Unknown。
    pub fn retire_dir(&self, path: &Path) -> Result<()>;

    // ---- 会话与工作区 ----

    /// 一个事务写入会话行（lifecycle = Creating）和每个 WorkspaceState 一行工作区。
    /// 码：ReadOnly、Exists（会话 id、root 或某个工作区路径已存在）、
    /// InvalidInput（某个基线没有登记；repos 为空；工作区路径不是 root/<repo>）、Busy、Unknown。
    pub fn create_session(&self, manifest: &SessionManifest, at: SystemTime) -> Result<()>;

    /// 比较并交换生命周期：当前是 from 时改成 to 并返回 true；当前不是 from 返回 false，什么都不写。
    /// 只允许 Creating→Active、Creating→Removing、Active→Removing、Removing→Removed；
    /// 进入 Removed 时记下 at。
    /// 码：ReadOnly、NoSuchSession、InvalidInput（不允许的转换）、Busy、Unknown。
    pub fn set_lifecycle(&self, id: &SessionId, from: Lifecycle, to: Lifecycle, at: SystemTime) -> Result<bool>;

    /// 记下 move_workspace 的结果：一条 UPDATE 同时换掉 base（tree 与 deps 一起）和 head（不变量 8）。
    /// 码：ReadOnly、NoSuchSession（不存在或已 Removed）、NoSuchRepo、InvalidInput（新基线没有登记）、Busy、Unknown。
    pub fn set_workspace_base(&self, id: &SessionId, repo: &RepoName, base: &BaseGen, head: &CommitOid) -> Result<()>;

    /// 记下一个工作区的种子文件记录。形状由 session.md 规定，store 原样存 JSON；只能写一次。
    /// 码：ReadOnly、NoSuchSession、NoSuchRepo、InvalidInput（已经写过）、Busy、Unknown。
    pub fn record_seeds(&self, id: &SessionId, repo: &RepoName, seeds: &serde_json::Value) -> Result<()>;

    /// 码：Corrupt、Unknown。
    pub fn session(&self, id: &SessionId) -> Result<Option<SessionRecord>>;

    /// 全部会话，含 Removed 而尚未被 gc 回收的，按创建时间排序。码：Corrupt、Unknown。
    pub fn sessions(&self) -> Result<Vec<SessionRecord>>;

    // ---- checkpoint ----

    /// 记一个 checkpoint（§5.6），返回 store 分配的 CheckpointId。
    /// 输入的 cp.id 不读 [我的决定]：序号在同一个写事务里分配为该会话已提交的最大值加 1，
    /// 写进行里并返回，调用方以返回值为准。cp.base 必须已登记；cp.entries 引用的 blob 必须
    /// 已由 put_blob 或 put_bytes 存好。reason 是 ToolCall { id }、而同一会话同一 repo 已经记过
    /// 这个工具调用 id 时，什么都不写，返回已有的序号。checkpoint 行、全部 overlay 条目与 blob 登记
    /// 在一个事务里提交；提交即持久（synchronous = FULL）。
    /// 码：ReadOnly、NoSuchSession（不存在或已 Removed）、NoSuchRepo、
    /// InvalidInput（条目不一致、工作区 tracked = false、基线没有登记）、Corrupt（引用的 blob 文件不在或长度不对）、
    /// TooLarge（序号到了 i64::MAX）、Busy（外部进程持有 SQLite 写锁）、
    /// Unknown（I/O；detail.outcome 为 "not_written" 或 "uncertain"；写者已中毒）。
    pub fn record_checkpoint(&self, cp: Checkpoint) -> Result<CheckpointId>;

    /// 读回一个完整的 checkpoint：含 base 与全部条目，条目按 path 排序。
    /// 码：NoSuchCheckpoint、Corrupt（读回的标识或序号不合法）、Unknown。
    pub fn checkpoint(&self, session: &SessionId, id: CheckpointId) -> Result<Checkpoint>;

    /// 该会话、该 repo 序号最大的 checkpoint；一个都没有时返回 None。码：Corrupt、Unknown。
    pub fn latest_checkpoint(&self, session: &SessionId, repo: &RepoName) -> Result<Option<Checkpoint>>;

    /// checkpoint 摘要（不含条目），按序号递增；repo 为 None 时列出全部 repo。码：Corrupt、Unknown。
    pub fn checkpoints(&self, session: &SessionId, repo: Option<&RepoName>) -> Result<Vec<CheckpointInfo>>;

    // ---- 依赖变更提议 ----

    /// 记一个依赖变更提议，返回 store 分配的 ProposalId。输入的 id、status、decided 不读，新行是 Pending。
    /// 码：ReadOnly、InvalidInput（from 或 to 依赖集没有登记）、TooLarge、Busy、Unknown。
    pub fn record_proposal(&self, row: &ProposalRow) -> Result<ProposalId>;

    /// 比较并交换：提议仍是 Pending 时改成 to（Approved 或 Superseded）并返回 true；
    /// 提议不存在或已决定时返回 false，调用方用 proposal 区分并给出 NoSuchProposal。
    /// 码：ReadOnly、InvalidInput（to 是 Pending）、Busy、Unknown。
    pub fn decide_proposal(&self, id: ProposalId, to: ProposalStatus, at: SystemTime) -> Result<bool>;

    /// 码：Corrupt、Unknown。
    pub fn proposal(&self, id: ProposalId) -> Result<Option<ProposalRow>>;

    /// status 为 None 时列出全部；按 id 递增。码：Corrupt、Unknown。
    pub fn proposals(&self, status: Option<ProposalStatus>) -> Result<Vec<ProposalRow>>;

    // ---- 发布与 ref 审计 ----

    /// 记一次发布的开始：写 publishes 行（phase = Running），每个批次的每个路径写一行 publish_items，
    /// 返回 store 分配的 ReleaseGroup.id（输入的 group.id 不读）。路径原样照抄，不展开改名。
    /// 码：ReadOnly、NoSuchCheckpoint（某个批次的 (session, checkpoint) 不存在）、
    /// InvalidInput（batches 为空或某个批次的 paths 为空）、TooLarge、Busy、Unknown。
    pub fn record_publish(&self, group: &ReleaseGroup, at: SystemTime) -> Result<u64>;

    /// 比较并交换发布的阶段：只允许 Running→Done、Running→Unknown、Unknown→Done。
    /// outcome 是 publish.md 规定的 JSON 摘要。离开 Running 时记下 at。
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

    /// 回收（§5.9）：删除超过保留期 keep、且没有任何引用的行与目录，清理 tmp/ 与 trash/，
    /// 再做一段完整性扫描，把报告追加进 gc_runs 并返回。全模块只有这里发出 DELETE。
    /// blob、临时文件与未登记的目录另有固定的年龄下限 1 小时。
    /// 码：ReadOnly、Busy、Unknown（中途 I/O：detail.outcome = "partial"，已提交的批次有效，下一轮接着做）。
    pub fn gc(&self, keep: Duration) -> Result<GcReport>;

    /// 最近一次 gc 的报告，status 用它报出损坏的 blob。码：Corrupt、Unknown。
    pub fn last_gc_report(&self) -> Result<Option<GcReport>>;
}

/// 会话的生命周期（sessions.lifecycle）。各状态的含义由 session.md 规定，store 只保证转换图（§4.3 的触发器）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Lifecycle { Creating, Active, Removing, Removed }

/// 从 store 读回的一个会话：types.md 的 SessionManifest，加上 types.md §4 说明里交给 store 保存的
/// 创建时间与状态。这几个读回类型是纯数据，不派生 serde [我的决定]：JSON 形状由 cli.md 规定。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRecord {
    pub manifest: SessionManifest,
    pub lifecycle: Lifecycle,
    pub created: SystemTime,
    /// 进入 Removed 的时刻。
    pub removed: Option<SystemTime>,
    /// record_seeds 写下的种子记录；没写过的 repo 不在表里。
    pub seeds: BTreeMap<RepoName, serde_json::Value>,
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

/// proposals 表的一行。deps.md §3 的 Proposal 与它互相转换；Proposal 的形状以 deps.md 为准，store 只存这些列。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProposalRow {
    pub id: ProposalId,
    pub repo: RepoName,
    /// 提议时钉着的依赖集，与新依赖集；两者都必须已登记。
    pub from: DepsSetId,
    pub to: DepsSetId,
    /// 新依赖集所属的 tree：批准时会话移到这个 tree 上（design §6.4）。不知道时为 None。
    pub target_tree: Option<TreeOid>,
    /// 提出变更的会话；None 表示主干（design §6.4 第 1 步）。
    pub origin: Option<SessionId>,
    pub status: ProposalStatus,
    pub created: SystemTime,
    pub decided: Option<SystemTime>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProposalStatus { Pending, Approved, Superseded }

/// 一次发布的记录。group.id 是 store 分配的号，batches 与每批的 paths 保持记录时的顺序。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishRecord {
    pub group: ReleaseGroup,
    pub phase: PublishPhase,
    pub started: SystemTime,
    pub ended: Option<SystemTime>,
    pub outcome: Option<serde_json::Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RefOp { UpdateRef, Push }

/// UpdateRef 只取 Applied、HeadMoved；Push 只取 Pushed、Refused、Unknown，
/// 与 types.md 的 PushOutcome 三个变体一一对应。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RefOutcome { Applied, HeadMoved, Pushed, Refused, Unknown }

/// 一次 gc 的报告，以 JSON 存进 gc_runs.report，也返回给调用方。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GcReport {
    #[serde(with = "strata_types::serde_rfc3339")]
    pub started: SystemTime,
    #[serde(with = "strata_types::serde_rfc3339")]
    pub ended: SystemTime,
    pub keep_secs: u64,
    /// 下面是各类被删掉的行数。
    pub sessions: u64,
    pub checkpoints: u64,
    pub base_gens: u64,
    pub base_trees: u64,
    pub deps_sets: u64,
    pub proposals: u64,
    pub blobs: u64,
    /// 被删的 blob 的逻辑字节数；CoW 之下实际释放的空间可能更少。
    pub blob_bytes: u64,
    /// 改名进 trash 的目录数。
    pub dirs_retired: u64,
    pub trash_removed: u64,
    pub tmp_removed: u64,
    /// 删不掉、留在原处的 trash 条目。
    pub stuck: Vec<PathBuf>,
    /// 本轮完整性扫描核对过的 blob 数。
    pub verified: u64,
    /// 扫描发现的损坏 blob（重算的 BLAKE3 不符或文件不在）。从不删除，status 报出。
    pub corrupt: Vec<BlobId>,
}

/// 测试钩子（cargo feature `fault-injection`，只在测试里打开，§8 Task 1）。
#[cfg(feature = "fault-injection")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FaultPoint {
    /// 下一次 put 在改名进 blobs/ 时返回这个 errno。
    BlobRename { errno: i32 },
    /// 下一次 put 生成的第一个临时名视为已被占用。
    TempNameTaken,
    /// 下一次建库在临时库改名到 store.db 之前以 I/O 错误中止。
    InitBeforeRename,
    /// 下一次 record_checkpoint 在插入 checkpoint 行之后、插入条目之前以 I/O 错误失败。
    AfterCheckpointRow,
    /// 下一次 COMMIT 时，插桩的 VFS 让 WAL 的 xSync 返回 I/O 错误。
    CommitSyncFails,
    /// 下一个写事务在 BEGIN IMMEDIATE 之后停住，直到 release_fault 放行。
    PauseInWriteTxn,
    /// 下一次 gc 在挪完第 n 个目录之后以 I/O 错误中止。
    GcStopAfterRetire { n: u64 },
    /// 之后每轮 gc 的完整性扫描预算改为这么多字节。
    ScrubBudget { bytes: u64 },
}

/// 插桩 VFS 与 store 自己的同步计数（fault-injection）。
#[cfg(feature = "fault-injection")]
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

#[cfg(feature = "fault-injection")]
impl Store {
    pub fn inject_fault(&self, fault: FaultPoint);
    pub fn release_fault(&self);
    pub fn sync_counts(&self) -> SyncCounts;
}
```

接口上的三条通则：

- **store 分配的序号**：checkpoint、发布组、提议三种行的序号由 store 在写事务里分配。输入结构里的序号字段（`Checkpoint.id`、`ReleaseGroup.id`、`ProposalRow.id`）不读，调用方以返回值为准 [我的决定]。占位值随便填，例如 `CheckpointId::from(0)`。
- **读回不存在的东西**：types.md 把 `NoSuchCheckpoint` 记在 store 名下，所以 `checkpoint` 返回这个码；其余"没有"一律返回 `None` 或 `false`，由调用方给出自己的码（`NoSuchSession`、`NoSuchProposal`……）。
- **线程**：所有方法都是 `&self`。写事务在 `db` 的锁里串行，读从连接池借独立的只读连接。

## 4. 数据与不变量

### 4.1 目录布局

```text
<root>/                                [paths] root（config.md），默认 /Volumes/GWD/inter/.strata
  store/
    store.db  store.db-wal  store.db-shm       SQLite（§4.2、§4.3）
    LOCK                                        写者守卫：open 持有它的 flock，进程退出即由内核释放
    blobs/00/ … blobs/ff/                       256 个扇出目录，建库时一次建好
    blobs/<id 前两位>/<id>                      一个 blob：内容就是文件内容；mode 0444，没有扩展属性，没有 BSD 标志
    tmp/                                        store 自己的临时文件：put 的克隆体（put-<32 位十六进制>）、建库用的临时库
    trash/<unix 毫秒>-<32 位十六进制>/          gc 与 retire_dir 改名进来、等 gc 按保留期删除的目录
  bases/<repo>/<tree>/<deps>/                   基线 generation 目录：session 建，store 登记，gc 回收
  deps/<repo>/<deps>/                           依赖集目录：deps 建，store 登记，gc 回收
  sessions/<session-id>/<repo>/                 工作区：session 拥有，gc 从不进入
```

- `<repo>` 是 `RepoName`，`<tree>` 是 `TreeOid`，`<deps>` 是 `DepsSetId`（types.md §3）。三者的合法字符都不含 `/`，路径与键一一对应（design §7"键在路径里"）。
- 基线目录比 design §6.1 的 `bases/<repo>/<oid>/` 多一级 `<deps>` [我的决定]：同一个 tree 可以配不止一个依赖集（工具链升级、重新解析出的 `@inter/*` SHA 都会改变 `DepsSetId`），而基线目录里装着依赖集的克隆（design §6.1 第 1 步"再把匹配的依赖集克隆进去"），按 `BaseGen` 的三个键分目录才不会互相覆盖。
- 建目录的模块先在自己的地方建好、封存（以 `.` 开头的临时名，或它自己的区域），再改名到 `base_dir`、`deps_dir` 给出的路径，最后登记。store 不规定临时名，只保证 gc 不碰它们（§5.9）。
- 基线树的文件不进 blob 存储 [我的决定]：基线的内容就在基线目录里，restore 把没有覆盖的路径从那里克隆回去（design §6.2）；基线清单只在 SQLite 里记每个文件的 BLAKE3、mode 与大小（`manifest_entries`）。所以 `ManifestEntry.blob` 是内容地址，不保证 `blobs/` 里有这个文件；overlay 条目引用的 blob 一定在（§4.5 S3）。
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
| `recursive_triggers` | 1（写连接） | `INSERT OR REPLACE` 的隐式删除也会触发 §4.3 的删除守卫，REPLACE 绕不过"只有 gc 删除"（2026-10-22 用 SQLite 3.50/3.54 实测） |
| `busy_timeout` | 写连接 0，读连接 2000 ms | 外部进程持锁时写立即失败、不重试（3a:620、3a:659）；读只在 WAL 恢复这种极短的独占期里等，普通写事务从不让读等 |
| `SQLITE_DBCONFIG_DEFENSIVE` | 每个连接都开 | 关掉 `writable_schema` 一类改 schema 的后门（沿用 LayerFS 的做法） |
| `query_only` | 1（读连接） | 读连接写不了 |
| `application_id` | 1398035009，即 0x53545241（"STRA"） | 建库时写入；在文件头偏移 68，open 靠它认出这是 Store |
| `user_version` | 1 | 建库时写入；在文件头偏移 60；schema 版本规则见 §4.4 |

`wal_autocheckpoint` 保持 SQLite 的缺省值（1000 页），写者提交时自动做；读事务都很短，不会让 WAL 无限增长。

### 4.3 完整 DDL

建库时在一个事务里按顺序执行（§5.1 第 8 步）。open 时把同一段文本在 `:memory:` 库里执行一遍，再与 store.db 的 `sqlite_schema` 逐行比对（type、name、tbl_name、sql），任何差别都是 `Corrupt`（§5.2 第 5 步）。触发器里的 `strata_gc_active()` 是 store 在每个写连接上注册的 SQL 函数，只在 gc 运行时返回 1；没有注册它的连接（例如人手开的 `sqlite3`）执行 DELETE 会因为找不到这个函数而失败，同样删不掉。这段 DDL 在 2026-10-22 用 SQLite 3.50.6 建过库，并逐条核对了下面的约束与触发器。

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
    created_ns  INTEGER NOT NULL,
    PRIMARY KEY (repo, id)
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
    FOREIGN KEY (repo, tree) REFERENCES base_trees (repo, tree) ON DELETE CASCADE
) STRICT;

CREATE TABLE base_gens (
    id         INTEGER PRIMARY KEY CHECK (id >= 1),
    repo       TEXT    NOT NULL,
    tree       TEXT    NOT NULL,
    deps       TEXT    NOT NULL,
    created_ns INTEGER NOT NULL,
    UNIQUE (repo, tree, deps),
    FOREIGN KEY (repo, tree) REFERENCES base_trees (repo, tree),
    FOREIGN KEY (repo, deps) REFERENCES deps_sets (repo, id)
) STRICT;
CREATE INDEX base_gens_by_repo ON base_gens (repo, created_ns);

CREATE TABLE sessions (
    id         TEXT    NOT NULL PRIMARY KEY CHECK (id GLOB 's-[a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7]'),
    ticket     TEXT,
    root       TEXT    NOT NULL UNIQUE,
    lifecycle  TEXT    NOT NULL CHECK (lifecycle IN ('creating', 'active', 'removing', 'removed')),
    created_ns INTEGER NOT NULL,
    removed_ns INTEGER,
    CHECK ((lifecycle = 'removed') = (removed_ns IS NOT NULL))
) STRICT;

CREATE TABLE workspaces (
    session  TEXT    NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    repo     TEXT    NOT NULL,
    base_gen INTEGER NOT NULL REFERENCES base_gens (id),
    head     TEXT    NOT NULL CHECK (length(head) = 40 AND head NOT GLOB '*[^0-9a-f]*'),
    path     TEXT    NOT NULL UNIQUE,
    tracked  INTEGER NOT NULL CHECK (tracked IN (0, 1)),
    seeds    TEXT    CHECK (seeds IS NULL OR json_valid(seeds)),
    PRIMARY KEY (session, repo)
) STRICT;
CREATE INDEX workspaces_by_base_gen ON workspaces (base_gen);

CREATE TABLE checkpoints (
    session     TEXT    NOT NULL,
    id          INTEGER NOT NULL CHECK (id >= 1),
    repo        TEXT    NOT NULL,
    base_gen    INTEGER NOT NULL REFERENCES base_gens (id),
    at_ns       INTEGER NOT NULL,
    reason      TEXT    NOT NULL CHECK (reason IN ('ToolCall', 'Manual', 'BeforePublish', 'BeforeRestore', 'BeforeRollout', 'BeforeRemove')),
    reason_text TEXT,
    entries     INTEGER NOT NULL CHECK (entries >= 0),
    PRIMARY KEY (session, id),
    FOREIGN KEY (session, repo) REFERENCES workspaces (session, repo) ON DELETE CASCADE,
    CHECK ((reason IN ('ToolCall', 'Manual')) = (reason_text IS NOT NULL))
) STRICT;
CREATE UNIQUE INDEX checkpoints_by_tool_call ON checkpoints (session, repo, reason_text) WHERE reason = 'ToolCall';
CREATE INDEX checkpoints_by_repo ON checkpoints (session, repo, id);
CREATE INDEX checkpoints_by_base_gen ON checkpoints (base_gen);

CREATE TABLE blobs (
    id         TEXT    NOT NULL PRIMARY KEY CHECK (length(id) = 64 AND id NOT GLOB '*[^0-9a-f]*'),
    size       INTEGER NOT NULL CHECK (size >= 0),
    created_ns INTEGER NOT NULL
) STRICT;

CREATE TABLE overlay_entries (
    session    TEXT    NOT NULL,
    checkpoint INTEGER NOT NULL,
    path       TEXT    NOT NULL,
    kind       TEXT    NOT NULL CHECK (kind IN ('Added', 'Modified', 'Deleted', 'Renamed', 'Untracked')),
    blob       TEXT    REFERENCES blobs (id),
    from_path  TEXT,
    mode       INTEGER NOT NULL,
    PRIMARY KEY (session, checkpoint, path),
    FOREIGN KEY (session, checkpoint) REFERENCES checkpoints (session, id) ON DELETE CASCADE,
    CHECK ((kind = 'Deleted') = (blob IS NULL)),
    CHECK ((kind = 'Renamed') = (from_path IS NOT NULL)),
    CHECK ((kind = 'Deleted' AND mode = 0) OR (kind <> 'Deleted' AND mode IN (33188, 33261, 40960)))
) STRICT;
CREATE UNIQUE INDEX overlay_entries_by_from ON overlay_entries (session, checkpoint, from_path) WHERE from_path IS NOT NULL;
CREATE INDEX overlay_entries_by_blob ON overlay_entries (blob);

CREATE TABLE proposals (
    id          INTEGER PRIMARY KEY AUTOINCREMENT CHECK (id >= 1),
    repo        TEXT    NOT NULL,
    from_deps   TEXT    NOT NULL,
    to_deps     TEXT    NOT NULL,
    target_tree TEXT    CHECK (target_tree IS NULL OR (length(target_tree) = 40 AND target_tree NOT GLOB '*[^0-9a-f]*')),
    origin      TEXT,
    status      TEXT    NOT NULL CHECK (status IN ('pending', 'approved', 'superseded')),
    created_ns  INTEGER NOT NULL,
    decided_ns  INTEGER,
    FOREIGN KEY (repo, from_deps) REFERENCES deps_sets (repo, id),
    FOREIGN KEY (repo, to_deps) REFERENCES deps_sets (repo, id),
    CHECK ((status = 'pending') = (decided_ns IS NULL))
) STRICT;
CREATE INDEX proposals_by_status ON proposals (status, repo);

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
    started_ns  INTEGER NOT NULL,
    ended_ns    INTEGER NOT NULL,
    report      TEXT    NOT NULL CHECK (json_valid(report)),
    scrub_after TEXT    CHECK (scrub_after IS NULL OR (length(scrub_after) = 64 AND scrub_after NOT GLOB '*[^0-9a-f]*'))
) STRICT;

CREATE TRIGGER deps_sets_no_update        BEFORE UPDATE ON deps_sets        BEGIN SELECT RAISE(ABORT, 'immutable: deps_sets'); END;
CREATE TRIGGER base_trees_no_update       BEFORE UPDATE ON base_trees       BEGIN SELECT RAISE(ABORT, 'immutable: base_trees'); END;
CREATE TRIGGER manifest_entries_no_update BEFORE UPDATE ON manifest_entries BEGIN SELECT RAISE(ABORT, 'immutable: manifest_entries'); END;
CREATE TRIGGER base_gens_no_update        BEFORE UPDATE ON base_gens        BEGIN SELECT RAISE(ABORT, 'immutable: base_gens'); END;
CREATE TRIGGER checkpoints_no_update      BEFORE UPDATE ON checkpoints      BEGIN SELECT RAISE(ABORT, 'immutable: checkpoints'); END;
CREATE TRIGGER blobs_no_update            BEFORE UPDATE ON blobs            BEGIN SELECT RAISE(ABORT, 'immutable: blobs'); END;
CREATE TRIGGER overlay_entries_no_update  BEFORE UPDATE ON overlay_entries  BEGIN SELECT RAISE(ABORT, 'immutable: overlay_entries'); END;
CREATE TRIGGER publish_items_no_update    BEFORE UPDATE ON publish_items    BEGIN SELECT RAISE(ABORT, 'immutable: publish_items'); END;
CREATE TRIGGER refs_audit_no_update       BEFORE UPDATE ON refs_audit       BEGIN SELECT RAISE(ABORT, 'immutable: refs_audit'); END;
CREATE TRIGGER gc_runs_no_update          BEFORE UPDATE ON gc_runs          BEGIN SELECT RAISE(ABORT, 'immutable: gc_runs'); END;

CREATE TRIGGER sessions_update_guard BEFORE UPDATE ON sessions
WHEN NEW.id IS NOT OLD.id OR NEW.ticket IS NOT OLD.ticket OR NEW.root IS NOT OLD.root OR NEW.created_ns IS NOT OLD.created_ns
  OR NOT ((OLD.lifecycle = 'creating' AND NEW.lifecycle IN ('active', 'removing'))
       OR (OLD.lifecycle = 'active' AND NEW.lifecycle = 'removing')
       OR (OLD.lifecycle = 'removing' AND NEW.lifecycle = 'removed'))
BEGIN SELECT RAISE(ABORT, 'illegal update: sessions'); END;
CREATE TRIGGER workspaces_update_guard BEFORE UPDATE ON workspaces
WHEN NEW.session IS NOT OLD.session OR NEW.repo IS NOT OLD.repo OR NEW.path IS NOT OLD.path
  OR NEW.tracked IS NOT OLD.tracked OR (OLD.seeds IS NOT NULL AND NEW.seeds IS NOT OLD.seeds)
BEGIN SELECT RAISE(ABORT, 'illegal update: workspaces'); END;
CREATE TRIGGER proposals_update_guard BEFORE UPDATE ON proposals
WHEN NEW.id IS NOT OLD.id OR NEW.repo IS NOT OLD.repo OR NEW.from_deps IS NOT OLD.from_deps OR NEW.to_deps IS NOT OLD.to_deps
  OR NEW.target_tree IS NOT OLD.target_tree OR NEW.origin IS NOT OLD.origin OR NEW.created_ns IS NOT OLD.created_ns
  OR NOT (OLD.status = 'pending' AND NEW.status IN ('approved', 'superseded'))
BEGIN SELECT RAISE(ABORT, 'illegal update: proposals'); END;
CREATE TRIGGER publishes_update_guard BEFORE UPDATE ON publishes
WHEN NEW.id IS NOT OLD.id OR NEW.started_ns IS NOT OLD.started_ns
  OR NOT ((OLD.phase = 'running' AND NEW.phase IN ('done', 'unknown')) OR (OLD.phase = 'unknown' AND NEW.phase = 'done'))
BEGIN SELECT RAISE(ABORT, 'illegal update: publishes'); END;

CREATE TRIGGER deps_sets_delete_gc_only        BEFORE DELETE ON deps_sets        WHEN NOT strata_gc_active() BEGIN SELECT RAISE(ABORT, 'delete outside gc: deps_sets'); END;
CREATE TRIGGER base_trees_delete_gc_only       BEFORE DELETE ON base_trees       WHEN NOT strata_gc_active() BEGIN SELECT RAISE(ABORT, 'delete outside gc: base_trees'); END;
CREATE TRIGGER manifest_entries_delete_gc_only BEFORE DELETE ON manifest_entries WHEN NOT strata_gc_active() BEGIN SELECT RAISE(ABORT, 'delete outside gc: manifest_entries'); END;
CREATE TRIGGER base_gens_delete_gc_only        BEFORE DELETE ON base_gens        WHEN NOT strata_gc_active() BEGIN SELECT RAISE(ABORT, 'delete outside gc: base_gens'); END;
CREATE TRIGGER sessions_delete_gc_only         BEFORE DELETE ON sessions         WHEN NOT strata_gc_active() BEGIN SELECT RAISE(ABORT, 'delete outside gc: sessions'); END;
CREATE TRIGGER workspaces_delete_gc_only       BEFORE DELETE ON workspaces       WHEN NOT strata_gc_active() BEGIN SELECT RAISE(ABORT, 'delete outside gc: workspaces'); END;
CREATE TRIGGER checkpoints_delete_gc_only      BEFORE DELETE ON checkpoints      WHEN NOT strata_gc_active() BEGIN SELECT RAISE(ABORT, 'delete outside gc: checkpoints'); END;
CREATE TRIGGER overlay_entries_delete_gc_only  BEFORE DELETE ON overlay_entries  WHEN NOT strata_gc_active() BEGIN SELECT RAISE(ABORT, 'delete outside gc: overlay_entries'); END;
CREATE TRIGGER blobs_delete_gc_only            BEFORE DELETE ON blobs            WHEN NOT strata_gc_active() BEGIN SELECT RAISE(ABORT, 'delete outside gc: blobs'); END;
CREATE TRIGGER proposals_delete_gc_only        BEFORE DELETE ON proposals        WHEN NOT strata_gc_active() BEGIN SELECT RAISE(ABORT, 'delete outside gc: proposals'); END;
CREATE TRIGGER publishes_no_delete     BEFORE DELETE ON publishes     BEGIN SELECT RAISE(ABORT, 'append-only: publishes'); END;
CREATE TRIGGER publish_items_no_delete BEFORE DELETE ON publish_items BEGIN SELECT RAISE(ABORT, 'append-only: publish_items'); END;
CREATE TRIGGER refs_audit_no_delete    BEFORE DELETE ON refs_audit    BEGIN SELECT RAISE(ABORT, 'append-only: refs_audit'); END;
CREATE TRIGGER gc_runs_no_delete       BEFORE DELETE ON gc_runs       BEGIN SELECT RAISE(ABORT, 'append-only: gc_runs'); END;
```

计划列出的十张表全在：`base_gens`、`deps_sets`、`sessions`、`workspaces`、`checkpoints`、`overlay_entries`、`blobs`、`publishes`、`publish_items`、`refs_audit`。另外四张是 [我的决定]：`base_trees` 与 `manifest_entries` 存基线清单（types.md 的 `BaseManifest` 要持久，不然 daemon 每次重启都得重新哈希整棵基线树）；`proposals` 存依赖变更提议（types.md 说 `ProposalId` 由 Store 分配）；`gc_runs` 存 gc 报告和完整性扫描的游标，只增不删。

### 4.4 列与序号的约定

- **时间**：`*_ns` 列是 Unix 纪元以来的纳秒数（UTC），INTEGER [我的决定]。读回的 `SystemTime` 与写入时逐纳秒相同；早于纪元、或晚于 i64 能表示的范围（2262 年）的时间返回 `InvalidInput`。
- **标识**：十六进制标识存成小写 TEXT，长度与字符由 CHECK 约束。`RepoName`、`RepoPath`、`SessionId` 存它们的字符串形式（types.md §3 的 `Display`）。读回时经 `TryFrom<String>` 重新校验，失败就是 `Corrupt`（`bad_row`）。
- **JSON 列**只有五个，形状由各自的所有者规定：`deps_sets.dirs`（`DepsSet.dirs` 的字符串数组）、`workspaces.seeds`（session.md 的种子记录）、`publishes.outcome`（publish.md 的结果摘要）、`refs_audit.confirmed`（types.md `Action` 的 JSON）、`gc_runs.report`（`GcReport`）。
- **序号**：
  - `CheckpointId`：每个会话一个序列，在 `record_checkpoint` 的写事务里取该会话已提交的最大值加 1。回滚的尝试不占号，下一个提交的 checkpoint 拿紧接着的号，所以序列没有空洞 [我的决定]。这与 3a:589 原文的"失败留下空洞"不同：LayerFS 的预留先发给调用方再使用，strata 的号与 checkpoint 行在同一个事务里出现，任何人都看不到一个没提交的号。提交过的号永不复用，重开之后接着递增。会话的行只会被 gc 整个删除，删除之后没有人再引用它的号。
  - `ProposalId`：`proposals.id` 用 `AUTOINCREMENT` [我的决定]。gc 会删掉已决定的旧提议，AUTOINCREMENT 保证最大号被删之后也不会再发出去，`git-strata deps approve <id>` 永远点不到另一个提议。
  - `ReleaseGroup.id`：`publishes` 的行永不删除，取最大值加 1。
  - 三种号都只写 1 到 `i64::MAX`（types.md §3）。下一个号会超过 `i64::MAX` 时，写操作在写任何东西之前返回 `TooLarge`（`detail.limit` 指明是哪种号），已存的状态不变，不回绕（3a:590、3a:596）。读回小于 1 的值说明库坏了：store 先检查，返回 `Corrupt`（`bad_row`），不把它交给 `From<i64>`（types.md §3）。
  - `base_gens.id`、`refs_audit.id`、`gc_runs.id` 只在 store 内部用，不出现在公开接口上。
- **可变的列**只有这些，其余每张表的每一列写入后都不可改（§4.3 的触发器拦）：`sessions.lifecycle` 与 `removed_ns`；`workspaces.base_gen` 与 `head`（`move_workspace` 的结果）、`workspaces.seeds`（从空写成值，只一次）；`proposals.status` 与 `decided_ns`；`publishes.phase`、`ended_ns` 与 `outcome`。状态列只能沿触发器里的转换图前进。
- **schema 版本**：`user_version = 1`。v1 没有旧版 Store，也不做迁移（3a:609 把版本策略交给本文）：版本号不是 1、`application_id` 不对、日志模式不是 WAL、schema 文本有任何差别的库一律拒绝打开（`Corrupt`），不修补、不转换。以后改 schema 就升版本号，迁移另写设计。

### 4.5 不变量

| # | 不变量 | 落实 design §9 |
| --- | --- | --- |
| S1 | blob 按内容寻址、只增不改：`blobs/<aa>/<id>` 一旦改名到位，字节永不改写（3a:103）；同一内容只存一份；文件名就是内容的 BLAKE3，不另存哈希。 | 不变量 1（恢复依赖它）、反目标 1 |
| S2 | 写入过的行只按 §4.4 列出的可变列改动。`DELETE` 只来自 gc（触发器加 `strata_gc_active()`）；`publishes`、`publish_items`、`refs_audit`、`gc_runs` 连 gc 也不删。文件系统上，store 只在两处删除东西：gc，以及丢弃本次调用自己刚建、从未登记的临时文件（put 的重复克隆、符号链接的克隆、建库的探测文件与空目录）。 | 不变量 5、6 |
| S3 | checkpoint 行提交之前，它引用的每个 blob 都已改名到位、已用 F_BARRIERFSYNC 排在提交之前、并在同一个事务里登记；不存在悬空引用（3a:475、3a:617、3a:672）。 | 不变量 1 |
| S4 | 一个 Store 只有一个写者：写连接只有一个，进程之间由 `LOCK` 的 flock 保证。读走独立的只读连接，WAL 之下从不等写者。 | design §5（daemon 不在时 CLI 只读）、§8 |
| S5 | 序号在 1 到 `i64::MAX` 之间、只增、提交过的永不复用（§4.4）。 | types.md §3 |
| S6 | 工作区行的 `base_gen` 指向一个 (repo, tree, deps) 组合，`set_workspace_base` 用一条 UPDATE 同时换掉 tree 与 deps，不存在只换一半的状态。 | 不变量 8 |
| S7 | 基线与依赖集的行、目录永不原地修改；目录只有 gc 能拿走，而且先改名进 trash 再删。 | 不变量 5 |
| S8 | `refs_audit` 里 `update_ref` 的行只能是 `refs/strata/` 下的 ref（CHECK 约束）。store 自己不写任何 git 文件；它的全部路径都在 `<root>` 下，而 config.md 的 V3 保证 `<root>` 不在任何主 checkout 里。 | 不变量 2 |
| S9 | 不确定的结果保留现场：结果不确定的提交让写者中毒，等重开（§6.4）；阶段为 Running 或 Unknown 的发布钉住它批次里的 checkpoint；gc 从不删 1 小时以内的 blob、临时文件与未登记目录；损坏的 blob 只报不删。 | 不变量 6 |
| S10 | 每个 checkpoint 记下它对着的 `BaseGen`（`checkpoints.base_gen`，即 `Checkpoint.base`），读回时原样给出。restore 到基线不同的工作区由 overlay 走 `overlay::rebase`，从不按条目原样回放（增补裁定 T7/T6/T15）。 | 不变量 1 |

### 4.6 限制

- **扩展属性**：strata 不记录也不恢复 xattr（与 git 相同）。put 把克隆体上的扩展属性全部剥掉，blob 只含内容（增补裁定 T6/T7，来自 3c）。例外是内核托管的 `com.apple.provenance`：2026-10-22 本机实测，删除它的调用返回成功而属性仍在；它不是内容，store 不把它当失败。透明压缩用来存数据的 `com.apple.decmpfs`、`com.apple.ResourceFork` 不碰，剥掉会毁掉内容。
- **空目录**：overlay 只有文件，checkpoint 与 restore 从不携带空目录（与 git 相同，增补裁定 T6/T7）。store 的 blob 都是文件，没有目录 blob。
- **mode 不在 blob 里**：blob 文件一律 0444；条目的 mode 记在 `overlay_entries.mode`，取值同 git。
- **断电**：提交时请求 `F_FULLFSYNC`，它要求设备把缓存写到介质（fcntl(2)）；设备或 USB 桥是否照做，store 管不了。§7 证明的是同步调用确实发出了，不声称断电级持久（3a:670）。
- **基线文件**不在 blob 存储里（§4.1）。基线目录里文件的完整性，由读基线目录的模块核对（overlay.md、session.md）。

## 5. 流程与算法

store 里的锁（daemon 按 repo 的写锁在 store 之外）：

- **L1 写者守卫**：`<root>/store/LOCK` 上的 `flock(LOCK_EX | LOCK_NB)`。`open` 取得，`Store` 析构时放开；进程被杀时由内核释放。
- **L2 写锁**：`db` 的 `Mutex`，包住每个写事务。事务里只做 SQL，几毫秒；哈希、克隆、同步都在锁外。
- **L3 blob 闸门**：`RwLock<()>`。put、get、read、verify、`record_checkpoint` 持共享锁；gc 删 blob 文件的那一段持独占锁。
- SQLite 自己的锁：写事务一律 `BEGIN IMMEDIATE`。
- 读不取 L2：每个读函数从连接池借一个只读连接，在一个读事务里读完。

store 不覆盖工作区里已有的文件，所以本模块没有"先做 checkpoint"这一步，不变量 1 由调用方落实。原子替换只用在 store 自己的区域（tmp 到 blobs、临时库到 store.db、generation 目录到 trash），一律用 `renamex_np(RENAME_EXCL)`：目标已存在时失败，从不覆盖。

### 5.1 `open`：第一次建库

1. `root` 必须是绝对路径，否则 `InvalidInput`。
2. 同卷检查，不写任何东西：取 `root`，不存在时取它最近的已存在上级（config.md §4.3），对每个 checkout 调 `apfs::same_volume`。任一为假 → `CrossVolume`；checkout 不存在 → `NoSuchPath`。
3. 看 `<root>`：不存在、是空目录、或只含 `store`、`bases`、`deps`、`sessions` 这几个名字，就继续；有任何别的条目 → `NotEmpty`（`detail.entries` 列出前五个）。
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
2. 不经 SQLite，直接读 `store.db` 的文件头：魔数 `SQLite format 3\0`；偏移 18 与 19 的值为 2（WAL）；偏移 60 的 `user_version` 为 1；偏移 68 的 `application_id` 为 1398035009。任一不对 → `Corrupt`（`reason` 为 `application_id`、`user_version` 或 `journal_mode`）。这一步保证对不是 Store 的文件不建连接、不产生 -wal 与 -shm（3a:661）。建库时做过 TRUNCATE 检查点，之后这两个 PRAGMA 再也不改，所以主文件的文件头一直是准的。
3. 建写连接（`SQLITE_OPEN_READ_WRITE | SQLITE_OPEN_NO_MUTEX`，不带 CREATE），开 DEFENSIVE，`busy_timeout = 0`；用 PRAGMA 再核一次 `application_id`、`user_version`、`journal_mode`（不对同样是 `Corrupt`）。
4. 设 §4.2 的连接级设置并逐项回读；不符 → `Unsupported`，`detail` 带设置名、期望值与实际值。
5. schema 比对：把 §4.3 的 DDL 在 `:memory:` 库里执行一遍，两边各取 `SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY type, name`，必须逐行相同；否则 `Corrupt`（`schema`，`detail` 带第一处差别）。不修补、不迁移（3a:597、3a:610）。
6. `PRAGMA quick_check` 必须返回 `ok`，否则 `Corrupt`（`sqlite`）[我的决定]。store.db 只放元数据，这一步的成本与库的大小成正比，只在 daemon 启动时付一次。
7. 注册 SQL 函数 `strata_gc_active()`。读连接池先空着，用到时再开。

第 3 到 6 步里 SQLite 报 `SQLITE_CORRUPT` 或 `SQLITE_NOTADB` → `Corrupt`（`sqlite`）。对崩溃后的库，第 3 步会先做 WAL 恢复；这是 SQLite 自己的行为，不改变已提交的内容。

只读（`open_read_only`）：

1. 不取 L1。`store.db` 不存在 → `NoSuchPath`。
2. 同写者第 2 步。
3. 建只读连接（`SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NO_MUTEX`），开 DEFENSIVE，`query_only = 1`，`busy_timeout = 2000`；核身份、比 schema（同写者第 3、5 步）。只读连接做不了 WAL 恢复时 SQLite 报 `SQLITE_READONLY_RECOVERY` → `ReadOnly`（`recovery_needed`：先启动 daemon）[我的决定]。
4. 不改任何文件。此后每个写函数第一步就返回 `ReadOnly`，不碰 SQLite 和文件系统（3a:659）。

### 5.3 `put_blob` 与 `put_bytes`

`put_blob(src)` 持 L3 共享锁，不进 SQLite：

1. 只读 → `ReadOnly`。
2. 取临时名 `store/tmp/put-<arc4random 生成的 128 bit，十六进制>`，`apfs::clone_file(src, tmp)`（不跟随 src 的符号链接）。临时名已存在（`Exists`）就换一个名字重试，最多 8 次，仍不行 → `Unknown`。新写入从不采用或截断已存在的临时文件（3a:228）。`clone_file` 的其它错误原样透传：`NoSuchPath`、`IsDirectory`、`NotFile`、`CrossVolume`、`Unsupported`。
3. 此后只看克隆体，不再碰 src：克隆是 src 那一刻的快照，哈希期间 src 被改写的竞态在这里不存在（overlay 的写竞态规则管的是它自己的判断，见 overlay.md）。`lstat(tmp)`：是符号链接 → `readlink(tmp)` 得到目标的字节，删掉自己的 tmp，转第 9 步按 `put_bytes` 存目标文本（符号链接的 blob 内容就是目标，mode 120000 记在条目里，裁定 T7）；是普通文件 → 继续。
4. 规整。克隆会把 src 的 mode、扩展属性、BSD 标志一起带过来，2026-10-22 实测连 `uchg` 也带。依次：`chflags(tmp, st_flags & UF_COMPRESSED)` 清掉其它标志；`chmod 0600`（只读文件上删扩展属性会 `EACCES`，同日实测）；`flistxattr(XATTR_NOFOLLOW)` 列出的每个名字都 `fremovexattr`，跳过 `com.apple.provenance`、`com.apple.decmpfs`、`com.apple.ResourceFork`（§4.6）。
5. 哈希：读 tmp 的全部字节算 BLAKE3，16 MiB 及以上走 mmap 加多线程。代价与文件大小成正比；稀疏文件和大文件一样整份读，空洞读成零，不特殊处理（裁定 T7）。结果是 `id`，读到的字节数是 `size`。
6. `chmod 0444`；`futimens(UTIME_NOW)`。blob 文件的 mtime 就是"最近一次 put"的时刻，gc 用它判断年龄（§5.9）。
7. `fcntl(tmp, F_BARRIERFSYNC)`：tmp 的数据与属性排在之后的任何 I/O 之前落盘（fcntl(2)：屏障之前用 fsync 刷出去的 I/O，保证先于屏障之后的 I/O 持久化）。
8. `renamex_np(tmp, blobs/<id 前两位>/<id>, RENAME_EXCL)`：
   - 成功 → 对扇出目录做 `fcntl(F_BARRIERFSYNC)`，让新名字也排在之后的写入之前；返回 `id`。
   - `EEXIST`，即同一内容已存在 → `lstat` 已存在的文件。不是普通文件、或大小不等于 `size` → `Corrupt`（`blob_size`），`detail.path` 指向 tmp：tmp 是正确的一份，留给人修复，gc 按年龄收走（3a:506）。大小相等 → `futimens(已存在的文件, UTIME_NOW)` 刷新年龄，删掉自己的 tmp，返回 `id`。不回读、不重算已存副本（3a:505、3a:507，裁定 T6）。
   - 其它错误 → `Unknown`（`outcome = "not_written"`：`blobs/` 里没有多出东西，tmp 留在原处）。
9. `put_bytes(bytes)`：在 `store/tmp/` 用 `O_CREAT | O_EXCL | O_WRONLY | O_NOFOLLOW`、权限 0600 建临时文件（重名处理同第 2 步），写入全部字节，从内存里的字节算 BLAKE3，然后同第 6 到 8 步。

返回之后，blob 已在 `blobs/` 里，名字与数据都排在之后的任何写入之前；下一次 SQLite 提交的 `F_FULLFSYNC` 排空设备队列，于是"checkpoint 行持久"蕴含"它引用的 blob 持久"（S3）。并发：两个线程 put 同一内容，各自克隆、哈希，`RENAME_EXCL` 只让一个改名成功，另一个走 `EEXIST` 分支；两边返回同一个 id，只落一份（3a:582）。

### 5.4 `get_blob`、`read_blob`、`verify_blob`

三者都持 L3 共享锁。

`get_blob(id, dst)`：

1. 读 `blobs` 行：不存在 → `Corrupt`（`blob_unregistered`）。
2. `lstat` blob 文件：不存在 → `Corrupt`（`blob_missing`）；大小与行不符 → `Corrupt`（`blob_size`）。不重算哈希：get 是受信任的零拷贝克隆（裁定 T6）。
3. `apfs::clone_file(blob, dst)`：`dst` 已存在 → `Exists`，dst 不动；上级目录不存在 → `NoSuchPath`；跨卷 → `CrossVolume`。clonefile 要么建出完整的 dst，要么不建（3a:148）。
4. 把 dst 弄成 git checkout 出来的样子 [我的决定]：`chmod 0644`，atime 与 mtime 设为当前时刻。blob 文件是 0444，mtime 是当初 put 的时刻；把旧 mtime 带进工作区，增量构建（cargo、tsc、Turbopack）会把内容变了的文件当成没变。可执行位由调用方按条目的 mode 设。这一步失败 → `Unknown`（`outcome = "partial"`，`detail.path` 是 dst：dst 已经建出，留在原处）。
5. 不写 SQLite，不碰工作区里别的文件。restore 要替换已有文件时，overlay 先 get 到同目录下的临时名，再自己 `rename` 覆盖（overlay.md）。

`read_blob(id, max)`：第 1、2 步同上；`size > max` → `TooLarge`（`limit = "read_max"`）；否则读出全部字节返回，不重算哈希。overlay 用它取符号链接的目标（不超过 1024 字节），publish 用它取三方合并的输入。

`verify_blob(id)`：第 1、2 步同上，再重算 BLAKE3；与 `id` 不符 → `Corrupt`（`blob_mismatch`，`detail` 带期望与实际）。`restore --verify` 和 gc 的完整性扫描用它；它从不修改或删除任何东西。

3a 有几行（3a:198、3a:555、3a:507）原文写的是"读出时发现内容与 id 不符就失败"。在 strata 里这由校验读承担，普通的 get 不承担：这是裁定 T6 的校验策略，§7 对应的测试按这个口径断言。

### 5.5 generation、清单与会话的写

每个函数一个 `BEGIN IMMEDIATE` 写事务，持 L2，提交即返回：

- `record_deps_set`：同一 (repo, id) 已登记且各列相同 → 不写，返回；各列不同 → `InvalidInput`；否则插入。没有依赖目录的仓库（config.md 里 `deps_dirs = []`）也要有一行，`dirs` 为空数组，因为 `BaseGen.deps` 不是可选的；它的 id 怎么算由 deps.md 定。
- `record_manifest`：同一 (repo, tree) 已登记 → 逐条比较，相同就不写、不同就 `InvalidInput`；否则一个事务插 `base_trees` 一行（`files` = 条目数）和全部 `manifest_entries`。有 `base_trees` 这一行，空 tree（0 条）和没登记才能区分。
- `record_base_gen`：清单与依赖集必须已登记，否则 `InvalidInput`；已登记同一 (repo, tree, deps) → 不写；否则插入。
- `create_session`：插 `sessions` 一行（`lifecycle = 'creating'`），每个 `WorkspaceState` 插 `workspaces` 一行。每个 `base` 必须已登记（`InvalidInput`）；`repos` 不能为空；每个工作区的 `path` 必须等于 `root/<repo>`（`InvalidInput`）；会话 id、`root` 或任何工作区路径已存在 → `Exists`。这一步排在 session::new 的第几步由 session.md 定，store 只保证它是一个事务。
- `set_lifecycle`：`UPDATE … WHERE id = ? AND lifecycle = from`。改了一行返回 true；没改到时区分"会话不存在"（`NoSuchSession`）与"当前不是 from"（false）；不允许的转换被触发器拦下 → `InvalidInput`。进入 `Removed` 时写 `removed_ns = at`。
- `set_workspace_base`：会话必须存在且不是 `removed`（否则 `NoSuchSession`），工作区必须存在（否则 `NoSuchRepo`），新基线必须已登记（否则 `InvalidInput`）；一条 UPDATE 同时改 `base_gen` 与 `head`（S6）。目录交换由 session 的 `move_workspace` 先做完，store 只记结果；两者之间崩溃怎么办归 session.md。
- `record_seeds`：把 `workspaces.seeds` 从 NULL 写成 JSON；已经写过 → `InvalidInput`（触发器也拦）。
- `retire_dir`：只接受 §3 列出的三种路径。按组件解析成 `RepoName`、`TreeOid`、`DepsSetId`、`SessionId`，深度必须恰好对上，任何一级都不能是符号链接。bases 与 deps 的路径对应的行必须不存在；sessions 的会话必须是 `removing` 或 `removed`。然后 `renamex_np(path, store/trash/<unix 毫秒>-<随机>, RENAME_EXCL)`。只改名，不删除；trash 由 gc 按保留期删（§5.9）。session 可以用它让 `rm` 在保留期内可以找回 [我的决定；用不用由 session.md 定]。

### 5.6 `record_checkpoint`

1. 纯校验，不碰任何东西（失败 → `InvalidInput`）：条目的 `path` 两两不同；没有哪条的 `path` 等于另一条 `Renamed` 的 `from`；`Renamed` 的 `from` 不等于自己的 `path`；`Deleted` 的 mode 为 0 且没有 blob，其余种类的 mode 是 0o100644、0o100755 或 0o120000 并带 blob；`at` 在 §4.4 的范围内。
2. 只读 → `ReadOnly`；写者已中毒 → `Unknown`（`reason = "poisoned"`，`outcome = "not_written"`）。
3. 持 L3 共享锁与 L2，`BEGIN IMMEDIATE`。外部进程持着 SQLite 写锁 → `Busy`（`lock = "sqlite"`），不重试，什么都没写（3a:620、3a:659）。
4. 事务内核对：会话存在且不是 `removed`（否则 `NoSuchSession`）；工作区 (session, repo) 存在（否则 `NoSuchRepo`）且 `tracked = 1`（否则 `InvalidInput`：`tracked = false` 的仓库没有 overlay，T9 增补）；`cp.base` 已登记，取得它的 `base_gens.id`（否则 `InvalidInput`）。
5. 钩子重放：`reason` 是 `ToolCall { id }`，而同一会话、同一 repo 已有这个工具调用 id 的 checkpoint → `ROLLBACK`，返回已有的 `CheckpointId`。唯一索引 `checkpoints_by_tool_call` 兜底。
6. 对每个被引用的、不同的 blob：`lstat` 它的文件，不存在 → `Corrupt`（`blob_missing`）；用 `INSERT OR IGNORE INTO blobs` 登记，已有的行按先到者为准（3a:657）；已有行的大小与文件不符 → `Corrupt`（`blob_size`）。引用由 store 按条目自己推导、核对，不信任调用方自报（3a:617）。
7. 分配序号：该会话的 `COALESCE(MAX(id), 0) + 1`；已到 `i64::MAX` → `ROLLBACK`，`TooLarge`（3a:590、3a:596）。
8. 插 `checkpoints` 一行（`entries` = 条目数），每个条目插 `overlay_entries` 一行（`Renamed` 的 `from` 写进 `from_path`）。
9. `COMMIT`。`synchronous = FULL` 加 `fullfsync = 1`：提交时在 WAL 上做 `F_FULLFSYNC`，返回即持久。提交之前的任何失败，SQLite 回滚整个事务，返回 `Unknown`（`outcome = "not_written"`）或上面各步的码；提交本身的 I/O 错误 → `Unknown`（`outcome = "uncertain"`），写者中毒（§6.4）。
10. 放开 L2、L3，返回序号。

整个单元在一个事务里：checkpoint 行、全部条目与 blob 登记要么一起出现，要么都不出现（3a:657、3a:658）。第 3 到 10 步只有 SQL，L2 只持几毫秒；put 的哈希与同步在这之前、在锁外完成。

### 5.7 提议、发布与 ref 审计

每个函数一个写事务，持 L2：

- `record_proposal`：from、to 两个依赖集必须已登记（否则 `InvalidInput`）；`sqlite_sequence` 里 proposals 的值已到 `i64::MAX` → `TooLarge`；插入一行，`status = 'pending'`，返回新号。
- `decide_proposal`：`UPDATE … WHERE id = ? AND status = 'pending'`；`to` 只能是 `Approved` 或 `Superseded`（否则 `InvalidInput`）；改了一行返回 true，否则 false。
- `record_publish`：`batches` 不能为空，每批的 `paths` 不能为空（否则 `InvalidInput`）；每个 `Batch` 的 (session, checkpoint) 必须存在（否则 `NoSuchCheckpoint`）；取号（最大值加 1，到顶同样 `TooLarge`）；插 `publishes` 一行（`phase = 'running'`），每个（批次下标，路径下标）插 `publish_items` 一行。路径原样照抄，不展开改名：批次怎么解释归 publish.md。返回号。
- `set_publish_phase`：比较并交换，只允许 Running→Done、Running→Unknown、Unknown→Done，其余被触发器拦下 → `InvalidInput`。离开 Running 时写 `ended_ns`，`outcome` 可选。daemon 启动时把残留的 Running 改成 Unknown 并报出，从不重放（daemon.md）。
- `record_ref_change`：追加一行 `refs_audit`。`UpdateRef` 只能配 `Applied` 或 `HeadMoved`，ref 名必须以 `refs/strata/` 开头；`Push` 只能配 `Pushed`、`Refused` 或 `Unknown`；不配 → `InvalidInput`（CHECK 约束也拦）。`confirmed` 存 `Action` 的 JSON，从不存 `ConfirmToken`。

### 5.8 读

每个读函数从池里借一个只读连接，在一个读事务里读完，再归还连接；例如 `checkpoint` 读到的行与全部条目来自同一个快照。读从不取 L2，所以写事务进行时读照常完成（3a:620、3d:165）；WAL 之下，新开的读事务看得见全部已提交的写。读回的标识经 `TryFrom` 重新校验，序号先查是否不小于 1；任何一项不对 → `Corrupt`（`bad_row`）。

### 5.9 `gc(keep)`

只有写者能跑（只读 → `ReadOnly`）。`now` 只取一次。年龄下限 `floor = max(keep, 1 小时)` 用于 blob、临时文件与未登记的目录 [我的决定]：put 与 `record_checkpoint` 之间有一小段窗口，blob 已在 `blobs/` 里而还没有行引用它；1 小时的下限让 gc 即使 `keep = 0` 也碰不到这样的 blob。

1. **保留集**，在一个读快照里算：
   - 可回收的会话 R：`lifecycle = 'removed'` 且 `removed_ns < now − keep`，并且没有阶段为 Running 或 Unknown 的发布在 `publish_items` 里引用它的任何 checkpoint（S9）。
   - 被钉住的基线 generation：不属于 R 的会话的工作区所钉的；不属于 R 的会话的 checkpoint 所对着的（S10）；每个 repo 里 `created_ns` 最新的那一个 [我的决定：下一个 `new` 多半要用它，回收掉就得重新导出、重新认领依赖]。
   - 已决定、且 `decided_ns < now − keep` 的提议可回收；待确认的提议钉住它的 from 与 to 依赖集。
2. **删行**：分批，每批一个写事务，不超过 10 000 行 [我的决定]；批与批之间放开 L2，checkpoint 照常进行。每批开始时打开 `strata_gc_active`，提交后关掉。顺序：
   1. R 的会话行，级联删掉它的工作区、checkpoint 与 overlay 条目。
   2. 可回收的提议。
   3. 没被钉住、且 `created_ns < now − keep` 的基线 generation 行；记下它们的目录。
   4. 已没有基线 generation 引用、且 `created_ns < now − keep` 的基线 tree 行，级联删掉清单。
   5. 没有基线 generation 与提议引用、且 `created_ns < now − keep` 的依赖集行；记下它们的目录。
   6. blob：持 L3 独占锁，删掉没有 overlay 条目引用、且文件 mtime 早于 `now − floor` 的 blob 行，提交后删掉它们的文件，再放开 L3。外键保证被引用的 blob 行删不掉（3a:600）。
3. **挪目录**：第 2 步记下的每个 generation 目录用 `renamex_np` 改名进 `store/trash/<now 毫秒>-<随机>`；目录已经不在就跳过。先删行、后挪目录：两者之间崩溃留下的是"活路径上没有登记的目录"，第 4 步会收走它。
4. **清扫**，只在 store 拥有的区域：
   - `blobs/<aa>/` 里没有行、且 mtime 早于 `now − floor` 的文件 → 删除。它们是 put 之后、`record_checkpoint` 之前崩溃留下的孤儿（3a:293）。
   - `store/tmp/` 里 mtime 早于 `now − floor` 的条目 → 删除。
   - 形状是 `bases/<repo>/<tree>/<deps>` 或 `deps/<repo>/<deps>`、每一级都能解析成对应的键、却没有登记、且 mtime 早于 `now − floor` 的目录 → 改名进 trash；之后变空的 `bases/<repo>/<tree>/` 用 `rmdir` 删掉。
   - gc 不进入 `<root>/sessions/`，不碰名字解析不成键的条目（包括别的模块以 `.` 开头的临时目录），不跟随任何符号链接：这些都可能是别的模块保留的现场（裁定 T5：中断的导出保留它的临时目录，由 status 报出）。
5. **删 trash**：名字里的时刻早于 `now − keep` 的条目逐个删除。经 `apfs::open_rooted` 逐级 `O_NOFOLLOW` 打开，先清掉 `UF_IMMUTABLE` 与 `UF_APPEND`（封存若用了 `chflags uchg`，克隆会把它带过来），再自底向上 `unlinkat`；符号链接只删链接本身。删不掉的条目留在原处，记进报告的 `stuck`，不让整轮失败。
6. **完整性扫描**：从上一行 `gc_runs.scrub_after` 之后，按 `blobs.id` 的顺序逐个 `verify_blob`，累计读满 2 GiB 就停 [我的决定]，记下最后一个 id；走到末尾则游标回到 NULL，下一轮从头开始。不符或文件不在的 blob 进报告的 `corrupt`：从不删除、从不覆盖（3f:557），由 status 报出、人来处理。这一段持 L3 共享锁，checkpoint 照常进行。
7. 追加一行 `gc_runs`（报告的 JSON 与新游标），返回报告。

gc 中途 I/O 失败 → `Unknown`（`outcome = "partial"`）：已提交的批次有效，已经挪进 trash 的目录留在 trash 里；下一轮从头算保留集、接着做。不回滚、不重放已删的东西。

### 5.10 复杂度与验收数字

| 操作 | 复杂度 | 说明 |
| --- | --- | --- |
| `put_blob` | O(文件大小) 的哈希加常数次系统调用 | 两次 `F_BARRIERFSYNC`（文件与扇出目录）；重复内容只多一次克隆和一次哈希 |
| `get_blob` | O(1) | 零拷贝克隆，不读内容 |
| `record_checkpoint` | O(条目数) 次插入加 1 次 `F_FULLFSYNC` | L2 只持几毫秒 |
| `checkpoint` 读 | O(条目数) | 一个读事务 |
| `gc` | O(行数 + blob 文件数 + 扫描预算) | 分批，不长时间挡住写 |
| `open`（已有的库） | O(schema) 加 `quick_check` 的 O(库大小) | 只在 daemon 启动时 |

design §8 的验收数字里与 store 有关的是"checkpoint 改 50 个文件 < 200 ms"。store 的份额是 50 次 put（每次一次克隆、一次哈希、两个屏障）加一次 `record_checkpoint`（一次 `F_FULLFSYNC`）。屏障与 `F_FULLFSYNC` 是主要成本，在 USB SSD 上更慢，首个 spike 要实测这个数字。超出预算时回到用户那里决定，store 不悄悄去掉屏障：它们是断电时"行持久则 blob 持久"的唯一保证。put 是线程安全的，overlay 可以并行 put 多个文件。"新建会话 < 5 s"里 store 的份额只是几次登记和一个 `create_session` 事务，毫秒级。

## 6. 失败与恢复

### 6.1 store 返回的码

"types.md 已列"一栏说的是 types.md §6.2"谁返回"里有没有 store。写"否"的几项，是本文把 store 的行为映射到最接近的已有码；Task 15 在 types.md 的"谁返回"里补上 store，码的集合不变（仍是 39 个）。

| 码 | 何时（函数） | 留下的现场 | 恢复 | types.md 已列 |
| --- | --- | --- | --- | --- |
| `CrossVolume` | open：root 与某个 checkout 不同卷；put、get 由 apfs 透传 | 无 | 改 `[paths] root` | 是 |
| `NotEmpty` | open 建库时 root 里有不属于 strata 的条目 | 无 | 换一个 root，或清空它 | 是 |
| `Corrupt` | open：文件头、身份、版本、日志模式、schema、quick_check 不对，或丢了 store.db；读：标识或序号不合法；blob：没登记、文件不在、大小不符、重算的哈希不符 | 一字不动；put 的大小不符会留下正确的 tmp | 人工检查（status 报出）；store 从不自动修补 | 是 |
| `Busy` | open：另一个写者持有 LOCK；任何写：外部进程持有 SQLite 写锁 | 无，`outcome = "not_written"` | 稍后重试 | 否（types.md 只列了 daemon 的按 repo 写锁） |
| `ReadOnly` | 只读 Store 上的任何写；只读打开时需要 WAL 恢复 | 无 | 启动 daemon | 否（列的是 cli 与写封存目录的模块） |
| `Unsupported` | open：卷不支持 clonefile（apfs 透传）；PRAGMA 回读不符 | 无；建库探测失败时删掉自己刚建的空目录 | 换 APFS 卷；换支持这些设置的 SQLite | 否（列的是 apfs、gitio、confirm；回读不符这一项是新增） |
| `NoSuchPath` | open：checkout 不存在；open_read_only：还没有 Store；put：src 不存在；get：dst 的上级不存在；retire_dir：路径不存在 | 无 | 修正路径 | 否 |
| `Exists` | get：dst 已存在；create_session：会话 id、root 或工作区路径已存在 | 无 | 换目标 | 否（列的是 apfs、session） |
| `IsDirectory` | put：src 是目录（apfs 透传） | 无 | — | 是 |
| `NotFile` | put：src 是 FIFO、socket 或设备文件（apfs 透传） | 无 | — | 透传 |
| `NoSuchCheckpoint` | `checkpoint` 读；record_publish 的批次点名了不存在的 checkpoint | 无 | — | 是 |
| `NoSuchSession` | 写操作点名的会话不存在或已 Removed；set_lifecycle 的会话不存在 | 无 | — | 否（列的是 session） |
| `NoSuchRepo` | 会话里没有这个 repo 的工作区 | 无 | — | 否 |
| `InvalidInput` | root 不是绝对路径；条目不一致；工作区 tracked = false；基线、清单、依赖集没登记；与已登记的内容不同；不允许的状态转换；retire_dir 的路径不合规或对应的行还在；种子记录写第二次；时间超出范围 | 无 | 调用方的错 | 否（列的是 types、cli、daemon） |
| `TooLarge` | 序号到了 `i64::MAX`；read_blob 的大小超过 max | 无 | — | 否（列的是 publish、search、daemon） |
| `Unknown` | types.md §6.3 归为 `Unknown` 的 I/O 错误（`ENOSPC`、`EIO` 等）；提交结果不确定；写者中毒；触发器拦下了 store 自己发出的语句（store 的 bug） | 看 `outcome` | §6.4 | 是（任何模块） |

### 6.2 `detail` 的形状

types.md 把 `detail` 的形状交给返回它的模块。store 的 `detail` 都是 JSON 对象，不适用的键省略：

- I/O 类（按 types.md §6.3 归类的错误）：`{"errno": 28, "path": "...", "op": "commit", "outcome": "not_written"}`。
- `outcome` 是本文规定的"写到哪一步"标记（3c:30 要求由 store.md 规定）：
  - `"not_written"`：没有任何外部可见的改变。事务回滚了或根本没开始，也没有文件改名进 store。可以直接重试，没有现场要保留。
  - `"uncertain"`：一次 `COMMIT` 的结果不确定（提交时同步或写失败）。那一行在恢复之后可能在、也可能不在。写者中毒（§6.4），status 报出，不重放。
  - `"partial"`：一个多步操作停在中间，已完成的步骤有效（gc 已提交的批次、get 已建出的 dst）。现场保留，status 报出。
  - 同样是 `ENOSPC`：写临时文件时满了是 `not_written`，提交时满了是 `uncertain`。两者都是 `Unknown`，调用方按 `outcome` 区分，永远不把不确定的结果当成确定的容量失败去重放或清理（3c:30）。
- `Corrupt`：`{"reason": "...", "blob": "...", "expected": "...", "actual": "...", "path": "..."}`。`reason` 取 `application_id`、`user_version`、`journal_mode`、`schema`、`sqlite`、`store_db_missing`、`bad_row`、`blob_unregistered`、`blob_missing`、`blob_size`、`blob_mismatch` 之一。
- `Busy`：`{"lock": "...", "path": "..."}`，`lock` 是 `store-writer`（LOCK）或 `sqlite`（SQLite 写锁）。
- `CrossVolume`：`{"root": "...", "checkout": "..."}`。`NotEmpty`：`{"root": "...", "entries": ["..."]}`。
- `TooLarge`：`{"limit": "...", "max": 9223372036854775807}`，`limit` 取 `checkpoint_id`、`proposal_id`、`release_group_id` 或 `read_max`。
- `Unsupported`：`{"setting": "fullfsync", "expected": 1, "actual": 0}`；apfs 透传的保持 apfs 的形状。
- `ReadOnly`：`{"reason": "read_only_handle"}` 或 `{"reason": "recovery_needed"}`。
- `InvalidInput`：`{"reason": "...", "path": "...", "field": "..."}`，按需取用。

### 6.3 崩溃会留下什么

| 崩溃发生在 | 留下 | 下一次 |
| --- | --- | --- |
| 建库第 9 步之前 | 空目录，`store/tmp/` 里的临时库 | open 从头建（§5.1）；临时库由 gc 收走 |
| put：克隆之后、改名之前 | `store/tmp/` 里的克隆体 | 不影响任何写入，新写入换新名字（3a:228）；gc 过了年龄下限收走 |
| put：改名之后、record 提交之前 | `blobs/` 里一个没有行的 blob | 之后同内容的 put 复用它；没人复用就由 gc 过了下限收走（3a:293）；不存在 checkpoint 行 |
| `record_checkpoint`：提交之前 | 什么都没有，SQLite 回滚 | checkpoint 不存在；同一工具调用的重放会重新记 |
| `record_checkpoint`：提交之后、回复之前 | 完整、持久的 checkpoint | 同一工具调用的重放返回同一个号；`checkpoint_write_is_durable_after_crash` 证明 |
| gc：删行之后、挪目录之前 | 活路径上没有登记的目录 | 下一轮 gc 第 4 步收走 |
| gc：删 trash 中途 | trash 里删了一半的目录 | 下一轮接着删 |
| 进程持着 LOCK 与 SQLite 写锁时被杀 | 没有陈旧的锁：flock 与 SQLite 的 POSIX 锁都随进程消失由内核释放 | 下一个写者立刻能打开、能写（3a:692）；WAL 恢复由 SQLite 做 |

### 6.4 `Unknown` 之后

按不变量 6 与"错误时机"裁定：store 只在写任何东西之前给出类型化的拒绝；写开始之后的失败一律 `Unknown`，现场保留，不清理，不回滚已经持久的东西，由 status 报出。

- `outcome = "uncertain"` 的提交之后，写者中毒：此后每个写函数返回 `Unknown`（`reason = "poisoned"`），直到 daemon 丢掉这个 `Store` 重新 `open` [我的决定]。重开时 SQLite 的 WAL 恢复给出真实结果：checkpoint 行在，就是提交了；不在，就是没提交，钩子的重放会重新记。中毒期间读照常工作。理由：同步失败之后继续写，可能把其实没落盘的状态当成已提交（PostgreSQL 在 fsync 失败后也不再继续）。
- gc 的 `partial` 不需要人处理，下一轮 gc 会接着做。
- `Corrupt` 从不自动处理：status 报出它（gc 报告里的 `corrupt` 也由 status 报出），由人决定。

## 7. 测试清单

带 `#[ignore]` 的测试需要 `hdiutil` 建的磁盘映像（不需要 root），用 `cargo test -p strata-store -- --ignored` 跑。子进程测试照 LayerFS 的做法：父测试以 `--exact <子测试名>` 重新执行本测试二进制并设环境变量，子测试函数在环境变量不在时直接返回。

| 测试名 | 断言 | 来源（矩阵行 或 新增） |
| --- | --- | --- |
| `open_creates_layout_on_empty_root` | 空的或不存在的 root 上，`open` 建出 §4.1 的布局：256 个扇出目录，`store.db` 的文件头 `application_id = 1398035009`、`user_version = 1`、WAL；第二次 `open` 不改任何文件 | 新增 |
| `interrupted_creation_is_resumed` | 用 `InitBeforeRename` 让建库在改名之前中止：root 下只有空目录和 `store/tmp/` 里的临时库；再次 `open` 成功建库，不报 `NotEmpty` | 新增 |
| `open_refuses_nonempty_foreign_root` | root 里有一个别的文件：`open` 返回 `NotEmpty`，`detail.entries` 列出它，root 下没有多出任何东西 | 新增 |
| `open_refuses_lost_store_db` | 建好的 Store 删掉 `store.db`，`sessions/` 里还有会话目录：`open` 返回 `Corrupt`（`store_db_missing`），不建新库 | 新增 |
| `open_refuses_cross_volume_root` | root 在 `hdiutil` 建的另一个 APFS 卷上：`open` 在写任何东西之前返回 `CrossVolume`，两个卷上都没有新文件（`#[ignore]`） | layerfs-project/tests/init_sqlite.rs::an_unsupported_platform_refuses_the_store_before_any_file_exists |
| `open_refuses_volume_without_clonefile` | root 与 checkout 同在一个 FAT 磁盘映像上：`open` 返回 `Unsupported`，本次建的空目录被删掉，没有净写入（`#[ignore]`） | layerfs-project/tests/init_sqlite.rs::an_unsupported_platform_refuses_the_store_before_any_file_exists |
| `open_pins_durable_pragmas` | 写连接回读 `journal_mode = wal`、`synchronous = 2`、`fullfsync = 1`、`checkpoint_fullfsync = 1`、`foreign_keys = 1`、`recursive_triggers = 1`、`busy_timeout = 0`；读连接 `query_only = 1`。strata 只有这一种持久配置，没有 Disposable 档。这是 `checkpoint_write_is_durable_after_crash` 的前置 | layerfs-persistence/tests/sqlite_publication.rs::wal_full_settings_and_unavailable_engine_are_explicit<br>layerfs-persistence/tests/serverless_store.rs::disposable_create_publish_seal_and_reopen_all_layouts<br>layerfs-daemon/tests/r5_10_producer_profile.rs::r5_10_each_mounted_commit_has_one_producer_on_the_one_open_disposable_writer_session |
| `open_refuses_non_wal_store` | 用原始连接把库改成 `journal_mode = delete`：`open` 返回 `Corrupt`（`journal_mode`），文件字节不变，日志模式仍是 delete | layerfs-persistence/tests/serverless_store.rs::non_wal_store_is_refused_without_conversion |
| `open_refuses_schema_drift` | 分别删一张表、去掉一个 CHECK 约束、改一列的类型、删一个触发器：`open` 都返回 `Corrupt`（`schema`），文件字节不变，不修补 | layerfs-persistence/tests/history_remediation.rs::reopen_refuses_each_independent_schema_or_binding_corruption<br>layerfs-persistence/tests/old_format_open.rs::a_store_with_any_other_schema_difference_is_still_refused<br>layerfs-project/tests/init_sqlite.rs::a_store_without_acquisition_tables_refuses_init |
| `open_refuses_unknown_user_version` | `user_version` 改成 0 和 2：`open` 都返回 `Corrupt`（`user_version`），不迁移；版本策略见 §4.4 | layerfs-persistence/tests/sqlite_inode_values.rs::a_store_of_the_value_group_format_is_refused_at_open<br>layerfs-persistence/tests/old_format_open.rs::an_old_format_store_opens_and_records_hints_from_then_on |
| `open_leaves_unrelated_database_untouched` | `store.db` 的位置上放一个 delete 日志模式的无关 SQLite 库：`open` 返回 `Corrupt`（`application_id`），文件字节与日志模式不变，没有产生 -wal 或 -shm | layerfs-persistence/tests/sqlite_publication.rs::writable_open_does_not_migrate_an_unrelated_database_journal |
| `second_writer_open_is_busy` | 已有一个写者 `Store`（同一进程里，以及子进程里）时，第二个 `open` 返回 `Busy`（`store-writer`），什么都不写；`open_read_only` 照常成功：Store 只有 daemon 一个写者，cli 只读，没有直连写路径 | layerfs-store-service/tests/kernel_guard.rs::direct_access_is_unsupported_off_linux<br>layerfs-store-service/tests/kernel_guard.rs::direct_access_is_unsupported_on_a_filesystem_outside_the_allowlist |
| `read_only_open_refuses_every_write` | `open_read_only` 之后，每个写函数（put、record、create_session、gc 等）都返回 `ReadOnly`，store 目录的文件列表与字节不变；读函数照常 | layerfs-persistence/tests/sqlite_publication.rs::readonly_refusal_and_external_writer_busy_do_not_retry |
| `blob_put_is_idempotent` | 同一内容 put 两次（同一个文件；两个不同路径的文件；一次 `put_blob` 一次 `put_bytes`）得到同一个 BlobId，`blobs/` 里只有一个文件；把已存副本改成 000 权限之后再 put 照样成功，证明不回读已存副本 | layerfs-content/tests/file_complete.rs::construction_is_deterministic_for_the_same_input<br>layerfs-content/tests/streaming.rs::root_identity_is_stable_across_repeated_streaming_runs<br>layerfs-storage/tests/exact_reuse_trust.rs::an_exact_reuse_reads_no_stored_bytes |
| `blob_id_ignores_delivery_shape` | 同样的字节，一次写成的文件、分块乱序写成的文件、经 mmap 写成的文件，以及 `put_bytes`，得到同一个 BlobId：身份只取决于内容 | layerfs-content/tests/file_complete.rs::known_length_and_streaming_inputs_agree_exactly<br>layerfs-content/tests/streaming.rs::unknown_length_and_known_length_agree_object_by_object<br>layerfs-workspace/tests/captured_namespace_fragments.rs::fragmented_writes_of_a_base_file_give_one_root |
| `blob_known_answer_vectors` | 空串的 BlobId 为 `af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262`，`abc` 的为 `6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85`（2026-10-22 用 BLAKE3 参考实现核过）；一百万个 `a` 的值写测试时用 `b3sum` 算出后固定。三者经 `put_blob` 与 `put_bytes` 都得到这些 id。原测试的 SHA-256 向量不保留 | layerfs-storage/tests/object_key.rs::sealed_keys_keep_standard_sha256_bytes_for_empty_short_and_many_blocks |
| `empty_file_is_a_valid_blob` | 0 字节的文件可以 put，id 是空串的 BLAKE3；get 回来是 0 字节的文件 | layerfs-content/tests/edit_single.rs::complete_deletion_returns_the_empty_representation |
| `stored_blob_is_never_rewritten` | 之后存任何别的内容，已存 blob 的 inode 与字节都不变；同内容再 put 只刷新它的 mtime | layerfs-content/tests/edit_single.rs::the_base_objects_are_never_rewritten |
| `blob_identity_changes_with_any_byte` | 一份内容在首、中、尾各改一个字节，得到三个互不相同、也不同于原来的 BlobId | layerfs-content/tests/object_identity.rs::identity_of_a_damaged_canonical_object_changes |
| `blob_roundtrip_is_byte_exact` | 空、1 字节、1 MiB 的文件 put 之后再 get，字节逐一相同 | layerfs-content/tests/file_read.rs::full_read_returns_exact_bytes_for_every_representation |
| `large_and_sparse_blobs_roundtrip` | 500 000 000 字节的稠密文件，与带空洞的稀疏文件，经 put、get 字节相同（空洞读成零）；get 是零拷贝克隆，不读内容 | layerfs-daemon/tests/complete_installed_roots.rs::dense_500000000_kernel_bytes_survive_install<br>layerfs-daemon/tests/complete_installed_roots.rs::sparse_chunk_bytes_and_holes_survive_install |
| `same_bytes_new_path_or_mode_reuse_blob` | 同样字节的文件换路径、换 inode（复制一份）、改 mode 之后 put，得到同一个 blob；差一个字节就是新 blob；两个路径在 checkpoint 里各是一条，blob 只存一份 | layerfs-daemon/tests/file_digests.rs::a_relinked_file_with_saved_bytes_reuses_the_object<br>layerfs-project/tests/init_counters.rs::init_counts_every_entry_and_each_file_identity_once |
| `blob_files_are_content_only` | 源文件带 `com.apple.quarantine`、一个用户扩展属性、`uchg` 标志、mode 0755：blob 文件没有这些扩展属性（只剩内核托管的 `com.apple.provenance`），没有 BSD 标志，mode 0444；get 出来的文件 mode 0644、mtime 在当前时刻附近，没有拷来的扩展属性 | layerfs-workspace/tests/captured_namespace_metadata.rs::a_metadata_patch_keeps_every_other_key_and_its_value_root |
| `symlink_blob_stores_target_text` | put 一个指向 `../a/b` 的符号链接：blob 是内容为 `../a/b` 的普通文件，`read_blob` 读回目标文本；链接本身不被跟随 | 新增 |
| `put_blob_refuses_directory_and_special_files` | src 是目录 → `IsDirectory`；FIFO → `NotFile`，而且打开 FIFO 不会挂住；不存在 → `NoSuchPath`；三种情况下 `blobs/` 与 `tmp/` 都没有新条目 | 新增 |
| `get_blob_refuses_existing_dst` | dst 已存在：`get_blob` 返回 `Exists`，dst 的字节与 inode 不变 | 新增 |
| `get_blob_missing_is_corrupt_and_leaves_no_dst` | 删掉一个已登记 blob 的文件：`get_blob` 返回 `Corrupt`（`blob_missing`），dst 不存在 | layerfs-content/tests/file_read.rs::a_provider_failure_is_returned_once_and_stops_the_read |
| `missing_or_mismatched_blob_is_corrupt_never_new` | blob 没登记、文件不在、大小与登记不符、内容与 id 不符（`verify_blob`）四种情况都返回 `Corrupt` 并带原因；没有哪个函数返回会被当成"这是新文件"或"没变"的结果 | layerfs-workspace/tests/captured_file_edits.rs::first_provider_absence_denial_identity_and_length_mismatch_never_prove_newness |
| `get_blob_is_trusted_verify_detects_flipped_bit` | 翻转 blob 文件的一个比特（长度不变）：`get_blob` 照常克隆（受信任读，不重算）；`verify_blob` 返回 `Corrupt`（`blob_mismatch`，带期望与实际），坏字节不会以"已校验"的名义交给调用方 | layerfs-content/tests/filesystem_failure.rs::a_malformed_or_unauthenticated_demanded_page_is_rejected_without_a_root<br>layerfs-storage/tests/selected_group_reads.rs::accessed_record_corruption_fails_canonical_authentication |
| `put_reuse_does_not_reread_existing_copy` | 已存副本被改坏（长度不变）：同内容再 put 照常返回同一个 id，不回读；之后 `verify_blob` 与 gc 的完整性扫描都报出它 | layerfs-storage/tests/exact_reuse_trust.rs::a_corrupt_stored_copy_is_accepted_at_save_and_fails_at_its_next_read |
| `put_reuse_with_length_mismatch_is_corrupt` | 已存的同名 blob 被截短：同内容再 put 返回 `Corrupt`（`blob_size`），已存副本不被替换，`detail.path` 指向留在 tmp 里的正确副本 | layerfs-storage/tests/exact_reuse_trust.rs::a_role_or_length_mismatch_is_still_refused_at_save |
| `put_restores_missing_blob_file` | blob 有行而文件被删：同内容再 put 重新建出文件，不信任悬空的行；之后 `verify_blob` 通过 | layerfs-daemon/tests/file_digests.rs::a_recorded_root_the_store_does_not_hold_is_not_reused |
| `stale_temp_files_do_not_block_put` | `store/tmp/` 里放满旧的临时文件和无关文件，再用 `TempNameTaken` 让第一个临时名已被占用：put 换名成功，既不采用也不截断已有文件，那些文件原样留着 | layerfs-content/tests/filesystem_ordering.rs::a_stale_run_file_does_not_block_a_later_backing |
| `concurrent_puts_of_same_content_store_one_blob` | 8 个线程同时 put 同一份新内容（相当于几个会话同时 checkpoint 同一个文件）：都成功，都返回同一个 id，`blobs/` 里只有一个文件，读回完整；随后两个会话各自 `record_checkpoint` 都成功 | layerfs-persistence/tests/concurrent_first_installs.rs::two_writers_saving_the_same_new_content_both_complete |
| `put_blob_orders_blob_before_later_writes` | `sync_counts`：每个新 blob 在 put 返回之前有一次文件屏障和一次扇出目录屏障；重复内容的 put 不做屏障 | 新增 |
| `corrupt_blob_is_reported_never_overwritten` | 已存 blob 被改坏（长度不变）：gc 的完整性扫描把它列进 `corrupt` 并原样保留；同内容再 put 不覆盖它；人把正确的字节写回之后，`verify_blob` 通过，下一轮扫描不再报 | tgrep-core/tests/generations.rs::malformed_metadata_is_rejected_without_overwriting_generation |
| `generation_rows_register_and_read_back` | `record_deps_set`、`record_manifest`、`record_base_gen` 写入后读回相等；同样内容再登记一次不写；内容不同的再登记返回 `InvalidInput` | 新增 |
| `manifest_absent_and_empty_are_distinct` | 没登记的 (repo, tree) 读回 `None`；登记过的空 tree 读回 entries 为空的清单；同一个 tree 在两个 repo 下各有一份 | 新增 |
| `base_gen_requires_registered_tree_and_deps` | 清单或依赖集没登记时，`record_base_gen` 返回 `InvalidInput`，什么都不写；没有依赖目录的仓库用 `dirs = []` 的依赖集登记成功 | 新增 |
| `generation_paths_derive_from_keys` | `base_dir` 与 `deps_dir` 给出 `<root>/bases/<repo>/<tree>/<deps>` 与 `<root>/deps/<repo>/<deps>`；同一个 tree 配两个依赖集得到两个不同的目录 | 新增 |
| `retire_dir_only_moves_unregistered_dirs_to_trash` | 没登记的 generation 目录、removed 会话的目录被改名进 `store/trash/`，内容完好；已登记的 generation、活会话的目录、三种形状以外的路径、含符号链接的路径都返回 `InvalidInput`，原样不动 | 新增 |
| `session_manifest_roundtrip` | `create_session` 之后，`session` 读回的 `SessionManifest` 与写入的相等，lifecycle 为 `Creating`，created 等于传入的时刻；会话 id 或工作区路径重复返回 `Exists`；基线没登记返回 `InvalidInput` | 新增 |
| `lifecycle_moves_forward_by_compare_and_swap` | 四种允许的转换成功；当前不是 from 时返回 false，什么都不写；Removed→Active 之类的转换返回 `InvalidInput`；进入 Removed 时记下时刻 | 新增 |
| `workspace_base_moves_tree_and_deps_together` | `set_workspace_base` 之后，读回的 `WorkspaceState.base` 的 tree 与 deps 都是新值，head 也是新值；用 `PauseInWriteTxn` 停在 UPDATE 之前时，另一个读看到的两者都还是旧值（不变量 8） | 新增 |
| `seeds_are_written_once` | `record_seeds` 第一次成功并能读回；第二次返回 `InvalidInput`，原值不变 | 新增 |
| `record_checkpoint_roundtrip_keeps_base` | 五种条目（含 `Renamed` 的 from 与 `Untracked`）记下之后，`checkpoint` 读回相等，`base` 是记录时对着的 `BaseGen`；输入里的 id 被忽略，返回值是分配的号 | 新增 |
| `checkpoint_ids_monotone_per_session` | 两个会话交替记 checkpoint，各自从 1 起连续递增，互不共用 | layerfs-persistence/tests/history_allocation.rs::reservations_are_monotone_half_open_and_per_scope |
| `failed_record_consumes_no_id` | 一次 record 失败（`AfterCheckpointRow`）之后，失败的那次没有留下任何行；下一次成功拿到上一个已提交的号加 1；提交过的号从不复用。与 3a:589 原文"失败留下空洞"不同：strata 的号与 checkpoint 行在同一个事务里出现，回滚的尝试不占号（§4.4） | layerfs-persistence/tests/history_allocation.rs::a_consumed_reservation_survives_a_later_failure |
| `checkpoint_id_refuses_past_i64_max` | 用原始连接放进一行 id 为 `i64::MAX` 的 checkpoint：下一次 record 返回 `TooLarge`（`checkpoint_id`），所有表的行不变，不回绕 | layerfs-persistence/tests/history_allocation.rs::counts_are_checked_and_the_terminal_endpoint_is_refused<br>layerfs-persistence/tests/history_remediation.rs::last_representable_reservation_is_consumed_and_terminal_refusal_is_atomic |
| `ids_survive_reopen` | 写入 blob 与 checkpoint，关闭再打开：都能读回；同内容再 put 计为复用；下一个号紧接着上一个 | layerfs-persistence/tests/sqlite_publication.rs::canonical_save_read_reuse_and_persisted_reservations_survive_reopen |
| `negative_id_read_back_is_corrupt` | 用原始连接（`PRAGMA ignore_check_constraints = 1`）放进一行负 id 的 checkpoint：`checkpoints` 与 `checkpoint` 读到它时返回 `Corrupt`（`bad_row`），不把负数交给 `CheckpointId::from` | 新增 |
| `tool_call_checkpoint_recorded_once` | 同一会话、同一 repo、同一个 `ToolCall { id }` 记两次：第二次什么都不写，返回第一次的号；换一个 repo 或换一个工具调用 id 各记一个新的 | 新增 |
| `checkpoint_unit_is_one_transaction` | checkpoint 行、全部条目与新 blob 的登记在一个事务里：用 `PauseInWriteTxn` 停在提交之前，另一个读看不到其中任何一项；放行之后同时看到全部；已登记过的 blob 按先到者为准，不报错 | layerfs-persistence/tests/sqlite_publication.rs::final_body_multirow_locators_and_first_wins_have_one_atomic_ack |
| `record_checkpoint_failure_rolls_back_whole_unit` | 在插入 checkpoint 行之后注入失败：没有 checkpoint 行，没有条目，没有新的 blob 登记；之前的 checkpoint 与 blob 仍可读；失败只报告一次，store 不自己重试 | layerfs-content/tests/filesystem_failure.rs::a_read_failure_at_each_boundary_publishes_no_root_and_keeps_the_old_one<br>layerfs-content/tests/filesystem_failure.rs::a_refusing_consumer_ends_the_operation_once_without_a_root<br>layerfs-persistence/tests/sqlite_publication.rs::failure_after_body_insert_rolls_back_the_entire_unit_and_published_body_is_immutable |
| `late_source_failure_writes_no_checkpoint` | 一组文件 put 到第 k 个时源文件出错：错误原样传出，不调用 record，没有 checkpoint 行；已 put 的 k−1 个 blob 留在 `blobs/` 里，gc 过了年龄下限收走 | layerfs-content/tests/filesystem_sorted.rs::a_late_source_error_propagates_and_publishes_nothing<br>layerfs-workspace/tests/captured_namespace_custody.rs::a_consumer_that_rejects_a_file_root_leaves_the_file_custody |
| `checkpoint_refuses_reference_to_missing_blob` | 条目引用一个从没 put 过的 BlobId：record 返回 `Corrupt`（`blob_missing`），什么都不写；引用由 store 按条目自己推导、核对；提交成功的 checkpoint 引用的 blob 都已在 `blobs/` 里并已登记 | layerfs-content/tests/streaming.rs::children_are_emitted_before_their_parents<br>layerfs-persistence/tests/semantic_admission.rs::omitted_wire_references_cannot_hide_an_inode_dependency_from_save<br>layerfs-persistence/tests/sqlite_reference_acquisition.rs::missing_logical_child_is_still_refused |
| `interleaved_sessions_do_not_see_failed_checkpoints` | 三个会话的 put 与 record 交错进行，其中一个注入失败：成功的两个可读，失败的那个不可见，互不影响 | layerfs-persistence/tests/interleaved_saves.rs::scoped_registry_interleaves_pending_reads_finish_abort_and_slot_reuse |
| `checkpoint_refused_for_untracked_or_removed` | `tracked = false` 的工作区返回 `InvalidInput`；会话已 Removed 返回 `NoSuchSession`；会话里没有这个 repo 返回 `NoSuchRepo`；三种都不写 | 新增 |
| `proposal_ids_never_reused` | 记三个提议，决定号最大的那个并让 gc 在过了 keep 之后删掉它：新提议拿到的号仍比被删的大；`decide_proposal` 只对 Pending 生效 | 新增 |
| `publish_record_roundtrip_and_phase_machine` | `record_publish` 读回的 `ReleaseGroup` 与写入的相同，批次与路径的顺序不变，id 是分配的号；Running→Unknown→Done 成功，Done→Running 返回 `InvalidInput`；批次点名不存在的 checkpoint 返回 `NoSuchCheckpoint` | 新增 |
| `open_publishes_listed_for_restart` | 阶段为 Running 与 Unknown 的发布出现在 `open_publishes` 里，Done 的不出现；重开 Store 之后结果相同 | 新增 |
| `ref_audit_only_strata_refs_for_update_ref` | `UpdateRef` 配 `refs/heads/main` 返回 `InvalidInput`，配 `refs/strata/publish/main` 成功；`Push` 配 `HeadMoved` 返回 `InvalidInput`；`confirmed` 读回的 `Action` 与写入的相同 | 新增 |
| `committed_rows_are_immutable` | 用注册了 `strata_gc_active`（返回 0）的原始连接：每张不可变表上的 UPDATE 都被触发器拦下（`immutable`）；`INSERT OR REPLACE` 撞到已有行时被删除守卫拦下；一批插入里有一行冲突时整批回滚，不留半批；已存的基线与依赖集行不被改写 | layerfs-persistence/tests/inode_branch_delta_custody.rs::a_branch_base_cannot_be_dropped_and_survives_reclamation<br>layerfs-persistence/tests/sqlite_inode_values.rs::a_stored_value_is_never_replaced_and_a_refused_batch_leaves_no_row<br>layerfs-persistence/tests/sqlite_publication.rs::failure_after_body_insert_rolls_back_the_entire_unit_and_published_body_is_immutable<br>layerfs-workspace/tests/operation_scope.rs::scoped_views_share_original_install_serials_and_keep_original_provider |
| `delete_outside_gc_is_refused` | gc 以外的 DELETE（store 的原始连接、没有注册函数的 `sqlite3` 连接）都失败，行不变；gc 期间的级联删除成功；`publishes`、`publish_items`、`refs_audit`、`gc_runs` 连 gc 也删不掉 | 新增 |
| `external_writer_is_busy_without_side_effects` | 子进程 `lock_child` 开原始连接并持着 `BEGIN IMMEDIATE`：本进程的 record 立即返回 `Busy`（`sqlite`），不重试，没有副作用；子进程放手之后，同样的调用成功 | layerfs-persistence/tests/serverless_store.rs::lock_child<br>layerfs-persistence/tests/serverless_store.rs::second_process_busy_has_no_effect_and_readers_keep_progressing<br>layerfs-persistence/tests/sqlite_publication.rs::readonly_refusal_and_external_writer_busy_do_not_retry<br>layerfs-daemon/tests/installed_store.rs::direct_reads_finish_under_another_process_writer_and_busy_stays_typed |
| `readers_never_block_on_writer` | 外部进程持写锁时，以及本进程的写事务停在 `PauseInWriteTxn` 时，另一个线程的读（`session`、`checkpoint`）都在 100 ms 内完成，读到提交之前的状态；只读打开的 Store 同样如此 | layerfs-persistence/tests/serverless_store.rs::second_process_busy_has_no_effect_and_readers_keep_progressing<br>layerfs-daemon/tests/installed_store.rs::direct_reads_finish_under_another_process_writer_and_busy_stays_typed |
| `killed_lock_holder_leaves_no_stale_lock` | 子进程以写者身份打开 Store、开着写事务时被 SIGKILL：父进程随即 `open` 成功并能写，没有陈旧的 LOCK 或 SQLite 锁 | layerfs-persistence/tests/writer_admission.rs::a_holder_process_that_exits_hands_the_lock_to_the_waiter |
| `checkpoint_write_is_durable_after_crash` | 子进程 `crash_child` put 两个 blob、record 一个 checkpoint，打印确认后等待；父进程收到确认即 SIGKILL 它，然后重开：checkpoint 行、全部条目、两个 blob 的文件与行都在，下一个号紧接着。这是 `synchronous = FULL` 的验证方法的一部分：SIGKILL 只证明"确认等于已提交"（进程被杀后页缓存仍在，`synchronous = OFF` 也能通过），FULL 本身由 `open_pins_durable_pragmas`（回读）与 `commit_issues_full_fsync`（同步调用确实发出）证明；不声称断电级持久 | layerfs-persistence/tests/sqlite_recovery.rs::crash_child<br>layerfs-persistence/tests/sqlite_recovery.rs::sigkill_preserves_acknowledged_body_locator_and_reservation |
| `commit_issues_full_fsync` | 插桩的 VFS 计数：每次 record 提交，在 record 返回之前，WAL 上至少有一次 `SQLITE_SYNC_FULL` 标志的 xSync（`fullfsync = 1` 时即 `F_FULLFSYNC`），`SQLITE_SYNC_NORMAL` 标志的 xSync 为 0 | 新增 |
| `commit_io_error_is_unknown_and_poisons_writer` | `CommitSyncFails`：record 返回 `Unknown`（`uncertain`）；此后的写返回 `Unknown`（`poisoned`），读照常；重开之后读到的就是真实结果，同一工具调用的重放要么返回已有的号、要么记一个新的；不确定的结果从不被当成确定的失败去重放或清理 | layerfs-overlay/tests/error_identity.rs::a_real_sqlite_full_identity_never_unwraps_an_uncertain_outcome |
| `disk_full_during_checkpoint_keeps_previous_state` | root 放在 64 MiB 的 APFS 磁盘映像上并写满：put 返回 `Unknown`（`not_written`），record 返回 `Unknown`（`not_written` 或 `uncertain`），都作为错误上报；之前的 checkpoint 与 blob 原样可读，store 不重放、不清理；腾出空间之后新的 checkpoint 成功（`#[ignore]`） | layerfs-overlay/tests/error_identity.rs::a_real_sqlite_full_identity_never_unwraps_an_uncertain_outcome<br>layerfs-overlay/tests/orphans.rs::a_failed_release_keeps_exact_custody_until_terminal_file_disposal<br>layerfs-telemetry/tests/full_disk.rs::actual_enospc_preserves_the_original_result<br>layerfs-daemon/tests/mounted_sqlite_full.rs::known_sqlite_full_is_enospc_and_normal_drain_completes |
| `gc_keeps_referenced_generations` | 基线 G1 被活会话的 checkpoint 引用，之后又建了基线 G2：`gc(keep = 0)` 之后 G1 的行与目录仍在，它上面的 checkpoint 可读，blob 都在；引用它的会话 removed 并过了保留期之后，G1 被回收（行删掉、目录进 trash），G2（最新）留着；被引用的 blob 行删不掉；回收按引用计数，不分页 | layerfs-persistence/tests/inode_branch_delta_custody.rs::a_branch_base_cannot_be_dropped_and_survives_reclamation<br>tgrep-core/tests/generations.rs::retained_views_and_checkpoints_outlive_managers_and_new_generations<br>layerfs-overlay/tests/payload_runs.rs::a_maintenance_page_is_fourteen_cells_of_bytes_whatever_the_rows |
| `gc_reclaims_removed_sessions_after_keep` | removed 未满 keep 的会话全部保留，含它的 BeforeRemove checkpoint；满了之后，它的工作区行、checkpoint、条目被删，只被它引用的 blob 在过了下限之后被删 | 新增 |
| `gc_keeps_newest_base_per_repo` | 一个 repo 最新的基线即使没有任何引用也不被回收；更旧的、没有引用、过了 keep 的被回收 | 新增 |
| `gc_keeps_pending_proposal_deps` | 待确认提议的 from 与 to 依赖集不被回收；提议被决定、且过了 keep 之后，提议行与不再被引用的依赖集一起被回收 | 新增 |
| `gc_never_deletes_young_blobs` | `keep = 0` 时，刚 put、还没被 record 引用的 blob 不被删（年龄下限 1 小时）；把它的 mtime 改到 2 小时前之后被删；之后 record 引用这个已被删的 blob 返回 `Corrupt`，不会写出悬空引用 | 新增 |
| `gc_keeps_open_publish_scenes` | removed 并过了 keep 的会话，只要有 Running 或 Unknown 的发布引用它的 checkpoint，就整个保留；发布变成 Done 之后，下一轮回收它 | 新增 |
| `gc_retires_dirs_through_trash_and_resumes` | 回收的 generation 目录先进 trash，过了 keep 才删；用 `GcStopAfterRetire` 让 gc 在挪目录中途失败：返回 `Unknown`（`partial`），已提交的批次有效，下一轮收走活路径上没登记的目录并把 trash 删完；trash 里带 `uchg` 的文件照样删掉；删不掉的条目（所在目录权限为 0500）进 `stuck`，整轮不失败 | 新增 |
| `gc_touches_nothing_outside_its_areas` | `sessions/` 下的一切、`bases/` 下以 `.` 开头的临时目录、解析不成键的名字、指向 root 之外的符号链接，gc 之后都原样存在，链接的目标没有被删 | 新增 |
| `gc_never_reclaims_live_session_with_missing_dirs` | 活会话的工作区目录被人手删掉：gc 不回收它的任何行，checkpoint 仍可读；这样的现场由 status 报出，不由 gc 清理 | sdk/js/native/tests/unit/reclaim.rs::a_gone_container_is_reclaimed_by_removing_its_records |
| `gc_scrub_reports_corrupt_and_resumes_from_cursor` | 用 `ScrubBudget` 把预算设为 4 KiB：每轮核对一段，游标前进，走到末尾回到开头；改坏的 blob 在扫到它的那一轮进 `corrupt` | 新增 |
| `gc_report_persisted_for_status` | gc 返回的报告与 `last_gc_report` 读回的相同；重开之后仍能读到；`gc_runs` 只增不删 | 新增 |

覆盖：目标模块为 store 的 56 行（3a 45、3c 4、3d 5、3f 2）加裁定改归 store 的 3c L139，共 57 行，全部出现在上表的来源栏里；另引用理由点名 store 的 `—` 行 14 条（3a 2、3b 4、3c 5、3d 2、3e 1）。上表共 82 个测试，其中新增 39 个。3b 的 `layerfs-telemetry/tests/platform.rs::retention_expiry_open_reader_and_deletion_failure` 理由写明"不以此为依据"，所以 gc 保留期的测试记为新增。

## 8. 实现任务（TDD）

各任务共用的约定：

- `Cargo.toml` 在 `[features]` 里声明 `fault-injection = []`，并在 `[dev-dependencies]` 里写 `strata-store = { path = ".", features = ["fault-injection"] }`，测试总能用到 §3 的测试钩子，发布构建里没有它们 [我的决定]。
- `tests/support/mod.rs` 提供：同卷的临时 root 与假 checkout；登记一个依赖集、清单、基线并建一个会话的夹具；注册了 `strata_gc_active` 的原始连接；按名字重新执行本测试二进制的子进程助手；`hdiutil` 磁盘映像（APFS、FAT，Drop 时卸载）。
- 每个任务的测试放在自己的测试文件里，Run 行用 `--test <文件名>` 跑整个文件。

### Task 1: 打开、布局与 schema

**Files:**
- Create: `crates/strata-store/Cargo.toml`、`crates/strata-store/src/lib.rs`、`crates/strata-store/src/schema.rs`、`crates/strata-store/src/open.rs`、`crates/strata-store/src/errors.rs`
- Test: `crates/strata-store/tests/open.rs`、`crates/strata-store/tests/support/mod.rs`

**Interfaces:**
- Consumes: `strata_types::{StrataError, Code}`、`apfs::{same_volume, clone_file}`
- Produces: `Store::open`、`Store::open_read_only`、`Store::root`、`Store::is_read_only`；§4.3 的 DDL 文本；§4.2 的设置与回读；`detail.outcome` 的构造

- [ ] **Step 1: 写失败的测试**：`open_creates_layout_on_empty_root`、`interrupted_creation_is_resumed`、`open_refuses_nonempty_foreign_root`、`open_refuses_lost_store_db`、`open_refuses_cross_volume_root`、`open_refuses_volume_without_clonefile`、`open_pins_durable_pragmas`、`open_refuses_non_wal_store`、`open_refuses_schema_drift`、`open_refuses_unknown_user_version`、`open_leaves_unrelated_database_untouched`、`second_writer_open_is_busy`、`read_only_open_refuses_every_write`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-store --test open`
Expected: FAIL（`Store::open` 还不存在，编译失败）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.1 的建库顺序（同卷与非空检查在任何写入之前，LOCK，clonefile 探测失败时 rmdir 自己的空目录，临时库加 `RENAME_EXCL`）；§5.2 的文件头预检、身份核对、设置回读、`:memory:` schema 比对、`quick_check`；只读打开与 `ReadOnly` 的统一入口；errno 与 SQLite 错误到 `StrataError` 的归类（types.md §6.3 加 §6.2 的 `detail`）
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-store --test open`（磁盘映像的两个用 `cargo test -p strata-store --test open -- --ignored`）
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-store/Cargo.toml crates/strata-store/src/lib.rs crates/strata-store/src/schema.rs crates/strata-store/src/open.rs crates/strata-store/src/errors.rs crates/strata-store/tests/open.rs crates/strata-store/tests/support/mod.rs
git commit -m "feat(store): open, layout and pinned schema"
```

### Task 2: blob 存储

**Files:**
- Create: `crates/strata-store/src/blob.rs`
- Test: `crates/strata-store/tests/blob.rs`

**Interfaces:**
- Consumes: `apfs::clone_file`、`strata_types::BlobId`
- Produces: `Store::put_blob`、`Store::put_bytes`、`Store::get_blob`、`Store::read_blob`、`Store::verify_blob`；`FaultPoint::{BlobRename, TempNameTaken}`；`SyncCounts` 的 blob 与目录屏障计数

- [ ] **Step 1: 写失败的测试**：`blob_put_is_idempotent`、`blob_id_ignores_delivery_shape`、`blob_known_answer_vectors`、`empty_file_is_a_valid_blob`、`stored_blob_is_never_rewritten`、`blob_identity_changes_with_any_byte`、`blob_roundtrip_is_byte_exact`、`large_and_sparse_blobs_roundtrip`、`same_bytes_new_path_or_mode_reuse_blob`、`blob_files_are_content_only`、`symlink_blob_stores_target_text`、`put_blob_refuses_directory_and_special_files`、`get_blob_refuses_existing_dst`、`get_blob_missing_is_corrupt_and_leaves_no_dst`、`missing_or_mismatched_blob_is_corrupt_never_new`、`get_blob_is_trusted_verify_detects_flipped_bit`、`put_reuse_does_not_reread_existing_copy`（gc 扫描那一半在 Task 9 补断言）、`put_reuse_with_length_mismatch_is_corrupt`、`put_restores_missing_blob_file`、`stale_temp_files_do_not_block_put`、`concurrent_puts_of_same_content_store_one_blob`、`put_blob_orders_blob_before_later_writes`、`corrupt_blob_is_reported_never_overwritten`（gc 扫描那一半在 Task 9 补断言）
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-store --test blob`
Expected: FAIL（`put_blob` 等还不存在）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.3 的克隆、只看克隆体、规整（标志、mode、扩展属性的跳过表）、BLAKE3（16 MiB 起 mmap 加 rayon）、`futimens`、两个 `F_BARRIERFSYNC`、`RENAME_EXCL` 与 `EEXIST` 分支；§5.4 的受信任 get（0644、mtime 为当前时刻）、read 的上限、verify 的重算。`blobs` 行的读在这里只读，登记在 Task 5
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-store --test blob`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-store/src/blob.rs crates/strata-store/tests/blob.rs
git commit -m "feat(store): content-addressed blob put, get and verify"
```

### Task 3: generation、清单与 `<root>` 布局

**Files:**
- Create: `crates/strata-store/src/gens.rs`
- Test: `crates/strata-store/tests/gens.rs`

**Interfaces:**
- Consumes: `strata_types::{DepsSet, BaseManifest, ManifestEntry, BaseGen, RepoName, TreeOid, DepsSetId}`
- Produces: `Store::deps_dir`、`Store::base_dir`、`Store::record_deps_set`、`Store::deps_set`、`Store::record_manifest`、`Store::manifest`、`Store::record_base_gen`、`Store::has_base_gen`、`Store::retire_dir`

- [ ] **Step 1: 写失败的测试**：`generation_rows_register_and_read_back`、`manifest_absent_and_empty_are_distinct`、`base_gen_requires_registered_tree_and_deps`、`generation_paths_derive_from_keys`、`retire_dir_only_moves_unregistered_dirs_to_trash`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-store --test gens`
Expected: FAIL（这些函数还不存在）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.5 的幂等登记（相同不写、不同 `InvalidInput`）、清单的一事务写入、按键算路径、`retire_dir` 的路径解析（逐级拒绝符号链接）与改名进 trash
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-store --test gens`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-store/src/gens.rs crates/strata-store/tests/gens.rs
git commit -m "feat(store): generations, manifests and root layout"
```

### Task 4: 会话与工作区

**Files:**
- Create: `crates/strata-store/src/sessions.rs`
- Test: `crates/strata-store/tests/sessions.rs`

**Interfaces:**
- Consumes: `strata_types::{SessionManifest, WorkspaceState, SessionId, CommitOid}`；Task 3 的 `record_base_gen`
- Produces: `Store::create_session`、`Store::set_lifecycle`、`Store::set_workspace_base`、`Store::record_seeds`、`Store::session`、`Store::sessions`、`Lifecycle`、`SessionRecord`；`FaultPoint::PauseInWriteTxn`

- [ ] **Step 1: 写失败的测试**：`session_manifest_roundtrip`、`lifecycle_moves_forward_by_compare_and_swap`、`workspace_base_moves_tree_and_deps_together`、`seeds_are_written_once`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-store --test sessions`
Expected: FAIL（这些函数还不存在）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.5 的会话事务、比较并交换的生命周期（区分不存在与不是 from）、一条 UPDATE 的工作区移动、只写一次的种子记录；读连接池与读事务（§5.8）在这里第一次用到
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-store --test sessions`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-store/src/sessions.rs crates/strata-store/tests/sessions.rs
git commit -m "feat(store): sessions, workspaces and lifecycle"
```

### Task 5: checkpoint

**Files:**
- Create: `crates/strata-store/src/checkpoints.rs`
- Test: `crates/strata-store/tests/checkpoints.rs`

**Interfaces:**
- Consumes: `strata_types::{Checkpoint, CheckpointId, CheckpointReason, OverlayEntry, EntryKind}`；Task 2 的 blob 文件布局；Task 4 的会话与工作区行
- Produces: `Store::record_checkpoint`、`Store::checkpoint`、`Store::latest_checkpoint`、`Store::checkpoints`、`CheckpointInfo`；`FaultPoint::AfterCheckpointRow`

- [ ] **Step 1: 写失败的测试**：`record_checkpoint_roundtrip_keeps_base`、`checkpoint_ids_monotone_per_session`、`failed_record_consumes_no_id`、`checkpoint_id_refuses_past_i64_max`、`ids_survive_reopen`、`negative_id_read_back_is_corrupt`、`tool_call_checkpoint_recorded_once`、`checkpoint_unit_is_one_transaction`、`record_checkpoint_failure_rolls_back_whole_unit`、`late_source_failure_writes_no_checkpoint`、`checkpoint_refuses_reference_to_missing_blob`、`interleaved_sessions_do_not_see_failed_checkpoints`、`checkpoint_refused_for_untracked_or_removed`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-store --test checkpoints`
Expected: FAIL（`record_checkpoint` 还不存在）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.6 的十步：纯校验、`BEGIN IMMEDIATE` 与 `Busy`、事务内核对、工具调用的幂等、blob 文件核对与先到者为准的登记、`MAX + 1` 与 `TooLarge`、批量插入、提交错误分成 `not_written` 与 `uncertain`；读回时的 `TryFrom` 与负 id 检查
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-store --test checkpoints`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-store/src/checkpoints.rs crates/strata-store/tests/checkpoints.rs
git commit -m "feat(store): atomic checkpoint records with store-assigned ids"
```

### Task 6: 提议、发布与 ref 审计

**Files:**
- Create: `crates/strata-store/src/records.rs`
- Test: `crates/strata-store/tests/records.rs`

**Interfaces:**
- Consumes: `strata_types::{ReleaseGroup, Batch, ProposalId, Action, CommitOid}`
- Produces: `Store::record_proposal`、`Store::decide_proposal`、`Store::proposal`、`Store::proposals`、`Store::record_publish`、`Store::set_publish_phase`、`Store::publish_record`、`Store::open_publishes`、`Store::record_ref_change`、`Store::ref_changes`、`ProposalRow`、`ProposalStatus`、`PublishRecord`、`PublishPhase`、`RefChange`、`RefOp`、`RefOutcome`

- [ ] **Step 1: 写失败的测试**：`proposal_ids_never_reused`（gc 那一步先用夹具里的原始连接在 gc 模式下删除，Task 9 换成真的 gc）、`publish_record_roundtrip_and_phase_machine`、`open_publishes_listed_for_restart`、`ref_audit_only_strata_refs_for_update_ref`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-store --test records`
Expected: FAIL（这些函数还不存在）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.7：AUTOINCREMENT 的提议号与到顶检查、提议的比较并交换、发布行与批次路径原样照抄、阶段转换、ref 审计的 op 与 outcome 配对、`Action` 的 JSON
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-store --test records`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-store/src/records.rs crates/strata-store/tests/records.rs
git commit -m "feat(store): proposals, publish records and ref audit"
```

### Task 7: schema 守卫

**Files:**
- Modify: `crates/strata-store/src/schema.rs`（守卫随 Task 1 的 DDL 已经在位；这里只补 `strata_gc_active` 的开关）
- Test: `crates/strata-store/tests/guards.rs`

**Interfaces:**
- Consumes: Task 1–6 写出的各种行
- Produces: 无新的公开接口；证明 §4.4 的可变列与 §4.5 S2 由 schema 兜底

- [ ] **Step 1: 写失败的测试**：`committed_rows_are_immutable`、`delete_outside_gc_is_refused`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-store --test guards`
Expected: FAIL（`strata_gc_active` 的开关还没有暴露给 gc 路径，gc 期间的级联删除那一项失败）

- [ ] **Step 3: 写让测试通过的最少实现**：写连接上注册 `strata_gc_active`，由 `gc_active` 控制；确认 `recursive_triggers = 1` 让 REPLACE 撞上删除守卫
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-store --test guards`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-store/src/schema.rs crates/strata-store/tests/guards.rs
git commit -m "feat(store): immutability and gc-only delete guards"
```

### Task 8: 并发与崩溃

**Files:**
- Create: `crates/strata-store/src/fault.rs`
- Test: `crates/strata-store/tests/concurrency.rs`、`crates/strata-store/tests/crash.rs`

**Interfaces:**
- Consumes: Task 1 的 LOCK 与写连接、Task 5 的 `record_checkpoint`；`rusqlite::ffi`（插桩 VFS）
- Produces: `FaultPoint::CommitSyncFails`、`Store::inject_fault`、`Store::release_fault`、`Store::sync_counts` 的 WAL 计数；写者中毒

- [ ] **Step 1: 写失败的测试**：`external_writer_is_busy_without_side_effects`（含子进程 `lock_child`）、`readers_never_block_on_writer`、`killed_lock_holder_leaves_no_stale_lock`、`checkpoint_write_is_durable_after_crash`（含子进程 `crash_child`）、`commit_issues_full_fsync`、`commit_io_error_is_unknown_and_poisons_writer`、`disk_full_during_checkpoint_keeps_previous_state`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-store --test concurrency`，`cargo test -p strata-store --test crash`
Expected: FAIL（插桩 VFS 与中毒还不存在；同步计数为 0）

- [ ] **Step 3: 写让测试通过的最少实现**：fault-injection 下注册包住 SQLite 缺省 `unix` VFS 的插桩 VFS，按文件后缀与 `flags & 0x0F` 分别计数 xSync，并能让下一次 WAL 的 xSync 返回 `SQLITE_IOERR`；提交错误为 `uncertain` 时置 `poisoned`；子进程助手（重新执行测试二进制、读确认行、SIGKILL）
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-store --test concurrency`，`cargo test -p strata-store --test crash`（磁盘满那个用 `-- --ignored`）
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-store/src/fault.rs crates/strata-store/tests/concurrency.rs crates/strata-store/tests/crash.rs
git commit -m "feat(store): busy, reader isolation and crash durability"
```

### Task 9: gc

**Files:**
- Create: `crates/strata-store/src/gc.rs`
- Test: `crates/strata-store/tests/gc.rs`

**Interfaces:**
- Consumes: `apfs::open_rooted`；Task 2 的 `verify_blob`；Task 3–6 的全部表
- Produces: `Store::gc`、`Store::last_gc_report`、`GcReport`；`FaultPoint::{GcStopAfterRetire, ScrubBudget}`

- [ ] **Step 1: 写失败的测试**：`gc_keeps_referenced_generations`、`gc_reclaims_removed_sessions_after_keep`、`gc_keeps_newest_base_per_repo`、`gc_keeps_pending_proposal_deps`、`gc_never_deletes_young_blobs`、`gc_keeps_open_publish_scenes`、`gc_retires_dirs_through_trash_and_resumes`、`gc_touches_nothing_outside_its_areas`、`gc_never_reclaims_live_session_with_missing_dirs`、`gc_scrub_reports_corrupt_and_resumes_from_cursor`、`gc_report_persisted_for_status`；并补上 `put_reuse_does_not_reread_existing_copy`、`corrupt_blob_is_reported_never_overwritten` 里 gc 扫描的断言，`proposal_ids_never_reused` 改用真的 gc
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-store --test gc`
Expected: FAIL（`gc` 还不存在）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.9 的七步：保留集（R、被钉住的基线、最新基线、提议）、分批删行与 `strata_gc_active`、blob 段的 L3 独占、先删行后挪目录、只在 store 区域清扫（按键解析、不跟随链接）、trash 的 `open_rooted` 删除与 `stuck`、带游标的完整性扫描、`gc_runs` 追加
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-store --test gc`，再跑一遍 `cargo test -p strata-store` 确认补上的断言全绿
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-store/src/gc.rs crates/strata-store/tests/gc.rs crates/strata-store/tests/blob.rs crates/strata-store/tests/records.rs
git commit -m "feat(store): reference-counted gc with trash and integrity scan"
```

## 9. 构建者决定记录

| 事项（design 里标 [交给构建者]、落在本模块） | 选择 | 理由 | 日期 |
| --- | --- | --- | --- |
| generation 保留期（design §7、§12） | 由 gc 的参数 `keep` 决定；默认值由 daemon.md 定，本文建议 14 天。blob、临时文件与未登记目录另有固定的年龄下限 1 小时 | 保留期是调度策略，daemon 管调度；下限保护 put 与 record 之间的窗口 | 2026-10-22；默认值待定（daemon.md） |
| `rm` 之前最后一次 checkpoint 保留 N 天（design §6.6） | 不单独设 N：removed 会话的全部行（含 BeforeRemove checkpoint）保留到 `removed_ns + keep`，之后由 gc 一起回收 | 一个参数管所有回收，行为可以预期 | 2026-10-22 |
| 封存目录的只读手段（design §7、§12），gc 这一侧 | gc 删 trash 之前清掉 `UF_IMMUTABLE` 与 `UF_APPEND`，`chflags uchg` 与"仅靠约定"两种手段都删得掉；手段本身由 session.md、deps.md 定 | clonefile 会把 `uchg` 一起复制（2026-10-22 实测），无论选哪种手段，gc 都必须删得掉 | 2026-10-22；手段本身待定（session.md、deps.md） |
