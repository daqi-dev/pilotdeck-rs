# #1334 A8.3-ch-webhook webhook channel 金样

`generate.mts` 在 Node **v22.22.0 / ICU 78.2** 上读取固定基线 **cc7c4e6** 的 `WebhookChannel.ts`、`WebhookSessionMapper.ts`、`webhook-render.ts`、`ImCronDelivery.ts`、`agentStatus.ts`，esbuild 转译后原样执行。共享基线守卫核提交与源闭包哈希。`node:http` 被换成记录 Server 的 shim（拿绑定的临时端口），仅 gateway 换成记录 `submitTurn` 并回放脚本事件的 mock；其余（监听、路由、限流、去重、HMAC、extractText、渲染、`/new`、cron 投递）都是原 TS 代码。

请求用**裸 socket**（latin1）发出，`oracle.json` 记录线上字节归一化后的结果：状态行、小写头序列（`date` 置 `<date>`）、body（chunked 时 chunk 尺寸行小写）。两种收尾模式：`close`（一次请求后等连接关闭）与 `keepalive`（同一 socket 连发 count 次）；`none` 是 400ms 静默探针，专门观测 null JSON body 永不回包的挂起格。两种「一个字节都没收到」的收尾分开记：探针到点是 `noResponse`（socket 仍开着），对端零字节关闭是 `closed`——越界格用的就是后者，Rust 侧同样分 `NoResponse` / `Closed`，所以越界路径若回归成「挂起不答」，该格会转红而不是照样 PASS。21 个场景覆盖：健康检查与路由（含 pin 的原始字节、GET/OPTIONS 404、未知方法 FROB 的解析器裸 400）、未知路由、限流、超长 body（越界零字节关闭 `closed` + 恰好 maxBodyBytes 的边界接受）、五种签名变体、`__INSECURE_NO_AUTH__`、重复投递、无文本（含 `{"text": ""}` 空串字段——TS `if (!text)` 视为无文本，回 ignored 且不调 gateway）、GitHub 提取、提交渲染、`deliver:'http'` 截断、提交错误、`/new` 命令（固定 uuid mapper）、gateway 缺失、无 secret 路由、端口占用、非法端口、cron 投递、停止再启动，以及 #1684 的 HTTP/1.0（带 `Connection: keep-alive`，录制的是 Node 回 `HTTP/1.1` 状态行 + `Connection: close`；Rust 的状态行差异按 A8.3-CH-WEBHOOK-004（继承 A8.4-NATIVE-HTTP-001 第 4 项）由 pinned 直测钉住），以及 #1682 的 `/webhooks/<Object.prototype 自有属性名>`（`constructor` / `__proto__` 带全局 secret 签名回 200，`toString` 不带签名回 401，`nope` 仍 404——`this.routes[name]` 走原型链，`__proto__` 取值器返回 `Object.prototype` 而非 `null`；`Object.getOwnPropertyNames(Object.prototype)` 的实测集合由 `channel::webhook::tests::object_prototype_names_match_the_measured_v8_set` 钉住）。

去确定化处理：`x-delivery-id` 显式给出；日志里的端口固定显示 `http://127.0.0.1:0`（`listening on` 行只含配置端口，配置即为 0）；失败启动日志尾部（含 errno）置 `<masked>`；`port-in-use` 场景记录哨兵 `"<busy-port>"`，重放端自建 blocker 换真实端口。handleIncoming 是 fire-and-forget，每场景用静默轮询（25ms × 至多 200 次，连续两次稳定即止）等日志/网关流平稳后再捕获。每场景结束后 stop 监听器，进程可正常退出。

Rust 重放（`channel::webhook::tests`）按同一步骤 DSL 驱动原生实现，比较响应、gateway 调用、日志、cron 布尔。已登记差异在重放端归一：hyper 对解析器裸 400 补的 `content-length: 0`——#2153 起仅在 `PARSER_ADMISSION_LABEL`（`unknown-method`）登记步丢弃，其余步逐字比，与录制侧 `generate.mts` 同一常量语义（A8.4-NATIVE-HTTP-001）；Node 的 `keep-alive: timeout=5` 头丢弃（hyper 不发）；Date 值本身两侧都置 `<date>`。重生成到**新的**目录：

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/webhook-channel/generate.mts /new/evidence/output
```

忽略测试 `webhook_channel_regenerates_identically` 必须在具备固定基线的宿主显式运行。Rust 直测补金样覆盖不到的状态格（假钟 prune 边界、限流窗口滑动、activeChats 重入、turn 中 stop、双 stop、热重载端口释放、原生 `/health` 原始字节 pin），不冒充 TS 行为。

#1813：新增 1 个开关（共 **46** 个）：`zero_step_cells_are_the_two_named_cells` 钉住「只有 `port-in-use` / `invalid-port` 两例可以是零步」，某个 cell 丢掉整段 `steps` 时不再静默退化为零覆盖重放。

#1696 / #1684：新增 3 个开关（共 **49** 个）：`webhook-http10-finalize-off` / `webhook-http10-framing-off` 与 `webhook-oracle-recorded-version`，分别钉 1.0 响应的版本、close-delimited 帧化，以及 `steps[].version` 的 fail-closed 读取。

#1729：`negative_controls.py` 当时增至 **49** 个开关（以 `switches` 表为准；`#1729` 之前为 20 个），新增 26 个录制字段保护，覆盖每个录制字段的读取点（含终态比对的 `expected` 容器）。每项先编译开关分支，再对普通断言运行正控、变异、复原；错型诊断包含 #1334、字段和实际值；既有 webhook 差异条目（`A8.3-CH-WEBHOOK-001/002/003`）不含开关计数判据。`steps` 按必填字段读取：键缺席即失败（不再静默变成零步重放），但空数组合法——`port-in-use` / `invalid-port` 两例本就录制为零步，故不照搬 sms 的空数组拒绝。

#1682：`negative_controls.py` 现有 **50** 个开关（新增 `webhook-prototype-routes-off`：恢复「只查自有键」的旧查找，`object-prototype-route-names` 单元格四步里前三步转红——`nope-signed` 既非自有键也非原型名，修前修后都是 404）。金样 `expectedCaseCount` 20 → **21**。

#2153 / #2789：新增 2 个开关（共 **52** 个）：`webhook-replay-cl0-mask-always` 钉「重放归一化的 `content-length: 0` 掩码只作用在 `unknown-method` 登记步」（`normalize` 无视作用域即红），`webhook-replay-cl0-wide` 钉「`fn parser_admission` 不得放宽」（放宽后唯一性守卫 `unknown_method_is_the_label_of_exactly_one_oracle_step` 读同一谓词转红）。每步只一个开关，与 `generate.mts` 录制侧同一常量语义（A8.4-NATIVE-HTTP-001）。
