# Gateway parity ruler（A2 / Issue #23）

运行前设置 `export PYTHONDONTWRITEBYTECODE=1`，防止手动 Python 探针给装置目录
留下 `__pycache__`；该目录会被未登记文件守卫拒绝。注意 `python3 -I` 忽略
`PYTHON*` 环境变量，所以临时隔离探针必须显式使用 `python3 -I -B`，不能只靠
上述环境变量。仓内正式入口已在导入装置模块前禁写并隔离缓存读取路径。
禁写不会清除已有缓存；如果此前手动运行已产生缓存，先核实来源并将其移出装置目录，
再重跑检查。保留现有守卫对 `__pycache__` 的拒绝，不把它当作可忽略文件。

在仓根运行。Python 只用标准库；Node 22 + 基线自带 tsx。所有 Python 入口一律 `python3 -I`（隔离模式；各入口按显式文件路径 `importlib.util.spec_from_file_location` 加载装置模块并断言 `module.__file__` 正是该路径，不把装置目录加入 `sys.path`，其余 `PYTHONPATH`/用户站点/cwd 均不参与导入）。A2 入口的引导块最前面断言 `sys.flags.isolated`：不带 `-I` 会在加载任何装置模块前退出，并打印应使用的完整命令。基线目录按「`--baseline` > `PDRS_BASELINE` > 本 checkout 的 `resources/PilotDeck`」解析（worktree 里通常没有，需显式指定），缺失时 BLOCKED；必须已构建且只读。Rust bin 来自本 workspace 的 `src/target/debug/pilotdeck-gateway`。

```sh
CARGO_HOME="$PWD/_build/cargo-home" cargo build --manifest-path src/Cargo.toml -p pilotdeck-gateway --locked
# 或用装置包装（拒绝未跟踪/祖先 .cargo/config[.toml] 与持久 $CARGO_HOME config）：
python3 -I scripts/gateway-parity/build_proxy.py --check
python3 -I scripts/gateway-parity/build_proxy.py --target-dir _build/target
python3 -I scripts/gateway-parity/check_ruler.py
# 机器项全过时 exit 2：independent_review=null，待协调者审查。
python3 -I scripts/gateway-parity/check_frozen.py
# 无 FROZEN.md 时 exit 2：草稿没有冒充已冻结。
python3 -I scripts/gateway-parity/test_safety.py
# A4 前置离线对照：需本目录 npm ci（schema库）及只读基线，不监听/不请求服务。
python3 -I scripts/gateway-parity/controls/run_all.py --offline --output _tmp/20261014-a4-controls
```

Gateway 差异账本阅读正本为 `specs/gateway-parity/accepted-diffs.md` 加
`specs/gateway-parity/diffs/*.md`（文件名排序，排除 README）。旧文件冻结，
新增/更正只新增独立文件，模板见 `specs/gateway-parity/diffs/README.md`；
`check_frozen.py --base origin/main` 强制 merge-base 中已有文件只读，
`--print-ledger` 输出完整合并视图。其他阶段的同名账本仍沿用各自规则。

装置哈希的批量登记（发车制）见 `specs/gateway-parity/BATCH-RELEASE.md`：一次车次一次
`check_frozen.py --write`，每张单各自一个 `frozen/` 事件、各自两家非作者评审与真实复算切片；
`--write` 仍只由车长（协调者）在车次收尾执行。

运行 `check_ruler.py` 前须先在本目录执行 `npm ci --ignore-scripts --no-fund`，并提供已构建的只读 TS 基线；在线校准末尾也强制跑 A3 prerequisites，缺库/缺基线会 FAIL。

默认产物 `_tmp/20261014-exp006/`。重新采集请选择新的 `--output _tmp/YYYYMMDD-名字`；脚本不覆盖既有 cur 或 label。每轮 run.json 与最终 ruler.json 记录 `common.py:A2_SCRIPTS` 白名单内 A2 装置脚本 sha256（含 controls、运行器和检查器；不绑定 A3 schema 脚本）；本目录 `node_modules/` 存在时另记整树字节摘要。另记基线 `node_modules/tsx`、`node_modules/ws` 及二者声明的传递依赖的**实际文件字节**摘要（`baselineDepsSha256`；不用 package-lock integrity：integrity 不约束 Node 实际解析到的磁盘字节），归档复用与运行结尾都要求一致，E1 calibrate 的 `report.json` 同记该字段。本目录下除顶层 `node_modules/`（整树另记摘要）外，**每个文件都必须是已登记文件**：A2 白名单成员、A3 三件（check_schema.py/export_schema.mjs/validate_fixtures.mjs），或显式数据清单 `common.py:DATA_FILES`（README.md、package.json、package-lock.json、fixtures/gateway-protocol.fixtures.json、fixtures/session-calibration.json），**与后缀无关**：清单外的 `.txt`、无后缀、点文件、脚本、字节码、CPython 扩展模块（`importlib.machinery.EXTENSION_SUFFIXES`，本机 `.cpython-314-darwin.so`/`.abi3.so`/`.so`，另含 `.pyd`/`.dylib`）、Node 原生插件 `.node` 与 `.pth` 一律判未登记并 FAIL。**装置目录内任何 `__pycache__` 与 `.pyc/.pyo` 一律判未登记**：PEP 552 unchecked-hash 缓存（`py_compile ... invalidation_mode=UNCHECKED_HASH`）不校验源文件即被 `import` 执行，字节没有任何 FROZEN 行绑定，r4 的「源文件仍在即放行」前提已撤销；`__pycache__` 目录本身即报未登记，扫描不进入。所有 Python 入口在导入装置模块前设置 `sys.dont_write_bytecode = True`，并把 `sys.pycache_prefix` 指向新建临时目录（选代码内设置而非 `-B`/`PYTHONDONTWRITEBYTECODE`：入口会被 CI、README、`check_ruler` 子进程等不同方式调用，代码内设置不依赖调用点，且 `-I` 并不隐含 `-B`）；`dont_write_bytecode` 只禁写不禁读，不重定向读取时一个 unchecked-hash 缓存可在扫描前执行并覆盖 `unknown_apparatus_files` 让 `check_frozen` 假 PASS（实测见 `_tmp/20261025-a54-pkg2/fix-r5/` 的 `negative-cache-subvert*.log`）；实测正常跑入口后装置目录不产生 `__pycache__`。普通文件 `.DS_Store` 是唯一元数据豁免（与 `scripts/ui-server-parity/apparatus.py:55-70` 一致），链接、目录或其它非普通文件仍判未登记。路径按大小写不敏感比较（APFS 默认不敏感；大小写变体只有与登记文件同一 inode 时才接受，因此在大小写敏感卷上仍拒绝），大写后缀（如 `WEIRD.PY`）不因后缀表大小写而漏判。`node_modules` 只允许**顶层**那一个：任意更深层级（含大小写变体）的 `node_modules` 目录本身即判未登记。`node_modules` 之外出现任何符号链接（文件或目录，不论自身后缀）也一律判未登记并 FAIL——`os.walk` 不进入目录链接，链接目标里的文件会逃过扫描，而同名链接（如 `compare -> <外部目录>`）会先于 `compare.py` 被 import 解析从而遮蔽已登记模块；与 A2/A3 模块同名的普通目录同样判未登记。`node_modules` 内的符号链接沿用整树摘要规则：以 `link:<目标>` 记入、不跟随（改指向即改变哈希，目标字节不绑定，属已登记边界）。`os.walk` 传 `onerror` 并直接抛出：任何目录不可读即 FAIL（`check_frozen.py` 报 `cannot scan apparatus`），不再静默跳过该子树；`tree_digest` 同规则。新增未登记文件即 FAIL（`check_frozen.py` 使用同一扫描，遇未登记文件报错而非跳过）；拒绝形态的负控表与物化/清理唯一来源是 `common.py:APPARATUS_PROBES` + `materialize_probe`/`remove_probe`，`test_safety.py` 与 `controls/prerequisites.py` 共用；未知 kind 直接 `ValueError`。`check_ruler` 只复用脚本哈希与基线依赖哈希都完全匹配的六轮原件；无哈希的旧档案或版本不匹配均拒绝，要求新 output。复用时重新比 10 对 TS、代理，重新生成 controls 目录；原件损坏就 FAIL，不自动重跑盖掉失败。运行期间脚本或基线依赖改变也拒绝生成成功账本。每轮约 20 秒，整轮有 180 秒超时；独立 driver 上限 45 秒。等待共享锁默认 600 秒超时，可用 `--lock-timeout` 调整。全量 k=5 + 代理轮整体持有 `/tmp/pdrs-parity.lock`（owner.json、guard、陈旧接管、owner token 校验释放）。

**同目录同名顶替（fix-r6 P1）**：`from common import …` 会在扫描前走 FileFinder，同目录 `common/__init__.py` 包或 `common.cpython-<abi>-<platform>.so` 扩展模块先于 `common.py` 被选中，检查器本身可被替换。各入口的引导块因此一律用 `spec_from_file_location(name, path)` 按显式路径加载 `common.py` 及其他装置模块，并断言 `module.__file__` 正是该路径；`controls/` 入口只从装置根（父目录）取 `common.py`，不认 `controls/common.py`；`sys.path` 不参与装置导入。负控：包目录 / 空扩展模块存在时旧入口分别假 PASS / dlopen 错，修复后入口仍加载真 `common.py`，扫描把该包目录 / 扩展文件报未登记；证据见 `_tmp/20261025-a54-pkg2/fix-r6/`。**跳过不是通过（fix-r6 P2）**：`ProbeSkipped` 记 `passed: null` + `skipped: true`，报告单列 `skipped` / `skippedCount`；只容忍 `common.DEGENERATE_PROBES` 声明的宿主退化（APFS 大小写变体、euid 0 的 mode-000 不可读目录），出现未声明跳过即 `passed: false`；`check_ruler` 的 `prerequisite_controls` 仍取该 `passed`，并另列 `prerequisite_skips` / `prerequisite_skipped_count` / `prerequisite_unexpected_skips`。共享探针表增 `legacy.pyo` 与 `controls/__pycache__/…pyc` 两类，`expected` 按第一个 `__pycache__` 路径段截取；`--code-root` 的探针表与物化器恒取运行树（旧树有表时也不再取旧表），否则对照恰好不演示新盲区。

cargo 构建输入：`run_round --side rust`、`check_ruler` 与 `build_proxy.py` 在构建/开跑前检查本树未跟踪的 `.cargo/config[.toml]`、祖先目录的同类 config、以及 `$CARGO_HOME/config[.toml]`，命中即 `BLOCKED` 并列出路径。边界：不校验 `$CARGO_HOME/registry/src` 或 `.crate` 缓存字节，也不校验 `RUSTFLAGS`/`CARGO_ENCODED_RUSTFLAGS` 等环境；`build_proxy.py` 以 `--locked` 钉住 lockfile。威胁模型同 `specs/ui-server-parity/acceptance.md`：防意外漂移与仓库内改动，不防本机恶意者。

A4 前置另有离线 prerequisites：拒绝空/畸形 fixtures、逐18别名移除 pass/fail 覆盖（断言具体缺例消息）与开放 schema、本地 `$ref` 仅允许 `#` 或 `#/definitions/<名>`、时钟组合、独立输入哈希变动、导出器成功/不一致/配置写入失败三路临时目录清理。A3 输入单独记在 prerequisite 结果 `inputSha256`，不纳入 A2 apparatus；该清单由 `common.A3_SCRIPTS + common.DATA_FILES` 派生，`apparatus-excludes-*` 正控同步覆盖全部 `DATA_FILES`；审计文档起终使用与 check_ruler 相同的原始字节 digest；check_ruler 的 `prerequisite_controls` 和最终输入哈希检查都必须通过。

r2 对照要求第一条失败规则等于 expectedRule；每项 comparison.json 和 results.json 保存 primaryRule。除原有对照，新增 receive 头部重编码（126→127、短长度→126/127、FIN、RSV、mask 位及四字节 mask key）、纯 UUID/实体列表换序、业务同名时钟边界、磁盘/HTTP/帧头元数据两侧同坏。正式尺子另从真实 proxy 原件验证 upstream.jsonl 缺失、为空、截断都主命中 G2.observer。

手动两轮也必须在**同一个 output**下运行，cur 路径保持一致：

```sh
python3 -I scripts/gateway-parity/run_round.py --side ts --label direct --output _tmp/20261014-gateway-manual
python3 -I scripts/gateway-parity/run_round.py --side rust --label proxy --output _tmp/20261014-gateway-manual
python3 -I scripts/gateway-parity/compare.py _tmp/20261014-gateway-manual/direct _tmp/20261014-gateway-manual/proxy --out _tmp/20261014-gateway-manual/comparison.json
python3 -I scripts/gateway-parity/controls/run_all.py --source _tmp/20261014-gateway-manual/direct --output _tmp/20261014-gateway-manual/controls --proxy-source _tmp/20261014-gateway-manual/proxy
```

端口可用 `run_round --ports TS PROXY OBSERVER MOCK` 参数化，四个必须互异且都是 28xxx；缺省 28791/28792/28793/28794，全部只监听 127.0.0.1。预检占用立即拒绝，不按端口查杀。各进程 start_new_session，清理只向本次 Popen 的组发信号，父进程早退也清理其剩余后代；processes.json/audit 保存 PID、argv、实际监听地址和结束状态。

拓扑：直连 driver→TS；代理 driver→Rust→装置 observer→TS。mock 模型独立监听 28794。HTTP 流式探针是 `/__ruler/stream/{sse,ndjson}` 明确测试路由，由 watcher fixture 转发到 mock；它不宣称是生产 API。浅层 polling 替代仅因 macOS 沙箱 fs.watch EMFILE，对两侧一致开启；不覆盖递归热加载。

`driver.mts --url --token --scenario --out` 从基线公开 `src/gateway/index.js` 导入官方客户端/RemoteGateway/协议常量，tsx 将其解析到同目录 index.ts。完整应用帧在 driver，Rust 不能伪造比较证据。observer 独立记录原始上游 payload 与方向，A4 可用 `compare.py --local-method supported_agents` 要求成功响应一致、请求未转发；A2 默认断言十个只读请求**全部**透传，避免空 observer 被误认作本地化。

规约见 `specs/gateway-parity/`；没有已接受差异。自动 UI 恢复、桌面、生产 DOM、全部 101 method、config/扩展热加载与真正 TS 退役均不在 A2 完成声明中。

## A5.3 / E1 草稿入口

`session_domain.py --suite calibrate --output <新目录>` 持有既有串行锁，原样调用
`scripts/a5-19/run.py`。独立清单 `fixtures/session-calibration.json` 固定 72 项场景，
每项 5 次，复用原观察规则和负控，保留六格原件。这是 TS 进程内受控校准；
没有启动 sidecar 或 HTTP gateway，不声称覆盖原八组在线回归。

`--suite deadline` 无参时输出四格 `NOT_RUN`，结构自检通过仍 exit 3（未运行专用码）。加
`--native-capture <采集目录>`（#1140 A5.4，只允许 deadline）时，rust/rust 格按
`scripts/native-witness/collect.py --session-domain` 的真实采集逐 case 判分：先过
`load_native_capture` 的完整附件哈希（清单见 `native_evidence.py:NATIVE_RECEIPT_KEYS`）/合同/绑定审计（含 #1140 fix-r3 的 driver 同源一致性副本；#1206：只防协作写者，具体边界见 `scripts/native-witness/README.md`），再与 #1177 登记的期望比三面，并按 #1284 A 从期望帧机械派生必需模型请求；缺少请求记对应 case 的 `provider.requests` FAIL，不阻断可信证据认证、不缩分母。输出逐
case PASS/FAIL；其余三格仍 `NOT_RUN`，批不能 PASS（native 全 PASS → 批 `NOT_RUN`
exit 3；任一 FAIL → 批 `FAIL` exit 1）。采集目录的记录契约与离线端到端验证见
`scripts/native-witness/README.md` 与 `tests/fixtures/native-session-e2e/`。
`audit_native` 提供缺证据即拒绝的审计契约；合成拓扑夹具仅证明规则能拒绝变异，
不能证明原生宿主已运行。旧 A2 代理驱动不改称原生驱动。

`python3 -I scripts/gateway-parity/controls/session_domain.py --case all --output <新目录>`
执行 10 类必需负控及 7 项补充回归。加 `--source <校准输出>/six-cells` 可变异真实 TS
观察；不加时使用明确标记的合成观察，并把 7 项 calibration case 记入 `notRun`。
每条负控须 exit 1 且首因正确。**批次汇总只允许一个共享函数**
`common.aggregate_status(*children) -> (status, exit_code)`：全序
`PASS < PASS_WITH_SKIPS < NOT_RUN < BLOCKED < FAIL`（未知状态直接判 `FAIL`），
返回最悲观状态及其登记退出码（`PASS=0`、`PASS_WITH_SKIPS/FAIL=1`、`NOT_RUN=3`、
`BLOCKED=2`）。`controls/session_domain.py`、`session_domain.py` calibrate、
fixture 的 `check_cleanup.py` 与源码负控 `negative_controls.py` 的顶层状态/退出码
都调用它，不再各自写 `== 'PASS'` / `all(passed)` 分支。只有所有 case 与 cleanup
都 `PASS` 才是顶层 `PASS` / exit 0；cleanup 在非 Darwin/Linux 跳过 3 项正控得到
`PASS_WITH_SKIPS`，缺 `--source` 的 `notRun` 也计入 `PASS_WITH_SKIPS`，两者都
必须 exit 1，绝不能汇总成 `PASS`（人定 A：装置顶层不得比任一子核验乐观）。
`check_cleanup.py --mode batch-aggregation` 钉住该全序、`run_all()`/`main()` 的
状态与退出码、calibrate 对 `e1Controls=PASS_WITH_SKIPS` 的映射，以及源码负控
「恢复正控含平台跳过 → 批次非 PASS/非零」的最终汇总点。
`check_cleanup.py --mode aggregation-audit` 静态枚举装置内所有顶层状态/退出码
计算点：每个登记点必须调用共享函数，新增未登记点或登记点改用 `int(...)` 直接
算退出码即失败；`negative_controls.py` 的 `restored-skip-accepted` 与
`aggregation-exit-bypassed` 分别钉这两条。

报告区分实测成功与验收状态：冻结哈希不符时，即使测量成功，校准总判也为
`BLOCKED` / exit 2。进程树读取受阻在 `launch.json:psCensus` 明确记录为 `BLOCKED`，
不能写成“空树且全部清理完成”；清理结论只看 `processTreeCleanup`。`run_fixture`
用 `os.waitid(P_PID, pid, WEXITED|WNOWAIT)` 等 launcher 退出但不回收：leader 还是僵尸时
它的 pid/pgid 不会被复用，`os.killpg(pgid, 0)` 仍指向本次 `start_new_session` 的组，
探测与 SIGTERM/SIGKILL 都在回收之前完成，最后才 `wait()`。macOS 上组内只剩僵尸 leader 时
`killpg(pgid, 0)` 返回 `EPERM`；`EPERM` 只说明「没有本进程可发信号的成员」，不能排除
权限受限的活后代，所以**不再**用 `kill(leaderPid, 0)` 直接判空。macOS 改用
`libproc.proc_listpids(PROC_PGRP_ONLY, pgid, …)` 一次枚举该组当前成员：不再做「全局 pid
快照后逐个 `getpgid`」，成员在枚举与查询之间 fork 同组后代再退出而漏掉的窗口因此不存在。
libproc 返回的字节数等于缓冲容量即视为截断（XNU 按容量复制），实现分配余量、只接受严格
小于容量且 4 字节对齐的写，满缓冲继续增长重试，重试仍满返回「不可核验」。两次完整组快照
必须一致，成员在两次快照之间变化同样不可核验。枚举失败、截断、快照变化、leader 探测被拒
一律 `BLOCKED`、不发信号；只有两次完整一致且除未回收 leader 外无成员才记 `VERIFIED`。
Linux `/proc` 仍不是完整组枚举，受 hidepid/dumpable/权限过滤的可见集合只能提供正向残留发现。Linux 分支持有未回收 leader 后才可发真实组信号，reap 后只作 signal-0；只有内核 `killpg0=ESRCH` 允许 VERIFIED。EPERM/未知仍 BLOCKED，残留发现即使清走仍 GROUP_NOT_EMPTY；其它无已登记证明的平台仍 BLOCKED。`observe_pid` 保留 `alive`/`missing`/`blocked` 三态，只有 `missing`
才可当已消失；`blocked`（EPERM/EACCES）只说明本进程无权探测，必须记未核验/失败，
不得当已清理。组内仍有存活成员记 `GROUP_NOT_EMPTY` 并只向该组 SIGTERM（5 s 有界等待
后仍不空则只对该组 SIGKILL）；leader 已被他人回收或平台无法证明归属时记 `NOT_VERIFIED`
且不发信号。所有实际信号（生产主路径、辅助清理、Node 探针、controls）都走同一个持有检查
入口：Python 是 `HeldProcessGroup`，只有 `waitid(WNOWAIT)` 仍证明 leader 为 `RUNNING` 或
`EXITED`（未回收僵尸）时才发送，`REAPED`/`UNSUPPORTED` 一律拒绝；Node 在 spawn 后、任何
await 前注册 `exit` 监听，`signalHeldGroup` / `signalHeldChild` 在 `exit` 事件后拒绝，
`term-early` 钉住「leader 在等待期间退出则 `signalRefused`」。signal-0 只作观察，不投递。
本证明的对象只是 launcher 进程组，且只到**最后一次完整组快照的观察时刻**：两次快照一致
证明这两个时刻组内没有未计入的活成员；之后新 fork 的后代不在该时刻的证明内（这是观察
边界，不是把不完整普查当成空）。后代自行 `setsid` / `detached` 逃出该组后也不在
`VERIFIED` 范围内，登记为 P2 边界跟进，不把 `VERIFIED` 表述为「所有后代均无残留」；
本轮没有用任何树遍历冒称覆盖这一逃逸。只有 `processTreeCleanup=VERIFIED` 才允许顶层
`status` / `executionStatus` 为 `PASS`；`GROUP_NOT_EMPTY` / `BLOCKED` / `NOT_VERIFIED`
一律为 `PASS_WITH_CLEANUP_UNVERIFIED`，`freezeStatus` 为
`BLOCKED_CLEANUP_UNVERIFIED`（只认 PASS 的冻结 / 矩阵消费者因此不会把它当通过），
且 `session_domain.py --suite calibrate` 返回 exit 2（与 `BLOCKED` 同约定），只看退出码的
CI/shell 调用方也不会把它当通过。
`controls/session_domain.py` 同批增加 `cleanup-empty-group`、
`cleanup-leftover-grandchild`、`cleanup-probe-blocked`、`cleanup-eperm-live-member`、
`cleanup-identity-lost`、`cleanup-census-snapshot-race`、`cleanup-census-descendant`、
`cleanup-census-unstable`、`cleanup-census-truncated`、`cleanup-census-enumeration-failure`、
`cleanup-pid-blocked`、`cleanup-linux-filtered-proc`、`cleanup-signal-entry-reaped`、
`cleanup-signal-entry-held` 等行为控制，cleanup 结果计入
`results.json:cleanup`。cleanup 子报告 `FAIL` 或任一 `case` 失败 → 整批 `FAIL`；
非 Darwin/Linux 跳过的正控记 `passed:null`+`skipped`，cleanup 子报告为
`PASS_WITH_SKIPS`，整批只能到 `PASS_WITH_SKIPS` / exit 1，不计为通过。行为 fixture
`src/pilotdeck-gateway/tests/fixtures/session-domain-cleanup/` 的 `ignore` 孙进程先装好
`SIG_IGN` 并写就绪标记，launcher 有界等待后才退出；同一 fixture 在非 Darwin/Linux 把
`clean/leak/ignore/eperm-live-member/runtime-negative` 记 `passed:null`+`skipped`，
汇总同样是 `PASS_WITH_SKIPS`（exit 1），不把平台未证明的正控报成 FAIL 或 PASS。源码
变异负控按变异绑定期望字段，未声明的 `signalError`/`probeError` 判 INVALID
（环境/启动失败也一样）且全部跑完再汇总；`batch-status-relaxed` 只回退汇总逻辑，
必须让 `batch-aggregation` 控制转红。源码负控自身的顶层状态/退出码也走
`common.aggregate_status()`：KILLED→`PASS`、SURVIVED→`FAIL`、INVALID→`BLOCKED`，
恢复正控作为同一批的子项原样计入；因此恢复正控为 `PASS_WITH_SKIPS` 时批次为
`PASS_WITH_SKIPS`/exit 1，不会打印 `PASS`/exit 0（`restored-skip-accepted` 负控
只删掉恢复正控这个子项，必须让 `batch-aggregation` 的 `negativeRestored.skip*`
转红）。`aggregation-exit-bypassed` 删掉 controls `main()` 的共享退出码调用，
必须让 `aggregation-audit` 报出登记点未走共享函数的专属失败。
A5.3 的白名单变更须由协调者完成双评审、哈希登记后再合并。

#209 A2 锁加固：锁获取时先发布 `cleanupPending: true`，成功退出才删除锁；因此 SIGKILL/OOM 或后续清理记录写入失败也不留下可自动接管的空标记。启动到登记、整批清理和进入释放 guard 之前挂起 SIGINT/SIGTERM，子进程 exec 前解除继承掩码。只有本次登记的进程组全部有界清理并证实消失，才释放本 owner 的共享锁；kill/wait/probe 失败保留登记并把 `cleanupPending` 更新成组列表，gateway 不自动接管该记录，须协调者核实后回收。每组等待上限为 TERM 3s + KILL 3s + reap 3s + 最终探测 2s，整批串行；SIGKILL 不能被屏蔽，只有遗留锁标记，不承诺当场清理。进程登记保护覆盖 A2 run_round/check_ruler；#209 E1 起 `session_domain.run_fixture` 也把本次 `start_new_session` 的 pgid 记入 `common.LIVE_GROUPS`，spawn+登记与 finally 清理都在 `deferred_signals()` 内，只有自身 `probe_process_group` 返回 `VERIFIED`、leader 已回收且无 `cleanupBlocked` 才移除登记；未证实则锁 finally 写 `owner.json.cleanupPending` 并保留租约，规则与 A1/cli/ui 姊妹装置相同。E1 的行为正控与源码负控见 `src/pilotdeck-gateway/tests/fixtures/session-domain-lock-release/`。锁协议保留既有 `.guard` 的 flock + owner 目录，未单边改成与姊妹装置不兼容的锁。

#909：CI 的 Node 前置条件由实际运行的 `test_metadata_io_driver.EnvironmentControls` 逐 job 检查；冻结 prerequisites 不再用跨 job 的首个 setup/npm 文本索引冒充证明。四个 Node job 各有删除、后移和条件 setup 的配对负控。

预置标记的可用性代价：即使获取锁后尚未 spawn，只要未正常退出，gateway 也不按 dead_pid/expired 自动回收；MAX_AGE 不覆盖这个保护。等待方有锁等待超时，不能据超时判断遗留组已消失。人工回收先保留 owner.json 与对应轮次原始日志，核实原 owner 已停止、所有本轮进程组均已停止且没有其他装置持锁，再由协调者处理该锁；证据不足则保留，继续独立工作。


## 交付前冻结预检（`scripts/check-frozen-delivery.py`）

冻结改动交给协调者执行 `check_frozen.py --write` **之前**，先在交付树跑一次：

```sh
python3 -I scripts/check-frozen-delivery.py
python3 -I scripts/check-frozen-delivery.py --root _build/wt-<N>   # 审计另一棵树
```

交付判据只有一条：**只剩 hash list differs 这类 `--write` 能消除的项，才算可交付**
（`BLOCKED 0 / READY <n>`，exit 0）。`check_frozen.py --base origin/main` 在
`--write` 之前必然失败——旧 pin 与新字节不符——所以「有失败」本身不说明缺件，
必须按失败的种类分流。预检自动发现三套装置（`parity` / `gateway-parity` /
`ui-server-parity`），逐套跑 `--base origin/main` 并分类：

- 可接受（`--write` 会消除）：`hash list differs from generated inventory`；
  旧 pin 与实际字节不符（gateway 的裸清单行、parity 的 `<name>: hash mismatch`、
  ui-server-parity 的 `invalid path/hash: <name>`）；首次登记的新文件
  （`A2 apparatus missing FROZEN hash` 与新文件的 `lacks exact new event chain`）
  在新增事件文件里有 `Change: - <当前字节 sha256> <路径>` 行时。
- 首次登记不总是写在失败列表里：parity 只说 hash list differs 并把跳过的文件放进
  `unregistered_skipped`，gateway-parity 只说生成的清单不同。预检因此另扫交付分支上
  各装置 `--write` **真会登记**的新增文件：parity 是 `scripts/parity/` 与
  `specs/parity/` 下被它自己 `manifest_inventory` 的 `names` 元组登记的文件
  （名单只取装置源，预检不留副本；读不出即不可判定），以及名单里带
  `scripts/` / `specs/` 前缀、落在两目录之外、`--write` 按原样登记的路径
  （扫描范围必须与「是否可登记」的判定一致，否则首登记漏检）；gateway-parity 是装置
  `manifest_inventory` 的成员
  （`A2_SCRIPTS` + 两个 session fixtures + 五个 a5-19 文件；`A3_SCRIPTS` 与
  `DATA_FILES` 只在未登记文件白名单里、**没有**哈希行，给它们补链反而是死路；
  成员落在 `scripts/gateway-parity/` 之外时 `--write` 照原样登记，预检同样按清单
  原样扫，不按目录前缀筛）。
  ui-server-parity 同理：`SPEC_FILES` 与 `TOOLCHAIN_FILES` 的成员即使不在
  被扫描的装置目录下也在扫描范围内。
  这些同样要求精确的 `Change: - <sha256> <路径>` 链——否则记为阻塞：`--write`
  登记后 `check_frozen.py --base` 必然拒绝。装置不会登记的新文件（未跟踪草稿、
  清单外文件）一律忽略：真实闸门同样无感，给它补事件行反而会变成未使用的
  event 行、被 `check_frozen` 判失败。

  「首登记」按**清单行**判定，不按 git 的 `A` 状态：分支把 merge-base 时就存在、
  但当时没有清单行的文件加进装置清单（改装置源码 / 改名进装置目录）时，`--write`
  照样加行，而 diff 里它既不是 `A` 也可能记成 `R`。链的起点因此是清单行：
  merge-base 无该行即从 `-` 起算（与 `check_frozen` 的 `old_hashes.get(name, '-')`
  一致），有该行才从旧字节起算。终点同理取**新清单**：成员被移出清单、文件仍跟踪
  （或文件被删）时 `--write` 删行，目标一律是 `-`，只认 `Change: <old> -`；
  只写 `Change: <old> <new>` 的交付判阻塞——真实闸门在 `--write` 后同样拒绝。
  **掉行不靠装置报文**：pre-write 时装置的 `new_hashes` 仍取还在的旧行，成员被
  移出清单只会报可消的 `hash list differs`；预检自己枚举「merge-base 有行、
  HEAD 已不可登记」的路径逐条查链（与首登记扫描对称）。ui-server-parity 的清单
  同样从装置源解析（`apparatus.py:SPEC_FILES` + 两个装置目录的跟踪文件）；
  清单源读不出或形态认不出时按「不可判定」阻塞，不折算成 `-`。
- 阻塞：其余全部。尤其「改了冻结字节却没有对应事件行」的
  `hash change lacks exact new event chain`、未登记文件、事件文件缺 `Change:`
  行或四字段（`Calibration-Evidence` / `Independent-Reviews` / `Approved-By` /
  `Approval-Basis`）、报告无法解析或无法读取；认不出的失败一律按阻塞
  （fail-closed，不把未知消息当通过）。
- 证据只取 **HEAD**：没提交的事件文件、工作树里的未提交字节都不能让分支显得可交付
  （未提交的事件文件直接报阻塞）。工作树有未提交的**已跟踪**改动同样判阻塞
  （`=== worktree === BLOCKED`）：子装置哈希的是工作树字节，脏树能让 HEAD 上的改动
  「隐身」，CI 在 HEAD 上跑仍会 FAIL；未跟踪文件子装置看不见，只作提示。
  运行开始与结束各解析一次 HEAD，两者不同即报 `snapshot` 阻塞：子装置自己也读
  HEAD，只钉住本进程的引用挡不住跨提交拼链。

`Independent-Reviews: Pending: ...` 占位**不阻塞**，但输出单列
`PENDING-REVIEWS`：协调者可以先 `--write`，最终登记仍须回填两家非作者的真实评审。

脚本只读，绝不执行 `--write`、不改任何文件；最后一行 `BLOCKED n / READY m`
按装置段落计数，任一阻塞即 exit 1。今天的 `#1355 fix-r2` 缺事件文件、
协调者拒绝 `--write` 就是这一类：预检会在交付前直接指出缺哪一个事件文件/哪一条链。

## 已知限制：没有与 `PDRS_REQUIRE_PERMISSION_FAULTS` 等价的强制开关（#1900）

Rust 侧的 `src/pilotdeck-gateway/tests/common/permission_faults.rs` 有
`PDRS_REQUIRE_PERMISSION_FAULTS`：宿主特权导致权限拒绝的探针跑不出来时，用例从
「跳过」变成 panic。本装置**没有**对应开关。

- 现状：`common.materialize_probe` 的 `unreadable-dir` 分支在 `os.geteuid() == 0`
  时抛 `ProbeSkipped('root can read mode-000 directories')`；`test_safety.py` 与
  `controls/prerequisites.py` 的共享探针循环把它记成 `passed: null` + `skipped: true`，
  并只容忍 `common.DEGENERATE_PROBES` 声明的两类宿主退化。批状态取最悲观子项：
  `PASS_WITH_SKIPS`，`BATCH_EXIT_CODES['PASS_WITH_SKIPS'] = 1` —— 非零退出，且报告
  单列 skipped，不是假 PASS。
- 缺口：root / `CAP_DAC_OVERRIDE` 宿主上权限面完全没验，形式宿主拿到的仍是
  `PASS_WITH_SKIPS` 而不是失败；也没有「本装置必须真跑到、退化即红」的开关可供
  形式宿主导出。
- 今天靠什么覆盖：形式宿主与本仓 CI 都以非特权用户运行（实测 `id -u`：本机 502、
  cb8 1000、mbp2 501、macmini-01 501），`os.geteuid() == 0` 不成立，`unreadable-dir`
  探针每次都真物化、真被拒。只要形式宿主保持非 root，权限面就是被对拍的；剩下的
  风险只有「有人用 root 跑正式对拍」，此时报告仍写 `PASS_WITH_SKIPS` / exit 1 并
  列出跳过项。
- 为什么本单不改：`common.py` 是带哈希行的冻结装置源（`specs/gateway-parity/FROZEN.md`
  里 `../../scripts/gateway-parity/common.py` 那一行），按 CLAUDE.md「冻结尺子的变更」
  需要独立 issue、理由、两家评审、真实数据复算（A1 二进制）与重新登记；新增一个
  失败语义还属于改判分语义，需要明确授权。本单按 #1900「怎么算做完」第 5 条走
  「不修 + 写进已知限制」这条路，另开跟进单 #1930 走冻结变更流程。
- 同族缺口（#1900 sibling 评审发现，一并记在这里）：`check_schema.py` 的
  `PDRS_REQUIRE_BASELINE` 解析只认 `("1", "true", "yes")`，不认 `on`，也不对
  未识别值 panic —— 与 Rust 侧 `permission_faults.rs` 刚收紧的口径不一致，属同一种
  fail-open。该文件同样有冻结登记行，CI 钉死 `=1`（`.github/workflows/ci.yml`）所以
  现网不踩；口径统一并入 #1930。
- 同族缺口之二（#1900 sibling 评审补充）：本仓第三个开关
  `PDRS_ALLOW_NO_RG`（`src/pilotdeck-gateway/tests/grep_tool.rs`）同样不 trim、
  不认 `off`/`no`、未识别值不拒绝（`=off` 会读成 ON）。它只影响「是否允许跳过
  ripgrep 检查」，误读方向是放行跳过而不是解除强制闸门，CI 从不设置它，真跳过时
  也有 loud skip 行，所以本单只登记不改；与上面两个开关一起并入 #1930 统一口径。


### #2916 Linux cleanup proof

Linux uses `/proc` and pidfd only to record positive same-group member findings. It never calls that visible set complete. The held leader pins the group before any real signal; after reaping it, only kernel `killpg(pgid, 0)=ESRCH` proves the group absent. EPERM/unknown remain BLOCKED, identity loss without a prior finding remains NOT_VERIFIED; an already recorded positive finding remains GROUP_NOT_EMPTY with signalRefused if a real signal is refused. Remediation never erases that finding. Darwin retains its independent complete-census path. Other platforms remain explicitly unsupported. `check_cleanup.py --mode all` runs the eleven `check_linux.py` controls (the original eight plus both helper identity gates and a refused-signal finding control) on Linux; visible-set/EPERM mutations target the new helper and must be KILLED. The registered proof concerns this process group, not descendants that escaped it with setsid.

#2916 strict standalone kernel CLI preloads the helper and verifies its HEAD bytes before controls. The explicit in-process fixture mode proves the registered direct parent owns both the live parity record and source flock before loading the helper; there is no allow-mutated flag or fake HEAD. Executing common.py bytes are now included in the result. Kernel control cancellation terminates pinned member identities and held unreaped groups before reaping, and the CLI supervisor sends TERM with bounded cleanup grace while retaining timeout output. The registered mutation inventory rejects an empty or changed eligible denominator; the restored-outcome replay selects a named cross-platform row and has an empty-eligible injection.
