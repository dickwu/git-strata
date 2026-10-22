# git-strata — 共享类型

> **范围**：所有跨模块的名字与形状，包括标识、结构、错误码、CLI 的 JSON 输出约定、crate 布局与依赖方向。来源是 [design.md](design.md) §4（词汇表）、§5（模块）、§6（流程）、§9（不变量）；错误码词汇表取自 LayerFS `sdk/js/src/errors.js` 的 `CODES`（main 44d3a26e5）。与 design.md 冲突时以 design.md 为准。
>
> 本阶段只写文档。下面的 Rust 定义是将来 `crates/strata-types` 的内容，现在不建 Cargo 工程。

## 1. 名字一览

| 名字 | 种类 | 含义 | 定义 |
| --- | --- | --- | --- |
| `RepoName` | 标识 | 配置里的仓库名，如 `tool`；同时是工作区的目录名 | §3 |
| `RepoId` | 标识 | 仓库身份：git common dir 的规范路径 | §3 |
| `SessionId` | 标识 | 会话 id，生成，`s-` + 8 位 base32 | §3 |
| `TreeOid` | 标识 | git tree 对象 id，40 位十六进制 | §3 |
| `CommitOid` | 标识 | git commit 对象 id，40 位十六进制 | §3 |
| `BlobId` | 标识 | blob 的内容地址（BLAKE3） | §3 |
| `DepsSetId` | 标识 | 依赖集 id：BLAKE3(安装输入 ∥ 平台 ∥ 工具链) | §3 |
| `CheckpointId` | 标识 | checkpoint 序号，每会话递增 | §3 |
| `RepoPath` | 标识 | 仓库内相对路径，`/` 分隔，NFC | §3 |
| `ProposalId` | 标识 | 依赖变更提议的序号（u64） | §3 |
| `ViewId` | 标识 | 检索视图的 id，由 search 生成 | §3 |
| `BaseGen` | 结构 | 基线 generation：repo + tree + 依赖集 | §4 |
| `DepsSet` | 结构 | 封存在 Store 里的依赖集 | §4 |
| `BaseManifest` | 结构 | 基线清单：一个 tree 的全部文件，只记内容 | §4 |
| `ManifestEntry` | 结构 | 基线清单中的一个文件：blob、mode、大小 | §4 |
| `OverlayEntry` | 结构 | overlay 中的一条整文件差异 | §4 |
| `EntryKind` | 枚举 | 增、改、删、改名、未跟踪 | §4 |
| `Checkpoint` | 结构 | 一个工作区在某一时刻的完整 overlay | §4 |
| `CheckpointReason` | 枚举 | 记这个 checkpoint 的原因 | §4 |
| `Batch` | 结构 | 从一个会话的一个 repo 挑出的路径 | §4 |
| `ReleaseGroup` | 结构 | 一次发布的全部批次 | §4 |
| `PublishTarget` | 结构 | 目标分支、是否推送、是否要确认 | §4 |
| `PushOutcome` | 枚举 | 一次推送的结果：推上了、被拒绝、不确定 | §4 |
| `SessionManifest` | 结构 | 会话清单 | §4 |
| `WorkspaceState` | 结构 | 会话里一个 repo 的工作区 | §4 |
| `ConfirmToken` | 结构 | 一次人类确认的凭据，一次一用，只在 daemon 内存里 | §4 |
| `Action` | 枚举 | 需要人类确认的三种动作 | §4 |
| `Config`、`RepoConfig` | 结构 | 全局配置与每仓库配置 | [config.md](config.md) |
| `StrataError` | 结构 | 被拒绝的调用：`code` + `message` + `detail` | §6 |
| `Code` | 枚举 | 错误码，共 39 个 | §6 |

## 2. 约定

### 2.1 命名

- 本文的名字是跨模块的唯一写法。config.md、test-matrix、`modules/*.md` 只用这些名字，不另起同义词。
- 只在一个 crate 内部使用的类型，由该模块文档的 §3 定义，算模块内部名。一旦它出现在另一个模块的接口里，就要先加进本文。
- `Config` 与 `RepoConfig` 是例外：它们放在 `strata-types` 里，但定义写在 [config.md](config.md)。本文只链接，不重复。

### 2.2 derive 与 serde

- 所有公开类型 derive `Clone`、`Debug`、`PartialEq`、`Serialize`、`Deserialize`；字段都能比较相等的再加 `Eq`。标识另加 `Hash`、`PartialOrd`、`Ord`，例如 `RepoName` 要作 `BTreeMap` 的键。唯一的例外是 `ConfirmToken`：它只 derive `Debug`，既不能复制，也不能序列化（见 §4）。
- 有格式的标识（`RepoName`、`SessionId`、`TreeOid`、`CommitOid`、`BlobId`、`DepsSetId`、`RepoPath`）字段私有，只能经 `TryFrom<String>` 构造。serde 也走这条路：`#[serde(try_from = "String", into = "String")]`，所以反序列化同样会校验；格式不对返回 `Code::InvalidInput`。没有格式可校验的 `RepoId`、`CheckpointId`、`ProposalId`，以及格式由 search 决定、对其他模块不透明的 `ViewId`，都用 `#[serde(transparent)]`。
- 字段名就是 JSON 的键，保持 snake_case，不用 `rename_all`。
- 带数据的枚举用内部标签 `#[serde(tag = "kind")]`。`OverlayEntry` 的 `kind` 字段再加 `#[serde(flatten)]`，这样一条条目在 JSON 里只有一层对象。`PushOutcome` 有元组变体，内部标签装不下其中的字符串，所以改用相邻标签 `#[serde(tag = "kind", content = "message")]`。不带数据的枚举（`Code`）序列化为变体名字符串，如 `"Conflict"`。
- `Option` 为 `None` 的字段不输出（`skip_serializing_if = "Option::is_none"`），读入时缺省为 `None`（`default`）。
- `SystemTime` 序列化为 RFC 3339 UTC 字符串，用哪个适配器交给构建者；`RepoPath` 与 `PathBuf` 序列化为字符串。
- `strata-types` 不做 I/O，只依赖 `serde`、`serde_json`（`StrataError.detail` 要用）和做 NFC 规整、十六进制校验的纯计算 crate。

## 3. 标识

```rust
use std::path::PathBuf;
use serde::{Deserialize, Serialize};

/// 配置里的仓库名，即 `[repo.<name>]` 中的 `<name>`，如 `tool`、`staff-api`。
/// 它同时是工作区在兄弟布局里的目录名（`<root>/sessions/<ticket>/<repo>/`，design §4），
/// 所以必须等于主 checkout 的目录名，dev-link 的 `../quick-start` 这类相对路径才对得上（design §3）。
/// 合法字符为 `[A-Za-z0-9._-]`，非空，且不是 `.` 或 `..`。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RepoName(String);

/// 仓库身份：git common dir 的规范路径（`git rev-parse --git-common-dir` 之后取 realpath，沿用 tgrep 的做法）。
/// 主 checkout 和接在它 object store 上的所有工作区是同一个 `RepoId`。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RepoId(PathBuf);

/// 会话 id：`s-` + 8 位 base32（RFC 4648 字母表的小写形式 `a–z2–7`，40 bit 随机），如 `s-k3q7xm2a`。
/// 由 `session::new` 生成，与 ticket 无关，一个 ticket 可以有多个会话。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SessionId(String);

/// git tree 对象 id：40 位小写十六进制（SHA-1 对象格式）。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TreeOid(String);

/// git commit 对象 id：格式同 TreeOid。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CommitOid(String);

/// Store 里 blob 的内容地址：文件内容的 BLAKE3，64 位小写十六进制。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct BlobId(String);

/// 依赖集 id：BLAKE3(inputs_hash ∥ platform ∥ toolchain)，64 位小写十六进制。
/// 三段如何拼接编码由 deps.md 规定，要求同样的输入必定得到同样的 id。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DepsSetId(String);

/// checkpoint 序号：每个会话一个计数器，从 1 递增；(SessionId, CheckpointId) 唯一。
/// 存进 SQLite 的值不超过 i64::MAX，因为 SQLite 的 INTEGER 是有符号 64 位；ProposalId 和 ReleaseGroup.id 也一样。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CheckpointId(u64);

/// 仓库内相对路径：`/` 分隔，UTF-8，Unicode NFC；不以 `/` 开头或结尾，不含空段、`.`、`..`。
/// derive 出来的比较逐字节进行、区分大小写；要按仓库的 `core.ignorecase` 比较，用 `apfs::case_fold_eq`。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RepoPath(String);

/// 依赖变更提议的序号：`deps::propose` 记录提议时由 Store 分配；`git-strata deps approve <id>` 用它指名提议。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProposalId(u64);

/// 检索视图的 id：search 给一个 (会话, repo) 挂上视图时生成（`search::attach`）。
/// daemon 和 cli 只把它原样传回 search；格式由 search.md 规定，对其他模块不透明。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ViewId(String);
```

- `RepoPath` 的 `TryFrom` 会把 NFD 规整为 NFC，与 macOS 上 git 的 `core.precomposeunicode` 一致。从 git tree 或 index 读到的路径字节如果不是 UTF-8，返回 `Code::NotText`。
- `TreeOid` 与 `CommitOid` 只接受 40 位，所以不支持 SHA-256 对象格式的仓库，遇到时返回 `Code::Unsupported`。
- 每个标识都实现 `Display`，输出与 JSON 里相同的字符串；`CheckpointId` 输出数字。

## 4. 结构

```rust
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::SystemTime;
use serde::{Deserialize, Serialize};

/// 基线 generation（design §4）：一个 repo 在某个 tree 上的不可变快照，即源码树加上与之匹配的依赖集。
/// 三个字段合起来就是它的键。它永不原地修改，新的基线就是新的 generation（不变量 5）。
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BaseGen {
    pub repo: RepoName,
    pub tree: TreeOid,
    pub deps: DepsSetId,
}

/// 依赖集（design §4）：一个 repo 的依赖目录按某组安装输入装好之后，封存在 Store 里的快照。
/// design §4 的键 (repo, 安装输入哈希, 平台, 工具链) 就是这里的 (repo, id)。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepsSet {
    pub id: DepsSetId,
    pub repo: RepoName,
    /// 安装输入的 BLAKE3，64 位小写十六进制。安装输入由 config.md 的 `install_inputs` 规定，
    /// 例如 package.json、被 git 跟踪的锁文件、解析出的 `@inter/*` SHA。
    /// 漂移的定义：工作区算出的这个值与所钉依赖集的不同。
    pub inputs_hash: String,
    /// 平台三元组，如 `aarch64-apple-darwin`。
    pub platform: String,
    /// 工具链版本，如 `bun 1.3.1`；怎么采集由 deps.md 规定。
    pub toolchain: String,
    /// 依赖目录，取自 config.md 的 `deps_dirs`，如 `node_modules`、`vendor`。
    pub dirs: Vec<RepoPath>,
}

/// 基线清单（design §6.2）：一个基线 tree 里的全部文件，只记内容。每个 tree 一份，钉住同一 tree 的会话共用。
/// gen count、inode、mtime 这类每个工作区各不相同的属性不在清单里，它们是 overlay 的本地缓存（见 overlay.md）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseManifest {
    pub tree: TreeOid,
    pub entries: BTreeMap<RepoPath, ManifestEntry>,
}

/// 基线清单里的一个文件。工作区里同一路径文件的 blob 和 mode 都与它相同时，该文件不进 overlay。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    /// 文件内容的 BLAKE3，与 OverlayEntry 里的 blob 是同一种地址，相等即内容相同。
    pub blob: BlobId,
    /// git 的文件模式，取值同 OverlayEntry 的 mode。
    pub mode: u32,
    /// 字节数。
    pub size: u64,
}

/// overlay 中的一条：工作区相对它钉住的基线的一个整文件差异（design §4）。
/// 与基线完全相同的文件不在 overlay 里，改回基线内容就等于撤销覆盖。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlayEntry {
    pub path: RepoPath,
    #[serde(flatten)]
    pub kind: EntryKind,
    /// git 的文件模式：0o100644、0o100755，或 0o120000（符号链接）；Deleted 时为 0。
    pub mode: u32,
}

/// overlay 条目的种类：design §4 的增、改、删、改名、未跟踪且合格。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum EntryKind {
    /// 基线里没有、工作区的 git 索引里有（会话 `git add` 过或在本地 commit 过）。已跟踪的文件永不被 ignore。
    Added { blob: BlobId },
    /// 基线里有，内容或 mode 变了。
    Modified { blob: BlobId },
    /// 基线里有、工作区里没了：这是墓碑，遮住基线的同名条目。
    Deleted,
    /// 基线里的 `from` 改名成了 `path`，内容可能也改了。这一条同时就是 `from` 的墓碑，
    /// `from` 不再另记 Deleted。只改大小写的改名也记成它；怎样认定改名由 overlay.md 规定。
    Renamed { from: RepoPath, blob: BlobId },
    /// 基线里没有、索引里也没有、又没被 gitignore 的文件，即"未跟踪且合格"，只属于本工作区。
    Untracked { blob: BlobId },
}

/// checkpoint（design §4、§6.2）：某一时刻一个工作区的完整 overlay，是恢复的单位。
/// entries 是完整快照，不是相对上一个 checkpoint 的增量。一次工具调用碰了两个 repo，就记两个 checkpoint。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub id: CheckpointId,
    pub session: SessionId,
    pub repo: RepoName,
    /// JSON 里是 RFC 3339 UTC 字符串（§2.2）。
    pub at: SystemTime,
    pub reason: CheckpointReason,
    pub entries: Vec<OverlayEntry>,
}

/// 记这个 checkpoint 的原因（design §6.2）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum CheckpointReason {
    /// Claude Code 的 PostToolUse 钩子调 `git-strata checkpoint --tool-call <id>`，id 是工具调用 id。
    /// 钩子可能重放，同一会话、同一 repo 的同一个 id 只记一次。
    ToolCall { id: String },
    /// `git-strata checkpoint -m <message>`。
    Manual { message: String },
    /// 发布组合成 staging 树之前（design §6.5）。
    BeforePublish,
    /// restore 之前，让恢复本身也可以恢复（不变量 1）。
    BeforeRestore,
    /// `session::move_workspace` 交换基线与依赖之前；`deps approve` 和发布后 rebase 都经过它。
    BeforeRollout,
    /// `rm` 删除会话之前；这个 checkpoint 按保留期留存（design §6.6）。
    BeforeRemove,
}

/// 批次（design §4、§6.5）：从一个会话的一个 repo 挑出的整文件路径，内容取自 checkpoint 那一刻。
/// paths 里的每个路径都必须在该 checkpoint 的 overlay 里。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Batch {
    pub session: SessionId,
    pub repo: RepoName,
    pub checkpoint: CheckpointId,
    pub paths: Vec<RepoPath>,
}

/// 发布组（design §4、§6.5）：一次发布的输入，批次可来自多个会话、跨多个 repo，每个 repo 各出一个 commit。
/// 同一 repo 的批次按 batches 里的顺序依次做三方合并。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseGroup {
    /// 由 Store 分配的序号。
    pub id: u64,
    pub batches: Vec<Batch>,
}

/// 发布目标（design §4、§6.7）：每个 repo 一个，即 config.md 里的 `publish = { branch, push, confirm }`。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishTarget {
    /// 目标分支；对应的发布链是 `refs/strata/publish/<branch>`。
    pub branch: String,
    /// true：发布后立刻推到 `origin/<branch>`；false：留在本地，等 `git-strata push <repo>`。
    pub push: bool,
    /// 推送前是否要人类确认（design §6.9）。`deploy_on_push = true` 的仓库不论此值都要确认，
    /// 因为配置不能绕过确认（不变量 7）。
    pub confirm: bool,
}

/// 一次 `git push` 的结果：gitio 的 `push` 产生，publish 消费。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "message")]
pub enum PushOutcome {
    /// 远端接受了推送，发布链与 `origin/<分支>` 一致。
    Pushed,
    /// 远端明确拒绝（非快进、权限、服务端钩子），带远端的原话；publish 把它转成 Code::PushRefused。
    Refused(String),
    /// 超时或连接中断，远端可能已经收到；publish 把它转成 Code::Unknown，保留发布链、不重放（不变量 6）。
    Unknown,
}

/// 会话清单（design §4、§6.1）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionManifest {
    pub id: SessionId,
    /// ticket 只是标签，一个 ticket 可以有多个会话。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket: Option<String>,
    /// 会话根目录，各工作区按兄弟布局排在它下面。
    pub root: PathBuf,
    /// 配置里的每个兄弟仓库都在这里，不只是 `--repos` 点名的那些（design §6.1）。
    pub repos: BTreeMap<RepoName, WorkspaceState>,
}

/// 会话里一个 repo 的工作区。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceState {
    /// 钉住的基线。base.deps 就是克隆进这个工作区的依赖集，两者只会一起移动（不变量 8）。
    pub base: BaseGen,
    /// 工作区目录，即 root/<repo>。
    pub path: PathBuf,
    /// true：`--repos` 点名的仓库，被监听、有 overlay、可以发布。
    /// false：只钉住基线，没有 overlay，发布时跳过（design §6.1 第 3 步）。
    pub tracked: bool,
}
```

- design §4 把"工具链版本是否入键"交给构建者。本阶段的计划已经定为入键（见 `DepsSetId` 的定义），deps.md 只决定怎么采集 `toolchain`。
- design §4 说清单还记录创建时间和状态。这两项不放在 `SessionManifest` 里：由 store.md 的 `sessions` 表保存，由 session.md 的 `status` 报出。

一个 `Checkpoint` 序列化之后的样子（实际输出是单行，这里为了好读拆成多行，哈希也截短了）：

```text
{
  "id": 7,
  "session": "s-k3q7xm2a",
  "repo": "tool",
  "at": "2026-10-22T18:04:11Z",
  "reason": { "kind": "ToolCall", "id": "toolu_01AbCd" },
  "entries": [
    { "path": "src/app/page.tsx", "kind": "Modified", "blob": "9f2c…", "mode": 33188 },
    { "path": "src/legacy.ts", "kind": "Deleted", "mode": 0 },
    { "path": "src/foo.ts", "kind": "Renamed", "from": "src/Foo.ts", "blob": "41d0…", "mode": 33188 }
  ]
}
```

人类确认（design §6.9）用的凭据也放在 `strata-types` 里，是纯数据：

```rust
use std::time::Instant;
use serde::{Deserialize, Serialize};

/// 一次人类确认的凭据（design §6.9）。它是纯数据，放在 strata-types 里；只由 strata-confirm 在
/// LocalAuthentication 通过之后签发，并由 strata-confirm 校验，只存在于 daemon 的内存里。
/// 它不实现 Clone、Copy、Serialize、Deserialize：消费它的函数按值接收，用一次就被移走；
/// 它不会出现在 RPC、JSON 或磁盘上，所以客户端无从伪造或重放。审计记录只存 action 和结果，从不存凭据本身。
#[derive(Debug)]
pub struct ConfirmToken {
    /// 被确认的动作。消费方要核对它与本次要执行的操作完全相同。
    pub action: Action,
    /// 签发时刻（单调时钟），用来判断是否过期；过期时长由 confirm.md 规定。
    pub issued: Instant,
    /// 128 bit 随机数。strata-confirm 签发时登记、校验通过时核销，同一个 nonce 只能用一次。
    pub nonce: u128,
}

/// 需要人类确认的动作，即 design §6.9 列出的三种。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Action {
    /// 推送一个需要确认的发布目标：`deploy_on_push = true` 的仓库，或 `PublishTarget.confirm = true`。
    PushDeploy { repo: RepoName, branch: String },
    /// `deps approve <proposal>`：批准一个依赖变更提议。
    DepsApprove { proposal: ProposalId },
    /// `rm --discard`：丢掉会话的未发布改动，再删除会话。
    RemoveDiscard { session: SessionId },
}
```

- **签发与校验**：只有 `strata-confirm` 签发和校验凭据（见 confirm.md），crate 关系见 §8。除了 `strata-confirm` 和测试，任何代码都不构造 `ConfirmToken`。
- **动作从哪来**：daemon 根据请求要执行的操作推出 `Action`，从不采用客户端传来的 `Action`。每个需要确认的动作各弹一次认证框。daemon 把凭据交给消费方之前，先经 `strata-confirm` 校验它没用过、没过期，并核销它的 nonce。
- **谁消费**：
  - `PushDeploy` 的凭据交给 `publish::push`，`RemoveDiscard` 的凭据交给 `session::remove`。这两个函数按值收下凭据并核对 `action`，不符时返回 `Code::NotConfirmed`。
  - `DepsApprove` 由 daemon 处理 `deps approve` 时校验一次，然后对每个目标会话调用 `session::move_workspace`。`move_workspace` 不收凭据，因为发布后 rebase 也会调它。

## 5. 配置结构

`Config`（全局配置）与 `RepoConfig`（每个仓库一张 `[repo.<name>]` 表）定义在 [config.md](config.md)。它们放在 `strata-types` 里，是纯数据、不做 I/O；`RepoConfig` 的 `publish` 字段类型是 `PublishTarget`。本文不重复它们的字段。

## 6. 错误

### 6.1 定义

```rust
use serde::{Deserialize, Serialize};

/// 一次被拒绝的调用。code 稳定，单独决定语义；message 是给人看的（同 LayerFS 的 LayerFSError）。
/// 实现 std::error::Error，Display 输出 "<code>: <message>"。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrataError {
    pub code: Code,
    pub message: String,
    /// 结构化细节，如冲突文件、打开文件的进程。形状由返回它的模块文档规定。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
}

/// 错误码，序列化为变体名字符串，如 "Conflict"。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Code {
    // LayerFS sdk/js/src/errors.js 的 CODES，去掉 NoSuchProject、MountsActive、NotMounted、NoSuchSandbox、NoSuchProcess
    Conflict, HeadMoved, Dirty, Busy, NoSuchPath, NoSuchBranch, NoSuchCommit,
    NoCommit, NoMergeBase, IsDirectory, NotDirectory, NotFile, Binary, NotText,
    NoMatch, AmbiguousMatch, Exists, NotEmpty, NameTaken, ReservedName, ReadOnly,
    TooLarge, InvalidInput,
    // git-strata 新增
    NoSuchSession, NoSuchRepo, NoSuchCheckpoint, NoSuchProposal, NoSuchView,
    NotConfirmed, NotTty, OpenFiles,
    CrossVolume, Drift, CheckFailed, CheckMutates, PushRefused, Unknown, Unsupported,
    Corrupt,
}
```

从 LayerFS 词汇表去掉的五个码在 git-strata 里没有对应物：git-strata 没有 Project（仓库不存在时用 `NoSuchRepo`），没有挂载，也没有 Sandbox 和 exec（命令由会话自己直接跑）。

### 6.2 错误码

"何时返回"写触发条件；"谁返回"写最先产生这个码的模块，"透传"表示上层模块原样交给调用方。

| 码 | 何时返回 | 谁返回 |
| --- | --- | --- |
| `Conflict` | 合成 staging 树时，同一文件的同一 hunk 被两边都改了（design §6.5 第 3 步）。整个发布组停下，什么都不提交；`detail` 列出冲突文件、hunk 和涉及的会话，用 `publish --assign` 指定由谁解决 | `publish` |
| `HeadMoved` | 比较并交换失败：要前进的 `refs/strata/publish/<分支>` 已经不是本次读取时的值，说明另一次发布先前进了发布链。不重新合并、不重试；`detail` 带期望值与实际值 | `gitio`（`update_ref`），`publish` 透传 |
| `Dirty` | `rm` 的会话还有未发布的改动（某个工作区的 overlay 非空），又没带 `--discard`（design §6.6） | `session` |
| `Busy` | 同一 repo 的写锁（design §8）被另一个操作持有，在等待上限内没拿到 | `daemon` |
| `NoSuchPath` | 点名的路径不存在：`restore` 的路径在目标 checkpoint 和基线里都没有；查询命令限定的路径在目标快照里不存在；配置里主 checkout 的 `path` 不存在 | `overlay`、`search`、`session` |
| `NoSuchBranch` | fetch 之后，`origin/<default_branch>` 或发布目标分支仍然不存在，也没有对应的发布链 | `gitio`，`session` 与 `publish` 透传 |
| `NoSuchCommit` | 要用的 commit 或 tree 不在 object store 里，例如钉住的基线被主 checkout 的 `git gc` 清掉，或发布记录里的 commit 已经不存在 | `gitio` |
| `NoCommit` | 没有可提交或可推送的东西：批次合成后与合并基线 B 完全相同；或 `push <repo>` 时发布链并不领先 `origin/<分支>` | `publish` |
| `NoMergeBase` | 会话钉住的基线 commit 不在目标历史里（上游被强推改写过），三方合并找不到可信的共同祖先；硬合会把上游丢掉的改动当成对方的撤销。发布、`diff --between`、`move_workspace` 都拒绝 | `publish`、`session` |
| `IsDirectory` | 要文件的地方给了目录：`apfs::clone_file` 或 `store::put_blob` 的源是目录；`publish --assign <file>` 指向目录 | `apfs`、`store`、`publish` |
| `NotDirectory` | 要目录的地方给了别的东西：`apfs::clone_tree` 的源、配置里主 checkout 的 `path`、`[paths] root`、`deps_dirs` 的条目不是目录 | `apfs`、`session`、`deps` |
| `NotFile` | 要复制或哈希的东西既不是普通文件、符号链接，也不是目录（FIFO、socket、设备文件）：`apfs::clone_file` 的源，或种子文件、安装输入里的条目 | `apfs`、`session`、`deps` |
| `Binary` | 要按行处理的文件是二进制（按 git 的判定：前 8000 字节里有 NUL）。按条目返回：`diff` 和三方合并预览里该文件的结果是这个码，不给 hunk。发布时两边都改了的二进制文件按整文件冲突报 `Conflict` | `publish` |
| `NotText` | 应当是 UTF-8 的路径名不是：git tree 或 index 里来自其他平台的非 UTF-8 文件名，既成不了 `RepoPath`，也写不到 APFS 上 | `types`（构造 `RepoPath`）、`gitio` |
| `NoMatch` | `publish --from <session>:<path>` 的路径在该会话的 overlay 里没有任何条目，即这个路径没有可发布的改动 | `publish` |
| `AmbiguousMatch` | 会话选择器给的是 ticket，而这个 ticket 有不止一个会话（design §4）。`detail` 列出候选的会话 id | `session` |
| `Exists` | 要创建的目标已经存在：`clonefile` 的目标已存在（`EEXIST`）；或 `new` 要建的会话目录已存在 | `apfs`、`session` |
| `NotEmpty` | 第一次在 `[paths] root` 下建 Store 时，该目录已经存在、不是空的，又不是 strata 的布局 | `store` |
| `NameTaken` | 两张 `[repo.<name>]` 表解析到同一个 git common dir，即同一个 `RepoId` 被两个 `RepoName` 占用。加载配置时拒绝 | `daemon` |
| `ReservedName` | ticket 的形式和会话 id 一样（`s-` + 8 位 base32），会与 `SessionId` 混淆 | `session` |
| `ReadOnly` | 写被拒绝：daemon 不在时 CLI 只读，写命令直接拒绝（design §5）；或者要写的是已封存的基线或依赖集目录（不变量 5） | `cli`；写封存目录的模块（按 §6.3 归类） |
| `TooLarge` | 超出上限：要读内容的操作遇到超过大文件阈值（默认 64 MiB，design §7）的文件，按条目返回，不读内容；或一条 RPC 消息超过 daemon 的帧上限 | `publish`、`search`、`daemon` |
| `InvalidInput` | 参数或配置值格式不对：标识格式错、`RepoPath` 是绝对路径或含空段或 `..`、选择器语法错、配置字段的类型或取值不对 | `types`（构造与解析）、`cli`、`daemon` |
| `NoSuchSession` | 会话选择器（id 或 ticket）对不上任何会话；或命令默认作用于"本会话"，而 cwd 不在任何工作区里 | `session` |
| `NoSuchRepo` | 名字不在配置的 `[repo.<name>]` 里，比如 `--repos`、批次或 `push <repo>` 里给的名字 | `session`、`publish`、`deps` |
| `NoSuchCheckpoint` | 该会话里没有这个 `CheckpointId`：从来没有过，或者已被 gc 回收 | `store`，`overlay` 与 `publish` 透传 |
| `NoSuchProposal` | `deps approve` 给的 `ProposalId` 不是待确认的提议：这个提议从来没有过，或者已经不在待确认状态 | `deps`，`daemon` 透传 |
| `NoSuchView` | 给的 `ViewId` 不是已挂上的检索视图：从来没挂上过，或者已经 detach | `search`，`daemon` 透传 |
| `NotConfirmed` | 需要人类确认的动作（一推上线仓库的推送、`deps approve`、`rm --discard`）没有通过确认：LocalAuthentication 认证失败、被取消或超时；或者交来的 `ConfirmToken` 已经用过、已经过期，或它的 `action` 与要执行的操作不符（一次一用，design §6.9） | `confirm`（签发与校验）；`session`、`publish`（核对 `action`）；`daemon` 透传 |
| `NotTty` | 需要人类确认的命令，stdin 不是 TTY。在弹认证框之前就拒绝（design §6.9） | `cli`、`confirm` |
| `OpenFiles` | 要替换或删除的目录里有进程打开着文件：`move_workspace` 要换依赖目录时，或 `rm` 时会话目录被占用（dev server 等）。跳过，目录一字不动；`detail` 列出 pid 和路径 | `session` |
| `CrossVolume` | 会话根目录、Store、基线与依赖集缓存跟主 checkout 不在同一个 APFS 卷上（design §7 要求同卷）。在克隆之前就拒绝，从不退化成复制 | `apfs`（`clone_tree`、`clone_file`）、`store`（`open`）、`session`（`new`） |
| `Drift` | 安装输入哈希对不上：`deps::adopt` 认领主 checkout 现成的依赖目录时，主 checkout 的安装输入与目标基线不符，调用方改为冷装。`status` 报告的工作区漂移是数据，不是这个错误 | `deps` |
| `CheckFailed` | 发布时，检查命令在 staging 克隆里以非零状态退出。不提交；`detail` 带命令、退出码和输出末尾 | `publish` |
| `CheckMutates` | 配置的检查命令会改文件：含 `--write` 或 `cs-fix` 而没有 `--dry-run`，规则见 config.md。这是配置错误，在任何检查运行之前就拒绝（design §6.5 第 4 步） | `publish`（`check`） |
| `PushRefused` | 远端明确拒绝了推送（非快进、权限、服务端钩子），即 gitio 的 `push` 返回了 `PushOutcome` 的 `Refused`。发布链留在本地，没有推上去；`detail` 带远端的原话 | `publish`（由 `PushOutcome` 转换） |
| `Unknown` | 结果不确定（推送超时或连接中断，即 `PushOutcome` 的 `Unknown`，远端可能已经收到；或写到一半被打断），或者失败归不到任何别的码。一律按不确定处理：保留现场、不重放、不清理，`status` 报出（不变量 6）；daemon 重启后同样报出、不重放 | `publish`（推送结果不确定时）、`gitio`、`daemon`；任何模块按 §6.3 归类后也可能返回 |
| `Unsupported` | 环境缺少需要的能力：卷不是 APFS 或不支持 `clonefile`（`ENOTSUP`）；仓库使用 SHA-256 对象格式（`TreeOid` 只接受 40 位）；没有 GUI 会话，LocalAuthentication 弹不出来 | `apfs`、`gitio`、`confirm` |
| `Corrupt` | Store 里的数据坏了：读 blob 时，字节的 BLAKE3 与它的 `BlobId` 不符；写入 blob 时长度对不上；打开 Store 时 SQLite 的 schema 或日志读不出来 | `store` |

### 6.3 I/O 错误的归类

模块把 `std::io::Error` 转成 `StrataError` 时按 errno 归类，`detail` 里带上 `errno` 和路径：

| errno | 码 |
| --- | --- |
| `ENOENT` | `NoSuchPath` |
| `EEXIST` | `Exists` |
| `ENOTDIR` | `NotDirectory` |
| `EISDIR` | `IsDirectory` |
| `ENOTEMPTY` | `NotEmpty` |
| `EXDEV` | `CrossVolume` |
| `ENOTSUP` | `Unsupported` |
| `ENAMETOOLONG`（APFS 文件名超过 255 字节，或路径超过 macOS 的 `PATH_MAX` 1024 字节） | `InvalidInput` |
| 写操作中的 `EROFS`、`EPERM`、`EACCES` | `ReadOnly` |
| 其他（`ENOSPC`、`EIO` 等） | `Unknown` |

如果失败发生在写到一半的时候，也就是目录或数据已经被改了一部分，那么不论 errno 是什么都返回 `Unknown`，按不变量 6 处理。

## 7. JSON 输出约定

`git-strata` 的每个子命令都在 stdout 上恰好输出一个 JSON 文档，单行、紧凑，信封固定是下面两种之一：

```text
{"ok":true,"data":<该子命令的结果>}
{"ok":false,"error":{"code":"Conflict","message":"…","detail":<结构化细节>}}
```

- `ok` 为 `true` 时有 `data`、没有 `error`；为 `false` 时有 `error`（一个 `StrataError`）、没有 `data`。没有结果可返回的命令输出 `"data":{}`。
- `data` 的形状由 cli.md 按子命令规定，里面的类型按 §2.2 序列化。
- 按条目返回的码（`Binary`、`TooLarge`）不会让整个调用失败。它们出现在 `data` 里对应的条目上，具体形状由 cli.md 规定。
- 程序只按 `error.code` 分支；`message` 是给人看的，措辞可以改。
- 给人看的提示（例如有待确认的依赖变更，design §6.4 第 4 步）只写到 stderr，不进 JSON。
- 退出码和 Claude Code 钩子的行为由 cli.md 规定。daemon 的 RPC 回复是同一个信封再加上 `id`，见 daemon.md。

## 8. Crate 布局与依赖方向

将来的 Cargo 布局如下，本阶段不创建：

```text
Cargo.toml                       workspace, edition 2024, rust-version 1.88
crates/strata-types              ids、错误、配置结构（无 I/O）
crates/strata-apfs               clonefile / getattrlistbulk / gen count / FSEvents
crates/strata-gitio              git 子进程封装、.git/index 解析、diff3
crates/strata-store              SQLite 元数据 + blob
crates/strata-overlay            overlay 计算、checkpoint、restore
crates/strata-deps               依赖集 generation、认领、rollout 计划
crates/strata-session            清单、兄弟布局、cwd 识别、move_workspace
crates/strata-publish            批次、发布组、staging、发布链、发布后 rebase
crates/strata-search             tgrep-core 集成
crates/strata-confirm            LocalAuthentication + TTY
crates/strata-daemon             bin git-stratad：launchd、RPC、调度
crates/git-strata                bin git-strata：CLI
```

模块名与 crate 的对应：`cli` 是 `crates/git-strata`（bin `git-strata`），`daemon` 是 `crates/strata-daemon`（bin `git-stratad`），其余模块 `<m>` 都是 `crates/strata-<m>`。

依赖方向（`A ← B` 读作"B 依赖 A"）：

```text
types ← apfs, gitio, store ← overlay, deps ← session ← publish, search ← daemon ← cli
confirm 只被 daemon 用
```

按层展开如下。每个 crate 只能依赖比自己低的层；同层之间不能互相依赖，只有表里写明的例外；任何依赖都不能反向，也不能成环。

| 层 | crate | 可以依赖 |
| --- | --- | --- |
| 0 | `strata-types` | 无 workspace crate |
| 1 | `strata-apfs`、`strata-gitio`、`strata-store` | 第 0 层。同层只允许 store 依赖 apfs，因为 blob 用 `apfs::clone_file` 写入 |
| 2 | `strata-overlay`、`strata-deps` | 第 0–1 层；二者互不依赖 |
| 3 | `strata-session` | 第 0–2 层 |
| 4 | `strata-publish`、`strata-search` | 第 0–3 层；二者互不依赖。外部的 `tgrep-core` 只给 search 用 |
| 5 | `strata-daemon` | 第 0–4 层，外加 `strata-confirm` |
| 6 | `git-strata` | 第 0–5 层，但不含 `strata-confirm` |
| — | `strata-confirm` | 只依赖第 0 层；只有 `strata-daemon` 依赖它，人类确认在 daemon 里发生（design §6.9） |

`strata-confirm` 只负责签发和校验凭据（LocalAuthentication + TTY）。"confirm 只被 daemon 用"在 crate 层面成立：session 与 publish 只使用数据类型 `ConfirmToken` 和 `Action`，这两个类型放在 `strata-types` 里（§4）。

基线与依赖一起移动（不变量 8）的原子动作是 `session::move_workspace(session, repo, new_base: BaseGen)`。daemon 处理 `deps approve` 时调它，`publish::rebase_session` 做发布后 rebase 时也调它。发布后的新基线如果需要另一个依赖集，`rebase_session` 不移动这个会话，而是产生一个依赖变更提议，等 `deps approve`（不变量 4 优先于 design §6.4 的最后一句）。`deps` 只计算依赖集、提议和 rollout 计划，不调用 `session` 或 `publish`，依赖方向也不允许它这样做。
