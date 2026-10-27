# Event buffer 金样

基线 cc7c4e6，生成器导入完整未改写的 `src/agent/protocol/events.ts`，直接调用 createAgentEventBuffer；不触碰配置或用户目录。
使用 Node v22.22.0 与基线 tsx loader，设置 `PDRS_BASELINE`，将全新输出目录作为 generate.mts 第一个参数；stdout 保存为 oracle.json。输出目录内 source.json 记录固定 commit 及 import 传递闭包 SHA，重生成守卫同时逐字节核验它。
输出 oracle.json 为两次 drain：按 emit 顺序消费所有事件，第二次为空。
有限生命周期与 run/generation 保护是 Rust 宿主新增边界，无该 TS API 金样；差异见 A6.9-INTERACTION-EVENTS-001。
