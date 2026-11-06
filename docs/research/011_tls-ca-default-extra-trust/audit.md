---
type: research
number: "011"
date: 2026-11-07
title: "A6.4 TLS CA 审计说明：Rust 默认模式比基线 Node 多信任 SSL_CERT_FILE / SSL_CERT_DIR 的 CA（#1865 选项 3）"
tags: [phase-a, audit, tls, ca, #1865, #1640, #1647, #1654]
status: 人已拍板选项 3（保持现状，issue #1865 评论 2026-11-06T15:33:38Z，决策卡 state/decisions-20261107-1865-option3.md）；放宽未消除，TODO 待复核
related: [specs/gateway-parity/diffs/A6.4-TLS-CA-SOURCES-001-R2.md, state/decisions-20261106-gateway-parity-r1.md, state/decisions-20261107-1865-option3.md, specs/gateway-parity/diffs/A6.4-TLS-CA-SOURCES-001-R1.md]
---

# 011 · 审计说明 · A6.4 TLS CA 来源：默认模式多信任两个显式环境来源（#1865）

> 本件是 #1865（选项 3）交付的审计说明，登记一条**安全相关的放宽**并留 TODO。
> 拍板出处：issue #1865 评论 2026-11-06T15:33:38Z（人批原文与决策卡见
> `state/decisions-20261107-1865-option3.md`）。
> 差异账本正本 `A6.4-TLS-CA-SOURCES-001-R2.md` 属 `specs/gateway-parity/diffs/` 只追加
> 账本、不可回改，故交叉引用单向：本件 → 该件；该件「范围」段已有的同主题登记继续有效。

## 结论（一句话）

Rust 网关的默认 CA 模式（`CaMode::Default`）在 bundled Mozilla 根之外**还会读
`SSL_CERT_FILE` 指向的 CA 文件与 `SSL_CERT_DIR` 指向的 OpenSSL hash 目录**，而项目基线
（`cc7c4e65` 源码构建 Node v22.22.0 / ICU 78.2）在同样条件下**两个都不读**、握手失败——
即基线拒绝的内部/自签站点，Rust 默认模式下可能接受。同一审计口径下 Rust 信任的 CA 更多。

## 证据与口径

- **Rust 行为**：`src/pilotdeck-gateway/src/runtime/model/http/tls.rs`（#1552 / PR #1647 落地）。
  `roots_from`（`:67-96`）先铺 bundled 根，随后 `if mode != CaMode::Bundled`（`:78`）时读显式
  `SSL_CERT_FILE`（`:79-81`）与 hash 命名的 `SSL_CERT_DIR`（`:82-89`，逐路径调 `load_hash_dir`）。
  模式选择 `ca_mode`（`:42-52`）只认 `--use-openssl-ca` / `--use-bundled-ca`（经 `NODE_OPTIONS`
  分词），**其余一切旗标组合（含所有 `system-*` 家族）都落进 `Default`**，于是照样读两个来源。
- **基线行为**：源码构建 Node 默认模式只用 bundled CA；实测见
  `A6.4-TLS-CA-SOURCES-001-R2.md`「基线依据」表（证据
  `_tmp/20261105-archive-wt-1162/20261105-issue-1162/r1/tls.log`、
  `_tmp/20261106-1552/option2-facts.md`，mbp2 / darwin arm64 实测；本件未新做 TS 实测，
  全部转引该两份既有实测）。
- **为什么现在这样**：#1640 口径裁决。冻结账本基线是 macOS Homebrew Node v22.22.0（默认模式也接
  OpenSSL 默认路径，`dir-unflagged` 全 true），项目基线是源码构建（全 false），两份实测冲突。决策史按
  时间并列：人 2026-11-06T02:57:54Z「**按照推荐，批选项 2**」（当时 #1640 正文的建议正是选项 2）、
  03:11:10Z「**1 和 2 都行，怎么快怎么来**」，协调者据此定**选项 1**（零改尺子）；#1865 复核时人
  2026-11-06T15:33:38Z 拍板**选项 3**（保持现状，只登记）。#1640 裁决与代价见
  `state/decisions-20261106-gateway-parity-r1.md`；#1865 拍板原文与决策卡见 issue #1865 该评论与
  `state/decisions-20261107-1865-option3.md`。

## 影响范围（按场景族）

| 场景族 | 条件 | 基线（源码构建） | Rust |
|---|---|---|---|
| `dir-unflagged` | 只设 `SSL_CERT_DIR=<c_rehash 目录>`，无任何 `--use-*-ca` 旗标 | 四客户端全 **false** | **true** |
| `ssl-unflagged` | 只设 `SSL_CERT_FILE=<CA bundle>`，无旗标 | 四客户端全 **false** | **true** |
| `system-*` 六格 | `--use-system-ca` / `NODE_USE_SYSTEM_CA=1`/`0` / `NODE_OPTIONS=--use-system-ca` 等组合 | 全 **false** | 全 **true** |

六格同向是同一机制：`ca_mode` 不识别系统 CA 旗标（含显式 `0`），一律按 `Default` 处理。平台注记：
darwin 基线 `system-dir` 全 false、Linux（cb8）源码构建实测全 true（`--use-system-ca` 走 OpenSSL
默认路径），跨平台不齐，详见 R2「范围」段的平台对账。`early-file`（extra 文件启动后、首个客户端前
写入）**不属于**本放宽——Rust 首次默认连接器构造时快照（`client_config` `:14-18`），两侧均 true，
按已登记差异处理。作用面（与 R2 原文一致）：MCP HTTPS、HTTP hook HTTPS、模型 HTTP、token count、
HTTPS 代理。

## 边界（防过度解读）

**本条不宣称「Rust 的信任集合 ⊇ 基线的信任集合」**，只限定在上述两个显式来源与已实测条件上：

- bundled 根集合两侧未证同源（原条目已登记）；
- Rust 跳过超过 8 MiB 的 CA bundle（`tls.rs:128` `MAX_CA_FILE_BYTES`），Node 无对应上限——存在
  Rust **少**信任的反方向情形；
- `--use-openssl-ca` 下保留 bundled 根的超集问题等四条残余另见 `-R1.md` 与 #1654，不归本件。

## TODO（#1865 选项 1/2 改法清单指针）

**TODO：若需与基线审计口径一致（默认模式不读 `SSL_CERT_FILE` / `SSL_CERT_DIR`），见 issue #1865
正文「建议做法（三选一，待人批）」段的选项 1/2 逐条清单。** `-R2.md` 不承载逐条改法，只登记「选项 2
要改 `tests/fixtures/private-ca/oracle.json` 的 8 格期望（darwin 基线数字；Linux 基线改格数待实测）、
须另走授权、本轮不再重测」。issue #1865 正文两行原文：

1. 「改判分语义走选项 2：`CaMode::Default` 只加载 bundled + `NODE_EXTRA_CA_CERTS`；`CaMode::Openssl`
   才加载 `SSL_CERT_FILE` + hash 目录。同步改 `oracle.json` 的 8 格期望，并新增更正件说明 Homebrew
   那几格在本机不可复现。」
2. 「保持选项 1，但补一个显式开关：默认不读 `SSL_CERT_FILE` / `SSL_CERT_DIR`，只有配置项（如
   `PILOTDECK_TLS_USE_OPENSSL_CA=1`）打开时才读，避免与基线审计口径分叉。」

落地的机器判据（`private_ca` 60 格重放、`tls.rs` 11 条单测中钉死本放宽的
`default mode trusts a hash-named SSL_CERT_DIR entry`、负控 10 条字面变异）随口径联动。改判分语义须
另走一次明确批准，不是本 TODO 的默认动作。

## 关联

- 差异账本：`specs/gateway-parity/diffs/A6.4-TLS-CA-SOURCES-001.md`（原文）、`-R1.md`、`-R2.md`（裁决与放宽登记）。
- 决策：`state/decisions-20261106-gateway-parity-r1.md`（#1640 选项 1 落地记录，含「代价」行）；
  `state/decisions-20261107-1865-option3.md`（#1865 选项 3 人批原文与决策卡）。
- 跟踪：issue #1865（本单，复核后关）；Refs #1640 · Refs #1552 · Refs #1647 · 关联 #1654。
