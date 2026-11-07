# TUI slash command TS 原始采集

2026-11-08 在 mbp2 独占目录 `/tmp/pdrs-tui-command-20261108-01` 采集 35 例。`capture.mjs` 从指定固定源码中按唯一边界截取真实 `handleCommand`，以 TypeScript 5.9.3 转译后在独立 VM 注入内存状态和外部 effect；每例记录输入、调用顺序、最终状态及 permission settings。调用前执行源码调用方同样的 JavaScript `raw.trim()`。覆盖大小写、空白与参数拆分、alias、mode、permission 子命令及异常输入等。原始 JSON 自带逐例输入及完整结果。

- 源：`resources/PilotDeck/src/adapters/channel/tui/app/TuiApp.tsx`，commit `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，文件 SHA256 `0134f0d8865eac294864ec41fd229fa3cf0d8032125a90437cc2c8ada556470b`。
- 截取范围：原文件第 661 行 `async function handleCommand(` 起，至第 823 行末、`function buildPermissionEntry(` 前；截取 SHA256 `bae4518e074a9d51ab60a0753a3ac32e463205d7682b5844abb49e43a22596bf`，转译 SHA256 `99080844e12f7887bc56a584016af52af062ed8af81b91ae26e9aa4cb0e8f138`。实际边界和运行环境也写入 JSON。
- Node v22.22.0、ICU 78.2；生成器 SHA256 `2a10ea2dafb908a7225b1b8e2205636e7735e6b495369316dbf7b4db4c1880a7`，`raw-mbp2-01/commands.json` SHA256 `fd900e8913b8a3bfbbe0242ccef35feaf82eb0afa11d0060693259bd9c6ac76a`；已与远端逐文件校验一致。

复采在固定环境下运行 `node capture.mjs <PilotDeck源码根> <不存在的输出目录>`；脚本检查 commit、源文件 hash、Node/ICU 和唯一截取边界，独占创建输出目录。依赖来自该源码树的 TypeScript。示例实际运行使用 mbp2 的 `/Users/da/opt/node-v22.22.0-icu78-darwin-arm64/bin/node` 和 `/Users/da/ws/pdrs-runner/resources/PilotDeck`。

边界：只执行真实私有处理器及 JS 内建 trim，React 更新、gateway、权限读写和退出为可观察的内存假对象；因此这是命令分派与指定状态下副作用的 oracle，不是 React/网络/权限文件端到端验证，也尚未接入 Rust 回归测试。输出中的假 session/project 和 permission 值不代表生产数据。
