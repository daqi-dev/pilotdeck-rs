# B2 ui/server 尺子冻结登记草稿（待定标）

> **待定标**：仅准备哈希清单。真实新装置 20 遍 TS 参考池 + 5 遍 TS 十对 + 1 遍 proxy、26 遍 clock PASS、独立双谱系评审及人批均待补。此文件不构成冻结或阶段验收。
> 定标命令及重采规则：`scripts/ui-server-parity/README.md`；不得借历史 replay 定标。
> 冻结时由协调者补定标证据与两家评审路径、替换下方审批及依据占位，再移除首行「待定标」。`Approved-By:` 接受具名人或「AI 代批，待人复核（协调者）」，并必须有 `Approval-Basis:` 记明定标报告路径和两家评审路径。冻结后变更需遵循 CLAUDE.md §五。
> 校验：`python3 -I scripts/ui-server-parity/check_frozen.py`；格式沿用 gateway：「sha256 两空格 相对本目录路径」。草稿哈希全一致退出 2 / DRAFT，缺项/漂移/未登记装置文件退出 1 / FAIL。
> 清单覆盖规约四份、scripts/ui-server-parity 与 scripts/exp008 两目录的全部 Git 跟踪文件；实际文件必须等于 Git 跟踪清单，额外/未跟踪/缺失文件、链接、__pycache__、*.pyc 一律 FAIL；仅精确名为 .DS_Store 的普通文件不登记、不执行、不参与 hash。根 rust-toolchain(.toml) 若存在也纳入清单。所有 Python 入口带 -I，装置预检源码自身先校验哈希，本目录模块按已验哈希的绝对路径载入。威胁模型与每批新构建规则见 README 和 acceptance.md；不防同时篡改装置、清单与审批的本机恶意者。

Approved-By: <待批准>
Approval-Basis: <待定标报告路径及两家评审路径>
Calibration-Evidence: <待定标>
Independent-Reviews: <待定标后登记两家评审>

f6d385e8da46ed612ba5207dcdf588c8331cfe025a866bdb59bb7e02ff32a7cc  ../../scripts/exp008/compare.py
3b8bec2dbcd28b3e99ee11017e4e37c3e59d873d2800331decf0f90a1fd25b24  ../../scripts/exp008/controls.py
803783f412eaff275249d0af1750d054b637f83dc8c4137ece5384241e099e02  ../../scripts/exp008/driver.mjs
3ea3e6f27cb55c194ccf2a456e3fe5241042726f4b0a369cc13597ee108bcaab  ../../scripts/exp008/enumerate_hook.mjs
7d4c383a249c29f371c2312671458bf996c1f2f36669ba924dcd96bfea2ef63b  ../../scripts/exp008/preload.mjs
3b48b07b6bd7430b34b64be5eba63a729bcaadbc40da44c5c5a809dd4455819a  ../../scripts/exp008/proxy/Cargo.lock
dc68cb70301ac7b8ed0bfd4d2810ebc6068c217dce5ae255576fbb1ec3cb8054  ../../scripts/exp008/proxy/Cargo.toml
f3fb65449e701d861ca6f3729a0f78fe4b4642201930f665160aa99051c514f1  ../../scripts/exp008/proxy/src/main.rs
1d94e91853632bbe963ebe61ff7c13550c7a75acbb05273b406464222a951665  ../../scripts/exp008/proxy/tests/node-ws-semantics.cjs
ed5d447d3ac2e4ba03def27645f355997be981e3d82012427f14b1f1a76870c1  ../../scripts/exp008/reconcile_routes.py
9be0907a3e5a8c01050858a47c659cb42b68d1cb6ce835a91c3558658fb2476d  ../../scripts/exp008/run.py
980130c241a926b0854c719210b8d1ccdbcf294f99bde3beb567a5bfda072d90  ../../scripts/exp008/vite.config.mjs
908843bce4155de0c04c6be4f4572cc61c44cc20df53c9f939c391417cf58b38  ../../scripts/ui-server-parity/README.md
268a6625899f6f4fa65e511b0ddbd4c690db1af85caa393ab41dde8f04b84f95  ../../scripts/ui-server-parity/apparatus.py
985515952fd382a08c9766c6bd507baf0192a8499a975d4479f62fcb7dc39711  ../../scripts/ui-server-parity/benchmark_clock.py
4fa02e24845a2e5698b071412462a930b9729047bf81e724a956cece0f4a9d3e  ../../scripts/ui-server-parity/calibrate.py
58f1641047497c8076ea93ed2119ffd7aa62601ff2360a99b79f1e55a2a91194  ../../scripts/ui-server-parity/calibrate.sh
206000021604a6f45890b1a3f8bc919117843452ca08648267de9c851b03ecd6  ../../scripts/ui-server-parity/chat_driver.mjs
1da600d1c0bf997d66564687671ee0247163ad7104da50e274877a95e886689b  ../../scripts/ui-server-parity/check_coverage.py
8c0f03b32536519990a5ff131ebdd06fd1bc2381b868c7c394ee6ccd808c0671  ../../scripts/ui-server-parity/check_frozen.py
83a0026fd43d885e857015b826242fbe2ec68706e419e1ba150d8157e44a2125  ../../scripts/ui-server-parity/check_ruler.py
e17965a4a1e12eea2d706f4c5ab51fbc45f131f22cf7c5c3661f3951a5161f6d  ../../scripts/ui-server-parity/common.py
d48505eb0a5723bc794e74a99448530cf374ef327d804c2cbf90bd2fe068f8ba  ../../scripts/ui-server-parity/compare.py
f785d81c732e18a4607eae11a845a1e7015db831150f0fc89a72f0580ebe89f9  ../../scripts/ui-server-parity/controls/README.md
0399a28bae1fed9a52593d981959de8380996f5ebe4b3f4854bde5622a0506da  ../../scripts/ui-server-parity/controls/identity_paths.py
4b5c5be5d20dff8f3309d52baf6849256db48006c6e5c2817acf4857004a3376  ../../scripts/ui-server-parity/controls/multimodal.py
a1de16ba7dc08e239ec1f4fb08ccf4e327ac5e0efd48e0df4d967ded5f7a1b74  ../../scripts/ui-server-parity/controls/run_all.py
931bb0975fc6341c97889514d8901dcf666160deb061e3a3bdde31dc7f7c8b3f  ../../scripts/ui-server-parity/controls/streams_and_clocks.py
acdc238631ed73d7558d6a54ff44f3a837c3466af723e5cfc6ede78b2a5c0cb6  ../../scripts/ui-server-parity/coverage.json
7665bb29aedeced0900e68011413ca447d28a55d512eeda6122ed49b4ffdc1f8  ../../scripts/ui-server-parity/driver.mjs
9d36c543ee29884fb5c578c1d1fd14b24d8394626b5e7deb33f8f149ef22af26  ../../scripts/ui-server-parity/enumerate_routes.mjs
d86ed3727dcc4b50902add4a1cd49da4e93305776eceaacc8049247ac2d1330b  ../../scripts/ui-server-parity/multimodal.py
83cbac908c33a58b46d015965950fb020d6489b7c3d76102f7f40242e42f09da  ../../scripts/ui-server-parity/policy.py
b45b2fff9623f7647adb14d9e1baa73c48da25a10fdbbba52f599313f728f06a  ../../scripts/ui-server-parity/recording.rs
93d3041da65be8c1cc9ea311e38bbc40083967269b778c2ca5a4caac421cba18  ../../scripts/ui-server-parity/run.py
bece605c49006c2f13f5de1a5550dc82616c5f469a0d172b164a765f5457cbd6  ../../scripts/ui-server-parity/scenarios.json
f74dd7c3acfb8a3c44c750772f98b9af93662539ce65dbe9aea7297ce638d389  ../../scripts/ui-server-parity/snapshot.py
03759b276dd861191b7372fb85ede5277677a6bc5db12901cb64e0d81d9f54ce  ../../scripts/ui-server-parity/summarize_ruler.py
bb1db10e125d9731494b2a14a3dc243e14d3a3e680573d0ed733521357b64f3d  ../../scripts/ui-server-parity/test_calibrate.py
fce6b194ea7ac70698fcc957e5064a2f51633852ed6adeb298d65529f0ac2976  ../../scripts/ui-server-parity/test_chat_driver.mjs
fc253da9424d62f331749d46d1854f01aff8f3f572df1f1a9cfecc2f15d4eae3  ../../scripts/ui-server-parity/test_compare.py
bb0911486110cae9fd91394a8f01c37613b73ba42bf38c8649f8db3fc1bf7957  ../../scripts/ui-server-parity/test_coverage.py
8cfc8ea18906981b14dcf5e1b757fecdbee6e38aa045e3c10fb38db8760adc56  ../../scripts/ui-server-parity/test_frozen.py
75d9418fa858c51ca9cd1bae960137be53edfe94d3e2b4f41413a6f4741a29ac  ../../scripts/ui-server-parity/test_multimodal.py
8568dbaffd2b20c070a20a773ab4fe73110d01def63f6364f6a007985c0fcca9  ../../scripts/ui-server-parity/test_recording.py
9ccf2a7fef528fdfecea33b8c9dc0c9cbd2982f09631b6fe2f3e377dca122d5e  ../../scripts/ui-server-parity/verify_auth.mjs
576ca926269827c0afd330624f2cf6223676a5657570ca1648290c8f0ce87408  ../../specs/ui-server-parity/acceptance.md
fbec7ad15d416c75d6be32f8e44a1393cfaacd0b4f43dde5b3ccbb547ff671b1  ../../specs/ui-server-parity/accepted-diffs.md
b2004ddf95e02c24b307a9c20905d76437c3eae1a251cff189dd6836e3be85bf  ../../specs/ui-server-parity/multimodal.md
37cd7d5f8358cdac36cf0d2bd5d5c09a73fa0d429d5225583d61abe261441b02  ../../specs/ui-server-parity/normalization.md
a1cf65d93b72351eaf5fce8c5356bfaa3b40f56f401d41151123b78582412fc4  ../../rust-toolchain.toml
