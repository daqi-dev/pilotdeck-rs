---
name: review-permissions
description: 外部模型只读审阅的硬执行边界；no-tools提示词、plan旗标和模型自述不能充权限证明。
type: feedback
---

2026-10-16，DS在明确no-tools任务仍调用19/31工具，对通用/tmp路径先rm再重建。删除前无stat，原状态未知，不能因现目录新建就宣称无损。GLM的plan旗标亦未证明runtime写被禁。完整事实与原始调用索引见主仓`_tmp/20261016-debt-cleanup/reviews/tool-boundary-incident.md`，未经授权运行不作验收。

每次外审前以执行层落实权限。使用本次CLI overlay硬禁工具时，须核本机实际registry空且shell/read/write/edit/subagent等均无执行入口，绑定overlay/loader/proof SHA，漂移或工具重新可见即在provider启动前拒绝。无工具文本API须核当前installed handler没有默认工具或agent/session执行循环；请求schema没有tools字段不够。只停未核通道，不杀有界在途provider、不退回宽权限默认、不读取/复制账号凭据。新入口的空final、字面工具调用和CHANNEL_OK都不代表完成审阅；实际角色覆盖及双谱系仍须齐。需要运行时事实，由协调者在已核owner的专有目录执行并把原件交非作者复核。

同类三次AgentNotFound的真实根因是子进程cwd虽设正确，继承PWD仍指回主仓；本机OpenCode原生run优先t.root/PWD而非cwd。以后任务入口必须同时绑定canonical cwd与子环境PWD，先以真实session导出的白名单agent/model/location验证，不以debug agents或模型自述代替。已保留三次失败，唯一改动PWD的有界试验25.175s成功。GLM恢复仅限固定pdrs-review-no-tools入口：installed binary/config/snapshot SHA、真实session agent/model/canonical目录匹配；相同installed筛选回调当前策略definitions=[]、允许阳性4定义、request prepare不能hook添工具；这是等价执行门证据，不冒称实际wire捕获或deny事件。主仓reviews/glm-context-research/native-active-context-proof.json为索引，漂移即启动前拒绝。
