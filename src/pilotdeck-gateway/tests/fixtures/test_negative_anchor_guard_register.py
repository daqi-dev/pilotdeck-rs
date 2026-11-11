#!/usr/bin/env python3
# anchor-selfcheck: unsupported static runner-guard register scanner, no mutation definitions
"""#2887: every ``mutant_switch.run`` caller must hash the denominator's code.

``mutant_switch.run(..., script=Path(__file__))`` imports ``anchor_selfcheck``
lazily, only *after* ``verify_unchanged`` has already re-hashed the guarded
files (``_shared/mutant_switch.py``: the denominator check does the import at
its own first line). A runner whose ``guarded=`` list omits
``fixtures/anchor_selfcheck.py`` therefore executes judgement code the source
guard never hashed: a concurrent edit to ``anchor_selfcheck.static_switch_names``
during the run passes ``verify_unchanged`` unnoticed, and ``enforce_denominator``
then measures the executed table with the tampered ruler -- a shrunken roster
publishes a false PASS (PR #2974 DS sibling P1; the same round's P2a fixed
``user-dialog-bus-observer`` alone, this register is the follow-up).

The register keeps both directions honest, in the
``BASELINE_GUARDED``-style shape of ``test_negative_optimize_guards.py``:

* ``RUN_CALLERS`` lists every ``mutant_switch.run`` call site together with
  the module-level name it passes as ``guarded=``. A listed entry must really
  pass a list that carries the ``anchor_selfcheck.py`` entry, so deleting one
  GUARDED line (the exact regression this file exists for) turns it red.
* The AST scan finds every ``mutant_switch.run`` call site under this
  directory and asserts it is listed: a new runner born without the entry is
  red until it is registered, and a stale entry whose runner went away is red
  too. The scan reads Call nodes, so a mention inside a comment or docstring
  (``test_negative_optimize_guards.py`` has one) is not a call site.

Fail-closed shapes: a call site without a ``guarded=`` keyword, or one whose
``guarded=`` argument is not a plain module-level name (an inline list, a
function call), is reported as a problem, never skipped -- those are the
shapes whose contents this static register cannot read.

Scope boundary: only the ``mutant_switch.run`` call table is asserted. The
``mutation_suite`` path (``_shared/negative_control.py``) keeps its own
``guarded=`` keyword argument and never runs ``enforce_denominator``, and
``negative_suite.py``'s top-level import already puts ``pending_reanchor.py``
on ``sys.modules`` before any recording -- neither is a call site here and
neither may be "fixed" by this register.

Run from this directory, no cargo and no tree lock::

    python3 -B -m unittest -v test_negative_anchor_guard_register
"""
import ast
from pathlib import Path
import unittest

FIXTURES = Path(__file__).resolve().parent
ANCHOR_ENTRY = 'anchor_selfcheck.py'

# (runner, module-level name passed as guarded=). Sorted by runner path.
RUN_CALLERS = (
    ('api-web/negative_controls.py', 'GUARDED'),
    ('channel-slack/negative_controls.py', 'GUARDED'),
    ('channel-sms/negative_controls.py', 'GUARDED'),
    ('channel-sms-head-timeout/negative_controls.py', 'GUARDED'),
    ('edgeclaw-native-default/negative.py', 'GUARDED'),
    ('edgeclaw-provider/negative.py', 'GUARDED'),
    ('edgeclaw-same-root-generation/negative.py', 'GUARDED'),
    ('edgeclaw-service-orchestration/switch_negative.py', 'GUARDED'),
    ('host-event-timeline/schemata.py', 'GUARDED'),
    ('host-factory/schemata.py', 'GUARDED'),
    ('live-recording/schemata.py', 'GUARDED'),
    ('live-session/schemata.py', 'GUARDED'),
    ('locale-adaptive/negative_controls.py', 'GUARDED'),
    ('mcp-resolver-lease/negative_controls.py', 'GUARDED'),
    ('module-host/negative_controls.py', 'GUARDED'),
    ('module-host-approvals/negative_controls.py', 'GUARDED'),
    ('native-http-basic/negative_controls.py', 'GUARDED'),
    ('session-abort-contract/schemata.py', 'GUARDED'),
    ('session-adopt/switch_negative.py', 'GUARDED'),
    ('session-cwd-timing/negative_controls.py', 'GUARDED'),
    ('session-domain/schemata.py', 'GUARDED'),
    ('session-factory/subagent_schemata.py', 'GUARDED'),
    ('session-owner/schemata.py', 'GUARDED'),
    ('session-owner/schemata_close_shutdown.py', 'GUARDED'),
    ('session-owner/schemata_equivalent.py', 'GUARDED'),
    ('session-owner-hooks/schemata.py', 'GUARDED'),
    ('tool-budget-cwd/schemata.py', 'GUARDED'),
    ('turn-gates/schemata.py', 'GUARDED'),
    ('turn-prompt-hooks/schemata.py', 'GUARDED'),
    ('turn-runner-tail/schemata.py', 'GUARDED'),
    ('turn-start-notices/schemata.py', 'GUARDED'),
    ('user-dialog-bus-observer/negative_controls.py', 'GUARDED'),
    ('user-dialog-http-store/negative_controls_1947.py', 'GUARDED'),
    ('webhook-channel/negative_controls.py', 'GUARDED'),
)


def scan_call_sites():
    """Every ``mutant_switch.run`` call site: {runner: guarded-name-or-None}.

    A site whose ``guarded=`` argument is missing or not a plain name is
    reported with ``None``; the assertions below treat that as a failure
    rather than skipping the file.
    """
    sites = {}
    for script in sorted(FIXTURES.rglob('*.py')):
        tree = ast.parse(script.read_text(encoding='utf-8'), filename=str(script))
        for node in ast.walk(tree):
            if not (isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute)
                    and node.func.attr == 'run'
                    and isinstance(node.func.value, ast.Name)
                    and node.func.value.id == 'mutant_switch'):
                continue
            relative = script.relative_to(FIXTURES).as_posix()
            argument = {keyword.arg: keyword.value for keyword in node.keywords}.get('guarded')
            name = argument.id if isinstance(argument, ast.Name) else None
            # Two sites in one file (channel-slack runs the suite twice over
            # the same list) may disagree; that is itself a fail-closed hit.
            if sites.get(relative, name) != name:
                sites[relative] = None
            else:
                sites[relative] = name
    return sites


def guarded_strings(runner, name):
    """Every string literal inside the runner's module-level ``name`` list."""
    tree = ast.parse((FIXTURES / runner).read_text(encoding='utf-8'))
    for node in tree.body:
        if isinstance(node, ast.Assign) and any(
                isinstance(target, ast.Name) and target.id == name for target in node.targets):
            return {part.value for part in ast.walk(node.value)
                    if isinstance(part, ast.Constant) and isinstance(part.value, str)}
    raise AssertionError(f'#2887 {runner}: no module-level {name} list to read')


class AnchorGuardRegister(unittest.TestCase):
    """Both directions of the runner register: content and completeness."""

    def test_registered_runners_really_guard_the_denominator(self):
        for runner, name in RUN_CALLERS:
            with self.subTest(runner=runner):
                self.assertTrue((FIXTURES / runner).is_file(),
                                f'#2887 registered runner is missing: {runner}')
                self.assertIn(ANCHOR_ENTRY, guarded_strings(runner, name),
                              f'#2887 {runner}: the {name} list handed to '
                              f'mutant_switch.run does not hash {ANCHOR_ENTRY}; '
                              'the lazily imported denominator code would run '
                              'unhashed (delete this GUARDED entry and the '
                              'suite can publish a shrunken-roster false PASS)')

    def test_every_mutant_switch_run_call_site_is_registered(self):
        sites = scan_call_sites()
        unreadable = sorted(runner for runner, name in sites.items() if name is None)
        self.assertEqual(unreadable, [],
                         '#2887 these mutant_switch.run call sites pass no '
                         'readable module-level guarded= name; the static '
                         'register cannot check their guard list')
        unregistered = sorted(set(sites) - {runner for runner, _ in RUN_CALLERS})
        self.assertEqual(unregistered, [],
                         '#2887 new mutant_switch.run caller(s) not in the '
                         'RUN_CALLERS register; register them and make their '
                         'guarded= list hash anchor_selfcheck.py')
        stale = sorted({runner for runner, _ in RUN_CALLERS} - set(sites))
        self.assertEqual(stale, [],
                         '#2887 RUN_CALLERS lists runner(s) with no '
                         'mutant_switch.run call site any more')


if __name__ == '__main__':
    unittest.main()
