# 启动配置迁移映射

| 原 `native_process.rs` 符号 | 新位置与边界 |
|---|---|
| `Options`、`native_requested`、`gateway_mode` | `options.rs`；父模块 `pub use` 保持原 API 路径 |
| `Options::{parse, environment}` | `options.rs`；原可见性及函数体保持 |
| `Options::{parse_at, serves_project}` | `options.rs`；仅 `pub(super)`，可访问范围仍为 `native_process` 及其子模块 |
| `absolute`、`directory`、`file`、`utf8_argument` | `options.rs` 私有解析辅助 |
| `capture_environment`、`validate_environment` | `options.rs` 的 `pub(super)` 函数；父模块私有显式导入，保留 `run` 及既有测试入口 |

`run`、`production_api_web`、`production_listener`、`NativeTurnEnvironment` 与测试挂载留父模块；测试依赖的 `Env`、`Path`、`Duration`、`Ipv4Addr` 在父模块以 `cfg(test)` 显式导入。新模块不使用 `include!` 或 `super::*`。

迁移保留参数优先级、默认值、环境校验顺序和错误文本；仅调整跨模块所需可见性与导入。风险为模块可见性及各 cfg 组合尚未经编译验证。

待获准 cb8 环境内运行：`cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib <过滤器> -j 4`。
既有有效过滤器：`native_process::tests`、`native_process::tests::review`、`native_process::management_tests`；后者直接调用 `validate_environment` 及 assembly，覆盖空存储配置等场景，不等同于经过生产 `run` 入口。`review_tests.rs` 实际由 `tests.rs` 的 `mod review` 挂载，`native_process::review_tests` 不是现有测试路径。
生产 `run` 入口的继承权限拒绝场景另待运行：`cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --test native_process native_entry_process_rejects_inherited_authority -j 4`。
另需同一集成版本的 workspace 构建、既有十阶段完整回归和独立双审；过滤单测不替代这些检查。
状态：仅完成静态源码迁移与文本核对；构建、测试、rustfmt、语法检查及远端执行均 NOT_RUN，未宣告验收通过。
