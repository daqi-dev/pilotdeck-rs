---
id: A4-MODEL-CATALOG-CWD-001
---

## 类别

不可避免（本次限定改动内的既有结构限制）：Rust `Metadata` 必须持有实际 cwd；无 cwd 时仍无法在本地处理绝对 home 的 catalog。此条登记现状，不认可该限制为最终行为。

## 基线依据

固定基线 cc7c4e6 的 `src/gateway/dialog/modelCatalog.ts:17` 调 `loadPilotConfig({env})`，`src/pilot/config/loadPilotConfig.ts:68` 经 `src/pilot/paths.ts:20` 解析 home。Node v22.22.0 实跑原 `listModelCatalog` 及 `GatewayWsConnection.handleRequest`：cwd 删除、PILOT_HOME 为绝对路径 / `~` / `~/nested/../home` / 未设（HOME 为隔离绝对路径）时，均读取隔离配置并成功返回；两次入口累计 cwdReads=0。相对/空 PILOT_HOME 的删除 cwd 场景则返回 ENOENT / uv_cwd。

持久金样与生成器：`src/pilotdeck-gateway/tests/fixtures/model-catalog-cwd/{oracle.json,generate.mts}`，含源码 SHA256。原始输出：本任务证据 `_tmp/20261026-issue-755/node-gateway-probe.log`（协调者归档前位于 wt-755）。

## Rust 行为

`src/pilotdeck-gateway/src/model_catalog.rs::native_metadata` 在健康 cwd 下照旧返回本地 Metadata。cwd 失败且 home 已绝对解析时，保留 `Ok(None)`；`dispatch.rs::route_catalog` / `route_with_metadata` 保留 Forward，不伪造 cwd，也不新增本地错误。因此没有把 TS 可成功请求改成本地失败，但本地能力仍有缺口。

这是修改前 `current_dir().ok()?` 已有的转发边界。本次只改变必须依赖 cwd 的相对/空 home 失败：传播 ENOENT 与 uv_cwd 消息，直接调用点以相同 code/message 响应。该部分由 TS 真实错误帧校验，不属于本差异。

## 范围

仅 cwd 不可取得、home 已能绝对解析、且调用未提供 Metadata 的 native catalog / 共用 native_metadata 的 session_model_get 适配路径。已注入 Metadata、健康 cwd、缺 HOME 的既有 host fallback 均不扩改。TS 实测针对 catalog；session_model_get 在本包仅验证共享入口适配，不声称其完整生产调用链已对拍。

## 机器判据

`model_catalog::cwd_tests::{absolute_deleted_cwd,tilde_deleted_cwd,tilde_child_deleted_cwd,unset_deleted_cwd}` 显式要求保留 Forward；对应 TS 金样 frame 成功。`absolute-home-error` 负控必须因 `native metadata matrix: <case>` 转红，恢复后通过。脚本位于同 fixture 的 `negative_controls.py`，仅允许默认 HEAD 且调用者设置 CARGO_TARGET_DIR；提交后由协调者在宿主跑。

生成器独占写 oracle；ignored `model_catalog_cwd_regenerates_identically` 验证字节重生成。最终冻结账本检查与测试证据见该任务 RESULT.md。

## 批准记录

未批准，非已接受差异；不增加判分豁免。前置于 #755 所指 A7 首个经此入口使用相对路径的生产调用，本条不得用作本地能力已完成的证明。
