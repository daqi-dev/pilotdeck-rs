# CLI 对拍首批装置

D0-pr1 / Refs #122，仅离线定标。规则先登记于 [normalization](../../specs/cli-parity/normalization.md)，场景与准确 TS 行号见 [scenarios](../../specs/cli-parity/scenarios.md)。没有修改 C/A/B 的冻结装置，没有宣称 Rust 命令已迁移。

```sh
python3 -I scripts/cli-parity/run_round.py --help
python3 -I scripts/cli-parity/run_round.py --baseline /absolute/PilotDeck --output _tmp/20261017-d0/calibration
python3 -I scripts/cli-parity/controls.py --source _tmp/20261017-d0/calibration/ts-1 --output _tmp/20261017-d0/controls
python3 -I -m unittest discover -s scripts/cli-parity -p 'test_*.py' -v
```

`--baseline > PDRS_BASELINE > 本 checkout resources/PilotDeck`；缺失或源 HEAD 不符为 BLOCKED（exit 2）。不自动安装依赖，不下载二进制。`--rust-bin /absolute/pilotdeck --rust-source /absolute/source-checkout` 可在 TS 自比与负控通过后加测 Rust；源码路径是操作者的构建来源声明，装置记录 HEAD 与二进制哈希，不声称能从二进制逆推出源码。省略时不运行 Rust。PASS=0、行为/负控 FAIL=1、装置/环境不具备 BLOCKED=2。

macOS `sandbox-exec` 是本批前置，无裸跑降级参数。沙箱仅允许 runtime 写入，基线与动态库只读，网络（含 loopback/Unix socket）、子进程创建拒绝。独立 OS 会话/进程组与白名单环境运行。探针确认 Node `os.homedir()` 等于 HOME，测试沙箱内读写、沙箱外哨兵读写拒绝、监听/连接拒绝、fork 拒绝；`ps` 提供真实进程树，缺任一能力在产品启动前 BLOCKED。Seatbelt **不改写 passwd/getpwuid 数据**，探针记录 `os.userInfo().homedir` 是否一致；基线使用的 `os.homedir()` 已隔离，其他原生 home 路径的实际访问由文件白名单拒绝。本批不允许子进程，后续需要子进程的场景须另行扩展隔离契约。

整个批次持有共享 `/tmp/pdrs-parity.lock`（`.guard` flock + mkdir + pid/timestamp/token），不抢旧锁。每场景 timeout≤60s，整批最多 30 场景，低于现有 2400s lease；输出上限 4MiB，超过拒绝记录。超时/取消/错误均清理所拥有进程组，清理失败保留锁；从不扫描或杀死用户 PilotDeck。只保存 owned PID 的 ps 行。输出目录限定本 checkout `_tmp/`，拒绝覆盖与路径软链接；场景原件保留，不自动删除工作树。

证据：`summary.json`、`self-comparison.json`、`controls/controls.json`；每轮 `round.json` 中的 source HEAD/实际源码及 dist SHA-256/可执行路径及哈希/装置哈希；每场景 argv/env 去密摘要、stdinHex、stdout.bin、stderr.bin、events.json、initial.json、final.json、tree/、processTree 与资源清理审计。跨流事件是 pipe 采集顺序，首批单输出流可精确比较；PTY、信号及跨流写入全序没有被提前覆盖。

五项负控基于真实 TS 原件复制到新目录，分别丢 stdout、改 stderr、改退出码、漏保留文件、多写文件；更新派生 manifest 仍须被对应 oracle 检出。无有效真实原件时 controls 为 BLOCKED。单测使用临时合成证据验证比较器机制，**不算 TS 定标或真实负控通过**。
