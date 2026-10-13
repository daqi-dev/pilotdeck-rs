# 旧迁移方案（`~/ws/PilotDeck-Rust/docs/rust-migration-package/`）—— 核查结果

> 2026-10-14。自有积累：**可照搬，先判时效**。由子 agent 产出，主会话抽查 3 条（✔）、**更正 1 条**（⚠）。
> 行号相对 `docs/rust-migration-package/`。子 agent 报告未发现凭据。

## 一、怎么切

- **第一刀与这次一致** ✔：P1 `CORE-01`「Rust AgentLoop 正式 sidecar 接入」，范围 `src/agent/loop/`、`src/agent/modules/transport/`，用旧 UI 验证（`tasks.yaml:301-342`；`roadmap.md:5`）。Session → TurnRunner → AgentLoop 的边界不动，sidecar 不带第二份宿主策略（`architecture.md:5`；`contracts/agent.md:3`）。
- 之后的顺序不同：P2 数据 / Session / 工具 → P3 Gateway 与 `ui/server`（`GW-01`，`tasks.yaml:829-850`）→ P4 Rust/Wasm UI 与 Wry/Tao 桌面。
- **范围大得多**：全部第一方逻辑，含 UI、桌面、SDK、CLI、macOS 安装包（`README.md:5`；`definition-of-done.md:6`）。
- 钉的原版：`b243fa0`，ref 同为 `standalone_alpha1`（`plan.yaml:7-11`）。

### ⚠ 时效更正

子 agent 判断「inventory 绑在 b243fa0 上，对 cc7c4e6 已过时」——**不成立**。实测（`resources/PilotDeck`）：
- `b243fa0` → `cc7c4e6` 只有 **2 个提交**（`4cc6c016` StaffDeck 每项目绑定独立 agent、`cc7c4e65` 合并）。
- `src/` + `tools/` 只改 **8 个文件，+372/−24**，全在 StaffDeck 绑定（`src/pilot/staffdeck/projectBinding.ts` +306 等），**不碰 agent 循环**。
- 全仓 +18.4 万行里 18.0 万行是 `docs/testing/evidence/staffdeck-binding-2026-10-13/` 的测试证据。

⇒ 旧 inventory **基本仍适用**，按这 2 个提交做增量修正即可。

## 二、怎么定义「一致」与「完成」

- **有对拍**：固定原版 53 个场景，必须走正式 factory / handshake / binding / module_call，**连续两轮通过**（`gates.yaml:22-27` G03；`contracts/parity.md:5`）；只允许事先冻结、不影响语义的归一化（`parity.md:7`）。
- **比较分三层** ✔（`execution.md:47-53`）：业务期望（scenarios.json oracle，不变）；端口通道值严格比（`model.request` 完整 modelView、tool / permission / usage / 终态）；公共事件流**各自局部顺序**比，跨通道只断言能从源码证明的先后。
- **比较器必须先被证伪** ✔（`execution.md:53`）：正向（原版自比、合法交错）、负向（每条规则各自的控制）。**原版自己跑两遍（r1 vs r2）是任何比较规则的否定控制**——规则若让原版自比失败，就是规则错了（`:50`）。
- **失败先分诊再动手**（`:52`）：来源分 Rust 产品 / 原版缺陷 / 比较器规则 / fixture 或环境。
- 同一缺陷最多 3 次有意义尝试，之后升级方法或模型并记录（`:49`）。
- 已接受差异：**没有正式清单**，只有分散原则（原版缺陷单列 `contracts/reuse.md:3`；有意差异先写 ADR `execution.md:33`；需用户认可 `definition-of-done.md:7`）。
- 请求体只写到「tokenizer 与请求 fixtures」（`contracts/agent.md:15`），**没有请求体的归一化规则**。

## 三、怎么组织 AI 执行

- 43 个任务、一个子系统一个任务（如 `CORE-01` 包下整个 loop + transport）——**粒度很粗**。
- 1 协调 + 2 作者 + 1 审查验收（`agents/strategy.md:5-12`）；唯一队列 + claim / handoff / receipt（`governance.md:11`）；同一失败最多重试两次、不许改 oracle（`execution.md:17`）。
- **仍然漂移了**：A1 的「无缺陷」结论被撤回（`../../rust/STATUS.md:18`）；协调者换了 4 轮（`STATUS.md:20-28`）；把无关 PID 记成了自己（`agents/strategy.md:33`）；observer 比较器迭代到 v2.4 仍只算诊断；**但尺子本身（G03 v1）10-11 已被采纳**，见 `findings-execution.md` 二·1 的更正。

## 四、inventory

3211 个 tracked 文件按 blob 记录（`inventory/README.md:3`）；93 个能力族、911 个接口面（`http_route` 257、`sdk_method` 119、`gateway_rpc` 101 …，`feature-surface-manifest.json`）。全是静态清单，行为都标 unverified（`README.md:29`）。

## 五、`rust/` 里写了什么

只有 4 个协议 crate，共 4182 行：`pilotdeck-protocol`（约 2080 行，26 个测试）、`plugin-protocol`（436 行，7 个测试）、`workspace-protocol`（206 行）、`terminal-protocol`（86 行）。**没有 agent crate**——STATUS 记的 `pilotdeck-agent`（460 个测试）在仓外的执行现场目录里（`rust/STATUS.md:9, 26`）。

## 子 agent 的判断（已按时效更正调整）

- **可照搬**：第一刀 = AgentLoop sidecar 且要证明跑的真是 Rust（`tasks.yaml:323-325`）· 原版自比作为比较规则的否定控制（`execution.md:50`）· 比较器先证伪才能用（`:53`）· 失败先分来源再修（`:52`）· **inventory（按 2 个提交增量修正后）**。
- **改造后用**：`parity.md` / `agent.md` 的对拍条款（补「请求体」与归一化规则）· gates 只留 G01–G04 · 「已接受差异」从 ADR 原则落成正式清单。
- **只借教训**：粗粒度任务 + 多轮协调者交接 → 漂移 · 比较器反复假通过 · 「运行中」必须有 PID / 会话证据才算。
- **不适合**：全栈 Rust/Wasm UI 与桌面范围 · 43 任务的 DAG 与 4 agent 编制 · legacy-map 映射。
- **首选照搬**：`execution.md:47-53` 的比较分层、分诊规则与比较器证伪要求。
