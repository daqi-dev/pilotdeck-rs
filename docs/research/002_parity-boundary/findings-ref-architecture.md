# 参考仓架构速读 · codex-rs / goose / rust-sdk

> 2026-10-14。角色：**要学的**（只读架构）。由子 agent 产出，主会话抽查 3 条（标 ✔）。
> codex @ `6fbfc352`（2026-10-13，Apache-2.0）；rust-sdk 最近提交 2026-10-12（MIT→Apache-2.0 过渡）；goose 克隆未完成，本轮不评。
> 行号相对 `resources/codex/codex-rs/`。

## codex-rs 分层（117 个 crate，下面只列对应 PilotDeck 后端的）

| PilotDeck 后端里的 | codex-rs 里在哪 |
|---|---|
| agent 循环 · 上下文压缩 | `codex-core`：`run_turn`（`core/src/session/turn.rs:166`），压缩 `core/src/compact*.rs` |
| 工具定义与 schema | `codex-tools` |
| MCP | `codex-mcp`（连接与工具目录）· `codex-rmcp-client`（包 rmcp） |
| 沙箱 · 命令执行 | `codex-sandboxing`（seatbelt / landlock / bwrap）· `codex-exec-server` |
| 模型 provider | `codex-model-provider` · `codex-api`（Responses API + SSE） |
| 会话存储 | `codex-rollout`（JSONL + 索引）· `codex-state`（sqlite）· `codex-thread-store`（内存 / 本地两实现） |
| **对外协议（≈ gateway）** | 三个 crate 拆开：`codex-app-server`（服务）· `-protocol`（类型）· `-transport`（stdio / unix socket / websocket） |

## 值得学的三件

1. **协议类型以 Rust 为准，导出 TS 给前端。** 手写 serde 类型 + 宏生成 `ClientRequest` 枚举（`app-server-protocol/src/protocol/common.rs:212`）；`ts-rs` 导出 TS ✔（`export.rs:123`）、`schemars` 导出 JSON Schema（`:202`）；产物提交在 `schema/typescript`（96 个文件 ✔），有 schema fixtures 测试守住。
   → 对我们：PilotDeck 的帧载荷类型在 TS（`src/gateway/protocol/types.ts`，1666 行）。**反方向用**：先从 TS 类型导出 JSON Schema，作为 Rust 类型的对拍基准。
2. **不真调模型也能测 agent 循环。** wiremock 起假 Responses API（`core/tests/common/responses.rs:31, 107`；`app-server/tests/common/mock_model_server.rs:14`）、脚本化 SSE（`core/tests/common/streaming_sse.rs`）、insta 快照（`core/tests/suite/snapshots/` 96 个 `.snap`）；ID 靠事后剥离（`responses.rs:153` `strip_response_item_ids`）。
3. **时钟可注入。** `trait TimeProvider` ✔（`core/src/current_time.rs:19`），测试用 `tokio::test(start_paused = true)`。**ID 没有注入点**（`Uuid::now_v7()` 直调），靠剥离。

## rust-sdk（rmcp）—— 零件

客户端、服务端都能当（feature `client` / `server`）；传输 stdio、子进程、streamable HTTP（双端），SSE 仅客户端，WebSocket 被注释掉（`crates/rmcp/Cargo.toml:196`）。版本 3.5.1，规范 2026-07-28。codex 钉了 git rev `4048ac40` 且 `=3.5.1`（`codex-rs/Cargo.toml:450`）。

## goose（补读，`4376f40`，2026-10-13，Apache-2.0）—— 行号相对 `resources/goose/crates/`

| 项 | 在哪 |
|---|---|
| 核心 | `goose`：agent 与工具执行（`goose/src/agents/agent.rs`、`tool_execution.rs`）· MCP 客户端（`agents/mcp_client.rs`）· 会话存 sqlite（`session/session_manager.rs:24`）· ACP 服务端（`src/acp/`） |
| agent 循环 | `goose-agent`：状态机（`machine.rs:41` `Step`、`:55` `StateMachine`） |
| provider | `goose-providers` · `goose-provider-types`（`trait Provider`，`base.rs:520`） |
| 对外协议 | ACP（JSON-RPC）；手写 serde + `JsonSchema` derive（`goose-sdk-types/src/custom_requests.rs:33`）→ `generate_acp_schema.rs:361` 导出 JSON Schema → 前端用 openapi-ts 生成 TS（`ui/goose-acp-client/generate-schema.ts:9`） |
| 时钟 / ID 注入 | **没有**（`Uuid::new_v4()`、`Utc::now()` 直调） |

**codex 没有、值得借的一件：provider 层录制回放。** `TestProvider::new_recording(inner, file)` / `new_replaying(file)` ✔（`goose/src/providers/testprovider.rs:50, 59`）——真模型跑一次录下来，之后回放；MCP 也有录制回放（`goose-test/src/bin/capture.rs`，数据在 `goose/tests/mcp_replays/` ✔）。
→ 对我们：这正是「TS 基线跑一次真模型 → 录下 → TS 与 Rust 都用同一份录音回放」所需的那一层。

## 子 agent 的判断

- **首选样板：codex-rs**（分层直接对得上；ts-rs 导出类型；wiremock + insta + `TimeProvider`）。
- **备选**：goose 未取到，暂无；MCP 层可看 rust-sdk 的 examples 与 conformance。
- **风险**：① 过度工程——117 个 crate 里大量是 OpenAI 专有逻辑，照搬会把复杂度一起搬来 ② 版本漂移——rmcp 刚到 3.x，codex 钉 git rev ③ 协议不同——codex 是标准 JSON-RPC，我们是四种帧 + 约 100 个 method，要自己写映射；ID 确定性要自己补。复制 Apache 代码须保留 NOTICE。
