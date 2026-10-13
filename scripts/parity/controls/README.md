# G03 v1 离线对照

`python3 scripts/parity/controls/run_all.py` 不启动产品、不拿运行锁；只读 exp001 的真实 trace，在本 worktree `_tmp` 的临时目录复制后注入差异，退出时清理自己创建的副本。可用 `--source` 指定另一份完整 53 场景的 TS 数据。详细 JSON 写到 `_tmp/controls.json`。

逐条规则的 witness 由 `run_all.py:rule_pairs` 生成：每条先断言合法 trace 自比 PASS，再断言注入后 FAIL 且 `triggered_rules` 包含指定规则。多个规则同时命中是允许的；**不以“任意 FAIL”代替指定规则命中**。E10 是流内顺序/值严格比较，E11 是每种事件身份的数量一致，旧源码以隐含方式执行这两条，此处赋予显式名称。E1v 是旧源码的 context-budget 可见形式检查，也提供对照。

真实 trace 对照覆盖：同份全轮自比、模型请求体改值、缺一个场景、完整 JSONL 前缀截断、JSON 行中间截断；两侧空 trace、空目录、同一个错误。两侧同错使用本应完成的 `pure_text`，由基线场景 oracle 判坏；预期就失败的模型错误场景仍按基线 oracle 接受。

另有跨通道重排的正向 witness，证明 G03 的放宽仍然存在。actor/channel 迁移负向 witness 证明拆流不会吞掉事件。对照不能证明所有可能的坏 trace 都能检出；独立审查仍是第四道闸门，结果由协调者填写。
