# HOME 未设：安全账户 home 字符串回退

本片只修复 Unix、真实进程 HOME 未设、real/effective UID 相同且账户 home 是 UTF8 的已有 TODO。`paths::homedir` 供默认 pilot/token 路径及 skills migrate 使用；HOME 既存（包括空、相对）保持原样。测试 `Env` 不借用宿主环境，须注入隔离账户 home。setuid、缺账户、非 UTF8 passwd、快照之后环境/UID变化及非 Unix 仍明确 TODO，不声称等同于 Node 成功。bare `~` 的空/相对值、setup 独立 home 逻辑、cwd 启动失败均是后续片。

固定 TS `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，Node v22.22.0 / ICU 78.2；已实采依据在主仓 `_tmp/20261108-cli-startup-baseline/README.md` 及 `mbp2-b1`/`mbp2-b2`，其中 HOME-unset 仅调用 os.homedir/路径函数，未访问所返回目录。资源源码不复制入本 fixture。

mbp2 编译 `pilotdeck` 测试bin之后，用本目录自写 `probe.py --node <固定Node> --test-bin <该test可执行文件绝对路径> --evidence <独占新目录>`。脚本创建私有临时 cwd，HOME unset，固定 Node 内建 os.homedir 得到字符串，再只运行 ignored `paths::tests::process_home_unset_string_probe`，比对 home 和默认路径拼接；不执行 CLI main、配置、SDK、技能扫描或 setup。所有子进程有期限，证据目录独占，临时 cwd 最后为空。原始账户 home 字符串只在仓外 node.json，仓内不登记个人账户路径。

普通单测以注入的 `/injected-account-only` 纯路径和独占迁移 fixture 检查 fallback/覆盖优先级及完整技能迁移 report 等价；UID不同不调用 lookup、None/非UTF8拒绝也由纯函数断言。完整 skills 迁移生产命令在 HOME unset 时会按产品语义扫描真实账户默认源，因此**禁止用它做本片真实进程冒烟**；测试注入的 fallback home 完全在 fixture 内。

mbp2 r1 已实际通过 fmt、all-targets 严格 clippy、10 个普通 paths 测试、单独实际执行的 ignored 字符串探针 1 个、14 个 skills_migrate 测试及正式 bin 构建。bin SHA256 `73de92635ee689a5786697fa5da20946424666eb0bd4b2e01716669e66a2c24b`；原始日志已回传主仓 `_tmp/20261108-cli-startup-home/r1-evidence/20261108-startup-home-r1.log`。自写 probe.py 在 mbp2 系统 Python3.9 的 r1 因 hashlib.file_digest 不存在而失败（Rust子进程先已通过），原件保留；循环1MiB读取哈希修复后 r2 实际exit0、Rust1passed、scratchEntries=[]，原件在同证据目录 `startup-home-driver-r2/`。该 testbin SHA256 `41710eeaa3b3fd53a4c0a058b1b41439b03a2d9fbc8b80a98ae3e784b9386330`，区别于上面的正式生产bin。

冻结的 3 个编译输入 SHA256：CLI Cargo.toml `30804f5e5f50a7a8c9ca929d48137c64c0361829960f40a4d0b8b2a425956d7f`；paths.rs `d06c3bc0103c25961a98aa24f6692bbeb0bd932bdafe99af30bb60dece9eaac5`；skills_migrate.rs `a8f2b2b611558a28a73b24b960a5e76c31d78c6db998d834c6fd69b7d3bc91ad`。Rust 编译和运行只在 mbp2。支持范围是在固定基线下恢复同UID账户的普通 HOME-unset 行为，不新增 accepted-diff 或冻结豁免；各异常账户仍未实现。协调者非作者独立审查见主仓 `_tmp/20261108-cli-startup-home/root-review.md`，无确认P0/P1；没有两家 formal gate 或真人实效签字结论。
