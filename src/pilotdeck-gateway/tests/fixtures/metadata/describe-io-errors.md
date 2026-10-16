# #139 describe_project：三个默认 helper 的真实目录错误

`describe-io-errors.json` 为 Node22.22.0/ICU78.2、固定基线 cc7c4e6 的十项原件。
调用真实 GatewayWsConnection → describeWebProject → resolveProjectStorageDir /
summarizeProject → getPilotProjectChatDir / listProjectSessions；只替换无监听 transport，
不替换 session catalog，不注入 errno 或返回值。fs/fsp 的 stat/readdir/read/open 及
FileHandle stat/read 只读观察器记录实际调用、成功值、失败 errno 和调用栈。
源码 SHA256、完整请求/响应、真实 filesystem facts 与 trace 随原件记录。

| 案例 | 真实错误入口 | 完整结果中的会话/时间 |
|---|---|---|
| positive、collision-positive | 正控；后者使用非 legacy storage ID | sessionCount=1，lastActivity 与 createdAt 保留 |
| projects-missing、projects-notdir | storage async readdir ENOENT/ENOTDIR；notdir 也触及 chat_dir sync readdir | sessionCount=0，无 lastActivity/createdAt |
| projects-denied-legacy | projects chmod111；无.cwd，legacy stat先成功，再 async/sync readdir EACCES | sessionCount=1、有 lastActivity，无 createdAt |
| projects-denied-marker | projects chmod111；有效.cwd使 storage早返回，chat_dir sync readdir EACCES | sessionCount=1，lastActivity 与 createdAt 保留 |
| chats-missing、chats-notdir、chats-denied、chats-loop | summary 默认会话目录 readdir ENOENT/ENOTDIR/EACCES/ELOOP | sessionCount=0，无 lastActivity，createdAt 保留 |

响应保留全部属性有无、顺序、真实 birth/ctime/mtime 和数值。使用原隔离绝对路径，
避免路径映射改变派生 legacy project ID。session mtime 通过真实 utimes 设定；createdAt
仍为实际 storage birthtime（或 ctime）。原件不是 list_projects 的复用结果。

Rust 的必跑 replay 使用原始 facts 和真实 RPC 观测错误，完全离线、不读取归档路径。
完整性守卫锁定十个唯一 ID、RUN 状态、describe_project 方法、各 helper 真实调用栈/
API/错误种类及结果时间属性；空、截短、重复、错方法、丢任一 helper 错误、错 errno 和
NOT_RUN 负控均拒绝。完整 response 逐字节比较；配对测试要求上游下一帧只能是独立
observer-fence，十项各自核对，无 describe_project 请求上游。

权限以实际能力为前提。若 chmod 未能制造目标 EACCES，Node 标 NOT_RUN 并退出2；
不能因为 root 或其他权限覆盖而记空 PASS。收集器保留受限目录到 Native 验证结束，
随后以自有 owner marker 校验恢复三处权限。恢复之后只有固定 facts replay 仍适用；
不能把已恢复的现场称作 Native 原位复现。两个 ignored Native 测试只接受显式新录制
manifest；常规CI的三项 replay 不依赖实际权限或这些归档路径。

维护入口为 `tests/record_metadata_io.mts`；编排入口为 `tests/check_metadata_io.py`。
后者先只读核验原 installed 工具链、完整组件和六个执行文件，Cargo/RUSTC/RUSTDOC
绝对绑定、原 HOME/CARGO_HOME/RUSTUP_HOME 固定，Node 独立隔离 HOME/XDG。缺失先拒绝，
RUSTC_WRAPPER/RUSTC_WORKSPACE_WRAPPER及两个CARGO_BUILD_*别名明确置空以覆盖Cargo配置，
不调用 rustup 安装。目标目录必须全新/空且独占，2jobs/locked/offline；运行有时间与
输出上限，退出0还必须匹配测试数和十个唯一实际案例回执，finally 恢复权限。

```sh
python3 src/pilotdeck-gateway/tests/check_metadata_io.py \
  --baseline /Users/da/ws/pilotdeck-rs/resources/PilotDeck \
  --toolchain /Users/da/.rustup/toolchains/1.99-aarch64-apple-darwin \
  --target /absolute/path/to/new-owned-cargo-target \
  --output /absolute/path/to/new-preflight-output --preflight-only
# 用另一个全新 output 执行；先核对 preflight 中的原安装及环境。
python3 src/pilotdeck-gateway/tests/check_metadata_io.py \
  --baseline /Users/da/ws/pilotdeck-rs/resources/PilotDeck \
  --toolchain /Users/da/.rustup/toolchains/1.99-aarch64-apple-darwin \
  --target /absolute/path/to/new-owned-cargo-target \
  --output /absolute/path/to/new-verification-output
python3 -m unittest discover -s src/pilotdeck-gateway/tests -p test_metadata_io_driver.py -v
```

本批不改变产品吞错行为或冻结装置。Linux真实 errno/非法目录项仍由 #117/#133 验证。
本轮曾发生隔离HOME触发rustup自动安装事故；事实、下载现场、停点与修复待验状态只在
state/board.md 和 state/memory/20261016-runner-home.md 维护，不以本说明代替独立审阅。

复现护栏补强（PR141增量）：录制器在第一次动态 import 前和采集结束后，逐字节
绑定固定 cc7c4e6 的全部 `src/` Git blob；缺文件、改字节、源码树内未登记/ignored
影子文件、同内容 symlink 都拒绝。只核 HEAD 不足以证明实际执行源码；`node --test
src/pilotdeck-gateway/tests/test_baseline_source_guard.mjs` 用自有 Git fixture 验证七种
正负控，不修改 resources。源码树之外的依赖闭合仍由 #85 跟进。

驱动只继承 locale/普通临时目录白名单，剥除调用者的 RUSTFLAGS/CARGO_*/NODE_OPTIONS
等注入；Node/Git 固定实际可执行位置，Rust 四 wrapper 仍明确空串。所有列明 source
必须存在，执行后重新核 source、六工具与 components SHA、Node SHA；原 rustup 的
`toolchains/downloads/tmp` 记录 type/size/mtime/link 前后摘要（不含 atime），Node
隔离 HOME 下出现 .cargo/.rustup 即失败。该快照只证明本次受监控子树，不追认旧事故
之前不存在的历史快照。子进程通过 pipe 有界读取，总日志至多1MiB；timeout/超输出
必须 kill/reap 并保留原因和 wait 记录。Python watchdog 负控只运行本 Python 解释器，
不尝试缺失 Rust 安装或下载。

finally 除退出码外，读取 owner/restored=3 的严格回执，并对三处实际权限逐一 stat；
缺/错回执和 no-op 都失败。恢复命令复用收集时相同 watcher/tsx loader。恢复后只能
重放固定 facts。CI 跑纯源绑定与环境/receipt/恢复/watchdog 负控；本机十项受限目录
Native 与 pairing 实测证据另存主仓 revision-verified，不由 CI mock 代填。


BND2输入交接：录制器每次真实chmod前先同步写出原path/mode计划到驱动pipe，写全后才改权限；最后将一次序列化的完整oracle同样送入pipe。驱动保留至多1MiB输出在内存，完整oracle与三项原permission计划必须一致，工作文件只作被验证副本。replay/Native前后、cleanup及最终报告前逐字节核对；文件改目标、改mode或删除都FAIL，仍按内存原计划恢复并stat原目标。报告记录原permission、completeOracle、artifactUnchanged、restored和真实回执，不从改过的JSON重新选期待。

record非零/超时/超输出时，即使尚无最终oracle或工作文件，已同步提交的permission计划仍在内存，finally按已发生步骤恢复1至3处。杀子进程后有界drain内核pipe中剩余字节，保留已写计划；直接子进程和持stdout后代控制见Python测试。旧`--restore fixture`缺原计划参数会拒绝；自动驱动把可信内存计划作为独立参数传给Node，standalone调用不能重新信任可改fixture来选择恢复对象。

该保证针对本批自有fixture数据跨阶段替换；目录/owner marker所有权检查继续执行。实际目录对象被移动/替换的恢复能力不由这批数据绑定证明。P0历史及独立终审仍按board处理。
