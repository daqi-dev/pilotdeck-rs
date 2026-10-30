#!/usr/bin/env python3
"""#1120 opt-in EdgeClaw quality smoke; stdlib, Python -I compatible.

The coordinator must reserve one shared provider slot for the duration and
provide a dated reservation file. This is an operational lease, not a new
semaphore that existing review runners would ignore. No key goes to children.
Exit 0 = all 18 observations complete, NOT human acceptance; 3 = incomplete.
"""
import argparse
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import secrets
import signal
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib import error, request

ROOT = Path(__file__).resolve().parents[2]
FIXTURE = ROOT / "src/pilotdeck-gateway/tests/fixtures/edgeclaw-real-provider"
spec = importlib.util.spec_from_file_location("real_provider", Path(__file__).with_name("real_provider.py"))
common = importlib.util.module_from_spec(spec)
spec.loader.exec_module(common)

MODEL = "glm-5.3-mb"
REPETITIONS = 3
CASES = 3
MAX_REQUESTS_PER_CELL = 8
MAX_REQUESTS = CASES * REPETITIONS * 2 * MAX_REQUESTS_PER_CELL
MAX_OUTPUT_TOKENS = 4096
MAX_TOTAL_OUTPUT_TOKENS = MAX_REQUESTS * MAX_OUTPUT_TOKENS
MAX_INPUT_BYTES = 65536
MAX_RESPONSE_BYTES = 2 * 1024 * 1024
HTTP_TIMEOUT = 60
CELL_TIMEOUT = 300
BUILD_TIMEOUT = 1200
UPSTREAM = common.LLM_CENTER_BASE_URL + "/v1/messages"
BASELINE_COMMIT = "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"


def write_json(path, value, redact=lambda s: s):
    with path.open("x", encoding="utf-8") as out:
        out.write(redact(json.dumps(value, ensure_ascii=False, indent=2)) + "\n")


def read_json(path):
    return json.loads(path.read_text(encoding="utf-8"))


def reservation(path, now=None):
    """Fail closed; a human/coordinator supplies the actual shared-slot lease."""
    value = json.load(path) if hasattr(path, "read") else read_json(path)
    now = time.time() if now is None else now
    if not (value.get("issue") == 1120 and value.get("model") == MODEL
            and value.get("provider_slots") == 1
            and isinstance(value.get("owner"), str) and value["owner"].strip()
            and isinstance(value.get("expires_unix"), (int, float))
            and not isinstance(value["expires_unix"], bool)
            and now + HTTP_TIMEOUT < value["expires_unix"] <= now + 7200):
        raise ValueError("#1120 valid coordinator provider reservation required")
    return value


def acquire_reservation(path):
    lease_file = path.open("r")
    try:
        fcntl.flock(lease_file, fcntl.LOCK_EX | fcntl.LOCK_NB)
        lease = reservation(lease_file)
        stat = os.fstat(lease_file.fileno())
        lease.update(path=str(path.resolve()), inode=[stat.st_dev, stat.st_ino])
        return lease_file, lease
    except BaseException:
        lease_file.close()
        raise


def capped_payload(raw):
    if len(raw) > MAX_INPUT_BYTES:
        raise ValueError("#1120 input byte cap")
    body = json.loads(raw)
    if not isinstance(body, dict) or body.get("model") != MODEL or body.get("stream"):
        raise ValueError("#1120 model or stream outside smoke scope")
    if not isinstance(body.get("messages"), list) or not body["messages"]:
        raise ValueError("#1120 missing messages")
    cap = body.get("max_tokens")
    if type(cap) is not int or cap <= 0:
        raise ValueError("#1120 missing native max_tokens")
    body["max_tokens"] = min(cap, MAX_OUTPUT_TOKENS)
    body["temperature"] = 0
    body["thinking"] = {"type": "disabled"}
    return body


def reported_usage(payload):
    usage = payload.get("usage") if isinstance(payload, dict) else None
    value = usage.get("output_tokens") if isinstance(usage, dict) else None
    return value if type(value) is int and value >= 0 else None


def verified_usage(payload, cap):
    value = reported_usage(payload)
    if value is None or value > cap:
        raise ValueError("#1120 missing/invalid/over-cap output usage")
    if payload.get("model") not in (MODEL, "GLM-5.3"):
        raise ValueError("#1120 response model mismatch")
    if payload.get("stop_reason") != "end_turn":
        raise ValueError("#1120 incomplete model output")
    if not isinstance(payload.get("content"), list) or not payload["content"]:
        raise ValueError("#1120 empty model output")
    return value


class NoRedirect(request.HTTPRedirectHandler):
    def redirect_request(self, *_args, **_kwargs):
        raise ValueError("#1120 upstream redirect refused")


def transport(dispatch):
    """Owned subprocess: parent imposes a hard wall-clock limit, including
    slow headers/body. Credentials are read only from .env.local, never argv.
    Partial response bytes are emitted as NDJSON chunks so a killed worker's
    already-received bytes survive in the parent's bounded capture.
    """
    # Refuse standalone/replayed worker invocations before reading credentials.
    # The coordinator's active reservation must still be held by our parent.
    signal.alarm(HTTP_TIMEOUT)
    record = read_json(dispatch)
    if (record.get("parent_pid") != os.getppid()
            or type(record.get("ordinal")) is not int or not 1 <= record["ordinal"] <= MAX_REQUESTS
            or type(record.get("cell_ordinal")) is not int
            or not 1 <= record["cell_ordinal"] <= MAX_REQUESTS_PER_CELL):
        raise ValueError("#1120 transport requires its owning harness parent")
    with Path(record["reservation_path"]).open("r") as lease_file:
        stat = os.fstat(lease_file.fileno())
        if [stat.st_dev, stat.st_ino] != record["reservation_inode"]:
            raise ValueError("#1120 reservation inode changed")
        reservation(lease_file)
        try:
            fcntl.flock(lease_file, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            pass
        else:
            raise ValueError("#1120 transport requires the parent's active reservation lock")
    body = capped_payload(json.dumps(record["request"], ensure_ascii=False).encode())
    with dispatch.with_suffix(".sent").open("x") as receipt:
        receipt.write("owned single dispatch\n")
    key_file = common.resolve_env_file(ROOT)
    key = common.read_env_file(key_file, {"ZAI_API_KEY"})["ZAI_API_KEY"]
    opener = request.build_opener(request.ProxyHandler({}), NoRedirect())
    req = request.Request(UPSTREAM, json.dumps(body, ensure_ascii=False).encode(), headers={
        "Content-Type": "application/json", "x-api-key": key,
        "anthropic-version": "2023-06-01",
    }, method="POST")
    try:
        response = opener.open(req, timeout=HTTP_TIMEOUT)
    except error.HTTPError as exc:
        response = exc
    with response:
        print(json.dumps({"http_status": response.status}), flush=True)
        count = 0
        while count <= MAX_RESPONSE_BYTES:
            data = response.read1(min(16384, MAX_RESPONSE_BYTES + 1 - count))
            if not data:
                break
            count += len(data)
            # Hex keeps UTF-8 boundaries lossless across incremental reads.
            print(json.dumps({"hex": data.hex()}), flush=True)
        print(json.dumps({"complete": count <= MAX_RESPONSE_BYTES}), flush=True)


class Relay:
    """Single serving thread, shared request budget across all fresh cells."""
    def __init__(self, evidence, redact, lease):
        self.evidence, self.redact, self.lease = evidence, redact, lease
        self.count, self.total_tokens = 0, 0
        self.cell, self.cell_count = None, 0
        self.records = []
        self.lock = threading.Lock()
        self.token = secrets.token_hex(24)

    def begin(self, cell):
        with self.lock:
            self.cell, self.cell_count = cell, 0
            # A queued HTTP request from a finished/aborted child must not be
            # attributed to the next cell, even if accept() runs late.
            self.token = secrets.token_hex(24)
            return len(self.records)

    def forward(self, raw):
        # Check at dispatch, not merely at process startup. Failures are never
        # retried here; native retries arrive as separate budgeted requests.
        if time.time() + HTTP_TIMEOUT >= self.lease["expires_unix"]:
            raise ValueError("#1120 provider reservation expired")
        if self.count >= MAX_REQUESTS or self.cell_count >= MAX_REQUESTS_PER_CELL:
            raise ValueError("#1120 request budget exhausted")
        body = capped_payload(raw)
        # Also remove host-specific paths before external transmission.
        body = json.loads(json.dumps(body).replace(str(ROOT), "/synthetic-smoke"))
        if len(json.dumps(body, ensure_ascii=False).encode()) > MAX_INPUT_BYTES:
            raise ValueError("#1120 serialized input byte cap")
        self.count += 1
        self.cell_count += 1
        record = {"ordinal": self.count, "cell": self.cell.name, "request": body,
                  "cell_ordinal": self.cell_count, "parent_pid": os.getpid(),
                  "reservation_path": self.lease.get("path"),
                  "reservation_inode": self.lease.get("inode")}
        dispatch = self.evidence / f"dispatch-{self.count:03}.json"
        write_json(dispatch, record, self.redact)
        try:
            outcome = common.run_bounded(
                [sys.executable, "-I", str(Path(__file__).resolve()), "--transport", str(dispatch)],
                ROOT, {"PATH": os.environ.get("PATH", "/usr/bin:/bin"),
                       "HOME": str(self.cell / "home"), "TMPDIR": str(self.cell / "tmp"),
                       "PILOT_HOME": str(self.cell / "pilot-home")}, HTTP_TIMEOUT)
            events, malformed = [], False
            for line in outcome["stdout"].splitlines():
                try:
                    events.append(json.loads(line))
                except ValueError:
                    malformed = True
            data = b"".join(bytes.fromhex(event["hex"]) for event in events if "hex" in event)
            record["response_text"] = data.decode("utf-8", "replace")
            record["http_status"] = next((event["http_status"] for event in events if "http_status" in event), None)
            record["transport"] = {k: v for k, v in outcome.items() if k != "stdout"}
            if malformed or outcome["exit"] != 0 or outcome["timedOut"] or not any(e.get("complete") for e in events):
                raise ValueError("#1120 bounded provider transport incomplete")
            if record["http_status"] != 200:
                raise ValueError("#1120 provider HTTP failure")
            if len(data) > MAX_RESPONSE_BYTES:
                raise ValueError("#1120 response byte cap")
            payload = json.loads(data)
            record["response"] = payload
            self.total_tokens += verified_usage(payload, body["max_tokens"])
            record["verified"] = True
            return self.redact(data.decode("utf-8")).encode()
        except Exception as exc:
            record["error"] = str(exc)
            raise
        finally:
            self.records.append(record)
            write_json(self.evidence / f"request-{self.count:03}.json", record, self.redact)


def server_for(relay):
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_POST(self):
            try:
                size = int(self.headers.get("Content-Length", "0"))
                if not 0 < size <= MAX_INPUT_BYTES:
                    raise ValueError("#1120 request length outside cap")
                with relay.lock:
                    if self.path != "/v1/messages" or self.headers.get("x-api-key") != relay.token:
                        raise ValueError("#1120 unauthorized relay request")
                    data = relay.forward(self.rfile.read(size))
                status = 200
            except Exception as exc:
                data = json.dumps({"error": relay.redact(str(exc))}).encode()
                status = 400  # native retries must not multiply rejected requests
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            try:
                self.wfile.write(data)
            except (BrokenPipeError, ConnectionResetError):
                pass

        def setup(self):
            super().setup()
            self.connection.settimeout(HTTP_TIMEOUT)
    server = HTTPServer(("127.0.0.1", 0), Handler)
    if server.server_port == 18789:
        server.server_close()
        raise ValueError("#1120 reserved port refused")
    return server


def measure(case, observation):
    """Quality metrics only read durable record content and injected context.

    Traces, query, input, and L0 transcripts cannot inflate a hit. Marker
    scoring is reproducible but conservative: paraphrases need human review.
    """
    records = observation["records"]
    if not isinstance(records, list) or any(not isinstance(r, dict) or not isinstance(r.get("content"), str) for r in records):
        raise ValueError("#1120 invalid memory records")
    summary = observation["user-summary"]
    # Whitelist fields from MemoryUserSummary; metadata is never evidence.
    if (not isinstance(summary, dict) or not isinstance(summary.get("identityBackground"), list)
            or any(not isinstance(s, str) for s in summary["identityBackground"])
            or not isinstance(summary.get("files"), list)
            or any(not isinstance(r, dict) or not isinstance(r.get("content"), str) for r in summary["files"])):
        raise ValueError("#1120 invalid user summary")
    durable = "\n".join([r["content"] for r in records + summary["files"]]
                        + summary["identityBackground"]).casefold()
    recall = observation["recall"]
    if not isinstance(recall, dict):
        raise ValueError("#1120 invalid recall object")
    context = recall.get("systemContext")
    if not isinstance(context, str):
        raise ValueError("#1120 invalid recall context")
    context = context.casefold()
    hits = lambda text: {fact["id"]: all(marker.casefold() in text for marker in fact["all"])
                         for fact in case["facts"]}
    return {"extracted": hits(durable), "recalled": hits(context),
            "chatter_written": any(word.casefold() in durable for word in case["chatter"]),
            "durable_records": len(records), "has_durable_content": bool(durable.strip()),
            "chatter_only_any_write": not case["facts"] and bool(durable.strip()),
            "context": context}


def observe(cell, case, calls):
    names = ("capture", "before-flush", "flush", "records", "entries",
             "user-summary", "recall", "after-flush", "index-traces")
    data = {name: read_json(cell / f"{name}.json") for name in names}
    if data["capture"].get("captured") is not True:
        raise ValueError("#1120 L0 capture missing")
    normalized = data["capture"].get("normalizedMessages")
    if (not isinstance(normalized, list)
            or [{"role": m.get("role"), "content": m.get("content")} for m in normalized] != case["messages"]):
        raise ValueError("#1120 capture transcript differs from preregistered sample")
    if data["before-flush"].get("overview", {}).get("pendingSessions") != 1:
        raise ValueError("#1120 captured L0 not pending")
    stats = data["flush"]
    if stats.get("failedSessions") != 0 or stats.get("capturedSessions") != 1:
        raise ValueError("#1120 native indexing failed/incomplete")
    if data["after-flush"].get("overview", {}).get("pendingSessions") != 0:
        raise ValueError("#1120 indexing left pending L0")
    if (not isinstance(data["entries"], list) or not isinstance(data["records"], list)
            or any(not isinstance(item, dict) or not isinstance(item.get("relativePath"), str)
                   for item in data["entries"] + data["records"])
            or {item["relativePath"] for item in data["entries"]}
            != {item["relativePath"] for item in data["records"]}):
        raise ValueError("#1120 listed memory files were not read completely")
    if not calls or any(call.get("verified") is not True for call in calls):
        raise ValueError("#1120 provider accounting incomplete")
    return measure(case, data)


def comparison(rows, cases):
    groups = []
    for case in cases:
        for implementation in ("rust", "ts"):
            selected = [row for row in rows if row["case"] == case["id"]
                        and row["implementation"] == implementation]
            valid = [row["quality"] for row in selected if row["status"] == "OBSERVED"]
            groups.append({"case": case["id"], "implementation": implementation,
                           "planned": REPETITIONS, "observed": len(valid),
                           "extracted": {fact["id"]: sum(q["extracted"][fact["id"]] for q in valid)
                                         for fact in case["facts"]},
                           "recalled": {fact["id"]: sum(q["recalled"][fact["id"]] for q in valid)
                                        for fact in case["facts"]},
                           "chatter_written": sum(q["chatter_written"] for q in valid),
                           "chatter_only_any_write": sum(q["chatter_only_any_write"] for q in valid)})
    return groups


def accounting(relay):
    reported, unknown = 0, 0
    for call in relay.records:
        usage = reported_usage(call.get("response"))
        if usage is not None:
            reported += usage
        else:
            unknown += 1
    unknown += relay.count - len(relay.records)
    return {"dispatches": relay.count, "reported_output_tokens": reported,
            "verified_output_tokens": relay.total_tokens, "unknown_usage_requests": unknown,
            "reserved_output_token_ceiling": relay.count * MAX_OUTPUT_TOKENS}


def report(evidence, groups):
    lines = ["# #1120 D-4 观测对比", "", "最终状态以 verdict.json 为准；实效待人签字。",
             "", "## AI 可实测", "",
             "比率只在该组 3/3 有效时计算；BLOCKED 不算零命中，也不从分母静默剔除。",
             "标记命中是保守的字面观测；语义改写和不当否定须人工复核。", "",
             "| 样本 | 实现 | 有效/计划 | 事实 | 抽取命中率 | 召回命中率 | 闲聊写入率 | 纯闲聊任意写入率 |",
             "|---|---|---|---|---|---|---|---|"]
    for group in groups:
        rate = lambda count: (f"{count}/3 ({count / 3:.1%})" if group["observed"] == 3
                              else f"N/A（已见 {count} 次）")
        for fact in group["extracted"] or {"不适用": 0}:
            extracted = rate(group["extracted"][fact]) if fact in group["extracted"] else "—"
            recalled = rate(group["recalled"][fact]) if fact in group["recalled"] else "—"
            lines.append(f"| {group['case']} | {group['implementation']} | {group['observed']}/3 | "
                         f"{fact} | {extracted} | {recalled} | {rate(group['chatter_written'])} | "
                         f"{rate(group['chatter_only_any_write']) if not group['extracted'] else '—'} |")
    lines += ["", "## 仅人眼", "", "- 对照样本与 records/user-summary/recall 原件，复核事实表达、否定和语义改写。",
              "- 评估记忆是否简洁、召回是否对后续回答有用；本装置不生成后续助手答案。",
              "- 签字人：待填写；日期：待填写；适用范围：本模型、本参数及三个合成样本。", ""]
    with (evidence / "comparison.md").open("x", encoding="utf-8") as out:
        out.write("\n".join(lines))


def isolated_env(cell, baseline, relay, port):
    env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "LANG": "en_US.UTF-8"}
    for key, directory in (("HOME", "home"), ("PILOT_HOME", "pilot-home"),
                           ("TMPDIR", "tmp"), ("XDG_CONFIG_HOME", "config"),
                           ("XDG_CACHE_HOME", "cache")):
        path = cell / directory
        path.mkdir()
        env[key] = str(path)
    (cell / "project").mkdir()
    env.update(EDGECLAW_SMOKE_CELL=str(cell), PDRS_BASELINE=str(baseline),
               EDGECLAW_SMOKE_MODEL=MODEL, EDGECLAW_SMOKE_TOKEN=relay.token,
               EDGECLAW_SMOKE_URL=f"http://127.0.0.1:{port}",
               TSX_DISABLE_CACHE="1")
    return env


def build(evidence, redact):
    target = ROOT / "_build/cargo-target"
    if os.environ.get("CARGO_TARGET_DIR") != str(target):
        raise ValueError("#1120 set the mandated CARGO_TARGET_DIR before running")
    for name in ("CARGO_PROFILE_DEV_DEBUG", "CARGO_PROFILE_TEST_DEBUG"):
        if os.environ.get(name) != "line-tables-only":
            raise ValueError(f"#1120 {name} must be line-tables-only")
    slots = common.run_cargo_slots(ROOT)
    write_json(evidence / "cargo-slots.json", slots, redact)
    if slots.get("exit") != 0 or slots.get("stderr", "").strip():
        raise ValueError("#1120 cargo slot count unavailable or full")
    result = common.run_bounded([
        "cargo", "build", "--locked", "--manifest-path", str(ROOT / "src/Cargo.toml"),
        "-p", "pilotdeck-gateway", "--example", "edgeclaw_smoke",
    ], ROOT, dict(os.environ), BUILD_TIMEOUT)
    write_json(evidence / "build.json", result, redact)
    if result["exit"] != 0 or result["timedOut"]:
        raise ValueError("#1120 Rust probe build incomplete")
    return target / "debug/examples/edgeclaw_smoke"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--evidence", type=Path, required=True)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--node", default="/opt/homebrew/bin/node")
    parser.add_argument("--reservation", type=Path, required=True)
    args = parser.parse_args()
    evidence = args.evidence.resolve()
    if not evidence.is_relative_to(ROOT / "_tmp"):
        parser.error("evidence must be inside this checkout's _tmp")
    evidence.mkdir(parents=True, exist_ok=False)
    keys = {}
    redact = common.redactor(keys)
    rows, cases, failed, lease_file, relay = [], [], False, None, None
    write_json(evidence / "limits.json", {"model": MODEL, "repetitions": REPETITIONS,
               "case_count": CASES, "limits": {name: value for name, value in globals().items()
               if name.startswith("MAX_") or name.endswith("TIMEOUT")}})
    try:
        env_file = common.resolve_env_file(ROOT)
        if env_file is None or env_file.name != ".env.local":
            raise ValueError("#1120 missing .env.local")
        keys = common.read_env_file(env_file, {"ZAI_API_KEY", "DEEPSEEK_API_KEY", "ANTHROPIC_API_KEY"})
        redact = common.redactor(keys)
        if not keys.get("ZAI_API_KEY"):
            raise ValueError("#1120 missing ZAI_API_KEY")
        lease_file, lease = acquire_reservation(args.reservation)
        cases = read_json(FIXTURE / "cases.json")
        if len(cases) != CASES or len({case["id"] for case in cases}) != CASES:
            raise ValueError("#1120 case count/identity mismatch")
        manifest = {"issue": 1120, "model": MODEL, "repetitions": REPETITIONS,
                    "case_count": CASES, "reservation": lease,
                    "limits": {name: value for name, value in globals().items()
                               if name.startswith("MAX_") or name.endswith("TIMEOUT")},
                    "parameters": {"max_tokens": MAX_OUTPUT_TOKENS, "temperature": 0,
                                   "thinking": {"type": "disabled"}},
                    "baseline_commit": BASELINE_COMMIT,
                    "files": {str(p.relative_to(ROOT)): hashlib.sha256(p.read_bytes()).hexdigest()
                              for p in (Path(__file__), FIXTURE / "cases.json", FIXTURE / "run.mts",
                                        ROOT / "src/pilotdeck-gateway/examples/edgeclaw_smoke.rs")}}
        git_env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
        for name, command in (("rust_head", ["git", "rev-parse", "HEAD"]),
                              ("rust_status", ["git", "status", "--porcelain", "--untracked-files=all"]),
                              ("rust_product_diff", ["git", "diff", "HEAD", "--", "src/*/src", "src/*/Cargo.toml", "src/Cargo.lock", "src/Cargo.toml", "src/data"])):
            observed = common.run_bounded(command, ROOT, git_env, 30)
            if observed["exit"] != 0 or observed["timedOut"]:
                raise ValueError("#1120 product provenance unavailable")
            manifest[name] = observed["stdout"]
        if manifest["rust_product_diff"].strip():
            raise ValueError("#1120 product source differs from HEAD")
        write_json(evidence / "manifest.json", manifest, redact)
        write_json(evidence / "cases.json", cases)
        probe = build(evidence, redact)
        relay = Relay(evidence, redact, lease)
        server = server_for(relay)
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()
        try:
            for repeat in range(1, REPETITIONS + 1):
                for case in cases:
                    # Alternate pair order to reduce systematic timing bias.
                    for implementation in (("rust", "ts") if repeat % 2 else ("ts", "rust")):
                        cell = evidence / f"{case['id']}-{repeat}-{implementation}"
                        cell.mkdir()
                        write_json(cell / "input.json", case)
                        start = relay.begin(cell)
                        env = isolated_env(cell, args.baseline.resolve(), relay, server.server_port)
                        command = ([str(probe)] if implementation == "rust" else [
                            args.node, "--import", str(args.baseline.resolve() / "node_modules/tsx/dist/loader.mjs"),
                            str(FIXTURE / "run.mts")])
                        outcome = common.run_bounded(command, cell / "project", env, CELL_TIMEOUT)
                        write_json(cell / "process.json", outcome, redact)
                        with relay.lock:
                            calls = list(relay.records[start:])
                        row = {"case": case["id"], "repeat": repeat, "implementation": implementation}
                        try:
                            if outcome["exit"] != 0 or outcome["timedOut"]:
                                raise ValueError("#1120 native driver incomplete")
                            row["quality"] = observe(cell, case, calls)
                            row["status"] = "OBSERVED"
                        except Exception as exc:
                            row.update(status="BLOCKED", error=str(exc))
                            failed = True
                        rows.append(row)
                        write_json(cell / "assessment.json", row, redact)
                        print(f"{cell.name}: {row['status']}", flush=True)
                        if outcome["timedOut"] or outcome["exit"] != 0:
                            raise ValueError("#1120 failed driver ends the run")
        finally:
            server.shutdown()
            worker.join(HTTP_TIMEOUT + 5)
            server.server_close()
    except Exception as exc:
        failed = True
        write_json(evidence / "blocked.json", {"error": str(exc)}, redact)
    finally:
        if relay is not None:
            write_json(evidence / "accounting.json", accounting(relay), redact)
        else:
            write_json(evidence / "accounting.json", {"dispatches": 0, "reported_output_tokens": 0,
                       "verified_output_tokens": 0, "unknown_usage_requests": 0,
                       "reserved_output_token_ceiling": 0})
        if lease_file is not None:
            lease_file.close()
    # All artifacts (including SQLite and model-written files) are scanned;
    # the real key never reached the native processes or local request bodies.
    complete = not failed and len(rows) == CASES * REPETITIONS * 2
    groups = comparison(rows, cases)
    write_json(evidence / "summary.json", {"observations_complete": complete,
               "human_acceptance": "待人签字", "rows": rows, "comparison": groups}, redact)
    report(evidence, groups)
    check = common.self_check(common.collect_artifacts(evidence), keys)
    write_json(evidence / "secret-scan.json", check)
    complete = complete and common.self_check_ok(check)
    write_json(evidence / "verdict.json", {"status": "AI 已实测，待人签字" if complete else "BLOCKED"})
    print("AI 已实测，待人签字" if complete else "BLOCKED (see evidence)")
    return 0 if complete else 3


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--transport":
        transport(Path(sys.argv[2]))
    else:
        raise SystemExit(main())
