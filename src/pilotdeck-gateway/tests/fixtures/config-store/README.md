# ConfigStore 的真实 TS 原件

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。
被测入口是基线 `src/pilot/config/PilotConfigStore.ts` 的
`createPilotConfigStoreSync`（`DefaultPilotConfigStore`：`reload`、`subscribe`、
`getDiagnostics`、`startWatching`）。Rust 对应 `src/config/store.rs`。

## 目录与条数

`oracle.json` 共 **15** 个场景（`count` 与 `scenarios.length` 一致，Rust 测试精确
断言条数）。每个场景是一串步骤（`steps`），`results[i]` 是第 i 步的真实结果：

- `create`：初始快照的 `version` 与 `getDiagnostics()`，或初始加载的错误；
- `reload`（`count` 次同步连发）：每次调用的结果（`version` 或错误的
  `name`/`code`/`message`/`diagnostics`），以及 `sameResult`（所有调用是否得到
  同一个快照对象 / 同一个错误对象）；
- `snapshot`、`diagnostics`：当时的 `getSnapshot().version` 与 `getDiagnostics()`；
- 每一步之后收到的订阅事件 `events`：监听者名、`previousSnapshot`/`nextSnapshot`
  的 version、`changedPaths`、`changeClasses`、回调内看到的 store version、
  `getSnapshot() === nextSnapshot`、回调内 `getDiagnostics().length`；可重入
  监听者另记其 `reload()` 是否得到同一快照。

覆盖：版本递增（失败不占号）、并发合并（3 连发、2 连发、失败连发）、
`PilotConfigError` 替换保留诊断而 `RangeError`（别名环）不替换、成功清空、
文件缺失、监听者抛错隔离、回调内 `reload` 并入当前 reload、发布中途订阅（被访问）
与退订（被跳过）、同一监听者重复订阅只算一次、项目配置增删、`startWatching`
去抖（30 ms 间隔三连写只触发一次）、失败的 watch reload 只留诊断、停止后不再
reload、缺失文件改监视目录、目录也不存在时不监视。

## 生成

溯源守卫与 `workspace-snapshot` 相同：`../../baseline_source_guard.mjs` 的
`bindSourceTree`（显式绝对 Git、固定 HEAD、每个 tracked `src` 字节对 Git blob），
加 tracked 树干净断言；`sourceSha256` 是 `PilotConfigStore.ts` 相对 import 传递
闭包（650 个文件）的 commit blob SHA-256。生成器断言 ICU 默认 locale 为
`en-US`。所有场景在一个 `mkdtemp` 根下各占子目录，只删除该根。

```sh
PDRS_BASELINE=/path/to/PilotDeck LANG=en_US.UTF-8 \
node --disable-warning=ExperimentalWarning \
  --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/config-store/generate.mts <输出目录>
```

watch 场景用真实 `fs.watch` 与真实计时器；写入间隔 30 ms、等待窗口为去抖的
数倍，连续三次生成逐字节一致。

## 归一化与回放

- 场景根目录记为 `<ROOT>`；`undefined` 成员记为 `{"$":"undefined"}`。
- 回放按步骤重建同键序的结果对象，序列化后逐字节比较（不排序键）。
- 非 `PilotConfigError`（如 `RangeError`）在基线上没有 `code` 与
  `diagnostics`；Rust `Error.code` 对这类错误是网关线协议码，回放按基线形状记为
  `undefined`、不记 `diagnostics`。
- 时间走 Tokio 暂停时钟：`wait` 推进虚拟时间，watcher 的轮询与去抖计时确定性
  触发，不依赖真实 sleep。Rust 以轮询（默认 20 ms）代替 `fs.watch` 事件，监视
  对象的选取（存在则监视自身、否则监视父目录、父目录也不存在则不监视）与基线
  一致，非 `NotFound` 的启动 stat 失败除外（保留该路径待首拍重探，#1792 r1）；
  同样的失败落在**父目录**的 stat 上时（配置文件缺失、走 `dirname` 回退），基线
  `existsSync` 对 stat 错误返回 false（Node v22.22.0 实测 EACCES 下为 false），于是
  仍 `watch(dirname)`；但该父目录不可读时 `watch()` 抛 EACCES，被基线的空 `catch`
  吞掉——**这条路径在基线上根本没有监视器**（只剩手动 reload），不会「监视上一级
  目录」。Rust 则保留父目录待首拍重探、探测恢复后上报期间错过的变更——这是
  **fixture 内登记、待差异账本收录**的差异（`specs/gateway-parity/` 现无 #1987/A3
  条目；#1987 A3 只登记不改语义，测试
  `startup_parent_probe_failure_watches_the_parent_dir` 钉住现行为并注明对齐 TS
  时恢复断言应变红）。
  本原件的 watch 场景写入间隔 30 ms，加一个轮询周期仍远小于 250 ms 去抖，结论不受
  下列差异影响。
- 与 `fs.watch` 不全等的三处（不加 crate 时无法消除，已用单测钉住现行为）：
  ① 延迟：去抖从**采样到**变化时起算，最多晚一个轮询周期；② 采样合并：两次变化
  的采样时刻落在同一去抖窗口内只 reload 一次，真实间隔略超 250 ms（默认 20 ms
  轮询下约 250–270 ms，随 tick 相位变化）也会合并，订阅者看不到中间快照
  （`change_spacing_around_the_debounce_window`：260 ms 间隔得「一次含第二次内容 +
  一次 identity」，300 ms 间隔与 TS 一致）；③ 净零突发：两次采样之间内容**和**
  元数据（含每次写入与 `utimes` 都会刷新的 ctime、inode）都恢复原状才不触发
  reload，而 `fs.watch` 会给一次 identity reload；#327 起同周期 A→B→A 并回拨 mtime
  已能看到（`restored_mtime_rewrite_between_samples_is_seen`），只剩「整个突发的 ctime
  都停在指纹记下的那个 tick 编号」这一格。每次采样先比对元数据（含每次写入与 `utimes` 都会
  刷新的 ctime、inode）：元数据未变则不读内容（#1739）。不完整采样不提交 metadata 指纹，
  按「什么失败」分两档（#1792 r1/r2）：条目或目录的 `NotFound` 是完整观测，缺失本身计入
  指纹；打开或读取配置文件失败把该条目内容哈希成 `None`，按指纹差至多报一次（#1792，
  `transient_read_failure_is_retried_until_the_content_is_seen`）；stat / `read_dir`
  的其它失败算被遮蔽采样，不提交任何指纹（含折叠指纹），只与上一遮蔽采样比较后上报可观测
  变化，读恢复后与最后一次完整采样比较、有差才报（#1792 r2，登记见
  `A6.3-CONFIG-POLL-001-R1.md`），同一不可读状态不会 reload 风暴、持续遮蔽也不会永久
  静默。#1987 收窄了这段口径的三格（启动采样不提交 metadata 短路 A2；遮蔽**过渡拍**
  另把 target 与最后一次完整采样比较 A1，与 R1 字面的差异待人批，见 issue #2094；
  metadata 拍遮蔽而同拍折叠拍完整时按完整观测处理 A4），细节见
  `src/config/store.rs` 模块文档。完整语义下漏检只有一格：改写后
  ctime 未跨 tick，且 len / is_dir / mtime / inode / dev 也都没变；精确表述见
  `src/config/store.rs` 模块文档。
  元数据变化后（或不完整采样重试时）才读被监视配置文件（含经父目录监视、后来出现的
  那个配置文件名）至多 1 MiB 内容，超出或其它目录项只看元数据。这一格按冻结程序登记为
  `specs/gateway-parity/diffs/A6.3-CONFIG-METADATA-SHORTCUT-001.md`（未批准），
  勘误见同目录 `A6.3-CONFIG-METADATA-SHORTCUT-001-R1.md` 与
  `A6.3-CONFIG-POLL-001-R1.md`；复算用挂载的 HFS+ 镜像把 `TMPDIR` 指过去，
  见该条目「机器判据」。
  相对的全局配置路径（`PILOTDECK_CONFIG_PATH`）按
  `StoreOptions.cwd` 锚定，与 loader 读的是同一文件。

```sh
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway \
  --test config_store --locked --offline
```

## 负控本地运行

`negative_controls.py` 在共享套件 runner（`negative_suite.run_cases`）上跑 52 个
源码变异（35 个 distinct 锚点；#1792 r2 新增 8 行：启动探测失败后的首拍补报、
遮蔽折叠指纹不提交、未分类模式不被 `NotFound` 定死、报变化拍不提交短路指纹、
目录被换成文件后的类型探针、持续遮蔽仍上报可观测变更、目录消失是完整观测、
`NotFound` stat 是完整观测；另有 #1899 的 6 行随 rebase 移植：停表证据退回
`is_finished()`、watch 循环首拍 panic / spawn 后干净的早退 / 首拍循环内退出
（drop 与 stop 两条路径）、以及永不停止——`#1650` 的静默死亡路径；#1946 再加 1 行：
claiming poll 必须让出调度、就地载入即杀；#1987 新增
6 行：遮蔽过渡拍不比较 target 的自身指纹、启动恢复两采样窗口（原行名
`startup-metadata-shortcut-committed`，#2122 更名 `startup-two-sample-window`——
替换文本只重排采样顺序、并不提交短路，旧名与旧描述超出实际覆盖面）、混合拍
误走遮蔽路径、启动即遮蔽把折叠值提交进完整指纹槽、`read_dir` 迭代中途错误被
吞、父目录启动探测失败丢路径；#2122 新增 4 行：第二遮蔽拍重复上报同一状态
（B6 测试的武装预算此前差一拍，第二条断言实际跑在完整拍上）、启动又提交
metadata 短路（A2 断言须在首拍 `changed()` 之前才能杀——报变化拍恒置 None）、
折叠拍内容未知的混合拍又走完整路径（守卫收成 `!= Complete`）、截断列表未走到
target 被当作 target 消失（未观测臂返回空折叠值）；#2122 fix-r1 再改锚新增
7 行：内容未知拍改由「unknown-read 孪生」门收口（`mixed-content-tick-committed`
行随守卫改动改锚孪生门，`&& false` 关门即恢复 F5 完整路径直落）、孪生门放宽
`&& true`（真实改写后的持续读失败被吞掉应报的那一次）、第二调用点遮蔽守卫放宽回
`!= Complete`（两采样间内容未知折叠进遮蔽链、欠报真实变更）、混合拍遮蔽守卫放宽
回 `!= Complete`（遮蔽链混进内容未知折叠、形状翻转误报）、报变化的完整拍不提交
孪生（其后的内容未知拍无基线可比对）、静默的内容未知拍不清短路（下一拍按相同
metadata 短路、读不被重试）、启动采样不播种孪生（未动磁盘的首拍内容读失败
误报——评审缺失格）；#1920 新增 3 行：watch 循环 panic / spawn 后
干净早退两形状重绑 stop 测试（marker 含 #1920 stop 报告断言）、`stop()` 改回吞
`JoinError` 恒报 Cancelled；#1920 fix-r1 再补 5 行：三种死亡形状补齐两半 drop
测试的交叉格（panic / 干净早退的 store 半、首拍循环内退出的 handle 半）、
`Ok(())` 臂误报 `Cancelled`（`Finished` 无正向用例即不可杀）、else 臂（task 被
take 后的 `stop`）删掉 debug_assert 沉默退回 `Finished`），名册分母是脚本里的
`EXPECTED_NAMES`
静态字面量（删行 / 改名 / 换序即拒绝开跑，不再用运行期表长），要求各绑定
测试自身 panic 载荷携带其行绑定的
`#1736` / `#1739` / `#1792` / `#1794` / `#1899` / `#1920` / `#1946` / `#1987` / `#2122` 杀点标记（绑定测试内另含 `#1793` 的
`MAX_POLL_INTERVAL == 60 s` 字面钉），每个变异前跑正控、后跑恢复正控，源码
逐字节恢复并复核。运行时要求：

```sh
CARGO_TARGET_DIR=<独立目录> LANG=en_US.UTF-8 \
python3 -I src/pilotdeck-gateway/tests/fixtures/config-store/negative_controls.py \
  <新的证据目录>
```

- `LANG` 前缀是 cb8 的 zh_CN 默认 locale 曾在 libtest 输出里产生假红，脚本内
  `env.setdefault` 只在调用方未显式选择时兜底；本地复跑请显式带上。
- `python3 -I` 不阻止字节码写入，脚本在导入共享 harness 前置了
  `sys.dont_write_bytecode = True`，不会在 fixtures 树里留下 `__pycache__`。
- runner 默认要求产品源码与 `HEAD` 逐字节一致；沙箱评审树无法 commit 时用
  `PDRS_NEGATIVE_BASELINE=$(git stash create)`（悬空提交，不动 ref、不改 index）
  钉住被审树，stdout 会打 `NON-HEAD BASELINE` 提醒这不是宿主证据。守卫同时比对
  worktree 与 index（`git diff --cached`，范围不止 `src`/`.cargo`/`rust-toolchain*` 被扫描区域，
  还包括 scanner 字面输入集——`src` 外也有），而 `stash create` 只快照 worktree——落在其中、未
  commit 的改动要先 `git add` 暂存，否则全行报 `source baseline mismatch`；比对范围之外的改动
  （如 `state/`）不参与该比对。字面输入集口径与冷、暖 scanner target 下该逃生口的可用条件
  统一见 `_negkit/README.md` 的「现状（#2106 实测）」段。
- 产物：每行 `-positive` / `-negative` / `-restored` 日志与 `summary.json`
  （`controls[]` + `counts`），全 `KILLED` 退出码 0。
