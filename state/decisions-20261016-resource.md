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
