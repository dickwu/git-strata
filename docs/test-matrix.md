# git-strata — 测试矩阵索引

六个部分在 [test-matrix/](test-matrix/) 下，每条原测试一行。本索引的条数是从那些文件的表行数点出来的，不是计划里的估计。每个文件自己的「总计」与表行数一致，没有改行去凑计划里的 692 / 81 / 255 / 344 / 骨架 / 525。3f 的计划估计是 525（只算 `src`）；文件上是 635（`src` 525 + `tests` 110），「总计」也是 635。

状态词以单元格原文为准。五个词都出现了：保留、改写、不保留、依赖承担、替代。没有第六种。

## 六个部分

| 部分 | 文件 | 表行数 | 文件自己的总计 |
| --- | --- | --- | --- |
| 3a | [layerfs-store-layer.md](test-matrix/layerfs-store-layer.md) | 692 | 692 |
| 3b | [layerfs-project-telemetry.md](test-matrix/layerfs-project-telemetry.md) | 81 | 81 |
| 3c | [layerfs-workspace-layer.md](test-matrix/layerfs-workspace-layer.md) | 255 | 255 |
| 3d | [layerfs-replaced.md](test-matrix/layerfs-replaced.md) | 344 | 344 |
| 3e | [layerfs-sdk-js.md](test-matrix/layerfs-sdk-js.md) | 52 | 52 |
| 3f | [tgrep-core.md](test-matrix/tgrep-core.md) | 635 | 635 |
| 合计 | | 2059 | |

3f 前言里的分组（逐条分类 161、`src/managed/` 120、其余 `src` 244、`tests/` 110）加起来也是 635，与「总计」不冲突。

## 按状态

| 状态 | 条数 |
| --- | --- |
| 保留 | 19 |
| 改写 | 321 |
| 不保留 | 1287 |
| 依赖承担 | 152 |
| 替代 | 280 |
| 合计 | 2059 |

依赖承担只出现在 3f（152）。保留只出现在 3a（2）和 3f（17）。替代出现在 3a（44）、3c（48）、3d（173）、3e（15）。

| 部分 | 保留 | 改写 | 不保留 | 依赖承担 | 替代 |
| --- | --- | --- | --- | --- | --- |
| 3a | 2 | 64 | 582 | 0 | 44 |
| 3b | 0 | 20 | 61 | 0 | 0 |
| 3c | 0 | 59 | 148 | 0 | 48 |
| 3d | 0 | 72 | 99 | 0 | 173 |
| 3e | 0 | 13 | 24 | 0 | 15 |
| 3f | 17 | 93 | 373 | 152 | 0 |

## 按目标模块

目标模块取表的第三列。`—` 是不落到某个模块的行。

| 目标模块 | 条数 |
| --- | --- |
| apfs | 25 |
| cli | 13 |
| daemon | 40 |
| deps | 5 |
| gitio | 53 |
| overlay | 131 |
| publish | 43 |
| search | 22 |
| session | 42 |
| store | 56 |
| types | 3 |
| — | 1626 |
| 合计 | 2059 |

各部分的目标模块：

| 部分 | 目标模块（条数） |
| --- | --- |
| 3a | overlay 15、store 45、types 2、publish 1、daemon 3、— 626 |
| 3b | session 10、apfs 2、daemon 2、cli 6、— 61 |
| 3c | overlay 60、store 4、publish 7、apfs 3、types 1、— 180 |
| 3d | cli 4、gitio 10、overlay 27、publish 34、deps 2、daemon 34、session 27、search 1、store 5、apfs 1、— 199 |
| 3e | overlay 6、cli 3、session 5、daemon 1、publish 1、apfs 1、— 35 |
| 3f | search 21、deps 3、gitio 43、apfs 18、overlay 23、store 2、— 525 |

## 各模块 §7 里来源为「新增」的行

计的是 [modules/](modules/) 里每份文档 §7 测试表的来源单元格。来源以「新增」开头即计入，包括「新增（…）」和「新增<br>…」：括号里是为什么新增，不是矩阵行。来源单元格恰好是「新增」的另计一列。重复的表头行不计入。

| 模块 | 来源以「新增」开头 | 其中恰好是「新增」 |
| --- | --- | --- |
| [apfs.md](modules/apfs.md) | 109 | 109 |
| [cli.md](modules/cli.md) | 8 | 8 |
| [confirm.md](modules/confirm.md) | 21 | 5 |
| [daemon.md](modules/daemon.md) | 21 | 21 |
| [deps.md](modules/deps.md) | 50 | 50 |
| [gitio.md](modules/gitio.md) | 71 | 71 |
| [overlay.md](modules/overlay.md) | 29 | 3 |
| [publish.md](modules/publish.md) | 37 | 0 |
| [search.md](modules/search.md) | 10 | 10 |
| [session.md](modules/session.md) | 35 | 0 |
| [store.md](modules/store.md) | 76 | 76 |
| 合计 | 467 | 353 |
