# Always-On error code oracle

固定旧 TS 基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，mbp2 Node v22.22.0 实跑 `DiscoveryFire.runApplyPhase` 四类 error.code：undefined、null、空串、命名值。原始运行与脚本在主仓 `_tmp/20261108-cli-completion/always-on-error-code/`。JSON SHA256 `c5466d74f2ac330383522b0a357c83ba6eb530943eab87c37ccb0ff36b4d5672`。保留原始随机隔离路径与时间；Rust 只比较 error 语义，不把路径当判据。
