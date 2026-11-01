# 决策卡：#1231 U3 snapshot 生产入口暂不接通（AI 代批，待人复核）

- **卡在哪**：r3 DeepSeek 指出 native isolated 模式拒绝继承 `PILOTDECK_STORAGE_ROOT` / `PILOTDECK_SNAPSHOT_ROOT`，且 `Options::parse` 没有存储根参数，生产进程里 U3 snapshot 恒禁用；plan / 账本却写「非空路径 snapshot 可用」。
- **谁受影响**：用 Rust 原生进程的用户在本包合入后看不到 snapshot_* 三项能力（describe 10 项），与 TS 设了存储根时的 13 项不同；TS 默认（未设存储根）同样禁用。
- **能否撤回**：能。后续加显式参数即可接通，不动尺子、不动意向。
- **选项**：A 本包新增 `--storage-root`（隔离授权、仍拒绝继承）；B 本包如实写明生产禁用、13 项只在测试配置出现，生产接线另开跟进单。
- **选 B**：本包范围是「接通路由」，CLI 参数面属于接线包；B 可撤回、改动小，不在修复轮最后一轮扩大范围。
- **原则**：修复轮最后一轮不扩大 CLI / 配置面；能力缺口如实登记并开跟进。
