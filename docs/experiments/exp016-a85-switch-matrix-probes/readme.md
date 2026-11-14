---
type: experiment
number: "016"
date: 2026-11-09
title: A8.5 切换状态机矩阵 · 探针装置进仓与补格（#2251/#2255/#2258）
status: 装置进仓（P1–P7 移植参数化）+ P8–P16 补格完成（#2251/#2255/#2258，批次 p2batch-d6 收口）+ #2259 零字节观测窗口径修正（≥95 s 待补）
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
  <a id="p12-window-spec"></a>p12 合规复跑（≥95 s 观测窗，#2259；fix-r5 起本段为 p12 装置规约正本——
  矩阵侧只留指针）：`P12_WINDOW_MS=120000 node scripts/a8-5/p12_zero_byte_close.mjs --out <新目录>`
  （窗长须为 (0, 300000] 内安全整数毫秒，未设置或空串回退默认 70_000，非法值 exit 2 且不落盘；
  端口默认 39891、可 `--port`/`P12_PORT` 改（非法值同 exit 2）；占用预检失败与 bind 失败
  （EADDRINUSE——预检→加载→bind 间竞态败者，fix-r5）均受控快速失败 exit 3、不落观测记录；
  三超时字段两层（server 直读/包装层）均读不到 ⇒ probe-shape-mismatch 受控 exit 4、不落观测
  产物（fix-r6，第四态——先修探针再复跑）；证据名带窗长+时间戳+pid，不覆写既有证据）。**判据与判废条件（fix-r5 登记、fix-r6 结构性收口；
  判废条件照 `specs/gateway-parity/diffs/A8.3-CH-SMS-012-R1.md:121-134` 逐字段对齐，且判读以
  下文判废面矩阵为正本）**：产物须含首条 `scenario` 记录——`headersTimeout`（两层实读：server
  对象直读、未命中再读 `server.server` 包装层——真实基线返回对象可能是包装层而非原生
  `http.Server`（close() 为 Promise 形态为证），三字段同形回退；`serverShape` 记录实际命中层级
  `native`/`wrapped`；两层均读不到 ⇒ **第四态 probe-shape-mismatch**：探针 exit 4、不落观测产物、
  不进 K/I/S 三态也不判有效/判废——**先修探针再复跑**，这是 ≥95 s 合规复跑的验收前置）
  **须 =60000**；`requestTimeout`/`connectionsCheckingInterval` 同形两层实读）/`port`/`windowMs`/
  `windowEnvRaw`/`serverShape`。observation 行判别字段 `firstByteMs`/`bytes`/`hex`/`closeSettled`，
  时间字段 `closeCalledMs`/`elapsedMs`（= R1 `stopWaitedMs` 对应面）恒有限数值、`closeResolvedMs`/
  `connectionEndedMs`/`closeWaitedMs`（= `closeResolvedMs − closeCalledMs`，settle 才有限；R1:127
  「提前 resolve ⇒ 走到的不是真实监听器」的 p12 对应面）事件发生才为有限数值（「窗内不 settle」
  正是观察对象，与 R1「必须有限」的差异点如实登记；`connectionEndedMs` 终值在 cleanup 行落定，
  200 ms 内未达记 null、此时该字段只用于 observation 行结论）；note 谓词（fix-r5）：判「支持『close 清
  checker ⇒ 永不 settle』」须同 observation 行核「bytes=0 且窗内无 end/close/error 服务端事件
  且 socket 本地仍开」，不满足只报事实不作机制归因；fix-r7 boundary#1（M11 自守）：「支持②」
  另须 scenario 行 `headersTimeout=60000`——读到但 ≠60000 时该产物按矩阵 M11 判废，探针
  不输出支持性结论、只报事实（fix-r8 boundary#1 + sibling#1：该自守**短路前置到一切 note
  分支之前**——≠60000 一律走 M11 判废事实分支，settle/有字节形状同样不得输出机制结论；
  分支序收敛进 `p12NoteBranch` 纯函数 `scripts/a8-5/p12_note_branch.mjs`，微测
  `p12_note_microtest.mjs` 直调）；settle 分支 note 携带 `closeWaitedMs` 与
  「是否 ≥60 s 相位窗」（fix-r6），且命中早 settle 判废面时产物带 `invalidationHit` 标注。
  **判废面矩阵（fix-r6 结构性正本——形状 × 判定全覆盖，判官按本表判读；r3→r5 逐轮补词
  不收敛后改为枚举形状）**：

| # | 形状（observation/scenario 字段组合） | 判定 | 探针侧行为 | 对应实测形状 |
|---|---|---|---|---|
| M1 | ≥95 s 窗不 settle、bytes=0、无窗内服务端事件、socketLocalStillOpen=true、headersTimeout=60000 | 有效（支持②判别面；第五条件=判据载重字段——≠60000 时 M1 不成立、落 M11 判废，fix-r7 起） | note 支持②分支 | fixr6 m3-delay0（plain/60000/96 s 不 settle、bytes=0——`STUB_CLOSE_DELAY_MS=0` 竞态计入侧；fix-r7 改引：原引 hang-96 的 headersTimeout=3600000，按 M11 即判废产物）；fixr7 m1-plain60000 同形复核（close 延后 300 ms 保证计入，新自守谓词下支持②分支照常命中） |
| M2 | settle 且 closeWaitedMs<1000 且 firstByteMs=null 且窗内无事件且 connectionEndedMs=null | **判废**（早 settle/连接未被计入——close 落定不可归因于该连接的任何可观测事件，非零字节观测；R1:127 对应面） | `invalidationHit` 标注 + note 判废句 | stub close 落到不含本连接的 server（STUB_CLOSE_TARGET=other） |
| M3 | settle 且 closeWaitedMs<1000 且窗内有 error/close 事件（connectionEndedMs≈closeResolvedMs） | 有效（settle·机制=close 清/断该连接（未被计入时 RST、计入则空闲清理），非 408 驱动） | note settle 分支 + closeWaitedMs<60 s 相位窗 | 实测 STUB_CLOSE_SYNC=1（fixr6 race-close-sync：settle、closeWaitedMs=0、事件=[error,close]）；STUB_CLOSE_DELAY_MS=0 同轮落竞态另一侧（连接计入→不 settle，同 M1 形状）——两态均为 close 时序竞态的真实结果，fixr5 目录旧 delay0 run 出过本行形状 |
| M4 | settle 且 closeWaitedMs≥60000（408 相位窗内，firstByteMs 通常非 null） | 有效（settle·408 驱动面） | note settle 分支 + ≥60 s 相位窗 | stub late408（61 s 写 408 字节后销毁连接） |
| M5 | 不 settle 且 firstByteMs≠null | 有效（排除「close 清 checker ⇒ 408 不发」） | note 排除分支 | fixr7 m5-bytes（stub bytes 在 headersTimeout=60000 下重跑换引；fixr6 旧 run 抬到 3600000，按 M11 即判废产物） |
| M6 | 不 settle、bytes=0、窗内有 end/close/error 事件或 socket 本地已关 | 有效（只报事实，不作机制归因） | note 事实分支 | fixr7 m6-finnever（stub finnever 在 headersTimeout=60000 下重跑换引；fixr6 旧 run 抬到 3600000，按 M11 即判废产物） |
| M7 | 不 settle 且窗 <95 s | 有效·右删失下界（不得作对拍判 PASS 依据） | note 右删失分支 | 1 s 窗冒烟 |
| M8 | 1 s 冒烟窗（装置自检） | 有效·装置自检（同 M7 形状） | 同上 | 同上 |
| M9 | 三超时字段两层（server/server.server）均读不到 | **第四态 probe-shape-mismatch**：exit 4、无产物、先修探针再复跑（不进 K/I/S、不判有效/判废） | exit 4 + stderr 诊断 | stub STUB_SHAPE=unreachable |
| M10 | `serverShape=wrapped`（命中包装层） | 有效（读取层级记录在案） | scenario 带 serverShape | stub STUB_SHAPE=wrapped |
| M11 | headersTimeout 读到但 ≠60000 | 判废（R1:124） | headersTimeout≠60000 **一律走判废事实分支**（fix-r8 boundary#1 + sibling#1 短路前置：settle/有字节形状同样只报事实并写明按 M11 判废，不再仅在「不 settle 且无字节」可达——分支序收敛进 `p12NoteBranch` 纯函数 `scripts/a8-5/p12_note_branch.mjs`，微测 `p12_note_microtest.mjs` 直调；反证不该出现时不输出「支持」） | stub hang 模式（抬到 3600000，作判废演示；fixr7 重跑核：note 只报事实并写明 M11 判废）；fixr8 补 M11×settle（late408+3600000）与 M11×bytes（bytes+3600000）两格负控（`STUB_HEADERS_TIMEOUT_MS` 通用旋钮），红侧（短路挪回只盖「支持②」分支）两格必红 |
| M12 | 无 scenario/observation 行或必需字段缺失 | 判废（R1:121-124） | — | 受控探针构不出：健康探针必落两行；该条防的是进程被杀/产物截断/主控写坏 |
| M13 | connectionEndedMs（非 null 时）< closeCalledMs | 判废（连接在 close() 前已没了） | — | 受控构不出（fixr6 实测 earlyrst 最近似形状 connectionEndedMs=26 > closeCalledMs=25——探针 connect→close 为同一同步块，连接结束事件只能在下一 tick 派发、恒晚于 close 调用；该条防探针结构变更/写坏产物） |
| M14 | closeSettled=false 且 elapsedMs<windowMs | 判废（早退/被截断） | — | 受控探针构不出：观测循环只在 settle 或窗满退出，两者都使条件不成立；防进程被杀/截断产物 |
| M15 | 窗 <95 s 而结论含「支持」 | 判废（探针自守谓词防绕过） | — | 受控探针构不出：COVERS_WORST_PHASE 谓词在探针内强制 |

  判废（任一 M2/M11/M12/M13/M14/M15 命中即作废重跑）；有效性只看上述条件、不看退出码
  （exit 4 例外：第四态根本没有产物可判）。边缘登记：settle 且 closeWaitedMs≥1000 而无可归因
  证据（无字节、无事件、connectionEndedMs=null）不落 M2（时间下界外的弱证据形状）——判官
  按 M12 字段完整性复核后人工判读，理由：该形状与「close 不等该连接」的真实机制不可由
  观测面区分，且 r3→r5 教训是不为未观测形状扩判废词。
- **必跑（fix-r4 接线登记）**：P16 守门/note 纯函数的入仓负控微测
  `node scripts/a8-5/p16_gate_microtest.mjs`（无参数、无落盘副作用、不依赖 TS 基线 dist，
  端口 39994；退出码 0 = 全过 T1–T16，任一断言失败 exit 1、总时限 watchdog 60s 超时
  exit 2——退出码集 {0,1,2} 复用不变，fix-r6：探针被 import 的宿主（含
  import.meta.main 缺失的 22.0–22.16 段）不再于 body 前被 exit 3 杀，微测在该段由
  不可用改为可用）——凡改动 `p16_stop_acceptance.mjs` / `common.mjs` 的守门/note 面，本微测必跑、
  原始输出归档进当轮证据目录。**该微测当前不被任何 runner/CI 收录**（fix-r4 评估：
  `scripts/a8-5` 全仓无脚本/CI 引用；`.github/workflows/ci.yml` 的 check job 有钉
  node-version 22.22.0 的 node 步（`test_metadata_observer.mjs`）可挂 `node
  scripts/a8-5/p16_gate_microtest.mjs`，或新增 `check_a8_5_gates.sh`——CI 闸门面属全员
  策略，归 #2759 P2-跟进由协调者终判）。
- **必跑（fix-r8 接线登记）**：P12 note 分支纯函数的入仓负控微测
  `node scripts/a8-5/p12_note_microtest.mjs`（无参数、无端口、无落盘副作用、不依赖
  TS 基线 dist，同步执行；退出码 0 = 全过 T1–T11，任一断言失败收集后汇总 exit 1）——
  直调 `p12_note_branch.mjs` 的 `p12NoteBranch`（p12 note 分支序整体收敛于此，M11 短路
  前置）；凡改动 `p12_zero_byte_close.mjs` / `p12_note_branch.mjs` 的 note/分支面，本微测
  必跑、原始输出归档进当轮证据目录。**该微测当前不被任何 runner/CI 收录**（同上 p16 微测
  的收录问题，归 #2759 P2-跟进）。
- **纪律**：仅 loopback 高位端口（39871–39898 段；p16 守门微测用 39994——fix-r4 挪出该段，
  原 39894 与 p15_driver 的 A-elevate 用例撞号）、隔离 `PILOT_HOME`/`diskSpillDir`、不外发网络
  （provider 指不可路由 loopback）、子进程记 pid 逐个收尸、观察窗全部有界、无无界派生。
- **证据**：P1–P7 原始输出（cb8）归档于主仓
  `_tmp/20261107-archive-wt-1353/20261026-issue-1353/raw/`；P8–P16 原始输出（mbp2）在
  `_tmp/20261109-p2batch-d6/raw/`（提交后归协调者归档，路径以 RESULT 登记为准）。

| 探针 | 覆盖格 | 一句话结论 |
|---|---|---|
| p1_smoke | 装置自检 | mbp2 复验通过（health/hello/describe/close 全过） |
| p1_p2_stages | E1/E2 × S0–S4 + t3 | 见 matrix §4.1/§4.2（cb8 原始数值） |
| p2b_block | close() 阻塞规则 | mbp2 复验：无连接 settle / 一条 WS 2000ms 不 settle、断开后 1ms / 空闲 keep-alive settle——与 cb8 一致（#2756：空闲 keep-alive 格以首次 `/health` 入池为前提，前提未成立记 `C/SKIP-idle-keepalive`；fix-r1 起还须 timedClose 前池内仍占用，池被服务端空闲超时提前收掉记 `C/SKIP-pool-drained-before-reuse`——两种降级下结论行均记 `skipped:true`，不作 A14② 对照；fix-r2 起降级行 `premise`（初始入池前提原值）与 `premiseAtClose`（close 前池状态）分立、`settledAfterHoldMs` 由 timedClose 显式置 null，不依赖展开顺序；fix-r3 起「C resolved after agent destroy」等待/打印分支改 `premiseAtClose && settledAfterHoldMs === false` 精确判，降级路径不再落该控制台行） |
| p2c_post_dispose | S3/S4 既有 WS | 见 matrix §4.1（cb8） |
| p3_child/p3_driver | t4 drain 注入 | 见 matrix §5.2（cb8） |
| p4_cron | E3 × 全阶段 | 见 matrix §4.3（cb8） |
| p5_background | E5 × 全阶段 | 见 matrix §4.5（cb8） |
| p6_real_server | t5 端到端 | 见 matrix §5.1/§5.3（cb8）（#2756 fix-r2：`keepalive-http` 在途行事后复核 `reusedSocket`——SIGTERM 前池占用复检通过但请求落新连接（复用池在发出前被收尾）时降级记 `ka/SKIP-reuse-fell-to-new-conn`、该行以 `skipped` 出现；fix-r3：降级行保留 once 原字段 `ok/error/code/reusedSocket`，reason 按实测值推导——`reusedSocket` 全路径恒布尔、error 路径非 undefined，实测见 fix-r3 判别性实测二） |
| p7_channels | E4 × S2 + 竞态 | 见 matrix §4.4（cb8） |
| p8_channel_stop_race | #2251 条目 2 | close 只等快照 handle：在飞 hot-reload stop 不被 join（close 1ms settle）、失败 stop 回填 map（deliverCronResult 复投 true）、二次 close 复用 rejected promise 不重停 |
| p9_background_descendants | #2251 条目 3 | 同组 TERM 免疫后代零残留（组 SIGKILL 覆盖）；setsid 跨组后代逃逸存活（ppid→1、自有 pgid；任务 pid 锚定的 ppid 链漏检） |
| p10_background_hangs | #2251 条目 4 | start 永不 settle ⇒ dispose 卡 whenStartsDrained（3s 窗右删失）；快照写挂起 ⇒ TERM 已杀、dispose 卡 `await done`（settleEntry 停 store.write） |
| p11_child/p11_driver | §4.5 S1–S3 接纳 + 连接两态 | closing 窗口内 E5 新任务被接纳（running）；断开 ⇒ dispose 杀（码 0 零残留）；保持 ⇒ close 永不 settle，SIGKILL 后 detached 任务成孤儿 |
| p12_zero_byte_close | A4 零字节，70 s 窗（右删失下界；#2259，见结论 6） | GatewayServer 实例级：零字节连接使 close() 70s 窗不 settle（窗未覆盖最坏相位 408 落点，见结论 6）、服务端不主动断该连接（fix-r4 限定：旧 70 s run 无字节观测面——探针只挂 close/error，只证窗内无 close/error 事件；字节面（408 是否发）由 p12 判别面判别，≥95 s 观测待复跑。复跑用法/参数/退出码与判据判废条件见「做法」p12 段（锚点 `p12-window-spec`，fix-r5 起为装置规约正本）；判据字段名：`firstByteMs`/`bytes`/`hex`/`closeSettled` + 首条 `scenario` 行（`headersTimeout` 两层实读须 =60000，`serverShape` 记命中层级，读不到走第四态 exit 4；`closeWaitedMs` 落 observation 行；判读以 p12 段判废面矩阵为正本，判废条件照 R1:121-134）） |
| p13_empty_set | #2255 空集合格 | 组件级三格 + 全空五步组合级：全部 0–4ms 落定（空集合不挂起、退出路径可达） |
| p14_child/p14_half_stop | #2255 半停格 | 真 cron 先真停不回滚 + alwaysOn stop 失败：单失败原样抛、finally 照走 unpublish、二次 stop no-op；server 级半停窗口 E1/E2 可服务、后续四步照走、退出码 0 |
| p15_child/p15_driver | #2255 单飞 + 退出码抬升 | 重复信号（SIGTERM×2+SIGINT）下 automation.stop 恰 1 次（两层单飞缓存）；挂起窗口内 late unhandledRejection ⇒ exit 1（对照 0） |
| p16_stop_acceptance | #2258 停接纳机制观测面 | 机制 A（关 listener，真实 server）：新连接三层 ECONNREFUSED、空闲 keep-alive 服务端 FIN、裸层复用已 FIN 连接 = 写成功后静默悬挂（同 keep-alive agent close 前池化、close 后复用直测 ECONNREFUSED connect 相位——重建连直测）；机制 B-RST/B-EOF（纯 node 受控）：握手完成后 RST（ECONNRESET）或 EOF（node:http 层两者同为 ECONNRESET 码，fetch 层可区分 ECONNRESET / UND_ERR_SOCKET）。前提守门（#2662，#2759 精化）：裸层以头/体切分后的头块查 Connection（体行不误判）、缺头仅完整 HTTP/1.1 响应隐式 keep-alive、显式头无论 1.0/1.1 一律按 token（`parseRawGate` 落盘原值）；agent 入池以 `agent.freeSockets` 直接观测量为准（头推断仅降级路径，`derivePooledPremise` 落盘——fix-r3 起 header/direct 分量随 `pooledHeader`/`pooledDirect` 字段落盘，reason 经 `pooledSkipReason` 按 pooled1/freeSockets/头推断三态分叉）——入池前提失败落 `A/SKIP-agent-reuse`、close 前池被收空落 `A/SKIP-pool-drained-before-reuse`（fix-r1 池占用复检、`A/pool-at-close` 显式行）；「静默悬挂」句由 `write2`（writeOk + write 回调 flush）与 `eventsAfterWrite`（保留 at/relToWrite；写前窗 [t0,writeAt)/写后窗 [writeAt,∞) 分区不双占）实测推导（`silentHang`，fix-r3 评审 P1 簇修：`write2.flush.flushed` 进守门——pending/回调失败（对已销毁 socket 写的 ERR_STREAM_DESTROYED 形状，实测写同步未抛、写后窗为空、error 全落写前窗 at<writeAt）不得判「写成功」），FIN 前提以 `finBeforeWrite`（`sawCleanFin`）体现；机制 B note 由 rawEvents 推导（sawConnect/sawEnd/sawError/sawAbnormal 落盘，握手前 error/无 EOF 等形状如实降级；fix-r3 起 rst 分支异常判定与裸层共用谓词——error ∥ close(hadError)，只余 close(hadError=true) 时不降级为「RST 未到达」）；B-EOF 收尾加界（`srv.close` 1.5s 界 + 残余连接销毁兜底，JSON+DONE 为完整性信号；fix-r3 实测 net.Server 无 `closeAllConnections`（http.Server 才有）——旧 `srv.closeAllConnections?.()` 兜底实为静默 no-op，改为跟踪已接受连接手工销毁，二次 race 结果落 `B-<mode>/srv-close-final` 显式行、未落定如实写降级行）。前提未成立的格降级记录（字段+note 原值），不出上述结论（#2756：keepAliveToken/入池前提判定收敛进 common.mjs——`keepAliveProbe.once` 返回 `premise`/`premiseReason`，本探针内联 agent 炮台改用共用 keepAliveProbe，p2b/p1_p2/p6 三探针同形；fix-r1：裸层 `rawPremise` 亦改共用 `keepAlivePremise`、once 另带 `reusedSocket`、agent 暴露 `freeSocketCount()` 供 close 前复检（fix-r2：`freeSocketCount` 改按各 origin 数组长度求和——原 origin 键计数在同 origin 多 socket 时低估；`A/agent-pooled-request1` note 收窄为「请求 1 已入池」——close 后 `reusedSocket` 两种路径同为 false 仅作参考，判别依据是 close 前的池占用复检）；fix-r3：池占用复检无论结果都落显式 `A/pool-at-close` 行，note 指向该行、不再以 SKIP 行缺席作负证据；#2759 fix-r2 两线整合：前提判定以 common 单源为结构主干，头/体切分与 freeSockets 直测并入该主干——入池直测在共用 agent 上经 `freeSocketKeys()` 取直接观测量（direct 压过头推断）、与 close 前池占用复检并存，pooled 侧头推断即 once 随响应带回的 `premise`（同源同形）、裸层与 pooled 缺头语义单源对齐；#2759 fix-r3：守门纯函数仓内负控微测进仓 `scripts/a8-5/p16_gate_microtest.mjs`（import 本探针真实代码、不复制逻辑），探针 import 无副作用（路径/Recorder 惰性初始化于 main）、主模块守卫改 `import.meta.main`（v22.22.0 实测 symlink 入口下不再静默 no-op）；p2b/p1_p2/p6 三探针的建池前提**登记豁免**：保持 once 头推断 + close 前池占用复检兜底（结论行被 #2756 fix-r1 复检与 SKIP 守门兜住、不产生假 PASS/假 FAIL），freeSockets 直测消费是 p16 侧 #2759 精化、升 common 共享归 #2759 跟进单）；#2759 fix-r4（2026-11-11，主机 cb8）评审修复：主模块守卫兜底——`isMainEntry` 纯函数（import.meta.main 为真，或 argv[1] 与模块 URL 两侧 realpathSync 归一后同 URL），import.meta.main 未提供的 Node 段（22.0–22.16）直接执行不再静默 no-op exit 0（尺子假通过面，评审 boundary#1），该段且 argv 兜底未命中时先独立路径复检（fix-r6：argv[1] 与本模块文件两侧 realpathSync 归一后相同才显式 exit 3——「直接执行而守卫未跑 main」以非零码可察；否则安静交还宿主。被 import 的宿主任何 Node 段不被杀——fix-r4 原始取舍与 fix-r6 收窄见各轮 RESULT）；现代 Node 被 import（main===false）安静交还宿主（微测/T12 消费方不受影响）；B-EOF 分支与 rst 分支同构（干净 EOF 判 `sawEnd && !sawAbnormal`，「end + 只余 close(hadError)」单列降级句，不再把异常拆链说成干净 EOF）；`A/raw-keepalive-request1` 增落 `connValue`（多行 Connection 形状可由落盘原值复算 rawPremise）；微测端口 39894→39994（原与 p15 A-elevate 撞号）+ T4 listen 挂 error 监听（EADDRINUSE 受控 FAIL）+ 总时限 watchdog 60s + T13/T14 守卫负控格（直接执行格：守卫误判为非 main 时必红），partB `srv.listen` 同挂 error 监听；#2759 fix-r5（2026-11-12，主机 cb8）评审修复：href 归一抽成可测导出 `entryHrefCandidates` + 微测 T15 **导出纯函数直调格**（symlink/空格路径/目录/不存在路径真实文件系统形状实跑——判杀函数体内变异：返回字段互换 / 去 realpathSync 归一 / pathToFileURL 误用；调用点接线——真实 process.argv[1]/import.meta.url 实参与 isMainEntry 消费——零运行时覆盖，登记待办 coord 跟进单）；#2759 fix-r6（2026-11-12，主机 cb8）评审修复：分支③ exit 3 收窄为独立路径复检（`isSelfEntryPath` 导出 + 微测 T16 直调格——路径级 realpathSync 归一，与 href 守卫变异面分立）——被 import 的宿主（含 22.0–22.16 段 import.meta.main 缺失形状）不再于 body 前被杀、微测退出码集 {0,1,2} 复用不变；T14 直跑负控格临时 scratch finally 清场 + 清场断言（对齐 T15）；登记面同步（本 readme 必跑行 T1–T16 + 退出码集复用、matrix §4.2 要点③「守门过为限」限定） |

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
6. **零字节格**（p12）：GatewayServer 实例级右删失下界 70s——408 落点为相位窗 (60, 90] s 内的相位值（含毫秒级 tick 执行抖动），随网格相位变化（见 `A7.4-HTTPSTORE-KEEPALIVE-001-R5` / `A8.4-NATIVE-HTTP-002-R2`），70 s 窗未覆盖最坏相位落点、不得作对拍判 PASS 依据（#2259）。两说须分列判别：①SmsChannel 实例 close → `httpServerPreClose` 清 connections checker（`A8.3-CH-SMS-014-R2` 机制链，SmsChannel 实例级在档）；②GatewayServer 实例 close 路径是否同样清 checker **未实测**——p12 起零字节 socket 挂 data/end 监听（`firstByteMs`/`bytes`/`hex` 落产物），≥95 s 窗内「不 settle 且无服务端字节、无 end/close/error 服务端事件且 socket 本地仍开，且 scenario 行 headersTimeout=60000（fix-r7 第五条件，M11 判废面——≠60000 的产物按矩阵 M11 判废）」（fix-r5/r7 谓词）支持②同①（408 不发）、「不 settle 且有字节」则排除①型机制（settle 等待另有原因）、谓词不满足时只报事实不作机制归因。**该 ≥95 s 观测待复跑**（当前未跑，不伪造观测）：复跑命令、判据与判废条件见「做法」p12 段（锚点 `p12-window-spec`）；判据字段名 = `firstByteMs`/`bytes`/`hex`/`closeSettled`/`closeWaitedMs` + 首条 `scenario` 行（`headersTimeout` 两层实读须 =60000、`serverShape` 记命中层级、读不到走第四态 exit 4——先修探针再复跑；判读以 p12 段判废面矩阵 M1–M15 为正本，判废条件照 R1:121-134）。
7. **空集合不挂起**（p13）：零任务/零 channel/零连接下全部停机面 0–4ms 落定——「空集合快速干净退出」
   断言有基线支撑（A13 同构）。
8. **半停无重试**（p14）：部分组件停成后失败——已停不回滚、失败组件永不再被停（二次 stop no-op）、
   控制面照常摘除；server 级半停窗口 E1/E2 照常可服务、后续步骤照走、退出码 0。
9. **单飞 + 退出码抬升**（p15）：重复信号下停机只进一次；退出码可被停机挂起窗口内迟到的
   unhandledRejection 抢写为 1（对照 0）——退出码不只由停切换过程产生。
10. **停接纳机制观测面**（p16）：机制 A 在 connect 相位明确拒绝并立即释放端口；机制 B 实测形状握手
    完成后失败推迟到读写相位且端口不释放（握手前 error 等形状由 rawEvents 推导、如实降级）；A 的裸层
    复用在前提成立时为静默悬挂（写成功 = writeOk + 写回调 flushed + 写后窗无回包/无 RST，由
    write2/eventsAfterWrite 实测支撑，重建连解释坐实）；B 的 EOF/RST 变体在 node:http 层同码（ECONNRESET）、fetch 层可区分——以上
    均以前提守门通过为限（keep-alive 头守门、入池直测、写前干净 FIN），未过门的格降级记录、不计入结论。

## 边界（时间与规模）

- P8–P12 的观察窗有界（3–70 s；p12 经 `P12_WINDOW_MS` 可放大，≥95 s（保守 120 s）观测才可立对拍断言；非法值探针拒绝落盘），「不落定」均为右删失下界、非「永不」证明；dispose 挂起格以源码
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
