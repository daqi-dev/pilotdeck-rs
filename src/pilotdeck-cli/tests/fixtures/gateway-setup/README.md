# Gateway setup：原生平台配置

本目录验证 Feishu/Lark、Weixin 与 WeCom 平台配置：手动输入、既有凭据复用、平台别名、菜单全部平台顺序及 QR 的隔离 mock。这些切片不代表 #124、D2、真实服务登录或真人终端实效已完成。

参照固定为 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的 `src/cli/commands/gatewaySetup.ts`。运行器固定 Node v22.22.0 / ICU 78.2，序列化包为基线安装的 `yaml@2.9.0`。实际安装 `@larksuiteoapi/node-sdk@1.65.0` 的 `default ?? module` 没有 `AppTicketManager` 导出，因此本切片走原版实际可达的手动凭据流程，不声称实现了扫码创建应用。

## Rust 自动验证

在有可用 cargo 槽位的主机、当前源码快照独立 target 中运行：

```sh
CARGO_TARGET_DIR="$PWD/_build/cargo-target" cargo test --manifest-path src/Cargo.toml \
  -p pilotdeck-cli --bin pilotdeck gateway_setup:: -- --nocapture
```

这些测试使用独占临时目录、虚构凭据和进程内 fake Fetch。HTTP 生产实现的测试只连接自己绑定的 `127.0.0.1:0`：验证真实请求体、Content-Type、302 的 POST→GET、同 origin 307 保留请求体、跨 origin 307 不产生第二端请求、JSON 错误文案和非成功 HTTP 状态下的字段判断。HTTPS 降级拒绝规则另有直接 URL 判据测试。不调用真实飞书、Lark 或用户的服务，也不使用 18789。

YAML 测试从原始输入生成配置并调用实际 `write_adapter`，核对输出字节和重读值。覆盖普通值、特殊字符串、键序、空容器、多行、图别名、循环、Set / Map 和字面反斜线与真实控制字符。保存拒绝路径检查原文件字节或目录内 sentinel 不变。实际 `ulimit -f 0` 子进程还验证写阶段错误准确为 `EFBIG: file too large, write`，并验证原子保存下旧文件原字节保留、临时文件清理；首包曾在同一装置下实测旧文件被截断，该历史原件保留于任务目录。该子测试由父测试定向启动，正常列表中标为 ignored。

## PTY 四面复放

`compare-pty.py` 创建独占输出目录，每个 TS / Rust 子进程有独立 HOME、PILOT_HOME、配置路径、cwd 和 PTY；输出目录已经存在即拒绝运行。它先拒绝错误的基线 HEAD、修改过的 setup 源文件或 Node / ICU 版本，并记录原始源码与 Rust 二进制 SHA-256。请另保存源码快照 SHA / dirty diff 与构建命令，二进制 hash 本身不能证明源码来自当前 HEAD。

先构建当前 Rust 二进制，再按实际绝对路径调用。以下 `$TASK_*` 变量需指向自己的隔离基线、Node 和当前二进制：

```sh
python3 src/pilotdeck-cli/tests/fixtures/gateway-setup/compare-pty.py \
  --baseline "$TASK_BASELINE" --node "$TASK_NODE" --rust "$TASK_RUST_BIN" \
  --output "$TASK_NEW_PTY_OUTPUT"
```

另以新的输出目录加 `--term dumb` 复放，不能复用或覆盖第一次的原始记录。八个场景为普通回车、退格、方向键与 Delete、Ctrl+C、Ctrl+D、Ctrl+U、中文编辑和 SIGTERM。每例逐项比较完整原始字节、退出码、终端属性恢复、输入 raw/echo 属性、无落盘、10 秒期限；不删 ANSI、不归一化 CR/LF、不容忍不同退出码。PTY 暂时 EIO 后仍只等待原 deadline，不能把退出收割竞态当成超时，也不能延长预算。

边界：TS 通过自写 `pty-driver.mjs` 直接调用真实 `runGatewaySetup(['feishu'])`，Rust 使用真实完整 bin。此装置没有运行旧 `pilotdeck.ts` 的全局代理安装或 server/default 启动编排，不能作为完整旧 bin 对拍。所有输入均为空凭据 / 取消，driver 还拒绝任何 fetch；因此不会外发请求。正常使用、编辑和退出属于 AI 可实测；真人终端体验仍需单列人眼验收。

## YAML 原件重生成

这些生成器只加载固定基线的公开 YAML API，在内存中对自写输入做 parse / stringify。它们不修改基线源码、产品配置或用户目录，不复制基线实现；输出路径必须不存在。

```sh
"$TASK_NODE" src/pilotdeck-cli/tests/fixtures/gateway-setup/generate-yaml.mjs \
  "$TASK_BASELINE" "$TASK_NEW_YAML_OUTPUT"
"$TASK_NODE" src/pilotdeck-cli/tests/fixtures/gateway-setup/generate-escapes.mjs \
  "$TASK_BASELINE" "$TASK_NEW_ESCAPES_FILE"
```

`generate-yaml.mjs` 从各 fixture 的输入字段重算 `yaml-corpus.json`（61 格）、`graph-corpus.json`（21 格）和 `leading-space.json`，并记录输入 hash；预期部分不会用 Rust 结果重写。`generate-escapes.mjs` 单独重算 8 格字面反斜线 / 真实控制字符输入。复杂 key 的 Node warning 是原版事实，应保留生成器 stderr。重生成后逐字节比较文件，出现差异先排查版本或输入变化。

## 安全差异与限制

本切片按默认修 bug 授权，对存在但不可读 / 无效 UTF-8 / 无法解析的配置拒绝保存；root / adapters 数组等非 mapping 不再显示写入成功；带凭据 body 的跨 origin 重定向以及任何 HTTPS→HTTP 重定向被拒绝。同 origin 307 / 308 保留 body，302 / 303 转 GET 移除 body 后可以跨 origin。差异登记由 D2 持有人维护；`graph-corpus.json` 保留 TS 原始事实，Rust 的拒绝断言不篡改该 oracle。

## 原子保存切片

配置保存先检查目标形态和读写权限，再在实际目标目录以独占 UUID 名创建临时文件。临时文件初始为 0600；检查继承元数据后才写内容。完整写入、保留受支持的旧文件 mode / uid / gid、同步文件并复核目标身份后，才以同目录 rename 发布。新配置最终保持 0600；已有配置保留原权限，无法保留则拒绝。发布前任一步失败都会清理该次临时文件，旧文件不被截断；rename 是最后一个可失败步骤，不把发布后的错误误报成旧文件仍在。

已有有效符号链接会解析到实际目标，并只替换目标文件；符号链接本身与其 inode 保留。断链、hardlink、非普通文件、特殊权限位、macOS 非零 BSD flags、任何 xattrs / ACL 都明确拒绝。macOS 使用固定系统 `/bin/ls`、`LC_ALL=C` 检查 ACL，命令失败也拒绝；检查 xattrs 在先，避免 `@` 覆盖 ACL 的 `+` 标记。Linux ACL 由 xattr 拒绝路径涵盖。此实现保留上述 mode / uid / gid，**没有声称保留任意平台元数据**；不支持的形态保持原件并返回错误。

真实回归包含 chmod mode、symlink、hardlink、xattr 注入、macOS ACL 与 `chflags hidden` 注入，以及实际 `ulimit -f 0` 写失败。还验证检查后目标被另一个写者改动时拒绝发布；新目标使用 NOREPLACE 避免覆盖同时出现的文件。这些检查不构成与所有旧写者的互斥锁：持有旧 inode 的写者以及最后一次检查与 rename 间的竞态仍有边界。文件 sync 不等于目录项已跨崩溃持久化，本切片不宣称完整断电耐久性。

## WeCom mock 金样

`wecom-oracle.json` 的 10 格为真实 TS `runGatewaySetup` 函数直接执行的手动、复用、取消、菜单、QR 成功 / 重试 / 格式异常 / HTTP 错误 / 缺凭据 / 超时及重新配置输出；`wecom-boundaries.json` 另追加孤立 UTF-16 scode 和深嵌套共享 / 循环 groups 两格，旧金样不被新样本覆盖。网络全面替换为 fake fetch，轮询使用 mock 时钟；没有调用企业微信服务。Rust 逐格核对完整 stdout、结果、配置原文字节、请求序列和时钟，不把 substring 命中当作完整输出一致。

孤立 surrogate 必须在生成扫码展示链接前按 JS `encodeURIComponent` 抛出 `URI malformed` 并转手动输入，不能替换成 U+FFFD 后再发 query。共享 groups 源自最初读取的图，保存时另读的新图与它不应被强行共享对象身份；组内部 alias/cycle 和 YAML 的重复引用编号顺序另由实际金样约束。HTTP 非成功状态在 JSON 解析前拒绝；Feishu 原先忽略 HTTP status 的判断不受此接口影响。

```sh
python3 src/pilotdeck-cli/tests/fixtures/gateway-setup/generate-wecom.py \
  --baseline "$TASK_BASELINE" --node "$TASK_NODE" --output "$TASK_NEW_WECOM_OUTPUT"
python3 src/pilotdeck-cli/tests/fixtures/gateway-setup/generate-wecom.py \
  --baseline "$TASK_BASELINE" --node "$TASK_NODE" --output "$TASK_NEW_WECOM_BOUNDARY_OUTPUT" --boundaries
```

生成器与 `wecom-driver.mjs` 为自写测试代码，先核固定 TS 源码字节和 Node/ICU，再在独占 HOME/PILOT_HOME/config/TMPDIR 下运行原函数。它只收集本切片配置副作用，不覆盖旧 bin 的全局代理启动。WeCom 自身保存失败另有真实 `ulimit -f 0` 子进程，分别在旧凭据复用与 QR 拿到凭据后检查失败结果、成功提示不出现、两份原件原字节及临时清理；辅助子测试由父测试通过回执确认实际执行。


## Weixin SDK mock 金样

`weixin-oracle.json` 14 格调用真实 TS setup 与安装的 `weixin-ilink@0.1.0`，覆盖旧凭据复用、重新登录、取消、扫码回调、过期刷新、缺凭据、HTTP 失败、Abort 后重试、孤立 UTF-16 qrcode、损坏旧凭据及自定义 PILOT_HOME 缺父目录。`weixin-all.json` 另录菜单 4：Feishu → Weixin → WeCom 的完整输出和取消顺序。Rust 逐格比较完整 stdout、结果、两份文件原文字节、请求/headers、时钟和 legacy HOME 目录副作用。

```sh
python3 src/pilotdeck-cli/tests/fixtures/gateway-setup/generate-weixin.py \
  --baseline "$TASK_BASELINE" --node "$TASK_NODE" --output "$TASK_NEW_WEIXIN_OUTPUT"
python3 src/pilotdeck-cli/tests/fixtures/gateway-setup/generate-weixin.py \
  --baseline "$TASK_BASELINE" --node "$TASK_NODE" --output "$TASK_NEW_WEIXIN_ALL_OUTPUT" --all-only
python3 src/pilotdeck-cli/tests/fixtures/gateway-setup/generate-weixin.py \
  --baseline "$TASK_BASELINE" --node "$TASK_NODE" --output "$TASK_NEW_WEIXIN_FAILURE_OUTPUT" --write-failure
```

生成器先核固定 TS SHA/源码字节、Node v22.22.0 / ICU 78.2 与 SDK 0.1.0；输出目录必须独占新建。自写 driver 直接调用原函数、替换全部 fetch 与 1 秒轮询计时器，35 秒 SDK abort 保留；不运行旧完整 bin 的全局代理安装，不调用真实微信网络。HOME/PILOT_HOME/config/cwd/TMPDIR 均隔离；版本与 oracle 不能由 Rust 结果重写。源码复用限制见 `weixin.rs`：现有 Gateway public opaque login 无法插入所需原版错误文案与严格 UTF-16 URI 处理，本包只复用其公开默认 URL，复用共同原生 HTTP/TLS、JS codec 与原子保存，未编辑 Gateway 作者占用文件。

`--write-failure` 只对子进程设真实 RLIMIT_FSIZE=0、忽略 SIGXFSZ 并禁用 tsx 磁盘缓存。原版在两次 fake 请求成功后将旧凭据截断为零，配置原件不变；`weixin-write-failure.json` 保留该事实与完整失败输出。事件回执改经 stdout 管道返回，避免写失败影响装置本身。Rust 对应父测试实际启动同样大小限制的辅助子测试，逐字比较失败输出并确认旧凭据和配置保留、无成功提示和临时残留；不是以构造 IO 错误代替真实失败。

Weixin 独立安全边界：凭据以非阻塞 open 后检查同一 fd，只接受普通文件，最大 1 MiB；文件增长也由 limit+1 有界读取截获。非普通/超限/读取失败明确拒绝，旧凭据与配置保留。无 writer FIFO 与超限稀疏文件由父进程的 3 秒期限保护真实子测试；超时会杀收子进程并失败。JSON 本身损坏时仍按基线允许用户明确选择新扫码登录，正常凭据不会因资源边界被静默清空。

iLink QR/status 的响应独立限制为最大 1 MiB、最终 headers 到完整 body 最多 30 秒，期限不因 chunk 续期；poll 的 35 秒 abort 仍只覆盖 headers，body guard 是明确失败而非 Abort 重试。真实 loopback 测试以构造参数缩小同一生产 guard 的 limit/timeout，发送超限 body 与持续每 5ms 发数据的慢流，覆盖 initial QR 和 poll，并确认旧凭据/配置不写入。Feishu/WeCom 的既有传输语义不因此收窄。差异见 `D2-WEIXIN-CREDENTIAL-ATOMIC-001` 与 `D2-WEIXIN-RESOURCE-BOUNDS-001`；它们未批准为冻结评分豁免。


`probe-weixin-resources.py` 使用同样的固定版本与独占目录，真实执行旧setup：父进程2秒杀收无writer FIFO挂起子进程，另证明1MiB以上有效凭据仍进入原版复用提示；不读FIFO来采集结果，不访问外网。调用参数与生成器相同。其原件只归档任务目录，不作为Rust正常金样重写。
