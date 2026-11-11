#!/usr/bin/env python3
"""check-ts-anchors.py — `GatewayWsConnection.ts:` 数值注释锚点机检（#2795）。

TS 基线（resources/PilotDeck，不入库）不动之后，仓内 Rust 注释里引用它的行号锚点会静默
漂移：TS 侧删几行，`GatewayWsConnection.ts:215-255` 就指向了别的分支——#2795 清点出 cron
族五处（连同正文外同形的 manager/projection、cron 六路由四处）共 13 处存量漂移。本检查器
把「清零」变成可机检的状态，防再漂移。

两层判据：

1. **引用名册（默认模式，不需要 TS 树）**：`REGISTERED_CITATIONS` 是入库的行号表——
   扫 `src/**/*.rs` 里所有 `GatewayWsConnection.ts:<span>` 引用（含跨注释行的续行、
   `187,236-253` / `187 and 236-253` / `405-425, 523-543` 连写、`542–557` 全角横线），
   逐文件与名册对账：
   - 引用不在名册（新引用或被改过的锚点）→ 判红：改锚点必须连带改名册，而改名册正是
     「停下来重新对 TS 实读一次」的强制点；
   - 名册条目在文件里已找不到（注释被删/改）→ 判红：名册不许留死条目。
   同一注释块内不带文件前缀的 `(lines 194-222)` 子锚点不在名册口径内（仓里 `(lines …)`
   也用于其它 TS 文件，无法按文件归因）；它们的宿主锚点受名册管，其数值本身由第 2 层守。

2. **TS 实读（`--ts-baseline <path>` 或环境变量 `PDRS_TS_BASELINE`，fail-closed）**：
   `TS_ANCHORS` 是「锚点名 → 行号 → 该行必须含的签名」表。给出基线路径就逐条实读
   GatewayWsConnection.ts 核对；路径给了但文件找不到/读不了 → 退 2，不静默跳过。
   没给路径时只跑第 1 层（CI 里没有 resources/ 基线，见 .gitignore）。
   基线钉在 schema-FROZEN.md 的 cc7c4e65（与 CI 两处浅克隆同一冻结 commit）。

用法：
  python3 -I scripts/check-ts-anchors.py
  python3 -I scripts/check-ts-anchors.py --ts-baseline /path/to/PilotDeck
  python3 -I scripts/check-ts-anchors.py --self-check   # 机检自身的变异负控

退出码：0 通过 · 1 判红（锚点漂移 / 名册失配）· 2 前置错误（基线路径失效等，fail-closed）。
"""

from __future__ import annotations

import argparse
import os
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCAN_ROOT = ROOT / "src"
TS_RELATIVE = "src/gateway/server/GatewayWsConnection.ts"

# ── 第 1 层：引用名册（#2795 清点后入库的行号表）─────────────────────────────
# 键 = 仓库相对路径；值 = 该文件里允许出现的规范化 span 集合。
# 规范化：空白剔除、`–`→`-`、`and`/`,`/`;` 分隔统一成 `,`。
# 改动任何 Rust 注释里的 GatewayWsConnection.ts 行号，都必须同步改这张表，
# 并用 --ts-baseline 对真实 TS 基线重核一次。
REGISTERED_CITATIONS: dict[str, set[str]] = {
    "src/pilotdeck-gateway/src/native_process/assembly/cron/cron_wiring_tests.rs": {"245-255"},
    "src/pilotdeck-gateway/src/native_process/assembly/user_dialog.rs": {"481-490"},
    "src/pilotdeck-gateway/src/native_process/capabilities.rs": {"112-115"},
    "src/pilotdeck-gateway/src/native_process/management.rs": {"223-255"},
    "src/pilotdeck-gateway/src/native_process/methods/always_on.rs": {"186-187", "246-254"},
    "src/pilotdeck-gateway/src/native_process/methods/cron.rs": {"194-255", "464-475"},
    "src/pilotdeck-gateway/src/native_process/methods/cron/cron_tests.rs": {"223-255"},
    "src/pilotdeck-gateway/src/native_process/methods/mcp.rs": {"387-398"},
    "src/pilotdeck-gateway/src/native_process/methods/settings.rs": {"405-425,523-543"},
    # session_dir.rs 的 :252 由 main 侧 #2199 fix-r3 新增（本批基线 9be18ab83 之后），
    # rebase 集成时收货侧对冻结基线 cc7c4e65 实读核正后补登：
    # :252 = `message: error instanceof Error ? error.message : String(error),`
    "src/pilotdeck-gateway/src/native_process/methods/session_dir.rs": {"252"},
    "src/pilotdeck-gateway/src/native_process/methods/sop.rs": {"332", "630-637"},
    "src/pilotdeck-gateway/src/native_process/methods/tests.rs": {"481-490", "528-536"},
    "src/pilotdeck-gateway/src/native_process/methods/user_dialog.rs": {"481"},
    "src/pilotdeck-gateway/src/native_process/methods/weixin_login.rs": {"528-536"},
    "src/pilotdeck-gateway/src/node_io.rs": {"367-368", "542-557"},
    "src/pilotdeck-gateway/src/runtime/connection/mod.rs": {"176-185"},
    "src/pilotdeck-gateway/src/runtime/connection/tests.rs": {"49-58", "138-171"},
    "src/pilotdeck-gateway/src/runtime/cron/manager_tests.rs": {"187,223-255", "464-475"},
    "src/pilotdeck-gateway/src/runtime/cron/methods.rs": {"464-475"},
    "src/pilotdeck-gateway/src/runtime/cron/projection.rs": {
        "187,223-255",
        "223-243,245-255",
        "223-255",
        "464-475",
    },
    "src/pilotdeck-gateway/src/runtime/manager/projection.rs": {"223-255", "249-253", "309-314"},
    "src/pilotdeck-gateway/src/runtime/mcp/gateway.rs": {"387-398"},
    "src/pilotdeck-gateway/src/runtime/memory/gateway.rs": {"223-255"},
    "src/pilotdeck-gateway/src/runtime/settings/gateway.rs": {"414", "405-425,523-543"},
    "src/pilotdeck-gateway/src/runtime/snapshot.rs": {"223-255"},
    "src/pilotdeck-gateway/src/session_model/replay.rs": {"251"},
    "src/pilotdeck-gateway/src/skills/binding.rs": {"542-557"},
    "src/pilotdeck-gateway/src/skills/mod.rs": {"542-557"},
    "src/pilotdeck-gateway/src/user_dialog/bus.rs": {"481-490", "490"},
}

# ── 第 2 层：TS 语义锚点（名字 → 行号 → 该行必含的签名）────────────────────
# 基线：cc7c4e65 的 src/gateway/server/GatewayWsConnection.ts（641 行）。
# 注册名册里出现的每一个 span，都必须能落回这些锚点标定的行号。
TS_ANCHORS: list[tuple[str, int, str]] = [
    ("constructor_message_hook", 44, "ws.onMessage"),
    ("close_set_union", 49, "new Set([...this.inFlightSessions, ...this.interactionSessions])"),
    ("abort_turn_cleanup", 58, "abortTurn"),
    ("hello_ok_type", 112, 'type: "hello_ok"'),
    ("hello_server_info", 115, "serverInfo: await this.options.gateway.describeServer()"),
    ("submit_session_registration", 126, "this.inFlightSessions.add(sessionKey)"),
    ("event_loop_start", 138, 'event.type === "turn_completed"'),
    ("terminal_event_selection", 157, "lastError ?? lastCompleted"),
    ("terminal_frame_end", 171, ");"),
    ("permission_resolved_block", 176, 'frame.method === "permission_decide"'),
    ("success_envelope", 187, 'type: "response", id: frame.id, ok: true, result'),
    ("catch_all", 188, "} catch (error) {"),
    ("skill_validation_arm", 194, "instanceof SkillValidationError"),
    ("skill_manager_arm", 209, "instanceof SkillManagerError"),
    ("structured_arm", 223, "instanceof DialogGatewayError || hasStructuredErrorCode"),
    ("fallback_send_text", 245, "this.ws.sendText("),
    ("fallback_json_arg", 246, "JSON.stringify({"),
    ("fallback_ok_false", 249, "ok: false,"),
    ("fallback_request_failed_code", 251, '"gateway_request_failed"'),
    ("fallback_error_object_close", 253, "},"),
    ("fallback_end", 255, ");"),
    ("handle_request_dispatch_call", 175, "await this.dispatchRequest(frame);"),
    ("manager_sessions_case", 309, 'case "manager_sessions":'),
    ("model_catalog_case", 367, 'case "model_catalog_list":'),
    ("sop_resume_guard", 332, "resumeSop"),
    ("mcp_group_start", 387, 'case "set_mcp_servers":'),
    ("mcp_group_end", 398, "set_mcp_permission_mode_override"),
    ("apply_flag_settings_case", 405, 'case "apply_flag_settings":'),
    ("set_session_thinking_case", 414, 'case "set_session_thinking":'),
    ("reload_output_styles_end", 425, "reload_output_styles is unavailable"),
    ("cron_cases_start", 464, 'case "cron_create":'),
    ("cron_cases_end", 475, "cronRunNow"),
    ("user_dialog_list_case", 481, 'case "user_dialog_list":'),
    ("user_dialog_respond_case", 490, 'case "user_dialog_respond":'),
    ("reload_config_case", 523, 'case "reload_config":'),
    ("weixin_login_case", 528, 'case "prepare_weixin_login":'),
    ("skill_list_case", 543, "skillsList"),
    ("skill_cases_start", 542, 'case "skill_list":'),
    ("skill_cases_end", 557, "skillScan"),
    ("is_request_frame", 630, "function isRequestFrame"),
    ("describe_project_case", 521, 'case "describe_project":'),
]

# 引用语法：`GatewayWsConnection.ts:` 后跟一个或多个 span。
# span 之间用 `,` / `;` / `and` 分隔；span 本身是 `N` 或 `N-M`（`–` 全角横线同认）。
# 允许三种跨行形态（都有仓内实例）：
#   - 冒号在行尾、span 在下一注释行（connection/mod.rs、connection/tests.rs）；
#   - `249-` 在行尾、span 终点在下一注释行（manager/projection.rs）；
#   - 分隔符后的数字在下一注释行（暂无实例，按同形处理）。
_LINE_BREAK = r"(?:\r?\n[ \t]*(?://+[ \t]*)?)"
_NUMBER = r"\d+"
_SPAN = rf"{_NUMBER}[ \t]*(?:-|–)[ \t]*{_LINE_BREAK}?{_NUMBER}"
_CITATION_RE = re.compile(
    rf"GatewayWsConnection\.ts:[ \t]*{_LINE_BREAK}?{_NUMBER}(?:[ \t]*(?:-|–)[ \t]*{_LINE_BREAK}?{_NUMBER})?"
    rf"(?:(?:[ \t]*(?:,|;)[ \t]*|[ \t]+and[ \t]+){_LINE_BREAK}?{_NUMBER}(?:[ \t]*(?:-|–)[ \t]*{_LINE_BREAK}?{_NUMBER})?)*"
)


def normalize_span(raw: str) -> str:
    """把引用原文规范成名册键：去注释前缀与空白、`–`→`-`、`and`/`;` 统一成 `,`。

    raw 里只可能有数字、`-`/`–`、分隔符（`,` `;` `and`）、空白和跨行注释前缀
    （`//` / `///`）——后两者不是 span 的一部分，剥掉再压实。
    """
    compact = raw.replace("–", "-")
    compact = compact.replace("and", ",").replace(";", ",")
    compact = re.sub(r"[/\s]+", "", compact)
    return re.sub(r",+", ",", compact)


def extract_citations(text: str) -> list[tuple[int, str]]:
    """返回 [(Rust 行号, 规范化 span)]。同一处多次出现逐条列出。"""
    found: list[tuple[int, str]] = []
    for match in _CITATION_RE.finditer(text):
        found.append((text.count("\n", 0, match.start()) + 1, normalize_span(match.group(0).split(":", 1)[1])))
    return found


def check_citations(root: Path, registered: dict[str, set[str]] | None = None) -> list[str]:
    """第 1 层：扫 src/**/*.rs 与名册对账。返回判红消息（空 = 通过）。"""
    table = REGISTERED_CITATIONS if registered is None else registered
    scanned: dict[str, set[str]] = {}
    problems: list[str] = []
    rust_files = sorted(root.glob("src/**/*.rs")) if (root / "src").is_dir() else []
    if not rust_files:
        return [f"FAIL: {root}/src 下没有任何 .rs 文件——扫描根不对，按前置错误处理"]
    for path in rust_files:
        rel = path.relative_to(root).as_posix()
        text = path.read_text(encoding="utf-8")
        scanned.setdefault(rel, set()).update(span for _, span in extract_citations(text))
    for rel in sorted(set(scanned) | set(table)):
        found, expected = scanned.get(rel, set()), table.get(rel, set())
        for span in sorted(found - expected):
            problems.append(
                f"DRIFT: {rel} 引用了未登记的 GatewayWsConnection.ts:{span} —— "
                "先用 --ts-baseline 对真实 TS 基线核对新行号，再把 span 登记进 REGISTERED_CITATIONS"
            )
        for span in sorted(expected - found):
            problems.append(
                f"STALE: REGISTERED_CITATIONS 里 {rel} 的 {span} 已不在该文件里 —— "
                "注释被删改时必须同步更新名册"
            )
    return problems


def resolve_ts_path(baseline: str) -> Path:
    """接受 PilotDeck 根、resources 目录、或直接指向 .ts 文件的路径。"""
    p = Path(baseline)
    if p.is_file():
        return p
    if p.is_dir():
        for candidate in (p / TS_RELATIVE, p / "PilotDeck" / TS_RELATIVE):
            if candidate.is_file():
                return candidate
    raise FileNotFoundError(f"基线路径 {baseline!r} 下找不到 {TS_RELATIVE}（也不像直接指向该文件）")


def check_ts_baseline(ts_path: Path, anchors: list[tuple[str, int, str]] | None = None) -> list[str]:
    """第 2 层：逐锚点实读 TS 行号与签名。返回判红消息（空 = 通过）。"""
    lines = ts_path.read_text(encoding="utf-8").split("\n")
    problems: list[str] = []
    for name, line, signature in (TS_ANCHORS if anchors is None else anchors):
        if not 1 <= line <= len(lines):
            problems.append(f"TS-DRIFT: {name} 钉在 :{line}，但基线只有 {len(lines)} 行")
            continue
        if signature not in lines[line - 1]:
            problems.append(
                f"TS-DRIFT: {name} 钉在 :{line}，但该行没有签名 {signature!r} —— "
                f"实读该行：{lines[line - 1].strip()[:100]}"
            )
    return problems


def _self_check() -> int:
    """机检自身的变异负控：每条都必须让对应检查变红，否则本步非零退出。"""
    import tempfile

    failures: list[str] = []

    def expect_red(label: str, problems: list[str], needle: str) -> None:
        if not any(needle in message for message in problems):
            failures.append(f"{label}: 未报出 {needle!r}（实际：{problems or 'PASS'}）")

    # NC1（名册失配）：把一个登记 span 改错（模拟 #2795 的原始漂移 215-255 未清）
    # → 扫描必须把它报成 DRIFT + STALE。
    mutated = {k: set(v) for k, v in REGISTERED_CITATIONS.items()}
    key = "src/pilotdeck-gateway/src/native_process/methods/cron/cron_tests.rs"
    mutated[key] = {"215-255"}
    problems = check_citations(ROOT, mutated)
    expect_red("NC1a", problems, "DRIFT")
    expect_red("NC1b", problems, "STALE")

    # NC2（真实漂移面）：名册不动、把一个文件的引用当已漂移处理（cron.rs 的 194-255
    # 被改回旧值 215-255 的形状），必须报 DRIFT。
    stale_registered = {k: set(v) for k, v in REGISTERED_CITATIONS.items()}
    stale_registered["src/pilotdeck-gateway/src/native_process/methods/cron.rs"] = {"215-255", "464-475"}
    expect_red("NC2", check_citations(ROOT, stale_registered), "DRIFT")

    # NC3（解析形态）：跨行 / 逗号 / and / 全角横线都必须被认出来（漏认 = 名册 STALE 假红）。
    samples = {
        "// (GatewayWsConnection.ts:\n// 176-185); keep": "176-185",
        "/// (`GatewayWsConnection.ts:249-\n/// 253`)": "249-253",
        "// GatewayWsConnection.ts:187,236-253": "187,236-253",
        "// GatewayWsConnection.ts:187 and 236-253": "187,236-253",
        "// `GatewayWsConnection.ts:405-425, 523-543`": "405-425,523-543",
        "//! `GatewayWsConnection.ts:542–557`": "542-557",
        "// GatewayWsConnection.ts:332 and InProcessGateway.ts:640": "332",
    }
    for text, expected in samples.items():
        got = extract_citations(text)
        if [expected] != [span for _, span in got]:
            failures.append(f"NC3: {text!r} 解析成 {got}，应为 {expected!r}")

    # NC4（TS 实读正控 + 负控）：用签名表合成一份「恰好命中」的假 TS → 必须通过；
    # 再抹掉 catch 行的签名（模拟 TS 在 catch 前删了一行、签名落到别处）→ 必须报 TS-DRIFT。
    with tempfile.TemporaryDirectory() as tmp:
        good_lines = [""] * 700
        for _name, line, signature in TS_ANCHORS:
            good_lines[line - 1] = signature
        fake_good = Path(tmp) / "GatewayWsConnection.ts"
        fake_good.write_text("\n".join(good_lines), encoding="utf-8")
        if check_ts_baseline(fake_good):
            failures.append(f"NC4a: 合成的全命中假 TS 被误判红：{check_ts_baseline(fake_good)}")
        bad_lines = list(good_lines)
        bad_lines[187] = ""  # catch_all 钉在 188 行
        fake_bad = Path(tmp) / "GatewayWsConnection-bad.ts"
        fake_bad.write_text("\n".join(bad_lines), encoding="utf-8")
        expect_red("NC4b", check_ts_baseline(fake_bad), "TS-DRIFT")

    # NC5（fail-closed）：--ts-baseline 给了但文件不存在 → main 必须退 2，不得静默跳过。
    rc = main(["--ts-baseline", "/nonexistent-pilotdeck-baseline-xyz"])
    if rc != 2:
        failures.append(f"NC5: 基线路径失效时 main 退 {rc}，应为 2（fail-closed）")

    if failures:
        for failure in failures:
            print(f"SELF-CHECK FAIL: {failure}", file=sys.stderr)
        return 1
    print("self-check: 全部负控按预期变红/解析正确")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--ts-baseline", help="PilotDeck 基线路径（根目录或 .ts 文件本身）")
    parser.add_argument("--self-check", action="store_true", help="跑机检自身的变异负控")
    args = parser.parse_args(argv)

    if args.self_check:
        return _self_check()

    problems = check_citations(ROOT)
    baseline = args.ts_baseline or os.environ.get("PDRS_TS_BASELINE")
    if baseline:
        try:
            ts_path = resolve_ts_path(baseline)
        except FileNotFoundError as exc:
            print(f"FAIL-CLOSED: {exc}", file=sys.stderr)
            return 2
        problems.extend(check_ts_baseline(ts_path))
    else:
        print("note: 未提供 --ts-baseline / PDRS_TS_BASELINE，只跑引用名册层（CI 形态）")

    if problems:
        for problem in problems:
            print(f"FAIL: {problem}", file=sys.stderr)
        print(f"BLOCKED {len(problems)} / READY 0", file=sys.stderr)
        return 1
    print(f"OK: 引用名册 {sum(len(v) for v in REGISTERED_CITATIONS.values())} 条对账通过")
    return 0


if __name__ == "__main__":
    sys.exit(main())
