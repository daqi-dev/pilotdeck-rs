# Provider HTTP 边界：#411 · #360 第 3/4 条 · #860

Node **v22.22.0**，固定 TS **cc7c4e6**，baseline 安装的 undici **8.3.0**。生成器调用真实 `streamModel` / `EnvHttpProxyAgent` / `undici.fetch`，仅访问自有 loopback；Rust 复用 `tests/common/model_mock.rs`。此包不覆盖 #360 第 1/2 条，不修改冻结尺子。

## 金样与回放

- `oracle.json`：61 个 provider 场景（重复块 5、duration 3、零 idle 2、稳定 timer 2、token-limit 29、请求体 20），29 个代理字符，6 个 TLS 结果。
- 请求体四协议 anthropic/openai/openai-responses/google，分别测试真正省略可选字段、完整字段、toolChoice none/required、空值；记录实际 HTTP 方法、路径、请求体字节。输入必须用保序 `JsValue` 装载，避免测试库排序嵌套 schema 键。
- `clockAfterFirst`：复用 `_shared/generator_guard.mjs::installFixedClock`，消费者收到首个 canonical text 后设逻辑时间。max=10 时 elapsed=9/10 继续、11 失败；对应 Rust 注入 `StreamOptions.clock`，不用墙钟等待赌边界。
- limit=3 时重复数量 2/3/4；另测 UTF-16 长度 2 的 emoji 不计数、非文本事件清空重复状态。
- streamTimeoutMs=0 分别回落 provider=80 与默认 600000；后者首帧后由父取消终止，覆盖永不完成 body 的取消路径。私有测试另直接比较 TS `resolvedIdle`，防止取消掩盖默认值错误。
- `tls.json` 是一次性测试 CA/leaf，仅 leaf 私钥公开作服务器夹具；CA 私钥已丢弃。固定日期适用于 **2026–2099**。实测包含有效、过期、未生效、错误 SAN、clientAuth-only 用途、错误签名；不声称穷尽 OpenSSL 全部证书错误。
- `no_proxy`：真实 TCP CONNECT 区分代理/直连；目标 `localhost.` 避开 PilotDeck 固定追加的 `localhost`。包含完整 ECMAScript 空白集合及逗号，另测 NEL/U+180E/U+200B 不作分隔。生产复用 `metadata::js_space`。

```sh
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
export LANG=en_US.UTF-8
export CARGO_TARGET_DIR="$PWD/_build/cargo-target"
export CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only
node --disable-warning=ExperimentalWarning --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/provider-http-boundaries/generate.mts <新输出目录>
scripts/cargo-slots.sh
cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-gateway --test provider_http_boundaries
scripts/cargo-slots.sh
cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib boundary_proxy_tests
scripts/cargo-slots.sh
cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-gateway --test provider_http_boundaries provider_http_boundaries_regenerate_identically -- --ignored --exact
```

生成器目录 `recursive:true`，金样文件 `flag:'wx'`；不覆盖已有输出。ignored 守卫逐字比较新生成 oracle。所有请求都有全局 45s 探针截止，Rust 请求/读体 5s 截止；持有的 sockets/agents 在 finally 中关闭。

## #411 对拍时机合同

`read_frame` 保留严格 `<`，不把 499ms 加进生产 timeout。undici parser 的 body timeout 使用 fast timer；实测 native tick=499ms，5000ms fast timer 在第 11 tick（fastNow=5489）触发。tick 相位、连接历史与事件循环延迟不在 Rust 当前时长参数中，固定补偿不能还原竞速。

本夹具只对 **idle=body** 或 **idle≥body+1000ms** 做相同错误断言。两项稳定 oracle 用 undici 测试接口 `timers.reset()` 清空已销毁 dispatcher 的 timer 池，明确是冷时钟探针；不声称生产任意热池或调度阻塞下永远同序。近窗用独立 Node 子进程实测分布，**不将非确定性分布写进逐字重生成金样，也不在判分器中豁免失败**。

```sh
PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck node src/pilotdeck-gateway/tests/fixtures/provider-http-boundaries/measure.mjs <新证据目录>
```

该命令复用 #396 的真实 TCP 探针（`probe.mts` 仅改为可移植导入），固定 body=5000，gap=0/100/300/500/700/900/1100，各 3 次，最多 21 请求、每子进程 30s；不启动 CPU 压力进程。本次原始记录在 `_tmp/20261026-issue-411/tcp-gap-*-run-*.jsonl`，汇总 `tcp-summary.json`：0/+100/+300 各 3 次 idle，+500/+700/+900/+1100 各 3 次 body；body 实际约 5496–5510ms。fast timer 原始 tick 记录 `timer-ticks.jsonl`。这是本机样本，非分布保证。

窗口登记：`specs/gateway-parity/diffs/A6.4-PROVIDER-IDLE-001.md`，未批准，非已接受差异。

## 负控

`negative_controls.py` 59 个独立变异，覆盖 BOM/NEL、零/default idle、等时与长间隔、重复边界/重置/短块、duration 边界、五种 TLS 错误、四协议五种可选字段组合的 wire body、token parser 各可观察分支及生成器请求快照。每个变异：正控 → 应用 → `cargo test --no-run` → 专属消息失败 → 还原正控；最后汇总所有 KILLED/SURVIVED/INVALID。编译失败/锚点失配不算 KILLED。

```sh
python3 src/pilotdeck-gateway/tests/fixtures/provider-http-boundaries/negative_controls.py --check-anchors
# 必须先提交，源码等于 HEAD；沿用调用方 CARGO_TARGET_DIR。
python3 src/pilotdeck-gateway/tests/fixtures/provider-http-boundaries/negative_controls.py <新证据目录> no-proxy-bom tls-future
# 全套由协调者在宿主串行跑，允许省略最后两个名字。
```

未提交候选不能绕 `mutating_sources` 守卫。提交后宿主实跑（2026-10-28，HEAD 1530c93ff）：macmini 58/59 KILLED，唯一 INVALID `generator-wire-snapshot` 为重生成类变异在宿主 Node v26 下的已知环境问题，本机 Node v22.22.0 补跑 KILLED → 合计 **59/59 KILLED、0 SURVIVED**；原始日志在主仓 `_tmp/20261026-issue-p2b3/remote-host/` 与 `host-local-r1b/`。
