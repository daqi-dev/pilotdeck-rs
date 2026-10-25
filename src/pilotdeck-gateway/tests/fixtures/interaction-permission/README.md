# PermissionRequest 基线输出

oracle.json 来自只读 cc7c4e6 的 `src/gateway/permission/createGatewayPermissionHook.ts` 实际调用：allow + remember、无 sink、预取消三种情况。采集脚本及原始输出在本包证据 `_tmp/20261025-issue-586/pkg2/probe.mts` 和 `ts-hook.json`。使用基线现有 tsx loader，无安装。Rust 只提供 specific.decision 原语，sync hook 包装属于下一包。

完整 PermissionRuntime 的 67 个决定复用 ../permission/oracle.json；本包首轮重跑其 generate.mts，生成结果与该文件逐字节一致。原生 BashTool、WebFetchTool、WebSearchTool 的 ToolDefinition 直接转发已有 PermissionTool 检查；production_catalog_keeps_native_permission_checks 经 ToolRuntime 真实目录验证三者，并以不转发权限检查的 web 包装器证明 ask 会退化为 allow。三条 hook oracle 均比较完整 specific.decision 和 rules；remember 用同一上下文再 decide，验证 danger 从 ask 变为 session rule allow。

fix-r2 的 `generate.mts` 直接 import cc7c4e6：检查基线 HEAD/工作区及入口源码哈希，真实构造 SessionInteractionBundle、attach 和调用 permission.decide。输出 `profiles.json` 是八例原始输出的逐字节副本。headless 的 ask 在 profiled port 变为无 answerer 的 deny；disabled 的 ask 变为 profile disabled；普通 canPrompt=false 的底层 deny 保留长文案，forced MCP ask 才在 profile 层得到短文案。完整决定对象由 profiled_decisions_match_ts_bundle_including_native_denials 逐例机检。

该生成器也重采集原三条 hook（result/rules 均一致），并实测 emit 抛 Error、抛字符串及先答复再抛错/返回 false。先答复再返回 false 的实测结果与原 allow oracle 一致，由 synchronous_answer_wins_over_delivery_false 机检。Rust emit 是 Fn -> bool，异常差异没有视为通过或豁免，见 `specs/gateway-parity/diffs/A6.9-PERMISSION-RUN-001-R2.md`。panic 清理测试只证明 pending 注册不泄漏。采集命令在 generate.mts 顶部；本轮完整原始输出、源码哈希与测试日志在 `_tmp/20261025-issue-586/pkg2-fix2/`。
