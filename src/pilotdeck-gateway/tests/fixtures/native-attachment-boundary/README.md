# 原生附件信任边界：非冻结真实 Server 探针

隔离 loopback 模型诊断；不执行 Cargo，不访问真实账号或外部 provider。\n已有 r65 固定二进制实跑见持有人独占证据目录，不能冒后续源码头或正式验收。
实施家族 Codex；不构成第二独立谱系批准。

范围：隔离真实 native Server 的普通 WS → submit_turn → 本地模型 HTTP，
以及 Weixin 生产轮询 → AES 附件下载 → typed 提交 → 同一模型接收端。
不改 standalone 默认 proxy，不改冻结判分，不覆盖全部 101 方法或 19 渠道。

P1（验收证据缺口，非已确认泄漏）：普通 WS 伪造 channelKey、
attachments[].metadata.channelKey，引用 typed 缓存时，现有
src/native_process/channel_attachment_security_tests.rs:65 仅直接调用 composer，
没有监听器与模型 HTTP 见证。路径相对 src/pilotdeck-gateway/。
src/native_process/services.rs:742、:1547 声明并选择内部缓存能力；
src/channel/weixin/media_host_tests.rs:239 已有真实 Server/A7 入模正例，
但没有普通 WS 反向越权请求。本补丁是该缺口的最小网络增量，不推断产品已泄漏。
固定 TS cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85 的
src/gateway/client/RemoteGateway.ts:140 使用 submit_turn；
client/GatewayWsClient.ts:160、:220 定义请求及终态帧。未复制 TS 实现。

运行者仅在 mbp2 固定源码根按既有资源闸门完成构建后执行：

    CARGO_TARGET_DIR="$PWD/_build/cargo-target" /Users/da/ws/pdrs-runner-tools/bin/cargo build --manifest-path src/Cargo.toml -p pilotdeck-gateway -p pilotdeck-transport --bin pilotdeck-gateway --bin pilotdeck-agent-loop-sidecar
    PDRS_SOURCE_MANIFEST_SHA256=<本次构建源码清单SHA256> /Users/da/opt/node-v22.22.0-icu78-darwin-arm64/bin/node src/pilotdeck-gateway/tests/fixtures/native-attachment-boundary/driver.mjs "$PWD/_build/cargo-target/debug/pilotdeck-gateway" "$PWD/_build/cargo-target/debug/pilotdeck-agent-loop-sidecar" "$PWD/_tmp/attachment-boundary-unique-run"

Node 必须为 22.22.0。输出目录必须不存在；失败原件保留，复跑换目录。
只启动回环 fixture 与本次子进程；HOME/PILOT_HOME/临时目录全部隔离。
凭据完全由 fixture 合成，不读取 .env.local、真实凭据或真实 home。
模型上限 16 请求、maxOutputTokens=128，单 HTTP 请求体 2 MiB，WS 记录 4 MiB。
这是本地模型接收见证，不是外部 provider 或正式 host/session/retirement 验收。

正例先由真实 Weixin 下载附件，并核模型 messages 含随机秘密及真实缓存字节。
随后三个独立会话分别伪造顶层字段、附件 metadata、两者同时伪造，引用同一文件。
每例必须完整结束且实际产生包含独有提示标记的模型 HTTP 请求；空请求集不能通过。
负例核所有该轮模型 messages 不含秘密；不凭助手回答或 composer 返回值判断。
不发送工具调用，因此不声称已覆盖 read_file 工具权限或缓存跨渠道、符号链接竞态。
evidence.json 保存模型原始请求、WS 原始帧、渠道请求、二进制哈希及失败。
sourceCommit 仅为审计目标标签；运行者仍须将构建来源凭据与二进制哈希绑定。
强制结束主进程时标明后代清理未验证，不能当作清理通过。

优先下一动作：
1. 在每个拟验收固定构建上运行，检查四个业务格及正常退出；诊断历史不累计为新头成绩。
2. 在独占产品源码副本暂将普通 WS 错授 Weixin 缓存能力，重建后确认负例能杀死该变异；不动冻结尺子。
3. 固定同一源码/二进制哈希收集正式证据，并交非 Codex 家族复核本增量。
