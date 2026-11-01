# 后台 panic 边界金样

基线为 PilotDeck `cc7c4e6`，使用真实 `BackgroundTaskRuntime.dispose/settleEntry` 和
`BackgroundSubagentRuntime.start/stopAndWait`。`generate.mts` 校验 commit、工作树和源码
sha256；输出目录 recursive 创建，oracle 以 `flag:'wx'` 写入，禁止覆盖原始采样。

snapshot 四族各 24 格：`write-a`（首条失败）、`write-b`（尾条失败）、`flush-empty`
（没有 entries）、`flush-pair`（a、b 两条）。每族展开观察者数 0/1/3、清理前/后到达、
sync throw / poll reject / 普通错误 / 成功。TS 的 poll 和 err 都通过 Promise reject；
Rust 分别注入 Future poll panic 和 io::Err。TS 两条 settleEntry 通过显式 predecessor
串行，证明 write:b 确实发生在 a 之后；这不是 TS dispose 自动串行写入的断言。

每格有独立 Rust 测试 `runtime::background::panic_tests::snapshot_<族>::<格>`，
逐条比较 calls，并验证取消首观察者、所有等待者通知、晚到观察者和 Disposed 状态。
start/storage 前驱先保持不完成，再显式 release 或 drop sender；仅测试 deadline 为
Node 10 秒、Rust 每次等待 2 秒。subagent 六格覆盖独立 sync/poll 文案、取消前/中、
成功/普通错误。非字符串 panic 和没有 panic payload 的兜底是 Rust 专属测试，不声称
对应 JavaScript 任意对象的 String 转换。

## 回放边界

`diagnostics` **只记录 TS 参考结果，不在 Rust 回放**。Rust 当前
`BackgroundTaskRuntimeOptions` 没有 `on_diagnostic`，因此没有对应的可观察回调；
为此添加公开回调会扩展产品 API，#799 采用 issue 明确允许的文档说明方案。
此金样不证明两端诊断事件等价。旧金样实际有 18 格非空 write diagnostics（issue
正文写 20）；扩展后有 36 格，均为 write-a/write-b 的三个失败模式。

calls 对比仅针对本夹具的受控序列：TS 写入来自 settleEntry，Rust 来自 dispose
最终写入，不能据此声称两个调用点或持久化生命周期全面等价。flush 失败契约的原登记
差异（Rust 吞错返回 ()/Disposed）**已由 R2 消除**：dispose 现在拒绝并保持
draining，详见 `specs/gateway-parity/diffs/A6.5-BACKGROUND-PANIC-001.md` 及其
R1 / R2 更正（R3 更正记录笔误与证据路径）。

全体 102 个 case ID 必须与显式笛卡尔积严格相等；先校验全集，再选择单格，因此
未知 family、漏格、重格都不能静默跳过。`background_panic_regenerates_identically`
是现有 `#[ignore]` 重生成守卫，扩展后仍比较整个 oracle 字节。

## 重生成与负控

设 `PDRS_BASELINE` 指向只读基线，使用 Node v22.22.0：

```sh
node --disable-warning=ExperimentalWarning --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/background-panic/generate.mts <新目录>
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib \
  background_panic_regenerates_identically -- --ignored
```

负控必须由调用方设置 `CARGO_TARGET_DIR`，先提交待测源码，再运行既有 runner。
它使用 `mutating_sources`、`run_owned`、`wait_for_slot`，锚点过期立即报错退出，
要求真实运行时断言失败并在恢复后通过；编译失败不计 KILLED。

```sh
python3 src/pilotdeck-gateway/tests/fixtures/background-panic/negative_controls.py \
  <新证据目录> --check-anchors
python3 src/pilotdeck-gateway/tests/fixtures/background-panic/negative_controls.py \
  <另一新证据目录> --only empty-flush-join
```

每个负控只跑一个 Rust 格；省略 `--only` 跑全部。源码未提交时 guard 拒跑，应交协调者
提交后宿主运行，不能建副本仓库或替换 guard 的 baseline 来规避。
