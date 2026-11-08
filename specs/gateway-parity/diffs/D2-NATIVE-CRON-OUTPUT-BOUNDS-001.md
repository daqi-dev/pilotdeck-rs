---
id: D2-NATIVE-CRON-OUTPUT-BOUNDS-001
---

## 类别

原生宿主安全/资源修复：限制单次Cron累计助手文本；持久事件和真实turn继续drain。另给原生结果投递任务30秒期限。

## 基线依据

固定cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85的CronFire.ts逐事件assistantText += event.text，没有显式文本上限；pilotdeck.ts的onCronResultDelivery detach真实ChannelServer.deliverCronResult，没有本片的30秒期限。真实23格事件/3格UTF16 scheduler oracle及Node版本/源码hash见tests/fixtures/cron-events；它们证明正常行为，不声称TS具备4M或30s限制。

## Rust 行为

旧库CronFireDeps.assistant_text_limit_units默认None。仅native assembly通过manager callbacks启用4*1024*1024个UTF16 code units；等于限制可成功，超出时failed/cron_output_limit、abort一次system:output_limit，保留先前累计但不交付截断内容，继续持久化/forward/drain后续事件后写失败终态并清理任务。该安全失败在后续error事件前保持。原生结果投递detach后30秒timeout，失败记录告警，不重放投递。

## 范围

只限制native Cron助手累计/原生投递等待；不是全部进程内存/磁盘预算，不修改旧库默认，也不修共享TurnSubmission的unbounded队列。现有Cron migration/discovery/store的阻塞文件系统边界另记，不以本上限冒称已修。没有更改既有冻结判分阈值。

## 机器判据

fire_event_tests覆盖UTF16精确边界/超限、一次abort和后续事件仍消费，以及真实4M+1输入。23格默认None逐字段及所有after文件正文重放TS；observed_drain_tests覆盖首error事件后完整drain、持久观察失败后abort/完整终止且保首因；cron_delivery_tests验证真实ChannelServer选择运行adapter/空origin/关闭负控。候选冻结时Rust尚未由root执行，不把测试存在当通过；30秒投递的超时路径仍须后续真实宿主fake adapter验证。

## 批准记录

2026-11-08 root明确同意4M UTF16 native opt-in安全上限，要求超限abort并drain；用户完整CLI开发/安全bug修复授权内执行。30秒原生投递期限为可撤回资源选择，AI代批待人复核。未批准、非已接受冻结豁免；实际验证以root mbp2原始记录为准。
