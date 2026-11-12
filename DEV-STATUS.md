# Gateway 接续开发状态

2026-11-12 接续结论：摘取快照已恢复为真实 Git 开发，模块拆分、监听器关闭修复和渠道 HTTP/WS 原字节比较已实现。完整回归发现的测试接线问题正在收口；正式 Gateway 替换验收仍未完成，用户服务未切换。

最新执行约束：用户已要求停用 cb8，优先接续的 mbp2 已完成 e5 的 238 项 Python 控制、CMake 源码构建与依赖缓存准备，随后 SSH 连续三次超时，该线已挂起。macmini-01 已核有既有 Python 3.14.3 和固定 Node，在独占临时导出中承接本批 Python 验证与真实原件复算；未接管其他任务工作树。首轮在套件启动前因原件清单排序不一致失败，35 个路径及 SHA 均相同，原失败保留；修正排序后独占新目录实跑 123 项与同原件 16 反控全部通过，selector 变异准确判杀、恢复通过。cb8 不再访问；Rust 新构建及 Linux 专属验证仍未完成。迁移现场见 `_tmp/20261112-wave1-validation/mbp2-migration-handoff.json`、`host-switch-20261112.md` 及 `macmini-readonly-inventory/SUMMARY.md`。

## 唯一开发线与证据

- 本机树：`/Users/da/.codex/worktrees/gateway-wave1-completion/pilotdeck-rs`，分支 `codex/gateway-wave1-completion`，root 独占写入。
- mbp2 执行树：`/Users/da/ws/pdrs-worker-wt/gateway-integration-20261110/automation`，分支 `codex/gateway-wave1-mbp2`，复用已核归属的干净旧树；原分支和证据保留，独立 `_build/cargo-target`。
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
| HTTP/WS wire | `f1defa53e` 把实际 TCP connect、同 socket FIFO 请求响应、upgrade 与 frame 验证接入比较主路径；本轮按人批 #3062 仅对认证 key/派生 Accept/mask 作有限 v2 投影例外，全部原字节保留，非随机帧头、心跳、调用次数、未知 UUID 与同连接全序仍严格比较；批准代码已按精确哈希实测通过，完整独立双审与合法冻结登记待完成 |
| 错误隔离 | 单渠道源码或子进程探针的 I/O/超时失败落入该渠道结果，其余渠道继续产生结果；不由 producer 成功推断运行见证 |
| 测试接线修复 | 8 处旧源码锚点重新绑定实际实现，匹配数量与断言保持；11 个采集/打包/私有 C 副本实验按既有规则精确登记；完整自检结果单独记录 |
| 本批审查修复 | 补最终比较层的 ping/未知 UUID 反例、key 长度/编码与 101 业务头边界；补 Git PermissionError、严格 State/Pid、wire proof 缺字段/非对象拒绝矩阵。六条 API server evidence 改指实际入口，状态仍为 rust-unaccepted |
| 正文类型碰撞修复 | #3068 在 wire 与 HTTP exchanges 比较层给 JSON/text/bytes 加互斥类型包络，修复 chunked JSON `{"raw":"X"}` 与文本 `X` 误等；raw/models 业务 oracle、重复 key 后值覆盖、合法数值溢出拒绝保持；models/channelStreams 等内部比较字段统一包络，非法 JSON 常量按完整文本比较 |

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
| mbp2 Python 回归 | clean e5f9f58a4：68 wire + 22 来源 + 26 HTTP + 122 锚点控制，共 238 项通过；扫描 6226 锚点、0 错误，源码前后不变。此结果不含本轮 #3062 新增代码 | `mbp2-python-e5f9f58a4-r1/` |
| mbp2 构建准备 | r1 预检因容量缓存过期停止；刷新后 r2 fmt 通过，clippy 在编译前因离线缺源码包失败。现六包及完整索引已补齐，离线 metadata 297 包/节点通过；CMake 3.31.12 源码构建及锁定 Ninja 源码冒烟通过，未取得 Rust 新构建结果 | `mbp2-cargo-source-cache/SUMMARY.md`、`mbp2-cmake-source-build/SUMMARY.md` |
| #3062 随机值仲裁 | mbp2 同业务的两次真实 ws8.21.0 连接证实八处随机值差异。批准前候选对原件复算相同，16 反控全部符合预期；旧结果仍为 OBSERVED_NOT_AUTHORIZED。人已批准有限范围，macmini-01 已对批准消费者和三份审后测试候选实跑 75/22/26 全通过，同原件 16 反控符合预期；仍不是完整 TS/native 采集 | `mbp2-ws-arbitration/`、`approved-v2-macmini-r2/` |
| ws.send 帧粒度仲裁 | macmini-01 同版本/同源码 ws8.21.0：普通及 128KiB 消息每次 send 一帧；显式分片两次 send、两帧、一条接收消息。六格共 8 send / 8 数据帧 / 6 message，原解析器和清理通过；不代表完整 TS Server 或正式 Linux preload 通过 | `ws-send-frame-arbitration/SUMMARY.md` |
| macmini 审后测试候选 | 581 Git 导出加三份明确哈希绑定的测试覆盖文件：75 wire / 22 来源 / 26 HTTP 全通过；同原件 16 反控匹配，删去 WS 差异选择的实际 mutant 被精确断言判杀、恢复后通过。8277 源文件、35 捕获及工具前后不变，进程组已消失；r1 预检 FAIL 保留 | `approved-v2-macmini-r2/` |
| #3068 修复实测 | macmini 独占148 Git导出+consumer423a/test3778两份精确覆盖：78 wire +22来源 +26 HTTP共126 PASS；19真实HTTP原件两投影复算及旧16 WS控制通过。旧49b59的独立合成完整比较反例精确1FAIL（True != False），恢复423a同1PASS；8277源、35 WS原件、59 HTTP证据及工具前后不变、7自有子组清理 | `wire-value-validation/` |
| #3068 表示一致性补充 | 5f17精确源码+consumer2cc6/test7bb两覆盖：有改动的79项通过，同19HTTP/16WS复算符合预期，3子组清理、8277源与捕获/工具前后不变；未重复不变22/26。业务oracle仍原shape，新增空body/null/空字符串控制，空chunk为单终止 | `wire-comparison-followup-validation/RESULT.md` |
| macmini Darwin首次构建 | clean5f17完整8277文件Git导出，fmt PASS；严格clippy在GLib原生依赖intl处FAIL，wrap下载继续禁止。现有全局gettext未注入，正在补合法源码闭包；此无Git导出仅可作Darwin编译证据，不能交正式retirement binary | `macmini-source-dependencies/` |

266 项 ignored 已逐名分类，不计通过。r7 的 26 个自有容器均已移除，589 个小型原件与 cb8 逐字节一致。历史构建、权限、格式、变异编译和预检失败继续保留。

## 独立审查

r4 两谱系八角色均有有效 final，最高发现是 receipt Pid 形状 P1，已由 188249b11 修复并实测。逐条判断见 `review-r4-findings-triage.md`。

r5 八结果均保持 ERROR：七份因执行器把合法 canonical 模型名误拒，一份 DeepSeek boundary 是真实畸形响应。`3bf393ef1` 已最小修复模型名守卫，不改旧结果；随后由修复后的执行器接续 r6/r7 审查，结果分开保留。

r6 固定 4b855：GLM 仅 simplify 完成、其余三角色超时；DeepSeek 三角色完成、sibling 超预算，两家均未通过完整四角色闸门。有效原文共 P0=0/P1=1/P2=3；随机 handshake/mask P1 已由 mbp2 真实控制证实，并由用户明确批准 #3062 的有限例外。本轮只实施该例外；旧失败与原等级保留，获批后实测见上表，完整双审与登记仍未闭合。详见 `r6-findings.json` / `.md` 及 `state/decisions-20261112.md`。其余 P2 不由本次批准自动消除。

r7 的 #3062 增量在固定 581aaa767 上取得两家各四角色有效 final，P0/P1 均为 0；五条 P2 为冻结登记及四类反控覆盖；代码反控已补并在 macmini 实跑通过，冻结登记与修复复核待完成。历史范围 01/04/05 的补审另存：DeepSeek 两角色实际 1200 秒超时，不能视为完整双审；心跳次数/双向全序的时序疑点保留原 P1 待实测，不能借 #3062 删除或重排帧；六条 evidence P1 已修待复核。另一 STARTUP_CALL P1 有同字节实际锚点通过及子串匹配反证，待非作者重裁。逐条原文与证据见 `r7-findings.md`、`r7-historical-findings.md`、`old05-static-triage/REVIEW.md`。

r8 固定148的补审原文共62条P0/P1/P2表行，含重复发现及不完整角色，逐字保存在 `r8-findings.md`，不能将其计为62个独立问题或全门通过。GLM新发现的正文类型碰撞已由19格真实HTTP实证，独立#3068代码修复及上述126/19/16验证完成，待新提交双谱系增量复核。STARTUP_CALL、wireBindings与101 Date的分歧按有限原字节证据送第三谱系仲裁；后者两次完整真实101均无Date，不新增Date豁免。旧DeepSeek direct通道仅alias守卫拒绝canonical响应的失败保留，复用既有已修run-review-roles入口，不改工具和原评语。

r9两家各四角色完成，原2条P1/11条P2保留在 `r9-findings.md`。旧423a的独立实际仲裁已走完整合成compare_pair：binary=true的JSON字节/ff62得到[channelStreams,wireWebsocketStreams]差异；额外/v1/probe两种正文虽models局部相等，但exchanges与wireHttpStreams判差，最终均equal=false。见 `r9-comparison-facts/SUMMARY.md`，不是实际双引擎capture。后续小批统一全部内部比较正文表示并补空值控制，不改raw/models业务oracle，也不根据binary分支误读改协议标准；新79项及原件复算已过，等新提交两家增量复核。

## 还未闭合

1. mbp2 连通后在新源版本执行 Rust 构建、真实 negative_anchor_selfcheck 入口、83 项迁移负控、两项 SMS 长耗时测试及可执行的重锚控制；7 个 Linux 监听器负控仍缺合适环境。Linux 专属用例在 Darwin 的零匹配不得计通过；已取消的 cb8 批次不得补写 PASS。
2. 新 wire 路径的完整渠道真实 TS/Native 双侧采集与正式消费者复算；116 个测试不能代替全部渠道验收，media/full-production 缺项和 BLOCKED 状态保留。
3. 固定 TS 的 Telegram、Discord、Slack、钉钉源码 tsx 和官方 dist 实际遇到 ESM bare require 错误，且有可选 SDK 缺失。原错误保留，未注入全局 require 或替代 SDK。
4. FROZEN 旧 registration-base 与新增装置的合法复算、双审和登记；不能用直接重写哈希清除阻塞，#2961 的授权不扩展到无关标准变化。
5. 全部 150 入口、四 host、session、retirement、冷切回滚与真实使用验收。A7 已被安全审查拒绝的范围保持 NOT_RUN，不重试或绕过；人眼体验和最终签字仍待人工。
6. 最终源版本的独立双审、推送前 gitleaks 和 Draft PR 整理。所有必需条件闭合前不入合并队列、不宣称完整替换通过。

下一步提交表示一致性补充并做两家小增量复核；79项及19 HTTP/16 WS复算按精确文件SHA复用。macmini所有322 registry来源已核、CMake/pkgconf从源码私有构建，但首次真实Darwin严格检查暴露GLib intl闭包缺失，继续按锁定源码修复，不借全局库或解除wrap守卫。mbp2仍待恢复；Linux、完整渠道、冻结、宿主和退役验收分别按原规则推进。
