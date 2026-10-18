输出被截断混排了，我重新用脚本精确提取两份 ruler 的 pairs 结构与裁决。
两份 ruler 的顶层 pairs 均为 10 对 TS–TS 全 PASS；190 对 ref 矩阵的 FAIL 是定标分类信号。现在核查 proxy_compare 细节与 #72 的两处注册差异。
proxy_compare PASS、零差异。现在抽检 proxy 原始录制的 WS 握手与 close 帧。
proxy 侧 7 连接全部握手 101 无 Date、每连接下行（u2c+downstream-write）close 恰 1 条。对照 direct 侧（ts1/ref1）确认语义一致。
录制证据核对完毕。最后核查装置 checkout、attempts 日志中的重试/REFUSED、以及规约的冻结门槛。
**无 P0/P1，可冻结**（登记动作留给协调者）。

- ① 自洽：TS 十对 10/10（两 ruler 顶层 pairs 全 PASS 零差异）；proxy PASS；26 遍 clock 全 PASS；UTC 日均 2026-10-14；HEAD=eb2f2c7（summary/batch/checkout 三处一致、checkout 干净）；26 遍 node/baseline/node_modules(b744de7b)/recorder_proxy=构建 sha(fe2fff64) 及 toolchain 全一致。
- ② 独立：26 个 run_id/PID/录制哈希唯一、时间区间无重叠；计分轮与池轮同用 5 个 ts 采集重算，未重采。
- ③ proxy 真实：7 个 WS 连接握手全 101 无 Date，每连接下行 close 恰 1 条，与 direct 侧逐一致；#80 后仍 PASS。
- ④ 无重采/REFUSED，59 次 space_check 全 approximate=false。

190 对 ref 矩阵 132 FAIL 属定标分类信号，非缺陷。独立评审 null、DRAFT 系流程待办，不影响证据成立。
