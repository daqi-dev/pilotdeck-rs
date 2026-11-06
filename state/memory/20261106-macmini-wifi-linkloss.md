---
name: macmini-wifi-single-link
description: macmini-01 唯一在用链路是 Wi-Fi；长链断链会作废整批宿主/全量对拍。B 已让等待器断链自动 resume + 证据分轮次目录；根治仍需现场插网线（#1498 人 11-06 选 B，暂不插）
type: feedback
---

**为什么**：2026-11-05 macmini-01 两次断链（07:5x 三台远端同时断，根因在本机侧；18:05–18:2x 只有 macmini 断），正在跑的整套宿主负控 / 全量对拍整批判 INVALID 重跑，单次墙钟损失 1–2 小时。机器本身没毛病：连续开机 10 天、pmset 自 10-29 起无 sleep/wake、sshd 正常、IP/MAC 未变、磁盘 150 GiB 可用。断的是**本机 ↔ macmini 的 Wi-Fi 链路**——en0 是 Wi-Fi 且是唯一在用链路（未插网线），实测抖动标准差 59 ms、峰值 270 ms。

**怎么用**：

- **已知限制（一处正本，别处只引用）**：macmini-01 仅 Wi-Fi。整套宿主负控（1.5–2 h）与全量对拍（53 场景、串行持锁）遇断链仍会作废整批，只是不再需要人工介入。排期给 macmini 留重跑余量，或把长任务改派 cb8（注意 cb8 车道更挤，cargo 名额排队本身也会造成 INVALID）。
- **B 已落地**（人 2026-11-06 批「B 关单」，执行记录见 https://github.com/daqi-dev/pilotdeck-rs/issues/1498#issuecomment-6662446655）：`remote-wait4` / `remote-host-chain20` / `chain-cb8-v3` / `chain-mbp2-v3` / `parity-full-run-mbp2-v2` 连续 3 次轮询失败后短 ssh 复核远端进程——存活转 resume（断链时长不计运行时长、仍计墙钟）、远端可达但进程不在连 3 次确认才判 98、链路仍断则继续等；证据落 `r<N>/` 轮次目录独占创建。`remote-wait.sh`/`v2`/`v3` 与 `host-local` 天然不适用（轮询期失败本就不判 INVALID / 无 ssh）。
- **根治仍是选项 A**：现场插网线（en0 走有线、系统设置里把「以太网」排在 Wi-Fi 前，5 分钟，不必关 Wi-Fi）。**未做**。再出现「只有 macmini 断、另外两台正常」时，先提插网线，不要先怀疑 macmini 宕机——先按本条核 en0 是不是还在 Wi-Fi 上。
- 相关：[[remote-worker-default]]（cb8 > mbp2 > macmini）、[[macmini-exec-stall]]（进程启动偶发卡 12–35 s，与断链无关）。
