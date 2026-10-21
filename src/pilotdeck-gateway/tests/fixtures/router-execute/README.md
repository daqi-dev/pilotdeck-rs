# Router execute / provider health 对拍（A6.4-pr9a）

固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。`record.mts` 调用真实 `RouterRuntime`、fallback admission、health tracker 和 session/generation health port；`generate.mts` 通过共享 `bindSourceTree` 与 `sourceClosureSha256` 固定 `RouterRuntime.ts` 的 650 项相对 import 闭包，并断言 en-US locale。生成目录由调用者独占提供；生成器用 `wx` 拒绝覆盖，没有删除动作。

## 本轮证据边界

本任务禁止任何 Git 命令，因此本轮没有运行 `generate.mts` 的干净树守卫。入库 `oracle.json` 是真实 TS 两次运行的相同输出；650 项源码 SHA256 逐项与既有 `router-decision` / `model-stream` 金样核对。它是既有溯源链的增量证据，**不是本轮重新证明基线干净**。协调者仍须按下述命令重新正式生成、逐字节比较。

本轮沙箱禁止 loopback `listen`（原生 Node 探针为 EPERM），因此 `--http` 的 TS 金样尚未生成。HTTP parity 测试显式 ignored，提供生成物后由协调者在允许监听的环境运行；缺文件直接失败，不能把 ignored 计作通过。Rust 另有正常执行的进程内故障注入测试，覆盖 HTTP 429、503、拒绝连接、断流、超时、取消，验证的是真实 `stream_model` + Router 路径。

## 正式复现（需协调者解除基线只读 Git 限制）

```sh
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
export PDRS_GIT=/usr/bin/git
# 每次指定一个新的 /tmp 独占目录；不向基线或 node_modules 写入。
LANG=en_US.UTF-8 TMPDIR=/tmp node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/router-execute/generate.mts /tmp/<owned-run-1>
LANG=en_US.UTF-8 TMPDIR=/tmp node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/router-execute/generate.mts /tmp/<owned-run-2>
cmp /tmp/<owned-run-1>/oracle.json /tmp/<owned-run-2>/oracle.json
cmp /tmp/<owned-run-1>/oracle.json src/pilotdeck-gateway/tests/fixtures/router-execute/oracle.json
# 同样两次录制 HTTP（两边都运行真实 loopback HTTP，而非外部 provider）。
LANG=en_US.UTF-8 TMPDIR=/tmp node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/router-execute/generate.mts /tmp/<owned-http-1> --http
LANG=en_US.UTF-8 TMPDIR=/tmp node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/router-execute/generate.mts /tmp/<owned-http-2> --http
cmp /tmp/<owned-http-1>/http-oracle.json /tmp/<owned-http-2>/http-oracle.json
# 先按任务要求取 cargo 槽并设置 CARGO_HOME / CARGO_TARGET_DIR。
env -u PDRS_BASELINE TMPDIR=/tmp PDRS_ROUTER_HTTP_ORACLE=/tmp/<owned-http-1>/http-oracle.json \
  cargo test --locked --offline --manifest-path src/Cargo.toml -p pilotdeck-gateway \
  --test router_execute http_sequences_match_real_ts_oracle -- --ignored
```

常规回放复用 `tests/common/baseline.rs` 的 `oracle()` 和 `Scratch`。未设 `PDRS_BASELINE` 时仅检查固定 commit 与哈希结构，不调用 Git。设置后会调用只读 Git，仍需遵守运行者的权限限制。

## 覆盖与比较

- 32 个 execute 场景：前置 framing 丢弃、最终错误重复输出、五类（含空）content 锁定、SDK fallback 对 explicit / disabled 生效、项目链回退、策略拒绝和动态筛空、fallback 优先 transient retry、zero-usage 使用最后一次 usage、open 中间项跳过但首尾必试、媒体重排与降级、孤立 UTF-16 代理码元、取消、generation 捕获、网络异常、请求错误 details；新增无 usage 的 text/thinking/tool-call 拼接估算、usage 为 null/空对象/字段为 null/缺字段时估算，以及只有单侧有效 token 时原样保留 usage。
- 8 个真实 `streamModel` 请求验证错误：content 路径的 unsupported_modality、unsupported_image_mime_type、image_too_large、pdf_too_large、audio_too_long，以及 Anthropic / OpenAI / Responses 的 unsupported_thinking；逐项比较 code、message、details，传输层一旦被调用即失败。
- 60 个错误准入组合；4 组逐步 health 轨迹；17 步 session / generation / dispose。health 冷却另用 Tokio 暂停时钟验证边界。
- 7 个 HTTP 场景：429、503、拒绝连接、内容后断流、首 provider 超时后恢复、取消、整链失败。正式 Rust 回放复用 `common/model_mock.rs` 与 `HyperModelTransport`，比较全部 invocation calls、HTTP 请求体/头/顺序、canonical/router/health trace、最终错误与 session usage；保持中的响应还验证 clientClosed。
- 全部序列以 JS JSON 字节保序比较，不排序事件。HTTP 只替换独占端口 origin，并去除两边统一登记的传输层头。默认健康状态、generation nullish 与数字/字符串同键，均保留 TS 行为。
- `pilotdeck_router_fallback` 本身没有 `fallbackTier`；该字段属于 decide 的 `pilotdeck_router_decision_fallback`，继续由既有 `router-decision` 金样覆盖，本轮不造字段。

统计聚合、请求日志及 SDK usage/model_usage 投影属于拆分后的 pr9b。`runtime::network` 尚不在本树，本轮复用 provider 原有竞争判定与退避，#396 接续统一。
