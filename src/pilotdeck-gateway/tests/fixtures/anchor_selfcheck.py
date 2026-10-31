#!/usr/bin/env python3
# anchor-selfcheck: unsupported scanner host, no executable product mutations
"""Read-only negative-control preflight; cargo is never run, no main() executes.

#996 fix-r3: switches-table schemata are the one deliberate exception to a
purely static scan. Each is imported once in an isolated ``python3 -I -B``
audit subprocess (module name is never ``__main__``, so ``main()`` and
``if __name__`` guards stay inert and no bytecode is written) and its final
executed ``switches`` table is reconciled row by row with the statically
extracted rows; any drift, import failure, timeout or malformed table fails
closed. The static whitelist below stays only as the fast first gate.

python3 -I .../anchor_selfcheck.py --anchors-only   # CI, report env gaps
python3 -I .../anchor_selfcheck.py                  # local, env gaps fail

Supported tables: (name, path, before, after[, count/test...]), single-source
(name, before, after...), and mutation(name, path, before, after, ...) calls.
Dictionary tables use name keys and tuple values; new keys may be added in
literal loops, but overwriting keys is rejected. Byte anchors count raw bytes.
Paths/constants are interpreted by a small AST allowlist. ANCHOR_COUNTS is an
optional nonempty {name: positive integer} declaration of *source occurrences*, distinct
from replace(..., 1)'s replacement limit. Unknown formats fail closed.

Snapshot names have one Assign or one official mutating_sources with-as binding.
An unread module-level empty dictionary before its sole official capture counts
as one binding. The exact exiting --check-anchors preflight below is a separate
path from its later capture. Assigns may also belong to a top-level function, without a file
allowlist; local dictionaries must be consumed in that same function scope.
Dictionary keys retain their read expression; unknown snapshot syntax fails closed.
Snapshot occurrences are allowlisted: canonical bindings, subscript reads and
exact dict(name) copies. Existing modeled originals[key] assignments, literal
pop(key, None) statements and read-only mapping calls remain compatibility forms.
The exact anchors(texts) helper below may inspect a decoded snapshot view.
Attribute names and string literals equal to a snapshot name are never aliases.
Read chains allow only the concrete methods in SNAPSHOT_READ_METHODS. This is a
syntax check, not execution or general Python alias/purity analysis.
"""
import argparse
import ast
import io
import json
from dataclasses import dataclass
from pathlib import Path
import re
import secrets
import subprocess
import sys
import tempfile
import time
import os
import tokenize

sys.path.insert(0, str(Path(__file__).resolve().parent))
import pending_reanchor as pending

TABLES = {'mutants', 'mutations', 'controls', 'cases', 'runner_mutations'}
# #996: migrated fixture runners declare compile-time switch bindings in a
# `switches = [...]` table of (name, path, test, assertion) rows. The counted
# anchor is the single `::mutants::on("<name>")` call site in that source file,
# not a text replacement; these names never enter TABLES because their rows
# have no before/after pair. fix-r2 makes the name a whitelist: only the one
# module-level literal declaration and the enumerated read-only uses below
# are supported, so aliases and dynamic rebinds cannot hide an update.
SWITCH_TABLE_NAMES = {'switches'}
# Schemata runners need none of these dynamic module forms; every checked
# script rejects them outright (#996 fix-r2), closing `'swi' + 'tches'`
# spellings that no occurrence whitelist can see.
SWITCH_DYNAMIC_SYMBOLS = {'setattr', 'globals', 'vars'}
# #996 fix-r3: a static whitelist still trusts call *names*, so a shadowed
# `len`, a rebound executor alias or an aliased `globals()` can shrink the
# executed table without one whitelisted occurrence (3rd round of the same
# hole; Python is dynamic, per-shape chasing cannot finish). The executed
# module is therefore the truth: every switches-table script is imported in
# an isolated audit subprocess (python3 -I -B; the module name below is never
# `__main__`, so main()/if __name__ blocks stay inert) and its final
# `switches` object is reconciled row by row with the static rows. The child
# answers with exactly one prefixed JSON audit record; duplicates (a forged
# decoy line), import failures, timeouts, non-list tables and malformed rows
# all fail closed. #996 fix-r4: a nonzero child exit is an error even with a
# single plausible record (an early `os._exit` after printing one), and the
# record must carry a per-run nonce that the parent generates and passes via
# argv; the bootstrap removes it from sys.argv *before* executing the module,
# so audited module code cannot read it and only the completed bootstrap can
# write an acceptable record. Forged audit output from deliberately
# obfuscating module code is out of scope (checker threat model,
# specs/negative-controls/README.md).
RUNTIME_SWITCH_PREFIX = 'PDRS_RUNTIME_SWITCHES '
RUNTIME_SWITCH_TIMEOUT = 30
RUNTIME_SWITCH_BOOTSTRAP = '''\
import importlib.util, json, os, sys
name = 'pdrs_switch_runtime_audit'
nonce = sys.argv.pop()
spec = importlib.util.spec_from_file_location(name, sys.argv[1])
module = importlib.util.module_from_spec(spec)
sys.modules[name] = module
document = {'nonce': nonce, 'rows': None, 'error': None}
try:
    spec.loader.exec_module(module)
    table = getattr(module, 'switches', None)
    if not isinstance(table, list):
        document['error'] = f'the imported switches binding is {type(table).__name__}, not a list'
    else:
        rows = []
        for row in table:
            if not isinstance(row, (list, tuple)) or len(row) != 4:
                document['error'] = 'a runtime switches row is not a (name, path, test, assertion) quadruple'
                break
            switch_name, switch_path, switch_test, switch_assertion = row
            if not isinstance(switch_name, str) or not switch_name:
                document['error'] = 'a runtime switches row has an empty or non-string name'
                break
            if not isinstance(switch_path, (str, os.PathLike)):
                document['error'] = 'a runtime switches row has a non-path mutation target'
                break
            if not ((isinstance(switch_test, str) and switch_test)
                    or (isinstance(switch_test, list) and switch_test
                        and all(isinstance(item, str) and item for item in switch_test))):
                document['error'] = 'a runtime switches row has an empty or non-string test binding'
                break
            if not ((isinstance(switch_assertion, str) and switch_assertion)
                    or (isinstance(switch_assertion, list) and switch_assertion
                        and all(isinstance(item, str) and item for item in switch_assertion))):
                document['error'] = 'a runtime switches row has an invalid assertion binding'
                break
            rows.append([switch_name, str(switch_path), switch_test, switch_assertion])
        document['rows'] = rows
except BaseException as error:
    document['error'] = f'import failed: {type(error).__name__}: {error}'
print('PDRS_RUNTIME_SWITCHES ' + json.dumps(document))
'''
# A script without a declarative, whole-file source replacement table declares
# its exact exemption in its own header: ``# anchor-selfcheck: unsupported
# <nonempty reason>`` within the first 20 lines. The reason lives next to the
# script, so a new control no longer edits one shared allowlist (the #933/#61
# conflicts). Missing reason, a marker after the limit, a marker without a
# script, and a tracked script missing from the working tree all fail closed.
MARKER_LINE = '# anchor-selfcheck: unsupported'
MARKER_LINE_LIMIT = 20
MARKER_PATTERN = re.compile(r'^#\s*anchor-selfcheck:\s*unsupported(?:\s+(.*))?$')
# Known table/call signals in intentionally unsupported formats. All other
# exempted helpers must remain free of these signals. The reason stays above;
# neither a new filename nor a reason alone can hide a source mutation table.
UNSUPPORTED_SIGNALS = {
    'native-witness/test_witness.py': set(),  # #1140 in-memory/raw-trace controls, no source mutation table
    # #1107: standalone M6 helpers; only test_contract declares a case table.
    '_negkit/contract.py': set(),
    '_negkit/test_contract.py': {'table:cases'},
    '_negkit/negative_contract.py': set(),
    '_negkit/toy_runner.py': set(),
    '_negkit/submit.py': set(),
    '_negkit/test_submit_migration.py': set(),
    '_negkit/negative_submit_migration.py': set(),
    # #994: meta-controls mutate disposable Python modules or in-memory loaders;
    # their import/bootstrap machinery is outside the product-anchor grammar.
    'negative-adaptive/negative_controls.py': {'table:cases'},
    'negative-adaptive/isolation_negative.py': {'table:CONTROLS'},
    'negative_process.py': set(),
    'workspace-pristine/test_controls.py': set(),  # #1166 synthetic Git/binary tests
    '_shared/negative_control.py': set(),
    'test_negative_adaptive.py': set(),
    'test_negative_adaptive_isolation.py': set(),
    'native-topology/run.py': {'table:CASES'},
    'native-topology/controls.py': {'call:mutate'},
    'negative_suite.py': {'call:mutate'},
    'test_negative_suite.py': {'table:cases'},
    'test_negative_panic.py': {'table:cases'},
    'session-domain-cleanup/check_cleanup.py': {'table:CONTROLS', 'table:cases', 'table:controls'},
    'negative_oracle_guard.py': {'table:CASES'},
    'edgeclaw-llm/test_controls.py': {'table:cases'},
    'turn-runner/negative_controls.py': {'table:mutations', 'call:mutate'},
    'turn-runner/runner_negative.py': {'table:mutations', 'call:mutate'},
    'turn-runner/input_negative.py': {'table:mutations', 'table:cases'},  # #741k fix-r1：运行路径走 negative_suite.run_cases（cases），--check 仍经 check_mutations 读 mutations
    'turn-runner/live_negative.py': {'table:mutations', 'table:cases'},  # #741k fix-r1：运行路径走 negative_suite.run_cases（cases），--check 仍经 check_mutations 读 mutations
    'runtime-spawn-panic/negative_controls.py': {'table:mutants', 'table:cases'},
    'plugin-runtime/negative-controls.py': {'table:CASES'},
    'plugin-runtime/panic-negative.py': {'table:CASES', 'table:cases'},
    'spill-isolation/negative_controls.py': {'call:mutate'},
    'skills/negative_controls.py': {'table:mutations'},
    # #996: the shared precompiled-switch executor and its self-tests own no
    # mutation table; registering the empty signal set pins that exemption.
    '_shared/mutant_switch.py': set(),
    '_shared/test_mutant_switch.py': set(),
    'edgeclaw-service-facade/test_switch_negative.py': set(),
    # #1036: same shape as the facade runner's self-tests — no mutation table.
    'edgeclaw-service-orchestration/test_switch_negative.py': set(),
    # #1019 fix-r1: same shape as the orchestration runner's self-tests.
    'session-adopt/test_switch_negative.py': set(),
}
REGEX_SCRIPTS = {'background-panic/negative_controls.py'}
IMPLICIT = {
    'hooks-output/negative.py': 'source',
    'session-owner-hooks/negative.py': 'source',
    'edgeclaw-file-memory/negative.py': 'source',
    'edgeclaw-heartbeat/negative.py': 'helpers',
    'edgeclaw-entries/negative_round7.py': 'path',
    'turn-submit/negative.py': 'source',
    'host-event-pump/negative_controls.py': 'path',
    'output-schema/negative.py': 'source',
}
# (name, path, package, test, before, after), unlike the usual table layout.
FIELD_ORDER = {'sidecar-payload/negative.py': (1, 4, 5)}
# These ordered rewrites cannot be counted as whole-file replacements. Keep
# them separate from the JSON-only provenance exceptions below.
SEQUENTIAL_ROWS = {
    # #996: the turn-gates ordered rows were deleted with negative.py; the
    # table stays empty until another control needs an ordered replacement.
}
ROW_EXCEPTIONS = {
    ('session-abort-contract/negative.py', 'tampered-source-hash'): 'JSON provenance corruption, not a source replacement',
    ('session-owner/negative.py', 'tampered-source-hash'): 'JSON provenance corruption, not a source replacement',
    ('session-admission/negative.py', 'tampered-source-hash'): 'JSON provenance corruption, not a source replacement',
    ('connection-context/negative.py', 'tampered-source-hash'): 'JSON provenance corruption, not a source replacement',
    ('user-dialog-channel/negative.py', 'tampered-source-hash'): 'JSON provenance corruption, not a source replacement',
}
SCOPED_ROWS = {
    ('turn-runner/recording_negative.py', 'text-value'): 'from_agent_input',
}
SOURCE_SUFFIXES = {'.rs', '.py', '.ts', '.tsx', '.js', '.jsx', '.mjs', '.cjs'}
SNAPSHOT_NAMES = {'originals', 'snapshots'}
# Fixture receivers use count/decode/encode/replace; strip is an existing
# modeled transform and split is retained for synthetic read-chain controls.
# This syntax allowlist also checks uncalled terminal attributes. value() has
# its own evaluable str/bytes methods; snapshot_target() has source-preserving
# transforms. Adding syntax here does not extend either narrower role.
SNAPSHOT_READ_METHODS = {'count', 'decode', 'encode', 'replace', 'split', 'strip'}

# These compatibility forms are complete AST shapes, not a general purity or
# control-flow analysis. Changes to their bodies must be reviewed explicitly.
SNAPSHOT_ANCHOR_HELPER = ast.parse('''
def anchors(texts):
    stale = []
    for name, path, before, _, _ in cases:
        count = texts[path].count(before)
        if count != 1:
            stale.append((name, count))
    assert set(messages) == {case[0] for case in cases}, 'every mutant needs its own named assertion'
    if stale:
        for name, count in stale: print(f'STALE ANCHOR {name}: {count} matches', file=sys.stderr)
        raise SystemExit(2)
''').body[0]
SNAPSHOT_DECODED_VIEW = ast.parse('''
originals = {path: snapshots[path].decode() for path in (source, text)}
''').body[0]
SNAPSHOT_PREFLIGHT = ast.parse('''
if sys.argv[1:] == ['--check-anchors']:
    originals = {path: path.read_text() for path in {case[1] for case in cases}}
    stale = []
    for name, path, before, _after, _test in all_cases:
        count = originals[path].count(before)
        if count != 1:
            stale.append((name, count, path.relative_to(root).as_posix()))
    if stale:
        for name, count, relative in stale:
            print(f'STALE ANCHOR {name}: {count} matches in {relative}')
        print('STALE ANCHORS: negative.py is out of sync with its sources; refusing to run')
        raise SystemExit(2)
    print(f'{len(all_cases)} anchors: ok')
    raise SystemExit(0)
''').body[0]


class Unsupported(ValueError):
    pass


def bound_names(node):
    """Include Python bindings whose names are strings rather than Name nodes."""
    if isinstance(node, ast.Name) and isinstance(node.ctx, (ast.Store, ast.Del)):
        return {node.id}
    if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
        return {node.name}
    if isinstance(node, ast.arg):
        return {node.arg}
    if isinstance(node, ast.alias):
        return {node.asname or node.name.split('.')[0]}
    if isinstance(node, (ast.ExceptHandler, ast.MatchAs, ast.MatchStar)):
        return {node.name} if node.name else set()
    if isinstance(node, ast.MatchMapping):
        return {node.rest} if node.rest else set()
    if isinstance(node, (ast.Global, ast.Nonlocal)):
        return set(node.names)
    # Added in Python 3.12; keep the checker usable with earlier interpreters.
    if type(node).__name__ in ('TypeVar', 'ParamSpec', 'TypeVarTuple'):
        return {node.name}
    return set()


@dataclass
class Anchor:
    name: str
    path: Path
    before: str | bytes
    count: int = 1
    regex: bool = False
    line: int = 0
    flags: int = 0
    scope: str = ''
    text_mutation: bool = True


class Static:
    """Interpret only pure literals, paths and string operations. No eval/exec."""
    def __init__(self, script, root, tree):
        self.script, self.root = script, root
        self.tree = tree
        self.defs = {}
        self.captures = {}
        self.preflight_snapshots = set()
        self.original_updates = []
        self.parents = {child: node for node in ast.walk(tree) for child in ast.iter_child_nodes(node)}
        bindings = self.check_snapshot_bindings()
        self.check_snapshot_uses(bindings)
        for node in ast.walk(tree):
            if isinstance(node, ast.Assign):
                if node in self.preflight_snapshots:
                    continue  # This exact branch exits before the capture path.
                for target in node.targets:
                    if isinstance(target, ast.Name):
                        if target.id in ('originals', 'snapshots') and not isinstance(node.value, (ast.Dict, ast.DictComp)):
                            raise Unsupported('unsupported snapshot mapping initializer')
                        self.defs.setdefault(target.id, []).append((node.lineno, node.value))
                    elif isinstance(target, ast.Tuple):
                        for index, item in enumerate(target.elts):
                            if isinstance(item, ast.Name):
                                part = ast.Subscript(value=node.value, slice=ast.Constant(index), ctx=ast.Load())
                                self.defs.setdefault(item.id, []).append((node.lineno, part))
                    elif (isinstance(target, ast.Subscript) and isinstance(target.value, ast.Name)
                          and target.value.id == 'originals'):
                        self.original_updates.append((node.lineno, target.slice, node.value))
        for defs in self.defs.values():
            defs.sort(key=lambda pair: pair[0])
        self.original_updates.sort(key=lambda update: update[0])
        # Resolution uses statement line numbers; refuse ambiguous same-line
        # deletion/update/read ordering instead of guessing column order.
        for line, _, _ in self.original_updates:
            origin = self.captures.get('originals')
            declared = origin.lineno if origin else next((n for n, _ in self.defs.get('originals', [])), 0)
            if self.function_at(declared) is not self.function_at(line):
                raise Unsupported('unsupported snapshot update across function scopes')
            if sum(isinstance(node, ast.stmt) and node.lineno == line for node in ast.walk(tree)) != 1:
                raise Unsupported('unsupported same-line snapshot update')

    @staticmethod
    def snapshot_name(node):
        name = node.id if isinstance(node, ast.Name) else node.attr if isinstance(node, ast.Attribute) else None
        return name if name in SNAPSHOT_NAMES else None

    def check_snapshot_read(self, access):
        """Follow every receiver, including calls followed by further accesses."""
        while True:
            owner = self.parents.get(access)
            if isinstance(owner, (ast.Subscript, ast.Attribute)) and owner.value is access:
                if not isinstance(owner.ctx, ast.Load):
                    raise Unsupported('unsupported snapshot nested mapping write')
            elif isinstance(owner, ast.Call) and owner.func is access:
                if not isinstance(access, ast.Attribute) or access.attr not in SNAPSHOT_READ_METHODS:
                    raise Unsupported('unsupported snapshot read method')
            else:
                if isinstance(access, ast.Attribute) and access.attr not in SNAPSHOT_READ_METHODS:
                    raise Unsupported('unsupported snapshot read method')
                return
            access = owner

    def check_snapshot_uses(self, bindings):
        """Every occurrence needs a known meaning, regardless of its namespace."""
        for node in ast.walk(self.tree):
            if isinstance(node, ast.Attribute) and node.attr in SNAPSHOT_NAMES:
                raise Unsupported('unsupported snapshot attribute binding or access')
            # A literal can name a namespace entry even through an unrecognized
            # accessor (or one imported under an alias). Do not enumerate APIs.
            if isinstance(node, ast.Constant) and isinstance(node.value, str) and node.value in SNAPSHOT_NAMES:
                raise Unsupported('unsupported snapshot dynamic write: literal namespace key')
            # Some identifiers are AST string fields, not Name nodes (keyword
            # arguments, import aliases and class-pattern attributes included).
            # Unknown syntax must not silently acquire another spelling of the
            # snapshot namespace. Only Name.id proceeds to the use allowlist.
            for field, value in ast.iter_fields(node):
                if isinstance(node, ast.Name) and field == 'id':
                    continue
                if isinstance(node, (ast.alias, ast.ImportFrom)) and field in ('name', 'module') and isinstance(value, str):
                    values = value.split('.')
                else:
                    values = value if isinstance(value, list) else [value]
                if any(isinstance(item, str) and item in SNAPSHOT_NAMES for item in values):
                    raise Unsupported('unsupported snapshot named occurrence')
            if not isinstance(node, ast.Name) or node.id not in SNAPSHOT_NAMES:
                continue
            if node in bindings:
                continue
            parent = self.parents.get(node)
            if isinstance(node.ctx, ast.Load):
                if self.readonly_anchor_view(node, parent):
                    continue
                if isinstance(parent, ast.Subscript) and parent.value is node:
                    if isinstance(parent.ctx, ast.Load):
                        # Reads may be transformed but must not become nested
                        # assignment/deletion targets such as originals[k][i].
                        self.check_snapshot_read(parent)
                        continue
                    if (node.id == 'originals' and isinstance(parent.ctx, ast.Store)
                            and isinstance(self.parents.get(parent), ast.Assign)
                            and self.parents[parent].targets == [parent]):
                        continue  # Existing modeled originals[key] = expression.
                if (isinstance(parent, ast.Call) and isinstance(parent.func, ast.Name)
                        and parent.func.id == 'dict' and parent.args == [node] and not parent.keywords):
                    self.check_snapshot_read(parent)
                    continue
                if isinstance(parent, ast.Attribute):
                    call = self.parents.get(parent)
                    if isinstance(call, ast.Call) and call.func is parent:
                        if parent.attr in ('items', 'keys', 'values') and not call.args and not call.keywords:
                            self.check_snapshot_read(call)
                            continue
                        if (parent.attr == 'get' and 1 <= len(call.args) <= 2 and not call.keywords
                                and not any(isinstance(arg, ast.Starred) for arg in call.args)):
                            self.check_snapshot_read(call)
                            continue
                        if (node.id == 'originals' and parent.attr == 'pop' and len(call.args) == 2
                                and isinstance(call.args[0], ast.Constant) and isinstance(call.args[0].value, str)
                                and isinstance(call.args[1], ast.Constant) and call.args[1].value is None
                                and not call.keywords and isinstance(self.parents.get(call), ast.Expr)):
                            # Preserve the existing removal model: a later read
                            # or filename target must not resurrect this key.
                            self.original_updates.append((call.lineno, call.args[0], None))
                            continue
                    raise Unsupported('unsupported snapshot mapping method')
            raise Unsupported('unsupported snapshot mapping escape or write')

    def readonly_anchor_view(self, node, call):
        """Allow only the repository's exact decoded view and read-only helper."""
        if not (node.id == 'originals' and isinstance(call, ast.Call)
                and isinstance(call.func, ast.Name) and call.func.id == 'anchors'
                and call.args == [node] and not call.keywords
                and isinstance(self.parents.get(call), ast.Expr)):
            return False
        helpers = [n for n in ast.walk(self.tree) if 'anchors' in bound_names(n)]
        views = [n for n in ast.walk(self.tree) if isinstance(n, ast.Assign)
                 and ast.dump(n) == ast.dump(SNAPSHOT_DECODED_VIEW)]
        capture = self.captures.get('snapshots')
        if len(helpers) != 1 or len(views) != 1 or capture is None:
            return False
        helper, view = helpers[0], views[0]
        owner = self.parents.get(self.parents.get(capture))
        # Proving the body is read-only is insufficient if the function can
        # escape to an alias or have __code__/defaults rewritten before use.
        for use in ast.walk(self.tree):
            for field, value in ast.iter_fields(use):
                if (use is helper and field == 'name'
                        or isinstance(use, ast.Name) and field == 'id'):
                    continue
                values = value if isinstance(value, list) else [value]
                if any(isinstance(item, str) and item == 'anchors' for item in values):
                    return False
            if isinstance(use, ast.Name) and use.id == 'anchors':
                parent = self.parents.get(use)
                if not (isinstance(use.ctx, ast.Load) and isinstance(parent, ast.Call)
                        and parent.func is use):
                    return False
        return (ast.dump(helper) == ast.dump(SNAPSHOT_ANCHOR_HELPER)
                and self.parents.get(helper) is self.tree
                and helper.lineno < capture.lineno < view.lineno < call.lineno
                and isinstance(owner, ast.With)
                and self.parents.get(owner) is self.tree
                and self.parents.get(view) is owner
                and self.parents.get(self.parents[call]) is owner)

    def check_snapshot_bindings(self):
        """Keep the original declaration; never resolve a read through a rebind."""
        bindings = {name: [] for name in ('originals', 'snapshots')}
        allowed = set()
        present = False
        for node in ast.walk(self.tree):
            present |= bool(self.snapshot_name(node))
            for name in bound_names(node).intersection(bindings):
                bindings[name].append(node)
                present = True
        for name, sites in bindings.items():
            if not sites:
                continue
            if self.empty_snapshot_placeholder(name, sites):
                placeholder, capture = sorted(sites, key=lambda node: node.lineno)
                allowed.add(placeholder)
                sites = [capture]
            elif self.exiting_snapshot_preflight(name, sites):
                preflight, capture = sorted(sites, key=lambda node: node.lineno)
                self.preflight_snapshots.add(self.parents[preflight])
                allowed.add(preflight)
                sites = [capture]
            if len(sites) != 1:
                raise Unsupported(f'unsupported snapshot binding: {name} must have exactly one declaration')
            site = sites[0]
            declaration = self.parents.get(site)
            if not isinstance(site, ast.Name) or not isinstance(site.ctx, ast.Store):
                raise Unsupported(f'unsupported snapshot binding: {name}')
            if (isinstance(declaration, ast.Assign) and len(declaration.targets) == 1
                    and declaration.targets[0] is site):
                self.check_snapshot_declaration(declaration, name)
                allowed.add(site)
                continue
            if (isinstance(declaration, ast.withitem) and declaration.optional_vars is site
                    and isinstance(self.parents.get(declaration), ast.With)
                    and isinstance(declaration.context_expr, ast.Call)
                    and ast.unparse(declaration.context_expr.func) in
                    ('mutating_sources', 'negative_process.mutating_sources')):
                self.check_snapshot_constructor(declaration.context_expr)
                self.captures[name] = declaration.context_expr
                allowed.add(site)
                continue
            raise Unsupported(f'unsupported snapshot binding: {name} requires Assign or mutating_sources with-as')
        if present:
            for node in ast.walk(self.tree):
                # exact dict(name) is trusted only without explicit shadowing,
                # including bindings stored in AST string fields and deletions.
                if 'dict' in bound_names(node):
                    raise Unsupported('unsupported snapshot dict binding')
                # Reject aliases of the capability as well as direct calls.
                symbol = (node.id if isinstance(node, ast.Name) else node.attr if isinstance(node, ast.Attribute)
                          else node.name if isinstance(node, ast.alias) else None)
                parent = self.parents.get(node)
                # Existing runner diagnostics inspect a callable's display name.
                # No callable/namespace capability is obtained by this shape.
                if (symbol == 'getattr' and isinstance(parent, ast.Call) and parent.func is node
                        and len(parent.args) in (2, 3) and not parent.keywords
                        and isinstance(parent.args[1], ast.Constant) and parent.args[1].value == '__name__'):
                    continue
                if symbol in {'globals', 'locals', 'vars', 'setattr', 'delattr', 'exec', 'eval', '__dict__',
                              'getattr', '__getattribute__', '__import__'}:
                    raise Unsupported(f'unsupported snapshot dynamic write: {symbol}')
        return allowed

    def exiting_snapshot_preflight(self, name, sites):
        """Recognize the complete, module-level check-only branch, never a rebind."""
        if name != 'originals' or len(sites) != 2:
            return False
        first, second = sorted(sites, key=lambda node: node.lineno)
        initial, capture = self.parents.get(first), self.parents.get(second)
        branch, owner = self.parents.get(initial), self.parents.get(capture)
        if not (isinstance(initial, ast.Assign) and initial.targets == [first]
                and isinstance(branch, ast.If) and self.parents.get(branch) is self.tree
                and ast.dump(branch) == ast.dump(SNAPSHOT_PREFLIGHT)
                and isinstance(capture, ast.withitem) and capture.optional_vars is second
                and isinstance(owner, ast.With) and self.parents.get(owner) is self.tree
                and branch.end_lineno < owner.lineno):
            return False
        return not any(isinstance(node, ast.Name) and node.id == name
                       and isinstance(node.ctx, ast.Load)
                       and not branch.lineno <= node.lineno <= branch.end_lineno
                       and node.lineno <= capture.context_expr.end_lineno
                       for node in ast.walk(self.tree))

    def empty_snapshot_placeholder(self, name, sites):
        """A never-observed module placeholder is part of its sole capture."""
        if len(sites) != 2:
            return False
        first, second = sorted(sites, key=lambda node: node.lineno)
        initial, capture = self.parents.get(first), self.parents.get(second)
        owner = self.parents.get(capture)
        if not (isinstance(initial, ast.Assign) and initial.targets == [first]
                and self.parents.get(initial) is self.tree
                and isinstance(initial.value, ast.Dict) and not initial.value.keys
                and isinstance(capture, ast.withitem) and capture.optional_vars is second
                and isinstance(owner, ast.With) and self.parents.get(owner) is self.tree
                and isinstance(capture.context_expr, ast.Call)
                and initial.lineno < owner.lineno):
            return False
        # Include multiline with arguments and earlier context-manager items;
        # they execute before the as-target receives the snapshot.
        return not any(isinstance(node, ast.Name) and node.id == name
                       and isinstance(node.ctx, ast.Load)
                       and node.lineno <= capture.context_expr.end_lineno
                       for node in ast.walk(self.tree))

    def check_snapshot_constructor(self, call):
        symbol = 'mutating_sources' if isinstance(call.func, ast.Name) else 'negative_process'
        bindings = [node for node in ast.walk(self.tree) if symbol in bound_names(node)]
        if len(bindings) == 1 and isinstance(bindings[0], ast.alias):
            imported = bindings[0]
            statement = self.parents[imported]
            official = (isinstance(statement, ast.ImportFrom) and statement.module == 'negative_process'
                        and statement.level == 0 and imported.name == 'mutating_sources' if symbol == 'mutating_sources'
                        else isinstance(statement, ast.Import) and imported.name == 'negative_process')
            rewritten = any(isinstance(node, ast.Attribute) and node.attr == 'mutating_sources'
                            and isinstance(node.ctx, (ast.Store, ast.Del)) for node in ast.walk(self.tree))
            if (official and self.parents.get(statement) is self.tree and statement.lineno < call.lineno
                    and not rewritten and len(call.args) == 2
                    and all(k.arg in ('baseline', 'lock_timeout') for k in call.keywords)):
                return
        raise Unsupported('unsupported snapshot constructor: require unique official import and two arguments')

    def check_snapshot_declaration(self, declaration, name):
        parent = self.parents.get(declaration)
        while parent is not self.tree:
            if isinstance(parent, ast.With) and all(
                    isinstance(item.context_expr, ast.Call) and ast.unparse(item.context_expr.func) in
                    ('mutating_sources', 'negative_process.mutating_sources') for item in parent.items):
                parent = self.parents[parent]
                continue
            if (isinstance(parent, (ast.FunctionDef, ast.AsyncFunctionDef))
                    and self.parents.get(parent) is self.tree):
                return
            raise Unsupported(f'unsupported snapshot binding: {name} requires an unconditional module/function Assign')

    def snapshot_definitions(self, name, line):
        definitions = self.defs.get(name, [])
        if definitions:
            declared = definitions[-1][0]
            scope = self.function_at(declared)
            if scope is not None and scope is not self.function_at(line):
                raise Unsupported('unsupported snapshot read across function scopes')
        return [(n, value) for n, value in definitions if n < line]

    def named(self, name, line, local):
        if name in local:
            return local[name]
        if name == '__file__':
            return str(self.script)
        if name in self.captures:
            call = self.captures[name]
            if call.lineno >= line or self.function_at(call.lineno) is not self.function_at(line):
                raise Unsupported('unsupported snapshot read before capture or across function scopes')
            base = self.value(call.args[0], call.lineno, local)
            paths = self.value(call.args[1], call.lineno, local)
            return {path: self.confined(Path(base) / path).read_bytes() for path in paths}
        choices = (self.snapshot_definitions(name, line) if name in ('originals', 'snapshots')
                   else [(n, value) for n, value in self.defs.get(name, []) if n < line])
        if not choices:
            raise Unsupported(f'unresolved name {name}')
        n, value = choices[-1]
        if name in SNAPSHOT_NAMES and isinstance(value, (ast.Dict, ast.DictComp)):
            return {key: self.value(expression, n, bound)
                    for key, expression, bound in self.snapshot_items(value, n, local)}
        return self.value(value, n, local)

    def function_at(self, line):
        frames = [node for node in ast.walk(self.tree)
                  if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda))
                  and node.lineno <= line <= node.end_lineno]
        return max(frames, key=lambda node: node.lineno, default=None)

    def confined(self, path):
        path = (self.root / path).resolve()
        if not path.is_relative_to(self.root):
            raise Unsupported(f'path outside repository: {path}')
        return path

    def value(self, node, line, local):
        if isinstance(node, ast.Constant):
            return node.value
        if isinstance(node, ast.Name):
            return self.named(node.id, line, local)
        if isinstance(node, ast.UnaryOp) and isinstance(node.op, ast.USub):
            value = self.value(node.operand, line, local)
            if type(value) is int:
                return -value
        if isinstance(node, ast.Compare) and len(node.ops) == 1 and isinstance(node.ops[0], (ast.Eq, ast.NotEq)):
            equal = self.value(node.left, line, local) == self.value(node.comparators[0], line, local)
            return equal if isinstance(node.ops[0], ast.Eq) else not equal
        if isinstance(node, ast.IfExp):
            condition = self.value(node.test, line, local)
            if type(condition) is bool:
                return self.value(node.body if condition else node.orelse, line, local)
        if isinstance(node, (ast.List, ast.Tuple, ast.Set)):
            values = [self.value(n, line, local) for n in node.elts]
            return set(values) if isinstance(node, ast.Set) else values
        if isinstance(node, ast.Dict):
            return {self.value(k, line, local): self.value(v, line, local)
                    for k, v in zip(node.keys, node.values)}
        if isinstance(node, (ast.ListComp, ast.DictComp, ast.SetComp)):
            if len(node.generators) != 1 or node.generators[0].ifs:
                raise Unsupported('only unconditional single-loop comprehensions are supported')
            gen = node.generators[0]
            result = []
            for item in self.value(gen.iter, line, local):
                bound = self.bind(gen.target, item, local)
                result.append((self.value(node.key, line, bound), self.value(node.value, line, bound))
                              if isinstance(node, ast.DictComp) else self.value(node.elt, line, bound))
            if isinstance(node, ast.DictComp):
                return dict(result)
            return set(result) if isinstance(node, ast.SetComp) else result
        if isinstance(node, ast.BinOp):
            left, right = self.value(node.left, line, local), self.value(node.right, line, local)
            if isinstance(node.op, ast.Div) and isinstance(left, Path) and isinstance(right, str):
                return left / right
            if isinstance(node.op, ast.Add) and type(left) is type(right) and isinstance(left, (str, list)):
                return left + right
            if isinstance(node.op, ast.Mult) and isinstance(left, str) and type(right) is int and 0 <= right <= 10000:
                return left * right
            if isinstance(node.op, ast.BitOr) and isinstance(left, set) and isinstance(right, set):
                return left | right
        if isinstance(node, ast.Attribute):
            obj = self.value(node.value, line, local)
            if isinstance(obj, Path) and node.attr in ('parent', 'parents', 'name', 'stem'):
                return getattr(obj, node.attr)
        if isinstance(node, ast.Subscript):
            if isinstance(node.value, ast.Name) and node.value.id == 'originals':
                key = self.value(node.slice, line, local)
                definition = max((n for n, _ in self.snapshot_definitions('originals', line)), default=0)
                for n, subscript, expression in reversed(self.original_updates):
                    if definition < n < line and self.value(subscript, n, local) == key:
                        if expression is None:
                            raise Unsupported(f'removed snapshot key: {key!r}')
                        return self.value(expression, n, local)
            obj = self.value(node.value, line, local)
            if isinstance(node.slice, ast.Slice):
                parts = [None if n is None else self.value(n, line, local)
                         for n in (node.slice.lower, node.slice.upper, node.slice.step)]
                return obj[slice(*parts)]
            return obj[self.value(node.slice, line, local)]
        if isinstance(node, ast.JoinedStr):
            return ''.join(str(self.value(n.value, line, local)) if isinstance(n, ast.FormattedValue)
                           else n.value for n in node.values)
        if isinstance(node, ast.Call) and not node.keywords:
            args = [self.value(n, line, local) for n in node.args]
            function = ast.unparse(node.func)
            if function in ('Path', 'pathlib.Path', 'str') and len(args) == 1:
                return Path(args[0]) if function != 'str' else str(args[0])
            if function == 'json.loads':
                return json.loads(*args)
            if function == 'replace_once' and len(args) == 3 and all(isinstance(a, str) for a in args):
                text, before, after = args
                if not before or text.count(before) != 1:
                    raise Unsupported(f'replace_once dependency: expected=1 actual={text.count(before)} anchor={before!r}')
                return text.replace(before, after)
            if isinstance(node.func, ast.Attribute):
                if isinstance(node.func.value, ast.Name) and node.func.value.id == 're' and node.func.attr == 'escape':
                    return re.escape(*args)
                obj = self.value(node.func.value, line, local)
                method = node.func.attr
                if isinstance(obj, Path):
                    if method == 'resolve' and not args:
                        return obj.resolve()
                    if method == 'with_name' and len(args) == 1:
                        return obj.with_name(args[0])
                    if method == 'read_text' and not args:
                        return self.confined(obj).read_text(encoding='utf-8')
                    if method == 'read_bytes' and not args:
                        return self.confined(obj).read_bytes()
                    if method == 'glob' and len(args) == 1:
                        return sorted(self.confined(obj).glob(args[0]))
                if isinstance(obj, bytes) and method == 'decode' and not args:
                    return obj.decode('utf-8')
                if isinstance(obj, str) and method in ('replace', 'strip', 'index', 'split', 'join'):
                    return getattr(obj, method)(*args)
        raise Unsupported(f'unsupported expression: {ast.unparse(node)[:100]}')

    @staticmethod
    def bind(target, value, local):
        names = target.elts if isinstance(target, ast.Tuple) else [target]
        values = value if isinstance(target, ast.Tuple) else [value]
        if len(names) != len(values) or not all(isinstance(n, ast.Name) for n in names):
            raise Unsupported('unsupported loop binding')
        return dict(local, **{n.id: v for n, v in zip(names, values)})

    def contexts(self, node):
        """Expand only literal for-loops enclosing a table append."""
        loops = []
        parent = self.parents.get(node)
        while parent is not None:
            if isinstance(parent, ast.For):
                loops.append(parent)
            elif isinstance(parent, ast.ListComp):
                loops.extend(reversed(parent.generators))
            parent = self.parents.get(parent)
        # run_suite(copy) checks a disposable copy of this exact source tree.
        aliases = {'copy': self.root} if self.script.parent.name == 'background-output' else {}
        contexts = [aliases]
        for loop in reversed(loops):
            next_contexts = []
            for local in contexts:
                if isinstance(loop, ast.comprehension) and loop.ifs:
                    raise Unsupported('conditional mutation comprehension')
                for value in self.value(loop.iter, getattr(loop, 'lineno', node.lineno), local):
                    next_contexts.append(self.bind(loop.target, value, local))
            contexts = next_contexts
        return contexts

    def target(self, node, line, local):
        value = self.value(node, line, local)
        if isinstance(value, Path):
            return self.confined(value)
        if isinstance(value, str):
            # A filename key must map to its actual read path, never a basename search.
            definitions = self.snapshot_definitions('originals', line)
            byte_definitions = self.snapshot_definitions('snapshots', line)
            if definitions and byte_definitions:
                raise Unsupported('ambiguous snapshot mappings for filename key')
            definitions = definitions or byte_definitions
            definition = definitions[-1][0] if definitions else 0
            for n, subscript, expression in reversed(self.original_updates):
                if definition < n < line and self.value(subscript, n, local) == value:
                    if expression is None:
                        raise Unsupported(f'removed snapshot key: {value!r}')
                    return self.snapshot_target(expression, n, local)
            for n, mapping in definitions[-1:]:
                matches = [(expression, bound) for key, expression, bound in self.snapshot_items(mapping, n, local)
                           if key == value]
                if matches:
                    # Literal dictionaries have deterministic last-key-wins order.
                    expression, bound = matches[-1]
                    return self.snapshot_target(expression, n, bound)
                if not value.startswith(('src/', 'pilotdeck-gateway/')):
                    raise Unsupported(f'unresolved source path: missing snapshot key {value!r}')
            if value.startswith('src/'):
                return self.confined(self.root / value)
            if value.startswith('pilotdeck-gateway/'):
                return self.confined(self.root / 'src' / value)
            for base in ('source', 'SRC', 'module'):
                try:
                    folder = self.named(base, line, local)
                except Unsupported:
                    continue
                if isinstance(folder, Path) and folder.is_dir():
                    return self.confined(folder / value)
        raise Unsupported(f'unresolved source path: {value!r}')

    def snapshot_items(self, mapping, line, local):
        """Keep the value AST and loop locals so keys retain their actual origin."""
        if isinstance(mapping, ast.Dict):
            for key, expression in zip(mapping.keys, mapping.values):
                if key is None:
                    raise Unsupported('unsupported snapshot dictionary unpacking')
                yield self.value(key, line, local), expression, local
        elif isinstance(mapping, ast.DictComp):
            if (len(mapping.generators) != 1 or mapping.generators[0].ifs
                    or mapping.generators[0].is_async):
                raise Unsupported('only unconditional single-loop snapshot comprehensions are supported')
            generator = mapping.generators[0]
            keys = set()
            for item in self.value(generator.iter, line, local):
                bound = self.bind(generator.target, item, local)
                key = self.value(mapping.key, line, bound)
                # Glob/set iteration order is not a stable last-key-wins oracle.
                if key in keys:
                    raise Unsupported('ambiguous snapshot origin from duplicate comprehension keys')
                keys.add(key)
                yield key, mapping.value, bound
        else:
            raise Unsupported('unsupported snapshot mapping initializer')

    def snapshot_target(self, expression, line, local):
        """Follow the receiver through transforms, preserving the latest read."""
        if isinstance(expression, ast.Call) and isinstance(expression.func, ast.Attribute):
            if expression.func.attr in ('read_text', 'read_bytes'):
                return self.target(expression.func.value, line, local)
            if expression.func.attr in ('replace', 'strip', 'decode'):
                return self.snapshot_target(expression.func.value, line, local)
        if (isinstance(expression, ast.Subscript) and isinstance(expression.value, ast.Name)
                and expression.value.id in ('originals', 'snapshots')):
            return self.target(expression.slice, line, local)
        raise Unsupported('unresolved source path: unsupported snapshot origin')


def table_rows(tree, static):
    """Only mutation table definitions/extensions, not incidental tuples."""
    dictionary_keys = {}

    def dictionary_context(node, *, declaration=False):
        conditional = (ast.If, ast.While, ast.Try, ast.With, ast.Match,
                       ast.AsyncFor, ast.AsyncWith, ast.Break, ast.Continue)
        parent = static.parents.get(node)
        while parent is not None:
            if (isinstance(parent, conditional)
                    or isinstance(parent, ast.For) and (declaration or any(
                        isinstance(child, conditional) for child in ast.walk(parent)))):
                raise Unsupported('conditional dictionary mutation declaration is not supported')
            parent = static.parents.get(parent)

    def dictionary_row(table, key, value):
        if key is None or not isinstance(value, ast.Tuple):
            raise Unsupported('unsupported dictionary mutation row')
        dictionary_context(value)
        row = ast.copy_location(ast.Tuple(elts=[key, *value.elts], ctx=ast.Load()), value)
        # Retain the original ancestry for literal-loop expansion of new keys.
        static.parents[row] = value
        for local in static.contexts(row):
            name = static.value(key, row.lineno, local)
            if not isinstance(name, str) or not name:
                raise Unsupported('invalid dictionary mutation key')
            if name in dictionary_keys[table]:
                raise Unsupported(f'duplicate mutation key: {name}')
            dictionary_keys[table].add(name)
        return row

    def lists(node):
        if isinstance(node, ast.List):
            for row in node.elts:
                if isinstance(row, ast.Starred) and isinstance(row.value, ast.ListComp):
                    yield row.value.elt
                else:
                    yield row
        elif isinstance(node, ast.BinOp) and isinstance(node.op, ast.Add):
            yield from lists(node.left)
            yield from lists(node.right)
        elif isinstance(node, ast.Name) and node.id.lower() in TABLES:
            return  # its own definition is scanned separately
        elif isinstance(node, ast.ListComp):
            # Selection of already declared rows is not another declaration.
            if (len(node.generators) != 1 or not isinstance(node.generators[0].iter, ast.Name)
                    or node.generators[0].iter.id.lower() not in TABLES
                    or not isinstance(node.generators[0].target, ast.Name)
                    or not isinstance(node.elt, ast.Name)
                    or node.elt.id != node.generators[0].target.id):
                raise Unsupported('unsupported mutation table comprehension')
        else:
            raise Unsupported(f'unsupported mutation table: {ast.unparse(node)[:100]}')
    for node in sorted(ast.walk(tree), key=lambda n: getattr(n, 'lineno', 0)):
        if isinstance(node, (ast.Delete, ast.AnnAssign)):
            targets = node.targets if isinstance(node, ast.Delete) else [node.target]
            for target in targets:
                name = target.value if isinstance(target, ast.Subscript) else target
                if isinstance(name, ast.Name) and name.id.lower() in TABLES:
                    raise Unsupported('deleting/annotating mutation tables is not supported')
        if isinstance(node, (ast.Assign, ast.AugAssign)):
            targets = node.targets if isinstance(node, ast.Assign) else [node.target]
            for target in targets:
                if (isinstance(target, ast.Subscript) and isinstance(target.value, ast.Name)
                        and target.value.id.lower() in TABLES):
                    table = target.value.id
                    if table not in dictionary_keys or not isinstance(node, ast.Assign):
                        raise Unsupported('in-place mutation table assignment is not supported')
                    yield dictionary_row(table, target.slice, node.value)
                elif isinstance(target, ast.Name) and target.id.lower() in TABLES:
                    table = target.id
                    if table in dictionary_keys:
                        raise Unsupported('dictionary mutation table reassignment is not supported')
                    if isinstance(node.value, ast.Dict) and isinstance(node, ast.Assign):
                        dictionary_context(node.value, declaration=True)
                        dictionary_keys[table] = set()
                        for key, value in zip(node.value.keys, node.value.values):
                            yield dictionary_row(table, key, value)
                    else:
                        yield from lists(node.value)
        if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute) and isinstance(node.func.value, ast.Name) and node.func.value.id.lower() in TABLES:
            if node.func.value.id in dictionary_keys:
                raise Unsupported('unsupported dictionary mutation table update')
            if node.func.attr == 'append' and len(node.args) == 1:
                yield node.args[0]
            elif node.func.attr == 'extend' and len(node.args) == 1:
                yield from lists(node.args[0])
            else:
                raise Unsupported('unsupported mutation table update')


def reject_dynamic_module_access(tree):
    """Reject setattr/globals()/vars()/sys.modules[...] in every checked script.

    #996 fix-r2: the switches whitelist below only sees names that occur
    statically, so `setattr(module, 'swi' + 'tches', ...)` and
    `globals()['swi' + 'tches'] = ...` rebind the executed table without one
    whitelisted occurrence. Per-shape chasing cannot enumerate these spellings;
    schemata scripts need none of the forms, and no other checked script uses
    them, so they fail closed outright.
    """
    for node in ast.walk(tree):
        if isinstance(node, ast.Call):
            symbol = (node.func.id if isinstance(node.func, ast.Name)
                      else node.func.attr if isinstance(node.func, ast.Attribute) else None)
            if symbol in SWITCH_DYNAMIC_SYMBOLS:
                raise Unsupported(f'unsupported dynamic module access: {symbol}')
        if (isinstance(node, ast.Subscript) and isinstance(node.value, ast.Attribute)
                and node.value.attr == 'modules'
                and isinstance(node.value.value, ast.Name) and node.value.value.id == 'sys'):
            raise Unsupported('unsupported dynamic module access: sys.modules[...]')


def switch_table_rows(tree, static):
    """Yield rows of the one literal `switches` table; every other name fails.

    #996 fix-r2: the table name is a whitelist, not a blacklist of update
    shapes (r1's per-form chase could not enumerate `s = switches; s.append`,
    `getattr(switches, 'pop')`, `list.clear(switches)` or import aliases).
    The name may occur only as (1) the unique module-level literal
    `switches = [...]` assignment, (2) the iterable of a `for ... in
    switches` loop or comprehension, (3) the single argument of
    `len(switches)`, or (4) a positional argument of a `mutant_switch.*`
    executor entry call. Imports, keyword names, attribute names, string
    literals and every other read or write are rejected: the audited rows
    must never drift from the executed table.
    """
    parents = static.parents
    declaration = None
    seen = False
    for node in sorted(ast.walk(tree), key=lambda n: getattr(n, 'lineno', 0)):
        # Any spelling that is not a Name occurrence: import aliases, def and
        # lambda parameters, keyword names, attribute names, string literals.
        # Only Name.id proceeds to the use allowlist.
        for field, value in ast.iter_fields(node):
            if isinstance(node, ast.Name) and field == 'id':
                continue
            if isinstance(node, (ast.alias, ast.ImportFrom)) and field in ('name', 'module') and isinstance(value, str):
                values = value.split('.')
            else:
                values = value if isinstance(value, list) else [value]
            if any(isinstance(item, str) and item.lower() in SWITCH_TABLE_NAMES for item in values):
                raise Unsupported('unsupported switches named occurrence')
        if not isinstance(node, ast.Name) or node.id.lower() not in SWITCH_TABLE_NAMES:
            continue
        seen = True
        parent = parents.get(node)
        if isinstance(node.ctx, (ast.Store, ast.Del)):
            if (isinstance(node.ctx, ast.Del) or declaration is not None
                    or not isinstance(parent, ast.Assign)
                    or len(parent.targets) != 1 or parent.targets[0] is not node
                    or parents.get(parent) is not tree):
                raise Unsupported('the switches table must be the single module-level assignment')
            if not isinstance(parent.value, ast.List):
                raise Unsupported('switches table must be a literal list of rows')
            declaration = parent
            continue
        if isinstance(parent, (ast.For, ast.AsyncFor, ast.comprehension)) and parent.iter is node:
            continue  # for ... in switches iterates the audited rows.
        if (isinstance(parent, ast.Call) and isinstance(parent.func, ast.Name)
                and parent.func.id == 'len' and parent.args == [node] and not parent.keywords):
            continue  # A size check never changes the audited rows.
        if (isinstance(parent, ast.Call) and isinstance(parent.func, ast.Attribute)
                and isinstance(parent.func.value, ast.Name)
                and parent.func.value.id == 'mutant_switch' and node in parent.args):
            continue  # The guarded shared executor consumes the table read-only.
        raise Unsupported('unsupported switches read: only for-in, len(switches) '
                          'and mutant_switch entry arguments are allowed')
    if seen and declaration is None:
        raise Unsupported('switches is read without its literal table declaration')
    if declaration is not None:
        for row in declaration.value.elts:
            if not isinstance(row, ast.Tuple) or len(row.elts) != 4:
                raise Unsupported('switches rows must be (name, path, test, assertion)')
            yield row


def switch_bindings(tree, static):
    """Evaluate the audited (name, path, test, assertion, line) switch rows."""
    bindings = []
    seen = set()
    for row in switch_table_rows(tree, static):
        for local in static.contexts(row):
            line = row.lineno
            name = static.value(row.elts[0], line, local)
            path = static.target(row.elts[1], line, local)
            test = static.value(row.elts[2], line, local)
            assertion = static.value(row.elts[3], line, local)
            if not isinstance(name, str) or not name:
                raise Unsupported(f'line {line}: invalid switch name')
            if name in seen:
                raise Unsupported(f'line {line}: duplicate switch name: {name}')
            seen.add(name)
            if not ((isinstance(test, str) and test)
                    or (isinstance(test, list) and test
                        and all(isinstance(item, str) and item for item in test))):
                raise Unsupported(f'{name}: invalid switch test binding')
            if not ((isinstance(assertion, str) and assertion)
                    or (isinstance(assertion, list) and assertion
                        and all(isinstance(item, str) and item for item in assertion))):
                raise Unsupported(f'{name}: invalid switch assertion binding')
            bindings.append((name, static.confined(path), test, assertion, line))
    return bindings


def static_switch_names(script, root):
    """The statically declared switch identities of a switches-table script.

    #996 fix-r4: the executor-side denominator check calls this same
    switch_bindings extraction the checker uses, so a table shrunk or grown
    between the static audit and the actual execution (for example inside a
    ``main()`` the import-time audit never runs) is reported as drift instead
    of silently executing a different suite than the declared one.
    """
    tree = ast.parse(script.read_text(encoding='utf-8'), filename=str(script))
    static = Static(script, root, tree)
    return {name for name, _path, _test, _assertion, _line in switch_bindings(tree, static)}


def static_mutant_names(script, root):
    """The statically declared mutation identities of a mutation-table script.

    #996 fix-r5: the edgeclaw-service-facade entries consume legacy mutation
    tables (negative.py's MUTANTS rows, switch_negative.py's MUTATIONS rows)
    instead of a switches table, so their executor-side denominator check
    calls this same table_rows extraction the checker uses instead of
    static_switch_names: a table shrunk or grown between the static audit and
    the actual execution is still reported as drift, never published as a
    shorter success. The runtime table itself is never a denominator.
    """
    tree = ast.parse(script.read_text(encoding='utf-8'), filename=str(script))
    static = Static(script, root, tree)
    names = set()
    for row in table_rows(tree, static):
        for local in static.contexts(row):
            if isinstance(row, ast.Call) and isinstance(row.func, ast.Name) \
                    and row.func.id in ('mutation', 'mutate'):
                fields = row.args
            elif isinstance(row, ast.Tuple):
                fields = row.elts
            else:
                raise Unsupported(f'line {row.lineno}: unsupported mutation row')
            if not fields:
                raise Unsupported(f'line {row.lineno}: mutation row without a name')
            name = static.value(fields[0], row.lineno, local)
            if not isinstance(name, str) or not name:
                raise Unsupported(f'line {row.lineno}: invalid mutation name')
            if name in names:
                raise Unsupported(f'line {row.lineno}: duplicate mutation name: {name}')
            names.add(name)
    if not names:
        raise Unsupported('no statically declared mutation table found')
    return names


def runtime_switch_table(script):
    """Import the switches-table script isolated; return its executed rows.

    The child never runs as ``__main__`` and answers with exactly one prefixed
    JSON audit record; anything else (import failure, timeout, a forged or
    duplicated record, a non-JSON record, a nonzero exit, a record without
    this run's nonce) fails closed. Output goes to a real temporary file, not
    a pipe, so a descendant that survives the timeout kill cannot hold
    collection open (#528 shape).
    """
    nonce = secrets.token_hex(16)
    command = [sys.executable or 'python3', '-I', '-B', '-c', RUNTIME_SWITCH_BOOTSTRAP,
               str(script), nonce]
    with tempfile.TemporaryFile() as report:
        try:
            process = subprocess.Popen(command, stdin=subprocess.DEVNULL,
                                       stdout=report, stderr=subprocess.STDOUT)
        except OSError as error:
            raise Unsupported(f'switches runtime audit could not start python: {error}') from error
        try:
            process.wait(timeout=RUNTIME_SWITCH_TIMEOUT)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
            raise Unsupported(f'switches runtime audit exceeded {RUNTIME_SWITCH_TIMEOUT}s') from None
        report.seek(0)
        finished = report.read().decode('utf-8', 'replace')
    if process.returncode != 0:
        # #996 fix-r4: a module that prints a plausible record and then exits
        # early (even with 0) must never be accepted on that record alone.
        tail = finished[-160:].replace('\n', ' ')
        raise Unsupported(f'switches runtime audit exited {process.returncode} '
                          f'(expected 0); output tail: {tail!r}')
    records = [line[len(RUNTIME_SWITCH_PREFIX):] for line in finished.splitlines()
               if line.startswith(RUNTIME_SWITCH_PREFIX)]
    if len(records) != 1:
        tail = finished[-160:].replace('\n', ' ')
        raise Unsupported(f'switches runtime audit produced {len(records)} audit records '
                          f'(exit={process.returncode}); output tail: {tail!r}')
    try:
        document = json.loads(records[0])
    except ValueError as error:
        raise Unsupported(f'switches runtime audit record is not JSON: {error}') from error
    if not isinstance(document, dict):
        raise Unsupported('switches runtime audit record is not an object')
    if document.get('nonce') != nonce:
        raise Unsupported('switches runtime audit record failed nonce verification: '
                          'only the completed bootstrap can write an acceptable record')
    if document.get('error') is not None:
        raise Unsupported(f'switches runtime audit failed: {document["error"]}')
    if not isinstance(document.get('rows'), list):
        raise Unsupported('switches runtime audit record carries no rows')
    return document['rows']


def audit_runtime_switches(script, rows):
    """The imported module is the truth; reconcile it with the static rows."""
    table = runtime_switch_table(script)
    if len(table) != len(rows):
        raise Unsupported(f'switches runtime drift: the static audit extracted {len(rows)} rows, '
                          f'the imported module has {len(table)}')
    for index, ((name, path, test, assertion, _line), row) in enumerate(zip(rows, table)):
        if row[0] != name:
            raise Unsupported(f'switches runtime row {index}: name {row[0]!r} differs from the audited {name!r}')
        if Path(row[1]).resolve() != path:
            raise Unsupported(f'switches runtime row {index} ({name}): target {row[1]} '
                              f'differs from the audited {path}')
        if row[2] != test:
            raise Unsupported(f'switches runtime row {index} ({name}): test {row[2]!r} '
                              f'differs from the audited {test!r}')
        if row[3] != assertion:
            raise Unsupported(f'switches runtime row {index} ({name}): assertion '
                              'differs from the audited binding')


def host_pump_count(tree, static, name, local):
    """Read this runner's exact occurrence guard, including its wire exception."""
    guards = [node for node in ast.walk(tree) if isinstance(node, ast.Assert)
              and isinstance(node.test, ast.Compare)
              and ast.unparse(node.test.left) == 'content.count(old)']
    if (len(guards) != 1 or len(guards[0].test.ops) != 1
            or not isinstance(guards[0].test.ops[0], ast.Eq)):
        raise Unsupported('expected one content.count(old) equality guard')
    guard = guards[0]
    return static.value(guard.test.comparators[0], guard.lineno, dict(local, name=name))


def extract(script, root):
    relative = script.relative_to(root / 'src/pilotdeck-gateway/tests/fixtures').as_posix()
    tree = ast.parse(script.read_text(encoding='utf-8'), filename=str(script))
    static = Static(script, root, tree)
    reject_dynamic_module_access(tree)
    submit_maps = ({key: submit_mapping(static, key) for key in ('mutant_source', 'mutant_anchor')}
                   if relative == 'turn-submit/negative.py' else {})
    flags = regex_flags(tree)
    anchors, notes = [], []
    declared = 'ANCHOR_COUNTS' in static.defs
    declarations = static.named('ANCHOR_COUNTS', 10**9, {}) if declared else {}
    if declared and declarations == {}:
        raise Unsupported('empty ANCHOR_COUNTS declaration; omit it to use default counts')
    if not isinstance(declarations, dict) or any(not isinstance(k, str) or not positive_count(v) for k, v in declarations.items()):
        raise Unsupported('ANCHOR_COUNTS must map names to positive integers')
    used = set()
    exempted = set()
    scoped = set()
    for row in table_rows(tree, static):
        for local in static.contexts(row):
            line = row.lineno
            if isinstance(row, ast.Call) and isinstance(row.func, ast.Name) and row.func.id == 'dict':
                fields_by_name = {k.arg: k.value for k in row.keywords}
                name = static.value(fields_by_name['name'], line, local)
                if 'changes' in fields_by_name:
                    for change in fields_by_name['changes'].elts:
                        path = static.target(change.elts[0], line, local)
                        before = static.value(change.elts[1], line, local)
                        static.value(change.elts[2], line, local)
                        if not isinstance(before, str) or not before:
                            raise Unsupported(f'{name}: empty/non-string anchor')
                        anchors.append(Anchor(name, path, before, line=line))
                elif 'replacements' in fields_by_name and relative == 'edgeclaw-dangling-links/negative.py':
                    path = static.confined(static.named('source', 10**9, local))
                    for change in fields_by_name['replacements'].elts:
                        before = static.value(change.elts[0], line, local)
                        static.value(change.elts[1], line, local)
                        if not isinstance(before, str) or not before:
                            raise Unsupported(f'{name}: empty/non-string anchor')
                        anchors.append(Anchor(name, path, before, line=line))
                else:
                    raise Unsupported(f'{name}: unsupported dict mutation')
                continue
            if isinstance(row, ast.Tuple):
                fields = row.elts
            elif isinstance(row, ast.Call) and isinstance(row.func, ast.Name) and row.func.id in ('mutation', 'mutate'):
                fields = row.args
            else:
                raise Unsupported(f'line {line}: unsupported mutation row')
            if len(fields) < 3:
                raise Unsupported(f'line {line}: incomplete mutation row')
            name = static.value(fields[0], line, local)
            if not isinstance(name, str) or not name:
                raise Unsupported(f'line {line}: invalid mutation name')
            if name == 'baseline' and all(isinstance(n, ast.Constant) and n.value is None for n in fields[1:4]):
                notes.append('baseline: positive control without a replacement')
                continue
            if relative == 'connection-context/negative.py' and name == 'ticket-scoped-in-flight':
                patches = static.named('ticket_patches', 10**9, local)
                for filename, replacements in patches.items():
                    path = static.target(ast.Constant(filename), line, local)
                    for before, _after in replacements:
                        anchors.append(Anchor(name, path, before, line=line))
                continue
            if (relative, name) in SEQUENTIAL_ROWS:
                reason = SEQUENTIAL_ROWS[relative, name]
                if not reason.strip():
                    raise Unsupported(f'{name}: empty sequential row reason')
                notes.append(f'{name}: {reason}')
                exempted.add(name)
                continue
            if relative in IMPLICIT:
                target = ast.Name(id=IMPLICIT[relative], ctx=ast.Load())
                before_node = fields[1]
                if relative == 'turn-submit/negative.py':
                    routing = submit_maps['mutant_source']
                    path = routing.get(name, static.value(target, line, local))
                else:
                    path = static.target(target, line, local)
                after_node = fields[2]
            else:
                path_index, before_index, after_index = FIELD_ORDER.get(relative, (1, 2, 3))
                if len(fields) <= max(path_index, before_index, after_index):
                    raise Unsupported(f'{name}: expected name/path/before/after')
                path = static.target(fields[path_index], line, local)
                before_node = fields[before_index]
                after_node = fields[after_index]
            before = static.value(before_node, line, local)
            if before is None and relative in REGEX_SCRIPTS and path.suffix == '.json':
                notes.append(f'{name}: JSON matrix/provenance mutation, no source anchor')
                continue
            # Generated after strings may depend on extra split/index/replace_once
            # anchors. Validate these dependencies without applying a mutation.
            after = static.value(after_node, line, local)
            if not isinstance(before, (str, bytes)) or not before:
                raise Unsupported(f'{name}: empty or non-string anchor')
            if (relative, name) in ROW_EXCEPTIONS:
                reason = ROW_EXCEPTIONS[relative, name]
                if not reason.strip():
                    raise Unsupported(f'{name}: empty row exception reason')
                if (path.suffix != '.json' or not isinstance(before, str) or not isinstance(after, str)
                        or not re.fullmatch(r'[0-9a-f]{64}', before)
                        or not re.fullmatch(r'[0-9a-f]{64}', after)):
                    raise Unsupported(f'{name}: row exception requires a JSON hash replacement')
                if before not in path.read_text(encoding='utf-8'):
                    raise Unsupported(f'{name}: exempted before is absent from JSON target')
                for candidate in sorted((root / 'src').rglob('*')):
                    if (candidate.suffix in SOURCE_SUFFIXES and candidate.is_file()
                            and before in static.confined(candidate).read_text(encoding='utf-8')):
                        raise Unsupported(f'{name}: exempted before occurs in source {candidate.relative_to(root)}')
                notes.append(f'{name}: {reason}')
                exempted.add(name)
                continue
            if type(after) is not type(before):
                raise Unsupported(f'{name}: before/after must have the same string/byte type')
            if isinstance(before, bytes) and flags is not None:
                raise Unsupported(f'{name}: byte anchors require literal consumers')
            if relative == 'turn-submit/negative.py':
                check_submit_anchor(static, name, fields, path, before, after, line, local, submit_maps['mutant_anchor'])
            count = declarations.get(name, 1)
            if relative == 'host-event-pump/negative_controls.py':
                if name in declarations:
                    raise Unsupported(f'{name}: duplicate count declaration')
                count = host_pump_count(tree, static, name, local)
            if name in declarations:
                used.add(name)
            fifth = None
            if relative not in IMPLICIT and relative not in FIELD_ORDER and len(fields) > 4:
                item = fields[4]
                if ((isinstance(item, ast.Constant) and isinstance(item.value, (int, float)))
                        or isinstance(item, ast.UnaryOp)):
                    fifth = static.value(item, line, local)
            if isinstance(fifth, (int, float)):
                if name in declarations:
                    raise Unsupported(f'{name}: duplicate count declaration')
                count = fifth
            if not positive_count(count):
                raise Unsupported(f'{name}: count must be a positive integer')
            scope = SCOPED_ROWS.get((relative, name), '')
            if scope:
                if count != 1:
                    raise Unsupported(f'{name}: function-scoped count must be one')
                check_scoped_consumer(static, name, scope)
                scoped.add(name)
            anchors.append(Anchor(name, static.confined(path), before, count, flags is not None, line, flags or 0, scope))
    # #996: compile-time switch tables count their single switch site per file.
    # fix-r3: the static whitelist stays the fast first gate; the imported
    # module is the truth for the executed table, so reconcile it here.
    switch_rows = switch_bindings(tree, static)
    for name, path, _test, _assertion, line in switch_rows:
        anchors.append(Anchor(name, path, f'::mutants::on("{name}")', 1, line=line, text_mutation=False))
    if switch_rows:
        audit_runtime_switches(script, switch_rows)
    # Imperative secondary replacements used alongside the declarative tables.
    # Match AST receiver + function scope, then read the existing argument; do
    # not duplicate the source fragment in this checker.
    extra = {
        'session-owner-hooks/negative.py': ('changed', 'source', None),
        'turn-gateway/negative-fence.py': ('s', 'submit', None),
        'model-complete/negative-controls.py': ('text', 'COMPLETE', 'self_test_hold'),
    }.get(relative)
    if extra:
        receiver, path_name, function = extra
        found = 0
        for call in ast.walk(tree):
            if not (isinstance(call, ast.Call) and isinstance(call.func, ast.Attribute)
                    and call.func.attr == 'replace' and isinstance(call.func.value, ast.Name)
                    and call.func.value.id == receiver):
                continue
            parent = static.parents.get(call)
            while parent is not None and not isinstance(parent, ast.FunctionDef):
                parent = static.parents.get(parent)
            if function and (parent is None or parent.name != function):
                continue
            path = static.target(ast.Name(id=path_name, ctx=ast.Load()), call.lineno, {})
            before = static.value(call.args[0], call.lineno, {})
            if not isinstance(before, str) or not before:
                raise Unsupported('invalid supplemental anchor')
            anchors.append(Anchor(f'supplemental-{call.lineno}', path, before, line=call.lineno))
            found += 1
        if not found:
            raise Unsupported('stale supplemental replacement rule')
    if declarations.keys() - used:
        raise Unsupported(f'unused ANCHOR_COUNTS: {sorted(declarations.keys() - used)}')
    expected_exemptions = {name for (file, name) in ROW_EXCEPTIONS.keys() | SEQUENTIAL_ROWS.keys()
                           if file == relative}
    if expected_exemptions - exempted:
        raise Unsupported(f'stale row exceptions: {sorted(expected_exemptions - exempted)}')
    expected_scopes = {name for (file, name) in SCOPED_ROWS if file == relative}
    if expected_scopes - scoped:
        raise Unsupported(f'stale scoped rows: {sorted(expected_scopes - scoped)}')
    if not anchors:
        raise Unsupported('no supported mutation anchors found; add "# anchor-selfcheck: unsupported <reason>" in the first 20 lines')
    return anchors, notes, tree


def positive_count(value):
    return type(value) is int and value > 0


def check_scoped_consumer(static, name, scope):
    """Keep this adapter tied to the existing function-scoped runner contract."""
    def shape(node):
        return ast.dump(node, include_attributes=False)

    expected = ast.parse(
        f'path.write_text(mutate(original, {scope!r} if name == {name!r} else None, '
        '[(old, new)], expected_count=ANCHOR_COUNTS.get(name, 1)))').body[0].value
    calls = [node for node in ast.walk(static.tree) if isinstance(node, ast.Call)
             and isinstance(node.func, ast.Name) and node.func.id == 'mutate']
    if len(calls) != 1 or shape(static.parents[calls[0]]) != shape(expected):
        raise Unsupported(f'{name}: unsupported function-scoped consumer')
    statement = static.parents[static.parents[calls[0]]]
    loop = static.parents.get(statement)
    if (not isinstance(loop, ast.For) or ast.unparse(loop.target) != '(name, path, old, new)'
            or ast.unparse(loop.iter) != 'mutations'):
        raise Unsupported(f'{name}: unsupported function-scoped loop')
    reads = [node for node in ast.walk(loop) if isinstance(node, ast.Assign)
             and any(isinstance(target, ast.Name) and target.id == 'original' for target in node.targets)]
    expected_read = ast.parse('original = originals[path].decode()').body[0]
    if len(reads) != 1 or shape(reads[0]) != shape(expected_read) or reads[0].lineno >= statement.lineno:
        raise Unsupported(f'{name}: unsupported function-scoped source read')


def submit_mapping(static, name):
    """Only one immutable declaration may supply a turn-submit override map."""
    definitions = static.defs.get(name, [])
    if len(definitions) != 1:
        raise Unsupported(f'{name}: expected one override mapping declaration')
    line, value = definitions[0]
    if not isinstance(value, ast.Dict):
        raise Unsupported(f'{name}: override mapping must be a literal mapping')
    for node in ast.walk(static.tree):
        if isinstance(node, ast.Name) and node.id == name:
            parent = static.parents[node]
            declaration = (isinstance(parent, ast.Assign) and parent.value is value
                           and len(parent.targets) == 1 and parent.targets[0] is node)
            if isinstance(node.ctx, (ast.Store, ast.Del)) and not declaration:
                raise Unsupported(f'{name}: unsupported override mapping binding')
            if isinstance(node.ctx, ast.Load) and node.lineno <= line:
                raise Unsupported(f'{name}: override mapping read before declaration')
            if (isinstance(parent, ast.Subscript) and isinstance(parent.ctx, (ast.Store, ast.Del))
                    or isinstance(parent, ast.Attribute) and parent.attr != 'get'):
                raise Unsupported(f'{name}: unsupported override mapping mutation')
            if isinstance(node.ctx, ast.Load):
                call = static.parents.get(parent)
                if not (isinstance(parent, ast.Attribute) and parent.attr == 'get'
                        and isinstance(call, ast.Call) and call.func is parent):
                    raise Unsupported(f'{name}: unsupported override mapping escape')
        if not isinstance(node, ast.Name) and name in bound_names(node):
            raise Unsupported(f'{name}: unsupported override mapping binding')
    result = static.value(value, line, {})
    if not isinstance(result, dict):
        raise Unsupported(f'{name}: override mapping must be a mapping')
    return result


def check_submit_anchor(static, name, fields, path, before, after, line, local, overrides):
    """Check the effective panic anchor, including overrides of a None marker."""
    if len(fields) != 6:
        raise Unsupported(f'{name}: expected six turn-submit fields')
    marker = static.value(fields[4], line, local)
    effective = overrides.get(name, ['message', marker])
    if not isinstance(effective, list) or len(effective) != 2:
        raise Unsupported(f'{name}: panic anchor must be a kind/value pair')
    kind, anchor = effective
    if kind not in ('message', 'location'):
        raise Unsupported(f'{name}: unsupported panic anchor kind {kind!r}')
    if kind == 'location':
        if not before.startswith('unwrap_or_else(') or not isinstance(after, str) or not after.startswith('unwrap()'):
            raise Unsupported(f'{name}: unwrap must start at the replacement span')
        if not static.confined(path).is_relative_to((static.root / 'src').resolve()):
            raise Unsupported(f'{name}: location anchor source must be inside rust workspace')
        # A literal cannot prove the runtime span. Require the source-bound
        # helper/consumer for every location, including hardcoded table values.
        derived = derived_submit_location(static, name, path, before, local)
        if anchor is not None and anchor != derived:
            raise Unsupported(f'{name}: location anchor differs from span: expected={derived} actual={anchor!r}')
        anchor = derived
    if not isinstance(anchor, str) or not anchor.strip():
        raise Unsupported(f'{name}: {kind} anchor must be a nonempty string')


def derived_submit_location(static, name, path, before, local):
    """Recognize the existing location helper and its source-bound consumer."""
    migrated = [node for node in ast.walk(static.tree) if isinstance(node, ast.Call)
                and ast.unparse(node.func) == 'adapter.unwrap_site']
    if migrated:
        expected_call = ast.parse('adapter.unwrap_site(rust, target, before, after, target.read_bytes())', mode='eval').body
        if len(migrated) != 1 or ast.dump(migrated[0], include_attributes=False) != ast.dump(expected_call, include_attributes=False):
            raise Unsupported(f'{name}: unsupported M6 location source binding')
        # Like switches, verify the executed build(), not merely a call name.
        # The child imports with __name__ != __main__, so cargo never starts.
        nonce = secrets.token_hex(16)
        bootstrap = '''import importlib.util,json,sys
nonce=sys.argv.pop()
spec=importlib.util.spec_from_file_location('m6_anchor_audit',sys.argv[1])
module=importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
entries,_=module.build()
print(json.dumps({'nonce':nonce,'rows':[[e.binding.mutation,str(e.path),e.before,e.after,e.binding.kind,e.binding.assertion] for e in entries if e.path is not None]}))
'''
        result = subprocess.run([sys.executable, '-I', '-B', '-c', bootstrap, str(static.script), nonce],
                                capture_output=True, text=True, timeout=30)
        if result.returncode != 0:
            raise Unsupported(f'{name}: M6 runtime anchor audit failed: {result.stderr[-600:]}')
        try:
            document = json.loads(result.stdout)
            rows = document['rows']
            if document['nonce'] != nonce:
                raise ValueError('nonce mismatch')
        except (ValueError, KeyError, TypeError) as error:
            raise Unsupported(f'{name}: invalid M6 runtime audit: {error}') from error
        declared = static.named('mutants', 10**9, local)
        if len(rows) != len(declared) or [row[0] for row in rows] != [row[0] for row in declared]:
            raise Unsupported(f'{name}: M6 runtime case inventory drift')
        routing = submit_mapping(static, 'mutant_source')
        for actual, declaration in zip(rows, declared):
            target = routing.get(declaration[0], static.named('source', 10**9, local))
            if actual[1:4] != [str(target), declaration[1], declaration[2]]:
                raise Unsupported(f'{name}: M6 runtime mutation drift')
        text = path.read_bytes().decode()
        if text.count(before) != 1:
            raise Unsupported(f'{name}: location span expected=1 actual={text.count(before)}')
        head = text[:text.index(before)]
        workspace = (static.root / 'src').resolve()
        derived = f'{path.relative_to(workspace)}:{head.count(chr(10)) + 1}:{len(head.rsplit(chr(10), 1)[-1]) + 1}'
        row = next(row for row in rows if row[0] == name)
        if row[4:] != ['location', derived]:
            raise Unsupported(f'{name}: M6 runtime location binding drift')
        return derived

    def shape(node):
        return ast.dump(node, include_attributes=False)

    expected = ast.parse('''def unwrap_site(anchor_source, before, original):
    assert before, 'a location anchor needs the mutation span'
    text = original.decode()
    assert text.count(before) == 1
    head = text[:text.index(before)]
    return f'{anchor_source.relative_to(rust)}:{head.count(chr(10)) + 1}:{len(head) - head.rfind(chr(10))}'
''').body[0]
    helpers = [node for node in ast.walk(static.tree)
               if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and node.name == 'unwrap_site']
    if len(helpers) != 1 or static.parents.get(helpers[0]) is not static.tree:
        raise Unsupported(f'{name}: unsupported location helper')
    helper = helpers[0]
    body = helper.body
    if body and isinstance(body[0], ast.Expr) and isinstance(body[0].value, ast.Constant) and isinstance(body[0].value.value, str):
        body = body[1:]
    if (not isinstance(helper, ast.FunctionDef) or helper.decorator_list or helper.returns
            or shape(helper.args) != shape(expected.args)
            or [shape(node) for node in body] != [shape(node) for node in expected.body]):
        raise Unsupported(f'{name}: unsupported location helper')
    calls = [node for node in ast.walk(static.tree) if isinstance(node, ast.Call)
             and ast.unparse(node.func) == 'unwrap_site']
    expected_branch = ast.parse("if anchor_kind == 'location':\n    anchor = unwrap_site(target, before, target_original)").body[0]
    if len(calls) != 1:
        raise Unsupported(f'{name}: unsupported location consumer')
    branch = static.parents.get(static.parents[calls[0]])
    if not isinstance(branch, ast.If) or shape(branch) != shape(expected_branch):
        raise Unsupported(f'{name}: unsupported location consumer')
    loop = static.parents.get(branch)
    if (not isinstance(loop, ast.For) or ast.unparse(loop.target) != '(name, before, after, case, marker, capability)'
            or ast.unparse(loop.iter) != 'selected' or branch not in loop.body):
        raise Unsupported(f'{name}: unsupported location loop')
    index = loop.body.index(branch)
    prelude = ast.parse('''target = mutant_source.get(name, source)
target_original = originals[target]
used_test = mutant_test.get(name, test)
anchor_kind, anchor = mutant_anchor.get(name, ('message', marker))
''').body
    if index < len(prelude) or [shape(node) for node in loop.body[index-len(prelude):index]] != [shape(node) for node in prelude]:
        raise Unsupported(f'{name}: unsupported location source binding')
    # Matching a definition/call is insufficient if either the helper or the
    # derived result is rebound elsewhere before the evidence is consumed.
    anchor_binding = loop.body[index - 1]
    allowed_writes = {node for statement in (anchor_binding, branch.body[0])
                      for node in ast.walk(statement)
                      if isinstance(node, ast.Name) and isinstance(node.ctx, ast.Store)}
    watched = {'unwrap_site', 'anchor', 'anchor_kind'}
    for node in ast.walk(static.tree):
        bindings = bound_names(node) if node not in allowed_writes and node is not helper else set()
        if bindings & watched:
            raise Unsupported(f'{name}: unsupported location binding: {sorted(bindings & watched)}')
    rust = static.named('rust', calls[0].lineno, local)
    if rust != (static.root / 'src').resolve():
        raise Unsupported(f'{name}: location helper workspace must be src')
    original = static.value(ast.parse('originals[target]', mode='eval').body,
                            calls[0].lineno, dict(local, target=path))
    if not isinstance(original, bytes) or original != static.confined(path).read_bytes():
        raise Unsupported(f'{name}: location snapshot must match source bytes')
    text = original.decode('utf-8')
    if text.count(before) != 1:
        raise Unsupported(f'{name}: location span expected=1 actual={text.count(before)}')
    head = text[:text.index(before)]
    return f'{path.relative_to(rust)}:{head.count(chr(10)) + 1}:{len(head) - head.rfind(chr(10))}'


def anchor_count(anchor):
    text = (anchor.path.read_bytes() if isinstance(anchor.before, bytes)
            else anchor.path.read_text(encoding='utf-8'))
    if anchor.scope:
        if not isinstance(anchor.before, str):
            raise Unsupported(f'{anchor.name}: function-scoped anchors must be strings')
        # Same method boundaries as turn-runner/_negative_common.mutate; never
        # import that executable runner to extract the function-scoped anchor.
        methods = list(re.finditer(r'^    (?:pub(?:\([^)]*\))? )?(?:async )?fn (\w+)', text, re.M))
        matches = [i for i, method in enumerate(methods) if method[1] == anchor.scope]
        if len(matches) != 1 or anchor.before != anchor.before.lstrip() or '\n' in anchor.before:
            raise Unsupported(f'{anchor.name}: invalid function-scoped anchor {anchor.scope}')
        index = matches[0]
        end = methods[index + 1].start() if index + 1 < len(methods) else len(text)
        text = text[methods[index].start():end]
    return sum(1 for _ in re.finditer(anchor.before, text, anchor.flags)) if anchor.regex else text.count(anchor.before)


def regex_flags(tree):
    """Recognize direct re.sub/subn consumers; unsupported flags fail closed."""
    def value(node):
        if isinstance(node, ast.Constant) and type(node.value) is int and node.value == 0:
            return 0
        if isinstance(node, ast.Attribute) and isinstance(node.value, ast.Name) and node.value.id == 're' and node.attr in ('S', 'DOTALL', 'M', 'MULTILINE', 'I', 'IGNORECASE', 'A', 'ASCII', 'X', 'VERBOSE'):
            return int(getattr(re, node.attr))
        if isinstance(node, ast.BinOp) and isinstance(node.op, ast.BitOr):
            return value(node.left) | value(node.right)
        raise Unsupported(f'unsupported regex flags: {ast.unparse(node)}')
    found = set()
    literal_replacement = False
    direct_calls = {node.func for node in ast.walk(tree) if isinstance(node, ast.Call)}
    for node in ast.walk(tree):
        if (isinstance(node, ast.Attribute) and isinstance(node.value, ast.Name)
                and node.value.id == 're' and node.attr in ('sub', 'subn', 'compile')
                and node not in direct_calls):
            raise Unsupported('regex function aliases require an explicit adapter')
        if isinstance(node, ast.ImportFrom) and node.module == 're':
            raise Unsupported('aliased/imported regex consumers require an explicit adapter')
        if isinstance(node, ast.Import) and any(n.name == 're' and n.asname for n in node.names):
            raise Unsupported('aliased regex module requires an explicit adapter')
        if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute):
            if node.func.attr in ('sub', 'subn') and ast.unparse(node.func.value) != 're':
                raise Unsupported('compiled/aliased regex consumers require an explicit adapter')
            if node.func.attr == 'replace':
                literal_replacement = True
        if isinstance(node, ast.Call) and ast.unparse(node.func) in ('re.sub', 're.subn'):
            if not node.args or not isinstance(node.args[0], ast.Name) or node.args[0].id not in ('pattern', 'before', 'old'):
                raise Unsupported('unsupported regex consumer')
            flag = node.args[4] if len(node.args) > 4 else ast.Constant(0)
            for keyword in node.keywords:
                if keyword.arg == 'flags':
                    flag = keyword.value
            found.add(value(flag))
    if len(found) > 1:
        raise Unsupported('mixed regex flag consumers require an explicit adapter')
    if found and literal_replacement:
        raise Unsupported('mixed literal/regex consumers require an explicit adapter')
    return next(iter(found)) if found else None


def environment_requirements(tree):
    """Report environment variables read by controls, excluding optional defaults.

Only these launcher prerequisites are relevant here. PDRS_* case selectors are
outputs of the runners, not required caller configuration.
"""
    required = set()
    names = {'PDRS_BASELINE', 'CARGO_TARGET_DIR', 'CARGO_HOME'}
    parents = {}
    for parent in ast.walk(tree):
        for field, children in ast.iter_fields(parent):
            for child in children if isinstance(children, list) else [children]:
                if isinstance(child, ast.AST):
                    parents[child] = (parent, field)

    def scope(node):
        result = []
        while node in parents:
            parent, field = parents[node]
            if isinstance(parent, (ast.FunctionDef, ast.AsyncFunctionDef, ast.If, ast.For,
                                   ast.While, ast.Try, ast.ExceptHandler, ast.With)):
                result.append((id(parent), field))
            node = parent
        return tuple(reversed(result))

    functions = {id(n) for n in ast.walk(tree) if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef))}

    def nonempty(node):
        # Conservative proof: unknown or empty defaults do not satisfy a
        # prerequisite. str(path / component) always spells a nonempty path.
        return ((isinstance(node, ast.Constant) and isinstance(node.value, str) and bool(node.value))
                or (isinstance(node, ast.Call) and ast.unparse(node.func) == 'str'
                    and len(node.args) == 1 and isinstance(node.args[0], ast.BinOp)
                    and isinstance(node.args[0].op, ast.Div)))

    supplied = []
    for node in sorted(ast.walk(tree), key=lambda n: getattr(n, 'lineno', 0)):
        key = None
        receiver = None
        if isinstance(node, ast.Assign):
            rebound = {target.id for target in node.targets if isinstance(target, ast.Name)}
            supplied = [item for item in supplied if item[0] not in rebound]
        if isinstance(node, ast.Subscript) and isinstance(node.value, (ast.Name, ast.Attribute)):
            key = node.slice
            receiver = ast.unparse(node.value)
            if isinstance(node.ctx, ast.Store) and isinstance(key, ast.Constant):
                parent, _ = parents[node]
                if isinstance(parent, ast.Assign) and nonempty(parent.value):
                    supplied.append((receiver, key.value, node.lineno, scope(node)))
                continue
        elif isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute) and node.func.attr in ('get', 'getenv', 'setdefault') and len(node.args) <= 2:
            default_name = 'value' if node.func.attr == 'setdefault' else 'default'
            default = node.args[1] if len(node.args) == 2 else next((k.value for k in node.keywords if k.arg == default_name), None)
            key = node.args[0] if node.args else next((k.value for k in node.keywords if k.arg == 'key'), None)
            receiver = ast.unparse(node.func.value)
            if nonempty(default):
                if node.func.attr == 'setdefault' and isinstance(key, ast.Constant):
                    supplied.append((receiver, key.value, node.lineno, scope(node)))
                continue
        if isinstance(key, ast.Constant) and key.value in names:
            here = scope(node)
            if not any(receiver == obj and key.value == name and line < node.lineno
                       and here[:len(where)] == where
                       and [frame for frame in here if frame[0] in functions] == [frame for frame in where if frame[0] in functions]
                       for obj, name, line, where in supplied):
                required.add(key.value)
    return required


def exemption_signals(tree):
    signals = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Name) and isinstance(node.ctx, ast.Store) and (
                node.id.lower() in TABLES or node.id.lower() in SWITCH_TABLE_NAMES):
            signals.add(f'table:{node.id}')
        if isinstance(node, ast.Call) and isinstance(node.func, ast.Name) and node.func.id in ('mutation', 'mutate'):
            signals.add(f'call:{node.func.id}')
    return signals


def marker_comments(source):
    """Yield (line number, comment text) for whole-line COMMENT tokens only.

    Marker text inside a string is not a comment, so the pre-#977 line scan
    (lstrip + regex per source line) cannot decide exemptions; only real
    tokenize COMMENT tokens can. An inline comment after code on the same
    line is not a whole-line marker either.
    """
    lines = source.split('\n')
    try:
        tokens = list(tokenize.generate_tokens(io.StringIO(source).readline))
    except (tokenize.TokenError, SyntaxError, ValueError) as error:
        raise Unsupported(f'unsupported marker source is not tokenizable: {error}') from error
    for token in tokens:
        if token.type != tokenize.COMMENT:
            continue
        number, column = token.start
        if lines[number - 1][:column].strip():
            continue
        yield number, token.string


def read_marker(script):
    """Return the header's unsupported reason, or None when it has no marker.

    Only whole-line comments in the first MARKER_LINE_LIMIT lines count. A
    missing file, malformed marker, empty reason, late marker or duplicate
    marker fails closed instead of silently becoming a checked script.
    """
    if not script.is_file():
        raise Unsupported(f'unsupported marker source does not exist: {script}')
    reason = None
    for number, stripped in marker_comments(script.read_text(encoding='utf-8')):
        match = MARKER_PATTERN.match(stripped)
        if match is None:
            if re.match(r'^#\s*anchor-selfcheck:', stripped):
                raise Unsupported(f'malformed unsupported marker on line {number}')
            continue
        if number > MARKER_LINE_LIMIT:
            raise Unsupported(f'unsupported marker must be within the first {MARKER_LINE_LIMIT} lines, found line {number}')
        text = (match.group(1) or '').strip()
        if not text:
            raise Unsupported(f'unsupported marker on line {number} needs a nonempty reason')
        if reason is not None:
            raise Unsupported(f'duplicate unsupported marker on line {number}')
        reason = text
    return reason


_TRACKED_FIXTURES = {}


def tracked_fixtures(root, fixtures):
    """Names of git-tracked fixture scripts: the discovery expectation.

    The index, not the working tree, is the expectation (#977 fix-r1): an
    exempt script without an UNSUPPORTED_SIGNALS registration that disappears
    from disk must still fail closed, so discovery alone cannot decide. An
    unreadable inventory fails closed too; untracked new scripts stay allowed.
    """
    prefix = f'{fixtures.relative_to(root).as_posix()}/'
    # One git process per (root, fixtures) per process: the index is static
    # here, and files deleted from disk are still caught because discovery
    # is recomputed on every check() call and compared against this list.
    key = (str(root), prefix)
    if key not in _TRACKED_FIXTURES:
        listing = subprocess.run(
            ['git', '-C', str(root), 'ls-files', '--', prefix.rstrip('/')],
            capture_output=True, text=True)
        if listing.returncode != 0:
            raise Unsupported(f'git ls-files failed: {(listing.stderr or "not a git checkout").strip()}')
        _TRACKED_FIXTURES[key] = {name[len(prefix):] for name in listing.stdout.splitlines()
                                  if name.startswith(prefix) and name.endswith('.py')}
    return set(_TRACKED_FIXTURES[key])


def check(root, *, anchors_only=False, environ=None):
    root = root.resolve()
    fixtures = root / 'src/pilotdeck-gateway/tests/fixtures'
    # Inventory every Python fixture: filenames are not proof that a script has
    # no mutations. Non-control helpers declare their reason in their own header.
    scripts = sorted(fixtures.rglob('*.py'))
    env = os.environ if environ is None else environ
    errors, checked, total, skipped = [], 0, 0, 0
    matched, pending_names = set(), set()
    try:
        registrations = pending.load(root)
    except (OSError, ValueError) as error:
        errors.append(str(error))
        registrations = []
    if not scripts:
        errors.append('no negative scripts discovered')
    # A tracked script that vanished is an error even without signals or a
    # marker entry naming it; rglob alone would silently skip it (#977 fix-r1).
    discovered = {script.relative_to(fixtures).as_posix() for script in scripts}
    try:
        expected = tracked_fixtures(root, fixtures)
    except (Unsupported, OSError) as error:
        errors.append(f'fixture inventory: {error}')
        expected = set()
    for name in sorted(expected - discovered):
        errors.append(f'missing fixture script: {name}')
    markers, marker_errors = {}, set()
    for script in scripts:
        relative = script.relative_to(fixtures).as_posix()
        try:
            reason = read_marker(script)
        except (Unsupported, OSError) as error:
            errors.append(f'{relative}: {error}')
            marker_errors.add(relative)
            continue
        if reason is not None:
            markers[relative] = reason
    for name in UNSUPPORTED_SIGNALS:
        if name not in markers:
            errors.append(f'stale UNSUPPORTED_SIGNALS entry: {name}')
    for script in scripts:
        relative = script.relative_to(fixtures).as_posix()
        if relative in marker_errors:
            continue
        try:
            tree = ast.parse(script.read_text(encoding='utf-8'), filename=str(script))
            for name in sorted(environment_requirements(tree)):
                if not env.get(name):
                    message = f'{relative}: MISSING_ENV {name}'
                    print(message)
                    if not anchors_only:
                        errors.append(message)
            if relative in markers:
                actual = exemption_signals(tree)
                expected = UNSUPPORTED_SIGNALS.get(relative, set())
                if actual != expected:
                    raise Unsupported(f'unsupported exemption signals: expected={sorted(expected)} actual={sorted(actual)}')
                print(f'UNSUPPORTED {relative}: {markers[relative]}')
                skipped += 1
                continue
            anchors, notes, _ = extract(script, root)
            checked += 1
            for note in notes:
                print(f'NON_SOURCE {relative}: {note}')
            stale = {}
            for anchor in anchors:
                actual = anchor_count(anchor)
                total += 1
                entry = next((entry for entry in registrations
                              if entry.matches(relative, anchor.name)), None)
                if entry is not None:
                    matched.add(entry)
                    if not anchor.text_mutation:
                        errors.append(f'{relative} {anchor.name}: pending-reanchor does not apply to compiled switches')
                        entry = None
                    elif not pending.supports_host(relative):
                        errors.append(f'{relative} {anchor.name}: pending-reanchor requires a shared skip entry; migrate the host first')
                        entry = None
                detail = (f'{relative}:{anchor.line} {anchor.name}: '
                          f'{anchor.path.relative_to(root)} expected={anchor.count} actual={actual}')
                if entry is not None:
                    pending_names.add((relative, anchor.name))
                    stale[anchor.name] = stale.get(anchor.name, True) and actual == anchor.count
                if actual != anchor.count:
                    if entry is not None:
                        print(f'WARN {detail}: {entry.reason()}')
                    else:
                        errors.append(detail)
            for name, unchanged in stale.items():
                if unchanged:
                    print(f'WARN {relative} {name}: pending-reanchor anchor still matches; remove registration / 删除登记')
            print(f'CHECKED {relative}: {len(anchors)} anchors')
        except (Unsupported, SyntaxError, OSError, ValueError, TypeError, KeyError, IndexError, re.error) as error:
            errors.append(f'{relative}: {error}')
    for entry in registrations:
        if entry not in matched:
            errors.append(f'{entry.fixture} {entry.mutation}: pending-reanchor registration has no checked text mutation')
    for error in errors:
        print(f'ERROR {error}')
    print(f'SUMMARY scripts={len(scripts)} checked={checked} unsupported={skipped} anchors={total} errors={len(errors)} pending-reanchor={len(pending_names)} mode={"anchors-only" if anchors_only else "anchors+environment"}')
    return 1 if errors else 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=Path(__file__).resolve().parents[4])
    parser.add_argument('--anchors-only', action='store_true', help='CI: report environment gaps without failing on them')
    args = parser.parse_args()
    start = time.monotonic()
    result = check(args.root, anchors_only=args.anchors_only)
    print(f'ELAPSED {time.monotonic() - start:.3f}s (no cargo or controls executed)')
    return result


if __name__ == '__main__':
    sys.exit(main())
