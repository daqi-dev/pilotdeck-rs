# module-host approvals transport TS 金样

固定基线 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，Node v22.22.0 / ICU 78.2（生成器两项都断言，oracle 头记录 node 与 icu）。生成器以 `startGatewayServer` 起六个真实 TS 服务器：`main`（静态 binding + 真实 `createPublicApprovalBridge` + 真实 `StaffDeckSopControlPlane`/`SopStateStore`，stateRoot 下逐 session 播种状态文件，`sess-done` 先经真实控制面 resume 一次以产生回放用的 receipt）、`dynamic`（+ 脚本 `resolveBinding`，独立 stateRoot）、`nomap`（无 `resolveSession` 口）、`bare`（无 `publicApprovals`，503 cell）、`stub`（纯脚本 bridge 对象）与 `nobind`（空 binding 串）。`publicApprovalHttpTransport` 保持原样。每个 cell 用裸 socket 写入原始请求字节并原样捕获响应字节（250 ms 静默窗、EOF 或错误结束），Date 头掩码。运行前核 commit 与六个源文件的 SHA-256。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/module-host-approvals/generate.mts <独立输出目录>
cd src && cargo test -p pilotdeck-gateway --lib module_host::approvals
```

`oracle.json` 91 个 cell 记录 server、原始请求字节、响应字节（latin1 映射）与 closed/halfClose 标志。64 KiB 预算 cell 的请求体不落盘实料：oracle 只存 `__PDRS_PAD__` 标记与长度，重放方写入 `'a'.repeat(size)`。已测场景族：

- 鉴权（缺 / 错 / 小写 scheme / 双空格 / 双 `Authorization` 头取首值 / GET+错鉴权序）、方法（GET / DELETE / HEAD）、路由（尾斜杠 / 近似路径 / `%73` 百分号 / 点段 / 大写 / 查询串）；
- 坏 JSON / 空 / 数字 / 数组 / null / 字符串 body、字段白名单（status 4 项共享 + resume 6 项独占，越权字段 / 体内 approverAuthorization）；
- 传输层 scope（静态 binding 下 tenant / agent 不匹配 / 缺失 403）、approver 鉴权（缺 / 坏 scheme / 空余段 / `BEARER` 大小写不敏感 / 未知 bearer）、subject 检查（跨租户 / 禁用 / 非 web 源 / guest 角色 / 空 userId）、session 解析（未注册 / projectKey 不匹配 / sessionKey 不匹配）；
- 未配置面（nomap 503 / bare 503 / nobind 503）；
- store 的 status 快照（handoff admin/member/other、active、缺失 null、坏 pin、无 assignee、模块禁用 502、resume 后）；
- store 的 resume 路径（成功 member/admin、重复 receipt / message 冲突 / revision 冲突 409、external_task 源不匹配、wait 过期、revision 冲突、不存在、非 human 源、坏 message/revision/waitId、member 越权、`requestId:"constructor"` 走原型链命中 legacy receipt 分支 502）；
- 动态 binding（scope 跳过 / 换 binding 403 / 解析器抛错 403）；
- stub bridge 的结果序列化（快照 / null / undefined / scope / 双头 approver 注入与 Node 逗号拼接 / 自定义 status+code / 418 数字 code / 499 unknown reason / 裸 Error 默认 502 / 只有 code 无 status / resume 成功 / undefined 空体 / 409 冲突 / 413）；
- 64 KiB 预算（恰 65536 字节照常服务 / 超 1 字节零字节关闭）、Connection: close、HTTP/1.0（Node 1.1 状态行，继承差异 A8.4-MODULE-HOST-APPROVALS-001「继承差异」段）、keep-alive 双请求复用、提前断连（Node 解析器层 400 对零字节关闭，本面残余 A8.4-MODULE-HOST-APPROVALS-001「继承差异」段）。

**超预算的线上行为**：TS 的 `for await` 内抛出走 IteratorClose，销毁请求流后 'aborted' 先于 catch 的 `send` 触发（nextTick 早于 promise 微任务），catch 看到 `signal.aborted` 后 destroy 响应——线上是**零字节关闭**，传输自己的 413 `APPROVAL_INPUT_TOO_LARGE` 帧到不了线上（最小探针 `_tmp/20261105-issue-1350/ts-probe/probe.mjs` 实测 5/5 确定性）。Rust 侧 `ReadError::OverBudget` 对应零字节关闭，这是 TS 内部行为不是 hyper/Node 差异，未登记账本条目。

Rust 重放在 `src/module_host/approvals/tests.rs`：同一请求字节经真实 TCP 监听器打到 `http_proxy::serve_connection_native_with_shutdown`，approvals 组件前置（TS `GatewayServer.ts:96` 的 `handlePublicApprovalHttpRequest` 同位）、#1349 module-host 组件次之、#1209 路由器兜底 404；每个 server 的 bridge/store 按 oracle 的 server 字段重建（每测试全新 store + 种子 + `sess-done` 预 resume，mutating cell 各占独立 session，cell 顺序无状态）。响应按 #1209 规则归一化（Date 掩码、头名小写、空 `content-length: 0` 删除、chunk 大小小写）逐 cell 与金样比对，closed 标志另比。`http10`（HTTP/1.0 状态行，继承差异）与 `premature-close`（Node 解析器层 400 对零字节关闭，本面残余）——连同 `resume-requestid-proto`（TS 原型链命中 legacy receipt 502、Rust 自有键 200）——三项均 A8.4-MODULE-HOST-APPROVALS-001 已登记差异，为字节 pin，不进归一化比对；`replay_cells_cover_exactly_the_oracle` 断言重放列表 + pin 列表 = oracle cell 全集。组件级直测另含：空 token 401、resume 形状校验先于 session 映射、传输 resume 后同一 store 对 WS 侧可见（A8.4 要求）、`isDeepStrictEqual` 语义；fix-r1 直测族：`failure_body` 的 undefined 兜底必须是合法 JSON（`{"code":null}`）、pinned SOP 双侧严格相等与缺字段歧义、活跃会话晚到 resume 的 `SOP_NOT_WAITING` 502（传输路径）、无 authority 的 `SOP_APPROVAL_AUTH_REQUIRED`、handoff 缺 `expectedRevision` 的 `SOP_REVISION_REQUIRED`、legacy receipt（无 `authorization` 事实的 pre-migration 形状）重放的 `SOP_APPROVAL_LEGACY_RECEIPT`——后两条与前述 legacy 会话族（`sess-legacy`，Rust 侧 store 级种子）同属纯 store 级断言：TS 生成器未登记该会话（未注册会话在 `resolveSession` 即 403），无金样 cell、不宣称 TS 侧字节，双侧对拍留待后续单。

重生成守卫（`#[ignore]`）：`PDRS_BASELINE=… cargo test -p pilotdeck-gateway --lib module_host::approvals::tests::regenerates_identically -- --exact --ignored --show-output`。生成器输出目录递归创建，oracle.json 以 `flag:'wx'` 独占创建。除 Date 掩码外无时钟 / PID 依赖（stateRoot 目录名含 PID 但只在 tmpdir），未登记 regeneration mask。

负控：`negative_controls.py`（`_shared/mutant_switch.py` 编译开关执行器，17 个开关，每个绑定一个 replay cell 的单行专属断言或一个组件直测的单行断言）。新增 cell 时同步更新 `src/module_host/approvals/tests.rs` 的 `replay_tests!` 表与 `negative_controls.py` 的开关绑定。
