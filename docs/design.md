# git-strata — 设计文档

> **状态**：brainstorm 产出，2026-10-22；**用户已确认**（唯一改动：项目名由 strata 改为 git-strata）。
> 本文是唯一的约束来源；`docs/modules/*.md` 的逐模块开发文档从本文派生，与本文冲突时以本文为准。写代码之前，所有模块文档和测试矩阵必须先完成。
>
> 命名：项目与仓库叫 **git-strata**，二进制也叫 `git-strata`，因此可以写作 `git strata new …`；文中的 `strata …` 都指它（可装一个 `strata` 别名）。refs、目录、环境变量沿用短名 `strata`（`refs/strata/*`、`.strata/`、`STRATA_PUBLISH`）。
>
> 标注约定：**[用户]** = 用户在对话中明确决定的；**[我的决定]** = 我替用户做的、随时可推翻的选择；**[交给构建者]** = 构建者自行决定。

---

## 1. 这是什么

**git-strata**（下文简称 strata）让一台 Mac 上的多个 Claude Code 会话各自在**真实目录**里修改**同一组仓库**，共享不可变的基线和依赖，记录每一步，互相可见，最后按选定的批次合成 git 提交。

一句话里的每个词都有来历：

| 词 | 含义 | 来源 |
| --- | --- | --- |
| 真实目录 | 工作区是普通的 APFS 目录，Claude Code 的 Read/Edit/Bash、dev server、cargo、bun 全速跑，无虚拟文件系统、无容器、无 kext | [用户] 接受"工作区就是真实 APFS 目录，不做虚拟文件系统" |
| 同一组仓库 | 一个会话 = 一个 ticket = 同时打开多个仓库的工作区 | [用户] |
| 共享基线和依赖 | `node_modules`/`target` 这类依赖不每个工作区装一份，所有工作区看主干那一份；依赖变更需人工确认才同步给所有会话 | [用户] |
| 记录每一步 | 每个工具调用之后记一个 checkpoint，可恢复 | [用户] "历史仅为合并与恢复" |
| 互相可见 | 会话之间能 grep、能 diff，看到彼此改了什么 | [用户] |
| 按批次合成 git 提交 | 一个会话的改动可以分批发布；多个会话的批次可以合成一次发布 | [用户] |

**git 是唯一的真相源。** strata 自己的历史只用于合并和恢复，发布的结果永远是真实仓库里的 git commit，能 push，能触发现有的 webhook 和 `build.sh`。[用户]

### 1.1 为什么现在

过去几周 git worktree 方案的真实痛点（均有记录）：

- 每个 worktree 要自己的 `node_modules` 和 `src-tauri/target`；靠 APFS `cp -Rc` 克隆 200 GB 的 target 才把 Hub 构建从 30–40 分钟压到约 2 分钟。
- rebase 之后 `bun.lock` 的 pin 一动，同伴新文件的 `tsc` 就红——依赖和源码的版本错位。
- 几个会话同时碰一个 ticket，互相不知道对方改了什么，直到 push 时撞上。

### 1.2 参考了什么、没有照搬什么

- **LayerFS**（Ephemeral-AI-Lab/layerfs）给了模型：Branch / Commit / Layer、即时 fork、去重 Store、`Conflict` 与 `.lww` 规则、`fs.*` 查询接口。它的 Workspace 是容器内的 FUSE 挂载，在 macOS 上只能靠 Docker 提供 Linux 内核（FUSE、同一内核内的 SQLite 锁、`docker exec`）。我们不要这一半：**Sandbox / daemon / FUSE 由真实目录替代**。[用户] 同意功能基准 = LayerFS 语义层 + tgrep 索引与同步层。
- **tgrep**（microsoft/tgrep）给了共享 worktree 的数据模型和代码：基线 = 精确 git tree OID、每个 worktree 钉住一个基线、overlay 用墓碑表示删除、"改回和基线一样即撤销覆盖"、"永远不原地更新共享基线"、通知只是优化而对账是真相。它索引的是三元组；内容存储、`clonefile`、依赖共享、发布到 git 仍是我们的活。

---

## 2. 目标、非目标、反目标

### 目标（v1 必须做到）

1. 新建一个会话：秒级得到整套兄弟布局的工作区目录，依赖已就位，可立刻 `bun run dev`。
2. 每个工具调用后自动 checkpoint；任何时刻可把工作区（整体或指定路径）恢复到任一 checkpoint。
3. 依赖跟主干：主干锁文件变了，strata 建新的依赖集 generation 并**提出**同步；用户确认后才换到各会话；提出变更的会话可先用自己的新依赖。
4. 跨会话可见：`grep`/`files` 默认本会话、可指定他人或全部；`diff` 能比本会话 vs 基线、他人 vs 基线、两会话之间（含重叠文件与三方合并预览）。
5. 分批发布：从一个或多个会话各挑一批路径，合成一棵 staging 树，hunk 不重叠自动三方合并、真冲突停下来由人指定谁解 [用户]，跑配置的检查，按仓库落 commit 到配置的目标分支并按配置推送。
6. 发布之后，参与的会话自动接到新基线上，剩余改动重算 overlay，冲突标出。
7. 发布目标是配置：每仓库一个目标分支 + 是否推送；默认 = 直接合主干并推 [用户]；一推就上线的仓库（staff-api `master`、front `main`）需显式确认 [我的决定]。

### 非目标（v1 不做）

- 虚拟文件系统（FSKit / macFUSE / NFS 环回）。可作为日后只读展示层。
- 容器、Linux、Windows。
- 替代 git 的任何部分：分支管理、远端协作、历史查看仍用 git。
- 图形界面。v1 只有 CLI（JSON 输出）；给人看的页面以后再说。[我的决定]
- hunk 级别的批次挑选。批次按路径挑，整文件进出。[我的决定，与 tgrep "按整文件不按 hunk" 一致]
- 跨机器共享 Store。

### 反目标（即使"能用"也算失败）

- 丢失或损坏任何会话的未发布改动。
- 发布了用户没有选的东西，或推送到用户没有配置的地方。
- 在一个正在跑 dev server 的会话脚下换掉依赖。
- 让构建或 dev server 比今天慢。
- 需要 kext、降低系统安全策略、或安装 VM。
- 碰用户的主 checkout（`/Volumes/GWD/inter/<repo>`）或替用户跑 `build.sh`。

---

## 3. 约束（环境事实，已核实）

| 约束 | 事实 | 对设计的影响 |
| --- | --- | --- |
| 平台 | macOS 26+（本机 Darwin 27.0.1），Apple Silicon，APFS | 可用 `clonefile(2)`、`getattrlistbulk(2)`、`ATTR_CMN_GEN_COUNT`、FSEvents、系统 libsqlite3 |
| 兄弟布局 | `admin/front/tool` 的 dev-link 用 `../quick-start`、`../release-notes`、`../node_modules` 等相对路径同步兄弟目录的 `@inter/*` 库 | 一个会话必须克隆**整套兄弟布局**，不是只克隆要改的仓库 |
| symlink 陷阱 | Turbopack 拒绝 symlink 的 `node_modules` | 依赖共享用 `clonefile`（CoW 克隆），**不用 symlink** |
| 一推就上线 | staff-api 推 `master` = 生产部署；front 推 `main` = 自动部署；任何分支推 staff-api 都会触发重建 | 发布到这些仓库需显式确认；推送目标可配置 |
| 主 checkout 常脏 | 用户和同伴在 `/Volumes/GWD/inter/<repo>` 里工作，有时是脏的 | 基线来自 `origin/<默认分支>` 的 tree OID，**不是**主 checkout 的工作树 |
| 多会话并发 | 同一台机器上同时有多个 Claude Code 会话和人类 | 所有共享状态由一个 daemon 持有，CLI 只是客户端 |
| 大小写 | APFS 默认大小写不敏感，仓库 `core.ignorecase` 多为 true | walker 按 git 的规则处理（已跟踪文件永不被 ignore）；路径比较按仓库的 ignorecase |
| dev-link 写依赖目录 | admin/front/tool 的 dev-link 每次 dev/build 都把兄弟库镜像进 `node_modules/@inter/*`，并在布局根建 `../node_modules/tailwindcss` 软链；front 的 `build` 自己跑 `bun install`；`src-tauri/target` 每次 cargo build 都被改写 | 工作区里的依赖目录必须**可写**（CoW 克隆归会话所有）；"不可变"只对 Store 里封存的 generation 成立；漂移检测比**安装输入的哈希**，不比目录内容 |
| 锁文件不一定在 git 里 | front 的 `bun.lock` 被 gitignore；`@inter/*` 多为不带 ref 的 git URL | 依赖集的键是每仓库配置的"安装输入"（package.json、锁文件若被跟踪、解析出的 `@inter/*` SHA），不是单一锁文件 |
| 库一推就部署 | `@inter/*` 库推送会自动部署 front | hr-*/admin-shared/quick-start/admin-time-off 也算"一推上线"，需确认 |
| 被 ignore 但必需的文件 | tool 的 `CLAUDE.md`（AGENTS.md 才被跟踪）、front 的 `.env.local`、staff-api 的 `.env`（生产库）、`.claude/settings.local.json` | 基线从 git 对象导出不含它们；见 §4 种子文件、§6.1 |

---

## 4. 词汇表

| 术语 | 定义 |
| --- | --- |
| **Repo** | 一个 git 仓库，以 git common dir 识别（tgrep 的做法）。strata 的每个 repo 有一份配置：默认分支、依赖目录、锁文件、安装命令、检查命令、发布目标、是否一推上线。 |
| **Base generation（基线）** | 一个 repo 在某个 git tree OID 上的不可变快照：源码树 + 与之匹配的依赖集。永远不原地修改；新的基线是新的 generation。键 = (repo, tree OID, 依赖集 id)。 |
| **Deps set（依赖集）** | 一个 repo 的依赖目录（默认 `node_modules`、`vendor`）按某组"安装输入"安装完成后在 Store 里封存的快照。键 = (repo, 安装输入哈希, 平台/架构, 工具链版本)。工作区里的那份是它的 CoW 克隆，归会话所有、可写；漂移 = 工作区的安装输入哈希与所钉依赖集不符。[工具链版本是否入键：交给构建者] |
| **Seed cache（种子缓存）** | 构建缓存（默认 `src-tauri/target`、`.next/cache`、`node_modules/.cache`）：新建会话时从温热的主 checkout `clonefile` 过来，归会话所有，随便写，**不同步、不 checkpoint、不封存**。它们不是"装"出来的，只是让第一次构建不从零开始。[我的决定] |
| **Seed files（种子文件）** | 被 gitignore 但运行必需的文件（每仓库配置，默认含 `CLAUDE.md`、`.env`、`.env.local`、`.claude/settings.local.json`）：新建会话时从主 checkout 复制，不 checkpoint，`status` 列出来源与时间。[我的决定] |
| **Session（会话）** | 一个工作单元，有自己生成的 id；ticket 只是标签，一个 ticket 可以有多个会话。清单记录它包含哪些 repo、各自钉住的基线、各自的依赖集、工作区路径、创建时间、状态。 |
| **Workspace（工作区）** | 会话中某个 repo 的真实目录。会话根目录下按兄弟布局排列：`<root>/sessions/<session-id>/<repo>/`——路径键是 `new` 生成的会话 id（types.md `SessionId`），ticket 只是会话的标签，不进路径（§11 第 18 条）。 |
| **Overlay** | 工作区相对其钉住基线的差异集合，按整文件：增/改 = 新内容，删 = 墓碑，改名 = 墓碑 + 新条目，未跟踪且合格 = `Untracked` 条目（可发布、可搜索，与新增文件同等，只记录它不在基线的 git 索引里），改回与基线相同 = 撤销覆盖。 |
| **Checkpoint** | 某一时刻 overlay 的持久化快照，带时间、触发原因、工具调用 id。恢复的单位。 |
| **Batch（批次）** | 发布时从一个会话挑出的路径集合。 |
| **Release group（发布组）** | 若干批次（可来自多个会话）的集合，一次发布的输入。 |
| **Staging tree** | 发布组合成的那棵树：基线 + 各批次三方合并的结果；检查在它上面跑；commit 从它产生。 |
| **Conflict** | 三方合并中同一 hunk 两边都改。停下来，由人指定哪个会话解决；解决 = 那个会话改好并重新 checkpoint。 |
| **`.lww`** | 从 LayerFS 借来的规则：被 `.strata/lww` 中模式命中的文件永不冲突，最新 checkpoint 的版本胜出（典型：生成文件、锁文件）。[我的决定：保留，默认为空] |
| **Publish target（发布目标）** | 每 repo 的 (目标分支, 是否推送, 是否需要确认)。 |

---

## 5. 架构

```text
Claude Code 会话 A ──┐                      ┌── /Volumes/GWD/inter/<repo>（人类的主 checkout，只读取 .git）
Claude Code 会话 B ──┤  strata CLI (JSON)   │
人类（终端）     ──┘        │               │
                            ▼ unix socket   ▼
                     ┌─────────────────────────────┐
                     │ stratad（一台机器一个，launchd）│
                     │  session / overlay / deps     │
                     │  publish / search / watch     │
                     └──────┬──────────┬────────────┘
                            │          │
              ┌─────────────▼──┐   ┌───▼──────────────────────┐
              │ Store           │   │ 工作区目录                 │
              │ SQLite 元数据    │   │ <root>/sessions/<sid>/     │
              │ 内容寻址 blob    │   │   tool/ hr-core/ staff-api/│
              │ 基线与依赖集缓存 │   │ （APFS clonefile 克隆）     │
              └────────────────┘   └───────────────────────────┘
```

**一台机器一个 daemon** [我的决定]：会话跨多个 repo，按 repo 起 daemon（tgrep 的做法）会让"发布组"跨进程协调；一个 daemon、按 repo 分命名空间更简单。CLI 永远是薄客户端；daemon 不在时 CLI 可以只读（看状态、看 checkpoint），但不做写操作。

**模块**（每个对应 `docs/modules/<name>.md` 和一个 Rust crate 或模块）

| 模块 | 职责 | 借用 |
| --- | --- | --- |
| `store` | SQLite 元数据（generations、deps sets、sessions、checkpoints、overlays、publish 记录）+ 内容寻址 blob 存储 | LayerFS 的"一个 Store、append-only、按内容去重"原则；schema 自写 |
| `apfs` | `clonefile` 克隆目录/文件；`getattrlistbulk` 遍历；gen count 判变；FSEvents 监听；大小写规则 | — |
| `gitio` | 干净的 git 子进程封装；直接读 `.git/index`；tree OID；commit/push；三方合并（diff3） | tgrep `generations/git.rs`、`git_index.rs`；diff3 用现成 crate [交给构建者选] |
| `overlay` | 从基线清单 + 当前目录算 overlay；checkpoint 写入；restore | tgrep overlay 规则（墓碑、改名、撤销覆盖） |
| `deps` | 依赖集 generation：在 staging 克隆里跑安装命令，封存，提出同步，按会话 rollout | tgrep "永远不原地更新基线；旧 generation 保留到没人用" |
| `session` | 清单、生命周期、兄弟布局、cwd → 会话识别 | — |
| `publish` | 批次、发布组、staging 树、三方合并、检查、commit/push、发布后 rebase | LayerFS 合并/冲突/publish 语义 |
| `search` | 共享三元组索引 + 每会话 overlay；`grep`/`files` | tgrep-core 作为库依赖 [我的决定；不可行则 vendor 所需模块] |
| `confirm` | 人类确认通道：LocalAuthentication（Touch ID / 密码）+ TTY 要求（§6.9） | — |
| `daemon` | launchd agent、unix socket RPC、监听与对账调度、锁 | — |
| `cli` | 子命令、JSON 输出、Claude Code hook 入口 | `gwd` 的输出风格 |

---

## 6. 关键流程

### 6.1 新建会话 `strata new <ticket> --repos tool,staff-api[,…]`

1. 对清单里**每个兄弟仓库**（不只是 `--repos` 指定的）：`git fetch` 主 checkout 的 `origin`，取发布链或默认分支的 tree OID（§6.7）；若该 OID 的基线不存在，建一个：从 git 对象导出源码树到基线缓存 `bases/<repo>/<oid>/`，再把匹配的依赖集克隆进去。依赖集不存在时先**认领**：主 checkout 现成的 `node_modules`/`vendor` 若其安装输入哈希与基线一致，直接 `clonefile` 封存为第一代（今天 `cp -Rc` 的做法，秒级）；不一致才冷装（tool 的 `bun install` 几分钟；`target` 冷编译 30–40 分钟——所以种子缓存永远只克隆、不冷建）。[我的决定] "不比今天慢"的比较对象是温热的主 checkout。
2. `clonefile` 基线缓存到 `<root>/sessions/<session-id>/<repo>/`，`.git` 用 `git worktree add --detach` 等价的方式接到主 checkout 的 object store（这样工作区里的 `git status`、`git log` 都正常，且不复制对象）。[交给构建者：worktree 方式 vs 独立 `.git` 文件指向 common dir]
3. 写清单；对 `--repos` 指定的仓库开始监听；其余仓库标记"钉住、无 overlay"。
4. 种子缓存与种子文件从主 checkout 克隆/复制（§4）。
5. 打印各工作区路径（JSON）。

**依赖目录的归属**：依赖目录属于依赖集，不进 overlay、不被监听；会话若直接改了依赖目录（例如手动 `bun add`），`status` 会报"依赖目录漂移"，走 6.4 的流程。

### 6.2 Checkpoint 与恢复

- 触发：Claude Code 的 `PostToolUse`（Edit/Write/Bash）钩子调 `strata checkpoint --tool-call <id>`；显式 `strata checkpoint -m`；发布、恢复、依赖 rollout 之前自动一次。
- 算法：取监听维护的"可能脏"路径集 → `getattrlistbulk` 读属性，用 gen count / size 对比基线清单 → 只对判定变了的文件哈希（BLAKE3）→ 新内容写 blob → 写 overlay 行与 checkpoint 行。改回与基线相同的文件撤销覆盖。
- 监听丢事件（`MustScanSubDirs`/`KernelDropped`）→ 对该子树重扫；每次发布前和每小时一次全量对账 [间隔交给构建者]。**正确性不依赖监听**：监听整段丢失时退化为全扫源码树（排除依赖目录和 `.git`）。
- 恢复：`strata restore <checkpoint> [paths…]`。先对当前状态做一次 checkpoint（恢复本身可恢复），再把目标 checkpoint 的 overlay 投影回目录：改/增的文件从 blob `clonefile` 回去，墓碑的删除，未覆盖的回到基线。

### 6.3 跨会话查看

- `strata status [--all]`：每会话改了哪些路径、钉住的基线、依赖集、待确认的依赖变更。
- `strata grep <pattern> [--session <id> | --all]`：共享基线索引 + 目标会话的 overlay（tgrep 的 HybridIndex 精确语义：overlay 条目遮住基线路径）；默认按 cwd 识别本会话。
- `strata diff [--base | --session <id> | --between <a> <b>]`：`--between` 先把两边投影到同一基线（若钉的基线不同，把旧的一方按发布后 rebase 的算法投影到新基线），再输出：仅 A 改、仅 B 改、两边都碰（每个文件附三方合并预览：可自动合 / 冲突 hunk）。

### 6.4 依赖变更

1. 某会话改了锁文件（或主干的锁文件变了）：strata 检测到锁文件哈希与当前依赖集不符。
2. 在 staging 克隆里跑该 repo 配置的安装命令，封存为新依赖集 generation。
3. 提出变更的会话可立刻切到新依赖集（克隆替换它自己的依赖目录）[用户]。
4. `strata deps propose` 记录"待确认"；`strata status` 和每次 CLI 调用的 stderr 提示它；**不自动同步**。
5. 人类经 §6.9 确认后 `strata deps approve <id> [--sessions …]`：对每个目标会话，先检查它没有在依赖目录上打开的文件（dev server）；有则跳过并报告；无则按下面"一起移动"的原子动作替换。
6. 旧依赖集保留到没有会话或基线引用它。

**依赖与源码一起移动** [用户，§13 问题 1 选严格匹配]：`deps approve` 对一个会话 = 把它的基线移到新依赖集所属的 tree——源码 rebase（§6.5 第 7 步）和依赖目录替换是**一个原子动作**：先 checkpoint，在临时目录准备好新基线树与新依赖集的克隆，`rename` 交换，冲突标出；该会话有打开的文件时整个跳过并报告。反过来**不**自动成立：发布后 rebase 把基线移到链顶时，若链顶的安装输入变了，不自动换依赖集——而是记一条"待确认"提议（§6.4 第 4 步），该会话停在旧基线上等待 `deps approve`，`status` 显示它在等（§9 不变量 4 优先于本段：依赖目录只在人类确认后替换）。于是 §4 里"基线 = tree OID + 匹配的依赖集"在任何时刻都成立；代价是依赖变了的发布之后，其它会话落后主干直到有人确认——这正是用户要的。

### 6.5 发布 `strata publish`

1. 组发布组：`--from <session>:<path>…` 可多次；或交互式从 `status` 选。
2. 对每个涉及的 repo：取目标分支的最新 tree OID（fetch），作为合并基线 B。
3. 构建 staging 树：对每个批次路径，三方合并（基线 = 该会话钉住的基线上该文件的版本，两边 = 会话当前版本与 B 的版本）；多个会话碰同一文件时按会话顺序依次合并。hunk 不重叠自动合；冲突则**整个发布组停下**，列出冲突文件，`strata publish --assign <file> <session>` 指定解决者 [用户]；`.lww` 命中的文件最新 checkpoint 胜出。
4. 在 staging 克隆里跑该 repo 的检查命令（例如 tool：`tsc --noEmit --incremental false` + `bun run lint-antd-deprecated`；staff-api：`php-cs-fixer check`（即 `cs-fix --dry-run --diff`）+ `composer analyse`）；失败则停，不提交。**检查一律只读**：会改文件的命令（`cs-fix`、`prettier --write`）是配置错误，strata 在跑之前就拒绝；格式化是会话自己的事（它有编辑后自动格式化的钩子）。这样"commit 内容 = 某个 checkpoint 的内容"永远成立，发布后 rebase 不会把格式化过的文件标成冲突。[我的决定] [检查命令的默认值交给构建者从各仓库 CLAUDE.md 抄]
5. 从 staging 树做 commit（只含批次路径），接到该目标的**发布链**上（§6.7）；按配置推送；一推上线的 repo 没有通过人类确认（§6.9）就在推送前停下。
6. 记录发布：哪些会话的哪些 checkpoint 的哪些路径进了哪个 commit。
7. **发布后 rebase**：对每个参与的会话，新基线 = 推送后的 tree OID；重算 overlay（已发布的路径从 overlay 消失；未发布的路径对新基线做三方投影；冲突标出但不改动工作区文件，等会话自己处理）；把工作区目录里已发布的文件替换为新基线版本只在它们与会话当前内容一致时进行（否则保留会话版本并标出）。

### 6.6 删除会话 `strata rm <ticket>`

拒绝条件：有未发布改动且没有 `--discard`；有正在跑的进程打开其目录。删除前最后一次 checkpoint 保留 N 天 [交给构建者]。

### 6.7 发布目标的语义 [我的决定]

- 发布目标 = (repo, 分支名, push, confirm)。strata 在该 repo 的 common dir 里维护一条自己的 ref：`refs/strata/publish/<分支>`，叫**发布链**。它从不碰 `refs/heads/*`（主 checkout 的分支），不变量 2 保持。
- 合并基线 B：发布链存在且领先 `origin/<分支>` 时取链顶；否则取 `origin/<分支>`。这样 push=false 时连续两次发布不会把上一次的改动当成"对方撤销"而回退。
- push=true：发布后立刻把链推到 `origin/<分支>`；push=false：留在本地，之后 `strata push <repo>` 再推。推送成功后链与 origin 一致。
- 新会话钉基线时，链领先 origin 就钉链顶（配置 `pin_unpushed`，默认 true）；否则新会话看不到已发布未推送的改动，下一次发布必撞。
- 参与会话发布后的新基线 = 链顶的 tree。
- staff-api 这种"推任何分支都重建生产"的仓库，预期配置是 push=false + 人工 `strata push`。

### 6.8 工作区里的 git [我的决定]

- 工作区 `.git` 接到主 checkout 的 object store，HEAD **detached** 在钉住的基线，不设 upstream。
- 允许本地 `git add/commit/stash`：overlay 是整文件相对钉住基线的差异，会话内的 commit 不会让差异消失（tgrep 的规则）；暂存与否对 overlay 无意义；"未跟踪且合格" = 没被 gitignore。
- strata 装一个 `pre-push` 钩子：没有 `STRATA_PUBLISH=1` 就拒绝，agent 习惯性的 `git push` 推不出去；发布只经 strata。
- tool 的 dev-link 每 30 秒对兄弟库 `git fetch` + `pull --ff-only`：在 detached 的兄弟库上 `pull --ff-only` 直接报"不在分支上"而不做任何事，无害——写进测试矩阵核实；`fetch` 只动主 checkout 的 FETCH_HEAD，与现有的"fetch 循环打断同伴 pull"陷阱同源，strata 不替 dev-link 解决。

### 6.9 人类确认 [用户]

agent 和用户是同一个 uid，且常以 bypass 权限运行，所以 flag 拦不住 agent。以下动作必须经过 **agent 无法代答的通道**：一推上线仓库的推送、`deps approve`、`rm --discard`。

- daemon 通过 **LocalAuthentication**（Touch ID，回退登录密码）弹系统认证框，通过才执行；daemon 以 launchd agent 跑在用户的 GUI 会话里，才能弹框。[实现：`objc2` 的 LocalAuthentication 绑定，交给构建者]
- CLI 同时要求 stdin 是 TTY；非 TTY 调用直接拒绝并说明原因。
- 认证结果一次一用，不缓存；每个需确认的动作各弹一次。
- `--assign`（指定谁解冲突）不需要确认。

---

## 7. 数据与存储

- 位置：Store、基线缓存、依赖集缓存、工作区**全部在与主 checkout 同一个卷上**，默认 `/Volumes/GWD/inter/.strata/{store,bases,deps,sessions}/`；`~/Library/Application Support/git-strata/` 只放配置和 daemon 的 socket。原因：`clonefile` 只在同卷内有效，**跨卷退化成复制**，而内置盘只剩约 40 GB。
- SQLite：WAL；checkpoint 边界 `synchronous=FULL`（与 LayerFS 的 Disposable 不同：我们的数据量小，而"恢复"要求可靠）。
- Blob：按 BLAKE3 内容寻址，一文件一 blob，存为普通文件；写入 = 从工作区 `clonefile` 到 blob 路径（零拷贝），读出 = 反向克隆。超过阈值（默认 64 MiB，tgrep 的经验）的文件只记录哈希与克隆，不进索引。
- 基线缓存与依赖集缓存都是普通目录树，键在路径里，封存后只读（`chflags uchg` 或仅靠约定 [交给构建者]）。
- 空间回收：generation 引用计数（会话清单、checkpoint、发布记录）；无引用且超过保留期才删。[保留期交给构建者]

---

## 8. 性能与稳定性设计

- 热路径只读元数据：checkpoint = O(改动文件数)；全量对账 = 一次 `getattrlistbulk` 遍历源码树（排除依赖目录与 `.git`）。参考：tgrep 在 macOS 上对 linux 内核 96K 文件建全文索引 37 s（读全部内容 + 写 1 GB 索引）；我们不读内容。
- 新建会话 = 若干次目录 `clonefile`，秒级；依赖 rollout = 克隆 + rename，秒级。
- 索引：共享基线索引一次建好，多会话复用；会话 overlay 增量。
- 并发：daemon 内按 repo 一把写锁；CLI 调用可并行；同一会话的 checkpoint 串行。
- **验收数字**（写进测试）：新建会话 < 5 s（tool + 全部兄弟库）；checkpoint 改 50 个文件 < 200 ms；全量对账 tool 源码树 < 3 s；`grep` 热查询 < 300 ms。[这些是目标，首个 spike 要实测后修正]

---

## 9. 不变量（反目标的落实）

1. 任何写工作区的操作之前先 checkpoint；恢复本身可恢复。
2. strata 对主 checkout 只读：读它的 `.git`（对象、refs）和种子文件，写只限自己的 `refs/strata/*`；从不改它的工作树、索引或 `refs/heads/*`。
3. 发布只提交发布组里的路径；推送只推配置的目标；一推上线需**人类**确认（§6.9）。
4. 依赖目录只在 `deps approve` 之后、且该会话没有打开的文件时替换。
5. 基线与依赖集 generation 永不原地修改。
6. 任何不确定的失败（推送超时、合并中断）保留现场、不重放、不清理；`status` 报出来。
7. 人类确认 = LocalAuthentication 通过 + TTY；没有任何 flag、环境变量或配置能绕过它。
8. 会话的基线与依赖集永远匹配：两者只会一起移动。

---

## 10. 测试策略与基准映射

**功能基准** [用户]：LayerFS 的语义层测试 + tgrep 的索引与 worktree 同步层测试；Sandbox/daemon/FUSE 由真实目录替代。

| 来源 | 测试数（#[test]） | 处理 |
| --- | --- | --- |
| LayerFS `layerfs-content` / `storage` / `persistence` / `history` / `project` | 487 / 78 / 127 / 0 / 24 | 映射到 `store`、`overlay`：内容寻址、去重、append-only、Branch/Commit/publish 语义、Init 导入规则（含 `.git`、ignored、symlink）。用例逐条读，按语义重写为我们的 API；不适用的（分块编码、SQLite 包布局）标记"实现细节，不保留"。 |
| LayerFS `layerfs-workspace` / `overlay` | 176 / 75 | 映射到 `overlay`、`publish`：Commit 的 Conflict / HeadMoved / Dirty / UpToDate、`.lww`、脏前沿、显式 unmount 不隐式提交。FUSE 投影相关标记"由真实目录替代"。 |
| LayerFS `layerfs-api` / `daemon` / `fuse` / `sandbox` / `bridge` | 117 / 166 / 8 / 28 / 25 | Sandbox、exec、daemon 控制通道、FUSE：**不保留**，在矩阵里逐条标明替代物（真实目录 / 直接跑命令 / 无需认证通道）。`fs.*` 查询接口的用例保留，映射到 `cli`。 |
| LayerFS `sdk/js/test` | 12 文件 | 错误码词汇表（`Conflict`、`HeadMoved`、`Dirty`、`Busy`、`NoSuchPath`…）保留为 CLI 的错误码；init 拒绝规则保留。 |
| tgrep `worktrees` / `hybrid` / `gitignore` / `git_index` / `generations` | 见 `tgrep-core/src/*/tests.rs` | 映射到 `search`、`gitio`、`overlay`：overlay 遮蔽、墓碑、改回撤销、对账、大小写不敏感、已跟踪文件永不 ignore。 |
| 新增 | — | 兄弟布局、`clonefile` 跨卷退化、依赖 rollout 的打开文件检查、发布组三方合并、发布后 rebase、一推上线确认。 |

交付物：`docs/test-matrix.md`，每条原测试一行：来源、原意、目标模块、状态（保留 / 改写 / 替代 / 不保留 + 理由）。构建前完成。

---

## 11. 我替用户做的决定（可推翻）

1. 名字 `git-strata` [用户]，位置 `/Volumes/GWD/opensource/git-strata`，仓库 `github.com/dickwu/git-strata`；二进制 `git-strata`（可作 `git strata` 子命令），daemon `git-stratad`。
2. Rust；macOS API 走 `libc` / `objc2`；索引层以 `tgrep-core` 为库依赖。
3. 一台机器一个 daemon（launchd agent），unix socket RPC。
4. 批次按整文件路径，不按 hunk。
5. v1 无 GUI，只有 CLI + JSON；人看的页面以后再议。
6. 一推上线的仓库需要 `--confirm-deploy`。
7. 保留 `.lww` 规则，默认为空。
8. Store 走持久（WAL + checkpoint 边界 fsync），不采用 LayerFS 的 Disposable。
9. 工作区根默认与主 checkout 同卷。
10. 未指定的兄弟仓库也进会话（钉住、无 overlay、发布跳过）。
11. 依赖目录在工作区里是可写的 CoW 克隆；不可变只对 Store 里的 generation 成立；漂移按安装输入哈希判断。
12. 构建缓存（`target`、`.next/cache`）是"种子缓存"：只克隆、归会话、不同步、不封存。
13. 被 ignore 但必需的文件按每仓库"种子文件"清单从主 checkout 复制，不 checkpoint。
14. 第一代依赖集从主 checkout 现成的目录**认领**，不冷建。
15. 检查命令一律只读；会改文件的检查是配置错误。
16. 发布链 `refs/strata/publish/<分支>`；合并基线取链顶；新会话默认钉链顶。
17. 工作区 detached、无 upstream、`pre-push` 拒绝；允许本地 commit。
18. Store 与工作区同卷；会话 id 与 ticket 分离。

## 12. 交给构建者的决定

- diff3 / 三方合并用哪个 crate；FSEvents 走 `notify` 还是直接 FSEvents API。
- 工作区 `.git` 的接法（worktree vs gitfile 指向 common dir）。
- 依赖集键是否含工具链版本；对账间隔；generation 保留期；大文件阈值。
- 各仓库检查命令与依赖目录的默认配置（从各仓库 CLAUDE.md 抄）。
- 封存目录的只读手段。
- RPC 编码（JSON lines 即可）。

## 13. 待用户回答的问题

审阅者（独立子代理，以"要照它来建"的视角读）提出 7 个问题；5 个我已在文中决定（§11 第 11–18 条），两个由用户决定（2026-10-22）：

- **问题 1 → (a) 严格匹配**，落实在 §6.4 与不变量 8。
- **问题 2 → (a) LocalAuthentication**，落实在 §6.9 与不变量 7。

原问题留档如下。

**问题 1：依赖和源码必须一起移动吗？**
文档原本自相矛盾：§4 说基线 = tree OID + 匹配的依赖集，但 §6.4 的 `deps approve` 只换依赖目录不动源码，§6.5 第 7 步的发布后 rebase 只动源码不提依赖。
- (a) **严格匹配**：approve 一次依赖同步 = 把该会话的基线移到锁文件所属的主干 tree（源码 rebase + 依赖替换一起做，冲突照 §6.5 第 7 步标出，同样受"不在 dev server 脚下换"的约束）。依赖永远和源码对得上；代价是 approve 可能带来源码冲突。
- (b) **只换依赖**：会话源码留在旧基线，依赖换成主干的；此后它的锁文件和依赖集长期不符，`status` 会一直报漂移——等于把 §1.1 的"依赖与源码错位"反过来再造一遍。
推荐 (a)。

**问题 2：谁有权确认？**
agent 和用户是同一个 uid，而且常以 bypass 权限运行——`--confirm-deploy` 这种 flag 拦不住 agent 自己加上。一推上线的推送、`deps approve`、`rm --discard` 这些动作，要不要一个 **agent 无法代答的人类确认通道**？
- (a) **LocalAuthentication（Touch ID / 登录密码）**：daemon 弹系统认证框，通过才执行；mac 原生，agent 伪造不了。CLI 另要求 stdin 是 TTY。
- (b) 只靠 flag + TTY 检查：简单，但 agent 在终端里一样能加 flag。
- (c) 不限制：用户自己管 agent 的权限。
推荐 (a)，范围 = 一推上线的推送 + `deps approve` + `rm --discard`；`--assign` 不需要。
