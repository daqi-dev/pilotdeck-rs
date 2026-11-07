# 2026-11-08 · 新机器入口的首片边界

## 有返回值、空返回值与清理

新入口的普通调用返回 `{"ok":true,"result":...}`，无 result 则保留为 `{"ok":true}`，不把已成功的删除 / 关闭操作误报失败，也不混同 null。收到匹配结果后独立进行有界连接清理；清理超时不推翻已确认结果。stdout 失败明确非零，告知操作可能已经完成，绝不自动重试写操作。该约定仅用于新增 `pilotdeck-control`，旧 CLI 输出不变。

## 认证材料与业务返回内容

- 卡在哪：最初任务书“不输出凭据”可被解释为任意返回文本都不能含 token 的任何字节。对任意合法短 token，这与返回业务数据无法同时成立：例如 token 为 `a`，正常能力名 `actual` 就有同样的字符。整段 JSON 子串检测曾使正常请求被误判失败。
- 谁受影响：新增机器入口的服务端返回数据；旧入口、Gateway 服务端和持久数据不变，可撤回。
- 选项与后果：拒绝短 token 会缩小既有 Gateway 可连接范围；对所有字串盲替换会破坏普通数据；完全不处理回显则容易把认证材料混入终端记录。
- 建议与决定：**AI 代批，待人复核**。客户端诊断不自动打印认证值、请求体和 URL；返回数据采用有界、明确的结构化脱敏：长 token 的字面回显、短 token 的完整值或独立词回显会替换，普通文字保持。先提取命令需要的原始字段，再处理返回 payload，不能因键被脱敏而误判服务端未返回能力。
- 实际限制：短 token 嵌在其他字词、编码或拆分回显不保证识别。这里没有“任意服务端数据绝对零泄漏”的承诺。此前独立审查在绝对解释下提出的 P1 记录保留；本决定明确可执行产品合同，不将那项更强承诺虚报已完成。
- 原则与断言：安全处理不能将已经完成的操作伪装为失败，也不能悄悄扩大所宣称的保证。真实子进程回归覆盖短 token 正常能力名、常见成功 / 错误回显、token 恰为能力字段名，以及 UTF-16 保真与字段碰撞。

## 完整性

首片为现有 Gateway WS 的一次性非流式入口，已做真实隔离 host 的跨入口读写验证；它不等于完整应用协议。serve、schema、流式提交、事件恢复以及其他尚未迁移的旧命令仍需后续实现。Windows 仅有条件编译边界，未做目标平台运行验收。


## 新机器流的消费与退出约定

**AI 代批，待人复核**：新增 `stream` 独立文本轮次，不修改旧 CLI/SDK 的 seq/final 语义。流序号连续且保留 final 闭合帧，完成、取消、错误分别可判；收完整输出才成功。事件键与 type/finishReason 是调用方分派所需的公开协议标识，保留；其它值沿用有限回显过滤，不作零泄漏承诺。输出一帧在途、8 MiB含换行、10秒交付预算，断线可能触发session中止，明确不承诺恢复或detach。范围局限新bin，可撤回、不动冻结尺子。断管/背压、标识同名token和非法序号均用真实子进程测试防止假成功。

## 2026-11-08 · 显式 native 前台 serve 入口

- 卡在哪：现有control能连接指定WS，native生产宿主只有gateway bin的显式入口；默认CLI/新app协议/发行自动安装还缺自己的边界，不能以薄包装冒称整体兼容。
- 谁受影响：新增 `pilotdeck-control serve` 首参分流；已有call/status/capabilities/stream保留原解析与输出。
- 能否撤回：可以，未迁移数据或改变Gateway装配；显式选用新入口者承担自己指定的数据根/sidecar权限。
- 动不动尺子/意向：不改冻结装置或TS oracle；属于既定完整CLI目标内可撤回的原生前台入口，不声称旧server或新app JSON-RPC兼容。
- 选项与后果：复制装配会产生第二套生命周期；直接复用native Options/run保留现有校验、依赖和drain。自动查找/安装/起第二host需其他所有权/发行合同，本片不推导授权。
- 建议与决定：**AI代批，待人复核**，首参serve先分流、原OsString参数交既有parser；当前线程enable_all runtime内安装既有SIGXFSZ guard后调用native run。ready/stopping仍stderr，不编造stdout JSON。单值项拒重复，sidecar-arg依原合同可重复。
- 原则与断言：薄入口必须通过真实bin证明与宿主连线，不能只用mock确认调用过run。实际参数拒绝无bind/无落盘，真实serve+独立旧WS共享状态，真实Rust sidecar/fake provider完成stream/取消/落盘，INT/TERM分别验证有界关闭及sidecar不遗留。
- 范围：调用方显式提供home/project/listen/token-file/sidecar，不触碰用户目录，不自动发现/杀宿主。native自身cron/IM配置生命周期保持，单根写者锁、默认本地boot、自动发行安装、新app协议及A8仍需独立完成。

- serve token安全增量：独审确认native原token无界同步重开可被换成FIFO。共享A文件有其他持有人，**AI代批，待人复核**采用CLI限定快照：复用既有O_NONBLOCK普通文件/64KiB读器，再以0700目录/0600匿名inode、CLOEXEC及/dev/fd交给native真实消费，原路径不再由native重开。unlink先于写secret；启动前重开正控后seek回0。仅Unix且/dev/fd可用，故障明确失败；原UTF8/CRLF/空token规则保持，不声称A全局已修、同UID/root不可读或跨写者事务。正常/失败清理由真实测试和实际serve流验证，SIGKILL可能只残空目录，不留命名token副本。

- 快照写阶段护栏：runtime与既有SIGXFSZ ignore安装先于快照write，实际`ulimit -f 0`辅助子测试核明确错误、原token字节与无该进程临时目录残留。参数parse仍在runtime/快照前，非法参数不启动宿主或落盘。

## 2026-11-08 · CLI 只读 binding 与 opt-in 有界配置加载

- 卡在哪：CLI把精确true的binding直接拦TODO，但Gateway已有只读runtime apply；直接接通又暴露共享NativeFiles对store/SOP/lock等同步无界打开/读取，FIFO可挂起。
- 谁受影响：默认CLI配置预检、供新CLI/TUI选用的bounded snapshot；旧Gateway API与NativeFiles、StaffDeck provisioning/认证/实际module调用不改。
- 所有权与依据：本轮2026-11-08只读扫描59登记worktree，CLI config.rs/Gateway config/mod.rs/new bounded.rs无未提交占用，HEAD↔origin/main接口无diff；协调者明确授权本Sol树持有mod.rs两行注册+新模块，不以未登记者不可绝对证明为空而停工。候选基底869bf7f42；开发中不换A源，后由协调者串行集成当前main。
- 能否撤回：可以，无数据格式迁移或绑定写入；普通配置继续既有inner/错误次序，资源拒绝时原件不转换、不保存。
- 动不动尺子/意向：不改冻结装置/旧金样；真实TS正常金样先采，8MiB/32MiB为明确新安全边界，不伪称TS已有预算。差异D2-CLI-CONFIG-READ-BOUNDS-001未批准、无冻结豁免。
- 选项与后果：只预检路径不能阻止换FIFO或增长；改共用读器会扩大A共同作者和旧语义；新Files wrapper复用inner，在实际fd读处限额且首fault后停止所有后续源IO可限定影响范围。
- 建议与决定：**AI代批，待人复核**采用Unix默认CLI opt-in load_project_bounded及供新入口用的load_snapshot_bounded；普通IO照旧，typed资源拒绝精确返回。非Unix默认CLI保留原load_project，不能把Unix保证变成其他平台普通配置的新前置；新bounded API自身非Unixunsupported，未验Windows、不宣称整crate可编译或平台已支持。
- 原则与断言：配置读取拒绝不可吞为空或TODO；预算累计包括部分/探测字节，不减计、不重置，fault后read/stat/entries不delegate、process_alive不查。真实FIFO/稀疏超限/32MiB累计IO、snapshot普通/超限、30新TS+14旧frozen与有界真实CLI mock逐项验证。
- 边界：CLI check只预检并丢弃Config，远端实际module由远端Gateway决定；本地fallback、StaffDeck远程实效和A8仍未完成。这里只限文件读字节/特殊文件，不宣称所有解析内存、故障FS内核IO可中断或跨写者事务；最终mbp2原始日志与源码hash据实登记，未运行不报通过。
