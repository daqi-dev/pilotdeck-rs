# SOP resume wire values

固定基线 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，Node v22.22.0 / ICU78.2。自写 capture.mjs 直接 import 真实 StaffDeckSopControlPlane → SopStateStore；不复制业务源码，不从 Rust 生成 expected。Date 在采样前固定为 2026-01-02T03:04:05.000Z；无事后时间/错误归一化。每格独占目录、实际状态文件，比较每步结果与 after，另记原字节 unchanged。

复采（BASELINE、NEW_OUTPUT 必须为绝对路径；输出目录必须不存在）：

```sh
/absolute/node --import "$BASELINE/node_modules/tsx/dist/loader.mjs" /absolute/capture.mjs "$BASELINE" "$NEW_OUTPUT"
```

运行时另用 macOS sandbox-exec profile `(version 1)(allow default)(deny network*)`，HOME/TMPDIR 指向独占采集目录；不得裸跑到真实 HOME。脚本校验基线 HEAD、Node/ICU；fetch 兜底计数，网络 OS 拒绝，实际 stderr 为空、49格54步、networkAttempts=0。脚本固定版在 mbp2 重新执行并逐字对原 oracle，相同 SHA256：`ebd652e173029883e22bfe7798ccf253f9bb5bcdbd7d37836f1537a603d3a89c`。

仓内回归执行 44 个真实字符串 sessionKey 的 disk/receipt 格，共49步；module disabled 格由真实 NativeSopControl profile 门承担，另四个非 string sessionKey 的 Node Buffer.from 探索格仅保留原始证据，明确未支持、不计对拍。原数组/字符串/bool 槽位 spread、数字 requestId/message 首次成功、畸形 receipt 下一读被 migration 过滤、未知 source 只能重放缺 source 旧收据均保留。

本包不新增物理 sessionKey 类型兼容，不改变 human 认证/authority override 禁令；unknown source 不转换成 external_task。NativeSopControl 的 production 回归实际构建项目配置并调用真实 disk store，验证 missing-state 优先、数组槽位确实持久化及配置原件不变。Rust 仅由协调者在 mbp2 编译/执行，交付时尚未运行。
