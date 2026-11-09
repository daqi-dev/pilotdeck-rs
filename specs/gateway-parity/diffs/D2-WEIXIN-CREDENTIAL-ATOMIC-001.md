---
id: D2-WEIXIN-CREDENTIAL-ATOMIC-001
---

## 类别

数据丢失与凭据权限的安全修复。微信扫码重新登录成功后，凭据通过现有 setup 原子保存器发布；写失败不截断旧凭据，新文件采用 0600。

## 基线依据

固定 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的 `src/cli/commands/gatewaySetup.ts:596-601` 先创建 HOME/.pilotdeck，再以 writeFileSync 直接写 PILOT_HOME/weixin-credentials.json。真实 TS `runGatewaySetup(['weixin'])` + 安装的 weixin-ilink@0.1.0，在隔离 HOME/PILOT_HOME/config、两次 fake fetch 成功后，施加真实 RLIMIT_FSIZE=0：输出 `EFBIG: file too large, write`、退出0，旧凭据截断为零，配置原件保持。没有调用真实微信服务，也没有用模拟 IO 错误替代系统失败。

可复现入口 `src/pilotdeck-cli/tests/fixtures/gateway-setup/generate-weixin.py --write-failure`，完整原件 `weixin-write-failure.json`；原始进程输出与受限文件留在主仓 `_tmp/20261108-issue-124-setup/weixin-worker/`。driver 仅在此模式通过 stdout 管道回传事件，禁用 tsx 写缓存，防止装置写入掩盖产品失败。

## Rust 行为

`gateway_setup/weixin.rs::save_credentials` 复用 `save::Target`，完整写入并同步同目录独占0600临时文件后，再发布到受支持的凭据目标。旧普通文件权限/owner/group保留；新文件0600；不能可靠保留的 ACL/xattrs/BSD flags、hardlink 等拒绝，symlink 只更新实际目标，遵循 `D2-SETUP-ATOMIC-SAVE-001` 的边界。实际 EFBIG 清理临时文件并保全旧凭据，输出仍为相同失败文案，且不显示登录成功。

## 范围

仅影响 CLI setup 微信凭据的保存失败/权限路径；成功 JSON 仍只保存 baseUrl、botToken、accountId，键序、缩进、无末尾LF与真实金样一致。保留 legacy HOME/.pilotdeck 创建和自定义 PILOT_HOME 父目录缺失时失败的行为。凭据保存与后续 YAML 启用是两个顺序操作，不声称跨两文件事务；配置保存失败时可能已经更新凭据。未修改 Gateway 公共登录/写者，没有与所有旧写者建立互斥协议，也不宣称目录项跨断电持久化。

## 机器判据

`tests_weixin.rs::weixin_failed_credential_write_keeps_old_credentials_and_config` 实际以文件大小0限制启动辅助子测试，父测试核对其回执、旧凭据和配置原字节、无临时残留。辅助子测试逐字比较真实 TS EFBIG 完整 stdout（仅剥离装置事件回执）；没有把 substring 命中当作输出一致。普通成功/重新登录/复用路径另由14格 SDK/TS金样核请求、凭据/配置原文和回调输出。最终 mbp2 完整测试与严格clippy/fmt原始日志由协调者归档于同一任务目录，未执行的真实微信账号登录与真人终端体验不得冒充通过。

## 批准记录

**AI 代批，待人复核**，按现有默认修复已实证数据丢失/安全边界的授权执行。**未批准，非已接受差异**，不提供冻结评分豁免，不改变阈值、分母、A8前置或阶段完成状态。决策见 `state/decisions-20261108-setup.md`。本轮仅本机提交，不推送GitHub或开PR。
