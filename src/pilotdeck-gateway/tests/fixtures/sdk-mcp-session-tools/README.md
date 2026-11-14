# #1433 会话 MCP 工具组合负控

`negative_controls.py` 变异 #1433/#2442 的生产组合面（`native_process/tools.rs` 的
`enabled_session_mcp_servers` / `SessionMcpTools::register` / `SessionMcpRelease::release` /
`session_mcp_tools` / `native_tools` 的 MCP 注册，及 `runtime/mcp/registry.rs` 的
`register_below_cap` 临界区复检），逐条验证：正向先行通过、
变异体可编译、绑定的 `#1433 …` / `#2442 …` 断言以本轮 nonce receipt 失败、还原后复通过。

绑定关系（9 条）：

| 变异 | 语义（TS 基线） | 绑定测试 | 绑定断言 |
|---|---|---|---|
| `enabled-filter-dropped` | `getSdkMcpServers` 过滤 `server.enabled`（ProjectRuntimeRegistry.ts:322-325） | `native_process::tools::tests::session_mcp_tools_skips_disabled_servers` | `#1433 a disabled session server composes no tools` |
| `shared-conflict-replaced` | shared 工具 `!has` 才注册（SessionMcpRuntimeBundle.ts:62） | `…::register_keeps_shared_names_and_replaces_per_session_names` | `#1433 a shared session MCP tool keeps the existing builtin name` |
| `per-session-replace-lost` | per-session 工具 `has ? replace : register`（:140） | 同上 | `#1433 a per-session MCP tool replaces the same-name builtin` |
| `release-order-swapped` | 释放序 per-session → shared（组合序的逆） | `…::session_mcp_release_stops_per_session_before_shared` | `#1433 release order: per-session MCP before shared MCP` |
| `per-session-limit-off` | `register_below_cap` 临界区复检 `>= cap`（#2442：等号档拒绝——竞态测试的档位是 total=5、cap=5.0，TS `5 >= 5` 拒绝、改成 `>` 会放进第 6 个；tools.rs:701 的 `session_mcp_tools` 预检仍是 TS 对齐快路径，单独翻转不可观测——预检拒绝同走 `limit_diagnostic()` 且假工厂无记录副作用，复检又兜住同一拒绝，故变异锚到复检、绑定竞态测试） | `…::session_mcp_tools_refuses_the_sixth_runtime_when_a_racer_registers_after_the_check` | `#2442 the over-cap compose starts no per-session runtime` |
| `per-session-limit-truncated` | 小数上限按 f64 比较、不截断为 usize（SessionMcpRuntimeBundle.ts:70；TS 不取整经 fix-r1 真 dist 探针实测，oracle 分数 cell 已由 #2445 落地：2.5 / 0.5→1 两格） | `…::session_mcp_tools_allows_the_third_runtime_under_a_fractional_limit` | `#1433 a 2.5 cap still admits the third per-session runtime` |
| `start-failure-propagated` | 启动失败降级为诊断（:147） | `…::session_mcp_tools_degrades_per_session_start_failures` | `#1433 composition survives the start failure` |
| `turn-registry-drops-mcp` | 组合工具进 turn 注册表（#1433 原始缺陷） | `…::native_turn_registry_carries_the_session_mcp_tools` | `#1433 the turn registry carries the session MCP tool` |
| `composition-reads-no-servers` | 组合读 enabled 表（生产读面） | `tests/sdk_mcp_session_tools.rs::discovery_composes_shared_and_per_session_tools` | `#1433 the shared lease tool is discovered` |

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
- 同树互斥（#2468）：本夹具原地改写 `tools.rs` 等守卫源码，运行期间**本工作树内
  不得有其他写者**——普通 `cargo test` / `cargo build` 也不行（同
  `cli-parity-mock-gateway/negative_controls.py` 的「Run with no other writers in
  this worktree」同款要求）。`.negative-source.lock` 的持有者谱系、互斥面与
  「锁缺席不证明树洁净」以 `src/pilotdeck-gateway/tests/common/read_lease.rs`
  头注释为正本（cli 侧 `src/pilotdeck-cli/tests/common/read_lease.rs` 同构镜像）；
  `mutating_sources` 与 `build_snapshot` 的锁内行为另见
  `src/pilotdeck-gateway/tests/fixtures/README.md`「共享输入扫描器（#1166 fix-r4）」段
  （该段不涉 `.negative-source.lock` 本身）。`cargo-slots.sh` 名额闸是并发配额，
  不互斥任何持锁者。本夹具的操作性规则同上条：变异窗口内本树什么都不另跑——
  写者、读者、锁外检查器（本夹具 `--check` 分支不取锁）与普通 cargo 一律算。
- 每个 `#[tokio::test]` 变异轮的正向/负向/还原三跑都经 `cargo-slots.sh` 名额闸。
- 单测体在 `mod tests` 内（缩进 4），外部测试在 crate 顶层——`test_body` 的
  函数尾锚按函数自身缩进回引（`(?P=indent)`），嵌套块的大括号不截断函数体；
  `check_binding_scope` 对两种布局各自反证。
