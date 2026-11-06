---
id: A8-UPLOAD-CLEANUP-ROBUSTNESS-001
---

## 类别

有意改进（Rust 比 TS 更健壮的单边偏离，共四条，均限于 upload 清理路径）：

1. `cleanup_expired` 逐条容错：单条记录读 / 判到期 / 删除失败只记日志跳过，不再中止整个 project 的一轮清理与后续 `cleanup_expired_leases`。
2. 无 `metadata.json` 的孤儿任务目录按 mtime 超过上传 retention 后回收（TS 永不回收）。
3. `lease.json` 原子写（temp + rename），读者永远不会观察到 0 字节 / 半截的 `lease.json`。
4. 两棵清理根（`chat-uploads` / `chat-upload-leases`）都加「必须解析回自身」闸
   （`plain_root` 要求 `canonicalize` 结果与 `<project>/.tmp/<leaf>` 字面相同，
   指向项目内其他目录的符号链接同样挡掉）+ 轮首根快照，两半轮与 `release` 的
   **每一处**租约/记录递归删除前都过 `inside_root`（现解析条目 × 轮首快照），
   清理绝不穿过符号链接根删到项目外（TS 无此防护）。

## TS 依据

cc7c4e6 `src/gateway/dialog/UploadStore.ts`（node v22.22.0 实测，证据存档在主仓
`_tmp/20261106-1646-receive/delivery/segment-1.md` 与
`_tmp/20261106-1646-receive/delivery/ts-repro/repro.mts`，三格各 3 次输出一致）：

- `cleanupExpired` L229-242：`listInProject` reject / `rm` 抛错 → 整个 `cleanupExpired`
  reject，后续记录与 L239 `cleanupExpiredLeases` 全跳过。实测：一条 `metadata.json`
  损坏的到期上传使整轮 reject，同 project 的好记录保留、过期租约不清。
- `readAt` L370-390 / `listInProject` L392-396：缺 `metadata.json` 的目录被过滤，永不入列。
  实测：`.tmp/chat-uploads/<id>/files/x`（无 metadata）推进 30 天后 `cleanupExpired()` 仍
  返回 0，目录与文件字节永久泄漏（单文件上限 1 GiB）。
- `writeLeaseExpiry` L404-413：`open(path, "wx", 0o600)` + `writeFile` + `sync`，非原子；
  `readLeaseExpiry` L431-438 catch 一切返回 undefined，`cleanupExpiredLeases` L423-424 当过期删除。
  实测：0 字节 `lease.json`（活租约的写入窗口）+ 未来 `expiresAt`，`cleanupExpired()` 后租约目录被删。

## Rust 行为

`src/pilotdeck-gateway/src/upload/mod.rs`（本条引入）：

- `cleanup_expired` L419-429：对外签名与返回值语义不变（`Ok(n)` = 删除的到期**记录**数；
  孤儿回收不计数），记录半轮改走 `sweep_expired_records`，`cleanup_expired_leases` 照常执行。
- `sweep_expired_records` L434-511：逐条容错。`read_at` Err / `expiresAt` 解析 Err /
  `remove_tree` Err → `skip_record`（`writeln!` 日志带记录路径，写失败丢弃）后继续。`list_in_project`
  的严格语义保留给 `create` / `find_idempotent` / `mutate` 等调用方，未放宽。
- `plain_root` L1285-1305：`.tmp/<leaf>` 的三态闸（`Plain` / `Missing` / `Blocked`）。
  两棵根都过它：记录半轮 `sweep_expired_records` L440 用 `chat-uploads`，
  租约半轮 `cleanup_expired_leases` L887 用 `chat-upload-leases`。`.tmp` 与
  `.<leaf>` 都是**真目录**才清扫；`Blocked`（符号链接 / 非目录）整轮跳过并记一行日志；
  `Missing`（从无上传 / 从无活租约，常态空态）静默返回，与旧 `names()` 吞错一致。
  动机：`read_dir` / `remove_dir_all` 跟随中间符号链接，而 `read_at` 的 containment
  校验只在有 `metadata.json` 时生效——没有它，符号链接根会把项目外目录当孤儿删掉；
  租约半轮更险，`lease.json` 缺失即判过期，删除是默认动作。
  判据是**等值**而非「在项目内」：`plain_root` 自己 `canonicalize` 并要求结果与
  `<project>/.tmp/<leaf>` 字面相同，不相等（含指向项目内另一目录的链接）即 `Blocked`。
  `Plain` 携带的就是这个已校验的解析路径，直接充当轮首快照，调用方不再各自锚定。
- `inside_root` L1249-1254（doc L1242-1248）：全条唯一的 containment 判定，两半轮与
  `release` 共四类递归删除点共用——解析 `dir`，要求「≠ 根快照 且 以根快照为前缀」；
  解析失败按「不在根内」处理（= 不删）。抽成单点实现，避免各处写法漂移出不同判据。
- `sweep_expired_records` L434-511：根来自 `plain_root` 后全程复用（`root_real`），
  之后的 containment 比对一律「现解析的条目 × 轮首快照」，**不再逐条重解析根**。
  逐条重解析会让换入的符号链接在等号两边同时生效，把闸变成 no-op 覆盖整轮剩余时间。
- `sweep_expired_records` L491-503：到期记录的删除分支与孤儿分支同样过 `inside_root`，
  未命中记 `[uploads] cleanup skipped <路径>: outside the uploads root` 并跳过该条。
  （r3 前这里没有守卫，轮中换链后本轮剩余每条到期记录都会删到项目外。）
- `reclaim_orphan` L512-547：只回收**真目录**条目；年龄判据 `mtime >= now - retention_ms`
  保留，之后才做 containment（L537 `inside_root`），把检查到 `remove_dir_all` 之间的
  窗口压到几条指令，而不是整轮。
- `UploadArtifactLease::release` L1086-1110：同一棵租约树上的第三个删除点，`dir` 在
  acquire 时由纪录的 canonical 项目拼出，此后可存活很多轮。删除前要求根
  `canonicalize` 回 `<project>/.tmp/chat-upload-leases` 字面（否则视为被换链，跳过），
  再过 `inside_root(&root_real, &self.dir)`；未命中静默返回 `Ok(())`（重复 release 语义不变）。
- `spawn_cleanup` L1037-1042：调度循环的失败日志同样改 `writeln!` + 丢弃错误；`eprintln!`
  在 stderr 不可写时 panic，而这里的 panic 发生在被 detach 的任务里——清理调度会静默死亡，
  正是该测试要防的结局。
- `skip_record` / `log_cleanup` L549-566：日志行 `[uploads] cleanup skipped <路径>: <错误>` /
  `[uploads] cleanup reclaimed orphan: <路径>`。日志写失败被丢弃（`writeln!` + 忽略错误），
  不用会 panic 的 `eprintln!`——否则 stderr 不可写会把「逐条容错」变回整轮中止。
- `acquire_attachment_lease` L578-644：`lease.json.<pid>.<rand>.tmp` → `write_all` →
  `sync_all` → `rename`（照 `write()` L779-807 的 metadata.json 范式），字节内容不变；
  失败由既有错误路径 `remove_tree(&dir)` 连 temp 清掉。新增 `lease-rename` fault 注入点。
- `cleanup_expired_leases` L880-927：删租约前新增 `lease-rm` fault 注入点（测试期模拟整轮失败，
  生产行为不变）。读侧语义不变：`lease.json` 缺失 / 不可解析仍按过期处理（TS L431-438 同病），
  本条只保证 Rust 自己的写者不再产生半截文件。根快照同样来自 `plain_root`（已核等值）；
  L913-919 每份租约目录删除前过 `inside_root`，未命中记
  `[uploads] lease cleanup skipped <路径>: outside the leases root` 并跳过；
  L923 清空后的 `<id>` 目录删除同样受 `inside_root` 约束。

## 范围

- 只影响 `upload` 清理与租约写路径；`create` / `get` / `write_part` / `complete` / `cancel`
  / `fail` / `verify_attachment` 的错误语义不变。
- 孤儿回收的 retention 取 `retention_ms`（上传 TTL，默认 24 h）而非
  `lease_retention_ms`：孤儿目录是 `chat-uploads` 下的**上传任务目录**，其生命周期上界
  就是上传 TTL；lease TTL 界定的是另一棵根 `.tmp/chat-upload-leases` 下 crash-orphan
  租约，有独立清扫。`metadata.json` 在 `create` 时落盘，故「刚创建、metadata 尚未写完」
  的目录 mtime ≈ now，必然满足 `mtime >= now - retention`，不会被误删（配对负例
  `upload_cleanup_keeps_fresh_orphan_directories` 钉死）。
- 返回计数语义：孤儿回收不进 `cleanup_expired` 的返回值，保持 TS 的
  「删除的到期记录数」含义（oracle 重放的 `cleanup` 结果不变）。
- `expiresAt` 不可解析 / 缺失：与 TS 一致，按**已到期删除**（`parse_node22_date` 返回
  `Ok(None)` → `future` 为 `false`）。只有本地时区解析器失败才走 `skip_record` 保留分支，
  与「数据坏了」无关；本单不含 fail-closed 保留。
- `cleanup_expired_leases` 的「解析失败 = 过期」读侧行为保留（TS parity）；原子化只在写侧，
  语义是「读者不会解析到半截 lease」，**不是**「活租约永不被删」——`make_dir` 到 `rename`
  之间 `lease.json` 缺席，读侧仍按过期处理（TS 同样的窗口）。

## 已知限制（本单不修，登记于此）

- `metadata.json` 存在但不可读 / 不可解析 / 身份不符的任务目录走 `skip_record`，不回收：
  `files/` 字节仍会永久保留（与 TS 相同的泄漏面，只是不再有整轮 Err 把它显式暴露出来）。
  放宽到「坏 metadata 也按 mtime 回收」需要先能区分「内容坏」与「暂时 IO 错」，本单不做。
- 租约半轮仍是全有全无（`cleanup_expired_leases` 内 `remove_tree(..)?`）：单条租约删不掉
  会中止本轮剩余租约与后续 project 的清扫。这是保留的 TS parity，记录半轮的容错不适用于它。
- 记录侧 `remove_tree` 失败分支由 `record-rm` fault 注入点 + `cleanup-record-rm-aborts`
  变异钉住；`future` 的 `Err` 分支只在本地时区解析器失败时可达，无独立注入点。
- 残留 TOCTOU：containment 检查与 `remove_dir_all` 之间仍有「几条指令」的窗口，
  若攻击者恰在此刻把**真目录本身**换成指向项目外的符号链接，std 的纯路径 API 仍会跟随。
  彻底消除需要 dirfd + `openat` / `unlinkat(O_NOFOLLOW)` 式遍历删除（std 无稳定 API），
  本单不做。威胁模型说明：能改动 `<project>/.tmp` 的进程本来就对项目树有写权限，
  可直接删数据，此窗口不跨越权限边界；本轮修掉的是「换链后整轮剩余时间都可利用」
  的确定性窗口（回归测试 `upload_cleanup_does_not_follow_a_root_swapped_mid_round` /
  `upload_cleanup_record_removal_does_not_follow_a_root_swapped_mid_round` /
  `upload_lease_cleanup_does_not_follow_a_root_swapped_mid_round`）。
- `plain_root` 的两次 `symlink_metadata` 与紧随的 `canonicalize` 之间的换链窗口由等值判据
  兜住（解析结果必须字面等于 `<project>/.tmp/<leaf>`，换成任何链接都不等值）；但
  「快照取到正确路径、条目在检查后被换成链接」仍属上面的指令级窗口，同样要 dirfd 才能根除。
- `release` 的守卫是「删除前再判一次」，不是「acquire 时的快照」：`dir` 本身没有保存
  acquire 时的根快照，因此 release 时若根已不存在（canonicalize 失败）静默按已删处理。
  语义上安全（不删即不越界），代价是「根被换链」时该租约目录留在原地，靠租约清扫在
  到期后回收。


## 机器判据

- `src/pilotdeck-gateway/src/upload/tests.rs`（行号以本提交为准）：
  `upload_cleanup_bad_record_does_not_abort_the_sweep`（L1032）、
  `upload_cleanup_reclaims_expired_orphan_directories`（L1078）、
  `upload_cleanup_keeps_fresh_orphan_directories`（L1107）、
  `upload_cleanup_never_deletes_through_a_symlinked_root`（L1134）、
  `store_swapping_tmp`（L1181，`now` 回调首次调用时换链并计数，三个换链测试共用）、
  `upload_cleanup_does_not_follow_a_root_swapped_mid_round`（L1205）、
  `upload_cleanup_record_removal_does_not_follow_a_root_swapped_mid_round`（L1243）、
  `upload_lease_cleanup_never_deletes_through_an_entry_symlink`（L1281）、
  `upload_lease_cleanup_does_not_follow_a_root_swapped_mid_round`（L1309）、
  `upload_lease_cleanup_never_deletes_through_a_symlinked_root`（L1353）、
  `upload_lease_release_does_not_follow_a_swapped_leases_root`（L1385）、
  `upload_cleanup_record_removal_failure_does_not_stop_the_sweep`（L1416）、
  `upload_lease_atomic_rename_failure_keeps_valid_leases`（L1446）、
  `cleanup_failure_keeps_the_schedule_alive`（L1519，失败注入点改为 `lease-rm`）。
- 负控 `src/pilotdeck-gateway/tests/fixtures/upload-gateway/negative_controls.py`：
  `cleanup-bad-record-aborts`、`cleanup-orphan-reclaim-removed`、
  `cleanup-orphan-fresh-deleted`、`lease-write-in-place`、
  `cleanup-symlink-root-swept`、`cleanup-record-rm-aborts`、
  `cleanup-orphan-root-recanonicalized`、`cleanup-lease-symlink-root-swept`、
  `cleanup-record-rm-follows-swapped-root`、`cleanup-lease-entry-symlink-swept`、
  `cleanup-lease-root-recanonicalized`、`lease-release-symlink-root-swept`
  （各绑上述具名测试与唯一失败文案，共 12 条；整份文件 34 条锚点）。
  其中 `cleanup-symlink-root-swept` / `cleanup-lease-symlink-root-swept` 的变异是
  「只 `canonicalize` 根、不加闸」的老写法：两层防护（`plain_root` 等值闸 + `inside_root`）
  一起失效，才真正复现穿链删除；只删一层时另一层仍挡住，负控会假绿。
- 复算命令：
  `cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib upload`
  （49 passed，含 `upload_oracle_replay_matches_ts_results` 与
  `upload_golden_bytes_match_ts_fixtures` 不改金样全绿）；
  `python3 -I src/pilotdeck-gateway/tests/fixtures/upload-gateway/negative_controls.py --check`；
  `python3 -I scripts/gateway-parity/check_frozen.py --base 5ab9485394f0`。
- TS 侧复现（基线 `resources/PilotDeck` @ cc7c4e6，node v22.22.0），一条命令直跑，
  无需手改导入路径（`repro.mts` 存档里写的是另一台机器的基线绝对路径，sed 换成主仓路径）：

  ```sh
  # 起点是主仓，不是 PR worktree：存档与只读基线都在主仓下面。
  cd /Users/da/ws/pilotdeck-rs
  rm -rf /tmp/ts-repro-1646 && mkdir -p /tmp/ts-repro-1646/work
  sed 's#/Users/da/ws/pdrs-runner/resources/PilotDeck#/Users/da/ws/pilotdeck-rs/resources/PilotDeck#' \
    _tmp/20261106-1646-receive/delivery/ts-repro/repro.mts > /tmp/ts-repro-1646/repro.mts
  (cd resources/PilotDeck && node --import ./node_modules/tsx/dist/loader.mjs \
    /tmp/ts-repro-1646/repro.mts /tmp/ts-repro-1646/work)
  ```

  实测输出（2026-11-06，主仓 `_tmp` 证据 + 只读基线）：

  ```
  ts-a threw: undefined - Expected property name or '}' in JSON at position 1 (line 1 column 2)
  ts-a bad-exists: true good-exists: true lease-exists: true
  ts-b cleaned: 0 orphan-exists: true files-exists: true
  ts-c cleaned: 0 live-lease-exists: false
  ```

## 批准记录

未批准，非已接受差异。来源 issue #1646（P2-跟进）；评审块
`_tmp/20261106-1646-review/deepseek/` 与 `_tmp/20261106-1646-review/minimax/`
（PR #1773，双谱系非作者评审；GLM 为实现方）。
