#!/usr/bin/env python3
"""Product scope admission, independent of frozen behavioral scoring."""
import argparse
import json
import os
import re
import signal
import subprocess
import unicodedata
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
REF = "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"
LEDGER = ROOT / "specs/product-coverage/entries.json"
STATUSES = {"placeholder", "partial-native", "ts-forwarded", "ts-only", "rust-unaccepted",
            "unsupported", "retained-shell-unverified", "ts-orchestration-pending"}


def unique(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON key: " + key)
        result[key] = value
    return result


def read(path):
    return json.loads(Path(path).read_text(), object_pairs_hook=unique)


def enumeration(baseline):
    cmd = ["node", str(Path(__file__).with_name("enumerate.mjs")), str(baseline)]
    with subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                          text=True, start_new_session=True) as process:
        try:
            stdout, stderr = process.communicate(timeout=60)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.communicate()
            raise
        if process.returncode:
            raise subprocess.CalledProcessError(process.returncode, cmd, stdout, stderr)
    return json.loads(stdout, object_pairs_hook=unique)


def local_ref(ref):
    if not isinstance(ref, str) or not ref.strip():
        return False
    file, _, fragment = ref.partition("#")
    file, _, line = file.partition(":")
    path = (ROOT / file).resolve()
    if not path.is_relative_to(ROOT) or not path.is_file():
        return False
    if not fragment and not line:
        return True
    lines = path.read_text().splitlines()
    if line and (not line.isdecimal() or not 1 <= int(line) <= len(lines)):
        return False
    if fragment:
        def slug(text):
            return ''.join(c for c in text.lower() if c in '-_ ' or unicodedata.category(c)[0] in 'LN').replace(' ', '-')
        if fragment not in {slug(text.lstrip('#').strip()) for text in lines if text.startswith('#')}:
            return False
    return True


def plan_ref(ref, *, acceptance=False):
    if not isinstance(ref, str):
        return False
    file = ref.split("#", 1)[0].split(":", 1)[0]
    allowed = file.startswith("docs/plan/") or (acceptance and file.startswith("specs/"))
    return allowed and file.endswith(".md") and local_ref(ref)


def check(data, baseline):
    errors = []
    refs = {}
    def valid_ref(ref):
        if not isinstance(ref, str):
            return False
        if ref not in refs:
            refs[ref] = local_ref(ref)
        return refs[ref]
    if type(data.get("schema")) is not int or data.get("schema") != 1 or data.get("baseline") != REF or baseline.get("baseline") != REF:
        errors.append("schema/baseline mismatch")
    groups = data.get("groups", {})
    if not isinstance(groups, dict) or not groups:
        return ["missing groups"]
    for name, group in groups.items():
        if not isinstance(group, dict):
            errors.append(f"{name}: invalid group")
            continue
        if group.get("kind") not in {"product", "apparatus"}:
            errors.append(f"{name}: missing group kind")
        if group.get("stage") not in {"A", "B", "C", "D", "F"} or not re.fullmatch(r"#[1-9][0-9]*", str(group.get("issue", ""))):
            errors.append(f"{name}: missing stage/issue")
        if not plan_ref(group.get("task")) or not plan_ref(group.get("acceptance"), acceptance=True):
            errors.append(f"{name}: missing task/acceptance reference")
    expected = {row["id"]: row for row in baseline["entries"]}
    if not expected or len(expected) != len(baseline["entries"]):
        return errors + ["invalid independent enumeration"]
    rows = data.get("entries")
    if not isinstance(rows, list) or not rows:
        return errors + ["missing entries"]
    seen = set()
    used = set()
    for row in rows:
        if not isinstance(row, dict) or not isinstance(row.get("id"), str):
            errors.append("invalid entry")
            continue
        key = row["id"]
        if key in seen:
            errors.append("duplicate entry: " + key)
        seen.add(key)
        group = row.get("group")
        if not isinstance(group, str) or group not in groups:
            errors.append("unowned entry: " + key)
        elif not isinstance(groups[group], dict):
            errors.append("invalid entry group: " + key)
        else:
            used.add(group)
            if groups[group].get("kind") != "product":
                errors.append("entry assigned to apparatus: " + key)
        if not isinstance(row.get("status"), str) or row["status"] not in STATUSES or not valid_ref(row.get("evidence")):
            errors.append("missing status/evidence: " + key)
        if key in expected and row.get("sources") != expected[key]["sources"]:
            errors.append("baseline source mismatch: " + key)
    errors.extend("missing entry: " + key for key in sorted(expected.keys() - seen))
    errors.extend("extra entry: " + key for key in sorted(seen - expected.keys()))
    errors.extend("unused product group: " + name for name in groups.keys() - used
                  if isinstance(groups[name], dict) and groups[name].get("kind") == "product")
    if data.get("source_hashes") != baseline.get("sources"):
        errors.append("baseline source hashes mismatch")
    return errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, default=ROOT / "resources/PilotDeck")
    parser.add_argument("--ledger", type=Path, default=LEDGER)
    args = parser.parse_args()
    try:
        data = read(args.ledger)
        if not isinstance(data, dict):
            raise ValueError("ledger must be an object")
        errors = check(data, enumeration(args.baseline))
        result = {"verdict": "FAIL" if errors else "PASS", "entries": len(data.get("entries", [])), "errors": errors}
    except (OSError, ValueError, TypeError, KeyError, subprocess.SubprocessError) as error:
        result = {"verdict": "FAIL", "errors": [str(error)]}
    print(json.dumps(result, ensure_ascii=False, indent=2))
    return int(result["verdict"] != "PASS")


if __name__ == "__main__":
    raise SystemExit(main())
