# 交互 producer 值金样（#678）

固定 cc7c4e6，Node 22。`generate.mts` 直接 import 未改写的真实 permission / elicitation bus，
经真实 reconnect 和 replay 转换录制 210 格（35 种值 × 根/对象/数组 × 两种 bus）及 10 格错误顺序、22 格空 replay 桶生命周期。
共享 baseline guard 前后核提交、tracked clean 与四份来源 blob；oracle 不手写。
`$number: "-0"` 是观测标签，避免 JSON 录制把负零压成正零。
每格比较完整 error、snapshot、count；无效 Date 不留 pending；重复校验先于转换，失败可重试。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/interaction-values/generate.mts /tmp/interaction-values-new
CARGO_TARGET_DIR="$PWD/_build/cargo-target" cargo test --offline --manifest-path src/Cargo.toml \
  -p pilotdeck-gateway --lib runtime::interaction
python3 src/pilotdeck-gateway/tests/fixtures/interaction-values/negative_controls.py \
  _tmp/20261026-issue-678/negative-new
```

负控须独占写树与 cargo：每次一个编译成功但断言失败的变异，finally 逐字节还原并保存 SHA-256；
编译失败/信号/超时不算检出，全部还原后重跑正控。复现输出目录必须不存在。

生产调用 `GatewayPermissionBus::register_producer` / `GatewayElicitationBus::register_producer`；
可单独调用 `to_interaction_replay_value`。arena 数组/对象索引保留共享/循环身份；Map/Set/类传
自身可枚举字符串字段（不传内部 entries、不可枚举或原型字段）。toJSON 作为 Function 不调用。
`Value(JsValue)` 为已有 wire 子树，容器同样参与身份去重；任意宿主 getter/Proxy/稀疏数组洞的
执行模型不在此数据表示内，producer 须先取得自身字段，不能把本包表述为 JS 执行器。
本包提供接线入口，尚未将未来宿主生产方迁到该入口；不代表 #589/#590 或阶段验收完成。
