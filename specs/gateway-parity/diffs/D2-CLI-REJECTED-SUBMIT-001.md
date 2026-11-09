---
id: D2-CLI-REJECTED-SUBMIT-001
---

## 类别

默认远端CLI在Gateway明确拒绝后仍等待事件的挂起修复。

## 基线依据

固定TS `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`的GatewayWsClient将stream登记在streams，而response只匹配pending。mbp2真实TS CLI与修复前Rust均在同请求ID的`response ok:false`后两秒仍保持进程/连接，无错误输出；异ID和普通event正控有正常文本。两秒采样本身不证明无限等待，但对应代码没有从该错误响应完成stream的路径，peer继续保持连接且不发final时将继续等。

完整原件及源/bin哈希在主仓`_tmp/20261108-cli-tui/defaultremote-rejection/`。自写复采脚本`src/pilotdeck-cli/tests/fixtures/cli-channel/rejection-probe.mjs`保留原观察方法；TS正常event格仍有既有连接残留，不能把它误报为文本正控失败。

## Rust 行为

提交循环先核相同请求ID；收到`type=response`且`ok=false`即返回error.message，由既有关闭路径关闭连接并退出1。message缺失或非字符串时使用`Gateway stream failed.`；空字符串保留为空行，孤立UTF-16代理项在终端输出为替换字符。无自动重试，不声明请求从未执行。

异ID响应仍忽略，成功response不视为event final。既有event渲染、final、stdin逐行及关闭语义保持。交互模式某行被拒绝后不继续提交已排队下一行。

## 范围

仅旧默认CLI连接既有远端Gateway的submit响应处理。不实现本地fallback，不新增协议、请求超时或成功ACK终止语义；SDK和TUI不在本片范围。

## 机器判据

mbp2`cli_channel_exit`真实进程回归含原TS矩阵和新增六格：普通/空/缺失/非字符串/孤立代理错误、异ID错误加成功ACK后正常event。拒绝格peer不发final也不主动关连接，须三秒内由客户端关闭；stdout/stderr精确断言，排队第二行不发新请求。固定18789只在mbp2由测试独占`/tmp/pdrs-cron-18789.lock`并自行bind，失败不接已有服务。

strict clippy、两进程测试及build通过，bin SHA256 `2f1d1e3b88e1959b72be5b4507ffc0d144842ae90fe1fd6a1913943c9c3a982f`；原始日志与源码绑定在上述目录`r1-evidence/`。不代替完整D2或真人实效验收。

## 批准记录

2026-11-08，AI代批，待人复核；依据本轮完整CLI实现及既有默认修复挂起授权执行。新候选差异未批准为冻结评分豁免，不修改旧金样、阈值或阶段状态。
