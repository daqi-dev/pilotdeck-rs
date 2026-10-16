---
name: observation-units
description: 测量计数必须与实际字节对齐；小缓冲、空队列项和短输入用真实ReadBuf证实，不拿当前fixture大小充通用不变量。
type: feedback
---

为什么：R64对旧Scripted精确源码实测，同一9字节chunk被3字节ReadBuf读取时pop计数3，实际已读9；空队列项产生假EOF，短later0..4还会切片panic。现有大fixture通过不能证明测量helper在小缓冲下正确。原件 `upgrade-read-boundary-r64-4ree12_9/`；修复R65/适用CI与两家scope门逐HEAD保留。

怎么用：AsyncRead观测ReadBuf新填入差量，区分prefilled与本次实际读取；空项跳过，零capacity不消费真实输入，短输入分割先限长度且不排空chunk。同步审计全部计数消费者的量纲，永久控制涵盖queued/delegate/prefilled/empty/zero/short。依赖当前Hyper缓冲的精确计数断言必须在各处写具体fixture假设，未来换策略重审而不松断言；完整EOF/全流重复拒绝仍独立保留。
