---
id: D2-WEIXIN-RESOURCE-BOUNDS-001
---

## 类别

避免挂起和无界内存占用的安全修复。微信凭据读取与 iLink QR/status 响应设明确资源上限；超限/不支持输入失败并保全旧文件。

## 基线依据

固定 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的 `gatewaySetup.ts:583-592` 同步无界 readFileSync 读取凭据。真实隔离 `runGatewaySetup(['weixin'])` 用无writer FIFO会停在读取；外部父进程2秒杀收后，FIFO/config原件保持。大于1MiB且有效的凭据JSON则被原版接受并进入旧凭据复用提示。可复现入口 `src/pilotdeck-cli/tests/fixtures/gateway-setup/probe-weixin-resources.py`，固定版本和原始结果见主仓 `_tmp/20261108-issue-124-setup/weixin-worker/`。

安装的 `weixin-ilink@0.1.0/dist/auth.js:15-35` 在收到 poll headers 后即 clearTimeout，再 res.json；35秒只约束headers。SDK body无总时长/字节上限，Node fetch的分块inactivity机制不能为持续慢流提供绝对期限。QR初始fetch也不设SDK body限制。这一事实在正常金样外单列，不声称新资源边界是原版已有语义。

## Rust 行为

凭据以 O_NONBLOCK 打开，同一fd的metadata必须是普通文件、大小不超过1048576字节；读取自身再限limit+1，文件增长也不能无界分配。非普通/超限/读取失败明确返回错误，未发网络请求、不保存旧凭据/配置；仅NotFound当作未登录。无效JSON的显式新扫码登录继续按基线处理。

iLink QR与status最终headers收到后，到body完整读取设30秒绝对deadline，任何chunk都不续期；body最大1048576字节，超限即失败。poll原35秒headers abort边界保留；body guard返回明确错误，不伪装为可重试Abort。该预算是本包明确采用的安全差异，不是从SDK默认值推断出的兼容事实。

## 范围

仅CLI setup微信凭据和iLink模式，Feishu/WeCom既有HTTP语义不变。普通文件读取有字节边界，并非宣称能中止所有故障文件系统的内核IO；不覆盖其他配置读者或Gateway旧写者。最终headers之前初始fetch仍沿共同传输headers期限，poll仍35秒。现有8分钟登录计时不替代body资源限制；没有修改冻结装置或基线金样。

## 机器判据

`weixin_unsafe_credential_inputs_are_bounded_and_preserved` 使用真实无writer FIFO和超限稀疏文件，由父进程3秒期限保护并定向启动辅助子测试；核明确错误、零请求、原inode/mode/size与配置字节不变。若回归成阻塞读取，父进程杀收并失败，不让cargo挂起。

`weixin_native_body_limits_reject_oversize_and_continuous_slow_flow_without_saving` 通过真实127.0.0.1:0服务、同一生产guard的构造参数缩小字节/时间预算，发送超限响应与每5ms持续发数据的慢流；覆盖初始QR和poll，核明确失败而非Abort、旧凭据/config原文保留、无成功提示/临时残留。`weixin_poll_abort_deadline_ends_at_headers_not_body` 正控headers前超时为Abort、headers后正常短body成功。正常14格TS/SDK金样仍逐字对齐；父进程定向辅助子测试的回执不可由空过滤器替代。

## 批准记录

**AI 代批，待人复核**，协调者已确认1MiB读/响应上限、30秒body绝对deadline、35秒poll headers保留。**未批准，非已接受差异**，不是冻结评分豁免，不改变阶段完成状态。决策见 `state/decisions-20261108-setup.md`。仅本机提交，不推送GitHub或开PR；真实平台和真人体验另行验收。
