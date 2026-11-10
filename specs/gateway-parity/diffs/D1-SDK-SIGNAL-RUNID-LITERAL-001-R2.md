---
id: D1-SDK-SIGNAL-RUNID-LITERAL-001-R2
original: D1-SDK-SIGNAL-RUNID-LITERAL-001
---

## 类别

同原条目 `D1-SDK-SIGNAL-RUNID-LITERAL-001`（已批准差异 · 动态字段）。本更正只修
「机器判据」的租约前置引用与复算命令的存放位置（#2273 跟进），不改变差异类别、
判分语义与豁免范围。

## TS 依据

同原条目：固定基线 cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85、Node v22.22.0，
`sdk-signals/source.json` 真实 TS CLI 观测；金样 stdout 的固定 UUID 值（R1 已引）不变，
本更正不涉及 TS 侧任何事实。

## Rust 行为

同原条目与 R1：`run--unknown-cleanup` / `resume--unknown-cleanup` 两格因每轮新 UUID
与冻结金样不符而 FAIL，失败面均为 stdout 单面；无产品代码、比较器或金样改动。

## 范围

只更正「机器判据」的两处口径，其余（100 格分母、98 ok + 2 failed、只覆盖该两格
`recovery.runId` 一个字段、`acceptedDifferences` 不含本条）沿用原条目与 R1：

1. **租约前置（原 :69-71）**：原句把「锁形态与回收不自创」一并指向
   `scripts/cli-parity/README.md`「锁被保留时怎么解」节，而当时该节不含取锁形态。
   宿主按旧引用裸 `mkdir` 短持时，锁没有 `cleanupPending`，会被带接管逻辑的装置
   （`scripts/parity/round.py` 的 `parity_lock()`、`scripts/gateway-parity/common.py`
   的 `parity_lock()`、复用后者的 `scripts/native-witness/collect.py:41`（调用点
   `:262`））接管——owner 进程已死即 `dead_pid` **即刻**接管（与锁龄无关），或锁龄超
   2400 s（gateway 系 `MAX_AGE` / parity 系 `LOCK_MAX_AGE`）按 `expired` `rmtree`
   （活 owner 也一样；判据只看 pid 存活与锁龄，无心跳）；全批兜底
   上界按 `选择格数 × (2×12 + 5×3) + 30` 秒推导（100 格为 3930 s）远超 2400 s，
   接管后两批并行绑 28999 会使本条「失败恰为 2 格」判据假 FAIL。该节现已承载
   取锁/释放形态的唯一正本（宿主片段：`.guard` flock + mkdir +
   `owner.json{pid,timestamp,token,cleanupPending}`，发布 `cleanupPending: true`——
   宿主块无装置内硬截止闹钟，不适用短块豁免）；sdk-signals README 的 28999 串行
   前置段承载 28999 车道特有约束并指向该正本。
2. **字段级复算命令（原 :76-100）**：内联脚本与同节「本条不再内联命令副本，以免与
   正本漂移」自相矛盾；且 `re.search(...).group(1)` 无 None 守护（runId 缺字段时
   `AttributeError` 而非可读断言——不产生假 PASS，但复核信息不可读），`source.json`
   用相对路径读取、依赖 CWD=仓根（不符即 `FileNotFoundError`）。命令正本迁至
   `src/pilotdeck-cli/tests/fixtures/sdk-signals/README.md`「Rust 回放与产品负控」节的
   验收段（与原条目对验收命令块的正本+指针做法一致），并补 None 守护、evidence 根
   目录与 source.json / cases.json 显式传参、**100 格集合断言**（根下格目录恰为
   cases.json 去 6 个 stdin 缺口格——关掉部分批口子；不声称绑定当前 HEAD，批次绑定
   由本批 cargo `test result` 行与验收现场保证）与两格 `comparison.json` 的 stdout
   单面前置（显式 utf-8 读取）；本条与 R1 只留判据语义
   与指针。复算语义不变：
   `evidence/<格>/stdout.bin`（只剥精确 libtest driver 前缀，Rust 侧不归一化）的
   36 字节 runId 替换为金样 UUID 后须与金样 stdout 逐字节相等。

## 机器判据

验收命令与运行前置的正本仍是 `src/pilotdeck-cli/tests/fixtures/sdk-signals/README.md`
「Rust 回放与产品负控」节（必须恰好执行 100 个测试，0 个测试不构成验收）；28999 串行
前置与间接绑定者判定口径以该节前置段为正本，持锁/释放形态以
`scripts/cli-parity/README.md`「锁被保留时怎么解」节的宿主片段为正本（一律发布
`cleanupPending: true`）。字段级复算命令的正本同样迁至该节验收段（surface 判据
不单独构成放行依据）；身份与失败面判据（失败恰为 `run--unknown-cleanup` /
`resume--unknown-cleanup` 两格、`differing surfaces` 均仅 `["stdout"]`、其余 98 格
`ok`）沿用原条目与 R1。原条目正文不改，与其冲突处以本更正为准。

## 批准记录

未单独报批：本更正为 #2273 评审跟进的指针与前置口径修正（原条目 :69-71 租约引用
不完整、:76-100 内联命令与「不再内联命令副本」自相矛盾），不改变 #2165 人拍 2
已批豁免的范围、判分语义或金样；判据语义（失败恰为 2 格、stdout 单面、字段级复算
字节相等）不变，随本单经双谱系评审。AI 代批，待人复核。
