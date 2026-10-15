# #121 产品入口覆盖与阶段D计划审计

作者：Codex/GPT；非作者模型：Z.AI Coding Plan GLM-5.3、DeepSeek Account Flash high。审阅固定基线cc7c4e6；按小批读取源代码、元数据及文档，后续轮只审增量。具体scope和独立实测在本机 `_tmp/20261016-entry-ledger/`；每个pr-ready角色均有独立原文，`_tmp/20261016-entry-ledger/pr-ready/{sibling,concurrency,boundary}.md` 与各增量产物。原文是本地过程证据，不作为CI依赖。

## 不变量与证据

| 不变量 | 证据与结论 |
|---|---|
| 固定基线与覆盖集合 | 独立git show/AST枚举678条，账本无漏/重/无主项；源码哈希68个；101 method、263 HTTP、19 channel、两个bin独立断言 |
| 外部对账与负对照 | 263 HTTP与既有runtime集合相等；678逐项删项均FAIL；重复/多项/坏引用/源码/哈希/组类型/空组/未知Express形态均注错；check PASS、6 tests OK |
| 边界和资源 | 不import/启动基线产品；Git每次30s，整体60s进程组kill/reap；失败不skip；原五worktree保留 |
| 分阶段归属 | groups连接具体stage/issue/plan/acceptance；D0装置不挂产品；D依赖gateway的真实集成在A8后、桌面编排B6；真人使用未代判 |
| CI强制门 | 实时保护API含check与gateway-schema-baseline（strict）；新覆盖步骤挂后者；原始输出附PR描述 |
| 冻结语义 | 本次无任何已冻结文件改动；阶段C完整哈希检查通过；不重定阈值或新增批准差异 |

## 评审与处理

GLM simplify覆盖复用/简化/效率/层级；复用现有TypeScript lockfile、CI固定基线和runtime种子，实测运行约1–3秒。其唯一P2为保护规则待验，实时API已确认无需修改设置。DeepSeek按sibling/concurrency/boundary分别实际审阅，非泛化合并报告替代。

| 问题 | 取高等级 | 处理 |
|---|---|---|
| SMS自带HTTP未列项 | P1 | 新增监听/可配POST路径两条、源码哈希及关键负对照，678项对齐 |
| 覆盖job可能非必需检查 | P1待验 | 实时API证实两job均required；已消除前提疑点 |
| 后台/监听锚点命中import或注释 | P2 | 全部改真实start/watch/forward/createServer调用；bin指针也钉实际bin映射 |
| Git无单次超时/孤儿进程 | P2 | 30s单次kill与60s进程组kill/reap |
| 未知Express形态、任务引用过宽 | P2 | Router绑定/别名/动态链fail；计划/验收路径限制，专用负对照 |
| 空product组、非dict组异常 | P2 | kind区分装置/产品、未使用组fail、坏类型统一错误，负例通过 |
| intent空行、修复未入index/board计数过期 | P2 | 人手意向先提交保存，仅清空行；新段记678并将全部代码/记忆纳入提交 |

无新增P2跟进债务。P3已知留白不在本轮范围：管理选项与命令profile有语义交集、部分证据为占位源码；行号身份绑定固定基线，基线升级时重新枚举；非Express工厂新形态需重新对账。这些不代表行为验收通过。

## 大步骤收尾自查

#121的产出为 `specs/product-coverage/`、`scripts/product-coverage/`、计划006与#122–#130归属、CI接线及本审计。范围内无TODO/unimplemented/空分派。`src/pilotdeck-cli/src/main.rs:2` 仍只输出版本，已在总账记placeholder并归D任务；它是后续实现缺口，不伪称本轮迁移完成。A4局部本地化、C历史51/53 FAIL、B/F未退役和真人验收均保持原边界。#121无前置于本节点而未清的P2；两家终审和CI完成后才合并关闭。

最终尾差由GLM与DeepSeek分别独立复核：P0/P1/P2均为0，可提交PR；两家均重跑check/test，核全部修复/记忆已在index、678计数与实际监听/bin行号一致、五工作树保留。CI仍须远端双门绿后合并。
