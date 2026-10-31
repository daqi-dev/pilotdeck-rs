# native-session-e2e 端到端控制（#1140 fix-r1）

协调者补充要求的核心 P1 证据：`collect.py --session-domain` 的真实输出必须能被
`session_domain.py --native-capture` 按案例打分，且断言逐案例 PASS/FAIL 判定
——绝不允许 EVIDENCE 整体 fail-closed 掩盖单案例差异。

## 文件

- `test_e2e.py` — 离线端到端套件。只 mock OS 层（strace、pidfd、platform）
  并把 settle/provider 延迟调短；mock 网关与采集器本体全部真实运行：

  - `mock_gateway.py` 是真实 socket 的 WebSocket 服务：完成升级握手、回答
    hello_ok（携带 `interactionBinding`），每个 submit_turn 先向真实环回
    provider POST 一次（让采集器自己的请求台账记录该次出网，正是真实网关
    的模型调用路径），再写 transcript 与 `runs.json`（durable/registry 先于
    该轮 final 帧落盘，采集器读面无竞态），然后回放该案例注册的 rust 期望
    帧，最后发 prefix-type 的 final 帧（frozen 投影过滤 turn_started）。
    它是被测的 Rust 网关替身，不是 witness。
  - `test_e2e.py` 经 `c._native_run` 走完整采集（合成 strace 文本由被 mock
    的 tracer 写出，exec/clone/connect 行均来自采集器自己注册的 argv 与
    合同），随后按显式路径字节加载 frozen `session_domain.py` 并在进程内调
    `deadline_report(native_capture=...)`（无 CLI 租约）打分。
  - 十个测试：真实采集 72/72 逐案例 PASS、批次保持 NOT_RUN、采集 schema
    （合同声明 connections=1、每行带 wire 元数据、send 帧记录 payload 文本、
    恰一个 hello），以及七个数据负控（每次只破坏一个已采面，receipt 哈希
    重钉——被测的是打分逻辑而非篡改检测器）：frames 伪造恰好单案例 FAIL、
    send 帧变对象、行缺 wire、空 provider 台账、record 缺 state（逐案例
    FAIL 而非批次崩溃）、observation 置 NULL 保持 NOT_RUN 且不计数、frames
    缺 type 是结构拒收、registry applicability 失配单案例 FAIL。
  - 端口用 28997/28998：在固定 28999..29008 采集系列之外，离线运行绝不与
    真实采集的网关/提供方端点争用。套件不取 parity 租约（负控运行器的
    `mutating_sources` 在整个执行期持有它）。

- `negative_controls.py` — 源码变异负控（11 行）：7 个 `session_domain.py`
  变异（方向感知 decoder 交叉核对退回 frame-only、删空台账 require、
  `record.get('state')` 退回 `record['state']`、executedCases 计入 NOT_RUN、
  删 frames type 结构守卫、hellos 不过滤 dict、删 wire 形状 require）与
  4 个 `collect.py` 变异（send 帧记成对象、删行 wire 元数据、删合同
  connections、写死 registry applicable）。每个变异绑定一个被点名测试的
  专属失败消息；positive → mutant → restored 三段，全部跑完才汇总，
  非 KILLED 即非零退出。守卫集含 common.py（gateway-parity 路径要求），
  `mutating_sources` 持 parity 租约并逐字节还原。

## 运行

```sh
python3 -I src/pilotdeck-gateway/tests/fixtures/native-session-e2e/test_e2e.py
python3 -I src/pilotdeck-gateway/tests/fixtures/native-session-e2e/negative_controls.py \
  --check-anchors
# 提交后由协调者在宿主跑（调用方必须已提供 CARGO_TARGET_DIR）：
python3 -I src/pilotdeck-gateway/tests/fixtures/native-session-e2e/negative_controls.py \
  <新证据目录> [--case NAME] [--baseline TREE]
```

Rust 侧入口 `src/pilotdeck-gateway/tests/native_session_e2e.rs` 在 cargo 测试
中拉起 `test_e2e.py`（`python3 -I`，成功即静默）。

## 已知边界

- `_native_run` 被 mock 的只有 OS 层；`w.capture` 的 trace 解析、合同/身份
  绑定、receipt 发布全部真实。合成 trace 因此必须逐字节匹配采集器注册的
  argv（含 resolve 后的绝对路径——macOS /var 符号链接）。
- 评分按 `load_frozen()` 读取工作树字节：这正是变异负控注入的通道；#166 的
  CLI HEAD 检查不影响进程内显式路径加载。官方冻结审计仍由提交后的协调者链
  跑。
