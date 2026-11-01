# Retry-After 方言：#360 第 2 条

Node **v22.22.0**，固定 TS **cc7c4e65**（`src/model/protocol/errors.ts` 的
`parseRetryAfterHeader`，`Date.now()` 打桩为 0）。覆盖 `Date.parse` 全方言：
delta-seconds 的 `Number` 陷阱（`2015` 是数、`  ` 修剪后是 0、`0x10` 是 16）、
IMF-fixdate 边界（两位年份、±8.64e15、24:00:00、秒 ≥60 转投 day 槽）、
RFC 850、asctime、ISO（date-only 走 UTC、无时区走本地）、命名时区、数字偏移、
不可解析余数。

生成器父进程分别以 `TZ=UTC` 与 `TZ=Asia/Shanghai` 跑子进程，oracle 不依赖
生成机的本地时区；本地解析的方言（asctime 两条、无时区 ISO 一条）落在上海的
平坦 +08:00 年段，回放用固定 −8h 注入解析器。生产入口的本地解析走进程时区
（与 Node `Date.parse` 相同）；系统 TZif 不可读时按不可解析处理（TS 无此态）。

```sh
export PDRS_BASELINE=/home/clawbox-08/ws/pdrs-runner/resources/PilotDeck
PATH=/home/clawbox-08/opt/node-v22.22.0-linux-arm64/bin:$PATH node --disable-warning=ExperimentalWarning \
  --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/retry-after-dialects/generate.mts <新输出目录>

scripts/cargo-slots.sh
cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib retry_after_dialect

scripts/cargo-slots.sh
cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-gateway --test model_stream \
  retry_after_dialects_regenerate_identically -- --ignored --exact
```

生成目录 `recursive:true`、金样 `flag:'wx'`，不覆盖已有输出。回放模块经
`#[path]` 挂在 `runtime/model/http.rs`（cfg(test)），断言消息带 `#360-2` 绑定
标记。负控见 `negative_controls.py`（提交后宿主串行跑）。
