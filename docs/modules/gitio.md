# `gitio` 模块

**Crate**：`crates/strata-gitio` · **设计来源**：design.md §3（大小写行、主 checkout 常脏）、§4（Repo、Base generation）、§5（`gitio` 行）、§6.1 第 1–2 步、§6.5 第 2–5 步、§6.7、§6.8、§9 不变量 2、3、5、6、7、8，§12（diff3 crate、工作区 `.git` 的接法）；module-rulings.md 的 "T5 gitio"、补充条目与 Addendum 2、3、4、6、7、8 · **矩阵**：tgrep-core（3f）目标为 `gitio` 的 43 行，layerfs-replaced（3d）目标为 `gitio` 的 10 行，以及理由点名 gitio 的行（§7 表后）

gitio 是 strata 里唯一起 git 子进程的模块。它从 tgrep 借了两段代码，照抄并注明出处，不依赖 tgrep-core：`tgrep-core/src/generations/git.rs` 的清洗过的子进程环境，`tgrep-core/src/git_index.rs` 对 index 与 config 的直接解析。本文按 module-rulings.md 的 "T5 gitio" 一节、补充条目和 Addendum 2、3 写成，其中最要紧的一条：agent 用的工作区，git 目录是**本会话在这个 repo 上的影子仓库**，从不接到主 checkout 的 common dir；接到主 checkout 上的只有 publish 的 staging 树。文中标"实测"的 git 行为，都是 2026-10-22 在本机 git 2.54.0（Apple Git-157）上用临时仓库验证过的；"22 个仓库"指 config.md 附录 A 的仓库，当天只用只读命令或直接读文件核对过。

## 1. 职责与边界

**负责**（design §5 的 `gitio` 行）：

1. **git 的唯一出口**。strata 自己的模块起的每个 git 子进程都经 gitio 的 crate 私有 `Git::command`，环境先清洗（§5.1）。唯一的例外在依赖里：search 若用 tgrep-core 的 `GenerationManager` 建基线索引，tgrep-core 在 `GenerationManager::ensure` 里经它自己的 `generations/git.rs` 起 git，不经 gitio；这由 search.md 交代（module-rulings Addendum 2 的 T11 条让它决定基线索引怎么建）。其他模块只调 §3 的类型化函数：它们不起 git，也不解析、不改写 git 的文件（gitfile、管理目录、影子仓库、index、config、ref）。gitio 交出的目录对它们是不透明的整体，只会被整体移动或退役：session 交换带着新 gitfile 的工作区目录，按 module-rulings Addendum 3 经 `store::retire_dir` 退役旧的管理目录与 `rm` 时的影子仓库。gitio 直接读的 git 文件只有 index、gitfile、`commondir`、`objects/info/alternates`、仓库 config、`info/` 下的规则文件与影子仓库的 `hooks/pre-push`（§5.6、§5.7、§5.8；index、gitfile、`commondir` 与 config 的读法借 tgrep），外加工作树里的 `.gitignore` 与全局排除文件，都经 apfs 的 `open_rooted_file`（G11）；直接写的只有影子仓库、管理目录与 staging 私有目录里它自己建的那几个文件（管理目录的 index 是 git 在 `tree_dir/.git/` 下建的临时 index 的副本），以及工作区与 staging 树根下的 gitfile 和 attach 时放临时 index 的 `tree_dir/.git/`（§5.6；gitfile 与 `new` 之后写进影子仓库的管理目录经 apfs 的 rooted 函数写，§2）；其余一律经 git。
2. **仓库身份**：`open` 由主 checkout 的工作树根得到 git common dir 的 realpath（types.md `RepoId`），拒绝 strata 的工作区、SHA-256 仓库、reftable 仓库与过低的 git 版本。
3. **远端与 ref**：`fetch` 把远端分支取进 strata 自己的跟踪 ref；`read_ref`、`is_ancestor`、`merge_base`；`refs/strata/*` 下比较并交换的 `update_ref` 与 `delete_ref`（发布链、pin ref）；`push` 产生 types.md 的 `PushOutcome`。
4. **tree 的两个方向**：`tree_of` 解析并校验基线 tree，`tree_contains` 查一个路径在不在 tree 里（种子文件规则）；`export_tree` 把 tree 导出成目录，逐字节等于 `git checkout` 的结果（检出方向的过滤器）；`stage` 走反方向，按检入方向的过滤器把 staging 树里的文件写成 tree；`commit_tree` 做 commit。
5. **工作区与 staging 的 git**（module-rulings Addendum 2）：`ensure_shadow` 在 `new` 时给一个会话建它在这个 repo 上的影子仓库，是唯一建影子仓库的函数；`shadow_at` 此后只读地认出它（含 daemon 重启后）；`attach_worktree` 给工作区的每一代建一个新的管理目录与 index；`shadow_drift` 报告影子仓库的镜像与主 checkout 现状的差别，以及 pre-push 守卫在工作区里是否可能失效；`attach_staging` 把 publish 的 staging 树接到主 checkout 的 common dir；`worktree_gitdir` 只读地读出一棵树的 gitfile 指向哪个 git 目录（session 核对失败的移动留下的现场）。
6. **index 与 ignore**：`read_index` 经工作树的 `.git` 直接解析 index 与 config；`is_ignored` 按 git 的规则判断，已跟踪的文件永不被 ignore（design §3）；`is_ignored_in_export` 在一个 tree 的导出上按同样的规则回答"检出成工作区之后 git 会不会忽略"，供工作区还不存在时用（session 判显式种子）。
7. **三方合并**：`merge3` 做 hunk 级三方合并，判定与 `git merge-file` 相同；`is_binary` 是 git 的二进制判定。

**不负责**：

| 事项 | 归谁 |
| --- | --- |
| 基线取哪个 commit（`origin/<default_branch>` 还是发布链顶、`pin_unpushed`）；基线目录放在哪、何时导出、复用与封存（design §7 的只读手段） | session |
| 影子仓库与工作区的布局：影子仓库放在 `<paths.root>/sessions/.repos/<session-id>/<repo>.git/`、管理目录的代数 n、新一代随工作区目录原子换入、旧管理目录与 `rm` 时影子仓库的退役、pin ref 叫 `refs/strata/pins/<session-id>`、status 里的镜像漂移告警 | session（session.md §4.2；module-rulings Addendum 2、3）。gitio 实现底层（§5.6） |
| 合并基线 B 的取法（design §6.7）；发布链的名字 `refs/strata/publish/<branch>`；staging 树放在哪；把 `PushOutcome` 转成 `PushRefused` 或 `Unknown`；推送前的人类确认；冲突的呈现与 `--assign`；增、删、改名层面的合并判定 | publish |
| `.lww` 的匹配 | overlay、publish |
| `BaseManifest` 的构建：对导出目录里每个文件取 BLAKE3，取导出后的字节而不是 git blob（3f 的 CRLF/smudge 两行） | overlay（module-rulings Addendum 3） |
| 同一个基线的并发导出者：各自在锁外导出到自己的随机暂存，后登记的那个找到先登记的、退役自己的暂存，谁都看不到 `Exists`（3d `interrupted_transfer…` 一行里并发的那一半；module-rulings "Base generations are built outside the lock" 取代了原来"同一个基线只有一个导出者，由 daemon 的写锁排队"的分工） | session（session.md @cec336b §5.2 第 7.2 步与 `ensure_base_builds_outside_the_lock`） |
| 只差 Unicode 大小写的两个路径（如 `É.txt` 与 `é.txt`）算不算同一个文件 | overlay，用 `apfs::case_fold_eq`（module-rulings Addendum 2）。gitio 的"已跟踪"、ignore 与碰撞判断只按 git 的 ASCII 规则折叠 [我的决定，§9] |
| overlay、checkpoint、监听与对账；依赖目录；种子文件与种子缓存；检查命令 | overlay、apfs、deps、session、publish |

## 2. 依赖

| 依赖 | 理由 |
| --- | --- |
| `strata-types` | `RepoPath`、`TreeOid`、`CommitOid`、`RepoId`、`PushOutcome`、`StrataError`、`Code`（types.md §8 的第 0 层） |
| `strata-apfs` | 根之下不跟随符号链接的读、写、改名与删除（§5.5、§5.6）：`create_file_rooted`（工作区与 staging 树根下的 gitfile）；管理目录 `worktrees/<n>/` 与它里面的文件（含 index 的副本）用的 `attrs_of`、`mkdir_rooted`、`create_file_rooted`、`open_rooted`，根是影子仓库目录（`new` 之后影子仓库是 agent 可写的地方，module-rulings 的 Addendum 8 clarification）；attach 的临时 index 用的 `mkdir_rooted`、`open_rooted_file`、`remove_rooted`，根是 `tree_dir`，核对暂存没被换过用的 `open_root`；进程内的每一次读（index、gitfile、`commondir`、config、alternates、`info/` 与 `.gitignore` 规则文件、全局排除文件、`hooks/pre-push`）用的 `open_rooted_file`，工作树里的 `.gitignore` 不是普通文件时分辨它是不是符号链接用的 `attrs_of`（G11）；导出用的 `mkdir_rooted`、`open_rooted`、`rename_rooted`（`RenameMode::Excl`）、`remove_rooted`（`RemoveKind::File`、`RemoveKind::EmptyDir`）、`open_root`（对根本身做 `full_fsync`）、`PATH_MAX_BYTES` 与 `NAME_MAX_UTF16_UNITS`；`full_fsync`，gitio 的每一次 `F_FULLFSYNC` 都经它；`classify_io_error`，apfs 给的句柄上的读写失败都经它归类：写用 `IoSide::AfterWrite`（gitio 往句柄里写时，这次调用总已建过文件），一律 `Unknown`；读用 `IoSide::Read`，读发生在这次调用的写入开始之后时（如 attach 读回临时 index），结果照 G4 一律当 `Unknown`。apfs 与 gitio 同在第 1 层：module-rulings Addendum 7 的 Layering 条准许 gitio 依赖 apfs（apfs 只依赖 types，没有环），T15 把 types.md §8 第 1 层一行改成"同层只允许 store 与 gitio 依赖 apfs"；Addendum 8 让根之下的文件系统操作都归 apfs，gitio 对工作区、staging 树与导出树不做自己的 `*at` 调用。这些名字与写法都已在 apfs 1e723f2 的 §3 里（module-rulings 的 "apfs names committed at sdd/task-4 1e723f2"），不再是上游要求：根句柄访问函数是 `open_root(root) -> Result<File>`，errno 归类是 `classify_io_error(err, side, path)`。不用 `case_fold_eq`：gitio 按 git 的 ASCII 规则折叠（§9）。不依赖 store |
| `git` 可执行文件，版本 ≥ 2.44 | 唯一的 git 实现。不用 libgit2 或 gitoxide：git 本身就是语义的标准答案。下限是第一个认 `GIT_NO_LAZY_FETCH` 的发行版（按 2.44 的发行说明，实现时再核实；review M6）：更低的 git 会悄悄从 promisor 远端补缺失的对象，G7 不成立。其余用到的参数都更早：`rev-parse --path-format=absolute`（2.31）、`fetch --no-write-fetch-head` 与 `--no-auto-maintenance`、`init --object-format`（2.29）、`--end-of-options`（2.24）、`user.useConfigOnly`（2.8）。从 daemon 的 `PATH` 解析（daemon.md 规定 PATH）；`HOME` 必须是用户的，全局配置里的身份、`core.excludesFile`、凭据助手才生效 |
| `memchr` | 抄来的 index 解析器用它找 NUL（tgrep 原样） |
| `libc` | `fsync`：逐个文件与目录的持久化（§5.5 第 5 步、§5.6）。std 的 `File::sync_all` 在 macOS 上做的是 `F_FULLFSYNC`（Rust 1.92 的 std 源码），逐个用就成了每个文件刷一次盘。`killpg`：超时时杀整个进程组（§5.1 第 5 步）。gitio 没有自己的 `*at` 调用（module-rulings Addendum 8） |
| gitignore 模式匹配 crate | 逐个规则文件匹配，优先级由 gitio 自己叠（§5.8）；选哪个见 §9（待定） |
| 行级 diff crate | `merge3` 用；design §12 交给构建者，§9 待定 |
| `tracing` | `PushOutcome::Unknown` 不带文字，push 与 fetch 的原始输出记进 daemon 的日志 [我的决定] |
| 开发依赖 `tempfile` | 测试用的临时仓库 |

**照抄的代码**（不是依赖）。tgrep 是 MIT 许可，Copyright (c) Microsoft Corporation，取 commit d25ade6。按 module-rulings 补充条目"copy tgrep's `git_index.rs` with attribution rather than depending on tgrep-core"：

| 落点 | 来源 | 改动 |
| --- | --- | --- |
| `src/index.rs` | `tgrep-core/src/git_index.rs` | 只留 `metadata_path`、`read_common_dir_using`、`config_bool`、`ignores_case`、`load_tracked`、`parse_index_with`、`hex`、`merge_split`、`SplitLink`、`ewah_len`、`read_ewah`、`extension`、`index_header`、`entry_capacity`、`walk_entries`、`read_varint`，只在测试里用的 `parse_index`，以及测试和测试辅助函数（`v2_index`、`ewah`、`with_extension`、`link`）；`metadata_paths_preserve_native_bytes` 不迁移（3f 标不保留）。删掉 `TrackedFiles` 连同 `normalise`、`membership_hash`、`fingerprint_*`，`git_dir`、`index_path`、`common_git_dir`、`read_repository_dirs*`，以及全部 `*_controlled` 与 `MemoryCharge`。`TrackedFiles` 换成 `IndexInfo`，保留大小写，路径是 `RepoPath`。`insert_path` 换成 gitio 自己的：非 UTF-8 路径返回 `NotText`，原来静默跳过。`load_tracked`、`ignores_case` 与各个解析函数返回 `Result`，原来是 `Option` 或 `bool`。不再把 `\` 归一成 `/`。遇到 sparse index 的目录条目（`sdir` 扩展）返回 `Unsupported`。`config_bool` 改成按 git 的 config 语法读（§5.7 第 6 步；review B1、M2）[我的决定，§9]：原来的读法不认引号、转义与行内注释，`shadow = "true"` 与 `ignorecase = "true"` 都读不出来。`read_git_dir_using` 不抄：它经 `fs::metadata(.git)` 跟随符号链接；只抄它的字节解析（去掉 `gitdir: ` 前缀，再 `metadata_path`），`read_index` 与 `worktree_gitdir` 共用（re-review r4 increment）。`read_common_dir_using` 的读改成 G11 的 rooted 读，结果按词法规整（§5.7 第 1 步） |
| `src/command.rs` | `tgrep-core/src/generations/git.rs` | 留 `command`、`output`、`path`、`discover_directory`、`repository_command`、`valid_oid`、`object_format_controlled`、`resolve`、`commit_tree`、`commit_tree_controlled`、`parse_entries`。tgrep 的 `discover_directory`、`object_format_controlled`、`resolve`、`commit_tree_controlled`、`parse_entries` 都带 `control: Option<&Control>`（managed v2 的取消与内存记账），照抄时去掉这个参数：`object_format_controlled` 改名 `object_format`；`commit_tree` 与 `commit_tree_controlled` 合成一个，改名 `resolve_commit_and_tree`，免得与本文的 `commit_tree` 同名；`controlled_output` 删掉，调用处改用 `output` 加 gitio 的超时（§5.1）。删掉 `worktree_root`、`worktrees`、`worktrees_controlled`、`entries_controlled`、`entries_for_paths`、`LegacyBlobs`、`Blobs`，以及对 `PipedProcess`、`Control` 的引用。`parse_entries` 的 `TrackedEntry`、`EntryMode` 换成 gitio 自己的条目类型，另改三处：遇到 gitlink 返回 `Unsupported`，原来记成成员；大小一栏为 `BAD` 返回 `NoSuchCommit`，原来算格式错（§5.4）；它调的 `validate_tracked_path` 换成 §5.4 的检查 |

两个文件开头写明出处：仓库、commit、原路径、MIT。crate 根放 `THIRD-PARTY-NOTICES`，收 tgrep 的 LICENSE 原文。

**谁调用 gitio**（module-rulings Addendum 2 的调用表）：

| 调用方 | 用到 |
| --- | --- |
| daemon | `open`、`repo_id`。加载配置时检查每个 `path` 是工作树根、两张表不是同一个 `RepoId`（config.md §4.3） |
| session | `fetch`、`read_ref`、`is_ancestor`、`merge_base`、`tree_of`、`tree_contains`（种子文件"在基线 tree 里未被跟踪"，config.md §4.3）、`export_tree`（在锁外导出到自己的基线暂存）、`ensure_shadow`（只在 `new`）、`shadow_at`（此后与 daemon 重启后）、`attach_worktree`、`update_ref` 与 `delete_ref`（pin ref 的比较并交换）、`is_ignored`、`is_ignored_in_export`（工作区建好之前判显式种子，session.md @853fdf8 §2 的上游要求）、`worktree_gitdir`（`resolve_failed_move` 核对失败的移动留下的现场，session.md @cec336b §2 的上游要求）；`shadow_drift`（status 的告警；fix round 4 起多一个参数 `workspace`，pre-push 守卫按 git 在工作区里的有效 hooks 目录判断，review M3）；`read_index`（`move_workspace` 第 13.2 步取工作区的 `ignorecase`，session.md @5797ee8 §5.7）与 `repo_id`（`RepoId` 就是 `git.repo_id()`，session.md @5797ee8 §4.4），这两项是 re-review r5 对照 session 补上的 |
| deps | `is_ignored`、`tree_of`；`read_ref` 与 `remote_ref`（`main_tip` 读 origin 默认分支的跟踪 ref 与发布链）、`is_ancestor`（只在 `main_tip` 里判断发布链是否领先 origin，design §6.7 的钉基线规则）。按 deps.md @22e1fac §2 的 gitio 行与 §3 的 `main_tip` 行 |
| overlay | `read_index`、`is_ignored`（增量点查与全量对账用同一个函数）、`merge3`（`rebase` 用） |
| publish | `fetch`、`read_ref`、`is_ancestor`、`merge_base`、`tree_of`、`export_tree`、`attach_staging`、`stage`、`commit_tree`、`update_ref`、`push`、`merge3`、`is_binary` |

调用方拿的都是对主 checkout `open` 出来的 `Git`。`read_index` 与 `is_ignored` 收工作区的根，经它的 gitfile 找到影子仓库，再核对影子仓库借的是这个仓库的对象库（§5.7）；没有哪个调用方需要对工作区 `open`。

## 3. 公开接口（Rust）

```rust
// crates/strata-gitio/src/lib.rs —— 签名清单（不是实现）
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use strata_types::{CommitOid, PushOutcome, RepoId, RepoPath, StrataError, TreeOid};

pub type Result<T> = std::result::Result<T, StrataError>;

/// git 版本下限（§2）：第一个认 GIT_NO_LAZY_FETCH 的发行版，实现时按发行说明再核实。
pub const MIN_GIT_VERSION: (u32, u32) = (2, 44);
/// 导出临时目录的名字前缀（§4.2）。status 用它列出失败导出留下的现场。
pub const EXPORT_TEMP_PREFIX: &str = ".export-";

/// 一个仓库，以主 checkout 的 git common dir 识别（design §4 Repo）。只能由 `Git::open` 得到。
/// Clone 很便宜；里面的缓存（§4.3）只影响速度，不影响结果。
#[derive(Clone, Debug)]
pub struct Git {
    /// git common dir 的 realpath。
    common_dir: PathBuf,
    // 私有：主 checkout 的工作树根（G1 用）、缓存（§4.3）
}

/// 一个会话在一个 repo 上的影子仓库（§4.2）。只能由 `Git::ensure_shadow`（`new` 时建）或 `Git::shadow_at`（此后只读地认出）得到。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shadow {
    /// 影子仓库目录的 realpath。
    dir: PathBuf,
}

/// 影子仓库的一项镜像与主 checkout 的现状不同，或 pre-push 守卫可能失效（§5.6）。只是报告，影子仓库从不因此改写。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MirrorDrift {
    /// `info/exclude` 的字节不同，或只有一边有这个文件。
    Exclude,
    /// `info/attributes`，同上。
    Attributes,
    /// 复制清单（§4.2）里的一个键，两边的值不同或只有一边有；或影子仓库 config 里的一条 `include.*`、`includeIf.*`
    /// 或一个 `extensions.*` 键（建仓时从不写，出现就是后加的；review M3，re-review r4 的 N4）。`key` 照 `git config --list`
    /// 的写法：节名与键名小写，子段原样。
    Config { key: String },
    /// pre-push 守卫可能失效，status 的 reason 是 `pre_push_guard_disabled`（module-rulings 的 T13 条）：
    /// git 在工作区里实际用的 hooks 目录不是 gitio 钉的 `<dir>/hooks`（按 realpath 比，即 `Shadow::dir()` 加 `/hooks`；
    /// `hooks_path` 是那个目录，仍是它时为 None；
    /// 由 git 自己在工作区的上下文里查，跟随 include，review M3），或 `hooks/pre-push` 不再是 gitio 写的那份（`hook_replaced`）。gitio 不修。
    PrePushGuardDisabled { hooks_path: Option<String>, hook_replaced: bool },
}

/// 一个工作树 index 的快照（§5.7）。由 `read_index` 产生，之后 index 再变它也不变。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexInfo {
    /// index 里的全部路径，保留 git 记录的大小写，NFC。
    pub tracked: BTreeSet<RepoPath>,
    /// 仓库的 core.ignorecase；工作区读的是影子仓库复制来的值。
    pub ignorecase: bool,
    /// 私有：ignorecase 为真时 tracked 的 ASCII 小写形，供大小写不敏感的查询（只折叠 ASCII，§9）。
    folded: BTreeSet<String>,
}

/// merge3 的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Merge3 {
    /// 没有冲突：合并后的字节。
    Clean(Vec<u8>),
    /// 至少一个冲突区；干净的部分不返回。
    Conflict(Vec<Hunk>),
}

/// 一个冲突区。三段文本都是整行，含行尾；`*_start` 是该段第一行的行号（从 1 起），
/// 该段为空时是插入点后面那一行的行号。约定 ours 是会话一侧，theirs 是上游一侧。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    pub base_start: usize,
    pub ours_start: usize,
    pub theirs_start: usize,
    pub base: Vec<u8>,
    pub ours: Vec<u8>,
    pub theirs: Vec<u8>,
}

impl Git {
    /// 打开主 checkout 的工作树根（人自己 `git worktree add` 的工作树也行）；`root` 不能是子目录（§5.2）。
    /// strata 的工作区被拒绝：strata 从不由工作区里的 git 推出 RepoId。
    /// Code：NoSuchPath；InvalidInput（不是仓库、bare、子目录、strata 的工作区）；
    /// Unsupported（SHA-256、reftable、git < 2.44）；Unknown。
    pub fn open(root: &Path) -> Result<Git>;
    /// common dir 的 realpath。
    pub fn common_dir(&self) -> &Path;
    /// `RepoId::new(common_dir)`：主 checkout 与人自己的 linked worktree 得到同一个值。
    pub fn repo_id(&self) -> RepoId;
    /// 清洗过环境的 `git` 命令（§5.1），下面的函数都经它起子进程。crate 私有：
    /// 其他模块不起 git，只调类型化函数（module-rulings Addendum 2；与计划不同，见本节末）。
    pub(crate) fn command(&self) -> Command;

    /// 把 `<remote>` 的 `refs/heads/<branch>` 取进 `remote_ref(remote, branch)`，返回远端 tip（§5.3）。
    /// 不写 FETCH_HEAD、refs/remotes/*、tag。
    /// Code：InvalidInput（名字不合法、remote 没配置）；NoSuchBranch；Unknown（网络、认证、超时）。
    pub fn fetch(&self, remote: &str, branch: &str) -> Result<CommitOid>;
    /// 读 `refs/strata/` 下的 ref，不存在为 None。Code：InvalidInput（不在 refs/strata/ 下）；Unknown。
    pub fn read_ref(&self, name: &str) -> Result<Option<CommitOid>>;
    /// `ancestor` 是否为 `descendant` 的祖先，相等也算。Code：NoSuchCommit；Unknown。
    pub fn is_ancestor(&self, ancestor: &CommitOid, descendant: &CommitOid) -> Result<bool>;
    /// 一个最近公共祖先；没有共同祖先为 None，调用方据此报 NoMergeBase。Code：NoSuchCommit；Unknown。
    pub fn merge_base(&self, a: &CommitOid, b: &CommitOid) -> Result<Option<CommitOid>>;
    /// 比较并交换：`name` 当前为 `old`（None 表示不存在）时才改成 `new`；只接受 `refs/strata/` 下的名字。
    /// Code：InvalidInput；HeadMoved（detail：ref、expected、actual）；Unknown。
    pub fn update_ref(&self, name: &str, new: &CommitOid, old: Option<&CommitOid>) -> Result<()>;
    /// 比较并交换地删除 `refs/strata/` 下的 ref。Code：同 update_ref。
    pub fn delete_ref(&self, name: &str, old: &CommitOid) -> Result<()>;

    /// commit 的 tree，并校验它能当基线（§5.4）；不受 `git replace` 影响。
    /// Code：NoSuchCommit（commit、tree 或 blob 不在库里，detail.path 给出缺对象的路径）；
    /// Unsupported（gitlink、未知 mode、`.git` 分量、路径碰撞）；NotText；InvalidInput（路径分量超过 255 字节等）；Unknown。
    pub fn tree_of(&self, commit: &CommitOid) -> Result<TreeOid>;
    /// `path` 是否在 `tree` 里：是其中的文件或符号链接，或是它的一个目录；ignorecase 时按 ASCII 不区分大小写（§5.4）。
    /// 先做与 tree_of 相同的校验。Code：同 tree_of。
    pub fn tree_contains(&self, tree: &TreeOid, path: &RepoPath) -> Result<bool>;
    /// 把 tree 导出到还不存在的 `dst`，结果逐字节等于 git checkout（§5.5）；dst 只以完整的形态出现。
    /// 不需要锁：只读主 checkout，只写 dst 与它旁边的临时目录，dst 由调用方保证独有（§5）。中断的导出把临时目录
    /// 留在 dst 的父目录里（§4.2）：dst 在调用方的暂存里时（session 的基线暂存），它随暂存一起退役。
    /// Code：tree_of 的那些；Exists（dst 已存在）；NoSuchPath（父目录不存在）；
    /// NotDirectory（父目录是符号链接或不是目录）；InvalidInput（dst 在主 checkout 里、路径超长）；写入开始之后的任何失败为 Unknown。
    pub fn export_tree(&self, tree: &TreeOid, dst: &Path) -> Result<()>;

    /// 在 `dir` 建一个会话的影子仓库（§5.6）。只在 `new` 时调用，是唯一建影子仓库的函数；
    /// `dir` 已经是本仓库的完整影子仓库时原样返回，不改写任何镜像。
    /// Code：NoSuchPath（父目录不存在）；InvalidInput（dir 不是绝对路径或在主 checkout 里）；
    /// Exists（dir 已存在却不是本仓库的完整影子仓库）；NotFile、IsDirectory、NotDirectory（主 checkout 的 `info/exclude`、
    /// `info/attributes` 解析符号链接之后不是普通文件，G11；什么都没写）；Unknown（它们读不了，同样什么都没写；
    /// 或建仓开始之后的任何失败）。
    pub fn ensure_shadow(&self, dir: &Path) -> Result<Shadow>;
    /// 只读地认出 `dir` 处本仓库的影子仓库（§5.6）：核对 `strata.shadow` 标记与 alternates，从不建、从不改。
    /// `new` 之后（含 daemon 重启后）session 都经它拿 `Shadow`；影子仓库不见了由 status 报出，从不重建。
    /// Code：NoSuchPath（dir 不存在）；InvalidInput（不是本仓库的影子仓库）；Unknown（读不了，config 或 alternates 不是普通文件
    /// （G11），或 config 有 git 也判坏的行）。
    pub fn shadow_at(&self, dir: &Path) -> Result<Shadow>;
    /// 影子仓库的镜像与主 checkout 现状的差别，以及 pre-push 守卫可能失效，供 status 告警；只读，从不修，空表示没有漂移（§5.6）。
    /// `shadow` 来自 `ensure_shadow` 或 `shadow_at`；`workspace` 是接在它上面的工作区根，守卫按 git 在那里的有效 hooks 目录判断，
    /// 工作区的 `.git` 不存在时只比镜像。
    /// Code：InvalidInput（不是本仓库的影子仓库，workspace 不是绝对路径）；Unknown（读不了，或主 checkout 一侧的镜像文件不是普通文件
    /// （G11）；hooks 目录查不出来，`hooks-lookup`：守卫状态不明，可能已失效；查询超时）。影子仓库一侧的镜像文件
    /// 不是普通文件时报成那一项漂移。
    pub fn shadow_drift(&self, shadow: &Shadow, workspace: &Path) -> Result<Vec<MirrorDrift>>;
    /// 给工作区的第 `generation` 代建新的管理目录 `<shadow>/worktrees/<generation>/`：HEAD detached 在 `head`，
    /// index = tree_of(head)，在 `tree_dir` 里 refresh（git 写在 `tree_dir/.git/` 下的临时 index，gitio 再 rooted 地复制进管理目录）；
    /// 再在 `tree_dir` 写 `.git` gitfile 指向它（§5.6）。
    /// `workspace` 是调用方把 `tree_dir` 换入之后的最终路径（`new` 时二者相同）；`shadow` 来自 `ensure_shadow` 或 `shadow_at`；
    /// `generation` 是 store 分配的代数，与 store 的 `WorkspaceRecord.generation` 同为 `u64`，永不复用。返回管理目录的路径，与 gitfile 里写的字节相同（§5.6 第 4 步）。
    /// Code：tree_of 的那些；InvalidInput（shadow 不属于本仓库、generation 为 0、路径不是绝对路径或在主 checkout 里）；
    /// NoSuchPath、NotDirectory（tree_dir）；NotDirectory（`<shadow>/worktrees` 是符号链接或文件，或建管理目录、复制 index 时路径上被换上了
    /// 符号链接）；Exists（`tree_dir/.git` 或这一代的管理目录已存在）。第 2、3 步的 rooted 写入在写之前被拒时原样返回，
    /// 已建的管理目录与临时 index 留给调用方（move 时是它的内部暂存，G9）；其余写入开始之后的失败为 Unknown，
    /// 含 refresh 期间 `tree_dir` 被换过（`detail.reason = "staging-changed"`）与临时 index 的读回被拒
    /// （`"index-read-back-refused"`）：这两种 git 可能写到了暂存之外，daemon 把它们交给人（§5.6）。
    pub fn attach_worktree(
        &self,
        shadow: &Shadow,
        generation: u64,
        head: &CommitOid,
        tree_dir: &Path,
        workspace: &Path,
    ) -> Result<PathBuf>;
    /// 把 publish 的 staging 树接到主 checkout 的 common dir：私有 git 目录 `gitdir`，HEAD detached 在 `head`，
    /// index = tree_of(head)（§5.6）。只给 publish 用，从不交给 agent。
    /// Code：tree_of 的那些；NoSuchPath、NotDirectory（staging）；Exists（staging/.git 或 gitdir 已存在）；
    /// InvalidInput（路径不是绝对路径或在主 checkout 里）；写入开始之后的任何失败为 Unknown。
    pub fn attach_staging(&self, staging: &Path, gitdir: &Path, head: &CommitOid) -> Result<()>;
    /// `tree_dir/.git` 是 gitfile 时返回它指向的 git 目录（§5.6 末段）：不跟随符号链接，经 apfs 的 rooted 读，
    /// 解析同 read_index 读 gitfile 的规则；`tree_dir` 或 `.git` 不存在为 None。只读，不需要锁，从不写。
    /// Code：InvalidInput（tree_dir 不是绝对路径；gitfile 的格式不对，detail.reason = "bad-gitfile"）；
    /// NotDirectory（tree_dir 是符号链接或不是目录）；IsDirectory（`.git` 是目录）；
    /// NotFile（`.git` 是符号链接、FIFO、socket 或设备）；Unknown（读不了）。
    pub fn worktree_gitdir(&self, tree_dir: &Path) -> Result<Option<PathBuf>>;
    /// 按检入方向的过滤器把 `paths` 的当前状态写进 staging 树自己的 index，再 write-tree（§5.9）：
    /// 是文件或符号链接的写入，不存在或是目录的删除，其余路径保持 index 原样。
    /// Code：InvalidInput（不是 attach_staging 接到本仓库的 staging 树）；NotFile（FIFO、socket、设备）；Unknown。
    pub fn stage(&self, staging: &Path, paths: &[RepoPath]) -> Result<TreeOid>;
    /// 以 `parent` 为唯一父 commit、`message` 原样为消息做 commit，不动任何 ref；身份只取 git 配置（§5.10）。
    /// Code：NoSuchCommit（tree 或 parent 不在库里）；InvalidInput（身份没配，detail.reason = "git-identity"）；Unknown。
    pub fn commit_tree(&self, tree: &TreeOid, parent: &CommitOid, message: &str) -> Result<CommitOid>;
    /// 在 `staging` 树根里，把 `src_ref`（必须在 refs/strata/ 下）推到 `<remote>` 的 `refs/heads/<dst_branch>`，不强推（§5.11）。
    /// 子进程起来之前的拒绝是 Err（InvalidInput；src_ref 不存在为 NoSuchCommit）；起来之后只返回 Ok：
    /// 只有本 ref 的状态行算数，超时或没有状态行一律是 PushOutcome::Unknown。
    pub fn push(
        &self,
        staging: &Path,
        remote: &str,
        src_ref: &str,
        dst_branch: &str,
        timeout: Duration,
    ) -> Result<PushOutcome>;
    /// 推到 `remote` 会去的地址：`git remote get-url --push <remote>`（有 `pushurl` 取它，否则 `url`，
    /// 已应用 `insteadOf` / `pushInsteadOf`），就是 push 第 2 步用的那个。只读本地 config，不连远端。
    /// `remote` 照 push 检查。Code：InvalidInput（remote 名不合法；没有这个 remote，detail.reason = "no-such-remote"）；Unknown。
    /// [实现时补的决定]（review-3 R4：daemon 把它记进保留的推送现场，只在推到同一地址时结清）
    pub fn push_url(&self, remote: &str) -> Result<String>;
    /// [实现时补的决定]（review-4 F1）`push_published` / `push_recorded` 推到恰好 `url`（调用方先用
    /// `push_url` 读好、记下的那个），第 2 步不再读 config；`remote` 仍决定 `refs/strata/remotes/<remote>/<dst>`。
    /// 空 `url` → InvalidInput（detail.reason = "empty-url"），什么都没发。其余同各自的无 `_to` 版本。
    pub fn push_published_to(&self, staging: &Path, remote: &str, url: &str, src_ref: &str, dst_branch: &str, anchor_ref: &str, timeout: Duration) -> Result<PushOutcome>;
    pub fn push_recorded_to(&self, staging: &Path, remote: &str, url: &str, src_ref: &str, dst_branch: &str, anchor_ref: &str, verify: &[RefUpdate], timeout: Duration) -> Result<PushOutcome>;
    /// [实现时补的决定]（review-5 N1）按现在的 config，推到恰好 `url` 是否真去 `url`：gitio 推送时给 `url`
    /// 钉一条恒等的 `insteadOf` / `pushInsteadOf`（§5.11 第 3 步），只有 config 里有一条匹配串恰好是 `url`、
    /// 另一个 base 的改写规则，或有名叫 `url` 的 remote（`remote.<url>.url` / `pushurl`）时才不成立；
    /// `url` 为空或含换行、NUL 也为 false。只读本地 config。daemon 推完、结清现场之前再查一次。
    pub fn push_url_exact(&self, url: &str) -> Result<bool>;

    /// 经 `worktree/.git` 直接解析它的 index 与仓库 config（§5.7）。worktree 是主 checkout、
    /// staging 树，或 strata 的工作区（经 gitfile 到本会话影子仓库的管理目录）。
    /// Code：NoSuchPath（没有 .git）；InvalidInput（worktree 属于别的仓库）；NotText；Unsupported（sparse index）；
    /// Unknown（index 不存在为 detail.reason = "no-index"；index、gitfile、`commondir`、config 读不了、不是普通文件（G11）或格式坏）。
    pub fn read_index(&self, worktree: &Path) -> Result<IndexInfo>;
    /// git 在这个 worktree 里会不会忽略这个路径（§5.8）。已跟踪的永远是 false；目录按规则回答，目录本身从不算已跟踪。
    /// Code：read_index 的那些；Unknown（规则文件读不了或不是普通文件，G11：工作树里的 `.gitignore` 是符号链接或目录时
    /// 当作没有；全局排除文件的查询超时）。
    pub fn is_ignored(&self, worktree: &Path, path: &RepoPath) -> Result<bool>;
    /// `tree` 检出成本仓库的工作区之后，git 会不会忽略 `path`（§5.8 末段）；`dir` 是 `tree` 的一份导出（没有 `.git`），
    /// 工作区还不存在时用（session 判显式种子）。规则同 is_ignored，差别三条：已跟踪看 tree 的路径表，`.gitignore` 从 `dir` 读，
    /// `info/exclude` 与全局排除文件取主 checkout 现在的那份（`ensure_shadow` 将要镜像的）。只读，不需要锁。
    /// Code：tree_of 的那些；InvalidInput（dir 不是绝对路径，或 dir 里有 `.git`）；NoSuchPath、NotDirectory（dir）；
    /// Unknown（规则文件读不了或不是普通文件（G11），主 checkout 的 config 有 git 也判坏的行，全局排除文件的查询超时）。
    pub fn is_ignored_in_export(&self, dir: &Path, tree: &TreeOid, path: &RepoPath) -> Result<bool>;
}

impl Shadow {
    /// 影子仓库目录。session 记下它，`rm` 时整体退役（§1）。
    pub fn dir(&self) -> &Path;
}

impl IndexInfo {
    /// path 是否已跟踪；ignorecase 时按 ASCII 不区分大小写。
    pub fn is_tracked(&self, path: &RepoPath) -> bool;
    /// dir 之下是否有已跟踪的文件，前缀止于 `/`；ignorecase 时按 ASCII 不区分大小写。
    pub fn tracked_under(&self, dir: &RepoPath) -> bool;
}

/// strata 的远端跟踪 ref：`refs/strata/remotes/<remote>/<branch>`（§4.1）。
pub fn remote_ref(remote: &str, branch: &str) -> String;
/// hunk 级三方合并（§5.12），判定与 git merge-file 相同。纯函数，不失败。
pub fn merge3(base: &[u8], ours: &[u8], theirs: &[u8]) -> Merge3;
/// git 的二进制判定：前 8000 字节里有 NUL。
pub fn is_binary(bytes: &[u8]) -> bool;

// ---- 测试接缝（module-rulings Addendum 4 "Test seams"）----
// 只在 cargo feature `test-seams` 打开时编译；只在测试里打开，从不进发布的二进制（§8）。
// 都挂在一个 `Git`（连同它的 clone）上，并行的测试各用各的仓库，互不干扰。
// 要在两步之间做事的测试用停住接缝，不用 errno 故障点。

/// 一个 git 子进程，在起之前记下；记录点就是 §5.1 起子进程的那一处。
#[cfg(feature = "test-seams")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitCall {
    /// 参数，不含程序名 `git`。
    pub args: Vec<std::ffi::OsString>,
    /// 子进程实际得到的环境：继承来的环境按 §5.1 清洗，再加上 gitio 设的（含 `set_env` 的值）。
    pub env: std::collections::BTreeMap<std::ffi::OsString, std::ffi::OsString>,
    /// 子进程的工作目录。
    pub cwd: PathBuf,
    /// gitio 往 stdin 喂数据时为 true（`commit-tree -F -`、`update-index -z --stdin`），否则 stdin 是 `/dev/null`。
    pub stdin_piped: bool,
}

/// 一次持久化或改名（§5.5、§5.6），按发生的顺序记下。
#[cfg(feature = "test-seams")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncEvent {
    /// 对这个路径做了 `fsync`。
    Fsync(PathBuf),
    /// 对这个路径的句柄调了 `apfs::full_fsync`（`F_FULLFSYNC`）。
    FullFsync(PathBuf),
    /// 调了 `apfs::rename_rooted(…, RenameMode::Excl)`；from、to 是改名前后的绝对路径。
    RenameExcl { from: PathBuf, to: PathBuf },
}

/// 停住接缝（`pause_export_before_rename`、`pause_attach_before_index`、`pause_attach_before_read_tree`、`pause_attach_before_readback`）的句柄。
#[cfg(feature = "test-seams")]
pub struct Pause { /* 私有 */ }

#[cfg(feature = "test-seams")]
impl Git {
    /// 同 `open`，但 `open` 自己起的子进程就已经带这些环境变量；返回的 Git 也带着它们，如同随后调了 `set_env`。
    /// 测试都用它打开仓库（§8，review N8）：临时的 `HOME`、`XDG_CONFIG_HOME` 与 `GIT_CONFIG_NOSYSTEM=1`。
    pub fn open_with_env(root: &Path, env: &[(&str, Option<&std::ffi::OsStr>)]) -> Result<Git>;
    /// 此后这个 Git 起的子进程，以及它自己对 `HOME`、`XDG_CONFIG_HOME` 的读取（§4.3 的全局排除文件），
    /// 都用这里的值；None 表示去掉。值在 §5.1 的清洗之后才设，所以 `GIT_CONFIG_NOSYSTEM` 这类 `GIT_*` 也设得上。
    /// 不改测试进程的环境：每个用例由此有自己的全局配置、全局排除文件与 `EMAIL`。
    pub fn set_env(&self, key: &str, value: Option<&std::ffi::OsStr>);
    /// 此后 fetch 的超时改为 `timeout`（缺省 120 s，§9）。
    pub fn set_fetch_timeout(&self, timeout: Duration);
    /// 取出并清空这个 Git 起过的 git 子进程，按起的顺序；含 `open` 自己起的。
    pub fn take_git_calls(&self) -> Vec<GitCall>;
    /// 取出并清空这个 Git 做过的 fsync、F_FULLFSYNC 与改名，按发生的顺序。
    pub fn take_sync_events(&self) -> Vec<SyncEvent>;
    /// 停住：这个 Git 的下一次 export_tree 在 §5.5 第 5 步持久化之后、第 6 步改名之前停住，直到放行。
    pub fn pause_export_before_rename(&self) -> Pause;
    /// 停住：这个 Git 的下一次 attach_worktree 在 §5.6 第 2 步建好管理目录之后、第 3 步建 index 之前停住，直到放行。
    pub fn pause_attach_before_index(&self) -> Pause;
    /// 停住：这个 Git 的下一次 attach_worktree 在 §5.6 的 3.1 已 open_root 并记下 (dev, ino, ctime) 之后、3.2 的 read-tree 之前停住，直到放行。
    pub fn pause_attach_before_read_tree(&self) -> Pause;
    /// 停住：这个 Git 的下一次 attach_worktree 在 §5.6 的 3.2（read-tree 与 refresh）之后、3.3 核对暂存与读回之前停住，直到放行。
    pub fn pause_attach_before_readback(&self) -> Pause;
}

#[cfg(feature = "test-seams")]
impl Pause {
    /// 阻塞到那次调用停在停住点。
    pub fn wait_until_paused(&self);
    /// 放行那次调用。
    pub fn release(self);
}
```

- **与计划的签名相比**：名字照计划：`Git { common_dir }`、`command`、`fetch`、`tree_of`、`export_tree`、`read_index`、`IndexInfo { tracked, ignorecase }`、`is_ignored`、`commit_tree`、`update_ref`、`push`、`merge3`、`Merge3 { Clean | Conflict(Vec<Hunk>) }`。计划里没写 `Result` 的五个（`tree_of`、`export_tree`、`read_index`、`is_ignored`、`commit_tree`）都包上 `Result`，因为 rulings 要求它们返回 `Unsupported`、`NotText` 和显式的 index 错误。`PushOutcome` 用 types.md 的定义，本文不重新定义。
- **交给 T15 改计划的接口差异**（计划 T5 的 Interfaces；module-rulings Addendum 2 的 T15 条）：
  1. `Git::command` 是 `pub(crate)`，计划写的是公开的 `fn command(&self) -> Command`（review I2）。
  2. OID 参数一律借用：`tree_of(&CommitOid)`、`export_tree(&TreeOid, …)`、`commit_tree(&TreeOid, &CommitOid, …)`；`update_ref` 的 `old` 是 `Option<&CommitOid>`，计划写的是 `Option<CommitOid>`（review M5）。
  3. `push` 多了第一个参数 `staging`：推送在 staging 树根里跑（review M16）。
  4. `attach_worktree` 换了意思：给本会话影子仓库建一代管理目录，不再接主 checkout；接主 checkout 的是新的 `attach_staging`（review I2）。
  5. types.md §3 的 `RepoId` 注释"主 checkout 和接在它 object store 上的所有工作区是同一个 `RepoId`"早于影子仓库的裁定：工作区的 common dir 是影子仓库，`RepoId` 只从主 checkout 得到（§5.2）。
  6. types.md §8 第 1 层一行改成"同层只允许 store 与 gitio 依赖 apfs"（module-rulings Addendum 7 的 Layering 条；§2）。
- **计划之外新增的公开项**：Addendum 2 点名的 `ensure_shadow`、`attach_staging`、`tree_contains`、`shadow_drift`，review N2 要的 `shadow_at`，以及它们用到的 `Shadow`、`MirrorDrift`；调用表要的 `open`、`repo_id`、`read_ref`、`is_ancestor`、`merge_base`、`remote_ref`、`delete_ref`；补充条目"clean 过滤器的反方向由 gitio 规定"要的 `stage`；session 的上游要求（session.md @853fdf8 §2，module-rulings 的 Requirements rounds 条）要的 `is_ignored_in_export`；session 第二轮的上游要求（session.md @cec336b §2，module-rulings 的 "gitio requirements from session r2"）要的 `worktree_gitdir`。另有三项 [我的决定]：`common_dir`（daemon 报错时给出路径）、`is_binary`（让 types.md 的 `Binary` 与 merge3 用同一个判定）、两个常量。本节代码块末尾 `test-seams` 下的测试接缝 [我的决定] 只在测试里编译，不算公开接口，也不进发布的二进制（module-rulings Addendum 4）。
- gitio 的类型（`Git`、`Shadow`、`MirrorDrift`、`IndexInfo`、`Merge3`、`Hunk`）只在进程内传递，不出现在 RPC 或 JSON 上，所以不 derive `Serialize`。status 要报镜像漂移时，由 session 转成自己的告警；publish 把 `Hunk` 转成 `Conflict.detail` 时，自己决定 JSON 形状与每个 hunk 的长度上限（module-rulings T10）。它们属于 types.md §2.1 所说"模块拥有的跨模块类型"，名字待 T15 补进那张表。

## 4. 数据与不变量

### 4.1 写进主 checkout 的东西

只有对象与 `refs/strata/*` 两类，此外一个字节都不写（不变量 2）：

| 写入 | 由谁写 | 说明 |
| --- | --- | --- |
| `<common>/objects/` 下的对象 | `fetch`（收到的 pack）；publish 经 `stage`（blob、tree）与 `commit_tree`（commit） | 只增不改，按内容寻址。gitio 从不运行 `gc`、`prune`、`repack` 或 maintenance：`fetch` 带 `--no-auto-maintenance`、`--no-write-commit-graph`。不变量 2 的原文只说"写只限 `refs/strata/*`"，没提对象；这是 design 隐含的例外（§6.1 的 fetch、§6.5 的 commit），由 T15 按 module-rulings Addendum 2 写进不变量 2 与计划的 Global Constraints：ref 只在 `refs/strata/*` 下，含 session 的 pin ref（本表 `refs/strata/pins/<session-id>` 一行）；对象由 fetch 与 publish 的 commit 写入（review M11）。另外，导出与 staging 的 refresh 会跑仓库配置的过滤器，过滤器自己也可能写 common dir，例如 git-lfs 的 smudge 把对象缓存在 `<common>/lfs/` |
| `refs/strata/publish/<branch>` | publish，经 `update_ref` | 发布链（design §6.7） |
| `refs/strata/pins/<session-id>` | session，经 `update_ref`、`delete_ref` | 会话在这个 repo 上的 head。主 checkout 的 `git gc` 看不到影子仓库，有了 pin，钉住的 commit 与 tree 不会被回收（session.md §4.2）。这是工作区给主 checkout 的唯一写入 |
| `refs/strata/remotes/<remote>/<branch>` | `fetch`；`push` 返回 `Pushed` 之后 | strata 自己看到的远端 tip [我的决定]。不用 `refs/remotes/*`：后台移动人的远端跟踪分支，会让人之后的 `git push --force-with-lease` 以为看过这些提交，从而把它们盖掉 |
| `refs/strata/` 下的其他名字 | 调用方经 `update_ref`、`delete_ref` | gitio 只检查前缀，名字的含义归调用方 |

不写：工作树、`<common>/index`、`HEAD`、`refs/heads/*`、`refs/remotes/*`、`refs/tags/*`、`refs/stash`、`config`、`hooks/`、`info/`、`FETCH_HEAD`、`<common>/worktrees/`，以及 fsmonitor 的 `fsmonitor--daemon.ipc` 与 `fsmonitor--daemon/`（§5.1）。reflog 只在用户把 `core.logAllRefUpdates` 设成 `always` 时才会给 `refs/strata/*` 记，那是用户自己的配置，22 个仓库都没设。

agent 在工作区里的 git 写入（`git add`、`commit`、`stash`、`branch`、`tag`、HEAD 的移动）全都落在本会话的影子仓库里，主 checkout 与别的会话都看不到（实测）。design §6.8 的"允许本地 git add/commit/stash"因此不触及不变量 2。

### 4.2 gitio 在主 checkout 之外写的东西

影子仓库（`ensure_shadow`）与工作区的管理目录（`attach_worktree`）。位置由 session 给，布局按 module-rulings Addendum 2：

```text
<dir>/                              影子仓库，即 <paths.root>/sessions/.repos/<session-id>/<repo>.git/
  HEAD、refs/、objects/             git init --bare 建的；agent 的 ref 与新对象都在这里
  objects/info/alternates           一行：主 checkout 的 <common>/objects 的 realpath
  config                            core.repositoryformatversion、core.bare、复制的键、core.hooksPath = <dir>/hooks（绝对路径）、strata.shadow = true
  info/exclude、info/attributes     主 checkout 的同名文件（是符号链接时取它指向的内容，G11），建仓时逐字节复制成普通文件；主 checkout 没有就不建
  hooks/pre-push                    没有 STRATA_PUBLISH=1 就拒绝
  worktrees/<n>/                    工作区第 n 代的管理目录，每一代一个新的
    commondir                       ../..
    gitdir                          <工作区>/.git 的绝对路径
    locked                          git-strata（git worktree prune 因此永不清理它）
    HEAD                            detached：<head> 加换行
    index                           git 在 <tree_dir>/.git/index 建出（read-tree，update-index --refresh 填好 stat），gitio 复制进来
<tree_dir>/.git                     一行 gitfile：恰好是 gitdir: <管理目录的绝对路径，末尾不带 /> 加一个 LF，随目录换入工作区
```

- **复制的配置键**：session.md §4.2 的清单，即 `core.ignorecase`、`core.precomposeunicode`、`core.autocrlf`、`core.eol`、`core.safecrlf`、`core.symlinks`、`core.filemode`、`core.checkRoundtripEncoding`，以及 `filter.*`、`diff.*`、`merge.*` 整节；再加 `core.excludesFile`、`core.attributesFile` [我的决定：主 checkout 在仓库级设了它们时，工作区里 git 的 ignore 与属性才与主 checkout 一致]。从主 checkout 的仓库级 config 读（`config --local`，不展开 `include`），清单以外的键一个都不复制；尤其从不复制 `remote.*`、`branch.*`、`core.hooksPath`、`core.worktree`、`core.bare`、`include.*`、`includeIf.*`、`extensions.*`（module-rulings Addendum 2）。用户级与系统级 config 由 git 照常读取，唯一的例外是 `core.hooksPath`：影子仓库自己钉一个绝对的 `core.hooksPath = <dir>/hooks`（review N1 的裁定），仓库级的设置盖过用户级与系统级，外面的 hooksPath 关不掉 `pre-push` 守卫（实测：用户级或系统级设了 hooksPath 而影子仓库没钉时推送成功，钉了就被拒）。钉住还有一个后果：用户级与系统级 hooksPath 里的其他钩子（比如全局的 commit-msg）在工作区里也不跑，工作区只跑 `<dir>/hooks` 里的钩子，也就是 pre-push 守卫（review B5；re-review r2 实测，全局的 commit-msg 钩子只在没钉的工作区里跑）。用户级 config 里以 `includeIf` 只给主 checkout 引入的配置，在工作区里不生效 [我的决定，§9]。
- **只取一次**：镜像（两个 `info/` 文件与复制的键）在建仓时取，会话存活期间从不改写；`shadow_drift` 报告它们与主 checkout 现状的差别，status 据此告警。
- **publish 的私有 git 目录**（`attach_staging`）：位置由 publish 给，不能在主 checkout 里。内容是 `commondir`（主 checkout common dir 的绝对路径）、`gitdir`（`<staging>/.git` 的绝对路径）、`HEAD`（detached）、`index`；staging 根下的 `.git` 是指向它的一行 gitfile。之后 `stage` 只改这个 index，并把对象写进主对象库（§4.1）。
- **导出临时目录**（`export_tree`）：`<dst 的父目录>/.export-<dst 的名字>-<nonce>/`，里面是 `tree/`（导出的文件）与 `index`（私有 index）。成功时 `tree/` 改名为 `dst`，临时目录随即删掉；失败时原样保留。导出到可见的目的地时，status 按前缀 `EXPORT_TEMP_PREFIX` 列出它；导出到调用方的暂存里时（session 的基线暂存 `bases/<repo>/.staging/<hex>/tree`），它是缓存填充的残留，留在暂存 `B` 里，随 store 的 gc 退役 `B` 一起离开（module-rulings "Base staging is registered right after its manifest"）。

### 4.3 进程内缓存

都按不可变的键或文件的 stat 键控，丢掉只会变慢。G11 里先 `realpath` 的人写文件，键用解析之后的路径与那个文件的 stat，每次查缓存前都重新 `realpath`，所以改了符号链接的指向、或改了它指向的文件，都会重读：

| 缓存 | 键 | 内容 |
| --- | --- | --- |
| tree 校验 | `TreeOid` | `tree_of` 的结论（Ok 或 Err），tree 不可变，结论永久有效；校验通过的 tree 另留路径表给 `tree_contains`，最多 64 个 tree，按最近使用淘汰 [我的决定] |
| index 解析 | index 文件路径加 (size, mtime 纳秒, inode)，再加 §5.7 第 6 步读过的 config 文件的同样三项 | `IndexInfo`；`ignorecase` 来自 config，config 一变就重读 |
| 规则文件 | 文件路径加 (size, mtime 纳秒, inode) | 解析后的模式；每次 `is_ignored`、`is_ignored_in_export` 对涉及的规则文件各 stat 一次 |
| 主 checkout 的 `core.ignorecase` | `<common>/config` 的 (size, mtime 纳秒, inode) | `is_ignored_in_export` 用的值（§5.8 末段） |
| 影子仓库的归属 | 影子仓库路径 | 标记与 alternates 的核对结论；影子仓库存活期间这两项不变 |
| 全局排除文件的路径 | common dir（主 checkout 或影子仓库） | 第一次用到时 `git --git-dir=<common> config --path --get core.excludesFile`（10 s 超时，§5.1），没设时取 `$XDG_CONFIG_HOME/git/ignore`，再缺省 `~/.config/git/ignore`。影子仓库复制了主 checkout 仓库级的这个键，所以两边解析出同一个文件，与各自 git 的判定一致（实测）。daemon 重载配置时重新 `open`，缓存随之清空 |
| git 版本 | 进程 | 第一次 `open` 时读一次 |

### 4.4 不变量

| # | 不变量 | 落实 design §9 |
| --- | --- | --- |
| G1 | 主 checkout 只读，写入只有 §4.1 的两类。每个读写 index 的命令都显式设 `GIT_INDEX_FILE`，指向导出的私有 index、attach 时 `tree_dir/.git/` 下的临时 index 或 staging 私有目录的 index，git 从不落到 `<common>/index`；每个 git 子进程都带 `-c core.fsmonitor=false`，用户级的 `core.fsmonitor` 不会让 git 在 common dir 里建 fsmonitor 的 socket、起守护进程（§5.1）。`export_tree` 的 `dst`、`ensure_shadow` 的 `dir`、`attach_worktree` 的 `tree_dir` 与 `workspace`、`attach_staging` 的 `staging` 与 `gitdir` 落在主 checkout 的工作树或 common dir 里时，返回 `InvalidInput`；拿主 checkout 或人自己的 linked worktree 当 `stage`、`push` 的 staging 树同样拒绝。`fetch` 不写 `FETCH_HEAD`、`refs/remotes/*`、tag；`push` 推到地址，不更新 `refs/remotes/*` | 2 |
| G2 | `update_ref`、`delete_ref`、`read_ref` 只接受 `refs/strata/` 开头的名字。`push` 的源必须在 `refs/strata/` 下，目标恰好一个 `refs/heads/<dst_branch>`，从不强推 | 2、3 |
| G3 | 导出不改已有的目录：`dst` 存在就拒绝；最后一步用 `apfs::rename_rooted` 的 `RenameMode::Excl`，竞争下也不覆盖。`dst` 只以完整的形态出现：先持久化，再改名 | 5 |
| G4 | 结果不确定就是 `Unknown`。`push` 没拿到本 ref 的状态行即 `Unknown`；任何写入开始之后的失败即 `Unknown`（`attach_worktree` 第 2、3 步的例外见 G9），已写的原样保留。gitio 从不重试写操作，也从不清理失败的现场 | 6 |
| G5 | 已跟踪的文件永不被 ignore：`is_ignored` 先查 index，ignorecase 时按 ASCII 不区分大小写 [我的决定，与 git 相同；只差 Unicode 大小写的归 overlay，§9] | —（design §3 大小写行） |
| G6 | 跨出 gitio 的路径一律是 `RepoPath`；git 给出的非 UTF-8 路径返回 `NotText`，从不跳过 | — |
| G7 | 解析的对象就是确切的那一个：`--no-replace-objects`、`GIT_NO_LAZY_FETCH=1`；拒绝 SHA-256 与 reftable 仓库、gitlink；影子仓库总是 SHA-1、files 引用存储 | 5 |
| G8 | 参数不会被当成选项：修订、remote、分支先校验，再放在 `--end-of-options` 之后；路径经 `-z --stdin` 传，或放在 `--` 之后并带 `--literal-pathspecs` | 3 |
| G9 | 先拒绝、后写入：`export_tree`、`ensure_shadow`、`attach_worktree`、`attach_staging`、`stage` 的类型化拒绝全部发生在创建任何文件之前，之后的失败按 G4 处理（module-rulings 的错误时序规则）。一个例外：`attach_worktree` 第 2、3 步的 rooted 写入在写之前被拒（`NotDirectory`、`Exists`）时原样返回。`move_workspace` 时这时写下的只有调用方的内部暂存（未被引用的下一代管理目录与移动暂存里的临时 index，module-rulings 的 "Error timing, refined"），由调用方退役再以这个码拒绝；`new` 时 `tree_dir` 就是工作区，调用方把 `create_session` 之后的任何失败都当现场（§5.6） | 6 |
| G10 | agent 用的工作区从不接到主 checkout 的 common dir：它的 git 目录是本会话的影子仓库，没有 remote，`pre-push` 没有 `STRATA_PUBLISH=1` 就拒绝（config 钉住 `core.hooksPath`，外面的 hooksPath 关不掉它；它是守卫，不是安全边界，§9），ref 与新对象不出影子仓库；影子仓库的镜像只在建仓时取一次，从不改写；gitio 从不由工作区里的 git 推出 `RepoId`（`open` 拒绝工作区）。接到主 checkout 的 common dir 上的只有 publish 的 staging 树（`attach_staging`），它由 strata 驱动，从不交给 agent。`new` 之后影子仓库是 agent 可写的地方：gitio 之后往里写的（move 时的管理目录与它的 index 副本）都经 apfs 的 rooted 函数，根是影子仓库目录；git 自己在 `new` 之后不往影子仓库里写任何东西（module-rulings 的 Addendum 8 clarification；§5.6 末尾的清单） | 2、3、7、8 |
| G11 | 进程内读的、别的进程也够得着的文件，都经 `apfs::open_rooted_file`：以 `O_NONBLOCK` 打开，要求普通文件，放进来的 FIFO 得 `NotFile`，从不挂住（module-rulings "gitio reads files through apfs" 与它的 "refined" 一条）。根：工作树里的文件（gitfile、`.gitignore`）以调用方给的工作树根为根，严格；只有这个参数与传给 `open` 的 `path` 逐字节相同时（主 checkout），才用 `open` 算出的 `realpath(root)`（§5.7 第 1 步；breaker 裁定，re-review r5 的 M-1）；git 目录里的文件以 common dir 为根，git 目录在它之下时用相对路径（工作区的 `worktrees/<n>/index`），不在之下时（publish 的私有 git 目录）以 git 目录自己为根，`commondir` 本身以 git 目录为根。只有人写的文件例外，先 `realpath`（跟随符号链接，同 git），再以它所在的目录为根：本仓库 common dir（主 checkout 的 git 目录，不是影子仓库）里的 `info/exclude`、`info/attributes`、`config`、`config.worktree`，以及用户级的全局排除文件 [我的决定，§9]。工作区的全局排除文件按影子仓库解析（§4.3）：影子仓库 config 里的 `core.excludesFile` 是复制来的键，agent 写得到，改了算 `Config` 漂移，所以这个路径其实可以由 agent 选；先 `realpath` 对它仍然无害——`realpath` 从不打开目标，打开以 `O_NONBLOCK` 进行、只收普通文件，读到的也正是工作区里的 git 读的那份（breaker 裁定，re-review r5 的 M-2）。`realpath` 得 `ENOENT`（含悬空的符号链接）或 `ENOTDIR`（路径上某一级是文件）照"不存在"，同 git；其余错误（`ELOOP` 也在内）一律 `Unknown`（`detail` 带路径与 errno），是有意取的安全一侧：git 对 `ELOOP` 随文件而异（排除文件警告一句后当作没有，config 直接失败），gitio 不去分（breaker 裁定，re-review r5 的 M-3）。agent 或 strata 写的照旧一个符号链接都不跟随（refined）：工作树里的文件、影子仓库、任何 `worktrees/<n>/` 管理目录、任何一处的 `.git` 与 index。不存在照各处原来"文件不存在"的规则；不是普通文件时，工作树里的 `.gitignore` 是符号链接或目录就当作没有，与 git 相同，其余照各函数"读不了"的结果（§6）[我的决定，§9]。git 子进程照旧有超时（§5.1） | — |

## 5. 流程与算法

gitio 自己不拿 strata 的锁，靠 git 的锁文件（ref 锁、`index.lock`）保证单个命令的原子性。会写主 checkout 或共享目录的操作（`fetch`、`attach_staging`、`stage`、`commit_tree`、`update_ref`、`delete_ref`）由调用方在 daemon 的 per-repo 写锁内调用；`ensure_shadow`、`attach_worktree` 只写本会话的影子仓库，按 module-rulings Addendum 3 同样在锁内调用（锁只罩 fetch、克隆、attach、pin、交换这些短临界区，`new` 的 fetch 也拿锁）。`export_tree` 不在其中（module-rulings 的 "gitio requirements from session r2"）：它只读主 checkout，只写调用方给的 `dst` 与它旁边的临时目录（§5.5），调用方保证 `dst` 独有就不需要锁。session 在锁外把基线导出到自己的随机暂存 `bases/<repo>/.staging/<hex>/tree`：基线是缓存填充，只有登记进锁（module-rulings "Base generations are built outside the lock"）。过滤器（若有）在 common dir 里的缓存由过滤器自己处理并发（session.md @cec336b §2）。`push` 的远端那一段不需要 gitio 的锁，它对本地唯一的写（§5.11 第 5 步）自带比较并交换，是否持锁推送由 publish.md 与 daemon.md 定。只读的（`read_index`、`is_ignored`、`is_ignored_in_export`、`worktree_gitdir`、`tree_of`、`tree_contains`、`shadow_at`、`shadow_drift`、`read_ref`、`is_ancestor`、`merge_base`、`merge3`）不需要锁。gitio 不做 checkpoint：发布前（`BeforePublish`）与移动前（`BeforeRollout`）的 checkpoint 是调用方的事。

### 5.1 子进程环境（`command`）

1. `Command::new("git")`；把 `std::env::vars_os()` 里大写后以 `GIT_` 开头的键全部 `env_remove`，与 tgrep 相同。这包括 `GIT_DIR`、`GIT_WORK_TREE`、`GIT_INDEX_FILE`、`GIT_OBJECT_DIRECTORY`、`GIT_CONFIG_*`、`GIT_AUTHOR_*`、`GIT_COMMITTER_*`、`GIT_SSH_COMMAND`、`GIT_ASKPASS`。另外去掉继承来的 `STRATA_PUBLISH`。
2. 加参数 `--no-replace-objects` 与 `-c core.fsmonitor=false` [我的决定，§9]；设 `GIT_TERMINAL_PROMPT=0`、`GIT_NO_LAZY_FETCH=1`、`GIT_OPTIONAL_LOCKS=0`。stdin 缺省接 `/dev/null`：凭据助手给不出凭据时立即失败，不会挂住；要喂数据的命令（`commit-tree -F -`、`update-index -z --stdin`）接管道，写完就关。关掉 fsmonitor，是因为用户级的 `core.fsmonitor = true` 会让 git 在 `checkout-index`、`update-index --refresh` 时起 fsmonitor 守护进程，并在 git 目录里建 `fsmonitor--daemon.ipc` 与 `fsmonitor--daemon/`：实测 `export_tree` 那样的 checkout-index（`--git-dir` 是主 checkout 的 common dir）就在主 checkout 的 `.git` 里建了这两项、留下一个守护进程，违反 G1；move 时的 refresh 在影子仓库里建了 `fsmonitor--daemon.ipc`。gitio 的命令都用不着它。
3. `LC_ALL=C` 只加在 gitio 要按原话分类 stderr 的命令上：`fetch`、`update-ref`、`commit-tree`，以及 `rev-parse`、`ls-tree` 的失败分类 [我的决定]。会跑用户命令的命令不加：`checkout-index`（smudge）、`update-index`（clean，含 `--refresh`）与 `push`（pre-push 钩子；它的归类只看 porcelain 状态行，git 不翻译这一行）。过滤器与钩子因此继承 daemon 自己的 locale（daemon.md 定；review M15）。加了 `LC_ALL=C` 的命令里也有会跑用户命令的：主 checkout 自己的 `reference-transaction` 钩子（若有；`update-ref` 与 `fetch` 时），以及 `fetch` 起的凭据助手与 ssh 传输（`core.sshCommand`；继承来的 `GIT_SSH_COMMAND` 已在第 1 步清掉）。它们都继承 `LC_ALL=C`，影响的只是它们输出的语言（提示、报错），不影响取凭据与连接；git 没有只给自己设 locale 的办法，而 fetch 的分类要读英文原话，所以留下这一点代价 [我的决定，review N5，§9]。
4. 仓库级命令（只碰对象与 ref：`rev-parse`、`ls-tree`、`fetch`、`update-ref`、`commit-tree`、`merge-base`、`config`）用 `current_dir(common_dir)` 加 `--git-dir=.`（tgrep 的做法）。git 这时把 common dir 当作工作树顶层，所以 gitio 从不在这种调用里跑碰工作树的命令；主 checkout 自己的钩子（若有）在这里以 common dir 为 cwd 运行。工作树级命令（`read-tree`、`update-index`、`write-tree`、`checkout-index`、`push`）用 `--git-dir=<git 目录> --work-tree=<树>`，`current_dir` 是树的根，不向上发现；`shadow_drift` 在工作区上下文里的 `rev-parse --git-path hooks` 也按这一条，`--git-dir` 给的是工作区的 gitfile（§5.6）。读写 index 的命令再设 `GIT_INDEX_FILE`；它在第 1 步清洗之后才设，所以只有 gitio 给的值生效。读写 index 的命令（`read-tree`、`update-index`、`checkout-index`、`write-tree`）另加关掉钩子的 `-c core.hooksPath=/dev/null` [我的决定，§9]：git 写完 index 会跑 `post-index-change` 钩子（实测 read-tree 与 refresh 都跑），而这些命令只碰 strata 自己的私有 index；对影子仓库，钩子目录是 agent 可写的，不关的话 move 时 strata 会替 agent 跑它放进去的钩子（实测）。`push` 不加：人自己的 pre-push 钩子照跑（§5.11）。命令行上的 `-c` 压过各级 config，也压过它们 include 进来的文件。
5. 子进程放进自己的进程组（`process_group(0)`）。超时就对整个组发 SIGKILL，`ssh` 这类孙进程一起结束，再等它退出。`fetch` 的超时是常量（§9），`push` 的由调用方给。本地命令里只有会读活影子仓库 config 的那几个设超时 [我的决定，§9]：git 的每个命令启动时都读仓库 config、跟随其中的 include，影子仓库 config 里一条指向 FIFO 的 include 让它一直挂住（实测 `config --local --list`、`config --get`、`rev-parse` 都挂住）。只读的查询各 10 s：`shadow_drift` 的三次子进程，与全局排除文件的查询（§4.3；`is_ignored` 与 `is_ignored_in_export` 共用这一处，按主 checkout 查时也一样）；`attach_worktree` 的 read-tree 与 refresh 各 600 s。到时杀进程组，返回 `Unknown`（`detail.reason = "timeout"`）。其余本地命令不设超时。
6. 输出只解析 `-z` 与 porcelain 格式。OID 经 `TreeOid`、`CommitOid` 的 `TryFrom` 校验（40 位）。stderr 只用来分类和填 `detail`，取末尾 4 KiB。

### 5.2 `open(root)`

1. git 版本，每个进程查一次：`git version` 低于 2.44 → `Unsupported`。
2. `root` 不存在 → `NoSuchPath`。
3. `git -C <root> rev-parse --path-format=absolute --show-toplevel --git-common-dir --show-object-format`（一次子进程，三行，实测）：失败（不是仓库）或没有工作树（bare）→ `InvalidInput`。
4. `realpath(toplevel) ≠ realpath(root)` → `InvalidInput`。配置的 `path` 必须是工作树根，否则会认成上层的仓库。
5. `common = realpath(common dir)`。用一次 `git config --local --get-regexp` 读出 common dir 的 `strata.shadow`、`extensions.refStorage`：
   - `strata.shadow = true`，即 `root` 是 strata 的工作区 → `InvalidInput`，`detail.reason = "strata-workspace"` [我的决定]。工作区的 common dir 是本会话的影子仓库，由它推出的身份每个会话都不一样；strata 从不由工作区里的 git 推出 `RepoId`（module-rulings Addendum 2），工作区的 `RepoId` 由 session 经清单给出（session.md §4.4）。人自己用 `git worktree add` 建的工作树不受影响，它的 common dir 就是主 checkout 的。
   - `extensions.refStorage` 设了且不是 `files`（reftable 仓库）→ `Unsupported` [我的决定]：`attach_staging` 手写的 detached `HEAD` 只在 files 引用存储下有效。
6. 第 3 步的第三行（对象格式）不是 `sha1` → `Unsupported`：`--show-object-format` 读的就是 `extensions.objectFormat`，types.md §3 的 `TreeOid` 只有 40 位。实测 `--object-format=sha256` 建的仓库里该键为 `sha256`，普通仓库没有这个键。
7. 记下主 checkout 的工作树：`root` 与第 4 步算出的 `realpath(root)`（G11 的读以后者为根），以及 common dir 名为 `.git` 时它的父目录，供 G1 判断。返回 `Git`；全局排除文件的路径在第一次用到时按 common dir 解析（§4.3）。

共三次子进程（版本只查一次），daemon 加载配置时每个仓库一次。22 个仓库的 config 里都没有 `extensions.*`（SHA-1、files），仓库级 `core.ignorecase` 都是 `true`。

### 5.3 `fetch(remote, branch)`

1. 校验：`remote` 只含 `[A-Za-z0-9._-]`，且不以 `-` 或 `.` 开头，这是 gitio 自己的规则，config.md 没有 remote 名的规则；`branch` 不以 `-` 开头，且 `git check-ref-format refs/heads/<branch>` 通过。分支这两条只是 config.md V4 的子集：V4 在加载配置时已对 `default_branch`、`publish.branch` 按更严的规则查过（另外拒绝 `HEAD`、`@`、`refs/` 开头等），gitio 再查一次，是因为 fetch 与 push 不只收配置里的名字（review M10）。不合法 → `InvalidInput`。
2. `git fetch --no-tags --no-write-fetch-head --no-auto-maintenance --no-write-commit-graph --no-prune --no-recurse-submodules --refmap= --end-of-options <remote> +refs/heads/<branch>:refs/strata/remotes/<remote>/<branch>`。`--refmap=` 让 git 不按 `remote.<remote>.fetch` 顺手更新 `refs/remotes/<remote>/<branch>`。实测只有 strata 的 ref 动了，没有 `FETCH_HEAD`，也没有 tag。超时 120 s [我的决定，§9]，到时杀进程组，返回 `Unknown`。
3. 退出 0：`git rev-parse --verify --end-of-options refs/strata/remotes/<remote>/<branch>^{commit}`，返回这个 commit。
4. 退出非 0：stderr 含 `couldn't find remote ref` → `NoSuchBranch`，包括远端还没有任何提交的情形，不会当成空 tree（实测原话）；含 `does not appear to be a git repository` 或 `No such remote` → `InvalidInput`；其他（网络、认证）→ `Unknown`，`detail` 带 stderr 末尾。失败时 strata 的 ref 不变，git 在最后一步才改 ref。
5. 远端分支缺失而发布链存在时算不算错，由 publish 判断（config.md §4.3：`origin/<publish.branch>` 存在，或者已有发布链）。

一次网络往返。session::new 对配置里的每个仓库都要 fetch，22 个仓库都在；design §8 的"新建会话 < 5 s"要求 session 并行发起，不同仓库的 fetch 互不相干。

### 5.4 `tree_of(commit)` 与 `tree_contains(tree, path)`

`tree_of(commit)`：

1. 照抄 tgrep 的 `resolve_commit_and_tree`：`git rev-parse --verify --end-of-options <commit>^{commit}`，再对结果取 `^{tree}`。commit 不在库里，或它的根 tree 对象不在（实测退出 128）→ `NoSuchCommit`。`--no-replace-objects` 让 `git replace` 不起作用（3f replace refs 一行）。
2. 缓存里有这个 tree 的校验结论 → 直接返回。
3. 校验，每个 tree 只做一次：`git ls-tree -r -z -l --full-tree <tree>`。退出非 0 而 stderr 说缺 tree 对象（`Could not read`、`not a tree object`；实测缺一个子 tree 时退出 1）→ `NoSuchCommit`；其他非 0 → `Unknown`；退出非 0 时已经输出的部分一律不用。逐条解析，照抄 tgrep 的 `parse_entries`：
   1. 模式只能是 `100644`、`100755`、`120000` 的 blob。`160000 commit`（gitlink，即子模块）→ `Unsupported`，`detail.path` 给出路径；其他模式 → `Unsupported`。
   2. 大小一栏：`-l` 要读每个 blob 的大小。blob 不在库里时 git **照样退出 0**，只把这一栏印成 `BAD`（实测，review I3）。`BAD` → `NoSuchCommit`，`detail.path` 给出路径；部分克隆缺的 blob 一样，`GIT_NO_LAZY_FETCH=1` 让 git 不去远端补。既不是十进制数也不是 `BAD` → `Unknown`。
   3. 路径字节不是 UTF-8 → `NotText`，`detail.path` 用有损形式；`RepoPath::try_from` 不接受（如 NFC 规整后有空段）→ `InvalidInput`。
   4. 任一路径分量超过 255 字节（APFS 的上限，types.md §6.3 的 `ENAMETOOLONG` 行）→ `InvalidInput`。
   5. 任一分量按 ASCII 不区分大小写等于 `.git` → `Unsupported`，`detail.path` 给出路径 [我的决定]。tgrep 的 `validate_tracked_path` 同样拒绝；`git mktree` 造得出这样的 tree，但 git 检出时拒绝（实测 `read-tree` 报 `invalid path '.GIT/config'`），等到导出才发现就已经写了临时目录。
   6. 碰撞：两条路径 NFC 规整后相同，或在 ignorecase 仓库里 ASCII 小写后相同 → `Unsupported`，`detail` 列出两条 [我的决定]。这样的 tree 在大小写不敏感的 APFS 上检出会少一个文件，导出必然对不上 tree。同一个名字既是文件或符号链接、又是目录（ls-tree 里一条路径是另一条路径的上级，`git mktree` 造得出来）也算碰撞 → `Unsupported` [我的决定]：`git fsck` 判这种 tree 为 `duplicateEntries`，read-tree 却悄悄丢掉其中的文件条目、照常退出 0（实测），导出与 `tree_contains` 就对不上。只差 Unicode 大小写的一对（如 `É.txt` 与 `é.txt`）gitio 不查：git 在 APFS 上检出这样一对同样只留下一个文件，导出仍与 `git checkout` 逐字节相同；它们算不算同一个文件归 overlay，用 `apfs::case_fold_eq` 判断（module-rulings Addendum 2；review M14）。22 个仓库没有 ASCII 碰撞。
   7. 输出截断、缺结尾 NUL、字段数或 OID 长度不对 → `Unknown`，不凭空造条目。
4. 记下结论与路径表（§4.3），返回 tree。

`tree_contains(tree, path)`：先按上面第 2–4 步得到这个 tree 的结论与路径表（路径表已被淘汰就再跑一次 ls-tree），结论是错误就原样返回。`path` 是表里的一个条目（文件或符号链接），或是某个条目的上级目录 → `true` [我的决定：目录也算在 tree 里，于是一个与被跟踪目录同名的显式种子条目照样被拒]；ignorecase 时按 ASCII 不区分大小写，与 `IndexInfo::is_tracked`、`tracked_under` 同一规则（§9）。session 用它判种子文件"在基线 tree 里未被跟踪"（config.md §4.3）：那时工作区还不存在，没有 index 可读。

复杂度 O(tree 的文件数)，一次子进程；`tree_contains` 命中缓存时 O(log 文件数)。tool 有 4196 个文件，staff-api 有 4947 个（2026-10-22 各自的 origin 默认分支）。`WorkspaceState.head` 的不变量 `tree_of(head) == base.tree` 走缓存。

### 5.5 `export_tree(tree, dst)`

1. 拒绝，此时还什么都没写（G9）：tree 校验同 §5.4；`dst` 已存在（lstat）→ `Exists`，不碰它；`dst` 的父目录不存在 → `NoSuchPath`，是符号链接或不是目录 → `NotDirectory`；`dst` 在主 checkout 的工作树或 common dir 里 → `InvalidInput`；`dst` 的名字当不了单级 `RepoPath`（apfs 的函数只收 `RepoPath`：要是 UTF-8，不能是 `.` 或 `..`）→ `InvalidInput`；第 2 步的临时目录名 `.export-<dst 的名字>-<nonce>` 超过 `apfs::NAME_MAX_UTF16_UNITS` 个 UTF-16 码元 → `InvalidInput`（review B7）；按第 2 步的 `T/tree` 算（git 写的是它下面的路径，`T` 的名字比 `dst` 长），加上最长的路径达到 `apfs::PATH_MAX_BYTES` 字节（1024：macOS 的 `PATH_MAX` 含结尾的 NUL）→ `InvalidInput`。
2. 建临时目录 `T = <父目录>/.export-<dst 的名字>-<nonce>/` 与其中的工作树目录 `T/tree`。`T` 与 `dst` 同一个父目录，所以在同一个卷上，最后的改名是原子的。gitio 自己的写都经 apfs 的 rooted 函数（module-rulings Addendum 7 的 Layering 条与 Addendum 8），根是 `dst` 的父目录 P，即调用方的暂存目录，归 strata 所有（见本步末尾）：`mkdir_rooted(P, <T 的名字>, 0o700, false)`，再 `mkdir_rooted(P, <T 的名字>/tree, 0o755, false)`，第 4 步的 checkout-index 要求工作树目录已经存在；第 6 步的改名、第 7 步的删除同样以 P 为根 [我的决定，§9]。第一次 `mkdir_rooted` 失败时什么都没写：父目录的 `NoSuchPath`、`NotDirectory` 原样返回（第 1 步之后父目录被删，或被换成符号链接），其余是 `Unknown`。它成功之后任何失败都是 `Unknown`，`T` 原样保留（G4）。`T` 建在 `dst` 旁边，所以调用方要把 `dst` 放进自己的意图记录点名的暂存目录里，gc 的护栏 (a) 才护得住 `T`（护栏见 module-rulings Addendum 4，这条规则见 Addendum 6 的 Export staging；session 与 store 从它们那边写同一条）。session 的基线暂存 `B`（`bases/<repo>/.staging/<hex>/`）没有意图记录，是另一种情形：它的护栏是 gc 对 `.staging` 的年龄下限、per-repo 写锁与改名进 trash（module-rulings "Base staging is registered right after its manifest"），`T` 建在 `B` 里，受同样的护栏，中断时随 `B` 离开（§4.2）。
3. `GIT_INDEX_FILE=T/index git --git-dir=<common> read-tree <tree>`。
4. `GIT_INDEX_FILE=T/index git --git-dir=<common> --work-tree=T/tree checkout-index --all --force`，不设 `LC_ALL`（§5.1）。这一步按仓库配置做检出方向的全部转换：`core.autocrlf`、`core.eol`，`.gitattributes` 的 `text` 与 `eol`、`working-tree-encoding`、`ident`、`filter=<driver>` 的 smudge（含 `filter.<driver>.process` 长驻过滤器）。属性取自这个 tree 自己的 `.gitattributes`、`<common>/info/attributes` 与全局属性文件，与在主 checkout 里 `git checkout` 这个 tree 完全相同。实测：带 `eol=crlf`、一对 rot13 smudge/clean 过滤器、符号链接与可执行文件的 tree，导出与 `git clone` 后的检出逐字节相同。
   - smudge 跑的是仓库或全局配置里的命令，以 daemon 的用户身份在 `T/tree` 里执行，和用户自己检出时是同一信任级别。联网的过滤器（如 git-lfs）会让导出也联网，还可能写它自己在 common dir 里的缓存（§4.1）。
   - `filter.<driver>.required = true` 的过滤器失败时，checkout-index 非零退出 → `Unknown`。没设 required 的过滤器失败时，git 照写原始 blob，与 `git checkout` 相同。
   - 符号链接写成符号链接（`core.symlinks`），`100755` 写上可执行位（`core.filemode`）。每个路径是独立的文件，git 不记录硬链接；git tree 里也没有空目录。
   - 不用 `git archive`：它会应用 `export-ignore` 与 `export-subst`，那不是检出语义。
   - `T/tree` 里的文件由 git 写进 gitio 在第 2 步建好的空目录，gitio 自己不往里写。工作树目录必须已经存在：不存在时 checkout-index 以 128 退出（`this operation must be run in a work tree`，实测）；建好之后空 tree 也照常退出 0，导出成空的 `dst`。实测 git 不穿过它自己写出的符号链接：同一个名字既是符号链接又是目录（§5.4 现在拒绝），或只差 Unicode 大小写的符号链接与目录，git 都用目录替掉符号链接，链接指向的目录一字没动。
   - 22 个仓库都没有 `.gitattributes`、没配过滤器、没设 `core.autocrlf`，所以今天的导出等于原样写出 blob。上面的规则为将来的仓库与测试而写。
5. 持久化 [我的决定]：`T/tree` 本身和它下面的每个文件与目录 `fsync`，再对 `T/tree` 做一次 `apfs::full_fsync`。macOS 的 `fsync` 不刷磁盘缓存，一次 `F_FULLFSYNC` 把之前写出的全部刷下去。要打开的是 `<T 的名字>/tree` 本身，以及这个 tree 的路径表（§4.3；已被淘汰就再跑一次 ls-tree）里模式 `100644`、`100755` 的条目与所有条目的各级上级目录在它下面的路径，逐个 `apfs::open_rooted(P, …)` 再 `fsync`；模式 `120000` 的条目（符号链接）不打开，`open_rooted` 对它们报 `NotFile`。不遍历目录，所以没有 `apfs::walk` 的深度上限；代价是每个路径 O(深度) 次 `openat`。
6. **原子替换点**：`apfs::rename_rooted(P, <T 的名字>/tree, <dst 的名字>, RenameMode::Excl)`，再对 P 做 `apfs::full_fsync`，P 本身的句柄取自 `apfs::open_root(P)`（§2）。改名之前崩溃，`dst` 不存在、`T` 保留；改名之后，`dst` 是完整的。撞上别人刚建好的 `dst` 时 `rename_rooted` 返回 `Exists`，这时按第 2 步的规则报 `Unknown` 并保留 `T`；调用方给的 `dst` 是它独有的（session 每次导出都用随机名的暂存），所以正常不会发生。
7. 删掉 `T`，此时里面只剩私有 index：`apfs::remove_rooted(P, <T 的名字>/index, RemoveKind::File)`，再 `remove_rooted(P, <T 的名字>, RemoveKind::EmptyDir)`。module-rulings Addendum 7 准许操作删掉自己建的空目录，非空的只经 `store::retire_dir` 离开，所以先删文件、再删空目录，不用 `remove_tree_rooted`。

复杂度 O(文件数 + 字节数)。design §8 没有导出的验收数字：它的"新建会话 < 5 s"假定基线已在缓存里，只有 origin tip 移动后的第一次 `new` 才导出。首个 spike 实测 tool 与 staff-api 的导出时间，写回 §9。

### 5.6 工作区与 staging 的 git

module-rulings Addendum 2 定了接法：**每个会话、每个 repo 一个影子仓库**，agent 用的工作区是它的 linked worktree，每一代一个新的管理目录；只有 publish 的 staging 树接在主 checkout 的 common dir 上，因为发布的 commit 要落进 `refs/strata/publish/*` 所在的主对象库。布局归 session（session.md §4.2），gitio 实现底层。不把工作区接到主 checkout 上，原因有四条：

1. 钩子与 config 只从 common dir 解析。接在主 checkout 上，design §6.8 的 `pre-push` 守卫只能装进主 checkout 的 `hooks/` 或改它的 config，两者都违反不变量 2；不装，agent 习惯性的 `git push origin HEAD:master` 就没有守卫，也没有人类确认（design §6.9，不变量 7）。
2. refs 共用：agent 的 `git stash`、`git branch`、`git tag` 会写进主 checkout，在工作区里还能改 `refs/strata/publish/*`。
3. front 的 dev-link 会对兄弟库执行 `git reset --hard origin/main`。接在主 checkout 上的兄弟工作区能解析 `origin/main`，会被拖离钉住的基线（不变量 8）。影子仓库没有 remote，`git fetch origin` 失败，`origin/main` 解析不到（实测）。
4. 所有会话共用一个影子仓库，就共用 `refs/stash` 与分支；在活着的会话下刷新它的镜像，文件会在被忽略与 Untracked 之间来回翻（module-rulings Addendum 2，T9 复审后的修订）。所以每会话一个，镜像只在建仓时取一次。

**`ensure_shadow(dir)`**（只在 `new` 时、每个 repo 一次；唯一建影子仓库的函数，review N2）：

1. 拒绝（G9）：`dir` 不是绝对路径，或落在主 checkout 的工作树或 common dir 里 → `InvalidInput`；`dir` 的父目录不存在 → `NoSuchPath`。`dir` 已存在时：它是本仓库的完整影子仓库（config 里有 `strata.shadow = true`，`objects/info/alternates` 第一行的 realpath 等于本仓库 `<common>/objects` 的 realpath）→ 原样返回 `Shadow`，一个字节都不写，镜像也不刷新；否则 → `Exists`，不碰它（config 与 alternates 经 `apfs::open_rooted_file(dir, …)` 读，G11；不是普通文件也算不完整）。`dir` 不存在时，再按 G11 人写文件的读法读出主 checkout 的镜像 `<common>/info/exclude` 与 `info/attributes`：先 `realpath`，再以所在目录为根经 `apfs::open_rooted_file` 读，dotfile 工具做成的符号链接读到的是它指向的内容（同 git）；不存在（`realpath` 得 `ENOENT` 或 `ENOTDIR`，含悬空的符号链接，G11）就不建；解析之后不是普通文件 → apfs 的码（`NotFile`、`IsDirectory`、`NotDirectory`），`realpath` 的其余错误与读不了 → `Unknown`，都在写任何东西之前。第 4 步写的就是这里读到的字节，影子仓库里的镜像因此总是普通文件。
2. 建仓：`git -c init.defaultRefFormat=files init --bare --template= --object-format=sha1 <dir>` [我的决定]。三个参数都是必需的：用户的全局配置可以设 `init.defaultObjectFormat = sha256`、`init.defaultRefFormat = reftable`、`init.templateDir`，不压住的话影子仓库会成为 SHA-256 或 reftable 仓库、带上模板里的钩子（实测）；`--template=` 还让 git 不建 `hooks/*.sample`、`description` 和带注释的默认 `info/exclude`（实测建出来只有 `config`、`HEAD`、`objects/`、`refs/`）。旧版 git 不认识的 `-c init.defaultRefFormat` 无害。从这一步起，任何失败都是 `Unknown`，`dir` 原样保留（G4）。
3. `objects/info/alternates`：一行，主 checkout `<common>/objects` 的 realpath。读对象经它，不复制对象；agent 在工作区里 `git add`、`git commit` 新建的对象写进影子仓库自己的 `objects/`（实测）。
4. 镜像，只取这一次：写入第 1 步读出的 `info/exclude`、`info/attributes` 的字节，主 checkout 没有的就不建。
5. `hooks/pre-push`（0755）：环境里没有 `STRATA_PUBLISH=1` 就往 stderr 写一行 `git-strata: push refused in a strata workspace; publish with git-strata publish`，以 1 退出（design §6.8）。它只对这个影子仓库的工作区生效；第 7 步钉住的 `core.hooksPath` 让外面的 hooksPath 关不掉它。
6. 持久化已写的部分：第 2–5 步建出的文件与目录逐个 `fsync`，再一次 `F_FULLFSYNC`（同 §5.5 第 5 步）。
7. `config` 由 gitio 整份重写，放在最后：`core.repositoryformatversion = 0`、`core.bare = true`，从主 checkout 的仓库级 config（`git config --local --null --list`）复制 §4.2 清单里的键，写 `core.hooksPath = <dir 的绝对路径>/hooks`（review N1 的裁定），最后写 `strata.shadow = true` [我的决定]。每个值照 git 自己的写法写出 [我的决定，§9]（review B1）：只有以空格开头或结尾、或含 `;`、`#`、回车的值整个加双引号；`"` 与 `\` 前加反斜杠，换行与制表符写成 `\n`、`\t`；其余原样，所以 `strata.shadow = true` 与钉住的 hooks 路径都不带引号，任何读者都读得对。主 checkout 里没有 `=` 的键照样不写 `=`（`--null --list` 里它没有换行），同一个键出现几次就照原顺序写几次，子段名加引号（其中的 `"`、`\` 加反斜杠）。实测 git 2.54 的 `git config --file` 就是这样写的，写出的每个值都能原样读回。整份内容先写进同目录的临时文件并 `fsync`，`rename` 成 `config`，再对 `dir` 做一次 `F_FULLFSYNC`。标记出现时其余部分已经落盘，影子仓库就是完整的；标记出现之前的任何崩溃，下一次 `ensure_shadow` 看到的都是"不是本仓库的完整影子仓库"，返回 `Exists`。

没有 remote：工作区里引用远端的命令（`git fetch origin`、`git pull`、`git log origin/main`）都失败，这是有意的，发布只经 strata；tool 的 dev-link 每 30 秒对兄弟库的 `git fetch` 因此失败，无害（design §6.8 第 4 条按 Addendum 2 改读，由 T15 改）。复杂度 O(复制的键数 + 镜像文件的大小)，两次子进程（`init` 与读主 checkout 的 config）。

**`shadow_at(dir)`**（只读；`new` 之后与 daemon 重启后，session 都经它拿 `Shadow`，review N2）：`lstat(dir)` 不存在 → `NoSuchPath`；不是目录，或 config 里没有 `strata.shadow = true`，或 `objects/info/alternates` 第一行的 realpath 不是本仓库 `<common>/objects` 的 realpath → `InvalidInput`；读不了 → `Unknown`。存在与否每次都 lstat，归属的结论可以走缓存（§4.3）。从不建、从不改：不刷新镜像，不补缺的文件，不碰 mtime。影子仓库不见了时，session 把工作区报成坏的（status），从不调 `ensure_shadow` 重建：重建会按今天的主 checkout 重新取镜像，违背"只取一次"，还会丢掉 agent 在影子仓库里的 ref 与 stash。不起子进程：标记用照抄并改过的 `config_bool` 直接读 config（§5.7 第 6 步的读法：引号、转义与行内注释都照 git），alternates 直接读文件；两者都经 `apfs::open_rooted_file(dir, …)`（G11），不是普通文件 → `Unknown`。标记缺失，或它的值不是 git 认的 true（false，或根本不是布尔值）→ `InvalidInput`；config 有 git 也判坏的行 → `Unknown`（`detail.reason = "bad-config"`）。

**`shadow_drift(shadow, workspace)`**（status 用，只读）：核对 `shadow` 属于本仓库（同 `shadow_at` 的核对，否则 `InvalidInput`），`workspace` 不是绝对路径 → `InvalidInput`。然后：

1. 镜像：两个 `info/` 文件逐字节比，两边都经 `apfs::open_rooted_file` 读（G11）：影子仓库一侧以影子仓库为根、不跟随符号链接，不是普通文件算作不同，同 `hooks/pre-push`；主 checkout 一侧照人写文件的读法先 `realpath`（与 `ensure_shadow` 第 1 步相同），解析之后不是普通文件 → `Unknown` [我的决定，§9]；复制清单里的每个键，两边各一次 `git config --local --null --list`，按值比。影子仓库那份列表里的每一条 `include.*`、`includeIf.*` 与每一个 `extensions.*` 键（`ensure_shadow` 从不写这几类，§4.2）各报一项 `Config { key }`（review M3，re-review r4 的 N4）：gitio 的读法不跟随 include（§5.7 第 6 步）；`extensions.worktreeConfig` 打开之后 git 还读管理目录的 `config.worktree`，那里的 include 不在这份列表里（re-review r4 实测），所以打开它本身就报。status 由此看得出读法可能与 git 不同。
2. pre-push 守卫（review B4、M3）：有效的 hooks 目录由 git 自己在工作区的上下文里查，`git --git-dir=<workspace>/.git --work-tree=<workspace> rev-parse --path-format=absolute --git-path hooks`，`current_dir` 是工作区根；`--git-dir` 给的是 gitfile 时 git 照常经它找到管理目录，不向上发现（实测）。git 照常读系统、用户、影子仓库与 `config.worktree` 各级 config，跟随 include，`includeIf "gitdir:…"` 按管理目录求值，与 agent 在工作区里推送时完全一样。结果与 `Shadow::dir()`（realpath，§3）加 `/hooks` 不同 → `PrePushGuardDisabled { hooks_path: Some(结果) }`。不与 `ensure_shadow` 第 7 步写进 config 的那个字符串比：git 给出的是 realpath（re-review r4 的 N8：`/tmp/…` 下的钉子回来是 `/private/tmp/…`）。实测（git 2.54）有四种情形，影子仓库 config 的本地列表里仍是钉住的值（或没有这个键），有效目录却在别处：在工作区里加的 `include.path` 指向一个设了钩子路径的文件；`extensions.worktreeConfig` 加 `git config --worktree core.hooksPath`；钉住的键被删而用户级设了钩子路径；gitfile 被改指主 checkout。前三种从工作区推到显式地址都成功了。钉住的键还在时，用户级的钩子路径（含只对管理目录生效的 `includeIf "gitdir:"`）压不过它，不报。`hooks/pre-push` 不再是 gitio 写的那份（经 `apfs::open_rooted_file(dir, hooks/pre-push)` 读，字节或可执行位不同，或读不出：不见了、是符号链接、不是普通文件）→ `hook_replaced`，不论有效目录在哪 [我的决定，§9]。钩子管理工具（husky、lefthook）在工作区里安装时就会这样改，§9。`<workspace>/.git` 不存在 → 不查守卫：没有工作区就没有从它的推送，status 已经报 `Missing`。查询失败 → `Unknown`（`detail.reason = "hooks-lookup"`），意思是守卫状态不明、可能已经失效（re-review r4 的 N2）：git 认不出工作区的 git 目录（gitfile 坏了），或 config 里有它拒绝的值，都以 128 退出；后一种实测过，`git config --worktree core.hooksPath ""` 之后查询报 "The empty string is not a valid path"，从工作区推到显式地址却照样成功、不跑钩子。这时其余的漂移项也不返回，status 照 `Unknown` 报出。

返回不同的项，按 `Exclude`、`Attributes`、`Config`（键名排序）、`PrePushGuardDisabled` 排列；空表示没有漂移。从不改写影子仓库与工作区，也不修守卫，是否重新钉住由 daemon 决定（module-rulings 的 T13 条）：镜像在会话存活期间不变（Addendum 2），漂移只是 status 的告警。读不了 → `Unknown`；三次子进程各有 10 s 超时（§5.1）。复杂度 O(镜像大小)，三次子进程。

**`attach_worktree(shadow, generation, head, tree_dir, workspace)`**（下文把 `generation` 记作 n：`new` 时 n = 1，此后每次 `move_workspace` 用 store 新分配的 n，`u64`，永不复用；`shadow` 来自 `ensure_shadow` 或 `shadow_at`）：

调用方先把基线克隆到 `tree_dir`：`new` 时就是工作区本身（`tree_dir == workspace`），`move_workspace` 时是旁边的临时树，交换之后它出现在 `workspace` 的位置。

1. 拒绝（G9）：`shadow` 不属于本仓库（同 `ensure_shadow` 第 1 步的核对）→ `InvalidInput`；`n = 0` → `InvalidInput`；`tree = tree_of(head)`，它的码原样返回；`tree_dir` 不存在 → `NoSuchPath`，不是目录（lstat，符号链接也算）→ `NotDirectory`；`tree_dir` 或 `workspace` 不是绝对路径，或落在主 checkout 的工作树或 common dir 里 → `InvalidInput`；`<tree_dir>/.git` 已存在（lstat，文件、目录、符号链接都算）→ `Exists`；`<shadow>/worktrees` 存在却不是目录（`apfs::attrs_of(shadow, worktrees)`，lstat 语义，符号链接也算）→ `NotDirectory`（session 处理这个拒绝时不留着它，而是把这个条目本身挪进 `<shadow>/strata-quarantine/worktrees-<nonce>`，做法、detail、告警与改名失败时的 `Unknown` 都同第 2 步末尾对 `worktrees/<n>` 的隔离：留着的话以后每次 move 都被拒，git 的 `gc` 还会经它删掉目标里的东西——re-review r5 实测，`worktrees` 链到一份主 checkout `.git` 的副本时，工作区里一次 `git gc` 删掉了那个 `.git` 里除放进去的管理目录之外的一切。挪走之后工作区里的 git 明显失败，不再悄悄留着陷阱；下一次 attach 时 `worktrees` 不存在，这一步放行，第 2 步的 `mkdir_rooted` 建一个新的。gitio 的行为不变；module-rulings "A symlinked worktrees/<n> is quarantined, never kept — amended"）；`<shadow>/worktrees/<n>/` 已存在 → `Exists`。管理目录从不复用：每一代一个新的，交换由调用方原子完成（Addendum 2；review M13）。
2. 建管理目录 A：路径是 `Shadow::dir()`、`/worktrees/` 与十进制的 n，末尾不带 `/`（第 4 步的 gitfile 与返回值都是这个写法）；在 A 里写 `commondir`（`../..`）、`gitdir`（`<workspace>/.git` 的绝对路径，即交换之后的位置）、`locked`（内容 `git-strata`，`git worktree prune` 因此永远不清理它）、`HEAD`（`<head>` 加换行，即 detached）。都经 apfs 的 rooted 函数，根是影子仓库目录（module-rulings 的 Addendum 8 clarification：`new` 之后影子仓库是 agent 可写的地方，agent 的 git 往里写 ref、对象、index 与管理文件；review B3）：`mkdir_rooted(shadow, worktrees, 0o755, false)`（已存在得 `Exists`，照常往下走），再 `mkdir_rooted(shadow, worktrees/<n>, 0o755, false)`，四个文件各一次 `create_file_rooted(shadow, worktrees/<n>/<名字>, 0o644)`，往返回的句柄里写。被换成符号链接或文件的 `worktrees` 在解析时就得 `NotDirectory`，写不到别处。不这样做的后果，re-review r2 实测过：指向 `<main>/.git/worktrees` 的符号链接让第 2 代的管理目录建进了主 checkout，主 checkout 的 `git worktree list` 列出了它，换入之后工作区里看得见 `origin`。错误时序（G9 的例外）：第 2、3 步里 rooted 写函数（`mkdir_rooted`、`create_file_rooted`）在写之前的拒绝（`NotDirectory`：路径上有符号链接或文件；`Exists`：名字已被占）原样返回，例外只有这些；其余失败都是 `Unknown`，3.1 的 `open_root`、3.3 的核对（重新打开 `tree_dir` 失败也在内）与读回都不在例外里（breaker 裁定，re-review r5 的 I-1）。`move_workspace` 时，第 4 步写 gitfile 之前这次调用写下的只有未被引用的下一代管理目录与移动暂存里的临时 index，正是 module-rulings "Error timing, refined" 所说的内部暂存：返回类型化的码时它们留在原地，由调用方退役再以这个码拒绝（session 退役 `S` 与 `worktrees/<n>`，意图记录关闭为 `refused`，从不删除。`worktrees/<n>` 被换成了符号链接时 session 退役不了它（store 的 `retire_dir` 拒绝符号链接），也不把它留在 `worktrees/` 里：git 自己的维护（`git worktree prune`、`git gc`，以及普通 commit 触发的自动维护）会跟随留在那里的链接、清空它的目标（re-review r5 的 I-2 实测，git 2.54：指向一份主 checkout `.git` 副本的链接让那个 `.git` 空了）。session 在把意图关闭为 `refused` 的同时，用 `apfs::rename_rooted`（根是影子仓库目录，只挪链接本身、从不跟随）把这个条目挪到 `<shadow>/strata-quarantine/<n>-<nonce>`（目录不存在时经 `apfs::mkdir_rooted` 建），move 的结果在 detail 里点名隔离路径，daemon 把它当告警交给人；改名失败则结果是 `Unknown`（链接可能还在），同样告警。`rm` 退役整个影子仓库之后由 `apfs::remove_tree_rooted` 连同隔离区删掉，它什么类型的条目都删、从不跟随（module-rulings "A symlinked `worktrees/<n>` is quarantined, never kept"，取代 fix round 5 的 (b) 写法）。gitio 自己的行为不变：attach 照旧在写之前以 `NotDirectory` 拒绝）。`new` 时 `tree_dir` 就是工作区，session 把 `create_session` 之后的任何失败都当现场（Creating → Unknown）。gitio 都不删。
3. index（re-review r3 与 fix round 4 的裁定）：git 不再按路径往 A 里写任何东西。下面四小步记作 3.1–3.4。
   1. 临时目录：`apfs::mkdir_rooted(tree_dir, ".git", 0o700, false)`。名字就是第 4 步 gitfile 的名字：第 1 步已确认它不存在，tree 里也不会有这个名字的条目（§5.4 拒绝 `.git` 分量），所以它碰不上任何导出或投影进来的文件 [我的决定，§9]。`move_workspace` 时 `tree_dir` 是调用方的移动暂存（session 的 `.strata-tmp-move-*`），`new` 时是还没交给 agent 的工作区；agent 的 git 从不往这里写。建好之后拿 `R = apfs::open_root(tree_dir)`，记下 `fstat(R)` 的 (dev, ino, ctime)，供 3.3 的核对（re-review r4 提的缓解，fix round 5 的裁定）。
   2. `GIT_INDEX_FILE=<tree_dir>/.git/index git -c core.splitIndex=false --git-dir=<shadow> --work-tree=<tree_dir> read-tree --index-output=<tree_dir>/.git/index <tree>`，再在 `tree_dir` 里跑同样参数的 `update-index -q --refresh`（`update-index` 没有 `--index-output`），把 stat 信息填上，工作区里第一次 `git status` 就不用重读所有文件。`--index-output` 与 `GIT_INDEX_FILE` 是同一个路径：read-tree 经锁文件加改名在这个路径上写出新 index，那里若被放了一个符号链接，链接被换掉，从不经它写（breaker 裁定；re-review r5 实测：链接指向 `<main>/.git/index` 时，主 checkout 的 index 逐字节、连 inode 都不变，主 checkout 里也不留锁）。`--git-dir` 是 bare 的影子仓库本身，git 只读它：对象经 alternates，过滤器、属性与 `core.autocrlf` 这类键取影子仓库复制来的配置，与工作区里的 git 一样。按 §5.1，这两个命令不跑钩子、不起 fsmonitor，各有 600 s 超时。用户级 `core.splitIndex = true` 不拆这个私有 index（实测），仍显式关掉：副本只有一个文件，这一点不靠 git 的内部规则。`-q` 让内容有差异的文件不导致失败（实测退出 0）；`--refresh` 会对 stat 对不上的文件跑 clean 过滤器，与 `git status` 相同。
   3. 复制。先核对暂存没被换过：`fstat(R)` 的 ctime 变了，或再 `apfs::open_root(tree_dir)` 一次失败（任何码）或得到的 (dev, ino) 与 R 的不同 → `Unknown`（`detail.reason = "staging-changed"`），什么都不复制。重新打开失败时，它的码从不原样返回（breaker 裁定，re-review r5 的 I-1）：这个核对就是为抓暂存被换设的，打不开是被换过最强的证据。re-review r5 实测（git 2.54）：3.2 之前把会话目录挪开、换上一个同名目录，其中 `<tree_dir 的名字>` 是指向主 checkout 工作树根的符号链接，git 经它重写了主 checkout 的 index，R 的 ctime 不变，重新打开得 `NotDirectory`；照类型化的拒绝返回的话，session 会关闭意图、退役暂存，没有告警。APFS 改名一个条目时改它所在目录的 ctime，被改名的目录自己的 ctime 也变（re-review r4 实测），所以 3.2 期间 `tree_dir/.git` 或 `tree_dir` 本身被换走，哪怕又换了回来，都在这里拦下；只看 strata 自己的暂存，从不看主 checkout，主 checkout 里正当的 git 碰不到它，代价是一次 `open_root` 加两次 `fstat`。拦不下的与会误报的见 §9 的"活影子仓库的写入清单"一行。核对过了，`apfs::open_rooted_file(tree_dir, ".git/index")` 读出，`apfs::create_file_rooted(shadow, worktrees/<n>/index, 0o644)` 写进返回的句柄，再把副本的 mtime 设成临时 index 的 mtime（`File::set_modified`）[我的决定，§9]：git 按 index 文件的 mtime 判定 racy 条目，副本因此与 git 写出的那份等价。实测：第 2 步之后把 `worktrees/<n>` 换成指向 `<main>/.git` 的符号链接，复制在写之前得到 `NotDirectory`，主 checkout 的 index 逐字节不变；旧的做法（`--git-dir=A`、`GIT_INDEX_FILE=A/index`）在同样的替换下经那个链接重写了主 checkout 的 index，人暂存的文件变成了 Untracked（re-review r3 的复现，fix round 4 在 git 2.54 上又复现了一次）。读临时 index 时 rooted 读被拒（`tree_dir/.git/index` 被换成了符号链接、FIFO 这类东西，或核对之后 `tree_dir/.git` 又被换掉）→ `Unknown`（`detail.reason = "index-read-back-refused"`）：git 可能已经经那个链接写到了别处——read-tree 带 `--index-output` 不经它写，其后的 refresh 写 index 时却经锁文件跟随符号链接（实测）。这两个理由（`staging-changed` 与 `index-read-back-refused`）是 attach 在 git 跑完之后、表示 git 可能写到了暂存之外的仅有的 `Unknown`：session 照它第 13 步里 attach 失败的常规处理，daemon（T13）把它当告警交给人，点名 `<main>/.git/index`（module-rulings "An attach that may have written outside its staging is raised to the human"；§9）。3.2 本身失败或超时时，先做同一个 3.3 核对再返回：核对失败（ctime 变了、重新打开失败、或 (dev, ino) 不同）→ `Unknown`（`staging-changed`）；核对通过 → 3.2 自己的 `Unknown`（`timeout` 或 git 的退出码）。这样 git 跑过之后，这两个理由盖住核对过的路；核对盖不住的无声剩余见 §9 的"活影子仓库的写入清单"一行。
   4. 删临时：`apfs::remove_rooted(tree_dir, ".git/index", RemoveKind::File)`，再 `remove_rooted(tree_dir, ".git", RemoveKind::EmptyDir)`（Addendum 7：删自己建的空目录；同 §5.5 第 7 步）。
4. `<tree_dir>/.git`：字节恰好是 `gitdir: `、`Shadow::dir()`、`/worktrees/`、n 的十进制写法（没有前导 0）与一个 `\n`，末尾不带 `/`，即 `git worktree add` 写 gitfile 的形式（fix round 5 的裁定；re-review r4 increment）。`Shadow::dir()` 是 realpath，这个路径不经任何符号链接。经 `apfs::create_file_rooted(tree_dir, ".git", 0o644)` 新建，往它返回的句柄里写：`O_CREAT|O_EXCL|O_NOFOLLOW`，不跟随符号链接，从不覆盖（module-rulings Addendum 8）。从这一步起任何失败都是 `Unknown`：gitfile 一写下，管理目录就被引用了（Addendum 3：活的 gitfile 与它指向的管理目录都算可见）。`new` 时 `tree_dir` 就是工作区，第 3 步的临时目录与这一步的 gitfile 都写在工作区里；`move_workspace` 时它们写在临时树里，随调用方的交换才变得可见（review N4）。
5. 持久化：A、它里面的文件与 index（经 `apfs::open_rooted(shadow, worktrees/<n>/…)` 打开）、`.git`（第 4 步的句柄）逐个 `fsync`，再一次 `F_FULLFSYNC`。返回 A，与 gitfile 里写的是同一个路径。
6. 原子换入是调用方的事：`move_workspace` 把整棵 `tree_dir` 交换到 `workspace`，新的 gitfile 跟着目录过去，git 的 HEAD 与 index 随之一起换（session.md @d296c12 §5.7）。目录改名不改文件的 inode 与 ctime，第 3 步填好的 stat 信息在交换之后仍然有效（实测：在临时树里建好、改名到工作区路径之后，`git status --porcelain` 为空）。旧一代的管理目录由调用方退役（§1）。

结果（实测）：HEAD detached、没有 upstream；`git status --porcelain` 为空，`node_modules/` 与只靠 `info/exclude` 忽略的 `.env` 都不出现；`git log` 经 alternates 读到主 checkout 的历史；`git rev-parse --git-common-dir` 是影子仓库；agent 的 commit 对象、`git stash`、分支与 tag 都只在影子仓库里，主 checkout 的 `for-each-ref` 不变；`git push` 被影子仓库的 `pre-push` 拒绝；主 checkout 的 `git worktree list`、index、`hooks/` 都不变。fix round 4 的做法同样实测过（git 2.54）：新树的 `git status --porcelain` 为空，`git rev-parse --git-common-dir` 是影子仓库，git 没有往影子仓库里写任何文件。复杂度 O(文件数)：一次 read-tree 加一次 refresh 的 stat，外加一次 index 的复制。

**`new` 之后 strata 对活影子仓库的写**（module-rulings 的 Addendum 8 clarification；re-review r3 与 fix round 4 要的清单）。`new` 之后影子仓库是 agent 的 git 写的地方（ref、对象、index、管理文件、`hooks/`、config）。strata 在那以后起的、碰到影子仓库的操作逐一如下：

| 操作 | 何时 | 往影子仓库里写什么 | 写法；路径被换成符号链接时 |
| --- | --- | --- | --- |
| `attach_worktree` 第 2、3 步 | 每次 `move_workspace` | `worktrees/`（不存在时）、`worktrees/<n>/` 与其中的 `commondir`、`gitdir`、`locked`、`HEAD`、`index` | gitio 自己经 apfs 的 rooted 函数写，根是影子仓库目录；`index` 是 `tree_dir/.git/index` 的副本。被换上的符号链接在写之前就得 `NotDirectory`，写不到影子仓库外 |
| `attach_worktree` 第 3 步的 `read-tree`、`update-index --refresh` | 同上 | git 自己什么都不写：`--git-dir` 是影子仓库，git 只读它，index 在 `tree_dir/.git/` 下。refresh 跑的 clean 过滤器以影子仓库为 `GIT_DIR`：配了 git-lfs 时它按路径往影子仓库的 `lfs/` 下写对象缓存（re-review r4 的 N3；导出时写的是主 checkout 的那份，§5.5 第 4 步），见下面剩下的暴露 (2)。没有 §5.1 关掉的两样时，git 会跑影子仓库 `hooks/` 里的 `post-index-change`，还会在影子仓库里建 `fsmonitor--daemon.ipc`（用户级 `core.fsmonitor = true` 时），两样都实测过 | 影子仓库整个被换成指向别处的链接时，git 读的是链接指向的仓库，仍然只写 `tree_dir/.git/` 下的临时 index |
| `attach_worktree` 第 5 步 | 同上 | 什么都不写 | `open_rooted` 拿只读句柄再 `fsync` |
| `shadow_drift`（三次子进程）、`is_ignored` 的全局排除文件查询、`read_index` 与 `is_ignored` 的直接读 | status、overlay | 什么都不写 | 只读；读影子仓库 config 的子进程有 10 s 超时（§5.1），直接读都经 `open_rooted_file`，放进来的 FIFO 不会挂住（G11） |
| ref、reflog、对象、config | — | 一个都不写：pin ref 在主 checkout（§4.1），`update_ref`、`delete_ref` 只对主 checkout 的 common dir；`stage`、`commit_tree` 写主对象库；gitio 没有修影子仓库的函数 | "影子仓库的 `refs/` 被换成符号链接，把一次 ref 更新引进主 checkout"这一类因此不会发生。T13 若决定重新钉住 hooksPath（module-rulings 的 T13 条），gitio 另加一个函数，同样经 rooted 函数、守 git 自己的 `config.lock` 协议：`create_file_rooted(shadow, config.lock)`（`O_EXCL` 就是锁）写好新内容，`rename_rooted(…, RenameMode::Swap)` 与 `config` 对换，再 `remove_rooted` 掉换下来的旧内容（apfs 没有覆盖式的改名，re-review r4）。它只修好被改或被删的钉子，include、`extensions.*`、`config.worktree`、gitfile 或换掉的 hooks 目录造成的失效修不了 |

剩下的暴露都要有意的篡改才够得着，按 pre-push 守卫已经裁定的威胁模型（review N1：守卫不是安全边界），最坏情形与之后 status 显示什么记在 §9 的"活影子仓库的写入清单"一行：临时 index 仍由 git 按路径写在 `tree_dir/.git/` 下（3.3 的核对拦下 refresh 期间对暂存本身的替换，也拦下换了上级而没换回的，拦不下换上级又换回的）；refresh 跑影子仓库 config 里的过滤器命令（配了 git-lfs 时还按路径写影子仓库的 `lfs/`）；影子仓库 config 的 include 可以指向 FIFO。

**`attach_staging(staging, gitdir, head)`**（publish 专用）：

1. 拒绝（G9）：`tree = tree_of(head)`，它的码原样返回；`staging` 不存在 → `NoSuchPath`，不是目录（lstat，符号链接也算）→ `NotDirectory`；`staging` 或 `gitdir` 不是绝对路径，或落在主 checkout 的工作树或 common dir 里 → `InvalidInput`；`<staging>/.git` 或 `gitdir` 已存在 → `Exists`。
2. 建私有 git 目录 `gitdir`：`commondir`（主 checkout common dir 的绝对路径）、`gitdir`（`<staging>/.git` 的绝对路径）、`HEAD`（detached 在 `head`）。从这一步起，任何失败都是 `Unknown`。
3. index：`GIT_INDEX_FILE=<gitdir>/index git --git-dir=<gitdir> --work-tree=<staging> read-tree <tree>`，再同样参数的 `update-index -q --refresh`（钩子与 fsmonitor 按 §5.1 关掉）。与 `attach_worktree` 不同，git 直接按路径写这个 index：私有 git 目录归 publish，从不交给 agent。
4. `<staging>/.git`：一行 `gitdir: <gitdir>`，经 `apfs::create_file_rooted(staging, ".git", 0o644)` 新建，同 `attach_worktree` 第 4 步；持久化同第 5 步。

不用 `git worktree add`：它要写主 checkout 的 `<common>/worktrees/`（不变量 2）。代价是主 checkout 的 `git gc` 看不到这个私有目录的 HEAD 与 index：`stage` 写进主对象库的 blob 在 `commit_tree` 与 `update_ref` 之前没有 ref 引用，靠 git 对新松散对象的宽限期（`gc.pruneExpire`，缺省两周）保住，而一次发布只需要几秒到几分钟。结果（实测）：在 staging 里 `update-index`、`write-tree`、`commit-tree` 写出的对象都在主对象库里，`refs/strata/publish/*` 可以指向它们；主 checkout 的 `git worktree list` 不列出它，index 与 `git status` 不变。复杂度 O(文件数)。

**`worktree_gitdir(tree_dir)`**（只读，不需要锁；session 的上游要求，session.md @cec336b §2；module-rulings 的 "gitio requirements from session r2"）：回答一棵树根下的 gitfile 接在哪个 git 目录上。`resolve_failed_move` 用它核对失败的移动留下的两棵树各接在哪一代的管理目录上（session.md @d296c12 §5.9 第 3 步），gitfile 的格式因此只有 gitio 一处解析。

1. `tree_dir` 不是绝对路径 → `InvalidInput`。
2. `apfs::open_rooted_file(tree_dir, ".git")`：根以 `O_NOFOLLOW` 打开，`.git` 也不跟随符号链接，FIFO 以 `O_NONBLOCK` 打开、不会挂住（apfs §3）。`NoSuchPath`（`tree_dir` 或 `.git` 不存在）→ `Ok(None)`：两种都是"这里没有 gitfile"，一次调用就答得出，不必先另查 `tree_dir` 再查 `.git`（两次查询之间 `tree_dir` 可能被挪走）[我的决定，§9]。apfs 的其余拒绝原样返回 [我的决定，§9]：`tree_dir` 是符号链接或不是目录 → `NotDirectory`；`.git` 是目录 → `IsDirectory`，主 checkout 是这样，attach 中断在第 3 步时临时树也是这样（放临时 index 的目录，gitfile 还没写）；`.git` 是符号链接、FIFO、socket 或设备 → `NotFile`，从不读链接指向的东西。
3. 读：上限 N = `8 + apfs::PATH_MAX_BYTES + 2` 字节（`gitdir: `、路径、CRLF），实际读到 N + 1 字节为止：读满 N + 1 字节就是长过上限（第 4 步的 `bad-gitfile`），不靠 `fstat` 的大小（re-review r4 increment）；读错误经 `apfs::classify_io_error(…, IoSide::Read, …)` 归类。
4. 解析同 §5.7 第 1 步，只用 tgrep 的字节解析（去掉 `gitdir: ` 前缀，再 `metadata_path`）；`read_git_dir_using` 本身不用，它经 `fs::metadata(.git)` 跟随符号链接（re-review r4 increment）：必须以 `gitdir: ` 开头，只去掉末尾的 `\n`、`\r`，路径两侧的空格与非 UTF-8 字节原样保留，相对路径照 git 接在 `tree_dir` 上（不规整、不解析符号链接）。不以 `gitdir: ` 开头、去掉行尾之后为空 → `InvalidInput`（`detail.reason = "bad-gitfile"`）；另加两条 [我的决定，§9]：含 NUL 字节，或长过第 3 步的上限（这样长的路径打不开）→ 同样是 `bad-gitfile`。
5. 返回 `Some(路径)`。不核对它指向的目录在不在、属于哪个仓库：那是调用方的核对（session 把它与自己拼出的管理目录路径比较；attach 写的字节形式见 `attach_worktree` 第 4 步）。从不写。

复杂度 O(1)：一串 `openat` 加一次至多 1 KiB 的读。

### 5.7 `read_index(worktree)`

1. 找 git 目录，读法同 `worktree_gitdir`（§5.6 末段；G11）。根是 worktree 参数本身，严格：根与其下的分量都不跟随符号链接。只有 worktree 与传给 `open` 的 `path` 逐字节相同时（主 checkout），才改用 `open` 算出的 `realpath(root)`（breaker 裁定，re-review r5 的 M-1）：按 module-rulings "Repo paths in config are realpaths"，配置的 `path` 就是它的 realpath；`open` 本身仍接受符号链接的 `path`，以同一个 `path` 调用照常工作（§7 的 `rule_and_index_reads_never_hang_on_non_regular_files`）。原来先取 worktree 的 realpath 再比，agent 把工作区换成指向主 checkout 的符号链接时，它会被当成主 checkout 来答（主 checkout 的 index 与规则）；现在严格的根读不了它的 gitfile，结果是 `Unknown`（§6）。`worktree/.git` 是 gitfile 就读出 `gitdir: <路径>`（只照抄 tgrep 的字节解析：去掉 `gitdir: ` 前缀再 `metadata_path`，只去掉 git 写入的行尾，路径两侧的空格保留；不用 `read_git_dir_using`，它经 `fs::metadata` 跟随 `.git` 的符号链接）；读出的路径（相对的先接在 worktree 上）按词法规整之后才当根用，与 `commondir` 一样：去掉末尾的 `/`、`.` 分量与重复的 `/`，`..` 按词法退一级（re-review r5 的跨模块项，breaker 裁定）。git 接受 `gitdir: <shadow>/worktrees/1/` 这样带 `/` 的写法，apfs 却拒绝不规整的根（module-rulings "Rooted roots are canonical"，`root_not_canonical`），不规整的话这样一个 gitfile 得到的就是 `InvalidInput`，不是 §6 的 `Unknown`。`.git` 不存在 → `NoSuchPath`；是目录（apfs 的 `IsDirectory`）就用它（主 checkout）；是符号链接、FIFO 这类 → `Unknown`（`detail`：路径与 apfs 的码）[我的决定，§9]：git 跟随符号链接的 `.git`，gitio 不跟随。common dir 取 `<git 目录>/commondir`（以 git 目录为根读），相对路径相对 git 目录按词法规整（工作区的 `../..` 由此落到影子仓库）；没有这个文件时就是 git 目录本身。此后 git 目录里的文件都以 common dir 为根读（G11）：工作区的 index 是 `open_rooted_file(<影子仓库>, "worktrees/<n>/index")`，`worktrees/<n>` 被换成符号链接时读不到它指向的东西，结果是 `Unknown`。
2. 归属 [我的决定]：common dir 的 realpath 等于本仓库的（主 checkout、人自己的 linked worktree、publish 的 staging 树），或者它是本仓库的影子仓库（`strata.shadow = true`，alternates 指向本仓库的 `objects`，两者经 `open_rooted_file(<common>, …)` 读，结论按影子仓库路径缓存）→ 继续；否则 → `InvalidInput`。这一步不推 `RepoId`：工作区的身份由 session 经清单给出（session.md §4.4），gitio 只核对它借的是不是本仓库的对象库。
3. index 文件与第 6 步读过的 config 文件的 (size, mtime 纳秒, inode) 都与缓存相同 → 返回缓存的快照。
4. 读 `<git 目录>/index`（以 common dir 为根，G11；不是普通文件 → `Unknown`）。文件不存在 → `Unknown`，`detail.reason = "no-index"`（module-rulings Addendum 2 的 T7 条；review M7）：gitio 给工作区与 staging 建的 index 总是在的，它不见了是异常；当成"什么都没跟踪"，ignore 规则就会藏起已跟踪的文件（3f `a_directory_without_git_reports_nothing` 一行）。解析照抄 tgrep 的 `parse_index_with`：版本 2 至 4；v4 的前缀压缩路径；扩展区与结尾校验和；split index 经同目录的 `sharedindex.<hash>` 合并，按 EWAH 位图删除与替换；头部声明的条目数按文件长度夹取，伪造的头部不能让进程按 40 亿条预留内存。读不完整一律返回 `Unknown`（`detail.reason`），不返回少了条目的集合，因为少一条就少一个"已跟踪"豁免。带 sparse 目录条目（`sdir` 扩展）的 index → `Unsupported` [我的决定]；gitio 自己建的 index 总是完整的。
5. 每条路径：不是 UTF-8 → `NotText`；经 `RepoPath::try_from`（NFC）后进 `tracked`；ignorecase 时它的 ASCII 小写形进 `folded`。
6. `ignorecase`：读 `<common>/config` 的 `core.ignorecase`（两份 config 都经 `open_rooted_file`，G11：本仓库 common dir 里的 `config`、`config.worktree` 是人写的文件，先 `realpath`；影子仓库与管理目录里的不跟随符号链接）；`extensions.worktreeConfig` 为真时，`<git 目录>/config.worktree` 覆盖它，照抄 tgrep 的 `config_bool` 与 `ignores_case`。`config_bool` 改成照 git 2.54 的 config 解析器读（`config.c` 的 `parse_file`、`get_value`、`parse_value`，布尔值照 `git_parse_maybe_bool`）[我的决定，§9]：agent 的 git 命令与手改都会改写影子仓库的 config，照抄的读法不认引号，`ignorecase = "true"` 会读不出来（review B1）。按字节读整个文件。下面每条都在 git 2.54 上实测过（`git config --file <f> --type=bool --get`；§7 的 `config_values_read_like_git` 与 git 对拍；review B1、M2）；规则说不准 git 会怎样的形式一律 → `Unknown`（`detail.reason = "bad-config"`），从不猜。
   - **文件**：开头恰好一个 UTF-8 BOM（`EF BB BF`）跳过，只有它的前一两个字节是坏行。`\r\n` 当作 `\n`；单独的 `\r` 不是行尾，按下面的空白规则处理。文件末尾没有换行等于有。文件里有 NUL 字节 → `Unknown`（`bad-config`），哪怕 git 接受 [我的决定，§9]：git 对 NUL 的处理随位置而变（注释里忽略，值里截断，名字里是坏行），读法不去复刻。
   - **空白**有两类：解析器用 git 自己的空白（空格、制表符、`\r`、`\n`，不含 `\v`、`\f`）；整数前面的空白用 C 库的（C locale 下多出 `\v`、`\f`；UTF-8 locale 下 macOS 的 C 库还认 `0xA0` 这样的字节，git 随 locale 把 `\xa01` 当 true，读法照 C locale 不认，报 `Unknown`，是安全的一侧，re-review r4）。
   - **行**：开头的空白跳过；`#`、`;` 开始注释，到行尾为止（注释里的 `\` 不续行）；`[` 开始段头；ASCII 字母开始一个键；别的字符（含 `\v`、`\f`）都是坏行。
   - **段头**：`[` 之后是零个或多个 `[A-Za-z0-9.-]`（段名，不分大小写），然后二选一：紧跟 `]`，这时段名不能为空（`[]` 是坏行）；或者一个以上的空白、一个 `"`、子段名（`\` 让下一个字符原样进名字，换行除外）、一个 `"`，紧跟着必须是 `]`，这时段名可以为空（`[ "x"]` git 照样接受，实测）。段名里的别的字符（`[co_re]`、`[core""]`）、空白之后不是 `"`（`[ core]`、`[\tcore]`）、`]` 前的空白（`[core "x" ]`）、到行尾还没闭合，都是坏行。`]` 之后同一行照"行"的规则接着读：又一个段头、一个键或注释都行，`[core]]` 是坏行。只有段名恰好是 `core`、没有子段的段头才开始 `core` 节：`[core "x"]`、`[core ""]`、`[core.x]`、`[core.]`、`[ "x"]` 都不是；第一个段头之前的键不属于任何节。
   - **键**：ASCII 字母开头，后面是字母、数字或 `-`，不分大小写。名字之后只跳过空格与制表符，然后必须是行尾（没有 `=`，值是"没有值"）或 `=`；别的都是坏行：`ignore_case = 1`、`9key = 1`、`ignorecase # c`、`ignorecase ; c`、名字后的 `\v` 与 `\f`，都让整个文件作废，不论这一行在哪一节、在要读的键之前还是之后。
   - **值**：`=` 之后，引号外、不是转义出来的空白，在开头跳过，中间的保留；末尾的一段只在它之后直到逻辑行尾（续行接上的行也算）只剩这样的空白或注释时去掉，之后还有引号（哪怕是空的 `""`）或任何 `\` 序列（含续行）时留在值里（re-review r4 的 N1，git 2.54 实测：`1 ""`、`"1" ""`、`1<TAB>""`、`1 "" # c`，以及 `1 \` 续行到文件末尾、到只有空白的行或到一行注释，值都是 `1` 加那个空白，不是布尔值；`1 # c`、`1` 加两个空格、`1\` 续行到只有空白的行，值是 `1`）；`"` 开关引号段，引号本身不进值，可以出现在值的任何位置；引号外的 `#`、`;` 开始注释；转义在引号内外都有效：`\"`、`\\`、`\n`、`\t`、`\b`，`\` 紧跟换行是续行（`\` 在文件末尾也算），转义出来的字符算内容、不被去掉；别的转义、到行尾还没闭合的引号，是坏行。
   - **取值**：坏行出现在文件里任何位置，git 都拒绝整个文件 → `Unknown`（`bad-config`）。`core` 节的 `ignorecase` 每出现一次都必须是 git 认的布尔值，任何一次不是 → `Unknown`（`bad-config`），git 读到它就报错退出；都是时以最后一次为准；一次都没有是 false，与 git 的缺省相同。布尔值：没有值是 true；空值（含 `""`）是 false；`true`、`yes`、`on` 与 `false`、`no`、`off` 不分大小写；否则当整数：跳过开头的空白，可选的 `+` 或 `-`，按 `strtoimax` 以 0 为基读最长的数字串（`0x`、`0X` 开头是十六进制，`0` 开头是八进制，否则十进制），一个数字也没有、或超出 64 位，都不是布尔值；数字串之后只能什么都没有，或恰好一个 `k`、`m`、`g`（不分大小写，乘 2^10、2^20、2^30）；乘积必须落在 32 位有符号整数里，git 用的是 `int`；非 0 是 true。所以 `1K`、`0x7fffffff`、`-2g`、`"\t1"` 是 true，`0k`、`-0` 是 false，`2g`、`3000000000`、`2097152k`、`0x80000000`、`08`、`0x`、`1kb`、`1e3`、`"1 "` 都不是布尔值。
   - **include**：`include.path`、`includeIf.<条件>.path` 只当普通的键，不跟随 [我的决定，§9]：跟随就要读 agent 指定的任意文件（可以是 FIFO），`includeIf` 的条件还要照 git 的上下文求值。影子仓库 config 里的 include 由 `shadow_drift` 报成漂移（§5.6），status 因此看得出这个读法可能与 git 不同。22 个仓库的仓库级 config 都没有 include（2026-10-22 只读核对）。

   只读仓库级 config，不读全局配置：git 在大小写不敏感的卷上 clone 或 init 时，总把 `core.ignorecase = true` 写进仓库 config，22 个仓库都是这样。影子仓库从不复制 `extensions.*`，所以工作区只看影子仓库 config 里复制来的值；agent 自己打开 `extensions.worktreeConfig` 时，`config.worktree` 照上面的规则覆盖它。
7. 返回快照：之后 index 再变，它也不变。

复杂度 O(index 字节数)；tool 约 4K 条，毫秒级。实测主 checkout 的 index 都是 v2，没有 split index。

### 5.8 `is_ignored(worktree, path)` 与 `is_ignored_in_export(dir, tree, path)`

1. `info = read_index(worktree)`，走缓存；`info.is_tracked(path)` 为真 → `false`（G5）。ignorecase 时这一查不区分 ASCII 大小写，与 `git status` 的判定相同。这里不照 `git check-ignore`：它查 index 时区分大小写，实测 index 里是 `tracked.LOG` 时它仍报 `tracked.log` 被忽略，而在大小写不敏感的磁盘上那是同一个文件。区分大小写的仓库里，被规则逐字命中的已跟踪文件同样不被忽略。这一点与 tgrep 的遍历器不同，design §3 已经定了用"已跟踪永不 ignore"。
2. `lstat(worktree/path)` 判断是不是目录。不存在的按文件算；符号链接也按文件算，不跟随。
3. 父目录：从 `path` 的第一级目录到它的父目录，逐级以目录身份判定，任何一级被排除 → `true`。git 的规则是父目录被排除的文件不能再包含回来；实测 `build/` 被排除时 `!build/keep.c` 不生效。
4. 路径本身，按优先级从高到低：`path` 所在目录的 `.gitignore`，逐级向上直到 worktree 根的 `.gitignore`，common dir 的 `info/exclude`，全局排除文件。第一份有命中的文件决定结果；同一份文件里，最后一条命中的模式决定结果；`!` 模式表示不忽略。
   - 工作区的 common dir 是影子仓库，读到的是建仓时镜像的那份 `info/exclude`。会话存活期间主 checkout 改了它，工作区的答案不变，与工作区里的 git 一致；`shadow_drift` 报出差别（module-rulings Addendum 2）。
   - 只读 worktree 根及以下的 `.gitignore`。会话根 `<root>/sessions/<session-id>/` 以及更上层的 `.gitignore` 不起作用。
   - 被规则自己命中的 `.gitignore` 照样贡献规则；一行坏模式（如 `[`）只跳过这一行，与 git 相同。
   - 全局排除文件（module-rulings T5 补充条目）按 common dir 解析（§4.3）：config.md §4.3 列出的九个仓库，`.claude/settings.local.json` 只靠 `~/.config/git/ignore` 被忽略，在主 checkout 与工作区里都一样。
   - 读法（G11）：`.gitignore` 以 worktree 为根；工作区的 `info/exclude` 在影子仓库里，以影子仓库为根；主 checkout、人自己的 linked worktree 与 staging 树的 `info/exclude` 在本仓库的 common dir 里，与全局排除文件一样先 `realpath` 再以所在目录为根，git 跟随这两处的符号链接、悬空的当作没有（实测）；都经 `apfs::open_rooted_file`。不存在 → 没有规则。工作树里的 `.gitignore` 是符号链接或目录 → 当作没有，与 git 2.54 相同（实测：符号链接的那份 git 警告一句后不读，目录静默跳过；apfs 给 `NotFile` 时用 `apfs::attrs_of` 分辨符号链接与 FIFO）[我的决定，§9]。其余不是普通文件的规则文件（FIFO、socket、设备，或符号链接指向它们；影子仓库里符号链接的 `info/exclude`）→ `Unknown`（`detail.path`）：FIFO 让 git 一直挂住（实测），gitio 不挂也不猜；影子仓库是 agent 写得到的地方，那里的符号链接 git 跟随，gitio 不跟随（§9）。
5. ignorecase 为真时，模式按 ASCII 不区分大小写匹配，否则逐字节匹配。git 的大小写折叠只管 ASCII，实测模式 `É.txt` 不命中 `é.txt`。
6. 目录路径按规则回答，目录本身从不算已跟踪。遍历的调用方遇到被忽略的目录、而 `info.tracked_under(dir)` 为真时，仍要进去，因为里面的已跟踪文件不能丢。
7. 增量点查与全量对账都调这一个函数，两者的判定因此一致（3f 的点查匹配器一行，目标 overlay）。

复杂度：每次调用 O(路径深度) 次 stat 加一次匹配，规则文件只在 stat 变化时重读。tool 源码树约 4K 个文件，在 design §8"全量对账 < 3 s"里占几十毫秒，在"checkpoint 改 50 个文件 < 200 ms"里占几毫秒。

**`is_ignored_in_export(dir, tree, path)`**（session 的上游要求，session.md @853fdf8 §2：工作区还不存在时判显式种子；module-rulings 的 Requirements rounds 条）：回答"把 `tree` 检出成本仓库的工作区之后，git 会不会忽略 `path`"。`dir` 是这个 tree 的一份导出，session 传的是基线 generation 目录（源码导出加依赖集，工作区就是它的克隆）。只读，不需要锁。

1. 拒绝，什么都不写：`dir` 不是绝对路径 → `InvalidInput`；不存在 → `NoSuchPath`；不是目录（lstat，符号链接也算）→ `NotDirectory`；`<dir>/.git` 存在（lstat）→ `InvalidInput`（`detail.reason = "has-git"`），有 `.git` 的是工作树，用 `is_ignored`。`tree` 的路径表按 §5.4 取（`tree_contains` 用的那份，在缓存里就不起子进程）；`tree` 不能当基线 → `tree_of` 的码原样返回。
2. `ignorecase`：主 checkout `<common>/config` 的 `core.ignorecase`，用 §5.7 第 6 步的读法，按 config 的 stat 缓存（§4.3）。这正是 `ensure_shadow` 复制给影子仓库的值（它按 `--local` 复制，不含 `config.worktree`）。
3. 已跟踪：`path` 是路径表里的文件或符号链接条目（ignorecase 时按 ASCII 不区分大小写）→ `false`。检出之后 index 里正是这些条目，所以这与 `is_ignored` 第 1 步的 `is_tracked` 是同一个判定；路径表里的目录不算已跟踪，与 `is_ignored` 第 6 步一样按规则回答 [我的决定，§9]。对文件，这就是 fix round 4 的裁定写的 `tree_contains(tree, path)`；`tree_contains` 对目录也答"在"，照搬到这里，两个函数对同一个目录就会给出不同的答案。session 先用 `tree_contains` 拒掉被跟踪的种子，只拿不在 tree 里的路径来问，两种写法对它没有差别。
4. 其余同 `is_ignored` 的第 2–6 步，只换规则的来源：`lstat(dir/path)` 判断是不是目录；父目录逐级以目录身份判定；`.gitignore` 从 `path` 所在目录逐级向上读到 `dir` 的根为止，`dir` 之上的不读（读法同 `is_ignored` 第 4 步，根是 `dir`，G11）；然后是主 checkout 的 `<common>/info/exclude`；最后是按主 checkout 的 common dir 解析的全局排除文件（§4.3）。`info/exclude` 与全局排除文件正是 `ensure_shadow` 将要镜像、工作区里的 git 将要读到的那两份，所以只要工作区建好之前主 checkout 不改它们，这个答案就等于工作区建好之后 `is_ignored(workspace, path)` 的答案。用户级 config 以 `includeIf "gitdir:…"` 只给主 checkout 引入的 `core.excludesFile` 在这里生效、在工作区里不生效（§9 的 `includeIf` 一行）。

复杂度同 `is_ignored`：O(路径深度) 次 stat 加一次匹配；路径表在缓存里时，已跟踪的判定是 O(log 文件数)。

### 5.9 `stage(staging, paths)`

这是 `export_tree` 的反方向（module-rulings T5/T10 补充条目）。publish 的用法：`export_tree(B 的 tree, staging)` → `attach_staging(staging, gitdir, B)` → 写入合并后的文件 → `stage(staging, 批次路径)` → `commit_tree` → `update_ref` → `push(staging, …)`。

1. 拒绝（G9）。staging 树的判定（gitfile 与 `commondir` 经 `apfs::open_rooted_file` 读，G11；读不出或不是普通文件也算不满足）：`<staging>/.git` 是 gitfile，它指向的 git 目录不在本仓库的 common dir 之下（人自己的 linked worktree 的 git 目录在 `<common>/worktrees/` 下），而那个目录的 `commondir` 的 realpath 是本仓库的 common dir。主 checkout 本身、人自己的 linked worktree、strata 的工作区（`commondir` 是影子仓库）都不满足 → `InvalidInput`。然后 `paths` 去重、排序，逐条 `lstat`：普通文件或符号链接记为"写"；不存在或是目录记为"删"；FIFO、socket、设备 → `NotFile`。"删"只删 index 里这个路径的文件条目；目录里的新文件是批次里的其他路径，类型转换总是成对出现（module-rulings T10）。
2. 删：`git … update-index --force-remove -z --stdin`，`GIT_INDEX_FILE=<私有 git 目录>/index`。
3. 写：`git … update-index --add --replace -z --stdin`。update-index 按检入方向转换，即 clean 过滤器、eol 与 autocrlf 规范化、`working-tree-encoding` 还原，然后写 blob；`--replace` 让文件与目录冲突的旧条目自动让位。实测 rot13 的 clean 过滤器与 eol 规范化都生效，blob 是检入形态。过滤器取自主 checkout 的仓库级 config（staging 的私有 git 目录读主 common dir 的 config）与用户级 config；用户级 config 里以 `includeIf "gitdir:…"` 只给主 checkout 引入的 `filter.*` 在这里不生效，导出时却生效，两边会不对称 [我的决定：记为限制，§9]。
   - 不用 `git add`：它对目录路径会递归；被忽略的文件要加 `-f` 才收，而 `-f` 碰上目录路径，会把目录里没选的被忽略文件一起收进来。那就是"发布了用户没有选的东西"（design §2 反目标）。update-index 只认逐条给出的文件路径，也不看 `.gitignore`。
4. `git … write-tree` 得到 tree。
5. 结果 = 起点 tree（attach 时的 index）加上批次路径的变化。不在 `paths` 里的路径保持 index 原样，即使磁盘上改过。

**往返稳定**：`export_tree` 导出、内容没改的文件，`stage` 之后得到原来的 blob。实测：带 `eol=crlf`、一对 rot13 smudge/clean 过滤器、符号链接与可执行文件的 tree，全部原样 stage 后 tree OID 不变。过滤器本身不对称时（例如 smudge 加时间戳）不保证，git 自己也不保证。复杂度 O(批次字节数)。

### 5.10 commit、ref 与祖先

- `commit_tree`：`git -c user.useConfigOnly=true commit-tree <tree> -p <parent> -F -`，消息从 stdin 原样传入，不清理注释，带 `LC_ALL=C`。身份只取 git 配置里的 `user.name`、`user.email`（主 checkout 的仓库级 config 与全局 config）：清洗环境去掉了 `GIT_AUTHOR_*`、`GIT_COMMITTER_*`，`user.useConfigOnly` 再让 git 不去猜。实测不加这个参数时，没配身份的 commit-tree 照样成功，用的是由主机名与用户名拼出的身份，publish 就会推出一个假作者；加了之后退出 128，stderr 为 `no email was given and auto-detection is disabled`（只缺名字时为 `no name was given and auto-detection is disabled`），环境里有 `EMAIL` 也一样（review I3）。stderr 含 `auto-detection is disabled` → `InvalidInput`，`detail.reason = "git-identity"`。`commit-tree` 不读 `commit.gpgSign`（git 2.54 的文档只认命令行的 `-S`），所以不会在 daemon 里弹 pinentry。输出的 OID 经校验。只写一个 commit 对象，不动 ref。
- `update_ref`：名字必须以 `refs/strata/` 开头，格式再由 git 自己校验。命令是 `git update-ref -m strata <name> <new> <old 或 40 个 0>`，git 在 ref 锁里做比较并交换，是原子的。失败原话是 `is at <实际> but expected <期望>` 或 `reference already exists`（两种都实测过）→ `HeadMoved`；`detail.actual` 在失败后再 `read_ref` 读一次，不从 stderr 里抠。其他失败 → `Unknown`。session 的 pin ref 用同一个比较并交换：`new` 时 `old = None`，移动时 `Some(旧 head)`（session.md §5.1、§5.6）。
- `delete_ref`：`git update-ref -d <name> <old>`，失败的分类同上。
- `read_ref`：`git rev-parse --verify --quiet --end-of-options <name>^{commit}`，退出 1 → `None`。经 git 读，不直接读 ref 文件或 `packed-refs`。
- `is_ancestor`：`git merge-base --is-ancestor <a> <b>`，退出 0 → `true`，退出 1 → `false`，其他 → `NoSuchCommit` 或 `Unknown`。
- `merge_base`：`git merge-base <a> <b>`，输出 OID → `Some`；退出 1 且没有输出 → `None`。有多个最近公共祖先时取 git 给出的那一个：祖先与合并基由 git 承担，strata 没有自己的提交图（3d 的祖先对拍一行）。浅克隆（`.git/shallow`）截断了历史时，git 的答案可能是 `None`，照实返回。

### 5.11 `push(staging, remote, src_ref, dst_branch, timeout)`

1. 拒绝，返回 `Err`，此时什么都没发出：`staging` 不是 `attach_staging` 接到本仓库的 staging 树（判定同 §5.9 第 1 步）→ `InvalidInput`；`src_ref` 不以 `refs/strata/` 开头，或 `remote`、`dst_branch` 不合法（规则同 §5.3）→ `InvalidInput`；`read_ref(src_ref)` 为 `None` → `NoSuchCommit`。
2. 推送地址：`git remote get-url --push <remote>`，已应用 `pushurl` 与 `pushInsteadOf`。推到地址，不推 remote 名 [我的决定]：推到 remote 名成功后，git 会更新 `refs/remotes/<remote>/<dst_branch>`，推到地址则不会（实测）。22 个仓库的推送地址都是 HTTPS，没设 `pushurl` 或 `insteadOf`。
3. `git push --porcelain --no-follow-tags --end-of-options <地址> <src_ref>:refs/heads/<dst_branch>`，`current_dir` 是 staging 树根，`--git-dir=<staging>/.git --work-tree=<staging>`，子进程另设 `STRATA_PUBLISH=1`（design §6.8 的约定：strata 自己的推送带它）。不带 `--force`、`--force-with-lease`、`+`，也不带 `--no-verify` [我的决定]。钩子从主 checkout 的 common dir（或它配置的 `core.hooksPath`）解析，所以**人自己的 `pre-push` 钩子（若有）照跑**，cwd 是 staging 树根（review M16）：实测钩子看到的 cwd 与 `GIT_WORK_TREE` 是 staging 树，`GIT_DIR` 是它的私有 git 目录。22 个仓库目前都没有非示例钩子。用户级 config 里以 `includeIf "gitdir:…"` 只给主 checkout 引入的凭据、http 与钩子配置，在这次推送里不生效（推送的 git 目录是 staging 的私有目录），第 2 步的 `remote get-url` 却照常读到（它在主 common dir 上跑）[我的决定：记为限制，§9]。超时就是 `timeout`，到时杀进程组。[实现时补的决定]（review-5 N1）第 2 步的地址已经过 `insteadOf` / `pushInsteadOf` 改写，git 对命令行给的地址还会再改写一次，推送就可能去了别处、与调用方记下的地址不符。所以子进程另带 `--config-env=url.<地址>.insteadOf=STRATA_PUSH_URL` 与同样的 `pushInsteadOf`（环境变量 `STRATA_PUSH_URL` 即地址；`--config-env` 按最后一个 `=` 切，地址里可以有 `=`）：一条恒等规则，匹配串是整个地址，是能有的最长匹配。git 取最长匹配、等长取先读到的，命令行的配置最后读，所以只输给 config 里匹配串恰好等于地址、base 不同的规则；名叫这个地址的 remote（`remote.<地址>.url`）也会让 git 推去它的 url。推送之前（`PUSH_BEFORE_SEND` 之后、起子进程之前）用 staging 同样的 config 视图（`config --null --get-regexp`）查这两种，有就 `InvalidInput`（`detail.reason` 为 `url-rewritten` 或 `url-is-remote-name`，另有 `url`），什么都没发。地址为空 → `empty-url`，含换行或 NUL（config 子节放不下）→ `bad-url`，同样什么都没发。（review-6 漏洞 1）[实现时补的决定] git 还从老式文件读 remote：地址是合法的 remote 名（没有 `/`，不是 `.`、`..`）时，common dir 下的 `remotes/<地址>`（`URL: <B>`）或 `branches/<地址>`（`<B>#<分支>`）也让它成了 remote 名，推送去 B。所以这两个文件里任一存在（不论内容、类型）也算 `url-is-remote-name`，发送前拒绝；`push_url_exact` 同样为 false。（review-6 漏洞 2，D25）[实现时补的决定] 上面的检查与推送是两个 git 进程，各自重读 config，检查之后、推送读 config 之前加的规则照样生效。所以检查挪进推送进程本身：每次推送在 staging 的私有 git 目录下建 `strata-push-<nonce>/hooks/pre-push`（守卫钩子，权限 0700），推送子进程的环境另带 `GIT_CONFIG_COUNT=1`、`GIT_CONFIG_KEY_0=core.hooksPath`、`GIT_CONFIG_VALUE_0=<该目录>`（命令行作用域，盖过所有 config 文件；不用 `-c`，见下）。git 在读完自己唯一一次 config、算好改写之后、发送任何对象之前运行 `pre-push`，参数 `$2` 就是它要发送去的地址；守卫把它追加写进同目录的 `seen`，不等于 `STRATA_PUSH_URL` 就退出非零（git 什么都不发），等于就 `exec` 人自己的 `pre-push`（推送之前以 `rev-parse --git-path hooks/pre-push` 在 staging 视图解析，相对路径相对 staging 树根；存在且可执行才跑，经 `STRATA_USER_PRE_PUSH` 传入）。（review-7）[实现时补的决定] `exec` 之前守卫先 `unset` `GIT_CONFIG_COUNT`、`GIT_CONFIG_KEY_0`、`GIT_CONFIG_VALUE_0` 与它自己的 `STRATA_PUSH_GUARD_SEEN`、`STRATA_USER_PRE_PUSH`，所以人的钩子拿到的参数、stdin、cwd 与环境（含 `GIT_CONFIG_PARAMETERS`：`core.fsmonitor=false` 与两条钉住规则）都与 D25 之前相同（唯一例外：守卫是 `sh` 脚本，`sh` 会把继承来的 `PWD` 规整成实际的 cwd，即 staging 树根；review-8），钩子里再起的 git 看到的 `core.hooksPath` 是人自己的（或没有）；`base_command` 本就剥掉继承来的 `GIT_*`，没有别的 `GIT_CONFIG_COUNT` 要还原。若用 `-c`，覆盖会经 `GIT_CONFIG_PARAMETERS` 传给钩子里的 git，嵌套的 `git hook run pre-commit` 就找不到钩子（review-7 实测）。推完读 `seen`：只有一条且不等于地址、又没有这个 ref 的状态行 → `InvalidInput`（`url-rewritten`），什么都没发；多条里有不等的 → `Unknown`；守卫没跑（`seen` 为空）却得到 `Pushed` → `Unknown`（目的地没人担保）。守卫目录推完（含超时）就删。检查之后、起子进程之前另有测试暂停点 `PUSH_AFTER_CHECK`（`pause_push_after_check`）。注意：git 在跑 `pre-push` 之前已经连上了它要推的远端读 ref 广告，所以被改写时会连到 B，但不发对象。daemon 推完仍用 `push_url_exact` 与重读的 `push_url` 再核对一次，作兜底。
4. 归类，只认本 ref 的 porcelain 状态行（`<flag>\t<src>:<dst>\t<摘要>`）：

   | 观察 | 结果 |
   | --- | --- |
   | flag 为空格（快进）、`*`（新分支）、`=`（已是最新） | `Pushed` |
   | flag 为 `!`：`[rejected] (non-fast-forward)`、`[remote rejected] (pre-receive hook declined)`、权限不足等 | `Refused(摘要 + stderr 里的 remote: 行)` |
   | 超时，进程组被杀 | `Unknown` |
   | 没有本 ref 的状态行（断线、远端挂断、认证失败、本地 pre-push 钩子拒绝等），不论退出码 | `Unknown` |
   | flag 为 `+` 或 `-`（gitio 不发这两种推送） | `Unknown` |

   实测依据：非快进得到 `!	…	[rejected] (non-fast-forward)`，退出 1；远端 pre-receive 拒绝得到 `!	…	[remote rejected] (pre-receive hook declined)`，退出 1；远端在 `reference-transaction` 钩子的 `committed` 阶段被杀，得到的是没有状态行、退出 128、stderr 为 `the remote end hung up unexpectedly`，**而远端分支已经指向推送的 commit**。这就是只能按状态行、不能按 stderr 判定"被拒"的原因。本地 pre-push 钩子拒绝时同样没有状态行（退出 1），也保守地报 `Unknown`。
5. `Pushed` 之后，把 `refs/strata/remotes/<remote>/<dst_branch>` 更新为推送的 commit，用比较并交换，期望值是推送之前读到的值 [我的决定]：推送期间被并发的 fetch 改过就不动它。这与 git 推送后更新远端跟踪分支同义，只是留在 `refs/strata/` 里；这一步失败或跳过都不改变结果，下次 fetch 会对齐。
6. gitio 从不自动重推。`Unknown` 之后要不要再推，是人的事：publish 不重放，status 报出（不变量 6）；人确认后再 `strata push`，如果远端其实已经收到，再推得到 `=`，也就是 `Pushed`。
7. 每次推送的退出码与 stdout、stderr 的末尾经 `tracing` 记进日志，因为 `PushOutcome::Unknown` 不带文字（types.md）。

### 5.12 `merge3(base, ours, theirs)` 与 `is_binary`

`is_binary(bytes)`：前 8000 字节里有 NUL，即 git 的 `buffer_is_binary`。types.md 的 `Binary` 码与 merge3 用同一个判定。

`merge3`：

1. `ours == theirs` → `Clean(ours)`；`ours == base` → `Clean(theirs)`；`theirs == base` → `Clean(ours)`。
2. 三份里任何一份是二进制 → `Conflict`，恰好一个整文件 Hunk（types.md：两边都改了的二进制文件按整文件冲突）。
3. 按 `\n` 切行：行含它的 `\n`；`\r` 留在行里，所以 CRLF 文件按行合并并保留 CRLF；最后一行没有 `\n` 也算一行。算 base→ours 与 base→theirs 的行级 diff，diff 的实现见 §9。
4. 把两边的改动按 base 坐标排在一起。两处改动的 base 区间重叠或相邻（中间没有一行未改的 base 行）而内容不同 → 冲突区；内容相同 → 只取一份；其余的各自应用。这是 `git merge-file`（xdiff 的 `xdl_merge`）的判定。实测：隔一行未改行的两处改动干净合并；相邻两行各改一行冲突；同一行改成同样的内容干净；两边在文件末尾追加不同的行冲突；CRLF 文件干净合并并保留 CRLF。
5. 没有冲突区 → `Clean(合并结果)`；有冲突区 → `Conflict(冲突区列表)`。

约定：ours 是会话一侧，theirs 是上游一侧，即 publish 里的 B、`overlay::rebase` 里的新基线。base 不存在（两边都新增）时调用方传空串，与 git 处理 add/add 一样。修改与删除这类冲突由调用方判定，merge3 只处理三份内容。merge3 不看 `.gitattributes` 的 merge 驱动（`merge=union`、`merge=ours`、自定义驱动），也不看 `binary`、`-text` 标记；22 个仓库都没有 `.gitattributes`，这个差别今天不会出现。strata 自己的"永不冲突"手段是 `.lww`（overlay、publish）。

复杂度：Myers diff 为 O(N·D)，N 是行数、D 是改动量；只对两边都改了的文件调用。

## 6. 失败与恢复

| 操作 | 失败 | Code | 现场 | 恢复 |
| --- | --- | --- | --- | --- |
| 任意 | `git` 不在 PATH，或版本低于 2.44 | `Unsupported` | 无 | 装 git，或修 daemon 的 PATH（daemon.md） |
| `open` | 路径不存在 | `NoSuchPath` | 无 | 改配置 |
| `open` | 不是仓库、bare、子目录、strata 的工作区（`detail.reason = "strata-workspace"`） | `InvalidInput` | 无 | 改配置；daemon 拒绝整份配置（config.md §4.3） |
| `open` | SHA-256 仓库、reftable 仓库 | `Unsupported` | 无 | v1 不支持 |
| `fetch` | 名字不合法，remote 没配置 | `InvalidInput` | 无 | 改配置 |
| `fetch` | 远端没有这个分支，含远端还没有提交 | `NoSuchBranch` | 无 | session、publish 透传；已有发布链时由 publish 判断 |
| `fetch` | 网络、认证、超时、其他 | `Unknown` | strata 的跟踪 ref 不变；对象目录里可能留下 git 自己的 `tmp_pack_*`，git 日后清理 | 读操作，调用方可以再试 |
| `tree_of`、`tree_contains` | commit、tree 或 blob 不在库里（ls-tree 的大小一栏为 `BAD`，或缺 tree 对象）：被主 checkout 的 gc 清掉，或部分克隆缺对象 | `NoSuchCommit`（`detail.path`） | 无 | 重新 fetch，或换基线 |
| `tree_of`、`tree_contains` | gitlink、未知 mode、`.git` 分量、路径碰撞 | `Unsupported` | 无 | 这个 commit 不能当基线 |
| `tree_of`、`tree_contains` | 非 UTF-8 路径 | `NotText` | 无 | 同上 |
| `tree_of`、`tree_contains` | 路径分量超过 255 字节，`RepoPath` 不接受 | `InvalidInput` | 无 | 同上 |
| `tree_of`、`tree_contains` | ls-tree 输出格式不对，或其他非 0 退出 | `Unknown` | 无 | — |
| `export_tree` | `dst` 已存在 | `Exists` | `dst` 一字不动 | 调用方的错：`dst` 应是它独有的新路径。session 每次导出到随机名的暂存，同一基线的并发导出者在登记时去重，看不到 `Exists`（session.md @cec336b §5.2 第 7.2 步） |
| `export_tree` | 父目录不存在；父目录是符号链接或不是目录；`dst` 在主 checkout 里，或它的名字当不了 `RepoPath`；临时目录的名字超过 255 个 UTF-16 码元，或临时目录里的完整路径达到 1024 字节 | `NoSuchPath`、`NotDirectory`、`InvalidInput` | 无 | — |
| `export_tree` | 写入开始之后：required 过滤器失败、`ENOSPC`、I/O 错误、进程被杀、改名（`RenameMode::Excl`）撞上别人建好的 `dst`、daemon 崩溃 | `Unknown`（`detail`：`tree`、`temp`、`errno` 或 stderr） | `dst` 不存在；`.export-*` 临时目录原样保留：可见的目的地由 status 报出；目的地在调用方的暂存里时，它留在暂存里 | 可见的目的地：人查看后删掉临时目录，再重新导出；调用方的暂存：随 gc 退役暂存一起离开（module-rulings "Base staging is registered right after its manifest"）。gitio 不重试 |
| `ensure_shadow` | `dir` 不是绝对路径或在主 checkout 里 | `InvalidInput` | 无 | 调用方的错 |
| `ensure_shadow` | 父目录不存在 | `NoSuchPath` | 无 | — |
| `ensure_shadow` | `dir` 不存在，主 checkout 的 `info/exclude` 或 `info/attributes` 解析符号链接之后不是普通文件，或读不了（G11） | `NotFile`、`IsDirectory`、`NotDirectory`（apfs 的码）；读不了为 `Unknown` | 无，什么都没写 | 人处理主 checkout 里的那个文件 |
| `ensure_shadow` | `dir` 已存在，却不是本仓库的完整影子仓库（没有标记，alternates 指向别处，或 config、alternates 不是普通文件） | `Exists` | `dir` 一字不动 | 调用方处理，例如它是上一次 `new` 留下的现场 |
| `ensure_shadow` | 建仓开始之后：`git init` 失败、`ENOSPC`、I/O 错误、崩溃 | `Unknown` | `dir` 半成，没有 `strata.shadow` 标记 | status 报出；session 的 `rm` 整体退役它 |
| `shadow_at` | `dir` 不存在 | `NoSuchPath` | 无，什么都不建 | session 在 status 里把工作区报成坏的，从不经 `ensure_shadow` 重建（review N2） |
| `shadow_at` | 不是目录、没有 `strata.shadow` 标记、alternates 不指向本仓库的对象库 | `InvalidInput` | 无 | — |
| `shadow_at` | config 或 alternates 读不了或不是普通文件（G11）；config 有 git 也判坏的行 | `Unknown`（`detail.reason = "bad-config"`） | 无 | — |
| `shadow_drift` | 不是本仓库的影子仓库；`workspace` 不是绝对路径 | `InvalidInput` | 无 | — |
| `shadow_drift` | 镜像或 config 读不了，或主 checkout 一侧的镜像文件解析符号链接之后不是普通文件（G11；影子仓库一侧的报成漂移）；hooks 目录的查询失败（`detail.reason = "hooks-lookup"`：gitfile 坏了，或 config 里有 git 拒绝的值；守卫状态不明，可能已失效）；子进程超过 10 s（`timeout`，例如影子仓库 config 的 include 指向 FIFO） | `Unknown` | 无 | status 报出，`hooks-lookup` 按"守卫可能已失效"报；工作区的 gitfile 坏了时 `read_index` 同样报错 |
| `attach_worktree` | `shadow` 不属于本仓库；`n = 0`；路径不是绝对路径或在主 checkout 里 | `InvalidInput` | 无 | 调用方的错 |
| `attach_worktree` | `tree_dir` 不存在、不是目录；`<shadow>/worktrees` 是符号链接或文件 | `NoSuchPath`、`NotDirectory` | 无 | `<shadow>/worktrees` 那一种：session 把这个条目本身挪进 `<shadow>/strata-quarantine/worktrees-<nonce>`，daemon 告警，改名失败为 `Unknown`；留着的话以后每次 move 都被拒，git 的 `gc` 还会经它删掉目标里的东西（module-rulings "A symlinked worktrees/<n> is quarantined, never kept — amended"）。gitio 什么都没写 |
| `attach_worktree` | `tree_dir/.git` 或 `worktrees/<n>/` 已存在 | `Exists` | 无 | 调用方用新的代数，或先处理留下的现场 |
| `attach_worktree` | 第 2、3 步的 rooted 写入在写之前被拒：`worktrees/<n>` 或它的上级在第 1 步之后被换成符号链接或文件（例如指向 `<main>/.git`），或要建的名字已被占 | `NotDirectory`、`Exists`（G9 的例外） | 已建的管理目录（或换上来的链接）与 `tree_dir/.git/` 下的临时 index 留在原地；影子仓库之外什么都没写，主 checkout 的 index 不变 | 调用方退役它的内部暂存再以这个码拒绝（session：`S` 与 `worktrees/<n>`，意图关闭为 `refused`；`worktrees/<n>` 是换上来的符号链接时退役不了（store 的 `retire_dir` 拒绝符号链接），session 用 `apfs::rename_rooted` 把它挪进 `<shadow>/strata-quarantine/<n>-<nonce>` 并把意图关闭为 `refused`，daemon 告警；留在 `worktrees/` 里的话，git 的 `worktree prune`、`gc` 会跟随它、清空它的目标；见 module-rulings "A symlinked `worktrees/<n>` is quarantined, never kept"）；`new` 时 session 按现场处理 |
| `attach_worktree` | `tree_of(head)` 的拒绝 | `tree_of` 的码 | 无 | — |
| `attach_worktree` | 写入开始之后：read-tree 或 refresh 失败或超时、临时 index 的锁被占着、`ENOSPC`、I/O 错误、崩溃；refresh 之后核对出暂存被换过（`detail.reason = "staging-changed"`：`tree_dir` 的 ctime 变了，或它已不是同一个目录）；临时 index 的读回被拒（`detail.reason = "index-read-back-refused"`：`tree_dir/.git/index` 或 `.git` 被换成了符号链接这类东西，git 可能已经经它写到了别处） | `Unknown` | 半成的管理目录；`tree_dir/.git` 可能还是放临时 index 的目录，或还没写。`move_workspace` 时 `tree_dir` 是临时树，工作区一字没动（交换是调用方的）；`new` 时 `tree_dir` 就是工作区，第 3 步的临时目录与第 4 步的 gitfile 都写在它里面，失败留下的是工作区里的现场（review N4） | `move_workspace`：调用方按 Addendum 3、4 处理它的内部暂存；`new`：按现场处理；都由 status 报出。`staging-changed`、`index-read-back-refused` 另由 daemon（T13）告警给人，点名 `<main>/.git/index`（module-rulings "An attach that may have written outside its staging is raised to the human"） |
| `attach_staging` | 路径不是绝对路径或在主 checkout 里 | `InvalidInput` | 无 | 调用方的错 |
| `worktree_gitdir` | `tree_dir` 不是绝对路径；gitfile 的格式不对：不以 `gitdir: ` 开头、路径为空、含 NUL、超过 `8 + PATH_MAX_BYTES + 2` 字节（`detail.reason = "bad-gitfile"`） | `InvalidInput` | 无 | 调用方决定（session 的 `resolve_failed_move` 据此核对现场） |
| `worktree_gitdir` | `tree_dir` 是符号链接或不是目录；`.git` 是目录；`.git` 是符号链接、FIFO、socket 或设备 | `NotDirectory`；`IsDirectory`；`NotFile`（apfs 的码原样返回） | 无，什么都不跟随、不读 | 同上 |
| `worktree_gitdir` | 读错误 | 按读归类（`classify_io_error`，`IoSide::Read`） | 无 | — |
| `attach_staging` | `staging` 不存在、不是目录；`staging/.git` 或 `gitdir` 已存在；`tree_of(head)` 的拒绝 | `NoSuchPath`、`NotDirectory`、`Exists`、`tree_of` 的码 | 无 | — |
| `attach_staging` | 写入开始之后失败 | `Unknown` | 私有 git 目录半成，staging 根下可能还没有 `.git` | publish 按不变量 6 处理这棵 staging 树 |
| `read_index`、`is_ignored` | 没有 `.git` | `NoSuchPath` | 无 | — |
| `read_index`、`is_ignored` | worktree 属于别的仓库（common dir 既不是本仓库的，也不是本仓库的影子仓库） | `InvalidInput` | 无 | — |
| `read_index`、`is_ignored` | git 目录有效而 index 文件不存在 | `Unknown`（`detail.reason = "no-index"`） | 无 | 不猜：调用方停下这次 checkpoint，status 报出 |
| `read_index`、`is_ignored` | index、gitfile、`commondir`、config 读不了或不是普通文件（G11：`.git` 是符号链接或 FIFO，工作区的 `worktrees/<n>` 被换成了符号链接；本仓库 common dir 里的 config 先解析符号链接），或格式坏：截断、版本不认识、split index 缺共享半、位图越界；config 有 git 也判坏的行，或 `core.ignorecase` 不是 git 认的布尔值（`bad-config`） | `Unknown`（`detail.reason`） | 无 | 同上 |
| `read_index` | 非 UTF-8 路径；sparse index | `NotText`；`Unsupported` | 无 | — |
| `is_ignored` | 规则文件读不了（`ENOENT` 以外的错误；先 `realpath` 的那几份，`realpath` 得 `ENOTDIR` 也当作没有，G11）或不是普通文件（G11；工作树里的 `.gitignore` 是符号链接或目录时当作没有，本仓库 common dir 的 `info/exclude` 与全局排除文件先解析符号链接）；全局排除文件的查询超过 10 s（`timeout`） | `Unknown`（`detail`：`errno`、路径，或 `reason`） | 无 | — |
| `is_ignored_in_export` | `dir` 不是绝对路径，或 `dir` 里有 `.git`（`detail.reason = "has-git"`） | `InvalidInput` | 无 | 调用方的错：有 `.git` 的是工作树，用 `is_ignored` |
| `is_ignored_in_export` | `dir` 不存在；不是目录 | `NoSuchPath`、`NotDirectory` | 无 | — |
| `is_ignored_in_export` | `tree` 不能当基线 | `tree_of` 的码 | 无 | — |
| `is_ignored_in_export` | 规则文件读不了（`ENOENT` 以外的错误；先 `realpath` 的那几份，`realpath` 得 `ENOTDIR` 也当作没有，G11）或不是普通文件（G11，同 `is_ignored`）；主 checkout 的 config 有 git 也判坏的行（`bad-config`）；全局排除文件的查询超过 10 s（`timeout`） | `Unknown`（`detail`） | 无 | — |
| `stage` | 不是 `attach_staging` 接到本仓库的 staging 树（含主 checkout、人自己的 linked worktree、strata 的工作区；gitfile 或 `commondir` 读不出、不是普通文件） | `InvalidInput` | 无 | 调用方的错 |
| `stage` | 路径是 FIFO、socket、设备 | `NotFile` | 无，还没写 | — |
| `stage` | update-index 或 write-tree 失败 | `Unknown` | staging 树的 index 可能已部分更新；它是 publish 的私有目录 | publish 按不变量 6 处理这棵 staging 树 |
| `commit_tree` | tree 或 parent 不在库里 | `NoSuchCommit` | 无 | — |
| `commit_tree` | git 身份没配（`user.useConfigOnly` 下 git 退出 128） | `InvalidInput`（`detail.reason = "git-identity"`） | 无 | 配 `user.name`、`user.email` |
| `commit_tree` | 其他 | `Unknown` | 至多留下一个没人引用的对象；没有 ref 变化 | — |
| `update_ref`、`delete_ref`、`read_ref` | 名字不在 `refs/strata/` 下 | `InvalidInput` | 无 | 调用方的错 |
| `update_ref`、`delete_ref` | ref 不是期望值 | `HeadMoved`（`detail`：`ref`、`expected`、`actual`） | ref 不变 | 不重试、不重新合并（types.md） |
| `update_ref`、`delete_ref` | 其他：`.lock` 残留、I/O 错误 | `Unknown` | ref 不变，git 在锁里失败不会改 ref | status 报出；gitio 不删别人的 `.lock` |
| `push` | 校验失败，含 `staging` 不是 attach 过的 staging 树 | `Err`：`InvalidInput`、`NoSuchCommit` | 什么都没发出 | — |
| `push` | 本 ref 的状态行是 `!` | `Ok(Refused(原话))` | 远端没变，发布链在本地 | publish 转成 `PushRefused` |
| `push` | 超时、断线、没有状态行（含人自己的 pre-push 钩子拒绝） | `Ok(Unknown)` | 远端可能已经收到；本地 ref 不变 | publish 转成 `Unknown`：保留发布链、不重放、status 报出 |
| `merge3`、`is_binary` | 纯函数，不会失败 | — | — | — |

- **错误时序**（module-rulings 共同规则、Addendum 3 与 Addendum 4 的补充 2，types.md §6.3 末段）：类型化的拒绝只出现在写任何东西之前（G9）；写入一旦开始，失败就是 `Unknown`，现场保留，gitio 不回滚、不清理，status 报出（不变量 6）。对 gitio 而言，"写入开始"指建出导出临时目录、`git init` 影子仓库、建管理目录或 staging 私有目录、运行 update-index 的那一刻。其中导出临时目录、尚未被引用的新管理目录是调用方的内部暂存（Addendum 3）：调用方在自己的意图记录之下可以删掉它们再以类型化的码拒绝，那条意图记录随之关闭为 `refused`，从不删除；gitio 自己从不删，也不写意图记录。`attach_worktree` 第 2、3 步的 rooted 写入在写之前就被拒的（`NotDirectory`、`Exists`）照样原样返回，已建的管理目录与 `tree_dir/.git/` 下的临时 index 留给调用方：`move_workspace` 时它们是调用方的内部暂存，退役之后以这个码拒绝；`new` 时按现场处理（G9 的例外）。`worktrees/<n>` 被换成符号链接时，拒绝发生在任何东西写到影子仓库之外以前。
- **重启**：gitio 没有自己的持久状态，也不写意图记录。残留算内部暂存还是现场，由调用方的意图记录决定（Addendum 3、4）：内部暂存由 `recover` 删掉，意图关闭为 `abandoned`，记录留作历史；现场由 status 报出、不重放，包括结果为 `Unknown` 的推送。gitio 保证残留认得出来：导出临时目录带 `EXPORT_TEMP_PREFIX`，半成的影子仓库没有 `strata.shadow` 标记，管理目录在 `<shadow>/worktrees/<n>/`，没走完的 attach 在 `tree_dir/.git/` 下留着临时 index。
- **对 types.md 码表的扩展**：gitio 给 `InvalidInput`、`Exists`、`NoSuchPath`、`NotFile`、`NotDirectory`、`IsDirectory`（`worktree_gitdir`，apfs 的码原样返回）增加了触发条件；`Unsupported` 在 types.md 列出的 SHA-256 与 gitlink 之外，多了 git 版本过低、reftable、未知 mode、`.git` 分量、路径碰撞、sparse index；`NoSuchCommit` 多了 ls-tree 的 `BAD`；`Unknown` 多了缺 index（`no-index`）、config 的坏行（`bad-config`）、查不出工作区的 hooks 目录（`hooks-lookup`，守卫状态不明）、attach 期间暂存被换过（`staging-changed`）、临时 index 的读回被拒（`index-read-back-refused`）与本地命令超时（`timeout`）。用的都是已有的码，不加新码；T15 可以把 gitio 补进这些码的"谁返回"一列。

## 7. 测试清单

| 测试名 | 断言 | 来源（矩阵行 或 新增） |
| --- | --- | --- |
| `command_strips_inherited_git_env` | 父进程带着指向另一个仓库的 `GIT_DIR`、`GIT_WORK_TREE`、`GIT_INDEX_FILE`、`GIT_OBJECT_DIRECTORY`、`GIT_CONFIG_PARAMETERS`，以及 `STRATA_PUBLISH`（这个用例改父进程环境，单独一个测试二进制）：`open` 仍打开本仓库，`repo_id` 不变；`take_git_calls` 里每个子进程（含 `open` 自己起的）的环境都没有继承来的 `GIT_*` 与 `STRATA_PUBLISH` | 新增 |
| `command_sets_safety_flags` | `take_git_calls` 里 `open` 起的每个子进程：argv 带 `--no-replace-objects` 与 `-c core.fsmonitor=false`，env 有 `GIT_TERMINAL_PROMPT=0`、`GIT_NO_LAZY_FETCH=1`、`GIT_OPTIONAL_LOCKS=0`，stdin 是 `/dev/null`。`open` 的 rev-parse 带 `LC_ALL=C`。其余命令的 `LC_ALL` 规则（§5.1 第 3 步）在实现那个命令的任务里断言：fetch 见 `fetch_writes_only_the_strata_tracking_ref`，update-ref 见 `update_ref_cas_refuses_moved_ref`，tree_of 的 rev-parse 与 ls-tree 见 `tree_validation_is_cached_per_tree`，checkout-index 见 `export_tree_matches_git_checkout_byte_for_byte`，update-index 见 `stage_applies_clean_filters`，commit-tree 见 `commit_tree_creates_single_parent_commit`，push 见 `push_sets_strata_publish_and_never_forces` | 新增 |
| `open_resolves_repo_id_from_main_checkout_only` | 主 checkout 与人自己 `git worktree add` 的工作树 `open` 得到同一个 `RepoId`（common dir 的 realpath，`/tmp` 与 `/private/tmp` 归一）；用子目录 `open` → `InvalidInput`；strata 的工作区（gitfile 指向带 `strata.shadow` 标记的影子仓库）→ `InvalidInput`（`detail.reason = "strata-workspace"`），从不得到影子仓库的路径 | tgrep-core/tests/generations.rs::linked_worktrees_and_equal_trees_share_one_arc |
| `open_refuses_non_repository_bare_and_missing` | 非仓库目录、bare 仓库 → `InvalidInput`；不存在的路径 → `NoSuchPath` | tgrep-core/tests/generations.rs::unsupported_storage_and_discovery_return_errors |
| `sha256_repository_is_unsupported` | `git init --object-format=sha256` 的仓库（`extensions.objectFormat = sha256`）→ `open` 返回 `Unsupported`；SHA-1 仓库正常打开 | tgrep-core/tests/generations.rs::sha256_repositories_and_unicode_paths_are_supported |
| `reftable_repository_is_unsupported` | `git init --ref-format=reftable` 的仓库（`extensions.refStorage = reftable`）→ `open` 返回 `Unsupported`；files 仓库正常打开 | 新增 |
| `old_git_is_unsupported` | 版本解析（私有纯函数，`src/repo.rs` 的单测直接喂字符串）：`git version 2.43.5` → `Unsupported`；`2.44.0`、`2.54.0 (Apple Git-157)` 通过 | 新增 |
| `metadata_paths_strip_only_git_line_endings` | 读 gitfile 与 `commondir` 时只去掉 git 写入的行尾：没有行尾与 CRLF 两种输入，路径两侧的空格都保留；只有行尾时报错 | tgrep-core/src/git_index.rs::metadata_paths_strip_only_git_line_endings |
| `reads_a_version_2_index` | v2 index 的全部路径进 `tracked`，未跟踪的路径不在其中 | tgrep-core/src/git_index.rs::reads_a_version_2_index |
| `tracked_lookup_folds_ascii_case_when_ignorecase` | ignorecase 时 `is_tracked` 按 ASCII 不区分大小写（`Assets/Logo.PNG` 命中 `assets/logo.png`），`tracked` 本身保留 git 记录的拼写；只差 Unicode 大小写的（`Ébauche.png` 与 `ébauche.png`）不命中，那归 overlay；不归一反斜杠与前导 `/`，`RepoPath` 不接受它们 | tgrep-core/src/git_index.rs::matches_paths_without_regard_to_case_or_separator |
| `tracked_under_stops_at_separator` | `tracked_under(keep)` 为真，ignorecase 下大小写变体也为真；`qlogs` 不命中 `qlogs_like/` | tgrep-core/src/git_index.rs::finds_tracked_files_under_a_directory |
| `handles_a_path_at_and_beyond_the_length_field_limit` | v2 条目 12 位长度字段饱和前后（0xFFE、0xFFF、0x1000、0x1100）的路径都解析正确 | tgrep-core/src/git_index.rs::handles_a_path_at_and_beyond_the_length_field_limit |
| `rejects_input_that_is_not_an_index` | 空输入、签名错、不认识的版本 → `Unknown`，不猜 | tgrep-core/src/git_index.rs::rejects_input_that_is_not_an_index |
| `refuses_a_truncated_index_instead_of_inventing_entries` | 在头部、第一条路径内、第二条路径内截断的 index 都被拒绝 | tgrep-core/src/git_index.rs::refuses_a_truncated_index_instead_of_inventing_entries |
| `tolerates_the_trailing_data_a_real_index_carries` | 条目之后的扩展区与结尾校验和不导致拒绝 | tgrep-core/src/git_index.rs::tolerates_the_trailing_data_a_real_index_carries |
| `a_count_larger_than_the_data_is_refused` | 头部条目数大于数据能容纳的 → 拒绝 | tgrep-core/src/git_index.rs::a_count_larger_than_the_data_is_refused |
| `a_header_claiming_four_billion_entries_does_not_reserve_for_them` | 头部声称 u32::MAX 条时不按此预留内存，返回拒绝而不是中止进程 | tgrep-core/src/git_index.rs::a_header_claiming_four_billion_entries_does_not_reserve_for_them |
| `reads_a_version_4_index_with_prefix_compressed_paths` | v4 前缀压缩的路径按顺序还原成完整路径 | tgrep-core/src/git_index.rs::reads_a_version_4_index_with_prefix_compressed_paths |
| `reads_a_split_index_through_its_shared_half` | 拆分 index 经共享半合并，按删除与替换位图处理；没有位图时不删不换 | tgrep-core/src/git_index.rs::reads_a_split_index_through_its_shared_half |
| `a_split_index_without_a_matching_shared_half_is_refused` | 缺少或对不上共享半、位图越界、替换数超过条目数 → 拒绝；全零的基准不需要共享半 | tgrep-core/src/git_index.rs::a_split_index_without_a_matching_shared_half_is_refused |
| `ewah_runs_and_literals_are_bounded_by_the_shared_index` | EWAH 的游程与字面字不越过共享 index 的条目数，标记声称的字面字超出位图时拒绝 | tgrep-core/src/git_index.rs::ewah_runs_and_literals_are_bounded_by_the_shared_index |
| `reads_the_split_indexes_git_writes` | 真实 git 生成的 v2、v4 拆分 index（含删除、替换、新增）与 `git ls-files -z` 逐项一致 | tgrep-core/src/git_index.rs::reads_the_split_indexes_git_writes |
| `reads_ignorecase_from_a_repository_config` | `core.ignorecase`：节名与键名不分大小写，`true`、`True`、`yes` 为真，别的节里的同名键不算，缺省为假 | tgrep-core/src/git_index.rs::reads_ignorecase_from_a_repository_config |
| `follows_a_gitdir_pointer_file` | `.git` 是 `gitdir:` 指针文件时，经它读到 config 与 index | tgrep-core/src/git_index.rs::follows_a_gitdir_pointer_file |
| `linked_worktree_uses_the_common_config_and_its_own_index` | config 取 common dir 的：人自己的 linked worktree 在 `extensions.worktreeConfig` 打开时由 `config.worktree` 覆盖，否则不生效；strata 的工作区取影子仓库复制来的 `core.ignorecase`。index 取工作树自己 git 目录的，工作区是管理目录 `worktrees/<n>/index` | tgrep-core/src/git_index.rs::linked_worktree_uses_the_common_config_and_its_own_index |
| `read_index_without_git_is_an_error` | 没有 `.git` 的目录 → `NoSuchPath`；git 目录有效而 index 文件不存在 → `Unknown`（`detail.reason = "no-index"`），从不是空的 `tracked` | tgrep-core/src/git_index.rs::a_directory_without_git_reports_nothing |
| `non_utf8_index_path_is_not_text` | 经 `git update-index --index-info` 写进字节为 `\xff.txt` 的条目，`read_index` 返回 `NotText`，不跳过 | tgrep-core/tests/generations.rs::non_unicode_repository_roots_are_lossless_but_tracked_paths_are_unsupported |
| `sparse_index_is_unsupported` | 带 `sdir` 扩展的 index → `Unsupported` | 新增 |
| `read_index_snapshot_is_immutable_and_cached` | 返回的 `IndexInfo` 在 index 之后改变时保持不变；index 文件的 (size, mtime, inode) 不变时第二次调用不重新解析：把 index 原地改写成等长的垃圾、再把 mtime 改回原值，第二次调用仍返回原快照，mtime 一变就返回 `Unknown` | 新增 |
| `config_values_read_like_git` | `src/index.rs` 的单测：同一组 config 文本，改过的 `config_bool` 与 `git config --file <f> --type=bool --get <键>` 的结论一一相同（同一个值，或两边都报错，或两边都没有这个键）：`true`、`"true"`、`TRUE`、`"tr"ue`、行内的 `#` 与 `;` 注释、引号里的 `#`、没有 `=`、`=` 后为空、`""`、`yes`、`On`、`off`、`0`、`1`、`2`、`-1`、`0x10`、`010`、`1k`、`0k`、`maybe`、`tr\"ue`、不认识的转义、没闭合的引号、引号里的首尾空格、同一个键写两次、`[Core]`、`[core "x"]`、`[ core ]`、段头同一行的键、行尾 `\` 续行（review B1）；review M2 补的：`ignorecase # c`、`ignorecase ; c`、名字后的 `\v` 与 `\f`、行首的 `\v`；文件里任何位置的非法键名（`ignore_case = 1`、`9key = 1`，在要读的键之前、之后、在别的节里）与坏段头（`[core]]`、`[]`、`[co_re]`、`[core""]`、`[core "x" ]`）；`2147483647`、`2147483648`、`-2147483648`、`-2147483649`、`3000000000`、`1g`、`2g`、`-2g`、`-3g`、`2097151k`、`2097152k`、`0x7fffffff`、`0x80000000`、`9223372036854775808`、`1K`、`1M`、`1G`、`+1`、`-0`、`07`、`08`、`0x`、`1kb`、`1e3`、`" 1"`、`"1 "`、`"\t1"`、值开头与末尾的 `\v`；开头的 BOM、只有前两个字节的 BOM、两个 BOM；CRLF 行尾、值里单独的 `\r`、`\` 加 CRLF 的续行、文件末尾的 `\`、跨续行的引号段；前一次是坏值而最后一次是好值；第一个段头之前的键；`[core][user]` 同一行两个段头；`[core "a\zb"]`；空段名带子段的 `[ "x"]`、`[\tcore]`、`[core\t"x"]`、`[.]`、`[core.]`、`[core ""]`；`-2097152k`、`-2097153k`、`0g`；re-review r4 的 N1：`1 ""`、`true ""`、`0 ""`、`off ""`、`"1" ""`、`1 "" # c`、`1<TAB>""`，`1 \` 续行到文件末尾、到只有空白的行、到 `# c` 一行，`1 \` 加 CRLF，与对照的 `1 # c`、`1\` 续行到只有空白的行。git 接受而读法报 `Unknown` 的只有含 NUL 字节的文件（§5.7 第 6 步的保守规则），用例照这个差别断言；`[include] path = <f>` 而 f 里设了 `ignorecase` 时读法不跟随（§9），用例断言读法给的是本文件里的值 | 新增 |
| `read_index_refuses_worktree_of_another_repo` | worktree 的 common dir 既不是本仓库的，也不是借本仓库对象库的影子仓库（alternates 指向另一个仓库，或没有 `strata.shadow` 标记）→ `InvalidInput`；本仓库影子仓库的工作区照常读出 | 新增 |
| `index_parse_tracked_never_ignored` | ignorecase 仓库：规则 `QLogs` 隐藏未跟踪的 `qlogs/` 下文件，`*.txt` 隐藏未跟踪的 `Gone.TXT`，已跟踪的 `Kept.TXT` 不被隐藏，磁盘上换成 `kept.txt` 拼写也不被隐藏（`git status` 的语义）；区分大小写的仓库里，被规则逐字命中的已跟踪文件同样不被隐藏 | tgrep-core/src/walker.rs::ignorecase_hides_what_git_hides_and_keeps_what_git_tracks |
| `untracking_a_file_lets_rules_apply` | `git rm --cached TRACKED.txt` 之后同一路径 `is_ignored` 变为真；index 被写坏后 `is_ignored` 返回 `Unknown`，不当成"没有已跟踪文件" | tgrep-core/src/worktrees/tests.rs::case_insensitive_ignore_snapshot_tracks_index_membership_coherently |
| `case_sensitive_repo_matches_exactly` | `core.ignorecase = false`：逐字节匹配，`*.txt` 不隐藏 `Gone.TXT`；逐条点查与遍历时的逐项调用结果相同 | tgrep-core/src/walker.rs::without_ignorecase_the_walk_is_unchanged；tgrep-core/src/walker.rs::the_point_query_matcher_follows_the_case_sensitivity_gate |
| `ascii_only_case_folding_like_git` | ignorecase 下模式 `É.txt` 不命中 `é.txt` | 新增 |
| `root_gitignore_file_and_dir_patterns` | 根 `.gitignore` 的 `*.log`、`target/` 生效，`target/` 只匹配目录；`src/main.rs` 不受影响 | tgrep-core/src/gitignore.rs::builds_matcher_from_root_gitignore |
| `self_ignored_gitignore_still_applies` | `build/.gitignore` 内容为 `*`：它自己被命中，但规则照样生效（`build/artifact.o` 被忽略）；被隐藏的 `.gitignore` 照样贡献规则 | tgrep-core/src/gitignore.rs::collects_a_gitignore_that_its_own_rules_ignore；tgrep-core/src/walker.rs::walk_dir_can_collect_gitignore_files_without_indexing_them |
| `malformed_ignore_line_skips_only_itself` | 一行 `[` 不让同一文件里其他规则作废 | tgrep-core/src/gitignore.rs::malformed_ignore_line_does_not_discard_valid_rules |
| `no_rules_ignores_nothing` | 没有任何规则文件（全局排除文件也不存在）时，所有路径都是 false | tgrep-core/src/gitignore.rs::returns_none_when_no_rules |
| `info_exclude_applies_through_worktree_gitfile` | 工作区的 `.git` 是 gitfile：经管理目录的 `commondir` 找到影子仓库镜像的 `info/exclude` 并生效；建仓之后主 checkout 改了 `info/exclude`，工作区的判定不变（镜像只取一次）；删掉影子仓库里的镜像后不再生效 | tgrep-core/src/gitignore.rs::the_repository_exclude_is_found_through_a_worktree_pointer |
| `relative_commondir_resolves_against_gitdir` | 管理目录里的相对 `commondir`（`../..`）相对管理目录解析到影子仓库，不相对工作树根 | tgrep-core/src/gitignore.rs::a_relative_commondir_resolves_against_the_worktree_git_dir |
| `global_excludes_file_is_honoured` | 没设 `core.excludesFile` 时读 `$XDG_CONFIG_HOME/git/ignore`（缺省 `~/.config/git/ignore`），设了就读它；主 checkout 在仓库级设了它时，影子仓库复制了这个键，工作区解析出同一个文件；`.claude/settings.local.json` 只靠全局排除文件被忽略时 `is_ignored` 为真，在主 checkout 与工作区里都是 | 新增 |
| `local_negation_overrides_global_excludes` | 本地 `!keep.log` 压过全局排除文件的 `*.log`；优先级为目录内 `.gitignore` > `info/exclude` > 全局排除文件 | tgrep-core/src/gitignore.rs::local_whitelist_overrides_global_ignore |
| `deeper_rules_override_shallower` | `pkg/` 里的 `!keep.log` 压过根的 `*.log`；更深目录的 `!important.log` 压过浅层的 `*.log` | tgrep-core/src/gitignore.rs::with_nested_keeps_deeper_rules_ahead_of_local_rules；tgrep-core/src/gitignore.rs::deeper_gitignore_negation_overrides_a_shallower_ignore |
| `nested_gitignore_scoped_to_its_directory` | 嵌套 `.gitignore` 里的 `*`、`*.c` 只作用于所在目录，不提升成全树规则；根的 `*.log` 与 `src/.gitignore` 的 `*.tmp` 各在自己的作用域内 | tgrep-core/src/gitignore.rs::nested_gitignore_rules_stay_scoped_to_their_directory；tgrep-core/src/walker.rs::build_gitignore_matcher_from_discovered_files |
| `gitignore_above_worktree_root_is_not_applied` | 会话根 `<root>/sessions/<session-id>/` 里、即工作树根之上的 `.gitignore`，不作用于工作区里的文件 | tgrep-core/src/walker.rs::parent_ignore_crosses_repository_boundary_but_gitignore_does_not |
| `negation_cannot_reinclude_under_excluded_dir` | `build/` 被排除时 `!build/keep.c` 不生效 | 新增 |
| `unreadable_index_is_an_error_not_empty` | index 不可读（权限、截断）或不存在时，`is_ignored` 返回 `Unknown`，既不全放行也不全隐藏 | tgrep-core/src/walker.rs::an_unreadable_index_excludes_nothing |
| `rule_and_index_reads_never_hang_on_non_regular_files` | 主 checkout 与人自己的 linked worktree 上（沙箱 `HOME`）：工作树根的 `.gitignore`（规则 `*.log`）换成指向同样内容的符号链接 → `x.log` 不被忽略，换成目录 → 同样当作没有，两者都与 `git status --ignored` 相同（实测）；`.gitignore`、`info/exclude`、`~/.config/git/ignore` 各换成 FIFO，`info/exclude` 换成指向 FIFO 的符号链接 → `is_ignored` 返回 `Unknown`（`detail.path` 是那个文件，经符号链接的是解析之后的路径）；`~/.config/git/ignore` 与主 checkout 的 `info/exclude` 换成指向普通文件的符号链接 → 照常读到其中的规则，悬空的 `info/exclude` 符号链接 → 没有规则，主 checkout 的 `config` 换成指向一份 `core.ignorecase = false` 的副本的符号链接 → `info/exclude` 里的规则 `*.LOG` 不再忽略 `x.log`，都与 `git check-ignore` 相同；linked worktree 的 `.git` 换成指向它原来那份 gitfile 的符号链接、或换成 FIFO → `read_index` 与 `is_ignored` 都是 `Unknown`；主 checkout 的 index 换成 FIFO、或换成指向它原来那份的符号链接 → `read_index` 是 `Unknown`；`open` 时配置的 `path` 是指向主 checkout 的符号链接，`read_index` 与 `is_ignored` 照常工作。工作区目录本身被换成指向主 checkout 的符号链接、再以工作区路径调用 `read_index`：`Unknown`，不是主 checkout 的 index（breaker 裁定，re-review r5 的 M-1）。`info/exclude` 的路径上某一级是普通文件（`ENOTDIR`）→ 当作没有，不是 `Unknown`（M-3）。每次调用都在 5 s 内返回（测试用看门狗）（module-rulings "gitio reads files through apfs"） | 新增 |
| `is_ignored_agrees_with_git_on_corpus` | 固定语料（嵌套 `.gitignore`、否定、目录模式、`**`、锚定、`info/exclude`、全局排除文件、ignorecase 开关）逐路径对拍：规则部分对 `git check-ignore --no-index`，已跟踪豁免部分对 `git status --porcelain --ignored`；主 checkout 与 strata 的工作区各跑一遍 | 新增 |
| `is_ignored_in_export_ignores_seeds_like_the_workspace` | 把主 checkout HEAD 的 tree 放进 `dir`（没有 `.git`；用 `tests/common` 的 `archive_tree_into`，Task 5 时还没有 `export_tree`）：只靠主 checkout `info/exclude` 忽略的 `.env`、只靠全局排除文件（`set_env` 的 `HOME`）忽略的 `.claude/settings.local.json` → true，`CLAUDE.md` → false，与主 checkout 上 `is_ignored` 的答案相同；主 checkout 改了 `info/exclude` 之后，答案跟着主 checkout 现在的那份走；调用前后 `dir` 一字不动（session.md @853fdf8 §2 的上游要求） | 新增 |
| `is_ignored_in_export_never_ignores_a_tracked_path` | tree 里跟踪了 `logs/keep.log`，`dir` 的 `.gitignore` 有 `*.log` 与 `logs/`：`logs/keep.log` → false，ignorecase 仓库里 `LOGS/KEEP.LOG` 也 → false；未跟踪的 `logs/other.log` → true；路径表里的目录 `logs` 按规则回答 → true，与主 checkout 上 `is_ignored` 对同一个目录的答案相同（目录从不算已跟踪）；`tree` 含 gitlink → 与 `tree_of` 相同的 `Unsupported` | 新增 |
| `is_ignored_in_export_honours_negation` | `dir` 的 `.gitignore` 为 `*.local` 与 `!keep.local`：`keep.local` → false，`x.local` → true；主 checkout 的 `info/exclude` 有 `*.env` 而 `dir` 的 `.gitignore` 有 `!keep.env`：`keep.env` → false（目录内 `.gitignore` 压过 `info/exclude`）；`build/` 被排除时 `!build/keep.c` 不生效 → true | 新增 |
| `is_ignored_in_export_reads_nested_gitignore_from_dir` | `dir/sub/.gitignore` 为 `secret.txt`：`sub/secret.txt` → true，`secret.txt` 与 `other/secret.txt` → false；主 checkout 工作树里没提交的 `.gitignore` 改动（加了一行 `CLAUDE.md`）不起作用，规则只从 `dir` 读；`dir` 的父目录里的 `.gitignore` 不起作用；`dir` 不存在 → `NoSuchPath`，`dir` 里有 `.git` → `InvalidInput`（`detail.reason = "has-git"`），都不写任何东西 | 新增 |
| `option_like_arguments_are_refused_not_executed` | remote 为 `--upload-pack=touch <f>`、分支为 `-x`、修订为 `--help` 时返回 `InvalidInput`，`<f>` 不出现；以 `-` 开头、含 `*` 或 `:` 的路径按字面处理 | tgrep-core/tests/generations.rs::interrupted_and_failed_publications_are_not_discoverable |
| `fetch_writes_only_the_strata_tracking_ref` | fetch 之后 `refs/strata/remotes/origin/main` 等于远端 tip，也是返回值；`refs/remotes/origin/main`、`FETCH_HEAD`、`refs/tags` 不变，远端有新 tag 也不带回；`take_git_calls` 里 fetch 的子进程带 `LC_ALL=C`，stdin 是 `/dev/null` | 新增 |
| `fetch_missing_branch_is_no_such_branch` | 远端没有这个分支，包括远端还没有任何提交 → `NoSuchBranch`，不当成空 tree | tgrep-core/tests/generations.rs::unsupported_storage_and_discovery_return_errors |
| `fetch_timeout_is_unknown_and_moves_nothing` | `set_fetch_timeout` 设成 2 s，远端挂住（假 upload-pack 一直睡）→ `Unknown`，跟踪 ref 不变，假 upload-pack 随进程组一起被杀 | 新增 |
| `ancestry_answers_come_from_git` | 有分叉与合并的历史上，`is_ancestor`、`merge_base` 与 `git merge-base` 相同；没有共同祖先 → `None`；两个最近公共祖先时返回 git 给出的那个 | layerfs-api/tests/history_ancestry_oracle.rs::bounded_ancestry_equals_the_whole_history_walk |
| `read_ref_absent_is_none` | 不存在的 `refs/strata/` ref → `None` | 新增 |
| `update_ref_cas_refuses_moved_ref` | 期望值与实际值不同 → `HeadMoved`，`detail` 带 `expected` 与 `actual`，ref 不变；期望值相同 → 前进；`take_git_calls` 里 update-ref 的子进程带 `LC_ALL=C`（`HeadMoved` 的分类读它的英文原话） | 新增 |
| `update_ref_create_requires_absent` | `old = None` 而 ref 已存在 → `HeadMoved`；不存在 → 创建 | 新增 |
| `refs_outside_strata_namespace_are_refused` | `read_ref`、`update_ref`、`delete_ref` 对 `refs/heads/main`、`refs/remotes/origin/main`、`HEAD` 返回 `InvalidInput`，什么都不写 | 新增 |
| `delete_ref_is_compare_and_swap` | 期望值不符 → `HeadMoved`，ref 仍在；相符 → 删除 | 新增 |
| `replace_refs_do_not_change_tree_of` | 对 commit 做 `git replace` 之后，`tree_of` 仍返回原 commit 的 tree，导出的也是原 tree 的内容 | tgrep-core/tests/generations.rs::replace_refs_do_not_change_exact_committed_tree |
| `gitlink_in_base_tree_is_unsupported` | tree 里有 160000 条目 → `tree_of` 与 `export_tree` 都返回 `Unsupported`（`detail.path`），不建任何目录 | tgrep-core/tests/generations.rs::gitlinks_are_membership_only_and_missing_blobs_are_errors |
| `missing_blob_refused_before_export` | 手工删掉一个松散 blob：`ls-tree -l` 照样退出 0，只把它的大小印成 `BAD` → `tree_of` 返回 `NoSuchCommit`，`detail.path` 是那个 blob 的路径；删掉一个子 tree（ls-tree 退出 1）同样是 `NoSuchCommit`；都在导出之前拒绝，已有的基线目录不受影响 | tgrep-core/tests/generations.rs::gitlinks_are_membership_only_and_missing_blobs_are_errors |
| `non_utf8_tree_path_is_not_text` | `git mktree` 造出含 `\xff.txt` 的 tree → `tree_of` 返回 `NotText`，不跳过 | tgrep-core/tests/generations.rs::non_unicode_repository_roots_are_lossless_but_tracked_paths_are_unsupported |
| `colliding_paths_are_unsupported` | tree 里两条路径 NFC 规整后相同，或在 ignorecase 仓库里 ASCII 小写后相同（`README` 与 `readme`）→ `Unsupported`，`detail` 列出两条；同一个名字既是符号链接又是目录（`git mktree` 造出 `a` 与 `a/b`）同样 → `Unsupported`；只差 Unicode 大小写的一对（`É.txt` 与 `é.txt`）不拒绝，导出与 `git checkout` 一样只留一个文件 | 新增 |
| `dot_git_component_is_unsupported` | `git mktree` 造出含 `.GIT/config` 或 `sub/.git/x` 条目的 tree → `tree_of` 返回 `Unsupported`（`detail.path`）；`export_tree` 在建临时目录之前同样拒绝 | 新增 |
| `overlong_component_refused_before_writing` | 路径分量 256 字节 → `tree_of` 返回 `InvalidInput`；导出时 `dst` 加路径超过 1024 字节同样在写之前拒绝 | layerfs-content/tests/filesystem_limits.rs::a_component_name_is_accepted_at_255_bytes_and_refused_at_256 |
| `ls_tree_output_parse_is_exact` | 解析器对截断的记录、缺结尾 NUL、字段数不对、OID 长度不对、未知 mode、既不是数字也不是 `BAD` 的大小一律报错，不凭空造条目 | layerfs-bridge/tests/install_records.rs::bounded_manifest_is_exact_and_does_not_cap_stream_length |
| `tree_validation_is_cached_per_tree` | 同一个 `TreeOid` 第二次 `tree_of` 不再起 ls-tree（`take_git_calls`），第一次起的 rev-parse 与 ls-tree 都带 `LC_ALL=C`；不同 commit 指向同一个 tree 时共用结论 | 新增 |
| `tree_contains_answers_from_the_validated_tree` | tree 里的文件、符号链接与目录 → `true`；ignorecase 仓库里的 ASCII 大小写变体 → `true`，区分大小写的仓库里 → `false`；不在里面的 → `false`；路径表在缓存里时不再起 ls-tree（`take_git_calls`）；不能当基线的 tree（含 gitlink）→ 与 `tree_of` 相同的 `Unsupported` | 新增 |
| `export_tree_matches_git_checkout_byte_for_byte` | 混合 tree（`.gitignore`；`.gitattributes` 的 `text`、`eol=crlf`、`working-tree-encoding=UTF-16`、`ident`；一对 smudge/clean 过滤器；二进制文件、空文件、可执行文件；悬空、自指、指向树外的符号链接；深目录；非 ASCII 文件名；再加 `core.autocrlf=true` 的变体）导出后，与 `git clone` 再检出同一 commit 的工作树逐文件比较：字节、可执行位、符号链接目标全同；导出目录里没有 `.git`、没有空目录，每个路径是独立的 inode；空 tree 导出成空的 `dst`；checkout-index 的子进程没有 `LC_ALL`（`take_git_calls`） | tgrep-core/src/worktrees/tests.rs::crlf_and_smudge_checkouts_are_verified_even_when_git_reports_clean；tgrep-core/tests/generations.rs::raw_blob_semantics_require_decoded_identity_not_git_cleanliness；layerfs-daemon/tests/complete_installed_roots.rs::all_supported_kernel_paths_survive_install_with_full_oracles；layerfs-daemon/tests/host_install.rs::authenticated_install_opens_one_store_and_reads_the_complete_root |
| `export_filter_failure_matches_checkout` | smudge 命令不存在：`filter.<d>.required = true` → 返回 `Unknown`，`dst` 不存在，临时目录保留；没设 required → 与 `git checkout` 一样写原始 blob | tgrep-core/tests/generations.rs::raw_blob_semantics_require_decoded_identity_not_git_cleanliness |
| `export_tree_refuses_existing_destination_untouched` | `dst` 已存在 → `Exists`；`dst` 里的字节与 mtime 不变，也没建临时目录 | layerfs-daemon/tests/host_install.rs::existing_store_is_refused_before_upload_and_preserved |
| `export_typed_refusals_write_nothing` | gitlink、`NotText`、碰撞、`.git` 分量、超长（含长到临时目录名超过 255 个 UTF-16 码元的 `dst` 名字）、父目录不存在、父目录是符号链接（`NotDirectory`）、`dst` 在主 checkout 里，这些拒绝都发生在建临时目录之前：`dst` 的父目录里没有多出任何条目，符号链接指向的目录也没有 | layerfs-api/tests/local_install.rs::an_installed_manifest_is_refused_before_anything_is_written |
| `interrupted_export_keeps_temp_and_never_creates_destination` | checkout-index 进行中杀掉 git（过滤器里注入 SIGKILL）→ `Unknown`；`dst` 不存在；`.export-*` 临时目录原样保留，按前缀可以列出；再次导出到同一个 `dst` 照常成功，用的是新临时目录 | layerfs-daemon/tests/host_install.rs::interrupted_transfer_keeps_original_partial_file_and_claim_refuses_another_installer |
| `concurrent_export_never_overwrites_destination` | 不经任何锁，同一个 `dst` 的两次导出：用 `pause_export_before_rename` 让导出 A 停在改名之前，导出 B 完整跑完，再放行 A → A 得到 `Unknown`（`RenameMode::Excl` 的改名撞上已有的 `dst`），A 的临时目录保留，`dst` 是 B 的完整结果，从未被覆盖；`dst` 出现之后才开始的导出得到 `Exists`，什么都不写。只断言 gitio 自己的保证；并发的那一半归 session（§1：同一基线的并发导出者在登记时去重，谁都看不到 `Exists`；原来写给 daemon.md §7 的 `second_exporter_waits_then_busy_never_exists` 由 T13 处理） | 新增 |
| `export_large_tree_is_complete` | 10 万个文件的 tree 导出后文件数与 ls-tree 一致（慢测试，`#[ignore]`，发布前跑） | layerfs-daemon/tests/complete_installed_roots.rs::huge_kernel_namespace_is_complete_after_install |
| `export_is_durable_before_rename` | `take_sync_events` 的顺序：改名之前 `T/tree` 与它下面的每个文件与目录都 `fsync` 过，并做过一次 `full_fsync`；改名是 `RenameMode::Excl` 的 `rename_rooted`，改名之后对父目录再做一次 `full_fsync` | 新增 |
| `exported_workspace_git_status_is_clean` | 主 checkout 配了一对 smudge/clean 过滤器与 `core.autocrlf`、tree 带 `eol=crlf` 的 `.gitattributes`：导出、`ensure_shadow`、`attach_worktree` 之后，工作区里 `git status --porcelain` 为空（影子仓库复制了 `filter.*` 与 `core.autocrlf`） | tgrep-core/src/worktrees/tests.rs::crlf_and_smudge_checkouts_are_verified_even_when_git_reports_clean |
| `ensure_shadow_builds_the_layout` | 全局配置（`set_env` 给的 `HOME` 里）设了 `init.defaultObjectFormat = sha256`、`init.defaultRefFormat = reftable`、`init.templateDir` 时，影子仓库仍是 SHA-1、files，没有模板钩子、`description` 与默认 `info/exclude`；`objects/info/alternates` 是主 checkout `objects` 的 realpath；config 恰好是 `core.repositoryformatversion`、`core.bare`、§4.2 清单里主 checkout 设了的键、`core.hooksPath = <dir>/hooks`（绝对路径）与 `strata.shadow = true`，主 checkout 设了的 `remote.origin.*`、`branch.*`、`include.path`、`extensions.*` 一个都没有，主 checkout 自己的 `core.hooksPath` 也没有被复制；`info/exclude`、`info/attributes` 与主 checkout 逐字节相同，主 checkout 没有的就不存在；`hooks/pre-push` 没有 `STRATA_PUBLISH=1` 时以 1 退出并打印那一行，有时以 0 退出；config 的每个值经 `git config --file <dir>/config --get` 读回写进去的原值（主 checkout 复制来的值里含 `#`、首尾空格、`"`、`\`、换行时也一样），只有 git 也会加引号的值带引号，标记一行是不带引号的 `shadow = true`（review B1） | 新增 |
| `ensure_shadow_is_idempotent_and_never_rewrites_mirrors` | 对同一个 `dir` 再调一次返回同一个 `Shadow`，影子仓库的每个字节不变，即使主 checkout 的 `info/exclude` 与 config 在两次之间变了；`dir` 已存在但没有 `strata.shadow` 标记（半成）或 alternates 指向别的仓库 → `Exists`，一字不动；`dir` 在主 checkout 里 → `InvalidInput` | 新增 |
| `shadow_at_reopens_without_writing` | `ensure_shadow` 建好之后，换一个新 `open` 的 `Git`（模拟 daemon 重启）调 `shadow_at`：返回与 `ensure_shadow` 相等的 `Shadow`（标记由 gitio 的写法写出、由 gitio 的读法读回，review B1）；影子仓库里每个文件的字节与 mtime 都不变；`shadow_drift` 接受它并返回空表，`attach_worktree` 用它建出下一代；再把标记手改成 `shadow = "true"`，`shadow_at` 照样认出 | 新增 |
| `shadow_at_missing_dir_is_no_such_path_and_creates_nothing` | `dir` 不存在 → `NoSuchPath`，之后 `dir` 仍不存在，父目录里没有新条目（不会重建）；`dir` 存在但没有 `strata.shadow` 标记、alternates 指向别的仓库、或是普通文件 → `InvalidInput`，一字不动 | 新增 |
| `shadow_drift_reports_changed_mirrors` | 刚建好时为空；主 checkout 改了 `info/exclude`、新建了 `info/attributes`、改了 `core.autocrlf`、删了一个 `filter.<d>.clean` → 依次报 `Exclude`、`Attributes` 与两条 `Config { key }`；影子仓库的字节不变 | 新增 |
| `shadow_drift_reports_a_disabled_pre_push_guard` | 在工作区里跑 husky 安装时的那条 `git config core.hooksPath .husky/_` → `shadow_drift` 只报 `PrePushGuardDisabled { hooks_path: Some("<工作区>/.husky/_"), hook_replaced: false }`（git 在工作区里解析出的绝对路径）；删掉这个键而外面没有 hooksPath → 不报（有效目录仍是 `<dir>/hooks`，守卫照常拒绝）；`ensure_shadow` 的 `dir` 经一个符号链接上级给出（`<tmp>/link/s.git`，`link` 指向真目录）时，刚建好也不报：git 给出 realpath，按 `Shadow::dir()` 比（re-review r4 的 N8）；再在沙箱的用户级 config 里设 hooksPath → `hooks_path: Some(<那个目录>)`；钉回 `<dir>/hooks` 之后把 `hooks/pre-push` 换成别的脚本 → `hooks_path: None, hook_replaced: true`；每次 `shadow_drift` 前后影子仓库的字节都不变，gitio 不修（review B4、M3） | 新增 |
| `shadow_drift_follows_the_effective_hooks_path` | 工作区里 `git config include.path <f>`，f 设了 `core.hooksPath = <别处>`：`shadow_drift(&shadow, 工作区)` 报 `Config { key: "include.path" }` 与 `PrePushGuardDisabled { hooks_path: Some("<别处>"), hook_replaced: false }`，而影子仓库 config 的本地列表里钉住的值没变；同一个 include 写成 `includeIf "gitdir:<shadow>/worktrees/**"` → 报的键是 `includeif.gitdir:<shadow>/worktrees/**.path`；`extensions.worktreeConfig = true` 加 `git config --worktree core.hooksPath <别处>` → 报 `Config { key: "extensions.worktreeconfig" }` 与 `hooks_path: Some("<别处>")`；只打开 `extensions.worktreeConfig`、在管理目录的 `config.worktree` 里加一条 include → 报 `Config { key: "extensions.worktreeconfig" }`（re-review r4 的 N4）；钉住的键在、沙箱用户级 config 设了 hooksPath（直接设，或经只对管理目录生效的 `includeIf "gitdir:"`）→ 不报守卫；gitfile 改指 `<main>/.git` → `hooks_path: Some("<main>/.git/hooks")`；工作区的 `.git` 不存在 → 只比镜像、不报守卫；gitfile 是坏的，或 `extensions.worktreeConfig` 打开后 `git config --worktree core.hooksPath ""`（git 拒绝的空路径）→ `Unknown`（`hooks-lookup`，re-review r4 的 N2）；影子仓库 config 的 include 指向 FIFO → `Unknown`（`timeout`），约 10 s；每次调用前后影子仓库与工作区的字节都不变（review M3） | 新增 |
| `attach_worktree_makes_git_work_in_workspace` | 第 1 代在临时树上 attach、再由测试把它改名到工作区路径之后：HEAD detached 在 head、没有 upstream；`git status --porcelain` 为空，`node_modules/` 与只靠 `info/exclude` 忽略的 `.env` 不出现；`git log -1` 经 alternates 是 head；`git rev-parse --git-common-dir` 是影子仓库；会话里 `git commit` 的对象在影子仓库的 `objects/`，主 checkout 里没有；`git fetch origin` 失败、`origin/main` 解析不到；`git push` 被 `pre-push` 拒绝；主 checkout 的 `git worktree list` 不列出它，index、HEAD、`refs/*`、`hooks/`、`config` 不变；对 `.env`、`node_modules/x` 与一个已跟踪的文件，`is_ignored_in_export(导出, head 的 tree, p)` 与 `is_ignored(工作区, p)` 相同 | 新增 |
| `user_hooks_path_cannot_disable_the_pre_push_guard` | 沙箱 `HOME` 的全局配置设了 `core.hooksPath`（指向一个空目录）：工作区里 `git push <远端地址> HEAD:refs/heads/x` 仍以 1 退出，远端没有这个分支，`git config --show-origin --get core.hooksPath` 给出影子仓库的 config 与 `<dir>/hooks` | 新增 |
| `attach_worktree_builds_a_fresh_admin_dir_per_generation` | 第 2 代在新的临时树上 attach：建出 `worktrees/2/`（HEAD 为新 head，index 等于新 tree），`worktrees/1/` 的每个字节不变，临时树的 `.git` 是指向第 2 代的 gitfile，字节恰好是 `gitdir: <Shadow::dir()>/worktrees/2` 加一个 LF（fix round 5 的裁定），临时 index 与它的目录都已删掉；把它交换到工作区路径之后 `git status --porcelain` 为空；已存在的 `worktrees/<n>/` → `Exists`，什么都不写；`generation = 0` → `InvalidInput` | 新增 |
| `attach_refuses_existing_dot_git` | `tree_dir/.git` 已存在（文件、目录或符号链接）→ `Exists`，不建管理目录，什么都不改 | 新增 |
| `attach_at_move_refuses_a_symlinked_worktrees_dir` | 第 1 代 attach 之后，测试把 `<shadow>/worktrees` 挪开，换成指向 `<main>/.git/worktrees`（人自己的 linked worktree 登记在那里）的符号链接；为第 2 代在新的临时树上 attach → `NotDirectory`，什么都没写：`<main>/.git/worktrees/` 的列表与每个字节不变，主 checkout 的 `git worktree list` 不变，临时树里没有 `.git`；挪回真目录之后同样的 attach 照常建出第 2 代（review B3） | 新增 |
| `attach_index_copy_refuses_a_swapped_admin_dir` | 主 checkout 里有人暂存的文件。为第 2 代在新的临时树上 attach，`pause_attach_before_index` 让它停在第 2 步之后；测试把 `worktrees/2` 挪开、换成指向 `<main>/.git` 的符号链接，再放行 → `NotDirectory`；主 checkout 的 index 逐字节不变，`git status --porcelain` 仍是那一行 `A`，`<main>/.git` 里没有多出条目；`take_git_calls` 里 read-tree 与 refresh 的 `GIT_INDEX_FILE` 在临时树的 `.git/` 下、`--git-dir` 是影子仓库，没有一次指向管理目录，read-tree 另带同一路径的 `--index-output`（breaker 裁定）；之后在另一棵临时树上为第 3 代 attach 照常成功（re-review r3、fix round 4 的裁定） | 新增 |
| `attach_readback_refuses_a_swapped_staging` | 为第 2 代在新的临时树上 attach，`pause_attach_before_readback` 让它停在 3.2 之后，测试在这里动临时树再放行：(a) `tree_dir/.git` 挪开、换成一个放好别的 index 的目录；(b) `.git` 挪开又挪回；(c) `tree_dir` 挪开又挪回；(d) `tree_dir` 的上级挪开，原路径换上一个新的上级，里面有同名的目录与 `.git/index` → 四种都是 `Unknown`（`detail.reason = "staging-changed"`）；(e) `tree_dir/.git/index` 换成指向 `<main>/.git/index` 的符号链接 → `Unknown`（`detail.reason = "index-read-back-refused"`：`tree_dir` 自己没变，核对放行，读回不跟随这个链接）；这五种之后 `worktrees/2/` 里都没有 `index`；(f) 什么都不动 → 照常成功，与不停住时的结果相同（不误报）；(g) `tree_dir` 的上级挪开，原路径换上一个新的上级，其中 `<tree_dir 的名字>` 是指向主 checkout 工作树根的符号链接 → `Unknown`（`detail.reason = "staging-changed"`），不是类型化的 `NotDirectory`（R 的 ctime 不变，重新打开失败；re-review r5 的 I-1），`worktrees/2/` 里同样没有 `index`；(h) 与 (g) 同一个上级符号链接替换，放在 `pause_attach_before_read_tree`（3.1 已 open_root 并记下 (dev, ino, ctime)，read-tree 还没跑；不是 `pause_attach_before_index`，那个停在 3.1 之前）。测试在这里种上替换，并让下一次 read-tree 因 tree 参数 `missing-tree` 以非零退出：停住时从主 checkout 的对象库去掉第 1 步 `tree_of(head)` 得到的 oid（影子仓库经 alternates 读它），argv 仍带这个 oid，git 因对象不存在以非零退出 → 仍是 `Unknown`（`detail.reason = "staging-changed"`），不是类型化的 `NotDirectory`，也不是不带理由的 git 退出码，`worktrees/2/` 里没有 `index`。主 checkout 的 index 每次都逐字节不变（re-review r4 的 N6，fix round 5 的缓解，breaker 裁定，module-rulings "An attach that may have written outside its staging is raised to the human"） | 新增 |
| `attach_runs_no_planted_hook_and_starts_no_fsmonitor` | 第 1 代 attach 之后，在 `<shadow>/hooks/` 里放一个会写标记文件的 `post-index-change`（钩子管理工具就会往这里写钩子），沙箱用户级 config 设 `core.fsmonitor = true` 与 `core.splitIndex = true`：为第 2 代 attach 之后，标记文件不存在；影子仓库与临时树里没有 `fsmonitor--daemon*`、`sharedindex.*`，也没有为它起的 fsmonitor 守护进程；`take_git_calls` 里 read-tree 与 refresh 的 argv 带 `-c core.hooksPath=/dev/null`、`-c core.fsmonitor=false`、`-c core.splitIndex=false`（不关时 git 会跑这个钩子，还会在影子仓库里建 `fsmonitor--daemon.ipc`，实测，§5.1） | 新增 |
| `worktree_gitdir_reads_the_gitfile_without_following_links` | 第 1 代 attach 之后，`worktree_gitdir(tree_dir)` 是 `Some(attach 返回的管理目录)`，按字节相等；停在 `pause_attach_before_index` 时临时树还没有 `.git` → `None`；`.git` 不存在、`tree_dir` 不存在 → `None`；gitfile 的行尾是 CRLF、路径两侧有空格 → 只去掉行尾；`gitdir: ../x` → `Some(<tree_dir>/../x)`，路径里仍有 `../x`，不按词法消掉 `..`；`gitdir: <shadow>/worktrees/1/`（末尾带 `/`）→ `Some(<shadow>/worktrees/1/)`，只去掉行尾，末尾的 `/` 还在，不是 `InvalidInput`（`root_not_canonical`）：这个函数不当根打开；`.git` 是指向一份有效 gitfile 的符号链接 → `NotFile`，那份 gitfile 里的路径不出现在任何结果里；`.git` 是目录（主 checkout 的工作树根；一个空目录，即 attach 中断在第 3 步时的样子）→ `IsDirectory`；`.git` 是 FIFO → `NotFile`，不挂住；`tree_dir` 是指向这棵树的符号链接 → `NotDirectory`；`gitdir:x`、`gitdir: ` 之后为空、不以 `gitdir: ` 开头、含 NUL、超过 `8 + PATH_MAX_BYTES + 2` 字节 → `InvalidInput`（`bad-gitfile`）；`tree_dir` 不是绝对路径 → `InvalidInput`；每次调用前后 `tree_dir` 一字不动（session.md @cec336b §2 的上游要求） | 新增 |
| `shadow_reads_never_hang_on_non_regular_files` | 影子仓库的 `config` 换成 FIFO → `shadow_at` 是 `Unknown`；`objects/info/alternates` 换成指向同样内容的符号链接 → `shadow_at` 是 `Unknown`；影子仓库的 `info/exclude` 换成 FIFO → `shadow_drift` 报 `Exclude`，换成指向同样内容的符号链接 → 同样报 `Exclude`，`is_ignored(工作区, p)` 是 `Unknown`；主 checkout 的 `info/exclude` 换成指向同样内容的符号链接 → `shadow_drift` 为空，对新的 `dir` 调 `ensure_shadow` 写出的镜像是同样字节的普通文件；主 checkout 的 `info/exclude` 换成 FIFO → `shadow_drift` 是 `Unknown`，对新的 `dir` 调 `ensure_shadow` 是 `NotFile` 且 `dir` 没有被建出；管理目录的 `index` 换成 FIFO → `read_index(工作区)` 与 `is_ignored(工作区, p)` 是 `Unknown`；`worktrees/<n>` 换成指向主 checkout `.git/worktrees/<人自己的 linked worktree>` 的符号链接 → `read_index(工作区)` 是 `Unknown`，从不返回那个 linked worktree 的已跟踪集合。每次调用都在 5 s 内返回（看门狗）（module-rulings "gitio reads files through apfs"） | 新增 |
| `read_index_reads_ignorecase_from_a_strata_built_shadow` | 大小写不敏感的卷上（主 checkout 的 `core.ignorecase = true`）：`ensure_shadow`、`attach_worktree` 之后对工作区 `read_index` → `ignorecase` 为 true，`is_tracked` 按 ASCII 不分大小写命中；把影子仓库 config 里这一行手改成 `ignorecase = "true"`、`ignorecase = true # x` 或只写 `ignorecase`，同一个 `Git` 再读照样为 true，改成 `ignorecase = false` 就为 false（config 一变缓存就失效；review B1）。工作区 gitfile 改成 `gitdir: <attach 返回的管理目录>/`（路径末尾多一个 `/`）再 `read_index` → 仍读到这份 index（`ignorecase` 仍为 true），不是 `InvalidInput`（`root_not_canonical`）：路径在当根用之前按词法规整（§5.7 第 1 步） | 新增 |
| `sessions_never_share_refs` | 同一个 repo 的两个会话各有影子仓库：会话 A 的工作区里 `git stash`、`git branch`、`git tag`、本地 commit 只出现在 A 的影子仓库里；B 的影子仓库与主 checkout 的 `for-each-ref` 都不变 | 新增 |
| `index_writers_refuse_the_main_checkout` | 把主 checkout 或人自己的 linked worktree 当 `staging` 传给 `stage`、`push`、`attach_staging`，把 `gitdir` 放在 common dir 里，把 `ensure_shadow` 的 `dir`、`attach_worktree` 的 `tree_dir` 或 `workspace` 放在主 checkout 里 → `InvalidInput`；主 checkout 的 index 字节不变 | 新增 |
| `attach_staging_commits_land_in_main_store` | `attach_staging` 之后 HEAD detached 在 head、`git status --porcelain` 为空；`stage` 与 `commit_tree` 写出的对象在主 checkout 的对象库里，`refs/strata/publish/*` 可以指向它；主 checkout 的 `git worktree list` 不列出 staging，index 与 HEAD 不变；把 strata 的工作区当 staging 传给 `stage` → `InvalidInput` | 新增 |
| `stage_after_export_roundtrips_to_same_tree` | 导出一个带 `eol=crlf`、smudge/clean 过滤器、符号链接、可执行文件的 tree，`attach_staging`，内容不改就 stage 全部路径 → `write-tree` 得到原 tree OID | 新增 |
| `stage_applies_clean_filters` | staging 树里改过的 CRLF 文件与过滤器文件，stage 之后的 blob 是检入形态（LF、clean 过滤后的字节），与在普通 checkout 里 `git add` 同样内容得到的 blob 相同；update-index 的子进程没有 `LC_ALL`（`take_git_calls`） | 新增 |
| `stage_handles_deletions_and_type_transitions` | 批次含删除、文件变目录（`a` 删、`a/b` 增）、目录变文件、文件与符号链接互换：结果 tree 与在普通 checkout 里做同样改动再 `git add -A` 的 tree 相同 | 新增 |
| `stage_never_adds_unlisted_paths` | 批次只点 `a/b`，`a/` 下还有没选的文件与被忽略的文件：结果 tree 只多出 `a/b`；不在批次里的已跟踪文件即使在磁盘上改过，也保持 index 里的原样 | 新增 |
| `stage_refuses_special_files_before_writing` | 批次路径是 FIFO → `NotFile`，index 不变 | 新增 |
| `commit_tree_creates_single_parent_commit` | 结果 commit 的 tree、唯一的 parent、消息都与输入逐字节相同，作者取 git 配置；`take_git_calls` 里 commit-tree 的 argv 带 `-c user.useConfigOnly=true`、env 带 `LC_ALL=C`；不动任何 ref；配置里有 `commit.gpgSign=true` 也不签名、不挂住 | 新增 |
| `commit_tree_without_identity_is_invalid_input` | `set_env` 让 `HOME` 指向没有 `user.email` 的配置、`EMAIL` 有值 → `InvalidInput`（`detail.reason = "git-identity"`），不会用自动探测的身份做出 commit；只缺 `user.name` 同样；不移动任何 ref | 新增 |
| `push_unknown_on_timeout` | 远端的 receive-pack 挂住超过 `timeout` → `Ok(PushOutcome::Unknown)`；整个进程组被杀；发布链与 `refs/strata/remotes` 不变 | layerfs-bridge/tests/install_records.rs::installed_and_refused_keep_original_publication_knowledge |
| `push_without_ref_status_is_unknown` | 远端在 `reference-transaction` 钩子的 `committed` 阶段杀掉 receive-pack：远端分支其实已更新，push 没有状态行、退出 128 → `Unknown` 而不是 `Refused` | 新增 |
| `push_refused_carries_remote_message` | 非快进 → `Refused`，含 `non-fast-forward`；远端 pre-receive 拒绝 → `Refused`，含 `pre-receive hook declined` 与钩子的 `remote:` 原话；远端分支不变 | 新增 |
| `push_pushed_fast_forward_new_and_up_to_date` | 快进、新分支、已是最新（`=`）三种都是 `Pushed`；之后 `refs/strata/remotes/<remote>/<dst>` 等于推送的 commit，主 checkout 的 `refs/remotes/<remote>/<dst>` 不变；推送期间 `refs/strata/remotes/<remote>/<dst>` 被改过时，第 5 步不动它。机制是主 checkout 里的固定装置 `hooks/pre-push`：它用 `git update-ref` 把这个 ref 改到另一个 commit，模拟推送期间落地的 fetch（publish 的推送照跑人自己的 pre-push 钩子，review M16、N9） | 新增 |
| `push_sets_strata_publish_and_never_forces` | `take_git_calls` 里 push 的子进程环境有 `STRATA_PUBLISH=1`、没有 `LC_ALL`；argv 里没有 `--force`、`--force-with-lease`、`+` 开头的 refspec、`--no-verify`；`src_ref` 不在 `refs/strata/` 下 → `Err(InvalidInput)`，不存在 → `Err(NoSuchCommit)`；`staging` 不是 attach 过的 staging 树 → `Err(InvalidInput)` | 新增 |
| `push_runs_human_pre_push_in_staging_root` | 主 checkout 自己的 `hooks/pre-push` 在推送时运行，它看到的 cwd 与 `GIT_WORK_TREE` 是 staging 树根；钩子以 1 退出时没有状态行 → `Ok(Unknown)`，远端分支不变 | 新增 |
| `push_to_is_pinned_against_url_rewriting` | （review-5 N1）有一条会把 A 改写到 `alt/<名>` 的前缀 `insteadOf` / `pushInsteadOf` 时，`push_published_to(.., A, ..)` 落在 A、alt 不变；再加一条匹配串恰好是 A 的 `pushInsteadOf`（指向 B）→ `push_url_exact(A)` 为 false，`push_recorded_to` → `InvalidInput`（`url-rewritten`），A、B、锚点都不动；名叫 `host:n1.git` 的 remote → `url-is-remote-name`；含换行的地址 → `bad-url`。去掉钉住与检查时推送落在 alt，测试失败 | 新增 |
| `push_to_refuses_a_bare_name_with_a_legacy_remote_file` | （review-6 漏洞 1）`remote.origin.url` 设为裸名 `legacy`，common dir 下有 `remotes/legacy`（`URL: <B>`）；再换成 `legacy2` 与 `branches/legacy2`（`<B>#main`）。两种都 `push_url_exact` 为 false、`push_published_to` → `InvalidInput`（`url-is-remote-name`），A、B、锚点都不动；删掉文件后同一裸名不再算 remote 名。去掉这条检查时推送落在 B，测试失败 | 新增 |
| `push_to_rule_added_between_check_and_push_is_refused` | （review-6 漏洞 2）`pause_push_after_check` 暂停时加一条 `url.<B>.pushInsteadOf=A`，推送返回后删掉：`push_published_to(.., A, ..)` → `InvalidInput`（`url-rewritten`），A、B、锚点、跟踪 ref 都不动，`push_url_exact(A)` 事后为真（推后核对查不出），私有 git 目录里没有留下 `strata-push-*`；没有这条规则时同样的推送落在 A。去掉守卫钩子时推送落在 B、返回 `Pushed`，测试失败 | 新增 |
| `push_human_pre_push_runs_nested_hooks_without_the_guard_override` | （review-7）人的 `pre-push` 里跑 `git hook run pre-commit`（人的 `pre-commit` 写标记）并记下 `git config core.hooksPath` 与 `GIT_CONFIG_COUNT`、`STRATA_PUSH_GUARD_SEEN`、`STRATA_USER_PRE_PUSH`：推送 `Pushed`，嵌套的 `pre-commit` 跑了，`core.hooksPath` 为空，三个变量都未设。用 `-c core.hooksPath` 时嵌套钩子找不到、推送 `Unknown`，测试失败 | 新增 |
| `push_to_sends_to_the_given_url_not_the_config` | （review-4 F1）`remote.origin.pushurl = B` 时 `push_published_to(.., A, ..)` 落在 A、B 不变，tracking ref 与锚点照常前移；`push_recorded_to` 同样；空 url → `InvalidInput`（`empty-url`），远端不变 | 新增 |
| `push_url_names_the_push_destination` | （review-3 R4）`push_url("origin")` 等于 `remote.origin.url`；设了 `remote.origin.pushurl = B`（另一个本地 bare 仓库）之后等于 B，此时 push 落在 B、不落在 url 指的远端；没有的 remote → `InvalidInput`（`no-such-remote`），名字不合法（`-x`）→ `InvalidInput` | 新增 |
| `merge3_nonoverlapping_hunks_clean` | 两边的改动之间至少隔一行未改的 base 行 → `Clean`，结果同时含两边的改动；恰好隔一行也干净 | 新增 |
| `merge3_same_hunk_conflict` | 两边把同一行改成不同内容 → `Conflict`，一个 Hunk，base、ours、theirs 三段文本与起始行号都对 | 新增 |
| `merge3_adjacent_changes_conflict` | 两边各改相邻的一行，中间没有未改行 → `Conflict`；两边在文件末尾追加不同的行 → `Conflict` | 新增 |
| `merge3_identical_or_one_sided_changes_clean` | 两边改成同样的内容 → `Clean`，只取一份；只有一边改 → `Clean`，即那一边；base 为空串而两边相同（add/add）→ `Clean` | 新增 |
| `merge3_preserves_line_endings` | CRLF 文件按行合并且保留 CRLF；最后一行没有换行符也照样保留 | 新增 |
| `merge3_binary_is_whole_file` | 任一侧是二进制且两边都改了 → `Conflict`，恰好一个整文件 Hunk；只有一边改 → `Clean`，即那一边 | 新增 |
| `merge3_agrees_with_git_merge_file_on_corpus` | 固定语料（每例的行级 diff 唯一）上，`Clean` 与 `Conflict` 的判定与 `git merge-file -p` 的退出码一致，`Clean` 时字节相同 | 新增 |
| `is_binary_matches_git_rule` | 前 8000 字节有 NUL → 二进制；NUL 在第 8001 字节 → 不是；空串不是 | 新增 |
| `main_checkout_untouched_except_strata_refs` | 沙箱用户级 config 设了 `core.fsmonitor = true` 与 `core.splitIndex = true`；对一个主 checkout 依次跑 `open`、`fetch`、`tree_of`、`tree_contains`、`export_tree`、`is_ignored_in_export`、`ensure_shadow`、`attach_worktree`、`shadow_drift`、`read_index`、`is_ignored`（工作区与影子仓库在别处），再在一棵 staging 树上跑 `attach_staging`、`stage`、`commit_tree`、`update_ref`、`push` 之后：主 checkout 的工作树文件、`<common>/index` 的字节、`HEAD`、`config`、`hooks/`、`info/`、`refs/heads/*`、`refs/remotes/*`、`refs/tags/*`、`refs/stash`、`FETCH_HEAD`、`<common>/worktrees/` 都与之前逐字节相同，没有多出 `fsmonitor--daemon*` 与 `sharedindex.*`，也没有留下 fsmonitor 守护进程，变化只有 `objects/` 与 `refs/strata/*` | layerfs-api/tests/local_install.rs::import_installs_the_store_in_process |

共 126 个用例：55 个引用矩阵行，71 个新增。被引用的不同矩阵行共 57 行：3f 目标为 gitio 的全部 43 行（按 3f@63587e5；`non_unicode_repository_roots_are_lossless_but_tracked_paths_are_unsupported` 的目标已是 gitio），3d 目标为 gitio 的全部 10 行，以及目标不是 gitio、但理由点名 gitio 用例的四行：3f 的 `linked_worktrees_and_equal_trees_share_one_arc`（search）、`interrupted_and_failed_publications_are_not_discoverable`（deps）、`sha256_repositories_and_unicode_paths_are_supported`（`—`），3a 的 `a_component_name_is_accepted_at_255_bytes_and_refused_at_256`（`—`）。其中 3f `linked_worktrees_and_equal_trees_share_one_arc` 与 3d `all_supported_kernel_paths_survive_install_with_full_oracles` 的理由仍写着工作区的 `.git` 接到主 checkout（会话目录与主 checkout 发现同一个仓库；`.git` 是指向主 checkout 的 gitfile），影子仓库的裁定之下不再成立，由 T15 改；本文引用它们时按 module-rulings Addendum 2 断言。3d `interrupted_transfer…` 一行的两条保证，gitio 只承担"中断的导出不建目标、保留临时目录"那一条；并发的那一半归 session（§1）。

理由点名 gitio、但本文不为它们单列用例的行：

- 3f tgrep-core/src/git_index.rs::metadata_paths_preserve_native_bytes：`—`、不保留（3f@63587e5 已从"保留"改过来，APFS 不产生非 UTF-8 名）；理由说 gitio 借用的 `git_index.rs` 仍按字节解析。gitio 不迁移这条测试，照抄的 `metadata_path` 仍按字节读 gitfile 与 `commondir` 的路径。
- 3f tgrep-core/src/walker.rs::public_matcher_lazily_freezes_tracked_exemptions_on_first_match：不保留；理由说 `read_index` 的返回值是快照，由 `read_index_snapshot_is_immutable_and_cached` 断言。
- 3f tgrep-core/src/walker.rs::the_point_query_matcher_hides_exactly_what_the_walk_hides（目标 overlay）、tgrep-core/src/worktrees/tests.rs::membership_visibility_binary_size_and_storage_rules_follow_the_requesting_root 与 tgrep-core/tests/generations.rs::builds_committed_blobs_not_staged_dirty_or_untracked_contents（目标 search）：理由引用 gitio 的两条规则，即增量点查与全量对账用同一个 `is_ignored`、已跟踪永不 ignore；gitio 一侧由 `case_sensitive_repo_matches_exactly` 与 `index_parse_tracked_never_ignored` 覆盖。
- 3a layerfs-persistence/tests/merge_records.rs::records_are_private_guarded_and_removed、3c layerfs-overlay/tests/indexed_operation_record.rs::complete_keys_and_full_file_scopes_never_alias_other_custody、3c layerfs-store-service/tests/kept_plans.rs::a_full_walk_answer_is_kept_like_a_plan_and_let_go_like_one：不保留；理由说 strata 的合并由 `merge3` 在 staging 目录里完成，不落库，也不缓存比较计划。`merge3` 是纯函数，没有可测的状态。
- 3c layerfs-overlay/tests/indexed_operation_record.rs::guarded_batches_are_atomic_capacity_bounded_and_allow_a_128_child_node_shape：不保留；理由指向本文的 `update_ref_cas_refuses_moved_ref`，表里已列为新增。
- 3b layerfs-project/tests/acquisition_custody.rs::a_backing_read_failure_is_definite_and_is_cleaned_up：目标 session；理由里 gitio 的 `NoSuchBranch` 经 `session::new` 原样透传，gitio 一侧由 `fetch_missing_branch_is_no_such_branch` 覆盖。

## 8. 实现任务（TDD）

前提：workspace 根 `Cargo.toml` 与 `crates/strata-types` 已经存在（types.md §8）；Task 2–8 还要 `crates/strata-apfs` 里 §2 列出的函数已经实现（apfs.md §8；Task 2、3 的读经 `open_rooted_file`，G11）。每个任务的测试都用 `tests/common/mod.rs` 的辅助函数在临时目录里建仓库、人自己的 linked worktree、影子仓库与它的工作区、staging 树和裸远端，需要 `PATH` 里有 git ≥ 2.44；测试起的每个 git 子进程都带这个用例自己的临时 `HOME` 与 `XDG_CONFIG_HOME`，外加 `GIT_CONFIG_NOSYSTEM=1`，而且在 `open` 之前就设好（review N8）：辅助函数在自己起的每个 `Command` 上设这三个变量；打开仓库一律用 `Git::open_with_env`（§3 的测试接缝），所以 `open` 自己的子进程也读不到真实的 `~/.gitconfig` 与系统配置（`commit.gpgSign`、`init.templateDir`、`core.hooksPath`）；之后要换全局配置用 `set_env`。测试从不改测试进程的环境，唯一的例外是 `command_strips_inherited_git_env`（它单独一个测试二进制）。测试只用临时目录，从不碰 `/Volumes/GWD/inter`。§3 的测试接缝只在 cargo feature `test-seams` 打开时编译，所以每条 `cargo test` 都带 `--features test-seams`。

### Task 1: 子进程环境与仓库身份

**Files:**
- Create: `crates/strata-gitio/Cargo.toml`（`[features]` 里声明 `test-seams = []`）、`crates/strata-gitio/THIRD-PARTY-NOTICES`、`crates/strata-gitio/src/lib.rs`、`crates/strata-gitio/src/command.rs`（抄 tgrep `generations/git.rs`，带出处头注释）、`crates/strata-gitio/src/repo.rs`
- Modify: `Cargo.toml`（把 `crates/strata-gitio` 加进 `members`）
- Test: `crates/strata-gitio/tests/repo.rs`、`crates/strata-gitio/tests/command_env.rs`（只放 `command_strips_inherited_git_env`，它改父进程环境）、`crates/strata-gitio/src/repo.rs` 的单测（`old_git_is_unsupported`）、`crates/strata-gitio/tests/common/mod.rs`

**Interfaces:**
- Consumes: `strata_types::{RepoId, StrataError, Code}`
- Produces: `Result`、`MIN_GIT_VERSION`、`Git::open`、`Git::common_dir`、`Git::repo_id`、`Git::command`（`pub(crate)`）；测试接缝 `GitCall`、`Git::open_with_env`、`Git::set_env`、`Git::take_git_calls`

- [ ] **Step 1: 写失败的测试**：§7 的 `command_strips_inherited_git_env`、`command_sets_safety_flags`、`open_resolves_repo_id_from_main_checkout_only`（strata 工作区那一半先用辅助函数手工建：带 `strata.shadow` 标记的影子仓库加 gitfile）、`open_refuses_non_repository_bare_and_missing`、`sha256_repository_is_unsupported`、`reftable_repository_is_unsupported`、`old_git_is_unsupported`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-gitio --features test-seams --lib repo:: && cargo test -p strata-gitio --features test-seams --test repo --test command_env`
Expected: FAIL（`Git` 还没定义，编译不过）

- [ ] **Step 3: 写让测试通过的最少实现**：照抄 tgrep 的 `command()`，补上去掉 `STRATA_PUBLISH`、每个子进程加 `-c core.fsmonitor=false` 与按命令加 `LC_ALL=C`（§5.1）；`open` 按 §5.2；版本解析写成私有纯函数，`src/repo.rs` 的单测直接喂字符串；测试接缝 `open_with_env`、`set_env` 与 `take_git_calls`：环境在起子进程的那一处套上，调用也在那一处记下，`open` 自己起的也算进返回的 `Git`。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-gitio --features test-seams --lib repo:: && cargo test -p strata-gitio --features test-seams --test repo --test command_env`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/strata-gitio
git commit -m "feat(gitio): sanitized git command and repository identity"
```

### Task 2: index 解析与 `read_index`

**Files:**
- Create: `crates/strata-gitio/src/index.rs`（抄 tgrep `git_index.rs`，带出处头注释）
- Modify: `crates/strata-gitio/Cargo.toml`（依赖加 `strata-apfs`，§2；review B6；G11 的读从这个任务起经它）
- Test: `crates/strata-gitio/src/index.rs` 的单测（迁移 tgrep 的测试）、`crates/strata-gitio/tests/index.rs`

**Interfaces:**
- Consumes: `RepoPath`；`Code::{NoSuchPath, NotText, InvalidInput, Unsupported, Unknown}`；Task 1 的 `Git`；`strata_apfs::{open_rooted_file, classify_io_error, IoSide, PATH_MAX_BYTES}`
- Produces: `IndexInfo`、`IndexInfo::is_tracked`、`IndexInfo::tracked_under`、`Git::read_index`

- [ ] **Step 1: 写失败的测试**：§7 从 `metadata_paths_strip_only_git_line_endings` 到 `read_index_refuses_worktree_of_another_repo` 的 24 个（影子仓库的工作区用 Task 1 的辅助函数手工建）
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-gitio --features test-seams --lib index:: && cargo test -p strata-gitio --features test-seams --test index`
Expected: FAIL（`IndexInfo`、`read_index` 还没有）

- [ ] **Step 3: 写让测试通过的最少实现**：按 §2 的改动表抄入解析器，`config_bool` 按 §5.7 第 6 步的 git 语法重写；`read_index` 按 §5.7（经 gitfile 到管理目录、核对归属、缺 index 为 `no-index`）；非 UTF-8 返回 `NotText`；按 index 文件的 stat 缓存；所有读经 `apfs::open_rooted_file`，根按 G11 取，gitfile 的字节解析（去掉 `gitdir: ` 前缀，再 `metadata_path`）写成 Task 7 的 `worktree_gitdir` 也用的内部函数，不用 `read_git_dir_using`（它经 `fs::metadata` 跟随符号链接）。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-gitio --features test-seams --lib index:: && cargo test -p strata-gitio --features test-seams --test index`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-gitio/Cargo.toml crates/strata-gitio/src/index.rs crates/strata-gitio/src/lib.rs crates/strata-gitio/tests/index.rs crates/strata-gitio/tests/common/mod.rs
git commit -m "feat(gitio): parse the git index and config directly"
```

### Task 3: `is_ignored`

**Files:**
- Create: `crates/strata-gitio/src/ignore.rs`
- Test: `crates/strata-gitio/tests/ignore.rs`、`crates/strata-gitio/tests/fixtures/ignore-corpus/`

**Interfaces:**
- Consumes: `Git::read_index`、`IndexInfo::is_tracked`；`strata_apfs::{open_rooted_file, attrs_of, classify_io_error, IoSide}`
- Produces: `Git::is_ignored`

- [ ] **Step 1: 写失败的测试**：§7 从 `index_parse_tracked_never_ignored` 到 `is_ignored_agrees_with_git_on_corpus` 的 19 个
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-gitio --features test-seams --test ignore`
Expected: FAIL（`is_ignored` 还没有）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.8 的优先级栈、父目录排除、ASCII 折叠；全局排除文件的路径按 common dir 解析并缓存（§4.3）；规则文件按 stat 缓存，按 G11 经 `apfs::open_rooted_file` 读（工作树里的 `.gitignore` 是符号链接或目录当作没有，本仓库 common dir 的 `info/exclude` 与全局排除文件先 `realpath`）；匹配引擎按 §9 选定。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-gitio --features test-seams --test ignore`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-gitio/src/ignore.rs crates/strata-gitio/src/repo.rs crates/strata-gitio/src/lib.rs crates/strata-gitio/tests/ignore.rs crates/strata-gitio/tests/fixtures/ignore-corpus
git commit -m "feat(gitio): git ignore rules with tracked files never ignored"
```

### Task 4: 远端、ref 与祖先

**Files:**
- Create: `crates/strata-gitio/src/remote.rs`、`crates/strata-gitio/src/refs.rs`
- Test: `crates/strata-gitio/tests/refs.rs`

**Interfaces:**
- Consumes: Task 1 的 `Git::command`；`Code::{InvalidInput, NoSuchBranch, NoSuchCommit, HeadMoved, Unknown}`
- Produces: `Git::fetch`、`Git::read_ref`、`Git::is_ancestor`、`Git::merge_base`、`Git::update_ref`、`Git::delete_ref`、`remote_ref`；测试接缝 `Git::set_fetch_timeout`

- [ ] **Step 1: 写失败的测试**：§7 的 `option_like_arguments_are_refused_not_executed`、`fetch_writes_only_the_strata_tracking_ref`、`fetch_missing_branch_is_no_such_branch`、`fetch_timeout_is_unknown_and_moves_nothing`、`ancestry_answers_come_from_git`、`read_ref_absent_is_none`、`update_ref_cas_refuses_moved_ref`、`update_ref_create_requires_absent`、`refs_outside_strata_namespace_are_refused`、`delete_ref_is_compare_and_swap`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-gitio --features test-seams --test refs`
Expected: FAIL（这些函数还没有）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.3 与 §5.10；名字校验先于子进程；超时杀进程组；测试接缝 `set_fetch_timeout`。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-gitio --features test-seams --test refs`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-gitio/src/remote.rs crates/strata-gitio/src/refs.rs crates/strata-gitio/src/lib.rs crates/strata-gitio/tests/refs.rs
git commit -m "feat(gitio): fetch into refs/strata, ancestry and compare-and-swap refs"
```

### Task 5: `tree_of`、`tree_contains`、tree 校验与 `is_ignored_in_export`

**Files:**
- Create: `crates/strata-gitio/src/tree.rs`（解析部分抄 tgrep `parse_entries`）
- Modify: `crates/strata-gitio/src/ignore.rs`（加 `is_ignored_in_export`，复用 Task 3 的规则栈）
- Modify: `crates/strata-gitio/tests/common/mod.rs`（加 `archive_tree_into(repo, tree, dir)`：`git -C <repo> archive --format=tar <tree>` 经管道交给 `tar -x -C <dir>`。`export_tree` 要到 Task 6 才有，`is_ignored_in_export` 的四个用例先用它把 tree 放进 `dir`；这些用例的 tree 不带 `export-ignore`、`export-subst` 属性，放出来的与导出的相同；re-review r4 的 N7）
- Test: `crates/strata-gitio/tests/tree.rs`、`crates/strata-gitio/tests/ignore.rs`（`is_ignored_in_export` 的四个用例）

**Interfaces:**
- Consumes: Task 1 的 `Git::command`；`RepoPath`；Task 2 的 config 读法；Task 3 的规则栈与全局排除文件的解析
- Produces: `Git::tree_of`、`Git::tree_contains`、`Git::is_ignored_in_export`

- [ ] **Step 1: 写失败的测试**：§7 的 `replace_refs_do_not_change_tree_of`、`gitlink_in_base_tree_is_unsupported`、`missing_blob_refused_before_export`、`non_utf8_tree_path_is_not_text`、`colliding_paths_are_unsupported`、`dot_git_component_is_unsupported`、`overlong_component_refused_before_writing`、`ls_tree_output_parse_is_exact`、`tree_validation_is_cached_per_tree`、`tree_contains_answers_from_the_validated_tree`、`is_ignored_in_export_ignores_seeds_like_the_workspace`、`is_ignored_in_export_never_ignores_a_tracked_path`、`is_ignored_in_export_honours_negation`、`is_ignored_in_export_reads_nested_gitignore_from_dir`（gitlink、碰撞、`.git` 分量、超长四条里 `export_tree` 的那一半在 Task 6 补断言）
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-gitio --features test-seams --test tree --test ignore`
Expected: FAIL（`tree_of`、`tree_contains`、`is_ignored_in_export` 还没有）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.4；大小一栏为 `BAD` 返回 `NoSuchCommit`；结论按 `TreeOid` 缓存，路径表按最近使用留 64 个；`is_ignored_in_export` 按 §5.8 末段：已跟踪查路径表，`.gitignore` 从 `dir` 读，`info/exclude`、全局排除文件与 `core.ignorecase` 取主 checkout 的。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-gitio --features test-seams --test tree --test ignore`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-gitio/src/tree.rs crates/strata-gitio/src/ignore.rs crates/strata-gitio/src/lib.rs crates/strata-gitio/tests/tree.rs crates/strata-gitio/tests/ignore.rs crates/strata-gitio/tests/common/mod.rs
git commit -m "feat(gitio): resolve and validate base trees, judge ignores on an export"
```

### Task 6: `export_tree`

**Files:**
- Create: `crates/strata-gitio/src/export.rs`
- Test: `crates/strata-gitio/tests/export.rs`

**Interfaces:**
- Consumes: `Git::tree_of`；`strata_apfs::{mkdir_rooted, open_rooted, open_root, rename_rooted, RenameMode, remove_rooted, RemoveKind, full_fsync, classify_io_error, IoSide, PATH_MAX_BYTES, NAME_MAX_UTF16_UNITS}`（§2）；`libc::fsync`
- Produces: `Git::export_tree`、`EXPORT_TEMP_PREFIX`；测试接缝 `SyncEvent`、`Pause`、`Git::take_sync_events`、`Git::pause_export_before_rename`

- [ ] **Step 1: 写失败的测试**：§7 的 `export_tree_matches_git_checkout_byte_for_byte`、`export_filter_failure_matches_checkout`、`export_tree_refuses_existing_destination_untouched`、`export_typed_refusals_write_nothing`、`interrupted_export_keeps_temp_and_never_creates_destination`、`concurrent_export_never_overwrites_destination`、`export_large_tree_is_complete`、`export_is_durable_before_rename`；补上 Task 5 留下的导出断言
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-gitio --features test-seams --test export -- --include-ignored`
Expected: FAIL（`export_tree` 还没有）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.5 的七步；私有 `GIT_INDEX_FILE`；持久化（`fsync`、`full_fsync`）与 `rename_rooted` 都经一个同步层，`test-seams` 下它把每次调用记进 `take_sync_events`；`pause_export_before_rename` 停在 §5.5 第 6 步之前。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-gitio --features test-seams --test export -- --include-ignored`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-gitio/src/export.rs crates/strata-gitio/src/lib.rs crates/strata-gitio/tests/export.rs crates/strata-gitio/tests/tree.rs
git commit -m "feat(gitio): export trees byte-identical to git checkout"
```

### Task 7: 影子仓库与 `attach_worktree`

**Files:**
- Create: `crates/strata-gitio/src/shadow.rs`、`crates/strata-gitio/src/worktree.rs`
- Modify: `crates/strata-gitio/tests/repo.rs`（`open_resolves_repo_id_from_main_checkout_only` 的工作区一半改用 `ensure_shadow` 与 `attach_worktree` 建）
- Test: `crates/strata-gitio/tests/shadow.rs`

**Interfaces:**
- Consumes: `Git::tree_of`、`Git::export_tree`、`Git::read_index`、`Git::is_ignored_in_export`；Task 2 的 gitfile 字节解析（§5.7 第 1 步）；Task 6 的同步层与 `Pause`；`strata_apfs::{create_file_rooted, mkdir_rooted, open_root, open_rooted, open_rooted_file, attrs_of, remove_rooted, RemoveKind, classify_io_error, IoSide}`
- Produces: `Shadow`、`Shadow::dir`、`MirrorDrift`、`Git::ensure_shadow`、`Git::shadow_at`、`Git::shadow_drift`、`Git::attach_worktree`、`Git::worktree_gitdir`；测试接缝 `Git::pause_attach_before_index`、`Git::pause_attach_before_read_tree`、`Git::pause_attach_before_readback`

- [ ] **Step 1: 写失败的测试**：§7 从 `exported_workspace_git_status_is_clean` 到 `sessions_never_share_refs` 的 20 个
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-gitio --features test-seams --test shadow`
Expected: FAIL（`ensure_shadow`、`shadow_at`、`attach_worktree` 还没有）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.6 的 `ensure_shadow`、`shadow_at`、`shadow_drift`、`attach_worktree`：`git init` 的三个参数；config 整份写进临时文件再 `rename`，值照 git 的写法（§5.6 第 7 步），钉住 `core.hooksPath`，标记最后；`shadow_at` 只读；`shadow_drift` 另查 pre-push 守卫：有效 hooks 目录在工作区的上下文里问 git，影子仓库 config 的 include 条目与 `extensions.*` 键报成漂移，守卫按 `Shadow::dir()` 加 `/hooks` 比；管理目录每代新建，经 apfs 的 rooted 函数，根是影子仓库目录；index 由 git 建在 `tree_dir/.git/` 下（`--git-dir` 是影子仓库，显式设 `GIT_INDEX_FILE`，read-tree 另带同一路径的 `--index-output`，关钩子、fsmonitor 与 split index，600 s 超时），复制之前用 3.1 之后拿的 `open_root(tree_dir)` 句柄核对暂存（ctime、重新打开是否成功与 (dev, ino)，任何一项不对都是 `staging-changed`；3.2 失败或超时也先做这个核对，核对失败是 `staging-changed`，通过才返回 3.2 自己的 `Unknown`；读回被拒为 `index-read-back-refused`），再 rooted 地复制进管理目录、保留 mtime；`pause_attach_before_index` 停在 §5.6 `attach_worktree` 第 2 步之后，`pause_attach_before_read_tree` 停在 3.1 已 open_root 并记下 (dev, ino, ctime) 之后、read-tree 之前，`pause_attach_before_readback` 停在 3.2 之后；gitfile 经 `apfs::create_file_rooted` 新建，字节恰好是 `gitdir: `、`Shadow::dir()`、`/worktrees/`、十进制的 n 与一个 `\n`；持久化经 Task 6 的同步层；`worktree_gitdir` 按 §5.6 末段：一次 `apfs::open_rooted_file`，`NoSuchPath` 为 None，只共用 Task 2 的字节解析，读至多 N + 1 字节；`shadow_at`、`shadow_drift`、`ensure_shadow` 的读按 G11。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-gitio --features test-seams --test shadow && cargo test -p strata-gitio --features test-seams --test repo`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-gitio/src/shadow.rs crates/strata-gitio/src/worktree.rs crates/strata-gitio/src/lib.rs crates/strata-gitio/tests/shadow.rs crates/strata-gitio/tests/repo.rs crates/strata-gitio/tests/common/mod.rs
git commit -m "feat(gitio): per-session shadow repositories and workspace generations"
```

### Task 8: `attach_staging`、`stage`、`commit_tree`

**Files:**
- Create: `crates/strata-gitio/src/staging.rs`、`crates/strata-gitio/src/stage.rs`、`crates/strata-gitio/src/commit.rs`
- Test: `crates/strata-gitio/tests/staging.rs`

**Interfaces:**
- Consumes: `Git::tree_of`、`Git::export_tree`、`Git::read_ref`；Task 7 建 index 与写 gitfile 的内部函数
- Produces: `Git::attach_staging`、`Git::stage`、`Git::commit_tree`

- [ ] **Step 1: 写失败的测试**：§7 从 `index_writers_refuse_the_main_checkout` 到 `commit_tree_without_identity_is_invalid_input` 的 9 个
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-gitio --features test-seams --test staging`
Expected: FAIL（这三个函数还没有）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.6 的 `attach_staging`、§5.9、§5.10 的 `commit_tree`（`-c user.useConfigOnly=true`，`auto-detection is disabled` 映射为 `git-identity`）；staging 树的判定按 §5.9 第 1 步；主 checkout 的判定按 §5.2 第 7 步记下的工作树根。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-gitio --features test-seams --test staging`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-gitio/src/staging.rs crates/strata-gitio/src/stage.rs crates/strata-gitio/src/commit.rs crates/strata-gitio/src/lib.rs crates/strata-gitio/tests/staging.rs
git commit -m "feat(gitio): attach publish staging, stage through clean filters, commit trees"
```

### Task 9: `push`

**Files:**
- Create: `crates/strata-gitio/src/push.rs`
- Test: `crates/strata-gitio/tests/push.rs`（裸远端、挂住的 receive-pack 包装脚本、`reference-transaction` 钩子、记下 cwd 的主 checkout `pre-push` 钩子、推送期间改 `refs/strata/remotes/…` 的主 checkout `pre-push` 钩子都由辅助函数生成）

**Interfaces:**
- Consumes: `Git::read_ref`、`Git::update_ref`、Task 8 的 staging 判定；types.md 的 `PushOutcome`
- Produces: `Git::push`

- [ ] **Step 1: 写失败的测试**：§7 的 `push_unknown_on_timeout`、`push_without_ref_status_is_unknown`、`push_refused_carries_remote_message`、`push_pushed_fast_forward_new_and_up_to_date`、`push_sets_strata_publish_and_never_forces`、`push_runs_human_pre_push_in_staging_root`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-gitio --features test-seams --test push`
Expected: FAIL（`push` 还没有）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.11；在 staging 树根里推到 `get-url --push` 的地址；只按本 ref 的 porcelain 状态行归类；`Pushed` 后比较并交换地更新 `remote_ref`；原始输出经 `tracing` 记录。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-gitio --features test-seams --test push`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-gitio/src/push.rs crates/strata-gitio/src/lib.rs crates/strata-gitio/tests/push.rs
git commit -m "feat(gitio): push with a definite outcome or Unknown"
```

### Task 10: `merge3` 与 `is_binary`

**Files:**
- Create: `crates/strata-gitio/src/merge.rs`
- Test: `crates/strata-gitio/tests/merge.rs`、`crates/strata-gitio/tests/fixtures/merge-corpus/`

**Interfaces:**
- Consumes: §9 选定的 diff crate
- Produces: `merge3`、`Merge3`、`Hunk`、`is_binary`

- [ ] **Step 1: 写失败的测试**：§7 从 `merge3_nonoverlapping_hunks_clean` 到 `is_binary_matches_git_rule` 的 8 个
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-gitio --features test-seams --test merge`
Expected: FAIL（`merge3` 还没有）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.12；相邻即冲突，同改合一；二进制按整文件处理。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-gitio --features test-seams --test merge`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-gitio/src/merge.rs crates/strata-gitio/src/lib.rs crates/strata-gitio/tests/merge.rs crates/strata-gitio/tests/fixtures/merge-corpus
git commit -m "feat(gitio): hunk-level three-way merge matching git merge-file"
```

### Task 11: 不变量 2 的集成测试

**Files:**
- Test: `crates/strata-gitio/tests/invariants.rs`

**Interfaces:**
- Consumes: Task 1–10 的全部公开函数
- Produces: 无新接口

- [ ] **Step 1: 写失败的测试**：§7 的 `main_checkout_untouched_except_strata_refs`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-gitio --features test-seams --test invariants`
Expected: FAIL（测试文件刚写，快照比较的辅助函数还没有；若前面的任务有遗漏，例如漏设 `GIT_INDEX_FILE`、推到了 remote 名、或把工作区接到了主 checkout，也在这里暴露）

- [ ] **Step 3: 写让测试通过的最少实现**：补齐快照辅助函数（对主 checkout 的工作树、`<common>/index`、`HEAD`、`config`、`hooks/`、`info/`、各类 ref、`FETCH_HEAD` 与 `<common>/worktrees/` 做逐字节快照）；若比较失败，修对应的函数，不放宽断言。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-gitio --features test-seams`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-gitio/tests/invariants.rs crates/strata-gitio/tests/common/mod.rs
git commit -m "test(gitio): the main checkout changes only in objects and refs/strata"
```

## 9. 构建者决定记录

| 事项 | 选择 | 理由 | 日期 |
| --- | --- | --- | --- |
| 工作区 `.git` 的接法（design §6.1 第 2 步、§12 交给构建者） | module-rulings Addendum 2 的裁定，不是 [我的决定]：每会话、每 repo 一个影子仓库，工作区是它的 linked worktree，每代一个新的管理目录；只有 publish 的 staging 树经 `attach_staging` 接到主 checkout 的 common dir；`Git::command` crate 私有 | §5.6 的四条理由，实测可行（§5.6）。主 checkout 的 `git gc` 看不到影子仓库：session 的 pin ref 保住钉住的 commit 与 tree，会话自己的对象在影子仓库里，不受主 checkout 的 gc 影响；staging 写进主对象库的 blob 靠 git 对新松散对象的宽限期撑过一次发布 | 2026-10-22 |
| 影子仓库的建法 | [我的决定] `git -c init.defaultRefFormat=files init --bare --template= --object-format=sha1`；config 由 gitio 整份重写 | 全局的 `init.*` 配置会让影子仓库变成 SHA-256、reftable 或带模板钩子（实测）；手写的管理目录要求 files 引用存储 | 2026-10-22 |
| 影子仓库的完整标记与归属核对 | [我的决定] config 里最后写的 `strata.shadow = true`；alternates 指向本仓库的 `objects` | `ensure_shadow` 据此认出完整的影子仓库（半成的返回 `Exists`），`shadow_at` 据此只读地认出它；`open` 据此拒绝工作区（`detail.reason = "strata-workspace"`），strata 从不由工作区里的 git 推出 `RepoId`；`read_index`、`is_ignored`、`attach_worktree`、`shadow_drift` 据此核对工作区借的是本仓库的对象库 | 2026-10-22 |
| 复制的配置键 | session.md §4.2 的清单，[我的决定] 再加 `core.excludesFile`、`core.attributesFile` | 主 checkout 在仓库级设了它们时，工作区里 git 的 ignore 与属性才与主 checkout 一致；session.md 的清单待 T9 补上 | 2026-10-22 |
| 影子仓库 config 的写法与读法（review B1、M2） | [我的决定] 写：照 git 自己的写法，只在 git 也会加引号时加（首尾空格、`;`、`#`、回车）。读：照抄的 `config_bool` 改照 git 2.54 的解析器（`parse_file`、`get_value`、`parse_value`、`git_parse_maybe_bool`）：BOM、两类空白、键名与段头的字符集、整个文件的坏行、每一次出现的值、32 位整数与不分大小写的后缀（§5.7 第 6 步）；git 判坏的一律 `Unknown`（`bad-config`）；文件里有 NUL 字节也是 `Unknown`，哪怕 git 接受；读 index 的缓存也按 config 的 stat 失效 | 原来的写法给每个值加引号，原来的读法不认引号：`shadow_at` 认不出任何 strata 建的影子仓库，工作区的 `core.ignorecase` 读成 false（re-review r2 把照抄的代码编译出来验证过）。agent 的 git 命令与手改都会改写活的影子仓库的 config，所以读的一侧也照 git。re-review r3 又找出五种与 git 不同的形式（M2），fix round 4 照 git 的源码逐条补齐，四个探针脚本在 git 2.54 上实测了 124 个形式（有少量重复）。fix round 5 按 re-review r4 的 N1 改了值末尾空白的去法：后面还有引号或 `\` 序列时留下，11 个形式与 4 个对照在 git 2.54 上实测。NUL 的处理随位置而变，不值得复刻：说不准就 `Unknown`，从不猜。`config_values_read_like_git` 与 git 对拍 | 2026-10-22 |
| 读法与 include（review M3） | [我的决定] gitio 进程内的 config 读法（`core.ignorecase`、`strata.shadow`）不跟随 `include.path`、`includeIf.<条件>.path`；影子仓库 config 里出现的 include 条目与 `extensions.*` 键由 `shadow_drift` 报成 `Config { key }` 漂移 | 跟随就要读 agent 指定的任意文件，包括 FIFO（实测 git 读到指向 FIFO 的 include 就一直挂住），`includeIf` 还要照 git 的上下文求值（gitdir、onbranch、hasconfig）。`ensure_shadow` 从不写 include，出现就是后加的，status 由此看得出读法可能与 git 不同。代价：agent 在影子仓库里经 include 改了 `core.ignorecase` 时，gitio 读到的仍是 config 本身的值，与工作区里的 git 不同，直到 status 的告警被处理。22 个仓库的仓库级 config 都没有 include。`extensions.*` 键同样报（re-review r4 的 N4）：`extensions.worktreeConfig` 打开之后 git 还读管理目录的 `config.worktree`，那里的 include 不在影子仓库 config 的列表里（实测） | 2026-10-22 |
| 工作区 `pre-push` 守卫的范围 | 影子仓库的 config 钉住绝对的 `core.hooksPath = <dir>/hooks`（review N1 的裁定）。守卫挡住习惯性与意外的推送，包括推到显式地址；挡不住有意的绕过：push 时跳过钩子的那个选项、命令行上的 `-c core.hooksPath=…`、有意导出的 `STRATA_PUBLISH=1`。在工作区里改影子仓库 config 的 `core.hooksPath` 却常常不是有意的：husky、lefthook 这类钩子管理工具在工作区里安装时就会改钩子路径，或往钩子目录里写自己的钩子，守卫随之失效（review B4）；22 个仓库今天都不用它们（2026-10-22 查过各仓库根目录的 `.husky`、`lefthook*.yml` 与 package.json）。`shadow_drift` 在工作区的上下文里问 git 自己用哪个 hooks 目录（跟随 include、读 `config.worktree`、`includeIf` 按管理目录求值；review M3），不是 `<dir>/hooks`（按 realpath 比，即 `Shadow::dir()` 加 `/hooks`，re-review r4 的 N8）就报 `PrePushGuardDisabled`（status 的 reason 是 `pre_push_guard_disabled`），影子仓库 config 里后加的 include 另报一项 `Config`；[我的决定] `hooks/pre-push` 被换掉也报这一项。gitio 不修，是否重新钉住由 daemon 决定（module-rulings 的 T13 条）。钉住的代价：用户级与系统级 hooksPath 里的其他钩子在工作区里都不跑（review B5） | design §6.8 的守卫是守卫，不是安全边界。re-review r2 实测：在工作区里把钩子路径改到 `.husky/_` 改写了影子仓库的 config，之后从工作区推到显式地址成功。实测：用户级或系统级设了 hooksPath 而影子仓库没钉时推送成功，钉了就被拒；跳过钩子的那个选项与 `-c core.hooksPath=/dev/null` 都能绕过。re-review r3 与 fix round 4 实测：影子仓库 config 的本地列表看不见 include 进来的、`config.worktree` 里的与用户级的钩子路径（钉住的键被删之后），也看不见被改指主 checkout 的 gitfile；这四种之下有效目录都在别处，前三种推到显式地址都成功，所以只能问 git 的有效值 | 2026-10-22 |
| `includeIf` 引入的条件配置 | [我的决定] 不处理，记为限制：用户级 config 以 `includeIf "gitdir:…"` 只给主 checkout 引入的配置，在 staging 树的 push 与 `stage` 里不生效（凭据、http、钩子与 `filter.*` 这些键），在工作区里也不生效；`onbranch:` 条件同样不生效（HEAD 是 detached 的）。`hasconfig:remote.*.url:` 在 staging 里生效（它读主 common dir 的 remote），在没有 remote 的工作区里不生效。gitio 在主 common dir 上跑的命令（`fetch`、`export_tree`、`remote get-url`）照常读到这些配置，所以条件配置里的过滤器会让导出与 `stage` 不对称 | staging 与工作区的 git 目录都不在主 checkout 之下（三种条件都实测过）。要让这些配置生效，写进主 checkout 的仓库级 config：staging 直接读到，工作区只复制 §4.2 清单里的键。T10 publish 重复这条限制（review N3） | 2026-10-22 |
| 主仓库的引用存储 | [我的决定] `extensions.refStorage` 不是 `files` → `Unsupported` | `attach_staging` 手写 detached `HEAD`，reftable 下 HEAD 不在这个文件里；22 个仓库的 config 都没有 `extensions.*` | 2026-10-22 |
| ignorecase 下的大小写折叠 | [我的决定] 只按 ASCII 折叠，与 git 相同：`is_tracked`、`tracked_under`、`tree_contains`、ignore 模式匹配、tree 的碰撞检查 | `git status` 就是这样判定的（实测模式 `É.txt` 不命中 `é.txt`）。只差 Unicode 大小写的路径归 overlay，用 `apfs::case_fold_eq`（module-rulings Addendum 2）。types.md:124 与 3f:155 写的是 `apfs::case_fold_eq`，由 T15 统一，本文不改它们 | 2026-10-22 |
| 三方合并的行级 diff 实现（design §5、§12） | 待定 | 约束：判定与 `git merge-file` 相同（§5.12），§7 的 merge3 用例全绿，包括与 git 的语料对拍。候选：imara-diff 加自写的区间合并；diffy 的 merge | — |
| gitignore 匹配引擎 | 待定 | 约束：§5.8 的优先级、父目录排除、ASCII 折叠，`is_ignored_agrees_with_git_on_corpus` 全绿。候选：`ignore` 0.4 的逐文件 `Gitignore`，由 gitio 叠优先级（它的 `case_insensitive` 按 Unicode 折叠，要改成 ASCII；tgrep-core 依赖它的一份打过补丁的副本，两份并存时要留意）；gix-ignore | — |
| git 版本下限 | 2.44（review M6 的裁定：第一个认 `GIT_NO_LAZY_FETCH` 的发行版，按它的发行说明；实现时再核实） | 更低的 git 会悄悄从 promisor 远端补对象，G7 不成立；本机 2.54.0 | 2026-10-22 |
| 子进程的 locale | [我的决定] `LC_ALL=C` 只加在要按原话分类 stderr 的命令上 | 分类要读 git 的英文原话；会跑过滤器与 pre-push 钩子的命令继承 daemon 的 locale（review M15）。代价：`fetch` 起的凭据助手与 ssh 传输、`update-ref` 与 `fetch` 触发的 `reference-transaction` 钩子也继承 `LC_ALL=C`，只改它们输出的语言（review N5） | 2026-10-22 |
| fetch 超时 | [我的决定] 120 s 的常量，首个 spike 后修正 | 网络操作必须有界，否则 `new` 握着 per-repo 写锁一直等 | 2026-10-22 |
| strata 的远端跟踪 ref | [我的决定] `refs/strata/remotes/<remote>/<branch>`，fetch 带 `--refmap=`；`Pushed` 之后比较并交换地更新 | 不写 `refs/remotes/*`（§4.1）；推送期间被 fetch 改过就留给 fetch 的值 | 2026-10-22 |
| 推送地址 | [我的决定] 推到 `git remote get-url --push` 给出的地址 | 推到 remote 名会更新 `refs/remotes/*`（实测） | 2026-10-22 |
| 公开推送地址 | [实现时补的决定] 第 2 步的读法提成公开的 `push_url`，push 自己也用它 | review-3 R4：daemon 要记下一次推送的目的地，只在推到同一目的地时结清保留的现场 | 2026-10-24 |
| 推到给定地址 | [实现时补的决定] `push_published_to` / `push_recorded_to`：第 2 步用调用方给的地址，不读 config | review-4 F1：同一份读法读两次，两次之间 config 可能被改，记下的地址就不是实际推的地址。daemon 读一次、记下、原样传进来，两者才一致 | 2026-10-24 |
| 钉住推送地址（review-5 N1） | [实现时补的决定] 推送子进程带恒等的 `insteadOf` / `pushInsteadOf`（`--config-env`），能赢过它的 config（等长规则、同名 remote，含老式 `remotes/` / `branches/` 文件，review-6 漏洞 1）在发送前拒绝；公开 `push_url_exact` 供推后核对 | review-5 N1：git 对命令行给的地址照样套改写规则，已改写过的地址会被再改一次。git 没有关掉改写的开关，也不能从命令行删掉别处的规则；最长匹配的恒等规则是能做到的最可靠的钉法，剩下赢不了的情形先查出来拒绝，查与推之间的窗口由调用方推后核对兜底 | 2026-10-24 |
| D25 推送进程自己核对目的地（review-6 漏洞 2） | [实现时补的决定] 推送子进程经 `GIT_CONFIG_COUNT` 带 `core.hooksPath=<守卫目录>`（守卫转给人的钩子之前撤掉，review-7），守卫 `pre-push` 记下并核对 git 实际要发送去的地址，不等于钉住的地址就让 git 什么都不发，等于再转给人自己的 `pre-push`；见 §5.11 第 3 步 | review-6 漏洞 2：检查与推送是两个进程，各读一次 config，中间加的规则会把推送带到 B。评审建议的“一份不可变的 config 快照”做不全：`GIT_CONFIG_GLOBAL` / `GIT_CONFIG_SYSTEM` 能换掉全局与系统层，仓库自己的 config（common dir 的 `config`）却没有环境变量能换，要冻结它只能让推送换一个私有 git 目录，钩子看到的 ref、`GIT_DIR` 与凭据配置都会变。守卫在推送进程里、在 git 那唯一一次 config 读取之后核对它真正要去的地址，检查和推送用的就是同一份 config（同一进程读的那一份），没有窗口；所有 config 来源（含老式 remote 文件）都覆盖，钩子、凭据、`sshCommand` 照旧 | 2026-10-24 |
| pre-push 钩子 | [我的决定] 不带 `--no-verify`；推送在 staging 树根里跑（review M16 的裁定），人自己的 `pre-push` 照跑 | 仓库自己的钩子照跑，cwd 是要发布的内容；钩子拒绝时没有状态行，报 `Unknown` | 2026-10-22 |
| 导出的持久化与原子性 | [我的决定] 每个文件与目录 `fsync`（`libc::fsync`），一次 `apfs::full_fsync`，再 `apfs::rename_rooted(…, RenameMode::Excl)`；要 `fsync` 的路径取自 tree 的路径表，不遍历目录 | §5.5：基线目录只以完整的形态出现，竞争下也不覆盖。路径表就是 git 写出的全部条目，按它逐个打开没有 `apfs::walk` 的深度上限 | 2026-10-22 |
| gitio 往工作区、staging 树、导出目标旁与活的影子仓库里的写 | module-rulings Addendum 7 的 Layering 条、Addendum 8 与它的 clarification 的裁定：都经 apfs 的 rooted 函数（gitfile 用 `create_file_rooted`；导出用 `mkdir_rooted`、`rename_rooted`、`remove_rooted`；move 时的管理目录用 `mkdir_rooted`、`create_file_rooted`，根是影子仓库目录），gitio 自己不做 `*at` 调用。`new` 时影子仓库还没有工作区，`ensure_shadow` 的写仍按路径。[我的决定] 导出以 `dst` 的父目录为根，`T` 建成 0700，`T/tree` 由 gitio 建成 0755 | Addendum 3 要求往工作区或 staging 树里的写都不跟随符号链接，Addendum 8 让这类操作只有 apfs 一份实现。gitio 因此依赖同在第 1 层的 apfs（apfs 只依赖 types，没有环），T15 把 types.md §8 第 1 层一行改成"同层只允许 store 与 gitio 依赖 apfs"。`T/tree` 要预先建好：工作树目录不存在时 checkout-index 以 128 退出（实测）。树里的文件由 git 写。影子仓库在 `new` 之后是 agent 可写的地方，re-review r2 用指向 `<main>/.git/worktrees` 的符号链接把第 2 代的管理目录引进了主 checkout（review B3）。管理目录的 index 也是 gitio 的 rooted 写：git 把 index 建在 `tree_dir/.git/` 下，gitio 复制进来（re-review r3 实测旧的做法在 `worktrees/<n>` 被换成指向 `<main>/.git` 的符号链接时重写了主 checkout 的 index）。剩下的按路径的 git 写只有那个临时 index，见"活影子仓库的写入清单"一行 | 2026-10-22 |
| attach 的 index 怎样进管理目录（re-review r3、fix round 4 的裁定） | 裁定：git 把 index 建在 strata 自己的临时路径（移动的暂存里），gitio 再 rooted 地复制进 `worktrees/<n>/index`，git 从不打开管理目录。[我的决定] 临时路径是 `tree_dir/.git/index`（临时目录 0700，与第 4 步的 gitfile 同名，复制之后删掉）；副本的 mtime 设成临时 index 的 mtime；`-c core.splitIndex=false` | `move_workspace` 时 `tree_dir` 就是 session 的移动暂存，裁定说的"移动的暂存里"就是它。`.git` 这个名字第 1 步已确认不存在，tree 里也不可能有（§5.4 拒绝 `.git` 分量），所以不必另起名字，碰不上树里的文件；崩溃时留下的也只在调用方的暂存里，会话根下不多出一类残留。git 按 index 文件的 mtime 判定 racy 条目，保留 mtime 让副本与 git 写的那份等价。用户级 `core.splitIndex = true` 实测不拆私有 index，显式关掉是为了不靠这条内部规则 | 2026-10-22 |
| 活影子仓库的写入清单与剩下的暴露（module-rulings 的 Addendum 8 clarification；fix round 4、5） | 清单见 §5.6 末尾：`new` 之后 strata 往影子仓库里写的，只有 move 时 gitio 经 rooted 函数建的管理目录与其中的五个文件；git 命令自己一个字节都不往里写（ref、reflog、对象、config 都没有：pin ref 在主 checkout，`stage` 与 `commit_tree` 写主对象库；refresh 跑的过滤器命令另算，见 (2)）。fix round 4 去掉的暴露：管理目录的 index 改成 rooted 的副本；index 命令不跑钩子，所有 git 命令不起 fsmonitor（§5.1）。剩下三项都要有意的篡改，按 pre-push 守卫已经裁定的威胁模型（review N1）记下最坏情形，以及之后 status 显示什么。(1) 临时 index 仍由 git 按路径写在 `tree_dir/.git/` 下：有人卡着时间把 `tree_dir/.git` 换成指向 `<main>/.git` 的符号链接（或把 `tree_dir` 换成指向主 checkout 工作树根的符号链接），git 就经它重写主 checkout 的 index，人暂存的改动丢失。fix round 5 加了 3.3 的核对（re-review r4 提的缓解）：3.1 之后拿着 `tree_dir` 的句柄 R，读回之前 R 的 ctime 变了、或重新打开 `tree_dir` 失败、或打开的已不是同一个目录 → `Unknown`（`staging-changed`）。APFS 改名条目时改所在目录的 ctime，被改名的目录自己的 ctime 也变，所以 `.git` 或 `tree_dir` 本身被换走，不论换没换回，都落到 `Unknown`（re-review r4 在 git 2.54 上实测：干净的 attach 不误报，`.git` 与 `tree_dir` 的换走又换回都拦下）；读回被拒另报 `index-read-back-refused`。这两个理由之下 move 返回带这个理由的 `Unknown`，session 照它第 13 步里 attach 失败的常规处理（健康仍是 Ok，报 `MoveLeftover`，由下一次 move、`rm` 或 recover 退役：暂存不是要紧的证据），daemon（T13）把它当告警交给人，点名 `<main>/.git/index`，请人在主 checkout 里看 `git status` 与 `git diff --cached`（module-rulings "An attach that may have written outside its staging is raised to the human"；re-review r4 increment 指出原来写的"session 保留现场"不对）。换了上级而没换回（原路径上的 `<tree_dir 的名字>` 成了符号链接）时，R 的 ctime 不变，重新打开失败，同样落到 `Unknown`（`staging-changed`），而不是类型化的 `NotDirectory`——若是那样，session 会悄悄退役暂存，主 checkout 的 index 已被重写却没有告警（re-review r5 的 I-1 实测，breaker 裁定）。拦不下的最坏情形是换上级又换回：会话目录被挪走、换上一个假的，里面的 `.strata-tmp-move-<n>` 是指向主 checkout 工作树根的符号链接，git 写完再挪回真的，并在真的 `tree_dir/.git/` 里放好一份 index——`tree_dir` 自己的 ctime 与 (dev, ino) 都没变（往 `.git/` 里放文件改的是 `.git` 的 ctime），gitio 照常复制，move 成功，status 什么都不显示，工作区拿到的是放进去的那份 index。往 `tree_dir/.git/` 里放一个指向 `<main>/.git/index` 的符号链接，read-tree 不再经它写：它带 `--index-output`，经锁文件加改名在这个路径上写出新 index，链接被换掉，主 checkout 的 index 逐字节、连 inode 都不变（re-review r5 实测，breaker 裁定采纳）。剩下的是 read-tree 之后再放一个链接：refresh 写 index 时经锁文件跟随它（git 2.54 实测，链接本身留着），按链接目标自己的条目重写目标——只刷新 stat 信息，人暂存的条目还在；目标不是 index 时解析失败，什么都不写。这个链接可以指向这个用户写得到的任何 index，`<main>/.git/index` 只是最可能的一个。链接留到读回时读回被拒（`index-read-back-refused`，交给人）；refresh 之后又被换成一份普通文件时 `tree_dir` 的 ctime 与 (dev, ino) 都没变，无声。3.2 本身失败或超时（比如影子仓库 config 里一个挂住的 clean 过滤器让 refresh 到 600 s 被杀）时，先做同一个 3.3 核对再返回：暂存被换过 → `staging-changed`，告警；核对通过 → 3.2 自己的 `Unknown`。核对之后、读回之前的窗口里再换，git 已经跑完，只让读回读到换来的那份，主 checkout 不会因此被写。会误报的：refresh 期间 clean 过滤器在它的 cwd（`tree_dir`）里建、删或改名文件，或别的进程改了 `tree_dir` 本身的属性（哪怕是把它 chmod 成原来的模式、或给它加一个 xattr，re-review r5 实测都会误报）——move 以 `Unknown` 结束，是安全的一侧。无声的那几种情形下，主 checkout 的 index 被重写既不在 strata 的 status 里，也没有告警；read-tree 经替换写过的那几种，人自己的 `git status` 看得到暂存的改动不见了（refresh 经链接的那一种只刷新 stat 信息）。(2) refresh 跑影子仓库 config 里的过滤器命令：agent 改写 `filter.<driver>.clean` 或 `.process`（或在影子仓库的 `info/attributes` 里给路径配上新的过滤器），strata 就以同一个用户替它跑那条命令，最长 600 s（§5.1）；status 报对应的 `Config` 漂移（`filter.<driver>.clean` 这类键）与 `Attributes` 漂移。配了 git-lfs 时（主 checkout 的 `filter.lfs.*` 照清单复制过来），它的 clean 还按路径往影子仓库的 `lfs/` 下写对象缓存（re-review r4 的 N3；导出时写的是主 checkout 的那份，§5.5 第 4 步）：agent 把 `lfs/` 换成符号链接时，这份缓存写到链接指向的地方，status 不显示。(3) 影子仓库 config 的 include 指向 FIFO：每个读它的 git 命令都挂住；strata 的子进程到时被杀，status 的漂移检查报 `Unknown`（`timeout`），move 在 600 s 后得到 `Unknown`，现场保留 | (1) `tree_dir` 是 strata 的暂存（session 的 `.strata-tmp-move-*`，名字随机），agent 的 git 从不往那里写；影子仓库的 `worktrees/` 却是 agent 自己的 git 也写的地方（`git worktree add`、`prune`），旧的做法正暴露在那里。按路径的 git 写去不掉：git 只认路径；3.3 的核对因此只看这份暂存，改看主 checkout 的 index 前后，主 checkout 里每一次正当的 git 都会触发它（fix round 4 对 #4 的裁定）。(2) refresh 要照工作区里 git 的规则判断内容，过滤器只能取影子仓库的；改取主 checkout 现在的，镜像漂移时会把改过的文件记成干净的。(3) 本地命令原本不设超时，status 与握着写锁的 move 都不能无限挂住 | 2026-10-22 |
| gitio 自己起的 git 不起 fsmonitor，index 命令不跑钩子 | [我的决定] 每个 git 子进程都带 `-c core.fsmonitor=false`；`read-tree`、`update-index`、`checkout-index`、`write-tree` 再带关掉钩子的 `-c core.hooksPath=/dev/null`；`push` 照跑钩子 | 实测：用户级 `core.fsmonitor = true` 让 `export_tree` 那样的 checkout-index 在主 checkout 的 `.git` 里建了 `fsmonitor--daemon.ipc` 与 `fsmonitor--daemon/`、留下一个守护进程（违反 G1），让 move 时的 refresh 在影子仓库里建了 `fsmonitor--daemon.ipc`；`post-index-change` 钩子在 read-tree 与 refresh 之后都会跑，影子仓库的 hooks 目录 agent 可写。gitio 的命令只碰自己的私有 index，用不着这两样；命令行的 `-c` 压过各级 config 与 include | 2026-10-22 |
| 本地命令的超时 | [我的决定] 只给会读活影子仓库 config 的子进程：只读的查询（`shadow_drift` 的三次、全局排除文件的查询）各 10 s，`attach_worktree` 的 read-tree 与 refresh 各 600 s；其余本地命令不设 | 实测 git 的每个命令启动时都跟随仓库 config 的 include，一条指向 FIFO 的 include 让 `config --local --list`、`config --get`、`rev-parse` 都一直挂住；status 与 overlay 的对账不能因为一个会话的影子仓库挂住，move 握着 per-repo 写锁。600 s 远大于 refresh 的正常耗时（O(源码字节数)，tool 约 4.2K 个文件），首个 spike 后修正 | 2026-10-22 |
| `is_ignored_in_export` 的"已跟踪"（session 的上游要求） | [我的决定] tree 路径表里的文件与符号链接条目；路径表里的目录不算，按规则回答 | 检出之后 index 里正是这些条目，这样它与工作区建好之后的 `is_ignored` 对每个路径都给同一个答案（`is_ignored` 里目录从不算已跟踪）。裁定写的是 `tree_contains(tree, path)`：对文件两者相同；`tree_contains` 对目录也答"在"（本表 `tree_contains` 对目录一行），照搬就会让两个函数对目录给出不同答案。session 先用 `tree_contains` 拒掉被跟踪的种子，只拿不在 tree 里的路径来问，对它没有差别 | 2026-10-22 |
| `worktree_gitdir` 的答案与码（session 第二轮的上游要求） | [我的决定] `tree_dir` 或 `.git` 不存在 → `None`；apfs 的拒绝原样返回：`.git` 是目录 → `IsDirectory`，是符号链接、FIFO、socket 或设备 → `NotFile`，`tree_dir` 是符号链接或不是目录 → `NotDirectory`；格式不对，以及含 NUL、长过 `8 + PATH_MAX_BYTES + 2` 字节 → `InvalidInput`（`bad-gitfile`）；相对路径照 git 接在 `tree_dir` 上；签名带 `&self`，同 session.md @cec336b §2 | 控制者的要求：目录、符号链接与格式不对各给一个类型化的码，码由我选。用 apfs 的码：`open_rooted_file` 一次调用就分清了，不另造一张表；`IsDirectory` 也正是 attach 中断在第 3 步时临时树的样子，调用方分得出"gitfile 还没写"。`tree_dir` 不存在也答 `None`，同 session.md 的写法：`NoSuchPath` 不必再分是哪一级，一次调用就答得出，不必先另查 `tree_dir`。session.md @cec336b 写的是 `.git` 不是普通文件时返回 None，本行按控制者的要求改成类型化的码，session 那一侧要跟着改 | 2026-10-22 |
| gitio 的进程内读（module-rulings "gitio reads files through apfs" 与 "…, refined"；review r4 的第 6 项，re-review r4 对这条裁定的 Important） | 裁定：别的进程也够得着的文件都经 `apfs::open_rooted_file` 读（G11）。refined：只有人写的文件——本仓库 common dir 里的 `info/exclude`、`info/attributes`、`config`、`config.worktree`，与用户级的排除文件（工作区的那份路径取自 agent 写得到的影子仓库 config，先 `realpath` 对它无害，G11；breaker 裁定）——先 `realpath` 再以所在目录为根，悬空的符号链接当作没有，指向 FIFO 这类的照各函数"读不了"；工作树里的 `.gitignore` 是符号链接或目录当作没有；agent 或 strata 写的（工作树里的文件、影子仓库、任何管理目录、任何一处的 `.git` 与 index）照旧不跟随。[我的决定] 根的取法：git 目录里的文件以 common dir 为根；工作树里的文件以参数本身为根，严格，只有参数与传给 `open` 的 `path` 逐字节相同时（主 checkout）用 `open` 算出的 `realpath(root)`（breaker 裁定，re-review r5 的 M-1：原来按 realpath 认主 checkout，被换成指向主 checkout 的符号链接的工作区会被当成主 checkout 来答）；`config`、`config.worktree` 归入人写的一类（我提出，控制者接受，写进 refined）；`realpath` 的 `ENOENT` 与 `ENOTDIR` 照不存在（同 git），其余错误（含 `ELOOP`）→ `Unknown`，有意取安全的一侧（breaker 裁定，re-review r5 的 M-3）；缓存按解析之后的文件（§4.3）。不是普通文件时各函数的结果：工作树里的 `.gitignore` 是符号链接或目录当作没有（同 git）；影子仓库一侧的镜像文件算漂移；`ensure_shadow` 在写之前返回 apfs 的码；staging 树的判定不满足；其余照各函数原来"读不了"的 `Unknown` | 裁定只定了读法与根；结果按 git 2.54 的实测与各函数已有的契约定：git 不读符号链接的 `.gitignore`（警告一句）与目录 `.gitignore`；FIFO 的 `.gitignore`、`info/exclude` 让 git 一直挂住，gitio 给出 `Unknown`，不挂也不猜。人写的文件跟随符号链接：dotfile 工具（stow 一类）常把它们做成符号链接；git 2.54 跟随符号链接的 `info/exclude`、`info/attributes`、`config`、`config.worktree` 与全局排除文件，悬空的当作没有（实测）；`ensure_shadow` 把读到的内容写成影子仓库里的普通文件。`config`、`config.worktree` 也归入这一类：strata 从不写它们，`open` 本来就经 git 子进程读主 checkout 的 config（跟随符号链接），进程内的读若不跟随，`open` 成功而 `read_index` 失败。剩下与 git 不同的：git 跟随影子仓库与管理目录里的符号链接、任何一处符号链接的 `.git` 与 index，gitio 不跟随，读到时报 `Unknown`（影子仓库的镜像文件报成漂移）；22 个仓库（连同工作区里没配置的 7 个仓库与 7 个 linked worktree）的 `.git`、index、config、`config.worktree`、`info/*`、`commondir`、`gitdir` 都没有符号链接，也没有 FIFO 这类文件（2026-10-22 只读核对）。主 checkout 用 realpath 作根：apfs 拒绝本身是符号链接的根；module-rulings "Repo paths in config are realpaths" 之后配置的 `path` 就是它的 realpath，`open` 本身仍接受符号链接的 `path`（§7 的一例），所以照旧取 `open` 算出的那个 | 2026-10-22 |
| tree 里的路径碰撞 | [我的决定] NFC 相同，或 ignorecase 下 ASCII 折叠相同，或同一个名字既是文件（符号链接）又是目录，都算碰撞，返回 `Unsupported`；只差 Unicode 大小写的放行 | §5.4；git 在 APFS 上检出只差 Unicode 大小写的一对同样只留一个文件，导出仍等于检出；同名的文件与目录 `git fsck` 判为 `duplicateEntries`，read-tree 却悄悄丢掉一条（实测） | 2026-10-22 |
| tree 里的 `.git` 分量 | [我的决定] `Unsupported` | tgrep 的 `validate_tracked_path` 同样拒绝；git 检出时拒绝写这样的路径（实测），到导出才发现就已经写了临时目录 | 2026-10-22 |
| `tree_contains` 对目录 | [我的决定] tree 里的目录也算"在 tree 里" | 与被跟踪目录同名的显式种子条目因此被拒，而不是把文件复制到目录的位置上 | 2026-10-22 |
| tree 路径表的缓存 | [我的决定] 最多 64 个 tree，按最近使用淘汰 | `tree_contains` 复用校验时的解析；丢掉只会变慢 | 2026-10-22 |
| sparse index | [我的决定] `Unsupported` | gitio 建的 index 总是完整的；影子仓库不复制 `index.sparse` | 2026-10-22 |
| index 文件不存在 | `Unknown`，`detail.reason = "no-index"`（review M7、module-rulings Addendum 2 的裁定） | gitio 总给工作区与 staging 建 index，缺了是异常；不当成"什么都没跟踪" | 2026-10-22 |
| push 与 fetch 的原始输出 | [我的决定] 经 `tracing` 记日志 | `PushOutcome::Unknown` 不带文字 | 2026-10-22 |
| 计划之外的公开项 | [我的决定] `common_dir`、`is_binary` 与两个常量 | daemon 报错时要给出路径；`Binary` 与 merge3 用同一个判定 | 2026-10-22 |
| 测试接缝（module-rulings Addendum 4） | [我的决定] cargo feature `test-seams` 下挂在 `Git` 上的九个入口：`open_with_env`、`set_env`、`set_fetch_timeout`、`take_git_calls`、`take_sync_events`、`pause_export_before_rename`、`pause_attach_before_index`、`pause_attach_before_read_tree`、`pause_attach_before_readback`（§3 末尾）；版本解析不开接缝，用 `src/repo.rs` 的单测 | 子进程的参数、环境与缓存命中只能在起子进程的那一处看到，而 `command` 是 crate 私有；持久化的顺序只能在同步层看到；120 s 的 fetch 超时在测试里太长；导出的改名竞态、attach 时管理目录被换、3.1 记下身份之后 read-tree 之前上级被换、refresh 之后暂存被换，都要停在两步之间才测得确定（Addendum 4：用停住接缝，不用 errno 故障点）；`set_env` 让每个用例有自己的全局配置与 `EMAIL`，不改测试进程的环境；`open_with_env` 让 `open` 自己的子进程也只读沙箱里的配置（review N8） | 2026-10-22 |
| 导出耗时 | 待定 | design §8 没有导出的数字；首个 spike 实测 tool（4196 个文件）与 staff-api（4947 个）后写回 | — |
