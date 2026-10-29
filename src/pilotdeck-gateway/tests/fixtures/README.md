# Fixtures 负控

## 待重锚（#1048）

产品 PR 仅让既有**源码文本锚点**漂移时，可以在 `pending-reanchor.txt` 追加一行登记：

```text
file-history-cwd/negative_controls.py · processor-message-unscoped · #1234 · #1235 · 2026-10-29
```

上例为格式示意，不是实际登记。五列以 `·` 分隔，依次为相对于本目录的 fixture **脚本路径**、精确变异名（或仅本脚本全部文本变异的 `*`）、漂移来源 PR、重锚 issue、登记日期。只追加本 PR 实际影响的项，不批量登记历史漂移；不允许路径越界、缺项、非法日期、重复或通配符重叠。重锚单必须写明**前置于对应阶段收尾**。

`anchor_selfcheck.py` 对登记的文本计数失配输出 `WARN`，`SUMMARY` 的 `pending-reanchor=N` 为涉及的不同变异数（一个变异有多个锚点只计一次）。未登记失配仍为 `ERROR`。登记项的全部锚点仍按原声明匹配时也输出 `WARN ... remove registration / 删除登记`，提示在重锚单处理遗留登记；不会因仍匹配而自动恢复执行。登记阶段只追加，重锚完成后的清理由对应重锚单处理。登记无法对应已检查变异、解析/清单/环境错误、编译期开关型负控均不享受降级。

宿主跳过逻辑位于 `negative_suite.run_cases`、`negative_suite.matrix_main`、`_shared.negative_control.mutation_suite`：登记项保留在结果中，状态为 **`SKIPPED(pending-reanchor)`**，计数单列 `pending-reanchor`；不变异、不运行该项正控/负控/恢复测试，不计 `KILLED` 或 `COMPILED`。同套件其他变异继续执行，源码与有效登记仍必须通过 `mutating_sources` 的 HEAD 守卫。**有任意 SKIPPED 时 rc=1**（包含全跳过、混合通过、`--compile-only`），只有全部实际判杀或 compile-only 全部实际编译才可 rc=0。无登记时原有判杀与退出规则不变；格式/身份错误非零退出，绝不能借 rc=0 掩盖未跑。

登记脚本必须已接入上述共享跳过入口并列入 `pending_reanchor.TEXT_HOSTS`（本单核验 20 个现有调用方）。尚自管执行或有先行编译/变异、名称展开的脚本，登记会报 `requires a shared skip entry`，须先在对应产品 PR 接通这些路径；本单不批量迁移。`mutant_switch` 编译期开关不适用。普通 CLI 以 `sys.argv[0]` 定位 fixture；导入调用 `run_cases` / `mutation_suite` 时可显式传 `script=__file__`；`matrix_main` 已有显式 script 参数。

## 自测与负控

```sh
python3 -B tests/fixtures/pending-reanchor/test_controls.py
python3 -B tests/fixtures/anchor_selfcheck.py --anchors-only
# 先提交；复用调用方的 CARGO_TARGET_DIR，不另建 git 或构建目录。
python3 -B tests/fixtures/pending-reanchor/negative_controls.py NEW_OUTPUT_DIR
```

以上命令在 `src/pilotdeck-gateway` 下运行。四格及边界自测也由 `tests/negative_anchor_selfcheck.rs` 纳入 Cargo 测试。负控只改 Python 共享件，逐项先 `compile()` 验证语法，再执行绑定自测与恢复自测；不运行 cargo，因此不占 cargo slot。锚点失配、HEAD 不一致、语法错误或非绑定断言失败记 `INVALID`，所有选择项汇总后非零退出；把 WARN 降级逻辑改为 no-op 必须杀于 `#1048 downgrade` 断言。
