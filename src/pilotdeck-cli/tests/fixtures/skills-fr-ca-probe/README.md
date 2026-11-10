# skills-fr-ca-probe 负控（#2140）

`negative_controls.py` 是 `src/pilotdeck-cli/src/skills.rs` 里 `collator()` 的
HEAD-only 变异负控，绑定唯一被点名测试
`skills::tests::fr_ca_secondary_matches_recorded_node_probe`
（回放 `../skills-fr-ca-probe.json` 记录的 Node v22.22.0 / ICU 78.2 原始输出）。
`pilotdeck-cli` 是 binary-only 包，单元测试跑在 `--bin pilotdeck` 目标下。

七条变异各绑一条专属断言消息（测试内失败点均已去裸 unwrap）：

- `fr-ca-repair-dropped`：删掉 `collator.with_backward_second_level(true)`，fr_CA 行
  `coté` vs `côte` 符号翻成 -1，绑 `#2140 fr_CA.UTF-8: "coté" vs "côte"`；
- `fr-ca-region-widened`：去掉 `region == "CA"` 合取项（放宽成 `language == "fr"`），
  fr_FR 行被误修复成 1，绑 `#2140 fr_FR.UTF-8: "coté" vs "côte"`；
- `fr-ca-script-lost`：`script == "Latn"` 门收紧成恒假，fr_Latn_CA 行丢失修复翻成 -1，
  绑 `#2140 fr_Latn_CA.UTF-8: "coté" vs "côte"`（gateway 侧同款
  french-script-fallback-lost；`is_none_or` 对无 script 的 fr_CA 行仍为真）；
- `fr-ca-script-widened`：`script == "Latn"` 门放宽成恒真，fr_Cyrl_CA 行被误修复翻成 1，
  绑 `#2140 fr_Cyrl_CA.UTF-8: "coté" vs "côte"`（gateway 侧同款 french-script-widened，
  即 #2170 gateway 测试钉住的方向）；
- `probe-resolved-rewritten`（#2170）：改写夹具里 fr_Cyrl_CA 行记录的
  `"resolved": "fr-Cyrl-CA"` 为 `"en-US"`，`skills.rs` 的 resolved 自审
  （fix-r5 起为首子标签相等：首子标签 `"en"` ≠ 行语言 `"fr"`）必红，绑
  `#2170 fr_Cyrl_CA.UTF-8: resolved`（`include_str!` 让夹具改动在重编译后生效）；
- `probe-language-emptied`（#2170 fix-r4）：改写 fr_CA 行的 `"lang"` 为
  `"_CA.UTF-8"`（语言子标签为空），`skills.rs` 的空语言守卫
  （`!language.is_empty()`）必红，绑
  `#2170 _CA.UTF-8: probe row needs a language subtag`（同样经
  `include_str!` 重编译生效；删掉该守卫后此行因 panic 载荷缺 marker 判
  INVALID（非 KILLED，与锚点过期同口径，见下方「均不记成 SURVIVED」段），
  钉住守卫本身）；
- `probe-resolved-prefix-poisoned`（#2170 fix-r6）：改写 fr_FR 行记录的
  `"resolved": "fr-FR"` 为 `"frr-FR"`——前缀比较（`"frr-FR"` 仍
  `starts_with("fr")`）会放行、首子标签相等（`"frr"` ≠ `"fr"`）必红，绑
  `#2170 fr_FR.UTF-8: resolved`；把 `skills.rs` 自审退回前缀式则此行
  SURVIVED（对拍回放也过），钉住 fix-r5 的相等判据本身。

用法（提交后由协调者在宿主跑；未提交源码会被 HEAD 守卫拒绝）：

```sh
export CARGO_TARGET_DIR=<本树独占构建目录>
python3 src/pilotdeck-cli/tests/fixtures/skills-fr-ca-probe/negative_controls.py --check
python3 src/pilotdeck-cli/tests/fixtures/skills-fr-ca-probe/negative_controls.py --self-check
python3 src/pilotdeck-cli/tests/fixtures/skills-fr-ca-probe/negative_controls.py \
  _tmp/<轮次>/negctl-out
```

`--check` 与 `--check-anchors` 互为别名（CI #1203 闸门按 `--check` 调用，须与
sdk-stdin / sdk-decode-budget 同口径；`--check` 直读源文件不带树锁，须在无并发
变异的树跑）。`--self-check` 清空清单验证空名册 fail-closed（#1364 P0-a 同款，
CI #1203 的 `--self-check` 循环已纳入本脚本）。逐变异先跑正控、再编译
（`--no-run`）、跑负控、恢复并复跑正控，全部汇总后才判定；任一非 KILLED 即非零
退出。清单与静态名单（`EXPECTED_NAMES`）漂移、未知选项/变异名（含 usage 错误）、
`CARGO_TARGET_DIR` 缺失或取共享规则禁止值、输出目录已存在，都按 rc=2
「从未开跑」拒止；锚点过期是 rc=1（`--check` 直接非零，全量运行记该行
INVALID），与 sdk-stdin / sdk-decode-budget 同口径，均不记成 SURVIVED。变异
写入与逐变异恢复走 `write_source_atomic`（#1903），SIGTERM / SIGHUP / SIGQUIT
触发的退出码（128+signum）原样透传。`--compile-only` 只做变异体编译确认。共享
执行器 `negative_process.py` / `negative_suite.py` / `negative_panic.py` /
`negative_selection.py` 归 M6 车道，本脚本只导入不修改。
