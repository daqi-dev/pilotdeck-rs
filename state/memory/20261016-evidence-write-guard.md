---
name: evidence-write-guard
description: 复用审计函数须在执行前限制真实写路径；字符串替换成功不等于证据目录隔离。
type: feedback
---

2026-10-16，提取旧run_audit时遗留硬编码输出目录，误覆盖已封57项中的real-input-audit.json。停止后保留误写新物，以同轮报告结构、原candidate身份和有界微秒重构命中既有封存SHA，恢复36178字节；不是从原件副本复制，也不是重新生成新验收。root与独立备料者分别核旧20和57共77项，SHA全匹配。事故、恢复来源及方法见主仓`_tmp/20261016-debt-cleanup/taxonomy/consumer-contract-proof/recovery.json`；旧索引不改。

后续实验只写本次明确持有的proof目录。源文本改写先断言每个锚点的唯一匹配次数；执行旧函数前再装实际写路径白名单，对write_text/write_bytes/replace等当前写入口越界即拒，不能只依赖改写后的字符串。负控先对自有模拟旧路径验证“开文件前拒绝”，不拿真实原件试写；执行前后逐项核封存索引。旧已封验证器只读归档，新的可重跑入口必须指向受guard版本；恢复无法命中旧SHA时保留缺口，不生成新时间结果冒充旧原件。

**为什么（2026-10-25 追加）**：#589 修复轮的命令日志三次因 tee/重跑同一路径覆写失败原文（同类第三次，按 AGENTS「同类错误第三次必须沉淀成规则」触发）——主仓 `_tmp/20261025-issue-589-live-integrity/fix-r2-log-overwrite-record.md` 记录 01 覆盖；本树 `fix-r2/log-overwrite-records.md` 自报 02 与 06 两处。覆写后中间失败原文不可恢复，只剩二手描述，红探针等证据的可复核性受损。

**怎么用**：命令日志一律独占创建——经 `open(..., "x")` 语义的捕获器（该轮为主仓 `_tmp/20261025-issue-589-live-integrity/capture-exclusive.py <新log绝对路径> <超时秒> <命令及args>`），写入前先确认目标路径不存在；重跑一律换新文件名（attempt-2 等），禁止 tee 到已有路径、禁止复用旧日志名。覆写已发生时保留现状并自报缺口，不以重跑输出冒充原件。
