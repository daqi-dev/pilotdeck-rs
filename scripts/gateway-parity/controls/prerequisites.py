"""Offline A4 prerequisite controls: mutate inputs and demand rejection.

No listeners, archive edits, or baseline edits. --code-root can run the same
probes against a saved pre-change tree to demonstrate the original blind spots.
"""
from contextlib import redirect_stdout
import copy
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[3]


def module_at(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def run_prerequisites(output, code_root=ROOT):
    code_root = code_root.resolve()
    scripts = code_root / 'scripts/gateway-parity'
    common = module_at('prerequisite_common', scripts / 'common.py')
    output.mkdir(parents=True, exist_ok=False)
    results = {}
    # A3 prerequisites have their own evidence manifest, separate from the
    # A2 apparatus. Do not reintroduce A3 into common.apparatus_hashes().
    inputs = (
        'scripts/gateway-parity/check_schema.py', 'scripts/gateway-parity/export_schema.mjs',
        'scripts/gateway-parity/validate_fixtures.mjs',
        'scripts/gateway-parity/fixtures/gateway-protocol.fixtures.json',
        'scripts/gateway-parity/package.json', 'scripts/gateway-parity/package-lock.json',
        'schema/gateway-protocol.schema.json', 'schema/gateway-methods.snapshot.json',
        'docs/research/005_phase-a-gateway/method-inventory.md',
        'specs/gateway-parity/normalization.md', '.github/workflows/ci.yml',
    )

    def input_hashes(root=code_root):
        return {name: hashlib.sha256((root / name).read_bytes()).hexdigest() for name in inputs}

    prerequisite_hashes = input_hashes()
    audit_path = code_root / 'docs/reviews/002_a4-prerequisites.md'
    audit_text = audit_path.read_text() if audit_path.is_file() else ''
    audit_hash = common.digest(audit_path) if audit_path.is_file() else None

    def record(name, passed, detail):
        results[name] = {'passed': bool(passed), 'detail': detail}
        print(f"[{'PASS' if passed else 'FAIL'}] {name}: {detail}")

    # Presence of the seven required audit rows is a machine gate. This only
    # checks the artifact; it cannot certify two independent model approvals.
    views = ('复用', '简化', '效率', '层级', 'sibling', 'concurrency', 'boundary')

    def audit_complete(text):
        return all(f'| {view} |' in text for view in views)

    record('audit-seven-views', audit_complete(audit_text), 'four simplify views and three pr-ready roles must exist')
    record('audit-missing-artifact', not audit_complete(''), 'missing audit artifact must FAIL')
    for view in views:
        incomplete = audit_text.replace(f'| {view} |', '| OMITTED |')
        record('audit-missing-' + view, not audit_complete(incomplete), 'missing required audit row must FAIL')

    # Load the selected version; compare.py imports common by module name.
    sys.path.insert(0, str(scripts))
    normalizer = module_at('prerequisite_compare', scripts / 'compare.py').Normalizer
    for marker in ('sessionId', 'runId', 'toolCallId'):
        for field in ('startedAt', 'completedAt'):
            a = {marker: 'business', field: 1}
            b = dict(a, **{field: 2})
            status = 'PASS' if normalizer().value(a) == normalizer().value(b) else 'FAIL'
            record(f'business-{marker}-{field}', status == 'FAIL', f'comparison={status}; expected FAIL')
    turn = dict(type='success', sessionId='s', turnId='t', stopReason='completed',
                usage={}, turns=1, startedAt=1, completedAt=2)
    tool = dict(type='error', toolCallId='c', toolName='bash', content=[],
                startedAt=1, completedAt=2)
    for name, shape in [('turn', turn), ('tool', tool)]:
        changed = dict(shape, startedAt=3, completedAt=4)
        record(f'metadata-{name}', normalizer().value(shape) == normalizer().value(changed),
               'documented metadata clocks still normalize')
        for key in sorted(shape.keys() - {'startedAt', 'completedAt'}):
            partial = dict(shape)
            partial.pop(key)
            changed = dict(partial, startedAt=3, completedAt=4)
            record(f'partial-{name}-{key}', normalizer().value(partial) != normalizer().value(changed),
                   'incomplete container must retain clocks')
        unknown = dict(shape, type='business')
        changed = dict(unknown, startedAt=3, completedAt=4)
        record(f'wrong-type-{name}', normalizer().value(unknown) != normalizer().value(changed),
               'unknown discriminator must retain clocks')
    x = '00000000-0000-4000-8000-000000000001'
    y = '00000000-0000-4000-8000-000000000002'
    named = normalizer()
    named.index({'id': x, 'name': 'entity', 'type': 'item', 'scope': 'local'})
    normalized_path = named.value('/project/' + x + '/fixture.txt')
    record('path-uuid-fragment', normalized_path.startswith('/project/<uuid:')
           and normalized_path.endswith('/fixture.txt') and named.value('/other/' + x + '/fixture.txt') != normalized_path,
           'only anchored UUID fragment changes; root and filename stay exact')
    for text in ('message: ' + x + '!', 'run-' + x + ':timeline', '/project/' + x + '/file'):
        record('string-uuid-fragment-' + text, named.value(text) == text.replace(x, named.ids[x]),
               'anchored UUID substitutes in any string, preserving other characters')
    record('string-unanchored-uuid', named.value('message: ' + y + '!') == 'message: ' + y + '!',
           'unanchored UUID stays exact')
    record('string-uuid-word-boundary', named.value('prefix' + x + 'suffix') == 'prefix' + x + 'suffix',
           'UUID regex word boundaries still apply')
    try:
        named.index({'id': y, 'name': 'entity', 'type': 'item', 'scope': 'local'})
        duplicate_rejected = False
    except ValueError:
        duplicate_rejected = True
    record('named-ambiguous-key', duplicate_rejected, 'same stable key with different UUIDs is rejected')
    for shape in ({'id': x}, {'name': 'entity'}, {'id': x, 'name': 'entity', 'type': 'request'},
                  {'id': x, 'name': 'entity', 'type': 'response'}, {'id': x, 'name': 'entity', 'type': 'event'}):
        n = normalizer()
        n.index(shape)
        record('named-excluded-' + str(shape), not n.ids, 'incomplete/protocol shape is not a named entity anchor')

    # Isolated SCRIPT/ROOT: actual hash function, no mutation of working scripts.
    with tempfile.TemporaryDirectory(prefix='pdrs-a4-hashes-') as scratch:
        common.ROOT = Path(scratch)
        common.SCRIPT = Path(scratch) / 'scripts/gateway-parity'
        names = getattr(common, 'A2_SCRIPTS', (
            'check_frozen.py', 'check_ruler.py', 'common.py', 'compare.py',
            'controls/run_all.py', 'driver.mts', 'mock.mjs', 'observer.py',
            'run_round.py', 'test_safety.py', 'watcher.mjs'))
        for name in names:
            path = common.SCRIPT / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('A2')
        hashes = common.apparatus_hashes()
        for name in ('check_schema.py', 'export_schema.mjs', 'validate_fixtures.mjs',
                     'node_modules/dependency/index.mjs', 'nested/node_modules/dependency/tool.py',
                     'notes.txt'):
            path = common.SCRIPT / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('A3 or installed dependency')
            record('apparatus-excludes-' + name, common.apparatus_hashes() == hashes,
                   'unrelated script cannot invalidate A2 evidence')
        for name in ('extra.py', 'extra.mjs', 'extra.mts', 'nested/extra.py',
                     'nested/check_schema.py'):
            path = common.SCRIPT / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('unregistered script')
            try:
                common.apparatus_hashes()
                unknown_rejected = False
            except ValueError as error:
                unknown_rejected = 'unregistered gateway-parity scripts:' in str(error) and name in str(error)
            finally:
                path.unlink()
            record('apparatus-unregistered-' + name, unknown_rejected,
                   'script outside A2 whitelist and three A3 tools must FAIL')
        (common.SCRIPT / 'compare.py').write_text('modified A2')
        record('apparatus-binds-A2', common.apparatus_hashes() != hashes, 'A2 edit changes evidence hash')
        (common.SCRIPT / 'compare.py').unlink()
        try:
            common.apparatus_hashes()
            missing_rejected = False
        except FileNotFoundError:
            missing_rejected = True
        record('apparatus-missing-A2', missing_rejected, 'missing member must FAIL, not shrink manifest')

    with tempfile.TemporaryDirectory(prefix='pdrs-a4-audit-hash-') as scratch:
        audit = Path(scratch) / 'audit.md'
        raw = '审计\r\n'.encode()
        audit.write_bytes(raw)
        before = common.digest(audit)
        record('audit-digest-raw-bytes', before == hashlib.sha256(raw).hexdigest()
               and before != hashlib.sha256(audit.read_text().encode()).hexdigest(),
               'CRLF audit uses the same raw-byte digest as check_ruler')
        audit.write_bytes(raw.replace(b'\r\n', b'\n'))
        record('audit-digest-line-endings', common.digest(audit) != before,
               'line-ending-only edits invalidate audit evidence')

    # Mutation of any independent prerequisite input must invalidate its gate,
    # including after A2 evidence was collected. Never edit real source inputs.
    with tempfile.TemporaryDirectory(prefix='pdrs-a4-prerequisite-hashes-') as scratch:
        scratch = Path(scratch)
        for name in inputs:
            target = scratch / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(code_root / name, target)
        before = input_hashes(scratch)
        record('prerequisite-manifest-stable', input_hashes(scratch) == before, 'unchanged inputs keep manifest')
        for name in inputs:
            target = scratch / name
            original = target.read_bytes()
            target.write_bytes(original + b'\nINJECTED_CHANGE')
            status = 'PASS' if input_hashes(scratch) == before else 'FAIL'
            record('prerequisite-mutation-' + name, status == 'FAIL', f'stability gate={status}; expected FAIL')
            target.write_bytes(original)

    fixtures = json.loads((scripts / 'fixtures/gateway-protocol.fixtures.json').read_text())
    schema = json.loads((code_root / 'schema/gateway-protocol.schema.json').read_text())
    snapshot = json.loads((code_root / 'schema/gateway-methods.snapshot.json').read_text())
    aliases = [e[k] for e in snapshot['payloadAliases'] for k in ('input', 'result') if e.get(k)]

    def validate(name, data, expected=1, schema_data=None, required_message=None):
        fixture_path = output / (name + '.fixtures.json')
        fixture_path.write_text(json.dumps(data))
        schema_path = output / (name + '.schema.json')
        schema_path.write_text(json.dumps(schema if schema_data is None else schema_data))
        proc = subprocess.run(['node', str(scripts / 'validate_fixtures.mjs'),
                               '--fixtures', str(fixture_path), '--schema', str(schema_path)],
                              capture_output=True, text=True, timeout=30)
        log = proc.stdout + proc.stderr
        (output / (name + '.txt')).write_text(log)
        # A dependency error or uncaught exception is not a successful control.
        rejected = ('FAIL validate_fixtures: fixtures 证据缺失/畸形' in log
                    or ('validate_fixtures:' in log and '项 FAIL' in log))
        if name.startswith('alias-open-'):
            rejected = f'[FAIL] definition {name.removeprefix("alias-open-")} ' in log
        if required_message is not None:
            rejected = rejected and f'  - {required_message}' in log.splitlines()
        passed = proc.returncode == expected and (expected == 0 or rejected)
        record(name, passed, f'exit {proc.returncode}; expected {expected}; contractRejected={rejected}'
               + (f'; requiredMessage={required_message}' if required_message is not None else ''))

    validate('fixtures-valid', fixtures, expected=0)
    validate('fixtures-missing', {})
    validate('fixtures-null', None)
    validate('fixtures-array', [])
    validate('fixtures-all-empty', {'frames': {'positive': [], 'negative': []}, 'definitions': []})
    for group in ('positive', 'negative'):
        for kind, value in [('missing', None), ('empty', []), ('wrong-type', {})]:
            bad = copy.deepcopy(fixtures)
            if kind == 'missing':
                del bad['frames'][group]
            else:
                bad['frames'][group] = value
            validate(f'frames-{group}-{kind}', bad, required_message=f'frames.{group} 必须是非空数组')
    for kind, value in [('missing', None), ('empty', []), ('wrong-type', {})]:
        bad = copy.deepcopy(fixtures)
        if kind == 'missing':
            del bad['definitions']
        else:
            bad['definitions'] = value
        validate('definitions-' + kind, bad, required_message='definitions 必须是非空数组')
    bad = copy.deepcopy(fixtures)
    bad['definitions'].append({'definition': 'GatewayAbortTurnInput', 'data': {}, 'expect': 'typo'})
    validate('definitions-invalid-expect', bad,
             required_message=f'definitions[{len(fixtures["definitions"])}] 必须含 definition/data 和 expect: pass|fail')
    for name in aliases:
        for expect in ('pass', 'fail'):
            bad = copy.deepcopy(fixtures)
            bad['definitions'] = [c for c in bad['definitions']
                                  if not (c['definition'] == name and c['expect'] == expect)]
            validate(f'alias-missing-{expect}-{name}', bad,
                     required_message=f'payloadAlias {name} 缺 definition 级 {expect} 用例')
        bad_schema = copy.deepcopy(schema)
        bad_schema['definitions'][name] = {}
        validate('alias-open-' + name, fixtures, schema_data=bad_schema)

    # Run the full Python checker, not only a helper, with an injected ref.
    checker = module_at('prerequisite_schema', scripts / 'check_schema.py')
    for name, ref in [('wrong-prefix', '#/missing/GatewayAbortTurnInput'),
                      ('extra-segment', '#/definitions/missing/GatewayAbortTurnInput'),
                      ('external', 'https://example.invalid/schema#/definitions/GatewayAbortTurnInput'),
                      ('invalid-escape', '#/definitions/~2GatewayAbortTurnInput'),
                      ('definitions-container', '#/definitions'),
                      ('metadata-container', '#/x-pilotdeck'),
                      ('nested-schema', '#/definitions/WsRequestFrame/properties/method'),
                      ('non-schema', '#/x-pilotdeck/protocolVersion')]:
        bad = copy.deepcopy(schema)
        bad['definitions']['A4RefControl'] = {'$ref': ref}
        schema_path = output / (name + '.schema.json')
        schema_path.write_text(json.dumps(bad))
        checker.SCHEMA_PATH = schema_path
        checker.failures.clear()
        log = io.StringIO()
        with redirect_stdout(log):
            code = checker.main()
        (output / ('ref-' + name + '.txt')).write_text(log.getvalue())
        record('ref-' + name, code == 1 and any('全文档无悬空' in f for f in checker.failures),
               f'checker exit {code}; expected A9 FAIL')
    if hasattr(checker, 'valid_local_ref'):
        sample = {'definitions': {'a/b~c': {}, 'boolean': False, 'list': [{}]}}
        record('ref-valid-root', checker.valid_local_ref(sample, '#'), 'root schema is allowed')
        record('ref-valid-escaped', checker.valid_local_ref(sample, '#/definitions/a~1b~0c'), 'RFC 6901 escapes resolve')
        record('ref-valid-percent-escaped', checker.valid_local_ref(sample, '#/definitions/a%7E1b%7E0c'),
               'URI-encoded RFC 6901 escapes resolve')
        record('ref-valid-boolean', checker.valid_local_ref(sample, '#/definitions/boolean'), 'boolean definition is a schema')
        record('ref-invalid-array', not checker.valid_local_ref(sample, '#/definitions/list/0'), 'nested array pointer is out of contract')
        record('ref-invalid-definition-array', not checker.valid_local_ref(sample, '#/definitions/list'), 'definition target must be a schema')

    package = json.loads((scripts / 'package.json').read_text())
    lock = json.loads((scripts / 'package-lock.json').read_text())
    record('ajv-pinned', package['dependencies']['ajv'] == '8.20.0'
           and lock['packages']['']['dependencies']['ajv'] == '8.20.0', 'manifest and lock root must pin ajv')
    ci = (code_root / '.github/workflows/ci.yml').read_text().split('  gateway-schema-baseline:')[0]
    record('ci-check-node', 'actions/setup-node@v4' in ci and ci.index('actions/setup-node@v4') < ci.index('npm ci'),
           'required check job sets Node before npm')
    text = (code_root / 'specs/gateway-parity/normalization.md').read_text()
    record('normalization-contract', all(term in text for term in
           ('含 id/name', '键须唯一', '任意字符串中的已锚定 UUID', '词边界', 'stopReason/usage/turns', 'toolName/content')),
           'named preconditions, arbitrary-string UUID rule and clock combinations documented')

    baseline, _ = checker.resolve_baseline()
    if baseline is None:
        record('exporter-cleanup', False, 'baseline required for success/mismatch cleanup controls')
    else:
        # Copy only our exporter/outputs into an isolated repository. Both old
        # process.exit branches are exercised; leaked dirs vanish with scratch.
        with tempfile.TemporaryDirectory(prefix='pdrs-a4-export-') as scratch:
            scratch = Path(scratch)
            exporter = scratch / 'scripts/gateway-parity/export_schema.mjs'
            exporter.parent.mkdir(parents=True)
            shutil.copyfile(scripts / 'export_schema.mjs', exporter)
            (exporter.parent / 'node_modules').symlink_to(scripts / 'node_modules', target_is_directory=True)
            (scratch / 'schema').mkdir()
            for name in ('gateway-protocol.schema.json', 'gateway-methods.snapshot.json'):
                shutil.copyfile(code_root / 'schema' / name, scratch / 'schema' / name)
            tmpdir = scratch / 'temp'
            tmpdir.mkdir()
            preload = scratch / 'write-failure.cjs'
            preload.write_text("const fs = require('node:fs'); const write = fs.writeFileSync;\n"
                               "fs.writeFileSync = function(p, ...args) {\n"
                               "if (String(p).endsWith('/tsconfig.json')) throw new Error('INJECTED_TSCONFIG_WRITE_FAILURE');\n"
                               "return write.call(this, p, ...args); };\n")
            for name, expected in [('success', 0), ('mismatch', 1), ('write-failure', 1)]:
                if name == 'mismatch':
                    with (scratch / 'schema/gateway-protocol.schema.json').open('a') as file:
                        file.write(' ')
                # Node os.tmpdir() uses TMPDIR on this host (TMP/TEMP on Windows).
                env = dict(os.environ, TMPDIR=str(tmpdir), TMP=str(tmpdir), TEMP=str(tmpdir))
                command = ['node'] + (['--require', str(preload)] if name == 'write-failure' else [])
                proc = subprocess.run(command + [str(exporter), '--check', '--baseline', str(baseline)],
                                      env=env, capture_output=True, text=True, timeout=120)
                leaked = sorted(p.name for p in tmpdir.iterdir())
                (output / ('export-cleanup-' + name + '.txt')).write_text(proc.stdout + proc.stderr)
                fault_hit = name != 'write-failure' or 'INJECTED_TSCONFIG_WRITE_FAILURE' in proc.stderr
                record('export-cleanup-' + name, proc.returncode == expected and not leaked and fault_hit,
                       f'exit {proc.returncode} (expected {expected}), leaked={leaked}; expected no leaks')

    record('prerequisite-input-stability', input_hashes() == prerequisite_hashes, 'independent prerequisite manifest unchanged during controls')
    current_audit_hash = common.digest(audit_path) if audit_path.is_file() else None
    record('audit-input-stability', current_audit_hash == audit_hash,
           'audit artifact unchanged during controls')
    report = {'passed': all(c['passed'] for c in results.values()), 'cases': results,
              'inputSha256': prerequisite_hashes,
              'auditSha256': audit_hash if audit_path.is_file() else None,
              'fixtureCounts': {'positiveFrames': len(fixtures['frames']['positive']),
                                'negativeFrames': len(fixtures['frames']['negative']),
                                'definitions': len(fixtures['definitions']), 'aliases': len(aliases)}}
    (output / 'results.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    return report
