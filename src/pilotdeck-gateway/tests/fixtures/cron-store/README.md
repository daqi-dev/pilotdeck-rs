# Cron store TS fixtures

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，被测源码 `src/cron/storage/CronPaths.ts`（42 行）、`CronTaskStore.ts`（250 行）、`CronStoreMigration.ts`（472 行），整个基线 668 个文件的 SHA-256 清单记在 `oracle.json`。所有路径、步骤结果与落盘树都由真实 TS 类（`resolveCronPaths` / `CronTaskStore` / `migrateCronStores`）产生，无实现源码复制；环境准备（基线要求的 Node v22.22.0、基线 lockfile 的 tsx 4.22.3、`PDRS_BASELINE`）同 `../operation-ledger/README.md`。生成器启动即断言 `new Intl.Collator().resolvedOptions().locale === 'en-US'`，非 en-US 环境直接失败。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/cron-store/generate.mts /tmp/fresh-cron-store-output
```

生成器先断言基线 HEAD，再把无参时钟钉在 `Date.UTC(2026, 0, 1)`（`installFixedClock`，同 `../_shared/generator_guard.mjs`；marker 的 `completedAt`、锁新鲜度判定都由该纪元决定，锁种子的 mtime 也按 `Date.now() - age` 相对该纪元回拨）。输入文件逐场景独占写入（`flag: 'wx'`，目录递归创建），`oracle.json` 独占发布，输出目录必须尚不存在。迁移场景记录一次执行的树，`twice` 场景在无错时用同一 logger 再跑一次并记录第二次的树；警告元数据与错误文案中的场景 home 替换为 `{HOME}`，使 oracle 与输出目录无关（fix-r1 重生成时验证过连续两次输出字节一致）。

矩阵形状固定为 **9 + 10 + 33 + 23**（paths / eventsPaths / store / migration）。`generate.mts` 在发布 `oracle.json` 前、Rust `oracle_matrix_counts_match_generator` 在重放前都精确断言这四个数，增删场景必须同时更新两侧断言及本段。Rust 侧每个 cell 恰好一个具名测试（`src/pilotdeck-gateway/src/runtime/cron/tests.rs`，79 个测试函数：76 直跑 + 2 个 locale 门控父测试 + 1 个 `#[ignore]` worker），逐 cell 比较记录结果与落盘树（文件 base64 解码后逐字节，目录集合逐一相等）。

两个 cell 的字节依赖非 ASCII 文本的 `localeCompare` 排序（`store:put-sort-missing-and-collation`——`ä` 对 `z`/`Z`，`migration:duplicate-runs-dedupe`）：Rust 通过 `crate::test_process::run_cases` 在 `LANG=en_US.UTF-8` 的子进程里重放（env_clear，`--exact --ignored --test-threads=1`，`PDRS_CRON_CASE=section:cell` 分发），与生成器自身的 en-US 断言同一契约；worker `cron_locale_worker` 只被子进程拉起。

```sh
cd src
cargo test -p pilotdeck-gateway --lib runtime::cron          # 78 passed; 1 ignored
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

## 负控

`negative_controls.py` 复用 `../negative_process.py` 和 `../negative_suite.py`，13 项变异各绑定一个具名 cell 重放测试，判杀证据是该测试自身 panic 载荷携带的 `#1216` 消息；编译失败、正控失败、锚点失配、恢复失败均为 INVALID，任一非 KILLED 即非零退出。守卫覆盖 `store.rs`、`migration.rs`、`tests.rs`、脚本自身与两个共享裁决依赖，任一脏即拒跑；调用方必须提供 `CARGO_TARGET_DIR`（本树构建目录）。

| 变异 | 绑定测试 | 对应问题范围 |
|---|---|---|
| migration-routing-removed（projectKey 路由不再移动） | migration_tasks_move_to_projectkey_dir_matches_ts | 迁移跳过 |
| run-routing-removed（显式 projectKey 的 run 不再路由） | migration_runs_by_explicit_projectkey_matches_ts | fix-r1 路由 |
| corrupt-tasks-parse-error / corrupt-tasks-not-array（损坏 tasks.json 不再视为阻塞） | migration_blocked_dir_keeps_source_tasks / _tasks_not_array_matches_ts | 损坏文件不报错 |
| read-tasks-error-aborts（EACCES/EISDIR 中止迁移而非视为阻塞） | migration_blocked_dir_tasks_eacces_matches_ts | fix-r1 P1 |
| non-atomic-task-write（原地写替代 temp+rename） | store_atomic_replace_keeps_old_inode_bytes_matches_ts | 非原子写 |
| migration-marker-renamed | migration_tasks_without_projectkey_stay_matches_ts | 迁移标记漂移 |
| list-runs-zero-limit-drains（limit≤0 清空） | store_list_runs_limit_0_matches_ts | slice(-0) 语义 |
| normalize-schedule-kind-ignored（不检查 schedule.type） | store_list_normalize_matches_ts | 归一化弱化 |
| relative-home-unresolved（入口不再绝对化 pilot home，同一目录相对/绝对双键互相覆盖） | migration_relative_home_same_dir_projectkey_matches_ts | fix-r1 P0 |
| concurrency-serialization-removed（put_task 不再持有每文件互斥，读改写交错丢任务） | store_concurrent_puts_across_threads_keep_every_task | fix-r1 P0 |
| stale-lock-enoent-errors（锁删除不容忍 ENOENT） | lock_removal_tolerates_a_vanished_lock_file | fix-r1 P2 |
| surrogate-lossy-reverted（严格 UTF-16 拒绝孤立代理 projectKey） | migration_lone_surrogate_projectkey_matches_ts | fix-r1 P3 |

迁移自身的 `atomic_write`（temp+rename）没有对应变异：绑定 `atomic-replace-keeps-old-inode-bytes` 的读 fd 观测只覆盖 store 写入路径，迁移路径的原子性没有可观测的 oracle 判据（迁移是单线程顺序写，重放树只比较最终字节），固不在负控范围。

```sh
python3 -I src/pilotdeck-gateway/tests/fixtures/cron-store/negative_controls.py --check
CARGO_TARGET_DIR=/absolute/build/dir python3 -I src/pilotdeck-gateway/tests/fixtures/cron-store/negative_controls.py /tmp/fresh-cron-store-negative [mutation ...]
```

**执行推迟**：本 worker 的 cron 源码未提交，`mutating_sources` 的 HEAD 守卫拒跑，实际变异由协调者提交后运行。本 worker 已用等价脚本（fix-r1 证据目录 `manual-mutations.py`）按同一序列手动验证全部 13 项：快照字节 → 应用变异 → `cargo test --no-run` 编译 → 被绑测试 `--exact --show-output` 实跑 → 恢复并逐字节校验，13/13 COMPILED 且 13/13 KILLED（panic 载荷含各自 `#1216` 断言消息；并发变异实测丢失 162/192 个任务）。

## 范围

只证明 cron 存储契约（路径解析、任务/运行历史/运行事件文件、跨目录迁移）；未接入调度器、会话运行时或 gateway method，默认代理路由不变。锁文件内容（pid/startedAt）与 marker 文案按 TS 原样记录；`CronStoreError`/迁移错误文案对拍 `String(caught)` 的 `{HOME}` 屏蔽形式，不声称与 V8 完全一致。
