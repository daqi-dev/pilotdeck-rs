---
name: no-unbounded-recursion
description: 对 wire 侧 JsValue（消息、schema、manifest）的遍历一律显式栈或复用 crate::js_value 的带上限实现；新写递归遍历必被闸门判 P0/P1（10-19 两次：A6.5-pr3 String()、A6.6-pr5 stable_serialize）。
metadata:
  type: feedback
---

pilotdeck-protocol 的不变量是「All container traversal uses explicit stacks」（codec.rs:2）；协议解析器能吃下数万层嵌套，任何递归遍历都会让 gateway 栈溢出整体 abort，而 TS 只抛可捕获的 RangeError、仅该请求失败。

**为什么**：10-19 A6.5-pr3（String() 无界递归，闸门 P1×2）、A6.6-pr5（fingerprint 的 stable_serialize，闸门 P0）同类两次，均由 worker 新写递归引起。

**怎么用**：派单的夹具规则里写明「对 JsValue 的遍历显式栈或复用 crate::js_value，深度上限用 `crate::js_value::MAX_ARRAY_DEPTH`（2000，保守值，不追 V8 精确阈值；见 state/decisions-20261019.md「卡 · JsValue 深度上限与 V8 实测阈值的差」）」；评审任务书里加一问「本 PR 有无对 JsValue 的递归遍历，或绕过深度上限的克隆/序列化入口」。第三次再出现就写进 CLAUDE.md。关联 [[runtime-mod-conflicts]]。
