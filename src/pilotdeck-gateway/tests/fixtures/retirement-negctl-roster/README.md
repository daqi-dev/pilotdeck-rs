# retirement-negctl-roster — #2378 名册/分母断言的尺子级负控

守护 `scripts/retirement-outcome-parity/` 负控汇总的静态名册 / 分母断言（#2378：
`EXPECTED_CONTROL_IDS` = N1–N17、`EXPECTED_POSITIVE_IDS` = 空集的字面 ID 全集、
汇总前 ID 名册复核、词表外判决折叠 INVALID、counts 整字典断言）。登记值按 main
现状适配（rebase 后）：A8.6-R1 的正控 PC1–PC2 与负控 N18 已随 #2294 落地按反转
条款撤回（A8.6-…-R2），正控侧变异改为「名册虚增」形态。

## 运行（提交后，协调者宿主）

```sh
python3 -I src/pilotdeck-gateway/tests/fixtures/retirement-negctl-roster/negative_controls.py <新的空输出目录>
```

- **必须在 #2378 修复已提交后跑**：`mutating_sources` 把两个目标文件逐字节比对
  HEAD，工作树不干净（修复未提交、或本目录自身不在 HEAD）就拒跑——这正是
  「源码须等于 HEAD」守卫，不得绕过。
- 无 cargo：变异目标是纯 Python 尺子，被测命令是 `python3 -I`；`run_owned` /
  `wait_for_slot` 不适用（`mutating_sources` 自己的扫描器引导不受影响）。
- 一轮全量真实数据（入库夹具 5 轮）约数秒；基线 + 4 变异共 5 轮。

## 判据（每条绑定 #2378 守卫的专属输出，不拿任意非零 rc 充数）

| 变异 | 期望 |
|---|---|
| `baseline`（正控，无变异） | 全量真实数据一轮照旧 PASS，counts 恰为 `{KILLED:17, SURVIVED:0, INVALID:0}` / `{HELD:0, BROKEN:0, INVALID:0}`（尺子改动的真实数据复算；正控注册数现为 0） |
| `roster-drop-n17` | 名册删 N17 → 执行器 rc=2「assembled control ids … do not match the registered roster」，不发布 summary（拒绝而非缩小） |
| `roster-extra-pc1` | 正控名册凭空虚增 PC1（实际构造 0 个）→ 同一守卫，rc=2 |
| `roster-rename-n5` | N5 改名 N4（一重复一缺失，**条数不变**）→ 只有 ID 名册比对能拒；旧条数守卫在此形态下放行（#2378 正文点名的盲区） |
| `verdict-out-of-vocabulary` | 判决记 `MISSED` → 汇总前折叠成 INVALID（reason 保留原判决可审计），counts 整字典不等 → status FAIL、rc=1；没有折叠时该判决不进任何键，「两个零」空真判 PASS（issue 正文现象） |

逐条记 KILLED / SURVIVED / INVALID + 原因，全部跑完再汇总，任一非 KILLED 即非零
退出；锚点过期（old 在当前源里不恰一次）报错退出 rc=2，不记 SURVIVED。
