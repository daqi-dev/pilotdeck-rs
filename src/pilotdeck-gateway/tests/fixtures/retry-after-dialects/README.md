# Retry-After 方言：#360 第 2 条

Node **v22.22.0 / ICU 78.2 / tzdata 2025c**（icu78 源码构建校准链
`~/opt/node-v22.22.0-icu78-*`；官方预编译 v22.22.0 自带 ICU 77.1 / tzdata 2025b，
本机 homebrew node@22 是 ICU 78.2 / tzdata 2026a——`stats-dateparse` 金样即按该
homebrew 链录制，其 `tzdata: 2026a` 是 homebrew Node ICU 的 `process.versions.tz`
（`stats-dateparse/generate.mts:195`），不是生成机的系统 TZif（2026d）；两份
2026a/2025c 都不是系统时区，维度见决策卡 `state/decisions-20261031-node-baseline.md`），
固定 TS **cc7c4e65**（`src/model/protocol/errors.ts` 的
`parseRetryAfterHeader`，`Date.now()` 打桩为 0）。覆盖 `Date.parse` 全方言：
delta-seconds 的 `Number` 陷阱（`2015` 是数、`  ` 修剪后是 0、`0x10` 是 16）、
IMF-fixdate 边界（两位年份、±8.64e15、24:00:00、秒 ≥60 转投 day 槽）、
RFC 850、asctime、ISO（date-only 走 UTC、无时区走本地）、命名时区、数字偏移、
不可解析余数。

生成器父进程分别以 `TZ=UTC` 与 `TZ=Asia/Shanghai` 跑子进程，oracle 不依赖
生成机的本地时区；本地解析的方言（asctime 两条、无时区 ISO 一条）落在上海的
平坦 +08:00 年段，回放用固定 −8h 注入解析器。生产入口的本地解析走进程时区，
但只经与 stats 保留判定同一道已接纳 TZif 门槛（`stats::admitted_local_to_utc`，
#360-2 fix-r2）：三标识 allowlist、未设 TZ 时 `/etc/localtime` 规范链接、1 MiB
读上界、`tests/fixtures/stats-dateparse/local-profiles.json` 的 SHA 配对。子集
之外的环境（POSIX TZ、自定义路径、TZif 不可读、可读但未对拍、`TZ=localtime`
等宿主相关别名）按不可解析处理、忽略该 header，而 Node 会继续解析（回退 UTC
或按宿主 C 库 zone 搜索；Node v22.22.0 实测：`TZ=unsupported-profile`、
`TZ=:/nonexistent` 下 `Date.parse` 与 `TZ=UTC` 同值；`TZ=localtime` 在有
`/usr/share/zoneinfo/localtime` 的机器走本地、没有的机器回退 UTC，故不可对拍）
——这是 `retry_after_header` 全部消费侧的保留差异子集（provider 流重试，以及
共享 `retry_after_header_ms` 的 MCP HTTP 重试、web 网络重试与 web fetcher 的
`retryAfterMs` 错误字段，TS 对应 `streamModel.ts` 与 `network/fetch.ts` 自带的
同语义 `parseRetryAfterHeader` 副本、`urlFetcher.ts`），登记于
`specs/gateway-parity/diffs/A6.4-PROVIDER-RETRY-AFTER-TZ-001.md`（未批准；沿用
与 A6.4-STATS-RETENTION-001 相同的 TZif 子集，但那条的人批范围只有 stats
retention，不覆盖本消费侧）。子进程测试
`retry_after_dialects_production_wiring_matches_node_zone_children` 经生产入口在
`TZ=UTC` / `TZ=Asia/Shanghai` 子进程里回放全部 62 条——仅当宿主 TZif 与
`local-profiles.json` 的 SHA 配对时；未配对的宿主由子进程打印可机检的
`#360-2 PARTIAL` 降级标记行、父测试整段回放子进程 stdout（`-- --nocapture`
下可见，fix-r3 P2-2），并按 fail-closed 断言本地用例为 `None`，不冒充 Node
对拍通过；负控 `negative_controls.py` 检测到该标记即把
`production-local-wiring` / `zone-child-admission-false` 显式拒跑为 INVALID
（fix-r3 P2-3）。`TZ=localtime`（allowlist 拒绝的宿主相关别名）、
`TZ=:/nonexistent`（必为不可读路径，不假设宿主缺少某 zone 名）与
`TZ=Europe/London`（非 allowlist 但宿主可读的 zone）子进程断言本地用例
`None`、时区无关用例等于 UTC 列；对可读别名还会先断言无门槛共享解析器能解析
宿主 profile（同 stats `dateparse_zone_child` 的做法），确保 `None` 来自
allowlist 拒绝而非文件缺席——`Europe/London` 因此不依赖 `/etc/localtime` 就能
钉死 allowlist（fix-r3 P2-3）。

超大 delta-seconds（`"1e308"` / `"1e309"`）不入 oracle：`JSON.stringify(Infinity)`
是 `null`，与不可解析的 null 无法区分。由
`retry_after_dialects_huge_delta_seconds_parse_to_infinity_and_clamp` 直接单测钉住：
`"1e308"` 解析为 `Infinity` 毫秒（真实 TS 实测同值）、`"1e309"` 不是有限
delta-seconds（TS 返回 `undefined`）、`decide` 把 `Infinity` 钳到 `maxDelayMs`
（默认 8000），不会进入 `Duration`。

```sh
export PDRS_BASELINE=<PilotDeck 基线仓库>   # 校准宿主 clawbox-08 为 /home/clawbox-08/ws/pdrs-runner/resources/PilotDeck；icu78 校准 Node 为各宿主 ~/opt/node-v22.22.0-icu78-<target>
PATH=<icu78 校准 node v22.22.0>/bin:$PATH node --disable-warning=ExperimentalWarning \
  --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/retry-after-dialects/generate.mts <新输出目录>

scripts/cargo-slots.sh
# zone children 的降级 PARTIAL 标记行只在 --nocapture 下可见（同 stats-dateparse）
cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib retry_after_dialect -- --nocapture

scripts/cargo-slots.sh
cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-gateway --test model_stream \
  retry_after_dialects_regenerate_identically -- --ignored --exact
```

生成目录 `recursive:true`、金样 `flag:'wx'`，不覆盖已有输出。回放模块经
`#[path]` 挂在 `runtime/model/http.rs`（cfg(test)），断言消息带 `#360-2` 绑定
标记；快速回放同时断言 oracle 的 `sourceCommit` / `sourceSha256` 与
`node` / `v8` / `icu` / `tzdata`（provenance 不只依赖 `#[ignore]` 的字节再生
守卫，fix-r2 P3-1）。负控见 `negative_controls.py`（提交后宿主串行跑）。

再生守卫对运行时 provenance 分两维（regen-r2，评审 P2b→P1 修法 C）：
`model_stream.rs` 的 `retry_after_dialects_regenerate_identically` 与
`generator_provenance.rs` 的 `retry_after_dialects_regenerates_identically`（同一
守卫的两个测试二进制孪生）先探测本机 Node 的 `icu`/`tz`，再决定怎么比——
**ICU 78.2 是硬钉**（`generate.mts` 断言 `process.versions.icu`，守卫也先断言，
不符即失败：不同 ICU 构建可能改变 `Date.parse` 本身，决策卡 2026-10-31）；
**tzdata 只记录不断言**，本机 tzdata 与金样记录的 2025c 不同时（如 homebrew
78.2/2026a），守卫打印可机检的 `#360-2 BLOCKED: tzdata <本机> != <金样> …`
并跳过字节比较而不误报 FAIL——语料的本地解析用例都落在上海平坦 +08:00 年段，
2026a 上 62×2 值逐字相同（r5 评审内存回放实测），差异只在元数据一行；相同时
照旧逐字节比较。快速回放对 oracle 元数据（含 `icu`/`tzdata`）的断言保持严格：
它钉的是 committed 金样的记录值、与宿主无关，篡改 committed `tzdata` 行只有该
断言能杀（再生守卫会因记录值≠本机而 BLOCKED）。负控检测到 `#360-2 BLOCKED`
即把 `generator-epoch` / `oracle-asctime-2015` 显式拒跑为 INVALID（regen-r2，
同 fix-r3 P2-3 的 PARTIAL 机制）。
