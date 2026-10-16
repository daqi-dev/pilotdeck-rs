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
