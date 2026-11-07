# TUI reducer TS fixture

`reducer.json` was captured from the actual exported `applyGatewayEventToTuiState`, fixed TS `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`, file SHA-256 `85e8528dff0c6a3596415aebbfbb01039b3f90c3a2d64f781e1667fe332696cc`. This is pure code: no network, home or provider access. To regenerate on mbp2, copy `generate.mjs` into a new empty temporary directory, then run:

```
PATH=/Users/da/opt/node-v22.22.0-icu78-darwin-arm64/bin:$PATH /Users/da/ws/pdrs-runner/resources/PilotDeck/node_modules/.bin/tsx /path/to/generate.mjs /Users/da/ws/pdrs-runner/resources/PilotDeck /new/empty/path/reducer.json
```

The Node version, checkout commit and source hash are checked before loading; output is exclusive-created. Compare byte-for-byte and record Node and ICU versions. Events are deterministic and each state comes directly from TS; command parsing has no exported TS function and is not claimed to have this evidence. PTY behavior and real integration remain future work.

Capture: mbp2 Node `22.22.0`, ICU `78.2`; generator SHA-256 `fbadc592f2fa3ecb9a898cee5228f594b6aa344027867bad98eebe238c91640b`; raw `reducer.json` SHA-256 `55cf9de5e8c16e3acedc59f2d3dcce4014da0740fb6cde338cc074a3e3f71422`. Original capture: `/Users/da/ws/pdrs-runner/_tmp/20261108-tui-reducer-capture-utf16/` on mbp2.

同连接 wire：`generate-wire.mjs` 在 mbp2 固定 Node 22.22.0 / ICU 78.2、固定 TS checkout 和两个 client 源文件 SHA 检查后，使用动态 loopback fake WS 调用真正 `RemoteGateway`。运行方式：`tsx generate-wire.mjs <TS源码目录> <不存在的输出目录>`。原件 `wire.json` SHA-256 `f223845bafcd2c227089fc8180f1835224612b5e939be0f93d3dee31a8152cb3`，脚本 SHA-256 `0a68461a95e1ec86be46a596b3612f7a1eb2f58373ca7e0439fca356850db23b`，mbp2 原件 `/Users/da/ws/pdrs-runner/_tmp/20261108-tui-wire-capture-02/`。该 fake 证明客户端帧/事件，不证明 native 权限真实闭环。首采尝试未生成结果，原空目录保留。

流拒绝：`generate-rejection.mjs` 读取相同固定 TS client，隔离 fake 服务返回同 ID `response ok:false`，观察 500ms 后清理并保留结果；`rejection.json` SHA-256 `003e34c20abb469cd22b4effd9f622dc277ac4a2f556da3402140de693d5d134`，脚本 SHA-256 `833ca5779b3129522e35efd596d7bcc409428a2f36ffff8c74a274792e634270`。按上述同样 tsx 命令和新输出目录复采；仅证明该观测窗口内等待。
