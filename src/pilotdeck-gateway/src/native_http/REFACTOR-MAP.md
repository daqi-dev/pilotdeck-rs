# HTTP 第一波移动映射

本文件是开发交接说明；代码状态为已编辑、编译与运行证据 NOT_RUN。

| 原 native_http.rs 符号 | 当前归属 |
|---|---|
| finish、parser_bad_request、request_header_fields_too_large、headless_bad_request、not_found | response.rs |
| handler_response、handler_response_with_length、node_plain_response、sized_body、streamed、json_string | response.rs |
| StreamedBody、Unsized 及其 Body 实现 | response.rs |
| CONTENT_TYPES、file_response、FileBody 及其 Body 实现 | static_assets.rs |
| serve_static、normalize_root、node_join、contained、is_file、content_type_for | static_assets.rs |
| Config、WebhookReply/Handler/Resolver、FEISHU_WEBHOOK_PATH、serve/serve_with_body、route、request_path、whatwg_path | 保留 native_http.rs |
| LLHTTP_METHODS、node_parser_admission、node_keep_alive、request_wants_close、llhttp_scheme_alpha/llhttp_target_form、tracked_header_bytes、over_header_budget | 保留 native_http.rs |

父模块显式 `pub(crate) use` 保留 `native_http::{handler_response, handler_response_with_length, node_plain_response, request_header_fields_too_large, streamed}`。`control` 对 `super::finish` 的调用保留；新增两个子模块均为私有模块。

`Unsized` 类型与元组字段仅扩到 `pub(super)`，父路由继续直接构造。父模块继续导入 `normalize_root`、`serve_static`；测试构建显式导入 `node_join`、`contained`、`content_type_for`，并保留 `Bytes`、`ProxyError`、`Pin`、`Context`、`Poll`，供原 `tests.rs` 的 `super::*` 使用。测试文件未改，`#[cfg(test)] mod tests` 仍只有一处。

各既有 mutant hook 随所属函数移动，名称及出现次数不变；未增加 hook、备用实现或状态。扣除导入、模块声明和必要可见性后，移动文本与剩余父模块文本均与编辑前一致。路由/admission/WHATWG 路径处理留原处，因为这些决定请求分派与拒绝，不能归入静态文件读取职责。

待跑：仅在获准 cb8 的现行资源/租约包装器内运行 `cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib native_http::tests -j 4`；同一集成提交还须 workspace 构建、既有十阶段完整回归及独立双审，过滤单测不能替代这些门。

风险：尚未由 Rust 编译器确认模块可见性、重导出及测试导入；须用既有测试覆盖 HTTP/1.0、HEAD、头顺序、chunk/EOF、断开与静态 containment。文本一致核对不能证明运行行为或替换验收通过。

本机未运行构建、测试、rustfmt、语法检查或 SSH；这些检查与远端运行均为 NOT_RUN。
