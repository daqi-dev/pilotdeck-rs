# CLI 对拍首批装置

D0-pr1 / Refs #122，仅离线定标。规则先登记于 [normalization](../../specs/cli-parity/normalization.md)，场景与准确 TS 行号见 [scenarios](../../specs/cli-parity/scenarios.md)。没有修改 C/A/B 的冻结装置，没有宣称 Rust 命令已迁移。

```sh
python3 -I scripts/cli-parity/run_round.py --help
python3 -I scripts/cli-parity/run_round.py --baseline /absolute/PilotDeck --output _tmp/20261017-d0/calibration
python3 -I scripts/cli-parity/controls.py --source _tmp/20261017-d0/calibration/ts-1 --output _tmp/20261017-d0/controls
python3 -I scripts/cli-parity/test_cli_parity.py
```

`--baseline > PDRS_BASELINE > 本 checkout resources/PilotDeck`；缺失或源 HEAD 不符为 BLOCKED（exit 2）。不自动安装依赖，不下载二进制。`--rust-bin /absolute/pilotdeck --rust-source /absolute/source-checkout` 可在 TS 自比与负控通过后加测 Rust；比较器必须从这两个参数取得独立预期身份（规范路径、源码 HEAD、二进制 SHA-256），与 Rust 证据逐项相等；来源 side 必须与轮次一致，Node 不可作为 Rust 候选，TS/Rust 身份不能混用。离线 compare.py 比 Rust 时同样必须提供两参数。源码路径仍是操作者的构建来源声明，不声称能从二进制逆推出源码。Rust 轮按发行布局在本轮输出下临时搭 `rust-stage/`（不入库）：`--rust-bin` 复制为 `rust-stage/bin/pilotdeck`，基线 `skills/` 复制为 `rust-stage/skills`（含符号链接即 BLOCKED）；实际执行复制件，身份仍校验原件，复制件哈希必须与原件相同（`round.rust-stage`）；Rust 策略只多放行 `rust-stage/skills` 只读子树，process-exec 只放行复制件，仍读不到基线。省略时不运行 Rust。PASS=0、行为/负控 FAIL=1、装置/环境不具备 BLOCKED=2。

macOS `sandbox-exec` 是本批前置，无裸跑降级参数。TS 与 Rust 各用独立 profile：TS 可读基线与 Homebrew Node 依赖；Rust 只可 exec 自己的候选，可读 runtime、候选自身和系统库，没有 Node exec 或基线读取权限。OpenSSL 统一读取 runtime 内空配置并写入 provenance。沙箱仅允许 runtime 写入，网络（含 loopback/Unix socket）、子进程创建拒绝。独立 OS 会话/进程组与白名单环境运行。TS 公共能力探针确认 Node `os.homedir()` 等于 HOME，测试 runtime 读写、外部哨兵读写与 metadata 拒绝、OpenSSL private/.keepme 拒读、监听/连接拒绝、fork 拒绝；Rust 继承公共能力证据（明确记录 probeProfileSha256），比较器重建并核对更窄的 Rust 实际 policy，另在实际 Rust policy 下测试 Node exec 必须拒绝。`ps` 提供真实进程树，缺任一能力在产品启动前 BLOCKED。已移除全局 file-read-metadata 与 opendirectoryd mach-lookup：metadata 仅限数据白名单及其祖先目录 literal。Seatbelt **不改写 passwd/getpwuid 数据**，如果系统缓存仍能返回 home，探针只记录是否一致；也记录查询拒绝，查询失败不放宽权限。祖先目录对象仍可能暴露存在性/metadata，不开放其子树内容。本批不允许子进程，后续需要子进程的场景须另行扩展隔离契约。

Python 入口在第一条语句断言 `sys.flags.isolated` 且装置目录未被重新加入 `sys.path`（守卫前只 `import sys`），随后按显式兄弟路径读取 `_bootstrap.py` 源码，由共享引导设置临时字节码前缀并提供唯一加载器；装置模块一律按显式文件路径 `importlib.util.spec_from_file_location` 加载并断言 `module.__file__`，不再 `sys.path.insert` 或裸名导入，因此显式文件入口下的同目录同名包/扩展无法顶替。只支持上述显式文件测试入口；禁止 `unittest discover`，它会重新插入测试目录，且同名测试包可能在入口守卫执行前就顶替测试文件。隔离负控在私有临时镜像放冒名物，不写实际装置目录（#701 item 3）：

```sh
python3 -I src/pilotdeck-gateway/tests/fixtures/cli-parity-isolation/negative_controls.py --output _tmp/20261026-issue-701/isolation-controls
python3 -I src/pilotdeck-gateway/tests/fixtures/cli-parity-isolation/test_negative_controls.py
```

整个批次持有共享 `/tmp/pdrs-parity.lock`（`.guard` flock + mkdir + pid/timestamp/token），不抢旧锁。每场景 timeout≤60s，整批最多 30 场景，启动前强制场景数×timeout×轮数+300s <2400s；持锁 2280s 硬截止触发清理，预留 lease 释放余量。guard 拒绝符号链接/多硬链接，lock/owner 以 0700/0600 创建，owner 通过目录 fd 排除路径重定向。输出上限 4MiB，超过拒绝记录。超时/取消/错误均清理所拥有进程组；锁是否释放只看「本次启动的进程组是否都已被 stop_group 证实停止」（`LIVE_GROUPS` 为空），与异常类型无关，未证实即保留锁（清理本身有界：TERM→1s→KILL→1s→2s）；从不扫描或杀死用户 PilotDeck。只保存 owned PID 的 ps 行。输出目录限定本 checkout `_tmp/`，拒绝覆盖与路径软链接；场景原件保留，不自动删除工作树。

证据：`summary.json`、`self-comparison.json`、`controls/controls.json`；每轮 `round.json` 中的 source HEAD/实际源码及 dist、packages、node_modules、skills 全内容清单 SHA-256/可执行路径及哈希/装置哈希；Rust 轮另记 `stagedExecutable` 与 `stagedSkills`（rust-stage 布局身份，compare 离线读盘复核）。清单包括隐藏 pnpm 目录、文件内容和内部链接目标；ui/apps 内部链接只登记身份，不给目标数据读权。每轮前后重新核对基线，批末复核 Node 与 Rust 身份；每场景内层 argv、外层 sandboxArgv、profile SHA-256（capture 前后复核）、env 去密摘要、stdinHex、stdout.bin、stderr.bin、events.json、initial.json、final.json、tree/、processTree 与资源清理审计。跨流事件是 pipe 采集顺序，首批单输出流可精确比较；PTY、信号及跨流写入全序没有被提前覆盖。

五项负控基于真实 TS 原件复制到新目录，分别丢 stdout、改 stderr、改退出码、漏保留文件、多写文件；更新派生 manifest 仍须同时被对应 oracle 与成对字段比较检出。另有 host_negative_controls.py，用本机 clang 从 exec_node_control.c 构建会 execv Node 的原生假候选，在 Rust 轮记录 EPERM/EACCES 和 exit=126 才算负控通过；该候选绝非 Rust 产品。无有效真实原件时 controls 为 BLOCKED。单测使用临时合成证据验证比较器机制，**不算 TS 定标或真实负控通过**。

本次新增隔离边界需要协调者沙箱外重跑；单测不证明 Seatbelt 实效。host_negative_controls.py 与真实定标的具体命令见 `_tmp/20261017-d0/FIX3.md`。产品超时/输出超限目前统一 BLOCKED、从不 PASS；Rust 行为 FAIL 的细分列为后续分类改进。

## 锁被保留时怎么解

运行器报「retaining shared lock」或下次运行报锁被占时：
1. 看 `/tmp/pdrs-parity.lock/owner.json` 的 pid/token/cleanupPending，并找残留 pgid：
   - **`cleanupPending: true` 不等于「无残留」**：它是获取时先发布的占位，意思是「残留清单还没发布」；
     只有正常释放且仍有存活组时才会被改写成 pgid 列表。看到 `true` 或看不到列表，要按 `pid` 查进程的
     启动会话与子进程组（`ps -o pid,pgid,sess,command -p <pid>`，再看 `-g <pgid>`）——**只按 pid 查进程树不够**：
     子进程常以 `start_new_session=True` 起在**另一个会话/进程组**里，owner 被 SIGKILL 后它们已被 reparent，
     进程树上与 owner 没有父子关系。
   - 本装置（`run_round.py` 等，`scripts/cli-parity/common.py:368`）的 `owner.json` **没有** `cleanupPending` 键，
     残留只能看本次输出目录 `round.json` / `isolation.json` 的 `audit.remaining`。
   - `scripts/parity/round.py` 看 `run.json` 的 `cleanup.remaining` 与 `ownedGroupsRemaining`；
     被 SIGKILL 时 `run.json` 停在启动快照、这两个键可能根本没有。
   - gateway 侧装置（`session_domain.py` / `collect.py` / `native-topology/run.py`）看 `owner.json` 的
     `cleanupPending` 列表（未证实停止的组会写在这里）。
   - 都拿不到时还有两条兜底线索：本次输出目录里的进程组记录（cli-parity 看 `round.json` / `isolation.json` /
     `summary.json`，gateway 系看 `launch.json` / `report.json`），
     以及 `ps` 里仍指向本装置 witness 端口或 `_tmp/<本次目录>` 的命令行。**证据不足就保留锁，继续独立工作**
     （同 `scripts/gateway-parity/README.md:169` 的「证据不足则保留」），不要凭「看不到列表」推定无残留。
2. 确认这些 pgid 已不存在：`ps -o pid,pgid,command -g <pgid>`；仍在就由人判断后 `kill -KILL -<pgid>`（只杀 owner 记录里本装置启动的组，绝不碰用户 PilotDeck）。
   **列表缺失或形态不可解析不等于无残留**：先 `ls -la /tmp/pdrs-parity.lock` 看实际内容——`owner.json.tmp.*`
   残留、0 字节或截断的 `owner.json` 都出现过（`scripts/parity/round.py:100-104` 是 tmp+replace，
   `scripts/gateway-parity/common.py:75-77` 的 `save` 是直接覆写，SIGKILL 可留半截）。截断的 `owner.json`
   **读不出 pid/token**，第 1、3 步的比对都做不了，此时只能靠第 1 步的兜底线索与 `ps` 判断；判不了就保留锁。
   注意损坏的已发布 `owner.json`（存在但解析不了）**永远不会被自动接管**，只能人工回收；
   目录里没有 `owner.json` 的空锁才会在 `MAX_AGE` 后按 mtime 被**带接管逻辑**的装置回收（见下节）。
3. **持 guard 互斥后再删**：用 `flock(1)` 或任意 `fcntl.flock(LOCK_EX)` 包装在 `/tmp/pdrs-parity.lock.guard` 上取独占锁
   （macOS 无 `flock(1)` 时用后者；装置自身的建锁、接管、释放都在同一 guard 下：`scripts/parity/round.py:199-207`、
   `scripts/gateway-parity/common.py:594-600`）。持锁期间**重新读一遍** `owner.json` 的 `pid` / `token` / `cleanupPending`，
   确认还是第 1 步那个已停止的 owner（不是接管者刚发布的新 owner），再删 `owner.json` 并 `rmdir /tmp/pdrs-parity.lock`，
   删完才放开 guard。`owner.json` 缺失/不可解析时没有可比对的 owner，确认第 1、2 步的进程组都停干净后再删。
   **只删 `owner.json` 与锁目录本身，不要删、不要替换 `/tmp/pdrs-parity.lock.guard`**
   （更别用 `rm -rf /tmp/pdrs-parity.lock*`）：`scripts/cli-parity` 的 guard 与 Rust 测试都以
   `O_NOFOLLOW` + `S_ISREG` + `nlink==1` 打开并校验该路径（其余装置用 `open('a')` 或 `os.open(..., O_CREAT)`），
   重建即换 inode，正持锁者的 flock 会静默失效。
   **不持 guard 直接删会与自动接管竞态，真实后果是「双持锁」而不是「排队卡死」**：人的 `rm` 若落在接管之后，
   抹掉的是**新 owner（仍在运行）**的标记；接着 `rmdir` 会成功（锁目录里只有 `owner.json`，`.guard` 是目录外的
   同名兄弟文件）⇒ 活 owner 手里的锁目录被整个删掉，下一个等待者看到锁不存在就立刻建锁发布，**两个运行同时进入
   持锁段**，互斥失效（它们会争同一批 witness 端口与 /tmp 共享状态，污染在跑的校准证据，可能造成假 PASS / 假 FAIL）。
   反过来，若只 `rm owner.json` 没 `rmdir`、留下无标记目录，它会在 `MAX_AGE`（2400 s）后按超龄被自动接管，
   **不会「永久」留存**——但**只有带接管逻辑的装置**（`scripts/parity/round.py` 与 gateway 系）会回收它；
   下一个恰好是 `calibrate.sh`（它先阻塞在 guard 上）或 `scripts/cli-parity` 自己的装置时，它们只报「锁被占」、
   绝不接管，空目录会原样留着，仍需人工 `rmdir`。
   所以别把「有人在排队」当成唯一症状——看到锁被异常改动，先按本节持 guard 核 owner 再动手。运行器自身从不抢锁或自动接管。

### 各装置的锁等待差异（同一把共享锁）

锁契约相同（`/tmp/pdrs-parity.lock` 目录租约 + `.guard` flock + `owner.json`），但**等待上限与失败形态不同**
（2026-11-06 从 #590 四格账本 §七上移，源码行号为当时 HEAD；行号会漂，符号名见括号）：

| 装置 | 等待上限 | 抢不到锁的形态 | 源码 |
|---|---|---|---|
| `scripts/parity/round.py`（宿主 vs sidecar，53 场景） | **无限等**（`sleep 2` 轮询，没有 timeout 形参） | 不超时、不写 `run.json`，在 stderr 打 `waiting for /tmp/pdrs-parity.lock`；**别当死锁去删锁** | `parity_lock` `:211-267` |
| `scripts/native-witness/collect.py`（原生采集） | **1 s** | 抛 `Blocked` → **BLOCKED / exit 2** | `parity_lock(timeout=1)` `:182` |
| `scripts/gateway-parity/session_domain.py`（E1 校准 / 原生判分） | **30 s** | 同 BLOCKED / exit 2（`TimeoutError` 走顶层 BLOCKED 分支） | `parity_lock(timeout=30)` `:2952` |
| `src/pilotdeck-gateway/tests/fixtures/native-topology/run.py`（4 案 × 5 轮） | **默认 600 s**（`common.parity_lock` 默认值） | `TimeoutError` 未被 main 捕获 → **traceback / exit 1**（与它自己的 `run()` 退出码不同，判成败看 `report.json`） | `parity_lock()` `:609` |
| `scripts/gateway-parity/{run_round,check_ruler}.py`（模块对拍 / 冻结清单核验） | **默认 600 s**（`--lock-timeout`） | `TimeoutError` 未被捕获 → **traceback / exit 1** | `common.py` `:604`、`:649` |
| `scripts/ui-server-parity/calibrate.sh`（UI↔server 定标） | 锁目录 **0**；guard 上**阻塞等**（不设上限） | 目录已存在 → `FileExistsError` 立即退出；guard 被占时会一直等（期间信号被挂起） | `acquire()` `:76-88` |
| `scripts/cli-parity` 自己的装置（`run_round.py` 等） | 锁目录 **0**；guard 最多 **60 s**（`GUARD_TIMEOUT`） | 目录已存在 → `Blocked` → **BLOCKED / exit 2**；guard 超时同样 `Blocked` | `common.py` `:287`、`:366-385` |

`waiting for /tmp/pdrs-parity.lock` 这行由上表的 `parity/round.py`（`:250`）与 gateway 系（`common.py:649`）打印；
看到它先按上表对号（无限轮询 vs 有上限），再决定等还是查。cli-parity 自己的装置与 `calibrate.sh` 不打这句。

**上表的等待上限只管锁目录，不管 `.guard`**：`.guard` 是另一把锁，`parity/round.py`、gateway 系与 `calibrate.sh`
都在它上面用**阻塞 flock、没有超时**（`round.py:220` 是 `deferred_signals(), lock_guard(lock)`，gateway 系
`common.py:621` 同理），超时判定在拿到 guard **之后**才执行。所以前一个持锁者若在临界区内被 `SIGSTOP` 或卡在
磁盘 I/O，等待方会**静默挂在 guard 上**，「到上限就报错」不会发生；这些装置在挂起期间还把 SIGINT/SIGTERM 延迟，
只能 SIGKILL。唯一例外是 `scripts/cli-parity` 自己：`LOCK_NB` + 50 ms 轮询 + 60 s 上限（#1064），guard 卡住也只
`Blocked`，不改动锁目录。

`2400 s` 只是 `MAX_AGE` 上限，**已发布且带 `cleanupPending` 的 owner 不会因年龄被自动接管**：接管条件要求
`cleanupPending` 为假。`parity/round.py`、gateway 系（`session_domain.py` / `collect.py` / `native-topology/run.py`）、
`ui-server-parity/calibrate.sh` 获取时都先发 `cleanupPending: true`，所以这些装置被 SIGKILL 后留下的锁
**不会自愈**，按上一节人工回收。两个例外：
- **SIGKILL 落在 `mkdir` 与 `owner.json` 发布之间**时没有 `owner.json`（原子发布的 `parity/round.py:239-242` 与
  `calibrate.sh:48-52` 只留 `owner.json.tmp.*`，`gateway-parity/common.py:637-641` 是直接 `write_text`，可能留空目录
  或**截断的已发布 owner**）；「从未发布」的空目录/临时文件会按 mtime 在 `MAX_AGE` 后被接管，**不是永不自愈**；
  但 gateway 系留下的**截断 owner 属于 `damaged`，永不接管**，只能人工回收。
- **`scripts/cli-parity` 自己的装置与 Rust CLI 测试的 `ParityLock`** 的 `owner.json` 没有 `cleanupPending` 键
  （前者 `common.py:368`，后者 `src/pilotdeck-cli/tests/*_exit.rs` 只写 pid/timestamp/token），锁不受这条规则保护，
  人工回收前务必先按上一节第 1 步看 `round.json` / `isolation.json` 的残留。

另：`scripts/gateway-parity/README.md:169` 的人工回收段写于本节的 guard 规则之前，只要求「核实 owner 已停止」，
**按旧口径操作会撞上一节所述的竞态**；该文件在冻结装置目录内（`DATA_FILES` 白名单），改动要走冻结程序，
本节不代改。两份说明冲突时以本节为准（双向指针待冻结程序补，见 #1783）。

**同契约但不在上表的另一个使用者**：Rust CLI 集成测试的 `ParityLock`
（`src/pilotdeck-cli/tests/*_exit.rs`，同一 `.guard` + `owner.json` 契约，另加 `O_NOFOLLOW` 与 `nlink==1` 加固）：
等待上限 **180 s**，超时即测试失败、绝不接管；`cargo test` 与全量对拍撞在一起时，这类等待属正常排队。
它同样不写 `cleanupPending`（见上）。

**不在这套租约里的**：`scripts/fe-parity/README.md` 的一批配方直接
`mkdir /tmp/pdrs-parity.lock || exit 1` + `trap 'rmdir' EXIT`——没有 `.guard`、没有 `owner.json`，
退出时的 `trap rmdir` 正是本节禁止的无 guard 删锁。看到锁目录里**没有** `owner.json` 时先怀疑它，
按上一节第 1 步核残留后再处理；它与本节的租约规则冲突时以本节为准（同一反模式还散落在
`docs/experiments/exp010-ui-server-parity-device/readme.md` 等实验记录里，同见 #1783）。
