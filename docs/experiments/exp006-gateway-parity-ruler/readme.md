---
type: experiment
number: "006"
date: 2026-10-14
title: Gateway 层对拍尺子：同路径五遍校准与透明代理回归
status: draft
related: [plan/002_phase-a-gateway, experiments/exp003-transparent-proxy]
---

# exp006 · Gateway 对拍装置

## 假设

同一 TS gateway 在相同 PILOT_HOME/cwd/TMPDIR、固定配置和确定性 mock 模型下，连接内全帧顺序、最终落盘和 HTTP 响应经窄归一化应稳定。A1 透明代理应保持这三面完全一致。尺子必须拒绝已知差异、缺场景、截断和两侧同坏，才有资格量 A4 本地化。

## 做法

实现入口 [`scripts/gateway-parity/README.md`](../../../scripts/gateway-parity/README.md)，规约在 [`specs/gateway-parity/`](../../../specs/gateway-parity/acceptance.md)，当前均为草稿；未修改阶段 C 冻结规约。

全量运行共用 `/tmp/pdrs-parity.lock`：guard 串行发布 owner.json、死 PID/超龄接管、等待超时、token 校验释放；TS 五遍和代理一遍整体持锁。每侧先起隔离 mock、TS gateway，再起装置自有 observer，代理侧额外起 Rust；driver 从基线公开 source index 导入官方客户端，应用帧由 driver 录制，原始 WS 探针另录 wireHex。`PDRS_BASELINE` 默认 `/Users/da/ws/pilotdeck-rs/resources/PilotDeck`，只读且已构建。

| 角色 | 实验端口/地址 |
|---|---|
| TS gateway | 127.0.0.1:28791 |
| A1 Rust 代理 | 127.0.0.1:28792 |
| 装置上游录制点 | 127.0.0.1:28793 |
| 独立 mock 模型 | 127.0.0.1:28794 |

直连 driver→TS；代理 driver→Rust→observer→TS。所有轮次使用同一个 `<output>/cur/home`、`cur/project`、`cur/tmp`。关完自有进程组后 **rename cur 到 label**，下一轮重建 cur；既有证据拒绝覆盖。预检端口占用，实际 listener 用自有 PID 的 lsof 校验只绑 127.0.0.1；不按端口查杀、不碰 3001/18789 或真实 `~/.pilotdeck`。

八组：hello 鉴权、完整 submit_turn、10 个 (a) 只读成功响应、协议断线重连与 pending 回放、真实权限 deny、cron/skill 落盘、HTTP（health/API/read/write/401/增量 SSE/NDJSON）、四种原始 WS 怪癖。详细 oracle/分母见 acceptance。10 个来自 method-inventory 末尾协调者已修正的小结；正文/计划残留 15 不扩成分母。

mock 独立进程，不用 A1 的同 gateway listener 模型替身；模型客户端/SSE 解码仍为基线生产实现。`/__ruler/stream/*` 是明确的测试运输夹具，通过 gateway 预载转发到 mock，不冒称生产 HTTP 流式 API。macOS 沙箱 fs.watch EMFILE 的浅层 polling 替代独立在 watcher.mjs，两侧一致启用，不改变只读基线文件。

## 冒烟发现与修正

1. 原生 archive artifact 实际在 **项目** `.pilotdeck/tool-results/<session>/`，不是 home/chat 下；固定成功读夹具后才覆盖 native_archive_artifact。
2. CLI 没有 cron 配置时 cron_create 返回 capability unavailable；配置 `cron.enabled=true`，使用固定 2099 once 只写盘不触发。
3. `echo` 原本被默认安全规则允许，不能作权限问答；隔离 permissions.json 显式 ask bash 后真实 permission_request/permission_changed/deny delivered=true 都被录到。
4. unmasked 并非单连接标准 close：基线手写 parser 抛错，CLI 的 uncaughtException 进入 shutdown，监听器关闭，出错连接可能暂时悬挂。探针外部检查 health 不可用，再清理自己的 socket；不用等待一个不存在的 WS close code。
5. 两遍 TS 对拍确定的噪声是文件 mtime、项目创建/活跃时间、skill mtime、run registry PID/acceptedAt/updatedAt，以及工具结果时间进入第二步模型 requestFingerprint。时间仅按容器形状归一化；原始 hash 先校验，再对完整归一化 canonical request 重算。token/预算、duration、业务 schedule、seq/revision 全保留。

初轮夹具失败保存在 [`_tmp/20261014-exp006-smoke/smoke/`](../../../_tmp/20261014-exp006-smoke/smoke/run.json)；修正夹具后的两遍 TS 原件在 [`smoke2/smoke`](../../../_tmp/20261014-exp006-smoke2/smoke/run.json)、[`smoke2/smoke2`](../../../_tmp/20261014-exp006-smoke2/smoke2/run.json)，早期窄口径自比 PASS 在 [`comparison-final.json`](../../../_tmp/20261014-exp006-smoke2/comparison-final.json)。这些是冒烟/定口径证据，正式尺子加强 wire 完整性后，以正式六轮为准。

## 代理差异与改变的取舍

修复前 [`proxy-before/scenarios/quirks/result.json`](../../../_tmp/20261014-exp006-smoke2/proxy-before/scenarios/quirks/result.json) 明确 FAIL：RSV 与孤立 continuation 在 tungstenite codec 被拒绝；unmasked 被 codec 拦住，没有触发 TS 的异常 shutdown。其余七组成功。原件和 [`proxy.log`](../../../_tmp/20261014-exp006-smoke2/proxy-before/proxy.log) 保留，没有把该差异登记为允许项。

因此调整 A1 的**透明运输层取舍**：握手用 Hyper 完成上下游 HTTP upgrade，保留 upgrade 读缓冲与上游头；升级后用 Tokio copy_bidirectional 搬全部原始字节，1:1 成对关闭。TS 自己决定 RSV/continuation/masking/control/close 语义。删除 codec 的整消息缓冲与 4MiB/16MiB 人为上限，改为固定大小字节运输缓冲；不把 WS 业务解析搬进 Rust。无 Rust native method 实现。A4 需要方法分发时，须另立明确的帧解析行为，不能再次把标准库拒绝行为误当成基线契约。

Rust 内存 duplex 回归覆盖四种原始头、128KiB 载荷、TCP 半关闭及返回真实 close 字节；保留 HTTP/握手头/坏 key/accept 分类测试。全 workspace fmt/clippy/test 已通过 694 个测试，完整输出见 [`rust-checks.log`](../../../_tmp/20261014-exp006-rust-checks.log)。

## 正式结果

正式账本 [`ruler.json`](../../../_tmp/20261014-exp006/ruler.json)，原始 stdout/stderr [`ruler-final.log`](../../../_tmp/20261014-exp006-ruler-final.log)。全套六轮合计约 48 秒；五个不同 TS gateway PID 为 33906/34064/34141/34245/34360，代理轮 TS PID 34449、Rust PID 34488。各轮 home/project/tmp 运行路径完全相同（正式 output/cur），归档分别为 ts1–ts5/proxy。

| 项 | 实际结果 | 原件 |
|---|---|---|
| TS 直连自比 k=5 | **true / 100%**：10 对均八组、三面 PASS，零排除 | `compare-ts1-ts2.json` 至 `compare-ts4-ts5.json`，五个 tsN/run.json |
| 阳性与逐规则对照 | **true**：11 个注入/损坏对照全部 FAIL，命中预期规则；包括请求指纹两侧同损坏 | [controls/results.json](../../../_tmp/20261014-exp006/controls/results.json) 和各项 comparison.json |
| 两侧同坏 | **true**：将两侧可见模型输出改为 WRONG_MODEL_OUTPUT，driver 的原 PASS 标记仍在，独立 G3.oracle 拒绝 | [both-sides-bad/comparison.json](../../../_tmp/20261014-exp006/controls/both-sides-bad/comparison.json) |
| 独立审查 | **null，待协调者** | ruler.json；整体 PENDING_REVIEW，check_ruler exit **2** |
| 直连 vs 经 A1 代理 | **true / 100%**：八组、帧/HTTP/落盘全 PASS | [compare-proxy.json](../../../_tmp/20261014-exp006/compare-proxy.json) |
| 透传不能冒充本地实现 | **PASS（正确拒绝）**：指定 supported_agents 本地化，实际上游仍录到请求，FAIL/G7.no-forward | [false-local-claim.json](../../../_tmp/20261014-exp006/false-local-claim.json) |
| 锁与装置安全回归 | **9 tests OK**：含等待后重新起算租约时间 | [safety.log](../../../_tmp/20261014-exp006-safety.log) |
| 冻结检查 | **DRAFT / exit 2**，未自行批准或建立 manifest | [frozen.log](../../../_tmp/20261014-exp006-frozen.log) |

每侧正式录到 **122 条 WS 记录**：auth 11、turn 13、readonly 23、reconnect 22、permission 27、writes 9、quirks 17；HTTP 另录 8 次响应含两个运输流。driver 全部 exit 0。以 ts1 为参照，代理侧数量与完整语义一致，四项原始 WS 怪癖也一致，没有允许差异。

全程的 10 个运行目录（含冒烟与修复前失败）共 **112 个自有服务/driver 进程条目**，审计全部组已停，实际 listener 全为 127.0.0.1，见 [audit-all-runs.json](../../../_tmp/20261014-exp006/audit-all-runs.json)。Python 等待/检查进程也已结束。两个 600 秒等待超时后改用 1800 秒队列；仅缩短轮询，没有接管活跃 owner。旧等待进程通过其工具 session 的 Ctrl-C 停止；尝试跨工具 exec 信号时被沙箱拒绝，未碰其他进程。全量结束释放自身锁，后续其他阶段的 owner 保持原样。

**结论：八组场景下装置的机器可信度检查成立，透明代理应答、持久化和 HTTP 均可 100% 对拍；可以交协调者独立审查后冻结。** 这份结论不等同于 Rust 已实现任何本地 method，也不把 null 或 DRAFT 改成 true。

## 边界

这套尺子证明的是固定八组场景的三面一致，不证明所有 101 method、生产 DOM/桌面、人肉 Web 验收、递归热加载、真实模型、并发吞吐或 TS 退役。原生 WebSocket API 录应用消息；底层 control/帧头由四个 raw probe 单独测，不声称录了所有隐式 ping/pong。

重连场景显式保存旧 binding，验证协议回放；保留两侧共同的 post-reconnect answer.delivered=false 并 abort 清理。A1 的真实 UI 自动恢复缺陷仍未修复，不把协议回放称为 UI 恢复或轮次完成。SSE/NDJSON 是运输测试夹具。单个 cron task、默认单项目 SQLite 不覆盖复杂多随机文件名/多业务表的排序；扩场景后须再次 TS 五遍校准。schema 导出属于 A3，本轮结构检查不替代它。
