# #1433 会话 MCP 工具组合负控

`negative_controls.py` 变异 #1433 的生产组合面（`native_process/tools.rs` 的
`enabled_session_mcp_servers` / `SessionMcpTools::register` / `SessionMcpRelease::release` /
`session_mcp_tools` / `native_tools` 的 MCP 注册），逐条验证：正向先行通过、
变异体可编译、绑定的 `#1433 …` 断言以本轮 nonce receipt 失败、还原后复通过。

绑定关系（11 条）：

| 变异 | 语义（TS 基线） | 绑定测试 | 绑定断言 |
|---|---|---|---|
| `enabled-filter-dropped` | `getSdkMcpServers` 过滤 `server.enabled`（ProjectRuntimeRegistry.ts:322-325） | `native_process::tools::tests::session_mcp_tools_skips_disabled_servers` | `#1433 a disabled session server composes no tools` |
| `shared-conflict-replaced` | shared 工具 `!has` 才注册（SessionMcpRuntimeBundle.ts:62） | `…::register_keeps_shared_names_and_replaces_per_session_names` | `#1433 a shared session MCP tool keeps the existing builtin name` |
| `per-session-replace-lost` | per-session 工具 `has ? replace : register`（:140） | 同上 | `#1433 a per-session MCP tool replaces the same-name builtin` |
| `release-order-swapped` | 释放序 per-session → shared（组合序的逆） | `…::session_mcp_release_stops_per_session_before_shared` | `#1433 release order: per-session MCP before shared MCP` |
| `per-session-limit-off` | `size >= max` 超限跳过（SessionMcpRuntimeBundle.ts:70） | `…::session_mcp_tools_respects_the_per_session_limit` | `#1433 the limit skips the per-session server` |
| `per-session-limit-truncated` | 小数上限按 f64 比较、不截断为 usize（SessionMcpRuntimeBundle.ts:70；TS 不取整经 fix-r1 真 dist 探针实测，oracle 分数 cell 待 #2445） | `…::session_mcp_tools_allows_the_third_runtime_under_a_fractional_limit` | `#1433 a 2.5 cap still admits the third per-session runtime` |
| `per-session-cap-two-locks` | cap 的 size 读取与插入同一锁内、无 check-then-register 窗口（#2442 单临界区；DS 评审 P2 补配对变异，变异体读后以计数会合 + 有界自旋保证两次陈旧读都先于任一插入） | `…::session_mcp_tools_holds_the_cap_across_concurrent_prepares` | `#2442 concurrent prepares register at most one runtime under the cap` |
| `start-failure-propagated` | 启动失败降级为诊断（:147） | `…::session_mcp_tools_degrades_per_session_start_failures` | `#1433 composition survives the start failure` |
| `turn-registry-drops-mcp` | 组合工具进 turn 注册表（#1433 原始缺陷） | `…::native_turn_registry_carries_the_session_mcp_tools` | `#1433 the turn registry carries the session MCP tool` |
| `composition-reads-no-servers` | 组合读 enabled 表（生产读面） | `tests/sdk_mcp_session_tools.rs::discovery_composes_shared_and_per_session_tools` | `#1433 the shared lease tool is discovered` |
| `shared-stop-unlinks-provider-state-early` | shared stop 的 provider state 摘除在 awaited stop 之后（#2444 新观测窗口负控；字面「provider 锁持过 await」重排被 `shared_on` 的 Send 约束禁编译，见该轮 RESULT） | `…::session_mcp_release_stays_concurrent_with_registry_and_provider_dispose` | `#1433 the parked shared stop keeps its entry until it finishes` |

用法（提交后，由协调者在宿主跑；与 `native-registry-switch` 同款）：

```
CARGO_TARGET_DIR=<dir> python3 -I tests/fixtures/sdk-mcp-session-tools/negative_controls.py NEW_OUTPUT_DIR
CARGO_TARGET_DIR=<dir> python3 -I tests/fixtures/sdk-mcp-session-tools/negative_controls.py --check
```

- `mutating_sources` 以 `baseline="HEAD"` 守卫全部被读/被变异文件（tools.rs、
  assembly.rs、native_process.rs、tests.rs、review_tests.rs、registry.rs、sdk.rs、
  tools_mcp.rs、外部测试、公共 receipt/scratch/test_process/process_group/
  exact_receipt、mock_server.mjs、本夹具与 negative_process/negative_suite），
  因此**必须在 #1433 提交之后运行**——交付时源树未提交，全量 KILLED 轮由
  协调者收货时在宿主补跑（r2 RESULT.md 已注明）。
- 每个 `#[tokio::test]` 变异轮的正向/负向/还原三跑都经 `cargo-slots.sh` 名额闸。
- 单测体在 `mod tests` 内（缩进 4），外部测试在 crate 顶层——`test_body` 的
  函数尾锚按函数自身缩进回引（`(?P=indent)`），嵌套块的大括号不截断函数体；
  `check_binding_scope` 对两种布局各自反证。
