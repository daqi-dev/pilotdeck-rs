---
id: D2-SETUP-ATOMIC-SAVE-001
---

## 类别

数据丢失与文件权限的安全修复。配置写入失败不再截断旧文件，新配置明确使用 0600；不能可靠承接的元数据或文件拓扑拒绝写入。

## 基线依据

固定 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的 `src/cli/commands/gatewaySetup.ts` 用 `writeFileSync` 直接保存。首个 Rust 切片本地提交 `f0c2c054fff6725856f962243291f4eabca8bdd7` 保持直接截断写入；在 mbp2 的真实 `ulimit -f 0` 子进程中，旧配置被截断到 0 字节，返回 `EFBIG: file too large, write`。这是实际文件写失败，不是仅构造错误对象。该行为属于意向中默认修复的数据丢失边界。

原始首包与本次失败 / 修复轮记录位于主仓 `_tmp/20261108-issue-124-setup/`。本片原始 mbp2 `atomic-test-r1.log`、`atomic-test-r2.log`、`atomic-test-r3.log`、`atomic-clippy-r3.log`、`atomic-fmt-r3.log` 已独占归档到该目录的 `atomic-worker/`，SHA256 见同目录 `sha256.json`。原始失败轮没有被成功轮覆盖。

## Rust 行为

`gateway_setup/save.rs` 检查受支持的目标及原权限，在实际目标的同目录独占创建 UUID 临时文件。初始 0600 并检查继承元数据后，才完整写入内容；随后保留已有普通文件的 mode / uid / gid、同步文件、复核目标身份，最后 rename 发布。新配置最终 0600。新目标使用 NOREPLACE，避免覆盖同时出现的文件；发布前出错清理自己的临时文件，旧文件保持原字节。rename 是最后一个可失败步骤。

有效符号链接只更新其实际目标，原符号链接保留。断链、hardlink、非普通文件、特殊权限位、macOS 非零 BSD flags、任何 xattrs / ACL，以及不能检查 / 保留的元数据均明确拒绝。macOS 使用固定 `/bin/ls -ledn`、`LC_ALL=C` 检查 ACL；检查失败也拒绝，xattr 拒绝检查在先。Linux ACL 由 xattr 拒绝路径覆盖。该策略不声称保留任意平台元数据；不支持的形态不自作转换。

## 机器判据

mbp2 最终 `atomic-test-r3.log` 为 `27 passed; 0 failed; 1 ignored`。ignored 是由父测试实际定向启动的文件大小限制子测试；父测试检查 `WRITE_ERROR` 回执，不能以空过滤器通过。

- `setup_write_reports_descriptor_stage_under_fsize_limit`：真实 EFBIG 后旧配置原字节不变、无本次临时文件；错误文案仍是无路径的 write 阶段。
- `atomic_new_config_is_private_and_complete` / `atomic_save_preserves_existing_mode_owner_and_group`：新文件 0600、旧 mode / uid / gid 保留。
- symlink / hardlink / dangling 与写者改动测试：只替换 symlink 目标；不支持或身份变化时拒绝发布，原件 / 后来写者的字节保留。
- 真实 xattr、macOS ACL 和 `chflags hidden` 注入：拒绝保存且原字节、元数据保留；BSD 记录 `BSD_FLAGS_PRESERVED=32768`。

同一源码快照的 fmt 与 `clippy --tests -- -D warnings` 通过。测试与检查全部在 mbp2 隔离 target 执行，本机未编译。真人终端体验与真实平台登录不由这些文件系统测试代填。

## 范围

仅适用于 CLI setup 的本地配置保存。未与全部旧写者建立互斥协议；其他写者持有旧 inode、最后一次复核到 rename 之间的竞态仍有明确边界。文件同步不代表目录项跨断电持久化，不宣称完整崩溃耐久。没有修改旧 TS 写者、Gateway 公共配置存储或冻结装置。

## 批准记录

**AI 代批，待人复核**，依据默认修复已实证数据丢失和安全问题的现有授权执行。**未批准，非已接受差异**；本条不提供冻结评分豁免，不修改阈值、分母、A8 前置或阶段完成状态。决策追加于 `state/decisions-20261108-setup.md`。本轮仅本机提交，不推送 GitHub，不开 PR。
