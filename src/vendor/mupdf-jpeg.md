# MuPDF 源码依赖的 JPEG 输出适配

`mupdf` 与 `mupdf-sys` 固定为 crates.io 0.8.0，原 VCS 为 `deaacc87c4cea6a2146e90e00e0d5658c911277b`。原始发布包的文件均保留，许可证与各子依赖版权文件留在原路径；原包 SHA-256、每个交付文件摘要与五个修改文件的前后摘要唯一清单是 [mupdf-source.json](mupdf-source.json)。此依赖携带的原始源码与测试资料约 70 MiB，最大单文件小于 7 MiB；未下载或提交预编译库。

修改限于：`pixmap.rs` 增加安全方法 `write_jpeg80_to`；`wrapper/pixmap.c` 增加 JPEG80 输出并在异常时释放 buffer；`wrapper/context.c` 在初始化失败后先标记异常已处理、释放 context 后立即返回空指针，销毁 mutex 前完成 context 清理；Rust `context.rs` 在安装回调前核空指针、克隆失败 panic 前释放基础 context 锁；`mupdf/Cargo.toml` 禁用上游 examples/tests target（原文件留存）。不新增公开 `ImageFormat` 枚举成员，以免原有 `save_as` 分派误用默认 PNG。业务 crate 仍禁止 unsafe；原 `mupdf-sys/build.rs` 未修改。两包通过全源清单常量 pin 的窄例外构建，见 `state/decisions-20261111-mupdf-source-boundary.md`；其它 vendored 包的 lib-only 规则不变。删除两个 `[patch.crates-io]` 条目可回到发布版，但调用方也须同时迁入支持此 API 的版本。

固定 TS MuPDF 1.27.0 与此源包中的 MuPDF 1.27.2 并非同版本。cb8 以相同生成 PDF、100/72 缩放、DeviceRGB、质量 80 比较：双页 fixture 的两页及十页 fixture 的十页 JPEG 原字节分别完全相等；可复核原件及摘要已取回主仓 `_tmp/20261111-gateway-closeout/pdf-evidence/RESULT.md`（111文件 manifest）。原 cb8 路径 `_tmp/channels-finish/mupdf-jpeg-probe/` 保留。此结果只覆盖实际样本，不把它扩称为所有 PDF 或字体的等价证明。后续异常路径修复还须重新运行这些消费者。

Cargo 从上述源构建 C/C++ 依赖，需要 Make、C/C++ 工具链及 libclang 的开发头文件。产物目录、并发限制与项目其他 Cargo 构建相同。依赖排除在 workspace 自有源码格式和业务 lint 范围之外，原版文件不因本项目格式化而改写。

错误清理实际注入见 `src/pilotdeck-gateway/tests/fixtures/native-pdf-boundary/README.md`。cb8 首轮夹具误计初始化常驻 buffer 的失败原件保留；修正计数基线后，真实初始化失败、32 次编码失败和恢复编码通过，两项私有删除修复的负例均被实际断言拒绝。
