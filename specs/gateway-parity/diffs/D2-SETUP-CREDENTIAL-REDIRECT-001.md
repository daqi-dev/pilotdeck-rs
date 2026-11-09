---
id: D2-SETUP-CREDENTIAL-REDIRECT-001
---

## 类别

安全 bug 修复：凭据验证请求不因重定向向其他来源重放秘密，不降级到明文 HTTP。

## 基线依据

固定 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的 `src/cli/commands/gatewaySetup.ts` 使用 Node fetch 验证 Feishu / Lark 凭据。2026-11-08 对真实 `runGatewaySetup` 使用两个自己绑定的动态 loopback origin：第一端返回跨 origin 307，第二端实际收到完整 `app_secret` 请求体。没有连接官方服务或其他人的端点。原件为主仓 `_tmp/20261108-issue-124-setup/worker-evidence/redirect307-{events,setup}.raw.json`，脚本 `run-redirect307.py`，哈希见同目录 `review-fix-evidence-sha256.json`。此证据不声称官方服务实际发生过恶意跳转。

## Rust 行为

`src/pilotdeck-cli/src/gateway_setup/http.rs` 在跟随每次重定向之前检查：仍携带 body 的请求只允许同 origin；HTTPS 到 HTTP 始终拒绝。拒绝时返回 `Refusing unsafe credential redirect`，进入现有验证失败 / 用户选择是否保存的流程。用户明确选择仍保存时只保存自己的输入，不重新外发已拒绝的请求。

## 范围

setup 凭据验证的 HTTP 跟随行为。同 origin 307 / 308 保持 POST 和 body；302 / 303 已按基线转 GET、去除 body 时可以跨 origin，但仍不得 HTTPS 降级。不改变 JSON 字段的成功判断、HTTP 状态处理、重试提示或 provider 登录协议。

## 机器判据

`gateway_setup::tests::production_fetch_never_sends_307_secret_to_another_origin` 使用两个动态 loopback 端点，要求第二端没有连接 / 请求；`production_fetch_keeps_same_origin_307_post_and_secret` 为正控；`credential_redirect_refuses_https_downgrade_even_without_body` 固定降级规则。既有 302 转 GET 与 body 移除测试继续通过。未用真实凭据进行验证。

## 批准记录

**AI 代批，待人复核**，按 `.42cog/intent.md` 安全 bug 默认修复边界执行。**未批准，非已接受差异**，不取得冻结判分豁免；不修改冻结装置或阶段完成状态。决策见 `state/decisions-20261108-setup.md`。
