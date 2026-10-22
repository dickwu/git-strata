# git-strata 模块文档阶段 — 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在写任何产品代码之前，产出 git-strata 全部模块的开发文档和一份把 LayerFS / tgrep 每条测试映射到模块的测试矩阵，使任何没见过这段对话的工程师能据此逐模块实现。

**Architecture:** 一份共享类型文档（`docs/types.md`）先钉住所有跨模块的名字和形状；测试矩阵按来源分 6 个部分、由脚本生成骨架保证条数对齐；11 份模块文档各自引用类型文档和矩阵中属于自己的行，并以 TDD 任务收尾。文档之间的一致性由机械检查（标题、标识符、条数）加审阅者把关。

**Tech Stack:** Markdown；检查用 `rg`/`awk`/`node -e` 一行命令（不写产品代码）；来源仓库 `/Volumes/GWD/opensource/layerfs`（main 44d3a26e5）与 `/Volumes/GWD/opensource/tgrep`（d25ade6）。

**Spec:** `docs/design.md`（已确认，a6c6190）。本计划只决定"文档怎么写、写什么、怎么核对"；产品行为以 design.md 为准。

## Global Constraints

- 平台：macOS 26+（本机 Darwin 27.0.1），Apple Silicon，APFS；不做 FSKit/FUSE/容器/VM。
- 语言：Rust，edition 2024，`rust-version = "1.88"`（`tgrep-core` 的下限）；索引层以 `tgrep-core` 为库依赖。
- 命名：项目/仓库/二进制 `git-strata`（可作 `git strata`），daemon `git-stratad`，refs `refs/strata/*`，目录 `.strata/`，环境变量前缀 `STRATA_`。
- Store、基线缓存、依赖集缓存、工作区同卷，默认 `/Volumes/GWD/inter/.strata/{store,bases,deps,sessions}/`；`clonefile` 跨卷是错误，不是降级。
- 依赖共享只用 `clonefile` 的 CoW 克隆，**永不 symlink**。
- 批次按整文件路径；检查命令一律只读；发布只提交批次路径；推送只推配置目标。
- 人类确认 = LocalAuthentication 通过 + stdin 为 TTY；范围：一推上线的推送、`deps approve`、`rm --discard`；没有任何 flag/环境变量/配置能绕过。
- 对主 checkout 只读（读 `.git` 与种子文件），写只限 `refs/strata/*`。
- 会话基线与依赖集永远一起移动。
- **本阶段不写产品代码、不建 Cargo 脚手架、不装依赖。**
- 文档语言：中文正文 + 英文标识符；每份文档一次 commit，`docs:` 前缀，不加署名行。

## Review Focus

design.md 隐含、但若不写进模块文档就会被漏掉的五类失败，每条都必须出现在指定模块文档的 §7 测试清单里（Task 4–14 的验收项）：

1. **跨卷克隆**：sessions 根与主 checkout 不在一个卷 → `clonefile` 会退化成复制。期望：`new` 在克隆前拒绝（`CrossVolume`），不悄悄复制。→ `apfs.md`：`clone_dir_refuses_cross_volume`。
2. **监听洪峰丢事件**：`bun install` 期间 FSEvents 报 `MustScanSubDirs`/`KernelDropped`。期望：该子树重扫后的 overlay 与冷扫完全一致。→ `overlay.md`：`reconcile_after_kernel_dropped_equals_cold_scan`。
3. **dev server 脚下换依赖**：rollout 时某会话的依赖目录里有被打开的文件。期望：跳过并报告，目录一字不动。→ `deps.md`：`rollout_skips_session_with_open_files`。
4. **只改大小写的改名**：APFS 大小写不敏感，`Foo.ts` → `foo.ts`。期望：overlay 记为改名，不是"删除 + 未跟踪"，发布后 git 里也是改名。→ `overlay.md`：`rename_case_only_recorded_as_rename`。
5. **推送中途超时**：发布链已前进、`git push` 无应答。期望：链保留、状态记为 `Unknown`、不重放、`status` 报出。→ `publish.md`：`push_timeout_keeps_chain_reports_unknown`。

---

## 文件结构

```text
docs/
  README.md                 阅读顺序与索引（Task 1）
  design.md                 设计（已确认）
  types.md                  共享类型、错误码、crate 布局（Task 1）
  config.md                 配置文件格式与各仓库默认值（Task 2）
  test-matrix.md            矩阵索引与总数（Task 15）
  test-matrix/
    layerfs-store-layer.md  content/storage/persistence/history（Task 3a）
    layerfs-project-telemetry.md  project/telemetry（Task 3b）
    layerfs-workspace-layer.md    overlay/workspace/store-service（Task 3c）
    layerfs-replaced.md     api/daemon/fuse/sandbox/bridge（Task 3d）
    layerfs-sdk-js.md       sdk/js/test（Task 3e）
    tgrep-core.md           tgrep-core（Task 3f）
  modules/
    _template.md            模块文档模板（Task 1）
    apfs.md gitio.md store.md overlay.md deps.md session.md
    publish.md search.md confirm.md daemon.md cli.md    （Task 4–14）
  plans/2026-10-22-module-docs.md   本计划
```

未来的 Cargo 布局（只在 `types.md` 里描述，本阶段不创建）：

```text
Cargo.toml                       workspace, edition 2024, rust-version 1.88
crates/strata-types              ids、错误、配置结构（无 I/O）
crates/strata-apfs               clonefile / getattrlistbulk / gen count / FSEvents
crates/strata-gitio              git 子进程封装、.git/index 解析、diff3
crates/strata-store              SQLite 元数据 + blob
crates/strata-overlay            overlay 计算、checkpoint、restore
crates/strata-deps               依赖集 generation、认领、rollout
crates/strata-session            清单、兄弟布局、cwd 识别
crates/strata-publish            批次、发布组、staging、发布链、发布后 rebase
crates/strata-search             tgrep-core 集成
crates/strata-confirm            LocalAuthentication + TTY
crates/strata-daemon             bin git-stratad：launchd、RPC、调度
crates/git-strata                bin git-strata：CLI
```

## 任务依赖

```text
Task 1 (types + template + README) ─┬─► Task 3a…3f（矩阵，可并行）─┬─► Task 4…14（模块文档，可并行）─► Task 15（索引 + 一致性）
Task 2 (config) ────────────────────┘                             │
                                                                   └─ 每份模块文档只依赖 types.md、config.md 和自己那部分矩阵
```

---

### Task 1: 共享类型文档、模块模板、README

**Files:**
- Create: `docs/types.md`
- Create: `docs/modules/_template.md`
- Create: `docs/README.md`

**Interfaces:**
- Produces（后续每份文档只能用这里定义的名字，不得另起）:
  - 标识：`RepoName(String)`（配置里的名字，如 `tool`）、`RepoId`（git common dir 的规范路径）、`SessionId(String)`（生成，`s-` + 8 位 base32）、`TreeOid`（40 位十六进制）、`CommitOid`、`BlobId`（BLAKE3 十六进制）、`DepsSetId`（BLAKE3(安装输入 ∥ 平台 ∥ 工具链) 十六进制）、`CheckpointId(u64，每会话递增)`、`RepoPath`（仓库内相对路径，`/` 分隔，NFC）。
  - 结构：`BaseGen { repo: RepoName, tree: TreeOid, deps: DepsSetId }`；`DepsSet { id: DepsSetId, repo, inputs_hash, platform, toolchain, dirs: Vec<RepoPath> }`；`OverlayEntry { path: RepoPath, kind: EntryKind, mode: u32 }` 与 `EntryKind = Added{blob} | Modified{blob} | Deleted | Renamed{from, blob} | Untracked{blob}`；`Checkpoint { id, session, repo, at: SystemTime, reason: CheckpointReason, entries: Vec<OverlayEntry> }`；`CheckpointReason = ToolCall{id} | Manual{message} | BeforePublish | BeforeRestore | BeforeRollout | BeforeRemove`；`Batch { session: SessionId, repo: RepoName, checkpoint: CheckpointId, paths: Vec<RepoPath> }`；`ReleaseGroup { id, batches: Vec<Batch> }`；`PublishTarget { branch: String, push: bool, confirm: bool }`；`SessionManifest { id, ticket: Option<String>, root: PathBuf, repos: BTreeMap<RepoName, WorkspaceState> }`；`WorkspaceState { base: BaseGen, path: PathBuf, tracked: bool }`。
  - 错误：`StrataError { code: Code, message: String, detail: Option<serde_json::Value> }`；`Code` 枚举 = LayerFS `sdk/js/src/errors.js` 的词汇表（`Conflict, HeadMoved, Dirty, Busy, NoSuchPath, NoSuchBranch, NoSuchCommit, NoCommit, NoMergeBase, IsDirectory, NotDirectory, NotFile, Binary, NotText, NoMatch, AmbiguousMatch, Exists, NotEmpty, NameTaken, ReservedName, ReadOnly, TooLarge, InvalidInput`）去掉 `NoSuchProject, MountsActive, NotMounted, NoSuchSandbox, NoSuchProcess`，加上 `NoSuchSession, NoSuchRepo, NoSuchCheckpoint, NotConfirmed, NotTty, OpenFiles, CrossVolume, Drift, CheckFailed, CheckMutates, PushRefused, Unknown, Unsupported`。每个码一行：何时返回、谁返回。
  - 模板 `_template.md` 的九个二级标题，原文固定：`## 1. 职责与边界`、`## 2. 依赖`、`## 3. 公开接口（Rust）`、`## 4. 数据与不变量`、`## 5. 流程与算法`、`## 6. 失败与恢复`、`## 7. 测试清单`、`## 8. 实现任务（TDD）`、`## 9. 构建者决定记录`。
  - Crate 布局（上文"文件结构"）与依赖方向：`types ← apfs, gitio, store ← overlay, deps ← session ← publish, search ← daemon ← cli`；`confirm` 只被 `daemon` 用。

- [ ] **Step 1: 写 `docs/types.md`**：上面 Produces 里的每个名字给出 Rust 定义（`pub struct` / `pub enum`，带 `serde` derive 说明），错误码表（码 / 含义 / 由哪个模块返回），crate 布局与依赖方向图，JSON 输出约定（所有 CLI 输出 `{ "ok": bool, "data"?: …, "error"?: StrataError }`）。
- [ ] **Step 2: 写 `docs/modules/_template.md`**：九个标题及每节一句"写什么"；§7 的行格式固定为 `| 测试名 | 断言 | 来源（矩阵行 或 新增） |`；§8 用本计划的任务/步骤格式。
- [ ] **Step 3: 写 `docs/README.md`**：阅读顺序（design → types → config → test-matrix → modules）、每份文档一句话、本计划链接。
- [ ] **Step 4: 核对**

Run: `cd /Volumes/GWD/opensource/git-strata && rg -c '^## [1-9]\. ' docs/modules/_template.md && rg -o '`[A-Z][A-Za-z]+`' docs/types.md | sort -u | wc -l`
Expected: `9`，以及类型名条数 ≥ 20。

- [ ] **Step 5: Commit**

```bash
git add docs/types.md docs/modules/_template.md docs/README.md
git commit -m "docs: shared types, module template, reading order"
```

---

### Task 2: 配置文档

**Files:**
- Create: `docs/config.md`

**Interfaces:**
- Consumes: `types.md` 的 `RepoName`、`PublishTarget`。
- Produces: 配置文件路径与格式：全局 `~/Library/Application Support/git-strata/config.toml`，结构 `[paths] root`、`[repo.<name>]` 表：`path`（主 checkout）、`default_branch`、`deps_dirs`、`seed_caches`、`seed_files`、`install_inputs`、`install`（命令）、`checks`（命令列表，只读）、`publish = { branch, push, confirm }`、`deploy_on_push`、`pin_unpushed`；以及 Rust 结构 `Config`、`RepoConfig`（放进 `strata-types`）。

- [ ] **Step 1: 读来源**：`/Volumes/GWD/inter/CLAUDE.md`、各仓库 `CLAUDE.md`/`AGENTS.md`、`admin/dev-link.sh`、`front/dev-link.sh`、`tool/dev-link.mjs`、各 `package.json`/`composer.json`/`Cargo.toml` 的位置，确定每个仓库的依赖目录、种子缓存、种子文件、安装输入、检查命令。
- [ ] **Step 2: 写 `docs/config.md`**：格式说明 + 校验规则（`checks` 里出现 `--write`/`cs-fix` 且无 `--dry-run` → `CheckMutates`）+ 附录：inter 工作区 **全部兄弟仓库** 的默认配置（admin、front、tool、slate_viewer、surgical-timeout、appointment_patient_viewer、patient_9_grid、maxident-sync、insurance-check、release-notes、admin-time-off、admin-shared、hr-core、hr-tasks、hr-components、hr-time-management、quick-start、staff-api、rust-socket、rust-mail、gwd、sockdbg），`deploy_on_push` 对 staff-api、front 和所有 `@inter/*` 库为 true。
- [ ] **Step 3: 核对**

Run: `rg -c '^\[repo\.' docs/config.md`
Expected: `22`（上面列出的仓库数）。

- [ ] **Step 4: Commit**

```bash
git add docs/config.md
git commit -m "docs: config format and per-repo defaults"
```

---

### Task 3a–3f: 测试矩阵（六部分，可并行）

每部分一个子任务，步骤相同；表格行格式固定：

```text
| 来源 | 原意 | 目标模块 | 状态 | 理由 |
```

`来源` = `<crate 或目录>/<文件>::<测试函数>`；`状态` ∈ `保留`（语义照搬）、`改写`（语义保留、API 换成我们的）、`替代`（由真实目录 / 直接跑命令 / git 承担，写明替代物）、`不保留`（实现细节，写明哪一条）、`依赖承担`（由 `tgrep-core` 库本身的测试覆盖）。同一文件内语义相同的连续行，`原意`/`理由` 可写 `同上`，但**每个测试函数一行**。

**Files:** 各自 `docs/test-matrix/<part>.md`。

**Interfaces:**
- Consumes: `types.md` 的模块名（目标模块只能填 `apfs gitio store overlay deps session publish search confirm daemon cli types` 或 `—`）。
- Produces: 每部分文件末尾一行 `总计: N`，N 等于骨架生成的行数。

| 子任务 | 来源 | 期望条数 |
| --- | --- | --- |
| 3a `layerfs-store-layer.md` | `core/crates/layerfs-content`（487）、`layerfs-storage`（78）、`layerfs-persistence`（127）、`layerfs-history`（0） | 692 |
| 3b `layerfs-project-telemetry.md` | `layerfs-project`（24）、`layerfs-telemetry`（57） | 81 |
| 3c `layerfs-workspace-layer.md` | `layerfs-overlay`（75）、`layerfs-workspace`（176）、`layerfs-store-service`（4） | 255 |
| 3d `layerfs-replaced.md` | `layerfs-api`（117）、`layerfs-daemon`（166）、`layerfs-fuse`（8）、`layerfs-sandbox`（28）、`layerfs-bridge`（25） | 344 |
| 3e `layerfs-sdk-js.md` | `sdk/js/test/*.mjs`（6 个脚本，按脚本内每个 case 一行）+ `sdk/js/test/verify/*.mjs`（12 个，每文件一行） | 由骨架决定 |
| 3f `tgrep-core.md` | `tgrep-core/src/**`（525）：`worktrees/`、`walker.rs`、`gitignore.rs`、`git_index.rs`、`generations/`、`hybrid.rs`、`rooted/` 逐条；其余文件逐条但状态为 `依赖承担` | 525 |

- [ ] **Step 1: 生成骨架**（条数由此固定）

Run（以 3a 为例，`CRATES` 换成本部分的 crate）：
```bash
cd /Volumes/GWD/opensource/layerfs/core/crates && for c in layerfs-content layerfs-storage layerfs-persistence layerfs-history; do rg -n -A3 '#\[test\]' "$c" -g '*.rs' | rg -o "^([^:]+):[0-9]+:.*fn ([a-z_0-9]+)" -r '| '"$c"'/$1::$2 | | | | |' ; done | sed "s#$c/##" > /Volumes/GWD/opensource/git-strata/docs/test-matrix/layerfs-store-layer.md
```
Expected: `wc -l` = 期望条数（3a: 692）。条数不等时先修 rg 表达式，不手写行。

- [ ] **Step 2: 逐文件读测试，填 `原意`/`目标模块`/`状态`/`理由`**。读的是测试函数体和它的断言，不是实现。
- [ ] **Step 3: 加表头和 `总计: N`**，N 用 `rg -c '^\| layerfs-' <file>`（3f 用 `'^\| tgrep-core/'`）得到。
- [ ] **Step 4: 核对**

Run: `f=docs/test-matrix/<part>.md; n=$(rg -c '^\| (layerfs|sdk|tgrep)' $f); t=$(rg -o 'Total: [0-9]+|总计: [0-9]+' $f | rg -o '[0-9]+'); echo "$n $t"; rg -c '^\| [^|]+\|[^|]*\|[^|]*\| *\|' $f || true`
Expected: 两数相等且等于期望条数；最后一行输出空（没有状态为空的行）。

- [ ] **Step 5: Commit**

```bash
git add docs/test-matrix/<part>.md
git commit -m "docs(test-matrix): <part>"
```

---

### Task 4: `apfs.md`

**Files:** Create `docs/modules/apfs.md`

**Interfaces:**
- Consumes: `types.md`（`RepoPath`、`Code::CrossVolume`）。
- Produces（名字不可改）: `fn clone_tree(src: &Path, dst: &Path) -> Result<(), StrataError>`（同卷检查 → `CrossVolume`；目录递归 clonefile）；`fn clone_file(src, dst)`；`fn same_volume(a: &Path, b: &Path) -> io::Result<bool>`；`struct Attrs { size, gen_count: u64, mtime, ctime, inode, kind }` 与 `fn walk(root: &Path, filter: &dyn Fn(&Path)->bool) -> impl Iterator<Item=(RepoPath, Attrs)>`（`getattrlistbulk`）；`struct Watcher` + `fn watch(root, on_event: impl Fn(WatchEvent))` 与 `enum WatchEvent { Changed(Vec<PathBuf>), MustRescan(PathBuf), Dropped }`；`fn case_fold_eq(a: &str, b: &str) -> bool`。

- [ ] **Step 1: 读** design.md §3、§6.2、§8；tgrep `walker.rs` 头注释（大小写、64 MiB 上限）；`man 2 clonefile`、`man 2 getattrlistbulk`（`ATTR_CMN_GEN_COUNT`）。
- [ ] **Step 2: 按模板写九节**；§7 必含 Review Focus 1（`clone_dir_refuses_cross_volume`）以及 `gen_count_changes_on_write_not_on_touch`、`walk_excludes_configured_dirs`、`watcher_reports_must_rescan`。
- [ ] **Step 3: 核对**

Run: `f=docs/modules/apfs.md; diff <(rg -o '^## [1-9]\. .*' docs/modules/_template.md) <(rg -o '^## [1-9]\. .*' $f) && rg -c 'clone_dir_refuses_cross_volume' $f`
Expected: diff 无输出；`1`。

- [ ] **Step 4: Commit** — `git add docs/modules/apfs.md && git commit -m "docs(modules): apfs"`

---

### Task 5: `gitio.md`

**Files:** Create `docs/modules/gitio.md`

**Interfaces:**
- Consumes: `types.md`（`TreeOid`、`CommitOid`、`RepoPath`）。
- Produces: `struct Git { common_dir: PathBuf }` + `fn command(&self) -> Command`（剥 `GIT_*`，`--no-replace-objects`，`GIT_TERMINAL_PROMPT=0`，`GIT_NO_LAZY_FETCH=1`，`GIT_OPTIONAL_LOCKS=0`）；`fn fetch(&self, remote, branch) -> Result<CommitOid>`；`fn tree_of(&self, commit) -> TreeOid`；`fn export_tree(&self, tree, dst: &Path)`；`fn read_index(&self, worktree) -> IndexInfo { tracked: BTreeSet<RepoPath>, ignorecase: bool }`（直接解析 `.git/index` 与 `config`，借 tgrep `git_index.rs`）；`fn is_ignored(&self, worktree, path) -> bool`；`fn commit_tree(&self, tree, parent, message) -> CommitOid`；`fn update_ref(&self, name, new, old: Option<CommitOid>) -> Result<()>`（compare-and-swap）；`fn push(&self, remote, src_ref, dst_branch, timeout) -> Result<PushOutcome>`，`enum PushOutcome { Pushed, Refused(String), Unknown }`；`fn merge3(base: &[u8], ours: &[u8], theirs: &[u8]) -> Merge3 { Clean(Vec<u8>) | Conflict(Vec<Hunk>) }`。

- [ ] **Step 1: 读** design.md §6.5、§6.7、§6.8；tgrep `generations/git.rs`、`git_index.rs`、`vendor/ignore/PATCHES.md`；矩阵 3f 中目标为 `gitio` 的行。
- [ ] **Step 2: 写九节**；§7 必含 `index_parse_tracked_never_ignored`、`update_ref_cas_refuses_moved_ref`、`push_unknown_on_timeout`、`merge3_nonoverlapping_hunks_clean`、`merge3_same_hunk_conflict`。
- [ ] **Step 3: 核对**（同 Task 4 的命令，文件换 `gitio.md`，关键字 `push_unknown_on_timeout`）
- [ ] **Step 4: Commit** — `docs(modules): gitio`

---

### Task 6: `store.md`

**Files:** Create `docs/modules/store.md`

**Interfaces:**
- Consumes: `types.md` 全部 id 类型；`apfs::clone_file`。
- Produces: `struct Store { root: PathBuf, db: Connection }`、`fn open(root) -> Result<Store>`（同卷检查）；表：`base_gens`、`deps_sets`、`sessions`、`workspaces`、`checkpoints`、`overlay_entries`、`blobs`、`publishes`、`publish_items`、`refs_audit`；`fn put_blob(&self, src: &Path) -> BlobId`（clonefile 进 `blobs/<2>/<id>`）、`fn get_blob(&self, id, dst: &Path)`；`fn record_checkpoint(&self, Checkpoint) -> CheckpointId`；`fn gc(&self, keep: Duration) -> GcReport`（引用计数）；SQLite：WAL，`synchronous=FULL`，单写者。

- [ ] **Step 1: 读** design.md §7、§9；矩阵 3a 中状态为 `保留`/`改写` 的行；LayerFS `docs/general/concepts.md`（append-only、内容寻址）。
- [ ] **Step 2: 写九节**，§4 给出完整 DDL；§7 必含 `blob_put_is_idempotent`、`checkpoint_write_is_durable_after_crash`（`synchronous=FULL` 的验证方法）、`gc_keeps_referenced_generations`。
- [ ] **Step 3: 核对**（关键字 `checkpoint_write_is_durable_after_crash`）
- [ ] **Step 4: Commit** — `docs(modules): store`

---

### Task 7: `overlay.md`

**Files:** Create `docs/modules/overlay.md`

**Interfaces:**
- Consumes: `apfs::{walk, Watcher, case_fold_eq}`、`gitio::{read_index, is_ignored}`、`store::{put_blob, get_blob, record_checkpoint}`。
- Produces: `struct BaseManifest`（基线每个文件的 `Attrs` + `BlobId`）；`fn compute(ws: &Path, base: &BaseManifest, dirty: Option<&[RepoPath]>) -> Overlay`；`fn checkpoint(ws, base, reason) -> Checkpoint`；`fn restore(ws, target: &Checkpoint, paths: Option<&[RepoPath]>)`（先 checkpoint 再投影）；`fn reconcile_full(ws, base) -> Overlay`；`fn rebase(overlay: &Overlay, old_base, new_base) -> Rebased { overlay, conflicts: Vec<RepoPath> }`（整文件三方投影，用 `gitio::merge3`）；`.lww` 匹配：`fn lww_matches(patterns, path) -> bool`。

- [ ] **Step 1: 读** design.md §4、§6.2、§6.5 第 7 步；tgrep `SHARED_WORKTREE_INDEXES.md` §"Base snapshots and worktree overlays"；矩阵 3c 中目标为 `overlay` 的行（Commit 的 `Conflict/HeadMoved/Dirty/UpToDate`、`.lww`）。
- [ ] **Step 2: 写九节**；§7 必含 Review Focus 2 和 4（`reconcile_after_kernel_dropped_equals_cold_scan`、`rename_case_only_recorded_as_rename`）以及 `restored_to_base_removes_override`、`deleted_base_file_is_tombstone`、`restore_is_itself_checkpointed`、`lww_newest_wins_no_conflict`。
- [ ] **Step 3: 核对**（关键字 `rename_case_only_recorded_as_rename`）
- [ ] **Step 4: Commit** — `docs(modules): overlay`

---

### Task 8: `deps.md`

**Files:** Create `docs/modules/deps.md`

**Interfaces:**
- Consumes: `config.md`（`deps_dirs`、`seed_caches`、`install_inputs`、`install`）、`apfs::clone_tree`、`store`。
- Produces: `fn inputs_hash(repo_cfg, tree_or_ws: &Path) -> DepsSetId`；`fn adopt(repo_cfg, from: &Path) -> Result<DepsSet>`（认领主 checkout 现成目录）；`fn build(repo_cfg, staging: &Path) -> Result<DepsSet>`（冷装）；`fn drift(ws: &Path, pinned: &DepsSet) -> Option<Drift>`；`struct Proposal { id, repo, from: DepsSetId, to: DepsSetId, origin: SessionId|Main }`；`fn propose(...) -> Proposal`；`fn rollout(session, proposal, confirm: ConfirmToken) -> RolloutReport`（与 `publish::rebase_session` 合为一个原子动作：先 checkpoint → 临时目录准备新树+新依赖 → `rename` 交换；有打开文件 → 跳过）；`fn open_files(dir: &Path) -> Vec<(pid, PathBuf)>`。

- [ ] **Step 1: 读** design.md §3（dev-link 写依赖目录、锁文件不在 git 里）、§4、§6.1、§6.4；`tool/dev-link.mjs`、`front/dev-link.sh`。
- [ ] **Step 2: 写九节**；§7 必含 Review Focus 3（`rollout_skips_session_with_open_files`）以及 `adopt_matches_inputs_hash_else_refuses`、`seed_cache_is_cloned_not_sealed`、`rollout_moves_base_and_deps_atomically`、`proposing_session_may_use_new_deps_before_approval`。
- [ ] **Step 3: 核对**（关键字 `rollout_skips_session_with_open_files`）
- [ ] **Step 4: Commit** — `docs(modules): deps`

---

### Task 9: `session.md`

**Files:** Create `docs/modules/session.md`

**Interfaces:**
- Consumes: `config.md`、`gitio::{fetch, tree_of, export_tree}`、`apfs::clone_tree`、`deps::{adopt, build}`、`store`。
- Produces: `fn new(ticket: Option<&str>, repos: &[RepoName]) -> SessionManifest`（整套兄弟布局；基线来自发布链顶或 `origin/<default_branch>`；种子缓存与种子文件；工作区 `.git` detached、无 upstream、装 `pre-push` 钩子）；`fn resolve(cwd: &Path) -> Option<(SessionId, RepoName)>`；`fn status(id) -> SessionStatus`；`fn remove(id, discard: bool, confirm: Option<ConfirmToken>)`；`fn seed_files(repo_cfg, from: &Path, to: &Path) -> Vec<SeedRecord>`。

- [ ] **Step 1: 读** design.md §3（兄弟布局、种子文件）、§4、§6.1、§6.6、§6.8；矩阵 3b 中目标为 `session` 的行（Init 导入规则：含 `.git`、ignored、symlink；`Exists` 拒绝）。
- [ ] **Step 2: 写九节**；§7 必含 `new_clones_entire_sibling_layout`、`new_refuses_cross_volume_root`、`seed_files_listed_in_status_and_never_checkpointed`、`workspace_git_is_detached_and_prepush_refuses`、`remove_refuses_unpublished_without_discard`、`resolve_by_cwd_inside_workspace`。
- [ ] **Step 3: 核对**（关键字 `new_clones_entire_sibling_layout`）
- [ ] **Step 4: Commit** — `docs(modules): session`

---

### Task 10: `publish.md`

**Files:** Create `docs/modules/publish.md`

**Interfaces:**
- Consumes: `overlay::{compute, rebase}`、`gitio::{merge3, commit_tree, update_ref, push, fetch}`、`config.md`（`checks`、`publish`、`deploy_on_push`、`pin_unpushed`）、`confirm::ConfirmToken`、`deps`。
- Produces: `fn chain_ref(branch) -> String`（`refs/strata/publish/<branch>`）；`fn merge_base(repo) -> CommitOid`（链顶若领先 origin，否则 origin）；`fn stage(group: &ReleaseGroup) -> Result<Staged, Conflicts>`；`fn check(repo_cfg, staged: &Path) -> Result<(), CheckFailed|CheckMutates>`；`fn commit(staged, target) -> CommitOid`；`fn push(repo, target, confirm: Option<ConfirmToken>) -> PushOutcome`；`fn assign(group, path, session)`；`fn rebase_session(session, repo, new_base: BaseGen) -> RebaseReport`（与 `deps::rollout` 共用原子动作）；`fn diff_between(a: SessionId, b: SessionId, repo) -> BetweenDiff { only_a, only_b, both: Vec<(RepoPath, Merge3Preview)> }`。

- [ ] **Step 1: 读** design.md §6.3、§6.5、§6.7、§9；矩阵 3c 中目标为 `publish` 的行（merge/publish 语义）；tgrep `SHARED_WORKTREE_INDEXES.md` §"When the base changes"。
- [ ] **Step 2: 写九节**；§7 必含 Review Focus 5（`push_timeout_keeps_chain_reports_unknown`）以及 `stage_only_selected_paths`、`two_sessions_nonoverlapping_hunks_merge_clean`、`same_hunk_conflict_stops_whole_group`、`check_mutating_command_is_config_error`、`merge_base_prefers_local_chain_when_ahead`、`deploy_on_push_without_confirm_stops_before_push`、`post_publish_rebase_drops_published_paths`。
- [ ] **Step 3: 核对**（关键字 `push_timeout_keeps_chain_reports_unknown`）
- [ ] **Step 4: Commit** — `docs(modules): publish`

---

### Task 11: `search.md`

**Files:** Create `docs/modules/search.md`

**Interfaces:**
- Consumes: `tgrep-core` 的 `HybridIndex`/worktree view API（文档里引用它的 `SHARED_WORKTREE_INDEXES.md` §"Implemented worktree synchronization API"）、`overlay::Overlay`、`session::resolve`。
- Produces: `fn attach(session, repo) -> ViewId`、`fn detach(view)`、`fn refresh(view, changed: &[RepoPath])`、`fn grep(view | all, pattern, opts) -> Vec<Hit { session, repo, path, line, text }>`、`fn files(view, glob) -> Vec<RepoPath>`；基线索引按 `BaseGen.tree` 复用。

- [ ] **Step 1: 读** design.md §6.3；tgrep `SHARED_WORKTREE_INDEXES.md`（"Overall architecture"、"Search flow"、"Implemented worktree synchronization API"）、`worktrees.rs` 头注释；矩阵 3f 中目标为 `search` 的行。
- [ ] **Step 2: 写九节**；§7 必含 `overlay_entry_shadows_base_path`、`tombstone_hides_base_in_this_session_only`、`grep_all_tags_each_hit_with_session`、`base_index_shared_across_sessions_same_tree`。
- [ ] **Step 3: 核对**（关键字 `overlay_entry_shadows_base_path`）
- [ ] **Step 4: Commit** — `docs(modules): search`

---

### Task 12: `confirm.md`

**Files:** Create `docs/modules/confirm.md`

**Interfaces:**
- Consumes: `types.md`（`Code::NotConfirmed`、`Code::NotTty`）。
- Produces: `struct ConfirmToken { action: Action, issued: Instant, nonce }`（一次一用，daemon 内存，30 s 过期）；`enum Action { PushDeploy{repo,branch}, DepsApprove{proposal}, RemoveDiscard{session} }`；`fn require(action) -> Result<ConfirmToken, StrataError>`（LocalAuthentication 弹框，`LAPolicyDeviceOwnerAuthentication`；失败/取消 → `NotConfirmed`）；CLI 侧 `fn require_tty() -> Result<(), NotTty>`。

- [ ] **Step 1: 读** design.md §6.9、§9 不变量 7；Apple LocalAuthentication 文档（`LAContext.evaluatePolicy`），`objc2-local-authentication` crate 的 API 面。
- [ ] **Step 2: 写九节**；§7 必含 `token_single_use`、`token_expires`、`non_tty_refused_before_prompt`、`no_env_or_flag_bypass`（测试枚举 `STRATA_*` 环境变量与所有 CLI flag，断言都不能跳过）。
- [ ] **Step 3: 核对**（关键字 `no_env_or_flag_bypass`）
- [ ] **Step 4: Commit** — `docs(modules): confirm`

---

### Task 13: `daemon.md`

**Files:** Create `docs/modules/daemon.md`

**Interfaces:**
- Consumes: 全部上游模块。
- Produces: launchd plist（`com.gwd.git-stratad`，GUI 会话，`KeepAlive`）；unix socket `~/Library/Application Support/git-strata/daemon.sock`；RPC = JSON lines，`{ "id", "method", "params" }` / `{ "id", "ok", "data" | "error" }`；方法表（每个 CLI 子命令一个方法，名字与 `cli.md` 一致）；锁：按 repo 一把写锁，按会话 checkpoint 串行；调度：监听、每小时全量对账、gc；崩溃恢复：启动时扫描 `publishes` 表中 `Unknown` 状态并报出，不重放。

- [ ] **Step 1: 读** design.md §5、§6.2、§8、§9；tgrep `SHARED_WORKTREE_INDEXES.md` §"Daemon wire contract v1"（只借形状）。
- [ ] **Step 2: 写九节**；§7 必含 `concurrent_checkpoints_same_session_serialize`、`unknown_publish_reported_on_restart_not_replayed`、`cli_readonly_commands_work_without_daemon`。
- [ ] **Step 3: 核对**（关键字 `unknown_publish_reported_on_restart_not_replayed`）
- [ ] **Step 4: Commit** — `docs(modules): daemon`

---

### Task 14: `cli.md`

**Files:** Create `docs/modules/cli.md`

**Interfaces:**
- Consumes: `daemon.md` 的方法表、`types.md` 的 JSON 约定。
- Produces: 子命令全集及参数：`new`, `status`, `checkpoint`, `restore`, `grep`, `files`, `diff`, `deps propose|approve|list`, `publish`, `push`, `assign`, `rm`, `gc`, `daemon start|stop|status`；每个子命令：参数、JSON 输出形状、错误码、是否需要 TTY/确认；Claude Code 集成：`PostToolUse` 钩子命令行（`git-strata checkpoint --tool-call "$TOOL_USE_ID"`）、`SessionStart` 提示；`git strata` 子命令安装方式。

- [ ] **Step 1: 读** design.md §6 全部；`gwd/CLAUDE.md`（输出风格）；矩阵 3e 的行（错误码、`fs.*` 查询接口中保留的 `read/list/stat/grep/glob/diff_files`）。
- [ ] **Step 2: 写九节**；§7 必含 `every_command_emits_ok_envelope`、`error_codes_match_types_md`、`hook_checkpoint_is_idempotent_per_tool_call`、`confirm_commands_refuse_non_tty`。
- [ ] **Step 3: 核对**

Run: `rg -o '`git-strata [a-z]+' docs/modules/cli.md | sort -u | wc -l`
Expected: `≥ 14`。

- [ ] **Step 4: Commit** — `docs(modules): cli`

---

### Task 15: 矩阵索引与全局一致性

**Files:**
- Create: `docs/test-matrix.md`
- Modify: `docs/README.md`

- [ ] **Step 1: 写 `docs/test-matrix.md`**：六部分链接、每部分条数、按状态汇总表（保留/改写/替代/不保留/依赖承担 各多少）、按目标模块汇总表、"新增"测试（各模块 §7 中来源为"新增"的行）总数。
- [ ] **Step 2: 标识符一致性**

Run: `cd docs && comm -13 <(rg -o '`[A-Z][A-Za-z]+(Id|Gen|Set|Entry|Kind|Reason|Error|Target|Group|Manifest|State|Token|Outcome)`' types.md | sort -u) <(rg -o '`[A-Z][A-Za-z]+(Id|Gen|Set|Entry|Kind|Reason|Error|Target|Group|Manifest|State|Token|Outcome)`' modules/*.md config.md | sed 's/^[^:]*://' | sort -u)`
Expected: 无输出（模块文档没有用 types.md 未定义的类型名）。有输出则补进 `types.md` 或改模块文档，二选一并写明。

- [ ] **Step 3: 覆盖一致性**：design.md §6 的每个 `strata …` 命令在 `cli.md` 有对应子命令；Review Focus 五条各在指定模块文档 §7 出现。

Run: `for k in clone_dir_refuses_cross_volume reconcile_after_kernel_dropped_equals_cold_scan rollout_skips_session_with_open_files rename_case_only_recorded_as_rename push_timeout_keeps_chain_reports_unknown; do printf '%s ' "$k"; rg -l "$k" docs/modules | wc -l; done`
Expected: 每行 `1`。

- [ ] **Step 4: 条数一致性**

Run: `for f in docs/test-matrix/*.md; do printf '%s ' "$f"; rg -c '^\| (layerfs|sdk|tgrep)' "$f"; done`
Expected: 692 / 81 / 255 / 344 / （3e 骨架数） / 525。

- [ ] **Step 5: 更新 `docs/README.md`** 的索引与"下一阶段：按模块文档 §8 实施"说明。
- [ ] **Step 6: Commit**

```bash
git add docs/test-matrix.md docs/README.md
git commit -m "docs: test matrix index and cross-document consistency"
```

---

## 自检记录（写计划时完成）

- **Spec 覆盖**：design.md §1–§9 的每条要求落在：§3 约束 → Task 2/4/8/9；§4 词汇 → Task 1；§5 模块 → Task 4–14（11 份，含 `confirm`）；§6.1 → Task 9；§6.2 → Task 7；§6.3 → Task 10/11；§6.4 → Task 8；§6.5/6.7 → Task 10；§6.6 → Task 9；§6.8 → Task 5/9；§6.9 → Task 12；§7 → Task 6；§8 → Task 4/7/13；§9 → 各模块 §4/§6；§10 → Task 3a–3f/15。无遗漏。
- **类型一致**：所有跨模块名字只在 Task 1 定义，其余任务的 Interfaces 只引用；Task 15 Step 2 机械核对。
- **比例**：本计划约 330 行，交付物为 ~16 份文档，合理。
