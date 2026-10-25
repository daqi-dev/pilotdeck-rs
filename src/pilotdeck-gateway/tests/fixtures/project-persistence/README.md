# Project persistence 金样

固定基线 OpenBMB/PilotDeck cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85，Node v22.22.0；probe.mjs 直接 import 基线，无 Rust 输出输入。运行（scratch 必须为新的隔离目录）：

```sh
node --import /absolute/PilotDeck/node_modules/tsx/dist/loader.mjs probe.mjs /absolute/PilotDeck /isolated/new-scratch > oracle.json
```

事件 envelope 的时钟和 UUID 固定；错误路径仅替换 scratch 前缀为 `<ROOT>`。Snapshot 比较身份 hash、状态、错误和二进制 payload；不抹除 manifest 时间后冒充原字节相等。Rust 对实际磁盘字节重启前后严格比较，TS envelope 回放另按完整 JSONL 字节比较。普通事件不是 fsync 承诺。超限拒写与原生 IO 错误表示见 A6.9-PERSISTENCE-LARGE-001 / IO-001（未批准）。同路径只能有一个活跃 writer；本组多会话证明不同路径隔离。
