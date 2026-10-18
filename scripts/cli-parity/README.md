# CLI 对拍首批装置

D0-pr1 / Refs #122，仅离线定标。规则先登记于 [normalization](../../specs/cli-parity/normalization.md)，场景与准确 TS 行号见 [scenarios](../../specs/cli-parity/scenarios.md)。没有修改 C/A/B 的冻结装置，没有宣称 Rust 命令已迁移。

```sh
python3 -I scripts/cli-parity/run_round.py --help
python3 -I scripts/cli-parity/run_round.py --baseline /absolute/PilotDeck --output _tmp/20261017-d0/calibration
python3 -I scripts/cli-parity/controls.py --source _tmp/20261017-d0/calibration/ts-1 --output _tmp/20261017-d0/controls
python3 -I -m unittest discover -s scripts/cli-parity -p 'test_*.py' -v
```

`--baseline > PDRS_BASELINE > 本 checkout resources/PilotDeck`；缺失或源 HEAD 不符为 BLOCKED（exit 2）。不自动安装依赖，不下载二进制。`--rust-bin /absolute/pilotdeck --rust-source /absolute/source-checkout` 可在 TS 自比与负控通过后加测 Rust；比较器必须从这两个参数取得独立预期身份（规范路径、源码 HEAD、二进制 SHA-256），与 Rust 证据逐项相等；来源 side 必须与轮次一致，Node 不可作为 Rust 候选，TS/Rust 身份不能混用。离线 compare.py 比 Rust 时同样必须提供两参数。源码路径仍是操作者的构建来源声明，不声称能从二进制逆推出源码。Rust 轮按发行布局在本轮输出下临时搭 `rust-stage/`（不入库）：`--rust-bin` 复制为 `rust-stage/bin/pilotdeck`，基线 `skills/` 复制为 `rust-stage/skills`（含符号链接即 BLOCKED）；实际执行复制件，身份仍校验原件，复制件哈希必须与原件相同（`round.rust-stage`）；Rust 策略只多放行 `rust-stage/skills` 只读子树，process-exec 只放行复制件，仍读不到基线。省略时不运行 Rust。PASS=0、行为/负控 FAIL=1、装置/环境不具备 BLOCKED=2。

macOS `sandbox-exec` 是本批前置，无裸跑降级参数。TS 与 Rust 各用独立 profile：TS 可读基线与 Homebrew Node 依赖；Rust 只可 exec 自己的候选，可读 runtime、候选自身和系统库，没有 Node exec 或基线读取权限。OpenSSL 统一读取 runtime 内空配置并写入 provenance。沙箱仅允许 runtime 写入，网络（含 loopback/Unix socket）、子进程创建拒绝。独立 OS 会话/进程组与白名单环境运行。TS 公共能力探针确认 Node `os.homedir()` 等于 HOME，测试 runtime 读写、外部哨兵读写与 metadata 拒绝、OpenSSL private/.keepme 拒读、监听/连接拒绝、fork 拒绝；Rust 继承公共能力证据（明确记录 probeProfileSha256），比较器重建并核对更窄的 Rust 实际 policy，另在实际 Rust policy 下测试 Node exec 必须拒绝。`ps` 提供真实进程树，缺任一能力在产品启动前 BLOCKED。已移除全局 file-read-metadata 与 opendirectoryd mach-lookup：metadata 仅限数据白名单及其祖先目录 literal。Seatbelt **不改写 passwd/getpwuid 数据**，如果系统缓存仍能返回 home，探针只记录是否一致；也记录查询拒绝，查询失败不放宽权限。祖先目录对象仍可能暴露存在性/metadata，不开放其子树内容。本批不允许子进程，后续需要子进程的场景须另行扩展隔离契约。

整个批次持有共享 `/tmp/pdrs-parity.lock`（`.guard` flock + mkdir + pid/timestamp/token），不抢旧锁。每场景 timeout≤60s，整批最多 30 场景，启动前强制场景数×timeout×轮数+300s <2400s；持锁 2280s 硬截止触发清理，预留 lease 释放余量。guard 拒绝符号链接/多硬链接，lock/owner 以 0700/0600 创建，owner 通过目录 fd 排除路径重定向。输出上限 4MiB，超过拒绝记录。超时/取消/错误均清理所拥有进程组；锁是否释放只看「本次启动的进程组是否都已被 stop_group 证实停止」（`LIVE_GROUPS` 为空），与异常类型无关，未证实即保留锁（清理本身有界：TERM→1s→KILL→1s→2s）；从不扫描或杀死用户 PilotDeck。只保存 owned PID 的 ps 行。输出目录限定本 checkout `_tmp/`，拒绝覆盖与路径软链接；场景原件保留，不自动删除工作树。

证据：`summary.json`、`self-comparison.json`、`controls/controls.json`；每轮 `round.json` 中的 source HEAD/实际源码及 dist、packages、node_modules、skills 全内容清单 SHA-256/可执行路径及哈希/装置哈希；Rust 轮另记 `stagedExecutable` 与 `stagedSkills`（rust-stage 布局身份，compare 离线读盘复核）。清单包括隐藏 pnpm 目录、文件内容和内部链接目标；ui/apps 内部链接只登记身份，不给目标数据读权。每轮前后重新核对基线，批末复核 Node 与 Rust 身份；每场景内层 argv、外层 sandboxArgv、profile SHA-256（capture 前后复核）、env 去密摘要、stdinHex、stdout.bin、stderr.bin、events.json、initial.json、final.json、tree/、processTree 与资源清理审计。跨流事件是 pipe 采集顺序，首批单输出流可精确比较；PTY、信号及跨流写入全序没有被提前覆盖。

五项负控基于真实 TS 原件复制到新目录，分别丢 stdout、改 stderr、改退出码、漏保留文件、多写文件；更新派生 manifest 仍须同时被对应 oracle 与成对字段比较检出。另有 host_negative_controls.py，用本机 clang 从 exec_node_control.c 构建会 execv Node 的原生假候选，在 Rust 轮记录 EPERM/EACCES 和 exit=126 才算负控通过；该候选绝非 Rust 产品。无有效真实原件时 controls 为 BLOCKED。单测使用临时合成证据验证比较器机制，**不算 TS 定标或真实负控通过**。

本次新增隔离边界需要协调者沙箱外重跑；单测不证明 Seatbelt 实效。host_negative_controls.py 与真实定标的具体命令见 `_tmp/20261017-d0/FIX3.md`。产品超时/输出超限目前统一 BLOCKED、从不 PASS；Rust 行为 FAIL 的细分列为后续分类改进。

## 锁被保留时怎么解

运行器报「retaining shared lock」或下次运行报锁被占时：
1. 看 `/tmp/pdrs-parity.lock/owner.json` 的 pid/token，并在本次输出目录的 `round.json`/`isolation.json` 里找 `audit.remaining`（未停的 pgid）。
2. 确认这些 pgid 已不存在：`ps -o pid,pgid,command -g <pgid>`；仍在就由人判断后 `kill -KILL -<pgid>`（只杀 owner 记录里本装置启动的组，绝不碰用户 PilotDeck）。
3. 确认无残留后，人工删除 `/tmp/pdrs-parity.lock/owner.json` 再 `rmdir /tmp/pdrs-parity.lock`。运行器自身从不抢锁或自动接管。
