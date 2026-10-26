# `apfs` 模块

**Crate**：`crates/strata-apfs` · **设计来源**：design.md §3（平台、symlink 陷阱、大小写）、§5（`apfs` 行）、§6.1（克隆）、§6.2（判变与监听）、§7（同卷）、§8（验收数字）、§9 不变量 5、6 · **矩阵**：layerfs-project-telemetry（3b）目标为 `apfs` 的 2 行，layerfs-workspace-layer（3c）3 行，layerfs-replaced（3d）1 行，layerfs-sdk-js（3e）1 行，tgrep-core（3f）18 行，共 25 行，以及理由点名 apfs 的 `—` 行 8 行、理由写"归类同上一行"的 1 行（§7 表后）

apfs 是最底层的 crate，只管 macOS 文件系统的原语，不知道会话、基线、overlay 这些概念：克隆目录和文件，在根之下不跟随符号链接地读、写、改名与删除，用 `getattrlistbulk` 批量读属性（带 gen count），把 FSEvents 的信号翻译成提示，以及 APFS 的名字规则。其余模块的判变、对账和现场处理都建在这些原语的**契约**之上，所以本文把每个原语的失败方式和边界写得很细。本文按 module-rulings.md 的 "T4 apfs" 一节、增补 1 的 gen count 条目、增补 3 的 "Rooted destinations" 与 "Durability" 条目、增补 4 与 6 的 gc guards 和"改目录的父级要写权限"两条、增补 7 的折叠键与 Layering 行、增补 8（apfs 拥有根之下的一切文件系统操作）写成，并吸收了第 1 轮评审（task-4-review.md）的 6 条 Important 与 17 条 Minor，以及第 2 轮的 9 条 Minor 和控制者随后的裁定（`CloneKind`、`make_removable`、`set_file_meta`（含目录的句柄）、`remove_tree_rooted` 返回报告而不是失败、共用的 inode 上不清标志、符号链接重建而不克隆、规范的根、挂载点上的 `rmdir`）。文中标"实测"的行为，都是 2026-10-22 在本机（macOS 27.0.1，Darwin 27.0.0，arm64）上用临时文件和 `hdiutil` 稀疏镜像（不需要 root）验证过的；计时用到的真实仓库只做了只读遍历，没有写。本模块的类型只在进程内传递（`Attrs` 进缓存，`WatchEvent` 进回调），不经 RPC 或 JSON，所以不 derive `Serialize`/`Deserialize`：types.md §2.2 的 serde 规则管 types.md 自己的类型，apfs 的进程内类型不适用 [我的决定，§9]。

## 1. 职责与边界

**负责**（design §5 的 `apfs` 行，加上裁定 T4 与增补 3 的两条）：

1. **克隆**（design §6.1、§7）：`clone_tree` 克隆目录树，`clone_file` 克隆文件或符号链接，都是 `clonefile(2)`，同卷检查在先，从不退化成复制（§5.2、§5.3）。
2. **根之下的一切文件系统操作，都不跟随符号链接**（裁定 T4、增补 3、增补 8）：调用方对工作区、staging 与 generation 树的读、写、改名、删除都经这一层，自己不再写 `*at` 调用。读：`open_root`（根自己的句柄）、`open_rooted`、`open_rooted_file`、`clone_from_rooted`（从根之下克隆出去，源这一侧同样钉住，调用方要说明源的种类 `CloneKind`）；写：目的地变体 `clone_file_rooted`、`clone_tree_rooted`、`create_file_rooted`、`mkdir_rooted`、`symlink_rooted`；改名：`rename_rooted`（`Excl`、`Swap`）；删除：`remove_rooted`（一个条目）、`remove_tree_rooted`（一整棵树，或任何一个条目：返回报告，不是失败）；另有 `make_removable`（调用方在要改名或删除封存的目录之前显式调用：先清标志、再补属主写位）、`set_file_meta`（经句柄改一个文件或目录的标志、mode、mtime；非目录只在不共用的 inode 上）、`full_fsync`（`F_FULLFSYNC` 的包装）和 `classify_io_error`（errno 到 `Code` 的公开归类）（§5.4、§6）。
3. **遍历与属性**（design §6.2、§8）：`walk` / `walk_with` 用 `getattrlistbulk` 前序遍历，`attrs_of` 点查，`Attrs` 带纳秒时间、inode 和 gen count，`Attrs::unchanged_since` 把"没被写过"的判法固定下来，`Attrs::last_changed` 给 gc 判年龄用（增补 4），`list_dir` 是单目录列表（§4.2、§5.5、§5.6）。
4. **监听**（design §6.2）：`watch` 把 FSEvents 的原始信号翻译成 `WatchEvent`：可能脏的路径、该重扫的子树、事件丢了。它只是提示，从不声称完整（裁定 T4，§5.7）。`Watcher::flush()` 是 `FSEventStreamFlushSync` 加对回调队列的排空，实测不是屏障，本文写明它保证什么、不保证什么（增补 8 "Event flush, amended"，§4.2 F、§5.7）。
5. **卷与名字规则**（design §3 的大小写行、§7）：`same_volume`、`volume_caps` 回答"clonefile 在这两处之间行不行、这个卷分不分大小写"；`nearest_existing_ancestor` 是 config.md §4.3 "最近的已存在上级"的那一条规则；`case_fold_eq` 与 `case_fold_key` 是 APFS 大小写不敏感卷上的名字同一性（§5.1、§5.8）。

**不负责**：

| 事项 | 归谁 |
| --- | --- |
| 哪些目录和路径不进遍历（`.git`、`deps_dirs`、`seed_caches`、ignore 规则）；遍历结果怎样变成 overlay 条目；改名配对、墓碑 | overlay（`filter` 闭包由调用方给，apfs 只负责剪枝） |
| 什么时候相信缓存、什么时候读内容算哈希；全量对账的节奏；监听丢失后怎样退化 | overlay（脏集合）与 daemon（调度）；apfs 只给 `Attrs::unchanged_since` 这一条判法和 `WatchEvent` |
| 调用 `same_volume`、`volume_caps` 做预检的时机（`Store::open`、`new`），以及预检失败怎样上报 | store、session；apfs 只回答问题 |
| 内容哈希、blob 的 xattr 剥离、清哪些标志与设什么 mode、mtime（改的动作是 `set_file_meta`）、`fsync` / `F_FULLFSYNC` 什么时候调、对哪些句柄调 | store（blob 规整见 store.md）与各调用方；apfs 不做持久化，克隆、改名、删除之后的落盘归调用方。apfs 只提供 `full_fsync` 这个包装，以及取目录句柄的 `open_root`、`open_rooted`（增补 3 "Durability"） |
| 什么时候删、删什么（gc 的 1 小时年龄、意图行点名的路径、`retire_dir` 先挪走再删）；`RemoveReport.stuck` 里剩下的条目怎么办（重试、留着、上报） | store、session、deps 决定；apfs 只提供删除的原语 `remove_rooted`、`remove_tree_rooted`（增补 8），不知道哪些东西该删。**调用方不自己写递归删除**：非空的树经 `store::retire_dir` 挪走，再由 `remove_tree_rooted` 删；调用方只可以 `remove_rooted(.., RemoveKind::EmptyDir)` 删自己建的空目录，或本次 restore 自己清空的目录（裁定 "Empty directories, amended"：目标位置上没有目录、每个条目都已挪走或被计划替换的那种；没清空的目录从不删，`NotEmpty` 是写竞争）；`RemoveKind::File` 只删证明是留存 checkpoint 里 blob 的逐字节副本的文件（增补 8 "Proven copies"） |
| 放宽权限（0555 的封存目录要先补属主写位才能改名到另一个父目录，带 `uchg` 的要先清标志） | 调用方显式调用 `make_removable(&handle)`（句柄由 `open_rooted` 取得，钉住对象，不经路径；**先清** `UF_IMMUTABLE`、`UF_APPEND`，**再** `fchmod` 补 u+w，顺序不能反：带 `uchg` 的目录 `fchmod` 得 `EPERM`，§4.2 H）。`rename_rooted`、`remove_rooted` 被拒绝就返回 `ReadOnly` 和 `detail.reason`，从不 `chmod`、不 `chflags`。apfs 自己只在三处改标志或权限：`remove_tree_rooted` 删它点名的那棵树的过程中，调用方调用 `make_removable` 的时候，和调用方调用 `set_file_meta` 的时候（不变量 A10，§4.2 H）；`SF_*` 系统标志从不清，共用的 inode（`st_nlink > 1` 的非目录）上也不清 |
| 克隆之后清除 BSD 标志与扩展属性、重设 mtime | store（`put_blob` 的规整，store.md §5.3 第 5 步）与 overlay（投影、restore）；apfs 的克隆原样把它们带过来，改标志、mode、mtime 的动作经 `set_file_meta`（核对是普通文件或目录，非目录再核对 `st_nlink == 1`），扩展属性的剥离留在调用方，在同一个句柄上 |
| 找出哪些进程打开着哪些文件（`OpenFiles`） | deps（`open_files`）与 session。apfs 只负责一件相关的事：报告 gen count 不可得（§4.2 C） |
| 路径字符串的语法、NFC 规整、`RepoPath` 的构造 | types（`RepoPath::try_from`） |
| 清理与回滚 | 谁也不做：apfs 失败时不自己清理、不撤销已经发生的写入，现场原样留给调用方；它删东西只在调用方点名的 `remove_rooted`、`remove_tree_rooted` 里，后者没删干净时只报告剩下什么，不重放、不清理（不变量 6，§4.3） |
| 要不要等监听追上，用冷扫（不靠事件的全量遍历，§5.7）还是接受一次工具调用的滞后 | overlay 决定并标注（增补 8 "Event flush, amended"）；apfs 只说明 `flush()` 保证什么、不保证什么（§5.7） |

## 2. 依赖

Workspace crate（types.md §8：apfs 在第 1 层，只依赖第 0 层；同层只有 store 与 gitio 依赖 apfs，增补 7 的 Layering 行，T15 改 types.md §8）：

| 依赖 | 用途 |
| --- | --- |
| `strata-types` | `RepoPath`（遍历与点查的路径形态）、`StrataError`、`Code` |

外部 crate（版本和选型交给构建者，这里只写理由）：

| 依赖 | 理由 |
| --- | --- |
| `libc` | design §11 第 2 条：macOS API 走 libc。用到 `clonefile`、`clonefileat`、`fclonefileat`、`getattrlist`、`getattrlistat`、`fgetattrlist`、`getattrlistbulk`、`openat`、`mkdirat`、`symlinkat`、`readlinkat`、`fstatat`、`fchmodat`、`fchmod`、`fchflags`、`futimens`、`faccessat`、`renameatx_np`、`unlinkat`、`fcntl`（`F_FULLFSYNC`）、`fdopendir` 与 `readdir`（`remove_tree_rooted` 读目录，也可以用 `getattrlistbulk`，构建者选）、`statfs`，测试里另有 `setrlimit`、`mmap`、`utimensat`、`chflags`。2026-10-22 查本机 cargo registry 里的 libc 0.2.191：以上函数和 `RENAME_SWAP`、`RENAME_EXCL`、`F_FULLFSYNC`、`AT_REMOVEDIR`、`O_SYMLINK`、`O_EVTONLY`、`UF_IMMUTABLE`、`UF_APPEND`、`UF_COMPRESSED`、`SF_IMMUTABLE`、`SF_APPEND`、`O_EXEC`、`UTIME_OMIT`、`AT_EACCESS`、`FSOPT_*`、`ATTR_CMN_GEN_COUNT` 都导出了，没有 `CLONE_NOFOLLOW`、`CLONE_NOOWNERCOPY`，这两个常量（0x1、0x2，§4.2 A）由 crate 自己定义。**不用 `fchflagsat`**：SDK 标它 macOS 27.0 起才有，而 design 写的是 macOS 26+，libc 也没导出；清标志一律是 `openat` 取句柄再 `fchflags`（§4.2 H） |
| CoreServices 的 FSEvents 与 libdispatch 的绑定 | 直接绑 `FSEventStreamCreate` 一族加一条私有串行 dispatch queue，不经 `notify`：本模块必须拿到每个事件的原始 flags（`MustScanSubDirs` 与 `UserDropped`、`KernelDropped` 的区别，`RootChanged`），不要第三方抽象替它们归类。用哪个绑定 crate 还是手写 `extern "C"`，交给构建者（§9） |
| `serde_json` | `StrataError.detail` 是 `serde_json::Value`（types.md §6.1），apfs 构造每个错误的 `detail` |
| `unicode-normalization` | NFC（目录项名字进 `RepoPath`）与 NFD（`case_fold_eq`、`case_fold_key`） |
| Unicode 全量大小写折叠表，版本须 ≥ 16.0 | `case_fold_key`、`case_fold_eq`。实测 macOS 27 的 APFS 已经用上 Unicode 16 新增的折叠（§4.2 B）。用 `icu_casemap`、`caseless`，还是用 CaseFolding.txt 生成表，交给构建者（§9）；验收是 §7 的 oracle 测试 |
| 开发依赖 `tempfile` | 测试的临时目录。跨卷、不支持、满盘、大小写敏感、嵌套挂载的测试另用 `hdiutil create -type SPARSE` 建的镜像（`tests/common` 的 `Image`，§8 Task 1），挂载点放在临时目录里，`Drop` 时 `hdiutil detach` 并删镜像文件 |

谁调用本模块（这张表按各调用方 2026-10-22 的草稿和 module-rulings 写成；T15 再按最终稿核对）：

| 调用方 | 用到（增补 8：对工作区、staging、generation 树的写、改名、删除一律经 apfs，调用方不自己写 `*at` 调用） |
| --- | --- |
| store | `put_blob_rooted`（overlay 的 checkpoint 捕获进 store，不是 session 的携带）：`attrs_of` 先看种类，文件经 `clone_from_rooted(.., CloneKind::File)` 克隆进 `store/tmp` 再哈希克隆体（检查之后被换成目录 → `IsDirectory`，换成 FIFO、socket、设备 → `NotFile`）；`set_file_meta` 规整 put 的临时克隆（`{ clear_flags, mode: 0600 }`，剥离扩展属性之后 `{ mode: 0444, mtime }`；store 私有区里这是选择，不是强制，裁定 "set_file_meta scope, corrected"）；`get_blob_rooted`：`clone_file_rooted(blob, root, dst, None)`，随后在 `open_rooted_file` 的句柄上 `set_file_meta`（清标志、设 mode 与 mtime；blob 写进工作区、staging 与会话根的唯一入口）；`clone_file`（`put_blob(path)` 与 `get_blob` 的按路径版本，源是 strata 自己的路径；`open` 的 clonefile 探测也用它：在 `store/tmp/` 建一个探测文件、克隆一次、删掉两个文件，克隆得 `Unsupported` 就原样透传）；按路径的 `get_blob`（只给 strata 自己的、不在会话树之下的目的地）克隆之后用 `std` 以 `O_RDONLY \| O_NOFOLLOW \| O_CLOEXEC` 打开 dst，在这个句柄上调 `set_file_meta`：句柄不是 apfs 的 `open_rooted_file` 给的，所以 `set_file_meta` 自己对种类与 `st_nlink` 的核对是唯一把关的一道；`same_volume` 与 `nearest_existing_ancestor`（`open` 的同卷检查）、`classify_io_error`；`retire_dir`：`open_rooted` 取要挪走的目录的句柄 → `make_removable` → `rename_rooted(Excl)` 进 trash → `full_fsync`（对 trash 目录的句柄，同样由 `open_rooted` 取；store 不用 `open_root`）；gc：`make_removable` 预备要挪走的封存目录，`list_dir` 与 `Attrs::last_changed`（清扫活路径与基线暂存、判 1 小时年龄，增补 4），`remove_tree_rooted` 删 trash 与 `store/tmp/` 里的**每个条目**（目录、文件、符号链接，一个调用，调用方不再按路径 `chflags`；返回 `RemoveReport`：`is_complete()` 为假就留着、下一轮再删，不当 `Unknown`），`remove_rooted(.., EmptyDir)` 删清扫之后变空的目录（`NotEmpty` 就跳过）。**`remove_tree_rooted` 只有 store 这一行用** |
| deps | `clone_from_rooted(.., CloneKind::Dir)`（`adopt`：从主 checkout 认领依赖目录，源不是 strata 自己的目录，所以不用 `clone_tree`）与 `CloneKind::File`（从工作区取安装输入）；`clone_tree_rooted`（staging 的源码树、`clone_into`：克隆进调用方的目录，跨卷时 `CrossVolume`）；`mkdir_rooted`、`create_file_rooted`、`symlink_rooted`（staging 里的安装输入与 `install.log`；建造锁：`mkdir_rooted` 建 `.locks/`，`open_rooted_file`，缺了就 `create_file_rooted`）；`rename_rooted(Excl)`（挪开、组装候选、封存：先改名，后对封存根目录的句柄 `set_file_meta(.., { mode: Some(0o555) })`，空依赖集的根目录同样；deps 自己不调 `fchmod`，裁定 "set_file_meta accepts a directory handle"，§4.2 G、H）；`open_root` 加 `full_fsync`；`walk` 与 `Attrs`（展开安装输入；认领前后的两次遍历）、`attrs_of`、`list_dir`、`open_rooted`（逐级不跟随地读安装输入，取同步与 `set_file_meta` 用的句柄）；`same_volume` 与 `nearest_existing_ancestor`（预检）；`classify_io_error`。**deps 不删东西**：`remove_rooted`、`remove_tree_rooted` 它都用不到（弃置的目录走 `store::retire_dir`） |
| session | `clone_tree_rooted`（`new` 把整个 base 目录克隆进会话根）；`clone_file_rooted`（种子文件、基线文件）；`mkdir_rooted`（种子缓存、依赖目录的父级）；`move_workspace` 的携带：`clone_from_rooted(.., CloneKind::File \| Dir)`，种类取自遍历，克隆到新树根下的私有临时名，再 `rename_rooted(Excl)` 放到位，**不经 store 的 blob 库**（裁定 "Carry is not a capture"）；种子与携带里的符号链接条目按重建的配方：`attrs_of` 取目标 → `symlink_rooted`，从不克隆（§5.4；session 的 D10）；`rename_rooted(Excl)`（基线改名进 `bases/`、携带的条目放进新树）与 `rename_rooted(Swap)`（交换工作区）；`remove_rooted(.., EmptyDir)`（删自己建的空暂存目录）；`attrs_of`、`list_dir`、`walk_with`（种子与携带的判定、找已有的基线 generation）；`volume_caps`、`same_volume` 与 `nearest_existing_ancestor`（`new` 的预检）；`case_fold_key` 与 `volume_caps` 的 `case_sensitive`（种子落点的磁盘同一性）；`open_root`、`open_rooted` 加 `full_fsync`（交换后同步会话根与父目录）；`classify_io_error`。**session 不删树**：`remove_tree_rooted` 它用不到，退役的树由 store 的 gc 删 |
| overlay | `walk` / `walk_with`、`WalkOptions`、`list_dir`、`Attrs`、`FileKind`、`attrs_of`、`unchanged_since`、`gen_count_known`；`watch_with`（`exclude` 只放 `.git`）与 `WatchEvent`、`Watcher`、`Watcher::flush`（不是屏障；daemon 经 `Tracker::flush` 调用，从不在回调里）；`volume_caps`（`case_sensitive`：磁盘身份按卷折叠）与 `case_fold_key`（大小写不敏感卷上磁盘拼写的同一性，受限目录的键前缀范围查找用它的路径性质，§5.8）；`open_rooted_file`、`open_rooted`（取临时目录 T 的句柄）、`open_root`、`full_fsync`；`classify_io_error`、`IoSide`。投影与 restore 的写：blob 由 store 的 `get_blob_rooted` 写进临时目录 T（overlay 自己不建 blob 的临时副本），基线文件用 `clone_file_rooted(.., None)` 再在 `open_rooted_file` 的句柄上经 `set_file_meta` 规整（清标志、设 mode 与 mtime；句柄是 FIFO、socket、设备，或 `st_nlink > 1` 的非目录，就什么都不改），overlay 自己再 `rename_rooted(Excl)` 或 `rename_rooted(Swap)` 放进工作区；另有 `mkdir_rooted`、`symlink_rooted`（目标取自 `attrs_of` 读到的符号链接目标）、`create_file_rooted`、`remove_rooted(.., File \| EmptyDir)`（只删自己建的空目录、本次 restore 自己清空的目录，以及证明是留存 checkpoint 里 blob 的逐字节副本的文件；`EmptyDir` 遇到非空目录得 `NotEmpty`，是写竞争，§6） |
| gitio | 根随场合不同：影子仓库目录（`new` 之后它是 agent 可写的地方，Addendum 8 clarification）里的管理目录 `worktrees/<n>/` 与它里面的四个文件，含 index 的副本：`attrs_of`、`mkdir_rooted`、`create_file_rooted`、`open_rooted`；`tree_dir`：工作区与 staging 树根下的 gitfile（`create_file_rooted`）、attach 的临时 index（`mkdir_rooted`、`open_rooted_file`、`remove_rooted`）；`export_tree` 的 `dst` 的父目录：`mkdir_rooted`、`open_rooted`、`rename_rooted(Excl)`、`remove_rooted(.., File \| EmptyDir)`（只删自己没写成的临时文件和空目录）；`shadow_drift` 读 `hooks/pre-push` 的 `open_rooted_file`；`open_root` 加 `full_fsync`（gitio 的每一次 `F_FULLFSYNC` 都经它）；`classify_io_error` 与 `IoSide`（apfs 给的句柄上的读写失败：写用 `AfterWrite`，读用 `Read`）；`PATH_MAX_BYTES`、`NAME_MAX_UTF16_UNITS`。进程内读文件（工作区或 `dir` 的 `.gitignore`、管理 index、hooks 文件、excludes 文件）走 `open_rooted_file`（FIFO → `NotFile`，不会挂住；裁定 "gitio reads files through apfs"，gitio 下一次修改时落实）。gitio 不用 `remove_tree_rooted`，也不用 `case_fold_eq`：git 身份只折叠 ASCII（§5.8） |
| search | 读工作区文件的内容与属性：`open_rooted_file`、`attrs_of`、`walk`（具体以 search.md 为准） |
| daemon | 启停每个 tracked 工作区的 `Watcher`（由 overlay 的 `Tracker` 持有）；启动时用 `volume_caps` 预检 |

## 3. 公开接口（Rust）

跨模块类型只用 types.md 的（`RepoPath`、`StrataError`、`Code`）。`Attrs`、`FileKind`、`WatchEvent`、`VolumeCaps`、`WalkOptions`、`WatchOptions`、`Walk`、`Watcher`、`RenameMode`、`RemoveKind`、`CloneKind`、`FileMeta`、`RemoveReport`、`Stuck`、`StuckReason`、`IoSide` 是本模块自有的公开类型，定义在这里；别的模块按名字使用。这些类型只在进程内传递，不 derive serde（types.md §2.2 管的是 types.md 自己的类型，这里不适用，§9）；没有哪个类型进 `StrataError.detail`：`detail` 是 apfs 手写的 JSON 对象，键见下面的"`detail` 的形状"。

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
/// walk 与 remove_tree_rooted 允许的最大深度，从起点目录算。超出的子树以错误条目（walk）或 stuck 条目（remove_tree_rooted）报告，也钉住同时持有的描述符数（§5.6）。
pub const MAX_WALK_DEPTH: u32 = 128;
/// getattrlistbulk 的缺省缓冲区，以及 `WalkOptions.buffer_bytes` 夹取的范围（§5.6）。
/// 下限 1024 的理由：最长的一条记录是 255 个 UTF-16 码元的名字（BMP 字符每个 3 字节，最多 765 字节加结尾的 NUL）加上请求的全部属性，约 860 字节（§5.6），1024 装得下；再小就可能 `ERANGE` 到永远。
pub const DEFAULT_BULK_BUFFER_BYTES: usize = 64 * 1024;
pub const MIN_BULK_BUFFER_BYTES: usize = 1024;
pub const MAX_BULK_BUFFER_BYTES: usize = 4 * 1024 * 1024;
/// FSEvents 的 latency 参数（秒）；配合 NoDefer：第一个事件立即送出，其后的在 50 ms 内合并（§5.7）。
pub const WATCH_LATENCY_SECS: f64 = 0.05;
/// Attrs.gen_count 的"不可得"值：卷不支持、内核没返回、记录走了 ERROR 补取分支，或文件正被以写权限打开的描述符做了共享映射（§4.2 C）。使用者见到它就改读内容哈希。
pub const GEN_COUNT_UNAVAILABLE: u64 = 0;
/// RemoveReport.stuck 最多记多少条（防止一棵全是受保护条目的大树把报告撑爆）；总数在 `stuck_total` 里。
pub const MAX_REPORTED_STUCK: usize = 1000;

// ───────────── errno 归类 ─────────────

/// 一个 I/O 失败发生在哪一侧，决定权限类 errno 的去向，以及"已经改了一部分"之后的失败怎样一律变成 Unknown（types.md §6.3）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IoSide {
    /// 读操作：EACCES、EPERM 是 Unknown（types.md §6.3 只有写操作的权限错误才是 ReadOnly）。
    Read,
    /// 写操作，这次调用还什么都没改：EROFS、EPERM、EACCES → ReadOnly。
    Write,
    /// 已经改了一部分之后的失败：不论 errno 是什么都是 Unknown（不变量 6）。
    AfterWrite,
}

/// errno → StrataError 的归类（表见 §4.2 I）：types.md §6.3 的表，加 apfs 的细化（EINVAL → InvalidInput 等）。
/// apfs 自己的每个函数都用它；别的模块对 apfs 给的句柄做 I/O 失败时也用它，不再各写一张表。两个典型用法：
/// 在 `open_rooted` 返回的句柄上 `read` 失败 → `IoSide::Read`；在 `create_file_rooted` 返回的句柄上 `write_all` 失败 → `IoSide::AfterWrite`——
/// 文件已经建出来了，不能再当"什么都没改"，所以不论 errno（哪怕是写侧会给 `ReadOnly` 的 `EROFS`、`EACCES`）都是 `Unknown`，现场留着一个文件。
/// `nearest_existing_ancestor` 返回的 `io::Error` 同样交给它：`classify_io_error(&e, IoSide::Read, path)`。
/// `detail` 带 `errno`（数字）与 `path`；EROFS → `reason = read_only_volume`，写侧的 EACCES、EPERM → `reason = no_permission`。
/// 纯函数，不碰文件系统：要靠 `fstatat` 才分得清的情形（`unlinkat` 一个目录得到 EPERM，带标志的 EPERM）由各个操作自己在调用它之前处理，所以这里 EPERM 只有 `no_permission` 一种说法。
/// 没有 `raw_os_error` 的 `io::Error` → Unknown。
pub fn classify_io_error(err: &io::Error, side: IoSide, path: &Path) -> StrataError;

// ───────────── 卷 ─────────────

/// a、b 是否在同一个卷上，即 clonefile 能不能在两者之间工作。两个路径都跟随符号链接。
/// 用 getattrlist(ATTR_CMN_DEVID, FSOPT_RETURN_REALDEV) 比较，不用 st_dev：封存的系统卷和数据卷的 st_dev 相同（§5.1）。
/// 路径不存在 → io::ErrorKind::NotFound。
pub fn same_volume(a: &Path, b: &Path) -> io::Result<bool>;

/// 从 path 往上找第一个存在的路径（path 自己存在就是它自己）并返回：config.md §4.3 的"最近的已存在的上级目录"。
/// `paths.root` 还不存在时，同卷检查拿它的最近已存在上级去比：`same_volume(&nearest_existing_ancestor(root)?, main_checkout)`。
/// path 必须是绝对路径（否则 `io::ErrorKind::InvalidInput`）。判断存在用跟随符号链接的 `stat`：`ENOENT`（含悬空符号链接）、`ENOTDIR` 都继续往上爬，别的错误（`EACCES` 等）原样返回。
/// 要把返回的 `io::Error` 变成 `StrataError` 的调用方用 `classify_io_error(&e, IoSide::Read, path)`：读侧的 `EACCES` 是 `Unknown`（`detail.errno` 带原值）；相对路径的 `InvalidInput` 没有 `raw_os_error`，也是 `Unknown`，那是调用方的 bug，不是要处理的情形。
/// 返回值是 path 的前缀，调用者的拼写，不做 realpath；path 的某一级是文件时返回的就是那个文件，要目录的调用方自己核对。
pub fn nearest_existing_ancestor(path: &Path) -> io::Result<PathBuf>;

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
/// src、dst 都是调用方给的路径，内核照常解析其中的符号链接：只用于 strata 自己的目录（基线、依赖集、staging、store）。
/// 源在工作区里的克隆用 `clone_from_rooted`，目的地在工作区里的克隆用 `clone_*_rooted`（增补 3、8）。
pub fn clone_tree(src: &Path, dst: &Path) -> Result<()>;
/// 把文件或符号链接 src 克隆成 dst（dst 必须不存在）。符号链接克隆成符号链接，从不跟随。路径的约定同 `clone_tree`。
pub fn clone_file(src: &Path, dst: &Path) -> Result<()>;

// ───────────── 在根之下不跟随符号链接地读 ─────────────
// **root 必须是规范路径**（裁定 "Rooted roots are canonical"）：所有以 `root` 为参数的函数——`open_root`、`open_rooted`、`open_rooted_file`、`clone_from_rooted` 的 `src_root`、
// 下面的目的地变体、`rename_rooted`、`remove_rooted`、`remove_tree_rooted`，以及 `attrs_of`、`walk`、`walk_with`、`list_dir`——在碰文件系统之前先按原始字节检查它：
// 空串、不是绝对路径（不以 `/` 开头）、以 `/` 结尾、或含 `.`、`..` 分量（以 `/.` 结尾是后者的一种）→ InvalidInput（reason = root_not_canonical），什么都不碰；`walk`、`list_dir` 把它作为第一项 Err。
// 分量按整段比较：只有恰好是 `.` 或 `..` 的分量才拒绝，`.repos`、`.staging`、`..x`、`a..b` 都是合法的名字（gitio 的影子仓库根 `<root>/sessions/.repos/<id>/<repo>.git`、`export_tree` 的 dst 的父目录 `.staging/<hex>` 就是这样的根）。
// 不用 `Path::components()` 检查，它把结尾的 `/` 与 `.` 吃掉了。理由：结尾的 `/` 或 `/.` 让内核把最后一级当目录解析，`O_NOFOLLOW` 因此跟着符号链接走（§4.2 E，实测），
// 根的符号链接拒绝和根的钉住都会落空。config.md 的 V2 在加载时已经拒绝这样的路径文本（绝对、不含 `.`、`..`、`//`、尾部 `/`）；最后一级是符号链接的仓库 `path`，由裁定 "Repo paths in config are realpaths" 挡在加载处，调用方把 `path` 原样传进来（T15，§9）。
// `watch` 不走这一套：它对 root 做 realpath（§5.7），只有"必须是绝对路径"这一条相同（相对路径同样 InvalidInput，root_not_canonical）。

/// 打开 root 本身（O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC）：根的 dirfd 访问器（增补 8）。
/// 返回的句柄可当 dirfd 用，也是 `full_fsync(&open_root(root)?)` 的入口（根目录的改名要同步根自己）。
/// root 不规范 → InvalidInput（root_not_canonical）；根的祖先里的符号链接照常解析；根本身是符号链接 → NotDirectory（reason = symlink）；不存在 → NoSuchPath；不是目录 → NotDirectory（not_a_directory）。
pub fn open_root(root: &Path) -> Result<File>;
/// 在 root 之下，按 path 逐级 openat(O_NOFOLLOW) 打开（只读）。每一级都不跟随符号链接；FIFO 以 O_NONBLOCK 打开，所以不会挂住。
/// 能打开普通文件、目录和 FIFO；调用方用 `File::metadata()` 核对类型。符号链接、socket、设备文件 → NotFile。
/// 目录的句柄可以当 dirfd 用（`as_raw_fd()` 传给 `openat`、`full_fsync`），也可以交给 `set_file_meta`（目录的 mode、标志、mtime，例如 deps 把封存根目录设成 0555），或交给 `make_removable`：要改名或删除一个已封存的目录（0555，或带 `uchg`）的调用方（`retire_dir`、gc，增补 6）
/// 先用它取句柄再调 `make_removable(&句柄)`，不经路径，也不自己写 `fchmod`、`fchflags`。
/// 递归删除不在这里做，也不由调用方自己写：用 `remove_tree_rooted`。
pub fn open_rooted(root: &Path, path: &RepoPath) -> Result<File>;
/// 同上，但只接受普通文件：打开后 fstat 确认，目录 → IsDirectory，FIFO、设备 → NotFile。读内容的路径用这个。
pub fn open_rooted_file(root: &Path, path: &RepoPath) -> Result<File>;
/// `clone_from_rooted` 的调用方说明它要克隆的是什么（裁定 "clone_from_rooted states its source kind"）。调用方的种类来自之前的 `attrs_of` 或遍历，
/// 到真正打开之间那个路径可能已经被换成别的东西：文件换成目录，`fclonefileat` 就把一整棵树克隆进 `store/tmp`（store 第 2 轮评审的发现）；
/// FIFO 的句柄交给 `fclonefileat` 只会得到误导的 `EINVAL`。所以 apfs 对钉住的句柄 `fstat`，种类不符就在克隆之前拒绝，什么都不克隆。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloneKind {
    /// 期望普通文件：目录 → IsDirectory；FIFO、socket、设备 → NotFile（reason = fifo、socket、device）；符号链接 → NotFile（reason = symlink）。
    File,
    /// 期望目录：不是目录 → NotDirectory（reason = not_a_directory）；符号链接 → NotDirectory（reason = symlink）。
    Dir,
}
/// 把 src_root 之下的 src 克隆到 dst，`kind` 说明 src 应当是普通文件还是目录：从工作区捕获内容的唯一入口（增补 8）。
/// store 的 `put_blob_rooted`（overlay 的 checkpoint 捕获进 store）与 session 的携带传 `File`（携带的目录传 `Dir`），deps 的 `adopt` 从主 checkout 认领依赖目录传 `Dir`。
/// 源这一侧同 `open_rooted`：逐级钉住父目录，最后一个分量 `openat(O_RDONLY | O_NOFOLLOW | O_NONBLOCK)` 取得句柄，`fstat` 核对种类（上面），再 `fclonefileat(句柄, …)` 从句柄克隆。
/// 所以克隆出来的永远是被打开的那个对象：打开之后路径被换成指向根外的符号链接，也跟不过去（§4.2 A、§7 的交换测试）。
/// **符号链接不经这里**（两种 `kind` 都拒绝）：它的"内容"是目标，由调用方重建——`attrs_of(src_root, p)` 取目标，`symlink_rooted(dst_root, q, target)`（裁定 "A symlink is recreated, never cloned"）；
/// 也不能经 `clone_file_rooted` 按路径克隆：它的 src 是路径，中间的符号链接内核照常跟随，`CLONE_NOFOLLOW` 只管最后一级（§4.2 A）。
/// dst 是调用方给的任意路径，通常是 strata 自己的目录（store/tmp、携带用的私有临时名、`adopt` 的 staging）：它的祖先里的符号链接由内核照常解析，所以不能指向工作区里面；往工作区里写用 `clone_file_rooted`、`clone_tree_rooted`。
/// 其余同 `clone_file`、`clone_tree`：同卷检查在 `fclonefileat` 之前（`CrossVolume`，从不复制），dst 已存在（含悬空符号链接）→ Exists，父目录缺失 → NoSuchPath，失败时 dst 不存在。
pub fn clone_from_rooted(src_root: &Path, src: &RepoPath, dst: &Path, kind: CloneKind) -> Result<()>;

// ───────────── 在根之下不跟随符号链接地写（增补 3 的目的地变体） ─────────────
// dst 是 root 之下的相对路径：先逐级 openat(O_DIRECTORY|O_NOFOLLOW) 取得父目录的句柄，再用只带最后一个分量的 *at 调用落地。
// 父目录缺失 → NoSuchPath（需要时先 mkdir_rooted）；任何一级是符号链接 → NotDirectory，什么都不写。dst 已存在（含悬空符号链接）→ Exists，从不覆盖。

/// 把文件或符号链接 src 克隆到 root 之下的 dst。mode = Some(m) 时，克隆之后把权限位设成 m & 0o777（setuid、setgid、sticky 被屏蔽；用于 blob 的 0600 投影成 0644 / 0755）；对符号链接无效。
/// 设 mode 这一步经句柄，不经名字 [我的决定]：克隆之后在钉住的父目录上 `openat(O_RDONLY | O_NOFOLLOW | O_NONBLOCK)` 取回 dst，交给 `set_file_meta(.., { mode })`，它核对普通文件与 `st_nlink == 1`，
/// 所以 dst 在这中间被换成指向根外文件的硬链接，也改不到那个文件（§4.2 H）。这一步的任何失败都是 `Unknown`（克隆已经发生）。
/// 克隆会带来源的 BSD 标志：源带 `uchg` 时克隆也带，设 mode 会得 `EPERM`（实测）。要先清标志的调用方传 None，再对 `open_rooted_file` 取得的句柄 `set_file_meta(.., { clear_flags: true, mode, mtime })`
/// （overlay 的基线文件、store 的 `get_blob_rooted` 都这样做）。
/// src 是调用方给的 strata 自己的路径（blob、基线文件）。**来自工作区的符号链接不用它**：src 按路径解析，中间的符号链接内核照常跟随；重建它——`attrs_of` 取目标，`symlink_rooted`。
pub fn clone_file_rooted(src: &Path, root: &Path, dst: &RepoPath, mode: Option<u32>) -> Result<()>;
/// 把目录树 src 克隆到 root 之下的 dst（clonefileat）。同卷检查在先，原子。
pub fn clone_tree_rooted(src: &Path, root: &Path, dst: &RepoPath) -> Result<()>;
/// 建一个新的空文件并返回可写句柄（O_WRONLY|O_CREAT|O_EXCL|O_NOFOLLOW）。mode 受 umask 影响；要精确的位，调用方对返回的句柄 `set_file_meta(.., { mode })`，不自己 `fchmod`（新建的 inode 只有一个名字，核对必过）。
pub fn create_file_rooted(root: &Path, dst: &RepoPath, mode: u32) -> Result<File>;
/// 建目录。parents = true 时像 `mkdir -p`：缺的父级逐级建出来，已有的目录跳过，叶子已是目录也算成功。
/// 两种取值下，叶子（最后一级）已存在而且不是目录（文件，或符号链接，哪怕它指向一个目录）→ Exists；中间一级是文件或符号链接 → NotDirectory。parents = false 时叶子已存在一律 Exists。
pub fn mkdir_rooted(root: &Path, dst: &RepoPath, mode: u32, parents: bool) -> Result<()>;
/// 建符号链接，目标是 target 的原始字节（不要求 UTF-8，不能为空）。
pub fn symlink_rooted(root: &Path, dst: &RepoPath, target: &OsStr) -> Result<()>;

// ───────────── 在根之下不跟随符号链接地改名与删除（增补 8） ─────────────
// from、to、path 都是 root 之下的相对路径：各自的父级逐级 openat(O_DIRECTORY|O_NOFOLLOW) 钉住，只用最后一个名字做一次 *at 调用。
// 任何一级是符号链接 → NotDirectory，什么都不动；最后一级是符号链接时，动的是链接本身。没有"覆盖"这一档（不变量 A1）。

/// 改名的模式，对应 renameatx_np 的 flags（名字由增补 8 定）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenameMode {
    /// RENAME_EXCL：to 已存在（文件、目录、含悬空符号链接）→ Exists，什么都不动。
    Excl,
    /// RENAME_SWAP：from 与 to 都必须存在（类型可以不同），原子对调；各对象的 inode、gen count、mtime 跟着对象走，只有名字换了。
    Swap,
}

/// 把 root 之下的 from 改名为 to，单次 renameatx_np，要么完成要么什么都没动。
/// RepoPath 相同（same_path）或 to 在 from 之下（into_itself）→ InvalidInput，在调内核之前拦下。
/// 把目录改名到另一个父目录，要求对这个目录本身有写权限（它的 `..` 要改）：0555 的目录得 ReadOnly（reparent_needs_write）。
/// 所以封存是先改名、后把 mode 设成 0555（对目录的句柄 `set_file_meta`）；要把已经封存的目录挪走（gc、retire），调用方先 `make_removable(&open_rooted(..)?)`（先清标志、再补 u+w，增补 4、6）。`rename_rooted` 自己从不放宽（A10）。
/// 不 fsync：要扛住断电的调用方，改名之后对 from 与 to 的父目录各调一次 `full_fsync`（增补 3 "Durability"）；崩溃之后 from 与 to 两个位置都合法的调用方（store 的 trash）只需同步 to 的父目录（§5.4）。
pub fn rename_rooted(root: &Path, from: &RepoPath, to: &RepoPath, mode: RenameMode) -> Result<()>;

/// remove_rooted 要删哪一类东西。选错类型是错误，不会"顺便"删另一类（增补 8）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoveKind {
    /// unlinkat(fd, name, 0)：文件、符号链接本身（不跟随）、FIFO、socket、设备节点。目标是目录 → IsDirectory（内核给的是 EPERM，用 fstatat 区分，§4.2 G）。
    /// 只删调用方自己建的东西，或证明是留存 checkpoint 里 blob 的逐字节副本的文件（增补 8 "Proven copies"）。
    File,
    /// unlinkat(fd, name, AT_REMOVEDIR)：空目录。非空 → NotEmpty；不是目录（含指向目录的符号链接）→ NotDirectory；挂载点 → CrossVolume（reason = mount_point）。
    /// 调用方只用它删自己建的空目录，或本次 restore 自己清空的目录（裁定 "Empty directories, amended"：目标位置上没有目录、每个条目都已挪走或被计划替换的那种）；没清空的目录从不删，`NotEmpty` 是写竞争；非空的东西经 `store::retire_dir` 与 `remove_tree_rooted`，不自己写递归。
    /// `NotEmpty` 说明有别的写者，怎样对待归调用方（裁定 "NotEmpty from apfs"）：overlay 的 restore 当写竞争（第一次可见的写之前 `Busy`，之后 `Unknown`），store 的 gc 清扫跳过这个目录。
    EmptyDir,
}
/// 删除 root 之下的一个条目，单次改动的系统调用（`unlinkat`），失败时什么都没动。不 chmod、不清标志（A10）。
/// `EmptyDir` 在调内核之前对目标 `fstatat`：是目录而 `st_dev` 与父目录的不同（挂载点）→ CrossVolume（reason = mount_point，`detail.src_dev` 是父目录的设备号，`dst_dev` 是目标的），什么都没动；
/// `rmdir` 晚到的 `EBUSY` 同样映射，从不是 Unknown（裁定 "remove_rooted(EmptyDir) on a mount point"）。
pub fn remove_rooted(root: &Path, path: &RepoPath, kind: RemoveKind) -> Result<()>;

// ───────────── 经句柄改元数据（裁定 "Flags are cleared only on an unshared inode"、"Metadata changes through a handle"） ─────────────
// fchflags、fchmod、futimens 改的是 inode，不是名字：非目录而 `st_nlink > 1` 时，改它也改了别的名字，别的名字可能在树之外（实测，§4.2 H；
// store 的 writer 复现过：克隆出的 dst 被换成指向根外 0600 文件的硬链接，`open(O_NOFOLLOW)` 照常打开它，`fchmod 0644` 把根外的文件改成了 0644）。
// 目录没有这个问题：APFS 拒绝对目录 `link(2)`（`EPERM`，§4.2 H），目录只有一个名字。
// 所以调用方不自己调这三个系统调用，目录的句柄也一样，一律经下面两个函数；它们先 `fstat` 句柄再动手，拒绝时什么都不改。

/// 让一个已封存的目录（0555，或带 uchg / uappnd）可以改名到另一个父目录或被删掉：**先**清 UF_IMMUTABLE、UF_APPEND，**再** fchmod 补属主的写位 u+w。
/// 顺序不能反：带 `uchg` 的目录 `fchmod` 得 EPERM（实测，§4.2 H）。`file` 是 `open_rooted` 取得的句柄（钉住对象，不经路径）。
/// 取代各调用方自己写的"fchmod u+w 加 fchflags"配方（store 的 `retire_dir` 与 gc 的预备）；apfs 不在 `rename_rooted`、`remove_rooted` 里自作主张地做这件事（A10）。
/// 只在需要时才调 `fchflags`、`fchmod`：无操作的 chmod、chflags 也更新 ctime（实测，§4.2 H），而 gc 的年龄是 `max(mtime, ctime)`；已经可改名、可删除的目录不碰，返回 Ok。其它标志位（`hidden` 等）与 `SF_*` 原样保留。
/// 拒绝且什么都不改：非目录而 `st_nlink > 1` → ReadOnly（reason = hard_link）；`SF_IMMUTABLE`、`SF_APPEND` 置位（要 root，apfs 从不清）→ ReadOnly（reason = immutable，`detail.flags` 列出置位的标志名）。
/// 两步的操作：标志清了而补写位失败 → Unknown（`IoSide::AfterWrite`，标志已清）；第一步的失败按写侧归类。
pub fn make_removable(file: &File) -> Result<()>;

/// `set_file_meta` 要改什么。三项都可以不要：`FileMeta::default()` 什么都不改，只做核对。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileMeta {
    /// 清掉写回的文件（或要封存的目录）不该带的用户标志：除 `UF_COMPRESSED` 之外的全部 `UF_*`（克隆会把封存基线的 `uchg`、`hidden` 等带过来）。`SF_*` 从不清。
    pub clear_flags: bool,
    /// Some(m)：权限位设成 m & 0o777（setuid、setgid、sticky 被屏蔽）。
    pub mode: Option<u32>,
    /// Some(t)：mtime 设成 t，atime 不动。超出 APFS 能表示的范围（约 1677 年到 2262 年）的时间被内核夹到边界，不报错（§4.2 H）；`set_file_meta` 自己先把 t 夹到同样的边界，再与当前的 mtime 比较，不同才调 `futimens`，所以越界的请求只改一次，之后同样的请求什么都不调，`ctime` 不动。
    pub mtime: Option<SystemTime>,
}
/// 经句柄改一个文件或目录的元数据。先 `fstat` 句柄，要求：普通文件或目录（别的种类 → NotFile）、非目录时 `st_nlink == 1`（否则 ReadOnly，reason = hard_link；目录不核对，它只有一个名字，见上）、
/// 没有会挡住改动的 `SF_IMMUTABLE`、`SF_APPEND`（否则 ReadOnly，reason = immutable）；任何一项不满足都**什么都不改**。
/// 然后依次：清标志（`clear_flags`）、设 mode、设 mtime，每一项只在需要时才调（已是目标值的不调）。第一项改动生效之后的失败 → Unknown（`IoSide::AfterWrite`，部分已改）；之前的失败按写侧归类。
/// 调用方（store 的 `get_blob`、`get_blob_rooted`、`put`，overlay 的 restore 与 `project`，deps 封存根目录）不自己调 `fchflags`、`fchmod`、`futimens`，目录的句柄也一样（裁定 "set_file_meta accepts a directory handle"）；`make_removable` 仍是目录一侧"让它可以改名或删除"的 helper。
/// 两者的分工：`make_removable` 只为让一个已封存的目录可以改名或删除（清 `UF_IMMUTABLE`、`UF_APPEND`，补 u+w；其它标志位与 mode 的其余位原样，不动 mtime，没有 `FileMeta`）；
/// `set_file_meta` 把一个要留下来的文件或目录改成调用方指定的样子（清除 `UF_COMPRESSED` 之外的全部用户标志，设指定的 mode 与 mtime）。两者共用同一套拒绝规则（非目录的 `st_nlink`、`SF_*`）和"先清标志"的次序。
pub fn set_file_meta(file: &File, meta: &FileMeta) -> Result<()>;

/// 一个没删掉的条目。怎样归因见 §5.4 的 "stuck 原因表"（唯一的一张，别处都引用它）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stuck {
    /// 相对 root 的路径（NFC）。
    pub path: RepoPath,
    pub reason: StuckReason,
    /// 失败的 errno。没有系统调用失败的是 0：`OtherDevice`、`TooDeep`，以及 (dev, inode) 不符的 `Changed`；
    /// 凭 `st_flags` 判定、没有去试的 `Immutable`（带 `SF_*` 的目录不进入；打不开又带 `UF_*` 的目录不去 chmod）记 `EPERM`，即内核对这个删除或 chmod 本来会给的 errno（§4.2 H、§5.4 的 stuck 原因表）。
    pub errno: i32,
    /// 补充说明，只有 `Immutable` 带：仍然拦着删除的标志，名字用 `|` 连接——清不掉的 `UF_IMMUTABLE`、`UF_APPEND`，以及置位的每个 `SF_*`（`SF_IMMUTABLE`、`SF_APPEND`，从不清）；
    /// 或者 `hard_link`（共用的 inode，没去碰它的标志，A10）。其余原因为 `None`。
    pub detail: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StuckReason {
    /// 文件标志拦着删除，而 apfs 没有清它或清不掉：`UF_IMMUTABLE`、`UF_APPEND` 清不掉（取不到句柄：属主既没有读位也没有搜索位的条目，文件、目录都一样，被 ACL 拦着而带标志的目录同样；没有句柄可以清，§4.2 H）；
    /// `SF_*` 系统标志要 root，从不清；共用的 inode（非目录、`st_nlink > 1`）上不清标志（`detail = hard_link`）。`Stuck.detail` 写出是哪些。
    Immutable,
    /// EACCES / EPERM，而且没有上面的标志：ACL 禁止删除、属主不是调用者、mode 拒绝等。
    NoPermission,
    /// 嵌套挂载点（st_dev 与目标不同）：不进入，也删不掉（rmdir 得 EBUSY）。
    OtherDevice,
    /// EBUSY。
    Busy,
    /// 条目在判定类型与打开之间被换掉，或删空之后目录又有了新条目（ENOTEMPTY）：别的进程在同一棵树里写。
    Changed,
    /// 打不开或读不了目录（EACCES 修不好、EMFILE 等）。
    Unreadable,
    /// 深度超过 MAX_WALK_DEPTH。
    TooDeep,
    /// 这个条目自己的 unlink 结果不确定：EIO、ETIMEDOUT、ENXIO、ENODEV、EDEVERR 一类（设备或文件系统出了问题），条目可能已经没了，也可能还在，调用方不能假定它还在。
    UnknownOutcome,
    /// 其余 errno：unlink 确定没成功（ENOSPC、EDQUOT 等）。
    Io,
}
impl StuckReason {
    /// 蛇形名字：`immutable`、`no_permission`、`other_device`、`busy`、`changed`、`unreadable`、`too_deep`、`unknown_outcome`、`io`。调用方写日志、或把报告写进自己的 `detail` 时用它。
    pub fn as_str(&self) -> &'static str;
}
/// remove_tree_rooted 的报告。遍历开始之后 `remove_tree_rooted` 一律返回 `Ok(report)`：没删干净不是 `Err`，而是 `stuck` 里有条目，`is_complete()` 回答删干净了没有。
/// 为什么（裁定 "remove_tree_rooted reports, it does not fail"）：部分删除的结局是确定的，store 的 gc 每一轮都重试剩下的；`Unknown`（保留现场、不重放）会禁止这种重试。
/// `#[must_use]`：忽略报告就是把半删的树当成功。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[must_use = "RemoveReport says what was left behind; check is_complete()"]
pub struct RemoveReport {
    /// 已删掉的条目数（文件、符号链接、特殊文件、目录各算一个，含目标自己）。
    pub removed: u64,
    /// 清掉 UF_IMMUTABLE 或 UF_APPEND 的条目数（apfs 唯一改过的属性之一，如实报数）。
    pub flags_cleared: u64,
    /// 补了属主读写执行位（u+rwx）的目录数（另一个）。
    pub made_writable: u64,
    /// 没删掉的条目，最多 MAX_REPORTED_STUCK 条，按遇到的顺序；它们的祖先目录当然也还在，不单列。
    pub stuck: Vec<Stuck>,
    /// 没删掉的条目总数（可能大于 stuck.len()）。
    pub stuck_total: u64,
}
impl RemoveReport {
    /// 整棵树（含 path 自己）都没了：`stuck_total == 0`。
    pub fn is_complete(&self) -> bool;
}
/// 删除 root 之下的 path 这一棵树：path 可以是目录、文件或符号链接（任何种类的条目，裁定 "remove_tree_rooted takes any entry kind"），从不跟随符号链接——符号链接只删链接本身；不跨设备，自底向上。
/// 为了删掉它点名的这棵树，清掉条目的 UF_IMMUTABLE、UF_APPEND（只在不共用的 inode 上，经句柄，§5.4），给打不开的目录补 u+rwx，如实记在报告里；树之外的东西一概不碰（A10）。
/// **`Err` 只给遍历开始之前、什么都没碰的拒绝**，都是具名的码：NoSuchPath、NotDirectory（某一级父目录是符号链接或文件）、ReadOnly（目标的父目录对调用者不可写或带不可变标志）、
/// CrossVolume（reason = mount_point：目标是目录而且本身是挂载点，`detail.src_dev` 是父目录的设备号，`dst_dev` 是目标的）。
/// **遍历一开始（哪怕还一个条目都没改过）就一律返回 `Ok(RemoveReport)`**：没删掉的条目（含 unlink 结果不确定的）在 `stuck` 里带原因。增补 8 写的是"第一次 unlink 之后"，这里更早：清标志与补权限也是可见的写。apfs 不重试、不回滚。
/// **前提**：被删的树通常没有并发写者——store 的 trash 与 tmp 里已退役的东西通常如此（增补 8），但这是惯例，不是保证：cwd 在旧工作区里的 shell 会跟着旧树穿过交换进了 trash，还能继续往里写（`move_workspace` 只核对依赖目录上打开的文件，session.md §6.5）。
/// 有写者时多半只是 `Changed` 进报告；唯一经名字改 mode 的那一步（属主打不开、没有搜索位、不带标志的目录）还留着一个暴露：名字被换成指向根外文件的硬链接时，改到的是那个 inode 的 mode（§4.2 H）。活的工作区不要交给它。
pub fn remove_tree_rooted(root: &Path, path: &RepoPath) -> Result<RemoveReport>;

/// fcntl(F_FULLFSYNC)：把数据和元数据一直刷到介质（macOS 的 fsync 不保证，增补 3）。
/// 普通文件句柄、open_rooted 取得的目录句柄、open_root 取得的根句柄都行。
pub fn full_fsync(file: &File) -> Result<()>;

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
    /// ATTR_CMN_GEN_COUNT。GEN_COUNT_UNAVAILABLE（0）= 不可得（卷不支持、内核没返回、`ERROR` 记录走了 `fstatat` 补取分支，或文件正被以写权限打开的描述符做了共享映射，§4.2 C），使用者改读内容哈希。
    /// 从不单独比较，用 `unchanged_since`。
    pub gen_count: u64,
    /// 纳秒精度。ctime 是状态变更时间：改名、chmod、chflags、setxattr 更新它而不更新 mtime；clonefile 出来的对象，ctime 一律是克隆那一刻，mtime 则是来源的（文件、空目录）或克隆那一刻（有子项的目录，§4.2 A、G）。
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
    /// gen_count 是否可得（不等于 GEN_COUNT_UNAVAILABLE）。
    pub fn gen_count_known(&self) -> bool;
    /// gc 判年龄用的时刻：mtime 与 ctime 中较晚的那个（增补 4 "gc guards"）。不能只看 mtime：clonefile 出来的文件带着来源的 mtime，
    /// 改名只更新 ctime（实测，§4.2 A、G）；克隆的 ctime 是克隆那一刻，所以 `max` 对刚克隆的东西给出"现在"。目录同样适用（目录自己的两个时间）。
    pub fn last_changed(&self) -> SystemTime;
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
/// 手写，不 derive：derive 出来的 `buffer_bytes` 是 0，会被夹成 1 KiB 而不是 64 KiB。
impl Default for WalkOptions { /* start: None，max_depth: None，buffer_bytes: DEFAULT_BULK_BUFFER_BYTES */ }

/// walk 的迭代器，惰性：第一次 next() 才打开 root；root 打不开 → 第一项是 Err，然后结束。
/// `Walk<'f>: Send`（持有的 filter 是 `Send + Sync` 的引用，描述符与缓冲区都可以跨线程移动）：overlay 可以把一次扫描整个移到工作线程上 [我的决定]。
pub struct Walk<'f> { /* 私有 */ }
impl Iterator for Walk<'_> {
    type Item = Result<(RepoPath, Attrs)>;
}

/// 前序遍历 root 之下的一切（不含 root 自己），不跟随符号链接，不进入挂载点。
/// **顺序**：前序——一个目录条目之后紧跟它的整棵子树；同一目录里的条目按 RepoPath 的字节序（NFC 之后）。所以整体**不是**全路径的字节序：
/// `foo`、`foo/x`、`foo-bar`（全路径的字节序是 `foo`、`foo-bar`、`foo/x`，因为 `-` 排在 `/` 前面）。要与 BaseManifest 的 `BTreeMap` 逐项对账的调用方，收进 `BTreeMap` 再比，或按键查找，不要假定产出是全局有序的。
/// filter 对每个条目（文件、目录、符号链接）调用一次，参数是相对 root 的路径；返回 false = 丢掉这个条目和它的整棵子树（不产出，也不读）。
pub fn walk<'f>(root: &Path, filter: &'f (dyn Fn(&Path) -> bool + Send + Sync)) -> Walk<'f>;
/// 同上，带选项，并且 filter 同时看到属性：ignore 规则要区分目录和文件，剪枝也可以按类型。
/// `walk(root, f)` 就是 `walk_with(root, &WalkOptions::default(), &|p, _| f(p))`。
pub fn walk_with<'f>(root: &Path, opts: &WalkOptions, filter: &'f (dyn Fn(&Path, &Attrs) -> bool + Send + Sync)) -> Walk<'f>;
/// 列出 root 之下目录 dir 的直接子项（dir = None 是 root 自己）：就是 `walk_with`，start = dir、max_depth = Some(1)、全部放行。
/// 子目录作为 Dir 条目产出但不读；条目按 RepoPath 的字节序；名字是 NFC 之后、大小写照磁盘上的样子
/// （overlay 在大小写不敏感的卷上用它找磁盘上的拼写，再用 case_fold_eq 或 case_fold_key 比较）。
pub fn list_dir(root: &Path, dir: Option<&RepoPath>) -> Walk<'static>;

// ───────────── 监听 ─────────────

/// 监听事件。回调在 Watcher 私有的串行队列上被调用：不得阻塞，不得 panic。回调期间不得对任何一个 `Watcher` 调 `flush`，也不得同步 drop 任何一个 `Watcher`（包括别的 `Watcher`）：这个判断按线程判，不按 Watcher 判（见 `Watcher`）。`dispatch_sync` 到回调正在跑的那条队列会让进程 `SIGTRAP`（见 `Watcher::flush`）。在回调里 drop 自己的 `Watcher` 是允许的，走异步路径（见 `Watcher`）。
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

/// 监听句柄。drop 时停止流。
/// **回调的状态只有 `Watcher` 一个所有者**（裁定 "Watcher state has one owner"）：`FSEventStreamContext` 的 `info` 指向堆上的一块状态，`closed` 标志、`on_event` 闭包（连同它捕获的一切）和排除用的数据都放在里面，不放在 `Watcher` 值里；
/// `retain`、`release`、`copyDescription` 都是 NULL，所以 FSEvents 除了把 `info` 交给回调，从不碰它。这块状态恰好释放一次，闭包与它的捕获物因此恰好 drop 一次，时点固定：`Stop`、`Invalidate`、`Release` 和排空流的队列之后的 `Watcher::drop` 里；在回调里 drop 的，是随后排在当前回调后面的那个块里。
/// 消费者可以依赖这一点，例如闭包里的 channel `Sender` 在 `Watcher` drop 的时候断开。为什么不把释放交给 FSEvents 的 `context.release`：FSEvents 在最后一次 `FSEventStreamRelease` 之后还让流活着，非 NULL 的 `release` 要到那之后才被调用，也就是在 `Watcher` 已经释放状态之后（实测，§4.2 F：普通 drop 每次如此，回调里 drop 时前后不定）；两个所有者就会释放两次。
/// 同步排空只在线程局部标志为假时跑（不在任何一个 `Watcher` 的回调里）：先置 `closed`（已排队的回调到了就直接返回，不进 `on_event`），`Stop`、`Invalidate`、`Release`，再对私有队列 `dispatch_sync` 一个空块排空在途的回调，然后释放状态与队列；返回之后不会再有回调。标志为真时，哪怕回调属于别的 `Watcher`，这次 drop 也走下一段已经写明的异步路径。
/// **在回调里 drop 自己的 `Watcher`**（例如回调持有的最后一个 `Arc<Tracker>` 在回调里被丢掉）[我的决定]：`dispatch_sync` 到自己所在的串行队列会让 libdispatch 以 `SIGTRAP` 杀掉整个进程（实测，§4.2 F），所以这条路径不排空：
/// 置 `closed`，`Stop`、`Invalidate`（在回调里调用它们没有问题，实测），把最后的 `Release` 与状态的释放交给私有队列上的 `dispatch_async`。串行队列保证这个块在当前回调返回之后才跑，所以正在跑的回调仍在用的状态（闭包、`Arc`）此前不会被释放。
/// 这时 drop 先返回、当前回调继续跑完；之后不会再有第二次回调进入 `on_event`。
/// "在回调里"这个判断（`drop` 与 `flush` 共用）按线程判，不按 Watcher 判：任何一个 strata `Watcher` 的回调在执行期间，线程局部标志为真（回调返回或 panic 时清掉）。在这个窗口里，对**任何一个** `Watcher` 调 `flush` 都是无操作（debug 构建 `debug_assert!`），drop **任何一个** `Watcher` 都走下面的异步路径。这是故意的：按 Watcher 精确判断的话，A 的回调可以对 B 做 `dispatch_sync` 排空，同时 B 的回调对 A 做同样的事，两条私有队列互相等，死锁。单向是安全的（A 的回调里排空 B 正常返回，§4.2 F），互相等不是，所以不用那条路（控制者对评审 n5 的更正，2026-10-23）。
/// `Watcher: Send + Sync` [我的决定]：overlay 的 `Tracker` 持有它，flush 可以从任何线程调。
pub struct Watcher { /* 私有 */ }
impl Watcher {
    /// FSEventStreamFlushSync，然后对流的私有队列 dispatch_sync 一个空块（增补 7、8 "Event flush, amended"）。
    /// **保证的**：flush 返回时，排空之前已经排进队列的回调都已执行完。排空之后新到的回调不在保证之内（它可能在 flush 返回之前就开始执行）。`FlushSync` 的文档语义是把客户端里还在等 latency 计时器的事件推出来，实测没有观察到它起了可见的作用，不对它作承诺。
    /// **不保证的**：返回前已经完成的写入，其事件已经到齐。实测（§4.2 F）FlushSync 约 0.05 ms 就返回，写 20 到 50 个文件之后立刻调，返回时只送达 11% 到 35%；
    /// 其余的事件还在 fseventsd 那一头，id 甚至比调用前的 `FSEventsGetCurrentEventId()` 还小；写完睡 5 到 100 ms 再调也一样（0/500）。所以这是"尽力缩小窗口"，**不是屏障**，apfs 不提供屏障（§9）。
    /// 调用方要怎样做是 overlay 的决定：Before* 与 Manual 检查点做不靠事件的全量扫描（"冷扫"：全量遍历，条目只靠这次遍历读到的属性经 `unchanged_since` 担保，从不靠事件流，也不重读每个文件的内容；裁定 "Cold scan"）；ToolCall 检查点用钩子给的路径加 `flush`，没有路径的工具（Bash）选一次温扫或记下"晚到的写入落进下一个检查点"（§5.7）。
    /// 同时被多个线程调用时互相串行。在任何一个 strata `Watcher` 的回调里调用（线程局部标志为真）都是无操作：debug 构建 `debug_assert!` 拦住，release 构建立即返回、什么都不做。自己的回调里 `dispatch_sync` 到自己的队列会 `SIGTRAP`（实测）；别的 Watcher 的回调里单向排空对方是安全的（§4.2 F），但两个回调同时互相排空会互相等待、死锁。所以回调里对任何一个 Watcher 都不做同步等待，不把"别互相 flush"留给调用方（控制者对评审 n5 的更正）。
    pub fn flush(&self);
}

pub fn watch(root: &Path, on_event: impl Fn(WatchEvent) + Send + 'static) -> Result<Watcher>;
pub fn watch_with(root: &Path, opts: &WatchOptions, on_event: impl Fn(WatchEvent) + Send + 'static) -> Result<Watcher>;

// ───────────── 名字规则 ─────────────

/// 大小写不敏感卷上的名字的折叠键：Unicode 规范无大小写匹配（D145）的键 NFD(toCasefold(NFD(s)))，全量折叠，不含土耳其语映射。折叠表 ≥ Unicode 16.0；纯 ASCII 走 `to_ascii_lowercase` 快路径。
/// 与 `case_fold_eq` 一致，由构造保证：`case_fold_eq(a, b) ⇔ case_fold_key(a) == case_fold_key(b)`（增补 7）。
/// 键只存在内存里：它不是 NFC，所以不是 RepoPath，不能当路径用，也不能落盘或进 RPC（折叠表升级会改变键）。
/// 按折叠身份做成员判断（集合、映射）用键，建一次、查多次，不做两两比较。
/// 路径性质（裁定 "case_fold_key prefix property"）：`case_fold_key(a + "/" + b) == case_fold_key(a) + "/" + case_fold_key(b)`——`/` 是 NFD 不会重排的 starter，折叠把它映射到自己（§5.8）。
/// overlay 对受限目录按键的前缀做范围查找，靠的就是它。
pub fn case_fold_key(s: &str) -> String;
/// APFS 大小写不敏感卷上两个名字是不是同一个：`a == b || case_fold_key(a) == case_fold_key(b)`。
/// 大小写敏感的卷（`VolumeCaps.case_sensitive`）不该调用它：那里 `a` 和 `A` 是两个文件，RepoPath 的字节相等就是同一个名字。
/// 它回答的是"磁盘上这两个名字是不是同一个文件"，不回答"git 把这两个路径当同一个吗"（§4.2 B 的"两个问题"）。
pub fn case_fold_eq(a: &str, b: &str) -> bool;

// ───────────── 测试接缝（只在 feature = "test-seams" 下编译，绝不进发布的二进制） ─────────────

#[cfg(feature = "test-seams")]
pub mod seams {
    use std::ffi::OsStr;

    /// 本线程里，在每一次对"root 之下某个名字"的 `*at` 系统调用之前调用：逐级 `openat`（打开中间目录；`open_rooted`、`open_rooted_file`、
    /// `clone_from_rooted` 打开终点那一次、`attrs_of` 取属性那一次也算）；目的地变体、`rename_rooted`、`remove_rooted`、`clone_from_rooted` 的 `fclonefileat` 落地的那一次调用（`clone_file_rooted` 带 `Some(mode)` 时，克隆之后取回 dst 的那一次 `openat` 也算，step 接着数）；
    /// `remove_tree_rooted` 打开每个子目录那一次（逐条目的 `unlinkat` 不触发）；walk 打开子目录那一次，walk 解析 `start` 的每一级。
    /// step 是这次公开调用里从 0 起的序号（`rename_rooted` 先走 from 的各级，再走 to 的各级，最后是 renameatx_np 那一次；`clone_from_rooted` 先走 src 的各级和打开终点那一次，最后是 fclonefileat 那一次），name 是即将处理的名字。
    /// 回调里可以改文件系统：把目录换成符号链接、把文件换成 FIFO、删掉一个目录。线程局部，不影响别的线程里的测试。None = 清除。
    pub fn set_before_open(hook: Option<Box<dyn Fn(usize, &OsStr)>>);
    /// 本线程里，`make_removable` 与 `set_file_meta` 在每一次真要调用的改动之前调用（已是目标值而跳过的步骤不触发）：参数是这次调用里从 0 起、只数实际要做的改动的序号，和改动的种类。测试借此在两步之间改文件的状态，造出确定的"第一项改动之后失败"。
    /// 它是另一个钩子，不与 `set_before_open` 共用参数 [我的决定]：`before_open` 的 `name` 永远是目录项的名字，这里的参数是枚举，所以一个叫 `mode` 的 dst 不会被当成"改 mode"那一步，按名字过滤的钩子也不会被它误触发。
    /// 序号每次 `make_removable` 或 `set_file_meta` 调用各自从 0 起，与 `before_open` 的 step 互不相干：`clone_file_rooted(.., Some(mode))` 里的 `set_file_meta` 照常触发它（mode 真要变时恰好一次 `(0, Mode)`），克隆与取回 dst 的那两次 `before_open` 仍按 `before_open` 自己的 step 数。线程局部，不影响别的线程里的测试。None = 清除。
    pub fn set_before_meta(hook: Option<Box<dyn Fn(usize, MetaChange)>>);
    /// `set_before_meta` 的改动种类，按 `set_file_meta` 的次序：清标志、设 mode、设 mtime（`make_removable` 只会有 `Flags` 与 `Mode`）。
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum MetaChange { Flags, Mode, Mtime }
    /// 本线程里，walk 每读完一次 getattrlistbulk（一个窗口）之后调用，参数是这个窗口里的记录数；用来在两个窗口之间改目录。
    pub fn set_after_bulk_window(hook: Option<Box<dyn Fn(usize)>>);
}
```

**与计划签名的差别**：

- 计划写的 `walk(root, filter) -> impl Iterator<Item = (RepoPath, Attrs)>` 在这里的条目带 `Result`，返回具名的 `Walk`：遍历会在单个条目上失败（目录不可读、名字不是 UTF-8、深度超限、根被换掉），吞掉它们会让调用方拿到一份看上去完整的残缺清单 [我的决定]。`filter` 的一参形态保留；要看属性的调用方用 `walk_with`。filter 要求 `Send + Sync`，`Walk` 因此是 `Send`（评审 M8，[我的决定]）。
- `Attrs` 比计划多一个 `mode`（overlay 要权限位，chmod 不改 gen count，§4.2 C），`kind` 的类型是 `FileKind`（符号链接带目标），不叫 `Kind` 是为了不与 types.md 的 `EntryKind` 混淆 [我的决定]。`gen_count` 保持计划的 `u64`，0 表示未知。
- `watch` 的签名保持；`watch_with` 加了 `exclude` [我的决定]。回调收到的 `WatchEvent` 就是计划的三个变体。
- 裁定 T4 加的 `open_rooted` 返回 `Result<File>`（计划里写的是 `-> File`）。
- 计划之外新增，各有调用方（§2 的表）：`open_rooted_file`；增补 3 要求的目的地变体 `clone_file_rooted`、`clone_tree_rooted`、`create_file_rooted`、`mkdir_rooted`、`symlink_rooted`（名字与参数是我定的 [我的决定]，裁定只说了"create/clone a file or tree at a path resolved component by component"）；`attrs_of`（std 的 `fs::metadata` 读不到 gen count）；`Attrs::unchanged_since`；`volume_caps`；`walk_with` 与 `WalkOptions`；`seams`。
- 修订轮（同日，增补 3 的 "Durability"、增补 4 的 gc guards 和 overlay 草稿的需求）又加了：`rename_rooted` 与 `RenameMode`、`full_fsync`、`list_dir`、`Attrs::last_changed`、`Attrs::gen_count_known` 与 `GEN_COUNT_UNAVAILABLE`，以及当时的 `unlink_rooted`、`rmdir_rooted`。overlay 的 restore 要 `RENAME_EXCL`、`RENAME_SWAP`、`unlinkat`、`mkdirat`、`symlinkat` 都经根之下钉住的父目录；gc 要 `max(mtime, ctime)`；扛断电的改名要 `F_FULLFSYNC` [我的决定]。
- 第 1 轮评审（同日，增补 7、8）：名字按增补 8 改为 `RenameMode::Excl`（原 `Exclusive`）、`remove_rooted(root, path, RemoveKind::{File, EmptyDir})`（取代 `unlink_rooted`、`rmdir_rooted`）；新增 `remove_tree_rooted` 与 `RemoveReport`、`clone_from_rooted`、`open_root`、`classify_io_error` 与 `IoSide`（名字由 apfs 定 [我的决定]）、`nearest_existing_ancestor`、`case_fold_key`、`Watcher::flush`。
- 第 2 轮评审与控制者裁定（同日，增补 8 之后）：`clone_from_rooted` 加 `kind: CloneKind`（在克隆之前核对种类；store 第 2 轮评审发现文件被换成目录时整棵树被克隆进 `store/tmp`）；`remove_tree_rooted` 遍历一开始就一律 `Ok(RemoveReport)`（`#[must_use]`、`is_complete()`、`Stuck.detail`、`StuckReason::UnknownOutcome` 与 `as_str`），目标可以是任何种类的条目，不再有 `Err` 形态的报告，也就没有 `to_detail`、`from_error`；新增 `make_removable`、`set_file_meta` 与 `FileMeta`（经句柄改元数据，共用的 inode 一律不动）；根必须规范（`root_not_canonical`）；`remove_rooted(EmptyDir)` 与 `remove_tree_rooted` 遇挂载点 → `CrossVolume`（`mount_point`）；`Watcher` 的 `Drop` 在回调里也安全；`clone_file_rooted` 的 `mode` 改经句柄设置。
- 没有 `fence` 或任何"等监听追上"的接口：实测哨兵文件、事件 id 规则、哨兵加 `FlushSync` 加排空、从旧 id 重放历史再等 `HistoryDone`，四种候选屏障都做不成（§4.2 F、§9）。`Watcher::flush` 只是尽力缩小窗口，文档里写明它保证什么、不保证什么。

**`detail` 的形状**：I/O 失败一律带 `errno`（数字）和 `path`（字符串）（types.md §6.3）。apfs 另用这几个键：`reason`（短标识）、`dst_exists`（布尔：失败之后 dst 是否存在）、`src_dev` 与 `dst_dev`（`CrossVolume`；`mount_point` 的拒绝里是父目录与目标的设备号）、`fs_type`（`Unsupported`）、`created`（`mkdir_rooted` 部分建好的目录）、`from` 与 `to`（`rename_rooted` 的两个路径，`path` 是出问题的那一个）、`flags`（`make_removable`、`set_file_meta` 因 `SF_*` 拒绝时，置位的标志名数组）。`RemoveReport` 不进 `detail`（遍历开始之后没有 `Err`）；`StuckReason::as_str` 的蛇形名字是调用方自己写日志或 `detail` 时的词表。`reason` 的取值：`trailing_slash`、`dst_inside_src`、`name_too_long`、`path_too_long`、`empty_target`、`symlink`、`not_a_directory`、`fifo`、`socket`、`device`、`too_deep`、`root_changed`、`root_not_canonical`、`unreadable_entry`；改名、删除与改元数据另有 `same_path`、`into_itself`、`from_missing`、`to_missing`、`reparent_needs_write`、`immutable`、`hard_link`、`no_permission`、`read_only_volume`、`busy`、`mount_point`。

**各函数的语义与可能返回的 `Code`**（"根"指 `root` 参数：它的祖先可以是符号链接，内核照常解析；它的最后一级用 `O_NOFOLLOW` 打开，所以根本身是符号链接 → `NotDirectory`，`reason = symlink`。**root 必须是规范路径**：每个以 `root` 为参数的函数都在碰文件系统之前检查，不规范 → `InvalidInput`（`reason = root_not_canonical`），下面各条不再重复；`walk`、`list_dir` 把它作为第一项 `Err`）：

- **`same_volume`**：见 §5.1。没有 `Code`，返回 `io::Result`；`store::open` 把 `false` 变成 `CrossVolume`。
- **`nearest_existing_ancestor`**：见 §5.1。没有 `Code`，返回 `io::Result`：相对路径 → `InvalidInput` 种类的 `io::Error`，`stat` 的非 `ENOENT`、`ENOTDIR` 错误原样。要变成 `StrataError` 的调用方用 `classify_io_error(&e, IoSide::Read, path)`（读侧的 `EACCES` 是 `Unknown`）。
- **`volume_caps`**：见 §5.1。`NoSuchPath`、`Unknown`。`fs_type` 不是 `apfs` 本身不是错误，由调用方决定。
- **`classify_io_error`**：纯函数，不失败；返回的 `Code` 见 §4.2 I 的表。
- **`clone_tree`**：见 §5.2。`NoSuchPath`（src 不存在，或 dst 的父目录不存在）、`NotDirectory`（src 不是目录，符号链接也算；dst 的父级不是目录）、`Exists`（dst 已存在，含悬空符号链接，含 dst 就是 src）、`CrossVolume`（`detail` 带 `src_dev`、`dst_dev`）、`Unsupported`（卷不支持 clonefile，`detail.fs_type`）、`InvalidInput`（dst 以 `/` 结尾、dst 在 src 里面、分量超 255 个码元、路径达到 1024 字节）、`ReadOnly`（`EACCES`、`EPERM`、`EROFS`：dst 的父目录不可写，**或者源树里有不可读的条目**，message 同时写明两种可能）、`Unknown`（`ENOSPC`、`EIO`、`EDEADLK`、描述符耗尽等；`detail.dst_exists` 如实写）。失败之后 dst 不存在（clonefile 原子，实测）；apfs 不删除任何东西。
- **`clone_file`**：同 `clone_tree`，差别：src 是目录 → `IsDirectory`；src 是 FIFO、socket、设备文件 → `NotFile`（`reason` 取 `fifo`、`socket`、`device`）；src 不要求是目录。
- **`open_root`**：见 §5.4。`NoSuchPath`、`NotDirectory`（不是目录；本身是符号链接时 `reason = symlink`）、`Unknown`（读取侧的 `EACCES` 等，`detail.errno` 带原值）。
- **`open_rooted`**：见 §5.4。`NoSuchPath`、`NotDirectory`（中间一级不是目录；是符号链接时 `reason = symlink`）、`NotFile`（最后一级是符号链接 `reason = symlink`、socket、设备文件）、`InvalidInput`（`ENAMETOOLONG`：名字或整条路径超限）、`Unknown`（读取侧的 `EACCES`：types.md §6.3 的表里只有写操作的权限错误才是 `ReadOnly`，`detail.errno` 带原值；其余 errno 一律 `Unknown`）。
- **`open_rooted_file`**：同上，另加 `IsDirectory`、`NotFile`（FIFO：已经以非阻塞方式打开并关掉了，不会挂住）。
- **`clone_from_rooted`**：见 §5.4。源这一侧，都在 `fclonefileat` 之前、什么都没克隆：`NoSuchPath`；`NotDirectory`（某一级父目录是文件或符号链接；`Dir` 时源不是目录，或本身是符号链接）；`IsDirectory`（`File` 而源是目录）；`NotFile`（`File` 而源是符号链接、FIFO、socket、设备）。目的这一侧与卷的码同 `clone_file`（`Exists`、`NoSuchPath`、`NotDirectory`、`CrossVolume`、`Unsupported`、`InvalidInput`、`ReadOnly`、`Unknown`）。失败之后 dst 不存在。
- **目的地变体**：见 §5.4。共同的：`NoSuchPath`（父目录缺失）、`NotDirectory`（中间一级是文件或符号链接，什么都没写）、`Exists`、`ReadOnly`（写侧的 `EACCES`、`EPERM`、`EROFS`）、`InvalidInput`（`ENAMETOOLONG`；`symlink_rooted` 的目标为空）、`Unknown`。`clone_file_rooted` 与 `clone_tree_rooted` 另有 `clone_file`、`clone_tree` 的源侧与卷的码（`IsDirectory`、`NotFile`、`NotDirectory`、`CrossVolume`、`Unsupported`）。**写入开始之后的失败一律 `Unknown`**（不变量 6）：`clone_file_rooted` 克隆成功而经句柄设 mode 失败（含 `set_file_meta` 的 `hard_link`、`immutable` 拒绝：那时已经写过了，不再是具名的 `ReadOnly`），克隆出的文件留着（`dst_exists = true`）；`mkdir_rooted(parents = true)` 建了几级之后失败，已建的留着，`detail.created` 列出它们。
- **`rename_rooted`**：见 §5.4。`InvalidInput`（`same_path`、`into_itself`、`name_too_long`，以及内核的 `EINVAL`）、`NoSuchPath`（`from` 不存在，`reason = from_missing`；`Swap` 时 `to` 不存在，`to_missing`；任一方的父级不存在）、`NotDirectory`（任一方的某一级是文件或符号链接，`reason = symlink`）、`Exists`（`Excl` 时 `to` 已存在，含悬空符号链接）、`CrossVolume`（跨卷，含跨嵌套挂载点；`detail.src_dev`、`dst_dev`）、`Unsupported`（`Swap` 在不支持的文件系统上，如 HFS+；`detail.fs_type`）、`ReadOnly`（`EACCES`、`EPERM`、`EROFS`；`detail.reason` 是 `reparent_needs_write`、`immutable`、`no_permission` 或 `read_only_volume`）、`Unknown`（`ENOSPC`、`EIO`、`EBUSY` 等）。单次系统调用：失败时什么都没动。
- **`remove_rooted`**：见 §5.4。`kind = File`：`NoSuchPath`、`NotDirectory`（某一级父目录是文件或符号链接）、`IsDirectory`（目标是目录；内核对它给的是 `EPERM`，要靠 `fstatat` 区分）、`ReadOnly`（`immutable`、`no_permission`、`read_only_volume`）、`Unknown`。`kind = EmptyDir`：`NoSuchPath`、`NotDirectory`（目标是文件或符号链接，符号链接指向的地方不被碰；或某一级父目录不是目录）、`NotEmpty`（`ENOTEMPTY`：types.md 已有的码，T15 给它的"谁返回"加 `apfs`、含义放宽成"要删的目录非空"，§9）、`CrossVolume`（`reason = mount_point`：目标是挂载点，调内核之前就判出，晚到的 `EBUSY` 同样映射，不是 `Unknown`）、`ReadOnly`（同上）、`Unknown`。
- **`remove_tree_rooted`**：见 §5.4。遍历之前的拒绝，树一个字节也没动：`NoSuchPath`、`NotDirectory`（某一级父目录是符号链接或文件；目标本身是什么种类都可以）、`ReadOnly`（目标的父目录对调用者不可写或带不可变标志，`reason = no_permission` 或 `immutable`）、`CrossVolume`（`reason = mount_point`：目标是目录而且本身是挂载点）。**遍历一开始就是 `Ok(RemoveReport)`，没有别的返回**：没删掉的条目（ACL、标志、硬链接、嵌套挂载点、被换掉的、太深的、`EBUSY`、unlink 结果不确定的）都在 `stuck` 里带原因（§5.4 的 stuck 原因表），哪怕一个条目都没删成；调用方看 `is_complete()`。
- **`make_removable`**：见 §5.4。`ReadOnly`（`hard_link`：非目录而 `st_nlink > 1`；`immutable`：`SF_IMMUTABLE`、`SF_APPEND` 置位，`detail.flags` 列出；写侧的 `EACCES`、`EPERM`、`EROFS`，`reason` 的判法同 `rename_rooted`），这些都是第一处改动之前的拒绝，什么都没动；清了标志而补写位失败 → `Unknown`（标志已清）。
- **`set_file_meta`**：见 §5.4。接受普通文件与目录；拒绝，什么都没改：`NotFile`（FIFO、socket、设备、符号链接）、`ReadOnly`（`hard_link`：非目录而 `st_nlink > 1`；`immutable`：`SF_IMMUTABLE`、`SF_APPEND` 置位，`detail.flags` 列出）；第一项改动之前的 `EACCES`、`EPERM`、`EROFS` 也是 `ReadOnly`（`reason` 的判法同 `rename_rooted`）。第一项改动生效之后的失败 → `Unknown`（部分已改）。
- **`full_fsync`**：见 §5.4。`Unsupported`（`ENOTSUP`）、`Unknown`（其它 errno；持久性不确定，调用方按"没有落盘"处理）。
- **`attrs_of`**：见 §5.5。`NoSuchPath`、`NotDirectory`、`Unknown`（读取侧的 `EACCES` 等）。
- **`walk` / `walk_with`**：见 §5.6。迭代器本身不失败；起点问题是第一项 `Err`（`NoSuchPath`、`NotDirectory`）；单个条目的问题是 `Err` 项，遍历继续：`NotText`（目录项名字不是 UTF-8，APFS 上建不出这样的名字，实测 `EILSEQ`，所以只有记录解析器的单元测试到得了）、`TooLarge`（`reason = too_deep`：深度超过 `MAX_WALK_DEPTH`，增补 8，[我的决定] 码的含义宽于 types.md §6.2，§9）、`Unknown`（目录不可读，`reason = unreadable_entry`，`detail.errno`），以及末尾的 `Busy`（`reason = root_changed`：遍历中根被换掉，增补 8，同样宽于 types.md §6.2 的原意，§9）。
- **`list_dir`**：同 `walk_with` 在 `start = dir`、`max_depth = Some(1)` 时：dir 不存在或某一级不是目录是第一项 `Err`，条目问题是 `Err` 项。
- **`watch` / `watch_with`**：见 §5.7。`NoSuchPath`、`NotDirectory`（root 不是目录）、`Unknown`（FSEvents 流建不起来或启动失败）。运行中的事件丢失不是错误，是 `WatchEvent::Dropped` 与 `MustRescan`。
- **`Watcher::flush`**、**`case_fold_key`**、**`case_fold_eq`**、**`Attrs::unchanged_since`**、**`StuckReason::as_str`**、**`RemoveReport::is_complete`**：不失败。`drop(Watcher)` 也不失败（在回调里 drop 见 §5.7）。

## 4. 数据与不变量

### 4.1 本模块拥有的持久数据

无。apfs 不写任何文件、不建 SQLite、不留状态文件；唯一的进程内状态是 `Watcher`（一个 FSEvents 流加一条私有串行 dispatch queue）。它克隆出来的目录、读出来的属性，都归调用方。测试用的稀疏镜像只在测试进程里存在，`Drop` 时分离并删除。

### 4.2 平台行为契约（实测，2026-10-22，macOS 27.0.1）

这一节是实现者的依据：每条都在本机用探针程序验证过，§7 的测试把它们钉成回归。

**A. `clonefile(2)` 与 `clonefileat(2)`**

| 项 | 行为 |
| --- | --- |
| 原子性 | 目录克隆全有或全无。12 MB 镜像写满时 `ENOSPC`，源树里有不可读条目时 `EACCES`，两种失败都不留 dst |
| 保留 | 内容、mode、mtime（文件和空目录带过去的是源的 mtime，有子项的目录的 mtime 是克隆那一刻——克隆往里填条目会更新它，实测；所以 gc 判"新旧"不能只看 mtime）、**ctime 一律是克隆那一刻**（文件、空目录、有子项的目录都一样，实测；正是它让增补 4 的 `max(mtime, ctime)` 对刚克隆的东西给出"现在"，gc 的 1 小时年龄保护靠这一点成立）、扩展属性（含 `user.*` 和 `com.apple.provenance`）、BSD 标志（`uchg`、`hidden`；克隆出来的 `uchg` 文件同样不可变，对它 `chmod` 得 `EPERM`，要先清标志）、稀疏文件的洞、符号链接原样（相对、绝对、悬空、成环）、目录树里的 FIFO 与 socket（克隆出来仍是 FIFO、socket 对象） |
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
| 从句柄克隆（`fclonefileat`，`clone_from_rooted` 的依据） | 句柄用 `openat(O_RDONLY\|O_NOFOLLOW\|O_NONBLOCK\|O_CLOEXEC)` 取得，文件和目录都行（实测）：克隆保留的东西同上（文件的 mtime 是源的，ctime 是现在，`uchg` 与扩展属性带过去）。源是 FIFO、字符设备 → `EINVAL`（所以先 `fstat` 分类）；源是目录时克隆整棵树（实测），所以分类之后还要与调用方说明的 `CloneKind` 核对：种类不符就拒绝，不调 `fclonefileat`（目录对 `File` → `IsDirectory`，FIFO、设备对 `File` → `NotFile`，非目录对 `Dir` → `NotDirectory`）；mode 000 的文件在 `open` 那一步就 `EACCES`；socket 打不开（`EOPNOTSUPP`）。dst 的各种情形与 `clonefile` 一致：已存在 → `EEXIST`（悬空符号链接、已有的目录也是），父目录不存在 → `ENOENT`，末尾带 `/` → `ENOENT` |
| 句柄钉住对象（交换测试的依据） | 打开之后把源路径换成指向根外的符号链接（文件、目录各试一次）：克隆出来的仍是被打开的那个原对象，根外的内容一个字节也没进来。对照，按路径的 `clonefile`：flags 为 0 时跟着符号链接克隆了根外的东西，带 `CLONE_NOFOLLOW` 时克隆的是链接本身，两种都不是调用方要的。所以源在工作区里的克隆必须经句柄，不能经路径（评审 I2） |
| 句柄的跨卷 | 句柄在一个卷、dst 在另一个卷（文件句柄、目录句柄，dst 用路径或 dirfd 加名字）→ `EXDEV`，dst 不存在；dst 已存在而且在另一个卷 → `EEXIST`；dst 的父目录不存在而且在另一个卷 → `ENOENT`，与按路径的 `clonefile` 同一个优先级 |
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
| 用法：两个问题，两套规则 | **（1）磁盘上这两个名字是不是同一个文件？**——看卷：`VolumeCaps.case_sensitive = false` 时用 `case_fold_eq`（成员判断用 `case_fold_key`）；敏感卷用 `RepoPath` 的字节相等（`RepoPath` 已是 NFC，规范等价的名字字节相等）。**（2）git 把这两个路径当同一个吗？**（`is_tracked`、ignore 匹配）——看 git 的规则：gitio 只折叠 ASCII（增补 2 的 gitio 决定），开关是 `core.ignorecase`，由 overlay 读取并传下去；这个问题不用 `case_fold_eq`。**哪些问题归哪一类**（裁定 "Disk identity follows the volume, git identity follows core.ignorecase"，以及 "Mapping disk entries to tracked paths is disk identity"）：磁盘身份——overlay 的排除集 X 的成员判断、种子路径与磁盘上别的路径撞名、restore 在磁盘上遇到的障碍、磁盘上的拼写，以及把磁盘上的条目对应到基线或已跟踪的路径；git 身份——index 里的拼写解析（git 自己报告的拼写）、`is_tracked`、ignore 匹配。缺省设置下两者一致；不一致时（大小写不敏感的卷上 `core.ignorecase = false`）磁盘上的条目按卷判，所以重建出来的 `.ENV` 仍在 X 里，不会被当成可发布的新文件。gitio 现在可以依赖 apfs（增补 7 的 Layering 行），但它对 git 身份仍按 ASCII 折叠，所以 3f 里 gitio 的 `tracked_lookup_folds_ascii_case_when_ignorecase` 与 apfs 的折叠无关。`case_fold_eq`、`case_fold_key` 自己不查卷，也不查 `core.ignorecase` |

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
| 共享映射，描述符以写权限打开（`open(O_RDWR)` 加 `mmap(MAP_SHARED)`） | 在整个映射存续期间读出来是 0（`RETURNED` 位仍置），**不论映射的保护位**——`PROT_READ` 的映射一样——**也不论**创建映射的描述符是否已经关闭、映射是否在另一个进程里、有没有发生过一次写。触发条件是描述符的打开方式（这个映射"可能被写"），不是 `PROT_WRITE`。`munmap` 之后值变了（+1；经映射写过则 +2）。所以 0 是"未知"，调用方改用内容哈希；`munmap` 后的变化是保守的变更信号。（初稿写成"`PROT_WRITE` 才触发"，偏窄；评审 M1 用 `O_RDWR` 加 `PROT_READ` 的映射推翻，本轮在本机重测一致，偏安全的一侧） |
| 不受影响的映射 | `O_RDONLY` 描述符的 `PROT_READ` `MAP_SHARED` 映射：gen count 照常可知；`O_RDONLY` 加 `PROT_WRITE` 加 `MAP_SHARED`：`mmap` 自己就 `EACCES`；所有 `MAP_PRIVATE` 映射（含 `O_RDWR` 描述符上可写的私有映射，它写不进文件）也照常可知 |
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
| `openat(O_RDONLY\|O_SYMLINK\|O_NONBLOCK)` | 最后一级是符号链接 | 打开的是链接本身（没有 `ELOOP`）；同一个调用对文件、目录、FIFO 也成功。`fchflags` 可以作用在这样的句柄上（§4.2 H） |
| `openat(O_EVTONLY\|…)` | mode 000 的文件或目录 | `EACCES`：并不绕开读权限 |
| `openat(O_RDONLY\|O_DIRECTORY\|O_NOFOLLOW)`，路径以 `/` 或 `/.` 结尾 | 最后一级是指向目录的符号链接 | **成功，跟随了链接**：`lnk/`、`lnk//`、`lnk/.` 都打开了链接指向的目录，只有不带斜杠的 `lnk` 是 `ENOTDIR`（2026-10-22，macOS 27.0.1）。`O_NOFOLLOW` 只管路径的最后一个分量，结尾的 `/` 让内核把它当目录解析。`./realdir`、`realdir/../realdir` 也能打开。所以根必须是规范路径（`root_not_canonical`，§3、§5.4） |
| `openat(O_EXEC\|O_NOFOLLOW)`（目录再加 `O_DIRECTORY`） | 属主有执行位（目录的搜索位）而没有读位的目录或文件（0100） | **成功**，而 `O_RDONLY` 是 `EACCES`；对这个句柄 `fchflags`（清 `uchg`）、`fchmod` 都成功，之后 `O_RDONLY` 重开也成功。属主没有执行位的（000、0200）：`O_RDONLY`、`O_EVTONLY`、`O_SYMLINK`、`O_EXEC` 对目录和文件全是 `EACCES`，没有句柄可取（2026-10-22，macOS 27.0.1；下限 macOS 26 没测）。§4.2 H 用它缩小了清不掉标志的角落 |
| 路径 `/dev/fd/N/name`（N 是目录句柄） | 想把"相对 dirfd 的路径"交给只收路径的 `lchflags` | `ENOENT`，解析不了 |
| `renameatx_np`、`unlinkat` 的权限类 errno | 不在这张表里，见 §4.2 G | 目录改父级要写权限（0555 → `EACCES`，同一个父目录之内改名成功）；带 `UF_IMMUTABLE` 的改名与删除 → `EPERM`；`unlinkat(目录, 0)` → `EPERM`（不是 `EISDIR`）。根之下的操作把它们归成具名的码（§4.2 I） |

**F. FSEvents**

| 项 | 行为 |
| --- | --- |
| 流的参数 | `kFSEventStreamCreateFlagFileEvents \| WatchRoot \| NoDefer`，latency 0.05 s，挂在一条串行 dispatch queue 上，`kFSEventStreamEventIdSinceNow` |
| 路径 | 被监听的根先被解析成 realpath（`/tmp/x` 变成 `/private/tmp/x`）；事件里的路径是 realpath，用的是**磁盘上的拼写**（原来的大小写，NFD 保留），哪怕操作用的是另一种拼写 |
| flags 累积 | 同一个路径上的多个动作合成一个事件（只创建了又删掉的文件是 "Created Removed Modified Xattr File"），所以事件之后要用 lstat 重新确认路径还在不在 |
| 目录事件 | 目录改名（`mv d e`）只有 `/d` 和 `/e` 两个事件，子项没有事件；`clonefile` 克隆目录只有一个 `Dir Cloned` 事件，子项没有事件。所以**目录事件意味着整棵子树**。文件克隆带 `Cloned` 标志；只改大小写的改名给旧名和新名各一个事件 |
| 丢事件的信号 | `MustScanSubDirs\|UserDropped`（0x3），报在**流的根路径**上。往被监听的树里一口气创建 4 到 6 万个文件，得到 1 到 21 个这样的事件（本轮复测一次 4 万个文件得 40 个），在此之前已经送出多达 3.2 万个事件；`KernelDropped` 没出现过。头文件的说法：这对标志出现时要对被监听的每个目录做全量扫描 |
| `RootChanged` | flags 0x20，事件 id 0，路径是被监听的根（它或它的祖先被改名、删除时） |
| 排除路径 | `FSEventStreamSetExclusionPaths`（最多 8 个）**挡不住**根上的 `MustScanSubDirs\|UserDropped`：在被排除的目录里洪泛，根上照样出 6 个。所以不用它，改在回调里过滤（没有个数限制）；消费者对 `Dropped` 的重扫本来就要排除这些目录 |
| latency 参数 | 0.0、0.05、0.5 三档对丢事件没有看得出来的影响（30,000 次创建，每档 0 到 7 个 `MustScanSubDirs`） |
| 送达延迟 | 从操作到回调 15 ms 到 3.3 s 不等：同一台机器、负载相近（8 到 9）时，平均 15 ms 的运行和平均 0.5 到 1.7 s 的运行前后相继出现 |
| 高负载下静默丢事件（2026-10-23 实测） | 机器上持续的文件改动洪泛（多路 cargo 编译、另一目录里每秒上万次建删文件）期间，**任何流**（洪泛开始前建的、期间新建的，以及一个不经 strata 的纯 C FSEvents 客户端）都可以几分钟收不到回调，期间写入的事件有的晚到 160 s 以上，有的**再也不来**，而且**不带** `UserDropped`、`KernelDropped`、`MustScanSubDirs` 中的任何一个。所以 `Dropped` 只是"丢了"的充分信号，不是必要信号：消费者（overlay）不能把"没收到 Dropped"当作"没丢"，要有与监听无关的定期或按需对账。测试里因此用一条独立的金丝雀流区分"fseventsd 当时没在送达"（跳过）与真的漏报（失败），见 `tests/watch_support/canary.rs` |
| 事件 id | 洪泛下同一批里的 id 会回退，不依赖 id 的顺序；fseventsd 分配 id 的顺序与写入的顺序不一致（见下面的候选 2） |
| `FlushSync`（`Watcher::flush` 的第一步） | 在 dispatch queue 的流上约 0.05 ms 就返回。写 50 个文件之后立刻调，返回时只送达 57/500（11%，2026-10-22 18:00 复测）；加上对私有队列的排空是 134/500（27%，评审），排空单独 24/500；评审的 20 个事件那一轮是 6 到 7 个。本轮另做了一次同样的复测（写 50 个文件、立刻 `FlushSync` 加排空、10 轮）：返回时 0/500，返回后 1.5 s 内一个也没到；同一次运行里的对照组（只排空）在 1.5 s 内也只到了 95/500——那段时间的送达延迟远大于评审那次，数据不干净，只拿它当"0 到 35%"这个范围的下端。写完睡 5、20、100 ms 再调，返回时送达 0/500：留在 fseventsd 那一头的事件，`FlushSync` 够不着。晚到的事件里，id 小于等于调用前 `FSEventsGetCurrentEventId()` 的占多数（评审 201 个；复测 267 对 176 个）：这个 id 快照低估了在途的事件。能确定的效果只有一个：配上对私有队列的排空，排空之前已排进队列的回调，返回时都跑完了（排空之后新到的回调不在内）。`FlushSync` 的文档语义是把客户端里还在等 latency 计时器的事件推出来，复测里没有观察到它起了可见的作用（送达 0 到 35%），所以不对它作承诺 |
| 在回调里再进回调所在的队列（`Watcher::flush`、`drop(Watcher)` 的防护依据） | 回调里 `dispatch_sync(自己的流队列, {})`：进程被 libdispatch 以 `SIGTRAP` 杀掉（退出码 133），**不是死锁**；`FSEventStreamStop`、`Invalidate`、`Release` 之后再 `dispatch_sync` 同样 `SIGTRAP`；回调里调 `FSEventStreamFlushSync(自己的流)` 正常返回。回调里 `Stop` + `Invalidate` 之后把 `Release` 放进 `dispatch_async(自己的队列, …)`：回调跑到结尾，它用到的状态一直有效，之后没有第二次回调，进程不崩（`drop(Watcher)` 在回调里的做法）。探针 `dropcb`、`dropcb2`（2026-10-22）共 241 轮：12 轮失败全在一次运行里（可见的 5 条是那条流一个事件都没收到，另 7 条的记录被截断没看；那次运行紧接在几个被 `SIGTRAP` 杀掉的探针之后，原因没有查），之后的 216 轮（含 8 路 CPU 负载下的 25 轮）0 失败。**"在回调里"按线程判，不按 Watcher 判**（控制者对评审 n5 的更正）。探针 `specific`（5 次运行共 60 轮，2026-10-23）说明单向是安全的：A 的回调里对 B 做 `FlushSync` 加 `dispatch_sync` 排空、再普通 drop B，都正常返回，没有 `SIGTRAP`。按 Watcher 精确判断就会走这条路，于是 A 的回调排空 B 的同时 B 的回调排空 A，两条私有队列互相等，死锁（按 libdispatch 的语义；互相等这一半没有探针，因为探针会挂住）。线程局部的标志在任何回调里都不做同步等待，关不上这个环，所以采用它：回调期间对任何一个 Watcher 的 `flush` 都是无操作，drop 都走异步路径 |
| 流的生命期：`context.release` 到得多晚（`Watcher` 的回调状态只有一个所有者的依据） | `FSEventStreamContext` 带一个非 NULL 的 `release`（`retain` 为 NULL）时，FSEvents 在最后一次 `FSEventStreamRelease` 之后、甚至排空队列之后才调它。把"状态的释放"模拟成一个标志，看 `release` 到来时标志是否已置位（探针 `ctxrel`，2026-10-23，构建者）：普通 drop（`Stop`、`Invalidate`、`Release`、`dispatch_sync` 排空、再"释放"）20/20 轮，`release` 在"释放"之后；回调里 drop（`Stop`、`Invalidate`、`dispatch_async { Release；"释放" }`）20 轮里 12 轮在之后、8 轮在之前；两种 drop，"释放"之后都没有回调再到。评审另测：普通 drop 20/20 轮、回调里 drop 20/30 轮在之后；回调里 drop 在写入洪泛加 `MallocScribble=1` 下 70 轮，drop 之后 0 次回调，状态在回调用着的时候完好。所以回调状态不能有两个所有者：`retain`、`release`、`copyDescription` 全是 NULL，由 `Watcher` 释放；让 `release` 也去释放，就会释放两次 |
| 候选屏障 1：哨兵文件 | 往被监听的位置写一批文件再写哨兵，等哨兵的事件回来：慢速写入（1200 对，间隔 15 ms）没有一次乱序；一口气写 50 或 200 个文件再写哨兵时，评审在 40 轮里测到哨兵排到这批事件之前**多达 10 轮**，初稿测到 1 到 4 轮，两次测量的差别本身说明它与负载有关。2026-10-22 复测，写入由独立进程做（像一次工具调用）：同一目录 20 个文件 0/40 轮；200 个文件 4/40 轮和 31/40 轮（两次运行，后一次的流上出现了 22 个丢事件标志，不是安静的数据）；分在 8 个目录的 200 个文件 16/40 轮（这一轮的流上同样出现了 13 个丢事件标志）；迟到的事件的 id 无一例外大于哨兵事件的 id（262、1,886、4,484 个，没有一个小于的） |
| 候选屏障 2：事件 id 规则 | 等到收到 id ≥ 写完之后读到的 `FSEventsGetCurrentEventId()` 的事件：评审测得安静时 17/30 轮有文件没被覆盖（6,000 个里 959 个），洪泛时 30/30 轮；迟到事件的 id 比写完之后读到的 id 大 31% 到 45%。fseventsd 的 id 不按写入顺序分配，这条规则从原理上就站不住 |
| 候选屏障 3：哨兵 + `FlushSync` + 排空 | 评审：安静的流上两次各 40 轮、0 轮不完整（只看哨兵是 7 到 8/40）。同一台机器上的复测不同：只看哨兵，与哨兵之后再加 `FlushSync` 和排空，不完整的轮数一模一样（200 个文件的爆发，两个探针分别是 7/40 与 37/40；50 个文件是 21/40）——`FlushSync` 与排空没有让任何一轮变得完整。评审：一个兄弟目录里每秒约 2 万个文件的洪泛下 26/30 轮不完整，系统负载高时 39/40 |
| 候选屏障 4：从旧 id 重放历史，等 `HistoryDone` | 另开一个 `sinceWhen` = 写入之前读到的 id 的流，数 `HistoryDone` 之前重放了多少个写入的事件。2026-10-22 在安静时复测（写入由独立进程做，40 轮）：`HistoryDone` 平均 0.28 到 0.40 s 后到（最长 0.5 s），到时重放不完整的轮数：1 个文件 27/40，20 个文件 30/40，200 个文件 34/40，有的轮次一个都还没重放。`HistoryDone` 比事件早到，并不表示到此刻为止的写入都已重放，所以也不是屏障 |
| 结论：没有屏障（增补 7 经评审修订） | 四种候选都做不成。apfs 因此提供 `Watcher::flush()` = `FlushSync` 加排空，文档里写明"尽力缩小窗口"，不提供 `fence`。依赖监听的做法交给 overlay 决定并标注（增补 8 "Event flush, amended"）：Before* 与 Manual 检查点做不靠事件的全量扫描（"冷扫"：全量遍历，条目只靠这次遍历读到的属性经 `unchanged_since` 担保，从不靠事件流，也不重读每个文件的内容；裁定 "Cold scan"）；ToolCall 检查点由 daemon 先把钩子给出的路径（Edit、Write 的文件）交给 overlay 的收件箱再 `flush`，没有路径的工具（Bash）选温扫受影响的工作区，或者记下"一次工具调用的滞后：晚到的写入落进下一个检查点"。哪个都不得声称比这些测量支持的更多（送达 15 ms 到 3.3 s） |

**G. 改名、删除、`F_FULLFSYNC` 与时间戳**（2026-10-22 修订轮实测，同一台机器；除标明的以外在 `/Volumes/GWD`，大小写不敏感的 APFS 上）

| 项 | 行为 |
| --- | --- |
| flags（本 SDK，`sys/stdio.h`） | `RENAME_SECLUDE` 0x1、`RENAME_SWAP` 0x2、`RENAME_EXCL` 0x4、`RENAME_NOFOLLOW_ANY` 0x10、`RENAME_RESOLVE_BENEATH` 0x20。本 crate 只用 SWAP 与 EXCL。后两个实测对 from 与 to 都生效：任一边的路径中间有符号链接 → `ELOOP`，任一边用 `..` 越出 dirfd → `ENOTCAPABLE`，对照：不带 flag 时穿过符号链接成功。所以一次系统调用就能约束两边，是可行的优化；不采用的理由在 §9 |
| `RENAME_EXCL` | to 已存在（文件、目录、悬空符号链接）→ `EEXIST`，什么都不动；from 不存在或 to 的父目录不存在 → `ENOENT`；to 的某一级父级是文件 → `ENOTDIR`；名字 300 个 ASCII 字符 → `ENAMETOOLONG`；跨卷 → `EXDEV`（包括跨嵌套挂载点，两个方向）。from 是符号链接时，改名的是链接本身，指向的目录不受影响 |
| 同一个条目 | from 与 to 是同一个路径时，EXCL、SWAP 和普通改名都返回成功而什么都不做；大小写不敏感的卷上只差大小写的 EXCL 改名（`Foo` 到 `foo`）也成功，改的是磁盘上的拼写。所以 `rename_rooted` 在调内核之前把 RepoPath 完全相同的情形拦成 `InvalidInput`（`same_path`），只差拼写的交给内核 |
| `RENAME_SWAP` | 文件与文件、文件与目录、目录与目录（同一个父目录或不同的父目录）、符号链接与文件，都成功；各对象的 inode、mtime、gen count 跟着对象走，换的只是名字（实测：换完之后 `x` 名下是原来 `y` 的 inode、mtime 和 gen count）；任一边不存在 → `ENOENT`；跨卷 → `EXDEV`。HFS+ 镜像上 → `ENOTSUP`，MS-DOS 镜像上成功 |
| 目录改名进自己里面 | EXCL 与 SWAP 都是 `EINVAL`（to 在 from 之下；SWAP 一个目录与它的后代） |
| 改目录的父级要写权限 | 把目录改名到另一个父目录，要求对这个目录本身有写权限（它的 `..` 要改）：0555 的目录 → `EACCES`；`RENAME_SWAP` 里有 0555 的目录、且两个目录在不同的父目录下，同样 `EACCES`；同一个父目录之内改名，或对调（`d1` 是 0555、`d2` 是 0755），成功；`chmod u+w` 之后改父级成功。所以 deps 先改名、后把 mode 设成 0555（`set_file_meta`）；gc 和 `retire_dir` 挪走已封存的目录之前先 `make_removable`（先清标志、再补 u+w，次序见 §4.2 H；增补 4 与 6 的"控制者实测"，这里在本机重做了一遍，结果一致） |
| 父目录与标志 | 文件挪出 0555 的父目录、或挪进 0555 的父目录 → `EACCES`；带 `UF_IMMUTABLE` 的文件或目录改名，或从带 `UF_IMMUTABLE` 的目录里挪出 → `EPERM` |
| 挂载点 | `rmdir`、`unlink` 一个挂载点 → `EBUSY`；改名一个挂载点目录成功，挂载跟着走 |
| `unlinkat(fd, name, 0)` | 文件、符号链接（指向文件、指向目录、悬空）、FIFO → 成功，符号链接只删链接，目标不动；目标是**目录** → `EPERM`（不是 Linux 的 `EISDIR`）；不存在 → `ENOENT`；带 `UF_IMMUTABLE` → `EPERM`；父目录是 0555 → `EACCES`。所以 `EPERM` 要靠 `fstatat` 区分"是目录"还是"带标志" |
| `unlinkat(fd, name, AT_REMOVEDIR)` | 空目录成功（目录自己是 0555 也行，要的是父目录的写权限）；非空 → `ENOTEMPTY`；文件或指向目录的符号链接 → `ENOTDIR`；不存在 → `ENOENT`；带 `UF_IMMUTABLE` → `EPERM`；父目录是 0555 → `EACCES` |
| 时间戳 | `clonefile`：克隆出的文件和空目录的 mtime 是来源的（实测 2020 年），有子项的目录的 mtime 是克隆那一刻（目录树里每个有子项的目录都是；`inner` 是空的、`onlysub` 里只有它，则 `inner` 是 2020 年、`onlysub` 是克隆那一刻），所有对象的 ctime 都是克隆那一刻；改名只更新被改名对象的 ctime（它的 mtime 不动），两个父目录的 mtime 与 ctime 都更新；目录改到另一个父目录同样只更新目录自己的 ctime；`RENAME_SWAP` 更新两个对象的 ctime；`chmod`、`chflags`、`setxattr` 只更新 ctime；`utimensat` 把 mtime 设成给定值、ctime 设成现在；写入两者都更新。只看 mtime 会把刚克隆、刚改名的东西当成很老，所以 gc 的年龄是 `max(mtime, ctime)`（增补 4），apfs 用 `Attrs::last_changed` 给出它 |
| `F_FULLFSYNC` | 对 `O_RDONLY\|O_NOFOLLOW\|O_NONBLOCK` 打开的普通文件句柄和目录句柄（正是 `open_rooted` 给的）都成功；HFS+、MS-DOS 和挂载的 APFS 镜像上也成功。对管道、socket 的句柄 → `EBADF`，`/dev/null` → `ENODEV`。USB SSD 上两次测量：文件 10 ms 与 12 ms，建文件加改名之后对目录 26 ms 与 4 ms，没有脏数据时近乎 0；不是常数，只说明量级 |

**H. 删除一整棵树、清标志与改元数据**（2026-10-22 第 1 轮评审，同一台机器；探针在内部卷的临时目录里，一棵含下列各类条目的树，删完之后核对根外的目录原样；`remove_tree_rooted`、`make_removable`、`set_file_meta` 的依据。第 2 轮补的几行是同日在同一台机器上用独立探针量的；第 3 轮补的探针（ACL 目录、带标志的目录上按名字的 `fchmodat`）是 2026-10-23 在同一台机器的内部卷上量的）

| 项 | 行为 |
| --- | --- |
| 受保护的条目删不掉 | 带 `UF_IMMUTABLE` 的文件、符号链接（`chflags -h uchg`）、目录，带 `UF_APPEND` 的文件、目录：`unlinkat` 与 `AT_REMOVEDIR` 都是 `EPERM`；`UF_APPEND` 的目录里再 `unlinkat` 一个条目也是 `EPERM`。五类各一个的探针树里，清标志的次数正好是 5 |
| 清标志只能经句柄 | 对条目取句柄，再 `fchflags(fd, flags & ~(UF_IMMUTABLE\|UF_APPEND))`：文件与目录的句柄用 `openat(O_RDONLY\|O_NOFOLLOW\|O_NONBLOCK)`（目录再加 `O_DIRECTORY`）；**符号链接**用 `openat(O_RDONLY\|O_SYMLINK\|O_NONBLOCK)`（打开的是链接本身），清了之后链接删得掉（带 `uchg` 的符号链接：清之前 `unlinkat` 得 `EPERM`，清之后成功）。取到句柄之后核对 (dev, inode) 与 `fstatat` 看到的一致，防止这中间条目被换掉；非目录还要再 `fstat` 看一遍 `st_nlink`，大于 1 就不清（见"共用的 inode"） |
| 不用 `fchflagsat` | SDK 标它 macOS 27.0 起才有，libc 0.2.191 没有导出，而 design 的下限是 macOS 26：不采用，句柄版本的 `fchflags` 够用（§9）。代价是下一行的那个角落 |
| 没有读权限的条目 | mode 000 的**目录**：`openat(O_RDONLY\|O_DIRECTORY)`、`O_EVTONLY`、`O_SYMLINK`、`O_EXEC` 对属主全是 `EACCES`，没有句柄可取；这时只有按名字的 `fchmodat(P, name, mode\|0700, AT_SYMLINK_NOFOLLOW)` 一条路，对属主成功（探针：0000 → 0700），之后能打开、能列、能删（这是 `remove_tree_rooted` 里唯一一处按名字改 mode，残余风险见下一行）。**这条路只在 chmod 帮得上忙的时候走**，由第 1 步 `fstat` 的 `st_mode` 与 `st_flags` 判：属主没有搜索位，**而且**目录不带 `UF_IMMUTABLE`、`UF_APPEND`、`SF_IMMUTABLE`、`SF_APPEND`。带标志的目录 chmod 永远是 `EPERM`（探针，2026-10-23：0000 加 `uchg`、0200 加 `uchg`、0000 加 `uappnd` 的目录，`O_RDONLY` 与 `O_EXEC` 都是 `EACCES`，按名字的 `fchmodat(0700)` 都是 `EPERM`，mode、标志、`ctime` 不变）；**属主有搜索位而两种打开都是 `EACCES` 的是 ACL**（属主 0700 的目录加 ACL `deny list,search`：两种打开都是 `EACCES`，ACL 压过 mode 位；`fchmodat(0700)` 返回成功却什么都没改，只让 `ctime` 前进。评审与构建者各自复现）——chmod 解不了它，却照样带着下一行的换名暴露，所以不去试，直接记 stuck（带标志 → `Immutable`，ACL 或别的 `EACCES` → `NoPermission`，§5.4）。**属主有执行位而没有读位**的目录（0100）：`O_RDONLY` 是 `EACCES`，但 `openat(O_EXEC\|O_DIRECTORY\|O_NOFOLLOW)` 成功（§4.2 E），在这个句柄上 `fchflags` 清 `uchg`、`fchmod` 补 `0700` 都成功，之后 `O_RDONLY` 重开就能列——所以这类目录不走按名字的那一条。mode 000 的**文件**：删它只要父目录可写，`unlinkat` 直接成功。mode 0555 的目录：能打开，但要删里面的条目得先 `fchmod(fd, mode\|0700)`。**既带 `uchg` 又没有读位也没有执行位的条目——文件、目录都一样**（0000、0200 一类）：要清标志就得先取句柄，上面的打开方式全是 `EACCES`；带 `uchg` 就不能 `chmod`（目录上按名字的 `fchmodat(P, name, 0700, AT_SYMLINK_NOFOLLOW)` 实测 `EPERM`，标志不变），也没有路径可交给 `lchflags`（`/dev/fd/N/name` 解析不了）——这个角落在不用 `fchflagsat` 时无解，报 `Immutable`（macOS 27 的 `fchflagsat` 能解，不采用，§9）。属主有执行位的 `uchg` 文件（0100）反而能删：`O_EXEC` 句柄上清标志，之后 `unlinkat` 成功（探针） |
| 按名字的 `fchmodat` 的残余风险 | 上一行里 mode 000 的目录（属主没有执行位）是 `remove_tree_rooted` 里**唯一**不经句柄改 mode 的地方：`fchmodat(P, name, mode\|0700, AT_SYMLINK_NOFOLLOW)`，P 是钉住的父目录。`fstatat` 判出它是属主的目录之后、`fchmodat` 之前，树里的并发写者（同一用户的别的进程）可以把这个名字换成别的对象。实测（探针）：换成符号链接——不跟随，根外文件的 mode 不变；换成**指向根外文件的硬链接**——`fchmodat` 改的是那个根外 inode，0644 变成了 0700。限度：（1）只在 `O_RDONLY` 与 `O_EXEC` 都得 `EACCES`、`st_uid` 是调用者、属主没有搜索位、目录又不带 `UF_IMMUTABLE`、`UF_APPEND`、`SF_IMMUTABLE`、`SF_APPEND`（后三条都由第 1 步的 `fstat` 判）的目录上走这一步——ACL 拦着的、带标志的目录 chmod 帮不上忙，不去试，也就不带这个暴露——别的情形都经句柄；（2）`fchmodat` 要求属主，所以只能改调用者自己拥有的 inode，不跨权限边界；（3）之后立刻 `openat` 重开并核对 (dev, inode) 与类型，不符记 `stuck(Changed)`，但 mode 已改，apfs 不回滚（A3）。所以 `remove_tree_rooted` 的前提是被删的树**通常**没有并发写者——store 的 trash 与 tmp 里已退役的东西通常如此（增补 8），但这是惯例，不是保证：`move_workspace` 交换之前只核对依赖目录上打开的文件（session.md §6.5），cwd 在旧工作区里的 shell 会跟着旧树穿过交换进了 trash，还能继续往里写；要利用这个暴露，写者还得在树里放一个指向根外文件的硬链接。活的工作区不该直接交给它 |
| 共用的 inode（硬链接） | `fchflags`、`fchmod`、`futimens` 改的是 inode：同一个 inode 的两个名字，经一个名字 `fchflags(uchg)`，另一个名字上读到的标志同样是 `uchg`，经另一个名字 `unlinkat` 它得 `EPERM`；经一个名字 `fchmod(0400)`，另一个名字的 mode 同样变成 0400（探针）。store 的 writer 复现过更糟的（2026-10-23，macOS 27.0.1）：get 的克隆 dst 被换成指向根外 0600 文件的硬链接，`open(O_NOFOLLOW)` 照常打开它，`fchmod 0644` 把根外的文件改成了 0644。所以非目录而 `st_nlink > 1` 的 inode 上：`remove_tree_rooted` 不清标志（留下，`stuck(Immutable)`，`detail = hard_link`），`make_removable` 与 `set_file_meta` 拒绝（`ReadOnly`，`hard_link`）。没有标志拦着的硬链接照常 `unlinkat`：只删这个名字，其它名字与 inode 不动。取到句柄之后再 `fstat` 看一遍 `st_nlink`，`unlinkat` 失败和取句柄之间有人 `link()` 也拦得住 |
| 目录没有别的名字 | 对目录 `link(2)` 得 `EPERM`，没有建出别名（在系统卷上探过；`/Volumes/GWD` 是 APFS（`diskutil`），没有在上面探，APFS 在任何卷上都不支持目录硬链接，2026-10-23）。目录的 `st_nlink` 随子目录数增长（带一个子目录的目录是 3），不能用来判"共用"。目录的句柄上：`fchmod(0555)` 与 `futimens([UTIME_OMIT, t])` 成功（mtime 带纳秒，atime 不动）；带 `uchg` 的目录 `fchmod` 和 `futimens` 都得 `EPERM`，`fchflags` 清掉之后都成功，所以"清标志在先"的次序对目录同样必要。因此 `set_file_meta` 对目录的句柄不核对 `st_nlink`，种类、`SF_*` 的核对与改动的次序同文件（裁定 "set_file_meta accepts a directory handle"） |
| mtime 超出范围被夹住 | `futimens` 给一个 APFS 表示不了的时间（3000 年、`i64::MAX` 秒、-9223372037 秒）**不报错**：读回的 mtime 是边界，上界 2262-04-11T23:47:16.854775807Z，下界 1677-09-21T00:12:43.145224192Z（探针，64 位纳秒）。所以 `set_file_meta` 不会因为时间太远而失败，也不替调用方校验时间的范围。它自己先把请求的时间夹到这两个边界，再与当前的 mtime 比较，不同才调 `futimens`：越界的请求第一次把 mtime 设成边界，之后同样的请求什么都不调，`ctime` 不动（不先夹的话，请求值永远不等于读回的边界值，"只在需要时才调"就变成每次都调 `futimens`，每次都前进 `ctime`）；测试的 mtime 除了专测越界的那一条，都取范围之内的 |
| `make_removable` 的次序 | 0555 加 `uchg` 的目录：先 `fchmod(0755)` → `EPERM`；先 `fchflags` 清 `uchg`（`flags & ~(UF_IMMUTABLE\|UF_APPEND)`）→ 成功，再 `fchmod(0755)` → 成功（mode 0755，flags 0；探针）。所以先清标志、再补 u+w，顺序不能反 |
| `UF_COMPRESSED` | 透明压缩的文件（`ditto` 复制 `/Library/Documentation/Acknowledgements.rtf` 得到，标志 0x20，1,386,862 字节；这台机器上 `ditto --hfsCompression` 造不出压缩文件）：经句柄加上 `uchg\|hidden`（0x8022）之后 `fchmod` 得 `EPERM`；`fchflags(fd, st_flags & (UF_COMPRESSED\|SF_*))` 成功，标志回到 0x20，大小与内容不变，之后 `fchmod` 成功（探针）。所以 `set_file_meta` 的 `clear_flags` 把 `UF_COMPRESSED` 留下：它描述内容的存储方式，不挡删除也不挡写，不是写回的文件要清的东西 |
| 无操作的 `chmod`、`chflags` 也更新 ctime | 文件上间隔 2 s 的三次调用：`fchmod` 成同样的 mode、`fchflags` 成同样的标志、`fchmod` 成已含 `u+w` 的 mode，每次 `ctime` 都前进约 2 s，`mtime` 不动（探针）；目录的句柄上同样，无操作的 `fchmod`、`fchflags` 也前进 `ctime`（探针）。gc 的年龄是 `max(mtime, ctime)`（§4.2 G），所以 `make_removable` 与 `set_file_meta` 只在需要时才调 `fchflags`、`fchmod`、`futimens`，无事可做就什么都不调；gc 也要先用 `Attrs::last_changed` 判过年龄、**之后**才对候选调用 `make_removable` |
| 修不好的条目 | ACL 里有 `deny delete` 的文件：`unlinkat` → `EACCES`，没有办法修复，进 `stuck`（`NoPermission`），树里其余的东西照常删光（探针：26 项删掉，1 项留下）。`SF_*` 系统标志要 root，进 `stuck`（`Immutable`，`detail` 列出标志名），从不清；没有标志的 `EACCES`、`EPERM` 才是 `NoPermission`（§5.4 的 stuck 原因表） |
| 不跟随、不越界 | 符号链接（指向文件、悬空、指向根外目录）、FIFO、unix socket：只删节点本身；硬链接：没有标志拦着时只删这个名字，其它名字与 inode 不动（有标志拦着的见"共用的 inode"）。探针里根外目录的两个文件原样 |
| 嵌套挂载点 | 对挂载点的名字 `fstatat(NOFOLLOW)` 得到被挂载卷根目录的属性，`st_dev` 与父目录的不同（探针里 16777258 对 16777233），据此不进入、不删（`OtherDevice`）；对它 `AT_REMOVEDIR` 是 `EBUSY`（§4.2 G）。探针：树里别处全部删光，挂载点和卷里的文件原样。目标本身是挂载点（`remove_tree_rooted` 的目标，或 `remove_rooted(EmptyDir)` 的目标）：`st_dev` 与父目录的不同，开始之前就拒绝（`CrossVolume`，`reason = mount_point`），什么都没动；`AT_REMOVEDIR` 晚到的 `EBUSY` 同样映射 |
| 父目录的写权限预检 | 对父目录句柄 `faccessat(P, ".", W_OK\|X_OK, AT_EACCESS)`：0755 → 成功；0555 → `EACCES`；`UF_IMMUTABLE` → `EPERM`；**`UF_APPEND` → 也成功**，实际往里 `unlinkat` 却是 `EPERM`，所以还要看 `fstat(P).st_flags`。用途：目标本身最后要靠 `AT_REMOVEDIR` 删，需要 P 可写，开始之前先查，免得树删空了才发现根删不掉 |
| 速度 | 内部卷上 102,001 项（1000 个目录，各含一个装 100 个文件的子目录）：`readdir` 加 `unlinkat` 的参考实现，三份克隆各 2.98 到 2.99 s（29 µs 每项），原树 3.65 s（36 µs）；`rm -rf` 一份克隆 3.87 s（38 µs 每项）。同一棵树 `clonefile` 一次是 0.67 到 0.70 s（6.6 到 6.8 µs 每项）：删除比克隆慢四倍多 |

**I. errno 归类表**（`classify_io_error` 与根之下各操作共用；types.md §6.3 的表加 apfs 的细化，全部在本机验证过）

| errno | `IoSide::Read` | `IoSide::Write` | 说明（实测哪些调用给出它） |
| --- | --- | --- | --- |
| `ENOENT` | `NoSuchPath` | `NoSuchPath` | 任何 `*at` 的缺失分量；`Swap` 缺一边也是它，`reason` 是 `from_missing` 或 `to_missing`（`rename_rooted` 用 `fstatat` 细分） |
| `EEXIST` | `Exists` | `Exists` | `clonefile*`、`fclonefileat`、`openat(O_CREAT\|O_EXCL)`、`mkdirat`、`symlinkat`、`RENAME_EXCL`；dst 是悬空符号链接也是它 |
| `ENOTDIR` | `NotDirectory` | `NotDirectory` | `openat(O_DIRECTORY\|O_NOFOLLOW)` 碰到符号链接也是 `ENOTDIR`（不是 `ELOOP`）；`AT_REMOVEDIR` 碰到文件或符号链接；`reason` 由 `fstatat` 细分为 `symlink` 或 `not_a_directory` |
| `EISDIR` | `IsDirectory` | `IsDirectory` | 很少出现：`unlinkat(目录, 0)` 给的是 `EPERM`，由 `remove_rooted` 先用 `fstatat` 判成 `IsDirectory` |
| `ENOTEMPTY` | `NotEmpty` | `NotEmpty` | `AT_REMOVEDIR` 非空目录 |
| `EXDEV` | `CrossVolume` | `CrossVolume` | `clonefile*`、`fclonefileat`、`renameatx_np` 跨卷，含跨嵌套挂载点 |
| `ENOTSUP`、`EOPNOTSUPP` | `Unsupported` | `Unsupported` | HFS+ 上的 `RENAME_SWAP`、`clonefile`（`ENOTSUP`）；`open` 一个 socket 的 `EOPNOTSUPP` 由 `open_rooted` 先判成 `NotFile`（`socket`） |
| `ENAMETOOLONG`、`EINVAL` | `InvalidInput` | `InvalidInput` | 名字超 255 个 UTF-16 码元、路径达 1024 字节；改名进自己里面是 `EINVAL`；FIFO、设备作 `clonefile` 的源是 `EINVAL`（`clone_*` 自己先分类成 `NotFile`） |
| `EACCES` | `Unknown` | `ReadOnly`（`no_permission`） | types.md §6.3：只有写操作的权限错误才是 `ReadOnly` |
| `EPERM` | `Unknown` | `ReadOnly`（`no_permission`） | 带标志的 `EPERM` 与 `unlinkat(目录)` 的 `EPERM` 由各个操作先用 `fstatat` 细分成 `immutable`、`IsDirectory` 之后才轮到这里 |
| `EROFS` | `Unknown` | `ReadOnly`（`read_only_volume`） | |
| `EBUSY` | `Unknown`（`busy`） | `Unknown`（`busy`） | 对挂载点 `rmdir`、`unlink`、`AT_REMOVEDIR` |
| 其余：`ENOSPC`、`EDQUOT`、`EIO`、`EDEADLK`、`EMFILE`、`ENFILE`、`ENOMEM`、`ELOOP` 等 | `Unknown` | `Unknown` | 没有 `raw_os_error` 的 `io::Error` 也是 `Unknown` |
| 任何 errno，`IoSide::AfterWrite` | — | `Unknown` | 已经改了一部分之后的失败，不论 errno（types.md §6.3 的末段，不变量 6） |

### 4.3 不变量

| # | 不变量 | 落实 |
| --- | --- | --- |
| A1 | 不原地改、不覆盖：`clone_*` 只读 src，dst 必须不存在；目的地变体对已存在的东西（含悬空符号链接）一律 `Exists`；`rename_rooted` 只有 `Excl` 与 `Swap` 两档，没有覆盖的一档；crate 里没有"覆盖"或"合并进已有目录"的路径 | design §9 不变量 5 |
| A2 | 没有复制回退：整个 crate 里没有任何复制字节的代码路径（`std::fs::copy`、`copyfile(3)`、`cp`）；卷不对就在写入之前返回 `CrossVolume` 或 `Unsupported` | design §7；types.md `CrossVolume` |
| A3 | 写入开始之后只会是 `Unknown`，现场原样留下：单次改动的操作（`clone_*`、`clone_from_rooted`、`create_file_rooted`、`symlink_rooted`、`rename_rooted`、`remove_rooted`）失败时什么都没动，可以给具名的码；多步的（`mkdir_rooted(parents)`、`clone_file_rooted` 的克隆加设 mode、`make_removable` 的清标志加补写位、`set_file_meta` 的清标志、设 mode、设 mtime）第一步生效之后的失败是 `Unknown`。**`remove_tree_rooted` 是例外**（控制者裁定）：遍历之前的拒绝是具名的码，什么都没碰；遍历一开始就算开始（清标志与补权限也是可见的写），之后永远返回 `Ok(RemoveReport)`，没删干净的条目在 `stuck` 里带原因（含 unlink 结果不确定的 `UnknownOutcome`），没有 `Err` 形态的部分失败——部分删除的结局是确定的，调用方重试剩下的。apfs 不在失败后自己清理、不回滚（`remove_rooted`、`remove_tree_rooted` 只删调用方点名的条目与树） | design §9 不变量 6；增补 8；裁定 "remove_tree_rooted reports, it does not fail" |
| A4 | 根之下不跟随符号链接：`open_root`（根本身最后一级）、`open_rooted`、`clone_from_rooted`（源这一侧）、`attrs_of`、`walk` 解析 `start`、全部目的地变体、`rename_rooted`（from 与 to 两边）、`remove_rooted`、`remove_tree_rooted`（含树内每一级子目录），对根之下的每一级 `openat(O_NOFOLLOW)`；写、改名、删除永远落不到根之外，也就碰不到工作区里指向基线、依赖集或主 checkout 的符号链接（dev-link 就会建这样的链接）。根本身必须规范（是绝对路径，不以 `/` 结尾，没有 `.`、`..` 分量，否则 `InvalidInput`，`root_not_canonical`）：结尾的 `/` 会让最后一级跟着符号链接走（§4.2 E）。`make_removable`、`set_file_meta` 只收句柄，不经路径 | design §9 不变量 2、5；裁定 T4、增补 3；裁定 "Rooted roots are canonical" |
| A5 | 监听只是提示：`WatchEvent` 的缺失不证明什么；`walk` 不是快照；crate 里没有"等监听追上"或"已完整"的接口——`Watcher::flush` 只是尽力缩小窗口，不是屏障（§4.2 F） | 裁定 T4；design §6.2"正确性不依赖监听" |
| A6 | gen count 为 0 是未知，未知永远不等于未变；`Attrs::unchanged_since` 是比较的唯一入口 | design §6.2；增补 1 |
| A7 | 资源有界：`walk` 与 `remove_tree_rooted` 同时持有的描述符不超过深度加一（≤ 129；后者读完一个目录的名字就关掉目录流，每层只留一个句柄），与目录总数无关；`RemoveReport.stuck` 最多 `MAX_REPORTED_STUCK` 条；`walk` 在内存里最多存当前路径上各目录的条目；`Watcher` 的回调串行 | 3f 的 `RLIMIT_NOFILE` 256 两行 |
| A8 | 没有全局可变状态：除线程局部的测试接缝外函数都无状态，可以从任意线程并发调用；`Watcher` 是唯一有生命周期的对象 | — |
| A9 | 只做被要求的 I/O：读写只限调用方传进来的路径，不创建临时文件，不写日志文件；删除只发生在 `remove_rooted`、`remove_tree_rooted` 点名的条目与树之下 | 增补 8 |
| A10 | 不替调用方放宽权限，也不自作主张：`rename_rooted`、`remove_rooted` 被拒绝就返回 `ReadOnly` 加 `detail.reason`，从不 `chmod`、不 `chflags`、不重试。apfs 只在三处改标志或 mode：（1）`make_removable`，调用方显式调用，**先**清 `UF_IMMUTABLE`、`UF_APPEND`、**再**补 u+w（0555 的封存目录要先补 u+w 才能改名到另一个父目录，带 `uchg` 的目录 `fchmod` 得 `EPERM`，§4.2 G、H）；（2）`set_file_meta`，调用方显式调用，改一个文件或目录（例如封存根目录的 0555）的标志、mode、mtime；（3）`remove_tree_rooted` 删它被点名的那棵树的过程中：清树内条目的 `UF_IMMUTABLE`、`UF_APPEND`（经句柄），给打不开或写不进的目录补属主的 `rwx`，如实记进 `RemoveReport`。调用方因此从不自己调 `fchmod`、`fchflags`、`futimens`，目录的句柄也一样（store 私有的 `store/blobs`、`store/tmp` 里增补 8 的 libc 允许照旧，裁定 "set_file_meta scope, corrected"）。三处共同的界限：`SF_*` 系统标志从不清（上报）；**非目录而 `st_nlink > 1` 的 inode 上不清标志、不改 mode**（标志与 mode 属于 inode，改了会连带别的名字，包括树之外的）：`make_removable`、`set_file_meta` 拒绝（`ReadOnly`，`hard_link`），`remove_tree_rooted` 留下并记 `stuck(Immutable)`（`detail = hard_link`）；树之外的东西（含它的父目录）一概不碰 | 增补 4、6、8；裁定 "make_removable"、"Flags are cleared only on an unshared inode"、"Metadata changes through a handle" |

## 5. 流程与算法

### 5.1 卷标识与能力（`same_volume`、`nearest_existing_ancestor`、`volume_caps`）

`same_volume(a, b)`：

1. 对 a、b 各做一次 `getattrlist(path, {commonattr: ATTR_CMN_DEVID}, FSOPT_RETURN_REALDEV)`，**不带** `FSOPT_NOFOLLOW`，即跟随符号链接。
2. 两个 `dev_t` 相等才返回 `true`。`ENOENT` 映射成 `io::ErrorKind::NotFound`，其余错误原样。

为什么是 `FSOPT_RETURN_REALDEV`：`/`（封存的系统卷）与 `/Users`（数据卷）的 `st_dev` 都是 16777233，真实设备号却是 16777235 与 16777233；`clonefile` 在两者之间返回 `EXDEV`，比较 `st_dev` 会误判为同卷。firmlink 两侧的路径（`/Users/x` 与 `/System/Volumes/Data/Users/x`）真实设备号一致（实测均为数据卷的 16777233）。`/tmp` 这样的符号链接对象本身在系统卷上，所以必须跟随。本机 `/Volumes/GWD` 是 16777243。

`nearest_existing_ancestor(path)`（评审 M17；config.md §4.3 "`paths.root` 还不存在时，取它最近的已存在的上级目录所在的卷"）：

1. `path` 不是绝对路径 → `io::ErrorKind::InvalidInput`。
2. 沿 `Path::ancestors()` 从 `path` 自己往上，对每个 `p` 做 `fs::metadata(p)`（`stat`，跟随符号链接）：成功 → 返回 `p`；`ErrorKind::NotFound`（含悬空符号链接）或 `raw_os_error() == ENOTDIR` → 继续往上；其它错误（`EACCES` 等）原样返回。
3. 爬到 `/` 也没有结果（实际不会发生：`/` 存在）→ 返回最后一个错误。

config.md §4.3 的规则因此是 `same_volume(&nearest_existing_ancestor(&paths.root)?, &main_checkout)`：一条规则一份实现。store.md、deps.md、session.md 各自写了一遍"爬到最近的已存在上级再比较"，它们在下一轮或 T15 改成调这一个（增补 8 的 T6/T8/T9 一行）。返回的可能是文件（`path` 的某一级是文件时），要目录的调用方自己核对；不做 realpath，返回值是 `path` 的前缀。

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

### 5.4 根之下的解析、`open_root`、`open_rooted`、`clone_from_rooted`、目的地变体、改名与删除

共同的解析 `resolve(root, components, final_flags)`：

1. 先检查 root 是规范路径：按原始字节，空串、不是绝对路径（不以 `/` 开头）、以 `/` 结尾、或含恰好是 `.` 或 `..` 的分量（`.repos`、`..x`、`a..b` 这样的名字是合法的）→ `InvalidInput`（`root_not_canonical`），什么都不碰（不用 `Path::components()`，它把结尾的 `/` 与 `.` 吃掉了；结尾的 `/` 让内核把最后一级当目录解析，`O_NOFOLLOW` 因此跟着符号链接走，§4.2 E）。再打开根：`open(root, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)`。`ENOENT` → `NoSuchPath`；`ENOTDIR`（不是目录，或根本身是符号链接）→ `NotDirectory`。根的祖先里的符号链接由内核照常解析。
2. 对除最后一个之外的每个分量：（测试接缝：`before_open(step, name)`）`openat(dirfd, comp, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)`，成功就关掉上一级。`ENOENT` → `NoSuchPath`；`ENOTDIR` → `NotDirectory`，并用 `fstatat(dirfd, comp, AT_SYMLINK_NOFOLLOW)` 区分：是符号链接 → `reason = symlink`，否则 `not_a_directory`；`EACCES` → `Unknown`（读取侧）。一级一级走，**任何时候手里只有当前一级的句柄**，所以之后目录被换成符号链接也改不了已经打开的这一级（3f 的 `directory_swap_after_open…`）。
3. 对最后一个分量，由调用它的函数决定怎么落地（下面）。传给每个 `*at` 调用的永远只是单个分量，不含 `/`。落地调用之前同样触发一次测试接缝 `before_open(step = 父级分量数, name = 最后一个名字)`（`open_rooted` 的终点 `openat` 就是这一步）。

**`open_root(root)`**：就是上面的第 1 步，返回根的句柄（`File`）。调用方对它 `full_fsync`，或者当 dirfd 用。

**`open_rooted(root, path)`**：解析父目录后，最后一个分量 `openat(dirfd, name, O_RDONLY | O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC)`：

- `ELOOP` → `NotFile`（`reason = symlink`）；`EOPNOTSUPP`（socket）→ `NotFile`（`socket`）；`ENOENT` → `NoSuchPath`；`ENAMETOOLONG` → `InvalidInput`（`name_too_long` 或 `path_too_long`）；`EACCES` → `Unknown`（读取侧）；其余 errno 一律 `Unknown`（`classify_io_error(.., IoSide::Read, ..)`，评审 M9）。
- 成功后 `fstat`：字符或块设备 → 关掉，`NotFile`（`device`）；普通文件、目录、FIFO 原样返回。FIFO 因为带 `O_NONBLOCK` 不会挂住，读它得到 `WouldBlock` 或 EOF。
- 单分量的 `path`（根下直接一级）没有"父目录"这一步，直接用根的句柄。

**`open_rooted_file`**：同上，打开之后要求 `S_ISREG`：目录 → `IsDirectory`，FIFO → `NotFile`（`fifo`）。TOCTOU 下，调用方以为的普通文件被换成 FIFO 也是这个结果，不挂住（3f 的 `fifo_swap_before_final_open…`）。

**`clone_from_rooted(src_root, src, dst, kind)`**（增补 8；评审 I2；裁定 "clone_from_rooted states its source kind"）：源在工作区里，捕获时不能跟着符号链接出去；做法是打开一次、核对种类、从这个句柄克隆。

1. 先查 dst，不碰源：形态（末尾 `/`、分量超 255 个码元、整条路径 ≥ 1024 字节 → `InvalidInput`，同 §5.2 第 2 步）；`lstat(dst)` 成功（含悬空符号链接）→ `Exists`；dst 的父目录不存在 → `NoSuchPath`，不是目录 → `NotDirectory`。dst 是调用方给的路径，它祖先里的符号链接内核照常解析：它只该指向 strata 自己的目录（store/tmp、携带用的私有临时名、staging），不该指向工作区里面。
2. `resolve` 到 src 的父目录句柄 P（单分量的 src 用根的句柄），接缝照常触发。
3. 最后一个分量 `openat(P, name, O_RDONLY | O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC)`，和 `open_rooted` 是同一段代码，errno 的处理也相同，只是最后一级是符号链接（`ELOOP`）或 socket（`EOPNOTSUPP`）时按 `kind` 归码：`File` → `NotFile`（`symlink`、`socket`），`Dir` → `NotDirectory`（`symlink`；socket 是 `not_a_directory`）。
4. **核对种类**（在同卷检查与 `fclonefileat` 之前）：`fstat` 句柄。`kind = File`：目录 → `IsDirectory`；FIFO、设备 → `NotFile`（`fifo`、`device`）；普通文件继续。`kind = Dir`：不是目录 → `NotDirectory`（`not_a_directory`）；目录继续。不符就关掉句柄，什么都不克隆。理由：调用方的种类来自之前的 `attrs_of` 或遍历，到打开之间路径可能被换成别的东西——文件换成目录，`fclonefileat` 就会把一整棵树克隆出去（store 第 2 轮评审发现的：`put_blob_rooted` 把整棵树克隆进 `store/tmp`，事后还得自己删）；FIFO 的句柄交给 `fclonefileat` 只会得到误导的 `EINVAL`。
5. 同卷检查：句柄的真实设备号（`fgetattrlist(fd, ATTR_CMN_DEVID, FSOPT_RETURN_REALDEV)`）与 dst 父目录的（`getattrlist`）比较，不同 → `CrossVolume`；`volume_caps(dst 的父目录)` 的 `clone == Some(false)` → `Unsupported`。
6. 接缝 `before_open(step, dst 的最后一个名字)`（step 接着数：各级父目录、打开终点、这一步），然后 `fclonefileat(fd, AT_FDCWD, dst, CLONE_NOFOLLOW | CLONE_NOOWNERCOPY)`。成功即结束。失败按 §5.2 第 8 步归类（`EXDEV` → `CrossVolume`，`EEXIST` → `Exists`，`ENOENT` → `NoSuchPath`……），再 `lstat(dst)` 填 `detail.dst_exists`。关闭句柄。
7. 克隆出来的是**句柄背后的那个对象**，不是路径当时指向的东西：打开之后路径被换成符号链接、被换成别的文件，都不影响结果（§4.2 A 的交换测试；种类核对之后换掉的东西同样，因为 `fclonefileat` 用的就是核对过的那个句柄）。所以 store 的 `put_blob_rooted` 对克隆出来的 dst 做哈希，得到的就是被克隆的那一版内容。**符号链接不经这里**：捕获的是内容，符号链接的"内容"是它的目标，调用方重建它——`attrs_of(src_root, p)` 取目标，`symlink_rooted(dst_root, q, target)`（裁定 "A symlink is recreated, never cloned"）；也不能经 `clone_file_rooted` 按路径克隆（它的 src 按路径解析，中间的符号链接内核照常跟随，§4.2 A）。不 `fsync`。

**目的地变体**共同的：

1. 把 `dst` 拆成父级分量和最后一个名字，`resolve` 到父目录的句柄 P（`mkdir_rooted(parents = true)` 例外，见下）。
2. 在 P 上只用最后的名字做一次 `*at` 调用，不预先 stat：系统调用自己的 `EEXIST`（含悬空符号链接，§4.2 E）就是 `Exists`，内核对 `clonefileat` 也是先报 `EEXIST` 再报 `EXDEV`。
3. 写侧的 `EACCES`、`EPERM`、`EROFS`（包括 `resolve` 里打开父级时的 `EACCES`）→ `ReadOnly`；`ENAMETOOLONG` → `InvalidInput`；其余 → `Unknown`。

各个函数的落地：

- `clone_file_rooted(src, root, dst, mode)`：src 的检查同 §5.3 第 1 步；`same_volume`（src 与 P）；`clonefileat(AT_FDCWD, src, P, name, CLONE_NOFOLLOW | CLONE_NOOWNERCOPY)`。`mode = Some(m)` 且克隆出来的是普通文件时，再**经句柄**设 mode [我的决定]：在 P 上 `openat(P, name, O_RDONLY | O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC)`（`before_open` 再触发一次，step 接着数，name 仍是 dst 的名字），对句柄 `set_file_meta(fd, { mode: Some(m & 0o777) })`（核对普通文件与 `st_nlink == 1`，见下面的"经句柄改元数据"，所以克隆之后 dst 被换成指向根外文件的硬链接，也改不到那个文件，§4.2 H）。`set_file_meta` 的接缝是另一个钩子 `before_meta`（§3）：mode 真要变时恰好触发一次 `(0, Mode)`，序号从这次 `set_file_meta` 调用的 0 起，不接着 `before_open` 数，所以叫 `mode` 的 dst 不会与它混淆。这一步的任何失败都是 `Unknown`（克隆已经发生，`set_file_meta` 的具名拒绝也一样）；源带 `UF_IMMUTABLE` 时这一步会得 `EPERM`（实测，§4.2 A）。src 是调用方给的 strata 自己的路径（临时目录里的 blob、基线文件）；源在工作区里的克隆用 `clone_from_rooted`，来自工作区的符号链接用重建的办法（`attrs_of` 取目标，`symlink_rooted`）。
- `clone_tree_rooted(src, root, dst)`：src 的检查同 §5.2 第 1 步；dst 在 src 里面同样拦（P 的路径用 `fcntl(F_GETPATH)` 取）；`same_volume`（src 与 P）；`clonefileat(AT_FDCWD, src, P, name, CLONE_NOFOLLOW | CLONE_NOOWNERCOPY)`。
- `create_file_rooted(root, dst, mode)`：`openat(P, name, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, mode)`；已存在的符号链接得到 `EEXIST`，链接指向的地方不被碰（§4.2 E）。
- `symlink_rooted(root, dst, target)`：目标为空 → `InvalidInput`（`empty_target`）；`symlinkat(target, P, name)`。重建来自工作区的符号链接就用它：目标取自 `attrs_of(src_root, p)` 的 `FileKind::Symlink`（原始字节）。
- `mkdir_rooted(root, dst, mode, parents)`：`parents = false`：解析到父目录后 `mkdirat(P, name, mode)`。`parents = true`：从根开始逐级 `openat(…, O_DIRECTORY | O_NOFOLLOW)`；某一级 `ENOENT` 就 `mkdirat` 再打开它（`mkdirat` 返回 `EEXIST` 说明别人刚建了，重新 `openat` 一次，仍然按符号链接和文件的规则判）；已存在的是目录就直接往下走；中间一级是文件或符号链接 → `NotDirectory`（符号链接时 `reason = symlink`）；叶子（最后一级）已存在而且不是目录 → `Exists`（文件，或符号链接，哪怕它指向一个目录）。建出至少一级之后的失败是 `Unknown`，`detail.created` 列出已建的路径。

所有目的地变体都不 `fsync`。

**改名、删除与 `full_fsync`**：

- `rename_rooted(root, from, to, mode)`（单次系统调用，失败时什么都没动）：
  1. 调内核之前：`from` 与 `to` 的 RepoPath 相同 → `InvalidInput`（`same_path`）；`to` 以 `from` 加 `/` 开头 → `InvalidInput`（`into_itself`）。大小写不敏感的卷上只差拼写的 `into_itself`，由内核的 `EINVAL` 兜住。
  2. `resolve` 到 `from` 的父目录句柄 PF，再 `resolve` 到 `to` 的父目录句柄 PT（测试接缝的 step 接着数）。单分量的路径用根的句柄。
  3. 测试接缝 `before_open(step, to 的最后一个名字)`，然后 `renameatx_np(PF, from_name, PT, to_name, flags)`，flags 是 `RENAME_EXCL`（`Excl`）或 `RENAME_SWAP`（`Swap`）。成功即结束。
  4. 失败按 errno 归类：`EEXIST` → `Exists`（`detail.path = to`）；`ENOENT` → `NoSuchPath`，`fstatat(PF, from_name, AT_SYMLINK_NOFOLLOW)` 也是 `ENOENT` 则 `reason = from_missing`，否则 `to_missing`（只会出现在 `Swap`）；`ENOTDIR` → `NotDirectory`；`EXDEV` → `CrossVolume`（`detail.src_dev`、`dst_dev` 取 PF、PT 的真实设备号）；`ENOTSUP` → `Unsupported`（`detail.fs_type`）；`EINVAL` → `InvalidInput`；`ENAMETOOLONG` → `InvalidInput`（`name_too_long`）；`EACCES`、`EPERM`、`EROFS` → `ReadOnly`，`detail.reason` 这样判：`EROFS` 是 `read_only_volume`；否则 `fstatat(PF, from_name, AT_SYMLINK_NOFOLLOW)` 取 `from` 的属性（`Swap` 时再用 `fstatat(PT, to_name, …)` 取 `to` 的），其中有目录、PF 与 PT 不是同一个目录、且这个目录的所有者写位未置 → `reparent_needs_write`；`st_flags` 含 `UF_IMMUTABLE`、`UF_APPEND`、`SF_IMMUTABLE` 或 `SF_APPEND` → `immutable`；其余 `no_permission`。别的 errno（`ENOSPC`、`EIO`、`EBUSY` 等）→ `Unknown`，`EBUSY` 的 `reason = busy`。
  5. 不 `fsync`，不 `chmod`，不清标志（A10）。
- `remove_rooted(root, path, kind)`（单次改动的系统调用，失败时什么都没动）：`resolve` 到父目录句柄 P；`kind = EmptyDir` 先 `fstatat(P, name, AT_SYMLINK_NOFOLLOW)`（只读，见下）；接缝 `before_open(step, name)`（落地的那一次调用之前，两种 `kind` 都是紧挨着 `unlinkat`，所以测试能在 `fstatat` 与 `unlinkat` 之间动手脚）；然后 `unlinkat(P, name, 0)`（`File`）或 `unlinkat(P, name, AT_REMOVEDIR)`（`EmptyDir`）。归类：
  - `File`：`ENOENT` → `NoSuchPath`；`EPERM` → `fstatat(P, name, AT_SYMLINK_NOFOLLOW)`：是目录 → `IsDirectory`（内核对目录给 `EPERM`，不是 `EISDIR`），否则 `ReadOnly`（`st_flags` 含 `UF_IMMUTABLE`、`UF_APPEND`、`SF_IMMUTABLE` 或 `SF_APPEND` 是 `immutable`，其余 `no_permission`）；`EACCES` → `ReadOnly`（`no_permission`）；`EROFS` → `ReadOnly`（`read_only_volume`）；`EBUSY` → `Unknown`（`busy`）；其余 `Unknown`。符号链接只删链接本身。
  - `EmptyDir`：先 `fstatat`：`ENOENT` → `NoSuchPath`；是符号链接 → `NotDirectory`（`symlink`，指向的目录不被碰）；不是目录 → `NotDirectory`（`not_a_directory`）；是目录而 `st_dev` 与 `fstat(P)` 的不同（挂载点）→ `CrossVolume`（`reason = mount_point`，`detail.src_dev` 是 P 的设备号，`dst_dev` 是目标的），什么都没动。然后 `AT_REMOVEDIR`：`ENOTDIR` → `NotDirectory`；`ENOTEMPTY` → `NotEmpty`（types.md 已有的码，T15 放宽含义，§9；调用方怎样对待它见 §3 的 `RemoveKind::EmptyDir`）；`ENOENT` → `NoSuchPath`；`EBUSY`（`fstatat` 之后才变成挂载点，或别的 `EBUSY`；`rmdir` 失败时什么都没变）→ 同样 `CrossVolume`（`mount_point`），从不是 `Unknown`（裁定 "remove_rooted(EmptyDir) on a mount point"）；`EPERM`、`EACCES`、`EROFS` → `ReadOnly`（`reason` 的判法同 `File`，只是 `EPERM` 不再问"是不是目录"：目标本来就是目录，带 `UF_IMMUTABLE` 等标志是 `immutable`，其余 `no_permission`）；其余 `Unknown`。
  - 不 chmod、不清标志（A10）：被拒绝就是 `ReadOnly`，放宽权限是调用方的事（`make_removable`）。
- `remove_tree_rooted(root, path)`（增补 8；评审 I1；依据 §4.2 H）。**返回形状（控制者裁定）**：`Err` 只给遍历开始之前、什么都没碰的拒绝；遍历一开始就永远是 `Ok(RemoveReport)`，没删掉的东西在 `stuck` 里，`is_complete()` 回答删干净了没有。**`path` 可以是任何种类的条目**（目录、文件、符号链接，从不跟随符号链接；裁定 "remove_tree_rooted takes any entry kind"）：非目录的目标直接走 `unlink_leaf`（下面第 8 步），报告的形状不变，成功就是 `removed == 1`。

  **遍历之前**（任何失败都是具名的码，什么都没动）：
  1. 检查 root 规范，`resolve` 到 `path` 的父目录句柄 P（单分量的 `path` 用根的句柄），接缝照常触发。
  2. `fstatat(P, name, AT_SYMLINK_NOFOLLOW)`：`ENOENT` → `NoSuchPath`。目标是什么种类都不拒绝（符号链接只删链接本身，指向的地方不被碰）。
  3. 目标是目录，而它的 `st_dev` 与 `fstat(P)` 的 `st_dev` 不同（目标本身是挂载点）→ `CrossVolume`（`reason = mount_point`，`detail.src_dev` 是 P 的设备号，`dst_dev` 是目标的）。
  4. 父目录预检：`fstat(P).st_flags` 含 `UF_IMMUTABLE`、`UF_APPEND`、`SF_IMMUTABLE` 或 `SF_APPEND`（→ `ReadOnly`，`immutable`），或 `faccessat(P, ".", W_OK | X_OK, AT_EACCESS)` 失败（→ `ReadOnly`，`no_permission`）。理由：最后要对目标本身 `AT_REMOVEDIR`（或 `unlink`），需要 P 可写；先查，免得树删空了才发现根删不掉。`faccessat` 对 `UF_APPEND` 的目录会说"可写"，所以标志要另看（§4.2 H）。
  5. 记下 `tdev` = 目标的 `st_dev`；报告清零。

  **遍历**（自底向上，递归深度 ≤ `MAX_WALK_DEPTH`；每一层只持有一个目录句柄）：`visit(P, name, depth)`：
  1. `fstatat(P, name, AT_SYMLINK_NOFOLLOW)`：`ENOENT` → 已经没了，返回；不是目录 → 走 `unlink_leaf`（第 8 步）；是目录但 `st_dev != tdev` → `stuck(OtherDevice)`，不进入；`depth >= MAX_WALK_DEPTH` → `stuck(TooDeep)`；目录带 `SF_IMMUTABLE` 或 `SF_APPEND`（里面的条目也删不掉，`SF_*` 从不清）→ `stuck(Immutable)`，`detail` 列出标志名，不进入。
  2. 接缝 `before_open(step, name)`；`openat(P, name, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)`。`ENOENT` → 已经没了；`ENOTDIR`、`ELOOP`（刚被换成文件或符号链接）→ `stuck(Changed)`。`EACCES`（目录对属主不可读）：
     1. 目录不属于调用者（第 1 步的 `st_uid`）→ `stuck(NoPermission)`。
     2. 属于调用者：先试 `openat(P, name, O_EXEC | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)`（属主有搜索位、又没有 ACL 拦着时成功，§4.2 E）。成功 → 核对 (dev, inode) 与第 1 步一致（不一致 → `stuck(Changed)`），目录带 `UF_IMMUTABLE` 或 `UF_APPEND` 就先 `fchflags` 清掉（`flags_cleared += 1`；失败 → `stuck(Immutable)`），再 `fchmod(fd, mode | 0700)`（`made_writable += 1`，之后 `mode` 取修过的值；失败 → `stuck(NoPermission)`），关掉，回到 `O_RDONLY` 重开一次，仍失败 → `stuck(Unreadable)`。**这一条是经句柄的**，不走按名字的路径。
     3. `O_EXEC` 也是 `EACCES`（没有句柄可取）。按名字的 chmod 只在它帮得上忙的时候试，所以先看第 1 步的 `fstat`（评审 M1）：
        - 目录带 `UF_IMMUTABLE` 或 `UF_APPEND`（带 `SF_*` 的目录在第 1 步就拒绝了，到不了这里；门里仍列出它们，纯函数表才测得到）→ 不去 chmod（chmod 对带这些标志的目录永远是 `EPERM`，§4.2 H），直接 `stuck(Immutable)`，`errno = EPERM`，`detail` 列出标志名。这就是既带 `uchg` 又没有读位也没有搜索位的角落，目录也一样；属主有搜索位的话，上一条的 `O_EXEC` 句柄已经能清标志。
        - 属主**有**搜索位（`st_mode & S_IXUSR != 0`）而两种打开都是 `EACCES`：拦着的不是 mode 位，而是 ACL（`deny list,search` 一类，ACL 压过 mode 位）或别的东西，chmod 帮不上忙 → 不去 chmod，直接 `stuck(NoPermission)`，`errno = EACCES`。
        - 属主**没有**搜索位**并且**不带这些标志 → 按名字 `fchmodat(P, name, mode | 0700, AT_SYMLINK_NOFOLLOW)`（`made_writable += 1`；**残余风险见 §4.2 H**：这是整个遍历里唯一不经句柄改 mode 的地方，只用于"属主打不开、也取不到句柄、而 chmod 又确实解得了"的目录）。失败 → `stuck(NoPermission)`。成功 → 再 `O_RDONLY` 打开一次并核对 (dev, inode)，不符 → `stuck(Changed)`，仍失败 → `stuck(Unreadable)`。
  3. `fstat(fd)`：(dev, inode) 与第 1 步看到的不同 → 关掉，`stuck(Changed)`。下一步用的 `st_flags` 与 `st_mode` 取自**这一次**的 `fstat`，不用第 1 步的旧值（评审 n1）：第 2 步已经经 `O_EXEC` 句柄清过标志、补过 mode 的目录，旧值会让 `flags_cleared` 与 `made_writable` 各被计两次（§7 `remove_tree_rooted_removes_a_protected_tree` 里的 7 与 5 钉住这一点）。
  4. 目录的 `st_flags`（第 3 步的）含 `UF_IMMUTABLE` 或 `UF_APPEND`：`fchflags(fd, flags & ~(UF_IMMUTABLE | UF_APPEND))`（`flags_cleared += 1`），失败 → 关掉，`stuck(Immutable)`。目录的属主位不全（第 3 步的 `st_mode & 0700 != 0700`）：`fchmod(fd, mode | 0700)`（`made_writable += 1`），失败 → 关掉，`stuck(NoPermission)`。
  5. 用 `fdopendir(dup(fd))` 加 `readdir` 把所有名字和 `d_type` 读进一个 `Vec`，然后立刻 `closedir`：读完再删，删的过程中目录的变动不会搅乱迭代器；`DT_UNKNOWN` 的条目回到第 1 步用 `fstatat` 判。
  6. 对每个子项：`d_type` 是目录 → `visit(fd, child, depth + 1)`；否则 `unlink_leaf(fd, child)`。
  7. 若这棵子树里没有记下 `stuck`：关闭 fd，`unlinkat(P, name, AT_REMOVEDIR)`：成功 `removed += 1`；`ENOTEMPTY` → `stuck(Changed)`（别的进程在写这棵树）；`EBUSY` → `stuck(Busy)`；`EPERM`、`EACCES` → `stuck(NoPermission)`；`EIO` 一类 → `stuck(UnknownOutcome)`，其余 → `stuck(Io)`。有 `stuck` 就不尝试，目录留着。
  8. `unlink_leaf(D, child)`（第 6 步里的非目录条目，以及非目录的目标；包括符号链接、FIFO、socket、设备节点、硬链接的一个名字）：`unlinkat(D, child, 0)`，成功 `removed += 1`，`ENOENT` 当作已经没了。`EPERM` 或 `EACCES` 时 `fstatat(D, child, AT_SYMLINK_NOFOLLOW)` 取 `st_flags` 与 `st_nlink`，按顺序：
     1. 含 `SF_IMMUTABLE` 或 `SF_APPEND` → `stuck(Immutable)`，`detail` 列出置位的 `SF_*`（同时置位的 `UF_*` 也列出）；`SF_*` 从不清。
     2. 含 `UF_IMMUTABLE` 或 `UF_APPEND`，而 `st_nlink > 1`（共用的 inode，清标志会连带别的名字，A10）→ `stuck(Immutable)`，`detail = hard_link`；不碰标志。
     3. 含 `UF_IMMUTABLE` 或 `UF_APPEND`，`st_nlink == 1` → 取条目的句柄（符号链接用 `O_RDONLY | O_SYMLINK | O_NONBLOCK`，其它用 `O_RDONLY | O_NOFOLLOW | O_NONBLOCK`；非符号链接得 `EACCES` 时再试 `O_EXEC | O_NOFOLLOW | O_NONBLOCK`，§4.2 E），核对 (dev, inode)，**对句柄再 `fstat` 一次确认 `st_nlink` 仍是 1**（中间有人 `link()` 过就按第 2 项处理），`fchflags` 清掉（`flags_cleared += 1`），再 `unlinkat` 一次。取不到句柄（`O_RDONLY` 与 `O_EXEC` 都是 `EACCES`：既带标志又没有读位也没有执行位的条目）或 `fchflags` 失败 → `stuck(Immutable)`，`detail` 列出清不掉的标志名。标志清掉之后 `unlinkat` 仍失败：标志已经不是原因，按那次失败的 errno 重新归类（`EPERM`、`EACCES` 是 `NoPermission`，比如 ACL 禁止删除；`EBUSY`、`EIO` 一类见下）；清掉的标志不还原，`flags_cleared` 如实记着。
     4. 不含这些标志 → `stuck(NoPermission)`（ACL 禁止删除、属主不是调用者、mode 拒绝）。
     别的 errno：`EBUSY` → `stuck(Busy)`；`EIO`、`ETIMEDOUT`、`ENXIO`、`ENODEV`、`EDEVERR` → `stuck(UnknownOutcome)`；其余 → `stuck(Io)`。
  9. 记 `stuck` 时 `stuck_total += 1`，`stuck` 向量未满 `MAX_REPORTED_STUCK` 才真正追加。

  **收尾**：返回 `Ok(report)`，不论 `stuck_total` 是多少（`is_complete()` 就是 `stuck_total == 0`，此时目标本身也删掉了）。`EINTR` 重试，其余从不重试，也不回滚清掉的标志与补上的权限。树之外的东西一概不碰，包括 P。符号链接指着的地方不被碰；挂载点不进入。

- `full_fsync(file)`：`fcntl(fd, F_FULLFSYNC)`；`EINTR` 重试；`ENOTSUP` → `Unsupported`；其余 → `Unknown`（`detail.errno`）。

**`remove_tree_rooted` 的 stuck 原因表**（唯一的一张；`StuckReason` 的文档与 §6 都引用它，裁定 N3）。一个条目因为文件标志拦着而留下，就是 `Immutable`，标志写在 `detail` 里（`UF_*` 是清不掉的，`SF_*` 是置位的、从不清）；`NoPermission` 只给没有标志的 mode 或 ACL 拒绝：

| 情形 | `reason` | `errno` | `detail` |
| --- | --- | --- | --- |
| 条目在判类型与打开之间被换成文件或符号链接，或打开之后 (dev, inode) 与 `fstatat` 所见不同 | `Changed` | `ENOTDIR`、`ELOOP`，或 0（(dev, inode) 不符） | — |
| 删空之后 `AT_REMOVEDIR` 得 `ENOTEMPTY`（别的进程又写进了新条目） | `Changed` | `ENOTEMPTY` | — |
| 目录的 `st_dev` 与目标的不同（嵌套挂载点；`rmdir` 它得 `EBUSY`） | `OtherDevice` | 0 | — |
| 深度到 `MAX_WALK_DEPTH` | `TooDeep` | 0 | — |
| 目录打不开：`EACCES`，目录不属于调用者 | `NoPermission` | `EACCES` | — |
| 目录打不开，属于调用者：补了 mode（经句柄或按名字）之后仍打不开；或 `EMFILE`、`ENFILE` | `Unreadable` | 原 errno | — |
| 属于调用者的目录，补 mode 失败（`fchmod`、`fchmodat`），而且目录不带 `UF_IMMUTABLE`、`UF_APPEND` | `NoPermission` | 原 errno | — |
| 属于调用者的目录，`O_RDONLY` 与 `O_EXEC` 都是 `EACCES`，属主**有**搜索位、目录不带 `UF_IMMUTABLE`、`UF_APPEND`：ACL 拦着（`deny list,search` 一类，压过 mode 位）或别的东西，chmod 帮不上忙，不去试 | `NoPermission` | `EACCES` | — |
| 属于调用者的目录，`O_RDONLY` 与 `O_EXEC` 都是 `EACCES`，目录带 `UF_IMMUTABLE` 或 `UF_APPEND`：没有句柄可清，chmod 永远 `EPERM`，不去试（角落，目录也一样，§4.2 H） | `Immutable` | `EPERM`（凭标志判定，没有系统调用失败） | 置位的标志名 |
| 条目（目录、文件、符号链接）带 `UF_IMMUTABLE`、`UF_APPEND`，有句柄而 `fchflags` 失败 | `Immutable` | `fchflags` 的 errno | 清不掉的标志名 |
| 标志已经清掉（`flags_cleared` 记着），之后 `unlinkat` 仍失败 | 按这次失败的 errno 归类（下面几行：`NoPermission`、`Busy`、`UnknownOutcome`、`Io`） | 这次的 errno | 同那几行 |
| 非目录带这些标志而取不到句柄（`O_RDONLY` 与 `O_EXEC` 都是 `EACCES`：既没有读位也没有执行位，角落） | `Immutable` | `EACCES` | 标志名 |
| 条目带 `SF_IMMUTABLE`、`SF_APPEND`（要 root，从不清） | `Immutable` | `EPERM` | 置位的 `SF_*`（同时置位的 `UF_*` 也列出） |
| 非目录、`st_nlink > 1`、带 `UF_IMMUTABLE` 或 `UF_APPEND`：共用的 inode，不动标志 | `Immutable` | `EPERM` | `hard_link` |
| `EACCES`、`EPERM`，而且没有上面任何一种标志：ACL 的 `deny delete`、属主不是调用者、mode 拒绝 | `NoPermission` | 原 errno | — |
| `EBUSY` | `Busy` | `EBUSY` | — |
| `EIO`、`ETIMEDOUT`、`ENXIO`、`ENODEV`、`EDEVERR`：这个条目的 unlink 有没有生效不确定 | `UnknownOutcome` | 原 errno | — |
| 其余 errno（`ENOSPC`、`EDQUOT` 等）：unlink 确定没成功 | `Io` | 原 errno | — |

**`make_removable(file)` 与 `set_file_meta(file, meta)`**（裁定 "make_removable"、"Flags are cleared only on an unshared inode"、"Metadata changes through a handle"；依据 §4.2 H）：只收句柄，不经路径；先做同一段核对，拒绝时什么都不改：

1. `fstat(file)`。`make_removable`：非目录而 `st_nlink > 1` → `ReadOnly`（`hard_link`）。`set_file_meta`：FIFO、socket、设备、符号链接 → `NotFile`（`fifo`、`socket`、`device`、`symlink`），普通文件与目录继续；**非目录**而 `st_nlink > 1` → `ReadOnly`（`hard_link`）。目录不核对 `st_nlink`：APFS 拒绝对目录 `link(2)`，目录只有一个名字（裁定 "set_file_meta accepts a directory handle"，§4.2 H）。
2. `st_flags` 含 `SF_IMMUTABLE` 或 `SF_APPEND` → `ReadOnly`（`immutable`，`detail.flags` 列出置位的标志名）：它们要 root 才清得掉，apfs 从不清；有它们在，后面的 `fchmod`、`futimens` 也会 `EPERM`，先拒绝才保证"拒绝时什么都不改"。

`make_removable` 然后：
3. `st_flags` 含 `UF_IMMUTABLE` 或 `UF_APPEND` → `fchflags(fd, st_flags & ~(UF_IMMUTABLE | UF_APPEND))`，其它标志位原样保留；失败 → 写侧归类（`ReadOnly`，`reason` 的判法同 `rename_rooted`）。
4. `st_mode & 0200 == 0` → `fchmod(fd, st_mode | 0200)`；第 3 步改过标志的话，这一步失败是 `Unknown`（`IoSide::AfterWrite`，标志已清），否则写侧归类。两步都不需要就什么都不调，返回 `Ok`（无操作的调用也会更新 ctime，§4.2 H）。

`set_file_meta` 然后（每一步只在需要时才调，已是目标值的不调）：
3. `clear_flags` 且 `st_flags` 含 `UF_COMPRESSED` 以外的 `UF_*` → `fchflags(fd, st_flags & (UF_COMPRESSED | 全部 SF_*))`。
4. `mode = Some(m)` 且 `st_mode & 0o7777 != m & 0o777` → `fchmod(fd, m & 0o777)`（setuid、setgid、sticky 随之清掉）。
5. `mtime = Some(t)`：先把 `t` 夹到 APFS 的范围（上界 2262-04-11T23:47:16.854775807Z，下界 1677-09-21T00:12:43.145224192Z，§4.2 H），夹过的值与第 1 步 `fstat` 读到的 mtime（纳秒精度）不同才 `futimens(fd, [UTIME_OMIT, 夹过的值])`，atime 不动。

第 3 到 5 步里第一项改动生效之后的失败 → `Unknown`（`AfterWrite`，部分已改），之前的失败写侧归类；`FileMeta::default()` 只做第 1、2 步。测试接缝：`make_removable` 的第 3、4 步和 `set_file_meta` 的第 3 到 5 步，每一次真要调用的系统调用之前触发 `before_meta(step, change)`（`change` 是枚举 `MetaChange::{Flags, Mode, Mtime}`，不是字符串，所以与 `before_open` 的文件名不会混淆 [我的决定]；step 每次调用从 0 起，只数实际要做的改动，已是目标值而不调的步骤不触发），测试借此在两步之间改文件的状态，造出确定的"第一项改动之后失败"（§7）。`clone_file_rooted(.., Some(mode))` 里的 `set_file_meta` 同样触发它（见该函数一条）。纯函数 `blocking_flags(st_flags)`（列出置位的 `UF_IMMUTABLE`、`UF_APPEND`、`SF_IMMUTABLE`、`SF_APPEND` 的名字）被这两个函数和 `remove_tree_rooted` 共用，有单元测试（§7）。

**扛断电的改名**（增补 3 的 "Durability"）：`rename_rooted` 成功之后，对 from 与 to 的父目录各做一次 `full_fsync`；父目录的句柄用 `open_rooted(root, parent)` 取，根目录本身用 `open_root(root)`。apfs 不替调用方做，因为调用方常常成批改名、最后同步一次。崩溃之后 from 与 to 两个位置都合法的调用方（store 的 trash：改名前或改名后崩溃，条目留在原处或已在 trash 里都行，gc 都能处理）可以只对 to 的父目录同步；要求"崩溃之后一定在 to"的才需要两个父目录都同步。**封存的次序**（增补 4）：目录在 staging 里是 0755，`rename_rooted(Excl)` 到封存位置，成功之后再对它的句柄（`open_rooted` 取）`set_file_meta(&dir, &FileMeta { mode: Some(0o555), ..Default::default() })`；挪走已封存的目录（gc、`retire_dir`）：先 `make_removable(&open_rooted(..)?)`（先清标志、再补 u+w），再 `rename_rooted(Excl)`，否则得 `ReadOnly`（`reparent_needs_write` 或 `immutable`）。挪走之后的目录经 `remove_tree_rooted` 删掉。

### 5.5 `attrs_of` 与 `Attrs`

`attrs_of(root, path)`：

1. `resolve` 到 `path` 的父目录句柄 P（单分量的 `path` 就用根的句柄）。
2. `getattrlistat(P, name, {commonattr: RETURNED_ATTRS | OBJTYPE | MODTIME | CHGTIME | ACCESSMASK | GEN_COUNT | FILEID, dirattr: MOUNTSTATUS, fileattr: DATALENGTH}, buf, size, FSOPT_NOFOLLOW | FSOPT_ATTR_CMN_EXTENDED)`。不请求 `NAME` 与 `ERROR`。`ENOENT` → `NoSuchPath`。
3. 用与批量读**同一个**记录解析器（§5.6）解出 `Attrs`。符号链接再 `readlinkat(P, name)` 取目标原字节；这一步 `ENOENT` 说明刚被换掉，重新 `getattrlistat` 一次，再失败 → `NoSuchPath`。

`Attrs` 的字段来源：`kind` 来自 `OBJTYPE`（`VREG`→`File`，`VDIR`→`Dir`，`VLNK`→`Symlink`，`VFIFO`、`VSOCK`、`VCHR`、`VBLK` 对应其余）；`size` 来自 `DATALENGTH`（目录为 0）；`mode = ACCESSMASK & 0o7777`；`gen_count` 在返回位图含 `GEN_COUNT` 时是它的值，否则为 0；`mtime`、`ctime` 来自 `timespec`（负的秒数用 `UNIX_EPOCH - Duration`）；`inode = FILEID`。

`unchanged_since(&self, earlier)`：`kind` 相同（符号链接比较 `target`）、`inode`、`size`、`mode` 相同，且 `self.gen_count != 0 && self.gen_count == earlier.gen_count`，才返回 `true`。`earlier.gen_count == 0` 同样是 `false`。

`gen_count_known()`：`gen_count != GEN_COUNT_UNAVAILABLE`。`last_changed()`：`max(mtime, ctime)`，纳秒精度；gc 的"年龄不足 1 小时"用它（增补 4），对文件和目录都一样（`attrs_of` 对目录取的是目录自己的两个时间）。

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
   4. e 是目录、不是挂载点（`DIR_MNTSTATUS_MNTPOINT` 未置）、深度还没到 `max_depth` 的限制 → 下一次 `next()` 时 `openat(dirfd, name, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)` 压栈（测试接缝：`before_open`）。`ENOENT`、`ENOTDIR`、`ELOOP`（读完目录之后它消失了，或被换成了文件、符号链接）→ 当作它已经不在，跳过子树，不报错；`EACCES` → 产出 `Err(Unknown, unreadable_entry)`，继续；深度已达 `MAX_WALK_DEPTH` → 产出 `Err(TooLarge, too_deep)`，不进入（增补 8，码的含义宽于 types.md §6.2，§9）；`EMFILE`、`ENFILE` → `Err(Unknown)`。
   5. 符号链接、挂载点、FIFO、socket、设备文件都是叶子：只产出，从不进入、从不打开。
6. frame 的条目用完 → 关闭句柄，弹栈。
7. 栈空了 → 对根路径 `lstat`，(dev, inode) 与第 1 步记下的不同（根被换掉或改名换了新目录）→ 再产出 `Err(Busy, root_changed)`（增补 8：和 overlay 报同一个竞争的码一致，码的含义宽于 types.md §6.2，§9），然后结束。已经产出的条目来自被钉住的旧目录，调用方应当丢弃这次遍历（`move_workspace` 之后 overlay 本来就要冷扫，增补 3）。

`read_dir(fd)`：

1. 循环 `getattrlistbulk(fd, &attrlist, buf, buf_len, 0)`：返回 `n > 0` 就解析这 `n` 条记录；`0` 结束；`EINTR` 重试；`ERANGE` 把缓冲区翻倍重试，已到 `MAX_BULK_BUFFER_BYTES` 仍 `ERANGE` → `Err(Unknown)`；`ENOENT`（目录在读的时候被删了）当作空；`EACCES` 等 → `Err(Unknown, unreadable_entry)`。缓冲区的初值是 `buffer_bytes` 夹在 `[MIN, MAX]` 之间。每个窗口读完调用一次测试接缝 `after_bulk_window(n)`。缓冲区下限 1024 的理由（评审 M12）：最长的一条记录是 255 个 UTF-16 码元的名字（BMP 字符每个 3 字节，最多 765 字节，加结尾的 NUL）加上请求的全部属性——4 字节的记录长度、20 字节的返回位图、8 字节的名字引用、`OBJTYPE` 4、两个 `timespec` 共 32、`ACCESSMASK` 4、`GEN_COUNT` 4、`FILEID` 8、`MOUNTSTATUS` 或 `DATALENGTH` 至多 8——合计约 860 字节，1024 装得下；实测 255 个 ASCII 字符的名字（记录约 350 字节）已经装不进 300 字节。
2. 每条记录按 §4.2 D 的布局、按它自己的返回位图解析。记录里 `ERROR` 非零（取这一项的属性失败）→ 用 `fstatat(fd, name, AT_SYMLINK_NOFOLLOW)` 补取一次：`ENOENT` 跳过这一项；其余错误生成一个 `Err` 条目。补取成功的那一项，`Attrs` 来自 `stat`，**读不到 gen count，所以 `gen_count = 0`（未知）**，`unchanged_since` 对它一律 `false`，调用方改读内容哈希（评审 M10）；`size` 取 `st_size`（目录为 0，符号链接是 `readlinkat` 的长度），`kind`、`mode`、`mtime`、`ctime`、`inode` 取 `st` 的对应字段。
3. 名字：字节 → UTF-8，失败 → `Err(NotText)`（不做 U+FFFD 替换）；NFC 规整；与父路径用 `/` 拼好，`RepoPath::try_from`（失败 → `Err(InvalidInput)`）。
4. 符号链接：`readlinkat(fd, name, buf, DATALENGTH + 1)` 取目标原字节；`ENOENT` 跳过这一项。
5. 全部条目按 `RepoPath` 的字节序排序（NFC 之后的字节），所以产出顺序确定，与目录里的物理顺序无关。**排序只在一个目录之内**：整体产出是前序，目录条目之后紧跟它的整棵子树，所以不是全路径的字节序（`foo`、`foo/x`、`foo-bar`，而全路径的字节序是 `foo`、`foo-bar`、`foo/x`，`-` 排在 `/` 前面）。要与 BaseManifest 的 `BTreeMap` 逐项对账的调用方，收进 `BTreeMap` 或按键查找，不要假定产出全局有序（评审 M11）。

`max_depth = Some(n)`：深度（从起点算，直接子项为 1）等于 `n` 的目录条目照样产出，但不压栈。overlay 的"列一个目录"就是 `start = Some(dir)`、`max_depth = Some(1)`；`list_dir(root, dir)` 是这个组合的具名写法：`walk_with(root, &WalkOptions { start: dir.cloned(), max_depth: Some(1), ..Default::default() }, &allow_all)`，`allow_all` 是 crate 内一个全部放行的函数项（`fn allow_all(_: &Path, _: &Attrs) -> bool { true }`，`'static`），所以返回的 `Walk<'static>` 不借用调用方的东西。

文件描述符：同时持有的只有栈上各 frame 的目录句柄，最多 `MAX_WALK_DEPTH + 1` 个（§4.3 A7）。

### 5.7 `watch` 与 `watch_with`

1. `root` 必须是绝对路径（相对路径 → `InvalidInput`，`reason = root_not_canonical`，与 §3 规范根检查里"不是绝对路径"是同一条）、存在且是目录（`NoSuchPath`、`NotDirectory`）。`canonical = realpath(root)`。用 `volume_caps(root)` 记下卷是否大小写敏感，供排除过滤用。
2. 先在堆上建回调状态（`closed`、`on_event`、`canonical`、排除表、卷的大小写性质），它的地址就是 `info`。`FSEventStreamCreate(NULL, callback, context, [canonical], kFSEventStreamEventIdSinceNow, WATCH_LATENCY_SECS, FileEvents | WatchRoot | NoDefer)`，`context = { version: 0, info, retain: NULL, release: NULL, copyDescription: NULL }`：FSEvents 除了把 `info` 交给回调，从不碰它，状态只由 `Watcher` 释放（裁定 "Watcher state has one owner"，§3；理由：FSEvents 在最后一次 `Release` 之后还让流活着，非 NULL 的 `context.release` 会到得比 `Watcher` 释放状态更晚，§4.2 F）；`FSEventStreamSetDispatchQueue(stream, queue)`（`queue` 是私有串行队列）。回调入口置线程局部标志、返回或 panic 时清掉，供 `drop` 与 `flush` 判断"在任何一个 Watcher 的回调里"（控制者对评审 n5 的更正，§3）。`FSEventStreamStart` 返回假 → 清理后 `Unknown`。不设 `IgnoreSelf`（strata 自己的写入——restore、移动工作区——也要被看见）；不调 `FSEventStreamSetExclusionPaths`（§4.2 F）。
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
6. `drop(Watcher)`（状态只有 `Watcher` 一个所有者，恰好释放一次，闭包与它的捕获物恰好 drop 一次；裁定 "Watcher state has one owner"）：
   1. 不在任何一个 Watcher 的回调里（线程局部标志为假）：置 `closed`（已排队的回调到了就直接返回，不进 `on_event`）；`FSEventStreamStop`、`FSEventStreamInvalidate`、`FSEventStreamRelease`；对队列 `dispatch_sync` 一个空块，排空之前入队的回调；然后释放状态（闭包在这里 drop）与队列。返回之后不会再有回调，闭包也已经 drop 过了。
   2. 在任何一个 Watcher 的回调里（线程局部标志为真：自己的回调，或别的 Watcher 的回调）[我的决定，评审 N2；控制者对 n5 的更正]：`dispatch_sync` 到自己所在的串行队列会 `SIGTRAP`，两个回调互相排空会死锁（§4.2 F），所以不排空：置 `closed`，`FSEventStreamStop`，`FSEventStreamInvalidate`（在回调里调用它们没有问题，实测），然后 `dispatch_async(queue, { FSEventStreamRelease；释放状态（闭包在这个块里 drop）与队列 })`。串行队列保证这个块在当前回调返回之后才跑，所以正在跑的回调仍在用的状态（闭包、`Arc`）此前不会被释放。drop 返回时当前回调还在继续，之后不会有第二次回调进入 `on_event`（`closed` 挡住已排队的）。
7. `Watcher::flush(&self)`（增补 7、8 "Event flush, amended"）：
   1. 在任何一个 Watcher 的回调里调用（线程局部标志为真）→ debug 构建 `debug_assert!` 失败；release 构建直接返回。自己的回调里 `dispatch_sync` 到自己的队列会 `SIGTRAP`；别的 Watcher 的回调里单向排空是安全的，互相排空会死锁（§4.2 F）。回调里对任何一个 Watcher 都不做同步等待。
   2. 取 `Watcher` 内部的互斥量：多个线程同时调用时互相串行（`FlushSync` 的线程安全文档没有说，保守起见）。
   3. `FSEventStreamFlushSync(stream)`。
   4. `dispatch_sync(queue, {})`：空块排在已经入队的回调之后，它返回就说明排空之前入队的回调都跑完了（排空之后新到的回调可能已经开始，不在保证之内；所以"返回时没有回调在执行"不是保证，只有在安静的流上才碰巧成立）。
   5. 返回，不带"送达了多少"或"已追上"的信号（§4.2 F 的测量：送达 0 到 35%，不是屏障）。调用方怎样用它是 overlay 的决定，apfs 不承诺：Before* 与 Manual 检查点做不靠事件的全量扫描（"冷扫"：全量遍历，条目只靠这次遍历读到的属性经 `unchanged_since` 担保，从不靠事件流，也不重读每个文件的内容；裁定 "Cold scan"）；ToolCall 检查点用钩子给的路径加 `flush`；没有路径的工具（Bash）选温扫或"晚到的写入落进下一个检查点"（§4.2 F 的结论）。

给消费者的契约：（a）`Changed` 里的目录路径表示整棵子树可能变了；（b）路径可能已不存在，也可能只是被创建又删掉；（c）事件可能晚到几秒（15 ms 到 3.3 s），也可能被合并或丢掉；`flush` 只能缩小窗口，没有办法等它"追上"，所以消费者必须有不靠监听的对账（design §6.2）；（d）`Dropped` 之后到下一次全量重扫完成之前，消费者不能相信自己的脏集合；（e）往 `exclude` 里的目录洪泛也会触发 `Dropped`。

### 5.8 `case_fold_key` 与 `case_fold_eq`

`case_fold_key(s)`（评审 I6，增补 7）：

1. `s` 是纯 ASCII → `s.to_ascii_lowercase()`（快路径：NFD 对 ASCII 是恒等，全量折叠对 ASCII 就是转小写）。
2. 否则 `NFD(full_case_fold(NFD(s)))`：`full_case_fold` 用 CaseFolding.txt 的 C 与 F 两类映射，不含 T（土耳其语）；折叠表的 Unicode 版本须 ≥ 16.0。

`case_fold_eq(a, b)` = `a == b || case_fold_key(a) == case_fold_key(b)`；两个都是 ASCII 时用 `eq_ignore_ascii_case` 免去分配。等价由构造保证：`a == b` 蕴含键相等，所以 `case_fold_eq(a, b) ⇔ case_fold_key(a) == case_fold_key(b)`（§7 的一致性测试在 `case_fold_eq` 的已知样本加随机串上钉住它）。

键只存在内存里：它不是 NFC，所以不是 `RepoPath`，不能当路径用，不落盘、不进 RPC（折叠表升级会改变键）。overlay 的"按折叠身份做成员判断"（例如大小写别名）建一次以键为键的 `HashMap`，查多次，不对每一对做两两比较（增补 7）。

不查卷、不查 `core.ignorecase`：调用方先决定要不要折叠，规则是 §4.2 B 的"两个问题"，哪些问题归磁盘身份（X 的成员判断、撞名、磁盘上的拼写）、哪些归 git 身份（index 拼写、`is_tracked`、ignore 匹配）也列在那一行。

**路径性质**（裁定 "case_fold_key prefix property"，来自 overlay 的上游要求）：`case_fold_key(a + "/" + b) == case_fold_key(a) + "/" + case_fold_key(b)`，对任意 `a`、`b`（含空串、以组合记号开头的 `b`）。理由：`/` 是 ccc = 0 的 starter，NFD 的规范重排不会把组合记号挪过它，NFD 也不会让它与前后的字符组合，所以 `NFD(a + "/" + b) = NFD(a) + "/" + NFD(b)`；`full_case_fold` 逐码位作用（含 F 类的一对多映射），不看上下文，`/` 映射到自己；再做一次 NFD，同样的论证。ASCII 快路径的 `to_ascii_lowercase` 显然满足。整条路径的键因此就是各分量的键用 `/` 拼起来；overlay 对受限目录按键的前缀做范围查找（"某个目录之下的全部键"），靠的就是这条。§7 的 `case_fold_key_of_a_path_is_the_slash_joined_keys` 钉住它。

### 5.9 复杂度与验收数字

| 操作 | 复杂度 | 实测与 design §8 的数字 |
| --- | --- | --- |
| `clone_tree` | 内核 O(项数)，单线程 | tool 的 `node_modules`（108,530 个文件、8,789 个目录、63 个符号链接，共 117,382 项）在 `/Volumes/GWD`（U 盘 SSD）上 2.4 s，机器忙（负载 8 到 9）时 3.9 s，约 20 到 30 µs 每项；4 路并行克隆 4 份用了 4.2 s（墙钟约 8.9 µs 每项）；对照 `cp -Rc` 13.8 s；`rm -rf` 一份克隆 4.8 s。整个布局的投影见下一行。（Task 10 实测，macOS 27.0.1，内部卷 `/System/Volumes/Data`（96% 满）上的合成树，`tests/budget.rs`）：10 万项（15,080 目录、83,958 文件、962 符号链接）单份 0.74 到 0.91 s（release，7.4 到 9.1 µs 每项；debug 构建里刚造完树紧接着克隆的那次 1.85 s），design §8 的 5 s 满足；**4 路并行克隆 4 份 2.9 到 3.2 s，是单份的 3.5 到 4.0 倍**（每份每项 7.3 到 7.9 µs）——同一个卷上 `clonefile` 不随线程数扩展，吞吐约 13 万项每秒封顶。原先的"4 路不超过单份 2 倍"断言测不过，改成"不比串行差"（≤ 5 倍）的护栏（impl-notes Task 10） |
| 整个布局的 `new`（评审 I3，增补 8 的 T15 与用户一行） | 各仓库逐项相加 | **deps 目录合计约 619,792 项**（评审在真实仓库上数的）：admin 100,269；front 70,999；tool 117,382；slate_viewer 50,125；surgical-timeout 38,647；appointment_patient_viewer 49,861；patient_9_grid 53,362；maxident-sync 49,824；insurance-check 50,111；hr-time-management 20,787；staff-api/vendor 18,425。按上面真实树的单线程成本算是 **12 到 19 s**，按 4 路并行实测的 8.9 µs 每项算约 **5.5 s**，**两者都还没有算源码树和 tool 的 `src-tauri/target`**。所以 design §8 的"新建会话 < 5 s（tool + 全部兄弟库）"对整个布局很可能做不到。下界的对照（本轮，内部卷，合成树）：102,001 项的树单次 `clonefile` 0.67 到 0.70 s，6.6 到 6.8 µs 每项、单线程，照此 62 万项约 4.2 s——每项成本差了三倍以上，原因没有拆开量（卷是 U 盘 SSD 还是内部卷、树的形状都不同），所以这个投影必须在工作区所在的卷上用真实的兄弟库复测，不能用合成树。并行是 session 的事（apfs 的函数是同步的单线程调用，§9）。可选的出路：并行克隆各仓库（session）、按仓库懒克隆 deps、放宽目标——**目标的取舍要交给用户**（经 lead）。§9 的那一行保持"待定"，Task 10 用全部兄弟库复测。Task 10 复测：真实兄弟库在 `/Volumes/GWD/inter` 下，本轮不许碰，**没有跑**；spike 留在 `tests/budget.rs::spike_primitive_latencies`（`STRATA_SPIKE_REPOS=a:b:...` 时并行度 1 到 4 克隆到第一个仓库旁边的临时目录再删掉）。合成树上的投影：62 万项单线程 4.6 到 5.6 s，4 路并行 4.5 到 5.0 s（并行不帮忙，见上一行），仍然没算源码树和 `target`，所以结论不变：目标的取舍交给用户 |
| `walk` | O(项数)，每个目录至少一次 `getattrlistbulk` | 温热 11.7 万项约 0.3 s（约 3 µs 每项）；冷（刚读盘）的 tool 源码树 28,889 项（不含 `node_modules`、`.git`、`target` 等）1.42 s，温热 0.20 s；staff-api 13,586 项冷 0.65 s；admin 85 ms（都是只读遍历）。design §8"全量对账 tool 源码树 < 3 s"冷读也满足；"checkpoint 改 50 个文件 < 200 ms"满足不了全量遍历（温热也要 0.2 s），要靠钩子带来的路径、监听事件、`attrs_of` 和 `max_depth = Some(1)` 的单目录列表缩小范围，这是 overlay 的事。Task 10 合成的 3 万项源码树（3,373 目录、26,321 文件、306 符号链接）刚造完温热遍历：release 0.23 s（7.6 µs 每项），debug 1.99 s（66 µs 每项），都在 3 s 内 |
| `attrs_of`、`open_root`、`open_rooted`、`clone_from_rooted`（克隆本身另算，成本同 `clone_file`、`clone_tree`）、目的地变体、`rename_rooted`（两个父级）、`remove_rooted`、`make_removable`、`set_file_meta`（一次 `fstat` 加至多三次系统调用） | O(深度) 次 `openat` 加一次属性或落地调用 | Task 10 实测（release，温热，深度 2）：`attrs_of` 中位 18.8 µs、p99 40 µs（debug 19.8 µs、31 µs）；100 项单目录的 `list_dir`（`max_depth = Some(1)`）中位 101 µs、p99 212 µs（debug 390 µs、1.7 ms）。50 个文件逐个 `attrs_of` 约 1 ms，离 200 ms 很远 |
| `remove_tree_rooted` | O(项数)：每个目录一次打开加一次列表，每个条目一次 `unlinkat` | 内部卷上参考实现 29 µs 每项（102,001 项 2.98 s），`rm -rf` 38 µs 每项；比同一棵树的 `clonefile` 慢四倍多（§4.2 H）。照 62 万项的整个布局算，删一遍约 18 s（单线程）：什么时候删、要不要放后台、一棵树一个线程，是 store、session 的调度（`retire_dir` 的改名是 O(1)，用户感知的 `rm` 不必等删除结束）。Task 10 在 96% 满的内部卷上：10 万项 4.7 到 5.6 s（47 到 56 µs 每项，release），`rm -rf` 同形状一份 5.3 到 6.4 s（53 到 64 µs）；debug 98 µs 对 80 µs。比参考实现的数字慢（卷更满），两者同量级，照 62 万项算单线程约 30 s |
| `full_fsync` | 一次 `fcntl`，等设备 | USB SSD 上两次测量：文件 10 到 12 ms，建文件加改名之后对目录 4 到 26 ms，没有脏数据时近乎 0（§4.2 G）。扛断电的改名每次要对两个父目录各做一次，成批改名应当合并同步 |
| `watch` | 每个事件 O(1) | 送达延迟 15 ms 到 3.3 s，不是屏障（§4.2 F） |
| `Watcher::flush` | 一次 `FlushAsync` 加一次 `dispatch_sync` 排空 | 空闲时亚微秒；不用 `FlushSync`：空闲时测到约 1.0 s 的停顿（impl-notes Task 9）；回调正忙时要等它返回 |
| `case_fold_key`、`case_fold_eq` | O(长度) | ASCII 走快路径，不分配；非 ASCII 要分配一个键 |

## 6. 失败与恢复

错误时序按 design §9 不变量 6 与增补 3 的细化：写入之前的失败是具名的码，什么都没留下；一旦第一次可见的写入发生，之后的失败一律 `Unknown`，现场原样保留。apfs 自己的操作大多是**单次系统调用**（`clonefile`、`clonefileat`、`openat(O_CREAT|O_EXCL)`、`symlinkat`、`renameatx_np`、`unlinkat`），失败时内核什么都没写，所以可以给具名的码；`ENOSPC` 与 `EIO` 按 types.md §6.3 归 `Unknown`，哪怕实测它们也没留下 dst。多步的有五个：`mkdir_rooted(parents = true)`、`clone_file_rooted` 的"克隆再经句柄设 mode"、`make_removable`（先清标志、再补写位）、`set_file_meta`（清标志、设 mode、设 mtime），以及 `remove_tree_rooted`。前四个第一步生效之后的失败是 `Unknown`；`remove_tree_rooted` 不同（控制者裁定）：遍历一开始就算开始，之后不再有 `Err`，部分失败进报告，因为部分删除的结局是确定的，调用方重试剩下的。apfs 从不清理、从不回滚、从不重试（`EINTR` 除外）；恢复的责任在调用方，它们在克隆之前写 intent 行（增补 3、4），崩溃之后不信任 dst 的存在。

| 情形 | 返回 | 现场 | 恢复 |
| --- | --- | --- | --- |
| dst 以 `/` 结尾；dst 在 src 里面；分量超过 255 个 UTF-16 码元；路径达到 1024 字节；`symlink_rooted` 的目标为空；**root 不规范**（空串、不是绝对路径、以 `/` 结尾、含 `.`、`..` 分量） | `InvalidInput`（`reason`：`trailing_slash`、`dst_inside_src`、`name_too_long`、`path_too_long`、`empty_target`、`root_not_canonical`） | 没写任何东西 | 改参数；config.md 的 V2 在加载时已经拒绝这样的路径文本，符号链接的仓库路径由裁定 "Repo paths in config are realpaths" 挡在加载处（给 T15 的记录，§9） |
| src 不存在；dst 的父目录不存在；`start` 或点查的路径不存在；`watch` 的 root 不存在 | `NoSuchPath` | 没写任何东西 | 目的地变体：先 `mkdir_rooted(parents = true)` 建父级 |
| `clone_tree` 的 src 不是目录（符号链接也算）；路径中间一级是文件或符号链接；`watch` 的 root 不是目录 | `NotDirectory`（`reason`：`symlink` 或 `not_a_directory`） | 没写任何东西。目的地变体撞上符号链接时，被符号链接指着的地方不被碰 | 调用方决定：工作区里多了一个符号链接，是 overlay 要报告的内容，不是 apfs 的错 |
| `clone_file` 的 src 是目录；`open_rooted_file` 的终点是目录；`clone_from_rooted(.., File)` 的源是目录（打开之前被换成目录的也是） | `IsDirectory` | 没写任何东西，没有克隆任何东西（整棵树没有被克隆，store 第 2 轮评审的场景） | 调用方的判断出了偏差，当作写竞争处理 |
| FIFO、socket、设备文件作 `clone_file` 的源；`open_rooted` 的终点是符号链接、socket、设备文件；`open_rooted_file` 的终点是 FIFO；`clone_from_rooted(.., File)` 的源是符号链接、FIFO、socket、设备；`set_file_meta` 的句柄是 FIFO、socket、设备 | `NotFile`（`reason`：`fifo`、`socket`、`device`、`symlink`） | 没写任何东西；FIFO 已经以非阻塞方式打开并关掉，没有挂住 | 符号链接的"内容"是目标，调用方重建它：`attrs_of` 取目标，`symlink_rooted`（不克隆，§5.4） |
| dst 已存在（文件、目录、符号链接，含悬空）；dst 就是 src | `Exists` | 原有的东西与符号链接指向的地方都不被碰；apfs 从不覆盖、从不合并 | 调用方先处理已有的东西 |
| src 与 dst 不在同一个卷 | `CrossVolume`（`detail.src_dev`、`dst_dev`） | 没写任何东西，没有任何字节被复制 | 把会话根、Store 放在与主 checkout 同一个 APFS 卷上（design §7） |
| 卷不是 APFS，或不支持 `clonefile`（`ENOTSUP`，或 `volume_caps` 明确 `clone = Some(false)`） | `Unsupported`（`detail.fs_type`） | 没写任何东西 | 同上 |
| 权限：dst 的父目录不可写，**或者**源树里有不可读的条目（mode 000 的文件或目录）；只读卷 | `ReadOnly`（`EACCES`、`EPERM`、`EROFS`；message 同时写出两种可能） | `clone_*`：dst 不存在（`clonefile` 原子，实测）。目的地变体：同 | 修权限；若是源树里的不可读条目，调用方对这棵树要么排除它，要么先修 |
| 磁盘满（`ENOSPC`）、`EIO`、dataless 文件（`EDEADLK`）、描述符耗尽 | `Unknown`（`detail.errno`、`dst_exists`） | `clone_*`：dst 不存在（实测 `ENOSPC`）。克隆是写时复制，克隆之后的覆盖写才可能耗尽空间，那是调用方写文件时的错误 | 不重放、不清理，状态上报（不变量 6）；空出空间后调用方重试整个操作 |
| `mkdir_rooted(parents = true)` 建出了几级之后失败 | `Unknown`（`detail.created`） | 已建的目录留着 | 调用方按 `created` 决定留还是删 |
| `clone_file_rooted` 克隆成功而经句柄设 mode 失败（含 `set_file_meta` 的 `hard_link` 拒绝：dst 在中间被换成了共用的 inode） | `Unknown`（`dst_exists = true`） | 克隆出的文件留着，mode 是源的；被换成硬链接时，根外的文件没有被改 | 调用方自己 `set_file_meta` 或删除重来 |
| `rename_rooted`：`Excl` 的 to 已存在（含悬空符号链接）；`Swap` 缺一边；RepoPath 相同；to 在 from 之下 | `Exists`；`NoSuchPath`（`from_missing`、`to_missing`）；`InvalidInput`（`same_path`、`into_itself`） | 什么都没动（单次系统调用） | 调用方先处理已有的东西 |
| `rename_rooted`：跨卷（含跨嵌套挂载点）；`Swap` 在 HFS+ 上 | `CrossVolume`；`Unsupported` | 什么都没动 | 放在同一个卷上；`Swap` 改用别的步骤 |
| `rename_rooted`、`remove_rooted`：权限（0555 的目录改父级、父目录不可写、带 `uchg`）、只读卷 | `ReadOnly`，`detail.reason` 是 `reparent_needs_write`、`immutable`、`no_permission`、`read_only_volume` | 什么都没动 | 调用方先 `make_removable`（句柄由 `open_rooted` 取，先清标志、再补 u+w）再重试；apfs 自己不放宽（A10） |
| `remove_rooted(File)` 的目标是目录；`remove_rooted(EmptyDir)` 的目标非空、是文件或符号链接 | `IsDirectory`；`NotEmpty`；`NotDirectory` | 什么都没动；符号链接指向的地方不被碰 | 调用方选对类型；非空的树经 `remove_tree_rooted`，不自己写递归（增补 7、8）。`NotEmpty` 说明有别的写者（裁定 "NotEmpty from apfs"）：overlay 的 restore 当写竞争（第一次可见的写之前 `Busy`，之后 `Unknown`），store 的 gc 清扫跳过这个目录；T15 给 types.md 里 `NotEmpty` 的"谁返回"加 `apfs`，含义放宽成"要删的目录非空"（§9） |
| `remove_rooted(EmptyDir)` 的目标是挂载点 | `CrossVolume`（`reason = mount_point`，`detail.src_dev`、`dst_dev` 是父目录与目标的设备号；`rmdir` 晚到的 `EBUSY` 同样映射） | 什么都没动 | 不删挂载点；`remove_tree_rooted` 遇到嵌套的挂载点记 `stuck`（`OtherDevice`），目标本身是挂载点则在遍历之前拒绝（同一个码） |
| `remove_tree_rooted`：遍历之前的拒绝——目标不存在；某一级父目录是符号链接或文件；目标是目录而且本身是挂载点；父目录不可写或带不可变标志（目标是什么种类的条目都不拒绝） | `NoSuchPath`；`NotDirectory`（`symlink` 或 `not_a_directory`）；`CrossVolume`（`reason = mount_point`）；`ReadOnly`（`no_permission` 或 `immutable`） | 什么都没动，树原样 | 调用方改正参数或先处理父目录（父目录的权限是调用方的，A10） |
| `remove_tree_rooted`：遍历开始之后没删干净——ACL 禁止删除的条目、清不掉的标志（`SF_*`；属主既没有读位也没有执行位又带 `uchg` 的条目，文件、目录都一样）、共用的 inode（硬链接）上的标志、嵌套的挂载点、条目在遍历中被别的进程换掉或又写进新条目、深度超限、`EBUSY`、unlink 结果不确定（`EIO` 一类） | **`Ok(RemoveReport)`**，不是 `Err`：`removed`、`flags_cleared`、`made_writable`、`stuck`（每项带 `reason`、`errno`、`detail`，原因见 §5.4 的 stuck 原因表）、`stuck_total`；`is_complete()` 为假 | 能删的都删了，`stuck` 里的条目（连同它们的祖先目录）留着；清掉的标志和补上的权限**不会还原**（如实记在 `flags_cleared`、`made_writable`）；树之外的东西一概没碰，共用的 inode 的标志没动 | 调用方看 `is_complete()` 并按报告决定：重试（别的进程也许已经停写；store 的 gc 下一轮再删）、留着、或上报；不重放整个删除。`UnknownOutcome` 的条目可能已经没了，也可能还在，不要假定 |
| `clone_from_rooted`：源的某一级父目录是符号链接；`kind = File` 而源本身是符号链接、FIFO、socket、设备；`kind = Dir` 而源不是目录或是符号链接 | `NotDirectory`（`symlink`）；`NotFile`（`symlink`、`fifo`、`socket`、`device`）；`NotDirectory`（`symlink` 或 `not_a_directory`） | 没克隆任何东西，dst 不存在（种类核对在 `fclonefileat` 之前，§5.4 第 4 步） | 符号链接的"内容"是目标，调用方重建它：`attrs_of` 取目标，`symlink_rooted`（不克隆） |
| `clone_from_rooted`：打开源之后，源路径被换成指向根外的符号链接或别的文件 | 成功，克隆出的是打开时的那个对象 | 根外的内容一个字节也没进 dst | 无需处理（§4.2 A） |
| `clone_from_rooted`：源与 dst 不在同一个卷；dst 已存在；dst 的父目录不存在 | `CrossVolume`；`Exists`；`NoSuchPath` | dst 不存在（已存在的那个原样），没有字节被复制 | 同 `clone_file` |
| `full_fsync` 失败 | `Unsupported`（`ENOTSUP`）或 `Unknown`（`detail.errno`） | 改名等操作本身已经发生；持久性不确定 | 调用方按"没有落盘"处理：重试或上报 |
| `make_removable`、`set_file_meta`：非目录而 `st_nlink > 1`；`SF_IMMUTABLE`、`SF_APPEND` 置位；（`set_file_meta`）写侧的 `EACCES`、`EPERM`、`EROFS` | `ReadOnly`（`hard_link`、`immutable`（带 `detail.flags`）、`no_permission`、`read_only_volume`） | 什么都没改（核对和第一处改动都在任何生效的改动之前） | 共用的 inode 不是 apfs 能处理的：先让别的名字消失；`SF_*` 要 root，调用方上报 |
| `make_removable`、`set_file_meta` 第一处改动生效之后失败（清了标志而补写位失败；改了 mode 而设 mtime 失败） | `Unknown`（`detail.errno`） | 已做的改动留着（标志已清、mode 已改） | 不重放，状态上报（不变量 6）；`make_removable` 是幂等的，调用方可以重试 |
| 进程在 `clonefile` 中途被杀 | — | 系统调用要么完成要么不可见（进程被杀的情形没有观察到半成品）。**断电**的情形没有测过：不能假定 dst 完整 | 调用方在克隆之前写 intent 行；崩溃后 `recover` 不信任 dst，按 intent 行处理（增补 3、4） |
| `walk`：目录不可读 | 一条 `Err(Unknown)`（`reason = unreadable_entry`），遍历继续 | 目录条目本身已产出，它的子树缺 | 调用方决定这次扫描是否有效；overlay 把它当作扫描失败（tracker 状态不变） |
| `walk`：条目在读与打开之间消失，或被换成文件、符号链接 | 不报错，跳过 | 子树不出现。遍历本来就不是快照（A5） | 下一次扫描或监听事件会补上 |
| `walk`：深度超过 `MAX_WALK_DEPTH` | 一条 `Err(TooLarge)`（`reason = too_deep`；码的含义宽于 types.md §6.2，§9） | 那棵子树不遍历，其余照常 | |
| `walk`：根在遍历中被换掉 | 末尾一条 `Err(Busy)`（`reason = root_changed`；码的含义宽于 types.md §6.2，§9） | 已产出的条目来自旧目录 | 丢弃这次遍历的全部结果，重来（`move_workspace` 之后 overlay 本来就要冷扫） |
| `walk`：名字不是 UTF-8 | 一条 `Err(NotText)` | APFS 上不会发生（`EILSEQ`），解析器仍然防住 | |
| `watch`：FSEvents 流建不起来、启动失败 | `Unknown` | 没有 Watcher | 调用方退化为只做全量对账（design §6.2） |
| `Watcher::flush` 之后事件仍未到齐 | 不是错误，没有返回值 | 事件还在 fseventsd 那一头（§4.2 F）；保证的只有排空之前入队的回调都跑完了 | overlay 的做法：冷扫（不靠事件的全量遍历，不重读内容）、钩子路径加 `flush`、或一次工具调用的滞后（增补 8） |
| `Watcher::flush` 在任何一个 Watcher 的回调里被调用 | debug 构建 `debug_assert!` 失败；release 构建立即返回、什么都不做 | 没有任何东西被改。自己的回调里放行会 `SIGTRAP`；两个回调互相排空会死锁（§4.2 F） | 调用方的 bug：`flush` 不得在任何一个 strata `Watcher` 的 `on_event` 里调用（daemon 在回调线程之外调 `Tracker::flush`） |
| `drop(Watcher)` 在回调里 | 正常返回 | 流已停；`Release` 与状态的释放留给私有队列上随后的 `dispatch_async` 块（闭包的捕获物在那个块里 drop，恰好一次）；当前回调继续跑完，之后没有第二次回调 | 无需处理（§5.7 第 6 步） |
| `nearest_existing_ancestor`：相对路径；`stat` 遇到 `EACCES` | `io::Error`（`InvalidInput`；原 errno） | 只读 | 调用方改正参数或权限；要变成 `StrataError` 就 `classify_io_error(.., IoSide::Read, ..)`（两种都是 `Unknown`，调用方的 bug 或读侧的权限错误） |
| 运行中事件丢失、合并、根被改名 | 不是错误：`Dropped`、`MustRescan` | — | 消费者全量或按子树重扫，排除 `exclude` 里的目录（§5.7） |
| `on_event` 回调 panic | 截住、记日志，补发一个 `Dropped` | Watcher 继续 | 消费者因此重扫；回调的 bug 由调用方修 |

## 7. 测试清单

除标明 `#[ignore]` 的以外都默认运行，包括用 `hdiutil` 稀疏镜像的：建不了镜像时测试**失败**并说明原因，不静默跳过（两卷、不支持的卷、满盘、大小写敏感、嵌套挂载都要镜像，不需要 root）。会改文件系统的测试都在 `tempfile` 目录或镜像里做，从不碰 `/Volumes/GWD/inter`。测试里的 `std` 调用只用来造场景和读结果，被测的是 apfs 的函数。

| 测试名 | 断言 | 来源（矩阵行 或 新增） |
| --- | --- | --- |
| `clone_dir_refuses_cross_volume` | 两个 `hdiutil` 建的 APFS 镜像 A、B（两个卷）：A 上有一棵带文件、子目录和符号链接的树。`clone_tree(A/tree, B/clone)` → `CrossVolume`，`detail.src_dev` 不等于 `detail.dst_dev`；A 的树里放一个 64 MiB 的随机文件（两个镜像各 256 MB 的稀疏镜像）。B 上没有 `clone`，B 的可用空间前后之差小于 1 MiB（APFS 的元数据让"精确相等"不可靠，评审 M14；一旦偷偷复制至少会少 64 MiB，1 MiB 的容差又稳又灵敏，做法同 `clone_then_small_write_allocates_only_the_touched_extent`），A 上的树逐字节不变。对 A 上另一个目录的 `clone_tree` 照常成功。这是 design 的"跨卷退化成复制"风险（Review Focus 1）：`clonefile` 自己返回 `EXDEV`，所以要钉住的不变量是 crate 里没有任何复制回退（§4.3 A2） | 新增 |
| `clone_file_refuses_cross_volume` | 同一对镜像：`clone_file(A/file, B/copy)` → `CrossVolume`；A 到 HFS+ 镜像同样是 `CrossVolume`，不是 `Unsupported`（APFS 到任何别的卷都是 `EXDEV`）；dst 的父目录不存在而且在另一个卷 → `NoSuchPath`，dst 已存在而且在另一个卷 → `Exists`（内核的优先级，§4.2 A） | 新增 |
| `clone_on_unsupported_volume_is_unsupported` | HFS+ 镜像里把 src 克隆到同一个镜像（内核 `ENOTSUP`）→ `Unsupported`，`detail.fs_type = "hfs"`；MSDOS 镜像里（`volume_caps` 明确 `clone = Some(false)`）在调用 `clonefile` 之前就 `Unsupported`；两种都没有留下 dst | layerfs-project/tests/init_sqlite.rs::an_unsupported_platform_refuses_the_store_before_any_file_exists |
| `clone_refusals_leave_no_residue` | 跨卷、不支持的卷、dst 已存在、dst 的父目录不存在这几种拒绝之后：dst 不存在，两个卷上都没有临时文件或半成品目录，src 树逐字节不变；`clone_file`、`clone_tree_rooted`、`clone_from_rooted` 的拒绝同理（后两者的那一半随各自的任务补上，§8）。写入之前拒绝、无残留（design §10 的 clonefile 跨卷退化项） | sdk/js/test/verify/direct_refusal_real.mjs |
| `clone_tree_error_precedence` | 表驱动，一种构造一个错误：src 不存在 → `NoSuchPath`；src 是文件 → `NotDirectory`；src 是指向目录的符号链接 → `NotDirectory`（`reason = symlink`）；dst 已存在（目录、文件、悬空符号链接）→ `Exists`；dst 就是 src → `Exists`；dst 的父目录不存在 → `NoSuchPath`；dst 的父级是文件 → `NotDirectory`；dst 末尾带 `/` → `InvalidInput`（`trailing_slash`）；dst 在 src 里面 → `InvalidInput`（`dst_inside_src`）。优先级：dst 已存在而且在另一个卷 → `Exists`，父目录不存在而且在另一个卷 → `NoSuchPath`，这两者都不是才是 `CrossVolume`。每一种之后原有的东西都没被碰，也没有新的 dst | 新增 |
| `clone_tree_preserves_content_modes_links_and_holes` | 源树含：mode 0755、0600、0444 的文件，中间有 1 MiB 洞的稀疏文件，相对、绝对、悬空、成环的符号链接，空目录，20 层深的嵌套。克隆后逐项相同：字节、mode、符号链接的目标原样（不被跟随）、空目录还在、洞仍是洞（`st_blocks` 与源同量级，远小于逻辑大小）；源树不变；克隆出来的文件属主是调用者 | 新增 |
| `clone_tree_keeps_xattrs_and_flags_but_drops_acls_and_setuid` | 带 `user.test` 扩展属性的文件克隆后仍有；`chflags uchg` 的文件克隆后同样不可变（删除它得 `EPERM`；测试结束前先 `chflags nouchg` 再清理）；ACL 不被复制；setuid、setgid 位被清掉（`4755` 变 `0755`）；源里的文件和空目录先设成 2020 年的 mtime，克隆出的同名文件和空目录仍是 2020 年，有子项的目录的 mtime 是克隆那一刻（§4.2 G） | 新增 |
| `clone_tree_makes_independent_hard_links` | 源里同一个 inode 的两个名字，克隆后是两个 inode（`nlink` 各为 1）；改其中一个不影响另一个；`clone_file` 克隆一个有两个名字的文件，结果也是独立的（裁定 T4：不保留硬链接） | 新增 |
| `clone_tree_clones_fifos_and_sockets_as_objects` | 目录树里的 FIFO 与 unix socket 文件，克隆后仍是 FIFO、socket（用 `std::fs::symlink_metadata` 看文件类型），不是普通文件，也不报错；顶层源是 FIFO 时 `clone_tree` → `NotDirectory`（socket 用 python 或相对路径建，避开 `sockaddr_un` 的 104 字节上限） | 新增 |
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
| `nearest_existing_ancestor_climbs_to_the_first_existing_path` | 评审 M17：已存在的路径 → 它自己；叶子不存在 → 父目录；连缺 5 级 → 第一个存在的祖先；路径的某一级是普通文件（`file/x/y`）→ 那个文件（`ENOTDIR` 也往上爬）；中间有悬空符号链接（`dang/x`）→ 符号链接所在的目录；经 `/tmp` 符号链接给出的路径 → 返回值以 `/tmp` 开头，不是 `/private/tmp`（不做 realpath）；相对路径 → `io::ErrorKind::InvalidInput`；路径上有一个 mode 000 的目录而 `stat` 得 `EACCES` → 原样返回 `PermissionDenied`，不往上爬。接上 `same_volume`：镜像挂载点之下尚不存在的 `mnt/new/deeper`，`same_volume(&nearest_existing_ancestor(..)?, 镜像里的文件)` 为 `true`，对内部卷上的文件为 `false`（config.md §4.3 的规则一份实现）。评审 N8：两种错误交给 `classify_io_error(&e, IoSide::Read, path)` 都得 `Unknown`——相对路径的 `InvalidInput`（没有 `raw_os_error`）和 mode 000 目录下的 `PermissionDenied`（`detail.errno` 为 `EACCES`）；读侧的权限错误从不是 `ReadOnly` | 新增 |
| `volume_caps_per_filesystem` | 用镜像：APFS → `fs_type = "apfs"`、`clone = Some(true)`、`case_sensitive = false`、`write_gen_count = true`；大小写敏感的 APFS → `case_sensitive = true`；HFS+ → `clone = None`；MSDOS → `clone = Some(false)`、`write_gen_count = false`；路径不存在 → `NoSuchPath` | 新增 |
| `errno_mapping_table` | 单元测试，表驱动，直接调公开的 `classify_io_error`，逐行对 §4.2 I：`ENOENT`→`NoSuchPath`，`EEXIST`→`Exists`，`ENOTDIR`→`NotDirectory`，`EISDIR`→`IsDirectory`，`ENOTEMPTY`→`NotEmpty`，`EXDEV`→`CrossVolume`，`ENOTSUP`、`EOPNOTSUPP`→`Unsupported`，`ENAMETOOLONG`、`EINVAL`→`InvalidInput`；`IoSide::Write` 的 `EACCES`、`EPERM`→`ReadOnly`（`reason = no_permission`），`EROFS`→`ReadOnly`（`reason = read_only_volume`），`IoSide::Read` 的同样 errno→`Unknown`；`ENOSPC`、`EIO`、`EDEADLK`、`EMFILE`、`ELOOP`→`Unknown`，`EBUSY`→`Unknown`（`reason = busy`）；`IoSide::AfterWrite` 对上面每一个 errno 都是 `Unknown`；没有 `raw_os_error` 的 `io::Error`→`Unknown`；每个错误的 `detail` 都带 `errno`（数字）与 `path`。评审 N8 的例子：`create_file_rooted` 返回的句柄上 `write_all` 失败，文件已经建出来了，调用方传 `IoSide::AfterWrite`，`EROFS`、`EACCES` 也得 `Unknown`，不是写侧的 `ReadOnly`（现场留着一个文件，不能当"什么都没改"） | 新增 |
| `classify_io_error_matches_real_syscalls` | 用真实的系统调用造出每个 errno，再把得到的 `io::Error` 交给 `classify_io_error`，核对 §4.2 I 的表（这张表因此是量出来的，不是抄来的）：`File::open` 一个不存在的路径（`ENOENT` → `NoSuchPath`）；`OpenOptions::create_new` 一个已有的文件（`EEXIST` → `Exists`）；把普通文件当目录用（`ENOTDIR` → `NotDirectory`）；对目录 `fs::write`（`EISDIR` → `IsDirectory`）；`remove_dir` 非空目录（`ENOTEMPTY` → `NotEmpty`）；`fs::rename` 跨两个镜像（`EXDEV` → `CrossVolume`）；HFS+ 镜像里的 `clonefile`（`ENOTSUP` → `Unsupported`）；256 个码元的名字（`ENAMETOOLONG` → `InvalidInput`）；mode 000 的文件读（`EACCES` → 读侧 `Unknown`、写侧 `ReadOnly`）；只读挂载的镜像上建文件（`EROFS` → `ReadOnly`，`reason = read_only_volume`）；带 `uchg` 的文件 `remove_file`（`EPERM` → 写侧 `ReadOnly`，`reason = no_permission`：细分成 `immutable` 是各个操作的事，这里不分）；填满的镜像（`ENOSPC` → `Unknown`）。`IoSide::AfterWrite` 对每一个都给 `Unknown` | 新增 |
| `rooted_open_opens_files_and_directories` | 在根之下按 `RepoPath` 打开普通文件，读到的内容正确；打开目录得到可当 dirfd 用的句柄（调用方拿它做 `make_removable`、`full_fsync`，或当 `openat` 的 dirfd）；`open_rooted_file` 对目录 → `IsDirectory`；空串、`.`、`./dir/file`、`../file`、`dir/../file`、绝对路径连 `RepoPath` 都构造不出来，apfs 的函数没有别的路径入口；根的祖先里有符号链接（`/tmp/…`）时照常打开 | tgrep-core/src/rooted/tests.rs::regular_opens_and_literal_path_validation |
| `open_root_returns_the_root_handle` | `open_root` 返回的句柄是目录（`metadata().is_dir()`），可以当 dirfd 用：对它 `openat` 一个子项成功；根的祖先里有符号链接（`/tmp/…`）时照常打开；根本身是符号链接 → `NotDirectory`（`reason = symlink`），指向的目录没被碰；根是普通文件 → `NotDirectory`（`not_a_directory`）；不存在 → `NoSuchPath`（对根句柄的 `full_fsync` 在 `full_fsync_accepts_file_and_directory_handles` 里测）。评审 N5、裁定 "Rooted roots are canonical"：root 不规范时在碰文件系统之前就 `InvalidInput`（`root_not_canonical`）：`root/`、`root//`、`root/.`、`root/./sub`、`root/../root`，以及相对路径 `repo`、`a/b`、`.hidden`（点开头的相对路径也不是绝对路径）；根是符号链接 `lnk` 时，`lnk` → `NotDirectory`（`symlink`），而 `lnk/`、`lnk//`、`lnk/.` 都 → `InvalidInput`（`root_not_canonical`），**不是**打开了链接指向的目录（内核对带结尾斜杠的路径跟随符号链接，`O_NOFOLLOW` 管不到，§4.2 E）；其余函数的同一检查见 `rooted_roots_must_be_canonical` | 新增 |
| `rooted_roots_must_be_canonical` | 评审 N5、裁定 "Rooted roots are canonical"。表驱动：对 `lnk/`（指向目录的符号链接加斜杠）、`root/`、`root/.`、`root/..`、`root/sub/../sub`、空串、相对路径 `repo`、`a/b`、`./repo`、`.hidden` 这几种不规范的 root，逐个调用每个以 root 为参数的函数：`open_root`、`open_rooted`、`open_rooted_file`、`clone_from_rooted`（`src_root`）、`clone_file_rooted`、`clone_tree_rooted`、`create_file_rooted`、`mkdir_rooted`（`parents` 取两个值）、`symlink_rooted`、`rename_rooted`（`Excl`、`Swap`）、`remove_rooted`（`File`、`EmptyDir`）、`remove_tree_rooted`、`attrs_of`：全部 `InvalidInput`（`root_not_canonical`）；`walk`、`walk_with`、`list_dir` 的第一项是同样的 `Err`。每一种之后，`lnk` 指向的目录与根里的内容、属性逐字节不变，dst 一律不存在。规范的 root（含祖先里有符号链接的 `/tmp/x`）照常工作；**点开头但不是 `.`、`..` 的分量也必须照常工作**（评审 M4：免得有人写成 `contains("/.")` 或 `starts_with('.')` 一类的检查）：`<tmp>/sessions/.repos/<id>/<repo>.git`（gitio 的影子仓库根）、`<tmp>/.staging/<hex>`（`export_tree` 的 dst 的父目录）、`<tmp>/..hidden`、`<tmp>/a..b`、`<tmp>/...` 这几个真实存在的目录作 root，`open_root`、`open_rooted_file`、`create_file_rooted`、`attrs_of` 都成功。`watch` 不走这张表（它对 root 做 realpath，§5.7），只有"不是绝对路径"这一条相同：相对路径 → `InvalidInput`（`root_not_canonical`），`root/`、`root/.` 这类绝对路径照常 realpath 后监听 | 新增 |
| `rooted_open_treats_colon_and_backslash_names_literally` | 名字 `a:b`、`C:relative.txt`、目录 `dir` 里的 `name:stream`、含反斜杠的 `a\b.txt`、含空格与 NFD 字符的名字，都按字面打开、读回，不当作盘符或数据流；`a\b.txt` 与 `a/b.txt` 是两个文件 | tgrep-core/tests/corrupt_index_reader.rs::builder_and_reader_round_trip_unix_colon_filenames |
| `rooted_open_fifo_is_nonblocking_and_symlinks_are_refused` | 在子进程里跑，20 秒死线（到点杀掉并判失败，防止测试挂死）：树里有一个没有写端的 FIFO：`open_rooted` 立即返回，`metadata().file_type().is_fifo()` 为真，读它得到 `WouldBlock` 或 EOF；`open_rooted_file` → `NotFile`（`fifo`）；同一目录里指向根内、根外文件的符号链接 → `NotFile`（`symlink`），链接目标的内容从未被读到 | tgrep-core/src/rooted/tests.rs::batch_child_fifo_open_is_nonblocking_and_never_follows_symlinks |
| `rooted_open_rejects_a_directory_swapped_for_a_link` | 接缝 `before_open` 在打开中间目录 `dir` 之前把它换成符号链接，指向根外（放着 `.env` 之类的文件）和指向根内的另一个目录两种：`open_rooted` 都 → `NotDirectory`（`reason = symlink`），根外文件的内容从未被读到 | tgrep-core/src/rooted/tests.rs::directory_swap_before_open_rejects_outside_and_inside_links |
| `rooted_open_keeps_the_parent_handle_after_a_swap` | 接缝在最后一个分量 `openat` 之前，把已经打开的父目录的路径换成指向根外的符号链接：这次调用仍读到原目录里的原内容（句柄没被重定向）；之后的新调用 → `NotDirectory` | tgrep-core/src/rooted/tests.rs::directory_swap_after_open_cannot_redirect_parent_handle |
| `rooted_open_pins_the_root` | 接缝在第一级分量之前把根目录换成符号链接：这次调用仍读到原根里的内容；之后的新调用 → `NotDirectory`（根是符号链接）。`walk` 上对应的"根身份已变"检查见 `walk_reports_a_swapped_root` | tgrep-core/src/rooted/tests.rs::pinned_root_cannot_be_retargeted_between_verification_and_open |
| `rooted_open_rejects_a_final_symlink_swap` | 接缝在最后一级之前把目标文件换成指向根外秘密文件的符号链接：`open_rooted` → `NotFile`（`reason = symlink`，`errno` 为 `ELOOP`），秘密文件的内容从未被读到；`open_rooted_file` 同 | tgrep-core/src/rooted/tests.rs::final_symlink_swap_is_rejected |
| `rooted_open_file_refuses_a_fifo_swapped_in` | 接缝在最后一级之前把目标文件换成 FIFO（子进程加死线）：`open_rooted_file` 立即 → `NotFile`（`fifo`），不阻塞；`open_rooted` 返回非阻塞的 FIFO 句柄，调用方 `fstat` 看得出它不是普通文件 | tgrep-core/src/rooted/tests.rs::fifo_swap_before_final_open_is_nonblocking |
| `rooted_open_error_codes` | 不存在 → `NoSuchPath`；中间一级是文件 → `NotDirectory`（`not_a_directory`）；终点是 unix socket → `NotFile`（`socket`）；`open_rooted(Path::new("/dev"), "null")`（字符设备）→ `NotFile`（`device`）；mode 000 的文件 → `Unknown`（`errno` 为 `EACCES`）；用 NFC 拼写能打开磁盘上以 NFD 存的名字 | 新增 |
| `rooted_open_name_too_long_is_invalid_input` | 评审 M9：`open_rooted` 与 `open_rooted_file` 的最后一个分量 256 个码元（另一轮是中间一级 256 个码元）→ `InvalidInput`（`name_too_long`，`detail.errno` 为 `ENAMETOOLONG`），不是 `Unknown`；255 个码元的名字（先用 `std` 建出来）照常打开；其余 errno 一律 `Unknown`（`classify_io_error` 的表钉住） | 新增 |
| `rooted_destination_refuses_a_symlinked_parent` | 根里有指向根外目录的符号链接 `link` 和指向根内另一个目录的符号链接 `inner`：对每个目的地变体（`clone_file_rooted`、`clone_tree_rooted`、`create_file_rooted`、`symlink_rooted`、`parents` 取两个值的 `mkdir_rooted`），路径为 `link/x`、`inner/x`、`link/a/b` 时 → `NotDirectory`（`reason = symlink`）；根外目录与 `inner` 指向的目录的列表和内容前后不变，根里也没有新东西（改名与删除类各有自己的行：`rename_rooted_refuses_symlinked_parents_on_both_sides`、`remove_rooted_refuses_symlinked_parents`、`remove_tree_rooted_refuses_before_touching_anything`）。这是增补 3 的测试：被跟踪的符号链接在种子文件的父目录上，写入永远出不了会话根 | 新增 |
| `rooted_destination_never_overwrites` | dst 的位置上已有文件、目录、指向文件的符号链接、悬空符号链接时，每个目的地变体（`mkdir_rooted` 取 `parents = false`）→ `Exists`，原有的东西与符号链接指向的地方都没被碰；`mkdir_rooted(parents = true)` 对已有目录成功，对已有的文件或符号链接叶子 → `Exists` | 新增 |
| `rooted_destination_swap_after_resolution_stays_inside` | 接缝（落地的 `*at` 调用之前触发的那一次）把父目录的路径换成指向根外的符号链接：写入仍落在原来被钉住的父目录里，根外目录没有新东西；`clone_tree_rooted`、`create_file_rooted`、`symlink_rooted`、`mkdir_rooted` 同理（删除类见 `remove_rooted_refuses_symlinked_parents`） | 新增 |
| `remove_rooted_refuses_symlinked_parents` | 根里有指向根外目录的符号链接 `link` 和指向根内另一个目录的符号链接 `inner`，被点名的 `x`（文件）与 `d`（空目录）事先放在符号链接指向的目录里：`remove_rooted(link/x, File)`、`inner/x`、`remove_rooted(link/d, EmptyDir)`、`inner/d`、`link/a/b` 都 → `NotDirectory`（`reason = symlink`），根外目录与 `inner` 指向的目录前后不变（`x`、`d` 都还在）；接缝在落地的 `unlinkat` 之前，把已经解析过的父目录的路径换成指向根外的符号链接：删掉的是被钉住的原目录里的条目，根外目录里的同名条目还在，之后的新调用 → `NotDirectory` | 新增 |
| `mkdir_rooted_parents_semantics` | `parents = true` 一次建出 `a/b/c`；再调一次成功（幂等）；中间一级是文件 → `NotDirectory`，没有新东西；`mode = 0o555` 建 `p/q`，`p` 建出之后 `q` 建不进去 → `Unknown`，`detail.created` 为 `["p"]`，`p` 留着 | 新增 |
| `clone_file_rooted_sets_the_requested_mode` | src 的 mode 是 0600：`mode = Some(0o644)` 后 dst 是 0644，`Some(0o755)` 得 0755，`None` 保持 0600；src 的 mode 不变；`Some(0o4755)` 的 setuid 位被屏蔽；src 是符号链接时 mode 参数无效。设 mode 是经句柄做的（`set_file_meta`，§5.4）：接缝在克隆之后、取回 dst 的那一次 `openat` 之前（同一个名字第二次被问到）把 dst 换成指向根外一个 0600 文件的硬链接 → `Unknown`（`dst_exists` 为 true），根外的文件仍是 0600，`ctime` 也没变——这是 store 的 writer 复现过的场景（经路径改 mode 会把根外的文件改成 0644）；换成指向根外文件的符号链接 → `Unknown`，根外文件不变（`O_NOFOLLOW`）；src 带 `uchg` 时 `Some(..)` → `Unknown`（`errno` 为 `EPERM`，`dst_exists` 为 true，克隆留着），`None` 成功，调用方对 `open_rooted_file` 取得的句柄 `set_file_meta(.., { clear_flags: true, mode: Some(..) })` 之后成功。两个接缝不会混淆（评审 n4）：dst 就叫 `mode`（再各试一次 `flags`、`mtime`），src 是 0600、`Some(0o644)`：`before_open` 恰好收到 `(0, "mode")`（克隆）与 `(1, "mode")`（取回）两次，`before_meta` 恰好收到一次 `(0, Mode)`；按名字过滤的 `before_open` 钩子不会把 `(.., "mode")` 当成改 mode 的一步；src 的 mode 已经是要设的值时，`before_meta` 一次也不触发（不调 `fchmod`），`before_open` 照旧两次 | 新增 |
| `clone_tree_rooted_is_atomic_and_refuses_cross_volume` | 根在镜像 B 上、src 在镜像 A 上：`clone_tree_rooted` → `CrossVolume`，B 上没有 dst；父目录缺失 → `NoSuchPath`；dst 在 src 里面 → `InvalidInput`；成功时 dst 与 `clone_tree` 的结果逐项相同 | 新增 |
| `create_file_rooted_and_symlink_rooted_basics` | `create_file_rooted` 返回的句柄可写，文件一开始是空的，mode 受 umask 影响、对句柄 `set_file_meta(.., { mode: Some(..) })` 后精确（新建的 inode 只有一个名字，核对必过），第二次调用 → `Exists`；`symlink_rooted` 建出的链接，`readlink` 得到的字节与给定的目标完全相同（含非 UTF-8 字节和相对路径 `../x`）；空目标 → `InvalidInput`（`empty_target`） | 新增 |
| `rename_rooted_excl_never_overwrites` | 根里有 `a/x`（文件）、`b/y`（文件）、`d/`（目录）、`dang`（悬空符号链接）：`Excl` 的 `a/x → b/y`、`a/x → d`、`a/x → dang` 都 → `Exists`（`detail.path` 是 to），两边原有的东西和符号链接指向的地方都没被碰；`a/x → b/z` 成功，用 `std::fs` 比较：内容、inode、mtime 不变，ctime 变新，两个父目录的 mtime 变新；`from` 不存在 → `NoSuchPath`（`from_missing`）；`from` 与 `to` 的 RepoPath 相同 → `InvalidInput`（`same_path`），文件没动 | 新增 |
| `rename_rooted_excl_renames_links_and_case_only_spellings` | `from` 是指向目录的符号链接（或悬空的）：改名的是链接本身，目录和里面的东西不动；大小写不敏感的 APFS 镜像上，`Excl` 的 `Foo.txt → foo.txt` 成功，`std::fs::read_dir` 看到的名字变成 `foo.txt`，内容和 inode 不变；大小写敏感的镜像上 `a → A` 是两个不同的名字，成功，`A` 已存在时 → `Exists`；`from` 在磁盘上是 NFD 拼写，用 NFC 的 RepoPath 能找到并改名 | 新增 |
| `rename_rooted_swap_exchanges_two_existing_entries` | `Swap`：文件与文件、文件与目录、目录与目录（同一个父目录和不同的父目录）、符号链接与文件，两个名字下的东西互换，每个对象的 inode 和内容跟着自己走（`std::fs` 比 inode、读内容）；任何一边不存在 → `NoSuchPath`（`from_missing` 或 `to_missing`），另一边没动；overlay 的 restore 用法：暂存目录里的新文件换进工作区旧文件的位置，旧文件换到暂存目录里，两份内容都完整；把目录与它自己的后代对调 → `InvalidInput`；两个镜像之间 → `CrossVolume`；HFS+ 镜像上 → `Unsupported`（`detail.fs_type` 为 `hfs`） | 新增 |
| `rename_rooted_error_mapping` | 表驱动，一种构造一个错误，每种之后两个路径下原有的东西都没被碰：`to` 的父目录不存在 → `NoSuchPath`；`to` 的父级是文件 → `NotDirectory`；目录改名到自己的后代下（`a → a/b/c`）→ `InvalidInput`（`into_itself`）；名字 256 个码元 → `InvalidInput`（`name_too_long`）；两个 APFS 镜像之间 → `CrossVolume`（`src_dev` 不等于 `dst_dev`）；根下嵌套挂载点的两侧之间（两个方向）→ `CrossVolume`；父目录是 0555 → `ReadOnly`（`no_permission`）；带 `uchg` 的文件 → `ReadOnly`（`immutable`；测试结束前先清标志再清理） | 新增 |
| `rename_rooted_refuses_symlinked_parents_on_both_sides` | 根里有指向根外目录的符号链接 `out` 和指向根内另一个目录的符号链接 `inn`：`from` 在 `out/x` 或 `inn/x`、`to` 在 `out/y` 或 `inn/y` 的各种组合（`Excl` 与 `Swap` 各一遍）都 → `NotDirectory`（`reason = symlink`），根外目录和 `inn` 指向的目录的内容前后不变；接缝在 `to` 的父级解析之前，把已经解析过的 `from` 的父目录的路径换成指向根外的符号链接：这次改名仍作用在被钉住的原目录上，根外目录里没有新东西，之后的新调用 → `NotDirectory` | 新增 |
| `rename_rooted_directory_reparenting_needs_write_permission` | 增补 4 的"封存先改名、后 chmod"：staging 里 0755 的目录 `Excl` 改名到封存位置（另一个父目录）成功，之后对它的句柄 `set_file_meta({ mode: Some(0o555) })` 成功（目录的句柄，不核对 `st_nlink`）；0555 的目录改名到另一个父目录 → `ReadOnly`（`reparent_needs_write`），内核给的 `detail.errno` 是 `EACCES`（增补 8 点名的"0555 改父级 → EACCES"一条），目录纹丝不动；同一个父目录之内改名成功；`Swap` 里有 0555 的目录、两个目录在不同的父目录下 → `ReadOnly`（`reparent_needs_write`）；对它 `make_removable(&open_rooted(..)?)` 之后都成功；0555 的父目录里的文件改名出去 → `ReadOnly`（`no_permission`），不是 `reparent_needs_write` | 新增 |
| `remove_rooted_file_on_a_directory_returns_is_directory` | 增补 8 点名的一条：macOS 上 `unlinkat(目录, 0)` 是 `EPERM`（不是 `EISDIR`），types.md §6.3 会把它读成 `ReadOnly`，所以 `remove_rooted(.., File)` 必须用 `fstatat` 区分。空目录、非空目录、0555 的目录、带 `uchg` 的目录：全部 → `IsDirectory`（`detail.errno` 为 `EPERM`），目录与里面的东西都在；对照，带 `uchg` 的**文件**同样是 `EPERM`，却 → `ReadOnly`（`immutable`）；不存在 → `NoSuchPath` | 新增 |
| `remove_rooted_file_removes_one_entry_without_following` | 文件、指向文件的符号链接、指向目录的符号链接（目录和里面的东西都在）、悬空符号链接、FIFO、socket 都能删，每次只少一个条目，符号链接指向的地方不被碰；目标是目录（空或非空）→ `IsDirectory`（内核给的是 `EPERM`，另有专门的 `remove_rooted_file_on_a_directory_returns_is_directory`），目录还在；不存在 → `NoSuchPath`；带 `uchg` 的文件 → `ReadOnly`（`immutable`）；0555 的父目录里的文件 → `ReadOnly`（`no_permission`）；父级是符号链接 → `NotDirectory`，链接指向的目录里的同名文件没被删（`remove_rooted_refuses_symlinked_parents` 钉住）；`kind = EmptyDir` 对文件 → `NotDirectory`，文件还在 | 新增 |
| `remove_rooted_empty_dir_removes_only_empty_directories` | 空目录能删（自己是 0555 也行）；非空 → `NotEmpty`，里面的东西都在；目标是文件 → `NotDirectory`；目标是指向目录的符号链接 → `NotDirectory`（`reason = symlink`），链接和目录都还在；不存在 → `NoSuchPath`；带 `uchg` 的空目录 → `ReadOnly`（`immutable`）；挂载点（第二个镜像挂在根下的 `mnt`）→ `CrossVolume`（`reason = mount_point`，`detail.src_dev` 不等于 `dst_dev`；调内核之前 `fstatat` 就判出了），挂载的内容没动，`mnt` 还在（评审 N7，不再是 `Unknown`）；接缝在 `fstatat` 之后、`unlinkat` 之前把一个空目录变成挂载点（`hdiutil attach -mountpoint`）：晚到的 `EBUSY` 同样 → `CrossVolume`（`mount_point`），不是 `Unknown`，镜像里的内容没动；父级是符号链接 → `NotDirectory`。调用方只用它删自己建的空目录，或本次 restore 自己清空的目录（裁定 "Empty directories, amended"），没清空的目录从不删，`NotEmpty` 是写竞争（上面已测）；非空的树经 `remove_tree_rooted`，不自己写递归 | 新增 |
| `remove_tree_rooted_removes_a_protected_tree` | 评审 I1、增补 8 的主测试，也是 N3、N4 的正面一半。树里有：mode 0400 的文件；带 `uchg`、`uappnd`、`hidden` 的文件；指向根外目录、指向树内文件、悬空的符号链接，以及带 `uchg` 的符号链接（`chflags -h`）；FIFO；unix socket；同一个**不带标志**的 inode 的两个名字，外加第三个名字放在根外（没有标志拦着，`unlinkat` 照常只删名字；带标志的共用 inode 见 `remove_tree_rooted_leaves_a_shared_flagged_inode_alone`）；mode 000 的目录和 0555 的目录（各装一个文件）；mode 0100 的目录（属主有搜索位而没有读位，各装一个文件，带 `uchg` 与不带各一个：走 `O_EXEC` 句柄，不用按名字的 `fchmodat`）；带 `uchg`、`uappnd` 的目录（各装一个文件）；mode 0100 又带 `uchg` 的普通文件（`O_EXEC` 句柄上清标志）；`nest/a/b/c/d/leaf` 的深链，其中 `b` 是 0555。根外有 `outside/`，装着文件与子目录。`remove_tree_rooted` → `Ok(report)`：`removed` 等于用 `std` 事先数好的树内条目数（含目标自己），`flags_cleared == 7`（`uchg` 文件、`uappnd` 文件、`uchg` 符号链接、`uchg` 目录、`uappnd` 目录、0100 加 `uchg` 的目录、0100 加 `uchg` 的文件），`made_writable == 5`（000、0555、`b`、0100、0100 加 `uchg`；0100 与 0100 加 `uchg` 的目录各在 `O_EXEC` 路线上计一次，进入目录之后的第 4 步用 `fstat(fd)` 新读的标志与 mode，所以不会再计第二次，评审 n1），`stuck_total == 0`，`is_complete()`；树整个没了；`outside/` 逐字节原样（符号链接没被跟随），第三个名字还在、内容与 mode 不变 | 新增 |
| `remove_tree_rooted_takes_any_entry_kind` | 裁定 "remove_tree_rooted takes any entry kind"（store 的 gc 用这一个调用删 tmp 与 trash 里的每个条目）。目标分别是：mode 0400 的普通文件、带 `uchg` 的文件（独占 inode）、指向根外目录的符号链接、带 `uchg` 的符号链接（`chflags -h`）、悬空符号链接、FIFO、unix socket、空目录：每一个都 → `Ok(report)`，`is_complete()`，`removed == 1`（两个带 `uchg` 的 `flags_cleared == 1`），目标没了，符号链接指向的目录与里面的东西一个字节没动，同一个父目录里的兄弟条目都在。目标是带 `uchg` 而 `st_nlink == 2` 的文件 → `Ok(report)`，`stuck` 恰好是目标（`Immutable`，`detail == "hard_link"`），`removed == 0`，目标和它的另一个名字都在，标志没动 | 新增 |
| `remove_tree_rooted_reports_what_it_cannot_remove` | 树里有：一个 ACL 禁止删除（`chmod +a "user:<当前用户> deny delete"`）的文件；一个嵌套的挂载点（第二个镜像挂在 `tree/mnt`，里面有文件和子目录）；一个 mode 0000 又带 `uchg` 的目录（里装一个文件），和一个 mode 0200 又带 `uchg` 的普通文件（属主既没有读位也没有搜索位，取不到句柄的角落，评审 N3，§4.2 H）；一个属主 0700 的目录，加了 ACL（`chmod +a "user:<当前用户> deny list,search"`，里装一个文件）：属主有搜索位而两种打开都是 `EACCES`，chmod 帮不上忙（评审 M1）；其余是普通条目。`Ok(report)`，`is_complete()` 为假，`stuck_total == 5`，`stuck` 恰好是这五条，原因按 §5.4 的 stuck 原因表：ACL 文件 `NoPermission`（`errno` 为 `EACCES`，`detail` 为 `None`）；挂载点 `OtherDevice`（`errno` 为 0）；0000 加 `uchg` 的目录 `Immutable`（`errno` 为 `EPERM`，`detail == "UF_IMMUTABLE"`）；0200 加 `uchg` 的文件 `Immutable`（`errno` 为 `EACCES`，`detail == "UF_IMMUTABLE"`）；ACL 目录 `NoPermission`（`errno` 为 `EACCES`，`detail` 为 `None`）。`removed` 等于事先数好的可删条目数（stuck 条目及其祖先目录不在内）；其余条目都删光了，五个 stuck 条目（含 ACL 目录里的文件）、它们的祖先目录、挂载点里的文件、`tree` 本身还在（不删根）；挂载的卷里的文件逐字节不变；0000 的目录和 ACL 目录 apfs 一个字节也没改（0000 的目录：mode 还是 0000，标志还在，`made_writable` 不含它；ACL 目录：mode 还是 0700）。两个目录的 `ctime` 前后相等（两次读之间睡 1.1 s：任何一次 `fchmodat`，哪怕是无操作的，都会让 `ctime` 前进，§4.2 H，所以这一条证明没有去试按名字的 `fchmodat`）。去掉两个 ACL（`chmod -a`）、清掉两个 `uchg`、给 0000 的目录 `chmod 0700`、卸载镜像之后再调一次：`Ok(report)`，`is_complete()`，`removed` 为剩下的条目数。两次调用都是 `Ok`，没有 `Err` | 新增 |
| `remove_tree_rooted_leaves_a_shared_flagged_inode_alone` | 评审 N4、裁定 "Flags are cleared only on an unshared inode"（`fchflags` 改的是 inode，不是名字）。树里：普通文件 `h` 带 `uchg`，它的第二个名字 `outside/h2` 在根外（`link()` 建的）；`g1`、`g2` 是同一个带 `uappnd` 的 inode 的两个名字，都在树里；另有普通条目若干。前提钉在断言里：`link()` 之后 `outside/h2` 读到的标志已经含 `UF_IMMUTABLE`（标志属于 inode，§4.2 H）。`Ok(report)`，`is_complete()` 为假，`stuck` 恰好是 `h`、`g1`、`g2`，都是 `Immutable`、`errno` 为 `EPERM`、`detail == "hard_link"`（共用的 inode 一律不清，哪怕另一个名字也在树里）；`flags_cleared == 0`；`outside/h2`：内容、mode、标志（仍含 `UF_IMMUTABLE`）、`ctime` 都与调用前相同（apfs 没有对这个 inode 调过 `fchflags`、`fchmod`，`ctime` 没动就是证据）；`removed` 等于其余可删条目数。测试结束前对 `h`、`g1` 先 `chflags nouchg`、`nouappnd` 再清理 | 新增 |
| `remove_tree_rooted_refuses_before_touching_anything` | 表驱动，一种构造一个拒绝，**返回 `Err`（没有报告）**，每种之后整棵树与父目录逐字节、逐属性（mode、标志、mtime）不变：目标不存在 → `NoSuchPath`；某一级父目录是符号链接（指向根外、指向根内各一轮）→ `NotDirectory`（`symlink`），根外的树没被碰；某一级父目录是文件 → `NotDirectory`（`not_a_directory`）；目标是目录而且本身是挂载点（第二个镜像挂在根下的 `mnt`，里面有文件）→ `CrossVolume`（`reason = mount_point`，`detail.src_dev` 不等于 `dst_dev`），卷里的文件原样；父目录 0555 → `ReadOnly`（`no_permission`）；父目录带 `uchg` 或 `uappnd` → `ReadOnly`（`immutable`）——`faccessat` 对 `uappnd` 的目录说"可写"，这一行钉住标志检查（§4.2 H）。目标是普通文件、符号链接这类**不是**拒绝：它们是合法的目标，见 `remove_tree_rooted_takes_any_entry_kind` | 新增 |
| `remove_tree_rooted_never_follows_a_swapped_directory` | 接缝（打开子目录之前触发）把子目录 `d` 换成指向根外目录的符号链接：根外目录的内容一个字节也没少，`d` 处的符号链接本身留着，报告里 `stuck` 含 `d`（`Changed`），其余兄弟照常删光，返回 `Ok(report)`（`is_complete()` 为假）；另一轮接缝把已经解析过的父目录的路径换成指向根外的符号链接：删掉的是被钉住的原目录里的东西，根外目录里的同名条目还在 | 新增 |
| `remove_tree_rooted_depth_limit` | 100 层深的链（每层一个字符的目录名）删得干净；130 层深的链：返回 `Ok(report)`（`is_complete()` 为假），`stuck` 含深度超限的那一层（`TooDeep`），浅的兄弟分支删光，`MAX_WALK_DEPTH` 层以内的目录与文件也删光，超限的那一层及其上的祖先留着 | 新增 |
| `remove_tree_fits_a_low_descriptor_limit` | 父测试启动子进程（`current_exe`，`--ignored --exact remove_tree_low_descriptor_limit_child --nocapture`）；子进程先把 `RLIMIT_NOFILE` 软限降到 256，再用 `remove_tree_rooted` 删一棵 512 个目录宽、外加一条 100 层深链的树，不得 `EMFILE`；删除期间每隔若干个条目数一次 `/dev/fd`，最大值不超过深度加一个小常数，与目录总数无关；所有断言都过了之后，子进程最后打印固定的一行 `low-fd-child: 512 wide, 100 deep, removed`；父测试断言子进程退出码为 0，**并且**标准输出里有这一行：`--ignored --exact <名字>` 什么都没匹配到时（测试被挪进模块、`#[ignore]` 被去掉、改了名字），libtest 跑 0 个测试、退出码照样是 0，只看退出码会静默通过（评审 M2）；失败时打印子进程的输出 | 新增 |
| `remove_tree_low_descriptor_limit_child` | 上一行的子进程入口（`#[ignore]`，只由父测试驱动）：降限不污染同一进程里的其他测试；断言全过之后打印上一行的标记 | 新增 |
| `remove_report_counts_and_completeness` | 单元测试加一个 doc-test：`RemoveReport::default()` 全零，`is_complete()` 为真；`stuck_total > 0` 时为假（哪怕 `stuck` 向量被截断，总数仍说实话）；用内部的记录函数记 `MAX_REPORTED_STUCK + 5` 条：向量被截在上限、前 `MAX_REPORTED_STUCK` 条按记录的顺序、`stuck_total` 是真实总数。`#[must_use]`：`compile_fail` 的 doc-test（`#![deny(unused_must_use)]` 下 `remove_tree_rooted(..);` 丢掉返回值）不能通过编译。没有 `to_detail`、`from_error`：报告不进 `StrataError`（遍历开始之后没有 `Err`） | 新增 |
| `stuck_reason_classification_and_names` | 单元测试，纯函数（内部，名字构建者定；输入是 errno、`st_flags`、`st_nlink`），表驱动，逐行对 §5.4 的 stuck 原因表：`EACCES`、`EPERM` 且无标志 → `NoPermission`；`EPERM` 且 `UF_IMMUTABLE` 或 `UF_APPEND`、`st_nlink == 1` → `Immutable`（`detail` 是置位的标志名，两个同时置位时都列出）；同上而 `st_nlink > 1` → `Immutable`（`detail == "hard_link"`）；`SF_IMMUTABLE`、`SF_APPEND` 置位（不论 `st_nlink`）→ `Immutable`，`detail` 列出 `SF_*`，同时置位的 `UF_*` 也列出；`EBUSY` → `Busy`；`ENOTEMPTY`、`ENOTDIR`、`ELOOP` → `Changed`；`EIO`、`ETIMEDOUT`、`ENXIO`、`ENODEV`、`EDEVERR` → `UnknownOutcome`；`ENOSPC`、`EDQUOT`、`EINVAL`、其余 → `Io`；`StuckReason::as_str` 的九个名字（`immutable`、`no_permission`、`other_device`、`busy`、`changed`、`unreadable`、`too_deep`、`unknown_outcome`、`io`）互不相同。`UnknownOutcome` 与 `Io` 的区分（unlink 有没有生效不确定）在这里钉住，真实文件系统上造不出 `EIO`。按名字 chmod 的门（内部纯函数，输入是目录的 `st_mode`、`st_flags`，评审 M1）也在这里钉住，因为 `SF_*` 要 root 才造得出：只有 `st_mode & S_IXUSR == 0` 而且 `blocking_flags(st_flags)` 为空才返回真——(0000, 0)、(0200, 0)、(0600, 0)、(0000, `UF_HIDDEN`) → 真；(0100, 0)、(0700, 0)（有搜索位）→ 假；(0000, `UF_IMMUTABLE`)、(0000, `UF_APPEND`)、(0000, `SF_IMMUTABLE`)、(0000, `SF_APPEND`) → 假 | 新增 |
| `blocking_flags_lists_the_flags_that_block_removal` | 单元测试，纯函数 `blocking_flags(st_flags)`：0 → 空；只有 `UF_IMMUTABLE` → `UF_IMMUTABLE`；`UF_APPEND`；`SF_IMMUTABLE`；`SF_APPEND`；四个都置位 → 固定顺序 `UF_IMMUTABLE\|UF_APPEND\|SF_IMMUTABLE\|SF_APPEND`；`UF_HIDDEN`、`UF_NODUMP`、`UF_COMPRESSED`、`UF_OPAQUE` 单独或与上面混合：不列出（它们不挡删除）。`make_removable`、`set_file_meta`、`remove_tree_rooted` 用同一个函数，所以三处的 `detail` 一致 | 新增 |
| `make_removable_clears_flags_before_mode` | 裁定 "make_removable"，评审 N3、N4。（a）0555 加 `uchg` 的目录：前提钉在断言里——对它直接 `fchmod(0755)`（用 `std`）得 `EPERM`；`make_removable(&open_rooted(..)?)` → `Ok`，之后标志为 0、mode 为 0755，`rename_rooted(Excl)` 到另一个父目录成功；（b）0555 的目录（没有标志）→ mode 变 0755、标志不变；带 `uappnd` 的 0755 目录 → 清掉 `uappnd`、mode 不变；`hidden` 等别的标志位原样保留；0400 的普通文件 → 0600；（c）无操作：已经是 0755 的目录、已经是 0600 的文件，`make_removable` → `Ok`，`ctime` 前后相等（没有调用 `fchmod`、`fchflags`；无操作的调用也会前进 `ctime`，§4.2 H）；（d）非目录而 `st_nlink == 2`（另一个名字在树外）、带 `uchg` 或只是 0400 → `ReadOnly`（`reason = hard_link`），两个名字的标志、mode、`ctime` 都没变；（e）取得句柄之后另一个进程才 `link()` 出第二个名字：`make_removable` 在句柄上重新 `fstat`，仍然 `ReadOnly`（`hard_link`）；（f）`SF_*` 的拒绝需要 root 才造得出，由 `make_removable_and_set_file_meta_refuse_by_rule` 的纯函数表覆盖；（g）第一项改动之后的失败（`before_meta` 接缝，§5.4）：0555 加 `uchg` 的目录，`flags` 那一步生效之后、`mode` 那一步之前，`before_meta(1, Mode)` 里把 `uchg` 重新设上（测试自己调 `libc::fchflags`，只用来造场景）→ `fchmod` 得 `EPERM` → `Unknown`（`detail.errno` 为 `EPERM`）；目录的标志是测试重设的 `uchg`（apfs 没有重试，也没有回滚），mode 仍是 0555 | 新增 |
| `set_file_meta_changes_metadata_through_a_handle` | 裁定 "Metadata changes through a handle"。（a）克隆自带 `uchg`、`hidden` 的 0444 基线文件，mtime 为 2020 年：句柄（`open_rooted_file`）上 `set_file_meta({ clear_flags: true, mode: Some(0o644), mtime: Some(t) })` → `Ok`，标志只剩 `UF_COMPRESSED`（没有就是 0），mode 0644，`mtime == t`（纳秒精度），`atime` 不变，内容不变；次序：前提钉在断言里——对带 `uchg` 的文件先 `fchmod`（用 `std`）得 `EPERM`，所以必须先清标志；（b）压缩文件（这台机器上 `ditto --hfsCompression` 造不出压缩文件，2026-10-22 实测，所以不用它：用 `ditto` 复制一个系统里本来就压缩的文件，复制会保留压缩——`find /Library -type f -flags +compressed -size +8k` 找一个不带 `restricted` 的，例如 `/Library/Documentation/Acknowledgements.rtf`；找不到就让测试失败并说明）加 `uchg`、`hidden` 之后 `set_file_meta({ clear_flags: true })`：标志只剩 `UF_COMPRESSED`（实测 0x8022 变回 0x20），内容读回与源逐字节相同；（c）无操作：`FileMeta::default()`、已经是目标值的 mode、没有用户标志时的 `clear_flags`、`mtime` 为 `None`：不调系统调用，`ctime` 前后相等；（d）拒绝，什么都不改（标志、mode、mtime、`ctime` 全部不变）：FIFO 句柄（`open_rooted` 非阻塞打开）→ `NotFile`（`fifo`）；socket 句柄（`UnixStream::pair()` 的一端转成 `File`）→ `NotFile`（`socket`）；设备句柄（`File::open("/dev/null")`）→ `NotFile`（`device`）；普通文件而 `st_nlink == 2` → `ReadOnly`（`hard_link`），另一个名字的标志、mode 也没变；（e）store 的 writer 复现过的场景：`get` 的克隆 dst 被换成指向根外 0600 文件的硬链接，`open_rooted_file` 照常打开它（`O_NOFOLLOW` 管不到硬链接），`set_file_meta({ mode: Some(0o644) })` → `ReadOnly`（`hard_link`），根外的文件仍是 0600；（f）打开句柄之后才建第二个名字：句柄上重新 `fstat`，同样 `hard_link`；（g）目录的句柄（裁定 "set_file_meta accepts a directory handle"）：0755 的目录里放一个子目录（`st_nlink == 3`），目录带 `uchg`、`hidden`，mtime 为 2020 年。前提钉在断言里——对它直接 `fchmod(0555)`、`futimens`（用 `std`）都得 `EPERM`；`open_rooted` 取句柄，`set_file_meta({ clear_flags: true, mode: Some(0o555), mtime: Some(t) })` → `Ok`：标志为 0、mode 0555、`mtime == t`（纳秒精度）、`atime` 不变，子目录与里面的东西不变；`st_nlink > 1` 没有拒绝（目录没有硬链接）；`mode: Some(0o4755)` 的 setuid 位被屏蔽；已经是 0555 的目录再调 `{ mode: Some(0o555) }`：不调系统调用，`ctime` 前后相等。deps 封存的写法单独钉一次：staging 里 0755 的目录 `rename_rooted(Excl)` 到封存位置，再对它的句柄 `set_file_meta({ mode: Some(0o555) })`：根目录 0555，里面的条目（含子目录）的 mode 一个都没变；（h）第一项改动之后的失败（`before_meta` 接缝，§5.4）：带 `uchg` 的 0644 文件，`{ clear_flags: true, mode: Some(0o600), mtime: Some(t) }`，`flags` 那一步生效之后、`mode` 那一步之前，`before_meta(1, Mode)` 里把 `uchg` 重新设上（同样用 `libc::fchflags`）→ `fchmod` 得 `EPERM` → `Unknown`（`detail.errno` 为 `EPERM`），失败的那一步之后 apfs 不再往下走：标志是测试重设的 `uchg`，mode 仍是 0644，mtime 没动；（i）超出 APFS 范围的 mtime 不是错误：3000 年 → `Ok`，读回的 mtime 是上界 2262-04-11T23:47:16.854775807Z（内核夹住，§4.2 H；钉住平台行为，免得以后有人以为超范围会报错）；-9223372037 秒 → 读回下界 1677-09-21T00:12:43.145224192Z；同一个越界请求再调一次（两次之间睡 1.1 s）→ `Ok`，`ctime` 前后相等，mtime 仍是边界值（`set_file_meta` 先夹再比较，只改一次，评审 n6） | 新增 |
| `make_removable_and_set_file_meta_refuse_by_rule` | 单元测试，纯函数（内部，名字构建者定；输入是合成的 `st_mode`、`st_flags`、`st_nlink` 与种类），表驱动，因为 `SF_*` 要 root 才造得出，真实文件系统上测不到：`make_removable`——目录加 `SF_IMMUTABLE` 或 `SF_APPEND` → `ReadOnly`（`immutable`，`detail.flags` 列出）；`UF_IMMUTABLE` 与 `SF_APPEND` 同时置位仍拒绝，`detail.flags` 两个都列出；非目录且 `st_nlink > 1` → `ReadOnly`（`hard_link`）；目录且 `st_nlink > 1`（子目录的 `..` 让它大于 1）→ 不拒绝。`set_file_meta`——FIFO、socket、设备、符号链接 → `NotFile`；目录（含 `st_nlink > 1`）通过种类与 `st_nlink` 两关，只剩 `SF_*` 一关；普通文件而 `st_nlink > 1` → `hard_link`；`SF_*` → `immutable`（目录同样）；检查的先后是种类、`st_nlink`（只对非目录）、`SF_*`。通过的输入返回"要做的步骤"：`make_removable` 对（0555、无标志）= [补 u+w]，对（0755、`UF_IMMUTABLE`）= [清标志]，对（0555、`UF_IMMUTABLE`）= [清标志, 补 u+w]（次序），对（0755、无标志）= []；`set_file_meta` 对各项已是目标值 = []，各项都不同 = [清标志, 设 mode, 设 mtime]（次序），目录（0755、无标志）对 `{ mode: Some(0o555) }` = [设 mode] | 新增 |
| `clone_from_rooted_clones_files_and_directories` | 工作区根下的普通文件（mode 0600、mtime 设成 2020 年、带扩展属性）以 `CloneKind::File` 克隆到 strata 自己的临时目录里：内容、mode、mtime、扩展属性相同，ctime 是现在；目录树（含相对和悬空的符号链接、空目录、FIFO、嵌套 20 层）以 `CloneKind::Dir` 克隆后与 `clone_tree` 的结果逐项相同，目录里的符号链接仍是符号链接（没被跟随）；源树不变；dst 与源是两个独立的 inode，改克隆不影响源。`File` 时符号链接作源 → `NotFile`（`symlink`）；FIFO → `NotFile`（`fifo`）；unix socket → `NotFile`（`socket`）；`clone_from_rooted(Path::new("/dev"), "null", dst, CloneKind::File)` → `NotFile`（`device`）；不存在 → `NoSuchPath`（种类不符的完整一张表见 `clone_from_rooted_checks_the_source_kind`） | 新增 |
| `clone_from_rooted_checks_the_source_kind` | 裁定 "clone_from_rooted states its source kind"，store 第 2 轮评审的场景。表驱动，一种构造一个结果，每种之后源原样，失败的 dst 不存在：`File` 对普通文件 → 成功；`File` 对目录（树里有文件）→ `IsDirectory`，**没有克隆任何东西**（`store/tmp` 里没有整棵树，这是原来的 bug）；`File` 对 FIFO → `NotFile`（`fifo`），不是误导的 `EINVAL`；`File` 对 socket → `NotFile`（`socket`）；`File` 对符号链接 → `NotFile`（`symlink`）；`Dir` 对目录 → 成功；`Dir` 对普通文件 → `NotDirectory`（`not_a_directory`）；`Dir` 对 FIFO → `NotDirectory`（`not_a_directory`）；`Dir` 对符号链接（指向目录的、悬空的）→ `NotDirectory`（`symlink`）。交换版本（接缝在打开终点那一次 `openat` 之前触发，模拟 `attrs_of` 与克隆之间路径被换）：调用方说 `File`，路径换成装着文件的目录 → `IsDirectory`，dst 不存在；换成 FIFO（子进程加死线）→ `NotFile`（`fifo`），不阻塞；调用方说 `Dir`，路径换成普通文件 → `NotDirectory`。优先级：`File` 而源是另一个卷上的目录 → `IsDirectory`，不是 `CrossVolume`（先核对种类，后查同卷，§5.4 第 4、5 步）；dst 已存在而且种类也不符 → `Exists`（先查 dst，第 1 步） | 新增 |
| `symlink_recipe_recreates_a_workspace_symlink` | 评审 N6、裁定 "A symlink is recreated, never cloned"。工作区根下的符号链接：指向树内文件、指向根外目录、悬空、相对目标 `../x`、目标含非 UTF-8 字节：对每个，`attrs_of(src_root, p)` 得 `FileKind::Symlink(target)`，`symlink_rooted(dst_root, q, &target)` 在另一个根下重建；`readlink` 读回的字节与原链接逐字节相同，链接指向的地方从未被读到或被碰（根外目录的内容、属性前后不变）；同一个链接交给 `clone_from_rooted` → `NotFile`（`File`）与 `NotDirectory`（`Dir`），都是 `reason = symlink`，dst 不存在；链接所在的父目录被接缝换成指向根外的符号链接：`attrs_of` → `NotDirectory`（`symlink`），什么都没读到。对照，证明为什么不能按路径克隆：`ws/d` 被换成指向根外目录的符号链接之后，按路径的 `clone_file_rooted(ws/d/link, ..)` 克隆出的是根外目录里的那个同名链接（中间一级被跟随，§4.2 A），而上面的配方得到 `NotDirectory` | 新增 |
| `clone_from_rooted_refuses_a_symlinked_parent` | 根里有指向根外目录（放着 `.env` 之类的文件）的符号链接 `link` 和指向根内另一个目录的符号链接 `inner`：`clone_from_rooted(root, link/x, dst)`、`inner/x`、`link/a/b` → `NotDirectory`（`reason = symlink`），dst 不存在，根外的内容一个字节也没进临时目录；根本身是符号链接 → `NotDirectory`（`symlink`）。这是评审 I2 的场景：overlay 的捕获不能把 `.env` 带进 blob | 新增 |
| `clone_from_rooted_clones_the_opened_object_after_a_swap` | 评审 I2 点名的交换测试。根里有 `ws/a/f`（内容 `INSIDE`）和 `ws/d/g`（内容 `INSIDE-G`），根外有 `outside/secret`（内容 `OUTSIDE`）与目录 `outside/dir`（含 `g`，内容 `OUTSIDE-G`）。接缝在 `fclonefileat` 之前（源已经打开之后）把 `ws/a/f` 换成指向 `outside/secret` 的符号链接：克隆出来的 dst 内容是 `INSIDE`，dst 里搜不到 `OUTSIDE`；另一轮把整个 `ws/d` 换成指向 `outside/dir` 的符号链接：dst 目录里是原来的 `g`（`INSIDE-G`），没有 `OUTSIDE-G`。对照，证明这个场景确实有效：同样把 `ws/d` 换成指向 `outside/dir` 的符号链接之后，按路径的 `clone_file(ws/d/g, ..)` 克隆出 `OUTSIDE-G`（`CLONE_NOFOLLOW` 只管最后一级，中间的符号链接照样被跟随，§4.2 A）。再一轮在解析父级的中途把已解析的祖先换成符号链接：仍读到被钉住的原目录里的内容，之后的新调用 → `NotDirectory` | 新增 |
| `clone_from_rooted_error_codes` | 表驱动，一种构造一个错误：dst 已存在（文件、目录、悬空符号链接）→ `Exists`，原有的东西不被碰；dst 的父目录不存在 → `NoSuchPath`；dst 的父级是文件 → `NotDirectory`；dst 末尾带 `/` → `InvalidInput`（`trailing_slash`）；dst 的名字 256 个码元 → `InvalidInput`（`name_too_long`）；src_root 与 dst 在两个 APFS 镜像上 → `CrossVolume`（`src_dev` 不等于 `dst_dev`），B 上没有 dst，没有字节被复制；HFS+ 镜像上 → `Unsupported`；源带 `uchg` 的文件克隆成功，dst 同样不可变（标志带过去，测试结束前清掉）；失败之后 dst 都不存在（"已存在"那一类里原有的除外） | 新增 |
| `full_fsync_accepts_file_and_directory_handles` | 对 `open_rooted` 取得的普通文件句柄、目录句柄调用 `full_fsync` → `Ok`；`create_file_rooted` 返回的可写句柄写入之后同样 `Ok`；根目录用 `open_root(root)` 取句柄同样 `Ok`；HFS+、MS-DOS 镜像上同样 `Ok`（实测）；管道的一端 → `Unknown`，`detail.errno` 为 `EBADF`（不 panic）。扛断电的改名配方：`rename_rooted` 之后对两个父目录的句柄各调一次，每次都 `Ok` | 新增 |
| `gen_count_changes_on_write_not_on_touch` | 以一个文件的 `attrs_of` 为基准。（a）`pwrite` 一个字节（哪怕与原来相同）→ gen_count 变；写入后用 `utimensat` 把 atime、mtime 还原成旧值，gen_count 仍是变了的值，而 `mtime` 与基准相等；`ftruncate` 变大、`O_TRUNC` 也变。（b）`utimensat` 改时间戳、还原时间戳、`chmod`（含加可执行位）、`setxattr`、`chflags`、`rename`、建硬链接、`ftruncate` 到相同大小、`open(O_WRONLY)` 后不写就关闭、`pread`：gen_count 都不变；`chmod` 之后 `mode` 变了，这就是 `Attrs` 带 `mode` 的原因。（c）`unchanged_since` 在（a）的每一步之后为 `false`，在（b）里除 `chmod` 以外的每一步之后为 `true` | 新增 |
| `gen_count_is_unknown_while_a_writable_mapping_exists` | 与 tgrep 原测试同一个情形，前提钉在断言里：描述符是 `open(O_RDWR)`，`mmap(PROT_READ \| PROT_WRITE, MAP_SHARED)`，创建映射的描述符随后关闭、映射仍在；映射之前先断言有一个非零的基准。另一个独立的读取（`attrs_of`）得到的 gen_count 在映射后、经映射写入后、`msync` 之后三个时点都是 `GEN_COUNT_UNAVAILABLE`（`gen_count_known()` 为 `false`），`unchanged_since` 为 `false`；写入（事后还原时间戳）后 gen_count 改变；解除映射后 gen_count 非零且不同于写入后的值，读到经映射写入的字节 | tgrep-core/tests/managed_storage_macos.rs::native_content_generation_invalidates_writes_and_rejects_retained_writable_mappings |
| `gen_count_unknown_with_creator_descriptor_still_open` | 创建可写映射的描述符仍开着：映射存续期间 `attrs_of` 的 gen_count 仍是 0（矩阵的备注：apfs 不得从 tgrep 那一种情形推广，要推广须自己的用例） | 新增 |
| `gen_count_unknown_for_a_mapping_made_by_another_process` | 子进程建立可写共享映射并保持（管道同步），父进程读到的 gen_count 是 0；子进程退出、映射消失之后读到非零 | 新增 |
| `gen_count_unknown_for_a_read_only_mapping_of_a_writable_descriptor` | 评审 M1 的反例：描述符是 `open(O_RDWR)`，映射是 `mmap(PROT_READ, MAP_SHARED)`（保护位只读，从不写）：映射存续期间 `attrs_of` 的 gen_count 是 0，不论创建映射的描述符是否已关闭，`unchanged_since` 为 `false`；`munmap` 之后非零且比映射前大（+1，没写过）。触发条件是描述符的打开方式，不是 `PROT_WRITE`（§4.2 C） | 新增 |
| `read_only_and_private_mappings_keep_gen_count_known` | `O_RDONLY` 描述符的 `PROT_READ` `MAP_SHARED` 映射存续期间 gen_count 非零且与映射前相同；`O_RDONLY` 加 `PROT_WRITE` 加 `MAP_SHARED` 的 `mmap` 自己就 `EACCES`（断言它失败，说明这一格没有"可写的只读描述符映射"）；`O_RDWR` 描述符上 `PROT_WRITE` 的 `MAP_PRIVATE` 映射（私有映射里的写不影响文件）存续期间 gen_count 非零且不变 | 新增 |
| `gen_count_alone_is_not_a_version` | 克隆一个文件，克隆的 gen_count 从 1 起，可能与源相同；用 `rename` 把另一个同样小 gen_count 的文件盖到原路径：gen_count 相同、inode 不同；这两种情况下 `unchanged_since` 都是 `false` | 新增 |
| `directory_gen_count_tracks_entries_not_content` | 目录：创建、改名、删除子项，建、删子目录，各使它的 gen_count 增加；往子文件里写内容不改变父目录的 gen_count；所以对目录 `unchanged_since` 的含义只是名字集合没变 | 新增 |
| `attrs_last_changed_is_the_later_of_mtime_and_ctime` | 先把源文件的 mtime 设成 2020 年：`clone_file` 之后克隆的 `mtime` 是 2020 年而 `ctime` 是现在，`last_changed()` 等于 `ctime`（现在）而不是 2020 年；`rename_rooted` 之后 `mtime` 不变、`ctime` 与 `last_changed()` 前进，对象的 inode 与 `gen_count` 不变；`chmod`、`chflags` 只让 `ctime` 前进；`utimensat` 把 mtime 设成 2017 年：`mtime` 回到 2017 年而 `last_changed()` 仍是现在；写入让两者都前进；目录同样：`clone_tree` 一棵源 mtime 都设成 2020 年的树，克隆出的文件和空目录 `mtime` 是 2020 年，有子项的目录 `mtime` 是现在，所有对象的 `last_changed()` 都是现在；目录改名到另一个父目录只让目录自己的 `ctime` 前进、`mtime` 不动；`Swap` 让两个对象的 `ctime` 都前进、`mtime` 不动。所以 gc 的"年龄不足 1 小时"保护刚克隆、刚改名的东西 | 新增 |
| `attrs_versions_differ_for_same_second_writes` | 同一秒内对同一文件两次不同内容的写入（size 相同、秒级 mtime 相同）：两次 `attrs_of` 的 `Attrs` 不相等，`unchanged_since` 为 `false`；`mtime` 带纳秒（至少一次的纳秒部分非 0） | tgrep-core/src/walker.rs::metadata_walk_reports_precise_same_second_versions |
| `same_size_write_with_restored_mtime_is_a_new_version` | 同尺寸改写后用 `utimensat` 还原 mtime：`mtime` 与改写前相等而 `gen_count` 不同，`unchanged_since` 为 `false`；纯改时间戳（`touch -r` 一类）不算改动，`unchanged_since` 为 `true` | tgrep-core/src/walker.rs::metadata_walk_detects_same_size_write_with_restored_mtime |
| `attrs_unchanged_since_rules` | 纯函数，表驱动：任一边 gen_count 为 `GEN_COUNT_UNAVAILABLE`（0，`gen_count_known()` 为 `false`）→ `false`，两边都是也是 `false`；inode、size、mode、kind 任一不同 → `false`；符号链接的目标不同 → `false`；mtime、ctime 不同而其余全同 → `true`；全同且 gen_count 非零 → `true` | 新增 |
| `attrs_of_reports_every_field` | 普通文件、目录、符号链接（目标取原字节，含非 UTF-8）、FIFO、socket：`kind`、`size`（符号链接是目标长度，目录与 FIFO 是 0）、`mode`（文件上的 setuid 位 `4755` 保留）、`inode` 与 `std::fs::metadata` 一致，`mtime`、`ctime` 与 `std` 的纳秒一致，`gen_count` 非零（与批量读给出的 `Attrs` 的逐项比较在 `walk_serves_full_and_incremental_scans`） | 新增 |
| `attrs_of_does_not_follow_links` | 最后一级是符号链接：返回链接本身（`Symlink`），不是目标；父级是符号链接 → `NotDirectory`（`symlink`）；不存在 → `NoSuchPath`；根下单分量的路径可用 | 新增 |
| `walk_lists_all_entries_relative_to_root` | 无排除时 `walk` 列出树里全部条目（文件、目录、符号链接），路径是相对根的 `RepoPath`，没有根前缀，不含根自己；扩展名为二进制的文件（`.png`、`.zip`）同样列出，apfs 不按扩展名过滤 | tgrep-core/src/walker.rs::walk_dir_no_excludes_returns_all_files |
| `walk_excludes_configured_dirs` | 树里有 `vendor/` 与 `src/`：filter 对 `vendor` 返回 `false` 后，`vendor` 本身及其下所有文件都不出现，`src` 下的文件不受影响；filter 收到的是相对根的路径，目录也会被问到 | tgrep-core/src/walker.rs::walk_dir_exclude_single_dir |
| `walk_excludes_several_dirs` | 同时排除 `vendor`、`node_modules`、`target` 三个目录名（filter 按名字匹配任意层级的同名目录）：三者都消失，其余保留 | tgrep-core/src/walker.rs::walk_dir_exclude_multiple_dirs |
| `walk_exclusion_of_a_missing_dir_is_a_noop` | 排除一个树里不存在的目录名，结果与不排除时逐项相同 | tgrep-core/src/walker.rs::walk_dir_exclude_nonexistent_dir_is_noop |
| `walk_exclusion_prunes_the_whole_subtree` | 被排除目录下嵌套的子目录与文件一并不出现；filter 对被剪掉的子树里的任何路径一次都没被调用（计数器断言：剪枝不读目录） | tgrep-core/src/walker.rs::walk_dir_exclude_skips_nested_files |
| `walk_serves_full_and_incremental_scans` | 同一个 `walk_with` 既做全树遍历，也做 `start = Some(dir)`、`max_depth = Some(1)` 的单目录列表（只产出 `dir` 的直接子项，子目录作为 `Dir` 产出但不读它们，路径仍相对根）；两种用法都服从同一个按目录名排除的 filter；git-strata 只有一个 `walk` 实现；对每个条目，`attrs_of` 与 `walk_with` 给出的 `Attrs` 逐项相等（含符号链接的目标、目录、FIFO） | tgrep-core/src/walker.rs::walk_file_metadata_excludes_dirs |
| `walk_is_preorder_with_siblings_sorted` | 评审 M11：目录里有 `foo/`（装着 `x`）、`foo-bar`、`foo.txt`：`walk` 的产出顺序是 `foo`、`foo/x`、`foo-bar`、`foo.txt`（前序，同一目录内按字节序）；它**不等于**全路径的字节序（`foo`、`foo-bar`、`foo.txt`、`foo/x`），断言两者不同，说明调用方不能把产出当全局有序；收进 `BTreeMap` 之后与 `std` 递归读出的集合相同 | 新增 |
| `walk_options_default_uses_the_default_buffer` | 评审 M7：`WalkOptions::default()` 的 `start` 与 `max_depth` 是 `None`，`buffer_bytes == DEFAULT_BULK_BUFFER_BYTES`（不是 derive 会给的 0）；`walk(root, f)` 与 `walk_with(root, &WalkOptions::default(), ..)` 的产出逐项相同 | 新增 |
| `walk_is_send_and_accepts_send_sync_filters` | 评审 M8：编译期断言 `Walk<'static>: Send`；把一个 `Walk` 整个移到另一个线程里迭代，结果与本线程相同；filter 是捕获 `&AtomicUsize` 的闭包（`Fn + Send + Sync`）可以传给 `walk` 与 `walk_with`，计数器在遍历后等于被问到的条目数 | 新增 |
| `list_dir_lists_one_level_in_byte_order` | `list_dir(root, Some(dir))` 只产出 `dir` 的直接子项（子目录作为 `Dir` 产出但不读它们），按 RepoPath 的字节序，路径相对根；`None` 列根自己；用 NFD 建的名字以 NFC 的 RepoPath 产出，且能被 `open_rooted` 打开；大小写不敏感的卷上，磁盘上是 `Foo.TXT` 时列出的名字就是 `Foo.TXT`（大小写照磁盘，与查询用的拼写比较是调用方的事）；`dir` 不存在 → 第一项 `Err(NoSuchPath)`；`dir` 是符号链接 → 第一项 `Err(NotDirectory)`（`symlink`）；返回值不借用调用方的局部变量（`Walk<'static>`，编译期保证） | 新增 |
| `walk_wide_directory_is_complete_and_stable_across_buffer_sizes` | 目录 `wide` 里 1300 个文件按降序创建，另有符号链接（相对、悬空）、嵌套目录、两个空目录、两个硬链接名字：每一项恰好出现一次，顺序是 `RepoPath` 的字节序；`buffer_bytes = 1024`（强制很多次续读）与缺省缓冲得到逐项相同的结果；符号链接带目标原字节、不被跟随；空目录各是一条 `Dir`；两个硬链接名字各一条，`inode` 相同；`after_bulk_window` 接缝被调用的次数大于 1 | layerfs-project/tests/backed_acquisition.rs::wide_nested_aliased_root_equals_the_whole_namespace_constructor |
| `walk_lists_the_longest_name_with_the_minimum_buffer` | 评审 M12：目录里有一个名字是 255 个 U+3042（765 字节的 UTF-8）的文件，另有普通文件和符号链接；`buffer_bytes = MIN_BULK_BUFFER_BYTES`（1024）遍历完整列出，没有 `ERANGE` 失败，名字逐字节正确 | 新增 |
| `walk_very_wide_directory_through_many_windows` | 一个目录里 17000 个悬空符号链接（目标约 960 字节，以打乱的顺序创建），用最小缓冲区遍历：恰好 17000 条，名字严格按 `s00000` 到 `s16999` 升序，每条都是 `Symlink` 且目标与创建时逐字节相同；窗口数远大于 1 | layerfs-project/tests/backed_acquisition.rs::very_wide_directory_is_ordered_through_many_windows |
| `walk_after_mass_deletion_lists_only_survivors` | 目录先建 10000 个文件，删掉其中 9870 个（留下大片连续的名字空洞），再用小缓冲区 `walk`：恰好列出剩下的 130 个，每个一次，没有被删的名字，也不因为某个窗口没有任何可用条目而提前结束 | layerfs-workspace/tests/kernel_directory.rs::empty_whiteout_pages_continue_and_only_accepted_names_become_resume_cookies |
| `walk_returns_untouched_names_once_while_the_directory_changes` | 6000 项的目录，缓冲区小到每个窗口两项左右，用接缝 `after_bulk_window` 在窗口之间删除、改名、新建一些名字：从未被动过的名字每个恰好出现一次，零缺零重复；被删、被改名与新建的名字可有可无，但不出现重复 | layerfs-workspace/tests/namespace_view.rs::enumeration_returns_every_persistent_name_once_across_mutation_and_resume |
| `walk_never_follows_symlinks` | 树里有指向根外目录、根内目录、文件的符号链接和悬空的符号链接：每个只产出一条 `Symlink`（带目标），目标目录里的东西不出现；成环的链接（`a -> .`）不会让遍历无限下去 | 新增 |
| `walk_does_not_descend_into_mount_points` | 树里 `mnt` 是第二个镜像的挂载点：产出一条 `Dir` 条目 `mnt`，其下没有任何条目，即使镜像里有文件 | 新增 |
| `walk_reports_an_unreadable_directory_and_continues` | 一个 mode 000 的子目录：产出 `Err(Unknown)`（`reason = unreadable_entry`，`detail.errno` 为 `EACCES`），目录条目本身仍然产出，其余兄弟与别的子树照常遍历完 | 新增 |
| `walk_skips_entries_that_vanish_mid_walk` | 接缝在 walk 打开子目录 `d` 之前删掉它：不报错，`d` 的子树不出现，遍历继续；条目被删后 `readlinkat` 失败的符号链接同样被跳过 | 新增 |
| `walk_never_follows_a_directory_swapped_for_a_link` | 接缝在 walk 打开子目录 `d` 之前把它换成指向根外目录的符号链接：根外目录的内容绝不出现，walk 不报错并继续（按"这个目录已经不在了"处理） | 新增 |
| `walk_reports_a_swapped_root` | walk 进行中把根目录改名，并在原路径上放一个新目录：已产出的条目来自被钉住的旧目录，遍历结束时多一条 `Err(Busy)`（`reason = root_changed`；增补 8，评审 I4）；根没被动时没有这一条 | 新增 |
| `walk_start_is_resolved_without_following_links` | `start = Some("a/b")`：产出的路径带 `a/b/` 前缀；`a` 是指向别处的符号链接 → 第一项 `Err(NotDirectory)`（`symlink`），别处的内容不出现；`start` 不存在 → `Err(NoSuchPath)`；`start` 是文件 → `Err(NotDirectory)` | 新增 |
| `walk_names_are_nfc_and_openable_either_way` | 用 NFD 建的名字：`walk` 产出 NFC 的 `RepoPath`，排序也按 NFC 的字节；`open_rooted` 用这个 NFC 路径能打开它；解析器的单元测试喂一个含 NFD 名字的缓冲区，得到同样的 NFC 结果 | 新增 |
| `walk_depth_limit_is_an_error_item` | 深 130 层的链（每层一个字符的目录名，路径远小于 1024 字节）：`walk` 产出到第 128 层的目录，然后一条 `Err(TooLarge)`（`reason = too_deep`；增补 8，评审 I4），不再往下；同一棵树里别的浅分支不受影响；`MAX_WALK_DEPTH` 层以内的树完整遍历 | 新增 |
| `walk_fits_a_low_descriptor_limit` | 父测试启动子进程（`current_exe`，`--ignored --exact low_descriptor_limit_child`）；子进程先把 `RLIMIT_NOFILE` 软限降到 256，再用 `walk` 遍历 512 个互不相同的目录（宽的，外加一条深 100 层的链），不得 `EMFILE`；遍历期间每条条目到达时数一次 `/dev/fd` 的条目，最大值不超过深度加一个小常数，与目录总数无关；父测试断言子进程退出码为 0 | tgrep-core/src/worktrees/tests.rs::managed_and_unmanaged_parent_pins_fit_a_low_descriptor_limit |
| `low_descriptor_limit_child` | 上一行的子进程入口（`#[ignore]`，只由父测试驱动）：在子进程里降限，做同样的 512 目录遍历，再对每个目录做 `attrs_of` 对账（每次都开关句柄，不泄漏）；降限不污染同一进程里的其他测试 | tgrep-core/src/worktrees/tests.rs::low_descriptor_limit_child |
| `bulk_record_parser_follows_the_returned_bitmap` | 单元测试，合成缓冲区：返回位图缺 `GEN_COUNT`（MSDOS 的情形）→ `gen_count = 0`，后面的 `FILEID` 仍解析在正确位置；位图含 `ERROR` 才读错误码；目录记录的尾部是 `MOUNTSTATUS`，文件记录的尾部是 `DATALENGTH`；各字段按 4 字节对齐的未对齐读；一个缓冲区里的 `n` 条记录按各自的长度前进；名字偏移相对 `attrreference_t` 自己 | 新增 |
| `bulk_record_parser_rejects_bad_records` | 单元测试：名字字节不是 UTF-8 → `NotText`（不做 U+FFFD 替换）；记录长度为 0、超出缓冲区、名字偏移越界 → `Unknown`，不 panic、不越界读；`ERROR` 非零的记录交给 `fstatat` 补取分支 | 新增 |
| `bulk_error_record_fallback_has_no_gen_count` | 评审 M10：合成缓冲区里有一条返回位图含 `ERROR`、错误码非零的记录，对应磁盘上一个真实的文件：走 `fstatat` 补取分支，得到的 `Attrs` 里 `kind`、`size`、`mode`、`mtime`、`ctime`、`inode` 与 `std::fs::symlink_metadata` 一致，**`gen_count == 0`**、`gen_count_known()` 为 `false`，`unchanged_since` 对它恒为 `false`；补取时文件已经不存在 → 这一项被跳过，不报错；符号链接的 `size` 是目标长度 | 新增 |
| `bulk_record_maximum_fits_the_minimum_buffer` | 评审 M12：单元测试，合成一条最长的记录（765 字节加 NUL 的名字，位图含 `ERROR` 与全部请求的属性）：长度在 850 到 880 字节之间，且不超过 `MIN_BULK_BUFFER_BYTES`；用 `const` 断言把这条钉在编译期 | 新增 |
| `watch_flags_map_to_events` | 单元测试，纯函数 `translate(flags, path)`：`MustScanSubDirs\|UserDropped`、`MustScanSubDirs\|KernelDropped`、`EventIdsWrapped`、`RootChanged` → `Dropped`；单独的 `MustScanSubDirs` → `MustRescan(path)`；`Mount`、`Unmount` → `MustRescan(path)`；`HistoryDone` 被忽略；`ItemCreated`、`ItemRemoved`、`ItemRenamed`、`ItemModified`、`ItemInodeMetaMod`、`ItemXattrMod`、`ItemCloned` 及其组合 → `Changed`；同一批里的 `Changed` 去重并合成一个，先于 `MustRescan`、`Dropped` 发出；路径前缀的 realpath 被换回调用者传入的 root | 新增 |
| `watcher_reports_must_rescan` | 默认运行（评审 M13）：用**卡住回调**的办法让丢事件很可能发生，再用**重试**让测试可靠——可靠性来自重试，单轮并不保证丢事件。监听一个临时根，回调在第一次被调用时阻塞在一个门上（`mpsc` 通道）；根下先建 16 个子目录，4 个线程一口气创建 20,000 个文件（约 0.4 s，实测每秒约 5 万个），然后放开门。**一轮在这一轮的所有文件都有了着落时结束**——每个被创建的文件，要么被某个 `Changed` 路径覆盖（它本身，或它的某个祖先目录），要么这期间收到过 `Dropped` 或覆盖它的 `MustRescan`——至多 30 s（实测排空要 1 到 4 s）；**不是**"1 s 内不再有回调"，那会让一轮在积压的事件到达之前就结束。数据：第 2 轮评审者复现时，18 个卡住回调的 20,000 文件轮里 16 轮出现丢弃（每轮 1 到 16 个 `UserDropped`），没丢的两轮都是进程里的第一条流，一轮因为 1 s 的静默条件在积压的事件到达之前就结束了（只收到一次回调），另一轮在 3 s 的窗口里收到了全部 20,017 个事件、没有丢弃；我的实测（卡住回调，5,000 到 6 万个文件，21 轮）每轮都出现 `UserDropped`（每轮至少 1 个，中位数 2 个，最多 61 个），不卡住回调、4 万个文件也出现了 40 个。两份数据合起来：卡住大大提高了丢弃的概率，但不保证。断言：至少收到一个 `Dropped`；**"不静默丢失"的不变式在每轮（含没有 `Dropped` 的轮）都成立**；`Dropped` 之后 Watcher 仍然有效：排空后再建一个文件，5 s 内收到覆盖它的 `Changed`。这一轮没有 `Dropped` 就用 4 万个文件再试，至多 3 轮，仍没有则失败并打印原因（不静默通过）。为什么 `Dropped` 满足"必须重扫"：FSEvents 的丢弃信号是 `MustScanSubDirs` 加 `UserDropped`（头文件：对被监听的每个目录做全量扫描），报在流的根路径上；apfs 把这一对译成 `Dropped`（整个监听范围当作未知、全量重扫），单独的 `MustScanSubDirs` 才是 `MustRescan(path)`（`watch_flags_map_to_events` 钉住翻译）。矩阵里的测试名保留。[实现时补的决定]（2026-10-26）写 `after` 之后的存活判定改为按进展：每等满 ARRIVE（15 s）仍没收到 `after`，只要这期间 `changed`/`dropped`/`rescans` 的计数还在增长（积压还在排空）就接着等，总上限 300 s；连续 ARRIVE 没有任何投递才算 watcher 已死。起因：全工作区测试在高负载下，积压的 2 万个文件事件 15 s 内还没排到 `after`（`changed` 从 603 涨到 3186），旧的固定 15 s 误报 | 新增 |
| `watcher_reports_file_and_directory_changes` | 创建、改写、改权限、改名、删除一个文件，各得到一个覆盖该路径的 `Changed`；目录改名只有旧、新两个目录路径，没有子项事件；`clone_tree` 克隆进被监听的根只有一个目录事件，没有子项事件（所以目录事件即整棵子树）；先创建后删除的路径仍出现在 `Changed` 里（消费者要 lstat 复核）；事件可能晚到，测试最多等 5 s，不断言延迟上限 | 新增 |
| `watcher_paths_use_the_callers_root_and_disk_spelling` | 以符号链接根（`/tmp/x`）调用 `watch`：事件路径以 `/tmp/x` 为前缀，不是 `/private/tmp/x`；用 `Foo.txt` 建文件、用 `foo.txt` 去改它：事件里是磁盘上的拼写 `Foo.txt`；NFD 建的名字，事件路径保持 NFD | 新增 |
| `watcher_excludes_configured_dirs` | `exclude = [node_modules]`：往 `node_modules/pkg/x` 写文件不产生 `Changed`，往 `src/x` 写产生；排除按分量比较（`node_modules2` 不被误排除；大小写不敏感的卷上 `Node_Modules` 被排除）；`Dropped` 不被过滤 | 新增 |
| `dropping_the_watcher_stops_callbacks` | `drop(watcher)` 返回之后，再怎么改动被监听的树，回调都不再被调用（等 1 s 复核）；回调正在执行时 drop 会等它返回；回调里 panic 被截住，随后收到一个 `Dropped`，Watcher 仍可正常 drop；闭包捕获一个带 `Drop` 计数的对象和一个 `mpsc::Sender`：`drop(watcher)` 一返回计数就是 1（普通路径在排空之后同步释放状态，闭包的捕获物就在那时 drop），接收端立刻 `Disconnected`，1 s 后计数仍是 1（裁定 "Watcher state has one owner"：消费者可以依赖这一点，也没有第二个所有者再释放一次，评审 M3） | 新增 |
| `watcher_reentry_from_the_callback_never_traps` | 评审 N2 [我的决定]（回调里的 `drop(Watcher)` 与 `flush`）；评审 r2 的 M2、M3、n5：父测试启动子进程（`current_exe`，`--ignored --exact watcher_reentry_child --nocapture`，20 秒死线），断言子进程退出码为 0，不是 133（`SIGTRAP`），**并且**标准输出里有子进程在全部断言通过之后打印的固定一行 `reentry-child: 100 rounds`（`--ignored --exact <名字>` 什么都没匹配到时 libtest 跑 0 个测试、退出码照样是 0，只看退出码会静默通过，评审 M2）；失败时打印子进程的输出和信号。子进程里：（a）100 轮，回调持有最后一个 `Arc<Holder>`，`Holder` 的 `Drop` 里 drop 自己的 `Watcher`（overlay 的 `Tracker` 的形状）；根下写文件触发第一次回调（每 200 ms 再写一个，直到回调到来，至多 10 s；10 s 仍没有事件则失败并打印"流没有收到事件"，不静默通过），回调在 drop 返回之后继续读它捕获的数据（一个 `Vec`，用 `Arc` 计数确认它没被提前释放），然后设一个"回调结束"标志：drop 没有 `SIGTRAP`、回调跑到了结尾、结束之后再等 1 s 没有第二次回调进入 `on_event`、私有队列上的收尾块已执行；**回调的闭包捕获一个带 `Drop` 计数的对象，5 s 内计数变成恰好 1，不是 0 也不是 2，再等 1 s 仍是 1**（评审 M3：状态只有 `Watcher` 一个所有者；若 FSEvents 的 `context.release` 也去释放，或回调里的路径与普通路径各释放一次，这里会读到 2 或崩溃）；（b）同样的回调里对**自己的** `Watcher` 调 `flush()`：release 构建（`cargo test --release`）立即返回、什么都不做、没有新的 `Dropped`；debug 构建 `debug_assert!` 失败，被回调外层的 `catch_unwind` 截住，随后收到一个 `Dropped`；两种构建都没有 `SIGTRAP`；（c）控制者对评审 n5 的更正，按线程判（20 轮）：两个 Watcher A、B 各监听一个临时根，B 的回调第一次被调用时睡 300 ms 才算结束（记 `started`、`finished`）；A 的回调只在测试线程设了 `go` 之后才动作，`go` 在 B 的回调已经开始之后才设；A 的回调对 B 调 `flush()`：立即返回（debug 构建被 `catch_unwind` 截住的 `debug_assert!` 也算这条无操作路径），返回时 B 的 `finished` 仍小于 `started`（没有等 B 的回调，不是排空）；A 的回调再 drop B：不 `SIGTRAP`，走异步路径，A 的回调跑到结尾，之后 B 的闭包的 `Drop` 计数变成恰好 1。[实现时补的决定]（2026-10-26）死线改为"无进展"判定：子进程每做完一轮在标准错误打印一行 `reentry-child: tick`，父进程只在 60 s 内没有新的 tick 时（或总计超过 900 s）杀掉子进程并失败，报出已完成的轮数。原来的固定墙钟死线在机器重负载（load average 约 76，别的 cargo 构建并行）时误报：健康的子进程要 94–122 s 才跑完，daemon 停与不停都一样，不是死锁，也不是 daemon 的 FSEvents 流挤占。回调里卡住仍被子进程自己的断言或 60 s 无进展抓住（两种注入都验证过会失败）。 | 新增 |
| `watcher_reentry_child` | 上一行的子进程入口（`#[ignore]`，只由父测试驱动）：`SIGTRAP` 只杀这个子进程，不连累同一进程里的其他测试；断言全过之后打印上一行的标记 | 新增 |
| `watcher_reports_a_renamed_root_as_dropped` | 被监听的根被改名（或它的祖先被改名）：收到 `Dropped`（`RootChanged`）；之后在原路径放一个新目录并写文件，旧 Watcher 看不到新目录里的事件，调用方丢弃它、重新 `watch` 才能再收到 | 新增 |
| `flush_drains_the_callback_queue` | 评审 I5、增补 7；评审 N1 改写了保证的措辞：flush 返回时，**排空之前已经排进队列的回调都已执行完**（不是"此刻没有回调在执行"：排空之后新到的事件可以在 flush 返回之前就开始一次回调）。（a）回调在第一次被调用时阻塞在门上，回调里记 `started` 与 `finished` 两个计数；另一个线程在调用 `flush()` 之前读下 `started`（记为 n），然后调 `flush()`：门没开之前 `flush` 不返回（200 ms 后 `is_finished()` 仍为假）；门一开、回调返回之后它才返回；返回时 `finished >= n`；**不断言**"返回时没有回调在执行"。（a'）安静的流上（写入停了，睡到计数稳定）：`flush()` 返回时"执行中"计数（`started - finished`）为 0——这一条只在流安静时成立，所以只在这里断言。（b）空闲的流上 `flush()` 在 1 s 内返回，连续 100 次也不挂；（c）4 个线程同时调 `flush()` 互相串行、不死锁；（d）编译期断言 `Watcher: Send + Sync`；（e）在任何一个 Watcher 的回调里调 `flush()`，见 `watcher_reentry_from_the_callback_never_traps`（debug 构建会 `debug_assert!`，所以在子进程里测） | 新增 |
| `flush_is_not_a_fence` | `#[ignore]`，只打印数字，供 macOS 26 上重测 §4.2 F：写 50 个文件之后立刻 `flush()`，打印返回时已送达的比例（本机 11% 到 35%）；写完睡 5、20、100 ms 再 `flush()` 的比例（本机 0）；"哨兵加 `flush`"规则在安静的流上与有洪泛时各 40 轮里的不完整轮数。不断言任何比例，只断言 `flush()` 没有 panic、没有挂住 | 新增 |
| `case_fold_eq_known_pairs` | 表驱动，不碰文件系统，期望值全取自 oracle 测试在本机验证过的结果：`ß`≡`SS`、`ﬃ`≡`ffi`、`İ`≡`i̇` 但 `İ`≢`i`、`ς`≡`σ`≡`Σ`、开尔文符号≡`k`、埃符号≡`å`、切罗基、格鲁吉亚、Deseret、Unicode 16 新增（Garay、U+1C89 与 U+1C8A、U+A7CB 与 U+0264、U+A7DC）、NFC≡NFD；零宽连接符、软连字符、VS16 不可忽略；全角 `Ａ` ≢ `a`；ASCII 快路径；空串 | 新增 |
| `case_fold_key_agrees_with_case_fold_eq` | 评审 I6、增补 7：对 `case_fold_eq_known_pairs` 的每一对和固定种子生成的 1 万对随机串（ASCII、拉丁文带重音、希腊文、西里尔文、格鲁吉亚文、切罗基、Deseret、Unicode 16 新增的字符、组合记号、NFC 与 NFD 混杂；其中一半是同一个串的大小写变体），断言 `case_fold_eq(a, b) == (case_fold_key(a) == case_fold_key(b))`；`case_fold_key` 对纯 ASCII 等于 `to_ascii_lowercase`；`case_fold_key("ß") == case_fold_key("SS")`；键不是 NFC（`"é"` 的键是 NFD）；`HashSet` 以键为元素时，大小写别名只占一个位置——折叠成员判断不需要两两比较。失败时打印那一对的码位 | 新增 |
| `case_fold_key_of_a_path_is_the_slash_joined_keys` | 裁定 "case_fold_key prefix property"（§5.8；overlay 对受限目录按键前缀做范围查找依赖它）：对固定的 (a, b) 与固定种子生成的 1 万对随机串（同 `case_fold_key_agrees_with_case_fold_eq` 的生成器，另加空串、以 `/` 开头或结尾的片段、组合记号开头的 `b`（`"\u{0301}x"`）、以基字符结尾的 `a`（`"e"`）、`ß`、`İ`、`ﬃ`、Deseret、切罗基）断言 `case_fold_key(a + "/" + b) == case_fold_key(a) + "/" + case_fold_key(b)`——特别是 `a = "e"`、`b = "\u{0301}x"`：路径整体 NFD 之后组合记号仍隔着 `/`，不会与 `e` 合并；纯 ASCII 路径同样成立；失败时打印那一对的码位。范围查找的用法：一组键里 `Dir/x`、`dir/y`、`DIR/z/w`、`dir2/x`、`dirs`，以 `case_fold_key("Dir") + "/"` 为前缀的恰好是前三个，`dir2/x` 与 `dirs` 不在其中 | 新增 |
| `case_fold_eq_matches_a_case_insensitive_volume` | oracle：在大小写不敏感的 APFS 镜像上，对表里每一对 (a, b) 先用 a 建文件，再问文件系统 b 能不能打开它，断言 `case_fold_eq(a, b)` 与 `case_fold_key(a) == case_fold_key(b)` 都与文件系统的答案一致；表覆盖 §4.2 B 的全部类别，再加按码位区间随机抽样的对子（折叠表的 Unicode 版本落后就会在这里失败） | 新增 |
| `case_sensitive_volume_distinguishes_case_but_not_normalization` | 大小写敏感的 APFS 镜像：`a` 与 `A` 是两个文件，NFC 与 NFD 的拼写是同一个文件（建一个，用另一个拼写 `open` 成功、`create` 得 `EEXIST`）；`volume_caps(..).case_sensitive` 为 `true`；说明调用方在这种卷上不该用 `case_fold_eq` | 新增 |
| `walk_tool_sized_tree_within_the_reconcile_budget` | `#[ignore]`（量级测试）：合成 3 万项的源码树（目录、小文件、符号链接的比例取自 tool），`walk` 全量遍历在 3 s 内完成（design §8"全量对账 tool 源码树 < 3 s"；实测冷读 1.42 s），打印耗时与每项微秒数，写回 §5.9 | 新增 |
| `clone_node_modules_sized_tree_within_the_budget` | `#[ignore]`：合成 10 万项的树，`clone_tree` 在 5 s 内完成（实测 11.7 万项 2.4 到 3.9 s）；4 路并行克隆 4 份的总耗时不超过单份的 2 倍；打印这些数字，以及用 `remove_tree_rooted` 删其中一份的耗时（与 `rm -rf` 对照；session `rm`、store gc 的成本，实测 29 对 38 µs 每项，§4.2 H）；另打印整个布局的投影（各仓库项数乘每项成本，§5.9）。`remove_tree_rooted` 的吞吐在这里量，不另设测试 | 新增 |

**理由点名 apfs 的 `—` 行**（共 8 行，外加 3a 的 `…a_path_is_accepted_at_4096_bytes_and_refused_at_4097` 一行，它的理由写"归类同上一行"）怎样落实：

- 3a `filesystem_attributes.rs::the_metadata_forms_compare_by_value`（不保留）：overlay 只比内容哈希与 mode，不比 mtime，"touch 不算改动"由 `gen_count_changes_on_write_not_on_touch` 与 `attrs_unchanged_since_rules` 承担。
- 3a `filesystem_limits.rs::a_component_name_is_accepted_at_255_bytes_and_refused_at_256` 与 `…a_path_is_accepted_at_4096_bytes_and_refused_at_4097`（替代）：上表的 `name_over_255_utf16_units_is_invalid_input`、`path_of_1024_bytes_is_invalid_input`；两行用了 3a 的来源。
- 3b `init_sqlite.rs::an_unsupported_platform_refuses_the_store_before_any_file_exists`（不保留）：上表的 `clone_on_unsupported_volume_is_unsupported`，另有 `volume_caps_per_filesystem` 给 store 的预检用。
- 3e `init_refusals.mjs::overlay`（不保留，Linux 容器的 overlayfs 回归）：没有对应的 apfs 测试；非 APFS 或跨卷的拒绝由 Review Focus 1 的跨卷拒绝测试和 `clone_on_unsupported_volume_is_unsupported` 承担。
- 3f `walker.rs::walk_dir_retains_binary_extensions_for_file_listing`（不保留）：`apfs::walk` 不按扩展名过滤，这一点在 `walk_lists_all_entries_relative_to_root` 里断言。
- 3f `worktrees/tests.rs::snapshot_fifo_swaps_invalidate_readiness_without_blocking` 与 `…::directory_link_swap_after_discovery_keeps_the_view_not_ready`（依赖承担，tgrep-core 自己的测试覆盖库内路径）：strata 自己读取路径上的同一语义是上表的 `rooted_open_*` 各行和 `walk_never_follows_a_directory_swapped_for_a_link`。
- 3f `corrupt_index_reader.rs::builder_and_reader_round_trip_unix_colon_filenames`（不保留）："冒号名字按字面打开"是 apfs 要满足的，上表的 `rooted_open_treats_colon_and_backslash_names_literally` 用了这个来源。

3f 里还有几行跟大小写折叠有关，但目标是别的模块：gitio 的 `tracked_lookup_folds_ascii_case_when_ignorecase` **不用** `case_fold_eq`（git 身份只折叠 ASCII，增补 2；3f:155 写的 `apfs::case_fold_eq` 由 T15 统一）；overlay 的大小写别名一行才用（磁盘身份，用 `case_fold_key`）。它们在各自模块里测"怎样使用"，apfs 的契约由 `case_fold_eq_*` 与 `case_fold_key_*` 钉住。折叠有两个问题：磁盘上这两个名字是不是同一个文件（看卷，用 apfs 的折叠），git 把这两个路径当同一个吗（看 `core.ignorecase`，gitio 的 ASCII 折叠）（§4.2 B）。

**统计**：上表共 138 个测试名。矩阵里目标为 `apfs` 的 25 行全部落在上表（3b 2 行、3c 3 行、3d 1 行、3e 1 行、3f 18 行），另有 4 行 `—` 行借用来源（3a 2 行、3b 1 行、3f 1 行），所以有矩阵来源的共 29 行；其余 109 行是新增用例。第 1 轮评审净增 28 行（加了 32 行，改了 4 个名字）：`rename_rooted_exclusive_*` → `rename_rooted_excl_*`，`unlink_rooted_removes_one_entry_without_following` → `remove_rooted_file_removes_one_entry_without_following`，`rmdir_rooted_removes_only_empty_directories` → `remove_rooted_empty_dir_removes_only_empty_directories`；矩阵来源的行一行没动。第 2 轮评审与控制者裁定净增 13 行（加了 14 行，删了 1 行）：新增 `rooted_roots_must_be_canonical`、`remove_tree_rooted_takes_any_entry_kind`、`remove_tree_rooted_leaves_a_shared_flagged_inode_alone`、`stuck_reason_classification_and_names`、`blocking_flags_lists_the_flags_that_block_removal`、`make_removable_clears_flags_before_mode`、`set_file_meta_changes_metadata_through_a_handle`、`make_removable_and_set_file_meta_refuse_by_rule`、`clone_from_rooted_checks_the_source_kind`、`symlink_recipe_recreates_a_workspace_symlink`、`watcher_reentry_from_the_callback_never_traps` 与它的子进程入口 `watcher_reentry_child`、`case_fold_key_of_a_path_is_the_slash_joined_keys`；`remove_report_round_trips_through_the_error_detail` 被 `remove_report_counts_and_completeness` 取代（`to_detail`、`from_error` 取消了）；另有几行改写了断言：`remove_tree_rooted_*` 的其余几行、`clone_file_rooted_sets_the_requested_mode`、`remove_rooted_empty_dir_removes_only_empty_directories`、`open_root_returns_the_root_handle`、`nearest_existing_ancestor_climbs_to_the_first_existing_path`、`errno_mapping_table`、`flush_drains_the_callback_queue`、`watcher_reports_must_rescan`；矩阵来源的行的"来源"一栏仍然一行没动（只有 `rooted_open_opens_files_and_directories` 的断言里有一句话随 `make_removable` 改了）。第 3 轮（r2 复审的 M1 到 M5、n1 到 n6）不增减行，也不改名：新的分支都写进了既有的行（`remove_tree_rooted_reports_what_it_cannot_remove` 的 ACL 目录、`stuck_reason_classification_and_names` 的 chmod 门、`rooted_roots_must_be_canonical` 的相对路径与点开头的分量、`watcher_reentry_from_the_callback_never_traps` 的跨 Watcher 一支与恰好一次的 `Drop` 计数、`dropping_the_watcher_stops_callbacks` 的普通 drop 计数、`clone_file_rooted_sets_the_requested_mode` 的叫 `mode` 的 dst、`set_file_meta_changes_metadata_through_a_handle` 的越界 mtime 只改一次，以及两个子进程测试的标记）。

## 8. 实现任务（TDD）

前提：workspace 根 `Cargo.toml` 与 `crates/strata-types` 已经存在（types.md §8）。本 crate 的 feature `test-seams` 默认关闭，要用接缝的集成测试（Task 3、4、5、6、7）带 `--features test-seams` 运行。挂载镜像的测试把整个测试根建在内部卷的临时目录（`std::env::temp_dir()`）下：2026-10-22 实测，挂载点放在外置盘 `/Volumes/GWD` 的子目录里（新建的 755 目录也一样；该卷以 `noowners` 挂载，见 `mount` 的输出），`hdiutil attach -mountpoint` 得 `Permission denied`，放在内部卷的临时目录里可以。所有测试只用 `tempfile` 目录和 `hdiutil` 稀疏镜像，从不碰 `/Volumes/GWD/inter`；`tests/common/mod.rs` 提供 `Image::create(fs, size_mb)`（`hdiutil create -type SPARSE`，`fs` 取 APFS、大小写敏感的 APFS、HFS+、MS-DOS，挂载点缺省在临时目录里，`Drop` 时 `hdiutil detach -force` 并删镜像；`attach_at(dir)` 把它挂到指定的空目录，嵌套挂载的测试用；`reattach_read_only()` 分离后以 `hdiutil attach -readonly` 重新挂载，只读卷的测试用）、`make_fifo`、`make_socket`（先在短路径上 `bind` 再 `rename` 进目标，避开 `sockaddr_un` 的 104 字节上限）、`fill_until_enospc`、`remove_protected_tree`（带标志、带 ACL、mode 000 的树留在磁盘上的测试靠它清理：先 `chflags -R nouchg,nouappnd`、`chmod -R u+rwx`、`chmod -R -N`，再删）。每个任务先写测试、看它因函数还没定义而编译失败，再实现。

### Task 1: crate 骨架、errno 归类、卷标识与能力、最近的已存在上级

**Files:**
- Create: `crates/strata-apfs/Cargo.toml`、`crates/strata-apfs/src/lib.rs`、`crates/strata-apfs/src/error.rs`（`IoSide`、`classify_io_error`，§4.2 I 与 types.md §6.3）、`crates/strata-apfs/src/volume.rs`
- Modify: `Cargo.toml`（把 `crates/strata-apfs` 加进 `members`）
- Test: `crates/strata-apfs/tests/volume.rs`、`crates/strata-apfs/tests/errno.rs`、`crates/strata-apfs/tests/common/mod.rs`

**Interfaces:**
- Consumes: `strata_types::{StrataError, Code}`
- Produces: `Result`、各常量、`IoSide`、`classify_io_error`、`same_volume`、`nearest_existing_ancestor`、`VolumeCaps`、`volume_caps`；crate 内部的错误构造辅助（带 `errno`、`path`、`reason` 的 `detail`）

- [ ] **Step 1: 写失败的测试**：§7 的 `errno_mapping_table`（`src/error.rs` 里的单元测试）、`classify_io_error_matches_real_syscalls`（`tests/errno.rs`，用镜像）、`creating_a_non_utf8_name_fails_with_eilseq`、`same_volume_tells_system_volume_from_data_volume`、`same_volume_follows_links_and_reports_missing_paths`、`nearest_existing_ancestor_climbs_to_the_first_existing_path`、`volume_caps_per_filesystem`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-apfs --test volume --test errno`
Expected: FAIL（`same_volume`、`volume_caps`、`classify_io_error` 还没定义，编译不过）

- [ ] **Step 3: 写让测试通过的最少实现**：`same_volume` 按 §5.1（`getattrlist` + `FSOPT_RETURN_REALDEV`，不带 `FSOPT_NOFOLLOW`）；`nearest_existing_ancestor` 按 §5.1（`stat` 跟随符号链接，`ENOENT`、`ENOTDIR` 往上爬，别的错误原样）；`volume_caps` 读 `statfs` 与 `ATTR_VOL_CAPABILITIES`，`clone` 用 `Option<bool>` 表示有效位；`classify_io_error` 是纯函数，表与 §4.2 I 逐行一致（也就是 types.md §6.3 加 apfs 的细化），读侧的 `EACCES`、`EPERM` 是 `Unknown`、写侧是 `ReadOnly`，`AfterWrite` 一律 `Unknown`，`detail` 带 `errno` 与 `path`；`Image` 辅助函数建不了镜像时 `panic!` 并说明原因，不跳过
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-apfs --lib && cargo test -p strata-apfs --test volume --test errno`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/strata-apfs
git commit -m "feat(apfs): errno classification, same_volume, nearest_existing_ancestor and volume_caps"
```

### Task 2: `clone_tree` 与 `clone_file`

**Files:**
- Create: `crates/strata-apfs/src/clone.rs`
- Test: `crates/strata-apfs/tests/clone.rs`

**Interfaces:**
- Consumes: Task 1 的 `same_volume`、`volume_caps`、`classify_io_error`；`strata_types::{Code, StrataError}`
- Produces: `clone_tree`、`clone_file`；crate 内部的检查函数（src 分类、dst 形态、已存在、父目录），Task 4、5 复用

- [ ] **Step 1: 写失败的测试**：§7 里 Review Focus 1 的跨卷拒绝测试（两个镜像，`clone_tree` 跨卷 → `CrossVolume`，B 的可用空间前后之差小于 1 MiB，不退化成复制；它的名字见 §7 表的第一行）、`clone_file_refuses_cross_volume`、`clone_on_unsupported_volume_is_unsupported`、`clone_refusals_leave_no_residue`（`clone_tree_rooted` 与 `clone_from_rooted` 的那一半在 Task 4、5 补上）、`clone_tree_error_precedence`、`clone_tree_preserves_content_modes_links_and_holes`、`clone_tree_keeps_xattrs_and_flags_but_drops_acls_and_setuid`、`clone_tree_makes_independent_hard_links`、`clone_tree_clones_fifos_and_sockets_as_objects`、`clone_tree_leaves_a_nested_mount_as_an_empty_stub`、`clone_tree_fails_atomically_on_unreadable_entry`、`clone_tree_on_a_full_volume_leaves_no_destination`、`clone_file_rejects_directories_and_special_files`、`clone_file_clones_symlinks_as_symlinks`、`name_over_255_utf16_units_is_invalid_input`、`path_of_1024_bytes_is_invalid_input`、`clone_is_copy_on_write_and_isolated`、`clone_then_small_write_allocates_only_the_touched_extent`
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

### Task 3: 根之下的解析、`open_root`、`open_rooted`、`open_rooted_file` 与测试接缝

**Files:**
- Create: `crates/strata-apfs/src/rooted.rs`（`resolve`，§5.4）、`crates/strata-apfs/src/seams.rs`
- Test: `crates/strata-apfs/tests/rooted.rs`、`crates/strata-apfs/tests/roots.rs`（`rooted_roots_must_be_canonical`：这张表随各任务落地的函数逐步加长，Task 7 之后齐全）

**Interfaces:**
- Consumes: Task 1 的 `classify_io_error`；`strata_types::RepoPath`
- Produces: `open_root`、`open_rooted`、`open_rooted_file`、`seams::{set_before_open, set_after_bulk_window}`（`feature = "test-seams"`）；crate 内部的 `resolve`（Task 4、5、6、7 复用）与规范根检查 `check_root`（每个以 root 为参数的函数的第一行）

- [ ] **Step 1: 写失败的测试**：§7 的 `rooted_open_opens_files_and_directories`、`open_root_returns_the_root_handle`（含不规范 root 与 `lnk/` 的几行）、`rooted_roots_must_be_canonical`（`tests/roots.rs`，此刻只有 `open_root`、`open_rooted`、`open_rooted_file` 的行）、`rooted_open_treats_colon_and_backslash_names_literally`、`rooted_open_fifo_is_nonblocking_and_symlinks_are_refused`（子进程加 20 秒死线）、`rooted_open_rejects_a_directory_swapped_for_a_link`、`rooted_open_keeps_the_parent_handle_after_a_swap`、`rooted_open_pins_the_root`、`rooted_open_rejects_a_final_symlink_swap`、`rooted_open_file_refuses_a_fifo_swapped_in`、`rooted_open_error_codes`、`rooted_open_name_too_long_is_invalid_input`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-apfs --features test-seams --test rooted --test roots`
Expected: FAIL（`open_rooted`、`open_root` 还没定义，编译不过）

- [ ] **Step 3: 写让测试通过的最少实现**：`resolve` 按 §5.4：根先按原始字节检查是规范路径（空串、不是绝对路径、结尾的 `/`、恰好是 `.` 或 `..` 的分量 → `InvalidInput`，`root_not_canonical`；不用 `Path::components()`，它把结尾的 `/` 与 `.` 吃掉了；这个检查是 crate 内部的 `check_root`，之后每个任务里以 root 为参数的函数都在第一行调它），再用 `O_DIRECTORY | O_NOFOLLOW` 打开（`open_root` 就是这一步），每一级 `openat(O_DIRECTORY | O_NOFOLLOW)`，每次 `openat` 之前调一次 `before_open`（落地的那一次调用之前也调，step 接着数），手里只留当前一级的句柄；`ENOTDIR` 之后用 `fstatat(AT_SYMLINK_NOFOLLOW)` 区分 `reason`；终点用 `O_RDONLY | O_NOFOLLOW | O_NONBLOCK`，`fstat` 后拒绝设备；`ENAMETOOLONG` → `InvalidInput`，其余未列出的 errno → `Unknown`；`open_rooted_file` 在此之上要求 `S_ISREG`。不用 `O_NOFOLLOW_ANY`
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-apfs --features test-seams --test rooted --test roots`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-apfs/src/rooted.rs crates/strata-apfs/src/seams.rs crates/strata-apfs/src/lib.rs crates/strata-apfs/tests/rooted.rs crates/strata-apfs/tests/roots.rs
git commit -m "feat(apfs): open_root and open_rooted with per-component O_NOFOLLOW and test seams"
```

### Task 4: 目的地变体、改名、`remove_rooted`、`full_fsync` 与经句柄改元数据

**Files:**
- Create: `crates/strata-apfs/src/rooted_write.rs`、`crates/strata-apfs/src/rooted_rename.rs`、`crates/strata-apfs/src/rooted_remove.rs`、`crates/strata-apfs/src/sync.rs`、`crates/strata-apfs/src/meta.rs`（`make_removable`、`set_file_meta`、`FileMeta`、crate 内部的 `blocking_flags`，§5.4）
- Test: `crates/strata-apfs/tests/rooted_write.rs`、`crates/strata-apfs/tests/rooted_rename.rs`、`crates/strata-apfs/tests/rooted_remove.rs`、`crates/strata-apfs/tests/meta.rs`；`meta.rs` 内的单元测试

**Interfaces:**
- Consumes: Task 2 的检查函数（src 分类、dst 形态、`same_volume`）；Task 3 的 `resolve`、`open_root` 与 `open_rooted`；Task 1 的 `tests/common` 的 `Image`（HFS+ 镜像、嵌套挂载）
- Produces: `clone_file_rooted`、`clone_tree_rooted`、`create_file_rooted`、`mkdir_rooted`、`symlink_rooted`、`RenameMode`、`rename_rooted`、`RemoveKind`、`remove_rooted`、`full_fsync`、`make_removable`、`FileMeta`、`set_file_meta`、`seams::{set_before_meta, MetaChange}`（`feature = "test-seams"`）；crate 内部的 `blocking_flags`（Task 5 复用）

- [ ] **Step 1: 写失败的测试**：§7 的 `make_removable_clears_flags_before_mode`、`set_file_meta_changes_metadata_through_a_handle`（都在 `tests/meta.rs`）、`blocking_flags_lists_the_flags_that_block_removal`、`make_removable_and_set_file_meta_refuse_by_rule`（`meta.rs` 里的单元测试，不碰文件系统）；`rooted_destination_refuses_a_symlinked_parent`、`rooted_destination_never_overwrites`、`rooted_destination_swap_after_resolution_stays_inside`、`mkdir_rooted_parents_semantics`、`clone_file_rooted_sets_the_requested_mode`、`clone_tree_rooted_is_atomic_and_refuses_cross_volume`、`create_file_rooted_and_symlink_rooted_basics`（都在 `tests/rooted_write.rs`）；`rename_rooted_excl_never_overwrites`、`rename_rooted_excl_renames_links_and_case_only_spellings`、`rename_rooted_swap_exchanges_two_existing_entries`、`rename_rooted_error_mapping`、`rename_rooted_refuses_symlinked_parents_on_both_sides`、`rename_rooted_directory_reparenting_needs_write_permission`、`full_fsync_accepts_file_and_directory_handles`（都在 `tests/rooted_rename.rs`）；`remove_rooted_refuses_symlinked_parents`、`remove_rooted_file_on_a_directory_returns_is_directory`、`remove_rooted_file_removes_one_entry_without_following`、`remove_rooted_empty_dir_removes_only_empty_directories`（都在 `tests/rooted_remove.rs`）；并把 Task 2 的 `clone_refusals_leave_no_residue` 里 `clone_tree_rooted` 的那一半补上，把本任务的函数（`clone_file_rooted`、`clone_tree_rooted`、`create_file_rooted`、`mkdir_rooted`、`symlink_rooted`、`rename_rooted`、`remove_rooted`）的行加进 Task 3 的 roots 表（`tests/roots.rs`）
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-apfs --features test-seams --test rooted_write --test rooted_rename --test rooted_remove --test meta`
Expected: FAIL（这些函数还没定义，编译不过）

- [ ] **Step 3: 写让测试通过的最少实现**：(a) 经句柄改元数据（先做，目的地变体要用它），照 §5.4 "`make_removable(file)` 与 `set_file_meta(file, meta)`"：先 `fstat` 句柄核对种类（`set_file_meta` 收普通文件与目录）、`st_nlink`（只对非目录）、`SF_*`，拒绝时什么都不改；`make_removable` **先**清 `UF_IMMUTABLE`、`UF_APPEND`（`fchflags`，其它位保留）、**再** `fchmod` 补 u+w；`set_file_meta` 按清标志、设 mode、设 mtime 的次序，每一项只在需要时才调（无操作的调用也会更新 `ctime`）；第一项改动生效之后的失败是 `Unknown`；两个函数都在每一次真要调用的改动之前触发 `before_meta(step, change)`（枚举参数，不与 `before_open` 的文件名共用，叫 `mode` 的 dst 不会混淆；序号每次调用从 0 起）；纯函数 `blocking_flags` 列出挡删除的标志名；`SF_*` 要 root 才造得出，它们的拒绝由纯函数表测。(b) 目的地变体：共同部分 `resolve` 到父目录句柄 P，只用最后一个名字做一次 `*at` 调用（`clonefileat`、`openat(O_CREAT | O_EXCL | O_NOFOLLOW)`、`mkdirat`、`symlinkat`），落地之前触发 `before_open`，不预先 stat，`EEXIST` 就是 `Exists`；flags 只传 `CLONE_NOFOLLOW | CLONE_NOOWNERCOPY`，不传 `CLONE_NOFOLLOW_ANY`、`CLONE_RESOLVE_BENEATH`（它们连 src 一起约束）；`clone_file_rooted` 的 `mode = Some(m)`：克隆之后在 P 上 `openat(O_RDONLY | O_NOFOLLOW | O_NONBLOCK)` 取回 dst（`before_open` 再触发一次，step 接着数），交给 `set_file_meta`（`mode: Some(m & 0o777)`，它自己触发 `before_meta(0, Mode)`），这一步的任何失败都是 `Unknown`；不用 `fchmodat`，不经名字改 mode；`mkdir_rooted(parents = true)` 逐级建，记录 `created`，建出一级之后的失败升级为 `Unknown`。(c) 改名与删除，照 §5.4 "改名、删除与 `full_fsync`"：`rename_rooted` 先拦 `same_path`、`into_itself`，解析两个父目录，`renameatx_np` 带 `RENAME_EXCL` 或 `RENAME_SWAP`，失败后用 `fstatat` 区分 `from_missing`、`to_missing`，并给 `ReadOnly` 填 `reason`（`reparent_needs_write`、`immutable`、`no_permission`、`read_only_volume`）；`remove_rooted` 一次 `unlinkat`（`File` 带 0，`EmptyDir` 带 `AT_REMOVEDIR`），`File` 对 `EPERM` 用 `fstatat` 区分目录（`IsDirectory`）与标志（`immutable`），`EmptyDir` 先 `fstatat` 判出挂载点（`st_dev` 与父目录不同）→ `CrossVolume`（`mount_point`），`rmdir` 晚到的 `EBUSY` 同样映射，接缝紧挨着 `unlinkat`（在 `fstatat` 之后）；`full_fsync` 是 `fcntl(F_FULLFSYNC)` 加 `EINTR` 重试。`rename_rooted`、`remove_rooted` 不 `fsync`、不 `chmod`、不清标志
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-apfs --lib meta && cargo test -p strata-apfs --features test-seams --test rooted_write --test rooted_rename --test rooted_remove --test meta --test clone --test roots`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-apfs/src/rooted_write.rs crates/strata-apfs/src/rooted_rename.rs crates/strata-apfs/src/rooted_remove.rs crates/strata-apfs/src/sync.rs crates/strata-apfs/src/meta.rs crates/strata-apfs/src/lib.rs crates/strata-apfs/tests/rooted_write.rs crates/strata-apfs/tests/rooted_rename.rs crates/strata-apfs/tests/rooted_remove.rs crates/strata-apfs/tests/meta.rs crates/strata-apfs/tests/roots.rs crates/strata-apfs/tests/clone.rs
git commit -m "feat(apfs): rooted no-follow destinations, rename, remove_rooted, full_fsync, make_removable and set_file_meta"
```

### Task 5: `remove_tree_rooted` 与 `clone_from_rooted`

**Files:**
- Create: `crates/strata-apfs/src/remove_tree.rs`（`remove_tree_rooted`、`RemoveReport`、`Stuck`、`StuckReason`、`MAX_REPORTED_STUCK`，§5.4）、`crates/strata-apfs/src/clone_from.rs`（`clone_from_rooted`、`CloneKind`）
- Test: `crates/strata-apfs/tests/remove_tree.rs`、`crates/strata-apfs/tests/clone_from.rs`；`remove_tree.rs` 内的单元测试

**Interfaces:**
- Consumes: Task 1 的 `classify_io_error`、`same_volume`、`volume_caps`；Task 2 的检查函数（dst 形态、已存在、父目录）与 `clone_tree`、`clone_file`（对照用）；Task 3 的 `resolve`、`check_root`、`open_rooted` 的终点打开逻辑与接缝；Task 4 的 `blocking_flags`
- Produces: `remove_tree_rooted`、`RemoveReport`（`#[must_use]`，`is_complete`）、`Stuck`（含 `detail`）、`StuckReason`（含 `as_str`）、`MAX_REPORTED_STUCK`、`CloneKind`、`clone_from_rooted`

- [ ] **Step 1: 写失败的测试**：§7 的 `remove_tree_rooted_removes_a_protected_tree`、`remove_tree_rooted_takes_any_entry_kind`、`remove_tree_rooted_reports_what_it_cannot_remove`、`remove_tree_rooted_leaves_a_shared_flagged_inode_alone`、`remove_tree_rooted_refuses_before_touching_anything`、`remove_tree_rooted_never_follows_a_swapped_directory`、`remove_tree_rooted_depth_limit`、`remove_tree_fits_a_low_descriptor_limit`、`remove_tree_low_descriptor_limit_child`（后者 `#[ignore]`，只由前者启动）、`remove_report_counts_and_completeness`（`remove_tree.rs` 里的单元测试加一个 `compile_fail` 的 doc-test，不碰文件系统）、`stuck_reason_classification_and_names`（`remove_tree.rs` 里的单元测试，纯函数）、`clone_from_rooted_clones_files_and_directories`、`clone_from_rooted_checks_the_source_kind`、`clone_from_rooted_refuses_a_symlinked_parent`、`clone_from_rooted_clones_the_opened_object_after_a_swap`、`clone_from_rooted_error_codes`；并把 Task 2 的 `clone_refusals_leave_no_residue` 里 `clone_from_rooted` 的那一半补上，把 `clone_from_rooted`（`src_root`）与 `remove_tree_rooted` 的行加进 Task 3 的 roots 表（`tests/roots.rs`）
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-apfs --features test-seams --test remove_tree --test clone_from`
Expected: FAIL（`remove_tree_rooted`、`clone_from_rooted` 还没定义，编译不过）

- [ ] **Step 3: 写让测试通过的最少实现**：(a) `remove_tree_rooted` 照 §5.4 的"遍历之前"五步与"遍历"九步：`Err` 只给遍历之前的拒绝（具名的码，什么都没碰），遍历一开始就一律 `Ok(RemoveReport)`；目标可以是任何种类的条目，非目录直接走 `unlink_leaf`；遍历自底向上，每层一个句柄，读完名字就关目录流；清标志只用 `fchflags`（经句柄；符号链接用 `O_SYMLINK` 的句柄，打不开的非符号链接再试 `O_EXEC`），**不用 `fchflagsat`**（macOS 27 才有），而且只在不共用的 inode 上（`st_nlink == 1`，取到句柄后再 `fstat` 一次）；打不开的目录先试 `O_EXEC` 句柄（清标志、`fchmod`、`O_RDONLY` 重开），只有属主没有搜索位、目录又不带 `UF_IMMUTABLE`、`UF_APPEND`、`SF_IMMUTABLE`、`SF_APPEND`（第 1 步的 `fstat` 判，一个纯函数，有单元测试）才用按名字的 `fchmodat(AT_SYMLINK_NOFOLLOW)` 补 `0700`（§4.2 H 的残余风险，只用于这一种目录）；属主有搜索位而两种打开都是 `EACCES`（ACL）→ `NoPermission`，带标志 → `Immutable`，这两种都不去 chmod；打得开而写不进的用句柄 `fchmod`；每次取到句柄都核对 (dev, inode)；不进入 `st_dev` 不同的目录（记 `OtherDevice`）；`SF_*` 只报告，从不清；原因与 `detail` 一律按 §5.4 的 stuck 原因表（归类是一个纯函数，有单元测试）；`stuck` 最多记 `MAX_REPORTED_STUCK` 条，`stuck_total` 照实数；没有 `to_detail`、`from_error`，报告不进 `StrataError`。(b) `clone_from_rooted` 照 §5.4：先查 dst，再 `resolve`、`openat(O_RDONLY | O_NOFOLLOW | O_NONBLOCK)`，`fstat` 句柄核对 `kind`（在同卷检查与 `fclonefileat` 之前，种类不符就什么都不克隆），同卷检查用句柄的真实设备号，最后 `fclonefileat(fd, AT_FDCWD, dst, CLONE_NOFOLLOW | CLONE_NOOWNERCOPY)`，落地之前触发 `before_open`
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-apfs --lib remove_tree && cargo test -p strata-apfs --doc && cargo test -p strata-apfs --features test-seams --test remove_tree --test clone_from --test clone --test roots`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-apfs/src/remove_tree.rs crates/strata-apfs/src/clone_from.rs crates/strata-apfs/src/lib.rs crates/strata-apfs/tests/remove_tree.rs crates/strata-apfs/tests/clone_from.rs crates/strata-apfs/tests/clone.rs crates/strata-apfs/tests/roots.rs
git commit -m "feat(apfs): remove_tree_rooted reports what it leaves for any entry kind, and clone_from_rooted checks its source kind"
```

### Task 6: `Attrs`、记录解析器、`attrs_of` 与 gen count 契约

**Files:**
- Create: `crates/strata-apfs/src/attrs.rs`（`Attrs`、`FileKind`、`unchanged_since`、`gen_count_known`、`last_changed`、`GEN_COUNT_UNAVAILABLE`、`attrs_of`）、`crates/strata-apfs/src/bulk.rs`（属性请求、记录解析器，Task 7 复用）
- Test: `crates/strata-apfs/tests/attrs.rs`、`crates/strata-apfs/tests/symlink_recipe.rs`；`bulk.rs` 内的单元测试

**Interfaces:**
- Consumes: Task 3 的 `resolve`、`check_root`；Task 1 的 `classify_io_error`；Task 2 的 `clone_file` 与 Task 4 的 `rename_rooted`（时间戳的测试用）；Task 4 的 `symlink_rooted` 与 Task 5 的 `clone_from_rooted`（符号链接配方的测试用）
- Produces: `Attrs`、`FileKind`、`Attrs::{unchanged_since, gen_count_known, last_changed}`、`GEN_COUNT_UNAVAILABLE`、`attrs_of`；crate 内部的 `parse_records(buf, n) -> Vec<Result<RawEntry>>` 与 `ERROR` 记录的 `fstatat` 补取函数

- [ ] **Step 1: 写失败的测试**：§7 的 `bulk_record_parser_follows_the_returned_bitmap`、`bulk_record_parser_rejects_bad_records`、`bulk_error_record_fallback_has_no_gen_count`、`bulk_record_maximum_fits_the_minimum_buffer`（都在 `bulk.rs` 里；前一个以外都不碰文件系统，`bulk_error_record_fallback_has_no_gen_count` 另用一个真实文件做 `fstatat` 补取）、`attrs_unchanged_since_rules`、`attrs_of_reports_every_field`、`attrs_of_does_not_follow_links`、`gen_count_changes_on_write_not_on_touch`、`gen_count_is_unknown_while_a_writable_mapping_exists`、`gen_count_unknown_with_creator_descriptor_still_open`、`gen_count_unknown_for_a_mapping_made_by_another_process`、`gen_count_unknown_for_a_read_only_mapping_of_a_writable_descriptor`、`read_only_and_private_mappings_keep_gen_count_known`、`gen_count_alone_is_not_a_version`、`directory_gen_count_tracks_entries_not_content`、`attrs_versions_differ_for_same_second_writes`、`same_size_write_with_restored_mtime_is_a_new_version`、`attrs_last_changed_is_the_later_of_mtime_and_ctime`、`symlink_recipe_recreates_a_workspace_symlink`（`tests/symlink_recipe.rs`）；并把 `attrs_of` 的行加进 Task 3 的 roots 表（`tests/roots.rs`）
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-apfs --features test-seams --test attrs --test symlink_recipe`
Expected: FAIL（`Attrs`、`attrs_of` 还没定义，编译不过）

- [ ] **Step 3: 写让测试通过的最少实现**：属性请求与记录布局照 §4.2 D，**按每条记录的返回位图解析**，用未对齐读；`GEN_COUNT` 要 `FSOPT_ATTR_CMN_EXTENDED`；位图里没有 `GEN_COUNT` 或值为 0 → `gen_count = 0`；`ERROR` 记录走 `fstatat` 补取，补取的 `Attrs` 里 `gen_count = 0`；`attrs_of` 用 `getattrlistat(FSOPT_NOFOLLOW)`；符号链接目标用 `readlinkat` 取原字节；`unchanged_since` 照 §5.5，未知永远是 `false`；`gen_count_known` 与 `last_changed`（`max(mtime, ctime)`）照 §5.5。多进程、映射的测试用 `fork` 或 `std::process::Command` 启动自身的子测试入口；映射的测试把描述符的打开方式（`O_RDWR` 或 `O_RDONLY`）写进断言，不只看保护位
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-apfs --lib bulk && cargo test -p strata-apfs --features test-seams --test attrs --test symlink_recipe --test roots`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-apfs/src/attrs.rs crates/strata-apfs/src/bulk.rs crates/strata-apfs/src/lib.rs crates/strata-apfs/tests/attrs.rs crates/strata-apfs/tests/symlink_recipe.rs crates/strata-apfs/tests/roots.rs
git commit -m "feat(apfs): Attrs, bulk record parser, attrs_of and the gen count contract"
```

### Task 7: `walk` 与 `walk_with`

**Files:**
- Create: `crates/strata-apfs/src/walk.rs`
- Test: `crates/strata-apfs/tests/walk.rs`

**Interfaces:**
- Consumes: Task 6 的 `bulk.rs`；Task 3 的 `resolve`、`check_root`
- Produces: `walk`、`walk_with`、`list_dir`、`WalkOptions`（手写 `Default`）、`Walk`（`Send`）

- [ ] **Step 1: 写失败的测试**：§7 的 `walk_lists_all_entries_relative_to_root`、`walk_excludes_configured_dirs`、`walk_excludes_several_dirs`、`walk_exclusion_of_a_missing_dir_is_a_noop`、`walk_exclusion_prunes_the_whole_subtree`、`walk_serves_full_and_incremental_scans`、`walk_is_preorder_with_siblings_sorted`、`walk_options_default_uses_the_default_buffer`、`walk_is_send_and_accepts_send_sync_filters`、`list_dir_lists_one_level_in_byte_order`、`walk_wide_directory_is_complete_and_stable_across_buffer_sizes`、`walk_lists_the_longest_name_with_the_minimum_buffer`、`walk_very_wide_directory_through_many_windows`、`walk_after_mass_deletion_lists_only_survivors`、`walk_returns_untouched_names_once_while_the_directory_changes`、`walk_never_follows_symlinks`、`walk_does_not_descend_into_mount_points`、`walk_reports_an_unreadable_directory_and_continues`、`walk_skips_entries_that_vanish_mid_walk`、`walk_never_follows_a_directory_swapped_for_a_link`、`walk_reports_a_swapped_root`、`walk_start_is_resolved_without_following_links`、`walk_names_are_nfc_and_openable_either_way`、`walk_depth_limit_is_an_error_item`、`walk_fits_a_low_descriptor_limit`、`low_descriptor_limit_child`（后者 `#[ignore]`，只由前者启动）；并把 `walk`、`walk_with`、`list_dir` 的行（不规范的 root 得第一项 `Err(InvalidInput)`）加进 Task 3 的 roots 表（`tests/roots.rs`），这张表到此齐全
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-apfs --features test-seams --test walk`
Expected: FAIL（`walk`、`walk_with` 还没定义，编译不过）

- [ ] **Step 3: 写让测试通过的最少实现**：栈式前序遍历，照 §5.6 的七步和 `read_dir` 的五步；一个目录读完、排序之后才产出（排序只在目录之内，整体不是全路径的字节序）；filter 在条目到手之后、产出与下探之前调用一次，类型是 `Fn + Send + Sync`，所以 `Walk` 是 `Send`；子目录用 `openat(O_DIRECTORY | O_NOFOLLOW)` 打开，`ENOENT`、`ENOTDIR`、`ELOOP` 当作它已经不在；挂载点、符号链接、特殊文件都是叶子；同时只持有栈上各 frame 的句柄；结束时对根路径 `lstat` 核对身份，变了产出 `Busy`（`root_changed`）；深度超限产出 `TooLarge`（`too_deep`）；`MAX_WALK_DEPTH` 与 `ERANGE` 翻倍重试照 §5.6；`WalkOptions` 手写 `Default`；`list_dir` 是 `walk_with` 在 `start = dir`、`max_depth = Some(1)`、全部放行下的具名写法，返回 `Walk<'static>`（放行用一个函数项，不借用调用方的东西）
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-apfs --features test-seams --test walk --test roots`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-apfs/src/walk.rs crates/strata-apfs/src/lib.rs crates/strata-apfs/tests/walk.rs crates/strata-apfs/tests/roots.rs
git commit -m "feat(apfs): getattrlistbulk walk with pruning, bounded descriptors and root pinning"
```

### Task 8: `case_fold_key` 与 `case_fold_eq`

**Files:**
- Create: `crates/strata-apfs/src/casefold.rs`
- Test: `crates/strata-apfs/tests/case.rs`；`casefold.rs` 内的单元测试

**Interfaces:**
- Consumes: Task 1 的 `tests/common` 的 `Image`；`unicode-normalization` 与折叠表（§9 待定）
- Produces: `case_fold_key`、`case_fold_eq`

- [ ] **Step 1: 写失败的测试**：§7 的 `case_fold_eq_known_pairs`（单元测试，表驱动）、`case_fold_key_agrees_with_case_fold_eq`（单元测试，固定种子的随机串）、`case_fold_key_of_a_path_is_the_slash_joined_keys`（单元测试，路径性质，固定的组合记号用例加固定种子的随机串）、`case_fold_eq_matches_a_case_insensitive_volume`（oracle，用镜像）、`case_sensitive_volume_distinguishes_case_but_not_normalization`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-apfs --lib casefold && cargo test -p strata-apfs --test case`
Expected: FAIL（`case_fold_key`、`case_fold_eq` 还没定义，编译不过）

- [ ] **Step 3: 写让测试通过的最少实现**：照 §5.8：`case_fold_key` 对纯 ASCII 走 `to_ascii_lowercase`，否则 `NFD(full_case_fold(NFD(s)))`；`case_fold_eq(a, b)` 就是 `a == b || case_fold_key(a) == case_fold_key(b)`（两边都是 ASCII 时用 `eq_ignore_ascii_case` 省掉分配）；折叠表选定后记入 §9 并核对 Unicode 版本 ≥ 16.0；oracle 测试的表按 §4.2 B 的类别建，再按码位区间随机抽样（固定种子，失败时打印那一对的码位），同时比较 `case_fold_eq` 与键的相等；路径性质（`case_fold_key(a + "/" + b)` 等于两边的键用 `/` 拼起来）由构造保证，不需要特别的代码，`case_fold_key_of_a_path_is_the_slash_joined_keys` 钉住它，§5.8 写明了理由
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-apfs --lib casefold && cargo test -p strata-apfs --test case`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-apfs/src/casefold.rs crates/strata-apfs/src/lib.rs crates/strata-apfs/tests/case.rs
git commit -m "feat(apfs): case_fold_key and case_fold_eq as Unicode canonical caseless matching, checked against a real volume"
```

### Task 9: 监听与 `flush`

**Files:**
- Create: `crates/strata-apfs/src/watch.rs`（流的建立、回调、`Watcher`、`flush`）、`crates/strata-apfs/src/watch/translate.rs`（flags 到 `WatchEvent` 的纯函数）
- Test: `crates/strata-apfs/tests/watch.rs`；`translate.rs` 内的单元测试

**Interfaces:**
- Consumes: Task 1 的 `volume_caps`、Task 8 的 `case_fold_eq`（排除过滤在大小写不敏感的卷上用它）；`strata_types::RepoPath`
- Produces: `watch`、`watch_with`、`WatchOptions`、`WatchEvent`、`Watcher`（`Send + Sync`）、`Watcher::flush`

- [ ] **Step 1: 写失败的测试**：§7 的 `watch_flags_map_to_events`（`translate.rs` 里的单元测试，合成 flags）、`watcher_reports_file_and_directory_changes`、`watcher_paths_use_the_callers_root_and_disk_spelling`、`watcher_excludes_configured_dirs`、`dropping_the_watcher_stops_callbacks`、`watcher_reports_a_renamed_root_as_dropped`、`watcher_reports_must_rescan`（卡住回调的洪泛，默认运行，几秒；一轮在所有文件都有了着落时结束，至多 30 s，重试至多 3 轮）、`flush_drains_the_callback_queue`、`watcher_reentry_from_the_callback_never_traps`、`watcher_reentry_child`（后者 `#[ignore]`，只由前者在子进程里启动）、`flush_is_not_a_fence`（`#[ignore]`，只打印数字）
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-apfs --lib translate && cargo test -p strata-apfs --test watch`
Expected: FAIL（`watch` 还没定义，编译不过）

- [ ] **Step 3: 写让测试通过的最少实现**：照 §5.7：`FSEventStreamCreate`（`FileEvents | WatchRoot | NoDefer`，latency 0.05，`SinceNow`）、私有串行队列；回调按翻译表出事件，路径前缀换回调用者的 root；一次回调一个 `Changed`；`on_event` 包在 `catch_unwind` 里，panic 后补发 `Dropped`，回调状态放在堆上，`info` 指向它，`retain`、`release`、`copyDescription` 全是 NULL，`Watcher` 是唯一所有者（`closed` 与闭包都在状态里）；回调入口置线程局部标志、返回或 panic 时清掉（控制者对评审 n5 的更正：在任何一个 Watcher 的回调里，对任何一个 Watcher 的 `flush` 都是无操作，drop 都走异步路径）；`drop`：标志为假时置 `closed`，`Stop`、`Invalidate`、`Release`，再 `dispatch_sync` 排空，然后释放状态；**在任何一个 Watcher 的回调里 drop**（标志为真）时不排空：置 `closed`，`Stop`、`Invalidate`，把最后的 `Release` 与状态的释放放进 `dispatch_async(queue, …)`，闭包的捕获物在那个块里 drop，恰好一次；不设 `IgnoreSelf`，不调 `FSEventStreamSetExclusionPaths`；`exclude` 在回调里按分量过滤。`flush`：标志为真时 `debug_assert!`（release 直接返回）；标志为假时取内部互斥量，`FSEventStreamFlushSync`，再对私有队列 `dispatch_sync` 一个空块；保证的只有"排空之前入队的回调都已执行完"，不返回"已追上"的信号。事件最多等 5 s，不断言延迟
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-apfs --lib translate && cargo test -p strata-apfs --test watch && cargo test --release -p strata-apfs --test watch watcher_reentry_from_the_callback_never_traps`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-apfs/src/watch.rs crates/strata-apfs/src/watch crates/strata-apfs/src/lib.rs crates/strata-apfs/tests/watch.rs
git commit -m "feat(apfs): FSEvents watcher with a best-effort flush that reports drops and never claims completeness"
```

### Task 10: 验收数字（design §8）

**Files:**
- Create: `crates/strata-apfs/tests/budget.rs`
- Modify: `docs/modules/apfs.md` §5.9 与 §9（把实测写回，待定行改成定案）

**Interfaces:**
- Consumes: Task 2、6、7 的 `clone_tree`、`attrs_of`、`walk_with`；Task 5 的 `remove_tree_rooted`
- Produces: 无新签名

- [ ] **Step 1: 写失败的测试**：§7 的 `walk_tool_sized_tree_within_the_reconcile_budget`、`clone_node_modules_sized_tree_within_the_budget`（都 `#[ignore]`，造合成树要几十秒）；同一个文件里再加一个只打印数字的 spike：`attrs_of` 与 `max_depth = Some(1)` 单目录列表的单次耗时，以及对 tool 加全部兄弟库（只读，路径由环境变量给出）在工作区所在的卷上并行度 1 到 4 克隆到同卷临时目录的总耗时（§5.9 的整个布局投影），再用 `remove_tree_rooted` 把克隆删掉并打印耗时
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
| `walk` 的形态 | [我的决定] 返回具名的 `Walk`，条目是 `Result<(RepoPath, Attrs)>`；另有 `walk_with`（`WalkOptions { start, max_depth, buffer_bytes }`，filter 同时看属性）；filter 是 `Fn + Send + Sync`，所以 `Walk` 是 `Send`；`WalkOptions` 手写 `Default` | 单个条目的失败不能吞；overlay 的增量要列单个目录；ignore 规则要区分目录与文件；小缓冲区是 3b、3c 多窗口续读的接缝；`&dyn Fn` 不带 `Sync` 就没法把一次扫描整个移到工作线程上（评审 M8）；derive 出来的 `Default` 给 `buffer_bytes = 0`，会被夹成 1 KiB 而不是 64 KiB（评审 M7） | 2026-10-22 |
| 遍历的顺序与读取方式 | [我的决定] 前序；每个目录读完、按 `RepoPath` 字节序排序之后再产出；整体**不是**全路径的字节序（`foo`、`foo/x`、`foo-bar`），文档写明，调用方收进 `BTreeMap` 再比（评审 M11） | 产出顺序确定；读的过程中目录变动不会让同一个名字出现两次（实测 §4.2 D）；3b 要求排序后顺序稳定 | 2026-10-22 |
| 挂载点与符号链接 | [我的决定] 挂载点产出但不进入；符号链接产出（带目标）但不跟随 | 裁定 T4；嵌套挂载点的克隆结果也是空占位，两边一致 | 2026-10-22 |
| 描述符与深度 | [我的决定] `MAX_WALK_DEPTH` 为 128（`remove_tree_rooted` 用同一个上限）；同时持有的描述符不超过深度加一 | launchd 的软限常为 256（3f 两行）；超限的子树以错误条目报告，不无声截断 | 2026-10-22 |
| 批量读的缓冲区 | [我的决定] 缺省 64 KiB，夹在 1024 到 4 MiB 之间，`ERANGE` 翻倍重试 | 实测 255 个 ASCII 字符的名字加属性装不进 300 字节，512 字节可以；下限 1024 的理由是最长的一条记录（765 字节的名字加 NUL 加全部属性）约 860 字节（§5.6，评审 M12） | 2026-10-22 |
| 目录项在读与打开之间消失或被换成非目录 | [我的决定] 跳过，不报错 | 与 `ENOENT` 同类，遍历本来就不是快照；报错会把正常的并发变动变成失败 | 2026-10-22 |
| `Attrs` 的形状 | [我的决定] 比计划多 `mode`（低 12 位）；`kind` 的类型叫 `FileKind`；`gen_count` 为 0 表示未知 | `chmod` 不改 gen count（§4.2 C）；不与 types.md 的 `EntryKind` 混淆 | 2026-10-22 |
| `attrs_of`、`Attrs::unchanged_since`、`volume_caps` | [我的决定] 新增 | `std::fs` 读不到 gen count；把"未知不等于未变、不能单比 gen count、版本的身份含 inode"固定成一个入口；store、session、daemon 的预检 | 2026-10-22 |
| `open_rooted` 的返回范围 | [我的决定] 普通文件、目录、FIFO（非阻塞）可以返回；符号链接、socket、设备 → `NotFile`；另有只收普通文件的 `open_rooted_file`；根自己用 `open_root` | store、session 要用 `open_rooted` 取目录句柄（所以与 tgrep 的 `RootedDir` 不同，目录不拒绝）；类型核对交给调用方或 `open_rooted_file` | 2026-10-22 |
| 根的解析 | [我的决定] 根的祖先可以是符号链接，根的最后一级 `O_NOFOLLOW`；根之下逐级 `O_DIRECTORY \| O_NOFOLLOW`；不用 `O_NOFOLLOW_ANY` | `O_NOFOLLOW_ANY` 连 `/tmp/x` 也拒绝（§4.2 E） | 2026-10-22 |
| 目的地变体 | [我的决定] `clone_file_rooted`（`mode: Option<u32>`，屏蔽到 `0o777`，**经句柄**设置：克隆之后取回 dst，交给 `set_file_meta`）、`clone_tree_rooted`、`create_file_rooted`、`mkdir_rooted`（`parents: bool`）、`symlink_rooted`；父目录缺失不自动建；不 `fsync` | 增补 3 只规定了机制，名字与参数是我定的。`mkdir_rooted(parents)` 单独提供，使每个函数只做一件事、失败边界清楚；`mode` 屏蔽 setuid、setgid 是为了不意外造出特殊位文件。mode 经句柄而不是 `fchmodat(名字)`：克隆之后 dst 的名字可能被换成指向根外文件的硬链接，按名字改 mode 会改到那个文件（§4.2 H） | 2026-10-22 |
| 克隆带来的 BSD 标志与扩展属性 | [我的决定] apfs 的克隆原样带过来，不清；清标志、设 mode、设 mtime 经 `set_file_meta`（`clear_flags` 清除 `UF_COMPRESSED` 之外的全部 `UF_*`，`SF_*` 从不清）；扩展属性的剥离留在 store，在同一个句柄上、两次 `set_file_meta` 之间 | 清不清、按什么规则清是 store.md §5.3 第 5 步和 overlay 投影的规整，规则不该在两处各写一份；但改标志、mode、mtime 的系统调用要有一份实现，而且要核对 `st_nlink`（第 2 轮裁定 "Metadata changes through a handle"）。已知的坑：`clone_file_rooted(.., Some(m))` 遇到带 `uchg` 的源会 `EPERM`（实测），要先清标志的调用方传 `None`，再对句柄 `set_file_meta` | 2026-10-22 |
| 根之下的改名与删除的接口 | [我的决定]（名字由增补 8 定，签名与细节是我的）`rename_rooted(root, from, to, RenameMode::{Excl, Swap})`、`remove_rooted(root, path, RemoveKind::{File, EmptyDir})`、`remove_tree_rooted(root, path) -> Result<RemoveReport>`（任何种类的条目都可以是目标）；另有 `make_removable(&File)`、`set_file_meta(&File, &FileMeta)`（下面各有一行）；与初稿的名字对应：`Exclusive` → `Excl`，`unlink_rooted` → `remove_rooted(.., File)`，`rmdir_rooted` → `remove_rooted(.., EmptyDir)`；没有覆盖的一档 | overlay 的 restore 与 `project`、store 的 `retire_dir` 与 gc、deps 的封存与 staging、session 的交换与 `rm`、gitio 的 `export_tree` 都要"根之下不跟随符号链接的改名与删除"（增补 3、8）；三份手写的递归删除碰上指向根外的符号链接就是数据丢失的风险（评审 I1）。覆盖违反 A1（design §9 不变量 5）。增补 8 取消了初稿"apfs 不做递归删除"的决定 | 2026-10-22 |
| `make_removable` | [控制者裁定；签名与细节是我的] `make_removable(file: &File) -> Result<()>`：句柄由调用方用 `open_rooted` 取得；**先**清 `UF_IMMUTABLE`、`UF_APPEND`、**再** `fchmod` 补 u+w；只在需要时才调系统调用；非目录而 `st_nlink > 1` → `ReadOnly`（`hard_link`）；`SF_IMMUTABLE`、`SF_APPEND` → `ReadOnly`（`immutable`），从不清；标志清了而补写位失败 → `Unknown` | 取代 store 的 `retire_dir` 与 gc 各自写的"`fchmod u+w` 加 `fchflags`"配方。次序不能反：带 `uchg` 的目录 `fchmod` 得 `EPERM`（实测，§4.2 H）。只在需要时才调：无操作的 `chmod`、`chflags` 也更新 `ctime`（实测），而 gc 的年龄是 `max(mtime, ctime)`，所以 gc 先判年龄、后调用。A10 随之改写：apfs 只在 `remove_tree_rooted` 删它点名的树的过程中、和调用方调用 `make_removable`、`set_file_meta` 的时候放宽权限或改元数据 | 2026-10-22 |
| `set_file_meta` 与 `FileMeta` | [控制者裁定；字段与次序的细节是我的] `set_file_meta(file: &File, meta: &FileMeta) -> Result<()>`，`FileMeta { clear_flags: bool, mode: Option<u32>, mtime: Option<SystemTime> }`：先 `fstat` 句柄，要求普通文件或目录（别的 → `NotFile`）、非目录时 `st_nlink == 1`（否则 `ReadOnly`，`hard_link`）、没有挡路的 `SF_*`（否则 `ReadOnly`，`immutable`），任何一项不满足什么都不改；然后按清标志、设 mode、设 mtime 的次序，每一项只在需要时才调；`UF_COMPRESSED` 保留；第一项改动之后的失败 → `Unknown`；store 的 `get_blob`、`get_blob_rooted`、`put` 的临时克隆、overlay 的 restore、`project` 与 deps 封存根目录（0555）都经它，不自己调 `fchflags`、`fchmod`、`futimens`，目录的句柄也一样 | store 的 writer 复现过：`get` 的克隆 dst 被换成指向根外 0600 文件的硬链接，`open(O_NOFOLLOW)` 照常打开它，`fchmod 0644` 把根外的文件改成了 0644。`fchflags`、`fchmod`、`futimens` 改的是 inode，不是名字，核对 `st_nlink` 才拦得住。范围（裁定 "set_file_meta scope, corrected"）：这条规则管其他进程够得着的文件（会话树、staging、基线 generation、工作区）；store 私有区域（`store/blobs`、`store/tmp`）里增补 8 的 libc 允许仍然成立，`put` 经它是选择，不是强制。目录的句柄（裁定 "set_file_meta accepts a directory handle"，取代原先的"目录 → `IsDirectory`"）：APFS 拒绝对目录 `link(2)`（`EPERM`，§4.2 H），目录只有一个名字，没有硬链接的暴露，所以只对非目录核对 `st_nlink`；封存要把根目录设成 0555，不经它就得让 deps 自己调 `fchmod`，与"没有模块自己调这三个系统调用"冲突 | 2026-10-22 |
| 共用的 inode 上不动标志与 mode | [控制者裁定] 非目录而 `st_nlink > 1`：`remove_tree_rooted` 留下并记 `stuck(Immutable, detail = "hard_link")`；`make_removable`、`set_file_meta` 拒绝（`ReadOnly`，`hard_link`）；取到句柄之后再 `fstat` 一次 `st_nlink`；目录不适用（APFS 拒绝对目录 `link(2)`，`EPERM`，§4.2 H；`st_nlink` 随子目录数增长，不能用来判共用；目录的句柄经 `set_file_meta` 照常改） | `fchflags`、`fchmod` 改的是 inode：清一个名字上的 `uchg` 也清了别的名字上的，其中可能有树之外的（agent 能给同一用户的任何文件建硬链接，比如主 checkout 里的文件），违反 A10"树之外的东西一概不碰"。代价：一个带标志的硬链接留在 trash 里，由人处理（`stuck` 里写明 `hard_link`）。没有标志拦着的硬链接照常 `unlinkat`，只删这个名字 | 2026-10-22 |
| `remove_tree_rooted` 的目标种类 | [控制者裁定] 目标可以是目录、文件或符号链接（从不跟随）；非目录的目标直接走叶子的删除（清标志只在不共用的 inode 上），报告的形状不变 | store 的 gc 用这一个调用删 tmp 与 trash 里的每个条目，调用方因此不需要在 apfs 之外按路径 `chflags`、`lchflags`（增补 8 + A10）。代价：目标种类不再是拒绝的理由，调用方传错路径（本想删一棵树却给了一个文件）不会被拦下；`RemoveReport.removed == 1` 看得出 | 2026-10-22 |
| 同一路径的改名 | [我的决定] RepoPath 完全相同 → `InvalidInput`（`same_path`）；只差拼写的交给内核 | 内核对同一条目的 EXCL、SWAP 都返回成功而什么都不做（实测），调用方的 bug 会被悄悄吞掉；大小写不敏感的卷上只差大小写的 EXCL 改名是有用的（改拼写），所以不一并拦 | 2026-10-22 |
| `RENAME_NOFOLLOW_ANY`、`RENAME_RESOLVE_BENEATH` | [我的决定] 不用，逐级钉住父目录 | 实测两个 flag 对 from 与 to 都生效（§4.2 G），用 dirfd 加相对路径一次系统调用就能约束两边，是可行的优化。不用是因为：逐级打开才分得出"哪一级是符号链接"（`reason = symlink`，对照内核只给 `ELOOP`），测试接缝能在各级之间触发（`rooted_destination_swap_after_resolution_stays_inside` 一类），这两个 flag 在比 macOS 27 更老的系统上是否可用没测过。以后若 spike 显示逐级 `openat` 太慢，可以换，契约不变 | 2026-10-22 |
| 改名与删除被拒绝时的 `detail.reason` | [我的决定] `reparent_needs_write`、`immutable`、`no_permission`、`read_only_volume`（都是 `ReadOnly`）；`make_removable`、`set_file_meta` 另有 `hard_link` | 同一个 `EACCES`、`EPERM` 对调用方的处理不同：前者（0555 的目录改父级）补写位即可，后者（标志）要先清标志，其余是真的没权限；`make_removable` 两样都做，次序固定（先清标志）。多一次 `fstatat` 换来可执行的诊断。apfs 自己不放宽权限（A10） | 2026-10-22 |
| `remove_rooted(EmptyDir)` 遇挂载点 | [控制者裁定] `CrossVolume`（`reason = mount_point`），在调内核之前用 `fstatat` 的 `st_dev` 与父目录比较判出，`rmdir` 晚到的 `EBUSY` 同样映射；`remove_tree_rooted` 的目标是挂载点时遍历之前就是同一个码 | `rmdir` 的 `EBUSY` 什么都没改，`Unknown`（保留现场、不重放）是错的归类。`CrossVolume` 的含义因此宽于 types.md §6.2（给 T15 的记录） | 2026-10-22 |
| `full_fsync` | [我的决定] 提供这个包装，不做"改名后自动同步" | 增补 3 要求扛断电的目录改名用 `F_FULLFSYNC`：包装一次，各模块不再各写各的常量与 `EINTR` 处理；什么时候同步（成批改名、最后一次）归调用方，实测文件约 10 ms、目录 4 到 26 ms，每次都做会拖慢成批操作 | 2026-10-22 |
| 年龄的 helper | [我的决定] `Attrs::last_changed()` = `max(mtime, ctime)` | 增补 4 的 gc guards：clonefile 出来的文件和空目录带来源的 mtime，改名只更新 ctime（实测）；放在 `Attrs` 上，gc 对文件和目录都能用 `attrs_of(..).last_changed()` | 2026-10-22 |
| `gen_count` 的"不可得"值 | [我的决定] 保持 `u64`，0 是 `GEN_COUNT_UNAVAILABLE`，另有 `gen_count_known()` | 计划里 `Attrs` 的 `gen_count` 就是 `u64`；`Option<NonZeroU64>` 更显式，若 overlay 要求，调用方各改一行即可 | 2026-10-22 |
| 单目录列表 | [我的决定] 加具名的 `list_dir(root, Option<&RepoPath>) -> Walk<'static>` | overlay 在大小写不敏感的卷上找磁盘拼写要列单个目录；`walk_with` 已经能做，具名写法避免每个调用方各拼一遍选项，并保证不借用局部变量 | 2026-10-22 |
| 测试接缝的 step 编号 | [我的决定] 每次公开调用从 0 起，落地的那一次调用也算一步；`rename_rooted` 先 from 后 to；`before_meta` 的序号另算（每次 `make_removable` 或 `set_file_meta` 调用从 0 起，只数实际要做的改动） | `rooted_destination_swap_after_resolution_stays_inside` 要在"解析完、落地前"换目录，初稿的定义覆盖不到落地那一步 | 2026-10-22 |
| 挂载镜像的测试放在哪里 | [我的决定] 测试根建在内部卷的临时目录 | 实测外置盘 `/Volumes/GWD` 的子目录上 `hdiutil attach -mountpoint` 得 `Permission denied`，内部卷的临时目录可以 | 2026-10-22 |
| `detail.reason` 词表 | [我的决定] §3 的"`detail` 的形状"里列出的那一串 | types.md 只规定了 `errno` 与 `path` | 2026-10-22 |
| 监听的绑定方式 | [我的决定] 直接绑 CoreServices（`FSEventStreamCreate` 加私有串行 dispatch queue），不经 `notify` | 要拿到原始 flags，自己决定 `Dropped` 与 `MustRescan` 的归类 | 2026-10-22 |
| `remove_tree_rooted` 的返回形状 | [控制者裁定，第 2 轮；原为我的决定] `Err` 只给遍历之前、什么都没碰的拒绝（目标缺失、某一级父目录是符号链接或文件、目标是挂载点 → `CrossVolume`（`mount_point`）、父目录不可写或带不可变标志）；遍历一开始就算开始（清标志、补权限也是可见的写），之后一律 `Ok(RemoveReport)`：没删干净的条目在 `stuck` 里带原因，`is_complete()` 回答删干净了没有；`#[must_use]`；没有 `to_detail`、`from_error` | 初稿照字面读增补 8，没删干净就是 `Err(Unknown)`、`detail` 里带报告，并请 lead 确认；第 2 轮裁定改成现在这样（"remove_tree_rooted reports, it does not fail"）：部分删除的结局是确定的，store 的 trash gc 每一轮重试剩下的，types.md 的 `Unknown`（保留现场、不重放）会禁止这种重试。代价是调用方可能忘了看报告：`#[must_use]` 与 `is_complete()` 把它变成显眼的一步 | 2026-10-22 |
| 清标志与补权限的办法 | [我的决定] 只用句柄版本：`openat` 取句柄（符号链接用 `O_SYMLINK`；属主有执行位而没有读位的条目 `O_RDONLY` 得 `EACCES`，就用 `O_EXEC`）、核对 (dev, inode)、`fchflags`；打不开的目录先走 `O_EXEC` 句柄清标志、`fchmod` 补 `0700`、`O_RDONLY` 重开；属主连搜索位都没有、又不带 `UF_IMMUTABLE`、`UF_APPEND`、`SF_IMMUTABLE`、`SF_APPEND` 的目录才用按名字的 `fchmodat(AT_SYMLINK_NOFOLLOW)` 补 `0700`（ACL 拦着的、带标志的目录 chmod 帮不上忙，不去试，评审 M1）；打得开而写不进的用 `fchmod`；只清 `UF_IMMUTABLE`、`UF_APPEND`，不动别的标志位；`SF_*` 清不掉，进 `stuck`；非目录而 `st_nlink > 1` 的 inode 不清（见"共用的 inode"一行） | `fchflagsat` 要 macOS 27.0，libc 没导出，design 的下限是 macOS 26（§4.2 H）。`O_EXEC` 是第 2 轮的发现：它把按名字改 mode 的范围缩到"属主没有搜索位"的目录，也把无解的角落缩到"既带 `uchg` 又没有读位也没有搜索位"的条目（文件、目录都一样，代价是极少见的 `Immutable`）。`O_EXEC` 只在 macOS 27.0.1 上量过（见"平台范围"一行） | 2026-10-22 |
| 按名字的 `fchmodat` 的残余风险 | [我的决定] 保留，只用于属主既打不开也取不到 `O_EXEC` 句柄、没有搜索位、又不带 `UF_IMMUTABLE`、`UF_APPEND`、`SF_IMMUTABLE`、`SF_APPEND` 的目录（mode 000 一类；ACL 拦着的、带标志的目录 chmod 解不了，不去试，评审 M1）；文档写明交换竞争下的暴露，并说明前提是被删的树通常没有并发写者（惯例，不是保证） | 这是唯一不经句柄改 mode 的地方：`fstatat` 判出是属主的目录之后、`fchmodat` 之前，别的进程可以把名字换成指向根外文件的硬链接，实测 `fchmodat` 改的是那个 inode（0644 变 0700）。去掉它就删不了 000 的树（`rm -rf` 做得到的事 apfs 做不到）；限度：只改调用者自己拥有的 inode，之后立刻重开并核对 (dev, inode)，store 的 trash 与 tmp 里已退役的东西通常没有并发写者——"通常"：cwd 在旧工作区里的 shell 会跟着旧树进 trash 继续写（session.md §6.5），要利用这个暴露还得有人在树里放一个指向根外文件的硬链接。（裁定 N4 要求"state the residual exposure"；第 3 轮 M1 收窄了门并改了"通常"） | 2026-10-22 |
| `remove_tree_rooted` 的父目录预检 | [我的决定] 开始之前检查父目录：标志含 `UF_IMMUTABLE`、`UF_APPEND`、`SF_IMMUTABLE`、`SF_APPEND`，或 `faccessat(P, ".", W_OK\|X_OK, AT_EACCESS)` 失败 → `ReadOnly` | 最后要 `AT_REMOVEDIR` 目标本身，需要 P 可写；先查，免得树删空了才发现根删不掉。`faccessat` 对 `UF_APPEND` 的目录说"可写"，所以标志要另看（§4.2 H） | 2026-10-22 |
| `RemoveReport` 的体量 | [我的决定] `stuck` 最多 `MAX_REPORTED_STUCK`（1000）条，总数在 `stuck_total` | 一棵全是受保护条目的大树不该把错误的 `detail` 撑到几十 MB（RPC 帧有上限，types.md §6.2 的 `TooLarge`） | 2026-10-22 |
| `Stuck.detail`、`StuckReason::UnknownOutcome`、`StuckReason::as_str` | [我的决定] `Stuck { path, reason, errno, detail: Option<String> }`；`detail` 只有 `Immutable` 带：`\|` 连接的标志名，或 `hard_link`；`UnknownOutcome` 给 `EIO`、`ETIMEDOUT`、`ENXIO`、`ENODEV`、`EDEVERR` 一类 unlink 结果不确定的 errno，其余 errno 是 `Io`；`as_str` 给蛇形名字 | 一张原因表（§5.4）说清每种情形（评审 N3）：文件标志拦着就是 `Immutable`，标志写在 `detail` 里，不让调用方去猜；`NoPermission` 只给 mode 与 ACL。`UnknownOutcome` 与 `Io` 分开：前者的条目可能已经没了，也可能还在，store 的 gc 不能假定；名字取 `unknown_outcome`，与 store.md 一致 | 2026-10-22 |
| `clone_from_rooted` 的形状 | [我的决定]（名字与参数由增补 8 定，`kind` 由第 2 轮裁定加）`clone_from_rooted(src_root, src: &RepoPath, dst: &Path, kind: CloneKind)`，`CloneKind::{File, Dir}`：源经 `openat(O_NOFOLLOW)` 链取得句柄，`fstat` 核对种类（不符就在克隆之前拒绝），`fclonefileat` 从句柄克隆；符号链接源 → `NotFile`（`File`）或 `NotDirectory`（`Dir`）；dst 是任意路径，调用方保证它是 strata 自己的目录 | 评审 I2：`clone_file` 的 `CLONE_NOFOLLOW` 只管最后一级，中间的符号链接照样被跟随，被换成符号链接的目录会把根外的文件克隆进 blob（实测，§4.2 A）。`kind`：调用方的种类来自之前的 `attrs_of`，到打开之间路径可能被换成目录，`fclonefileat` 就把整棵树克隆进 `store/tmp`（store 第 2 轮评审发现的），FIFO 会得到误导的 `EINVAL`。符号链接源拒绝：捕获的是内容，符号链接的"内容"是目标，调用方重建它，从不克隆（见"符号链接的重建配方"一行）；dst 若指向工作区里面应走 `clone_*_rooted` | 2026-10-22 |
| 符号链接的重建配方 | [控制者裁定] 来自工作区的符号链接：`attrs_of(src_root, p)` 取目标（`FileKind::Symlink` 里的原始字节），`symlink_rooted(dst_root, q, target)` 重建；从不克隆 | 经 `clone_file_rooted` 按路径克隆，src 一侧中间的符号链接内核照常跟随（`CLONE_NOFOLLOW` 只管最后一级，实测，§4.2 A）；经 `clone_from_rooted` 则符号链接被拒绝。重建只读目标字节，不读目标指向的任何地方。session 的携带与种子、overlay 的捕获（它本来就从 `attrs_of` 读目标）都用它 | 2026-10-22 |
| 根的 dirfd 访问器 | [我的决定] `open_root(root) -> Result<File>` | 增补 8 要一个访问器；`open_rooted` 取不到根自己（`RepoPath` 不能为空）。调用方对根句柄 `full_fsync`（根目录改名之后同步根自己），或当 dirfd 用 | 2026-10-22 |
| 规范的根 | [控制者裁定；检查的做法是我的] 每个以 root 为参数的函数在碰文件系统之前按原始字节检查：空串、不是绝对路径、以 `/` 结尾、或含恰好是 `.` 或 `..` 的分量（`.repos`、`.staging`、`..x` 这样的名字是合法的）→ `InvalidInput`（`root_not_canonical`）；`watch` 不走这一套（它对 root 做 realpath），只有"必须是绝对路径"这一条相同 | `open("link/")` 即使带 `O_DIRECTORY \| O_NOFOLLOW` 也会跟随符号链接（实测，§4.2 E），根的符号链接拒绝和根的钉住都会落空。用原始字节而不是 `Path::components()`：后者把结尾的 `/` 与 `.` 吃掉了。"不是绝对路径"是评审 r2 M4 补的：相对的 root 原先能通过检查，而 `watch` 与 config.md 的 V2 都要求绝对路径。config.md 的 V2 在加载时已经拒绝这样的路径文本；符号链接的仓库路径由裁定 "Repo paths in config are realpaths" 挡在加载处（给 T15 的记录） | 2026-10-22 |
| 公开的 errno 归类 | [我的决定] `classify_io_error(err, IoSide::{Read, Write, AfterWrite}, path)`，表见 §4.2 I | 增补 8 要求公开一个归类函数（名字由 apfs 定）：别的模块对 apfs 给的句柄做 `write_all`、`read` 失败时用同一张表，不再各写一份。`AfterWrite` 是 types.md §6.3 末段（已改一部分的失败一律 `Unknown`）的显式入口。纯函数：要靠 `fstatat` 才分得清的 `EPERM`（目录还是标志）由各个操作自己先处理。两个调用方用法（评审 N8）：`nearest_existing_ancestor` 返回的 `io::Error` 用 `IoSide::Read`；`create_file_rooted` 返回的句柄上 `write_all` 失败用 `AfterWrite`（文件已经建出来了，不论 errno 都是 `Unknown`） | 2026-10-22 |
| 最近的已存在上级 | [我的决定] `nearest_existing_ancestor(path) -> io::Result<PathBuf>`，不做"会爬的 `same_volume`" | 评审 M17：store.md、deps.md、session.md 各写了一遍 config.md §4.3 的"爬到最近的已存在上级再比较"。爬与比是两个小函数，比一个会爬的 `same_volume` 好测，`same_volume` 的"路径不存在 → `NotFound`"也不变。三个模块在下一轮或 T15 改成调它。它返回的 `io::Error` 由调用方交给 `classify_io_error(&e, IoSide::Read, path)`（评审 N8） | 2026-10-22 |
| `root_changed`、`too_deep` 的码 | [我的决定]（增补 8，评审 I4）`root_changed` → `Busy`（`reason = root_changed`），`too_deep` → `TooLarge`（`reason = too_deep`），不再用 `InvalidInput` | 两者都不是参数格式错。`Busy` 与 overlay 把同一个竞争报成 `Busy`（`workspace_changed`）一致；`TooLarge` 是"超出上限"。两个码的含义都宽于 types.md §6.2 的原意（`Busy` 写的是 repo 写锁，`TooLarge` 写的是文件阈值与 RPC 帧）。**给 T15**：types.md §6.2 为这两个码各加一句"何时返回"，"谁返回"各加 `apfs` | 2026-10-22 |
| `flush` 在回调里被调用 | [控制者更正，2026-10-23] debug 构建 `debug_assert!`，release 构建立即返回、什么也不做；"在回调里"按线程判（任何一个 strata `Watcher` 的回调期间，线程局部标志为真），对**任何一个** Watcher 的 `flush` 都走这条无操作路径 | 自己的回调里 `dispatch_sync` 到自己的队列会 `SIGTRAP`（实测，§4.2 F）。按 Watcher 精确判断会让 A 的回调排空 B、同时 B 的回调排空 A，两条队列互相等，死锁。单向排空是安全的（探针 `specific`），互相等不是，所以回调里不做同步等待 | 2026-10-23 |
| `drop(Watcher)` 在回调里 | [控制者更正，2026-10-23] 与 `flush` 同一个线程局部判断：在任何一个 Watcher 的回调里 drop 任何一个 Watcher 都不排空，置 `closed`，`Stop`、`Invalidate`，最后的 `Release` 与状态的释放交给 `dispatch_async`；状态的所有权见下一行 | 自己的队列上 `dispatch_sync` 会 `SIGTRAP`；两个回调互相排空会死锁（§4.2 F）。异步路径让闭包在当前回调返回之后恰好 drop 一次 | 2026-10-23 |
| 回调状态的所有者 | [控制者裁定，第 3 轮（module-rulings "Watcher state has one owner"）；落实的做法是我的] `Watcher` 是回调状态的唯一所有者：`FSEventStreamContext` 的 `retain`、`release`、`copyDescription` 全是 NULL；`closed` 与 `on_event` 闭包放在 `info` 指向的堆上状态里，不放在 `Watcher` 值里；状态在 `Stop`、`Invalidate`、`Release` 和排空流的队列之后恰好释放一次（回调里 drop 的，由排在当前回调后面的 `dispatch_async` 块释放）；测试断言闭包的 `Drop` 计数恰好为 1 | 评审 r2 M3：FSEvents 在最后一次 `Release` 之后还让流活着，`context.release` 到得比 `Watcher` 释放状态更晚（普通 drop 20/20 轮，回调里 drop 前后不定，§4.2 F），今天只因为没人再碰 `info` 才没出事；两个所有者会释放两次。这个形状也让闭包的捕获物在固定的时点 drop：overlay 与 daemon 可以依赖闭包里的 channel 在 `Watcher` drop 时断开 | 2026-10-23 |
| `Watcher` 的线程安全 | [我的决定] `Send + Sync`，`flush` 在内部互斥量下串行 | overlay 的 `Tracker` 持有它，checkpoint 可能在任何线程上调 `flush`；`FlushSync` 的线程安全文档没有说，保守起见自己串行（评审 M8） | 2026-10-22 |
| 进程内类型不 derive serde | [我的决定] `Attrs`、`WatchEvent`、`FileKind`、`RemoveReport`、`Stuck`、`FileMeta` 等都不 derive `Serialize`/`Deserialize` | types.md §2.2 的 serde 规则管 types.md 自己的类型；这些类型不经 RPC 或 JSON。第 1 轮曾让 `RemoveReport` 经 `to_detail`、`from_error` 成对手写进 `detail`（评审 M5）；第 2 轮 `remove_tree_rooted` 不再有 `Err` 形态的报告，这一对随之取消。调用方要把报告写进自己的日志或 `detail` 时用 `StuckReason::as_str` 的词表 | 2026-10-22 |
| `case_fold_key` | [我的决定] `case_fold_key(s: &str) -> String` 是 D145 的键；`case_fold_eq(a, b)` = `a == b \|\| key(a) == key(b)`；键只在内存里 | 增补 7：折叠成员判断用键，不做两两比较。键不是 NFC（不是 `RepoPath`），折叠表升级会改变键，所以不落盘、不进 RPC、不当路径用 | 2026-10-22 |
| 大小写折叠的两个问题 | [我的决定] 磁盘身份（看卷：`case_sensitive = false` 才用 `case_fold_eq` 与 `case_fold_key`）和 git 身份（看 `core.ignorecase`：`is_tracked`、ignore 匹配，gitio 只折叠 ASCII）分开写，并列出各自管哪些问题（裁定 "Disk identity follows the volume, git identity follows core.ignorecase"，§4.2 B） | 评审 M4 与增补 2、7：gitio 现在可以依赖 apfs，但对 git 身份仍按 ASCII 折叠，`git status` 就是这样判定的；3f:155 写的 `apfs::case_fold_eq` 对 gitio 不适用，由 T15 统一 | 2026-10-22 |
| `watcher_reports_must_rescan` 怎样足够可靠 | [我的决定] 卡住回调（第一次被调用时阻塞在门上），4 个线程创建 2 万个文件；**一轮在这一轮的所有文件都有了着落时结束**（至多 30 s），不是"1 s 内不再有回调"；没触发 `Dropped` 就再试，至多 3 轮（再用 4 万个）；"不静默丢失"的不变式在每轮都断言 | 评审 M13：初稿"6 万次创建能触发 1 到 21 个"靠运气，所以是 `#[ignore]`。实测卡住回调之后 5,000 到 6 万个文件的 21 轮运行每轮都出现 `UserDropped`；不卡住、4 万个文件也出现了 40 个（每秒约 5 万次创建超过了回调的消化速度）。第 2 轮评审者复现：18 轮里 16 轮出现丢弃，没丢的两轮都是进程里的第一条流（一轮被"1 s 静默"条件提前结束，另一轮 3 s 的窗口里收到了全部 20,017 个事件）。所以卡住只是大大提高概率，可靠性来自重试和以着落为结束条件，单轮不保证（初稿的措辞已改）。2 万个文件创建约 0.4 s，排空 1 到 4 s，默认运行可以接受 | 2026-10-22 |
| 给 T15 的记录 | [我的决定] 本文不改别的文档，记在这里 | （1）types.md §6.2：`NotText` 的"谁返回"加 `apfs`（`walk` 以 `Err` 项产出；APFS 上建不出非 UTF-8 的名字，只有解析器的单元测试到得了，评审 M6）；`Busy`、`TooLarge` 各加一句（见上）；`CrossVolume` 的"谁返回"已含 `apfs`，`clone_from_rooted`、`rename_rooted` 同属。（2）types.md §6.3："255 字节"改"255 个 UTF-16 码元"，"超过 1024"改"达到 1024 字节"（§4.2 B）；表里加 `EINVAL` → `InvalidInput`。（3）types.md §8：layer-1 一行改为"同层只允许 store 与 gitio 依赖 apfs"（增补 7）。（4）3f:155 与 types.md:124 写的 `apfs::case_fold_eq` 对 gitio 不适用（git 身份折叠 ASCII，增补 2）。（5）计划写的"`clonefile` 退化成复制"不对：跨卷是 `EXDEV`（§4.2 A）。（6）store.md、deps.md、session.md："最近的已存在上级"循环换成 `nearest_existing_ancestor`；删除、改名、从工作区捕获换成 `remove_tree_rooted`、`rename_rooted`、`clone_from_rooted`（增补 8）。（7）types.md §6.2：`NotEmpty` 的"谁返回"加 `apfs`，含义放宽成"要删的目录非空"（`remove_rooted(.., EmptyDir)` 返回它，裁定 "NotEmpty from apfs"）；overlay 把它当写竞争，store 的 gc 清扫跳过。（8）types.md §6.2：`CrossVolume` 也用于"目标本身是挂载点"的拒绝（`remove_rooted(.., EmptyDir)`、`remove_tree_rooted`，`reason = mount_point`），含义宽于"src 与 dst 不在同一个卷"，加一句。（9）config.md：V2 已经拒绝不规范的路径文本（相对路径、`.`、`..`、`//`、尾部 `/`），不用再加；缺的是符号链接，已由裁定 "Repo paths in config are realpaths"（03:52Z）交给 T15：仓库的 `path` 必须存在且等于它的 realpath，config 加载时拒绝别的写法并在错误的 `detail` 里写出 realpath；session 与 deps 把 `path` 原样传给 apfs（`open_root` 拒绝最后一级是符号链接的根）。apfs 对不规范的 root 仍一律 `InvalidInput`（`root_not_canonical`：空串、不是绝对路径、尾部 `/`、`.`、`..` 分量），不指望 config 先挡。（10）§2 的调用方表按各调用方 2026-10-22 的草稿和 module-rulings 写成，T15 按最终稿核对：只有 store 用 `remove_tree_rooted`；deps 的 `adopt` 用 `clone_from_rooted(.., Dir)`；session 的携带用 `clone_from_rooted` 加符号链接重建，不经 store 的 blob 库；gitio 进程内读文件走 `open_rooted_file`（裁定 "gitio reads files through apfs"，gitio 下一次修改时落实） | 2026-10-22 |
| 监听的绑定 crate | 手写 `extern "C"`（CoreServices、CoreFoundation、libdispatch） | 不引入依赖；拿到原始 flags 与 C 字符串路径；回调在自己的串行 queue 上，`drop` 用 `dispatch_sync` 排空（回调里 `drop` 改走 `dispatch_async` 收尾） | 2026-10-23 |
| 监听流的参数 | [我的决定] `FileEvents \| WatchRoot \| NoDefer`，latency 0.05 s，`SinceNow`；不设 `IgnoreSelf`；不用 `FSEventStreamSetExclusionPaths` | strata 自己的写入（restore、移动工作区）也要看见；排除 API 挡不住根上的 `Dropped`，还最多 8 个；latency 三档对丢事件没有可见影响（§4.2 F） | 2026-10-22 |
| `watch_with` 与 `exclude` | [我的决定] `watch(root, on_event)` 的签名保持；另加 `watch_with(root, &WatchOptions { exclude: Vec<RepoPath> }, on_event)`，落在 `exclude` 里的目录之下的事件在回调里丢掉，不产生 `Changed`；`Dropped` 不被过滤；排除按分量比较，大小写不敏感的卷上按 `case_fold_eq` | overlay 不关心 `deps_dirs`、`seed_caches`、`.git` 之下的事件，它们很吵。`FSEventStreamSetExclusionPaths` 最多 8 个路径，而且挡不住根上的 `Dropped`（§4.2 F），所以自己在回调里按分量过滤，没有个数限制。往被排除的目录里洪泛写入照样会让整个流报 `Dropped`，消费者对 `Dropped` 的全量重扫也要排除它们 | 2026-10-22 |
| 事件到 `WatchEvent` 的映射 | [我的决定] 见 §5.7 的表；路径前缀换回调用者传入的 root；一次回调合成一个 `Changed`；回调 panic → 截住并补发 `Dropped` | 消费者对自己持有的路径做 `strip_prefix`（FSEvents 给的是 realpath）；不因回调的 bug 静默丢事件 | 2026-10-22 |
| 监听的屏障与 `flush` | [我的决定]（增补 7 经评审修订、增补 8）不提供 `fence`；提供 `Watcher::flush()` = `FSEventStreamFlushSync` 加对私有队列的 `dispatch_sync`，保证的只有"排空之前入队的回调都已执行完"（评审 N1；不是"返回时没有回调在执行"），文档写明它只是尽力缩小窗口，并给出测量 | 四种候选屏障都做不成（§4.2 F）：哨兵文件（评审多达 10/40 轮、复测多达 31/40 轮乱序，迟到事件的 id 总是更大）；事件 id 规则（fseventsd 的 id 不按写入顺序分配：安静时 17/30 轮有文件没覆盖）；哨兵加 `FlushSync` 加排空（一轮也没让不完整的轮数减少；洪泛下 26/30 轮不完整）；`sinceWhen` 重放加 `HistoryDone`（`HistoryDone` 比事件早到，27/40 到 34/40 轮不完整）。`FlushSync` 本身约 0.05 ms、只送达 11% 到 35%。正确性本来就不依赖监听（design §6.2、裁定 T4）；overlay 决定怎么用（Before*/Manual 冷扫，即不靠事件、靠 `unchanged_since` 担保的全量遍历，不重读内容；ToolCall 钩子路径加 `flush`；Bash 温扫或一次调用的滞后），不得声称比测量支持的更多 | 2026-10-22 |
| 只改 xattr 或 FinderInfo 的事件 | 待定 | 现在不过滤（保守）。首个 spike 看真实噪声（`com.apple.provenance` 一类）再定是否丢掉只有 `ItemXattrMod`、`ItemFinderInfoMod` 的事件 | — |
| `case_fold_eq` 的定义 | [我的决定] D145：`NFD(toCasefold(NFD(x)))`，全量折叠，无土耳其语映射，折叠表 ≥ Unicode 16.0；以 oracle 测试作验收；经 `case_fold_key` 定义（见下一行） | macOS 27 的 APFS 上 51 对样本零不符，含 Unicode 16 新增（§4.2 B） | 2026-10-22 |
| 折叠表的来源 | 自己从 Unicode 17.0 的 CaseFolding.txt（C+F）生成表，`scripts/gen_casefold_table.py` 逐字节复现 | 不引入 ICU；`case_fold_eq_matches_a_case_insensitive_volume` 全绿 | 2026-10-23 |
| 测试接缝 | [我的决定] 线程局部的 `set_before_open`、`set_before_meta`、`set_after_bulk_window`，在 feature `test-seams` 下；`before_open(step, name)` 覆盖 `remove_tree_rooted` 的每次子目录打开、`clone_from_rooted` 的 `fclonefileat`、`clone_file_rooted(.., Some(mode))` 克隆之后取回 dst 的那一次 `openat`，以及 `remove_rooted` 紧挨着 `unlinkat` 的那一次（`EmptyDir` 的 `fstatat` 之后，所以测试能在两者之间把空目录变成挂载点）；`make_removable`、`set_file_meta` 每次实际改动之前触发的是另一个钩子 `before_meta(step, change)`，`change` 是枚举 `MetaChange::{Flags, Mode, Mtime}`，序号每次调用从 0 起（评审 n4：不共用 `before_open` 的 `name` 参数，所以叫 `mode` 的 dst 不会被按名字过滤的钩子当成"改 mode"那一步；`clone_file_rooted(.., Some(mode))` 里的 `set_file_meta` 触发 `(0, Mode)`，与 `before_open` 的 step 互不相干）；真实文件系统上造不出确定的"第一项改动之后失败"：属主的 `fchmod`、`futimens` 不会因 ACL 的 `deny writesecurity` 或超界的时间而失败，探针；钩子里把 `uchg` 重新设上，后一步就得 `EPERM` | 裁定 "Test seams"：换目录、换符号链接需要在两步之间执行代码；线程局部使并行的测试互不干扰 | 2026-10-22 |
| 并行克隆 | [我的决定] apfs 的每个函数是同步的单线程调用，并行度归调用方（session 的 `new` 并行克隆各仓库） | design §8 的 5 s 要靠并行（tool 的 `node_modules` 4 路 4.2 s 完成 4 份）；整个布局的投影见下面的待定行；crate 里不开线程池；**Task 10 实测同一个卷上 4 路并行的总耗时是单份的 3.5 到 4.0 倍，即并行几乎不提速**（合成树、内部卷；U 盘卷上没复测）| 2026-10-23 |
| 持久化 | [我的决定] apfs 不 `fsync` | 断电安全归调用方（`F_FULLFSYNC`，裁定 "Durability"） | 2026-10-22 |
| 断电后 `clonefile` 的原子性 | 待定 | 进程被杀时没有观察到半成品；断电没有测过。调用方的 intent 行是兜底（§6）。首个 spike 能测则测 | — |
| 新建会话的总耗时（评审 I3；增补 8 的用户一行） | 待定 | 整个布局的 deps 目录约 62 万项（§5.9 逐仓库列出）：真实树上单线程 12 到 19 s、4 路并行约 5.5 s，**还没算源码树和 tool 的 `src-tauri/target`**；内部卷上合成树的下界是单线程 4.2 s。design §8 的"新建会话 < 5 s"对整个布局很可能做不到。并行归 session；出路：并行克隆各仓库、按仓库懒克隆 deps、放宽目标。**设计目标的取舍请用户定**（经 lead）；Task 10 用全部兄弟库、并行度 1 到 4，在工作区所在的卷上复测。Task 10：真实兄弟库在 `/Volumes/GWD/inter`，本轮禁止访问，没有复测；合成树上 `clonefile` 并行不扩展（4 路 ≈ 串行），所以"靠并行做到 5 s"这条出路基本不成立，只剩懒克隆或放宽目标。复测方法：`STRATA_SPIKE_REPOS=... cargo test --release -p strata-apfs --test budget spike -- --ignored --nocapture` | — |
| `attrs_of` 与单目录列表的单次耗时 | `attrs_of` 中位约 19 µs（p99 40 µs）；100 项单目录列表中位约 0.1 ms（release）、0.4 ms（debug）（§5.9） | Task 10 `spike_primitive_latencies` 实测；"checkpoint 改 50 个文件 < 200 ms"在原语层面余量很大 | 2026-10-23 |
| macOS 27 上用 `fchflagsat` 解"既带 `uchg` 又没有读位也没有搜索位"的角落 | 待定 | 第 2 轮的 `O_EXEC` 句柄已经把角落缩小到属主既没有读位也没有执行位（搜索位）又带 `uchg` 的条目（文件、目录都一样，0000、0200 一类）；`fchflagsat` 能解，但要运行期检测（`#[available]` 或 `dlsym`），多一个分支只为一个极少见的角落；首个 spike 若在真实的树里见到这类条目再加 | — |
| 平台范围 | 待定 | 本文的实测只在 macOS 27.0.1 上做过；design 写的是 macOS 26+，用到的接口（`clonefile`、`fclonefileat`、`getattrlistbulk`、`FSOPT_RETURN_REALDEV`、`O_SYMLINK`、`fchflags`、FSEvents）都早于 26，但 §7 的契约测试要在 26 上复跑一遍确认；**唯一晚于 26 的是 `fchflagsat`（27.0），所以不用**；`flush_is_not_a_fence` 的数字要在 26 上重测；`O_EXEC` 句柄对 mode 0100、0000、0200 的条目的行为，以及无操作的 `chmod`、`chflags` 也前进 `ctime` 的行为，同样只在 27.0.1 上量过，26 上要复测 | — |
