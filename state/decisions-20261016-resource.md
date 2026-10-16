# 2026-10-16 资源决策

## R44 为65新精确产物释放两个自有可恢复缓存（自决）

卡在哪：五树与46GiB硬上限，余约648MiB不足全新构建；复用112000会覆盖旧379证据。谁受影响：65的A0/A1/A2实验，不动产品/冻结。可恢复性：仅Rust incremental缓存可由保留Git源码与依赖源重建；全部旧binary/deps/build/fingerprint/原件保留。两个候选是_build/20261016-101136/debug/incremental（约376.77MiB）与_build/20261016-130100-metadata/debug/incremental（约245.75MiB），均为root已归还的metadata专属目标；101136来源见revision-pair/git/tail-handoff-index，130100为846观察器fresh530来源。当前无Cargo/rustc使用；不能仅凭这句跳过执行时核验。

选择：不提高46上限，不删旧target或binary；仅在精确路径非软链接、uid/设备/inode、Cargo锁与进程、完整非cacheSHA及无证据索引引用核清后删除这两处可恢复cache。路径及前后清单/执行结果写主仓_tmp/20261016-debt-cleanup/order-architecture新自有预检目录。任一未知所有权/并发/字节漂移即停该动作，不扩到其他目录。R36/R38既有五目标不是永久授权，本卡仅增加这两个精确目标。

执行后完整du若留不出至少1.20GiB（1GiB执行窗外保留至少204.8MiB），不开构建窗；若足够，仅给全新_build/实际时间戳target+新实验输出合计1GiB硬窗，剩余作并行小证据余量。外部监督新增预算及总46GiB，触限先停/回收本轮自有进程，保现场，不删实验原件。原绝对1.99工具、原HOME/cache、四wrapper显式空、offline/locked/jobs2不变。仅编译当前test-only护栏/65探针及有界实证；自然未见与未跑变异不改写为通过。

原则与断言：可再生缓存与证据产物分别核验；不能靠覆盖旧精确产物取得新HEAD的运行身份。全部非cache文件路径/字节SHA前后一致、完整du预算、精确新产物来源与自有进程回收是开跑前后硬门。

## R45 用最后硬链接可释放块预算补083300精确缓存（自决）

R44两目标执行后真实余量仅0.975994GiB，低于1.20，按卡停在Cargo/Node前；4920非cache与旧112000全2153文件SHA一致。1273已删inode仍由保留noncache引用，局部du相加不能代表实际回收。

新候选仅_build/20261016-083300/debug/incremental：原upgrade/p2-delta.md:3/:29的ff75ca146来源/归还root及原task专属target为持有人依据，无活跃构建。只读resource-candidate-prep/index.json SHAa6853d4c…记录1598 distinct inode，81个所有链接都在候选内，可释放allocated块343207936B=327.3086MiB；其余1517外链inode计零并定位到保留路径。广扫6848潜在sealed清单仅Git incrementalBase词命中，真实cache引用未见，原bin/binding必须保留。

采用单处cache清理，前提沿R44：执行时重核uid/dev/inode/nonlink、Cargo锁/进程、sealed引用与完整非cache前后SHA；不删外链保留物，不扩其他目录，不动旧迁移/用户产品或原件。只允许这处已核可再生cache的路径删除，实际释放由完整du重算。达到1.20GiB后仍仅原1GiB新target+实验证据窗，不提高46上限；未达继续停该构建并转独立工作。

原则与断言：删掉一个cache链接不等于释放inode。预算按候选内全部nlink均可删除的distinct inode已分配块估算，外链计零，最终以完整du复核；非cache路径/内容SHA始终不变。这条同类经验写入memory，不再靠连续清cache猜释放量。


## R48 为M1–M4新产物窗释放083100与092037精确缓存（自决）

卡在哪：R45实验证据封存后完整du45.272274GiB/46，余0.727726GiB不足1.20新窗门。谁受影响：#65同一装置的M1–M4实际路径负控；不动产品/冻结语义。可恢复性：仅原源码可重建的incremental缓存，保留所有noncache、旧112000与最新145841/c990精确产物及原失败；两个target均有明确归还root锚点。

唯一授权路径为_build/20261016-083100/debug/incremental（dev16777230/inode42367862/uid502）和_build/20261016-092037/debug/incremental（同dev/uid，inode42423766）。只读resource-next-window-prep/index.json SHAe66196a9728c082e8dbaa01851f02c45e29c50af79aa67e016982915e4dc0aef绑定持有人与封存引用扫描；两处union last-link allocated573763584B=547.183594MiB，1789外链inode计零、有noncache路径见证。预计释放0.534359GiB，预测不等于已释放。

执行者ledger_fidelity先于任何删除重核上述精确目录身份/父链/所有条目非软链接、本uid、dev/inode、nlink/blocks、所有构建进程与实际锁、最新封存引用及保留要求；完整两target noncache与旧112000全target路径/size/SHA先存新唯一owned目录，并核原R45封存index。仅清这两处已核可恢复缓存，目录漂移/未知引用/新写入者/锁/异常即停相关动作，不补删其他目录。保留真实删除清单、全部非cache及原件SHA逐项后核、完整du与回收块。只读计划不替执行核验；已有R44/R45不是永久授权。

清后先回root实际du，不启动Cargo/Node。若实际余量达到1.20GiB且总量不超46，再由root给一次全新纯时间戳_build/YYYYMMDD-HHMMSS与新实验输出合计1GiB硬窗；600s总上限、每child60s、每流1MiB、终于回收owned PGID，四wrapper显式空、原绝对Rust1.99与原HOME/cache、offline/locked/jobs2、incremental0。源码9df46654只准备完，后续需真实新artifact与A1/A2前后健康+四mutant八格；不重跑A0，不改原validator，不以setup/EOF/超时/0test冒负控成功。新oracle若旧identity门不能复用须显式报root，不暗加Node任务。

原则与断言：资源释放按真实last-link allocated与完整du核；缓存清理授权必须落到精确对象和当次门，不能泛化为清target或实验原件。非cache/封存SHA守恒、1.20开窗门、1GiB执行窗与46总上限任何一项不满足就只停相关动作，继续独立工作。


## R53 只释放093057自有可恢复缓存（自决）

卡在哪：#65共用验收门候选17334d2已静态复核，首次精确产物仍缺空间；覆盖旧产物会丢身份。R52单位facet经GLM157.433s/DS89.472s独立确认，unit-scope-gate.json SHA0318f1523bc5cfcaa861443ec494dc1f696f86ec42d13de968248ea45921f900（主仓_tmp/20261016-debt-cleanup/reviews），只闭该单位，不是PR146或#65全通过。按已说明du1024/46GiB核当前资源，不改变人定46数值。

唯一允许清理的对象是_build/20261016-093057/debug/incremental（uid502/dev16777230/inode42432864；target父42432855/debug父42432857）。原holder debt_catalog_impl的archive-implementation/handoff-index.json与source94030771明确已结束/target保留；本卡仅把可由Git源码与原依赖重建的incremental缓存释出，全部noncache/binary/deps/build/fingerprint与原始实验证据继续保留，不把“停止占用”冒成整个target可删。当前root持有该归还对象，无人手cache产物。

执行者ledger_fidelity仅对这一根作临界重核与精确unlink。只读plan resource-ready-admission-prep-161835/index6e22424b…、前检cache-ready-admission-preflight-1645/index add24c1f6fc3ba2eb20efdff7ca6439395fbc027598b43ddfe3c2749a180c84d都在主仓_tmp/20261016-debt-cleanup/order-architecture/。1897cache条目、6029 noncache与旧1120002356条目完整身份/字节SHA已准备；last-link allocated预测494833664B，1373外链计零并有保留路径见证，预测不当已释放。原guard只是readonly、实际execute已拒且无删除primitive，本卡不改变或执行旧guard原件，另建唯一执行入口/目录。

删除前必须再次核卡与unit gate全部精确SHA、根/父链非symlink及uid/dev/inode/各条目mode/nlink/blocks/内容SHA、所有target锁和Cargo/rustc/FD、最新sealed引用与保留要求；新未知项/漂移/写者/锁任何一项不清即停相关动作，不补清其他根。临界锁仅允许本PID和已登记精确FD路径，不能泛放本PID。保留按inode实际删链接清单，noncache前后路径/身份/size/SHA守恒，nlink只许减本次实际unlink次数。旧112000全部、R45 745/R48 134/R51 63共942原件及c990/3ed确切产物逐SHA前后核；其余旧工作/并行研究/源文件/人手材料一律不动。

清后完整du实数交root，不Cargo/Node/新target。只有root据新HEAD clean/资源现况另发一次更小0.90GiB窗口、外留0.20GiB（实际门至少1181116007B）才开构建；原1GiB/1.20门不修改，未够门只停本实验。此处只批准可恢复cache清理，不预批准该新窗或任何评分/阶段完成。

原则与断言：缓存授权精确到当次对象及临界条件，保留报告不推导整个目录可删；全部不可再生证据继续保留。实际释放由全局du证明，后续运行必须有独立新窗，清理成功不能替编译/验收通过。


## R54 ReadyAdmission首次精确产物与四正常两反控（AI代批，待人复核）

R53仅清093057 cache完成，index001e1c1608261fd824a238245b013bc94d5298b127cf9c621c7ad2405ba42527的15原件已root逐SHA/size核；6029noncache/旧1120002356与942原件/c990/3ed守恒。2026-10-16T17:03:11.626375+08:00完整du总48153473024B，46GiB口径余1238650880B≥1181116007B；原始值在本次root-preflight.json，清理预测不替代此实数。

给ledger_fidelity独占wt-a4m3运行权（不写源），source17334d2d93f6a1b98bde641a8e2ce6475c18fabf clean；原两P1共用门修复仅root/Astra静态核同，GLM/DS终审仍待。本次唯一新target为_build/20261016-170311，唯一输出_tmp/20261016-debt-cleanup/order-architecture/ready-admission-r54-run-vifvft2v，task/execution-spec来自order-architecture/ready-admission-common-audit-20261016-163800/index f685d4da5ee3eac8356dd2132a7e660a4eae4e105af8af38ed24af5d861c5987。单位gate0318f152前提保持，仅本次显式override源树205旧单位简写，不改源码HEAD或倒填旧实验。

这是独立更小窗：新target+本输出逻辑字节合计≤966367641B（向下取0.90GiB），全局外留≥214748365B（向上取0.20GiB），当前门1181116007B；既有1GiB/1.20窗不变。持续监督总46GiB/此窗/余量与输出量，触限仅终止本轮自有进程组并wait/reap、保所有失败原件，不删旧物补预算。600s总限；Cargo独立≤300s（编译不是实验格，不套60s测试限），每个精确test≤60s/每流1MiB，Node host内部15s只作失败护栏；首次build失败不选旧artifact、健康格失败不盲跑后续、不重试固定矩阵。

按execution-spec原绝对Rust1.99/原HOME-cache、四wrapper显式空、offline/locked/jobs2、incremental0；Cargo JSON中实际唯一lib test executable必须在此新target内，存完整原始编译输出、source/tool/manifest/prereg前后SHA、artifact路径/大小/SHA，不复用旧c990/3ed。只一次D1+/D1−/A+/A−及X1/X2，反控须有真正hit/W0/C0与同一audit.json具体R2_REQUIRED/COMMIT_SEQUENCE_VIOLATION拒，setup/timeout/0test不是成功；四健康必须两共用audit都Ok且完整动态wire一致。仅六格，没有额外恢复格、没有旧A0/全量53/自然窗口重跑。所有自有PGID与host都须finally回收，旧942原件/112000/精确二进制/source/tools前后核。

原则与断言：显式接纳子域实验不代表默认产品修复，成功也只记录该子域与装置，#65.1默认反例/65.2自然窗/P3/A5.4/真人实效继续未完成；失败保原始具体断言并转独立工作，不换期望让结果变绿。资源判断可撤回、判分语义与冻结标准均不动。


## R55 修复监督竞态后的新精确产物窗（AI代批，待人复核）

R54首次构建由监督器size()在Rust正常临时lib.rmeta消失时触发FileNotFoundError中断；没有完整artifact，六格全部NOT_RUN，不能算Rust类型错误或负控通过。原执行器未持久化Cargo实际退出码，保持未知；自有PGID32724已TERM/KILL/wait且无残留。R54的12原件index1ea57c7a9e42b040e544b3353a9dece1a141046019f2c1f181b8a457b2158b77及partial target20261016-170311原样保留，不补写旧receipt、不复用partial产物。旧R54原件与卡都不覆盖。

本次唯一输出_tmp/20261016-debt-cleanup/order-architecture/ready-admission-r55-prep-20261016-172334，immutable预登记index9b42ba28f27da9e0a84bf64dac27b719e25f991c4ab9cf75ebfe030581b17668（9件）；controller SHA f1bf40256e5f62ce02c8c017be2fdb2e4a72eb9459f9bf4bd6c26b2c0fe3af4b。监督helper a7e9cf4fea3cbcc949467cc7dff441ccf3984b953b814b57ff9edb5dee7c0326在真实unlink六控制过，index70ef9895f55bf59f0df0237433595e01f77d64bf935299eb9728ab57c35fdfd0；写期只容忍ENOENT，静态后核严格，未知权限/设备/软链接不吞错；全部异常finally必须持久化实际wait退出码/回收/失败。旧control的Path.is_file两stat假设不成立已另存失败，改用实际unlink的准确注入，未抹历史。

ledger_fidelity独占a4运行权、源只读clean9f8a45251138d73ae14d7942c94fe8085a604178；新增capture e6和容量checked_add 890只完成静态/源码层，仍待真实运行与独立审阅。890→9f8严格仅同步mainb888的CLAUDE与引用R52的followup两文件，Rust/Node/prereg所有blob不变；五树现共同规则b390，rules-sync-euy79jd5/result.json SHA4ba09d8878b720ebc0e0cf409c1902bdaa470a6f04a78c826f20bbb9e630bd97。新artifact环境BUILD_SOURCE_HEAD必须9f8，不倒填890/173。

唯一新target为_build/20261016-172909。这是独立更小窗，target+本输出逻辑字节≤858993459B（向下0.80GiB），外留≥214748365B，开窗实际free须≥1073741825B；原0.90/1.10门不降低、不挪作本次通过。最近完整du22886960+24222248KiB，free1152294912B仅作现场参考，controller在mkdir或Cargo/Node前再实测；不足即停，不删旧物、不再缩窗凑门。总46GiB、600s总限、Cargo300s、每test60s、每流1MiB，触限仅回收本轮ownedPGID保失败。并行CLI helper26另获至多8MiB/120s/child60s独立验证权，共用全局46GiB门、不能借用本target或声称PR138全过；中央小评审证据仍从外留预算内计。

以原绝对Rust1.99/原HOME-cache、四wrapper显式空、offline/locked/jobs2/incremental0建新唯一test artifact，绑定Cargo JSON/source/tools/TS1000与三个config/prereg/sourceHEAD前后SHA。先实际两辅助guard各一次：ready_finish_rejects_existing_stderr_file_after_successful_child、bounded_host_log_rejects_first_excess_byte；再真实六格D1+/D1-/A+/A-/X1/X2各一次，最后显式新增X2后的D1+-restore第七格（case仍D1+、新output/runID、同artifact/common gate），不改旧六格预注册或旧原件。健康双audit Ok/全部动态wire；X1/X2须实际hit/W0/C0与共同R2_REQUIRED/COMMIT_SEQUENCE_VIOLATION拒。每格包含负控均核两个host实际reap/完整stderr join/bytes与事件，实际wire总字节≤既有1MiB；0test/setup/timeout/未知失败不算负控成功、不盲跑后续、不重试自然窗/全量53。

执行前后核旧942+R53的15+R54的12共969原件、c990/3ed、112000全2356条目和R54partial target全路径/身份/内容SHA；当前源/工具/新prereg/authority card SHA与Git HEAD保持不变。root只在此卡提交后新增create_new authority.json，记录该提交、卡SHA、target/output/index/最终source与预算，ledger仅一次--execute；原index不改，运行后新execution-index另封。失败保全、只停相关动作；#65.1默认反例/65.2自然窗口/P3/A5.4/冻结语义和真人实效继续未完成。

原则与断言：监督器自身读目录的竞态不能冒成被测程序失败或通过；先证明控制注入真实，再以新产物重算。每次资源窗与恢复格必须执行前显式登记，原失败和原门不重写。以上为2026-10-16T17:29:09.540948+08:00的可撤回执行判断，不替代两家终审或实效验收。
