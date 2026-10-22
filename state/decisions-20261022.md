
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

## A6.4-pr9b：账目保留判断不再手写 V8 Date.parse，只认规范格式（自决 · 待复核，AI 代批，待人复核）
- **卡在哪**：TS `TokenStatsCollector.ts:297-302` 用 `Date.parse(record.endedAt)` 判断账目是否过期，解析不了的（NaN）永远保留。Rust 修复轮 1 手写了一套 V8 Date.parse 的子集，codex r2 随即又找出 3 个 P0：多字节偏移切片会 panic；`00:00.500Z`、`24:00:00.0001Z` 这类非法时间被判为过期而删账；本地时间的夏令时推算与 V8 不同，会提前删账。V8 Date.parse 的兼容格式太多，逐个补是同一处改 3 次以上的循环。
- **谁受影响**：只有 stats JSONL 的保留和压缩。数据由 TS / Rust 自己写入，`endedAt` 是 `toISOString()` 的规范格式。
- **能否撤回**：能，改的是纯函数。
- **动不动尺子**：不动冻结件。属于行为差异：非规范但 V8 能解析的时间戳，TS 可能会删，Rust 保留。差异方向是多留，不是少留。
- **选项**：A 继续补全 V8 Date.parse（含本地时区与夏令时），成本高、风险是误删；B 只精确解析规范的 `YYYY-MM-DDTHH:mm:ss.sssZ`（以及 TS 实际会写出的其他格式，以源码核实为准），其余按「无法解析 → 保留」处理；C 保留现状。
- **选择**：B。先例是 `runtime/model/http/errors.rs:121`，那里只移植 IMF-fixdate，其余返回 None。
- **原则**：会删数据的路径，宁可多留，不可误删；移植 JS 宽松解析器时，只移植基线自己产出的格式，其余按「无法解析」的既有分支处理，并写明差异。

## 人定：资源上限再放宽 + 并发上调（2026-10-22，用户本人在协调者会话）
- **磁盘**：`_build+_tmp` 上限 100 → **160 GiB**（CLAUDE.md「资源上限」、`scripts/pdrs-loop.sh` DISK_CAP_KIB 同步）。冻结装置 `calibrate.py` 的 `CAP_BYTES` 仍 80 GiB，走冻结程序另改。
- **GLM 并发**：人看到两闸门 8 路实测通过后提议再往上试；三闸门同时跑（12 路）也正常出块，采样器 `_tmp/20261022-loop/glm-conc.log` 无 API 错误。闸门默认四角色并行，`--serial` 只在排查时用。
- **dsh 并发**：人提议可试到 12。DeepSeek 单 key 50 RPM，16:54 约 6 个并发时撞过 429，所以先设 8（`dsh-slot.sh`），并加 `dsh-retry.sh`：撞 429 后等 60 s 带续跑说明重跑，最多 3 次。429 次数记 `_tmp/20261022-loop/dsh-429.log`，若不频繁再上调到 12。
- **磁盘清理（人定）**：`_build/20261016-*` 选 A，只删 deps/build，保留可执行文件、.d 与 fingerprint，先记 sha256 清单 `_tmp/20261022-loop/build-20261016-manifest.txt`；`_tmp/archive` 按目录压缩为 tar.xz，逐个核对后删原目录（zstd 被系统 SIGKILL，改用 bsdtar 内置 xz）。
- **原则**：并发上限按实测的真实限制设，GLM 按并发数限、DeepSeek 按每分钟请求数限，两家分开定；被限流的失败自动续跑，不计入「同一失败 3 次」。

