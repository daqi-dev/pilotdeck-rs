---
id: A8-UPLOAD-CLEANUP-ROBUSTNESS-001
---

## 类别

有意改进（Rust 比 TS 更健壮的单边偏离，共三条，均限于 upload 清理路径）：

1. `cleanup_expired` 逐条容错：单条记录读 / 判到期 / 删除失败只记日志跳过，不再中止整个 project 的一轮清理与后续 `cleanup_expired_leases`。
2. 无 `metadata.json` 的孤儿任务目录按 mtime 超过上传 retention 后回收（TS 永不回收）。
3. `lease.json` 原子写（temp + rename），读者永远不会观察到 0 字节 / 半截的 `lease.json`。

## TS 依据

cc7c4e6 `src/gateway/dialog/UploadStore.ts`（node v22.22.0 实测，证据
`_tmp/20261106-issue-1646/segment-1.md` 与 `_tmp/20261106-issue-1646/ts-repro/repro.mts`，
三格各 3 次输出一致）：

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
  `remove_tree` Err → `skip_record`（`eprintln!` 日志带记录路径）后继续。`list_in_project`
  的严格语义保留给 `create` / `find_idempotent` / `mutate` 等调用方，未放宽。
- `plain_uploads_root` L1181-1191：记录半轮只在 `<project>/.tmp` 与
  `<project>/.tmp/chat-uploads` 都是**真目录**时运行，否则整轮跳过并记一行日志。
  `read_dir` / `remove_dir_all` 会跟随中间符号链接，而 `read_at` 的 containment 校验
  只在有 `metadata.json` 时生效——没有它，符号链接根会把项目外目录当孤儿删掉。
- `reclaim_orphan` L488-521：只回收**真目录**条目，且 canonical 路径必须落在
  canonical 根之下（root 已由 `plain_uploads_root` 保证是真目录），mtime 早于
  `now - retention_ms` 才回收，否则保留。
- `skip_record` / `log_cleanup` L523-540：日志行 `[uploads] cleanup skipped <路径>: <错误>` /
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


## 机器判据

- `src/pilotdeck-gateway/src/upload/tests.rs`：
  `upload_cleanup_bad_record_does_not_abort_the_sweep`（L1032）、
  `upload_cleanup_reclaims_expired_orphan_directories`（L1078）、
  `upload_cleanup_keeps_fresh_orphan_directories`（L1107）、
  `upload_cleanup_never_deletes_through_a_symlinked_root`（L1134）、
  `upload_cleanup_record_removal_failure_does_not_stop_the_sweep`（L1175）、
  `upload_lease_atomic_rename_failure_keeps_valid_leases`（L1205）、
  改写的 `cleanup_failure_keeps_the_schedule_alive`（L1278，失败注入点改为 `lease-rm`）。
- 负控 `src/pilotdeck-gateway/tests/fixtures/upload-gateway/negative_controls.py`：
  `cleanup-bad-record-aborts`、`cleanup-orphan-reclaim-removed`、
  `cleanup-orphan-fresh-deleted`、`lease-write-in-place`、
  `cleanup-symlink-root-swept`、`cleanup-record-rm-aborts`
  （各绑上述具名测试与唯一失败文案）。
- 复算命令：
  `cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway upload`（43 passed，
  含 `upload_oracle_replay_matches_ts_results` 与 `upload_golden_bytes_match_ts_fixtures`
  不改金样全绿）；
  `python3 -I src/pilotdeck-gateway/tests/fixtures/upload-gateway/negative_controls.py --check`；
  `python3 -I scripts/gateway-parity/check_frozen.py --base 5ab9485394f0`。
- TS 侧复现：`node --import <baseline>/node_modules/tsx/dist/loader.mjs
  _tmp/20261106-issue-1646/ts-repro/repro.mts <tmpdir>`（node v22.22.0，三格 ×3 一致）。

## 批准记录

未批准，非已接受差异。来源 issue #1646（P2-跟进）；评审块
`_tmp/20261106-1646-review/deepseek/` 与 `_tmp/20261106-1646-review/minimax/`
（PR #1773，双谱系非作者评审；GLM 为实现方）。
