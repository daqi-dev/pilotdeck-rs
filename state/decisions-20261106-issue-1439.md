# 决策卡 #1439：`cleanup_expired` 的调度点（AI 代批，待人复核）

- 日期：2026-11-06
- 状态：**AI 代批，待人复核**（可撤回、在意向范围内、不动尺子）
- 关联：issue #1439（P2-跟进，来源 PR #1418 闸门 `_tmp/20261105-issue-1321-gate/concurrency.md` 第 4 条）
- 分诊：`_tmp/20261106-issue-1439/triage.md`

## 卡在哪

`UploadStore::cleanup_expired`（`src/pilotdeck-gateway/src/upload/mod.rs:409`）在原生路径上**没有执行者**：上传 TTL（24h）与租约 TTL（6h，崩溃孤儿）无人回收。TS 的调度在 `resources/PilotDeck/ui/server/routes/uploads.js:73-76`：`setInterval(…, 15*60*1000)` + `unref()`。Rust 侧没有既有「进程级周期任务」惯例可以照搬（`config/store.rs:645` 的 `watch_loop` 是对象自带 `Weak` 的循环，持有关系不同），存在多种接法且语义不同——按决策分级属「自决 · 待复核」。

## 谁受影响

- 长驻原生进程的磁盘：`.tmp/chat-uploads`（24h TTL）与 `.tmp/chat-upload-leases`（6h TTL，硬链接钉住已删文件块）。
- 不做时**没有任何用户可见错误**，只是无界增长；异常路径（进程崩溃 / `resolve_attachments` 的 `spawn_blocking` panic）留下的租约永久残留。
- 混合部署下 TS ui/server 与原生进程同跑时，TS 侧仍会清同一目录（两侧共享磁盘格式）；但原生单独跑时无人清。

## 能否撤回

能。最小实现是一个 15 分钟常量 + 一个 `spawn_cleanup` 函数 + 装配层一行调用；删除即回到现状，不产生磁盘格式变化、不改变任何已有 API 语义。

## 动不动尺子 / 意向

不动。不碰冻结文件、`specs/`、差异账本、任何对拍判据；不新增 CLI/env 开关（TS 无此开关）。

## 选项与后果

| 选项 | 做法 | 后果 |
|---|---|---|
| **A 装配层周期任务（选）** | `native_process/assembly.rs` 起一个 detached `tokio` 任务：15 min 一次，`spawn_blocking` 调 `cleanup_expired`，失败 `eprintln!("[uploads] cleanup failed: …")`；`Weak` 持有 store，store 全 drop 即退出 | 与 TS 同节奏同语义（首扫在 t=15min、不阻塞关停、失败继续）。代价：多一个常驻任务（15 min 一次空转，成本可忽略）；与在途 PR #1561 同改 `assembly.rs`（不同段落，rebase 可解） |
| B 惰性清理 | 每次 `upload_*` 请求或附件解析前顺带跑一次 | 无新任务、无生命周期问题；但**空闲进程永不清**（正是本 issue 的无界增长场景），且把 O(上传数) 的目录扫描塞进请求热路径。语义偏离 TS |
| C 挂在 `TurnGateway` host 线程 runtime | 在 `start_with_cleanup` 的 build 闭包里 spawn | 生命周期天然绑 host；但清理与 turn 串行宿主耦合，host 重启会重置节奏，且清洁任务与 turn 抢同一个 current_thread runtime。语义比 A 更偏 |
| D 不做，登记差异账本 | 写明「原生侧由 TS ui/server 负责清理」 | 零成本；但原生独立运行时目录无界增长，且与 #1321 的「原生接线」目标相反（该闸门条目本来就是把这条列为待修） |

## 建议（AI 代批）

**A**。理由：唯一与 TS 逐条对齐（周期 / 首次时机 / 失败处理 / unref 语义）、改动面最小（一个函数 + 一行）、可撤回；B/C 都偏离 TS 或把清理耦合进热路径。实现细节与验收（含红转绿负控）见 `_tmp/20261106-issue-1439/extra.md` 与 triage §5。

## 同文件串行核验（2026-11-06）

开工时 open PR 里只有 #1561 同改 `native_process/assembly.rs` 与 `native_process/tests.rs`。实测合并（`git merge --no-commit` #1561 head `17f8d9a72`）自动合并成功、两处改动并存（`storage_stimulus_switch_matrix` 与本单装配用例同在 `tests.rs`），故不另选接法；结论与命令记在 PR 正文。

## 复核要点（待人）

1. 15 分钟常量是否按 TS 原值（`15 * 60 * 1000`）——是。
2. detached（不 await / 不 abort）是否等价 `unref()`——是：runtime 丢弃即停，关停路径不等待。
3. 是否应该两边都跑（TS + 原生同跑时会出现两个清理者）——TS 与原生本就是两个进程共享磁盘格式；`cleanup_expired` 对已删条目幂等（`NotFound` 视为成功），重复清理无副作用。若后续由 TS 单侧负责，删掉本任务即可。
