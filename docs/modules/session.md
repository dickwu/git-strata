# `session` 模块

**Crate**：`crates/strata-session` · **设计来源**：design.md §3、§4、§5、§6.1、§6.3、§6.4、§6.5 第 7 步、§6.6、§6.7、§6.8、§6.9、§7、§8、§9 · **矩阵**：目标模块为 `session` 的行，加上理由点名 session 的 `—` 行，共 51 行：3b 13 行、3c 4 行、3d 29 行（含裁定改归 session 的 `:176`、`:178`）、3e 5 行；3a、3f 没有。逐条见 §7

> 本文按 module-rulings.md 写："Common"、"T9 session"，以及增补 1–8 里标了 T9、session 或 Everyone 的条目。与 design.md 的旧措辞不同的地方以裁定为准：工作区路径按会话 id 取，即 `<root>/sessions/<session-id>/<repo>/`，ticket 只是标签；不变量 4 优先于 design §6.4 的最后一句；工作区的 git 是每个会话、每个 repo 一个影子仓库（增补 2）；错误时机按增补 3–6 的细化（§6.1）。标 [我的决定 Dn] 的地方是 design 和裁定都没有定、由本文决定的，§9 按编号逐条列出。用到的邻居文档版本：gitio.md @ac4149f、store.md @0062a3e、deps.md @22e1fac、overlay.md @77e2e0c、apfs.md @1e723f2。本文用到、而它们还没有的名字（如 apfs 第 2 轮才加的 `CloneKind`），列在 §2 的"上游要求"里。

## 1. 职责与边界

session 负责：

- **会话生命周期**：`new` 建会话，`status` 报状态，`remove` 删会话（design §6.1、§6.3、§6.6）。生命周期的词汇、转换图与含义归 session，store 镜像它们并用触发器守住（增补 4，§4.3）。
- **兄弟布局**：配置里的每个仓库在会话根下都有一个工作区目录，目录名就是 `RepoName`，所以 `../quick-start`、`../release-notes`、`../node_modules` 这类相对路径在会话里照常成立（design §3）。
- **基线的取得与钉住**：每个仓库取发布链顶或 `origin/<default_branch>` 的 commit（design §6.7）；基线 generation 缺失时建出来：导出源码树，经 deps 取得匹配的依赖集并克隆进去，经 overlay 建清单，改名到 `Store::base_dir` 再登记（`ensure_base`、`ensure_base_for`；增补 3 的 "Base generation layout" 与 "Base manifest"）。建是缓存填充，调用方在拿 repo 写锁之前调用，锁里只做登记（module-rulings "Base generations are built outside the lock"）。
- **种子缓存与种子文件**：判定、从主 checkout 克隆、记录来源与时间（design §4）。
- **工作区里的 git 的布局**：每个会话、每个 repo 一个影子仓库放在哪、管理目录的代数、新一代随目录原子换入、pin ref、退役；底层的建法归 gitio（增补 2，§4.2）。
- **会话识别**：按 cwd 识别会话（`resolve`），会话选择器（id 或 ticket，`select`），列出会话（`list`）。
- **`move_workspace`**：基线与依赖一起移动的原子交换，以及它的结果 `MoveReport`（types.md §2.1、§8；design §6.4、§6.5 第 7 步）。交换开始之后失败、把工作区留在健康 `Unknown` 的移动，由人看过现场之后经 `resolve_failed_move` 放弃或接受（§5.9；module-rulings "Resolving a failed move"）。
- **自己操作的意图、阶段与现场**：`new`、`move_workspace`、`remove` 走到哪一步（phase）、`Unknown` 现场的形状（`Scene`），以及 daemon 重启时的收尾（`recover`）。现场在 `status` 里报出（不变量 6）。

session 不负责的事，以及各归哪个模块：

| 事项 | 归谁 |
| --- | --- |
| overlay 的计算、checkpoint 的写入、restore、整文件三方投影（`rebase`）、把条目落到目录上（`project`）、基线清单的内容哈希（`build_manifest`） | overlay。session 在移动和删除之前经 daemon 交来的 `BeforeCheckpoint` 记 checkpoint（用 daemon 的 Tracker，§3），在移动时调它投影 |
| 依赖集的键、认领、冷装、封存、漂移判定、变更提议、rollout 计划，以及枚举打开的文件 | deps。session 只调 deps.md §3 的函数，从不自己认领、冷装或算 set id（增补 3 "Deps API"） |
| 起 git 子进程、导出 tree、ignore 判定、ref 的比较并交换、影子仓库与管理目录的建法 | gitio。session 从不起 git（`Git::command` 是 gitio 的 crate 私有） |
| clonefile、根之下不跟随符号链接的读写与改名、遍历、同卷判定、`F_FULLFSYNC` | apfs |
| 表结构与全部 SQL、目录的退役（`retire_dir`）、gc 与保留期 | store。目录只经 `retire_dir` 离开活路径；session 自己只删自己建的空目录（`remove_rooted`，增补 3、7、8） |
| 发布、发布链，以及发布后要不要移动、移动不了时记提议 | publish。它是 `move_workspace` 的调用方之一 |
| per-repo 写锁的实现与 `Busy`、同一会话的请求排队、监听（overlay 的 `Tracker`；Before* checkpoint 经 `BeforeCheckpoint` 用它）、status 回复的组装（含工作区当前的改动 `changed`，以及失败的移动可选的两种处理）、启动时机 | daemon。人类确认：confirm。TTY 检查与处理失败的移动的命令：cli |
| 配置的静态校验 V1–V11，含 V3 的字面嵌套 | types（config.md §4.1）。经符号链接等别名才嵌套的写法不是配置规则，由 `new` 在阶段 A 按真实路径判定（§5.1 第 4 步；增补 1 的 T9 条） |
| 检索视图与共享基线索引 | search。影子仓库让工作区里的 `git rev-parse --git-common-dir` 得到本会话的影子仓库，tgrep-core 由 common dir 推出的仓库身份因此每个会话都不同；基线索引怎样仍跨会话共享，由 search.md 定（增补 2 的 T11 条）。本文只说明这个后果（§4.2） |

对应 design 的章节：§3 的兄弟布局、symlink 陷阱、被 ignore 但必需的文件；§4 的 Session、Seed cache、Seed file；§6.1 全节；§6.3 的 `status`；§6.4 的"依赖与源码一起移动"；§6.5 第 7 步的交换；§6.6 全节；§6.7 的钉基线规则；§6.8 全节；§6.9 的 `rm --discard`；§8 的两个验收数字；§9 不变量 1、2、4、5、6、7、8。

## 2. 依赖

Workspace crate（types.md §8：session 在第 3 层，只依赖第 0–2 层）：

| 依赖 | 用途 |
| --- | --- |
| `strata-types` | `RepoName`、`RepoId`、`SessionId`、`TreeOid`、`CommitOid`、`DepsSetId`、`CheckpointId`、`RepoPath`、`BaseGen`、`BaseManifest`、`SessionManifest`、`WorkspaceState`、`CheckpointReason`、`ConfirmToken`、`Action`、`StrataError`、`Code`、`serde_rfc3339`；config.md 的 `Config`、`Paths`、`RepoConfig`（含 `seed_files()`） |
| `strata-apfs`（apfs.md @1e723f2） | `same_volume`、`nearest_existing_ancestor`、`volume_caps`（`new` 的卷预检，config.md §4.3）；目的地变体 `clone_tree_rooted`、`clone_file_rooted`、`mkdir_rooted`（工作区、种子缓存、种子文件：增补 3 "Rooted destinations"）；`clone_from_rooted` 加 `CloneKind::{File, Dir}`（携带：从旧树里不跟随符号链接地克隆出来；`CloneKind` 见上游要求）；`rename_rooted` 加 `RenameMode::Excl`（基线改名进 `bases/`、携带的条目放进新树）与 `RenameMode::Swap`（交换工作区）；`remove_rooted` 加 `RemoveKind::EmptyDir`（删自己建的空暂存目录，增补 7、8）；`full_fsync`，以及取目录句柄的 `open_root`（会话根这类 rooted 操作的根）与 `open_rooted`（增补 3 "Durability"）；`attrs_of`（含符号链接的目标 `FileKind::Symlink`）、`walk_with`、`WalkOptions`、`list_dir`（种子与携带的判定、找已有的基线 generation）；`symlink_rooted`（种子与携带里的符号链接按目标重建，module-rulings "A symlink is recreated, never cloned"）；`case_fold_key` 与 `volume_caps` 的 `case_sensitive`（种子落点的磁盘同一性，§5.3）；`classify_io_error`（session 自己的 `open` 与 `fcntl(F_GETPATH)`、`nearest_existing_ancestor` 的 errno 归类） |
| `strata-gitio`（gitio.md @ac4149f） | `Git` 的 `fetch`、`read_ref`、`is_ancestor`、`merge_base`、`tree_of`、`tree_contains`、`export_tree`、`ensure_shadow`（只在 `new`）、`shadow_at`、`shadow_drift`、`attach_worktree`、`update_ref`、`delete_ref`、`is_ignored`、`read_index`、`repo_id`；上游要求里的 `is_ignored_in_export`、`worktree_gitdir`；类型 `Shadow`、`MirrorDrift`（含 `PrePushGuardDisabled`）、`IndexInfo`；常量 `EXPORT_TEMP_PREFIX` |
| `strata-store`（store.md @0062a3e） | 路径与基线：`root`、`base_dir`、`has_base_gen`、`manifest`、`record_base_gen`、`retire_dir`；会话的写：`create_session`、`set_session_phase`、`set_lifecycle`、`record_seeds`、`begin_move`、`set_move_phase`、`set_move_state`、`finish_move`、`fail_move`、`set_old_tree`，上游要求里的 `resolve_move`；读：`session`、`sessions`、`move_intents`、`open_move_intents`、`latest_checkpoint`、`checkpoints`、`checkpoint`；行类型 `Lifecycle`、`Health`、`SessionRecord`、`WorkspaceRecord`、`SeedRow`、`MoveIntent`、`MoveState`、`CheckpointInfo` |
| `strata-overlay`（overlay.md @77e2e0c） | `build_manifest`；`reconcile_full`（`remove` 的未发布改动检查、旧树核对）；`rebase` 与 `RebaseOptions::for_move`；`project`；类型 `WorkspaceRef`、`Base`、`Overlay`、`Rebased`、`Checkpointed`（`BeforeCheckpoint` 的签名用到；`BeforeRollout`、`BeforeRemove` 由 daemon 的实现经 `checkpoint` 记下，§3）；常量 `RESTORE_TEMP_PREFIX` |
| `strata-deps`（deps.md @22e1fac） | `Ctx`、`RepoWriteLock`、`InstallEnv`；`main_tip`、`key_of`、`find_set`、`ensure`、`clone_into`、`nested_seed_caches`、`observe_main`、`open_files`、`drift`、`open_proposals`；类型 `Drift`、`Proposal`、`ProposalState`、`OpenFile` |
| `serde`、`serde_json` | §3 里出现在 JSON 上的类型；`Scene` 以 JSON 交给 store 存（形状归 session，store 只核对它是 JSON，增补 4） |
| `libc` | `fcntl(F_GETPATH)`（`resolve` 与嵌套检查要的真实路径，大小写按卷上的写法）、`arc4random_buf`（会话 id 与暂存目录名的随机数） |

依赖符合 types.md §8：不依赖 publish、search、daemon、confirm、cli。

谁调用本模块：

- **daemon**：持有 `Sessions` 句柄（加载配置时建一个，重载配置时重建），调用全部公开函数。启动时先调一次 `recover`，然后才跑 gc、接受请求。每次移动之前，它在锁外先备好目标基线 generation，再持该 repo 的写锁调 `move_workspace`（module-rulings "Base generations are built outside the lock"）：
  - `deps approve`：锁外先调 `ensure_base(repo, P.head)`。它导出 `P.tree`（deps.md §5.12 第 3 步"经 session 与 gitio 备好 P.tree 的导出"），`P.to` 还没封存时经 `deps::ensure` 封存它。再按 deps.md 调 `ensure_target(ctx, id, store.base_dir(&gen))`；得到 `Ready` 之后，锁外调 `ensure_base_for(repo, &P.tree, &P.to)`，确保计划的目标 generation 在（通常上一步已经建好，这里只是一次 `has_base_gen`；提议之后工具链变了而 `P.to` 早已封存时，上一步建的是别的依赖集）。然后持锁，`plan_rollout`，对计划里的每个 `Move` 调 `move_workspace`。
  - 提出者自换（deps.md §5.12 `deps propose` 第 4 步）：目标按 deps.md §5.10 `plan_switch` 第 2 步，`AwaitingPublish` 时是"旧 tree 配新依赖集"（`ws.base.tree`），`Pending` 时是 `P.tree`，依赖集都是已封存的 `P.to`。锁外调 `ensure_base_for(repo, <目标 tree>, &P.to)`，再持锁调 `plan_switch` 与 `move_workspace`。
  - cli 发起的移动同样先调 `ensure_base` 或 `ensure_base_for`。
  - 交换之后失败的移动：status 给出 Abandon 与 Accept 两种处理（T13），人经 cli 选定之后调 `resolve_failed_move`（§5.9）。
- **publish**：daemon 把 `&Sessions` 按引用交给 `publish::rebase_session`。它在锁外备好新基线，再持该 repo 的写锁调用 `move_workspace`（M23）。用哪一个由 publish.md 按 design §6.5 第 7 步定：依赖集不变的移动用 `ensure_base_for(repo, &tree_of(H′), &ws.base.deps)`，从不触发冷装；`ensure_base(repo, H′)` 会按 H′ 的安装输入取依赖集，可能冷装。
- **search**：调用 `resolve`。
- **cli**：只经 daemon 的 RPC。daemon 不在时，cli 可以用只读打开的 Store 建 `Sessions`，只调 `resolve`、`select`、`list`、`status`；写函数会得到 store 的 `ReadOnly`。

### 上游要求

下面是 session 用到、而邻居文档的当前版本还没有的名字与行为。

**对 gitio**（gitio.md @ac4149f）：

1. 新增 `Git::is_ignored_in_export(&self, dir: &Path, tree: &TreeOid, path: &RepoPath) -> Result<bool>`：在 `tree` 的一份导出 `dir`（没有 `.git`）上回答"把这个 tree 检出成本仓库的工作区之后，git 会不会忽略 `path`"。规则同 `is_ignored`（gitio §5.8），差别有三：已跟踪的判定用 `tree_contains(tree, path)` 而不是 index；`.gitignore` 从 `dir` 里读；`info/exclude` 与 `core.excludesFile` 取主 checkout 现在的那份，也就是 `ensure_shadow` 将要镜像的那份。只读，不需要锁。session 要在 `create_session` 之前、对着基线导出判定显式种子文件（review I3；增补 6：`new` 的类型化拒绝全在 `create_session` 之前），那时工作区还不存在，`is_ignored(worktree, …)` 没有 `.git` 可读。
2. `export_tree` 在锁外调用：gitio.md @ac4149f §5 开头把它列在"由调用方在 per-repo 写锁内调用"的操作里。module-rulings "Base generations are built outside the lock" 让 session 在锁外导出基线（§5.2 第 4 步）。目的地是 session 自己的暂存 `B/tree`，每次调用一个随机名，两次导出从不撞名；私有 index 在 `.export-*` 临时目录里，不写主 checkout 的工作树、index 与 refs。22 个仓库都没有过滤器（gitio §5.5 第 4 步），将来的过滤器在 common dir 里的缓存由过滤器自己处理并发。gitio 的下一轮把 `export_tree` 从那张持锁清单里拿出来。
3. 新增 `Git::worktree_gitdir(&self, tree_dir: &Path) -> Result<Option<PathBuf>>`：不跟随符号链接地读 `<tree_dir>/.git`。它是 gitfile 时返回其中 `gitdir: ` 之后的路径，读法同 gitio 读 gitfile 的规则（§5.7 第 1 步）；`tree_dir` 或 `.git` 不存在、`.git` 不是普通文件时返回 None。只读，不需要锁。`resolve_failed_move` 用它判断哪棵树接在哪一代的管理目录上（§5.9 第 3 步）：gitfile 的格式归 gitio，session 不自己解析。

**对 store**（store.md @0062a3e）：

1. 已照写，不再是要求：`retire_dir` 第 7 行按形状接受基线暂存 `bases/<repo>/.staging/<32 位小写十六进制>`（store.md §5.5），谁拿走它由调用方保证：建它的那次调用，以及 daemon 启动时的 `recover`（那时没有建造在跑）；建它的调用把名字记在进程内的集合里，别的调用从不碰它（§5.2）。`gc_repo` 第 5 步把最近一次直接条目的变化已过年龄下限的暂存挪进 trash（store.md §5.11）。§5.2 照此在清单一建好就登记，并把暂存上的 `NoSuchPath` 当作"已被收走"（module-rulings "Base staging is registered right after its manifest"）。
2. 已照写、不再是要求：`MoveState` 的词汇与会话行的 `phase` 列（增补 5 的 T9 条）session 照原样采用（§4.3）；`set_old_tree` 接受与意图的 `staging` 相同的路径（形状相同），session 把保留的旧树留在原地（§5.7 第 19 步）。
3. 说明，不是要求：module-rulings "Carry is not a capture" 改正了增补 8 的说法。session 的携带把旧工作区的条目克隆进新树，不是往 store 里捕获，不进 blob store，也不调 `put_blob_rooted`（§5.7.1 [我的决定 D34]）；store.md @0062a3e 的调用方清单已照此改。同样，session 自己从不往工作区、会话根或暂存树里写 blob 内容：新一代的树由 overlay 的 `project` 投影（增补 8 让它改用 `get_blob_rooted`），种子与携带都是克隆。所以 session 既不调 `get_blob`，也不调增补 8 新增的 `get_blob_rooted`。
4. 处理失败的移动（module-rulings "Resolving a failed move"；§5.9）要一个带守卫的方法，暂名 `resolve_move(&self, id: &SessionId, repo: &RepoName, n: u64, accept: bool, at: SystemTime) -> Result<()>`。守卫：会话是 `Active`；工作区健康是 `Unknown`；意图 n 是这个工作区代数最大的意图，状态是 `Unknown`。一个事务里：
   - `accept = false`（Abandon）：意图 `Unknown → Abandoned`；工作区健康 `Unknown → Ok`；base、head、代数与冲突不变。
   - `accept = true`（Accept）：意图 `Unknown → Done`；工作区的 base、head、代数换成意图的 `to`、`head`、`n`，冲突换成第 5 条记在意图行上的那份；意图的 `old_tree` 记成它的 `staging`；健康 `Unknown → Ok`。
   - 两种都把工作区的现场留作历史（同会话行离开 `Unknown` 之后的做法）。守卫不过 → `InvalidInput`。这是 store 现在没有的三条边：健康 `Unknown → Ok`，意图 `Unknown → Abandoned` 与 `Unknown → Done`，都只经这个方法。
5. 意图进入 `Swapping` 时（`set_move_state` 的 `Preparing → Swapping` 这条边），同一个事务里把 `rebase` 算出的冲突路径记在意图行上（`MoveIntent` 加 `conflicts`）[我的决定 D46]。交换之后失败、进程又崩溃时，冲突只能从这里找回；Accept 照它恢复，冲突才会继续挡住发布（design §6.5 第 7 步）。给这条边加参数还是另开一个方法，由 store 定。

**对 apfs**（apfs.md @1e723f2）：

1. `clone_from_rooted` 的 `kind: CloneKind` 参数与 `CloneKind { File, Dir }`（module-rulings "clone_from_rooted states its source kind"）：1e723f2 的 `clone_from_rooted` 还是三个参数，apfs 第 2 轮加上。种类对不上时，apfs 在克隆之前给 `IsDirectory`、`NotFile` 或 `NotDirectory`。session 的携带按遍历看到的种类传 `File` 或 `Dir`（§5.7.1）。

**对 deps**（deps.md @22e1fac）：

1. `observe_main`：module-rulings "deps follows main's current tip" 把它定为 repo 写锁里的一小段临界区，deps 自己取锁，在锁里读主干的 tip。session 调它时不持有任何 repo 写锁（§5.1 第 14 步；锁的顺序见 §5 开头的【锁 R】）。签名不变（deps.md §2 对 session 的要求第 4 条）。
2. 已照写，不再是要求：deps.md §2 对 session 的要求第 7 条，`new` 第 8 步 fetch 之后用 `deps::main_tip(cfg, git)` 选 head（§5.1 第 8 步）。design §6.7 的钉基线规则只写在 deps 一处，`observe_main` 判定主干当前的 tip 用的是同一个函数。

**对 daemon**（daemon.md 还没写）：

1. 每个 repo 对主 checkout 打开一个 `Git`，按 `RepoName` 交给 `Sessions::open`；实现 `deps::RepoWriteLock`（有界等待之后 `Busy`），给出 `deps::InstallEnv`；实现 `BeforeCheckpoint`（§3）：用它为这个工作区持有的 Tracker 记 `BeforeRollout`、`BeforeRemove`，还没有 Tracker 时传 None（module-rulings "Session's Before* checkpoints use the daemon's Tracker"）。
2. 调 `new`、`remove`、`ensure_base`、`ensure_base_for`、`resolve_failed_move` 时不持有任何 repo 写锁（它们自己在短临界区里取，`RepoWriteLock` 不可重入）。调 `move_workspace` 之前，先在锁外用 `ensure_base` 或 `ensure_base_for` 备好目标基线，再持该 repo 的写锁调它（module-rulings "Base generations are built outside the lock"）。
3. 同一会话的 checkpoint、restore、`remove`、`move_workspace`、`resolve_failed_move` 串行（module-rulings T7、T13）：`move_workspace`、`remove` 与 `resolve_failed_move` 进行时，这个会话没有别的写在跑。
4. `move_workspace` 返回之后（`Ok`，或交换之后的 `Unknown`），以及 `resolve_failed_move` 的 Accept 之后（工作区换了基线），对该工作区 `Tracker::reset` 再 `watch`，下一次扫描是冷扫（增补 3 "Projection"、增补 5）。
5. 组装 status 回复：`SessionStatus` 之外，每个 tracked 工作区的当前改动 `changed` 由 `overlay::compute` 在读的时候算出（增补 3：`status.changed` 读时重算；[我的决定 D21]）；并入 `Tracker::warnings`、restore 现场目录里的 `plan.json`（增补 7）、`Sessions::leftovers()` 与 `deps::attempts`（M22）。健康为 `Unknown` 的工作区给出 Abandon 与 Accept 两种处理（module-rulings "Resolving a failed move" 的 T13 部分）；保留的旧树列出持有它的进程（module-rulings 对 session 853fdf8 的 T13 条，取 `OldTreeKept.holders`）。
6. 启动时：`recover` 之后才 gc（增补 4 的 T13 条），然后才接受请求。

**对 publish**（publish.md 还没写）：`rebase_session` 在拿锁之前调 `Sessions::ensure_base_for` 或 `Sessions::ensure_base` 备好新基线（见上文的调用方），再持该 repo 的写锁调 `Sessions::move_workspace`；`MoveReport` 的字段见 §3。

**对 cli**（cli.md 还没写）：处理失败的移动是一个显式命令，人看过 status 之后选 Abandon 或 Accept，没有选择就不运行（module-rulings "Resolving a failed move" 的 T14 部分）；它经 daemon 调 `resolve_failed_move`。

## 3. 公开接口（Rust）

```rust
// crates/strata-session/src/lib.rs —— 签名清单（不是实现）
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use strata_deps::{Drift, InstallEnv, OpenFile, Proposal, RepoWriteLock};
use strata_gitio::Git;
use strata_overlay::{Base, Checkpointed, WorkspaceRef};
use strata_store::Store;
use strata_types::{
    BaseGen, CheckpointId, CheckpointReason, CommitOid, Config, ConfirmToken, DepsSetId, RepoConfig,
    RepoId, RepoName, RepoPath, SessionId, SessionManifest, StrataError, TreeOid,
};

/// types.md §8 的 `Result<T>`。
pub type Result<T> = std::result::Result<T, StrataError>;

/// move_workspace 的暂存目录名前缀：`<会话根>/.strata-tmp-move-<repo>-<16 位十六进制>/`（§4.1）。
pub const MOVE_STAGING_PREFIX: &str = ".strata-tmp-move-";
/// 建基线 generation 的暂存目录：`<root>/bases/<repo>/.staging/<32 位十六进制>/`（§4.1、§5.2）。
pub const BASE_STAGING_DIR: &str = ".staging";
/// remove 的 Dirty 细节里每个仓库最多列出的路径数（§5.6）。
pub const DIRTY_PATHS_CAP: usize = 100;

/// daemon 持有的会话句柄（M23）：借用配置、每个 repo 对主 checkout 打开的 Git、Store、
/// daemon 的 per-repo 写锁、安装环境与 Before* checkpoint 的入口。不缓存会话状态，每次调用都读 Store；
/// 进程内只记"哪些基线暂存目录正被本进程的调用使用"（§5.2）。它不是数据，不 derive serde。
pub struct Sessions<'a> { /* config、gits、store、locks、env、before、live_staging：私有 */ }

/// daemon 实现，交给 `Sessions::open`：session 的 `BeforeRollout`、`BeforeRemove` 都经它记下
/// （module-rulings "Session's Before* checkpoints use the daemon's Tracker"）[我的决定 D44]。
pub trait BeforeCheckpoint: Sync {
    /// daemon 为这个工作区持有 Tracker 时调 `overlay::checkpoint(ws, base, Some(tracker), reason)`：不依赖事件、
    /// 由 `unchanged_since` 作保的全量遍历（module-rulings "Cold scan"）；还没有 Tracker（daemon 启动之后
    /// 这个工作区的第一次扫描之前）时传 None，即读遍每个候选文件。调用期间这个 Tracker 不给别的扫描用。
    /// `move_workspace` 在 repo 写锁里调用它，所以实现不能取这把锁（`RepoWriteLock` 不可重入）。
    fn checkpoint(&self, ws: &WorkspaceRef<'_>, base: &Base, reason: CheckpointReason) -> Result<Checkpointed>;
}

impl<'a> Sessions<'a> {
    pub fn open(
        config: &'a Config,
        gits: &'a BTreeMap<RepoName, Git>,
        store: &'a Store,
        locks: &'a dyn RepoWriteLock,
        env: &'a InstallEnv,
        before: &'a dyn BeforeCheckpoint,
    ) -> Result<Sessions<'a>>;

    pub fn new(&self, ticket: Option<&str>, repos: &[RepoName]) -> Result<SessionManifest>;
    /// `head` 的 tree 配上它的安装输入所需的依赖集（经 `deps::ensure`，可能冷装）。锁外调用（§5.2）。
    pub fn ensure_base(&self, repo: &RepoName, head: &CommitOid) -> Result<BaseGen>;
    /// 指定的 tree 配上指定的、已封存的依赖集，从不调 `deps::ensure`。提出者自换的目标"旧 tree 配新依赖集"
    /// 由它备好（design §6.4 第 3 步）。锁外调用（§5.2）。
    pub fn ensure_base_for(&self, repo: &RepoName, tree: &TreeOid, deps: &DepsSetId) -> Result<BaseGen>;
    pub fn resolve(&self, cwd: &Path) -> Result<Option<(SessionId, RepoName)>>;
    pub fn select(&self, selector: &str) -> Result<SessionId>;
    pub fn list(&self) -> Result<Vec<SessionId>>;
    pub fn status(&self, id: &SessionId) -> Result<SessionStatus>;
    pub fn leftovers(&self) -> Result<Vec<Leftover>>;
    pub fn remove(
        &self,
        id: &SessionId,
        discard: bool,
        confirm: Option<ConfirmToken>,
    ) -> Result<RemoveReport>;
    pub fn move_workspace(
        &self,
        session: &SessionId,
        repo: &RepoId,
        new_base: BaseGen,
        head: CommitOid,
    ) -> Result<MoveReport>;
    /// 人看过现场之后，处理一次交换开始之后失败的移动（§5.9）。
    pub fn resolve_failed_move(
        &self,
        session: &SessionId,
        repo: &RepoName,
        choice: MoveResolution,
    ) -> Result<ResolutionReport>;
    pub fn repo_name(&self, repo: &RepoId) -> Result<RepoName>;
    pub fn recover(&self) -> Result<RecoverReport>;
}

/// 判定一个仓库的种子文件（§5.3）：对着主 checkout `main` 读来源，对着基线 generation 目录 `base`
/// （tree 为 `tree`）判定被跟踪、被 ignore 与目的地。只读。
pub fn seed_plan(
    cfg: &RepoConfig,
    git: &Git,
    main: &Path,
    base: &Path,
    tree: &TreeOid,
) -> Result<Vec<SeedSource>>;

/// 按 seed_plan 的结果，把种子文件克隆进工作区 `to`（根之下、不跟随符号链接），返回复制了的那些。
pub fn seed_files(plan: &[SeedSource], to: &Path) -> Result<Vec<SeedRecord>>;

/// seed_plan 判定通过的一个种子文件：来源的 lstat 结果（符号链接另有 attrs_of 读出的目标）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeedSource {
    pub path: RepoPath,
    /// 主 checkout 里的绝对路径。
    pub source: PathBuf,
    /// 来源是符号链接时，链接的目标：经 `apfs::attrs_of(main, path)` 读出，复制时 `symlink_rooted` 按它重建（§5.3）。
    pub link: Option<PathBuf>,
    pub size: u64,
    pub source_mtime: SystemTime,
}

/// 一个复制过来的种子文件（store 的 SeedRow 与它一一对应）。只有来源与时间，从不含内容（design §4、config.md §2.3）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeedRecord {
    pub path: RepoPath,
    pub source: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<PathBuf>,
    pub size: u64,
    #[serde(with = "strata_types::serde_rfc3339")]
    pub source_mtime: SystemTime,
    #[serde(with = "strata_types::serde_rfc3339")]
    pub copied: SystemTime,
}

/// 一个会话的状态（design §6.3）。JSON 形状由 cli.md 引用。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionStatus {
    pub manifest: SessionManifest,
    pub state: SessionState,
    #[serde(with = "strata_types::serde_rfc3339")]
    pub created: SystemTime,
    /// new 或 remove 最近记下的阶段（§6.3）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// state 为 Unknown 时的现场。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<Scene>,
    pub workspaces: BTreeMap<RepoName, WorkspaceStatus>,
}

/// 会话的生命周期（§4.3）：词汇与转换图归 session，store 的 `Lifecycle` 逐项镜像它。序列化为变体名。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionState { Creating, Active, Removing, Removed, Unknown }

/// 工作区的健康。Missing 看文件系统（工作区目录、它的 `.git` 或影子仓库不在），从不存；Unknown 来自 store。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspaceHealth { Ok, Missing, Unknown }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceStatus {
    pub health: WorkspaceHealth,
    /// 管理目录的代数（§4.2）。
    pub generation: u64,
    /// health 为 Unknown 时的现场。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<Scene>,
    /// 最新的 checkpoint；tracked = false 的仓库没有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<CheckpointId>,
    /// 工作区算出的安装输入哈希与所钉依赖集的不同（deps 的 `drift`）。是数据，不是错误。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drift: Option<Drift>,
    /// 这个会话正在等待的提议：Pending，且本工作区 tracked、`base.deps ≠ P.to`（deps.md §4.5）。
    pub pending: Vec<Proposal>,
    /// 最近一次 move_workspace 记下的冲突路径。
    pub conflicts: Vec<RepoPath>,
    pub seeds: Vec<SeedRecord>,
    pub warnings: Vec<StatusWarning>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum StatusWarning {
    /// tracked = false：这里的编辑不进 checkpoint，rm 需要 --discard（增补 1 的 T9 条）。
    NotCheckpointed,
    /// 主 checkout 的来源在复制之后变过（mtime 不同），或已不在。
    SeedStale { path: RepoPath },
    /// 工作区的 ignore 规则已不再忽略这个种子文件。overlay 照样把它排除在外（overlay.md §5.1 的排除集合），
    /// 所以它仍不进 checkpoint、不会被发布；但工作区里的 `git status` 会把它列为未跟踪。
    SeedNotIgnored { path: RepoPath },
    /// 影子仓库与主 checkout 的现状不同，或 pre-push 守卫可能失效（gitio 的 shadow_drift，§5.5）。
    /// 镜像只在 new 时取一次，session 从不改写；守卫要不要钉回由 daemon 定（module-rulings 的 T13 条）。
    MirrorDrift { items: Vec<MirrorDriftItem> },
    /// 影子仓库不见了。只报告，从不重建（§4.2）。
    ShadowMissing { path: PathBuf },
    /// 一次没有走到交换的移动留下的暂存与意图（§5.7 第 9 步、§6.1），或 Abandon 之后没能退役的暂存（§5.9 第 6 步）。
    MoveLeftover { n: u64, staging: PathBuf },
    /// move_workspace 交换之后保留在原地的旧树（§5.7 第 19 步）。holders 与 changed 都在读的时候重算：
    /// changed = 旧树与那次移动的 BeforeRollout checkpoint 不一致，即交换之后有人往旧树里写过。
    /// retained = 交换时旧树里有新树没有的东西（没带过来的内容或改动），以后的移动不自动收走它。
    OldTreeKept { path: PathBuf, n: u64, holders: Vec<OpenFile>, changed: bool, retained: bool },
    /// overlay 的 restore 现场：`<会话根>/.strata-tmp-restore-<repo>/` 非空（增补 7）。
    RestoreScene { path: PathBuf },
}

/// gitio 的一条 MirrorDrift 在 status 里的形状（gitio 的类型不 derive serde）[我的决定 D43]。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MirrorDriftItem {
    /// `"info_exclude"`、`"info_attributes"`、`"config"` 或 `"pre_push_guard_disabled"`。
    pub reason: String,
    /// reason = "config" 时，节名与键名（小写），如 `core.excludesfile`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// reason = "pre_push_guard_disabled" 时：影子仓库 config 里 `core.hooksPath` 的现值（None = 键被删了）
    /// 与 `hooks/pre-push` 是否被换掉或删掉。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hooks_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_replaced: Option<bool>,
}

/// 一个 Unknown 现场：哪个操作、停在哪个阶段、留下了什么、当时返回的错误。以 JSON 存进 store
/// （会话行或工作区行），所以 daemon 重启之后报出的是同一个现场。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scene {
    pub op: SceneOp,
    pub session: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<RepoName>,
    pub phase: String,
    #[serde(with = "strata_types::serde_rfc3339")]
    pub at: SystemTime,
    pub kept: Vec<PathBuf>,
    /// 当时返回给调用方的那个 Unknown 错误（§6.3）。
    pub error: StrataError,
}

/// 序列化为小写：`"new"`、`"move"`、`"remove"`，与 Unknown 的 `detail.op` 相同（M19）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SceneOp { New, Move, Remove }

/// move_workspace 的结果（types.md §2.1：所有者 session，使用者 daemon、publish）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveReport {
    pub session: SessionId,
    pub repo: RepoName,
    pub from: BaseGen,
    pub from_head: CommitOid,
    /// 等于调用参数 new_base 与 head。
    pub to: BaseGen,
    pub head: CommitOid,
    /// 交换之前记的 BeforeRollout checkpoint；目标与当前相同、什么都没做时为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<CheckpointId>,
    /// 离开 overlay 的路径：新基线里已经是会话的内容（已发布，或两边改成了一样）。
    pub dropped: Vec<RepoPath>,
    /// 两边都改了而合不上、保留了会话版本的路径（design §6.5 第 7 步"冲突标出"）。
    pub conflicts: Vec<RepoPath>,
    /// 被 .lww 命中、整份取会话一侧的路径（overlay 的 Rebased.lww）。
    pub lww: Vec<RepoPath>,
    /// 没有带进新树的被 ignore 内容（新树在同一路径上已有东西，或是 FIFO 这类克隆不了的条目）；它们留在旧树里（§5.7.1）。
    pub uncarried: Vec<RepoPath>,
    /// 旧树被保留时的位置（§5.7 第 19 步）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_tree: Option<PathBuf>,
}

/// remove 的结果。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveReport {
    pub session: SessionId,
    pub discarded: bool,
    /// 每个 tracked 仓库删除前记的 BeforeRemove checkpoint，由 store 的 gc 按保留期留存（design §6.6）。
    pub checkpoints: BTreeMap<RepoName, CheckpointId>,
}

/// 人对一次失败的移动的选择（module-rulings "Resolving a failed move"；§5.9）[我的决定 D45]。
/// 序列化为小写：`"abandon"`、`"accept"`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MoveResolution {
    /// 交换没有发生：退役暂存，关掉意图，工作区回到原来的基线，健康回到 Ok。
    Abandon,
    /// 交换已经发生：留下新树，旧树作为保留的旧树（OldTreeKept）留着，完成意图，健康回到 Ok。
    Accept,
}

/// resolve_failed_move 的结果。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionReport {
    pub session: SessionId,
    pub repo: RepoName,
    pub choice: MoveResolution,
    /// 被关掉的那条意图的代数。
    pub n: u64,
    /// 处理之后工作区的基线与 head：Abandon 是原来的，Accept 是意图的目标。
    pub base: BaseGen,
    pub head: CommitOid,
    /// Accept：旧树的位置，以后不自动收走，随 remove 退役。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_tree: Option<PathBuf>,
    /// Abandon：没能退役的暂存，status 报成 MoveLeftover，由以后的操作收走（§5.9 第 6 步）。
    pub leftover: Vec<PathBuf>,
}

/// 不属于任何会话的残留（§5.5）。daemon 把它并进 status。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Leftover {
    /// 建基线 generation 时进程中途停下、或失败之后没能退役留下的暂存，以及其中 gitio 的导出临时目录（前缀 EXPORT_TEMP_PREFIX）。
    BaseStaging { repo: RepoName, path: PathBuf, export_temps: Vec<PathBuf> },
}

/// recover 做了什么（§5.8）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoverReport {
    /// Creating、Removing 的会话被记成 Unknown。
    pub unknown: Vec<SessionId>,
    /// 没走到交换的移动意图被关成 Abandoned，暂存与管理目录已退役。
    pub abandoned: Vec<IntentRef>,
    /// 停在交换中的移动意图：工作区记成 Unknown。
    pub failed: Vec<IntentRef>,
    /// 退役进 store trash 的基线暂存。
    pub retired: Vec<PathBuf>,
    /// 单项处理失败的错误；recover 不因一项失败而停下（§5.8）。
    pub errors: Vec<StrataError>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntentRef {
    pub session: SessionId,
    pub repo: RepoName,
    pub n: u64,
}

// ---- 测试接缝（增补 4 "Test seams"：只在 cargo feature `test-seams` 下编译，从不进发布的二进制）----

/// 装在某个阶段开始处的接缝。phase 是带操作前缀的阶段名，取值见 §6.3：
/// `new.root` … `new.finalize`、`base.export` … `base.register`、`move.prepare` … `move.old_tree`、`remove.unpin` … `remove.record`、
/// `resolve.pin`、`resolve.record`、`resolve.retire`。
/// 记下阶段的操作（`new` 的阶段 D、`move` 的第 13–18 步、`remove`）先记下阶段名，再检查接缝，所以 `StopAt`
/// 之后 `recover` 报出的 phase 就是这个阶段。`base.*` 与 `resolve.*` 不记下阶段，只是接缝的位置与 `detail.phase` 的取值。
/// 另有一个只给接缝用的位置 `move.carry`：携带的遍历做完、第一次克隆之前（§5.7.1）。它不是记下的阶段，
/// 不调 `set_move_phase`，也不出现在 `detail.phase` 里。
/// `new` 里按仓库派发的阶段（阶段 B 里的 `base.*`，阶段 D 的 `workspace` … `observe`）对每个仓库各开始一次（§5.1 第 13 步）：
/// phase 加 `@<repo>` 后缀时只在那个仓库上触发，不加时在第一个派发的仓库上触发。接缝在派发时检查，
/// 所以 `FailAt` 触发之后，排在后面的仓库都不会开始。
#[cfg(feature = "test-seams")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionSeam {
    /// 在这个阶段开始处注入一个该 errno 的 I/O 错误。
    FailAt { phase: String, errno: i32 },
    /// 在这个阶段开始处就地返回，不更新任何状态，模拟进程在这里崩溃。
    StopAt { phase: String },
    /// 在这个阶段开始处停住，直到 release（要在两步之间做事的测试用它，不用 errno 故障点）。
    PauseAt { phase: String },
}

#[cfg(feature = "test-seams")]
impl Sessions<'_> {
    /// 下一次 new 用这个 id，不随机生成（测 Exists 与 id 不复用）。
    pub fn set_next_id(&self, id: SessionId);
    /// 装上一个接缝；用一次就失效。
    pub fn arm(&self, seam: SessionSeam);
    /// 阻塞到 PauseAt 真的停住为止（由测试的另一个线程调用）。
    pub fn wait_paused(&self);
    /// 放行停住的那一步。
    pub fn release(&self);
}
```

types.md §8 写的 `session::move_workspace(session: &SessionId, repo: &RepoId, new_base: BaseGen, head: CommitOid) -> Result<MoveReport>` 就是上面的 `Sessions::move_workspace`：参数和返回类型逐字一致，接收者是 daemon 持有的 `&Sessions`（M23）。与计划 Task 9 的 Produces 相比：

- `new`、`resolve`、`status`、`remove` 都包上 `Result`：它们都读写 Store 或文件。`resolve` 返回 `Result<Option<…>>`，不把 Store 的错误吞成"不在会话里" [我的决定 D16]。
- 计划的 `seed_files(repo_cfg, from, to)` 拆成两步 [我的决定 D40]：`seed_plan` 只判定，在 `create_session` 之前对着基线 generation 目录做（review I3、增补 6）；`seed_files` 只复制，在之后做。一次调用里既判定又复制，判定就只能落在第一次对外可见的写之后。
- 计划之外新增：`select`、`list`、`ensure_base`、`ensure_base_for`、`leftovers`、`move_workspace`、`resolve_failed_move`、`repo_name`、`recover`，以及它们的结果类型。`Sessions::open` 借用的是每个 repo 对主 checkout 打开的 `Git`（`RepoId` 就是 `git.repo_id()`）、daemon 的写锁与安装环境 [我的决定 D39]，以及 daemon 实现的 `BeforeCheckpoint` [我的决定 D44]。

各函数的语义与可能返回的 `Code`（具名拒绝都发生在该操作第一次对外可见的写之前，规则见 §6.1）：

- **`open`**：`gits` 的键与 `config.repos` 的键必须完全相同，否则 `InvalidInput`。不做 I/O。
- **`new`**：建一个会话：配置里的每个仓库都克隆成兄弟布局里的工作区，`repos` 点名的那些标为 tracked；返回 `SessionManifest`。流程见 §5.1。`create_session` 之前的具名拒绝：`InvalidInput`、`ReservedName`、`NoSuchRepo`、`NoSuchPath`、`NotDirectory`、`IsDirectory`、`NotFile`、`CrossVolume`、`Unsupported`、`Exists`；下层在这之前给出的码原样透传（gitio 的 `NoSuchBranch`、`NoSuchCommit`、`Unsupported`、`NotText`，deps 的 `Drift`、`InvalidInput`，写锁的 `Busy`，store 的码）；建基线时写到一半失败为 `Unknown`（§5.2）。`create_session` 之后的任何失败：`Unknown`，会话进入 `Unknown` 并带现场。
- **`ensure_base`**：返回 `head` 的 tree 配上它安装输入所需依赖集的基线 generation，缺失就建出来（§5.2）。这是缓存填充，不算对外可见的写（增补 6 细化 4）；它可能认领或冷装依赖集（deps 的 `ensure`），可能要几分钟。导出、`ensure` 与清单都在锁外，只有登记那一段自己取该 repo 的写锁，调用方不能持有它（module-rulings "Base generations are built outside the lock"）。码：`tree_of`、`export_tree`、deps 的 `ensure` 与 `clone_into`、overlay 的 `build_manifest`、store 的 `record_base_gen` 给出的码原样透传；`Busy`（写锁；§5.2 依赖集一再被回收，`deps_set_race`；暂存一再被 `gc_repo` 收走，`base_staging_retired`）；写到一半的失败为 `Unknown`，暂存连同写到一半的东西已退役（`kept` 为空，退役也失败时才留下它，§5.2）。
- **`ensure_base_for`**：同 `ensure_base`，只是 tree 与依赖集都由调用方给出（§5.2）：依赖集必须已经封存，它从不调 `deps::ensure`，所以从不冷装。用于提出者自换的目标"旧 tree 配新依赖集"，那个依赖集不是由 tree 推出来的。码：同 `ensure_base`，另有 `InvalidInput`（`reason: "deps_unsealed"`：依赖集没封存，或在登记之前被 gc 收走）。
- **`resolve`**：cwd 在某个会话的某个工作区里（含子目录）时返回 `Some((id, repo))`，否则 `Ok(None)`。Store 读不出来时原样透传。命令默认作用于"本会话"而得到 None 时，由调用方返回 `NoSuchSession`。见 §5.4。
- **`select`**：选择器是会话 id 就按 id 找，否则当作 ticket 找。码：`NoSuchSession`、`AmbiguousMatch`（`detail.candidates`）、`InvalidInput`（既不是 id 也不是合法的 ticket）。
- **`list`**：没有删除的会话（含 `Creating` 与 `Unknown`），按创建时间排序。只透传 Store 的码。
- **`status`**：见 §5.5。工作区缺失时报 `Missing`，本身不失败。码：`NoSuchSession`（不存在或已删除）；透传 Store、deps、gitio 的读错误。
- **`leftovers`**：列出不属于任何会话的残留（§5.5）。只读。
- **`remove`**：删除会话，删前记 `BeforeRemove` checkpoint。见 §5.6。具名拒绝：`NoSuchSession`、`InvalidInput`（会话在 `Creating` 或 `Removing`）、`NotConfirmed`、`OpenFiles`、`Dirty`；收走 `MoveLeftover` 或记 `BeforeRemove` 失败时，store 或 overlay 的码原样透传；进入 `Removing` 之后：`Unknown`。`confirm` 按值收下，核对 `action == Action::RemoveDiscard { session: id }`（types.md §4）。
- **`move_workspace`**：把一个工作区的基线与依赖一起移到 `new_base`，HEAD 移到 `head`。调用方先在锁外用 `ensure_base` 或 `ensure_base_for` 备好 `new_base`，再持该 repo 的写锁调用它；它在锁里只找这个基线，从不建。见 §5.7。具名拒绝：`NoSuchSession`、`NoSuchRepo`、`InvalidInput`（含 `base_unbuilt`：`new_base` 没有登记）、`NoSuchPath`、`NoMergeBase`、`OpenFiles`（预检或交换前复查）、`Corrupt`（所钉的依赖集或清单不见了）；会话或工作区已是未决现场时以 `Unknown` 拒绝，什么都不做；`BeforeRollout`（经 `BeforeCheckpoint`）、`begin_move` 给出的码原样透传；准备暂存时下层给出具名码（`rebase`、`project`、`attach_worktree` 等）→ 先拿走暂存、把意图关成 `Refused`，再原样透传；下层给出 `Unknown`（写到一半的 I/O 错误）→ `Unknown`，暂存与 `Preparing` 的意图留作现场，以后由这个工作区的下一次移动、`remove` 或 `recover` 收走（§5.7、§6.1）；交换开始之后：`Unknown`，工作区记成 `Unknown`。
- **`resolve_failed_move`**：处理一个被交换开始之后失败的移动留在健康 `Unknown` 的工作区（§5.9）：先核对磁盘上的现场与意图记下的进度，再按人的选择 Abandon 或 Accept。自己在短临界区里取写锁（只为移 pin），调用方不能持有它。具名拒绝：`NoSuchSession`、`NoSuchRepo`、`InvalidInput`（`lifecycle`、`not_failed`、`no_failed_move`、`scene_mismatch`）、`HeadMoved`（Accept 移 pin 的比较并交换没过）；`shadow_at`（影子仓库不见了是 `NoSuchPath`）与 store 的码原样透传；Accept 这一次移过 pin 之后的失败为 `Unknown`。
- **`repo_name`**：`RepoId` → `RepoName`。码：`NoSuchRepo`。
- **`recover`**：daemon 启动时调用一次（§5.8）。只透传 Store 的码；单项失败记进 `RecoverReport.errors`。
- **`seed_plan`**：见 §5.3。码：`IsDirectory`、`NotFile`、`InvalidInput`（显式条目）；gitio 的 `tree_contains` 与 `is_ignored_in_export`、apfs 的 `attrs_of` 给出的读错误原样透传。候选条目从不报错。
- **`seed_files`**：见 §5.3。任何失败都按 apfs 的码返回；它只在 `create_session` 之后被调用，`new` 把它的失败记成 `Unknown` 现场。

## 4. 数据与不变量

### 4.1 目录布局

```text
<paths.root>/
  bases/<repo>/<tree>/<deps>/              基线 generation = Store::base_dir(&BaseGen)：源码导出加上依赖集的克隆（增补 3）。
                                           session 建好之后改名进来、store 登记；之后只读，由 store 的 gc_repo 回收
  bases/<repo>/.staging/<32 位十六进制>/    建基线 generation 的暂存（§5.2）。tree/ 是导出的目标；gitio 的导出临时目录
                                           .export-tree-<nonce>/ 也在这里（增补 6 "Export staging"）。清单建好就登记；
                                           gc_repo 收走过了年龄下限的（store.md §5.11）
  deps/<repo>/…                            依赖集，归 deps（deps.md §4.1）
  store/…                                  归 store，含 retire_dir 用的 trash/（store.md §4.1）
  sessions/
    .repos/<session-id>/<repo>.git/        影子仓库：每个会话、每个 repo 一个（§4.2）
      worktrees/<n>/                       工作区第 n 代的管理目录，每一代一个新的
    <session-id>/                          会话根 = SessionManifest.root
      <repo>/                              工作区 = WorkspaceState.path；配置里的每个 repo 都有
        .git                               gitfile：gitdir: <shadow>/worktrees/<n>
      .strata-tmp-move-<repo>-<16 位十六进制>/  move_workspace 的暂存，即移动意图的 staging；交换之后放的是旧树（§5.7）
      .strata-tmp-restore-<repo>/          overlay 的 restore 暂存（RESTORE_TEMP_PREFIX），不是工作区（增补 5、7）
      node_modules/…                       tool 与 front 的 dev-link 在布局根自己建的，不归 strata
每个主 checkout 的 git common dir：
  refs/strata/pins/<session-id>            该会话在这个 repo 的 head（§4.2）
  refs/strata/remotes/origin/<branch>      gitio 的 fetch 写的远端跟踪 ref（gitio.md §4.1），session 经它取基线
```

- 工作区里没有 strata 建的符号链接：依赖目录、种子缓存、种子文件都用 clonefile 克隆（design §3：Turbopack 拒绝 symlink 的 `node_modules`）。基线树里本来就有的符号链接原样保留。
- 一个工作区 = 一次 `clone_tree` 克隆整个基线 generation 目录：源码导出加上依赖集 `dirs` 的克隆（增补 3 "Base generation layout"）。session 不留只有源码的基线缓存；同一个 tree 配不同的依赖集，是 `bases/<repo>/<tree>/` 下不同的 `<deps>`。
- `<session-id>/` 下的名字：`RepoName` 是工作区；以 `.strata-tmp-` 开头的是 strata 的暂存（移动的、restore 的）；其余是会话里的程序在布局根自己建的（dev-link）。`resolve` 只认工作区（§5.4）。暂存的名字是一个组件，不是任何工作区的目录名，这正是 store 的 `begin_move` 与 `retire_dir` 第 5 行要的形状。移动暂存的名字 [我的决定 D32]：前缀与 overlay 的 restore 暂存同族，随机后缀让同一工作区的两次移动不会撞名（代数 n 在 `begin_move` 之前还不知道，而 `begin_move` 要先拿到暂存路径）。
- 种子缓存只克隆给 tracked 仓库，种子文件给每个仓库 [我的决定 D11]。种子缓存只是让第一次构建不从零开始（design §4），`tracked = false` 的兄弟库是被相对路径读取的钉住副本，不在这里构建；而每个 Tauri 应用的 `src-tauri/target` 可达几十到 200 GB，克隆的条目数决定耗时（§5.1 复杂度）。种子文件（`.env`、`CLAUDE.md` 等）是运行兄弟库所需的，小，给每个仓库都复制。

### 4.2 工作区里的 git：每个会话、每个 repo 一个影子仓库（增补 2）

design §6.1 第 2 步把接法交给构建者；增补 2 定为影子仓库，gitio.md §4.2、§5.6 实现底层。布局归本文：

- **位置**：`<paths.root>/sessions/.repos/<session-id>/<repo>.git/`。只在 `new` 时经 `Git::ensure_shadow` 建，每个会话每个 repo 一个；不同会话从不共用。
- **对象**：`objects/info/alternates` 指向主 checkout 的 `<common>/objects`，读对象经它，不复制；agent 在工作区里 `git add`、`git commit` 新建的对象写进影子仓库自己的 `objects/`。
- **refs**：影子仓库自己的。agent 的 `git stash`、`git branch`、`git tag`、本地 commit 都只在本会话的影子仓库里，主 checkout 与别的会话都看不到。
- **镜像只取一次**：`info/exclude`、`info/attributes` 与复制的配置键（gitio.md §4.2 的清单，含 `core.excludesFile`、`core.attributesFile`）在 `ensure_shadow` 时取，会话存活期间从不改写。主 checkout 之后改了它们，工作区里的 ignore 与属性不变；`status` 用 `shadow_drift` 报出差别（`StatusWarning::MirrorDrift`）。
- **守卫**：`hooks/pre-push` 没有 `STRATA_PUBLISH=1` 就拒绝；影子仓库的 config 钉住 `core.hooksPath = <影子仓库>/hooks`，用户级或系统级的 `core.hooksPath` 关不掉它（gitio review N1）。它是守卫，不是安全边界（§6.5）。没有 remote。在工作区里改影子仓库的 `core.hooksPath`（husky、lefthook 的安装常这样做），或换掉、删掉 `hooks/pre-push`，守卫就失效：`status` 经 `shadow_drift` 报出（`pre_push_guard_disabled`），钉不钉回由 daemon 定（module-rulings 的 T13 条），session 不改影子仓库。
- **管理目录**：`worktrees/<n>/`，每一代一个新的。`new` 时 n = 1；每次 `move_workspace` 用 store 的 `begin_move` 分配的 n（`u64`，永不复用）。`attach_worktree` 在新树里写 gitfile、建 index；新树随交换出现在工作区路径上，git 的 HEAD 与 index 就随目录一起换了过来。`new` 之后影子仓库是 agent 能写的地方，移动时的管理目录由 gitio 在影子仓库根之下用 apfs 的 rooted 操作建（增补 8 对影子仓库的澄清）：`<shadow>/worktrees` 被换成符号链接时，`attach_worktree` 在任何写之前以 `NotDirectory` 拒绝。被换下的旧管理目录经 `retire_dir`（store 第 6 行）退役；旧树被保留时它也留着，因为核对旧树要经它的 gitfile（§5.7 第 19 步）。
- **拿 `Shadow` 的方法**：`ensure_shadow` 只在 `new`；此后（含 daemon 重启后）一律经只读的 `Git::shadow_at`。影子仓库不见了，`status` 把工作区报成 `Missing` 并给 `ShadowMissing`，`move_workspace` 以 `NoSuchPath` 拒绝；从不重建：重建会按今天的主 checkout 重新取镜像，违背"只取一次"，还会丢掉 agent 在影子仓库里的 ref 与 stash（gitio.md §5.6 的 `shadow_at`）。
- **pin ref**：主 checkout 里每个会话一个 `refs/strata/pins/<session-id>`，指向该会话在这个 repo 的 head，经 `update_ref` 与 `delete_ref` 比较并交换。主 checkout 的 `git gc` 看不到影子仓库的 HEAD，有了 pin，钉住的 commit 与 tree 就不会被回收。
- **退役**：`remove` 经 `retire_dir` 把 `sessions/.repos/<session-id>/`（本会话全部影子仓库）整体拿走（store 第 4 行）。

为什么不接到主 checkout 上，也不让会话共用一个影子仓库：

1. linked worktree 的 hooks 与 config 取自 common dir（gitrepository-layout 对 `hooks` 的说明："This directory is ignored if $GIT_COMMON_DIR is set"）。接在主 checkout 上，`pre-push` 只能装进主 checkout 的 `hooks/` 或改它的 config，都违反不变量 2。
2. refs 是共用的：接在主 checkout 上，会话里的 `git stash`、`git branch` 会出现在主 checkout 里。
3. front 的 `dev-link.sh` 会对每个干净、能 `fetch origin`、又没有未推送 commit 的兄弟库执行 `git reset --hard origin/main`（`front/dev-link.sh:13-28`）。兄弟工作区若能解析 `origin/main`，在会话里跑一次 `bun run dev`，钉住的兄弟工作区就被悄悄挪走，违反不变量 8。影子仓库没有 remote，`fetch origin` 失败，dev-link 跳过。
4. 所有会话共用一个影子仓库（本文 1788345 版的做法）同样共用 refs：A 先 stash、B 再 stash、A 再 pop，B 的改动落进 A，B 的 stash 也没了。在活着的会话下刷新共用的镜像，文件会在被忽略与 `Untracked` 之间来回翻：一个未发布的未跟踪文件变成被忽略，就离开 overlay，`rm` 不带 `--discard` 就把它删了（review C1）。所以每会话一个，镜像只取一次。

别的模块需要知道的后果：

- 在工作区里跑 `git rev-parse --git-common-dir`，得到的是本会话的影子仓库。strata 从不由工作区里的 git 推出 `RepoId`（`Git::open` 拒绝工作区，§4.4）。
- tgrep-core 的仓库身份是 `--git-common-dir` 规范路径的哈希（`tgrep-core/src/generations/mod.rs`），所以由工作区推出的身份每个会话都不同。共享基线索引不能按工作区的 common dir 取键；怎样做由 search.md 定（增补 2 的 T11 条）。
- 工作区里引用远端的命令（`git fetch origin`、`git pull`、`git log origin/main`、`build.sh` 开头的 `git pull`）都会失败，这是有意的：发布只经 strata。

### 4.3 Store 里的会话数据

表、索引与触发器都在 store.md §4.3（增补 4）；session 只经 §2 列出的具名方法读写。session 拥有下面这些词汇、转换图与含义，store 镜像它们：

- **会话的生命周期**（`SessionState`，store 的 `Lifecycle`）：
  - `Creating`：`create_session` 写下会话行与工作区行之后、目录建完之前。`create_session` 就是 `new` 的第一次对外可见的写（增补 6）。
  - `Active`：建完，可用。
  - `Removing`：`remove` 已越过它的第一次对外可见的写，正在退役目录。
  - `Removed`：已删除；行由 store 的 gc 在保留期 `keep` 之后删掉，连同 `BeforeRemove` checkpoint（增补 3：没有永久保留的行）。
  - `Unknown`：上一次操作越过了第一次对外可见的写却没做完；`scene` 存着现场。
  - 转换只有六条（增补 4）：`Creating → Active`、`Creating → Unknown`、`Active → Removing`、`Removing → Removed`、`Removing → Unknown`、`Unknown → Removing`。最后一条只给 `rm --discard`：store 见不到凭据，由 session 在 §5.6 第 2 步守住。
- **工作区的健康**（store 的 `Health`）：`Ok` 或 `Unknown`。一次 `move_workspace` 越过交换却没做完，工作区记成 `Unknown` 并带现场（`fail_move`）；这不是会话的状态（增补 4）。离开 `Unknown` 只有一条路：人看过现场之后经 `resolve_failed_move` 选 Abandon 或 Accept，store 的 `resolve_move` 带着匹配的意图把它记回 `Ok`，现场留作历史（§5.9；module-rulings "Resolving a failed move"）。`Missing` 看文件系统，从不存。
- **移动意图**（store 的 `MoveIntent`，状态 `MoveState`，增补 5 的 T9 条：session 采用 store 提出的词汇）：
  - `Preparing`：意图行已写，只建了内部暂存（暂存树与下一代管理目录），还没有任何对外可见的写。
  - `Swapping`：即将或已经交换。进入它的同一个事务里，意图行记下 `rebase` 算出的冲突路径（§2 对 store 的要求第 5 条）[我的决定 D46]。
  - 终态：`Done`（交换完成）、`Refused`（具名拒绝，暂存已拿走）、`Abandoned`（没走到交换就停下，由下一次移动、`remove` 或 `recover` 收走，§5.7 第 9 步、§5.6 第 5.1 步、§5.8；或人经 §5.9 放弃了一次失败的移动）、`Unknown`（交换开始之后失败，现场在工作区行上；人经 §5.9 把它关成 `Abandoned` 或 `Done`）。
  - 转换：`Preparing → Swapping | Refused | Abandoned`，`Swapping → Done | Unknown`，`Unknown → Abandoned | Done`（只经 §5.9，与健康 `Unknown → Ok` 在同一个事务里）。每个工作区至多一条没结束的意图；n 永不复用。关闭的意图行是历史，从不删（增补 4 细化 2）。
- **阶段**：`new`、`remove` 经 `set_session_phase` 记在会话行上，移动经 `set_move_phase` 记在意图行上；词汇见 §6.3。
- **现场**：`Scene`（§3）序列化成 JSON，进入 `Unknown` 时交给 `set_lifecycle` 或 `fail_move`；store 只核对它是 JSON。
- **种子**：`record_seeds` 在会话为 `Creating` 时每个工作区写一次，每个路径一行（`SeedRow` ↔ `SeedRecord`）。
- **清单**：`SessionManifest` 由会话行与工作区行拼成。按 types.md §4 末段，创建时间与状态不在清单里，由 `status` 报出。
- **引用**：没删除的会话，其工作区行引用各自的 `BaseGen`；store 的 gc 从不回收被引用的 generation（store.md §5.11 守卫 (c)），所以 `create_session` 之后基线目录不会在克隆途中消失（§5.1 第 13 步）。

### 4.4 RepoId 与 RepoName

- `RepoName` 是配置的表名，也是兄弟布局里的目录名；`BaseGen.repo`、清单的键、工作区路径都用它。`RepoId` 是主 checkout 的 git common dir 的规范路径（types.md §3）。
- 二者经配置一一对应：daemon 加载配置时对每张 `[repo.<name>]` 的 `path` 做 `Git::open`，`git.repo_id()` 就是它的 `RepoId`；两张表得到同一个 `RepoId` 时整份配置以 `NameTaken` 拒绝。`Git::open` 拒绝的仓库（SHA-256、reftable、git 低于 2.44，gitio.md §3）在这里就让配置加载失败，到不了 `new`（review M-c）。这些 `Git` 交给 `Sessions::open`。
- `move_workspace` 收 `RepoId`，因为它的调用方（daemon 的 `deps approve`、publish 的 rebase）以仓库身份识别仓库。session 先映射成 `RepoName`：映射不到，或者清单里没有这个仓库 → `NoSuchRepo`；`new_base.repo` 与映射出的名字不同 → `InvalidInput`（增补 1 的 T9 条）。
- 工作区的 `RepoId` 只经清单得到：工作区路径 → `RepoName` → 映射。session 从不在工作区里跑 git 推出它，因为那会得到本会话的影子仓库。

### 4.5 不变量

| # | 不变量 | 落实 design §9 |
| --- | --- | --- |
| S1 | session 经 gitio 对主 checkout 的写入只有 pin ref `refs/strata/pins/<session-id>`，以及 `new` 的 fetch 写下的对象与 `refs/strata/remotes/origin/<branch>`（gitio.md §4.1）。工作区的 refs、新对象、hooks、config 都在本会话的影子仓库里；种子文件与种子缓存只从主 checkout 读出 | 2 |
| S2 | 对每个工作区：克隆进来的依赖集就是 `base.deps`；`gitio::tree_of(head) == base.tree`；依赖集与工作区当前内容（基线 tree 加 overlay）的安装输入哈希相符，对除"提出变更后自己先换过去的会话"（design §6.4 第 3 步）以外的每个会话，这就是基线 tree 的安装输入（增补 1 对不变量 8 的改写）；base、head、依赖目录与管理目录的代数只经 `move_workspace` 的一次交换一起改变，store 里由 `finish_move` 一个事务一起记下；交换之后失败的移动，由人经 `resolve_failed_move` 的 Accept 用 store 的 `resolve_move` 一个事务补记（§5.9） | 8 |
| S3 | 基线 generation 目录与封存的依赖集永不被修改：工作区是 CoW 克隆；基线目录只以"在别处建好、改名进来、再登记"的方式出现，session 从不往已登记的基线目录里写 | 5 |
| S4 | 改写一个已有工作区之前先记 checkpoint：`move_workspace` 记 `BeforeRollout`，`remove` 记 `BeforeRemove`（只对 tracked 仓库；`new` 建的是全新的工作区）。两者都经 daemon 交来的 `BeforeCheckpoint`，每次都新记一行（§3；overlay.md §5.6） | 1 |
| S5 | 具名拒绝只发生在该操作第一次对外可见的写之前；之后的任何失败都是 `Unknown`，现场原样保留、记进 store，`status` 报出，daemon 重启之后照样报出；session 从不自己重放、清理或回滚它。只有从未对外可见的内部暂存，才由建它的操作在具名拒绝前拿走，或由之后的操作（这个工作区的下一次移动、这个会话的 `remove`、daemon 启动时的 `recover`）收走（§6.1）。现场只由人越过：会话的经 `rm --discard` 加确认；失败的移动留下的工作区现场，也可以经 `resolve_failed_move` 的 Abandon 或 Accept，它先核对磁盘上的现场与意图的记录，对不上就拒绝（§5.9） | 6 |
| S6 | `remove(…, discard = true, …)` 只接受 `action` 为 `RemoveDiscard { session: 本会话 }` 的凭据；越过任何未决状态都要它，没有任何参数、环境变量或配置能代替 | 7 |
| S7 | 依赖目录、种子缓存、种子文件从不以符号链接的形式进入工作区；session 往工作区或暂存树里的每一次写都在根之下逐级 `O_NOFOLLOW` 解析，树里的符号链接从不把写送到会话根之外（增补 3 "Rooted destinations"） | design §3、§4（为不变量 2、4、8 服务） |
| S8 | 依赖目录只在 `move_workspace` 里被替换，交换之前复查依赖目录上打开的文件；调用方只在 `deps approve` 之后、或提出变更的会话自己要求时（design §6.4 第 3 步）才传入新的依赖集 | 4 |
| S9 | 会话 id 由 40 bit 随机数生成；store 留着一个会话的行（含 `Removed`，直到 gc 按保留期删掉它）期间，`new` 撞上这个 id 一律 `Exists`；行被 gc 删掉之后再撞上的概率是 2^-40。ticket 只是标签，不进任何路径 | design §11 第 18 条 |
| S10 | 配置里的每个仓库在会话根下都有 `<repo>/`；工作区的 HEAD 是 detached 的，没有 upstream，`pre-push` 在没有 `STRATA_PUBLISH=1` 时拒绝 | design §3、§6.8 |
| S11 | 每个会话、每个 repo 一个影子仓库；它的镜像只在 `new` 时取一次，会话存活期间从不改写，不见了只报告、从不重建 | 2、8（增补 2） |

## 5. 流程与算法

下文的记号：

- 【锁 R】：session 自己经 `locks.with_write(R, …)` 取该 repo 的写锁，只罩住这一小段（`new`、`remove`、`ensure_base`、`ensure_base_for`、`resolve_failed_move` 里）。`move_workspace` 在调用方持有的锁里运行，从不自己取（`RepoWriteLock` 不可重入）。锁只罩 fetch、克隆依赖集、attach、pin、交换、登记基线这些短临界区，从不跨过导出、清单与冷装（增补 3、4 "Locks"；module-rulings "Base generations are built outside the lock"）。持锁的那一段里，session 从不调用会自己取这把锁的函数（deps 的 `ensure` 与 `observe_main`，本模块的 `ensure_base`、`ensure_base_for`）；一个线程一次至多持一把 repo 锁。所以既不会自锁，也没有锁的顺序问题（module-rulings "deps follows main's current tip"：`observe_main` 自己取锁）。`Busy` 来自写锁的有界等待，由 daemon 给出。
- rooted 调用的根：`paths.root`、主 checkout 的 `path` 由配置在加载时规范化（config.md，T15），`R` 是 `F_GETPATH` 给的真实路径；会话根、工作区、暂存树、基线目录都由 session 用 `Path::join` 从它们接上固定的分量或 `RepoPath` 拼出。所以根从不带结尾的 `/`、`.` 或 `..` 分量，apfs 的 `root_not_canonical` 拒绝（module-rulings "Rooted roots are canonical"）在这里不会出现。session 从不把用户直接给的路径当根：`resolve` 的 `cwd` 只经 `F_GETPATH` 取真实路径，不交给 rooted 调用。
- 【缓存】：缓存填充，不算对外可见的写（增补 6 细化 4）。
- 【可见】：该操作第一次对外可见的写（§6.1）。
- 【rename】：原子改名。【F】：`apfs::full_fsync`，即 `F_FULLFSYNC`（增补 3 "Durability"）。

### 5.1 `new(ticket, repos)`

**阶段 A：校验。** 只读（读配置、`lstat`、`F_GETPATH`、Store 的读），失败都是具名拒绝。

1. ticket 必须匹配 `^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`，否则 `InvalidInput`（`detail = {field: "ticket", value}`）；形如会话 id（`^[sS]-[A-Za-z2-7]{8}$`，不分大小写）→ `ReservedName` [我的决定 D1]。ticket 不再是目录名，但它出现在选择器里，`publish --from <session>:<path>` 要求它不含 `:`；`../foreign` 这类写法因此被拒（3e `packaging::lazy`）。
2. `repos` 为空 → `InvalidInput`（`field: "repos"`）；名字不在配置里 → `NoSuchRepo`（`detail.repo`）；重复的名字合并为一个。
3. 对配置里的**每个**仓库（不只是 `repos`）`lstat(path)`：不存在 → `NoSuchPath`（`detail = {repo, field: "path", path, errno}`）；是符号链接 → `NotDirectory`（`reason: "symlink"`）；是别的非目录 → `NotDirectory`（`reason: "not_a_directory"`）。`paths.root` 存在时按同样的规则检查（`field: "paths.root"`）。符号链接一律拒绝 [我的决定 D2]：同卷、互不嵌套、对主 checkout 只读这几条保证都要按真实位置成立，经符号链接的写法让人看不出真实位置（3b `:15`、3e `link` 把这个选择留给本文）。
4. 别名嵌套（review I2；增补 1 的 T9 条：别名不是配置规则，由 `new` 按 `RepoId` 的身份判定）[我的决定 D3]。真实路径一律以 `O_DIRECTORY` 打开再 `fcntl(F_GETPATH)` 取得：符号链接已解析，大小写按卷上的写法；这两个调用的 errno 经 `apfs::classify_io_error(…, IoSide::Read, …)` 归类。`R` 是 `paths.root` 的真实路径；它还不存在时，取 `apfs::nearest_existing_ancestor(&paths.root)` 的真实路径，再接上余下的字面分量。对每个仓库，`W` 是 `path` 的真实路径，`C` 是 `git.repo_id()`（common dir 的 realpath）。`R` 等于或位于 `W`、`C` 之内，或者 `W`、`C` 位于 `R` 之内 → `InvalidInput`（`detail = {rule: "nesting", repo, root: R, checkout: W}`，或以 `common_dir: C` 代替 `checkout`）。"位于之内"按路径分量的前缀判断；卷不分大小写时（`volume_caps(R).case_sensitive == false`）按 ASCII 不区分大小写比较。config.md 的 V3 只比字面路径：经符号链接、大小写不同的拼写，或者 common dir 在别处的人自己的 linked worktree，都会让 `paths.root` 落进人的 checkout，只有这一步拦得住；拦不住，session 往会话根里的每一次写都落进人的仓库。
5. 卷：对每个仓库，`apfs::same_volume(&apfs::nearest_existing_ancestor(&paths.root)?, path)` 必须为真（config.md §4.3 的规则；`paths.root` 已存在时，最近的已存在上级就是它自己），否则 `CrossVolume`（`detail = {root, repo, path}`）。这两个函数返回 `io::Result`，它们的 `io::Error` 经 `apfs::classify_io_error(…, IoSide::Read, …)` 归类之后原样返回；`apfs::volume_caps(<同上>)` 的 `fs_type` 不是 `apfs` → `Unsupported`（`detail.fs_type`），`clone == Some(false)` → `Unsupported`（`reason: "no_clone"`）。都在任何写之前判定，从不退化成复制（design §7）。
6. 来源的类型：每个仓库的**显式**种子文件（`seed_files()` 返回 `explicit = true` 时的条目）在主 checkout 里 `lstat`：目录 → `IsDirectory`（`detail = {repo, field: "seed_files", index, path}`）；FIFO、socket、设备 → `NotFile`（另带 `file_type`）。tracked 仓库的种子缓存：已存在的条目是符号链接或不是目录 → `NotDirectory`（`detail = {repo, field: "seed_caches", index, path, reason}`；config.md §4.3）。候选种子文件不在这一步判（§5.3）。只用 `lstat`，从不打开，所以 FIFO 不会让 `new` 阻塞（3e `fifo`）。
7. 会话 id [我的决定 D4]：`arc4random_buf` 取 40 bit，编码成 `s-` 加 8 位小写 base32。Store 里已有这个 id 的行（任何生命周期，含 `Removed` 而尚未被 gc 删掉的）→ `Exists`（`detail = {session, reason: "store_row"}`）；`sessions/<id>` 或 `sessions/.repos/<id>` 已存在 → `Exists`（`detail = {session, path, reason: "directory"}`）。不换个 id 重试：碰撞的概率可以忽略，真撞上了多半是 bug 或有人手工建了目录，报出来比悄悄绕开好；调用方再调一次 `new` 就会得到新 id。这一步放在阶段 A 的末尾、任何缓存填充之前，所以这个拒绝没有任何效果。

**阶段 B：钉基线、填缓存、判种子。** 每个仓库各自进行，按 `RepoName` 的顺序派发给最多 4 个并行的工作者 [我的决定 D41]。这里写下的只有缓存（增补 6 细化 4）与 gitio 对 origin 的观察；失败是具名码或原样透传。一个仓库失败，`new` 就在 `create_session` 之前停下，别的仓库已经填好的缓存留着给以后用。

8. 【锁 R】`git.fetch("origin", default_branch)`；同一段锁里 `head = deps::main_tip(cfg, git)`，放锁。`main_tip` 就是 design §6.7 的钉基线规则：`pin_unpushed = true` 且 `publish.branch == default_branch` 时，发布链 `refs/strata/publish/<default_branch>` 领先 origin 就钉链顶，与 origin 相同、落后或分叉都钉 origin 的 tip；其余一律钉 origin 的 tip（config.md §2.3 的 `publish` 与 `pin_unpushed` 两条；增补 1 的 T9/T10 条）。规则只写在 deps 一处，`observe_main` 判定主干当前的 tip 用的是同一个函数（deps.md §2 对 session 的要求第 7 条、§5.9）；它只读 ref、不取锁，在 session 持着的锁里调用不会自锁。它返回 None（fetch 之后 origin 仍没有默认分支）→ `NoSuchBranch`（config.md 的运行时检查）。`tree = git.tree_of(head)`。gitio 与 deps 的码原样透传（`NoSuchBranch`、`NoSuchCommit`、`Unsupported`……，3b `:14`）。fetch 写下的对象与 `refs/strata/remotes/origin/<default_branch>` 是 gitio 自己对 origin 的观察（gitio.md §4.1）：以后的操作都复用它，重做得到同样的结果；增补 6 把 `new` 的第一次对外可见的写定为 `create_session`，而 fetch 必须在它之前，所以本文把 fetch 当作缓存填充看待 [我的决定 D5]。
9. `gen` = §5.2 的 `ensure_base` 流程，沿用第 8 步得到的 head 与 tree：导出、`ensure` 与清单在锁外，只有登记取锁（第 8 步的锁这时已经放开）。
10. 种子计划：`plan = seed_plan(cfg, git, path, store.base_dir(&gen), &tree)`（§5.3）。显式条目的拒绝在这里发生，对着的是基线 generation（review I3）。tracked 仓库再定种子缓存：条目 `c` 在主 checkout 里存在（第 6 步已确认是真目录），它在基线 generation 目录里不存在，而且各级上级要么不存在、要么是真目录（`apfs::attrs_of(base, 上级)`，逐级 `O_NOFOLLOW`）→ 进计划；否则静默跳过 [我的决定 D12]。种子缓存只是加速，缺省列表的条目不该因为某个仓库的形状让它的每一次 `new` 都失败（与 review M10 同一个教训）；这种情形要是在 `create_session` 之后才发现，就只能报 `Unknown`。

**阶段 C：写会话行。**

11. 【可见】`store.create_session(&manifest, now)`：清单的 `root = <paths.root>/sessions/<id>`，每个仓库是 `WorkspaceState { base: gen, head, path: root/<repo>, tracked }`。store 的码原样透传：`Exists`（`store_row`：并发的同一个 id，概率 2^-40）、`InvalidInput`（某个基线在第 9 步之后被 gc 收走了；重试 `new` 即可）。这一行写失败时，事务原子地什么都不留。从这一步起，任何失败都是 `Unknown`。

**阶段 D：建工作区。** 每个阶段开始时先 `set_session_phase(id, phase)`，崩溃之后 `recover` 报出的就是正在做的那一步（词汇见 §6.3）。

12. `root`：`mkdir_rooted(paths.root, "sessions", 0o755, parents = true)`；`mkdir_rooted(paths.root, "sessions/<id>", 0o755, parents = false)`，已存在即 `Exists`，排他由 `mkdir` 本身保证（M21）；`mkdir_rooted(paths.root, "sessions/.repos/<id>", 0o755, parents = true)`。
13. 下面五个阶段依次进行，一个阶段对全部仓库做完才进入下一个，所以任何时刻会话只有一个 phase。阶段之内按 `RepoName` 的顺序把仓库派发给最多 4 个并行的工作者 [我的决定 D41]；某个仓库失败之后不再派发，等已经开始的做完，再按本节末尾"第 12–15 步的任何失败"处理，`detail.repo` 是按 `RepoName` 顺序第一个失败的仓库。
    1. `workspace`：`apfs::clone_tree_rooted(store.base_dir(&gen), root, <repo>)`，一次 clonefile 克隆源码与依赖目录（增补 3）。不取写锁 [我的决定 D13]：第 11 步写下的工作区行已经引用这个 generation，store 的 gc 不回收被引用的 generation（§4.3）。
    2. `git`【锁 R】：`shadow = git.ensure_shadow(<paths.root>/sessions/.repos/<id>/<repo>.git)`；`git.attach_worktree(&shadow, 1, &head, ws, ws)`。gitio 要求两者在锁内（gitio.md §5 开头）。
    3. `pin`【锁 R】：`git.update_ref("refs/strata/pins/<id>", &head, None)`，期望这个 ref 原本不存在。
    4. `seed_caches`（只对 tracked 仓库）：计划里的每个条目 `c`：`mkdir_rooted(ws, c 的上级, 0o755, parents = true)`；`clone_tree_rooted(path/c, ws, c)`。`node_modules/.cache` 因此落进刚克隆的依赖目录里（deps 封存时排除了它，见 `nested_seed_caches`）。
    5. `seed_files`：`records = seed_files(&plan, ws)`；`store.record_seeds(id, repo, rows)`。
14. `observe`：对每个仓库（按第 13 步的方式派发）`deps::observe_main(ctx, &tree, &head, &store.base_dir(&gen))`，记下主干当前的依赖集，id 变了就登记提议（deps.md §2 对 session 的要求第 4 条）。session 调它时不持有任何 repo 写锁：按 module-rulings "deps follows main's current tip"，`observe_main` 自己取锁，是写锁里的一小段临界区，在锁里读主干的 tip（签名以 deps 的下一轮为准，§2 对 deps 的要求）。到这一步，第 8 步的 fetch、第 9 步的登记、第 13 步 `git` 与 `pin` 取的锁都已放开，阶段又是一个接一个进行的，所以它取锁不会撞上本线程或本次 `new` 的另一个工作者持有的锁（§5 开头的【锁 R】）。它写的提议与主干记录是对外可见的写，又在 `create_session` 之后，所以它的任何失败（含 deps 的 `Unknown`，`reason = "main_not_recorded"`）都与本阶段别的失败一样：会话 `Creating → Unknown`，现场保留，不当作具名拒绝、也不清理（增补 6：`create_session` 是 `new` 的第一次对外可见的写；deps.md @22e1fac §2 对 session 的要求第 4 条、§5.9 与 §6）。放在 `finalize` 之前 [我的决定 D15]，所以会话失败时还在 `Creating`，走的是已有的那条边。
15. `finalize`：【F】对会话根与 `sessions/.repos/<id>/` 的目录句柄（`apfs::open_rooted(paths.root, …)`）各做一次 `full_fsync` [我的决定 D14]：会话一旦记成 `Active`，目录就要扛得住断电（增补 3 "Durability"）。然后 `store.set_lifecycle(id, Creating, Active, None, now)`。
16. 返回清单。daemon 随后为 tracked 仓库建 `Tracker` 并开始监听（design §6.1 第 3 步）。

第 12–15 步的任何失败：`set_lifecycle(id, Creating, Unknown, Some(scene), now)`，返回 `Unknown`（`detail` 与 `message` 见 §6.3），`scene.kept` 列出已经建出的会话根与影子仓库目录。`set_lifecycle` 本身也失败时照样返回 `Unknown`：会话行停在 `Creating`，下一次启动时 `recover` 把它记成 `Unknown`。

复杂度：

- 阶段 A：O(仓库数) 次 `lstat` 与 `F_GETPATH`，O(种子条目数) 次 `lstat`。
- 阶段 B：每个仓库一次网络 fetch（4 路并行，D41；每次是一个到 origin 的网络往返，持着这个 repo 的写锁）。基线已在缓存里时，§5.2 只是一次目录列表加一次 `key_of`（deps.md §5.13：tool 小于 20 ms）。种子计划 O(条目数 × 路径深度)。
- 阶段 D：每个仓库一次目录 clonefile，内核里按条目数线性（apfs.md 草稿实测 tool 的 `node_modules` 克隆 2.4–3.9 s），4 路并行（D41）。`attach_worktree` 的 `read-tree` 建出的 index 没有 stat 信息，随后的 `update-index --refresh` 要读一遍每个源码文件，是 O(源码字节数)（review M11；tool 约 4.2K 个文件）。种子缓存的 clonefile 按条目数线性（tool 的 `src-tauri/target` 约 76 万个条目，`cp -Rc` 实测几秒）。

验收数字：design §8 的"新建会话 < 5 s（tool + 全部兄弟库）"，指基线与依赖集都已在缓存里的情况（`new_warm_under_5s`）。第一次遇到某个 tree 的导出（秒级）、依赖集的认领（秒级）或冷装（分钟级）不在此列。代价（增补 8 的估算）：配置里的仓库合计约 62 万个依赖条目，单线程克隆 12–19 s，按实测的 4 路速率约 5.5 s。这还没算阶段 B 的 22 次网络 fetch（4 路并行，每次一个到 origin 的往返，不在增补 8 的估算里），也没算源码树、22 次 refresh 与种子缓存（tool 的 `src-tauri/target` 约 76 万个条目）。所以即使按 4 路并行，整套布局的 `new` 也很可能超过 5 s。这个目标留给用户在这一阶段结束时定，选项是并行克隆（本文的 D41）、按仓库懒装依赖或放宽目标（§6.5）；`new_warm_under_5s` 记下各阶段的耗时，供用户决定。

### 5.2 `ensure_base(repo, head)` 与 `ensure_base_for(repo, tree, deps)`：基线 generation

两个函数走同一个流程，都是缓存填充（增补 6 细化 4）。调用方在拿 repo 写锁之前调用它们，导出、依赖集与清单都在锁外，只有第 7 步的登记取锁（module-rulings "Base generations are built outside the lock"）。`new` 的第 9 步用 `ensure_base`；别的调用方见 §2。`move_workspace` 在锁里只找基线、从不建（§5.7 第 7 步）。两者只在第 1、2、5 步与第 7.1 步不同：`ensure_base` 由 tree 的安装输入定依赖集（`deps::ensure`，可能认领或冷装）；`ensure_base_for` 用调用方给的、已封存的依赖集，从不调 `deps::ensure`。提出者自换的目标"旧 tree 配新依赖集"（design §6.4 第 3 步）只能这样备好，因为那个依赖集不是由 tree 推出来的。

暂存的寿命（module-rulings "Base staging is registered right after its manifest"）：没登记的暂存只有年龄下限护着。`gc_repo` 在 repo 写锁里，把最近一次直接条目的变化已过下限（1 h）的暂存挪进 trash（store.md @0062a3e §5.11 第 5 步）。所以暂存的清单一建好就登记，从不带着它跨过认领、冷装这样的长步去等登记。第 5 步要认领或冷装时，这份暂存只作 `deps::ensure` 的输入，之后退役，另建一份再登记 [我的决定 D7]。

1. `ensure_base`：`tree = git.tree_of(head)`。`ensure_base_for`：`set = deps::find_set(store, repo, deps)` 为 None → `InvalidInput`（`detail = {reason: "deps_unsealed", repo, deps}`），什么都没写。
2. 热路径：
   - `ensure_base` [我的决定 D6]：在 `apfs::list_dir(paths.root, "bases/<repo>/<tree>")` 里找一个能解析成 `DepsSetId`、且 `has_base_gen` 为真的条目 `d`。找到就算 `(_, id) = deps::key_of(ctx, <那个 generation 目录>)`：同一个 tree 的各个 generation 目录里，源码与安装输入都相同。`has_base_gen(BaseGen { repo, tree, deps: id })` 为真就返回它：不导出、不拿锁。没找到，或 id 对应的 generation 还没有，往下。
   - `ensure_base_for`：`has_base_gen(BaseGen { repo, tree, deps })` 为真就返回它；否则往下。
3. 暂存 [我的决定 D7]：`B = bases/<repo>/.staging/<32 位十六进制>`；`mkdir_rooted(paths.root, "bases/<repo>/.staging", 0o755, parents = true)`，再 `mkdir_rooted(paths.root, B, 0o755, parents = false)`。把 `B` 记进进程内的"正在用的暂存"集合（`leftovers` 不报它）。
4. `export`（锁外）【缓存】：`git.export_tree(&tree, B/tree)`。gitio 那一侧的锁规则见 §2 对 gitio 的要求第 2 条。`dst` 总是暂存目录之下的 `B/tree`，从不是 `B` 本身，所以 gitio 建在 `dst` 父目录里的导出临时目录 `.export-tree-<nonce>/` 也在 `B` 里（增补 6 "Export staging"；store.md §5.11 守卫 (a) 对 `dst` 的要求）。`B` 每次调用一个随机名，两次导出从不撞名。`B` 没有意图行：建基线是缓存填充，不属于哪个会话，`new` 又在 `create_session` 之前做它 [我的决定 D7]。护着 `B` 的是进程内的"正在用"集合（只有建它的调用拿走它）与 store 的年龄下限（`retire_dir` 第 7 行按形状接受它，`gc_repo` 只收过了下限的）；它怎样收尾见下面的"失败"。
5. `deps`（锁外）：
   - `ensure_base_for`：用第 1 步的 `set`。
   - `ensure_base`：`(_, id) = deps::key_of(ctx, B/tree)`，`set = deps::find_set(store, repo, id)`。命中 → 第 6 步。落空 → 要认领或冷装（deps.md §5.4、§5.5），这是长步：`set = deps::ensure(ctx, B/tree)`。deps 只在开头读 `B/tree`：认领核对它的安装输入，冷装先把它克隆进自己的暂存。`ensure` 返回之后 `B` 不再登记：`retire_dir(B)`（得到 `NoSuchPath` 说明 `gc_repo` 已经收走了它，照常往下），从"正在用"的集合里去掉，回到第 2 步，另建一份；这一轮第 5 步通常命中（第 5 步与第 7.1 步合起来连续 3 轮落空 → `Busy`，见第 7.1 步）。deps 在封存的临界区里自己取写锁，所以这一步 session 不持锁。
6. `manifest`（锁外）：`manifest = overlay::build_manifest(B/tree, &tree, cfg)`，O(源码字节数)。在依赖目录克隆进来之前做；它本来也排除依赖目录与种子缓存。
7. `register`【锁 R】【缓存】，清单建好之后紧接着做，一段临界区（deps.md §5.7：`find_set` 复查与 `clone_into` 在锁里，紧挨着改名与登记）：
   1. `deps::find_set(store, repo, set.id)` 复查。落空说明 gc 在两步之间收走了一个没人引用的依赖集。`ensure_base`：放锁，`retire_dir(B)`，回到第 2 步，第 5 步会再走一次 `ensure`（deps.md §2 对 session 的要求第 3 条）；第 5 步与这里合起来连续 3 轮落空 → `Busy`（`detail.reason = "deps_set_race"`）[我的决定 D8]，与 deps 的 `proposal_race` 同一做法。`ensure_base_for`：放锁，`retire_dir(B)`，返回 `InvalidInput`（`deps_unsealed`）：它不建依赖集，要调用方重新封存。
   2. `gen = BaseGen { repo, tree, deps: set.id }`。`has_base_gen(gen)` 已为真（另一个调用在第 2 步之后先登记了它）→ 放锁，`retire_dir(B)`（store 第 7 行；里面还有导出，不是空目录），从"正在用"的集合里去掉，返回 `gen`。两个调用同时为同一个 (tree, 依赖集) 建基线时，就是这样收场：各自在锁外建好，后登记的那个退役自己的暂存（module-rulings "Base generations are built outside the lock"）。
   3. `deps::clone_into(store, &set, B/tree)`。
   4. `mkdir_rooted(paths.root, "bases/<repo>/<tree>", 0o755, parents = true)`；【rename】`rename_rooted(paths.root, "bases/<repo>/.staging/<x>/tree", "bases/<repo>/<tree>/<set.id>", RenameMode::Excl)`。得到 `Exists` 说明那里有一个没登记的残留（上一次改名之后、登记之前崩溃）：`store.retire_dir(base_dir(gen))`（store 第 1 行），再改名一次。
   5. 【F】对 `bases/<repo>/<tree>/` 与 `B` 的目录句柄（`apfs::open_rooted(paths.root, …)`）各做一次 `full_fsync`。
   6. `store.record_base_gen(&gen, &manifest, now)`。放锁。
8. 这时 `B` 应当是空的：`remove_rooted(paths.root, B, RemoveKind::EmptyDir)`（增补 7：操作可以删自己建的空目录）。得到 `NotEmpty`（apfs 把 `ENOTEMPTY` 归成它，module-rulings "NotEmpty from apfs"；导出成功时 gitio 已删掉它的临时目录，所以不该发生）就改用 `retire_dir(B)`。从"正在用"的集合里去掉，返回 `gen`。

`B` 已被 `gc_repo` 收走：第 7.3、7.4 步得到 `NoSuchPath`，或某一步失败之后退役 `B` 时得到 `NoSuchPath`（失败正是因为 `B` 不见了）。`gc_repo` 只在 repo 写锁里挪暂存，所以这只会发生在 `B` 建好之后、第 7 步拿到锁之前；这时什么都没登记，也没有任何对外可见的写。放锁（持着时），从"正在用"的集合里去掉，回到第 2 步重建一次；同一次调用里第二次遇到 → `Busy`（`detail = {reason: "base_staging_retired", repo, tree}`）。别处退役 `B` 得到 `NoSuchPath`（第 5 步认领或冷装之后、第 7.1、7.2 步）只说明它已经在 trash 里，照常往下走。

失败：

- 第 1–2 步：原样透传，什么都没写。
- 第 3 步起的任何失败，先拿走暂存：`retire_dir(B)`（store 第 7 行）。第 7.4 步的改名可能已经做了时，改名进来、没有登记的 `base_dir(gen)` 也用 `retire_dir` 拿走（第 1 行）。写到一半的导出或克隆是缓存填充的中间产物，随 `B` 一起离开，不是要保留的现场（module-rulings "Base staging is registered right after its manifest"：gitio "中断的导出保留临时目录"只对导出到可见位置的情形）。然后：
  - 具名码（gitio 在写之前的拒绝、deps 的具名码、`build_manifest` 的 `NotText` 与 `Unsupported`、`record_base_gen` 的 `InvalidInput`、`Busy`、`ensure_base_for` 的 `deps_unsealed`）：原样返回。调用之前的状态原样恢复，只多了已经封存的依赖集这类缓存。
  - 写到一半的失败（导出中途、`clone_into` 中途、改名或同步得到 `EIO`、`ENOSPC`）：返回 `Unknown`（`detail = {op: "base", repo, tree, phase, path: B, cause, kept: [], retired: [B …]}`）。什么都没留下，修好原因之后重试即可 [我的决定 D7]。
  - 退役本身失败：`B` 原样留着，从"正在用"的集合里去掉，返回 `Unknown`（`kept: [B]`），`leftovers()` 从此报出它。daemon 下一次启动时 `recover` 把它退役（§5.8）；在那之前，`gc_repo` 过了年龄下限也会收走它。
  - `record_base_gen` 返回 `Unknown`：登记与否说不准。改名进来的目录不退役（登记了的 generation 只由 `gc_repo` 按保留期回收），`B` 已空，照第 8 步删掉；返回 `Unknown`（`kept: [base_dir(gen)]`）。没登记的话，`gc_repo` 过了年龄下限收走它（store.md §5.11 第 5 步）。

`move_workspace` 不建基线 [我的决定 D31]。它在调用方的锁里运行，目标基线没登记就以 `InvalidInput`（`reason: "base_unbuilt"`）拒绝，在任何写之前（§5.7 第 7 步）。调用方在拿锁之前调本节的函数：deps approve 先 `ensure_base(repo, P.head)` 再 `ensure_base_for(repo, &P.tree, &P.to)`，提出者自换用 `ensure_base_for(repo, <plan_switch 的目标 tree>, &P.to)`，publish 的 rebase 按 publish.md 选其一（§2 的调用方）。进程内不合并同时到来的同一个基线的调用：第 7.2 步让它们各自收场，代价只是一次多余的导出与清单。

复杂度：热路径是一次目录列表加一次 `key_of`（`ensure_base_for` 只是一次 `has_base_gen`）。冷路径是导出 O(文件数 + 字节数)、一次 `key_of`、清单 O(源码字节数)、`clone_into` O(依赖目录条目数)、改名 O(1)；要认领或冷装依赖集时另加一次 `ensure`（认领秒级，冷装分钟级）与第二次导出。写锁只罩第 7 步：复查、`clone_into`、改名、同步、登记；其中 `clone_into` 按依赖目录的条目数线性（tool 的 `node_modules` 约 2.4–3.9 s，apfs.md 的实测），是这段临界区里最长的一步。导出、`ensure` 与清单从不在锁里（增补 3、4 "Locks"；module-rulings "Base generations are built outside the lock"）。

### 5.3 种子文件：`seed_plan` 与 `seed_files`

判定规则（增补 1 的 T9 条；config.md §2.3、§4.3）。条目表由 `RepoConfig::seed_files()` 给出，它返回 `(条目, 是否显式)`。配置里没写 `seed_files` 时，条目是 design §4 的默认四项（`CLAUDE.md`、`.env`、`.env.local`、`.claude/settings.local.json`），每一项都是**候选**；写了时，表里的每一项都是**显式**的硬要求，即使名字与默认四项之一相同。

`seed_plan(cfg, git, main, base, tree)` 对每个条目 `p`（序号 `index`）依次判定 [我的决定 D9]：

1. **来源**：`lstat(main/p)`。不存在 → 静默跳过。普通文件 → 继续。符号链接 → 经 `apfs::attrs_of(main, p)` 读出 `FileKind::Symlink { target }`，继续；主 checkout 里 `p` 的某一级上级是符号链接（`attrs_of` 逐级不跟随，给 `NotDirectory`）→ 显式：`NotDirectory`（`reason: "symlink"`）；候选：跳过（module-rulings "A symlink is recreated, never cloned"；§5.3 末尾的 `seed_files`）。目录 → 显式：`IsDirectory`；候选：跳过。FIFO、socket、设备 → 显式：`NotFile`（`detail.file_type`）；候选：跳过。候选在下面任何一条不满足时同样静默跳过，不报错、不告警（review M10：上一版把类型检查排在候选过滤之前，一个是目录的缺省项就能让 `new` 失败）。
2. **被跟踪**：`git.tree_contains(tree, p)` 为真（`p` 是 tree 里的文件或符号链接，或是被跟踪的目录）→ 显式：`InvalidInput`（`detail = {repo, field: "seed_files", index, rule: "seed_files", reason: "tracked"}`）；候选：跳过。
3. **落点**（增补 3 "Rooted destinations"）：`p` 的每一级上级 `a`，在基线 generation 目录里 `apfs::attrs_of(base, a)`：不存在或是真目录 → 可以；是符号链接或不是目录 → 显式：`InvalidInput`（`reason: "parent_not_directory"`，`detail.parent = a`）；候选：跳过。`p` 本身在基线目录里已存在 → 显式：`InvalidInput`（`reason: "exists_in_base"`）；候选：跳过。这一条保证之后根之下逐级 `O_NOFOLLOW` 的写一定成功，而不是在 `create_session` 之后才撞上一个被跟踪的符号链接、只能报 `Unknown`。"已存在"与"同一个文件"是磁盘同一性的问题，按卷折叠，不按字节，也不按 `core.ignorecase`（module-rulings "Disk identity follows the volume, git identity follows core.ignorecase"）：`volume_caps(base).case_sensitive` 为假的卷上，`attrs_of` 的逐级查找本身就按卷折叠，种子 `.env` 落在被跟踪的 `.ENV` 上，算作已存在，不会覆盖它；同一份条目表里两个条目的 `case_fold_key` 相同时，后一个与前一个是同一个文件：显式 → `InvalidInput`（`reason: "duplicate"`，`detail.other` 是前一个的序号）；候选：跳过 [我的决定 D9]。大小写敏感的卷上按 `RepoPath` 的字节比较。第 2 步的"被跟踪"与第 4 步的"被 ignore"是 git 的看法，照 gitio 的规则。
4. **被 ignore**：`git.is_ignored_in_export(base, tree, p)`（§2 的上游要求）为假 → 显式：`InvalidInput`（`reason: "not_ignored"`）；候选：跳过。判定与工作区建好之后 git 的判定相同：基线里的 `.gitignore`，主 checkout 现在的 `info/exclude`（即 `ensure_shadow` 将要镜像的那份），以及全局排除文件 `core.excludesFile`（增补 1 的 T5 条）。
5. 通过 → 一条 `SeedSource`：`source = main/p`；是符号链接时 `link` 记第 1 步经 `attrs_of` 读出的目标；`size` 与 `source_mtime` 取自第 1 步的 `lstat`。

`seed_files(plan, to)`：对每条 `SeedSource`，`mkdir_rooted(to, p 的上级, 0o755, parents = true)`。普通文件：`apfs::clone_file_rooted(source, to, p, None)`。符号链接按目标重建，从不克隆、从不跟随：`apfs::symlink_rooted(to, p, link)`，目标字节就是计划里记下的那份（module-rulings "A symlink is recreated, never cloned"）[我的决定 D10]。复制出来的文件与主 checkout 的互相独立（CoW），权限位照来源。`copied` 是复制完成的时刻。它只在 `create_session` 之后被调用，任何失败都由 `new` 记成 `Unknown`；来源在计划与复制之间被换了类型（例如换成 FIFO）也一样。

种子文件不进 checkpoint：它们被 ignore，overlay 本来就跳过；即使会话改了 `.gitignore`，让某个种子文件不再被 ignore，overlay 的排除集合也照样把它挡在外面（overlay.md §5.1 的第 3 项）。`status` 每次都对种子文件重查 ignore，不再被 ignore 时给 `SeedNotIgnored`（§5.5）。

### 5.4 `resolve`、`select`、`list`

`resolve(cwd)` [我的决定 D16]：

1. 以 `O_DIRECTORY` 打开 cwd，`fcntl(F_GETPATH)` 取真实路径：符号链接已解析，大小写按卷上的写法。打不开 → `Ok(None)`。
2. 真实路径不在 `<paths.root>/sessions/` 的真实路径之下 → None。
3. 下一段不能经 `SessionId::try_from` 解析 → None。`.repos` 在这一步落空，所以影子仓库里的路径不属于任何工作区。
4. 再下一段不是该会话清单里的某个 `RepoName` → None。逐字节比较：目录是 session 按 `RepoName` 建的。会话根本身、dev-link 建的 `node_modules/`、`.strata-tmp-move-*`、`.strata-tmp-restore-*` 在这一步落空。
5. 查 Store：会话存在、不是 `Removed`、清单里有这个仓库 → `Some((id, repo))`，否则 None。`Creating` 与 `Unknown` 的会话照样识别出来：拿到它的命令（例如 checkpoint）由各自的检查拒绝，而 `status` 正需要认出它。Store 读不出来 → `Err`，原样透传，不吞成"不在会话里"。

复杂度 O(1)，daemon 不在时也能用。

`select(selector)`：

1. 能经 `SessionId::try_from` 解析 → 按 id 找；不存在或已 `Removed` → `NoSuchSession`（`detail.selector`）。
2. 否则按 §5.1 第 1 步的 ticket 规则校验，不合格 → `InvalidInput`。合格时，找 ticket 逐字节相等、不是 `Removed` 的会话：0 个 → `NoSuchSession`；1 个 → 它；多个 → `AmbiguousMatch`（`detail = {ticket, candidates}`，按创建时间排序）。

ticket 不会与 id 混淆：`new` 以 `ReservedName` 拒绝会话 id 形式的 ticket。

`list()`：不是 `Removed` 的会话，含 `Creating` 与 `Unknown`，按创建时间排序 [我的决定 D17]：未决的现场要能被列出来，人才看得到它、才能 `rm --discard` 它。

### 5.5 `status(id)` 与 `leftovers()`

`status(id)` 只读，不取写锁（读者不排在写锁后面，module-rulings T13）：

1. `store.session(id)`：没有，或是 `Removed` → `NoSuchSession`（已删除会话的 id 对所有调用都过期，3d `:219`）。`state` 由 `Lifecycle` 逐项映射，`phase` 照抄；`scene` 只在 `state == Unknown` 时报出（store 在会话离开 `Unknown` 之后仍留着它，作为历史）。
2. 每个工作区：
   1. `health`：工作区行是 `Unknown` → `Unknown`，带它的 `scene`。否则 `lstat` 工作区目录与它的 `.git`，再 `git.shadow_at(<影子仓库>)`：任何一个不在 → `Missing`；不在的是影子仓库时另给 `ShadowMissing { path }` [我的决定 D20]。这一项直接看文件系统，不按记录推断（3d `:317`–`:319`）；影子仓库只报告、从不重建（§4.2）。
   2. `generation`：工作区行的代数。
   3. tracked：`checkpoint` 取 `latest_checkpoint(id, repo)` 的 id。`tracked = false`：给 `NotCheckpointed`，`checkpoint` 为 None（增补 1 的 T9 条）。
   4. `drift`：工作区存在、所钉依赖集的 `dirs` 非空时，`pinned = deps::find_set(store, repo, base.deps)`，`drift = deps::drift(cfg, git, ws, &pinned)`。按 types.md 的 `Drift` 行，这是数据，不是错误。
   5. `pending`：tracked 工作区，`deps::open_proposals(store, Some(repo))` 里状态为 Pending、且 `base.deps ≠ P.to` 的提议（deps.md §4.5；增补 6 的 T9 条）。发布后 rebase 因为新基线需要别的依赖集而没有移动时登记的那条提议也在其中（不变量 4）。
   6. `conflicts`：工作区行记下的、最近一次移动的冲突路径。
   7. `seeds`：每条种子行。主 checkout 里来源的 mtime 与 `source_mtime` 不同、或来源已不在 → `SeedStale` [我的决定 D18]；`git.is_ignored(ws, p)` 为假 → `SeedNotIgnored`。工作区不在时不查 [我的决定 D19]：`is_ignored` 要经工作区的 `.git` 读规则，`Missing` 已经说明了问题。
   8. `MirrorDrift`：影子仓库在时，`git.shadow_drift(&shadow)` 非空 → 一条告警，每项一个 `MirrorDriftItem`：`Exclude` → `info_exclude`，`Attributes` → `info_attributes`，`Config { key }` → `config` 加 `key`，`PrePushGuardDisabled { hooks_path, hook_replaced }` → `pre_push_guard_disabled` 加这两个字段（gitio @ac4149f：husky、lefthook 这类安装常把 `core.hooksPath` 改掉）。session 只报告，不钉回；要不要钉回由 daemon 定（module-rulings 的 T13 条）。
   9. 移动意图（`store.move_intents(id)` 里这个仓库的）：`Preparing` → `MoveLeftover { n, staging }`；`Abandoned` 而它的 `staging` 目录还在（§5.9 第 6 步没能退役）→ 同样是 `MoveLeftover`；`Unknown` 的意图不另报：工作区健康已是 `Unknown`，带着现场（第 2.1 步），daemon 据此给出 Abandon 与 Accept 两种处理（§5.9）；`Done` 而 `old_tree` 有值，或者它的 `staging` 目录还在（§5.7 第 19 步没走完）→ `OldTreeKept`（`path` 取 `old_tree`，没有时取 `staging`；`retained` 即 `old_tree` 有值），`holders = deps::open_files(path)`，`changed` 在读的时候重算 [我的决定 D22]：找这次移动的 `BeforeRollout` checkpoint，即 `checkpoints(id, Some(repo))` 里原因为 `BeforeRollout`、时刻不晚于意图 `started` 的最新一个；用 `overlay::reconcile_full` 冷扫旧树（`ws.root` 取旧树，它的 gitfile 仍指向旧管理目录；基线取那个 checkpoint 的 `base`），条目与 checkpoint 的条目不同即 `changed = true`。
   10. `RestoreScene`：`<会话根>/.strata-tmp-restore-<repo>/` 存在且非空（增补 7）。里面的 `plan.json` 归 overlay，由 daemon 读出并入回复。
3. 返回 `SessionStatus`。

工作区当前的改动（`changed`）不在 `SessionStatus` 里 [我的决定 D21]：它由 overlay 对着 daemon 持有的 `Tracker` 算（`overlay::compute`：属性缓存加脏集合，交换之后先冷扫一次），由 daemon 在读 status 时算出并入（增补 3 "Projection"：`status.changed` 读时重算；review I9：上一版取最新 checkpoint 的条目，交换之后一直显示 `BeforeRollout` 的旧条目，直到下一次 checkpoint）。overlay 的告警（特殊 mode 位等）、publish 的未决推送也由 daemon 并入。

复杂度：每个工作区几次 `lstat`，一次安装输入哈希（deps.md：tool 小于 20 ms），一次 `shadow_drift`（gitio：两次子进程），每个种子文件一次 `is_ignored`（通常只有几个）；不遍历工作区。每棵保留的旧树另加一次进程扫描（deps.md §5.13：约 800 个进程时小于 1 s）与一次冷扫（O(源码字节数)），所以旧树多的会话 `status` 会慢；只因为有进程持有而保留的旧树由以后的移动收走（§5.7 第 20 步），其余随 `remove` 退役。

`leftovers()`：对每个仓库 `list_dir(paths.root, "bases/<repo>/.staging")`，不在"正在用"集合里的每个条目报成 `Leftover::BaseStaging { repo, path, export_temps }`，`export_temps` 是其中名字以 `EXPORT_TEMP_PREFIX` 开头的目录（gitio 失败的导出；增补 1 的 T5 条：status 报出它）。只读。依赖集尝试的现场由 deps 的 `attempts` 报出，daemon 把两者并进 status（review M22）。

### 5.6 `remove(id, discard, confirm)`

**阶段 A：检查。** 都是具名拒绝，什么都不写。

1. `store.session(id)`：没有或已 `Removed` → `NoSuchSession`。`Creating` 或 `Removing` → `InvalidInput`（`detail = {reason: "lifecycle", state}`）[我的决定 D23]：生命周期图里没有从这两个状态出去的 `remove` 边；它们只在另一个操作进行中（daemon 按会话排队，不会与本调用并发）或进程崩溃之后出现，后者由下一次启动的 `recover` 记成 `Unknown`，之后可以 `rm --discard`。
2. `discard = true` 时，`confirm` 必须是 `Some`，且 `token.action == Action::RemoveDiscard { session: id }`，否则 `NotConfirmed`（`detail = {expected, reason: "missing" | "mismatch"}`）。凭据是否用过、是否过期，由 daemon 在交给 session 之前经 confirm 校验（types.md §4）。`discard = false` 时不看 `confirm`，凭据随函数返回被丢弃。`Unknown → Removing` 这条边只在 `discard` 通过这一步之后放行（§4.3）。
3. 打开的文件：`deps::open_files(会话根)` 与 `deps::open_files(sessions/.repos/<id>)`。有进程打开其中的文件、cwd 在其中、或 mmap 了其中的文件 → `OpenFiles`（`detail.open` 列出 pid、命令、路径与持有方式）。session 不发信号、不等待；`discard` 绕不过这一条，`remove` 也没有 force 参数 [我的决定 D24]（3d `fp32` 三行）。
4. `discard = false` 时，下列任一成立 → `Dirty`，`detail` 分项列出：
   1. `unresolved`：会话是 `Unknown`；某个工作区是 `Unknown` 或 `Missing`；某个 restore 现场目录非空（增补 7，`detail` 写出它的路径）。越过任何未决状态都要 `--discard` 加人类确认，从未成为 `Active` 的会话（`new` 留下的 `Unknown`）也不例外（3d `:144`；review M15）。没走到交换的移动留下的暂存（`MoveLeftover`）不算未决状态：它从未对外可见，工作区还是原样，第 5 步把它收走 [我的决定 D33]。
   2. `repos`：tracked、存在且健康的工作区，`overlay::reconcile_full(ws, base)` 非空（读遍每个候选文件，以它为真相；只读，不写 checkpoint）。这一次不经 daemon 的 Tracker [我的决定 D44]：`rm` 只动一个会话，一次全量对账（design §8：tool 小于 3 s）换来不依赖任何缓存的答案；module-rulings 让 Tracker 承担的是 Before* checkpoint，那是每次 rollout 对每个会话都要做的。每个仓库最多列 `DIRTY_PATHS_CAP` 个路径，超出时 `truncated = true` [我的决定 D25]：`detail` 要能放进一条 JSON 回复，完整的列表在 `status` 与随后的 checkpoint 里。
   3. `untracked_repos`：会话里有 `tracked = false` 的仓库。那里的编辑没有 checkpoint，strata 无从知道删了会不会丢东西（增补 1 的 T9 条）。
   4. `old_trees`：保留的旧树里 `changed` 为真的（算法同 §5.5 第 2.9 步）。交换之后写进旧树的东西没有 checkpoint（review I5）。

**阶段 B：收尾与 checkpoint。** 增补 5 细化 3：对当前、未改动状态的安全 checkpoint 不算对外可见的写；收走内部暂存也不算（增补 3）。这一阶段之后仍可具名拒绝。

5. 收尾与 checkpoint：
   1. 没走到交换的移动意图：这个会话的每条 `Preparing` 意图 m，照 `recover` 的做法收走（§5.8 第 2 步）。会话是 `Active` 时【锁 R】`retire_dir` 它的暂存与管理目录 `worktrees/<m>`（不存在就跳过）；会话是 `Unknown` 时不单独退役，它们随第 8 步的会话根与影子仓库一起离开（store 的第 5、6 行只接受 `Active` 的会话）。再 `set_move_state(…, m, Preparing, Abandoned)`。暂存还在的 `Abandoned` 意图（Abandon 之后没能退役的，§5.9 第 6 步）同样退役它的暂存与管理目录，状态不变。这是收走内部暂存，不是对外可见的写（增补 3）。失败 → 原样透传，意图还是原来的状态，什么都没删。
   2. 会话是 `Active` 时，对每个存在且健康的 tracked 工作区：`before.checkpoint(&wsref, &base, CheckpointReason::BeforeRemove)`，即经 daemon 交来的 `BeforeCheckpoint`，用它为这个工作区持有的 Tracker（module-rulings "Session's Before* checkpoints use the daemon's Tracker"）；记下 `checkpoint.id`。Before* checkpoint 总是新记一行（overlay.md §5.6 第 4 步；review M-a），所以这个号就是这次删除前的状态。保留期由 store 的 gc 管（design §6.6）。失败 → 原样透传，什么都没删。会话是 `Unknown` 时不记 [我的决定 D26]：它的工作区可能是半成品，扫描会失败，而人已经经 `--discard` 确认过丢弃。

**阶段 C：删。** 第 6 步写成之后，任何失败都是 `Unknown`。

6. 【可见】`set_lifecycle(id, 当前状态, Removing, None, now)`，当前状态是 `Active` 或 `Unknown`。返回 false（状态在读与写之间变了；按会话排队时不会发生）→ `InvalidInput`（`reason: "lifecycle_changed"`）。
7. `unpin`：对每个仓库【锁 R】：`c = git.read_ref("refs/strata/pins/<id>")`，有值就 `git.delete_ref(pin, &c)`（比较并交换，review M20），没有就跳过 [我的决定 D27]。比较的是刚读到的值，不是清单里的 head：交换之后、`finish_move` 之前崩溃的工作区，pin 已经指向新 head，清单还记着旧 head。
8. `retire`：`store.retire_dir(会话根)`，再 `store.retire_dir(<paths.root>/sessions/.repos/<id>)`（store 第 4 行）。目录不存在（`NoSuchPath`，例如 `new` 在建会话根之前失败）就跳过 [我的决定 D27]。两次改名之后，会话根与本会话全部影子仓库都已离开活路径。session 自己什么都不删：trash 由 store 的 gc 过了 1 小时用 `apfs::remove_tree_rooted` 删掉（store.md §5.11；增补 8），删的时候不跟随符号链接，带 `uchg` 的条目由它清掉旗标再删，所以会话根里 dev-link 建的 `node_modules/tailwindcss` 这类指向外面的链接只删链接本身。
9. `record`：`set_lifecycle(id, Removing, Removed, None, now)`。返回 `RemoveReport`。

第 7–9 步的任何失败：`set_lifecycle(id, Removing, Unknown, Some(scene), now)`，返回 `Unknown`；之后的 `remove` 需要 `--discard` 加确认。

复杂度：阶段 A 是两次进程扫描（各小于 1 s），加上每个 tracked 工作区一次全量对账（`reconcile_full`，读遍候选文件），O(源码字节数)（design §8：tool 全量对账 < 3 s）。第 5.2 步的 `BeforeRemove` 经 daemon 的 Tracker 是一次属性遍历（tool 温热时约 0.20 s），没有 Tracker 时 O(源码字节数)。阶段 C 是两次目录改名，O(1)；删除由 store 的 gc 在后台做。

### 5.7 `move_workspace(session, repo, new_base, head)`

调用方先在锁外用 `ensure_base` 或 `ensure_base_for` 备好 `new_base`（§5.2），再持该 repo 的写锁调用它；它在锁里只找这个基线，从不建（module-rulings "Base generations are built outside the lock"）。daemon 按会话排队，这个会话没有并发的 checkpoint、restore、`remove` 或 `resolve_failed_move`（module-rulings T7、T13）。下文的 `ws` 是工作区路径，`root` 是会话根，`base` 与 `ws.head` 是工作区当前的基线与 head。第 13、14、16、17、18 步开始时先 `set_move_phase(…, n, <阶段名>)`；第 19–20 步在 `finish_move` 之后，意图已经结束，不再记阶段。

**阶段 A：检查。** 都是具名拒绝，什么都不写。

1. `name = repo_name(repo)`（`NoSuchRepo`）；`new_base.repo != name` → `InvalidInput`（`detail = {reason: "repo_mismatch", repo_id, name, base_repo}`）。
2. 会话：没有或已 `Removed` → `NoSuchSession`；`Unknown` → 以 `Unknown` 拒绝，`detail.scene` 是记下的现场，什么都不做 [我的决定 D29]；`Creating` 或 `Removing` → `InvalidInput`（`reason: "lifecycle"`）[我的决定 D30]（review M13：上一版对不是 `Active` 的会话一律答 `NoSuchSession`，而它明明存在）。
3. 工作区：清单里没有 → `NoSuchRepo`；`tracked = false` → `InvalidInput`（`reason: "untracked_repo"`）[我的决定 D28]：增补 1 规定这类仓库从不移动，没有现成的码表达"这个仓库不能移动"，按共同规则映射到最接近的 `InvalidInput`。工作区行是 `Unknown` → 以 `Unknown` 拒绝，带它的现场。`ws` 或 `ws/.git` 不在 → `NoSuchPath`；`shadow = git.shadow_at(<影子仓库>)` 得到 `NoSuchPath` → `NoSuchPath`（`reason: "shadow_missing"`）；`<root>/.strata-tmp-restore-<name>/` 非空 → 以 `Unknown` 拒绝（`reason: "restore_scene"`，与 overlay 的 restore 前提相同）[我的决定 D29]。
4. 目标与当前相同（`base == new_base` 且 `ws.head == head`）→ 返回 `checkpoint = None` 的 `MoveReport`，什么都不写。
5. `git.tree_of(head) != new_base.tree` → `InvalidInput`（`reason: "head_tree"`）：`WorkspaceState` 要求 `tree_of(head) == base.tree`（types.md §4）。
6. `git.merge_base(ws.head, head)` 为 None → `NoMergeBase`（types.md §6.2）。
7. 依赖集与基线：`set = deps::find_set(store, name, new_base.deps)` 为 None → `InvalidInput`（`reason: "deps_unsealed"`）[我的决定 D31]；`old_set = deps::find_set(store, name, base.deps)` 为 None → `Corrupt`（`reason: "deps_set_missing"`：被引用的依赖集不该消失）；`store.has_base_gen(&new_base)` 为假 → `InvalidInput`（`detail = {reason: "base_unbuilt", base: new_base}`）[我的决定 D31]：基线由调用方在拿锁之前建好，`move_workspace` 在锁里从不建（module-rulings "Base generations are built outside the lock"）。是否该换、换成哪个，是调用方（deps 的计划、publish 的契约）的事。session 不核对 `new_base.deps` 是否等于新 tree 的安装输入，因为提出变更的会话自换时（design §6.4 第 3 步），目标正是"旧 tree 配新依赖集"（deps.md `plan_switch`）。
8. 预检：`D` 是 `cfg.deps_dirs`、`old_set.dirs`、`set.dirs` 三者的并集；对 `ws` 下 `D` 里每个存在的目录调 `deps::open_files`，有 → `OpenFiles`（design §6.4 第 5 步：不在 dev server 脚下换）。
9. 这个工作区有一条 `Preparing` 的意图 m（上一次移动没走到交换就停下，`MoveLeftover`）→ 照 `recover` 的做法收走它 [我的决定 D33]：`retire_dir` 它的暂存与它的管理目录 `worktrees/<m>`（store 第 5、6 行；不存在就跳过），再 `set_move_state(…, m, Preparing, Abandoned)`。调用方持有写锁，而每次移动都在这把锁里，所以这条意图的主人已经不在运行。暂存还在的 `Abandoned` 意图（§5.9 第 6 步）同样退役它的暂存与管理目录，状态不变。

**阶段 B：基线。** 只读。

10. `old = Base { gen: base, manifest: store.manifest(name, base.tree), dir: base_dir(base) }`，`new` 同样由 `new_base` 组成。清单为 None → `Corrupt`（`reason: "manifest_missing"`）：已登记的 generation 都有清单，`record_base_gen` 把两者一起写下。

**阶段 C：checkpoint。** 不变量 1 要求的那一次；增补 5 细化 3：它不算对外可见的写。

11. `cp = before.checkpoint(&wsref, &old, CheckpointReason::BeforeRollout)`：经 daemon 交来的 `BeforeCheckpoint`，用它为这个工作区持有的 Tracker，做不依赖事件、由 `unchanged_since` 作保的全量遍历；daemon 还没有这个工作区的 Tracker 时才读遍每个候选文件（module-rulings "Session's Before* checkpoints use the daemon's Tracker"、"Cold scan"）[我的决定 D44]。它的条目就是要投影的 overlay。失败 → 原样透传。

**阶段 D：意图行与内部暂存。** 意图行在建任何暂存之前写下（增补 3）。

12. `S = <root>/.strata-tmp-move-<name>-<16 位十六进制>`；`n = store.begin_move(session, &name, &new_base, &head, &S, now)`。失败原样透传，这时什么都没建。
13. `prepare`：
    1. `apfs::clone_tree_rooted(base_dir(new_base), root, <S 的名字>)`：新基线的源码与新依赖集的目录一次克隆出来。
    2. `rebased = overlay::rebase(&Overlay::from_checkpoint(&cp.checkpoint), &old, &new, store, &RebaseOptions::for_move(&new, ignorecase)?)`，`ignorecase` 取 `git.read_index(ws)` 的 `ignorecase`（增补 5 的 T9 条）。
    3. `overlay::project(store, &rebased.overlay, S)`：mode 0o100644 与 0o100755 的文件、由 blob 内容（即目标）建出的 0o120000 符号链接，根之下逐级不跟随（增补 3 "Projection"、增补 7 "Rooted projection"；review I9）。
    4. 携带被 ignore 的内容（§5.7.1）。
    5. `a_n = git.attach_worktree(&shadow, n, &head, S, ws)`：新管理目录 `worktrees/<n>/` 与 `S/.git`；refresh 是 O(源码字节数)（review M11）。`<shadow>/worktrees` 被换成符号链接时它在任何写之前给 `NotDirectory`，按本节末尾的具名码处理：移动被拒绝，什么都没写进影子仓库之外（增补 8 对影子仓库的澄清）。session 不修影子仓库：在有人把 `worktrees` 改回目录之前，这个工作区的每次移动都这样被拒绝，错误的 `detail` 指出它。
    6. 【F】对 `S` 与 `root` 的目录句柄（`apfs::open_rooted(root, <S 的名字>)` 与 `apfs::open_root(root)`）各做一次 `full_fsync` [我的决定 D42]：交换之后，新树必须扛得住断电（增补 3 "Durability"；`project` 不做同步，留给调用方）。
14. `recheck`：紧挨着交换之前，对 `ws` 下 `D` 里每个存在的目录再调一次 `deps::open_files`（增补 1 的 T8 条；review M18：上一版把复查放在写意图之前，中间还隔着几步）。有 → `retire_dir(S)`、`retire_dir(a_n)`（store 第 5、6 行），再 `set_move_state(…, n, Preparing, Refused)`，返回 `OpenFiles`。工作区一字不动，第 11 步的 checkpoint 留作历史。先拿走暂存、再关意图：两步之间崩溃，意图还是 `Preparing`，下一次移动或 `recover` 会把它关掉（store.md §5.5）。
15. `set_move_state(…, n, Preparing, Swapping)`，紧挨着第一次对外可见的写。同一个事务里把 `rebased.conflicts` 记在意图行上（§2 对 store 的要求第 5 条）[我的决定 D46]：交换之后失败、进程又崩溃时，冲突只能从这里找回（§5.9 的 Accept）。

**阶段 E：交换。** 第 16 步开始之后，任何失败都是 `Unknown`。

16. `swap`：【可见】【rename】`apfs::rename_rooted(root, <S 的名字>, name, RenameMode::Swap)`：工作区路径原子地换成新树，旧树换到 `S`。然后【F】对 `root` 的目录句柄（`apfs::open_root(root)`）做 `full_fsync`。新树的 gitfile 指向 `worktrees/<n>/`，git 的 HEAD 与 index 随目录一起换了过来。
17. `pin`：`git.update_ref("refs/strata/pins/<session>", &head, Some(&ws.head))`。
18. `record`：`store.finish_move(session, &name, n, &rebased.conflicts, now)`：一个事务里把 base（tree 与依赖集一起）、head、代数与冲突换掉，意图记成 `Done`（不变量 8）。

第 16–18 步的任何失败：`store.fail_move(session, &name, n, &scene, now)`（工作区与意图都记成 `Unknown`），返回 `Unknown`（§6.3）。新旧两棵树、两个管理目录都原样保留。之后由人看过现场，经 `resolve_failed_move` 选 Abandon 或 Accept（§5.9），或者 `rm --discard`。

**阶段 F：旧树。** 交换已经持久；这一阶段（`old_tree`）的失败不改变移动的结果，只反映在 `MoveReport.old_tree` 与 `status` 里。

19. 旧树现在在 `S`，它的 gitfile 仍指向旧管理目录 `worktrees/<旧代数>/`。`holders = deps::open_files(S)`；`unchanged` 指 `overlay::reconcile_full`（`ws.root = S`，基线 `old`）的条目与 `cp` 的条目相同。`holders` 为空、`unchanged` 为真、`uncarried` 为空 → `retire_dir(S)`（store 第 5 行：`Done` 意图的 staging）与 `retire_dir(worktrees/<旧代数>)`（第 6 行），`old_tree = None`。否则把旧树留在原地，`old_tree = Some(S)` [我的决定 D35]：
    - `uncarried` 非空或 `unchanged` 为假（旧树里有新树没有的东西）：`set_old_tree(session, &name, n, &S)`。这样的旧树以后不被自动收走（`retained = true`），随 `remove` 一起退役。
    - 只因为有进程持有：不记 `old_tree`。`Done` 意图的 `staging` 还在，`status` 照样报出它（`retained = false`），以后的移动在它没人持有、也没被改过时收走它（第 20 步）。

    留在原地而不改名，是为了交换之后不再添一步可能半途而废的目录操作；`S` 由 `Done` 意图点名，`retire_dir` 随时拿得走它。
20. 同一工作区以前只因为有进程持有而保留的旧树（其它 `Done` 意图里 `old_tree` 为空、`staging` 还在的）：没有进程持有、且与各自的 `BeforeRollout` checkpoint 一致的（算法同 §5.5 第 2.9 步），连同各自的管理目录一并退役 [我的决定 D35]。agent 的 cwd 多半就在工作区里，几乎每次移动都会这样保留一棵旧树；它们就这样逐次被收走，不会一直累积。
21. 返回 `MoveReport { session, repo: name, from: base, from_head: ws.head, to: new_base, head, checkpoint: Some(cp.checkpoint.id), dropped: rebased.dropped, conflicts: rebased.conflicts, lww: rebased.lww, uncarried, old_tree }` [我的决定 D36]。daemon 随后对这个工作区 `Tracker::reset` 再 `watch`（增补 5）。

第 13 步的失败（这时还没有任何对外可见的写，增补 3）分两种 [我的决定 D33]：

- 下层给出具名码（`rebase` 的、`project` 的 `Corrupt` 或 `NotDirectory`、携带放到位时的 `ReadOnly`（§5.7.1）、`attach_worktree` 的 `Exists` 或 `NotDirectory`（`<shadow>/worktrees` 是符号链接或文件）……）：照第 14 步的做法，先 `retire_dir(S)`，管理目录建了的话再 `retire_dir(a_n)`，然后 `set_move_state(…, n, Preparing, Refused)`，返回这个码。调用之前的状态原样恢复。
- 下层给出 `Unknown`（克隆、投影、携带、attach、同步写到一半得到 `EIO` 或 `ENOSPC`，apfs 把这些归为 `Unknown`），或者上一种情形里拿走暂存本身失败：返回 `Unknown`（`detail = {op: "move", phase: "prepare", …, kept: [S]}`）。暂存与 `Preparing` 的意图原样留着，`status` 报 `MoveLeftover`；工作区还是原样，健康仍是 `Ok`。这个工作区的下一次移动（第 9 步）、这个会话的 `remove`（§5.6 第 5.1 步）或 daemon 下一次启动时的 `recover`，把它关成 `Abandoned`、暂存退役。

理由：写到一半的暂存里有什么，调用方说不准，所以不当作具名拒绝；但它从未对外可见，只有诊断价值，而返回的错误已经带着原因，所以由下一个操作收走（增补 3：`recover` 收走没走到第一次对外可见的写的意图）。不收走它，这个工作区以后的每次移动都会撞上 `begin_move` 的 `Exists`。

#### 5.7.1 携带被 ignore 的内容

新树由新基线加投影后的 overlay 组成，会话自己的被 ignore 的东西（种子文件、种子缓存、`.next/`、`dist/` 这类构建产物、空目录）不在其中，要从旧工作区带过来 [我的决定 D34]：

1. 用 `apfs::walk_with` 遍历 `ws`，剪掉根下的 `.git` 与 `D` 里的依赖目录。条目路径 `p` 是旧基线清单里的路径、`cp` 条目的路径或某条 Renamed 的 `from` → 不携带，投影负责它；目录下面还有清单或 overlay 的路径时往下走。
2. 其余的条目是"外来"的，按 `attrs_of(S, p)` 与它在旧树里的种类处理：
   - `S` 里在 `p` 上已经是目录、`p` 也是目录 → 往下走。
   - `S` 里在 `p` 上已经有别的东西（新基线在同一路径上新增了被跟踪的文件，或投影放了东西）；或 `p` 的某一级上级在 `S` 里是符号链接或不是目录；或 `p` 是 FIFO、socket、设备；或 `p` 是对属主不可写的目录（改名到别的父目录要求对它本身有写权限，apfs 会给 `ReadOnly`）；或 `p` 对属主不可读（mode 缺 u+r：apfs 打开源时得 `EACCES`，归为 `Unknown`）→ 不携带，记进 `uncarried`；它留在旧树里，旧树因此被保留（第 19 步）。携带从不覆盖新树里的任何东西：一个被 ignore 的本地文件要是盖掉一个新的被跟踪文件，它就会以 `Modified` 进 overlay，变成可发布的改动。
   - 否则携带 [我的决定 D34]：缺的上级用 `mkdir_rooted(S, p 的上级, 0o755, parents = true)`。普通文件与目录：`apfs::clone_from_rooted(ws, p, <S>/.strata-carry-<16 位十六进制>, kind)` 克隆到 `S` 根下的一个私有临时名（目录整块克隆，不再往下走），`kind` 按第 1 步遍历看到的种类取 `CloneKind::File` 或 `CloneKind::Dir`，源一侧逐级钉住、不跟随符号链接；再 `rename_rooted(S, <临时名>, p, RenameMode::Excl)` 放到位，目的地一侧同样逐级不跟随（增补 7、8 的做法）。符号链接按目标重建，从不克隆（module-rulings "A symlink is recreated, never cloned"）：`apfs::attrs_of(ws, p)` 逐级不跟随地读出 `FileKind::Symlink { target }`，再 `apfs::symlink_rooted(S, p, target)`。不用按路径的 `clone_file_rooted(<ws>/p, …)`：它的源一侧跟随中间级的符号链接，旧树里一个被换成符号链接的上级就能把它引到会话之外；`clone_from_rooted` 又不接受符号链接。
   - 遍历之后、克隆之前源变了（与 agent 的写并发），不算移动失败：不见了（`NoSuchPath`）就跳过，没有东西要带；种类变了（apfs 在克隆之前给 `IsDirectory`、`NotFile` 或 `NotDirectory`，即 `CloneKind` 对不上；符号链接是 `attrs_of` 读到的不再是符号链接）或某一级上级成了符号链接（`attrs_of` 与 `clone_from_rooted` 都给 `NotDirectory`），就记进 `uncarried`，它留在旧树里（module-rulings "clone_from_rooted states its source kind"、"A symlink is recreated, never cloned"）。从不从会话之外复制东西进来。apfs 在克隆之前拒绝，所以这时临时名下什么都没有。测试在遍历之后停住造出这种竞争（接缝位置 `move.carry`，§3）。
   - 克隆把源的 `uchg`、`uappnd` 旗标与扩展属性原样带到临时名上（apfs 不清旗标）。`Attrs` 不带旗标，第 2 步看不出来；带 `uchg` 或 `uappnd` 的条目在放到位的那次改名上得 `EPERM`，apfs 归为 `ReadOnly`，按第 13 步的具名失败处理：移动被拒绝，暂存退役（临时名在暂存里一起退役，store 的 gc 用 `remove_tree_rooted` 清掉旗标再删），`detail.path` 指出这个条目。session 不替人清旗标 [我的决定 D34]。
3. 嵌套在依赖目录里的种子缓存（`deps::nested_seed_caches(cfg)`，如 `node_modules/.cache`）在第 1 步被剪掉了，单独按第 2 步的规则携带。依赖目录本身来自新依赖集，不携带；dev-link 下一次运行时会重新镜像 `node_modules/@inter/*`。

携带不进 blob store [我的决定 D34]：被 ignore 的内容可能是几百 GB 的构建产物，逐个文件存成 blob 再取回来，要把它们全部读一遍、算一遍哈希，还把构建产物塞进 blob store。module-rulings "Carry is not a capture" 改正了增补 8 把"session 的携带"列在 `put_blob_rooted` 之下的说法：携带是把旧工作区的条目克隆进新树，不是往 store 里捕获；`put_blob_rooted` 的调用方只是往 store 里捕获的（overlay 的 checkpoint）。从工作区读时不跟随符号链接，由 `clone_from_rooted` 与 `attrs_of` 本身保证。

复杂度（review M14）：遍历是 O(依赖目录与整块克隆的目录之外的条目数)，tool 约 4.2K 个源码文件；整块克隆在内核里按条目数线性，主要是 `src-tauri/target`：tool 的约 76 万个条目，`cp -Rc` 实测几秒（2026-10-21，本机 USB SSD 上约 200 GB 的 `target`）。design §8 写"依赖 rollout = 克隆 + rename，秒级"；对带着这种目录的工作区，"秒级"取决于这一次克隆，首个 spike 实测之后修正（`move_workspace_seconds_warm`）。不用改名来携带：改名会把目录从旧树里搬走，交换之前的具名拒绝或崩溃就得把它搬回去，而搬回去之前，旧工作区已经少了一块，那是对外可见的改动。

复杂度合计：第 11 步在 daemon 有 Tracker 时是一次属性遍历（tool 的源码树温热时约 0.20 s，overlay.md §5.6），没有时 O(源码字节数)；第 13 步是几次 clonefile、O(overlay 条目数) 的投影加上冲突文件的三方合并、携带、O(源码字节数) 的 refresh；第 16 步 O(1)；第 19 步一次进程扫描加一次冷扫。

### 5.8 `recover()`

daemon 启动时调用一次，先于 gc 与任何请求；这时没有进行中的操作。

1. `store.sessions()` 里 `Creating` 的 → `set_lifecycle(id, Creating, Unknown, Some(scene), now)`，`scene = { op: New, phase: 最后记下的 phase, kept: 存在的会话根与影子仓库目录, error: Unknown 且 detail.reason = "interrupted" }`；`Removing` 的同样记成 `Unknown`，`op: Remove`。
2. `store.open_move_intents()`：`Preparing` 的 → 会话是 `Active` 时 `retire_dir` 它的暂存与管理目录 `worktrees/<n>`（不存在就跳过）；会话不是 `Active` 时不单独退役，它们随会话根与影子仓库一起离开（store 的第 5、6 行只接受 `Active` 的会话）；再 `set_move_state(…, Preparing, Abandoned)`（增补 3、增补 4 细化 2）。`Swapping` 的 → `fail_move(…, scene)`，`scene` 列出工作区与暂存两个位置，工作区记成 `Unknown`，之后由人经 §5.9 处理。
3. 对每个仓库，`bases/<repo>/.staging/` 下的每个条目 → `retire_dir`（store 第 7 行）[我的决定 D7]：它们是缓存填充的内部暂存，从未对外可见。`NoSuchPath`（`gc_repo` 已经收走）不算失败。daemon 不重启时，漏下的由 `gc_repo` 过了年龄下限收走（store.md §5.11 第 5 步）。
4. 别的一概不碰：不删除工作区，不重放，不替上次完成交换（不变量 6）。
5. 单项失败记进 `RecoverReport.errors`，接着处理下一项 [我的决定 D37]：一项坏掉的现场不该让 daemon 起不来；这些错误由 daemon 写进日志并在 status 里报出。返回 `RecoverReport`。

### 5.9 `resolve_failed_move(session, repo, choice)`：处理一次失败的移动

交换开始之后失败的移动，把工作区留在健康 `Unknown`（§5.7 第 16–18 步、§5.8 第 2 步）。overlay 拒绝在这样的工作区上 restore（overlay.md @77e2e0c §3 的 `restore` 与 §6.3），这一条不变；store 也没有从 `Unknown` 回到 `Ok` 的边。所以在这之前，人只剩 `rm --discard` 一条路（review M-b）。module-rulings "Resolving a failed move"：现场保留并报出（不变量 6），人看过之后可以清掉它。daemon 的 status 给出两种处理（T13）；cli 用一个显式命令收下人的选择，没有选择就不运行（T14）。daemon 在这个会话的队列里调用本函数，不持锁；它只为移 pin 自己取一小段写锁 [我的决定 D45]。

**阶段 A：核对。** 只读，失败都是具名拒绝，什么都不写。

1. 会话：没有或已 `Removed` → `NoSuchSession`；不是 `Active` → `InvalidInput`（`reason: "lifecycle"`）。工作区：清单里没有 → `NoSuchRepo`；健康不是 `Unknown` → `InvalidInput`（`reason: "not_failed"`）。影子仓库：`git.shadow_at(<影子仓库>)` 的码原样透传（不见了是 `NoSuchPath`，`status` 报 `ShadowMissing`）。
2. 意图：`m` 是 `store.move_intents(session)` 里这个仓库代数最大的那条；它的状态不是 `Unknown` → `InvalidInput`（`reason: "no_failed_move"`）。记下 `n = m.n`、`S = m.staging`、`P = m.phase`、`h1 = m.head`，以及工作区行的代数 `g` 与 head `h0`（`finish_move` 没跑，它们还是移动之前的）。
3. 现场：`k_ws = git.worktree_gitdir(ws)`、`k_S = git.worktree_gitdir(S)`（§2 对 gitio 的要求第 3 条），`p = git.read_ref("refs/strata/pins/<session>")`。`attach_worktree` 在 gitfile 里写的是管理目录的绝对路径（gitio.md §5.6），session 交给它的又是自己按会话根的真实路径拼出的路径，所以按字节比较。两种可能的现场：
   - **没换**：`k_ws` 是 `<影子仓库>/worktrees/<g>`，`k_S` 是 `<影子仓库>/worktrees/<n>`，`p == h0`（移 pin 在交换之后）。
   - **已换**：`k_ws` 是 `<影子仓库>/worktrees/<n>`，`k_S` 是 `<影子仓库>/worktrees/<g>`，`p` 是 `h0` 或 `h1`（交换之后、移 pin 之前失败时还是 `h0`）。

   `h0 == h1`（提出者自换，head 不变）时 pin 分不出两种现场，由两份 gitfile 定。
4. 与意图记下的进度核对：`P` 是 `recheck` 或 `swap` 时交换可能没做（阶段名 `swap` 在改名之前记下）；`P` 是 `swap`、`pin` 或 `record` 时可能做了。`Abandon` 要求现场是"没换"且 `P ∈ {recheck, swap}`；`Accept` 要求现场是"已换"且 `P ∈ {swap, pin, record}`。不符 → `InvalidInput`（`detail = {reason: "scene_mismatch", choice, n, phase: P, found: {workspace_gitdir: k_ws, staging_gitdir: k_S, pin: p}, matches}`），什么都不写；`matches` 是现场与记录合得上的那一种（`"abandon"`、`"accept"`），哪种都合不上时为 null。现场与记录对不上，说明有人动过它，要人重新看 [我的决定 D45]。

**阶段 B：Abandon。** 交换没有发生，工作区还是原来的树。

5. `record`：`store.resolve_move(session, &name, n, false, now)`（§2 对 store 的要求第 4 条）：一个事务里意图 `Unknown → Abandoned`、工作区健康 `Unknown → Ok`；base、head、代数与冲突不变，现场留作历史。这是它第一次对外可见的写。失败 → store 的码原样透传，什么都没变。
6. `retire`：收走暂存，`retire_dir(S)`，再 `retire_dir(<影子仓库>/worktrees/<n>)`（store 第 5、6 行接受 `Abandoned` 的意图）。这一步的失败不改变结果，就像移动的第 19 步：没退役的路径写进 `ResolutionReport.leftover`，`status` 报 `MoveLeftover`，由这个工作区的下一次移动或这个会话的 `remove` 收走（§5.7 第 9 步、§5.6 第 5.1 步）[我的决定 D45]。它们是内部暂存，从未对外可见。

**阶段 C：Accept。** 交换已经发生，工作区是新树。

7. `pin`：`p == h1` 时跳过（含 `h0 == h1`）。否则（`p == h0`）【锁 R】`git.update_ref(pin, &h1, Some(&h0))`：这是第一次对外可见的写。比较并交换没过（两次读之间 pin 被动过）→ `HeadMoved` 原样返回，什么都没写。
8. `record`：`store.resolve_move(session, &name, n, true, now)`：一个事务里意图 `Unknown → Done`；工作区的 base、head、代数换成意图的 `to`、`head`、`n`；冲突换成意图行在进入 `Swapping` 时记下的那份（§5.7 第 15 步，D46）；意图的 `old_tree` 记成 `S`；健康 `Unknown → Ok`。失败：第 7 步这一次移了 pin → `Unknown`（`detail = {op: "resolve", session, repo, n, phase: "record", cause, kept: [ws, S]}`），工作区仍是 `Unknown`，原来的现场不变；pin 已经在 `h1`，再 Accept 一次时第 7 步跳过它。第 7 步跳过了 → store 的码原样透传：事务什么都没留，这一次调用什么都没写（§6.1 规则 1）。
9. 旧树留在 `S`，`status` 报 `OldTreeKept { retained: true }`。交换时没带过来的内容（`uncarried`）没有记下，所以旧树不被自动收走，随 `remove` 退役（§5.7 第 19 步的规则）。daemon 之后对这个工作区 `Tracker::reset` 再 `watch`（§2 对 daemon 的要求第 4 条）。

10. 返回 `ResolutionReport { session, repo: name, choice, n, base, head, old_tree, leftover }`。

复杂度：两次 gitfile 读、一次 ref 读、至多一次 ref 写与一个 store 事务；不扫工作区。

## 6. 失败与恢复

### 6.1 错误时机

本节落实不变量 6，以及增补 3 的 "Error timing, refined"、增补 4 细化 2、增补 5 细化 3、增补 6 细化 4，对 session 精确规定"对外可见的写"。

- **算对外可见的写**（增补 3）：工作区目录与它下面的一切；活的 gitfile 与它指向的管理目录；Store 里除了本操作自己的意图行之外的行（会话行与工作区行、种子行、生命周期与健康的改动）；ref；主 checkout。各操作的第一次对外可见的写：
  - `new`：`create_session`（§5.1 第 11 步，增补 6）；
  - `move_workspace`：交换（§5.7 第 16 步）；
  - `remove`：进入 `Removing`（§5.6 第 6 步）；
  - `resolve_failed_move`：Abandon 是 store 的 `resolve_move`（§5.9 第 5 步），Accept 是移 pin，pin 已经移过时是 `resolve_move`（§5.9 第 7、8 步）。
- **不算**：
  - 缓存填充（增补 6 细化 4）：建基线 generation（锁外的导出、`deps::ensure` 的认领或冷装与封存、清单，锁里的改名、登记）与它的暂存 `B`；`rebase` 存下的合并结果 blob；`new` 的 fetch 写下的对象与 `refs/strata/remotes/origin/<branch>` [我的决定 D5]。
  - `BeforeRollout`、`BeforeRemove` checkpoint（增补 5 细化 3）。
  - 移动意图点名的内部暂存：暂存树 `S` 与还没换入的下一代管理目录 `worktrees/<n>/`。意图行在建它们之前写下（增补 3）。
  - 收走别的操作留下的内部暂存，并把它的意图关成 `Abandoned`（§5.7 第 9 步、§5.6 第 5.1 步、§5.8），或收走 Abandon 之后没能退役的暂存（§5.9 第 6 步）。
- **规则**：
  1. 第一次对外可见的写之前：具名拒绝，或原样透传下层的码，调用之前的状态原样恢复。建了内部暂存的，先拿走暂存（`retire_dir`，空目录则 `remove_rooted`），再把意图关成 `Refused`，然后返回（增补 3、增补 4 细化 2）。
  2. 例外：内部暂存写到一半时下层给出 `Unknown`（`ENOSPC`、`EIO` 这类）。移动暂存里有什么说不准，所以返回 `Unknown`，暂存原样留着，由 `status` 报成 `MoveLeftover`，之后由下一个操作或 `recover` 收走 [我的决定 D33]。基线暂存不同：写到一半的导出或克隆是缓存填充的中间产物，随暂存一起退役，返回的 `Unknown` 里 `kept` 为空；只有退役本身失败时它才留下，由 `leftovers()` 报出（§5.2；module-rulings "Base staging is registered right after its manifest"）[我的决定 D7]。工作区与会话的状态都没变。
  3. 第一次对外可见的写之后：任何失败都是 `Unknown`。现场原样保留，`Scene` 记进 store（会话行经 `set_lifecycle`，工作区行经 `fail_move`），`status` 报出，daemon 重启之后照样报出。session 从不自己重放、清理或回滚它。越过它要人：会话的现场经 `rm --discard` 加人类确认（S6）；失败的移动留下的工作区现场，还可以经 `resolve_failed_move` 的 Abandon 或 Accept，它先核对磁盘上的现场与意图的记录（§5.9）。
  4. 意图行从不删除，只被关进终态（`Done`、`Refused`、`Abandoned`、`Unknown`），由 store 的 gc 按保留期回收（增补 4 细化 2）。`Unknown` 的意图只经 §5.9 再被关成 `Abandoned` 或 `Done`。

### 6.2 失败表

`new`（`ensure_base`、`ensure_base_for` 单独调用时，按其中"建基线"三行；`ensure_base_for` 另有依赖集没封存的 `InvalidInput`）：

| 失败 | 码 | 现场 | 恢复 |
| --- | --- | --- | --- |
| 阶段 A：ticket、`repos`、路径与符号链接、别名嵌套、卷、来源类型、id 冲突 | 具名码（§5.1 第 1–7 步） | 无 | 改正后重试；重试得到新 id |
| fetch、链顶判定、`tree_of` | gitio 的码，原样透传 | 无（fetch 写下的对象与远端跟踪 ref 是 gitio 对 origin 的观察，以后复用） | 重试 |
| 建基线：具名码（导出之前的拒绝、deps 的 `Drift`、`build_manifest` 的 `NotText`……），写锁的 `Busy` | 原样透传 | 无：暂存已退役；已经封存的依赖集留作缓存 | 修好后重试 |
| 建基线：写到一半的 I/O 错误 | `Unknown`（`detail.op = "base"`，`kept` 为空） | 无：暂存连同写到一半的导出或克隆已退役；退役也失败时暂存留下，`leftovers()` 报出 | 空出空间后重试；留下的暂存由 `recover` 或 `gc_repo` 收走 |
| 建基线：暂存在登记之前被 `gc_repo` 收走（`clone_into`、改名或退役得到 `NoSuchPath`） | 重建一次；再被收走 → `Busy`（`reason = "base_staging_retired"`） | 无 | 重试 |
| 显式种子文件：类型、被跟踪、上级不是目录、未被 ignore | `IsDirectory`、`NotFile`、`InvalidInput` | 无（基线已建好，是缓存） | 改配置或主 checkout 之后重试 |
| `create_session` | store 的码（`Exists`、`InvalidInput`……） | 无（事务是原子的） | 重试 |
| `create_session` 之后：`ENOSPC`、`EIO`、克隆中途、attach、pin、种子复制、`observe_main` 的任何失败（含 `main_not_recorded`）、进程崩溃 | `Unknown` | 会话 `Unknown`，带最后的 phase 与现场；会话根、已建的工作区、影子仓库、管理目录、pin，以及 `observe_main` 已登记的提议，都原样保留 | 看 `status`；`rm --discard` 加确认之后重新 `new`；主干记录由下一次观察补上 |

`move_workspace`：

| 失败 | 码 | 现场 | 恢复 |
| --- | --- | --- | --- |
| 仓库、会话、名字不匹配、生命周期、`tracked = false`、head 与 tree 不符、依赖集没封存、工作区或影子仓库不在 | `NoSuchRepo`、`NoSuchSession`、`InvalidInput`、`NoSuchPath` | 无 | 调用方改参数，或看 `status` |
| 会话或工作区已是未决现场，或 restore 现场在 | `Unknown`（拒绝，什么都不做） | 原有现场不变 | 人处理：失败的移动留下的工作区经 `resolve_failed_move`（§5.9），其余经 `rm --discard` |
| 新旧 head 没有共同祖先 | `NoMergeBase` | 无 | — |
| 预检：依赖目录上有打开的文件（第 8 步） | `OpenFiles` | 无 | 停掉 `detail.open` 里的进程（dev server）后重试。多会话的 `deps approve` 里按条目返回，其余会话照常移动（types.md §6.2） |
| 目标基线没有登记 | `InvalidInput`（`reason = "base_unbuilt"`） | 无 | 调用方先在锁外 `ensure_base` 或 `ensure_base_for`，再持锁重试 |
| `BeforeRollout`、`begin_move` 失败 | 原样透传 | 无；checkpoint 留作历史 | 重试 |
| 准备暂存时下层给出具名码 | 原样透传 | 无：暂存与新管理目录已退役，意图 `Refused` | 看码 |
| 准备暂存时写到一半的 I/O 错误 | `Unknown`（`phase = "prepare"`） | 暂存 `S` 与 `Preparing` 的意图；工作区不变、健康仍是 `Ok`，`status` 报 `MoveLeftover` | 下一次移动、`remove` 或 `recover` 收走；重试 |
| 复查：交换之前依赖目录上有打开的文件（第 14 步） | `OpenFiles` | 无：暂存与新管理目录已退役，意图 `Refused`；checkpoint 留作历史 | 同预检 |
| 交换开始之后（第 16–18 步）：`EIO`、pin 的 `HeadMoved`、store 写失败、进程崩溃 | `Unknown` | 工作区健康 `Unknown` 带现场；新旧两棵树、两个管理目录、意图都保留；pin 可能已经移动 | 看 `status`；人选 Abandon 或 Accept，经 `resolve_failed_move` 核对现场之后清掉它（§5.9）；或 `rm --discard`。overlay 的 restore 在健康 `Unknown` 时一直拒绝，所以 `BeforeRollout` checkpoint 要等健康回到 `Ok` 才能用来恢复 |
| 旧树仍被使用、被改过，或有没带过来的内容 | 不是错误 | 旧树留在原地：`MoveReport.old_tree`，`status` 的 `OldTreeKept` | 停掉进程。只因为有进程持有而保留、之后也没被改过的旧树，由以后的移动收走；其余的随 `remove` 退役，有改动的要 `--discard` |
| 第 19–20 步（旧树的核对与退役）失败 | 不是错误：移动已经完成 | 旧树留着，按 `OldTreeKept` 报出 | 同上 |

`remove`：

| 失败 | 码 | 现场 | 恢复 |
| --- | --- | --- | --- |
| 会话不存在或已删除 | `NoSuchSession` | 无 | — |
| 会话在 `Creating` 或 `Removing` | `InvalidInput`（`reason = "lifecycle"`） | 无 | 等进行中的操作结束；崩溃留下的由 `recover` 记成 `Unknown`，之后 `rm --discard` |
| `discard` 没有凭据，或凭据与本会话不符 | `NotConfirmed` | 无 | 经 cli 加 TTY 与 LocalAuthentication 重来 |
| 有进程打开会话里（含影子仓库）的文件、cwd 在其中、或 mmap 了其中的文件 | `OpenFiles` | 无；目录一字不动，不发信号 | 停掉 `detail.open` 里的进程后重试；`--discard` 也绕不过 |
| 未发布的改动、`tracked = false` 的仓库、未决现场、有改动的旧树 | `Dirty` | 无 | 先发布或恢复；或 `--discard` 加确认 |
| 收走 `MoveLeftover`、`BeforeRemove` checkpoint 失败 | 原样透传 | 无（收走的只是内部暂存） | 重试 |
| 进入 `Removing` 之后（第 7–9 步） | `Unknown` | 会话 `Unknown` 带 phase 与现场；已退役的目录在 store 的 trash 里，剩下的 pin 与目录原样 | 看 `status`；之后的 `remove` 需要 `--discard` 加确认 |

`resolve_failed_move`：

| 失败 | 码 | 现场 | 恢复 |
| --- | --- | --- | --- |
| 会话不存在或已删除、不是 `Active`；仓库不在清单里；影子仓库不在 | `NoSuchSession`、`InvalidInput`（`lifecycle`）、`NoSuchRepo`、`shadow_at` 的码（`NoSuchPath`） | 无 | — |
| 工作区健康不是 `Unknown`，或它最新的意图不是 `Unknown` | `InvalidInput`（`not_failed`、`no_failed_move`） | 无 | 没有要处理的失败移动 |
| 磁盘上的现场与意图的记录对不上，或与人的选择不符 | `InvalidInput`（`reason = "scene_mismatch"`，`detail.matches` 指出合得上的那一种） | 无：什么都没写 | 重新看现场；按 `matches` 重选，或 `rm --discard` |
| Abandon：store 的事务失败 | store 的码，原样透传 | 无 | 重试 |
| Abandon：退役暂存失败 | 不是错误 | 暂存留着，`ResolutionReport.leftover` 列出，`status` 报 `MoveLeftover` | 下一次移动或 `remove` 收走 |
| Accept：移 pin 的比较并交换没过 | `HeadMoved` | 无 | 重新看现场 |
| Accept：这一次移了 pin 之后，store 的事务失败 | `Unknown`（`detail.op = "resolve"`） | 工作区仍是 `Unknown`，原来的现场不变；pin 在新 head | 再 Accept 一次：pin 已在新 head，跳过它 |
| Accept：pin 本来就在新 head，store 的事务失败 | store 的码，原样透传 | 无：这一次什么都没写 | 重试 |

`recover` 的单项失败记进 `RecoverReport.errors`，那一项原样留着，下一次启动再试；`status`、`resolve`、`select`、`list`、`leftovers` 只读，失败时原样透传下层的码，不留任何东西。

### 6.3 `Unknown` 的 `detail` 与 `message`

`new` 留下现场时返回的 `Unknown`（3b `:13` 留给本文定）：

```json
{
  "op": "new",
  "session": "s-k3q7xm2a",
  "phase": "workspace",
  "repo": "tool",
  "path": "/Volumes/GWD/inter/.strata/sessions/s-k3q7xm2a/tool",
  "cause": { "errno": 28, "message": "No space left on device (os error 28)" },
  "kept": ["/Volumes/GWD/inter/.strata/sessions/s-k3q7xm2a"]
}
```

对应的 `message` 是：`new s-k3q7xm2a: workspace failed after writing began; kept /Volumes/GWD/inter/.strata/sessions/s-k3q7xm2a; nothing was rolled back, cleaned up or retried; see git-strata status s-k3q7xm2a`。

- `detail` 的字段固定：`op`、`phase`、`cause`、`kept`，除 `base` 之外都带 `session`。`new` 另带出错的 `repo` 与 `path`；`base` 带 `repo`、`tree`、`path`（暂存 `B`）、`retired`（已经退役的暂存）；`move` 带 `repo`、`n`、`from`、`to`、`staging`；`remove` 带 `retired`（已经改名进 trash 的路径）；`resolve` 带 `repo`、`n`。
- `op` 取 `"new"`、`"base"`、`"move"`、`"remove"`、`"resolve"`。`phase` 的取值固定，也就是测试接缝的阶段名（去掉操作前缀，§3）：
  - `new`：`root`、`workspace`、`git`、`pin`、`seed_caches`、`seed_files`、`observe`、`finalize`；
  - `base`：`export`、`deps`、`manifest`、`register`；
  - `move`：`prepare`、`recheck`、`swap`、`pin`、`record`（`old_tree` 阶段的失败不返回错误）；
  - `remove`：`unpin`、`retire`、`record`；
  - `resolve`：`pin`、`record`、`retire`（只有 Accept 移过 pin 之后才会返回 `Unknown`，这时 phase 是 `record`，§5.9 第 8 步；`retire` 的失败不返回错误）。
- `cause`：下层给出的是 `StrataError` 时就是那个错误（`code`、`message`、`detail`）；是 I/O 错误时是 `errno` 加原文。
- `message` 的模板：`<op> <session>[/<repo>]: <phase> failed after writing began; kept <kept 里的路径，逗号分隔>; nothing was rolled back, cleaned up or retried; see git-strata status <session>`。在第一次对外可见的写之前的两种改用：`move` 的 `prepare` 是 `move <session>/<repo>: prepare failed before anything visible changed; kept <路径>; the next move, rm or daemon restart collects it`；`base` 是 `base <repo> <tree>: <phase> failed before anything visible changed; the staging was retired; retry when the cause is fixed`，退役也失败时把后半句换成 `kept <B>; the next daemon restart collects it`。
- 同一个 `StrataError` 存进 `Scene.error`（`new`、`move` 交换之后、`remove`；`resolve` 不新记现场，工作区原来的现场不变），所以 `status`，以及 daemon 重启之后的 `status`，报出的是与当初完全相同的错误。`Scene.op` 序列化为小写，与 `detail.op` 相同（review M19）。

### 6.4 重启

daemon 启动时先调 `recover`（§5.8），然后才跑 gc、接受请求。中断时的各种状态在 `recover` 之后是这样：

| 中断时的状态 | `recover` 之后 |
| --- | --- |
| 会话 `Creating` | `Unknown`（`op = new`，最后记下的 phase），文件系统不动 |
| 会话 `Removing` | `Unknown`（`op = remove`），trash 里的与剩下的都不动 |
| 意图 `Preparing` | `Abandoned`；暂存与管理目录退役（会话不是 `Active` 时随会话一起离开） |
| 意图 `Swapping` | 意图与工作区都是 `Unknown`（`fail_move`），新旧两棵树都保留；之后由人经 `resolve_failed_move` 处理（§5.9） |
| 基线暂存 `bases/<repo>/.staging/*` | 退役进 trash |
| `Done` 意图的旧树、restore 现场 | 不动，`status` 照常报出 |
| `resolve_failed_move` 做到一半 | 不动：Abandon 记下之后没退役的暂存由 `status` 报 `MoveLeftover`，下一次移动或 `remove` 收走；Accept 移了 pin、没记下时，工作区仍是 `Unknown`，再 Accept 一次（§5.9 第 7、8 步） |

其余一概不碰：不重放、不清理工作区、不替上一次完成交换（不变量 6）。

### 6.5 已知限制

- `pre-push` 只拦住习惯性的 `git push`。agent 与用户是同一个 uid，可以设 `STRATA_PUBLISH=1`，或直接绕过钩子，所以它不是安全边界；真正的人类确认在 publish 的推送路径上（design §6.9）。工作区没有 remote，`git push` 不带 URL 本来就会失败。
- 交换之前只对依赖目录检查打开的文件（design §6.4 第 5 步）。cwd 在工作区里的进程（在工作区里启动的 Claude Code 或 shell）交换之后留在旧树上：继承这个 cwd 的命令在旧树里读写，`resolve` 对旧树里的路径返回 None。旧树因此被保留并在 `status` 里告警；写进旧树的东西没有 checkpoint，`changed` 读的时候重算，有改动时 `remove` 需要 `--discard`（review I5）。按路径重新进入工作区的进程不受影响。
- 交换之后，会话的暂存状态和本地 commit 不随工作区走：新管理目录的 index 等于新基线。overlay 里的 `Added` 条目因此变成 `Untracked`，两者发布与检索的待遇相同（types.md `EntryKind`）。本地 commit 的对象还在本会话的影子仓库里，随 `remove` 一起退役。
- 镜像只在 `new` 时取一次：之后主 checkout 的 `info/exclude`、`info/attributes` 与复制的配置键改了，已有的会话不受影响，只在 `status` 里给 `MirrorDrift`；新会话取新的。
- 建基线失败之后没能退役的暂存没有意图行，由 daemon 下一次启动时的 `recover` 或 `gc_repo` 过了年龄下限收走；在那之前它占着空间，`leftovers()` 报出它。
- 要认领或冷装依赖集时，基线多导出一次（§5.2 第 5 步）：那份导出跨过了装依赖，按裁定不再登记。相对几秒到几分钟的认领或冷装，这一次导出（tool 几秒）可以忽略。
- 整套兄弟布局的 `new` 很可能达不到 design §8 的 5 s：增补 8 的估算是全部仓库约 62 万个依赖条目，单线程 12–19 s，按实测的 4 路并行约 5.5 s，还没算源码树，也没算阶段 B 的 22 次网络 fetch（每次一个到 origin 的往返，4 路并行，§5.1 复杂度）。session 的计划是按仓库 4 路并行克隆（D41），按这个速率仍很可能超过 5 s。`new_warm_under_5s` 实测这一点；选项（按仓库并行克隆，即 D41；按仓库懒装依赖；放宽目标）在这一阶段结束时交给用户定。
- 带着几十万条目的被 ignore 目录（`src-tauri/target`）的工作区，移动的耗时由携带的克隆决定（§5.7.1），可能达不到 design §8 的"秒级"。
- 保留的旧树越多，`status` 越慢：每棵旧树一次进程扫描加一次冷扫（§5.5）。
- `resolve_failed_move` 只认 §5.9 第 3 步的两种现场。人手动改过的现场（把两棵树换回去、删掉其中一棵、动过 pin）一律得到 `scene_mismatch`，这时只剩 `rm --discard`。Accept 不知道当时没带过来的内容，所以旧树一律保留到 `remove`。

## 7. 测试清单

测试用真实的 git 仓库（file:// 的裸仓库当 origin）和真实的 Store（WAL + `synchronous=FULL`），都放在测试机 APFS 卷上的临时目录里；跨卷与非 APFS 的情形用 `hdiutil` 挂上的磁盘映像。持有文件的进程由同一个测试二进制以 `SESSION_TEST_HOLD=fd|cwd|mmap:<path>` 重新执行自己。测试从不碰 `/Volumes/GWD/inter`。故障与停顿经 §3 的 `SessionSeam` 注入 [我的决定 D38]：`FailAt` 注入 errno（经 apfs 的归类，`EIO`、`ENOSPC` 成为 `Unknown`，`ENOTDIR` 成为 `NotDirectory`），`StopAt` 模拟崩溃，要在两步之间做事的测试用 `PauseAt` 加 `wait_paused` 与 `release`（增补 4 "Test seams"；review M17）。daemon 那一侧的行为（`Tracker::reset`、status 回复的合并、请求排队）不在这里测。

| 测试名 | 断言 | 来源（矩阵行 或 新增） |
| --- | --- | --- |
| `new_typed_refusals_leave_no_trace` | 参数化：未配置的 repo → `NoSuchRepo`；主 checkout 的 `path` 不存在 → `NoSuchPath`；tracked 仓库的 `seed_caches` 条目是普通文件 → `NotDirectory`；显式种子文件是目录 → `IsDirectory`；显式种子文件是 FIFO → `NotFile`；`path` 是符号链接 → `NotDirectory`；`origin/<default_branch>` 不存在 → `NoSuchBranch`。每种都返回该码，`detail` 点名是哪一项检查、哪个条目。之后：Store 没有新会话行，`<paths.root>/sessions/`（含 `.repos/`）下没有新目录，没有建任何影子仓库，`refs/strata/pins/*` 不变；主 checkout 的工作树（每个条目的 mtime 与 ctime）、index、`config`、`hooks/`、`info/` 不变，refs 除了 gitio 的 fetch 可能写下的 `refs/strata/remotes/origin/<branch>` 之外不变（review M16） | `layerfs-project/tests/acquisition_custody.rs::a_definite_failure_removes_its_rows_and_its_record`；`layerfs-daemon/tests/mounted_serial_admission.rs::busy_initial_reservation_has_no_kernel_mount_effect` |
| `new_passes_lower_codes_through_unchanged` | gitio 返回的 `NoSuchBranch`、`Unsupported`（tree 里有 gitlink）经 `new` 原样到达调用方，`code`、`message`、`detail` 与 gitio 给出的逐字相同；Store 没有会话行，`sessions/` 下没有目录 | `layerfs-project/tests/acquisition_custody.rs::a_backing_read_failure_is_definite_and_is_cleaned_up` |
| `new_validates_ticket` | `../foreign`、空串、65 字节、含空格、含 `:` → `InvalidInput`（`detail.field = "ticket"`）。`s-k3q7xm2a`、`S-K3Q7XM2A` → `ReservedName`。`9246`、`gwd-9246`、`a.b_c-d` 通过 ticket 校验（随后的检查照常进行）。ticket 不出现在任何路径里。`RepoName` 的格式由 types 自己的测试负责 | `sdk/js/test/packaging.mjs::lazy`；`layerfs-sandbox/tests/docker_naming.rs::container_names_follow_the_engine_rule` |
| `new_refuses_cross_volume_root` | `paths.root` 在 `hdiutil` 挂上的另一个 APFS 卷上；`paths.root` 还不存在时，按它最近的已存在上级所在的卷判定。断言：`CrossVolume`，`detail` 含 root 与 repo 路径；在任何 fetch、导出与克隆之前返回，两个卷上都没有新目录，Store 没有新行，从不退化成复制。这是计划 Review Focus 1 的 session 一半，apfs 一半是 `clone_dir_refuses_cross_volume` | `layerfs-project/tests/init_sqlite.rs::an_unsupported_platform_refuses_the_store_before_any_file_exists` |
| `new_refuses_non_apfs_root` | `paths.root` 在 HFS+ 磁盘映像上：`Unsupported`（`detail.fs_type = "hfs"`），在任何写之前返回，什么都没写 | `layerfs-project/tests/init_sqlite.rs::an_unsupported_platform_refuses_the_store_before_any_file_exists` |
| `new_refuses_existing_session_dir_before_any_effect` | 用 `set_next_id` 固定会话 id。三种情形：预先建好 `<paths.root>/sessions/<sid>/`，或 `sessions/.repos/<sid>/` → `Exists`，`detail.reason = "directory"`；Store 里已有该 id 的行（含 `Removed` 而尚未被 gc 删掉的）→ `Exists`，`detail.reason = "store_row"`。预先存在的目录内容一字不动；没有 fetch、没有导出、没有 pin，也没有建影子仓库（id 在阶段 A 的末尾判定） | `sdk/js/test/reinit.mjs::entry`；`layerfs-api/tests/sandbox_operations.rs::create_mismatched_existing_project_refuses_before_any_deployment_or_control_effect`；`layerfs-daemon/tests/kernel_mount.rs::attach_refusals_have_no_effect_and_a_failed_attach_returns_to_unattached` |
| `new_refuses_root_aliased_into_checkout` | 四种写法，配置的字面路径都不嵌套（V3 通过）：`paths.root` 经符号链接落进某个主 checkout；`paths.root` 用大小写不同的拼写写进主 checkout（大小写不敏感的卷）；`paths.root` 落在人自己的 linked worktree 里，那个 worktree 的 common dir 就是该仓库的 `RepoId`；某个仓库的 `path` 经符号链接落进 `paths.root`。每种都返回 `InvalidInput`（`detail.rule = "nesting"`，带 `repo`、`root` 的真实路径，以及 `checkout` 或 `common_dir`），在任何写之前：真实位置下没有新目录，主 checkout 的 mtime 与 ctime、refs 都不变（还没有 fetch） | 新增（review I2、增补 1 的 T9 条）；理由出处 `layerfs-project/tests/acquisition_custody.rs::backing_inside_source_is_refused_before_an_operation_begins` |
| `new_refuses_symlinked_repo_path` | 主 checkout 的 `path` 是指向真实仓库目录的符号链接：`NotDirectory`，`detail = {repo, field: "path", path, reason: "symlink"}`，没有残留；`paths.root` 是符号链接时同样 `NotDirectory`（`field = "paths.root"`）。把 `path` 换成真实目录后，同一次 `new` 成功 | `layerfs-project/tests/acquisition_custody.rs::a_symlinked_source_is_refused_by_name_before_anything_begins`；`sdk/js/test/init_refusals.mjs::link` |
| `new_refuses_fifo_seed_before_writing` | 主 checkout 里的显式种子文件是 FIFO（`mkfifo`）：`NotFile`，在任何写之前返回，FIFO 从未被打开（调用不阻塞），没有残留；换成普通文件后重试成功，得到一个新的会话 id，不返回 `Exists` | `sdk/js/test/init_refusals.mjs::fifo` |
| `new_clones_entire_sibling_layout` | 配置三个仓库：`tool` 的脚本按 `../quick-start/src` 读兄弟库，另有 `release-notes`。`new(Some("9246"), [tool])` 返回的清单含全部三个仓库，只有 `tool` 的 `tracked = true`。`<paths.root>/sessions/<sid>/` 下恰好是三个仓库目录，`sessions/.repos/<sid>/` 下恰好是三个影子仓库 `<repo>.git`。在 `tool` 工作区里按 `../quick-start/src/index.ts` 读到的字节等于 quick-start 钉住基线里的同一文件。`root = <paths.root>/sessions/<sid>`，`path = root/<repo>`，路径里没有 ticket | 新增（design §3 兄弟布局、§10 新增项） |
| `new_workspace_equals_base_tree` | 100 与 1000 个文件两档（参数化）。每个工作区去掉 `.git`、依赖目录、种子缓存和种子文件之后，路径集合与 `git ls-tree -r <head>` 完全相同，逐文件字节、可执行位、符号链接目标都相同。`bases/<repo>/<tree>/<deps>/` 已登记（`has_base_gen`），`bases/<repo>/.staging/` 下与会话根旁都没有遗留的临时目录。用真实 Store，不用内存假实现 | `layerfs-project/tests/init_memory.rs::init_100_and_1000_over_memory_ports_pass_the_full_namespace_oracle`；`layerfs-project/tests/init_sqlite.rs::selected_profiles_init_100_and_1000_pass_the_full_namespace_oracle`；`layerfs-workspace/tests/captured_namespace.rs::the_model_of_the_fixture_is_the_base_root` |
| `new_workspace_keeps_seed_bytes_and_link_targets` | 基线 tree 里有 5 个符号链接：指向依赖目录、断链、自指、指向仓库外、目标是非 UTF-8 字节；还有二进制文件。主 checkout 里有被 ignore 的种子文件（含二进制字节），以及一个不在种子清单里的被 ignore 文件。断言：工作区里的链接目标逐字节等于 tree 里的，从不被跟随；种子文件字节相同；种子文件是符号链接（目标是非 UTF-8 字节）时在工作区里按目标重建，`readlink` 的字节相同；不在清单里的被 ignore 文件不被带入；之后改写、删除主 checkout 的种子文件，工作区里的副本不变；`.git` 是 gitfile，对象没有被复制。用真实 Store | `layerfs-project/tests/complete_root.rs::host_import_keeps_ignored_git_dependencies_outputs_and_exact_link_targets`；`layerfs-project/tests/complete_root.rs::real_host_profiles_acquire_and_reuse_the_exact_complete_root` |
| `new_refuses_explicit_seed_tracked_or_unignored` | 显式条目对着基线 generation 判定，都在 `create_session` 之前返回：`src-tauri/src/secrets.rs` 在基线 tree 里被跟踪 → `InvalidInput`，`detail = {repo, field: "seed_files", index, rule: "seed_files", reason: "tracked"}`；未被 ignore → 同样的码，`reason: "not_ignored"`；显式写出的 `CLAUDE.md` 在跟踪它的仓库里同样被拒（显式条目不按候选处理）。大小写不敏感的卷上，基线里被跟踪的是 `.ENV`、显式条目是 `.env` → `InvalidInput`（`reason: "exists_in_base"`），`.ENV` 一字不变；同一份表里写了 `.env` 与 `.ENV` 两个显式条目 → `InvalidInput`（`reason: "duplicate"`）。大小写敏感的卷（`hdiutil` 镜像）上前一种的 `.env` 照常复制，与 `.ENV` 并存。显式条目 `.claude/settings.local.json` 是符号链接、而主 checkout 里的 `.claude` 本身是符号链接 → `NotDirectory`（`reason: "symlink"`）。每种之后 Store 没有会话行、`sessions/` 下没有目录；基线 generation 已经建好并登记（缓存，留给以后用） | 新增（config.md §2.3、§4.3，module-rulings T9、"Disk identity follows the volume, git identity follows core.ignorecase"，review I3，增补 6） |
| `new_seed_writes_never_leave_the_session_root` | 基线 tree 跟踪一个符号链接 `conf -> <测试临时目录里会话根之外的目录 O>`。显式种子 `conf/.env` → `InvalidInput`（`reason = "parent_not_directory"`），在 `create_session` 之前；去掉它之后：候选种子 `.claude/settings.local.json`（`.claude` 是被跟踪的、指向 O 的符号链接）静默跳过，种子缓存 `conf/cache` 静默跳过，`new` 成功；O 里从头到尾没有新条目，O 的 mtime 不变 | 新增（增补 3 "Rooted destinations" 点名的测试；review I6） |
| `new_pins_chain_top_when_ahead` | `pin_unpushed = true` 且 `publish.branch == default_branch`：`refs/strata/publish/main` 领先 `origin/main` → `head` 是链顶；链与 origin 相同、落后或分叉 → `head` 是 `origin/main` 的 tip；`pin_unpushed = false` → 一律 origin | 新增（design §6.7） |
| `new_pins_default_branch_when_publish_branch_differs` | `publish.branch = "release"`、`default_branch = "main"`，`refs/strata/publish/release` 领先：新会话仍钉 `origin/main`，`pin_unpushed` 不起作用 | 新增（config.md §2.3 的 `publish` 条，T2 裁定） |
| `new_seeds_default_names_only_when_untracked_and_ignored` | 配置里没写 `seed_files`，缺省四项都是候选：`CLAUDE.md` 被跟踪 → 不复制，不报错也不告警，工作区里是基线版本；`.env` 未被 ignore → 不复制；`.env.local` 被 ignore 且未被跟踪 → 复制；`.claude/settings.local.json` 只靠 `core.excludesFile` 被 ignore → 复制；主 checkout 里不存在的条目静默跳过。另一组：候选 `.env` 在主 checkout 里是目录、`.env.local` 是 FIFO → 都静默跳过，FIFO 从未被打开，`new` 成功（review M10）。大小写不敏感的卷上基线里被跟踪的是 `.ENV`：候选 `.env` 静默跳过，工作区里的 `.ENV` 是基线版本 | 新增（module-rulings T9 from T2，config.md §2.3） |
| `new_clones_seed_caches_as_real_directories` | tracked 仓库的 `src-tauri/target`、`.next/cache`、`node_modules/.cache` 从主 checkout 克隆为真实目录：`lstat` 不是符号链接；改写工作区里的文件，主 checkout 的同一文件不变。`node_modules/.cache` 落在依赖目录里。`tracked = false` 的仓库不克隆种子缓存。主 checkout 的 `target` 是符号链接时返回 `NotDirectory`，在任何写之前 | 新增（design §3 symlink 陷阱、§4） |
| `workspace_git_is_detached_and_prepush_refuses` | 每个工作区：`git symbolic-ref -q HEAD` 失败（detached）；`git rev-parse HEAD` 等于清单的 `head`，且 `tree_of(head) == base.tree`；`git rev-parse --git-common-dir` 是 `<paths.root>/sessions/.repos/<sid>/<repo>.git`；`git remote` 输出为空，没有 upstream；新建后 `git status --porcelain` 为空（种子文件与依赖目录被 ignore）。`git push <裸仓库 URL> HEAD:refs/heads/x` 被 `pre-push` 拒绝：非零退出，stderr 含 strata 的提示，远端没有 `x`；用户级配置（`GIT_CONFIG_GLOBAL` 指向的文件）把 `core.hooksPath` 设成一个空目录时照样被拒（影子仓库钉住了 `core.hooksPath`）。带 `STRATA_PUBLISH=1` 时钩子放行（design §6.8 的约定） | 新增（design §6.8；增补 2；gitio 复审 N1） |
| `manifest_round_trips_through_store` | `new` 返回的清单（id、ticket、root，以及每个 repo 的 base、head、path、tracked）与重开 Store 之后的 `status(id).manifest` 完全相等，JSON 往返不变 | `layerfs-sandbox/tests/container_labels.rs::labels_are_sent_at_create_and_found_in_any_order_beside_the_image_s` |
| `new_warm_under_5s` | 合成一套规模接近 tool 加 21 个兄弟库的配置，基线与依赖集都已在缓存里：记录 `new` 的墙钟时间与各阶段的分解，断言小于 5 s。标 `#[ignore]`，在 spike 里跑，并据此修正目标（§6.5：增补 8 的估算是整套布局很可能超过） | 新增（design §8 验收数字） |
| `new_same_ticket_creates_another_session` | 同一 ticket 连续 `new` 两次：得到两个不同的会话 id、两个独立的会话根和各自的影子仓库；第二次不复用第一次的目录，也不重建已有的基线与依赖集（Store 里 generation 的行数不变）。`select("9246")` 返回 `AmbiguousMatch`，`detail.candidates` 恰为这两个 id | `sdk/js/test/reinit.mjs::entry`；`layerfs-api/tests/sandbox_operations.rs::create_matching_existing_project_keeps_one_original_hello_and_does_not_reinstall` |
| `new_lost_reply_is_found_not_replayed` | 调用方没有拿到 `new` 的返回值（测试丢弃它）：会话照常处于 `Active`，`list` 和按 ticket 的 `select` 都能找到它；没有任何重放的路径，再调一次 `new` 得到的是另一个会话 | `layerfs-daemon/tests/kernel_mount_routes.rs::lost_bind_and_attach_acknowledgements_are_located_without_replay` |
| `resolve_by_cwd_inside_workspace` | cwd 为 `<root>/tool/src/app`，包括大小写不同的拼写、经符号链接到达的同一目录 → `Some((sid, tool))`。cwd 为会话根本身、会话根下 dev-link 建的 `node_modules/`、`.strata-tmp-move-*`、`.strata-tmp-restore-*`、`sessions/.repos/<sid>/tool.git`、主 checkout、不存在的路径 → `None`。`Unknown` 会话的工作区路径 → `Some`；已删除会话的工作区路径 → `None`；Store 读不出来 → `Err`，不是 `None` | `layerfs-sandbox/tests/container_labels.rs::a_missing_or_different_label_or_another_mount_is_a_topology_mismatch` |
| `select_by_id_or_ticket` | 会话 id → 该会话；已删除 → `NoSuchSession`。ticket 只有一个会话 → 该会话；没有会话 → `NoSuchSession`；两个会话 → `AmbiguousMatch` 加 `candidates`。不合格的选择器 → `InvalidInput`。`list` 含 `Unknown` 的会话，不含已删除的 | 新增（types.md §6.2 的 `AmbiguousMatch`、`NoSuchSession`） |
| `sessions_on_one_base_are_isolated` | 两个会话 A、B 钉同一基线：在 A 里写、删、改名文件，B 的工作区、清单与 checkpoint 都不变；A 的 `git stash` 之后，B 的 `git stash list` 为空，A 的 `git stash pop` 取回的是 A 自己的改动；A 的 `git branch x` 在 B 与主 checkout 里都不存在。`new` 之后改主 checkout 的 `info/exclude`，让 A 里一个被 ignore 的文件不再被 ignore：A 里 `git check-ignore` 的结果、A 的 overlay 都不变，`status(A)` 给 `MirrorDrift`。一次被拒绝的 `new`（`NoSuchRepo`）不改变 A 或 B 的任何状态 | `layerfs-daemon/tests/mounted_cycles.rs::simultaneous_mounts_over_related_roots_are_isolated_and_a_stale_token_never_redirects` |
| `seed_files_listed_in_status_and_never_checkpointed` | `new` 之后，`status` 的每个工作区都列出每个种子文件的 `path`、`source`、`size`、`source_mtime`、`copied`，没有任何内容字段。改写工作区里的 `.env` 后做 checkpoint：entries 里没有任何种子文件路径。之后改写主 checkout 的 `.env` → `SeedStale`；会话把 `.env` 从 `.gitignore` 里删掉 → `SeedNotIgnored`，再做 checkpoint，entries 里仍然没有 `.env`（overlay 的排除集合），工作区里 `git status` 把它列为未跟踪 | 新增（design §4 种子文件） |
| `status_warns_untracked_repo_edits_not_checkpointed` | `tracked = false` 的每个工作区都带 `NotCheckpointed` 告警，`checkpoint` 为 None，`pending` 为空；tracked 的工作区没有这条告警 | 新增（module-rulings T9） |
| `status_reads_workspace_presence_from_filesystem` | 不经 Store 删掉一个工作区目录（或它的 `.git`）：`status` 报 `Missing`，Store 里的行没有被改写；把目录放回去后，`status` 回到 `Ok`。在会话根下建一个非空的 `.strata-tmp-restore-tool/`（含 `plan.json`）：`tool` 的告警里出现 `RestoreScene { path }`，删掉它之后告警消失 | `layerfs-sandbox/tests/engine_lifecycle.rs::only_the_engines_no_such_container_answer_is_a_gone_container`；`layerfs-sandbox/tests/engine_lifecycle.rs::a_stop_the_engine_answers_with_no_such_container_is_a_gone_container`；`layerfs-sandbox/tests/engine_lifecycle.rs::only_the_exact_intact_container_not_running_is_a_stopped_container`；`layerfs-sandbox/tests/container_labels.rs::a_missing_or_different_label_or_another_mount_is_a_topology_mismatch` |
| `status_reports_drift_and_pending_proposals` | 在工作区里改 `package.json` 的依赖：`drift` 为工作区算出的安装输入哈希，与所钉依赖集的不同；只改 `version` 字段不产生 drift（依 deps 的 `inputs_hash` 规则）。deps 登记一条 Pending 的提议：`to` 不等于这个工作区的 `base.deps` → `pending` 列出它；`to` 等于它（会话已经在目标上）→ 不列；`tracked = false` 的工作区从不列（deps.md §4.5，增补 6 的 T9 条） | 新增（design §6.3、§6.4；types.md `Drift` 行） |
| `status_reports_mirror_drift_and_vanished_shadow` | `new` 之后改主 checkout 的 `info/exclude` 与 `core.excludesFile`：`MirrorDrift.items` 恰为 `[{reason: "info_exclude"}, {reason: "config", key: "core.excludesfile"}]`，影子仓库里的镜像文件逐字节不变。再在工作区里跑 husky 安装时的那条 `git config core.hooksPath .husky/_`：多出一项 `{reason: "pre_push_guard_disabled", hooks_path: ".husky/_", hook_replaced: false}`，`status` 之后影子仓库的 config 逐字节不变（session 不钉回）。删掉影子仓库：`status` 报 `Missing` 与 `ShadowMissing`，之后影子仓库仍然不在（从不重建） | 新增（增补 2；gitio 复审；gitio @ac4149f 的 `PrePushGuardDisabled`，module-rulings T13 条） |
| `new_failure_after_writing_began_is_unknown` | `FailAt { phase: "new.workspace@<第二个仓库>", errno: ENOSPC }`：返回 `Unknown`，`detail.repo` 是第二个仓库。会话根与第一个仓库已克隆的目录原样留下，排在第二个之后的仓库都没有开始，还没有影子仓库；不删除、不重试、不回滚。Store 里该会话为 `Unknown`，`phase = "workspace"`。`status` 报出这个现场（`Scene.op = New`）；`resolve` 对已克隆仓库的路径返回 `Some`（会话存在，由各命令自己的检查拒绝它）。用同一 ticket 再 `new`，得到新 id，旧现场不受影响。另一例：`FailAt { phase: "new.observe", errno: EIO }`：同样返回 `Unknown`，`phase = "observe"`；会话停在 `Unknown`，从未成为 `Active`；工作区、影子仓库、pin 都在 | 新增；理由出处 `layerfs-project/tests/acquisition_custody.rs::a_failed_cleanup_reports_its_cause_and_the_rows_still_held`；`observe` 一例：增补 6，deps.md @22e1fac §2 对 session 的要求第 4 条 |
| `new_unknown_detail_names_the_kept_scene` | 同上的失败：`detail` 恰好有 `op = "new"`、`session`、`phase`、`repo`、`path`、`cause`（`errno = 28` 与原文）、`kept`（会话根的绝对路径）；`message` 符合 §6.3 的模板；`Scene` 序列化后 `op` 为 `"new"`，与 `detail.op` 相同；清单从未返回给调用方 | `layerfs-project/tests/acquisition_custody.rs::an_unknown_outcome_leaves_the_operation_untouched` |
| `unknown_scene_survives_daemon_restart` | 两种情形。（a）上一条的 `Unknown` 之后，丢弃 `Sessions` 与 `Store` 再重新打开；`recover()` 之后，`status` 报出同一个现场（同 id、phase、kept、error）。（b）`StopAt { phase: "new.seed_files" }` 就地返回，不更新状态，模拟崩溃；重开后 `recover()` 把仍为 `Creating` 的行记为 `Unknown`，`status` 报出最后记下的 phase。两种情形下，文件系统在重开前后逐字节相同：不重放，不清理 | 新增（module-rulings T9）；理由出处 `layerfs-project/tests/acquisition_custody.rs::a_failed_cleanup_after_a_failure_keeps_both_causes` |
| `base_staging_leftover_reported_then_retired_by_recover` | `StopAt { phase: "base.register" }` 模拟进程停在登记之前：重开之后 `leftovers()` 报出 `BaseStaging { repo, path: B, .. }`；`recover()` 之后 `B` 在 store 的 trash 里（`RecoverReport.retired`），`leftovers()` 为空，已登记的 generation 不受影响。另一个线程里停在 `PauseAt { phase: "base.deps" }` 的 `ensure_base` 的暂存不在 `leftovers()` 里。`FailAt { phase: "base.register", errno: EIO }`：返回 `Unknown`（`detail.op = "base"`，`kept` 为空，`retired` 是 `B`），`B` 连同其中的导出已在 trash 里，`leftovers()` 为空。具名失败（`FailAt` 以 `ENOTDIR` 在 `base.export`）→ 原样返回 `NotDirectory`，暂存当场退役。被 `gc_repo` 收走：停在 `PauseAt { phase: "base.register" }` 时，测试用 `store.retire_dir(B)` 把暂存挪进 trash（同 `gc_repo` 第 5 步的做法），放行之后 `ensure_base` 重建一次、登记成功；两次都被挪走（第二次的 `PauseAt` 在重建时装上）→ `Busy`（`reason = "base_staging_retired"`），没有登记，`bases/<repo>/<tree>/` 下没有目录 | 新增（review M22；增补 6 "Export staging"；module-rulings "Base staging is registered right after its manifest"） |
| `base_generation_never_modified_in_place` | 在工作区里改写、删除、chmod 文件，`bases/<repo>/<tree>/<deps>/` 与依赖集目录的字节和 mode 都不变；`move_workspace` 移到新基线之后，旧的 generation 目录仍然不变 | `layerfs-workspace/tests/operation_scope.rs::scoped_views_share_original_install_serials_and_keep_original_provider` |
| `ensure_base_builds_outside_the_lock` | 测试用的 `RepoWriteLock` 记下每一段持锁。停在 `PauseAt { phase: "base.export" }`、`"base.deps"`、`"base.manifest"` 时，另一个线程对同一个 repo 的 `with_write` 立即拿到锁。停在 `PauseAt { phase: "base.register" }` 时另一个线程先拿住这把锁 200 ms：这期间 `has_base_gen` 为假，放开之后 `ensure_base` 才登记并返回。两个调用建同一个 tree：A 停在 `base.register`，B 整个做完并登记；放行 A 之后两者返回同一个 `BaseGen`，`bases/<repo>/<tree>/` 下只有一个 generation 目录，A 的暂存在 store 的 trash 里，`leftovers()` 为空。`ensure_base_for(repo, 旧 tree, 已封存的新依赖集)` 建出 (旧 tree, 新依赖集) 的 generation，测试的安装环境不能冷装，它也从不调 `deps::ensure`；依赖集没封存 → `InvalidInput`（`deps_unsealed`），`bases/<repo>/.staging/` 为空。要认领的冷路径（测试的安装环境让 `ensure` 认领主 checkout 的依赖目录）：`ensure` 返回之后第一份暂存已在 trash 里，登记的 generation 来自之后新建的第二份暂存（名字不同），这份暂存从建到登记之间没有调用 `deps::ensure` | 新增（module-rulings "Base generations are built outside the lock"、"Base staging is registered right after its manifest"；review C-d） |
| `move_workspace_validates_before_writing` | 参数化：RepoId 不在配置里 → `NoSuchRepo`；`new_base.repo` 与映射出的名字不同 → `InvalidInput`（`repo_mismatch`）；`tracked = false` → `InvalidInput`（`untracked_repo`）；`tree_of(head) != new_base.tree` → `InvalidInput`（`head_tree`）；依赖集没封存 → `InvalidInput`（`deps_unsealed`）；依赖集封存了而 `new_base` 这个基线 generation 没有登记 → `InvalidInput`（`base_unbuilt`，review C-d）；没有共同祖先 → `NoMergeBase`；工作区不在 → `NoSuchPath`；影子仓库不在 → `NoSuchPath`（`shadow_missing`）；会话在 `Creating` 或 `Removing` → `InvalidInput`（`lifecycle`，review M13）；会话或工作区是 `Unknown` → `Unknown`，带记下的现场；restore 现场非空 → `Unknown`（`restore_scene`）。每种都不写任何东西：没有 checkpoint、没有意图行、没有暂存，refs 不变 | 新增（types.md §8、module-rulings T9） |
| `move_workspace_swaps_base_and_deps_atomically` | 移动之后：有一个 `BeforeRollout` checkpoint，它经 `Sessions::open` 收到的 `BeforeCheckpoint` 记下（测试的实现计数，并转给不带 Tracker 的 `overlay::checkpoint`），session 自己没有调 `overlay::checkpoint`；工作区的源码等于新基线加上投影后的 overlay，依赖目录是新依赖集的克隆；`WorkspaceState.base`（tree 与依赖集）与 `head` 一起更新，代数变成意图的 n，`.git` 指向 `worktrees/<n>/`，旧管理目录已退役；HEAD detached 在新 head，pin 移到新 head。`StopAt` 分别停在 `move.prepare`、`move.swap`、`move.pin`、`move.record`，各自重开并 `recover()`：停在 `prepare` → 意图 `Abandoned`、暂存退役，工作区与 Store 都是旧状态；其余三处 → 工作区 `Unknown`，新旧两棵树都保留。任何一处都不出现新源码配旧依赖。提出者自换：`ensure_base_for(repo, 旧 tree, 新依赖集)` 在锁外建出 (旧 tree, 新依赖集) 的 generation，再 `move_workspace` 移过去：源码与 head 不变，依赖目录换成新依赖集的克隆 | 新增（design §6.4"依赖与源码一起移动"、第 3 步，不变量 8；module-rulings "Base generations are built outside the lock"） |
| `move_workspace_rechecks_open_files_before_swap` | `PauseAt { phase: "move.recheck" }`：测试线程等到停住，让持有进程打开新依赖目录里的一个文件，再放行。返回 `OpenFiles`；工作区目录一字不动（inode 与内容都不变）；暂存与新管理目录已在 store 的 trash 里；意图为 `Refused`；Store 里的 base 与 head 不变；`BeforeRollout` checkpoint 留作历史；下一次移动的意图是 n + 1，不复用 n | 新增（module-rulings T8：交换前复查；增补 3；review M17、M18） |
| `move_workspace_projects_overlay_and_marks_conflicts` | 从旧基线移到新基线（模拟一次发布）：会话已发布的路径离开 overlay，记入 `dropped`；未发布、与新基线不重叠的改动经三方合并后写入；同一 hunk 两边都改的路径保留会话版本，列在 `conflicts` 里，`status` 也列出它；overlay 里 mode 0o100755 的文件在新树里仍可执行，0o120000 的条目成为符号链接，目标字节等于 blob 的内容。交换之后对工作区冷扫（`reconcile_full`）得到的条目等于 `rebase` 的结果 | 新增（design §6.5 第 7 步；增补 3 "Projection"；review I9） |
| `move_workspace_carries_ignored_content` | 移动之后，种子文件、种子缓存（`target/` 里的内容）、`node_modules/.cache`、其它被 ignore 的文件、被 ignore 的符号链接（链接本身，目标不变）和空目录都原样出现在新工作区里，内容相同，blob store 的条目数不因它们增加。被 ignore 的本地文件与新基线新增的同名被跟踪文件相撞 → 不携带，记进 `uncarried`，新树里是被跟踪的版本，本地文件留在保留的旧树里（`retained = true`，再移动一次它也不被收走）。新基线把 `conf` 换成指向会话根之外目录 O 的被跟踪符号链接，而旧树的 `conf/` 下有被 ignore 的文件 → 记进 `uncarried`，O 里没有新条目。FIFO、mode 000 的文件 → `uncarried`。`PauseAt { phase: "move.carry" }` 停在遍历之后，测试把一个要带的被 ignore 文件换成同名目录、删掉另一个，再放行：移动成功，前者记进 `uncarried`（旧树保留），后者既不在新树里也不在 `uncarried` 里，新树里没有 `.strata-carry-*`。被 ignore 的文件带 `uchg`：返回 `ReadOnly`（`detail.path` 是它），意图 `Refused`，暂存退役，工作区不变。被 ignore 的符号链接 `src/gen-link`（`src/` 下有被跟踪的文件，遍历走进 `src/`）在新树里按目标重建，`readlink` 的字节与旧树相同（含非 UTF-8 目标），不是克隆。`PauseAt { phase: "move.carry" }` 停在遍历之后，测试把旧树的 `src` 换成指向会话外目录 O 的符号链接（O 里有一个同名、目标不同的 `gen-link`），再放行：`src/gen-link` 记进 `uncarried`（`attrs_of` 在 `src` 上给 `NotDirectory`），新树里没有来自 O 的链接 | 新增（design §4 种子文件、种子缓存；增补 3 "Rooted destinations"；review I6、M14；module-rulings "clone_from_rooted states its source kind"、"A symlink is recreated, never cloned"） |
| `move_workspace_keeps_old_tree_while_in_use` | 移动时持有进程的 cwd 在旧工作区里：移动成功，`MoveReport.old_tree` 为 `<root>/.strata-tmp-move-<repo>-<十六进制>/`，旧树原样留着，`status` 带 `OldTreeKept`（含该 pid，`changed = false`，`retained = false`）。两种后续（参数化）：（a）往旧树里写一个文件，`status` 重算出 `changed = true`；持有进程退出之后再移动一次，这棵旧树仍然留着。（b）旧树没被改过，持有进程退出之后再移动一次，它连同它的管理目录一起退役 | 新增（design §2 反目标"丢失未发布改动"；module-rulings T9 的交换规则；review I5） |
| `move_workspace_seconds_warm` | 与 `new_warm_under_5s` 同样的规模，新基线与依赖集都已在缓存里：`move_workspace` 的墙钟时间在秒级（目标小于 5 s），分别记录投影、携带（含几十万条目的 `target/`）与 refresh 的耗时。标 `#[ignore]` | 新增（design §8"依赖 rollout = 克隆 + rename，秒级"） |
| `recover_abandons_pre_visible_move_intents` | （a）`FailAt { phase: "move.prepare", errno: EIO }`：返回 `Unknown`（`phase = "prepare"`，`kept` 是暂存）；工作区健康仍是 `Ok`，`status` 报 `MoveLeftover { n, staging }`；再对同一工作区 `move_workspace`：意图 n 被关成 `Abandoned`、它的暂存与管理目录退役，新的移动用 n + 1 并成功。（b）`StopAt { phase: "move.prepare" }` 之后重开并 `recover()`：意图 `Abandoned`，`RecoverReport.abandoned` 列出它，暂存在 trash 里，工作区与 Store 的 base、head 不变。（c）`FailAt` 以 `ENOTDIR` 在 `move.prepare`：返回 `NotDirectory`，意图当场关成 `Refused`，暂存退役，没有 `MoveLeftover`。（d）不靠注入：把影子仓库的 `worktrees` 换成指向主 checkout `.git/worktrees` 的符号链接，再 `move_workspace`：返回 `NotDirectory`（来自 `attach_worktree`），意图 `Refused`，暂存退役；主 checkout 的 `.git/worktrees/` 的列表与每个字节不变，工作区不变 | 新增（增补 3 "Error timing, refined"；增补 4 细化 2；review I4；增补 8 对影子仓库的澄清，gitio @ac4149f） |
| `resolve_failed_move_abandons_when_never_swapped` | `StopAt { phase: "move.swap" }` 之后重开并 `recover()`：意图与工作区健康都是 `Unknown`（`phase = "swap"`），工作区还是旧树，暂存 `S` 是新树，pin 未动。`resolve_failed_move(id, repo, Abandon)` 成功：意图 `Abandoned`，健康 `Ok`，base、head、代数与冲突同移动之前，工作区目录逐字节不变，`S` 与 `worktrees/<n>` 在 store 的 trash 里，`ResolutionReport.leftover` 为空，`status` 没有 `MoveLeftover`。之后对这个工作区 `move_workspace` 用 n + 1 并成功。另一次：`FailAt { phase: "resolve.retire", errno: EIO }` 下 Abandon 照样成功，没退役的暂存列在 `leftover` 里、`status` 报 `MoveLeftover`，下一次 `move_workspace` 收走它 | 新增（module-rulings "Resolving a failed move"；review M-b） |
| `resolve_failed_move_accepts_when_swapped` | `FailAt { phase: "move.pin", errno: EIO }`：`move_workspace` 返回 `Unknown`（`phase = "pin"`），工作区是新树，旧树在 `S`，pin 还是旧 head，健康 `Unknown`。（a）`FailAt { phase: "resolve.record", errno: EIO }` 下 Accept 返回 `Unknown`（`detail.op = "resolve"`、`phase = "record"`），pin 已是新 head，健康仍是 `Unknown`，原来的现场不变。（b）不带接缝再 Accept：成功，pin 不再移动；`status` 的 base、head、代数是意图的 `to`、`head`、`n`，`conflicts` 等于那次 `rebase` 的冲突（进入 `Swapping` 时记下的）；`OldTreeKept { path: S, retained: true }`；意图 `Done`，`old_tree = S`；工作区里 `git rev-parse HEAD` 是新 head。之后的 `move_workspace` 用 n + 1 并成功，旧树随 `remove` 退役 | 新增（module-rulings "Resolving a failed move"；review M-b） |
| `resolve_failed_move_refuses_a_disagreeing_scene` | 每种情形都返回 `InvalidInput`，store 的行、pin、两棵树与两个管理目录逐字节不变：（a）交换之后失败（`FailAt` 在 `move.pin`）却选 Abandon → `reason = "scene_mismatch"`，`detail.matches = "accept"`；（b）交换之前停下（`StopAt` 在 `move.swap`，再 `recover()`）却选 Accept → `matches = "abandon"`；（c）（b）之后把 `S` 改名挪走 → 两种选择都是 `scene_mismatch`，`matches = null`，之后 `rm --discard` 加凭据仍能删掉会话；（d）（a）之后把 pin 移到别的 commit → Accept 是 `scene_mismatch`；（e）健康 `Ok` 的工作区，以及只有 `MoveLeftover` 的工作区 → `not_failed` | 新增（module-rulings "Resolving a failed move"；review M-b） |
| `workspace_git_never_writes_main_checkout` | 在 `new`、工作区里的 `git add`、`git commit`、`git stash`、`git branch`，以及 `remove` 之后：主 checkout 的工作树、index、`config`、`hooks/`、`info/`、`worktrees/` 逐字节不变；`refs/` 只多出 `refs/strata/pins/<sid>`（`new` 时出现，`remove` 后消失）与 gitio 的 fetch 写的 `refs/strata/remotes/origin/<branch>`，人自己的 `refs/remotes/origin/*` 不变；主对象库只多出 fetch 取回的对象。工作区里新建的 commit 对象在本会话的影子仓库里。工作区里 `git fetch origin` 失败、`origin/main` 解析不到，所以 front dev-link 那段"干净就 `reset --hard origin/main`"不会在兄弟工作区里发生 | 新增（design §9 不变量 2、§6.8；`front/dev-link.sh`；review M16） |
| `removed_session_id_never_resolves_or_reissued` | 删除会话 A 之后：`select(A)`、`status(A)` → `NoSuchSession`；在 A 的旧工作区路径新建一个同名目录，`resolve` 对它返回 None；A 的 `Removed` 行还在保留期里时，`set_next_id` 再给出 A 的 id，`new` → `Exists`（`store_row`），A 的目录也不会被新会话复用 | `sdk/js/test/verify/exec_mount_lookup_real.mjs`；`layerfs-daemon/tests/mounted_cycles.rs::simultaneous_mounts_over_related_roots_are_isolated_and_a_stale_token_never_redirects` |
| `remove_refuses_unpublished_without_discard` | 全部仓库都是 tracked，`tool` 有未发布改动（overlay 非空）：`remove(id, false, None)` → `Dirty`，`detail.repos.tool` 列出路径；会话目录、清单、checkpoint 一字不动。`remove(id, true, Some(RemoveDiscard{id} 的凭据))` 成功，`RemoveReport.checkpoints` 含经 `BeforeCheckpoint` 新记的 `BeforeRemove` checkpoint，其 entries 等于删除前的 overlay。另一个会话：一次移动保留了旧树，之后往旧树里写了文件，工作区本身没有改动 → `Dirty`，`detail.old_trees` 列出旧树（review I5） | `layerfs-overlay/tests/orphans.rs::logical_close_refuses_new_captured_owners_and_keeps_existing_consumers`；`layerfs-overlay/tests/piece_lifetimes.rs::a_closed_workspace_retains_its_pieces_until_file_disposal` |
| `remove_untracked_repo_requires_discard` | 会话含 `tracked = false` 的仓库，tracked 仓库没有改动：不带 discard → `Dirty`，`detail.untracked_repos` 列出这些仓库；带 discard 和匹配的凭据 → 成功 | 新增（module-rulings T9） |
| `remove_discard_checks_the_token` | `discard = true` 而 `confirm` 为 None → `NotConfirmed`（`reason = "missing"`）；凭据的 action 是另一个会话的 `RemoveDiscard`、或是 `PushDeploy` → `NotConfirmed`（`reason = "mismatch"`）。都什么都不删；凭据按值消费 | 新增（types.md §4"谁消费"、design §6.9） |
| `remove_after_mtime_only_change_needs_no_discard` | 全部仓库都是 tracked，只 `touch` 过文件，内容与 mode 都没变：overlay 为空，`remove(id, false, None)` 成功。`RemoveReport.checkpoints` 里的每个号都是这次新记的 `BeforeRemove` 行（比删除前最新的号大），没有变化也照样记（review M-a） | `layerfs-daemon/tests/mounted_commit.rs::an_unmount_without_discard_succeeds_after_an_mtime_only_commit` |
| `remove_refuses_when_a_process_holds_fd_cwd_or_mmap` | 参数化三种持有：持有进程打开会话里的一个文件、cwd 在工作区里（另一变体：cwd 在 `sessions/.repos/<sid>/tool.git` 里）、mmap 了依赖目录里的一个文件，其中一个变体让持有进程阻塞在写满的管道上。每种都返回 `OpenFiles`，`detail.open` 含该 pid 与路径；目录一字不动；持有进程不被发信号，之后仍能读写；持有释放后 `remove` 成功 | `layerfs-api/tests/observed_control.rs::unmount_force_selects_one_original_request_without_unknown_relinquishment`；`layerfs-daemon/tests/f046_fork_reference.rs::a_descriptor_copied_by_another_threads_fork_keeps_the_plain_unmount_busy`；`layerfs-daemon/tests/forced_unmount.rs::fp33_a_held_working_directory_leaves_the_mount_aborted_and_retained`；`layerfs-daemon/tests/forced_unmount.rs::fp33_a_held_descriptor_leaves_the_mount_aborted_and_retained`；`layerfs-daemon/tests/mounted_cycles.rs::a_caller_blocked_on_a_full_pipe_neither_blocks_commit_nor_is_touched_by_unmount` |
| `remove_open_files_in_one_session_never_block_another` | 会话 A 被进程占用，B 没有：`remove(A)` → `OpenFiles`；`remove(B)` 照常成功；B 的删除不影响 A | `layerfs-daemon/tests/forced_unmount.rs::fp22_a_reference_in_a_never_holds_b_and_force_on_a_signals_no_process` |
| `remove_has_no_force_path` | 即使 `discard = true` 并带匹配的凭据，进程占用时仍然返回 `OpenFiles`；`remove` 从不返回 `Busy`，排队是 daemon 的事；API 上没有 force 参数 | `layerfs-daemon/tests/forced_producers.rs::fp32_force_is_busy_while_a_commit_is_held_before_save_finish`；`layerfs-daemon/tests/forced_producers.rs::fp32_force_is_busy_while_a_commit_is_held_before_publication`；`layerfs-daemon/tests/forced_producers.rs::fp32_force_is_busy_while_a_commit_is_held_before_its_local_install` |
| `remove_unresolved_scene_requires_discard_confirmation` | 参数化：某个工作区是 `Unknown`（`move_workspace` 交换之后失败留下的现场）、`Missing`、有非空的 restore 现场目录（含 `plan.json`）、会话从未成为 `Active`（`new` 留下的 `Unknown`）。不带 discard → `Dirty`，`detail.unresolved` 列出它（restore 现场写出路径）；带 discard 和匹配的凭据 → 成功，会话根连同 restore 现场一起退役。只有 `MoveLeftover` 的会话不带 discard 就能删，那条意图被关成 `Abandoned`（review M15） | `layerfs-daemon/tests/forced_producers.rs::fp32_unknown_commit_custody_is_forced_only_when_relinquished_and_stays_unknown` |
| `remove_failure_midway_is_unknown_and_keeps_scene` | `FailAt { phase: "remove.retire", errno: EIO }`：返回 `Unknown`；pin 已经删掉，会话根与影子仓库目录都还在原处；状态为 `Unknown`，`phase = "retire"`；不重试；`status` 报出这个现场；之后的 `remove` 需要 discard 和凭据 | `layerfs-api/tests/sandbox_operations.rs::failed_stop_preserves_original_owner_channel_inputs_and_attempt_without_delete`；`layerfs-api/tests/sandbox_operations.rs::failed_delete_retains_acknowledged_stop_and_exact_delete_failure` |
| `remove_deletes_only_its_own_session` | 删除 A 之后：A 的会话根与 `sessions/.repos/<A>/` 在 store 的 trash 里，A 的 pin 都不在了；B 的工作区、B 的影子仓库与 pin、`bases/`、依赖集、主 checkout 逐字节不变。A 的会话根里有 dev-link 建的、指向会话外的符号链接 `node_modules/tailwindcss`：store 的 gc 删掉 trash 之后，链接的目标不受影响 | `layerfs-api/tests/sandbox_operations.rs::close_stops_then_deletes_the_original_owner_without_deleting_its_borrowed_volume`；`layerfs-daemon/tests/workspace_databases.rs::terminal_unmount_deletes_only_its_file_after_retained_requests_release`；`layerfs-sandbox/tests/engine_lifecycle.rs::exited_daemon_is_stopped_by_the_engines_not_modified_then_deleted`；`layerfs-sandbox/tests/engine_lifecycle.rs::a_stopped_container_is_deleted_once_and_never_forced` |

### config 级：只列出，不由 session 实现

V2、V3 的字面检查在 daemon 加载配置时由 `Config::validate` 做，不做 I/O（增补 1 的 T9 条）。下面这条由 strata-types 的测试承担，不进 §8 的任务。经符号链接、大小写或 linked worktree 才嵌套的写法不是配置规则，由 `new` 在阶段 A 判定，测试是上表的 `new_refuses_root_aliased_into_checkout`（review I2）。

| 测试名 | 断言 | 来源（矩阵行 或 新增） |
| --- | --- | --- |
| `config_v3_root_and_repo_paths_never_nest` | 三种字面写法：`paths.root` 位于某个 `path` 之内；某个 `path` 位于 `paths.root` 之内；Store 落在主 checkout 里（`paths.root = <path>/.strata`）。每种都由 `Config::validate` 返回 `InvalidInput`（`detail.rule = "V3"`），daemon 加载配置时整份拒绝：不会有会话、没有目录，主 checkout 的 mtime 与 ctime 不变。`..` 绕行的写法在 V2 就被拒 | `layerfs-project/tests/acquisition_custody.rs::backing_inside_source_is_refused_before_an_operation_begins`；`layerfs-project/tests/init_sqlite.rs::a_store_inside_the_source_is_refused_before_an_operation_begins` |

覆盖：目标模块为 session 的矩阵行，加上理由点名 session 的 `—` 行，共 51 行（3b 13、3c 4、3d 29（含裁定改归 session 的 `:176`、`:178`）、3e 5），全部出现在上面两张表的来源栏里。主表 60 个测试，其中新增 33 个；config 级 1 个。

## 8. 实现任务（TDD）

七个任务按顺序做，每个任务都先写失败的测试。§7 主表里的每个测试恰好落在下面某个任务的第一步；config 级的那一条按 §7 的说明不在这里。测试的公共脚手架放在 `crates/strata-session/tests/support/mod.rs`：建裸仓库和主 checkout、写配置、对每个仓库打开 `Git`、开 Store、一个测试用的 `RepoWriteLock`（记下每一段持锁）、一个测试用的 `BeforeCheckpoint`（不带 Tracker，转给 `overlay::checkpoint` 并计数）、`hdiutil` 卷、持有进程。它在 Task 1 建立，之后各任务按需扩充。

### Task 1: `new` 的写前拒绝

**Files:**
- Create: `crates/strata-session/src/lib.rs`、`crates/strata-session/src/ticket.rs`、`crates/strata-session/src/new/validate.rs`、`crates/strata-session/src/seams.rs`
- Test: `crates/strata-session/tests/new_refusals.rs`、`crates/strata-session/tests/support/mod.rs`

**Interfaces:**
- Consumes: types.md 的 `Config`、`RepoConfig`、`RepoName`、`RepoId`、`SessionId`、`StrataError`、`Code`；`apfs::{same_volume, nearest_existing_ancestor, volume_caps, classify_io_error, IoSide}`；`gitio::Git` 的 `repo_id`、`fetch`、`read_ref`、`is_ancestor`、`tree_of`；`deps::{RepoWriteLock, InstallEnv, main_tip}`；`store::Store` 的 `session`、`sessions`
- Produces: `Sessions::open`、`BeforeCheckpoint`；`Sessions::new` 的阶段 A 与第 8 步（§5.1 第 1–8 步）；`SessionSeam`、`set_next_id`、`arm`、`wait_paused`、`release`

- [ ] **Step 1: 写失败的测试**：`new_typed_refusals_leave_no_trace`、`new_passes_lower_codes_through_unchanged`、`new_validates_ticket`、`new_refuses_cross_volume_root`、`new_refuses_non_apfs_root`、`new_refuses_existing_session_dir_before_any_effect`、`new_refuses_root_aliased_into_checkout`、`new_refuses_symlinked_repo_path`、`new_refuses_fifo_seed_before_writing`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-session --features test-seams --test new_refusals`
Expected: FAIL（`Sessions` 还不存在）

- [ ] **Step 3: 写让测试通过的最少实现**：ticket 白名单与保留形式；`repos` 校验；每个仓库与 `paths.root` 的 `lstat`（符号链接按 `NotDirectory`）；`F_GETPATH` 取真实路径（errno 经 `classify_io_error`），按路径分量与 `RepoId` 做别名嵌套检查（§5.1 第 4 步）；同卷（`nearest_existing_ancestor`）与 `volume_caps`；显式种子与种子缓存来源的类型；生成会话 id 并检查 `Exists`；【锁 R】fetch 与同一段锁里的 `main_tip`（`None` → `NoSuchBranch`），`tree_of`。各码的 `detail` 按 §5.1。阶段 B 余下的部分这时返回一个占位错误，测试只覆盖拒绝的路径。接缝在 `test-seams` feature 下编译。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-session --features test-seams --test new_refusals`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-session/src/lib.rs crates/strata-session/src/ticket.rs crates/strata-session/src/new/validate.rs crates/strata-session/src/seams.rs crates/strata-session/tests/new_refusals.rs crates/strata-session/tests/support/mod.rs
git commit -m "feat(session): refuse invalid new requests before writing"
```

### Task 2: 基线 generation、种子判定与整套兄弟布局

**Files:**
- Create: `crates/strata-session/src/base.rs`、`crates/strata-session/src/seed.rs`、`crates/strata-session/src/new/mod.rs`
- Test: `crates/strata-session/tests/new_layout.rs`

**Interfaces:**
- Consumes: `gitio::Git` 的 `export_tree`、`tree_contains`、`is_ignored_in_export`（§2 的上游要求）、`ensure_shadow`、`attach_worktree`、`update_ref`；`deps::{Ctx, key_of, ensure, find_set, clone_into, nested_seed_caches, observe_main}`；`overlay::build_manifest`；`apfs::{list_dir, attrs_of, FileKind, open_rooted, mkdir_rooted, clone_tree_rooted, clone_file_rooted, symlink_rooted, rename_rooted, RenameMode, remove_rooted, RemoveKind, full_fsync, volume_caps, case_fold_key}`；`store::Store` 的 `base_dir`、`has_base_gen`、`record_base_gen`、`retire_dir`、`create_session`、`set_session_phase`、`record_seeds`、`set_lifecycle`
- Produces: `Sessions::ensure_base`、`Sessions::ensure_base_for`；`seed_plan`、`seed_files`、`SeedSource`、`SeedRecord`；`Sessions::new` 的第 9–16 步（§5.1）；`BASE_STAGING_DIR`

- [ ] **Step 1: 写失败的测试**：`ensure_base_builds_outside_the_lock`、`new_clones_entire_sibling_layout`、`new_workspace_equals_base_tree`、`new_workspace_keeps_seed_bytes_and_link_targets`、`new_refuses_explicit_seed_tracked_or_unignored`、`new_seed_writes_never_leave_the_session_root`、`new_pins_chain_top_when_ahead`、`new_pins_default_branch_when_publish_branch_differs`、`new_seeds_default_names_only_when_untracked_and_ignored`、`new_clones_seed_caches_as_real_directories`、`workspace_git_is_detached_and_prepush_refuses`、`manifest_round_trips_through_store`、`new_warm_under_5s`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-session --features test-seams --test new_layout`
Expected: FAIL（阶段 A 之后还没有实现，返回的是占位错误）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.2 的热路径与建基线，`ensure_base` 与 `ensure_base_for` 两种入口（暂存目录；锁外导出、`key_of` 与清单，`ensure_base_for` 只用已封存的依赖集；要认领或冷装时第一份暂存只作 `ensure` 的输入，之后退役、另建一份；清单一建好就在锁内复查、`clone_into`、改名、同步、登记，后登记的退役自己的暂存；删掉空的暂存；任何失败先退役暂存；暂存上的 `NoSuchPath` 当作已被收走，重建一次，再遇到 → `Busy` `base_staging_retired`）；§5.3 的四步判定（落点按卷折叠，条目表里折叠之后重名的按 `duplicate` 处理）与根之下的复制（符号链接种子按目标重建）；§5.1 第 10 步的种子缓存计划；`create_session`；阶段 B 与阶段 D 的 4 路派发（D41），阶段 D 一个阶段做完全部仓库再进下一个，每个阶段之前 `set_session_phase`；`observe`（`observe_main`，在 `finalize` 之前）；finalize 的 `full_fsync` 与 `Creating → Active`。阶段 D 的失败这时先原样返回，`Unknown` 现场在 Task 4 接上。`new_warm_under_5s` 标 `#[ignore]`。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-session --features test-seams --test new_layout`（性能测试另跑：`cargo test -p strata-session --features test-seams --test new_layout new_warm_under_5s -- --ignored`）
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-session/src/base.rs crates/strata-session/src/seed.rs crates/strata-session/src/new/mod.rs crates/strata-session/tests/new_layout.rs
git commit -m "feat(session): build the full sibling layout on per-session shadow repos"
```

### Task 3: 识别与状态

**Files:**
- Create: `crates/strata-session/src/resolve.rs`、`crates/strata-session/src/status.rs`
- Test: `crates/strata-session/tests/resolve_status.rs`

**Interfaces:**
- Consumes: `store::Store` 的 `session`、`sessions`、`latest_checkpoint`；`deps::{find_set, drift, open_proposals, Drift, Proposal, ProposalState}`；`gitio::Git` 的 `shadow_at`、`shadow_drift`、`is_ignored`，类型 `MirrorDrift`（含 `PrePushGuardDisabled`）；`overlay::checkpoint`（只在测试里用来造 checkpoint）；`apfs::attrs_of`
- Produces: `Sessions::resolve`、`Sessions::select`、`Sessions::list`、`Sessions::status`；`SessionStatus`、`SessionState`、`WorkspaceHealth`、`WorkspaceStatus`、`StatusWarning`、`MirrorDriftItem`

- [ ] **Step 1: 写失败的测试**：`new_same_ticket_creates_another_session`、`new_lost_reply_is_found_not_replayed`、`resolve_by_cwd_inside_workspace`、`select_by_id_or_ticket`、`sessions_on_one_base_are_isolated`、`seed_files_listed_in_status_and_never_checkpointed`、`status_warns_untracked_repo_edits_not_checkpointed`、`status_reads_workspace_presence_from_filesystem`、`status_reports_drift_and_pending_proposals`、`status_reports_mirror_drift_and_vanished_shadow`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-session --features test-seams --test resolve_status`
Expected: FAIL（`resolve`、`select`、`list`、`status` 还不存在）

- [ ] **Step 3: 写让测试通过的最少实现**：`F_GETPATH` 取真实路径，再按 §5.4 逐段匹配；按 id 或 ticket 选择；`list`；`status` 按 §5.5 的第 1、2.1–2.8、2.10 步：`Missing` 直接看文件系统与 `shadow_at`，影子仓库从不重建，`pending` 按 deps.md §4.5，种子逐个查 `is_ignored`，镜像漂移经 `shadow_drift`，restore 现场看目录。移动意图的两条告警在 Task 5 加上。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-session --features test-seams --test resolve_status`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-session/src/resolve.rs crates/strata-session/src/status.rs crates/strata-session/tests/resolve_status.rs
git commit -m "feat(session): resolve sessions by cwd or selector and report status"
```

### Task 4: `Unknown` 现场、基线残留与重启收尾

**Files:**
- Create: `crates/strata-session/src/scene.rs`、`crates/strata-session/src/recover.rs`
- Modify: `crates/strata-session/src/new/mod.rs`（阶段 D 的失败记成 `Unknown` 现场）、`crates/strata-session/src/base.rs`（写到一半的失败留下暂存）
- Test: `crates/strata-session/tests/unknown_scenes.rs`

**Interfaces:**
- Consumes: `store::Store` 的 `set_lifecycle`、`sessions`、`retire_dir`；`apfs::list_dir`；`gitio::EXPORT_TEMP_PREFIX`
- Produces: `Scene`、`SceneOp`、`Leftover`、`Sessions::leftovers`、`Sessions::recover`（会话与基线暂存部分）、`RecoverReport`；§6.3 的 `detail` 与 `message`

- [ ] **Step 1: 写失败的测试**：`new_failure_after_writing_began_is_unknown`、`new_unknown_detail_names_the_kept_scene`、`unknown_scene_survives_daemon_restart`、`base_staging_leftover_reported_then_retired_by_recover`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-session --features test-seams --test unknown_scenes`
Expected: FAIL（还没有现场记录、`leftovers` 与 `recover`）

- [ ] **Step 3: 写让测试通过的最少实现**：按 §6.1 给失败分类（`create_session` 之前与之后；内部暂存写到一半）；`Unknown` 的 `detail` 与 `message` 按 §6.3，同一个错误序列化进 `Scene`，经 `set_lifecycle` 存下；基线暂存的"正在用"集合与 `leftovers`（只报中途停下或退役失败留下的暂存）；`recover` 的第 1、3、4、5 步（§5.8）；接缝在每个阶段开始处生效。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-session --features test-seams --test unknown_scenes`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-session/src/scene.rs crates/strata-session/src/recover.rs crates/strata-session/src/new/mod.rs crates/strata-session/src/base.rs crates/strata-session/tests/unknown_scenes.rs
git commit -m "feat(session): keep Unknown scenes and collect leftovers after restarts"
```

### Task 5: `move_workspace`

**Files:**
- Create: `crates/strata-session/src/move_workspace.rs`、`crates/strata-session/src/carry.rs`
- Modify: `crates/strata-session/src/recover.rs`（移动意图）、`crates/strata-session/src/status.rs`（`MoveLeftover`、`OldTreeKept`）
- Test: `crates/strata-session/tests/move_workspace.rs`

**Interfaces:**
- Consumes: Task 1 的 `BeforeCheckpoint`（`CheckpointReason::BeforeRollout`）；`overlay::{reconcile_full, rebase, RebaseOptions, project, Overlay, Base, WorkspaceRef, Rebased, Checkpointed}`；`deps::{find_set, open_files, nested_seed_caches, OpenFile}`；`gitio::Git` 的 `tree_of`、`merge_base`、`shadow_at`、`read_index`、`attach_worktree`、`update_ref`；`apfs::{walk_with, WalkOptions, attrs_of, FileKind, mkdir_rooted, clone_tree_rooted, clone_from_rooted, CloneKind, clone_file_rooted, symlink_rooted, rename_rooted, RenameMode, open_root, open_rooted, full_fsync}`；`store::Store` 的 `manifest`、`has_base_gen`、`checkpoints`、`begin_move`、`set_move_phase`、`set_move_state`（进入 `Swapping` 时带冲突，§2 的上游要求）、`finish_move`、`fail_move`、`set_old_tree`、`move_intents`、`open_move_intents`、`retire_dir`；Task 2 的 `ensure_base`、`ensure_base_for`（测试在拿锁之前调用它们）
- Produces: `Sessions::move_workspace`、`Sessions::repo_name`、`MoveReport`、`IntentRef`、`MOVE_STAGING_PREFIX`；`recover` 的意图部分（§5.8 第 2 步）

- [ ] **Step 1: 写失败的测试**：`base_generation_never_modified_in_place`、`move_workspace_validates_before_writing`、`move_workspace_swaps_base_and_deps_atomically`、`move_workspace_rechecks_open_files_before_swap`、`move_workspace_projects_overlay_and_marks_conflicts`、`move_workspace_carries_ignored_content`、`move_workspace_keeps_old_tree_while_in_use`、`move_workspace_seconds_warm`、`recover_abandons_pre_visible_move_intents`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-session --features test-seams --test move_workspace`
Expected: FAIL（`move_workspace` 还不存在）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.7 的 21 步与 §5.7.1。要点：RepoId 到 RepoName 的映射与不匹配规则；检查阶段不写任何东西；收走上一次留下的 `Preparing` 意图；目标基线没登记就以 `base_unbuilt` 拒绝，从不建基线、从不调 `ensure`；`BeforeRollout` 经 `BeforeCheckpoint`；`begin_move` 在建任何暂存之前；克隆、`rebase`、`project`、携带（`CloneKind` 按遍历看到的种类，符号链接经 `attrs_of` 读目标再 `symlink_rooted` 重建，克隆之前源变了的记进 `uncarried`，`move.carry` 接缝）、`attach_worktree`、同步；复查打开的文件；`Preparing → Swapping` 紧挨着交换，同一个事务记下冲突（D46）；`RenameMode::Swap`；CAS 移 pin；`finish_move`；交换之后失败经 `fail_move`；旧树的核对、保留与退役。准备阶段的失败按第 13 步之后的两种处理。`recover` 加上意图，`status` 加上两条意图告警。`move_workspace_seconds_warm` 标 `#[ignore]`。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-session --features test-seams --test move_workspace`（性能测试另跑：`cargo test -p strata-session --features test-seams --test move_workspace move_workspace_seconds_warm -- --ignored`）
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-session/src/move_workspace.rs crates/strata-session/src/carry.rs crates/strata-session/src/recover.rs crates/strata-session/src/status.rs crates/strata-session/tests/move_workspace.rs
git commit -m "feat(session): move base and deps together with an atomic directory swap"
```

### Task 6: `remove`

**Files:**
- Create: `crates/strata-session/src/remove.rs`
- Test: `crates/strata-session/tests/remove.rs`

**Interfaces:**
- Consumes: types.md 的 `ConfirmToken`、`Action`；`overlay::reconcile_full`；Task 1 的 `BeforeCheckpoint`（`CheckpointReason::BeforeRemove`）；`deps::open_files`；`gitio::Git` 的 `read_ref`、`delete_ref`；`store::Store` 的 `set_lifecycle`、`set_session_phase`、`set_move_state`、`open_move_intents`、`retire_dir`
- Produces: `Sessions::remove`、`RemoveReport`、`DIRTY_PATHS_CAP`

- [ ] **Step 1: 写失败的测试**：`workspace_git_never_writes_main_checkout`、`removed_session_id_never_resolves_or_reissued`、`remove_refuses_unpublished_without_discard`、`remove_untracked_repo_requires_discard`、`remove_discard_checks_the_token`、`remove_after_mtime_only_change_needs_no_discard`、`remove_refuses_when_a_process_holds_fd_cwd_or_mmap`、`remove_open_files_in_one_session_never_block_another`、`remove_has_no_force_path`、`remove_unresolved_scene_requires_discard_confirmation`、`remove_failure_midway_is_unknown_and_keeps_scene`、`remove_deletes_only_its_own_session`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-session --features test-seams --test remove`
Expected: FAIL（`remove` 还不存在）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.6 的九步。要点：生命周期的检查；核对凭据的 `action`；对会话根与本会话影子仓库查打开的文件，`discard` 不绕过；`reconcile_full` 判断未发布改动，加上 `tracked = false`、未决状态与有改动的旧树三条规则，路径上限 `DIRTY_PATHS_CAP`；收走 `MoveLeftover`（含 Abandon 之后没退役的暂存）；`BeforeRemove` 经 `BeforeCheckpoint`，每次新记一行；进入 `Removing`；按 CAS 删 pin；`retire_dir` 会话根与影子仓库目录；记 `Removed`；中途失败记 `Unknown` 现场。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-session --features test-seams --test remove`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-session/src/remove.rs crates/strata-session/tests/remove.rs
git commit -m "feat(session): remove sessions behind discard confirmation and open-file checks"
```

### Task 7: 处理失败的移动

**Files:**
- Create: `crates/strata-session/src/failed_move.rs`
- Modify: `crates/strata-session/src/status.rs`（`Abandoned` 而暂存还在的意图报 `MoveLeftover`）、`crates/strata-session/src/move_workspace.rs`（第 9 步也收走这样的暂存）、`crates/strata-session/src/remove.rs`（第 5.1 步同上）
- Test: `crates/strata-session/tests/resolve_failed_move.rs`

**Interfaces:**
- Consumes: `gitio::Git` 的 `worktree_gitdir`（§2 的上游要求）、`shadow_at`、`read_ref`、`update_ref`；`store::Store` 的 `resolve_move`（§2 的上游要求）、`session`、`move_intents`、`retire_dir`；Task 5 的 `move_workspace` 与接缝（造出两种现场）
- Produces: `Sessions::resolve_failed_move`、`MoveResolution`、`ResolutionReport`

- [ ] **Step 1: 写失败的测试**：`resolve_failed_move_abandons_when_never_swapped`、`resolve_failed_move_accepts_when_swapped`、`resolve_failed_move_refuses_a_disagreeing_scene`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-session --features test-seams --test resolve_failed_move`
Expected: FAIL（`resolve_failed_move` 还不存在）

- [ ] **Step 3: 写让测试通过的最少实现**：§5.9 的十步。要点：阶段 A 只读，会话、健康、最新意图的检查，两份 gitfile 与 pin 对出现场，再与意图的 phase 和人的选择核对，对不上给 `scene_mismatch` 与 `matches`；Abandon 先 `resolve_move` 再退役暂存，退役失败只记进 `leftover`；Accept 先【锁 R】按 CAS 移 pin（已移过就跳过），再 `resolve_move`，之后失败是 `Unknown`（`op = "resolve"`）；`resolve.*` 接缝。`status`、`move_workspace` 第 9 步与 `remove` 第 5.1 步认出 `Abandoned` 而暂存还在的意图。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-session --features test-seams --test resolve_failed_move`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/strata-session/src/failed_move.rs crates/strata-session/src/status.rs crates/strata-session/src/move_workspace.rs crates/strata-session/src/remove.rs crates/strata-session/tests/resolve_failed_move.rs
git commit -m "feat(session): resolve a failed move by abandoning or accepting it"
```

## 9. 构建者决定记录

正文里每个 [我的决定 Dn] 在这里有一行，编号相同。最后两行是 design 交给构建者、已由裁定定下的事项，不是本文的决定。

| 事项 | 选择 | 理由 | 日期 |
| --- | --- | --- | --- |
| D1 ticket 的格式 | `^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`；形如会话 id 的 → `ReservedName` | ticket 出现在选择器与 `publish --from <session>:<path>` 里，不能含 `:`、`/`；与 id 同形会让选择器有歧义 | 2026-10-22 |
| D2 主 checkout 的 `path` 或 `paths.root` 是符号链接 | `NotDirectory`（`reason = "symlink"`），不解析 | 同卷、不嵌套、只读这些保证要按真实位置成立；3b `:15`、3e `link` 把选择留给本文 | 2026-10-22 |
| D3 经别名才嵌套的写法 | `new` 阶段 A 以 `F_GETPATH` 取真实路径，按路径分量（大小写不敏感的卷上按 ASCII 折叠）与 `RepoId` 判定，`InvalidInput`（`rule = "nesting"`） | `Config::validate` 不做 I/O（增补 1）；拦不住时 session 的写会落进人的仓库（review I2） | 2026-10-22 |
| D4 会话 id 的生成与冲突 | `arc4random_buf` 取 40 bit，`s-` 加 8 位小写 base32；撞上已有的行或目录 → `Exists`，不换 id 重试 | macOS 自带，不加依赖；碰撞可以忽略，真撞上多半是 bug 或手工建的目录，报出来比绕开好 | 2026-10-22 |
| D5 `new` 的 fetch 算什么 | 缓存填充：在 `create_session` 之前，失败原样透传 | 它写的是 gitio 对 origin 的观察，以后的操作复用、重做结果相同；增补 6 定 `create_session` 为第一次对外可见的写，而 fetch 必须在它之前 | 2026-10-22 |
| D6 找已有的基线 generation | 列出 `bases/<repo>/<tree>/`，对一个已登记的 generation 目录算 `deps::key_of`，再 `has_base_gen` | 同一个 tree 的各 generation 源码与安装输入相同，热路径不必导出 | 2026-10-22 |
| D7 建基线的暂存 | `bases/<repo>/.staging/<32 位十六进制>/`，导出进其中的 `tree/`；清单一建好就登记；要认领或冷装依赖集时，这份暂存只作 `ensure` 的输入，之后退役，另建一份再登记；任何失败都先退役暂存，写到一半的导出或克隆随它离开，返回的 `Unknown` 里 `kept` 为空；暂存上的 `NoSuchPath` 当作已被 `gc_repo` 收走，重建一次，再遇到 → `Busy`（`base_staging_retired`）；只有退役本身失败时暂存才留下，`leftovers()` 报出，`recover` 或 `gc_repo` 收走 | module-rulings "Base staging is registered right after its manifest" 定了：没登记的暂存只有年龄下限护着，`gc_repo` 在锁里收走过了下限的，所以暂存不能跨过装依赖这样的长步去等登记；`NoSuchPath` 的含义与 `base_staging_retired` 也由它定。本文定的是做法：认领或冷装时 deps 只在开头读这份导出，之后重建一份，比设法延长它的寿命简单，代价只是那一条路上多一次导出；失败时它里面的东西是缓存填充的中间产物，从未对外可见，不是要保留的现场。缓存填充没有意图行（增补 6）；`retire_dir` 第 7 行按形状接受它（store.md @0062a3e） | 2026-10-22 |
| D8 复查时依赖集一再被回收 | 连续 3 轮 `find_set` 落空 → `Busy`（`reason = "deps_set_race"`） | 与 deps 的 `proposal_race` 同一做法；有界，不无限重试 | 2026-10-22 |
| D9 种子文件的判定次序 | 来源类型 → 被跟踪 → 落点（上级都是真目录、基线里没有同一个文件，按卷折叠；条目表里折叠之后重名的，后一个按 `duplicate` 处理）→ 被 ignore；候选任何一步不满足都静默跳过，显式的给具名码 | 对着基线 generation 在 `create_session` 之前判定（review I3、M10）；落点检查保证之后根之下的写一定成功。"同一个文件"按卷判定由 module-rulings "Disk identity follows the volume, git identity follows core.ignorecase" 定；折叠之后重名的两个条目在大小写不敏感的卷上是同一个文件，第二次复制会在 `create_session` 之后撞上 `Exists`、只能报 `Unknown`，所以在计划里就拒绝或跳过 | 2026-10-22 |
| D10 种子文件是符号链接 | 按目标重建：计划经 `attrs_of(main, p)` 读出目标，复制时 `symlink_rooted(to, p, 目标)`，目标字节不变；从不克隆、从不跟随；主 checkout 里它的某一级上级是符号链接 → 显式 `NotDirectory`，候选跳过 | module-rulings "A symlink is recreated, never cloned"：按路径的 `clone_file_rooted` 在源一侧跟随中间级的符号链接。与工作区里其它符号链接同一待遇；跟随会把链接指向的内容带进工作区。普通文件的种子仍按路径从主 checkout 克隆：那是人自己的树，经过人自己的链接取文件（如指向共享目录的 `.claude`）正是种子的本意 | 2026-10-22 |
| D11 种子缓存与种子文件给哪些仓库 | 种子缓存只给 tracked 仓库；种子文件给每个仓库 | 缓存只为加速构建，`tracked = false` 的兄弟库不在会话里构建，而 `target` 动辄几十万条目；种子文件小，运行兄弟库需要它 | 2026-10-22 |
| D12 种子缓存的落点被挡 | 静默跳过 | 缓存只是加速；缺省列表的条目不该因为某个仓库的形状让 `new` 失败，也不该到 `create_session` 之后才发现 | 2026-10-22 |
| D13 克隆工作区时拿不拿锁 | 不拿 | 第 11 步写下的工作区行已经引用这个 generation，gc 不回收被引用的 generation | 2026-10-22 |
| D14 `new` 的持久化 | 进入 `Active` 之前对会话根与影子仓库目录做 `full_fsync` | 记成 `Active` 的会话要扛得住断电（增补 3 "Durability"） | 2026-10-22 |
| D15 `observe_main` 在 `new` 里的位置 | 阶段 D 的 `observe`，在 `finalize` 之前；它的任何失败都按 `create_session` 之后的失败处理：`Unknown`，会话 `Creating → Unknown` | 它写的提议与主干记录是对外可见的写，又在 `create_session` 之后（增补 6），失败只能是 `Unknown`、现场保留；放在 `finalize` 之前，失败时会话还在 `Creating`，走生命周期图里已有的边 | 2026-10-22 |
| D16 `resolve` 的签名与算法 | `Result<Option<…>>`；真实路径，`sessions/` 下一段是 id，再下一段是清单里的 `RepoName`，会话没删除 | Store 的错误不该被吞成"不在会话里"；只认工作区，暂存与影子仓库不算 | 2026-10-22 |
| D17 `list` 列哪些会话 | 不是 `Removed` 的，含 `Creating` 与 `Unknown` | 未决的现场要能被人看到，才能 `rm --discard` | 2026-10-22 |
| D18 种子文件"过期" | 来源的 mtime 与记录不同，或来源已不在 → `SeedStale` | 只读元数据，不读内容；只是告警 | 2026-10-22 |
| D19 `SeedNotIgnored` 的查法 | 每个种子逐个 `is_ignored`；工作区不在时不查 | 种子通常只有几个；`is_ignored` 要经工作区的 `.git`，`Missing` 已经说明了问题 | 2026-10-22 |
| D20 影子仓库不见了 | 工作区报 `Missing`，另给 `ShadowMissing`；从不重建 | 重建会按今天的主 checkout 重取镜像，并丢掉会话的 ref 与 stash（增补 2、gitio 复审） | 2026-10-22 |
| D21 status 里工作区当前的改动 | 不在 `SessionStatus` 里；daemon 读 status 时用 `overlay::compute` 算出并入 | `Tracker` 归 daemon；交换之后要冷扫（增补 3 "Projection"，review I9） | 2026-10-22 |
| D22 保留的旧树的 `changed` | 读的时候重算：冷扫旧树，与那次移动的 `BeforeRollout` checkpoint（时刻不晚于意图 `started` 的最新一个）比较 | 交换之后写进旧树的东西没有 checkpoint，只能每次重算（review I5） | 2026-10-22 |
| D23 删除 `Creating`、`Removing` 的会话 | `InvalidInput`（`reason = "lifecycle"`） | 生命周期图里没有这两条边；崩溃留下的由 `recover` 记成 `Unknown` | 2026-10-22 |
| D24 `remove` 的强制路径 | 没有；`OpenFiles` 连 `--discard` 也绕不过，不发信号 | 3d `fp32` 三行；删掉进程脚下的目录是数据丢失 | 2026-10-22 |
| D25 `Dirty` 细节的大小 | 每个仓库最多 `DIRTY_PATHS_CAP` = 100 个路径，超出时 `truncated = true` | 细节要放进一条 JSON 回复；完整的列表在 status 与 checkpoint 里 | 2026-10-22 |
| D26 `Unknown` 会话的 `BeforeRemove` | 不记 | 工作区可能是半成品，扫描会失败；人已经经 `--discard` 确认丢弃 | 2026-10-22 |
| D27 删 pin 与退役目录 | pin 按刚读到的值 CAS 删（`delete_ref`），不存在就跳过；`retire_dir` 得到 `NoSuchPath` 就跳过 | 交换之后、记录之前崩溃的工作区，pin 已经指向新 head（review M20）；`new` 早期失败的会话可能没有目录 | 2026-10-22 |
| D28 移动 `tracked = false` 的仓库 | `InvalidInput`（`reason = "untracked_repo"`） | 增补 1 规定它们从不移动；没有现成的码，按共同规则映射到最接近的 | 2026-10-22 |
| D29 对未决现场移动 | 会话或工作区是 `Unknown`、restore 现场在 → 以 `Unknown` 拒绝，什么都不做 | 现场只能由人处理；与 overlay 的 restore 前提相同 | 2026-10-22 |
| D30 移动 `Creating`、`Removing` 的会话 | `InvalidInput`（`reason = "lifecycle"`） | 会话存在，不是 `NoSuchSession`（review M13） | 2026-10-22 |
| D31 `move_workspace` 与基线、依赖集 | 只用已封存的依赖集（`find_set`，没有 → `InvalidInput` `deps_unsealed`）与已登记的基线 generation（`has_base_gen`，没有 → `InvalidInput` `base_unbuilt`），从不调 `ensure`、从不建基线；基线由调用方在拿锁之前经 `ensure_base` 或 `ensure_base_for` 建好（§5.2） | module-rulings "Base generations are built outside the lock" 定下锁里只找、缺了就在写之前类型化拒绝，取代上一版"缺的基线在调用方的锁里建"；冷装不能在写锁里跑（增补 3 "Deps API"、"Locks"）。码是本文选的：缺基线是调用方没按约定先建，不是存储损坏（`Corrupt`）也不是路径问题（`NoSuchPath`），所以用 `InvalidInput` 加 reason，与 `deps_unsealed` 同一做法；码表封闭，不加码 | 2026-10-22 |
| D32 移动暂存的名字 | `<会话根>/.strata-tmp-move-<repo>-<16 位十六进制>/` | 与 restore 暂存同族；代数在 `begin_move` 之前还不知道，随机后缀避免撞名；形状是 store 的 `begin_move` 要的 `sessions/<id>/<name>` | 2026-10-22 |
| D33 准备阶段的失败与 `MoveLeftover` | 下层具名码 → 退役暂存、意图 `Refused`、透传；下层 `Unknown` → 返回 `Unknown`，暂存与 `Preparing` 的意图留着，由下一次移动、`remove` 或 `recover` 收走；`MoveLeftover` 不算 `rm` 的未决状态 | 写到一半的暂存说不准是什么，不能当作具名拒绝；但它从未对外可见、工作区原样，所以不需要人确认（增补 3） | 2026-10-22 |
| D34 携带被 ignore 的内容 | 整块克隆：文件与目录经 `clone_from_rooted`（`CloneKind` 按遍历看到的种类）到 `S` 根下的私有临时名，再 `rename_rooted(Excl)` 到位；符号链接经 `attrs_of` 读出目标、`symlink_rooted` 重建，从不克隆；从不覆盖新树，挡住的、对属主不可读的、遍历之后种类变了的（`IsDirectory`、`NotFile`、`NotDirectory`）记进 `uncarried`，不见了的跳过；带 `uchg`、`uappnd` 的条目让移动以 `ReadOnly` 被拒绝，session 不清旗标；不经 blob store；耗时 O(条目数) | 源与目的两侧都不跟随符号链接（增补 3、8；module-rulings "A symlink is recreated, never cloned"：按路径克隆链接会在源一侧跟随中间级）；构建产物不该进 blob store；改名携带要有回退步骤，而交换之前的拒绝会让旧工作区少一块（review M14）；与 agent 并发的写不该让移动失败（module-rulings "clone_from_rooted states its source kind"）；`Attrs` 不带旗标，克隆之前看不出来，而清掉旗标会改掉人有意设下的保护 | 2026-10-22 |
| D35 交换之后的旧树 | 没人用、没改过、没有 `uncarried` → 连同旧管理目录退役；否则留在原地。有 `uncarried` 或改动的经 `set_old_tree` 记下（`retained`），只随 `remove` 退役；只因为有进程持有而保留的不记，以后的移动在它没人用、没改过时收走 | 交换之后不再添一步可能半途而废的目录操作；`Done` 意图点名它，`retire_dir` 随时拿得走。agent 的 cwd 多半在工作区里，几乎每次移动都保留一棵旧树，要能自动收走；但有新树没有的东西的旧树不能被悄悄删掉 | 2026-10-22 |
| D36 `MoveReport` 的字段 | 另带 `lww`、`uncarried`、`old_tree`；`checkpoint` 为 `Option`（目标与当前相同时什么都不做） | 调用方要知道哪些路径整份取了会话一侧、哪些内容还在旧树里 | 2026-10-22 |
| D37 `recover` 遇到单项失败 | 记进 `RecoverReport.errors`，接着处理下一项 | 一项坏掉的现场不该让 daemon 起不来；错误由 daemon 写日志并在 status 报出 | 2026-10-22 |
| D38 测试注入 | `test-seams` feature 下手写的 `SessionSeam`（`FailAt`、`StopAt`、`PauseAt`）与 `set_next_id`，不引入 failpoint crate；位置是 §6.3 的阶段名（记下阶段的操作先记阶段名再检查接缝），外加只给接缝用的 `move.carry`；`new` 里按仓库并行的阶段用 `@<repo>` 后缀指定仓库 | 只需在阶段开始处注入 errno、就地返回或停住；`PauseAt` 满足要在两步之间做事的测试（增补 4，review M17）；携带的竞争要停在遍历与克隆之间，并行的阶段要能指定哪个仓库失败 | 2026-10-22 |
| D39 `Sessions` 句柄借用什么 | `Config`、每个 repo 对主 checkout 打开的 `Git`、`Store`、daemon 的写锁与安装环境，以及 daemon 实现的 `BeforeCheckpoint`（D44）；daemon 持有，按引用交给 publish | `RepoId` 就是 `git.repo_id()`；写锁与安装环境是 deps 的 `Ctx` 要的（review M23） | 2026-10-22 |
| D40 `seed_files` 的形状 | 拆成 `seed_plan`（判定，`create_session` 之前）与 `seed_files`（复制，之后） | 计划的一步式签名会把判定推到第一次对外可见的写之后（review I3，增补 6） | 2026-10-22 |
| D41 fetch、建基线、克隆的并发度 | 阶段 B 与阶段 D 都按 `RepoName` 的顺序把仓库派发给最多 4 个并行的工作者；阶段 D 一个阶段对全部仓库做完才进入下一个 | 22 个仓库串行做很可能超过 5 s（§6.5）；4 路是增补 8 实测克隆速率时的并发度；按阶段并行而不是每个仓库各走一条流水线，会话任何时刻只有一个 phase，现场与 `recover` 报出的阶段才明确。首个 spike 用 `new_warm_under_5s` 实测 2、4、8 路之后复核 | 2026-10-22 |
| D42 交换之前的持久化 | 对暂存 `S` 与会话根做 `full_fsync` | `project` 不同步；交换之后新树必须扛得住断电（增补 3 "Durability"） | 2026-10-22 |
| D43 `MirrorDrift` 在 status 里的形状 | 每项一个 `MirrorDriftItem { reason, key?, hooks_path?, hook_replaced? }`，reason 取 `info_exclude`、`info_attributes`、`config`、`pre_push_guard_disabled` | gitio 的 `MirrorDrift` 不 derive serde；守卫失效那一项带两个字段，拼成字符串就丢了；reason 与 module-rulings T13 条写的 `pre_push_guard_disabled` 一致 | 2026-10-22 |
| D44 Before* checkpoint 的入口 | `Sessions::open` 收一个 daemon 实现的 `&dyn BeforeCheckpoint`，只有一个方法 `checkpoint(ws, base, reason)`；daemon 的实现用它为这个工作区持有的 Tracker，没有时传 None；`rm` 的未发布改动检查仍是一次 `reconcile_full` | module-rulings "Session's Before* checkpoints use the daemon's Tracker" 让 daemon 交来 Tracker 或闭包，形状由本文定。Tracker 归 daemon，每个工作区一个，随监听增删，而 `Sessions` 是长期句柄：借出 `&Tracker` 就要处理它的生命周期与并发，trait 对象让 daemon 在自己的锁里挑 Tracker 或 None，session 只看见一次调用；测试实现转给不带 Tracker 的 `overlay::checkpoint` 并计数，能证明 session 没有自己去调。`rm` 的检查是删除前唯一的一次判断，读遍候选文件换来不依赖任何缓存的答案（design §8：tool 全量对账 < 3 s）；Tracker 要省的是 rollout 时对每个会话的那一次 | 2026-10-22 |
| D45 处理失败的移动的形状 | 一个函数 `resolve_failed_move(session, repo, MoveResolution::{Abandon, Accept})`，返回 `ResolutionReport`。先只读核对：两份 gitfile（经 gitio 的 `worktree_gitdir`）、pin、意图的 phase，对不上 → `InvalidInput`（`scene_mismatch`）并给出 `matches`。Abandon 先 `resolve_move` 再退役暂存，退役失败不算错误（`MoveLeftover`）；Accept 先按 CAS 移 pin，再 `resolve_move` | module-rulings "Resolving a failed move" 定了两种处理与先核对再动手，形状由本文定。一个函数加一个选择，对应 cli 的"一个显式命令"；用 `InvalidInput` 加 reason，不加码（码表封闭），`matches` 让人不必猜另一种。Abandon 先改 store：若先退役暂存、再记 store 时失败，现场已经变了，再试会因为对不上而永远被拒；暂存是内部的，晚些收走无妨。Accept 先移 pin：store 记成 `Done` 时 pin 必须已是新 head，否则健康是 `Ok` 而 pin 落后；pin 移过、store 失败时，再试一次会跳过 pin，是幂等的 | 2026-10-22 |
| D46 移动的冲突记在意图上 | 进入 `Swapping` 的同一个事务里，把 `rebase` 的冲突路径记在意图行上（§2 对 store 的要求第 5 条） | `rebased` 只在内存里：交换之后失败、进程又崩溃，冲突就找不回来了。Accept 要把冲突交回工作区行，它们才会继续挡住发布（design §6.5 第 7 步）。在 `Swapping` 之前记下，交换与冲突就在同一条记录里，`finish_move` 照旧在完成时写工作区行 | 2026-10-22 |
| 工作区 `.git` 的接法（design §12 交给构建者） | 每个会话、每个 repo 一个影子仓库（§4.2） | 裁定增补 2 定下 | 2026-10-22 |
| `rm` 前最后一个 checkpoint 的保留期（design §6.6） | 由 store 的 gc 按 `keep` 回收；session 只在 `RemoveReport` 里给出 `CheckpointId` | 裁定增补 3 定下 | 2026-10-22 |
