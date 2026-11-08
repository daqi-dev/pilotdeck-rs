---
id: D1-NATIVE-TCP-BOUNDS-001-R1
original: D1-NATIVE-TCP-BOUNDS-001
---

## 类别

修正首次候选共享输入预算导致跨连接误杀的实现与资源描述。

## TS 依据

同原条目真实套接字基线；TCP client 不因别的连接半包停止而失去自己的连接。原条目8MiB共享在读预算让A占满无换行时B的hello失败，独立GLM评审P1揭露，不能算隔离通过。

## Rust 行为

每连接输入行各8MiB，32连接最多256MiB原始在读字节；共享decode门一次只展开/派发一帧，避免32份JSON解析放大。共享ledger及输出计费仍保持原预算，非RSS承诺。accept的对端复位、描述符资源压力退避重试；新连接准备失败丢弃该连接，已入场者继续；真正不完整执行回收仍要求进程监督退出。

## 范围

仅原生TCP owner，stdio不变。前次测试失败另含Darwin accept继承O_NONBLOCK，现显式切回阻塞reader/writer；r66三项socket与真实Rust TCP→Gateway→模型链均过。本条新增预算/错误恢复待后续源验证，不外推通过。

## 机器判据

`tcp_partial_frame_does_not_spend_another_connections_input_budget` 保留8MiB无换行仍成功hello/execute；`transient_accept_errors_preserve_the_real_listener` 在同生产accept循环注入ConnectionAborted和EMFILE后真socket hello成功。既有3socket与stdio用例保留；HOST字节按Node环境lossy UTF-8处理，不能静默把非法值当缺省。

## 批准记录

AI代批待人复核；未批准，非已接受差异。修本轮已登记native入口的隔离错误，不改冻结计分。
