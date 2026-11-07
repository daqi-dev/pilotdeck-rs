# 远端 TUI 的真实 PTY 冒烟

这些脚本由 mbp2 执行：使用编译出来的当前 Rust `pilotdeck`、固定 Node 与固定 TS 树里已安装的 `ws` 作为本地 fake 服务；每次要求不存在的专属输出目录，使用动态 loopback 端口、独立 home/project、虚构 token，记录原始终端、双向帧、退出和终端模式。运行参数分别见脚本 `--help`。`smoke.py` 含 `--small`；`scenarios.py` 包括权限、取消、history、流和资源预算，以及 `abort-late`、`abort-timeout` 与 `throughput`；`backpressure.py` 停止读取 PTY；`output-pipe.py` 验证非终端输出明确拒绝。假服务不代表真实 provider/native 权限闭环，不是阶段验收。

吞吐回归前的原始失败保存在主仓 `_tmp/20261108-cli-tui/throughput-r1/out/` 和 mbp2 `/Users/da/ws/pdrs-worker-wt/control-cli-20261108/_tmp/20261108-control-verify/throughput-r1/out/`：100 个小 delta/秒时仅到 seq 147 便关闭连接、25 秒未收到 final；result SHA-256 `8c7b06fb969d8e117cff48a477bef40ee975cbc50f982c1d1f69334eec0059d6`，wire SHA-256 `0d7dd630af4a196282872be5cd4f13d3edcd42bf91ce7c6481f7d5db6d0c9762`，terminal SHA-256 `ea2731dfcfc69b8d1c1fd292c79c02d038571eb57f97339c4a53c316be19669a`。独立 `throughput` 场景发送 300 个 10ms 间隔 delta 并要求 final 与界面尾部呈现；不能以旧失败记录冒称修复后通过。

`grapheme` 验证长文本末尾、组合字符和 emoji，交互中改变终端尺寸并上滚；布局单测另覆盖4096旧分块边界、逐delta的ZWJ/区域指示符、拆分代理对及分批推进。`budget` 分别等待真实abort请求和三秒内的可见提示；网络动作与终端重绘独立，不能用看到请求的瞬间断言提示已绘制，也不能假设Ratatui不会在单词间插入光标控制序列。

2026-11-08 r12：18库测试通过（一个辅助子进程由父测试调用），组合130旧bin/6control单测通过，fmt/严格clippy通过；当前bin的16个真实PTY场景全部通过。`pilotdeck` SHA256 为 `653af4164ca4cb1eb763d0f485ecbf0e464b536f64d20069d07dfcb332a5a769`，1031个Rust/manifest/lock输入与本地零差异。r11保留为旧bin的证据，因rsync保留的源码mtime早于前一轮构建完成时间，实际bin未更新；r12刷新已同步输入mtime后重编并重跑，不覆盖旧失败。该标记只对应当轮候选，后续修改须重新绑定来源。

工具交互增量：`tools-capture.py` / `tools-server.mjs`采真实TS原件；`tools-smoke.py` / `tools-server-rust.mjs`验证当前Rust bin。mbp2 tools-r3的22库测试与strict clippy通过；tools-r5实际PTY覆盖Tab/ShiftTab/展开/Esc、短输出、viewer首尾/长行/resize、1MiB+1普通文件拒绝及FIFO预览回退，正常退出0并恢复termios、关闭socket。当前bin SHA256 `5389b8887801e78b2319fbf6437b26e0b9f42afad4be9bd75eda7a7e671ddbce`；另7项预算/吞吐/Unicode/权限取消/切会话/背压/迟到abort回归通过。r4边界失败是观察器误要求包含空格的连续ANSI原字节，原始画面已有明确超限提示；r5只修观察器为本次新增输出的多个关键词，FIFO另要求出现次数增加，原失败保留。证据主仓 `_tmp/20261108-cli-tui/tools-r3-evidence/`、`tools-r5-evidence/`；来源指纹按后者核，不作为后续修改的自动验收。

`config-resources.py`只在mbp2验证实际TUI启动入口：global YAML FIFO、8MiB+1普通文件、binding store FIFO必须五秒内精确报错退出1，终端属性保持；普通文件核长度、FIFO核类型，不能把它表述为所有输入原字节hash验证。2026-11-08 binding-tui-r1三格与普通TUI smoke通过；组合22库/132主bin测试、两个config进程测试及strict clippy通过，七个主bin ignored按原口径（五个父调用辅助、两个未跑既有场景），原件主仓 `_tmp/20261108-cli-binding/tui-r1-evidence/`。当前bin SHA256 `bb16f62e920e97f4049222a8db780e9b230b055f86eae9b934a0a36f193f02e4`。

帮助与设置：`help-capture.py`复采真实TS三页/模式，`help-save-failure-capture.py`原样保留EISDIR探针的失败假设（预期退出而TS未退，故历史采集exit1/observed:false不是Rust失败，也不应改原件来报绿），两者复用同目录server.mjs。`help-smoke.py`使用专属help-server.mjs，可选`--case base|restart|small|permission|permission-abort|long-session`及`--fail`；参数使用`--bin`/`--out`。mbp2 help-r1严格clippy/24库测试及七PTY全通过，help-r2-fail追加实际submit.mode仍default断言通过；六个编译/fixture来源哈希与原始帧在主仓 `_tmp/20261108-cli-tui/help-r1-evidence/`。固定错误提示按当前ASCII屏幕坐标验，非完整终端仿真；session界面最多256字符，完整协议值不截断。差异和验收边界见`D4-TUI-HELP-SETTINGS-001.md`。
