# #1221 calibrate 租约释放

`test_lease.py` 使用独立临时目录租约，真实有界 Python 子进程和确定性钩子；不运行 TS、不改判分、不绕过真实 CLI 的 HEAD 守卫。BLOCKED/nonzero/cancel/deadline 退出后只有已持有且 reap 的 leader、无清理错误、最终 `killpg(pgid, 0)` 为 ESRCH 才允许释放；存在最多等待 2 秒，权限和其他错误立即保留。原 processTreeCleanup 与 cleanup_status 不变，stale cleanupPending 仍不得接管。

#1221 更正 #209 E1 的 `allStopped` 定义：在原 `processTreeCleanup=VERIFIED` 且 leader 已回收、无清理错误的路径之外，增加本次持有且已回收 leader 的最终 ESRCH 租约专用证明。`allStopped` 是租约释放证明，不是判分通过；`processTreeCleanup`、`cleanup_status` 和判分规则均不变。历史说明见 `specs/gateway-parity/FROZEN.md` 的 #209 E1 段，代码变更登记见 `specs/gateway-parity/frozen/20261102-1221.md`；历史文件原字节保留，本段明确两者的更正关系。

```sh
python3 -I src/pilotdeck-gateway/tests/fixtures/session-lease/test_lease.py -v
python3 -I src/pilotdeck-gateway/tests/fixtures/session-lease/negative.py --check-anchors
# 提交之后；CARGO_TARGET_DIR 必须由调用方设置，脚本本身不运行 cargo。
python3 -I src/pilotdeck-gateway/tests/fixtures/session-lease/negative.py NEW_OUTPUT
```

`negative.py` 的独立 `EXPECTED_CASES = 27` 钉住完整清单：原 19 项，加 TERM/ALRM 注册遗漏与清单删项 3 项，加 #1519 两项（sdk-signals `capture.py` 自定义 `interrupted` 缺 #1194 展开期二次信号守卫、`--memory` 演练不持 tree_lock），再加 #1662 两项（把 `capture.py` 加回 guarded sources、capture 案例在 committed 模式退化为原地写盘），加 #1528 fix-r2 的 duplicate-lease-case 等长替换重名负控。全部入口（含 `--case`、`--memory`、`--check-anchors`）先核完整清单数量与行名唯一（漏项或等长替换重名立即非零）；summary 另核所选项执行结果。源码/文档负控覆盖旧的仅按判分释放、非 PASS 清理、登记、reap/身份/pgid、严格布尔证据、清理错误、探针错误、仍存活的组、有限等待。每项先编译 Python 变异，绑定单测与专属单行消息，恢复后重跑；全部变异汇总 KILLED/SURVIVED/INVALID，锚点过期在运行前拒绝。源码保护使用 mutating_sources，正式原地验证必须在提交后运行。`--memory` 仅在独立子进程编译并注入候选，不改磁盘、不冒充已提交验证，且整个演练循环在 `tree_lock(root, timeout=900)` 内执行（只读也持锁，与 committed 原地变异互斥）；`--case NAME` 选择单个样本。另覆盖 guarded sources 中 common.py 不可漏项、allStopped 文档定义与 CLI 的 TERM/ALRM 重复取消抑制。#1662：`capture.py` 的盘上字节由常驻 `sdk_signal_exit` 测试做 sha256 金样校验，因此不在本 fixture 的 guarded sources 内，capture 案例在 committed 模式也走 `--memory-run` 子进程注入（守卫集外一律不改盘），防止并发假 FAIL 与 SIGKILL 留变异体。CLI 测试通过真实信号覆盖正常执行、finally 展开、except 处理及恢复正常后的新取消，无 TS 对应实现。真实 calibrate → deadline 的集成复算仍须提交后的 HEAD/FROZEN 校验通过，单测不代替它。

#a5b 注册检查先设置 sentinel，再调用生产 `install_handlers()` / `install_handlers(lease=True)`，用 `signal.getsignal` 核 TERM/ALRM 注册和原 ALRM 返回值；保留原入口/取得租约后的安装时机。信号类负控在后台或 nohup 下须经宿主 `sigint-default.py` 恢复 SIGINT 默认行为后启动。
