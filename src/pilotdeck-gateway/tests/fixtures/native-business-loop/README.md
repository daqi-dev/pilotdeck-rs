# Native business loop diagnostic

未冻结产品端到端探针，不替代正式Gateway退役尺子。固定TS cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85的官方GatewayWsClient/RemoteGateway直接连接TS和原生Rust WS；mock实际调用read_file并验证内容到达模型；另一turn请求bash写文件，permission_request后deny，要求真实失败tool_result及无副作用。关闭session/宿主后同home冷重启，历史和JSONL SHA不变。

## 运行

mbp2 Python3.11+和Node22；基线须已有dist、tsx依赖和Gateway源码；不运行Cargo。输出目录必须不存在：

```sh
python3 -I src/pilotdeck-gateway/tests/fixtures/native-business-loop/probe.py \
  --baseline /Users/da/ws/pdrs-runner/resources/PilotDeck \
  --gateway /absolute/fixed-bin/pilotdeck-gateway \
  --sidecar /absolute/fixed-bin/pilotdeck-agent-loop-sidecar \
  --out /absolute/exclusive/output \
  --provenance-label source-commit-and-build-manifest-reference
```

独占复制执行bin并核SHA；隔离HOME/PILOT_HOME/TMPDIR/project，顺序复用实际路径，侧末归档home；loopback mock、高位28xxx宿主。持既有gateway parity_lock直到provider及子进程清理完；锁忙有界拒跑。240秒绝对期限，子进程各自期限，24请求上限，1MiB请求体，配置maxOutputTokens256。仅清理已观察且pid/birth一致身份，不杀未知进程；不干净则失败且保留锁cleanupPending。

## 结果

result.json区分executionPass（两边业务闭环、清理与只读冷恢复）和alignmentPass（声明投影相等）；二者均过才pass/exit0；失败或差异exit1。comparison.json精确比较read/permission的事件类型顺序、工具toolName/ok/errorCode、终态type/finishReason，不删语义事件、不揉错误码。它不是完整帧/落盘对拍。

证据含每侧warm/cold raw wire、完整result、stdout/stderr、顶层provider请求响应、两个home完整落盘。provenance.json绑定执行bin、driver/probe/common、TS dist与Gateway源码SHA。旧r134必须标old-r134-e1d44bd17-diagnostic，不充新整合头验收；新整合bin另目录重跑。

旧r134首诊断：read/冷恢复、两边权限拒绝闭环成功；TS多permission_denied事件且finished.errorCode=permission_required，Rust无此事件且code=permission_denied。差异如实FAIL。probe树_tmp/20261110-business-diag-r1原始诊断与r2精确差异检测（exit1）保留。

不是Linux pidfd/strace见证，不证明无隐藏helper，不覆盖全部Gateway配置或正式阶段验收。#1355现役车道承接正式采集/判分接线，本探针不改共享冻结文件。
