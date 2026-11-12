# 附件服务职责迁移

第一波；仅迁移附件组合闭包。`services.rs`：2459 → 919 行（−1540）；新增 `attachments.rs`：1562 行；产品源码净增 22 行。

| 原位置 / 符号 | 新归属 |
|---|---|
| `services.rs:39–1508` 附件组合闭包 | `attachments.rs:16–1485`，函数体、常量、错误文本和处理顺序按原字节迁移 |
| `compose_attachment_turn`、`compose_attachment_turn_with_cache` | `attachments.rs:708`、`:719`；父模块保留原 `services::` 路径 |
| `attachment_identity_matches`（unix / not(unix)）、`handle_bound_to_upload_areas`、`canonical_upload_store` | `attachments.rs:564`、`:570`、`:602`、`:634`；父模块保留原路径 |
| `AttachEntry`、`RegisteredAttachment`、`LeaseGuard`、`DiagnosticSeverity`、`AttachmentRoute` | 附件子模块私有类型 |
| `MAX_INLINE_FILE_BYTES`、`MAX_INLINE_IMAGE_BYTES`、`BYTES_PER_PDF_PAGE`、`ATTACHMENT_PATH_NOTE_MARKER`、`TEXT_EXTENSIONS`、`BINARY_CONTAINER_EXTENSIONS`、`AUDIO_EXTENSIONS`、`PLAIN_PATH_NOTE_GUIDANCE` | 附件子模块私有常量 |
| `base64_encode`、`lease_guard`、`extension_of`、`resolver_extension`、`image_mime_for_ext`、`diagnostic_enters`、`pdf_size_estimate`、`is_binary_container`、`unsupported_file_message`、`image_mime` | 附件子模块私有函数 |
| `is_audio_shape`、`is_audio_attachment`、`is_read_file_inspectable`、`map_audio_path_for_funasr`、`clean_attachment_name`、`flat_image_block`、`image_not_found`、`file_not_found`、`attachment_route`、`attachment_not_found` | 附件子模块私有函数 |
| `surrogate_path_not_found`、`cached_process_cwd`、`attachment_lexical`、`file_too_large`、`image_too_large`、`read_bounded`、`attachment_diagnostics_guidance` | 附件子模块私有函数；唯一 `CACHED` 随函数迁移 |
| `services.rs:2391–2459` 的 `swap_test_hook`，以及原 `:2294–2301` 的 hook 说明 | `attachments.rs:1486–1562`；保留 `SwapWindow`、`Guard`、`set`、`notify`、唯一线程局部 `HOOKS`；父模块原路径仍仅在 `cfg(test)` 导出 |

子模块声明用 `pub(in crate::native_process)`，父模块用 `pub(super)` 重导出，保持原可见范围。三个兼容 helper 重导出局部允许未使用导入，避免非测试编译新增导入警告。生产依赖显式列出；共享错误构造仅导入 `super::failure`。父模块恢复 `std::io::Write as IoWrite`，供保留的两处 `writeln!(std::io::stderr(), ...)` 使用。未增加 `Services`、`Options` 或 assembly 依赖，未复制租约或 hook 状态。

`Services`、`compose_native_attachments` 的 profile/origin 能力选择与 `spawn_blocking`、`SubmitHostServices`、配置刷新、`ScopedServices` 留父模块。移除误附于 `ScopedServices` 的 hook 文档，并放回 hook 模块前。

静态字节核对：组合闭包仅有六处可见性声明变化；hook 仅有模块可见性和 `Path` 显式导入变化；其余迁移片段及父模块保留实现一致。测试文件未修改。原文件 SHA-256：`350300c669cd3b1081f9aba8dad74b4318868e5704775451a680f074b79136a7`。

风险与待办：跨模块可见性、unix / not(unix) 与测试 hook 挂载尚无编译证据；新源文件需协调者登记，冻结摘要未更新。既有 lazy cwd 缓存及租约身份边界残余说明原样保留，本次不宣称修复。

仅获准 cb8 资源/租约包装器内待跑：

- `cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib native_process::services_tests -j 4` — NOT_RUN。
- `cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib native_process::channel_attachment_security_tests -j 4` — NOT_RUN。
- 同一集成提交的 workspace 构建、既有十阶段完整回归、独立双审与真实 TS/native 同输入证据仍待协调；过滤单测不替代这些门。

本机未运行构建、测试、rustfmt 或语法检查，未 SSH；静态核对不计入验收通过。
