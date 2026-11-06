# api-web 路由组 TS 金样

固定基线 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，Node v22.22.0 / ICU 78.2（生成器两项都断言，oracle 头记录 node 与 icu）。生成器以 `startGatewayServer` 起一个真实 TS 服务器：`gateway` stub 固定返回一个项目、`describeProject` 对 key `boom` 抛错，其余原样走 `adapters/web/httpRouter.ts` 与 `adapters/web/projectFiles.ts`（`ProjectFileService` 用真实文件系统）。每个 cell 用裸 socket 写入原始请求字节并原样捕获响应字节（250 ms 静默窗、EOF 或错误结束），`date` 头掩码。运行前核 commit 与三个源文件的 SHA-256。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/api-web/generate.mts src/pilotdeck-gateway/tests/fixtures/api-web
cd src && cargo test -p pilotdeck-gateway --lib api_web
```

## 夹具树与掩码

临时目录下建 `proj/`（根 mtime 2021-02-03T04:05:06.789Z）：

- 目录 `sub`（2020-01-02T03:04:05.006Z）、`sub2`（2021-02-03T04:05:06.789Z）；
- 文件 `aaa.txt` `alpha\n`（OLD）、`bbb.js`、`binary.png`（`00 01 02 ff`）、`data.bin`、`empty.txt`（0 字节）、`UPPER.TXT`、`config.env`、`README`（无扩展名）、`big.bin`（1 MiB + 1，超 `maxReadBytes`）、`space name.txt`、`sub/child.txt`（OLD）；
- 符号链接 `inside-link -> aaa.txt`。`utimes` 跟随链接，故链接 cell 记录的是目标 mtime（TS 侧用 `stat`，同样跟随）。

所有文件与目录 mtime 用 `utimes` 钉死（先文件、再目录倒序、后根），`modifiedAt` 因此是稳定字面量。目录 `size` 随文件系统变（APFS 96/64 vs ext4 4096），统一掩码成 `"type":"directory","size":0`。机器路径一律不外泄：请求里 `<root>` 展开成 `encodeURIComponent(root)` 上线路，响应里编码形态与原始形态都回填成 `<root>`。

## 已测场景族（79 cell）

- 鉴权：缺 / 错 token / 小写 `bearer` / `Bearer\t` / 尾随空格 / `Basic` / 空值 / 重复头两种顺序（Node 取第一个重复头）；均落在 `/api/web/projects`。
- 方法与路由：GET / POST / PUT / HEAD（HEAD 不走 GET 分支，落到最终 404 且无 body、无 TE/CL）、`/api/web/projects/` 尾斜杠、`/api/web/`、`/api/web`（不掉进 web 路由，落 native 纯文本 `not found`）、`/api/web/nope`、`files/nope`、`files%2Ftree`（不匹配正则）、GET 打 write。
- describe：正常、尾斜杠、POST、key 坏百分号（`decodeURIComponent` 抛错在 try 内 → 404 `project_not_found` "URI malformed"）、`a+b`、stub 抛错 → 404 带错因。
- tree：根 / 子目录 / 空 path / `.` / `sub/..` / 不存在 / 指向文件 / `..`（403 越界）/ 绝对路径（403）/ `..foo`（`normalize` 后同样以 `..` 开头 → 403，与 `..` 同类）。目录在前、同类按 `localeCompare`（ICU：小写先于大写）。
- read：文本、`./` 与 `sub/../` 归一、重复 `path`（取第一个）、`+`→空格、`%2F` 解码、二进制扩展名 base64、无扩展名走 base64、大写扩展名、`.env` 走文本、空文件、1 MiB+1 超限、符号链接、缺 query、空 path、`../..` 越界、绝对路径、不存在、指向目录（`Not a file`）、query 坏百分号（`URLSearchParams` 宽松解码成 U+FFFD，随后 `stat` ENOENT → 500）。
- write：文本、base64、非法 base64（Node 宽松解码）、嵌套目录自动建、多余字段忽略、坏 JSON / 空 body / 缺 path / 空 path / 非字符串 path 与 content / 非法 encoding / `encoding: null`、`..` 越界、绝对路径、`..foo`（同样 403）；`files-write-round` + `files-read-round` 是两次独立请求（同一连接上流水线 write+read 在 Node 里会与 `await writeFile` 竞态，故拆开）。
- 传输：同连接两次 keep-alive 复用、`Connection: close`、HTTP/1.0（Node 始终回 `HTTP/1.1` 状态行 + `Connection: close` + 无 framing 的 close 定界体）。

## Rust 重放规则

- 重放在 `src/api_web/tests.rs`：同一请求字节经真实 TCP 监听器打到 `http_proxy::serve_connection_native_with_shutdown`，api-web 组件前置、`native_http::serve` 兜底；响应按 #1209 规则归一化（Date 掩码、头名小写、空 `content-length: 0` 删除、chunk 大小写小写），`closed` 标志另比。
- `http10` 为字节 pin（hyper 写 `HTTP/1.0` 状态行，Node 写 `HTTP/1.1`，已登记在 `specs/gateway-parity/diffs/A8.4-API-WEB-001.md`），不进归一化比对；`replay_cells_cover_exactly_the_oracle` 断言重放列表 + pin 列表 = oracle cell 全集。
- 不在金样里的一格：files 分支上 **项目 key** 的坏百分号（`decodeURIComponent` 在 try 外）。TS 侧是 unhandled rejection——零字节、连接保持打开，客户端只能等超时。Rust 不回零字节挂起，改为 500 `internal_error` "URI malformed"，差异登记在 A8.4-API-WEB-001.md 并用独立测试 pin 住。

## 重生成守卫

`#[ignore]`：`PDRS_BASELINE=… cargo test -p pilotdeck-gateway --lib api_web::tests::regenerates_identically -- --exact --ignored --show-output`。生成器输出目录递归创建，`oracle.json` 以 `flag:'wx'` 独占创建。除 Date 掩码与目录 size 掩码外无时钟 / PID 依赖；mtime 全部 utimes 钉死，未登记 regeneration mask。

## 负控

`negative_controls.py`（`_shared/mutant_switch.py` 编译开关执行器）：鉴权闸、边界闸（`isAbsolute` / `..` 检查）、`project_describe` 的 catch、文本扩展名判定、方法闸、`maxReadBytes` 上限、#1675 的 NOFOLLOW 句柄加固共七个开关，每个绑定专属 replay cell 或边界探针的单行断言，去掉后必须变红（KILLED）。`api-web-nofollow-off` 绑定 #1675 的八条探针：写侧五条（确定性中间段目录链接、中间段目录竞态换链、整根竞态换链、末段叶竞态换链、缺根时祖先竞态换链）、读侧两条（末段叶竞态换链、中间段目录竞态换链）、枚举一条（目录竞态换链），去掉加固后八条都必须变红。新增 cell 时同步更新 `src/api_web/tests.rs` 的 `replay_tests!` 表与开关绑定。
