# 决策卡 · #1865 选项 3：保持现状（默认模式多信任 SSL_CERT_FILE / SSL_CERT_DIR 只登记、不消除）

> 本卡登记 **#1865 复核的最终拍板**：人已选选项 3。前史（#1640 口径裁决选项 1 与该放宽的代价登记）
> 见 `state/decisions-20261106-gateway-parity-r1.md`；本卡是它的后续，不回改该卡。

| 卡项 | 内容 |
|---|---|
| 卡在哪 | `state/decisions-20261106-gateway-parity-r1.md`「代价」行登记的**安全相关放宽**——Rust 网关默认模式（`CaMode::Default`）读 `SSL_CERT_FILE` 与 hash 命名的 `SSL_CERT_DIR`，项目基线（`cc7c4e65` 源码构建 Node）同条件两个都不读——挂在跟进单 #1865，等复核选「消除 / 显式开关 / 保持现状」 |
| 人怎么批的 | **2026-11-06T15:33:38Z，issue #1865 评论（zhengdaqi）原文**：「保持现状，只在文档/审计说明里写明『Rust 默认比基线 Node 多信任 SSL_CERT_FILE / SSL_CERT_DIR』。目的是留TODO，方便之后判断是否要做得更细致。」 |
| 执行 | 不改代码、不改 `oracle.json`、不改任何冻结文件；新增审计说明 `docs/research/011_tls-ca-default-extra-trust/audit.md` 登记该放宽并留 TODO；`A6.4-TLS-CA-SOURCES-001-R2.md`「范围」段已有登记原样保留（差异账本只追加） |
| 谁受影响 | 默认模式下设了 `SSL_CERT_FILE` / `SSL_CERT_DIR` 的用户：在这两个来源上信任的 CA 比项目基线多（不是整体超集，反方向边界见审计说明「边界」段） |
| 能否撤回 | 能。TODO 指向 issue #1865 正文「建议做法」段：选项 1（改判分语义，`CaMode::Default` 只加载 bundled + `NODE_EXTRA_CA_CERTS`）要改 `oracle.json` 8 格期望（darwin 基线数字；Linux 基线改格数待实测）+ 代码分支 + 再补更正件，须另走一次明确批准；选项 2（补显式开关）不改判分语义 |
| 动不动尺子 / 意向 | **不动**。判分语义、`oracle.json`、阈值、`FROZEN.md` 均未改；纯文档登记 |
| 持久指针 | issue #1865 评论 2026-11-06T15:33:38Z（人批原文）；审计说明 `docs/research/011_tls-ca-default-extra-trust/audit.md`；前史 `state/decisions-20261106-gateway-parity-r1.md` |
| 产物 | `docs/research/011_tls-ca-default-extra-trust/audit.md` + `docs/research/README.md` 索引行 |

## 与 #1640 决策史的关系（防「人从头就支持现状」的误读）

#1640 裁决时（2026-11-06）人的评论按时间并列：02:57:54Z「**按照推荐，批选项 2**」（当时
#1640 正文的建议正是选项 2）→ 03:11:10Z「**1 和 2 都行，怎么快怎么来**」→ 协调者据此定**选项 1**
（零改尺子），放宽的代价登记为跟进单 #1865。#1865 复核时（同日 15:33:38Z）人拍板**选项 3（保持
现状，只登记）**。即：该放宽先被批过「消除」（选项 2），随后放宽为「都行」，最终在复核节点定为
「保持现状 + 文档登记」——不是人从头就支持保持现状。

## 可复用原则

跟进单复核拍板要当场落一张独立决策卡持久化人批原文（时间戳 + 指回 issue 评论），不依赖不入库的
`_tmp/` 任务书——评审工具看不到 issue 评论与 `_tmp/`，仓内无出处会被判「批准出处不可核」（2026-11-07
#1865 收货评审 P1 实例）。
