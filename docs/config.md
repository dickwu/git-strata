# git-strata — 配置文档

> **状态**：模块文档阶段产出（计划 Task 2）。派生自 [`design.md`](design.md) §1–§4、§6.1、§6.4、§6.5、§6.7、§6.9、§11；与 `design.md` 冲突时以 `design.md` 为准。
> `RepoName`、`PublishTarget` 由 [`types.md`](types.md) 定义；**`Config`、`RepoConfig` 的 Rust 定义在本文 §3**（落在 crate `strata-types`），`types.md` 链接到这里。
> 附录 A 的每个值都在行内注明来源；附录 B 记录 2026-10-22 在干净的 `git archive` 导出里实测各检查命令的结果。行内标记：`[已核]` = 在导出里跑绿过；`[红]` = 在基线上就是红的，所以没有启用；`[未核]` = 照仓库文档抄，没有执行（`install` 一律未核：它们会冷装依赖；hr-core 的除外，附录 B 在导出里装过一次）；`[我的决定]` = design 和仓库文档都没有规定、由本文自己定的。

---

## 1. 概览

- **一份全局配置**：`~/Library/Application Support/git-strata/config.toml`（TOML 1.0，UTF-8）。该目录只放配置和 daemon 的 socket（design §7）。design §7 把这个目录写作改名前的 `strata/`；本文按项目名用 `git-strata/`，design 的这一处待同步。
- **只有两种表**：一张 `[paths]`，和每个仓库一张 `[repo.<name>]`。
- **没有第二个来源**：strata 不读主 checkout 里的任何 strata 配置文件（不变量 2：对主 checkout 只读，且只读 `.git` 与种子文件）；没有环境变量或命令行 flag 能覆盖配置。对需要确认的仓库（`publish.confirm = true`），没有任何 flag、环境变量或配置项能让它的推送绕过人类确认（不变量 3、7）。[实现时补的决定]（2026-10-26）：门只看 `publish.confirm`，`deploy_on_push` 只是信息，见 §2 `deploy_on_push` 条。但 `deploy_on_push`、`publish.confirm`、`publish.push` 本身就是同一个 uid 可写的文件里的设置，把它们调松等于绕过确认，所以 daemon 钉住（pin）它加载过的这三个值：之后的重载若调松其中任何一个——`deploy_on_push` 由 `true` 变 `false`，`publish.confirm` 由 `true` 变 `false`，`publish.push` 由 `false` 变 `true`——要先过一次人类确认（`Action::ConfigRelax { repo: RepoId }`，types.md §4），调紧立即生效 [交给 `daemon.md`]。
- **表是完整的，没有继承**：每张 `[repo.<name>]` 只在字段级有缺省值（§2.2 的"缺省"列），不存在"继承一份内置的 inter 默认表"。附录 A 是一份完整可用的 `config.toml`（把各小节的代码块按顺序拼起来即可）；它怎么随二进制分发（`include_str!`、安装时写出……）[交给构建者]，但只许维护这一份文本，不得在代码里手抄第二份。
- **解析与校验在 `strata-types`，无 I/O**：输入是文本，不是路径；读文件是 daemon 的事。配置无效时使用方必须拒绝使用，不得带着部分配置运行。`strata-types` 为 `Config::parse` 依赖 `toml`（types.md §2.2 已列入）；§2.4 的 argv 切分是几十行的纯字符串代码，自己写，不另加依赖。
- **`[repo.*]` 的全集 = 兄弟布局**。design §6.1 要求 `strata new` 对"每个兄弟仓库（不只是 `--repos` 指定的）"钉基线并克隆，原因是 dev-link 之类的脚本用 `../<目录名>` 找兄弟库（design §3）。一个仓库不在配置里，就不在任何会话里。inter 工作区里这种依赖来自：

| 消费者 | dev-link 镜像进 `node_modules/@inter/*` 的兄弟库 | 出处 |
| --- | --- | --- |
| admin | admin-shared、admin-time-off | `admin/dev-link.sh:37-38` |
| front | hr-components、hr-core、hr-tasks、hr-time-management、quick-start；另外 clone/同步 release-notes | `front/dev-link.sh:13`、`:48-57`、`:66-88` |
| tool | hr-components、hr-core、hr-tasks、hr-time-management、quick-start、admin-shared、admin-time-off | `tool/dev-link.mjs:32-40` |

另外 admin 的 `prebuild` 跑 `scripts/permission-map/generate.mjs`，它读 `../staff-api`、`../tool`、`../front`、`../app`；兄弟缺失时保留已提交的 `generated.json`，不会失败（`admin/scripts/permission-map/generate.mjs:64-70`，`admin/CLAUDE.md`）。

[实现时补的决定]（2026-10-28，publish.md D21）：D51 之后会话可以只含部分 repo（session.md D51），所以「全集 = 兄弟布局」只对不带 `--repo` 的会话成立。检查要的兄弟由 `check_siblings`（§2.2）显式列出，发布时布局会话里没有的由发布从它的 main tip 基线补进 staging。上表 admin、tool 两行就是附录 A 里这两个 repo 的 `check_siblings`；front 的 dev-link 不是检查，不列。

- **派生目录**：`paths.root` 之下固定是 `store/`、`bases/`、`deps/`、`sessions/`（design §7），不单独配置。工作区路径 = `<root>/sessions/<session-id>/<repo>/`：session id 由 `new` 生成（types.md 的 `SessionId`，形如 `s-k3q7xm2a`），ticket 只是会话的标签，不进路径（design §4、§11 第 18 条"会话 id 与 ticket 分离"）。

---

## 2. 文件格式

### 2.1 `[paths]`

| 字段 | 类型 | 缺省 | 语义 | 出处 |
| --- | --- | --- | --- | --- |
| `root` | string（绝对路径） | `/Volumes/GWD/inter/.strata` | Store、基线缓存、依赖集缓存、工作区的共同根；必须与每个仓库的 `path` 在同一卷（`clonefile` 只在同卷有效，跨卷是错误，不是降级） | design §7、§3 |

### 2.2 `[repo.<name>]`

`<name>` 就是 `RepoName`（types.md），必须等于 `path` 的最后一级目录名（规则 V1）；含 `.` 的名字要写成带引号的键（`[repo."foo.bar"]`）。TOML 里 `publish` 写成内联表（`publish = { branch = …, push = …, confirm = … }`），不用 `[repo.<name>.publish]` 子表——这是写法约定，serde 不区分两种写法，也不校验。完整示例见附录 A 的任一仓库。

| 字段 | TOML 类型 | 必填 / 缺省 | 一句话语义 | 出处 |
| --- | --- | --- | --- | --- |
| `path` | string（绝对路径） | 必填 | 主 checkout。strata 只读它的 `.git`、种子文件、种子缓存、依赖目录 | design §3、不变量 2 |
| `default_branch` | string | 必填 | 新基线取自 `origin/<default_branch>` 的 tree OID，不是主 checkout 的工作树 | design §3、§6.1 |
| `deps_dirs` | string 数组（仓库内相对路径，目录） | `["node_modules", "vendor"]` | 属于依赖集的目录 | design §4、§6.1 |
| `seed_caches` | string 数组（目录） | `["src-tauri/target", ".next/cache", "node_modules/.cache"]` | 种子缓存 | design §4 |
| `seed_files` | string 数组（文件） | `["CLAUDE.md", ".env", ".env.local", ".claude/settings.local.json"]` | 种子文件；缺省四项是候选（§2.3） | design §4、§3 |
| `install_inputs` | string 数组（文件或目录） | `[]` | 依赖集的键 | design §3、§4 |
| `install` | string（一条命令行） | 缺省 = 没有 | 在 staging 克隆里冷装依赖的命令 | design §6.1、§6.4 |
| `checks` | string 数组（命令行，有序） | `[]` | 发布前在 staging 树里跑的检查，只读 | design §6.5 步骤 4 |
| `publish` | 内联表 `PublishTarget` | 必填 | 发布目标 | design §6.7 |
| `deploy_on_push` | bool | `false` | 推送到 `origin/<publish.branch>` 本身就会触发生产部署 | design §3、不变量 3、7 |
| `pin_unpushed` | bool | `true` | 新会话钉基线时，发布链领先 origin 就钉链顶 | design §6.7 |
| `check_siblings` | string 数组（`RepoName`） | `[]` | `checks` 要在 `../<名字>` 找到的兄弟 repo；会话里没有时发布从它的 main tip 基线补上 | publish.md D21 [实现时补的决定] |

### 2.3 字段语义

- **`path`**：主 checkout 的路径。加载配置时它必须已经存在，并且等于它的 realpath：绝对、没有尾部 `/`、任何分量都不是符号链接。不是 realpath 的，加载时 `InvalidInput`，`detail` 写出 realpath（module-rulings「Repo paths in config are realpaths」）。通过之后 session、deps、apfs 把 `path` 原样往下传，不再解析。词法上的绝对与已规范化仍由 V2 在无 I/O 的 `validate` 里拒绝。
- **`deps_dirs`**：属于依赖集的目录（design §4）：不进 overlay、不被监听；在会话里是可写的 CoW 克隆（归会话所有）。`strata new` 优先从主 checkout **认领**（安装输入哈希与基线一致就 `clonefile` 封存为第一代），不一致才用 `install` 冷装（design §6.1）。目录里除 `node_modules`、`vendor` 外，还要包含 `install` 产出的其它被 ignore 的目录：tool 的 `postinstall` 把 wasm 等资源从 `node_modules` 复制到 `public/mediapipe/wasm`、`public/pdfium`、`public/file-viewer`；insurance-check 的 `postinstall` 另外装 `sidecar/node_modules`。
- **`seed_caches`**：构建缓存（design §4）。新建会话时从温热的主 checkout `clonefile` 过来，归会话所有、可写，**不同步、不 checkpoint、不封存**。可以嵌套在某个依赖目录之内（默认的 `node_modules/.cache`），封存依赖集时把它排除。冷建要 30–40 分钟（design §6.1），所以只克隆、绝不冷建。纯 Rust 仓库用 `target`（`src-tauri/target` 的对应物）。主 checkout 里不存在的条目静默跳过。
- **`seed_files`**：被 gitignore 但运行必需的文件，新建会话时从主 checkout 复制（design §4）；不 checkpoint；`status` 列出来源与时间，不显示内容（`staff-api/.env` 是生产库配置，`secrets.rs` 含密钥）。**条目必须是被 ignore 且未被跟踪的路径**：已被跟踪的文件随基线导出，再从主 checkout 复制会把可能是脏的版本盖到基线上；未被 ignore 的文件复制过去会以 `Untracked` 进 overlay。两种条目两种待遇 [我的决定]：**缺省的四项是候选**——每一项只在该仓库里未被跟踪且被 ignore 时才是种子文件，已被跟踪（admin、maxident-sync、insurance-check、rust-socket、rust-mail、gwd、sockdbg 七个仓库跟踪着 `CLAUDE.md`）或未被 ignore 的缺省项静默不算，不报错也不告警，所以省略 `seed_files` 的最小 `[repo.<name>]` 表在任何仓库上都能 `new`；**显式写出的条目**是硬要求：存在但被跟踪或未被 ignore → `session::new` 拒绝（`InvalidInput`，§4.3）。附录 A 的取法：design §4 的默认四项 ∩ 该仓库里被 ignore 的，再加仓库特有的（maxident-sync、insurance-check 的 `src-tauri/src/secrets.rs`：被 ignore，但没有它 Rust 编译不过）。主 checkout 里不存在的条目静默跳过。
- **`install_inputs`**：依赖集的键（design §3"锁文件不一定在 git 里"、§4）。每项是仓库内相对路径，指文件，或指目录（目录 = 其下被 git 跟踪的全部文件，递归）。内容经 BLAKE3 哈希，与平台、工具链一起得到 `DepsSetId`（types.md）。经验规则：
  1. 只列**被 git 跟踪**的文件。基线是从 git 对象导出的；被 ignore 的锁文件会被 `bun install` 改写，列进去会让漂移检测抖动。front 因此用被跟踪的 `package-lock.json`，不用被 ignore 的 `bun.lock`；surgical-timeout 的 `bun.lock` 同样被 ignore，所以它只有 `package.json`。
  2. `@inter/*` 的解析 SHA 不另设输入项：它们已经写在被跟踪的锁文件里（admin、tool 的 `bun.lock`）；front 的被跟踪 `package-lock.json` 也带 5 个 `@inter/*` 的 SHA，但 bun 实际解析用的是被 ignore 的 `bun.lock`，其中 3 个 SHA 与它不同（`B`）——所以对 front 这项只是漂移检测的键，不是"解析结果"；front 的 `@inter/*` 真正的解析 SHA 不在任何被跟踪文件里，是本文接受的盲区。
  3. 输入不只是锁文件：tool 的 `postinstall` 跑 `patch-package`，读被跟踪的 `patches/`，所以 `patches` 也是输入。
  4. `package.json` 的 `version` 每次 `build.sh` 发布都会变。哈希是否忽略 `version` 等与依赖无关的字段，由 `deps.md` 的 `inputs_hash` 决定 [交给构建者]；不忽略，每次发布都会产生一次无意义的"依赖变更"。
- **`install`**：冷装命令，在 staging 克隆的仓库根目录执行，产出 `deps_dirs`（design §6.4 第 2 步）。它本来就是安装，可以有写副作用。安装钩子（`postinstall`）由包管理器自己触发，不单列。
- **`checks`**：有序命令列表，在 staging 树里该仓库的根目录依次运行；任何一条非零退出就停，不提交（design §6.5 步骤 4）。**一律只读**：不得修改任何被 git 跟踪的文件，也不得新建没被 ignore 的文件；写被 ignore 的构建产物（`.next/`、`dist/`、`target/`、`node_modules/`）是允许的。这样"commit 内容 = 某个 checkpoint 的内容"永远成立，发布后 rebase 不会把格式化过的文件标成冲突（design §6.5）。词法规则见 §4.2。默认值的取法：
  1. 只放静态检查（类型检查、lint 式门禁）。测试只有在导出里不装依赖（或装依赖只要几秒）就能全绿时才进默认值（`@inter/*` 库的 `bun test`，附录 B）；其它测试不进：design §6.5 的示例都是静态检查；staff-api 的测试连生产库；admin 的测试本文没有实测。要加就追加一条（例如 slate_viewer 的 `bun run test`，附录 B：272 个测试 8 秒内通过）。
  2. 凡是在基线上就是红的检查不启用（`[红]`），否则每次发布都会停。附录 A 里因此（或因为没有可用的静态检查）`deploy_on_push = true` 却 `checks = []` 的仓库有四个：front（tsc 基线红）、admin-shared、hr-tasks、quick-start；对 `checks = []` 的仓库，`publish` 必须在输出里写明 "0 checks ran"，不得让空列表看起来像检查通过 [交给 `publish.md`]。
  3. `tsc` 一律带 `--incremental false`：所有 Next 应用的 `tsconfig.json` 都设了 `incremental: true`，不带的话 `tsc --noEmit` 会读写 `tsconfig.tsbuildinfo`——既不是真的只读，陈旧缓存还会把红说成绿（design §6.5 的 tool 示例就带它）。
  4. Next 应用先跑 `next typegen`：干净导出里没有被 ignore 的 `next-env.d.ts` 与生成的路由类型，`*.module.css` 的类型声明因此缺失，admin 的 `tsc` 报 334 个错、tool 报 226 个；先 `next typegen`（Next 文档："To ensure `next-env.d.ts` is present before type-checking run `next typegen`"）后 admin 归零。它只写 `next-env.d.ts` 与 `<distDir>/types`，不碰已跟踪文件（附录 B）。
  5. 依赖目录里的 `node_modules/@inter/*` 是 dev-link 从兄弟库镜像进去的，不是 `install` 产出的；镜像陈旧时 `tsc` 报幻影错（附录 B：tool 报 5 个，`@inter/hr-tasks` 缺 `MentionMember`；刷新镜像后归零）。所以需要新鲜镜像时，把 dev-link 写进 `checks` 的开头，并且 `publish` 要满足各自的前提 [交给 `publish.md`]：
     - admin 的 `./dev-link.sh`（单次、无 `--watch`）只写 `node_modules/@inter/*`，兄弟目录缺失时静默跳过；**但它的 self-heal 会在不是 git 工作树的目录里 `rm bun.lock`**（`git ls-files --error-unmatch bun.lock` 失败即删，`admin/dev-link.sh:15-18`；附录 B 复现过），所以 staging 树必须带 `.git`、且 `bun.lock` 在其中是被跟踪的。已列入。
     - tool 的 `bun dev-link.mjs --sync-only` 在兄弟目录缺失时会 `git clone`，并在布局根写 `../node_modules/tailwindcss`（`tool/dev-link.mjs:204-233`），所以要求完整的兄弟布局（design §3）。已列入。
     - front 的 `dev-link.sh` 会对干净的兄弟库 `git reset --hard origin/main`（`front/dev-link.sh:23`），**不得**列入。
     - [实现时补的决定]（2026-10-28）兄弟布局不再靠会话含全部 repo：admin、tool 的 `check_siblings` 列出 dev-link 要的兄弟，发布保证它们在 staging 里（publish.md D21）。
  6. 检查跑在 staging 树里，所以需要种子文件才能跑的检查（maxident-sync、insurance-check 的 `cargo check` 需要 `secrets.rs`）要么不列，要么由 `publish` 把种子文件带进 staging [交给 `publish.md`]；附录 A 对这两个仓库没有启用 Rust 检查。Tauri 应用的 Rust 侧检查同样没有启用：`src-tauri/target` 冷编译要 30–40 分钟（design §6.1）。
- **`publish`**：`PublishTarget { branch, push, confirm }`（types.md；design §6.7），三个字段都必须写。`branch` 是发布链 `refs/strata/publish/<branch>` 对应的分支，附录 A 里恒等于 `default_branch`（两者不同时 [我的决定]：发布链以 `origin/<publish.branch>` 为合并基并推到它；新会话仍钉 `origin/<default_branch>`，`pin_unpushed` 只在 `publish.branch == default_branch` 时有意义；细节交给 `publish.md` 与 `session.md`）；`push = true`：发布后立刻把链推到 `origin/<branch>`；`false`：留在本地，之后 `strata push <repo>`；`confirm`：该目标的推送是否需要人类确认。**实际是否需要确认 = `confirm || deploy_on_push`**（`RepoConfig::needs_human_confirm`）。默认 = 直接合主干并推（design §2 目标 7），即 `push = true, confirm = false`；一推上线的仓库 `confirm = true`；staff-api 例外：`push = false`，预期配置是人工 `strata push`（design §6.7，因为推任何分支都会重建生产）。
- **`deploy_on_push`**：为 `true` 时，推送必须经 design §6.9 的人类确认，没有任何 flag、环境变量或运行期配置能让这次推送绕过（不变量 3、7）；规则 V5 保证 `confirm` 也是 `true`。[实现时补的决定]（2026-10-26，peilin："将api 也改为不用确认"）：上面这句作废。`deploy_on_push` 只是信息（推送算作 `Action::PushDeploy`、status 里显示），不再是门；要不要人类确认只看 `publish.confirm`（`needs_human_confirm() = publish.confirm`），V5 一并撤销。现有配置里 `deploy_on_push = true` 的仓库都显式写了 `confirm = true`，门不变；唯一改的是 staff-api（`confirm = false`，`push = false`、`deploy_on_push = true` 不变）。把 `confirm` 从 true 改成 false 仍是"调松"，要人跑 `git-strata config relax <REPO>`（TTY + Touch ID）才生效。把 `deploy_on_push` 本身改成 `false` 属于"调松"，daemon 只在人类确认之后接受（§1 的钉住规则，[交给 `daemon.md`]）。`true` 的有：staff-api、front 和全部 `@inter/*` 库（admin-time-off、admin-shared、hr-core、hr-tasks、hr-components、hr-time-management、quick-start）——design §3、§11 第 6 条。
- **`pin_unpushed`**：新会话钉基线时，发布链领先 `origin/<branch>` 就钉链顶；`false` 则一律钉 `origin/<default_branch>`，新会话看不到已发布未推送的改动，下一次发布必撞（design §6.7）。只在 `publish.branch == default_branch` 时有意义：两者不同时新会话一律钉 `origin/<default_branch>`（见上条 `publish`）。
- **`check_siblings`**：[实现时补的决定]（2026-10-28，publish.md D21）每项是另一个已配置的 repo（V12）。只在 `checks` 非空时起作用。发布（以及 D2a 的无头重放）staging 时：布局会话钉了它 → 用那个钉；没钉 → 发布在锁外 fetch 它的 `origin/<default_branch>`、前移它的锚点、取 main tip 的基线（可能冷装依赖），克隆进 `<布局根>/<名字>/`。补不上 → `InvalidInput`（`siblings_missing`），`detail.fix` 是 `git-strata add --session <id> --repo …`，检查不跑。缺省空列表 = 旧行为，不碰任何兄弟。旧二进制不认这个键（V11），所以上线顺序是：先把新的 `git-strata` 与 `git-stratad` 一起装好，再改线上配置，最后重启 daemon；若在重启之后才改配置，要再重启一次才生效。

### 2.4 命令行字符串（`install` 与 `checks`）

一条命令一个字符串，不经过 shell（切分规则是本文定的 [我的决定]）：

1. 按 shell-words 规则切成 argv：以空白分隔，单引号、双引号把含空白的部分合成一个词，`\` 转义。
2. 不展开变量、`~`、通配符。引号之外出现 `|`、`&`、`;`、`<`、`>` 一律是 `InvalidInput`（规则 V9）——否则 `a && b` 会被当成"把 `&&` 和 `b` 当参数传给 `a`"，悄悄跑出另一种东西。
3. 工作目录：`checks` 是 staging 树里该仓库的根目录；`install` 是 staging 基线克隆里该仓库的根目录。
4. 程序名按 daemon 的 `PATH` 解析。launchd agent 的默认 `PATH` 不含 `~/.bun/bin`、`~/.cargo/bin`、`/opt/homebrew/bin`，所以 daemon 的 plist 必须带上用户的 `PATH` [交给 `daemon.md`]（daemon.md D1：`~/.bun/bin` 加 Homebrew 与系统目录；`~/.cargo/bin`、`~/.pub-cache/bin` 不在里面，cargo 用 Homebrew 的）[实现时补的决定]；staff-api 的 php 命令像它自己的 `composer.json` 脚本那样写绝对路径 `/opt/homebrew/bin/php`。

### 2.5 `[dev_ports]`（可选）[实现时补的决定]（2026-10-28，dev-ports 设计）

每个会话工作区（不含主 checkout）在 `new` / `add` 时租一组 4 个连续的 TCP 端口：`web`（dev server，`PORT`）、`connector`、`connector_mcp`、`aux`。租约记在 Store 的 `dev_ports` 表（store.md §4.4a），由 `git-strata session ports` / `session exec` 交给进程（cli.md §3.2）。

| 字段 | 类型 | 缺省 | 语义 |
| --- | --- | --- | --- |
| `range` | string `"LO-HI"`（含两端） | `"20000-29999"` | 切成按 4 对齐的块；每块是一个租约 |
| `exclude` | 端口数组 | `[]` | 含其中任一端口的块永不出租 |

规则（`DevPorts::validate`，`Config::validate` 的最后一步，`InvalidInput`，`detail.rule`）：
- P1：`range` 是 `LO-HI` 且 `LO <= HI`；`exclude` 没有 0。
- P2：范围在 1024–49151 之内（低于 macOS 的临时端口），且不与常见默认端口重叠：3000–3999（Next）、5173（Vite）、8080、9300–9400（tauri-connector bridge）、9555–9656（tauri-connector WS/MCP）。所以主 checkout 用默认端口，与任意多个会话共存。
- P3：去掉 `exclude` 后至少剩一块。

整个表可以不写（全用缺省）。daemon 启动时按它设置分配策略；只有装上带租约的 daemon 后才该写这个表（旧 daemon 会以 V11 拒绝未知键）。分配时对每块的 4 个端口做绑定探测（`0.0.0.0`、`[::]`、`127.0.0.1`，`SO_REUSEADDR`），不通过的块跳过；全部用尽时 `new` / `add` 照常成功，只是没有租约（`session ports` 再要时 `Busy ports_exhausted`）。

---

## 3. Rust 定义（`strata-types`）

`Config`、`RepoConfig`、`Paths` 的定义在这里，`RepoName`、`PublishTarget`、`RepoPath`、`StrataError` 来自 `types.md`。下面的 `impl` 块只列签名，实现按 §4 的规则写。

```rust
// crates/strata-types/src/config.rs
use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{PublishTarget, RepoName, RepoPath, StrataError};

/// `config.toml` 的内存形态。无 I/O。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub paths: Paths,
    /// TOML 里的 `[repo.<name>]`。BTreeMap：遍历顺序 = 名字的字典序，稳定。
    #[serde(rename = "repo")]
    pub repos: BTreeMap<RepoName, RepoConfig>,
    /// `[dev_ports]`（§2.5），可省略。
    #[serde(default)]
    pub dev_ports: DevPorts,
}

/// `[dev_ports]`（§2.5）。`blocks()` 给出每个可用块的起始端口（升序）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevPorts {
    #[serde(default = "DevPorts::default_range")]
    pub range: String,
    #[serde(default)]
    pub exclude: Vec<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Paths {
    /// Store、基线缓存、依赖集缓存、工作区的共同根；缺省 `/Volumes/GWD/inter/.strata`。
    #[serde(default = "Paths::default_root")]
    pub root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoConfig {
    /// 主 checkout，绝对路径。
    pub path: PathBuf,
    /// 分支短名，如 `main`、`master`。
    pub default_branch: String,
    /// 缺省 `["node_modules", "vendor"]`。
    #[serde(default = "RepoConfig::default_deps_dirs")]
    pub deps_dirs: Vec<RepoPath>,
    /// 缺省 `["src-tauri/target", ".next/cache", "node_modules/.cache"]`。
    #[serde(default = "RepoConfig::default_seed_caches")]
    pub seed_caches: Vec<RepoPath>,
    /// `None` = 没写，用缺省四项 `["CLAUDE.md", ".env", ".env.local", ".claude/settings.local.json"]` 当候选（只在该仓库里未被跟踪且被 ignore 时生效）；
    /// `Some(list)` = 显式条目，硬要求（§2.3）。用 `Option` 是为了区分"没写"与"写了"[我的决定]；读取方用 `seed_files()`。
    #[serde(default)]
    pub seed_files: Option<Vec<RepoPath>>,
    /// 文件或目录；目录 = 其下被跟踪的全部文件。
    #[serde(default)]
    pub install_inputs: Vec<RepoPath>,
    /// 一条命令行（§2.4）；`None` = 没有依赖目录可装。
    #[serde(default)]
    pub install: Option<String>,
    /// 有序；每条一条命令行（§2.4），只读（§4.2）。
    #[serde(default)]
    pub checks: Vec<String>,
    /// 内联表 `{ branch, push, confirm }`，三个字段都必须写。
    pub publish: PublishTarget,
    #[serde(default)]
    pub deploy_on_push: bool,
    /// 缺省 `true`。
    #[serde(default = "RepoConfig::default_pin_unpushed")]
    pub pin_unpushed: bool,
    /// 检查要在 `../<名字>` 找到的兄弟 repo（publish.md D21）；缺省空。[实现时补的决定]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub check_siblings: Vec<RepoName>,
}

// ---- 签名清单（不是实现）----
// Default for Paths：root = Paths::default_root()

impl Paths {
    pub fn default_root() -> PathBuf;     // /Volumes/GWD/inter/.strata
    pub fn store(&self) -> PathBuf;       // <root>/store
    pub fn bases(&self) -> PathBuf;       // <root>/bases
    pub fn deps(&self) -> PathBuf;        // <root>/deps
    pub fn sessions(&self) -> PathBuf;    // <root>/sessions
}

impl RepoConfig {
    pub fn default_deps_dirs() -> Vec<RepoPath>;
    pub fn default_seed_caches() -> Vec<RepoPath>;
    pub fn default_seed_files() -> Vec<RepoPath>;
    /// `(候选列表, 是否显式)`：`None` → `(default_seed_files(), false)`；`Some(v)` → `(v, true)`。
    pub fn seed_files(&self) -> (Vec<RepoPath>, bool);
    pub fn default_pin_unpushed() -> bool;
    /// design §9 不变量 3、7：`self.publish.confirm || self.deploy_on_push`。
    /// [实现时补的决定]（2026-10-26）改为只看 `self.publish.confirm`，见 §2 `deploy_on_push` 条。
    /// 即使 `validate` 被绕过（代码里直接构造结构体），门也不会被配置打开；配置本身被调松则由 daemon 的钉住规则把守（§1）。
    pub fn needs_human_confirm(&self) -> bool;
    /// V9 与 V10：`install` 与全部 `checks` 的切分与只读规则（§2.4、§4.2）。
    /// `Config::validate` 对每个仓库调用它，并在 `detail` 里补上 `repo`；
    /// `publish::check` 在运行任何检查之前再调用一次（此时 `detail.repo` 为 `null`）。
    pub fn validate_commands(&self) -> Result<(), StrataError>;
}

impl Config {
    /// `toml::from_str` 之后调用 `validate`。
    pub fn parse(text: &str) -> Result<Config, StrataError>;
    /// §4.1 与 §4.2 的全部规则，无 I/O。
    pub fn validate(&self) -> Result<(), StrataError>;
    /// 未配置的名字 → `Code::NoSuchRepo`。
    pub fn repo(&self, name: &RepoName) -> Result<&RepoConfig, StrataError>;
}
```

校验失败返回 `StrataError { code, message, detail }`（types.md），`detail` 的形状固定为：

```json
{ "repo": "tool", "field": "checks", "index": 1, "rule": "V10" }
```

`repo` 在 `[paths]`、全局规则或调用方不知道仓库名时（`publish::check`）为 `null`；`index` 在不是数组元素时为 `null`；`rule` 是 §4.1 的规则编号。TOML 本身解析失败（语法错误、类型不符）也是 `InvalidInput`：`rule` 为 `"toml"`，并多一个 `message` 键放 `toml` 的原话（含行列）；未知键由 `deny_unknown_fields` 在同一步解析里报出，`parse` 把这类错误映射为 `rule = "V11"`（`message` 同样带 toml 的原话）[我的决定]。

`PublishTarget` 定义在 types.md，并且已经带 `#[serde(deny_unknown_fields)]`。规则 V11 要求 `publish` 内联表拒绝多出来的键，解析期就由这个属性挡住，不改变序列化形状。

---

## 4. 校验规则

### 4.1 静态规则（`Config::validate`，无 I/O）

按 `[paths]`、再按各 `[repo.<name>]` 表在文件里的出现顺序检查，表内按 V1→V12 的顺序；只报第一条违规 [我的决定]。

| 编号 | 规则 | 错误码 | 依据 |
| --- | --- | --- | --- |
| V1 | `repo` 表非空；`<name>` 是合法的 `RepoName`（types.md：`[A-Za-z0-9._-]`，非空，不是 `.` 或 `..`），且等于 `path` 的最后一级目录名 | `InvalidInput` | design §3（dev-link 用 `../<目录名>` 找兄弟）、§4（工作区路径 `<root>/sessions/<session-id>/<repo>/`，见 §1） |
| V2 | `paths.root` 与每个 `path` 是绝对、已规范化的路径：不含 `.`、`..`、`//`、尾部 `/`，不展开 `~` | `InvalidInput` | design §7（daemon 由 launchd 启动，工作目录不确定，相对路径无从解析） |
| V3 | `paths.root` 不得位于任何 `path` 之内，任何 `path` 不得位于 `paths.root` 之内 | `InvalidInput` | design §9 不变量 2（strata 不写主 checkout 的工作树）[我的决定] |
| V4 | `default_branch`、`publish.branch` 是合法的分支短名（纯字符串实现 `git check-ref-format --branch` 的规则，不调用 git）：非空；不含 ASCII 控制字符、空格和 `~ ^ : ? * [ \`；不含 `..`、`@{`、`//`；不以 `/` 开头或结尾；每一级不以 `.` 开头或结尾、不以 `.lock` 结尾；不等于 `@`、不等于 `HEAD`、不以 `-` 开头；不以 `refs/` 开头 | `InvalidInput` | design §6.7（分支名拼进 `refs/strata/publish/<分支>`） |
| V5 | **已撤销**（[实现时补的决定]（2026-10-26，peilin："将api 也改为不用确认"）；`publish.confirm` 是唯一的门，见 §2 `deploy_on_push` 条）。原规则：`deploy_on_push = true` ⇒ `publish.confirm = true`（加载期的显式检查；运行期仍以 `needs_human_confirm` 兜底，types.md 的 `PublishTarget.confirm` 注释说的是这一层） | `InvalidInput` | design §3、§9 不变量 3 与 7（没有配置能关掉一推上线的人类确认） |
| V6 | 四个路径列表（`deps_dirs`、`seed_caches`、`seed_files`、`install_inputs`）的每一项都是合法 `RepoPath`（types.md），不是 `.git` 也不在 `.git` 之下；同一列表内无重复、互不嵌套 | `InvalidInput` | design §4 |
| V7 | 四个列表之间没有相同或嵌套的路径；唯一例外：`seed_caches` 的条目可以位于某个 `deps_dirs` 条目之内（默认的 `node_modules/.cache`） | `InvalidInput` | design §4（依赖目录不进 overlay；种子缓存不封存、不 checkpoint）[我的决定] |
| V8 | `deps_dirs` 非空 ⇔ `install` 存在且 `install_inputs` 非空（没有依赖目录就不能有安装命令和输入；有依赖目录就必须能装、能判漂移） | `InvalidInput` | design §4 Deps set、§6.4 [我的决定] |
| V9 | `install` 与每条 `checks`：非空；能按 §2.4 切分（引号配对）；引号之外没有 `\|`、`&`、`;`、`<`、`>` | `InvalidInput` | §2.4 |
| V10 | `checks` 的只读规则（§4.2） | `CheckMutates` | design §6.5 步骤 4、§11 第 15 条 |
| V11 | 任何表里的未知键都拒绝；`publish` 内联表恰好三个字段 | `InvalidInput` | — |
| V12 | `check_siblings` 的每项是另一个已配置的 repo：不是本 repo，不重复。`detail` 带 `repo`、`field = "check_siblings"`、`index`、`rule = "V12"`。未知键 V11 由 serde 在解析时报，先于其它规则 | `InvalidInput` | publish.md D21 [实现时补的决定] |

### 4.2 `checks` 只读规则（`CheckMutates`）

对每一条 `checks`，在**整行原文**（引号内外都算）里按"词"查找；词边界 = 行首、行尾，或不属于 `[A-Za-z0-9_-]` 的字符：

- 含词 `--write`（`--write=…` 也算）→ `CheckMutates`；
- 含词 `cs-fix` 且不含词 `--dry-run` → `CheckMutates`。

按词而不是按子串匹配，是为了不误伤 `php-cs-fixer check`：design §6.5 步骤 4 把它列为合法的检查（等价于 `cs-fix --dry-run --diff`），而 `php-cs-fixer` 里的 `cs-fix` 后面紧跟 `e`，不构成一个词。

谁返回：`Config::validate` 在加载配置时经 `RepoConfig::validate_commands`（§3）返回它，daemon 因此拒绝整份配置。types.md 把 `CheckMutates` 的返回者记作 `publish`（`check`）：它在运行任何检查之前再调用一次同一个函数，所以不经过 `parse`、直接用结构体构造出来的配置，也跑不出一条会改文件的检查（与 `needs_human_confirm` 同一思路）。

| 例子 | 判定 | 原因 |
| --- | --- | --- |
| `prettier --write .` | `CheckMutates` | 含词 `--write` |
| `sh -c "prettier --write ."` | `CheckMutates` | 整行原文，引号内也算 |
| `composer cs-fix -- src/Foo.php` | `CheckMutates` | 含词 `cs-fix`，没有 `--dry-run`（staff-api 的 `cs-fix` 脚本是 `php-cs-fixer fix $1`） |
| `composer cs-fix -- --dry-run --diff src/Foo.php` | 通过 | 有 `--dry-run` |
| `vendor/bin/php-cs-fixer check --diff` | 通过 | `php-cs-fixer` 不是词 `cs-fix` |
| `bunx tsc --noEmit --incremental false` | 通过 | — |

这条词法规则只是下限。下面这些写法同样会改文件，但不在上面两条里，附录 A 不使用，构建者可以按同样方式追加到规则里：`cargo fmt`（无 `--check`）、`prettier -w`、`eslint --fix`、`php-cs-fixer fix`（无 `--dry-run`）[交给构建者]。

### 4.3 运行期校验（需要 I/O，不在 `strata-types`）

这些规则要看文件系统或 git。第一行是加载时的 realpath 核对（module-rulings「Repo paths in config are realpaths」）；其余由用到配置的模块在用到时执行。失败同样返回 `StrataError`，`detail` 的形状同 §3。错误码取自 types.md §6.2 的错误码表。

| 规则 | 谁、何时 | 错误码 |
| --- | --- | --- |
| 每个 `path` 必须存在且等于它的 realpath（绝对、无尾部 `/`、任何分量都不是符号链接）。否则拒绝，`detail` 写出 realpath。通过之后调用方把 `path` 原样往下传（module-rulings「Repo paths in config are realpaths」） | daemon，加载配置时 | `InvalidInput` |
| 每张 `[repo.<name>]` 的 `path` 存在 | `session`，用到该仓库时 | `NoSuchPath` |
| `paths.root`（存在时）与每个 `path` 是目录；`deps_dirs`、`seed_caches` 里已存在的条目是目录（不存在则静默跳过） | `session`、`deps`；`apfs::clone_tree` 的源不是目录时兜底 | `NotDirectory` |
| `seed_files` 里已存在的条目是普通文件：是目录 → `IsDirectory`，是 FIFO、socket、设备文件 → `NotFile`；`install_inputs` 里已存在的条目既不是普通文件、符号链接，也不是目录 → `NotFile` | `apfs::clone_file`（`session::new` 复制种子文件时）；`deps::inputs_hash` | `IsDirectory`、`NotFile` |
| 每个 `path` 是一个 git 仓库（能解析出它的 git common dir）；两张 `[repo.<name>]` 表不得解析到同一个 git common dir，即同一个 `RepoId` | `daemon`，加载配置时，整份配置一起拒绝 | `InvalidInput`（不是仓库）、`NameTaken`（`RepoId` 重复） |
| fetch 之后 `origin/<default_branch>` 存在；`origin/<publish.branch>` 存在，或者该分支已有发布链 `refs/strata/publish/<branch>` | `gitio`，`session::new` 每次取基线时；`publish` 透传 | `NoSuchBranch` |
| `paths.root` 与每个 `path` 在同一个 APFS 卷（`clonefile` 只在同卷有效；跨卷是错误，从不退化成复制）；`paths.root` 还不存在时，取它最近的已存在的上级目录所在的卷 | `session::new` 在克隆之前；`store::open`；`apfs::clone_*` 遇到 `EXDEV` 兜底 | `CrossVolume` |
| 每个**显式写出**的 `seed_files` 条目（存在时）在该仓库里被 ignore，且在基线 tree 里未被跟踪；不存在则静默跳过。缺省列表的四项不受此规则约束：不满足的项静默不算（§2.3）。"被 ignore"由 `gitio::is_ignored` 判定，含 git 的全局排除文件（`core.excludesFile`）：release-notes、hr-core、hr-tasks、hr-components、hr-time-management、quick-start、rust-mail、gwd、sockdbg 九个仓库的 `.claude/settings.local.json` 只靠 `~/.config/git/ignore` 被 ignore [交给 `gitio.md`] | `session::new`（复制种子文件时） | `InvalidInput` |
| 每个 `install_inputs` 条目在基线 tree 里存在且被跟踪 | `deps::inputs_hash` | `InvalidInput` |

---

## 附录 A. inter 工作区的默认配置

覆盖任务清单里的 22 个仓库，顺序同清单。工作区里还有别的目录（`app`、`staff-api-container`、`database-sqlserver-incubator`、`cloakbrowser`、`exam-center`、`infra-docs` 等）不在这 22 个里，没有配置；需要时按 §2 追加 `[repo.<name>]`。

来源标记：

| 标记 | 含义 |
| --- | --- |
| `G` | 2026-10-22 对该仓库的 git 观察：`origin/HEAD`、`git ls-files`、`git check-ignore`、`.gitignore` 行号 |
| `P` | 该仓库的 `package.json` / `composer.json` / `Cargo.toml` |
| `C` | 该仓库自己的 `CLAUDE.md`（或 `AGENTS.md`） |
| `R` | `/Volumes/GWD/inter/CLAUDE.md` |
| `L` | `dev-link.sh` / `dev-link.mjs` |
| `D` | 本仓库 `docs/design.md` 的章节 |
| `I` | `/Volumes/GWD/inter/infra-docs/04-deployment.md` §1 的 webhook 表（2026-09-15 核验） |
| `B` | 本文附录 B 的实测记录 |

### A.1 `[paths]`

```toml
[paths]
root = "/Volumes/GWD/inter/.strata"   # D§7；与下面每个 path 同卷（/Volumes/GWD）
```

### A.2 前端应用：admin、front、tool

**admin**：分支是 `master` 不是 `main`（`C`）。`CLAUDE.md` 已被跟踪，随基线走，所以不在 `seed_files` 里。`tsc` 要先跑 dev-link 才可信（`C`："`bunx tsc --noEmit` lies unless `./dev-link.sh` has run first"；"tsc is the only gate here"），而 `./dev-link.sh` 在没有 `.git` 的目录里会删掉被跟踪的 `bun.lock`（§2.3 checks 第 5 条）；测试不列（`C`：测试有过基线红）。

**front**：`CLAUDE.md`、`AGENTS.md`、`.claude*` 被 ignore（`C`）；`bun.lock` 被 ignore，被跟踪的锁文件是 npm 格式的 `package-lock.json`（`C`、`G`）。`tsc` 在基线上是红的（`B`），所以 `checks` 为空。

**tool**：`CLAUDE.md` 被 ignore，`AGENTS.md` 被跟踪（`C`）。`postinstall` 做两件事——`patch-package`（读 `patches/`）和把 wasm 资源复制到三个被 ignore 的 `public/` 子目录（`P`、`G` `.gitignore:94-97`、`scripts/sync-mediapipe.ts` 与 `scripts/sync-file-viewer-assets.ts` 的头注释），所以这三个目录属于依赖集，`patches` 属于安装输入。

```toml
[repo.admin]
path = "/Volumes/GWD/inter/admin"
default_branch = "master"                  # G origin/HEAD=origin/master；C "This repo is on master, not main"
deps_dirs = ["node_modules"]               # G
seed_caches = ["src-tauri/target", ".next/cache", "node_modules/.cache"]   # D§4 默认值；G 前两个在盘上
seed_files = [".env", ".env.local", ".claude/settings.local.json"]         # G CLAUDE.md 已跟踪（随基线）；这三个被 ignore
install_inputs = ["package.json", "bun.lock"]                              # G bun.lock 已跟踪
install = "bun install --frozen-lockfile"  # R 释放 worktree 的配方 [未核]
checks = [
  "./dev-link.sh",                         # C+L 单次同步，只写 node_modules/@inter/*；注意：非 git 工作树里会 rm 掉 bun.lock（§2.3 checks 第 5 条）；B 在 git 工作树里 bun.lock 保留 [已核]
  "bunx next typegen",                     # B 干净导出里不跑它 tsc 报 334 个错，跑了归零 [已核]
  "bunx tsc --noEmit --incremental false", # C 唯一的门禁；D§6.5；B 0 错 [已核]
]
publish = { branch = "master", push = true, confirm = false }   # D§6.7 默认
deploy_on_push = false                     # I 无 webhook（admin 靠手动 build.sh 发布）
pin_unpushed = true                        # D§6.7 默认
check_siblings = ["admin-shared", "admin-time-off"]   # D21：admin 的 checks 读 ../admin-shared、../admin-time-off（dev-link）；不在 session 里时 publish 用它们 main 的最新 base [实现时补的决定]

[repo.front]
path = "/Volumes/GWD/inter/front"
default_branch = "main"                    # G origin/HEAD=origin/main
deps_dirs = ["node_modules"]               # G
seed_caches = [".next/cache", "node_modules/.cache"]                        # D§4 默认值去掉 src-tauri/target：front 无 Tauri（C）
seed_files = ["CLAUDE.md", ".env", ".env.local", ".claude/settings.local.json"]   # G 四项都被 ignore；CLAUDE.md、.env.local 在盘上
install_inputs = ["package.json", "package-lock.json", "bunfig.toml"]   # G bun.lock 被 ignore；package-lock.json 已跟踪，含 5 个 @inter/* 条目（B 只核对了存在：3 个的 SHA 与盘上的 bun.lock 不同，bun 解析用的是 bun.lock，这里只拿它当漂移键）；bunfig.toml（install.peer=false）影响 bun install，已跟踪
install = "bun install"                    # P scripts.build 自己跑 `bun install`；C [未核]
checks = []                                # [红] B：tsc 在 origin/main(7e64cb2) 上 5 个错，front 设了 typescript.ignoreBuildErrors（C）。绿了再启用：
                                           #   ["bunx next typegen", "bunx tsc --noEmit --incremental false"]
                                           # dev-link.sh 不得列入：它对干净的兄弟库 git reset --hard origin/main（L front/dev-link.sh:23）
publish = { branch = "main", push = true, confirm = true }   # D§3 front 推 main = 自动部署，需显式确认
deploy_on_push = true                      # D§3；I "Staff Front" webhook；C "A push to main IS the deploy"
pin_unpushed = true

[repo.tool]
path = "/Volumes/GWD/inter/tool"
default_branch = "main"                    # G
deps_dirs = ["node_modules", "public/mediapipe/wasm", "public/pdfium", "public/file-viewer"]   # P postinstall；G .gitignore:94-97
seed_caches = ["src-tauri/target", ".next/cache", "node_modules/.cache"]   # D§4；G 三个都在盘上
seed_files = ["CLAUDE.md", ".env", ".env.local", ".claude/settings.local.json"]   # G 四项都被 ignore；CLAUDE.md、.claude/settings.local.json 在盘上
install_inputs = ["package.json", "bun.lock", "patches", "bunfig.toml"]   # G bun.lock、patches/、bunfig.toml（install.peer=false）已跟踪；P postinstall = patch-package + 两个 sync 脚本
install = "bun install --frozen-lockfile"  # R；触发 postinstall [未核]
checks = [
  "bun dev-link.mjs --sync-only",          # L 刷新 node_modules/@inter/* 镜像；B 不刷新则 tsc 报 5 个幻影错。前提：完整的兄弟布局（§2.3 checks 第 5 条）[已核]
  "bunx next typegen",                     # B [已核]
  "bunx tsc --noEmit --incremental false", # D§6.5 的 tool 示例；C；B 刷新镜像后 0 错 [已核]
  "bun run lint-antd-deprecated",          # C 硬门禁（build.sh 里也是）；B 只读，通过 [已核]
]
publish = { branch = "main", push = true, confirm = false }   # D§6.7 默认
deploy_on_push = false                     # I 无 webhook；但 tool 的 build.sh 会推 6 个兄弟库，其中 5 个的推送会部署 front（C tool、front）
pin_unpushed = true
check_siblings = ["admin-shared", "admin-time-off", "hr-components", "hr-core", "hr-tasks", "hr-time-management", "quick-start"]   # D21：tool 的 checks 读这 7 个兄弟库（dev-link）[实现时补的决定]
```

### A.3 kiosk 与诊所桥接：slate_viewer、surgical-timeout、appointment_patient_viewer、patient_9_grid、maxident-sync、insurance-check

六个仓库都是 Next 16 静态导出 + Tauri，`bun@1.3.3`。ESLint 因 TypeScript 7 跑不起来（`R` "Linting"），所以门禁是 `tsc`（maxident-sync、insurance-check 的 `lint` 脚本本身就是 `tsc --noEmit`；surgical-timeout 没有 lint 脚本）。除 surgical-timeout 外 `bun.lock` 都被跟踪。`CLAUDE.md` 在 slate_viewer、surgical-timeout、appointment_patient_viewer、patient_9_grid 被 ignore，在 maxident-sync、insurance-check 被跟踪。目录名用下划线（`slate_viewer`、`appointment_patient_viewer`、`patient_9_grid`），发布槽用连字符（`slate-viewer` 等）；配置名取目录名（规则 V1）。

**surgical-timeout**：`bun.lock` 被 ignore（`.gitignore:47`），没有被跟踪的锁文件，只能靠 `package.json`（`C`：依赖用 caret 范围）；此时 `--frozen-lockfile` 会因为没有锁文件而失败，所以 `install` 是 `bun install`。`C` 把它的门禁写成 `test + tsc + prettier --check + build`；`build` 太重，`test` 按 §2.3 不进默认值。

**maxident-sync、insurance-check**：`src-tauri/src/secrets.rs` 被 ignore（`.gitignore:77`）但编译必需（`C` "Build prerequisite"），所以是种子文件。insurance-check 的 `sidecar/` 有自己的 `package.json` 与 `node_modules`，`sidecar/package-lock.json` 被 ignore（`.gitignore:52`）所以不是输入；`sidecar/node_modules/cloakbrowser` 是指向被跟踪目录 `../vendor/cloakbrowser` 的相对软链（`G`），克隆后仍指向工作区自己的那份。

```toml
[repo.slate_viewer]
path = "/Volumes/GWD/inter/slate_viewer"
default_branch = "main"                    # G；C "Commit directly to main"
deps_dirs = ["node_modules"]               # G
seed_caches = ["src-tauri/target", ".next/cache", "node_modules/.cache"]   # D§4；G 前两个在盘上
seed_files = ["CLAUDE.md", ".env", ".env.local", ".claude/settings.local.json"]   # G 四项都被 ignore（C ".gitignore:54"）；AGENTS.md 已跟踪
install_inputs = ["package.json", "bun.lock"]   # G；C "bun.lock is intentionally committed"
install = "bun install --frozen-lockfile"  # R [未核]
checks = ["bunx next typegen", "bunx tsc --noEmit --incremental false"]   # C "Type check: bunx tsc --noEmit"；B 0 错 [已核]
publish = { branch = "main", push = true, confirm = false }   # D§6.7 默认
deploy_on_push = false                     # I 无 webhook
pin_unpushed = true

[repo.surgical-timeout]
path = "/Volumes/GWD/inter/surgical-timeout"
default_branch = "main"                    # G
deps_dirs = ["node_modules"]               # G
seed_caches = ["src-tauri/target", ".next/cache", "node_modules/.cache"]   # D§4；G 三个都在盘上
seed_files = ["CLAUDE.md", ".env", ".env.local", ".claude/settings.local.json"]   # G 四项都被 ignore（C ".gitignore:53,55"）
install_inputs = ["package.json"]          # G bun.lock 被 ignore（.gitignore:47）
install = "bun install"                    # 无被跟踪的锁文件，不能 --frozen-lockfile [未核]
checks = [
  "bunx next typegen",                     # B [已核]
  "bunx tsc --noEmit --incremental false", # C 门禁之一；B 0 错 [已核]
  "bunx prettier --check .",               # C 门禁之一；B 通过（5 s）[已核]
]
publish = { branch = "main", push = true, confirm = false }   # D§6.7 默认
deploy_on_push = false                     # I 无 webhook
pin_unpushed = true

[repo.appointment_patient_viewer]
path = "/Volumes/GWD/inter/appointment_patient_viewer"
default_branch = "main"                    # G
deps_dirs = ["node_modules"]               # G
seed_caches = ["src-tauri/target", ".next/cache", "node_modules/.cache"]   # D§4；盘上这份是干净克隆，三个都还没有，条目静默跳过
seed_files = ["CLAUDE.md", ".env", ".env.local", ".claude/settings.local.json"]   # G 四项都被 ignore；盘上一项都没有
install_inputs = ["package.json", "bun.lock"]   # G bun.lock 已跟踪
install = "bun install --frozen-lockfile"  # R [未核]
checks = ["bunx next typegen", "bunx tsc --noEmit --incremental false"]   # P 同兄弟 kiosk（无自己的 CLAUDE.md 可读）；B 0 错 [已核]
publish = { branch = "main", push = true, confirm = false }   # D§6.7 默认
deploy_on_push = false                     # I 无 webhook
pin_unpushed = true

[repo.patient_9_grid]
path = "/Volumes/GWD/inter/patient_9_grid"
default_branch = "main"                    # G；C "Commit directly to main"
deps_dirs = ["node_modules"]               # G
seed_caches = ["src-tauri/target", ".next/cache", "node_modules/.cache"]   # D§4；G 前两个在盘上
seed_files = ["CLAUDE.md", ".env", ".env.local", ".claude/settings.local.json"]   # G 四项都被 ignore；CLAUDE.md 在盘上
install_inputs = ["package.json", "bun.lock"]   # G；C "bun.lock is intentionally committed"
install = "bun install --frozen-lockfile"  # R [未核]
checks = ["bunx next typegen", "bunx tsc --noEmit --incremental false"]   # C "Type check: bunx tsc --noEmit"；B 0 错 [已核]
publish = { branch = "main", push = true, confirm = false }   # D§6.7 默认
deploy_on_push = false                     # I 无 webhook
pin_unpushed = true

[repo.maxident-sync]
path = "/Volumes/GWD/inter/maxident-sync"
default_branch = "main"                    # G；C "Commit directly to main"
deps_dirs = ["node_modules"]               # G
seed_caches = ["src-tauri/target", ".next/cache", "node_modules/.cache"]   # D§4；G 前两个在盘上
seed_files = [".env", ".env.local", ".claude/settings.local.json", "src-tauri/src/secrets.rs"]   # G CLAUDE.md 已跟踪；secrets.rs 被 ignore 且编译必需（C）
install_inputs = ["package.json", "bun.lock"]   # G；C "bun.lock is intentionally tracked"
install = "bun install --frozen-lockfile"  # R [未核]
checks = ["bunx next typegen", "bunx tsc --noEmit --incremental false"]   # C "bun run lint — tsc --noEmit"；B 0 错 [已核]。cargo check/clippy 不列：需要 secrets.rs 与 src-tauri/target（§2.3 checks 第 6 条）
publish = { branch = "main", push = true, confirm = false }   # D§6.7 默认
deploy_on_push = false                     # I 无 webhook；C "Releases are publish-server-only"
pin_unpushed = true

[repo.insurance-check]
path = "/Volumes/GWD/inter/insurance-check"
default_branch = "main"                    # G
deps_dirs = ["node_modules", "sidecar/node_modules"]   # G .gitignore:4,81；P postinstall = vendor-cloakbrowser.mjs --ensure，它会装 sidecar
seed_caches = ["src-tauri/target", ".next/cache", "node_modules/.cache"]   # D§4；G 前两个在盘上
seed_files = [".env", ".env.local", ".claude/settings.local.json", "src-tauri/src/secrets.rs"]   # G CLAUDE.md 已跟踪；secrets.rs 被 ignore 且编译必需（C "Build prerequisite"）
install_inputs = ["package.json", "bun.lock", "sidecar/package.json"]   # G 均已跟踪；sidecar/package-lock.json 被 ignore，不列
install = "bun install --frozen-lockfile"  # C 释放配方（注释 "postinstall vendors cloakbrowser"）[未核]
checks = ["bunx next typegen", "bunx tsc --noEmit --incremental false"]   # C "bun run lint — tsc --noEmit"；B 0 错 [已核]。cargo 不列，理由同 maxident-sync
publish = { branch = "main", push = true, confirm = false }   # D§6.7 默认
deploy_on_push = false                     # I 无 webhook；C "it does NOT touch git — commit the version bump and push with glab yourself"
pin_unpushed = true
```

### A.4 release-notes 与 `@inter/*` 库

**release-notes** 只有 Node 脚本，没有依赖、没有测试（`G`、`P`）。front 构建时才把它 vendored 进去（`C` front），推送它本身不部署，所以 `deploy_on_push = false`。

**七个 `@inter/*` 库**：`deploy_on_push = true`，由任务清单与 design §3、§11 规定。`I` 的 webhook 表里 hr-core、hr-tasks、hr-time-management、quick-start、hr-components 推送都会重建 front；**admin-shared 与 admin-time-off 没有 webhook**（`I`；`infra-docs/01-overview.md:200` 称它们是"Libraries, developer tooling and build-time dependencies"，front 的 `package.json` 也不依赖它们），这里按 design 设为 `true`，只是多一道确认。

库都是 peer 依赖，多数在主 checkout 里没有 `node_modules`，所以 `deps_dirs = []`；hr-core、hr-tasks、hr-components、hr-time-management、quick-start 都声明了 devDependencies（admin-shared、admin-time-off 没有），但盘上只有 hr-time-management 有 `node_modules`；两个库配了 `deps_dirs = ["node_modules"]`（两者的 `bun.lock` 都被 ignore，所以输入只有 `package.json`）：hr-time-management 因为盘上本来就有 `node_modules`、可以认领；hr-core 因为它的测试要装依赖才绿。其余库的检查不装依赖就绿，不配依赖目录。检查：hr-components、admin-time-off、hr-time-management 的 `bun test` 不装依赖就能全绿（`B`）；hr-core 的 `bun test src` 不装依赖时红（`src/api/client.ts` 引用 peer 依赖 `@tanstack/react-query`），`bun install` 之后绿，所以配依赖目录并启用（`B`）；admin-shared、hr-tasks、quick-start 没有测试。

```toml
[repo.release-notes]
path = "/Volumes/GWD/inter/release-notes"
default_branch = "main"                    # G
deps_dirs = []                             # P 没有依赖
seed_caches = []
seed_files = [".claude/settings.local.json"]   # G 默认四项里只有它被 ignore（.env 等未被 ignore，列了会以 Untracked 进 overlay）
install_inputs = []
checks = []                                # P 只有 serve/draft/pages/prune/publish 脚本，没有测试
publish = { branch = "main", push = true, confirm = false }   # D§6.7 默认
deploy_on_push = false                     # I 不在 webhook 表里
pin_unpushed = true

[repo.admin-time-off]
path = "/Volumes/GWD/inter/admin-time-off"
default_branch = "main"                    # G
deps_dirs = []                             # P 只有 peerDependencies
seed_caches = []
seed_files = [".claude/settings.local.json"]   # G
install_inputs = []
checks = ["bun test"]                      # G tests/ 有 4 个测试文件；B 13 个测试通过，无需安装 [已核]
publish = { branch = "main", push = true, confirm = true }   # D§3 库一推就部署，需确认
deploy_on_push = true                      # 任务清单与 D§3、§11；I 这个库没有 webhook（见 A.4 说明），保守设 true
pin_unpushed = true

[repo.admin-shared]
path = "/Volumes/GWD/inter/admin-shared"
default_branch = "main"                    # G
deps_dirs = []                             # P 只有 peerDependencies
seed_caches = []
seed_files = [".claude/settings.local.json"]   # G
install_inputs = []
checks = []                                # G 没有测试、没有脚本
publish = { branch = "main", push = true, confirm = true }   # D§3
deploy_on_push = true                      # 任务清单与 D§3、§11；I 这个库没有 webhook（见 A.4 说明），保守设 true
pin_unpushed = true

[repo.hr-core]
path = "/Volumes/GWD/inter/hr-core"
default_branch = "main"                    # G
deps_dirs = ["node_modules"]               # P devDependencies 3 项（@types/react、@types/react-dom、typescript）+ peerDependencies（bun 默认一并安装，测试要的 @tanstack/react-query 在其中）；盘上没有 node_modules，第一个会话按 install 冷装（B：2.7 s）
seed_caches = []
seed_files = [".claude/settings.local.json"]   # G
install_inputs = ["package.json"]          # G bun.lock 被 ignore（.gitignore:3），没有被跟踪的锁文件（同 hr-time-management）
install = "bun install"                    # B 在导出里装过一次：2.7 s
checks = ["bun test src"]                  # P scripts.test；B 不装依赖 22 通过 / 1 失败 / 1 错误，装依赖后 34 通过 / 0 失败 [已核]
publish = { branch = "main", push = true, confirm = true }   # D§3
deploy_on_push = true                      # D§3；I "Staff Front" webhook
pin_unpushed = true

[repo.hr-tasks]
path = "/Volumes/GWD/inter/hr-tasks"
default_branch = "main"                    # G
deps_dirs = []                             # P 有 dependencies/devDependencies，但盘上没有 node_modules，bunfig.toml 还关了 peer 安装
seed_caches = []
seed_files = [".claude/settings.local.json"]   # G
install_inputs = []
checks = []                                # G 没有测试、没有脚本
publish = { branch = "main", push = true, confirm = true }   # D§3
deploy_on_push = true                      # D§3；I "Staff Front" webhook
pin_unpushed = true

[repo.hr-components]
path = "/Volumes/GWD/inter/hr-components"
default_branch = "main"                    # G
deps_dirs = []                             # P 有 dependencies(dompurify)，但盘上没有 node_modules
seed_caches = []
seed_files = [".claude/settings.local.json"]   # G
install_inputs = []
checks = ["bun test"]                      # G 5 个测试文件；B 43 个测试通过，无需安装 [已核]
publish = { branch = "main", push = true, confirm = true }   # D§3
deploy_on_push = true                      # D§3；I 两个 webhook（gchat 与 chat）都会触发 "Staff Front"
pin_unpushed = true

[repo.hr-time-management]
path = "/Volumes/GWD/inter/hr-time-management"
default_branch = "main"                    # G
deps_dirs = ["node_modules"]               # G 盘上有 node_modules（.gitignore:1）；P devDependencies
seed_caches = []
seed_files = [".claude/settings.local.json"]   # G
install_inputs = ["package.json"]          # G bun.lock 被 ignore（.gitignore:3）
install = "bun install"                    # 无被跟踪的锁文件 [未核]
checks = ["bun test"]                      # G 2 个测试文件；B 36 个测试通过 [已核]
publish = { branch = "main", push = true, confirm = true }   # D§3
deploy_on_push = true                      # D§3；I "Staff Front" webhook
pin_unpushed = true

[repo.quick-start]
path = "/Volumes/GWD/inter/quick-start"
default_branch = "main"                    # G
deps_dirs = []                             # P 只有 peerDependencies 与 @types/react
seed_caches = []
seed_files = [".claude/settings.local.json"]   # G
install_inputs = []
checks = []                                # G 没有测试、没有脚本
publish = { branch = "main", push = true, confirm = true }   # D§3
deploy_on_push = true                      # D§3；I "Staff Front" webhook
pin_unpushed = true
```

### A.5 后端与 Rust：staff-api、rust-socket、rust-mail、gwd、sockdbg

**staff-api**：分支是 `master`，推它就是生产部署（`C`、`I`）。`composer.lock` 虽然还留在 `.gitignore` 里，但自 2026-09-15 起重新被跟踪（`C`、`G`）。依赖目录只有 `vendor`；`node_modules` 是空的（`package.json` 没有依赖）。`runtime/` 是 Hyperf 的代理与扫描缓存，源码一改就必须重建（`C` "`rm -rf runtime/container` after every `app/` edit"），所以不能当种子缓存。`.env` 是**生产库配置**（`C`），design §3 把它列为必需的种子文件。`.env.local` 在这个仓库里没被 ignore，不列。检查只有 phpstan：`php-cs-fixer` 整仓检查在基线上是红的（4537 个文件里 135 个需要修，`B`），`C` 的做法是对改动的文件逐个 `cs-fix`，要表达这个需要"批次范围的检查"，目前的配置格式没有；测试不列（`C`：`.env` 指向生产库，一部分测试会写生产 Redis 和发真实推送）。`composer analyse` 声明的 `--memory-limit 300M` 在整仓运行里会 OOM（`C`），所以用 `C` 给出的 2G 形式（`B`：2G 形式 51 s 通过）。

**rust-socket、rust-mail、gwd、sockdbg**：纯 Rust，依赖在 `~/.cargo`，没有依赖目录；`target` 是种子缓存。检查按"仓库文档点名的命令的只读形式，并且在干净导出里是绿的"取：rust-socket 点名 `cargo test`（`C`）以及 clippy、fmt（`R`）；rust-mail 只点名 `cargo test`（`C`），它的 clippy（1 处 `large_enum_variant`）与 `cargo fmt --check`（8 个文件）在基线上是红的（`B`）；gwd 点名 build、test、`cargo clippy --all-targets -- -D warnings`（`C`）；sockdbg 点名 clippy 与 fmt "keep both clean"、没有测试（`C`）。四个仓库推送都不部署（`C`，`I`）。

```toml
[repo.staff-api]
path = "/Volumes/GWD/inter/staff-api"
default_branch = "master"                  # G origin/HEAD=origin/master；C "Mainline is master"
deps_dirs = ["vendor"]                     # G .gitignore vendor/
seed_caches = []                           # runtime/ 不能当缓存种子（见上）
seed_files = ["CLAUDE.md", ".env", ".claude/settings.local.json"]   # G 三项被 ignore；.env 是生产库（C）；D§3；AGENTS.md 已跟踪
install_inputs = ["composer.json", "composer.lock"]   # G composer.lock 已跟踪（C "TRACKED again as of 2026-09-15"）
install = "composer install --no-interaction"   # R/C `composer install`；--no-interaction 因为 daemon 没有 TTY [未核]
checks = ["/opt/homebrew/bin/php -d memory_limit=2G vendor/bin/phpstan analyse -c phpstan.neon.dist --no-progress"]   # C 的作用域形式去掉文件参数；B [已核] 51 s，No errors
                                           # php-cs-fixer check 整仓 [红]（B：135/4537 个文件待修），未启用
publish = { branch = "master", push = false, confirm = false }  # D§6.7 "预期配置是 push=false + 人工 strata push"；confirm=false：2026-10-26 peilin 要求 staff-api 的 publish/push 无需确认（无 TTY、无 Touch ID）
deploy_on_push = true                      # D§3；I "Staff API Source" webhook；C "a push to master IS a production deploy"
pin_unpushed = true

[repo.rust-socket]
path = "/Volumes/GWD/inter/rust-socket"
default_branch = "main"                    # G
deps_dirs = []                             # 纯 Rust：依赖在 ~/.cargo
seed_caches = ["target"]                   # G 在盘上；build.sh 结尾 rm -rf target（C），所以主 checkout 的 target 可能是冷的
seed_files = [".claude/settings.local.json"]   # G CLAUDE.md、AGENTS.md 已跟踪；.env 未被 ignore
install_inputs = []
checks = ["cargo clippy --all-targets -- -D warnings", "cargo fmt --check", "cargo test"]   # C "default cargo test is offline and safe"；R 的 Rust 命令表只说 cargo clippy / cargo fmt，`--all-targets -- -D warnings` 是本文加的 [我的决定]；B 三条全绿 [已核]
publish = { branch = "main", push = true, confirm = false }   # D§6.7 默认
deploy_on_push = false                     # C "a push to main deploys nothing (the gchat webhook auto-builds staff-api and front only)"
pin_unpushed = true

[repo.rust-mail]
path = "/Volumes/GWD/inter/rust-mail"
default_branch = "main"                    # G
deps_dirs = []                             # 纯 Rust
seed_caches = ["target"]                   # G 在盘上
seed_files = [".env", ".claude/settings.local.json"]   # G 两项被 ignore；CLAUDE.md 已跟踪。运行配置只在服务器的 /server/rust-mail.env（C），本地 .env 用不上，列着无害
install_inputs = []
checks = ["cargo test"]                    # C "cargo test is offline"；B 绿 [已核]。clippy -D warnings 与 fmt --check [红]（B）
publish = { branch = "main", push = true, confirm = false }   # D§6.7 默认
deploy_on_push = false                     # C "a push deploys nothing"
pin_unpushed = true

[repo.gwd]
path = "/Volumes/GWD/inter/gwd"
default_branch = "main"                    # G
deps_dirs = []                             # 纯 Rust
seed_caches = ["target"]                   # G 在盘上
seed_files = [".claude/settings.local.json"]   # G CLAUDE.md、AGENTS.md 已跟踪
install_inputs = []
checks = ["cargo clippy --all-targets -- -D warnings", "cargo test"]   # C "Build & Verify"；B 两条绿 [已核]。测试不连生产 API（C "Tests cover CLI parsing, config I/O, and output formatting only"）
publish = { branch = "main", push = true, confirm = false }   # D§6.7 默认
deploy_on_push = false                     # C "No CI, and no deploy"
pin_unpushed = true

[repo.sockdbg]
path = "/Volumes/GWD/inter/sockdbg"
default_branch = "main"                    # G
deps_dirs = []                             # 纯 Rust
seed_caches = ["target"]                   # G 在盘上
seed_files = [".claude/settings.local.json"]   # G CLAUDE.md 已跟踪
install_inputs = []
checks = ["cargo clippy --all-targets -- -D warnings", "cargo fmt --check"]   # C "cargo clippy / cargo fmt — keep both clean"，没有测试；`--all-targets -- -D warnings` 是本文加的 [我的决定]；B 两条绿 [已核]
publish = { branch = "main", push = true, confirm = false }   # D§6.7 默认
deploy_on_push = false                     # C "Nothing ships on push"
pin_unpushed = true
```

---

## 附录 B. 检查命令的实测记录（2026-10-22）

方法：对各仓库做 `git archive <ref> | tar -x` 得到干净导出（没有 `.git`、没有被 ignore 的文件），放在 scratchpad 里；`node_modules` 取主 checkout 的只读软链或整份拷贝；Rust 用独立的 `CARGO_TARGET_DIR` 并加 `--offline`；PHP 拷贝 `vendor/`。导出、安装、运行检查全在 scratchpad 里，对 `/Volumes/GWD/inter` 只跑了只读的 git 命令（`ls-files`、`check-ignore`、`archive`、`show`、`rev-parse`）和读文件；跑完后检查过各仓库的 `node_modules` 没有新文件，并把导出与导出所用的提交逐文件比对：typegen、tsc、prettier `--check`、`lint-antd-deprecated`、两个 dev-link（admin 的在 git 工作树里）都没有改动任何已跟踪文件，唯一的例外是下表里 admin 的 `./dev-link.sh` 在没有 `.git` 的导出里删掉了 `bun.lock`。导出所用的提交：front 取本地 HEAD = `origin/main` = `7e64cb2`，admin 取 HEAD `83128f93`，tool 取 HEAD `66d0d343`（落后 `origin/main` 11 个提交），staff-api 取 `origin/master` = `407a74f4b`，其余取当时的本地 HEAD。

| 仓库 | 命令 | 结果 |
| --- | --- | --- |
| admin | `tsc --noEmit --incremental false`（不跑 typegen） | 红：334 个错（`*.module.css` 等声明缺失、`node:test` 缺类型） |
| admin | `next typegen` → 同上 `tsc` | 绿：0 错。`typegen` 1.6 s，只生成 `next-env.d.ts` 与 `dist/types/*`；`AGENTS.md`、`CLAUDE.md` 等已跟踪文件未被改动 |
| tool | `tsc`（不跑 typegen） | 红：226 个错（同上原因） |
| tool | `next typegen` → `tsc` | 5 个错，全是陈旧镜像：`node_modules/@inter/hr-tasks` 缺 `MentionMember`、`RichTextContent`，而兄弟 hr-tasks 的 `b710e52` 已导出它们 |
| tool | `bun dev-link.mjs --sync-only`（7 个兄弟库在同级目录）→ `next typegen` → `tsc` | 绿：0 错。在布局根（导出的上一级）新建了 `node_modules/tailwindcss`，指向本工作区自己的 `node_modules/tailwindcss` 的绝对路径软链；已跟踪文件未被改动 |
| tool | `bun run lint-antd-deprecated` | 绿；已跟踪文件未被改动（陈旧镜像与刷新后的镜像下都是） |
| admin | `./dev-link.sh`（`admin-shared`、`admin-time-off` 在同级目录），导出**没有 `.git`** | **删掉了被跟踪的 `bun.lock`**（输出 "Removing stale untracked bun.lock"） |
| admin | 同上，导出是 git 工作树且 `bun.lock` 已提交 | `bun.lock` 保留；随后 `next typegen` → `tsc` 0 错 |
| front | `tsc`（带与不带 `next-env.d.ts`） | 红：5 个错，与 typegen 无关：`components/pre-appointment/FormProgress.tsx:67-70`、`utils/polyfills/readableStreamAsyncIterator.ts:44` |
| front | `package-lock.json` | `@inter/hr-components`、`hr-core`、`hr-tasks`、`hr-time-management`、`quick-start` 五项都带 `git+https://…#<sha>`（只核对了存在；hr-core、hr-tasks、quick-start 三项的 SHA 与盘上被 ignore 的 `bun.lock` 不同——bun 实际解析用的是 `bun.lock`） |
| slate_viewer | `next typegen` → `tsc`；`bun run test` | 绿：0 错；272 个测试通过（8 s） |
| appointment_patient_viewer、patient_9_grid、surgical-timeout、maxident-sync、insurance-check | `next typegen` → `tsc` | 各绿：0 错。不跑 typegen 也是 0 错；仍统一先跑，因为它们的 `tsconfig.json` 同样 `include` 了 `next-env.d.ts`，一旦出现依赖生成类型的写法就会变红 |
| surgical-timeout | `bunx prettier --check .` | 绿（5 s） |
| gwd | `cargo clippy --all-targets -- -D warnings` / `cargo fmt --check` / `cargo test` | 绿 / 绿 / 绿（43 s / 0 s / 36 s） |
| sockdbg | 同上 | 绿 / 绿 / 绿（13 s / 0 s / 11 s；无测试） |
| rust-socket | 同上 | 绿 / 绿 / 绿（28 s / 0 s / 29 s） |
| rust-mail | 同上 | **红**（1 处 `clippy::large_enum_variant`）/ **红**（8 个文件格式不符）/ 绿（28 s） |
| staff-api | `php vendor/bin/php-cs-fixer check --diff` | **红**：退出码 8，4537 个文件里 135 个可修（98 s） |
| staff-api | `php -d memory_limit=2G vendor/bin/phpstan analyse -c phpstan.neon.dist --no-progress` | 绿：`[OK] No errors`（51 s）。随后同一份导出里 `--memory-limit 300M` 也是 `No errors`（1 s，命中结果缓存，所以不能证明 300M 够用） |
| hr-components、admin-time-off、hr-time-management | `bun test`（不装依赖） | 绿：43、13、36 个测试，均 <1 s |
| hr-core | `bun test src` | 不装依赖 **红**：22 通过 / 1 失败 / 1 错误（`src/api/client.ts` 引用 peer 依赖 `@tanstack/react-query`）；`bun install`（2.7 s）后 **绿**：34 通过 / 0 失败，所以配 `deps_dirs` 并启用 |
| 全部 Next 应用 | `tsconfig.json` 里的 `incremental` | admin、front、tool、slate_viewer、surgical-timeout、appointment_patient_viewer、patient_9_grid、maxident-sync、insurance-check 都是 `true` |

没有执行的：全部 `install` 命令（会冷装；hr-core 的 `bun install` 除外，为核对测试在导出里装过一次）、staff-api 的测试（连生产库）、各 Tauri 应用的 Rust 检查。
