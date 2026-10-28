# #983 SDK WebSocket 上限

`generate.mjs` 在 Node v22.22.0 中直接加载基线 `packages/sdk/src/transport.ts`，包装构造器记录实参但不改请求参数；真实本地 ws mock 发送一个未压缩、未分片的 Text 帧。响应 JSON 后追加空白，帧总长分别 16 MiB+1 与 100 MiB+1；成功结果始终 `{sessions:[]}`，不生成大 stdout，也不把大 payload 入库。

实测 SDK 仅传 URL、默认使用 Node 原生 `globalThis.WebSocket`（undici 6.23.0），两个尺寸都可接收。显式替换为 `ws` 的诊断对照才使用默认 maxPayload=104857600；后一尺寸被拒。因此 Rust 显式将 `max_frame_size` / `max_message_size` 设为 `None`，避免误套第三方 ws 上限。这里证明两个实测尺寸，未宣称对任意大于宿主可分配内存的消息等价。

`oracle.json` 只存真实 transport 返回值、尺寸、版本、构造实参数量及 TS transport 的 SHA-256；它不是完整 TS CLI 四面实录。Rust `sessions_list_accepts_ts_large_frames` 启动真实 CLI 重放两个成功格，验证退出/输出与记录的 sessions 投影；旧 77 格继续覆盖完整 CLI 行为。生成器全程最多 30 秒，清理连接和监听；Rust mock 与子进程各最多 15 秒，并沿用已有 D0 端口租约。

`PDRS_TS_ROOT` 可指定拥有相同基线及已安装依赖的只读路径，默认主仓 resources/PilotDeck。不下载运行器。生成目录 recursive=true、文件 flag=wx。重生成守卫：

```sh
cargo test --offline -p pilotdeck-cli --test sdk_sessions_list_exit \
  sessions_list_transport_limits_regenerates_identically -- --exact --ignored --nocapture
```

负控同 `sdk-sessions-list/negative_controls.py`：恢复默认 config→16 MiB+1 格失败；只恢复 64 MiB 消息上限或误设 100 MiB 帧上限→100 MiB+1 格失败；篡改构造实参数量→重生成守卫失败；隐藏一格原生记录→完整性断言失败。精确测试名与 `#983` 断言由 nonce 回执绑定。提交后全量逐项编译、判杀、恢复，未提交树不得绕过 HEAD 守卫。
