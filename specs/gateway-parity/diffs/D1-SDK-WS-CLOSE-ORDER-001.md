---
id: D1-SDK-WS-CLOSE-ORDER-001
---

## 类别

跨连接 close 观测噪声的精确范围豁免；仅放宽下列实测 UNSTABLE 格的数组次序，不改产品关闭、采集器到达序、逐事件 trace 或其他对拍面。

## 基线依据

真实 TS checkout cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85、Node v22.22.0 / ICU 78.2。96 格中全部 88 个多关闭格，TS / Rust 各重复十次，共 1760 份原始记录。原始 closes、trace、来源哈希与清理在 `_tmp/20261026-issue-1252/fix-r2/repeat/`；逐格分布见 `classification.md` / `classification.json`，TS 880 行全部通过契约及真实文件哈希核验，见 `validation-final.log`。

本轮分类：STABLE-DIFF=16、STABLE-SAME=41、UNSTABLE=31。STABLE 只表示本轮十次结果稳定，不推论所有未来调度。TS 自身已在以下格出现跨连接关闭序翻转；观测点追加全局计数器只能记录回调先后，不能为同时退出 / SIGKILL 的连接创造产品因果序。

## Rust 行为

收集器仍按各 socket actor 送达的 Closed 事件追加原始记录，不排序。比较器只在下列完整 ID 中排序两个比较副本，再比对完整逐连接对象；不去重、不剥实际字段。各条 connection / code / reason / cleanup、关闭数、重复项和额外字段继续严格比较。四格 reason 例外仍仅按 D1-SDK-WS-CLOSE-REASON-001，其范围不因本条扩张。

## 范围

仅以下 31 个完整格名；其他格（包括所有 STABLE 格、空 ID、近似名和未登记 ID）保持严格顺序。此 JSON 清单由离线测试独立读取并与实现白名单核对。

<!-- close-order-ids:start -->
```json
[
  "run--query-resume-hold-sigint",
  "run--query-model-hold-sigint",
  "run--abort-error-sigint",
  "run--second-sigint",
  "run--second-sigint-sigterm",
  "run--terminal-consumed-failed-sigint",
  "run--terminal-during-abort-final-sigint",
  "run--query-resume-hold-sigterm",
  "run--abort-error-sigterm",
  "run--second-sigterm-sigint",
  "run--terminal-during-abort-final-sigterm",
  "run--query-resume-pending",
  "run--query-model-pending",
  "resume--query-resume-hold-sigint",
  "resume--query-resume-release-sigint",
  "resume--query-model-hold-sigint",
  "resume--abort-error-sigint",
  "resume--abort-hold-sigint",
  "resume--second-sigint",
  "resume--second-sigint-sigterm",
  "resume--terminal-during-abort-final-sigint",
  "resume--query-resume-hold-sigterm",
  "resume--query-resume-release-sigterm",
  "resume--query-model-hold-sigterm",
  "resume--abort-error-sigterm",
  "resume--abort-hold-sigterm",
  "resume--second-sigterm",
  "resume--second-sigterm-sigint",
  "resume--terminal-during-abort-final-sigterm",
  "resume--query-resume-pending",
  "resume--query-model-pending"
]
```
<!-- close-order-ids:end -->

`source.json` 仅按纯 TS 条件刷新 run--second-sigterm 的完整原始 capture：新十次稳定但旧单次序不同，统一选 ts-01，不依据 Rust 挑样。此格仍严格比较。混合批次共用完全相同 TS / producer 身份，原件哈希和选择规则见 `reference-refresh.json`；两份相依 oracle 由既有生成器重生。

## 机器判据

`src/pilotdeck-cli/tests/common/signal_replay.rs` 的 `close_rows_match` / `closes_match` 实施比较；`comparison_tests::all_multiclose_cells_follow_registered_order_semantics` 动态枚举全部 88 格（含 26 个观察超时格），逐格验证交换、每个位置的字段损坏、缺条、多条、重复、额外字段、非数组及未登记 ID。`unordered_close_scope_equals_registration` 钉住精确清单和分母。每个名单项各有一条可编译字符串值变异，完整测试名和该 ID 的专属断言绑定在 sdk-signals/negative_controls.py；范围扩大、未登记条目、非数组放行也各有独立变异。原收集器、稳定格 exact / approved-reason 顺序负控继续保留。

TS `contract.mjs` 要求 closes 数组的 seq 严格递增；数据负控 `close-order` 调换两个元素且保持 seq，必须命中专属断言。源级变异按调用方 target、HEAD 守卫、逐项编译与恢复执行；尚未执行项不冒称通过。

## 批准记录

本轮任务书 TASKBOOK-VERSION 77121931cba9 明确授权：先完成双方各十次实测，仅 UNSTABLE 格可改为序不敏感并新增差异登记；STABLE 格保持严格。按该预注册规则执行，不改变冻结尺子的其他语义。#1260 归因按本轮分类修订，见本轮 RESULT；此条不表示 P08 / #123 完成人工实效验收。
