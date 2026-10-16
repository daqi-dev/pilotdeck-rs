---
name: runner-home
description: 隔离产品HOME时固定工具缓存并关闭自动工具链安装，防cargo --offline仍触发rustup下载。
---

2026-10-16 #139首版复现驱动把Node隔离HOME也给Cargo，未固定CARGO_HOME/RUSTUP_HOME；rustup先自动安装预编译工具链，再由cargo报告离线sha2缺失。违反本仓不下载预编译二进制红线。失败原件在主仓`_tmp/20261016-debt-cleanup/metadata/verified-run-fixed/`，下载现场保留待协调者处理；不把它算通过。

产品隔离环境和工具运行环境分开。先从原环境解析既有CARGO_HOME/RUSTUP_HOME；Rust调用显式固定二者，设置RUSTUP_AUTO_INSTALL=0，再加cargo --locked --offline。--offline只约束cargo，不约束rustup。未知/缺失工具链应立即失败，绝不自动安装或改全局默认。复现驱动须在manifest写明运行缓存和禁止自动安装，失败也执行自有权限恢复。

事故后的充分修复还须先只读核对原安装根、完整组件和原exe哈希，再直接绑定installed cargo及RUSTC/RUSTDOC；保留原HOME，Node单独隔离。仅AUTO_INSTALL=0不是充分条件。缺工具链负控用mock命令/环境，预检失败不能启动proxy；不真实尝试空HOME或缺失版本。事故现场与关联产物隔离保留，由协调者决定后续；恢复构建须先交只读preflight核验。
