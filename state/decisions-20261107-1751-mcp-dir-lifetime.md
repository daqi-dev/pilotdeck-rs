# 决策卡 · #1751 选项 A：A6.5 MCP per-session 目录寿命差异按冻结程序登记（B 不开工，留作可选优化）

> 本卡持久化 **#1751 的最终拍板**：人已选选项 A（先登记）。选项表与证据见 issue #1751 正文，
> 探针原始记录在 `_tmp/20261106-issue-401/probe/` 与 `_tmp/20261106-1751-1754-diff/`（不入库）。

| 卡项 | 内容 |
|---|---|
| 卡在哪 | A6.5 MCP per-session 目录寿命差异未登记：基线 `close()` 返回前同步 `rmSync` 删 per-session 临时目录，端口等迟到握手落地 / 握手超时后的 detached cleanup 才删（运行时先拆则残留）。按账本首页规则「没有登记的差异一律算 Rust 的错」，须人或批登记、或批改产品；选项 A / B / C 挂在 issue #1751 |
| 人怎么批的 | **2026-11-07T07:22:33Z，issue #1751 评论（zhengdaqi）原文**：「A（先登记，B 作为后续可选优化）」（https://github.com/daqi-dev/pilotdeck-rs/issues/1751#issuecomment-6703869035 ）；协调者回执 coord2/e7 2026-11-07T07:39:44Z（https://github.com/daqi-dev/pilotdeck-rs/issues/1751#issuecomment-6704022412 ） |
| 执行 | 按冻结程序登记：`specs/gateway-parity/diffs/A6.5-MCP-SESSION-DIR-LIFETIME-001-R1.md` 把该套原条目「未批准」更正为人批已接受；`specs/parity/diffs/A6.5-MCP-SESSION-DIR-LIFETIME-001.md` 新条目直接登记已接受（同 ID 跨套登记）。不改判分装置、阈值与分母 |
| 谁受影响 | MCP stdio per-session 目录寿命的可观察差异审计（`close()` 返回时基线 false / 端口 true）；方案 B 暂不实施，宿主进程在迟到握手落地前退出时目录残留 `$TMPDIR` 的用户感知保持现状 |
| 能否撤回 | 能。撤销登记须再走独立 issue + 两家评审 + 重新登记；方案 B（`close()` 即删在飞握手目录）若后续要做，在 #1751 决策线上单独拍板 + 独立 PR |
| 动不动尺子 / 意向 | **不动**。纯差异登记：冻结的 `accepted-diffs.md` 字节不变（只新增 `diffs/<ID>.md`），`score.py`、oracle、阈值、`FROZEN.md` 均不改 |
| 选项与后果 | A（**已批**）登记差异，可见可审计，须独立 issue + 两家评审 + 真实数据复算 + 人批；B 改产品让 `close()` 即删，与 TS 对齐但动 MCP runtime 关键路径，需单独 PR + 两家评审；C 仅在 `client.rs` 注释一句，不满足账本首页规则（详见 #1751 正文选项表） |
| 持久指针 | issue #1751 评论 2026-11-07T07:22:33Z（人批原文，URL 见上）；PR #2225；钉子缺口跟进单 #2233（前置于 A6.5 批次复核） |
| 产物 | `specs/gateway-parity/diffs/A6.5-MCP-SESSION-DIR-LIFETIME-001-R1.md` · `specs/parity/diffs/A6.5-MCP-SESSION-DIR-LIFETIME-001.md` · 本卡 |

## 可复用原则

人拍「登记」类选项时，登记条目里点名的钉子缺口（正控 / 定值形负控 / 未实测交错）**当场开跟进单并写清前置节点**，不留在更正件的文字里等下一轮评审才被指出（2026-11-07 PR #2225 r1 评审 P2-c 实例）。
