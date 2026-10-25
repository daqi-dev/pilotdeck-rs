# 决策 · 2026-10-25 · 闸门结果块按补丁指纹沿用

> 单独成文件：同日多个 PR 往 `decisions-20261025.md` 末尾追加，在 merge queue 里反复冲突。

## 人定 · 闸门结果块按补丁指纹沿用（方案 A，2026-10-25 15:5x）
- **卡在哪**：merge queue 用 REBASE 合入，前一包合入后提交 SHA 改变；叠在它上面的后一包必须 rebase，head 随之改变。`check-pr-gate.py` 要求结果块 `head=` 等于 PR head，所以内容一字未改也要把四角色整套重跑（opus + GLM 各一次，约 10 分钟、8 次模型调用）。10-25 #586 第三、四包 `git range-diff` 全为 `=`，重跑没有新信息。
- **人定**：选 A（另一选项 B 是 queue 改 MERGE，会让 main 历史分叉，并改动 10-18 人批的设置）。
- **做法**：`run-review-roles.py` 在结果块里写 `patch=`，值是整个 PR `git diff base...head` 的 `git patch-id --verbatim`（保留空白；diff 参数写死，不受本机配置影响）。`check-pr-gate.py` 在 head 不等时，只有块内 `patch=` 与当前 PR 的补丁指纹（CI 用 `origin/<base>...<head>` 计算）完全相同才沿用。以下情况一律按旧提交拒收：改了任何一行（含只改缩进或空白、文件模式、新增空文件、删除、二进制）、3 行上下文内的周边代码被 main 改动、块内没有指纹、算不出指纹、指纹键重复。
- **能否撤回**：能，回退本 PR 即恢复只认 head 的规则。
- **动不动尺子 / 意向**：不动产品对拍尺子；只调整合并闸门的机检方式，方向与 Gerrit trivial rebase 相同。防的仍是「审完又推了新代码」，由机器判定内容是否变了。
- **原则**：评审结果绑定内容，不绑定提交编号；内容是否相同交给机器判定，不靠口头说明。断言：`scripts/test-review-roles.py` 的 `_carry_tests`（正控 1 条，负控 12 条）；对闸门的 5 个变异全部被杀（含把 `--verbatim` 换回会剥空白的 `--stable`）。
