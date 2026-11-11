# #1253 append 增量与 reload 重试

固定 TS 基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，Node v22.22.0 / ICU 78.2。`generate.mts` 直接调用真实 `SessionProjectionDriver` 与 `AgentTranscriptProjections`，逐事件比较实时 drive 和从日志构建的投影，并保存完整输入、输出及来源闭包哈希。`context_snapshot` 等日志含 legacy/out-of-turn 记录，矩阵针对投影契约；不能把这组直接 drive 结果说成 recorder 域校验通过。

`oracle.json` 有 interleaved / compaction / restore 三组，每步附 expected durable state；另有 `delivery`（真实 TS canonical replay / hydrate 后迟到消息，以及 appendRecorded 重复/过期序列拒绝）与 `checkpointMetadata`（真实 checkpoint 后 metadata/modelSelection 补放），两组由 Rust 消费。Rust 内部 durable 通知去重用相同 canonical 状态作不双计不变量，不将它等同于 TS 重复提交 API。另用真实 `SessionRuntime`、订阅门闩与 AbortController 观测 before / held / cancelled / settled。取消等待者不取消已经入队的 append。`concurrency` 仅交叉观测、非 Rust 对拍判据。门闩最多 10 秒，未释放前驱的观察与清理释放分开；没有网络、provider、用户 home 或真实运行会话调用。

## 产品边界与保留理由

|事件 / 状态|本单处理|理由与代价|
|---|---|---|
|accepted_input|追加消息与合法 modelSelection|不重新读取历史；只克隆本次消息|
|assistant_message / tool_result_message / durable_message|以 completed 索引判断可见性，跳过 compactReplacement|普通 append 不扫描历史；已完成 turn 的迟到消息同样处理|
|context_snapshot|完成后插到最近真实 user 前|不重放日志，但定位和 Vec 插入仍为 O(messages)|
|metadata|原地合并|去掉旧代码对所有历史 messages 的 clone|
|无关事件 / 非 compact 边界|保持 durable state|仍将原始 entry 保存到日志与投影日志|
|turn_result|保留全量重放|完成可能激活任意旧 turn 的历史消息、context；重复结果也可能累加 usage/denials，不能只 append 当前消息|
|compact / compact_boundary|保留全量重放|最后一个有效替换边界会改变此前所有消息的可见性；合法快照和 legacy replacement 的完成条件不同|
|valid=false|保留全量重放直到恢复|后续压缩可能遮蔽坏消息；不能让无关增量洗掉错误，也不能永久标坏|
|restore|重放并按最终合并 entries 重建 completed 索引|checkpoint 只支持原有两行；不扩大 checkpoint 格式或冻结语义|
|capture / invalid snapshot|保留全量重放|capture 要从同一 writer 版本的 durable entries 生成 checkpoint；snapshot 仅在 valid=false 时重放以返回原始错误或确认恢复，不能拿 last_valid 当当前状态|

这里不是“所有非 metadata 事件均 O(1)”：turn_result/compact/错误恢复保留历史重放，context 插入仍随消息数增长。TS 的 conversation reducer 本身也保存 entries，finalize 仍扫描历史；该事实可从本 fixture 的固定基线与来源哈希复现。缩围直接对应 #1253 正文允许的“登记不改理由”。

## #1011 / PR #1175 更正依据

PR #1175 的“去掉 8 次一致性重试”不符合当前实现，应更正为：**metadata append 改增量；reload 的 8 次有界一致性重试保留，checkpoint capture 的 3 次重试是另一条路径。**

`ReloadSource::snapshot_consistent_inner` 先后读取 writer 状态，中间取得 projection 与 file_state。调用方 `runtime/turn/live.rs` 的 adapter 锁不排斥其他 recorder 写入，file_state 回调也允许同步追加。直接删掉重试会返回混合版本；把 writer 锁跨回调持有会死锁。因此本单保留已有保护，补稳定 / 一次真实并发追加 / 八次冲突耗尽测试。这里是 Rust 内部安全理由，不宣称 TS 的同步 snapshot 有相同重入 API。协调者应将本段更正同步到 #1011 与 PR #1175。

## 第三条：tail 生成器工作目录

`turn-runner-tail/generate.mts` 原本创建 `run-*` 而不清理，同时将 collector 临时数据写在 OUT/collector。本单把 collector 纳入本次 mkdtemp 根，并在 finally 中只清理该根，保留 raw 报告与 oracle。raw 报告内已有 transcript 原字节与 SHA256；其中记录的工作路径是历史路径。正控覆盖成功和 oracle 独占创建冲突两种退出、外来 run-* 与 collector 保留；负控覆盖漏清理、宽泛删目录及 collector 写回 OUT。`session-factory/file-snapshot-test.mts` 同样在 finally 中清理自己的 snapshot-contract 根；`probe.mts` 独立运行时自清理，完整 factory 生成器则预先分配并持有各 probe 根，等 `check.py` 的物理库存/字节审计结束后统一 finally 清理。父子不按目录前缀批量删除，外来目录与 raw/oracle 保留。正控覆盖独立 probe 成功/输出冲突、snapshot 探针、完整生成器成功/输出冲突，并比较 factory 与 tail oracle 原字节。

#1281 补齐失败路径：`file-snapshot-test.mts` 提供 `PDRS_1253_SNAPSHOT_FAIL=1` 失败 hook（body 在 mkdtemp 后抛错），`generate.mts` 的子进程 probe 经预置 `raw/index.json` 与 index 写入冲突而失败，两格均断言 owned 根清空、外来目录保留；负控覆盖「snapshot 清理仅在成功时执行」与「probe 根清理仅在子进程成功时执行」。`projection-cache/generate.mts` 的 `work` 清理从文件末尾移入 finally，`generator_cleans_owned_work` 扩为成功 + `oracle.json` 冲突两格（断言两格均无 `work-*`、外来 oracle 保留），负控覆盖「work 清理仅在成功退出执行」。

原始 phase2 RESULT 不在本树引用资料中，问题归因依据为当前行号与 `73c9d952` 的 #1011 同类清理修复，未伪称读到原件。

## #2116 `.generation-owner` 判据与手工负控

`generate.mts` 的 `.generation-owner` 是对 OUT 的首个 `wx` 写入（早于 raw 报告与 oracle 等一切输出），身份串 `session-factory/append-delta\n`：OUT 已被外来 marker 标记时，生成器在写出任何输出前于该独占 open 处以 EEXIST 退出，外来 owner 字节不被覆写。本夹具不分配 `run-*` 工作根，marker 在此不承载清理所有权，承载输出目录的跨生成器独占。

重生成守卫 `append_delta_regenerates_identically` 经共享 helper 只比较 `oracle.json` 字节，marker 不进入比较面（#1500/#2116 口径）；marker 判据由专用用例 `append_delta_generator_writes_owned_marker`（`runtime::session::factory::projection_delta_tests`）承载：成功格钉 marker 存在且内容为身份串，冲突格预置外来 `.generation-owner`，断言失败点落在独占 open（输出同时含 `EEXIST` 与 `.generation-owner`）且外来字节保留。

按 #2116 两条 issue 评论的结论，本轮不为这些 `#[ignore]` marker 断言登记自动化负控变异体（不扩 `negative_controls.py` 的 37 项表）；判据强度由交付证据中的手工负控留档保证：`{flag:'wx'}`→`{flag:'w'}`、marker 写入改 no-op、身份串换邻居夹具（`session-factory`）三只 `generate.mts` 变异各使绑定断言 FAILED（KILLED），恢复后复跑通过。复跑正控的钉版环境（Node v22.22.0 / ICU 78.2、`PDRS_BASELINE` 指向 `cc7c4e65` 检出、从仓库根加 `--manifest-path src/Cargo.toml`）统一见上级 [`fixtures/README.md`](../../README.md) 的「手工负控钉版环境（#2304 条 3）」段；`test result:` 行须为 `1 passed; 0 failed`：

```sh
cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib \
  runtime::session::factory::projection_delta_tests::append_delta_generator_writes_owned_marker \
  -- --exact --include-ignored
```

手工负控复跑：对 `generate.mts` 的 marker 写入行分别施加上述三只变异，同命令复跑须 FAILED 于对应 `#2116` 绑定断言（`test result:` 行含 `1 failed`）；每只跑完 `git checkout -- <generate.mts>` 恢复并核对工作区干净，复跑正控须回到 `1 passed; 0 failed`。

#2304（条 2）：本用例的重复形状（Scratch 守卫、命令组装、双格断言、首写 listing、末尾显式 remove）已收敛到共享 `tests/common/baseline.rs::generation_owner_marker_cell`，断言语义与顺序不变，本节复跑口径不变。

## 复现

调用方设置 `PDRS_BASELINE` 指向只读固定基线，PATH 的 node 必须满足版本与 ICU；`CARGO_TARGET_DIR` 使用本 worktree 独占目录，debug profile 两项设为 `line-tables-only`。从仓库根运行：

```sh
scripts/cargo-slots.sh
cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib \
  runtime::session::factory::projection_delta_tests -- --include-ignored
python3 -B src/pilotdeck-gateway/tests/fixtures/session-factory/append-delta/negative_controls.py --audit
```

基准 `non_metadata_append_benchmark` 分事件和 n=256/512/1024 输出，不以耗时判 CI；所有输出另外与完整 replay 核对。原有 `projection_cache_append_benchmark` metadata 基准保留。TS 重生成守卫为 `append_delta_regenerates_identically`；tail 另复用原有 `runtime::turn::runner::tail::tests::regenerates_identically`。

负控在交付提交后运行 `negative_controls.py <独占OUT>`。执行器先逐项验证锚点、应用变异并 no-run 编译，再跑正控/负控/还原正控，最后汇总 KILLED/SURVIVED/INVALID；任何非 KILLED 均非零退出。`mutating_sources` 覆盖产品与判据输入，来源默认必须是 HEAD；置于 `PDRS_NEGATIVE_BASELINE` 的沙箱候选快照可作为受支持的例外（冷/暖 target 语义统一见上级 [`fixtures/README.md`](../../README.md)「共享输入扫描器」节，#2305 条 5 同步），不建影子仓库。未提交交付在沙箱只能做静态锚点核验，编译和判杀交协调者宿主。整组可能超过 20 分钟；用 `negative_controls.py <独占OUT> --only <变异名>` 跑单样本。本轮共 37 项（以 `negative_controls.py --audit` 打印的条目数为准）；`.mts` 项的 no-run 只编译 Rust 测试载体，TS 语法/运行错误由测试执行阶段判 INVALID，不能称为 TS 编译通过。

第四只变异（#2304 条 5）：在 marker 写入前落一个输出写（等价于把 marker 后移到首个输出写之后），绑定用例 FAILED 于冲突格首写 listing 断言「collision cell writes nothing before the exclusive marker open」，`git checkout --` 恢复后复跑回到 `1 passed; 0 failed`——三只已登记变异都死在更早断言，该 listing 断言由此获得自己的负控留档。
