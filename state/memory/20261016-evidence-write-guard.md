---
name: evidence-write-guard
description: 复用审计函数须在执行前限制真实写路径；字符串替换成功不等于证据目录隔离。
type: feedback
---

2026-10-16，提取旧run_audit时遗留硬编码输出目录，误覆盖已封57项中的real-input-audit.json。停止后保留误写新物，以同轮报告结构、原candidate身份和有界微秒重构命中既有封存SHA，恢复36178字节；不是从原件副本复制，也不是重新生成新验收。root与独立备料者分别核旧20和57共77项，SHA全匹配。事故、恢复来源及方法见主仓`_tmp/20261016-debt-cleanup/taxonomy/consumer-contract-proof/recovery.json`；旧索引不改。

后续实验只写本次明确持有的proof目录。源文本改写先断言每个锚点的唯一匹配次数；执行旧函数前再装实际写路径白名单，对write_text/write_bytes/replace等当前写入口越界即拒，不能只依赖改写后的字符串。负控先对自有模拟旧路径验证“开文件前拒绝”，不拿真实原件试写；执行前后逐项核封存索引。旧已封验证器只读归档，新的可重跑入口必须指向受guard版本；恢复无法命中旧SHA时保留缺口，不生成新时间结果冒充旧原件。
