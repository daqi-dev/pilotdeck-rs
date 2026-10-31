# #1145 P08：先矩阵、后实现

本包只交信号与资源生命周期的 TS 候选金样、采集器和原件验证。
**不包含 Rust 信号实现或逐格 Rust CLI 对拍，不表示 P08 / #123 完成。**
协调者提交本阶段矩阵和金样后再进入实现；不能用本包的数据负控替代
listener 提前/漏移除、abort 不等回复、前驱假通过、遗留连接等产品负控。

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

```sh
PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck \
  python3 -I src/pilotdeck-cli/tests/fixtures/sdk-signals/capture.py NEW_EVIDENCE_DIR
node src/pilotdeck-cli/tests/fixtures/sdk-signals/generate.mjs NEW_EXTRACT_DIR
node src/pilotdeck-cli/tests/fixtures/sdk-signals/negative_controls.mjs
```

采集默认跳过已挂起的 stdin 格，并带上其原件缺口清单。新原件须审查后才替换
已登记 source；生成目录 recursive 创建，文件全部 wx 独占写入。Rust 的 ignored
`signal_oracle_regenerates_identically` 调用 `regenerates_identically("sdk-signals")`
实跑提取并逐字比较，它明确不是重新执行 TS。

`negative_controls.mjs` 在内存中独立破坏真实采集数据，绑定专属断言判杀，全部变异
跑完再汇总 KILLED/SURVIVED/INVALID，并在每轮重验原件。它不修改产品源码、不使用
影子 git、不绕过 HEAD 守卫，也不需要变异 Rust 编译。产品源码负控留在第二阶段。

```sh
scripts/cargo-slots.sh
export CARGO_TARGET_DIR=/Users/da/ws/pilotdeck-rs/_build/wt-1145/_build/cargo-target
export CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only
cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-cli \
  --test sdk_signal_exit -- --include-ignored --test-threads=1 --show-output
```
