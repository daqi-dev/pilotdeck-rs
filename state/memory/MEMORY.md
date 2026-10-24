# PilotDeck-RS：把 PilotDeck 后端从 TypeScript 重写为 Rust · 记忆索引

> 记教训，下次自动生效。一件事一个文件：`YYYYMMDD-slug.md`（最后更新日期制，更新即改名并同步本索引）。
> **AI 平时不会把全部记忆加载进来，它加载的只是下面这一行行描述**——描述写准，它才知道该翻哪一条。索引每行：`- [标题](文件名) — 一句话钩子`

单条记忆的写法：抬头 `name`（英文短名）+ `description`（一句话说清管什么、不管什么）+ 可选 `type`；正文两段——**为什么**（这条规矩怎么来的、当初什么教训）与**怎么用**（下次遇到同类情形具体怎么做）。前者防止后人不明就里把规矩废掉，后者让它可执行。


- [Grok Bot 通道](20261019-grokbot-channel.md) — 本机可直连调用（scripts/grokbot-call.py + CURSOR_ACCESS_TOKEN）；未定为评审谱系
- [模型通道正本](20261015-model-runners.md) — 2026-10-22 起优先 glm-5.3-mb + deepseek-v4.1-flash-mb（llm-center）；各入口、unrecognized_model 非失败等已知坑；10-25 起 --runner anthropic 经 llm-center 可用

- [基线源码锚点](20261016-source-anchors.md) — 入口枚举先定位真实源码表达式，锚点失配不靠猜名或放松匹配解决。

- [评审输入边界](20261016-review-bounds.md) — 三角色按精确增量；同一环境核心三次300秒空结果已挂起，不第四改写，事件先后须原始时间证据。

- [证据写路径守卫](20261016-evidence-write-guard.md) — 复用旧审计函数先核锚点次数并限制实际写路径；封存原件只读，恢复须匹配原SHA；命令日志独占创建禁tee覆写，重跑换新文件名。

- [外审执行权限](20261016-review-permissions.md) — no-tools/plan不等于只读；派发前核真实空registry或无工具执行器，越界运行不充验收。

- [测试宿主能力的独立判据](20261016-test-oracles.md) — 权限不可达先用原生探针交叉核对，包装吞EACCES的变异必须失败。

- [资源预算与硬链接](20261016-resource-budget.md) — 清cache按最后硬链接的allocated块估算并完整du复核，保护所有非cache证据。

- [对拍 Rust 二进制路径](20261017-parity-binary-path.md) — 二进制取装置树 src/target；A2 校准在 A1 纯透传源树构建、CARGO_TARGET_DIR 指进装置树，A4 用 HEAD 同树；核 sha256；成片 G1 缺场景先查 run.json error（#167 首采为缺二进制级联）。

- [工作树路径核对](20261016-worktree-paths.md) — 源文件先列真实路径，忽略资源不假定每树都有链接；共享基线按任务卡绝对路径只读。
- [最终审计前先 rebase](20261017-gate-head-rebase.md) — main 保护要求分支最新、pr-gate 结果块绑 head；开 PR 前先 rebase 再跑最终四角色，免得重跑。
- [收敛后的 P2 只修一批](20261017-p2-fix-cycle.md) — 两家无 P0/P1 后剩余 P2 一次修完、只做一轮确认；确认轮新 P2 走跟进，不再开下一轮。
- [DeepSeek 空回复是思考吃光额度](20261018-deepseek-thinking.md) — finish_reason=length 空回复看 reasoning_tokens，评审默认关思考；任务书写明无工具；别把它误判成大输入不可用。
- [shell-text-edits](20261018-shell-text-edits.md) — 中文文本编辑用 python 不用 sed；停进程按 PID 不用宽模式 pkill
- [auto-merge-armed](20261018-auto-merge-armed.md) — 入队失败也可能已开 auto-merge；再推修复前先 --disable-auto，合入后核最后一个提交在 main 里
- [clone-author-email](20261018-clone-author-email.md) — 仓外 clone 不共享主仓的 noreply 邮箱，提交前先设 user.email；空邮箱进 main 后无法改写
- [dsh-channel](20261018-dsh-channel.md) — DeepSeek 首选 dsh：实现 workspace-write、评审 read-only；人定 10-18 起 DeepSeek 担实现与第一家评审大头，codex 守第二家评审与难单
- [quote-review-findings](20261018-quote-review-findings.md) — 修复任务书里的评审结论只能从原文机械摘取，不凭记忆转述
- [glm-concurrency](20261018-glm-concurrency.md) — ZAI key 全局并发≈10、超限立即 429 并被退避拖成超时；四角色闸门同时最多 2 个，GLM 评审另留余量
- [gate-oracle-exclusion](20261018-gate-oracle-exclusion.md) — 闸门 --paths 排除 oracle.json 会招来「oracle 未入库」误报；PR 里预先写明并贴 ls-tree
- [dsh 绕沙箱搬 gitdir](20261020-dsh-gitlocal.md) — dsh/codex 不做 git rebase/commit，协调者在主仓侧做；出现 .git-local 的恢复步骤

- [merge-email-exposure](20261020-merge-email-exposure.md) — 服务端生成的提交用账号主邮箱；公开仓库每次合入后核 %ae %ce，不假定账号已设隐私
- [delete-branch-worktree](20261020-delete-branch-worktree.md) — --delete-branch 会连带删 worktree 与未入库 _tmp/ 证据；合并不加，证据放主仓 _tmp/
- [rebase-conflict-merge](20261020-rebase-conflict-merge.md) — 状态文件冲突可取 main 侧；代码测试逐块真合并，#142 机械取侧丢 8 测试
- [arbitration-runtime-test](20261020-arbitration-runtime-test.md) — 仲裁结论依赖运行时/标准库行为时必须最小实测贴输出（readdir 排序误判）
- [ruler-real-data](20261020-ruler-real-data.md) — 尺子/装置改动用真实数据复算；对照全过不证明真实输入可用（#35、F2）
- [GLM 5 小时额度](20261020-glm-5h-quota.md) — 429/1308「Usage limit reached for 5 hour」会让闸门全 ERROR（模型不符 <synthetic>）；等重置时间过了再重跑，不算闸门结论
- [测试日志归属](20261021-test-log-tree-identity.md) — 跨 worktree 全量测试先用「本分支新测试在、他分支不在」核日志属于哪棵树；通过数相等先 diff 名单
- [macOS exec SIGKILL](20261022-macos-exec-sigkill.md) — 测试二进制 `--list` 也 137 是系统拒启动非测试失败；复制新 inode + ad-hoc 重签取真实结果，不记通过
- [zsh 等号展开](20261022-zsh-equals-echo.md) — 未加引号的 `===` 在 zsh 报 == not found 并截断命令，分隔线用 '---'
- [worker 证据移回](20261022-worker-evidence-pullback.md) — 沙箱 worker 证据落树内 _tmp；移回只用 pull-evidence.sh，RESULT.md 按节追加，永不覆盖
- [只按 pid 停进程](20261022-kill-by-pid.md) — 禁 pkill -n / 宽模式 pkill -f；实验与正式任务输出目录不同名（10-22 两次误杀）
- [DeepSeek 50 RPM](20261022-deepseek-rpm.md) — dsh 默认 ≤8 同跑（dsh-slot.sh），撞 429 由 dsh-retry.sh 自动续跑
- [合并队列只认 rebase](20261022-merge-queue-rebase-only.md) — PR 分支不能有合并提交，否则入队后被静默踢出；rebase 冲突 ours/theirs 与 merge 相反
