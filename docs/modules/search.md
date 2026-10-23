# search

**Crate**：`crates/strata-search` · **设计来源**：design.md §6.3 · **邻居版本**：overlay.md @08ffa53，session.md @a324f9c，tgrep-core 的 `SHARED_WORKTREE_INDEXES.md` §"Implemented worktree synchronization API"

> 本文按 module-rulings.md 写：「T11 search」。索引的建立、读取与查询由 tgrep-core 承担（依赖承担）。strata 只用它的共享基线模型：基线索引按 git tree OID 复用，每个会话一份带墓碑的 overlay。`ViewId` 用 types.md 的定义，不另造。零命中是空列表，不是错误。不加错误码。

## 1. 职责与边界

search 让一个会话，或全部会话，在「基线 + 本会话 overlay」上看 grep 和文件列表。

- **做**：挂上视图、卸下、按变更路径刷新、grep、按 glob 列文件。
- **不做**：不建索引格式（tgrep-core）。不用 tgrep 的托管 v2 存储、`stale_reads`、`.ignore` / `p4ignore.ini`、Windows 路径和它的 CLI 选项。不看别的会话的未发布改动，除非调用方要「全部」。

## 2. 依赖

| 模块 | 用到的 |
| --- | --- |
| tgrep-core | `WorktreeView::new`、`invalidate_path`、`refresh`、`with_snapshot`、`files`。基线索引的键是（repo 身份，tree OID）。引用 `SHARED_WORKTREE_INDEXES.md` 那一节，不抄它的实现 |
| overlay @08ffa53 | `Overlay`：本会话的增、改、删（墓碑）、改名 |
| session @a324f9c | `resolve`：从 `SessionId` 找到工作区根 |

视图排除工作区里的 `.git` gitfile，以及 strata 自己的 `.strata/` 树。

## 3. 公开接口（Rust）

```rust
pub fn attach(session: SessionId, repo: &RepoName) -> Result<ViewId>;
pub fn detach(view: ViewId) -> Result<()>;
pub fn refresh(view: ViewId, changed: &[RepoPath]) -> Result<()>;
pub fn grep(which: Which, pattern: &str, opts: &GrepOpts) -> Result<Vec<Hit>>;
pub fn files(view: ViewId, glob: &str) -> Result<Vec<RepoPath>>;

pub enum Which { One(ViewId), All }
pub struct Hit {
    pub session: SessionId,
    pub repo: RepoName,
    pub path: RepoPath,
    pub line: u32,
    pub text: String,
}
```

`ViewId` 是 types.md 的那个。`GrepOpts` 只有大小写是否敏感；别的标志不收 [我的决定 D1]。

## 4. 数据与不变量

- 基线索引按 `BaseGen.tree` 复用。两个会话钉同一个 tree，共用一份基线索引，不各建一份。
- 每个视图有一份只属于这个会话的 overlay：新增和修改盖住基线同路径；墓碑只在这个会话里藏起基线文件。别的会话看不见这批墓碑，也看不见这批未发布内容。
- 改名是墓碑加新路径，与 overlay 的记法相同。
- 查询在 `with_snapshot` 里做。快照前后核对工作区根的身份。对不上 → `Busy`（reason `root_changed`），不返回半份结果。
- 零命中返回空 `Vec`，不是错误。未知的 `ViewId` → `NoSuchView`。

## 5. 流程与算法

**attach**

1. `session::resolve` 得到工作区根。不存在 → `NoSuchSession` 或 `NoSuchRepo`，原样透传。
2. 基线索引：已有这个 `(repo, tree)` 的就借用；没有就让 tgrep-core 为这个 tree 建一份。这一步是缓存填充。
3. `WorktreeView::new` 挂上工作区。拒绝独立克隆、裸仓库、子目录根，与 tgrep 的契约相同 → `InvalidInput`。
4. 记下 `ViewId`。返回前做一次 `refresh`（全量）。失败 → 不留下视图 id。

**refresh**

对 `changed` 里的每个路径 `invalidate_path`，再 `refresh()`。空切片等于没有提示，tgrep 会核对它认为变了的文件。overflow 或说不准 → `invalidate_all` 再刷新。视图不存在 → `NoSuchView`。

**grep / files**

`One`：在该视图的快照上查。overlay 条目盖住基线同路径；墓碑使基线文件不出现。`All`：对每个已挂视图各查一次，每条 `Hit` 带上 `session` 与 `repo`。零命中是空列表。`files` 用 walker 的文件名成员，不用倒排表，与 tgrep 的 `files` 相同。glob 语法用 tgrep 已有的；匹配不到是空列表。

**detach**

卸下视图，不影响共享的基线索引。最后一个使用者离开后，基线索引仍留着，按 tree OID 给以后的会话用。进程退出时全部卸下。

## 6. 失败与恢复

| 情形 | 码 | 写了什么 | 之后 |
| --- | --- | --- | --- |
| 会话或仓库没有 | 透传 `NoSuchSession` / `NoSuchRepo` | 无 | 调用方改参数 |
| 根不是这个 repo 的工作区 | `InvalidInput` | 无 | — |
| 未知视图 | `NoSuchView` | 无 | 重新 attach |
| 查询中途工作区根被换 | `Busy`（`root_changed`） | 无结果 | 重试 |
| 零命中 | 成功，空列表 | 无 | — |
| 索引读失败 | `Unknown` | 视图留着，标为未就绪 | 下一次 refresh 全量核对 |

search 不持 repo 写锁，也不进会话队列。它只读工作区。

## 7. 测试清单

| 测试 | 断言 | 来源 |
| --- | --- | --- |
| `overlay_entry_shadows_base_path` | 会话改了 `a.ts` 之后，grep 命中会话的内容，不是基线的 | 计划 |
| `tombstone_hides_base_in_this_session_only` | 会话删了基线文件后，本视图的 files/grep 没有它；另一个钉同一 tree 的会话仍然看得到 | 计划 |
| `grep_all_tags_each_hit_with_session` | `Which::All` 的每条 Hit 都有 session 与 repo；两个会话各有一处命中时返回两条 | 计划 |
| `base_index_shared_across_sessions_same_tree` | 两个会话钉同一个 tree，基线索引只建一次（同一份 id） | 计划 |
| `grep_zero_hits_is_empty` | 没有匹配 → `Ok(vec![])`，不是错误 | 裁定 |
| `unknown_view_is_no_such_view` | detach 之后再 grep → `NoSuchView` | types.md |

3f 里索引格式、读取与查询的行是依赖承担，不在本表重写。tgrep 的 Windows、`.ignore`、托管 v2 存储标不保留。

## 8. 实现任务（TDD）

- [ ] **Step 1: 写失败的测试**：§7 的六行。
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-search --test search`
Expected: FAIL

- [ ] **Step 3: 写让测试通过的最少实现**：§5。基线索引用 `(repo, tree)` 做键。查询走 `with_snapshot`。
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-search --test search`
Expected: PASS

- [ ] **Step 5: Commit** — `feat: search over base index plus session overlay`

## 9. 构建者决定记录

| 决定 | 选择 | 理由 | 日期 |
| --- | --- | --- | --- |
| D1 `GrepOpts` 只有大小写 | 不收 tgrep CLI 的其余标志 | 裁定明确不用那些 CLI 选项。以后要再加，改这一行 | 2026-10-23 |
