---
id: A8-UPLOAD-CLEANUP-ROBUSTNESS-001-R1
original: A8-UPLOAD-CLEANUP-ROBUSTNESS-001
---

## 类别

更正（原条目两处登记性错误 + 三条「已知限制」被 #1850 闭合或改写 + #2423 fix-r1
仲裁加固一条），不改原条目字节：

1. 原机器判据对 `plain_root_rejects_a_root_that_resolves_elsewhere` 四格的
   描述有误：原文「只有最后一格走得到等值比较」不准确——**真目录格同样走得到
   等值比较**（并满足它），走到等值比较且**被拒**的才是别名项目格；前两格
   （`chat-uploads` 根本身是符号链接，指向项目内或项目外）在两次
   `symlink_metadata` 即挡下，根本走不到 `canonicalize`。
2. 原机器判据引用的 L1231 / L1277 行号已随后续提交漂移。
3. 原「已知限制」登记的 `cancel` 裸删与 `acquire_attachment_lease` 失败回收
   两个无守卫删除点（r5 评审 P2/P3）：`cancel` 已由 #1850 过闸并配齐正控与
   负控；acquire 失败回收经处置**保留**原行为（见「Rust 行为」）。
4. 原「已知限制」登记的 `release` realpath 非 `NotFound` 分支「没有直接回归
   测试」：#1850 以 ELOOP 符号链接环补上直接回归（含 `get_or_insert_with`
   的 Err 缓存钉）。
5. #2423 评审（minimax concurrency#1/#2）仲裁（fix-r1）：#1850 给 cancel 的
   containment 比对只看**解析后**路径——`<id>` 换成指向同根另一 upload 的
   链接时确定性放行（无需竞态），删除落到别人的 files 上。cb8 实测两形态：
   良性预置别名被 `read_at` 身份校验挡在闸前（`UPLOAD_INTEGRITY_MISMATCH`）；
   伪造 `uploadId` 的记录经别名**可达**（删除了另一 upload 的 files）→ 按
   「丢数据一律 P0」当轮修：闸由 `inside_root` 改为 `canonical_home` 逐组件
   走查（拒绝根到 `files` 之间任何链接组件），证据
   `_tmp/20261108-cb8-2423-arb/`。

## TS 依据

原条目所引 cc7c4e6 `src/gateway/dialog/UploadStore.ts` 行为不变（cancel L205-213
in-gate 转换后删 `files`；`cleanupExpired` / `cleanupExpiredLeases` / TS 侧
release 均无穿链防护），本更正不重录。#1850 的两处新核对（探针存档
`_tmp/20261026-issue-1441/ts-probe/`，node v22.22.0，只读基线）：

- `p3-lease-lifecycle.mts/.out.txt`：cancel on completed 拒绝
  （UPLOAD_ALREADY_COMPLETED），记录保留清扫（retentionMs）与租约 TTL
  （leaseRetentionMs）分离——Rust `cancel` 的删除点闸门仍以此 TS 语义为界，
  守卫只是 Rust 单边加固（与原条目第 4 条同一性质，TS 无对应行为）。
- `p2-dual-path.mts/.out.txt`：allowedReadFiles 双拼写注册（#1446），不属本条
  更正范围，仅说明同批探针口径。

## Rust 行为

`src/pilotdeck-gateway/src/upload/mod.rs`（行号以本提交为准）：

- `cancel` L415-466：#1850 后删除点过闸——`plain_root(project, "chat-uploads")`
  三态（`Missing` 静默返回，等价于原 `remove_tree` 的 NotFound no-op；`Blocked`
  记 `[uploads] cancel skipped <路径>: uploads root is not a plain directory`
  后返回）+ 删除点比对。**fix-r1（#2423 仲裁）**：比对由
  `inside_root(&root_real, &dir)`（只看解析后路径）改为
  `canonical_home(&dir, &project, &root_real)` 逐组件走查——拒绝 project 到
  `files` 之间任何链接组件（`<id>` 换成指向同根另一 upload 的链接即「root 之内
  ≠ 本 upload 之内」的确定性别名，原比对放行、走查拒绝），并顺带重走
  `.tmp`/`chat-uploads`，`plain_root` 之后换根也能在此挡下；未命中记
  `[uploads] cancel skipped <路径>: outside the uploads root or a linked task dir`
  后返回（原消息为其前缀，既有断言不变）。
  状态转换已在 `mutate` 内持久，被拒的删除记日志跳过、**不是** cancel 错误：
  记录已是 cancelled，与清理半轮「宁可不删」同一取舍。窗口描述更正：原条目
  说「覆盖整次 mutate 的 metadata 重写」——实际窗口更宽，从 `get` 的记录读出
  （含 expire 判定的时钟读）起就覆盖；走查把删除侧的确定性窗口全部收进
  「走查→`remove_tree` 的几条指令」（`canonical_home` 自身登记的同一残余形态，
  见下「范围」）。
- `UploadArtifactLease::release` L1276-1311：行为**未变**（原条目登记即为现
  状），本更正只补判据：非 `NotFound` 的 `canonicalize` 失败（ELOOP 符号链接
  环是唯一无需权限位注入的确定性形态）继续以
  `UploadError::io(.., "realpath", ..)` 传播，且经 `get_or_insert_with` 缓存——
  根复原后重试 release 仍答同一 Err、不再执行删除。
- `acquire_attachment_lease` 失败回收 `remove_tree(&dir)` L721：**处置为保留**。
  `dir` 由本次 acquire 的 `make_dir` 刚建（随机名），删除的是自建残骸；
  `remove_tree` 的 `symlink_metadata` 不跟随最终组件（`dir` 本身被换成链接
  只会摘掉链接），中间组件（`<id>` 一层）被换链的窗口与原条目登记的指令级
  TOCTOU 同类；即便不删，泄漏的也只是无 lease.json 的 crash-orphan 租约目录，
  租约清扫按 TTL 兜底回收。加闸收益不抵「失败路径上再引入一次解析失败」的
  复杂度，登记为有意保留。
- `src/pilotdeck-gateway/src/native_process/services.rs` 两处租约释放日志
  （`let _ = writeln!` 宏起 L1617/L1651，消息行 L1619/L1653）由 `eprintln!`
  改 `writeln!` + 丢弃错误：与原条目
  `spawn_cleanup` 的理由相同——释放跑在 detached 的 `spawn_blocking` 任务里，
  stderr 不可写时 `eprintln!` 的 panic 会静默杀死剩余租约的释放循环。日志
  内容不变。

## 范围

- 本更正不改变原条目登记的任何清理半轮行为：逐条容错、孤儿回收、lease.json
  原子写、两根 `plain_root` 闸、轮首快照、计数语义、`expiresAt` 不可解析按
  过期，全部维持。
- 原条目「已知限制」中仍成立的条目不变：坏 metadata 任务目录的泄漏面、租约
  半轮全有全无（TS parity）、指令级 TOCTOU 残余、`release` 守卫非快照与
  「跳过的租约目录不自动回收」、`future` Err 分支无注入点。**fix-r1 起把
  cancel 的实例显式点名**（#2423 minimax concurrency#1）：`canonical_home` 走查
  与 `remove_tree` 之间的几条指令窗口（含走查后把 `<id>` 换成同根内链接的
  竞态别名）与原条目登记的四类删除点同形同威胁模型——能改
  `<project>/.tmp` 的进程本就对项目树有写权，std 纯路径 API 关不掉（需
  dirfd/`openat`），登记于此不另修。
- `cancel` 闸门只作用于 in-gate 转换后的 `files` 删除；`mutate` 自身的
  metadata 写（经 `task_dir` 直拼）在换链后仍会跟着写——那是写路径残差，
  与删除守卫分属两题，#1850 未扩范围，登记于此。

## 机器判据

`src/pilotdeck-gateway/src/upload/tests.rs`（行号以本提交为准，同时更正原条目
两处漂移行号）：

- 行号更正：`plain_root_rejects_a_root_that_resolves_elsewhere` 原 L1231 →
  现 L1970（四格措辞更正见「类别」第 1 条）；`store_swapping_tmp` 原 L1277 →
  现 L2030。
- 新增：`store_swapping_dir`（L2472，首个 `now` 回调把任意单个目录换成指向
  项目外的符号链接，三测试共用）、
  `upload_cancel_does_not_follow_a_root_swapped_mid_mutate`（L2504，换 `.tmp`，
  两拍时钟：expire 判定 + updatedAt，均先于删除）、
  `upload_cancel_does_not_follow_a_task_dir_swapped_mid_mutate`（L2550，换
  `<id>` 任务目录，只有 `inside_root` 能拒）、
  `upload_lease_cleanup_does_not_remove_an_id_dir_swapped_mid_round`（L2599，
  无上传记录故首拍即租约到期检查；清扫跳过链接 `<id>` 后 `names` 经链接读到
  空目标，守卫保住 `<id>` 符号链接本身）、
  `upload_lease_release_surfaces_and_caches_a_realpath_failure`（L2644，ELOOP
  环 → 首 release Err 且 code ELOOP；复原根后二次 release 仍同一 Err 且租约
  树还在，钉 `get_or_insert_with` 的 Err 缓存）、
  `upload_cancel_on_a_preplanted_alias_is_refused_at_the_identity_check`
  （L2690，fix-r1 仲裁正证：良性预置别名在 `read_at` 身份校验处被拒、victim
  files 完好）、`upload_cancel_does_not_delete_through_an_aliased_task_dir`
  （L2730，fix-r1 仲裁加固：伪造 `uploadId` 的记录经别名 cancel 不再删除另一
  upload 的 files，拒绝记 cancel skipped 日志）。
- 负控 `src/pilotdeck-gateway/tests/fixtures/upload-gateway/negative_controls.py`
  （fix-r1 后）六条：`cancel-remove-tree-ungated`（还原裸 `remove_tree`）、
  `cancel-delete-gate-dropped`（删整个删除点闸块，原
  `cancel-inside-root-dropped` 改名对齐新闸形）、
  `cancel-alias-walk-downgraded`（走查降级回 containment-only 比对，只有
  别名测试杀得掉这格）、`cleanup-lease-id-guard-dropped`（删空目录守卫的
  `inside_root` 项）、`release-realpath-swallowed`（非 NotFound 分支吞成
  `Ok`）、`release-err-not-cached`（Err 不缓存，复原后重跑删除）。整份文件
  54 锚点。
- 复算命令同原条目：
  `cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib upload`
  （fix-r1 后 78 passed / 3 ignored，含金样与 oracle 重放与两条别名用例；cb8
  实测 `test result: ok. 78 passed; 0 failed; 3 ignored`，探针记录
  `_tmp/20261108-cb8-2423-arb/post-fix-validation.out.txt`）；
  `python3 -I src/pilotdeck-gateway/tests/fixtures/upload-gateway/negative_controls.py --check`；
  `python3 -I scripts/gateway-parity/check_frozen.py --base 5ab9485394f0`。

## 批准记录

未批准，非已接受差异。原条目来源 #1646（评审块
`_tmp/20261106-1646-review/deepseek/` 与 `minimax/`，GLM 为实现方）；本更正来自
#1850（upload 加固批，GLM 为实现方），两家非作者评审与全量原地负控由协调者
续办。
