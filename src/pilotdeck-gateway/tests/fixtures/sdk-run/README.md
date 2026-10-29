# #1058 run 负控

产品/金样说明见 `src/pilotdeck-cli/tests/fixtures/sdk-run/README.md`。

- `python3 negative_controls.py --check`：只读检查每个锚点恰好一次，过期非零退出。
- `python3 negative_controls.py --compile-only OUTPUT`：逐项应用、no-run、恢复，全部汇总。
- `python3 negative_controls.py OUTPUT drop-stdin`：正控 → 编译变异 → 精确测试/nonce收据 → 恢复 → 正控。
- 任何 mutation/source guard/编译/目标断言缺失均为 INVALID，不作 KILLED；全部变异结束再汇总。
- `CARGO_TARGET_DIR` 必须由调用方提供，不创建第二构建目录。源码与 HEAD 不同（含新文件）即拒绝，不绕守卫。

本单未提交，故本地样本被 HEAD 守卫拒绝。编译全部变异与 KILLED 证据交协调者提交后采集；不可把锚点通过或正常产品 no-run 当成变异可编译证明。
