---
id: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R14
original: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001
---

## 类别

本件保留 CLI 来源的旧 R12（71dccc886），仅为避免与 main 已登记 R12 同名而顺延；原 R13 所说的 CLI R12 指本件。主线 R12 及原 R13 不改字节，本条未获接受的批准状态不变。

R11 的追加更正：目录只读与冷状态写入沿各自实际 CLI 合同；注册要求只用于原版明确要求注册的 dialog 写入。旧条目不修改。

## 基线依据

固定 cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85 在 mbp2 的真实 createLocalGateway 实测：已有但未注册 B 的 listSessions 返回空列表且零新增文件；recordAgentStatusMessage 返回 recorded:true，在 home/projects 的词法项目 id 下创建该会话 transcript/projections。再列目录仍为空（单独状态记录不成为可见对话）。四个所读源码 Git blob 前后校验，Node 22.22；原件主仓 `_tmp/20261108-cli-completion/review-session-consumers/ts-boundary/oracle.json`，SHA256 `f0a13b35977bd6e2b6df3563f78ed2f76d9439a3ac838704121c779ca0a61374`。采集器 `driver-parse-fix.mts` 只使用隔离 home 与本地拒绝连接的测试 provider。

## Rust 行为

list_sessions 与 record_agent_status_message 采用词法项目解析，前者不创建存储，后者允许未注册项目的冷 transcript。delete/rename/tag 继续在 session key 校验前验证注册项目。写者与文件路径栅栏保持一致，中央 SessionOwner 的已存在会话跨项目拒绝保持有效；不恢复 TS 只凭全局 sessionKey 把显式 B 状态写入 A 的混用行为。不可表示的 UTF-16 项目路径继续按原值拒绝，拒绝逻辑不做有损别名。

## 范围

更正 R11 的 list/status 过宽注册门，不改默认会话作用域、已有 owner 身份检查、一般 replay 或冻结尺子。原版 live 分支忽略显式 projectKey 的行为没有被宣称为安全隔离的等价行为；native 相关差异按本系列已有身份隔离记录复核。

## 机器判据

mbp2 运行当前源码的 `unregistered_catalog_and_cold_status_keep_actual_cli_contract`、`unregistered_project_never_creates_session_storage` 及 scope、active replay、history 同名会话隔离。实际 TS 复采：`PDRS_BASELINE=<固定资源根> node_modules/.bin/tsx <driver-parse-fix.mts> <新独占输出目录>`。编译、实测和必要独审未完成前不记通过；应绑定 source/bin SHA 与原始日志。

## 批准记录

AI 代批，待人复核；非已接受差异、不提供冻结判分豁免。恢复已实测旧 CLI 行为的代码继续开发，阶段与真人验收未完成。
