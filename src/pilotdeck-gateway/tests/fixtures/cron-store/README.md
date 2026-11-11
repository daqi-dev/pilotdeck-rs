# Cron store TS fixtures

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，被测源码 `src/cron/storage/CronPaths.ts`（42 行）、`CronTaskStore.ts`（250 行）、`CronStoreMigration.ts`（472 行），整个基线 668 个文件的 SHA-256 清单记在 `oracle.json`。所有路径、步骤结果与落盘树都由真实 TS 类（`resolveCronPaths` / `CronTaskStore` / `migrateCronStores`）产生，无实现源码复制；环境准备（基线要求的 Node v22.22.0、基线 lockfile 的 tsx 4.22.3、`PDRS_BASELINE`）同 `../operation-ledger/README.md`。生成器启动即断言 `new Intl.Collator().resolvedOptions().locale === 'en-US'`，非 en-US 环境直接失败。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/cron-store/generate.mts /tmp/fresh-cron-store-output
```

生成器先断言基线 HEAD，再把无参时钟钉在 `Date.UTC(2026, 0, 1)`（`installFixedClock`，同 `../_shared/generator_guard.mjs`；marker 的 `completedAt`、锁新鲜度判定都由该纪元决定，锁种子的 mtime 也按 `Date.now() - age` 相对该纪元回拨）。输入文件逐场景独占写入（`flag: 'wx'`，目录递归创建），`oracle.json` 独占发布，输出目录必须尚不存在。迁移场景记录一次执行的树，`twice` 场景在无错时用同一 logger 再跑一次并记录第二次的树；警告元数据与错误文案中的场景 home 替换为 `{HOME}`，使 oracle 与输出目录无关（fix-r1 重生成时验证过连续两次输出字节一致）。

矩阵形状固定为 **9 + 10 + 33 + 23**（paths / eventsPaths / store / migration）。`generate.mts` 在发布 `oracle.json` 前、Rust `oracle_matrix_counts_match_generator` 在重放前都精确断言这四个数，增删场景必须同时更新两侧断言及本段。Rust 侧每个 cell 恰好一个具名测试（`src/pilotdeck-gateway/src/runtime/cron/tests.rs`，90 个测试函数：86 直跑（含 fix-r2 的 deleted-cwd 父测试、#1258 的 panic / contended tail 守卫与 #1637 的真实 EACCES 故障注入）+ 2 个 locale 门控父测试 + 2 个 `#[ignore]` worker（locale / deleted-cwd）；其中 non-UTF-8 home 守卫 #1400 起 `#[cfg(unix)]` + 运行期探测，拒绝非 UTF-8 文件名的宿主自跳过并打印带 `PDRS_TEST_SKIPPED` 机器前缀的 skip 行（#1637 起，`tests/common/skip_line.rs` 的共享常量），见下「非 cell 的守卫测试」），逐 cell 比较记录结果与落盘树（文件 base64 解码后逐字节，目录集合逐一相等）。

两个 cell 的字节依赖非 ASCII 文本的 `localeCompare` 排序（`store:put-sort-missing-and-collation`——`ä` 对 `z`/`Z`，`migration:duplicate-runs-dedupe`）：Rust 通过 `crate::test_process::run_cases` 在 `LANG=en_US.UTF-8` 的子进程里重放（env_clear，`--exact --ignored --test-threads=1`，`PDRS_CRON_CASE=section:cell` 分发），与生成器自身的 en-US 断言同一契约；worker `cron_locale_worker` 只被子进程拉起。

```sh
cd src
cargo test -p pilotdeck-gateway --lib runtime::cron::tests  # Linux、macOS：均 88 passed；均 2 ignored（macOS 的 non-UTF-8 守卫自跳过，计入 passed；#1637 的 EACCES 注入用例 root 下同理）
PDRS_BASELINE=/absolute/path/to/PilotDeck cargo test -p pilotdeck-gateway --test cron_store -- --ignored
```

锁等待时长是注入参数（`MigrationInput::lock_retry_delay`）：TS 基线按 100 次 × 100ms 退避（生成 fresh-lock cell 时真实等约 10 秒），重放用 1ms 延迟（约 100ms）保持套件秒级，超时文案与尝试次数不受影响。

`oracle.json` 各段：`paths[]`（8 个字段）、`eventsPaths[]`（`runId` + 期待路径，含孤立代理 runId）、`store[]`（`home`/`projectKey`/`initial[{path,text|bytes}]`/`steps[{op,...}]`/`results`/`final{files(base64),dirs}`）、`migration[]`（`home`/`input`/`warnings`/`error`/`once`/`twice`，fix-r1 新增 `pilotHomeRelative` 与 `chmod000[]`——前者让重放把 home 以相对路径传入，后者在迁移前 chmod 000、记录树前恢复 644，两侧行为一致才可对拍）。重放要点：JS `slice(-Math.max(0, limit))` 在 `limit ≤ 0` 时因 `-0 === 0` 保留全部记录（limit-0/negative cell 存在的意义）；`readFile utf-8` 有损读取、`JSON.parse` 拒绝 BOM（解析失败按空任务文件处理）；写走 temp+rename，`EPERM`/`EACCES` 回退 copy；`normalize` 要求 `schedule.type === "once"/"cron"` 且分别带字符串 `runAt`/`expression`；迁移读 `tasks.json` 的非 ENOENT 错误（EACCES/EISDIR）与解析失败同样按"阻塞并保留"处理；迁移 marker 是普通写（与 store 的原子写有意保持 TS 原样的差异）；陈旧锁（>10 分钟）删除并告警，新鲜锁按 1 秒退避重试 100 次后超时报错。迁移的 `readDir` 枚举顺序不是契约：桶按目录聚合、记录在写盘前排序，最终每个目录的字节与枚举顺序无关（两侧都已如此实现，`blocked-dir` 家族的多目录场景覆盖了聚合顺序不变量）。

| 家族 | cell |
|---|---|
| paths（9） | absolute、dot-segments、trailing-slash、repeated-separators、slug-fold、slug-root-empty、backslash-and-dashes、unicode-slug、collapse-runs |
| eventsPaths（10） | plain、space-and-slash、whitespace-only、empty、dot-runs、cjk、lone-surrogate、edge-dashes、collapse-runs、mixed-allowed |
| store 读取（7） | list-missing、list-corrupt、list-bom、list-empty-object、list-tasks-not-array、list-schema-version-2、list-normalize |
| store 单任务/写入（9） | get-found-and-missing、put-new、put-new-with-optionals、put-same-key-replaces-and-sorts、put-sort-missing-and-collation†、put-after-corrupt-starts-empty、replace-missing-writes-nothing、replace-found、update-modify |
| store 变更（5） | update-remove、update-missing、delete-found、delete-missing、concurrent-puts-serialize |
| store 原子性（1） | atomic-replace-keeps-old-inode-bytes |
| store 运行历史/事件（11） | append-run-new、append-run-twice、list-runs-missing、list-runs-mixed、list-runs-limit-0、list-runs-limit-negative、list-runs-limit-2、list-runs-limit-large、list-runs-invalid-utf8、append-run-event、append-run-event-sanitizes |
| migration 目录/路由（5） | no-cron-dir、empty-projects-dir、tasks-without-projectkey-stay、tasks-move-to-projectkey-dir、lone-surrogate-projectkey |
| migration 相对 home（2） | relative-home-same-dir-projectkey、relative-home-blocked-dir |
| migration run 历史（3） | runs-by-explicit-projectkey、runs-by-unique-task-project、runs-ambiguous-task-stay |
| migration 阻塞目录（4） | blocked-dir-keeps-source-tasks、blocked-dir-tasks-not-array、blocked-dir-tasks-eacces、blocked-dir-tasks-isdir |
| migration 合并/幂等（7） | invalid-run-lines-preserved、run-events-merge-and-move、duplicate-tasks-dedupe、duplicate-runs-dedupe†、missing-tasks-file-is-writable、non-directory-entries-skipped、migration-twice-is-idempotent |
| migration 锁（2） | stale-lock-removed（11 分钟陈旧）、fresh-lock-times-out（60 秒新鲜锁） |

† locale 门控。
‡ 杀伤仅限 Linux（#1289 → #1400）：绑定测试 #1400 起在所有 unix 上存在，但 APFS/HFS+ 拒绝非 UTF-8 文件名（EILSEQ），测试在夹具创建前自跳过，变异后的 migration 永远到不了——非 Linux 宿主该行记 SKIPPED（不计 KILLED 也不计 INVALID，不使整轮失败）；entry-lossy-home 的杀伤证据须在 cb8 等 Linux 宿主跑负控脚本（CI 只跑 Rust 正控，不跑负控脚本——entry-lossy-home 的杀伤证据只来自 Linux 宿主负控），darwin 记 SKIPPED。
§ #1637 重审：该行历史上唯一的 darwin 杀伤建立在「正控自跳过」之上——被绑测试在拒绝型宿主上什么都没证明（空转正控），那个 KILLED 不被认证。正控自跳过现在带 `PDRS_TEST_SKIPPED` 机器前缀（#1637，`tests/common/skip_line.rs`），脚本的循环在标记命中时把该行记 SKIPPED（原因写明「正控自跳过，本行在本宿主什么都没证明」），不再让空转正控走完变异；Linux 上探测本来就返回 true，强制无行为差异，仍按 NOT_ON_LINUX 记 SKIPPED——即该行在所有宿主都记 SKIPPED。探测的 fail-closed 契约由合成 errno 单测、#1672 四行与 #1637 的真实 EACCES 行（`fail-closed-arm-skips-real-eacces`）承担。

## 负控

`negative_controls.py` 复用 `../negative_process.py` 和 `../negative_suite.py`，29 项变异各绑定一个具名测试，判杀证据是该测试自身 panic 载荷携带的 `#1216` 消息（#1258 起 panic 路径的 tail release 变异绑定 `#1258` 消息，#1400 起跳探针变异绑定 `#1400` 消息，#1672 起四条分诊/重试变异绑定合成 errno 单测的 `#1400` 消息、任意宿主可杀，#1637 起真实 EACCES 行绑定 `#1637` 消息）；编译失败、正控失败、锚点失配、恢复失败均为 INVALID，任一非 KILLED 即非零退出（#1289 → #1400 起平台缺席按镜像记账：非 Linux 宿主对 entry-lossy-home 记 SKIPPED（绑定测试自跳过，变异到不了），Linux 宿主对 skip-probe-forced-writable 记 SKIPPED（探测本来为真，强制无差异）——均不计 KILLED 也不计 INVALID，不使整轮失败；#1637 起共享裁决 `passed()` 排除带 `PDRS_TEST_SKIPPED` 标记的自跳过正控，脚本循环在标记命中时把该行记 SKIPPED（原因写明正控自跳过、本行在本宿主什么都没证明），不再让空转正控走完变异；但全 SKIPPED、零 KILLED 仍非零退出：什么都没证明）。守卫覆盖 `store.rs`、`migration.rs`、`paths.rs`、`tests.rs`、脚本自身与两个共享裁决依赖，任一脏即拒跑；调用方必须提供 `CARGO_TARGET_DIR`（本树构建目录）。

| 变异 | 绑定测试 | 对应问题范围 |
|---|---|---|
| migration-routing-removed（projectKey 路由不再移动） | migration_tasks_move_to_projectkey_dir_matches_ts | 迁移跳过 |
| run-routing-removed（显式 projectKey 的 run 不再路由） | migration_runs_by_explicit_projectkey_matches_ts | fix-r1 路由 |
| corrupt-tasks-parse-error / corrupt-tasks-not-array（损坏 tasks.json 不再视为阻塞） | migration_blocked_dir_keeps_source_tasks_matches_ts / migration_blocked_dir_tasks_not_array_matches_ts | 损坏文件不报错 |
| read-tasks-error-aborts（EACCES/EISDIR 中止迁移而非视为阻塞） | migration_blocked_dir_tasks_isdir_matches_ts | fix-r1 P1（fix-r2 改绑 EISDIR cell：EACCES cell 的测试在 root 下自跳过，EISDIR 与 uid 无关） |
| non-atomic-task-write（原地写替代 temp+rename） | store_atomic_replace_keeps_old_inode_bytes_matches_ts | 非原子写 |
| migration-marker-renamed | migration_tasks_without_projectkey_stay_matches_ts | 迁移标记漂移 |
| list-runs-zero-limit-drains（limit≤0 清空） | store_list_runs_limit_0_matches_ts | slice(-0) 语义 |
| normalize-schedule-kind-ignored（不检查 schedule.type） | store_list_normalize_matches_ts | 归一化弱化 |
| relative-home-unresolved（入口不再绝对化 pilot home，同一目录相对/绝对双键互相覆盖） | migration_relative_home_same_dir_projectkey_matches_ts | fix-r1 P0 |
| concurrency-serialization-removed（put_task 不再持有每文件互斥，读改写交错丢任务） | store_concurrent_puts_across_threads_keep_every_task | fix-r1 P0 |
| stale-lock-enoent-errors（锁删除不容忍 ENOENT） | lock_removal_tolerates_a_vanished_lock_file | fix-r1 P2 |
| surrogate-lossy-reverted（严格 UTF-16 拒绝孤立代理 projectKey） | migration_lone_surrogate_projectkey_matches_ts | fix-r1 P3 |
| entry-lossy-home‡（home 重新经 to_string_lossy 字符串下传，非 UTF-8 home 同目录双键空桶覆盖） | migration_non_utf8_home_keeps_one_physical_key | fix-r2 P1 |
| skip-probe-forced-writable§（#1400 的运行期探测被强制为 true：拒绝非 UTF-8 文件名的宿主上夹具不再自跳过，撞 EILSEQ 死在带 #1400 标记的 create panic——#1637 重审后该杀伤不被认证，行恒记 SKIPPED，见 § 注） | migration_non_utf8_home_keeps_one_physical_key | #1400 探测跳过 / #1637 重审 |
| triage-any-error-skips（#1672 A：分诊兜底臂把任何非 EILSEQ 错误折叠成 RejectsName——任何错误都跳过，即 #1400 三轮评审要杀死的回归；合成 errno 直喂决策，任意宿主可杀） | triage_probe_sorts_every_outcome_with_one_bounded_fd_retry | #1672 分诊回归 |
| fd-limit-folded-into-scratch-failure（#1672 B：fd 臂不再认 EMFILE/ENFILE，fd 限额被折回 scratch 故障、误报成「文件名以外的原因」——#1672 修前形态） | triage_probe_sorts_every_outcome_with_one_bounded_fd_retry | #1672 fd 限额臂 |
| fail-closed-arm-skips（#1672 A：fail-closed 臂改返回 false，Linux 侧 scratch EACCES/ENOSPC 被吞成静默跳过、#1216 双 key 守卫蒸发） | non_eilseq_probe_outcomes_fail_closed_at_the_skip_decision | #1672 fail-closed 臂 |
| fail-closed-arm-skips-real-eacces（#1637 补记 3/7：同一 fail-closed 臂喂**真实** EACCES——0500 scratch 使探测真失败（darwin/APFS 实测权限检查先于名字检查），绑定测试要求 panic；臂折成 false 时探测静默报「不可写」，测试死在自身 #1637 标记上。任意非特权 unix 宿主可杀；root 下绑定测试自跳过带标记，行记 SKIPPED） | probe_eacces_from_a_read_only_scratch_fails_closed | #1637 文件系统级故障注入 |
| fd-retry-any-error（#1672 fix-r2：重试条件放宽成「任何错误都重试一次」，非 fd 错误烧掉第二次尝试；杀点是映射测的 `calls == 1` 钉子） | triage_probe_sorts_every_outcome_with_one_bounded_fd_retry | #1672 重试条件 |
| entry-cwd-unconditional（入口无条件读 cwd，绝对 home 被已删 cwd 误报） | cron_deleted_cwd_worker | fix-r2 P2 / fix-r3 P1 |
| project-key-cwd-unconditional（normalizedProjectKey 的单参 resolve 再次无条件读 cwd，已删 cwd 下绝对 projectKey 路由失败） | cron_deleted_cwd_worker | fix-r3 P2 |
| paths-key-cwd-unconditional（resolveCronPathsFrom 的绝对 key 解析再次无条件读 cwd，公共入口在已删 cwd 下失败） | cron_deleted_cwd_worker | fix-r3 P2 |
| paths-entry-cwd-unconditional（公共入口自身的 home/key 解析再次无条件读 cwd，与迁移入口同形） | cron_deleted_cwd_worker | fix-r3 P2 |
| lock-guard-removed（Drop guard 不再释放锁，panic 后锁滞留 10 分钟） | panicking_migration_body_still_releases_the_lock | fix-r2 P2 |
| tail-entry-never-removed（完成的 tail 永不从注册表删除，进程级 map 无界增长） | store_tail_entry_removed_after_uncontended_mutation | fix-r2 P3 |
| tail-release-counts-live-sibling（release 把自己已释放的克隆计作存活持有者，兄弟变异克隆仍在途时即删条目，同文件下一次变异会链接第二条 tail 丢串行） | store_tail_entry_removed_after_contended_mutation_pair | fix-r3 P2 |
| tail-release-removes-with-live-waiter（release 不看存活持有者就删条目——同 anchor 的 `> 0` 变体，经真实 `mutate_task_file` 入口捕获；`== 2` 变体在该测试里只在一种交错下触发（兄弟自己的 mutation 克隆在 barrier 之后才取，与 holder 的 release 竞争），可能幸存，故另立一条） | store_tail_entry_removed_after_contended_mutate_task_file_pair | #1574 / fix-r3 P2 |
| tail-release-skipped-on-panic（mutate_task_file 退回显式 release 调用，panic 的 mutation 跳过 release，注册表条目永久残留——TS finally 在 throw 时也删，CronTaskStore.ts:168-172） | store_tail_entry_removed_after_panicking_mutation | #1258 第 2 项 |

cwd 相关四行直接绑 `#[ignore]` worker `cron_deleted_cwd_worker`（`--ignored` + `PDRS_CRON_CASE=absolute-home-deleted-cwd`）：父测试的 panic 载荷内嵌 worker 的 libtest 输出，共享裁决在 worker 自身的 FAILED 行截断，`#1216` 标记到不了父测试的 panic 块（fix-r3 P1 改绑）。

迁移自身的 `atomic_write`（temp+rename）没有对应变异：绑定 `atomic-replace-keeps-old-inode-bytes` 的读 fd 观测只覆盖 store 写入路径，迁移路径的原子性没有可观测的 oracle 判据（迁移是单线程顺序写，重放树只比较最终字节），固不在负控范围。

```sh
python3 -I src/pilotdeck-gateway/tests/fixtures/cron-store/negative_controls.py --check
CARGO_TARGET_DIR=/absolute/build/dir python3 -I src/pilotdeck-gateway/tests/fixtures/cron-store/negative_controls.py /tmp/fresh-cron-store-negative [mutation ...]
```

**执行记录**：脚本要求树等于已提交状态（`mutating_sources` 的 HEAD 守卫，任一守卫文件或整个 `src/` 树脏即拒跑）。改后脚本（`SKIPPED`/`name` 记账）的 **21 项**全量跑由协调者在宿主完成：macmini（darwin）20 KILLED + 1 SKIPPED（`entry-lossy-home` 行，绑定测试 Linux-only）、cb8（Linux）21 KILLED（协调者宿主证据，见 PR #1291 正文）。#1258 追加的第 22 项 `tail-release-skipped-on-panic` 单独实跑 KILLED：cb8（Linux，原始日志 `_tmp/20261026-issue-1258/negative/summary.json`）、协调者 macOS 本机（与同路径旧行 `tail-release-counts-live-sibling` 一并 `KILLED=2`，`_tmp/20261026-issue-1258/negative-recheck/summary.json`，见 PR #1546 正文）。#1400 追加的第 23 项 `skip-probe-forced-writable` 在 darwin（APFS：探测说谎 → fixture 创建 EILSEQ）按同一序列手动变异验证 KILLED——应用 → `cargo test --no-run` 编译 → 被绑测试 `--exact` 实跑 → 恢复逐字节核对并重编正控，原始输出见 `_tmp/20261026-issue-1495/fix-1400/verify-full.log`（KILLED 于 `#1400 non-UTF-8 home: the probe accepted the name but creating … failed: Illegal byte sequence (os error 92)`；还原后 85 passed 且 skip 行可见）；Linux 上探测本来为 true、强制无差异，该行按镜像记 SKIPPED。`--check` 报 28 项锚点唯一。按 28 项计（#1672 收货轮 fix-r2 起），预期 darwin 27 KILLED + 1 SKIPPED（`entry-lossy-home`）、Linux 27 KILLED + 1 SKIPPED（`skip-probe-forced-writable`；#1672 的四行绑定合成 errno 单测，两边都计 KILLED）；跑满全量的宿主原始记录（#1672 收货轮）：cb8（Linux）28 项全量宿主负控由协调者实跑，`summary.json` 拷存主仓 `_tmp/20261107-issue-1672/harvest/negctl-cb8/`；#1672 的第 25/26/27 项另在 mbp2 按同一序列手动变异验证（原始记录主仓 `_tmp/20261107-issue-1672/harvest/manual-mutations-final/`）；两处计数（:12 测试函数数、:18 passed 数）已按实测回填（实测 `--list` 89 个测试函数；darwin mbp2 与 Linux cb8 同为 87 passed, 2 ignored；旧 README 的 87/85 在 #1672 前就已各少 1，本轮回填到实测值）。CI 只跑 Rust 正控，不跑负控脚本，故 `entry-lossy-home` 的杀伤证据只来自 Linux 宿主负控（cb8），CI 不覆盖。更早轮次源码未提交时曾用等价脚本（fix-r2 证据目录 `sample_negative.py`，与 fix-r1 的 `manual-mutations.py` 同一序列）手动验证：快照字节 → 应用变异 → `cargo test --no-run` 编译 → 被绑测试 `--exact` 实跑 → 恢复并逐字节校验（改前证据：该序列不产生 SKIPPED/name 记账，不作为改后脚本行为的证据）；fix-r2 抽样 5 项（entry-lossy-home / entry-cwd-unconditional / lock-guard-removed / tail-entry-never-removed / concurrency-serialization-removed）全 KILLED——该抽样早于 #1289 的 Linux-only 门（当时守卫为 `#[cfg(unix)]`，APFS/HFS+ 宿主建不出非 UTF-8 夹具、正控即失败，样本必然出自可建夹具的宿主），#1289 起 darwin 宿主按上述规则对 entry-lossy-home 记 SKIPPED。第 24 项 `tail-release-removes-with-live-waiter`（#1574）在 mbp2（darwin）单行实跑 KILLED，判杀来自被绑测试自身的注册表断言；反证（临时删掉 `Arc::ptr_eq` 断言）同一变异仍 KILLED——该反证证明的是「兄弟的 mutation 克隆先于 holder 的 release」这一交错；另一交错下条目被先删、兄弟重建新 tail，`task_file_tail_registered` 为真，`Arc::ptr_eq` 是唯一杀手，属承重断言，不得删——注意这一臂至今**没有被实跑观测到**（四次实跑都死在第一条断言），它是由读码 + 上面的反证共同支撑的。原始 `summary.json` 与测试日志见 `_tmp/20261106-p2-pack1/negctl/`，PR 正文贴原始输出。#1637（29 项计）：`--check` 报 29 项锚点唯一；预期非 root darwin 27 KILLED + 2 SKIPPED（`entry-lossy-home` 平台缺席、`skip-probe-forced-writable` 正控自跳过标记）、非 root Linux 28 KILLED + 1 SKIPPED（`skip-probe-forced-writable` NOT_ON_LINUX）；root 宿主上 `fail-closed-arm-skips-real-eacces` 的绑定测试自跳过，行按标记记 SKIPPED。第 29 项 `fail-closed-arm-skips-real-eacces` 在 mbp2（darwin，非 root）按同一序列手动变异验证 KILLED（本单树未提交，`mutating_sources` 拒跑，与 #1672 收货前的偏差同款），同时回归复跑第 25/27 项 KILLED、第 23 项在新判据下验证为「正控自跳过（真实日志过 `self_skipped()` 为真）+ 变异仍达 `#1400` panic（行为见证，不计杀伤）」；原始记录见本单证据目录 `_tmp/20261111-issue-1637/`（`mutation-r1{,b}.log`、`ts-mutation-r1.log`、`passed-witness.log`）。29 项全量（两平台）与 root 形态由协调者在宿主跑。

非 cell 的守卫测试：`migration_non_utf8_home_keeps_one_physical_key`（#1289 → #1400：`#[cfg(unix)]` + 运行期探测 `non_utf8_names_writable`，按 fixture 的操作序探测同形操作——先 `create_dir_all` 一个非 UTF-8 名目录链、再经该目录路径写非空文件（`tests/workspace_snapshot.rs` 的同名探测仍是文件叶写：那边只建非 UTF-8 名**文件**、从不建目录，故文件写才是它的同形操作）；接受非 UTF-8 文件名的宿主（Linux 等）照跑原断言，拒绝的宿主（APFS/HFS+，EILSEQ）自跳过并打印 skip 行。#1400 评审（sibling/boundary P1）→ #1672：探测按 errno 分诊，**只有 EILSEQ**（文件名本身被拒）算「宿主不支持」而跳过，其余错误（EACCES、ENOSPC、EROFS……）以 `#1400 non-UTF-8 home: probing … failed for a reason other than the file name` 硬失败——否则探测会把 Linux 侧真实的夹具故障吞成「跳过」，与它自己声明的判据相反。#1672 起 fd 限额（EMFILE/ENFILE——成因在进程/系统而非 scratch，macmini-01 #816 首跑即 `ulimit -n=256` 的实例）单列一臂：先恰好重试一次（重试后 errno 为准），仍触顶则带 `#1400 … hit the process fd limit, not the file name` 专属 marker 硬失败，不再误报成文件名问题。该分诊的**可执行证据**是两个单测把决策整体（含 fd 臂与重试）当被测单元：`runtime::cron::tests::triage_probe_sorts_every_outcome_with_one_bounded_fd_retry`（九类映射 + 重试恰好一次 + 终止）与 `runtime::cron::tests::non_eilseq_probe_outcomes_fail_closed_at_the_skip_decision`（skip/fail-closed 折叠的每条臂，`catch_unwind` 断言非 EILSEQ 必须 panic）——#1400 三轮评审时只有 errno 谓词被测、调用点 match 无可执行证据（三家评审同点），#1672 把调用点折叠进被测函数后，`Err(_) => false`（任何错误都跳过）一类的回归在任意宿主上都是红测试，且负控第 25/26/27/28 行可直接变异该决策（第 28 行钉「非 fd 错误不得重试」的收紧方向）（文件系统级的故障注入负控由 #1637 交付：`probe_eacces_from_a_read_only_scratch_fails_closed` 以 0500 scratch 给探测喂**真实** EACCES——darwin/APFS 实测权限检查先于名字检查，非 UTF-8 名也报 EACCES——`catch_unwind` 断言 fail-closed panic 的诊断与措辞；ENOSPC/EROFS 等仍由合成 errno 单测等价覆盖）。#1289 的编译期门从此不再需要一次 macOS 套件运行里「缺席」与「通过」不再同形，且探测独立于夹具，Linux 侧真实夹具失败仍硬失败——正是 #1289 当初选编译门时的顾虑；非 UTF-8 home + 同目录 projectKey，P1 双 key 回归）、`migration_absolute_home_survives_deleted_cwd`（父测试经 `run_cases` 拉起 `#[ignore]` worker `cron_deleted_cwd_worker`，worker 删除自身 cwd 后以绝对 home 跑迁移）、`panicking_migration_body_still_releases_the_lock`（warn 里 panic 的 logger + `catch_unwind`，断言锁文件已删）、`store_tail_entry_removed_after_uncontended_mutation`（完成 mutation 后 `TASK_FILE_TAILS` 条目删除，对齐 CronTaskStore.ts:168-172；等待者持有克隆时不删，串行性由并发测试守卫）。`blocked-dir-tasks-eacces` 的重放在 `geteuid()==0` 时跳过（root 可读 000 文件，该 cell 会因 warning/tree 不符而误报；读错误分支由 EISDIR cell 与 `read-tasks-error-aborts` 变异覆盖），chmod 恢复按保存的原 mode 而非固定 644。#1400 评审（sibling P1）：该 root 跳过与上面的非 UTF-8 跳过同形——同样**直写 stderr 句柄**打印 skip 行（`eprintln!` 会被 libtest 捕获，默认跑法下与通过不可区分），所以 root 宿主上这组用例是 22/23 格，日志里说得出来而不是报成全部通过。

## 范围

只证明 cron 存储契约（路径解析、任务/运行历史/运行事件文件、跨目录迁移）；未接入调度器、会话运行时或 gateway method，默认代理路由不变。锁文件内容（pid/startedAt）与 marker 文案按 TS 原样记录；`CronStoreError`/迁移错误文案对拍 `String(caught)` 的 `{HOME}` 屏蔽形式，不声称与 V8 完全一致。
