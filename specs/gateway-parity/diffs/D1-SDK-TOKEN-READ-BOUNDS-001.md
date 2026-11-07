---
id: D1-SDK-TOKEN-READ-BOUNDS-001
---

## 类别

避免SDK token文件读取挂起/无界分配的安全修复，仅Unix文件来源；普通IO/目录/空值原文和已实现SDK命令/wire保持原路径。

## 基线依据

固定`cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`的`src/cli/sdkCli.ts:180-191`在argv/env均未提供时，用异步readFile读取整个server-token并trim，所有IO错误catch为空。真实完整pilotdeck.ts sessions list在隔离HOME/PILOT_HOME/cwd、动态loopback fake WS实采19格：65537字节确实进入hello并完成list；无writer FIFO 2.5秒仍无连接，父进程精确杀收；立即writer和延迟3秒writer均能发送token并完成。延迟正控先无连接再写入成功，不能把慢启动猜成读取挂起。

源sdkCli.ts SHA256 `d41c08ce59380f85bb81ec8bd2d9bc876c778f9b2ebaea3fd25b1b5f08e288ec`，Node v22.22.0/ICU78.2、实际SDK dist hashes及完整原件由`src/pilotdeck-cli/tests/fixtures/sdk-token/`的自写生成器/新oracle记录，原始采集在主仓任务目录。token以假数据构造，日志记录长度/hash而非复制实际值；不连接外部Gateway/用户服务。

## Rust 行为

`src/pilotdeck-cli/src/sdk/token.rs`只替代SDK文件回退分支。Unix以O_NONBLOCK打开、同fd判断类型、元数据超65536提前拒绝；实际read使用take(65536+1)捕获增长。FIFO/其他已打开特殊文件拒绝为`Gateway auth token file must be a regular file.`；超限为`Gateway auth token file exceeds the 65536-byte limit.`，退出1且不连接。没有原路径预检后再无界重开。

普通文件仍UTF8 lossy +原JS trim；缺失/EACCES/普通读错/目录/空文件继续旧token-required原文，不把目录改成新错误。argv和env分支不碰文件：空flag由原readValue视为缺省并回退，空env保留且不回退，argv/env空白不trim。非Unix保留原文件读取，不新增未经验证的Windows保证。

## 范围

仅SDK run/resume/sessions/settings读取token文件的Unix分支。argv/env token无新增长度限制，URL/SDK transport及成功序列化不改；cron/default remote/TUI/serve不在此范围。文件字节限额不等于字符数；只保证特殊文件打开和实际读取字节有界，不承诺中断故障文件系统内核IO、跨写者快照或其他平台同等安全。没有修改冻结装置/旧D1金样。

## 机器判据

先有19格真实TS整CLI原件，再以真实Rust CLI对照完整输出/rc、正常hello/请求字段与token精确摘要，记录两侧原hello长度/hash。四个safety行仍保留TS接受/挂起事实，Rust按本差异核静态拒绝完整文案和零连接。Rust保持已有sessions JSON serializer，不新增帧key字节顺序判据。

四个真实文件/fd单测覆盖原codec/普通IO、65536/65537、fstat后实际增长、原路径替换FIFO但仍读原fd。集成15个run/resume/sessions/settings get/set资源组合核期限、零连接、输入不变和active pipe未被消费。TS延迟writer的3秒正控不等于Rust时序复放；Rust预写active FIFO只验证拒绝/不消费。stdin=null与有prompt的测试不声称开放stdin顺序。编译/检查/旧SDK回归全部mbp2，原始日志和源码hash据实归档，未运行不报通过。

## 批准记录

**AI代批，待人复核**，按现有默认修复挂起/安全问题及本轮SDK文件防护任务授权执行。**未批准，非已接受差异**，无冻结评分豁免，不改阈值、分母、D1完成历史或A8前置。决策见`state/decisions-20261108-control.md`。仅本机提交，不推送GitHub/开PR；真实服务与真人实效另验。
