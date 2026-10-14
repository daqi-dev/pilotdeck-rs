# ui/server 对拍验收（DRAFT）

本规约属于计划 003、Issue #47 B2 S3/S4。没有人批、没有真实 k=5、没有独立评审，因此尚未冻结；已准备 FROZEN.md「待定标」哈希草稿，不改 specs/parity/ 或 specs/gateway-parity/。主入口为 `scripts/ui-server-parity/compare.py`，规则编码与精确字段位置进入 JSON `differences`；退出码 0=PASS、1=FAIL、2=REFUSED（多态池准入未满足）。PASS 只表示给定证据通过当前草稿，不能替代冻结与阶段验收。

## 输入与证据硬门

每侧必须提供 S1 `recording.jsonl`、`activity-direct.jsonl`、`cases.jsonl`、六场景 `scenarios.json`，以及 S3 `run-meta.json`、停掉自有服务之后采集的 `snapshot.json` 与对应的实际 `home/`、`project/`。本轮 Express enumeration 必须显式传入，不能回退到静态分母。matrix/scenarios 默认用本仓 S2 文件，可显式传不同夹具。

`run-meta.json` 记录 schema、实际 direct/proxy mode、固定 fixture_path、独立 run_id、observer_port=13102、wall_start_ms/wall_end_ms、baseline_path、synthetic 标记与装置/规约 SHA256。真实录制的装置哈希必须等于当前版本；比较期间不能改装置。左侧必须是 TS direct；两侧 **fixture_path 一字不差**。S1 默认的双目录录制可继续使用，但 direct/proxy 路径不同，不能作为本草稿的等价性证书。应使用 run.py 单侧、固定 slot、固定 --out，完成后改名归档，再用原路径启动下一侧。

- `evidence.integrity`：schema 1，唯一 start/end，末条为 run_end，全局 seq 从 1 连续递增；arrival_ns 是非负十进制字符串。并发不同 task 在取得写锁前采时钟，故不要求全局 arrival_ns 跟着 seq 单调；HTTP 同一 id/direction 的 body 时间须单调。
- HTTP request/response id 一一关联；每个非 101 请求两方向都有唯一完整 http_end；offset、bytes、base64、Content-Length 与实际 body 一致。recorder_error/http_body_error/http_aborted/ws_partial、WS transport error、成功状态却报 application error、非法元数据、未知 recorder kind 均失败。101 按 WS 握手/连接结束验证。
- 六场景恰好齐全且逐项 PASS；驱动报错、任一侧缺文件/空录制/不完整，独立 FAIL。已声明 JSON body 必须合法 JSON，禁止 NaN/Infinity。
- WS 连接必须有握手、完整应用帧、双向 1000 close、唯一 ws_end；原始 wire 与解除 mask 后 payload、FIN/RSV/opcode/mask/长度必须自洽。当前六场景不接受错误 kind/type 帧，mock turn complete 必须 success=true/finishReason=completed。
- `sse.complete`：必有非空完整 SSE 事件流；上传流须 upload_started→upload_completed 成功收尾，事件 JSON/uploadId 与请求一致；没有事件、末尾未结束事件/行、非法 UTF-8/到达证据皆失败。
- `disk.snapshot`：home/project 各含真实文件；重新采集实际目录，先核对录制时保存的 snapshot，再做跨侧 diff。缺/多文件、内容/权限/目录/链接/类型变化均失败。SQLite integrity_check 与逻辑表/列/行参与比较；不能只对 DB 大文件字节作宽松忽略。

## 路由 × 用例覆盖矩阵硬门

`coverage.matrix` 同时检查本轮 enumeration、S2 manifest、matrix 与两侧 recorder 实际证据。每条运行时路由必须有 ≥1 个直接 HTTP 用例或合法显式豁免 `{route, reason, approver}`；漏行、新路由、过期行、重复/碰撞行、错误 case 引用都 FAIL。每条声明 case 必须两侧实际出现，method/path/状态符合 manifest，驱动 case_http 的 route/status/handler/ok 与 recorder 相符；只写声明不算执行。显式预期的 401/400/501 是登记过的负向用例，不能把任意两侧报错当等价。

`coverage.handler` 以 recorder 原始 `X-PilotDeck-Handler` 为处理侧真相源，再交叉核对 metadata 与客户端证据。direct 必须没有 DUT 标记；proxy 每条响应必须唯一 ts/rust，冲突/伪造/缺失即 FAIL。标记验完才从 HTTP 等价 diff 剥离。

任一路由观察到 rust，或 matrix 行预先声明 `handler: "rust"`，其 proxy 侧每个登记 case 必须实际由 rust 处理；TS reference 无须 rust 标记。漏 case/只有 TS 响应不能冒充 Rust 覆盖。Rust 路由豁免必须人工批准，含「AI 代批」的豁免失败；豁免不算行为验证，也不能替代该行已有 case 的 Rust 执行证明。没有枚举或 matrix 的 Rust 请求失败。

## 行为硬门

HTTP 的请求、状态、同名多值头列表、JSON 结构/确定字段、非 JSON 原始字节与 trailer 均比较。ID 跨 HTTP/WS/SSE/落盘使用同一分类映射，重复引用与别名关系须一致；时间/随机字段只允许 normalization.md 中公开的完整字段路径。未知路径、数组顺序、缺失/null、bool/number 差异仍参与 diff。

WS **先比较同一连接内全序帧序列，再比较按 type/kind 的结构和确定字段**。全局 seq 与每连接 seq 子序列严格递增断言保留，但两侧的绝对 seq 数字不直接相等；不比较跨连接到达先后；归一化按稳定连接键遍历，各连接内部保留 seq 次序，不能通过按 type 分桶洗掉换序。binary/continuation/ping/pong/close 原始有效载荷也保留。SSE 比完整事件序列与事件到达上界，不用 chunk 数/边界作硬门。

活动 revision 只使用每侧 `activity-direct.jsonl` 中绕过 recorder/DUT、直连 13102 的原始 activity 与 snapshot。每个被测 `(sessionId, revision)` 必须有带外同值且全内容一致的活动；当前六场景的 watch snapshot 必须等于 `after-scenarios` 的稳定直连活动列表。用户必须等于成功鉴权的用户，四个 anchor phase 完整，观察连接唯一。没有用户 snapshot 最大值推导、没有 DUT snapshot offset、没有首帧自建基准。带外证据按每会话真实 revision 次序映射；其他用户造成的全局 revision 空洞允许保留，不要求连续。

## 四项尺子对照与冻结门

`check_ruler.py` 输出四项：`self_compare`、`per_rule_negative`、`same_bad_rejected`、`independent_review`。

1. TS direct k=5，必须五个独立 run_id、五个 recorder PID、同一个固定 fixture_path、相同装置哈希/运行时分母、成功退出与清理。比较全部 10 对，100% PASS 才能 self_compare=true。复制同一次录制或 synthetic 元数据不能充数。
2. 逐规则注入负例必须 FAIL，并在预期 location 命中预期 rule；不能仅凭“随便某处失败”给对照判 PASS。
3. 两侧同时坏（全空、同样 HTTP/场景错误、空 WS、空 SSE、空落盘、同样 WS error）必须独立拒收。
4. 独立双谱系评审由协调者收集，本 worker 保留 `independent_review: null`，不自填 true。

无真实五遍输入时 self_compare=null、PENDING_CALIBRATION；已自比但未评审时 PENDING_REVIEW；这些均退出 2。任一已运行门失败则 FAIL/退出 1。另可提供 --proxy 单独报告 direct/DUT 比较，不用 DUT 结果定阈值。

冻结必须先有人过目，随后协调者在 manifest 登记四份规约、B2 与 exp008 两目录的全部 Git 跟踪装置文件 SHA256，并填唯一的 `Approved-By:`、`Approval-Basis:`。审批人接受具名人，或精确的「AI 代批，待人复核（协调者）」；拒绝 TODO、模型名、零宽/不可见字符、占位、空值与重复。依据至少包含三个 realpath 不同、确实存在的文件（定标报告 + 两家评审），且必须位于当前仓库或共享主仓 `_tmp/` 内，链接逃逸也失败。审批清单自身不参与 capture hash。

装置完整性威胁模型（协调者自决，**AI 代批，待人复核**）：防 (a) 意外漂移；(b) worker / 开发者在仓库内对装置的改动，包括未跟踪文件、影子模块和旧构建产物。不防拥有本机写权限、能够同时改装置、清单与审批的恶意者；该模型之外的发现按 P2 跟进。Git 跟踪清单、FROZEN 哈希与审批材料构成本模型的信任依据。

B2 与 exp008 目录实际文件必须恰好等于 Git 跟踪内容；额外文件（包括忽略文件）、缺失跟踪文件、任意链接、Python 缓存与非普通文件一律 FAIL，不得过滤后继续。FROZEN 必须完整登记两目录及四份规约，重复/逃逸路径、漏项或哈希漂移均 FAIL。所有 Python 入口及子进程带 `-I`，预检断言 isolated / ignore_environment / no_user_site 和未启用 pycache prefix；Python 环境变量不生效。装置预检源码及本目录模块须经 hash 校验，显式从绝对路径按源码加载；B1 加载后恢复 sys.path，reconcile 同样使用隔离子进程。

calibrate 开始与续跑要求装置/规约路径 Git status 为空，并将 HEAD 绑定到 batch.json；批次内 HEAD 漂移即拒绝。每批创建新的 CARGO_TARGET_DIR 于批次目录中，已存在 target 不可复用；构建后登记可执行产物 hash，每遍及续跑重新核验，禁止钉住 `_build` 旧产物。缺 manifest 只允许 HEAD 源码且返回 DRAFT/2；首行「待定标」且完整哈希正确也返回 DRAFT/2；正式 manifest 缺有效审批返回 FAIL。负例自检为 `python3 -I scripts/ui-server-parity/check_frozen.py --self-test`。真实 k=5、TS 池、直连/DUT 比较、前端 page.route 与双谱系评审仍是后续验收，离线 controls 不替代它们。

## 第 3 轮顺序规则补充（DRAFT）

上文 WS 完整序列门对 `/shell` 使用 normalization.md「第 3 轮」中明确的例外：先逐帧验证完整性，再按连接/方向比较完整 output 字节流及非 output 帧序列；resize 值、次数、方向内顺序不变，output 分块和跨方向交错仅作诊断。普通 `/ws` 连接继续完整全序比较。projects_updated 仅七条完整时钟路径归一并保留同广播全部时钟大小/相等关系；changedFile 的值不归一，第 4 轮仅对登记项增加类判定。本补充未冻结，不修改已有冻结尺子。


## 第 4 轮：登记多态仍在守护内

人已批准沿用阶段 C #35 方案 B / 1B；精确范围、同日同源池准入、全类成员匹配见 [multimodal.md](multimodal.md)。普通字段和全部健康门不变，TS 参考池不得吸收其他字段的差异。`check_ruler` 必须显式提供 >=20 遍 TS 池，再验证全新的 k=5 全十对和 proxy；报告同时保留池的全部两两严格比较和每遍的全成员匹配。未提供池或池不合格为 REFUSED/2。`--paired-only` 与历史 `--replay-of` 仅用于诊断，不能定标；前者不与池同用。无实跑数据时 self_compare 仍 null；全部机器门通过仍需独立评审，不自动冻结。

## 第 5 轮：联合单元与既有池离线重放

多态范围以 multimodal.md 第 5 轮为准：两连接第一次 changeType 与第二次 changedFile 是一个精确路径集合，按全部联合值确认类，不独立接受字段组合。`--replay-pool-of` 可绑定原 calibration ruler（含 REFUSED 报告）离线重算已有池和计分捕获；报告必须标识 historical-replay、calibration_eligible=false 并保留原报告与新旧装置哈希。此路径不修改原始账本，不取消任何源/身份/健康门，也不作为新装置定标；默认正式入口仍拒绝装置哈希不匹配。

## 第 6 轮：独立单元更正（替代第 5 轮联合判法）

多态范围以 multimodal.md 第 6 轮为准：U1 为两连接第一次 changeType，U2 为第二次 changedFile，分别满足池 >=20、每类 >=2、全成员命中；同单元两连接同值，跨单元类独立组合。判定一个单元时只将另一个单元的精确登记差异交给其独立判定，其他完整比较/健康门不变。报告 TS_calibration.units 与 multimodal 各侧的 units 分别保存 U1/U2 类和全成员证据；保留原始严格比较。历史池离线入口及其 calibration_eligible=false 标记不变。
