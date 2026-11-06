---
id: A8-UPLOAD-CLEANUP-ROBUSTNESS-001
---

## 类别

有意改进（Rust 比 TS 更健壮的单边偏离，共四条，均限于 upload 清理路径）：

1. `cleanup_expired` 逐条容错：单条记录读 / 判到期 / 删除失败只记日志跳过，不再中止整个 project 的一轮清理与后续 `cleanup_expired_leases`。
2. 无 `metadata.json` 的孤儿任务目录按 mtime 超过上传 retention 后回收（TS 永不回收）。
3. `lease.json` 原子写（temp + rename），读者永远不会观察到 0 字节 / 半截的 `lease.json`。
4. 两棵清理根（`chat-uploads` / `chat-upload-leases`）都加「必须是真目录」闸 + 轮首根快照，
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
- `sweep_expired_records` L434-479：逐条容错。`read_at` Err / `expiresAt` 解析 Err /
  `remove_tree` Err → `skip_record`（`writeln!` 日志带记录路径，写失败丢弃）后继续。`list_in_project`
  的严格语义保留给 `create` / `find_idempotent` / `mutate` 等调用方，未放宽。
- `plain_root` L1236-1252：`.tmp/<leaf>` 的三态闸（`Plain` / `Missing` / `Blocked`）。
  两棵根都过它：记录半轮 `sweep_expired_records` L440 用 `chat-uploads`，
  租约半轮 `cleanup_expired_leases` L879 用 `chat-upload-leases`。`.tmp` 与
  `.<leaf>` 都是**真目录**才清扫；`Blocked`（符号链接 / 非目录）整轮跳过并记一行日志；
  `Missing`（从无上传 / 从无活租约，常态空态）静默返回，与旧 `names()` 吞错一致。
  动机：`read_dir` / `remove_dir_all` 跟随中间符号链接，而 `read_at` 的 containment
  校验只在有 `metadata.json` 时生效——没有它，符号链接根会把项目外目录当孤儿删掉；
  租约半轮更险，`lease.json` 缺失即判过期，删除是默认动作。
- `sweep_expired_records` L447-457：轮首把根 **canonicalize 一次**并全程复用（`root_real`），
  之后的 containment 比对一律「现解析的条目 × 轮首快照」，**不再逐条重解析根**。
  逐条重解析会让换入的符号链接在等号两边同时生效，把闸变成 no-op 覆盖整轮剩余时间。
- `reclaim_orphan` L502-540：只回收**真目录**条目；年龄判据 `mtime >= now - retention_ms`
  保留，之后才做 containment（`canonicalize(dir)` 必须落在轮首根快照之下），
  把检查到 `remove_dir_all` 之间的窗口压到几条指令，而不是整轮。
- `spawn_cleanup` L1045-1050：调度循环的失败日志同样改 `writeln!` + 丢弃错误；`eprintln!`
  在 stderr 不可写时 panic，而这里的 panic 发生在被 detach 的任务里——清理调度会静默死亡，
  正是该测试要防的结局。
- `skip_record` / `log_cleanup` L542-560：日志行 `[uploads] cleanup skipped <路径>: <错误>` /
  `[uploads] cleanup reclaimed orphan: <路径>`。日志写失败被丢弃（`writeln!` + 忽略错误），
  不用会 panic 的 `eprintln!`——否则 stderr 不可写会把「逐条容错」变回整轮中止。
- `acquire_attachment_lease` L547-601：`lease.json.<pid>.<rand>.tmp` → `write_all` →
  `sync_all` → `rename`（照 `write()` L748-776 的 metadata.json 范式），字节内容不变；
  失败由既有错误路径 `remove_tree(&dir)` 连 temp 清掉。新增 `lease-rename` fault 注入点。
- `cleanup_expired_leases` L849-865：删租约前新增 `lease-rm` fault 注入点（测试期模拟整轮失败，
  生产行为不变）。读侧语义不变：`lease.json` 缺失 / 不可解析仍按过期处理（TS L431-438 同病），
  本条只保证 Rust 自己的写者不再产生半截文件。

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
  的确定性窗口（回归测试 `upload_cleanup_does_not_follow_a_root_swapped_mid_round`）。


## 机器判据

- `src/pilotdeck-gateway/src/upload/tests.rs`：
  `upload_cleanup_bad_record_does_not_abort_the_sweep`（L1032）、
  `upload_cleanup_reclaims_expired_orphan_directories`（L1078）、
  `upload_cleanup_keeps_fresh_orphan_directories`（L1107）、
  `upload_cleanup_never_deletes_through_a_symlinked_root`（L1134）、
  `upload_cleanup_does_not_follow_a_root_swapped_mid_round`（L1178）、
  `upload_lease_cleanup_never_deletes_through_a_symlinked_root`（L1229）、
  `upload_cleanup_record_removal_failure_does_not_stop_the_sweep`（L1260）、
  `upload_lease_atomic_rename_failure_keeps_valid_leases`（L1205）、
  改写的 `cleanup_failure_keeps_the_schedule_alive`（L1278，失败注入点改为 `lease-rm`）。
- 负控 `src/pilotdeck-gateway/tests/fixtures/upload-gateway/negative_controls.py`：
  `cleanup-bad-record-aborts`、`cleanup-orphan-reclaim-removed`、
  `cleanup-orphan-fresh-deleted`、`lease-write-in-place`、
  `cleanup-symlink-root-swept`、`cleanup-record-rm-aborts`、
  `cleanup-orphan-root-recanonicalized`、`cleanup-lease-symlink-root-swept`
  （各绑上述具名测试与唯一失败文案，共 8 条）。
- 复算命令：
  `cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway upload`（43 passed，
  含 `upload_oracle_replay_matches_ts_results` 与 `upload_golden_bytes_match_ts_fixtures`
  不改金样全绿）；
  `python3 -I src/pilotdeck-gateway/tests/fixtures/upload-gateway/negative_controls.py --check`；
  `python3 -I scripts/gateway-parity/check_frozen.py --base 5ab9485394f0`。
- TS 侧复现（本机，基线 `resources/PilotDeck` @ cc7c4e6，node v22.22.0）：
  `node --import ./node_modules/tsx/dist/loader.mjs <存档>/ts-repro/repro.mts <tmpdir>`
  （在基线目录内运行；`repro.mts` 里的 `UploadStore.ts` 导入路径按基线实际位置改一处，
  其余逐字不变）。`<存档>` = `_tmp/20261106-1646-receive/delivery`。三格 ×3 一致。

## 批准记录

未批准，非已接受差异。来源 issue #1646（P2-跟进）；评审块
`_tmp/20261106-1646-review/deepseek/` 与 `_tmp/20261106-1646-review/minimax/`
（PR #1773，双谱系非作者评审；GLM 为实现方）。
