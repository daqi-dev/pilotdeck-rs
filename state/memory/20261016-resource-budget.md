---
name: resource-budget
description: 清可再生cache前按最后硬链接的allocated块估算，局部du不代表全局可回收量。
type: feedback
---

为什么：R36/R38多次出现局部incremental du与完整du释放不等，R44再次实际确认1273个已删cache inode仍在保留noncache路径有链接，清两处后余0.975994GiB仍不够1.20窗；所有非cache证据SHA保留。不能为追预算删保留deps/bin或突破现行上限（正本：CLAUDE.md「资源上限」，2026-10-18 起 100 GiB；冻结装置 calibrate.py 的 CAP_BYTES 仍 80 GiB，另走冻结程序）。原件在主仓_tmp/20261016-debt-cleanup/order-architecture/acquisition-run-144502/。

怎么用：先证明精确cache的持有人/可再生性、无锁/进程/保留要求与sealed内容引用，保存完整非cache SHA。按dev/inode去重、核nlink；只有候选内路径数覆盖该inode全部链接才计可释放，外链一律计零并定位其保留路径。估算用st_blocks的allocated字节，不用逻辑size或各目录du相加。独立临时新target及日志/原件合并预算，留并行文本余量；清后全量du复核满足实际窗口才启动，不够只停相关构建、转独立工作。删除必须新精确决策卡，不把R36/R38/R44/R45当永久清理授权；非cache前后路径/字节SHA断言始终必做。
