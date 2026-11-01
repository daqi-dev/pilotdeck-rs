# #1140 Linux 原生 exec / 出网见证

本目录是独立采集器，不是评分入口；但 `--session-domain` 模式的采集目录可被 `session_domain.py --suite deadline --native-capture` 消费（#1140 A5.4 接线），native 格由此产生真实的 PASS/FAIL，无证据的格保持 `NOT_RUN`。采集器与冻结审计之间的记录契约（#1140 fix-r1 对齐三处形状）：launch 合同显式声明 `'connections': 1`（计划连接数不靠 observer 反推），每条 observer row 携带 `wire` 元数据（opcode/fin/rsv/masked/lengthCode/payloadLength，供 `compare.wire_frame` 复核），且 send 行的 `frame` 是 payload **文本**、只有 receive 行记录解码后的 JSON——冻结审计的方向感知 decoder 交叉核对据此读取。该契约由离线端到端套件 `src/pilotdeck-gateway/tests/fixtures/native-session-e2e/` 用采集器的真实输出逐案例判分验证。

## 运行

只支持 Linux（内核需支持 pidfd），要求 Python 3.11+、已有 strace（支持 `--kill-on-exit`）、已编译的 Linux gateway 与 sidecar。无需 root，不 attach 用户进程。macOS 的 eslogger + root 后续另做，不回退 ps 采样。

```sh
# 从仓根构建；sidecar 属 pilotdeck-transport，两个 --bin 分别构建。
export CARGO_TARGET_DIR="$PWD/_build/cargo-target"
export CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only
scripts/cargo-slots.sh && cargo build --manifest-path src/Cargo.toml -p pilotdeck-gateway --bin pilotdeck-gateway
scripts/cargo-slots.sh && cargo build --manifest-path src/Cargo.toml -p pilotdeck-transport --bin pilotdeck-agent-loop-sidecar

python3 -I scripts/native-witness/collect.py \
  --gateway "$CARGO_TARGET_DIR/debug/pilotdeck-gateway" \
  --sidecar "$CARGO_TARGET_DIR/debug/pilotdeck-agent-loop-sidecar" \
  --output _tmp/20261026-issue-1140/linux-native-001

python3 -I src/pilotdeck-gateway/tests/fixtures/native-witness/test_witness.py \
  --capture _tmp/20261026-issue-1140/linux-native-001/capture

# session-domain 采集：72 个冻结 case 各占一个 session/run，随后由 deadline CLI 判分。
python3 -I scripts/native-witness/collect.py \
  --gateway "$CARGO_TARGET_DIR/debug/pilotdeck-gateway" \
  --sidecar "$CARGO_TARGET_DIR/debug/pilotdeck-agent-loop-sidecar" \
  --output _tmp/20261026-issue-1140/linux-session-001 --session-domain

python3 -I scripts/gateway-parity/session_domain.py --suite deadline \
  --native-capture _tmp/20261026-issue-1140/linux-session-001 \
  --output _tmp/20261026-issue-1140/deadline-session-001
```

输出目录必须不存在。默认网关 `127.0.0.1:28999`、模型 mock `127.0.0.1:29000`；`--port` 可选 28999..29008（模型为下一端口），绑定前使用 `scripts/gateway-parity/common.py::parity_lock` 的 `/tmp/pdrs-parity.lock` 目录租约，覆盖采集和 provider 清理；目录占用轮询 timeout 为 1 秒（guard 行为继承共享协议），锁忙或任一固定端口占用均报告 `BLOCKED`、退出码 2，不搜索或连接其他进程。探针按真实监听方区分策略：gateway 的预检和启动失败复查均不设 `SO_REUSEADDR`，TIME_WAIT 若使裸 bind 失败也报告 BLOCKED；provider 预检设置 `SO_REUSEADDR`，与 Python `HTTPServer` 一致，可复用 TIME_WAIT。两种策略均拒绝活动监听。与 CLI/native-topology/全量对拍共用租约。独立 `home/`、`project/`；环境只带这次 home、固定 PATH/LANG，不读真实 key，不碰 `~/.pilotdeck` 或 18789。仅发一次本地模型轮次（产品可能另发标题请求）；driver 的每次等待共用 60 秒绝对 deadline。模型服务器在 accept 后立即设置 5 秒 socket 空闲超时，覆盖请求行、headers 和 body。

CLI 消费者判定可重试 BLOCKED 时，必须同时核对退出码 2 和 **stdout JSON 的 `status == "BLOCKED"`**。argparse 用法错误同样返回 2，但只往 stderr 写 usage、stdout 为空，不能按退出码单独归为 BLOCKED。成功状态也走 stdout。绑定竞态可能已创建输出目录，重试必须换新目录。

EADDRINUSE 仅在预检/启动失败复查的 `probe.bind` 和 provider 的实际 `server_bind` 中映射为 BLOCKED。`WireClient.create_connection` 的临时源端口耗尽、socket 创建/选项错误以及其他采集行为保留原异常，不能归为固定监听端口冲突。WS 发送按 payload 字节数支持 7/16/64 位长度编码，包含 65535/65536 边界。

`collect.py` 的 smoke 模式通过真实 native opt-in 启动 gateway，完成 WS hello / submit_turn，本地 mock 返回文本，以 pidfd 核实所属 tracer 后 SIGTERM 停机，等待完整 trace 与退出码 0，再确认 sidecar exec、模型出网和 JSONL 落盘；这些检查及 provider 清理都先于 receipt 发布。smoke 模式下 `driver.json` 保留本轮 WS 字节、解码和 mock 请求；它仍是驱动观察。采集 deadline：smoke 60 秒、session-domain 900 秒（`w.capture` 的 `timeout`/`limit` 显式传参，不接受无上限等待）。

`--session-domain` 模式按 `scripts/gateway-parity/fixtures/session-calibration.json` 的 72 个冻结 case 顺序驱动：每个 case 独立 sessionKey/runId 与一次 submit_turn，实际模型请求数量如实保留，超时类 case 带 `timeoutMs=400`，时序类 case 带 `timeoutMs=30000` 且 provider 延迟 1.5 秒；最终帧后固定 settle 0.5 秒再读取三面（frames / durableStatus / registry）。结果发布到输出目录的 `session-domain/`：

- `observer.json`：采集器内 observer 在一条连接上记录的 hello 与全部帧（`{connections, rows}`）；observer 记录独立于被测网关与预注册 contract，不独立于 driver。每行形如 `{direction, wireHex, wire, frame, connection}`，`send` 行的 `frame` 为 payload 文本、`receive` 行为解码对象。
- `cases.json`：72 行 `{case, identity, observation}`，identity 含 requestId/projectKey/sessionKey/runId/实际 transcriptPath，observation 含三面。registry 面的 `applicable` 是观测值：由该 run 在 `runs.json` 中是否出现 events/record 推导（`bool(events or record)`），不是常量——宿主真的没有该 run 的 registry 记录时如实记 `applicable: false`，与 `no-registry-*` 案例的登记期望可以匹配；runs.json 缺失、events 与 record 均缺时同样记不适用。record 缺 `state` 属于该案的 `registry.state` 期望不符（单案 FAIL），不是结构错误，更不会让整批判分崩溃。
- `provider.json`：driver 侧模型请求台账 `{endpoint, requests}`，与 strace 出网见证交叉核对；每条受控 streaming model 请求必须独占一个 manifest case 的 marker，marker 必须来自 user content；标题请求不能补足模型调用覆盖。#1284 人批 A：`native_evidence.required_provider_cases` 仅从冻结 #1177 `rust.frames` 机械派生必需请求集合（含模型产出 / 请求后的 `turn_completed` 帧），不读 Rust 结果或超时标签。缺请求仅使该 case 以 `provider.requests` 判 FAIL，仍形成 72 case 同源链并逐格评分；无模型产出期望的 case 不强求请求。空请求台账同样进入逐格评分，不因结果错误作废可信原始证据。
- `driver.json`（#1140 fix-r3）：driver 在 hello 收到时单独记下的绑定值与逐 case 映射 `{binding, identities}`，是 observer 行台账之外、由同一采集器生成的第二份一致性记录；冻结 loader 要求二者一致，observer 侧 hello binding 被改成另一合法值（wire 字节与 receipt 哈希重算）时以 `BINDING` 拒收。

`capture/` 保留：

- `prelaunch.json`：执行前固定的 ELF 路径、完整 argv、sha256、角色、端点与 case；`launch.json` 只追加实际 tracer 的 launcherPid。两者都不得预注册 `binding` / `observerRows` / `bindingSource`。
- `trace.log`：strace 原始完整进程树日志；`stdout.log`、`stderr.log`：被测物输出与跟踪器诊断。
- `evidence.json`：processes / execWitness / networkWitness / transcriptWitness / cleanup，直接采用冻结消费者字段名；exec 包含短命后代、退出码、父子关系与线程归属；网络包含地址、端口、TID、TGID 和系统调用结果（失败的 connect 也保留）；transcriptWitness 记录 host 的会话转录首开（openat 路径与该行已记录的 exec 数——fix-r4 P1-1 的逐 session 归责锚，marker/session key 由冻结 `native_evidence.py` 唯一导出，采集侧按显式路径加载）。
- `receipt.json`：只有正常完成、解析完整、smoke/session 验证及 provider 清理通过后才原子发布；附件哈希清单以 `native_evidence.py:NATIVE_RECEIPT_KEYS` 为准与 tracer 退出码。失败只有 `failure.json`，不能算 WITNESS_CAPTURED。

## 证据边界

`strace -f` 追踪 execve/execveat、clone/clone3/fork/vfork、exit/exit_group、connect、openat、sendto/sendmsg/sendmmsg、io_uring_setup。保留 exit status，不能使用 `-qq`；`-xx` 避免 C 字符串转义歧义，过长/不认识的格式拒收。[strace 官方手册](https://github.com/strace/strace/blob/master/doc/strace.1.in) 定义这些选项。

本版本是受控 ELF / TCP profile：argv/path 必须与启动前登记一致，sha256 在启动前及采集后核对；假定受控二进制运行中不被替换后又恢复，**不是 exec 瞬间的内核镜像哈希证明**。脚本解释器、成功 execveat、同 PID 重复 exec、PID 复用、无 exec 的 fork 子进程、未知可执行文件、未退出线程、逃逸 clone flags、AF_UNIX helper、地址型 UDP/SCM_RIGHTS、成功 io_uring_setup 均拒完整。Popen 关闭继承描述符，观测范围是它启动的子树；不防恶意内核/同用户篡改整套原始证据。unfinished/resumed 必须按 PID 配对并核对 syscall 名，缺失、重复或错配均拒收；配对后再提取参数和返回值，去掉 strace 在参数末尾插入的分隔空白。线程 connect 归 TGID，已连接 socket 的无目的地址 write/send 不另计新出口。孙代照实记录，但冻结审计当前只允许 host 直接子进程；不会把孙代伪装成直接子进程。

openat 只在 host 拥有、路径形如 `(.+)/chats/([^/]+)\.jsonl`、成功且为绝对路径时记为会话转录首开（fix-r5 P2-1）；其余 openat——sidecar/子进程的打开、host 的非转录打开、标准库 dirfd 相对路径（read_dir、remove_dir_all 内部）、失败打开——一律跳过，不再作废整份采集。host 拥有但转录形状的相对路径仍拒收（无法归责），跳过本身 fail-closed：`validate_session` / `audit_native` 要求 manifest 内每个 session 的 host 首开都被记录，且审计拒绝任何非 host 拥有的已记录首开。

**sendmmsg 所有批次均明确拒收，包括已连接 socket**；保留 TRACE 中该调用用于发现不支持的行为，不表示可认证。当前不解析每条 msg_hdr，不能凭批次中某个 `msg_name=NULL` 就认证混有目的地址或控制消息的整个批次。单条 sendto/sendmsg 的既有边界不变。此处选择 #1169 允许的“明示拒收”方案，扩展支持需真实批量 trace 与逐条核验测试。

argv 元素内的逗号、空格、引号在 `-xx` 下均编码为 `\\xNN`，例如 `, ` 是 `\\x2c\\x20`；因此字面 `, ` 仅分隔元素，既有 roundtrip 用例覆盖这类路径，#1169 的逗号误拆未复现。非 UTF-8 的 path/argv/address 字节受控报 Incomplete，不泄漏 UnicodeDecodeError，也不替换非法字节。

`trace.log` 的三处文件读取（driver 查 host、capture 解析、离线 reload）共用 **64 MiB（含边界）**上限：在同一打开文件上先 fstat，超限不读取；随后最多读上限加一字节并再次检查，覆盖检查后增长。超限或非 UTF-8 都拒收，采集路径不发布成功 receipt。该上限控制读取与解析输入，不限制 strace 在采集窗口内的磁盘写入量；原始失败 trace 保留供排查。

正常停机要求 gateway 在 `--drain-ms 1500` 下处理 SIGTERM 并 exit 0，tracer 同样退出 0；cb8 对 HEAD 949b385a 的原始 trace/receipt 已实证此形态。被信号杀死或其他非零退出仍拒收，不能当正常 drain。`signal.pidfd_send_signal` 自 Python 3.9 已提供（见 [Python 3.11 官方文档](https://docs.python.org/3.11/library/signal.html#signal.pidfd_send_signal)），不降级为存在 PID 复用窗口的 `os.kill`。

取租约之前安装 SIGINT / SIGTERM 处理器，直到租约退出后才恢复；预检、provider 启动、capture 前后都走取消清理。provider 身份发布和清理窗口使用共享 `deferred_signals`，provider 线程继承屏蔽掩码，主线程进入 capture/Popen 前恢复原掩码。capture 启动发布窗延迟 Python 异常直到 Popen 身份存下，清理期间重复信号延迟处理。provider shutdown 等待上限 6 秒，随后线程 join 上限 6 秒；清理失败保留 `cleanupPending` 租约，不发布成功。异常/超时杀本次持有的 tracer；`--kill-on-exit` 使内核终止其 tracees（包括改变会话的后代）。强制 SIGKILL 采集器不保证执行 Python 清理，协调者应先 SIGTERM；不声称失败路径 `allStopped=true`。

## 原有审计与漏记负控

离线 `load_capture()` 核对 receipt 哈希与启动前后合同；`checked_evidence()` 从原始 trace 重建全部进程与出网清单，再和派生产物精确对比，绝不把不完整标志提升为 true。`audit()` 将核验后的证据交给原封不动的 `session_domain.audit_native`，并调用冻结 `_verify_cli_imports()`；没有影子 git、替代 HEAD 或关闭守卫。

冻结函数本身只信 `networkWitness.complete`，`calls=[]` 且 complete=true 会被它接受；本包明确测试这个既有边界。漏记出网通过独立回放使 complete=false，然后由原函数以 `NETWORK_WITNESS` 拒绝。短命子进程漏记与 source=ps 同样由原函数拒绝。

完整 audit 还要求 observer rows、binding、capabilities、cases 和 output/disk/http 表面。**binding 的证据合同已按 #1140 改为：采集器内 observer 在 hello 时记录，再与 contract 绑定；hello 观测独立于被测网关与预注册 contract，不独立于 driver。**——`witness.bind_contract()` 拒绝任何预注册了 `binding`/`observerRows`/`bindingSource` 的合同，只接受从 `evidence['observer']` 的 hello 帧推导的绑定，与冻结 loader 的规则互为镜像；离线 `--capture` 检查沿用同一入口，不再有 `SYNTHETIC_CONTROL_ONLY` 外壳，输出 `REAL_WITNESS_AUDIT`。全部真实报告继续标 `scoring: NOT_RUN`。

deadline 判分入口只接受 `receipt.status == WITNESS_CAPTURED`、全部 receipt 附件哈希核对通过、launch/prelaunch 一致且无预注册 binding 的采集目录；随后逐 case 与 #1177 登记的期望比对（`executedCases` 只收非 NOT_RUN 的 verdict，单案观测缺失保持 `NOT_RUN` 且不计入已执行）。真实 Rust 的场景刺激与 TS 六格语义尚待 #1242 对齐；目前真实失配不能全部归责为产品错误，也不允许缩小分母。离线端到端套件 `tests/fixtures/native-session-e2e/`（真实 socket mock 网关回放登记期望 + 只 mock OS 层的完整采集，再按显式路径加载冻结评分）断言 72/72 逐案例 PASS，配套数据负控与源码变异负控，列表以各脚本 `cases` 为准。

`load_native_capture` 用 observer/hello 重构 `audited` evidence 的 capabilities/binding/observerRows 字段（loader 是运行期绑定的绑定方），这三处与 contract 的比对在新路径上是同源自比、恒真——真正闭合该面的是 `_observer_binding`（hello 帧推导）、driver 记录与 observer 绑定的一致性（只改单侧时可发现不一致）、loader 的预注册 key 拒收、wire 解码交叉核对与完整 receipt 哈希；`audit_native` 的 `bindingSource` require 是对非 loader 调用方的纵深防御（loader 在真实路径上自己覆写该字段）；已知边界见 #1140 fix-r1 RESULT（评审 P2-1，无假 PASS）。

#1206 已知边界：只防协作写者的误改或只改单侧。driver.json、observer.json 由同一采集器生成，且附件哈希位于可共同重钉的 receipt；能够同时重写三者的对抗写者不在此威胁模型内。driver 记录不是采集侧之外的独立签名锚。`tests/fixtures/native-witness/test_boundary.py` 固定“健康通过 / 只改 observer 拒收 / 共同改写接受”的已知边界；该接受用例不代表可防篡改，不改变 #1177 判分。历史 #1140-r4 冻结事件的“独立锚”措辞由新增 #1206 事件更正，旧事件原字节保留。

## 开发检查

```sh
python3 -I src/pilotdeck-gateway/tests/fixtures/native-witness/test_witness.py
python3 -I src/pilotdeck-gateway/tests/fixtures/native-witness/negative.py --check-anchors
# 新源码提交后，使用调用方既有 CARGO_TARGET_DIR：
python3 -I src/pilotdeck-gateway/tests/fixtures/native-witness/negative.py \
  _tmp/20261026-issue-1140/negative-committed-001
```

源码负控依赖 `negative_process.mutating_sources/run_owned`；每条变异先 `compile` 检查 Python 语法，再正控、变异、恢复复测，全部跑完汇总，变异列表与数量以 `negative.py` 的 `cases` 和执行汇总为准；可加 `--case NAME` 仅选一个样本。采集器、解析器、测试与负控脚本都须等于 HEAD；未提交时正常拒绝，不是 KILLED。Python 测试不运行 cargo，不创建第二构建目录。不新增 TS 金样，不声称 TS 行为对齐。

端到端套件 `tests/fixtures/native-session-e2e/`（变异列表见 `negative_controls.py::cases`，绑定被点名测试的专属失败消息）同样要求提交后等于 HEAD 运行；Rust 侧入口 `src/pilotdeck-gateway/tests/native_session_e2e.rs` 在 cargo 测试中拉起该套件。

`test_frozen_audit_and_controls` 会调用 `_verify_cli_imports()` 的 #166 守卫：工作树与 HEAD 不一致时该用例按设计失败，提交后在干净检出上运行（协调者 chain 脚本已包含）。候选阶段只跑无需 CLI HEAD 核验的合成／内存桩路径；不能把它们记为正式冻结入口 PASS。

## fix-r6 证据认证边界

正式 `load_native_capture` 先拒绝 `SYNTHETIC_CONTROL_ONLY`，再共用冻结侧 `load_capture` 与 `checked_evidence`：拒 failure/receipt 并存，从原始 trace 回放进程、出网和 marker，禁止把 complete=false 提升为 true。合成控制只能调用明确的测试辅助入口，不能产出正式 deadline PASS。

`session-domain/sources.json` 保存每案 `.cwd` 路径/内容、实际转录路径/原始 JSONL、`runs.json` 路径/原始内容，纳入 receipt。冻结侧从 observer 重建顺序 submit/event/final，核 event 信封与连续 seq，再核 case→request→project/session/run、driver 映射、原始文件投影。registry 按完整 project/session/run 键匹配，拒重复与 false applicability 携带非空记录。原始路径须绝对且规范化，marker 必须 host 成功打开同一实际转录；保留 dirfd/flags/result。读打开也可作为“已经打开实际转录”的证据，不额外推断写模式。

session profile 从冻结 manifest 独立派生：一个 host、每 case 一个 sidecar、固定角色集合、一个 WS 连接、固定范围的 gateway 端口及唯一相邻 provider 端口。合同与证据同步增加 helper 或出口仍拒收。WS profile 为完整 text frame、FIN=true、RSV=0、client masked/server unmasked；采集器与正式 loader 共用校验。

离线复算必须在 **原绝对路径** 恢复同 sha256 的 host/sidecar；只有另一目录中的同内容副本不够。缺原二进制为 `BINARY_UNAVAILABLE` / BLOCKED，同路径错误内容仍拒收。当前不提供 relocation 映射。deadline 评分前与结束时均要求冻结检查成功；失效时不保留可消费的 cells。

tracer 在 Popen 返回且取消仍延迟时登记共享所有权，只有 wait 确认回收才移除。kill 与 wait 分别保护；provider 清理不能清除 tracer 登记。failure 发布失败附在原异常上，未确认回收保留 cleanupPending/租约，不能写成功 receipt。

`test_authentication.py`、`test_chain.py` 与 `negative_authentication.py` 覆盖上述拒收。后者 `--check-anchors` 编译每个 Python 变异；候选阶段可用 `--memory` 在单独子进程内注入源码字节，不改工作树、不跳过生产守卫。默认原地模式仍须源码等于 HEAD，并使用共享变异守卫；两种模式都逐项保存 positive/mutant/restored，完整汇总 KILLED/SURVIVED/INVALID。合成、mock 与内存验证只证明装置认证和接线，不是实际 Rust 场景验收。

#1140 fix-r8-res 的 mock 生命周期矩阵在 macOS 实跑：未完成取消返回 False，全部 case 服务完返回 True；close 同时关闭 listener/active socket。provider 端口重用负控由同轮 RESULT 记录，Linux SO_REUSEADDR 复核由协调者在宿主跑，未声称已完成。
