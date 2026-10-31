# session-domain-native 源码负控（#1140）

绑定对象：`scripts/gateway-parity/session_domain.py` 的 #1140 新增行——
`load_native_capture` 的预注册 binding 拒收、driver 台账/出网见证交叉核对、
manifest 相等（fail-fast 层），`audit_native` 的 `bindingSource` 溯源 require，
以及 `deadline_report` 的两处共享聚合调用——与
`scripts/gateway-parity/controls/session_domain.py` 的 `NATIVE_DEADLINE_CASES`
数据负控（含 `native-shrunk-manifest`）和通用 `forged-binding-source`。无
cargo、无全量对拍：控制只离线重放合成 capture fixture。

## 文件

- `run_control.py` — 单控制运行器：按显式路径加载 controls 条目并调用
  `check_case`，输出与 controls CLI 相同形状的 verdict JSON。它不走 CLI
  `main()`：变异中的 `session_domain.py` 与 HEAD 必然不同，而 `main()` 先跑
  `verify_loaded_apparatus`；直接绑定 `main()` 自己调用的 `check_case` 是
  #209 / #1177 已确立的形态。
- `negative_controls.py` — 源码变异元负控：`MUTATIONS` 六行。每行要求：
  baseline 未变异时被绑控制按注册规则 FAIL；mutant 下同一控制的 verdict 按
  `MUTANT` 表精确变化（多数是控制自报 `mutation accepted`；binding-source
  删除使 `forged-binding-source` 直接通过；批次旁路被每个 native 控制的健康
  前置抓住；写死 native 格状态被 `native-surface-mismatch` 自己的断言抓住；
  manifest 相等删除后由既有 `audit_native` COVERAGE 影子以不同 detail 拒收，
  证明快线失效也不开洞）；另有一条不相关控制在该 mutant 下仍被杀（防串线，
  批次旁路除外——其健康前置本身就是绑定观测）；恢复后复杀。
  `mutating_sources`（带 common.py 与 parity 租约）守源码并逐字节还原，
  summary 原子发布、guard 失败全 INVALID（#979/#1037）。

## 运行

```sh
python3 -I src/pilotdeck-gateway/tests/fixtures/session-domain-native/negative_controls.py \
  --check-anchors
# 提交后由协调者跑；调用方必须已提供 CARGO_TARGET_DIR（不创建第二个构建目录）：
python3 -I src/pilotdeck-gateway/tests/fixtures/session-domain-native/negative_controls.py \
  --output <新证据目录>
```

`mutating_sources` 要求被守文件等于 `--baseline`（默认 HEAD）：提交后在宿主
跑；未提交树上 SESSION/COMMON 若仍等于 HEAD 可预演，但不冒充 HEAD 准入。
`--only <name>` 可只选一条。fixture 数据负控（九项 native/audit 控制）由官方
`controls/session_domain.py --case <name>` 在提交后跑。

## 已知边界

- loader 的 manifest 相等 require 是纵深防御：`audit_native` 的既有 COVERAGE
  require（HEAD 已有）在任何它拒收的输入上同样拒收，因此单删 loader 行的
  mutant 预期是被影子以不同 detail 拒收，而不是 `mutation accepted`；若影子
  也被同时删除，控制会自报 mutation accepted，本套件按失败处理。
- `bypass-batch-aggregation` 没有不相关控制项：所有 native 控制的健康前置
  （native 格全 PASS 时批次必须 NOT_RUN）都会先触发，这本身就是 #892 要求的
  运行时观测；提交后 `test_safety.py` 的 #1140 块从真实 `main()` 出口再钉
  一次（报告状态等于共享聚合输出）。
