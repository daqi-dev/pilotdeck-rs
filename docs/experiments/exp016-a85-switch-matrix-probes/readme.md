---
type: experiment
number: "016"
date: 2026-11-09
title: A8.5 切换状态机矩阵 · 探针装置进仓与补格（#2251/#2255/#2258）
status: 装置进仓（P1–P7 移植参数化）+ P8–P16 补格完成（#2251/#2255/#2258，批次 p2batch-d6 收口）
related: [research/012_phase-a-switch-matrix, experiments/exp015-a52b-session-fixtures]
---

# exp016：A8.5 切换状态机矩阵探针装置（P1–P16）

研究对象是 `docs/research/012_phase-a-switch-matrix/matrix.md` 的逐格断言。原始 P1–P7 探针只在
#1353 轮次的 `_tmp/` 里（不进提交），矩阵 A1–A14 断言表是主实现轮照抄成测试的输入——没有可复跑
装置就无法复算。本实验把装置进仓（`scripts/a8-5/`），并补齐 #2251/#2255/#2258 点名的缺口格。

## 假设

1. 装置与结论可分离：P1–P7 移植（路径参数化、行为逐字保留）后可在另一台主机（mbp2/Darwin）复现
   cb8（Linux）的关键结论（p1_smoke 全过、p2b 三态 settle 行为一致）。
2. #2251 四项遗留 + 矩阵内 #2251 标注的 S1–S3 接纳/零字节格可按同一装置实测，不需要改 TS 基线。

## 做法

- **被测**：只读 TS 基线 PilotDeck @ `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`（dist 构建，dist
  mtime 晚于 src 最后修改）。原始轮次在 cb8（Linux，路径 `/home/clawbox-08/ws/pdrs-runner/...`）；
  进仓后在 mbp2（Darwin）以 `/Users/da/ws/pdrs-runner/resources/PilotDeck` 复测。Node v22.22.0 /
  ICU 78.2。
- **落位**：`scripts/a8-5/`（common.mjs + 各探针；`--base`/`--out` 参数化，默认基线取
  `PILOTDECK_BASE` 或标准 runner 路径）。`node scripts/a8-5/<probe>.mjs --out <证据目录>`。
- **纪律**：仅 loopback 高位端口（39871–39898 段）、隔离 `PILOT_HOME`/`diskSpillDir`、不外发网络
  （provider 指不可路由 loopback）、子进程记 pid 逐个收尸、观察窗全部有界、无无界派生。
- **证据**：P1–P7 原始输出（cb8）归档于主仓
  `_tmp/20261107-archive-wt-1353/20261026-issue-1353/raw/`；P8–P16 原始输出（mbp2）在
  `_tmp/20261109-p2batch-d6/raw/`（提交后归协调者归档，路径以 RESULT 登记为准）。

| 探针 | 覆盖格 | 一句话结论 |
|---|---|---|
| p1_smoke | 装置自检 | mbp2 复验通过（health/hello/describe/close 全过） |
| p1_p2_stages | E1/E2 × S0–S4 + t3 | 见 matrix §4.1/§4.2（cb8 原始数值） |
| p2b_block | close() 阻塞规则 | mbp2 复验：无连接 settle / 一条 WS 2000ms 不 settle、断开后 1ms / 空闲 keep-alive settle——与 cb8 一致（#2756：空闲 keep-alive 格以首次 `/health` 入池为前提，前提未成立记 `C/SKIP-idle-keepalive`；fix-r1 起还须 timedClose 前池内仍占用，池被服务端空闲超时提前收掉记 `C/SKIP-pool-drained-before-reuse`——两种降级下结论行均记 `skipped:true`，不作 A14② 对照；fix-r2 起降级行 `premise`（初始入池前提原值）与 `premiseAtClose`（close 前池状态）分立、`settledAfterHoldMs` 由 timedClose 显式置 null，不依赖展开顺序） |
| p2c_post_dispose | S3/S4 既有 WS | 见 matrix §4.1（cb8） |
| p3_child/p3_driver | t4 drain 注入 | 见 matrix §5.2（cb8） |
| p4_cron | E3 × 全阶段 | 见 matrix §4.3（cb8） |
| p5_background | E5 × 全阶段 | 见 matrix §4.5（cb8） |
| p6_real_server | t5 端到端 | 见 matrix §5.1/§5.3（cb8）（#2756 fix-r2：`keepalive-http` 在途行事后复核 `reusedSocket`——SIGTERM 前池占用复检通过但请求落新连接（复用池在发出前被收尾）时降级记 `ka/SKIP-reuse-fell-to-new-conn`、该行以 `skipped` 出现） |
| p7_channels | E4 × S2 + 竞态 | 见 matrix §4.4（cb8） |
| p8_channel_stop_race | #2251 条目 2 | close 只等快照 handle：在飞 hot-reload stop 不被 join（close 1ms settle）、失败 stop 回填 map（deliverCronResult 复投 true）、二次 close 复用 rejected promise 不重停 |
| p9_background_descendants | #2251 条目 3 | 同组 TERM 免疫后代零残留（组 SIGKILL 覆盖）；setsid 跨组后代逃逸存活（ppid→1、自有 pgid；任务 pid 锚定的 ppid 链漏检） |
| p10_background_hangs | #2251 条目 4 | start 永不 settle ⇒ dispose 卡 whenStartsDrained（3s 窗右删失）；快照写挂起 ⇒ TERM 已杀、dispose 卡 `await done`（settleEntry 停 store.write） |
| p11_child/p11_driver | §4.5 S1–S3 接纳 + 连接两态 | closing 窗口内 E5 新任务被接纳（running）；断开 ⇒ dispose 杀（码 0 零残留）；保持 ⇒ close 永不 settle，SIGKILL 后 detached 任务成孤儿 |
| p12_zero_byte_close | A4 零字节 ≥65s | GatewayServer 实例级：零字节连接使 close() 70s 窗不 settle（>60s headersTimeout，右删失下界 70s）、服务端不主动断该连接 |
| p13_empty_set | #2255 空集合格 | 组件级三格 + 全空五步组合级：全部 0–4ms 落定（空集合不挂起、退出路径可达） |
| p14_child/p14_half_stop | #2255 半停格 | 真 cron 先真停不回滚 + alwaysOn stop 失败：单失败原样抛、finally 照走 unpublish、二次 stop no-op；server 级半停窗口 E1/E2 可服务、后续四步照走、退出码 0 |
| p15_child/p15_driver | #2255 单飞 + 退出码抬升 | 重复信号（SIGTERM×2+SIGINT）下 automation.stop 恰 1 次（两层单飞缓存）；挂起窗口内 late unhandledRejection ⇒ exit 1（对照 0） |
| p16_stop_acceptance | #2258 停接纳机制观测面 | 机制 A（关 listener，真实 server）：新连接三层 ECONNREFUSED、空闲 keep-alive 服务端 FIN、裸层复用已 FIN 连接 = 写成功后静默悬挂（同 keep-alive agent close 前池化、close 后复用直测 ECONNREFUSED connect 相位——重建连直测）；机制 B-RST/B-EOF（纯 node 受控）：握手一律完成后 RST（ECONNRESET）或 EOF（node:http 层两者同为 ECONNRESET 码，fetch 层可区分 ECONNRESET / UND_ERR_SOCKET）。前提守门（#2662）：两处健康检查响应须为 keep-alive（缺头按完整 HTTP/1.1 响应头隐式）、窗内须实际观测到干净 FIN（`end`，非 error→close 异常拆链），否则对应观察降级——agent-reuse 前提失败落 `A/SKIP-agent-reuse`、close 前池被收空落 `A/SKIP-pool-drained-before-reuse`（fix-r1 池占用复检），裸层/FIN 前提失败以 `rawPremise`/`finObserved` 字段与降级 note 体现（不发独立 SKIP 行），不出上述结论（#2756：keepAliveToken/入池前提判定收敛进 common.mjs——`keepAliveProbe.once` 返回 `premise`/`premiseReason`，本探针内联 agent 炮台改用共用 keepAliveProbe，p2b/p1_p2/p6 三探针同形；fix-r1：裸层 `rawPremise` 亦改共用 `keepAlivePremise`、once 另带 `reusedSocket`、agent 暴露 `freeSocketCount()` 供 close 前复检；fix-r2：`freeSocketCount` 改各 origin 数组长度求和、`A/agent-pooled-request1` note 收窄为「请求 1 已入池，close 前是否仍占用见 A/SKIP-pool-drained-before-reuse」——close 后 `reusedSocket` 两路径同为 false 仅作参考，判别依据是 close 前的池占用复检） |

## 结论

逐格结论登记进 matrix.md（§4.4 新两行、§4.5 新两行 + S1–S3 接纳行改实测、§4.2 零字节注、A4/A10/A11
的「未实测」标注解除、§4.6 空集合/半停四行、§二 单飞段、A2/A13 断言补登）。要点：

1. **装置可移植**：p2b 在 mbp2 逐态复现 cb8 行为——`server.close()` 阻塞规则不是主机特异。
2. **closeServer 的 join 面只有快照**（p8）：被 claim 的 handle 的在飞 stop 不进 `Promise.allSettled`，
   close 先 settle、flush/dispose 照走；失败 stop 在 `closing=true` 后回填 map 且永不再被停。
3. **杀灭口径必须覆盖后代**（p9）：基线组杀覆盖同组后代；跨组逃逸（setsid）残留是杀灭断言的边界，
   「零残留」检测必须用快照差集/全量 cmdline（ppid 链在 reparent 后漏检）。
4. **「SIGKILL 后 dispose 必然收敛」不成立**（p10）：start 未落定或快照写挂起时 dispose 无界挂起
   （无二次超时）；是否加界归 #1353 决策卡。
5. **E5 在 S1–S3 照常接纳 + 孤儿路径**（p11）：接纳面只看运行时自身状态；提交连接保持打开 ⇒
   S4 不可达 ⇒ SIGKILL 旧进程后 detached 任务成孤儿。
6. **零字节格**（p12）：GatewayServer 实例级右删失下界 70s，>headersTimeout(60s)。
7. **空集合不挂起**（p13）：零任务/零 channel/零连接下全部停机面 0–4ms 落定——「空集合快速干净退出」
   断言有基线支撑（A13 同构）。
8. **半停无重试**（p14）：部分组件停成后失败——已停不回滚、失败组件永不再被停（二次 stop no-op）、
   控制面照常摘除；server 级半停窗口 E1/E2 照常可服务、后续步骤照走、退出码 0。
9. **单飞 + 退出码抬升**（p15）：重复信号下停机只进一次；退出码可被停机挂起窗口内迟到的
   unhandledRejection 抢写为 1（对照 0）——退出码不只由停切换过程产生。
10. **停接纳机制观测面**（p16）：机制 A 在 connect 相位明确拒绝并立即释放端口；机制 B 握手一律完成、
    失败推迟到读写相位且端口不释放；A 的裸层复用是静默悬挂（重建连解释坐实）；B 的 EOF/RST 变体在
    node:http 层同码（ECONNRESET）、fetch 层可区分。

## 边界（时间与规模）

- P8–P12 的观察窗有界（3–70s），「不落定」均为右删失下界、非「永不」证明；dispose 挂起格以源码
  无超时逻辑 + 窗口不落定共同支撑。
- mbp2（Darwin）与 cb8（Linux）行为差异未系统排查；p2b/p1_smoke 复验一致是移植可用性的证据，
  不构成全谱系跨平台等价声明。macOS 无 `setsid` 命令，逃逸用例用 `python3 os.setsid()` 构造。
- P8/P9/P10 的注入均不改动基线文件（假 channel / 假 shell provider / 假 snapshotStore 都是运行时注入）。
- P14 的 alwaysOn 失败与 cron 记录包装均为运行时注入（`createAlwaysOnManager`/`createCronManager`
  组合覆盖点，基线自带 test/composition override）；P15 的 automation 是快停 stub（单飞的可观察面），
  closeServer 是真实 `server.close()` 挂在 driver 持有的 WS 上（P2b 机制）——信号接线照抄
  `src/cli/pilotdeck.ts:266-280`。
- P16 的机制 B（bind 保留 + accept 后 RST/EOF）是纯 node 受控构造（基线不存在该机制，观测面
  不来自基线）；机制 A 对真实 server 的裸层复用观测用 `allowHalfOpen` 客户端（收到服务端 FIN 后
  可写侧保持打开，才能真实观测复用后果）。
