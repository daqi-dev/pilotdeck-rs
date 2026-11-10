# skills — 产品默认技能与项目技能规划

## 产品默认技能

本目录的 32 项产品技能来自 [OpenBMB/PilotDeck](https://github.com/OpenBMB/PilotDeck/tree/cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85/skills)，固定提交 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。2026-11-09 用户明确授权仅复用这 32 项旧版技能资产，随源码安装交付。109 个文件的来源与可执行位保留，包括引用材料、辅助脚本、运行依赖声明和测试。其中文件 `audio-transcription/SKILL.md` 与 `browser-use/SKILL.md` 的安装指引已适配 Rust 源码交付；其余 107 个文件保留上游原字节。导入不执行脚本或安装技能的外部依赖。

逐文件交付 Git blob、SHA-256、大小及模式见 [upstream-assets.json](upstream-assets.json)；适配文件的 `upstream` 字段另存上游原字节身份，`adaptation` 明示差异。交付完整性检查不代表 TS 模型输入逐字相同；安装文本差异见 [A6.5-NATIVE-INSTALL-TEXT-001](../specs/gateway-parity/diffs/A6.5-NATIVE-INSTALL-TEXT-001.md)，尚非已接受豁免。上游根许可证与本库 [LICENSE](../LICENSE) 字节相同（AGPL-3.0）；各资产内原有的许可证及署名全部保留，包括 [docx/LICENSE](docx/LICENSE)、[frontend-slides/LICENSE](frontend-slides/LICENSE)、[skill-creator/LICENSE.txt](skill-creator/LICENSE.txt)。各项的具体许可仍以对应文件为准。

源码安装使每一代可执行文件读取同代 `source/skills`，更新随 `current` 原子切换，旧代技能随旧代源码保留。显式指定外部技能目录时继续使用该目录，不以随包技能覆盖它；技能依赖及凭据仍按各项说明配置。

## 本系统自产技能规划

> 布局遵循 Agent Plugins 1.0.0：`skills/<名>/SKILL.md`——**一个包下可以有任意多个 skill，共用根目录那一份 `plugin.json`**（规范 §5.1：每个插件恰好一份清单）。
>
> 旁边的 `plugin.json` 是这套技能的**打包清单**——它让整套技能可以带走，换台机器、换个项目都能装上。
> 防膨胀：人类入口 ≤2 常用、≤4 上限；子技能挂主技能编排；淘汰进 `_archive/`。
> 增删判据三问：功能是否重复？调用条件是否明确？能否说清它解决了哪个现有能力解决不了的缺口？

| 技能 | 四步走 | 角色（守哪条铁律） | 状态 |
|------|--------|------------------|------|
| `pdrs-init` | 初始化 | 定方向 | 待建 |
| `pdrs-research` | 分析 | 找依据、排真相源 | 待建 |
| `pdrs-mini` | 原型 | 第一件作品与定标 | 待建 |
| `pdrs-evolve` | 演化 | 放量与护栏 | 待建 |


## 三层能力

装技能之前先分清这三层，别把专属能力当核心能力使：

| 层 | 是什么 | 例 |
|---|---|---|
| **元能力** | 生成能力的能力。**它生成的是有载体的能力**——看得见、装得上、交得出去 | 四步走这四个技能本身 |
| **核心能力** | 跨领域通用 | 信息分析 · 行为分析 · 论证分析 · 决策分析 |
| **专属能力** | 只在一个窄口子里有效 | 你这个领域独有的那些 |

**先有元能力，才谈得上持续长出新能力。** 这也是「先建方法，再建作品」的由来。

## 两份清单，各管一层

| 文件 | 是什么 |
|---|---|
| `plugin.json` | **我是谁**——本系统作为插件包的身份（名字/版本/许可），跨厂商格式 |
| `42plugin.json` | **我装了谁**——从哪装、装了哪些外部扩展。这一层标准未定，随插件工具而变 |

并存不是冗余：**一个 AI 自主系统，既装别人的能力，也把自己的能力打成包给别人装。**
改完任一份跑 `python3 scripts/check-manifests.py`。
