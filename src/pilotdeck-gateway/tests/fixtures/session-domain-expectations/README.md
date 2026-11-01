# session-domain-expectations 负控（#1177 fix-r1，#1190 补齐 require 覆盖与计数推导）

绑定对象：`scripts/gateway-parity/session_domain.py` 的 `validate_expectations`
（A19-HOST 逐格期望的结构校验）与
`scripts/gateway-parity/controls/session_domain.py` 的 `EXPECTATION_CASES`
数据负控。无 cargo、无全量对拍：控制只离线重放记录在案的 A5.2a 数据。

## 文件

- `run_control.py` — 单控制运行器：按显式路径加载 controls 条目并调用
  `check_case`，输出与 controls CLI 相同形状的 verdict JSON。它不走 CLI
  `main()`：变异中的 `session_domain.py` 与 HEAD 必然不同，而 `main()` 先跑
  `verify_loaded_apparatus`；直接绑定 `main()` 自己调用的 `check_case` 是
  #209（session-domain-lock-release）已确立的形态。
- `negative_controls.py` — 源码变异元负控：`MUTATIONS` 各删 / 中和
  `validate_expectations` 里一条 canonical `require`，覆盖全部六条——
  frames、durableStatus、registry events / state / result、inapplicable
  registry（#1177 fix-r1 评审要求三条，#1190 补齐其余三条）。每行要求：
  baseline 未变异时被绑控制按期望规则 FAIL；mutant 下同一控制接受坏
  fixture（KILLED），仍按同规则 FAIL 记 SURVIVED；另有一条不相关控制在该
  mutant 下仍被杀（防串线）；恢复后复杀。绑定表（BOUND / UNAFFECTED）
  由 `check_bindings` 与面板的 `EXPECTATION_CASES` 机检（case 与规则都须
  相符，且每条 `EXPECTATION_RUST_DERIVATION` 控制至少被一档变异行使），
  漂移即 exit 2；`evidence` 在进入守卫窗口前初始化（#1190）。计数不在
  叙述里钉死：运行时由 `ast` 解析（不执行）面板推导并写
  `derived-counts.json` 入证据目录。`mutating_sources`（带 common.py 与
  parity 租约）守源码并逐字节还原，summary 原子发布、guard 失败全
  INVALID（#979/#1037）。

## 运行

```sh
python3 -I src/pilotdeck-gateway/tests/fixtures/session-domain-expectations/negative_controls.py \
  --check-anchors
python3 -I src/pilotdeck-gateway/tests/fixtures/session-domain-expectations/negative_controls.py \
  --output <新证据目录> --source <A5.2a 六格原始输出>
```

`--source` 需要真实 A5.2a 校准输出（`raw/` 下六格 JSON + preregistration /
manifest / launch）。`mutating_sources` 要求被守文件与 `src/` 输入区都等于
`--baseline`（默认 HEAD）：提交后由协调者在宿主跑；脏树预演用
`--baseline "$(git stash create)"`（先 `git add` 让索引一致），但不冒充
HEAD 准入。fixture 数据负控本身（`EXPECTATION_CASES` 全表，计数以运行时
推导为准）由官方 `controls/session_domain.py --case <name> --source …`
在提交后跑。
