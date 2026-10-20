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

## A6.5-pr7：bash 输出超 V8 字符串上限时不随 TS 关停网关（自决 · 待复核，AI 代批，待人复核）
- **卡在哪**：gate3 P1 指出，子进程 stdout/stderr 在运行期间无上限累积，`cat /dev/zero`（只读命令，不需要权限）能把网关撑到 OOM。作者实测 TS 的行为：块超过 MAX_STRING_LENGTH 时抛出未捕获的 RangeError，服务端走 `shutdownAndExit(1)` 关停**整个网关**；放不下的块丢弃，子进程不被杀。
- **选项**：A 对拍到底，Rust 也退出进程；B 逐流封顶、只丢超限块、网关继续运行；C 超限时杀进程组。
- **选择**：B。可撤回，不动尺子。「崩溃 / 进程退出」在分级表里属于 P0 类结果，对拍不复制基线的崩溃；可观察输出（丢块、后续块照常追加、子进程不被杀）仍与 TS 一致。
- **原则**：**基线因未捕获异常导致整个进程退出的路径，Rust 不复刻退出，只对齐退出前可观察的输出；差异登记为已接受。** 断言：`bash_tool` 用真实上限的那条测试。

## A6.5-pr9：web_fetch 的 HTML→markdown 需要 HTML5 解析器（人批 A，2026-10-20）
- **卡在哪**：TS `urlFetcher.ts:134` 动态 import npm `turndown` 7.2.4（规则层 774 行）。turndown 在 Node 上用 `@mixmark-io/domino` 2.2.0 解析 HTML：完整的 WHATWG HTML5 解析器加 DOM，16,732 行，其中 HTMLParser.js 7,254 行。锁文件里没有任何 HTML 解析 crate。
- **谁受影响**：只影响 web_fetch 对 text/html 的转换；URL 校验、抓取、缓存、web_search 不受影响。
- **选项**：A 新增 html5ever（Servo 的 WHATWG 解析器，新外部依赖），turndown 规则层自写移植，用 TS 金样对拍；B 自写 HTML5 解析器，约万行，误差面大；C 本单不做转换，走注入 trait，无注入时按 TS 动态 import 失败的路径报错，转换另开单。
- **建议**：A。批下来之前按 C 推进。
- **档位**：A 属于新外部依赖，暂不执行，等人批；C 自决。
- **人批（2026-10-20，用户本人在协调者会话）**：选 A——新增 html5ever；turndown 规则层自写移植，TS 金样逐条对拍。依赖单独在 A6.5-pr9b 引入（版本 `=` 精确钉、只开所需 feature、源码构建、不用预编译二进制），PR 描述单列锁文件增量。pr9 本单按 C 先交付（转换走注入边界）。

## CLAUDE.md 精简（自决 · 待复核，AI 代批，待人复核）
- **卡在哪**：CLAUDE.md 171 行 / 26.7 KB，混有事故经过、课程出处、已失效期限，且 §六约定与页脚夹在 §五中间；aias-meta-init 标准为「只留系统一生不变、AI 不知道就会做错的祈使句」（`.claude/skills/aias-meta-init/SKILL.md:217-219`，模板 `templates/CLAUDE.md.tpl` 63 行）。
- **谁受影响**：所有协调者会话；引用 CLAUDE.md 小节名的脚本与规约（`scripts/pdrs-loop.sh`「连续推进模式」「技能停点覆盖表」「资源上限」，`scripts/cargo-slots.sh`，`specs/*/FROZEN.md`「§五」，`.claude/workflows/review-*.md`「问题分级」「P2 折中」，memory「当前协作与持续推进」）——这些标题与编号（铁律 12/14、§五、①–④）全部保留。
- **能否撤回**：可撤回（`git show 4fd3ee5:CLAUDE.md` 即原文）。不动尺子 / 意向。
- **做法**：人定规则原意保留在新 CLAUDE.md（压缩措辞、不降强度）；事故经过迁 5 条 memory；仓库/队列/看板配置细节迁 `docs/changes/ops/002`；以下理由与出处只删不迁（均可在 Git 原文与所列决策文件找到）：
  - 连续推进依据「2026-10-13~14 单会话 40.9 小时 + 84 子代理」（原第 19 行）；
  - 资源上限理由：127 GB 旧迁移教训、32 GB 内存瓶颈、46 GiB 单位消歧、10-18 放宽的盘面数字（原第 70–72 行；正本 `state/decisions-20261017.md:107-112`、`state/decisions-20261016-followup.md:42` R52、`docs/research/003_prior-attempt/`）；
  - 问题分级的课程原口径与 10-14「P2 不挡」偏离史（原第 118 行；正本 `state/decisions-20261014.md:223-227`）；
  - 大步骤自查出处引文（原第 127 行）；决策分级课程出处与「5 张卡全部一致」依据（原第 134–136、145–146 行；正本 `state/decisions-20261014.md:99-116`）；
  - 技能停点表的「42plugin 按人在场一次一单设计」说明与「执行器」列中的说明性内容（原第 151–153 行；执行器脚本名保留在行内）。
- **明确已失效、删除**：原第 88 行「2026-10-14 夜跑的 08:30 截止及首 Issue 专属期限不重复生效」——原文自述已不生效，历史见当日晨报与 Git。
- **原则**：CLAUDE.md 只放每轮要执行的祈使句；为什么 → memory / decisions，怎么配的 → docs/changes/ops，标准 → specs。
- **断言**：被外部引用的小节名在新文件中逐个 `grep` 命中（见 `_tmp/20261020-claude-md-slim/mapping.md` 末节）。
