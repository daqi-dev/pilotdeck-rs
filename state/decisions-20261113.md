# 2026-11-13 接续决策

## #3076 HTTP完整正文与JSON类型合同修复

自决 · 待复核（AI代批，待人复核）；本卡修复已有明确判据，不新增比较豁免。原认证1212份A2 TS原件正常完整比较PASS，将单侧SSE正文JSON字符串化并同步原chunks后仍最终PASS，独立#3076按尺子P0处理。后续同源运行实际证实health和files/write的等长true→1也都完整validate+compare假PASS；Python布尔/数字等值同时漏过health oracle和G5。原失败/假PASS继续保留。

影响是Gateway A2及standard HTTP产品侧比较装置，可用Git撤回，不改意向或原件。不采用删流、跳验证、放开头字段、取消UUID映射或canonical字符串全域比较。共享正文投影先识别SSE/NDJSON，保留全文结构/顺序；JSON与文本包装为互斥表示，原值先按既有ID/元数据规则处理。normalization.md:11明确任意字符串的已锚UUID映射，:38不取消它；未执行过的私有“text不映射”候选已撤回保全。原headers/CL/请求字节/状态/原状态行不新增豁免。

有限bool修复复用既有native_evidence.same_json，仅health原oracle与G5.http使用，保留数值1/1.0等值；frames/disk、业务oracle其它部分和native_evidence实现不变。#2961已批准的项目摘要createdAt差量独立保留，不以本修复扩大字段。#3062仍只key/匹配Accept/4mask人批范围。

macmini独占r2报告32单位及真实完整正反控通过，源0641/97064/6900与root收货逐SHA相同；140份原件已逐SHA回存并独立核32单位footer和完整G3/G5正反控；两家非作者终审和合法冻结登记仍为交付前提，当前未称全部完成。前置于本Gateway合并/A2重新定标/A8退役；证据_tmp/20261112-wave1-validation/frozen-http-body-validation/。可复用原则：先认证原字节与原输入，再按既有合同建立互斥表示；语言的隐式相等不能抹去JSON类型。机检为真实SSE/health/write完整反控与test_retirement_standard_http定向用例。
