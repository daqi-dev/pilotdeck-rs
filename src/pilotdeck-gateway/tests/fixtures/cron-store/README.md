# Cron store TS fixtures

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，被测源码 `src/cron/storage/CronPaths.ts`（42 行）、`CronTaskStore.ts`（250 行）、`CronStoreMigration.ts`（472 行），整个基线 668 个文件的 SHA-256 清单记在 `oracle.json`。所有路径、步骤结果与落盘树都由真实 TS 类（`resolveCronPaths` / `CronTaskStore` / `migrateCronStores`）产生，无实现源码复制；环境准备（基线要求的 Node v22.22.0、基线 lockfile 的 tsx 4.22.3、`PDRS_BASELINE`）同 `../operation-ledger/README.md`。生成器启动即断言 `new Intl.Collator().resolvedOptions().locale === 'en-US'`，非 en-US 环境直接失败。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/cron-store/generate.mts /tmp/fresh-cron-store-output
```

生成器先断言基线 HEAD，再把无参时钟钉在 `Date.UTC(2026, 0, 1)`（`installFixedClock`，同 `../_shared/generator_guard.mjs`；marker 的 `completedAt`、锁新鲜度判定都由该纪元决定，锁种子的 mtime 也按 `Date.now() - age` 相对该纪元回拨）。输入文件逐场景独占写入（`flag: 'wx'`，目录递归创建），`oracle.json` 独占发布，输出目录必须尚不存在。迁移场景记录一次执行的树，`twice` 场景在无错时用同一 logger 再跑一次并记录第二次的树；警告元数据与错误文案中的场景 home 替换为 `{HOME}`，使 oracle 与输出目录无关（fix-r1 重生成时验证过连续两次输出字节一致）。

矩阵形状固定为 **9 + 10 + 33 + 23**（paths / eventsPaths / store / migration）。`generate.mts` 在发布 `oracle.json` 前、Rust `oracle_matrix_counts_match_generator` 在重放前都精确断言这四个数，增删场景必须同时更新两侧断言及本段。Rust 侧每个 cell 恰好一个具名测试（`src/pilotdeck-gateway/src/runtime/cron/tests.rs`，87 个测试函数：83 直跑（含 fix-r2 的 deleted-cwd 父测试与 #1258 的 panic / contended tail 守卫）+ 2 个 locale 门控父测试 + 2 个 `#[ignore]` worker（locale / deleted-cwd）；其中 non-UTF-8 home 守卫 #1400 起 `#[cfg(unix)]` + 运行期探测，拒绝非 UTF-8 文件名的宿主自跳过并打印 skip 行，见下「非 cell 的守卫测试」），逐 cell 比较记录结果与落盘树（文件 base64 解码后逐字节，目录集合逐一相等）。

两个 cell 的字节依赖非 ASCII 文本的 `localeCompare` 排序（`store:put-sort-missing-and-collation`——`ä` 对 `z`/`Z`，`migration:duplicate-runs-dedupe`）：Rust 通过 `crate::test_process::run_cases` 在 `LANG=en_US.UTF-8` 的子进程里重放（env_clear，`--exact --ignored --test-threads=1`，`PDRS_CRON_CASE=section:cell` 分发），与生成器自身的 en-US 断言同一契约；worker `cron_locale_worker` 只被子进程拉起。

```sh
cd src
cargo test -p pilotdeck-gateway --lib runtime::cron::tests  # Linux、macOS：均 85 passed；均 2 ignored（macOS 的 non-UTF-8 守卫自跳过，计入 passed）
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
§ 与 ‡ 互为镜像（#1400）：把探测强制为 true 只在拒绝非 UTF-8 文件名的宿主上可杀（夹具创建不再自跳过，撞 EILSEQ 死在带 `#1400` 标记的 create panic 上）；Linux 上探测本来就返回 true，强制无行为差异，该行记 SKIPPED——不计 KILLED 也不计 INVALID，不使整轮失败。

## 负控

`negative_controls.py` 复用 `../negative_process.py` 和 `../negative_suite.py`，23 项变异各绑定一个具名测试，判杀证据是该测试自身 panic 载荷携带的 `#1216` 消息（#1258 起 panic 路径的 tail release 变异绑定 `#1258` 消息，#1400 起跳探针变异绑定 `#1400` 消息）；编译失败、正控失败、锚点失配、恢复失败均为 INVALID，任一非 KILLED 即非零退出（#1289 → #1400 起平台缺席按镜像记账：非 Linux 宿主对 entry-lossy-home 记 SKIPPED（绑定测试自跳过，变异到不了），Linux 宿主对 skip-probe-forced-writable 记 SKIPPED（探测本来为真，强制无差异）——均不计 KILLED 也不计 INVALID，不使整轮失败；但全 SKIPPED、零 KILLED 仍非零退出：什么都没证明）。守卫覆盖 `store.rs`、`migration.rs`、`paths.rs`、`tests.rs`、脚本自身与两个共享裁决依赖，任一脏即拒跑；调用方必须提供 `CARGO_TARGET_DIR`（本树构建目录）。

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
| skip-probe-forced-writable§（#1400 的运行期探测被强制为 true：拒绝非 UTF-8 文件名的宿主上夹具不再自跳过，撞 EILSEQ 死在带 #1400 标记的 create panic） | migration_non_utf8_home_keeps_one_physical_key | #1400 探测跳过 |
| entry-cwd-unconditional（入口无条件读 cwd，绝对 home 被已删 cwd 误报） | cron_deleted_cwd_worker | fix-r2 P2 / fix-r3 P1 |
| project-key-cwd-unconditional（normalizedProjectKey 的单参 resolve 再次无条件读 cwd，已删 cwd 下绝对 projectKey 路由失败） | cron_deleted_cwd_worker | fix-r3 P2 |
| paths-key-cwd-unconditional（resolveCronPathsFrom 的绝对 key 解析再次无条件读 cwd，公共入口在已删 cwd 下失败） | cron_deleted_cwd_worker | fix-r3 P2 |
| paths-entry-cwd-unconditional（公共入口自身的 home/key 解析再次无条件读 cwd，与迁移入口同形） | cron_deleted_cwd_worker | fix-r3 P2 |
| lock-guard-removed（Drop guard 不再释放锁，panic 后锁滞留 10 分钟） | panicking_migration_body_still_releases_the_lock | fix-r2 P2 |
| tail-entry-never-removed（完成的 tail 永不从注册表删除，进程级 map 无界增长） | store_tail_entry_removed_after_uncontended_mutation | fix-r2 P3 |
| tail-release-counts-live-sibling（release 把自己已释放的克隆计作存活持有者，兄弟变异克隆仍在途时即删条目，同文件下一次变异会链接第二条 tail 丢串行） | store_tail_entry_removed_after_contended_mutation_pair | fix-r3 P2 |
| tail-release-skipped-on-panic（mutate_task_file 退回显式 release 调用，panic 的 mutation 跳过 release，注册表条目永久残留——TS finally 在 throw 时也删，CronTaskStore.ts:168-172） | store_tail_entry_removed_after_panicking_mutation | #1258 第 2 项 |

cwd 相关四行直接绑 `#[ignore]` worker `cron_deleted_cwd_worker`（`--ignored` + `PDRS_CRON_CASE=absolute-home-deleted-cwd`）：父测试的 panic 载荷内嵌 worker 的 libtest 输出，共享裁决在 worker 自身的 FAILED 行截断，`#1216` 标记到不了父测试的 panic 块（fix-r3 P1 改绑）。

迁移自身的 `atomic_write`（temp+rename）没有对应变异：绑定 `atomic-replace-keeps-old-inode-bytes` 的读 fd 观测只覆盖 store 写入路径，迁移路径的原子性没有可观测的 oracle 判据（迁移是单线程顺序写，重放树只比较最终字节），固不在负控范围。

```sh
python3 -I src/pilotdeck-gateway/tests/fixtures/cron-store/negative_controls.py --check
CARGO_TARGET_DIR=/absolute/build/dir python3 -I src/pilotdeck-gateway/tests/fixtures/cron-store/negative_controls.py /tmp/fresh-cron-store-negative [mutation ...]
```

**执行记录**：脚本要求树等于已提交状态（`mutating_sources` 的 HEAD 守卫，任一守卫文件或整个 `src/` 树脏即拒跑）。改后脚本（`SKIPPED`/`name` 记账）的 **21 项**全量跑由协调者在宿主完成：macmini（darwin）20 KILLED + 1 SKIPPED（`entry-lossy-home` 行，绑定测试 Linux-only）、cb8（Linux）21 KILLED（协调者宿主证据，见 PR #1291 正文）。#1258 追加的第 22 项 `tail-release-skipped-on-panic` 单独实跑 KILLED：cb8（Linux，原始日志 `_tmp/20261026-issue-1258/negative/summary.json`）、协调者 macOS 本机（与同路径旧行 `tail-release-counts-live-sibling` 一并 `KILLED=2`，`_tmp/20261026-issue-1258/negative-recheck/summary.json`，见 PR #1546 正文）。#1400 追加的第 23 项 `skip-probe-forced-writable` 在 darwin（APFS：探测说谎 → fixture 创建 EILSEQ）按同一序列手动变异验证 KILLED——应用 → `cargo test --no-run` 编译 → 被绑测试 `--exact` 实跑 → 恢复逐字节核对并重编正控，原始输出见 `_tmp/20261026-issue-1495/fix-1400/verify-full.log`（KILLED 于 `#1400 non-UTF-8 home: the probe accepted the name but creating … failed: Illegal byte sequence (os error 92)`；还原后 85 passed 且 skip 行可见）；Linux 上探测本来为 true、强制无差异，该行按镜像记 SKIPPED。`--check` 报 23 项锚点唯一。按 23 项计，预期 darwin 22 KILLED + 1 SKIPPED（`entry-lossy-home`）、Linux 22 KILLED + 1 SKIPPED（`skip-probe-forced-writable`）；但树内没有一次跑满 23 项的宿主原始记录（现有证据为上面的 21 项全量 + 第 22/23 项单行实跑与手动变异验证），故本段不把 21 项的全量计数当作 23 项的结论；补跑后回填两处计数。CI 只跑 Rust 正控，不跑负控脚本，故 `entry-lossy-home` 的杀伤证据只来自 Linux 宿主负控（cb8），CI 不覆盖。更早轮次源码未提交时曾用等价脚本（fix-r2 证据目录 `sample_negative.py`，与 fix-r1 的 `manual-mutations.py` 同一序列）手动验证：快照字节 → 应用变异 → `cargo test --no-run` 编译 → 被绑测试 `--exact` 实跑 → 恢复并逐字节校验（改前证据：该序列不产生 SKIPPED/name 记账，不作为改后脚本行为的证据）；fix-r2 抽样 5 项（entry-lossy-home / entry-cwd-unconditional / lock-guard-removed / tail-entry-never-removed / concurrency-serialization-removed）全 KILLED——该抽样早于 #1289 的 Linux-only 门（当时守卫为 `#[cfg(unix)]`，APFS/HFS+ 宿主建不出非 UTF-8 夹具、正控即失败，样本必然出自可建夹具的宿主），#1289 起 darwin 宿主按上述规则对 entry-lossy-home 记 SKIPPED。

非 cell 的守卫测试：`migration_non_utf8_home_keeps_one_physical_key`（#1289 → #1400：`#[cfg(unix)]` + 运行期探测 `non_utf8_names_writable`，按 fixture 的操作序探测同形操作——先 `create_dir_all` 一个非 UTF-8 名目录链、再经该目录路径写非空文件（`tests/workspace_snapshot.rs` 的同名探测仍是文件叶写：那边只建非 UTF-8 名**文件**、从不建目录，故文件写才是它的同形操作）；接受非 UTF-8 文件名的宿主（Linux 等）照跑原断言，拒绝的宿主（APFS/HFS+，EILSEQ）自跳过并打印 skip 行。#1400 评审（sibling/boundary P1）：探测按 errno 分诊，**只有 EILSEQ**（文件名本身被拒）算「宿主不支持」而跳过，其余错误（EACCES、ENOSPC……）以 `#1400 non-UTF-8 home: probing … failed for a reason other than the file name` 硬失败——否则探测会把 Linux 侧真实的夹具故障吞成「跳过」，与它自己声明的判据相反。#1289 的编译期门从此不再需要：一次 macOS 套件运行里「缺席」与「通过」不再同形，且探测独立于夹具，Linux 侧真实夹具失败仍硬失败——正是 #1289 当初选编译门时的顾虑；非 UTF-8 home + 同目录 projectKey，P1 双 key 回归）、`migration_absolute_home_survives_deleted_cwd`（父测试经 `run_cases` 拉起 `#[ignore]` worker `cron_deleted_cwd_worker`，worker 删除自身 cwd 后以绝对 home 跑迁移）、`panicking_migration_body_still_releases_the_lock`（warn 里 panic 的 logger + `catch_unwind`，断言锁文件已删）、`store_tail_entry_removed_after_uncontended_mutation`（完成 mutation 后 `TASK_FILE_TAILS` 条目删除，对齐 CronTaskStore.ts:168-172；等待者持有克隆时不删，串行性由并发测试守卫）。`blocked-dir-tasks-eacces` 的重放在 `geteuid()==0` 时跳过（root 可读 000 文件，该 cell 会因 warning/tree 不符而误报；读错误分支由 EISDIR cell 与 `read-tasks-error-aborts` 变异覆盖），chmod 恢复按保存的原 mode 而非固定 644。#1400 评审（sibling P1）：该 root 跳过与上面的非 UTF-8 跳过同形——同样**直写 stderr 句柄**打印 skip 行（`eprintln!` 会被 libtest 捕获，默认跑法下与通过不可区分），所以 root 宿主上这组用例是 22/23 格，日志里说得出来而不是报成全部通过。

## 范围

只证明 cron 存储契约（路径解析、任务/运行历史/运行事件文件、跨目录迁移）；未接入调度器、会话运行时或 gateway method，默认代理路由不变。锁文件内容（pid/startedAt）与 marker 文案按 TS 原样记录；`CronStoreError`/迁移错误文案对拍 `String(caught)` 的 `{HOME}` 屏蔽形式，不声称与 V8 完全一致。
