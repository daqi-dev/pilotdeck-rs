#!/usr/bin/env python3
"""Compare isolated Feishu setup PTYs, without services or real credentials."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import pty
import selectors
import signal
import subprocess
import termios
import time

BASELINE = "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"
SOURCE = "src/cli/commands/gatewaySetup.ts"
TIMEOUT = 10
CASES = {
    "blank": [b"\n", b"\n", b"\n"],
    "backspace": [b"larkx\x7f\n", b"\n", b"\n"],
    "arrow_delete": [b"larkx\x1b[D\x1b[3~\n", b"\n", b"\n"],
    "ctrl_c": [b"\x03"],
    "ctrl_d": [b"\x04"],
    "ctrl_u": [b"typed\x15\n", b"\n", b"\n"],
    "unicode": ["中文".encode() + b"\x7f\x15\n", b"\n", b"\n"],
    "sigterm": ["SIGTERM"],
}
PROMPTS = ["(当前: feishu): ".encode(), b"App ID: ", b"App Secret: "]


def publish(path, value):
    with path.open("x") as output:
        json.dump(value, output, ensure_ascii=False, indent=2)
        output.write("\n")


def provenance(args):
    head = subprocess.check_output(["git", "-C", str(args.baseline), "rev-parse", "HEAD"], text=True).strip()
    if head != BASELINE:
        raise SystemExit(f"refusing baseline HEAD {head}; expected {BASELINE}")
    original = subprocess.check_output(["git", "-C", str(args.baseline), "show", f"{BASELINE}:{SOURCE}"])
    if original != (args.baseline / SOURCE).read_bytes():
        raise SystemExit("refusing modified baseline setup source")
    versions = json.loads(subprocess.check_output([
        str(args.node), "-p", "JSON.stringify({node:process.version,icu:process.versions.icu})"
    ], text=True))
    if versions != {"node": "v22.22.0", "icu": "78.2"}:
        raise SystemExit(f"refusing Node/ICU mismatch: {versions}")
    return {
        "baseline": head,
        "source": SOURCE,
        "sourceSha256": hashlib.sha256(original).hexdigest(),
        "node": str(args.node),
        "versions": versions,
        "rustBinary": str(args.rust),
        "rustBinarySha256": hashlib.sha256(args.rust.read_bytes()).hexdigest(),
        "term": args.term if args.term is not None else os.environ.get("TERM"),
        "boundary": "direct runGatewaySetup; Rust complete bin; no old-bin bootstrap",
    }


def run(args, out, kind, name, answers):
    home = out / f"{name}-{kind}"
    home.mkdir()
    env = os.environ.copy()
    env.update(HOME=str(home), PILOT_HOME=str(home / "pilot"),
               PILOTDECK_CONFIG_PATH=str(home / "pilot" / "pilotdeck.yaml"))
    env.pop("NODE_OPTIONS", None)
    if args.term is not None:
        env["TERM"] = args.term
    driver = Path(__file__).with_name("pty-driver.mjs")
    command = ([str(args.rust), "gateway", "setup", "feishu"] if kind == "rust" else [
        str(args.node), "--import", str(args.baseline / "node_modules/tsx/dist/loader.mjs"),
        str(driver), str(args.baseline / SOURCE),
    ])
    master, slave = pty.openpty()
    before = termios.tcgetattr(slave)
    child = subprocess.Popen(command, stdin=slave, stdout=slave, stderr=slave, env=env, cwd=home)
    os.close(slave)
    selector = selectors.DefaultSelector()
    selector.register(master, selectors.EVENT_READ)
    raw, index, timed_out, flags = b"", 0, False, []
    deadline = time.monotonic() + TIMEOUT
    while time.monotonic() < deadline:
        if not selector.select(.03):
            if child.poll() is not None:
                break
            continue
        try:
            chunk = os.read(master, 65536)
        except OSError:
            break
        if not chunk:
            break
        raw += chunk
        if index < len(answers) and index < len(PROMPTS) and PROMPTS[index] in raw:
            attributes = termios.tcgetattr(master)
            flags.append({"ECHO": bool(attributes[3] & termios.ECHO),
                          "ICANON": bool(attributes[3] & termios.ICANON)})
            answer = answers[index]
            index += 1
            if answer == "SIGTERM":
                child.send_signal(signal.SIGTERM)
            else:
                os.write(master, answer)
    # Closing the slave may produce EIO just before waitpid observes exit.
    # Preserve the original deadline instead of misclassifying that race.
    if child.poll() is None:
        try:
            child.wait(timeout=max(.001, deadline - time.monotonic()))
        except subprocess.TimeoutExpired:
            timed_out = True
            child.kill()
    code = child.wait()
    after = termios.tcgetattr(master)
    selector.close()
    os.close(master)
    files = {str(path.relative_to(home)): path.read_text()
             for path in home.rglob("*") if path.is_file()}
    result = {"rc": code, "timeout": timed_out, "stdout": raw.decode(errors="replace"),
              "bytes": list(raw), "flagsAtPrompt": flags, "restored": after == before, "files": files}
    publish(out / f"{name}-{kind}.json", result)
    return result


def main():
    parser = argparse.ArgumentParser()
    for name in ("rust", "baseline", "node", "output"):
        parser.add_argument(f"--{name}", required=True, type=Path)
    parser.add_argument("--term")
    args = parser.parse_args()
    args.rust, args.baseline, args.node = (path.resolve() for path in (args.rust, args.baseline, args.node))
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    publish(out / "provenance.json", provenance(args))
    results = []
    for name, answers in CASES.items():
        ts, rust = (run(args, out, kind, name, answers) for kind in ("ts", "rust"))
        results.append({
            "case": name, "stdout": ts["bytes"] == rust["bytes"], "rc": ts["rc"] == rust["rc"],
            "restored": ts["restored"] and rust["restored"],
            "flags": ts["flagsAtPrompt"] == rust["flagsAtPrompt"],
            "noFiles": not ts["files"] and not rust["files"],
            "bounded": not ts["timeout"] and not rust["timeout"],
        })
    publish(out / "summary.json", results)
    print(json.dumps(results, indent=2))
    return int(any(not all(value for key, value in item.items() if key != "case") for item in results))


if __name__ == "__main__":
    raise SystemExit(main())
