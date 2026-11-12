# Rust Gateway 重构执行方案

目标：在保留固定 TS 行为契约的前提下，缩小产品模块的职责与修改范围，加快整合、定位与回归。最终目标仍是完整替换验收；文件拆分或静态审查不是替换通过。

当前开发位置：本目录。源身份、原快照位置与执行边界见 DEV-STATUS.md、DEV-CONSTRAINTS.md。正式基线为 Rust 7d7c0ab2dad32b4c71bb2ed465147cdff179f374、TS cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85。

## 第一波：三个互不覆盖的产品边界

| 线 | 当前问题 | 交付与文件归属 | 行为约束 |
|---|---|---|---|
| HTTP | native_http.rs 同时含路由、Node HTTP 兼容、响应体及静态文件流，957 行 | 拆出 native_http/response.rs 与 static_assets.rs；仅该线修改 native_http.rs 和新子模块 | 保留路由优先级、状态/头顺序、chunk/EOF、HTTP/1.0、HEAD、断开行为；原调用路径保留 |
| 附件服务 | services.rs 2459 行，附件解析与租约保护混在提交/配置服务中 | 将完整附件组合实现移入 native_process/services/attachments.rs；仅该线修改 services.rs 和新子模块 | 保留句柄检查、路径/租约边界、错误文本、UTF-16及大小上限；不改变权限与提交生命周期 |
| 启动配置 | native_process.rs 将进程组装与选项解析混在一起 | 拆出 native_process/options.rs；仅该线修改 native_process.rs 和新子模块 | Options 与模式选择 API 保持原路径；保留参数优先级、环境校验、错误顺序，不统一掉有意不同的 host 默认值 |

第一波采取职责内聚的移动与必要私有导入调整；不引入抽象框架、第二份状态或备用实现。每条线只移动它能证明依赖闭合的代码，不以减少行数强切模块。

独立审查后的边界优化：

- HTTP：`file_response`、`FileBody`、`CONTENT_TYPES` 和静态路径函数归 `static_assets`；`StreamedBody`、`Unsized` 与通用响应归 `response`；路由和 admission 留父模块。保留 `Unsized` 必要构造可见性及原测试对路径函数的访问。
- 附件：仅抽取附件组合闭包，`Services::compose_native_attachments` 的 profile/origin 能力检查与阻塞调度留原处。五个内部共享函数维持 `native_process` 范围；测试 hook 及唯一线程局部状态一起移动。不得复制租约或状态。
- 配置：保留原 `Options` / 模式函数导出路径，`parse_at`、`serves_project` 只增加跨本层模块所需的最小可见性；环境校验与父模块测试导入显式列出。保留测试通过 `super::*` 使用的基础类型。

## 协调者同时收口的整合工作

1. 完成公共 HTTP 判分的剩余拒绝漏口和已识别反例；既有补丁按功能去重，不继续扩散到无关问题。协调者仅写 `scripts/gateway-parity/`、公共 HTTP fixtures、共享登记及开发说明，不与 HTTP 线争写 `native_http.rs`。
2. 把 TS 渠道入口、HOME/PILOT_HOME、项目身份及原始材料对齐；真实依赖或官方启动方式未核清时标 BLOCKED。CAPTURED 只代表采集。
3. 独占共享注册和 FROZEN。登记新增源文件/控制入口；在获准 cb8 的校准、回归、独立双审以前不更新冻结摘要以制造已验收状态。

## 合流与第二波条件

- 三条线每条只回传：变更文件、边界变化、被保留的入口、风险及待运行检查。独立静态复核只报可复现问题。
- 判分修复、TS 采集修复、产品职责迁移分别生成变更清单与原始/当前内容哈希，避免无法归因的混合补丁；用初始来源清单生成各组补丁。
- 在获准位置应用这些补丁并建立可追溯的集成提交后，才能绑定二进制与运行证据。本轮已把快照集成到完整 Git 工作树；不修改只读正本或借用旧提交身份。
- 首批产品改动合流后，先核导入可见性、cfg、测试挂载和函数体移动前后一致性；这些是静态核对，不冒充编译。
- 构建与所有测试只在 cb8。本轮已按此前用户授权接通 cb8；保持现行资源/隔离边界，不通过其他工具、终端或会话绕行。
- 获准执行后，先同一集成提交的编译/相关回归，再补真实 TS/native 同输入证据；根据实测失败选择下一批产品改动。未编译的产品重构不继续叠加第二波。
- 三线待跑目标：cb8 现行资源/租约包装器内执行 `cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib <过滤器> -j 4`；HTTP 使用 `native_http::tests`，附件使用 `native_process::services_tests` 与 `native_process::channel_attachment_security_tests`，配置使用 `native_process::tests`（含 `native_process::tests::review`）及 `native_process::management_tests`。同时覆盖调用这些公开入口的 workspace 构建及既有十阶段完整回归；过滤单测不替代这些门。
- 配置的真实进程入口另跑 `cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --test native_process native_entry_process_rejects_inherited_authority -j 4`；`--lib` 的环境校验测试不能替代它。
- 函数移动同时更新既有负向控制的源文件定位与源身份保护：HTTP 的响应/静态开关分别绑定新模块，附件文本锚点绑定 `services/attachments.rs`；保留相关父模块的身份保护。不得放宽锚点匹配次数或跳过负向控制。
- 每线补交函数/类型移动映射、保留导出/测试 hook 列表及具体风险；独立审查者不得参与对应补丁编写。TS 依赖阻塞不阻止第一波产品代码交付。
- 最终按同一版本完成 150 入口、四 host、session、retirement、冷切回滚、完整回归和独立双审；A7 已拒绝范围仍 NOT_RUN。推送前 gitleaks，PR 保持 Draft，不切用户服务。

## 验收与加速指标

开发阶段只报告实际代码：哪些职责已独立、哪些调用入口保持、哪些问题已修、哪些检查未跑。第一波的目标是让后续 HTTP、附件、启动配置改动能独立阅读和修改，而不争用大文件。

正式通过必须有当前集成提交及二进制/依赖身份相符的原始证据；静态复制、旧头成绩、编译成功和 CAPTURED 均不计入 PASS。

状态：方案 v4（接续执行）。三条产品线已完成源码迁移，按独立静态审查补齐日志 trait 导入、负向控制源定位和真实进程测试映射。渠道采集修正已完成独立静态复核；各项完成情况以 DEV-STATUS.md 为准。完整 Git 集成及首轮 cb8 构建/回归已执行，仍有真实失败及正式验收缺项；具体版本与结果见 DEV-STATUS.md。
