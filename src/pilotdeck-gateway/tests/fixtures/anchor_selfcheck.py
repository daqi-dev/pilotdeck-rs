#!/usr/bin/env python3
"""Read-only negative-control preflight; never imports a control or runs cargo.

python3 -I .../anchor_selfcheck.py --anchors-only   # CI, report env gaps
python3 -I .../anchor_selfcheck.py                  # local, env gaps fail

Supported tables: (name, path, before, after[, count/test...]), single-source
(name, before, after...), and mutation(name, path, before, after, ...) calls.
Paths/constants are interpreted by a small AST allowlist. ANCHOR_COUNTS is an
optional {name: positive integer} declaration of *source occurrences*, distinct
from replace(..., 1)'s replacement limit. Unknown formats fail closed.
"""
import argparse
import ast
import json
from dataclasses import dataclass
from pathlib import Path
import re
import sys
import time
import os

TABLES = {'mutants', 'mutations', 'controls', 'cases', 'runner_mutations'}
# These scripts do not have a declarative, whole-file source replacement table.
# Keep exact paths and explanations; new scripts never inherit an exemption.
UNSUPPORTED = {
    '_shared/negative_control.py': 'shared mutation runner, no mutation definitions',
    'negative_process.py': 'shared process/source guard, no mutation definitions',
    'negative_oracle_guard.py': 'JSON provenance corruption, no source before fragments',
    'test_negative_process.py': 'unit tests of the shared process/source guard',
    'background-output/test_negative.py': 'unit tests of the mutation runner',
    'replacement/test-negative-518.py': 'unit tests of the mutation runner',
    'edgeclaw-entries/test_negative_runners.py': 'unit tests of the mutation runners',
    'turn-runner/_negative_common.py': 'shared function-scoped replacement helper',
    'turn-runner/negative_controls.py': 'anchors are scoped to Rust functions by _negative_common.mutate',
    'turn-runner/runner_negative.py': 'anchors are scoped to Rust functions by _negative_common.mutate',
    'turn-runner/input_negative.py': 'mixed function-scoped and whole-file replacements',
    'turn-runner/live_negative.py': 'ordered multi-step replacements on a generated function body',
    'runtime-spawn-panic/negative_controls.py': 'conditional sequential re.subn rewrites of changed content with different flags, not whole-file table anchors',
    'plugin-runtime/negative-controls.py': 'CASES contains precomputed complete mutated files, not before fragments',
    'plugin-runtime/panic-negative.py': 'CASES contains precomputed complete mutated files, not before fragments',
    'plugin-runtime/notify-window-negative.py': 'imperative slicing and chained replacements, no mutation table',
    'edgeclaw-entries/negative_fix1.py': 'imperative function-body slicing and multiple run calls',
    'edgeclaw-export/negative_locks.py': 'imperative sequential once/write mutations',
    'skills-tree-files/negative.py': 'generator computes function spans and yields transformed bodies',
    'tool-budget/negative_project.py': 'imperative function-scoped replacements',
    'spill-isolation/negative_controls.py': 'name-dependent function slicing and sequential rewrites',
    'session-domain/negative.py': 'imperative nth-occurrence and sliced guard mutations',
    'session-domain/adapter-negative.py': 'imperative replacement chain, no mutation table',
    'session-domain/fix1-negative.py': 'imperative replacement chain, no mutation table',
    'session-domain/fix2-negative.py': 'imperative replacement chain, no mutation table',
    'recorder/fix3-negative.py': 'imperative replacement chain, no mutation table',
    'run-registry/count-negative.py': 'imperative replacement chain, no mutation table',
    'skills/negative_controls.py': 'mixed imperative Rust edits and generated TS fixture corruption',
}
REGEX_SCRIPTS = {'background-panic/negative_controls.py'}
IMPLICIT = {
    'hooks-output/negative.py': 'source',
    'model-complete/negative-controls.py': 'HTTP',
    'session-owner-hooks/negative.py': 'source',
    'edgeclaw-file-memory/negative.py': 'source',
    'edgeclaw-heartbeat/negative.py': 'helpers',
    'edgeclaw-entries/negative_round7.py': 'path',
    'turn-submit/negative.py': 'source',
}
ROW_EXCEPTIONS = {
    ('turn-gates/negative.py', 'factory-ready-priority'): 'moves the lazy factory branch before cancellation with ordered replacements on the first select',
    ('turn-gates/negative.py', 'omit-prepared-cleanup'): 'moves pending_host.handoff with ordered removal and insertion',
    ('turn-runner/recording_negative.py', 'text-value'): 'anchor is scoped to from_agent_input by _negative_common.mutate',
    ('session-abort-contract/negative.py', 'tampered-source-hash'): 'JSON provenance corruption, not a source replacement',
    ('session-owner/negative.py', 'tampered-source-hash'): 'JSON provenance corruption, not a source replacement',
    ('session-admission/negative.py', 'tampered-source-hash'): 'JSON provenance corruption, not a source replacement',
    ('connection-context/negative.py', 'tampered-source-hash'): 'JSON provenance corruption, not a source replacement',
    ('user-dialog-channel/negative.py', 'tampered-source-hash'): 'JSON provenance corruption, not a source replacement',
}


class Unsupported(ValueError):
    pass


@dataclass
class Anchor:
    name: str
    path: Path
    before: str
    count: int = 1
    regex: bool = False
    line: int = 0
    flags: int = 0


class Static:
    """Interpret only pure literals, paths and string operations. No eval/exec."""
    def __init__(self, script, root, tree):
        self.script, self.root = script, root
        self.defs = {}
        self.parents = {}
        for node in ast.walk(tree):
            for child in ast.iter_child_nodes(node):
                self.parents[child] = node
            if isinstance(node, ast.Assign):
                for target in node.targets:
                    if isinstance(target, ast.Name):
                        self.defs.setdefault(target.id, []).append((node.lineno, node.value))
                    elif isinstance(target, ast.Tuple):
                        for index, item in enumerate(target.elts):
                            if isinstance(item, ast.Name):
                                part = ast.Subscript(value=node.value, slice=ast.Constant(index), ctx=ast.Load())
                                self.defs.setdefault(item.id, []).append((node.lineno, part))
        for defs in self.defs.values():
            defs.sort(key=lambda pair: pair[0])

    def named(self, name, line, local):
        if name in local:
            return local[name]
        if name == '__file__':
            return str(self.script)
        choices = [(n, value) for n, value in self.defs.get(name, []) if n < line]
        if not choices:
            raise Unsupported(f'unresolved name {name}')
        n, value = choices[-1]
        return self.value(value, n, local)

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
        if isinstance(node, (ast.List, ast.Tuple)):
            return [self.value(n, line, local) for n in node.elts]
        if isinstance(node, ast.Dict):
            return {self.value(k, line, local): self.value(v, line, local)
                    for k, v in zip(node.keys, node.values)}
        if isinstance(node, (ast.ListComp, ast.DictComp)):
            if len(node.generators) != 1 or node.generators[0].ifs:
                raise Unsupported('only unconditional single-loop comprehensions are supported')
            gen = node.generators[0]
            result = []
            for item in self.value(gen.iter, line, local):
                bound = self.bind(gen.target, item, local)
                result.append((self.value(node.key, line, bound), self.value(node.value, line, bound))
                              if isinstance(node, ast.DictComp) else self.value(node.elt, line, bound))
            return dict(result) if isinstance(node, ast.DictComp) else result
        if isinstance(node, ast.BinOp):
            left, right = self.value(node.left, line, local), self.value(node.right, line, local)
            if isinstance(node.op, ast.Div) and isinstance(left, Path) and isinstance(right, str):
                return left / right
            if isinstance(node.op, ast.Add) and type(left) is type(right) and isinstance(left, (str, list)):
                return left + right
            if isinstance(node.op, ast.Mult) and isinstance(left, str) and type(right) is int and 0 <= right <= 10000:
                return left * right
        if isinstance(node, ast.Attribute):
            obj = self.value(node.value, line, local)
            if isinstance(obj, Path) and node.attr in ('parent', 'parents', 'name', 'stem'):
                return getattr(obj, node.attr)
        if isinstance(node, ast.Subscript):
            if isinstance(node.value, ast.Name) and node.value.id == 'snapshots':
                path = self.value(node.slice, line, local)
                return self.confined(path).read_bytes()
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
                    if method == 'glob' and len(args) == 1:
                        return sorted(self.confined(obj).glob(args[0]))
                if isinstance(obj, bytes) and method == 'decode' and not args:
                    return obj.decode('utf-8')
                if isinstance(obj, str) and method in ('replace', 'index', 'split', 'join'):
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
            if value.startswith('src/'):
                return self.confined(self.root / value)
            if value.startswith('pilotdeck-gateway/'):
                return self.confined(self.root / 'src' / value)
            # A filename key must map to its actual read path, never a basename search.
            for n, mapping in self.defs.get('originals', []):
                if n < line and isinstance(mapping, ast.Dict):
                    for key, expression in zip(mapping.keys, mapping.values):
                        if self.value(key, n, local) == value and isinstance(expression, ast.Call) and isinstance(expression.func, ast.Attribute) and expression.func.attr == 'read_text':
                            return self.target(expression.func.value, n, local)
            for base in ('source', 'SRC', 'module'):
                try:
                    folder = self.named(base, line, local)
                except Unsupported:
                    continue
                if isinstance(folder, Path) and folder.is_dir():
                    return self.confined(folder / value)
        raise Unsupported(f'unresolved source path: {value!r}')


def table_rows(tree):
    """Only mutation table definitions/extensions, not incidental tuples."""
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
        if isinstance(node, (ast.Assign, ast.AugAssign)):
            targets = node.targets if isinstance(node, ast.Assign) else [node.target]
            for target in targets:
                if (isinstance(target, ast.Subscript) and isinstance(target.value, ast.Name)
                        and target.value.id.lower() in TABLES):
                    raise Unsupported('in-place mutation table assignment is not supported')
            if any(isinstance(t, ast.Name) and t.id.lower() in TABLES for t in targets):
                yield from lists(node.value)
        if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute) and isinstance(node.func.value, ast.Name) and node.func.value.id.lower() in TABLES:
            if node.func.attr == 'append' and len(node.args) == 1:
                yield node.args[0]
            elif node.func.attr == 'extend' and len(node.args) == 1:
                yield from lists(node.args[0])
            else:
                raise Unsupported('unsupported mutation table update')


def extract(script, root):
    relative = script.relative_to(root / 'src/pilotdeck-gateway/tests/fixtures').as_posix()
    tree = ast.parse(script.read_text(encoding='utf-8'), filename=str(script))
    static = Static(script, root, tree)
    flags = regex_flags(tree)
    anchors, notes = [], []
    declarations = static.named('ANCHOR_COUNTS', 10**9, {}) if 'ANCHOR_COUNTS' in static.defs else {}
    if not isinstance(declarations, dict) or any(not isinstance(k, str) or type(v) is not int or v < 1 for k, v in declarations.items()):
        raise Unsupported('ANCHOR_COUNTS must map names to positive integers')
    used = set()
    exempted = set()
    for row in table_rows(tree):
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
            if (relative, name) in ROW_EXCEPTIONS:
                if not ROW_EXCEPTIONS[relative, name].strip():
                    raise Unsupported(f'{name}: empty row exception reason')
                notes.append(f'{name}: {ROW_EXCEPTIONS[relative, name]}')
                exempted.add(name)
                continue
            if relative in IMPLICIT:
                target = ast.Name(id=IMPLICIT[relative], ctx=ast.Load())
                before_node = fields[1]
                if relative == 'turn-submit/negative.py':
                    routing = static.named('mutant_source', 10**9, {})
                    path = routing.get(name, static.value(target, line, local))
                else:
                    path = static.target(target, line, local)
            else:
                if len(fields) < 4:
                    raise Unsupported(f'{name}: expected name/path/before/after')
                path = static.target(fields[1], line, local)
                before_node = fields[2]
            before = static.value(before_node, line, local)
            if before is None and relative in REGEX_SCRIPTS and path.suffix == '.json':
                notes.append(f'{name}: JSON matrix/provenance mutation, no source anchor')
                continue
            # Generated after strings may depend on extra split/index/replace_once
            # anchors. Validate these dependencies without applying a mutation.
            static.value(fields[2 if relative in IMPLICIT else 3], line, local)
            if not isinstance(before, str) or not before:
                raise Unsupported(f'{name}: empty or non-string anchor')
            count = declarations.get(name, 1)
            if name in declarations:
                used.add(name)
            fifth = None
            if relative not in IMPLICIT and len(fields) > 4:
                item = fields[4]
                if ((isinstance(item, ast.Constant) and isinstance(item.value, (int, float)))
                        or isinstance(item, ast.UnaryOp)):
                    fifth = static.value(item, line, local)
            if isinstance(fifth, (int, float)):
                if name in declarations:
                    raise Unsupported(f'{name}: duplicate count declaration')
                count = fifth
            if type(count) is not int or count < 1:
                raise Unsupported(f'{name}: count must be a positive integer')
            anchors.append(Anchor(name, static.confined(path), before, count, flags is not None, line, flags or 0))
    # Imperative secondary replacements used alongside the declarative tables.
    # Match AST receiver + function scope, then read the existing argument; do
    # not duplicate the source fragment in this checker.
    extra = {
        'session-owner-hooks/negative.py': ('changed', 'source', None),
        'turn-gateway/negative-fence.py': ('s', 'submit', None),
        'model-complete/negative-controls.py': ('text', 'HTTP', 'self_test_hold'),
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
    expected_exemptions = {name for (file, name) in ROW_EXCEPTIONS if file == relative}
    if expected_exemptions - exempted:
        raise Unsupported(f'stale row exceptions: {sorted(expected_exemptions - exempted)}')
    if not anchors:
        raise Unsupported('no supported mutation anchors found; add an explicit reason to UNSUPPORTED')
    return anchors, notes, tree


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
        elif isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute) and node.func.attr in ('get', 'getenv') and len(node.args) == 1:
            key = node.args[0]
            receiver = ast.unparse(node.func.value)
        elif isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute) and node.func.attr == 'setdefault' and len(node.args) == 2 and isinstance(node.args[0], ast.Constant):
            if nonempty(node.args[1]):
                supplied.append((ast.unparse(node.func.value), node.args[0].value, node.lineno, scope(node)))
        if isinstance(key, ast.Constant) and key.value in names:
            here = scope(node)
            if not any(receiver == obj and key.value == name and line < node.lineno
                       and here[:len(where)] == where
                       and [frame for frame in here if frame[0] in functions] == [frame for frame in where if frame[0] in functions]
                       for obj, name, line, where in supplied):
                required.add(key.value)
    return required


def check(root, *, anchors_only=False, environ=None):
    root = root.resolve()
    fixtures = root / 'src/pilotdeck-gateway/tests/fixtures'
    scripts = sorted(fixtures.rglob('*negative*.py'))
    env = os.environ if environ is None else environ
    errors, checked, total, skipped = [], 0, 0, 0
    paths = {p.relative_to(fixtures).as_posix() for p in scripts}
    for name, reason in UNSUPPORTED.items():
        if name not in paths or not reason.strip():
            errors.append(f'stale/empty UNSUPPORTED entry: {name}')
    if not scripts:
        errors.append('no negative scripts discovered')
    for script in scripts:
        relative = script.relative_to(fixtures).as_posix()
        try:
            tree = ast.parse(script.read_text(encoding='utf-8'), filename=str(script))
            for name in sorted(environment_requirements(tree)):
                if not env.get(name):
                    message = f'{relative}: MISSING_ENV {name}'
                    print(message)
                    if not anchors_only:
                        errors.append(message)
            if relative in UNSUPPORTED:
                print(f'UNSUPPORTED {relative}: {UNSUPPORTED[relative]}')
                skipped += 1
                continue
            anchors, notes, _ = extract(script, root)
            checked += 1
            for note in notes:
                print(f'NON_SOURCE {relative}: {note}')
            for anchor in anchors:
                text = anchor.path.read_text(encoding='utf-8')
                actual = sum(1 for _ in re.finditer(anchor.before, text, anchor.flags)) if anchor.regex else text.count(anchor.before)
                total += 1
                if actual != anchor.count:
                    errors.append(f'{relative}:{anchor.line} {anchor.name}: {anchor.path.relative_to(root)} expected={anchor.count} actual={actual}')
            print(f'CHECKED {relative}: {len(anchors)} anchors')
        except (Unsupported, SyntaxError, OSError, ValueError, TypeError, KeyError, IndexError, re.error) as error:
            errors.append(f'{relative}: {error}')
    for error in errors:
        print(f'ERROR {error}')
    print(f'SUMMARY scripts={len(scripts)} checked={checked} unsupported={skipped} anchors={total} errors={len(errors)} mode={"anchors-only" if anchors_only else "anchors+environment"}')
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
