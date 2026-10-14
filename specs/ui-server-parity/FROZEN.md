# B2 ui/server 尺子冻结登记草稿（待定标）

> **待定标**：仅准备哈希清单。真实新装置 20 遍 TS 参考池 + 5 遍 TS 十对 + 1 遍 proxy、26 遍 clock PASS、独立双谱系评审及人批均待补。此文件不构成冻结或阶段验收。
> 定标命令及重采规则：`scripts/ui-server-parity/README.md`；不得借历史 replay 定标。
> 冻结时由协调者补定标证据与评审路径、替换下方具名人批占位，再移除首行「待定标」。冻结后变更需遵循 CLAUDE.md §五。
> 校验：`python3 scripts/ui-server-parity/check_frozen.py`；格式沿用 gateway：「sha256 两空格 相对本目录路径」。草稿哈希全一致退出 2 / DRAFT，缺项/漂移/未登记装置文件退出 1 / FAIL。
> 清单覆盖规约四份、scripts/ui-server-parity 全部文件（递归，排除 __pycache__/及 *.pyc）、反代 Cargo.toml/Cargo.lock/src/main.rs；check_frozen.py 自身也在表内。

Human-Approved-By: <待人批准>
Calibration-Evidence: <待定标>
Independent-Reviews: <待定标后登记两家评审>

20dbb3d2dd8b7674f37d01c44951f29ea7e00f6360c42cbb5ae4972c4e5aab6b  acceptance.md
fbec7ad15d416c75d6be32f8e44a1393cfaacd0b4f43dde5b3ccbb547ff671b1  accepted-diffs.md
b2004ddf95e02c24b307a9c20905d76437c3eae1a251cff189dd6836e3be85bf  multimodal.md
9875b5b0fecc81189a2941722d28d83ccfae260cb876358eba7d8bd6f253275d  normalization.md
3b48b07b6bd7430b34b64be5eba63a729bcaadbc40da44c5c5a809dd4455819a  ../../scripts/exp008/proxy/Cargo.lock
dc68cb70301ac7b8ed0bfd4d2810ebc6068c217dce5ae255576fbb1ec3cb8054  ../../scripts/exp008/proxy/Cargo.toml
f3fb65449e701d861ca6f3729a0f78fe4b4642201930f665160aa99051c514f1  ../../scripts/exp008/proxy/src/main.rs
0d2e1e76846b001507b514c2be96e5eead050036acec69005216cb956c4c5dbd  ../../scripts/ui-server-parity/README.md
4487f869d5c591502df44be6840a52aed9be23924fe0c7fda201c63ffa516576  ../../scripts/ui-server-parity/benchmark_clock.py
0a92719f0e1fdfa2d3e58a3a8079bf1068c6f917696ea21e7affec9155b7ec39  ../../scripts/ui-server-parity/calibrate.py
23481d6faff9884845a0c97e43b2a643ed2a0c4fcf622ec3fb514998a86a6694  ../../scripts/ui-server-parity/calibrate.sh
206000021604a6f45890b1a3f8bc919117843452ca08648267de9c851b03ecd6  ../../scripts/ui-server-parity/chat_driver.mjs
98e2f73189a29a3e2e8ed2f2f564f917cc0a2826ec8f740307d793ef3c722da7  ../../scripts/ui-server-parity/check_coverage.py
f03488a5ffc08f8d54d9c6948628cea47f4232d0eb12f8e5c387add3f881aff8  ../../scripts/ui-server-parity/check_frozen.py
c3713ba3dd9e459ea93be9eac4ab25a8b0fbc01da7dc0837a2722a4eac852abd  ../../scripts/ui-server-parity/check_ruler.py
4b27c355d0834db8b6aca5183805663fcb445ee2cbaa42a48ad983b622c2f135  ../../scripts/ui-server-parity/common.py
5c0afc37372570ad6d6c47f9104b703c838d84dfb2da93cbe1c622f340257b2f  ../../scripts/ui-server-parity/compare.py
d072a58dae5a9b83246ab2b0ac7127b19e3426711120777c05ea51af27c8d990  ../../scripts/ui-server-parity/controls/README.md
b532d0fe912894ca7cc3aff701bced13abdc527895b2a71e0994031fa352253b  ../../scripts/ui-server-parity/controls/identity_paths.py
99274aa0ebaef71cb3865af4358673934753080e9dadcfcdb58f633f5e7e9ea1  ../../scripts/ui-server-parity/controls/multimodal.py
6e3aeee8be6f26345af7f12189a2c90280131854f2faec16ca0502e8ba7dae7f  ../../scripts/ui-server-parity/controls/run_all.py
62772e9896e6815486bc609b7c5e17125440775dd698dee10cd5f26e280c712f  ../../scripts/ui-server-parity/controls/streams_and_clocks.py
acdc238631ed73d7558d6a54ff44f3a837c3466af723e5cfc6ede78b2a5c0cb6  ../../scripts/ui-server-parity/coverage.json
7665bb29aedeced0900e68011413ca447d28a55d512eeda6122ed49b4ffdc1f8  ../../scripts/ui-server-parity/driver.mjs
9d36c543ee29884fb5c578c1d1fd14b24d8394626b5e7deb33f8f149ef22af26  ../../scripts/ui-server-parity/enumerate_routes.mjs
053ce088dd7f85301ef0dee50ad20382eef50c2ffb9d93ff6cf4ea2e525a2ab1  ../../scripts/ui-server-parity/multimodal.py
ce1cc991e7d6588a67711af6b81ee5e3c44aa2981971c7bcedd97fa9095f4906  ../../scripts/ui-server-parity/policy.py
b45b2fff9623f7647adb14d9e1baa73c48da25a10fdbbba52f599313f728f06a  ../../scripts/ui-server-parity/recording.rs
db163bbd0c763425e523ece62b45d68ac77e700906c4913761ee626150e59ae0  ../../scripts/ui-server-parity/run.py
bece605c49006c2f13f5de1a5550dc82616c5f469a0d172b164a765f5457cbd6  ../../scripts/ui-server-parity/scenarios.json
9bae4c0023e35e27fd1f2490ee35595712b5bbbbc8e552ab468777bbceed5903  ../../scripts/ui-server-parity/snapshot.py
793530d71e91aaec31e48312de7aedffd5be67095efa4df8a5bd536a8c467f25  ../../scripts/ui-server-parity/summarize_ruler.py
4f517c2adf41fb32a9a085b7ca874a33bbd58fa06d418ac40035d80fe2299387  ../../scripts/ui-server-parity/test_calibrate.py
fce6b194ea7ac70698fcc957e5064a2f51633852ed6adeb298d65529f0ac2976  ../../scripts/ui-server-parity/test_chat_driver.mjs
bf8f3dc2c4d5519b7767c53faae04294b28b2857f8a308df058b138cb1f56f23  ../../scripts/ui-server-parity/test_compare.py
9ec4ee5d8a89beb5b672286623334be704485581f43adf2eb097fe03c98b109f  ../../scripts/ui-server-parity/test_coverage.py
33b4ea7a4b1670a2be0fd3d540ecb5f0a888ab09e636f2d9441c5c00241dc039  ../../scripts/ui-server-parity/test_frozen.py
f6410a65d6fc4c84b74b89216272139afb4aa91b858ac2c0a26573cfef2edad6  ../../scripts/ui-server-parity/test_multimodal.py
74a26132db348a51641073d9c6aefdd9306f725cca93297bf134f52b9d3cb04f  ../../scripts/ui-server-parity/test_recording.py
9ccf2a7fef528fdfecea33b8c9dc0c9cbd2982f09631b6fe2f3e377dca122d5e  ../../scripts/ui-server-parity/verify_auth.mjs
