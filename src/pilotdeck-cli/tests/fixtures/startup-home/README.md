# HOME 未设：安全账户 home 字符串回退

本片只修复 Unix、真实进程 HOME 未设、real/effective UID 相同且账户 home 是 UTF8 的已有 TODO。`paths::homedir` 供默认 pilot/token 路径及 skills migrate 使用；HOME 既存（包括空、相对）保持原样。测试 `Env` 不借用宿主环境，须注入隔离账户 home。setuid、缺账户、非 UTF8 passwd、快照之后环境/UID变化及非 Unix 仍明确 TODO，不声称等同于 Node 成功。bare `~` 的空/相对值、setup 独立 home 逻辑、cwd 启动失败均是后续片。

固定 TS `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，Node v22.22.0 / ICU 78.2；已实采依据在主仓 `_tmp/20261108-cli-startup-baseline/README.md` 及 `mbp2-b1`/`mbp2-b2`，其中 HOME-unset 仅调用 os.homedir/路径函数，未访问所返回目录。资源源码不复制入本 fixture。

mbp2 编译 `pilotdeck` 测试bin之后，用本目录自写 `probe.py --node <固定Node> --test-bin <该test可执行文件绝对路径> --evidence <独占新目录>`。脚本创建私有临时 cwd，HOME unset，固定 Node 内建 os.homedir 得到字符串，再只运行 ignored `paths::tests::process_home_unset_string_probe`，比对 home 和默认路径拼接；不执行 CLI main、配置、SDK、技能扫描或 setup。所有子进程有期限，证据目录独占，临时 cwd 最后为空。原始账户 home 字符串只在仓外 node.json，仓内不登记个人账户路径。

普通单测以注入的 `/injected-account-only` 纯路径和独占迁移 fixture 检查 fallback/覆盖优先级及完整技能迁移 report 等价；UID不同不调用 lookup、None/非UTF8拒绝也由纯函数断言。完整 skills 迁移生产命令在 HOME unset 时会按产品语义扫描真实账户默认源，因此**禁止用它做本片真实进程冒烟**；测试注入的 fallback home 完全在 fixture 内。

mbp2 r1 已实际通过 fmt、all-targets 严格 clippy、10 个普通 paths 测试、单独实际执行的 ignored 字符串探针 1 个、14 个 skills_migrate 测试及正式 bin 构建。bin SHA256 `73de92635ee689a5786697fa5da20946424666eb0bd4b2e01716669e66a2c24b`；原始日志已回传主仓 `_tmp/20261108-cli-startup-home/r1-evidence/20261108-startup-home-r1.log`。自写 probe.py 在 mbp2 系统 Python3.9 的 r1 因 hashlib.file_digest 不存在而失败（Rust子进程先已通过），原件保留；循环1MiB读取哈希修复后 r2 实际exit0、Rust1passed、scratchEntries=[]，原件在同证据目录 `startup-home-driver-r2/`。该 testbin SHA256 `41710eeaa3b3fd53a4c0a058b1b41439b03a2d9fbc8b80a98ae3e784b9386330`，区别于上面的正式生产bin。

冻结的 3 个编译输入 SHA256：CLI Cargo.toml `30804f5e5f50a7a8c9ca929d48137c64c0361829960f40a4d0b8b2a425956d7f`；paths.rs `d06c3bc0103c25961a98aa24f6692bbeb0bd932bdafe99af30bb60dece9eaac5`；skills_migrate.rs `a8f2b2b611558a28a73b24b960a5e76c31d78c6db998d834c6fd69b7d3bc91ad`。Rust 编译和运行只在 mbp2。支持范围是在固定基线下恢复同UID账户的普通 HOME-unset 行为，不新增 accepted-diff 或冻结豁免；各异常账户仍未实现。协调者非作者独立审查见主仓 `_tmp/20261108-cli-startup-home/root-review.md`，无确认P0/P1；没有两家 formal gate 或真人实效签字结论。

# setup 的独立 HOME 计算

`gatewaySetup.ts` 的路径与 resolvePilotHome 不同：模块初始 `PILOT_HOME || join(homedir(),'.pilotdeck')` 不展开非空 `~`；Weixin 保存另调用 homedir 后 mkdir，即使自定义 PILOT_HOME 指向别处。本片在 SetupPaths 中只保存 HOME 输入及来源，显式非空 PILOT_HOME 不查账户；真正 Weixin 保存才解析其独立目录，保留相对 mkdir 错误 label 与现有 HOME 缓存。无/空 PILOT_HOME 必须查询 home，不再偷偷降为 cwd/.pilotdeck。

`generate-setup.py --baseline <固定TS仓> --node <固定Node> --output <独占新目录>` 使用系统 Python3.9；自写 setup-driver.mjs 仅通过 Node builtin 同步导出注入假的 os.homedir，调用固定 TS 原 runGatewaySetup 和真实 SDK，复用已有自写 Weixin callback driver 离线 mock fetch。不会调用真实 OS home 或在其目录写入；每格 cwd、账户、配置、缓存、日志均位于独占输出目录，12秒子进程上限。这个受控 OS oracle 用于验证查询时机/产品写入位置，不冒充真实账户系统调用。真实 OS 字符串兼容由首片独立 probe 保留。

采集10格含 HOME unset/empty/relative，PILOT_HOME absent/empty/literal `~`/relative/absolute、菜单或Weixin取消、Weixin成功与缺自定义parent失败；保留逐格原始 stdout/stderr/rc、fetch、homedir调用及产品文件、三种 mkdir 位置。拟提交 oracle 仅将独占 case root 替换为 `<CASE>`，原始字节在仓外证据保留。固定TS十格已在mbp2实际exit0、全部无timeout，原件在主仓 `_tmp/20261108-cli-startup-home/setup-lazy/20261108-startup-setup-ts-r1/`；新 oracle SHA256 `dcdd4e89ac094768e61759730506f1a9b9a36f72a5f0ea19f5b450ac6385971e`，与回传原件逐字相同。PILOT absent/empty取消各调用homedir一次；literal~/relative/absolute取消及Weixin取消零次；三成功和缺parent保存各一次。这是Node受控OS层的实际调用记录，不冒称Rust测试计数。

mbp2 setup r1 已实际通过严格clippy、49普通测试和4个由父实际调用的ignored辅助测试及build，5产品文件postfmt无变化；原始日志在同主仓目录 `20261108-startup-setup-r1.log`。之后只加一个十格完整oracle测试，核完整stdout、返回Ok、全部产品文件/三类mkdir位置、精确HTTP请求、poll headers语义与callbackclock；mbp2 r2 已实际exit0，严格clippy、50普通测试/4父实际调用helper及build通过，5源+oracle六项postfmt零变化。原始日志 `20261108-startup-setup-r2.log`，生产bin SHA256 `59b258b5d90e4261001d421799417cd8fd030dc8a9df9b49aa46a6008f541c89`；非作者独立审查 `setup-lazy/review-r2.md` 无确认P0/P1或需处置具体P2，oracle逐字核对通过。现有47格/YAML/原子保存/网络/PTY逻辑没有改动。

未新增accepted差异、冻结豁免或用户home IO许可；HOME absent真正成功Weixin会按产品语义创建系统home/.pilotdeck，因此只能注入隔离fake account验证，禁止用真实账户路径做成功保存试跑。非Unix/setuid/账户异常仍继承首片明确TODO；整CLI完成/真人签字与正式双谱系合并闸门均不由本片宣称。
