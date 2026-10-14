# gateway 对拍尺子冻结记录（阶段 A · exp006）

> **2026-10-15 重登记（issue #68，人批）**：accepted-diffs.md 新增 A4-FILE-TOCTOU-001（project_file_read 检查后符号链接替换，原版安全缺陷）；其余文件哈希不变。两家评审见 PR。

> **2026-10-14 重登记（A4 前置 #45，PR #50）**：时钟归一收紧到容器组合、装置哈希改 12 件显式白名单（未登记脚本即 FAIL）、具名键前置写入 normalization；两家（GLM、Claude 替补 DeepSeek）两轮均无 P0/P1；rebase 后正式重跑 `check_ruler` 机器项全 true。**AI 代批，待人复核**。
> 2026-10-14 冻结。**AI 代批，待人复核**（依据：`check_ruler.py` 机器项全 true——TS 自比 k=5 十对、五个不同 gateway 进程、阳性对照 33 项主规则命中、两侧同坏拒绝、代理一致；双谱系评审第 3 轮 GLM、DeepSeek 均 `independent_review: true`，无 P0/P1，P2 转跟进 issue）。
> 冻结后改任何一份都要另开 issue、两家评审同意、写明理由（`CLAUDE.md` §五「尺子不许自己改」）。A3 的 schema 产物与脚本另见 `schema-FROZEN.md`，不在本表。
> 校验：`python3 scripts/gateway-parity/check_frozen.py`（下面每行「sha256 两空格 相对本目录的路径」）。

886d12cdd36a5ecac53d314fabd52fed9ecd093b2a07939bbcf553d88b37e519  acceptance.md
122eb3f8f75300144f76445bb7f9576ae06c1ae7795481c13d50b0f92376fcfa  normalization.md
f951a8943194f2064b9faedecc8ae123dffc9757d4377c76f554b840c1ec54da  accepted-diffs.md
cdea9efadae918a7894f54d3fba7bb535e4da2cda961cfd54997e956cc4994b0  ../../scripts/gateway-parity/check_frozen.py
3517f52c318da237f17d8634ee4bf3da64f0d8c063feecd38cd664c33ea12d89  ../../scripts/gateway-parity/check_ruler.py
62798f75d13cca08a377d71b72746c39f1a054494b5e73565671785a18dea2dc  ../../scripts/gateway-parity/common.py
4ae7570b725bfb79fdef01b0860d67f0a36864582a667da5a1e53eba1ecb8436  ../../scripts/gateway-parity/compare.py
0bf89d5277e04fea0da89da8d24a2c05680d1d77a6c20390a8a27a2cd28b0da5  ../../scripts/gateway-parity/controls/prerequisites.py
13e1f91b3fe5c81140dd8fcbf9d6ac24695ec8f1098b330c6ab0bc4ef2c382ea  ../../scripts/gateway-parity/controls/run_all.py
2cb39a091ba02f745a1d1ade35b77562ed96d6ba61a9979cf0ac6cffa61deb17  ../../scripts/gateway-parity/driver.mts
95fe265a7b7512f1969f19b7ee2d14133ce56916fb25c0953a84b5ec05568f74  ../../scripts/gateway-parity/mock.mjs
3a758aa57cbb5d8129c2e9b73c0baa04028a87aa7bae5dba6d8519f713869692  ../../scripts/gateway-parity/observer.py
e3529e28181f338a8ff09fa6b7d6c8ce669af86b16806aca32bd0995e86918f6  ../../scripts/gateway-parity/run_round.py
9d8a1825e4a152affbcb6df6c8e3e682edad634281e782ad21c1493e673c1d67  ../../scripts/gateway-parity/test_safety.py
e1dfeac23d5cfd0777fd2836cb1011430c73d6b3b10fe44724738194f49901df  ../../scripts/gateway-parity/watcher.mjs
