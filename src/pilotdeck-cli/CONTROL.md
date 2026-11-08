# pilotdeck-control：调用、流式文本轮次与显式宿主

`status`、`capabilities`、`call` 和 `stream` 连接显式指定的 Gateway，使用已有 hello/request/response 协议，不自动启动宿主。`call` 在连接前拒绝 `submit_turn`；`stream` 提供单轮文本事件流。`serve` 显式启动现有原生 Gateway 前台宿主。`schema` 提供编译登记与可选实时能力发现；`watch` 提供通知与指定状态快照重同步。两者仍为Gateway机器桥，不表示独立/app/v1服务或全局持久事件回放。

```sh
pilotdeck-control --url ws://127.0.0.1:19000/ws --token-file /isolated/token status
pilotdeck-control --url ws://127.0.0.1:19000/ws --token-file /isolated/token capabilities
printf '%s' '{"method":"list_sessions","params":{}}' | pilotdeck-control --url ws://127.0.0.1:19000/ws --token-file /isolated/token call
pilotdeck-control --url ws://127.0.0.1:19000/ws --token-file /isolated/token call --input-file /isolated/request.json
```

`status` 返回版本标记、来源及服务端 `describe_server` 原值；`capabilities` 返回该响应中的原始能力字段。`call` 一次请求成功时返回 `{"ok":true,"result":...}`；服务端成功但未提供 result 时返回 `{"ok":true}`，与明确的 `null` 区分。服务端失败时 stderr 为含 `kind: gateway_error` 和原始错误的 JSON；输入、协议和传输错误使用不同 kind。stdout 仅一条 JSON；帮助文字除外。客户端不主动在错误里打印 URL、token 或请求体。服务端 JSON 字符串的认证 token 明文回显会替换为 `[REDACTED]`：长度至少 8 个 UTF-16 单元时匹配任意子串；更短的 token 只匹配完整值或独立词边界，普通业务数据原样返回。键名脱敏后若发生冲突，后续同名键会增加数字后缀以保留值。此规则不识别编码、拆分或变形的凭据，不能保证阻断所有泄露。

输入 JSON 上限 1 MiB；只支持有限数，整数必须在 JavaScript 安全整数范围内。标准输入与网络等待分别有 10 秒和 30 秒预算；普通文件读取受大小限制，但文件系统卡住时同步文件 IO 无法保证完成时限。收到结果后最多再等一秒清理连接，清理超时不改变已收到的结果。请求不自动重试；连接在请求发送后失败或超时时，操作结果可能未知。URL 与 token 文件必须显式提供，不读取默认家目录。Unix 管道、socket、普通文件可用作 stdin；非 Unix 使用受时限约束的后台读取，尚未在 Windows 目标实测。

调用某方法不代表该方法在目标 Gateway 已装配；以该 Gateway 的真实返回为准。真实运行时、阶段验收和人工使用证据分别记录。


## stream 文本轮次

```sh
pilotdeck-control --url ws://127.0.0.1:19000/ws --token-file /isolated/token stream --input-file /isolated/turn.json
```

输入为一个 JSON 对象，必需非空字符串 `sessionKey`、`runId`、`channelKey`、字符串 `message` 和明确的 `canPrompt:false`；`projectKey` 可省略或为字符串。本片只接受这六个字段，附件、审批、追问和其它扩展输入尚未接入。示例：

```json
{"sessionKey":"chat-1","runId":"run-1","channelKey":"cli","message":"hello","canPrompt":false}
```

stdout 是逐条 flush 的 JSONL，外层含 `version:"pilotdeck-control/1"`、`type:"event"`、请求 `id`、`seq`、`final` 和完整 `event`。序号必须从 0 连续增长；缺号、重复、倒序或缺少必要字段均非零退出，不自动重连或重发。`final:true` 是协议闭合，它的 payload 通常重复上一条终态，消费者不能把它再次当作增量应用。只有 `turn_completed` 且 `finishReason:"completed"`、全程未见 error、最终输出成功才返回 0；取消、未知结果、服务错误和不完整流均非零并在 stderr 提供结构化错误。成功的 `abort_turn` 请求也须结合流终态判断取消结果。

握手与发送沿用 30 秒预算，合法业务轮次不套这个总时限。每条 JSONL（含换行）最多 8 MiB，沿用 128 MiB WS 接收上限；当前只交付一帧，输出由独立线程承担，每条写出并 flush 最多等 10 秒。超限、断管或阻塞输出导致非零退出与关闭；这不是整个进程内存预算，编码和解析仍需暂存数据。

stream 的认证回显过滤保留所有 JSON 键，以及事件顶层 `type`/`finishReason` 协议标识；其它字符串值沿用前述 token 匹配规则。这些公开结构标识不属于过滤承诺，token 恰好与其同名时也保持可消费。普通 `call` 的既有键名规则不变。过滤不保证阻断服务端任意形式的凭据回显。

可从另一进程通过已有 `call` 显式请求取消：

```json
{"method":"abort_turn","params":{"sessionKey":"chat-1","runId":"run-1"}}
```

关闭或丢失流连接可能按现有 Gateway 规则触发整个 session 的中止。SIGINT、输出失败、超时和进程崩溃均不意味着 detach，也不能证明副作用未执行。事件恢复、后台持久连接、审批恢复和前端 Web 传输仍未提供。

真实组合探针：`tests/fixtures/control-stream/native.py`，传入刚构建的三个 Rust binary 和新的独占证据目录；它使用假 OpenAI SSE 服务、独立 home/project、动态端口，验证真实输出、run_get、transcript、另一连接取消和 provider 关闭，不访问真实用户服务。

## serve 前台宿主

```sh
pilotdeck-control serve --home /isolated/home --project /isolated/project \
  --listen 127.0.0.1:0 --token-file /isolated/token \
  --sidecar /explicit/pilotdeck-agent-loop-sidecar
```

home、project、配置、token 和 sidecar 需事先准备，home 与 project 为分离的既有目录；所有权限来源均显式给出。监听仅支持 loopback 的高端口或内核分配的端口 0。原生 readiness/stopping 信息写入 stderr，stdout 没有 JSON ready 协议。SIGINT/SIGTERM 交给现有宿主 drain 流程处理。

token 必须为不超过 64 KiB 的普通文件；CLI 以同一描述符有界读取，再经私有匿名文件描述符交给宿主，避免宿主重新打开被替换的原路径。此入口目前要求 Unix 和可用的 `/dev/fd`。它复用已有服务端装配，配置可能启动 cron/IM；调用方负责独占所选数据根。本片没有自动安装或发现 sidecar、跨进程写锁、默认 CLI 本地 fallback 或完整旧 `pilotdeck server` 的 UI 编排。

参数、token 隔离与真实 Rust sidecar 的验证说明见 [control-stream README](tests/fixtures/control-stream/README.md)。上述真实组合探针的 `--host-mode control-serve --signal TERM|INT` 分别验证两种正常退出；假 provider 结果不等于真实 provider 或人工验收。


## schema 与 watch 机器观察

`pilotdeck-control schema` 离线输出versioned描述，不要求URL/token、不读取stdin。带既有URL/token选项时追加真实describe_server原值；registered_methods只表示编译登记，provider能否调用仍以目标capabilities及实际RPC结果为准。开放params/result Schema不是完整逐字段验证器。

```sh
pilotdeck-control --url ws://127.0.0.1:19000/ws --token-file /isolated/token watch --input-file /isolated/watch.json
```

watch文件可含sessionKey/runId/projectKey和100..60000的intervalMs（默认1000）；runId需sessionKey。无文件时仅服务状态及该认证连接实际通知，不读取stdin。JSONL含version/type/epoch/seq/cursor:null；通知payload和快照沿既有脱敏。每次附着/重连显式resync_required，刷新独立describe_server/active_turn_snapshot/run_get快照，再resync_complete；duringResync、snapshotsAtomic:false和notificationReplay:false明确交接边界。没有全局持久notification cursor；cursor输入拒绝，run_events的run局部游标不冒充它。当前状态去重，断线只重试只读查询，绝不重放写请求。

SIGINT/SIGTERM停止观察；正常最后stopped/exit0，阻塞输出无法交付最终行则有界output_error/exit1。每RPC30秒，交付10秒/8MiB行，停止后的最后行最多500ms，退避最多5秒；不承诺文件系统同步IO卡住时也可限时。初连失败、认证/协议/能力及输出错误明确退出；重连刷新不表示补齐瞬时通知或原子快照订阅。watch关闭只读连接不主动abort其它任务，不代表旧submit连接具有detach语义。

实现及真实进程用例为src/control_machine.rs与tests/control_watch.rs；测试/原生组合和平台实测以本轮实际结果为准，不因源码存在称通过。

## 双向机器会话

`pilotdeck-control --url <ws-url> --token-file <file> connect` 接收逐行 JSON-RPC 2.0，请求需带 id；stdout 只有响应和通知。`initialize` 的 `protocolVersion` 为 `pilotdeck-connect/1`，返回实际 `describe_server` 与连接边界。`gateway.call` 的参数为 `{"method":"...","params":{...}}`；`run.submit`、`run.cancel`、`permission.respond` 分别接真实 `submit_turn`、`abort_turn`、`permission_decide`。其他交互与上传方法通过同连接 `gateway.call` 调用，避免权限应答丢失连接归属。

流使用 `run.event` 通知（requestId、seq、final、event），最后再给对应请求响应；调用方必须检查 terminal.finishReason，取消不等同正常完成。至多64个未结请求、每行1MiB；无自动变更重放。EOF/中断关闭连接，沿用Gateway关闭可能取消活动turn的合同。断线后可显式新连接查询 `run_get`、`run_events`、历史与活动快照；本入口不提供 `/app/v1` 的默认detach或全局原子订阅语义。
