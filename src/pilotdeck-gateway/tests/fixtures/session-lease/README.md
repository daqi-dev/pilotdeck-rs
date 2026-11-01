# #1221 calibrate 租约释放

`test_lease.py` 使用独立临时目录租约，真实有界 Python 子进程和确定性钩子；不运行 TS、不改判分、不绕过真实 CLI 的 HEAD 守卫。BLOCKED/nonzero/cancel/deadline 退出后只有已持有且 reap 的 leader、无清理错误、最终 `killpg(pgid, 0)` 为 ESRCH 才允许释放；存在最多等待 2 秒，权限和其他错误立即保留。原 processTreeCleanup 与 cleanup_status 不变，stale cleanupPending 仍不得接管。

```sh
python3 -I src/pilotdeck-gateway/tests/fixtures/session-lease/test_lease.py -v
python3 -I src/pilotdeck-gateway/tests/fixtures/session-lease/negative.py --check-anchors
# 提交之后；CARGO_TARGET_DIR 必须由调用方设置，脚本本身不运行 cargo。
python3 -I src/pilotdeck-gateway/tests/fixtures/session-lease/negative.py NEW_OUTPUT
```

14 个源码负控覆盖旧的仅按判分释放、非 PASS 清理、登记、reap/身份/pgid、严格布尔证据、清理错误、探针错误、仍存活的组、有限等待。每项先编译 Python 变异，绑定单测与 #1221 单行消息，恢复后重跑；全部变异汇总 KILLED/SURVIVED/INVALID，锚点过期在运行前拒绝。源码保护使用 mutating_sources，未提交候选必须留给协调者在提交后运行。真实 calibrate → deadline 的集成复算仍须提交后的 HEAD/FROZEN 校验通过，单测不代替它。
