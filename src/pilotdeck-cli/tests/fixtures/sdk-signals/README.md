# #1145 P08：信号矩阵与 Rust 生命周期回放

本包包含 TS 原件、来源验证和 100 格独立的 Rust CLI 进程回放。Rust 实现位于
`src/sdk/{signals,turn,transport,output}.rs`。**仍不表示 P08 / #123 完成**：
stdin 六格挂起，动态 UUID 仍按字面比较；
#1151 已批准严格的业务行后 Node 尾段归一化及四格 4003 close reason 差异。
#1164 选 B 已批准八格未处理 rejection 的 stderr 差异，精确范围见
`specs/gateway-parity/diffs/D1-SDK-SIGNAL-UNHANDLED-REJECTION-001.md`。产品变异负控独立于数据负控。
上述第 5 行的字面 UUID 比较差异（`{run,resume}--unknown-cleanup` 两格的
`recovery.runId`）已登记为 `specs/gateway-parity/diffs/D1-SDK-SIGNAL-RUNID-LITERAL-001.md`
（#2165 人拍 2，登记正文见本文件下文）。

## 矩阵与原件

`cases.json` 是 106 格的预注册输入：run/resume，各自覆盖两种信号、注册前的
control 前驱、stdin、query hello/resume/model 前驱的保持/释放/拒绝、
abort 成功/拒绝/保持、同种与交叉重复信号、终态消费后取消、abort 与终态交错、
正常/异常收尾；#1178 另加 `action: rst` 轴——server 对已发出的 abort_turn 直接
RST query socket（不回复），覆盖 abort 在途时对端重启/断链。每格 12 秒观察
上界；未退出只是观察事实，不能推断永不退出。
TS hello/RPC 的默认超时也不同于宿主观察上限。

`source.json` 保存 100 格真实 TS CLI 的真实 WebSocket 观测，另有 6 格 stdin
缺口。原样 stdout/stderr、wire、客户端确认、退出与清理、初末文件树均保留；
Node 版本固定 v22.22.0，TS 基线固定 cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85，
实际加载的 source/dist 与探针哈希随采集保存。`oracle.json` 只作确定性提取，
不剥警告、不归一化 UUID、不把服务端发送当成客户端消费。

stdin 缺口的原件单独保留在 `gap-source.json`：本宿主的 Node pipe 与阻塞 FIFO
都未稳定建立等待 EOF，真实 `/dev/stdin` 读取出现 EAGAIN。readFile 调用钩子
不能证明读成功；某次信号先于错误到达也不算覆盖。该采集线已挂起，前置于
#123 / P09。旧观测用当时的 probe 哈希标识，只用于说明缺口；不作为有效消费格。

#1178 的 RST 轴按 #1260 单格刷新同一协议增量入库：只重采四格 abort-rst，
旧 96 格原件字节不动；唯一的数据面变更是为旧格补上 `milestones.rstApplied`
这一由 trace 重算恒真（`action !== 'rst'`）的推导字段，合并脚本先逐格断言
`input.action !== 'rst'` 再回填。probe 的 `server-rst` 用
`socket._socket.resetAndDestroy()` 发真实 TCP RST，Rust 侧 mock 用
SO_LINGER 0 关闭等效实现，两侧 close 都记 1006。四格实测：自然退出码 1、
closes [(2,1006),(1,1005)]（query 先于 control，顺序因果稳定）、stdout 空、
stderr 为业务行 `Gateway WebSocket closed.` + 完整 Node 尾段，按既有
#1151/#1164 归一化后与 Rust 单行业务行相等，无需新差异登记。

## 客户端确认与资源边界

- `hooks.mjs` 预加载后仍调用原 TS CLI dist、SDK 和 Node WebSocket。它透明代理
  once/removeListener、readFile 调用、stdout 写入和 query.abort 入口，返回原结果，
  不补 signal listener、不 catch abort 拒绝、不增加业务等待。额外未启动 query
  只用来取得真实 prototype，不建连接。完整 `result/error` JSONL 行来自
  consumeRun 已消费事件之后；保持 final=false 才形成稳定信号窗口。
- 服务端 mock 只 bind `127.0.0.1:28999`。WebSocket 入口只允许这一 URL，并实际
  负控验证 18789 被同步拒绝。这是范围受限的函数入口 guard，**不是 D0 OS 网络沙箱**。
- `capture.py` 复用 cli-parity 的 `.guard + mkdir` 租约及 owned process group
  清理，不回收前驱锁。父进程中断通过 finally 清理；原子发布接管 PID 与 readiness。
  SIGINT 被后台 shell 忽略的问题按 sigint-default.py 的原则复位。
- 各等待分别有上界：listen/退出后的 pipe drain/超时后的 kill 回收/socket close/
  server close 为 3 秒，每格触发与观察为 12 秒，全批兜底按
  `选择格数 × (2×12 + 5×3) + 30` 秒保守推导。原始 trace 中的
  `cleanup` 明确区分真实退出和宿主清理；逐连接记录显式 close 调用与服务端 close。
- 重复信号必须见到首个 abort RPC；query 前驱释放/拒绝必须见到 client abort-enter；
  后续动作必须真实到达。`contract.mjs` 从原始 trace 重算这些 milestone，并把
  naturalExit/cleanupExit 绑定到原始 child-exit，拒绝只有摘要声称成功的记录。
  采集前后都核对 producer/baseline 哈希，禁止中途修改采集代码再给旧原件贴新身份。

沙箱允许逐 PID 信号，但本轮中断自测的 `killpg` 返回 EPERM，共享执行器按约保留
租约并失败；不能据此称强制进程组清理已验证。协调者须在宿主重跑这项。自然完成
的采集批次已经逐连接归零，进程组检查 allStopped=true。`cleanupAll` 的异常路径
另有无进程的故障注入：首项清理失败后仍访问后续资源，再汇总错误。

自然流最终结束到 finally 移除、以及移除到 process exit 的微任务窗口没有新增暂停钩子，
不能保证信号精确命中；该端到端交错仍是 #123 缺口。模型无 project、错误 code/message
转换等 P06 旧异常留连接格也未纳入本轮分母。

## 重采、提取与数据负控

phase2-A 已补来源与固定分母守卫。默认非 ignored 的
`signal_oracle_matches_recorded_source_and_local_files` 不依赖 Node，实际读取
本目录六个 producer 文件并比对 SHA256，同时核对完整 source→oracle 投影。
它只表示本地原件一致，不表示外部 TS 文件已经核验。完整
`signal_oracle_regenerates_identically` 在 Rust 与 Node 两侧都重算七个 TS/source/dist
文件的磁盘哈希，缺 `PDRS_BASELINE` 明确失败，保留 ignored 是为了声明外部基线依赖。
选用常跑本地金样守卫解决未运行问题，不修改 CI 元数据或冻结装置。

```sh
PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck \
  python3 -I src/pilotdeck-cli/tests/fixtures/sdk-signals/capture.py NEW_EVIDENCE_DIR
PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck \
  node src/pilotdeck-cli/tests/fixtures/sdk-signals/generate.mjs NEW_EXTRACT_DIR
PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck \
  node src/pilotdeck-cli/tests/fixtures/sdk-signals/negative_controls.mjs
```

采集默认跳过已挂起的 stdin 格，并带上其原件缺口清单。新原件须审查后才替换
已登记 source；生成目录 recursive 创建，文件全部 wx 独占写入。Rust 的 ignored
`signal_oracle_regenerates_identically` 调用 `regenerates_identically("sdk-signals")`
实跑提取并逐字比较，它明确不是重新执行 TS。

`negative_controls.mjs` 在内存中独立破坏真实采集数据，绑定专属断言判杀，全部变异
跑完再汇总 KILLED/SURVIVED/INVALID，并在每轮重验原件。它不修改产品源码、不使用
影子 git、不绕过 HEAD 守卫，也不需要变异 Rust 编译。产品源码负控见下节。

固定分母为 **64 个数据变异 + 11 个额外控制 = 75**：额外控制包括 cleanup、
磁盘漂移、合法但错误的哈希、空/缩短变异表、空结果表，以及三个真实生成器入口
漂移负控。入口负控在独占临时 fixture 目录运行正控→改坏→恢复正控；不复制 Git
仓库、不改原件。所有项跑完再汇总；Rust 解析末行并核 total 与三种状态计数。
Rust 本地金样检查也有实际磁盘漂移、等长换身份、金样输出篡改与分母负控。

```sh
scripts/cargo-slots.sh
export CARGO_TARGET_DIR=/Users/da/ws/pilotdeck-rs/_build/wt-1145/_build/cargo-target
export CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-cli \
  --test sdk_signal_exit signal_ -- --include-ignored --skip signal_cases:: --skip signal_driver
```

## Rust 回放与产品负控

`tests/common/signal_replay.rs` 将同一份真实 Rust 入口编入子进程测试，只有观察钩子
属于 cfg(test)。父进程持有 28999，按客户端原子状态文件触发信号与前驱动作；
不以「已发送」代替「已消费」。子进程通过 `PDRS_SIGINT_WRAPPER` 指定的
仓内 `sigint_default.py` 恢复默认 SIGINT，包装 Python 使用 `-I -B` 避免运行目录缓存。
默认包装位于本 fixture 目录，负控与 Rust 回放均使用它，不依赖宿主 `_tmp/`。

100 个 `signal_cases::*` 测试因依赖固定端口、外部 TS checkout 和观察窗口而 ignored；
默认本地金样守卫照常运行。六个 stdin 测试独立注明 #123/P09 缺口，强制运行会失败，
不能作为成功格。下面命令必须恰好执行 **100** 个测试；0 个测试不构成验收。
每次使用新的绝对证据路径，保存原始 stdout/stderr、actual/comparison、连接 wire
与完整初末树。TS stderr 仅按 `specs/cli-parity/normalization.md` §6/§7 剥精确启动前缀与完整业务行后异常尾段。
关闭帧的 reason 只应用 `D1-SDK-WS-CLOSE-REASON-001` 的四格单字段例外。
#1252 收集器始终保留原始到达次序；TS 数据契约还要求 closes 数组的 seq 严格递增。
全局 seq 记录观察先后，不能为同时关闭的连接创造产品因果序。fix-r2 当时对全部
88 个多关闭格（62 个自然退出 / 26 个观察超时）在真实 TS 与 Rust 各重复十次，
得到 **41 STABLE-SAME / 16 STABLE-DIFF / 31 UNSTABLE**（#1178 后多关闭格共
92：新增 4 个 abort-rst 格均为自然退出双关闭，query RST 先于 client finally
的 control close，顺序因果稳定，按严格序比较并在两轮回放中均 PASS）。
只有实测 UNSTABLE 的 31 个完整 ID
按 `specs/gateway-parity/diffs/D1-SDK-WS-CLOSE-ORDER-001.md` 作序不敏感比较；排序只作用于
比较副本，不去重，不剥实际字段。各条 connection/code/reason/cleanup、条数、重复和
额外字段仍严格；所有 STABLE 格、单关闭格和未登记 ID 保持原比较。
完整逐格十次分布和原始 closes 见 `_tmp/20261026-issue-1252/fix-r2/repeat/classification.md`
及 `classification.json`；STABLE 只表示这十次稳定，不推论所有未来调度。

原 58 PASS / 38 FAIL 的重分类如下（八个单关闭 PASS 不计入多关闭分母）：

| 原结果 | STABLE-SAME | STABLE-DIFF | UNSTABLE | 单关闭 |
|---|---:|---:|---:|---:|
| 58 PASS | 38 | 0 | 12 | 8 |
| 38 FAIL | 3 | 16 | 19 | 0 |

#1260 原新增失败的 36 格中，16 格是稳定不同序，19 格确认重复噪声，1 格
`run--second-sigterm` 是新 TS 十次同序且与 Rust 一致、旧单次金样不同。
该格仅按纯 TS 条件取 ts-01 完整 capture 刷新 source，并重生成主 oracle 及
绑定父 source 哈希的 abort-state oracle，仍保持严格比较；原始来源与哈希见
`_tmp/20261026-issue-1252/fix-r2/reference-refresh.json`。#1260 的当前产品差异范围应为
那 16 个 STABLE-DIFF 格，另外两格动态 UUID 差异仍独立存在。
修复前（#1252 fix-r2）当前实现实跑全部96格：**78 PASS / 18 FAIL**，失败恰为上述16格顺序与2格UUID，
无意外差异；原始结果见 `_tmp/20261026-issue-1252/fix-r2/matrix-final-summary.json`。
**#1260 修复后实跑全部 96 格：94 PASS / 2 FAIL**，失败只剩上面两格动态 runId 的 stdout，
16 格关闭顺序全部 `ok`；原始 `test result` 行与逐格输出见 `_tmp/20261107-issue-1260/matrix/`
（mbp2，HEAD `14393b5f715a64f721b4ffd1d962db02c89d2daa`，dirty 0，持 `/tmp/pdrs-parity.lock` 目录租约）。同一 16 格在修复前后
各重复十轮：修复前 160/160 以 `differing surfaces: ["connection closures"]` 失败，修复后 160/160 `ok`
（`_tmp/20261107-issue-1260/pre/` 与 `post-repeat/`）。
未处理 rejection 仅按 `D1-SDK-SIGNAL-UNHANDLED-REJECTION-001` 的八个完整 ID
接受各自固定的 Rust 单行错误，不匹配或推广 Node 堆栈形状；stdout、退出码、
abort、连接、监听器和文件树继续严格比较。96 个有效观察格（不含另列的六个 stdin 缺口）在 phase4 登记为 94 PASS / 2 FAIL；
#1178 又实测入库 4 格 abort-rst 并全部对齐（两轮 4/4 PASS，evidence 见
`_tmp/20261026-issue-1178/replay-rst-{1,2}/`），现有效观察格共 100。
#1252 首轮严格比较的 58 PASS / 38 FAIL 已按上述真实重复结果重新归因，不能沿用 phase4 历史结论。
`{run,resume}--unknown-cleanup` 的动态 runId 继续按字面比较，该 2 格红已登记为
`specs/gateway-parity/diffs/D1-SDK-SIGNAL-RUNID-LITERAL-001.md`（#2165 人拍 2：只覆盖
这 2 格的 `recovery.runId` 一个字段，属该登记的预期红格；`comparison.json` 的
`acceptedDifferences` 不含该条，比较器与金样语义不变）；16 个稳定关闭顺序差异已由 #1260
按实测 TS 顺序修好（query 连接先于 control，hello-reject 为服务端 4003），修复后不再 FAIL。
该 2 格红的验收与复核**不只数红格**：`differing surfaces: ["stdout"]` 是整段字节
比较的单一名，单凭它分不清「仅 runId 不同」与「该两格 stdout 混入其它回归」，
surface 判据不单独构成放行依据。放行前必须按 `D1-SDK-SIGNAL-RUNID-LITERAL-001`
「机器判据」的字段级复算执行——登记件与更正件（R1 补分母 96→100；R2 修租约前置
引用并把复算命令正本迁到本节）只留判据语义与指针，命令正本如下：把该批落盘的
`evidence/<格>/stdout.bin`（只剥精确 libtest driver 前缀，Rust 侧**不归一化**；
勿用含 driver 横幅的 `stdout.raw`）中 `recovery.runId` 的 36 字节值替换为金样
固定 UUID 后与金样 stdout 逐字节比对，相等才在登记范围内，否则红格不得按该登记
放行（evidence 根目录、source.json 与 cases.json 均显式传参，不依赖 CWD；命令先断言
该根下的格目录集合恰为 `cases.json` 去掉 6 个 stdin 缺口格后的 100 格（关掉「只跑
两格/部分批」的口子），再核两格 `comparison.json` 的 `differences` 恰为 `["stdout"]`。
这两道前置只能把 evidence 根绑到一次**完整跑过本矩阵**的批次，**不能**证明它就是本轮
刚跑的批次——批次与当前 HEAD 的绑定由本批 cargo 的 `test result` 行与验收现场共同
保证，勿把命令指向历史根出假放行。原条目 `:86-100` 的内联命令已由 R2 作废、勿照抄；
同格程序序是先写 `stdout.bin` 再发布 `comparison.json`，故 comparison.json 前置已
蕴含 `stdout.bin` 写完，不存在批中读到半截文件的口子——但「恰 100 格、失败恰 2 格」
等分母判据仍须在该批 cargo 进程退出后核）：

```sh
python3 -I - <evidence-root> <repo>/src/pilotdeck-cli/tests/fixtures/sdk-signals/source.json \
  <repo>/src/pilotdeck-cli/tests/fixtures/sdk-signals/cases.json <<'PY'
import json, pathlib, re, sys
assert len(sys.argv) == 4, 'usage: python3 -I - <evidence-root> <source.json> <cases.json>'
root, source, cases = (pathlib.Path(arg) for arg in sys.argv[1:4])
caps = {c['input']['id']: c for c in json.loads(source.read_text(encoding='utf-8'))['captures']}
expected = {c['id'] for c in json.loads(cases.read_text(encoding='utf-8'))
             if 'stdin' not in c['id']}
actual = {p.name for p in root.iterdir() if p.is_dir()}
assert actual == expected, 'evidence root is not a complete matrix batch: missing ' \
    + str(sorted(expected - actual)[:3]) + ', extra ' + str(sorted(actual - expected)[:3])
for cid in ('run--unknown-cleanup', 'resume--unknown-cleanup'):
    comp = json.loads((root / cid / 'comparison.json').read_text(encoding='utf-8'))
    assert comp.get('id') == cid and comp.get('differences') == ['stdout'], \
        f'{cid}: replay comparison.json missing or not stdout-only — wrong evidence root?'
    g = caps[cid]['stdout'].encode()
    a = (root / cid / 'stdout.bin').read_bytes()
    ar = re.search(rb'"runId":"([0-9a-f-]{36})"', a)
    gr = re.search(rb'"runId":"([0-9a-f-]{36})"', g)
    assert ar is not None, f'{cid}: actual stdout has no 36-byte runId literal'
    assert gr is not None, f'{cid}: golden stdout has no 36-byte runId literal'
    assert a.count(ar.group(1)) == 1, f'{cid}: actual runId literal not unique'
    assert g.count(gr.group(1)) == 1, f'{cid}: golden runId literal not unique'
    assert a.replace(ar.group(1), gr.group(1)) == g, f'{cid}: runId-substituted stdout differs'
    print(f'{cid}: runId {ar.group(1).decode()} -> {gr.group(1).decode()}, substituted stdout byte-equal')
PY
```
`signal_replay::comparison_tests::stable_query_before_control_cells_keep_concrete_order`
逐格钉住这 16 格的 `(2, 1006|4003)` / `(1, 1005)` 具体先后，并断言反序必被比较器拒绝；
该名单还与 source.json 里呈同一形状的格集合逐集合相等（防丢格、防新增同形格；
#1178 的 4 个 abort-rst 格同属该形状，已入名单，名单现 20 格；#2509 起名单长度由测试内
`EXPECTED_STABLE_DIFF_COUNT = 20` 常量钉住——代码常量、模块级 `const STABLE_DIFF_CELLS:
[(&str, u64); 20]` 类型字面、source.json 实测三角，防「数组与实测同步丢格」只剩 SHA 链一道防线；
20 = 16 个 #1260 稳定不同序格 + 4 个 #1178 abort-rst 格，specs/cli-parity/normalization.md
§7 同步钉住）；
产品侧负控 `initialization-query-close-before-control` 把顺序改回旧写法点名用例即红。
`rst_cells_keep_verified_original_shape` 离线钉住 4 格 abort-rst 原件形状（穷尽集合、
input.signal/stage、status、naturalExit、cleanupExit、aborts、connections、activeAfterExit、
stdout 逐字段字面断言；rstApplied milestone、归一化后 stderr、reset 在 query 连接上的
trace 位置与 dispatched abort 的先后、reset 之后无 wire 事件；再以 `compare_surfaces`
校验 TS→Rust 投影形态本身被比较器接受——closes 投影与 listener 折叠，非值钉。格序严格
与不适用任何例外登记由上面的 stable 名单断言和 `approved_stderr_match_domain_is_exact`
旁证钉住；20 格业务前缀 stderr 集与 stable 名单的同集合关系由
`stderr_complete_node_tail_preserves_business_line` 机检钉住）。
离线 `signal_replay::comparison_tests::*` 用真实原件覆盖边界与配对损坏输入，并验证 SIGINT 包装。
`approved_stderr_match_domain_is_exact` 用 proc-macro2 读取匹配臂，严格核对独立八 ID 常量与差异条目；
新增矩阵内独有字节第九臂、矩阵外第九臂、少一臂均须失败。批准 stderr 同时要求原件
保留精确 `PilotDeckError: <msg>` 行；比较器测试的 hooks 投影直接来自 TS hooks，不以错误文字推导。
所有窗口格断言 12 秒内没有自然退出，再由本测试 SIGKILL 并在 3 秒内回收。

整批执行须独占 28999 mock 端口车道：批内只有进程内 `SERIAL` 互斥（`signal_replay.rs:34`），
跨进程无互斥，与同机任何绑定 `127.0.0.1:28999` 的测试 / 脚本须串行。名单是活口径，
不在本文件维护固定枚举：直接绑定者以
`grep -rl 28999 src/pilotdeck-cli src/pilotdeck-gateway/src src/pilotdeck-gateway/tests scripts`
现跑为准（覆盖 `tests/common/`、fixtures 脚本与两侧 `src/`，含仅引用不绑定者，
宁多勿漏）；间接绑定者不含 28999 字面量、grep 抓不到，判定口径是「执行路径会真实
到达 `TcpListener::bind("127.0.0.1:28999")`（mock 端口）的入口都算」——不含字面量
的调用方顺着 `--test sdk_signal_exit` / `signal_cases::` 这类测试选择器找，现知一处：
`src/pilotdeck-gateway/tests/fixtures/sdk-signals/negative_controls.py`（其变异窗口
自 #2513 fix-r3 起是共享租约消费方——`src/pilotdeck-cli/tests/` 已入锁族谓词；自
#2513 fix-r4 起该窗口自身的租约**重定向**到本批证据目录内的
`parity-redirect.lock`（`PDRS_SDK_TEST_PARITY_LOCK` 的 Python 同型，见其
`_redirect_window_parity_lock`），对共享租约仍不互斥，内部 cargo test 选
`signal_cases::*` / `pending_abort_*` 真 bind 28999，与本节矩阵同样须宿主先持租约
再跑）。**自取租约者是例外，宿主不要为它们预持**：重采块
`capture.py:45`（cli-parity 的 `parity_lock()`；`probe.mjs:17-18` 断言 owner.pid 是
capture.py 自己，预持即 `Blocked`）；D0 Rust 家族——名单以
`grep -l 'ParityLock::acquire' src/pilotdeck-cli/tests/*.rs` 现跑为准（当前恰六个
文件：`sdk_{run,resume,format,settings,sessions_list,sessions_messages}_exit.rs`），
各自取同一把锁（180 s 上限后 `.expect` panic），宿主预持即
整族假 FAIL，它们自己就是取锁者；新增自取锁文件时名单随 grep 现跑更新，不靠本段
枚举。本矩阵（`sdk_signal_exit`）不自取锁（只有进程内
`SERIAL` 互斥），故须宿主预持。除上述自取者外，宿主先取 `/tmp/pdrs-parity.lock`
目录租约再执行任何绑 28999 的命令块（以 grep 现跑加间接判定口径为准；取不到租约即
退出稍后重跑，不是自动排队），批期间不得并行任何绑 28999 的套件。取锁/释放的
**形态与片段正本**（含失败路径的 trap 释放与「取不到即退出」）在
`scripts/cli-parity/README.md`「锁被保留时怎么解」节，此处不复写；接管阈值与带接管
装置名单同以该节为准。28999 车道特有的约束：矩阵全批兜底上界按
`选择格数 × (2×12 + 5×3) + 30` 秒保守推导（100 格为 3930 s，公式同 `capture.py:44`），
远超 2400 s 接管阈值，且宿主块没有装置内的硬截止闹钟（cli-parity `run_round.py` 的
2280 s `signal.alarm` 是调用方闹钟，不是租约属性）——宿主持锁**一律发布
`cleanupPending: true`**。重采块自取的恰是**无** `cleanupPending` 的 cli-parity
租约，无格参数全量重采按 `cases.json` 106 格计（106×39+30=4164 s；probe 实跑跳过
stdin 缺口格）同样可超 2400 s，属可被接管的形态（owner 进程死即 `dead_pid` **即刻**
接管、与锁龄无关；或锁龄超 2400 s 按 `expired`）——补发布属代码改动且
capture.py 字节被金样测试 SHA 锚定，跟进单 #2784；重采期间同机不得并行任何带接管
逻辑的装置取这把锁。被接管/双批并行的后果：`TcpListener::bind` 在比较阶段之前直接
panic（该格没有 `differing surfaces` 行），不再满足「失败恰为 2 格且两格均为
stdout 单面」。

#2513 fix-r7 起本车道的租约义务对照如下（车道口径以 grep 现跑为准，本表是
#2513 时点的快照，不是固定枚举）：

| 命令块 / 装置 | 绑 28999 | 自身与共享租约 `/tmp/pdrs-parity.lock` 的关系 | 宿主是否预持 |
| --- | --- | --- | --- |
| `sdk_signal_exit` 矩阵本批（`signal_cases::*` / `pending_abort_*`） | 是 | 不取租约（仅进程内 `SERIAL` 互斥） | **必须预持** |
| `src/pilotdeck-gateway/tests/fixtures/sdk-signals/negative_controls.py`（窗口改 gateway 侧源） | 内部 cargo 真 bind | 变异窗口租约自 fix-r4 重定向到证据目录 `parity-redirect.lock`，对共享租约不互斥 | **必须预持** |
| `src/pilotdeck-gateway/tests/fixtures/sdk-signal-read-lease/negative_controls.py`（GREEN/RED/`--stop-check`，窗口改 cli 侧 `capture.py`） | 常驻测试 `--exact` 只读盘不绑端口 | 变异窗口租约自 fix-r7 同样重定向（`_redirect_window_parity_lock` 同型，`_require_host_lease` 前置闸取不到即退出 2）；不重定向则 writer/red 在宿主持有的共享锁上自排队 60 s 超时 → 全部 INVALID | **必须预持**（与整批同车道；`--window-lock-check` / `--kill-window-lock` 见该 fixture README） |
| 重采块 `capture.py` | 是 | 自取 cli-parity `parity_lock()`（`probe.mjs` 断言 owner.pid 是它自己） | **不要预持**（预持即 `Blocked`） |
| D0 Rust 六文件 `sdk_{run,resume,format,settings,sessions_list,sessions_messages}_exit.rs` | — | 各自 `ParityLock::acquire` 同一把锁（180 s 上限） | **不要预持**（预持即整族假 FAIL） |

```sh
scripts/cargo-slots.sh
PDRS_SIGNAL_RESULTS=/absolute/new-evidence-directory \
  cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-cli \
  --test sdk_signal_exit signal_cases:: -- --ignored --skip stdin_ --test-threads=1
```

`../sdk-signal-abort-state/` 增补 21 格（含四格 abort 固定期限及 hello/request 各三格期限）真实 SDK 状态探针：两类 turn_started 各三种
runId、两种流失败与待决 abort、三种 iterator/consumer 错误。仅 WebSocket IO
被替换；不是完整 CLI 四面对拍。生成器在执行前后校验同一真实 TS 磁盘来源，
默认测试校验生成器与父原件身份，另有显式 ignored 重生成守卫。
待决 abort 的本地 socket 回归须独占 28999：

```sh
scripts/cargo-slots.sh
cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-cli --test sdk_signal_exit \
  product::sdk::turn::tests::pending_abort_keeps_query_until_reply -- --exact --ignored
python3 -I src/pilotdeck-gateway/tests/fixtures/sdk-signals/negative_controls.py --check-anchors
python3 -I -B src/pilotdeck-cli/tests/fixtures/sdk-signals/sigint_default.py \
  python3 -I -B src/pilotdeck-gateway/tests/fixtures/sdk-signals/negative_controls.py NEW_NEGATIVE_DIR
```

产品及比较边界负控固定 79 个变异：原有 41 个（9 个既有产品变异、1 个 abort 期限变异、2 个 hello/request 期限变异、12 个既有比较/包装器变异、11 个八格 stderr 边界变异、2 个嵌套子测试变异、1 个注册差异账本漂移变异 `rejection-registered-difference-drift`（#1278）、3 个关闭顺序变异），#1260 新增 1 个产品关闭顺序变异 `initialization-query-close-before-control`（把初始化失败的关闭顺序改回 control 先关，绑定 `signal_cases::run_query_hello_hold_sigint`）、1 个 #1260 穷尽性变异 `stable-order-pin-broadened`（放宽 16 格形状谓词，绑定比较器用例的集合相等断言），新增 31 个精确名单项变异，#1178 新增 2 个：产品变异 `rst-abort-waits-for-reply`（把 turn.rs 流阶段 socket 错误带在途 abort 的映射改回 retain_query 等回复，12 秒观察内不退出，绑定 `signal_cases::run_abort_rst_sigint`）与 mock 保真变异 `rst-mock-never-resets`（signal_replay.rs 的 Action::Reset 改为 no-op，Rust 子进程不死亡、观察超时，同一绑定），以及
范围扩大、未登记条目、非数组放行三个边界变异。每个名单项独立绑定
`signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics`
与含完整格名的 `#1252 unordered close swap must pass: <ID>` 消息；其余完整绑定见脚本。
该测试从 source 动态枚举全部 92 格（#1178 新增 4 个 abort-rst 格均为多关闭自然退出格），期望策略独立读取差异账本；逐格交换与字段损坏配对验证。
`unordered_close_scope_equals_registration` 另核精确登记与固定分母，旧五格严格 / reason 集成测试仍保留。
每项先正控、变异编译、负控、
恢复、复跑正控，最后汇总全部结果。它使用共享 `mutating_sources/run_owned/wait_for_slot`，
要求调用方设置 `CARGO_TARGET_DIR`，拒绝 dirty/new HEAD，不能绕过。尚未提交的新增
源码须先由协调者提交，随后在宿主编译所有变异并执行全量；未执行项不能称 KILLED。

`--check-anchors` 与兼容入口 `--check` 共用同一份 79 项目录校验。#1252 修复轮只调整下列绑定：

| 变异体 | 完整测试名 | 专属失败消息 |
|---|---|---|
| `held-predecessor-exits-early` | `signal_cases::run_query_model_hold_sigterm` | `#1145 run--query-model-hold-sigterm four surfaces, abort count, listeners and connections match TS` |

该格的 `--model` 使 `initialize_turn` 等待被 mock 持有的 `model_catalog_list`；
SIGTERM 已消费后仍不释放前驱，覆盖同一处两秒提前返回变异。
本轮该格真实 TS/Rust 各 10/10 为 `[2,1]`（STABLE-SAME），继续作为严格比较的正控。
旧 `run_query_resume_hold_sigint` 已实测为 UNSTABLE，其观察噪声不再归入 #1260 的稳定产品差异。

fix-r6 的 hello/request 三格为：持续零间隔非应答帧、前驱永不回复、期限前匹配回复。
Node v22.22.0 的真实 SDK 计时器由 mock.timers 推进；Rust 用每连接独立的 cfg(test)
IO 钩子跨过同一个绝对期限，第二帧阻塞一个有界时间且不让出 poll，最多八帧后结束。
Text / Ping / Pong 都经过同一检查，计时发送与解析使用同一绝对期限。
两个独立负控把各自的接收循环退回 timeout_at(async loop)，用专属测试和错误消息判杀；
abort 两条旧变异随共享 helper 迁到 transport.rs，仍由原来的 abort 用例验证。
