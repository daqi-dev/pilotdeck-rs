# Cron store TS fixtures

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，被测源码 `src/cron/storage/CronPaths.ts`（42 行）、`CronTaskStore.ts`（250 行）、`CronStoreMigration.ts`（472 行），整个基线 668 个文件的 SHA-256 清单记在 `oracle.json`。所有路径、步骤结果与落盘树都由真实 TS 类（`resolveCronPaths` / `CronTaskStore` / `migrateCronStores`）产生，无实现源码复制；环境准备（Node v22.22.1——基线要求的 v22.22.0 在本机不可用，已用补丁版本并在 RESULT 披露、基线 lockfile 的 tsx 4.22.3、`PDRS_BASELINE`）同 `../operation-ledger/README.md`。生成器启动即断言 `new Intl.Collator().resolvedOptions().locale === 'en-US'`，非 en-US 环境直接失败。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/cron-store/generate.mts /tmp/fresh-cron-store-output
```

生成器先断言基线 HEAD，再把无参时钟钉在 `Date.UTC(2026, 0, 1)`（`installFixedClock`，同 `../_shared/generator_guard.mjs`；marker 的 `completedAt`、锁新鲜度判定都由该纪元决定，锁种子的 mtime 也按 `Date.now() - age` 相对该纪元回拨）。输入文件逐场景独占写入（`flag: 'wx'`，目录递归创建），`oracle.json` 独占发布，输出目录必须尚不存在。迁移场景记录一次执行的树，`twice` 场景在无错时用同一 logger 再跑一次并记录第二次的树；警告元数据与错误文案中的场景 home 替换为 `{HOME}`，使 oracle 与输出目录无关。

矩阵形状固定为 **9 + 10 + 33 + 18**（paths / eventsPaths / store / migration）。`generate.mts` 在发布 `oracle.json` 前、Rust `oracle_matrix_counts_match_generator` 在重放前都精确断言这四个数，增删场景必须同时更新两侧断言及本段。Rust 侧每个 cell 恰好一个具名测试（`src/pilotdeck-gateway/src/runtime/cron/tests.rs`，72 个测试函数：69 直跑 + 2 个 locale 门控父测试 + 1 个 `#[ignore]` worker），逐 cell 比较记录结果与落盘树（文件 base64 解码后逐字节，目录集合逐一相等）。

两个 cell 的字节依赖非 ASCII 文本的 `localeCompare` 排序（`store:put-sort-missing-and-collation`——`ä` 对 `z`/`Z`，`migration:duplicate-runs-dedupe`）：Rust 通过 `crate::test_process::run_cases` 在 `LANG=en_US.UTF-8` 的子进程里重放（env_clear，`--exact --ignored --test-threads=1`，`PDRS_CRON_CASE=section:cell` 分发），与生成器自身的 en-US 断言同一契约；worker `cron_locale_worker` 只被子进程拉起。

```sh
cd src
cargo test -p pilotdeck-gateway --lib runtime::cron          # 71 passed; 1 ignored
PDRS_BASELINE=/absolute/path/to/PilotDeck cargo test -p pilotdeck-gateway --test cron_store -- --ignored
```

锁等待时长是注入参数（`MigrationInput::lock_retry_delay`）：TS 基线按 100 次 × 100ms 退避（生成 fresh-lock cell 时真实等约 10 秒），重放用 1ms 延迟（约 100ms）保持套件秒级，超时文案与尝试次数不受影响。

`oracle.json` 各段：`paths[]`（8 个字段）、`eventsPaths[]`（`runId` + 期待路径，含孤立代理 runId）、`store[]`（`home`/`projectKey`/`initial[{path,text|bytes}]`/`steps[{op,...}]`/`results`/`final{files(base64),dirs}`）、`migration[]`（`home`/`input`/`warnings`/`error`/`once`/`twice`）。重放要点：JS `slice(-Math.max(0, limit))` 在 `limit ≤ 0` 时因 `-0 === 0` 保留全部记录（limit-0/negative cell 存在的意义）；`readFile utf-8` 有损读取、`JSON.parse` 拒绝 BOM（解析失败按空任务文件处理）；写走 temp+rename，`EPERM`/`EACCES` 回退 copy；`normalize` 要求 `schedule.type === "once"/"cron"` 且分别带字符串 `runAt`/`expression`；迁移 marker 是普通写（与 store 的原子写有意保持 TS 原样的差异）；陈旧锁（>10 分钟）删除并告警，新鲜锁按 1 秒退避重试 100 次后超时报错。

| 家族 | cell |
|---|---|
| paths（9） | absolute、dot-segments、trailing-slash、repeated-separators、slug-fold、slug-root-empty、backslash-and-dashes、unicode-slug、collapse-runs |
| eventsPaths（10） | plain、space-and-slash、whitespace-only、empty、dot-runs、cjk、lone-surrogate、edge-dashes、collapse-runs、mixed-allowed |
| store 读取（7） | list-missing、list-corrupt、list-bom、list-empty-object、list-tasks-not-array、list-schema-version-2、list-normalize |
| store 单任务/写入（9） | get-found-and-missing、put-new、put-new-with-optionals、put-same-key-replaces-and-sorts、put-sort-missing-and-collation†、put-after-corrupt-starts-empty、replace-missing-writes-nothing、replace-found、update-modify |
| store 变更（5） | update-remove、update-missing、delete-found、delete-missing、concurrent-puts-serialize |
| store 原子性（1） | atomic-replace-keeps-old-inode-bytes |
| store 运行历史/事件（11） | append-run-new、append-run-twice、list-runs-missing、list-runs-mixed、list-runs-limit-0、list-runs-limit-negative、list-runs-limit-2、list-runs-limit-large、list-runs-invalid-utf8、append-run-event、append-run-event-sanitizes |
| migration 目录/路由（4） | no-cron-dir、empty-projects-dir、tasks-without-projectkey-stay、tasks-move-to-projectkey-dir |
| migration run 历史（5） | runs-by-explicit-projectkey、runs-by-unique-task-project、runs-ambiguous-task-stay、blocked-dir-keeps-source-tasks、blocked-dir-tasks-not-array |
| migration 合并/幂等（7） | invalid-run-lines-preserved、run-events-merge-and-move、duplicate-tasks-dedupe、duplicate-runs-dedupe†、missing-tasks-file-is-writable、non-directory-entries-skipped、migration-twice-is-idempotent |
| migration 锁（2） | stale-lock-removed（11 分钟陈旧）、fresh-lock-times-out（60 秒新鲜锁） |

† locale 门控。

## 负控

`negative_controls.py` 复用 `../negative_process.py` 和 `../negative_suite.py`，7 项变异各绑定一个具名 cell 重放测试，判杀证据是该测试自身 panic 载荷携带的 `#1216` 消息；编译失败、正控失败、锚点失配、恢复失败均为 INVALID，任一非 KILLED 即非零退出。守卫覆盖 `store.rs`、`migration.rs`、`tests.rs`、脚本自身与两个共享裁决依赖，任一脏即拒跑；调用方必须提供 `CARGO_TARGET_DIR`（本树构建目录）。

| 变异 | 绑定测试 | 对应问题范围 |
|---|---|---|
| migration-routing-removed（projectKey 路由不再移动） | migration_tasks_move_to_projectkey_dir_matches_ts | 迁移跳过 |
| corrupt-tasks-parse-error / corrupt-tasks-not-array（损坏 tasks.json 不再视为阻塞） | migration_blocked_dir_keeps_source_tasks / _tasks_not_array_matches_ts | 损坏文件不报错 |
| non-atomic-task-write（原地写替代 temp+rename） | store_atomic_replace_keeps_old_inode_bytes_matches_ts | 非原子写 |
| migration-marker-renamed | migration_tasks_without_projectkey_stay_matches_ts | 迁移标记漂移 |
| list-runs-zero-limit-drains（limit≤0 清空） | store_list_runs_limit_0_matches_ts | slice(-0) 语义 |
| normalize-schedule-kind-ignored（不检查 schedule.type） | store_list_normalize_matches_ts | 归一化弱化 |

```sh
python3 -I src/pilotdeck-gateway/tests/fixtures/cron-store/negative_controls.py --check
CARGO_TARGET_DIR=/absolute/build/dir python3 -I src/pilotdeck-gateway/tests/fixtures/cron-store/negative_controls.py /tmp/fresh-cron-store-negative [mutation ...]
```

**执行推迟**：本 worker 的 cron 源码未提交，`mutating_sources` 的 HEAD 守卫拒跑，实际变异由协调者提交后运行；本 worker 已手动完成编译验证（逐项应用 → `cargo check --tests` → 字节恢复并校验哈希一致，7/7 COMPILED，未对变异体执行任何测试）。

## 范围

只证明 cron 存储契约（路径解析、任务/运行历史/运行事件文件、跨目录迁移）；未接入调度器、会话运行时或 gateway method，默认代理路由不变。锁文件内容（pid/startedAt）与 marker 文案按 TS 原样记录；`CronStoreError`/迁移错误文案对拍 `String(caught)` 的 `{HOME}` 屏蔽形式，不声称与 V8 完全一致。
