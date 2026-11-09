# 显式原生前台入口与真实流测试

`pilotdeck-control serve` 仅在Unix且可用`/dev/fd`时运行，将显式参数交给现有 `pilotdeck_gateway::native_process::Options::parse` 和 `run`，复用同一生产装配/信号/关闭流程。它是新增的原生前台入口；不会自动查找sidecar、连接后回退另建host、安装组件、探测/杀旧进程或默用用户数据根，不声称完成默认本地CLI、新app JSON-RPC服务、旧server兼容或A8。

所有单值参数沿用native校验：home/project/token-file/sidecar必须显式；home/project为既有分离目录；监听仅127.0.0.1、高端口或0；未知/重复/缺参数直接失败。`--sidecar-arg`按已有合同允许重复以传多个argv，drain-ms/storage-root仍沿原实现。标准输出不提供ready JSON协议；native readiness/stopping保留stderr。一个数据根只能由调用方明确管理的宿主使用，本入口没有解决与旧写者的锁协议。配置可启用native既有cron/IM生命周期，不能把这种server装配冒称为旧默认CLI的本地boot。

```sh
pilotdeck-control serve --home /isolated/home --project /isolated/project \
  --listen 127.0.0.1:0 --token-file /isolated/token \
  --sidecar /explicit/pilotdeck-agent-loop-sidecar
```

home里的配置、token和sidecar均需先准备；缺失按原生错误拒绝。不要将示例替换成运行中的用户数据根。SIGINT或SIGTERM触发既有drain，成功退出0。其余status/capabilities/call/stream继续用显式WS URL访问该listener。

## Rust process 测试

按项目资源闸门在mbp2当前源码独立target运行，不在本机编译。`control_serve_exit`测试实际bin的缺省/重复/未知/相对/不安全监听/重叠目录参数，核完整错误、stdout空、home/project无文件与零连接。help和首参位置分流也由实际bin验证；原control_exit/control_stream全部旧测试继续执行。

```sh
CARGO_TARGET_DIR="$PWD/_build/cargo-target" cargo test --manifest-path src/Cargo.toml \
  -p pilotdeck-cli --test control_serve_exit
CARGO_TARGET_DIR="$PWD/_build/cargo-target" cargo test --manifest-path src/Cargo.toml \
  -p pilotdeck-cli --test control_native \
  one_control_serve_host_is_shared_by_control_and_gateway_wire -- --ignored --nocapture
```

后一个测试是真实 `control serve` 宿主 + 独立旧WS读者 + 真实control子进程，交叉核status/capabilities/session/cron状态、void返回和SIGINT退出；它不提交模型turn，所以其中sidecar是明确失败的占位测试文件，不能据此声称sidecar工作。原 `one_native_host_is_shared_by_control_and_gateway_wire` 仍保留，通过显式PDRS_NATIVE_GATEWAY_BIN运行原gateway入口、SIGTERM关闭。

## 真实 Rust sidecar / fake provider

`native.py` 的 `--host-mode gateway` 是原装置默认；`--host-mode control-serve`只改宿主启动argv，继续使用同一fake OpenAI provider、真实Rust sidecar、控制流/取消/持久化断言。`--signal TERM|INT`选择本次实际关闭信号。每轮独占out，不能覆盖原件。

```sh
python3 src/pilotdeck-cli/tests/fixtures/control-stream/native.py \
  --control "$TASK_CURRENT_CONTROL_BIN" --gateway "$TASK_CURRENT_GATEWAY_BIN" \
  --sidecar "$TASK_CURRENT_RUST_SIDECAR_BIN" --host-mode control-serve \
  --signal TERM --out "$TASK_NEW_SERVE_TERM_OUTPUT"
```

另以新out跑INT。三条bin必须对应当前冻结源码/构建记录；脚本记录bin SHA256不能单独证明源码来源。装置创建隔离home/project/token，provider和宿主动态绑定127.0.0.1:0，不使用18789/真实平台/key。status/capabilities、正常stream、run_get、实际transcript、挂起stream后的abort_turn、provider连接关闭均需通过。宿主stdout须空、stderr须有原生停止信息。

宿主启动后独立监控以有界系统`/bin/ps`持续捕获真实后代PID、开始时间和可执行文件，不依赖provider收到POST；provider处理真实turn时另正控观察sidecar。结束前必须确认实际观察到sidecar，优雅关闭后该身份不再存在。失败清理在杀宿主前先采集后代、join监控，杀收本次直接创建的子进程，再复核所有已捕获后代的开始时间/可执行文件并清理、核退出，不按命令名批量杀进程。扫描/监控失败明确使cleanup_verified为false，不能以空集合假报通过。总期限90秒、子进程和网络步骤均有界，结果/观测/日志保留在out；这只证明隔离fake provider的原生接线，不能代填真实provider、人眼交互、默认宿主安装或A8签字。


## serve token 隔离防护

native原始入口的token路径在parse后重新打开，可能被换成FIFO，原读器还无大小上限；本片不修改其他作者占用的Gateway读器，也不声称A全局已修。新增serve先复用control实际`read_regular`：非阻塞打开同一fd、核普通文件、最多64KiB+1有界读取，超限/不支持即失败。随后只把受控匿名快照交给native，绝非预读后仍让native打开原路径。

快照使用固定、已核root所有且无不受保护写权限的`/tmp`父目录fd；独占0700目录拒xattrs/macACL，通过目录fd create_new/no-follow/CLOEXEC创建0600文件。unlinkat且核nlink=0后才写入token；`/dev/fd/N`启动前重开逐字正控，再seek回0（macOS共享offset），原始UTF8/CRLF/空token判断仍由native执行。File守卫持有至run返回/启动失败再关闭，目录随守卫清理；fd带CLOEXEC，sidecar不能继承。缺`/dev/fd`明确启动前失败，不造Windows成功路径。生产诊断不输出token或快照路径。

Rust真实测试包含私有权限、匿名inode/精确重开、CLOEXEC、native启动失败清理、空token原错误与原件保持；父进程期限保护四格实际子测试：parse后原token换无writer FIFO、快照后原路径换FIFO但native消费匿名原始invalid-UTF8字节、超限文件拒绝，以及真实`ulimit -f 0`在快照写入失败后返回错误、原件不变和目录清理。runtime内的既有SIGXFSZ guard在任何快照write之前安装；不让文件大小限制抢先杀死进程。匿名快照只防原路径替换/增长与其他UID的文件路径访问；同UID调试者/root不在防护范围，原文件同时写入时不承诺原始读是跨写者事务。SIGKILL也不留下命名token副本，但可能留下空0700目录；不宣称安全擦除底层存储块或修复全部故障文件系统IO。
