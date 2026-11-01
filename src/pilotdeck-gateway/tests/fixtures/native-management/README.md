# #1231 原生 A8 管理路由金样

固定只读 TS cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85、Node v22.22.0 / ICU 78.2 / en-US。运行真实 createLocalGateway、registry、SessionRouter、GatewayRuntimeRefreshBundle 和各族领域组件；不调用模型。隔离 HOME、PILOT_HOME、XDG 与 storage，输出目录递归创建，oracle 独占写入。

U3 snapshot、U4 manager、U5 commands、U11 skills 每族都有调用 → 显式 reloadExtensions → 再读，同时观察真实 generation 身份与 dirty 导致的会话替换。RPC 本身没有通知；reload 的顺序为 invalidate → dirty → config_changed。测试中的 session factory 是无 provider 的最小会话，缓存与 dirty 逻辑仍是真实 SessionRouter/SessionOwner。

fs.watch 只替换为静默 EventEmitter，避免 OS 调度和沙箱 EMFILE；此金样不验证 watcher。Rust 复用现有 RefreshBundle seam 驱动这段序列；原生进程的外部 reload/watch 控制面不在四族请求路由范围。组合根测试另行验证真实 host、describe、四族请求及 skill_create 后重启；其中 snapshot 显式配置仅测试可达，见下文生产入口边界。

路径替换为 <root>；mtime 先断言为有限数字再替换为 <mtime>；nextCursor 双层 base64 解码后逐字段比较。cursorForm 行按当前运行的项目目录重建真实字节数组、Buffer JSON 或 arraylike，避免将随机路径编码进金样。其余 params 原样重放。

FIFO pendingIo 序列令真实 skill_read 保持未完成，snapshot_list 必须在 3 秒内独立完成，随后原子替换 FIFO 路径并释放 writer。Rust 还丢弃调用方响应等待器，验证任务仍完成。物理阻塞 IO 不支持强制取消；host 的 drain deadline 可以返回错误，但不能声称 blocking worker 和资源已经释放。

## 重生成与负控

设置 PDRS_BASELINE 指向固定 TS 检出，PATH 的 node 必须是固定版本，运行 generate.mts NEW_OUTPUT；Rust ignored regenerates_identically 测试调用共享重生成守卫。生成两次必须逐字节一致。

negative_controls.py --check 仅检查锚点。实际负控命令为 negative_controls.py NEW_OUTPUT [mutation ...]，必须由调用方设置 CARGO_TARGET_DIR 与 PDRS_BASELINE，使用共享 HEAD 守卫、进程所有权和最终汇总。每项先正控，再编译变异，再判杀，恢复后复跑；过期锚点及无法编译都是 INVALID，继续执行后续项。未提交源码不能运行，不得使用副本仓库绕过；预计超过 20 分钟时由协调者在宿主运行。

manager_browsers 已分发到现有组件；原生没有 browser provider，因此返回 CAPABILITY_UNAVAILABLE，describe 不广告。U2/U6 保持未接。技能管理可使用持久化技能目录；turn admission 仍拒绝未接的技能执行组合。snapshot 只在配置项目内恢复；host-dependent JS 路径形状明确拒绝。

manager_sessions 金样读取固定时间戳的真实非空 JSONL transcript（session.jsonl），验证配置项目的 chat 目录映射。管理写入不会自动刷新执行资源；原生 turn admission 仍明确拒绝配置了技能的项目，生产宿主用例对此断言。commands 只将同步 acquire 放进 blocking job，后续复用 catalog 自有异步 loader/list，避免持有外层 blocking 线程等待内层。限额线程池用例通过 channel 占用一席，不制造 CPU 压力。sourceSha256 是共享生成器计算的依赖闭包，不仅限于入口文件。

U4 在 TS 注入真实 listProjectSessions 作为 sessionCatalog（与 Rust 已合入的 TranscriptSessionCatalog 提供同一 JSONL provider）；未注入固定返回值。TS 默认 sessionDataPlane 的底层存储选择与原生 JSONL provider 不同，不把两者混为同一部署。

queryUnits 行保留真实 UTF-16 查询：孤立代理项不会先替换成 U+FFFD。固定命令 replacement�.md 用于区分有损替换；typed callers 继续沿已有字符串接口，wire caller 使用同一 catalog 的 UTF-16 入口。限额线程池测试覆盖四族，describe 在测试显式非空 storage 下排序后精确比较 13 项，重复、多报和漏报均失败，同时验证 expected 是 oracle.contract.capabilities 的子集；5 项管理能力分别配漏报变异，另含 upload_create 多报与 run_get 重复变异。8 个 skill_* 仅作为 RPC 方法，不属于 TS capability 契约，逐项配错误广告变异。

## 生产入口边界（fix-r3）

native isolated 模式拒绝继承非空 `PILOTDECK_STORAGE_ROOT` / `PILOTDECK_SNAPSHOT_ROOT`，包括纯空白；`Options::parse` 没有 storage/snapshot 配置参数。本包生产入口 U3 snapshot 恒禁用：describe 精确 10 项，snapshot_* 返回 CAPABILITY_UNAVAILABLE。13 项与 snapshot 成功路径只在测试经 `assembly::start` 显式配置出现；生产接线另由跟进单处理，前置于 A8 U3 生产入口验收。

`production_entry_snapshot_capabilities_are_disabled` 走 Options::parse + validate_environment，覆盖两个 key 缺省/空串的四种组合，及非空/纯空白的继承拒绝；成功启动后精确比较 10 项并验证 TS 子集。配置只在 persistence stage 构造 provider 时解析；装配将实际 provider 存在性缓存到 Requests.snapshot_enabled，describe 使用该值和显式能力名单。TS 接受纯空白 storage 并禁用 provider，而 native 启动拒绝是既有隔离边界，已在同一差异账本明确登记，不声称两者一致。

## #1231 fix-r1 契约与成功磁盘路径

`contract` 来自真实 `describeServer` 与 `PILOTDECK_STORAGE_ROOT` 缺省、空串两种真实 TS gateway。`emptyStorage` / `missingStorage` 均保存 config、capabilities 及 list/get/restore 完整错误：provider 缺失，三请求返回 CAPABILITY_UNAVAILABLE；TS describe 仍声明 snapshot。原生组合根不再补 `home/storage`，调用错误形状与 TS 金样逐项相等；按 #1231 fix-r2/fix-r3 协调者决定（AI 代批，待人复核），禁用时 describe 精确保留 10 项、隐藏三个 snapshot 能力。这一广告差异及入口拒绝范围登记于 `specs/gateway-parity/diffs/A8-NATIVE-SNAPSHOT-CAPABILITY-001.md`。两个宿主用例预先在 home/storage 种入真实 TS snapshot，确保既有磁盘数据不会隐式启用 provider；每格分别配默认补根与错误广告负控。非空成功与 FIFO 独立请求测试显式传入隔离 storage。

`home/projects/a8-main/.cwd` 固定项目 storage ID；`snapshotSuccess.storageFiles` 是固定钟下真实 TS recorder 生成的 manifest、marker 和对象原文。路由重放成功 get、其他 workspace 的 get 拒绝、显式配置项目 target 的 restore；先验证恢复目录 parent/name 和完整文件字节，再仅归一化随机目录后缀。此项不包含原生 turn capture 或跨隔离项目恢复。

`skillImport` 重放真实外部目录的 import → read/list/scan → 修改源内容后重复 import 冲突 → force 覆盖 → read/list/scan；每步验证全部目标文件，包括 assets/note.txt，冲突必须保留旧内容。生成器输出目录 recursive:true，oracle 使用 flag:'wx'；既有 ignored regenerates_identically 守卫覆盖整份金样。

负控共 69 项，覆盖真实 U2/U6 的分发与广告边界、能力精确集合和 TS 子集、生产入口继承拒绝、装配状态与显式过滤、禁用 storage 的调用与广告、snapshot 成功/过滤/目标/字节、skill 附属文件/冲突/覆盖。`capability-added-skill_scan` 名称明确表示追加错误能力；既有能力增删变异绑定 `#1231 exact wired capability set`，新增禁用过滤变异绑定生产 10 项，TS 子集变异绑定专属 subset 消息。`snapshot-storage-omitted` 只绑定 `#1231 snapshot route with explicit storage`。每个变异的完整测试名和专属失败消息在 negative_controls.py；所有产品源码变异仍受 mutating_sources 的 HEAD/恢复守卫保护，逐项先编译并汇总，不能在未提交修复树上绕过运行。
