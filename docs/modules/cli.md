# cli

**Crate**：`crates/strata-cli`（bin `git-strata`）· **设计来源**：design.md §6 · **邻居版本**：daemon.md（T13 方法表），types.md 的 JSON 约定，confirm.md @229f4e3

> 本文按 module-rulings.md 写：「T14 cli」。`require_tty` 在这里，不在 daemon。每个子命令对应 daemon 的一个方法。成功与失败都是一行 JSON：`{ "ok": bool, "data": …, "error": … }`。`data` 总是有，空时是 `{}`。零命中的 grep 是空列表。确认类命令在非 TTY 上、在连接 daemon 之前拒绝。

## 1. 职责与边界

cli 是人与钩子的入口。它不持有 repo 锁，不弹 LocalAuthentication 框（那是 daemon 经 confirm 做的）。它做的是：检查 TTY、把参数收成 RPC、把回答打成一行 JSON。

daemon 没在跑时，只读命令直接读 store 与工作区。写命令失败并说明要先 `git-strata daemon start`。

## 2. 依赖

| 模块 | 用到的 |
| --- | --- |
| daemon | §3 的方法表。名字一一对应 |
| types | `Code` 的变体名就是 `error.code`。信封见 §4 |
| confirm | 不调用 `require`。只在确认类命令上先 `require_tty` |

环境变量与 flag 都不能跳过确认（`no_env_or_flag_bypass` 在 confirm 的测试里枚举；cli 不再提供这种 flag）。

## 3. 公开接口（Rust）

子命令：

| 命令 | daemon 方法 | TTY | 确认 |
| --- | --- | --- | --- |
| `git-strata new` | `new` | 否 | 否 |
| `git-strata status` | `status` | 否 | 否 |
| `git-strata checkpoint` | `checkpoint` | 否 | 否 |
| `git-strata restore` | `restore` | 否 | 否 |
| `git-strata grep` | `grep` | 否 | 否 |
| `git-strata files` | `files` | 否 | 否 |
| `git-strata diff` | `diff` | 否 | 否 |
| `git-strata deps propose` | `deps_propose` | 否 | 否 |
| `git-strata deps approve` | `deps_approve` | 是 | `DepsApprove` |
| `git-strata deps list` | `deps_list` | 否 | 否 |
| `git-strata publish` | `publish` | 否 | 否 |
| `git-strata push` | `push` | 是，当仓库要确认 | `PushDeploy` |
| `git-strata assign` | `assign` | 否 | 否 |
| `git-strata rm` | `rm` | `--discard` 时是 | `RemoveDiscard` |
| `git-strata gc` | `gc` | 否 | 否 |
| `git-strata daemon start` | — | 否 | 否 |
| `git-strata daemon stop` | — | 否 | 否 |
| `git-strata daemon status` | `daemon_status` | 否 | 否 |

`git-strata daemon start` 用 `launchctl bootstrap` 装上 `com.gwd.git-stratad`，不等待业务。`stop` 用 `bootout`。

参数要点：

- `new --repos a,b --ticket T`。ticket 只是标签。
- `checkpoint --session S --tool-call ID`。同一个 ID 再来一次是同一条检查点（幂等）。
- `grep <pattern> [--session S] [--repo R]`。不给 session 就是全部。
- `rm --session S [--discard]`。`--discard` 会丢掉未发布改动，所以要确认。
- `push <repo>`。`deploy_on_push` 或 `publish.confirm` 时要 TTY 和凭据。

Claude Code：`PostToolUse` 跑 `git-strata checkpoint --tool-call "$TOOL_USE_ID"`。`SessionStart` 打印一行：daemon 是否在跑、当前会话 id（有的话）。安装：把 `git-strata` 放在 PATH 上；`git strata` 是同一二进制的别名 [我的决定 D1]，不另写一套子命令。

## 4. 数据与不变量

每一行输出：

```json
{"ok":true,"data":{},"error":null}
{"ok":false,"data":{},"error":{"code":"Busy","detail":{}}}
```

- 一行，紧凑，无空格以外的换行。`data` 总是在。
- `error.code` 必须是 types.md `Code` 的变体名。cli 自己不发明码。
- stdout 写失败（管道关掉）按 3b 的写法：退出码非零，能写 stderr 就写一行，不假装成功。
- 非 TTY 上的确认命令：不连接 daemon，不发 RPC，退出码非零，`error.code` 是 `NotTty`。

## 5. 流程与算法

1. 解析子命令。未知子命令 → `InvalidInput`，并把已知子命令写进 `error.detail.commands`。
2. 确认类：`require_tty`。失败即返回，不到第 3 步。
3. daemon 在听：写一行请求，读一行回答，原样映射到信封（上游的码不变）。
4. daemon 没在听：`status`、`grep`、`files`、`diff`、`deps list`、`daemon status` 走本地只读路径。其余返回 `error.code = "Busy"`，`detail.reason = "daemon_down"` [我的决定 D2：不新加码]。
5. `checkpoint --tool-call ID`：先看本会话是否已有这条 ID 的检查点。有则返回那一条，不再请 daemon 记第二条。

## 6. 失败与恢复

| 情形 | 码 | 连了 daemon 吗 |
| --- | --- | --- |
| 非 TTY 上的确认命令 | `NotTty` | 否 |
| 未知子命令 | `InvalidInput` | 否 |
| daemon 没跑，且是写命令 | `Busy`（`daemon_down`） | 否 |
| 上游拒绝 | 上游的码 | 是 |
| stdout 已关 | 无 JSON（写不出） | 视命令而定 |

## 7. 测试清单

| 测试 | 断言 | 来源 |
| --- | --- | --- |
| `every_command_emits_ok_envelope` | §3 每个命令的成功输出都能解析成 `{ok, data, error}`，且 `data` 不是 null | 计划 |
| `error_codes_match_types_md` | 故意触发的每个 `error.code` 都在 types.md 的 `Code` 里 | 计划 |
| `hook_checkpoint_is_idempotent_per_tool_call` | 同一个 `--tool-call` 跑两次，检查点 id 相同，store 里只有一行 | 计划 |
| `confirm_commands_refuse_non_tty` | `deps approve`、要确认的 `push`、`rm --discard` 在非 TTY 上返回 `NotTty`，daemon 没收到请求 | 计划 |
| `readonly_works_without_daemon` | 与 daemon 的同名测试同一形状：没 daemon 时 `status` 仍有信封 | daemon §7 |

## 8. 实现任务（TDD）

- [ ] **Step 1: 写失败的测试**：§7 的五行。
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-cli --test cli`
Expected: FAIL

- [ ] **Step 3: 写让测试通过的最少实现**：§3 的表、§4 的信封、§5 的五步。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-cli --test cli`
Expected: PASS

核对：`rg -o 'git-strata [a-z]+' docs/modules/cli.md | sort -u | wc -l` 至少 14。

- [ ] **Step 5: Commit** — `feat: git-strata cli envelope and confirm gate`

## 9. 构建者决定记录

| 决定 | 选择 | 理由 | 日期 |
| --- | --- | --- | --- |
| D1 `git strata` | 同一二进制的别名 | 计划要求能当 git 子命令装。两套实现会分叉 | 2026-10-23 |
| D2 daemon 没跑 | `Busy`，reason `daemon_down` | 不加码。人能从 reason 看出要先启动 | 2026-10-23 |
