#!/usr/bin/env python3
# anchor-selfcheck: unsupported meta-test of runner lease acquire budgets, loads the guarded runners in memory only
"""#2929 fix-r2/fix-r3: every global-lease runner acquires with a family-max budget.

The runners in the roster below each guard at least one
``scripts/gateway-parity/`` source inside a ``mutating_sources`` window, so
they hold the host-wide ``/tmp/pdrs-parity.lock`` lease for that window.
fix-r1 derived each runner's acquire budget from its *own* whole-round hold
bound -- but the binding wait for a queued runner is the *family's largest*
holder (fix-r1's own derivation put gateway-batch at the family maximum),
so seven of those eight budgets expired early behind the real maximum: the
#2929 failure survived with a bigger threshold (sibling#1/boundary#1).  Four
more call sites still ran on the shared default ``lock_timeout=900`` or an
explicit ``60`` (boundary#3), and three pre-existing holders carried
peer-window budgets of their own (sibling#6/#7/#8).

fix-r2 derives every budget from one shared roster, this file:

* **Closed set** (boundary#6/sibling#9): the roster is re-derived from a
  tree scan on every run -- every ``.py`` under ``src/`` and ``scripts/``
  that calls ``mutating_sources`` (bare, qualified or aliased) and spells a
  ``scripts/gateway-parity`` path the runtime predicate would resolve
  under the repo root -- a string constant with the segment, or a ``/``-
  join / ``joinpath`` chain over it (fix-r3, MM boundary#7 ≡ DS
  boundary#2: the old trailing-slash-literal precondition silently
  missed ``ROOT / 'scripts/gateway-parity' / x`` spellings).  A new runner
  that takes the global lease without registering turns this red; the scan
  candidates that never take the host-wide lease are pinned in ``EXCLUDED``
  with machine-checked reasons, and exclusion happens after candidacy.
* **Live hold bounds** (simplify#1/boundary#4): each member's whole-window
  hold bound is derived from its loaded module -- table sizes,
  guarded-path counts and stage timeouts are read from the module or
  cross-checked against its literals, never restated.  Three members cap
  their own hold with a fail-closed budget (same-json-surfaces and
  session-domain-cleanup check ``lease_budget_left`` before every case;
  native-session-e2e bounds each case window with
  ``PER_CASE_LEASE_SECONDS``), so their registered hold is that cap.
* **FAMILY_MAX + margin** (boundary#5): every runner's acquire constant is
  the roster's maximum hold bound plus its own restore tail and one stage
  of handoff margin, rounded up to the 900 s family unit -- strictly above
  the wait it must survive, never equal.

A live holder never loses the lease to age, so the budgets deliberately
exceed ``common.MAX_AGE`` (2400 s): every taker publishes ``cleanupPending``
before it can spawn anything, both takeover-capable implementations
(``scripts/gateway-parity/common.py``, ``scripts/parity/round.py``) refuse
to take over an owner that carries the flag, and the other takers never
steal at all (cli-parity blocks on an existing lock, calibrate.sh raises
``FileExistsError``, fe-parity does not use this lock).  ``MAX_AGE`` only
ages out never-published stale locks: a published owner, dead pid included,
carries the ``cleanupPending`` protection and is never taken over
automatically -- such a residual is only recoverable by hand, and since
fix-r3 both takeover-capable implementations fail the acquire fast with a
diagnostic instead of waiting the budget out.  The
``LEASE_BUDGET + TAIL < MAX_AGE`` startup gates in same-json-surfaces and
session-domain-cleanup are *hold-side* self-checks of their fail-closed
budgets -- a different quantity from the acquire budget pinned here; do not
conflate the two (sibling#6/#7, boundary#2).

fix-r3 (#2929 review P1) also closed the iteration multiplication those
budgets introduced: frozen-manifest's gateway-parity rows share ONE lease
window instead of one each, and native-session-e2e's round hands every
per-case acquire the remaining time of one monotonic deadline (pinned by
``test_round_deadline_runner_waits_on_one_monotonic_deadline``) instead of
the full constant per case.  fix-r4 (#2929 review) hardened the closed-set
scan itself (DS sibling#2/MM boundary#4): call and subscript bases resolve
as roots, ``Path(...)``/``os.path.join`` folds count, and a guarded
argument the resolver cannot read fails the scan closed instead of being
silently skipped.

Run isolated, from this directory::

    python3 -I test_lease_acquire_budgets.py
"""
import ast
import importlib.util
import inspect
import re
import sys
import tempfile
import time
from pathlib import Path
import unittest

if not sys.flags.isolated:
    # The native-witness runners refuse a non-isolated launch at import; this
    # meta-test loads them, so it inherits the same contract.
    raise SystemExit('run with python3 -I')

FIXTURES = Path(__file__).resolve().parent
REPO_ROOT = FIXTURES.parents[3]
FIXTURES_REL = FIXTURES.relative_to(REPO_ROOT).as_posix()
sys.path.insert(0, str(FIXTURES))
import negative_process  # noqa: E402  (path inserted above: the same module the runners import)

CONSTANT = 'LEASE_ACQUIRE_BUDGET_SECONDS'
# The restore/verify tail of one window: every guarded path is rewritten
# atomically (fsync charged as slack) and re-verified against HEAD through one
# _git call per path (GIT_TIMEOUT plus twice the kill grace), then the lease is
# released -- the same tail arithmetic as same-json-surfaces' TAIL_OVERHEAD_
# SECONDS, imported rather than restated so a copy cannot drift.
KILL_GRACE_SECONDS = negative_process.KILL_TIMEOUT
GIT_TIMEOUT = negative_process.GIT_TIMEOUT
FSYNC_AND_LEASE_SLACK_SECONDS = 20
SHARED_DEFAULT_LOCK_TIMEOUT = inspect.signature(
    negative_process.mutating_sources).parameters['lock_timeout'].default


def static_guarded(_module, tree):
    """Guarded-path count read from the call site's own bytes (simplify#1).

    The single ``mutating_sources`` call's second argument is either a
    literal list/tuple (count its elements) or a name bound to exactly one
    literal list/tuple in the file (count that one's elements).  Table-driven
    call sites that compute the set cannot be counted this way and provide
    their own module-reading lambda below.
    """
    calls = [node for node in ast.walk(tree) if isinstance(node, ast.Call)
             and isinstance(node.func, ast.Name)
             and node.func.id == 'mutating_sources']
    if len(calls) != 1:
        raise AssertionError(f'expected one mutating_sources call, found {len(calls)}')
    if len(calls[0].args) < 2:
        raise AssertionError('the mutating_sources call passes no paths argument')
    argument = calls[0].args[1]
    if isinstance(argument, ast.Name):
        assignments = [node for node in ast.walk(tree)
                       if isinstance(node, ast.Assign)
                       and any(isinstance(target, ast.Name)
                               and target.id == argument.id
                               for target in node.targets)
                       and isinstance(node.value, (ast.List, ast.Tuple))]
        if len(assignments) != 1:
            raise AssertionError(
                f'{len(assignments)} literal-list assignments for {argument.id}')
        argument = assignments[0].value
    if not isinstance(argument, (ast.List, ast.Tuple)):
        raise AssertionError('the window paths are not a static list')
    return len(argument.elts)


# One row per #2929 fix-r2 roster member.  ``bounded_runs`` reads the
# runner's own stage shape live from the loaded module (table lengths and
# BOUND sizes are never restated here); ``stage`` is the per-run ``timeout=``
# literal, or a (module constant, value) pair for the runners that pass the
# timeout by name; ``guarded`` counts the window's guarded paths from the
# module or the call site's own bytes.  The three fail-closed holders carry
# ``hold`` instead of the stage arithmetic: their whole-window hold is capped
# by their own budget check, so the cap is the registered hold.
# ``budget`` is the rounded constant the runner declares; the family check
# below recomputes FAMILY_MAX and every member's tail from these same live
# numbers, so a table that grows without the budget turns this red.
RUNNERS = {
    # fix-r1's eight members, re-derived from the family max (sibling#1).
    'native-witness/negative.py': {
        'bounded_runs': lambda m: 3 * len(m.cases),
        'stage': 30, 'guarded': static_guarded, 'budget': 65700},
    'native-witness/negative_authentication.py': {
        'bounded_runs': lambda m: 3 * len(m.cases),
        'stage': 60, 'guarded': static_guarded, 'budget': 68400},
    'native-witness/negative_boundary.py': {
        'bounded_runs': lambda m: 3 * len(m.cases),
        'stage': 60, 'guarded': static_guarded, 'budget': 65700},
    'session-domain-lock-release/negative_controls.py': {
        'bounded_runs': lambda m: 3 * len(m.MUTATIONS),
        'stage': 180, 'guarded': static_guarded, 'budget': 65700},
    'session-domain-native/negative_controls.py': {
        # baseline + mutant + unaffected + restored: every mutation but the
        # batch bypass has an unaffected control, so four is the worst case.
        'bounded_runs': lambda m: 4 * len(m.MUTATIONS),
        'stage': 300, 'guarded': static_guarded, 'budget': 65700},
    'session-domain-expectations/negative_controls.py': {
        # 2 * len(BOUND[name]) baseline+mutant runs plus one unaffected and
        # one restored run per mutation -- the BOUND sizes are read live.
        'bounded_runs': lambda m: sum(2 * len(m.BOUND[row[0]]) + 2
                                      for row in m.MUTATIONS),
        'stage': 300, 'guarded': static_guarded, 'budget': 65700},
    'gateway-batch/negative_controls.py': {
        'bounded_runs': lambda m: 3 * len(m.MUTATIONS),
        'stage': 120,
        'guarded': lambda m, t: len({m.COMMON} | {row[1] for row in m.MUTATIONS}),
        'budget': 66600},
    'gateway-entry-guard/negative_controls.py': {
        'bounded_runs': lambda m: 3 * len(m.MUTATIONS),
        'stage': 300,
        'guarded': lambda m, t: len({m.COMMON} | {row[1] for row in m.MUTATIONS}),
        'budget': 67500},
    # boundary#3's four call sites: the shared default 900 and an explicit 60.
    'session-lease/negative.py': {
        'bounded_runs': lambda m: 3 * len(m.cases),
        'stage': 30, 'guarded': static_guarded, 'budget': 65700},
    'parity-lock-release/negative_controls.py': {
        'bounded_runs': lambda m: 3 * len(m.MUTATIONS),
        'stage': 180,
        'guarded': lambda m, t: len({m.RUNNER} | {row[1] for row in m.MUTATIONS}),
        'budget': 65700},
    'frozen-manifest/negative_controls.py': {
        # #2929 fix-r3: the gateway-parity rows share ONE window (they guard
        # the same file pair, with a per-row restore between rows); every
        # other row keeps its own window.  The shared window is the largest:
        # positive + negative per gateway row, restored runs outside.
        'bounded_runs': lambda m: 2 * max(
            sum(1 for row in m.mutations if row[1].parent.name == 'gateway-parity'), 1),
        'stage': 60,
        # #2929 fix-r4 (MM boundary#5): the guarded count is the runner's own
        # window_paths over the gateway group -- derived live at the same
        # place the runner derives it, never a formula replica here (the old
        # ``1 + int(any(...))`` restated the window's path count and would
        # drift silently if the table changed).
        'guarded': lambda m, t: len(m.window_paths(
            [row for row in m.mutations
             if row[1].parent.name == 'gateway-parity'])) or 1,
        'budget': 64800},
    'frozen-reinsert-guard/negative_controls.py': {
        'bounded_runs': lambda m: 3 * len(m.MUTATIONS),
        'stage': 60,
        'guarded': lambda m, t: len({row[1] for row in m.MUTATIONS}
                                    | {m.COMMON, m.TEST, m.PROBE, Path(m.__file__)}),
        'budget': 69300},
    # the three pre-existing own-constant holders (sibling#6/#7/#8).
    'same-json-surfaces/negative_controls.py': {
        'hold': lambda m: m.LEASE_BUDGET_SECONDS,
        'stage': ('RUN_TIMEOUT_SECONDS', 60),
        'guarded': static_guarded, 'budget': 65700},
    'session-domain-cleanup/negative_controls.py': {
        'hold': lambda m: m.LEASE_BUDGET_SECONDS,
        'stage': ('CHECK_TIMEOUT_SECONDS', 180),
        'guarded': lambda m, t: len({m.COMMON_SOURCE}
                                     | {Path(row[1]) for row in m.MUTATIONS}),
        'budget': 65700},
    'native-session-e2e/negative_controls.py': {
        # fix-r3 (#2929 review): the round passes each per-case acquire the
        # remaining time of one monotonic deadline, not the full constant --
        # the call-shape pin and the deadline pin live in this file.
        'constant': 'LEASE_ACQUIRE_SECONDS',
        'call_shape': 'round_deadline',
        'hold': lambda m: m.PER_CASE_LEASE_SECONDS,
        'stage': ('BOUND_TEST_SECONDS', 300),
        'guarded': static_guarded, 'budget': 66600},
}

# Scan candidates that never take the host-wide lease (boundary#6): their
# exclusion is part of the closed set, so the roster test re-checks that each
# of them is still a scan candidate and that its reason still holds -- every
# prose number below is pinned by a machine check in the roster test, so a
# call-count drift turns the test red instead of silently invalidating the
# reason (fix-r3, MM boundary#8 ≡ DS boundary#2).
EXCLUDED = {
    'src/pilotdeck-gateway/tests/fixtures/test_negative_process.py': {
        'reason':
            'every mutating_sources call runs against TemporaryDirectory scratch '
            'git repos; the ParityMutationLock fixture copies common.py into the '
            'temp root and patches the lock path, so none takes /tmp/pdrs-parity.lock',
        # 13 real calls + the one inside the SIGKILL_RESIDUE script string.
        'text_calls': 14,
        'ast_calls': 13,
        # The patch that keeps the fixture off the real lock: both real-lock
        # literals in the file are the fixture's assert+replace pair.
        'lock_literal': "Path('/tmp/pdrs-parity.lock')",
        'lock_literal_count': 2,
        'patch_anchor': "original.replace(b\"Path('/tmp/pdrs-parity.lock')\"",
    },
    'src/pilotdeck-gateway/tests/test_metadata_io_driver.py': {
        'reason':
            'its one call guards src/pilotdeck-gateway/tests/fixtures/'
            'negative-restore/test_restore.py, not a gateway-parity source; the '
            'gateway-parity string literals in the file name unrelated fixtures',
        # The other four textual occurrences are quoted strings (docstring,
        # mutation-table rows, stub), not calls.
        'text_calls': 5,
        'ast_calls': 1,
        'guarded_anchor':
            "relative = 'src/pilotdeck-gateway/tests/fixtures/"
            "negative-restore/test_restore.py'",
    },
}


def call_site_problems(source, constant=CONSTANT, shape='constant'):
    """Why ``source`` fails the explicit-budget pin (empty list: it passes).

    ``shape`` is the runner's ``call_shape`` (default ``'constant'``): the
    budget is passed either as the constant name, or -- for the
    ``round_deadline`` runners (fix-r3) -- as the remaining-time call
    ``lease_wait_left(lease_deadline)`` computed from one monotonic round
    deadline, so N queued cases cannot multiply the constant into an
    unbounded wait.  The helper and the deadline assignment are pinned
    separately, in test_round_deadline_runner_waits_on_one_monotonic_deadline.
    """
    tree = ast.parse(source)
    calls = [node for node in ast.walk(tree) if isinstance(node, ast.Call)
             and isinstance(node.func, ast.Name)
             and node.func.id == 'mutating_sources']
    if len(calls) != 1:
        return [f'expected exactly one mutating_sources call, found {len(calls)}']
    for keyword in calls[0].keywords:
        if keyword.arg == 'lock_timeout':
            if shape == 'round_deadline':
                if (isinstance(keyword.value, ast.Call)
                        and isinstance(keyword.value.func, ast.Name)
                        and keyword.value.func.id == 'lease_wait_left'
                        and len(keyword.value.args) == 1
                        and isinstance(keyword.value.args[0], ast.Name)
                        and keyword.value.args[0].id == 'lease_deadline'):
                    return []
                return ['lock_timeout is not the remaining-time form '
                        'lock_timeout=lease_wait_left(lease_deadline)']
            if isinstance(keyword.value, ast.Name) and keyword.value.id == constant:
                return []
            return [f'lock_timeout is not passed as the {constant} name']
    return ['the mutating_sources call site relies on the shared default '
            f'lock_timeout ({SHARED_DEFAULT_LOCK_TIMEOUT}): '
            'the acquire budget must be explicit']


def stage_timeout_problems(source, spec):
    """Why ``source`` fails the stage-timeout pin (empty list: it passes).

    The literal is matched with digit anchors on both sides so a tenfold
    bump (``timeout=60`` -> ``timeout=600``) cannot keep the old pin green
    (boundary#4); the name-passing runners are pinned on the name itself and
    on the loaded module's value.
    """
    stage = spec['stage']
    if isinstance(stage, tuple):
        name, _value = stage
        if not re.search(r'(?<![\w.])timeout=' + name + r'(?![\w.])', source):
            return [f'no timeout={name} kwarg in the launch helper']
        return []
    if not re.search(r'(?<![\d.])timeout=' + str(stage) + r'(?![\d.])', source):
        return [f'the stage timeout literal timeout={stage} is missing']
    return []


def load_runner(relative):
    path = FIXTURES / relative
    spec = importlib.util.spec_from_file_location(
        'lease_budget_' + relative.replace('/', '_').replace('.', '_'), path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


_ROSTER = None


def family_roster():
    """[(relative, spec, module, source, tree)] for every roster member."""
    global _ROSTER
    if _ROSTER is None:
        _ROSTER = []
        for relative, spec in RUNNERS.items():
            source = (FIXTURES / relative).read_text(encoding='utf-8')
            _ROSTER.append((relative, spec, load_runner(relative),
                           source, ast.parse(source)))
    return _ROSTER


def stage_value(module, spec):
    stage = spec['stage']
    if isinstance(stage, tuple):
        return getattr(module, stage[0])
    return stage


def tail_seconds(module, tree, spec):
    return (spec['guarded'](module, tree)
            * (GIT_TIMEOUT + 2 * KILL_GRACE_SECONDS)
            + FSYNC_AND_LEASE_SLACK_SECONDS)


def hold_bound(module, tree, spec):
    """This member's worst whole-window hold on the global lease."""
    if 'hold' in spec:
        return spec['hold'](module)
    return (spec['bounded_runs'](module)
            * (stage_value(module, spec) + 2 * KILL_GRACE_SECONDS)
            + tail_seconds(module, tree, spec))


def _path_spellings(node):
    """The literal path strings an expression can spell; ``''`` is a root.

    Covers the spellings a guarded-path argument (or any mention) may use:
    string constants, ``/``-join chains, ``.joinpath()`` calls, ``Path()``
    constructors, ``os.path.join`` folds, string concatenation
    (``'scripts' + '/gateway-parity/' + 'common.py'`` folds to the joined
    spelling), sequence concatenation (``[a] + more``) and comprehensions
    whose items are opaque.  An
    unknown base -- a name like ROOT, a call like ``Path(__file__)`` or
    ``(...).resolve()``, or a subscript like ``parents[5]`` -- contributes
    the empty prefix, so ``Path(__file__).resolve().parents[5] / 'scripts'
    / 'gateway-parity'`` resolves to ``scripts/gateway-parity`` the way
    the runtime resolves a guarded path against the repository root
    (#2929 fix-r4, DS sibling#2: the old resolver returned None for call
    and subscript roots, so a future taker could spell its way out of
    the closed roster with a joined ``Path(...)`` base).  ``None`` marks a
    shape this resolver refuses to guess (f-strings, arithmetic, ...);
    the guarded-argument check fails closed on those instead of skipping.

    One documented residual class stays non-fail-closed by design (fix-r5,
    DS boundary#2): an unknown ``ast.Call`` -- a container call like
    ``sorted(...)`` / ``list(...)`` / ``tuple(...)`` / ``set(...)`` or an
    opaque helper from another module -- folds to the root ``''`` instead
    of ``None``, because roughly twenty real call sites rely on that
    branch to stay readable.  A concatenation whose right piece spells
    its own leading slash (``str(ROOT) + '/scripts/gateway-parity/x'``)
    is folded too (fix-r6, DS boundary#2): the joined spelling is
    absolute, yet the runtime resolves it against the repo root and takes
    the lease, so the de-slashed join stays in the fold (``/``-joins do
    not need the same treatment: an absolute right operand replaces the
    base in pathlib, a path the runtime itself rejects as outside-root).
    The sequence-concatenation cross products are a documented
    over-approximation (fix-r6, DS boundary#5): they can only add
    spellings, so a stray cross product can at worst fail the scan
    closed -- a false FAIL -- never pass a real lease taker silently.
    """
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        return {node.value}
    if isinstance(node, ast.BinOp):
        if isinstance(node.op, ast.Div):
            lefts, rights = _path_spellings(node.left), _path_spellings(node.right)
            if lefts is None or rights is None:
                return None
            return {'/'.join((left, right)) if left and right else (left or right)
                    for left in lefts for right in rights}
        if isinstance(node.op, ast.Add):
            # String concatenation folds the pieces (``'scripts' +
            # '/gateway-parity/'`` -> ``'scripts/gateway-parity/'``), so a
            # split prefix cannot spell its way out of the closed roster
            # (#2929 fix-r5, DS boundary#1: the plain union dropped exactly
            # that joined value).  #2929 fix-r6 (DS boundary#2): a right
            # piece that spells its own leading slash (``str(ROOT) +
            # '/scripts/gateway-parity/x'``) joins to an absolute spelling
            # the predicate below does not match, yet the runtime resolves
            # that absolute path against the repo root and takes the lease
            # -- the de-slashed join is folded too, so such a site stays a
            # mention.  Sequence concatenation (``[a, b] + more_paths``)
            # keeps the union -- either side may carry the spelling -- and
            # the cross products only add spellings, so the result still
            # over-approximates toward a mention, the fail-safe direction
            # for a closed-set scan.
            lefts, rights = _path_spellings(node.left), _path_spellings(node.right)
            if lefts is None or rights is None:
                return None
            joined = {left + right for left in lefts for right in rights}
            joined |= {left + right.lstrip('/')
                       for left in lefts for right in rights
                       if right.startswith('/')}
            return lefts | rights | joined
        return None
    if isinstance(node, (ast.List, ast.Tuple, ast.Set)):
        spellings = set()
        for element in node.elts:
            resolved = _path_spellings(element)
            if resolved is None:
                return None
            spellings |= resolved
        return spellings or {''}
    if isinstance(node, ast.Starred):
        return _path_spellings(node.value)
    if isinstance(node, (ast.ListComp, ast.SetComp, ast.GeneratorExp)):
        # The iterated items are opaque at scan time; the element
        # expression's own name lookups resolve to roots.
        return _path_spellings(node.elt)
    if isinstance(node, ast.Call):
        func, arguments = node.func, node.args
        if isinstance(func, ast.Attribute) and func.attr == 'joinpath' and arguments:
            joined = _path_spellings(func.value)
        elif (isinstance(func, ast.Attribute) and func.attr == 'join'
              and isinstance(func.value, ast.Attribute)
              and func.value.attr == 'path'
              and isinstance(func.value.value, ast.Name)
              and func.value.value.id == 'os'):
            # os.path.join(root, 'scripts', 'gateway-parity', x): fold the
            # arguments with / semantics, an opaque root staying neutral.
            joined = {''}
        elif isinstance(func, ast.Name) and func.id == 'Path':
            # Path('scripts', 'gateway-parity') folds like a join; a Path
            # over an opaque value (Path(__file__)) resolves to a root.
            joined = {''}
        else:
            # Any other call -- Path(__file__), (...).resolve(), an opaque
            # helper -- stands for a root.
            return {''}
        if joined is None:
            return None
        for argument in arguments:
            spellings = _path_spellings(argument)
            if spellings is None:
                return None
            joined = {'/'.join((prefix, spelling)) if prefix and spelling
                      else (prefix or spelling)
                      for prefix in joined for spelling in spellings}
        return joined
    if isinstance(node, ast.Subscript):
        # parents[5] and friends: the base is a root, the index is not a
        # path segment.
        return {''}
    # A name, attribute or any other opaque base stands for a root.
    if isinstance(node, (ast.Name, ast.Attribute)):
        return {''}
    return None


def _is_gateway_parity_spelling(spelling):
    """Does this spelled path fall under the runtime lease predicate?"""
    return (spelling == 'scripts/gateway-parity'
            or spelling.startswith('scripts/gateway-parity/'))


def _mutating_sources_calls(tree):
    """Every mutating_sources Call node (bare, qualified or aliased)."""
    aliases = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.ImportFrom):
            for name in node.names:
                if name.name == 'mutating_sources' and name.asname:
                    aliases.add(name.asname)
    return [node for node in ast.walk(tree) if isinstance(node, ast.Call)
            and ((isinstance(node.func, ast.Name)
                  and (node.func.id == 'mutating_sources' or node.func.id in aliases))
                 or (isinstance(node.func, ast.Attribute)
                     and node.func.attr == 'mutating_sources'))]


def _calls_mutating_sources(tree):
    """True if the AST calls mutating_sources bare, qualified or aliased."""
    return bool(_mutating_sources_calls(tree))


def _unresolvable_guarded_arguments(calls):
    """The (call, node) pairs whose guarded paths this resolver cannot read.

    A ``mutating_sources`` call whose ``paths`` argument (second positional
    or ``paths=`` keyword) spells an f-string or another refused shape can
    be neither cleared nor accused of guarding a gateway-parity path --
    exactly the silent-None skip a future lease taker could hide behind
    (#2929 fix-r4, DS sibling#2/MM boundary#4, two-family P1).  Callers
    fail closed on a non-empty result.
    """
    problems = []
    for call in calls:
        if len(call.args) >= 2:
            argument = call.args[1]
        else:
            argument = next((keyword.value for keyword in call.keywords
                             if keyword.arg == 'paths'), None)
        if argument is None:
            problems.append((call, None))
            continue
        nodes = (list(argument.elts) if isinstance(argument, (ast.List, ast.Tuple, ast.Set))
                 else [argument])
        for node in nodes:
            if _path_spellings(node) is None:
                problems.append((call, node))
    return problems


def global_lease_call_sites(repo_root):
    """Repo-relative paths of every file that takes the global lease.

    A candidate is a ``.py`` under ``src/`` or ``scripts/`` that calls
    ``mutating_sources`` (bare, qualified or aliased) and spells a
    ``scripts/gateway-parity`` path -- a string constant with the segment or
    a ``/``-join / ``joinpath`` chain over it, the spellings the runtime
    predicate (``negative_process.py``: resolved relative path
    ``startswith("scripts/gateway-parity/")``) accepts (fix-r3: the old
    trailing-slash-literal precondition silently missed the joined form;
    fix-r4: call and subscript bases resolve as roots, so an inline
    ``Path(__file__).resolve().parents[5] / ...`` spelling cannot slip out).
    ``EXCLUDED`` entries are *not* skipped here: exclusion happens after
    candidacy, in the roster test.
    """
    found = set()
    try:
        self_relative = Path(__file__).resolve().relative_to(repo_root).as_posix()
    except ValueError:
        # The scan root (e.g. the synthetic red side's temporary tree) does
        # not contain this file; there is nothing to skip.
        self_relative = None
    for base in ('src', 'scripts'):
        for path in sorted((repo_root / base).rglob('*.py')):
            relative = path.relative_to(repo_root).as_posix()
            if relative == self_relative:
                continue
            text = path.read_text(encoding='utf-8')
            if 'mutating_sources' not in text:
                continue
            tree = ast.parse(text)
            calls = _mutating_sources_calls(tree)
            if not calls:
                continue
            for _call, node in _unresolvable_guarded_arguments(calls):
                where = 'no paths argument at all' if node is None else (
                    f'line {node.lineno}: {ast.unparse(node)}')
                raise SystemExit(
                    f'closed-set scan: {relative}: mutating_sources paths '
                    f'argument cannot be resolved ({where}); spell it as '
                    'literals, joins, names or opaque roots so the roster '
                    'can read it')
            mentions = False
            for node in ast.walk(tree):
                if isinstance(node, ast.Constant) and isinstance(node.value, str):
                    if _is_gateway_parity_spelling(node.value):
                        mentions = True
                        break
                if isinstance(node, (ast.BinOp, ast.Call)):
                    spellings = _path_spellings(node)
                    if spellings and any(_is_gateway_parity_spelling(spelling)
                                         for spelling in spellings):
                        mentions = True
                        break
            if mentions:
                found.add(relative)
    return found


class LeaseAcquireBudgetTests(unittest.TestCase):
    maxDiff = None

    def _without_kwarg(self, source, constant):
        """Drop the real call's lock_timeout kwarg via AST, not text.

        The e2e runner's mutation table quotes its own call line verbatim
        above the call itself, so a textual ``count=1`` removal would hit the
        quoted row and leave the real call untouched.
        """
        tree = ast.parse(source)
        calls = [node for node in ast.walk(tree) if isinstance(node, ast.Call)
                 and isinstance(node.func, ast.Name)
                 and node.func.id == 'mutating_sources']
        calls[0].keywords = [keyword for keyword in calls[0].keywords
                             if keyword.arg != 'lock_timeout']
        return ast.unparse(tree)

    def test_roster_is_the_closed_set_of_global_lease_call_sites(self):
        marker = ('#2929 fix-r2: the roster is the closed set of '
                  'global-lease call sites')
        self.assertEqual(len(RUNNERS), 15, marker + ': roster count')
        found = global_lease_call_sites(REPO_ROOT)
        # Exclusion happens AFTER candidacy (fix-r3, MM boundary#3): a file
        # hidden in EXCLUDED must still show up as a candidate here, or the
        # closed set could silently swallow a real lease taker.
        self.assertLessEqual(set(EXCLUDED), found,
                             marker + ': a pinned exclusion is no longer a '
                             'scan candidate; re-derive its reason or drop it')
        runners_found = {relative for relative in found if relative not in EXCLUDED}
        fixtures_found = {relative[len(FIXTURES_REL) + 1:]
                          if relative.startswith(FIXTURES_REL + '/') else relative
                          for relative in runners_found}
        self.assertEqual(fixtures_found, set(RUNNERS),
                         marker + ': tree scan and roster disagree')
        # Every exclusion must still be a scan candidate that justifies its
        # entry, and its justification must be machine-checked (fix-r3):
        # prose call counts drift, and a drifted reason guards nothing.
        for relative, entry in EXCLUDED.items():
            with self.subTest(excluded=relative):
                self.assertTrue(entry['reason'].strip(), marker + ': empty reason')
                path = REPO_ROOT / relative
                self.assertTrue(path.is_file(),
                                marker + f': {relative} does not exist')
                text = path.read_text(encoding='utf-8')
                self.assertIn(relative, found,
                              marker + f': {relative} is no candidate')
                self.assertEqual(text.count('mutating_sources('), entry['text_calls'],
                                 marker + f': {relative}: mutating_sources call '
                                 'count drifted from the pinned exclusion reason')
                calls = _mutating_sources_calls(ast.parse(text))
                self.assertEqual(len(calls), entry['ast_calls'],
                                 marker + f': {relative}: AST call count drifted')
                # Fail closed here too (#2929 fix-r4): an unresolvable paths
                # argument can be neither cleared nor accused, so the
                # "guards only temp-root copies" claim is unverifiable.
                for _call, node in _unresolvable_guarded_arguments(calls):
                    where = ('no paths argument at all' if node is None
                             else f'line {node.lineno}: {ast.unparse(node)}')
                    self.fail(marker + f': {relative}: the mutating_sources '
                              f'paths argument ({where}) cannot be resolved; '
                              'the exclusion claim is unverifiable')
                # None of its mutating_sources calls may carry a guarded
                # path that resolves under the repo's scripts/gateway-parity/
                # -- the scratch-repo calls reach the temp root's copy, whose
                # lock path the fixture pins to a temporary lock.
                for call in calls:
                    for argument in call.args:
                        for node in ast.walk(argument):
                            if isinstance(node, ast.Constant) and isinstance(node.value, str):
                                spellings = {node.value}
                            elif isinstance(node, (ast.BinOp, ast.Call)):
                                spellings = _path_spellings(node)
                            else:
                                continue
                            if spellings and any(_is_gateway_parity_spelling(spelling)
                                                 for spelling in spellings):
                                self.fail(marker + f': {relative}: a mutating_sources '
                                          'call guards a repo gateway-parity path')
                if 'lock_literal' in entry:
                    self.assertEqual(text.count(entry['lock_literal']),
                                     entry['lock_literal_count'],
                                     marker + f': {relative}: real-lock literal '
                                     'count drifted from the patch pin')
                    self.assertIn(entry['patch_anchor'], text,
                                  marker + f': {relative}: the lock-path patch '
                                  'anchor is gone')
                if 'guarded_anchor' in entry:
                    self.assertIn(entry['guarded_anchor'], text,
                                  marker + f': {relative}: the guarded-path '
                                  'binding anchor is gone')

    def test_scanner_finds_a_synthetic_gateway_parity_candidate(self):
        """Red side (fix-r3, MM boundary#7 ≡ DS boundary#2): a candidate the
        old trailing-slash scan silently missed -- an aliased import plus a
        ``ROOT / 'scripts/gateway-parity' / x`` joined path with no
        slash-bearing literal anywhere -- must be found by the scanner."""
        marker = ('#2929 fix-r3: the closed-set scan catches joined-path, '
                  'aliased-call candidates')
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / 'src' / 'runner').mkdir(parents=True)
            (root / 'src' / 'runner' / 'negative_controls.py').write_text(
                "from pathlib import Path\n"
                "from negative_process import mutating_sources as ms\n"
                "ROOT = Path(__file__).resolve().parents[2]\n"
                "COMMON = ROOT / 'scripts' / 'gateway-parity' / 'common.py'\n"
                "with ms(ROOT, [COMMON], lock_timeout=900):\n"
                "    pass\n")
            found = global_lease_call_sites(root)
            self.assertEqual(found, {'src/runner/negative_controls.py'}, marker)

    def test_path_spellings_resolve_call_and_subscript_roots(self):
        """#2929 fix-r4 (DS sibling#2, P1): the resolver itself.

        Call bases (``Path(__file__)``, ``(...).resolve()``) and subscript
        bases (``parents[5]``) must contribute the root prefix -- the old
        resolver returned None for them, so a future lease taker could
        spell its guarded path as an inline ``Path(...)`` chain and slip
        out of the closed roster.  ``Path(...)`` and ``os.path.join`` fold
        their arguments; refused shapes still return None so the scan can
        fail closed on them.
        """
        marker = '#2929 fix-r4: call and subscript bases resolve as roots'

        def spellings(expression):
            return _path_spellings(ast.parse(expression, mode='eval').body)

        self.assertEqual(
            spellings("Path(__file__).resolve().parents[5] / 'scripts' "
                      "/ 'gateway-parity' / 'common.py'"),
            {'scripts/gateway-parity/common.py'}, marker)
        self.assertEqual(
            spellings("Path(__file__).resolve().joinpath('scripts', "
                      "'gateway-parity', 'common.py')"),
            {'scripts/gateway-parity/common.py'}, marker)
        self.assertEqual(
            spellings("os.path.join(ROOT, 'scripts', 'gateway-parity', NAME)"),
            {'scripts/gateway-parity'}, marker)
        self.assertEqual(spellings("Path('scripts', 'gateway-parity')"),
                         {'scripts/gateway-parity'}, marker)
        self.assertEqual(spellings("Path(__file__)"), {''}, marker)
        # Sequence shapes resolve conservatively (opaque items are roots).
        self.assertEqual(spellings("[COMMON, ROOT / 'x', *more]"),
                         {'', 'x'}, marker)
        self.assertEqual(spellings("[row[1] for row in selected]"), {''}, marker)
        # Refused shapes stay None: the scan fails closed on them instead
        # of guessing (f-strings, arithmetic).
        self.assertIsNone(spellings("f'scripts/gateway-parity/{name}'"), marker)
        self.assertIsNone(spellings("BASE - 'x'"), marker)

    def test_path_spellings_fold_string_concatenation(self):
        """#2929 fix-r5 (DS boundary#1, P1): the resolver itself.

        String concatenation must fold to the joined spelling -- the old
        plain union returned the pieces, so ``'scripts' +
        '/gateway-parity/' + NAME`` spelled its way out of the closed
        roster without tripping the fail-closed None.  Sequence
        concatenation keeps the union spellings (the right side is an
        opaque root), pinned here so the fold cannot regress it.
        """
        marker = '#2929 fix-r5: string concatenation folds to the joined spelling'

        def spellings(expression):
            return _path_spellings(ast.parse(expression, mode='eval').body)

        # Red side: the split prefix folds into the predicate's spelling.
        split = spellings("'scripts' + '/gateway-parity/' + NAME")
        self.assertIn('scripts/gateway-parity/', split, marker)
        self.assertTrue(any(_is_gateway_parity_spelling(value) for value in split),
                        marker + ': the folded spelling must hit the predicate')
        # The pieces survive as spellings (over-approximation), and a fully
        # literal concatenation folds to the exact joined constant among
        # them.
        folded = spellings("'scripts' + '/gateway-parity/' + 'common.py'")
        self.assertIn('scripts/gateway-parity/common.py', folded, marker)
        self.assertTrue(any(_is_gateway_parity_spelling(value) for value in folded),
                        marker + ': the folded literal must hit the predicate')
        # Sequence concatenation is unchanged: the opaque right side keeps
        # contributing the root spelling alongside the literal items.
        self.assertEqual(spellings("['a.py'] + more"), {'', 'a.py'}, marker)
        self.assertEqual(spellings("[A, B] + more_paths"), {''}, marker)
        # A root base concatenated with a segment folds like the runtime's
        # resolve-against-root.
        self.assertEqual(spellings("BASE + 'x.py'"), {'', 'x.py'}, marker)
        # fix-r6 (DS boundary#2, P1): a right piece spelling its own
        # leading slash joins to an absolute spelling the predicate does
        # not match, but the runtime resolves that absolute path against
        # the repo root and takes the lease -- the de-slashed join must
        # stay folded so the closed roster keeps failing toward a mention.
        absolute = spellings("str(ROOT) + '/scripts/gateway-parity/x'")
        self.assertIn('/scripts/gateway-parity/x', absolute, marker)
        self.assertIn('scripts/gateway-parity/x', absolute, marker)
        self.assertTrue(any(_is_gateway_parity_spelling(value)
                            for value in absolute),
                        marker + ': the de-slashed join must hit the '
                        'predicate')

    def test_scanner_finds_inline_call_base_candidates(self):
        """Red side (fix-r4, DS sibling#2/MM boundary#4): candidates the old
        resolver silently missed -- the guarded path spelled inline at the
        call with a ``Path(__file__).resolve().parents[5]`` base, an
        ``os.path.join`` fold and a ``Path('scripts', ...)`` constructor,
        with no name-bound joined chain anywhere in the file."""
        marker = ('#2929 fix-r4: the closed-set scan catches inline '
                  'Path/os.path.join-base candidates')
        shapes = {
            'inline-parents': ("from pathlib import Path\n"
                               "from negative_process import mutating_sources as ms\n"
                               "with ms(Path(__file__).resolve().parents[5],\n"
                               "        [Path(__file__).resolve().parents[5] / 'scripts'\n"
                               "         / 'gateway-parity' / 'common.py'], lock_timeout=900):\n"
                               "    pass\n"),
            'os-path-join': ("import os\n"
                             "from negative_process import mutating_sources as ms\n"
                             "with ms(os.getcwd(),\n"
                             "        [os.path.join(os.getcwd(), 'scripts',\n"
                             "                      'gateway-parity', 'common.py')],\n"
                             "        lock_timeout=900):\n"
                             "    pass\n"),
            'path-constructor': ("from pathlib import Path\n"
                                 "from negative_process import mutating_sources as ms\n"
                                 "with ms(Path.cwd(),\n"
                                 "        [Path(Path.cwd(), 'scripts',\n"
                                 "              'gateway-parity', 'common.py')],\n"
                                 "        lock_timeout=900):\n"
                                 "    pass\n"),
            'string-concat': ("from negative_process import mutating_sources as ms\n"
                              "with ms(ROOT,\n"
                              "        ['scripts' + '/gateway-parity/' + 'common.py'],\n"
                              "        lock_timeout=900):\n"
                              "    pass\n"),
            'leading-slash-concat': ("from negative_process import mutating_sources as ms\n"
                                     "with ms(ROOT,\n"
                                     "        [str(ROOT) + '/scripts/gateway-parity/common.py'],\n"
                                     "        lock_timeout=900):\n"
                                     "    pass\n"),
        }
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            for name, text in shapes.items():
                (root / 'src' / name).mkdir(parents=True)
                (root / 'src' / name / 'negative_controls.py').write_text(text)
            found = global_lease_call_sites(root)
        self.assertEqual(found, {'src/' + name + '/negative_controls.py'
                                 for name in shapes}, marker)

    def test_scanner_fails_closed_on_unresolvable_guarded_argument(self):
        """Red side (fix-r4): a mutating_sources call whose paths argument
        the resolver refuses to read must fail the scan loudly, not skip
        silently -- the silent None skip was exactly the hole a future
        taker could hide behind."""
        marker = ('#2929 fix-r4: an unresolvable guarded argument fails the '
                  'closed-set scan closed')
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / 'src' / 'runner').mkdir(parents=True)
            (root / 'src' / 'runner' / 'negative_controls.py').write_text(
                "from negative_process import mutating_sources as ms\n"
                "NAME = 'common.py'\n"
                "with ms(ROOT, [f'scripts/gateway-parity/{NAME}'],\n"
                "        lock_timeout=900):\n"
                "    pass\n")
            with self.assertRaises(SystemExit, msg=marker) as caught:
                global_lease_call_sites(root)
            self.assertIn('cannot be resolved', str(caught.exception), marker)

    def test_call_sites_pass_the_budget_constant(self):
        marker = '#2929 fix-r2: every guarded runner passes its acquire budget explicitly'
        for relative, spec, _module, source, _tree in family_roster():
            with self.subTest(runner=relative):
                constant = spec.get('constant', CONSTANT)
                self.assertEqual(
                    call_site_problems(source, constant, spec.get('call_shape', 'constant')),
                    [], marker + ': ' + relative)

    def test_call_site_pin_rejects_the_default_fallback(self):
        marker = '#2929 fix-r2: the call-site pin must fire on the default-900 fallback'
        for relative, spec, _module, source, _tree in family_roster():
            with self.subTest(runner=relative):
                constant = spec.get('constant', CONSTANT)
                shape = spec.get('call_shape', 'constant')
                self.assertEqual(call_site_problems(source, constant, shape), [],
                                 marker + ': pristine source must pass first')
                # Damage shapes, the same proof shape as the e2e runner's
                # #1535/#2929 pins.  Constant shape: the kwarg dropped
                # entirely (the silent shared-default fallback #1529 let
                # through), the shared default spelled out, and a foreign
                # constant name.  Round-deadline shape (fix-r3): the kwarg
                # dropped, the full constant reappearing where the remaining
                # time belongs, and a foreign deadline name.
                if shape == 'round_deadline':
                    damages = (
                        ('kwarg dropped', self._without_kwarg(source, constant)),
                        ('full constant instead of remaining time',
                         source.replace('lock_timeout=lease_wait_left(lease_deadline)',
                                        f'lock_timeout={constant}')),
                        ('foreign deadline name',
                         source.replace('lock_timeout=lease_wait_left(lease_deadline)',
                                        'lock_timeout=lease_wait_left(SOME_OTHER_DEADLINE)')),
                    )
                else:
                    damages = (
                        ('kwarg dropped', self._without_kwarg(source, constant)),
                        ('shared default 900',
                         source.replace(f'lock_timeout={constant}', 'lock_timeout=900')),
                        ('foreign name',
                         source.replace(f'lock_timeout={constant}',
                                        'lock_timeout=SOME_OTHER_BUDGET')),
                    )
                for damage, damaged in damages:
                    with self.subTest(damage=damage):
                        self.assertNotEqual(damaged, source,
                                            marker + f': {relative}: damage {damage} did not land')
                        self.assertNotEqual(call_site_problems(damaged, constant, shape), [],
                                            marker + f': {relative}: damage {damage} must be rejected')

    def test_round_deadline_runner_waits_on_one_monotonic_deadline(self):
        marker = ('#2929 fix-r3: the e2e runner hands every per-case acquire '
                  'the remaining time of one monotonic round deadline')
        row = next(item for item in family_roster()
                   if item[0] == 'native-session-e2e/negative_controls.py')
        _relative, spec, module, source, tree = row
        # The helper is the clamped remaining-time form, defined exactly once.
        helper = [node for node in tree.body
                  if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
                  and node.name == 'lease_wait_left']
        self.assertEqual(len(helper), 1, marker + ': lease_wait_left defined '
                         'more than once or missing')
        self.assertEqual(
            ast.unparse(helper[0]),
            'def lease_wait_left(deadline):\n'
            '    return max(deadline - time.monotonic(), 1)',
            marker + ': the helper must clamp the remaining time to >= 1 '
            '(parity_lock refuses a non-positive timeout; an exhausted round '
            'must fail through the TimeoutError path, not ValueError)')
        # The round sets exactly one deadline from the budget constant,
        # armed from the single enter_finished sample (fix-r6, DS
        # concurrency#1): the interval between two monotonic() calls
        # double-counted into the first refund and pushed the first
        # acquire's budget above the constant against the zero-tolerance
        # upper bound.
        self.assertEqual(len(re.findall(
            r'enter_finished = time\.monotonic\(\)\s*\n\s*'
            r'lease_deadline = enter_finished \+ ' + spec['constant'] + r'\b',
            source)), 1, marker + ': the round must set one monotonic '
            'deadline from ' + spec['constant'] + ', sampled once')
        # Behavior: an exhausted deadline still yields a positive budget, a
        # future one yields the remaining time.
        now = time.monotonic()
        self.assertEqual(module.lease_wait_left(now - 1000), 1, marker)
        left = module.lease_wait_left(now + 100)
        self.assertTrue(1 <= left <= 101, marker)

    def test_stage_timeout_pin_rejects_a_bumped_literal(self):
        marker = ('#2929 fix-r2: the stage pin must fire on a tenfold '
                  'timeout bump (boundary#4)')
        for relative, spec, _module, source, _tree in family_roster():
            with self.subTest(runner=relative):
                self.assertEqual(stage_timeout_problems(source, spec), [],
                                 marker + ': pristine source must pass first')
                stage = spec['stage']
                if isinstance(stage, tuple):
                    name, _value = stage
                    damaged = re.sub(r'(?<![\w.])timeout=' + name + r'(?![\w.])',
                                     'timeout=600', source)
                else:
                    damaged = re.sub(r'(?<![\d.])timeout=' + str(stage) + r'(?![\d.])',
                                     'timeout=' + str(stage * 10), source)
                self.assertNotEqual(damaged, source,
                                    marker + f': {relative}: the bump did not land')
                self.assertNotEqual(stage_timeout_problems(damaged, spec), [],
                                    marker + f': {relative}: the bumped stage must be rejected')

    def test_budgets_cover_the_family_max_hold_bound(self):
        marker = ('#2929 fix-r2: every acquire budget covers the family '
                  'max hold bound plus its own tail and one stage')
        roster = family_roster()
        family_max = max(hold_bound(module, tree, spec)
                         for _relative, spec, module, _source, tree in roster)
        for relative, spec, module, source, tree in roster:
            with self.subTest(runner=relative):
                # fix-r3 (MM sibling#3/#7): the family maximum is derived
                # live, above -- a hand-copied number in a runner comment
                # silently drifted once already (62,460 -> 62,320), so no
                # roster member may carry the number in prose.
                self.assertNotIn('62,320', source,
                                 marker + f': {relative}: the family maximum '
                                 'is derived live here, not restated in prose')
                # The stage timeout must still be the runner's own literal
                # or named constant: a silent timeout bump elsewhere must
                # not leave this pin computing a stale bound.
                self.assertEqual(stage_timeout_problems(source, spec), [],
                                 marker + f': {relative}: stage timeout pin failed')
                stage = spec['stage']
                if isinstance(stage, tuple):
                    self.assertEqual(getattr(module, stage[0]), stage[1],
                                     marker + f': {relative}: {stage[0]} drifted')
                constant = spec.get('constant', CONSTANT)
                self.assertTrue(hasattr(module, constant),
                                marker + f': {relative}: no {constant}')
                budget = getattr(module, constant)
                self.assertEqual(budget, spec['budget'],
                                 marker + f': {relative}: budget drifted from the registered value')
                tail = tail_seconds(module, tree, spec)
                own_stage = stage_value(module, spec)
                self.assertGreaterEqual(
                    budget, family_max + tail + own_stage,
                    marker + f': {relative}: {budget} < family max {family_max} '
                    f'+ own tail {tail}s + one stage {own_stage}s '
                    '(boundary#5: equal-to-the-bound is not a wait)')

    def test_shared_default_stays_the_documented_fallback(self):
        marker = '#2929 fix-r2: the shared default lock_timeout documents the fallback being refused'
        self.assertEqual(SHARED_DEFAULT_LOCK_TIMEOUT, 900, marker)


if __name__ == '__main__':
    unittest.main(verbosity=2)
