# #65 顺序与显式接纳域测试装置

本目录及 `src/pairing/{order_probe,result_acquisition,ready_admission,ready_audit}.rs` 提供测试专用观察、真实结果取得、显式 ReadyAdmission 和共同因果审计。`pairing.rs` 接点均受 `cfg(test)` 限制；默认生产协议与排序未因本装置而修复。#65、P3、A5.4 和真人实效继续未完成，状态正本是 `state/board.md`。

## 入口及证据范围

| 入口 | 做什么 | 已有证据与限制 |
|---|---|---|
| `run.mjs` / `probe.mjs` / `stdio-host.mjs` | 固定默认 TS Gateway 装配，六格输入与原 wire | 旧原件单独绑定身份，不能当本次 Rust 或当前 HEAD 证据 |
| `compare.mjs` / `compare-check.mjs` | 核 TS 与 native recorder 输出、输入及清理 | 不把 wire 相同当自然任务窗口已观察 |
| `domain-run.mjs` / `domain-controls.mjs` | JS 单提交者建模及离线负控 | 是模型原型，不是 Rust 默认产品接线 |
| `result_acquisition.rs` / `acceptance-probe.mjs` | A0 自然观察、A1 closure 前干预、A2 真 take 后缓存及 M1–M4 实际路径变异；另有 TS 取得/提交资格原型 | 自然未观察仍记 NOT_OBSERVED；干预不计自然兼容通过。历史运行逐次见 board，不能由已编译推断全部通过 |
| `ready_admission.rs` / `ready-host.mjs` / `ready-owner.mjs` | 显式 callback envelope 携带实际 Rust dispatch / NativeFiles payload，TS 内存 adapter 与 Tokio duplex 收集完整帧 | 仅显式受控域，不证明真实 kernel W0/C0、生产多连接身份或天然 settlement |
| `ready_audit.rs` | 正常和反控调用同一 R2/token/seq/wire 因果门 | X1/X2 拒绝由共同审计产生，反控分支只确认实际注入命中 |

R59 在源码 `f7938095711cd2e175dc4a5fb69586a61bf3adc2` 的精确 Cargo libtest 产物上实际运行：四正常 D1+/D1−/A+/A−及 D1+恢复共五健康双审计通过；X1 实际101、R2_REQUIRED拒绝，X2 实际101、COMMIT_SEQUENCE_VIOLATION拒绝。三个辅助 guard 通过，14 host 完整 capture/reap。原件主仓 `_tmp/20261016-debt-cleanup/order-architecture/ready-admission-r59-prep-0c23ix74/`，sealed index SHA256 `619ffc8400c1ff3bd5cee2bc9fc1e9f4751a0ca259a5161b57899d8fa85fb22a`；产物 SHA256 `efbcc3f7c1c05a9937ced074630ab45ad310c4bb5893e4c14e3e30646d1a78c3`。这份旧产物不代表后续 HEAD。

R61 使用原 loader 的 native caller 回归因输出预算提前终止；超量、EPERM与部分原件完整保留，不能说六格通过。R66 用已安装 tsx 的禁磁盘缓存分支验证新 wrapper 后，R67 在同一 f793/efbcc 产物上实际完成 Off/None 六 caller：describe-supported、list-supported、supported-list、list-ping、list-close、list-response-then-ping。六 host stderr 实际各167B、完整回收，无 tsx 磁盘缓存。原件 `order-architecture/native-finish-r67-cleanup-190100/`，index SHA256 `b0f87c93eb0b81d5d4d37e566e9f2a64a94af1fffdb0e75c379ea234bf01df09`。只证明共享 finish 的这六个调用者，不扩到 Natural/On/变异路径。

R68 在 `7b1c629d076a0c27addce27c10283b3cfe4f8a7f` 修复单行先分配后限额和无消费者的 qualified 字段；std-only 四个控制实际通过，其中三条为永久 guard，第四条把 R59 的14个 `*-pipe.json` 中92条实际已消费的 parsed stdout 记录重序列化后，经相同读取 helper 回算。该输入不是原始 pipe 字节，也不是新整 crate 七格重跑。原件 `order-architecture/ready-line-r68-f3za19ti/`，index SHA256 `293c5c83274d50238c07f3c13f5880517d009b77f997fd68e5a04a0e2ba0c204`。新分支编译、适用 CI、独立评审和相应整合验收各自留证，不替旧原件换 HEAD。

## 身份、运行与失败合同

基线固定 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，`guard.mjs` 核1000个 src 文件实际字节及配置。Node 固定原安装 v22.22.0，Node/Git/loader 使用显式绝对路径与 SHA256，运行前后重新核验；网络护栏仅覆盖已登记的固定版本表面，扩面债见 [#149](https://github.com/zhengdaqi/pilotdeck-rs/issues/149)。不下载预编译二进制，不从 PATH 猜工具，不改基线资源。

有副作用的入口和 Rust ignored recorder 必须由唯一协调者按任务、矩阵、工具与 source/binary 身份登记后运行，fresh 输出目录不得覆盖。编译期 `PDRS_ORDER_BUILD_SOURCE_HEAD` 必须等实际检出 HEAD，运行期 `PDRS_ORDER_BINARY_SHA256` 必须等本次产物；产物绑定不等于已运行。`PDRS_ORDER_*`、Ready 的 `PDRS_READY_*`、acquisition 的 `PDRS_ACQUISITION_*` 必填项以相应源码及先登记 task/matrix 为准，未填或漂移直接失败。不得用旧 ignored test 的原因字符串推断当前全项目未跑或已跑。

输出、独占 TMPDIR、配置与 fixture 均在本次自有目录；HOME/CARGO_HOME/RUSTUP_HOME 的选择遵循本轮任务和原工具链约束，不以更换 HOME 绕下载护栏。每流、单行、总证据预算及外层 PGID watchdog 各自核算。stdout/stderr 需完整 join；capture 写失败、超限、child未回收、缺件或身份漂移都不是成功。检查实际进程组后才按已登记策略发信号，保存每次具体 signal 结果；只收到 API Ok 不推断 OS 信号已发送。

`node --check`、`node check.mjs` 等静态检查不代表启动默认 Gateway 或执行 Rust。纯比较/重放也必须标明读取的旧输入及 SHA，不能冒原运行。全量53场景继续遵守 `/tmp/pdrs-parity.lock`，与这些局部 controlled 格分开计数。

## 预登记与未完成项

`domain-preregister.md`、`acquisition-preregister.md`、`acceptance-preregister.md`、`ready-preregister.md` 保留当时准备态及先于实现的任务指针，标题中的 PREPARED_NOT_RUN 仅表示该历史登记时点；其后执行以绑定 HEAD/产物的原件为准。原探索完整提交链保存在 `codex/completion-domain-pre-delivery-20261016`，新 source 分支不搬旧 state 队列。

扩域负控、重复 resolver/token、额外帧分类、多连接代际、独立 per-row prereg 身份、controller 期望码、网络/解码表面和同步 finish 调度的具体前置节点逐项在 [#149](https://github.com/zhengdaqi/pilotdeck-rs/issues/149)。延期仍未完成；到节点未还不得关闭阶段。真实默认混合批次顺序、completed-but-unpolled 自然窗口、生产提交资格、P3全域、A5.4与真人实效均不能从显式装置通过推导完成。
