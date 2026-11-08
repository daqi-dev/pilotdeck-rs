#!/usr/bin/env python3
"""#1841 device-patch recompute: strict-comparator context.budget position race.

Verifies the registered device patch
(specs/parity/device-patches/20261108-1841-trace-context-budget-position.patch)
against the frozen baseline trace.py and the real evidence corpus carried
with issue #1841 (11 labels x 75 rounds/side = 150 native/sidecar pairs).

Fail-closed: the baseline must hash to the registered clean SHA256, the patch
must apply to it, and the applied file must hash to the registered patched
SHA256. Since fix-r1 the evidence corpus is anchored too: the patch file,
this script and the corpus manifest must each hash to the sha256 pinned on
the registration file (20261108-1841.md), and every one of the 753 evidence
files must hash to its corpus-manifest line. Any mismatch aborts before any
comparison result is reported.

Since fix-r2 the anchoring is validate-once end to end (the same discipline
fix-r1 applied to the baseline): the corpus manifest, the patch file and
every evidence file are read exactly once, hash-checked against their pins,
and every later criterion consumes those verified bytes — the manifest
entries are parsed from the pinned bytes, git applies the patch from stdin,
and the 753-file corpus is held in memory (measured peak well under the
500 MB budget, see the registration file) with the frozen path-based
load_trace loading scratch materializations of the cached bytes. Nothing
that happens to any input file after its check can change what is compared.

Since fix-r3 the corpus read is path-set-first with an enforced budget: the
walk over --evidence collects file paths only, the missing/extra comparison
against the corpus manifest runs before any byte is read, and the bytes are
then read from the manifest's entries alone under a running total that
aborts past 500 MB — an unexpected file is rejected by the fail-closed set
check, never by an out-of-memory.

Usage (must run with -I; writes only to --out, its own temporary scratch
directories, and a temporary bytecode-cache directory — the same guard every
apparatus entry point uses, never a __pycache__ next to the baseline):
    python3 -I 20261108-1841-verify.py \
        --baseline <path to baseline tools/agent-loop-parity/trace.py> \
        --patch <path to 20261108-1841-trace-context-budget-position.patch> \
        --evidence <path to the #1841 evidence directory> \
        [--manifest <path to 20261108-1841-corpus-manifest.txt>] \
        [--out <path for the machine-readable JSON result>]

--manifest and the registration file (20261108-1841.md) default to the copies
next to this script. --out is created exclusively: an existing file is never
overwritten, the run writes <name>.attempt-N.<ext> instead. Neither --out nor
--manifest may sit inside or resolve inside the --evidence tree — as a
symlink, in a directory that itself sits inside it, or by resolve() target
(asserted at entry, fix-r3; the symlink and parent-directory readings are
fix-r4), or by filesystem identity: two spellings of one directory on a
case-insensitive volume are one directory, judged by os.path.samefile over the
nearest existing ancestor chain (the identity reading is fix-r5) — and since
fix-r6 that chain starts from the physicalized spelling
(realpath(path), direct since fix-r7), so a `..` over a not-yet-existing directory, a
relative --out with the cwd inside the tree and a symlink landing inside the
tree all reduce to the real ancestor chain before the walk; realpath anchors
relative paths to the cwd and resolves components left to right, following a
symlink before folding a subsequent `..` (the kernel open order); any OSError,
RuntimeError (resolve() over a symlink loop on Python 3.11/3.12) or ValueError
(embedded NUL) while judging dies at entry, and a degenerate --out (a
directory, or a parent that is a regular file) dies at entry too: criterion 0
pins the exact file set, so a file written there would make the next recompute
report it as unexpected and FAIL.

Criteria (registration 20261108-1841.md):
  0. evidence anchoring: corpus manifest (per-file sha256, exact file set)
     plus a trace-shape gate (every trace non-empty; per-trace counts of
     context.budget / model.request / model.response pinned to the corpus).
  1. patch applicability: git apply --check, apply, patched sha, restore.
  2. full-corpus recompute: exactly the 15 recorded position-race rounds
     change, deletions only, every deleted row attributable to a
     model.request/model.response/context.budget insertion misalignment
     (path-normalized, full-value row signatures); the other 135 pairs are
     unchanged; residual #6 / text rows are kept row by row (multiset).
  3. census recompute: deviation rows match evidence/census-all-labels.txt.
  4. negative controls: value, count, exemption-boundary, and within-kind
     order injections are still caught by the patched comparator; NC-E
     documents (not gates) that the position waiver has no window bound.
"""
from __future__ import annotations

import sys

# Same four-part entry guard as every apparatus entry point (scripts/parity/
# check_frozen.py:11-37, g03_compare.py, self_compare.py, check_ruler.py,
# controls/run_all.py); fix-r2, DS sibling #3 + simplify #1 — the guard is
# asserted, not narrated in the docstring.
# -I keeps PYTHONPATH, user site and the script directory out of module
# resolution; without it, exit before loading any apparatus module.
if not sys.flags.isolated:
    raise SystemExit(f'run with -I: python3 -I {__file__}')

if sys.version_info < (3, 11) or 'os' not in sys.modules:
    raise SystemExit('os must be preloaded from CPython 3.11+ frozen modules')
import os

_boot_dir = os.path.dirname(os.path.realpath(__file__))
# Identity, not spelling: a case-insensitive alias, a symlinked spelling or a
# mixed ``//`` form names this very directory while the string differs
# (#1033). Nonexistent sys.path entries skip; an empty entry means cwd.
if any(
        os.path.isdir(entry or os.curdir)
        and os.path.isdir(_boot_dir)
        and os.path.samefile(entry or os.curdir, _boot_dir)
        for entry in sys.path):
    raise SystemExit('apparatus directory on sys.path; run the entry file directly with -I')

import tempfile
sys.pycache_prefix = tempfile.mkdtemp(prefix="pdrs-1841-pyc-")
sys.dont_write_bytecode = True

import argparse
import hashlib
import importlib.util
import json
import re
import subprocess
from collections import Counter
from pathlib import Path

CLEAN_SHA256 = "6f4139c9f5938a5bd3e44f6a6bed69a69104e593d9c580bbc2f37c5c56c0d289"
PATCHED_SHA256 = "53a1c7f6f50dead912e420e5c20e5e57dafc89b9e370b371174a39fe770e1ce6"
TARGET_PATH = "tools/agent-loop-parity/trace.py"
REGISTRATION_FILENAME = "20261108-1841.md"
MANIFEST_FILENAME = "20261108-1841-corpus-manifest.txt"
# Only these kinds may disappear from a strict report because of the patch:
# each removal must be an insertion row (exactly one side None) of one of
# these kinds, i.e. pure position-misalignment noise.
POSITION_NOISE_KINDS = {"model.request", "model.response", "context.budget"}
# Residual rows that must survive the patch row by row (deadline-family #6
# leftovers and the budget-family user.output text difference).
RESIDUAL_PATH_MARKERS = ("durableStopReason", "resultType")
RESIDUAL_VALUE_MARKERS = ("gateway_submit_failed",)
# Corpus shape, measured on the #1841 evidence (fix-r1): every one of the
# 300 traces (150 native + 150 sidecar) carries exactly one context.budget and
# one model.request; 140 traces legitimately record no model.response (rounds
# without a response) and 160 record exactly one. Any drift from these
# histograms means the corpus is not the anchored one.
TRACE_SHAPE_KINDS = ("context.budget", "model.request", "model.response")
TRACE_SHAPE_HISTOGRAMS = {
    "context.budget": {1: 300},
    "model.request": {1: 300},
    "model.response": {0: 140, 1: 160},
}
# Negative-control base pair: a round that passes BOTH comparators with zero
# semantic differences and is not one of the 15 position-race rounds, so any
# failure after an injection is attributable to that injection alone.
NC_BASE = (
    "20261106-issue-1811-quiet2-dd-tool",
    "rust-r1",
    "deadline_during_tool",
)
# Git subprocess isolation (concurrency review #3): never inherit GIT_* from
# the caller, never read a global gitconfig, and always time out.
GIT_TIMEOUT_SECONDS = 60
# Corpus memory budget (fix-r3, P2-c+e): the walk over --evidence collects
# paths only and the bytes come from the manifest's entries alone, under a
# running total — the 500 MB the registration narrates is asserted here.
CORPUS_BUDGET_BYTES = 500_000_000
# Criterion 4's gated controls, pinned by name (fix-r3, P2-b): fix-r2 ruled
# out an empty gated list, but one control's killed drifting to None would
# silently shrink the denominator all() judges — the exact name set is
# asserted instead of just its non-emptiness.
EXPECTED_GATED = (
    "NC-A value mutation",
    "NC-B count mutation",
    "NC-C exemption boundary",
    "NC-D within-kind order",
)


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def die(message: str) -> "None":
    print(f"FAIL-CLOSED: {message}", file=sys.stderr)
    raise SystemExit(2)


def load_module(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


def read_jsonl(data: bytes) -> list[dict]:
    """JSONL reader over already-anchored bytes; splits on \\n only (never
    str.splitlines, #1140 r6/r9: U+2028/U+2029/U+0085/\\x0b/\\x0c would cut a
    record in half)."""
    records: list[dict] = []
    for line in data.decode("utf-8").split("\n"):
        if line.strip():
            records.append(json.loads(line))
    return records


def run_git(cwd: Path, *args: str, input_bytes: bytes | None = None) -> None:
    """Git subprocess isolation (concurrency review #3): never inherit GIT_*
    from the caller, never read a global gitconfig, and always time out.
    input_bytes feeds the command's stdin — `git apply -` reads the pinned
    patch bytes through the pipe, so the patch file is never opened again
    after its pin check (fix-r2, P1-a)."""
    env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
    env["GIT_CONFIG_GLOBAL"] = os.devnull
    command = ["git", "--no-optional-locks", "-c", "core.fsmonitor=false",
                "-c", "core.hooksPath=/dev/null", *args]
    try:
        if input_bytes is None:
            proc = subprocess.run(command, cwd=cwd, capture_output=True, text=True,
                                  env=env, timeout=GIT_TIMEOUT_SECONDS)
        else:
            proc = subprocess.run(command, cwd=cwd, capture_output=True,
                                  env=env, timeout=GIT_TIMEOUT_SECONDS, input=input_bytes)
    except subprocess.TimeoutExpired:
        die(f"git {' '.join(args)} timed out after {GIT_TIMEOUT_SECONDS}s")
    stderr = proc.stderr.decode("utf-8", "replace") if isinstance(proc.stderr, bytes) else proc.stderr
    if proc.returncode != 0:
        die(f"git {' '.join(args)} failed: {stderr.strip()}")


def parse_registration(path: Path) -> dict[str, str]:
    """sha256 pins the registration file must carry (fix-r1, sibling #1).

    A field's pin may sit on the ``- <Field>:`` line itself or on its indented
    continuation lines (the field block runs until the next ``- `` item).
    """
    if not path.exists():
        die(f"registration file not found: {path}")
    lines = path.read_text(encoding="utf-8").split("\n")
    blocks: dict[str, str] = {}
    current: str | None = None
    for line in lines:
        match = re.match(r"^- ([A-Za-z一-鿿]+): ", line)
        if match:
            current = match.group(1)
            blocks.setdefault(current, "")
        if current is not None and (match or line.startswith(" ")):
            blocks[current] += line + "\n"
        elif current is not None:
            current = None
    pins: dict[str, str] = {}
    for field in ("Patch", "Verify", "Corpus"):
        match = re.search(r"sha256=`?([0-9a-f]{64})`?", blocks.get(field, ""))
        if not match:
            die(f"registration {path} has no sha256= pin on its {field}: field")
        pins[field.lower()] = match.group(1)
    change = re.search(r"old=`?([0-9a-f]{64})`?\s+new=`?([0-9a-f]{64})`?", blocks.get("Change", ""))
    if not change or change.group(1) != CLEAN_SHA256 or change.group(2) != PATCHED_SHA256:
        die("registration Change: old/new sha256 disagree with this script's CLEAN/PATCHED constants")
    return pins


def parse_manifest(manifest: Path, data: bytes) -> dict[str, str]:
    """corpus manifest lines: '<sha256>  ./relative/path' (two spaces).
    fix-r2 (P1-a): parses the already-pinned bytes passed in — the manifest
    file itself is never opened here, so the entries always come from the
    very bytes the Corpus: pin verified."""
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError as error:
        die(f"corpus manifest is not utf-8: {error}")
    entries: dict[str, str] = {}
    for number, line in enumerate(text.split("\n"), 1):
        if not line:
            continue
        match = re.fullmatch(r"([0-9a-f]{64})  (\./.+)", line)
        if not match:
            die(f"corpus manifest line {number} of {manifest} is malformed: {line[:120]!r}")
        sha, rel = match.group(1), match.group(2)
        if rel in entries:
            die(f"corpus manifest lists {rel} more than once")
        entries[rel] = sha
    if not entries:
        die(f"corpus manifest is empty: {manifest}")
    return entries


def normalized_path(path: str) -> str:
    return re.sub(r"\[\d+\]", "[#]", path)


def row_signature(difference) -> tuple[str, str, str]:
    """Path-normalized, full-value row signature.

    Index renumbering is invisible ([n] -> [#]); the field part of the path
    and both difference values are compared exactly, so a field-level row is
    compared by its field and an insertion row carries its whole record.
    """
    return (
        normalized_path(difference.path),
        json.dumps(difference.left, ensure_ascii=False, sort_keys=True),
        json.dumps(difference.right, ensure_ascii=False, sort_keys=True),
    )


def is_residual(signature: tuple[str, str, str]) -> bool:
    path, left, right = signature
    if any(marker in path for marker in RESIDUAL_PATH_MARKERS):
        return True
    if path.endswith(".text"):
        return True
    return any(marker in left or marker in right for marker in RESIDUAL_VALUE_MARKERS)


def residual_rows(rows: Counter) -> Counter:
    """Multiset of residual row signatures: every occurrence counted.

    Distinct-signature counts are not enough (fix-r1, boundary #2): a dropped
    or duplicated residual row must be visible, so the comparison is over
    the full multiset of residual rows, not over the number of distinct ones.
    """
    return Counter(signature for signature in rows.elements() if is_residual(signature))


def is_position_noise_removal(signature: tuple[str, str, str]) -> tuple[bool, str]:
    """A removed row is attributable iff it is an insertion row (path has no
    pair marker, exactly one side None) of a position-noise kind."""
    path, left, right = signature
    if "~" in path or path != "trace[#]":
        return False, f"non-insertion path {path!r}"
    if (left == "null") == (right == "null"):
        return False, f"not a one-sided insertion: {path!r}"
    value = json.loads(left if left != "null" else right)
    if not isinstance(value, dict) or value.get("kind") not in POSITION_NOISE_KINDS:
        return False, f"insertion kind not in {sorted(POSITION_NOISE_KINDS)}: {value!r}"
    return True, value.get("kind")


SIDECAR_REL_PATTERN = re.compile(
    r"^\./([^/]+)/rounds/([^/]+)/([^/]+)\.pilotdeck-current-sidecar\.jsonl$")


def discover_pairs(corpus: dict[str, bytes]) -> list[tuple[str, str, str, str]]:
    """Pair enumeration over the anchored corpus file set (fix-r2, P1-a):
    exactly the shape the old */rounds/*/*.pilotdeck-current-sidecar.jsonl
    glob matched, but derived from the criterion-0 cache — the evidence
    tree is never scanned again after the read-and-hash pass."""
    pairs: list[tuple[str, str, str, str]] = []
    for rel in sorted(corpus):
        match = SIDECAR_REL_PATTERN.fullmatch(rel)
        if not match:
            continue
        label, round_name, scenario = match.groups()
        native = f"./{label}/rounds/{round_name}/{scenario}.pilotdeck-current-native.jsonl"
        if native not in corpus:
            die(f"sidecar trace without native counterpart: {rel}")
        pairs.append((label, round_name, native, rel))
    return pairs


def load_trace_from_bytes(module, data: bytes, scratch: Path, name: str):
    """Materialize anchored corpus bytes in the run's own scratch directory
    and load them with the frozen baseline loader (its API is path-based).
    The bytes the comparator sees are the bytes criterion 0 hashed: the
    evidence tree is never opened again (fix-r2, P1-a)."""
    path = scratch / name
    path.write_bytes(data)
    return module.load_trace(path)


# ---------------------------------------------------------------- criterion 0

def criterion0(evidence: Path, entries: dict[str, str]) -> tuple[dict, Counter, dict[str, bytes]]:
    """Evidence anchoring, validate-once (fix-r2, P1-a): every file under
    --evidence is read exactly once; the bytes are hashed against the corpus
    manifest and kept as the in-memory corpus cache that every later
    criterion consumes. Then a shape gate over the 150 pairs (boundary #1,
    fix-r1). fix-r3, P2-c+e: the walk collects paths only — the missing/extra
    set comparison runs before any byte is read, so an unexpected file (a
    mistyped --evidence, a stray multi-GiB payload) is rejected by the
    fail-closed path instead of being read first; the bytes then come from
    the manifest's entries alone, under a 500 MB running total."""
    rels = {"./" + path.relative_to(evidence).as_posix()
            for path in evidence.rglob("*") if path.is_file()}
    missing = sorted(set(entries) - rels)
    extra = sorted(rels - set(entries))
    if missing or extra:
        die(f"evidence tree does not match corpus manifest: {len(missing)} missing "
            f"(e.g. {missing[:3]}), {len(extra)} unexpected (e.g. {extra[:3]})")
    corpus: dict[str, bytes] = {}
    total_size = 0
    for rel in sorted(entries):
        data = (evidence / rel).read_bytes()
        total_size += len(data)
        if total_size > CORPUS_BUDGET_BYTES:
            die(f"corpus exceeds the {CORPUS_BUDGET_BYTES}-byte budget after "
                f"{len(corpus) + 1} files ({total_size} bytes read)")
        corpus[rel] = data
    mismatched = sorted(rel for rel in entries if sha256_bytes(corpus[rel]) != entries[rel])
    if mismatched:
        die(f"evidence file hash mismatch against corpus manifest: {len(mismatched)} "
            f"files differ (e.g. {mismatched[:3]})")

    pairs = discover_pairs(corpus)
    if len(pairs) != 150:
        die(f"expected 150 native/sidecar pairs under {evidence}, found {len(pairs)}")
    histograms = {kind: Counter() for kind in TRACE_SHAPE_KINDS}
    for _, _, native_rel, sidecar_rel in pairs:
        for rel in (native_rel, sidecar_rel):
            records = read_jsonl(corpus[rel])
            if not records:
                die(f"trace {rel} is empty (corpus shape gate)")
            for kind in TRACE_SHAPE_KINDS:
                histograms[kind][sum(1 for r in records if r.get("kind") == kind)] += 1
    for kind in TRACE_SHAPE_KINDS:
        observed = dict(sorted(histograms[kind].items()))
        if observed != TRACE_SHAPE_HISTOGRAMS[kind]:
            die(f"corpus shape gate: {kind} per-trace count histogram {observed} "
                f"!= expected {TRACE_SHAPE_HISTOGRAMS[kind]}")

    result = {
        "name": "0 evidence anchoring (corpus manifest + trace shape)",
        "manifest_files": len(entries),
        "trace_files": 2 * len(pairs),
        "kind_histograms": {
            kind: {str(count): total for count, total in sorted(histograms[kind].items())}
            for kind in TRACE_SHAPE_KINDS
        },
        "passed": True,
    }
    return result, histograms["context.budget"], corpus


# ---------------------------------------------------------------- criterion 1

def criterion1(baseline: Path, patch_bytes: bytes) -> tuple[dict, bytes, bytes]:
    """Patch applicability over the pinned patch bytes (fix-r2, P1-a: the
    patch file is never re-read — `git apply -` consumes the very bytes the
    Patch: pin verified, through a pipe). Returns the hash-verified clean and
    patched bytes so main() never has to re-read the baseline from disk
    (fix-r1, concurrency #1: validate-once, use-once)."""
    clean_bytes = baseline.read_bytes()
    if sha256_bytes(clean_bytes) != CLEAN_SHA256:
        die(f"baseline {baseline} sha256 {sha256_bytes(clean_bytes)} != registered {CLEAN_SHA256}")

    try:
        patch_text = patch_bytes.decode("utf-8")
    except UnicodeDecodeError as error:
        die(f"patch bytes are not utf-8: {error}")
    targets = re.findall(r"^diff --git a/(\S+) b/(\S+)$", patch_text, re.M)
    if len(targets) != 1 or targets[0] != (TARGET_PATH, TARGET_PATH):
        die(f"patch must touch exactly {TARGET_PATH}, found: {targets}")

    with tempfile.TemporaryDirectory(prefix="pdrs-1841-verify-") as tmp:
        repo = Path(tmp)
        target = repo / TARGET_PATH
        target.parent.mkdir(parents=True)
        target.write_bytes(clean_bytes)
        run_git(repo, "init", "-q", ".")
        run_git(repo, "add", TARGET_PATH)
        run_git(repo, "apply", "--check", "-", input_bytes=patch_bytes)
        run_git(repo, "apply", "-", input_bytes=patch_bytes)
        patched_bytes = target.read_bytes()
        patched_sha = sha256_bytes(patched_bytes)
        if patched_sha != PATCHED_SHA256:
            die(f"applied file sha256 {patched_sha} != registered {PATCHED_SHA256}")
        # Restore from the index (which still holds the clean blob) and
        # confirm the tree is back to the clean bytes.
        run_git(repo, "checkout", "--", TARGET_PATH)
        restored_sha = sha256_bytes(target.read_bytes())
        if restored_sha != CLEAN_SHA256:
            die(f"restored file sha256 {restored_sha} != clean {CLEAN_SHA256}")

    result = {
        "name": "1 patch applicability",
        "clean_sha256": CLEAN_SHA256,
        "patched_sha256": PATCHED_SHA256,
        "targets": [TARGET_PATH],
        "steps": [
            "baseline sha256 == registered clean value",
            "git apply --check - OK (isolated repo, file staged at tools/agent-loop-parity/trace.py, pinned patch bytes on stdin)",
            "git apply - OK (pinned bytes on stdin), applied sha256 == registered patched value",
            "git checkout -- restore, sha256 == clean value",
        ],
        "passed": True,
    }
    return result, clean_bytes, patched_bytes


# ---------------------------------------------------------------- criterion 2

def removed_rows_count(entry: dict) -> int:
    """Total deleted rows recorded for one changed pair (the per-row counts
    the result JSON carries). The registration says every changed pair loses
    exactly two insertion rows; criterion 2 gates on it (fix-r2, DS
    boundary #2)."""
    return sum(row["count"] for row in entry["removed"])


def criterion2(pairs, clean, patched, corpus: dict[str, bytes], scratch: Path,
               census_deviations: set[tuple[str, str]] | None = None) -> dict:
    if len(pairs) != 150:
        die(f"expected 150 native/sidecar pairs in the anchored corpus, found {len(pairs)}")

    changed: list[dict] = []
    unchanged = 0
    residual_mismatch: list[str] = []
    clean_pass = 0
    for label, round_name, native_rel, sidecar_rel in pairs:
        left = load_trace_from_bytes(clean, corpus[native_rel], scratch, "c2-native.jsonl")
        right = load_trace_from_bytes(clean, corpus[sidecar_rel], scratch, "c2-sidecar.jsonl")
        clean_rows = Counter(row_signature(d) for d in clean.compare_trace_details(left, right).semantic)
        patched_rows = Counter(row_signature(d) for d in patched.compare_trace_details(left, right).semantic)
        if not clean_rows:
            clean_pass += 1
        removed = clean_rows - patched_rows
        added = patched_rows - clean_rows
        # Residual rows (#6 leftovers, budget-family text diffs) must be kept
        # row by row even in the changed pairs: full multiset, so a dropped
        # or duplicated residual row fails the check.
        residual_kept = residual_rows(clean_rows) == residual_rows(patched_rows)
        if not residual_kept:
            residual_mismatch.append(f"{label}/{round_name}")
        if not removed and not added:
            unchanged += 1
            continue
        attribution = [is_position_noise_removal(sig) for sig, _ in removed.items()]
        changed.append({
            "label": label,
            "round": round_name,
            "removed": [
                {"path": sig[0], "left": sig[1][:400], "right": sig[2][:400], "count": count}
                for sig, count in sorted(removed.items())
            ],
            "added_count": sum(added.values()),
            "attributable": all(ok for ok, _ in attribution),
            "attribution_detail": [detail for ok, detail in attribution if not ok],
        })

    changed_set = {(entry["label"], entry["round"]) for entry in changed}
    matches_census = census_deviations is None or changed_set == census_deviations
    passed = (
        len(changed) == 15
        and unchanged == 135
        and all(entry["added_count"] == 0 for entry in changed)
        and all(entry["attributable"] for entry in changed)
        and all(removed_rows_count(entry) == 2 for entry in changed)
        and not residual_mismatch
        and matches_census
    )
    return {
        "name": "2 full-corpus recompute (150 pairs)",
        "pairs": len(pairs),
        "clean_pass_pairs": clean_pass,
        "changed_pairs": len(changed),
        "unchanged_pairs": unchanged,
        "only_deletions": all(entry["added_count"] == 0 for entry in changed),
        "all_deletions_attributable": all(entry["attributable"] for entry in changed),
        "two_removals_per_changed_pair": all(removed_rows_count(entry) == 2 for entry in changed),
        "residual_rows_preserved": not residual_mismatch,
        "residual_mismatch": residual_mismatch,
        "changed_matches_census_deviations": matches_census,
        "changed": changed,
        "passed": passed,
    }


# ---------------------------------------------------------------- criterion 3

def classify_census(records: list[dict]) -> str:
    first: dict[str, object] = {}
    for record in records:
        kind = record.get("kind")
        if kind in ("context.budget", "model.request", "model.response") and kind not in first:
            sequence = record.get("sequence")
            if isinstance(sequence, bool) or not isinstance(sequence, int):
                die(f"census: {kind} record has non-int 'sequence': {sequence!r}")
            first[kind] = sequence
    cb = first.get("context.budget")
    request = first.get("model.request")
    if cb is None or request is None:
        # The corpus shape gate (criterion 0) already anchored exactly one
        # context.budget and one model.request per trace, so an "n/a" here
        # means corrupted evidence, not a classification (fix-r1, boundary #1).
        die("census: context.budget/model.request missing from a trace the corpus shape gate anchored")
    if cb < request:
        return "cb_before_request"
    if (response := first.get("model.response")) is not None and cb < response:
        return "cb_between_req_resp"
    return "cb_after"


def criterion3(corpus: dict[str, bytes], pairs) -> dict:
    reference_rel = "./census-all-labels.txt"
    if reference_rel not in corpus:
        die(f"missing census reference {reference_rel} in the anchored corpus")
    expected = [line.split("\t", 1)[1]
                for line in corpus[reference_rel].decode("utf-8").split("\n")
                if line.startswith("DEVIATION\t")]

    tally: Counter = Counter()
    rows: list[str] = []
    for label, round_name, native_rel, sidecar_rel in pairs:
        side = "ts" if round_name.startswith("ts-") else "rust"
        sc = classify_census(read_jsonl(corpus[sidecar_rel]))
        nc = classify_census(read_jsonl(corpus[native_rel]))
        tally[(side, sc)] += 1
        tally[(side, "native:" + nc)] += 1
        if sc != nc:
            scenario = Path(sidecar_rel).name.split(".pilotdeck-current-sidecar")[0]
            rows.append(f"{label}\t{round_name}\t{scenario}\t{sc}\t{nc}")

    passed = rows == expected
    return {
        "name": "3 census recompute",
        "deviation_rows": rows,
        "expected_rows": expected,
        "deviation_pairs": sorted({(row.split("\t")[0], row.split("\t")[1]) for row in rows}),
        "tally": {f"{side}|{cls}": count for (side, cls), count in sorted(tally.items())},
        "passed": passed,
    }


# ---------------------------------------------------------------- criterion 4

def nc_result(clean, patched, left_records, right_records) -> tuple[dict, dict]:
    clean_out = clean.compare_trace_details(left_records, right_records)
    patched_out = patched.compare_trace_details(left_records, right_records)
    return (
        {"fail": bool(clean_out.semantic), "rows": len(clean_out.semantic),
         "paths": [normalized_path(d.path) for d in clean_out.semantic]},
        {"fail": bool(patched_out.semantic), "rows": len(patched_out.semantic),
         "paths": [normalized_path(d.path) for d in patched_out.semantic]},
    )


def write_temp_trace(records: list[dict], path: Path) -> None:
    path.write_text("".join(json.dumps(r, ensure_ascii=False) + "\n" for r in records), encoding="utf-8")


def criterion4(corpus: dict[str, bytes], clean, patched, scratch: Path, budget_histogram: Counter) -> dict:
    # NC-D precondition (fix-r1, boundary #3): the fallback construction
    # assumes exactly one context.budget per trace; the histogram comes from
    # the criterion-0 shape gate and is asserted here, not just narrated.
    if dict(budget_histogram) != {1: 300}:
        die(f"NC-D precondition: budget-per-trace histogram "
            f"{dict(sorted(budget_histogram.items()))} != {{1: 300}}")

    label, round_name, scenario = NC_BASE
    native_rel = f"./{label}/rounds/{round_name}/{scenario}.pilotdeck-current-native.jsonl"
    sidecar_rel = f"./{label}/rounds/{round_name}/{scenario}.pilotdeck-current-sidecar.jsonl"
    for rel in (native_rel, sidecar_rel):
        if rel not in corpus:
            die(f"negative-control base pair missing from the anchored corpus: {rel}")
    left = load_trace_from_bytes(clean, corpus[native_rel], scratch, "nc-base-native.jsonl")
    right = load_trace_from_bytes(clean, corpus[sidecar_rel], scratch, "nc-base-sidecar.jsonl")
    base_clean, base_patched = nc_result(clean, patched, left, right)
    if base_clean["fail"] or base_patched["fail"]:
        die("NC base pair must pass both comparators with zero semantic differences")

    raw_left = read_jsonl(corpus[native_rel])
    raw_right = read_jsonl(corpus[sidecar_rel])

    def find_index(records: list[dict], kind: str, event: str | None = None) -> int:
        for index, record in enumerate(records):
            if record.get("kind") == kind and (event is None or record.get("event") == event):
                return index
        wanted = f"{kind}({event})" if event else kind
        die(f"negative-control base pair has no {wanted} record: {sidecar_rel}")

    def with_sidecar(records: list[dict]) -> tuple[dict, dict]:
        sidecar_file = scratch / "nc-sidecar.jsonl"
        write_temp_trace(records, sidecar_file)
        mutated = clean.load_trace(sidecar_file)
        return nc_result(clean, patched, left, mutated)

    def with_both(left_records: list[dict], right_records: list[dict]) -> tuple[dict, dict]:
        native_file = scratch / "nc-native.jsonl"
        sidecar_file = scratch / "nc-sidecar.jsonl"
        write_temp_trace(left_records, native_file)
        write_temp_trace(right_records, sidecar_file)
        return nc_result(clean, patched, clean.load_trace(native_file), clean.load_trace(sidecar_file))

    controls: dict[str, dict] = {}

    # NC-A: value mutation inside one context.budget record.
    mutant = [dict(r) for r in raw_right]
    for record in mutant:
        if record.get("kind") == "context.budget":
            if isinstance(record.get("used"), bool) or not isinstance(record.get("used"), int):
                die("NC-A fixture: context.budget record has non-int 'used'")
            record["used"] = record["used"] + 1
            break
    else:
        die("NC-A fixture: no context.budget record in the base pair")
    c_clean, c_patched = with_sidecar(mutant)
    controls["NC-A value mutation"] = {
        "injection": "sidecar context.budget used += 1",
        "clean": c_clean,
        "patched": c_patched,
        "expected_row": "trace[#]~[#].used value difference present in patched",
    }
    controls["NC-A value mutation"]["killed"] = (
        c_patched["fail"]
        and any(p.endswith(".used") for p in c_patched["paths"])
    )

    # NC-B: count mutation (delete one context.budget record).
    mutant = [dict(r) for r in raw_right]
    del mutant[find_index(mutant, "context.budget")]
    c_clean, c_patched = with_sidecar(mutant)
    controls["NC-B count mutation"] = {
        "injection": "delete the sidecar context.budget record",
        "clean": c_clean,
        "patched": c_patched,
        "expected_row": "context.budget insertion row present in patched",
    }
    controls["NC-B count mutation"]["killed"] = (
        c_patched["fail"]
        and any(p == "trace[#]" for p in c_patched["paths"])
    )

    # NC-C: exemption boundary. Move durable.status(context_budget) past the
    # model.request boundary AND past durable.status(turn_timeout): a position
    # change the reviewed continuable comparator would forgive; strict mode
    # (clean and patched) must still fail.
    # anthropic boundary #3 (fix-r2): the fixture relocates *the*
    # durable.status(context_budget) record — if the base sidecar ever
    # carried more (or fewer) than one, find_index would pick the first and
    # the injection signal would be ambiguous. Measured on the anchored
    # corpus: exactly one; asserted, not narrated.
    ds_count = sum(1 for record in raw_right
                   if record.get("kind") == "durable.status"
                   and record.get("event") == "context_budget")
    if ds_count != 1:
        die(f"NC-C fixture: expected exactly 1 durable.status(context_budget) record "
            f"in the base sidecar, found {ds_count}")
    mutant = [dict(r) for r in raw_right]
    ds_index = find_index(mutant, "durable.status", "context_budget")
    request_index = find_index(mutant, "model.request")
    record = mutant.pop(ds_index)
    mutant.append(record)
    c_clean, c_patched = with_sidecar(mutant)
    continuable = patched.compare_continuable_trace_details(left, clean.load_trace(scratch / "nc-sidecar.jsonl"))
    controls["NC-C exemption boundary"] = {
        "injection": (
            "move durable.status(context_budget) from raw index "
            f"{ds_index} (before model.request at {request_index}) to the end: "
            "crosses the request boundary and the durable.status(turn_timeout) order"
        ),
        "clean": c_clean,
        "patched": c_patched,
        "continuable_on_mutant": "PASS" if not continuable.semantic else f"FAIL({len(continuable.semantic)})",
        "expected_row": "durable.status mismatch rows present in patched",
    }
    controls["NC-C exemption boundary"]["killed"] = (
        c_patched["fail"]
        and any(p == "trace[#]" or ".event" in p or ".statusKind" in p for p in c_patched["paths"])
    )

    # NC-C-literal (documented, not a gate): moving the record just past the
    # request boundary without crossing another durable record is invisible
    # to BOTH comparators: project_semantic_trace orders durable records only
    # within their own actor stream (pre-existing reviewed projection, not a
    # relaxation of this patch). Recorded as evidence, expected PASS.
    # fix-r2, DS boundary #1: the pop at ds_index shifts the first
    # model.request one slot left, so "immediately after the first
    # model.request" is slot request_index, not request_index + 1 (the old
    # slot landed past the model.response that follows the request, crossing
    # a boundary the control's own text says it does not cross).
    mutant = [dict(r) for r in raw_right]
    record = mutant.pop(ds_index)
    mutant.insert(request_index, record)
    # The record must now sit immediately after the first model.request:
    # the request shifted to request_index - 1 when ds_index was popped, and
    # the insert landed the record at request_index.
    if mutant[request_index] is not record \
            or mutant[request_index - 1].get("kind") != "model.request":
        die("NC-C-literal fixture: insert slot is not immediately after the first model.request")
    c_clean, c_patched = with_sidecar(mutant)
    controls["NC-C literal request-boundary only (documented)"] = {
        "injection": "move durable.status(context_budget) to immediately after the first model.request",
        "inserted_after_kind": mutant[request_index - 1].get("kind"),
        "clean": c_clean,
        "patched": c_patched,
        "note": (
            "invisible to clean and patched alike: durable records are ordered only "
            "within the durable actor stream of project_semantic_trace; the observable "
            "boundary (crossing durable.status(turn_timeout)) is covered by NC-C above"
        ),
        "killed": None,
    }

    # NC-D: order within the kind. Every evidence trace carries exactly one
    # context.budget record, so the documented fallback applies: add a second,
    # value-different budget record to BOTH sides, then swap the two on the
    # sidecar.
    def add_second(records: list[dict]) -> list[dict]:
        out: list[dict] = []
        added = False
        for record in records:
            out.append(record)
            if record.get("kind") == "context.budget" and not added:
                if (isinstance(record.get("used"), bool) or not isinstance(record.get("used"), int)
                        or isinstance(record.get("displayUsed"), bool) or not isinstance(record.get("displayUsed"), int)):
                    die("NC-D fixture: context.budget record has non-int 'used'/'displayUsed'")
                extra = dict(record)
                extra["used"] = record["used"] + 500
                extra["displayUsed"] = record["displayUsed"] + 500
                out.append(extra)
                added = True
        if not added:
            die("NC-D fixture: no context.budget record to duplicate in the base pair")
        return out

    two_left = add_second(raw_left)
    two_right = add_second(raw_right)
    base2_clean, base2_patched = with_both(two_left, two_right)
    swapped = [dict(r) for r in two_right]
    budget_index = [i for i, r in enumerate(swapped) if r.get("kind") == "context.budget"]
    if len(budget_index) != 2:
        die(f"NC-D fixture: expected exactly 2 context.budget records after add_second, found {len(budget_index)}")
    swapped[budget_index[0]], swapped[budget_index[1]] = swapped[budget_index[1]], swapped[budget_index[0]]
    c_clean, c_patched = with_both(two_left, swapped)
    controls["NC-D within-kind order"] = {
        "injection": (
            "swap the two context.budget records on the sidecar after adding a second, "
            "value-different budget record to BOTH sides (fallback construction: every "
            f"evidence trace has exactly {dict(sorted(budget_histogram.items()))} budget record(s))"
        ),
        "constructed_base": {"clean": base2_clean, "patched": base2_patched},
        "clean": c_clean,
        "patched": c_patched,
        "expected_row": "trace[#]~[#].used / .displayUsed order differences present in patched",
    }
    controls["NC-D within-kind order"]["killed"] = (
        base2_clean["fail"] is False
        and base2_patched["fail"] is False
        and c_patched["fail"]
        and any(p.endswith(".used") or p.endswith(".displayUsed") for p in c_patched["paths"])
    )

    # NC-E (documented, not a gate): the position waiver for context.budget
    # has no window bound — the patch pulls every context.budget out of the
    # positional walk, so moving one past terminal/lifecycle records is
    # exactly as invisible as moving it across the request boundary.
    # Recorded so the exemption width stays visible in every recompute;
    # narrowing it would change registered behavior, not fix a bug here.
    mutant = [dict(r) for r in raw_right]
    budget_pos = find_index(mutant, "context.budget")
    record = mutant.pop(budget_pos)
    mutant.append(record)
    c_clean, c_patched = with_sidecar(mutant)
    controls["NC-E budget moved past terminal state (documented)"] = {
        "injection": "move the sidecar context.budget record to the very end (past terminal/lifecycle records)",
        "clean": c_clean,
        "patched": c_patched,
        "note": (
            "exempt by design: context.budget is compared as a separate ordered-by-kind "
            "sequence, so its position is invisible to the patched comparator anywhere in "
            "the trace (registration Scope wording, fix-r1); a narrower window would be a "
            "registered-behavior change — tracked as a P2 follow-up in the registration"
        ),
        "killed": None,
    }

    gated = [name for name, control in controls.items() if control["killed"] is not None]
    # fix-r3, P2-b: not just non-empty (fix-r2, anthropic boundary #1) — the
    # exact four registered gated controls. One control's killed drifting to
    # None drops it from gated and would leave all() judging a smaller
    # denominator, so the name set itself is pinned.
    if sorted(gated) != sorted(EXPECTED_GATED):
        die(f"criterion4: gated controls {sorted(gated)} != expected {sorted(EXPECTED_GATED)}")
    passed = all(controls[name]["killed"] for name in gated)
    return {
        "name": "4 negative controls",
        "base_pair": f"{label}/rounds/{round_name}/{scenario}",
        "base_clean": base_clean,
        "base_patched": base_patched,
        "budget_records_per_trace_histogram": {str(k): v for k, v in sorted(budget_histogram.items())},
        "controls": controls,
        "gated_controls": gated,
        "killed": sum(1 for name in gated if controls[name]["killed"]),
        "passed": passed,
    }


def write_result_exclusive(path: Path, text: str) -> Path:
    """Create the result file exclusively; never overwrite an earlier run.

    If path already exists, the run goes to <stem>.attempt-N<suffix> (fix-r1,
    concurrency #2): a failed later run must not leave an earlier PASS
    looking current, and two concurrent runs must not clobber each other.
    """
    try:
        with path.open("x", encoding="utf-8") as handle:
            handle.write(text)
        return path
    except FileExistsError:
        pass
    attempt = 2
    while True:
        candidate = path.with_name(f"{path.stem}.attempt-{attempt}{path.suffix}")
        try:
            with candidate.open("x", encoding="utf-8") as handle:
                handle.write(text)
            return candidate
        except FileExistsError:
            attempt += 1


def _sits_in_evidence(path: Path, evidence: Path) -> bool:
    """Identity, not spelling (fix-r5, review #2372 r4 P1 — MiniMax sibling#2 +
    boundary#1 + DS boundary#1, one cluster, coordinator-reproduced; start
    point physicalized in fix-r6, review #2372 r5 P1 — DS concurrency#1 +
    boundary#1, coordinator-reproduced): the three lexical checks below
    compare strings, and on a case-insensitive volume (darwin's default APFS)
    two spellings of one directory make all of them False while the write
    still lands inside the evidence tree. os.path.samefile compares by
    filesystem identity, so the nearest existing ancestor chain of the start
    point is walked level by level: a `--out` file that does not exist yet (or
    sits under directories that do not exist yet — main() only mkdirs its
    parent at write time) climbs until a level exists, and any existing level
    that IS the evidence tree means the write lands inside it, at any depth
    (EvD/evidence/sub/out.json matches only two levels up). A level that
    still cannot be compared for any other reason (ELOOP, EACCES, ...) counts
    as inside — fail-closed, the same identity-not-spelling discipline as the
    sys.path check above (#1033).
    fix-r6: the walk used to start from the literal `path` spelling, so a
    `..` over a not-yet-existing directory never reached the evidence level
    (the write-stage mkdir(parents=True) then folded the `..` back into the
    tree — scenario A) and a relative `--out` with the cwd inside the tree
    never climbed above Path('.') (scenario B). The start point is now the
    PHYSICAL spelling: os.path.realpath(path) directly (fix-r7) anchors the
    path to the cwd and resolves components left to right, following a symlink
    before folding a subsequent `..` (the kernel open order). abspath must
    not run first: its lexical normalization would discard `link/..` before
    the link is resolved, so the walk could miss the evidence tree (a symlink loop
    yields the loop spelling, which the OSError branch below still fails
    closed on; an embedded NUL raises ValueError, caught by the guard's
    except), so every start-normalized variant — case aliases, `..` over
    missing levels, links landing inside the tree — reduces to the real
    ancestor chain before the samefile walk."""
    anchor = Path(os.path.realpath(path))
    q = anchor if anchor.exists() else anchor.parent
    while True:
        try:
            if os.path.samefile(q, evidence):
                return True
        except FileNotFoundError:
            # This spelling does not exist yet: step up one level towards the
            # nearest existing ancestor (the level that exists decides the
            # identity; the fix-r5 claim that the lexical checks cover the
            # exactly-spelled case was wrong for `..` over missing levels —
            # realpath has folded those away here already, fix-r6).
            if q.parent == q:
                return True  # nothing existing to compare against — fail-closed
            q = q.parent
            continue
        except OSError:
            return True  # cannot compare (ELOOP/EACCES/...) — fail-closed
        if q.parent == q:
            return False
        q = q.parent


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--baseline", required=True, type=Path)
    parser.add_argument("--patch", required=True, type=Path)
    parser.add_argument("--evidence", required=True, type=Path)
    parser.add_argument("--manifest", type=Path,
                        help=f"corpus manifest (default: {MANIFEST_FILENAME} next to this script)")
    parser.add_argument("--out", type=Path)
    args = parser.parse_args()

    for name, path in (("--baseline", args.baseline), ("--patch", args.patch), ("--evidence", args.evidence)):
        if not path.exists():
            die(f"{name} does not exist: {path}")
    # The apply steps run inside a temporary repo; every input path must be
    # absolute so it still resolves from there.
    args.baseline = args.baseline.resolve()
    args.patch = args.patch.resolve()
    args.evidence = args.evidence.resolve()

    script = Path(__file__).resolve()
    registration = script.parent / REGISTRATION_FILENAME

    # fix-r3, P2-a / fix-r4, P1 (review #2372, DS r3 concurrency#3 + MiniMax
    # r3 boundary#2, coordinator e2e reproduced): criterion 0 pins the exact
    # file set, so --out (or a --manifest copy) inside --evidence would poison
    # the anchored tree — the first run writes it, the next recompute reports
    # it as an unexpected file and fake-FAILs. The fix-r3 resolve()-only check
    # judged the link's TARGET while the write goes to the LITERAL parent
    # (POSIX O_CREAT|O_EXCL over a symlink is EEXIST, so the attempt-N file
    # lands next to the link) — three checks now, any hit dies: the literal
    # parent directory (main check, the actual write location), the symlink
    # shape itself (is_symlink() is False for a nonexistent path, never
    # raises), and the resolve() target (kept from fix-r3). The --manifest
    # default branch is already resolved; walking the same guard is harmless.
    # fix-r5, P1 + P2 (review #2372 r4, MiniMax sibling#2 + boundary#1 + DS
    # boundary#1 same cluster, coordinator-reproduced): those three checks are
    # all LEXICAL (spelling) comparisons — on a case-insensitive volume (darwin
    # default APFS) two spellings of one directory make them all False while
    # the write lands inside the evidence tree. A fourth check compares
    # FILESYSTEM IDENTITY (_sits_in_evidence: samefile over the nearest
    # existing ancestor chain), and the whole judgement is fail-closed: any
    # OSError it raises (e.g. resolve() over a symlink loop, on the Python
    # versions where it raises) dies at entry, never a bare traceback.
    # fix-r6, P1 + P2 (review #2372 r5, DS concurrency#1 + boundary#1 +
    # sibling#1/concurrency#2/boundary#2 + MM boundary#2, coordinator
    # reproduced both chains): the identity walk started from the LITERAL
    # path spelling, so a `..` over a not-yet-existing directory and a
    # relative --out with the cwd inside the tree both stayed off the
    # ancestor chain while the write still landed inside. _sits_in_evidence
    # now starts from the physicalized spelling (realpath(path), direct since
    # fix-r7: follow links before folding subsequent `..`, anchoring cwd too); the
    # manifest default-branch resolve() moved into this same fail-closed zone
    # (a symlink loop raises RuntimeError on Python 3.11/3.12, not OSError;
    # an embedded NUL raises ValueError on every version), and a degenerate
    # --out (a directory, or a parent that is a regular file) dies at entry
    # instead of surfacing as a write-stage traceback.
    # Asserted at entry, before any pin or criterion runs, never narrated
    # only in the README.
    try:
        manifest = (args.manifest or script.parent / MANIFEST_FILENAME).resolve()
    except (OSError, RuntimeError, ValueError) as error:
        die(f"--manifest placement guard could not resolve {args.manifest}: {error!r} — "
            f"treating it as inside (fail-closed; criterion 0 pins the exact file set)")
    for name, path in (("--out", args.out), ("--manifest", manifest)):
        try:
            inside = path is not None and (
                path.is_symlink()
                or path.resolve().is_relative_to(args.evidence)
                or path.parent.resolve().is_relative_to(args.evidence)
                or _sits_in_evidence(path, args.evidence)
            )
        except (OSError, RuntimeError, ValueError) as error:
            die(f"{name} placement guard could not resolve {path}: {error!r} — treating it "
                f"as inside (fail-closed; criterion 0 pins the exact file set)")
        if inside:
            die(f"{name} must not sit inside or resolve inside the --evidence tree: {path} "
                f"(criterion 0 pins the exact file set)")

    # fix-r6, P2 (review #2372 r5, MM boundary#2): degenerate --out spellings
    # used to surface as bare write-stage tracebacks — an existing directory
    # (argparse maps both "" and "." to Path(".")) died in
    # write_result_exclusive's open("x") with IsADirectoryError, and a parent
    # that is a regular file died in the mkdir. Both now die at entry, after
    # the placement guard and before any pin or criterion runs.
    if args.out is not None:
        if args.out.is_dir():
            die(f"--out must name a file, not an existing directory: {args.out}")
        if args.out.parent.is_file():
            die(f"--out parent exists and is a regular file: {args.out.parent} "
                f"(--out must name a file under a directory)")

    # Registration pins (fix-r1): the patch file, this script and the corpus
    # manifest are each sha256-anchored by the registration file, which is
    # itself the append-only, human-reviewed anchor of the directory.
    # Validate-once (fix-r2, P1-a): each of the three is read exactly once —
    # the manifest entries below are parsed from the very bytes the Corpus:
    # pin verified, and criterion 1 applies the very bytes the Patch: pin
    # verified (git reads them from stdin) — so none of the three files is
    # ever opened again after its check.
    pins = parse_registration(registration)
    self_sha = sha256_bytes(script.read_bytes())
    if self_sha != pins["verify"]:
        die(f"verify script sha256 {self_sha} != registration Verify: pin {pins['verify']}")
    patch_bytes = args.patch.read_bytes()
    patch_sha = sha256_bytes(patch_bytes)
    if patch_sha != pins["patch"]:
        die(f"patch file {args.patch} sha256 {patch_sha} != registration Patch: pin {pins['patch']}")
    if not manifest.exists():
        die(f"corpus manifest does not exist: {manifest}")
    manifest_bytes = manifest.read_bytes()
    manifest_sha = sha256_bytes(manifest_bytes)
    if manifest_sha != pins["corpus"]:
        die(f"corpus manifest {manifest} sha256 {manifest_sha} != registration Corpus: pin {pins['corpus']}")
    entries = parse_manifest(manifest, manifest_bytes)

    criterion_0, budget_histogram, corpus = criterion0(args.evidence, entries)
    pairs = discover_pairs(corpus)
    criterion_1, clean_bytes, patched_bytes = criterion1(args.baseline, patch_bytes)
    criterion_3 = criterion3(corpus, pairs)

    with tempfile.TemporaryDirectory(prefix="pdrs-1841-verify-") as tmp:
        scratch = Path(tmp)
        # Load both comparators from the bytes criterion 1 already hash-verified
        # (fix-r1, concurrency #1): the baseline path is never read again, so
        # nothing that happens to it after the check can change what runs.
        clean_file = scratch / "clean-trace.py"
        patched_file = scratch / "patched-trace.py"
        clean_file.write_bytes(clean_bytes)
        patched_file.write_bytes(patched_bytes)
        clean = load_module(clean_file, "pdrs1841_clean")
        patched = load_module(patched_file, "pdrs1841_patched")
        if clean is patched or clean.compare_trace_details is patched.compare_trace_details:
            die("clean and patched modules must not alias")

        criterion_2 = criterion2(
            pairs, clean, patched, corpus, scratch,
            census_deviations=set(criterion_3["deviation_pairs"]),
        )
        criterion_4 = criterion4(corpus, clean, patched, scratch, budget_histogram)

    result = {
        "issue": 1841,
        "device_patch": "specs/parity/device-patches/20261108-1841-trace-context-budget-position.patch",
        "baseline": str(args.baseline),
        "baseline_sha256": CLEAN_SHA256,
        "patch_file": str(args.patch),
        "patch_file_sha256": patch_sha,
        "evidence": str(args.evidence),
        "evidence_manifest": str(manifest),
        "evidence_manifest_sha256": manifest_sha,
        "registration": str(registration),
        "verify_script_sha256": self_sha,
        "criteria": [criterion_0, criterion_1, criterion_2, criterion_3, criterion_4],
    }
    result["passed"] = all(criterion["passed"] for criterion in result["criteria"])

    if args.out:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        out_path = write_result_exclusive(args.out, json.dumps(result, ensure_ascii=False, indent=1) + "\n")
        if out_path != args.out:
            print(f"note: {args.out} already exists; wrote {out_path} instead (earlier runs are never overwritten)")

    print("#1841 device-patch verify")
    for criterion in result["criteria"]:
        print(f"- {criterion['name']}: {'PASS' if criterion['passed'] else 'FAIL'}")
    if not result["passed"]:
        for criterion in result["criteria"]:
            if not criterion["passed"]:
                print(json.dumps(criterion, ensure_ascii=False, indent=1))
    print(f"overall: {'PASS' if result['passed'] else 'FAIL'}")
    raise SystemExit(0 if result["passed"] else 1)


if __name__ == "__main__":
    main()
