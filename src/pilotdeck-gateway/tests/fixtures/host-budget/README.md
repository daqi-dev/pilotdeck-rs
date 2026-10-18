# Host budget adapter 金样

固定基线 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。生成器向真实 `createSidecarDefaultModuleDispatcher` 注入假 budget port，经真实 `SidecarTurnProtocol.moduleResponseFor` 记录请求、响应和 port 入参；私有 class 只在内存中补 export，不写基线。校验整个基线 tracked checkout 干净，`sourceSha256` 逐文件对照固定 commit 的 `git show` blob 与工作树。

用例覆盖：全部八种能力组合与无 budget、必填 operation/request/provider/model、request 浅校验、evaluate 三个 options 的值域与顺序、calibration 原样保留、宿主 signal 身份与 active/aborted、usage record 过滤、cost undefined/null 投影与非法有限值、端口异常、tokens/snapshot 不额外校验。`cases[].wireResponse` 是真实协议响应；`response` 是从该响应提取的 ok/result/message。假端口的 calls 记录传入参数及顺序。具体条数以生成器输出和 oracle 的 cases 为准。

本地路径调用真实 `nativeAgentTurnCapabilitiesAdapter.ts` 导出的 `createAgentTurnCapabilities`（导入别名 `createNativeAgentTurnCapabilities`），注入临时配置、假 router 与真实 TokenAccountingRuntime。假 router 的 estimateUsageCost 绑定真实 TokenStatsCollector.estimateCost，与 RouterRuntime 的直接委托一致；不添加 AgentLoop 调用方的费用兜底。负 token 和对象 token 导致的负值/NaN 必须经过真实 dispatcher 报 invalid usage cost，不能写成 null。两份组合源码均收入 sourceSha256。

请求输入复用 model-cost 金样的请求（不读其期望输出），包含多媒体、工具结果、系统块与 schema；另有定价覆盖、nativeCost、JS 数字转换、UTF-16 定价键与模板转换、非对象 schema 和非标准 MIME PDF。深层宽松输入覆盖 null message/block、字符串 content、缺 name、数字 text、字符串/数组 pages、非字符串 systemPrompt/tools、工具结果 map/join 和 system → messages → tools 的错误次序。所有期望均来自真实 TS 调用；真实 evaluate/TokenBudgetManager 装配属于 A6.4-pr3。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/host-budget/generate.mts [output-parent]
```

每次生成器用 `mkdtemp` 在 output-parent（默认系统临时目录）下建立独占子目录，stdout 最后一行给出 JSON `{output,cases}`。不覆写仓库 oracle、不删除任何目录；录用新金样需核对后明确写入。统计器 disabled、网络 transport 禁用，不读取用户 PilotDeck 数据。

```sh
cargo test --manifest-path src/Cargo.toml --locked --offline -p pilotdeck-gateway --test host_budget
cargo test --manifest-path src/Cargo.toml --locked --offline -p pilotdeck-gateway \
  --test host_budget budget_fixtures_regenerate_twice_identically -- --ignored --nocapture
```

回放逐项精确比对（包含对象键序，无 epsilon），核 manifest、端口调用与响应。设置 PDRS_BASELINE 时检查 fixture provenance；ignored 测试在 UUID 独占父目录下运行生成器两次，并比对两次与入库 oracle 字节完全一致。临时证据保留以供排障。
