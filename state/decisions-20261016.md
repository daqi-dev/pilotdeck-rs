# 2026-10-16 开发恢复决策

## R16 产品入口账本的有限实现（AI 代批，待人复核）

- 卡在哪：已有101 method与263 HTTP清单分别服务阶段尺子，未覆盖默认CLI/后台监听/桌面旁路；照搬它们会漏范围。
- 谁受影响：整体迁移的任务归属和后续收尾；不改变C/A/B冻结分母、评分或已接受差异。
- 能否撤回：账本/脚本/计划可经后续PR修改；五个旧工作树和人手意向保持。
- 选项与后果：仅手填清单会漏项；启动整套基线枚举引入用户现场与装置副作用；采用离线固定Git源码AST＋少量已读实际源码的语义锚点，独立验证完整集合和来源。
- 建议与执行：采用离线枚举；账本将每条入口关联唯一任务组，组维护阶段/issue/计划/验收，所有当前状态保守不标完成。D0–D4拆为#122–#126；A/B/F跨阶段接续分别#127–#129，sidecar factory/TCP兼容缺口#130。
- 可复用原则：覆盖门与行为验收分离；固定基线入口即使尚无实现也必须有明确归属，保留桌面壳不豁免其后端旁路。
- 断言：独立枚举＝账本，删任意真实项、重复、多项、坏归属/状态/源码/证据/哈希皆FAIL；263 HTTP与历史runtime独立对账。没有新增冻结阈值。

## R17 #118复用主树并保留旧工作区（自决）

卡在哪：worktree已满5个，原#118落后main且旧RESULT混入#91；谁受影响：本轮P2清账与证据。可撤回：原958ec6c和旧分支/工作区/none均保留，根目录新分支单独持有修改；不动冻结语义或意向。选项：另开第六树违反上限；改旧树会覆盖接续现场；建议复用主树创建codex/p2-cleanup-resume，合并新main后只向现有PR分支做快进更新，不删除原分支或工作区。按建议执行，main最终仍使用rebase合并。原则：同步与证据保护分离，旧产物可回溯而新验证绑定新工作区；断言：旧wt-p2c原HEAD及未跟踪哈希不变，新树独立CARGO_TARGET_DIR。

## R20 #118改用线性提交历史（自决）

卡在哪：GitHub对11bd6da返回“This branch can't be rebased”；R17的快进同步引入merge commit，不能完成规定的rebase合并。谁受影响：现有PR118提交指针和CI，不影响产品语义。可撤回：本地codex/p2-cleanup-resume保留旧tip，另推同名保留引用后，PR分支只在remote仍等于11bd6da时用force-with-lease更新；main绝不强推。不动尺子/意向。选项：改squash违背合并方式；另开重复PR保留两处评论；建议以origin/main为父重建同一文件树，Git write-tree与旧tip树完全相同后补本卡，再两家核元数据增量和重新跑最终CI。按建议执行。原则：证据保全与线性合并分开，受控lease只更新已核持有的PR分支，遇未知远端新tip立即停止；断言：linearize-tree-proof.json同树、仅本卡增量，旧引用可恢复，最终两门CI新HEAD绿。
## R18 D6 A2 exact产物的临时绑定（自决）

- 卡在哪：冻结A2运行器固定主仓src/target/debug/pilotdeck-gateway；独立target产物不能靠共享旧二进制证明。
- 谁受影响：仅本轮候选A2接入和固定未跟踪单文件；root已明确授权这一单文件写入，其他主仓文件不写。
- 能否撤回：只在parity_lock内原子创建缺失文件软链，finally核inode/device/target后撤销本轮拥有链接；已有路径（含悬空）拒绝接入。
- 动不动尺子/意向：不改冻结装置、输入、八场景十readonly及评分；沿用原预注册判据，当前七local，archive三项保持透传。
- 选项与后果：写共享target会污染来源；改运行器涉及冻结变更；缺失固定文件名临时绑定独立exact二进制可保留原装置和产物身份。
- 建议与执行：采用主协调认可的a2-plan外层脚本；lock覆盖软链和direct/proxy两轮，记录Cargo artifact、源码HEAD、前后binary SHA、装置哈希和process audit。
- 原则与断言：产物位置绑定须可撤回且身份可核验；绑定无主、前后SHA/装置不变、仅七method无转发、两轮全场景DONE/comparison PASS、自有进程全停、自己的链接已撤才算本轮通过。任何不满足保留失败，不改判据。

## R19 非良构UTF16排序的精确宿主边界（AI代批，待人复核）

- 卡在哪：真实Node与ICU4X compare_utf16对孤立surrogate/U+FFFD不同；YAML模型键和JSONL同序createdAt均可达，属于行为P1。
- 谁受影响：实际参与locale排序的catalog id及同sequence组createdAt；合法Unicode继续原生。
- 能否撤回：精确检查后走既有JS_HOST_REQUIRED/requires_host整请求转发，可后续以原生比较实现替换。
- 动不动尺子/意向：不动冻结评分或差异豁免；这是已存在宿主边界的兼容性保全，不能宣称全输入原生化。
- 选项与后果：猜PUA或替换会改语义；恢复全nonASCII退回会放弃已验证D6；选择仅多项filtered catalog或同sequence比较键含孤立surrogate才转发。
- 建议与执行：采用精确宿主边界，保留paired supplementary、U+FFFD与CJK本地；原生illformed UTF16由主协调登记A8前置后续issue。
- 原则与断言：只退回当前依赖尚不能表达的可达输入；真实Node新原件、本地正控与上游observer负控明确区分单项/过滤后单项/不同sequence，历史fixture原件不改。

原生非良构UTF16排序跟进 #132，Linux真实非法UTF8 dirent端到端跟进 #133，均前置A8/#127收尾。22组Node Buffer解码原件仅证明共用CStr lossy解码结果；macOS实际建名返回EILSEQ(92)，真实非法dirent未验，不能把单元解码核验视为真实文件系统验收。

## R21 · #112 真实sidecar绝对deadline与读面结算（AI代批，待人复核）

- 卡在哪：旧固定TIME构造的operationDeadline今天已过期；session.submit退出也早于gateway catch写完registry，原无registry格没暴露后者。
- 谁受影响：仅scripts/a5-19/sidecar-b真实四格证据；产品/冻结尺子不变。
- 可撤回：可以；所有失败原件保留在`_tmp/20261016-a5-112/`。
- 尺子/意向：均不动，不预设实际terminal；本次gateway.now采用采集时刻，只让绝对deadline可进入实际model/tool，逐run记录实际operationDeadline；实测status.createdAt及durable语义时间均保持TIME，不新增时间剥除。
- 选项后果：保留旧clock只能证明过期admission拒绝，无法回答#112；修改真实child时钟会掩盖生产行为。采用生产gateway可注入clock，真实child时钟不改，并等真实第二次queue.close再观察settled三面。
- 原则：跨进程绝对deadline必须在采集时有效；目标阶段barrier与Module call先于人工fire才算该格；registry读面须在gateway pump最终close之后采集。断言已落入sidecar-b，terminal只记录实际值。
- 初轮复核更正：真实路径status.createdAt仍为固定TIME，原先“deadline clock同时改变status.createdAt”的说明不成立，已按原件纠正。改为每run独立clock只解决绝对deadline入场，不剥任何新增语义时间；新四格预注册656d6e3先提交后复采，旧20次明确事后观察。各组精确k5且稳定才通过；cleanup与observer异常显式失败。最终20/360原件见exp015/exp014追加说明。

## R22 #112接续D6的线性同步（自决）

卡在哪：并行候选均已独立收敛，D6先合main使PR135落后；谁受影响：112提交身份及接续状态，可撤回。沿R20证据保全原则，先保存本地codex/a5-112-pre-d6与远端codex/a5-112-preserved-ecf6fa5，再将四提交rebase到eb76e4a，冲突只保留并接合双方board/R18–R21；不动意向、冻结或实测语义。旧ecf6fa5的14项脚本/报告/记忆逐字节相同，D6非状态文件与新main相同；已有真实数据适用，不重复360/20/扫描/A2。新元数据交两家原session续审，PR分支仅在远端仍为ecf6fa5时exact lease更新，main不强推。原则：源码与输入未变才复用验证，提交身份变化仍重跑最终CI；断言见rebase-final-proof.json。

## R23 两小时为自动调度交接点（2026-10-16人定）

用户明确：集中清账，逐项分类/重分级，P0/P1和当前节点前置P2优先，延期仍未完成；两小时交给下一调度者，不是停止开发。按原授权执行，不逐项等批。原则：时间片结束必须移交实际现场与下一就绪任务并落实自动接续，不能把交接当暂停；CLAUDE资源条款同步。断言：每片board有实际HEAD、持有人、证据、余债、资源与下一步；自动接续配置ACTIVE只证明已配置，首次触发前不称已运行。

## R24 外审超时的有界诊断（AI代批，待人复核）

卡在哪：GLM正常代码审三次120秒内无final；部分Transport关闭恰为本地runner SIGTERM时点，不能据此断定提供者拒绝，另有一次明确400/content-filter必须尊重。谁受影响：非作者双谱系闸门，无完整final不得合并。可撤回、不动尺子或意向。选择：停止该120秒/旧scope路线并保留失败；对从未尝试的新catalog候选原task，只做一次standalone300秒正常代码审诊断（health3.883秒成功不算审过）。任务字节不改，明确过滤即停，不换词绕拒；300秒无final也挂新scope，不再变体。其间DS审计与产品修复独立继续，不用GPT/Astra补缺的非作者谱系。原则：自设预算导致的终止与服务拒绝分开记，恢复须有新前提、有界验证，不能循环重试或假报通过。

结果：catalog原task以实际候选工作区运行，124.538秒/exit0获得完整四视角和问题表；路径核验未读主树旧产品。这次有效审查提出child零匹配P2，仍需修复。原件主仓`_tmp/20261016-debt-cleanup/catalog/reviews/`，教训并入既有review-bounds记忆。

## R25 #65保留P1并先补调度证据（AI代批，待人复核）

- 卡在哪：实际TS探针证明同帧序列的响应顺序随data/微任务边界而异；Rust的read缓冲不提供这一边界。两个执行器中“host已完成但响应在途”和“host仍pending”对Rust局部可见状态相同，却要求相反响应顺序。
- 谁受影响：混合本地/上游请求及ping/close、A4 G7零本地method上游、A5.4定标与A8统一执行域；#65继续是A5.4前置，不能悄悄迁走闸门。
- 能否撤回：probe和实现可撤回，已发wire不可撤回。不改冻结或意向；任何忽略同连接全序/放宽G7均另需精确授权。
- 选项与后果：整混批或整连接转TS违反G7，且不证明TCP分块和跨批pending；FIFO/sleep/yield不能获得缺失状态。统一完成域是后续实现路线，先补真实默认装配与磁盘交错矩阵，再判断所需协作协议和依赖次序；不能凭A8未来计划称当前已修。
- 建议与执行：保留已确认P1及第二条候选P1，先形成可重现的完成/观察状态反例及真实TS矩阵；#56独立修复与其余债务照推。依据主仓`_tmp/20261016-debt-cleanup/order-architecture/report.md`及原始日志；Astra架构审查不是两家非作者终审。
- 原则与断言：传输缓冲区不能当宿主批次；同流不同受控data/read边界须各有实际oracle。当前已知差异测试只能叫诊断，不能把assert_ne通过计成兼容性通过。


## R26 #113批准原件的可移植登记（AI代批，待人复核）

- 卡在哪：B2已登记FROZEN引用主仓_tmp三份小原文，干净检出恒FAIL；完整定标/ruler厚原件仍在本地归档。
- 谁受影响：B2/B3验收可复现性；不改变C/A冻结语义。可撤回，原归档保留。
- 动不动尺子/意向：只改FROZEN的批准依据路径并迁入三原文；53注册文件、checker、captured hash、比较器、阈值/时钟判据均不变。
- 选项与后果：改checker只检查SHA会放过原文缺失；重写历史摘要会抹PENDING/null/false与旧P2；采用三原文逐字节迁入approval，厚原件仍路径+SHA+说明。
- 建议与执行：GLM249.309s完整四视角、DS175.807s实际sibling均同意此具体scope，无需新26轮；原53身份、两个真实ruler/各十对及26记录clock复核、无共享_tmp的独立Git fixture校验PASS。其余实际角色及廉价验证器补项/最终CI仍须闭合才合并；不冒称fresh定标。
- 原则与断言：搬迁批准依据不等于改写批准或补造历史；迁入文件SHA等于原件，三依据任一缺失FAIL。历史summary的PENDING_REVIEW/independent_review:null/frozen:false与原Claude专项负控P2原样保留，批准以当时FROZEN和两份原审查共同为据。来源SHA证明本次迁移，不宣称现有checker防后续内容篡改；#47仍未整单完成。

依据主仓`_tmp/20261016-debt-cleanup/{approval-portability/,reviews/approval-candidate-*-deepseek.md,reviews/approval-candidate-simplify-glm.md}`。同类其他FROZEN只有具体实测成立才新增缺陷，未知项不捏造。

R26后续廉价补项：driver原文已入仓，Calibration-Evidence当前指针同步；独立CI provenance guard校验四小原文与FROZEN声明SHA并跑变异负控。冻结checker与53注册/capture字节不变，CI规则层补身份门，不改定标或审批身份。该新具体实现仍交两家精确增量终审，不凭旧同意冒通过。

## R27 #120 子进程验收完整性（自决）

卡在哪：公开测试按继承环境跳worker可只验1/13；--exact零匹配也exit0，子进程输出与运行无界。仅测试装置受影响，可撤回，不动冻结/意向。采用独立ignored worker：公开父入口无env分支，共享test-only helper清环境、限时30s/总输出64KiB，要求恰1个libtest通过和唯一case receipt，父级集合与预注册ID对等。旧四处同类启动也迁移。原则：子进程退出0不代表目标被验证；执行数、案例身份、资源上限必须共同证明。零匹配/无回执/重复回执/失败/超时/超输出负控落入test_process。
