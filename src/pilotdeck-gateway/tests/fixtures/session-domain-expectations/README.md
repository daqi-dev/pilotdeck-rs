# session-domain-expectations 负控（#1177 / #1190）

绑定对象：`scripts/gateway-parity/session_domain.py` 的 `validate_expectations`
与 `scripts/gateway-parity/controls/session_domain.py` 的 `EXPECTATION_CASES`。
防御性质量加固，不改 A19-HOST 期望或判分语义。数据控制离线重放真实
A5.2a 数据，不启动全量对拍。

## 源码 require 负控

`negative_controls.py` 的 `MUTATIONS` 覆盖 `validate_expectations`
里的 canonical `require`，清单以该表为准：frames、durableStatus、registry
的 events / state / result 与 inapplicable 形状。`run_control.py` 按显式路径
加载面板并调用 `check_case`，保持原有执行方式：

- baseline：被绑控制按期望规则 FAIL。
- mutant：先编译 Python 变异，再验证同一控制改报 `CONTROL_EXPECTATION` /
  `mutation accepted`，记 KILLED；仍按原规则 FAIL 记 SURVIVED。
- unaffected：不同 require 的控制仍按期望规则 FAIL；恢复后被绑控制复杀。
- 每行在 try 前初始化 `evidence`；异常记 INVALID，继续后继行，末尾汇总。

源码修改仍由共享 `mutating_sources` 守卫（common.py 与 parity 租约），恢复
核验后由共享 `publish_summary` 原子发布。新目录、未提交 src 输入等不满足
守卫时，不冒充正式运行；该部分由协调者提交后在宿主跑。

## 内存漂移闸门

`check_bindings(expectations, mutations, bound, unaffected)` 核表的键集、条目
形状、非空值、重复行、规则与 derivation 控制覆盖；覆盖只来自 BOUND。
`derived_counts` 从同一面板与变异表推导计数，源码负控记录到
`derived-counts.json`，文档不维护计数副本。

`doc_counts(documents, frozen)`、`check_docs(counts, documents, frozen)` 与
`check_scan_set(frozen)` 只接收内存映射。扫描通配 `SCAN_GLOBS` 仅在
`negative_controls.py` 定义；选择文档与反向覆盖共用它。被扫描内容为本
README、A19-HOST-001-R1 与通配命中的 #1190 事件；中文 / 阿拉伯数字计数
短语（条 / 档 / 项 / 个 / 处）必须与推导计数相符。`KNOWN_EVENTS` 钉已登记
事件下限；含 `1190` 却逃出通配的名称同样报漂移。

正式 CLI 只从真实 `specs/gateway-parity/frozen/` 读取。目录各级按 descriptor
以 `O_NOFOLLOW` 打开；子项先不跟链 stat，确认普通文件后只读打开并再次
核类型。所有链接（含悬空链接）、子目录和特殊文件直接 fail-closed，
`O_NONBLOCK` 防止竞态换成 FIFO 后挂住。不接受替代目录参数。

`meta_controls.py` 的全部情形只在内存中构造：删 / 空 / 变形 / 重复绑定表，
错误计数，新增事件、删除已知事件、改名逃扫与空扫描集。每行情形绑定
`meta_controls.check_scenario[情形名]` 及专属漂移消息；异常或无关诊断为
INVALID，全部行结束才汇总。`--self-test` 逐个把对应闸门替换为 always-pass，
必须在该情形的 `#1190 … expected drift:` 断言失败，恢复函数后再通过。
真实目录前后由同一只读适配器比较 bytes 映射；不相等则撤销全部 KILLED。

元负控不写任何文件，也不建临时目录；结果 JSON 只输出到 stdout，由调用方
重定向保存。旧的副本构造、文件归属记录、独占夹具创建、重命名、删除、
清理和文件快照实现均已移除。

`test_meta_controls.py` 验证纯函数无 I/O、真实目录不变、类型与 no-follow
拒绝、禁止替代目录、整表汇总、只读校验失败撤销结果、evidence 提前初始化。
`--negative` 只在内存编译函数变体，逐项运行被绑 unittest 完整名；仅该测试
在专属 AssertionError 上失败才 KILLED，编译失败和无关异常一律 INVALID。

## 运行

```sh
python3 -I src/pilotdeck-gateway/tests/fixtures/session-domain-expectations/negative_controls.py --check-anchors
python3 -I src/pilotdeck-gateway/tests/fixtures/session-domain-expectations/meta_controls.py
python3 -I src/pilotdeck-gateway/tests/fixtures/session-domain-expectations/meta_controls.py --self-test
python3 -I src/pilotdeck-gateway/tests/fixtures/session-domain-expectations/test_meta_controls.py
python3 -I src/pilotdeck-gateway/tests/fixtures/session-domain-expectations/test_meta_controls.py --negative
# 提交后，源码 require 变异的正式宿主运行：
python3 -I src/pilotdeck-gateway/tests/fixtures/session-domain-expectations/negative_controls.py \
  --output <新证据目录> --source <A5.2a 六格原始输出>
```

`--source` 包含 `raw/` 下的观测与 preregistration / manifest、launch。
原地源码负控要求 HEAD 与输入一致；不为绕过该检查创建副本仓库或构建目录。
历史登记描述的是当轮事实，fix-r5 的执行方式以本文件和代码为准。
