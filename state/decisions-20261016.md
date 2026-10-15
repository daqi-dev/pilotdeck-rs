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
