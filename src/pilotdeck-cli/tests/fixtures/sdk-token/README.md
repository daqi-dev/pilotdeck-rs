# SDK token 文件的有界读取

本片只改SDK `read_connection` 的文件回退分支：Unix使用非阻塞打开、同fd判断普通文件、最多65536字节原始文件输入。argv/env选择、URL、SDK transport、正常成功wire和其他入口不改。非Unix继续原读器，未做目标平台编译/运行验收，不宣称同样资源保证。cron/default remote/serve/TUI的读器各有自己的合同，本片不能作为它们已修的证据。

目录和普通IO错误仍按真实SDK `readFile(...).catch(() => "")` 变成旧token-required错误；空文件/空白文件同样处理。普通文件保留UTF8 lossy和JS trim；超限、FIFO和其他成功打开的特殊文件明确失败，不把它们当认证成功。限制的是文件原始字节，不是trim后的token长度；恰65536允许，多1拒绝，增长由实际limit+1读取截获。此保证不能强制中断故障文件系统内核IO，也不承诺与原文件写者的事务快照。

## 固定真实 TS 采集

```sh
"$TASK_NODE_22" src/pilotdeck-cli/tests/fixtures/sdk-token/generate.mjs \
  "$TASK_TS_BASELINE" "$TASK_NEW_TOKEN_OUTPUT"
```

生成器拒绝已存在目录，固定Node v22.22.0 / ICU78.2及TS HEAD `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`；逐字核sdkCli.ts、pilotdeck.ts、authToken.ts的Git对象。SDK安装为本基线packages/sdk的symlink，版本0.1.0-alpha.0；实际dist client/transport/index hash也记录在oracle。子进程是真实完整 `pilotdeck.ts sessions list --gateway-url <owned-dynamic-url>`，不是截取read函数；env全隔离HOME/PILOT_HOME/TMPDIR/cwd，不继承用户key/proxy，NODE_NO_WARNINGS=1明确控制实验警告，不删应用错误输出。WS只绑定自己127.0.0.1:0，不访问18789或真实用户目录/服务。

19格原始结果覆盖普通Unicode/UTF8损坏/trim、空文件/缺失/目录/EACCES、65536/65537、多字节边界、无writer FIFO、立即和延迟writer FIFO，以及argv/SDK env/generic env优先级。特别空`--auth-token ''`被真实readValue的truthy校验视为缺省，**会回退env**；空SDK/generic env是present-empty，**不回退**。argv/env空白token保持不trim，只有文件分支trim。实际65537字节token完整进入hello并完成list；无writer FIFO在2.5秒仍无连接、只杀收本次子PID；延迟writer例先3秒确认无连接，再写入并成功，证明不是导入慢被误当FIFO阻塞。

raw.json保留完整stdout/stderr/退出/信号、实际请求UUID、原始帧长度/hash、token摘要、文件metadata和时长。假凭据仅由生成器创建；hello token以utf8字节数/utf16单位数/SHA256记录，避免日志复制token。归一oracle只替换请求UUID，保留其余帧字段、精确token摘要、原hello hash和完整输出；helloRaw附加自同轮rawHashes[0]，不是从Rust反推。FIFO由自己的producer写入会改变mtime，原件核inode/type/mode/size；普通文件另用预先打开的fd核原字节和完整记录metadata不变（不比较读入必然可变的atime）。首轮误把FIFO writer的mtime变化当改写而中断，原始失败目录保留，成功重采未覆盖它。

## Rust 验证

```sh
CARGO_TARGET_DIR="$PWD/_build/cargo-target" cargo test --manifest-path src/Cargo.toml \
  -p pilotdeck-cli --bin pilotdeck sdk:: -- --nocapture
CARGO_TARGET_DIR="$PWD/_build/cargo-target" cargo test --manifest-path src/Cargo.toml \
  -p pilotdeck-cli --test sdk_token_exit -- --nocapture
```

仅在mbp2当前冻结源码与独立target运行，本机禁止Rust构建。四个unit测试用真实文件/fd检查普通IO和原codec、边界、fstat后实际增长、fstat后路径替FIFO仍读取原fd。hook是私有构造参数，生产传空closure，没有公开env/flag后门。资源拒绝和普通IO分别有精确完整错误，原文件不写入。

集成测试19格逐项执行真实Rust CLI：正常路径完整stdout/stderr/rc、hello/请求字段和精确token摘要与TS比较，记录Rust原hello digest；保留既有sessions serializer，不在此重定义D1的JSON字节顺序。四格标safety的TS成功/挂起事实保持原样，Rust按独立安全差异断言退出1、完整拒绝文本、零连接。TS delayed-writer行的3秒时间线只由真实TS采集执行；Rust拒绝测试预写并持有active FIFO，验证拒绝且未消费pipe内容，**没有复放3秒延迟时序**。

另以run、resume、sessions、settings get/set五条实际命令，在无writer/active writer FIFO和超限文件共15组合核有界退出、零连接、输入metadata/pipe内容保留。每步WS和子进程有deadline，失败只杀收自己的PID。测试stdin为null且run/resume提供prompt，只证明配置读取/不连网，不据此声称开放stdin的等待顺序；既有SDK preflight/turn/sessions/settings回归继续执行。所有输入为假凭据，生产错误不包含token内容或路径。

安全差异 `D1-SDK-TOKEN-READ-BOUNDS-001` 未批准，不是冻结评分豁免；真实provider、SDK实际认证服务和真人实效不由本mock代填。


## 本轮自动验证结果

mbp2 r1冻结快照的严格clippy、fmt和构建通过：SDK unit31通过/1既有ignored，preflight4，resume12/1既有ignored，run19/3既有ignored，sessions26/2既有ignored，settings16/1既有ignored，新token集成2通过（内部19金样与15拒绝组合）。既有ignored是此前平台/重生成入口，本轮没有冒充执行；本片新token测试无ignored。真实Rust binary SHA256为`c13aec20dc34a15aeb848b15468f6a50da15e40b316b94d907ff8facdc92f32f`；五个编译输入postfmt hash已与mbp2逐项核对。

原始验证日志/源hash/构建证明在主仓`_tmp/20261108-cli-sdk-token/r1-evidence/`，TS原件另归同任务目录；安全差异正本为`specs/gateway-parity/diffs/D1-SDK-TOKEN-READ-BOUNDS-001.md`。这次AI隔离实测不替代真实服务或人签字。
