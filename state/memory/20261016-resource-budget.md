---
name: resource-budget
description: 清可再生cache前按最后硬链接的allocated块估算，局部du不代表全局可回收量。
type: feedback
---

为什么：R36/R38多次出现局部incremental du与完整du释放不等，R44再次实际确认1273个已删cache inode仍在保留noncache路径有链接，清两处后余0.975994GiB仍不够1.20窗；所有非cache证据SHA保留。不能为追预算删保留deps/bin或放宽46上限。原件在主仓_tmp/20261016-debt-cleanup/order-architecture/acquisition-run-144502/。

怎么用：先证明精确cache的持有人/可再生性、无锁/进程/保留要求与sealed内容引用，保存完整非cache SHA。按dev/inode去重、核nlink；只有候选内路径数覆盖该inode全部链接才计可释放，外链一律计零并定位其保留路径。估算用st_blocks的allocated字节，不用逻辑size或各目录du相加。独立临时新target及日志/原件合并预算，留并行文本余量；清后全量du复核满足实际窗口才启动，不够只停相关构建、转独立工作。删除必须新精确决策卡，不把R36/R38/R44/R45当永久清理授权；非cache前后路径/字节SHA断言始终必做。


目录nlink须目标卷实测：R57在APFS因只扣子目录假设停，实际每个直接file/dir entry都减1。双次真实控制主仓_tmp/20261016-debt-cleanup/apfs-directory-links-odyyiuk1/index.json（SHAc2968f4e…）；新snapshot扣旧实际unlink、新入口按全部entry精确计数，保inode/内容/blocks门，不泛用Linux经验。采样配额不称内核硬quota：helper首轮8MiB实际超110592B并停，独立16MiB窗提前2MiB门后实际26通过，两结论分别保留。

外部loader临时缓存也算本轮输出：R61六caller每格tsx生成约6.6MiB allocated，16MiB早停仍实际超380928B，失败/EPERM原件保留。先用已安装tsx源码与typed42独立控制证明TSX_DISABLE_CACHE wrapper真正无磁盘cache，再按新工具身份开R67，六格实际约0.7MiB；不以改预算或删旧证据充新前提。不可把历史cache硬链接去重当无害：tsx可按cache key覆写/TTL删除，未来写会污染共享sealed别名。只读理论节省不等于可执行回收。
