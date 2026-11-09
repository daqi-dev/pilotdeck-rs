# skills-fr-ca-probe 负控（#2140）

`negative_controls.py` 是 `src/pilotdeck-cli/src/skills.rs` 里 `collator()` 的
HEAD-only 变异负控，绑定唯一被点名测试
`skills::tests::fr_ca_secondary_matches_recorded_node_probe`
（回放 `../skills-fr-ca-probe.json` 记录的 Node v22.22.0 / ICU 78.2 原始输出）。
`pilotdeck-cli` 是 binary-only 包，单元测试跑在 `--bin pilotdeck` 目标下。

两条变异各绑一条 `#2140` 专属断言消息（测试内失败点均已去裸 unwrap）：

- `fr-ca-repair-dropped`：删掉 `collator.with_backward_second_level(true)`，fr_CA 行
  `coté` vs `côte` 符号翻成 -1，绑 `#2140 fr_CA.UTF-8: "coté" vs "côte"`；
- `fr-ca-region-widened`：去掉 `region == "CA"` 合取项（放宽成 `language == "fr"`），
  fr_FR 行被误修复成 1，绑 `#2140 fr_FR.UTF-8: "coté" vs "côte"`。

用法（提交后由协调者在宿主跑；未提交源码会被 HEAD 守卫拒绝）：

```sh
export CARGO_TARGET_DIR=<本树独占构建目录>
python3 src/pilotdeck-cli/tests/fixtures/skills-fr-ca-probe/negative_controls.py --check-anchors
python3 src/pilotdeck-cli/tests/fixtures/skills-fr-ca-probe/negative_controls.py \
  _tmp/<轮次>/negctl-out
```

逐变异先跑正控、再编译（`--no-run`）、跑负控、恢复并复跑正控，全部汇总后才判定；
任一非 KILLED 即非零退出。锚点过期或清单为空都报错退出，不会记成 SURVIVED。
`--compile-only` 只做变异体编译确认。输出目录必须不存在；`CARGO_TARGET_DIR` 必须由
调用方设置。共享执行器 `negative_process.py` / `negative_suite.py` /
`negative_panic.py` 归 M6 车道，本脚本只导入不修改。
