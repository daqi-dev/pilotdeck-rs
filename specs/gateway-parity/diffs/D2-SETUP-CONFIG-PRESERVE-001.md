---
id: D2-SETUP-CONFIG-PRESERVE-001
---

## 类别

数据丢失及错误成功提示的 bug 修复。已有配置无法读取、无法解析或不能容纳适配器字段时拒绝保存。

## 基线依据

固定 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的 `src/cli/commands/gatewaySetup.ts` 捕获配置读取 / 解析错误后将其当作空配置；对数组添加非索引属性后，YAML 序列化又丢弃该属性。

2026-11-08 在隔离 HOME、虚构凭据和 fake fetch 下直接执行真实 `runGatewaySetup`：原文件 `keep: [\n` 被覆盖成仅含新 Feishu 字段的配置；根数组和 `adapters: []` 两格均退出 0、提示已写入，文件中却没有新适配器。原始记录保存在主仓 `_tmp/20261108-issue-124-setup/worker-evidence/save-bug-{invalid,root-array,adapters-array}.raw.json`；SHA256 清单为同目录 `review-fix-evidence-sha256.json`，重现入口为 `probe-save-bugs.py`。这是真实命令函数的证据，不是完整旧 bin 的全局启动验收。

## Rust 行为

`src/pilotdeck-cli/src/gateway_setup/config.rs` 的保存路径只在配置不存在或解析结果为空文档时初始化配置。存在但读取失败时保留文件并返回分阶段文件错误；UTF-8 解码、YAML 解析 / 转换失败时报 `Cannot save setup configuration: existing YAML is invalid.`；不支持的根 / adapters 容器报对应 `must be a YAML mapping` 错误，不显示写入成功。普通读取提示仍可使用空默认值，保存前必须重新严格读取。

## 范围

`gateway setup` 的配置保存，不改变正常 mapping 的合并、其他字段保留、YAML 别名 / 循环、路径约定或原错误优先级。拒绝保存不等于原子写入：写入中途失败或断电的完整性另行处理，不在此宣称已解决。

## 机器判据

`src/pilotdeck-cli/src/gateway_setup/tests.rs` 的 `strict_save_keeps_existing_invalid_yaml_and_unreadable_objects`、`strict_save_preserves_existing_invalid_utf8_bytes`、`strict_save_refuses_array_shapes_without_claiming_success` 检查错误和原文件字节 / sentinel 不变；正常配置的 TS 字节样例与成功保存测试继续通过。完整命令及已执行日志见主仓 `_tmp/20261108-issue-124-setup/`。原始 TS oracle 保留，不改写成 Rust 期望。

## 批准记录

**AI 代批，待人复核**，按 `.42cog/intent.md` 默认修复丢数据与自身结果矛盾的边界执行。**未批准，非已接受差异**，不取得冻结判分豁免；本条不修改冻结装置或阶段验收门。决策见 `state/decisions-20261108-setup.md`。
