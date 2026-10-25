# Network cancellation timing fixture (#456 pr1)

100 格来自固定 TS cc7c4e6 + Node v22.22.0 的实际执行；`oracle.json`
保留 name、message、delivery、content、calls、retries 与请求路径。生产源码未改。

| 路径 | 阶段 | 完成源 | 时机 | 格数 |
|---|---|---|---|---|
| web_fetch | headers / body / redirect-hop | fulfilled / rejected / idle | continuation-first / parent-first / tie-parent-first / tie-parent-last | 36 |
| web_fetch | retry-status / retry-error | fulfilled | 同上 | 8 |
| fetch_redirect | headers / body / text-body / redirect-hop | fulfilled / rejected / idle | 同上 | 48 |
| fetch_redirect | retry-status / retry-error | fulfilled | 同上 | 8 |

`delay()` 自身没有独立 rejected/idle 完成源：拒绝来自父取消；对应不适用格
不能算已测。MCP、hooks、background、真实 socket `ExchangeFuture::io` 就绪交错
是 pr2，未测。这里的 `io(pending())` 仅表示永不完成，不表示真实 I/O 就绪证据。

## 真实入口和注入边界

- web_fetch：`src/tool/builtin/web/urlFetcher.ts:261` 的
  `getURLMarkdownContent`，默认 hook 经 `:98` 调真正的 `networkFetch`；不替换 hook。
  外层 hop timeout 与内层 request timeout 都是 60000ms；GET 重试2次、base500/cap5000。
  首跳302 body故意永不完成，下一跳必须可达。cache仅为本次探针的空内存实例。
- fetch_redirect：baseline **installed Undici** `fetch`（8.3.0，独立依赖哈希在金样中），
  自动 follow，最终 `arrayBuffer()` / `text()`；不冒充 Node 内置 fetch。
  `fetch_redirect::follow` 本身不拥有 headers 取消或重试：裸 headers/hop 的
  Rust 注入 transport 用已测注册序调共享 dispatcher；这些格验证 traversal，
  不能据此声称 hooks/MCP 等真实 caller 已覆盖。
- retry 组合：真实 `networkFetch(Undici.fetch → follow)`。成功重试返回302后再到200。
  Rust 外层只调一次真实 `network_fetch`，`FollowingTransport` 调原 `follow`；
  所有嵌套响应都已就绪，才用 microtask（分支断言禁止隐藏timer/I/O）。
  这不代表 helper 有自带重试，也不覆盖所有 caller 的重试布局。
- 注入在 Undici dispatcher：`onRequestStart/onResponseStart/onResponseData/onResponseEnd/onResponseError`，
  真实 fetch promise、redirect、body 和 NetworkFetchError 逻辑照常执行。
  dispatcher只允许 `https://fixture.test`，没有 socket、外网、用户配置或密钥。

## 时钟、同刻与原始证据

Node 原生 timer（不是假 timer 队列）。目标完成20ms；idle target40ms；父为目标±5ms或同刻。
为了有界运行，web idle headers 的60000ms timer压到40ms，其他60000ms watchdog压到1000ms；
retry500ms压到20ms，body/裸headers的600000ms独立timeout压到40ms。
原始 requested/effective 值都保留，Rust 暂停时钟使用原逻辑60000/600000/500ms。
这些注入验证顺序与归一化，不验证实际网络超时时长或 libuv socket 相位。

每个候选忙等55ms使目标和父timer均overdue；接受前核实际
`_idleStart + _idleTimeout` 差值与注册顺序，跨tick候选丢弃重跑（最多10次）。
生成器再次独立验证这些原值以及实际callback顺序；缺格、错序、错deadline都拒绝。
每格有3s宿主上限，整probe180s；所有自有timer在finally回收。

生成器将绝对timer原值、timeline写入输出目录 `raw.ndjson`，丢弃候选及Node告警写
`probe.stderr.log`。金样只移除绝对timer/timeline，保留已核验的相对deadline/注册顺序、
全部结果与请求记录。绝对uptime不可逐字节重现，不能伪称固定值。
TS commit/源码hash及installed Undici六个文件hash在生成前后验证。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/network-timing/generate.mts /fresh/output
```

目录recursive创建，三个输出文件都用`flag:'wx'`，已有原始证据不覆盖。
复采两次须`cmp`两份oracle；默认忽略的
`network_timing_regenerates_identically` 会重新运行生成器并严格比较oracle字节。

## Rust回放和已登记差异

`tests/network_timing.rs` 每格独立测试，严格比较六个observable字段。
私有 `fetch_redirect.rs` / `network.rs` 通过 `#[path]` 编译原文件，未复制实现、
未改公开接口；web使用实际公开 `UrlFetcher`。错误映射只来自真实typed outcome：
web `FetchError::Other` 没有name，观察为null，绝不根据message补造类型。
额外六个overdue测试验证“父事件已送达，但先注册续体仍先行”的晚poll分支。

本次62格对齐、38格严格差异：差异用独立 `#[ignore = "#456 差异：..."]`
保留TS期望（34格web，4格redirect）。主要为web丢错误类型、headers后仍取消body、
父先注册的同刻判定以及retry delay同刻仍推进。完整逐格TS/Rust原值、原始日志和
源码行号由交付证据目录 `RESULT.md` 提供；这些ignore不是已批准差异，不表示本阶段通过。

```sh
CARGO_TARGET_DIR="$PWD/_build/cargo-target" cargo test --offline --manifest-path src/Cargo.toml \
  -p pilotdeck-gateway --test network_timing -- --test-threads=1
# 真正重新生成（需要PDRS_BASELINE）：
CARGO_TARGET_DIR="$PWD/_build/cargo-target" cargo test --offline --manifest-path src/Cargo.toml \
  -p pilotdeck-gateway --test network_timing network_timing_regenerates_identically -- --ignored --exact
```

## 负控

`negative_controls.py /fresh/evidence` 对原始产品源码临时变异；必须独占本树、
无其他cargo或写者。使用 `negative_process.mutating_sources` 验证HEAD并负责恢复，
`wait_for_slot` 排队，`run_owned` 有界运行和回收进程。每项正控先绿，变异后必须是
oracle assertion失败（编译失败不算），恢复后同一用例再绿。覆盖父先、同刻两序、
status/error retry tie、message、retry次数、body内容、body取消的name/delivery。
另有四组全矩阵变异补齐逐格覆盖：已对齐的43个父先/同刻关键格都有实际oracle失败与恢复证据；覆盖索引在交付目录negative-coverage.json。已知差异格原实现即红，按任务第4条保持ignore，不将其包装成正控通过。

`negative_observations.mts <raw.ndjson>` 则对原始采集记录内存变异，21组守卫逐条
拒绝并复验原始100格；它不写TS源码、金样或原始证据。
