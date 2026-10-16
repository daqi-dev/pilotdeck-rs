---
name: review-bounds
description: 外部模型pr-ready角色按小批和精确增量派活，避免120秒内反复重读全仓。
type: feedback
---

首次同时给三个DeepSeek角色全量四批材料，三者均在120秒被中断；继续已有session才完成。超时不是通过，不得拿partial或退出码冒充完整结论。

每角色只读自己维度的必要文件，将脚本/元数据/文档分成各少于500行的批次；先取逐批结论。后续保留session和确切diff，仅审上轮后的增量；长链推演要求收敛已有证据，不重复全仓读取。每个有界调用核final、退出码与实际等级；同一具体失败3次挂起，转独立工作。审阅原文、模型来源和未验边界分别留盘。

### 结束事件不等于完成评审

记录任务/source SHA、实际cwd/候选HEAD、模型通道、起止时间、timeout、JSONL/stderr/退出。socket错误先比对本地terminate时间；仅有runner终止逻辑与socket关闭事件不足以判先后，必须有原始时间证据。先前简述“本轮GLM的Transport关闭在runner发SIGTERM之后”不能泛化到当前环境核心的两次失败：它们未留SIGTERM发生时刻，具体先后未知，不能据此断定提供者故障。健康probe的CHANNEL_OK不等于具体审查通过。明确HTTP400/content-filter保留原件并停该scope，不换词绕审。

每份final另核角色专属覆盖、发现表及等级、未验边界；空文本、仅“继续检查”、漏批次均为PARTIAL，即使exit0/type=final也不算过闸门。传输完成与实质完成分别记录，原文不改写。R24的新前提诊断对未尝试catalog候选保持原task字节、单GLM串行standalone有界300秒；实际124.538秒得到完整结论，证实旧120秒可能截断有效工作。这个本轮有界诊断不能泛化为永久延时或无限重试。

逐子项改档先核稳定ID、原标题、facet和priorSource。一次有效GLM输出把85.2（C未登记文件/Python）错映为85.1中的A-cargo，中央先发17条、root发现后回退14条；错误快照与回退证据保留，未进入canonical。退出成功和等级碰巧相同不能补scope错配。新修订先落独立next及这四列preview，由canonical持有人实核后再原子发布；这是工作区数据协调，不是新增用户批准。错scope原文保留，只对精确缺口求同家澄清，不擅改原话或换ID搬用其等级。

### 结构化原件输出（第三次同类大输出后固化）

检查JSON先用正向白名单取keys、计数、哈希和有限叶路径；未知字典不能用“排除几个大字段”后整块打印。含rawText/内嵌JSON的文件不用裸rg或全文输出：必须加`--max-columns 200 --max-columns-preview`，或在Python中只输出命中路径和前120字符。一次只读需要的有界片段；原件保留在文件，避免工具截断掩盖真正证据。

执行派发：给已完成或idle的worker新任务必须用`followup_task`；`send_message`只用于正在运行任务的steer。dispatch后核running状态或首条进度，queued消息不代表已经开工。2026-10-16协调中第三次出现idle消息只排队，故固化为执行规则。

验证到提交必须失败即停：一次临时核验把FROZEN表的16位前缀误当完整SHA，断言失败后同shell下一行仍提交。实际README完整SHA此前已匹配，后续正确前缀核验通过；这不代表命令编排可靠。断言和提交分成依赖调用，读取exit0后再提交；不能用无`set -e`的换行串联当闸门，所有失败原始输出照留。

### 同一环境核心第三次空结果后挂起（2026-10-16 13:03）

三次原件`reviews/metadata-latest-source-environment-glm-exit.json`、`metadata-latest-tool-env-glm-exit.json`、`metadata-environment-third-glm-exit.json`分别300.085/300.095/300.040秒、timeout=true、textCount=0，均不算有效审阅；路径前缀为主仓`_tmp/20261016-debt-cleanup/`。第三次虽有逐字可复用preflight和新0c11实际环境/postflight这两个实质新前提，仍未取得final，故当前环境核心scope挂起，不再第四次改写/缩包/变换措辞或重复延时。raw/stderr/task与SHA保留，不归因供应商、过滤或任一事件先后；不凭其它scope成功填此缺口。

只停该scope及依赖它的合并。转真正独立的任务/角色；不同memory函数、31单账本或独立产品问题须有各自明确源范围，不能借此重新包装同一环境核心。将来解除挂起须记录新的外部状态或独立事实，明确为何能改变这类失败；旧健康probe与纯拆包不是新前提。pre-provider硬门失败且未发请求单独记，不算完成一次provider评审，也不可拿旧门证据绕过当场准入。
