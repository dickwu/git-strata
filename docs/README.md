# git-strata 文档

git-strata 让一台 Mac 上的多个 Claude Code 会话，各自在真实的 APFS 目录里修改同一组仓库。会话之间共享不可变的基线和依赖，每一步都记 checkpoint，彼此能看到对方改了什么，最后按批次合成 git commit。

现阶段只写文档：所有模块文档和测试矩阵都写完之前，不写代码（design.md 开头的约束）。文档用中文写正文，标识符保持英文。

## 阅读顺序

1. [design.md](design.md)：设计本身，包括目标与反目标、环境约束、词汇表、架构、关键流程和不变量。它是唯一的约束来源，其他文档与它冲突时以它为准。
2. [types.md](types.md)：跨模块共享的名字，包括标识、结构、错误码、JSON 输出约定、crate 布局与依赖方向。后续文档只用这里的名字。
3. [config.md](config.md)：配置文件的格式与校验规则，以及 inter 工作区每个兄弟仓库的默认配置（Task 2）。
4. [test-matrix.md](test-matrix.md)：测试矩阵的索引，记录 LayerFS 和 tgrep 的每一条测试映射到哪个模块、怎么处理；各部分的明细在 [test-matrix/](test-matrix/) 下（Task 3a–3f、Task 15）。
5. [modules/](modules/)：逐模块的开发文档，每份都按 [_template.md](modules/_template.md) 的九节来写，其中包括 TDD 实现任务（Task 4–14）。
   - [apfs.md](modules/apfs.md)：`clonefile` 克隆、`getattrlistbulk` 遍历、用 gen count 判断变化、FSEvents 监听、大小写规则。
   - [gitio.md](modules/gitio.md)：干净的 git 子进程封装、`.git/index` 解析、commit 与 push、三方合并。
   - [store.md](modules/store.md)：SQLite 元数据，以及按 BLAKE3 寻址的 blob 存储。
   - [overlay.md](modules/overlay.md)：根据基线和当前目录计算 overlay，写 checkpoint，执行 restore。
   - [deps.md](modules/deps.md)：依赖集 generation 的认领、冷装、漂移判断、变更提议和 rollout 计划。
   - [session.md](modules/session.md)：会话清单与生命周期、兄弟布局、按 cwd 识别会话，以及让基线和依赖一起移动的 `move_workspace`。
   - [publish.md](modules/publish.md)：批次、发布组、staging 树、检查、发布链、推送，以及发布后的 rebase。
   - [search.md](modules/search.md)：基于 tgrep-core 的跨会话 `grep` 和 `files`。
   - [confirm.md](modules/confirm.md)：人类确认通道，用 LocalAuthentication（Touch ID 或登录密码）签发和校验确认凭据；TTY 检查在 cli 里。
   - [daemon.md](modules/daemon.md)：launchd agent、unix socket RPC、锁，以及监听和对账的调度。
   - [cli.md](modules/cli.md)：子命令、JSON 输出和 Claude Code 钩子入口。

## 本阶段计划

[plans/2026-10-22-module-docs.md](plans/2026-10-22-module-docs.md)：模块文档阶段的任务拆分、每个任务的接口约定和核对命令。
