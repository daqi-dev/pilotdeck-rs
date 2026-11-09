

### Gateway 整合：恢复读取失败保全与显式 Webhook routes（AI 代批，待人复核）

卡点：固定 TS 对恢复读取 current 的所有异常当作空数据，Rust 额外有50MiB读取上限，存在覆盖新数据风险；TS Webhook loader漏传显式routes使配置启用后仍404。影响仅原生隔离候选，均可通过代码回退撤回，不修改意向、冻结尺子或已记录失败。决定：恢复读取失败报告RecoveryFailure并保留current/backup/journal；只有成功读到空文件才可按TS正常恢复。Webhook仅透传明确extra.routes，缺省空路由和HMAC保留。理由：已授权完整功能与持久数据不丢失，不能以复刻基线缺陷作为完成。原则：失败读取不是空数据，明确配置必须到达消费者。分别落读取失败保全测试、显式/缺省routes与实际HTTP闭环；正式差异仍登记，不自行豁免。
