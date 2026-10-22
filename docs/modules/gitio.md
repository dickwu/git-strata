# `gitio` 模块

**Crate**：`crates/strata-gitio` · **设计来源**：design.md §3（大小写行、主 checkout 常脏）、§4（Repo、Base generation）、§5（`gitio` 行）、§6.1 第 1–2 步、§6.5 第 2–5 步、§6.7、§6.8、§9 不变量 2、3、5、6，§12（diff3 crate、工作区 `.git` 的接法） · **矩阵**：tgrep-core（3f）目标为 `gitio` 的 43 行，layerfs-replaced（3d）目标为 `gitio` 的 10 行，以及理由点名 gitio 的行（§7 表后）

gitio 是 strata 里唯一执行 git 的模块。它从 tgrep 借了两段代码，照抄并注明出处，不依赖 tgrep-core：`tgrep-core/src/generations/git.rs` 的清洗过的子进程环境，`tgrep-core/src/git_index.rs` 对 index 与 config 的直接解析。本文按 module-rulings.md 的 "T5 gitio" 一节及其补充条目写成。文中标"实测"的 git 行为，都是 2026-10-22 在本机 git 2.54.0（Apple Git-157）上用临时仓库验证过的；"22 个仓库"指 config.md 附录 A 的仓库，当天只用只读命令核对过。

## 1. 职责与边界

**负责**（design §5 的 `gitio` 行）：

1. **git 子进程的唯一出口**。strata 对 git 的每一次读写都经 `Git::command` 起子进程，环境先清洗（§5.1）。其他模块不执行 git，也不读写 `.git` 下的任何文件。gitio 自己直接读的文件只有 index、gitfile 与 `commondir`、仓库 config（§5.7，借 tgrep），其余一律经 git。
2. **仓库身份**：`open` 由工作树根得到 git common dir 的 realpath（types.md `RepoId`），拒绝 SHA-256 仓库与过低的 git 版本。
3. **远端与 ref**：`fetch` 把远端分支取进 strata 自己的跟踪 ref；`read_ref`、`is_ancestor`、`merge_base`；`refs/strata/*` 下比较并交换的 `update_ref` 与 `delete_ref`；`push` 产生 types.md 的 `PushOutcome`。
4. **tree 的两个方向**：`tree_of` 解析并校验基线 tree；`export_tree` 把 tree 导出成目录，逐字节等于 `git checkout` 的结果（检出方向的过滤器）；`stage` 走反方向，按检入方向的过滤器把工作树里的文件写成 tree，供发布的 staging 用；`commit_tree` 做 commit。
5. **工作区的 git**：`attach_worktree` 给一个目录接上 `.git`（design §6.1 第 2 步、§6.8），HEAD detached，index 等于基线 tree。
6. **index 与 ignore**：`read_index` 直接解析 index 与 config；`is_ignored` 按 git 的规则判断，已跟踪的文件永不被 ignore（design §3）。
7. **三方合并**：`merge3` 做 hunk 级三方合并，判定与 `git merge-file` 相同；`is_binary` 是 git 的二进制判定。

**不负责**：

| 事项 | 归谁 |
| --- | --- |
| 基线取哪个 commit（`origin/<default_branch>` 还是发布链顶、`pin_unpushed`）；基线目录放在哪、何时导出、复用与封存（design §7 的只读手段） | session |
| 合并基线 B 的取法（design §6.7）；发布链的名字 `refs/strata/publish/<branch>`；把 `PushOutcome` 转成 `PushRefused` 或 `Unknown`；推送前的人类确认；冲突的呈现与 `--assign`；增、删、改名层面的合并判定 | publish |
| `.lww` 的匹配 | overlay、publish |
| `BaseManifest` 的构建：对导出目录里每个文件取 BLAKE3，取导出后的字节而不是 git blob（3f 的 CRLF/smudge 两行） | overlay 或 session，按各自文档 |
| 同一个基线只有一个导出者：daemon 的 per-repo 写锁，第二个请求者等待，超时得 `Busy`，从不是 `Exists`（3d `interrupted_transfer…` 一行） | daemon |
| 磁盘上的大小写同一性（APFS 按 Unicode 折叠）：`apfs::case_fold_eq`。gitio 只按 git 的 ASCII 折叠判断 ignore 与"已跟踪" | apfs |
| overlay、checkpoint、监听与对账；依赖目录；种子文件与种子缓存；检查命令 | overlay、apfs、deps、session、publish |
| 工作区的 `pre-push` 守卫钩子装在哪（design §6.8）。钩子只从 common dir 或 `core.hooksPath` 解析；按工作区单独设置，要在主 checkout 的 config 里开 `extensions.worktreeConfig`，不变量 2 不允许 | session |

## 2. 依赖

| 依赖 | 理由 |
| --- | --- |
| `strata-types` | `RepoPath`、`TreeOid`、`CommitOid`、`RepoId`、`PushOutcome`、`StrataError`、`Code`。types.md §8 把 gitio 放在第 1 层，只能依赖第 0 层：不能调 `apfs::case_fold_eq`，也不依赖 store |
| `git` 可执行文件，版本 ≥ 2.31 | 唯一的 git 实现。不用 libgit2 或 gitoxide：git 本身就是语义的标准答案。下限来自 `rev-parse --path-format=absolute`（2.31）、`fetch --no-write-fetch-head` 与 `--no-auto-maintenance`（2.29）、`--end-of-options`（2.24）。从 daemon 的 `PATH` 解析（daemon.md 规定 PATH）；`HOME` 必须是用户的，全局配置里的身份、`core.excludesFile`、凭据助手才生效 |
| `memchr` | 抄来的 index 解析器用它找 NUL（tgrep 原样） |
| `libc` | `fcntl(F_FULLFSYNC)`、`renameatx_np(RENAME_EXCL)`（§5.5） |
| gitignore 模式匹配 crate | 逐个规则文件匹配，优先级由 gitio 自己叠（§5.8）；选哪个见 §9（待定） |
| 行级 diff crate | `merge3` 用；design §12 交给构建者，§9 待定 |
| `tracing` | `PushOutcome::Unknown` 不带文字，push 与 fetch 的原始输出记进 daemon 的日志 [我的决定] |
| 开发依赖 `tempfile` | 测试用的临时仓库 |

**照抄的代码**（不是依赖）。tgrep 是 MIT 许可，Copyright (c) Microsoft Corporation，取 commit d25ade6。按 module-rulings 补充条目"copy tgrep's `git_index.rs` with attribution rather than depending on tgrep-core"：

| 落点 | 来源 | 改动 |
| --- | --- | --- |
| `src/index.rs` | `tgrep-core/src/git_index.rs` | 只留 `metadata_path`、`read_git_dir_using`、`read_common_dir_using`、`config_bool`、`ignores_case`、`index_header`、`entry_capacity`、`walk_entries`、`read_varint`、`extension`、`SplitLink`、`ewah_len`、`read_ewah`、`merge_split`、`parse_index_with` 与它们的测试。删掉 managed 的 `*_controlled`、`fingerprint_*`、`MemoryCharge`。`TrackedFiles` 换成 `IndexInfo`，保留大小写，路径是 `RepoPath`。非 UTF-8 路径返回 `NotText`，原来的 `insert_path` 静默跳过。返回 `Result`，原来是 `Option`。不再把 `\` 归一成 `/`。遇到 sparse index 的目录条目（`sdir` 扩展）返回 `Unsupported` |
| `src/command.rs` | `tgrep-core/src/generations/git.rs` | 留 `command`、`discover_directory`、`valid_oid`、`object_format`、`resolve`、`parse_entries`。删掉 `PipedProcess`、`Control` 与 `LegacyBlobs`。tgrep 的 `commit_tree`（解析修订的 commit 与 tree）改名 `resolve_commit_and_tree`，免得与本文的 `commit_tree` 同名。`parse_entries` 遇到 gitlink 返回 `Unsupported`，原来记成成员 |

两个文件开头写明出处：仓库、commit、原路径、MIT。crate 根放 `THIRD-PARTY-NOTICES`，收 tgrep 的 LICENSE 原文。

**谁调用 gitio**：

| 调用方 | 用到 |
| --- | --- |
| daemon | `open`、`repo_id`。加载配置时检查每个 `path` 是工作树根、两张表不是同一个 `RepoId`（config.md §4.3） |
| session | `fetch`、`remote_ref`、`read_ref`、`is_ancestor`、`merge_base`、`tree_of`、`export_tree`、`attach_worktree`；钉基线若用 ref，再加 `update_ref`、`delete_ref` |
| overlay | `read_index`、`is_ignored`（增量点查与全量对账用同一个函数）、`merge3`（`rebase` 用） |
| publish | `fetch`、`read_ref`、`is_ancestor`、`merge_base`、`tree_of`、`export_tree`、`attach_worktree`、`stage`、`commit_tree`、`update_ref`、`push`、`merge3`、`is_binary` |

## 3. 公开接口（Rust）

```rust
// crates/strata-gitio/src/lib.rs —— 签名清单（不是实现）
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use strata_types::{CommitOid, PushOutcome, RepoId, RepoPath, StrataError, TreeOid};

pub type Result<T> = std::result::Result<T, StrataError>;

/// git 版本下限（§2）。
pub const MIN_GIT_VERSION: (u32, u32) = (2, 31);
/// 导出临时目录的名字前缀（§4.2）。status 用它列出失败导出留下的现场。
pub const EXPORT_TEMP_PREFIX: &str = ".export-";

/// 一个仓库，以 git common dir 识别（design §4 Repo）。只能由 `Git::open` 得到。
/// Clone 很便宜；里面的缓存（§4.3）只影响速度，不影响结果。
#[derive(Clone, Debug)]
pub struct Git {
    /// git common dir 的 realpath。
    common_dir: PathBuf,
    // 私有：主 checkout 工作树根、全局排除文件的路径、缓存（§4.3）
}

/// 一个工作树 index 的快照（§5.7）。由 `read_index` 产生，之后 index 再变它也不变。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexInfo {
    /// index 里的全部路径，保留 git 记录的大小写，NFC。
    pub tracked: BTreeSet<RepoPath>,
    /// 仓库的 core.ignorecase。
    pub ignorecase: bool,
    /// 私有：ignorecase 为真时 tracked 的 ASCII 小写形，供大小写不敏感的查询。
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
    /// 打开主 checkout（或已 attach 的工作区）的工作树根；`root` 不能是它的子目录（§5.2）。
    /// Code：NoSuchPath；InvalidInput（不是仓库、bare、子目录）；Unsupported（SHA-256、git < 2.31）；Unknown。
    pub fn open(root: &Path) -> Result<Git>;
    /// common dir 的 realpath。
    pub fn common_dir(&self) -> &Path;
    /// `RepoId::new(common_dir)`：主 checkout 与它的所有工作区得到同一个值。
    pub fn repo_id(&self) -> RepoId;
    /// 清洗过环境、绑定本仓库的 `git` 命令（§5.1）。下面的函数都经它起子进程。
    pub fn command(&self) -> Command;

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

    /// commit 的 tree，并校验它能当基线（§5.4）；不受 `git replace` 影响。
    /// Code：NoSuchCommit（commit、tree 或 blob 不在库里）；Unsupported（gitlink、未知 mode、路径碰撞）；
    /// NotText；InvalidInput（路径分量超过 255 字节等）；Unknown。
    pub fn tree_of(&self, commit: &CommitOid) -> Result<TreeOid>;
    /// 把 tree 导出到还不存在的 `dst`，结果逐字节等于 git checkout（§5.5）；dst 只以完整的形态出现。
    /// Code：tree_of 的那些；Exists（dst 已存在）；NoSuchPath（父目录不存在）；
    /// InvalidInput（dst 在主 checkout 里、路径超长）；写入开始之后的任何失败为 Unknown。
    pub fn export_tree(&self, tree: &TreeOid, dst: &Path) -> Result<()>;
    /// 给 `worktree` 接上 `.git`：`gitdir` 是 strata 自己的私有 git 目录，HEAD detached 在 `head`，
    /// index = tree_of(head)（§5.6）。对已接好的工作区再调用，就把 HEAD 移到新 head 并重建 index。
    /// Code：tree_of 的那些；NoSuchPath、NotDirectory（worktree）；Exists（worktree/.git 已存在且不指向 gitdir）；
    /// InvalidInput（worktree 或 gitdir 在主 checkout 里）；Unknown。
    pub fn attach_worktree(&self, worktree: &Path, gitdir: &Path, head: &CommitOid) -> Result<()>;
    /// 按检入方向的过滤器把 `paths` 的当前状态写进 worktree 自己的 index，再 write-tree（§5.9）：
    /// 是文件或符号链接的写入，不存在或是目录的删除，其余路径保持 index 原样。worktree 必须是 attach 过的。
    /// Code：InvalidInput（是主 checkout、没 attach）；NotFile（FIFO、socket、设备）；Unknown。
    pub fn stage(&self, worktree: &Path, paths: &[RepoPath]) -> Result<TreeOid>;

    /// 直接解析 worktree 的 index 与仓库 config（§5.7）。
    /// Code：NoSuchPath（没有 .git）；InvalidInput（worktree 属于别的仓库）；NotText；
    /// Unsupported（sparse index）；Unknown（index 或 gitfile 读不了、格式坏）。
    pub fn read_index(&self, worktree: &Path) -> Result<IndexInfo>;
    /// git 会不会忽略这个路径（§5.8）。已跟踪的永远是 false；目录按规则回答，目录本身从不算已跟踪。
    /// Code：read_index 的那些；Unknown（规则文件读不了）。
    pub fn is_ignored(&self, worktree: &Path, path: &RepoPath) -> Result<bool>;

    /// 以 `parent` 为唯一父 commit、`message` 原样为消息做 commit，不动任何 ref；作者与提交者取 git 配置。
    /// Code：NoSuchCommit（tree 或 parent 不在库里）；InvalidInput（git 身份没配，detail.rule = "git-identity"）；Unknown。
    pub fn commit_tree(&self, tree: &TreeOid, parent: &CommitOid, message: &str) -> Result<CommitOid>;
    /// 比较并交换：`name` 当前为 `old`（None 表示不存在）时才改成 `new`；只接受 `refs/strata/` 下的名字。
    /// Code：InvalidInput；HeadMoved（detail：ref、expected、actual）；Unknown。
    pub fn update_ref(&self, name: &str, new: &CommitOid, old: Option<&CommitOid>) -> Result<()>;
    /// 比较并交换地删除 `refs/strata/` 下的 ref。Code：同 update_ref。
    pub fn delete_ref(&self, name: &str, old: &CommitOid) -> Result<()>;
    /// 把 `src_ref`（必须在 refs/strata/ 下）推到 `<remote>` 的 `refs/heads/<dst_branch>`，不强推（§5.11）。
    /// 子进程起来之前的拒绝是 Err（InvalidInput；src_ref 不存在为 NoSuchCommit）；起来之后只返回 Ok：
    /// 只有本 ref 的状态行算数，超时或没有状态行一律是 PushOutcome::Unknown。
    pub fn push(&self, remote: &str, src_ref: &str, dst_branch: &str, timeout: Duration) -> Result<PushOutcome>;
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
```

- **与计划的签名相比**：名字全部照计划：`Git { common_dir }`、`command`、`fetch`、`tree_of`、`export_tree`、`read_index`、`IndexInfo { tracked, ignorecase }`、`is_ignored`、`commit_tree`、`update_ref`、`push`、`merge3`、`Merge3 { Clean | Conflict(Vec<Hunk>) }`。计划里没写 `Result` 的五个（`tree_of`、`export_tree`、`read_index`、`is_ignored`、`commit_tree`）都包上 `Result`，因为 rulings 要求它们返回 `Unsupported`、`NotText` 和显式的 index 错误；OID 参数改为借用。`PushOutcome` 用 types.md 的定义，本文不重新定义。
- **计划之外新增的公开项**都有调用方 [我的决定]：`open`、`common_dir`、`repo_id` 给 daemon 加载配置；`read_ref`、`is_ancestor`、`merge_base`、`remote_ref` 给 design §6.7 的合并基线 B 与 `pin_unpushed`，以及 types.md 的 `NoMergeBase`；`attach_worktree` 落实 design §6.1 第 2 步与 §6.8；`stage` 落实补充条目"clean 过滤器的反方向由 gitio 规定"；`delete_ref` 用于清理 `refs/strata/*`；`is_binary` 让 types.md 的 `Binary` 与 merge3 用同一个判定；另有两个常量。
- gitio 的类型（`Git`、`IndexInfo`、`Merge3`、`Hunk`）只在进程内传递，不出现在 RPC 或 JSON 上，所以不 derive `Serialize`。publish 把 `Hunk` 转成 `Conflict.detail` 时，自己决定 JSON 形状与每个 hunk 的长度上限（module-rulings T10）。它们属于 types.md §2.1 所说"模块拥有的跨模块类型"，名字待 T15 补进那张表。

## 4. 数据与不变量

### 4.1 写进主 checkout 的东西

只有对象与 `refs/strata/*` 两类，此外一个字节都不写（不变量 2）：

| 写入 | 由谁写 | 说明 |
| --- | --- | --- |
| `<common>/objects/` 下的对象 | `fetch`（收到的 pack）、`stage`（blob、tree）、`commit_tree`（commit） | 只增不改，按内容寻址。gitio 从不运行 `gc`、`prune`、`repack` 或 maintenance：`fetch` 带 `--no-auto-maintenance`、`--no-write-commit-graph` |
| `refs/strata/publish/<branch>` | publish，经 `update_ref` | 发布链（design §6.7） |
| `refs/strata/remotes/<remote>/<branch>` | `fetch`；`push` 返回 `Pushed` 之后 | strata 自己看到的远端 tip。不用 `refs/remotes/*`：后台移动人的远端跟踪分支，会让人之后的 `git push --force-with-lease` 以为看过这些提交，从而把它们盖掉 |
| `refs/strata/` 下的其他名字 | 调用方经 `update_ref`、`delete_ref` | gitio 只检查前缀，名字的含义归调用方（例如 session 钉基线） |

不写：工作树、`<common>/index`、`HEAD`、`refs/heads/*`、`refs/remotes/*`、`refs/tags/*`、`refs/stash`、`config`、`hooks/`、`info/`、`FETCH_HEAD`、`.git/worktrees/`。reflog 只在用户把 `core.logAllRefUpdates` 设成 `always` 时才会给 `refs/strata/*` 记，那是用户自己的配置，22 个仓库都没设。

工作区与主 checkout 共用 ref 命名空间。会话里 agent 自己跑的 `git stash`、`git branch`、`git tag` 会写主 checkout 的 `refs/stash`、`refs/heads/*`、`refs/tags/*`：git 规定 `refs/` 下除 `refs/bisect`、`refs/worktree`、`refs/rewritten` 以外都共享，实测工作区里的 `git stash` 出现在主 checkout 的 `git stash list` 里。这些不是 gitio 写的，但与 design §6.8"允许本地 git add/commit/stash"放在一起，需要 session 与 design 表态。

### 4.2 gitio 在主 checkout 之外写的东西

- **私有 git 目录**（`attach_worktree`）：位置由调用方给，不能在主 checkout 里。内容是 `commondir`（common dir 的绝对路径）、`gitdir`（`<worktree>/.git` 的绝对路径）、`HEAD`（detached，40 位 OID）、`index`、`logs/HEAD`（git 自己写）。工作树根下的 `.git` 是一行 `gitdir: <私有目录>`。
- **导出临时目录**（`export_tree`）：`<dst 的父目录>/.export-<dst 的名字>-<nonce>/`，里面是 `tree/`（导出的文件）与 `index`（私有 index）。成功时 `tree/` 改名为 `dst`，临时目录随即删掉；失败时原样保留，status 按前缀 `EXPORT_TEMP_PREFIX` 列出。
- 调用方给的 staging 工作树里，`stage` 只写它自己私有 git 目录下的 index。

### 4.3 进程内缓存

都按不可变的键或文件的 stat 键控，丢掉只会变慢：

| 缓存 | 键 | 内容 |
| --- | --- | --- |
| tree 校验结论 | `TreeOid` | `tree_of` 的 Ok 或 Err；tree 不可变，结论永久有效 |
| index 解析 | index 文件路径加 (size, mtime 纳秒, inode) | `IndexInfo` |
| 规则文件 | 文件路径加 (size, mtime 纳秒, inode) | 解析后的模式；每次 `is_ignored` 对涉及的规则文件各 stat 一次 |
| 全局排除文件的路径 | `Git` 实例 | `open` 时解析一次：`git config --path --get core.excludesFile`，没设时取 `$XDG_CONFIG_HOME/git/ignore`，再缺省 `~/.config/git/ignore`。daemon 重载配置时重新 `open` |
| git 版本 | 进程 | 第一次 `open` 时读一次 |

### 4.4 不变量

| # | 不变量 | 落实 design §9 |
| --- | --- | --- |
| G1 | 主 checkout 只读，写入只有 §4.1 的两类。每个读写 index 的命令都显式设 `GIT_INDEX_FILE`，指向私有 index 或工作区自己的 index，git 从不落到 `<common>/index`。`export_tree` 的 `dst`、`attach_worktree` 的 `worktree` 与 `gitdir`、`stage` 的 `worktree` 落在主 checkout 的工作树或 common dir 里时，返回 `InvalidInput`。`fetch` 不写 `FETCH_HEAD`、`refs/remotes/*`、tag；`push` 推到地址，不更新 `refs/remotes/*` | 2 |
| G2 | `update_ref`、`delete_ref`、`read_ref` 只接受 `refs/strata/` 开头的名字。`push` 的源必须在 `refs/strata/` 下，目标恰好一个 `refs/heads/<dst_branch>`，从不强推 | 2、3 |
| G3 | 导出不改已有的目录：`dst` 存在就拒绝；最后一步用 `renameatx_np(RENAME_EXCL)`，竞争下也不覆盖。`dst` 只以完整的形态出现：先持久化，再改名 | 5 |
| G4 | 结果不确定就是 `Unknown`。`push` 没拿到本 ref 的状态行即 `Unknown`；导出开始写之后的失败即 `Unknown`，临时目录保留。gitio 从不重试写操作，也从不清理失败的现场 | 6 |
| G5 | 已跟踪的文件永不被 ignore：`is_ignored` 先查 index，ignorecase 时按 ASCII 不区分大小写 | —（design §3 大小写行） |
| G6 | 跨出 gitio 的路径一律是 `RepoPath`；git 给出的非 UTF-8 路径返回 `NotText`，从不跳过 | — |
| G7 | 解析的对象就是确切的那一个：`--no-replace-objects`、`GIT_NO_LAZY_FETCH=1`；拒绝 SHA-256 仓库与 gitlink | 5 |
| G8 | 参数不会被当成选项：修订、remote、分支先校验，再放在 `--end-of-options` 之后；路径经 `-z --stdin` 传，或放在 `--` 之后并带 `--literal-pathspecs` | 3 |
| G9 | 先拒绝、后写入：`export_tree`、`attach_worktree`、`stage` 的类型化拒绝全部发生在创建任何文件之前，之后的失败按 G4 处理（module-rulings 的错误时序规则） | 6 |

## 5. 流程与算法

gitio 自己不拿 strata 的锁，靠 git 的锁文件（ref 锁、`index.lock`）保证单个命令的原子性。会写东西的操作（`fetch`、`export_tree`、`attach_worktree`、`stage`、`commit_tree`、`update_ref`、`delete_ref`、`push`）由调用方在 daemon 的 per-repo 写锁内调用（design §8）；只读的（`read_index`、`is_ignored`、`tree_of`、`read_ref`、`is_ancestor`、`merge_base`、`merge3`）不需要锁。gitio 不做 checkpoint：发布前（`BeforePublish`）与移动前（`BeforeRollout`）的 checkpoint 是调用方的事。

### 5.1 子进程环境（`command`）

1. `Command::new("git")`；把 `std::env::vars_os()` 里大写后以 `GIT_` 开头的键全部 `env_remove`，与 tgrep 相同。这包括 `GIT_DIR`、`GIT_WORK_TREE`、`GIT_INDEX_FILE`、`GIT_OBJECT_DIRECTORY`、`GIT_CONFIG_*`、`GIT_AUTHOR_*`、`GIT_COMMITTER_*`、`GIT_SSH_COMMAND`、`GIT_ASKPASS`。另外去掉继承来的 `STRATA_PUBLISH`。
2. 加参数 `--no-replace-objects`；设 `GIT_TERMINAL_PROMPT=0`、`GIT_NO_LAZY_FETCH=1`、`GIT_OPTIONAL_LOCKS=0`。另设 `LC_ALL=C` [我的决定]，因为 fetch 的错误分类要读 git 的英文原话。stdin 接 `/dev/null`：凭据助手给不出凭据时立即失败，不会挂住。
3. 仓库级命令用 `current_dir(common_dir)` 加 `--git-dir=.`（tgrep 的做法）。工作树级命令用 `--git-dir=<worktree>/.git --work-tree=<worktree>`，不向上发现。读写 index 的命令再设 `GIT_INDEX_FILE`；它在第 1 步清洗之后才设，所以只有 gitio 给的值生效。
4. 子进程放进自己的进程组（`process_group(0)`）。超时就对整个组发 SIGKILL，`ssh` 这类孙进程一起结束，再等它退出。`fetch` 的超时是常量（§9），`push` 的由调用方给，本地命令不设超时。
5. 输出只解析 `-z` 与 porcelain 格式。OID 经 `TreeOid`、`CommitOid` 的 `TryFrom` 校验（40 位）。stderr 只用来分类和填 `detail`，取末尾 4 KiB。

### 5.2 `open(root)`

1. git 版本，每个进程查一次：`git version` 低于 2.31 → `Unsupported`。
2. `root` 不存在 → `NoSuchPath`。
3. `git -C <root> rev-parse --path-format=absolute --show-toplevel --git-common-dir`：失败（不是仓库）或没有工作树（bare）→ `InvalidInput`。
4. `realpath(toplevel) ≠ realpath(root)` → `InvalidInput`。配置的 `path` 必须是工作树根，否则会认成上层的仓库。
5. `common = realpath(common dir)`。common dir 名为 `.git` 时，它的父目录记为主 checkout 工作树根，供 G1 判断。
6. 对象格式：`git rev-parse --show-object-format` 读的就是 `extensions.objectFormat`；不是 `sha1` → `Unsupported`，因为 types.md §3 的 `TreeOid` 只有 40 位。实测 `--object-format=sha256` 建的仓库里该键为 `sha256`，普通仓库没有这个键。
7. 解析全局排除文件的路径（§4.3），返回 `Git`。

共三次子进程，daemon 加载配置时每个仓库一次。22 个仓库都是 SHA-1，仓库级 `core.ignorecase` 都是 `true`。

### 5.3 `fetch(remote, branch)`

1. 校验：`remote` 只含 `[A-Za-z0-9._-]`，且不以 `-` 或 `.` 开头；`branch` 不以 `-` 开头，且 `git check-ref-format refs/heads/<branch>` 通过。这与 config.md 规则 V4 是同一套规则，加载配置时已经查过一次。不合法 → `InvalidInput`。
2. `git fetch --no-tags --no-write-fetch-head --no-auto-maintenance --no-write-commit-graph --no-prune --no-recurse-submodules --refmap= --end-of-options <remote> +refs/heads/<branch>:refs/strata/remotes/<remote>/<branch>`。`--refmap=` 让 git 不按 `remote.<remote>.fetch` 顺手更新 `refs/remotes/<remote>/<branch>`。实测只有 strata 的 ref 动了，没有 `FETCH_HEAD`，也没有 tag。超时 120 s（§9），到时杀进程组，返回 `Unknown`。
3. 退出 0：`git rev-parse --verify --end-of-options refs/strata/remotes/<remote>/<branch>^{commit}`，返回这个 commit。
4. 退出非 0：stderr 含 `couldn't find remote ref` → `NoSuchBranch`，包括远端还没有任何提交的情形，不会当成空 tree（实测原话）；含 `does not appear to be a git repository` 或 `No such remote` → `InvalidInput`；其他（网络、认证）→ `Unknown`，`detail` 带 stderr 末尾。失败时 strata 的 ref 不变，git 在最后一步才改 ref。
5. 远端分支缺失而发布链存在时算不算错，由 publish 判断（config.md §4.3：`origin/<publish.branch>` 存在，或者已有发布链）。

一次网络往返。session::new 对配置里的每个仓库都要 fetch，22 个仓库都在；design §8 的"新建会话 < 5 s"要求 session 并行发起，不同仓库的 fetch 互不相干。

### 5.4 `tree_of(commit)`

1. `git rev-parse --verify --end-of-options <commit>^{tree}` 得到 tree；commit 不在库里 → `NoSuchCommit`。`--no-replace-objects` 让 `git replace` 不起作用（3f replace refs 一行）。
2. 缓存里有这个 tree 的校验结论 → 直接返回。
3. 校验，每个 tree 只做一次：`git ls-tree -r -z -l --full-tree <tree>`。`-l` 要读每个 blob 的大小，所以缺失的 blob 在这里就报错 → `NoSuchCommit`；部分克隆缺的对象不会去远端补（`GIT_NO_LAZY_FETCH=1`）。逐条解析，照抄 tgrep 的 `parse_entries`：
   1. 模式只能是 `100644`、`100755`、`120000` 的 blob。`160000 commit`（gitlink，即子模块）→ `Unsupported`，`detail.path` 给出路径；其他模式 → `Unsupported`。
   2. 路径字节不是 UTF-8 → `NotText`，`detail.path` 用有损形式；`RepoPath::try_from` 不接受（如 NFC 规整后有空段）→ `InvalidInput`。
   3. 任一路径分量超过 255 字节（APFS 的上限，types.md §6.3 的 `ENAMETOOLONG` 行）→ `InvalidInput`。
   4. 碰撞：两条路径 NFC 规整后相同，或在 ignorecase 仓库里 ASCII 小写后相同 → `Unsupported`，`detail` 列出两条 [我的决定]。这样的 tree 在大小写不敏感的 APFS 上检出会少一个文件，导出必然对不上 tree。只按 Unicode 折叠才相同的（如 `É.txt` 与 `é.txt`）gitio 不查，那是 APFS 的规则，gitio 不能依赖 apfs。22 个仓库没有 ASCII 碰撞。
   5. 输出截断、缺结尾 NUL、字段数或 OID 长度不对 → `Unknown`，不凭空造条目。
4. 记下结论，返回 tree。

复杂度 O(tree 的文件数)，一次子进程。tool 有 4196 个文件，staff-api 有 4947 个（2026-10-22 各自的 origin 默认分支）。`WorkspaceState.head` 的不变量 `tree_of(head) == base.tree` 走缓存。

### 5.5 `export_tree(tree, dst)`

1. 拒绝，此时还什么都没写（G9）：tree 校验同 §5.4；`dst` 已存在（lstat）→ `Exists`，不碰它；`dst` 的父目录不存在 → `NoSuchPath`；`dst` 在主 checkout 的工作树或 common dir 里 → `InvalidInput`；`dst` 加上最长的路径超过 1024 字节（macOS 的 `PATH_MAX`）→ `InvalidInput`。
2. 建临时目录 `T = <父目录>/.export-<dst 的名字>-<nonce>/`。它与 `dst` 同一个父目录，所以在同一个卷上，最后的改名是原子的。从这一步起任何失败都是 `Unknown`，`T` 原样保留（G4）。
3. `GIT_INDEX_FILE=T/index git --git-dir=<common> read-tree <tree>`。
4. `GIT_INDEX_FILE=T/index git --git-dir=<common> --work-tree=T/tree checkout-index --all --force`。这一步按仓库配置做检出方向的全部转换：`core.autocrlf`、`core.eol`，`.gitattributes` 的 `text` 与 `eol`、`working-tree-encoding`、`ident`、`filter=<driver>` 的 smudge（含 `filter.<driver>.process` 长驻过滤器）。属性取自这个 tree 自己的 `.gitattributes`、`<common>/info/attributes` 与全局属性文件，与在主 checkout 里 `git checkout` 这个 tree 完全相同。实测：带 `eol=crlf`、一对 rot13 smudge/clean 过滤器、符号链接与可执行文件的 tree，导出与 `git clone` 后的检出逐字节相同。
   - smudge 跑的是仓库或全局配置里的命令，以 daemon 的用户身份在 `T/tree` 里执行，和用户自己检出时是同一信任级别。联网的过滤器（如 git-lfs）会让导出也联网。
   - `filter.<driver>.required = true` 的过滤器失败时，checkout-index 非零退出 → `Unknown`。没设 required 的过滤器失败时，git 照写原始 blob，与 `git checkout` 相同。
   - 符号链接写成符号链接（`core.symlinks`），`100755` 写上可执行位（`core.filemode`）。每个路径是独立的文件，git 不记录硬链接；git tree 里也没有空目录。
   - 不用 `git archive`：它会应用 `export-ignore` 与 `export-subst`，那不是检出语义。
   - 22 个仓库都没有 `.gitattributes`、没配过滤器、没设 `core.autocrlf`，所以今天的导出等于原样写出 blob。上面的规则为将来的仓库与测试而写。
5. 持久化：`T/tree` 下每个文件与目录 `fsync`，再做一次 `fcntl(F_FULLFSYNC)`。macOS 的 `fsync` 不刷磁盘缓存，一次 `F_FULLFSYNC` 把之前写出的全部刷下去。
6. **原子替换点**：`renameatx_np(T/tree, dst, RENAME_EXCL)`，再对父目录做 `F_FULLFSYNC`。改名之前崩溃，`dst` 不存在、`T` 保留；改名之后，`dst` 是完整的。`RENAME_EXCL` 撞上别人刚建好的 `dst` 时返回 `EEXIST`，这时按第 2 步的规则报 `Unknown` 并保留 `T`；在 daemon 的写锁之下不会发生。
7. 删掉 `T`，此时里面只剩私有 index。

复杂度 O(文件数 + 字节数)。design §8 没有导出的验收数字：它的"新建会话 < 5 s"假定基线已在缓存里，只有 origin tip 移动后的第一次 `new` 才导出。首个 spike 实测 tool 与 staff-api 的导出时间，写回 §9。

### 5.6 `attach_worktree(worktree, gitdir, head)`

1. 拒绝（G9）：`tree = tree_of(head)`；`worktree` 不存在 → `NoSuchPath`，不是目录 → `NotDirectory`；`worktree` 或 `gitdir` 在主 checkout 的工作树或 common dir 里 → `InvalidInput`；`worktree/.git` 已存在、而且不是指向 `gitdir` 的 gitfile → `Exists`。
2. `gitdir` 不存在时：建目录，写 `commondir`、`gitdir` 两个文件，再写 `HEAD`（`<head>` 加换行，即 detached）。`gitdir` 已存在时就是"再次接上"，也就是移动：`git --git-dir=<gitdir> update-ref --no-deref -m "strata: move" HEAD <head>`。`logs/HEAD` 会记下旧值，agent 在会话里做的本地 commit 仍能从 reflog 找到。
3. index：`GIT_INDEX_FILE=<gitdir>/index git --git-dir=<gitdir> --work-tree=<worktree> read-tree <tree>`。不做 `--refresh`：stat 信息留空，工作区里第一次 `git status` 会把文件重算一遍，之后正常。再次接上时 index 换成新 tree，相当于 `git reset --mixed <head>`：agent 暂存过的条目从 index 消失，文件内容不动。overlay 随之把这类新文件记成 `Untracked` 而不是 `Added`，两者的发布与检索一样（types.md `EntryKind`）。
4. **原子替换点**：`.git` 先写成同目录的临时文件，再 `rename` 成 `worktree/.git`。
5. 结果：HEAD detached、没有 upstream（design §6.8）；`git status`、`git log` 正常；会话里 `git commit` 的对象进主 checkout 的 object store，不复制对象。实测：`git status --porcelain` 为空，`git log -1` 是 head，主 checkout 的 `git worktree list` 不列出它，主 checkout 的 index 与状态不变。

不用 `git worktree add`，因为它要写主 checkout 的 `.git/worktrees/<id>/`；代价见 §9 第 1 条。复杂度 O(文件数)，花在 read-tree 上。

### 5.7 `read_index(worktree)`

1. 找 git 目录：`worktree/.git` 是目录就用它；是 gitfile 就读 `gitdir: <路径>`，照抄 tgrep 的 `read_git_dir_using`，只去掉 git 写入的行尾，路径两侧的空格保留；不存在 → `NoSuchPath`。common dir 取 `<gitdir>/commondir`，相对路径相对 git 目录解析；没有这个文件时就是 git 目录本身。common dir 的 realpath 不等于本仓库的 → `InvalidInput`。
2. index 文件的 (size, mtime 纳秒, inode) 与缓存相同 → 返回缓存的快照。
3. 读 `<gitdir>/index` 并解析，照抄 tgrep 的 `parse_index_with`：版本 2 至 4；v4 的前缀压缩路径；扩展区与结尾校验和；split index 经同目录的 `sharedindex.<hash>` 合并，按 EWAH 位图删除与替换；头部声明的条目数按文件长度夹取，伪造的头部不能让进程按 40 亿条预留内存。读不完整一律返回 `Unknown`（`detail.reason`），不返回少了条目的集合，因为少一条就少一个"已跟踪"豁免。git 目录有效而 index 文件不存在 → 空集合，这是 git 的语义：没有 index 就是什么都没暂存。带 sparse 目录条目（`sdir` 扩展）的 index → `Unsupported`；gitio 自己建的工作区 index 总是完整的。
4. 每条路径：不是 UTF-8 → `NotText`；经 `RepoPath::try_from`（NFC）后进 `tracked`；ignorecase 时它的 ASCII 小写形进 `folded`。
5. `ignorecase`：读 `<common>/config` 的 `core.ignorecase`；`extensions.worktreeConfig` 为真时，`<gitdir>/config.worktree` 覆盖它，照抄 tgrep 的 `config_bool` 与 `ignores_case`。只读仓库级 config，不读全局配置与 `include`：git 在大小写不敏感的卷上 clone 或 init 时，总把 `core.ignorecase = true` 写进仓库 config，22 个仓库都是这样。
6. 返回快照：之后 index 再变，它也不变。

复杂度 O(index 字节数)；tool 约 4K 条，毫秒级。实测主 checkout 的 index 都是 v2，没有 split index。

### 5.8 `is_ignored(worktree, path)`

1. `info = read_index(worktree)`，走缓存；`info.is_tracked(path)` 为真 → `false`（G5）。ignorecase 时这一查不区分 ASCII 大小写，与 `git status` 的判定相同。这里不照 `git check-ignore`：它查 index 时区分大小写，实测 index 里是 `tracked.LOG` 时它仍报 `tracked.log` 被忽略，而在大小写不敏感的磁盘上那是同一个文件。区分大小写的仓库里，被规则逐字命中的已跟踪文件同样不被忽略。这一点与 tgrep 的遍历器不同，design §3 已经定了用"已跟踪永不 ignore"。
2. `lstat(worktree/path)` 判断是不是目录。不存在的按文件算；符号链接也按文件算，不跟随。
3. 父目录：从 `path` 的第一级目录到它的父目录，逐级以目录身份判定，任何一级被排除 → `true`。git 的规则是父目录被排除的文件不能再包含回来；实测 `build/` 被排除时 `!build/keep.c` 不生效。
4. 路径本身，按优先级从高到低：`path` 所在目录的 `.gitignore`，逐级向上直到 worktree 根的 `.gitignore`，`<common>/info/exclude`，全局排除文件。第一份有命中的文件决定结果；同一份文件里，最后一条命中的模式决定结果；`!` 模式表示不忽略。
   - 只读 worktree 根及以下的 `.gitignore`。会话根 `<root>/sessions/<session-id>/` 以及更上层的 `.gitignore` 不起作用。
   - 被规则自己命中的 `.gitignore` 照样贡献规则；一行坏模式（如 `[`）只跳过这一行，与 git 相同。
   - 全局排除文件（module-rulings T5 补充条目）：config.md §4.3 列出的九个仓库，`.claude/settings.local.json` 只靠 `~/.config/git/ignore` 被忽略。
5. ignorecase 为真时，模式按 ASCII 不区分大小写匹配，否则逐字节匹配。git 的大小写折叠只管 ASCII，实测模式 `É.txt` 不命中 `é.txt`。
6. 目录路径按规则回答，目录本身从不算已跟踪。遍历的调用方遇到被忽略的目录、而 `info.tracked_under(dir)` 为真时，仍要进去，因为里面的已跟踪文件不能丢。
7. 增量点查与全量对账都调这一个函数，两者的判定因此一致（3f 的点查匹配器一行，目标 overlay）。

复杂度：每次调用 O(路径深度) 次 stat 加一次匹配，规则文件只在 stat 变化时重读。tool 源码树约 4K 个文件，在 design §8"全量对账 < 3 s"里占几十毫秒，在"checkpoint 改 50 个文件 < 200 ms"里占几毫秒。

### 5.9 `stage(worktree, paths)`

这是 `export_tree` 的反方向（module-rulings T5/T10 补充条目）。publish 的用法：`export_tree(B 的 tree)` 建 staging 树 → `attach_worktree(staging, …, B)` → 写入合并后的文件 → `stage(staging, 批次路径)` → `commit_tree` → `update_ref` → `push`。

1. 拒绝（G9）：`worktree` 是主 checkout，或没有指向本仓库的私有 git 目录 → `InvalidInput`。`paths` 去重、排序，再逐条 `lstat`：普通文件或符号链接记为"写"；不存在或是目录记为"删"；FIFO、socket、设备 → `NotFile`。"删"只删 index 里这个路径的文件条目；目录里的新文件是批次里的其他路径，类型转换总是成对出现（module-rulings T10）。
2. 删：`git … update-index --force-remove -z --stdin`，`GIT_INDEX_FILE=<gitdir>/index`。
3. 写：`git … update-index --add --replace -z --stdin`。update-index 按检入方向转换，即 clean 过滤器、eol 与 autocrlf 规范化、`working-tree-encoding` 还原，然后写 blob；`--replace` 让文件与目录冲突的旧条目自动让位。实测 rot13 的 clean 过滤器与 eol 规范化都生效，blob 是检入形态。
   - 不用 `git add`：它对目录路径会递归；被忽略的文件要加 `-f` 才收，而 `-f` 碰上目录路径，会把目录里没选的被忽略文件一起收进来。那就是"发布了用户没有选的东西"（design §2 反目标）。update-index 只认逐条给出的文件路径，也不看 `.gitignore`。
4. `git … write-tree` 得到 tree。
5. 结果 = 起点 tree（attach 时的 index）加上批次路径的变化。不在 `paths` 里的路径保持 index 原样，即使磁盘上改过。

**往返稳定**：`export_tree` 导出、内容没改的文件，`stage` 之后得到原来的 blob。实测：带 `eol=crlf`、一对 rot13 smudge/clean 过滤器、符号链接与可执行文件的 tree，全部原样 stage 后 tree OID 不变。过滤器本身不对称时（例如 smudge 加时间戳）不保证，git 自己也不保证。复杂度 O(批次字节数)。

### 5.10 commit、ref 与祖先

- `commit_tree`：`git commit-tree <tree> -p <parent> -F -`，消息从 stdin 原样传入，不清理注释。作者与提交者取 git 配置的 `user.name`、`user.email`，因为清洗环境去掉了 `GIT_AUTHOR_*`、`GIT_COMMITTER_*`。`commit-tree` 不读 `commit.gpgSign`（git 2.54 的文档只认命令行的 `-S`），所以不会在 daemon 里弹 pinentry。输出的 OID 经校验。只写一个 commit 对象，不动 ref。身份没配 → `InvalidInput`，`detail.rule = "git-identity"`。
- `update_ref`：名字必须以 `refs/strata/` 开头，格式再由 git 自己校验。命令是 `git update-ref -m strata <name> <new> <old 或 40 个 0>`，git 在 ref 锁里做比较并交换，是原子的。失败原话是 `is at <实际> but expected <期望>` 或 `reference already exists`（两种都实测过）→ `HeadMoved`；`detail.actual` 在失败后再 `read_ref` 读一次，不从 stderr 里抠。其他失败 → `Unknown`。
- `delete_ref`：`git update-ref -d <name> <old>`，失败的分类同上。
- `read_ref`：`git rev-parse --verify --quiet --end-of-options <name>^{commit}`，退出 1 → `None`。经 git 读，不直接读 ref 文件、`packed-refs` 或 reftable，所以与 ref 存储后端无关。
- `is_ancestor`：`git merge-base --is-ancestor <a> <b>`，退出 0 → `true`，退出 1 → `false`，其他 → `NoSuchCommit` 或 `Unknown`。
- `merge_base`：`git merge-base <a> <b>`，输出 OID → `Some`；退出 1 且没有输出 → `None`。有多个最近公共祖先时取 git 给出的那一个：祖先与合并基由 git 承担，strata 没有自己的提交图（3d 的祖先对拍一行）。浅克隆（`.git/shallow`）截断了历史时，git 的答案可能是 `None`，照实返回。

### 5.11 `push(remote, src_ref, dst_branch, timeout)`

1. 拒绝，返回 `Err`，此时什么都没发出：`src_ref` 不以 `refs/strata/` 开头，或 `remote`、`dst_branch` 不合法（规则同 §5.3）→ `InvalidInput`；`read_ref(src_ref)` 为 `None` → `NoSuchCommit`。
2. 推送地址：`git remote get-url --push <remote>`，已应用 `pushurl` 与 `pushInsteadOf`。推到地址，不推 remote 名 [我的决定]：推到 remote 名成功后，git 会更新 `refs/remotes/<remote>/<dst_branch>`，推到地址则不会（实测）。22 个仓库的推送地址都是 HTTPS，没设 `pushurl` 或 `insteadOf`。
3. `git push --porcelain --no-follow-tags --end-of-options <地址> <src_ref>:refs/heads/<dst_branch>`，子进程另设 `STRATA_PUBLISH=1`，design §6.8 的守卫钩子认它。不带 `--force`、`--force-with-lease`、`+`，也不带 `--no-verify`：仓库自己的 pre-push 钩子照跑，22 个仓库目前都没有非示例钩子。超时就是 `timeout`，到时杀进程组。
4. 归类，只认本 ref 的 porcelain 状态行（`<flag>\t<src>:<dst>\t<摘要>`）：

   | 观察 | 结果 |
   | --- | --- |
   | flag 为空格（快进）、`*`（新分支）、`=`（已是最新） | `Pushed` |
   | flag 为 `!`：`[rejected] (non-fast-forward)`、`[remote rejected] (pre-receive hook declined)`、权限不足等 | `Refused(摘要 + stderr 里的 remote: 行)` |
   | 超时，进程组被杀 | `Unknown` |
   | 没有本 ref 的状态行（断线、远端挂断、认证失败、本地 pre-push 钩子拒绝等），不论退出码 | `Unknown` |
   | flag 为 `+` 或 `-`（gitio 不发这两种推送） | `Unknown` |

   实测依据：非快进得到 `!	…	[rejected] (non-fast-forward)`，退出 1；远端 pre-receive 拒绝得到 `!	…	[remote rejected] (pre-receive hook declined)`，退出 1；远端在 `reference-transaction` 钩子的 `committed` 阶段被杀，得到的是没有状态行、退出 128、stderr 为 `the remote end hung up unexpectedly`，**而远端分支已经指向推送的 commit**。这就是只能按状态行、不能按 stderr 判定"被拒"的原因。本地 pre-push 钩子拒绝时同样没有状态行（退出 1），也保守地报 `Unknown`。
5. `Pushed` 之后，把 `refs/strata/remotes/<remote>/<dst_branch>` 强制更新为推送的 commit。这与 git 推送后更新远端跟踪分支同义，只是留在 `refs/strata/` 里；这一步失败不改变结果，下次 fetch 会对齐。
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
| 任意 | `git` 不在 PATH，或版本低于 2.31 | `Unsupported` | 无 | 装 git，或修 daemon 的 PATH（daemon.md） |
| `open` | 路径不存在 | `NoSuchPath` | 无 | 改配置 |
| `open` | 不是仓库、bare、子目录 | `InvalidInput` | 无 | 改配置；daemon 拒绝整份配置（config.md §4.3） |
| `open` | SHA-256 仓库 | `Unsupported` | 无 | v1 不支持 |
| `fetch` | 名字不合法，remote 没配置 | `InvalidInput` | 无 | 改配置 |
| `fetch` | 远端没有这个分支，含远端还没有提交 | `NoSuchBranch` | 无 | session、publish 透传；已有发布链时由 publish 判断 |
| `fetch` | 网络、认证、超时、其他 | `Unknown` | strata 的跟踪 ref 不变；对象目录里可能留下 git 自己的 `tmp_pack_*`，git 日后清理 | 读操作，调用方可以再试 |
| `tree_of` | commit、tree 或 blob 不在库里：被主 checkout 的 gc 清掉，或部分克隆缺对象 | `NoSuchCommit` | 无 | 重新 fetch，或换基线 |
| `tree_of` | gitlink、未知 mode、路径碰撞 | `Unsupported` | 无 | 这个 commit 不能当基线 |
| `tree_of` | 非 UTF-8 路径 | `NotText` | 无 | 同上 |
| `tree_of` | 路径分量超过 255 字节，`RepoPath` 不接受 | `InvalidInput` | 无 | 同上 |
| `tree_of` | ls-tree 输出格式不对 | `Unknown` | 无 | — |
| `export_tree` | `dst` 已存在 | `Exists` | `dst` 一字不动 | 调用方复用已有基线；用户看到的是等待或 `Busy`（daemon 的写锁），不是 `Exists` |
| `export_tree` | 父目录不存在；`dst` 在主 checkout 里；完整路径超过 1024 字节 | `NoSuchPath`、`InvalidInput` | 无 | — |
| `export_tree` | 写入开始之后：required 过滤器失败、`ENOSPC`、I/O 错误、进程被杀、`RENAME_EXCL` 撞上别人建好的 `dst`、daemon 崩溃 | `Unknown`（`detail`：`tree`、`temp`、`errno` 或 stderr） | `dst` 不存在；`.export-*` 临时目录原样保留，status 报出 | 人查看后删掉临时目录，再重新导出；gitio 不重试 |
| `attach_worktree` | `worktree` 不存在、不是目录 | `NoSuchPath`、`NotDirectory` | 无 | — |
| `attach_worktree` | `worktree/.git` 已存在且不指向 `gitdir` | `Exists` | 无 | 调用方处理 |
| `attach_worktree` | `worktree` 或 `gitdir` 在主 checkout 里 | `InvalidInput` | 无 | — |
| `attach_worktree` | 写入开始之后失败：index 锁被工作区里的 git 占着、I/O 错误 | `Unknown` | 私有 git 目录可能半成：HEAD 已移而 index 未重建，或 `.git` 还没写 | status 报出；再次 attach 是幂等的修复手段，是否调用由调用方决定 |
| `read_index`、`is_ignored` | 没有 `.git` | `NoSuchPath` | 无 | — |
| `read_index`、`is_ignored` | worktree 属于别的仓库 | `InvalidInput` | 无 | — |
| `read_index`、`is_ignored` | index、gitfile、`commondir` 读不了或格式坏：截断、版本不认识、split index 缺共享半、位图越界 | `Unknown`（`detail.reason`） | 无 | 不猜：调用方停下这次 checkpoint，status 报出 |
| `read_index` | 非 UTF-8 路径；sparse index | `NotText`；`Unsupported` | 无 | — |
| `is_ignored` | 规则文件读不了（`ENOENT` 以外的错误） | `Unknown`（`detail`：`errno`、路径） | 无 | — |
| `stage` | 是主 checkout，或没 attach | `InvalidInput` | 无 | 调用方的错 |
| `stage` | 路径是 FIFO、socket、设备 | `NotFile` | 无，还没写 | — |
| `stage` | update-index 或 write-tree 失败 | `Unknown` | staging 工作树的 index 可能已部分更新；它是 publish 的私有目录 | publish 按不变量 6 处理这棵 staging 树 |
| `commit_tree` | tree 或 parent 不在库里 | `NoSuchCommit` | 无 | — |
| `commit_tree` | git 身份没配 | `InvalidInput`（`detail.rule = "git-identity"`） | 至多留下一个没人引用的对象 | 配 `user.name`、`user.email` |
| `commit_tree` | 其他 | `Unknown` | 没有 ref 变化 | — |
| `update_ref`、`delete_ref`、`read_ref` | 名字不在 `refs/strata/` 下 | `InvalidInput` | 无 | 调用方的错 |
| `update_ref`、`delete_ref` | ref 不是期望值 | `HeadMoved`（`detail`：`ref`、`expected`、`actual`） | ref 不变 | 不重试、不重新合并（types.md） |
| `update_ref`、`delete_ref` | 其他：`.lock` 残留、I/O 错误 | `Unknown` | ref 不变，git 在锁里失败不会改 ref | status 报出；gitio 不删别人的 `.lock` |
| `push` | 校验失败 | `Err`：`InvalidInput`、`NoSuchCommit` | 什么都没发出 | — |
| `push` | 本 ref 的状态行是 `!` | `Ok(Refused(原话))` | 远端没变，发布链在本地 | publish 转成 `PushRefused` |
| `push` | 超时、断线、没有状态行 | `Ok(Unknown)` | 远端可能已经收到；本地 ref 不变 | publish 转成 `Unknown`：保留发布链、不重放、status 报出 |
| `merge3`、`is_binary` | 纯函数，不会失败 | — | — | — |

- **错误时序**（module-rulings 共同规则，types.md §6.3 末段）：类型化的拒绝只出现在写任何东西之前（G9）；写入一旦开始，失败就是 `Unknown`，现场保留，不回滚、不清理，status 报出（不变量 6）。对 gitio 而言，"写入开始"指建出导出临时目录、写私有 git 目录或 `.git`、运行 update-index 的那一刻。
- **重启**：gitio 没有自己的持久状态。daemon 重启后，`.export-*` 残留与结果为 `Unknown` 的推送由 status 报出，不重放。
- **对 types.md 码表的扩展**：gitio 给 `InvalidInput`、`Exists`、`NoSuchPath`、`NotFile`、`NotDirectory` 增加了触发条件；`Unsupported` 在 types.md 列出的 SHA-256 与 gitlink 之外，多了 git 版本过低、未知 mode、路径碰撞、sparse index。用的都是已有的码，不加新码；T15 可以把 gitio 补进这些码的"谁返回"一列。

## 7. 测试清单

| 测试名 | 断言 | 来源（矩阵行 或 新增） |
| --- | --- | --- |
| `command_strips_inherited_git_env` | 父进程带着指向另一个仓库的 `GIT_DIR`、`GIT_WORK_TREE`、`GIT_INDEX_FILE`、`GIT_OBJECT_DIRECTORY`、`GIT_CONFIG_PARAMETERS` 时，`command()` 起的 git 仍只作用于本仓库；子进程环境里没有任何继承来的 `GIT_*`，也没有继承来的 `STRATA_PUBLISH` | 新增 |
| `command_sets_safety_flags` | argv 带 `--no-replace-objects`；env 有 `GIT_TERMINAL_PROMPT=0`、`GIT_NO_LAZY_FETCH=1`、`GIT_OPTIONAL_LOCKS=0`、`LC_ALL=C`；stdin 是空的，要凭据的远端立即失败而不是挂住 | 新增 |
| `open_resolves_one_repo_id_for_checkout_and_workspace` | 主 checkout 与 attach 过的工作区 `open` 得到同一个 `RepoId`（common dir 的 realpath，`/tmp` 与 `/private/tmp` 归一）；用子目录 `open` → `InvalidInput` | tgrep-core/tests/generations.rs::linked_worktrees_and_equal_trees_share_one_arc |
| `open_refuses_non_repository_bare_and_missing` | 非仓库目录、bare 仓库 → `InvalidInput`；不存在的路径 → `NoSuchPath` | tgrep-core/tests/generations.rs::unsupported_storage_and_discovery_return_errors |
| `sha256_repository_is_unsupported` | `git init --object-format=sha256` 的仓库（`extensions.objectFormat = sha256`）→ `open` 返回 `Unsupported`；SHA-1 仓库正常打开 | tgrep-core/tests/generations.rs::sha256_repositories_and_unicode_paths_are_supported |
| `old_git_is_unsupported` | 版本解析：`git version 2.30.9` → `Unsupported`；`2.31.0`、`2.54.0 (Apple Git-157)` 通过 | 新增 |
| `metadata_paths_strip_only_git_line_endings` | 读 gitfile 与 `commondir` 时只去掉 git 写入的行尾：没有行尾与 CRLF 两种输入，路径两侧的空格都保留；只有行尾时报错 | tgrep-core/src/git_index.rs::metadata_paths_strip_only_git_line_endings |
| `reads_a_version_2_index` | v2 index 的全部路径进 `tracked`，未跟踪的路径不在其中 | tgrep-core/src/git_index.rs::reads_a_version_2_index |
| `tracked_lookup_folds_ascii_case_when_ignorecase` | ignorecase 时 `is_tracked` 按 ASCII 不区分大小写（`Assets/Logo.PNG` 命中 `assets/logo.png`），`tracked` 本身保留 git 记录的拼写；不归一反斜杠与前导 `/`，`RepoPath` 不接受它们 | tgrep-core/src/git_index.rs::matches_paths_without_regard_to_case_or_separator |
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
| `linked_worktree_uses_the_common_config_and_its_own_index` | config 取公共目录的（`extensions.worktreeConfig` 打开时 `config.worktree` 覆盖，否则不生效），index 取工作区自己 git 目录的 | tgrep-core/src/git_index.rs::linked_worktree_uses_the_common_config_and_its_own_index |
| `read_index_without_git_is_an_error` | 没有 `.git` 的目录 → `NoSuchPath`，不是空集合；git 目录有效但没有 index 文件 → 空的 `tracked` | tgrep-core/src/git_index.rs::a_directory_without_git_reports_nothing |
| `non_utf8_index_path_is_not_text` | 经 `git update-index --index-info` 写进字节为 `\xff.txt` 的条目，`read_index` 返回 `NotText`，不跳过 | tgrep-core/tests/generations.rs::non_unicode_repository_roots_are_lossless_but_tracked_paths_are_unsupported |
| `sparse_index_is_unsupported` | 带 `sdir` 扩展的 index → `Unsupported` | 新增 |
| `read_index_snapshot_is_immutable_and_cached` | 返回的 `IndexInfo` 在 index 之后改变时保持不变；index 文件的 (size, mtime, inode) 不变时第二次调用不重新解析 | 新增 |
| `read_index_refuses_worktree_of_another_repo` | worktree 的 common dir 与本仓库不同 → `InvalidInput` | 新增 |
| `index_parse_tracked_never_ignored` | ignorecase 仓库：规则 `QLogs` 隐藏未跟踪的 `qlogs/` 下文件，`*.txt` 隐藏未跟踪的 `Gone.TXT`，已跟踪的 `Kept.TXT` 不被隐藏，磁盘上换成 `kept.txt` 拼写也不被隐藏（`git status` 的语义）；区分大小写的仓库里，被规则逐字命中的已跟踪文件同样不被隐藏 | tgrep-core/src/walker.rs::ignorecase_hides_what_git_hides_and_keeps_what_git_tracks |
| `untracking_a_file_lets_rules_apply` | `git rm --cached TRACKED.txt` 之后同一路径 `is_ignored` 变为真；index 被写坏后 `is_ignored` 返回 `Unknown`，不当成"没有已跟踪文件" | tgrep-core/src/worktrees/tests.rs::case_insensitive_ignore_snapshot_tracks_index_membership_coherently |
| `case_sensitive_repo_matches_exactly` | `core.ignorecase = false`：逐字节匹配，`*.txt` 不隐藏 `Gone.TXT`；逐条点查与遍历时的逐项调用结果相同 | tgrep-core/src/walker.rs::without_ignorecase_the_walk_is_unchanged；tgrep-core/src/walker.rs::the_point_query_matcher_follows_the_case_sensitivity_gate |
| `ascii_only_case_folding_like_git` | ignorecase 下模式 `É.txt` 不命中 `é.txt` | 新增 |
| `root_gitignore_file_and_dir_patterns` | 根 `.gitignore` 的 `*.log`、`target/` 生效，`target/` 只匹配目录；`src/main.rs` 不受影响 | tgrep-core/src/gitignore.rs::builds_matcher_from_root_gitignore |
| `self_ignored_gitignore_still_applies` | `build/.gitignore` 内容为 `*`：它自己被命中，但规则照样生效（`build/artifact.o` 被忽略）；被隐藏的 `.gitignore` 照样贡献规则 | tgrep-core/src/gitignore.rs::collects_a_gitignore_that_its_own_rules_ignore；tgrep-core/src/walker.rs::walk_dir_can_collect_gitignore_files_without_indexing_them |
| `malformed_ignore_line_skips_only_itself` | 一行 `[` 不让同一文件里其他规则作废 | tgrep-core/src/gitignore.rs::malformed_ignore_line_does_not_discard_valid_rules |
| `no_rules_ignores_nothing` | 没有任何规则文件（全局排除文件也不存在）时，所有路径都是 false | tgrep-core/src/gitignore.rs::returns_none_when_no_rules |
| `info_exclude_applies_through_worktree_gitfile` | 工作区的 `.git` 是 gitfile，经私有 git 目录的 `commondir` 找到主 checkout 的 `info/exclude` 并生效；删掉 `info/exclude` 后不再生效 | tgrep-core/src/gitignore.rs::the_repository_exclude_is_found_through_a_worktree_pointer |
| `relative_commondir_resolves_against_gitdir` | `commondir` 的相对路径（`../..`）相对私有 git 目录解析，不相对工作树根 | tgrep-core/src/gitignore.rs::a_relative_commondir_resolves_against_the_worktree_git_dir |
| `global_excludes_file_is_honoured` | 没设 `core.excludesFile` 时读 `$XDG_CONFIG_HOME/git/ignore`（缺省 `~/.config/git/ignore`），设了就读它；`.claude/settings.local.json` 只靠全局排除文件被忽略时 `is_ignored` 为真 | 新增 |
| `local_negation_overrides_global_excludes` | 本地 `!keep.log` 压过全局排除文件的 `*.log`；优先级为目录内 `.gitignore` > `info/exclude` > 全局排除文件 | tgrep-core/src/gitignore.rs::local_whitelist_overrides_global_ignore |
| `deeper_rules_override_shallower` | `pkg/` 里的 `!keep.log` 压过根的 `*.log`；更深目录的 `!important.log` 压过浅层的 `*.log` | tgrep-core/src/gitignore.rs::with_nested_keeps_deeper_rules_ahead_of_local_rules；tgrep-core/src/gitignore.rs::deeper_gitignore_negation_overrides_a_shallower_ignore |
| `nested_gitignore_scoped_to_its_directory` | 嵌套 `.gitignore` 里的 `*`、`*.c` 只作用于所在目录，不提升成全树规则；根的 `*.log` 与 `src/.gitignore` 的 `*.tmp` 各在自己的作用域内 | tgrep-core/src/gitignore.rs::nested_gitignore_rules_stay_scoped_to_their_directory；tgrep-core/src/walker.rs::build_gitignore_matcher_from_discovered_files |
| `gitignore_above_worktree_root_is_not_applied` | 会话根 `<root>/sessions/<session-id>/` 里、即工作树根之上的 `.gitignore`，不作用于工作区里的文件 | tgrep-core/src/walker.rs::parent_ignore_crosses_repository_boundary_but_gitignore_does_not |
| `negation_cannot_reinclude_under_excluded_dir` | `build/` 被排除时 `!build/keep.c` 不生效 | 新增 |
| `unreadable_index_is_an_error_not_empty` | index 不可读（权限、截断）时 `is_ignored` 返回 `Unknown`，既不全放行也不全隐藏 | tgrep-core/src/walker.rs::an_unreadable_index_excludes_nothing |
| `is_ignored_agrees_with_git_on_corpus` | 固定语料（嵌套 `.gitignore`、否定、目录模式、`**`、锚定、`info/exclude`、全局排除文件、ignorecase 开关）逐路径对拍：规则部分对 `git check-ignore --no-index`，已跟踪豁免部分对 `git status --porcelain --ignored` | 新增 |
| `option_like_arguments_are_refused_not_executed` | remote 为 `--upload-pack=touch <f>`、分支为 `-x`、修订为 `--help` 时返回 `InvalidInput`，`<f>` 不出现；以 `-` 开头、含 `*` 或 `:` 的路径按字面处理 | tgrep-core/tests/generations.rs::interrupted_and_failed_publications_are_not_discoverable |
| `fetch_writes_only_the_strata_tracking_ref` | fetch 之后 `refs/strata/remotes/origin/main` 等于远端 tip，也是返回值；`refs/remotes/origin/main`、`FETCH_HEAD`、`refs/tags` 不变，远端有新 tag 也不带回 | 新增 |
| `fetch_missing_branch_is_no_such_branch` | 远端没有这个分支，包括远端还没有任何提交 → `NoSuchBranch`，不当成空 tree | tgrep-core/tests/generations.rs::unsupported_storage_and_discovery_return_errors |
| `fetch_timeout_is_unknown_and_moves_nothing` | 远端挂住（假 upload-pack 一直睡）超过超时 → `Unknown`，跟踪 ref 不变 | 新增 |
| `ancestry_answers_come_from_git` | 有分叉与合并的历史上，`is_ancestor`、`merge_base` 与 `git merge-base` 相同；没有共同祖先 → `None`；两个最近公共祖先时返回 git 给出的那个 | layerfs-api/tests/history_ancestry_oracle.rs::bounded_ancestry_equals_the_whole_history_walk |
| `read_ref_absent_is_none` | 不存在的 `refs/strata/` ref → `None` | 新增 |
| `update_ref_cas_refuses_moved_ref` | 期望值与实际值不同 → `HeadMoved`，`detail` 带 `expected` 与 `actual`，ref 不变；期望值相同 → 前进 | 新增 |
| `update_ref_create_requires_absent` | `old = None` 而 ref 已存在 → `HeadMoved`；不存在 → 创建 | 新增 |
| `refs_outside_strata_namespace_are_refused` | `read_ref`、`update_ref`、`delete_ref` 对 `refs/heads/main`、`refs/remotes/origin/main`、`HEAD` 返回 `InvalidInput`，什么都不写 | 新增 |
| `delete_ref_is_compare_and_swap` | 期望值不符 → `HeadMoved`，ref 仍在；相符 → 删除 | 新增 |
| `replace_refs_do_not_change_tree_of` | 对 commit 做 `git replace` 之后，`tree_of` 仍返回原 commit 的 tree，导出的也是原 tree 的内容 | tgrep-core/tests/generations.rs::replace_refs_do_not_change_exact_committed_tree |
| `gitlink_in_base_tree_is_unsupported` | tree 里有 160000 条目 → `tree_of` 与 `export_tree` 都返回 `Unsupported`（`detail.path`），不建任何目录 | tgrep-core/tests/generations.rs::gitlinks_are_membership_only_and_missing_blobs_are_errors |
| `missing_blob_refused_before_export` | 手工删掉一个松散 blob → `tree_of` 返回 `NoSuchCommit`，在导出之前就拒绝；已有的基线目录不受影响 | tgrep-core/tests/generations.rs::gitlinks_are_membership_only_and_missing_blobs_are_errors |
| `non_utf8_tree_path_is_not_text` | `git mktree` 造出含 `\xff.txt` 的 tree → `tree_of` 返回 `NotText`，不跳过 | tgrep-core/tests/generations.rs::non_unicode_repository_roots_are_lossless_but_tracked_paths_are_unsupported |
| `colliding_paths_are_unsupported` | tree 里两条路径 NFC 规整后相同，或在 ignorecase 仓库里 ASCII 小写后相同（`README` 与 `readme`）→ `Unsupported`，`detail` 列出两条 | 新增 |
| `overlong_component_refused_before_writing` | 路径分量 256 字节 → `tree_of` 返回 `InvalidInput`；导出时 `dst` 加路径超过 1024 字节同样在写之前拒绝 | layerfs-content/tests/filesystem_limits.rs::a_component_name_is_accepted_at_255_bytes_and_refused_at_256 |
| `ls_tree_output_parse_is_exact` | 解析器对截断的记录、缺结尾 NUL、字段数不对、OID 长度不对、未知 mode 一律报错，不凭空造条目 | layerfs-bridge/tests/install_records.rs::bounded_manifest_is_exact_and_does_not_cap_stream_length |
| `tree_validation_is_cached_per_tree` | 同一个 `TreeOid` 第二次 `tree_of` 不再起 ls-tree；不同 commit 指向同一个 tree 时共用结论 | 新增 |
| `export_tree_matches_git_checkout_byte_for_byte` | 混合 tree（`.gitignore`；`.gitattributes` 的 `text`、`eol=crlf`、`working-tree-encoding=UTF-16`、`ident`；一对 smudge/clean 过滤器；二进制文件、空文件、可执行文件；悬空、自指、指向树外的符号链接；深目录；非 ASCII 文件名；再加 `core.autocrlf=true` 的变体）导出后，与 `git clone` 再检出同一 commit 的工作树逐文件比较：字节、可执行位、符号链接目标全同；导出目录里没有 `.git`、没有空目录，每个路径是独立的 inode | tgrep-core/src/worktrees/tests.rs::crlf_and_smudge_checkouts_are_verified_even_when_git_reports_clean；tgrep-core/tests/generations.rs::raw_blob_semantics_require_decoded_identity_not_git_cleanliness；layerfs-daemon/tests/complete_installed_roots.rs::all_supported_kernel_paths_survive_install_with_full_oracles；layerfs-daemon/tests/host_install.rs::authenticated_install_opens_one_store_and_reads_the_complete_root |
| `export_filter_failure_matches_checkout` | smudge 命令不存在：`filter.<d>.required = true` → 返回 `Unknown`，`dst` 不存在，临时目录保留；没设 required → 与 `git checkout` 一样写原始 blob | tgrep-core/tests/generations.rs::raw_blob_semantics_require_decoded_identity_not_git_cleanliness |
| `export_tree_refuses_existing_destination_untouched` | `dst` 已存在 → `Exists`；`dst` 里的字节与 mtime 不变，也没建临时目录 | layerfs-daemon/tests/host_install.rs::existing_store_is_refused_before_upload_and_preserved |
| `export_typed_refusals_write_nothing` | gitlink、`NotText`、碰撞、超长、父目录不存在、`dst` 在主 checkout 里，这些拒绝都发生在建临时目录之前：`dst` 的父目录里没有多出任何条目 | layerfs-api/tests/local_install.rs::an_installed_manifest_is_refused_before_anything_is_written |
| `interrupted_export_keeps_temp_and_never_creates_destination` | checkout-index 进行中杀掉 git（过滤器里注入 SIGKILL）→ `Unknown`；`dst` 不存在；`.export-*` 临时目录原样保留，按前缀可以列出；再次导出到同一个 `dst` 照常成功，用的是新临时目录 | layerfs-daemon/tests/host_install.rs::interrupted_transfer_keeps_original_partial_file_and_claim_refuses_another_installer |
| `concurrent_export_never_overwrites_destination` | 不加 daemon 的锁，两个导出抢同一个 `dst`：最后恰好一个完整的 `dst`；另一个得到 `Exists`（写之前发现）或 `Unknown`（改名时撞上，临时目录保留） | layerfs-daemon/tests/host_install.rs::interrupted_transfer_keeps_original_partial_file_and_claim_refuses_another_installer |
| `export_large_tree_is_complete` | 10 万个文件的 tree 导出后文件数与 ls-tree 一致（慢测试，`#[ignore]`，发布前跑） | layerfs-daemon/tests/complete_installed_roots.rs::huge_kernel_namespace_is_complete_after_install |
| `export_is_durable_before_rename` | 经测试用的同步钩子计数：改名之前每个文件与目录都 `fsync` 过，并做过一次 `F_FULLFSYNC`；改名用的是 `RENAME_EXCL` | 新增 |
| `exported_workspace_git_status_is_clean` | 导出加 attach 之后，工作区里 `git status --porcelain` 为空，过滤器与 eol 配置开着也是 | tgrep-core/src/worktrees/tests.rs::crlf_and_smudge_checkouts_are_verified_even_when_git_reports_clean |
| `attach_worktree_makes_git_work_in_workspace` | attach 之后工作区 HEAD detached 在 head、没有 upstream，`git status --porcelain` 为空，`git log -1` 是 head；会话里 `git commit` 的对象进主 checkout 的 object store；主 checkout 的 `git worktree list` 不列出它，主 checkout 的 index、HEAD、`refs/heads` 不变 | 新增 |
| `reattach_moves_head_and_rebuilds_index` | 已 attach 的工作区再 attach 到新 head：HEAD 移到新 head，`logs/HEAD` 记下旧值；index 等于新 tree；agent 暂存过的条目从 index 消失，文件内容不动 | 新增 |
| `attach_refuses_foreign_dot_git` | `worktree/.git` 已存在、且不是指向给定 `gitdir` 的 gitfile → `Exists`，什么都不改 | 新增 |
| `index_writers_refuse_the_main_checkout` | 把主 checkout 当 `worktree` 传给 `stage` 或 `attach_worktree`，或把 `gitdir` 放在 common dir 里 → `InvalidInput`；主 checkout 的 index 字节不变 | 新增 |
| `stage_after_export_roundtrips_to_same_tree` | 导出一个带 `eol=crlf`、smudge/clean 过滤器、符号链接、可执行文件的 tree，attach，内容不改就 stage 全部路径 → `write-tree` 得到原 tree OID | 新增 |
| `stage_applies_clean_filters` | 工作区里改过的 CRLF 文件与过滤器文件，stage 之后的 blob 是检入形态（LF、clean 过滤后的字节），与在普通 checkout 里 `git add` 同样内容得到的 blob 相同 | 新增 |
| `stage_handles_deletions_and_type_transitions` | 批次含删除、文件变目录（`a` 删、`a/b` 增）、目录变文件、文件与符号链接互换：结果 tree 与在普通 checkout 里做同样改动再 `git add -A` 的 tree 相同 | 新增 |
| `stage_never_adds_unlisted_paths` | 批次只点 `a/b`，`a/` 下还有没选的文件与被忽略的文件：结果 tree 只多出 `a/b`；不在批次里的已跟踪文件即使在磁盘上改过，也保持 index 里的原样 | 新增 |
| `stage_refuses_special_files_before_writing` | 批次路径是 FIFO → `NotFile`，index 不变 | 新增 |
| `commit_tree_creates_single_parent_commit` | 结果 commit 的 tree、唯一的 parent、消息都与输入逐字节相同，作者取 git 配置；不动任何 ref；配置里有 `commit.gpgSign=true` 也不签名、不挂住 | 新增 |
| `commit_tree_without_identity_is_invalid_input` | `HOME` 指向没有 `user.email` 的配置 → `InvalidInput`（`detail.rule = "git-identity"`），不移动任何 ref | 新增 |
| `push_unknown_on_timeout` | 远端的 receive-pack 挂住超过 `timeout` → `Ok(PushOutcome::Unknown)`；整个进程组被杀；发布链与 `refs/strata/remotes` 不变 | layerfs-bridge/tests/install_records.rs::installed_and_refused_keep_original_publication_knowledge |
| `push_without_ref_status_is_unknown` | 远端在 `reference-transaction` 钩子的 `committed` 阶段杀掉 receive-pack：远端分支其实已更新，push 没有状态行、退出 128 → `Unknown` 而不是 `Refused`；本地 pre-push 钩子拒绝（同样没有状态行）也是 `Unknown` | 新增 |
| `push_refused_carries_remote_message` | 非快进 → `Refused`，含 `non-fast-forward`；远端 pre-receive 拒绝 → `Refused`，含 `pre-receive hook declined` 与钩子的 `remote:` 原话；远端分支不变 | 新增 |
| `push_pushed_fast_forward_new_and_up_to_date` | 快进、新分支、已是最新（`=`）三种都是 `Pushed`；之后 `refs/strata/remotes/<remote>/<dst>` 等于推送的 commit，主 checkout 的 `refs/remotes/<remote>/<dst>` 不变 | 新增 |
| `push_sets_strata_publish_and_never_forces` | 子进程环境有 `STRATA_PUBLISH=1`；argv 里没有 `--force`、`--force-with-lease`、`+` 开头的 refspec、`--no-verify`；`src_ref` 不在 `refs/strata/` 下 → `Err(InvalidInput)`，不存在 → `Err(NoSuchCommit)` | 新增 |
| `merge3_nonoverlapping_hunks_clean` | 两边的改动之间至少隔一行未改的 base 行 → `Clean`，结果同时含两边的改动；恰好隔一行也干净 | 新增 |
| `merge3_same_hunk_conflict` | 两边把同一行改成不同内容 → `Conflict`，一个 Hunk，base、ours、theirs 三段文本与起始行号都对 | 新增 |
| `merge3_adjacent_changes_conflict` | 两边各改相邻的一行，中间没有未改行 → `Conflict`；两边在文件末尾追加不同的行 → `Conflict` | 新增 |
| `merge3_identical_or_one_sided_changes_clean` | 两边改成同样的内容 → `Clean`，只取一份；只有一边改 → `Clean`，即那一边；base 为空串而两边相同（add/add）→ `Clean` | 新增 |
| `merge3_preserves_line_endings` | CRLF 文件按行合并且保留 CRLF；最后一行没有换行符也照样保留 | 新增 |
| `merge3_binary_is_whole_file` | 任一侧是二进制且两边都改了 → `Conflict`，恰好一个整文件 Hunk；只有一边改 → `Clean`，即那一边 | 新增 |
| `merge3_agrees_with_git_merge_file_on_corpus` | 固定语料（每例的行级 diff 唯一）上，`Clean` 与 `Conflict` 的判定与 `git merge-file -p` 的退出码一致，`Clean` 时字节相同 | 新增 |
| `is_binary_matches_git_rule` | 前 8000 字节有 NUL → 二进制；NUL 在第 8001 字节 → 不是；空串不是 | 新增 |
| `main_checkout_untouched_except_strata_refs` | 对一个主 checkout 依次跑 `open`、`fetch`、`tree_of`、`export_tree`、`attach_worktree`（工作区在别处）、`stage`、`commit_tree`、`update_ref`、`push` 之后：主 checkout 的工作树文件、`<common>/index` 的字节、`HEAD`、`config`、`hooks/`、`refs/heads/*`、`refs/remotes/*`、`refs/tags/*`、`refs/stash`、`FETCH_HEAD`、`.git/worktrees/` 都与之前逐字节相同，变化只有 `objects/` 与 `refs/strata/*` | layerfs-api/tests/local_install.rs::import_installs_the_store_in_process |

共 99 个用例：56 个引用矩阵行，43 个新增。表里引用了 3f 目标为 gitio 的全部 43 行（其中 `non_unicode_repository_roots_are_lossless_but_tracked_paths_are_unsupported` 当前目标是 types，3f 修订按 task-3f-review 的 I2 改为 gitio）、3d 目标为 gitio 的全部 10 行，以及目标不是 gitio、但理由点名 gitio 用例的四行：3f 的 `linked_worktrees_and_equal_trees_share_one_arc`（search）、`interrupted_and_failed_publications_are_not_discoverable`（deps）、`sha256_repositories_and_unicode_paths_are_supported`（`—`），3a 的 `a_component_name_is_accepted_at_255_bytes_and_refused_at_256`（`—`）。

理由点名 gitio、但本文不为它们单列用例的行：

- 3f tgrep-core/src/git_index.rs::metadata_paths_preserve_native_bytes：当前目标 gitio、保留；3f 修订（task-3f-review 的 Minor）改为不保留，与同一部分里另外三条"APFS 不产生非 UTF-8 名"的行一致。gitio 不迁移这条测试，gitfile 路径按字节读取的代码仍照抄。
- 3f tgrep-core/src/walker.rs::public_matcher_lazily_freezes_tracked_exemptions_on_first_match：不保留；理由说 `read_index` 的返回值是快照，由 `read_index_snapshot_is_immutable_and_cached` 断言。
- 3a layerfs-persistence/tests/merge_records.rs::records_are_private_guarded_and_removed、3c layerfs-overlay/tests/indexed_operation_record.rs::complete_keys_and_full_file_scopes_never_alias_other_custody、3c layerfs-store-service/tests/kept_plans.rs::a_full_walk_answer_is_kept_like_a_plan_and_let_go_like_one：不保留；理由说 strata 的合并由 `merge3` 在 staging 目录里完成，不落库，也不缓存比较计划。`merge3` 是纯函数，没有可测的状态。
- 3c layerfs-overlay/tests/indexed_operation_record.rs::guarded_batches_are_atomic_capacity_bounded_and_allow_a_128_child_node_shape：不保留；理由指向本文的 `update_ref_cas_refuses_moved_ref`，表里已列为新增。
- 3b layerfs-project/tests/acquisition_custody.rs::a_backing_read_failure_is_definite_and_is_cleaned_up：目标 session；理由里 gitio 的 `NoSuchBranch` 经 `session::new` 原样透传，gitio 一侧由 `fetch_missing_branch_is_no_such_branch` 覆盖。

## 8. 实现任务（TDD）

前提：workspace 根 `Cargo.toml` 与 `crates/strata-types` 已经存在（types.md §8）。每个任务的测试都用 `tests/common/mod.rs` 的辅助函数在临时目录里建仓库、手工接的工作区与裸远端，需要 `PATH` 里有 git ≥ 2.31；测试只用临时目录，从不碰 `/Volumes/GWD/inter`。

### Task 1: 子进程环境与仓库身份

**Files:**
- Create: `crates/strata-gitio/Cargo.toml`、`crates/strata-gitio/THIRD-PARTY-NOTICES`、`crates/strata-gitio/src/lib.rs`、`crates/strata-gitio/src/command.rs`（抄 tgrep `generations/git.rs`，带出处头注释）、`crates/strata-gitio/src/repo.rs`
- Modify: `Cargo.toml`（把 `crates/strata-gitio` 加进 `members`）
- Test: `crates/strata-gitio/tests/repo.rs`、`crates/strata-gitio/tests/common/mod.rs`

**Interfaces:**
- Consumes: `strata_types::{RepoId, StrataError, Code}`
- Produces: `Result`、`MIN_GIT_VERSION`、`Git::open`、`Git::common_dir`、`Git::repo_id`、`Git::command`

- [ ] **Step 1: 写失败的测试**：§7 的 `command_strips_inherited_git_env`、`command_sets_safety_flags`、`open_resolves_one_repo_id_for_checkout_and_workspace`（工作区那一半先用辅助函数手工接）、`open_refuses_non_repository_bare_and_missing`、`sha256_repository_is_unsupported`、`old_git_is_unsupported`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-gitio --test repo`
Expected: FAIL（`Git` 还没定义，编译不过）

- [ ] **Step 3: 写让测试通过的最少实现**：照抄 tgrep 的 `command()`，补上去掉 `STRATA_PUBLISH` 与 `LC_ALL=C`（§5.1）；`open` 按 §5.2；版本解析写成纯函数，单测直接喂字符串。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-gitio --test repo`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/strata-gitio
git commit -m "feat(gitio): sanitized git command and repository identity"
```

### Task 2: index 解析与 `read_index`

**Files:**
- Create: `crates/strata-gitio/src/index.rs`（抄 tgrep `git_index.rs`，带出处头注释）
- Test: `crates/strata-gitio/src/index.rs` 的单测（原样迁移 tgrep 的测试）、`crates/strata-gitio/tests/index.rs`

**Interfaces:**
- Consumes: `RepoPath`；`Code::{NoSuchPath, NotText, InvalidInput, Unsupported, Unknown}`；Task 1 的 `Git`
- Produces: `IndexInfo`、`IndexInfo::is_tracked`、`IndexInfo::tracked_under`、`Git::read_index`

- [ ] **Step 1: 写失败的测试**：§7 从 `metadata_paths_strip_only_git_line_endings` 到 `read_index_refuses_worktree_of_another_repo` 的 23 个
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-gitio --lib index:: && cargo test -p strata-gitio --test index`
Expected: FAIL（`IndexInfo`、`read_index` 还没有）

- [ ] **Step 3: 写让测试通过的最少实现**：按 §2 的改动表抄入解析器；`read_index` 按 §5.7；非 UTF-8 返回 `NotText`；按 index 文件的 stat 缓存。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-gitio --lib index:: && cargo test -p strata-gitio --test index`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-gitio/src/index.rs crates/strata-gitio/src/lib.rs crates/strata-gitio/tests/index.rs
git commit -m "feat(gitio): parse the git index and config directly"
```

### Task 3: `is_ignored`

**Files:**
- Create: `crates/strata-gitio/src/ignore.rs`
- Test: `crates/strata-gitio/tests/ignore.rs`、`crates/strata-gitio/tests/fixtures/ignore-corpus/`

**Interfaces:**
- Consumes: `Git::read_index`、`IndexInfo::is_tracked`
- Produces: `Git::is_ignored`

- [ ] **Step 1: 写失败的测试**：§7 从 `index_parse_tracked_never_ignored` 到 `is_ignored_agrees_with_git_on_corpus` 的 18 个
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-gitio --test ignore`
Expected: FAIL（`is_ignored` 还没有）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.8 的优先级栈、父目录排除、ASCII 折叠；全局排除文件的路径在 `open` 里解析（§4.3）；规则文件按 stat 缓存；匹配引擎按 §9 选定。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-gitio --test ignore`
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
- Produces: `Git::fetch`、`Git::read_ref`、`Git::is_ancestor`、`Git::merge_base`、`Git::update_ref`、`Git::delete_ref`、`remote_ref`

- [ ] **Step 1: 写失败的测试**：§7 的 `option_like_arguments_are_refused_not_executed`、`fetch_writes_only_the_strata_tracking_ref`、`fetch_missing_branch_is_no_such_branch`、`fetch_timeout_is_unknown_and_moves_nothing`、`ancestry_answers_come_from_git`、`read_ref_absent_is_none`、`update_ref_cas_refuses_moved_ref`、`update_ref_create_requires_absent`、`refs_outside_strata_namespace_are_refused`、`delete_ref_is_compare_and_swap`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-gitio --test refs`
Expected: FAIL（这些函数还没有）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.3 与 §5.10；名字校验先于子进程；超时杀进程组。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-gitio --test refs`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-gitio/src/remote.rs crates/strata-gitio/src/refs.rs crates/strata-gitio/src/lib.rs crates/strata-gitio/tests/refs.rs
git commit -m "feat(gitio): fetch into refs/strata, ancestry and compare-and-swap refs"
```

### Task 5: `tree_of` 与 tree 校验

**Files:**
- Create: `crates/strata-gitio/src/tree.rs`（解析部分抄 tgrep `parse_entries`）
- Test: `crates/strata-gitio/tests/tree.rs`

**Interfaces:**
- Consumes: Task 1 的 `Git::command`；`RepoPath`
- Produces: `Git::tree_of`

- [ ] **Step 1: 写失败的测试**：§7 的 `replace_refs_do_not_change_tree_of`、`gitlink_in_base_tree_is_unsupported`（`export_tree` 那一半在 Task 6 补断言）、`missing_blob_refused_before_export`、`non_utf8_tree_path_is_not_text`、`colliding_paths_are_unsupported`、`overlong_component_refused_before_writing`（导出那一半同上）、`ls_tree_output_parse_is_exact`、`tree_validation_is_cached_per_tree`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-gitio --test tree`
Expected: FAIL（`tree_of` 还没有）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.4；结论按 `TreeOid` 缓存。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-gitio --test tree`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-gitio/src/tree.rs crates/strata-gitio/src/lib.rs crates/strata-gitio/tests/tree.rs
git commit -m "feat(gitio): resolve and validate base trees"
```

### Task 6: `export_tree`

**Files:**
- Create: `crates/strata-gitio/src/export.rs`
- Test: `crates/strata-gitio/tests/export.rs`

**Interfaces:**
- Consumes: `Git::tree_of`；`libc::{fcntl, F_FULLFSYNC, renameatx_np, RENAME_EXCL}`
- Produces: `Git::export_tree`、`EXPORT_TEMP_PREFIX`

- [ ] **Step 1: 写失败的测试**：§7 的 `export_tree_matches_git_checkout_byte_for_byte`、`export_filter_failure_matches_checkout`、`export_tree_refuses_existing_destination_untouched`、`export_typed_refusals_write_nothing`、`interrupted_export_keeps_temp_and_never_creates_destination`、`concurrent_export_never_overwrites_destination`、`export_large_tree_is_complete`、`export_is_durable_before_rename`；补上 Task 5 留下的两处导出断言
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-gitio --test export -- --include-ignored`
Expected: FAIL（`export_tree` 还没有）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.5 的七步；私有 `GIT_INDEX_FILE`；持久化与 `RENAME_EXCL` 经一个可替换的同步层，测试借它计数。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-gitio --test export -- --include-ignored`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-gitio/src/export.rs crates/strata-gitio/src/lib.rs crates/strata-gitio/tests/export.rs crates/strata-gitio/tests/tree.rs
git commit -m "feat(gitio): export trees byte-identical to git checkout"
```

### Task 7: `attach_worktree`、`stage`、`commit_tree`

**Files:**
- Create: `crates/strata-gitio/src/worktree.rs`、`crates/strata-gitio/src/stage.rs`、`crates/strata-gitio/src/commit.rs`
- Test: `crates/strata-gitio/tests/worktree.rs`

**Interfaces:**
- Consumes: `Git::tree_of`、`Git::export_tree`、`Git::read_ref`
- Produces: `Git::attach_worktree`、`Git::stage`、`Git::commit_tree`

- [ ] **Step 1: 写失败的测试**：§7 从 `exported_workspace_git_status_is_clean` 到 `commit_tree_without_identity_is_invalid_input` 的 12 个；`open_resolves_one_repo_id_for_checkout_and_workspace` 改用 `attach_worktree` 建工作区
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-gitio --test worktree`
Expected: FAIL（这三个函数还没有）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.6、§5.9、§5.10 的 `commit_tree`；所有写 index 的命令显式设 `GIT_INDEX_FILE`；主 checkout 判定按 §5.2 第 5 步记下的工作树根。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-gitio --test worktree && cargo test -p strata-gitio --test repo`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-gitio/src/worktree.rs crates/strata-gitio/src/stage.rs crates/strata-gitio/src/commit.rs crates/strata-gitio/src/lib.rs crates/strata-gitio/tests/worktree.rs crates/strata-gitio/tests/repo.rs
git commit -m "feat(gitio): attach workspaces, stage through clean filters, commit trees"
```

### Task 8: `push`

**Files:**
- Create: `crates/strata-gitio/src/push.rs`
- Test: `crates/strata-gitio/tests/push.rs`（裸远端、挂住的 receive-pack 包装脚本、`reference-transaction` 钩子都由辅助函数生成）

**Interfaces:**
- Consumes: `Git::read_ref`、`Git::update_ref`；types.md 的 `PushOutcome`
- Produces: `Git::push`

- [ ] **Step 1: 写失败的测试**：§7 的 `push_unknown_on_timeout`、`push_without_ref_status_is_unknown`、`push_refused_carries_remote_message`、`push_pushed_fast_forward_new_and_up_to_date`、`push_sets_strata_publish_and_never_forces`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-gitio --test push`
Expected: FAIL（`push` 还没有）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.11；推到 `get-url --push` 的地址；只按本 ref 的 porcelain 状态行归类；`Pushed` 后更新 `remote_ref`；原始输出经 `tracing` 记录。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-gitio --test push`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-gitio/src/push.rs crates/strata-gitio/src/lib.rs crates/strata-gitio/tests/push.rs
git commit -m "feat(gitio): push with a definite outcome or Unknown"
```

### Task 9: `merge3` 与 `is_binary`

**Files:**
- Create: `crates/strata-gitio/src/merge.rs`
- Test: `crates/strata-gitio/tests/merge.rs`、`crates/strata-gitio/tests/fixtures/merge-corpus/`

**Interfaces:**
- Consumes: §9 选定的 diff crate
- Produces: `merge3`、`Merge3`、`Hunk`、`is_binary`

- [ ] **Step 1: 写失败的测试**：§7 从 `merge3_nonoverlapping_hunks_clean` 到 `is_binary_matches_git_rule` 的 8 个
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-gitio --test merge`
Expected: FAIL（`merge3` 还没有）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.12；相邻即冲突，同改合一；二进制按整文件处理。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-gitio --test merge`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-gitio/src/merge.rs crates/strata-gitio/src/lib.rs crates/strata-gitio/tests/merge.rs crates/strata-gitio/tests/fixtures/merge-corpus
git commit -m "feat(gitio): hunk-level three-way merge matching git merge-file"
```

### Task 10: 不变量 2 的集成测试

**Files:**
- Test: `crates/strata-gitio/tests/invariants.rs`

**Interfaces:**
- Consumes: Task 1–9 的全部公开函数
- Produces: 无新接口

- [ ] **Step 1: 写失败的测试**：§7 的 `main_checkout_untouched_except_strata_refs`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-gitio --test invariants`
Expected: FAIL（测试文件刚写，快照比较的辅助函数还没有；若前面的任务有遗漏，例如漏设 `GIT_INDEX_FILE` 或推到了 remote 名，也在这里暴露）

- [ ] **Step 3: 写让测试通过的最少实现**：补齐快照辅助函数（对主 checkout 的工作树、`<common>/index`、`HEAD`、`config`、`hooks/`、各类 ref 与 `FETCH_HEAD` 做逐字节快照）；若比较失败，修对应的函数，不放宽断言。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-gitio`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-gitio/tests/invariants.rs crates/strata-gitio/tests/common/mod.rs
git commit -m "test(gitio): the main checkout changes only in objects and refs/strata"
```

## 9. 构建者决定记录

| 事项 | 选择 | 理由 | 日期 |
| --- | --- | --- | --- |
| 工作区 `.git` 的接法（design §6.1 第 2 步、§12 交给构建者） | [我的决定] gitfile 指向 strata 自己的私有 git 目录，`commondir` 指回主 checkout 的 common dir；不用 `git worktree add` | 不变量 2：`git worktree add` 要写主 checkout 的 `.git/worktrees/<id>/`。实测可行：status、log、commit 正常，对象进共享库。代价也实测过：主 checkout 的 `git gc` 看不到工作区的 HEAD 与 index，只被工作区引用的对象（会话里的本地 commit、暂存的 blob）过了 `gc.pruneExpire`（缺省两周）会被清掉，之后工作区里 `git log` 报 `bad object HEAD`；登记过的 worktree 的 commit 则留下。strata 的数据在 Store 里，不受影响；要保住工作区 HEAD，session 可以把它记进 `refs/strata/` 下的 ref | 2026-10-22 |
| 三方合并的行级 diff 实现（design §5、§12） | 待定 | 约束：判定与 `git merge-file` 相同（§5.12），§7 的 merge3 用例全绿，包括与 git 的语料对拍。候选：imara-diff 加自写的区间合并；diffy 的 merge | — |
| gitignore 匹配引擎 | 待定 | 约束：§5.8 的优先级、父目录排除、ASCII 折叠，`is_ignored_agrees_with_git_on_corpus` 全绿。候选：`ignore` 0.4 的逐文件 `Gitignore`，由 gitio 叠优先级（它的 `case_insensitive` 按 Unicode 折叠，要改成 ASCII；tgrep-core 依赖它的一份打过补丁的副本，两份并存时要留意）；gix-ignore | — |
| git 版本下限 | [我的决定] 2.31 | §2 列出的三个参数；本机 2.54.0 | 2026-10-22 |
| 子进程的 locale | [我的决定] `LC_ALL=C` | fetch 的错误分类读英文原话 | 2026-10-22 |
| fetch 超时 | [我的决定] 120 s 的常量，首个 spike 后修正 | 网络操作必须有界，否则 `new` 握着 per-repo 写锁一直等 | 2026-10-22 |
| strata 的远端跟踪 ref | [我的决定] `refs/strata/remotes/<remote>/<branch>`，fetch 带 `--refmap=` | 不写 `refs/remotes/*`（§4.1） | 2026-10-22 |
| 推送地址 | [我的决定] 推到 `git remote get-url --push` 给出的地址 | 推到 remote 名会更新 `refs/remotes/*`（实测） | 2026-10-22 |
| pre-push 钩子 | [我的决定] 不带 `--no-verify` | 仓库自己的钩子照跑；钩子拒绝时没有状态行，报 `Unknown` | 2026-10-22 |
| 导出的持久化与原子性 | [我的决定] 每个文件与目录 `fsync`，一次 `F_FULLFSYNC`，`renameatx_np(RENAME_EXCL)` | §5.5：基线目录只以完整的形态出现，竞争下也不覆盖 | 2026-10-22 |
| tree 里的路径碰撞 | [我的决定] NFC 相同，或 ignorecase 下 ASCII 折叠相同，都算碰撞，返回 `Unsupported` | §5.4；非 ASCII 的大小写碰撞属于 APFS 的规则 | 2026-10-22 |
| sparse index | [我的决定] `Unsupported` | gitio 建的工作区 index 总是完整的 | 2026-10-22 |
| index 文件不存在 | [我的决定] 当作空集合 | git 的语义 | 2026-10-22 |
| push 与 fetch 的原始输出 | [我的决定] 经 `tracing` 记日志 | `PushOutcome::Unknown` 不带文字 | 2026-10-22 |
| 导出耗时 | 待定 | design §8 没有导出的数字；首个 spike 实测 tool（4196 个文件）与 staff-api（4947 个）后写回 | — |
