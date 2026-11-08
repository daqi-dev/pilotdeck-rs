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

## 2026-11-08 · SDK token 文件的 Unix 有界读取

- 卡在哪：SDK文件回退使用无界fs::read，真实完整TS整CLI证明无writer FIFO挂起、65537字节token会完整发送；所有SDK命令族均经此入口。
- 谁受影响：SDK read_connection的文件来源；argv/env/URL/transport/其他CLI读器不改。58登记worktree只读核sdk.rs及新token.rs无未提交占用后，协调者明确授予本Sol树所有权。
- 能否撤回：可以，没有数据写入/迁移；普通文件与普通IO/目录错误保持旧合同。新特殊文件/超限拒绝不读pipe内容、不发网络。
- 动不动尺子/意向：不动冻结装置或旧D1金样；独立D1-SDK-TOKEN-READ-BOUNDS-001未批准、无评分豁免。SDK已合内容/--model专支不重做，A8仍独立。
- 选项与后果：路径预检不能挡替FIFO/增长；实际fd O_NONBLOCK+普通文件判断+65536+1真实读可限定内存/特殊文件风险，不复制SDK runtime或A装配。保持非Unix旧读器，不臆造Windows保护/实测。
- 建议与决定：**AI代批，待人复核**采用65536原始字节上限，FIFO/其他已打开特殊文件和超限明确退出1；目录/EACCES等普通IO仍旧token-required。空flag不是空env：真实readValue过滤空flag而回退env；空env不回退，argv/env空白原样、文件UTF8 lossy与trim不变。
- 原则与断言：读器必须绑定已打开fd，不能以路径预检假称安全；实际增长/替换FIFO barrier、64KiB边界、19格整TS+真实Rust CLI/15命令族资源组合核原件/零连接。延迟3秒writer正控仅TS实际执行，Rustactive pipe不消费不是时序对拍；不以null stdin声称开放stdin等待顺序。
- 边界：只修SDK文件来源，不称cron/default remote等已保护；旧serializer继续，不扩D1字节顺序判据。故障FS内核IO和跨写者事务不在保证内，真实provider/认证服务和真人签字待验；全部Rust仅mbp2、本机只编辑/离线实采，不推GitHub。


## 2026-11-08 · HOME 未设的首片范围

- 卡在哪：CLI 私有 homedir 在 HOME absent 时直接 TODO，固定 TS/Node 则查询系统账户；技能迁移还直接要求 HOME，单改 tilde 展开不足以接通。
- 谁受影响：Unix 真实进程 HOME absent 的普通账户、现有路径展开及 skills migrate；HOME 存在包括空/相对继续原值，bare `~` 的既有额外 resolve、setup 独立目录计算与 cwd 启动先后另片串行。SDK 文件读器已经冻结，不修改。
- 能否撤回：可以，只增加已有 TODO 的原生实现，无格式迁移；不改 Gateway API、默认写目标规则或冻结装置。
- 选项与后果：直接用 std fallback 处理注入 Env 会把真实宿主 home 混入隔离测试；新增 libc unsafe 被工作区禁止；复制 Gateway 的失败后 `/` fallback 无 TS 成功依据。采用标明来源的 Env 与 HOME-only 延迟输入，synthetic 只用注入 fallback，真实 process 才调用安全标准库。
- 建议与决定：**AI 代批，待人复核**。协调者明确允许 CLI manifest 加 rustix process。只有 snapshot 与实际进程均 HOME absent、real/effective UID 相同且查询前后未变化时才用 std::env::home_dir；UTF8 账户 home 成功返回，缺账户/非UTF8/不同UID/环境变化明确 TODO，未实测不得伪造空值或根路径。非 Unix 保持未实现，不宣称平台运行已验。
- 原则与断言：可注入环境不能悄悄读取宿主环境；真实账户 home 仅取字符串做与固定 Node 的比对，不执行其路径的任何 fs 操作。普通单测以隔离 fake account 覆盖优先级和完整迁移 report；真实 HOME-unset 只运行路径专用 ignored probe，禁止生产技能扫描。
- 批准与验证：这是恢复固定 TS 的普通账户语义，不新增已接受差异或冻结豁免；异常账户未支持仍据实列出。所有 Rust runtime 由协调者在 mbp2 执行，未完成不报通过。后续 setup 与裸 tilde 另片审查，不扩大本片修复声明。


## 2026-11-08 · setup 账户 home 延迟查询

- 卡在哪：setup自有`PILOT_HOME || join(homedir(),'.pilotdeck')`在Rust无HOME时误落cwd，Weixin保存再TODO；复用pilot_home会错误展开非空~，构造时提前查Weixin目录又会阻断本来无需homedir的显式PILOT_HOME取消/其它平台。
- 谁受影响：gateway setup目录计算与Weixin凭据保存；普通HOME含空/相对原label保持，不触碰原子保存、HTTP、prompt、SDK/main/TUI或共享Gateway。
- 能否撤回/尺子：可撤回，无新格式迁移；不改冻结文件/既有金样，追加自己的真实TS受控OS金样，正常分支恢复TS语义。
- 建议与决定：**AI代批，待人复核**。SetupPaths只保存HOME-only输入/来源与传入cwd；默认目录需要时查询home，非空PILOT_HOME保持字面并短路账户查询，Weixin真正保存时才解析独立mkdir目录；IO以cwd定位、错误label仍用join原相对值。
- 原则与断言：延迟行为不能被目录预备动作破坏。固定TS受控fake os.homedir十格记录真实查询时机/输出/文件/HTTP，Rust无fallback synthetic取消仍成功、保存才要求home，成功写入只发生在注入fake账户。真实OS账户只查字符串不做fs，不以“隔离PILOT_HOME”掩盖Weixin另写homedir的事实。
- 验证与边界：所有Rust与Node运行在mbp2。r1严格clippy/49测试/build已通过，新增完整十格oracle断言后r2严格clippy/50普通+4父实际调用helper/build实际通过、非作者独审无确认P0/P1或需处置具体P2；兼容账户范围同首片，未批准任何冻结豁免。


## 2026-11-08 · 统一工作树与 native history 接线

用户明确指定 `cli-gateway-setup/pilotdeck-rs` 为唯一开发树，要求保留两侧快照和旧目录、整合最新main，只本机提交/Rust全部mbp2。按此执行；旧8e32清理为clean并锁定保留，所有原始WIP/patch/bundle与合并逐文件依据在主仓 `_tmp/20261108-cli-consolidation/`，后续不再以两棵树轮流写入。上游持续前进时按抓取的明确SHA整合，不把某次快照称为未来始终最新。

**AI代批，待人复核**：native history复用既有项目runtime、JSONL decoder与TurnGateway读取合并路径；补齐TS投影而不复制宿主装配，避开其他持有人在途的LocalHost生命周期。新限定读器防止实际fd被FIFO/增长拖住，目录marker同样有界；后台路径使用已有beneath打开，完整响应沿既有WS上限显式拒绝。可撤回、不迁移原数据、不改旧通用读器或冻结尺子；具体边界和未接受差异唯一写于 `specs/gateway-parity/diffs/D2-NATIVE-HISTORY-BOUNDS-001.md`。原则：读取端不可把拒绝吞成空数据/错误项目，也不可只限制叶文件而漏掉路径选择的前置IO。断言覆盖marker FIFO、恢复后原历史、完整封套超限及背景/普通文件不同内容；实际结果按原始日志登记，未标完整CLI/native权限/local fallback或阶段完成。
