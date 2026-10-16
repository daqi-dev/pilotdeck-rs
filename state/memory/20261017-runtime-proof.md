---
name: runtime-proof
description: SHA核源码不保证Python实际执行该源码，故障注入要区分解释器拒绝与内核errno。
type: feedback
---

为什么：R81实际同秒同size pyc反例中source VALUE2的SHA校验与module.__file__检查均过，旧加载器却执行VALUE1；dont_write_bytecode只阻写，不阻读。fsync(-1)在Python3.14先抛ValueError，前两报告的EBADF措辞已撤；正整数已关闭FD才实际得到OSError errno9。两类原件均保，不用校验成功或错误文本代替真实执行路径。

怎么用：需要源码身份闭包时，no-follow读取并SHA核字节后直接compile/exec，固定依赖顺序与module路径，不加载旧pyc；loader也以原反例做正反验证。故障注入记录实际异常类型/errno/原函数路径/返回值与FD终态，ValueError不称内核失败；健康返回在方法自身审计前落raw。准备控制只证明对应层，Node/Python/产品、合成事实/真实cleanup分别记，不互相代票。原件主仓_tmp/20261016-debt-cleanup/r81-fixedloader-production-t0zgqle8/；历史PARTIAL在r81-production-slot-method-lb2kgwhv/。
