#!/usr/bin/env python3
"""Real-provider smoke harness (stdlib only; runs under `python3 -I`).

Layer 0 (runnable now): the gateway model runtime's public `complete_model` /
`stream_model` HTTP path, exercised through `scripts/smoke/probe/` against the
llm-center Anthropic endpoint.

Layer 1 (NOT_YET): the full gateway process path (#589 SubmitHost/WS + host
composition + permission/hooks/steer). Those scenarios are reported as NOT_YET
with the exact missing wiring; they are never marked PASS.

Secrets: read at runtime from the repository `.env.local` (or --env-file),
passed only in the probe child environment, redacted from every artifact, and
never printed. The self-check reports hit/miss only.
"""

import argparse
import json
import os
import re
import shutil
import socket
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

# llm-center host root, same value as scripts/run-review-roles.py:47.
LLM_CENTER_BASE_URL = "https://llm-center.modelbest.co"

# Hard limits for one smoke run (three probe scenarios, one request each).
MAX_REQUESTS = 6
MAX_OUTPUT_TOKENS_PER_REQUEST = 128
MAX_TOTAL_OUTPUT_TOKENS = MAX_REQUESTS * MAX_OUTPUT_TOKENS_PER_REQUEST
PROBE_TIMEOUT_SECONDS = 120

# Ports reserved for the future #589 host/WS assembly; checked before use.
RESERVED_PORTS = (28789, 28790, 28810, 28811, 28812)

# Missing wiring for the acceptance steps that cannot run on origin/main.
NOT_YET = {
    "gateway_e2e_text": (
        "no gateway process entry: src/pilotdeck-gateway/src/main.rs:15-32 is a "
        "WebSocket/HTTP proxy only; src/pilotdeck-cli/src/main.rs:63 'server' and "
        "src/pilotdeck-cli/src/sdk.rs:41-44 'run' are Failure::Todo; the local "
        "gateway path at src/pilotdeck-cli/src/main.rs:104-106 is Todo"
    ),
    "gateway_e2e_tool": (
        "same missing SubmitHost/WS entry as gateway_e2e_text; model.stream is a "
        "host callback (src/pilotdeck-transport/src/default_factory.rs:841-851) "
        "and the parent host composition is not wired"
    ),
    "gateway_e2e_cancel": (
        "no run/turn process path to cancel; runtime cancel exists only inside "
        "the library (src/pilotdeck-gateway/src/runtime/turn/live.rs) and is not "
        "reachable from a shipped binary"
    ),
    "permission_ask_allow_deny": (
        "#586 acceptance step 2: needs the SubmitHost/WS + per-turn permission "
        "port binding from #589; factory registers placeholders only "
        "(src/pilotdeck-gateway/src/runtime/project/interaction.rs)"
    ),
    "hooks_invocation": (
        "#586 acceptance step 2: needs the host-owned session contribution "
        "HooksSettings/Binding from #589; no host process composes HookRuntime "
        "with a real turn"
    ),
    "steer_durable_order": (
        "#586 acceptance step 3: needs session mailbox + durable recorder in a "
        "live turn (#589); no run process accepts steer"
    ),
    "reload_generation": (
        "#586 acceptance step 4: needs old-turn reload/generation drain through "
        "the host composition (#589)"
    ),
    "cleanup_once": (
        "#586 acceptance step 3/5: needs the live host to observe handler and "
        "resource release counters; not reachable without #589"
    ),
}

SECRET_LINE = re.compile(r"^\s*(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*)$")
GENERIC_SECRET = re.compile(
    r"(?:sk-[A-Za-z0-9_-]{16,}|x-api-key\s*:\s*\S+|Bearer\s+[A-Za-z0-9._-]{16,})",
    re.IGNORECASE,
)


def repo_root() -> Path:
    return Path(__file__).resolve().parents[2]


def iso_now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds")


def read_env_file(path: Path, names: set[str]) -> dict[str, str]:
    """Read only the requested names. Never log keys or values."""
    found: dict[str, str] = {}
    if not path.is_file():
        return found
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        match = SECRET_LINE.match(line)
        if not match:
            continue
        key, value = match.group(1), match.group(2).strip()
        if key not in names:
            continue
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "'\"":
            value = value[1:-1]
        if value:
            found[key] = value
    return found


def key_env_for(model: str) -> str:
    lowered = model.lower()
    if lowered.startswith("glm"):
        return "ZAI_API_KEY"
    if lowered.startswith("deepseek"):
        return "DEEPSEEK_API_KEY"
    if lowered.startswith("claude"):
        return "ANTHROPIC_API_KEY"
    return "ANTHROPIC_API_KEY"


def redactor(secrets: dict[str, str]):
    values = [v for v in secrets.values() if len(v) >= 4]

    def redact(text: str) -> str:
        for value in values:
            text = text.replace(value, "[REDACTED]")
        return GENERIC_SECRET.sub("[REDACTED]", text)

    return redact


def port_state(port: int) -> str:
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    try:
        sock.bind(("127.0.0.1", port))
        return "free"
    except OSError:
        return "in_use"
    finally:
        sock.close()


def run_cargo_slots(root: Path) -> dict:
    script = root.parent.parent / "scripts" / "cargo-slots.sh"
    if not script.is_file():
        return {"status": "SKIPPED", "detail": f"missing {script}"}
    proc = subprocess.run(
        ["sh", str(script)], capture_output=True, text=True, timeout=30
    )
    return {
        "status": "PASS" if proc.returncode == 0 else "BLOCKED",
        "exit": proc.returncode,
        "stdout": redactor({})(proc.stdout.strip()),
        "stderr": redactor({})(proc.stderr.strip()),
    }


def build_probe(root: Path, evidence: Path, redact) -> dict:
    probe = root / "scripts" / "smoke" / "probe"
    target = evidence / "cargo-target"
    env = os.environ.copy()
    env["CARGO_TARGET_DIR"] = str(target)
    env.pop("PDRS_PROVIDER_KEY", None)
    started = time.time()
    proc = subprocess.run(
        ["cargo", "build", "--offline"],
        cwd=probe,
        env=env,
        capture_output=True,
        text=True,
        timeout=600,
    )
    log = proc.stdout + ("\n" + proc.stderr if proc.stderr else "")
    (evidence / "probe-build.log").write_text(redact(log), encoding="utf-8")
    binary = target / "debug" / "real-provider-probe"
    return {
        "status": "PASS" if proc.returncode == 0 and binary.is_file() else "FAIL",
        "exit": proc.returncode,
        "binary": str(binary),
        "elapsedSeconds": round(time.time() - started, 2),
        "log": str(evidence / "probe-build.log"),
    }


def classify_failure(payload: dict) -> str:
    error = payload.get("error")
    text = json.dumps(error, ensure_ascii=False).lower() if error is not None else ""
    if payload.get("kind") == "timeout":
        return "environment-or-provider (timeout)"
    for needle in (
        "authentication",
        "permission_error",
        "rate_limit",
        "overloaded",
        "api_error",
        "timeout",
        "econn",
        "network",
        "fetch failed",
        "dns",
    ):
        if needle in text:
            return "environment-or-provider"
    return "product-or-protocol"


def run_probe(binary: Path, scenario: str, base_url: str, model: str, key: str,
              run_dir: Path, redact) -> tuple[dict, int]:
    home = run_dir / "home"
    cwd = run_dir / "cwd"
    tmp = run_dir / "tmp"
    for path in (home, cwd, tmp):
        path.mkdir(parents=True, exist_ok=True)
    child_env = {
        "PATH": "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin",
        "HOME": str(home),
        "TMPDIR": str(tmp),
        "LANG": "C.UTF-8",
        "PDRS_PROVIDER_KEY": key,
    }
    started = time.time()
    try:
        proc = subprocess.run(
            [str(binary), scenario, base_url, model],
            cwd=cwd,
            env=child_env,
            capture_output=True,
            text=True,
            timeout=PROBE_TIMEOUT_SECONDS,
        )
        stdout, stderr, code = proc.stdout, proc.stderr, proc.returncode
    except subprocess.TimeoutExpired as exc:
        stdout = (exc.stdout or "") if isinstance(exc.stdout, str) else ""
        stderr = (exc.stderr or "") if isinstance(exc.stderr, str) else ""
        code = -1
    elapsed = round(time.time() - started, 3)
    raw_dir = run_dir / "raw"
    raw_dir.mkdir(parents=True, exist_ok=True)
    (raw_dir / f"{scenario}.stdout.txt").write_text(redact(stdout), encoding="utf-8")
    (raw_dir / f"{scenario}.stderr.txt").write_text(redact(stderr), encoding="utf-8")
    payload = None
    for line in stdout.splitlines():
        if line.startswith("PROBE_RESULT "):
            try:
                payload = json.loads(line[len("PROBE_RESULT "):])
            except json.JSONDecodeError:
                payload = None
    if payload is None:
        payload = {
            "scenario": scenario,
            "status": "FAIL",
            "kind": "harness",
            "error": "probe produced no PROBE_RESULT line",
            "exit": code,
        }
    payload["elapsedSeconds"] = elapsed
    payload["exit"] = code
    payload["raw"] = {
        "stdout": str(raw_dir / f"{scenario}.stdout.txt"),
        "stderr": str(raw_dir / f"{scenario}.stderr.txt"),
    }
    return payload, code


def missing_scenarios() -> list[dict]:
    return [
        {
            "id": key,
            "layer": "gateway-process",
            "status": "NOT_YET",
            "missing": value,
        }
        for key, value in NOT_YET.items()
    ]


def write_markdown(report: dict, path: Path) -> None:
    lines = [
        "# Real provider smoke report",
        "",
        f"- generated: {report['generatedAt']}",
        f"- commit: `{report['git']['commit']}` on `{report['git']['branch']}`",
        f"- endpoint: `{report['provider']['baseUrl']}` (Anthropic Messages)",
        f"- model: `{report['provider']['model']}`",
        f"- credential source: `{report['provider']['keyAlias']}` (value never read out)",
        f"- limits: {report['limits']['maxRequests']} requests, "
        f"{report['limits']['maxOutputTokensPerRequest']} output tokens/request",
        f"- ports checked: {report['ports']}",
        "",
        "## Scenarios",
        "",
        "| scenario | layer | status | detail |",
        "| --- | --- | --- | --- |",
    ]
    for item in report["scenarios"]:
        detail = (
            item.get("detail")
            or item.get("missing")
            or item.get("error")
            or item.get("text")
            or ""
        )
        if not detail and item.get("toolCalls"):
            detail = f"toolCalls={len(item['toolCalls'])}"
        if not detail and item.get("firstDeltaChars"):
            detail = f"firstDeltaChars={item['firstDeltaChars']}"
        if isinstance(detail, dict):
            detail = json.dumps(detail, ensure_ascii=False)
        detail = str(detail).replace("|", "\\|").replace("\n", " ")
        lines.append(
            f"| `{item['id']}` | {item['layer']} | **{item['status']}** | {detail[:300]} |"
        )
    lines += [
        "",
        "## Failure classification",
        "",
    ]
    failures = [s for s in report["scenarios"] if s["status"] == "FAIL"]
    if not failures:
        lines.append("No FAIL.")
    else:
        for item in failures:
            lines.append(
                f"- `{item['id']}`: {item.get('classification', 'product-or-protocol')}"
            )
    lines += [
        "",
        "## Secret self-check",
        "",
        "| key alias | result |",
        "| --- | --- |",
    ]
    for check in report["secretSelfCheck"]["keys"]:
        lines.append(f"| `{check['alias']}` | {check['result']} |")
    lines.append("")
    lines.append(
        f"Scanned {report['secretSelfCheck']['files']} artifact files "
        f"(excluding cargo-target); generic-pattern hits: "
        f"{report['secretSelfCheck']['genericHits']}."
    )
    lines += [
        "",
        "## What is not proven",
        "",
        "The three PASS/FAIL rows exercise the model HTTP runtime directly. "
        "They do not prove the gateway process, SubmitHost/WS, permission, "
        "hooks, steer, reload drain, or cleanup counters; those acceptance "
        "steps are NOT_YET above and stay 待人工验收.",
        "",
    ]
    path.write_text("\n".join(lines), encoding="utf-8")


def collect_artifacts(evidence: Path) -> list[Path]:
    files = []
    for path in evidence.rglob("*"):
        if not path.is_file():
            continue
        if "cargo-target" in path.parts:
            continue
        files.append(path)
    return sorted(files)


def self_check(files: list[Path], secrets: dict[str, str]) -> dict:
    keys = []
    for alias, value in secrets.items():
        needle = value.encode()
        prefix = value[:8].encode()
        hit = False
        for path in files:
            data = path.read_bytes()
            if needle in data or prefix in data:
                hit = True
                break
        keys.append({"alias": alias, "result": "命中" if hit else "未命中"})
    generic = 0
    for path in files:
        text = path.read_text(encoding="utf-8", errors="replace")
        generic += len(GENERIC_SECRET.findall(text))
    return {
        "files": len(files),
        "keys": keys,
        "genericHits": generic,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", default="deepseek-v4.1-flash-mb")
    parser.add_argument("--base-url", default=LLM_CENTER_BASE_URL)
    parser.add_argument("--key-env", default=None,
                        help="override the .env.local key name")
    parser.add_argument("--env-file",
                        default="/Users/da/ws/pilotdeck-rs/.env.local")
    parser.add_argument("--evidence-dir", default=None)
    parser.add_argument("--self-check-only", action="store_true",
                        help="scan existing evidence artifacts for key leakage")
    args = parser.parse_args()

    root = repo_root()
    evidence = Path(args.evidence_dir) if args.evidence_dir else (
        root / "_tmp" / "20261025-real-provider-smoke"
    )
    evidence.mkdir(parents=True, exist_ok=True)

    key_alias = args.key_env or key_env_for(args.model)
    env_names = {key_alias, "ANTHROPIC_API_KEY", "DEEPSEEK_API_KEY", "ZAI_API_KEY"}
    secrets = read_env_file(Path(args.env_file), env_names)
    # Redact/scan every key this run learned about, not only the one passed
    # to the child. Only `key` below reaches the child environment.
    redact = redactor(secrets)
    key = secrets.get(key_alias, "")

    if args.self_check_only:
        files = collect_artifacts(evidence)
        check = self_check(files, secrets)
        lines = [
            f"files: {check['files']}",
            f"generic-pattern hits: {check['genericHits']}",
        ]
        for item in check["keys"]:
            lines.append(f"{item['alias']}: {item['result']}")
        (evidence / "self-check-final.txt").write_text(
            "\n".join(lines) + "\n", encoding="utf-8"
        )
        post = self_check(collect_artifacts(evidence), secrets)
        print("final secret self-check (all artifacts, cargo-target excluded):")
        for item in post["keys"]:
            print(f"  {item['alias']}: {item['result']}")
        print(f"  generic-pattern hits: {post['genericHits']}")
        ok = all(item["result"] == "未命中" for item in post["keys"]) and (
            post["genericHits"] == 0
        )
        print(f"  finalPass: {ok}")
        return 0 if ok else 2

    run_dir = evidence / "run"
    if run_dir.exists():
        shutil.rmtree(run_dir)
    run_dir.mkdir(parents=True)

    git_commit = subprocess.run(
        ["git", "rev-parse", "--short", "HEAD"], cwd=root,
        capture_output=True, text=True,
    ).stdout.strip() or "unknown"
    git_branch = subprocess.run(
        ["git", "branch", "--show-current"], cwd=root,
        capture_output=True, text=True,
    ).stdout.strip() or "unknown"

    limits = {
        "maxRequests": MAX_REQUESTS,
        "maxOutputTokensPerRequest": MAX_OUTPUT_TOKENS_PER_REQUEST,
        "maxTotalOutputTokens": MAX_TOTAL_OUTPUT_TOKENS,
        "requestsSent": 0,
    }
    ports = {str(port): port_state(port) for port in RESERVED_PORTS}
    slots = run_cargo_slots(root)

    build = {"status": "SKIPPED", "detail": "no key available"}
    scenarios: list[dict] = []
    if not key:
        for name in ("text", "stream-cancel", "tool"):
            scenarios.append({
                "id": f"model_runtime_{name.replace('-', '_')}",
                "layer": "model-runtime",
                "status": "BLOCKED",
                "missing": f"{key_alias} is not present in the env file",
                "raw": {},
            })
    elif slots["status"] == "BLOCKED":
        for name in ("text", "stream-cancel", "tool"):
            scenarios.append({
                "id": f"model_runtime_{name.replace('-', '_')}",
                "layer": "model-runtime",
                "status": "BLOCKED",
                "missing": "cargo-slots.sh reports all slots busy",
                "raw": {},
            })
    else:
        build = build_probe(root, evidence, redact)
        if build["status"] != "PASS":
            for name in ("text", "stream-cancel", "tool"):
                scenarios.append({
                    "id": f"model_runtime_{name.replace('-', '_')}",
                    "layer": "model-runtime",
                    "status": "BLOCKED",
                    "missing": "probe build failed; see probe-build.log",
                    "raw": {},
                })
        else:
            binary = Path(build["binary"])
            for name, scenario_id in (
                ("text", "model_runtime_text"),
                ("stream-cancel", "model_runtime_stream_cancel"),
                ("tool", "model_runtime_tool"),
            ):
                if limits["requestsSent"] >= MAX_REQUESTS:
                    scenarios.append({
                        "id": scenario_id,
                        "layer": "model-runtime",
                        "status": "BLOCKED",
                        "missing": "MAX_REQUESTS exhausted",
                        "raw": {},
                    })
                    continue
                limits["requestsSent"] += 1
                payload, _ = run_probe(
                    binary, name, args.base_url, args.model, key, run_dir, redact
                )
                payload["id"] = scenario_id
                payload["layer"] = "model-runtime"
                if payload.get("status") == "FAIL":
                    payload["classification"] = classify_failure(payload)
                scenarios.append(payload)

    scenarios.extend(missing_scenarios())

    report = {
        "generatedAt": iso_now(),
        "git": {"commit": git_commit, "branch": git_branch},
        "provider": {
            "baseUrl": args.base_url,
            "model": args.model,
            "protocol": "anthropic",
            "keyAlias": key_alias,
        },
        "limits": limits,
        "ports": ports,
        "cargoSlots": slots,
        "build": build,
        "scenarios": scenarios,
        "notes": [
            "Layer 0 drives pilotdeck-gateway's public model HTTP runtime "
            "(complete_model/stream_model) through scripts/smoke/probe.",
            "Layer 1 gateway/permission/hooks/steer scenarios are NOT_YET, "
            "not failures and not passes.",
        ],
    }

    # First pass: artifacts with a placeholder check, so the scan sees the
    # report itself too. Then write the final check result and rescan.
    report["secretSelfCheck"] = {"files": 0, "keys": [], "genericHits": 0}
    (evidence / "report.json").write_text(
        json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8"
    )
    write_markdown(report, evidence / "report.md")
    check = self_check(collect_artifacts(evidence), secrets)
    report["secretSelfCheck"] = check
    (evidence / "report.json").write_text(
        json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8"
    )
    write_markdown(report, evidence / "report.md")
    final = self_check(collect_artifacts(evidence), secrets)
    final["finalPass"] = all(
        item["result"] == "未命中" for item in final["keys"]
    ) and final["genericHits"] == 0
    report["secretSelfCheck"] = final
    (evidence / "report.json").write_text(
        json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8"
    )
    write_markdown(report, evidence / "report.md")

    # Post-write scan: the last writes add only check results/booleans, but
    # verify the exact bytes on disk once more.
    post = self_check(collect_artifacts(evidence), secrets)
    post_ok = all(item["result"] == "未命中" for item in post["keys"]) and (
        post["genericHits"] == 0
    )

    print(f"evidence: {evidence}")
    print(f"model: {args.model} via {args.base_url} (key={key_alias})")
    print(f"requests sent: {limits['requestsSent']} / {MAX_REQUESTS}")
    for item in report["scenarios"]:
        line = f"{item['status']:8} {item['id']} ({item['layer']})"
        if item["status"] == "FAIL":
            line += f" [{item.get('classification', 'product-or-protocol')}]"
        print(line)
    print("secret self-check (artifacts, cargo-target excluded):")
    for item in post["keys"]:
        print(f"  {item['alias']}: {item['result']}")
    print(f"  generic-pattern hits: {post['genericHits']}")
    print(f"  finalPass: {post_ok}")
    print(f"report: {evidence / 'report.md'}")
    if not post_ok:
        return 2
    return 1 if any(s["status"] == "FAIL" for s in report["scenarios"]) else 0


if __name__ == "__main__":
    sys.exit(main())
