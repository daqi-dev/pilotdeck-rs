# 2026-10-16 资源决策

## R44 为65新精确产物释放两个自有可恢复缓存（自决）

卡在哪：五树与46GiB硬上限，余约648MiB不足全新构建；复用112000会覆盖旧379证据。谁受影响：65的A0/A1/A2实验，不动产品/冻结。可恢复性：仅Rust incremental缓存可由保留Git源码与依赖源重建；全部旧binary/deps/build/fingerprint/原件保留。两个候选是_build/20261016-101136/debug/incremental（约376.77MiB）与_build/20261016-130100-metadata/debug/incremental（约245.75MiB），均为root已归还的metadata专属目标；101136来源见revision-pair/git/tail-handoff-index，130100为846观察器fresh530来源。当前无Cargo/rustc使用；不能仅凭这句跳过执行时核验。

选择：不提高46上限，不删旧target或binary；仅在精确路径非软链接、uid/设备/inode、Cargo锁与进程、完整非cacheSHA及无证据索引引用核清后删除这两处可恢复cache。路径及前后清单/执行结果写主仓_tmp/20261016-debt-cleanup/order-architecture新自有预检目录。任一未知所有权/并发/字节漂移即停该动作，不扩到其他目录。R36/R38既有五目标不是永久授权，本卡仅增加这两个精确目标。

执行后完整du若留不出至少1.20GiB（1GiB执行窗外保留至少204.8MiB），不开构建窗；若足够，仅给全新_build/实际时间戳target+新实验输出合计1GiB硬窗，剩余作并行小证据余量。外部监督新增预算及总46GiB，触限先停/回收本轮自有进程，保现场，不删实验原件。原绝对1.99工具、原HOME/cache、四wrapper显式空、offline/locked/jobs2不变。仅编译当前test-only护栏/65探针及有界实证；自然未见与未跑变异不改写为通过。

原则与断言：可再生缓存与证据产物分别核验；不能靠覆盖旧精确产物取得新HEAD的运行身份。全部非cache文件路径/字节SHA前后一致、完整du预算、精确新产物来源与自有进程回收是开跑前后硬门。
