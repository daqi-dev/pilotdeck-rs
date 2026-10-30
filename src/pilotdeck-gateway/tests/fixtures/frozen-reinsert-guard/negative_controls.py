#!/usr/bin/env python3
"""#1066 paired source controls, after the coordinator commits this candidate.

All mutations use the real checkout under mutating_sources (fixed HEAD and
gateway parity lease). No shadow repository, source-loader patch or cargo is
used. Python compile is the build check. --check-anchors only reads/compiles
candidate strings; it never claims a runtime kill. Every row is accounted for.
"""
import argparse
from pathlib import Path
import re
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, new_output_directory, run_owned, wait_for_slot
from negative_suite import invalidate_completed, publish_summary

ROOT = Path(__file__).resolve().parents[5]
TEST = Path(__file__).with_name('test_entry_guard.py')
PROBE = Path(__file__).with_name('probe.py')
COMMON = ROOT / 'scripts/gateway-parity/common.py'

# Literal rows are also read by anchor_selfcheck.py. The binding includes the
# exact named unittest and its own #1066 assertion, never a generic failure.
MUTATIONS = [
    ('gateway_parity_run_round_py_reinsert', ROOT / 'scripts/gateway-parity/run_round.py',
     'if any(\n        os.path.isdir(entry or os.curdir)',
     'if False and any(\n        os.path.isdir(entry or os.curdir)',
     'EntryGuardTests.test_gateway_parity_run_round_py_reinsert',
     '#1066 scripts/gateway-parity/run_round.py: reinsertion must stop before stdlib'),
    ('gateway_parity_run_round_py_old_python', ROOT / 'scripts/gateway-parity/run_round.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     "False or 'os' not in sys.modules",
     'EntryGuardTests.test_gateway_parity_run_round_py_old_python',
     '#1066 scripts/gateway-parity/run_round.py: old-python must stop before os import'),
    ('gateway_parity_run_round_py_unloaded_os', ROOT / 'scripts/gateway-parity/run_round.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     'sys.version_info < (3, 11) or False',
     'EntryGuardTests.test_gateway_parity_run_round_py_unloaded_os',
     '#1066 scripts/gateway-parity/run_round.py: unloaded-os must stop before os import'),
    ('gateway_parity_run_round_py_identity', ROOT / 'scripts/gateway-parity/run_round.py',
     'os.path.samefile(entry or os.curdir, root)',
     '(entry or os.curdir) == root',
     'EntryGuardTests.test_gateway_parity_run_round_py_reinsert',
     '#1066 scripts/gateway-parity/run_round.py: reinsertion must stop before stdlib'),
    ('gateway_parity_compare_py_reinsert', ROOT / 'scripts/gateway-parity/compare.py',
     'if any(\n        os.path.isdir(entry or os.curdir)',
     'if False and any(\n        os.path.isdir(entry or os.curdir)',
     'EntryGuardTests.test_gateway_parity_compare_py_reinsert',
     '#1066 scripts/gateway-parity/compare.py: reinsertion must stop before stdlib'),
    ('gateway_parity_compare_py_old_python', ROOT / 'scripts/gateway-parity/compare.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     "False or 'os' not in sys.modules",
     'EntryGuardTests.test_gateway_parity_compare_py_old_python',
     '#1066 scripts/gateway-parity/compare.py: old-python must stop before os import'),
    ('gateway_parity_compare_py_unloaded_os', ROOT / 'scripts/gateway-parity/compare.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     'sys.version_info < (3, 11) or False',
     'EntryGuardTests.test_gateway_parity_compare_py_unloaded_os',
     '#1066 scripts/gateway-parity/compare.py: unloaded-os must stop before os import'),
    ('gateway_parity_check_frozen_py_reinsert', ROOT / 'scripts/gateway-parity/check_frozen.py',
     'if any(\n        os.path.isdir(entry or os.curdir)',
     'if False and any(\n        os.path.isdir(entry or os.curdir)',
     'EntryGuardTests.test_gateway_parity_check_frozen_py_reinsert',
     '#1066 scripts/gateway-parity/check_frozen.py: reinsertion must stop before stdlib'),
    ('gateway_parity_check_frozen_py_old_python', ROOT / 'scripts/gateway-parity/check_frozen.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     "False or 'os' not in sys.modules",
     'EntryGuardTests.test_gateway_parity_check_frozen_py_old_python',
     '#1066 scripts/gateway-parity/check_frozen.py: old-python must stop before os import'),
    ('gateway_parity_check_frozen_py_unloaded_os', ROOT / 'scripts/gateway-parity/check_frozen.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     'sys.version_info < (3, 11) or False',
     'EntryGuardTests.test_gateway_parity_check_frozen_py_unloaded_os',
     '#1066 scripts/gateway-parity/check_frozen.py: unloaded-os must stop before os import'),
    ('gateway_parity_check_ruler_py_reinsert', ROOT / 'scripts/gateway-parity/check_ruler.py',
     'if any(\n        os.path.isdir(entry or os.curdir)',
     'if False and any(\n        os.path.isdir(entry or os.curdir)',
     'EntryGuardTests.test_gateway_parity_check_ruler_py_reinsert',
     '#1066 scripts/gateway-parity/check_ruler.py: reinsertion must stop before stdlib'),
    ('gateway_parity_check_ruler_py_old_python', ROOT / 'scripts/gateway-parity/check_ruler.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     "False or 'os' not in sys.modules",
     'EntryGuardTests.test_gateway_parity_check_ruler_py_old_python',
     '#1066 scripts/gateway-parity/check_ruler.py: old-python must stop before os import'),
    ('gateway_parity_check_ruler_py_unloaded_os', ROOT / 'scripts/gateway-parity/check_ruler.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     'sys.version_info < (3, 11) or False',
     'EntryGuardTests.test_gateway_parity_check_ruler_py_unloaded_os',
     '#1066 scripts/gateway-parity/check_ruler.py: unloaded-os must stop before os import'),
    ('gateway_parity_build_proxy_py_reinsert', ROOT / 'scripts/gateway-parity/build_proxy.py',
     'if any(\n        os.path.isdir(entry or os.curdir)',
     'if False and any(\n        os.path.isdir(entry or os.curdir)',
     'EntryGuardTests.test_gateway_parity_build_proxy_py_reinsert',
     '#1066 scripts/gateway-parity/build_proxy.py: reinsertion must stop before stdlib'),
    ('gateway_parity_build_proxy_py_old_python', ROOT / 'scripts/gateway-parity/build_proxy.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     "False or 'os' not in sys.modules",
     'EntryGuardTests.test_gateway_parity_build_proxy_py_old_python',
     '#1066 scripts/gateway-parity/build_proxy.py: old-python must stop before os import'),
    ('gateway_parity_build_proxy_py_unloaded_os', ROOT / 'scripts/gateway-parity/build_proxy.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     'sys.version_info < (3, 11) or False',
     'EntryGuardTests.test_gateway_parity_build_proxy_py_unloaded_os',
     '#1066 scripts/gateway-parity/build_proxy.py: unloaded-os must stop before os import'),
    ('gateway_parity_observer_py_reinsert', ROOT / 'scripts/gateway-parity/observer.py',
     'if any(\n        os.path.isdir(entry or os.curdir)',
     'if False and any(\n        os.path.isdir(entry or os.curdir)',
     'EntryGuardTests.test_gateway_parity_observer_py_reinsert',
     '#1066 scripts/gateway-parity/observer.py: reinsertion must stop before stdlib'),
    ('gateway_parity_observer_py_old_python', ROOT / 'scripts/gateway-parity/observer.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     "False or 'os' not in sys.modules",
     'EntryGuardTests.test_gateway_parity_observer_py_old_python',
     '#1066 scripts/gateway-parity/observer.py: old-python must stop before os import'),
    ('gateway_parity_observer_py_unloaded_os', ROOT / 'scripts/gateway-parity/observer.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     'sys.version_info < (3, 11) or False',
     'EntryGuardTests.test_gateway_parity_observer_py_unloaded_os',
     '#1066 scripts/gateway-parity/observer.py: unloaded-os must stop before os import'),
    ('gateway_parity_session_domain_py_reinsert', ROOT / 'scripts/gateway-parity/session_domain.py',
     'if any(\n        os.path.isdir(entry or os.curdir)',
     'if False and any(\n        os.path.isdir(entry or os.curdir)',
     'EntryGuardTests.test_gateway_parity_session_domain_py_reinsert',
     '#1066 scripts/gateway-parity/session_domain.py: reinsertion must stop before stdlib'),
    ('gateway_parity_session_domain_py_old_python', ROOT / 'scripts/gateway-parity/session_domain.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     "False or 'os' not in sys.modules",
     'EntryGuardTests.test_gateway_parity_session_domain_py_old_python',
     '#1066 scripts/gateway-parity/session_domain.py: old-python must stop before os import'),
    ('gateway_parity_session_domain_py_unloaded_os', ROOT / 'scripts/gateway-parity/session_domain.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     'sys.version_info < (3, 11) or False',
     'EntryGuardTests.test_gateway_parity_session_domain_py_unloaded_os',
     '#1066 scripts/gateway-parity/session_domain.py: unloaded-os must stop before os import'),
    ('gateway_parity_test_safety_py_reinsert', ROOT / 'scripts/gateway-parity/test_safety.py',
     'if any(\n        os.path.isdir(entry or os.curdir)',
     'if False and any(\n        os.path.isdir(entry or os.curdir)',
     'EntryGuardTests.test_gateway_parity_test_safety_py_reinsert',
     '#1066 scripts/gateway-parity/test_safety.py: reinsertion must stop before stdlib'),
    ('gateway_parity_test_safety_py_old_python', ROOT / 'scripts/gateway-parity/test_safety.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     "False or 'os' not in sys.modules",
     'EntryGuardTests.test_gateway_parity_test_safety_py_old_python',
     '#1066 scripts/gateway-parity/test_safety.py: old-python must stop before os import'),
    ('gateway_parity_test_safety_py_unloaded_os', ROOT / 'scripts/gateway-parity/test_safety.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     'sys.version_info < (3, 11) or False',
     'EntryGuardTests.test_gateway_parity_test_safety_py_unloaded_os',
     '#1066 scripts/gateway-parity/test_safety.py: unloaded-os must stop before os import'),
    ('gateway_parity_controls_run_all_py_reinsert', ROOT / 'scripts/gateway-parity/controls/run_all.py',
     'if any(\n        os.path.isdir(entry or os.curdir)',
     'if False and any(\n        os.path.isdir(entry or os.curdir)',
     'EntryGuardTests.test_gateway_parity_controls_run_all_py_reinsert',
     '#1066 scripts/gateway-parity/controls/run_all.py: reinsertion must stop before stdlib'),
    ('gateway_parity_controls_run_all_py_old_python', ROOT / 'scripts/gateway-parity/controls/run_all.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     "False or 'os' not in sys.modules",
     'EntryGuardTests.test_gateway_parity_controls_run_all_py_old_python',
     '#1066 scripts/gateway-parity/controls/run_all.py: old-python must stop before os import'),
    ('gateway_parity_controls_run_all_py_unloaded_os', ROOT / 'scripts/gateway-parity/controls/run_all.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     'sys.version_info < (3, 11) or False',
     'EntryGuardTests.test_gateway_parity_controls_run_all_py_unloaded_os',
     '#1066 scripts/gateway-parity/controls/run_all.py: unloaded-os must stop before os import'),
    ('gateway_parity_controls_prerequisites_py_reinsert', ROOT / 'scripts/gateway-parity/controls/prerequisites.py',
     'if any(\n        os.path.isdir(entry or os.curdir)',
     'if False and any(\n        os.path.isdir(entry or os.curdir)',
     'EntryGuardTests.test_gateway_parity_controls_prerequisites_py_reinsert',
     '#1066 scripts/gateway-parity/controls/prerequisites.py: reinsertion must stop before stdlib'),
    ('gateway_parity_controls_prerequisites_py_old_python', ROOT / 'scripts/gateway-parity/controls/prerequisites.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     "False or 'os' not in sys.modules",
     'EntryGuardTests.test_gateway_parity_controls_prerequisites_py_old_python',
     '#1066 scripts/gateway-parity/controls/prerequisites.py: old-python must stop before os import'),
    ('gateway_parity_controls_prerequisites_py_unloaded_os', ROOT / 'scripts/gateway-parity/controls/prerequisites.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     'sys.version_info < (3, 11) or False',
     'EntryGuardTests.test_gateway_parity_controls_prerequisites_py_unloaded_os',
     '#1066 scripts/gateway-parity/controls/prerequisites.py: unloaded-os must stop before os import'),
    ('gateway_parity_controls_session_domain_py_reinsert', ROOT / 'scripts/gateway-parity/controls/session_domain.py',
     'if any(\n        os.path.isdir(entry or os.curdir)',
     'if False and any(\n        os.path.isdir(entry or os.curdir)',
     'EntryGuardTests.test_gateway_parity_controls_session_domain_py_reinsert',
     '#1066 scripts/gateway-parity/controls/session_domain.py: reinsertion must stop before stdlib'),
    ('gateway_parity_controls_session_domain_py_old_python', ROOT / 'scripts/gateway-parity/controls/session_domain.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     "False or 'os' not in sys.modules",
     'EntryGuardTests.test_gateway_parity_controls_session_domain_py_old_python',
     '#1066 scripts/gateway-parity/controls/session_domain.py: old-python must stop before os import'),
    ('gateway_parity_controls_session_domain_py_unloaded_os', ROOT / 'scripts/gateway-parity/controls/session_domain.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     'sys.version_info < (3, 11) or False',
     'EntryGuardTests.test_gateway_parity_controls_session_domain_py_unloaded_os',
     '#1066 scripts/gateway-parity/controls/session_domain.py: unloaded-os must stop before os import'),
    ('parity_score_py_reinsert', ROOT / 'scripts/parity/score.py',
     'if any(\n        os.path.isdir(entry or os.curdir)',
     'if False and any(\n        os.path.isdir(entry or os.curdir)',
     'EntryGuardTests.test_parity_score_py_reinsert',
     '#1066 scripts/parity/score.py: reinsertion must stop before stdlib'),
    ('parity_score_py_old_python', ROOT / 'scripts/parity/score.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     "False or 'os' not in sys.modules",
     'EntryGuardTests.test_parity_score_py_old_python',
     '#1066 scripts/parity/score.py: old-python must stop before os import'),
    ('parity_score_py_unloaded_os', ROOT / 'scripts/parity/score.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     'sys.version_info < (3, 11) or False',
     'EntryGuardTests.test_parity_score_py_unloaded_os',
     '#1066 scripts/parity/score.py: unloaded-os must stop before os import'),
    ('parity_score_py_identity', ROOT / 'scripts/parity/score.py',
     'os.path.samefile(entry or os.curdir, root)',
     '(entry or os.curdir) == root',
     'EntryGuardTests.test_parity_score_py_reinsert',
     '#1066 scripts/parity/score.py: reinsertion must stop before stdlib'),
    ('parity_score_py_future', ROOT / 'scripts/parity/score.py',
     'import sys\n',
     'from __future__ import annotations\n\nimport sys\n',
     'EntryGuardTests.test_parity_score_py_reinsert',
     '#1066 scripts/parity/score.py: reinsertion must stop before stdlib'),
    ('parity_check_frozen_py_reinsert', ROOT / 'scripts/parity/check_frozen.py',
     'if any(\n        os.path.isdir(entry or os.curdir)',
     'if False and any(\n        os.path.isdir(entry or os.curdir)',
     'EntryGuardTests.test_parity_check_frozen_py_reinsert',
     '#1066 scripts/parity/check_frozen.py: reinsertion must stop before stdlib'),
    ('parity_check_frozen_py_old_python', ROOT / 'scripts/parity/check_frozen.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     "False or 'os' not in sys.modules",
     'EntryGuardTests.test_parity_check_frozen_py_old_python',
     '#1066 scripts/parity/check_frozen.py: old-python must stop before os import'),
    ('parity_check_frozen_py_unloaded_os', ROOT / 'scripts/parity/check_frozen.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     'sys.version_info < (3, 11) or False',
     'EntryGuardTests.test_parity_check_frozen_py_unloaded_os',
     '#1066 scripts/parity/check_frozen.py: unloaded-os must stop before os import'),
    ('parity_check_ruler_py_reinsert', ROOT / 'scripts/parity/check_ruler.py',
     'if any(\n        os.path.isdir(entry or os.curdir)',
     'if False and any(\n        os.path.isdir(entry or os.curdir)',
     'EntryGuardTests.test_parity_check_ruler_py_reinsert',
     '#1066 scripts/parity/check_ruler.py: reinsertion must stop before stdlib'),
    ('parity_check_ruler_py_old_python', ROOT / 'scripts/parity/check_ruler.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     "False or 'os' not in sys.modules",
     'EntryGuardTests.test_parity_check_ruler_py_old_python',
     '#1066 scripts/parity/check_ruler.py: old-python must stop before os import'),
    ('parity_check_ruler_py_unloaded_os', ROOT / 'scripts/parity/check_ruler.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     'sys.version_info < (3, 11) or False',
     'EntryGuardTests.test_parity_check_ruler_py_unloaded_os',
     '#1066 scripts/parity/check_ruler.py: unloaded-os must stop before os import'),
    ('parity_g03_compare_py_reinsert', ROOT / 'scripts/parity/g03_compare.py',
     'if any(\n        os.path.isdir(entry or os.curdir)',
     'if False and any(\n        os.path.isdir(entry or os.curdir)',
     'EntryGuardTests.test_parity_g03_compare_py_reinsert',
     '#1066 scripts/parity/g03_compare.py: reinsertion must stop before stdlib'),
    ('parity_g03_compare_py_old_python', ROOT / 'scripts/parity/g03_compare.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     "False or 'os' not in sys.modules",
     'EntryGuardTests.test_parity_g03_compare_py_old_python',
     '#1066 scripts/parity/g03_compare.py: old-python must stop before os import'),
    ('parity_g03_compare_py_unloaded_os', ROOT / 'scripts/parity/g03_compare.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     'sys.version_info < (3, 11) or False',
     'EntryGuardTests.test_parity_g03_compare_py_unloaded_os',
     '#1066 scripts/parity/g03_compare.py: unloaded-os must stop before os import'),
    ('parity_g03_compare_py_future', ROOT / 'scripts/parity/g03_compare.py',
     'import sys\n',
     'from __future__ import annotations\n\nimport sys\n',
     'EntryGuardTests.test_parity_g03_compare_py_reinsert',
     '#1066 scripts/parity/g03_compare.py: reinsertion must stop before stdlib'),
    ('parity_round_py_reinsert', ROOT / 'scripts/parity/round.py',
     'if any(\n        os.path.isdir(entry or os.curdir)',
     'if False and any(\n        os.path.isdir(entry or os.curdir)',
     'EntryGuardTests.test_parity_round_py_reinsert',
     '#1066 scripts/parity/round.py: reinsertion must stop before stdlib'),
    ('parity_round_py_old_python', ROOT / 'scripts/parity/round.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     "False or 'os' not in sys.modules",
     'EntryGuardTests.test_parity_round_py_old_python',
     '#1066 scripts/parity/round.py: old-python must stop before os import'),
    ('parity_round_py_unloaded_os', ROOT / 'scripts/parity/round.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     'sys.version_info < (3, 11) or False',
     'EntryGuardTests.test_parity_round_py_unloaded_os',
     '#1066 scripts/parity/round.py: unloaded-os must stop before os import'),
    ('parity_self_compare_py_reinsert', ROOT / 'scripts/parity/self_compare.py',
     'if any(\n        os.path.isdir(entry or os.curdir)',
     'if False and any(\n        os.path.isdir(entry or os.curdir)',
     'EntryGuardTests.test_parity_self_compare_py_reinsert',
     '#1066 scripts/parity/self_compare.py: reinsertion must stop before stdlib'),
    ('parity_self_compare_py_old_python', ROOT / 'scripts/parity/self_compare.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     "False or 'os' not in sys.modules",
     'EntryGuardTests.test_parity_self_compare_py_old_python',
     '#1066 scripts/parity/self_compare.py: old-python must stop before os import'),
    ('parity_self_compare_py_unloaded_os', ROOT / 'scripts/parity/self_compare.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     'sys.version_info < (3, 11) or False',
     'EntryGuardTests.test_parity_self_compare_py_unloaded_os',
     '#1066 scripts/parity/self_compare.py: unloaded-os must stop before os import'),
    ('parity_controls_lock_release_py_reinsert', ROOT / 'scripts/parity/controls/lock_release.py',
     'if any(\n        os.path.isdir(entry or os.curdir)',
     'if False and any(\n        os.path.isdir(entry or os.curdir)',
     'EntryGuardTests.test_parity_controls_lock_release_py_reinsert',
     '#1066 scripts/parity/controls/lock_release.py: reinsertion must stop before stdlib'),
    ('parity_controls_lock_release_py_old_python', ROOT / 'scripts/parity/controls/lock_release.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     "False or 'os' not in sys.modules",
     'EntryGuardTests.test_parity_controls_lock_release_py_old_python',
     '#1066 scripts/parity/controls/lock_release.py: old-python must stop before os import'),
    ('parity_controls_lock_release_py_unloaded_os', ROOT / 'scripts/parity/controls/lock_release.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     'sys.version_info < (3, 11) or False',
     'EntryGuardTests.test_parity_controls_lock_release_py_unloaded_os',
     '#1066 scripts/parity/controls/lock_release.py: unloaded-os must stop before os import'),
    ('parity_controls_run_all_py_reinsert', ROOT / 'scripts/parity/controls/run_all.py',
     'if any(\n        os.path.isdir(entry or os.curdir)',
     'if False and any(\n        os.path.isdir(entry or os.curdir)',
     'EntryGuardTests.test_parity_controls_run_all_py_reinsert',
     '#1066 scripts/parity/controls/run_all.py: reinsertion must stop before stdlib'),
    ('parity_controls_run_all_py_old_python', ROOT / 'scripts/parity/controls/run_all.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     "False or 'os' not in sys.modules",
     'EntryGuardTests.test_parity_controls_run_all_py_old_python',
     '#1066 scripts/parity/controls/run_all.py: old-python must stop before os import'),
    ('parity_controls_run_all_py_unloaded_os', ROOT / 'scripts/parity/controls/run_all.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     'sys.version_info < (3, 11) or False',
     'EntryGuardTests.test_parity_controls_run_all_py_unloaded_os',
     '#1066 scripts/parity/controls/run_all.py: unloaded-os must stop before os import'),
    ('parity_controls_multimodal_py_reinsert', ROOT / 'scripts/parity/controls/multimodal.py',
     'if any(\n        os.path.isdir(entry or os.curdir)',
     'if False and any(\n        os.path.isdir(entry or os.curdir)',
     'EntryGuardTests.test_parity_controls_multimodal_py_reinsert',
     '#1066 scripts/parity/controls/multimodal.py: reinsertion must stop before stdlib'),
    ('parity_controls_multimodal_py_old_python', ROOT / 'scripts/parity/controls/multimodal.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     "False or 'os' not in sys.modules",
     'EntryGuardTests.test_parity_controls_multimodal_py_old_python',
     '#1066 scripts/parity/controls/multimodal.py: old-python must stop before os import'),
    ('parity_controls_multimodal_py_unloaded_os', ROOT / 'scripts/parity/controls/multimodal.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     'sys.version_info < (3, 11) or False',
     'EntryGuardTests.test_parity_controls_multimodal_py_unloaded_os',
     '#1066 scripts/parity/controls/multimodal.py: unloaded-os must stop before os import'),
    ('parity_controls_review_r1_py_reinsert', ROOT / 'scripts/parity/controls/review_r1.py',
     'if any(\n        os.path.isdir(entry or os.curdir)',
     'if False and any(\n        os.path.isdir(entry or os.curdir)',
     'EntryGuardTests.test_parity_controls_review_r1_py_reinsert',
     '#1066 scripts/parity/controls/review_r1.py: reinsertion must stop before stdlib'),
    ('parity_controls_review_r1_py_old_python', ROOT / 'scripts/parity/controls/review_r1.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     "False or 'os' not in sys.modules",
     'EntryGuardTests.test_parity_controls_review_r1_py_old_python',
     '#1066 scripts/parity/controls/review_r1.py: old-python must stop before os import'),
    ('parity_controls_review_r1_py_unloaded_os', ROOT / 'scripts/parity/controls/review_r1.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     'sys.version_info < (3, 11) or False',
     'EntryGuardTests.test_parity_controls_review_r1_py_unloaded_os',
     '#1066 scripts/parity/controls/review_r1.py: unloaded-os must stop before os import'),
    ('check_approval_provenance_py_reinsert', ROOT / 'scripts/check-approval-provenance.py',
     'if any(\n        os.path.isdir(entry or os.curdir)',
     'if False and any(\n        os.path.isdir(entry or os.curdir)',
     'EntryGuardTests.test_check_approval_provenance_py_reinsert',
     '#1066 scripts/check-approval-provenance.py: reinsertion must stop before stdlib'),
    ('check_approval_provenance_py_old_python', ROOT / 'scripts/check-approval-provenance.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     "False or 'os' not in sys.modules",
     'EntryGuardTests.test_check_approval_provenance_py_old_python',
     '#1066 scripts/check-approval-provenance.py: old-python must stop before os import'),
    ('check_approval_provenance_py_unloaded_os', ROOT / 'scripts/check-approval-provenance.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     'sys.version_info < (3, 11) or False',
     'EntryGuardTests.test_check_approval_provenance_py_unloaded_os',
     '#1066 scripts/check-approval-provenance.py: unloaded-os must stop before os import'),
    ('check_approval_provenance_py_identity', ROOT / 'scripts/check-approval-provenance.py',
     'os.path.samefile(entry or os.curdir, root)',
     '(entry or os.curdir) == root',
     'EntryGuardTests.test_check_approval_provenance_py_reinsert',
     '#1066 scripts/check-approval-provenance.py: reinsertion must stop before stdlib'),
    ('test_approval_provenance_py_reinsert', ROOT / 'scripts/test-approval-provenance.py',
     'if any(\n        os.path.isdir(entry or os.curdir)',
     'if False and any(\n        os.path.isdir(entry or os.curdir)',
     'EntryGuardTests.test_test_approval_provenance_py_reinsert',
     '#1066 scripts/test-approval-provenance.py: reinsertion must stop before stdlib'),
    ('test_approval_provenance_py_old_python', ROOT / 'scripts/test-approval-provenance.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     "False or 'os' not in sys.modules",
     'EntryGuardTests.test_test_approval_provenance_py_old_python',
     '#1066 scripts/test-approval-provenance.py: old-python must stop before os import'),
    ('test_approval_provenance_py_unloaded_os', ROOT / 'scripts/test-approval-provenance.py',
     "sys.version_info < (3, 11) or 'os' not in sys.modules",
     'sys.version_info < (3, 11) or False',
     'EntryGuardTests.test_test_approval_provenance_py_unloaded_os',
     '#1066 scripts/test-approval-provenance.py: unloaded-os must stop before os import'),
]


def changed(source, old, new, path):
    if source.count(old) != 1:
        raise ValueError(f'#1066 stale anchor in {path}: expected 1, found {source.count(old)}')
    result = source.replace(old, new, 1)
    compile(result, str(path), 'exec', dont_inherit=True)
    return result


def passed(result, test):
    log = result.stdout + result.stderr
    method = test.split('.')[-1]
    named = re.search(r'^' + re.escape(method) + r' \(__main__\.' + re.escape(test)
                      + r'\) \.\.\. ok$', log, re.M)
    return (result.returncode == 0 and re.search(r'^Ran 1 test in ', log, re.M)
            and named is not None
            and re.search(r'^OK$', log, re.M) and not re.search(r'^ERROR:|^FAIL:', log, re.M))


def classify(result, test, assertion):
    if passed(result, test):
        return 'SURVIVED', 'bound test still passed'
    log = result.stdout + result.stderr
    blocks = re.findall(r'^FAIL: (.*?)\n-+\n(.*?)(?=^=+\n|^--+\nRan |\Z)', log, re.M | re.S)
    method = test.split('.')[-1]
    if (result.returncode == 1 and re.search(r'^Ran 1 test in ', log, re.M)
            and not re.search(r'^ERROR:', log, re.M) and blocks
            and all(title.startswith(method + ' ') for title, _ in blocks)
            and all('AssertionError:' in body and assertion in body.split('AssertionError:', 1)[1]
                    for _, body in blocks)):
        return 'KILLED', 'bound unittest failed at its exclusive assertion'
    return 'INVALID', 'wrong failure point, missing test, timeout or harness error'


def run(out, label, test):
    result = run_owned([sys.executable, '-I', '-B', str(TEST), test, '-v'], cwd=ROOT, timeout=60)
    with (out / (label + '.log')).open('x') as stream:
        stream.write(result.stdout + result.stderr)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--only', action='append', default=[], help='exact mutation name; repeatable')
    parser.add_argument('--check-anchors', action='store_true')
    args = parser.parse_args()
    names = {row[0] for row in MUTATIONS}
    if set(args.only) - names:
        parser.error('unknown --only mutation')
    selected = [row for row in MUTATIONS if not args.only or row[0] in args.only]
    if args.check_anchors:
        errors = 0
        for name, path, old, new, _test, _assertion in selected:
            try:
                changed(path.read_text(), old, new, path)
                print(f'{name}: anchor OK; Python mutant compiles (runtime not run)')
            except Exception as error:
                errors += 1
                print(f'{name}: INVALID: {error}')
        return int(bool(errors))
    if args.output is None:
        parser.error('--output is required unless --check-anchors is used')
    out = new_output_directory(args.output)
    results = []

    def record(row, status, reason, compiled=False):
        results.append({'mutation': row[0], 'test': row[4], 'assertion': row[5],
                        'status': status, 'reason': reason, 'compiled': compiled})
        print(f'{row[0]}: {status}: {reason}', flush=True)

    paths = list(dict.fromkeys([row[1] for row in selected] + [COMMON, TEST, PROBE, Path(__file__)]))
    phase = 'enter'
    try:
        with mutating_sources(ROOT, paths, lock_timeout=60) as originals:
            phase = 'body'
            for row in selected:
                name, path, old, new, test, assertion = row
                status, reason, compiled = 'INVALID', '', False
                try:
                    candidate = changed(originals[path].decode(), old, new, path)
                    compiled = True
                    if not passed(run(out, name + '-positive', test), test):
                        reason = 'bound positive test did not pass'
                    else:
                        path.write_text(candidate)
                        status, reason = classify(run(out, name + '-negative', test), test, assertion)
                except Exception as error:
                    status, reason = 'INVALID', f'{type(error).__name__}: {error}'
                finally:
                    phase = 'restore'
                    path.write_bytes(originals[path])
                    if path.read_bytes() != originals[path]:
                        raise RuntimeError(f'#1066 restore mismatch: {path}')
                    phase = 'body'
                try:
                    if not passed(run(out, name + '-restored', test), test):
                        status, reason = 'INVALID', reason + '; restored test did not pass'
                except Exception as error:
                    status, reason = 'INVALID', reason + f'; restore check: {error}'
                record(row, status, reason, compiled)
            phase = 'exit'
    except BaseException as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate_completed(results, reason)
        for row in selected[len(results):]:
            record(row, 'INVALID', reason)
        publish_summary(out, results)
        return 1 if phase == 'enter' and isinstance(error, Exception) else 2
    publish_summary(out, results)
    counts = {state: sum(row['status'] == state for row in results)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    print('summary: ' + ', '.join(f'{key}={value}' for key, value in counts.items()))
    return int(any(row['status'] != 'KILLED' for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
