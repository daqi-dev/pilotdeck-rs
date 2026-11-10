# MuPDF 原生错误清理的实际注入

仅在 Linux 构建机运行，使用同一 checkout 已经从源码编译的 MuPDF 静态库，不下载产物：

```sh
python3 -I -B src/pilotdeck-gateway/tests/fixtures/native-pdf-boundary/check.py --library-dir /absolute/current-target/debug/build/mupdf-sys-ID/out/build --output /absolute/new-evidence-directory
```

输出独占创建。原 C wrapper、真实引擎静态库、夹具、编译命令和 SHA 全部留证。通过链接器替换两个调用点，实际触发文档 handler 初始化抛错和 JPEG 编码抛错；检查初始化返回 NULL，32 次失败后新增 buffer 的引用释放，以及恢复正常 JPEG 编码。计数以编码前实际存活 buffer 为基线，不把初始化的其他常驻 buffer 误判为泄漏。

两个私有源码负例分别去掉初始化失败 return、错误 buffer drop；原工作区不变。它们必须编译成功且被同一程序拒绝，原正例必须成功。结尾重核输入 SHA。本夹具只证明列出的错误路径，不替代 PDF 字节对拍、完整内存审计或 Gateway 正式验收。
