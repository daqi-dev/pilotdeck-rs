# B2 批准依据

## 当前（2026-10-18 重新冻结，#94）

| 当前文件 | 来源 | 说明 |
|---|---|---|
| [calibration-summary.json](calibration-summary.json) | 批次 `_tmp/archive/b2-calibration-2026-10-18-a025aa97/summary.json`（仓外 checkout `/Users/da/ws/pdrs-b2cal-20261018` 录制后归档） | PENDING_REVIEW / independent_review=null / frozen=false 是评审前现场；批准以 FROZEN 与两份评审共同为据 |
| [review-glm.md](review-glm.md) | `_tmp/20261018-b2-94/freeze-glm.md` | 冻结结论：同意 |
| [review-deepseek.md](review-deepseek.md) | `_tmp/20261018-b2-94/freeze-ds2.md` | 补核后冻结结论：同意 |
| [review-deepseek-r1.md](review-deepseek-r1.md) | `_tmp/20261018-b2-94/freeze-ds.md` | DeepSeek 首轮：附件不足判「不同意」，保留以示过程，不作为批准依据 |
| [driver.log](driver.log) | `_tmp/20261018-b2-94/{calib2,calib-retry16,calib3}.log` 依次拼接 | 首跑至 ref16 REFUSED、ref16 单遍重采、续跑完成 |

厚 ruler 留归档，仓内只留路径与哈希：
- `_tmp/archive/b2-calibration-2026-10-18-a025aa97/ts-ruler-55036e36-ee90-4304-a5ca-60ef5aeead02.json` · `68799e6b2a833f0ee3d2c99f2d72dfec1d936eb5dfd2c9f35042967f072a3e60` · 22511759 bytes
- `_tmp/archive/b2-calibration-2026-10-18-a025aa97/final-ruler-333fbde2-b5d9-4b01-b29b-c2952ac87357.json` · `ef977aaeb3902b3be8704ed38ff72ffa7716b2570674a4333a64891b29a816e0` · 24043440 bytes

两批依赖覆盖对照（DeepSeek 冻结条件）：

| 项 | 10-14 | 10-18 |
|---|---|---|
| node_modules 根数 | 1594 | 1594 |
| workspace_packages | apps/desktop, packages/sdk, ui | 同 |
| link_targets | 1483 | 1483 |
| covered_roots | 4 | 4 |
| node_modules.sha256 | `b744de7b…c097` | `a619eaf9…9798` |

摘要差异来自基线 `ui/` 于 10-15 新增的 `ui/dist`（Vite 构建产物）与空目录 `ui/node_modules/.vite-temp`；apps/desktop、node_modules、packages/sdk 三根自 10-14 无改动。基线演进留痕规则另开 P2-跟进（前置于下次定标）。

## 历史（2026-10-14 冻结）

原四份原件移至 [2026-10-14/](2026-10-14/)，内容逐字节未改，下文为当时的索引原文。

### 原索引

此目录是当前FROZEN批准依据的可移植引用。三份批准原文与6979字节驱动日志均逐字节来自原归档，不重写历史状态；完整定标录制及两份厚ruler仍在原归档。`calibration-summary.json`的PENDING_REVIEW、independent_review=null和frozen=false是评审前现场，后续批准以FROZEN和两份原评审共同为据。Claude原文的“缺对应专项负控”P2留白仍保留，不能从本次迁移推导B2全部缺陷为0。

本次只修#113的新checkout证据缺失。现有冻结检查器只校验批准依据存在；独立的规则层CI另运行`scripts/check-approval-provenance.py`，按FROZEN的四条Approval-Source-SHA256检查原文身份，并跑内容变异/缺失/重复/逃逸/链接负控。它不进入53份冻结装置或capture摘要，不改评分；它信任经评审的登记，不防同时篡改登记与原文的恶意者，也不改变R5的审批身份规则。原始依据仍作为历史原件保留，以下是唯一当前引用；不维护另一份改写摘要。

| 当前文件 | 原归档位置 | SHA256 |
|---|---|---|
| [calibration-summary.json](calibration-summary.json) | `_tmp/archive/b2-calibration-2026-10-14-d3879d24-a200-45b0-afb5-331be9a29ffb/summary.json` | `5fe7f6c6dde4f2f1082cc0c986839397b2e6b42e05156a9008ffcba8c51e9c18` |
| [review-glm.md](review-glm.md) | `_tmp/review-b2-freeze2/rev-glm.md` | `108245ecb62996a9c88aa93659cfbcca6cc9e31c378dbc6983cd07da3ce3cf99` |
| [review-claude.md](review-claude.md) | `_tmp/review-b2-freeze2/rev-claude.md` | `5d3e308c3ab508f542b1eaf095465e240433ac13c62942f8b7680fcb93d7f890` |

采集日志同时可移植：

- [driver.log](driver.log) · 原归档`_tmp/archive/b2-calibration-2026-10-14-d3879d24-a200-45b0-afb5-331be9a29ffb/driver.log` · SHA256 `4efbe795b6f06d53fdc92951b40a342c1c3c55ab4607d59f848666351a8ed730` · 6979 bytes；历史日志，非本轮新采集。

两份厚ruler留原归档，仓内只留路径、哈希及说明：

- `_tmp/archive/b2-calibration-2026-10-14-d3879d24-a200-45b0-afb5-331be9a29ffb/final-ruler-72ccd4e0-a2bc-4adb-98f9-05919887e396.json` · `be1da7323fa022698127ed2eac7bc4073625865f5e41787accf7d0b2ef4a7183` · 原定标ruler或驱动日志，22834273 bytes。
- `_tmp/archive/b2-calibration-2026-10-14-d3879d24-a200-45b0-afb5-331be9a29ffb/ts-ruler-f32ed7c9-dbe1-4859-bc6e-a7f18809ccba.json` · `370404f56a4c4211cf0c0c0f7ee1dee6f65e531c265b193e4797ac1112176436` · 原定标ruler或驱动日志，21456644 bytes。
