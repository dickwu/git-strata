# `<module>` 模块

> **模板用法**（写模块文档时删掉本段）：把本文件复制为 `docs/modules/<module>.md`。九个二级标题原文照抄，不改字、不增删、不换顺序，Task 4–14 用 `diff` 核对。每节下面的"写什么"换成正文。类型名只用 [types.md](../types.md) 定义的名字和本模块 §3 声明的模块内部名，配置字段只用 [config.md](../config.md) 里的。

**Crate**：`crates/strata-<module>`（cli 是 `crates/git-strata`） · **设计来源**：design.md §… · **矩阵**：test-matrix 里目标模块为 `<module>` 的行

## 1. 职责与边界

> 写什么：本模块负责什么、明确不负责什么（各交给哪个模块），以及分别对应 design.md 的哪几节。

## 2. 依赖

> 写什么：直接依赖的 workspace crate 与外部 crate，各附一句理由；依赖必须符合 types.md §8 的依赖方向。另写明哪些模块会调用本模块。

## 3. 公开接口（Rust）

> 写什么：全部 `pub` 项的 Rust 签名（放在 rust 代码块里），每个函数写一句语义和它可能返回的 `Code`；跨模块类型只引用 types.md，不重新定义。

## 4. 数据与不变量

> 写什么：本模块拥有的持久数据（表结构、目录布局、文件格式），以及必须始终成立的不变量，每条注明它落实 design.md §9 的第几条。

## 5. 流程与算法

> 写什么：每个公开操作的编号步骤，标出加锁、先做 checkpoint、原子替换（`rename`）各在哪一步，并写出复杂度和 design.md §8 里对应的验收数字。

## 6. 失败与恢复

> 写什么：每种失败（参数错、崩溃、超时、打开的文件、磁盘满……）返回哪个 `Code`、留下什么现场、怎样恢复；结果不确定的按 design.md §9 不变量 6 处理。

## 7. 测试清单

> 写什么：每个测试一行，行格式固定为下表。`来源` 写矩阵行（`<来源>::<测试函数>`，与 test-matrix 的 `来源` 列写法一致），或写 `新增`。

| 测试名 | 断言 | 来源（矩阵行 或 新增） |
| --- | --- | --- |
| `<test_name>` | <这个测试断言什么> | <矩阵行 或 新增> |

## 8. 实现任务（TDD）

> 写什么：按实施计划（`docs/plans/`）的任务/步骤格式拆任务，§7 的每个测试都要落在某个任务的第一步里。每个任务按顺序做：先写失败的测试，再实现，再核对，最后 commit。格式如下：

### Task 1: <任务名>

**Files:**
- Create: `crates/strata-<module>/src/<file>.rs`
- Test: `crates/strata-<module>/tests/<file>.rs`

**Interfaces:**
- Consumes: <用到的 types.md 名字与上游模块函数>
- Produces: <本任务交付的签名>

- [ ] **Step 1: 写失败的测试**：<§7 的哪几个测试>
- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p strata-<module> <test_name>`
Expected: FAIL（<失败原因>）

- [ ] **Step 3: 写让测试通过的最少实现**：<要点>
- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p strata-<module> <test_name>`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add <具体路径>
git commit -m "feat(<module>): <一句话>"
```

## 9. 构建者决定记录

> 写什么：design.md 里标 [交给构建者]、并且落在本模块的决定。每条一行，写明决定的事项、选择、理由和日期；还没定的写"待定"。
