# #1261 原生 run() 入口调用点负控

`src/pilotdeck-gateway/src/native_process.rs` 的 `run()` 在捕获继承环境后调用
`validate_environment(&inherited)?`。#1231 的三个 `production-*-authority-accepted`
负控只变异函数体 / 键名；`production_entry_snapshot_capabilities_are_disabled`
自己直接调用 `validate_environment` 镜像组合，都不经过 `run()` 的调用点。删除或后移
该调用后，生产进程会先消费再（或不再）拒绝继承的 `PILOTDECK_STORAGE_ROOT` /
`PILOTDECK_SNAPSHOT_ROOT` / `PILOTDECK_MANAGED_CONFIG`，此前的测试与负控均观察不到。

绑定测试是集成测试 `native_entry_process_rejects_inherited_authority`
（`src/pilotdeck-gateway/tests/native_process.rs`）：用 `CARGO_BIN_EXE_pilotdeck-gateway`
起真实 `--native` 子进程（完整合法 CLI、`env_clear()` 后注入被测键），断言子进程非零
退出且 stderr 含 `native isolated mode does not accept inherited <key>`。子进程由
`kill_on_drop` + 显式 `kill`/`wait` 回收，stderr 经 `observe_child_stderr` 按行读，读到
**整行恰好是产品广播形状**立即返回：`native listening` + 一个空格 + 非空且不含空白的地址 +
换行，与 `src/native_process.rs` 的 `eprintln!("native listening {address}")` 一致（#1401 评审
P2：原先 `read_to_end` 撞 5 秒截止期，常驻子进程每次白等满 5 秒，慢主机又会被读成「没有
广播」）。观察有三重界：单行静默 60 秒、整体预算 180 秒，以及直接套在流上的 64 KiB
`take`——逐行超时会在每读到一行时重置，没有总预算则持续输出不匹配行的子进程会让测试永久
挂起（#1401 评审两家独立提出）；`read_until` 见到换行前会一直往同一个缓冲追加，只在读完后
检查字节数挡不住单行无换行的持续输出，所以上限必须套在流本身（#1401 评审 P0）。**没有读完
的行一律不算广告**：子进程只打出半行前缀就卡住或退出时，超时 / EOF / 字节上限分支返回
`false`（半行字节仍作为证据返回，供调用方自己的 needle 检查），scrub 测试因此不会把「喊了
半句」当成正控通过（#1401 评审 P0；换行也是形状的一部分——EOF 或 `take` 截断的残行不带
换行，`advertises` 直接拒绝）。读取函数 `read_advertisement` 对流与两个时限泛化，函数级
负控用字节切片、超长无换行行与 duplex 管道分别覆盖「完整广告 / EOF 残行 / 上限截断 / 静默」
四支，另加可控读错流与「持续输出非广告整行」两支，不再只有真实子进程一条路（#1401 评审 P0 / #1989 项 5）。调用点的两种破坏各有专属观察：删除调用时子进程会带着
被接受的权限常驻，stderr 出现子进程自己的 `native listening` 行即判杀（主机慢本身不是
证据）；后移调用时装配层的项目注册 / 运行清单会在拒绝前写入 `home`，测试断言 `home`
除自身写入的 config 外无新增条目。

判杀集合（MARKERS）只含上述三个键断言消息：launch / stderr 管道 / exit-wait 等
环境性失败一律落 INVALID。子进程早退分两支（#1401 项 1）：stderr **既无 rejection
也无 `native listening` 广告**（先于入口门退出，dyld/OOM/装配失败）时，测试用
**不带判杀标记**的 panic 报「子进程提前退出，无法判定」，只落 INVALID——环境性
早退不得记成 KILLED；stderr **已有 `native listening` 广告**再退出时，广播本身已
证明入口门接受了继承权限，测试用带键标记的断言判杀（KILLED）。测试自身的失败点均
带 `#1261 native entry ...` 专属消息（测试自建 scratch 目录，不再依赖共享
`test_scratch::Scratch`，#1401 项 5）。

#1401 项 6 为 `run()` 里 `options.environment(inherited)` 的调用点另绑一个测试
`native_entry_scrubs_inherited_process_environment`：`environment()` 会移除
`PILOTDECK_CONFIG_PATH`（`config::global_config_path` 里该键赢了 `<home>/pilotdeck.yaml`），
测试注入指向无效 YAML 的该键——正控下键被移除、子进程读测试写好的合法 home config
并常驻（见 `native listening` 行即正控结束）；变异 `let env = inherited;`（跳过 scrub）下
键保留、子进程读注入文件快速失败，stderr 必现 `Failed to parse default config YAML.`
（实测：错误转换丢弃携带 path 的 diagnostics，消息文本是唯一可观察证据）。两分支的
失败方向均为 INVALID（早退无该消息 / 静默无广播都只报无标记 panic），不引入
「慢主机 → KILLED」路径。

cargo 目标为 `--test native_process`（非 `--lib`）：`env!("CARGO_BIN_EXE_...")` 只在集成
测试编译期可用，且 `--test native_process` 会连带重建 gateway 二进制，变异体才被编进
真实子进程。已登记差异 `specs/gateway-parity/diffs/A8-NATIVE-SNAPSHOT-CAPABILITY-001.md`
记录了 TS 接受纯空白 storage 而 native 入口拒绝的隔离边界；第三个键
`PILOTDECK_MANAGED_CONFIG` 是 A8 snapshot 两键范围之外的 Rust 侧 authority。本负控只
锚定 Rust 侧调用点，不声称两者一致。

## 运行

`python3 -I negative_controls.py --check` 检查锚点与标记绑定（不依赖 cargo）。两个
`--check`（本脚本与 `early_exit_invalid.py`）的 marker 常量与每一根 needle 都放在同目录的
`native_entry_binding.py`：两张表此前各存一份且已经分叉（scrub「无法判定」针一张绑整句、
一张只绑 `if !advertised { panic!(`），#1989 项 6 合并成一处，各脚本只留自己的
`GATE` / pin 段 / `ANCHOR` / `ROWS`；`GATE` 自己也按整条守卫绑定（条件 + panic 头 + 消息），
短路条件或把 panic 降级成 `let _ = "…"` 都算漂移。绑定按**测试函数体切段**：rejects 测试里的
`observe_child_stderr(stderr)` 与三条判杀断言**整条调用**（`negative_assert!(!advertised,
"…"`、`!status.success()`、`leftover.is_empty()`），scrub 测试里的
`Failed to parse default config YAML.`、同一条广播观察、判继承键的断言整条调用
（`negative_assert!(!leaked_parse, "…"`）与它自己的「无法判定」分支
（`if !advertised { panic!("…"); }` 整条守卫）。整文件 needle 分不清哪个测试在观察，两个测试都含
`native listening` 后删掉 rejects 那处仍会通过；只绑片段也不够——三处断言复用同一句消息，
删掉其中一条、反转条件极性、或把它改写成 `let _ = (!leaked_parse, "…");` 都仍会通过，所以
needle 含调用头 + 条件 + 极性 + 消息；比较前用共享 `negative_source.squeeze` 折叠**字面量
之外**的空白，rustfmt 换行不影响绑定。

两个 pin 测试也一并绑：`native_listening_advertisement_shape` 绑两条形状断言整条调用（含
正反对照表里的尾部多余内容与 IPv6 输入），`native_listening_observation_needs_a_finished_line`
绑完整广告 / EOF 残行 / 上限截断 / 静默 / 读错 / 总预算六支的**输入与断言**，加上两条
`Instant` 上界（静默 50 ms 与预算 5 s 都必须在 1 s 内收口，把调用点两个实参互换变成红）与
`collected.len() <= CHILD_STDERR_LIMIT`；`advertises` 里的换行守卫按
`squeeze(test_body(…, 'advertises'))` 整条绑定（`if !line.ends_with(b"\n") { return false; }`），
短路与返回动作改写都算漂移——两个变异体都只打印拒绝行或一条完整广告，看不出形状被放宽成
前缀匹配，只有 pin 自己会红（#1401 评审 P2）。整条调用 / 整条守卫的 needle 咬不咬得住由同目录的
`test_native_entry_binding.py` 逐条变异证明（22 条漂移负控，含 scrub「无法判定」分支的
极性 / 消息两条与 `early_exit_invalid.py` 自己那根拒绝闸门的短路 / 降级两条；外加真源码满足
全部绑定、两个脚本不许再留私有副本——按 AST 收集赋值名，缩进、类型注解、同行多赋值都算——
与两处 `--check` 在真树上 rc=0，共 26 条）。纯输入 needle（IPv6、尾部多余内容、
`Failed to parse default config YAML.`、`!status.success()`、`leftover.is_empty()`）没有专属
变异，只由「真源码满足全部绑定」兜底——改动它们要真跑负控才能判；`squeeze` 的**词法切分**（字符字面量 / 普通串 / raw string /
字节与 C 串 / lifetime / 行与嵌套块注释，注释整段丢弃但仍分隔 token）由
`../test_negative_source.py` 的碰撞反例覆盖（#1989 项 1–4）。切段函数由两个脚本共用
`negative_source.test_body`，在函数自己的收尾 `}` 处截断，因此最后一个测试之后的辅助
函数不会被吞进来，`#[tokio::test(flavor = ...)]` 这类属性也不会被漏认。实际负控命令
为 `python3 -I negative_controls.py NEW_OUTPUT [mutation ...]`，必须由调用方设置
`CARGO_TARGET_DIR`，证据目录走共享 `new_output_directory`（`#953` 非空拒用、
`.claim` O_EXCL），使用共享 HEAD 守卫、进程所有权和最终汇总：每项先正控，再
`--no-run` 确认变异体可编译，再判杀，恢复后复跑；过期锚点与无法编译都是 INVALID，
任一非 KILLED 即非零退出。未提交源码不能运行（`mutating_sources` 拒跑），不得使用
副本仓库绕过；由协调者在提交后的宿主树上运行。

`python3 -I early_exit_invalid.py NEW_OUTPUT`（同样必须设置 `CARGO_TARGET_DIR`，
`--check` 为静态检查）证明早退分支的两半：它把 `CARGO_TARGET_DIR` 里已构建的
`debug/pilotdeck-gateway` 原子换成 stub（临时名走 `tempfile.mkstemp`，#1401 项 2），
跑对应的绑定测试并用共享 `negative_suite.classify` 判定——无输出 stub（立即 `exit 86`）
必须 INVALID 且命名测试的 panic 载荷含该测试自己的早退分支消息（rejects / scrub 各一
行）；`native listening` stub（先向 stderr 打印 `native listening 127.0.0.1:1` 再
`exit 86`）必须 KILLED 且载荷含绑定 marker。每行变体的原始输出单独落
`early-exit-neg-<行名>.log`，互不覆盖。正控与恢复后复跑对两个测试各做一次，随后按
SHA-256 校验恢复原二进制。stub 窗口改写的是共享 `$CARGO_TARGET_DIR/debug` 路径，整段
运行持 `negative_process.tree_lock(ROOT)`（与 `mutating_sources` 同一把树锁），别的负控
既不能在窗口内发布自己的二进制，也不能把 stub 当成自己的产物（#1401 评审两家独立提出）。
汇总行只发布共享比对器白名单内的字段（`stubRestored` 这类内部记账不外发，#1401 评审
P1）。证据目录走共享 `new_output_directory`（`#953` 非空拒用、`.claim` O_EXCL，
#1401 项 3）。它不变异任何源码，也不经过 `mutating_sources`，所以未提交的树（含本
fixture 所在测试文件的改动）上也能运行。任一环节不符即非零退出。
