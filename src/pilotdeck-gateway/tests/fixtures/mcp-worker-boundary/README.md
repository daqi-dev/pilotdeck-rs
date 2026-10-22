#394：真实 PluginToToolBridge 执行普通父目录链接、符号链接与 FIFO 的内容投影；所有文件均属临时测试目录。TS 的零延时 timer 在同步读取前不运行。Rust 仅保持输出投影一致，并将 stdio 的文件读取移入 spawn_blocking，调度时机有意改变。cwd 逃逸行为只核实，未在本单修改；不将任意真实本机文件放入金样。

Node 22.22.0，固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。生成命令（仓库根执行，输出目录自定）：

```sh
PDRS_BASELINE=/absolute/path/PilotDeck node --import /absolute/path/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/mcp-worker-boundary/generate.mts /tmp/pdrs-mcp-worker-boundary
```

四组金样均由 `tests/p2_sweep_boundaries.rs::boundary_fixtures_regenerate_twice_from_real_ts` 再生成两次并与入库字节比较（需显式运行 ignored 测试）。
