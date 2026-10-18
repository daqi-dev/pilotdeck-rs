# 工具运行值夹具

`oracle.json` 来自固定 TS 基线真实运行：71 条错误恢复及完整错误结果、126 条工具名修复、16 条 deadline 预算和 66 步注册表状态序列。64 个 builtin alias 的键全部覆盖，另覆盖备用目标。

```sh
PDRS_BASELINE=/absolute/fixed/PilotDeck node --import /absolute/fixed/PilotDeck/node_modules/tsx/dist/loader.mjs src/pilotdeck-gateway/tests/fixtures/tool-runtime-values/generate.mts /absolute/output-parent
```

生成器检查 `bindSourceTree`，在输出父目录内用 `mkdtemp` 创建独占目录并打印其路径；不执行删除。连续运行两次后比较两份 `oracle.json`，字节一致才更新本目录正本。运行需要已有 Node 22 和基线的 tsx，不安装依赖。

输入中的 Unicode 孤代理项与非有限预算标签是有意边界；Rust 回放使用 `JsValue`，不能先经丢失 UTF-16 信息的字符串转换。`recovery-inputs.json` 只有输入，所有期望由真实 TS 函数产出。
