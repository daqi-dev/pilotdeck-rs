# macmini-01 pdrs-host 卷迁移：外接 4 TB 盘已弹出，映像暂存内置盘（2026-11-12）

**事件**：人 15:3x 提出换 SSD 加速编译。核查：外接 4 TB USB 机械盘（disk5「Untitled」）**不是纯冷存储**——macmini 的 pdrs-host 工作卷（`/Volumes/pdrs-host`，宿主链/全量对拍用）是挂载自该盘上的 `pdrs-host.sparsebundle`（52 GiB 实占，卷内 1.2 Ti APFS、已用 51 GiB）。盘上另有 ~1.9 TB 视频等非项目文件（未动，随旧盘留给人）。

**已做**（2026-11-12 16:0x，协调者执行）：
1. 确认无进程占用（lsof=0、无 cargo/rustc/parity 进程）→ `hdiutil detach` 卸下 pdrs-host 映像；
2. `cp -R` 把 `pdrs-host.sparsebundle`（52 GiB / 6598 文件 / 55,298,290,664 字节）拷到 macmini 内置盘 `/Users/macmini-01/pdrs-host.sparsebundle.copy`；核验：文件数、总字节数、Info.plist `cmp`、3 个随机 band sha256 全部一致；
3. `diskutil unmount /Volumes/Untitled` + `diskutil eject disk5`——**盘已弹出，等人拔下换 SSD**。

**待办（新 SSD 接上后）**：
1. 新盘格式化（APFS 或保持外置方案均可，SSD）；
2. 把 `/Users/macmini-01/pdrs-host.sparsebundle.copy` 拷到新盘（建议路径沿用一个稳定挂载点名，改 ops 侧引用前先 `grep -rn pdrs-host ~/ws/pdrs-ops/`）；
3. `hdiutil attach <新盘>/pdrs-host.sparsebundle` 恢复 `/Volumes/pdrs-host`；
4. 期间 macmini 不派宿主链/全量对拍单（卷不在）；cb8 不受影响（cb8 经 ProxyJump macmini-01 只走 SSH，与该盘无关——已核实 ssh 配置）；
5. 内置盘副本（52 GiB）在迁移确认后删除前，先确认新盘上有一致副本（铁律 8：删前确认另一处还在）。

**教训**：memory 旧口径「外接 4 TB 只作冷存储」不准确——pdrs-host 工作卷一直挂在这块盘上；判断「盘是否在用」要 `hdiutil info` 查映像挂载链，不能只看 mount 顶层与 ps。

---

## 收尾（2026-11-13 13:3x，协调者执行，人令闭环）

人 13:0x 授权「macmini-01 的硬盘上的数据可以全部清掉然后格式化，之后统一使用」。实际接上的新盘是 **disk5（HDD027_7TB，7.7 TB，原 exFAT，941 GiB 旧数据）**（不是原计划的 SSD）：

1. `hdiutil detach /Volumes/pdrs-host`（前一晚轮次已把 sparsebundle 拷入该盘并 attach，detach 时确认零占用）；
2. `diskutil eraseDisk APFS pdrs-ssd GPT disk5`——旧 941 GiB 数据全清（人令授权，13:09–13:27）；
3. 内置盘正本副本先核验（55,298,290,664 字节与本文基准逐位一致）→ `cp -R` 到 `/Volumes/pdrs-ssd/pdrs-host.sparsebundle`（13:28–13:31）→ 核验全过：文件数 6598/6598、总字节两侧一致、Info.plist cmp 一致、bands/0 与 bands/50 sha256 双侧且与基准吻合（日志 `macmini-01:~/pdrs-copy-verify.log`，脚本 `~/pdrs-copy-verify.sh`）；
4. `hdiutil attach` 恢复 `/Volumes/pdrs-host`（挂载点名不变，ops 侧引用零改动；卷内 build/pdrs-cold 完整，51 Gi 用量）；
5. 核验一致后删内置副本（铁律 8 满足），内盘释放 52 GiB（112→164 GiB avail）。

**统一使用布局（协调者定）**：`/Volumes/pdrs-host` 宿主链/对拍工作卷照旧；`/Volumes/pdrs-ssd`（7.0 Ti 可用）为映像本体所在 + 剩余 ~6.9 Ti 作冷存储/归档区（对拍产物、树回收前证据），编译缓存暂不迁（每树 target dir 规则不变）。磁盘欠压「待办 1-5」全部销账。disk4「飞书 Feishu」为另一块用户盘，未动。
