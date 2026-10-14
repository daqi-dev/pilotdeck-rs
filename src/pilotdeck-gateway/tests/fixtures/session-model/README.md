# session_model_get：默认原生链的 Node 原件

固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，Node 22.22.0 / ICU 78.2。
`cases.json` 的 193 个请求/response 字符串来自实际 GatewayWsConnection 编码，
领域能力采用默认 GatewaySessionModelBundle、Native selection port/policy、
ProjectSessionDataPlane persistence/catalog 和 GatewayDialogBundle resolver。
仅 WS transport 是无 listener 的离线录制接口，router spy 触碰就抛错，最终 calls=0。
没有用手写预期替代 Node 回包。`load` 另录真实项目配置加载器的完整诊断。

目录路径用 `/fixture` 替换隔离的每个案例根；relative-project 请求按同一个映射
使用 `cwd=/fixture, projectKey=project`。该映射也应用于文件字节中的 marker、
binding root 和所有回包/诊断路径；其他内容与字段顺序不归一化。文件 bytes 为
base64，保留坏 UTF-8。50MiB+1 案例只记 stat，原件全为 x；测试读取边界会忽略
该 transcript，catalog summary 也为空。fixture PID 是录制器自有子进程，
mandatory snapshot 测试只能模拟该 PID，绝不探测归档 PID 对应的当前进程。

覆盖：项目/General/相对路径/未知项目、session 不存在和非法 key、saved model/
auto/历史 reasoning 过期/speed/退役字段、metadata merge/clear/accepted_input，
损坏行/截断/非 UTF-8/超限/目录，以及 conversation、turn-summary、metadata、
compact-boundary 的错误顺序和 compact 截断边界。配置包括项目覆盖、环境优先、
非法 YAML/根/其他配置节、router、binding 各 status/runtimeReady、identity 不符、
SOP id/skill_id/空 bundle/非法 YAML。catalog 两项专门证明不传 projectRoot 时
忽略项目配置和 binding store，既有全局配置 fixtures 也保持不变。

锁矩阵用同一自有 Node 子进程测试纯数字、JS 空白含换行/BOM、空串、0/负数、
小数、0x/0o/0b、科学计数、Infinity、非法输入、安全整数/int32 上界、读失败、
目录、坏 UTF-8。退出只关闭 stdin，让子进程自然退出，再测同一 PID 的死锁。
EPERM 在 Node 的 `_kill` 边界注入 -1，未调用 OS；Rust fixture 记录为 false，
NativeFiles 的 syscall 结果使用 is_ok，不区分 errno。另有真实 libc 的自有子进程
alive→自然退出→dead 测试。
每个案例分别记录 RPC 的 `process.killCalls` 和直接 loader 的
`process.loadKillCalls`；Rust 的 SnapshotFiles.calls 对两条路径均逐项断言。
存活、已退出和 EPERM 各探测一次；非法 PID 在 syscall 前拒绝，调用次数为零。

第 3 轮增加 19 项：三条保留环路径（provider extraBody、adapter extra、外部
tools catalog）的完整覆盖、未覆盖和部分覆盖；provider/adapter 还区分只覆盖
其他键与覆盖 self。双源都递归的 merge 仍报 RangeError；未被消费的环不报错。
普通双源按 TS deepMerge 的浅复制、object/object 递归、数组替换语义保留引用。
双源同时涉及环和 `__proto__` 时保守转发 TS，`loadForward` 对 loader 也断言
requires_host；这个边界不算作本地 wire 命中。

NUL 案例覆盖 General/注册项目的 sessionKey、projectKey、配置路径及 bindingId。
sessionKey 在首次 transcript stat 时报 ERR_INVALID_ARG_VALUE；projectKey 在项目
解析处报 PROJECT_NOT_FOUND；后两条 existsSync 路径按不存在处理。配置路径 NUL
只能存在于直接传入的 env 对象，OS 环境变量不接受 NUL，因此只作为单元控制。
`invalid-paths.json` 另录 82 个真实 Node fs 错误，覆盖引号选择、控制字符、Unicode、
孤立 surrogate、长字符串/换行和 128 UTF-16 单元截断；由
`record_invalid_paths.mjs` 生成，没有手写 message。

NUL 错误的 inspect 截断与路径替换不可交换。录制器先保存原始 WS frame 到 scratch
的 `cases/<index>/nul-wire-original.json`，再用真实 Node fs 校验映射后的 invalidPath；
只有 message 的路径表示重新录制，code 和 WS 包装保留。NUL 校验在 syscall 前失败。
真实 wire 检查同样向 Node fs 传入该次运行的完整路径，逐字节比较完整 frame；
不会通过删除或放宽 message 比较掩盖截断差异。

`numbers.json` 由 `tests/record_binding_numbers.mjs` 实测 53 类 Number/Node PID
边界；全部 `_kill` 调用被注入接管，所以不探测任何真实 PID。最大 int32 被接受，
2147483648、4294967296 以及安全整数上限等被 Node 在 syscall 前拒绝。

排序遵循 D6 C：同 sequence 的 createdAt 才是 localeCompare 排序键；任一需比较
的时间戳非 ASCII 则原始帧交 TS。不同 sequence 的非 ASCII 时间戳继续本地。
历史 reasoning 恢复调用全局 catalog，继承其 D6 与 JS_HOST_REQUIRED 回退。
这些内部路由信号不进入 wire；未加排序差异豁免。

复现（仓根运行，选全新 scratch；不下载依赖）：

```sh
A4_CASES="$(mktemp -d /private/tmp/a4-session-model.XXXXXX)"
A4_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
mkdir -p "$A4_CASES/isolation/home" "$A4_CASES/isolation/xdg" "$A4_CASES/isolation/tmp"
env HOME="$A4_CASES/isolation/home" \
  XDG_CONFIG_HOME="$A4_CASES/isolation/xdg" \
  XDG_CACHE_HOME="$A4_CASES/isolation/xdg" \
  XDG_DATA_HOME="$A4_CASES/isolation/xdg" TMPDIR="$A4_CASES/isolation/tmp" \
  PILOTDECK_SKIP_BROWSER_OPEN=1 SKIP_BROWSER_OPEN=1 \
  node --import "$A4_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/record_session_model.mts "$A4_BASELINE" \
  "$A4_CASES/cases.json" "$A4_CASES"
env HOME="$A4_CASES/isolation/home" \
  XDG_CONFIG_HOME="$A4_CASES/isolation/xdg" XDG_CACHE_HOME="$A4_CASES/isolation/xdg" \
  XDG_DATA_HOME="$A4_CASES/isolation/xdg" TMPDIR="$A4_CASES/isolation/tmp" \
  PILOTDECK_SKIP_BROWSER_OPEN=1 SKIP_BROWSER_OPEN=1 \
  node src/pilotdeck-gateway/tests/record_invalid_paths.mjs "$A4_CASES/invalid-paths.json"
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --locked --offline
cargo build --manifest-path src/Cargo.toml -p pilotdeck-gateway --locked --offline
python3 src/pilotdeck-gateway/tests/check_session_model_wire.py \
  --output "$PWD/_tmp/session-model-native-$(date +%Y%m%d-%H%M%S)"
```

真实二进制检查使用 127.0.0.1:28860/28861，只启动自己的 Rust proxy 和一个自有
锁持有子进程。189 个本地案例均须匹配 source oracle 的 wire bytes；随后发送
非本地的 observer-fence，观察上游只有 fence，证明请求已被处理而未转发。
D6/原型 fallback 由 pairing 的原始帧对照验证，EPERM 和 NUL env 对象由单元
fixture 验证；这四项不计入 189 个真实二进制案例。fixture 哈希、原始请求/响应、observations、进程 argv/
PID/结束状态写入输出目录。fixture 源码被更新时必须用新目录重跑。

第 4 轮的 `io-errors.json` 另录 **39** 个真实 Node 文件系统错误案例，原有
193 个 `cases.json` 原件保持不变。新增案例仍调用默认 native provider 和真实
GatewayWsConnection；另外比较项目 loader 的完整诊断。每项 `io.observations`
保留实际 Node 的 code/syscall/message/path；只有案例根映射为 `/fixture`。

| 调用点 | Node 行为及 Rust 对照 |
|---|---|
| transcript stat | EACCES / ENOTDIR 报 `stat 'path'`；ENOENT 被 reader 当作缺失，RPC 返回默认选择。 |
| transcript readFile | EACCES / ENOTDIR 报 `open 'path'`；ENOENT（含 stat 后消失）仍当作缺失；EISDIR 报 `illegal operation on a directory, read`，不带路径。 |
| default/project existsSync | EACCES / ENOENT / ENOTDIR 均返回 false，不传播底层 stat 错误；后续配置诊断优先级保持 Node 行为。 |
| default/project readFileSync | CONFIG_READ_FAILED 诊断 hint 保留 `open 'path'`；目录读取 hint 为 EISDIR/read。 |
| binding store existsSync/readFileSync | exists 失败视作无绑定；read 失败统一 PROJECT_BINDING_STORE_INVALID，不暴露底层 syscall。 |
| SOP bundle existsSync/readFileSync | exists 失败使用空 bundle；read 失败传播真实 Node code/message：EACCES / ENOENT / ENOTDIR 为 open，EISDIR 为 read。 |
| binding lock readFileSync | 所有读取失败均返回 false，不调用 kill、不暴露底层 syscall。 |

权限案例通过 chmod 实际制造 EACCES，要求非 root。read ENOENT / ENOTDIR 竞态
在真实 stat/existsSync 成功后移除文件或将父目录替换成普通文件；lock 的 ENOTDIR
在进入锁读取时才替换父目录，避免提前遮住 binding store。hooks 只调度自有文件
变更，不注入错误、syscall 或 message；两次调用各恢复再构造同一场景。
Rust SnapshotFiles 按录制的 statError/readError 重放对应阶段，逐字节比较 RPC
response，同时比较有序 loader diagnostics；39 项均为必跑测试。

`webui-spread.json` 录制 **14** 个真实 applyProjectBinding 输出，包含启用/禁用
两分支的空数组、混合数组、12 项索引顺序、字符串 UTF-16 单元、null/number 和
已有 staffdeckCopy 的对象。Rust 测试调用完整 binding::apply，比较 webui JSON，
因此即使 session_model_get 不输出 webui，也会检查数组的 "0","1"… 键。
字符串展开产生孤立 surrogate，使用 protocol 的 UTF-16 JSON codec 读取原件。

复录本轮新增原件（非 root、仓根运行、全新 scratch；复用既有 Node 22/tsx）：

```sh
A4_IO="$(mktemp -d /private/tmp/a4-session-io.XXXXXX)"
A4_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
mkdir -p "$A4_IO/isolation/home" "$A4_IO/isolation/xdg" "$A4_IO/isolation/tmp"
env HOME="$A4_IO/isolation/home" XDG_CONFIG_HOME="$A4_IO/isolation/xdg" \
  XDG_CACHE_HOME="$A4_IO/isolation/xdg" XDG_DATA_HOME="$A4_IO/isolation/xdg" \
  TMPDIR="$A4_IO/isolation/tmp" PILOTDECK_SKIP_BROWSER_OPEN=1 SKIP_BROWSER_OPEN=1 \
  node --import "$A4_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/record_session_io.mts "$A4_BASELINE" \
  "$A4_IO/io-errors.json" "$A4_IO"
```

本轮真实 proxy 检查现在同时读取两个 fixture，预期 **217** 项本地 wire 匹配
（原有 189 + 新增 28）。11 个精确控制读取时机的竞态由必跑单元 fixture 验证，
不计作真实 binary wire 命中；连同原有四项控制，在 observations.json 明确列出
15 个 excluded ID。稳态权限、路径缺失、非目录和目录读取案例均进入实际 binary
检查；每项上游仍只允许 observer-fence。新增 ioFixtureSha256 与原有 fixtureSha256
分别保存两个原件哈希。前文 wire 命令仍可使用，须非 root 且选择全新输出目录。
新增 39 项的 RPC/load killCalls 在真实 Node `_kill` 边界分别计数，均为空；
任何意外 PID 探测都会使录制失败，不会访问归档或用户进程。

## 第 5 轮：完整 Node errno 表与 ENAMETOOLONG / ELOOP

公共映射和生成流程见 `../node-io/README.md`；`node_io` 是 session、config、
binding 和 project_files 的唯一底层 io code/message 表。所有未映射的 errno
（包括真实 EDQUOT）和没有原始 errno 的 Rust 错误都转发整请求。已知错误保留
上表的吞错/包装语义，未知错误也在 exists/store/lock 的吞错边界转发，不猜测
Node 的返回。project_files 的 NOFOLLOW/ELOOP 安全拒绝规则保持本地。
model_catalog 通过同一 global config loader 获得相同映射和转发边界。

`io-errors-round5.json` 独立新增 **34** 个真实 Node 场景；原有 `cases.json`、
`io-errors.json`、`webui-spread.json` 保持原字节。录制器仍实测全部 73 项，
`--round5` 只导出本轮新增项。新增覆盖 transcript/global/project/store/SOP 的
stat/open ENAMETOOLONG、自环 symlink ELOOP、80 层 symlink 链 ELOOP；lock 另有
三类 read 失败。独立长 sessionKey 案例使用原样 `"web:" + "x".repeat(300)`。
read 竞态在真实 stat/existsSync 成功后替换叶子；只有文件树变更 hooks，没有
注入 errno/message。RPC 和直接 loader 分别重建文件树，各自记录真实错误。

复录（Node v22.22.0、非 root、全新隔离目录）：

```sh
A4_IO="$(mktemp -d /private/tmp/a4-session-io-r5.XXXXXX)"
A4_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
mkdir -p "$A4_IO/isolation/home" "$A4_IO/isolation/xdg" "$A4_IO/isolation/tmp"
env HOME="$A4_IO/isolation/home" XDG_CONFIG_HOME="$A4_IO/isolation/xdg" \
  XDG_CACHE_HOME="$A4_IO/isolation/xdg" XDG_DATA_HOME="$A4_IO/isolation/xdg" \
  TMPDIR="$A4_IO/isolation/tmp" PILOTDECK_SKIP_BROWSER_OPEN=1 SKIP_BROWSER_OPEN=1 \
  node --import "$A4_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/record_session_io.mts "$A4_BASELINE" \
  "$A4_IO/io-errors-round5.json" "$A4_IO" --round5
```

快照中 file size 保留真实 scratch 路径字节长度（例如 marker、binding JSON）；
不同 scratch 长度可改变这些事实，不改变根映射后的 wire/diagnostics。全部
73 项 RPC response 与 loader diagnostics 都是必跑逐字节对照。未知 errno
对 transcript/global/project/store/SOP/lock 的 stat/read 边界执行 33 项必跑
控制；另在同一实际 pairing 中验证 session/catalog/project_file_read 原始
请求帧（含扩展字段、空白、126/127 长度编码和 mask）完整转发及 TS 回包透传。

真实 proxy 检查读取三个 fixture，预期 **236** 项本地 wire 匹配：原有 217 +
本轮 19 稳态案例。新增 15 个精确读取竞态由必跑单元 fixture 验证，连同此前
15 项，在 observations.json 明确列出 30 个 excluded ID。每项本地请求的上游
仍只允许 observer-fence；新增 ioRound5FixtureSha256 独立绑定第三份原件。
前文的 wire 命令仍可使用，须非 root、28860/28861 空闲、全新输出目录。


## 第 6 轮：显式 method errno 白名单

`session_model_get` 的本地 errno 集合为 ENOENT/EACCES/ENOTDIR/EISDIR/
ENAMETOOLONG/ELOOP，均来自第 4/5 轮生产原件；已有 73 项 RPC/loader 原件
保持原字节。完整 libuv 格式表不再隐式授权其余 errno。config 共同消费面
（包括 model_catalog_list）沿相同六项准入，既有吞错/包装语义不变。
11 个 fs 边界的必跑转发控制从三种未知错误扩成所有「已映射但无 method
原件」的 errno，加 EDQUOT、123456 和无 errno 错误；实际 pairing 另验证
EIO 的原始帧转发与 TS 回包透传。白名单与生产原件观测集合相等有独立断言。
