
## #421-surrogate-p1 · AI 代批，待人复核

**卡在哪**：A7.1-pr3 连接层入站帧字段（id/method/clientName/sessionKey）的孤立 UTF-16 代理处理。修复轮 6 解决了 params 展开，修复轮 7 把入站解析改为 parse_json，但 jv_str 用 to_utf8() 导致含孤立代理的 id/method/clientName 被误判缺失（关 4002），同时 sessionKey 的 JsValue 真值与 serde round-trip 取值双轨不一致。boundary 角色连续 3 轮各角度命中（满足「同一失败 3 次挂起」原则）。

**谁受影响**：#421 合并阻塞。

**能否撤回**：可；代码改动全在 `_build/wt-a64-pr7`。

**动不动尺子/意向**：不动冻结文件；在意向范围内。

**选项与后果**：
- A（自决继续）：jv_str 改为 equals_str 比较，id/method/sessionKey 统一走 JsValue 路径。代价：context.rs SessionKey 也需改为基于 JsValue 的 SameValueZero，影响面扩大。
- B（挂起请人）：当前轮停止 #421 修复，人确认方向后再开。

**建议**：选 B。孤立代理的处理正确性边界（TS 接受代理项 id/method 后返回 error response 还是 invalid_frame）需要真实 TS 金样验证，否则会循环修。请人实测 `{"type":"request","id":"\ud800","method":"describe_server"}` 的 TS 行为，确认是 4002 close 还是 error response，再定改法。

**状态**：AI 代批 B，待人复核。

## A6.6-pr10 起：EdgeClaw memory 需新增 sqlite 依赖（暂不执行，待人批）
- **卡在哪**：TS 的 EdgeClaw 记忆核心（`resources/PilotDeck/src/context/memory/edgeclaw-memory-core/src/core/storage/sqlite.ts`，2004 行；整个 core 13251 行）用 Node 22 自带的 `node:sqlite`（:409，Bun 下走 `bun:sqlite`）把记忆存进本地 `.db` 文件。本机 Node 的内置 SQLite 是 3.51.2。Rust 侧 `src/Cargo.lock`（199 个包）里没有任何 sqlite crate，移植必须新增外部依赖。按 10-18 拆单约定（`_tmp/20261018-a6-split/triage.md:7`），新外部依赖要人批。
- **谁受影响**：A6.6-pr10 及之后约 5 个 PR（存储、检索、抽取、heartbeat、dream review），以及依赖它们的 memory 附件真实 provider 和 A6.9 总装。其余 A6 尾包（grep、插件 loader、stats）不受影响。
- **能否撤回**：可以，依赖单独在一个 PR 引入。但这属于供应链面的变更，而且 `.db` 文件格式一旦有用户数据，换实现就要做迁移。
- **动不动尺子 / 意向**：不动冻结尺子。完成口径沿用 D6-B（`docs/research/008_phase-a-session-domain/decision.md:81-91`，自决·待复核）：每个配置分支，成功和不可用两种情况都要验证。
- **选项**：
  - **A `rusqlite` + `bundled`**：crate 里自带 SQLite C 源码，本机用 `cc` 从源码编译，不下载预编译二进制。可以把 SQLite 版本钉成与 Node 内置相同的 3.51.x，`.db` 文件能与 TS 双向读写对拍。新增约 4–6 个包（rusqlite、libsqlite3-sys、cc、hashlink 等，以实际锁文件为准）；首次编译多 20–40 秒。
  - **B `rusqlite` 链接系统 libsqlite3**：包更少，但 macOS 自带版本和 Node 不同，Linux CI 还要装 `libsqlite3-dev`，跨机器行为难以钉死。
  - **C 纯 Rust 的 SQLite 重实现**（如 limbo/turso）：没有 C 编译，但还不成熟，文件格式兼容和功能覆盖风险大。
  - **D 不加依赖**：EdgeClaw 继续经 Node 宿主跑 TS。可以过渡，但按 D6 口径这块永远不算退役，TS gateway 下不了线。
- **建议**：A。只开 `bundled` 这一个必需 feature，依赖单独成一个 PR，锁文件变更在描述里单列，作法与 10-20 的 TLS 卡相同。
- **档位**：暂不执行（新外部依赖），等人批。批之前 EdgeClaw 不开工，memory 附件只接注入的 provider trait。
- **怎么批**：在协调者会话里直接回一句，例如「EdgeClaw sqlite 选 A」，或者选别的、加条件。批了以后，协调者把结论记进本卡「人批」一行，再派单。
- **原则**：新增外部 crate 一律单独成卡、单独成 PR（同 TLS 卡）。
- **人批（2026-10-22，用户本人在协调者会话）**：选 A——`rusqlite` 的 `bundled` 版本。依赖在 A6.6-pr10a 引入，锁文件变更在 PR 描述里单列，只开所需 feature，从源码构建。
