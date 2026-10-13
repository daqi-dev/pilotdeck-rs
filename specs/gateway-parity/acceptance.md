# Gateway 对拍验收口径（草稿，Issue #23 / A2）

未冻结。独立审查由协调者填写；不得把 `null` 当成 true。基线来源为 `PDRS_BASELINE`，只读；被测侧在 A2 是 TS gateway 直连与 TS gateway 经 A1 Rust 透明代理。A2 不把透传计作 Rust 本地实现。

## 场景分母

固定八组，顺序定义在 `scripts/gateway-parity/common.py:SCENARIOS`，不得依据 Rust 结果排除场景。各组保存 driver 原始录帧、结果、完整性尾标；HTTP 组保存请求路径、status、完整头、body 和带单调时钟的 chunks。

| 组 | 独立预期 |
|---|---|
| auth | hello 前请求、错版本、错 token、非法 JSON 分别关闭 4001/4001/4003/4002；合法 hello 有 binding，正常关闭 1000 |
| turn | 官方 `RemoteGateway.submitTurn` 发一轮，流中有固定 `GP_MOCK_OK`，唯一 final 为 turn_completed；文本已落盘 |
| readonly | `method-inventory.md` 末尾冻结小结的 10 个 (a) method 全部成功，不用错误响应充数；artifact 是项目夹具中的真实文件 |
| reconnect | 在真实 ask_user_question 形成 pending 后保存旧 binding、关闭第一连接、第二连接 reconnect_interaction 与 active_turn_snapshot 回放同一 requestId；显式断言基线已知 answer.delivered=false，再 abort 释放轮次 |
| permission | `permissions.json` 显式 ask bash，真实轮次产生 permission_request；deny 的 delivered=true，收到终态；含交错 permission_changed 通知 |
| writes | cron_create 使用固定 2099 once，不触发定时执行；skill_create/skill_write 成功，task 和 Written 内容真实落盘 |
| http | `/health`、`/auth/local-token`、`/api/web/projects`、项目文件 read/write、无 Bearer 的 401；另测明确标注测试路由的 SSE/NDJSON 增量转发 |
| quirks | 原始 TCP WS：RSV 非零仍解 hello；孤立 continuation 被忽略后仍解 hello；客户端 4444 close 回 1000/空 reason；unmasked 引发 TS CLI 异常退出流程（监听器关闭，原 socket 可悬挂） |

10 个只读 method 按序：native_archive_manifest、native_archive_entries、native_archive_artifact、project_files_list、project_file_read、model_catalog_list、session_model_get、supported_agents、list_projects、describe_project。覆盖依据是冻结小结的 **10**；研究正文残留的 15 不作为分母。

## 机器规则

| 规则 | 拒绝条件 |
|---|---|
| G0.round / G0.paths | 未完成、清理失败、运行路径/端口/基线身份不一致。PILOT_HOME/cwd/TMPDIR 必须同路径逐轮运行后改名归档 |
| G1.coverage | 固定组缺失/多出、身份不符、10 method 顺序/集合不全、组内要求的 method 未调用 |
| G2.recording | 非法或截断 JSONL、空 WS 录帧、非连续 recorder seq、raw/parsed 或原始 wireHex/解码不一致、连接数/关闭尾标不符、完整性 footer 缺失；落盘 JSONL 同样不得截断；模型请求原始 fingerprint 必须可重算 |
| G2.event-sequence / G2.link | 每个连接的每个 request 流从 seq=0 连续递增；响应/事件必须引用已录制的唯一 request ID，event 只能来自 submit_turn |
| G2.observer | A1 代理侧上游独立录制必须连续且含全部 10 个透传只读请求，空观测点不能冒充未转发 |
| G3.oracle | 任一侧独立业务预期失败。即使两侧相同也 FAIL；检查原帧、实际落盘及 HTTP 流证据，不仅信 driver 的 PASS 标记 |
| G4.frames | 同连接全序、direction、全部语义载荷、通知、event.seq/final、close code/reason、WS 探针语义或头行为有差异 |
| G5.http | status、非 hop-by-hop 头、完整 body 有差异；流式健康还须两段以上、首末数据相隔至少 300ms，固定内容齐全 |
| G6.disk | home/project 的目录树、文件内容或 SQLite 逻辑内容有差异（仅排除明确的运行诊断目录） |
| G7.no-forward（A4 可选） | compare 的 --local-method 声明仍出现在独立上游日志；输出一致、成功响应、非本地锚点录制完备三项同样必须满足，空观测点不能证明本地化 |

规则健康检查先于相等判决。FAIL 带规则号并 exit 1；比较器不会通过“两个坏日志一样”放行。未经 schema 导出/审批，A2 的结构检查不称作 A3 JSON Schema 校验。

## 四项可信度检查与冻结

`check_ruler.py` 跑 5 个 TS 直连轮次，同路径，每两轮比较，共 10 对；八组和三面必须 **100% 一致**，零排除。逐规则阳性对照：改帧、删事件、改落盘、缺场景、截断，另测 HTTP/异路径；每项必须 FAIL 且命中预期规则。两侧同坏单独要求 G3.oracle FAIL。最后 `independent_review=null`，由协调者独立审查后登记。

额外要求直连 vs A1 代理八组/三面 100%。全部机器项 true、独立审查未填时报告 PENDING_REVIEW / exit 2，不能声称全部四项已绿。

草稿经协调者批准后另建 `FROZEN.md`，行格式 `sha256  filename`，至少登记三份规约；建议同时登记装置的全部 Python/MTS/MJS 文件（相对该目录，如 `../../scripts/gateway-parity/compare.py`）。`check_frozen.py` 无 manifest 返回 DRAFT / exit 2；缺项、重复项、路径越界或哈希漂移 exit 1。A2 worker 不自行冻结。阶段 C 冻结目录保持不动。
