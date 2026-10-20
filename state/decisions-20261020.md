# 2026-10-20 决策卡

## A6.4-pr5 起：真实模型 provider 需新增 TLS 依赖（人批 A，2026-10-20）
- **卡在哪**：A6.4-pr5（Provider HTTP 公共层 + anthropic）及其后 openai / responses / google / fallback（pr6~pr9）、A6.5-pr9 web_fetch/web_search、A6.6-pr8 全量 compaction（需辅助模型）都要发 HTTPS 请求；`src/Cargo.lock` 无 rustls / native-tls / hyper-rustls（盘点 `_tmp/20261018-a6-split/inventory.md` §0.3）。
- **谁受影响**：上述 7 单；其余 A6.5 单（rewind、bash、MCP、后台任务）不受影响。
- **能否撤回**：加依赖可撤回，但引入外部 crate 属供应链面变更，现有约定为「不加新 crate 或锁文件变更」。
- **动不动尺子 / 意向**：不动尺子；涉及依赖策略。
- **选项**：A 引入 rustls + hyper-rustls（纯 Rust，无系统 OpenSSL）；B native-tls（系统 Security.framework / OpenSSL）；C 先只做明文 HTTP + mock 的公共层，TLS 连接器留接口，待批后补一单。
- **建议**：A（与 hyper 现有栈一致，跨平台无系统依赖）；批前可按 C 先推进公共层。
- **档位**：暂不执行（新外部依赖）。C 也先不做，等人一并定，避免公共层接口按错误假设定型。
- **原则**：新增外部 crate 一律单独成卡、单独 PR。
- **人批（2026-10-20，用户本人在协调者会话）**：选 A——rustls + hyper-rustls。依赖单独在 A6.4-pr5 引入（不另拆纯依赖 PR，但锁文件变更在 PR 描述单列），只开所需 feature，源码构建，不用预编译二进制。
