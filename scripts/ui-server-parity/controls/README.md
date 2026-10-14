# B2 离线对照

从仓库根运行 `python3 scripts/ui-server-parity/controls/run_all.py --out _tmp/controls.json`。不监听端口、不下载、不需要后端。每次在临时目录生成最小 schema-1 的 HTTP、多值头、WS 全帧、带外 activity、SSE 两阶段流和真实 home/project 文件，先要求完整夹具 direct/proxy PASS，再分别复制一份注入差异。

每条负例登记 expected_rule + expected_location；只有 FAIL 且命中该规则/字段位置才 passed=true。修改帧后重排录制 seq，保证「删帧」命中的是 ws.sequence，而不是偶然的日志缺号。截断 SSE 时重写完整 HTTP offsets/EOF，保证命中 sse.complete。整流缓冲保留原字节，只有到达时刻改变；snapshot/revision 同偏移同时修改 DUT 两种帧，带外内容保持独立。

阳性对照包含 HTTP header/status/body/request、Set-Cookie 丢一个值、ID 别名变化、WS 换序/删帧/type 确定字段、SSE 丢事件/合并/缓冲/截断、非法白名单时间、落盘修改、handler 欺骗/覆盖缺 case、#53 同值 offset、录制缺 EOF。两侧同坏分别覆盖全空、HTTP/场景错误、成功状态下相同 error body、WS error、SSE failed/空事件、WS 空帧、空目录。

`test_compare.py` 另检查同名多值头的大小写/分组与值顺序、SSE rechunk/CRLF/多行/id 继承、seq gap、wire 矛盾、并发时钟轻微倒退、fragmented 消息/type 与孤立 continuation、未知 timestamp 不被擦掉、字面点号 key 不冒充字段路径、SSE 归一用例作用域、跨连接时序、literal trailers、wire/后端共同过期时间偏移不丢 TTL、类型、Rust 声明处理侧、两侧同步落盘损坏、SQLite WITHOUT ROWID/二进制、CLI 失败 JSON、五份 synthetic 无法充当 k=5、未冻结/无人批 manifest。

这些是装置离线正确性证据，**不是真实 TS 自比、DUT 行为等价或冻结证明**。真实命令见 exp010 readme。

第 2 轮增加 `identity_paths.py`：113 个实际 ID 完整字段路径、153 条路径级差异对照，每条先验证合法重命名 PASS，再要求同一路径实体合并 FAIL；block/timeline 另要求派生种类/序号改变 FAIL。结果标记 `level=full-path-policy`，并记录精确 expected_location 和 matched，不把这些当成 153 份端到端实录。与原 28 条合计 181 条。另有 WS/落盘端到端引用对照，以及 `node scripts/ui-server-parity/test_chat_driver.mjs` 验证第二次 send 必须等待两连接 idle/watcher、前次失败不得继续；不启动任何服务。

第 3 轮增加 `streams_and_clocks.py` 的 27 条端到端对照，合计 208 条。等价侧同时改变 PTY 分块与 resize/output 双向交错，并对 watcher 时钟整体平移；另测 output WS fragmentation 等价。25 条故障对照验证改字节/丢段/重复/换序/方向、resize 值/次数/次序、mask、output 未知字段（含两侧同坏）、auth_url 换序、非 shell 不豁免、同实体时钟交换（含两侧同坏）、跨实体交换、别名等值、类型、null/缺失/非法值、未知字段及 changedFile 内容。每条仍须命中 expected_rule + expected_location。

分片边界对照另验证：同方向 ping 从一个非 output 分片消息中间移到它之前必须 FAIL；非 output 使用原始帧序列，不按消息完成顺序重新排列控制帧。
