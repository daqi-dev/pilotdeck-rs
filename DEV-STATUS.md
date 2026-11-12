# Gateway 接续开发状态

本轮已将先前摘取快照整合为真实 Git 提交，完成第一波模块拆分，并根据 cb8 回归与独立审查修复实际问题。完整 Gateway 替换验收仍未完成；本文件不授予 PASS，也不授权切换用户服务。

## 代码与证据位置

- 本机开发树：`/Users/da/.codex/worktrees/gateway-wave1-completion/pilotdeck-rs`，分支 `codex/gateway-wave1-completion`。
- cb8 执行树：`/home/clawbox-08/ws/pdrs-worker-wt/gateway-integration-20261110`，同名分支，树内独立 `_build/cargo-target`。
- Rust 起点：`7d7c0ab2dad32b4c71bb2ed465147cdff179f374`；固定 TS：`cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。
- 45 个交付文件和 6 份补丁已逐哈希核验，首个集成提交为 `1ddb1221069a9fbb11c398177813cd137c8f20c9`。原件仍在 `/Users/da/ws/pilotdeck-rs-codex/gateway-dev`，只读源未改。
- 本轮构建、测试、语法和格式检查全部在 cb8；本机仅编辑、静态阅读、哈希、Git 和只读模型审查。
- 原始运行材料在两端各自树内 `_tmp/20261112-wave1-validation/`；最终运行索引为该目录的 `validation-latest.json`（未产生时即尚未闭合）。每条结果须看自己的 HEAD、命令、退出码与原始日志，不合并不同版本的成绩。

## 已整合代码

| 范围 | 行为与边界 |
|---|---|
| HTTP | 响应与静态资源分别移入 `native_http/response.rs`、`static_assets.rs`，保持原路由入口 |
| 附件 | 组合逻辑移入 `native_process/services/attachments.rs`；能力检查、提交生命周期与阻塞调度仍由父模块控制 |
| 启动配置 | 选项与环境校验移入 `native_process/options.rs`，保留原导出、参数优先级及组装顺序 |
| 迁移证明 | 既有负控锚点绑定新文件并保留父文件身份保护；补齐 webhook 源码守卫及产品覆盖引用 |
| 公共 HTTP | 完整保留状态行、重复头、正文、同 socket 原字节与模型交换；Docker 运行见证须实际 Running 且 PID 为正 |
| 渠道 v2 | 两轮真实入站、权限/问答续接、reload、当前轮公开历史及停机落盘；进程、依赖、角色源码和保留目录均需对应原件 |
| 渠道路由与关联 | Discord 保留 `/api/v10`；飞书 text/post 与 PUT 绑定实际 message_id；同 socket 双向记录保持完整顺序 |
| 失败报告 | 固定 TS 的 Git 对象读取失败或超时落到具体渠道的 `ENTRY_CHANNEL_SOURCE`，其余渠道仍产生结果 |
| 监听器关闭 | API/SMS 已实测修复 Linux 继承描述符导致 stop 后仍可连接；同模式延伸到 webhook、wecom_callback、native WS、HTTP store、execute_code RPC，保留各自原 drain 时序并传播关闭错误，新增真实入口 dupFD/rebind 控制 |

最后一行的延伸修复及新增边界控制还须以下最终提交的 cb8 验证，静态复核不算运行通过。

## 已取得的真实结果

| 检查 | 观察结果 | 实际版本/原件 |
|---|---|---|
| workspace build/check、严格 clippy | PASS | `983e23a31`，对应 `workspace-*.log`、`clippy.log` |
| native HTTP | 138 通过、1 忽略 | `1ddb12210`，`native-http.log` |
| API Server | 69 通过、1 忽略；dupFD 修复的断言级红/恢复控制通过 | `74f61521b`，`api-server.log`、`listener-stop-negative.json` |
| SMS 头期限/关闭 | 9 通过、2 忽略；有效 dupFD 红/恢复控制通过 | `029efa413`，`sms-head-timeout.log`、`sms-listener-stop-r2-negative.json` |
| native_process 广域回归 | 763 通过、0 失败、69 忽略 | `029efa413`，`native-process-after-sms-fix.log` |
| 实际进程权限拒绝、loopback 端点 | PASS | 对应 `native-authority.log`、`loopback-origins.log` |
| 渠道 v2/来源/reference | 41 / 12 / 9 项通过 | `68c187fb5`，`channel-*-reviewed.log` |
| 公共 HTTP / 完整 HTTP / 项目时钟范围 | 25 / 12 / 13 项通过 | 对应 `public-http.log`、`standard-http.log`、`project-clock.log` |
| safety | 153 项通过，1 项被旧 FROZEN 阻塞；不重写冻结检查 | `safety-reviewed.log` |
| 同头十阶段构建 | 全部通过：fmt、严格 clippy、4 bin、各指定 no-run 和 workspace inputs | `94b79f40c`，`common-94b79f40c-verify-wave120261112T070412Z/results.json` |
| 完整隔离回归 | r3 在准备预检失败，测试未开始；83 迁移负控未启动 | 固定 TS 目录既有 `trace.cpython-312.pyc` 未跟踪缓存；完整原字节、inode、mtime 已保全移出，tracked TS 未改，待最终头重跑 |
| 审查后证据修复 | 来源控制 14 项、CheckoutProvenance、webhook `--check`、公共 HTTP 控制通过 | `c6c06d504`，`evidence-review-c6c06d504/results.json` |
| 产品覆盖 | PASS，678 entries、0 errors | `c6c06d504`，`product-coverage-deps-20261112T072438Z-ee5c4e87/`；20 个固定纯 JS 工具依赖由离线缓存安装，未执行安装脚本 |
| Docker 状态仲裁 | 真实 create 后 inspect exit0 但 Running=false/Pid=0；start 后 Running=true/Pid>0；本次容器已移除 | `scratch/docker-inspection-20261112T071555Z-111bbddd/` |

所有历史失败原件保留，包括首轮系统头缺失、API/SMS 关闭失败、第一次 SMS 变异的编译错误、磁盘配额拒绝、四处格式失败和 r3 准备预检失败。只有 `sms-listener-stop-r2-negative.json` 是有效的 SMS 断言级负控；初次同名报告不能计通过。

## 独立审查

GLM 与 DeepSeek 按产品/验收装置两个互不重叠范围审查，完整覆盖审查头 `029efa413` 的 60 个变更文件。16 角色均有实际 final，严格运行器为 15 PASS、1 PARTIAL（DeepSeek product sibling 的覆盖行格式不符）；PASS 仅表示审查有效完成，不表示无问题。

26 条原始 P0/P1/P2 意见归并为 21 组，原件与逐条核对在 `review-findings-triage.json`、`.md`。已确认的源异常、Docker 快照、守卫、入口引用与同类监听器问题正在同批收口。v1 路径被误连到 v2、当前 HTTP profile 并不存在的空响应要求有具体反证；不据此放宽比较器。当前修复仍需两谱系增量复核，不能沿用旧头结果块冒充最终头通过。

## 正式验收仍缺

- 固定 TS 的 Telegram、Discord、Slack、钉钉：真实源码 tsx 与官方 dist 均出现 ESM bare `require` 错误，对应可选 SDK 也缺失。仅安装 SDK 不会修复该加载缺陷；`ts-reference/` 保留实际原件，未注入全局 require 或替代 SDK。
- v2 应用层结果名为 `semanticComparison`。HTTP 同 socket 原字节绑定与 WS upgrade/帧头验证仍缺，不能把 TCP chunks 或语义一致当完整业务对拍通过。
- 无稳定实体锚点的 UUID、业务头、轮询/心跳和调用次数保持原比较口径；不能为绿色结果删字段。
- FROZEN 原字节保持。实际 `frozen-check.json` 还包含起点已有 registration-base blob 不匹配；需合法校准、复算、独立审查和登记，不能直接 `--write` 清除阻塞。
- 当前最终头完整隔离回归、83 条迁移负控及新增监听器正/红/恢复控制、被影响的长耗时测试、独立双审尚待原件；默认 ignored 必须逐名处置，不能汇总为已通过。
- 全部 150 入口、四 host、session、retirement、冷切回滚与真实使用验收未闭合。A7 先前被拒绝的范围保持 NOT_RUN，不重试或绕过。
- 推送前重新 gitleaks；PR 保持 Draft；用户服务不切换。未通过的完整替换不标完成。

下一步：最终修复提交在 cb8 格式化、构建和完整回归，完成受影响负控与两谱系增量审查，写入当前运行索引，再按固定基线和既定批准程序推进剩余正式验收。
