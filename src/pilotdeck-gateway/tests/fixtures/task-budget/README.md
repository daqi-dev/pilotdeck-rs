# SDK task budget 真实 TS 夹具

固定基线 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。出处：`src/cli/ProjectRuntimeRegistry.ts:125-139,1375-1540,2206-2256`；源码 SHA-256 记录在 `oracle.json`。生成器检查基线 HEAD 与该文件无改动，再在内存中追加 helper export，编译完整原文件并从原位置解析依赖。基线磁盘文件不修改、实现不复制。

通过 `Object.create(ProjectRuntimeRegistry.prototype)` 调用真正的持久化方法，注入 Map、可控时钟、临时 pilotHome，以及只返回 projectRoot 的 resolve seam；避免创建项目运行资源。运行配置校验、会话管理、AbortSignal 和 gateway 接入不属于本夹具范围。结果中的 `ok: true` 是将 TS void 返回归一化为可记录的成功标志。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/task-budget/generate.mts src/pilotdeck-gateway/tests/fixtures/task-budget
```

需 Node 22 与固定基线已安装的 tsx/esbuild，沿用 `../operation-ledger/README.md` 的环境。生成器不会安装依赖。重复执行两次 `oracle.json` 必须逐字节一致；文件中的固定 projectRoot 仅为账本身份，不创建对应项目目录。所有真实文件操作只发生于生成器创建的临时 pilotHome。

`oracle.json.scenarios[]` 包含初始文件全文、每步输入/返回值/有效记录计数/账本全文、最终文件重载后的状态，以及 TS 继续结算的文件全文。初始正常数据来自实际 TS 写入；坏行及兼容性案例是对真实记录的明确变异，期望结果仍全部来自 TS 执行。生成器对关键分支加了命中断言，避免引用错位导致空跑。

| 场景组 | 覆盖 |
|---|---|
| normal / project | session 与 project、无预算/非法费用无操作、重复结算、0 费用、配置 total/retention 冲突、跨 session 项目共享、清除不误删项目账本、过期前一毫秒/等于边界、重复配置与重复结算不续期、超期后 settle 刷新时间 |
| compact / empty-compact / default-threshold | 四种行格式、字段顺序、缺省字段省略、Map 插入顺序、UTF-16 runId 排序、空 snapshot 文件、默认 512 阈值 |
| clear-before-load / clear-before-load-compact / deferred-load-compact | TS 的 clear 不先加载、加载时新 clear 再计数、低阈值首次 clear 整理为空、读入达到阈值只在下一次 append 整理 |
| replay-duplicate / no-final-newline / bad-lines | 有效重复行计数但不累加、完整无换行尾行、坏截断尾行、未知/空白/非法 JSON 行、append 不补前置换行及重启后行为 |
| snapshot-merge / max-activity / snapshot-clears-metadata | snapshot 覆盖与 Set 去重、configured 分字段 `??=`、settled 活动时间取最大值、snapshot 缺失元数据清除旧值 |
| optional-metadata / nullish-configure / invalid-metadata-expiry / primitive-* / load-primitive-error / invalid-total / orphan-activity / validation | 宽松可选元数据、null/字符串/对象、负历史费用、NaN 比较导致过期、非数字 total 的错误、toString 遮蔽导致数值转换抛错及加载失败缓存、无 retention 时整理丢弃孤立 activity、无效类型行不计数 |
| path-and-unicode / numbers / fixed-rounding | 词法路径归一化、中文/孤立代理/astral ID、IEEE-754 累加、极小/极大值与溢出、JS toFixed(6) 的中点舍入 |

Rust 默认 `tests/task_budget.rs` 回放每步并核对结果、计数与文件原字节，另直接加载 TS 文件核对状态。文件替换测试持有旧 inode，验证先 append 再同目录临时文件 rename；错误测试验证加载失败缓存和写失败前内存已更新的 TS 行为。

双向交叉测试需显式运行：

```sh
cd src
PDRS_BASELINE=/absolute/path/to/PilotDeck cargo test --locked --offline -p pilotdeck-gateway --test task_budget -- --include-ignored --nocapture
```

该测试先由 Rust 回放生成所有场景文件，真实 TS 再逐个读取并验证恢复状态/计数，成功恢复的场景继续结算并核对完整字节，坏元数据场景核对缓存的加载错误；最后 Rust 重开读取 TS 新写的结算。TS 验证模式不把 Rust 输出当作期望，使用已录制 oracle。

范围限制：没有 runtime 接线（A6.3-pr8），没有多进程写者互斥（基线本身没有锁），没有进程崩溃/断电注入。可选元数据的极深数组数值转换复用现有 `js_value` 深度上限（2000），与 V8 的调用栈相关上限不承诺完全相同；常规 JSON 数据与已录场景逐字节一致。
