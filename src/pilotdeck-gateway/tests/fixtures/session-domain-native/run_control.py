# anchor-selfcheck: unsupported single-control runner for the frozen native deadline apparatus; the source mutation table lives in negative_controls.py
#!/usr/bin/env python3
"""#1140: run one native deadline control in a fresh interpreter.

Loads the working-tree controls entry by explicit path (never sys.path) and
reports the verdict shape of the controls CLI.  ``negative_controls.py`` binds
its ``session_domain.py`` source mutations to these runs: unmutated, the
named control must fail with its registered rule; with one #1140 require
deleted or bypassed, the same control must change its verdict exactly as the
suite's mutant table expects.

This runner deliberately does not go through the controls CLI ``main()``:
``verify_loaded_apparatus`` compares the loaded ``session_domain`` bytes with
HEAD, which by construction differs while a mutant is on disk.  Binding to
``check_case`` (the entry ``main()`` itself calls) is the established shape
(#209 session-domain-lock-release, #1177 session-domain-expectations).  No
cargo is launched: the controls replay synthetic capture fixtures offline.
"""
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import traceback

ROOT = Path(__file__).resolve().parents[5]
ENTRY = ROOT / 'scripts/gateway-parity/controls/session_domain.py'

# Loading the controls entry by path would drop a __pycache__ directory next
# to it inside the frozen apparatus; check_frozen scans that tree and flags
# unregistered files, so never write bytecode here.
sys.dont_write_bytecode = True


def _load(name, path):
    """Load one module by explicit path; never resolve via sys.path."""
    path = Path(path).resolve()
    existing = sys.modules.get(name)
    if existing is not None and getattr(existing, '__file__', None) \
            and Path(existing.__file__).resolve() == path:
        return existing
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    try:
        spec.loader.exec_module(module)
    except BaseException:
        sys.modules.pop(name, None)
        raise
    if Path(module.__file__).resolve() != path:
        raise RuntimeError(f'{name}: loaded {module.__file__}, expected {path}')
    return module


def main():
    if len(sys.argv) != 3:
        print('usage: run_control.py <case> <output>', file=sys.stderr)
        return 2
    name, output = sys.argv[1], Path(sys.argv[2])
    panel = _load('native_deadline_controls', ENTRY)
    # Synthetic native/audit controls only: the calibration and expectation
    # families need a real A5.2a --source the source mutations never touch.
    if name not in panel.CASES or name in panel.CALIBRATION_CASES \
            or name in panel.EXPECTATION_CASES:
        print(f'unknown synthetic control: {name}', file=sys.stderr)
        return 2
    output.mkdir(parents=True, exist_ok=True)
    try:
        panel.check_case(name, output)
    except (ValueError, OSError, KeyError, TypeError, StopIteration,
            subprocess.SubprocessError) as error:
        # The controls CLI's own catch set; EvidenceError carries its rule.
        result = {'case': name, 'status': 'FAIL',
                  'rule': getattr(error, 'rule', 'CONTROL_EXECUTION'),
                  'detail': str(error)}
        print(json.dumps(result, ensure_ascii=False))
        return 1
    print(json.dumps({'case': name, 'status': 'PASS', 'rule': None},
                     ensure_ascii=False))
    return 0


if __name__ == '__main__':
    try:
        sys.exit(main())
    except SystemExit:
        raise
    except BaseException:
        # Unexpected harness failures are INVALID evidence, never a kill.
        traceback.print_exc()
        sys.exit(3)
