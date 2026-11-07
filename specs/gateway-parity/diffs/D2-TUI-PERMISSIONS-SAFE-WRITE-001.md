---
id: D2-TUI-PERMISSIONS-SAFE-WRITE-001
---

## 类别

TUI 权限设置写入保护与读取资源预算。

## 基线依据

固定 TS `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的 `src/permission/settings.ts` 读取错误或 JSON 损坏时返回默认空设置，写时重新读取默认值并直接覆盖 `permissions.json`；mbp2 上 Node 22.22.0 / ICU 78.2 的真实该模块在隔离目录对 `{bad` 实测：读取返回默认，写 `skipPermissions:true` 后原损坏字节被替换。原件 `src/pilotdeck-cli/tests/fixtures/tui/permissions.json` SHA-256 `d1b34edcea849a53c14dabd22e8c7d5aa31c62244365c22019e0396a63869835`，复采脚本 `generate-permissions.mjs`；远端原件位于 `/Users/da/ws/pdrs-runner/_tmp/20261108-tui-permission-capture-v2/`。该证据不覆盖所有文件系统类型。

## Rust 行为

正常文件的读取规范化及写入字段语义依 TS；保留 JS UTF-16 字符串和 JS 数字真值。缺失、常规读取错误和损坏 JSON 的只读查询返回默认值；超过 8 MiB 明确返回错误，避免无限分配。写入前先用共享 `atomic_file::Target::prepare` 检查目标，再严格读原文件；除不存在外，读取错误、损坏 JSON 和超 8 MiB 都拒绝写入。成功仅在临时文件同步与原子发布后报告；符号链接、权限/所有者、扩展属性、硬链接和并发校验按共享 Target 支持范围，不支持则显式拒绝。Target 不替所有其他程序串行化写入，检测与重命名间仍有竞争窗口；不能宣称具备跨进程事务。首次发布创建模式采用共享 Target 的 0600，不复制 TS 默认 umask 结果。
## 范围

仅限 TUI 的 permissions.json，不影响其他配置或 SDK。

## 机器判据

`src/pilotdeck-cli/src/tui/permission_settings.rs` 单元测试消费真实 TS 金样，覆盖未配、别名、去重、空对象、未知字段、超大数、孤立 surrogate、正常读写及损坏文件不覆盖；实际文件目录型目标、8 MiB 超限和子进程实际 EFBIG 写入失败负控保留原字节并清理临时文件。共享 `atomic_file` 的 setup 测试覆盖保存失败与元数据约束；不以此冒称真实 TUI 或 provider 验收。

## 批准记录

**AI 代批，待人复核**：本轮用户授权范围内的数据保全和资源限制；人签前为未批准，非已接受差异，不取得冻结判分豁免。
