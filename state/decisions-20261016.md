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

## R30 外审只读必须落到执行器（自决）

- 卡在哪：DS在两份no-tools任务中仍调用19/31工具，并对未核原状态的通用/tmp路径先删除再重建；提示词未约束执行器。原内容未知，不能补造无损证明。
- 谁受影响：所有本机外审派发及其证据可信范围；意见文本与未经授权工具执行分列，后者不充验收。
- 能否撤回：本次CLI overlay、私有server与外审入口可撤回；历史删除不可追认，原日志和未知事实保留。
- 动不动尺子/意向：不改冻结、评分、行为范围或双非作者/实际三角色闸门，只落实既有只读授权边界。
- 选项与后果：只加prompt会重复越界；改全局配置会波及用户其它任务。采用单次调用硬禁工具、实际空registry或确定无tool/session执行器的文本入口，未核前停该通道，继续独立工作。
- 建议与执行：DS固定本次overlay SHA并核实际availableTools=[]、危险工具resolveExecution不存在；GLM绑定已安装binary/handler的无执行器调用链及本机前置控制。没有可用原账号受约束入口则保留未通过，不提取凭据、不借GPT补第二谱系。失败无实质final、字面工具调用都不算审阅通过。
- 原则与断言：**只读是执行权限，不是请求语气**。每次provider启动前核权限证明及输入SHA，漂移/启用工具/未知入口拒绝且不fallback；新入口仍须完整final、角色覆盖与问题表。模型若要求机器核验，由协调者在授权且有主的工作区执行有界控制，再交独立审阅，不削减实际角色覆盖。

唯一事故事实与原始索引见主仓`_tmp/20261016-debt-cleanup/reviews/tool-boundary-incident.md`；DS本地无provider启动诊断与GLM源码/route原件同目录。运行器修复候选不等于已通过双家终审，历史影响未知不记已修。

## R31 #52 与 #85A 组合输入加固的准备范围（AI代批，待人复核）

- 卡在哪：52补后缀后仍有85A实际依赖/构建输入漏绑定，不能分两次登记避开当前前置债；main七本地method与原透明proxy校准对象不同。
- 谁受影响：A2输入完整性及A4/A5.3验收；85C仍另有A7.7前置，不冒称整单关闭。
- 能否撤回：候选代码/专有输出可撤回，旧原件与分支保留；不改意向、八场景/十只读/阈值/G7/全序/归一化。冻结实际登记仍等独立两家与真实复算。
- 选项与后果：单独52再登记保留实际漏项；把七local产品塞给G2会改校准语义。建议一个输入绑定批次，在本地Git精确导出A1运输语义快照4d818489到自有artifact，新编译仅作尺子校准，不能当当前产品验证。
- 建议与执行：root已读Astra组合实施卡，只准备范围评审与源身份；两家范围意见未齐前不启动冻结实施/重登记。之后新fresh5TS+1A1、原controls和新输入变异、双家终审与实际重新登记依序完成，不能预写通过。
- 原则与断言：校准对象须由原尺子用途事先确定，不能按当前产品结果挑对象或放松G2；输入绑定变化后旧真实输出不能冒认新hash。每次运行前后核全部inputs，任一漂移拒绝而不重写expected。

唯一实施细节与证据主仓`_tmp/20261016-debt-cleanup/apparatus-plan/`；未知缓存/配置或空间不足只停实际构建，继续独立审查。

## R32 #56.2 先限定资源域补事实（AI代批，待人复核）

卡在哪：frame声明长度只有协议数值界而无操作性限额是源码事实，但没有真实入口资源/健康观测，不能当已证远程DoS或已证安全。与56.1字节丢失PR138分列。候选probe可撤回，不改生产cap/冻结或TS怪癖；任何超限新输出另按精确差异程序。

执行准备：复用wt-p2c新codex/frame-resource-probe，从main再ff到固定4a37测试模块，旧archive d148本地/remote同SHA且未知none/保留。worker只准备源码，不监听/负载/Cargo。未来实际单侧最多8MiB未齐masked input、声明64MiB，单侧20秒/总45秒；绝对RSS512MiB或健康起点+64MiB先到即终止，日志≤1MiB，健康小帧对照，独立端口/自有进程finally清理。执行前root再核资源、工具/源身份和所有权后分配窗口，禁止无限流或压垮宿主。OS buffer不可观察时标未知，不以sent字节代填。

原则与断言：资源探针的成功只证明预注册有界域，未挂起不能外推不可达；可达增长、当前健康、静态无cap与风险推断分别列证。父进程watchdog和硬额度不承担兼容性通过判据。

## R33 #139当前验收装置缺口按P1记账（自决）

卡在哪：139.1最初按真实errno证据跟进P2记账，后续DS明确R4和R2-F4/P2-F3会让测量假PASS；实际Rust复现静态facts/response自洽回填、已录错read/stat路径仍通过。新修复9e164859尚未独立收敛，旧P2不能继续代表当前缺口。

谁受影响：PR141、A4/#46验收可信性。账本可撤回；不改冻结、评分、Node原件或意向；不新增平行子项队列。选项是保留旧P2遗漏当前P1，或按现行P2-b取P1并保留历史P2。执行后者，确认度与in-review/fixed=false分列；P0下载事件139.2独立保持OPEN。

原则与断言：候选修复不能提前降低存量等级；影响测量结论的具体漏洞按P1记至独立闭合。JSON保留historicalSnapshotSeverity=P2、current severity=P1、DS原文SHA与9e候选证据SHA，33项旧账保真核查与新项字段断言分别验证。依据主仓_tmp/20261016-debt-cleanup/reviews/metadata-path-correction-deepseek.md及metadata/provenance-report.md。

## R34 动态Snapshot数值复算差异另列未修facet（AI代批，待人复核）

卡在哪：A-N1额外尝试将9e动态Node原件完整Snapshot RPC重放，collision-positive的JSON facts.birth_ms与Node createdAt raw均1792123157858.0955，Rust变1792123157858.0957。最初口头把facts也误说成0957，现以原始JSON/log精确更正，不能为了绿改期望。

谁受影响：动态facts重放可复现性及139.1；原9e静态三replay和真Native十+pair十并未执行此动态Snapshot路径，不能一概抹去或扩张其成功范围。可撤回分类，不改冻结/意向/原件或生产。

选项与后果：将本次A-N1纯validator通过泛化到完整RPC会掩盖差异；立即加舍入/改oracle会变尺子；当前采用新增P1候选facet，confirmation=candidate/fixed=false，先独立定级和影响范围，再精确修数值解码。极小实际Rust probe中serde_json::from_str<Value>变1ULP，标准str.parse<f64>和serde_json序列化该标准值保持原值；证据只定位已测链，不代替依赖选型或全链复验。

原则与断言：新暴露的确定性运行差异不因它来自额外验证而抹掉；已过范围与新失败范围分列。JSON用raw字符串保存两值、绑定原失败/诊断SHA、保持P1候选未修并明确nextAction，35项纯JSON核查包含此断言。原件见主仓_tmp/20261016-debt-cleanup/metadata/{success-api-report.md,success-api-extra-rpc-mismatch.json,success-api-float-probe.log}。

## R35 测试构建启用JSON正确舍入（自决）

卡在哪：139.1 dynamic Snapshot的原始十进制值经默认serde_json解析+1ULP，实测相对Node与标准Rust假FAIL；两独立家已判P1，尚未证产品路径。谁受影响：测试装置及其Cargo测试图，普通产品build范围须另证；可撤回，不改oracle/容差/冻结/意向。

选项与后果：workspace普通依赖开feature会改变产品图；手写定点解析引入更多语义；gateway dev-dependency开启既有float_roundtrip在resolver3下限于含dev的构建，但test图同版本serde_json统一feature，绝不能称只改Snapshot函数。缓存1.0.151 archive与lock checksum及de.rs/lexical源已独立逐字核同，feature无新crate。采用gateway dev-dep，普通build与test分别看实际compiler fingerprint/消息，--all-targets不能代普通build。普通实现改动已有用户授权，无需新人批。

原则与断言：修解码、保原件；真实数值位级回归和dynamic十原件完整RPC都须过，不能只validator变绿。SOURCE_FILES13→14绑定gateway manifest并更新精确负控；driver增加dynamic正式阶段并核恰1test/十唯一回执/原件前后SHA。ok:null/missing/nonbool两门廉价缺口同批修，不增新队列。独立方案见主仓_tmp/20261016-debt-cleanup/metadata/numeric-feature-scope.md；新candidate双家/CI未齐不核销139。

## R36 只清可再生的旧incremental缓存（自决）

卡在哪：五树与44.58GiB/46GiB使新feature/fresh证据窗口余量不足；不能删旧工作、可执行产物或原始记录腾空间。谁受影响：已交回root且无占用的113400-metadata编译加速缓存；作者明确本轮仅用120000。

选项与后果：整删target损毁证据；放宽46上限违规则；只清debug/incremental可从保留Git输入/依赖源重新生成，影响仅重编速度。采用第三项。实际cache路径/非symlink/设备inode、Cargo target锁先核；所有非cache2510路径及文件SHA完整前后比，binary/deps/raw/source一律保留，任一漂移停止并留证，不扩到其它target。

原则与断言：缓存与实验产物分列；只在明确所有权、无使用且可恢复时丢可再生缓存，不能以清理为由删除证据。限定计划及执行原件主仓_tmp/20261016-debt-cleanup/metadata/owned-incremental-prune-{plan,result}.json；这不是永久自动清理授权。新fresh窗口仍须完整du+明确预留小于46上限，独立target/真正Cargo产物绑定不降低。

R36同片扩展：113400仅incremental清理后2510非cache路径/字节全同；新feature实际增长后总44.843GiB，新fresh保守1.4GiB仍不足。已明确归root、作者本轮从未使用的_build/20261016-104320 incremental为470228KiB，同样只清可恢复cache、保binary/deps/raw、目标锁和非cacheSHA全核。清后重新完整du，再以1.5GiB预留核新target，不扩为常设自动清理。

R36续：104320仅incremental清理后6862非cache路径/字节全同；du因硬链接/统计范围等不保证按局部cache du等量下降，实际完整du仍44.58GiB，1.5GiB保守fresh预留不够。再将root独占、无构建者的112000-order同一可恢复incremental缓存（局部365592KiB）纳入本次精确清单，保留已录制的test executable/所有deps与原件；同样锁/非cacheSHA核后重新完整du再发窗口，不按估算批准。

## R37 逐子项主次归类与等级轴分离（AI代批，待人复核）

卡在哪：原31单55子项已有两家原始意见，类型抽取/范围限定常被混为全部待归类；一行同时含现缺陷、缺证和未来任务，不能靠选一个类型抹去其余债。谁受影响：清账排序与原始记录可追溯性。此归类可撤回，不动冻结/意向/等级/修复状态/期限，也不增加审批门。

判据：当前实现或装置违反现行合同的候选主记真实缺陷；主要缺可复现证据、实效或威胁模型外加固的主记验收债务；已明确将来新增能力且当前合同未被证明违反的主记阶段任务。混合项列次类和具体未决事实，不能把候选叫已证。52.2仅A9措辞P3，依现行P3规则主记已知留白、不在本轮范围；这是P3例外，不强塞三类或增阻塞。

执行：采纳主仓taxonomy/current-disposition-proposal.json的54个主次类型及逐项理由，52.2按上列P3例外。32项初始主类一致、22项协调者主次取舍和1项P3例外分列；其中85.1、98.1/.2的真实缺陷次类和独立异议、55.2/56.2的候选/unknown全保。所谓一致只指相应初始类型输入，不替后来等级修订或双家终审。proposal中旧13修订/单家R5/grade/state信息不得回灌，实际等级始终从最新独立原件重新计算。

原则与断言：分类、等级、事实确认、交付状态和真人实效是分开的轴；改主次类不降低一个等级、不改一个fixed或到期节点、不清任何原始限定。协调者明确归类不等于两家已同意；原类型、分歧、PARTIAL和原needsCanonicalAdjudication逐字留存，增独立的类型处置状态与事实待核，不以抽取flag继续冒充尚未做归类。投影必须绑定原proposal/SHA及所引原文SHA，单字段变异拒绝；PR143仍过实际非作者四视角/三角色及双家终审。

## R38 新观察器复验的两个精确缓存目标（R36本片延续，自决）

R36本片第四个精确目标：124315-metadata实际fresh已结束并交回root，12:57进程清单无Cargo/rustc占用，作者只用120000并已冻结；其incremental局部253116KiB。新8460123观察器代际修复改变record源码，需要新真实录制。仍只清124315/debug/incremental，保0c11实跑binary/deps/fingerprint及537原件，锁/路径/设备inode/全部非cache SHA前后同才成立。清后用实际完整du核1GiB fresh预留，容量未满足则仅继续独立审阅，不降低driver fresh门、不动生产/冻结。旧fresh体积736432KiB为预留依据，不拿它冒新HEAD产物。

R36第五个精确目标：124315清理后1898个非cache路径/字节全同，实际总45.124GiB仍不足1GiB预留，局部du不能当全局腾出量。作者已明确120000/124315均无占用并交回root，当前无Cargo/rustc；仅纳入120000/debug/incremental（局部847876KiB），保其所有当前与历史测试binary/deps/fingerprint/回归原件。执行相同路径/锁/设备inode及非cache全SHA后重新完整du；满足才发新独立target窗口，不将该清单扩成其它目录删除授权。
