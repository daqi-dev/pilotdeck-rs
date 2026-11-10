# MuPDF 源码依赖的 JPEG 输出适配

`mupdf` 与 `mupdf-sys` 固定为 crates.io 0.8.0，原 VCS 为 `deaacc87c4cea6a2146e90e00e0d5658c911277b`。原始发布包完整保留，许可证与各子依赖版权文件留在原路径；原包 SHA-256、每个交付文件摘要与两处修改前后摘要的唯一清单是 [mupdf-source.json](mupdf-source.json)。此依赖携带的原始源码与测试资料约 70 MiB，最大单文件小于 7 MiB；未下载或提交预编译库。

只有两处窄修改：`mupdf/src/pixmap.rs` 增加安全方法 `write_jpeg80_to`；`mupdf-sys/wrapper/pixmap.c` 在既有、带 MuPDF 异常处理的 buffer 输出 switch 中增加内部值 5，调用 `fz_write_pixmap_as_jpeg(..., 80, 0)`。不新增公开 `ImageFormat` 枚举成员，以免原有 `save_as` 分派误用默认 PNG。业务 crate 仍禁止 unsafe；FFI 留在原有第三方绑定的边界内。删除两个 `[patch.crates-io]` 条目即可回到发布版，但 PDF 图像调用方也须同时移除或迁入已支持此 API 的上游版本。

固定 TS MuPDF 1.27.0 与此源包中的 MuPDF 1.27.2 并非同版本。cb8 以相同生成 PDF、100/72 缩放、DeviceRGB、质量 80 比较：双页 fixture 的两页及十页 fixture 的十页 JPEG 原字节分别完全相等；证据在 `_tmp/channels-finish/mupdf-jpeg-probe/{ts-jpeg,native-jpeg,ten-comparison.json}`。此结果只覆盖实际样本，不把它扩称为所有 PDF 或字体的等价证明。其余渲染与错误边界继续随 `builtin_tools` 真 TS 消费者验证。

Cargo 从上述源构建 C/C++ 依赖，需要 Make、C/C++ 工具链及 libclang 的开发头文件。产物目录、并发限制与项目其他 Cargo 构建相同。依赖排除在 workspace 自有源码格式和业务 lint 范围之外，原版文件不因本项目格式化而改写。
