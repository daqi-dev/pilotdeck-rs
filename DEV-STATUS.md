# Gateway 接续开发状态

2026-11-12 接续结论：摘取快照已恢复为真实 Git 开发，模块拆分、监听器关闭修复和渠道 HTTP/WS 原字节比较已实现。完整回归发现的测试接线问题正在收口；正式 Gateway 替换验收仍未完成，用户服务未切换。

最新执行约束：用户已要求停用 cb8，后续优先迁到 mbp2。cb8 本轮在途构建已安全取消，原件回存、容器移除、双锁释放；只完成 8/10 阶段，未产生完整 candidate。mbp2 是 Darwin arm64，已有 Rust 1.99 和固定 Node，但当前无可用 Linux 容器；其主仓由其他任务持有，不能接管。迁移现场见 `_tmp/20261112-wave1-validation/mbp2-migration-handoff.json` 与 `host-switch-20261112.md`。

## 唯一开发线与证据

- 本机树：`/Users/da/.codex/worktrees/gateway-wave1-completion/pilotdeck-rs`，分支 `codex/gateway-wave1-completion`，root 独占写入。
- 已停用的 cb8 树：`/home/clawbox-08/ws/pdrs-worker-wt/gateway-integration-20261110`，源码停在干净 `4b855f9ef`。此前构建测试均在 cb8；切换指令后不再启动新任务。本机继续编辑、读码、Git 和只读模型审查；新执行环境以 mbp2 实际核验为准。
- Rust 起点 `7d7c0ab2dad32b4c71bb2ed465147cdff179f374`，固定 TS `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。45 文件及 6 补丁逐 SHA 整合为 `1ddb12210`，原树和快照保留。
- 当前运行索引：`_tmp/20261112-wave1-validation/validation-latest.json`。每个结论均绑定自己的源版本、输入哈希、命令、退出码和原始记录；历史失败不覆盖，旧头通过不冒充新头通过。

## 已实现

| 范围 | 实际行为 |
|---|---|
| 模块拆分 | HTTP response/static_assets、attachments service、启动 options 独立成模块；路由、能力检查、提交生命周期、阻塞调度与参数顺序保留 |
| 监听器关闭 | API、SMS、webhook、wecom_callback、native WS、HTTP store、execute_code RPC 对共享 socket 执行 shutdown，修复 Linux 继承描述符使 stop 后仍可连接；保留各入口原 drain 时序，传播错误 |
| 公共 HTTP | 保留完整状态行、重复头、正文、同 socket 原字节与模型交换；receipt 的 State 必须是对象、Running 必须为 true、Pid 必须是严格正整数；启动与采集共用 240 秒预算 |
| 渠道 v2 | 两轮实际入站、权限/问答续接、reload、当前轮公开历史和停机落盘；绑定实际进程、依赖、角色源码和原保留目录 |
| HTTP/WS wire | `f1defa53e` 把实际 TCP connect、同 socket FIFO 请求响应、upgrade 与 frame 验证接入比较主路径；保留帧头、mask、心跳、调用次数及未知 UUID 差异，仅双方严格证据完整才清除两个 wire 缺项 |
| 错误隔离 | 单渠道源码或子进程探针的 I/O/超时失败落入该渠道结果，其余渠道继续产生结果；不由 producer 成功推断运行见证 |
| 测试接线修复 | 8 处旧源码锚点重新绑定实际实现，匹配数量与断言保持；11 个采集/打包/私有 C 副本实验按既有规则精确登记；完整自检结果单独记录 |

## 当前已取得的实测

| 检查 | 结果与版本边界 | 原始索引 |
|---|---|---|
| 同头十阶段构建 | c2c906246 全部 PASS：fmt、严格 clippy、四 bin、各指定 no-run 和 workspace inputs；no-run 不算测试运行 | `r5-build-results.json` |
| 完整隔离回归 r7 | c2c906246 实际 6654 passed、1 failed、266 ignored。唯一真实失败为 negative_anchor_selfcheck 的 19 处失配/未登记；另一个分片因嵌套 libtest footer 被旧汇总器误报。原总 FAIL 保留 | `r7-full-artifacts-manifest.json`、`r7-full-summary.json`、`r7-anchor-errors.txt` |
| 汇总器修复 | cb8 对 25 份原日志重新解析、17 个正负控制通过；此复算只纠正汇总，不把真实锚点失败改为通过 | `footer-replay-20261112T110915Z/` |
| 来源/HTTP/覆盖 | 188249b11 的来源 19 项、HTTP 26 项、覆盖 678 条通过；初次缺 TypeScript 依赖的失败保留 | `root188-checks-summary.json` |
| 完整 wire 比较 | f1defa53e 对应五文件精确 SHA 的独立导出：渠道 v2 68、来源 22、HTTP 26，共 116 项通过；真实 Node/ws 连接和三篡改反例另有原件 | `wire-integrated-checks-summary.json` |
| 源码锚点 | 原五脚本实际复现 8 个失配；修复后 72 个锚点全部匹配；未把静态匹配算作真实 mutant 判杀 | `source-anchor-checks-20261112T110851Z/summary.json` |
| 完整锚点自检 | 与 4b855 精确字节一致的独立 Git 导出：6226 锚点、0 错误，122 项控制通过；Rust 入口与真实 mutant 仍未运行 | `full-anchor-checks-summary.json` |
| 审查执行器 | cb8 完整既有测试与两模型×四种 canonical/alias/unknown 组合通过；只接受当前声明模型及其已登记展示名 | `review-model-alias-scratch/RESULT.md` |

266 项 ignored 已逐名分类，不计通过。r7 的 26 个自有容器均已移除，589 个小型原件与 cb8 逐字节一致。历史构建、权限、格式、变异编译和预检失败继续保留。

## 独立审查

r4 两谱系八角色均有有效 final，最高发现是 receipt Pid 形状 P1，已由 188249b11 修复并实测。逐条判断见 `review-r4-findings-triage.md`。

r5 八结果均保持 ERROR：七份因执行器把合法 canonical 模型名误拒，一份 DeepSeek boundary 是真实畸形响应。`3bf393ef1` 已最小修复模型名守卫，不改旧结果；当前代码须由修复后的执行器重新完成双谱系增量审查。

r6 固定 4b855：GLM 仅 simplify 完成、其余三角色超时；DeepSeek 三角色完成、sibling 超预算，两家均未通过完整四角色闸门。有效原文共 P0=0/P1=1/P2=3；P1 指独立连接的随机 handshake/mask 进入逐字节比较，需要 mbp2 真实控制仲裁。原等级与原件保留，不自行归一化、删帧或改冻结标准。详见 `r6-findings.json` / `.md`。

## 还未闭合

1. mbp2 合法独立目录与平台依赖接续、真实 Rust negative_anchor_selfcheck 入口、83 项迁移负控、两项 SMS 长耗时测试、7 个监听器和 8 个重锚变异控制。Linux 专属用例在 Darwin 的零匹配不得计通过；已取消的 cb8 批次不得补写 PASS。
2. 新 wire 路径的完整渠道真实 TS/Native 双侧采集与正式消费者复算；116 个测试不能代替全部渠道验收，media/full-production 缺项和 BLOCKED 状态保留。
3. 固定 TS 的 Telegram、Discord、Slack、钉钉源码 tsx 和官方 dist 实际遇到 ESM bare require 错误，且有可选 SDK 缺失。原错误保留，未注入全局 require 或替代 SDK。
4. FROZEN 旧 registration-base 与新增装置的合法复算、双审和登记；不能用直接重写哈希清除阻塞，#2961 的授权不扩展到无关标准变化。
5. 全部 150 入口、四 host、session、retirement、冷切回滚与真实使用验收。A7 已被安全审查拒绝的范围保持 NOT_RUN，不重试或绕过；人眼体验和最终签字仍待人工。
6. 最终源版本的独立双审、推送前 gitleaks 和 Draft PR 整理。所有必需条件闭合前不入合并队列、不宣称完整替换通过。

下一步在 mbp2 先执行平台支持的有界验证及 wire 随机性仲裁，再按实际环境推进剩余测试与采集；每项结果分别标明源版本和平台。
