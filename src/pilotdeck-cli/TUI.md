# Rust 远端 TUI

`pilotdeck tui` 已接入实际终端。它从隔离 `PILOT_HOME` 的配置读取 `gateway.port`，先探测 loopback Gateway，再用该 home 的 `server-token` 建立一条 WebSocket；普通请求、流、取消和权限应答共用这一连接。输入和输出都需要交互式终端。

```sh
PILOT_HOME=/isolated/home /path/to/pilotdeck tui
```

配置中的模型、Gateway 端口和 token 需与所选宿主一致。 Unix入口使用有界配置加载：每个输入文件最多8MiB、一次加载累计最多32MiB，特殊文件或超限明确报错。该预检不会把本地StaffDeck绑定配置发送给远端宿主。当前尚未接默认进程内 Gateway fallback；未发现可连接的宿主时明确返回未实现状态，不自动启动另一份宿主。显式宿主入口的准备方式见 [CONTROL.md](CONTROL.md)。

当前基础交互包括输入与提交、增量回复、模式及权限文件、`/new`、`/switch`、`/sidebar`、`/view`、`/clear`、`/help` 和 `/exit`，PageUp/PageDown 可翻阅聊天。权限请求展示工具及参数摘要，y/n 分别允许一次或拒绝，a 保存本地允许规则后应答。服务端未确认送达时保留请求，可重试或中止。实际可用能力仍由宿主决定。

空输入时 Tab / Shift-Tab 在工具结果间切换焦点，Enter展开或折叠，Esc解除焦点。无焦点时空Enter打开最近的长工具输出，`/view [序号]`可选择结果。短输出写入终端滚动记录，长输出打开viewer：方向键或j/k滚动，PageUp/Down翻页，g/G到首尾，q/Esc返回。长行随终端宽度换行；很宽或较大的短输出也可能进入viewer。超出1MiB的输出会明确提示，无法读取的文件回退到已有预览。

空输入时`?`或`/help`打开帮助，1/2/3选择快捷键、设置和说明，PageUp/Down浏览完整内容，q/Esc关闭。设置页上下选择，Enter或空格只修改Mode，依次为default、plan、bypass；只有保存成功才改变当前模式。bypass持久为skipPermissions，重启可恢复；plan不会单独持久化。命令形式使用`/mode bypassPermissions`。保存失败固定显示提示并保留旧模式。超长会话标识在设置页显示前256字符，实际请求使用完整标识。

Ctrl-E打开或关闭会话列表，Ctrl-S切换项目和状态分组。空输入时上下选择、左右展开/返回组头，Enter展开组或切换会话；窄屏同样可操作。列表打开时Ctrl-C先关闭列表，空闲时再次Ctrl-C退出。非空输入保持原文，导航不会把Ctrl-E/Ctrl-S写进提示词。切换会话仍需等待活动回合、取消和工具读取结束。

侧栏最多接受256条会话，项目/状态键各最多4096个UTF-16单元；标题预览最多256个UTF-16单元，每项显示一行，完整会话标识用于请求。分组索引计入已有状态预算；超限或字段不合法明确提示，不静默截断列表。分组/标题的固定TS对照及PTY脚本见[侧栏测试说明](tests/fixtures/tui-sidebar/README.md)。

运行中 Esc 或 Ctrl-C 请求取消；取消确认前不会提交下一轮或切换会话。取消结果未知时关闭连接并要求退出后重连。空闲 Ctrl-C 退出；退出、错误和信号路径负责恢复终端模式。文字按 Unicode 字素宽度连续硬换行，显示缓存分批更新以让事件和输入继续推进；不承诺复刻旧 Ink 的像素排版。

状态、显示缓存及输入合计约8MiB计数，资源不足会明确提示并中止活动回合，逐步移除最新内容，保留可显示的旧历史。它不是进程 RSS 上限。输入最多64KiB，token最多64KiB；工具输出文件最多读取1MiB。终端短暂不可写会等待，整帧一秒后仍无法写入则失败退出。具体计数与尚待人复核的行为差异见 `specs/gateway-parity/diffs/D4-TUI-*.md`。

本片是远端交互实现，不代表完整 D4。输入编辑与粘贴的剩余兼容性继续实现；当前 native 宿主的权限发射和历史读取接线仍有限，fake WS 的权限/history 场景不能代表实际宿主已具备这些能力。真实 provider 与人眼终端体验仍需独立验收。

复现脚本见 [tui-pty README](tests/fixtures/tui-pty/README.md)。Rust 编译和 PTY 验证均在 mbp2，证据保留在主仓 `_tmp/20261108-cli-tui/`；真实执行的 bin 必须与本轮源码指纹绑定，不能仅凭增量构建退出0判断来源。
