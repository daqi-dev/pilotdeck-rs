# Hooks 生产绑定金样

`probe.mts` 通过现有 tsx loader 直接 import cc7c4e6 的 HookRuntime、LifecycleRuntime、GatewayPermissionHook 和 AsyncHookRegistry；共享 baseline_source_guard 校验源码树，oracle 内记录 commit 与相关源文件 SHA256。不安装依赖，不用 socket，不写基线。

从仓库根运行（stdout 保存到新的证据文件，不覆盖 oracle）：

```sh
PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck node --import /Users/da/ws/pilotdeck-rs/resources/PilotDeck/node_modules/tsx/dist/loader.mjs src/pilotdeck-gateway/tests/fixtures/interaction-hooks/probe.mts
```

三条 PermissionRequest 场景完整比较 lifecycle result 与 remembered rules：配置 command 在 callback 之前，allow/deny/no-sink，toolUseId/input fallback，首个合法 allow_session、无效候选、toolName fallback、pattern 和去重。异步场景比较完整 dispatch、首次/重复 complete、完整 collect 与重复 collect；唯一归一化字段 pendingAsyncHooks.startedAt → `<time>`。命令真实执行 printf，无模型/网络 mock。

`difference` 单独实测 TS 以同 id 覆盖注册后接受旧 id completion；它不作为 Rust 期望。Rust 的 opaque receipt 绑定 run 与 descriptor generation，差异见 `specs/gateway-parity/diffs/A6.9-HOOKS-RUN-001.md`（未批准）。测试在 `runtime/project/interaction_hooks/tests.rs`，factory 接线与 rollback/lease 测试在 `runtime/project/factory_tests.rs`。运行 `cargo test -p pilotdeck-gateway --lib interaction_hooks -- --test-threads=4`。

配置来源遵循 TS `ProjectSessionRuntimeBundle.ts:380` 的 session contributions.hooks：宿主保留 plugin contribution lease，将已解析 HooksSettings 传入本 generation 的 bind_turn，并传入同代同 session/run 的 TurnPermission。不能用配置中不存在的顶层 config.hooks；SDK HTTP hook selector 与远端 invocationId registry 的组合仍属后续宿主接线。Interaction marker 本身不安装任何端口。

真实 TERM 测试只验证既有 shell 的 TERM 清理被执行、runtime job 完成后才能报告 drain；保留 exec.rs 自己的延迟进程组 KILL/reaper 语义，不把 runtime release_count 等同所有后代进程均已退出。真实 provider/UI 效果待人工验收。
