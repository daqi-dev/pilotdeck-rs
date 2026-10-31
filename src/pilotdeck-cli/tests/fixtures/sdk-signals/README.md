# #1145 P08：信号矩阵与 Rust 生命周期回放

本包包含 TS 原件、来源验证和 96 格独立的 Rust CLI 进程回放。Rust 实现位于
`src/sdk/{signals,turn,transport,output}.rs`。**仍不表示 P08 / #123 完成**：
stdin 六格挂起，动态 UUID 仍按字面比较；
#1151 已批准严格的业务行后 Node 尾段归一化及四格 4003 close reason 差异。
#1164 选 B 已批准八格未处理 rejection 的 stderr 差异，精确范围见
`specs/gateway-parity/diffs/D1-SDK-SIGNAL-UNHANDLED-REJECTION-001.md`。产品变异负控独立于数据负控。

## 矩阵与原件

`cases.json` 是 102 格的预注册输入：run/resume，各自覆盖两种信号、注册前的
control 前驱、stdin、query hello/resume/model 前驱的保持/释放/拒绝、
abort 成功/拒绝/保持、同种与交叉重复信号、终态消费后取消、abort 与终态交错、
正常/异常收尾。每格 12 秒观察上界；未退出只是观察事实，不能推断永不退出。
TS hello/RPC 的默认超时也不同于宿主观察上限。

`source.json` 保存 96 格真实 TS CLI 的真实 WebSocket 观测，另有 6 格 stdin
缺口。原样 stdout/stderr、wire、客户端确认、退出与清理、初末文件树均保留；
Node 版本固定 v22.22.0，TS 基线固定 cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85，
实际加载的 source/dist 与探针哈希随采集保存。`oracle.json` 只作确定性提取，
不剥警告、不归一化 UUID、不把服务端发送当成客户端消费。

stdin 缺口的原件单独保留在 `gap-source.json`：本宿主的 Node pipe 与阻塞 FIFO
都未稳定建立等待 EOF，真实 `/dev/stdin` 读取出现 EAGAIN。readFile 调用钩子
不能证明读成功；某次信号先于错误到达也不算覆盖。该采集线已挂起，前置于
#123 / P09。旧观测用当时的 probe 哈希标识，只用于说明缺口；不作为有效消费格。

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

固定分母为 **61 个数据变异 + 11 个额外控制 = 72**：额外控制包括 cleanup、
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

96 个 `signal_cases::*` 测试因依赖固定端口、外部 TS checkout 和观察窗口而 ignored；
默认本地金样守卫照常运行。六个 stdin 测试独立注明 #123/P09 缺口，强制运行会失败，
不能作为成功格。下面命令必须恰好执行 **96** 个测试；0 个测试不构成验收。
每次使用新的绝对证据路径，保存原始 stdout/stderr、actual/comparison、连接 wire
与完整初末树。TS stderr 仅按 `specs/cli-parity/normalization.md` §6/§7 剥精确启动前缀与完整业务行后异常尾段。
关闭帧只应用 `D1-SDK-WS-CLOSE-REASON-001` 的四格单字段例外。
未处理 rejection 仅按 `D1-SDK-SIGNAL-UNHANDLED-REJECTION-001` 的八个完整 ID
接受各自固定的 Rust 单行错误，不匹配或推广 Node 堆栈形状；stdout、退出码、
abort、连接、监听器和文件树继续严格比较。96 格预期 94 PASS / 2 FAIL；
`{run,resume}--unknown-cleanup` 的动态 runId 保留 FAIL。
离线 `signal_replay::comparison_tests::*` 用真实原件覆盖边界与配对损坏输入，并验证 SIGINT 包装。
所有窗口格断言 12 秒内没有自然退出，再由本测试 SIGKILL 并在 3 秒内回收。

```sh
scripts/cargo-slots.sh
PDRS_SIGNAL_RESULTS=/absolute/new-evidence-directory \
  cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-cli \
  --test sdk_signal_exit signal_cases:: -- --ignored --skip stdin_ --test-threads=1
```

`../sdk-signal-abort-state/` 增补 11 格真实 SDK 状态探针：两类 turn_started 各三种
runId、两种流失败与待决 abort、三种 iterator/consumer 错误。仅 WebSocket IO
被替换；不是完整 CLI 四面对拍。生成器在执行前后校验同一真实 TS 磁盘来源，
默认测试校验生成器与父原件身份，另有显式 ignored 重生成守卫。
待决 abort 的本地 socket 回归须独占 28999：

```sh
scripts/cargo-slots.sh
cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-cli --test sdk_signal_exit \
  product::sdk::turn::tests::pending_abort_keeps_query_until_reply -- --exact --ignored
python3 -I src/pilotdeck-gateway/tests/fixtures/sdk-signals/negative_controls.py --check
python3 -I src/pilotdeck-gateway/tests/fixtures/sdk-signals/negative_controls.py NEW_NEGATIVE_DIR
```

产品及比较边界负控固定 29 个变异（9 个既有产品变异、12 个既有比较/包装器变异、6 个八格 stderr 边界变异、2 个嵌套子测试变异），完整绑定测试名与单行断言；每项先正控、变异编译、负控、
恢复、复跑正控，最后汇总全部结果。它使用共享 `mutating_sources/run_owned/wait_for_slot`，
要求调用方设置 `CARGO_TARGET_DIR`，拒绝 dirty/new HEAD，不能绕过。尚未提交的新增
源码须先由协调者提交，随后在宿主编译所有变异并执行全量；未执行项不能称 KILLED。
