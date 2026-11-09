# TUI 会话分组与导航

`oracle.mjs`实际导入固定TS的sidebar helper，生成项目/状态分组、折叠和标题优先级金样；运行前检查Node22.22.0/ICU78.2、TS commit及helper工作文件与Git对象的原字节。`expected.json`供Rust纯模型测试读取，不由Rust结果生成。

`ts-capture.py`/`ts-server.mjs`为自行编写的真实TS CLI采集器，记录宽/窄屏PTY和实际请求。原件在主仓`_tmp/20261108-cli-tui/sidebar-ts-{wide,narrow}-r1/`：宽屏实际发出resume_session及read_session_messages；窄屏进入overlay状态却没有可见导航，Ctrl-S写入提示词并最终提交`s`。采集器的observed表示预定观察动作已完成，不代表窄屏功能通过。

`smoke.py`/`server.mjs`用真实Rust bin与隔离loopback假宿主验分组、展开/折叠、长标题可见、双请求选中、非空输入保全及Ctrl-C先关闭列表后退出。选项`--narrow`、`--slow`可组合；`--large`发送256个长共同前缀项目路径，检查出现真实basename、之后仍能输入并恢复终端。large耗时包含脚本的固定0.5秒等待，只能作为显示响应上界，不代表构组期间公平性或CPU微基准。假宿主不证明native history接线。

所有运行在mbp2；参数见各脚本，输出目录必须不存在。例：

```sh
python3 smoke.py --bin "$PILOTDECK_BIN" --node "$NODE22" --ts-root "$FIXED_TS" --out "$EXCLUSIVE_OUTPUT" --narrow --slow
"$NODE22" --import "$FIXED_TS/node_modules/tsx/dist/loader.mjs" oracle.mjs "$FIXED_TS" "$NEW_ORACLE"
cmp "$NEW_ORACLE" expected.json
```

脚本名曾为`pty.py`，会遮蔽Python标准库同名模块；首次运行失败保留在`sidebar-pty-r3`，改名后重新执行，不把该失败算作产品验收。

mbp2 sidebar-r3严格clippy、28库测试及build通过；sidebar-pty-r4五侧栏场景与五相关回归全部通过，真实bin SHA256 `fafd1b3ce89284faf4d671a8d592404723dce2b31ca8cf74b291e5d8684dc599`。large显示观察值0.539243秒，按前述边界解释。原件与来源指纹见主仓`_tmp/20261108-cli-tui/sidebar-r3-evidence/`；后续修改需重新绑定来源。
