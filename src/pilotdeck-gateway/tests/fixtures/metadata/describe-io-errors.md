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

Snapshot 只接受已登记事实或错误中的 API/path 组合；未知请求直接 panic，不能用
可被产品吞掉的默认 ENOENT 代替。direct、pairing 和保留现场 Native 都记录生产实际
entries/read/stat/session_summary 的路径及成功/errno，核对归一后的 Node 观察集合，
并断言 errorTarget 被使用；独立探测正确目录不算生产请求证据。同步/异步 API 合并，
fsp.open 代表 session_summary 边界，handle 内部读取与 existsSync 不作为独立 Files
请求；不比较次数和顺序。成功结果记成功状态，数据与时间仍由固定事实及完整 RPC 校验。
两个空 RPC 的 missing 场景保留错目录负控，expectedError 与 observation.code 各自有
单轴负控；errorTarget 还必须匹配 home/projects 或 chats。

静态 fixture 的全部168154字节固定为原 Node 录制 SHA256
`995400bce07ef1bee5e4e0d6d3766343ae0229e21eac496394b6866e68518c7c`，独立字面量
位于 Rust loader，解析前校验；不信任 JSON 自报 sources/owner 等元数据。它与主仓
`_tmp/20261016-debt-cleanup/metadata/metadata-io.json` 原件逐字节相同，初录命令与收集器
身份见该目录 record-runner.json，绑定证据见 provenance-original-binding.json。新增录制
需保留新 Node 原件并独立复核锚的更新，不能由 Rust 回填期望。驱动 source allowlist
由12项扩为13项，新增此 JSON，控制核精确集合及该文件运行中漂移。早前12源执行索引
保持原有范围；本次13源/定向与静态锚负控见 provenance-handoff-index.json。

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
src/pilotdeck-gateway/tests/test_baseline_source_guard.mjs` 用自有 Git fixture 验证
guard 的正负分支；控制数量以对应源码版本的执行日志和证据索引为准，不修改 resources。
源码树之外的依赖闭合仍由 #85 跟进。

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
重放固定 facts。CI 的 Node 控制用自有 Git fixture 验证 source guard，Python 控制
覆盖环境/receipt/恢复/watchdog；这些控制不代表真实基线源码或受限目录已经复算。
本机早期真实 Node、受限目录 Native 与 pairing 完整链证据保留于主仓
`_tmp/20261016-debt-cleanup/metadata/snapshot-verified/verification.json`，执行时的
十二源文件 SHA 逐项绑定 `07b20537bae029d5bc596e492c9a62a964620177` Git blob，见
`snapshot-head-binding.json` 与 `snapshot-handoff-index.json`。早前 `revision-verified`、
`revision-tail-verified` 等原件保留，分别对应各自历史源码，不能视作最新版本复算。

`fce91709506c95ea0c66ec3b0591a379b99039bb` 仅追加 sourceHead 身份控制尾差，验证见
`head-handoff-index.json`；`1c90ff5f3d4e81472f6f8654081a5e555952c02b` 仅追加 Python
捕获顺序和输出超限恢复控制，验证见 `capture-handoff-index.json`。两段尾差各有 Python/
Node 控制及十二源 SHA 绑定，未重新执行完整 Rust 链；不能将07b真实链宣称为最新 HEAD
完整复算。此前文档澄清的源码范围与证据索引见 `doc-handoff-index.json`，各次控制计数
以各自绑定日志为准，不由 CI mock 代填本机真实证据。

加入 Snapshot 路径护栏与生产请求审计后，完整实际 driver 已另用全新独占 target
`_build/20261016-113400-metadata` 复算，原件在主仓 metadata 的
`path-fresh-verified/verification.json`；真实十 Node/十 Native/十 pairing 与三处权限
恢复通过，两个新 guard、fmt/clippy 另有原始输出。十二个执行源文件与最终 Git blob
的逐项 SHA 对应关系见 `path-head-binding.json`，精确 argv/env、负控和旧件继承见
`path-handoff-index.json`。这里的完整链属于新增路径护栏源码，不能反向追认早前尾差
已经执行过它；静态 fixture 来源边界和其他条件项仍由独立评审判断。

后续静态 SHA 锚与四个 Files 边界集合的联合版本，另用全新
`_build/20261016-120000-metadata` 跑实际 driver；原件为主仓 metadata 的
`provenance-fresh-verified/verification.json`。这轮包含新增静态 JSON 在内的13源前后
绑定、真实 Node/Native/pairing 以及权限恢复，见 provenance-handoff-index.json；
旧12源索引不改写。独立评审/CI/P0与139的治理结论仍不能由作者的成功运行代填。

动态 facts 的完整 Snapshot RPC 现在是 driver 的独立必跑阶段，位于静态三项 replay
之后、Native 之前；必须恰有一项测试及十个唯一 `PDRS_NUMERIC_SNAPSHOT_OK` 回执，
阶段前后继续核原 oracle 字节。此前13源真实链只包含静态 Snapshot 和动态 Native/
pairing，未覆盖动态 JSON→Snapshot 的完整 RPC；这些历史范围保持原样。

真实数字 `1792123157858.0955` 在默认 serde_json 解析下曾比 Node/标准 Rust 多一 ULP，
使 Snapshot 时间字段假 FAIL。gateway 的 dev-dependency 启用 `float_roundtrip`，
测试以独立 Node bits `0x427a142dd3d62187` 为锚，并逐个核对静态及动态 JSON 的数值
token（含请求/响应中的数值）与标准 double 解析。feature 在同版本 serde_json 的
test 构建图中统一；普通 lib 构建不含它，不将这项装置修复宣称为生产 parser 修复。
静态原件精确核384个数值；已登记SHA的两份动态原件精确核386个。未来不同形状录制
不套用386，另有固定七数值的对象/数组/嵌入request与response遍历canary，四种漏支
变异必须失败。driver的动态阶段还分别拒绝缺失、重复、未知回执，并核具体失败原因。
gateway Cargo.toml 纳入 driver 精确来源集合，现为14项；workspace feature、lock 与
录制 JSON 不变。observation.ok 在完整性和消费两门均须为布尔，missing/null/字符串/
数字/数组/对象的单轴负控分别拒绝，既有成功 API 闭包和失败 API 策略不变。
数值首次失败、修后原件重放及 feature 构建范围的原始输出见主仓 metadata 的
`numeric-check-results.json`；完整新录制链是否完成，以独立的 numeric 交接索引为准。


BND2输入交接：录制器每次真实chmod前先同步写出原path/mode计划到驱动pipe，写全后才改权限；最后将一次序列化的完整oracle同样送入pipe。驱动保留至多1MiB输出在内存，完整oracle与三项原permission计划必须一致，工作文件只作被验证副本。replay/Native前后、cleanup及最终报告前逐字节核对；文件改目标、改mode或删除都FAIL，仍按内存原计划恢复并stat原目标。报告记录原permission、completeOracle、artifactUnchanged、restored和真实回执，不从改过的JSON重新选期待。

record非零/超时/超输出时，即使尚无最终oracle或工作文件，已同步提交的permission计划仍在内存，finally按已发生步骤恢复1至3处。杀子进程后有界drain内核pipe中剩余字节，保留已写计划；直接子进程和持stdout后代控制见Python测试。旧`--restore fixture`缺原计划参数会拒绝；自动驱动把可信内存计划作为独立参数传给Node，standalone调用不能重新信任可改fixture来选择恢复对象。

该保证针对本批自有fixture数据跨阶段替换；目录/owner marker所有权检查继续执行。实际目录对象被移动/替换的恢复能力不由这批数据绑定证明。P0历史及独立终审仍按board处理。
