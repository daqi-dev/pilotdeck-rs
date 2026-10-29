# negative-controls — 负控规约：变异必须证明杀得死

> 负控不是「多跑几遍测试」，是**证明每条防线真的会拦**：把防线改坏，绑定的测试必须当场死。
> 这里立的是负控产出该长什么样的规矩；怎么跑、跑多久，见开工手册。

## 变异开关契约（#996 起）

文本替换式负控（改产品源文件 → 跑测试 → 还原）每条变异都要重编译，且锚点漂移要靠人盯。
从 #988 试点、#996 起批次迁移为**编译期开关**，契约如下。

### 产品侧：只许加开关分支

- 产品代码里的变异点只允许 `crate::mutants::on("<kebab-case-名>")` 分支形态：
  守卫禁用写 `if !crate::mutants::on("名") && <原条件>`，值翻转写
  `x = !crate::mutants::on("名")`，分支选择写 `if crate::mutants::on("名") { … } else { … }`。
  **开关关闭时逐点还原原语义**，不得顺手改写正常路径。
- `pdrs-mutants` feature 不进默认图；无 feature 时 `on()` 是 `const false`，所有分支折叠，
  普通构建 / 发布构建零行为变化（`cargo test` 无 feature 全量通过 + `--features pdrs-mutants`
  编译通过是迁移的前置证据）。
- `mutants::on` 的两道边界由负控看住（`edgeclaw-service-facade/switch_negative.py`）：
  空名选择器恒 false；非 test 构建开 feature 仍恒 false
  （integration `tests/mutants_boundary.rs` 链接的就是这种构建形态）。

### 执行器侧：预编译、独立进程、三相

- 执行器共享 `_shared/mutant_switch.py`（fixture 的 `schemata.py` 只声明
  `switches = [(名, 源文件, 绑定测试, 断言锚点)]` 表）：
  - **预编译一次** `cargo test --lib --features pdrs-mutants --no-run --message-format=json`
    定位 feature 库测试二进制；每个开关一个独立进程（`--exact`）以
    `PDRS_MUTANT=<名>` 选择，跑完即弃，**全程不改工作区源码**。
  - **三相齐全**：positive（无选择器，绑定测试必须过）→ negative（开选择器，必须以
    绑定断言锚点死掉）→ restored（无选择器再过一遍）；任何一相失败记 INVALID，
    后续开关照跑，最后统一 summary，存在非 KILLED 则退出非零。
  - KILLED 的证据是**具名测试 FAILED + panicked + 断言锚点**，编译失败、超时、错测试
    一律不算杀。
  - `PDRS_MUTANT` 是执行器的输出，不是输入：子进程环境先清理再注入；
    summary 用 `summary.json.tmp` + `rename` 原子发布，跑一半被杀也不会留下半份结论。
  - 工作树用 tree_lock 互斥，源文件哈希记入 `source.json`（记录的是工作树哈希，
    不是 HEAD 等价声明）。
  - **分母核对（#996 fix-r4 起）**：执行器跑完后把实际执行的开关名集合与
    `anchor_selfcheck.py` 同一提取函数（`switch_bindings`）得到的静态声明集合比对；
    不等（含显式选择超出声明集合）即整套 `status=INVALID`、列出差集、非零退出。
    这是 `main()` 等执行期改表（静态白名单与导入期对账都看不到的地方）的结构性闭合。
  - **分母核对覆盖全部执行入口（#996 fix-r5 起）**：声明源不是 `switches` 表的
    执行入口走同一共享 `mutant_switch.enforce_denominator`，声明集合由检查器对
    legacy 变异表的同一静态提取（`static_mutant_names`）得到，**不取运行期表**：
    edgeclaw-service-facade 的预编译套件（`schemata.py`，声明源为其
    `negative.py` 的 `MUTANTS`，显式选择即用户请求的子集）与文本替换边界负控
    （`switch_negative.py`，声明源即自身的 `MUTATIONS`）。
  - **空执行表也是漂移（#996 fix-r6 起）**：运行期把整表清空时没有可标记的行，
    而 `any()` 对空结果列表为假——退出码与 summary 都会假通过。分母核对在
    drift 且执行行为空时补一条套件级 `switch-denominator` INVALID 行（reason
    即差集），使每个入口的最终失败判定与 summary 都带上漂移原因，非零退出。

### 检查器威胁模型（2026-10-29 人定，#996）

`anchor_selfcheck.py` 的 switches 表审计承诺的是**防无意漂移**，不是对抗蓄意混淆：

- 承诺覆盖：负控作者改了表 / 锚点忘了同步；随手引入让执行表缩水的动态写法
  （别名、拼接键、遮蔽内置等，无论出现在模块顶层还是 `main()` 里——后者由
  执行器侧分母核对兜底）。审计子进程非零退出即 ERROR；唯一可接受记录须携带
  本轮随机 nonce（父进程生成、经 argv 传入、bootstrap 在执行被审模块前从
  `sys.argv` 移除，模块代码读不到）。
- **明确不在范围内**：蓄意混淆——伪造审计输出、`os._exit` 抢跑、运行期动态改名
  之类刻意构造的绕过。负控脚本每轮经非作者评审，这类蓄意形态在评审层拒收，
  检查器不再逐形态追堵（Python 动态性下不存在完备静态解）。
- 分母的真相源在**执行期**：见上文「分母核对」。

### 迁移契约：先对表，再删旧表

- 迁移一个 fixture 前，先在**同一 HEAD** 跑旧文本替换机制与新开关机制各一遍，
  两份 KILLED 集合**逐条一致**后才允许删旧表（旧 `negative.py` 整脚本删除，不留双轨）。
- 迁移后 `anchor_selfcheck.py` 以 `switches` 表计数：`::mutants::on("<名>")` 在目标源文件
  中必须**恰好出现 1 次**；声明了 marker 的脚本若定义 `switches` 表，按 table 信号管住。
- 旧机制因避让（在途分支改同一文件）暂时不能跑的，推迟批次，不换文件、不放宽断言。
