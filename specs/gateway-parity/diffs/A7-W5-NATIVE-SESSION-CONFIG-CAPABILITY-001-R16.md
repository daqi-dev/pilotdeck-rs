---
id: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R16
original: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001
---

## 类别

#2068 与完整 CLI 配置/会话目录的整合更正及历史来源澄清。main fe497364d59a3dcafde7e452de19ac281a6030ab 的既有 canonical R13 保留原字节。CLI 分支原同名未批准 R13 作为 R15 保全，审批状态保持未批准、非已接受。R14 保留原字节。

历史引用有两组映射。R14 所称“原 R13”指 CLI 来源、现保全于 R15 的提案。R15 旧正文中的“CLI R12”“最终行为以 R12 为准”指已保全为 R14 的 CLI 更正件。它们没有将 main 的 canonical R12/R13 改写成另一份文档。R15 frontmatter 的 original 仍指基础 A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001，原 CLI R13 原字节另有 SHA 与 Git 来源留档。

## 基线依据

固定 TS cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85。main #2068 原件 R13 记录的 path.resolve 非串项目键、Node argument_received、结构化 ERR_INVALID_ARG_TYPE 与描述阶段裸抛错、new_session truthiness 原 UTF-16 前缀继续作为对应修复依据。原件与既有机器数据由该 canonical R13 的路径、提交及 SHA 追溯。

CLI 目录/注册范围依据 R14 的真实 createLocalGateway 采集。已有未注册项目的 listSessions 返回空列表，recordAgentStatusMessage 可写独立冷转录，delete/rename/tag 先解析注册项目。原件 oracle SHA f0a13b35977bd6e2b6df3563f78ed2f76d9439a3ac838704121c779ca0a61374。R15 保全当前多项目与原 UTF-16 身份拒绝的未批准提案，不能由保全动作推定 TS 等价或人已接受。

两侧 R13 原字节不同。main canonical R13 SHA 8688aecbc63ccf8a6a8e83dd83af1050b6878b6d4477d19eda0795b1887961b3。CLI 来源旧 R13 SHA 92644ff65ae77f84b2406c5c4b599ceb8a82a4bc856383ea90fb332902ef2bd0，来源共同头 96ea28c58a998dcd93bb6f5e9e0a227c3a7052eb。CLI R14 SHA 60f3a12d5a7ba6566a808519f4c27386122ef1fa5aabe710b3adbc0cdcc91c9e。新 R15 的改名文件 SHA 及原字节快照路径由协调者收货后补入该轮登记记录。

## Rust 行为

两个请求族共用 projectKey 的 nullish/type 读取。nullish 回退到当前选中请求视图的项目。非字符串使用同源 argument_received/invalid_arg_type_message，保留 Node paths[0] 错误文案及 ERR_INVALID_ARG_TYPE。描述阶段自身抛错时保留裸 error.message 及原 gateway_request_failed 边界。

字符串以原 JsString/UTF-16 交给完整 CLI 已有解析器。config 模型写入与目录 delete/rename/tag 使用有界注册解析，styles 及目录 catalog/status 使用其已有词法解析。保留多项目能力、现有 SessionOwner 身份、原始代理单元拒绝和 bounded storage，避免提前替换编码误选另一个项目。当前不可表示项目仍按原单位返回 PROJECT_NOT_FOUND。原 main R13 中孤立项目 gate 的 CAPABILITY_UNAVAILABLE 描述只保留为其原来源事实，不能将旧 isolated scope 门重装到当前完整 CLI。

new_session 前缀按 main #2068 的普通 JS truthiness 和 String 转换构造，保留原 UTF-16 单元及描述抛错的裸文案。模型写 reservation、实际 owner 关闭以及异步写入的取消闭合继续使用本支既有完整流程。

## 范围

仅本轮 config.rs、session_dir.rs、methods/mod.rs 与 native-config 控制绑定的整合合同及 R13/R14/R15 引用身份。现 R13、R14 原文及 accepted-diffs.md 原字节保留。此件不扩展 Gateway 150、101 RPC、session50 分母，不改变 k、m 或冻结判分，不将旧证据提升为新共同头结果。R15 仍保留其未批准状态。

## 机器判据

新共同头须运行 main #2068 的非串项目键/裸描述抛错、string 真键、new_session truthiness/原单位及 throwing 前缀测试，同时保留完整 CLI 的注册项目、选中项目 styles、原单位未知项目、catalog/status 冷存储和 model owner/取消闭合测试。已有测试在冲突合并后按实际 registered/lexical 合同判定，不能保留旧 isolated gate 预期来覆盖完整 CLI。

合并候选此前仅在 cb8 私有目录通过 rustfmt、Python AST 和11条受影响控制的唯一代码锚/断言绑定检查，未编译或运行源码变异。新共同头的构建、正常工具/业务实测及两家非作者评审仍以该轮 source/bin/runtime-library 清单和原始日志为准。此前因自动安全审查停止的 A7 权限扩展及 Linux 变异保持停止，该记录不授权重试相关动作。

## 批准记录

整合与追加更正方案由本轮已授权产品范围内的协调者裁定，AI 代批、待人复核。R15 原未批准/非已接受状态完整保留，本 R16 草稿也不宣称验收或实效通过。root 收货时确认 canonical main 基线、两份原字节快照和更名后的唯一 ID，通过对应新头真实复算与双谱系评审后再按既有冻结程序登记。人签字及阶段完成另行核对。
