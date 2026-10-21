# 决策记录 · 2026-10-21

## 卡 · pr9 同源遗漏的范围（AI 代批，待人复核）
- **卡在哪**：A6.5-pr9 连续两轮闸门都查出「同源 TS 的其它移植没跟上」。第五轮查的是 MCP `network_fetch`，已在 pr9 内修好；第六轮又查出 provider 路径 `model/http.rs:800`（P1）、hooks exec、count_http / mcp 的 bodyTimeout（P2）。
- **谁受影响**：provider、hooks、MCP 三条网络路径在竞争时序上与 TS 不一致；现阶段没有对拍场景覆盖这类交错。
- **能否撤回**：能。只是拆单，不改任何判定标准。
- **动不动尺子 / 意向**：不动。
- **选项**：A. 继续在 pr9 里修（pr9 已经过 6 个修复轮，范围会一路扩到 provider 与 hooks，评审 diff 更大，下游的 pr9b 也一直等着）；B. pr9 只修自己 diff 范围内的问题，这一类在已合入代码里的遗漏另开产品单（#396），挂在 A6 收尾前，不挡 pr9。
- **建议 / 执行**：B。这条 P1 描述的是 main 上已有的行为，不是 pr9 引入的回归；pr9 里同一判定（web、MCP）已经由两家评审和闸门验证。
- **原则**：闸门的 sibling 角色查出「已合入代码里的同类遗漏」时，若本 PR 没有改动那段行为，就另开单并写明前置节点，不在本 PR 里无限扩大范围；若本 PR 改过那段代码，仍然当轮修。
- **可机检**：暂无，靠评审任务书写明「本 PR diff 范围」来判断。

## 卡 · A6.5-pr10b outputSchema 深度预算的向严差异（AI 代批，待人复核）
- **卡在哪**：SDK 里 Ajv 的栈溢出断点随 schema 形状、调用栈深度、JIT 状态变化（实测约 2300 到 4250 层），Rust 无法精确复刻。深度上限这一处已连续三轮出问题（无上限导致 abort 的 P0 → 只测一种形状的 P1 → 断点随形状下降的 P1）。
- **谁受影响**：MCP 工具的 outputSchema 校验。只有极深嵌套，或很宽的 anyOf / allOf / dependencies 组合 schema 才会触发；正常工具 schema 在预算以内。
- **能否撤回**：能，预算是一个常数加一张权重表。
- **动不动尺子 / 意向**：动 accepted-diffs（冻结件，只追加）。差异方向是向严：端口返回与 SDK 栈溢出完全相同的 -32602 和文案，所以超过 SDK 断点的输入两边结果一致；只有「端口预算 ≤ 深度 < SDK 断点」这一段，端口拒绝而 SDK 通过。按形状列出的区间见 `_build/wt-a65-pr10b/_tmp/20261020-a65-pr10b/RESULT.md:951-972`。
- **选项**：A. 统一保守预算 + 登记向严差异（本轮做法）；B. 继续逼近 V8 断点（已证明做不到稳定，且可能再出假 PASS）；C. 不设上限（会回到 abort 的 P0）。
- **建议 / 执行**：A。代码随 PR #391 合入；accepted-diffs 按冻结程序另开 issue，追加一条「待批」条目，人批前不算已接受。
- **原则**：目标行为取决于运行时的栈深、无法稳定复刻时，宁可向严（拒绝、并给出与基线相同的错误），不向宽（放行）；向严的部分登记成待批差异，绝不算作对拍通过。
- **可机检**：oracle 覆盖各形状在端口预算两侧和 SDK 断点之上的样本；超过 SDK 断点的样本两边必须逐字一致。

## 卡 · #386 子进程信号处置重置的实现边界（暂不执行，待人批）
- **卡在哪**：Node/libuv 在子进程里把继承的信号处置重置为默认、清空信号掩码；Rust std/Tokio 只重置 SIGPIPE。gateway 是 `unsafe_code = "forbid"`，现有依赖（libc、rustix、tokio）没有不需要 unsafe 的子进程信号重置接口。codex 在 wt-a63-pr1 实测 Node 464 例、Rust 174 例后停在选型，未改产品代码（原件 `_tmp/20261021-issue-386/RESULT.md` §2、§5）。
- **谁受影响**：后台任务、bash 工具、command hook、宿主 sidecar、MCP stdio 子进程。只在 gateway 以「信号被忽略或屏蔽」的方式启动时（nohup、部分服务管理器）出现差异；正常前台启动不受影响。
- **能否撤回**：代码能回滚；但新增依赖或 unsafe 边界会改变构建与审查约束。
- **动不动尺子 / 意向**：不动。目标仍是固定 TS 行为。
- **选项**：A. 维持零 unsafe、零新依赖，本单挂起，登记为已知差异；B. 允许一个经核验、对外是安全 API 的依赖，前提是它能跨 macOS/Linux 表达处置与掩码重置，且不改 spawn 的错误与回收行为（具体 crate 尚未核验）；C. 授权一个极小的 unsafe 边界（`pre_exec` 里调 sigaction 和 sigprocmask，或 posix_spawn 属性），单独审查并跨平台实测；D. 用外部中间程序（不建议：引入运行时依赖，还改变进程层级）。
- **建议**：C 最直接、也最贴近 libuv；B 要先找到合适的库；不批时取 A。这几项都涉及 CLAUDE.md 第 14 条的依赖 / unsafe 约束，**暂不执行**，等人批。
- **原则**：子进程信号一致性要同时验证处置和掩码；SIGINT 单个例子变绿不等于完成。
- **可机检**：人批之后，在 SIGINT 被忽略的父进程里回放 `exit-statuses`，结果与 TS 一致；去掉重置后必须失败。

## 卡 · A6.5-pr10b outputSchema 栈溢出模型第五次出问题，按「同一处 3 次不过」挂起（暂不执行，待人定）
- **卡在哪**：SDK 的 Ajv 在 V8 上会因递归深度（修复轮 5–7）或正则回溯（修复轮 8）栈溢出，端口无法逐点复刻。端口改为「向严 + 登记」后，Grok r9（`_tmp/20261021-loop/grok-read-156.txt:3-15`）实测又发现一类漏网：`/u` 下 `.*`、`[^<>]*` 这种原子重复，遇到 BMP 与星外字符混排的数据时，SDK 约在 1260 万单位溢出，端口把它记成权重 0、判 valid（假 PASS，P1）。同时报 P2：阈值 65536 太严，`format: uri` / `byte` 的大输出（data URL、base64）超过 64 KB 就会被端口拒绝。
- **谁受影响**：MCP 工具 outputSchema 校验，只在超长字符串上有区别；PR #391 整体（MCP 运行时、工具桥）跟着挂住，A6.5 收尾也被它挡着。
- **能否撤回**：能。一个常数加一张分类表。
- **动不动尺子 / 意向**：动 accepted-diffs（冻结件，只追加），差异方向是向严。
- **选项**：A. 不再按 pattern 结构细分：只要含无界重复（`*`、`+`、`{n,}`、大上界），权重至少 1，阈值提到 1,048,576（最坏的嵌套捕获上限约 35 万，对它约 210 万的断点留约 6 倍余量；对本轮约 1260 万的断点留约 12 倍）。这样同时修掉 P1 和 P2，也不再追逐逐个形状。B. 维持现有分类，只补这一类再审（已证明会继续漏）。C. 超过某个长度一律拒绝，不看 pattern（最简单，但会误伤不含正则的长字符串，不建议）。D. 把这一类整体登记为向宽差异，暂不处理。
- **建议**：A。规则一次性覆盖「任意无界重复」，不靠枚举形状。按 CLAUDE.md「同一失败 3 次挂起」，本轮不自行派修，等人定后再派 codex 按 A 实现，并用 Grok 的混排样本做回归。
- **原则**：要模拟运行时资源断点时，按「最宽的可疑类 + 足够余量」一次性保守建模，不按形状逐个枚举；能枚举出漏网的模型，就会继续漏。
- **可机检**：oracle 覆盖每个无界重复原子类型（`.`、取反类、含星外字符的类、`\p{}`、分组）在混排与非混排数据下、阈值两侧的样本。

## 人定 · 2026-10-21 13:10 · A6.5-pr10b 正则长串与差异登记
- **原话**：「PR #391 (A6.5-pr10b) 选 A；批准对应正则预算差异及 #4、#5。深度预算暂不批准，请先补清正常工具受影响的范围。超过 1 MB 的输出校验暂不作为本 PR 的硬性验收要求，限制须明确记录。」
- **执行**：codex 修复轮 9 按 A 实现（无界重复一律权重≥1、阈值 1,048,576），补「深度预算对正常工具的影响范围」数据（不改深度常数），在夹具 README 与常量注释写明 1 MB 限制；正则预算差异实现后按实测区间追加进 accepted-diffs，状态记「已批：郑达奇 · 2026-10-21」；#4 #5 状态改已批（PR #404）。深度预算条目保持待批，影响范围数据出来后再报人。
- **原则**：人批的是方向和类别，具体区间以实现后的实测数为准登记；人未批的项不因同一 PR 合入而视为已批。

## 人定 · 2026-10-21 13:15 · #386 允许安全 API 依赖
- **原话**：「#386 继续推进：允许为本项修复引入提供安全 API 的必要依赖，保留项目自身的 unsafe 禁令。nix 可作为候选，先验证能否完整接入现有子进程流程，覆盖信号处置与屏蔽掩码、工作目录和管道、启动失败、取消及进程回收，并在 macOS/Linux 实测。验证通过后可按正常评审流程引入，无须重复请批。若找不到合适方案，再提交局部 unsafe 例外的具体范围与风险供我决定，不自行放宽禁令。」
- **执行**：协调者把 nix 0.31.3 源码拉进 `_build/cargo-home`（安全的 `posix_spawn`、`set_sigdefault`、`set_sigmask`；未见 addchdir）；codex 在 wt-a65-pr2 先做七项可行性验证，再实现。Linux 实测走 CI（本机无 Linux 环境；起容器需要拉预编译镜像，碰红线）。
- **原则**：依赖批准附带条件时，先逐条验证条件，不满足就回到人，不降级成 unsafe。

## 人定 · 2026-10-21 14:20 · #386 选 B（局部 unsafe 例外）
- **原话**：「#386 按照你的推荐，选B」
- **B 的内容**（`_tmp/20261021-issue-386/decision-brief.md`、`unsafe-exception-draft.md`）：只有 `pilotdeck-gateway` 一个 crate 从 `forbid(unsafe_code)` 改为 `deny(unsafe_code)`，只给一个共享函数 `install_child_signal_reset(&mut Command)` 加 `#[allow(unsafe_code)]`；函数内只用 `pre_exec` 注册，并在 fork 之后、exec 之前调用 async-signal-safe 的 `sigemptyset` / `sigaction(SIG_DFL)` / `sigprocmask(SIG_SETMASK, 空集)`；其余 workspace crate 继续 forbid。五处 spawn 在 `.spawn()` 前调用它，cwd、管道、错误上报、kill_on_drop、回收都沿用 std / Tokio。
- **审查条件**：两家非作者专审 unsafe 合法性与 fork 后 async-signal-safety（附实际工具链 std / Tokio 的调用顺序与平台 libc 声明）；信号集合以 Node / libuv 实测为准（第一轮 Node 464 例）；父进程注入 INT 忽略 + TERM 屏蔽后，`background_tasks` 的 exit-statuses 与 TS 一致，并用原生报告器 / Linux `/proc` 断言处置与掩码；去掉调用后两条回归都必须失败；macOS 本机 + Linux CI 都跑。
- **原则**：unsafe 例外按「一个函数、一份清单、一套专审」批；超出清单的任何 unsafe 都要重新请批。

## 人定 · 2026-10-21 14:25 · A6.5-pr10b 深度预算取 2048
- **原话**：「pr10b depth budget 按照你的推荐选用2048。」
- **依据**：修复轮 9 影响范围实测（`_tmp/20261021-a65-pr10b-r9/depth-impact.md`）——1024 会提前拒绝 84 列×3 层查询、54 边目录树；2048 支持到 168 列、106 边，原 12 种窄形状最后支持点 48/48 SDK valid，最坏栈估计约 480 KiB（默认可用约 864 KiB）。
- **执行**：codex 修复轮 10 把 `MAX_VALIDATION_BUDGET` 改为 2048，按新值重算各形状向严区间并补 oracle 两侧样本；协调者据实测区间追加 accepted-diffs 深度条目，状态「已批：郑达奇 · 2026-10-21」。仍会拒绝的残余（如 169 列以上、107 边以上）在 README 与条目中写明。

## 人定 · 2026-10-21 15:25 · A6.5-pr9b HTML→Markdown 深度上限 256
- **原话**：「pr9b 选 A，批准 256 层限制及对应向严差异。若此前已收到同项批准，请沿用并补齐登记，不再重复请批；按既定测试、评审和兼容性跟进安排继续推进。」
- **执行**：此前未就该项请批过，本条为首次人批。accepted-diffs 追加深度条目，状态「已批：郑达奇 · 2026-10-21」；依据 `_tmp/20261021-a65-pr9b/depth-brief.md` 与 RESULT「TS 断点测量方法与原值」。内存放大（#409）与解析器差异（pr9c）照原安排跟进。
- **原则**：同一项人批只请一次；后续登记、评审与兼容性跟进按既定流程推进，不再回头请批。

## 卡 · gateway 差异登记账本放错（AI 代批，待人复核）
- **卡在哪**：仓里有两本已接受差异账本：`specs/parity/accepted-diffs.md`（阶段 C sidecar 对拍，#1–#3 都是 deadline 场景）与 `specs/gateway-parity/accepted-diffs.md`（阶段 A gateway，冻结件，按 `A4-…-001` 分节、每条写类别 / 基线依据 / Rust 行为 / 范围 / 机器判据 / 批准记录，改动须在 FROZEN.md 重登记哈希）。A6.5 的 gateway 差异被写进了前者：#4 handshakeClose、#5 background-task-async-spawn（10-20 会话经 #372 合入，今天由 #404 标为人批），以及协调者今天给 pr10b 写的 #6 正则长串、#7 深度 / 宽度预算（尚未合入）。pr9b 的 gate1 sibling 指出应登在 gateway 账本，协调者核实属实。
- **谁受影响**：对拍判分时读错账本会把已批差异当成未登记（假 FAIL）或把 sidecar 账本混入 gateway 判据。不改变任何已批内容与批准状态。
- **能否撤回**：能，都是只追加的登记动作。
- **动不动尺子 / 意向**：动两个冻结件的登记位置，不改判分语义；人批内容原样迁移。
- **选项**：A. 迁到 gateway 账本——pr10b 未合入的 #6 #7 直接改登到 `specs/gateway-parity/accepted-diffs.md`（A6.5-MCP-OUTSCHEMA-REGEX-001、A6.5-MCP-OUTSCHEMA-DEPTH-001）；已合入的 #4 #5 在 gateway 账本补登（A6.5-MCP-HANDSHAKE-CLOSE-001、A6.5-BG-ASYNC-SPAWN-001，批准记录引 #404），在 sidecar 账本按「只增不删，作废划线写原因」把 #4 #5 划线注明迁移去向；各自在 FROZEN.md 重登记。B. 维持现状，两本都认。
- **建议 / 执行**：A。pr9b 已按 A 登记（A6.5-WEBFETCH-DEPTH-001）；pr10b 在第十轮评审结束后同批改登；#4 #5 的迁移另开小 PR。
- **原则**：差异登在被测对象所属阶段的账本；新登记前先确认账本归属与格式。
- **可机检**：`scripts/gateway-parity/check_frozen.py` 与 `scripts/parity/check_frozen.py` 都 PASS；gateway 账本每节含六项字段。

## 卡 · #386 unsafe 例外改为独立 crate，gateway 恢复 forbid（AI 代批，待人复核）
- **卡在哪**：B 方案把 gateway 从 `forbid(unsafe_code)` 降到了 `deny`，只靠文本扫描守卫（`tests/unsafe_boundary.rs`）把例外限定在一个函数里。三轮评审接连找到绕过：r2 的 expect / 注释、gate1 的大写扩展名 / include! / 指向 crate 外的 #[path]、Grok r4 用 macro_rules 把 `include`、`path` 当参数传（P0×2，已实测）。文本扫描追不完宏展开。
- **谁受影响**：PR #407；gateway 的 unsafe 边界。
- **能否撤回**：能。只是挪代码位置，信号重置行为与人批 B 的范围（一个函数）不变。
- **动不动尺子 / 意向**：不动。比人批更严：gateway 恢复编译器强制的 forbid，例外只留在一个独立小 crate `pilotdeck-child-signals` 里，这个 crate 只放那一个函数。
- **选项**：A. 继续给文本守卫打补丁（fail-closed 拒 `$ident!` 与 `#[$k`），可能还有别的绕过；B. 例外挪进独立 crate，gateway 恢复 forbid，绕过由编译器拦截；C. 用 rustc dep-info 比对实际编译用到的源文件（复杂）。
- **建议 / 执行**：B，派 codex 修复轮 4（`_tmp/20261021-issue-386/fix4.md`）。不加外部依赖，libc 已经在依赖里。
- **原则**：安全边界交给编译器强制；文本扫描只做辅助，不当唯一防线。
- **可机检**：在 gateway 里注入 Grok r4 / gate1 / r2 的绕过写法，cargo build 必须报错；守卫断言新 crate 只有两个文件、`allow(unsafe_code)` 恰好一处。
