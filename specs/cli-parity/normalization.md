# CLI D0-pr1 比较口径（未冻结）

本文件先于实现登记。仅用于首批离线定标，不批准任何 Rust 行为差异。

1. **stdout / stderr 原件不归一化**：保存原始 bytes，包括换行、ANSI、空白、错误文本；除第 6、7 条明确授权的字节归一化外，每路内部顺序和完整字节精确比较。另存双 pipe 的采集事件，合并相邻同流 read 后，默认比较流名与 bytes；不比较 read 分块大小、采集时钟。这个事件序列是观察到的跨流顺序，不能证明内核 write 的全序。只有符合第 8 条且逐场景声明的例外才不比较跨流事件顺序。
2. **退出码精确比较**：信号产生的负值保留，超时/截断/采集或清理失败属于装置错误，不能因两侧相同而 PASS。
3. **完整落盘**：对唯一可写 runtime 的初始及最终树逐项记录相对路径、类型、mode，普通文件的长度和 SHA-256、软链接的目标。含空目录、隐藏文件；不跟随软链接。初始树必须与场景 fixture 一致，最终树重新从原件读取，拒绝与缓存 manifest 不符。增加/删除/内容/类型/权限/链接变化都比较；不只比较预期文件。mtime/ctime/atime、inode、uid/gid 不属于本轮内容契约，不参与比较。特殊节点、越界链接、硬链接一律拒绝。
4. **实际输出的路径不替换**：两次 TS 与可选 Rust 均顺序复用同一个 runtime 绝对路径，结束后移至各自证据目录，每次恢复初始树。不同 runtime 路径的证据不能互比。实际流和成对比较没有 UUID、时间、路径、错误、版本文本的替换；警告仅有第 6 条精确例外。D2 的独立 stdout oracle 从真实 TS 原件机械提取，只把当次已知 runtime 输入写成 `{runtime}`；显式 `stdoutRuntime: "text" | "json"` 让比较器按本轮 `round.runtime` 实例化**期望值**，JSON 模式仅转义插入路径，不解析/重排正文。不改原件或实际比较视图，不放宽路径后缀、空白或其他字节；正文中的 `{runtime}` 也不被解释。登记时须反向展开逐字还原来源原件。
5. **观测元数据**：PID、wall clock、耗时、源 HEAD/二进制哈希用于 provenance 与完整性检查，不当产品相等字段；每一侧仍须有清理成功、OS 隔离预检通过、有效进程树记录。运行命令、环境、stdin、场景集合、fixture、装置/spec 哈希必须绑定一致。

6. **TS stderr 开头的 Node 22 SQLite 两行**（本轮按用户明确指定边界修正）：不向 Node 传 `--disable-warning`，完整保存 stderr.bin 与 events.json；先校验二者原始字节相等，再构造只用于 oracle 与成对比较的视图。仅当 **TS** 的 stderr 以以下完整字节模式开头时，剥离 **一次**，`<数字>` 仅匹配一个或多个 ASCII 十进制数字，两行均须以 LF 结束：

   ```text
   (node:<数字>) ExperimentalWarning: SQLite is an experimental feature and might change at any time
   (Use `node --trace-warnings ...` to show where the warning was created)
   ```

   同时仅从比较用事件序列的 stderr 字节中扣除这段前缀，保留剩余跨流顺序；原件不写回。**Rust 侧完全不剥**；正文中非开头位置、第二次重复、缺行、CRLF、其他文本/类型/code 的任何警告均保留并比较。原因是 Node 22 SQLite 的已实测启动提示含每次不同的 PID，且不属于 Rust 产品应复刻的输出；按类别禁用会吞产品警告，故禁止。实测原件位于主仓 `/Users/da/ws/pilotdeck-rs/_tmp/20261017-d0/worker/calibration-host2/ts-1/cases/help-long/stderr.bin`（两行完整原件），对应 `ts-2/cases/help-long/stderr.bin` 证明 PID 变化；host5 使用旧禁用参数，不作本条的新定标证据。新 profile/空 OpenSSL 配置须由协调者重新真实定标。

7. **TS stderr 的完整 Node 未处理异常尾段**（2026-10-31 人批 #1151 一 A，见 `state/decisions-20261031-human-batch.md`；本轮用户指令直接确认授权）：先按第 6 条处理，再仅在一行业务错误之后紧跟以下完整形状、且恰好延伸至 stderr 末尾时，剥除一次异常尾段。业务错误行及其 LF 原样保留；**Rust 侧完全不剥**。当前实现仅用于 P08 `sdk_signal_exit` 的 `common/signal_replay.rs` 比较，独立 D0 Python 比较器不共用此实现，本轮不扩其范围。

   本轮仅接受真实 Node v22.22.0 原件已见的 `PilotDeckError`：`file:///…/packages/sdk/dist/transport.js:<ASCII 行号>` 源位置行、一个以空格缩进、正文首字符非空格/Tab 的源码摘录行、只含空格及单个 `^` 的箭头行、空行、`PilotDeckError: <msg>`、至少一个以四个空格加 `at ` 开头、正文首字符非空格/Tab 的栈帧。最后栈帧以 ` {` 结束，随后对象展开必须按顺序为 `code`（`timeout` / `authentication_error` / `server_error` / `transport_error`）、`requestId: undefined`、`details: undefined`、`retryable: true|false`、`[cause]: undefined`，保持 Node 原件的两个空格缩进、逗号和单引号；再接独占一行的 `}`、空行、`Node.js v22.22.0` 与最终 LF。错误 `<msg>` 必须逐字等于前置业务行（不含该 LF），全段不允许 CR。

   缺业务行、源位置、箭头、空行、栈帧、任一对象字段、结束版本行，或版本不符、CRLF、对象字段增减、尾后文字、重复尾段，一律保留比较。业务行被改不得靠堆栈归一化掩盖。当前 96 格原件中 16 格符合此形状；另外 8 格只有未处理异常、没有业务行，不属于本条授权边界。原始 `source.json` / `oracle.json` / stderr 文件不写回。

8. **有实证的场景按流比较**：仅在同一场景已有两轮真实 TS 运行、证明 stdout / stderr 各自字节相同而有效跨流顺序不同后，才可在 `scenarios.json` 为该场景登记 `"streamOrder": "per-stream"`，并填写非空字符串 `streamOrderReason`，说明原因及两轮原件证据路径。合法声明仅跳过该场景的跨流 `order` 比较；stdout、stderr 各自的完整字节仍逐字节比较，TS 对独立期望及 Rust 对 TS 均适用。TS 自比与 Rust 配对共用同一比较路径；原始 events 与每路 `.bin` 的一致性、退出码、完整树、隔离和身份检查照常执行。

   未声明时仍严格比较跨流顺序；`streamOrder` 唯一合法值是字符串 `per-stream`。缺 reason、空或纯空白 reason、非法值或类型（含 null），以及只有 reason 而没有声明，均为**装置错误**：比较器返回 `BLOCKED` / exit 2、规则 `apparatus.stream-order`；运行器在启动被测进程前拒绝。不能回退成默认值，也不能判 PASS。

   当前仅 `legacy-skills-mixed` 登记此例外：真实 TS 迁移摘要写 stdout、配置报错写 stderr；按第 6 条去除启动警告后，`ts-1` 为 stdout→stderr，`ts-2` 为 stderr→stdout，两路分别为相同的 203 / 38 bytes。证据根是 worktree `/Users/da/ws/pilotdeck-rs/_build/wt-d1-cli/_tmp/20261018-d2-skills/`：`d0-final/ts-1/cases/legacy-skills-mixed/events.json` 与 `d0-final/ts-2/cases/legacy-skills-mixed/events.json`，各自同目录的 stdout.bin / stderr.bin 为原始字节；`stream-audit-third-round.json` 登记审计及原件 SHA-256。原批次其余 14 个 D2 场景保持单流、不加声明；新增的 `legacy-skills-current` 在 `d0-final3/ts-1` 与 `ts-2` 中均为 stdout→stderr，保留默认严格顺序比较，不登记例外。`legacy-skills-mixed` 的期望继续使用第二轮从 `d0-calib1/ts-1/` 提取的实录，不做函数探针拆分。


TS 自比必须先过场景独立预期及证据完整性检查，再比较两次原件。任一缺失场景、额外场景、缺失原件、模式不符或未运行都不能 PASS。运行器不提供放宽/忽略参数。
