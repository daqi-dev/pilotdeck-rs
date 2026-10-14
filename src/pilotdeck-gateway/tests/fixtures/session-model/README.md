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
