---
id: D2-NATIVE-HISTORY-BOUNDS-001
---

## 类别与范围

新接通的 native `read_session_messages` 有限资源拒绝与后台文件隔离。旧 CLI 语法/输出仍由原入口处理；常规历史消息、分页、字段与JS类型语义按固定TS实采对齐。现有 `read_transcript` 和其他 NativeFiles 消费者的行为不改。

## 基线与差异

固定TS `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的 readWebSessionMessages / JsonlSessionPersistence 使用实际文件读取；真实99格来源及运行记录见 `src/pilotdeck-gateway/tests/fixtures/native-history/README.md`。正常、空、相对背景路径以及归一化后 `.jsonl` 后缀按真实基线保留；不把源码阅读推导的风险冒称已完成TS超限/FIFO实采。

Rust 新读器在 Unix 对实际打开的同一 fd 使用 O_NONBLOCK、普通文件校验和最多50MiB+1读取；超过50MiB拒绝。用于确定当前项目目录的 `.cwd` 同样限为64KiB，并且资源拒绝不会被原Metadata的IO容错吞成另一个目录。后台相对路径先规范化，拒绝绝对/越界/非.jsonl路径，实际打开使用现有beneath/NOFOLLOW机制拒绝symlink路径。普通会话仍沿用已有项目目录选择和普通会话symlink语义，不声称两条路径都改成NOFOLLOW。

每进程独立4个history请求许可覆盖请求、最终编码和发送；blocking编码持有共享owned许可，取消外层future不能提前释放。另4个读取许可覆盖读文件、目录信息和投影，以覆盖非WS直接调用。完整响应含id和active stream按既有100MiB WS上限核字节；超限返回 `gateway_request_failed` / `History output exceeds Gateway frame limit`。异常长id连错误帧也装不下时仍由原transport限制处理。不会以输出条数或提前分页偷改旧总数/usage语义。

这些限额是新拒绝行为，不是TS已有承诺。只有Unix实际验证；非Unix bounded读取明确不支持。它们约束文件字节/单帧与并发数，不保证总RSS固定、全部算法线性、目录总扫描量有界、故障文件系统内核IO可中断或跨写者事务；项目配置其他读器不在本片修复范围。

## 验证与批准

97格完整投影结果/异常与2个读前路径拒绝分别检查；真实marker FIFO/超限、普通转录FIFO/超限、后台路径与symlink、较小输入膨胀成超大完整封套、SDK/CLI/TUI恢复、活动轮次合并和重启读取由本片测试/隔离探针验证。只把实际完成的运行记为通过，结果与源码指纹见主仓 `_tmp/20261108-cli-consolidation/README.md`。

**AI代批，待人复核；未批准，非已接受差异，无冻结判分豁免。** 此为可撤回的原生读取接线及防挂起修复，不改accepted-diffs原件、阈值或阶段完成判据。只做本机提交，Rust全部mbp2；真实provider/人眼体验与阶段签字另验。决策见 `state/decisions-20261108-control.md`。
