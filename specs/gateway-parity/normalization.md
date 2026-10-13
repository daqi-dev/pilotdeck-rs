# Gateway 归一化口径（草稿，Issue #23 / A2）

代码入口 `scripts/gateway-parity/compare.py`。原始录帧与 home/project 文件不改写；归一化仅用于比较。两侧同一路径是硬前提，绝不事后删路径、替换根目录或对齐不同路径。

## 路径、顺序和 ID

`run_round.py` 在同一 output/cur/home、output/cur/project、output/cur/tmp 运行，所有自有组停止后将整个 cur **rename** 成 label，再创建下一轮 cur。配置/provider URL、token 内容、端口及 cwd 一致。`run.json.runtime` 和 ports 精确比较。driver 请求到 TS 端口或代理端口的连接地址不进语义投影；HTTP 记录保存 path，原始连接地址可由 argv 追溯。

每份输入使用一个共享 UUID 映射，按固定场景顺序、对象键排序、列表原序首次出现赋号，一直共用于帧、HTTP、落盘目录名/内容。所有 UUID（含 UUID 后的 timeline 后缀、cron session 名）逐个映射，不塌缩成同一 ID。同值引用必须仍为同值，多个不同值不能合并。绑定 generation 单独一一映射，仅当同对象含 connectionId 时生效。固定 runId/sessionKey、非 UUID ID、普通文本和文件路径原样保留。

原帧 `raw` 与 parsed frame 一致、原始 WS 探针 `wireHex` 与 wire/解码一致才能比较。`raw`/`wireHex` 本身留原件，不与归一化后的 UUID 文本逐字节比较。标准官方客户端录完整应用消息（含 notification）；底层帧头/control 由专用原始 TCP 探针录全字节。这不声称 Native WebSocket API 暴露了每个隐式 ping/pong。

recorder.seq 从 1 起逐行连续，用于完整性；语义投影按 connection 拆流。同连接方向、交错顺序、帧字段全部保留；跨连接相对时序不比较。协议 event.seq 从 0 连续，**原值保留**，final 原值保留。不排序通知、事件或目录内文件内容的列表。

## 元数据时钟与进程身份

仅下面的**容器形状 × 字段**归一化，不用广义时间键集合：

| 容器形状 | 允许归一化的字段 |
|---|---|
| transcript entryId、cron taskId、pending requestId | createdAt |
| 项目摘要同时含 lastActivity/name | createdAt |
| cron taskId、session metadata firstPrompt/lastPrompt/isSnapshot | updatedAt |
| 含 sessionId/runId/toolCallId 的运行/工具记录 | startedAt、completedAt |
| run registry 同时含 runId/ownerPid/acceptedAt | ownerPid、acceptedAt、updatedAt |
| 项目摘要含 projectKey/path/name | lastActivity |
| 文件条目含 path/relativePath 与 kind/name/isDirectory | mtimeMs |
| skill 摘要含 slug/scope | mtime |

不会忽略 durationMs、elapsedMs、业务 schedule.runAt/nextRunAt、超时值、预算数字、token 估计、usage、sequence、seq、revision。请求 params 中的同名字段没有以上容器形状时保持原值。数字不做容差。

模型 `requestFingerprint` 是 `sha256(JSON.stringify({...canonicalRequest, stream:true}))`（基线 `src/model/streaming/requestFingerprint.ts`）。工具结果 raw 中 startedAt/completedAt 导致第二步原始 hash 自比不同。装置先验证原始 hash 与同 step 的 model_request.request 对应，再对完整请求归一化后重算 hash；**完整请求本身仍参与逐字段对拍**。缺对应 request、原始 hash 不符或未知 retry hash 都拒绝，不直接抹掉 hash。当前夹具不覆盖重试改写 currentRequest。

## HTTP 与落盘

HTTP 精确比 path、status、完整 body 和其余全部头，JSON body 解析后按上述 ID/元数据规则比较。仅 date（响应时钟）、connection/keep-alive/transfer-encoding（逐跳传输头）不比。**content-length 保留**。SSE/NDJSON body 保留全字节顺序；chunk 切分与毫秒值不比较，独立 oracle 验证增量到达和首末时差，防缓冲到末尾才返回。

扫描真实 home/project 的目录与文件，不用缓存快照冒充原件；JSON/JSONL 保留所有内容并归一化；普通 UTF-8 文本保持全文，二进制以 base64 精确比较；SQLite 用只读 iterdump 比 schema/行内容。拒绝符号链接、坏 JSON/截断 JSONL。只排除 home/logs/、home/telemetry/ 下运行诊断，原始目录仍保存。不排除 transcript、projections、runs/registry、cron、skills、config、token、工具产物。

目录名也用相同 UUID 映射，文件排序为相对路径字典序。夹具仅一个 cron task；多随机 ID 命名文件的集合排序、SQLite 复杂业务表、并发多个会话的时序不是此次校准结论，扩场景须重新 TS 五遍校准。
