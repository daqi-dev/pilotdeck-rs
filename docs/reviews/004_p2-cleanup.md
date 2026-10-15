# #118 P2清账接续审计

范围：#101（四项）、#95（Set merge与资源观察）、#76（隐藏URL断言）、#105（入口与历史证据追溯）。原Claude Opus候选958ec6c由Codex/GPT同步main5eeac27并补廉价P2，旧工作树/分支/未跟踪原件保留。#91与#112不在本PR，继续跟踪。

## 独立闸门

GLM独立simplify四视角；DeepSeek按实际pr-ready技能分别完成sibling、concurrency、boundary角色，逐个独立会话留产物。初轮提出证据路径、上游空Connection响应边界、双编码疑点；P2全部当轮处理，不另开债务。后两项依据真实静态服务函数最小实测与回归，不凭代码猜测或协调者口述降级。最终GLM/DeepSeek增量终审均P0/P1/P2=0，独立读实测原件后确认双编码问题不适用；CI放行见[PR118](https://github.com/zhengdaqi/pilotdeck-rs/pull/118)。原始报告和命令输出的唯一任务目录为`_tmp/20261016-p2-resume/`，角色报告`.claude/notes/pr-ready/{sibling,concurrency,boundary}.md`，终审`final-{glm,deepseek}.jsonl`。超时日志和窄续会话全部保留，partial未被当作通过。

| 发现 | 分类/处置 | 证据 |
|---|---|---|
| router首跑101原因不明 | 已解释并保留原始输出：offline源crate缺缓存；11个已有源包经Cargo.lock SHA验证复用，无下载 | router-cache-miss.log、cache-inputs.json |
| 历史360测量无确定路径 | README补受保护旧树精确路径与历史定性；launch exit0/reaped，日志DONE360；不冒认为新HEAD | exp012-historical.json、旧wt-p2c原件 |
| 上游空Connection缺响应侧测试 | 四类坏值及正常SSE头保留回归；最终8/8 | prefix-router/src/main.rs:379、router-tests-final.log |
| 单解码可能放行双编码穿越 | 实际Rust guard＋真实Express static/send＋真实Vite static/sirv＋mock 8组无listen实测；字面百分号目录匹配、外部哨兵未泄漏。保留单解码并补双编码URI保持负控；独立终审复核原件 | double-decode/report.md、result.json、probe.mjs |
| P3可读性/重复helper/精确文本提取/本机证据路径 | 不在本轮范围，已知留白，不开issue | 各角色与终审原文 |

## 规则和真实输入验证

- workspace fmt、yaml33项通过（2资源测试ignore单独测）、workspace/router全目标clippy `-D warnings`；prefix-router最终8/8。完整命令与退出码在validation-exits.json/final-router-exits.json，原始输出同目录。
- Node22.22.0/yaml2.9.0独立重录86条；原44不变，新42；89632字节与fixture逐字节一致，SHA256 `aae5f3d2d687a4c4d7118adeca96f833dcbaff0325581c8387db444e03e443f3`。
- exp007两控与F2离线42项通过；实际系统Chrome隔离headless重跑streaming-scroll，legacy/timeline各4共8/8。原件`_tmp/fe-parity/20261016-p2-streaming-scroll/`；只测old，new为NOT_RUN，不代表前端阶段验收。
- #95真实数据只复算第12族anchors_merges，N由Node-only定为10/40、每规模3次独立进程；中位耗时比0.606/0.727、绝对RSS比0.785/1.090、相对耗时增长1.199、增量RSS比1.258，原阈值通过。[完整原始采样](004_p2-cleanup-resources.json)，适用范围和以后重跑要求见yaml README。其余11族未冒认重测。
- 旧wt-p2c仍958ec6c、none73文件与保留快照逐项同hash；五工作树，总约29.24GiB＜46GiB。冻结README完整SHA不变，与FROZEN前16位一致。

## 逐项收尾与边界

#101四项分别对应prefix-router点段/空Connection拒绝、build-coexist.sh三条离线锁定构建和exp007已有浏览器证据文案；#95对应Set merge的Undefined/迭代语义/86条参照和真实第12族资源复算；#76对应隐藏URL零断言与实际spec AST负控，200行虚拟化搜索依既有P3处置留白；#105对应当前launcher入口和历史360次确定位置，其2/3/6已由#111处理、4废止、5/7仍在#112。逐issue证据与占位审计最终评论见PR/issue。

已有占位`streaming-scroll.spec.mjs:15`的new NOT_IMPLEMENTED归F5，`scripts/a5-19/run.py:36`的e1Integration NOT_IMPLEMENTED归#112/E1/A19-HOST；本PR无新增占位，不关闭相应阶段。D6、archive、A4、C历史51/53与真人实效验收均不因本PR改变。合并前仍必须最终HEAD两门CI通过，合并用rebase并核服务端提交邮箱。
