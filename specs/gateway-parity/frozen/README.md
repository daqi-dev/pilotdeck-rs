# gateway-parity 冻结事件

#1085 人定 D / #1089：从本次迁移起，重登记只新增本目录文件；FROZEN.md 原叙述及表格历史说明原字节保留。这里记录登记，不改变比较器、阈值、分母、场景或定标输入。现有冻结批准程序仍须独立 issue、真实数据复算、两家非作者评审与人复核；机器通过不代表人签字。三套检查器按本单要求独立实现，规则改动须同步三套并跑三套登记单测；单测比较共享函数 AST，防止漏改。

## 生成清单

`python3 -I scripts/gateway-parity/check_frozen.py --write` 只重写现有连续哈希区块、按路径排序，从装置文件计算 SHA256；保留区块外每个字节及 parity 表格说明列。生成不批准事件，也不隐式创建事件。区块内纯哈希冲突可重新生成；区块外冲突或历史说明冲突拒绝。不要手合哈希。

Gateway 的执行源码继续核 HEAD；UI writer 在固定 HEAD 下验证执行代码、scanner 源码和每份被登记文件，核对与计算使用同一次读取的字节。因此先由协调者提交装置候选，再运行 --write 并把生成结果纳入最终提交；未提交 worker 留宿主命令，不绕过守卫。普通 UI 检查继续原 FROZEN 自举。三套 writer 都要求被登记文件字节等于固定 HEAD；parity 同时保留原入口隔离规则。

检查：`python3 -I scripts/gateway-parity/check_frozen.py --base origin/main`。无 --base 只核清单和事件格式；有 --base 另核 merge-base 历史、旧事件和每条哈希变化。历史 accepted-diffs / diffs 的 #1088 规则保持。`--write` 不与 --base、--print-ledger 或 --self-test 组合。

## 一事件一文件模板

文件名 `YYYYMMDD-ID.md`，例如 `20261030-1089.md`；同日同单多次登记可用 `20261030-1089-r2.md`。已有文件相对 merge-base 原字节、普通非可执行文件类型冻结，修改或撤销另开新事件，不原地覆盖。

```text
# 重登记理由和范围
Calibration-Evidence: _tmp/YYYYMMDD-issue-ID/raw/recalculation.json
Independent-Reviews: DeepSeek: _tmp/YYYYMMDD-issue-ID/review-deepseek.md; GLM: _tmp/YYYYMMDD-issue-ID/review-glm.md
Approved-By: 具名人或 AI 代批，待人复核（协调者）
Approval-Basis: 人批 issue / 决策及本次定标、两家评审路径
Change: <old-full-sha256-or-dash> <new-full-sha256-or-dash> scripts/gateway-parity/check_frozen.py
```

字段各一行且非空；Independent-Reviews 恰两条，以分号分隔，谱系和引用必须不同。该格式检查不核验评审文件内容，不能据此声称审批通过；待评审候选须正文标明。`Change:` 使用规范仓库相对路径和完整小写 SHA256，新增用旧值 `-`，删除用新值 `-`。每个实际内容变化（含新增/删除）须由相对 base 的新增事件精确连接旧→新；多事件连续变更可构成唯一链，旧事件、重复/分叉/断链/多余边不能冒充本次登记。parity 的旧短前缀扩为完整 SHA256 属表示变化，以 base blob 的完整摘要比较内容，无需伪造事件。

每次 `--write` 后运行普通检查和 --base 检查，核新事件确实覆盖当前文件；人工确认判分语义未动、证据和评审属于本次变更。
