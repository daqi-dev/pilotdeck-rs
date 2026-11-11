# 普通生产入口采集

--mode history 仅提供现有 read_session_messages、read_subagent_messages、export_session_transcript、restore_session_transcript 的正常业务原始证据。结果及 provenance 明确标注 diagnosticOnly=true、formalAcceptance=false、formalAcceptanceState=NOT_RUN。

该模式复用已注册 U9/U10 驱动并增加普通 portable 导出恢复。每侧正常单轮不能代替正式 150 入口、101 RPC 的 k5、原生 witness 或 session 合同。正式 retirement_entries 只接收其已登记的 HTTP/Cron/AlwaysOn profile，本模式本身不增加分母或填充正式 PASS。

参照 probe.py --help，指定固定 TS、同版四bin/sourceManifest/candidateManifest，在已验证的 network-none 和自有 parity lease 内分别执行 engine ts/native，输出必须独占。三项 history driver/bundle 的 provenance SHA、实际 wire 和停机后 durable bytes 由独立只读审计复核。当前正式接线未完成的部分继续 BLOCKED。
