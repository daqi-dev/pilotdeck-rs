> 历史预登记：以下准备态只指登记当时；后续实际 HEAD / 产物 / 运行范围见本目录 [README.md](README.md)，实时接续仍以 `state/board.md` 为准。

# #65共同提交域测试预注册

固定于候选及本轮oracle执行之前；本文件不含候选结果。授权依据R25/R40：`/Users/da/ws/pilotdeck-rs/_build/wt-d6/state/decisions-20261016.md`，本次读取SHA256 `5bb21e58e07754e1e0e9d0df5c50ed558b3d585cdc8629e9a42d1b99d6fa1104`；架构依据主仓`_tmp/20261016-debt-cleanup/order-architecture/next-domain/report.md`及其index。独占wt-a4m3，原件只追加到主仓domain-prototype目录；不启产品、不改资源/冻结、不运行Cargo。

## 输入与oracle

独立oracle先运行原cc7c4e默认createLocalGateway及真实TextWebSocketConnection/GatewayWsConnection，捕获完整frame十六进制与实际callback/方法调用/Promise状态。成功后固定oracle文件SHA，再执行候选。同一隔离磁盘夹具路径、同一输入脚本；hello包含随机交互binding，仅保留原件作健康握手，不纳入请求矩阵比较。请求及控制帧逐byte比较，不归一时间/path/载荷。

D1：describe→supported、supported→describe，同一data。
D2：supported与describe各自→ping及→close，分别同一data、两次同步data、setImmediate真实后续回调；反控把cross-turn合并为同步或把同data拆跨turn，应抓到至少一项pong顺序/close抑制差异。setImmediate是显式输入脚本事实，不是产品等待补丁。
D3：unknown同步throw与supported/ping正反序；真实session_model_get缺参Promise拒绝与supported/ping正反序。成功Promise取D1真实路径。阶段全序以先录oracle为准，不猜跳数。若缺参不是Promise拒绝须记录精确轨迹，不能换方法刷绿。
D4：默认list_projects→supported、supported→list、list→ping、list→close、观察list真实response后ping；实际FS与原六格共同保留。list IO不能归到ReadyAtAdmission。
P1：真实describe已调用但显式扣住其原返回Promise；对照真实describe已经commit而只扣通道。随后即时slot。前者local可先，后者host已得较小seq；去seq按到达写反控必须失败。gate是干预，不声称自然pending窗口。
P2：旧379真实Rust原件只有route/COMPLETE采样而无目标自然JoinSet可消费因果点，首轮NOT_RUN，不能由本轮Node模拟代填。
P3：独立runtime LaterReady与随后epoch相对关系首轮INDETERMINATE，未建可合法移植的共同完成事实；不能据偶然同序判通过。
C1：丢commit、重复seq、乱序运输、跨connection/generation、未知token、close后local：乱序运输须按连续seq复原；其余严格拒绝或finalize有界失败。延迟/分块只作用已决定的commit。
C2：空矩阵、少一格、host非本地锚点缺失、候选host执行supportedAgents/收到local原请求、控制帧缺失均拒绝。原始method钩子必须保留原返回Promise/同步throw，仅同步记录调用与返回性质，不额外then观察而改变微任务顺序。

## 候选与判据

受控epoch携ordered HostFrame/NativeSlot，仅supported即时类有资格。NativeSlot只有opaque token/readiness；native payload/id留唯一writer侧，来自旧379真实Rust结果并绑定每blob SHA。host slot调度只模拟一次对已fulfilled Promise的await续体；其依据是上述真实async无await方法+真实RPC await路径，并以先录oracle阶段核验，不增加跳数凑绿。host真实业务路径只收非本地帧。唯一commit owner为所有host wire（含错误/通知/pong/close）及slot分配connection/generation/连续seq；writer替换slot并恢复transport乱序，不编造缺失帧。close后失效，local不得再commit。

全部矩阵行必须存在，未执行也写NOT_RUN及缺因果。D1–D3通过只能SYNC_SUBDOMAIN_ONLY；只要P2/P3等必要门缺证，总体NECESSARY_EVIDENCE_MISSING。D4通过也不能外推自然跨读可消费窗口。不能输出全域兼容PASS，不能关#65/A5.4。

## 预算、失败与回收

原Node22.22.0、tsx4.22.3身份按旧rust-six索引逐SHA确认；资源src1000文件/解析配置及工具前后绑定。每child硬上限60s、stdout+stderr≤1MiB，总窗≤10min、全部新增≤16MiB。finally只回收自己的child/hook，保留每次失败原件；同类失败三次挂起该项，转控制，不改oracle期待。首轮不Cargo/不新target/不监听/不外网。提交前冻结README完整SHA核对FROZEN，推前gitleaks。独立评审仅由中央队列安排。
