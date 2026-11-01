# #1265 RegExp 验收 fixture

`regress-oracle.json` 是 Git `2c2e4f87` 的 `output-schema/regress-oracle.json` 经固定 Node v22.22.0 / ICU 78.2、只读 TS 基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的真实 SDK 重生成，108 条和旧文件逐字节相同。`generate-sdk.mjs` 恢复同提交 `generate-regress.mts`，仅加强精确版本、独占写入与导出结果；保留 provider/Ajv/formats 哈希、版本及 sourceClosure 守卫。

`report-cases.json` 保留 #685 报告的 25 条输入，来自该任务证据 `cases.json`。本轮读取的原件为 `/Users/da/ws/pdrs-worker-wt/issue-685/_tmp/20261026-issue-685/`，详细哈希写入本单 RESULT 的来源记录；它不是手写期望。

`generate.mjs` 调用真实 Node RegExp 生成 `oracle.json`：108 SDK 输入的 RegExp 结果、25 报告样本加 75 个 flags/起点/捕获/代理项样本，以及 254/255 捕获组边界。全部字符串转为 u16 数组，避免 JSON→Rust String 丢失孤立代理项。新增金样存原始 Node 观察值，已知 SDK URIError 和 QuickJS 255 捕获组差异由测试明确断言；不是接受差异账本或生产兼容性放行。

`src/pilotdeck-regexp-proto/src/tests.rs` 的 ignored `fixture_regenerates_identically` 使用 `regenerates_identically("rquickjs-regexp")`，由 `regenerate.py` 在独占临时目录、有界进程组中生成两份金样，再逐字节比较。生成器 mkdir 使用 recursive，输出文件使用 wx，拒绝覆盖已有证据。

```sh
export PDRS_BASELINE=/Users/da/ws/pdrs-runner/resources/PilotDeck
# 将经过验证的 Node v22.22.0 / ICU 78.2 放到 PATH 首位。
export LC_ALL=en_US.UTF-8
cargo test --locked --manifest-path src/pilotdeck-regexp-proto/Cargo.toml --lib tests::fixture_regenerates_identically -- --exact --ignored --show-output
python3 src/pilotdeck-gateway/tests/fixtures/rquickjs-regexp/negative_controls.py --check
```

负控必须在协调者提交新增文件后、源码等于 HEAD 的独占树运行；脚本复用 `negative_process.mutating_sources/run_owned/wait_for_slot` 与 `negative_suite` 的判杀/汇总，不允许影子 Git、复制仓库或替换索引。每个变异先正控，再 `cargo test --no-run`，之后要求指定测试及指定 #1265 断言失败，恢复后复跑正控；全部变异结束后汇总，任何非 KILLED 均失败。`--compile-only` 可单独收集逐变异编译证据。所有 cargo 继承调用方 `CARGO_TARGET_DIR`，未设置则报错，不使用 quiet 模式。输出目录必须全新。

```sh
python3 src/pilotdeck-gateway/tests/fixtures/rquickjs-regexp/negative_controls.py /ABS/NEW_NEGATIVE_OUTPUT
# 单项样本或单独编译；目录仍须全新：
python3 src/pilotdeck-gateway/tests/fixtures/rquickjs-regexp/negative_controls.py /ABS/NEW_SAMPLE_OUTPUT disable-interrupt
python3 src/pilotdeck-gateway/tests/fixtures/rquickjs-regexp/negative_controls.py --compile-only /ABS/NEW_COMPILE_OUTPUT
```

Linux 由协调者在已授权 Linux 源码树执行同一测试、clippy、重生成、负控与 release 构建；不要下载预编译引擎/工具链。release 可执行文件无参数运行 100 个独立请求，输出 `requests=100 matches=100`，用于记录尺寸和一次运行的峰值 RSS。macOS 的测量不代表 Linux，也不代表产品接入增量。测试原始结果与平台、rustc、依赖锁、命令一起留档。
