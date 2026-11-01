# session-domain-expectations 负控（#1177 fix-r1，#1190 补齐 require 覆盖与计数推导）

绑定对象：`scripts/gateway-parity/session_domain.py` 的 `validate_expectations`
（A19-HOST 逐格期望的结构校验）与
`scripts/gateway-parity/controls/session_domain.py` 的 `EXPECTATION_CASES`
数据负控。无 cargo、无全量对拍：控制只离线重放记录在案的 A5.2a 数据。

## 文件

- `run_control.py` — 单控制运行器：按显式路径加载 controls 条目并调用
  `check_case`，输出与 controls CLI 相同形状的 verdict JSON。它不走 CLI
  `main()`：变异中的 `session_domain.py` 与 HEAD 必然不同，而 `main()` 先跑
  `verify_loaded_apparatus`；直接绑定 `main()` 自己调用的 `check_case` 是
  #209（session-domain-lock-release）已确立的形态。
- `negative_controls.py` — 源码变异元负控：`MUTATIONS` 各删 / 中和
  `validate_expectations` 里的 canonical `require`，清单以该表为准
  （frames、durableStatus、registry events / state / result、inapplicable
  registry；#1177 fix-r1 先行，#1190 补齐其余）。每行要求：
  baseline 未变异时被绑控制按期望规则 FAIL；mutant 下同一控制接受坏
  fixture（KILLED），仍按同规则 FAIL 记 SURVIVED；另有不相关控制在该
  mutant 下仍被杀（防串线）；恢复后复杀。绑定表（BOUND / UNAFFECTED）
  由 `check_bindings` 与面板的 `EXPECTATION_CASES` 机检：case 与规则都须
  相符，`exercised` 只由 BOUND 构造，每条 `BOUND` 非空且为
  (case, rule) 字符串对，`BOUND` / `UNAFFECTED` / `MUTATIONS` 三表键集
  完全一致，每条 `EXPECTATION_RUST_DERIVATION` 控制必须出现在某个 BOUND
  行里；任何缺失 / 多余 / 变形都是 `BINDING DRIFT`（exit 2），先于任何
  变异执行，主循环不会因此 IndexError（#1190 fix-r1）；变形的具体形状
  ——非序列 `BOUND` 容器、非 (case, rule) 对的条目、非
  (name, source, old, new) 的 `MUTATIONS` 行、重名变异行——同样以专属
  漂移消息拒绝而不是 traceback（#1190 fix-r2）。`evidence` 在进入
  守卫窗口前初始化（#1190）。计数不在叙述里钉死：运行时由 `ast` 解析
  （不执行）面板推导并写 `derived-counts.json` 入证据目录；登记文档
  （本 README、`specs/gateway-parity/diffs/A19-HOST-001-R1.md` 与
  `specs/gateway-parity/frozen/` 下全部 #1190 冻结事件，按目录通配推导，
  新增事件不漏扫）中的中文 / 阿拉伯数字计数短语（条 / 档 / 项 / 个 / 处）
  同样与推导计数机检，不一致或无法归类即 `COUNT DRIFT`（exit 2）
  （#1190 fix-r1，fix-r2 扩单位与扫描集）。扫描集本身不再只信通配
  （#1190 fix-r3）：`check_scan_set` 把已提交的 #1190 事件按文件名钉为
  下限，且 `frozen/` 下凡文件名含 `1190` 的文件必须落在扫描集内——已知
  事件被删 / 被改名脱出通配时，门禁以专属 `COUNT DRIFT` 拒绝而不是静默
  缩小扫描集后仍绿。`mutating_sources`（带
  common.py 与 parity 租约）守源码并逐字节还原，summary 原子发布、guard
  失败全 INVALID（#979/#1037）。
- `meta_controls.py` — 元负控的元负控（#1190 fix-r1，fix-r2 扩容，fix-r3
  修所有权）：原地变异 `negative_controls.py` 自身的绑定表与被扫描的
  README，要求上述漂移闸门各自拒绝——删除 `drop-registry-state-require`
  变异行、清空其 `BOUND` 条目、变形其条目 / 容器 / 变异行、整行重复、
  README 钉回与推导计数不符的数字（含个单位形式）、在 `frozen/` 里新建
  携带过期计数的冻结事件夹具，以及删除 / 改名已提交的 #1190 冻结事件
  （fix-r3 新增的文件系统行，事件文件进 `mutating_sources` 守卫集，行内
  从守卫快照还原字节）——都必须以 exit 2 加各自专属漂移消息失败；恢复后
  `--check-anchors` 闸门复绿。夹具行经独占创建（`open(..., 'x')`）取得
  所有权：预存文件只报 INVALID、原字节不动，`finally` 只删本轮真正创建
  的文件，守卫窗口外另有按字节核对的兜底清理，任何情况下不删非本轮创建
  的文件（fix-r3，r3 评审 P0/P2）。逐行记
  KILLED / SURVIVED / INVALID 后汇总，任一非 KILLED 即非零退出。

## 运行

```sh
python3 -I src/pilotdeck-gateway/tests/fixtures/session-domain-expectations/negative_controls.py \
  --check-anchors
python3 -I src/pilotdeck-gateway/tests/fixtures/session-domain-expectations/negative_controls.py \
  --output <新证据目录> --source <A5.2a 六格原始输出>
python3 -I src/pilotdeck-gateway/tests/fixtures/session-domain-expectations/meta_controls.py \
  --output <新证据目录>
python3 -I src/pilotdeck-gateway/tests/fixtures/session-domain-expectations/meta_controls.py \
  --output <新证据目录> --self-test
```

`--source` 需要真实 A5.2a 校准输出（`raw/` 下六格 JSON + preregistration /
manifest / launch）。`mutating_sources` 要求被守文件与 `src/` 输入区都等于
`--baseline`（默认 HEAD）：提交后由协调者在宿主跑；脏树预演用
`--baseline "$(git stash create)"`（先 `git add` 让索引一致），但不冒充
HEAD 准入。fixture 数据负控本身（`EXPECTATION_CASES` 全表，计数以运行时
推导为准）由官方 `controls/session_domain.py --case <name> --source …`
在提交后跑。
