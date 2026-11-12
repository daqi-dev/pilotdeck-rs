# Gateway 第一波接续开发状态

本轮已从摘取快照回到完整 Git 工作树，并在用户授权的 cb8 环境执行实际构建和回归。源码集成与构建已发生；完整 Gateway 替换验收仍未完成，本文件不授予 PASS。

## 代码与执行位置

- 本机开发树：`/Users/da/.codex/worktrees/gateway-wave1-completion/pilotdeck-rs`，分支 `codex/gateway-wave1-completion`。
- cb8 执行树：`/home/clawbox-08/ws/pdrs-worker-wt/gateway-integration-20261110`，使用同名分支和树内 `_build/cargo-target`。
- Rust 起点：`7d7c0ab2dad32b4c71bb2ed465147cdff179f374`；固定 TS：`cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。
- 原快照与来源清单保留在 `/Users/da/ws/pilotdeck-rs-codex/gateway-dev`。45 个交付文件及 6 份补丁均已核对原始/交付哈希，首个集成提交为 `1ddb1221069a9fbb11c398177813cd137c8f20c9`。
- 本轮构建/测试只在 cb8 运行。本机仅静态阅读、编辑、哈希、Git 与只读模型审查。没有切换用户运行中的服务。

## 已集成的开发内容

| 范围 | 实际变化 |
|---|---|
| HTTP | 响应构造与静态资源服务移入 `native_http/response.rs`、`static_assets.rs`，保持调用入口和兼容语义 |
| 附件 | 组合逻辑移入 `native_process/services/attachments.rs`，能力检查、提交生命周期和阻塞调度保留在父模块 |
| 启动配置 | 选项解析和环境校验移入 `native_process/options.rs`，保留原导出、参数优先级及进程组装顺序 |
| 迁移控制 | 既有负控绑定真实新文件，原父模块继续受源码身份保护；未删除原锚点或拒绝规则 |
| 公共 HTTP | 逐请求绑定真实响应和同 socket 原字节；完整保留状态行、重复头、正文与模型交换 |
| 项目时钟 | `createdAt` 仅在已批准的完整项目摘要形状归一化，业务数字与其它字段继续比较 |
| 渠道 v2 | 新消费者核对两轮真实入站、权限/问答续接、reload、当前轮公开历史和停机落盘；接入来源、进程、依赖、容器与保留目录证据 |
| 渠道适配 | 飞书支持真实 text/post 以及绑定已创建 message_id 的 PUT；Discord 保留 `/api/v10`；按 socket 保留流内双向顺序，仅排除跨连接调度和观察器编号 |

## 已取得的 cb8 结果

下列结果绑定各自日志中的 HEAD，不借用旧头分数。后续修复须运行受影响回归；完整回归与负控结果以本轮产物为准。

| 检查 | 已观察结果 | 版本/原件 |
|---|---|---|
| workspace 全量构建 | PASS | `983e23a31`，`workspace-build.log` |
| workspace 全目标 check | PASS | `983e23a31`，`workspace-check.log` |
| fmt | PASS | `983e23a31`，`fmt.log` |
| native HTTP 单测 | 138 通过、1 忽略 | `1ddb12210`，`native-http.log` |
| native_process 广域回归 | 762 通过、1 失败、69 忽略；API Server stop 后端口仍可连接，继续定位/修复 | `983e23a31`，`native-process-fixed.log` |
| 真实进程拒绝继承权限 | PASS | `983e23a31`，`native-authority.log` |
| loopback 端点 | PASS | `983e23a31`，`loopback-origins.log` |
| 公共 HTTP 控制 | 25 通过 | `public-http.log` |
| 完整 HTTP 控制 | 12 通过 | `standard-http.log` |
| 项目摘要时钟范围 | 13 通过 | `project-clock.log` |
| 渠道 reference 控制 | 8 通过；新增 Discord 控制尚待新头运行 | `channel-reference.log` |
| v2 消费者控制 | 初版 26 通过；飞书/跨 socket 新控制待新头运行 | `channel-v2-initial.log` |
| 证据来源控制 | 初版 5 通过；新增角色/覆盖边界控制待新头运行 | `channel-provenance-initial.log` |
| safety | 154 项中报告 8 个失败：7 个为新增模块登记/入口类型遗漏，已修待复验；1 个 deadline 被未重新校准的 FROZEN 阻塞 | `safety-integrated.log` |

原始日志位置：本机和 cb8 各自树内 `_tmp/20261112-wave1-validation/`。首轮缺少 libclang 系统头的构建失败保存在 `native-http-missing-clang-headers.log`；后续使用宿主现有 GCC 13 系统头完成构建，没有安装或下载工具链。

## 正式验收仍保留的缺项

- 四个固定 TS 可选渠道（Telegram、Discord、Slack、钉钉）：真实 Node 22 + tsx 以及官方 dist 都观察到 ESM 中 bare `require` 的异常；对应 SDK 也未安装。没有注入全局 require、替身 SDK 或修改固定基线。原件在 `ts-reference/`。
- v2 的应用层比较记为 `semanticComparison`，不能解除完整业务对拍缺项。HTTP 同 socket 原字节绑定和 WS upgrade/帧头验证尚缺；仅保留 TCP chunks 不能算完成验证。
- 当前 normalization 保留无稳定实体锚点的 UUID、业务 header、轮询/心跳与调用次数。真实独立 TS/native 采集可能产生差异，不能由合成正例或删除字段宣称一致。
- FROZEN 保持原字节。重新登记源码不等于冻结校准，deadline 的真实 BLOCKED 不能通过改哈希或替换冻结检查变绿。
- 当前头的 83 条迁移负控、完整隔离回归、150 入口、四 host、session、retirement、冷切回滚和正式 GLM + DeepSeek 双审仍需各自原件。NOT_RUN、BLOCKED、FAIL、PASS 分开报告；第一波尚未闭合前不叠加第二波产品重构。
- A7 先前已被拒绝的范围保持 NOT_RUN。推送前 gitleaks，PR 保持 Draft，用户服务不切换。

下一执行顺序：修复真实回归失败并提交；同步干净 HEAD；在 cb8 现有 network-none、cargo 配额及源码锁边界内完成同头构建、完整回归、受迁移影响负控；完成独立审查；再按固定 TS 实测与既定校准流程推进剩余验收。
