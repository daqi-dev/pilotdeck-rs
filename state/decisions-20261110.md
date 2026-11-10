

### Gateway 整合：恢复读取失败保全与显式 Webhook routes（AI 代批，待人复核）

卡点：固定 TS 对恢复读取 current 的所有异常当作空数据，Rust 额外有50MiB读取上限，存在覆盖新数据风险；TS Webhook loader漏传显式routes使配置启用后仍404。影响仅原生隔离候选，均可通过代码回退撤回，不修改意向、冻结尺子或已记录失败。决定：恢复读取失败报告RecoveryFailure并保留current/backup/journal；只有成功读到空文件才可按TS正常恢复。Webhook仅透传明确extra.routes，缺省空路由和HMAC保留。理由：已授权完整功能与持久数据不丢失，不能以复刻基线缺陷作为完成。原则：失败读取不是空数据，明确配置必须到达消费者。分别落读取失败保全测试、显式/缺省routes与实际HTTP闭环；正式差异仍登记，不自行豁免。


### 模型输入兼容资产窄范围复用（人已批准）

用户在当前整合会话明确批准：仅复用固定 cc7c4e6 中模型可见的固定提示文本、工具描述和 JSON Schema，保留来源与许可，作为兼容数据交付。此授权独立于先前32技能授权，不扩大为复制TS业务实现；项目路径、时间、agent深度与动态工具清单仍由真实运行时提供。原则：固定模型输入按原合同保真，执行逻辑自行实现；字段与token差异通过实际请求比对判定，不调计数器凑绿。另用户决定先完成当前阶段，到完成节点再讨论迁移；本机编辑/远端构建及原统一目录安排继续，未执行迁移。

### 缺失正文的 replacement 事务保全（AI 代批，待人复核）

正文缺失不能证明用户有意删除。新会话/portable restore确需阻止旧事务影响新生存期，因此在持有正文写锁时把孤儿事务同目录原子改名为不被自动恢复识别的 quarantine 文件，并保留字节；已确认成功删除正文的路径仍按删除语义清除sidecar。晚到rollback先验证正文仍可读，缺失/读失败不复活正文、不消耗备份。可撤回，不改变冻结判分；原则：先失效自动回放，再保留不明来源的唯一恢复副本。定向验证在mbp2 r54/r55，日志见主仓_tmp/20261110-gateway-integration/。


### Native rewind 的路径权限来源（AI 代批，待人复核）

卡点：新接入的真实文件历史消费者必须处理本地 JSONL 中的绝对路径；原 `rewind_files` 根据记录直接读、写、删除目标，记录自身不能证明授权。只 pin 备份目录不能保护目标路径。影响 native 文件回退；公共 TS 合同实现保留，正式差异未获豁免，不改冻结判分。

决定：native 路径以已配置项目根的打开目录句柄为授权来源，兼容同一根的配置别名和 canonical 路径；逐组件 nofollow，拒绝外根、符号链接及非独占普通文件；全体路径预检后再修改。拒绝不能借未认证 transcript 自动扩大权限。单次审批过的项目外文件目前没有可认证的持久授权记录，因此该回退场景保守拒绝，必须在完整替换差异账中保留，不能称完全 TS 等价。可撤回：将来可接已有明确授权根或另建可信授权消费者后拓展；本轮不静默放宽。

依据：native methods/tools.rs 的 rewind 入口只限制 transcript 位置；runtime/rewind.rs 读取 trackedFileBackups/expectedFileStates；storage/file_history.rs 原空 expected 表不产生冲突，Null 备份会删除记录指定路径。当前 portable restore 未确认能注入 file_snapshot_recorded，不将其写成已证实攻击入口。测试断言覆盖根外路径、父/叶符号链接、硬链接、合法路径先于恶意路径时无部分修改，以及正常重启后回退。候选、前像和实测索引：主仓 `_tmp/20261110-gateway-integration/received-rewind-target-guard/`；r65 正常闭环通过，安全增量待 r66。
