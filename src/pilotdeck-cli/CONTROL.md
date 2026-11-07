# pilotdeck-control：单次 Gateway 调用

新入口连接显式指定的 Gateway，使用已有 hello/request/response 协议，不启动第二个宿主。它不是持久连接、事件流或恢复接口；`submit_turn` 需要流式客户端，本入口在连接前拒绝。`schema` 和 `serve` 尚未提供。

```sh
pilotdeck-control --url ws://127.0.0.1:19000/ws --token-file /isolated/token status
pilotdeck-control --url ws://127.0.0.1:19000/ws --token-file /isolated/token capabilities
printf '%s' '{"method":"list_sessions","params":{}}' | pilotdeck-control --url ws://127.0.0.1:19000/ws --token-file /isolated/token call
pilotdeck-control --url ws://127.0.0.1:19000/ws --token-file /isolated/token call --input-file /isolated/request.json
```

`status` 返回版本标记、来源及服务端 `describe_server` 原值；`capabilities` 返回该响应中的原始能力字段。`call` 一次请求成功时返回 `{"ok":true,"result":...}`；服务端成功但未提供 result 时返回 `{"ok":true}`，与明确的 `null` 区分。服务端失败时 stderr 为含 `kind: gateway_error` 和原始错误的 JSON；输入、协议和传输错误使用不同 kind。stdout 仅一条 JSON；帮助文字除外。客户端不主动在错误里打印 URL、token 或请求体。服务端 JSON 字符串的认证 token 明文回显会替换为 `[REDACTED]`：长度至少 8 个 UTF-16 单元时匹配任意子串；更短的 token 只匹配完整值或独立词边界，普通业务数据原样返回。键名脱敏后若发生冲突，后续同名键会增加数字后缀以保留值。此规则不识别编码、拆分或变形的凭据，不能保证阻断所有泄露。

输入 JSON 上限 1 MiB；只支持有限数，整数必须在 JavaScript 安全整数范围内。标准输入与网络等待分别有 10 秒和 30 秒预算；普通文件读取受大小限制，但文件系统卡住时同步文件 IO 无法保证完成时限。收到结果后最多再等一秒清理连接，清理超时不改变已收到的结果。请求不自动重试；连接在请求发送后失败或超时时，操作结果可能未知。URL 与 token 文件必须显式提供，不读取默认家目录。Unix 管道、socket、普通文件可用作 stdin；非 Unix 使用受时限约束的后台读取，尚未在 Windows 目标实测。

调用某方法不代表该方法在目标 Gateway 已装配；以该 Gateway 的真实返回为准。真实运行时、阶段验收和人工使用证据分别记录。
