# #1209 原生 HTTP 基础路由金样

`generate.mts` 用固定 Node v22.22.0 加载真实 `GatewayServer.ts`（PilotDeck@cc7c4e6，
stub 依赖同 native-ws-entry 模式），起四个服务器：`a`（127.0.0.1，静态根含
index.html 等 11 个固定文件）、`b`（127.0.0.1，静态根仅 only.txt，无 index）、
`c`（127.0.0.2，allowRemoteHost，静态根同 a）、`d`（localhost，无静态根）。
88 个 cell 逐个发送原始请求字节，记录响应原始字节（`date:` 值掩码为
`date: <masked>`，全行、跨管线化多响应）与 closed 标志，覆盖：

- health：35 个 llhttp 方法中代表性的 16 个 × query/fragment/点段/`%2e`/
  反斜杠/双斜杠/绝对形式/大小写/尾斜杠；`Connection: close` 请求（GET/HEAD）
  以 `Connection: close`、无 Keep-Alive、关连接应答；
- local-token：回环返回 token、127.0.0.2 直接 404、localhost、query、
  无关 Authorization、尾斜杠走静态；
- 静态回退：扩展名映射（大小写敏感）、目录、缺失回退 index、
  `enc%20oded` 字面名、root2 无 index → 404；绝对形式
  `http://127.0.0.1//app.js` 保留 `//app.js` 路径（非协议相对折叠）；
  `static-upper-lower`（`GET /upper.txt`，夹具只写 `upper.TXT`）是唯一
  依赖宿主文件系统大小写敏感性的 cell：大小写敏感（录制平台 Linux）
  缺失、回退 index；大小写不敏感（macOS 默认 APFS）解析到 `upper.TXT`、
  TS 与 Rust 一致地服务该文件（#1359 实测 `_tmp/20261026-issue-1359/`
  `probe-macos.out`，登记 `specs/gateway-parity/diffs/`
  `A8.4-NATIVE-HTTP-003.md`）。回放按运行时探测夹具根选期望，不写死
  cfg；其余 cell 的请求路径要么与夹具文件精确匹配、要么无大小写变体
  可碰撞，`/HEALTH` 等路由比较在内存中进行，不涉文件系统；
- 穿越：URL 规范化中和后落到 index 回退或目标文件
  （`/%2e%2e/app.js`、`/sub\..\app.js` 服务 app.js 本身）；
- 解析层：FOO/get/GETX 裸 400 关连接、非 ASCII 路径/查询 400、无 Host 400、
  垃圾请求行、`%2Fhealth`、authority-form（`GET 127.0.0.1:1`）、空请求行、
  CONNECT 零字节销毁（非 ASCII target 的 CONNECT 在字节检查处 400）、
  非字母 scheme（`a1://`）裸 400、管线化与未读请求体管线化。

`oracle.json` 同时记录 Node `http.METHODS`（llhttp 方法表，Rust 侧方法
允许表的正本来源）。

## 重生成

```sh
PDRS_BASELINE=/home/clawbox-08/ws/pdrs-runner/resources/PilotDeck \
/home/clawbox-08/opt/node-v22.22.0-icu78-linux-arm64/bin/node \
  --disable-warning=ExperimentalWarning \
  --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  tests/fixtures/native-http-basic/generate.mts <独占输出目录>
```

输出目录只需要存在；`oracle.json` 以 `wx` 写入。Rust 侧守卫
`native_http::tests::regenerates_identically`（ignore）重生成并逐字节比较
（date 掩码后）。两次连续运行已验证逐字节一致。该守卫须在录制平台
（Linux，大小写敏感文件系统）上跑：在大小写不敏感宿主上重生成，
`static-upper-lower` 这一个 cell 会得到 upper 文件的字节（见上），逐字节
比较在该宿主上不成立（见 #1359 登记，不改金样、不改共享重生成机制）。
