# daemon

**Crate**：`crates/strata-daemon`（lib + bin `git-stratad`）· **设计来源**：design.md §5、§6.2、§8、§9 · **邻居版本**：session.md @a324f9c，deps.md @dafdefc，publish.md（本阶段 T10），confirm.md @229f4e3，gitio.md @1cd5ac5，store.md 与 overlay.md @08ffa53

> 本文按 module-rulings.md 写：「T13 daemon」，以及标了 T13 的增补。一台机器一个 daemon。方法表是 cli 子命令的对照，名字一致。`Busy` 只用于第一次可见的写之前的等锁（缺省 30 秒）。写之后的等待不受这 30 秒截断。重启报出 `Unknown` 的发布，不重放。不加 `Code`：`Failed` 与 `UnknownMethod` 是 RPC 层的回答。

## 1. 职责与边界

daemon 是本机唯一的写入口。cli 和钩子经 unix socket 叫它。

- **做**：持有每 repo 一把写锁和每会话一条队列；打开唯一一个 `Sessions`；跑检查点、发布、推送、gc；在图形会话里经 confirm 弹框；重启后报出不确定的发布。
- **不做**：不自己签发凭据的规则（confirm 签发，daemon 核对 action）。不在回调线程里 `flush` 任何一个 Watcher。不持有 Tracker 的同时去等队列或 repo 锁。不重放 `Unknown`。

## 2. 依赖

消费全部上游。本文件点名 daemon 必须照做的几条：

1. 整个生命周期只 `Sessions::open` 一次。cli 只在 daemon 没在跑时自己打开（session §2）。
2. 要队列又要 repo 锁：先按 `SessionId` 升序占队列，再拿锁。持锁时不等队列。
3. `ensure` / `build` 在占队列之前做完（"Lock order, extended"）。顺序是建造锁 → 会话队列 → repo 写锁。
4. fetch、push、链与锚点事务都在 repo 写锁里。任何 fetch 之后用 `publish_chain` 判定要不要前移锚点；让 `main_tip` 动了的 fetch 之后还调 `observe_main`。
5. `new` 的 `ObserveFailed` 之后再观察一次。`index-read-back-refused` 或 `staging-changed` 时告警，请人看主 checkout 的 `git status`。
6. 发现 `<shadow>/strata-quarantine/` 里有条目，或 attach 把链接挪进去：告警。不在这种影子仓库上跑 `git gc`（"A symlinked worktrees/<n> is quarantined, never kept — amended"）。
7. 持锁超过 30 秒就在 status 里写出谁持着哪把锁、从什么时候起。
8. launchd 没有用户的 PATH。`install` 和 `checks` 用本文写明的 PATH：`/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin` [我的决定 D1]。

## 3. 公开接口（Rust）

RPC 是 JSON 行。请求 `{ "id", "method", "params" }`，回答 `{ "id", "ok", "data" | "error" }`。`data` 在成功时总是有。socket 是 `~/Library/Application Support/git-strata/daemon.sock`。

方法与 cli 子命令同名：

| method | 作用 |
| --- | --- |
| `new` | `session::new` |
| `status` | 读，不排队 |
| `checkpoint` | 记检查点 |
| `restore` | 恢复到一个检查点 |
| `grep` | `search::grep` |
| `files` | `search::files` |
| `diff` | `publish::diff_between` |
| `deps_propose` | 提议依赖集 |
| `deps_approve` | 确认后铺开 |
| `deps_list` | 列出提议 |
| `publish` | `publish::stage` 到 `commit` |
| `push` | `publish::push` |
| `assign` | 点名冲突解决者 |
| `rm` | 删除会话 |
| `gc` | `store` 的 gc |
| `daemon_status` | 锁、队列、Unknown 发布 |

未知方法 → RPC `UnknownMethod`，不是 `Code`。参数对不上 → RPC `Failed`。上游的 `StrataError` 放在 `error` 里，码用变体名。

launchd：`Label` = `com.gwd.git-stratad`，`KeepAlive` 为真，装在 GUI 会话（Aqua）里，这样 LocalAuthentication 弹得出框。

## 4. 数据与不变量

- 每 repo 一把写锁。缺省等 30 秒。只在这次调用还没写任何对外可见的东西时，超时返回 `Busy`。写了之后就等到拿到。
- 同一会话的 checkpoint、restore、`rm`、`move_workspace` 串行。进行中的 checkpoint 不取消，后来的请求排队。
- status 不排队，不等写锁。
- publishes 表里 `Unknown` 的行，重启后由 `daemon_status` 和 `status` 报出，不重放、不删除。
- 凭据在交给 session 或 publish 之前，daemon 核对 action 与这次请求一致，并且 confirm 说它还没用过、没过期。不符 → `NotConfirmed`。
- 不持 Tracker 去等队列或锁。检查点要等锁时，先放开 Tracker。

## 5. 流程与算法

**启动**

1. 打开 store，`session` 的 `recover`，然后才 gc，然后才听 socket。
2. `Sessions::open` 一次。
3. 扫 publishes 的 `Unknown` 行，记进内存，供 status。不调用 `git push`。
4. 每小时：全量对账（每个工作区冷扫一次）和 gc。与请求共用同一把 repo 锁和同一条会话队列。

**请求**

1. 读一行，对上方法。
2. 只读方法（`status`、`grep`、`files`、`diff`、`deps_list`、`daemon_status`）直接跑。
3. 写方法：若这是该会话已有的 checkpoint，排队，不取消。否则占队列（要锁的话先队列后锁），调用上游。
4. `deps_approve` 与 `push`：先要求 cli 已做 `require_tty`（daemon 不查 TTY，它查的是凭据）。弹出 confirm，核对 action，再调用。
5. 回答一行 JSON。

**告警**

`move_workspace` 或 attach 返回的 `Unknown` 带 `index-read-back-refused` 或 `staging-changed`，或隔离区非空：写进 status 的 warnings，并在 stderr 打一行给人看。不自动改主 checkout。

## 6. 失败与恢复

| 情形 | 回答 | 之后 |
| --- | --- | --- |
| 未知方法 | RPC `UnknownMethod` | 调用方改方法名 |
| 参数坏 | RPC `Failed` | — |
| 写之前等锁超时 | `Busy` | 重试 |
| 写之后等锁 | 一直等，status 报持锁者 | 人看 status |
| 会话正在做 checkpoint | 排队 | 做完再跑 |
| 凭据不符 | `NotConfirmed` | 重新确认 |
| 上次推送不确定 | status 报出，不重放 | 人决定 |
| daemon 没在跑 | cli 的只读命令仍可用 | 见 §7 |

## 7. 测试清单

| 测试 | 断言 | 来源 |
| --- | --- | --- |
| `concurrent_checkpoints_same_session_serialize` | 同一会话两个 checkpoint 同时到，第二个排队，两个都完成，顺序与到达一致，没有一个被取消 | 计划 |
| `unknown_publish_reported_on_restart_not_replayed` | publishes 里有一行 `Unknown`；重启后 status 含它；`git push` 没被调用 | 计划 |
| `cli_readonly_commands_work_without_daemon` | daemon 没听 socket 时，`status`、`grep`、`files` 仍返回 JSON（cli 直接读）；`publish` 与 `push` 失败并说明 daemon 没在跑 | 计划 |
| `status_does_not_wait_on_write_lock` | 写锁被占住时，`status` 在 1 秒内返回 | 裁定 |
| `busy_only_before_first_write` | 调用还没写时，锁超过 30 秒 → `Busy`；已经写过的调用在同样的持锁下等到拿到，不返回 `Busy` | 裁定 |
| `quarantine_is_an_alert` | 隔离区有一条链接时，status 的 warnings 点名它，且这次启动没有对那个影子仓库跑 `git gc` | 裁定 |

## 8. 实现任务（TDD）

- [ ] **Step 1: 写失败的测试**：§7 的六行。
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-daemon --test daemon`
Expected: FAIL

- [ ] **Step 3: 写让测试通过的最少实现**：§3 的方法表、§5 的启动与请求、§4 的锁。plist 用 §3 的 Label。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-daemon --test daemon`
Expected: PASS

- [ ] **Step 5: Commit** — `feat: daemon rpc, locks, and unknown-publish report`

## 9. 构建者决定记录

| 决定 | 选择 | 理由 | 日期 |
| --- | --- | --- | --- |
| D1 显式 PATH | `/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin` | launchd 没有用户 PATH。这台机器的 bun 与 git 在 Homebrew。配置以后可以覆盖，缺省用这一条 | 2026-10-23 |
