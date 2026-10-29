# Real sidecar integration fixture (#748)

生产 TurnSubmitter、GatewaySubmitHost、ProjectSessionFactory、LiveSession 经固定 Python stdio 中间人连接真实 TS sidecar。HTTP mock 仅替代 provider；工具使用真实 read runtime。TS 金样来自独立 InProcessGateway，共用真实 AgentSessionEventRecorder 和 durable model invoker。

先设 `PDRS_BASELINE` 指向固定 TS baseline（oracle 的 sourceCommit/sourceSha256 校验），设调用方独占 `CARGO_TARGET_DIR`，Node 必须 v22.22.0；两个 Cargo debug profile 均设 `line-tables-only`。不读取用户 PilotDeck home，不需要 provider key。

- 宿主全验：`python3 -B tests/fixtures/live-e2e/run_host.py <全新输出目录>`（在 gateway crate 目录运行或使用完整脚本路径）。其 cargo `--test` 清单包含 `live_e2e` 和 `generator_provenance`，后者执行 ignored 的七格重生成守卫。
- 独立生成：`node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" tests/fixtures/live-e2e/generate.mts <全新输出目录>`。
- 独立自检：`python3 -B tests/fixtures/live-e2e/selftest.py <全新输出目录>`。
- 负控绑定检查：`python3 -B tests/fixtures/live-e2e/negative_controls.py --check`。
- 提交后负控：同一脚本加 `<全新输出目录> [变异名…]`；先编译，再正控、带专属回执的负控、恢复正控，最后汇总全部结果。源码/装置须等于 HEAD；未提交新文件时拒跑，禁止绕过。所有开关仅在 `pdrs-mutants` 测试构建内生效。

七格：文本、真实 read、模型流取消、read 执行中取消、sequence 缺号、重复、提前 final。重复帧等待客户端原子可见标记再发送；工具取消等待真实 read 进入后取消。mock 挂起最长 10 秒、MITM I/O 有界、Rust 流最长 45 秒、TS capture 最长 70 秒。前驱不完成时依靠取消/截止退出，不用任意 sleep 充当业务进度。

比较包含完整 GatewayEvent JSON 字节、registry 所有事件和终态、模型/步骤/消息 transcript、重新打开的会话历史与进程回收。随机 entry identity 先验证非空唯一、sequence/parent 连续，再去除；时间只在已验证的持久化 envelope、registry 和本地错误时间处规范化；请求 fingerprint 必须先用原始有序请求重算。原生 session metadata 两条独立严格校验。case 路径、loopback 端口和已核实 owner PID 是环境身份，不参与跨进程值比较。

提前 final 的 TS 金样保留其成功行为；Rust 按 #748 明确要求拒绝，差异依据 `specs/gateway-parity/diffs/A7.2-TURN-EARLYFINAL-001-R2.md`。该格错误期望从 TS duplicate 格的完整错误形状派生，生成器不读 Rust 输出。

MITM 路径固定；生成器、自检与宿主拒绝 `LIVE_E2E_MITM`。oracle 同时记录 MITM/driver/mock/capture 等六份装置源码 SHA256 及七格金样 SHA256。输出目录和文件独占创建，原始捕获不覆盖。
