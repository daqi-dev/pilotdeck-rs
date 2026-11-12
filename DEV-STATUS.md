# Gateway 接续开发状态

2026-11-13 01:09 CST：当前开发分支为 `codex/gateway-wave1-completion`；本批接续33e修复HTTP比较，准确提交身份见本树Git HEAD和末尾机器索引。Gateway 产品实现及定向修复持续推进，**完整替换验收未完成，未合并、未切用户服务**。本文件只列当前结论；历史失败和过程记录留在 board 与原始证据，不以新结果覆盖。

唯一写入树为 `/Users/da/.codex/worktrees/gateway-wave1-completion/pilotdeck-rs`，root/Codex 持有。子代理仅写本轮 `_tmp/` 私有候选，由 root 核基点和差量后应用。当前两条产品侧阻断为 #3076（HTTP 比较漏判）与 #3077（完整源码输入闭包拒绝）；#3076已收三份实测候选，尚待提交、两家终审与合法冻结；#3077仍为私有候选。

## 执行环境

- 用户已要求停用 cb8；本轮取消后不再访问，旧成绩只作历史证据。
- mbp2 连续三次 SSH 超时，已挂起，等待用户确认恢复；不重复连接。
- macmini-01 承接独占临时源码导出/真实浅 Git 检出的验证，私有 HOME/TMPDIR/target，Cargo 每台最多2个。只从源码补齐依赖，没有下载预编译程序，也未接管其他工作树。
- 本机仅编辑、Git、哈希/原件核验和模型评审，不执行产品构建或测试。
- Darwin 成绩不替代 Linux `/proc`、dupFD、正式 retirement 合同。A7 已被安全审查拒绝的范围继续 NOT_RUN；未绕过。

## 已有真实结果

| 范围 | 当前可支持的结论 | 本轮证据目录（均在 `_tmp/20261112-wave1-validation/`） |
|---|---|---|
| #3062 有限 WS 例外 | 用户仅批准合法 key、匹配的派生 Accept、4字节 mask。已实现；原始字节及其它字段/次数/心跳/全序保留。两家独立增量无P0/P1；真实原件16反控匹配。未等同正式全渠道验收 | `approved-v2-macmini-r2/`、`mbp2-ws-arbitration/` |
| #3068 正文类型与编码 | 当前 consumer SHA `2cc6a942…`，test SHA `2f1b29d2…`；80项套件真实PASS，19份真实HTTP/16份WS控制按精确输入复用，不重复计跑。不变的来源22/公共HTTP26原成绩继续有效 | `wire-comparison-followup-validation/`、`utf8-full-pair-controls/` |
| #3072 Darwin GLib 源码闭包 | 固定上游 proxy-libintl 源码并保持 nodownload；fmt/四包 all-targets 严格 Clippy PASS，52库/别名/profile/依赖来源通过。29个成功调用共37项Rust测试终端footer核准；首次SMS缺sidecar失败保留，正常构建sidecar后原入口PASS | `darwin-libintl-source-candidate/` |
| 原 negative_anchor_selfcheck | 真实1ea5 Git树上的3项Rust全部PASS，122扫描控制、42进程控制PASS，6226锚点0错误。不是83个实际变异结果，也不是完整 workspace 闭包PASS | `macmini-negative-anchor-1ea5/` |
| #3076 真缺陷 | 1212份历史A2原件认证，基线完整validate+compare PASS；只把单侧SSE正文改为JSON字符串并同步两chunk，仍最终PASS。该结果为尺子P0；新候选经macmini报告32单位及真实SSE/health/write正反控通过，140份原件已逐SHA回存，待双审与合法冻结，仍禁止报完整验收通过 | `frozen-http-string-facts/` |

以上构建/测试各自绑定其实际源：#3072是5f17全导出+精确五路径候选，与1ea5全部Rust/Cargo相关字节等价但不是同一Git身份；3项Rust自检是独立真实1ea5检出；80项是1ea5导出+唯一测试覆盖。未将任一局部结果写成当前HEAD全仓通过。所有旧FAIL/PARTIAL及NOT_RUN保留。

## 独立审查与处置

- #3072：GLM、DeepSeek各四角色有效final，无P0/P1。两条来源JSON/计数P2已在33e补齐；33e的DeepSeek增量复核无P0/P1。MiniMax四项额度429原件保留。
- 原“HTTP与WS必须共socket、priorHttp不得为空”的P0：GLM明确撤销这一给定主张，规范只要求HTTP事务内部绑定；完整pair与四原字节反例支持。其余receipt风险未自动消除。明确答复见 `two-claim-factual-answer/RESULT.md`。
- standard HTTP组件JSON字符串/普通文本折叠P1由第三谱系维持，逐条承接OPEN #3076；该组件例子的entry前置会拒绝，不能与真实SSE最终P0混为一谈。
- 33e新增四条测试覆盖P2已逐条承接OPEN #3079，前置于A8 channel-v2正式退役或阶段关闭；尚未实施。原r8/r9/r10其它条目与零观察P2保留，不能以一次总VERDICT代处置。

## 下一步与未完成边界

1. #3076：共享有类型的HTTP正文投影，SSE/NDJSON保留完整结构/顺序，沿用已批准的锚定UUID映射；真实基线仍PASS、同一SSE变异必须最终FAIL。另以实际原件核验HTTP JSON bool/number等值疑点；只有证实才收最小修复。不扩大#3062相等例外。
2. #3077：完整workspace扫描拒绝已pin MuPDF宏include，以及后续已登记OUT_DIR生成来源。接通完整2869文件+manifest证明，其他宏/包/cfg仍扫描；私有候选和完整根实测待完成。
3. 上述前置过后继续原83项迁移变异、两项SMS慢测试和重锚控制；7个Linux监听器负控缺可执行Linux环境。Darwin零匹配不算通过。
4. 固定TS的Telegram/Discord/Slack/钉钉仍有官方依赖/ESM入口阻断；完整真实TS/native渠道、媒体、150分面、四host、session、冷切回滚及真实使用验收未闭合。没有注入全局require或替代SDK冒充固定基线。
5. 合法冻结：旧登记与新增成员须逐条接到真实校准、双谱系评审和事件链，登记基点 `8639283b7a9b692e986bd9c78cd7968c597c2075`。不改历史apparatusSha、不直接重写哈希消除阻塞。#2961及#3062只按各自人批范围处理。
6. 最终全范围双审、CI与PR整理、合并队列和人签字仍未完成。Draft #2786 属旧分支，不冒充当前分支交付。

最新机器索引：`_tmp/20261112-wave1-validation/validation-latest.json`。板上历史数值以各段实际输入和时间为准，不累加为新头完整成绩。
