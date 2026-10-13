# 004 · 阶段 C S7：Rust ↔ TS 模块对应检查

> 2026-10-14 · Issue #3 S7（人 2026-10-14 要求加入）。对应表见 `mapping.md`，膨胀候选见 `bloat-candidates.md`。

## 结论

四个 crate 的 **59 个产品 `.rs` 文件**（`#[cfg(test)]` 之前，共 **26884 行**）全部对应到了 TS 源头，映射区间合计约 **16085 行**，总体比值 **1.67**——落在架构决策（`docs/changes/arch/001`「体量风险」）预告的 1.5–2 倍区间内。三档分布：**对得上 46、部分 10、对不上 3**。

关键发现是「膨胀」的构成：**没有发现任何一处「Rust 多写了 TS 没有的产品功能」**。多出的约 1.1 万行分四类——

1. **JS 语义/引擎等价物模拟**（最大头）：`JSON.parse/stringify` 的手写实现（codec.rs）、`JsString/JsObject/JsValue` 值语义层（protocol/lib.rs）、Node v22 `Date.parse` 的 V8 dateparser 全量移植（system_clock.rs），以及各 `*_policy.rs` 里 toFixed/join/ToPrimitive/正则的手工展开。TS 侧这些是语言与引擎原生，对应行数为 0 或个位数。
2. **对拍期确定性防线**：stdio 的 DispatchPhase 状态机与精确字节缓存、typed reducer 的双轨校验（model_event_reducer.rs）、投影前边界校验（tool_projection.rs、tool_context_policy.rs）、宿主回包身份相关性校验。TS 靠单线程事件循环与类型系统不需要这些。
3. **模块边界样板**：`X.rs` + `X_policy.rs` 拆分、trait 解码层、纯委托 facade（execute.rs、request_materialization.rs）。
4. **依赖内联展开**：tokenizer 把 `new Tiktoken(o200k_base)` 一行展开成词表解析+校验（84 行）。

来历查证：`src/PROVENANCE.md` 与旧迁移包 ADR（decisions/architecture/execution、inventory）**均无**这些机制的单独决策记录；唯一总方针是「TS 旧实现是冻结 oracle」，唯一直接痕迹是旧执行现场 `build/ARCH01-codec-a1/`（codec 专项任务，带负例对照）。动机大多写在代码注释里。真正值得后续精简的是第 2 类（约 2000 行）与 system_clock 的宽松解析器（约 800 行），合计约占产品代码 10%；**删前必须用尺子证明不掉分**（计划 S7 规矩），候选清单已逐项给出建议，后续 issue 由人批另开。

## 汇总

| crate | 产品文件数 | Rust 产品行数 | 映射 TS 行数（区间和） | 比值 |
|---|---|---|---|---|
| pilotdeck-agent | 38 | 16302 | 10533 | 1.55 |
| pilotdeck-transport | 11 | 8465 | 4628 | 1.83 |
| pilotdeck-protocol | 8 | 2015 | 913 | 2.21 |
| pilotdeck-tokenizer | 2 | 102 | 11 | 9.3 |
| **合计** | **59** | **26884** | **16085** | **1.67** |

- 三档：对得上 **46**（78%）· 部分 **10**（17%）· 对不上 **3**（5%）。
- 两个口径互相印证：映射区间和 16085（跨 Rust 文件共享区间重复计入）；被引用的 50 个 TS 文件 + Module Protocol schema 整文件合计 16209。`src/agent/` 全树 24315 行中未映射部分（scope/、runtime/ 装配、宿主侧未引用区间）不属于 sidecar 职责。
- 注意：外壳/策略拆分（`X.rs`+`X_policy.rs`）使单文件比值失真（外壳 <1、policy >1），成对看才公平。

**最大的 5 个膨胀候选**（详见 `bloat-candidates.md`，共 11 项）：

| # | 文件 | 多出行数 | 建议 |
|---|---|---|---|
| 1 | transport/system_clock.rs（V8 dateparser 移植） | ≈797 | 精简候选 |
| 2 | transport/stdio.rs（确定性防线） | ≈700 | 分条，部分列删除候选 |
| 3 | agent/model_event_reducer.rs（双轨校验，未接线） | ≈511 | 删除候选 |
| 4 | agent/tool_projection.rs（投影前校验层） | ≈446 | 精简候选 |
| 5 | protocol/codec.rs（手写 JSON 解析/stringify） | ≈398 | 保留 |

## 相关

`docs/plan/001_phase-c-agent-loop-sidecar.md` S7 · `docs/changes/arch/001_phase-c-agent-loop-sidecar.md`（体量风险 R4）· `src/PROVENANCE.md` · `docs/experiments/exp005-first-score/`（S6 首个分数 50/53）
