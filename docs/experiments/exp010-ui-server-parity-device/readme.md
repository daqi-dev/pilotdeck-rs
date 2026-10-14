# exp010：ui/server 对拍装置的录制层、运行时枚举与覆盖矩阵

2026-10-14，GitHub #47（B2 第二个 PR），基于 `60ca8e5`。本次范围是计划 003 Issue B2 的 **S1 + S2**。实验原拟用 exp007，但编号已被前端占用，改用 **exp010**。S3 比较器、S4 规约与冻结是下一个 PR，本次的 PASS 只表示录制/场景/覆盖检查成立，不代表 Rust/TS 行为等价。

**状态：第 3 轮修复的离线检查 PASS；协调者 run2 六场景 × 直连/反代通过，但录制含误记的 `http_aborted`，需 run3 按新完整性门重录。** worker 沙箱禁止 loopback listen，没有启动真实服务，也没有用离线结果冒充端到端证据。完整交接与最不确定的三处见 worktree 的 `_tmp/RESULT.md`。

## 组件与拓扑

- `scripts/exp008/proxy/src/main.rs`：复用 B1 Rust bin，新增 `EXP010_RECORD_LOG` 模式；没有该变量时继续作为 B1 被测入口。
- `scripts/ui-server-parity/recording.rs`：JSONL 写盘、原始头多值、请求凭证遮蔽、HTTP 流式 body tee、WS 原始帧观察器。crate 未新增依赖或修改 lockfile。
- `scripts/ui-server-parity/enumerate_routes.mjs`：S1 枚举入口，复用 `scripts/exp008/enumerate_hook.mjs` 的真实 Express stack dump。B1 并没有独立的 `enumerate_routes.mjs`，而且 `Router.prototype` hook 在 Express 4.22 不命中，不能用它猜分母。
- `scripts/ui-server-parity/run.py`：复用 B1 的临时目录、mock 模型、按进程构造 env、lsof 回环断言与自有进程组清理；不调用 B1 比较器或旧场景；真实服务录制关闭 B1 的浅层 polling 替代（EXP008_POLL_WATCH=0），使用原生 watcher。
- `driver.mjs` + `scenarios.json`：真实 ui/server/gateway 协议驱动与直接 HTTP 用例定义；后端不 mock，只 mock 模型回答。
- `coverage.json` + `check_coverage.py`：路由 × HTTP 用例矩阵及声明/执行证据两种检查。

两侧**顺序运行、分别冷启动、各有全新的 home/project/auth.db/config**，不共享可变后端。端口可复用，数据不可混用：

```text
直连侧：driver → recorder 127.0.0.1:13101 → TS UI 127.0.0.1:13102 → TS gateway 127.0.0.1:28990
反代侧：driver → recorder 127.0.0.1:13101 → B1 DUT 127.0.0.1:13103 → TS UI 127.0.0.1:13102 → gateway :28990
带外活动观察：driver ──────────────────────────────────────────→ TS UI :13102（两侧各取自己这份）
枚举：真实 UI 单独监听 127.0.0.1:13104；13105 预留前端，本轮不启动 Vite/浏览器
```

「直连」指无被测 DUT；录制器本身仍在线上，以同一格式落两侧 HTTP/WS 证据。**活动基准连录制器也绕开**。录制器只读取 `X-PilotDeck-Handler`，不添加/覆盖它：直连 TS 通常记 `handler:null`，经过 B1 DUT 记 `ts`；未来接管路由可记 `rust`。本 PR 不声称存在已接管路由，不实现 S3 的 Rust 处理侧硬门。

所有实际监听必须是 `127.0.0.1`，就绪后由自有 PID 的 lsof 结果断言。`PILOT_HOME`、`DATABASE_PATH`、`PILOTDECK_CONFIG_PATH`、`WORKSPACES_ROOT`、`TMPDIR` 全部指向本轮产物目录。gateway 与 UI 共享本侧同一 home 和 0600 `server-token`。只给 `Popen(start_new_session=True)` 创建的自有进程组发 SIGTERM/SIGKILL，从不按端口杀进程。不下载浏览器或其他预编译二进制。

## 录制格式（schema 1；S3 的输入接口）

每条 JSONL 记录都有全局单调 `seq` 和 `arrival_ns`（**十进制字符串**，单调时钟距 recorder 启动的纳秒数）。同一 WS `connection` 的记录是该全序的严格递增子序列，不要求连续；HTTP 使用 `id` 关联 request/response/body。两份日志的 seq/时钟起点独立，不应跨侧直接比较绝对值。

| kind | 内容 |
|---|---|
| `run_start` | schema、listen/upstream、时钟与 body 编码 |
| `http_request` | id、method、path、query、headers 列表、驱动提供时的 case 关联键 |
| `http_response` | id、status、headers 列表、上游 `X-PilotDeck-Handler`；WS 101 另带 connection |
| `http_chunk` | id、direction=request/response、**该块起始 byte offset**、bytes、body_b64；trailer 帧另记 trailers 列表 |
| `http_end` | 本方向完整 EOF，或已读满构造 body 时的精确长度；总 byte offset；已知空 body 也记结束 |
| `http_body_error` / `http_aborted` | 传输错误或消费者中断，保留已录制部分；不能当完整 body |
| `recorder_error` | 录制器生成的 400/502/500、body_b64，区别于 DUT 响应 |
| `ws_frame` | connection、leg、dir、FIN、RSV、数值 opcode、masked、payload_b64（解除 mask 后）、wire_b64（原始帧）；含 continuation/binary/ping/pong/close |
| `ws_partial` | 断开时未完整到达的 WS 帧字节，不能当完整帧 |
| `ws_end` | 连接退出与 error；TCP 异常断开也留记录 |
| `run_end` | SIGINT/SIGTERM 正常收尾标志；进程异常退出不能只看已有日志就 PASS |

头是 `[{name, value_b64}, ...]`，**不是 dict**；`get_all`/iteration 保留同名重复值（包括 Set-Cookie、Connection），base64 保留非 ASCII 字节。请求 Authorization/Cookie/API-key/凭证 token 头整值替为 `<redacted>`，query 的 token 参数也遮蔽（包含 percent-encoded 参数名）；其他 query 顺序与重复项保留。**请求凭证只遮头与 query；请求 body 不解析、不遮蔽，注册/登录等 body 中的密码、token 等凭证会原样以 base64 落盘。** 响应头/body 同样原样记录，所以请求凭证和鉴权响应里的临时 token 都仍在本轮 `_tmp` 证据中，勿把真实录制产物直接入库。

驱动给请求加 `X-Pdrs-Parity-Case`，在录制 metadata 中关联 HTTP 用例或 WS 场景；同一字段贯穿请求头与录制 id，后续 S3 不必靠时间猜重复请求的用例归属。该头在两侧一致，不作为处理侧证明。

HTTP tee 逐块转发，**不攒完整响应**。录完帧后检查 inner body 的 `is_end_stream()`，已结束时立即补记 `http_end`；Hyper 读满 Content-Length 后可能直接丢弃 body，不再轮询 EOF，因此 Drop 还用构造时保存的 `size_hint().exact()` 与累计 offset 判断是否完整，未读满或未知长度的 Drop 仍记 `http_aborted`。记录块边界仅用于诊断，S3 不能用块数/边界当 SSE 硬门。本次没有设 k/m、没有归一化 token/ID/revision、没有引入比较规则。

WS 录制模式用 Hyper 的双向 Upgrade 保留握手后的预读字节，随后 raw duplex 搬运。旁路帧解析器只观察字节，不重组消息、不自行产生 pong/close；单帧录制上限 16 MiB。四个 leg 区分 `downstream-read/write` 与 `upstream-read/write`，同一帧在入/出边界各有记录。S3 应明确选择边界（例如 downstream-read 的上行 + downstream-write 的下行），**不得把四条腿混起来当四个应用事件**。B1 原有消息级 tee 的 `ws_message` 是另一模式，不用作 exp010 的全帧证据。

每条写盘事件在 `write + flush` 成功后才确认；没有异步批次静默丢弃窗口。写失败设置共享 fatal 标志，HTTP body/WS 传输立即返回错误，主循环独立 interval 检查并非零退出。协调者 runner 还在进程收尾后要求 recorder exit=0、日志 start/end 与连续 seq 完整、每个 HTTP 用例在 recorder 中有对应请求/状态码。**每个 HTTP 用例的每次请求及响应 body 都必须以唯一 `http_end` 收尾，chunk 与结束 offset 必须符合已录字节，有 Content-Length 时总 offset 必须等于它；用例关联记录中出现 `http_aborted` 或 WS 连接的 `ws_partial` 即拒收。** 这避免写失败恰好发生在驱动结束前而漏判。这个完整性门不做行为 diff。同步写盘会增加录制开销，真实延迟数据需协调者检查，S3 再定口径。这里的 flush 不宣称抗断电 fsync 持久性。

## #53 前置闭环

| #53 问题 | S1/S2 落点 |
|---|---|
| snapshot 与 activity 一起被 DUT 恒偏移可假通过 | `driver.mjs` 创建直接连 13102 的常驻 observer；auth 后、并发对话后、watch 重连后、场景后分别取 snapshot，整个期间 raw activity 也保留到 `activity-direct.jsonl`；既不经过 recorder，也不经过 13103 DUT |
| 单用户/无并发、全局 registry revision 与用户 snapshot 作用域不同 | 记录真实 user_id、每个 session/run 的原始活动和各次完整 snapshot；chat 同时提交两个会话。**不求用户 snapshot 的最大 revision，不推算全局基线，不减统一 offset**。跨用户导致的全局 revision 空洞仍保留；将来 S3 按本侧带外证据建立对应关系。基线公开注册本身只允许单用户，本轮不伪称已跑多用户服务 |
| Connection 非 ASCII 值被跳过，提名头泄漏 | HTTP/WS 共用 byte split + OWS trim + `HeaderName::from_bytes`；任一非法请求 token 在转发前回 400。上游非法 Connection 记错误并回 502，不静默略过 |
| Set-Cookie 等重复响应头被 dict 覆盖 | Rust HeaderMap 值列表 + base64；Node 客户端使用 rawHeaders 平面列表，二者都不压成 dict |
| 非 JSON 帧导致驱动崩且证据丢 | recorder 从不解析 JSON；driver 先落 raw base64 后尝试 JSON，失败另记 parse_error 并继续，binary/control 仍保留 |
| 写日志线程失败当批静默丢失 | 移除线程批处理，逐条确认写盘；fatal 标志使退出码非零；注入 Broken writer 的离线测试保证失败可见 |

这里解决的是**录制前置**。本 PR 不提供通用 revision 比较器；S3 必须消费带外日志，禁止回头使用经过 DUT 的 snapshot 作为归一化基准。

## 六场景与覆盖门

| 场景 | 驱动内容 | 产物/边界 |
|---|---|---|
| auth | setup、无凭证 user 401、register、错误密码 401、login、Bearer user | 请求凭证头遮蔽；响应 body 原始 |
| readonly | 项目、会话、消息、配置、模型；files/model 穿透与 MCP read 遮蔽 | MCP `/config/read` 预期 400，由先注册 `/:scope` 遮蔽；其独立注册路由在矩阵中显式豁免 |
| chat | 两条会话并发提交，一轮 deterministic mock 模型响应直到 complete | UI/gateway 真服务；observer 的真实 revision/snapshot 带外记录 |
| watch | watch → close → reconnect → watch/state/snapshot → unwatch；再做 PTY 哨兵、断线复用、resize/stty、exit | PTY 哨兵完整字符串不在输入中，防止回显假满足；沙箱外必须真成功，失败不降成 PASS-with-note |
| config | GET 配置、PUT raw YAML（保持 mock/runtime 配置）、等待 `config:reloaded` WS 广播 | 隔离 home 文件写与广播真实发生 |
| upload | manifest → 先订阅 SSE → multipart content → completed → GET 结果 | 同时记录 HTTP body 与 SSE chunks；真实上传到临时 home |

上传场景还记录真实 modules GET 的三种续传输入：Last-Event-ID、lastEventId、afterSeq。**此夹具没有 StaffDeck host，预期 501，仅覆盖未配置 host 的分支，不证明续传语义**；scenarios manifest 已写明 scope。完整 host 续传 fixture 仍待后续补，不能拿这些请求宣称已验证「不重放/不丢」。

矩阵用 B1 已保存的 **263 条唯一运行时路由**预填，当前 **24 个直接 HTTP 用例覆盖 19 条路由、244 条显式豁免**。每个豁免均有 `{route, reason, approver:"AI 代批，待人复核"}`，多数是「首批六场景未驱动、目前仍留 TS，接管前补例或重新批准」。这不等于 100% 实测覆盖，也不是接管许可。

`check_coverage.py` 必须显式传本轮 `--enumeration`；命令无默认静态分母。声明模式检查每条枚举路由都有直接 HTTP 用例或有效豁免、case 定义一致、新路由/过期行/重复行都 FAIL。`--evidence cases.jsonl` 另要求每个登记用例在 direct/proxy 两侧都有匹配 method/path/route、符合 manifest 状态码且 ok=true 的实际 HTTP 记录。**evidence 模式下，只要某路由任一 `case_http` 记录的实际 `handler` 为 `rust`，该路由的豁免 approver 含「AI 代批」就 FAIL，必须人批；失败记录和未登记用例的记录也参与这项检查。其他路由暂允许 AI 代批，目前无 Rust 接管路由。** 声明模式不推断实际处理侧。**声明 PASS 不能当执行 PASS**；S3 还需读取 recorder 中的处理侧与真实响应作比对。

## 可复现命令

从本 worktree 根目录执行。Rust 依赖是源码构建，使用本 worktree 的 CARGO_HOME；首次缺少源码缓存时可由协调者准备源码依赖，命令不会下载预编译二进制。

```bash
export CARGO_HOME="$PWD/_build/cargo-home"
export CARGO_TARGET_DIR="$PWD/_build/exp010-target"
python3 -m py_compile scripts/ui-server-parity/*.py
node --check scripts/ui-server-parity/driver.mjs
node --check scripts/ui-server-parity/enumerate_routes.mjs
cargo fmt --check --manifest-path scripts/exp008/proxy/Cargo.toml
cargo clippy --locked --offline --manifest-path scripts/exp008/proxy/Cargo.toml -- -D warnings
cargo test --locked --offline --manifest-path scripts/exp008/proxy/Cargo.toml
python3 scripts/ui-server-parity/test_coverage.py
python3 scripts/ui-server-parity/test_recording.py
# 历史分母的声明检查，仅是离线种子验证
python3 scripts/ui-server-parity/check_coverage.py --enumeration docs/experiments/exp008-ui-server-proxy/router-enumeration.json
```

协调者在允许 loopback listen、能起原生 PTY 的环境跑完整录制；输出目录必须全新，失败证据不会覆盖：

```bash
python3 scripts/ui-server-parity/run.py \
  --baseline /Users/da/ws/pilotdeck-rs/resources/PilotDeck \
  --out _tmp/20261014-exp010-run
python3 scripts/ui-server-parity/check_coverage.py \
  --enumeration _tmp/20261014-exp010-run/enumeration/router-enumeration.json \
  --evidence _tmp/20261014-exp010-run/cases.jsonl \
  --out _tmp/20261014-exp010-run/coverage-recheck.json
```

预期：run.py exit 0，`result.json` status=PASS，direct/proxy 各六个场景 PASS；新枚举目前预计 263，`route-reconciliation.json` 差集与 B1 标签差异一致；declarations/evidence coverage 都 PASS；两侧 recording/client/activity-direct/cases.jsonl 存在；observer origin 直指 13102；proxy 处理侧为 ts、direct 通常 null；audit 无存活自有进程组/清理错误且每个自有 listener 都在 127.0.0.1。**任何缺证据/PTY 失败/新路由无豁免都必须非零退出**。预期不是本次已获得的结果。

仅手动挂 recorder（上游已经由协调者安全启动时）：

```bash
cargo build --locked --offline --manifest-path scripts/exp008/proxy/Cargo.toml
EXP008_LISTEN=127.0.0.1:13101 \
EXP008_UPSTREAM=http://127.0.0.1:13103 \
EXP010_RECORD_LOG="$PWD/_tmp/manual-exp010-recording.jsonl" \
"$CARGO_TARGET_DIR/debug/pdrs-exp008-proxy"
```

S3/S4 接续时保留这些原始字段；在新 PR 决定比较/归一化、SSE 阈值、处理侧门及规约，切勿修改已冻结的 `specs/parity/`、`specs/gateway-parity/`。

## 协调者实跑（2026-10-14，沙箱外）
- 第 1 次 `_tmp/20261014-exp010-run`：auth / chat / readonly / watch / config 两侧 PASS；upload 两侧同样 FAIL（`PUBLIC_SELECTED_SCOPE_REQUIRED`）→ 夹具缺 scope，第 2 轮按 `ui/server/routes/modules.js:334-353` 补齐。
- 第 2 次 `_tmp/20261014-exp010-run2`：**六场景直连 / 反代全部 PASS**；当时 `result.json` status=PASS；`check_coverage.py`（本轮枚举 + 用例证据）PASS。审计：监听全在 127.0.0.1（13101–13104、28990），自有进程组全部退出，cleanup_errors=[]。第 2 轮评审核实两侧各有 30 条 `http_aborted`、27 条 `http_end`，正常定长 body 读满后被误记为中断；这些旧录制在第 3 轮的新完整性门下拒收，不能作为完整 body 证据。修复后的实录需新建 `_tmp/20261014-exp010-run3` 由协调者重跑，见 `_tmp/RESULT.md`「第 3 轮」。
- 边界：本 PR 只有录制与覆盖门，**没有行为比较**（S3）；覆盖矩阵 244 条为 AI 代批豁免——当前没有路由由 Rust 接管，豁免只是占位，B3 起每接管一条路由须先补直接用例。
- 第 3 次 `_tmp/20261014-exp010-run3`（PR #57 第 2 轮评审修改后）：六场景两侧 PASS；每侧 `http_end` 57、`http_aborted` **0**（修前误记 30 条）；覆盖门 PASS。
