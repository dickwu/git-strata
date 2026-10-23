# publish

**Crate**：`crates/strata-publish` · **设计来源**：design.md §4、§6.3、§6.5、§6.7、§6.9、§9 不变量 4、6、7 · **矩阵**：3c 中目标为 `publish` 的合并/发布行，3d 的路径级合并行（inode oracle 改写成路径结果或标不保留）· **邻居版本**：gitio.md @1cd5ac5，deps.md @dafdefc，session.md @a324f9c，confirm.md @229f4e3，overlay.md 与 store.md @08ffa53，types.md 与 config.md 同 `docs/module-docs`

> 本文按 module-rulings.md 写：「T10 publish」，以及标了 T10 或 Everyone 的增补。发布链的判定不在本文重写：`deps::publish_chain` 是唯一一份（"The chain predicate lives in deps"）。锚的前移按 "The anchor follows origin along the chain — amended"。fetch、push 与链/锚点事务都在 repo 写锁里（"Every update of a ref main_tip reads runs under the repo lock"）。建造依赖集在占队列和拿锁之前（"Lock order, extended"）。第一次可见的写之后的等锁不受超时截断（"Waits after a call's first visible write are not bounded"）。观察主干失败只是警告 `ObserveFailed`。`ConfirmToken` 只引用 types.md §4，不重新定义。不加错误码。

## 1. 职责与边界

publish 把一个 `ReleaseGroup` 变成真实 git 仓库里的 commit，写在发布链 `refs/strata/publish/<branch>` 上，按配置推送，然后把参与的会话接到新基线。

- **做**：按路径做 hunk 级三方合并；在真实目录上跑只读检查；`commit_tree` 与比较并交换地前进发布链；推送；冲突时停下并让 `assign` 点名解决者；发布后 `rebase_session`。
- **不做**：不签发凭据（confirm）。不自己判定链领先、回退或分叉（deps）。不在持有 repo 写锁时 `build` 或 `ensure`。不把特殊 mode 位写进 git（T7）。不重放一次 `Unknown` 的推送。不跟随符号链接写 staging。

## 2. 依赖

| 模块 | 用到的 |
| --- | --- |
| deps @dafdefc | `publish_chain`、`main_tip`、`propose`、`ensure`（锁外）、`key_of` |
| gitio @1cd5ac5 | `fetch`、`merge3`、`commit_tree`、`update_ref`、`push`、`attach_staging`、`tree_of`、`is_ancestor`、`merge_base` |
| overlay @08ffa53 | `compute`、`rebase` |
| session @a324f9c | `move_workspace`（锁内）、`ensure_base_for`（锁外） |
| confirm @229f4e3 | 不调用。只核对 daemon 交来的 `ConfirmToken.action` |
| store @08ffa53 | 发布记录（`Unknown` 的推送留下，重启不重放） |
| types / config | `ReleaseGroup`、`Batch`、`PublishTarget`、`PushOutcome`、`Code`；`checks`、`publish`、`deploy_on_push`、`pin_unpushed` |

**对 daemon**（daemon.md）：占会话队列再拿 repo 写锁；`deploy_on_push` 或 `publish.confirm` 时，推送前经 confirm 签发 `Action::PushDeploy { repo, branch }`，再把凭据交给 `push`。推送超时后 status 报出 `Unknown`，不重放。fetch 之后按 deps §2 前移锚点，并在 `main_tip` 移动后观察主干。

**对 cli**（cli.md）：`publish`、`push`、`assign`、`diff`。需要确认的命令先 `require_tty`。被回退或分叉的链由显式命令处理（接受 origin 或恢复）；恢复只在 `is_ancestor(o, c)` 时是快进，gitio 从不强推。

## 3. 公开接口（Rust）

```rust
pub fn chain_ref(branch: &str) -> String; // "refs/strata/publish/<branch>"

/// 计划里的 merge_base。实现是 deps::publish_chain 的薄包装 [我的决定 D1]：
/// 只有 Leads 返回链顶，其余返回 origin 的 tip。None（还没 fetch）→ NoSuchBranch。
pub fn merge_base(git: &Git, cfg: &RepoConfig) -> Result<CommitOid>;

pub fn stage(group: &ReleaseGroup) -> Result<Staged, Conflicts>;
pub fn check(cfg: &RepoConfig, staged: &Path) -> Result<()>;
pub fn commit(git: &Git, staged: &Staged, parent: &CommitOid, message: &str) -> Result<CommitOid>;
pub fn push(git: &Git, cfg: &RepoConfig, confirm: Option<ConfirmToken>) -> Result<PushOutcome>;
pub fn assign(group: &ReleaseGroup, path: &RepoPath, session: SessionId) -> Result<()>;
pub fn rebase_session(session: SessionId, repo: &RepoName, new_base: BaseGen) -> Result<RebaseReport>;
pub fn diff_between(a: SessionId, b: SessionId, repo: &RepoName) -> Result<BetweenDiff>;
```

`Staged` 是 staging 目录的路径、合成后的 `TreeOid`，以及每个 repo 实际写入的路径。`Conflicts` 列出冲突路径和所属会话，整组不提交。`BetweenDiff { only_a, only_b, both }` 里 `both` 的每一项是 `(RepoPath, Merge3Preview)`。`RebaseReport` 要么是 `Moved`，要么是 `Waiting { proposal }`（新基线要另一个依赖集，会话留在旧基线上）。

## 4. 数据与不变量

- 一次 `ReleaseGroup` 里，每个 repo 至多出一个 commit。同一 repo 的批次按 `batches` 的顺序做三方合并。
- 合并按路径，不按 inode。目录、文件、符号链接的类型转换必须成对出现在同一批次里（overlay 的删 + 增）。缺了另一半 → `Conflict`。
- `.lww`：同一发布组里后应用的批次胜；同一会话里较晚的 `CheckpointId` 胜。
- 特殊 mode 位不进 git。可执行位可以。
- 发布链与锚点在同一个 ref 事务里写。锚点前移：`publish_chain` 为 Equal、Leads、Behind 或 Diverged，且 `git.merge_base(o, c)` 是 a 的后代又不等于 a 时，把 a 设成这个 merge-base（对旧 a 比较并交换）。Absent 与 Rewound 不前移。
- `deploy_on_push` 为真，或 `publish.confirm` 为真，推送前必须有 `Action::PushDeploy { repo, branch }` 的凭据。没有、用过、过期或 action 不符 → `NotConfirmed`，还没推。
- 推送超时：链留在本地，结果是 `PushOutcome::Unknown`，记入 publishes 表。重启只报出，不重放。
- 真冲突停下整组。`assign` 只点名解决者，不推送。
- 发布之后，已发布的路径从参与会话的 overlay 里去掉。新基线要换依赖集时，经 `deps::propose` 登记，会话留在旧基线上等 `deps approve`（不变量 4 优先于 design §6.4 最后一句）。

## 5. 流程与算法

一次 `publish(group)` 按 repo 分组。每个 repo：

1. **锁外**。`fetch` 不在这一步（它要锁）。先用已有的 `main_tip` 与 `publish_chain` 计划合并基线 B：只有 Leads 取链顶，否则取 origin 的 tip [我的决定 D1]。还没 fetch 过 → 先占锁做第 2 步，再回来计划。
2. **占队列，再拿 repo 写锁**。队列是本组要移动的会话，按 `SessionId` 升序。持锁时不等队列。锁里：`fetch`；按第 4 节前移锚点；再读一次 `publish_chain`。Rewound 或 Diverged：能把 a..c 干净重放到 o 上就重放（新链，a := o）[我的决定 D2：默认不自动重放，返回 `InvalidInput`，reason `chain_rewound` 或 `chain_diverged`，等人用显式命令]；否则同样拒绝，任何 commit 之前。`main_tip` 与计划里的 head 不一致 → `HeadMoved`（reason `main_moved`），什么都没写。
3. **staging**。在会话根下建 `.strata-publish-<repo>/`，真实目录，兄弟布局，相对路径与工作区相同。只放入本组选中的路径（`stage_only_selected_paths`）。每个路径做 hunk 级三方合并：base 是 B 的树，ours 是链或 origin 上的文件，theirs 是会话 overlay 的内容。hunk 不重叠 → 合并。同一 hunk → `Conflict`，整组停下，目录留下供人看，不提交、不推送。`gitio::merge3` 做字节合并。类型转换不成对 → `Conflict`。
4. **检查**。`RepoConfig::validate_commands` 先跑。会改文件的命令（`--write`，或 `cs-fix` 没有 `--dry-run`）→ `CheckMutates`，一条检查都不跑。然后在 staging 目录里跑 `checks`。非零退出 → `CheckFailed`（detail 带命令、退出码、输出末尾），不提交。配置了 0 条检查 → 不失败，结果里写 `checks_ran: 0` [我的决定 D3]。
5. **提交**。`commit_tree` 得到 commit，`update_ref(chain_ref, new, Some(expected))`。比较并交换失败 → `HeadMoved`，不重新合并、不重试。这一步是第一次对外可见的写。之后的失败是 `Unknown`。
6. **推送**。`publish.push` 为假 → 停在本地，返回已提交的 oid。要推：先核对凭据（第 4 节），再在同一把 repo 写锁里 `git.push`。`Pushed` → 锚点事务里 a := 推上去的 commit。`Refused` → `PushRefused`，链留在本地。超时或连接中断 → `PushOutcome::Unknown`，链留着，publishes 行记 `Unknown`，不重放（`push_timeout_keeps_chain_reports_unknown`）。
7. **放开锁之后** `observe_main`。失败只报 `ObserveFailed`，发布照常成功。
8. **rebase**。对每个参与会话，锁外 `ensure_base_for`。新基线的依赖集与会话当前的不同 → `deps::propose`，`RebaseReport::Waiting`，会话不动。相同 → 再占该会话队列、拿锁、`session::move_workspace`。已发布的路径从 overlay 去掉（`post_publish_rebase_drops_published_paths`）。rebase 要的 `ensure`/`build` 必须在占队列之前做完（"Lock order, extended"）。

`assign(group, path, session)` 只把这条冲突的解决者记成该会话。不合并、不推送。

`diff_between` 只读。两边 `overlay::compute`，只在一边有的路径进 `only_a` / `only_b`，两边都有的用 `merge3` 出预览。

## 6. 失败与恢复

| 情形 | 码 | 写了什么 | 之后 |
| --- | --- | --- | --- |
| 链 Rewound 或 Diverged，人还没处理 | `InvalidInput`（`chain_rewound` / `chain_diverged`） | 无 | 显式命令 |
| head 已不是 main tip | `HeadMoved`（`main_moved`） | 无 | 观察后重试 |
| 发布链比较并交换失败 | `HeadMoved` | 无新链顶 | 不重试、不重合并 |
| 同一 hunk 或类型转换不成对 | `Conflict` | staging 留下 | `assign` 后重来 |
| 检查命令会改文件 | `CheckMutates` | 无 | 改配置 |
| 检查非零退出 | `CheckFailed` | 无 commit | 改完重来 |
| 要确认而没有凭据 | `NotConfirmed` | commit 可能已在本地链上 | 确认后再 `push` |
| 远端拒绝 | `PushRefused` | 链在本地 | 人处理后再推 |
| 推送超时 | `Unknown` | 链在本地，行是 Unknown | 重启只报出，不重放 |
| 提交之后的 I/O 失败 | `Unknown` | 现场留下 | 不重放 |

等锁：占锁前的 `Busy` 是具名拒绝。`commit_tree` 之后的等锁一直等到拿到（"Waits after a call's first visible write are not bounded"）。

## 7. 测试清单

| 测试 | 断言 | 来源 |
| --- | --- | --- |
| `push_timeout_keeps_chain_reports_unknown` | 推送超时后链 ref 仍是新 commit，结果 `Unknown`，重启后 status 报出且不重放 | Review Focus 5 |
| `stage_only_selected_paths` | 未选路径不进 staging，也不进 commit | 计划 |
| `two_sessions_nonoverlapping_hunks_merge_clean` | 两个会话改同一文件的不同 hunk，合并干净，一个 commit | 计划 |
| `same_hunk_conflict_stops_whole_group` | 同一 hunk 两边都改 → `Conflict`，无 commit、无 push | 计划 |
| `check_mutating_command_is_config_error` | `--write` 或没有 `--dry-run` 的 `cs-fix` → `CheckMutates`，检查进程没启动 | 计划 |
| `merge_base_prefers_local_chain_when_ahead` | 链 Leads 且锚在 origin 上 → B 是链顶；无锚、Rewound、Diverged、落后 → B 是 origin | 计划 + 锚裁定 |
| `deploy_on_push_without_confirm_stops_before_push` | 无凭据 → `NotConfirmed`，`git.push` 没被调用 | 计划 |
| `post_publish_rebase_drops_published_paths` | 发布后参与会话的 overlay 不再含已发布路径 | 计划；3d `:252` |
| `zero_checks_reports_zero_ran` | 没配置检查 → 成功，`checks_ran = 0` | 裁定 |
| `rebase_waits_when_deps_must_change` | 新基线要另一个依赖集 → `Waiting`，会话仍钉旧基线 | 不变量 4 |
| `publish_does_not_build_while_holding_repo_lock` | `ensure`/`build` 的调用全部在占队列之前 | "Lock order, extended" |

3d 里按 inode 判合并的行（`merge_by_serial`、`merge_root_oracle` 的 13 种）不保留 inode 语义：每一种在本表或上表里有一条路径级结果，或标不保留并写明「按路径，不按 inode」。依赖目录不参与合并（3d 里 unit 行：替代，归 deps）。

## 8. 实现任务（TDD）

- [ ] **Step 1: 写失败的测试**：§7 的十一行。
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-publish --test publish`
Expected: FAIL（函数未定义）

- [ ] **Step 3: 写让测试通过的最少实现**：§5 的八步。`merge_base` 只调 `deps::publish_chain`。staging 用真实目录和 `gitio::attach_staging`。检查前先 `validate_commands`。推送在 repo 写锁里。超时留链。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-publish --test publish`
Expected: PASS

- [ ] **Step 5: Commit** — `feat: publish chain, checks, and rebase`

## 9. 构建者决定记录

| 决定 | 选择 | 理由 | 日期 |
| --- | --- | --- | --- |
| D1 `merge_base` 的形状 | 薄包装，只在 Leads 时返回链顶 | 计划要这个名字；裁定要求判定只有 deps 一份。两份判定会在锚上分叉 | 2026-10-23 |
| D2 不自动重放回退或分叉的链 | 返回 `InvalidInput`，等人用显式命令 | 自动重放会把被人拿掉的提交推回去。干净重放是否自动留给那条命令，不留给 `publish` | 2026-10-23 |
| D3 零条检查 | 成功，并写出 `checks_ran: 0` | 静默跳过会让人以为检查跑过了 | 2026-10-23 |
