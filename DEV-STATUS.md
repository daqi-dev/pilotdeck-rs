# Gateway 第一波开发状态

结论：优化方案已执行，产品重构及验收源码修复已写入本开发副本。尚未编译、测试或完成正式替换验收；本文件不授予 PASS。

源版本：Rust `7d7c0ab2dad32b4c71bb2ed465147cdff179f374`，TS `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。初始来源见 `SOURCE-MANIFEST.json`。本目录无 Git 元数据，当前文件哈希不是集成提交身份。

| 已执行范围 | 具体结果 |
|---|---|
| HTTP 产品职责 | `native_http.rs` 从 957 到 538 行；响应体/HTTP 兼容构造与静态文件服务分别移入 `response.rs`、`static_assets.rs`，保留原调用入口 |
| 附件产品职责 | `services.rs` 从 2459 到 919 行；附件组合闭包移入 `services/attachments.rs`，能力检查、提交生命周期及阻塞调度留父模块，唯一 hook 状态一起移动 |
| 启动配置职责 | `native_process.rs` 从 558 到 292 行；选项解析及环境校验移入 `options.rs`，保留原导出与进程组装顺序 |
| 既有负向控制 | HTTP、附件及启动配置控制改绑真实的新源文件并保留父/子源身份保护；原锚点和拒绝规则保留 |
| 公共 HTTP 判分 | 绑定每个请求与真实线上响应；保留协议、状态文本、重复头、完整正文及模型/渠道交换；拒绝遗漏业务交换、伪升级及加载后换回磁盘文件的身份混淆 |
| 项目摘要时钟 | `createdAt` 仅扩展到已批准的完整项目摘要形状；`sessionCount` 要求精确非负整数，其他业务字段仍参与比较 |
| TS 渠道参考 | 使用固定 TS 的真实 CLI/SDK；对齐 HOME、PILOT_HOME、项目、输入材料与普通权限应答；保存公开会话及当前轮证据。成功采集只记 CAPTURED |
| 隔离渠道端点 | QQ token 和钉钉连接使用真实路径；钉钉回调只替换 origin，保留 path/query；旧错误路径别名移除 |

重构作者之外的静态审查已检查函数移动、可见性、cfg、原测试挂载及源绑定；发现的日志 trait 导入遗漏已修复。独立静态检查也推动修正了 HTTP 原始交换绑定和渠道跨轮证据问题。静态检查不是 cb8 执行结果，也不是正式 GLM + DeepSeek 双审。

新增 25 个公共 HTTP Python 控制方法、13 个项目摘要范围控制方法，以及渠道 transport/前置条件/旧轮混淆控制源码；全部 NOT_RUN。FROZEN 保持原字节，新增源码登记不表示冻结校准完成。

当前仍有明确未闭合项：

- 当前环境不能通过普通 SSH 访问 cb8，且只读正本不在可写范围；没有绕过限制或改动正本。先保留可回收补丁，待获准执行环境内建立真实集成提交后再构建。原副本的测试密钥 fixture/已安装依赖等未复制，补丁须回收到完整仓库，不把摘取副本直接当完整验收环境。
- 四个可选 SDK 渠道（Telegram、Discord、Slack、钉钉）在固定 TS 官方 ESM 入口存在 bare `require` 的静态阻塞；真实执行前不能当作可采集成功。未注入全局 require、替身 SDK 或改变 TS 基线。
- 新采集证据显式记录 `schemaVersion: 2` 与 `native-eight-v2` / `native-media-v2` profile。字段 `deliveries`、`modelRoundInputs`、`roundContexts`、`contextHelperSha256` 保留原始记录索引和真实轮次身份；飞书使用入站 message_id 和真实 turnIds，不额外注入文本消息。helper 与控制源码已登记；原 `native-eight-v1` 消费者的单模型请求/旧路径契约不能替代新 profile 的比较器。当前轮采集与固定 TS/native 完整业务比较仍须在同一集成版本接通并验证。
- 150 入口、四 host、session、retirement、冷切回滚、完整回归及正式独立双审均尚未在本次修改后的版本运行。A7 已拒绝范围保持 NOT_RUN；用户服务未切换，PR 仍按 Draft 约束推进，推送前须 gitleaks。

获准 cb8 执行后的顺序：先接收带原始/当前哈希的分组补丁并建立真实集成提交；运行同提交构建与相关回归；按现行冻结校准流程验证装置和新增控制；再采集真实同输入参考、闭合所有必需门。新冻结摘要必须来自合法校准流程，不能仅用当前源码哈希替换。

待跑的精确入口（仅记录，不在本机执行；须放入 cb8 现行资源/租约包装器）：

```text
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib native_http::tests -j 4
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib native_process::services_tests -j 4
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib native_process::channel_attachment_security_tests -j 4
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib native_process::tests -j 4
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib native_process::management_tests -j 4
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --test native_process native_entry_process_rejects_inherited_authority -j 4
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib loopback_fixture_origin_tests -j 4
python3 -I scripts/gateway-parity/test_retirement_public_http.py
python3 -I scripts/gateway-parity/test_project_clock_scope.py
python3 -I scripts/gateway-parity/test_safety.py
node --test src/pilotdeck-gateway/tests/fixtures/native-channel-production/reference-controls.test.mjs
```

上述过滤测试须与 workspace 调用方构建、受迁移影响的既有负向控制及原完整十阶段检查一起执行，不能替代完整验收。第一波未经 cb8 验证以前不继续叠加第二波产品重构。
