# 原生图片源码构建与交付边界（#2940，AI 判断，待独立复核）

- 卡点：固定 TS 图片成功分支使用 libvips 编解码，Rust 原只接受完整 JPEG；完成 Gateway 替换必须补成功压缩与 PDF 超预算路径，不能以 unavailable 隐藏已配置功能。
- 选择：13 个固定上游源码归档随源代码交付，最大单件约 28.2 MiB、总计 58,647,062 bytes；源码安装离线可重建，不下载预编译库。完整来源、许可和 SHA 以 `src/vendor/image-sources.md`、`image-source-provenance.json` 为准；vips URL 本轮复核超时的事实保留。
- 构建：只读取包内固定归档，哈希通过的字节写入私有快照，再构建原生库；新库 profile 包含来源、HOST/TARGET 和输出文件名及实际字节。CLI/Gateway 相对自身加载同批库，源码安装将其加入同一 generation，逐文件哈希、私有目录与清单核验继续生效。旧无库字段 generation 保持读取兼容。
- 边界：新 Rust 窄包和原生构建包继承 workspace forbid unsafe；上游 libvips 绑定按完整 31 文件清单、固定版本与 lib/custom-build target 单独登记。构建入口、实际 stage helper、Cargo 与各 shell/Python 构建脚本分别 pin；普通 vendored 包原规则不变。没有允许 arbitrary build.rs、动态 include 或目录外源。
- 已复验但不代表本整合头通过：隔离源码编译、Linux 重定位、图片 9 原格及 PNG/WebP/GIF 压缩、私有 source snapshot 与额外文件/symlink 拒绝。当前组合还需新头严格检查、四 bin、安装负例、真实图片工具、完整 Gateway 回归和两家非作者评审。macOS 专项未跑，不用 Linux 结果代填。
- 授权与可撤回：用户完整 Gateway / 源码安装要求及 `decisions-20261110-ai-full-verdict-batch.md` 判断授权；可撤回，未切用户服务、未改固定 TS 和对拍判分。不将受信 checkout 持有人间禁止并写扩成恶意本机攻击模型；SHA snapshot 保证实际编译字节仍受 pin。

原则：原生第三方依赖需以可重建源码和明确执行闭包登记，不能把目录或构建脚本整体变成免检区域。
