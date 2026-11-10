# MuPDF 原生源码依赖边界（#2938，AI 判定，待独立复核）

- 卡点：原 registry C 依赖已能编译，但 JPEG 安全接口必须补绑定；本地副本的既有 build.rs / bindgen 输出超出 #132 的纯 Rust lib-only 守卫。不能通过移除守卫让构建变绿。
- 依据：用户要求完整原生 Gateway 替换、源码安装、禁止预编译下载；`decisions-20261110-ai-full-verdict-batch.md` 授权 AI 作判断并备案。原 #132 lib-only 决定继续适用于普通 vendored 包。
- 选择：仅 mupdf 与 mupdf-sys 固定 0.8.0 的原生源码 profile。完整 2869 文件及原包来源/许可在 vendor/mupdf-source.json；守卫再以常量 SHA 锁定清单，按实际目录全集比对文件/字节，拒绝增删、symlink、版本和 target 漂移。唯一 custom-build 是原样 mupdf-sys/build.rs，其 SHA 单独核定；另一个名为 build.rs 的文件只是已固定的普通 Rust 模块。
- 生成边界：mupdf-sys 原有 lib 的 bindgen OUT_DIR include 由已审原样构建脚本和全源 pin 约束，不以它为理由放行其它包的动态 include、build script 或 proc macro。业务 workspace 的 forbid unsafe 与依赖声明闭包守卫不变。上游 examples/tests targets 关闭但源码和许可证保留。
- 差量：JPEG80 安全 API；buffer 错误清理；C 初始化失败立即 return NULL、Rust 安装 callback 前核 NULL、clone 失败前释放锁；manifest 关闭示例/测试 target。每项在同一 manifest 记修改前后 SHA，构建脚本不变。
- 能否撤回：可恢复 registry 原包与两条 patch，但调用 JPEG 接口的产品代码须同时迁移；不影响用户运行中服务。所有验证仍在 cb8 隔离实例。
- 验证门：完整源码清单/路径/target 正负控，严格 lint，Gateway 相关及 transport/CLI 回归，固定 TS PDF 原件重放，错误清理的实际故障注入，provider probe 相同 patch/lock 构建，两家非作者增量评审。当前未宣称完成，也不新增任何对拍豁免。

原则：必须执行的原生依赖构建行为以精确、可复验的源码 profile 登记；不能把普通 vendored 目录整体变成不受检查的构建逃生口。
