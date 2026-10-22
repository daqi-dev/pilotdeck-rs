
## #421-surrogate-p1 · AI 代批，待人复核

**卡在哪**：A7.1-pr3 连接层入站帧字段（id/method/clientName/sessionKey）的孤立 UTF-16 代理处理。修复轮 6 解决了 params 展开，修复轮 7 把入站解析改为 parse_json，但 jv_str 用 to_utf8() 导致含孤立代理的 id/method/clientName 被误判缺失（关 4002），同时 sessionKey 的 JsValue 真值与 serde round-trip 取值双轨不一致。boundary 角色连续 3 轮各角度命中（满足「同一失败 3 次挂起」原则）。

**谁受影响**：#421 合并阻塞。

**能否撤回**：可；代码改动全在 `_build/wt-a64-pr7`。

**动不动尺子/意向**：不动冻结文件；在意向范围内。

**选项与后果**：
- A（自决继续）：jv_str 改为 equals_str 比较，id/method/sessionKey 统一走 JsValue 路径。代价：context.rs SessionKey 也需改为基于 JsValue 的 SameValueZero，影响面扩大。
- B（挂起请人）：当前轮停止 #421 修复，人确认方向后再开。

**建议**：选 B。孤立代理的处理正确性边界（TS 接受代理项 id/method 后返回 error response 还是 invalid_frame）需要真实 TS 金样验证，否则会循环修。请人实测 `{"type":"request","id":"\ud800","method":"describe_server"}` 的 TS 行为，确认是 4002 close 还是 error response，再定改法。

**状态**：AI 代批 B，待人复核。
