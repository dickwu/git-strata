# cli

**Crate**：`crates/git-strata`（bin `git-strata`）· **设计来源**：design.md §5、§6、§8、§9 不变量 3、6、7 · **邻居版本**：daemon 方法表以本文冻结的名字为准（T13 可并行改写，不跟它的草稿改名），confirm.md @229f4e3，types.md 的 JSON 约定与闭集 `Code`，config.md 的 `needs_human_confirm`

> 本文按 module-rulings.md 写：「T14 cli」，以及标了 T14 或 Everyone 的增补。后写的裁定压过本文初稿里冲突的句子，冲突记在 §9。`require_tty` 在这里，不在 daemon。每个子命令对应 daemon 的一个方法，方法名不改。成功与失败都是一行 JSON：`{ok, data, error}`。`data` 总是有，空时是 `{}`。零命中的 grep 是空列表。确认类命令在非 TTY 上、在连接 daemon 之前拒绝。没有环境变量、没有 flag 能跳过确认。cli 不发明 `Code`。

## 1. 职责与边界

cli 是人与钩子的入口。它不持有 repo 锁，不进会话队列，不弹 LocalAuthentication 框（那是 daemon 经 confirm 做的），不签发、不校验 `ConfirmToken`。它做的是：解析参数、检查 TTY、把人要看的计划打到 stderr、把参数收成一次 RPC、把回答打成一行 JSON。

- **做**：§3 的子命令；`git strata` 与 `git-strata` 是同一个二进制（D1）；Claude Code 的 `PostToolUse` / `SessionStart` 命令行；daemon 没在跑时的只读路径。
- **不做**：不调用 `strata-confirm`。不自己 `Command::new("git")`（要 git 的只读路径走 gitio 已经规定的函数）。不新增错误码。不提供 `--human`、`--yes`、`--force` 或任何“跳过确认”的 flag（design §11 第 5 条：v1 只有 JSON；不变量 7）。不自动批准被取代的提议，不自动处理 Rewound / Diverged 的发布链，不在没有人选的情况下处理失败的移动。

输出风格借用 `gwd`：机器只读这一行 JSON；退出码只有 0 和 1；分支只看 `error.code`，不看 `message`。给人看的提示只写 stderr，不进 JSON（types.md §7）。

daemon 没在跑时，只读命令直接读 store 与工作区。写命令失败并说明要先 `git-strata daemon start`。

## 2. 依赖

| 模块 | 用到的 |
| --- | --- |
| daemon | lib 里的 RPC 类型。方法名见 §3，一一对应。cli 依赖 `strata-daemon` 的 lib，不依赖 bin |
| types | `Code`、`StrataError`、`Action`、`SessionId`、`RepoName`、`RepoPath`、`CheckpointId`、`ProposalId`、`Checkpoint`、`PushOutcome`、`SessionManifest`。信封见 §4 |
| confirm @229f4e3 | 不调用 `require`。只在确认类命令上先 `require_tty`。等人的上限要大于 confirm 的 `PROMPT_TIMEOUT + CANCEL_GRACE`（60 s + 5 s） |
| session | 本地只读路径打开 `Sessions`（session.md：cli 只在 daemon 没在跑时自己打开）。`resolve`、`select`、`status`、`resolve_failed_move` 的结果类型 |
| deps | `open_proposals`、`Proposal`。批准时展示的就是这条 `Proposal`，不另造一种计划 |
| search | `grep`、`files`、`Hit`。零命中是空 `Vec` |
| publish | `diff_between`、`PushOutcome`。链的判定不在 cli 重写 |
| config | `RepoConfig::needs_human_confirm`（`publish.confirm \|\| deploy_on_push`；[实现时补的决定]（2026-10-26，peilin："将api 也改为不用确认"）改为只看 `publish.confirm`，`Pins::needs_confirm` 取钉住的 `confirm` 与文件的较严者）。读 `~/Library/Application Support/git-strata/config.toml` |

crate 依赖到 `strata-daemon` 的 lib 为止，不含 `strata-confirm`（types.md §8）。外部 crate：`clap`、`serde_json`、`libc`（`isatty`）。

**上游要求**（方法表里还没有、T14 的后写裁定要求有命令。名字若 daemon 已定成别的，改本文，不改行为）：

1. `resolve_move`：包装 `session::resolve_failed_move`。人在 status 里看过现场之后选 Abandon 或 Accept，没有选择就不运行（「Resolving a failed move」）。
2. `chain_resolve`：包装 publish 对 Rewound / Diverged 的显式处理。`--accept-origin replay` 与 `--accept-origin drop` 不推送，不取凭据。`--restore` 才推：只在 `is_ancestor(o, c)` 时是快进，否则在任何 push 之前以已有的 `InvalidInput` 拒绝，不加码；仓库要确认时用已有的 `PushDeploy`，不新加 `Action` 变体。gitio 从不强推。推送与链/锚点事务在 repo 写锁里，由 daemon 拿锁，cli 不拿（「Every update of a ref main_tip reads runs under the repo lock」）。
3. 钉住的 `deploy_on_push` / `publish.confirm` / `publish.push` 写在配置目录里的哪个文件，由 daemon.md 点名。cli 只读它，用来在连接之前决定要不要 `require_tty`。

调用方：人的终端，以及 Claude Code 钩子。没有别的 crate 调用 cli。

## 3. 公开接口（Rust）

```rust
/// stdin 是终端才返回 Ok。否则 Code::NotTty，message 说明这条命令要在终端里跑。
/// 不读环境变量，不看 flag。失败时还没有连接 socket。
pub fn require_tty() -> Result<(), StrataError>;
```

子命令与冻结的方法名：

| 命令 | daemon 方法 | TTY | 确认 |
| --- | --- | --- | --- |
| `git-strata new` | `new` | 否 | 否 |
| `git-strata add` | `add` | 否 | 否 |
| `git-strata status` | `status` | 否 | 否 |
| `git-strata checkpoint` | `checkpoint` | 否 | 否 |
| `git-strata restore` | `restore` | 否 | 否 |
| `git-strata grep` | `grep` | 否 | 否 |
| `git-strata files` | `files` | 否 | 否 |
| `git-strata diff` | `diff` | 否 | 否 |
| `git-strata deps propose` | `deps_propose` | 否 | 否 |
| `git-strata deps approve` | `deps_approve` | 是 | `DepsApprove` |
| `git-strata deps list` | `deps_list` | 否 | 否 |
| `git-strata publish` | `publish`（先 `checkpoint`；批次里有要推且要确认的仓库时再 `publish_plan`） | 是，当这次还要提交的仓库里有要推且钉住的值要确认的（review-2 N2） | 每个这样的仓库一次 `PushDeploy`，在该仓库 `commit_tree` 之前。不推的仓库、已经落地的仓库不确认 |
| `git-strata push` | `push` | 是，当仓库要确认 | `PushDeploy` |
| `git-strata assign` | `assign` | 否 | 否 |
| `git-strata rm` | `rm` | `--discard` 时是 | `RemoveDiscard` |
| `git-strata gc` | `gc` | 否 | 否 |
| `git-strata du [--refresh]` | `du` | 否 | 否 |
| `git-strata daemon start` | — | 否 | 否 |
| `git-strata daemon stop` | — | 否 | 否 |
| `git-strata daemon status` | `daemon_status` | 否 | 否 |
| `git-strata resolve` | `resolve_move`（上游要求 1） | 是（D18） | 否。没有 `Action` 变体，不弹框 |
| `git-strata chain` | `chain_resolve`（上游要求 2） | 选择本身要 TTY（D18）。不要确认的仓库分叉时，`push` 自己无头重放（publish.md D2a），`chain` 本身不变。`--restore` 且仓库要确认时，连接前再按 `push` 的钉住值查一次 | `--restore` 且要确认：`PushDeploy`。`--accept-origin replay` / `drop`：不取凭据 |
| `git-strata config relax` | `config_relax` | 是 | `ConfigRelax`。`require_tty` 之后连接并弹框，与其他确认命令相同。不因文件变化而跑，也不处理收紧 |

`git-strata daemon start` 用 `launchctl bootstrap gui/<uid> ~/Library/LaunchAgents/com.gwd.git-stratad.plist` 装上 `com.gwd.git-stratad`，不等待业务，不连接 socket。`stop` 用 `launchctl bootout gui/<uid>/com.gwd.git-stratad`，也不连接 socket。plist 不存在 → `InvalidInput`，`detail.reason = "no_plist"`。已经装上或已经卸下 → 成功，`data.already = true`，不是错误（D15、D16）。[实现时补的决定][我的决定 D24] `stop` 在 `bootout` 成功之后再用 `launchctl print` 轮询，直到 launchd 不再列出这个服务才返回（每 100 ms 一次，最多 10 s；超时 → `Unknown`，`detail = {reason: "launchctl", step: "bootout_wait"}`）。真实安装里 `bootout` 返回时服务还在列表里，紧接着的 `start` 回答 `already` 却什么都没起来。

`git-strata publish --assign <file> <session>` 与 `git-strata assign` 是同一次 `assign` 调用，不是第二套实现（D13）。

### 3.1 参数

选择器：`--session` 接受 `SessionId` 或 ticket。对不上 → 透传 `NoSuchSession`。ticket 对上不止一个会话 → 透传 `AmbiguousMatch`，`detail` 列出候选 id。不传时用 cwd：`session::resolve`；对不上任何工作区，而这条命令又需要会话 → `NoSuchSession`。`--all` 与 `--session` 同时给 → `InvalidInput`，`detail.reason = "both_selectors"`。

| 命令 | 参数 | 不合法 |
| --- | --- | --- |
| `git-strata new` | `--repos a,b`（至少一个 `RepoName`）。ticket 用 `--ticket T` 或第一个位置参数，二者给了同一个字段；都不给则没有标签。ticket 只是标签，不进路径。`--repos` 只选 tracked，每个配置的 repo 都建工作区；cli 在 stderr 提示一行 `note: --repos tracks …`（D35） | 空 `--repos`；ticket 形如 `SessionId` 由 session 返回 `ReservedName`，cli 透传 |
| `git-strata new`（精简） | `--repo R`，可重复或逗号分隔：只有这些 repo 建工作区，全部 tracked（session.md D51；daemon `new` 的 `only = true`）。ticket 同上 | 与 `--repos` 同给 → `InvalidInput`，`detail.reason = "repo_flags"`；两个都不给 → `InvalidInput` |
| `git-strata add` | `--repo R`（至少一个，可重复或逗号分隔）`[--session S]`，不给 = cwd 的会话（session.md D52） | 没有 `--repo` → `InvalidInput`（`repos`）；未知 repo 透传 `NoSuchRepo`；会话不是 `Active` 透传 session 的码 |
| `git-strata status` | `[--all]` 或 `[--session S]`。都不给 = cwd 的会话。`[--brief]`：只读 store 的摘要（§3.2，D36） | — |
| `git-strata ls` | `[--table]`：成功时 stdout 只有表格（D36，对 D19 的修订） | — |
| `git-strata checkpoint` | `--tool-call [ID]` 与 `-m MSG` 二选一；`ID` 省略或为空时取钩子 stdin JSON 的 `tool_use_id`（§3.5，D22）。`[--session S]`。`--path P` 可重复，P 是工作区里的路径，cli 收成相对该 repo 根的 `RepoPath` | 两个都给，或两个都不给 → `InvalidInput`，`detail.reason = "checkpoint_reason"` |
| `git-strata restore` | `<checkpoint>`（十进制 `CheckpointId`，超过 `i64::MAX` → `InvalidInput`）`[paths…]` `[--session S]` | 非数字 id → `InvalidInput` |
| `git-strata grep` | `<pattern>`。`[--session S]` 或 `[--all]`。都不给 = cwd 的会话（design §6.3；压过初稿“不给就是全部”，见 §9）。没有 `--repo`，没有 type / encoding / glob 之类的 tgrep 选项（T11） | 空 pattern → `InvalidInput` |
| `git-strata files` | 可选的一个位置参数 glob，交给 search 的 `files`；不给 = 该视图全部文件（D11）。选择器同 grep。没有别的 flag | — |
| `git-strata diff` | 三选一：`--base`（缺省，cwd 或 `--session` 相对它钉住的基线）、`--session S`（该会话相对它的基线）、`--between A B`（两个选择器）。`--repo R` 在 `--between` 时必填；否则用 cwd 的 repo。`--session S` 既没有 `--repo` 也不在某个 repo 的工作区里时，cli 扫该会话的全部 repo（8 个线程），取有内容改动或有 checkpoint 的那一个；没有一个合格 → `repo: null`、三个空数组；多于一个 → `InvalidInput`，`detail.reason = "repo"`，`detail.candidates` 列出候选 [实现时补的决定，2026-10-27] | 两种模式同时给 → `InvalidInput`，`detail.reason = "diff_mode"` |
| `git-strata deps propose` | `[--session S] [--repo R] [--switch]`。repo 缺省 = cwd 的 repo。`--switch`：本会话（只有提议者）立刻换到提议的依赖集，不确认（design §6.4 第 3 步、deps §5.10 `plan_switch`）；别的会话不动 [实现时补的决定，review M3] | — |
| `git-strata deps approve` | `<id>`（`ProposalId`）。`[--sessions S,S]`，不给 = 该 repo 的全部会话，由 daemon 展开 | 非数字或超过 `i64::MAX` → `InvalidInput` |
| `git-strata deps list` | `[--repo R]`。不给 = 全部 repo 的未关闭提议 | — |
| `git-strata publish` | `[--from S:path]` 或 `[--from S:repo:path]` 可重复。path 是 `RepoPath`（D31）。发布的是这些路径当下的内容：cli 先对每个批次记一条 `BeforePublish` checkpoint，再把它的号交给 `publish`（review L1）；没有选旧 checkpoint 的参数。点到 `Renamed` 的任一半由 publish 选中整个改名，cli 不拆开。一个都不给且 stdin 是 TTY：交互选择（§5.4）。不给且不是 TTY → `InvalidInput`，`detail.reason = "no_batch"` | `S:path` 缺冒号，或 path 不是 `RepoPath` → `InvalidInput` |
| `git-strata push` | `<repo>` | 未知 repo 透传 `NoSuchRepo` |
| `git-strata assign` | `--file PATH --session S`。PATH 是 `RepoPath`。不需要确认（design §6.9） | 缺一 → `InvalidInput` |
| `git-strata rm` | `--session S` `[--discard]`。`--discard` 会丢掉未发布改动，所以要确认 | 不给会话 → `InvalidInput` |
| `git-strata gc` | 无 | — |
| `git-strata du` | `--refresh` | 缺省 false：只返回缓存，过期的单元在后台重算；true：后台把全部单元重算一遍，立即返回 |
| `git-strata resolve` | `--session S --repo R`，以及 `--abandon` 与 `--accept` 恰好一个。没有缺省 | 缺选择、或两个都给 → `InvalidInput`，`detail.reason = "no_choice"` 或 `"both_choices"`。不连接 |
| `git-strata chain` | `--repo R`，以及恰好一个：`--accept-origin replay`、`--accept-origin drop`、`--restore`。没有缺省。`--restore` 在 `!is_ancestor(o, c)` 时不连接、不推 | 缺选择、或两个都给 → `InvalidInput`，`detail.reason = "no_choice"` 或 `"both_choices"`。不连接。不能快进 → `InvalidInput`，`detail.reason = "not_ancestor"`，在任何 push 之前 |
| `git-strata config relax` | `<repo>` | 未知 repo 透传 `NoSuchRepo`。文件变了不会自己跑这条命令 |

未知子命令 → `InvalidInput`，`error.detail.commands` 是已知的第一级名字，按字母序：`add`、`assign`、`chain`、`checkpoint`、`config`、`daemon`、`deps`、`diff`、`du`、`files`、`gc`、`grep`、`ls`、`new`、`publish`、`push`、`resolve`、`restore`、`rm`、`status`。`git-strata deps`、`git-strata daemon` 或 `git-strata config` 后面没有第二级 → 同样 `InvalidInput`，`detail.commands` 只列那一组：`propose` / `approve` / `list`，`start` / `stop` / `status`，或 `relax`。

`-h` / `--help` 与 `--version` 走 clap 的文本，打在 stdout，退出码 0。它们不是子命令，不套信封（D19）。另一个例外：`ls --table` 成功时 stdout 只有表格（D36）；出错时照常是完整错误信封、退出码非 0。除此之外，stdout 上只有信封。

### 3.2 每个子命令的 `data`

类型按 types.md §2.2 序列化：字段名 snake_case，`Option` 为 `None` 不输出，时间是 RFC 3339 UTC，带数据的枚举用 `kind`。cli 不重新定义 `SessionStatus`、`Proposal`、`Checkpoint`、`Hit`、`PushOutcome`、`NewReport`、`RemoveReport`、`ResolutionReport`。

| 命令 | 成功时的 `data` | 按条目返回、不让整个调用失败 |
| --- | --- | --- |
| `new` | `NewReport` | — |
| `add` | `AddReport`：`{ session, added: {<repo>: WorkspaceState}, already: {<repo>: WorkspaceState}, warnings }`；锚点未知的 repo 打在 stderr | — |
| `status` | 一个会话：`SessionStatus`（顶层带 `id`、`ticket`（无则 null）；`manifest.id`、`manifest.ticket` 与之重复，**已弃用**，下一个大版本去掉，新代码读顶层）。warning `OldTreeKept`（status-perf A1）：`changed` 可为 `null`（daemon 还没在后台检查这棵旧树，同时带 `unchecked: true`，此时 `holders` 为空；daemon 不在、走本地回退时总是这样）；检查过的带 `checked_at`（RFC 3339）。旧 GUI 读到 `null` 应当当作「未知」。`--all`：`{ "sessions": [SessionStatus, …] }`，按 `SessionId` 升序。健康为 `Unknown` 的工作区带 `choices: ["abandon", "accept"]`。`OldTreeKept` 的 `holders` 原样透传 | — |
| `status --brief` | 只来自 store（status-perf C）：`SessionSummary` = `{ id, ticket（无则 null）, state, created（RFC 3339）, repos: [RepoName], phase?, workspaces: [{ repo, health_stored, path, checkpoint }] }`。`state` 是 store 的 lifecycle（`Creating`/`Active`/`Removing`/`Unknown`），没有只能从文件系统得出的 `Missing`；`health_stored` 是 store 记录的健康（`Ok`/`Unknown`），工作区目录缺失在这里看不出；`checkpoint` 是最新检查点，untracked 或没有则 null。不算 drift、seeds、mirrors、旧树，不 stat 工作区，不跑 git。一个会话：该对象；`--all`：`{ "sessions": [SessionSummary, …] }`，按 id 升序。daemon 不在或 daemon 不认识 `summary`/`workspaces` 时读 store，并带 `source: "local"` | — |
| `ls` | `{ "sessions": [{ id, ticket, state, created, repos, phase? }, …], "source"? }`：全部未 Removed 的会话，按 id 升序，字段同上（没有 `workspaces`）。`source: "local"` 同上。`--table`：`ID  TICKET  STATE  REPOS  CREATED`（ticket 为空写 `-`，repos 用逗号连），不套信封 | — |
| `checkpoint` | `{ "checkpoints": [Checkpoint, …] }`，该次工具调用或 `-m` 在每个被记的 repo 上的那一行。幂等命中时就是已有的那几行，不是新 id | — |
| `restore` | `{ "checkpoint": CheckpointId, "session": SessionId }` | — |
| `grep` | `{ "hits": [Hit, …] }`。零命中是 `"hits": []`，`ok` 为 true，不是 `NoMatch`。`hits` 这个键总在 | 前 8000 字节含 NUL 的文件跳过，不进 `hits`。被跳过的放 `skipped: [{ "session", "repo", "path", "code": "Binary" }]`。超过 64 MiB 的同样按条目，`code` 为 `TooLarge`，不读内容。非法 UTF-8 按宽松解码，每个坏序列一个 U+FFFD，不让整条输出失败（D9） |
| `files` | `{ "paths": [ { "session", "repo", "path" }, … ] }`。没有匹配是空数组 | — |
| `diff` | `{ "repo": RepoName \| null, "only_a": [FileChange], "only_b": [FileChange], "both": [BothChange] }`。`repo` 是这次比的那个 repo（缺省规则见 §2），`null` 只在 `--session` 缺省扫描没有命中时出现。相对基线时只有 `only_a` 有条目（会话相对基线），`only_b` 与 `both` 为空数组。字节相同的文件不列出 | 二进制或超阈值的条目不带 `preview`，改为 `code`：`Binary` 或 `TooLarge`。调用仍然 `ok` |
| `deps propose` | `Proposal`；带 `--switch` 时是 `{ "proposal": Proposal, "switched": { "session", "kind": "moved"\|"skipped", "code"?, "detail"? } }`。本会话有打开的依赖文件等原因没换：`kind = "skipped"`、带 `code`（如 `OpenFiles`），提议照样记下，仍是 `ok: true`（同 D17） | `OpenFiles`（在 `switched.code` 里） |
| `deps approve` | `{ "results": [ { "session", "kind": "moved"\|"skipped", "code"?, "detail"? } ] }`。某个会话 `OpenFiles` 则那一条 `kind = "skipped"`，`code = "OpenFiles"`，其余照常。全部跳过仍是 `ok: true`（D17） | `OpenFiles` |
| `deps list` | `{ "proposals": [Proposal, …] }`，按 `ProposalId` 升序。没有则空数组 | — |
| `publish` | `{ "commits": { "<repo>": CommitOid }, "checks_ran": <u32>, "waiting": [Proposal, …], "pushes": [ { "repo", "outcome": PushOutcome } ], "already_published": [ { "repo", "publish", "commit", "push" } ] }`。`pushes` 只含这次真的推了的仓库。`already_published` 是重跑时之前已经落地、这次跳过的仓库（daemon.md D22，review H4）。多仓库时某个仓库失败（如 `Conflict`），前面已经提交的仓库照样写进 `data.commits` / `data.pushes`，`error.detail.repo` 是停下的那个仓库。没有检查的仓库 `checks_ran` 为 0，不是错误。冲突不是成功：整次 `ok: false`，`error.code = "Conflict"`，`detail` 用 publish 给的清单，cli 不再截断。`ok: true` 只在每个 `outcome` 都是 `Pushed`（或 `pushes` 为空）时成立，不能把 `Unknown` 藏进成功。`commit_tree` 之后推送超时：`ok: false`，`error.code = "Unknown"`，链留下，`data.pushes` 仍带那个 `PushOutcome`。远端拒绝不在这次里改写成成功：`publish.push` 为假时不推，`PushRefused` 留给之后单独的 `git-strata push`；这次若已经推了且远端拒绝，`ok: false`，`error.code = "PushRefused"`，链留下。这两种情况下停下的仓库的提交和推送结果都在 `data.commits` / `data.pushes` 里（取自 daemon 的 `detail.landed`，review M4） | — |
| `push` | `PushOutcome`（`kind` + `message`）。`Refused` 不在成功里出现，publish 已把它转成 `PushRefused` | — |
| `assign` | `{}` | — |
| `rm` | `RemoveReport` | — |
| `gc` | `{}`；`--store` 时 `{ store: GcReport }`；`--store --dry-run` 时 `{ dry_run: true, preview: GcPreview }` | daemon 回复里的 `skipped` 与 `trash`（`removed`、`young`、`refused`、`stuck`）打到 stderr；`--store` 时再打 `trash_refused`、`stuck` 与 `corrupt`（D33） |
| `du` | daemon `du` 的回复原样（daemon.md §5.12） | — |
| `daemon start` / `stop` | `{ "already": true }` 或 `{}` | — |
| `daemon status` | 见 §3.3。daemon 没在跑也有这个对象 | — |
| `resolve` | `ResolutionReport` | — |
| `chain` | `{ "repo", "choice", "outcome": PushOutcome }`。`choice` 是 `accept_origin_replay`、`accept_origin_drop` 或 `restore`。replay 与 drop 不推，没有 `outcome`，改为 `"refs_moved": true`，也不带凭据。`--restore` 的 `outcome` 与 `push` 相同：超时是 `Unknown`，`ok` 不能为 true | — |

`grep` / `files` / `diff` 的路径按 `RepoPath` 的字节序排列（NFC，区分大小写）。不按 inode，不分页，一次把结果放进这一个 JSON（D10）。

`diff` 的接线类型是 cli 自己的，只活在 JSON 里。hunk 的字段用 publish 给出的，cli 不另定一份：

```rust
/// 一边有、另一边没有的路径。`kind` 序列化为变体名。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    pub path: RepoPath,
    pub kind: ChangeKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChangeKind { Added, Deleted, Modified }

/// 两边都碰过的路径。`code` 为 `Binary` 或 `TooLarge` 时没有 `preview`。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BothChange {
    pub path: RepoPath,
    pub kind: ChangeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<DiffPreview>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<Code>,
}

/// `hunks` 的元素是 publish 的合并预览，cli 不重新定义 hunk。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum DiffPreview {
    Clean { hunks: Vec<serde_json::Value> },
    Conflict { hunks: Vec<serde_json::Value> },
}
```

`status` 在健康为 `Unknown` 的工作区上加的 `choices` 是 `MoveResolution` 的 JSON 字符串（`abandon`、`accept`），不是新枚举。

### 3.3 `daemon status` 的 `data`

```json
{"running":true,"socket":"/Users/…/Library/Application Support/git-strata/daemon.sock","session":"s-k3q7xm2a","holders":[{"repo":"tool","since":"2026-10-23T00:00:00Z"}],"unknown_publishes":[],"alerts":[]}
```

- `running`：daemon 是否在听 socket。没在跑时 cli 本地造这份对象，`running` 为 false，`holders` 与 `unknown_publishes` 为空数组，不假装知道锁。
- `session`：cwd 能解析出会话才有，否则省略。daemon 回答了时，这个查找（读 config、开 store）放在另一个线程里，最多等 2 秒（`STATUS_LOOKUP_WAIT`）；超时就省略 `session`，加 `"session_lookup": "timed_out"`，信封照常 `ok`。SessionStart 钩子不会因为 store 一时读不动而卡住。daemon 没在跑时直接查。
- `holders`：谁持着哪把 repo 写锁、从什么时候起。daemon 在跑时由 `daemon_status` 填。
- `fds`：daemon 的文件描述符计数（`open`、`limit` 等），daemon 在跑时原样透传 `daemon_status` 的 `fds`（D35）。
- `alerts`：隔离区、`index-read-back-refused`、`staging-changed` 这类要人看的项，原样透传 daemon 的 warnings。每条同时在 stderr 打一行。

### 3.4 RPC

请求一行：`{"id":"<十进制>","method":"<上表的方法名>","params":{…}}`。`id` 从 1 递增。回答一行：同一个 `id`，加上信封。`id` 对不上 → `InvalidInput`，`detail.reason = "rpc_id"`。

daemon 的 `UnknownMethod` 与 `Failed` 不是 `Code`。cli 收成 `InvalidInput`：`detail.reason` 为 `"unknown_method"` 或 `"rpc_failed"`，`detail.method` 是发出去的方法名（D7）。上游的 `StrataError` 原样放进 `error`，码不改写。

socket 是 `~/Library/Application Support/git-strata/daemon.sock`。没有环境变量能改这个路径。

### 3.5 Claude Code

`PostToolUse` 的命令行就是：

```text
git-strata checkpoint --tool-call
```

Claude Code 不设 `TOOL_USE_ID` 这类环境变量；工具调用的 id 在钩子 stdin 的 JSON 里（`tool_use_id`，与 `session_id`、`cwd`、`tool_name`、`tool_input` 并列）。`--tool-call` 不带值或带空值时，id 取 stdin JSON 的 `tool_use_id`。stdin 是 TTY、不是 JSON、或其中 `tool_use_id` 缺失或为空 → `InvalidInput`，`detail.reason = "checkpoint_reason"`，与现在相同。`--tool-call ID` 带非空值时用命令行的值，stdin 只用来取路径。[实现时补的决定]（D22）

会话从钩子的 cwd 解析，不在命令行上传 `--session`。一次钩子可以记下不止一个 repo，不能只记 cwd 所在的那个。`--tool-call` 存在且 stdin 不是 TTY 时，读 stdin 的一个 JSON 对象（Claude Code 的钩子输入）：`tool_input.file_path`、`tool_input.file_paths`、`tool_input.edits[].file_path` 都收成绝对路径。每条绝对路径落到哪个会话仓库，就记哪个仓库：工作区是 `<root>/sessions/<session-id>/<repo>/`，用路径前缀对上，不调用 `Sessions::open`。从 tool 工作区来的 `../hr-core` 一旦变成绝对路径，落在同一会话的 `hr-core` 里，就必须记在 `hr-core` 上，不能丢掉，也不能记成 cwd 那个 repo 的检查点。落在这个会话之外的路径跳过，不让 cwd 的检查点因此失败。没有路径的工具（`Bash`）对这个会话每个 tracked repo 走 `Inbox::require_full`，不是只扫 cwd。stdin 是 TTY 时只认命令行上的 `--path`，映射规则相同（D5）。

同一个 `--tool-call` ID 再来一次是同一条检查点（幂等）。键是（会话，repo，id），与 types.md 的 `CheckpointReason::ToolCall` 一致。cli 先查 store 里已有的 `Checkpoint` 行，不调用 `Sessions::open`：这次要记的每个 repo 都已经有这个 id → 返回那些 `Checkpoint`，不再请 daemon 记第二条。缺了的才连接。daemon 没在跑、而 id 已经都在 → 仍返回那些行（这是读）。daemon 没在跑、而 id 是新的 → `Busy`，`detail.reason = "daemon_down"`，什么都不写。

`SessionStart` 的命令行是 `git-strata daemon status`。它打出的那一行信封里有 daemon 是否在跑，以及 cwd 能解析出的会话 id。没有第二行散文。

安装：把 `git-strata` 放在 `PATH` 上。`git strata` 能用，是因为 git 会执行 `PATH` 上的 `git-strata`，参数是 `strata` 后面的那些。`argv[0]` 两种叫法都进同一个 clap 应用，不另写一套子命令，仓库里不放包装脚本（D1）。

钩子由人写进 Claude Code 的设置，本文不生成文件：

```json
{"hooks":{"PostToolUse":[{"matcher":"Edit|Write|Bash|MultiEdit","hooks":[{"type":"command","command":"git-strata checkpoint --tool-call"}]}],"SessionStart":[{"hooks":[{"type":"command","command":"git-strata daemon status"}]}]}}
```

## 4. 数据与不变量

cli 不拥有表，不拥有目录。它读的配置目录是 `~/Library/Application Support/git-strata/`（design 裁定：不是 `~/Library/Application Support/strata/`）。

每一行输出：

```json
{"ok":true,"data":{},"error":null}
{"ok":false,"data":{},"error":{"code":"Busy","message":"daemon is not running","detail":{"reason":"daemon_down"}}}
```

- 一行，紧凑（`serde_json` 的紧凑格式，不是 pretty），末尾一个 `\n`，stdout 上没有别的字节（D12）。
- `ok`、`data`、`error` 三个键都在。`data` 不是 null；没有结果时是 `{}`。成功时 `error` 是 null，不是省略。这压过 types.md §7“失败时没有 `data`、成功时没有 `error`”和 3b 那一行的同句：T14 的裁定是 `data` 总在（§9）。
- `error` 是一个 `StrataError`：`code` 是变体名，`message` 总在，`detail` 按 types.md 省略 `None`。`Busy` 且 daemon 没跑时 `detail` 总有，`reason` 为 `"daemon_down"`。
- `error.code` 必须是 types.md `Code` 的变体名。闭集 39 个，cli 自己不发明码。对不上的 RPC 层名字按 D7 收成 `InvalidInput`。
- 程序只按 `error.code` 分支。`message` 的措辞可以改。
- 退出码：信封 `ok` 为 true 且 stdout 的 write 与 flush 都成功 → 0。其余 → 1。不使用别的退出码。
- stdout 写失败（管道关掉、flush 失败）按 3b：退出码 1，能写 stderr 就写一行 `stdout_write_failed`，不写半行 JSON，不 panic，不在 `Drop` 里吞掉 flush 错误。显式 `flush` 并检查。

不变量：

1. 不变量 7：`deps approve`、要确认的 `push`、会推且要确认的 `publish`、要确认的 `chain --restore`、`config relax`、`rm --discard` 在 `require_tty` 失败时不连接、不发 RPC、不弹框（`publish` 例外：批次里有要确认的仓库时，只先发 `BeforePublish` checkpoint 与只读的 `publish_plan`，用来知道哪些仓库已经落地；不发 `publish`、不弹框、不取凭据，D23）。没有 flag、环境变量或配置项能跳过。配置文件把 `deploy_on_push` 写成 false 也不够：钉住的值若仍要确认，照样先查 TTY（读上游要求 3 的文件；文件不存在则用 `needs_human_confirm()`；文件读不出则当作要确认，D21）。不推的仓库、以及 `chain` 的 replay / drop，不取 `PushDeploy`。
2. 不变量 6：cli 自己不清理现场。连接在请求写出去之后断开，写命令返回 `Unknown`，`detail.reason = "socket_closed"`，因为不知道 daemon 写到了哪一步。读命令在同样的断开上返回 `Busy`，`detail.reason = "daemon_down"`（读还没写任何东西）。
3. 不变量 3：cli 不提供绕过推送确认的开关。`assign` 不确认，与 design §6.9 一致。
4. 一次确认对上的就是那一次计划。被取代的提议、主干已经移开的 head、Rewound / Diverged 的链，都是另一次命令，不是这次命令的重试。

## 5. 流程与算法

cli 不拿 repo 写锁，不占会话队列，不等待建造锁。锁在 daemon 里，顺序是建造锁 → 会话队列 → repo 写锁。写命令发出去之后，cli 不再设截止时间：第一次可见的写之后的等待不受 30 秒截断，客户端提前断开会把一次还在进行的发布看成失败，而 daemon 仍在写（「Waits after a call's first visible write are not bounded」）。连接还没建立时，socket 的连接超时是 200 ms（D6）；连不上就是 daemon 没在跑。

共同的五步：

1. 解析子命令。未知子命令 → `InvalidInput`，`detail.commands` 见 §3.1。不到第 2 步。
2. 确认类，以及 D18 的 `resolve` 与 `chain` 的选择：`require_tty`。要确认的 `chain --restore`、`config relax` 也在连接前查 TTY。会推且钉住的值要确认的 `publish` 是例外（D23）：批次里有这类仓库时，先连 daemon 发每个批次的 `BeforePublish` checkpoint 与只读的 `publish_plan`，再只对还要提交的那些仓库查 TTY；不是 TTY 时同样返回 `NotTty`，不发 `publish`、不弹框、不取凭据（review-3 T4）。失败即返回 `NotTty`，不到第 3 步。`resolve` / `chain` 在这一步之前就拒绝缺选择或两个都给，同样不连接。`chain --restore` 且 `!is_ancestor(o, c)` → `InvalidInput`，`detail.reason = "not_ancestor"`，不连接、不取凭据、不推。
3. daemon 在听：写一行请求，读一行回答，原样映射到信封（上游的码不变）。`UnknownMethod` / `Failed` 按 D7。
4. daemon 没在听：`status`、`grep`、`files`、`diff`、`deps list`、`daemon status` 走本地只读路径（§5.1）。其余返回 `error.code = "Busy"`，`detail.reason = "daemon_down"`，`message` 告诉人先跑 `git-strata daemon start`。不新加码（D2）。
5. `checkpoint --tool-call ID`：先按 §3.5 查本地。已经都有则返回那几条，不再请 daemon 记第二条。

确认类在第 3 步之前多做一次展示，仍然不发写请求：

1. `deps approve`：读那条 `Proposal`（daemon 在跑就经 `deps_list`，否则 `deps::open_proposals`；两种都不调用 `Sessions::open`）。把 `id`、`repo`、`state`、`from`、`to`、`from_inputs`、`to_inputs`、`tree`、`head` 打到 stderr。凭据只有 `DepsApprove { proposal }`，不含会话：弹框前再逐行列出这次会移动的会话 id（只读的 rollout 计划里判定为移动的那些；同样不调用 `Sessions::open`）。然后才发 `deps_approve`。daemon 弹框。回答是 `NoSuchProposal`：把 `error.detail` 打到 stderr；`detail.state` 若是一个 `ProposalId`，或 `detail.replacement` 是一条 `Proposal`，再把那条替换提议的同样字段打到 stderr。进程到此退出，不发第二次 `approve`。`HeadMoved` 且 `detail.reason = "main_moved"` 同样：打出 `detail.tip`，不重试，不批准另一个 head。
2. 要确认的 `push`，会推且要确认的 `publish`，以及要确认的 `chain --restore`：先按 §4 判断要不要 TTY（钉住的值；读不出当作要确认）。要确认才展示。凭据只有 `PushDeploy { repo, branch }`，不含提交：stderr 列出这次确认的 a..c 里每个 commit，不只写链顶。`publish` 对批次里每个要推且要确认的仓库各准备一次 `PushDeploy`，在该仓库的 `commit_tree` 之前；没有则该仓库 `NotConfirmed`，这个仓库不提交（front 就是要推且要确认的仓库）。不推的仓库不确认。然后才发请求。daemon 弹框。链在展示之后变了，由 daemon 拒绝；cli 不重发。为此 cli 把展示过的链顶随请求发出（`push`/`chain_resolve` 的 `shown`，`publish` 的 `shown: { repo: top }`，没展示链为 `null`），daemon 在锁里比它（daemon.md D21，review H2）。`publish` 每个仓库的推送结果按 `push` 映射，写进 `data.pushes`：`commit_tree` 之后超时是 `Unknown`，链留下，`ok` 不能为 true；`publish.push` 为假时这次不推，`PushRefused` 留给之后单独的 `git-strata push`。
3. `rm --discard`：stderr 打一行会话 id 和“未发布的改动会被丢掉”。然后发 `rm`。
4. `config relax`：`require_tty` 之后才连接，然后用 `ConfigRelax` 弹框并调用 `config_relax`，与其他确认命令相同。不因配置文件变了而跑。收紧不走这条命令。

`resolve` 在发 RPC 之前把该工作区的 `scene` 和两条 `choices` 打到 stderr，并写明这次 flag 选的是哪一条。`chain` 的 replay / drop 不取凭据；`--restore` 只有在上面第 2 步要确认时才取 `PushDeploy`。两个命令都只发一次 RPC。

设计 §8 的数字不是 cli 自己的预算：checkpoint 改 50 个文件 < 200 ms、热 `grep` < 300 ms，由 daemon / search 达到。cli 加上的只有参数解析、一行 JSON、以及确认类在连接前的 TTY 检查。确认类的等待是人的时间，上限不由 cli 截断。

### 5.1 本地只读路径

只在第 4 步、且只对那六个命令，并且只在 daemon 没在跑时。不调用 `launchctl`，不创建会话，不调用任何 store 写方法。daemon 在跑时，`open_proposals` 与 `--tool-call` 的查找只读 store 的行或走只读 RPC，不调用 `Sessions::open`。再开一个 `Sessions` 会把进行中的移动报成 `MoveAwaitingRecover`。

1. 读 `config.toml`，`Config::parse` 失败 → 透传 `InvalidInput`（`detail.rule = "toml"` 由 types 填的原样保留）。
2. 仅在 daemon 没在跑时按 session.md 打开 `Sessions`：每个 repo 一个 gitio 的 `Git`（打在主 checkout 上），store 以读者打开（WAL，读者不堵写者）。交给它的 `RepoWriteLock` 立刻返回 `Busy`，从不等待；`BeforeCheckpoint` 若被调用则返回 `InvalidInput`，`detail.reason = "readonly_client"`。只读路径不得走到会调用它们的函数。daemon 在跑时不执行这一步。
3. `status` → `Sessions::status` / `list`。`changed` 用 session 已经规定的读时重算。健康 `Unknown` 时 cli 自己加上 `choices`。
4. `grep` / `files` → 进程内 attach、查询、detach。零命中是空列表。flag 只有 §3.1 那两个。
5. `diff` → `publish::diff_between` 或相对基线的只读比较，形状按 §3.2。
6. `deps list` → `deps::open_proposals`。
7. `daemon status` → §3.3 里 `running: false` 的那份。cwd 能解析出会话才填 `session`。

`ls` 与 `status --brief`（D36）不走上面这几步：daemon 以 `status{summary:true}` 回答；只在 `CallError::Down`（socket 连不上），或 daemon 以 RPC `Failed` 且 message 含 ``unknown field `summary` `` / ``unknown field `workspaces` ``（早于它的 daemon，参数 `deny_unknown_fields`）时，以 `Store::open_read_only` 读 store（不开 `Sessions`、不建 `Git`），`data.source = "local"`。其它 daemon 错误照常返回，不回退。选择器只读 store：`--session` 查 id / ticket，`--all` 列 store，cwd 只把 cwd 与 `<root>/sessions` 各 realpath 一次再按路径分量对 store 的会话与 repo 名比，不 stat、不 canonicalize 任何工作区路径。

本地打开失败（store 打不开、配置缺路径）透传那个码，不改写成 `daemon_down`。`daemon_down` 只表示 socket 连不上。

### 5.2 幂等的检查点

`--tool-call` 的查找只读 store 里该会话已有的 `Checkpoint` 行，不调用 `Sessions::open`。`reason.kind = "ToolCall"` 且 `reason.id` 相等即命中。要记哪些 repo 按 §3.5 的路径映射，不是只记 cwd。没有路径时，对该会话每个 tracked repo 各查一次。`-m` 不按 id 幂等，但也不保证每次一条新行：与最新的 checkpoint 相比没有变化时，overlay 返回那条最新的（它的号与 message，`recorded = false`，overlay.md §5.6 第 4 步）。新的 `--tool-call` id 即使没有变化也记一行（review M8）。（review L1）

### 5.3 stderr

JSON 成功之后：若这次解析出了会话，再读一次 `deps::open_proposals`（store 的读者，不发写 RPC，不调用 `Sessions::open`）。这次读在写信封之前。对该会话仍是 Pending 的提议非空、且 `data` 是对象时，加一个键 `data.pending_proposals: [{ "id": ProposalId, "repo": RepoName }]`；没有就不加这个键。stderr 是终端时每条再打一行 `pending proposal <id> for <repo>`，不是终端（代理、管道）时不打，代理只看信封。这次附加读失败当作没有，不影响信封的 `ok` 和退出码（D20）。`status` 的 `data` 里照常有 `pending`。

回答里的 `alerts`，以及 `status` 里要人看的 warnings（隔离区、attach 的 `index-read-back-refused` / `staging-changed`），各在 stderr 打一行。不改 `ok`。

### 5.4 交互式发布选择

`git-strata publish` 没有 `--from`、且 stdin 是 TTY：先取 cwd 会话的 `status`，把有改动的路径按编号打到 stderr，读 stdin 的编号或路径，空行结束，收成 `--from` 等价的批次。读到的不是编号也不是路径 → `InvalidInput`，不发 `publish`。选路径不是确认，不代替 `PushDeploy`。批次收齐之后，按 D23 先发 checkpoint 与 `publish_plan`，再对还要提交的、要推且要确认的仓库 `require_tty`（review-3 T4：不是在连接前），并在该仓库 `commit_tree` 之前各取一次 `PushDeploy`。

## 6. 失败与恢复

| 情形 | 码 | 连了 daemon 吗 | 留下什么 |
| --- | --- | --- | --- |
| 非 TTY 上的确认命令：`deps approve`、要确认的 `push`、会推且要确认的 `publish`、要确认的 `chain --restore`、`config relax`、`rm --discard`，以及 D18 的 `resolve` 与 `chain` 的选择 | `NotTty` | 否（`publish` 例外：只发了 checkpoint 与 `publish_plan`，D23） | 无（`publish` 只留下 `BeforePublish` checkpoint） |
| `chain --restore` 且 `!is_ancestor(o, c)` | `InvalidInput`（`not_ancestor`） | 否 | 无 push |
| 要推且要确认的仓库，在它的 `commit_tree` 之前没有匹配的 `PushDeploy` | `NotConfirmed` | 是 | 这个仓库没有 commit。不回滚别的仓库 |
| 未知子命令、缺选择、两个选择都给、参数格式错 | `InvalidInput` | 否 | 无 |
| daemon 没跑，且是写命令（新的 `--tool-call` 也算写） | `Busy`（`daemon_down`） | 否 | 无 |
| 已有的 `--tool-call` 再来，daemon 没跑 | 成功，返回已有行 | 否 | 无新行 |
| 上游拒绝 | 上游的码 | 是 | 上游已经留下的 |
| 提议已被取代 | `NoSuchProposal` | 是 | 无移动。stderr 有替换提议。不自动批准 |
| 批准时 head 已不是主干 tip | `HeadMoved`（`main_moved`） | 是 | 无移动。不重试 |
| 链 Rewound / Diverged，人还没用 `chain` | 透传 publish 的 `InvalidInput`（`chain_rewound` / `chain_diverged`） | 是 | 无。人用 `git-strata chain` |
| 凭据被取消、用过、过期、action 不符 | `NotConfirmed` | 是 | 一次一用。人重新跑命令 |
| 请求写出去之后 socket 断了，且是写命令 | `Unknown`（`socket_closed`） | 曾经是 | 不清理。人看 `status` |
| stdout 已关，或 flush 失败 | 无 JSON（写不出） | 视命令而定 | 退出码 1 |
| RPC `UnknownMethod` / `Failed` | `InvalidInput`（`unknown_method` / `rpc_failed`） | 是 | 无 |

cli 不重放 `Unknown`，不删除现场，不在失败后改跑 `gc` 或 `rm`。

## 7. 测试清单

| 测试名 | 断言 | 来源（矩阵行 或 新增） |
| --- | --- | --- |
| `every_command_emits_ok_envelope` | §3 每个命令的成功输出都是一行紧凑 JSON，独立解析器（不是 `serde_json`）能还原；三个键都在；`data` 不是 null；成功时 `error` 为 null；没有结果的命令 `data` 是 `{}`。不断言两空格缩进 | `layerfs-telemetry/tests/timer_json.rs::json_matches_the_documented_shape` |
| `envelope_keys_always_present` | 三个键都在。成功时 `error` 为 null，不省略。失败时 `error` 是对象，`data` 不是 null。`data` 为 `{}` 只在这次失败没有结果时成立。`commit_tree` 之后的推送超时是 `ok: false`，`error.code` 为 `Unknown`，`data.pushes` 仍在，不得为了让 `data` 变成 `{}` 而丢掉 `pushes`。本行的“另一个键不出现”不沿用 | `layerfs-telemetry/tests/timer_json.rs::outcome_and_incomplete_fields_are_opt_in` |
| `json_escaping_round_trips` | `data` 里的路径与 `message` 含引号、反斜杠、制表、换行、控制字符、非 ASCII 时，独立解析器还原出原字符串 | `layerfs-telemetry/tests/timer_json.rs::escaping_round_trips_through_an_independent_parser` |
| `stdout_write_failure_is_nonzero` | stdout 在写信封时被关闭：进程退出码 1，不 panic，不把该次调用报成成功 | `layerfs-telemetry/tests/timer_json.rs::writer_failures_propagate` |
| `stdout_format_write_failure_is_nonzero` | 同上，覆盖格式化写失败这条同源用例 | `layerfs-telemetry/tests/timer_format.rs::writer_failures_propagate` |
| `stdout_flush_failure_is_nonzero` | 显式 flush 失败（`BufWriter` 的 `Drop` 会吞掉）也是退出码 1 | `layerfs-telemetry/tests/timer_json.rs::a_flush_failure_is_surfaced` |
| `error_codes_match_types_md` | 故意触发的每个 `error.code`，以及 `skipped[].code` 与 `BothChange.code` 里的码，都是 types.md `Code` 的变体名。闭集 39 个，测试从枚举读名字，不在 cli 里抄第二份。RPC 的 `UnknownMethod` 不出现在 `error.code` 里 | 新增 |
| `hook_checkpoint_is_idempotent_per_tool_call` | 同一个 `--tool-call` 对同一会话同一 repo 跑两次：两次 `data.checkpoints[].id` 相同，store 里该键只有一行；第二次没有发写 RPC。从 tool 工作区来的 `../hr-core` 记在同一会话的 `hr-core` 上，不丢掉，也不记成 cwd 的 repo。会话外的路径跳过，cwd 的检查点不因此失败。一次钩子可以记下不止一个 repo | 新增 |
| `daemon_up_read_does_not_open_sessions` | daemon 在跑时，`open_proposals` 与 `--tool-call` 的查找不调用 `Sessions::open`。第二次打开会把进行中的移动报成 `MoveAwaitingRecover`，这个测试断言没有第二次打开 | 新增 |
| `confirm_commands_refuse_non_tty` | `deps approve`、`needs_human_confirm` 的 `push`、批次里有要推且要确认的仓库的 `publish`（front 即是）、要确认的 `chain --restore`、`config relax`、`rm --discard` 在非 TTY 上退出码 1，`error.code` 为 `NotTty`，socket 上没有字节（`publish` 在会话读不到时同样没有；`non_tty_refused_before_prompt` 断言真实会话上只发了 checkpoint 与 `publish_plan`，D23）。`non_tty_rerun_of_a_landed_guarded_repo_is_not_gated`：要确认的仓库已经落地，非 TTY 重跑成功、`already_published` 带它、不弹框；内容再变则 `NotTty`，不发 `publish`。`non_tty_publish_refuses_a_guarded_repo_the_plan_left_out`（review-3 T1）：计划里没有那个仓库（已落地），`publish` 到达前另一条 Done 行落地了不同内容，daemon 回 `NotConfirmed` `plan_changed`，没有弹框，链和 origin 不变。`chain` 的 replay / drop 在非 TTY 上也是 `NotTty`，且不取凭据 | 新增 |
| `readonly_works_without_daemon` | socket 不存在时 `status`、`grep`、`files`、`diff`、`deps list`、`daemon status` 仍是信封；`publish` 与 `push` 是 `Busy`，`detail.reason` 为 `daemon_down` | `sdk/js/test/trace.mjs::store` |
| `history_is_read_from_the_store_alone` | daemon 没跑时 `status` 的检查点列表来自 store 的只读打开，不要求 socket | `layerfs-api/tests/operator_history.rs::history_is_read_from_the_store_alone` |
| `binary_file_is_marked_not_rejected` | 内容含 NUL 的文件：`diff` 该条 `code` 为 `Binary`，没有 hunk，整个命令仍 `ok`；`grep` 不把该文件的字节当文本命中，放进 `skipped` | `sdk/js/test/binary_write.mjs::blob` |
| `lossy_utf8_one_fffd_per_bad_sequence` | `grep` 命中行与 `diff` hunk 里的非法 UTF-8 按每个坏序列一个 U+FFFD 解码，命令不失败 | `sdk/js/test/exec_output.mjs::binary` |
| `diff_lists_content_changes_only` | 新增、删除、修改按内容列出；字节相同不算变更，不出现在列表里。没有游标，没有 inode 身份字段 | `layerfs-api/tests/diff_resume.rs::a_resumed_walk_lists_what_the_whole_walk_lists_after_its_cursor` |
| `diff_omits_hunks_for_binary_and_ignores_inode` | 硬链接身份不输出；类型变化按条目给出；二进制文件省略 hunk，调用不失败 | `layerfs-api/tests/sdk_compare.rs::diff_preserves_hardlink_identity_type_changes_and_hunk_omission` |
| `grep_skips_nul_and_keeps_valid_hits` | 含 NUL 的文件按 `Binary` 跳过；非法 UTF-8、多字节字符、超长行的停止点与“按行可观察”的结果一致。不断言窗口或块的扫描策略 | `layerfs-api/tests/sdk_text_scan.rs::grep_and_read_give_the_window_scan_results` |
| `no_multi_fault_order` | 不保留：clap 先拒绝参数，再解析会话与路径。没有“同一次调用里两处故障谁先报”的契约。错误码名字仍只来自 types.md | `layerfs-api/tests/method_refusal_order.rs::the_first_fault_met_is_the_refusal` |
| `path_order_is_repopath_ord` | 不保留 inode 分片续点。输出按 `RepoPath` 字节序，不按目录组件的 64 项分片 | `layerfs-api/tests/sdk_compare.rs::traversal_continues_at_64_and_65_children_in_component_order` |
| `no_pagination` | 不保留字节窗口与续页。`grep` / `files` / `diff` 各一次输出完整 JSON | `layerfs-api/tests/sdk_compare.rs::byte_limited_windows_preserve_diff_grep_and_glob_membership` |
| `conflict_json_is_not_windowed` | 不保留 `conflictsTruncated` 之类的窗口标志。`Conflict` 的 `detail` 是 publish 给的完整清单，cli 不再截 | `layerfs-api/tests/sdk_merge_diagnostics.rs::sdk_conflict_keeps_its_outcome_and_explicit_byte_window_flags` |
| `shape_is_this_doc_not_a_golden` | 不保留 SDK 方法表 golden。每个子命令的 `data` 以 §3.2 为准 | `layerfs-api/tests/sdk_method_golden.rs::public_methods_answer_as_recorded` |
| `one_method_table` | 不保留 public/operator 两张分发表。cli 到 daemon 只有 §3 这一张表 | `sdk/js/test/packaging.mjs::dispatch` |
| `results_live_in_data_not_a_log` | 不保留 trace 行。会话 id、检查点 id、commit oid 只出现在 `data` 里 | `sdk/js/test/trace.mjs::ids` |
| `listing_is_one_document` | 不保留按字节分页的列表协议。`files` 的 `paths` 一次给完 | `layerfs-content/tests/filesystem_read.rs::listing_is_bounded_by_bytes_as_well_as_count` |
| `superseded_proposal_is_not_auto_approved` | `deps approve` 得到 `NoSuchProposal` 且 `detail.state` 是另一个 id：stderr 有那条替换提议的 id，进程退出码 1，socket 上只有第一次请求 | 新增 |
| `resolve_and_chain_require_an_explicit_choice` | `resolve` 不带 `--abandon`/`--accept`、`chain` 不带三个动作之一：`InvalidInput`，不连接。两个都给同样不连接。非 TTY 上即使 flag 齐也是 `NotTty`，不连接。`chain --restore` 且 `!is_ancestor(o, c)`：`InvalidInput`，`detail.reason = "not_ancestor"`，不推、不取凭据。replay / drop 不取凭据 | 新增 |
| `daemon_stop_waits_until_launchd_drops_the_agent` | 替身 launchctl 在 bootout 之后还列出服务三次：`stop` 一直轮询到不再列出才成功（D24） | 新增 |
| `publish_from_bad_paths_are_input_errors_not_not_tty`、`publish_from_prefix_is_caught_in_a_single_repo_session` | 配置里有要确认的仓库、非 TTY：`S:app/a.txt` → `InvalidInput` `from_repo_prefix`（`expected = S:a.txt`），`S:nope/b.txt` → `InvalidInput` `from_path`，两个仓库都有的路径 → `AmbiguousMatch` `from_repo`；都不是 `NotTty`，daemon 收不到请求（D30） | 新增 |
| `publish_from_prefers_the_changed_repo_and_takes_a_repo_qualified_form` | 两个仓库都有的路径：没有 checkpoint 改过 → `AmbiguousMatch`，`expected[0] = S:app:src/x.ts`；只有 app 的 checkpoint 改过 → 发布到 app；`S:app:src/y.ts` → 发布到 app；两个都改过 → `AmbiguousMatch`，`changed_in = [app, lib]`；`S:lib:nope.txt`、`S:zzz:src/x.ts` → `InvalidInput` `from_path`（D31） | 新增 |
| `daemon_start_stop_do_not_use_the_socket` | `daemon start` 与 `daemon stop` 的测试替身只记录 `launchctl` 参数，socket 上没有字节 | 新增 |
| `git_strata_is_the_same_binary` | `argv[0]` 为 `git-strata` 时，`status` 与经 git 的外部子命令约定传入的同样参数走同一个分发，没有第二棵命令树 | 新增 |

## 8. 实现任务（TDD）

### Task 1: 信封与写失败

**Files:**
- Create: `crates/git-strata/src/envelope.rs`
- Test: `crates/git-strata/tests/envelope.rs`

**Interfaces:**
- Consumes: types.md 的 `StrataError`、`Code`
- Produces: 一行 `{ok, data, error}`，以及 write/flush 失败时的退出码 1

- [ ] **Step 1: 写失败的测试**：`every_command_emits_ok_envelope`、`envelope_keys_always_present`、`json_escaping_round_trips`、`stdout_write_failure_is_nonzero`、`stdout_format_write_failure_is_nonzero`、`stdout_flush_failure_is_nonzero`、`error_codes_match_types_md`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p git-strata --test envelope`
Expected: FAIL（信封模块还不存在）

- [ ] **Step 3: 写让测试通过的最少实现**：紧凑 JSON、三键都在、显式 flush、`Code` 名字从 types 的枚举来
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p git-strata --test envelope`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/git-strata
git commit -m "feat(cli): json envelope and write failures"
```

### Task 2: TTY 闸门与确认

**Files:**
- Create: `crates/git-strata/src/tty.rs`
- Test: `crates/git-strata/tests/confirm_gate.rs`

**Interfaces:**
- Consumes: `require_tty`；config 的 `needs_human_confirm`；daemon 钉住的三个 flag 的文件（上游要求 3）
- Produces: 非 TTY 的 `NotTty`，且 socket 无字节

- [ ] **Step 1: 写失败的测试**：`confirm_commands_refuse_non_tty`、`superseded_proposal_is_not_auto_approved`、`resolve_and_chain_require_an_explicit_choice`
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p git-strata --test confirm_gate`
Expected: FAIL（闸门还不存在）

- [ ] **Step 3: 写让测试通过的最少实现**：§5 的确认顺序；替换提议只展示、不发第二次 RPC
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p git-strata --test confirm_gate`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/git-strata
git commit -m "feat(cli): require tty before confirm"
```

### Task 3: 子命令、只读路径与钩子

**Files:**
- Create: `crates/git-strata/src/main.rs`
- Test: `crates/git-strata/tests/cli.rs`

**Interfaces:**
- Consumes: §3 的方法表；daemon 没在跑时才 `Sessions::open`；search 的 `grep` / `files`；`deps::open_proposals`
- Produces: 子命令全集、本地只读路径、`--tool-call` 幂等

- [ ] **Step 1: 写失败的测试**：`hook_checkpoint_is_idempotent_per_tool_call`、`daemon_up_read_does_not_open_sessions`、`readonly_works_without_daemon`、`history_is_read_from_the_store_alone`、`daemon_start_stop_do_not_use_the_socket`、`git_strata_is_the_same_binary`，以及 §7 里 `binary_file_is_marked_not_rejected` 到 `listing_is_one_document` 的各行
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p git-strata --test cli`
Expected: FAIL（子命令还没分发）

- [ ] **Step 3: 写让测试通过的最少实现**：§3 的表、§5 的五步、§5.1 的只读打开。不添加 tgrep 的 CLI 选项
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p git-strata --test cli`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/git-strata
git commit -m "feat(cli): subcommands, readonly path, hook idempotency"
```

核对：`rg -o 'git-strata [a-z]+' docs/modules/cli.md | sort -u | wc -l` 至少 14。

## 9. 构建者决定记录

| 决定 | 选择 | 理由 | 日期 |
| --- | --- | --- | --- |
| D1 `git strata` | 同一二进制。git 执行 `PATH` 上的 `git-strata` | 初稿已定。两套实现会分叉。不放包装脚本 | 2026-10-23 |
| D2 daemon 没跑 | 写命令 `Busy`，`detail.reason = "daemon_down"` | 初稿已定。不加码。types.md 把这种情况写成 `ReadOnly` 的那半句不沿用：人能从 reason 看出要先启动 | 2026-10-23 |
| D5 钩子 stdin | `--tool-call` 且 stdin 非 TTY 时读 Claude Code 的钩子 JSON，取出 `file_path` / `file_paths` / `edits[].file_path` | daemon 要把 Edit/Write 的路径送进 `Inbox::hint`。命令行仍是简报里的那一条，路径不靠人再写一遍 | 2026-10-23 |
| D6 连接超时 | 还没连上时 200 ms。连上之后写命令不再设截止时间 | 连不上要快失败。连上之后截断会把一次还在写的发布看成客户端失败 | 2026-10-23 |
| D7 RPC 层错误 | `UnknownMethod` / `Failed` 收成 `InvalidInput`，reason 为 `unknown_method` / `rpc_failed` | 这两个不是 `Code`。最近的已有码是参数不被接受 | 2026-10-23 |
| D9 二进制与非法 UTF-8 | `skipped[]` 与 `preview.code`；文本按每个坏序列一个 U+FFFD | 3e 把形状交给 cli。整个调用不因一个二进制文件失败 | 2026-10-23 |
| D10 排序与分页 | `RepoPath` 字节序；一次输出，不分页 | 3d 的续页与 inode 序不保留，排序得有一个确定的规则 | 2026-10-23 |
| D11 `files` 的 glob | 一个可选位置参数，不是 flag | search 的 `files` 要 glob。T11 禁止的是 tgrep 那种选项，不是这个位置参数 | 2026-10-23 |
| D12 行尾 | 紧凑 JSON 后恰好一个 `\n` | “一行”要能被按行读取。多出来的字节会让独立解析器失败 | 2026-10-23 |
| D13 `publish --assign` | 与 `git-strata assign` 同一次 `assign` | design §6.5 的写法与冻结的方法名都要能用，不能变成两个实现 | 2026-10-23 |
| D15 plist 路径 | `~/Library/LaunchAgents/com.gwd.git-stratad.plist`，域 `gui/<uid>` | `start` 没有路径就无法 bootstrap。Label 用 daemon 已定的 `com.gwd.git-stratad` | 2026-10-23 |
| D16 重复 start/stop | 已装上或已卸下是成功，`data.already = true` | `launchctl bootstrap` 对已加载的服务会失败。人再跑一次不该看成故障 | 2026-10-23 |
| D24 `stop` 等服务真正卸下 | `bootout` 之后轮询 `print`，不再列出才返回，最多 10 s | 真实安装里 `stop` 后立刻 `start` 得到 `already` 而 daemon 没在跑。[实现时补的决定] | 2026-10-26 |
| D30 `publish --from` 的路径错误先报 | `--from S:PATH` 的 PATH 在会话里找不到仓库时，不再先对全部配置仓库做 TTY 检查（那会把路径错误变成 `NotTty`），直接返回：PATH 以会话里某个仓库名开头且去掉它之后在该仓库存在 → `InvalidInput` `from_repo_prefix`，`detail {path, session, repo, expected}`（单仓库会话也查）；多仓库会话里没有仓库有它 → `InvalidInput` `from_path`，`detail {path, session, repos, expected}`，message 写明 SESSION:PATH 格式；几个仓库都有 → `AmbiguousMatch` `from_repo`（不变）。这些都只读 store 与工作区，socket 上没有字节。会话本身读不到时仍先做 TTY 检查（`confirm_commands_refuse_non_tty`）。[实现时补的决定] | 真实使用里 `publish --from S:staff-api/x` 在非 TTY 上得到 `NotTty`，agent 以为要人类，实际是路径写错；路径错误不是要确认的动作，不变量 7 不涉及 | 2026-10-26 |
| D31 `publish --from` 指定仓库、按 checkpoint 消歧 | 新增 `--from S:REPO:PATH`：第一个与第二个 `:` 之间的文本是会话里的某个仓库名时当作 REPO，否则第一个 `:` 之后全部是 PATH（`S:PATH` 不变，PATH 里可以有 `:`；以 `<repo>:` 开头的 PATH 写成 `S:<repo>:<repo>:...`）。REPO 的工作区（磁盘或 base）没有 PATH、或 REPO 形似仓库名但不在会话里 → `InvalidInput` `from_path`。`S:PATH` 有几个仓库都有时，取会话里 checkpoint 改过该路径（目录则其下任一路径）的那一个仓库；没有或不止一个改过 → `AmbiguousMatch` `from_repo`，`detail {path, session, candidates, changed_in, expected}`，`expected` 是每个候选的 `S:REPO:PATH`。只读 store，纯 cli 改动，daemon 不变。[实现时补的决定] | 真实使用里 #9301 的 `src/utils/apis/locationDuty.ts` 在 admin 与 tool 都存在，只有 admin 改过它，`--from` 无法指到 admin，发版被卡 | 2026-10-27 |
| D17 全部跳过 | `deps approve` 每个会话都 `OpenFiles` 时仍 `ok: true`，条目里是 skipped | types.md：按条目返回，其余照常移动。没有“全部跳过”这个码 | 2026-10-23 |
| D18 选择要 TTY | `resolve`，以及 `chain` 的 replay / drop：非 TTY 返回 `NotTty`，不连接，不取凭据。`chain --restore` 在仓库要确认时走与 `push` 相同的 `PushDeploy`，会弹框。不再说 chain 从不弹框 | 裁定要求没有人的选择就不运行。replay / drop 不推，所以不取凭据。`--restore` 是推送，用已有的 `PushDeploy`，不新加 `Action` 变体 | 2026-10-23 |
| D19 `--help` / `--version` | clap 文本，退出码 0，不套信封 | 这两个不是子命令。其余 stdout 只有信封 | 2026-10-23 |
| D20 待确认提示 | 成功之后读 `deps::open_proposals`，不调用 `Sessions::open`，失败不影响退出码。2026-10-27 起放进 `data.pending_proposals`，stderr 那行只在终端上打 | design §6.4 第 4 步要每次 CLI 都提示。不能为了提示再发一次写 RPC，也不能再开一个 `Sessions` | 2026-10-23 |
| D21 钉住的值读不出 | 当作要确认，先 `require_tty` | 读不出就放行，等于让一份坏文件跳过 TTY 检查 | 2026-10-23 |
| D23 publish 的 TTY 检查只看还要做事的仓库（review-2 N2） | 批次里有要推且要确认的仓库时，cli 先发每个批次的 `BeforePublish` checkpoint，再发只读的 `publish_plan`（daemon.md §4），然后只对 `repos` 里那些要推且要确认的仓库 `require_tty`、展示链顶、发 `shown`。全部已经落地 → 不查 TTY，`publish` 回 `already_published`。还有要确认的仓库且不是 TTY → `NotTty`；在此之前送到 daemon 的只有这些 checkpoint 和 `publish_plan`：不发 `publish`，不弹框，不取凭据。checkpoint 或 `publish_plan` 失败且不是 TTY → 仍是 `NotTty`，原错误放在 `detail.plan_error`。批次里没有要确认的仓库时不发 `publish_plan`。`publish` 请求带 `plan`：TTY 检查覆盖了的那些仓库（可以为空）。`publish_plan` 与 `publish` 之间历史可能变（别的发布落地了不同内容），daemon 重算后多出来的要确认仓库不在 `plan` 里 → 在弹框、取凭据之前 `NotConfirmed`（`plan_changed`），什么都不写；agent 重跑，cli 重新计划并查 TTY（review-3 T1）。[实现时补的决定] | agent 重跑是主流程（D22）：之前要确认的仓库已经落地、只剩不用确认的仓库时，非 TTY 的重跑不该因已经落地的仓库被拒。checkpoint 只是记录当下内容，不是要确认的动作；不变量 7 守的是发布、弹框和凭据 | 2026-10-24 |
| D22 钩子的工具调用 id | `--tool-call` 不带值或为空 → 取 stdin JSON 的 `tool_use_id`；仍缺或为空 → `InvalidInput`（`checkpoint_reason`）。§3.5 的命令行改为 `git-strata checkpoint --tool-call`。[实现时补的决定] | Claude Code 不设 `TOOL_USE_ID` 环境变量，原命令行会传空串，每次钩子都失败 | 2026-10-23 |
| D31 `--repo` 与 `add` | `new --repo R`（可重复）给精简会话；不带时保留 `--repos` 的整套布局；新命令 `add [--session S] --repo R…`。`NoSuchRepo` 的 `not_in_session` 带 `detail.fix` 原样给出这条 `add` 命令。cli 只在 `--repo` 时给 daemon 发 `only` | 见 session.md D51/D52。新 flag 不改 `--repos` 的含义，旧用法与旧 daemon 都不受影响。否决：给 `--repos` 加 `--only` 修饰 | 2026-10-27 |
| D32 路径落在哪个工作区：末尾的符号链接不跟随（review P1-9） | `checkpoint`/`restore` 等把路径映射到会话与 repo 时，最后一个分量是符号链接就只解析它的父目录、保留链接名；这样落在某个工作区里就用它（链接条目本身）；链接本身不在任何工作区里（外面指进来的链接）才跟随到目标 | 原来整条路径 canonicalize：`restore CP link` 恢复的是链接指向的文件，改过的链接原样不动 | 2026-10-27 |
| D33 `gc --store [--dry-run]`（review P1-12） | 全局 store gc 是显式的选项，不是 `gc` 的缺省：`--store` 调 daemon 的 `gc { store: true }`（30 天保留期，daemon.md D33）；`--dry-run` 要求 `--store`（clap `requires`），走只读的 `gc { store: true, dry_run: true }`，回 `{ dry_run: true, preview }`，什么都不改。帮助里写明先跑 `--dry-run` |
| D34 推送计划写出目标 URL（review P1-15） | 要确认的 `push`/`publish`/`chain --restore` 在 stderr 的计划里多一行 `  to <origin 推送 URL>`（用户信息写成 `***`），读不出就写一句说明；弹框文字也带这个 URL（confirm.md §5.6），daemon 在锁里核对它没变（daemon.md D35） | 人批准的是推到哪里，不只是哪个分支 | 2026-10-27 |
| D36 `ls`、`status --brief`、`ls --table`（status-perf C，D4/D6） | 新增 `ls [--table]` 与 `status --brief`，只读 store（§3.2、§5.1）。daemon 方法不新增：`status` 多两个可选参数 `summary`、`workspaces`（daemon.md §4）。**修订 D19**：`ls --table` 成功时 stdout 是表格不是信封；出错仍是信封、退出码非 0。推迟到 daemon-independence r5：`--wait N`、`Busy starting` 不回退的语义、共享等待机制 | `status --all` 在 8 会话 42 工作区上要 30–60 s，大半是工作区与旧树的文件系统工作；列会话不需要这些。人看的列表要能直接读 | 2026-10-28 |
| D35 `--repos` 的提示与 `daemon status` 的 `fds` | `new --repos` 时 cli 在 stderr 写一行 `note: --repos tracks A,B; every configured repo still gets a workspace (pass --repo R for only these)`；`--repo` 时不写。`daemon status` 把 daemon 的 `fds` 原样放进 data。`new` 失败已回滚时错误带 `detail.rolled_back = true`（session.md D56），cli 原样透传 | 事故 2026-10-27：agent 把 `--repos tool` 当成只建 tool，得到 23 个工作区；`fds` 在 daemon 里有、cli 丢了。提示走 stderr，不改 data | 2026-10-27 |

初稿里被后写裁定压过的两句，不沿用：`grep` 不给 session 就是全部（改为 cwd 会话，`--all` 才是全部，design §6.3 与 T11）；`grep` 的 `--repo`（T11 只留 `--session` 与 `--all`）。crate 名从初稿的 `crates/strata-cli` 改为 types.md §8 的 `crates/git-strata`。

待定：上游要求 1 与 2 的方法名，若 daemon 定了别的名字，只改 §3 的方法列，不改命令行。`resolve` 仍没有 `Action` 变体。`chain --restore` 用已有的 `PushDeploy`，不等新变体。`config relax` 用 types.md 的 `Action::ConfigRelax`，不加码。
