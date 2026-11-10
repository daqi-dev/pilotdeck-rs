# 真实 Server 的渠道 Cron 回送探针

非冻结产品夹具；无 Cargo、无真实账号/凭据、无外部模型请求。必须独占新输出目录，并与其他实际 Gateway 对拍共用 /tmp/pdrs-parity.lock。固定 Node22.22.0/ICU78.2；外层调用者先核 source manifest 与 Gateway/sidecar SHA，再传 PDRS_SOURCE_MANIFEST_SHA256。该变量由本夹具记录，不能单凭标签冒认构建来源。

原生运行：
PDRS_SOURCE_MANIFEST_SHA256=<SHA> node weixin.mjs <gateway> <sidecar> <new-out>

固定 TS 对照：
PDRS_PROBE_ENGINE=ts PDRS_BASELINE=<cc7c4e6只读源码> PDRS_SOURCE_MANIFEST_SHA256=<目标Native来源SHA> node weixin.mjs <gateway> <sidecar> <new-out>

TS 通过现有 ../_shared/baseline.mts 校验固定源码。TS 实际默认凭证路径是隔离 HOME/.pilotdeck/weixin-credentials.json，Native 是显式 --home 根目录；分别按真实产品路径写合成凭据，不碰用户 HOME。TS 子进程用 NODE_OPTIONS 预载 loopback-network-guard.mjs；必须有 actual readFileSync 凭证读取有效见证并且没有 denied。守卫在 Node net/TLS/fetch/DNS 原调用之前拒非数值回环；只准受控 TMPDIR 内真实父目录的 IPC，允许 data/file/blob 非网络资源，UDP 全拒。它是 Node 产品测试 preload，不宣称内核沙箱或任意非 Node 子进程的网络隔离。

先以 guard-negative.mjs 验证官方/非回环 net、fetch、非本夹具 Unix 地址被拒，以及数值回环 TCP、owned IPC、data 资源的正控；具体运行环境变量见独立 guard-network-negative 原件。任何真实测试 denied 都记 FAIL，不能当 mock 通过。

真实路径：生产配置启用 Weixin/Cron；公开 list_projects 核实际 .cwd 注册；ILink 入站建立 context token并经真实 A7；公开 cron_create/run_now 触发新 A7并持久完成；第一次回送由 mock 返回 HTTP500，随后真实 poll 完成触发 pending reply 重送200，A7请求只有1次；reload_config禁用来源后另一 Cron仍实际A7完成，但完整 owned shutdown 后发送计数不增。ret1+HTTP200不是SDK发送失败，不能用它冒认重试。

证据包含 raw model HTTP、ILink 请求、WS frames、持久 Cron结果索引、stderr及二进制摘要。先前错误凭证位置导致至少一次非预期官方 QR fetch，次数上界未知、无真实账号/凭据，原件与事件记录必须保留且不算mock通过；之后改为精确路径与 fail-closed preload。其他装置失败均独占保存。TS/r71两端实际PASS不替代正式尺子/人签字；新整合头须重跑。SMS另用现有cfg(test)端点seam的实际Server测试，不能冒充生产bin身份。
