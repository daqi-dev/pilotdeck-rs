# 代码入口

- Rust workspace：`src/Cargo.toml`、`src/Cargo.lock`；保留 11 个成员、本地 patch/vendor 与构建所需源码归档。
- Gateway：`src/pilotdeck-gateway/src/main.rs`、`lib.rs`、`native_process/`、`native_http/`、`channel/`、`switch.rs`；CLI：`src/pilotdeck-cli/src/main.rs`、`server.rs`。
- HTTP 判分：`scripts/gateway-parity/retirement_public_http.py`、`retirement_standard_http.py`、`compare.py`；单测：`test_retirement_standard_http.py`、`test_safety.py`。
- HTTP / 渠道生产器：`src/pilotdeck-gateway/tests/fixtures/retirement-public-http/`、`native-channel-production/`。
- 150 / host / session / retirement：`scripts/gateway-parity/retirement_entries.py`、`host_suite.py`、`session_domain.py`、`retirement_native.py`、`retirement_cold.py`。判据在 `specs/gateway-parity/`，共享登记与冻结归主智能体。
- TS 固定参考：`reference/PilotDeck-cc7c4e6/`；真实启动入口 `src/cli/pilotdeck.ts`，渠道 `src/adapters/channel/`，协议 `src/gateway/protocol/types.ts`，项目摘要 `src/web/server/listProjects.ts` 与 `src/web/client/protocol.ts`。

TODO：修复当前 HTTP 评分边界与响应配对；保持 TS/Native 同输入、实际项目与环境；补齐真实渠道引用及完整后台 wire/model/disk 比较；在获准环境完成同版本全域回归与独立双审。上述均为开发任务，不是本静态摘取已通过的结论。

缺口：5 个含私钥的 TLS 测试 fixture、1 个 TS LFS logo、安装后依赖/生成 dist/工具链与运行环境未带入；无 Git 身份，既有基线路径尚未重绑定。精确路径、原始 SHA 与边界见 `SOURCE-MANIFEST.json`，执行约束见 `DEV-CONSTRAINTS.md`。
