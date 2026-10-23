# `search` 模块

**Crate**：`crates/strata-search` · **设计来源**：design.md §5、§6.3、§7、§8、§9 不变量 2、5、6；tgrep `SHARED_WORKTREE_INDEXES.md`（d25ade6）的 "Overall architecture"、"Search flow"、"Implemented generation API"、"Implemented worktree synchronization API" · **矩阵**：目标模块为 `search` 的 22 行（3f 21、3d 1），以及理由点名 search 的 `—` 行 50 条，逐条见 §7

> 本文按 module-rulings.md 写：「Common」「T11 search」，以及增补里标了 T11 或 Everyone 的条目。后写的、写明 amended 的条目覆盖先写的。与 design.md 旧措辞不同的地方以裁定为准：工作区路径是 `<root>/sessions/<session-id>/<repo>/`；配置目录是 `~/Library/Application Support/git-strata/`（search 不读它，索引不放那儿）。邻居版本：overlay.md @08ffa53，session.md @a324f9c。`ViewId`、`Code`、`BaseGen`、`BaseManifest`、`OverlayEntry` 用 types.md 的定义，本文不重新定义。`Overlay` 用 overlay.md §3 的定义，只按名字使用。不新增错误码。标 [我的决定 Dn] 的地方是 design 和裁定都没有定、由本文决定的，§9 按编号列出。

控制器初稿里「`WorktreeView::new` 挂上工作区」这一步，被增补 2 的 T11 条改掉了：会话工作区的 `git rev-parse --git-common-dir` 是该会话的影子仓库，不是主 checkout，tgrep 的同仓库工作树检查会拒绝它，而增补 2 又禁止把代理用的工作树接到主 checkout 的 common dir。初稿的结果都保留：基线按 tree 复用、overlay 与墓碑只属于本会话、`Which::All` 的每条命中带 session 与 repo、零命中是空列表、未知视图是 `NoSuchView`、排除 `.git` gitfile 与 `.strata/`、不用托管 v2 / `stale_reads` / `.ignore` / `p4ignore.ini` / Windows 路径 / tgrep CLI 选项、`GrepOpts` 只有大小写。变的是做法，见 §5.1。

## 1. 职责与边界

search 让一个已挂上的视图，或全部已挂上的视图，在「共享基线索引 + 本会话 overlay」上做 grep 和按 glob 列文件（design §6.3）。

做：

1. `attach` / `detach`：给一个 (会话, repo) 挂上或卸下视图，发 `ViewId`。
2. `refresh`：按 daemon 交来的变更路径，更新该视图的私有 overlay 索引。
3. `grep`：在一个视图或全部已挂视图上查一个模式。`Which::All` 的每条 `Hit` 带 `session` 与 `repo`。
4. `files`：按 glob 列出该视图的文件名成员。成员来自基线的跟踪路径加本会话 overlay，不来自倒排表。
5. 基线索引：按（主 checkout 的仓库身份，tree OID，profile）复用。两个会话钉同一个 tree、同一个 profile，共用一份，不各建一份。

不做：

- 不算 overlay、不写 checkpoint、不投影、不改工作区。那些是 overlay 与 session。
- 不建索引格式、不实现三元组查询。那是 tgrep-core，本文只引用它的公开入口，不抄实现。
- 不调用 `WorktreeView::new`、`create_worktree`、`with_snapshot`、`save_checkpoint`、`restore`。原因在 §5.1。这些入口上的 3f 行改成未使用 API 或 不保留，§4.6 逐条列出，不把它们改挂到 search 上重做。
- 不用 tgrep 的托管 v2 存储、`stale_reads`、`.ignore`、`p4ignore.ini`、Windows 路径，也不暴露 tgrep CLI 的选项（`--type`、编码标签、`-P`、固定字符串开关等）。`strata grep` / `files` 只有 `--session` 与 `--all`（design §6.3，增补 1 的 3f 条）。
- 不看别的会话的未发布改动，除非调用方传 `Which::All`，并且那个会话的视图已经挂上。
- 不拿 repo 写锁，不进会话队列。search 只读工作区。
- 不写主 checkout 的工作树、索引、`refs/heads/*`，也不把索引写进它的 `.git`（不变量 2）。`GenerationManager::new` 会写 common dir，禁止调用。
- 不删基线索引。tgrep 的 `RetentionPolicy::RetainAll` 本来就不删；v1 也不另做回收 [我的决定 D8]。

对应 design.md §6.3 的 `grep`。`status` 与 `diff` 不是本模块：`status` 是 session，`diff` 是 publish 投影之后的比较。

## 2. 依赖

依赖方向按 types.md §8：search 在第 4 层，可以依赖第 0–3 层，不依赖 publish、daemon、cli、confirm。外部的 `tgrep-core` 只给 search 用。

| 依赖 | 用什么 | 为什么 |
| --- | --- | --- |
| `strata-types` | `ViewId`、`SessionId`、`RepoName`、`RepoPath`、`TreeOid`、`CommitOid`、`BaseGen`、`Config`、`RepoConfig`、`StrataError`、`Code` | 跨模块名字只来自这里 |
| `strata-session` | `Sessions::status` | 工作区路径、`base`、`head`、`tracked`、生命周期。不调用 `move_workspace` |
| `strata-overlay` | `Overlay`、`Overlay::hides` / `tombstones` / `paired` / `get`、`MERGE_MAX_BYTES`、`Tracker` 的结果（经下面的 trait） | 本会话的增、改、删、改名。大小阈值与 overlay 用同一个数（3f 的 64 MiB 行） |
| `strata-apfs` | `open_root`、`open_rooted_file`、`attrs_of`、`volume_caps` | 读工作区不跟随符号链接。不在工作区、staging、基线 generation 树上自己调 `*at`（增补 8） |
| `strata-store` | 不调用 | 会话与清单经 session 读。search 不跑 SQL |
| `strata-gitio` | 不调用 | 增补 4：strata 自己的模块里只有 gitio 孵化 git。tgrep-core 自己孵化 git，见 §5.2，不经 gitio 的环境 |
| `tgrep-core`（d25ade6） | `Repository::discover`、`GenerationManager::with_storage`、`GenerationManager::ensure`、`Generation::base` / `entries` / `entry`、`TrackedEntry::matches_worktree_bytes`、`SharedBase::reader`、`IndexReader`、`query::build_query_plan`、`HybridIndex` 的查询与 `live` | 基线索引与三元组查询。引用 `SHARED_WORKTREE_INDEXES.md` 上面点名的节，不抄实现 |
| `globset` 0.4.18（tgrep-core 的 Cargo.toml 在 d25ade6 钉的版本，不另选主版本） | `GlobBuilder` | `files` 的 glob。不用 tgrep 的 `filetypes` 模块，那个是 `--type`，不保留 |

谁调用本模块：daemon。cli 不链接 search 的查询实现，它把 `--session` / `--all` 交给 daemon，daemon 再调本节的方法。cli 拿到空 `Vec` 就输出空列表，不得改成 `NoMatch`（types.md：`grep` 零命中从不返回 `NoMatch`）。这是给 T14 的上游要求。

`Search` 不拥有扫描。daemon 持有 overlay 的 `Tracker`。search 定义一个只读口，daemon 实现它 [我的决定 D7]：

```rust
use strata_overlay::Overlay;
use strata_types::{RepoName, SessionId};

pub trait OverlaySource: Send + Sync {
    /// `Tracker::last`。还没有扫过就是 None。
    fn last(&self, session: &SessionId, repo: &RepoName) -> Option<Overlay>;
    /// `Tracker::needs_full`。提示溢出或监听丢失时 daemon 已经要求全扫。
    fn needs_full(&self, session: &SessionId, repo: &RepoName) -> bool;
}
```

上游要求（本文不实现，点名给主人）：

1. **tgrep-core**：把 `HybridIndex::from_reader(Arc<IndexReader>, &Path)` 从 `pub(crate)` 改成 `pub`，或另给一个等价的 `from_shared_reader`。今天 `from_reader` 是 `pub(crate)`（`hybrid.rs`，d25ade6），crate 外造不出一个绑在共享 `Arc<IndexReader>` 上的 `HybridIndex`。`HybridIndex::open` 会再映射一份，对不上 §7 `base_index_shared_across_sessions_same_tree` 的 `Arc::ptr_eq`。在这个构造函数公开之前，`crates/strata-search` 编不过已发布的 tgrep-core。不要复制 `HybridIndex`。若上游不能改可见性，按 design.md §5 的「不可行则 vendor 所需模块」处理，仍不在 search 里重写合并。v1 计划按公开构造函数写。本任务不改 tgrep。
2. **T13 daemon**：实现 `OverlaySource`；`Which::All` 之前把要查的 Active 会话先 `attach`（search 的 `All` 只查已挂视图，不在查询里附带挂上）；launchd 没有用户 PATH，daemon 在第一次 `attach` 之前把进程 PATH 设成它文档里的显式 PATH，并且这条 PATH 上要找得到 `git`（tgrep 自己 `Command::new("git")`，search 改不了那条子进程的环境，见 §5.2）；长时间停在 `ensure` 上的 attach 由 daemon 的 status 报「正在建索引」，search 不提供 status。
3. **store**：`<root>/search/` 不在 store.md §4.1 的布局里，gc 的 1 小时下限也不覆盖根下不认识的一级目录，所以 gc 不会收走它。索引按 D8 一直留着，不挪到 `bases/` 下。第一次 `Store::open` 时，`<root>` 里只要有 `store`、`bases`、`deps`、`sessions` 以外的名字就返回 `NotEmpty`（store.md §5.1 第 3 步）。因此 `<root>/search/` 只能在这次 open 成功、`store.db` 已经在之后再建。`store.db` 在了之后再 open，容忍 `search/`，也从不收集它。

## 3. 公开接口（Rust）

跨模块类型全部来自 types.md。本节声明的模块内部名：`Search`、`OverlaySource`、`Which`、`GrepOpts`、`Hit`、`SearchSeam`。`ViewId` 不是这里定义的。

```rust
// crates/strata-search/src/lib.rs —— 签名清单（不是实现）
use std::path::PathBuf;

use strata_session::Sessions;
use strata_types::{
    Config, RepoName, RepoPath, SessionId, StrataError, TreeOid, ViewId,
};

/// types.md §8 的 `Result<T>`。
pub type Result<T> = std::result::Result<T, StrataError>;

/// daemon 持有的检索句柄。不缓存会话状态，每次 attach / refresh 都读 `Sessions::status` 与 `OverlaySource`。
/// 进程内记三样：每个 repo 一个 `GenerationManager`（存在 `<root>/search/<repo>/<仓库身份>/`）、
/// 已挂视图、测试接缝。它不是数据，不 derive serde。
pub struct Search<'a> { /* 私有 */ }

impl<'a> Search<'a> {
    /// 不做 I/O，不挂视图。
    pub fn open(
        config: &'a Config,
        sessions: &'a Sessions<'_>,
        overlays: &'a dyn OverlaySource,
    ) -> Search<'a>;

    /// 给这个 (会话, repo) 挂上视图。成功才发 `ViewId`。失败不留下 id。
    /// 码：`NoSuchSession`、`NoSuchRepo`、`NoSuchPath`、`NotDirectory`、`Busy`、`Unknown`、
    /// `Unsupported`、`NoSuchCommit`、`InvalidInput`。见 §5.1、§6。
    pub fn attach(&self, session: &SessionId, repo: &RepoName) -> Result<ViewId>;

    /// 卸下视图。不影响共享基线索引。未知 id，包括已经卸下的，返回 `NoSuchView`。
    pub fn detach(&self, view: ViewId) -> Result<()>;

    /// 按 `changed` 更新该视图的私有索引。未知 id → `NoSuchView`。
    /// 空切片不是「让 tgrep 自己猜变了哪些文件」：本模块不调用 `WorktreeView`。
    /// 视图就绪且 `needs_full` 为假时，空切片只复核根身份与 overlay 的 base。
    /// 视图未就绪时，下一次 refresh 一律重做 §5.4，哪怕 `changed` 是空的、`needs_full` 为假。见 §5.3。
    /// 码：`NoSuchView`、`Busy`、`Unknown`、`NotText`。
    pub fn refresh(&self, view: ViewId, changed: &[RepoPath]) -> Result<()>;

    /// 查一个模式。零命中是 `Ok(vec![])`，不是错误，也不是 `NoMatch`。
    /// 码：`NoSuchView`、`InvalidInput`、`Busy`、`Unknown`。
    pub fn grep(&self, which: Which, pattern: &str, opts: &GrepOpts) -> Result<Vec<Hit>>;

    /// 按 glob 列文件名成员。匹配不到是 `Ok(vec![])`，不是 `NoSuchPath`。
    /// 码：`NoSuchView`、`InvalidInput`、`Busy`、`Unknown`。
    pub fn files(&self, view: ViewId, glob: &str) -> Result<Vec<RepoPath>>;
}

/// `One` 只查这个视图。`All` 只查已经挂上的视图，按 `SessionId` 再按 `RepoName` 的顺序。
/// 没有挂上的会话不出现，也不算错 [我的决定 D9]。
pub enum Which {
    One(ViewId),
    All,
}

/// 只有大小写这一项。不收 tgrep CLI 的其余标志 [我的决定 D1]。
/// `Default`：`case_insensitive = false`。路径的大小写不归这个结构，见 §4.4。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GrepOpts {
    pub case_insensitive: bool,
}

/// 一条命中。`Which::One` 也填 `session` 与 `repo`，不只是 `All`。
/// `line` 从 1 起。`text` 是命中的那一整行，去掉末尾的 `\n` 或 `\r\n`，不含列号。
/// 同一行只出一条 [我的决定 D13]。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub session: SessionId,
    pub repo: RepoName,
    pub path: RepoPath,
    pub line: u32,
    pub text: String,
}

/// 只在 `test-seams` feature 后编译，不进发布二进制（增补 4）。
#[cfg(feature = "test-seams")]
pub enum SearchSeam {
    /// `grep` 在捉住 reader 快照之后、读任何工作区文件之前停住，直到 `release`。
    PauseBeforeVerify,
    /// 下一次 `ensure` 在 `.stage-*` 出现之后、发布改名之前失败。一次有效。
    /// §7 `ensure_failure_after_stage_is_unknown` 用它。
    FailEnsureAfterStage,
}

#[cfg(feature = "test-seams")]
impl<'a> Search<'a> {
    /// 这个 (repo, tree) 的 `Arc<Generation>` 的指针。相等即 `Arc::ptr_eq`。没有建过就是 None。
    pub fn generation_ptr(&self, repo: &RepoName, tree: &TreeOid) -> Option<usize>;
    /// `<root>/search/<repo>/<仓库身份>/`。测试用来断言索引不在主 checkout 的 `.git` 里。
    pub fn storage_dir(&self, repo: &RepoName) -> Option<PathBuf>;
    pub fn arm(&self, seam: SearchSeam);
    pub fn release(&self);
    /// 停在 `PauseBeforeVerify` 的那次查询捉住的 reader 指针。没有这样的查询就是 None。
    /// 相等即与 `generation_ptr` 同一把 `Arc`。§7 `query_keeps_reader_snapshot_across_base_swap` 用它。
    pub fn paused_reader_ptr(&self) -> Option<usize>;
    /// 把这个视图当前装着的 reader 换成同一 repo 里已打开的另一份 generation。
    /// 已经捉住的查询快照不变。未知视图 → `NoSuchView`。那份 tree 还没打开 → `InvalidInput`（`reason = "no_generation"`）。
    /// §7 `query_keeps_reader_snapshot_across_base_swap` 用它换成另一份 generation。
    pub fn swap_view_reader(&self, view: ViewId, tree: &TreeOid) -> Result<()>;
}
```

`ViewId` 的字符串格式由 §4.3 规定。调用 `ViewId::new`，不用别的构造。daemon 与 cli 只把 `get()` 的字符串原样传回来。

`detail` 是一个 JSON 对象，至少有 `reason` 字符串。本文用到的 `reason`：`not_active`、`overlay_not_ready`、`root_changed`、`base_moved`、`not_ready`、`index_unreadable`、`index_build`、`pattern`、`glob`、`git_missing`、`head_tree_mismatch`、`index_storage`、`sha256`。不另造码。

## 4. 数据与不变量

### 4.1 基线索引放哪儿、键是什么

键是（主 checkout 的仓库身份，tree OID，profile），不是（会话影子仓库的 common dir，tree）。增补 2：影子仓库每个会话一份，tgrep 的仓库身份是 common dir 的哈希，按影子仓库键会让同一 tree 建出多份。禁止用工作区的 common dir 当键。

仓库身份取主 checkout：`Repository::discover(RepoConfig.path)`。`RepoConfig.path` 是配置里的主 checkout，session 已经要求它等于 realpath。两个 `RepoName` 不会共用一个 common dir（`NameTaken` 在加载配置时拒绝），所以（`RepoName`，tree，profile）与（主 checkout 身份，tree，profile）一一对应。

存储用 `GenerationManager::with_storage`，目录是 `<paths.root>/search/<repo>/`。tgrep 再在下面建 `<仓库身份>/`。`publication.lock` 在 `<paths.root>/search/<repo>/<仓库身份>/publication.lock`。这在 `.strata/` 下，不在主 checkout 的 `.git` 里。`with_storage` 拒绝落在 common dir 之内的存储，所以这条路径也过不了 `GenerationManager::new`。不变量 2、5。

`<paths.root>` 就是 config.md 的 `[paths] root`，默认 `/Volumes/GWD/inter/.strata`。不写到 `~/Library/Application Support/git-strata/`。

profile [我的决定 D3]：`IndexingProfile::default()`，但 `max_blob_bytes = Some(overlay::MERGE_MAX_BYTES)`。今天两边都是 64 MiB（design §7），写死成 overlay 的常量，避免以后各改各的。v1 不读配置文件里的阈值，config.md 没有这个字段。内容语义仍是 `RawGitBlobAutoV1`，覆盖仍是 `TrackedRegularFilesV1`：基线索引的是 git blob，不是 checkout 过滤之后的字节（`SHARED_WORKTREE_INDEXES.md` "Implemented generation API"）。ignore 规则、隐藏文件、二进制扩展名不从基线成员里拿掉。超限文件不进索引，仍可以出现在 `files` 里。

同一 repo、同一 tree、同一 profile 的两个会话共用一份 `Arc<Generation>`，因而共用一份 `Arc<IndexReader>`。不同 repo 即使 tree OID 相同也不共用（身份不同）。profile 不同（阈值不同）也不共用。commit 不同、tree 相同，共用；generation 上记下的 commit 只是第一个建它的那个，不参与键。

### 4.2 视图

一个视图是进程内的：`ViewId`、`SessionId`、`RepoName`、挂上时的 `BaseGen`、`CommitOid`（`WorkspaceState.head`）、工作区根的 `(st_dev, st_ino)`、一份 `HybridIndex`。`HybridIndex` 的 reader 是 `generation.base().reader()` 那个 `Arc`，不是再 `open` 一次。`live` 只放本会话的差异。

视图不落盘。不写 tgrep 的 `overlay.json`，不调用 `save_checkpoint` / `restore`。进程退出，视图全部卸下；共享的 generation 还在目录里，下次 `ensure` 复用。重启之后必须重新 `attach`，重新对账，不得用基线单独作答。

`tracked = false` 的仓库可以挂 [我的决定 D11]。它没有 overlay，`OverlaySource::last` 为 None 时当成空 overlay，不返回 `overlay_not_ready`。`tracked = true` 而 `last` 为 None：还没有扫过，`attach` 返回 `Busy`（`overlay_not_ready`），不发 id。daemon 等第一次扫描后再挂。

### 4.3 `ViewId`

`v-` 加 16 位小写十六进制，64 bit，`arc4random_buf`（取不到就用 `getrandom`）。不从 session 或 repo 推导，所以卸下再挂是新 id。碰撞就再取一次。只经 `ViewId::new` 构造。

### 4.4 成员与遮蔽

一个视图的文件名成员：

- 基线 `Generation::entries` 里的普通文件与符号链接，去掉被本会话墓碑遮住的；
- 加上本会话 overlay 的 `Added`、`Modified`、`Untracked`、`Renamed` 的 `path`；
- 减去排除集合。

排除前缀，连同其下的一切。按卷折叠判断「落不落在前缀之下」，与 overlay.md §5.1 那张表里「落不落在 X 之下」同一行：`volume_caps(workspace).case_sensitive` 为假时用 `apfs` 的折叠键，为真时按字节。search 不自己实现折叠，也不再算一遍 overlay 的 X。

1. 根下的 `.git`（工作区里它是 gitfile，不是目录）；
2. `config.deps_dirs` 与 `config.seed_caches` 的每一项；
3. `.strata/`（裁定：视图排除 strata 自己的 `.strata/` 树）。`.strata/lww` 即使被跟踪也不出现。

种子文件不用另列。成员只从基线 `entries` 和 overlay 条目来，不扫工作区目录。种子文件未被跟踪，overlay 的 X 不让它进 overlay，所以它不在成员里，不会出现。被跟踪的同名文件在基线 `entries` 里，照常出现。会话 `git add -f` 了一个种子候选时，overlay 会记成 `Added`，search 跟着这条出现，不自己再藏一次。

已跟踪但被 `.gitignore` 命中的文件仍在成员里，grep 也查得到。这是 git-strata 的规则（design §3，gitio 的「已跟踪永不 ignore」），不沿用 tgrep 工作树视图随后再按 `.gitignore` 藏起来的那一步。未跟踪且被 ignore 的文件不在 overlay 里，也不在基线里，所以不会出现。不用 ripgrep 的 `.ignore`。

遮蔽，与 `SHARED_WORKTREE_INDEXES.md` "Search flow" 和 overlay 的记法相同：

- overlay 条目盖住基线同路径，即使新内容不再匹配查询；
- `Deleted` 的 path 与 `Renamed` 的 `from` 是墓碑，只在这个会话里藏起基线文件；
- 改名是旧路径墓碑加新路径条目，不另记一条 `Deleted`；
- 改回与基线相同（overlay 里没有这条）就露出基线；
- 对基线里本就不存在的路径记墓碑，不沿用：未跟踪文件被删只是从成员里消失；
- `Untracked` 与 `Added` 一样可被检索（增补 1）。

路径拼写：命中用 overlay 条目的 path（磁盘上的确切拼写）。只改大小写的改名按 overlay 已经配对好的 `Renamed` 处理，search 不再配一次。

符号链接不跟随，不把链接目标当文件内容索引。gitlink 不跟随。模式 160000 的仓库在 `export_tree` 就被 gitio 拒绝，正常会话到不了这里；`ensure` 若在跟踪成员里看到 gitlink，不索引其内容。

字面 `\` 是普通字符（overlay.md §5.1，`RepoPath` 的规则）。`a\b.txt` 与 `a/b.txt` 是两个路径。不采用 tgrep 对字面反斜杠 fail-closed、视图无法就绪的做法（§4.6）。

### 4.5 查询快照

一次 `grep` 或 `files` 从开始到返回，只用它开始时捉住的那份快照：`Arc<IndexReader>`、当时的 `live` 副本、当时的根 `(st_dev, st_ino)`、当时的 `BaseGen`。并发的 `move_workspace` 换基线，不得改掉这一次的解析。文件号只用这次捉住的 reader 解，不用查询中途换上的新 reader（`file_path(id)` 取当前 reader 的那种接口不用）。

工作区字节的复核走 `open_rooted_file`，它按路径重新解析。增补 8 不允许 search 自己 `openat`。所以一次查询在读完之后再 `open_root`，比对 `(st_dev, st_ino)`。变了就丢掉全部命中，返回 `Busy`（`root_changed`），不返回半份，也不返回新目录里的内容 [我的决定 D6]。下一次查询要先重新 `attach`，新视图钉新的 `BaseGen`。旧 `ViewId` 在根被换掉之后拒绝服务，不得对新目录里的同名文件作答。

这是增补 1「查询全程用自己的快照，并发换基线不改变这一次的结果；下一次查询看到新基线」在现有 apfs 接口下的读法：这一次不会返回新结果；下一次指重新挂上之后的查询。若要让进行中的查询返回旧目录的内容命中，apfs 得先有「相对已持有的根句柄打开」的函数。v1 不要求那个函数。

`Which::All` 里只要有一个视图的根对不上，整次调用失败，不返回少了一个会话的列表 [我的决定 D12]。

### 4.6 3f 行的状态变化

机制改成「不对工作区调用 `WorktreeView`」之后，下面这些原先 `依赖承担` 的行，search 走不到那些入口。状态按这里写的改。其余 `依赖承担` 行（`ensure`、`SharedBase`、`IndexReader`、`build_query_plan`、`matches_worktree_bytes`）不变，§7 仍引用。

改成 **unused-API**（库的测试还在，search 不调用；需要的安全性质改由本文的测试或 apfs 承担）：

- `tgrep-core/src/query.rs::multi_pattern_plan_unions_every_pattern`
- `tgrep-core/src/query.rs::multi_pattern_plan_collapses_to_single_branch`
- `tgrep-core/src/query.rs::multi_pattern_plan_absorbs_unindexable_patterns`
- `tgrep-core/src/query.rs::multi_pattern_plan_supports_fixed_strings`

  增补 3：这四行只在 search 建多模式计划时才继续 `依赖承担`。v1 的 `grep` 只有一个模式，只调用 `query::build_query_plan`。不调用 `build_multi_pattern_plan`，也不调用 `build_relaxed_multi_pattern_plan`。所以这四行是 unused-API。

- `tgrep-core/src/rooted/tests.rs::owned_query_batch_retains_root_until_verified_finish`
- `tgrep-core/src/rooted/tests.rs::query_batch_finish_rejects_a_replaced_parent_without_following_it`
- `tgrep-core/src/rooted/tests.rs::streaming_file_open_does_not_reuse_a_replaced_parent_handle`
- `tgrep-core/src/rooted/tests.rs::sibling_batches_release_pins_and_recheck_ancestor_identity`
- `tgrep-core/src/rooted/tests.rs::ancestor_observations_are_point_in_time_not_lifetime_pins`
- `tgrep-core/src/rooted/tests.rs::completed_batch_never_accepts_a_reparse_replacement`
- `tgrep-core/src/walker.rs::walk_file_metadata_includes_nonignored_ignore_sources`
- `tgrep-core/src/worktrees/tests.rs::actual_bytes_not_discovery_size_decide_size_and_binary_classification`
- `tgrep-core/src/worktrees/tests.rs::snapshot_sibling_batches_reject_foreign_roots_and_record_open_failures`
- `tgrep-core/src/worktrees/tests.rs::snapshot_candidate_opens_reject_directory_link_swaps`
- `tgrep-core/src/worktrees/tests.rs::snapshot_open_failures_require_full_repair_even_if_the_callback_ignores_them`
- `tgrep-core/src/worktrees/tests.rs::snapshot_fifo_swaps_invalidate_readiness_without_blocking`
- `tgrep-core/src/worktrees/tests.rs::directory_link_swap_after_discovery_keeps_the_view_not_ready`
- `tgrep-core/src/worktrees/tests.rs::observed_parent_replacement_after_batch_drain_blocks_publication`
- `tgrep-core/src/worktrees/tests.rs::fifo_swap_after_discovery_is_bounded_and_keeps_the_view_not_ready`
- `tgrep-core/src/worktrees/tests.rs::registration_rejects_other_repositories_subdirectories_and_immutable_checkpoint_paths`
- `tgrep-core/src/worktrees/tests.rs::no_ignore_still_excludes_private_storage_and_larger_view_can_overlay_capped_base`
- `tgrep-core/tests/shared_worktrees.rs::worktree_root_must_exist_and_be_a_directory`

  根必须存在且是目录，改由 `attach` 自己检查（`open_root`），不经 `create_worktree`。符号链接与 FIFO 的不跟随、非阻塞，用 `open_rooted_file`（`O_NOFOLLOW`，FIFO 非阻塞），不经 `WorktreeSnapshot`。

改成 **不保留**：

- `tgrep-core/src/walker.rs::hidden_visibility_matches_walks_with_ignore_whitelists_and_scoped_roots`：这条把 `.ignore` 算进成员规则。裁定不用 `.ignore`。隐藏文件是否出现，只看它在不在 §4.4 的成员里。
- `tgrep-core/src/worktrees/tests.rs::literal_backslash_checkout_paths_cannot_alias_directory_paths`：fail-closed 不沿用。`\` 是普通字符，视图照常就绪。

不改状态、仍然 `依赖承担` 的一条要写明，免得被看成遗漏：`tgrep-core/src/worktrees/tests.rs::checkout_decoding_can_reuse_base_despite_different_raw_bytes`。search 调用 `TrackedEntry::matches_worktree_bytes`，不调用 `WorktreeView`。比较语义由库的测试覆盖。

`tgrep-core/tests/generations.rs::linked_worktrees_and_equal_trees_share_one_arc` 仍是目标模块 search 的改写行，但原意里「从会话目录发现的是同一个仓库」不沿用。会话目录发现到的是影子仓库。search 只对主 checkout 调用 `discover`。

### 4.7 不变量

| 不变量 | 在本模块 |
| --- | --- |
| 2 对主 checkout 只读 | 索引在 `<root>/search/`。git 调用见 §5.2，只读 plumbing，`--git-dir` 指向主 checkout 的 common dir。不调用 `GenerationManager::new` |
| 5 generation 不原地改 | 基线索引同样不原地改。`ensure` 对已发布的键只打开，不覆盖。损坏的已发布目录不修复、不替换 |
| 6 不确定的失败留现场 | `ensure` 已经建出 `.stage-*` 之后失败：返回 `Unknown`，暂存目录留着，tgrep 本来也不自动删。不发 `ViewId` |
| 1、4、8 | search 不写工作区、不换依赖、不移动基线。换基线是 `move_workspace`，search 只观察它的结果 |

错误时机按增补 3：工作区、gitfile、Store 行、refs、主 checkout、主对象库才算对外可见。`<root>/search/` 里的索引不在这张表里。所以 `ensure` 建成之后仍可以返回具名拒绝。建成的 generation 的 tree 与 `base.tree` 不符是已知的不符，返回 `InvalidInput`（`head_tree_mismatch`），不是 `Unknown`：结果是确定的，generation 留着，不发 `ViewId`。已经开始、结果不确定的构建（`.stage-*` 出现之后失败）才用 `Unknown`，不删暂存。

## 5. 流程与算法

### 5.1 `attach`

1. `sessions.status(session)`。错误原样透传。
2. `state` 不是 `Active`：`Removed` → `NoSuchSession`；`Creating` 或 `Removing` → `Busy`（`not_active`）；`Unknown` → `Unknown`，`detail` 带 status 里的 scene，search 什么都不写。
3. `repo` 不在 `manifest.repos` → `NoSuchRepo`。
4. 工作区 `health`：`Missing` → `NoSuchPath`；`Unknown` → `Unknown`，带工作区的 scene。`Ok` 才继续。
5. `tracked = true` 且 `overlays.last` 为 None → `Busy`（`overlay_not_ready`），不发 id。`tracked = false` 且 None → 空 overlay。
6. `last` 的 `base` 与 `WorkspaceState.base` 不同 → `Busy`（`base_moved`）。daemon 要等 overlay 对新基线冷扫完再挂。不在旧基线上答新目录。
7. `open_root(workspace)`。不存在 → `NoSuchPath`。不是目录 → `NotDirectory`（`open_root` 的码原样透传）。记下 `(st_dev, st_ino)`。路径用清单里的 `WorkspaceState.path`，不接受调用方另给的根。因此没有「独立克隆 / 裸仓库 / 子目录根」这种参数可拒；那些情况不会进入 API。
8. 这个 repo 还没有 manager：`Repository::discover(主 checkout)`，`with_storage(repo, <root>/search/<repo>)`。目录没有就建。`with_storage` 拒绝（存储落在某个已注册的 worktree 里，或落在索引快照里）→ `Unsupported`（`index_storage`），不发 id。`git` 找不到 → `Unsupported`（`git_missing`）。
9. `ensure(head 的 40 位十六进制, profile, 前驱)`。`head` 用 `WorkspaceState.head`，不用 tree OID：`ensure` 会做 `rev^{commit}`，tree 不是 commit。前驱：本进程里同一 repo、同一 profile、已经打开的另一份 generation，没有就 None。增量是否只抽新 blob，由库保证（§7 的 `incremental_reuses_masks_renames_and_copies_extracting_only_new_blobs`）。
10. 这一步会拿 tgrep 的 `publication.lock`（`generations/mod.rs` 的 `ensure_inner` 调用 `lock`，锁文件就是 §4.1 的那个）。search 不再加一把锁，也不给这个等待加超时 [我的决定 D5]。锁在第一次对外可见的工作区写之前，但 tgrep 的 `lock_exclusive` 本身不返回超时。一个 daemon 里所有线程共用这一把锁，不同 tree 的构建也串行。挂上第二个同 tree 的会话时，锁内只打开已发布的 generation，不重新抽取。
11. `ensure` 已经返回，generation 的 tree 不等于 `base.tree` → `InvalidInput`（`head_tree_mismatch`）。这是已知的不符，不是不确定。generation 留着（它仍是那个 tree 的合法索引），不发 id。不用 `Unknown`。
12. `ensure` 失败：还没有 `.stage-*` → 按 §6 的表返回具名码。已经有本次新建的 `.stage-*` → `Unknown`（`index_build`），目录留着，不发 id。
13. 用上游要求 1 的构造函数，把 `generation.base().reader()` 的 `Arc` 装进 `HybridIndex`，根设为工作区路径。按 §5.4 把 overlay 填进 `live`。这一步失败 → 不发 id，manager 与 generation 留着。
14. 填完之后视图才算就绪。发 `ViewId`。中途任何失败都不把 id 放进表。

第二个会话钉同一个 tree：第 9 步复用，`generation_ptr` 与第一个相等，不为 `.git` 再抽取（search 排除 `.git` 之后，干净文件的私有抽取次数是 0）。

验收：热 `grep`（视图已就绪、索引已在）目标 < 300 ms（design §8）。这是目标，第一次 spike 要实测，不把它写成 `ensure` 的上限。第一次为一个大 tree 建索引不在 300 ms 里。tgrep 对 linux 内核约 96K 文件的全文索引是 37 s 量级（design §8），那是建索引，不是热查询。

### 5.2 tgrep 孵化的 git

gitio 的净化环境到不了这些子进程。tgrep 的 `generations/git.rs` `command()` 自己做这些事，search 再设一遍进程环境也覆盖不了别的模块，而且会跟 daemon 的 PATH 抢 [我的决定 D2]：

- 去掉继承的全部 `GIT_*`；
- 参数带 `--no-replace-objects`；
- 设 `GIT_TERMINAL_PROMPT=0`、`GIT_NO_LAZY_FETCH=1`、`GIT_OPTIONAL_LOCKS=0`；
- stdin 是 null；
- 不设固定 PATH。`git` 来自 daemon 已经设好的进程 PATH。

`attach` 走到建索引时，tgrep 实际调用的只有这些，cwd 或 `--git-dir` 都是主 checkout，不是影子仓库，不是工作区：

| 调用 | 命令 | 何时 |
| --- | --- | --- |
| `Repository::discover` | `git rev-parse --path-format=absolute --git-common-dir`，以及 `--absolute-git-dir` | 每个 repo 的第一次 attach |
| 同上 | `git rev-parse --show-object-format`，`--git-dir .` | 同上 |
| `with_storage` | `git worktree list --porcelain -z` | 同上，用来拒绝把存储放进已注册的 worktree |
| `ensure` | `git rev-parse --verify --end-of-options <rev>^{commit}`，再 `<commit>^{tree}` | 每次缓存未命中，以及打开前的解析 |
| 构建 | `git ls-tree -r -z -l --full-tree <tree>` | 只在要发布新 generation 时 |
| 构建 | `git cat-file --batch` | 同上，按批读 blob |

这些都是只读 plumbing。`rev-parse`、`ls-tree`、`cat-file`、`worktree list` 不跑 `pre-push`。基线索引读的是 raw blob，仓库里的 smudge / `core.autocrlf` 不改索引字节；工作区字节与 blob 不一致时由 §5.4 第 4 步盖上 overlay，不靠 git 的 checkout 过滤。可以接受的理由是：命令只读、环境由 tgrep 自己去掉 `GIT_*` 并关掉 replace 与 lazy fetch、目标只是主 checkout 的对象库。残留：用户 `~/.gitconfig` 里的 alias 仍可能截走子命令。search 包不住 tgrep 内部的 `Command`，v1 接受这个残留 [我的决定 D2]。

不调用 `git status`、`git diff`、`git commit`、`git push`。成员差异来自 overlay，不来自 tgrep 对工作区的 git status。

### 5.3 `refresh`

1. 未知 id → `NoSuchView`。
2. 再读 status 与 `open_root`。根的 `(st_dev, st_ino)` 变了，或 `WorkspaceState.base` 与视图钉住的不同 → 视图标成必须重新挂上，返回 `Busy`（`root_changed` 或 `base_moved`）。不把这一次的对账发布出去。对应「对账发布前根被换走：不发布，视图保持未就绪」。
3. `overlays.last` 的 `base` 与视图的 `BaseGen` 不同 → 同样 `Busy`（`base_moved`），不发布。
4. 视图未就绪：不论 `changed` 是不是空的、`needs_full` 是不是假，都重做 §5.4。这是未就绪之后的下一次 `refresh`，与 §6 的「下一次 refresh 重做 §5.4」是同一句。
5. 视图就绪，且 `changed` 非空：对每个路径取 `overlay.paired`，整组都重读。视图就绪、空切片、且 `needs_full` 为假：只做了第 2、3 步就返回。视图就绪、空切片、且 `needs_full` 为真：对全部成员重做 §5.4。
6. 读失败、ignore 规则一时算不出、文件在读的过程中变了（`attrs.unchanged_since` 为假且重读一次仍变）：不发布，视图标成未就绪，返回 `Unknown`（`not_ready`）。不得用基线或旧 overlay 作答。下一次 `refresh` 按第 4 步重做 §5.4，哪怕这次调用的 `changed` 是空的、`needs_full` 为假。成功发布之后才重新就绪。
7. 发布时把新的 `live` 与新的根身份一起换上。换不上（第 2 步的身份在发布前又变了）就不发布。

初稿写「空切片等于没有提示，tgrep 会核对它认为变了的文件」。那是 `WorktreeView::refresh` 的契约。本模块不调用它。就绪视图的空切片按第 5 步，只复核身份，不让 tgrep 自己猜。未就绪视图的下一次 `refresh` 按第 4 步重做 §5.4，不因为切片是空的就跳过。溢出或说不准（`needs_full`）仍然全量复核，不得静默用基线作答。

### 5.4 把 overlay 填进 `live`

从共享 reader 出发，按这个顺序。只动 `live`，不动基线文件。

1. 每个墓碑（`Deleted` 的 path，`Renamed` 的 `from`）：`live.delete_file`。
2. 每个 `Added`、`Modified`、`Untracked`、`Renamed` 的 path：mode 是 0o100644 或 0o100755 时才经 `open_rooted_file` 读工作区字节（上限 `MERGE_MAX_BYTES + 1`），再 `live.upsert_file`。mode 0o120000 的符号链接不打开、不读目标，只留在文件名成员里。打开得到 `NoSuchPath` 不是读失败，不让 `attach` 失败。不自己调用 `openat`。
3. 排除集合与 `.strata/` 之下的路径：即使基线索引里有，也 `delete_file`，`files` 与 `grep` 都看不到。
4. 只读 `hides` 为假的普通文件。`TrackedEntry::matches_worktree_bytes` 为真就留着基线倒排；为假就把读到的字节 upsert 进 `live`。这盖住 checkout 过滤（CRLF、smudge）与「git 状态干净但字节不同」。这个 upsert 只在 search 的 `live` 里，不写回 overlay，也不写 checkpoint。符号链接、gitlink、墓碑（`hides` 为真，包括 `Deleted` 的 path 与 `Renamed` 的 `from`；`from` 不是条目的 path，文件已经不在）只进成员，不读字节。这些路径上文件不在，不是读失败，不让 `attach` 失败。`hides` 为假的普通文件若打开得到 `NoSuchPath`，同样不是读失败，不 upsert，不让 `attach` 失败，基线倒排留着。不自己调用 `openat`。要读的普通文件经 `open_rooted_file`。
5. 超过 `MERGE_MAX_BYTES` 的普通文件：不读进索引，不 upsert。`files` 仍列出它（它在成员里）。`grep` 不从它出命中，也不因此让整次查询失败。

第 4 步的花费随要读的普通文件数增长。发生在第一次填视图、`needs_full` 的那次 refresh、以及未就绪之后的下一次 refresh（§5.3 第 4 步）。就绪视图之后的 refresh 只重读 `changed` 与 `paired` 里 `hides` 为假的普通文件，再用 `attrs_of` 的 `unchanged_since` 决定要不要重读内容。gen count 不可得（0）时 `unchanged_since` 为假，改读内容。与 overlay 的判变同一条规则，不另造。符号链接、gitlink、墓碑在这些路径上也不读。

短文件、空文件没有三元组也留在成员里。查询计划若走 MatchAll 或扫描，仍能命中。不因为没有倒排就把它们从 `files` 拿掉。

### 5.5 `grep`

1. `pattern` 为空，或 `query::build_query_plan(pattern, opts.case_insensitive)` 返回错误 → `InvalidInput`（`pattern`），不查询，也不看视图。只这一次调用。不调用 `build_multi_pattern_plan`、`build_literal_plan`、`build_relaxed_multi_pattern_plan`。模式是库的正则，不是固定字符串。想匹配字面点号的调用方自己转义。没有开关可以改这个行为。
2. `Which::One` 的 id 未知 → `NoSuchView`。`All` 没有任何已挂视图 → `Ok(vec![])`，不是错误。
3. 对每个目标视图捉住 §4.5 的快照。视图未就绪 → 先按 §5.3 做一次 `refresh`。这次 `changed` 是空的也重做 §5.4（§5.3 第 4 步），不看 `needs_full`。仍然未就绪 → `Unknown`（`not_ready`），不返回基线命中。
4. `HybridIndex::execute_query_with_masks` 拿候选文件号，再用这次的 reader `resolve_path`。不用 `file_path` 去问「当前」reader。
5. 候选再过 §4.4 的成员。排除集合里的路径丢掉。
6. 对每个候选 `open_rooted_file` 读字节，用同一个模式做全匹配，得到行。一行一条 `Hit`。行不是合法 UTF-8 → 丢掉这一行，查询继续 [我的决定 D13]。二进制、没有倒排、又不是 MatchAll 候选的文件不会进到这里。
7. 读完之后核对根身份。不符 → 丢掉这一视图的全部命中。`One` 返回 `Busy`（`root_changed`）并把视图标成必须重新挂上。`All` 整次 `Busy`，不返回其他会话的命中。
8. 排序：`session`、`repo`、`path`、`line`，都按它们的 `Ord`。相同输入顺序稳定。
9. 零命中 → `Ok(vec![])`。

热路径不持有 repo 写锁，也不在持有视图表锁时读文件。捉住快照之后放开表锁再读。refresh 可以同时更新别的视图。同一视图的发布与快照捉住互相排斥；已经捉住的快照不被后来的发布改掉。

### 5.6 `files`

1. 未知 id → `NoSuchView`。未就绪的处理与 `grep` 第 3 步相同。
2. `glob` 为空，或 `GlobBuilder` 拒绝 → `InvalidInput`（`glob`）。
3. 语法 [我的决定 D10]：`globset` 的 `GlobBuilder`。`*` 不跨 `/`。`**` 跨 `/`。`?` 是除 `/` 以外的一个字符。没有 `{a,b}`。`backslash_escape(false)`，`\` 是字面字符。卷不区分大小写时 `case_insensitive(true)`，否则按字节。这是卷的规则，不是 `GrepOpts`。globset 的大小写是 Unicode 折叠，与 `apfs` 的折叠键在个别字符上可能不同。v1 接受这个差别，不另写匹配器。
4. 对 §4.4 的成员过滤。不读文件内容，所以不因超限返回 `TooLarge`。
5. 核对根身份，规则同 `grep` 第 7 步。
6. 结果按 `RepoPath` 的 `Ord`。匹配不到 → `Ok(vec![])`。

### 5.7 `detach`

从表里去掉这个 id。再 `grep` / `files` / `refresh` / `detach` → `NoSuchView`。不关闭、不删除 generation。最后一个视图卸下之后，基线索引仍留在 `<root>/search/`，按 tree 给以后的会话用。进程退出等于全部 detach，索引目录还在。

### 5.8 锁

search 不拿 repo 写锁，不进会话队列，不等待它们。它不等 `publication.lock` 以外的锁；那把锁在 `ensure` 里面，见 §5.1 第 10 步。视图表一把 `Mutex`，每个视图一把 `Mutex`。不在持有这两把锁时调用 `ensure`（`ensure` 可能很久）。顺序：不持视图锁去等 manager 锁；建索引时不持视图锁。这样不会跟「会话队列 → repo 锁」的顺序交叉，也不会自己成环。

## 6. 失败与恢复

search 不写工作区。下面「写了什么」指 `<root>/search/` 与进程内的视图表。

| 情形 | 码 | 写了什么 | 之后 |
| --- | --- | --- | --- |
| 会话没有，或已 `Removed` | `NoSuchSession`，透传或按 §5.1 | 无 | 调用方改参数 |
| repo 不在清单里 | `NoSuchRepo` | 无 | 调用方改参数 |
| 会话在 `Creating` / `Removing` | `Busy`（`not_active`） | 无 | 等它 Active 再挂 |
| 会话或工作区已是 `Unknown` | `Unknown`，带原来的 scene | 无 | 人处理现场。search 不清理 |
| `tracked` 仓库还没有 overlay | `Busy`（`overlay_not_ready`） | 无 | daemon 扫描后再挂 |
| 工作区目录不在 | `NoSuchPath` | 无 | 不挂 |
| 工作区路径不是目录 | `NotDirectory`，透传 `open_root` | 无 | 不挂 |
| 路径名不是 UTF-8 | `NotText`，透传读目录的那一侧 | 无视图 id | 该名字进不了 `RepoPath` |
| `git` 不在 PATH 上 | `Unsupported`（`git_missing`） | 无 | daemon 修 PATH |
| 仓库是 SHA-256（`rev-parse --show-object-format`） | `Unsupported`（`sha256`） | 无 | gitio 在 `new` 时本就拒绝；这里是第二道 |
| 对象不在主 checkout 的对象库 | `NoSuchCommit` | 无 | 钉住的 commit 被 gc 掉了。不建空索引冒充 |
| `with_storage` 拒绝这条存储路径 | `Unsupported`（`index_storage`） | 可能有空的 `<root>/search/<repo>/` | 不把存储改到 `.git` 里。人看为什么主 checkout 的 worktree 列表盖住了这条路径 |
| `ensure` 在写出 `.stage-*` 之前失败 | 上表里对应的具名码 | 无 generation | 改原因后重试 attach |
| `ensure` 在 `.stage-*` 出现之后失败 | `Unknown`（`index_build`） | 暂存目录留着 | 不删、不重放。下次 attach 会再 `ensure`；已发布的键仍可打开。废弃的 `.stage-*` tgrep 不自动收 |
| `ensure` 已返回，tree 与 `base.tree` 不符 | `InvalidInput`（`head_tree_mismatch`） | generation 留着 | 已知的不符，不是不确定。不发 id。不用 `Unknown` |
| 未知或已卸下的 `ViewId` | `NoSuchView` | 无 | 重新 attach |
| 查询或 refresh 时根被换掉 | `Busy`（`root_changed`） | 无命中 | 旧 id 作废。重新 attach。进行中的查询不返回新目录的内容 |
| overlay 的 base 与视图钉住的不同 | `Busy`（`base_moved`） | 不发布对账 | 重新 attach |
| 模式或 glob 不合法，或模式为空 | `InvalidInput`（`pattern` 或 `glob`） | 无 | 调用方改参数 |
| 对账读失败，视图未就绪 | `Unknown`（`not_ready`） | 视图还在，未就绪 | 下一次 `refresh` 重做 §5.4，哪怕 `changed` 是空的、`needs_full` 为假（§5.3 第 4 步）。此前 grep 不得用基线单独作答 |
| 打开已发布索引失败 | `Unknown`（`index_unreadable`） | generation 目录留着，不修复 | 不返回空命中冒充成功。tgrep 对损坏的已发布目录不覆盖 |
| 零命中 | 成功，空 `Vec` | 无 | 不是 `NoMatch` |
| 文件超过 `MERGE_MAX_BYTES` | 成功 | 该文件不进索引 | `files` 仍列出。`grep` 不读它，不因此失败。types.md 把 `TooLarge` 标在 search 名下，指「按条目、不读内容」。v1 的公开方法不把整次调用失败成 `TooLarge`，因为 `GrepOpts` 不能再加路径参数，一次查询里夹一个超限文件也不该让其他命中失败 [我的决定 D4] |
| 点名的路径不在快照里 | 不单设入口 | 无 | v1 的 `grep` / `files` 没有路径参数（简报的签名，D1 也不许把路径塞进 `GrepOpts`）。types.md `NoSuchPath` 那句「查询命令限定的路径」v1 由 cli 在结果上过滤时处理；search 自己的 `NoSuchPath` 只用于工作区目录不存在。请 T15 把那半句的返回方改到 cli，若 cli 确实做了路径过滤。`Busy` 的「谁返回」今天只写 daemon（写锁）；search 为 `root_changed`、`base_moved`、`not_active`、`overlay_not_ready` 返回 `Busy`，请 T15 把 search 加进那一行。`InvalidInput`（模式、glob、`head_tree_mismatch`）与 `Unsupported`（找不到 git、SHA-256、存储路径被拒）也请 T15 把 search 加进「谁返回」；码是现成的，没有新码 |

索引读失败不是 `Corrupt`。`Corrupt` 是 store 的 blob 与 schema。search 不读 blob 存储。

恢复：没有视图级的回滚。未就绪就再 `refresh`。根被换掉就 `detach`（若 id 还在）再 `attach`。generation 从不删除，所以没有「删了一半」要修。daemon 重启之后视图不在了，索引目录还在，重新 attach 走 `ensure` 的打开路径，不重建已发布的 generation。

## 7. 测试清单

测试在临时目录里跑。fixture 建一个小的 git 仓库（一个提交、一个 tree），再 `clone --bare` 不够：要一个主 checkout，search 对它 `Repository::discover`。两个会话是两个目录，各自有 gitfile 指向自己的影子 common dir，影子的对象经 alternates 看到主 checkout。工作区里放基线导出的文件，再加上测试自己写的 overlay 文件。`OverlaySource` 用测试里的假实现，直接返回准备好的 `Overlay`，不启动 `Tracker`。tgrep 的索引建在测试自己的 `<root>/search/` 下。断言用 `generation_ptr` 的相等表示 `Arc::ptr_eq`，用数目录表示没有建第二份。不测墙钟，design §8 的 300 ms 是目标，不是这条清单的门。带 `test-seams` 的测试才看得到 `generation_ptr`、`PauseBeforeVerify`、`FailEnsureAfterStage`、`paused_reader_ptr`、`swap_view_reader`。§7 用到的接缝都在 §3 声明。

`来源` 列写矩阵的 `来源` 单元格，原文。一条测试覆盖多行时用 `<br>` 隔开。状态在断言里写明。3f 里索引格式、读取与查询、仍从 `ensure` / `SharedBase` / `IndexReader` / `build_query_plan` 走得到的行保持 `依赖承担`，不在本表重写成 search 的实现。tgrep 的 Windows、`.ignore`、托管 v2、`--type`、`-P` 保持不保留，不改挂到 search。

| 测试名 | 断言 | 来源（矩阵行 或 新增） |
| --- | --- | --- |
| `overlay_entry_shadows_base_path` | 基线 `a.ts` 含一行 `BASE_ONLY`。会话 overlay 把 `a.ts` 改成含 `SESSION_ONLY`。本视图 `grep` `BASE_ONLY` 得到空 `Vec`；`grep` `SESSION_ONLY` 得到一条，`path` 是 `a.ts`，`text` 含 `SESSION_ONLY`，`session` 与 `repo` 是这个视图的。另一个钉同一 tree 的会话看不到 `SESSION_ONLY`。新内容不再匹配时，基线命中也不出现 | tgrep-core/src/worktrees/tests.rs::divergent_commits_staging_renames_deletes_and_whole_file_overlays_are_isolated<br>tgrep-core/tests/snapshot_consistency.rs::direct_lookups_share_snapshot_overlay_precedence |
| `tombstone_hides_base_in_this_session_only` | 会话把基线文件 `old.rs` 记成 `Deleted`。本视图的 `files` 与 `grep` 都没有它。另一个钉同一 tree 的会话仍列出它，并能 grep 到基线内容。`Renamed` 的 `from` 同样只在本会话隐藏 | tgrep-core/src/worktrees/tests.rs::divergent_commits_staging_renames_deletes_and_whole_file_overlays_are_isolated<br>tgrep-core/tests/shared_worktrees.rs::worktrees_share_reader_but_not_changes_or_roots |
| `grep_all_tags_each_hit_with_session` | 两个已挂视图，各有一处互不相同的命中。`Which::All` 返回两条。每条的 `session` 与 `repo` 都填了，并且对得上那个视图。没有第三条。`Which::One` 只返回自己那条，但 `session` 与 `repo` 同样填了 | 新增 |
| `base_index_shared_across_sessions_same_tree` | 两个会话、同一 repo、同一 tree、head 可以是两个不同的 commit。两次 `attach` 之后 `generation_ptr` 相等。第二个 `ensure` 不发布新 generation：该 tree 的 `gen-*` 目录只有一个。干净文件不进私有 `live`。`.git` 不产生私有抽取。8 个并发 `attach` 仍是同一个指针 | tgrep-core/src/generations/mod.rs::concurrent_cold_loads_recheck_before_cache_insertion<br>tgrep-core/src/worktrees/tests.rs::identical_tracked_files_share_reader_without_extraction<br>tgrep-core/tests/generations.rs::linked_worktrees_and_equal_trees_share_one_arc<br>tgrep-core/tests/generations.rs::simultaneous_managers_share_publication_and_reader<br>tgrep-core/tests/shared_worktrees.rs::worktrees_share_reader_but_not_changes_or_roots<br>tgrep-core/tests/shared_worktrees.rs::many_worktrees_share_one_reader_while_querying_independent_overlays |
| `grep_zero_hits_is_empty` | 模式合法、视图里没有匹配 → `Ok(vec![])`，不是 `Err`，不是 `NoMatch`。`Which::All` 在零个已挂视图时同样是空 `Vec` | 新增 |
| `unknown_view_is_no_such_view` | 没挂过的 id，以及 `detach` 之后的 id，再 `grep`、`files`、`refresh`、`detach`，都是 `NoSuchView`。共享 generation 的目录还在 | 新增 |
| `other_session_publish_does_not_change_this_view` | 会话 B 连续 checkpoint。会话 A 钉住的 tree 不变。A 的 `grep` 与 `files` 每一轮都与 B 开始写之前相同。按 A 的 `ViewId` 查不到 B 的新文件。B 自己的视图查得到 | layerfs-api/tests/sdk_reads_under_publish.rs::reads_of_main_are_unchanged_while_another_branch_publishes |
| `gitfile_and_strata_dir_excluded` | 工作区有 `.git` gitfile，内容以 `gitdir:` 开头，还有 `.strata/lww`。`files` 与 `grep` 都没有这两条，也没有任何以 `.strata/` 开头的路径。依赖目录与种子缓存下的路径同样没有 | tgrep-core/src/worktrees/tests.rs::linked_gitfile_matches_hidden_scan_without_exposing_metadata_directories |
| `tracked_ignored_file_still_listed` | 基线里有 `git add -f` 的 `kept.txt`，工作区 `.gitignore` 命中它。它仍在 `files` 里，`grep` 能命中它的基线内容。未被跟踪、被 ignore 的 `skip.txt` 不在 `files` 里。不用 `.ignore`：放一个 `.ignore` 命中 `kept.txt`，结果不变 | tgrep-core/src/worktrees/tests.rs::membership_visibility_binary_size_and_storage_rules_follow_the_requesting_root |
| `base_excludes_dirty_and_untracked` | 基线 generation 的成员只有已提交的 blob。已暂存的新内容、工作区脏内容、未跟踪文件不在基线索引里，只出现在有对应 overlay 条目的那个视图。已提交的点文件、已跟踪但被 ignore 的文件、扩展名像二进制但内容是文本的文件在基线里 | tgrep-core/tests/generations.rs::builds_committed_blobs_not_staged_dirty_or_untracked_contents |
| `same_tree_oid_different_repos_do_not_share` | 两个 repo、同一个 tree OID。`generation_ptr` 不相等，存储目录不同，互相打不开对方的 generation。把 `max_blob_bytes` 改成 3 的 profile（只在这个测试里构造，v1 产品路径不用）得到另一个键；超过 3 字节的文件不进那份索引，仍在 overlay 的成员语义里可被 `files` 列出 | tgrep-core/tests/generations.rs::clone_tree_and_profile_keys_do_not_collide |
| `ensure_storage_is_under_strata_search_not_git_dir` | `storage_dir` 在 `<root>/search/<repo>/` 之下，不在主 checkout 的 `.git` 之下。`publication.lock` 在这个存储目录里。对工作区调用 `discover` 得到的身份与对主 checkout 调用得到的不同；search 不用前者当键。不调用 `GenerationManager::new` | tgrep-core/tests/generations.rs::linked_worktrees_and_equal_trees_share_one_arc |
| `oversized_file_listed_not_indexed` | 一个 `MERGE_MAX_BYTES + 1` 的普通文件在成员里。`files` 列出它。`grep` 一个只出现在这个文件里的字符串得到空 `Vec`，调用本身是 `Ok`，不是 `TooLarge`。`MERGE_MAX_BYTES - 1` 的文本文件能被 grep 命中。阈值与 `overlay::MERGE_MAX_BYTES` 是同一个数 | tgrep-core/src/walker.rs::walks_default_to_a_64_mib_size_cap |
| `not_ready_does_not_answer_from_base` | 对账读到一半文件被换成不可读。`refresh` 返回 `Unknown`（`not_ready`），视图未就绪。此后 `grep` 在自动重试一次仍失败时返回 `Unknown`，不返回基线命中。修复文件后再 `refresh` 成功，命中变成新内容 | tgrep-core/src/worktrees/tests.rs::read_and_discovery_errors_close_the_gate_and_force_full_retry |
| `root_replacement_refuses_old_view` | `attach` 之后把工作区目录 `rename` 走，在原路径放一个同名新目录，里面有同名文件但内容不同。对旧 `ViewId` 的 `grep`（包括匹配不到任何东西的模式）与 `files` 都返回 `Busy`（`root_changed`），不是新目录的内容，也不是空列表冒充成功。重新 `attach` 得到新 id，新查询只见新基线 | tgrep-core/src/worktrees/tests.rs::snapshots_reject_root_replacement_before_and_during_empty_queries |
| `refresh_during_root_swap_does_not_publish` | `refresh` 开始之后、发布之前换掉根。`refresh` 返回 `Busy`（`root_changed`），视图未就绪，随后的 `grep` 不返回换根之后的内容 | tgrep-core/src/worktrees/tests.rs::root_replacement_before_publication_keeps_the_view_not_ready |
| `query_keeps_reader_snapshot_across_base_swap` | `arm(PauseBeforeVerify)`。查询停住后 `paused_reader_ptr` 等于开始时的 `generation_ptr`。另一个线程 `swap_view_reader` 换成另一份已打开的 generation。`release` 之后，这一次解析文件号仍用捉住的那份（`paused_reader_ptr` 与开始时相等）。若根的 inode 也变了，整次返回 `Err`，码是 `Busy`（`root_changed`），不是带着命中的 `Ok`，也没有新树的行。不出现「用新 reader 解旧文件号」 | tgrep-core/tests/snapshot_consistency.rs::snapshot_survives_reader_swap<br>tgrep-core/tests/snapshot_consistency.rs::concurrent_swap_during_query |
| `new_query_after_reattach_sees_new_base` | 换基线并重新 `attach` 之后的新 `grep` 只命中新 tree 的文件，没有旧 tree 的残留。旧 `ViewId` 仍是 `NoSuchView` 或 `Busy`（`root_changed`），不是新结果 | tgrep-core/tests/snapshot_consistency.rs::new_query_after_swap_resolves_correctly |
| `candidate_count_matches_snapshot` | 同一路径替换不使成员数加一。新增加一。墓碑减一。对基线里不存在的路径 `delete_file` 不改变成员数。同一次查询里的成员数与它捉住的那份（基线 reader，live）一致，不混用换基线前后的两份 | tgrep-core/tests/snapshot_consistency.rs::candidate_totals_account_for_overlay_and_reader_snapshot |
| `rebuild_preserves_answers_for_empty_short_tombstone` | 卸下再挂、进程内重建 `live` 之后，对 `.`、短文件、空文件、墓碑的 `grep` / `files` 答案与重建前相同。不比较倒排掩码的内部字节。基线里本没有的路径不记墓碑：未跟踪文件删掉之后只是从成员消失 | tgrep-core/tests/shared_worktrees.rs::checkpoint_preserves_masks_tombstones_empty_files_and_unmasked_postings |
| `commit_inside_session_keeps_overlay_hit` | 工作区里对已改文件做了一次不改变「相对钉住基线」的提交。overlay 仍有这条。`grep` 仍命中会话内容，不是基线内容。把文件改回基线字节并从 overlay 拿掉之后，命中回到基线，私有覆盖消失 | tgrep-core/src/worktrees/tests.rs::divergent_commits_staging_renames_deletes_and_whole_file_overlays_are_isolated |
| `rename_is_tombstone_plus_new_path` | overlay 一条 `Renamed { from: old.ts, path: new.ts }`。`files` 有 `new.ts` 没有 `old.ts`。`grep` 命中新路径的内容，旧路径的基线内容不出现。`refresh` 只收到 `from` 或只收到 `path`，两条都更新（`paired`） | tgrep-core/src/worktrees/tests.rs::divergent_commits_staging_renames_deletes_and_whole_file_overlays_are_isolated |
| `literal_backslash_is_normal_path` | 成员里有 `a\b.txt`。视图就绪。`files` 列得出它，`grep` 命中它的内容。它不等于 `a/b.txt`。不因为字面反斜杠把视图标成未就绪。这条矩阵行的 fail-closed 不保留 | tgrep-core/src/worktrees/tests.rs::literal_backslash_checkout_paths_cannot_alias_directory_paths |
| `attach_missing_workspace_is_no_such_path` | 清单里的工作区路径不存在 → `NoSuchPath`，视图表里没有新 id。路径存在但是普通文件 → `NotDirectory`。不经 `create_worktree` | tgrep-core/tests/shared_worktrees.rs::worktree_root_must_exist_and_be_a_directory |
| `verify_does_not_follow_symlink` | 成员路径在打开前被换成指向工作区外的符号链接。`open_rooted_file` 不跟随。`grep` 不返回链接目标里的字节。查询不挂起（FIFO 换上时非阻塞打开，视图未就绪，不读管道） | tgrep-core/src/worktrees/tests.rs::snapshot_candidate_opens_reject_directory_link_swaps |
| `invalid_pattern_is_invalid_input` | 空模式、不平衡的 `(` → `InvalidInput`，`detail.reason = "pattern"`。不调用 `build_multi_pattern_plan` | 新增 |
| `files_glob_does_not_cross_slash` | glob `*.rs` 命中 `a.rs`，不命中 `src/a.rs`。`**/*.rs` 命中 `src/a.rs`。空 glob → `InvalidInput`（`glob`）。没有命中 → `Ok(vec![])` | 新增 |
| `ensure_failure_after_stage_is_unknown` | `arm(FailEnsureAfterStage)` 之后 `attach`：`ensure` 在 `.stage-*` 出现之后、发布之前失败。返回 `Unknown`（`index_build`），暂存目录还在，没有 `ViewId`。不装这个接缝、失败发生在任何 `.stage-*` 之前时返回具名码，没有 generation 目录 | 新增 |
| `attach_refuses_inactive_session` | `Creating` → `Busy`（`not_active`）。`Removed` → `NoSuchSession`。工作区 `Unknown` → `Unknown`，不发 id，不建视图 | 新增 |
| `which_all_fails_if_one_root_changes` | 两个已挂视图。查询中途只换掉其中一个的根。`Which::All` 返回 `Busy`，不是只含另一个会话的列表 | 新增 |
| `untracked_searches_like_added` | `Untracked` 与 `Added` 一样出现在 `files` 与 `grep` 里。两者的区别不在检索结果里 | 新增 |
| `rename_or_symlink_does_not_fail_attach` | overlay 有 `Renamed { from: old.rs, path: new.rs }`，`old.rs` 已不在。基线有一条 mode 0o120000 的 `link.txt`。`attach` 返回 `Ok`，发出 `ViewId`。`files` 有 `new.rs` 与 `link.txt`，没有 `old.rs`。不打开 `old.rs`，不读链接目标，不自己调用 `openat` | 新增 |

下面各行是矩阵归档。断言写的是处置，不是要在 `strata-search` 里重跑 tgrep 的测试。`来源` 仍是矩阵原文，供核对。

| 测试名 | 断言 | 来源（矩阵行 或 新增） |
| --- | --- | --- |
| `matrix_filetypes_not_adopted` | 不实现 `--type` / `--type-not` / `--type-add`。v1 没有这些参数。这些行保持不保留，不改挂到 search | tgrep-core/src/filetypes.rs::builtin_table_covers_ripgrep_type_count<br>tgrep-core/src/filetypes.rs::aliases_resolve_to_the_same_globs<br>tgrep-core/src/filetypes.rs::character_class_globs_match<br>tgrep-core/src/filetypes.rs::case_varied_makefile_globs_match<br>tgrep-core/src/filetypes.rs::negation_takes_precedence_over_selection<br>tgrep-core/src/filetypes.rs::negation_only_includes_everything_else<br>tgrep-core/src/filetypes.rs::multiple_selected_types_union<br>tgrep-core/src/filetypes.rs::empty_filter_matches_everything<br>tgrep-core/src/filetypes.rs::unknown_type_is_an_error<br>tgrep-core/src/filetypes.rs::type_add_defines_a_new_type<br>tgrep-core/src/filetypes.rs::type_add_appends_to_an_existing_type<br>tgrep-core/src/filetypes.rs::type_add_include_copies_other_types<br>tgrep-core/src/filetypes.rs::type_add_include_rejects_unknown_types<br>tgrep-core/src/filetypes.rs::type_add_rejects_malformed_specs<br>tgrep-core/src/filetypes.rs::type_clear_removes_a_definition<br>tgrep-core/src/filetypes.rs::later_selection_wins<br>tgrep-core/src/filetypes.rs::negate_then_select_of_overlapping_types_keeps_shared_globs<br>tgrep-core/src/filetypes.rs::definitions_apply_in_argument_order<br>tgrep-core/src/filetypes.rs::selections_resolve_against_final_definitions<br>tgrep-core/src/filetypes.rs::unordered_equivalence<br>tgrep-core/src/filetypes.rs::globs_do_not_cross_path_separators<br>tgrep-core/src/filetypes.rs::types_added_since_the_old_hardcoded_list |
| `matrix_cli_only_decoders_not_adopted` | 拥有式解码只有 tgrep-cli 调用。不保留 | tgrep-core/src/encoding.rs::decoding_owned_bytes_matches_decoding_borrowed_ones<br>tgrep-core/src/encoding.rs::owned_valid_utf8_reuses_the_buffer |
| `matrix_pcre_relaxed_plan_not_adopted` | 不暴露 `-P`，不调用 `build_relaxed_multi_pattern_plan`。不保留 | tgrep-core/src/query.rs::relaxed_multi_pattern_plan_degrades_instead_of_erroring<br>tgrep-core/src/query.rs::a_lookaround_only_pattern_falls_back_to_a_full_scan |
| `matrix_multi_pattern_unused` | 不调用 `build_multi_pattern_plan`。这四行从 `依赖承担` 改为 unused-API（§4.6） | tgrep-core/src/query.rs::multi_pattern_plan_unions_every_pattern<br>tgrep-core/src/query.rs::multi_pattern_plan_collapses_to_single_branch<br>tgrep-core/src/query.rs::multi_pattern_plan_absorbs_unindexable_patterns<br>tgrep-core/src/query.rs::multi_pattern_plan_supports_fixed_strings |
| `matrix_stale_and_provisional_not_adopted` | 不开启 `stale_reads`，不用 provisional base。未就绪时不回答。不保留 | tgrep-core/src/worktrees/tests.rs::provisional_base_serves_first_attach_before_full_content_verification<br>tgrep-core/src/worktrees/tests.rs::stale_reads_serve_published_state_and_keep_newer_hints_after_churn |
| `matrix_tgrep_checkpoint_file_not_adopted` | 不用 `save_checkpoint` / `restore`。重启后必须重新 attach。不保留 | tgrep-core/src/worktrees/tests.rs::restored_checkpoints_revalidate_contents_membership_and_exact_generation |
| `matrix_walk_dir_not_used` | 成员不走 `walk_dir`。不保留。`.ignore` 那条改为不保留，见 §4.6 | tgrep-core/src/walker.rs::walk_dir_retains_binary_extensions_for_file_listing<br>tgrep-core/src/walker.rs::walk_dir_collects_and_indexes_gitignore_when_hidden_included<br>tgrep-core/src/walker.rs::hidden_visibility_matches_walks_with_ignore_whitelists_and_scoped_roots |
| `matrix_legacy_profile_wire_not_adopted` | 没有旧版 tgrep 索引要兼容。v1 只用 §4.1 的 profile。不保留 | tgrep-core/src/generations/mod.rs::default_content_eligibility_keeps_the_legacy_profile_wire_shape |
| `matrix_sdk_paging_not_adopted` | 不做 SDK 的字节窗口分页。cli 一次输出 JSON。不保留。结果集合由上面的 grep/files 测试覆盖 | layerfs-api/tests/sdk_compare.rs::byte_limited_windows_preserve_diff_grep_and_glob_membership |
| `matrix_js_body_refusals_not_adopted` | 没有 JS 写入体。不保留。`NoSuchPath` 与 `InvalidInput` 两个码仍在 `Code` 里，search 按 §6 使用，不由这一行代表 | sdk/js/test/binary_write.mjs::refusals |
| `matrix_worktree_view_unused` | 不调用 `WorktreeView` / `WorktreeSnapshot` / `RootedDir` 批 API。这些行改为 unused-API（§4.6）。根身份与不跟随由 `root_replacement_refuses_old_view`、`verify_does_not_follow_symlink`、`attach_missing_workspace_is_no_such_path` 与 apfs 承担 | tgrep-core/src/rooted/tests.rs::owned_query_batch_retains_root_until_verified_finish<br>tgrep-core/src/rooted/tests.rs::query_batch_finish_rejects_a_replaced_parent_without_following_it<br>tgrep-core/src/rooted/tests.rs::streaming_file_open_does_not_reuse_a_replaced_parent_handle<br>tgrep-core/src/rooted/tests.rs::sibling_batches_release_pins_and_recheck_ancestor_identity<br>tgrep-core/src/rooted/tests.rs::ancestor_observations_are_point_in_time_not_lifetime_pins<br>tgrep-core/src/rooted/tests.rs::completed_batch_never_accepts_a_reparse_replacement<br>tgrep-core/src/walker.rs::walk_file_metadata_includes_nonignored_ignore_sources<br>tgrep-core/src/worktrees/tests.rs::actual_bytes_not_discovery_size_decide_size_and_binary_classification<br>tgrep-core/src/worktrees/tests.rs::snapshot_sibling_batches_reject_foreign_roots_and_record_open_failures<br>tgrep-core/src/worktrees/tests.rs::snapshot_open_failures_require_full_repair_even_if_the_callback_ignores_them<br>tgrep-core/src/worktrees/tests.rs::snapshot_fifo_swaps_invalidate_readiness_without_blocking<br>tgrep-core/src/worktrees/tests.rs::directory_link_swap_after_discovery_keeps_the_view_not_ready<br>tgrep-core/src/worktrees/tests.rs::observed_parent_replacement_after_batch_drain_blocks_publication<br>tgrep-core/src/worktrees/tests.rs::fifo_swap_after_discovery_is_bounded_and_keeps_the_view_not_ready<br>tgrep-core/src/worktrees/tests.rs::registration_rejects_other_repositories_subdirectories_and_immutable_checkpoint_paths<br>tgrep-core/src/worktrees/tests.rs::no_ignore_still_excludes_private_storage_and_larger_view_can_overlay_capped_base |
| `matrix_library_still_covers` | 这些入口 search 会调用到。保持 `依赖承担`，不在 strata-search 重写。空 tree 的 `ensure` 由库覆盖。大小写不敏感的模式走 `build_query_plan`。打开索引时拒绝不安全路径、损坏索引不 panic，由 `IndexReader` / `SharedBase` 覆盖。增量构建传前驱。查询返回的文件号能用同一次的快照解开 | tgrep-core/src/generations/mod.rs::parallel_extraction_builds_an_empty_tree<br>tgrep-core/src/worktrees/tests.rs::checkout_decoding_can_reuse_base_despite_different_raw_bytes<br>tgrep-core/tests/case_insensitive_roundtrip.rs::case_insensitive_search_roundtrip<br>tgrep-core/tests/corrupt_index_reader.rs::reader_rejects_unsafe_stored_paths_before_queries<br>tgrep-core/tests/corrupt_index_reader.rs::adversarial_lookup_entries_are_handled_without_panic_or_hang<br>tgrep-core/tests/generations.rs::incremental_reuses_masks_renames_and_copies_extracting_only_new_blobs<br>tgrep-core/tests/shared_worktrees.rs::shared_base_rejects_unaligned_overlapping_and_gapped_posting_ranges<br>tgrep-core/tests/shared_worktrees.rs::shared_base_accepts_empty_sections_for_empty_and_short_files<br>tgrep-core/tests/snapshot_consistency.rs::all_query_ids_resolve_via_snapshot<br>tgrep-core/tests/snapshot_consistency.rs::match_all_uses_consistent_snapshot |

## 8. 实现任务（TDD）

按这个顺序。每一步的测试都在 §7。先写失败的测试，再写让它通过的最少实现。crate 还不存在：任务 1 的第一步同时建 `crates/strata-search`，只放类型与会失败的测试，不放查询实现。不在本阶段改 workspace 的 `Cargo.toml` 以外的产品代码；把 `strata-search` 加进 workspace 是这个 crate 的一部分，不是提前做功能。

### Task 1: 视图表

**Files:**
- Create: `crates/strata-search/src/lib.rs`
- Create: `crates/strata-search/Cargo.toml`
- Test: `crates/strata-search/tests/views.rs`

**Interfaces:**
- Consumes: `ViewId::new`、`Sessions::status`、`OverlaySource`
- Produces: `Search::open`、`attach`、`detach`、`GrepOpts`、`Which`、`Hit`

- [ ] **Step 1: 写失败的测试**：`unknown_view_is_no_such_view`、`attach_refuses_inactive_session`、`attach_missing_workspace_is_no_such_path`、`invalid_pattern_is_invalid_input`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-search --test views`
Expected: FAIL（`attach` / `grep` 还没有）

- [ ] **Step 3: 写让测试通过的最少实现**：视图表与 §5.1 的拒绝。`grep` 在空索引上对合法模式返回空 `Vec`。不调用 `ensure`
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-search --test views`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-search
git commit -m "feat(search): view table and typed refusals"
```

### Task 2: 共享基线索引

**Files:**
- Create: `crates/strata-search/src/base.rs`
- Test: `crates/strata-search/tests/base.rs`

**Interfaces:**
- Consumes: `Repository::discover`、`GenerationManager::with_storage`、`GenerationManager::ensure`、`overlay::MERGE_MAX_BYTES`
- Produces: `generation_ptr`、`storage_dir`（`test-seams`）

- [ ] **Step 1: 写失败的测试**：`base_index_shared_across_sessions_same_tree`、`ensure_storage_is_under_strata_search_not_git_dir`、`same_tree_oid_different_repos_do_not_share`、`base_excludes_dirty_and_untracked`、`ensure_failure_after_stage_is_unknown`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-search --test base --features test-seams`
Expected: FAIL（还没有 manager）

- [ ] **Step 3: 写让测试通过的最少实现**：§4.1 与 §5.1 第 8–12 步。只对主 checkout `discover`。`with_storage` 指向 `<root>/search/<repo>/`。profile 用 §4.1
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-search --test base --features test-seams`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-search/src/base.rs crates/strata-search/tests/base.rs crates/strata-search/src/lib.rs
git commit -m "feat(search): share base index by main checkout tree and profile"
```

### Task 3: overlay 遮蔽与 All

**Files:**
- Create: `crates/strata-search/src/view.rs`
- Test: `crates/strata-search/tests/overlay.rs`

**Interfaces:**
- Consumes: `Overlay`、`HybridIndex`（上游要求 1 的构造函数）、`LiveIndex::upsert_file` / `delete_file`
- Produces: `refresh`、填好的 `grep` / `files`

- [ ] **Step 1: 写失败的测试**：`overlay_entry_shadows_base_path`、`tombstone_hides_base_in_this_session_only`、`grep_all_tags_each_hit_with_session`、`grep_zero_hits_is_empty`、`rename_is_tombstone_plus_new_path`、`commit_inside_session_keeps_overlay_hit`、`untracked_searches_like_added`、`rename_or_symlink_does_not_fail_attach`、`gitfile_and_strata_dir_excluded`、`tracked_ignored_file_still_listed`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-search --test overlay`
Expected: FAIL（命中仍是基线，或 `All` 没有 session）

- [ ] **Step 3: 写让测试通过的最少实现**：§4.4 与 §5.4、§5.5。排除集合与墓碑先于查询
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-search --test overlay`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-search/src/view.rs crates/strata-search/tests/overlay.rs
git commit -m "feat(search): session overlay shadows the shared base"
```

### Task 4: 快照、换根、成员边界

**Files:**
- Test: `crates/strata-search/tests/snapshot.rs`

**Interfaces:**
- Consumes: `execute_query_with_masks`、`resolve_path`、`apfs::open_root`、`apfs::open_rooted_file`
- Produces: 无新签名。`SearchSeam::PauseBeforeVerify`

- [ ] **Step 1: 写失败的测试**：`query_keeps_reader_snapshot_across_base_swap`、`new_query_after_reattach_sees_new_base`、`root_replacement_refuses_old_view`、`refresh_during_root_swap_does_not_publish`、`not_ready_does_not_answer_from_base`、`which_all_fails_if_one_root_changes`、`other_session_publish_does_not_change_this_view`、`candidate_count_matches_snapshot`、`rebuild_preserves_answers_for_empty_short_tombstone`、`oversized_file_listed_not_indexed`、`literal_backslash_is_normal_path`、`verify_does_not_follow_symlink`、`files_glob_does_not_cross_slash`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-search --test snapshot --features test-seams`
Expected: FAIL（换根之后仍返回新目录的内容，或超限文件被读入）

- [ ] **Step 3: 写让测试通过的最少实现**：§4.5、§5.3、§5.6。查询捉住 reader。根变了就丢结果
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-search --test snapshot --features test-seams`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-search
git commit -m "feat(search): pin the query snapshot and refuse a replaced root"
```

§7 里 `matrix_*` 那些行不是要新写的测试。它们记录处置。不要为它们建空测试，也不要跳过。

## 9. 构建者决定记录

| 决定 | 选择 | 理由 | 日期 |
| --- | --- | --- | --- |
| D1 `GrepOpts` 只有大小写 | 不收 tgrep CLI 的其余标志。默认区分大小写 | 裁定：`strata grep` / `files` 只有 `--session` 与 `--all`。多加标志会把约 70 条已标不保留的 3f 行重新打开。路径大小写走卷，不走这个结构 | 2026-10-23 |
| D2 git 环境 | 不替 tgrep 设进程环境。接受 alias 残留 | tgrep 的 `command()` 自己去掉 `GIT_*` 并加上 `--no-replace-objects`、`GIT_NO_LAZY_FETCH`、`GIT_OPTIONAL_LOCKS`。gitio 的环境进不去那个 `Command`。再改进程 PATH 会跟 daemon 抢。命令只读、只打主 checkout | 2026-10-23 |
| D3 profile | `IndexingProfile::default()`，`max_blob_bytes = Some(overlay::MERGE_MAX_BYTES)` | 3f 要求 overlay 与 search 用同一个 64 MiB。config.md 没有这个字段。不各写一个常量 | 2026-10-23 |
| D4 `TooLarge` | 公开的 `grep` / `files` / `refresh` 不因超限文件失败。超限文件不进索引，`files` 仍列出 | 一次多文件查询不该被一个大文件拖失败。`GrepOpts` 不能加路径参数。types.md 的「按条目、不读内容」落实为跳过该文件。请 T15 看要不要改「谁返回」 | 2026-10-23 |
| D5 `publication.lock` | 不包超时，不再加一把锁 | 锁在 `ensure` 内部，`lock_exclusive` 不返回超时。包一层超时会留下仍在跑的构建，下一次还是堵在同一把锁上。等待发生在工作区写之前 | 2026-10-23 |
| D6 进行中的查询与换根 | 根 inode 变了就丢全部命中，返回 `Busy`（`root_changed`），不返回旧内容也不返回新内容 | 增补 8 不允许自己 `openat`。`open_rooted_file` 按路径重新解析，保不住旧目录的字节。索引一侧仍用捉住的 reader。下一次查询指重新 attach 之后 | 2026-10-23 |
| D7 `OverlaySource` | search 不调用 `overlay::compute`，只读 daemon 的 `Tracker::last` | 一个工作区不能有两个扫描者。daemon 已经持有 `Tracker` | 2026-10-23 |
| D8 不回收索引 | v1 不删 `<root>/search/` 下的 generation。一直留着，不挪到 `bases/` | tgrep 是 RetainAll。store.md 不列这个目录，gc 的 1 小时下限也不覆盖根下不认识的一级名字，所以 gc 不会收走它。不是「过 1 小时就被收」 | 2026-10-23 |
| D9 `Which::All` | 只查已挂视图。没挂上的会话不出现，不是错误 | 查询不做附带的 `attach`（那会建索引、有副作用）。daemon 在 `--all` 之前挂上要查的会话 | 2026-10-23 |
| D10 glob | `globset` 0.4.18，`*` 不跨 `/`，`\` 是字面字符。卷不区分大小写时用 globset 的 Unicode 大小写，不自己对齐 `apfs` 折叠键 | 简报的 `files` 要 glob。tgrep 的 `files` 是前缀，不是 glob。不用 `filetypes`（那是 `--type`）。个别字符上与 apfs 折叠不同，v1 接受 | 2026-10-23 |
| D11 `tracked = false` | 可以挂，没有 overlay 就当空 overlay | 增补 1：这种仓库没有 overlay，但仍有基线树。拒绝挂会让 `--all` 漏掉它的基线文件 | 2026-10-23 |
| D12 `All` 的失败 | 任一视图的根对不上，整次 `Busy`，不返回其余会话 | 少了一个会话的列表看起来像成功，调用方会当成完整结果 | 2026-10-23 |
| D13 命中形状 | 一行一条。行不是 UTF-8 就丢掉这一行，查询继续 | `Hit` 没有列号，简报的字段就是 session、repo、path、line、text。`text` 是 `String`，非法 UTF-8 放不进去。不因此让整次查询失败 | 2026-10-23 |

待定：没有。上面 13 条都定了。T15 要看的两处写在 §6 的 `TooLarge` 行和 `NoSuchPath` 行，不是未定的产品行为。
