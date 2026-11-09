# #1356 冷切换与回滚产品诊断

本目录只负责未冻结的产品侧真实进程诊断。#1356 OPEN，当前无功能 PR/评论/在途实现持有人；#1355 的 claude/issue-1355-r1 fix-r25 继续持有冻结 retirement 装置，本包不修改 session_domain、retirement_driver、评分、账本或 FROZEN。结果交该车道承接，不把诊断 PASS 改称 E1 coldRestartRollback PASS。

复用 native-business-loop/probe.py 中已运行的进程、二进制快照、mock provider、pid/birth、隔离环境与 common.parity_lock；固定共享源 SHA c07c62a1816692709f57e6bb1a860597f092f1282fada935f93283f268af2a7d，漂移即拒跑。固定 TS cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85；真实官方 GatewayWsClient/RemoteGateway 驱动两侧，没有自己实现 TS 预期。

## 实际拓扑与数据

五个顺序阶段，每段新写者启动前都必须有前一段的成功退出、监听关闭与全部已观察身份消失凭据：
1. TS server 后端 + r15 Gateway 的真实 proxy 模式：公开 restore、cron_create、upload 五法种数据。
2. r15 native Gateway + Rust sidecar：读取 TS 数据，写新 portable、真实模型回合/JSONL、Cron 和 upload，关闭会话后排空停机。
3. TS server + 真实 Rust proxy：仅调用读 API，完整比对新旧 portable/历史/Cron/upload，不恢复任何旧快照。
4. 再启 native Rust：完整读回两代数据，写入 RUST_AFTER_ROLLBACK 新会话。
5. 再次 native 冷重启：读回最终新数据，durable 文件字节不变。

Cron 启用以允许公开 create；所有任务定在2035/2036年，演练期间不触发。所有 provider/mock/key/文件均为私有 fixture 数据；一次真实模型请求，maxOutputTokens256；没有真实账号或外部 provider。home/project 不交叠，端口28610–28689，proxy 后端和前端独立；用户18789/3001与真实 home 不使用。

## 回滚副作用如实保留

TS 回滚启动会执行其已有 Cron 迁移：新增 home/cron/store-migration-v1.json，重写 tasks.json 的序列化键顺序/末尾换行。readback-effects.json 明记 notByteReadOnly=true、formalDifferenceApprovalClaimed=false；不称 TS 全程只读。比较要求原路径均存在，Cron 完整 JSON 对象严格相等，仅允许 tasks.json 的纯序列化变化和该已观察到的版本1迁移标记；其他新增/删除/内容变化立即阻断。会话 JSONL、上传 metadata 与文件逐字节保持；逐阶段 durable-files 保存原始字节作审计，绝不作为还原输入。

## 真实排空失败格

--force-real-drain-failure 复用 #1353 的 native_process::switch_wiring_tests::an_expired_drain_halts_and_the_death_march_closes_the_survivor：真实 Rust 新数据写完后，建立实际 RFC6455 upgrade 的 raw WS survivor 并保持不退出，再向实际 native Gateway 发 SIGTERM。要求真实非零退出和 drain deadline stderr、端口关闭，捕获 close1001/server switching 与 EOF；handoff receipt 留为 false，下一 TS 写者不启动，已生成新数据前后快照相等。该负向场景脚本退出2，result.pass=false，realDrainFailureObserved=true；不能把“失败被正确阻断”包装为 Gateway 排空成功。

--inject-missing-drain-receipt 是另一独立的控制器丢证书检查，退出2，明确不冒充真实 runtime 排空失败。

## 运行与证据

使用同一源码绑定候选的 gateway/sidecar 绝对路径和 manifest.json：

python3 -I src/pilotdeck-gateway/tests/fixtures/native-cold-switch/probe.py --baseline /Users/da/ws/pdrs-runner/resources/PilotDeck --gateway /absolute/candidate/pilotdeck-gateway --sidecar /absolute/candidate/pilotdeck-agent-loop-sidecar --source-receipt /absolute/candidate/manifest.json --provenance-label source-bound-diagnostic --out /absolute/fresh/output

真实排空失败另加 --force-real-drain-failure，必须新 out。输出目录独占；共享 parity lock 获取1秒有界拒跑；总240秒、阶段 driver45秒/进程50秒、停机20秒，各网络/系统调用有界。每侧原始 wire/stdout/stderr、启动/drain 身份与端口、完整数据副本/哈希、provider 请求、执行源字节及二进制哈希均保留。清理无法证实则保留 cleanupPending；没有按进程名批量杀进程。

r15 实跑：_tmp/20261110-native-cold-switch-r6 正向五阶段 exit0/pass=true，10.85秒/1次模型请求；r5-real-drain-fail 真实 Gateway exit1、控制器期望 exit2，下一写者未启动、新数据保持；r4-blocked 是独立缺证书负控。三者 cleanupVerified/binariesUnchanged=true。r1/r2 失败及源码均保留：Cron 禁用时 create 被 TS 正确拒绝；随后过严的全字节只读断言揭示 TS Cron 迁移副作用，没有改产品迎合测试。

receipt SHA551fb5b7bf93c12cfed1bd27faa431855bb1fce740dfe48f08ed830ce3748e56 绑定 r15 的9b9941b9之后精确源码快照，不是新正式验收提交。Node22.22.0/ICU77.1 为诊断环境；未执行冻结 E1、Linux pidfd/strace、真实 provider、人签字或故障文件系统断电持久性证明。真实失败覆盖 S2 WS drain deadline，不声称注入了磁盘 fsync 失败。
