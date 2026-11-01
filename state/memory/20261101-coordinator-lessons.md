---
name: coordinator-lessons-1101
description: 11-01 一天的返工归纳：停进程、评审家族、修复轮与宿主、冻结件收货、rebase 后检查、宿主分诊；管协调动作，不管产品实现
type: feedback
---

**为什么**：11-01 协调者犯了 14 类错（清单 `_tmp/20261101-mistakes/LIST.md`），其中几条是规则早有、靠记忆没挡住；能机检的已做成守卫，其余写在这里。

**怎么用**：
- **停进程**：只按明确 pid 或树路径停；`pkill -P` 前断言 ppid > 1 且属本项目（11-01 误执行 `pkill -P 1`，用户 PilotDeck 重启、VPN 与 Safari 被关）。远端按 pid 杀时，命令里不要写含目标路径的字面量（会匹配到自己的 shell）。
- **谁写谁不审**：选第二家前看 `<base>..HEAD` 的 Co-Authored-By；协调者 review 入口已机检（`review-family-guard`）。直接跑 `run-review-roles.py` 不经过守卫，范围含协调者提交时那部分另由 GLM + DeepSeek 审。
- **修复轮与宿主**：派修复轮（head 将变）时同步撤掉该单旧 head 的宿主，否则占满车道（11-01 mbp2 9 个宿主 6 个作废）。
- **冻结件收货**：改冻结装置的交付必被 check_frozen 拒推——协调者删 `__pycache__`、`check_frozen.py --write`、只提交 FROZEN.md 再推；两个冻结单都改 FROZEN.md 时，后合入方 rebase 后重跑 `--write`。
- **rebase 后**：除冲突文件外，必跑全仓 `anchor_selfcheck.py --root . --anchors-only` 与 `cargo test --workspace --no-run`（别的 PR 新增的锚点可能指向本 PR 改过的源码，#1145）。
- **宿主分诊**：先列全表（每套 K / I / S、`FAIL` / `rc=[1-9]` 行、seq.log 停在哪步），不只 grep INVALID；macmini 进程启动偶发卡 12–35 s，超时类 INVALID 改在别的 macOS 机器单项复跑；Linux 上 calibrate 按设计 BLOCKED，复算放 macOS。
- **关单前收尾自查**：PR 写 `Closes #N` 自动关单也要先在 issue 上贴收尾自查（DoD 逐条证据、占位 grep、前置跟进单）。
- **（11-01 晚补）停进程要核实到 ps**：zsh `set -- $x` 不拆词，曾把「pid 单号」整串当一个 pid 传给 stop-tree，存活探测出错被当成已停、误报核实（4 个进程实际仍在）。stop-tree 已拒非纯数字 pid、用 `ps -p` 判存活并有回归测试；多参数循环一律写成 bash 脚本文件再跑。
- **（11-01 晚补）交付守望**：同一轮次目录重做时 exit 文件内容不变，按内容哈希的守望会判「未变」——#1139 交付闲置 8.5 h。守望 v10 键含 mtime，round-start 另扫「exit=0 晚于树 HEAD 且树脏」。READY 段每轮重写（round-start 已核 updated 时限）。
- **（11-01 晚补）给所有清单加行前想消费方**：负控清单生成器给每份清单加了全仓 test_safety 行，macmini 车道按清单文本判「占 28999 / 对拍锁」→ 全部链串行 15–78 min。改共享产物格式时 grep 所有读它的脚本。
- **（11-01 晚补）评审家族名以块内 model 为准**：`--runner anthropic` 10-25 至 11-01 默认是 claude-sonnet-5-5，正文写的「opus」不准；#1294 起默认 claude-opus-5-5。
