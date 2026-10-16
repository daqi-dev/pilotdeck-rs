---
name: archive-read-boundary
description: archive存在性校验不能先读摘要；NOFOLLOW祖先替换可报ENOTDIR，区分真实canonical诊断。
---

归档安全并非只看最后返回的字节。旧候选为项目存在性调用list_projects，会顺带打开聊天摘要，发生在archive containment之前。独立实测又发现macOS的NOFOLLOW|DIRECTORY面对被替换祖先/根返回ENOTDIR，而leaf通常ELOOP；只给ELOOP打安全标签会把祖先替换误判普通错误并退回不安全TS。失败与修复探针见主仓`_tmp/20261016-debt-cleanup/archive-implementation/`的tests-host.log、tests-openat-final.log。

下次安全读盘先梳理存在性/helper的所有内容打开，不借摘要完成注册关系校验。canonical阶段自然ELOOP/ENOTDIR与后续checked stat/open失败分开；安全拒绝不能转发TS。保留实际底层打开与普通read阳性对照，不能只凭ErrorKind或代码审阅。共享原语不共享批准范围；archive的#134精确差异仍待授权，本记录不改冻结或宣称已接受。
