---
name: runner-home
description: 隔离产品HOME时固定工具缓存并关闭自动工具链安装，防cargo --offline仍触发rustup下载。
---

2026-10-16 #139首版复现驱动把Node隔离HOME也给Cargo，未固定CARGO_HOME/RUSTUP_HOME；rustup先自动安装预编译工具链，再由cargo报告离线sha2缺失。违反本仓不下载预编译二进制红线。失败原件在主仓`_tmp/20261016-debt-cleanup/metadata/verified-run-fixed/`，下载现场保留待协调者处理；不把它算通过。

产品隔离环境和工具运行环境分开。先从原环境解析既有CARGO_HOME/RUSTUP_HOME；Rust调用显式固定二者，设置RUSTUP_AUTO_INSTALL=0，再加cargo --locked --offline。--offline只约束cargo，不约束rustup。未知/缺失工具链应立即失败，绝不自动安装或改全局默认。复现驱动须在manifest写明运行缓存和禁止自动安装，失败也执行自有权限恢复。

事故后的充分修复还须先只读核对原安装根、完整组件和原exe哈希，再直接绑定installed cargo及RUSTC/RUSTDOC；保留原HOME，Node单独隔离。仅AUTO_INSTALL=0不是充分条件。缺工具链负控用mock命令/环境，预检失败不能启动proxy；不真实尝试空HOME或缺失版本。事故现场与关联产物隔离保留，由协调者决定后续；恢复构建须先交只读preflight核验。

后续实测：仅pop RUSTC_WRAPPER两键会重新暴露Cargo配置及CARGO_BUILD_*两个别名，四格harmless spy均实际执行（只写自有marker并退出86，无rustup）。驱动现在把RUSTC_WRAPPER、RUSTC_WORKSPACE_WRAPPER及对应CARGO_BUILD_*四键明确置空，覆盖配置而非删除环境；mock覆盖继承非空值。依据[Cargo环境变量文档](https://doc.rust-lang.org/cargo/reference/environment-variables.html)的空串重置语义；实际前后spy证据在主仓reviews/metadata-wrapper-*。旧成功运行的真实环境无wrapper，不因此否定其数据；未来护栏修复仍交独立审阅。

PR141后续护栏：构建环境改为必要白名单，不能先复制整个环境再只删四wrapper；
RUSTFLAGS/CARGO_*及动态加载注入也会改变验收。原工具和列明source必须全部存在，
before/after重哈希；原rustup三子树只读type/size/mtime/link摘要用于本次新增/漂移检测，
不追认事故历史。restore要消费owner/count并stat实际权限，exit0或no-op不是恢复证明；
超时/输出上限用自有Python子进程负控，不拿缺安装触发真实下载。旧P0仍由两家独立
审阅收敛，失败原件和下载现场继续保留。
