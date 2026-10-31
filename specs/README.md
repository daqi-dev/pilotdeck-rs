# specs — 规约：产出该长什么样

> 与开工手册同属「我立的规矩」：**手册管怎么干，规约管产出该长什么样。**
> 多品牌 / 多产品线时，这里一线一份，与作品区一一对应。

## 子目录

| 目录 | 装什么 |
|---|---|
| [`style/`](style/) | **风格档案**——正例、负例、用词表、定标样本 |

**风格不住在真相源里，住在这里。** `vault/` 管「事实是什么」，规约管「产出该长什么样」——
真相源解决不了「应该是什么」。详见 `style/README.md`。

> **形容词自述不可靠，可靠的是你亲手改掉的每一处。风格不是宣言，是改稿史的压缩。**

## 命名：三种命名法

名字里带什么，取决于你日后**怎么找它**——有的靠序号找，有的靠时间找，有的根本不用找、它自己会来。

| 命名法 | 用于 | 样子 | 为什么 |
|---|---|---|---|
| **流水编号** | 档案（调研 · 计划 · 评审） | `001_kebab-case.md` | 要供人引用，**编号一变引用就断**，所以永不重排 |
| **创建日期** | 过程材料 | `2026-07-16_界面审计.md` | 你只关心它什么时候产生的 |
| **最后更新日期** | 记忆 | `20260728-禁用词.md` | 新的覆盖旧的，更新即改名并同步索引 |

> 记忆和档案的差别只有一条：**记忆是下次自动生效，档案是你去查才生效。**
> 命名方式不同，正因为读取方式不同。

规约类文件（被路径引用的那些）**不编号、不带日期**——稳定性是命根，版本走提交链。

### 三个忽略区的目录名

`_build/` `_tmp/` `_archive/` 不入库，但**目录名有硬格式**——它们靠时间排序，格式一乱就再也理不清：

| 目录 | 只许 | 为什么 |
|---|---|---|
| `_build/` | `YYYYMMDD-HHMMSS/` | 一次构建一个；按天分会撞车（一天出多版就互相覆盖） |
| `_tmp/` `_archive/` | `YYYYMMDD-slug/` | 最后更新日期制 |

三区共同规则：**日期一律打头**（方能一路排序、一眼看出时间）；**均禁裸文件与目录混住**（找起来累，管起来更累）。

### worktree 与分支（2026-10-17 登记现行实践）

| 对象 | 格式 | 例 |
|---|---|---|
| worktree | `_build/wt-<issue号或阶段码>[-<短名>]/` | `_build/wt-19/`、`_build/wt-a53/` |
| 一次性构建目录 | `_build/YYYYMMDD-HHMMSS[-<用途>]/` | `_build/20261016-093057/`、`_build/20261016-112000-order/` |
| 分支 | `<owner>/<issue或阶段码>-<内容>` | `codex/issue-19-terminal`、`codex/a53-session-domain`、`fix/a53-repetition-guard` |

- owner ∈ **codex**（worker 实现）/ **fix**（评审修复轮）/ **docs** / **dev** / **chore**；名字**全小写**，`<issue或阶段码>-` 前缀为推荐项（机器只强制 owner 白名单）。
- **一个 worktree 恰好一个分支**；PR 描述写三角对应：worktree ↔ 分支 ↔ issue/阶段。
- 机器检查：`python3 scripts/check-naming.py`（**协调者/worker 开工自查**；忽略区不入库故 CI 查不到本地脏）。存量违规冻结于 `scripts/check-naming.baseline.txt`，**只减不增**——清一项删一行，新增即 FAIL。

## 格式

### 提交信息

前缀 `feat` / `fix` / `docs` / `chore`，后接一句话说清改了什么。

**提交尾注**（2026-10-31 人定，#1156 选 2A；与 CLAUDE.md「署名如实写」一致，以 CLAUDE.md 为准）：最后一段只写实际实现方的 `Co-Authored-By`，例如

```
Co-Authored-By: Codex <noreply@openai.com>
Co-Authored-By: DeepSeek <noreply@deepseek.com>
Co-Authored-By: GLM-5.3 <noreply@zhipu.com>
Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>   # Claude 真改过代码才加
```

原样提交 worker 代码时只写 worker；协调者改过才加 Claude。pr-gate 的作者谱系依赖这一行。原先的五行标注（Change-By / Agent / Model / Suggested-By / Co-authored-by）不再要求，旧提交不改。

**作者亲手改的那些提交，就是日后喂给 AI 学你风格的唯一真相源**——标注是为了日后分得出哪些提交是人亲手改的。

### 产出格式

（本系统产出的格式约定：篇幅、结构、必备字段。踩到一次不一致再补一条。）

### board READY 队列块（2026-10-17 登记）

`state/board.md` 里**唯一可原地改写的区域**，放在标题之后、第一个历史段（行首 `> ## `）之前；历史段照旧只追加不改。READY 块是「现在排什么」的唯一正本，各轮段不再写「下一棒」散文，只写「READY：移出 X / 加入 Y」一句留痕。读取与校验：`scripts/next-task.sh`（`--check` 校验，默认输出队首；kind→通道路由表也只在该脚本里）。

```
<!-- READY:BEGIN -->
updated: YYYY-MM-DD HH:MM · by <谁>
- <ref> | <kind> | <state> | <一句话，可带任务书路径>
<!-- READY:END -->
```

| 字段 | 取值 |
|---|---|
| ref | issue 号（`#167`）或阶段码（`A6.1-pr1`）；无空格 |
| kind | `impl` 实现 · `gate` 开 PR 前角色审计 · `review` 双谱系评审 · `research` 调研 · `docs` · `coord` 协调 |
| state | `ready` 可取 · `doing` 已派出（summary 写持有人/worktree/任务书，不再派）· `blocked` 有前置（summary 写前置）· `human` 待人拍板（summary 写决策卡路径） |

- 自上而下即优先级；队首 = 第一条 `ready`。summary 内不得出现 ` | `。
- 每次改块必改 `updated` 行——接续者靠它判断上一轮有没有推进。
