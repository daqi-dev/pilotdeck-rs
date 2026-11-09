---
id: D4-TUI-HELP-SETTINGS-001
---

## 类别

帮助设置页的安全保存与完整可读交互；显示有界、失败可见，候选差异不取得冻结豁免。

## 基线依据

固定TS `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的 `TuiApp.tsx:345-411` 与 `HelpDialog.tsx` 定义三页、Settings内方向/Tab不换页、数字换页、三种模式循环。真实TS PTY原件在主仓 `_tmp/20261108-cli-tui/help-ts-r1/`：三次保存skipPermissions依次false/true/false。真实`permissions.json`为目录时出现EISDIR错误栈，两秒内未退出，TERM后仍需收割；失败探针旧断言期待退出，故其observed:false保留为失败假设的原件，不能称TS恢复通过。详见`help-ts-probe/README.md`与`help-ts-fail-r1/`。

基线帮助文案列`/mode bypass`，实际命令解析只接受`bypassPermissions`；Rust帮助按实际命令说明，不新增别名改变既有解析。基线入口传入的model显示值为`PilotDeck`，不假称已取回远端实际模型配置。

## Rust 行为

三页内容可翻页，窄屏也能浏览。Settings保存复用既有原子权限文件，只有写入成功才切换当前模式；失败保持模式与原目标，固定顶部提示失败，完整错误保留在可滚动正文。运行、待决会话、取消未确认或连接不确定时禁止该模式修改；权限应答与Ctrl-C继续优先于帮助键。

Settings只显示session前256个Unicode字符及省略标记，协议继续使用完整session。此为防御性显示边界，未声称已实测TS长session挂起。布局和文案自行实现，不复制Ink排版；真实连接显示本地选定的Gateway地址，不能由此推断宿主真实权限或history已接通。

## 范围

远端TUI帮助/模式设置，沿用现有权限文件规范、原子保存与取消互锁。不改变`/mode`参数语义、Gateway方法、既有持久字段或冻结装置；前256字符是显示预览，不是会话标识截断。全TUI、默认本地fallback及真人体验尚未完成。

## 机器判据

`help-smoke.py`及专属`help-server.mjs`验证三页、Settings内导航、只读项、三次保存、实际bypass请求、退出后重启的持久模式、窄屏滚动、打开帮助时权限应答/取消、10万字符session与终端恢复。EISDIR格用当前屏幕ASCII坐标解码检查固定提示，并在失败后的实际submit核mode仍为default；该解码不是通用终端模拟器。`help.rs`的TestBackend另核滚动后错误仍可见。

mbp2 help-r1严格clippy、24库测试通过；一个辅助子测试由父调用。七PTY场景通过，help-r2-fail追加实际请求模式断言也通过，同bin SHA256 `60485228bfb96159371c21335824571b43a134e682431d37a2f7890f751f0383`；源码、原始日志和帧保留主仓 `_tmp/20261108-cli-tui/help-r1-evidence/`。后续修改不自动沿用此结论。

## 批准记录

2026-11-08，AI代批，待人复核。用户授权功能完整而不要求逐像素重构；安全失败和显示限额仍未批准为冻结验收差异。不得据此标D4、完整CLI或真人实效完成。
