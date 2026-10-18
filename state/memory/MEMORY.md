# PilotDeck-RS：把 PilotDeck 后端从 TypeScript 重写为 Rust · 记忆索引

> 记教训，下次自动生效。一件事一个文件：`YYYYMMDD-slug.md`（最后更新日期制，更新即改名并同步本索引）。
> **AI 平时不会把全部记忆加载进来，它加载的只是下面这一行行描述**——描述写准，它才知道该翻哪一条。索引每行：`- [标题](文件名) — 一句话钩子`

单条记忆的写法：抬头 `name`（英文短名）+ `description`（一句话说清管什么、不管什么）+ 可选 `type`；正文两段——**为什么**（这条规矩怎么来的、当初什么教训）与**怎么用**（下次遇到同类情形具体怎么做）。前者防止后人不明就里把规矩废掉，后者让它可执行。

- [本机模型评审入口](20261015-model-runners.md) — 2026-10-15 已核验的 Z.AI GLM 与 DeepSeek 通道（DeepSeek 首选 scripts/review-deepseek.py 直连 API，dsh 兜底）；区分 provider 错误与工具不可用，不改全局默认。

- [基线源码锚点](20261016-source-anchors.md) — 入口枚举先定位真实源码表达式，锚点失配不靠猜名或放松匹配解决。

- [评审输入边界](20261016-review-bounds.md) — 三角色按精确增量；同一环境核心三次300秒空结果已挂起，不第四改写，事件先后须原始时间证据。

- [证据写路径守卫](20261016-evidence-write-guard.md) — 复用旧审计函数先核锚点次数并限制实际写路径；封存原件只读，恢复须匹配原SHA。

- [外审执行权限](20261016-review-permissions.md) — no-tools/plan不等于只读；派发前核真实空registry或无工具执行器，越界运行不充验收。

- [测试宿主能力的独立判据](20261016-test-oracles.md) — 权限不可达先用原生探针交叉核对，包装吞EACCES的变异必须失败。

- [资源预算与硬链接](20261016-resource-budget.md) — 清cache按最后硬链接的allocated块估算并完整du复核，保护所有非cache证据。

- [对拍 Rust 二进制路径](20261017-parity-binary-path.md) — 二进制取装置树 src/target；A2 校准在 A1 纯透传源树构建、CARGO_TARGET_DIR 指进装置树，A4 用 HEAD 同树；核 sha256；成片 G1 缺场景先查 run.json error（#167 首采为缺二进制级联）。

- [工作树路径核对](20261016-worktree-paths.md) — 源文件先列真实路径，忽略资源不假定每树都有链接；共享基线按任务卡绝对路径只读。
- [最终审计前先 rebase](20261017-gate-head-rebase.md) — main 保护要求分支最新、pr-gate 结果块绑 head；开 PR 前先 rebase 再跑最终四角色，免得重跑。
- [收敛后的 P2 只修一批](20261017-p2-fix-cycle.md) — 两家无 P0/P1 后剩余 P2 一次修完、只做一轮确认；确认轮新 P2 走跟进，不再开下一轮。
- [DeepSeek 空回复是思考吃光额度](20261018-deepseek-thinking.md) — finish_reason=length 空回复看 reasoning_tokens，评审默认关思考；任务书写明无工具；别把它误判成大输入不可用。
- [shell-text-edits](20261018-shell-text-edits.md) — 中文文本编辑用 python 不用 sed；停进程按 PID 不用宽模式 pkill
