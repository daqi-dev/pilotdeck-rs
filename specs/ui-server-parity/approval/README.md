# B2 原批准依据

此目录是当前FROZEN批准依据的可移植引用。三份批准原文与6979字节驱动日志均逐字节来自原归档，不重写历史状态；完整定标录制及两份厚ruler仍在原归档。`calibration-summary.json`的PENDING_REVIEW、independent_review=null和frozen=false是评审前现场，后续批准以FROZEN和两份原评审共同为据。Claude原文的“缺对应专项负控”P2留白仍保留，不能从本次迁移推导B2全部缺陷为0。

本次只修#113的新checkout证据缺失。现有检查器只校验批准依据存在，未新增未来内容防篡改机制；来源SHA及本次验证证明迁移时与原件相同，不宣称自动防后续篡改。原始依据仍作为历史原件保留，以下是唯一当前引用；不维护另一份改写摘要。

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
