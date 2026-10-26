# Network cancellation timing fixture (#456 pr1 / pr2)

固定 TS cc7c4e6 + Node v22.22.0 的真实执行共336格，沿用同一个生成器、金样、差异表和反向守卫。
当前Rust接线336格：274对齐、62差异。pr2b补上MCP GET56、POST48、hooks48，新增111对齐、41差异；#752 修复 web 错误类型名后，ERROR-CLASS 的 34 格（pr1 23 + pr2a 11）转绿，剩余 62 格差异。差异不等于已接受或阶段完成。
非test产品代码、公开接口和行为不变，仅在两个私有模块追加cfg(test)接线。产品修复归#752。pr2b原始证据与逐格状态见本树 `_tmp/20261026-issue-456c/RESULT.md`；pr2a记录仍在原目录。

| 路径 | timer headers/body/redirect | timer retry-status/error | io-headers | lifecycle | TS合计 | Rust已接线 |
|---|---:|---:|---:|---:|---:|---:|
| web_fetch | 36 | 8 | 12 | 0 | 56 | 56 |
| fetch_redirect（另含text-body） | 48 | 8 | 12 | 0 | 68 | 68 |
| mcp_get | 36 | 8 | 12 | 0 | 56 | 56 |
| mcp_post | 36 | 0 | 12 | 0 | 48 | 48 |
| hooks | 36 | 0 | 12 | 0 | 48 | 48 |
| background | 36 | 8 | 12 | 4 | 60 | 60 |

普通阶段各为fulfilled/rejected/idle × continuation-first/parent-first/tie-parent-first/tie-parent-last。
retry阶段只含fulfilled × 四时机：delay拒绝来自父取消，无独立rejected/idle完成源。
hooks无自身retry，MCP POST不重试，这些均不适用，不算已测。

## 真实入口与边界

TS路径和行号相对固定基线根：

- web_fetch：`src/tool/builtin/web/urlFetcher.ts:261` 的真实 `getURLMarkdownContent`，默认hook经`:98`调用`networkFetch`；hop/request各60000ms，GET retry2/base500/cap5000。
- fetch_redirect：baseline installed Undici 8.3.0的真实fetch/follow/text或arrayBuffer。裸follow不拥有headers取消或retry；retry组合是networkFetch(Undici.fetch→follow)，Rust真实network_fetch包真实follow。不能将它泛称所有caller已覆盖。
- MCP：`src/mcp/client/McpClient.ts:183-199` buildTransport提供的真实fetch closure，经SDK保存的`_fetch`调用；GET10000ms、POST60000ms，GET retry1/base500/cap5000，POST无retry。只覆盖HTTP adapter，未做SDK握手、事件流或会话生命周期。
- hooks：`src/extension/hooks/execution/HttpHookExecutor.ts:9,18,27,35`真实execute，既有fetchImpl端口注入installed Undici；POST→text→parseHookOutput。signal贯穿body；无自身retry/timeout。完整返回对象保留为observable.hook，并核stdout/stderr投影，不把返回non_blocking_error伪称throw。
- background：`src/agent/sub/BackgroundSubagentRuntime.ts:45,63-80,102-116`真start/run/stop/stopAndWait；指定child调用networkFetch(GET,timeout60000,retry1/base500/cap5000)。这组参数属于指定child，不是所有后台任务的网络策略。网络结果和task.status/error/ended/stopped分别观察。

Rust web/background调用公开真实实现；私有fetch_redirect/network经`#[path]`编译原文件。
web `FetchError::Other` 现在保留 TS 的 `Error.name`（`NetworkFetchError` / `TypeError` /
plain `Error`），未知宿主错误仍是null，绝不根据message补造类型；`fetch_tool` 仍只把
message 包进 `tool_execution_failed`，与 `webFetch.ts` 的工具错误形状一致。
background timer的redirect首跳ready、末跳after元数据保留；断言禁止将timer隐藏于io/microtask。
MCP通过cfg(test)模块调用真实 `network_fetch_continuing_with` + `fetch_with/follow` + `streamable_text`；hooks调用真实 `execute_with` + 既有 `ExecutionResult::failure(false)` + `to_value`，完整保留返回对象。测试不声称私有seam覆盖public catch调用位；既有hooks_runtime真实execute回归另验。
MCP timer traversal仅把已经同步走过的真实follow结果连同唯一末跳timer元数据交给caller；不会把timer藏在microtask里。真实I/O另走socket路径。每跳检查请求路径，次数/重试仍严格比较完整observable。
MCP retry真实带500..624ms随机jitter；TS probe固定Math.random=0。测试以独立Wake观察caller真实timer，同起点直接await500ms校准Sleep，只接受同tick唤醒的候选，最多4096次，未选候选drop且不发第二次请求。保留随机策略，不注入Retry-After；采样证据为RETRY_SAMPLE行。

## timer、真实I/O、生命周期

Timer probe在Undici dispatcher注入完成/body事件，真实fetch promise、redirect和错误归一化照常执行。
原生Node timer保留requested/effective/_idleStart/_idleTimeout及注册序；完成20ms、retry500→20、idle watchdog→40，其他真实request/hop watchdog→1000。
父deadline为目标±5ms或同刻；忙等55ms后按真实deadline和注册序筛候选，跨tick重跑最多10次。
每格3s、整probe180s，finally回收自有timer。Rust暂停时钟使用原始逻辑时长。

`timing.source`区分baseline retry timer、baseline request/hop timeout和fixture-injected completion/watchdog。
裸fetch_redirect idle及hooks idle、所有headers后body idle使用注入600000ms watchdog；不能据此宣称真实Agent600000ms超时已测。
真实request idle已测MCP GET10000、POST/background60000；headers后networkFetch移除父监听，仍单独测body。
redirect首跳body故意不结束而下一跳必须到达。pr1无watchdog80ms观测只证明观察窗内pending，原记录见pr1 RESULT。

`io.mts`用真实Undici Agent与127.0.0.1 HTTP server，URL仅fixture.test并在transport层映射回环。
真实onResponseStart/onResponseError到达后设门；idle只见peer-request、没有响应。C=release并排空真实续体后abort；
P=abort排空后release；PF/PL=同JS栈abort/release两序，**不是libuv同毫秒timer**。
trace守卫核真实peer、wire、唯一gate/release/parent、顺序及child-settled/retry-armed见证。
Rust用真实Hyper socket先取得结果，再由poll门控制ExchangeFuture::io与父取消；显式3s有界等待，最终回收server/driver。

四个background生命周期格：pending stop不运行child；fulfilled/rejected settled后stop=false且终态不变；
不合作child收到stop后stopAndWait仍Pending，释放gate后才完成。保存afterStop与endedUnchanged，Rust释放前两次核Pending。
这些是有界观测与显式收尾，未把永不释放的任务宣称可自动结束。

## 生成与重生成

sourceSha256覆盖六份入口源文件，dependency fingerprint覆盖九个Undici文件与SDK streamableHttp一项。
生成前后核固定commit、源码与依赖哈希。raw.ndjson保留绝对timer/timeline/trace；oracle只移除这些绝对或调度诊断记录，保留全部语义观察。
生成器目录recursive:true、三个输出文件flag:'wx'；已有证据拒绝覆盖。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
export TSX_DISABLE_CACHE=1
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/network-timing/generate.mts /fresh/output
export CARGO_TARGET_DIR="$PWD/_build/cargo-target"
scripts/cargo-slots.sh
cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-gateway --test network_timing
# 同样先检查cargo槽位；TMPDIR可指定证据目录中的scratch
cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-gateway --test network_timing \
  network_timing_regenerates_identically -- --ignored --exact
```

既有默认ignore的重生成守卫复用同一network-timing fixture，重新调用生成器并逐字节比较全部336格。
integration默认171通过=163对齐+6overdue+1差异守卫+1registry负控，22忽略=21差异+1重生成；两个private lib模块默认114通过=111对齐+2差异守卫+1metadata/trace负控，41忽略=41差异。忽略不等于已批准或阶段通过。
同样先核cargo槽位：`cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib network_timing_tests`。

## 差异与负控

known_differences.json是TS原值/Rust当前原值/category/test的单一维护表。现62格=pr1的15+pr2a的6+pr2b的41。
历史账本字节不变；pr2b按根因新增PARENT-TIE-002、RETRY-TIE-002、POST-HEADERS-PARENT-003、ERROR-CLASS-003未批准条目。
#752 修复 web 错误对象的 TS 类型名后，ERROR-CLASS-001（23格）与 ERROR-CLASS-002（11格）
从表中删除并解除 ignore，更正见两份 R1；PARENT-TIE-001 的 2 格、POST-HEADERS-PARENT-001
的 5 格因修复后 name 变化同步 Rust 快照（仍因注册序/headers 后取消保留差异），见各自 R1。
每格严格TS断言并独立ignore；共享registry核三处源码的全部336格集合、测试属性、两个cfg(test)模块接线、TS原值、TS≠Rust、ignore集合/理由和函数格ID绑定。三个默认反向守卫按所属路径逐格重放Rust，未将缺失格过滤成通过。
改变差异表现或修复后仍保留ignore都会失败；不改产品来迁就期望。

`negative_controls.py /fresh/evidence [control...]`临时变异产品源码，要求独占树；使用mutating_sources验证基线（默认HEAD，未提交候选可用`PDRS_NEGATIVE_BASELINE`指定候选树对象）、锁和恢复，
cargo均经wait_for_slot/run_owned，正控→预期测试断言失败→恢复正控。每项绑定测试名与断言锚点，编译失败不算。
新增background signal/同步取消/未启动/等待完成/done/ended/完成态/错误内容负控，及真实I/O完成源和同步取消结果负控。
#752 另加错误类型 name 负控：`error-name-network`（From<NetworkFetchError>）、`error-name-body`
（body Terminated → TypeError）、`error-name-body-aborted`（body Aborted → NetworkFetchError，经默认
`known_differences_remain_exact_and_ignored` 重放）、`error-name-plain`（plain Error），以及
`error-name-invalid-url-redirect` 与 `error-name-invalid-url-upgrade` 两处 `Invalid URL` 的 TypeError；
`lib::` 选择走 `--lib`。
四组整矩阵负控实际失败格清单在summary.json；逐格关键覆盖见交付negative-coverage.json。

`negative_observations.mts <raw.ndjson>`只改内存中的原始记录，每条新增守卫、新源码/依赖项和每个新增矩阵项均有负控；
每次拒绝变异后重新验证原始完整336格。原始TS文件、金样和记录不被该脚本改写。


`negative_private.py --catalog`列出pr2b的源码变异、具体测试名及断言锚点；运行方式为
`negative_private.py /fresh/evidence [control...]`，必须由调用方设置CARGO_TARGET_DIR，不另建仓库或target。
本包未提交时mutating_sources会因cfg(test)接线不匹配HEAD而拒绝全部产品变异；提交后由协调者在宿主执行，不能把拒绝算负控通过。
内存负控不改源码/金样：registry的194项覆盖新41条差异的TS值/反向值/test/category绑定、三个测试文件属性、三处反向回放守卫及两模块接线；private metadata/trace的848项覆盖所有116个timer格的6个字段、全部152个请求路径。每次必须在绑定断言失败且恢复原值后通过。
