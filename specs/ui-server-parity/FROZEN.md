# B2 ui/server 尺子冻结登记草稿（待定标）

> **待定标**：仅准备哈希清单。真实新装置 20 遍 TS 参考池 + 5 遍 TS 十对 + 1 遍 proxy、26 遍 clock PASS、独立双谱系评审及人批均待补。此文件不构成冻结或阶段验收。
> 定标命令及重采规则：`scripts/ui-server-parity/README.md`；不得借历史 replay 定标。
> 冻结时由协调者补定标证据与两家评审路径、替换下方审批及依据占位，再移除首行「待定标」。`Approved-By:` 接受具名人或「AI 代批，待人复核（协调者）」，并必须有 `Approval-Basis:` 记明定标报告路径和两家评审路径。冻结后变更需遵循 CLAUDE.md §五。
> 校验：`python3 scripts/ui-server-parity/check_frozen.py`；格式沿用 gateway：「sha256 两空格 相对本目录路径」。草稿哈希全一致退出 2 / DRAFT，缺项/漂移/未登记装置文件退出 1 / FAIL。
> 清单覆盖规约四份、scripts/ui-server-parity 全部文件（os.walk 递归，不跟随符号链接；出现任意符号链接、__pycache__ 或 *.pyc 即 FAIL）、exp008 共用 run.py/compare.py/enumerate_hook.mjs/preload.mjs/reconcile_routes.py 和反代 Cargo.toml/Cargo.lock/src/main.rs；check_frozen.py 与入口安全预检源码自身也在表内。共用 exp008 目录也执行同样的链接/缓存拒绝规则。

Approved-By: <待批准>
Approval-Basis: <待定标报告路径及两家评审路径>
Calibration-Evidence: <待定标>
Independent-Reviews: <待定标后登记两家评审>

f6d385e8da46ed612ba5207dcdf588c8331cfe025a866bdb59bb7e02ff32a7cc  ../../scripts/exp008/compare.py
3ea3e6f27cb55c194ccf2a456e3fe5241042726f4b0a369cc13597ee108bcaab  ../../scripts/exp008/enumerate_hook.mjs
7d4c383a249c29f371c2312671458bf996c1f2f36669ba924dcd96bfea2ef63b  ../../scripts/exp008/preload.mjs
3b48b07b6bd7430b34b64be5eba63a729bcaadbc40da44c5c5a809dd4455819a  ../../scripts/exp008/proxy/Cargo.lock
dc68cb70301ac7b8ed0bfd4d2810ebc6068c217dce5ae255576fbb1ec3cb8054  ../../scripts/exp008/proxy/Cargo.toml
f3fb65449e701d861ca6f3729a0f78fe4b4642201930f665160aa99051c514f1  ../../scripts/exp008/proxy/src/main.rs
ed5d447d3ac2e4ba03def27645f355997be981e3d82012427f14b1f1a76870c1  ../../scripts/exp008/reconcile_routes.py
9be0907a3e5a8c01050858a47c659cb42b68d1cb6ce835a91c3558658fb2476d  ../../scripts/exp008/run.py
fcc5d1501c4fedbe0e4e5d101639e7b12cc3c425c24509562249c066b7a78f86  ../../scripts/ui-server-parity/README.md
8a2358192de2fcd9afd44f56bcabbf9fba79223ab26d0485c71008b47f9f9ba0  ../../scripts/ui-server-parity/apparatus.py
f213f840dea5eef4a4fc4d507c6b2adc43307dbc3069c9895de05fa8113b4e6e  ../../scripts/ui-server-parity/benchmark_clock.py
3220d1193f47aa1dc99a2e567b5df3dfe8fa4d68354728eb82d4f648fff468b5  ../../scripts/ui-server-parity/calibrate.py
23481d6faff9884845a0c97e43b2a643ed2a0c4fcf622ec3fb514998a86a6694  ../../scripts/ui-server-parity/calibrate.sh
206000021604a6f45890b1a3f8bc919117843452ca08648267de9c851b03ecd6  ../../scripts/ui-server-parity/chat_driver.mjs
39d5be8ffb5f772052776f4aa6fddc782a1095cc94a2fa5aea2dd6fc476060fd  ../../scripts/ui-server-parity/check_coverage.py
deaac771afc0d78ccd1a6aaf892714a56c1ab6085ac5fb767fd6d5f9c4909445  ../../scripts/ui-server-parity/check_frozen.py
c0bafc737e941d7d94cf4e8258dec2a8bc110a96fe13d589d967a2bf14ebf07f  ../../scripts/ui-server-parity/check_ruler.py
22a8132e55a14d0283bc102a95c4900f9e10e2b3c210e5846475dfa37a2dda5f  ../../scripts/ui-server-parity/common.py
00604a917259fbb17094c3f41f8d2dcdec63c58db0e78e6392dcfa001c06d5a2  ../../scripts/ui-server-parity/compare.py
d072a58dae5a9b83246ab2b0ac7127b19e3426711120777c05ea51af27c8d990  ../../scripts/ui-server-parity/controls/README.md
d12ea0c4122e7a18f7f4acf59e1f083b9282d233071648fdaa20c01be1865cff  ../../scripts/ui-server-parity/controls/identity_paths.py
e56cb54148e2deb9acfae5ee2bad9fa6fbcad6f341c031db7a83deaa281bc715  ../../scripts/ui-server-parity/controls/multimodal.py
6077fd3fc4c5d22d0c094e50363319e58ac3f0b12d7b775f75d730811777291c  ../../scripts/ui-server-parity/controls/run_all.py
96d316d09437564d92e4c54425c959ced1e9c0adcbeace4d129398caa5a37c8e  ../../scripts/ui-server-parity/controls/streams_and_clocks.py
acdc238631ed73d7558d6a54ff44f3a837c3466af723e5cfc6ede78b2a5c0cb6  ../../scripts/ui-server-parity/coverage.json
7665bb29aedeced0900e68011413ca447d28a55d512eeda6122ed49b4ffdc1f8  ../../scripts/ui-server-parity/driver.mjs
9d36c543ee29884fb5c578c1d1fd14b24d8394626b5e7deb33f8f149ef22af26  ../../scripts/ui-server-parity/enumerate_routes.mjs
177dbaf124875c0bad28618cb6302284a4ce5f7e68ee2a5db4b75d9de8919972  ../../scripts/ui-server-parity/multimodal.py
de442556d012a6158e33756e760f8b6597ca9d1c7f3b432b95e4d7464de174f9  ../../scripts/ui-server-parity/policy.py
b45b2fff9623f7647adb14d9e1baa73c48da25a10fdbbba52f599313f728f06a  ../../scripts/ui-server-parity/recording.rs
f2f5548cbf475c4ea9fa71afdf35ae475fde896a70d11bab761c08e721abb123  ../../scripts/ui-server-parity/run.py
bece605c49006c2f13f5de1a5550dc82616c5f469a0d172b164a765f5457cbd6  ../../scripts/ui-server-parity/scenarios.json
4bea69ca2463c8830735470aaecffd3101de49c84f044e35636e366c9af266f7  ../../scripts/ui-server-parity/snapshot.py
b9a54f994caabe0de57f05a8e6b3f94dcb416dffc580c620d07210c63bb4f7dd  ../../scripts/ui-server-parity/summarize_ruler.py
a2b9d554afe9bb7452e20da76a26ddba2f7c859b666a2b5187c8a6d744c68048  ../../scripts/ui-server-parity/test_calibrate.py
fce6b194ea7ac70698fcc957e5064a2f51633852ed6adeb298d65529f0ac2976  ../../scripts/ui-server-parity/test_chat_driver.mjs
7078398dc27538d2cd1a94ea7fcd32af57c0894405f5c0170465f8d9ae2b94f2  ../../scripts/ui-server-parity/test_compare.py
1fe82f61978e8367d592b192ff3381470f4e8c7f7dbaf09fffd01a81bad63bad  ../../scripts/ui-server-parity/test_coverage.py
e85af38a29763e428042081cb0bf55543ca97358ef36dd1ec9c73ba84c37a1cd  ../../scripts/ui-server-parity/test_frozen.py
e3d7258ce146977553efb528e12cdfecdd2cba19415695e6ed26c830f64be7ac  ../../scripts/ui-server-parity/test_multimodal.py
c76afff11101e49f196ff9b61493ffbbc23ce3334146ec7415818caf5402b908  ../../scripts/ui-server-parity/test_recording.py
9ccf2a7fef528fdfecea33b8c9dc0c9cbd2982f09631b6fe2f3e377dca122d5e  ../../scripts/ui-server-parity/verify_auth.mjs
20dbb3d2dd8b7674f37d01c44951f29ea7e00f6360c42cbb5ae4972c4e5aab6b  acceptance.md
fbec7ad15d416c75d6be32f8e44a1393cfaacd0b4f43dde5b3ccbb547ff671b1  accepted-diffs.md
b2004ddf95e02c24b307a9c20905d76437c3eae1a251cff189dd6836e3be85bf  multimodal.md
37cd7d5f8358cdac36cf0d2bd5d5c09a73fa0d429d5225583d61abe261441b02  normalization.md
