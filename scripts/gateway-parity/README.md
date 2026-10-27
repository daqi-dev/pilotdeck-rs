# Gateway parity ruler（A2 / Issue #23）

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

`--suite deadline` 输出四格 `NOT_RUN`，结构自检 exit 0。实际原生执行、exec/egress
采集器接入和产品比较仍需 A7。`audit_native` 提供缺证据即拒绝的审计契约；
合成拓扑夹具仅证明规则能拒绝变异，不能证明原生宿主已运行。旧 A2 代理驱动不改称原生驱动。

`python3 -I scripts/gateway-parity/controls/session_domain.py --case all --output <新目录>`
执行 10 类必需负控及 7 项补充回归。加 `--source <校准输出>/six-cells` 可变异真实 TS
观察；不加时使用明确标记的合成观察，并把 7 项 calibration case 记入 `notRun`。
每条负控须 exit 1 且首因正确。**批次汇总取所有子报告的最悲观状态**（全序
`PASS < PASS_WITH_SKIPS < NOT_RUN < BLOCKED < FAIL`；未知状态直接判 `FAIL`）：
只有所有 case 与 cleanup 都 `PASS` 才是顶层 `PASS` / exit 0；cleanup 在非 Darwin
跳过 3 项正控得到 `PASS_WITH_SKIPS`，缺 `--source` 的 `notRun` 也计入
`PASS_WITH_SKIPS`，两者都必须 exit 1，绝不能汇总成 `PASS`（人定 A：装置顶层不得
比任一子核验乐观）。`main()` 只在顶层 `PASS` 时返回 0。逐条命令和输出落在
`controls/results.json` 及各子目录；`cleanupSkipped` 单列跳过的 cleanup 正控。
`check_cleanup.py --mode batch-aggregation` 钉住该全序、`run_all()`/`main()` 的
状态与退出码，以及 calibrate 对 `e1Controls=PASS_WITH_SKIPS` 的映射。

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
非 Darwin 平台没有等价的完整组枚举：Linux `/proc` 受挂载选项 `hidepid` 与
dumpable/权限过滤影响，`iterdir`/`getpgid` 可以**无错误地漏掉仍在本组的活后代**，
逐 pid 汇总的可见集合不能证明完整性；因此 `probe_process_group` 在非 darwin 上 leader
未回收后直接 `BLOCKED`，不读普查、不发信号（即使 `killpg0=ESRCH` 也不判 `empty`）。
这是证明边界：非 Darwin 的 cleanup 不会出现 `VERIFIED`/`PASS`，由宿主上的 macOS
calibrate 证明。`observe_pid` 保留 `alive`/`missing`/`blocked` 三态，只有 `missing`
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
非 Darwin 跳过的 Darwin-only 正控记 `passed:null`+`skipped`，cleanup 子报告为
`PASS_WITH_SKIPS`，整批只能到 `PASS_WITH_SKIPS` / exit 1，不计为通过。行为 fixture
`src/pilotdeck-gateway/tests/fixtures/session-domain-cleanup/` 的 `ignore` 孙进程先装好
`SIG_IGN` 并写就绪标记，launcher 有界等待后才退出；同一 fixture 在非 Darwin 把
`clean/leak/ignore/eperm-live-member/runtime-negative` 记 `passed:null`+`skipped`，
汇总同样是 `PASS_WITH_SKIPS`（exit 1），不把平台未证明的正控报成 FAIL 或 PASS。源码
变异负控按变异绑定期望字段，未声明的 `signalError`/`probeError` 判 INVALID
（环境/启动失败也一样）且全部跑完再汇总；`batch-status-relaxed` 只回退汇总逻辑，
必须让 `batch-aggregation` 控制转红。
A5.3 的白名单变更须由协调者完成双评审、哈希登记后再合并。
