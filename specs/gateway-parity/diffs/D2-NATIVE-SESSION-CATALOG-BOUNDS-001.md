---
id: D2-NATIVE-SESSION-CATALOG-BOUNDS-001
---

## 类别

原生会话列表与原生记录器对本地持久化资源的有限读取和拒绝；这是 Rust 侧新增的可撤回边界。

## 基线依据

固定 TS 的会话目录和 JSONL 读写合同见 `src/pilotdeck-gateway/tests/fixtures/native-history/README.md`，普通文件内容及列表语义继续沿用既有投影。这里没有声称 TS 对 FIFO、并发文件增长或超限目录的拒绝与 Rust 相同，也不将源码风险推断冒称 TS 实测。

## Rust 行为

原生 `list_sessions` / `manager_sessions` 的 catalog，每次调用最多扫描 16,384 个目录项、实际累计读取 128 MiB；超限返回错误而非将不完整列表作为成功。列出的 transcript 使用实际打开的同一普通文件描述符及非阻塞打开。原生 session factory 打开写锁时拒绝非普通文件及锁文件软链；恢复 transcript 以同 fd 校验普通文件，并在读取前和读取期间执行 50 MiB 上限。原生写入 fence 与 factory 使用同一 bounded marker 路径，已加载 recorder 与请求解析出的路径不一致时拒绝写入。相应 blocking 工作受现有 native session IO 许可约束，并由任务持有直到完成。

## 范围

本条只覆盖上述 native 公共路径，不改变非 native 原调用合同；Unix 文件描述符实现之外明确不支持。目录与字节预算不是全部进程 RSS 上限，也不能保证故障文件系统的内核 IO 可中断或对外部目录并发替换提供完整事务隔离。既有旧 transcript 读取调用与 frozen accepted-diffs 均不改。

## 机器判据

FIFO、软链、50 MiB 超限以及正常文件由独立有界子进程验证；catalog 正常、多条、超限和拒绝以真实文件验证。相对 marker 的真实 native host 与 sidecar 对照包括冷写、真实 turn、路径变更拒绝及恢复；具体完成情况以 mbp2 测试日志、不可变二进制与 `_tmp/20261108-session-fence-identity-01/` 实测记录为准。候选在生成本文时尚未编译和完成新宿主正控，不能写成已通过。

## 批准记录

**AI 代批，待人复核；未批准、非已接受差异，无冻结判分豁免。** 不修改冻结的 `accepted-diffs.md`，不据此标记阶段验收完成；最终采用及决策记录由主控核对。
