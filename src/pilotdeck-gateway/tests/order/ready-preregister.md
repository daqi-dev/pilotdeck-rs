> 历史预登记：以下准备态只指登记当时；后续实际 HEAD / 产物 / 运行范围见本目录 [README.md](README.md)，实时接续仍以 `state/board.md` 为准。

# ReadyAdmission真实local结果（PREPARED_NOT_RUN）

唯一预登记：主仓 `_tmp/20261016-debt-cleanup/order-architecture/ready-admission-real-payload-20261016-162000/{task.md,matrix.json}`。上游设计：`../post-ts-acceptance-plan-20261016-161241/`。基线13ff8bfbcefb9f7414e809f3adfbc872d37f22a9；六格D1+/D1-/A+/A-/X1/X2先登记，后源码实现。运行需新exact Cargo artifact；旧145841/c990、153955/3ed与112000均不代表此HEAD。

测试专用显式callback envelopes；本地payload来自本次Rust真实dispatch/NativeFiles，host不得执行本地业务。list真实R2与supported即时类分别记录。只有四正常格完整帧一致、两实际路径反控具体拒绝后，才可报告CONTROLLED_ADMISSION_SUBDOMAIN_PROTOTYPE；当前全部未跑，默认产品、自然窗、P3全域、A5.4与真人实效仍开。

共用审计P1修复预登记：主仓 `_tmp/20261016-debt-cleanup/order-architecture/ready-admission-common-audit-20261016-163800/{task.md,matrix.json}`，先于本轮源码改动提交。旧“接收方拒”的准确边界为Rust独立后置因果审计，不是owner运行时能识别伪造R2的承诺；旧原件保留。六格都必须调用同一token级R2与seq/wire验收函数，反控分支只核实际hit，不构造最终拒绝。

## 2026-10-16 17:15 · stderr 正常收尾修复（PREPARED_NOT_RUN）

窄任务与判据见主仓 `_tmp/20261016-debt-cleanup/order-architecture/ready-capture-fix-1715/task.md`。正常 Ready 路径必须检查 stderr capture task / I/O / 字节，真实 exit0 + create_new 碰撞 guard 要求拒绝并保原文件；本轮只准备源码，不执行 child 或 Cargo。旧矩阵与所有历史证据不重写。

## 2026-10-16 17:20 · wire容量边界（PREPARED_NOT_RUN）

主仓 `_tmp/20261016-debt-cleanup/order-architecture/ready-wire-capacity-1720/task.md` 先登记：candidate 在创建duplex/写入前checked_add核总wire≤现有1MiB；溢出或超限明确UNMAPPED_WIRE_SIZE_LIMIT，不改排序/wire/矩阵与评分，不以超限为通过。源码准备，真实格大小尚未知。

## 2026-10-16 17:35 · Result JSON表达修复（PREPARED_NOT_RUN）

主仓 `_tmp/20261016-debt-cleanup/order-architecture/ready-result-json-1735/task.md` 先登记：R55实际构建E0277后，仅用显式match保留Ok/Err JSON分支，不改报告keys/拒绝选择/common gate，不加serde feature；新HEAD须新artifact，类型与原七格由后续实跑证明。

## 2026-10-16 17:50 · DNS离线门窄修（待实际控制与独审）

主仓 `_tmp/20261016-debt-cleanup/order-architecture/ready-dns-guard-1750/task.md` 预登记实际自有marker控制。旧64受控调用中62可达marker、2被阻，34 named出口只核身份；不涉及真实DNS，不推断历史外联。修复callback/promises及两Resolver原型上的查询族，保既有net/tls/dgram/fetch行为；同一control实跑新deny，再交GLM/DS，不改基线/评分/wire/cases。

## 2026-10-16 18:00 · native收尾复用finish（PREPARED_NOT_RUN）

R58已封后按主仓 `_tmp/20261016-debt-cleanup/order-architecture/native-finish-1800/task.md` 预登记，仅复用Process::finish的完整capture检查，保status/LIMIT/Ready/audit/wire/serde，不增case。R59新artifact另跑原窗，native caller未重跑与双非作者待审边界保留。

## 2026-10-16 18:05 · exact-capacity真实duplex护栏（PREPARED_NOT_RUN）

主仓 `_tmp/20261016-debt-cleanup/order-architecture/ready-capacity-guard-1805/task.md` 预登记：共享原WIRE_CAPACITY/LIMIT，真实duplex无reader时先写满恰capacity，再read_exact全payload比对。只增辅助guard，不改原七格；R59第三guard另登记运行。
