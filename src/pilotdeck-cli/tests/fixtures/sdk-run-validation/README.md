# #1058 模型与序号验证

`generate.mjs` 必须用 Node v22.22.0，直接导入只读固定基线 `packages/sdk/dist/client.js` 与 `transport.js`。模型格通过 `createQueryWithTransport(...).resolveModelOverride()` 调真实方法（仅替换 catalog 请求返回值）；序号格通过真实 `GatewayTransport.handleMessage()` 观察 push/fail（只替换队列记录器）。没有复制 TS 选择/校验逻辑，也不启动 query 或网络连接。

`oracle.json` 包含源码/产物 SHA-256、12 个模型格和 12 个序号格；输入、输出与原始探针控制台逐格相同。缺 provider/model、空串、disabled、重复匹配、undefined/undefined 的拼串歧义，以及 gap/负数/小数/缺失/null/字符串/布尔值都有独立格。

Rust 单测调用实际产品使用的 select_model / next_sequence；`sdk_run_exit::run_validation_oracle_regenerates_identically` 是 ignored 的 `regenerates_identically("sdk-run-validation")` 守卫，会重新调用 TS 逐字比较金样。目录创建 recursive:true，文件 flag:wx，已存在金样拒绝覆盖。

```sh
PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck \
node src/pilotdeck-cli/tests/fixtures/sdk-run-validation/generate.mjs NEW_OUTPUT
```

模型错误格仅约束选择结果，不能据此宣称异常会话生命周期已对齐；该范围继续留在 #123 P06–P08。
