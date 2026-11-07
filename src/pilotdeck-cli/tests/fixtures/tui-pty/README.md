# 远端 TUI 的真实 PTY 冒烟

这些脚本由 mbp2 执行：使用编译出来的当前 Rust `pilotdeck`、固定 Node 与固定 TS 树里已安装的 `ws` 作为本地 fake 服务；每次要求不存在的专属输出目录，使用动态 loopback 端口、独立 home/project、虚构 token，记录原始终端、双向帧、退出和终端模式。运行参数分别见脚本 `--help`。`smoke.py` 含 `--small`；`scenarios.py` 包括权限、取消、history、流和资源预算，以及 `abort-late`、`abort-timeout` 与 `throughput`；`backpressure.py` 停止读取 PTY；`output-pipe.py` 验证非终端输出明确拒绝。假服务不代表真实 provider/native 权限闭环，不是阶段验收。

吞吐回归前的原始失败保存在主仓 `_tmp/20261108-cli-tui/throughput-r1/out/` 和 mbp2 `/Users/da/ws/pdrs-worker-wt/control-cli-20261108/_tmp/20261108-control-verify/throughput-r1/out/`：100 个小 delta/秒时仅到 seq 147 便关闭连接、25 秒未收到 final；result SHA-256 `8c7b06fb969d8e117cff48a477bef40ee975cbc50f982c1d1f69334eec0059d6`，wire SHA-256 `0d7dd630af4a196282872be5cd4f13d3edcd42bf91ce7c6481f7d5db6d0c9762`，terminal SHA-256 `ea2731dfcfc69b8d1c1fd292c79c02d038571eb57f97339c4a53c316be19669a`。独立 `throughput` 场景发送 300 个 10ms 间隔 delta 并要求 final 与界面尾部呈现；不能以旧失败记录冒称修复后通过。

`grapheme` 验证长文本末尾、组合字符和 emoji，交互中改变终端尺寸并上滚；布局单测另覆盖4096旧分块边界、逐delta的ZWJ/区域指示符、拆分代理对及分批推进。`budget` 分别等待真实abort请求和三秒内的可见提示；网络动作与终端重绘独立，不能用看到请求的瞬间断言提示已绘制，也不能假设Ratatui不会在单词间插入光标控制序列。

2026-11-08 r12：18库测试通过（一个辅助子进程由父测试调用），组合130旧bin/6control单测通过，fmt/严格clippy通过；当前bin的16个真实PTY场景全部通过。`pilotdeck` SHA256 为 `653af4164ca4cb1eb763d0f485ecbf0e464b536f64d20069d07dfcb332a5a769`，1031个Rust/manifest/lock输入与本地零差异。r11保留为旧bin的证据，因rsync保留的源码mtime早于前一轮构建完成时间，实际bin未更新；r12刷新已同步输入mtime后重编并重跑，不覆盖旧失败。该标记只对应当轮候选，后续修改须重新绑定来源。
