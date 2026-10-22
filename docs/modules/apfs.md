# `apfs` 模块

**Crate**：`crates/strata-apfs` · **设计来源**：design.md §3（平台、symlink 陷阱、大小写）、§5（`apfs` 行）、§6.1（克隆）、§6.2（判变与监听）、§7（同卷）、§8（验收数字）、§9 不变量 5、6 · **矩阵**：layerfs-project-telemetry（3b）目标为 `apfs` 的 2 行，layerfs-workspace-layer（3c）3 行，layerfs-replaced（3d）1 行，layerfs-sdk-js（3e）1 行，tgrep-core（3f）18 行，共 25 行，以及理由点名 apfs 的 `—` 行 8 行、理由写"归类同上一行"的 1 行（§7 表后）

apfs 是最底层的 crate，只管 macOS 文件系统的原语，不知道会话、基线、overlay 这些概念：克隆目录和文件，在根之下不跟随符号链接地读与写，用 `getattrlistbulk` 批量读属性（带 gen count），把 FSEvents 的信号翻译成提示，以及 APFS 的名字规则。其余模块的判变、对账和现场处理都建在这些原语的**契约**之上，所以本文把每个原语的失败方式和边界写得很细。本文按 module-rulings.md 的 "T4 apfs" 一节、增补 1 的 gen count 条目、增补 3 的 "Rooted destinations" 条目写成。文中标"实测"的行为，都是 2026-10-22 在本机（macOS 27.0.1，Darwin 27.0.0，arm64）上用临时文件和 `hdiutil` 稀疏镜像（不需要 root）验证过的；计时用到的真实仓库只做了只读遍历，没有写。本模块的类型只在进程内传递（`Attrs` 进缓存，`WatchEvent` 进回调），不经 RPC 或 JSON，所以不 derive `Serialize`/`Deserialize`。

## 1. 职责与边界

**负责**（design §5 的 `apfs` 行，加上裁定 T4 与增补 3 的两条）：

1. **克隆**（design §6.1、§7）：`clone_tree` 克隆目录树，`clone_file` 克隆文件或符号链接，都是 `clonefile(2)`，同卷检查在先，从不退化成复制（§5.2、§5.3）。
2. **在根之下不跟随符号链接地读与写**（裁定 T4、增补 3）：`open_rooted`、`open_rooted_file` 逐级 `openat(O_NOFOLLOW)` 打开；写的一侧是目的地变体 `clone_file_rooted`、`clone_tree_rooted`、`create_file_rooted`、`mkdir_rooted`、`symlink_rooted`（§5.4）。
3. **遍历与属性**（design §6.2、§8）：`walk` / `walk_with` 用 `getattrlistbulk` 前序遍历，`attrs_of` 点查，`Attrs` 带纳秒时间、inode 和 gen count，`Attrs::unchanged_since` 把"没被写过"的判法固定下来（§4.2、§5.5、§5.6）。
4. **监听**（design §6.2）：`watch` 把 FSEvents 的原始信号翻译成 `WatchEvent`：可能脏的路径、该重扫的子树、事件丢了。它只是提示，从不声称完整（裁定 T4，§5.7）。
5. **卷与名字规则**（design §3 的大小写行、§7）：`same_volume`、`volume_caps` 回答"clonefile 在这两处之间行不行、这个卷分不分大小写"；`case_fold_eq` 是 APFS 大小写不敏感卷上的名字同一性（§5.1、§5.8）。

**不负责**：

| 事项 | 归谁 |
| --- | --- |
| 哪些目录和路径不进遍历（`.git`、`deps_dirs`、`seed_caches`、ignore 规则）；遍历结果怎样变成 overlay 条目；改名配对、墓碑 | overlay（`filter` 闭包由调用方给，apfs 只负责剪枝） |
| 什么时候相信缓存、什么时候读内容算哈希；全量对账的节奏；监听丢失后怎样退化 | overlay（脏集合）与 daemon（调度）；apfs 只给 `Attrs::unchanged_since` 这一条判法和 `WatchEvent` |
| 调用 `same_volume`、`volume_caps` 做预检的时机（`Store::open`、`new`），以及预检失败怎样上报 | store、session；apfs 只回答问题 |
| 内容哈希、blob 的 xattr 剥离、`chflags`、`fsync` / `F_FULLFSYNC` | store（blob 规整见 store.md）与各调用方；apfs 不做持久化，克隆之后的落盘归调用方 |
| 递归删除目录（`unlinkat` 自底向上、清 `UF_IMMUTABLE`） | session、store、deps（清 staging 树）：它们用 `open_rooted` 取目录句柄后自己 `unlinkat` |
| 克隆之后清除 BSD 标志与扩展属性、重设 mtime | store（`put_blob` 的规整，store.md §5.3 第 5 步）与 overlay（投影）；apfs 的克隆原样把它们带过来 |
| 找出哪些进程打开着哪些文件（`OpenFiles`） | deps（`open_files`）与 session。apfs 只负责一件相关的事：报告 gen count 不可得（§4.2 C） |
| 路径字符串的语法、NFC 规整、`RepoPath` 的构造 | types（`RepoPath::try_from`） |
| 清理与回滚 | 谁也不做：apfs 失败时从不删除、从不撤销已经发生的写入，现场原样留给调用方（不变量 6，§4.3） |

## 2. 依赖

Workspace crate（types.md §8：apfs 在第 1 层，只依赖第 0 层；同层只有 store 依赖 apfs）：

| 依赖 | 用途 |
| --- | --- |
| `strata-types` | `RepoPath`（遍历与点查的路径形态）、`StrataError`、`Code` |

外部 crate（版本和选型交给构建者，这里只写理由）：

| 依赖 | 理由 |
| --- | --- |
| `libc` | design §11 第 2 条：macOS API 走 libc。用到 `clonefile`、`clonefileat`、`getattrlist`、`getattrlistat`、`fgetattrlist`、`getattrlistbulk`、`openat`、`mkdirat`、`symlinkat`、`readlinkat`、`fstatat`、`fchmodat`、`statfs`，测试里另有 `setrlimit`、`mmap` |
| CoreServices 的 FSEvents 与 libdispatch 的绑定 | 直接绑 `FSEventStreamCreate` 一族加一条私有串行 dispatch queue，不经 `notify`：本模块必须拿到每个事件的原始 flags（`MustScanSubDirs` 与 `UserDropped`、`KernelDropped` 的区别，`RootChanged`），不要第三方抽象替它们归类。用哪个绑定 crate 还是手写 `extern "C"`，交给构建者（§9） |
| `unicode-normalization` | NFC（目录项名字进 `RepoPath`）与 NFD（`case_fold_eq`） |
| Unicode 全量大小写折叠表，版本须 ≥ 16.0 | `case_fold_eq`。实测 macOS 27 的 APFS 已经用上 Unicode 16 新增的折叠（§4.2 B）。用 `icu_casemap`、`caseless`，还是用 CaseFolding.txt 生成表，交给构建者（§9）；验收是 §7 的 oracle 测试 |
| 开发依赖 `tempfile` | 测试的临时目录。跨卷、不支持、满盘、大小写敏感、嵌套挂载的测试另用 `hdiutil create -type SPARSE` 建的镜像（`tests/common` 的 `Image`，§8 Task 1），挂载点放在临时目录里，`Drop` 时 `hdiutil detach` 并删镜像文件 |

谁调用本模块：

| 调用方 | 用到 |
| --- | --- |
| store | `clone_file`（blob 写入与读出，不跟随 `src` 的符号链接）、`same_volume`（`open` 的同卷检查）、`open_rooted`（gc 删目录时逐级取句柄） |
| deps | `clone_tree`（认领、staging、`clone_into`）、`clone_file`（安装输入进 staging）、`walk`（展开安装输入；认领前后的两次遍历，比较 `Attrs`）、`open_rooted`（读安装输入） |
| session | `clone_tree`、`clone_file`（工作区、依赖目录、种子缓存、种子文件）、目的地变体（增补 3：往工作区或 staging 树里写的每一处）、`same_volume`、`walk`（`move_workspace` 的携带遍历）、`open_rooted`（删除时逐级取句柄） |
| overlay | `walk` / `walk_with`、`Attrs`、`attrs_of`、`unchanged_since`、`watch` 与 `WatchEvent`、`case_fold_eq`、`open_rooted_file`、`clone_file` 与目的地变体（restore 的投影） |
| search | 读工作区文件的内容与属性：`open_rooted_file`、`attrs_of`、`walk`（具体以 search.md 为准） |
| daemon | 启停每个 tracked 工作区的 `Watcher`（由 overlay 的 `Tracker` 持有）；启动时用 `volume_caps` 预检 |

## 3. 公开接口（Rust）

跨模块类型只用 types.md 的（`RepoPath`、`StrataError`、`Code`）。`Attrs`、`FileKind`、`WatchEvent`、`VolumeCaps`、`WalkOptions`、`WatchOptions`、`Walk`、`Watcher` 是本模块自有的公开类型，定义在这里；别的模块按名字使用。

```rust
// crates/strata-apfs/src/lib.rs —— 签名清单（不是实现）
use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use strata_types::{RepoPath, StrataError};

/// types.md §8 的 `Result<T>`。
pub type Result<T> = std::result::Result<T, StrataError>;

/// APFS 的文件名上限：每个路径分量最多 255 个 UTF-16 码元（不是 255 字节，§4.2 B）。
pub const NAME_MAX_UTF16_UNITS: usize = 255;
/// macOS 的 PATH_MAX，含结尾的 NUL：路径达到 1024 字节就失败，1023 字节还行（§4.2 B）。
pub const PATH_MAX_BYTES: usize = 1024;
/// walk 允许的最大深度，从起点目录算。超出的子树以错误条目报告，也钉住 walk 同时持有的描述符数（§5.6）。
pub const MAX_WALK_DEPTH: u32 = 128;
/// getattrlistbulk 的缺省缓冲区，以及 `WalkOptions.buffer_bytes` 夹取的范围（§5.6）。
pub const DEFAULT_BULK_BUFFER_BYTES: usize = 64 * 1024;
pub const MIN_BULK_BUFFER_BYTES: usize = 1024;
pub const MAX_BULK_BUFFER_BYTES: usize = 4 * 1024 * 1024;
/// FSEvents 的 latency 参数（秒）；配合 NoDefer：第一个事件立即送出，其后的在 50 ms 内合并（§5.7）。
pub const WATCH_LATENCY_SECS: f64 = 0.05;

// ───────────── 卷 ─────────────

/// a、b 是否在同一个卷上，即 clonefile 能不能在两者之间工作。两个路径都跟随符号链接。
/// 用 getattrlist(ATTR_CMN_DEVID, FSOPT_RETURN_REALDEV) 比较，不用 st_dev：封存的系统卷和数据卷的 st_dev 相同（§5.1）。
/// 路径不存在 → io::ErrorKind::NotFound。
pub fn same_volume(a: &Path, b: &Path) -> io::Result<bool>;

/// 一个卷的能力。只读，不改任何东西。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumeCaps {
    /// statfs 的 f_fstypename：`apfs`、`hfs`、`msdos` 等。
    pub fs_type: String,
    /// VOL_CAP_INT_CLONE。Some(false) = 内核明确说不支持（msdos）；None = 内核没有声明（hfs），只有真的 clone 一次才知道。
    pub clone: Option<bool>,
    /// VOL_CAP_FMT_CASE_SENSITIVE。
    pub case_sensitive: bool,
    /// VOL_CAP_FMT_WRITE_GENERATION_COUNT。false 时 `Attrs.gen_count` 恒为 0（未知）。
    pub write_gen_count: bool,
}
pub fn volume_caps(path: &Path) -> Result<VolumeCaps>;

// ───────────── 克隆 ─────────────

/// 把目录树 src 克隆成 dst（dst 必须不存在）。同卷检查在先，目录用 clonefile(2) 一次性克隆，原子，从不退化成复制。
pub fn clone_tree(src: &Path, dst: &Path) -> Result<()>;
/// 把文件或符号链接 src 克隆成 dst（dst 必须不存在）。符号链接克隆成符号链接，从不跟随。
pub fn clone_file(src: &Path, dst: &Path) -> Result<()>;

// ───────────── 在根之下不跟随符号链接地读 ─────────────

/// 在 root 之下，按 path 逐级 openat(O_NOFOLLOW) 打开（只读）。每一级都不跟随符号链接；FIFO 以 O_NONBLOCK 打开，所以不会挂住。
/// 能打开普通文件、目录和 FIFO；调用方用 `File::metadata()` 核对类型。符号链接、socket、设备文件 → NotFile。
/// 目录的句柄可以当 dirfd 用（`as_raw_fd()` 传给 `unlinkat`、`openat`、`fchflags`）：store 的 gc、session 的 `rm`、deps 清 staging 树靠它做不跟随符号链接的递归删除，递归删除本身不在本 crate 里。
pub fn open_rooted(root: &Path, path: &RepoPath) -> Result<File>;
/// 同上，但只接受普通文件：打开后 fstat 确认，目录 → IsDirectory，FIFO、设备 → NotFile。读内容的路径用这个。
pub fn open_rooted_file(root: &Path, path: &RepoPath) -> Result<File>;

// ───────────── 在根之下不跟随符号链接地写（增补 3 的目的地变体） ─────────────
// dst 是 root 之下的相对路径：先逐级 openat(O_DIRECTORY|O_NOFOLLOW) 取得父目录的句柄，再用只带最后一个分量的 *at 调用落地。
// 父目录缺失 → NoSuchPath（需要时先 mkdir_rooted）；任何一级是符号链接 → NotDirectory，什么都不写。dst 已存在（含悬空符号链接）→ Exists，从不覆盖。

/// 把文件或符号链接 src 克隆到 root 之下的 dst。mode = Some(m) 时，克隆之后把权限位设成 m & 0o777（setuid、setgid、sticky 被屏蔽；用于 blob 的 0600 投影成 0644 / 0755）；对符号链接无效。
/// 克隆会带来源的 BSD 标志：源带 `uchg` 时克隆也带，`fchmodat` 会得 `EPERM`（实测）。要先清标志的调用方传 None，清掉之后自己设 mode。
pub fn clone_file_rooted(src: &Path, root: &Path, dst: &RepoPath, mode: Option<u32>) -> Result<()>;
/// 把目录树 src 克隆到 root 之下的 dst（clonefileat）。同卷检查在先，原子。
pub fn clone_tree_rooted(src: &Path, root: &Path, dst: &RepoPath) -> Result<()>;
/// 建一个新的空文件并返回可写句柄（O_WRONLY|O_CREAT|O_EXCL|O_NOFOLLOW）。mode 受 umask 影响；要精确的位，调用方对返回的句柄 fchmod。
pub fn create_file_rooted(root: &Path, dst: &RepoPath, mode: u32) -> Result<File>;
/// 建目录。parents = true 时像 `mkdir -p`：缺的父级逐级建出来，已有的目录跳过，叶子已是目录也算成功；
/// 叶子或中间一级是文件或符号链接 → 前者 Exists，后者 NotDirectory。parents = false 时叶子已存在一律 Exists。
pub fn mkdir_rooted(root: &Path, dst: &RepoPath, mode: u32, parents: bool) -> Result<()>;
/// 建符号链接，目标是 target 的原始字节（不要求 UTF-8，不能为空）。
pub fn symlink_rooted(root: &Path, dst: &RepoPath, target: &OsStr) -> Result<()>;

// ───────────── 属性、遍历 ─────────────

/// 对象类型。符号链接带着目标的原始字节。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileKind {
    File,
    Dir,
    Symlink { target: PathBuf },
    Fifo,
    Socket,
    CharDevice,
    BlockDevice,
}

/// 一个目录项（或点查）的属性，lstat 语义：符号链接就是链接本身。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attrs {
    pub kind: FileKind,
    /// 数据叉的逻辑长度（ATTR_FILE_DATALENGTH）；目录、FIFO、socket 为 0；符号链接是目标的字节数。
    pub size: u64,
    /// st_mode 的低 12 位：权限加 setuid / setgid / sticky。类型位在 `kind` 里。
    pub mode: u32,
    /// ATTR_CMN_GEN_COUNT。0 = 未知（卷不支持、内核没返回、或文件正被可写映射着，§4.2 C）。从不单独比较，用 `unchanged_since`。
    pub gen_count: u64,
    /// 纳秒精度。
    pub mtime: SystemTime,
    pub ctime: SystemTime,
    /// ATTR_CMN_FILEID，只在同一个卷内有意义。
    pub inode: u64,
}

impl Attrs {
    /// "自 earlier 以来没有被写过"的唯一正确判法（§4.2 C）。全部满足才为 true：kind 相同（符号链接连目标）、inode 相同、
    /// size 相同、mode 相同，且两边的 gen_count 都不是 0 并且相等。任何一边未知（0）都是 false，由调用方改用内容哈希。
    /// mtime、ctime 不参与：touch 不算改动，被还原的 mtime 也掩盖不了写入。
    /// 对目录，true 的意思只是"没有项被创建、删除、改名"，不代表子孙没变。
    pub fn unchanged_since(&self, earlier: &Attrs) -> bool;
}

/// 点查 root 之下 path 的属性。父目录逐级 O_NOFOLLOW 解析，最后一级是 lstat 语义。
pub fn attrs_of(root: &Path, path: &RepoPath) -> Result<Attrs>;

/// walk_with 的选项。Default：整棵树、深度只受 MAX_WALK_DEPTH 限制、缓冲区 DEFAULT_BULK_BUFFER_BYTES。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalkOptions {
    /// 从 root 之下的这个目录起遍历（逐级 O_NOFOLLOW 解析）；None = root 本身。产出的路径仍然相对 root。
    pub start: Option<RepoPath>,
    /// Some(n)：只产出 start 之下 n 层以内的条目；第 n 层的目录照样作为 Dir 产出，但不读它。Some(1) = 只列 start 的直接子项。
    pub max_depth: Option<u32>,
    /// getattrlistbulk 的缓冲区字节数，夹在 [MIN_BULK_BUFFER_BYTES, MAX_BULK_BUFFER_BYTES] 之间。测试用小值强制多次续读。
    pub buffer_bytes: usize,
}

/// walk 的迭代器，惰性：第一次 next() 才打开 root；root 打不开 → 第一项是 Err，然后结束。
pub struct Walk<'f> { /* 私有 */ }
impl Iterator for Walk<'_> {
    type Item = Result<(RepoPath, Attrs)>;
}

/// 前序遍历 root 之下的一切（不含 root 自己），不跟随符号链接，不进入挂载点，同一目录里的条目按 RepoPath 的字节序。
/// filter 对每个条目（文件、目录、符号链接）调用一次，参数是相对 root 的路径；返回 false = 丢掉这个条目和它的整棵子树（不产出，也不读）。
pub fn walk<'f>(root: &Path, filter: &'f dyn Fn(&Path) -> bool) -> Walk<'f>;
/// 同上，带选项，并且 filter 同时看到属性：ignore 规则要区分目录和文件，剪枝也可以按类型。
/// `walk(root, f)` 就是 `walk_with(root, &WalkOptions::default(), &|p, _| f(p))`。
pub fn walk_with<'f>(root: &Path, opts: &WalkOptions, filter: &'f dyn Fn(&Path, &Attrs) -> bool) -> Walk<'f>;

// ───────────── 监听 ─────────────

/// 监听事件。回调在 Watcher 私有的串行队列上被调用：不得阻塞，不得 panic，不得在回调里 drop 自己的 Watcher。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatchEvent {
    /// 这些绝对路径发生了变化（文件或目录；目录表示它的整棵子树都可能变了）。路径以传给 `watch` 的 root 为前缀，
    /// 其余部分是磁盘上的拼写（大小写、NFC 或 NFD 都可能与 RepoPath 不同）。一个路径可能已经不存在。
    Changed(Vec<PathBuf>),
    /// 内核把这个子树的事件合并了（MustScanSubDirs，没有丢弃标志，或 Mount / Unmount）：重扫这个子树。路径同上。
    MustRescan(PathBuf),
    /// 事件可能已经丢了（UserDropped、KernelDropped、EventIdsWrapped、RootChanged）：整个监听范围都当作未知，全量重扫。
    Dropped,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WatchOptions {
    /// root 之下不关心的目录（deps_dirs、seed_caches、.git）。落在这些目录之下的事件在回调里被丢掉，不产生 Changed。
    /// 注意：往这些目录里洪泛写入照样会让整个流报 Dropped（§4.2 F），所以消费者对 Dropped 的全量重扫也要排除它们。
    pub exclude: Vec<RepoPath>,
}

/// 监听句柄。drop 时停止流，并等正在进行的回调返回；drop 返回之后不会再有回调。
pub struct Watcher { /* 私有 */ }

pub fn watch(root: &Path, on_event: impl Fn(WatchEvent) + Send + 'static) -> Result<Watcher>;
pub fn watch_with(root: &Path, opts: &WatchOptions, on_event: impl Fn(WatchEvent) + Send + 'static) -> Result<Watcher>;

// ───────────── 名字规则 ─────────────

/// APFS 大小写不敏感卷上两个名字是不是同一个：Unicode 规范无大小写匹配（D145），
/// NFD(toCasefold(NFD(a))) == NFD(toCasefold(NFD(b)))，全量折叠，不含土耳其语映射。折叠表 ≥ Unicode 16.0。
/// 大小写敏感的卷（`VolumeCaps.case_sensitive`）不该调用它：那里 `a` 和 `A` 是两个文件，RepoPath 的字节相等就是同一个名字。
pub fn case_fold_eq(a: &str, b: &str) -> bool;

// ───────────── 测试接缝（只在 feature = "test-seams" 下编译，绝不进发布的二进制） ─────────────

#[cfg(feature = "test-seams")]
pub mod seams {
    use std::ffi::OsStr;

    /// 本线程里，在每一次逐级 `openat` 之前调用（open_rooted、open_rooted_file、各个目的地变体与 attrs_of 解析父目录的每一级，
    /// walk 打开子目录那一次，walk 解析 `start` 的每一级）。step 是 0 起的组件序号（0 = root 之下的第一级），name 是即将打开的名字。
    /// 回调里可以改文件系统：把目录换成符号链接、把文件换成 FIFO、删掉一个目录。线程局部，不影响别的线程里的测试。None = 清除。
    pub fn set_before_open(hook: Option<Box<dyn Fn(usize, &OsStr)>>);
    /// 本线程里，walk 每读完一次 getattrlistbulk（一个窗口）之后调用，参数是这个窗口里的记录数；用来在两个窗口之间改目录。
    pub fn set_after_bulk_window(hook: Option<Box<dyn Fn(usize)>>);
}
```

**与计划签名的差别**：

- 计划写的 `walk(root, filter) -> impl Iterator<Item = (RepoPath, Attrs)>` 在这里的条目带 `Result`，返回具名的 `Walk`：遍历会在单个条目上失败（目录不可读、名字不是 UTF-8、深度超限、根被换掉），吞掉它们会让调用方拿到一份看上去完整的残缺清单 [我的决定]。`filter` 的一参形态保留；要看属性的调用方用 `walk_with`。
- `Attrs` 比计划多一个 `mode`（overlay 要权限位，chmod 不改 gen count，§4.2 C），`kind` 的类型是 `FileKind`（符号链接带目标），不叫 `Kind` 是为了不与 types.md 的 `EntryKind` 混淆 [我的决定]。`gen_count` 保持计划的 `u64`，0 表示未知。
- `watch` 的签名保持；`watch_with` 加了 `exclude` [我的决定]。回调收到的 `WatchEvent` 就是计划的三个变体。
- 裁定 T4 加的 `open_rooted` 返回 `Result<File>`（计划里写的是 `-> File`）。
- 计划之外新增，各有调用方（§2 的表）：`open_rooted_file`；增补 3 要求的目的地变体 `clone_file_rooted`、`clone_tree_rooted`、`create_file_rooted`、`mkdir_rooted`、`symlink_rooted`（名字与参数是我定的 [我的决定]，裁定只说了"create/clone a file or tree at a path resolved component by component"）；`attrs_of`（std 的 `fs::metadata` 读不到 gen count）；`Attrs::unchanged_since`；`volume_caps`；`walk_with` 与 `WalkOptions`；`seams`。
- 没有 `fence` 或任何"等监听追上"的接口：实测哨兵文件做不成屏障（§4.2 F、§9）。

**`detail` 的形状**：I/O 失败一律带 `errno`（数字）和 `path`（字符串）（types.md §6.3）。apfs 另用这几个键：`reason`（短标识）、`dst_exists`（布尔：失败之后 dst 是否存在）、`src_dev` 与 `dst_dev`（`CrossVolume`）、`fs_type`（`Unsupported`）、`created`（`mkdir_rooted` 部分建好的目录）。`reason` 的取值：`trailing_slash`、`dst_inside_src`、`name_too_long`、`path_too_long`、`empty_target`、`symlink`、`not_a_directory`、`fifo`、`socket`、`device`、`too_deep`、`root_changed`、`unreadable_entry`。

**各函数的语义与可能返回的 `Code`**（"根"指 `root` 参数：它的祖先可以是符号链接，内核照常解析；它的最后一级用 `O_NOFOLLOW` 打开，所以根本身是符号链接 → `NotDirectory`，`reason = symlink`。调用方应传规范路径）：

- **`same_volume`**：见 §5.1。没有 `Code`，返回 `io::Result`；`store::open` 把 `false` 变成 `CrossVolume`。
- **`volume_caps`**：见 §5.1。`NoSuchPath`、`Unknown`。`fs_type` 不是 `apfs` 本身不是错误，由调用方决定。
- **`clone_tree`**：见 §5.2。`NoSuchPath`（src 不存在，或 dst 的父目录不存在）、`NotDirectory`（src 不是目录，符号链接也算；dst 的父级不是目录）、`Exists`（dst 已存在，含悬空符号链接，含 dst 就是 src）、`CrossVolume`（`detail` 带 `src_dev`、`dst_dev`）、`Unsupported`（卷不支持 clonefile，`detail.fs_type`）、`InvalidInput`（dst 以 `/` 结尾、dst 在 src 里面、分量超 255 个码元、路径达到 1024 字节）、`ReadOnly`（`EACCES`、`EPERM`、`EROFS`：dst 的父目录不可写，**或者源树里有不可读的条目**，message 同时写明两种可能）、`Unknown`（`ENOSPC`、`EIO`、`EDEADLK`、描述符耗尽等；`detail.dst_exists` 如实写）。失败之后 dst 不存在（clonefile 原子，实测）；apfs 不删除任何东西。
- **`clone_file`**：同 `clone_tree`，差别：src 是目录 → `IsDirectory`；src 是 FIFO、socket、设备文件 → `NotFile`（`reason` 取 `fifo`、`socket`、`device`）；src 不要求是目录。
- **`open_rooted`**：见 §5.4。`NoSuchPath`、`NotDirectory`（中间一级不是目录；是符号链接时 `reason = symlink`）、`NotFile`（最后一级是符号链接 `reason = symlink`、socket、设备文件）、`Unknown`（读取侧的 `EACCES`：types.md §6.3 的表里只有写操作的权限错误才是 `ReadOnly`，`detail.errno` 带原值）。
- **`open_rooted_file`**：同上，另加 `IsDirectory`、`NotFile`（FIFO：已经以非阻塞方式打开并关掉了，不会挂住）。
- **目的地变体**：见 §5.4。共同的：`NoSuchPath`（父目录缺失）、`NotDirectory`（中间一级是文件或符号链接，什么都没写）、`Exists`、`ReadOnly`（写侧的 `EACCES`、`EPERM`、`EROFS`）、`InvalidInput`（`ENAMETOOLONG`；`symlink_rooted` 的目标为空）、`Unknown`。`clone_file_rooted` 与 `clone_tree_rooted` 另有 `clone_file`、`clone_tree` 的源侧与卷的码（`IsDirectory`、`NotFile`、`NotDirectory`、`CrossVolume`、`Unsupported`）。**写入开始之后的失败一律 `Unknown`**（不变量 6）：`clone_file_rooted` 克隆成功而设 mode 失败，克隆出的文件留着（`dst_exists = true`）；`mkdir_rooted(parents = true)` 建了几级之后失败，已建的留着，`detail.created` 列出它们。
- **`attrs_of`**：见 §5.5。`NoSuchPath`、`NotDirectory`、`Unknown`（读取侧的 `EACCES` 等）。
- **`walk` / `walk_with`**：见 §5.6。迭代器本身不失败；起点问题是第一项 `Err`（`NoSuchPath`、`NotDirectory`）；单个条目的问题是 `Err` 项，遍历继续：`NotText`（目录项名字不是 UTF-8，APFS 上建不出这样的名字，实测 `EILSEQ`，所以只有记录解析器的单元测试到得了）、`InvalidInput`（`reason = too_deep`，或末尾的 `root_changed`）、`Unknown`（目录不可读，`reason = unreadable_entry`，`detail.errno`）。
- **`watch` / `watch_with`**：见 §5.7。`NoSuchPath`、`NotDirectory`（root 不是目录）、`Unknown`（FSEvents 流建不起来或启动失败）。运行中的事件丢失不是错误，是 `WatchEvent::Dropped` 与 `MustRescan`。
- **`case_fold_eq`**、**`Attrs::unchanged_since`**：纯函数，不失败。

## 4. 数据与不变量

### 4.1 本模块拥有的持久数据

无。apfs 不写任何文件、不建 SQLite、不留状态文件；唯一的进程内状态是 `Watcher`（一个 FSEvents 流加一条私有串行 dispatch queue）。它克隆出来的目录、读出来的属性，都归调用方。测试用的稀疏镜像只在测试进程里存在，`Drop` 时分离并删除。

### 4.2 平台行为契约（实测，2026-10-22，macOS 27.0.1）

这一节是实现者的依据：每条都在本机用探针程序验证过，§7 的测试把它们钉成回归。

**A. `clonefile(2)` 与 `clonefileat(2)`**

| 项 | 行为 |
| --- | --- |
| 原子性 | 目录克隆全有或全无。12 MB 镜像写满时 `ENOSPC`，源树里有不可读条目时 `EACCES`，两种失败都不留 dst |
| 保留 | 内容、mode、mtime（带过去的是源的 mtime，gc 判"新旧"不能只看 mtime）、扩展属性（含 `user.*` 和 `com.apple.provenance`）、BSD 标志（`uchg`、`hidden`；克隆出来的 `uchg` 文件同样不可变，对它 `chmod` 得 `EPERM`，要先清标志）、稀疏文件的洞、符号链接原样（相对、绝对、悬空、成环）、目录树里的 FIFO 与 socket（克隆出来仍是 FIFO、socket 对象） |
| 不保留 | 硬链接（每个名字一个独立 inode，`nlink` 为 1，与 git 一致，裁定 T4）；ACL（不传 `CLONE_ACL`）；普通文件的 setuid、setgid 位（`4755` 变 `0755`）；属主（传 `CLONE_NOOWNERCOPY`，克隆归调用者） |
| 嵌套挂载点 | 克隆成功，得到一个空目录占位，被挂载卷的内容不克隆 |
| 顶层源 | FIFO、socket、字符设备 → `EINVAL`（所以先 lstat 再分类）。符号链接：flags 为 0 时跟随它（悬空 → `ENOENT`），带 `CLONE_NOFOLLOW` 时克隆链接本身 |
| 路径形态 | dst 末尾带 `/` → `ENOENT`（误导，所以先拦成 `InvalidInput`）；src 是文件却带 `/` → `ENOTDIR`；dst 在 src 里面 → `EINVAL`；dst 就是 src → `EEXIST`；dst 的父目录是文件 → `ENOTDIR`；父目录不存在 → `ENOENT`；根 `/` → `EINVAL` |
| errno 的优先级 | dst 已存在而且在另一个卷 → `EEXIST`（不是 `EXDEV`）；dst 的父目录不存在而且在另一个卷 → `ENOENT`；APFS 到任何别的卷（含 HFS+、MSDOS）→ `EXDEV`；`ENOTSUP` 只在 src 与 dst 都在 HFS+ 或都在 MSDOS 时出现 |
| 不会自己退化 | 跨卷时 `clonefile` 返回 `EXDEV`，不会偷偷复制。"退化成复制"只发生在 `cp -c`、`copyfile(COPYFILE_CLONE)`、`std::fs::copy` 这类封装里，所以本 crate 不用它们（不变量 2） |
| dst 存在性检查 | 对 Unicode 规范化不敏感：先建 NFC 的 `café`，再克隆到 NFD 的 `café` → `EEXIST`；留下来的名字保持创建时的形式 |
| flags（本 SDK） | `CLONE_NOFOLLOW` 0x1、`CLONE_NOOWNERCOPY` 0x2、`CLONE_ACL` 0x4、`CLONE_NOFOLLOW_ANY` 0x8、`CLONE_RESOLVE_BENEATH` 0x10。没有"不带 xattr"的 flag |
| `CLONE_NOFOLLOW` 管不到 dst | 它只管 src 的最后一级。dst 以路径给出时，中间的符号链接照样被跟随：`clonefileat(AT_FDCWD, src, rfd, "evil/pwn", CLONE_NOFOLLOW\|CLONE_NOOWNERCOPY)`（`evil` 是指向根外的符号链接）实测把文件写到了根外。这就是增补 3 要目的地变体的原因 |
| 两个新 flag 不能用在路径形态上 | `CLONE_NOFOLLOW_ANY`（穿过任何符号链接 → `ELOOP`）与 `CLONE_RESOLVE_BENEATH`（越界 → `ENOTCAPABLE`）**同时作用于 src 与 dst**：src 是绝对路径，或路径上有符号链接祖先（`/tmp/…`），一律失败。所以目的地变体改用"先钉住父目录的句柄，再用单个分量落地"，flags 只传 `CLONE_NOFOLLOW\|CLONE_NOOWNERCOPY` |
| man page 的告诫 | `man 2 clonefile`（2021-06）写着"强烈不建议用于目录，请改用 `copyfile(3)`"。实测 `copyfile(COPYFILE_CLONE_FORCE)` 不支持目录，`COPYFILE_RECURSIVE\|COPYFILE_CLONE` 是逐文件克隆、可退化成字节复制、不原子，正是本模块要避免的。决定保留 `clonefile(2)` 克隆目录（§9）。已知代价：克隆不是并发写入的源目录的快照（deps.md 用前后两次 `walk` 兜住） |
| 耗时 | 见 §5.9 |
| 之后 | 克隆之后对共享区段的覆盖写是写时复制，卷快满时可能 `ENOSPC`；克隆一个 128 MiB 文件不占空间，往克隆里写 64 KiB 只多占 0.06 MiB，写 1 字节多占 0.02 MiB（安静的镜像上量的）；`st_blocks` 与源相同，不能拿它证明共享 |

**B. 名字与大小写**

| 项 | 行为 |
| --- | --- |
| 分量长度 | APFS 按 **UTF-16 码元**数限制，每个分量最多 255 个，不是 255 字节：255 个 U+3042（765 字节）可以建，256 个码元 → `ENAMETOOLONG`；127 个 U+1F600（254 个码元）可以，128 个 → `ENAMETOOLONG`；256 个 ASCII 字符 → `ENAMETOOLONG`。types.md §6.3 与 LayerFS 矩阵写的"255 字节"只对 ASCII 恰好成立，已记入报告请 T15 改 |
| 整条路径 | `PATH_MAX` 是 1024，含结尾的 NUL：1023 字节还行，1024 字节起 → `ENAMETOOLONG`（实测，types.md 写的"超过 1024"差一） |
| 非 UTF-8 名字 | 建不出来，`EILSEQ`（92）。所以磁盘上的名字一定是合法 UTF-8，`NotText` 在 APFS 上到不了，只有记录解析器的单元测试能构造它 |
| 保存与查找 | 保大小写、保规范化形式（NFD 建的名字，`getattrlistbulk` 返回的仍是 NFD）；查找和"已存在"检查对规范化不敏感：用 NFC 拼写能打开 NFD 存的名字，在它上面建同名文件或 `mkdir` → `EEXIST` |
| 大小写不敏感的卷（缺省，`/Volumes/GWD`） | 两个名字相同 ⇔ Unicode 规范无大小写匹配（D145）：`NFD(toCasefold(NFD(x)))` 相等，全量折叠（C+F），无土耳其语的 T 映射。51 对样本零不符：`ß`≡`SS`、`ﬃ`≡`ffi`、`İ`≡`i̇` 但 `İ`≢`i`、`ς`≡`σ`≡`Σ`、开尔文符号 K≡`k`、埃符号 Å≡`å`、切罗基、格鲁吉亚、Deseret，以及 Unicode 9 到 16 新增的折叠（含 Unicode 16 的 Garay、U+1C89 与 U+1C8A、U+A7CB 与 U+0264、U+A7DC）。零宽连接符、零宽非连接符、软连字符、VS16 **不是**可忽略字符；兼容映射（NFK*）不适用（全角 `Ａ` ≢ `a`） |
| 大小写敏感的卷 | 只对规范等价不敏感，其余全不同，`a` 与 `A` 是两个文件 |
| 用法 | 不敏感卷用 `case_fold_eq`；敏感卷用 `RepoPath` 的字节相等（`RepoPath` 已是 NFC，规范等价的名字字节相等）。选哪个由调用方按 `VolumeCaps.case_sensitive` 或 `core.ignorecase` 决定，`case_fold_eq` 自己不查卷 |

**C. gen count（`ATTR_CMN_GEN_COUNT`，0x00080000）**

`u32`，非零，单调；需要 `FSOPT_ATTR_CMN_EXTENDED`（0x20），放在 `commonattr` 里（`forkattr` 保持 0）。只在**同一个对象**（同一个 inode）上比较才有意义。man page：0 是无效值，"不能用来判断数据有没有变"，文件被 mmap 时无效。

| 动作 | gen count |
| --- | --- |
| 创建 | 空文件从 1 起；创建后写一次是 2，再写一次是 3 |
| `write`、`pwrite`（哪怕写入的字节相同）、`ftruncate` 变大、`O_TRUNC` | 变（每次写调用约 +1） |
| 写入之后把 atime、mtime 还原成旧值 | **仍然变**：这正是它比 mtime 可靠的地方（tgrep 的 `native_content_generation…` 同一结论） |
| `pread`、`touch` 与 `utimensat`、还原时间戳、`chmod`（含可执行位）、`setxattr`、`chflags`、`rename`、建硬链接、`ftruncate` 到相同大小、`open(O_WRONLY)` 后不写就 `close` | **不变**。所以可执行位与 mode 的改动 gen count 看不见，`Attrs` 带 `mode` |
| 克隆出的文件 | 自己的计数，从 1 起，与源无关 |
| 用 `rename` 盖掉一个文件 | 新 inode，计数很小，可能恰好与旧文件的相同：**不能只比 gen count**，版本的身份是 (inode, gen count) |
| 可写的共享映射（`mmap(PROT_WRITE, MAP_SHARED)`） | 在整个映射存续期间读出来是 0（`RETURNED` 位仍置），**不管**创建映射的描述符是否已经关闭、映射是否在另一个进程里、有没有发生过一次写。`munmap` 之后值变了（+1 到 +2，即使没写过）。所以 0 是"未知"，调用方改用内容哈希；`munmap` 后的变化是保守的变更信号 |
| 只读的共享映射、私有的可写映射 | 不受影响 |
| 目录 | 随目录项的增删改名变（创建、改名、`unlink`、`mkdir`、`rmdir` 各 +1，与目录 mtime 同步），**不随子文件的内容写入变**。所以目录的 gen count 只能说明"名字集合变过没有" |
| 符号链接、FIFO、socket | 也有计数（≥ 1） |
| 不支持的卷 | MSDOS 的 `VOL_CAP_FMT_WRITE_GENERATION_COUNT` 为 0，属性不返回，`Attrs.gen_count` 为 0 |

**D. `getattrlistbulk(2)`**

- 请求的属性：`commonattr = RETURNED_ATTRS | NAME | ERROR | OBJTYPE | MODTIME | CHGTIME | ACCESSMASK | GEN_COUNT | FILEID`，`dirattr = MOUNTSTATUS`，`fileattr = DATALENGTH`，`options = FSOPT_ATTR_CMN_EXTENDED`。`NAME` 与 `RETURNED_ATTRS` 是必需的；卷属性不能用。
- 记录布局：每条记录以 `u32` 的总长度开头（8 字节对齐，下一条从 `起点 + 长度` 开始）；随后是 `attribute_set_t`（5 个 `u32`：本条**实际返回**的位图）；若返回位图含 `ERROR` 则下一个 `u32` 是错误码（只在非零时出现）；然后是 `NAME` 的 `attrreference_t`（`i32` 偏移，相对**这个字段自己的地址**，加 `u32` 长度，长度含结尾 NUL）；再按属性位的顺序依次是：`OBJTYPE`（`u32`，`VREG` 1、`VDIR` 2、`VBLK` 3、`VCHR` 4、`VLNK` 5、`VSOCK` 6、`VFIFO` 7）、`MODTIME`（`timespec`，16 字节）、`CHGTIME`（同）、`ACCESSMASK`（`u32`，完整的 `st_mode`）、`GEN_COUNT`（`u32`）、`FILEID`（`u64`），最后目录有 `MOUNTSTATUS`（`u32`，`DIR_MNTSTATUS_MNTPOINT` 为 1），文件和符号链接有 `DATALENGTH`（`u64`）。各属性按 4 字节对齐，64 位的也一样，所以必须用未对齐读。
- **解析必须按每条记录的返回位图**，不能按请求的位图假定：不支持的属性（如 MSDOS 上的 gen count）不在记录里；`ERROR` 只在出错的记录里；目录与文件的尾部属性不同。（探针里就因为按请求位图解析目录，读出过错误的 gen count。）
- 缓冲区：返回 `ERANGE` 表示装不下一条记录。64 字节到 300 字节装不下一个 255 字符的名字，512 字节可以；缺省 64 KiB，`ERANGE` 时翻倍重试。
- 顺序不定；返回 0 表示读完（之后新增的项不再出现）；同一个目录句柄上不要混用 `readdir`；不返回 `.` 和 `..`，返回以点开头的隐藏文件（`.DS_Store`）。
- 变动中的目录：一个 6000 项的目录，用 600 字节的缓冲区读（426 次调用），两次调用之间删、改名、新建：从未被动过的名字**恰好出现一次**，零缺零重复（缓冲区 2048 与 64 KiB 也一样）。被删或被改名的，之前读到了就留在结果里；新建的可能出现也可能不出现。
- 挂载点：目录项报 `DIR_MNTSTATUS_MNTPOINT`，带的是底下占位目录的属性。
- 符号链接的 `DATALENGTH` 是目标的字节数；目标本身要另外 `readlinkat`。FIFO、socket、目录的长度是 0。
- 返回的名字是磁盘上的形式（NFD 保留）。

**E. `openat` 一族的 errno**

| 调用 | 情形 | 结果 |
| --- | --- | --- |
| `openat(O_RDONLY\|O_DIRECTORY\|O_NOFOLLOW)` | 目标是符号链接（不论指向什么） | `ENOTDIR`，不是 `ELOOP` |
| 同上 | 目标是文件 / 不存在 | `ENOTDIR` / `ENOENT` |
| `openat(O_RDONLY\|O_NOFOLLOW\|O_NONBLOCK)` | 最后一级是符号链接 | `ELOOP`（62） |
| 同上 | 最后一级是 socket | `EOPNOTSUPP`（102） |
| 同上 | FIFO | 成功，`fstat` 看得出是 FIFO；不带 `O_NONBLOCK` 会一直阻塞到有写端 |
| 同上 | 字符设备（`/dev/null`）、目录 | 成功（目录可以用 `O_RDONLY` 打开） |
| 同上 | mode 000 的文件 | `EACCES` |
| 只带 `O_NOFOLLOW` | 中间一级是符号链接 | **成功**，`O_NOFOLLOW` 只管最后一级 → 必须逐级 `openat` |
| `O_NOFOLLOW_ANY`（0x20000000） | 路径里有任何符号链接 | `ELOOP`；与 `O_NOFOLLOW` 一起给 → `EINVAL`；根的祖先是符号链接（`/tmp`）也被拒 → 不用 |
| `O_RESOLVE_BENEATH`（0x1000） | 绝对路径 | `ENOTCAPABLE`（107）→ 不用 |
| `openat(O_CREAT\|O_EXCL\|O_NOFOLLOW)` | 目标是符号链接（含悬空） | `EEXIST`，链接指向的地方不被碰 |
| `openat(O_CREAT\|O_NOFOLLOW)`，没有 `O_EXCL` | 悬空符号链接 | `ELOOP` |
| 名字里带 `/` 的 `openat` | | 会穿过前面的分量（`sub/n2` 实测建在子目录里）→ 传给 `*at` 的永远只是单个分量 |
| `mkdirat`、`symlinkat` | 目标已存在（文件、目录、符号链接，含指向目录的符号链接） | `EEXIST` |
| `fchmodat(…, 0)` | 目标是符号链接 | **跟随**，改的是链接指向的东西 → 一律带 `AT_SYMLINK_NOFOLLOW` |
| `clonefileat(…, dirfd, "name", …)` | dst 已存在，哪怕是悬空符号链接 | `EEXIST` |

**F. FSEvents**

| 项 | 行为 |
| --- | --- |
| 流的参数 | `kFSEventStreamCreateFlagFileEvents \| WatchRoot \| NoDefer`，latency 0.05 s，挂在一条串行 dispatch queue 上，`kFSEventStreamEventIdSinceNow` |
| 路径 | 被监听的根先被解析成 realpath（`/tmp/x` 变成 `/private/tmp/x`）；事件里的路径是 realpath，用的是**磁盘上的拼写**（原来的大小写，NFD 保留），哪怕操作用的是另一种拼写 |
| flags 累积 | 同一个路径上的多个动作合成一个事件（只创建了又删掉的文件是 "Created Removed Modified Xattr File"），所以事件之后要用 lstat 重新确认路径还在不在 |
| 目录事件 | 目录改名（`mv d e`）只有 `/d` 和 `/e` 两个事件，子项没有事件；`clonefile` 克隆目录只有一个 `Dir Cloned` 事件，子项没有事件。所以**目录事件意味着整棵子树**。文件克隆带 `Cloned` 标志；只改大小写的改名给旧名和新名各一个事件 |
| 丢事件的信号 | `MustScanSubDirs\|UserDropped`（0x3），报在**流的根路径**上。往被监听的树里一口气创建 4 到 6 万个文件，得到 1 到 21 个这样的事件，在此之前已经送出多达 3.2 万个事件；`KernelDropped` 没出现过。头文件的说法：这对标志出现时要对被监听的每个目录做全量扫描 |
| `RootChanged` | flags 0x20，事件 id 0，路径是被监听的根（它或它的祖先被改名、删除时） |
| 排除路径 | `FSEventStreamSetExclusionPaths`（最多 8 个）**挡不住**根上的 `MustScanSubDirs\|UserDropped`：在被排除的目录里洪泛，根上照样出 6 个。所以不用它，改在回调里过滤（没有个数限制）；消费者对 `Dropped` 的重扫本来就要排除这些目录 |
| latency 参数 | 0.0、0.05、0.5 三档对丢事件没有看得出来的影响（30,000 次创建，每档 0 到 7 个 `MustScanSubDirs`） |
| 送达延迟 | 从操作到回调 15 ms 到 3.3 s 不等：同一台机器、负载相近（8 到 9）时，平均 15 ms 的运行和平均 0.5 到 1.7 s 的运行前后相继出现。`FSEventStreamFlushSync` 与 `FlushAsync` 在 dispatch queue 的流上约 0.05 ms 就返回，不送达积压的事件，不是屏障 |
| 哨兵做不成屏障 | 往另一个被监听的位置写哨兵文件，等它的事件回来就当作之前的事件都到了：慢速写入（1200 对，间隔 15 ms）没有一次乱序，但一口气写 50 或 200 个文件再写哨兵时，40 轮里有 1 到 4 轮哨兵排到了这批最后 5 个事件之前，两个根和同一个根里都出现；20 个以内的爆发没有出现。→ 不提供 `fence` |
| 事件 id | 洪泛下同一批里的 id 会回退，不依赖 id 的顺序 |

### 4.3 不变量

| # | 不变量 | 落实 |
| --- | --- | --- |
| A1 | 不原地改、不覆盖：`clone_*` 只读 src，dst 必须不存在；目的地变体对已存在的东西（含悬空符号链接）一律 `Exists`；crate 里没有"覆盖"或"合并进已有目录"的路径 | design §9 不变量 5 |
| A2 | 没有复制回退：整个 crate 里没有任何复制字节的代码路径（`std::fs::copy`、`copyfile(3)`、`cp`）；卷不对就在写入之前返回 `CrossVolume` 或 `Unsupported` | design §7；types.md `CrossVolume` |
| A3 | 写入开始之后只会是 `Unknown`，现场原样留下：单次系统调用的操作（`clone_*`、`create_file_rooted`、`symlink_rooted`）失败时什么都没写，可以给具名的码；多步的（`mkdir_rooted(parents)`、`clone_file_rooted` 的克隆加设 mode）第一步成功之后的失败是 `Unknown`；apfs 从不删除、从不回滚 | design §9 不变量 6 |
| A4 | 根之下不跟随符号链接：`open_rooted`、`attrs_of`、`walk` 解析 `start`、全部目的地变体，对根之下的每一级 `openat(O_NOFOLLOW)`；写操作永远落不到根之外，也就碰不到工作区里指向基线、依赖集或主 checkout 的符号链接（dev-link 就会建这样的链接） | design §9 不变量 2、5；裁定 T4、增补 3 |
| A5 | 监听只是提示：`WatchEvent` 的缺失不证明什么；`walk` 不是快照；crate 里没有"等监听追上"或"已完整"的接口 | 裁定 T4；design §6.2"正确性不依赖监听" |
| A6 | gen count 为 0 是未知，未知永远不等于未变；`Attrs::unchanged_since` 是比较的唯一入口 | design §6.2；增补 1 |
| A7 | 资源有界：`walk` 同时持有的描述符不超过深度加一（≤ 129），与目录总数无关；`walk` 在内存里最多存当前路径上各目录的条目；`Watcher` 的回调串行 | 3f 的 `RLIMIT_NOFILE` 256 两行 |
| A8 | 没有全局可变状态：除线程局部的测试接缝外函数都无状态，可以从任意线程并发调用；`Watcher` 是唯一有生命周期的对象 | — |
| A9 | 只做被要求的 I/O：读写只限调用方传进来的路径，不创建临时文件，不写日志文件 | — |

## 5. 流程与算法

### 5.1 卷标识与能力（`same_volume`、`volume_caps`）

`same_volume(a, b)`：

1. 对 a、b 各做一次 `getattrlist(path, {commonattr: ATTR_CMN_DEVID}, FSOPT_RETURN_REALDEV)`，**不带** `FSOPT_NOFOLLOW`，即跟随符号链接。
2. 两个 `dev_t` 相等才返回 `true`。`ENOENT` 映射成 `io::ErrorKind::NotFound`，其余错误原样。

为什么是 `FSOPT_RETURN_REALDEV`：`/`（封存的系统卷）与 `/Users`（数据卷）的 `st_dev` 都是 16777233，真实设备号却是 16777235 与 16777233；`clonefile` 在两者之间返回 `EXDEV`，比较 `st_dev` 会误判为同卷。firmlink 两侧的路径（`/Users/x` 与 `/System/Volumes/Data/Users/x`）真实设备号一致（实测均为数据卷的 16777233）。`/tmp` 这样的符号链接对象本身在系统卷上，所以必须跟随。本机 `/Volumes/GWD` 是 16777243。

`volume_caps(path)`：

1. `statfs(path)` 取 `f_fstypename`。
2. `getattrlist(path, {volattr: ATTR_VOL_INFO | ATTR_VOL_CAPABILITIES})`，取能力位和它的有效位掩码：`VOL_CAP_INT_CLONE`（接口能力）、`VOL_CAP_FMT_CASE_SENSITIVE`、`VOL_CAP_FMT_WRITE_GENERATION_COUNT`（格式能力）。
3. `clone`：位置位且有效 → `Some(true)`；位清零而有效 → `Some(false)`；无效 → `None`。其余两项：有效且置位才是 `true`。

实测的取值：APFS：clone 是，大小写敏感 否，gen count 是；大小写敏感的 APFS：大小写敏感 是；HFS+：clone 位为 0 且**无效**（`None`，要真的 clone 才会得到 `ENOTSUP`）；MSDOS：clone 位为 0 且有效（`Some(false)`），gen count 否，不支持硬链接。

### 5.2 `clone_tree(src, dst)`

全部检查在写入之前，按下面的顺序（决定了不同错误并存时返回哪个码）：

1. `lstat(src)`：`ENOENT` → `NoSuchPath`；不是目录（符号链接也算，`reason = symlink`）→ `NotDirectory`。
2. 检查 dst 的形态：以 `/` 结尾 → `InvalidInput`（`trailing_slash`；内核会给误导的 `ENOENT`）；最后一个分量超过 255 个 UTF-16 码元，或整条路径 ≥ 1024 字节 → `InvalidInput`（`name_too_long`、`path_too_long`；内核也会返回 `ENAMETOOLONG`，两处同码）。
3. `lstat(dst)` 成功（含悬空符号链接）→ `Exists`；dst 就是 src 也在此返回。
4. 检查 dst 的父目录：不存在 → `NoSuchPath`；不是目录 → `NotDirectory`。
5. dst 在 src 里面（取 dst 父目录与 src 的真实路径，按分量比较）→ `InvalidInput`（`dst_inside_src`）。
6. `same_volume(src, 父目录)`：`false` → `CrossVolume`（`detail.src_dev`、`dst_dev`）。**这一步在 3、4 之后**，与内核的优先级一致（dst 已存在或父目录缺失时先报那两个码），也保证一个已经存在的 dst 不会被说成"跨卷"。
7. `volume_caps(src)`：`clone == Some(false)` → `Unsupported`（`detail.fs_type`）；`None`（HFS+）不预判，进入第 8 步让内核回答。
8. `clonefile(src, dst, CLONE_NOFOLLOW | CLONE_NOOWNERCOPY)`。成功即结束。失败按 errno 归类（types.md §6.3 加 apfs 的补充）：`EEXIST` → `Exists`；`ENOENT` → `NoSuchPath`；`ENOTDIR` → `NotDirectory`；`EXDEV` → `CrossVolume`（嵌套挂载点之下的目录也可能走到这里）；`ENOTSUP` → `Unsupported`；`ENAMETOOLONG`、`EINVAL` → `InvalidInput`；`EACCES`、`EPERM`、`EROFS` → `ReadOnly`；其余（`ENOSPC`、`EIO`、`EDEADLK`、`EMFILE`、`ENFILE` 等）→ `Unknown`。
9. 任何失败之后 `lstat(dst)` 一次，把结果写进 `detail.dst_exists`。apfs 不删除、不重试。

复杂度：内核做的 O(项数)；单线程调用，并行由调用方决定。克隆之后 apfs 不 `fsync`：需要断电安全的调用方自己 `F_FULLFSYNC`（裁定 "Durability"）。

### 5.3 `clone_file(src, dst)`

1. `lstat(src)`：`ENOENT` → `NoSuchPath`；目录 → `IsDirectory`；FIFO、socket、字符或块设备 → `NotFile`（`reason` 取 `fifo`、`socket`、`device`；内核对这些也只会给误导的 `EINVAL`）；普通文件和符号链接继续。
2. 同 §5.2 的第 2、3、4 步（dst 的形态、已存在、父目录）。
3. `same_volume`、`volume_caps`：同 §5.2 的第 6、7 步。
4. `clonefile(src, dst, CLONE_NOFOLLOW | CLONE_NOOWNERCOPY)`：普通文件克隆内容；符号链接克隆成同目标的符号链接，不读目标，悬空也行。失败的归类同 §5.2 第 8、9 步。

store 的 `put_blob` 因此不会把工作区里的符号链接当内容读进 blob：符号链接的"内容"是它的目标，由 overlay 另行处理。

### 5.4 根之下的解析、`open_rooted` 与目的地变体

共同的解析 `resolve(root, components, final_flags)`：

1. 打开根：`open(root, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)`。`ENOENT` → `NoSuchPath`；`ENOTDIR`（不是目录，或根本身是符号链接）→ `NotDirectory`。根的祖先里的符号链接由内核照常解析。
2. 对除最后一个之外的每个分量：（测试接缝：`before_open(step, name)`）`openat(dirfd, comp, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)`，成功就关掉上一级。`ENOENT` → `NoSuchPath`；`ENOTDIR` → `NotDirectory`，并用 `fstatat(dirfd, comp, AT_SYMLINK_NOFOLLOW)` 区分：是符号链接 → `reason = symlink`，否则 `not_a_directory`；`EACCES` → `Unknown`（读取侧）。一级一级走，**任何时候手里只有当前一级的句柄**，所以之后目录被换成符号链接也改不了已经打开的这一级（3f 的 `directory_swap_after_open…`）。
3. 对最后一个分量，由调用它的函数决定怎么落地（下面）。传给每个 `*at` 调用的永远只是单个分量，不含 `/`。

**`open_rooted(root, path)`**：解析父目录后，最后一个分量 `openat(dirfd, name, O_RDONLY | O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC)`：

- `ELOOP` → `NotFile`（`reason = symlink`）；`EOPNOTSUPP`（socket）→ `NotFile`（`socket`）；`ENOENT` → `NoSuchPath`；`EACCES` → `Unknown`。
- 成功后 `fstat`：字符或块设备 → 关掉，`NotFile`（`device`）；普通文件、目录、FIFO 原样返回。FIFO 因为带 `O_NONBLOCK` 不会挂住，读它得到 `WouldBlock` 或 EOF。
- 单分量的 `path`（根下直接一级）没有"父目录"这一步，直接用根的句柄。

**`open_rooted_file`**：同上，打开之后要求 `S_ISREG`：目录 → `IsDirectory`，FIFO → `NotFile`（`fifo`）。TOCTOU 下，调用方以为的普通文件被换成 FIFO 也是这个结果，不挂住（3f 的 `fifo_swap_before_final_open…`）。

**目的地变体**共同的：

1. 把 `dst` 拆成父级分量和最后一个名字，`resolve` 到父目录的句柄 P（`mkdir_rooted(parents = true)` 例外，见下）。
2. 在 P 上只用最后的名字做一次 `*at` 调用，不预先 stat：系统调用自己的 `EEXIST`（含悬空符号链接，§4.2 E）就是 `Exists`，内核对 `clonefileat` 也是先报 `EEXIST` 再报 `EXDEV`。
3. 写侧的 `EACCES`、`EPERM`、`EROFS`（包括 `resolve` 里打开父级时的 `EACCES`）→ `ReadOnly`；`ENAMETOOLONG` → `InvalidInput`；其余 → `Unknown`。

各个函数的落地：

- `clone_file_rooted(src, root, dst, mode)`：src 的检查同 §5.3 第 1 步；`same_volume`（src 与 P）；`clonefileat(AT_FDCWD, src, P, name, CLONE_NOFOLLOW | CLONE_NOOWNERCOPY)`。`mode = Some(m)` 且克隆出来的是普通文件时，再 `fchmodat(P, name, m & 0o777, AT_SYMLINK_NOFOLLOW)`；这一步失败是 `Unknown`（克隆已经发生）；源带 `UF_IMMUTABLE` 时这一步必然 `EPERM`（§4.2 A）。
- `clone_tree_rooted(src, root, dst)`：src 的检查同 §5.2 第 1 步；dst 在 src 里面同样拦（P 的路径用 `fcntl(F_GETPATH)` 取）；`same_volume`（src 与 P）；`clonefileat(AT_FDCWD, src, P, name, CLONE_NOFOLLOW | CLONE_NOOWNERCOPY)`。
- `create_file_rooted(root, dst, mode)`：`openat(P, name, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, mode)`；已存在的符号链接得到 `EEXIST`，链接指向的地方不被碰（§4.2 E）。
- `symlink_rooted(root, dst, target)`：目标为空 → `InvalidInput`（`empty_target`）；`symlinkat(target, P, name)`。
- `mkdir_rooted(root, dst, mode, parents)`：`parents = false`：解析到父目录后 `mkdirat(P, name, mode)`。`parents = true`：从根开始逐级 `openat(…, O_DIRECTORY | O_NOFOLLOW)`；某一级 `ENOENT` 就 `mkdirat` 再打开它（`mkdirat` 返回 `EEXIST` 说明别人刚建了，重新 `openat` 一次，仍然按符号链接和文件的规则判）；已存在的是目录就直接往下走；是文件 → `NotDirectory`（中间级）或 `Exists`（叶子）；是符号链接 → `NotDirectory`（中间级，`reason = symlink`）或 `Exists`（叶子，哪怕它指向一个目录）。建出至少一级之后的失败是 `Unknown`，`detail.created` 列出已建的路径。

所有目的地变体都不 `fsync`。

### 5.5 `attrs_of` 与 `Attrs`

`attrs_of(root, path)`：

1. `resolve` 到 `path` 的父目录句柄 P（单分量的 `path` 就用根的句柄）。
2. `getattrlistat(P, name, {commonattr: RETURNED_ATTRS | OBJTYPE | MODTIME | CHGTIME | ACCESSMASK | GEN_COUNT | FILEID, dirattr: MOUNTSTATUS, fileattr: DATALENGTH}, buf, size, FSOPT_NOFOLLOW | FSOPT_ATTR_CMN_EXTENDED)`。不请求 `NAME` 与 `ERROR`。`ENOENT` → `NoSuchPath`。
3. 用与批量读**同一个**记录解析器（§5.6）解出 `Attrs`。符号链接再 `readlinkat(P, name)` 取目标原字节；这一步 `ENOENT` 说明刚被换掉，重新 `getattrlistat` 一次，再失败 → `NoSuchPath`。

`Attrs` 的字段来源：`kind` 来自 `OBJTYPE`（`VREG`→`File`，`VDIR`→`Dir`，`VLNK`→`Symlink`，`VFIFO`、`VSOCK`、`VCHR`、`VBLK` 对应其余）；`size` 来自 `DATALENGTH`（目录为 0）；`mode = ACCESSMASK & 0o7777`；`gen_count` 在返回位图含 `GEN_COUNT` 时是它的值，否则为 0；`mtime`、`ctime` 来自 `timespec`（负的秒数用 `UNIX_EPOCH - Duration`）；`inode = FILEID`。

`unchanged_since(&self, earlier)`：`kind` 相同（符号链接比较 `target`）、`inode`、`size`、`mode` 相同，且 `self.gen_count != 0 && self.gen_count == earlier.gen_count`，才返回 `true`。`earlier.gen_count == 0` 同样是 `false`。

### 5.6 `walk` 与 `walk_with`

惰性迭代器。第一次 `next()` 时开始：

1. 打开根（同 §5.4 第 1 步），记下它的 (dev, inode) 作为根的身份。根不存在 → 产出 `Err(NoSuchPath)`；不是目录 → `Err(NotDirectory)`；之后迭代器结束。
2. `opts.start` 是 `Some(s)`：从根的句柄起，逐级 `openat(O_DIRECTORY | O_NOFOLLOW)` 解析 `s`（与 §5.4 第 2 步同一段代码，测试接缝照样触发）。失败 → 产出 `Err` 后结束。得到起点目录的句柄，深度 0；根的句柄与中间各级的句柄就此关掉。
3. 栈里放一个 frame：目录句柄、它相对根的路径、深度、尚未读取的条目表。
4. 栈顶 frame 的条目表没读过就读（`read_dir`，下面）。读完整个目录之后才开始产出该目录的条目：这样续读期间的目录变动不会让同一个名字出现两次（§4.2 D）。
5. 取下一个条目 e：
   1. e 是 `Err` → 产出这个 `Err`。
   2. `filter(&rel, &attrs)` 为 `false` → 丢弃 e 和它的整棵子树，取下一个。`walk` 的一参 `filter` 不看属性。
   3. 产出 `Ok((rel, attrs))`。
   4. e 是目录、不是挂载点（`DIR_MNTSTATUS_MNTPOINT` 未置）、深度还没到 `max_depth` 的限制 → 下一次 `next()` 时 `openat(dirfd, name, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)` 压栈（测试接缝：`before_open`）。`ENOENT`、`ENOTDIR`、`ELOOP`（读完目录之后它消失了，或被换成了文件、符号链接）→ 当作它已经不在，跳过子树，不报错；`EACCES` → 产出 `Err(Unknown, unreadable_entry)`，继续；深度已达 `MAX_WALK_DEPTH` → 产出 `Err(InvalidInput, too_deep)`，不进入；`EMFILE`、`ENFILE` → `Err(Unknown)`。
   5. 符号链接、挂载点、FIFO、socket、设备文件都是叶子：只产出，从不进入、从不打开。
6. frame 的条目用完 → 关闭句柄，弹栈。
7. 栈空了 → 对根路径 `lstat`，(dev, inode) 与第 1 步记下的不同（根被换掉或改名换了新目录）→ 再产出 `Err(InvalidInput, root_changed)`，然后结束。已经产出的条目来自被钉住的旧目录，调用方应当丢弃这次遍历（`move_workspace` 之后 overlay 本来就要冷扫，增补 3）。

`read_dir(fd)`：

1. 循环 `getattrlistbulk(fd, &attrlist, buf, buf_len, 0)`：返回 `n > 0` 就解析这 `n` 条记录；`0` 结束；`EINTR` 重试；`ERANGE` 把缓冲区翻倍重试，已到 `MAX_BULK_BUFFER_BYTES` 仍 `ERANGE` → `Err(Unknown)`；`ENOENT`（目录在读的时候被删了）当作空；`EACCES` 等 → `Err(Unknown, unreadable_entry)`。缓冲区的初值是 `buffer_bytes` 夹在 `[MIN, MAX]` 之间。每个窗口读完调用一次测试接缝 `after_bulk_window(n)`。
2. 每条记录按 §4.2 D 的布局、按它自己的返回位图解析。记录里 `ERROR` 非零（取这一项的属性失败）→ 用 `fstatat(fd, name, AT_SYMLINK_NOFOLLOW)` 补取一次：`ENOENT` 跳过这一项；其余错误生成一个 `Err` 条目。
3. 名字：字节 → UTF-8，失败 → `Err(NotText)`（不做 U+FFFD 替换）；NFC 规整；与父路径用 `/` 拼好，`RepoPath::try_from`（失败 → `Err(InvalidInput)`）。
4. 符号链接：`readlinkat(fd, name, buf, DATALENGTH + 1)` 取目标原字节；`ENOENT` 跳过这一项。
5. 全部条目按 `RepoPath` 的字节序排序（NFC 之后的字节），所以产出顺序确定，与目录里的物理顺序无关。

`max_depth = Some(n)`：深度（从起点算，直接子项为 1）等于 `n` 的目录条目照样产出，但不压栈。overlay 的"列一个目录"就是 `start = Some(dir)`、`max_depth = Some(1)`。

文件描述符：同时持有的只有栈上各 frame 的目录句柄，最多 `MAX_WALK_DEPTH + 1` 个（§4.3 A7）。

### 5.7 `watch` 与 `watch_with`

1. `root` 必须是绝对路径、存在且是目录（`NoSuchPath`、`NotDirectory`）。`canonical = realpath(root)`。用 `volume_caps(root)` 记下卷是否大小写敏感，供排除过滤用。
2. `FSEventStreamCreate(NULL, callback, context, [canonical], kFSEventStreamEventIdSinceNow, WATCH_LATENCY_SECS, FileEvents | WatchRoot | NoDefer)`；`FSEventStreamSetDispatchQueue(stream, queue)`（`queue` 是私有串行队列）；`FSEventStreamStart` 返回假 → 清理后 `Unknown`。不设 `IgnoreSelf`（strata 自己的写入——restore、移动工作区——也要被看见）；不调 `FSEventStreamSetExclusionPaths`（§4.2 F）。
3. 回调（`n` 个事件，各有路径与 flags），按下表翻译。路径先把 `canonical` 前缀换回调用者传入的 `root` 字符串：消费者对自己持有的路径做 `strip_prefix` 才对得上；前缀之后是磁盘上的拼写，原样保留。

| 事件的 flags | 产出 |
| --- | --- |
| 含 `MustScanSubDirs`，并含 `UserDropped` 或 `KernelDropped` | `Dropped` |
| 含 `EventIdsWrapped` 或 `RootChanged` | `Dropped` |
| 含 `MustScanSubDirs`，不含丢弃标志 | `MustRescan(path)` |
| 含 `Mount` 或 `Unmount` | `MustRescan(path)` |
| 含 `HistoryDone` | 忽略（流从 `SinceNow` 开始，不会出现） |
| 其余（有 `ItemCreated`、`ItemRemoved`、`ItemRenamed`、`ItemModified`、`ItemInodeMetaMod`、`ItemChangeOwner`、`ItemXattrMod`、`ItemFinderInfoMod`、`ItemCloned` 中任何一个，或只有 `ItemIs*`） | 路径进 `Changed` |

4. 一次回调里所有 `Changed` 的路径去重、按到达顺序合成一个 `Changed(Vec<PathBuf>)` 先发；`MustRescan` 与 `Dropped` 按事件顺序随后发。落在 `exclude` 之下的路径（按分量比较，两边先 NFC；大小写不敏感的卷用 `case_fold_eq`，敏感的卷用字节相等）从 `Changed` 与 `MustRescan` 里丢掉；`Dropped` 从不过滤。没有路径可发时不发空的 `Changed`。
5. `on_event` 在 `catch_unwind` 里调用，panic 不能穿过 FFI：截住、记日志，然后补发一个 `Dropped`（消费者因此全量重扫，不会因为丢了一个事件而错过改动）。
6. `drop(Watcher)`：`FSEventStreamStop`、`FSEventStreamInvalidate`、`FSEventStreamRelease`，再对队列 `dispatch_sync` 一个空块排空在途的回调，然后释放队列。返回之后不会再有回调。在回调线程里 drop 会死锁，debug 构建里 `debug_assert` 拦住。

给消费者的契约：（a）`Changed` 里的目录路径表示整棵子树可能变了；（b）路径可能已不存在，也可能只是被创建又删掉；（c）事件可能晚到几秒，也可能被合并或丢掉，没有办法等它"追上"，所以消费者必须有不靠监听的对账（design §6.2）；（d）`Dropped` 之后到下一次全量重扫完成之前，消费者不能相信自己的脏集合；（e）往 `exclude` 里的目录洪泛也会触发 `Dropped`。

### 5.8 `case_fold_eq`

1. 两个字符串字节相等 → `true`。两个都是 ASCII → `eq_ignore_ascii_case`。
2. 否则两边各自算 `fold(x) = NFD(full_case_fold(NFD(x)))` 再比较相等。`full_case_fold` 用 CaseFolding.txt 的 C 与 F 两类映射，不含 T（土耳其语）；折叠表的 Unicode 版本须 ≥ 16.0。
3. 不查卷、不查 `core.ignorecase`：调用方先决定要不要折叠（§4.2 B 的"用法"）。

按整条路径调用与按分量调用结果一致：`/` 在折叠与规范化下不变，也不会被产生。

### 5.9 复杂度与验收数字

| 操作 | 复杂度 | 实测与 design §8 的数字 |
| --- | --- | --- |
| `clone_tree` | 内核 O(项数)，单线程 | tool 的 `node_modules`（108,530 个文件、8,789 个目录、63 个符号链接，共 117,382 项）在 `/Volumes/GWD`（U 盘 SSD）上 2.4 s，机器忙（负载 8 到 9）时 3.9 s，约 20 到 30 µs 每项；4 路并行克隆 4 份用了 4.2 s；对照 `cp -Rc` 13.8 s；`rm -rf` 一份克隆 4.8 s。design §8 的"新建会话 < 5 s（tool + 全部兄弟库）"因此只靠调用方**并行**克隆各仓库才有把握，单棵 `node_modules` 就占去一半预算（待 spike 用全部兄弟库复测，§9） |
| `walk` | O(项数)，每个目录至少一次 `getattrlistbulk` | 温热 11.7 万项约 0.3 s（约 3 µs 每项）；冷（刚读盘）的 tool 源码树 28,889 项（不含 `node_modules`、`.git`、`target` 等）1.42 s，温热 0.20 s；staff-api 13,586 项冷 0.65 s；admin 85 ms（都是只读遍历）。design §8"全量对账 tool 源码树 < 3 s"冷读也满足；"checkpoint 改 50 个文件 < 200 ms"满足不了全量遍历（温热也要 0.2 s），要靠钩子带来的路径、监听事件、`attrs_of` 和 `max_depth = Some(1)` 的单目录列表缩小范围，这是 overlay 的事 |
| `attrs_of`、`open_rooted` 与目的地变体 | O(深度) 次 `openat` 加一次属性或落地调用 | 每次几十微秒量级（待 spike 实测，§9） |
| `watch` | 每个事件 O(1) | 送达延迟 15 ms 到 3.3 s，不是屏障（§4.2 F） |
| `case_fold_eq` | O(长度) | ASCII 走快路径 |

## 6. 失败与恢复

错误时序按 design §9 不变量 6 与增补 3 的细化：写入之前的失败是具名的码，什么都没留下；一旦第一次可见的写入发生，之后的失败一律 `Unknown`，现场原样保留。apfs 自己的操作大多是**单次系统调用**（`clonefile`、`clonefileat`、`openat(O_CREAT|O_EXCL)`、`symlinkat`），失败时内核什么都没写，所以可以给具名的码；`ENOSPC` 与 `EIO` 按 types.md §6.3 归 `Unknown`，哪怕实测它们也没留下 dst。多步的只有两个：`mkdir_rooted(parents = true)` 和 `clone_file_rooted` 的"克隆再设 mode"。apfs 从不清理、从不回滚、从不重试（`EINTR` 除外）；恢复的责任在调用方，它们在克隆之前写 intent 行（增补 3、4），崩溃之后不信任 dst 的存在。

| 情形 | 返回 | 现场 | 恢复 |
| --- | --- | --- | --- |
| dst 以 `/` 结尾；dst 在 src 里面；分量超过 255 个 UTF-16 码元；路径达到 1024 字节；`symlink_rooted` 的目标为空 | `InvalidInput`（`reason`：`trailing_slash`、`dst_inside_src`、`name_too_long`、`path_too_long`、`empty_target`） | 没写任何东西 | 改参数 |
| src 不存在；dst 的父目录不存在；`start` 或点查的路径不存在；`watch` 的 root 不存在 | `NoSuchPath` | 没写任何东西 | 目的地变体：先 `mkdir_rooted(parents = true)` 建父级 |
| `clone_tree` 的 src 不是目录（符号链接也算）；路径中间一级是文件或符号链接；`watch` 的 root 不是目录 | `NotDirectory`（`reason`：`symlink` 或 `not_a_directory`） | 没写任何东西。目的地变体撞上符号链接时，被符号链接指着的地方不被碰 | 调用方决定：工作区里多了一个符号链接，是 overlay 要报告的内容，不是 apfs 的错 |
| `clone_file` 的 src 是目录；`open_rooted_file` 的终点是目录 | `IsDirectory` | 没写任何东西 | |
| FIFO、socket、设备文件作 `clone_file` 的源；`open_rooted` 的终点是符号链接、socket、设备文件；`open_rooted_file` 的终点是 FIFO | `NotFile`（`reason`：`fifo`、`socket`、`device`、`symlink`） | 没写任何东西；FIFO 已经以非阻塞方式打开并关掉，没有挂住 | |
| dst 已存在（文件、目录、符号链接，含悬空）；dst 就是 src | `Exists` | 原有的东西与符号链接指向的地方都不被碰；apfs 从不覆盖、从不合并 | 调用方先处理已有的东西 |
| src 与 dst 不在同一个卷 | `CrossVolume`（`detail.src_dev`、`dst_dev`） | 没写任何东西，没有任何字节被复制 | 把会话根、Store 放在与主 checkout 同一个 APFS 卷上（design §7） |
| 卷不是 APFS，或不支持 `clonefile`（`ENOTSUP`，或 `volume_caps` 明确 `clone = Some(false)`） | `Unsupported`（`detail.fs_type`） | 没写任何东西 | 同上 |
| 权限：dst 的父目录不可写，**或者**源树里有不可读的条目（mode 000 的文件或目录）；只读卷 | `ReadOnly`（`EACCES`、`EPERM`、`EROFS`；message 同时写出两种可能） | `clone_*`：dst 不存在（`clonefile` 原子，实测）。目的地变体：同 | 修权限；若是源树里的不可读条目，调用方对这棵树要么排除它，要么先修 |
| 磁盘满（`ENOSPC`）、`EIO`、dataless 文件（`EDEADLK`）、描述符耗尽 | `Unknown`（`detail.errno`、`dst_exists`） | `clone_*`：dst 不存在（实测 `ENOSPC`）。克隆是写时复制，克隆之后的覆盖写才可能耗尽空间，那是调用方写文件时的错误 | 不重放、不清理，状态上报（不变量 6）；空出空间后调用方重试整个操作 |
| `mkdir_rooted(parents = true)` 建出了几级之后失败 | `Unknown`（`detail.created`） | 已建的目录留着 | 调用方按 `created` 决定留还是删 |
| `clone_file_rooted` 克隆成功而设 mode 失败 | `Unknown`（`dst_exists = true`） | 克隆出的文件留着，mode 是源的 | 调用方自己 `fchmod` 或删除重来 |
| 进程在 `clonefile` 中途被杀 | — | 系统调用要么完成要么不可见（进程被杀的情形没有观察到半成品）。**断电**的情形没有测过：不能假定 dst 完整 | 调用方在克隆之前写 intent 行；崩溃后 `recover` 不信任 dst，按 intent 行处理（增补 3、4） |
| `walk`：目录不可读 | 一条 `Err(Unknown)`（`reason = unreadable_entry`），遍历继续 | 目录条目本身已产出，它的子树缺 | 调用方决定这次扫描是否有效；overlay 把它当作扫描失败（tracker 状态不变） |
| `walk`：条目在读与打开之间消失，或被换成文件、符号链接 | 不报错，跳过 | 子树不出现。遍历本来就不是快照（A5） | 下一次扫描或监听事件会补上 |
| `walk`：深度超过 `MAX_WALK_DEPTH` | 一条 `Err(InvalidInput)`（`too_deep`） | 那棵子树不遍历，其余照常 | |
| `walk`：根在遍历中被换掉 | 末尾一条 `Err(InvalidInput)`（`root_changed`） | 已产出的条目来自旧目录 | 丢弃这次遍历的全部结果，重来（`move_workspace` 之后 overlay 本来就要冷扫） |
| `walk`：名字不是 UTF-8 | 一条 `Err(NotText)` | APFS 上不会发生（`EILSEQ`），解析器仍然防住 | |
| `watch`：FSEvents 流建不起来、启动失败 | `Unknown` | 没有 Watcher | 调用方退化为只做全量对账（design §6.2） |
| 运行中事件丢失、合并、根被改名 | 不是错误：`Dropped`、`MustRescan` | — | 消费者全量或按子树重扫，排除 `exclude` 里的目录（§5.7） |
| `on_event` 回调 panic | 截住、记日志，补发一个 `Dropped` | Watcher 继续 | 消费者因此重扫；回调的 bug 由调用方修 |

## 7. 测试清单

除标明 `#[ignore]` 的以外都默认运行，包括用 `hdiutil` 稀疏镜像的：建不了镜像时测试**失败**并说明原因，不静默跳过（两卷、不支持的卷、满盘、大小写敏感、嵌套挂载都要镜像，不需要 root）。会改文件系统的测试都在 `tempfile` 目录或镜像里做，从不碰 `/Volumes/GWD/inter`。测试里的 `std` 调用只用来造场景和读结果，被测的是 apfs 的函数。

| 测试名 | 断言 | 来源（矩阵行 或 新增） |
| --- | --- | --- |
| `clone_dir_refuses_cross_volume` | 两个 `hdiutil` 建的 APFS 镜像 A、B（两个卷）：A 上有一棵带文件、子目录和符号链接的树。`clone_tree(A/tree, B/clone)` → `CrossVolume`，`detail.src_dev` 不等于 `detail.dst_dev`；B 上没有 `clone`，B 的可用空间前后不变（没有字节被偷偷复制），A 上的树逐字节不变。对 A 上另一个目录的 `clone_tree` 照常成功。这是 design 的"跨卷退化成复制"风险（Review Focus 1）：`clonefile` 自己返回 `EXDEV`，所以要钉住的不变量是 crate 里没有任何复制回退（§4.3 A2） | 新增 |
| `clone_file_refuses_cross_volume` | 同一对镜像：`clone_file(A/file, B/copy)` → `CrossVolume`；A 到 HFS+ 镜像同样是 `CrossVolume`，不是 `Unsupported`（APFS 到任何别的卷都是 `EXDEV`）；dst 的父目录不存在而且在另一个卷 → `NoSuchPath`，dst 已存在而且在另一个卷 → `Exists`（内核的优先级，§4.2 A） | 新增 |
| `clone_on_unsupported_volume_is_unsupported` | HFS+ 镜像里把 src 克隆到同一个镜像（内核 `ENOTSUP`）→ `Unsupported`，`detail.fs_type = "hfs"`；MSDOS 镜像里（`volume_caps` 明确 `clone = Some(false)`）在调用 `clonefile` 之前就 `Unsupported`；两种都没有留下 dst | layerfs-project/tests/init_sqlite.rs::an_unsupported_platform_refuses_the_store_before_any_file_exists |
| `clone_refusals_leave_no_residue` | 跨卷、不支持的卷、dst 已存在、dst 的父目录不存在这几种拒绝之后：dst 不存在，两个卷上都没有临时文件或半成品目录，src 树逐字节不变；`clone_file` 与 `clone_tree_rooted` 的拒绝同理。写入之前拒绝、无残留（design §10 的 clonefile 跨卷退化项） | sdk/js/test/verify/direct_refusal_real.mjs |
| `clone_tree_error_precedence` | 表驱动，一种构造一个错误：src 不存在 → `NoSuchPath`；src 是文件 → `NotDirectory`；src 是指向目录的符号链接 → `NotDirectory`（`reason = symlink`）；dst 已存在（目录、文件、悬空符号链接）→ `Exists`；dst 就是 src → `Exists`；dst 的父目录不存在 → `NoSuchPath`；dst 的父级是文件 → `NotDirectory`；dst 末尾带 `/` → `InvalidInput`（`trailing_slash`）；dst 在 src 里面 → `InvalidInput`（`dst_inside_src`）。优先级：dst 已存在而且在另一个卷 → `Exists`，父目录不存在而且在另一个卷 → `NoSuchPath`，这两者都不是才是 `CrossVolume`。每一种之后原有的东西都没被碰，也没有新的 dst | 新增 |
| `clone_tree_preserves_content_modes_links_and_holes` | 源树含：mode 0755、0600、0444 的文件，中间有 1 MiB 洞的稀疏文件，相对、绝对、悬空、成环的符号链接，空目录，20 层深的嵌套。克隆后逐项相同：字节、mode、符号链接的目标原样（不被跟随）、空目录还在、洞仍是洞（`st_blocks` 与源同量级，远小于逻辑大小）；源树不变；克隆出来的文件属主是调用者 | 新增 |
| `clone_tree_keeps_xattrs_and_flags_but_drops_acls_and_setuid` | 带 `user.test` 扩展属性的文件克隆后仍有；`chflags uchg` 的文件克隆后同样不可变（删除它得 `EPERM`；测试结束前先 `chflags nouchg` 再清理）；ACL 不被复制；setuid、setgid 位被清掉（`4755` 变 `0755`）；mtime 与源相同 | 新增 |
| `clone_tree_makes_independent_hard_links` | 源里同一个 inode 的两个名字，克隆后是两个 inode（`nlink` 各为 1）；改其中一个不影响另一个；`clone_file` 克隆一个有两个名字的文件，结果也是独立的（裁定 T4：不保留硬链接） | 新增 |
| `clone_tree_clones_fifos_and_sockets_as_objects` | 目录树里的 FIFO 与 unix socket 文件，克隆后仍是 FIFO、socket（用 `attrs_of` 看 `FileKind`），不是普通文件，也不报错；顶层源是 FIFO 时 `clone_tree` → `NotDirectory`（socket 用 python 或相对路径建，避开 `sockaddr_un` 的 104 字节上限） | 新增 |
| `clone_tree_leaves_a_nested_mount_as_an_empty_stub` | 源树里有一个挂载点（第二个镜像挂在 `tree/mnt`，镜像里有文件）：克隆成功，`clone/mnt` 是空目录，被挂载卷的文件没被克隆 | 新增 |
| `clone_tree_fails_atomically_on_unreadable_entry` | 源树里有一个 mode 000 的文件（另一轮是 mode 000 的子目录）：`clone_tree` → `ReadOnly`（`detail.errno` 为 `EACCES`），message 提到两种可能，dst 不存在（`dst_exists = false`）；mode 555 的目录不受影响，克隆成功 | 新增 |
| `clone_tree_on_a_full_volume_leaves_no_destination` | 12 MB 的 APFS 镜像用文件写到 `ENOSPC`，再 `clone_tree` 一棵有几千个小文件的树 → `Unknown`（`errno = ENOSPC`），dst 不存在，apfs 没有删除任何东西；删掉填充文件后同样的调用成功 | 新增 |
| `clone_file_rejects_directories_and_special_files` | `clone_file` 的 src 是目录 → `IsDirectory`；FIFO、unix socket → `NotFile`（`reason` 分别是 `fifo`、`socket`）；字符设备 `/dev/null` → `NotFile`（`device`）；src 不存在 → `NoSuchPath`；dst 已存在 → `Exists` | 新增 |
| `clone_file_clones_symlinks_as_symlinks` | src 是符号链接（指向文件、指向目录、悬空）：`clone_file` 得到同目标的符号链接，目标的内容不被读取；src 路径中间有符号链接时照内核的规则解析，只有最后一级不跟随 | 新增 |
| `name_over_255_utf16_units_is_invalid_input` | dst 的最后一个分量：255 个 U+3042（765 字节）克隆成功，256 个 → `InvalidInput`（`name_too_long`，`detail.errno` 为 `ENAMETOOLONG`）；127 个 U+1F600（254 个码元）成功，128 个 → `InvalidInput`；256 个 ASCII 字符 → `InvalidInput`；对照：直接用 `std::fs::File::create` 建同名文件得到同一个 `ENAMETOOLONG`，说明限制来自文件系统，单位是 UTF-16 码元不是字节 | layerfs-content/tests/filesystem_limits.rs::a_component_name_is_accepted_at_255_bytes_and_refused_at_256 |
| `path_of_1024_bytes_is_invalid_input` | 构造绝对路径总长恰为 1023 字节的 dst（目录用 `openat` 链建，避开超长路径本身）：克隆成功；1024 字节、1025 字节 → `InvalidInput`（`path_too_long`）。macOS 的 `PATH_MAX` 是 1024 且含 NUL，与 LayerFS 的 4096 无关 | layerfs-content/tests/filesystem_limits.rs::a_path_is_accepted_at_4096_bytes_and_refused_at_4097 |
| `clone_is_copy_on_write_and_isolated` | 5 个基线文件的目录树 `clone_tree` 成 A、B 两份：B 上 2 个线程各往一个文件写 5 MiB，同时 A 的克隆和源树被各 6 个线程反复读，每次读都与基线的精确字节比对，从不看到 B 的写入；B 读到自己写入的字节；源树逐字节不变 | layerfs-daemon/tests/mounted_concurrency.rs::readers_on_two_workspaces_read_exact_bytes_while_one_is_written |
| `clone_then_small_write_allocates_only_the_touched_extent` | 安静的 APFS 镜像上：128 MiB 的随机文件克隆后可用空间几乎不变（少于 1 MiB）；往克隆里 `pwrite` 64 KiB 并 `fsync` 后，可用空间只再少了不到 1 MiB（实测 0.06 MiB），远小于文件大小；源文件的对应区段不变；`st_blocks` 与源相等（所以不能用它判断共享）。克隆之后改少量字节，新增占用只在被改区段的量级 | layerfs-workspace/tests/payload.rs::writes_change_only_their_window_and_never_read_the_inherited_payload |
| `creating_a_non_utf8_name_fails_with_eilseq` | 在 APFS 上用含字节 `0xff` 的名字建文件 → `EILSEQ`：证实磁盘上的名字一定是 UTF-8，`walk` 的 `NotText` 只由解析器的单元测试覆盖 | 新增 |
| `same_volume_tells_system_volume_from_data_volume` | `/usr/bin/true`（封存的系统卷）与临时目录（数据卷）：`st_dev` 相同而 `same_volume` 为 `false`；同一个临时目录里的两个文件为 `true`；临时目录与 `hdiutil` 镜像的挂载点为 `false` | 新增 |
| `same_volume_follows_links_and_reports_missing_paths` | `/tmp/x`（`/tmp` 是符号链接）与 `/private/tmp/x` 为 `true`；`/Users/<当前用户>/x` 与 `/System/Volumes/Data/Users/<当前用户>/x`（firmlink 别名）为 `true`；指向另一个卷的目录的符号链接，比较的是它指向的卷；不存在的路径 → `io::ErrorKind::NotFound` | 新增 |
| `volume_caps_per_filesystem` | 用镜像：APFS → `fs_type = "apfs"`、`clone = Some(true)`、`case_sensitive = false`、`write_gen_count = true`；大小写敏感的 APFS → `case_sensitive = true`；HFS+ → `clone = None`；MSDOS → `clone = Some(false)`、`write_gen_count = false`；路径不存在 → `NoSuchPath` | 新增 |
| `errno_mapping_table` | 单元测试，表驱动：`ENOENT`→`NoSuchPath`，`EEXIST`→`Exists`，`ENOTDIR`→`NotDirectory`，`EISDIR`→`IsDirectory`，`ENOTEMPTY`→`NotEmpty`，`EXDEV`→`CrossVolume`，`ENOTSUP`→`Unsupported`，`ENAMETOOLONG`、`EINVAL`（克隆上下文）→`InvalidInput`；写侧的 `EACCES`、`EPERM`、`EROFS`→`ReadOnly`，读侧同样的 errno→`Unknown`；`ENOSPC`、`EIO`、`EDEADLK`、`EMFILE`→`Unknown`；每个错误的 `detail` 都带 `errno` 与 `path` | 新增 |
| `rooted_open_opens_files_and_directories` | 在根之下按 `RepoPath` 打开普通文件，读到的内容正确；打开目录得到可当 dirfd 用的句柄（store、session 用它做 `unlinkat`、`fchflags`）；`open_rooted_file` 对目录 → `IsDirectory`；空串、`.`、`./dir/file`、`../file`、`dir/../file`、绝对路径连 `RepoPath` 都构造不出来，apfs 的函数没有别的路径入口；根的祖先里有符号链接（`/tmp/…`）时照常打开 | tgrep-core/src/rooted/tests.rs::regular_opens_and_literal_path_validation |
| `rooted_open_treats_colon_and_backslash_names_literally` | 名字 `a:b`、`C:relative.txt`、目录 `dir` 里的 `name:stream`、含反斜杠的 `a\b.txt`、含空格与 NFD 字符的名字，都按字面打开、读回，不当作盘符或数据流；`a\b.txt` 与 `a/b.txt` 是两个文件 | tgrep-core/tests/corrupt_index_reader.rs::builder_and_reader_round_trip_unix_colon_filenames |
| `rooted_open_fifo_is_nonblocking_and_symlinks_are_refused` | 在子进程里跑，20 秒死线（到点杀掉并判失败，防止测试挂死）：树里有一个没有写端的 FIFO：`open_rooted` 立即返回，`metadata().file_type().is_fifo()` 为真，读它得到 `WouldBlock` 或 EOF；`open_rooted_file` → `NotFile`（`fifo`）；同一目录里指向根内、根外文件的符号链接 → `NotFile`（`symlink`），链接目标的内容从未被读到 | tgrep-core/src/rooted/tests.rs::batch_child_fifo_open_is_nonblocking_and_never_follows_symlinks |
| `rooted_open_rejects_a_directory_swapped_for_a_link` | 接缝 `before_open` 在打开中间目录 `dir` 之前把它换成符号链接，指向根外（放着 `.env` 之类的文件）和指向根内的另一个目录两种：`open_rooted` 都 → `NotDirectory`（`reason = symlink`），根外文件的内容从未被读到 | tgrep-core/src/rooted/tests.rs::directory_swap_before_open_rejects_outside_and_inside_links |
| `rooted_open_keeps_the_parent_handle_after_a_swap` | 接缝在最后一个分量 `openat` 之前，把已经打开的父目录的路径换成指向根外的符号链接：这次调用仍读到原目录里的原内容（句柄没被重定向）；之后的新调用 → `NotDirectory` | tgrep-core/src/rooted/tests.rs::directory_swap_after_open_cannot_redirect_parent_handle |
| `rooted_open_pins_the_root` | 接缝在第一级分量之前把根目录换成符号链接：这次调用仍读到原根里的内容；之后的新调用 → `NotDirectory`（根是符号链接）。`walk` 上对应的"根身份已变"检查见 `walk_reports_a_swapped_root` | tgrep-core/src/rooted/tests.rs::pinned_root_cannot_be_retargeted_between_verification_and_open |
| `rooted_open_rejects_a_final_symlink_swap` | 接缝在最后一级之前把目标文件换成指向根外秘密文件的符号链接：`open_rooted` → `NotFile`（`reason = symlink`，`errno` 为 `ELOOP`），秘密文件的内容从未被读到；`open_rooted_file` 同 | tgrep-core/src/rooted/tests.rs::final_symlink_swap_is_rejected |
| `rooted_open_file_refuses_a_fifo_swapped_in` | 接缝在最后一级之前把目标文件换成 FIFO（子进程加死线）：`open_rooted_file` 立即 → `NotFile`（`fifo`），不阻塞；`open_rooted` 返回非阻塞的 FIFO 句柄，调用方 `fstat` 看得出它不是普通文件 | tgrep-core/src/rooted/tests.rs::fifo_swap_before_final_open_is_nonblocking |
| `rooted_open_error_codes` | 不存在 → `NoSuchPath`；中间一级是文件 → `NotDirectory`（`not_a_directory`）；终点是 unix socket → `NotFile`（`socket`）；`open_rooted(Path::new("/dev"), "null")`（字符设备）→ `NotFile`（`device`）；mode 000 的文件 → `Unknown`（`errno` 为 `EACCES`）；用 NFC 拼写能打开磁盘上以 NFD 存的名字 | 新增 |
| `rooted_destination_refuses_a_symlinked_parent` | 根里有指向根外目录的符号链接 `link` 和指向根内另一个目录的符号链接 `inner`：对每个目的地变体（`clone_file_rooted`、`clone_tree_rooted`、`create_file_rooted`、`symlink_rooted`、`parents` 取两个值的 `mkdir_rooted`），dst 为 `link/x`、`inner/x`、`link/a/b` 时 → `NotDirectory`（`reason = symlink`）；根外目录与 `inner` 指向的目录的列表前后不变，根里也没有新东西。这是增补 3 的测试：被跟踪的符号链接在种子文件的父目录上，写入永远出不了会话根 | 新增 |
| `rooted_destination_never_overwrites` | dst 的位置上已有文件、目录、指向文件的符号链接、悬空符号链接时，每个目的地变体（`mkdir_rooted` 取 `parents = false`）→ `Exists`，原有的东西与符号链接指向的地方都没被碰；`mkdir_rooted(parents = true)` 对已有目录成功，对已有的文件或符号链接叶子 → `Exists` | 新增 |
| `rooted_destination_swap_after_resolution_stays_inside` | 接缝在最后一个分量落地之前，把父目录的路径换成指向根外的符号链接：写入仍落在原来被钉住的父目录里，根外目录没有新东西；`clone_tree_rooted` 同理 | 新增 |
| `mkdir_rooted_parents_semantics` | `parents = true` 一次建出 `a/b/c`；再调一次成功（幂等）；中间一级是文件 → `NotDirectory`，没有新东西；`mode = 0o555` 建 `p/q`，`p` 建出之后 `q` 建不进去 → `Unknown`，`detail.created` 为 `["p"]`，`p` 留着 | 新增 |
| `clone_file_rooted_sets_the_requested_mode` | src 的 mode 是 0600：`mode = Some(0o644)` 后 dst 是 0644，`Some(0o755)` 得 0755，`None` 保持 0600；src 的 mode 不变；`Some(0o4755)` 的 setuid 位被屏蔽；src 是符号链接时 mode 参数无效；src 带 `uchg` 时 `Some(..)` → `Unknown`（`errno` 为 `EPERM`，`dst_exists` 为 true，克隆留着），`None` 成功，调用方清掉克隆的标志之后自己 `fchmod` 成功 | 新增 |
| `clone_tree_rooted_is_atomic_and_refuses_cross_volume` | 根在镜像 B 上、src 在镜像 A 上：`clone_tree_rooted` → `CrossVolume`，B 上没有 dst；父目录缺失 → `NoSuchPath`；dst 在 src 里面 → `InvalidInput`；成功时 dst 与 `clone_tree` 的结果逐项相同 | 新增 |
| `create_file_rooted_and_symlink_rooted_basics` | `create_file_rooted` 返回的句柄可写，文件一开始是空的，mode 受 umask 影响、对句柄 `fchmod` 后精确，第二次调用 → `Exists`；`symlink_rooted` 建出的链接，`readlink` 得到的字节与给定的目标完全相同（含非 UTF-8 字节和相对路径 `../x`）；空目标 → `InvalidInput`（`empty_target`） | 新增 |
| `gen_count_changes_on_write_not_on_touch` | 以一个文件的 `attrs_of` 为基准。（a）`pwrite` 一个字节（哪怕与原来相同）→ gen_count 变；写入后用 `utimensat` 把 atime、mtime 还原成旧值，gen_count 仍是变了的值，而 `mtime` 与基准相等；`ftruncate` 变大、`O_TRUNC` 也变。（b）`utimensat` 改时间戳、还原时间戳、`chmod`（含加可执行位）、`setxattr`、`chflags`、`rename`、建硬链接、`ftruncate` 到相同大小、`open(O_WRONLY)` 后不写就关闭、`pread`：gen_count 都不变；`chmod` 之后 `mode` 变了，这就是 `Attrs` 带 `mode` 的原因。（c）`unchanged_since` 在（a）的每一步之后为 `false`，在（b）里除 `chmod` 以外的每一步之后为 `true` | 新增 |
| `gen_count_is_unknown_while_a_writable_mapping_exists` | 与 tgrep 原测试同一个情形：对文件建立可写共享映射，创建映射的描述符已经关闭、映射仍在；另一个独立的读取（`attrs_of`）得到的 gen_count 在映射后、经映射写入后、`msync` 之后三个时点都是 0，`unchanged_since` 为 `false`；映射之前先有一个非零的基准；写入（事后还原时间戳）后 gen_count 改变；解除映射后 gen_count 非零且不同于写入后的值，读到经映射写入的字节 | tgrep-core/tests/managed_storage_macos.rs::native_content_generation_invalidates_writes_and_rejects_retained_writable_mappings |
| `gen_count_unknown_with_creator_descriptor_still_open` | 创建可写映射的描述符仍开着：映射存续期间 `attrs_of` 的 gen_count 仍是 0（矩阵的备注：apfs 不得从 tgrep 那一种情形推广，要推广须自己的用例） | 新增 |
| `gen_count_unknown_for_a_mapping_made_by_another_process` | 子进程建立可写共享映射并保持（管道同步），父进程读到的 gen_count 是 0；子进程退出、映射消失之后读到非零 | 新增 |
| `read_only_and_private_mappings_keep_gen_count_known` | 只读共享映射、私有可写映射存续期间 gen_count 非零且与映射前相同（私有映射里的写不影响文件） | 新增 |
| `gen_count_alone_is_not_a_version` | 克隆一个文件，克隆的 gen_count 从 1 起，可能与源相同；用 `rename` 把另一个同样小 gen_count 的文件盖到原路径：gen_count 相同、inode 不同；这两种情况下 `unchanged_since` 都是 `false` | 新增 |
| `directory_gen_count_tracks_entries_not_content` | 目录：创建、改名、删除子项，建、删子目录，各使它的 gen_count 增加；往子文件里写内容不改变父目录的 gen_count；所以对目录 `unchanged_since` 的含义只是名字集合没变 | 新增 |
| `attrs_versions_differ_for_same_second_writes` | 同一秒内对同一文件两次不同内容的写入（size 相同、秒级 mtime 相同）：两次 `attrs_of` 的 `Attrs` 不相等，`unchanged_since` 为 `false`；`mtime` 带纳秒（至少一次的纳秒部分非 0） | tgrep-core/src/walker.rs::metadata_walk_reports_precise_same_second_versions |
| `same_size_write_with_restored_mtime_is_a_new_version` | 同尺寸改写后用 `utimensat` 还原 mtime：`mtime` 与改写前相等而 `gen_count` 不同，`unchanged_since` 为 `false`；纯改时间戳（`touch -r` 一类）不算改动，`unchanged_since` 为 `true` | tgrep-core/src/walker.rs::metadata_walk_detects_same_size_write_with_restored_mtime |
| `attrs_unchanged_since_rules` | 纯函数，表驱动：任一边 gen_count 为 0 → `false`，两边都是 0 也是 `false`；inode、size、mode、kind 任一不同 → `false`；符号链接的目标不同 → `false`；mtime、ctime 不同而其余全同 → `true`；全同且 gen_count 非零 → `true` | 新增 |
| `attrs_of_reports_every_field` | 普通文件、目录、符号链接（目标取原字节，含非 UTF-8）、FIFO、socket：`kind`、`size`（符号链接是目标长度，目录与 FIFO 是 0）、`mode`（文件上的 setuid 位 `4755` 保留）、`inode` 与 `std::fs::metadata` 一致，`mtime`、`ctime` 与 `std` 的纳秒一致，`gen_count` 非零；与 `walk_with(max_depth = Some(1))` 对同一条目给出的 `Attrs` 相等 | 新增 |
| `attrs_of_does_not_follow_links` | 最后一级是符号链接：返回链接本身（`Symlink`），不是目标；父级是符号链接 → `NotDirectory`（`symlink`）；不存在 → `NoSuchPath`；根下单分量的路径可用 | 新增 |
| `walk_lists_all_entries_relative_to_root` | 无排除时 `walk` 列出树里全部条目（文件、目录、符号链接），路径是相对根的 `RepoPath`，没有根前缀，不含根自己；扩展名为二进制的文件（`.png`、`.zip`）同样列出，apfs 不按扩展名过滤 | tgrep-core/src/walker.rs::walk_dir_no_excludes_returns_all_files |
| `walk_excludes_configured_dirs` | 树里有 `vendor/` 与 `src/`：filter 对 `vendor` 返回 `false` 后，`vendor` 本身及其下所有文件都不出现，`src` 下的文件不受影响；filter 收到的是相对根的路径，目录也会被问到 | tgrep-core/src/walker.rs::walk_dir_exclude_single_dir |
| `walk_excludes_several_dirs` | 同时排除 `vendor`、`node_modules`、`target` 三个目录名（filter 按名字匹配任意层级的同名目录）：三者都消失，其余保留 | tgrep-core/src/walker.rs::walk_dir_exclude_multiple_dirs |
| `walk_exclusion_of_a_missing_dir_is_a_noop` | 排除一个树里不存在的目录名，结果与不排除时逐项相同 | tgrep-core/src/walker.rs::walk_dir_exclude_nonexistent_dir_is_noop |
| `walk_exclusion_prunes_the_whole_subtree` | 被排除目录下嵌套的子目录与文件一并不出现；filter 对被剪掉的子树里的任何路径一次都没被调用（计数器断言：剪枝不读目录） | tgrep-core/src/walker.rs::walk_dir_exclude_skips_nested_files |
| `walk_serves_full_and_incremental_scans` | 同一个 `walk_with` 既做全树遍历，也做 `start = Some(dir)`、`max_depth = Some(1)` 的单目录列表（只产出 `dir` 的直接子项，子目录作为 `Dir` 产出但不读它们，路径仍相对根）；两种用法都服从同一个按目录名排除的 filter；git-strata 只有一个 `walk` 实现 | tgrep-core/src/walker.rs::walk_file_metadata_excludes_dirs |
| `walk_wide_directory_is_complete_and_stable_across_buffer_sizes` | 目录 `wide` 里 1300 个文件按降序创建，另有符号链接（相对、悬空）、嵌套目录、两个空目录、两个硬链接名字：每一项恰好出现一次，顺序是 `RepoPath` 的字节序；`buffer_bytes = 1024`（强制很多次续读）与缺省缓冲得到逐项相同的结果；符号链接带目标原字节、不被跟随；空目录各是一条 `Dir`；两个硬链接名字各一条，`inode` 相同；`after_bulk_window` 接缝被调用的次数大于 1 | layerfs-project/tests/backed_acquisition.rs::wide_nested_aliased_root_equals_the_whole_namespace_constructor |
| `walk_very_wide_directory_through_many_windows` | 一个目录里 17000 个悬空符号链接（目标约 960 字节，以打乱的顺序创建），用最小缓冲区遍历：恰好 17000 条，名字严格按 `s00000` 到 `s16999` 升序，每条都是 `Symlink` 且目标与创建时逐字节相同；窗口数远大于 1 | layerfs-project/tests/backed_acquisition.rs::very_wide_directory_is_ordered_through_many_windows |
| `walk_after_mass_deletion_lists_only_survivors` | 目录先建 10000 个文件，删掉其中 9870 个（留下大片连续的名字空洞），再用小缓冲区 `walk`：恰好列出剩下的 130 个，每个一次，没有被删的名字，也不因为某个窗口没有任何可用条目而提前结束 | layerfs-workspace/tests/kernel_directory.rs::empty_whiteout_pages_continue_and_only_accepted_names_become_resume_cookies |
| `walk_returns_untouched_names_once_while_the_directory_changes` | 6000 项的目录，缓冲区小到每个窗口两项左右，用接缝 `after_bulk_window` 在窗口之间删除、改名、新建一些名字：从未被动过的名字每个恰好出现一次，零缺零重复；被删、被改名与新建的名字可有可无，但不出现重复 | layerfs-workspace/tests/namespace_view.rs::enumeration_returns_every_persistent_name_once_across_mutation_and_resume |
| `walk_never_follows_symlinks` | 树里有指向根外目录、根内目录、文件的符号链接和悬空的符号链接：每个只产出一条 `Symlink`（带目标），目标目录里的东西不出现；成环的链接（`a -> .`）不会让遍历无限下去 | 新增 |
| `walk_does_not_descend_into_mount_points` | 树里 `mnt` 是第二个镜像的挂载点：产出一条 `Dir` 条目 `mnt`，其下没有任何条目，即使镜像里有文件 | 新增 |
| `walk_reports_an_unreadable_directory_and_continues` | 一个 mode 000 的子目录：产出 `Err(Unknown)`（`reason = unreadable_entry`，`detail.errno` 为 `EACCES`），目录条目本身仍然产出，其余兄弟与别的子树照常遍历完 | 新增 |
| `walk_skips_entries_that_vanish_mid_walk` | 接缝在 walk 打开子目录 `d` 之前删掉它：不报错，`d` 的子树不出现，遍历继续；条目被删后 `readlinkat` 失败的符号链接同样被跳过 | 新增 |
| `walk_never_follows_a_directory_swapped_for_a_link` | 接缝在 walk 打开子目录 `d` 之前把它换成指向根外目录的符号链接：根外目录的内容绝不出现，walk 不报错并继续（按"这个目录已经不在了"处理） | 新增 |
| `walk_reports_a_swapped_root` | walk 进行中把根目录改名，并在原路径上放一个新目录：已产出的条目来自被钉住的旧目录，遍历结束时多一条 `Err(InvalidInput)`（`reason = root_changed`）；根没被动时没有这一条 | 新增 |
| `walk_start_is_resolved_without_following_links` | `start = Some("a/b")`：产出的路径带 `a/b/` 前缀；`a` 是指向别处的符号链接 → 第一项 `Err(NotDirectory)`（`symlink`），别处的内容不出现；`start` 不存在 → `Err(NoSuchPath)`；`start` 是文件 → `Err(NotDirectory)` | 新增 |
| `walk_names_are_nfc_and_openable_either_way` | 用 NFD 建的名字：`walk` 产出 NFC 的 `RepoPath`，排序也按 NFC 的字节；`open_rooted` 用这个 NFC 路径能打开它；解析器的单元测试喂一个含 NFD 名字的缓冲区，得到同样的 NFC 结果 | 新增 |
| `walk_depth_limit_is_an_error_item` | 深 130 层的链（每层一个字符的目录名，路径远小于 1024 字节）：`walk` 产出到第 128 层的目录，然后一条 `Err(InvalidInput)`（`reason = too_deep`），不再往下；同一棵树里别的浅分支不受影响；`MAX_WALK_DEPTH` 层以内的树完整遍历 | 新增 |
| `walk_fits_a_low_descriptor_limit` | 父测试启动子进程（`current_exe`，`--ignored --exact low_descriptor_limit_child`）；子进程先把 `RLIMIT_NOFILE` 软限降到 256，再用 `walk` 遍历 512 个互不相同的目录（宽的，外加一条深 100 层的链），不得 `EMFILE`；遍历期间每条条目到达时数一次 `/dev/fd` 的条目，最大值不超过深度加一个小常数，与目录总数无关；父测试断言子进程退出码为 0 | tgrep-core/src/worktrees/tests.rs::managed_and_unmanaged_parent_pins_fit_a_low_descriptor_limit |
| `low_descriptor_limit_child` | 上一行的子进程入口（`#[ignore]`，只由父测试驱动）：在子进程里降限，做同样的 512 目录遍历，再对每个目录做 `attrs_of` 对账（每次都开关句柄，不泄漏）；降限不污染同一进程里的其他测试 | tgrep-core/src/worktrees/tests.rs::low_descriptor_limit_child |
| `bulk_record_parser_follows_the_returned_bitmap` | 单元测试，合成缓冲区：返回位图缺 `GEN_COUNT`（MSDOS 的情形）→ `gen_count = 0`，后面的 `FILEID` 仍解析在正确位置；位图含 `ERROR` 才读错误码；目录记录的尾部是 `MOUNTSTATUS`，文件记录的尾部是 `DATALENGTH`；各字段按 4 字节对齐的未对齐读；一个缓冲区里的 `n` 条记录按各自的长度前进；名字偏移相对 `attrreference_t` 自己 | 新增 |
| `bulk_record_parser_rejects_bad_records` | 单元测试：名字字节不是 UTF-8 → `NotText`（不做 U+FFFD 替换）；记录长度为 0、超出缓冲区、名字偏移越界 → `Unknown`，不 panic、不越界读；`ERROR` 非零的记录交给 `fstatat` 补取分支 | 新增 |
| `watch_flags_map_to_events` | 单元测试，纯函数 `translate(flags, path)`：`MustScanSubDirs\|UserDropped`、`MustScanSubDirs\|KernelDropped`、`EventIdsWrapped`、`RootChanged` → `Dropped`；单独的 `MustScanSubDirs` → `MustRescan(path)`；`Mount`、`Unmount` → `MustRescan(path)`；`HistoryDone` 被忽略；`ItemCreated`、`ItemRemoved`、`ItemRenamed`、`ItemModified`、`ItemInodeMetaMod`、`ItemXattrMod`、`ItemCloned` 及其组合 → `Changed`；同一批里的 `Changed` 去重并合成一个，先于 `MustRescan`、`Dropped` 发出；路径前缀的 realpath 被换回调用者传入的 root | 新增 |
| `watcher_reports_must_rescan` | `#[ignore]`（洪泛，几秒到几十秒）。监听一个临时根，往里一口气创建 6 万个文件（分在多个子目录），收集全部事件。**"不静默丢失"的不变式**：每个被创建的文件，要么被某个 `Changed` 路径覆盖（它本身，或它的某个祖先目录），要么这期间至少收到过一个 `Dropped` 或覆盖它的 `MustRescan`。最多重试 5 轮洪泛，直到亲眼看到一个 `Dropped`（实测 4 到 6 万次创建能触发 1 到 21 个 `MustScanSubDirs\|UserDropped`）；5 轮都没触发则测试失败并打印原因，不静默通过；`Dropped` 之后的回调仍然有效 | 新增 |
| `watcher_reports_file_and_directory_changes` | 创建、改写、改权限、改名、删除一个文件，各得到一个覆盖该路径的 `Changed`；目录改名只有旧、新两个目录路径，没有子项事件；`clone_tree` 克隆进被监听的根只有一个目录事件，没有子项事件（所以目录事件即整棵子树）；先创建后删除的路径仍出现在 `Changed` 里（消费者要 lstat 复核）；事件可能晚到，测试最多等 5 s，不断言延迟上限 | 新增 |
| `watcher_paths_use_the_callers_root_and_disk_spelling` | 以符号链接根（`/tmp/x`）调用 `watch`：事件路径以 `/tmp/x` 为前缀，不是 `/private/tmp/x`；用 `Foo.txt` 建文件、用 `foo.txt` 去改它：事件里是磁盘上的拼写 `Foo.txt`；NFD 建的名字，事件路径保持 NFD | 新增 |
| `watcher_excludes_configured_dirs` | `exclude = [node_modules]`：往 `node_modules/pkg/x` 写文件不产生 `Changed`，往 `src/x` 写产生；排除按分量比较（`node_modules2` 不被误排除；大小写不敏感的卷上 `Node_Modules` 被排除）；`Dropped` 不被过滤 | 新增 |
| `dropping_the_watcher_stops_callbacks` | `drop(watcher)` 返回之后，再怎么改动被监听的树，回调都不再被调用（等 1 s 复核）；回调正在执行时 drop 会等它返回；回调里 panic 被截住，随后收到一个 `Dropped`，Watcher 仍可正常 drop | 新增 |
| `watcher_reports_a_renamed_root_as_dropped` | 被监听的根被改名（或它的祖先被改名）：收到 `Dropped`（`RootChanged`）；之后在原路径放一个新目录并写文件，旧 Watcher 看不到新目录里的事件，调用方丢弃它、重新 `watch` 才能再收到 | 新增 |
| `case_fold_eq_known_pairs` | 表驱动，不碰文件系统，期望值全取自 oracle 测试在本机验证过的结果：`ß`≡`SS`、`ﬃ`≡`ffi`、`İ`≡`i̇` 但 `İ`≢`i`、`ς`≡`σ`≡`Σ`、开尔文符号≡`k`、埃符号≡`å`、切罗基、格鲁吉亚、Deseret、Unicode 16 新增（Garay、U+1C89 与 U+1C8A、U+A7CB 与 U+0264、U+A7DC）、NFC≡NFD；零宽连接符、软连字符、VS16 不可忽略；全角 `Ａ` ≢ `a`；ASCII 快路径；空串 | 新增 |
| `case_fold_eq_matches_a_case_insensitive_volume` | oracle：在大小写不敏感的 APFS 镜像上，对表里每一对 (a, b) 先用 a 建文件，再问文件系统 b 能不能打开它，断言 `case_fold_eq(a, b)` 与文件系统的答案一致；表覆盖 §4.2 B 的全部类别，再加按码位区间随机抽样的对子（折叠表的 Unicode 版本落后就会在这里失败） | 新增 |
| `case_sensitive_volume_distinguishes_case_but_not_normalization` | 大小写敏感的 APFS 镜像：`a` 与 `A` 是两个文件，NFC 与 NFD 的拼写是同一个文件（建一个，用另一个拼写 `open` 成功、`create` 得 `EEXIST`）；`volume_caps(..).case_sensitive` 为 `true`；说明调用方在这种卷上不该用 `case_fold_eq` | 新增 |
| `walk_tool_sized_tree_within_the_reconcile_budget` | `#[ignore]`（量级测试）：合成 3 万项的源码树（目录、小文件、符号链接的比例取自 tool），`walk` 全量遍历在 3 s 内完成（design §8"全量对账 tool 源码树 < 3 s"；实测冷读 1.42 s），打印耗时与每项微秒数，写回 §5.9 | 新增 |
| `clone_node_modules_sized_tree_within_the_budget` | `#[ignore]`：合成 10 万项的树，`clone_tree` 在 5 s 内完成（实测 11.7 万项 2.4 到 3.9 s）；4 路并行克隆 4 份的总耗时不超过单份的 2 倍；打印这些数字，以及 `rm -rf` 其中一份的耗时（session `rm` 的成本） | 新增 |

**理由点名 apfs 的 `—` 行**（共 8 行，外加 3a 的 `…a_path_is_accepted_at_4096_bytes_and_refused_at_4097` 一行，它的理由写"归类同上一行"）怎样落实：

- 3a `filesystem_attributes.rs::the_metadata_forms_compare_by_value`（不保留）：overlay 只比内容哈希与 mode，不比 mtime，"touch 不算改动"由 `gen_count_changes_on_write_not_on_touch` 与 `attrs_unchanged_since_rules` 承担。
- 3a `filesystem_limits.rs::a_component_name_is_accepted_at_255_bytes_and_refused_at_256` 与 `…a_path_is_accepted_at_4096_bytes_and_refused_at_4097`（替代）：上表的 `name_over_255_utf16_units_is_invalid_input`、`path_of_1024_bytes_is_invalid_input`；两行用了 3a 的来源。
- 3b `init_sqlite.rs::an_unsupported_platform_refuses_the_store_before_any_file_exists`（不保留）：上表的 `clone_on_unsupported_volume_is_unsupported`，另有 `volume_caps_per_filesystem` 给 store 的预检用。
- 3e `init_refusals.mjs::overlay`（不保留，Linux 容器的 overlayfs 回归）：没有对应的 apfs 测试；非 APFS 或跨卷的拒绝由 Review Focus 1 的跨卷拒绝测试和 `clone_on_unsupported_volume_is_unsupported` 承担。
- 3f `walker.rs::walk_dir_retains_binary_extensions_for_file_listing`（不保留）：`apfs::walk` 不按扩展名过滤，这一点在 `walk_lists_all_entries_relative_to_root` 里断言。
- 3f `worktrees/tests.rs::snapshot_fifo_swaps_invalidate_readiness_without_blocking` 与 `…::directory_link_swap_after_discovery_keeps_the_view_not_ready`（依赖承担，tgrep-core 自己的测试覆盖库内路径）：strata 自己读取路径上的同一语义是上表的 `rooted_open_*` 各行和 `walk_never_follows_a_directory_swapped_for_a_link`。
- 3f `corrupt_index_reader.rs::builder_and_reader_round_trip_unix_colon_filenames`（不保留）："冒号名字按字面打开"是 apfs 要满足的，上表的 `rooted_open_treats_colon_and_backslash_names_literally` 用了这个来源。

3f 里还有几行用到 `case_fold_eq`，但目标是别的模块（gitio 的 `tracked_lookup_folds_ascii_case_when_ignorecase`，overlay 的大小写别名一行）：它们在各自模块里测"怎样使用"，apfs 的契约由 `case_fold_eq_*` 三行钉住。

**统计**：矩阵里目标为 `apfs` 的 25 行全部落在上表（3b 2 行、3c 3 行、3d 1 行、3e 1 行、3f 18 行），另有 4 行 `—` 行借用来源（3a 2 行、3b 1 行、3f 1 行）；其余是新增用例。

## 8. 实现任务（TDD）

前提：workspace 根 `Cargo.toml` 与 `crates/strata-types` 已经存在（types.md §8）。本 crate 的 feature `test-seams` 默认关闭，要用接缝的集成测试（Task 3、4、6）带 `--features test-seams` 运行。所有测试只用 `tempfile` 目录和 `hdiutil` 稀疏镜像，从不碰 `/Volumes/GWD/inter`；`tests/common/mod.rs` 提供 `Image::create(fs, size_mb)`（`hdiutil create -type SPARSE`，`fs` 取 APFS、大小写敏感的 APFS、HFS+、MS-DOS，挂载点在临时目录里，`Drop` 时 `hdiutil detach -force` 并删镜像）、`make_fifo`、`make_socket`（先在短路径上 `bind` 再 `rename` 进目标，避开 `sockaddr_un` 的 104 字节上限）、`fill_until_enospc`。每个任务先写测试、看它因函数还没定义而编译失败，再实现。

### Task 1: crate 骨架、errno 归类、卷标识与能力

**Files:**
- Create: `crates/strata-apfs/Cargo.toml`、`crates/strata-apfs/src/lib.rs`、`crates/strata-apfs/src/error.rs`（errno 到 `StrataError`，§6 与 types.md §6.3）、`crates/strata-apfs/src/volume.rs`
- Modify: `Cargo.toml`（把 `crates/strata-apfs` 加进 `members`）
- Test: `crates/strata-apfs/tests/volume.rs`、`crates/strata-apfs/tests/common/mod.rs`

**Interfaces:**
- Consumes: `strata_types::{StrataError, Code}`
- Produces: `Result`、各常量、`same_volume`、`VolumeCaps`、`volume_caps`；`error.rs` 里 crate 内部的 `io_error(errno, ctx, path)`（读侧、写侧两套归类）

- [ ] **Step 1: 写失败的测试**：§7 的 `errno_mapping_table`（`src/error.rs` 里的单元测试）、`creating_a_non_utf8_name_fails_with_eilseq`、`same_volume_tells_system_volume_from_data_volume`、`same_volume_follows_links_and_reports_missing_paths`、`volume_caps_per_filesystem`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-apfs --test volume`
Expected: FAIL（`same_volume`、`volume_caps` 还没定义，编译不过）

- [ ] **Step 3: 写让测试通过的最少实现**：`same_volume` 按 §5.1（`getattrlist` + `FSOPT_RETURN_REALDEV`，不带 `FSOPT_NOFOLLOW`）；`volume_caps` 读 `statfs` 与 `ATTR_VOL_CAPABILITIES`，`clone` 用 `Option<bool>` 表示有效位；`error.rs` 的映射表与 types.md §6.3 逐行一致，读侧的 `EACCES` 是 `Unknown`、写侧是 `ReadOnly`，`detail` 带 `errno` 与 `path`；`Image` 辅助函数建不了镜像时 `panic!` 并说明原因，不跳过
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-apfs --lib && cargo test -p strata-apfs --test volume`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/strata-apfs
git commit -m "feat(apfs): errno mapping, same_volume and volume_caps"
```

### Task 2: `clone_tree` 与 `clone_file`

**Files:**
- Create: `crates/strata-apfs/src/clone.rs`
- Test: `crates/strata-apfs/tests/clone.rs`

**Interfaces:**
- Consumes: Task 1 的 `same_volume`、`volume_caps`、`error.rs`；`strata_types::{Code, StrataError}`
- Produces: `clone_tree`、`clone_file`

- [ ] **Step 1: 写失败的测试**：§7 里 Review Focus 1 的跨卷拒绝测试（两个镜像，`clone_tree` 跨卷 → `CrossVolume`，B 的可用空间不变，不退化成复制；它的名字见 §7 表的第一行）、`clone_file_refuses_cross_volume`、`clone_on_unsupported_volume_is_unsupported`、`clone_refusals_leave_no_residue`（`clone_tree_rooted` 的那一半在 Task 4 补上）、`clone_tree_error_precedence`、`clone_tree_preserves_content_modes_links_and_holes`、`clone_tree_keeps_xattrs_and_flags_but_drops_acls_and_setuid`、`clone_tree_makes_independent_hard_links`、`clone_tree_clones_fifos_and_sockets_as_objects`、`clone_tree_leaves_a_nested_mount_as_an_empty_stub`、`clone_tree_fails_atomically_on_unreadable_entry`、`clone_tree_on_a_full_volume_leaves_no_destination`、`clone_file_rejects_directories_and_special_files`、`clone_file_clones_symlinks_as_symlinks`、`name_over_255_utf16_units_is_invalid_input`、`path_of_1024_bytes_is_invalid_input`、`clone_is_copy_on_write_and_isolated`、`clone_then_small_write_allocates_only_the_touched_extent`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-apfs --test clone`
Expected: FAIL（`clone_tree`、`clone_file` 还没定义，编译不过）

- [ ] **Step 3: 写让测试通过的最少实现**：`clone_tree` 照 §5.2 的九步，检查顺序不能换（dst 已存在、父目录缺失先于 `same_volume`）；`clone_file` 照 §5.3；flags 只传 `CLONE_NOFOLLOW | CLONE_NOOWNERCOPY`；失败后 `lstat(dst)` 填 `dst_exists`；crate 里不出现 `std::fs::copy`、`copyfile`，Review Focus 1 的跨卷拒绝测试里的可用空间对比是兜底
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-apfs --test clone`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-apfs/src/clone.rs crates/strata-apfs/src/lib.rs crates/strata-apfs/tests/clone.rs
git commit -m "feat(apfs): clone_tree and clone_file with volume checks and no copy fallback"
```

### Task 3: 根之下的解析、`open_rooted`、`open_rooted_file` 与测试接缝

**Files:**
- Create: `crates/strata-apfs/src/rooted.rs`（`resolve`，§5.4）、`crates/strata-apfs/src/seams.rs`
- Test: `crates/strata-apfs/tests/rooted.rs`

**Interfaces:**
- Consumes: Task 1 的 `error.rs`；`strata_types::RepoPath`
- Produces: `open_rooted`、`open_rooted_file`、`seams::{set_before_open, set_after_bulk_window}`（`feature = "test-seams"`）；crate 内部的 `resolve`（Task 4、5、6 复用）

- [ ] **Step 1: 写失败的测试**：§7 的 `rooted_open_opens_files_and_directories`、`rooted_open_treats_colon_and_backslash_names_literally`、`rooted_open_fifo_is_nonblocking_and_symlinks_are_refused`（子进程加 20 秒死线）、`rooted_open_rejects_a_directory_swapped_for_a_link`、`rooted_open_keeps_the_parent_handle_after_a_swap`、`rooted_open_pins_the_root`、`rooted_open_rejects_a_final_symlink_swap`、`rooted_open_file_refuses_a_fifo_swapped_in`、`rooted_open_error_codes`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-apfs --features test-seams --test rooted`
Expected: FAIL（`open_rooted` 还没定义，编译不过）

- [ ] **Step 3: 写让测试通过的最少实现**：`resolve` 按 §5.4：根用 `O_DIRECTORY | O_NOFOLLOW` 打开，每一级 `openat(O_DIRECTORY | O_NOFOLLOW)`，每次 `openat` 之前调一次 `before_open`，手里只留当前一级的句柄；`ENOTDIR` 之后用 `fstatat(AT_SYMLINK_NOFOLLOW)` 区分 `reason`；终点用 `O_RDONLY | O_NOFOLLOW | O_NONBLOCK`，`fstat` 后拒绝设备；`open_rooted_file` 在此之上要求 `S_ISREG`。不用 `O_NOFOLLOW_ANY`
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-apfs --features test-seams --test rooted`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-apfs/src/rooted.rs crates/strata-apfs/src/seams.rs crates/strata-apfs/src/lib.rs crates/strata-apfs/tests/rooted.rs
git commit -m "feat(apfs): open_rooted with per-component O_NOFOLLOW and test seams"
```

### Task 4: 目的地变体

**Files:**
- Create: `crates/strata-apfs/src/rooted_write.rs`
- Test: `crates/strata-apfs/tests/rooted_write.rs`

**Interfaces:**
- Consumes: Task 2 的检查函数（src 分类、dst 形态、`same_volume`）；Task 3 的 `resolve`
- Produces: `clone_file_rooted`、`clone_tree_rooted`、`create_file_rooted`、`mkdir_rooted`、`symlink_rooted`

- [ ] **Step 1: 写失败的测试**：§7 的 `rooted_destination_refuses_a_symlinked_parent`、`rooted_destination_never_overwrites`、`rooted_destination_swap_after_resolution_stays_inside`、`mkdir_rooted_parents_semantics`、`clone_file_rooted_sets_the_requested_mode`、`clone_tree_rooted_is_atomic_and_refuses_cross_volume`、`create_file_rooted_and_symlink_rooted_basics`；并把 Task 2 的 `clone_refusals_leave_no_residue` 里 `clone_tree_rooted` 的那一半补上
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-apfs --features test-seams --test rooted_write`
Expected: FAIL（目的地变体还没定义，编译不过）

- [ ] **Step 3: 写让测试通过的最少实现**：共同部分 `resolve` 到父目录句柄 P，只用最后一个名字做一次 `*at` 调用（`clonefileat`、`openat(O_CREAT | O_EXCL | O_NOFOLLOW)`、`mkdirat`、`symlinkat`），不预先 stat，`EEXIST` 就是 `Exists`；flags 只传 `CLONE_NOFOLLOW | CLONE_NOOWNERCOPY`，不传 `CLONE_NOFOLLOW_ANY`、`CLONE_RESOLVE_BENEATH`（它们连 src 一起约束）；`mode` 屏蔽到 `0o777` 后用 `fchmodat(AT_SYMLINK_NOFOLLOW)`；`mkdir_rooted(parents = true)` 逐级建，记录 `created`，建出一级之后的失败升级为 `Unknown`；任何一步都不 `fsync`
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-apfs --features test-seams --test rooted_write --test clone`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-apfs/src/rooted_write.rs crates/strata-apfs/src/lib.rs crates/strata-apfs/tests/rooted_write.rs crates/strata-apfs/tests/clone.rs
git commit -m "feat(apfs): rooted no-follow destination variants"
```

### Task 5: `Attrs`、记录解析器、`attrs_of` 与 gen count 契约

**Files:**
- Create: `crates/strata-apfs/src/attrs.rs`（`Attrs`、`FileKind`、`unchanged_since`、`attrs_of`）、`crates/strata-apfs/src/bulk.rs`（属性请求、记录解析器，Task 6 复用）
- Test: `crates/strata-apfs/tests/attrs.rs`；`bulk.rs` 内的单元测试

**Interfaces:**
- Consumes: Task 3 的 `resolve`；Task 1 的 `error.rs`
- Produces: `Attrs`、`FileKind`、`Attrs::unchanged_since`、`attrs_of`；crate 内部的 `parse_records(buf, n) -> Vec<Result<RawEntry>>`

- [ ] **Step 1: 写失败的测试**：§7 的 `bulk_record_parser_follows_the_returned_bitmap`、`bulk_record_parser_rejects_bad_records`（都在 `bulk.rs` 里，不碰文件系统）、`attrs_unchanged_since_rules`、`attrs_of_reports_every_field`、`attrs_of_does_not_follow_links`、`gen_count_changes_on_write_not_on_touch`、`gen_count_is_unknown_while_a_writable_mapping_exists`、`gen_count_unknown_with_creator_descriptor_still_open`、`gen_count_unknown_for_a_mapping_made_by_another_process`、`read_only_and_private_mappings_keep_gen_count_known`、`gen_count_alone_is_not_a_version`、`directory_gen_count_tracks_entries_not_content`、`attrs_versions_differ_for_same_second_writes`、`same_size_write_with_restored_mtime_is_a_new_version`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-apfs --test attrs`
Expected: FAIL（`Attrs`、`attrs_of` 还没定义，编译不过）

- [ ] **Step 3: 写让测试通过的最少实现**：属性请求与记录布局照 §4.2 D，**按每条记录的返回位图解析**，用未对齐读；`GEN_COUNT` 要 `FSOPT_ATTR_CMN_EXTENDED`；位图里没有 `GEN_COUNT` 或值为 0 → `gen_count = 0`；`attrs_of` 用 `getattrlistat(FSOPT_NOFOLLOW)`；符号链接目标用 `readlinkat` 取原字节；`unchanged_since` 照 §5.5，未知永远是 `false`。多进程、映射的测试用 `fork` 或 `std::process::Command` 启动自身的子测试入口
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-apfs --lib bulk && cargo test -p strata-apfs --test attrs`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-apfs/src/attrs.rs crates/strata-apfs/src/bulk.rs crates/strata-apfs/src/lib.rs crates/strata-apfs/tests/attrs.rs
git commit -m "feat(apfs): Attrs, bulk record parser, attrs_of and the gen count contract"
```

### Task 6: `walk` 与 `walk_with`

**Files:**
- Create: `crates/strata-apfs/src/walk.rs`
- Test: `crates/strata-apfs/tests/walk.rs`

**Interfaces:**
- Consumes: Task 5 的 `bulk.rs`；Task 3 的 `resolve`
- Produces: `walk`、`walk_with`、`WalkOptions`、`Walk`

- [ ] **Step 1: 写失败的测试**：§7 的 `walk_lists_all_entries_relative_to_root`、`walk_excludes_configured_dirs`、`walk_excludes_several_dirs`、`walk_exclusion_of_a_missing_dir_is_a_noop`、`walk_exclusion_prunes_the_whole_subtree`、`walk_serves_full_and_incremental_scans`、`walk_wide_directory_is_complete_and_stable_across_buffer_sizes`、`walk_very_wide_directory_through_many_windows`、`walk_after_mass_deletion_lists_only_survivors`、`walk_returns_untouched_names_once_while_the_directory_changes`、`walk_never_follows_symlinks`、`walk_does_not_descend_into_mount_points`、`walk_reports_an_unreadable_directory_and_continues`、`walk_skips_entries_that_vanish_mid_walk`、`walk_never_follows_a_directory_swapped_for_a_link`、`walk_reports_a_swapped_root`、`walk_start_is_resolved_without_following_links`、`walk_names_are_nfc_and_openable_either_way`、`walk_depth_limit_is_an_error_item`、`walk_fits_a_low_descriptor_limit`、`low_descriptor_limit_child`（后者 `#[ignore]`，只由前者启动）
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-apfs --features test-seams --test walk`
Expected: FAIL（`walk`、`walk_with` 还没定义，编译不过）

- [ ] **Step 3: 写让测试通过的最少实现**：栈式前序遍历，照 §5.6 的七步和 `read_dir` 的五步；一个目录读完、排序之后才产出；filter 在条目到手之后、产出与下探之前调用一次；子目录用 `openat(O_DIRECTORY | O_NOFOLLOW)` 打开，`ENOENT`、`ENOTDIR`、`ELOOP` 当作它已经不在；挂载点、符号链接、特殊文件都是叶子；同时只持有栈上各 frame 的句柄；结束时对根路径 `lstat` 核对身份；`MAX_WALK_DEPTH` 与 `ERANGE` 翻倍重试照 §5.6
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-apfs --features test-seams --test walk`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-apfs/src/walk.rs crates/strata-apfs/src/lib.rs crates/strata-apfs/tests/walk.rs
git commit -m "feat(apfs): getattrlistbulk walk with pruning, bounded descriptors and root pinning"
```

### Task 7: `case_fold_eq`

**Files:**
- Create: `crates/strata-apfs/src/casefold.rs`
- Test: `crates/strata-apfs/tests/case.rs`；`casefold.rs` 内的单元测试

**Interfaces:**
- Consumes: Task 1 的 `tests/common` 的 `Image`；`unicode-normalization` 与折叠表（§9 待定）
- Produces: `case_fold_eq`

- [ ] **Step 1: 写失败的测试**：§7 的 `case_fold_eq_known_pairs`（单元测试，表驱动）、`case_fold_eq_matches_a_case_insensitive_volume`（oracle，用镜像）、`case_sensitive_volume_distinguishes_case_but_not_normalization`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-apfs --lib casefold && cargo test -p strata-apfs --test case`
Expected: FAIL（`case_fold_eq` 还没定义，编译不过）

- [ ] **Step 3: 写让测试通过的最少实现**：照 §5.8：字节相等或两边都是 ASCII 走快路径，否则比较 `NFD(full_case_fold(NFD(x)))`；折叠表选定后记入 §9 并核对 Unicode 版本 ≥ 16.0；oracle 测试的表按 §4.2 B 的类别建，再按码位区间随机抽样（固定种子，失败时打印那一对的码位）
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-apfs --lib casefold && cargo test -p strata-apfs --test case`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-apfs/src/casefold.rs crates/strata-apfs/src/lib.rs crates/strata-apfs/tests/case.rs
git commit -m "feat(apfs): case_fold_eq as Unicode canonical caseless matching, checked against a real volume"
```

### Task 8: 监听

**Files:**
- Create: `crates/strata-apfs/src/watch.rs`（流的建立、回调、`Watcher`）、`crates/strata-apfs/src/watch/translate.rs`（flags 到 `WatchEvent` 的纯函数）
- Test: `crates/strata-apfs/tests/watch.rs`；`translate.rs` 内的单元测试

**Interfaces:**
- Consumes: Task 1 的 `volume_caps`、Task 7 的 `case_fold_eq`（排除过滤在大小写不敏感的卷上用它）；`strata_types::RepoPath`
- Produces: `watch`、`watch_with`、`WatchOptions`、`WatchEvent`、`Watcher`

- [ ] **Step 1: 写失败的测试**：§7 的 `watch_flags_map_to_events`（`translate.rs` 里的单元测试，合成 flags）、`watcher_reports_file_and_directory_changes`、`watcher_paths_use_the_callers_root_and_disk_spelling`、`watcher_excludes_configured_dirs`、`dropping_the_watcher_stops_callbacks`、`watcher_reports_a_renamed_root_as_dropped`、`watcher_reports_must_rescan`（`#[ignore]`，洪泛；用 `-- --ignored` 单独跑）
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-apfs --lib translate && cargo test -p strata-apfs --test watch`
Expected: FAIL（`watch` 还没定义，编译不过）

- [ ] **Step 3: 写让测试通过的最少实现**：照 §5.7：`FSEventStreamCreate`（`FileEvents | WatchRoot | NoDefer`，latency 0.05，`SinceNow`）、私有串行队列；回调按翻译表出事件，路径前缀换回调用者的 root；一次回调一个 `Changed`；`on_event` 包在 `catch_unwind` 里，panic 后补发 `Dropped`；`drop` 时 `Stop`、`Invalidate`、`Release`，再 `dispatch_sync` 排空；不设 `IgnoreSelf`，不调 `FSEventStreamSetExclusionPaths`；`exclude` 在回调里按分量过滤。事件最多等 5 s，不断言延迟
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-apfs --lib translate && cargo test -p strata-apfs --test watch && cargo test -p strata-apfs --test watch -- --ignored watcher_reports_must_rescan`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-apfs/src/watch.rs crates/strata-apfs/src/watch crates/strata-apfs/src/lib.rs crates/strata-apfs/tests/watch.rs
git commit -m "feat(apfs): FSEvents watcher that reports drops and never claims completeness"
```

### Task 9: 验收数字（design §8）

**Files:**
- Create: `crates/strata-apfs/tests/budget.rs`
- Modify: `docs/modules/apfs.md` §5.9 与 §9（把实测写回，待定行改成定案）

**Interfaces:**
- Consumes: Task 2、5、6 的 `clone_tree`、`attrs_of`、`walk_with`
- Produces: 无新签名

- [ ] **Step 1: 写失败的测试**：§7 的 `walk_tool_sized_tree_within_the_reconcile_budget`、`clone_node_modules_sized_tree_within_the_budget`（都 `#[ignore]`，造合成树要几十秒）；同一个文件里再加一个只打印数字的 spike：`attrs_of` 与 `max_depth = Some(1)` 单目录列表的单次耗时，以及对 tool 加全部兄弟库（只读，路径由环境变量给出）并行克隆到同卷临时目录的总耗时
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-apfs --test budget -- --ignored`
Expected: FAIL（合成树的辅助函数还没写，编译不过）

- [ ] **Step 3: 写让测试通过的最少实现**：合成树的生成器（目录、小文件、符号链接的比例取自 tool 的源码树与 `node_modules`）；预算断言用 design §8 的数字（全量对账 3 s，克隆 5 s），超了只在这里失败，不影响默认的测试集；spike 的数字按 §9 的待定行写回
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-apfs --test budget -- --ignored --nocapture`
Expected: PASS（并打印耗时）

- [ ] **Step 5: Commit**

```bash
git add crates/strata-apfs/tests/budget.rs docs/modules/apfs.md
git commit -m "test(apfs): design section 8 budgets for walk and clone, with measurements written back"
```

## 9. 构建者决定记录

| 事项 | 选择 | 理由 | 日期 |
| --- | --- | --- | --- |
| 目录克隆用 `clonefile(2)` 还是 `copyfile(3)`（`man 2 clonefile` 强烈不建议把前者用于目录） | [我的决定] 目录用 `clonefile(2)` | `copyfile(COPYFILE_CLONE_FORCE)` 不支持目录；`COPYFILE_RECURSIVE\|COPYFILE_CLONE` 逐文件克隆、可退化成字节复制、不原子（§4.2 A）。tool 的 `node_modules` 实测 `clonefile` 2.4 s、`cp -Rc` 13.8 s。代价：目录克隆不是并发写入的源目录的快照，deps.md 用前后两次 `walk` 兜住 | 2026-10-22 |
| `clonefile` 的 flags | [我的决定] `CLONE_NOFOLLOW \| CLONE_NOOWNERCOPY`；不传 `CLONE_ACL`；不传 `CLONE_NOFOLLOW_ANY`、`CLONE_RESOLVE_BENEATH` | 前两个：克隆符号链接本身、克隆归调用者；ACL 不带过去。后两个同时约束 src 与 dst，src 是绝对路径或路径上有 `/tmp` 一类符号链接祖先就失败（实测）；保护改由"钉住父目录句柄、只传单个分量"实现 | 2026-10-22 |
| `same_volume` 的判法 | [我的决定] `getattrlist(ATTR_CMN_DEVID)` 加 `FSOPT_RETURN_REALDEV`，跟随符号链接 | `st_dev` 分不开封存的系统卷与数据卷（§5.1，实测） | 2026-10-22 |
| dst 的预检 | [我的决定] dst 末尾 `/`、dst 在 src 里面，在调内核之前拦成 `InvalidInput` | 内核对前者给误导的 `ENOENT`，对后者给 `EINVAL`；检查顺序与内核的 errno 优先级一致（§5.2） | 2026-10-22 |
| 克隆的权限错误 | [我的决定] `EACCES`、`EPERM`、`EROFS` → `ReadOnly`，message 写明两种可能 | 源树里有不可读的条目也是 `EACCES`（实测，无 dst）；types.md 没有更贴切的码 | 2026-10-22 |
| 复制回退 | [我的决定] crate 里没有任何复制字节的路径 | design §7；`clonefile` 自己跨卷返回 `EXDEV`，退化只发生在 `cp -c`、`copyfile`、`std::fs::copy` 这类封装里 | 2026-10-22 |
| `walk` 的形态 | [我的决定] 返回具名的 `Walk`，条目是 `Result<(RepoPath, Attrs)>`；另有 `walk_with`（`WalkOptions { start, max_depth, buffer_bytes }`，filter 同时看属性） | 单个条目的失败不能吞；overlay 的增量要列单个目录；ignore 规则要区分目录与文件；小缓冲区是 3b、3c 多窗口续读的接缝 | 2026-10-22 |
| 遍历的顺序与读取方式 | [我的决定] 前序；每个目录读完、按 `RepoPath` 字节序排序之后再产出 | 产出顺序确定；读的过程中目录变动不会让同一个名字出现两次（实测 §4.2 D）；3b 要求排序后顺序稳定 | 2026-10-22 |
| 挂载点与符号链接 | [我的决定] 挂载点产出但不进入；符号链接产出（带目标）但不跟随 | 裁定 T4；嵌套挂载点的克隆结果也是空占位，两边一致 | 2026-10-22 |
| 描述符与深度 | [我的决定] `MAX_WALK_DEPTH` 为 128；同时持有的描述符不超过深度加一 | launchd 的软限常为 256（3f 两行）；超限的子树以错误条目报告，不无声截断 | 2026-10-22 |
| 批量读的缓冲区 | [我的决定] 缺省 64 KiB，夹在 1024 到 4 MiB 之间，`ERANGE` 翻倍重试 | 实测 255 字符的名字加属性装不进 300 字节，512 字节可以 | 2026-10-22 |
| 目录项在读与打开之间消失或被换成非目录 | [我的决定] 跳过，不报错 | 与 `ENOENT` 同类，遍历本来就不是快照；报错会把正常的并发变动变成失败 | 2026-10-22 |
| `Attrs` 的形状 | [我的决定] 比计划多 `mode`（低 12 位）；`kind` 的类型叫 `FileKind`；`gen_count` 为 0 表示未知 | `chmod` 不改 gen count（§4.2 C）；不与 types.md 的 `EntryKind` 混淆 | 2026-10-22 |
| `attrs_of`、`Attrs::unchanged_since`、`volume_caps` | [我的决定] 新增 | `std::fs` 读不到 gen count；把"未知不等于未变、不能单比 gen count、版本的身份含 inode"固定成一个入口；store、session、daemon 的预检 | 2026-10-22 |
| `open_rooted` 的返回范围 | [我的决定] 普通文件、目录、FIFO（非阻塞）可以返回；符号链接、socket、设备 → `NotFile`；另有只收普通文件的 `open_rooted_file` | store、session 要用 `open_rooted` 取目录句柄（所以与 tgrep 的 `RootedDir` 不同，目录不拒绝）；类型核对交给调用方或 `open_rooted_file` | 2026-10-22 |
| 根的解析 | [我的决定] 根的祖先可以是符号链接，根的最后一级 `O_NOFOLLOW`；根之下逐级 `O_DIRECTORY \| O_NOFOLLOW`；不用 `O_NOFOLLOW_ANY` | `O_NOFOLLOW_ANY` 连 `/tmp/x` 也拒绝（§4.2 E） | 2026-10-22 |
| 目的地变体 | [我的决定] `clone_file_rooted`（`mode: Option<u32>`，屏蔽到 `0o777`）、`clone_tree_rooted`、`create_file_rooted`、`mkdir_rooted`（`parents: bool`）、`symlink_rooted`；父目录缺失不自动建；不 `fsync` | 增补 3 只规定了机制，名字与参数是我定的。`mkdir_rooted(parents)` 单独提供，使每个函数只做一件事、失败边界清楚；`mode` 屏蔽 setuid、setgid 是为了不意外造出特殊位文件 | 2026-10-22 |
| 克隆带来的 BSD 标志与扩展属性 | [我的决定] apfs 不清，原样带过来；`clone_file_rooted` 只提供 `mode` | 清不清、按什么规则清是 store.md §5.3 第 5 步和 overlay 投影的规整，规则不该在两处各写一份。已知的坑：`mode = Some(..)` 遇到带 `uchg` 的源会 `EPERM`（实测），要先清标志的调用方传 `None` | 2026-10-22 |
| `detail.reason` 词表 | [我的决定] §3 末尾列出的那一串 | types.md 只规定了 `errno` 与 `path` | 2026-10-22 |
| 监听的绑定方式 | [我的决定] 直接绑 CoreServices（`FSEventStreamCreate` 加私有串行 dispatch queue），不经 `notify` | 要拿到原始 flags，自己决定 `Dropped` 与 `MustRescan` 的归类 | 2026-10-22 |
| 监听的绑定 crate | 待定 | 约束：拿得到原始 flags 与路径，回调在自己的 dispatch queue 上，`drop` 能排空在途回调。候选：`fsevent-sys` 加 `core-foundation-sys`，或手写 `extern "C"` | — |
| 监听流的参数 | [我的决定] `FileEvents \| WatchRoot \| NoDefer`，latency 0.05 s，`SinceNow`；不设 `IgnoreSelf`；不用 `FSEventStreamSetExclusionPaths` | strata 自己的写入（restore、移动工作区）也要看见；排除 API 挡不住根上的 `Dropped`，还最多 8 个；latency 三档对丢事件没有可见影响（§4.2 F） | 2026-10-22 |
| 事件到 `WatchEvent` 的映射 | [我的决定] 见 §5.7 的表；路径前缀换回调用者传入的 root；一次回调合成一个 `Changed`；回调 panic → 截住并补发 `Dropped` | 消费者对自己持有的路径做 `strip_prefix`（FSEvents 给的是 realpath）；不因回调的 bug 静默丢事件 | 2026-10-22 |
| 监听的屏障 | [我的决定] 不提供 `fence` 或任何"等监听追上"的接口 | 实测哨兵文件做不成屏障：一口气写 50 或 200 个文件再写哨兵，40 轮里有 1 到 4 轮哨兵排到最后 5 个事件之前（两个根、同一个根都出现）；`FlushSync` 在 dispatch queue 的流上不送达积压事件；延迟 15 ms 到 3.3 s。正确性本来就不依赖监听（design §6.2、裁定 T4），overlay 用钩子带来的路径、监听提示和全量对账 | 2026-10-22 |
| 只改 xattr 或 FinderInfo 的事件 | 待定 | 现在不过滤（保守）。首个 spike 看真实噪声（`com.apple.provenance` 一类）再定是否丢掉只有 `ItemXattrMod`、`ItemFinderInfoMod` 的事件 | — |
| `case_fold_eq` 的定义 | [我的决定] D145：`NFD(toCasefold(NFD(x)))`，全量折叠，无土耳其语映射，折叠表 ≥ Unicode 16.0；以 oracle 测试作验收 | macOS 27 的 APFS 上 51 对样本零不符，含 Unicode 16 新增（§4.2 B） | 2026-10-22 |
| 折叠表的来源 | 待定 | 约束：Unicode ≥ 16.0，`case_fold_eq_matches_a_case_insensitive_volume` 全绿。候选：`icu_casemap` 的 `CaseMapper::fold_string` 加 `icu_normalizer`；`caseless`（先核对它的 Unicode 版本，旧则不用）；自己从 CaseFolding.txt 生成表 | — |
| 测试接缝 | [我的决定] 线程局部的 `set_before_open`、`set_after_bulk_window`，在 feature `test-seams` 下 | 裁定 "Test seams"：换目录、换符号链接需要在两步之间执行代码；线程局部使并行的测试互不干扰 | 2026-10-22 |
| 并行克隆 | [我的决定] apfs 的每个函数是同步的单线程调用，并行度归调用方（session 的 `new` 并行克隆各仓库） | design §8 的 5 s 要靠并行（4 路 4.2 s 完成 4 份）；crate 里不开线程池 | 2026-10-22 |
| 持久化 | [我的决定] apfs 不 `fsync` | 断电安全归调用方（`F_FULLFSYNC`，裁定 "Durability"） | 2026-10-22 |
| 断电后 `clonefile` 的原子性 | 待定 | 进程被杀时没有观察到半成品；断电没有测过。调用方的 intent 行是兜底（§6）。首个 spike 能测则测 | — |
| 新建会话的总耗时 | 待定 | 只量了 tool 的 `node_modules`（2.4 到 3.9 s）；"新建会话 < 5 s（tool + 全部兄弟库）"要用全部兄弟库、并行度 1 到 4 复测（Task 9） | — |
| `attrs_of` 与单目录列表的单次耗时 | 待定 | 估计几十微秒与亚毫秒；"checkpoint 改 50 个文件 < 200 ms"由 overlay 用这些原语达成，Task 9 量好写回 §5.9 | — |
| 平台范围 | 待定 | 本文的实测只在 macOS 27.0.1 上做过；design 写的是 macOS 26+，用到的接口（`clonefile`、`getattrlistbulk`、`FSOPT_RETURN_REALDEV`、FSEvents）都早于 26，但 §7 的契约测试要在 26 上复跑一遍确认 | — |
