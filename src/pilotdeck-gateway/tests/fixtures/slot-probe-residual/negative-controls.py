#!/usr/bin/env python3
"""#2516 residual controls: four mutants of the model-complete slot probe.

Each control re-introduces one of the defects the issue fixed in
src/pilotdeck-gateway/tests/fixtures/model-complete/negative-controls.py
(items 1-4: the strip()ed rc=1 stderr gate, the single-sweep pgrep zero, the
text=True ps sample, the numeral-quantified probe comment) and is killed by
a named probe test. The tests drive the REAL subprocess machinery: fake
`ps` / `pgrep` executables are generated per run into a system temporary
directory and put first on PATH, so the probe under test spawns real
children through real pipes -- under the text=True mutant the actual
stdlib universal-newline translation does the row tearing.

Mutants are never written into the tree. Each one is applied to an
in-memory publication of the probe module through
negative_process.published_module (the shared helper for controls that
compile a candidate without touching the disk), so the apply -> kill ->
restore cycle is publish -> assertion failure -> unpublish, and the runner
proves at the end that the probe file's bytes on disk never changed. No
cargo is launched anywhere, so no CARGO_TARGET_DIR is required.

  python3 negative-controls.py                  # full run: baseline + kills
  python3 negative-controls.py --check-anchors  # anchor preflight only
  python3 negative-controls.py --smoke          # real ps/pgrep, no fakes
  python3 negative-controls.py --probe-tests [--mutant NAME] TEST...
      internal mode used by the orchestration above; each test prints one
      PASS / FAIL line, and a FAIL line is single-line evidence carrying
      the control's "#2516-<n>" marker in its assertion message.
"""
from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import traceback
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import published_module, run_owned  # noqa: E402

ROOT = Path(__file__).resolve().parents[5]
NEGCTL = ROOT / "src/pilotdeck-gateway/tests/fixtures/model-complete/negative-controls.py"
SCRIPT = Path(__file__).resolve()
MODULE_NAME = "pdrs_2516_probe_under_test"

# (name, path, fixed snippet, mutant snippet, killing test, kill marker).
# The fixed snippet must occur exactly once in the probe file; a stale
# anchor aborts the run (exit 2) instead of being recorded as SURVIVED.
CONTROLS = [
    (
        # #2516 item 1: rc=1 stderr must be judged on the raw string, like
        # the stdout gate in the same branch -- whitespace-only stderr is
        # "someone spoke", never a clean no-match.
        "pgrep-stderr-strip-restored",
        NEGCTL,
        """                if stderr:
                    return None, f"pgrep -x {name} exit 1 with stderr: {stderr!r}"
""",
        """                if stderr.strip():
                    return None, f"pgrep -x {name} exit 1 with stderr: {stderr!r}"
""",
        "pgrep-stderr-whitespace-fails",
        "#2516-1",
    ),
    (
        # #2516 item 2: zero is released only after two consecutive clean
        # whole-table sweeps; one sweep misses an entry point that started
        # after the sweep had already probed its name.
        "pgrep-single-sweep-restored",
        NEGCTL,
        """    for _sweep in (1, 2):
""",
        """    for _sweep in (1,):
""",
        "pgrep-second-sweep-sees-late-cargo",
        "#2516-2",
    ),
    (
        # #2516 item 3: the ps sample must be captured as bytes and decoded
        # here; text=True folds a literal \r into \n and tears one row into
        # two, and the torn tail can forge a parent-map entry.
        "ps-text-mode-restored",
        NEGCTL,
        """        result = subprocess.run(
            ["ps", "-Aww", "-o", "pid=,ppid=,args="], capture_output=True
        )
    except OSError as error:
        return None, f"ps failed: {error}"
    if result.returncode != 0:
        return None, (
            f"ps exit {result.returncode}: "
            f"{result.stderr.decode('utf-8', 'replace').strip()}"
        )
    stdout = result.stdout.decode("utf-8", "surrogateescape")
    if not stdout.strip():
        return None, "ps output empty"
    if re.search(CONTROL_CHARACTERS, stdout):
        # #2516 item 3: a control byte here (a \\r most plausibly) means the
        # row structure is not what the parser assumes -- under text=True
        # that \\r would already have torn a row and forged a parent-map
        # entry. Fail closed, never read the sample as a zero count.
        return None, "ps output contains control characters"
""",
        """        result = subprocess.run(
            ["ps", "-Aww", "-o", "pid=,ppid=,args="], capture_output=True, text=True
        )
    except OSError as error:
        return None, f"ps failed: {error}"
    if result.returncode != 0:
        return None, f"ps exit {result.returncode}: {result.stderr.strip()}"
    stdout = result.stdout
    if not stdout.strip():
        return None, "ps output empty"
""",
        "ps-attack-control-char-fails",
        "#2516-3",
    ),
    (
        # #2516 item 4: probe comments must not quantify the name list
        # ("ten clean no-matches" was already forced from nine to ten once).
        "numeral-comment-restored",
        NEGCTL,
        """                # whitespace-only) and exits 1 is a probe failure too,
                # never a clean no-match sweep -> 0.
""",
        """                # whitespace-only) and exits 1 is a probe failure too,
                # never ten clean no-matches -> 0.
""",
        "probe-comments-drop-numerals",
        "#2516-4",
    ),
]

FAKE_PS = '''#!/usr/bin/env python3
"""#2516 slot-probe control fake (generated into a temp dir, never committed).

Prints the scripted sample from the JSON spec so the probe under test keeps
using the real pipe machinery; {CALLER_PID} is substituted with the calling
process pid, satisfying the #1770 fix-r2 caller invariant.
"""
import base64
import json
import os
import sys

sample = json.loads(open(os.environ["PDRS_2516_FAKE_SPEC"]).read())["ps"]
out = base64.b64decode(sample["stdout_b64"])
out = out.replace(b"{CALLER_PID}", str(os.getppid()).encode())
sys.stdout.buffer.write(out)
sys.stdout.buffer.flush()
sys.stderr.buffer.write(base64.b64decode(sample["stderr_b64"]))
sys.stderr.buffer.flush()
sys.exit(sample["rc"])
'''

FAKE_PGREP = '''#!/usr/bin/env python3
"""#2516 slot-probe control fake (generated into a temp dir, never committed).

Stateful by invocation count per name: the JSON spec maps a pgrep name to a
list of responses consumed in order (the last one repeats), so two-sweep
semantics can be probed deterministically.
"""
import base64
import json
import os
import sys

spec = json.loads(open(os.environ["PDRS_2516_FAKE_SPEC"]).read())["pgrep"]
name = sys.argv[sys.argv.index("-x") + 1]
counter = os.path.join(
    os.path.dirname(os.environ["PDRS_2516_FAKE_SPEC"]), "pgrep-" + name
)
seen = 0
if os.path.exists(counter):
    seen = int(open(counter).read())
open(counter, "w").write(str(seen + 1))
responses = spec.get(name, spec["default"])
response = responses[min(seen, len(responses) - 1)]
sys.stdout.buffer.write(base64.b64decode(response["stdout_b64"]))
sys.stdout.buffer.flush()
sys.stderr.buffer.write(base64.b64decode(response["stderr_b64"]))
sys.stderr.buffer.flush()
sys.exit(response["rc"])
'''


def check(condition, message):
    """Assert with a dedicated single-line marker message."""
    if not condition:
        raise AssertionError(message)


def ps_response(rc, stdout=b"", stderr=b""):
    return {"rc": rc, "stdout_b64": base64.b64encode(stdout).decode(),
            "stderr_b64": base64.b64encode(stderr).decode()}


def pgrep_response(rc, stdout=b"", stderr=b""):
    return {"rc": rc, "stdout_b64": base64.b64encode(stdout).decode(),
            "stderr_b64": base64.b64encode(stderr).decode()}


CLEAN_NOMATCH = pgrep_response(1)

# One genuine-looking table: two cargo trees (a clippy chain rooted at the
# cargo-clippy process, and a plain cargo), plus the caller's own row.
BENIGN_TABLE = (
    "{CALLER_PID} 1 python3 probe-tests\n"
    "900 800 cargo clippy\n"
    "800 700 cargo-clippy\n"
    "700 600 rustc --crate-name probe\n"
    "600 1 /bin/login\n"
    "950 1 cargo build\n"
)

# The item-3 attack table: the \r tears the hostile row under text=True and
# the torn tail "999 800 forged" reparents pid 999 onto the cargo pid 800,
# so the cargo's ancestor walk reaches a cargo (itself) and the tree stops
# being counted -- a full slot reads 0.
ATTACK_TABLE = (
    "{CALLER_PID} 1 python3 probe-tests\n"
    "800 999 cargo build\n"
    "700 1 /bin/hostile a\r999 800 forged\n"
)

INVALID_UTF8_TABLE = (
    b"{CALLER_PID} 1 python3 probe-tests\n"
    b"710 1 weird \xff\xfe argv\n"
)


def t_ps_tree_counts_roots(module, _source):
    result = module.slots_from_ps()
    check(result == (2, ""),
          f"#2516-pin two cargo trees must count 2 roots, got {result!r}")


def t_ps_attack_control_char(module, _source):
    result = module.slots_from_ps()
    check(result[0] is None,
          f"#2516-3 a \\r-bearing ps sample must fail the probe, got {result!r}")
    check("control" in result[1],
          f"#2516-3 the failure must name the control characters, got {result[1]!r}")


def t_ps_caller_pid_required(module, _source):
    result = module.slots_from_ps()
    check(result[0] is None and "caller" in result[1],
          f"#2516-pin a parsed table lacking the caller pid must fail, got {result!r}")


def t_ps_zero_parseable_rows(module, _source):
    result = module.slots_from_ps()
    check(result[0] is None and "parseable" in result[1],
          f"#2516-pin a nonempty sample with no parseable rows must fail, got {result!r}")


def t_ps_invalid_utf8_argv(module, _source):
    result = module.slots_from_ps()
    check(result == (0, ""),
          f"#2516-pin invalid UTF-8 args must decode to surrogates, not fail, got {result!r}")


def t_pgrep_stderr_whitespace_fails(module, _source):
    result = module.slots_from_pgrep()
    check(result[0] is None and "stderr" in result[1],
          f"#2516-1 rc=1 with whitespace-only stderr must fail the probe, got {result!r}")


def t_pgrep_second_sweep_sees_late_cargo(module, _source):
    result = module.slots_from_pgrep()
    check(result == (1, ""),
          f"#2516-2 a name alive only in the second sweep must read 1, got {result!r}")


def t_pgrep_both_sweeps_clean(module, _source):
    result = module.slots_from_pgrep()
    check(result == (0, ""),
          f"#2516-pin two clean whole-table sweeps must read 0, got {result!r}")


def t_pgrep_first_sweep_alive(module, _source):
    result = module.slots_from_pgrep()
    check(result == (1, ""),
          f"#2516-pin a first-sweep match must read 1, got {result!r}")


def t_pgrep_rc0_without_pid_row(module, _source):
    result = module.slots_from_pgrep()
    check(result[0] is None and "pid row" in result[1],
          f"#2516-pin rc=0 without a pid row must fail the probe, got {result!r}")


def t_pgrep_rc1_with_stdout(module, _source):
    result = module.slots_from_pgrep()
    check(result[0] is None and "stdout" in result[1],
          f"#2516-pin rc=1 with stdout must fail the probe, got {result!r}")


def t_pgrep_output_control_char(module, _source):
    result = module.slots_from_pgrep()
    check(result[0] is None and "control" in result[1],
          f"#2516-pin control bytes in pgrep output must fail the probe, got {result!r}")


def t_probe_comments_drop_numerals(_module, source):
    check(b"ten clean" not in source,
          "#2516-4 numeral-quantified probe wording must stay removed ('ten clean' found)")
    check(b"nine clean" not in source,
          "#2516-4 numeral-quantified probe wording must stay removed ('nine clean' found)")


PROBE_TESTS = [
    ("ps-attack-control-char-fails", {"ps": ps_response(0, ATTACK_TABLE.encode())},
     t_ps_attack_control_char),
    ("ps-tree-counts-roots", {"ps": ps_response(0, BENIGN_TABLE.encode())},
     t_ps_tree_counts_roots),
    ("ps-caller-pid-required",
     {"ps": ps_response(0, b"800 999 cargo build\n700 1 /bin/plain\n")},
     t_ps_caller_pid_required),
    ("ps-zero-parseable-rows", {"ps": ps_response(0, b"diagnostics only\n")},
     t_ps_zero_parseable_rows),
    ("ps-invalid-utf8-argv", {"ps": ps_response(0, INVALID_UTF8_TABLE)},
     t_ps_invalid_utf8_argv),
    ("pgrep-stderr-whitespace-fails",
     {"pgrep": {"default": [pgrep_response(1, stderr=b"  \n")]}},
     t_pgrep_stderr_whitespace_fails),
    ("pgrep-second-sweep-sees-late-cargo",
     {"pgrep": {"default": [CLEAN_NOMATCH],
                "cargo": [CLEAN_NOMATCH, pgrep_response(0, b"4242\n")]}},
     t_pgrep_second_sweep_sees_late_cargo),
    ("pgrep-both-sweeps-clean",
     {"pgrep": {"default": [CLEAN_NOMATCH]}},
     t_pgrep_both_sweeps_clean),
    ("pgrep-first-sweep-alive",
     {"pgrep": {"default": [CLEAN_NOMATCH], "cargo": [pgrep_response(0, b"4242\n")]}},
     t_pgrep_first_sweep_alive),
    ("pgrep-rc0-without-pid-row",
     {"pgrep": {"default": [pgrep_response(0, b"not-a-pid\n")]}},
     t_pgrep_rc0_without_pid_row),
    ("pgrep-rc1-with-stdout",
     {"pgrep": {"default": [pgrep_response(1, b"leak\n")]}},
     t_pgrep_rc1_with_stdout),
    ("pgrep-output-control-char",
     {"pgrep": {"default": [pgrep_response(1, stderr=b"a\rb")]}},
     t_pgrep_output_control_char),
    ("probe-comments-drop-numerals", {}, t_probe_comments_drop_numerals),
]


def control_by_name(name):
    for row in CONTROLS:
        if row[0] == name:
            return row
    return None


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def anchor_preflight(source):
    """Every fixed snippet must occur exactly once; stale anchors abort."""
    text = source.decode("utf-8")
    stale = []
    for name, _path, before, _after, _test, _marker in CONTROLS:
        count = text.count(before)
        if count != 1:
            stale.append((name, count))
    if stale:
        for name, count in stale:
            print(f"STALE ANCHOR {name}: {count} matches in the probe file")
        print("STALE ANCHORS: negative-controls.py is out of sync with the probe; refusing to run")
        return False
    print(f"{len(CONTROLS)} anchors: ok")
    return True


def mutated_source(source, control):
    text = source.decode("utf-8")
    _name, _path, before, after, _test, _marker = control
    return text.replace(before, after, 1).encode("utf-8")


def probe_tests_mode(mutant, names):
    """Internal mode: run the named probe tests, optionally on a mutant."""
    tests = {row[0]: row for row in PROBE_TESTS}
    unknown = [name for name in names if name not in tests]
    if unknown or not names:
        print(f"probe-tests: unknown or empty test selection: {names!r}", file=sys.stderr)
        return 2
    disk_before = NEGCTL.read_bytes()
    source = disk_before
    if mutant is not None:
        control = control_by_name(mutant)
        if control is None:
            print(f"probe-tests: unknown control {mutant!r}", file=sys.stderr)
            return 2
        if not anchor_preflight(disk_before):
            return 2
        source = mutated_source(disk_before, control)
        try:
            compile(source, str(NEGCTL), "exec")
        except SyntaxError as error:
            print(f"probe-tests: mutant {mutant!r} does not compile: {error}", file=sys.stderr)
            return 2
        print(f"published mutant {mutant} (sha256={sha256(source)})")
    fake_dir = Path(tempfile.mkdtemp(prefix="pdrs-2516-fakes-"))
    spec_path = fake_dir / "spec.json"
    previous_path = os.environ.get("PATH", "")
    try:
        for name, body in (("ps", FAKE_PS), ("pgrep", FAKE_PGREP)):
            script = fake_dir / name
            script.write_text(body)
            script.chmod(0o755)
        os.environ["PATH"] = f"{fake_dir}:{previous_path}"
        os.environ["PDRS_2516_FAKE_SPEC"] = str(spec_path)
        failures = 0
        with published_module(MODULE_NAME, NEGCTL, source) as module:
            for name in names:
                spec_path.write_text(json.dumps(tests[name][1]))
                for leftover in fake_dir.glob("pgrep-*"):
                    leftover.unlink()
                try:
                    tests[name][2](module, source)
                except AssertionError as error:
                    print(f"FAIL {name}: AssertionError: {error}")
                    failures += 1
                except BaseException as error:  # noqa: BLE001 - reported, never silent
                    print(f"ERROR {name}: {type(error).__name__}: {error}")
                    traceback.print_exc()
                    failures += 1
                else:
                    print(f"PASS {name}")
        unchanged = NEGCTL.read_bytes() == disk_before
        print(f"probe file bytes unchanged: {unchanged}")
        if not unchanged:
            return 2
        return 1 if failures else 0
    finally:
        os.environ["PATH"] = previous_path
        if "PDRS_2516_FAKE_SPEC" in os.environ:
            del os.environ["PDRS_2516_FAKE_SPEC"]
        shutil.rmtree(fake_dir, ignore_errors=True)


def smoke_mode():
    """Real ps/pgrep through the published pristine module; no fakes."""
    source = NEGCTL.read_bytes()
    with published_module(MODULE_NAME, NEGCTL, source) as module:
        count, error = module.slots_from_ps()
        print(f"slots_from_ps()   -> count={count!r} error={error!r}")
        count, error = module.slots_from_pgrep()
        print(f"slots_from_pgrep() -> count={count!r} error={error!r}")
    return 0


def child_command(extra):
    return [sys.executable, "-I", str(SCRIPT), *extra]


def full_run():
    source = NEGCTL.read_bytes()
    print(f"probe file: {NEGCTL.relative_to(ROOT)}")
    print(f"probe file sha256: {sha256(source)}")
    if not anchor_preflight(source):
        return 2
    baseline_names = [row[0] for row in PROBE_TESTS]
    result = run_owned(child_command(["--probe-tests", *baseline_names]), timeout=300)
    print(result.stdout, end="")
    if result.stderr:
        print(result.stderr, end="", file=sys.stderr)
    passed = [line[5:] for line in result.stdout.split("\n")
              if line.startswith("PASS ")]
    missing = [name for name in baseline_names if name not in passed]
    if result.returncode != 0 or missing:
        print(f"BASELINE FAILED (exit {result.returncode}, missing {missing})")
        return 1
    print(f"baseline: {len(passed)}/{len(baseline_names)} probe tests PASS")
    verdicts = []
    for control in CONTROLS:
        name, _path, _before, _after, test, marker = control
        mutant = mutated_source(source, control)
        try:
            compile(mutant, str(NEGCTL), "exec")
        except SyntaxError as error:
            print(f"{name}: INVALID (mutant does not compile: {error})")
            verdicts.append("INVALID")
            continue
        result = run_owned(
            child_command(["--probe-tests", "--mutant", name, test]), timeout=300)
        output = result.stdout
        print(output, end="")
        if result.stderr:
            print(result.stderr, end="", file=sys.stderr)
        killed = result.returncode != 0 and any(
            line.startswith(f"FAIL {test}: AssertionError:") and marker in line
            for line in output.split("\n"))
        if killed:
            verdicts.append("KILLED")
            print(f"VERDICT {name}: KILLED by {test} ({marker})")
        elif result.returncode == 0:
            verdicts.append("SURVIVED")
            print(f"VERDICT {name}: SURVIVED ({test} still passes on the mutant)")
        else:
            verdicts.append("INVALID")
            print(f"VERDICT {name}: INVALID (exit {result.returncode}, no clean "
                  f"'{marker}' assertion failure)")
    unchanged = NEGCTL.read_bytes() == source
    print(f"probe file bytes unchanged after the run: {unchanged}")
    if not unchanged:
        return 1
    killed = verdicts.count("KILLED")
    print(f"NEGATIVE CONTROLS {'PASS' if killed == len(CONTROLS) else 'FAILED'} "
          f"({killed}/{len(CONTROLS)})")
    return 0 if killed == len(CONTROLS) and unchanged else 1


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check-anchors", action="store_true",
                        help="verify every control anchor occurs exactly once")
    parser.add_argument("--smoke", action="store_true",
                        help="run the real probes with no fakes (evidence only)")
    parser.add_argument("--probe-tests", action="store_true",
                        help="internal: run the named probe tests")
    parser.add_argument("--mutant", metavar="CONTROL",
                        help="internal: publish this control's mutant first")
    parser.add_argument("tests", nargs="*", help="probe test names")
    args = parser.parse_args(argv)
    if args.check_anchors:
        return 0 if anchor_preflight(NEGCTL.read_bytes()) else 2
    if args.smoke:
        return smoke_mode()
    if args.probe_tests:
        return probe_tests_mode(args.mutant, args.tests)
    return full_run()


if __name__ == "__main__":
    sys.exit(main())
