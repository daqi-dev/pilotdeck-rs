# 2026-10-20 决策卡

## A6.4-pr5 起：真实模型 provider 需新增 TLS 依赖（人批 A，2026-10-20）
- **卡在哪**：A6.4-pr5（Provider HTTP 公共层 + anthropic）及其后 openai / responses / google / fallback（pr6~pr9）、A6.5-pr9 web_fetch/web_search、A6.6-pr8 全量 compaction（需辅助模型）都要发 HTTPS 请求；`src/Cargo.lock` 无 rustls / native-tls / hyper-rustls（盘点 `_tmp/20261018-a6-split/inventory.md` §0.3）。
- **谁受影响**：上述 7 单；其余 A6.5 单（rewind、bash、MCP、后台任务）不受影响。
- **能否撤回**：加依赖可撤回，但引入外部 crate 属供应链面变更，现有约定为「不加新 crate 或锁文件变更」。
- **动不动尺子 / 意向**：不动尺子；涉及依赖策略。
- **选项**：A 引入 rustls + hyper-rustls（纯 Rust，无系统 OpenSSL）；B native-tls（系统 Security.framework / OpenSSL）；C 先只做明文 HTTP + mock 的公共层，TLS 连接器留接口，待批后补一单。
- **建议**：A（与 hyper 现有栈一致，跨平台无系统依赖）；批前可按 C 先推进公共层。
- **档位**：暂不执行（新外部依赖）。C 也先不做，等人一并定，避免公共层接口按错误假设定型。
- **原则**：新增外部 crate 一律单独成卡、单独 PR。
- **人批（2026-10-20，用户本人在协调者会话）**：选 A——rustls + hyper-rustls。依赖单独在 A6.4-pr5 引入（不另拆纯依赖 PR，但锁文件变更在 PR 描述单列），只开所需 feature，源码构建，不用预编译二进制。

## A6.4-pr5：anthropic 流的 jsonrepair 缺口（自决 · 待复核，AI 代批，待人复核）
- **卡在哪**：TS `normalizeAnthropicStreamEvent` 对工具参数 JSON 解析失败时先调 npm `jsonrepair` 修复，修不好才延迟失败；pr5 未移植，DeepSeek 评审判 P1（行为与 TS 矛盾）。
- **能否撤回**：可撤回（独立模块）。不动尺子 / 意向。
- **选项**：A 新增 Rust crate（新外部依赖，需人批）；B 按基线 jsonrepair 算法自写移植，真实 TS 金样对拍；C 缩范围、标未移植（行为差异仍在）。
- **选择**：B——不扩大依赖面，且可用真实 TS 逐条对拍；超过 3 小时则拆单回报。
- **原则**：基线用到的小型纯算法库优先自写移植并用 TS 金样对拍，不引新 crate。

## A6.7-pr4：hooks matcher 消费型嵌套量词回溯不设预算（自决 · 待复核，AI 代批，待人复核）
- **卡在哪**：#277-3。`/^(a+)+z/` 一类消费型嵌套量词是指数回溯，不是栈溢出，卡 B（decisions-20261017.md:121，V8 资源耗尽边界）管不到。
- **实测**（#351 修复 ac7cbfe，`_tmp/20261019-a67-pr4/p2-prereq-result.md`）：Node `/^(a*)*z/` 20/24/26 个 a 用 48/137/552 ms；Rust release `/^(a+)+z/` 18/20/22 个 a 用 49 ms/213 ms/1.04 s——同阶无界。
- **选项**：A 不设预算，与 TS 一样迟到但结果相同；B 设步数预算，超了返回 false——会把 TS「迟到的 true」改成「提前的 false」，是行为差异。
- **选择**：A。可撤回，不动尺子；病态正则来自用户自己的 hooks 配置，不是外部输入。
- **原则**：**对拍优先于自我保护——TS 本身无界的计算，Rust 不另加改变结果的预算；只在 TS 有确定失败（栈溢出等）时按卡 B 对齐。** 断言：`tests/fixtures/hooks-config/README.md` 资源表一行。
