# PermissionRequest 基线输出

oracle.json 来自只读 cc7c4e6 的 `src/gateway/permission/createGatewayPermissionHook.ts` 实际调用：allow + remember、无 sink、预取消三种情况。采集脚本及原始输出在本包证据 `_tmp/20261025-issue-586/pkg2/probe.mts` 和 `ts-hook.json`。使用基线现有 tsx loader，无安装。Rust 只提供 specific.decision 原语，sync hook 包装属于下一包。

完整 PermissionRuntime 的 67 个决定复用 ../permission/oracle.json；本包再次实跑 generate.mts，生成结果与该文件逐字节一致。新增测试的原生 BashTool 通过 PermissionDefinition 进入同一个 ToolDefinition catalog，保留原 checkPermissions 与执行/验证方法。
