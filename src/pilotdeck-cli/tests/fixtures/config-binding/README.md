# CLI 只读 StaffDeck runtime binding

默认CLI配置预检现在沿Gateway已有loader执行`applyProjectBinding(..., runtime=true)`，不再把环境变量精确`true`直接拦为TODO。它只加载/校验后丢弃Config；远端Gateway仍决定实际运行的配置，CLI不会把本地module配置经WS传过去。本片不实现provisioning、身份认证、SOP/knowledge服务、绑定写入或本地Gateway fallback，不改变A8/阶段完成状态。

## 固定真实 TS 原件

基线为`cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，Node v22.22.0 / ICU78.2；生成器逐字核`loadPilotConfig.ts`和`projectBinding.ts`与Git对象，后者SHA256固定为`845bcf26546ff25901ce31aa93a588cd0971a639fe3af05cb8109f2e53fceca2`。

```sh
"$TASK_NODE_22" --import "$TASK_TS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-cli/tests/fixtures/config-binding/generate.mjs \
  "$TASK_TS_BASELINE" "$TASK_NEW_BINDING_OUTPUT"
python3 src/pilotdeck-cli/tests/fixtures/config-binding/probe-resources.py \
  --baseline "$TASK_TS_BASELINE" --node "$TASK_NODE_22" \
  --output "$TASK_NEW_RESOURCE_OUTPUT"
```

输出目录必须不存在。自写生成器在隔离HOME/PILOT_HOME/cwd直接调用真实`loadPilotConfig`，不是复制binding实现。fetch被禁止，调用前后逐格比较文件树hash；bootstrap HOME也为空。30格覆盖缺失/损坏/空/类型错误store、project/status/tenant/origin匹配、SOP缺失/合法/legacy id/空默认/损坏、binding活死锁/未ready/坏lock、非法path类型、孤立UTF16 agent URI、错误优先级。原件在任务目录，`oracle.json`只把独占根目录归一为`<root>`，输入project以`<project>`标记；锁`alive`用本次实际进程PID，`dead`以不可达int32正PID验证catch。结果保留TS完整错误和归一后的实际runtime modules JSON，不能由Rust回写。SQLite实验警告为实际导入时的Node输出，不代表打开了真实用户DB。

`nativeTodo:true`那格保留现有native对JS host依赖的显式拒绝：真实TS结果仍记录，不把TODO写成TS成功或修改oracle消息。孤立URI样例本身应精确报`URI malformed`。另直接复用已有冻结catalog的6格与diagnostics的8格enabled/type/prototype事实，不改旧原件。

资源探针实际执行原TS：无writer store FIFO在打印LOAD_BEGIN后3秒被外部父进程杀收；大于8MiB的普通合法配置被旧loader接受。两份原件metadata/字节均不变，不用玩具read函数代替真实loader。生成器、探针都是自己写的测试代码，未复制资源实现；不调用StaffDeck外网或用户目录。

## 有界配置 facade

Unix默认CLI显式选用新`load_project_bounded`；新增`load_snapshot_bounded`供新CLI/TUI入口串行接入。二者共享已有`load_project_with_files_inner`，没有复制binding或配置解析。既有Gateway两API和`NativeFiles`行为不变。非Unix默认CLI继续使用原`load_project`；新bounded API自身明确unsupported，Windows没有在本轮编译/运行验收，不能据cfg分支声称整crate已支持该平台。

新facade的所有read使用O_NONBLOCK打开、同fd检查普通文件；单文件最多8388608字节，每次加载最多33554432字节累计读取。元数据已超限会在读内容前拒绝，文件增长由limit+1有界读取捕获；实际部分读取与检测字节全部累计，不减计/重置。预算耗尽后不再尝试下一次read，即使它可能是空文件。typed资源拒绝记录首个fault，随后的read/stat/entries不delegate，process_alive直接false；旧loader退栈后只对该fault返回精确资源错误，普通IO继续用既有错误映射/优先级。read_source的未知IO提示会使其立即退栈，不把拒绝悄悄当成空配置。

精确错误为CONFIG_READ_NOT_REGULAR、CONFIG_READ_LIMIT、CONFIG_READ_BUDGET，requires_host=false。配置输入包括global/project YAML、binding store、SOP、lock；symlink可用，但最终打开的同一个fd必须为普通文件。仅约束这些文件读入字节和特殊文件打开；不宣称限制所有解析后对象内存、目录枚举或能强制中断故障文件系统内核IO，也不提供跨写者文件快照事务。

## mbp2 验证入口

所有Rust构建只在mbp2独立target、当前冻结源码运行：

```sh
CARGO_TARGET_DIR="$PWD/_build/cargo-target" cargo test --manifest-path src/Cargo.toml \
  -p pilotdeck-cli --bin pilotdeck config:: -- --nocapture
CARGO_TARGET_DIR="$PWD/_build/cargo-target" cargo test --manifest-path src/Cargo.toml \
  -p pilotdeck-gateway --lib config::bounded:: -- --nocapture
CARGO_TARGET_DIR="$PWD/_build/cargo-target" cargo test --manifest-path src/Cargo.toml \
  -p pilotdeck-cli --test config_exit
CARGO_TARGET_DIR="$PWD/_build/cargo-target" cargo test --manifest-path src/Cargo.toml \
  -p pilotdeck-cli --test config_binding_exit -- --ignored --nocapture
```

unit测试逐格核check结果、snapshot归一modules、错误优先级、现有requires_host和文件原字节不变；snapshot普通例全值对比原API。有界测试真实打开global/store/SOP/lock FIFO、超限稀疏文件，由父进程5秒限额保护定向子测试（双API都执行）；累计预算测试实际读4次8MiB普通文件，再拒绝额外读，并核fault后missing路径不是ENOENT、活PID不再查询。不能将列表中的ignored helper当作未执行，父测试要求逐phase回执。

真实CLI测试因legacy discovery固定18789而标ignored，只允许mbp2显式运行：先取得既有`/tmp/pdrs-cron-18789.lock`，bind失败就拒绝，不杀占用者，绝不在本机探测18789。隔离home/project/fake凭据，坏store/YAML/SOP/URI须完整错误、退出1、零连接；普通缺binding/bound/dead binding在自己持有的HTTP+WS fake中核hello/submit/完整输出，503明确保留本地fallback TODO/退出2。每步网络与子进程有期限，输入文件保持原件。这证明配置预检接线和远端选择，不证明远端采用本地binding modules，更不代填真实StaffDeck或真人实效。
