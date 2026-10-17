---
type: ops
number: "001"
date: 2026-10-17
title: 开发流程技能协议：42plugin 8 技能 × 重构环节（含 #142/#157 复盘）
tags: [skill-protocol, pr-ready, dev-launch, process]
related: []
---

# 开发流程技能协议：42plugin 8 技能 × 重构环节

> **收件人**：opencode 主仓协调 session `ses_eba5a7e79ffeiai4qbibtxH98b`（glm-5.3，即「接续项目评估：检查CLAUDE.md与Claude Code提交」，2026-10-17 00:57 起活跃）。你是协调者、有技能装载能力——本文第一节说的「协调者层」就是你。其他协调者会话同样适用。
>
> 背景：两轮复盘结论——你执行的**双谱系评审（dev-launch §7）到位且超标准**（#142 三轮评审抓到 rebase 丢 8 测试的 P1，好样本）；但 **#142 与 #157 连续两单跳过 pr-ready 三角色**（`notes/pr-ready/` 最新产物停在 10-16 #141 线），且 **`/simplify` 是悬空引用**（技能未装，靠手工模拟四视角）。另：08:50 你发现「#157 尚未合并但 issue 已关——顺序错了」并于 08:54 完成纠偏（合并 #157、#154/#139 已关）——教训有效，说明交付纪律需要显式挂到流程上而非靠当场记起。
>
> 请按第四节反思、按第三/五节组织 #157 收尾与 #46 起步。依据：`.claude/skills/`（42plugin 安装，symlink→`~/.42plugin/cache/42ailab/`）+ `.claude/dev-launch.review.md` + AGENTS.md 合并闸门。

## 一、调用的三个层次（最重要，先读这段）

技能是 **markdown 方法论，不是可执行程序**。它在三种角色手里的用法不同：

1. **协调者层**（有 skill 工具的会话：Claude Code / GLM opencode）：用 skill 装载 SKILL.md → 按 Step 编排——派子代理、写任务书、收留痕、更新 PR 描述。**技能的「调用」只发生在这一层。**
2. **worker 层**（codex 等无 skill 工具的模型）：不能调用技能。协调者把 SKILL.md 相关段落（检查清单、判据、输出格式）**嵌进任务书**，经文件→stdin 喂给它。worker 收到的是「按这个清单做、按这个格式交」，任务书里注明方法出处（如「按 pr-ready Agent 2 的 Rust 系统型清单」）。
3. **规则层**：技能中可脚本化的部分（状态预检、编号规则、manifest 校验）落成 `scripts/` 下脚本，任何 session 都能跑（先例：`scripts/check-manifests.py`）。

## 二、8 技能速查（定位｜执行者｜输出留痕）

| 技能 | 一句话定位 | 谁执行 | 留痕在哪 |
|---|---|---|---|
| dev-launch | 单 issue 全流程入口：校验→调研→分流→细化→实现→三层验证→双谱系评审→交付 | 协调者编排，worker 干活 | `_tmp/issue-N/triage.md`、board |
| dev-req | 高不确定需求细化（实体建模→信息流→元反思），结果回填 issue 评论 | 协调者（opus 级） | issue 评论区 |
| dev-docs | docs/ 三族编号文档（changes/research/plan），追加只增不删 | 协调者 | `docs/NNN_*.md` + 索引 |
| pr-ready | 开 PR 前 SOP：三角色对抗审计 + invariant 表，**不改代码** | 协调者派 3 子代理 | `.claude/notes/pr-ready/<线名>/{sibling,concurrency,boundary,simplify,invariants}.md` + diff.patch，audit 表贴 PR 描述 |
| aias-meta-init | 系统初始化（已用，2026-10-13 生成骨架） | — | 已完成 |
| aias-meta-proto | 原型即定标：标准样本+负例+检测脚本 | 人+协调者（定标时） | `specs/` 三件套（已落地为 specs/parity） |
| aias-meta-research | 调研即取舍：决策点+差距表+取舍卡 | 人+协调者（选型时） | `docs/research/NNN_`（已有 001–008） |
| aias-meta-evolve | 演化体检：信息量/复杂度双线 | 人触发（里程碑时） | 暂无使用痕迹 |

## 三、重构基本环节 × 技能映射（照此组织）

| 环节 | 用什么 | 具体动作 |
|---|---|---|
| 1. 接续开工 | dev-launch §1 | `git fetch && git status && git log origin/main -5`；有在途工作先调查不覆盖（worktree 里未入库的 `_tmp/` 证据是命门，清理前先归档） |
| 2. 调研分流 | dev-launch §2–4 | 读 issue 全文+关联 PR；确定性分流：高确定快走 / 触产品方向停下问人 / >3 天拆 sub-issue。**续接单（如 #154）跳过这步直接进 5** |
| 3. 需求细化 | dev-req | 仅高不确定性时；产出回填 issue 评论 |
| 4. 实现前 | dev-launch §6.1/§9–10 | 协调者按 MEMORY 关键词触发表 cat 相关 memory，把触发的 invariant 写进任务书；「改一处必 grep 同源 sibling」写进任务书 |
| 5. 实现+自证 | dev-launch §6.2–3 | worker 任务书含：规则层命令清单（fmt/clippy/test，原始输出贴 RESULT 文件）+ 判据层走查点；三层绿即存档 checkpoint |
| 6. 开 PR 前 | **pr-ready 全套** | 协调者执行 Step 1–6：三角色子代理写 `notes/pr-ready/<线名>/`（#142 漏的就是这步）；audit 表生成后贴 PR 描述 |
| 7. 简化 | 四视角（见反思②） | 派一家非作者只出建议不改码 |
| 8. 双谱系评审 | dev-launch §7 | 按 `.claude/dev-launch.review.md` 配置（A=codex、B=opencode/GLM、C=DeepSeek：首选 `scripts/review-deepseek.py`、dsh 兜底，降级链 D=Claude 子代理）；提示词文件→stdin 绝不插值；收敛=无 P0/P1（P2 按 AGENTS.md 折中三分） |
| 9. 合并交付 | dev-launch §8 + AGENTS.md 闸门 | 回复说明→rebase 合并→核邮箱→关单，顺序固定；board 每轮追加一段 |
| 10. 文档 | dev-docs | 重大决策/复盘才写编号文档（本文即按此入库）；board 段落不替代 |

## 四、反思四问（含最新实例）

1. **#142、#157 为何连续跳过 pr-ready？**（`notes/pr-ready/` 最后一条是 10-16 #141 线。）若是 15 分钟级小单（如 #154/#157），可声明豁免并在 PR 描述写明理由（范围<15min 无生产改动）；**#46 是 P0 主线，三角色审计表必跑必贴**，无豁免。
2. **`/simplify` 悬空怎么办？** 两个选项：a) 42plugin 补装 simplify 技能；b) 承认现状，把「四视角任务书模板」固化进 `.claude/workflows/`，每次按模板派。选一个落定，别让闸门文字指向不存在的技能。
3. **#157 曾出现「PR 未合并、issue 已关」的顺序倒挂**（08:54 已纠偏完成）——对照 dev-launch §8「先回复说明再 PATCH state=closed」，把交付收尾做成固定 checklist（回复→合并→核邮箱→关单→board 一段），而不是靠当场发现。
4. **worker 无 skill 工具这一事实是否写进了派活习惯？** 派给 codex 的任务书（如 task-codex-154.md）质量很高，但未注明方法出处（按哪个技能哪段清单）。以后任务书加一行方法引用，评审与 worker 口径就对上了。

## 五、下一单的具体建议

- **#157 已收尾**（08:54 合并，#154/#139 已关）。遗留一条 P2a 级留痕建议：当时 PR 未贴「pr-ready 豁免声明」；同类 15 分钟级小单以后在 PR 描述补一句豁免理由即可，不必回补 #157。
- **#46（A4 gateway 透传 + 10 method 本地化，P0 主线，下一单）**：完整走环节 1–10：开工 §1 状态校验；按 §3 评估是否拆 sub-issue（大概率 >3 天，拒绝无人值守整单跑）；实现任务书带 MEMORY 触发表 + 方法出处；**pr-ready 三角色必跑**（`.claude/notes/pr-ready/a4-gateway/`），audit 表贴 PR；双谱系评审沿用现有打法（那是已验证的强项）。

---
*技能版本锚定（以各 SKILL.md frontmatter 实测为准，非 42plugin 安装登记值）：dev-launch 1.0.2 / dev-req 1.0.0 / dev-docs 1.0.1 / pr-ready 1.0.1 / aias-meta-init 6.4.0 / proto 2.1.0 / research 1.1.1 / evolve 2.1.0。原件在 `.claude/skills/*/SKILL.md`。*
