# native-session-e2e 端到端控制（#1140）

本套件用真实 loopback socket mock 网关和 provider，OS 的 strace/pidfd/platform 为确定性桩。它验证采集器、认证与评分接线；不证明真实 Rust 的全部场景行为，刺激对齐仍由 #1242 接续。

`mock_gateway.py` 按登记期望发 event/seq/final 信封，写带项目身份的 `.cwd`、转录与 registry；`test_e2e.py` 经采集器走原始附件、receipt、冻结认证和评分。被测的评分投影负控在先认证健康输入后单独改观测，避免前置认证拒收掩盖评分分支；认证负控则重钉 receipt，完整走 loader。逐案 PASS 与整批 NOT_RUN 分开断言。

端口在受控的 gateway 28999..29008 系列中成对探测，并预留 gateway 与下一 provider 端口。provider 实际绑定前必须释放预留，因此第三方抢占会让本样本失败；不借用用户进程。并行副本验证各自取不同端口对。

`negative_controls.py` 的 `cases` 与执行汇总是变异列表和数量的唯一来源。每项绑定完整测试名与专属失败消息，先 compile，再 positive → mutant → restored，全部跑完汇总。原地模式用共享 mutating_sources/run_owned，并要求被测源码与 HEAD 一致；新冻结文件提交之前由协调者在提交后运行。

```sh
python3 -I src/pilotdeck-gateway/tests/fixtures/native-session-e2e/test_e2e.py
python3 -I src/pilotdeck-gateway/tests/fixtures/native-session-e2e/negative_controls.py --check-anchors
# 提交后，调用方已提供 CARGO_TARGET_DIR：
python3 -I src/pilotdeck-gateway/tests/fixtures/native-session-e2e/negative_controls.py <新证据目录> [--case NAME]
# 未提交 Python 源码的只读内存预演，同样要求正控/具名断言/恢复正控：
python3 -I src/pilotdeck-gateway/tests/fixtures/native-session-e2e/negative_controls.py <新证据目录> --memory --case drop-contract-connections
```

Rust 入口 `native_session_e2e.rs` 拉起 Python 套件。正式冻结 CLI 的 HEAD 与登记核验仍由提交后的协调者链执行，内存加载与合成证据不能替代它。

fix-r7：provider 预留 socket 与 HTTPServer 同用 SO_REUSEADDR，避免正常 HTTP 关闭后的 TIME_WAIT 被误认为活服务；gateway 仍按无复用策略预留。失败的保留尝试关闭两只 socket。Gateway.close 主动关闭 listener/活动连接并有界 join；这里都是本进程线程，没有 provider/gateway 子进程。class cleanup 在 setUpClass 成功前登记，异常也清理临时目录。`LifecycleTests` 覆盖同端口对连续两格、预留失败、未连接/半帧取消、采集异常；新配对变异归 `native-witness/negative_authentication.py`，其 `--memory --case provider-probe-no-reuse` 可在未提交树预演，完整源变异仍留提交后的宿主执行。

fix-r8：close 的 True 同时要求全部 manifest case 已发完最终帧、线程已退出、没有错误；未完成取消即使资源已回收仍返回 False。连接空闲读保留 180 秒，取消靠 shutdown 唤醒而非等待超时。profile 两条负控由 LifecycleTests 显式调用采集 harness，仅将精确的 session profile 异常转成测试专属 AssertionError，不把 setUpClass ERROR 或无关异常算作 KILLED。macOS 生命周期和 SO_REUSEADDR 负控见 fix-r8-res/RESULT.md；Linux 复核待协调者宿主执行。
