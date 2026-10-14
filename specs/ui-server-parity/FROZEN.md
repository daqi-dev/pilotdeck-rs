# B2 ui/server 尺子冻结登记

> **2026-10-14 重新冻结**（AI 代批，待人复核）。PR #93（#59 路由审批门与白名单、#80 反代 WS 四项、#88/#89 P2）改动装置后重新定标：main eb2f2c7，仓外 detached checkout，同一 UTC 日 20 遍 TS 参考池 + 5 遍 TS 计分 + 1 遍 proxy；TS 十对 10/10、proxy PASS（7 个 WS 握手无 Date、下行 close 各一）、26 遍时钟 PASS、无重采 / REFUSED / 近似测量。两家评审（GLM、Claude）均「无 P0/P1，可冻结」。上一版冻结（PR #90，批次 7d28f0de）随装置变更失效。Rust 路由豁免审批须为 human-approvers.txt 白名单身份（与本冻结记录的代批规则不同，见 acceptance.md）。
> 定标命令及重采规则：`scripts/ui-server-parity/README.md`；不得借历史 replay 定标。
> 冻结时由协调者补定标证据与两家评审路径、替换下方审批及依据占位，再移除首行「待定标」。`Approved-By:` 接受具名人或「AI 代批，待人复核（协调者）」，并必须有 `Approval-Basis:` 记明定标报告路径和两家评审路径。冻结后变更需遵循 CLAUDE.md §五。
> 校验：`python3 -I scripts/ui-server-parity/check_frozen.py`；格式沿用 gateway：「sha256 两空格 相对本目录路径」。草稿哈希全一致退出 2 / DRAFT，缺项/漂移/未登记装置文件退出 1 / FAIL。
> 清单覆盖规约四份及 rust-routes.json、human-approvers.txt（路由审批白名单；改动属装置变更，须评审）、scripts/ui-server-parity 与 scripts/exp008 两目录的全部 Git 跟踪文件；实际文件必须等于 Git 跟踪清单，额外/未跟踪/缺失文件、链接、__pycache__、*.pyc 一律 FAIL；仅精确名为 .DS_Store 的普通文件不登记、不执行、不参与 hash。根 rust-toolchain(.toml) 若存在也纳入清单。所有 Python 入口带 -I，装置预检源码自身先校验哈希，本目录模块按已验哈希的绝对路径载入。威胁模型与每批新构建规则见 README 和 acceptance.md；不防同时篡改装置、清单与审批的本机恶意者。

Approved-By: AI 代批，待人复核（协调者）
Approval-Basis: _tmp/archive/b2-calibration-2026-10-14-d3879d24-a200-45b0-afb5-331be9a29ffb/summary.json; _tmp/review-b2-freeze2/rev-glm.md; _tmp/review-b2-freeze2/rev-claude.md
Calibration-Evidence: _tmp/archive/b2-calibration-2026-10-14-d3879d24-a200-45b0-afb5-331be9a29ffb/summary.json; _tmp/archive/b2-calibration-2026-10-14-d3879d24-a200-45b0-afb5-331be9a29ffb/final-ruler-72ccd4e0-a2bc-4adb-98f9-05919887e396.json; _tmp/archive/b2-calibration-2026-10-14-d3879d24-a200-45b0-afb5-331be9a29ffb/ts-ruler-f32ed7c9-dbe1-4859-bc6e-a7f18809ccba.json; _tmp/archive/b2-calibration-2026-10-14-d3879d24-a200-45b0-afb5-331be9a29ffb/driver.log
Independent-Reviews: _tmp/review-b2-freeze2/rev-glm.md; _tmp/review-b2-freeze2/rev-claude.md

f6d385e8da46ed612ba5207dcdf588c8331cfe025a866bdb59bb7e02ff32a7cc  ../../scripts/exp008/compare.py
3b8bec2dbcd28b3e99ee11017e4e37c3e59d873d2800331decf0f90a1fd25b24  ../../scripts/exp008/controls.py
803783f412eaff275249d0af1750d054b637f83dc8c4137ece5384241e099e02  ../../scripts/exp008/driver.mjs
3ea3e6f27cb55c194ccf2a456e3fe5241042726f4b0a369cc13597ee108bcaab  ../../scripts/exp008/enumerate_hook.mjs
7d4c383a249c29f371c2312671458bf996c1f2f36669ba924dcd96bfea2ef63b  ../../scripts/exp008/preload.mjs
3ef27b94af7d6bdfc8b90c4f0ccead09f0f97422d3fa9f3d7dab3e4e146fe605  ../../scripts/exp008/proxy/Cargo.lock
56a68e9239f0701c46a6b42ef3316b2ac3ff9b238bed2b086a4f8f297d64609f  ../../scripts/exp008/proxy/Cargo.toml
abde68d203a8c76330668554b3606a10a94c1e4a145effcb3cbfc856556aadce  ../../scripts/exp008/proxy/src/main.rs
89e62305ff30e49caefd8f649657b6aa1a9c968700e37509a705330ec21d8224  ../../scripts/exp008/proxy/tests/node-ws-semantics.cjs
ed5d447d3ac2e4ba03def27645f355997be981e3d82012427f14b1f1a76870c1  ../../scripts/exp008/reconcile_routes.py
a60f04b6ce5cccab4c40c75b0a6d8204344977bde85d4931dde213b18ddd126e  ../../scripts/exp008/run.py
980130c241a926b0854c719210b8d1ccdbcf294f99bde3beb567a5bfda072d90  ../../scripts/exp008/vite.config.mjs
a57f3d6b8a942b16ac17cebbac6b7564d236699b5f84fb9b17e6690f6805d5d8  ../../scripts/ui-server-parity/README.md
34d08991149d39843bc6a2133359cc05a288a6e4e2a162737817b03928fadd2f  ../../scripts/ui-server-parity/apparatus.py
985515952fd382a08c9766c6bd507baf0192a8499a975d4479f62fcb7dc39711  ../../scripts/ui-server-parity/benchmark_clock.py
bc2a16f7c50431f1b6588c394184d87a0258e719c4605f4f0c000525839b421e  ../../scripts/ui-server-parity/calibrate.py
58f1641047497c8076ea93ed2119ffd7aa62601ff2360a99b79f1e55a2a91194  ../../scripts/ui-server-parity/calibrate.sh
206000021604a6f45890b1a3f8bc919117843452ca08648267de9c851b03ecd6  ../../scripts/ui-server-parity/chat_driver.mjs
88b788b631e956f9b4fd3219bbfd53a5e5fd52215c1d51233da9cb3fc0442200  ../../scripts/ui-server-parity/check_coverage.py
517ef5940a65abcff8c193ba77b3bff11b92750ac7d0db86e63f3043e91bd48a  ../../scripts/ui-server-parity/check_frozen.py
83a0026fd43d885e857015b826242fbe2ec68706e419e1ba150d8157e44a2125  ../../scripts/ui-server-parity/check_ruler.py
84df4021c035a1af4426b175dc6634a7db1d31f2172d7d8c5f24991b8c3692f0  ../../scripts/ui-server-parity/common.py
e09b8199597d3a4b9751a056b168d7e9272a5c3e1d18053e56590c10642e8ea7  ../../scripts/ui-server-parity/compare.py
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
959951479084cb739b5136f723575cdbc1f5a01f6ec12536c9c8c9428a0dfe65  ../../scripts/ui-server-parity/run.py
bece605c49006c2f13f5de1a5550dc82616c5f469a0d172b164a765f5457cbd6  ../../scripts/ui-server-parity/scenarios.json
f74dd7c3acfb8a3c44c750772f98b9af93662539ce65dbe9aea7297ce638d389  ../../scripts/ui-server-parity/snapshot.py
03759b276dd861191b7372fb85ede5277677a6bc5db12901cb64e0d81d9f54ce  ../../scripts/ui-server-parity/summarize_ruler.py
13fabe1a72f3e1c156005624532127abe3f878baaadf8a8ff9cb1ef34db093d0  ../../scripts/ui-server-parity/test_calibrate.py
fce6b194ea7ac70698fcc957e5064a2f51633852ed6adeb298d65529f0ac2976  ../../scripts/ui-server-parity/test_chat_driver.mjs
bdd5bc5f85ac703a8fca8b4221f5c06f56efd94c02c47dd118854485e02f048b  ../../scripts/ui-server-parity/test_compare.py
43d6a28e6c97ea525769b4faee0c03d934e887175813d97eadcad25f0b3cb1e0  ../../scripts/ui-server-parity/test_coverage.py
ac4cd82e1076e8e19ef35b6e65e93bf090eb3961b1022b9e50a672b6cd91a1a6  ../../scripts/ui-server-parity/test_frozen.py
75d9418fa858c51ca9cd1bae960137be53edfe94d3e2b4f41413a6f4741a29ac  ../../scripts/ui-server-parity/test_multimodal.py
8568dbaffd2b20c070a20a773ab4fe73110d01def63f6364f6a007985c0fcca9  ../../scripts/ui-server-parity/test_recording.py
9ccf2a7fef528fdfecea33b8c9dc0c9cbd2982f09631b6fe2f3e377dca122d5e  ../../scripts/ui-server-parity/verify_auth.mjs
1da2613ac4ac554550b0d46e4f05c2d71085595f807f9f46f6b6574a1755ac78  ../../specs/ui-server-parity/acceptance.md
f8d552dcebfc6bc702aaac6ecc546c7969a83fa7ea178e6a8ea8b1a5ce272484  ../../specs/ui-server-parity/accepted-diffs.md
b2004ddf95e02c24b307a9c20905d76437c3eae1a251cff189dd6836e3be85bf  ../../specs/ui-server-parity/multimodal.md
37cd7d5f8358cdac36cf0d2bd5d5c09a73fa0d429d5225583d61abe261441b02  ../../specs/ui-server-parity/normalization.md
a1cf65d93b72351eaf5fce8c5356bfaa3b40f56f401d41151123b78582412fc4  ../../rust-toolchain.toml
7670c4a531150c7886d224ecdebb9eca3fa7724ac91ac8632cc98446d501964f  ../../specs/ui-server-parity/rust-routes.json
0365b045f3ec15853726a26ccd7b4b6f54993c3d52d6c667f9b6e81341121a65  ../../specs/ui-server-parity/human-approvers.txt
