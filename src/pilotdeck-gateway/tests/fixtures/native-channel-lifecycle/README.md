# Native channel lifecycle consumer probe

非冻结产品诊断；复用 native-storage-stimulus 的真实 executable、隔离 home、WS 控制请求和 loopback OpenAI SSE 模型服务。没有 mock TurnGateway。当前支持 Matrix、Signal、Mattermost、HomeAssistant、Email、WhatsApp；硬编码官方端点的 Telegram/Discord/QQ/DingTalk 通过独有 native_process/channel_lifecycle_tests.rs 和测试专用装配 seam 验证，不开放生产配置。Email 使用实际 IMAP/SMTP 命令，WhatsApp 显式启动自有 bridge.mjs（外部 bridge 合同 mock）。WA 端口使用 bind/drop，存在短暂 TOCTOU，失败保留而不当正式验收。

运行：PDRS_BASELINE=<固定 TS 仓库，仅借用已安装 ws 包> PDRS_CHANNEL=matrix|signal|mattermost|homeassistant|email|whatsapp node process.mjs <gateway-bin> <sidecar-bin> <独占证据目录>。

判据：Server 从真实 adapters 配置装配，真实通道协议入站；模型 stream 请求含输入 marker；实际出站请求含模型输出 marker；reload_config 禁用后允许 TS 异步队列最多十秒，必须停止轮询或关闭接收连接；重新启用后新连接/Matrix whoami 和第二轮模型输出；SIGTERM 退出零、停机后无请求。总体90秒。二进制 SHA、脚本启动时 SHA、原始网络请求与 stderr 写 evidence.json，独占 wx，不覆盖。

局限：Node/ICU环境明确记录，formalAcceptance:false；不是冻结金样或完整平台验收。未通过重载的轮次不能宣称完整生命周期完成。9b9941b9 首次 Matrix 原判据只有200ms出站安静，出现假阳性，原 evidence 留存而不计 PASS；后续增加接收连接与新认证断言，并按固定 TS 异步 reload 语义允许队列完成窗口。
