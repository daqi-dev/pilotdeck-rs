# S7 膨胀候选清单（只登记，不删代码）

> 2026-10-14 · 阶段 C Issue #3 S7。入选标准：对应表（`mapping.md`）里判 **对不上**，或判 **部分** 且比值 > 2.5。共 11 项，按「多出行数」（Rust 产品行数 − 映射 TS 行数）降序。
> **规矩（计划 S7）：每个候选删之前必须用冻结的尺子（`scripts/parity/`）证明删了不掉分。** 本清单只登记与建议，不动代码；后续 issue 由人批另开。

## 来历查证（适用于全部条目）

- `src/PROVENANCE.md`：只登记搬入来源与适配例外，**没有**任何关于下述防御机制的交代。
- 旧迁移包 ADR（`~/ws/PilotDeck-Rust/docs/rust-migration-package/` 的 `decisions.md`、`architecture.md`、`execution.md`、`tasks.yaml` 及 `inventory/*.csv`）：grep `防御/defensive/dateparser/V8/clock/JsValue/语义漂移` 等关键词**零命中**。唯一相关的是 `decisions.md` 里「TypeScript 旧实现是冻结 oracle」的总方针——逐字节对齐 TS 是全部这些代码的动机，但每个机制本身无单独决策记录。
- 旧执行现场（`~/ws/PilotDeck-rust-program-20261009/build/`，只读）有 `ARCH01-codec-a1/`（含 `codec-before-negative.rs`、`ryu-js` 依赖）——codec 的 JS 语义模拟是旧执行的**专项任务、带负例对照**的产物；其余候选无对应任务目录痕迹。
- 大部分机制的动机写在**代码注释**里（如 system_clock.rs 文件头的 V8 版权与说明、tool_context_policy.rs 的 "no source RangeError was invented"、codec.rs 的 "verifies JSON grammar without constructing serde Value"），属于自我交代，无 ADR 背书。

---

## 1. `pilotdeck-transport/src/system_clock.rs`（815 行 · 比值 45.3 · 对不上 · 多出约 797 行）

- **在做什么**：生产 UTC 时钟 + Node v22 `Date.parse` 的完整移植：手写 V8 dateparser（ISO/宽松格式、AM/PM、命名时区、Smi 上限，文件头带 V8 版权声明）+ 系统 TZif 读取与 ICU FORMER 偏移策略 + `toISOString` 的 ±扩展年格式。
- **TS 为什么没有**：TS 三处用到时间（`agentLoopSidecarServer.ts:697-702` 的 earliestDeadline 用 `Date.parse`、updatedAt 用 `new Date().toISOString()`）全走引擎内建，共约 18 行。
- **删掉的风险**：换成 chrono/手写简化解析后，日期字符串解析语义（宽松格式、本地时区、闰秒/重复时刻）与 V8 不一致 → executionDeadline 判定与 trace 时间戳漂移 → 掉分。
- **建议**：**精简候选**。先取证：宿主实际传给 `Date.parse` 的输入若全部是自家 `toISOString()` 产出的 ISO 格式（大概率），V8 宽松解析器的大部分分支永远走不到，可裁到「ISO 8601 + 显式偏移」子集并用负例对照锁住；TZif 解析与 `now_iso` 保留。

## 2. `pilotdeck-transport/src/stdio.rs`（838 行 · 比值 6.1 · 部分 · 多出约 700 行）

- **在做什么**：Module2.0 NDJSON 连接宿主。骨架（发 module_call、按 inReplyTo 关联响应、写行）与 TS 对应；独有的是：`DispatchPhase` 五态状态机、**精确字节**请求缓存（永不重构重发）、响应身份相关性校验（requestId/runId/operationId/binding）、`owned_containment_cleanup`/`owned_abort_capture`（model.close_stream 须持有同 preparation 的 stream_next、context.capture_turn 仅一次的有界补救）、有界队列与和解。
- **TS 为什么没有**：TS `agentLoopSidecarServer.ts` 是无界 promise 链 + inReplyTo 匹配即 resolve，abort 即删 pending——没有确定性防线，也不需要（单线程事件循环天然串行）。
- **删掉的风险**：这些防线让 Rust 在重复响应/abort 后回包/乱序等异常场景下「比 TS 更严格」。删掉=向 TS 的宽容形状对齐——**正是 sidecar 对 sidecar 对拍想要的**；53 场景未必覆盖这些异常，删了大概率不掉分，但需尺子证明。串行性与丢失唤醒防护（WakeSignal 配合）是线程模型必需，不能全删。
- **建议**：**分条处置**：响应身份校验保留（便宜、防串线）；DispatchPhase+字节缓存+containment/abort_capture 列**删除候选**（各开一条 issue、逐条用尺子验证）。

## 3. `pilotdeck-agent/src/model_event_reducer.rs`（725 行 · 比值 3.4 · 部分 · 多出约 511 行）

- **在做什么**：模型流事件归并（typed reducer）+ `same_value`/`matches_fields` 递归校验层（typed 事件与 boundary 矛盾即拒绝）+ typed→boundary 反向投影约 250 行 + 每个事件写 typed 与 boundary 双份状态。
- **TS 为什么没有**：TS `assembleModelMessage.ts`（214 行）只有一份 reducer，直接 switch typed 事件、不做校验。
- **删掉的风险**：产品路径（`turn.rs`）实际走 `source_invocation` 的 source reducer（`source_model_reducer.rs`），typed reducer 是与之平行的**双轨验证**实现；删校验层不影响产品行为，但失去「typed 与原始对象矛盾即拒绝」的自检。附带：`driver.rs` 的 `run_model_invocation`（约 200 行）是同一双轨的另一端（未接线），应同批评估。
- **建议**：**删除候选**（连同 `driver.rs` 的未接线路径）。先确认无产品接线、相关测试随迁裁剪，删除后跑全量对拍。
- ⚠️ **协调者核对（2026-10-14 04:30）**：「未接线」只对 typed reducer 的主体成立——它导出的 `same_value` / `matches_fields` / `object` / `optional_string` 被 `tool_projection.rs:3`、`large_file_repair.rs:2` 引用，`lib.rs:293, 296` 也有 `model_event_reducer::new` / `block_id` 的调用点（是否在产品路径待核）。**删之前先把这几个辅助函数迁出**，否则编译即失败。

## 4. `pilotdeck-agent/src/tool_projection.rs`（614 行 · 比值 3.7 · 部分 · 多出约 446 行）

- **在做什么**：工具结果 FIFO 配对（对应 TS）+ 投影到规范化消息（对应 TS）+ `validate_result`/`validate_content`/`validate_array`/`validate_supplemental` 边界校验层（投影前校验每条边界与规范化视图一致）+ `ProjectionGap` 降级（媒体字段 lax、未知 kind 保 BoundaryOnly 而非失败），合计约 250 行。
- **TS 为什么没有**：TS `projectToolResults` 直接信任 `PilotDeckToolResult`，类型即契约。
- **删掉的风险**：宿主给宽松形状时，Rust 不再降级保底而是按 TS 方式直接消费——与 TS 行为一致化，掉分风险低；但若 53 场景里有场景依赖降级路径（形状本就宽松），删校验会暴露为失败。
- **建议**：**精简候选**：校验层收缩为 `debug_assert` 或删除，`ProjectionGap` 降级保留（它是「不发明数据」的保底，与 TS 的实际行为差异需先在 accepted-diffs 里登记）。

## 5. `pilotdeck-protocol/src/codec.rs`（398 行 · 比值 ∞ · 对不上 · 多出约 398 行）

- **在做什么**：手写迭代栈 JSON 解析（先用 serde_json RawValue 扫一遍语法再跑自写解析器，双保险）+ JS 语义 stringify（ryu-js 数字格式、数组下标键升序、undefined 省略/顶层返 None）+ 迭代深拷。
- **TS 为什么没有**：TS 是引擎原生 `JSON.parse(line)` / `JSON.stringify`（一行）。
- **删掉的风险**：**不可整体删**。stringify 的键序（整数键升序+插入序）与数字格式（ryu-js）必须与 JS 逐字节一致，serde_json 两者都不保证——换掉直接造成 JSON 字节级漂移、大面积掉分。parse 侧的双保险（RawValue 预扫）是可精简的冗余。
- **建议**：**保留**（parse 侧可小幅精简为单遍）。旧执行现场 `ARCH01-codec-a1/` 有专项任务与负例对照，是这套模拟的出处。

## 6. `pilotdeck-agent/src/tool_context_policy.rs`（366 行 · 比值 7.0 · 部分 · 多出约 314 行）

- **在做什么**：applyToolResults 的重挂尾决策（核心约 40% 对应 TS）+ 值域防御（约 60%）：非数组 messages/appendedMessages 的完整处理（字符串按代理对拆、ToPrimitive/radix 数值比较）、≥1024 深度护栏（`TOOL_CONTEXT_NATIVE_COERCION_DEPTH_PROFILE`，注释自述「不发明原生 RangeError」）、host 回复标签校验。
- **TS 为什么没有**：TS 用 `CanonicalMessage[]` 类型排除这些值域，宿主真回字符串时 JS 自己抛 TypeError；Rust 的 BoundarySnapshot 承载宿主任意返回值，只能显式模拟。
- **删掉的风险**：删掉后宿主回非常规形状时 Rust 走 `InvalidResponse` 而非模拟的 JS TypeError——错误路径与 TS 不再逐字一致；正常路径不受影响。
- **建议**：**删除候选**（`primitive`/`string_items`/`radix` 整段）；保留 host 回复标签校验。需尺子验证异常路径文案不进 trace 比较。

## 7. `pilotdeck-protocol/src/lib.rs`（236 行 · 比值 118 · 对不上 · 多出约 234 行）

- **在做什么**：JS 值语义模拟层：`JsString`（WTF-16、未配对代理）、`JsObject`（插入序 + O(1) 索引双结构）、`JsValue`（含 Undefined）、手写结构相等、迭代 Drop（防深对象图递归 drop 爆栈）；仅两个协议版本常量有 TS 对应。
- **TS 为什么没有**：这些是 JS 引擎原生能力。
- **删掉的风险**：删=重写整个移植——BoundarySnapshot、所有 policy 文件的 JS 语义展开都建在这层上。
- **建议**：**保留**。它不是「膨胀」而是语言适配地基，登记在此只为让「对不上」档有交代；唯一可评估的是迭代 Drop 等防御在输入受控后是否仍必要（不建议现在动）。

## 8. `pilotdeck-protocol/src/consumer.rs`（170 行 · 比值 3.6 · 部分 · 多出约 123 行）

- **在做什么**：binding/capabilities 解析校验（对应 TS）+ `wire_string`（约 31%）：把 TS 一行模板字面量 `${message.protocolVersion ?? "missing"}` 的 JS ToString 强转（Array#join、[object Object]、NaN/Infinity、自有 toString 即抛 TypeError）整个重实现。
- **TS 为什么没有**：一行字面量，引擎求值。
- **删掉的风险**：错误消息会进 trace 与比较器视野，格式漂移可能被判差异；52 行换更简单的 `to_utf8` 拼接可能在边缘输入（NaN、对象）上产出不同文案。
- **建议**：**保留**（便宜且守住错误文案逐字一致）。

## 9. `pilotdeck-tokenizer/src/implementation.rs`（84 行 · 比值 9.3 · 部分 · 多出约 75 行）

- **在做什么**：`count_tokens` 懒初始化 + encode 计数（对应 TS 的 getTokenizer 单例）；独有：内嵌 o200k 词表的 base64 解析 + 完整性三重断言（199998 条、rank 全覆盖、256 单字节）、JS 空白字符类改写（FEFF 属空白、U+0085 不属）、USVString 转换、`catch_unwind` 包 bpe.encode。
- **TS 为什么没有**：TS 把这些交给 js-tiktoken 依赖与引擎。
- **删掉的风险**：token 计数漂移 → 预算估算漂移 → 掉分；完整性断言与 catch_unwind 很便宜，删了省不了几行。
- **建议**：**保留**。与 `src/PROVENANCE.md`「后续精简候选」里登记的 **o200k 词表重复嵌入**（implementation.rs 的 RANKS 与 vendor 的 `o200k_base()` 各嵌一份约 3.6 MB）合并处理——那是二进制体积问题，不是行数问题。

## 10. `pilotdeck-transport/src/execute.rs`（62 行 · 比值 3.1 · 部分 · 多出约 42 行）

- **在做什么**：FactoryPayload 结构 + `FactoryPayloadDecoder` trait / `OriginalFactoryPayloadDecoder` 再导出边界。
- **TS 为什么没有**：这层解码在 TS 由 `createSidecarExecution` 内联完成，无独立模块分层。
- **删掉的风险**：无行为风险，纯结构合并；仅动模块边界。
- **建议**：**精简候选**（trait 层并入 `factory_payload.rs`），优先级最低。

## 11. `pilotdeck-agent/src/request_materialization.rs`（38 行 · 比值 2.9 · 部分 · 多出约 25 行）

- **在做什么**：三个 Input 结构 + `OriginalRequestMaterialization` 纯委托 facade。
- **TS 为什么没有**：三个变换在 TS 内联在 AgentLoop 方法里，无独立文件。
- **删掉的风险**：无；仅损失一个文档化的边界命名。
- **建议**：**精简候选**（内联进 `request_assembly.rs` 或保留），收益最小，可不动。

---

## 汇总

| # | 文件 | 判断 | 比值 | 多出行数 | 建议 |
|---|---|---|---|---|---|
| 1 | transport/system_clock.rs | 对不上 | 45.3 | ≈797 | 精简候选（先取证输入分布） |
| 2 | transport/stdio.rs | 部分 | 6.1 | ≈700 | 分条：校验保留，状态机/缓存/containment 列删除候选 |
| 3 | agent/model_event_reducer.rs | 部分 | 3.4 | ≈511 | 删除候选（连 driver.rs 未接线路径） |
| 4 | agent/tool_projection.rs | 部分 | 3.7 | ≈446 | 精简候选（校验层收缩，降级保留） |
| 5 | protocol/codec.rs | 对不上 | ∞ | ≈398 | 保留（parse 侧可小幅精简） |
| 6 | agent/tool_context_policy.rs | 部分 | 7.0 | ≈314 | 删除候选（值域防御段） |
| 7 | protocol/lib.rs | 对不上 | 118 | ≈234 | 保留（语言适配地基） |
| 8 | protocol/consumer.rs | 部分 | 3.6 | ≈123 | 保留 |
| 9 | tokenizer/implementation.rs | 部分 | 9.3 | ≈75 | 保留（与词表重复嵌入合并处理） |
| 10 | transport/execute.rs | 部分 | 3.1 | ≈42 | 精简候选（优先级最低） |
| 11 | agent/request_materialization.rs | 部分 | 2.9 | ≈25 | 精简候选（可不动） |

**成因分布**：11 项中没有一项是「多写了 TS 没有的产品功能」；多出行数来自四类——①引擎等价物移植（system_clock、codec、lib.rs、consumer.wire_string）；②对拍期确定性/一致性防线（stdio、model_event_reducer、tool_projection、tool_context_policy 的防御段）；③模块边界样板（execute、request_materialization）；④依赖内联展开（implementation.rs）。其中真正值得动的是 ②（约 2000 行）与 ① 的 system_clock（约 800 行），合计约占产品代码的 10%。
