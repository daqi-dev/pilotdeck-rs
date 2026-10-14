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

## B2 第 3 个 PR：S3/S4 草稿（2026-10-14）

新增 `compare.py`、按完整字段路径登记的 `policy.py`、停服务后的 `snapshot.py`、`controls/`、`check_ruler.py` 与 `check_frozen.py`，以及 `specs/ui-server-parity/{acceptance,normalization,accepted-diffs}.md`。规约为 **DRAFT**，独立评审为 null；不修改 C/A 的冻结尺子。此前 S1/S2 的声明/录制 PASS 与本次离线 controls PASS 都不能当 B2 已冻结或 Rust 路由等价。

S3 消费原始 recording，不回用 exp008 的 snapshot offset。HTTP 同名多值头按列表比较；处理侧先验证 recorder 原始头，再剥离。WS 在客户端边界按每连接全序比较后再比完整 JSON message 的 type/确定字段，seq 断言保留。SSE 按事件结束空行的字节到齐计算到达，完整事件/id/retry/data 保留，chunk 边界只诊断。带外每会话 revision 的原值与内容必须一致；watch snapshot 对稳定的 after-scenarios 带外列表，不按用户最大 revision 估全局基准。目录实际内容重新采集核对保存快照；SQLite schema/逻辑 rows、密码/JWT/VAPID 验证也入门。

S1 写锁前取 arrival_ns，故不同 task 的时钟采样可能相对 seq 轻微倒退；S3 以 seq 判全序，仅 HTTP 同一 body 方向要求 arrival_ns 单调，不按全局时钟重新排序 WS。

真实 auth/落盘验证使用 `run-meta.baseline_path` 下**已安装**的 TS bcrypt 与 Node 内置 crypto，不安装依赖、不下载二进制。新 run.py 保持原双侧入口，另增加 `--side direct|proxy --slot direct` 单侧入口与 run-meta/snapshot。为满足绝对路径一致，定标与等价比较应全部复用同一 --out，跑完只改名保存；下列脚本由协调者在沙箱外执行。

```bash
# 从本 worktree 根执行；当前源与规约在所有轮次之间保持不变。
export CARGO_HOME="$PWD/_build/cargo-home"
export CARGO_TARGET_DIR="$PWD/_build/exp010-target"
task_prefix="$PWD/_tmp/20261014-b2-live"
mkdir /tmp/pdrs-parity.lock
trap 'rmdir /tmp/pdrs-parity.lock' EXIT
for task_index in 1 2 3 4 5; do
  test ! -e "$task_prefix"
  test ! -e "${task_prefix}-ts${task_index}"
  python3 scripts/ui-server-parity/run.py --out "$task_prefix" --side direct --slot direct
  mv "$task_prefix" "${task_prefix}-ts${task_index}"
done
test ! -e "$task_prefix"
test ! -e "${task_prefix}-proxy"
python3 scripts/ui-server-parity/run.py --out "$task_prefix" --side proxy --slot direct
mv "$task_prefix" "${task_prefix}-proxy"
python3 scripts/ui-server-parity/check_ruler.py \
  --round "${task_prefix}-ts1/direct" --round "${task_prefix}-ts2/direct" \
  --round "${task_prefix}-ts3/direct" --round "${task_prefix}-ts4/direct" \
  --round "${task_prefix}-ts5/direct" \
  --enumeration "${task_prefix}-ts1/enumeration/router-enumeration.json" \
  --proxy "${task_prefix}-proxy/direct" --out "${task_prefix}-ruler.json"
# 此处退出 2 是 PENDING_REVIEW，独立评审仍 null；退出 1 是 FAIL。
python3 scripts/ui-server-parity/compare.py \
  "${task_prefix}-ts1/direct" "${task_prefix}-proxy/direct" \
  --enumeration "${task_prefix}-ts1/enumeration/router-enumeration.json" \
  --out "${task_prefix}-compare-proxy.json"
```

所有 `run-meta.fixture_path` 都必须等于 `$task_prefix/direct`。归档目录名不同不影响已录制的原始路径；不要在新 ts1/ts2 输出路径分别采集，也不要把两侧参数改成不同 slot。运行使用既有回环端口 13101/13102/13103/13104/28990，只停自有进程组，不碰 3001/18789 或真实 home。

离线验证：`python3 -m py_compile scripts/ui-server-parity/*.py scripts/ui-server-parity/controls/*.py`；`python3 scripts/ui-server-parity/controls/run_all.py`；`python3 scripts/ui-server-parity/test_compare.py`；S1/S2 的 test_recording.py/test_coverage.py。`check_ruler.py` 不给 --round 时只运行离线对照，self_compare=null/PENDING_CALIBRATION/退出 2；`check_frozen.py` 缺人批 manifest 返回 DRAFT/退出 2。本次还用主仓只读 run3 direct 归档做了格式兼容自比（适配的 metadata 明确 synthetic），PASS；这不是五次独立 TS 采集。真实 k=5、direct/DUT、两家评审、人批冻结与前端回归门仍待协调者执行。

## B2 第 3 个 PR · 第 2 轮：真实 TS k=5 标定修正（2026-10-14）

**结果：旧五轮真实数据重放仍为 0/10 PASS，self_compare=false；如实保留 FAIL。** 总 differences 从 3913 降为 2389；这包含全序、按 type 和 identity 的重复诊断，不代表 2389 个独立缺陷。per_rule_negative=true、same_bad_rejected=true、independent_review=null。没有修改/补写原始录制或 run-meta，没有提交，没有改 `state/`、C/A 冻结规约与尺子，也没有启动服务。

本轮新增九条精确 ID 路径（HTTP 历史/stream 各三条，registry event 三条），详见 `specs/ui-server-parity/normalization.md`「第 2 轮」，每条有 TS 随机生成/传递处路径和行号。原分类映射升级为跨 HTTP/WS/落盘共用 UUID 单射；块后缀与消息种类前缀保留。以两个固定输入 session 及各自 chat 原事件顺序预分配记号，修复“按 HTTP 返回列表顺序给会话命名”导致的编号漂移，同时继续让真正的列表翻转 FAIL。十对中的 `readonly.messages` 与 `disk.snapshot` 差异现均为 0。

| 差异族 | 字段路径 × 规则的典型簇 | 五遍分析与处置 |
|---|---|---|
| 未归一随机块/turn 引用 | `http[readonly.messages#0].body.{messages,stream.messages}[*].{blockId,timeline.id,timeline.turnId}` × `http.body`；`disk[@registry].data.events[*].entries[*].event.{blockId,timeline.id,timeline.turnId}` × `disk.snapshot` | 补全九条路径；只重命名 UUID 根；跨渠道引用与后缀保留。新报告这些簇均为 0 |
| 首见编号漂移及真正列表翻转 | `http[readonly.messages#0].request.path`、各 sessionId/runId × `identity.consistency`；`http[readonly.{projects,sessions}#0]...sessions[*].id` × `http.body` | 预分配只影响实体命名，不排序比较内容；TS1 的 HTTP 列表 `21` 与其余 `12` 仍报 4 个 http.body 差异/对 |
| 并发广播/关闭前文件监听 | `ws."ws.chat.{2,3}#0"[*].body.*` × `ws.sequence` 与 `ws.by_type...session-activity` × `ws.structure` | 同一连接内已有交错，不是简单去掉“跨连接总序”能解决。驱动现在逐会话发送，完成后等待两连接同 run idle + 之后的 project watcher。旧录制保留原顺序与全部事件，仍 FAIL |
| 终端 onData 分帧与 URL 顺序 | `ws."ws.watch.{6,7}#0"[*].body.data`、`ws.by_type...output` / `.auth_url[*].body.url` × WS 两规则 | 五遍拼接输出字节一致但分帧不同；TS4 的 auth_url 顺序还真实翻转。登记为阶段 C 多态候选，未批准也未执行豁免 |

`specs/ui-server-parity/accepted-diffs.md` 登记五遍 activity 顺序、连接长度、watcher 消息数、PTY output 数/拼接 SHA256 与 URL 顺序。逐原始 seq 的证据保存在 `_tmp/b2-round2-variants.json`。PTY fixture 另改为固定 prompt、禁 rc、抑制 macOS bash 启动公告，并用 prompt 屏障替换 sleep；这可能消除 URL 噪声，但内核 onData 分帧仍可能不同。本轮只完成无服务驱动测试，**没有声称新驱动已被 TS 实跑验证**；project watcher 的时间字段、重复通知等也需新数据再定标，当前没有直接归一或忽略它们。

离线对照 **181/181 PASS**：原 28 条端到端 + 153 条完整路径级对照（113 个 ID 路径；每条重命名 PASS、实体合并 FAIL，20 个 block/timeline 路径另测种类和序号变化 FAIL）。Python S1/S2/S3 合计 **43/43 PASS**；新增 WS/落盘端到端引用断裂对照、跨通道一致性、数组保序和默认/重放哈希门回归。Node 驱动对照验证第二次发送必须等待双连接 idle 与之后的 watcher、早期 watcher 不算完成屏障、第一轮错误不继续。编译、Node 语法与 diff 检查通过；Python 报告还包含旧 SQLite 测试未显式 close 的 ResourceWarning，不影响测试结果。

原数据每份各自与自身比较都为 PASS（`_tmp/b2-round2-same-archive.json`），这是格式/完整性检查，**不是独立 k=5 标定通过**。旧 proxy 也用新比较器单独重放：FAIL、253 个诊断；其中另有七个 101 响应 Date 头只在 proxy 出现。未用这些 DUT 差异添加任何规则。

原录制绑定上一轮装置哈希，默认 compare/check_ruler 继续拒收过期哈希。为执行用户要求的旧五轮重算，增加显式 `--replay-of`，校验原报告绑定的哈希/run ID/PID、保存新旧哈希与原报告摘要，并标记 `historical-replay / calibration_eligible=false`。旧数据不能证明修改后的串行驱动已标定；即使重放全过也只能 REPLAY_ONLY。本轮真实执行的重算命令（退出 1 = FAIL）：

```bash
python3 scripts/ui-server-parity/check_ruler.py \
  --round _tmp/20261014-b2-live-ts1/direct \
  --round _tmp/20261014-b2-live-ts2/direct \
  --round _tmp/20261014-b2-live-ts3/direct \
  --round _tmp/20261014-b2-live-ts4/direct \
  --round _tmp/20261014-b2-live-ts5/direct \
  --enumeration _tmp/20261014-b2-live-ts1/enumeration/router-enumeration.json \
  --replay-of _tmp/20261014-b2-live-ruler.json \
  --proxy _tmp/20261014-b2-live-proxy/direct \
  --out _tmp/20261014-b2-round2-ruler.json
python3 scripts/ui-server-parity/summarize_ruler.py \
  --before _tmp/20261014-b2-live-ruler.json \
  --after _tmp/20261014-b2-round2-ruler.json \
  --out _tmp/b2-round2-clusters.json
```

以下新驱动采集命令**本轮未执行**，留协调者在沙箱外运行；使用新 prefix，旧五轮及原 ruler 保留。全量采集之间保持源文件不变，固定相同 --out 与 --slot，归档只在停服务后改名。不承诺这次修改即可全绿：若 PTY 全帧门、watcher 仍不稳定，必须保留结果并独立评审多态规则。

```bash
# 从本 worktree 根运行；与上一轮相同隔离环境和固定回环端口。
export CARGO_HOME="$PWD/_build/cargo-home"
export CARGO_TARGET_DIR="$PWD/_build/exp010-target"
task_prefix="$PWD/_tmp/20261014-b2-serial"
mkdir /tmp/pdrs-parity.lock || exit 1
trap 'rmdir /tmp/pdrs-parity.lock' EXIT
for task_index in 1 2 3 4 5; do
  test ! -e "$task_prefix" && test ! -e "${task_prefix}-ts${task_index}" || exit 1
  python3 scripts/ui-server-parity/run.py --out "$task_prefix" --side direct --slot direct || exit 1
  mv "$task_prefix" "${task_prefix}-ts${task_index}" || exit 1
done
test ! -e "$task_prefix" && test ! -e "${task_prefix}-proxy" || exit 1
python3 scripts/ui-server-parity/run.py --out "$task_prefix" --side proxy --slot direct || exit 1
mv "$task_prefix" "${task_prefix}-proxy" || exit 1
# 不传 --replay-of：新真实录制必须与当前装置哈希完全相等。
python3 scripts/ui-server-parity/check_ruler.py \
  --round "${task_prefix}-ts1/direct" --round "${task_prefix}-ts2/direct" \
  --round "${task_prefix}-ts3/direct" --round "${task_prefix}-ts4/direct" \
  --round "${task_prefix}-ts5/direct" \
  --enumeration "${task_prefix}-ts1/enumeration/router-enumeration.json" \
  --proxy "${task_prefix}-proxy/direct" --out "${task_prefix}-ruler.json"
```

下面表格由 `summarize_ruler.py` 逐对处理全部原始 differences 生成，包含旧/新并集 266 个完整字段路径 × 规则簇。JSON 存每簇每对计数和两版具体值例子，表格保留原本重复诊断。

<!-- BEGIN B2 ROUND2 CLUSTERS -->
### 十对逐对结果

| TS 对 | 原 differences | 第 2 轮 differences | 第 2 轮结果 |
|---|---:|---:|---|
| 1-2 | 605 | 339 | FAIL |
| 1-3 | 554 | 214 | FAIL |
| 1-4 | 554 | 294 | FAIL |
| 1-5 | 446 | 186 | FAIL |
| 2-3 | 232 | 211 | FAIL |
| 2-4 | 338 | 236 | FAIL |
| 2-5 | 282 | 180 | FAIL |
| 3-4 | 361 | 285 | FAIL |
| 3-5 | 363 | 287 | FAIL |
| 4-5 | 178 | 157 | FAIL |

### 字段路径 × 比较规则：完整聚类表

逐对遍历原报告和重放报告的全部 `differences`，仅将数组数字下标换为 `[*]`；字段缺失、长度和错位产生的重复诊断也计入，不把条数当成独立缺陷数。WS 全序与 by_type 是两道独立门，因此表中保留两套位置。磁盘槽位解析为以下文件别名：

- `@chat:1` = `home/projects/exp008-fixture/chats/web:33333333-3333-4333-8333-000000000001.jsonl`
- `@chat:2` = `home/projects/exp008-fixture/chats/web:33333333-3333-4333-8333-000000000002.jsonl`
- `@projection:1` = `home/projects/exp008-fixture/chats/web:33333333-3333-4333-8333-000000000001.jsonl.projections.json`
- `@projection:2` = `home/projects/exp008-fixture/chats/web:33333333-3333-4333-8333-000000000002.jsonl.projections.json`
- `@registry` = `home/runs/registry.json`

表中“原→新”是十对累计数；“涉及 TS 对”取前后并集。每簇逐对计数与原始完整 location/值例子另存同名 JSON，便于复核。没有过滤掉任何一对或规则。

| 规则 | 完整字段路径（数组下标归一） | 原→新 | 涉及 TS 对 |
|---|---|---:|---|
| `disk.snapshot` | `disk[@chat:1].data[*].result.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `disk.snapshot` | `disk[@chat:1].data[*].sessionId` | 76→0 | 1-2, 1-3, 1-4, 1-5 |
| `disk.snapshot` | `disk[@chat:2].data[*].detail.runId` | 6→0 | 1-2, 1-3, 2-4, 2-5, 3-4, 3-5 |
| `disk.snapshot` | `disk[@chat:2].data[*].result.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `disk.snapshot` | `disk[@chat:2].data[*].sessionId` | 76→0 | 1-2, 1-3, 1-4, 1-5 |
| `disk.snapshot` | `disk[@projection:1].data.projections."agent-transcript.conversation".state.entries[*].entry.result.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `disk.snapshot` | `disk[@projection:1].data.projections."agent-transcript.conversation".state.entries[*].entry.sessionId` | 12→0 | 1-2, 1-3, 1-4, 1-5 |
| `disk.snapshot` | `disk[@projection:1].data.projections."web-history.agent-statuses".state[*].sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `disk.snapshot` | `disk[@projection:1].data.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `disk.snapshot` | `disk[@projection:2].data.projections."agent-transcript.conversation".state.entries[*].entry.result.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `disk.snapshot` | `disk[@projection:2].data.projections."agent-transcript.conversation".state.entries[*].entry.sessionId` | 12→0 | 1-2, 1-3, 1-4, 1-5 |
| `disk.snapshot` | `disk[@projection:2].data.projections."web-history.agent-statuses".state[*].detail.runId` | 6→0 | 1-2, 1-3, 2-4, 2-5, 3-4, 3-5 |
| `disk.snapshot` | `disk[@projection:2].data.projections."web-history.agent-statuses".state[*].sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `disk.snapshot` | `disk[@projection:2].data.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `disk.snapshot` | `disk[@registry].data.events[*].entries[*].event.blockId` | 40→0 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5, 4-5 |
| `disk.snapshot` | `disk[@registry].data.events[*].entries[*].event.runId` | 96→0 | 1-2, 1-3, 1-4, 1-5, 2-4, 2-5, 3-4, 3-5 |
| `disk.snapshot` | `disk[@registry].data.events[*].entries[*].event.timeline.id` | 40→0 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5, 4-5 |
| `disk.snapshot` | `disk[@registry].data.events[*].entries[*].event.timeline.turnId` | 40→0 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5, 4-5 |
| `disk.snapshot` | `disk[@registry].data.events[*].key` | 12→0 | 1-2, 1-3, 1-4, 1-5, 2-4, 2-5, 3-4, 3-5 |
| `disk.snapshot` | `disk[@registry].data.records[*].result.runId` | 6→0 | 1-2, 1-3, 2-4, 2-5, 3-4, 3-5 |
| `disk.snapshot` | `disk[@registry].data.records[*].runId` | 6→0 | 1-2, 1-3, 2-4, 2-5, 3-4, 3-5 |
| `disk.snapshot` | `disk[@registry].data.records[*].sessionKey` | 8→0 | 1-2, 1-3, 1-4, 1-5 |
| `http.body` | `http[readonly.messages#0].body.messages[*].blockId` | 10→0 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5, 4-5 |
| `http.body` | `http[readonly.messages#0].body.messages[*].sessionId` | 12→0 | 1-2, 1-3, 1-4, 1-5 |
| `http.body` | `http[readonly.messages#0].body.messages[*].timeline.id` | 10→0 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5, 4-5 |
| `http.body` | `http[readonly.messages#0].body.messages[*].timeline.turnId` | 10→0 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5, 4-5 |
| `http.body` | `http[readonly.messages#0].body.stream.messages[*].blockId` | 20→0 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5, 4-5 |
| `http.body` | `http[readonly.messages#0].body.stream.messages[*].sessionId` | 28→0 | 1-2, 1-3, 1-4, 1-5 |
| `http.body` | `http[readonly.messages#0].body.stream.messages[*].timeline.id` | 20→0 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5, 4-5 |
| `http.body` | `http[readonly.messages#0].body.stream.messages[*].timeline.turnId` | 20→0 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5, 4-5 |
| `http.body` | `http[readonly.projects#0].body[*].sessions[*].id` | 0→8 | 1-2, 1-3, 1-4, 1-5 |
| `http.body` | `http[readonly.sessions#0].body.sessions[*].id` | 0→8 | 1-2, 1-3, 1-4, 1-5 |
| `http.request` | `http[readonly.messages#0].request.path` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `disk[@chat:1].data[*].result.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `disk[@chat:1].data[*].sessionId` | 76→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `disk[@chat:2].data[*].detail.runId` | 6→0 | 1-2, 1-3, 2-4, 2-5, 3-4, 3-5 |
| `identity.consistency` | `disk[@chat:2].data[*].result.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `disk[@chat:2].data[*].sessionId` | 76→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `disk[@projection:1].data.projections."agent-transcript.conversation".state.entries[*].entry.result.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `disk[@projection:1].data.projections."agent-transcript.conversation".state.entries[*].entry.sessionId` | 12→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `disk[@projection:1].data.projections."web-history.agent-statuses".state[*].sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `disk[@projection:1].data.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `disk[@projection:2].data.projections."agent-transcript.conversation".state.entries[*].entry.result.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `disk[@projection:2].data.projections."agent-transcript.conversation".state.entries[*].entry.sessionId` | 12→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `disk[@projection:2].data.projections."web-history.agent-statuses".state[*].detail.runId` | 6→0 | 1-2, 1-3, 2-4, 2-5, 3-4, 3-5 |
| `identity.consistency` | `disk[@projection:2].data.projections."web-history.agent-statuses".state[*].sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `disk[@projection:2].data.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `disk[@registry].data.events[*].entries[*].event.runId` | 96→0 | 1-2, 1-3, 1-4, 1-5, 2-4, 2-5, 3-4, 3-5 |
| `identity.consistency` | `disk[@registry].data.records[*].result.runId` | 6→0 | 1-2, 1-3, 2-4, 2-5, 3-4, 3-5 |
| `identity.consistency` | `disk[@registry].data.records[*].runId` | 6→0 | 1-2, 1-3, 2-4, 2-5, 3-4, 3-5 |
| `identity.consistency` | `disk[@registry].data.records[*].sessionKey` | 8→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `http[readonly.messages#0].body.messages[*].sessionId` | 12→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `http[readonly.messages#0].body.stream.messages[*].sessionId` | 28→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `http[readonly.projects#0].body[*].sessions[*].id` | 0→8 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `http[readonly.sessions#0].body.sessions[*].id` | 0→8 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `ws."ws.chat.2#0"[*].body.activity.completedRunId` | 9→6 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `identity.consistency` | `ws."ws.chat.2#0"[*].body.activity.runId` | 15→12 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `identity.consistency` | `ws."ws.chat.2#0"[*].body.activity.sessionId` | 13→12 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `identity.consistency` | `ws."ws.chat.2#0"[*].body.options.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `ws."ws.chat.2#0"[*].body.sessionId` | 27→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `ws."ws.chat.3#0"[*].body.activity.completedRunId` | 9→6 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `identity.consistency` | `ws."ws.chat.3#0"[*].body.activity.runId` | 15→12 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `identity.consistency` | `ws."ws.chat.3#0"[*].body.activity.sessionId` | 13→12 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `identity.consistency` | `ws."ws.chat.3#0"[*].body.options.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `ws."ws.chat.3#0"[*].body.runId` | 39→0 | 1-2, 1-3, 2-4, 2-5, 3-4, 3-5 |
| `identity.consistency` | `ws."ws.chat.3#0"[*].body.sessionId` | 27→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `ws."ws.watch.4#0"[*].body.sessionId` | 8→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `ws."ws.watch.5#0"[*].body.activities[*].completedRunId` | 15→12 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `identity.consistency` | `ws."ws.watch.5#0"[*].body.activities[*].runId` | 15→12 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `identity.consistency` | `ws."ws.watch.5#0"[*].body.activities[*].sessionId` | 8→12 | 1-2, 1-3, 1-4, 1-5, 2-3, 3-4, 3-5 |
| `identity.consistency` | `ws."ws.watch.5#0"[*].body.sessionId` | 12→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `ws.by_type."ws.chat.2#0".complete[*].body.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `ws.by_type."ws.chat.2#0".model-selection-changed[*].body.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `ws.by_type."ws.chat.2#0".pilotdeck-command[*].body.options.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `ws.by_type."ws.chat.2#0".session-activity[*].body.activity.completedRunId` | 15→12 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `identity.consistency` | `ws.by_type."ws.chat.2#0".session-activity[*].body.activity.runId` | 27→24 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `identity.consistency` | `ws.by_type."ws.chat.2#0".session-activity[*].body.activity.sessionId` | 20→24 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `identity.consistency` | `ws.by_type."ws.chat.2#0".session-input-accepted[*].body.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `ws.by_type."ws.chat.2#0".status[*].body.id` | 6→0 | 1-2, 1-3, 2-4, 2-5, 3-4, 3-5 |
| `identity.consistency` | `ws.by_type."ws.chat.2#0".status[*].body.sessionId` | 12→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `ws.by_type."ws.chat.2#0".stream_delta[*].body.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `ws.by_type."ws.chat.2#0".text[*].body.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `ws.by_type."ws.chat.3#0".complete[*].body.runId` | 6→0 | 1-2, 1-3, 2-4, 2-5, 3-4, 3-5 |
| `identity.consistency` | `ws.by_type."ws.chat.3#0".complete[*].body.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `ws.by_type."ws.chat.3#0".model-selection-changed[*].body.runId` | 6→0 | 1-2, 1-3, 2-4, 2-5, 3-4, 3-5 |
| `identity.consistency` | `ws.by_type."ws.chat.3#0".model-selection-changed[*].body.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `ws.by_type."ws.chat.3#0".pilotdeck-command[*].body.options.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `ws.by_type."ws.chat.3#0".session-activity[*].body.activity.completedRunId` | 15→12 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `identity.consistency` | `ws.by_type."ws.chat.3#0".session-activity[*].body.activity.runId` | 27→24 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `identity.consistency` | `ws.by_type."ws.chat.3#0".session-activity[*].body.activity.sessionId` | 20→24 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `identity.consistency` | `ws.by_type."ws.chat.3#0".session-input-accepted[*].body.runId` | 6→0 | 1-2, 1-3, 2-4, 2-5, 3-4, 3-5 |
| `identity.consistency` | `ws.by_type."ws.chat.3#0".session-input-accepted[*].body.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `ws.by_type."ws.chat.3#0".status[*].body.runId` | 18→0 | 1-2, 1-3, 2-4, 2-5, 3-4, 3-5 |
| `identity.consistency` | `ws.by_type."ws.chat.3#0".status[*].body.sessionId` | 12→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `ws.by_type."ws.chat.3#0".stream_delta[*].body.runId` | 6→0 | 1-2, 1-3, 2-4, 2-5, 3-4, 3-5 |
| `identity.consistency` | `ws.by_type."ws.chat.3#0".stream_delta[*].body.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `ws.by_type."ws.chat.3#0".text[*].body.runId` | 6→0 | 1-2, 1-3, 2-4, 2-5, 3-4, 3-5 |
| `identity.consistency` | `ws.by_type."ws.chat.3#0".text[*].body.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `ws.by_type."ws.watch.4#0".input-queue-state[*].body.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `ws.by_type."ws.watch.4#0".watch-session[*].body.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `ws.by_type."ws.watch.5#0".input-queue-state[*].body.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `ws.by_type."ws.watch.5#0".session-activity-snapshot[*].body.activities[*].completedRunId` | 15→12 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `identity.consistency` | `ws.by_type."ws.watch.5#0".session-activity-snapshot[*].body.activities[*].runId` | 15→12 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `identity.consistency` | `ws.by_type."ws.watch.5#0".session-activity-snapshot[*].body.activities[*].sessionId` | 8→12 | 1-2, 1-3, 1-4, 1-5, 2-3, 3-4, 3-5 |
| `identity.consistency` | `ws.by_type."ws.watch.5#0".unwatch-session[*].body.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `identity.consistency` | `ws.by_type."ws.watch.5#0".watch-session[*].body.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `ws.sequence` | `ws."ws.chat.2#0"` | 6→6 | 1-2, 1-3, 1-5, 2-4, 3-4, 4-5 |
| `ws.sequence` | `ws."ws.chat.2#0"[*].body.activity` | 24→24 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.sequence` | `ws."ws.chat.2#0"[*].body.activity.completedRunId` | 9→6 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.sequence` | `ws."ws.chat.2#0"[*].body.activity.runId` | 15→12 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.sequence` | `ws."ws.chat.2#0"[*].body.activity.sessionId` | 13→12 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.sequence` | `ws."ws.chat.2#0"[*].body.changedFile` | 1→1 | 1-4 |
| `ws.sequence` | `ws."ws.chat.2#0"[*].body.current` | 6→6 | 1-2, 1-3, 1-5, 2-4, 3-4, 4-5 |
| `ws.sequence` | `ws."ws.chat.2#0"[*].body.exitCode` | 12→12 | 1-2, 1-4, 1-5, 2-3, 3-4, 3-5 |
| `ws.sequence` | `ws."ws.chat.2#0"[*].body.finishReason` | 12→12 | 1-2, 1-4, 1-5, 2-3, 3-4, 3-5 |
| `ws.sequence` | `ws."ws.chat.2#0"[*].body.id` | 24→24 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.sequence` | `ws."ws.chat.2#0"[*].body.kind` | 24→24 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.sequence` | `ws."ws.chat.2#0"[*].body.options.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `ws.sequence` | `ws."ws.chat.2#0"[*].body.phase` | 12→12 | 1-2, 1-3, 1-5, 2-4, 3-4, 4-5 |
| `ws.sequence` | `ws."ws.chat.2#0"[*].body.processed` | 12→12 | 1-2, 1-3, 1-5, 2-4, 3-4, 4-5 |
| `ws.sequence` | `ws."ws.chat.2#0"[*].body.projectListRevision` | 1→1 | 1-4 |
| `ws.sequence` | `ws."ws.chat.2#0"[*].body.projects[*].lastActivity` | 1→1 | 1-4 |
| `ws.sequence` | `ws."ws.chat.2#0"[*].body.projects[*].sessions[*].createdAt` | 2→2 | 1-4 |
| `ws.sequence` | `ws."ws.chat.2#0"[*].body.projects[*].sessions[*].created_at` | 2→2 | 1-4 |
| `ws.sequence` | `ws."ws.chat.2#0"[*].body.projects[*].sessions[*].id` | 2→2 | 1-4 |
| `ws.sequence` | `ws."ws.chat.2#0"[*].body.projects[*].sessions[*].lastActivity` | 2→2 | 1-4 |
| `ws.sequence` | `ws."ws.chat.2#0"[*].body.projects[*].sessions[*].updated_at` | 2→2 | 1-4 |
| `ws.sequence` | `ws."ws.chat.2#0"[*].body.provider` | 24→24 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.sequence` | `ws."ws.chat.2#0"[*].body.raw_b64` | 12→12 | 1-2, 1-3, 1-5, 2-4, 3-4, 4-5 |
| `ws.sequence` | `ws."ws.chat.2#0"[*].body.runId` | 24→24 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.sequence` | `ws."ws.chat.2#0"[*].body.sessionId` | 51→24 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.sequence` | `ws."ws.chat.2#0"[*].body.success` | 12→12 | 1-2, 1-4, 1-5, 2-3, 3-4, 3-5 |
| `ws.sequence` | `ws."ws.chat.2#0"[*].body.text` | 12→12 | 1-2, 1-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.sequence` | `ws."ws.chat.2#0"[*].body.timestamp` | 25→25 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.sequence` | `ws."ws.chat.2#0"[*].body.total` | 12→12 | 1-2, 1-3, 1-5, 2-4, 3-4, 4-5 |
| `ws.sequence` | `ws."ws.chat.2#0"[*].body.type` | 36→36 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5, 4-5 |
| `ws.sequence` | `ws."ws.chat.2#0"[*].body.usage` | 12→12 | 1-2, 1-4, 1-5, 2-3, 3-4, 3-5 |
| `ws.sequence` | `ws."ws.chat.2#0"[*].dir` | 6→6 | 1-2, 1-3, 1-5, 2-4, 3-4, 4-5 |
| `ws.sequence` | `ws."ws.chat.2#0"[*].masked` | 6→6 | 1-2, 1-3, 1-5, 2-4, 3-4, 4-5 |
| `ws.sequence` | `ws."ws.chat.2#0"[*].opcode` | 12→12 | 1-2, 1-3, 1-5, 2-4, 3-4, 4-5 |
| `ws.sequence` | `ws."ws.chat.2#0"[*].type` | 36→36 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5, 4-5 |
| `ws.sequence` | `ws."ws.chat.3#0"` | 8→8 | 1-2, 1-3, 1-5, 2-3, 2-4, 2-5, 3-4, 4-5 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].body.activity` | 24→24 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].body.activity.completedRunId` | 9→6 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].body.activity.runId` | 15→12 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].body.activity.sessionId` | 13→12 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].body.changeType` | 2→2 | 1-2, 2-4 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].body.changedFile` | 3→3 | 1-2, 1-4, 2-4 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].body.current` | 10→10 | 1-2, 1-3, 1-5, 2-3, 2-4, 2-5, 3-4, 4-5 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].body.exitCode` | 12→12 | 1-2, 1-4, 1-5, 2-3, 3-4, 3-5 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].body.finishReason` | 12→12 | 1-2, 1-4, 1-5, 2-3, 3-4, 3-5 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].body.id` | 24→24 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].body.kind` | 24→24 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].body.options.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].body.phase` | 14→14 | 1-2, 1-3, 1-5, 2-3, 2-4, 2-5, 3-4, 4-5 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].body.processed` | 14→14 | 1-2, 1-3, 1-5, 2-3, 2-4, 2-5, 3-4, 4-5 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].body.projectListRevision` | 3→3 | 1-2, 1-4, 2-4 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].body.projects` | 2→2 | 1-2, 2-4 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].body.projects[*].lastActivity` | 1→1 | 1-4 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].body.projects[*].sessions[*].createdAt` | 2→2 | 1-4 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].body.projects[*].sessions[*].created_at` | 2→2 | 1-4 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].body.projects[*].sessions[*].id` | 2→2 | 1-4 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].body.projects[*].sessions[*].lastActivity` | 2→2 | 1-4 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].body.projects[*].sessions[*].updated_at` | 2→2 | 1-4 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].body.provider` | 24→24 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].body.raw_b64` | 14→14 | 1-2, 1-3, 1-5, 2-3, 2-4, 2-5, 3-4, 4-5 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].body.runId` | 63→24 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].body.sessionId` | 51→24 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].body.success` | 12→12 | 1-2, 1-4, 1-5, 2-3, 3-4, 3-5 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].body.text` | 12→12 | 1-2, 1-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].body.timestamp` | 27→27 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].body.total` | 12→12 | 1-2, 1-3, 1-5, 2-3, 2-4, 2-5, 3-4, 4-5 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].body.type` | 38→38 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5, 4-5 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].body.usage` | 12→12 | 1-2, 1-4, 1-5, 2-3, 3-4, 3-5 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].body.watchProvider` | 2→2 | 1-2, 2-4 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].dir` | 6→6 | 1-2, 1-3, 1-5, 2-4, 3-4, 4-5 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].masked` | 6→6 | 1-2, 1-3, 1-5, 2-4, 3-4, 4-5 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].opcode` | 14→14 | 1-2, 1-3, 1-5, 2-3, 2-4, 2-5, 3-4, 4-5 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].type` | 38→38 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5, 4-5 |
| `ws.sequence` | `ws."ws.watch.4#0"[*].body.sessionId` | 8→0 | 1-2, 1-3, 1-4, 1-5 |
| `ws.sequence` | `ws."ws.watch.5#0"[*].body.activities[*].completedRunId` | 15→12 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.sequence` | `ws."ws.watch.5#0"[*].body.activities[*].runId` | 15→12 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.sequence` | `ws."ws.watch.5#0"[*].body.activities[*].sessionId` | 8→12 | 1-2, 1-3, 1-4, 1-5, 2-3, 3-4, 3-5 |
| `ws.sequence` | `ws."ws.watch.5#0"[*].body.sessionId` | 12→0 | 1-2, 1-3, 1-4, 1-5 |
| `ws.sequence` | `ws."ws.watch.6#0"` | 8→8 | 1-2, 1-3, 1-4, 2-3, 2-4, 2-5, 3-5, 4-5 |
| `ws.sequence` | `ws."ws.watch.6#0"[*].body.autoOpen` | 24→24 | 1-2, 1-3, 1-4, 2-4, 2-5, 3-4, 3-5, 4-5 |
| `ws.sequence` | `ws."ws.watch.6#0"[*].body.data` | 90→90 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5, 4-5 |
| `ws.sequence` | `ws."ws.watch.6#0"[*].body.raw_b64` | 10→10 | 1-2, 1-3, 1-4, 2-3, 2-4, 2-5, 3-5, 4-5 |
| `ws.sequence` | `ws."ws.watch.6#0"[*].body.type` | 46→46 | 1-2, 1-3, 1-4, 2-3, 2-4, 2-5, 3-4, 3-5, 4-5 |
| `ws.sequence` | `ws."ws.watch.6#0"[*].body.url` | 24→24 | 1-2, 1-3, 1-4, 2-4, 2-5, 3-4, 3-5, 4-5 |
| `ws.sequence` | `ws."ws.watch.6#0"[*].dir` | 30→30 | 1-2, 1-3, 1-4, 2-3, 2-4, 2-5, 3-4, 3-5, 4-5 |
| `ws.sequence` | `ws."ws.watch.6#0"[*].masked` | 30→30 | 1-2, 1-3, 1-4, 2-3, 2-4, 2-5, 3-4, 3-5, 4-5 |
| `ws.sequence` | `ws."ws.watch.6#0"[*].opcode` | 10→10 | 1-2, 1-3, 1-4, 2-3, 2-4, 2-5, 3-5, 4-5 |
| `ws.sequence` | `ws."ws.watch.6#0"[*].type` | 46→46 | 1-2, 1-3, 1-4, 2-3, 2-4, 2-5, 3-4, 3-5, 4-5 |
| `ws.sequence` | `ws."ws.watch.7#0"` | 8→8 | 1-2, 1-3, 1-4, 2-3, 2-4, 2-5, 3-5, 4-5 |
| `ws.sequence` | `ws."ws.watch.7#0"[*].body.cols` | 16→16 | 1-2, 1-3, 1-4, 2-3, 2-4, 2-5, 3-5, 4-5 |
| `ws.sequence` | `ws."ws.watch.7#0"[*].body.data` | 165→165 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5, 4-5 |
| `ws.sequence` | `ws."ws.watch.7#0"[*].body.raw_b64` | 10→10 | 1-2, 1-3, 1-4, 2-3, 2-4, 2-5, 3-5, 4-5 |
| `ws.sequence` | `ws."ws.watch.7#0"[*].body.rows` | 16→16 | 1-2, 1-3, 1-4, 2-3, 2-4, 2-5, 3-5, 4-5 |
| `ws.sequence` | `ws."ws.watch.7#0"[*].body.type` | 54→54 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5, 4-5 |
| `ws.sequence` | `ws."ws.watch.7#0"[*].dir` | 54→54 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5, 4-5 |
| `ws.sequence` | `ws."ws.watch.7#0"[*].masked` | 54→54 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5, 4-5 |
| `ws.sequence` | `ws."ws.watch.7#0"[*].opcode` | 10→10 | 1-2, 1-3, 1-4, 2-3, 2-4, 2-5, 3-5, 4-5 |
| `ws.sequence` | `ws."ws.watch.7#0"[*].type` | 54→54 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5, 4-5 |
| `ws.structure` | `ws.by_type."ws.chat.2#0".complete[*].body.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `ws.structure` | `ws.by_type."ws.chat.2#0".loading_progress` | 6→6 | 1-2, 1-3, 1-5, 2-4, 3-4, 4-5 |
| `ws.structure` | `ws.by_type."ws.chat.2#0".model-selection-changed[*].body.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `ws.structure` | `ws.by_type."ws.chat.2#0".pilotdeck-command[*].body.options.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `ws.structure` | `ws.by_type."ws.chat.2#0".projects_updated` | 6→6 | 1-2, 1-3, 1-5, 2-4, 3-4, 4-5 |
| `ws.structure` | `ws.by_type."ws.chat.2#0".projects_updated[*].body.changedFile` | 1→1 | 1-4 |
| `ws.structure` | `ws.by_type."ws.chat.2#0".projects_updated[*].body.projectListRevision` | 1→1 | 1-4 |
| `ws.structure` | `ws.by_type."ws.chat.2#0".projects_updated[*].body.projects[*].lastActivity` | 1→1 | 1-4 |
| `ws.structure` | `ws.by_type."ws.chat.2#0".projects_updated[*].body.projects[*].sessions[*].createdAt` | 2→2 | 1-4 |
| `ws.structure` | `ws.by_type."ws.chat.2#0".projects_updated[*].body.projects[*].sessions[*].created_at` | 2→2 | 1-4 |
| `ws.structure` | `ws.by_type."ws.chat.2#0".projects_updated[*].body.projects[*].sessions[*].id` | 2→2 | 1-4 |
| `ws.structure` | `ws.by_type."ws.chat.2#0".projects_updated[*].body.projects[*].sessions[*].lastActivity` | 2→2 | 1-4 |
| `ws.structure` | `ws.by_type."ws.chat.2#0".projects_updated[*].body.projects[*].sessions[*].updated_at` | 2→2 | 1-4 |
| `ws.structure` | `ws.by_type."ws.chat.2#0".projects_updated[*].body.timestamp` | 1→1 | 1-4 |
| `ws.structure` | `ws.by_type."ws.chat.2#0".session-activity[*].body.activity.completedRunId` | 15→12 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.structure` | `ws.by_type."ws.chat.2#0".session-activity[*].body.activity.runId` | 27→24 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.structure` | `ws.by_type."ws.chat.2#0".session-activity[*].body.activity.sessionId` | 20→24 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.structure` | `ws.by_type."ws.chat.2#0".session-input-accepted[*].body.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `ws.structure` | `ws.by_type."ws.chat.2#0".status[*].body.id` | 6→0 | 1-2, 1-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.structure` | `ws.by_type."ws.chat.2#0".status[*].body.sessionId` | 12→0 | 1-2, 1-3, 1-4, 1-5 |
| `ws.structure` | `ws.by_type."ws.chat.2#0".stream_delta[*].body.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `ws.structure` | `ws.by_type."ws.chat.2#0".text[*].body.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `ws.structure` | `ws.by_type."ws.chat.3#0".complete[*].body.runId` | 6→0 | 1-2, 1-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.structure` | `ws.by_type."ws.chat.3#0".complete[*].body.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `ws.structure` | `ws.by_type."ws.chat.3#0".loading_progress` | 8→8 | 1-2, 1-3, 1-5, 2-3, 2-4, 2-5, 3-4, 4-5 |
| `ws.structure` | `ws.by_type."ws.chat.3#0".model-selection-changed[*].body.runId` | 6→0 | 1-2, 1-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.structure` | `ws.by_type."ws.chat.3#0".model-selection-changed[*].body.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `ws.structure` | `ws.by_type."ws.chat.3#0".pilotdeck-command[*].body.options.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `ws.structure` | `ws.by_type."ws.chat.3#0".projects_updated` | 6→6 | 1-2, 1-3, 1-5, 2-4, 3-4, 4-5 |
| `ws.structure` | `ws.by_type."ws.chat.3#0".projects_updated[*].body.changedFile` | 1→1 | 1-4 |
| `ws.structure` | `ws.by_type."ws.chat.3#0".projects_updated[*].body.projectListRevision` | 1→1 | 1-4 |
| `ws.structure` | `ws.by_type."ws.chat.3#0".projects_updated[*].body.projects[*].lastActivity` | 1→1 | 1-4 |
| `ws.structure` | `ws.by_type."ws.chat.3#0".projects_updated[*].body.projects[*].sessions[*].createdAt` | 2→2 | 1-4 |
| `ws.structure` | `ws.by_type."ws.chat.3#0".projects_updated[*].body.projects[*].sessions[*].created_at` | 2→2 | 1-4 |
| `ws.structure` | `ws.by_type."ws.chat.3#0".projects_updated[*].body.projects[*].sessions[*].id` | 2→2 | 1-4 |
| `ws.structure` | `ws.by_type."ws.chat.3#0".projects_updated[*].body.projects[*].sessions[*].lastActivity` | 2→2 | 1-4 |
| `ws.structure` | `ws.by_type."ws.chat.3#0".projects_updated[*].body.projects[*].sessions[*].updated_at` | 2→2 | 1-4 |
| `ws.structure` | `ws.by_type."ws.chat.3#0".projects_updated[*].body.timestamp` | 1→1 | 1-4 |
| `ws.structure` | `ws.by_type."ws.chat.3#0".session-activity[*].body.activity.completedRunId` | 15→12 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.structure` | `ws.by_type."ws.chat.3#0".session-activity[*].body.activity.runId` | 27→24 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.structure` | `ws.by_type."ws.chat.3#0".session-activity[*].body.activity.sessionId` | 20→24 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.structure` | `ws.by_type."ws.chat.3#0".session-input-accepted[*].body.runId` | 6→0 | 1-2, 1-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.structure` | `ws.by_type."ws.chat.3#0".session-input-accepted[*].body.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `ws.structure` | `ws.by_type."ws.chat.3#0".status[*].body.runId` | 18→0 | 1-2, 1-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.structure` | `ws.by_type."ws.chat.3#0".status[*].body.sessionId` | 12→0 | 1-2, 1-3, 1-4, 1-5 |
| `ws.structure` | `ws.by_type."ws.chat.3#0".stream_delta[*].body.runId` | 6→0 | 1-2, 1-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.structure` | `ws.by_type."ws.chat.3#0".stream_delta[*].body.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `ws.structure` | `ws.by_type."ws.chat.3#0".text[*].body.runId` | 6→0 | 1-2, 1-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.structure` | `ws.by_type."ws.chat.3#0".text[*].body.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `ws.structure` | `ws.by_type."ws.watch.4#0".input-queue-state[*].body.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `ws.structure` | `ws.by_type."ws.watch.4#0".watch-session[*].body.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `ws.structure` | `ws.by_type."ws.watch.5#0".input-queue-state[*].body.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `ws.structure` | `ws.by_type."ws.watch.5#0".session-activity-snapshot[*].body.activities[*].completedRunId` | 15→12 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.structure` | `ws.by_type."ws.watch.5#0".session-activity-snapshot[*].body.activities[*].runId` | 15→12 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5 |
| `ws.structure` | `ws.by_type."ws.watch.5#0".session-activity-snapshot[*].body.activities[*].sessionId` | 8→12 | 1-2, 1-3, 1-4, 1-5, 2-3, 3-4, 3-5 |
| `ws.structure` | `ws.by_type."ws.watch.5#0".unwatch-session[*].body.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `ws.structure` | `ws.by_type."ws.watch.5#0".watch-session[*].body.sessionId` | 4→0 | 1-2, 1-3, 1-4, 1-5 |
| `ws.structure` | `ws.by_type."ws.watch.6#0".auth_url[*].body.url` | 8→8 | 1-4, 2-4, 3-4, 4-5 |
| `ws.structure` | `ws.by_type."ws.watch.6#0".output` | 8→8 | 1-2, 1-3, 1-4, 2-3, 2-4, 2-5, 3-5, 4-5 |
| `ws.structure` | `ws.by_type."ws.watch.6#0".output[*].body.data` | 60→60 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5, 4-5 |
| `ws.structure` | `ws.by_type."ws.watch.7#0".output` | 8→8 | 1-2, 1-3, 1-4, 2-3, 2-4, 2-5, 3-5, 4-5 |
| `ws.structure` | `ws.by_type."ws.watch.7#0".output[*].body.data` | 130→130 | 1-2, 1-3, 1-4, 1-5, 2-3, 2-4, 2-5, 3-4, 3-5, 4-5 |

<!-- END B2 ROUND2 CLUSTERS -->

## 第 3 轮（2026-10-14）：串行 live2 k=5 离线重算

**最终 self_compare=false，4/10 PASS；十对共 1718 → 24 条诊断，只剩 changedFile 内容差异。** PTY 输出分块/双向交错与七条 watcher 时钟路径的差异已经全部消失。没有为达成 10/10 擦掉 watcher 文件名。

本轮未提交、未启动服务，保留上一轮未提交成果；未修改 `state/`、`specs/parity/`、`specs/gateway-parity/`、`scripts/parity/`、`scripts/gateway-parity/` 或 live2 原始归档及原 ruler.json。原始归档哈希记录在 `_tmp/b2-round3-archive-sha256.json`，收尾再次核对一致。

| TS 对 | live2 原诊断数 | 第 3 轮诊断数 | 结果 |
|---|---:|---:|---|
| 1–2 | 161 | 4 | FAIL |
| 1–3 | 184 | 4 | FAIL |
| 1–4 | 174 | 4 | FAIL |
| 1–5 | 178 | 0 | PASS |
| 2–3 | 186 | 0 | PASS |
| 2–4 | 173 | 0 | PASS |
| 2–5 | 182 | 4 | FAIL |
| 3–4 | 180 | 0 | PASS |
| 3–5 | 115 | 4 | FAIL |
| 4–5 | 185 | 4 | FAIL |

**实施内容与边界**

1. `/shell` 只通过实际成功握手路径选择。完整验证原始 wire、mask、seq、分片链、双向 close 后，按连接/方向将 `output.data` 还原为 UTF-8 完整字节流比较；分块长度、累计偏移、wire 帧数进入 `pty_chunk_diagnostics`，不进入硬门。output envelope 必须恰好为 type/data，额外字段失败。resize 的值、次数及方向内顺序完整保留，其他非 output 帧也按原始方向内顺序比较；不比跨方向交错。普通 `/ws` 的全序不变，auth_url 不丢不排序。
2. WS `projects_updated` 新增完整路径 `projects.*.sessions.*.{createdAt,created_at,lastActivity,updated_at}`，以及原报告同样有差异且源码证明是墙钟的 `projects.*.lastActivity`、`projectListRevision`、`timestamp`。每个非 null 已登记时钟保留原类型和本广播的时间排名，同实体、跨实体的大小/相等关系都在；数组不排序。每侧独立拒绝创建时间晚于活动时间，因此两侧同坏也 FAIL。未知路径、null 与缺字段不擦。未修改其他既有时钟规则或 SSE 门限。
3. normalization.md 第 3 轮逐条给出完整字段路径、来源和对照。PTY 来源链包括 `ui/node_modules/node-pty/lib/unixTerminal.js:92`–95 的 tty.ReadStream、`lib/terminal.js:90`–93 的 data→onData、`ui/server/index.js:2900`–2953 的 output；resize 在 `index.js:2994`–2999。session 时钟在 `ui/server/projects.js:106`–117，project/revision/broadcast 时钟在 `projects.js:68`–79、247–270 和 `index.js:334`–337。全部为基线只读。
4. 新增 `controls/streams_and_clocks.py` **27 条端到端对照**，包含 2 条等价 PASS、25 条故障/同坏 FAIL；加既有 181 条，共 **208/208 PASS**。用户要求的输出改一个字节、少一段、多一段、resize 改值/少一次、同实体两个时钟交换，全部命中预期 rule/location。还验证输出换序/方向、resize 重复/换序、mask、envelope 未知字段、auth_url 次序、跨实体时间交换、别名相等关系、类型/null/缺失/非法/未知字段和 changedFile；非 output 分片中 ping 的位置也保留。

**PTY 实录诊断。** 只取客户端边界 downstream-read/downstream-write，避免把同一数据另一条转发腿重复计入。`ws.watch.6` 五轮 output 分块数为 `10/12/8/9/8`，总字节数每轮均 **192**，SHA256 前缀 `5c9496822d76`；`ws.watch.7` 为 `20/20/17/18/16`，每轮均 **220** 字节，SHA256 前缀 `0da245cdaa7d`。完整哈希、原 seq 和 watcher 值见 `_tmp/b2-round3-variants.json`。

**剩余 4 簇。** 均为第二次 `projects_updated.changedFile`：TS1/TS5 是 `exp008-fixture/chats/web:33333333-3333-4333-8333-000000000002.jsonl`，TS2/TS3/TS4 是同文件名后追加 `.projections.json`。因此组内 1–5、2–3、2–4、3–4 PASS；跨组的六对各 4 条 FAIL。每对两个 chat 连接，各有全序与 by_type 两个诊断；不是四种独立内容问题。

| 比较规则 | 剩余完整字段簇 | 十对累计 |
|---|---|---:|
| `ws.sequence` | `ws."ws.chat.2#0"[*].body.changedFile` | 6 |
| `ws.sequence` | `ws."ws.chat.3#0"[*].body.changedFile` | 6 |
| `ws.structure` | `ws.by_type."ws.chat.2#0".projects_updated[*].body.changedFile` | 6 |
| `ws.structure` | `ws.by_type."ws.chat.3#0".projects_updated[*].body.changedFile` | 6 |

完整前后聚类为 `_tmp/b2-round3-clusters.json` / `.md`，仅把数组数字下标折叠，按现有 summarize 脚本得到 54 个前后并集簇，其中 4 个仍有差异；十对所有 differences 均计入。单独剩余表为 `_tmp/b2-round3-remaining.json`。来源 `ui/server/index.js:332`–340 把 scheduler 的 filePath 转相对路径发出。本轮不接受把不同文件视为等价，后续应单独研究 watcher 触发来源与前端消费语义。

**最终验证与产物**

- `_tmp/20261014-b2-round3-ruler.json`：全部十对及 proxy 原始结果，`per_rule_negative=true`、`same_bad_rejected=true`、`independent_review=null`、`errors=[]`；整体 FAIL，exit 1。报告中的当前 apparatus 哈希与收尾源码一致。
- `_tmp/b2-round3-controls.json`：208/208 PASS，missing_rule_controls=[]；与最终 ruler 内 controls 一致。
- `_tmp/b2-round3-tests.txt`：Python 43/43 PASS；旧 SQLite 测试仍有 ResourceWarning，无失败。`_tmp/b2-round3-chat-tests.txt`：串行驱动离线屏障测试 PASS。Python 编译、Node 语法及 git diff --check 通过。
- `_tmp/b2-round3-same-archive.json`：五个归档各自与自身比较均 PASS/0 differences，仅证明完整性，不计入独立十对。
- `_tmp/b2-round3-strict-rejection.json`：未带 replay 时，旧归档被当前 apparatus 哈希门拒收；没有修改 run-meta 或关闭完整性门。
- live2 proxy 与 TS1 重放 **FAIL / 20 条诊断**：7 个 101 响应仅 proxy 有 Date；7 个连接各多 1 个下行 close，产生 9 条序列/结构诊断；另 4 条 changedFile。没有据此改变规则。
- `check_frozen.py` 仍为 DRAFT/未批准。报告 `comparison_mode=historical-replay`、`calibration_eligible=false`；原报告 SHA256 与原装置哈希保留在 replay_provenance。该离线结果不充当当前装置的新采集、独立评审或冻结证明。

复现最终重算（不启动服务；预期 exit 1，因为仍有 changedFile 差异）：

```sh
python3 scripts/ui-server-parity/check_ruler.py \
  --round _tmp/20261014-b2-live2-ts1/direct \
  --round _tmp/20261014-b2-live2-ts2/direct \
  --round _tmp/20261014-b2-live2-ts3/direct \
  --round _tmp/20261014-b2-live2-ts4/direct \
  --round _tmp/20261014-b2-live2-ts5/direct \
  --enumeration _tmp/20261014-b2-live2-ts1/enumeration/router-enumeration.json \
  --replay-of _tmp/20261014-b2-live2-ruler.json \
  --proxy _tmp/20261014-b2-live2-proxy/direct \
  --out _tmp/20261014-b2-round3-ruler.json
```


## B2 第 3 个 PR · 第 4 轮：原版多态参考池（DRAFT）

按人已批的阶段 C 1B 原则，将 chat 两连接第二次 projects_updated.body.changedFile 登记为精确多态字段，见 [multimodal.md](../../../specs/ui-server-parity/multimodal.md)。参考池 >=20 遍 TS、每类 >=2；同日、同源、同采集路径且与全新 k=5/proxy 独立。类别先由 TS 全部两两严格比较确定，候选须完整命中某类所有成员；其他字段严格比较不变。

本轮离线 controls 221/221、Python 49/49；原 live2 严格重放仍 4/10 PASS。没有新真实参考池/live3，不声称定标完成。完整交接和证据在本 worktree `_tmp/RESULT.md`「第 4 轮」，协调者运行 `_tmp/b2-round4-coordinator.zsh` 采 20+5+1 并验收；该脚本共用一个 live3 --out 前缀，完整停服务后改名，所有实际路径字节相等。compare/check_ruler 正式入口需显式 --reference-pool；只作旧配对诊断时传 --paired-only，不能据此定标。
