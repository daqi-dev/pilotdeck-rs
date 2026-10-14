# gateway 对拍尺子冻结记录（阶段 A · exp006）

> 2026-10-14 冻结。**AI 代批，待人复核**（依据：`check_ruler.py` 机器项全 true——TS 自比 k=5 十对、五个不同 gateway 进程、阳性对照 33 项主规则命中、两侧同坏拒绝、代理一致；双谱系评审第 3 轮 GLM、DeepSeek 均 `independent_review: true`，无 P0/P1，P2 转跟进 issue）。
> 冻结后改任何一份都要另开 issue、两家评审同意、写明理由（`CLAUDE.md` §五「尺子不许自己改」）。A3 的 schema 产物与脚本另见 `schema-FROZEN.md`，不在本表。
> 校验：`python3 scripts/gateway-parity/check_frozen.py`（下面每行「sha256 两空格 相对本目录的路径」）。

886d12cdd36a5ecac53d314fabd52fed9ecd093b2a07939bbcf553d88b37e519  acceptance.md
291cddfd6d9eab7a44aa7e20ebd26aa3d6545ee467d1606a9003e2a4140b9a68  normalization.md
13da73b398182ae3a33ea6b57557f3c7221a143248efde9c19e4280e38ea6046  accepted-diffs.md
cdea9efadae918a7894f54d3fba7bb535e4da2cda961cfd54997e956cc4994b0  ../../scripts/gateway-parity/check_frozen.py
149b6ab067e532121e75573924e5e877750f46c3806bcde3b6e87a7a5d60855f  ../../scripts/gateway-parity/check_ruler.py
2240d21bd9851341848bf96e6459891bc72b4df7a2ee03aad80ebcb027918c6e  ../../scripts/gateway-parity/common.py
93016c464a329186d73694fb79290da57e899c60203c5318061ab1838182bfe0  ../../scripts/gateway-parity/compare.py
b2131718ef79d55f369db642c347405aabd453bf3aff3c8c2a674197a6a21602  ../../scripts/gateway-parity/controls/run_all.py
2cb39a091ba02f745a1d1ade35b77562ed96d6ba61a9979cf0ac6cffa61deb17  ../../scripts/gateway-parity/driver.mts
95fe265a7b7512f1969f19b7ee2d14133ce56916fb25c0953a84b5ec05568f74  ../../scripts/gateway-parity/mock.mjs
3a758aa57cbb5d8129c2e9b73c0baa04028a87aa7bae5dba6d8519f713869692  ../../scripts/gateway-parity/observer.py
e3529e28181f338a8ff09fa6b7d6c8ce669af86b16806aca32bd0995e86918f6  ../../scripts/gateway-parity/run_round.py
9d8a1825e4a152affbcb6df6c8e3e682edad634281e782ad21c1493e673c1d67  ../../scripts/gateway-parity/test_safety.py
e1dfeac23d5cfd0777fd2836cb1011430c73d6b3b10fe44724738194f49901df  ../../scripts/gateway-parity/watcher.mjs
