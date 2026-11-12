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
