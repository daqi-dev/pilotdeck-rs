---
id: D4-TUI-SIDEBAR-001
---

## 类别

会话侧栏的窄屏交互修复与有界显示。候选差异，不取得冻结判分豁免。

## 基线依据

固定TS `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的`sidebar-helpers.ts`定义项目/状态分组、排序、活动组展开和标题优先级；实际导入helper得到的金样位于`src/pilotdeck-cli/tests/fixtures/tui-sidebar/expected.json`。原始真实CLI/PTY记录在主仓`_tmp/20261108-cli-tui/sidebar-ts-{wide,narrow}-r1/`，采集说明在`sidebar-ts-probe/README.md`。

宽屏实际导航并发送resume_session与read_session_messages。窄屏进入overlay状态却没有可见侧栏，Ctrl-S进入提示词并在Enter后提交`s`；采集脚本observed:true仅表示观察步骤完成，不代表该功能正确。标题按customTitle、aiTitle、firstPrompt、summary、sessionId选择首个非空值，保留此优先顺序。

## Rust 行为

宽窄屏均以可滚动列表完成分组、折叠和选择，不复刻Ink像素布局。Ctrl-E打开/关闭、Ctrl-S切分组，空输入时方向键与Enter导航；打开列表时Ctrl-C先关闭，之后空闲Ctrl-C退出。非空输入原样保留，控制快捷键不变成提示词；活动回合、取消未定和工具读取期间不切会话。

列表最多256项，cwd/tag须为空值或至多4096个UTF-16单元的字符串；不合条件明确报错。标题/组名预览有256个UTF-16单元边界，每行按字素宽度裁剪，避免长标题使选中项离开视口。完整session标识用于RPC。索引容量计入8MiB应用状态预算，清理时释放容量；此预算不是RSS上限。安全边界不声称基线所有大输入均已实测挂起。

## 范围

远端TUI会话列表与会话切换交互；沿用现有Gateway请求、session选择与history读取。不改变协议或历史数据，不代表native history/权限、本地fallback或完整D4完成。

## 机器判据

`sidebar.rs`实际TS金样测试核分组/标题及导航，另核长前缀basename和清理容量。`smoke.py`真实PTY检查宽/窄屏、1200ms慢列表不重复请求、长标题可见、选择时两个RPC使用相同完整session key、非空输入保全及退出终端恢复。`--large`发送256个约4087单元共同前缀项目键，显示后仍可关闭并提交。

mbp2 sidebar-r3严格clippy、28库测试通过（一个辅助子测试由父实际调用）、build通过；新bin SHA256 `fafd1b3ce89284faf4d671a8d592404723dce2b31ca8cf74b291e5d8684dc599`的十项PTY全部通过，包含五侧栏场景及权限取消/切换/迟到abort/help/tools回归。large显示观察值0.539243秒，包含脚本固定0.5秒等待，仅约束显示及后续可操作，不证明构组期间公平性。源码、失败与最终原件在主仓`_tmp/20261108-cli-tui/sidebar-r3-evidence/`；首编译测试借用错误及脚本与Python标准库重名失败均保留。

## 批准记录

2026-11-08，AI代批，待人复核。用户授权完整功能且不要求逐像素重构；新资源边界、预览长度与窄屏修复未作为已批准冻结差异。未运行真人体验或真实provider验收，不据此标阶段完成。
