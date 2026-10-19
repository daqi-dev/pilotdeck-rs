---
name: grokbot-channel
description: Grok Bot 本机调用通道（Cursor 个人 key + scripts/grokbot-call.py）；官方无 CLI、gbot 0.12.4 不可用；未定为评审谱系。
type: feedback
---

# Grok Bot 本机调用（2026-10-19 实测）

**结论**：Grok Bot 桌面 App（0.71.2）没有官方 CLI；第三方 `grok-bot-cli`（`gbot` 0.12.4）在本机不可用——它要读 `~/Library/Application Support/Grok Bot/gateway-descriptor.json`，该文件只在桌面端成功连上 Agent Computer 后写入，本机缺失；换 `CURSOR_ACCESS_TOKEN` 仍报 `EnsureSandBox failed: 401`。可用的是 Cursor 个人 key 直连 `https://api.cursor.com/v0/grokbot/*`。

**怎么用**：
1. 首次授权：`npx -p @cursor/bdk bdk login` 走浏览器（本机 2026-10-19 已做），key 已写入 `.env.local` 的 `CURSOR_ACCESS_TOKEN`（gitignore 覆盖）。
2. 调用：`python3 scripts/grokbot-call.py --bot "Grok Bot" --text-file prompt.md --timeout 300`；`--json` 出结构化结果，stdout 即回复。
3. Bot 名要与 App 显示完全一致：`safe`、`重构代码`、`买手`、`问问`、`代码分析`、`Grok Bot`。名字打错会在 `POST /sessions` get-or-create 出新 Bot，脚本不拦。
4. 端点（宿主必须是 `api.cursor.com`；`api2.cursor.sh` 一律 404）：`POST /sessions {name}`、`POST /sessions/{id}/messages {text,messageId}`、`GET /sessions/{id}/entries?afterUpdatedSeq=N`（用 `turn.inFlight/queued` 判结束）、`POST /sessions/{id}/interrupt`。
5. 读历史：`python3 scripts/grokbot-call.py --bot "代码分析" --read [--since-seq N] [--json]`，按 seq 排序输出。`代码分析` bot 已经做过评审（例：A6.5-pr5 → `P0=0 P1=1 P2=3 P3=2`，在 seq 17）；这是现成的评审结果入口。

**边界**：`/v0/grokbot` 是 undocumented，随时可能变；key 等价账号权限，不要读取、打印或复制；消息经 Cursor 云端，评审材料外发前先过权限与密钥检查；回复靠轮询 transcript，`afterUpdatedSeq` 必须逐轮推进（或按 `updatedSeq` 去重），否则同一条回复会被读多遍——早期脚本就是没推进，误判成“Bot 回两条”，已修（第四次冒烟修复后单条）。**尚未定为第四家评审谱系**，启用要人定并登记 decisions。

**证据**：`_tmp/20261019-grokbot-probe/`（`notes.md`、`bdk-login.log`、`.resp-*`）；冒烟三次：`GBOT-OK`（6.4 s）、`GBOT-CALL-OK`、`GBOT-ENV-OK`。

## 加入评审谱系（2026-10-19 人批）

A6.5-pr5 并行测试中，Grok Bot 发现了 1 条 P1（DeepSeek P0=0 P1=0、GLM P0=0 P1=0、**Grok Bot P0=0 P1=1**），证明独特价值。人批加入评审谱系（decisions-20261019.md「卡 · Grok Bot 加入评审谱系」选 B）。

**用法**：
- 第三家非作者评审，或与 GLM 并行（提速）
- Bot 选「代码分析」（已测试有效）
- 读取：`python3 scripts/grokbot-call.py --bot "代码分析" --read --since-seq N`

关联 [[glm-scarcity-deepseek-shift]]、[[dsh-channel]]。
