# session-domain-expectations 负控（#1177 / #1190）

绑定对象：`scripts/gateway-parity/session_domain.py` 的 `validate_expectations`
与 `scripts/gateway-parity/controls/session_domain.py` 的 `EXPECTATION_CASES`。
数据控制离线重放真实 A5.2a 数据，不启动全量对拍。

数量以 `negative_controls.py` 中读取的 `EXPECTATION_CASES` / `MUTATIONS`
为准，`python3 negative_controls.py --list` 打印当前清单与数量。
`EXPECTATION_CASES` 的定义仍只有控制面板一份；负控用 AST 读取，不执行面板。
`--list` 同时列出 `CALIBRATION_CASES` 与派生计数，只读、不创建证据目录、
不执行控制、不修改源码；但组合 `--check-anchors` 时先输出锚点报告
（如 `6 anchors: ok`）再输出 JSON，stdout 不再是纯 JSON，下游解析需先剥
锚点行；`--only` 非法或重复时返回 2 且不输出清单，锚点预检失败同样
返回 2 且不列清单（#1279）。`--check-anchors` 单独调用（不带 `--list`）
只做锚点预检：打印 `N anchors: ok` 后返回 0，不要求 `--output` / `--source`。

## 源码 require 负控

`MUTATIONS` 覆盖 canonical `require`：frames、durableStatus、registry 的
events / state / result 与 inapplicable 形状。`run_control.py` 按显式路径
加载面板并调用 `check_case`。

- baseline：被绑控制按期望规则 FAIL。
- mutant：先编译 Python 变异，再验证同一控制改报 `CONTROL_EXPECTATION` /
  `mutation accepted`，记 KILLED；仍按原规则 FAIL 记 SURVIVED。
- unaffected：不同 require 的控制仍按期望规则 FAIL；恢复后被绑控制复杀。
- 每行在 try 前初始化 `evidence`；异常记 INVALID，继续后继行，末尾汇总。

源码修改由共享 `mutating_sources` 守卫（common.py 与 parity 租约），恢复
核验后由共享 `publish_summary` 原子发布。输入不满足守卫时不算有效运行；
正式全量由协调者提交后在宿主跑。

## 绑定与回归

`check_bindings` 核表的键集、条目形状、非空值、重复行、登记规则与
derivation 控制覆盖；覆盖只来自 BOUND。`derived_counts` 从面板与变异表
推导计数，源码负控写入 `derived-counts.json`，文档不维护计数副本。

fix-r6 删除文档计数解析、扫描集检查、冻结目录读入与 `meta_controls.py`
入口，包括其副本和纯内存文档元负控。历史 R1 的描述由
[`A19-HOST-001-R2`](../../../../../specs/gateway-parity/diffs/A19-HOST-001-R2.md)
更正；旧冻结事件保留当轮记录。

`test_meta_controls.py` 沿用既有文件名，现仅验证绑定、只读清单与 evidence。
规则错绑分别覆盖 BOUND 和 UNAFFECTED；清单用实际表与扩展后的表验证。
只读测试用 Python audit hook 拒绝可写 open、文件修改及子进程启动。
`--negative` 先在内存编译函数变体，再运行被绑 unittest 完整名；只有专属
AssertionError 才 KILLED，无关异常或编译失败记 INVALID，逐项执行后汇总。

## 运行

```sh
python3 -I src/pilotdeck-gateway/tests/fixtures/session-domain-expectations/negative_controls.py --list
python3 -I src/pilotdeck-gateway/tests/fixtures/session-domain-expectations/negative_controls.py --check-anchors
python3 -I src/pilotdeck-gateway/tests/fixtures/session-domain-expectations/negative_controls.py --list --check-anchors
python3 -I src/pilotdeck-gateway/tests/fixtures/session-domain-expectations/test_meta_controls.py
python3 -I src/pilotdeck-gateway/tests/fixtures/session-domain-expectations/test_meta_controls.py --negative
# 提交后，源码 require 变异的正式宿主运行：
python3 -I src/pilotdeck-gateway/tests/fixtures/session-domain-expectations/negative_controls.py \
  --output <新证据目录> --source <真实 A5.2a 输出>
```

单独 `--check-anchors` 不带 `--list`、`--output`、`--source` 也能跑：先打印
`derived counts: …` 再打印 `N anchors: ok`，返回 0；组合调用（第 3 行）先跑
同一预检再列 JSON 清单，预检失败时返回 2 且不输出清单。

`--source` 包含 `raw/` 下的观测与 preregistration / manifest、launch。
原地源码负控要求 HEAD 与输入一致；不创建副本仓库或构建目录。
