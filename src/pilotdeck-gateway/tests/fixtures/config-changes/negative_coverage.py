#!/usr/bin/env python3
"""#280/#305/#911: independently corrupt existing oracle cases; never bypass the HEAD guard.

The integration target requires a local all-cases loop because negative_suite's
runner selects --lib. Classification is shared with negative_suite.
"""
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, classify, passed

root = Path(__file__).resolve().parents[5]
oracle = root / 'src/pilotdeck-gateway/tests/fixtures/config-changes/oracle.json'
catalog = root / 'src/pilotdeck-gateway/tests/fixtures/config/catalog.json'
diagnostics = root / 'src/pilotdeck-gateway/tests/fixtures/config/diagnostics.json'
changes_generator = root / 'src/pilotdeck-gateway/tests/fixtures/config-changes/generate.mts'
snapshot_generator = root / 'src/pilotdeck-gateway/tests/fixtures/config-snapshot/generate.mts'
cases = [
    ('skip-relative-normalization', snapshot_generator,
     "value === relativeConfigPath", "value === undefined",
     'config_snapshot_regenerates_identically', '#280 config-snapshot/oracle.json differs'),
    ('duplicate-catalog', catalog, '    {\n      "id": "basic",\n      "yaml": "schemaVersion: 1\\nagent:\\n  model: parity/deterministic\\nmodel:\\n  providers:\\n    parity:\\n      protocol: openai\\n      url: http://127.0.0.1:28794/v1\\n      apiKey: mock-only\\n      models:\\n        deterministic: {}\\n",\n      "params": {},\n      "env": {},\n      "load": {\n        "ok": true,\n        "diagnostics": []\n      },\n      "request": "{\\"type\\":\\"request\\",\\"id\\":\\"basic\\",\\"method\\":\\"model_catalog_list\\",\\"params\\":{}}",\n      "response": "{\\"type\\":\\"response\\",\\"id\\":\\"basic\\",\\"ok\\":true,\\"result\\":{\\"items\\":[{\\"id\\":\\"parity/deterministic\\",\\"provider\\":\\"parity\\",\\"model\\":\\"deterministic\\",\\"displayName\\":\\"deterministic\\",\\"available\\":true,\\"capabilities\\":{}}],\\"defaultSelection\\":{\\"mode\\":\\"model\\",\\"provider\\":\\"parity\\",\\"model\\":\\"deterministic\\"},\\"router\\":{\\"enabled\\":false,\\"autoAvailable\\":false}}}"\n    },\n', '    {\n      "id": "basic",\n      "yaml": "schemaVersion: 1\\nagent:\\n  model: parity/deterministic\\nmodel:\\n  providers:\\n    parity:\\n      protocol: openai\\n      url: http://127.0.0.1:28794/v1\\n      apiKey: mock-only\\n      models:\\n        deterministic: {}\\n",\n      "params": {},\n      "env": {},\n      "load": {\n        "ok": true,\n        "diagnostics": []\n      },\n      "request": "{\\"type\\":\\"request\\",\\"id\\":\\"basic\\",\\"method\\":\\"model_catalog_list\\",\\"params\\":{}}",\n      "response": "{\\"type\\":\\"response\\",\\"id\\":\\"basic\\",\\"ok\\":true,\\"result\\":{\\"items\\":[{\\"id\\":\\"parity/deterministic\\",\\"provider\\":\\"parity\\",\\"model\\":\\"deterministic\\",\\"displayName\\":\\"deterministic\\",\\"available\\":true,\\"capabilities\\":{}}],\\"defaultSelection\\":{\\"mode\\":\\"model\\",\\"provider\\":\\"parity\\",\\"model\\":\\"deterministic\\"},\\"router\\":{\\"enabled\\":false,\\"autoAvailable\\":false}}}"\n    },\n    {\n      "id": "basic",\n      "yaml": "schemaVersion: 1\\nagent:\\n  model: parity/deterministic\\nmodel:\\n  providers:\\n    parity:\\n      protocol: openai\\n      url: http://127.0.0.1:28794/v1\\n      apiKey: mock-only\\n      models:\\n        deterministic: {}\\n",\n      "params": {},\n      "env": {},\n      "load": {\n        "ok": true,\n        "diagnostics": []\n      },\n      "request": "{\\"type\\":\\"request\\",\\"id\\":\\"basic\\",\\"method\\":\\"model_catalog_list\\",\\"params\\":{}}",\n      "response": "{\\"type\\":\\"response\\",\\"id\\":\\"basic\\",\\"ok\\":true,\\"result\\":{\\"items\\":[{\\"id\\":\\"parity/deterministic\\",\\"provider\\":\\"parity\\",\\"model\\":\\"deterministic\\",\\"displayName\\":\\"deterministic\\",\\"available\\":true,\\"capabilities\\":{}}],\\"defaultSelection\\":{\\"mode\\":\\"model\\",\\"provider\\":\\"parity\\",\\"model\\":\\"deterministic\\"},\\"router\\":{\\"enabled\\":false,\\"autoAvailable\\":false}}}"\n    },\n', 'all_successful_config_corpus_cases_have_snapshot_projections', '#305 catalog successful input count'),
    ('duplicate-id-catalog', catalog, '"id": "basic",', '"id": "input-missing",', 'all_successful_config_corpus_cases_have_snapshot_projections', '#305 catalog unique successful input IDs'),
    ('duplicate-diagnostics', diagnostics, '    {\n      "id": "diag-0-legal",\n      "yaml": "{\\"schemaVersion\\":1,\\"agent\\":{\\"model\\":\\"parity/deterministic\\"},\\"model\\":{\\"providers\\":{\\"parity\\":{\\"protocol\\":\\"openai\\",\\"url\\":\\"http://127.0.0.1:28794/v1\\",\\"apiKey\\":\\"mock-only\\",\\"models\\":{\\"deterministic\\":{}}}}}}",\n      "params": {},\n      "env": {},\n      "load": {\n        "ok": true,\n        "diagnostics": []\n      },\n      "request": "{\\"type\\":\\"request\\",\\"id\\":\\"diag-0-legal\\",\\"method\\":\\"model_catalog_list\\",\\"params\\":{}}",\n      "response": "{\\"type\\":\\"response\\",\\"id\\":\\"diag-0-legal\\",\\"ok\\":true,\\"result\\":{\\"items\\":[{\\"id\\":\\"parity/deterministic\\",\\"provider\\":\\"parity\\",\\"model\\":\\"deterministic\\",\\"displayName\\":\\"deterministic\\",\\"available\\":true,\\"capabilities\\":{}}],\\"defaultSelection\\":{\\"mode\\":\\"model\\",\\"provider\\":\\"parity\\",\\"model\\":\\"deterministic\\"},\\"router\\":{\\"enabled\\":false,\\"autoAvailable\\":false}}}"\n    },\n', '    {\n      "id": "diag-0-legal",\n      "yaml": "{\\"schemaVersion\\":1,\\"agent\\":{\\"model\\":\\"parity/deterministic\\"},\\"model\\":{\\"providers\\":{\\"parity\\":{\\"protocol\\":\\"openai\\",\\"url\\":\\"http://127.0.0.1:28794/v1\\",\\"apiKey\\":\\"mock-only\\",\\"models\\":{\\"deterministic\\":{}}}}}}",\n      "params": {},\n      "env": {},\n      "load": {\n        "ok": true,\n        "diagnostics": []\n      },\n      "request": "{\\"type\\":\\"request\\",\\"id\\":\\"diag-0-legal\\",\\"method\\":\\"model_catalog_list\\",\\"params\\":{}}",\n      "response": "{\\"type\\":\\"response\\",\\"id\\":\\"diag-0-legal\\",\\"ok\\":true,\\"result\\":{\\"items\\":[{\\"id\\":\\"parity/deterministic\\",\\"provider\\":\\"parity\\",\\"model\\":\\"deterministic\\",\\"displayName\\":\\"deterministic\\",\\"available\\":true,\\"capabilities\\":{}}],\\"defaultSelection\\":{\\"mode\\":\\"model\\",\\"provider\\":\\"parity\\",\\"model\\":\\"deterministic\\"},\\"router\\":{\\"enabled\\":false,\\"autoAvailable\\":false}}}"\n    },\n    {\n      "id": "diag-0-legal",\n      "yaml": "{\\"schemaVersion\\":1,\\"agent\\":{\\"model\\":\\"parity/deterministic\\"},\\"model\\":{\\"providers\\":{\\"parity\\":{\\"protocol\\":\\"openai\\",\\"url\\":\\"http://127.0.0.1:28794/v1\\",\\"apiKey\\":\\"mock-only\\",\\"models\\":{\\"deterministic\\":{}}}}}}",\n      "params": {},\n      "env": {},\n      "load": {\n        "ok": true,\n        "diagnostics": []\n      },\n      "request": "{\\"type\\":\\"request\\",\\"id\\":\\"diag-0-legal\\",\\"method\\":\\"model_catalog_list\\",\\"params\\":{}}",\n      "response": "{\\"type\\":\\"response\\",\\"id\\":\\"diag-0-legal\\",\\"ok\\":true,\\"result\\":{\\"items\\":[{\\"id\\":\\"parity/deterministic\\",\\"provider\\":\\"parity\\",\\"model\\":\\"deterministic\\",\\"displayName\\":\\"deterministic\\",\\"available\\":true,\\"capabilities\\":{}}],\\"defaultSelection\\":{\\"mode\\":\\"model\\",\\"provider\\":\\"parity\\",\\"model\\":\\"deterministic\\"},\\"router\\":{\\"enabled\\":false,\\"autoAvailable\\":false}}}"\n    },\n', 'all_successful_config_corpus_cases_have_snapshot_projections', '#305 diagnostics successful input count'),
    ('duplicate-id-diagnostics', diagnostics, '"id": "diag-0-legal",', '"id": "diag-5-schemaVersion",', 'all_successful_config_corpus_cases_have_snapshot_projections', '#305 diagnostics unique successful input IDs'),
    ('drop-generated-pair', changes_generator,
     "const expectedNames = JSON.parse(await readFile(new URL('./case-names.json', import.meta.url), 'utf8'));",
     "pairs.pop();\nconst expectedNames = JSON.parse(await readFile(new URL('./case-names.json', import.meta.url), 'utf8'));",
     'config_changes_regenerates_identically', '#911 TS pairs count'),
    ('drop-generated-classify', changes_generator,
     "const expectedNames = JSON.parse(await readFile(new URL('./case-names.json', import.meta.url), 'utf8'));",
     "cases.pop();\nconst expectedNames = JSON.parse(await readFile(new URL('./case-names.json', import.meta.url), 'utf8'));",
     'config_changes_regenerates_identically', '#911 TS classify count'),
    ('drop-pair', oracle, '    {\n      "name": "identical-config",\n      "previous": {\n        "schemaVersion": 1,\n        "version": 1,\n        "config": {\n          "a": 1,\n          "nested": {\n            "b": [\n              1,\n              2\n            ]\n          }\n        }\n      },\n      "next": {\n        "schemaVersion": 1,\n        "version": 1,\n        "config": {\n          "a": 1,\n          "nested": {\n            "b": [\n              1,\n              2\n            ]\n          }\n        }\n      },\n      "changedPaths": [],\n      "changeClasses": [],\n      "runPolicyOnly": false\n    },\n', '', 'real_ts_snapshot_pairs_match_diff_and_classes', '#911 exact pair count'),
    ('rename-pair', oracle, '"name": "identical-config",', '"name": "missing-identical-config",', 'real_ts_snapshot_pairs_match_diff_and_classes', '#911 exact pair names'),
    ('drop-classify', oracle, '    {\n      "name": "empty",\n      "paths": [],\n      "classes": [],\n      "runPolicyOnly": false\n    },\n', '', 'real_ts_direct_classify_cases_match', '#911 exact classify count'),
    ('rename-classify', oracle, '"name": "empty",', '"name": "missing-empty",', 'real_ts_direct_classify_cases_match', '#911 exact classify names'),
    ('rename-catalog', catalog, '"id": "basic",', '"id": "missing-basic",', 'all_successful_config_corpus_cases_have_snapshot_projections', '#305 catalog successful case IDs'),
    ('drop-catalog', catalog, '    {\n      "id": "basic",\n      "yaml": "schemaVersion: 1\\nagent:\\n  model: parity/deterministic\\nmodel:\\n  providers:\\n    parity:\\n      protocol: openai\\n      url: http://127.0.0.1:28794/v1\\n      apiKey: mock-only\\n      models:\\n        deterministic: {}\\n",\n      "params": {},\n      "env": {},\n      "load": {\n        "ok": true,\n        "diagnostics": []\n      },\n      "request": "{\\"type\\":\\"request\\",\\"id\\":\\"basic\\",\\"method\\":\\"model_catalog_list\\",\\"params\\":{}}",\n      "response": "{\\"type\\":\\"response\\",\\"id\\":\\"basic\\",\\"ok\\":true,\\"result\\":{\\"items\\":[{\\"id\\":\\"parity/deterministic\\",\\"provider\\":\\"parity\\",\\"model\\":\\"deterministic\\",\\"displayName\\":\\"deterministic\\",\\"available\\":true,\\"capabilities\\":{}}],\\"defaultSelection\\":{\\"mode\\":\\"model\\",\\"provider\\":\\"parity\\",\\"model\\":\\"deterministic\\"},\\"router\\":{\\"enabled\\":false,\\"autoAvailable\\":false}}}"\n    },\n', '', 'all_successful_config_corpus_cases_have_snapshot_projections', '#305 catalog successful input count'),
    ('rename-diagnostics', diagnostics, '"id": "diag-0-legal",', '"id": "missing-diag-0-legal",', 'all_successful_config_corpus_cases_have_snapshot_projections', '#305 diagnostics successful case IDs'),
    ('drop-diagnostics', diagnostics, '    {\n      "id": "diag-0-legal",\n      "yaml": "{\\"schemaVersion\\":1,\\"agent\\":{\\"model\\":\\"parity/deterministic\\"},\\"model\\":{\\"providers\\":{\\"parity\\":{\\"protocol\\":\\"openai\\",\\"url\\":\\"http://127.0.0.1:28794/v1\\",\\"apiKey\\":\\"mock-only\\",\\"models\\":{\\"deterministic\\":{}}}}}}",\n      "params": {},\n      "env": {},\n      "load": {\n        "ok": true,\n        "diagnostics": []\n      },\n      "request": "{\\"type\\":\\"request\\",\\"id\\":\\"diag-0-legal\\",\\"method\\":\\"model_catalog_list\\",\\"params\\":{}}",\n      "response": "{\\"type\\":\\"response\\",\\"id\\":\\"diag-0-legal\\",\\"ok\\":true,\\"result\\":{\\"items\\":[{\\"id\\":\\"parity/deterministic\\",\\"provider\\":\\"parity\\",\\"model\\":\\"deterministic\\",\\"displayName\\":\\"deterministic\\",\\"available\\":true,\\"capabilities\\":{}}],\\"defaultSelection\\":{\\"mode\\":\\"model\\",\\"provider\\":\\"parity\\",\\"model\\":\\"deterministic\\"},\\"router\\":{\\"enabled\\":false,\\"autoAvailable\\":false}}}"\n    },\n', '', 'all_successful_config_corpus_cases_have_snapshot_projections', '#305 diagnostics successful input count'),
    ('skip-relative-source', snapshot_generator,
     "env.PILOTDECK_CONFIG_PATH = relative(process.cwd(), join(home, 'pilotdeck.yaml'));",
     "// mutant: omit the relative source environment path",
     'config_snapshot_regenerates_identically', '#305 relative source must use a relative environment path'),
    ('skip-alternate-source', snapshot_generator,
     "env.PILOTDECK_CONFIG_PATH = `  ${join(home, 'other.yaml')}  `;",
     "// mutant: omit alternate source environment path",
     'config_snapshot_regenerates_identically', '#280 config-snapshot/oracle.json differs'),
    ('skip-byte-source', snapshot_generator,
     "const bytes = item.sourceKind === 'bytes'",
     "const bytes = item.sourceKind === 'never-bytes'",
     'config_snapshot_regenerates_identically', '#280 config-snapshot/oracle.json differs'),
    ('resample-diagnostics', snapshot_generator,
     'const successful = corpus.cases.filter((item: any) => item.load?.ok);',
     'const successful = corpus.cases.filter((item: any) => item.load?.ok).filter((_: unknown, index: number) => index % 18 === 0);',
     'config_snapshot_regenerates_identically', '#280 config-snapshot/oracle.json differs'),
 ]

def main():
    if not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('#911 CARGO_TARGET_DIR must be supplied by the caller')
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    env = os.environ.copy()
    command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
               '-p', 'pilotdeck-gateway']
    target = 'config_changes'
    selected = cases
    if len(sys.argv) > 2:
        selected = [case for case in cases if case[0] in sys.argv[2:]]
        if len(selected) != len(set(sys.argv[2:])):
            raise SystemExit('#911 unknown/duplicate mutation name')
    results = []

    def run(label, args):
        slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=120)
        (out / (label + '.slots.log')).write_text(slot.stdout + slot.stderr)
        result = run_owned(command + ['--test', target] + args, cwd=root, env=env, timeout=900)
        (out / (label + '.log')).write_text(result.stdout + result.stderr)
        return result

    def record(name, status, reason):
        results.append(dict(mutation=name, status=status, reason=reason))
        print(f'{name}: {status}: {reason}', flush=True)

    try:
        with mutating_sources(root, list(dict.fromkeys(case[1] for case in selected))) as originals:
            for name, path, old, new, test, assertion in selected:
                status, reason = 'INVALID', 'not executed'
                target = 'config_snapshot' if test in {'config_snapshot_regenerates_identically', 'all_successful_config_corpus_cases_have_snapshot_projections'} else 'config_changes'
                args = [test, '--', '--exact', '--include-ignored', '--nocapture']
                try:
                    content = originals[path].decode()
                    if content.count(old) != 1:
                        raise RuntimeError(f'#911 stale anchor {name}: count={content.count(old)}')
                    changed = content.replace(old, new, 1)
                    if path.suffix == '.json':
                        json.loads(changed)  # malformed JSON is a harness error, not a kill
                    if not passed(run(name + '-positive', args), test):
                        raise RuntimeError('#911 positive bound test did not pass')
                    path.write_text(changed)
                    compiled = run(name + '-compile', ['--no-run'])
                    if compiled.returncode:
                        raise RuntimeError('#911 mutant did not compile')
                    result = run(name + '-negative', args)
                    status, reason = classify(result, Case(name, path, old, new, test, assertion))
                except Exception as error:
                    status, reason = 'INVALID', str(error)
                finally:
                    path.write_bytes(originals[path])
                try:
                    if not passed(run(name + '-restored', args), test):
                        raise RuntimeError('#911 restored bound test did not pass')
                except Exception as error:
                    status, reason = 'INVALID', reason + '; ' + str(error)
                record(name, status, reason)
    except Exception as error:
        for row in results:
            row.update(status='INVALID', reason=str(error))
        for name, *_ in selected[len(results):]:
            record(name, 'INVALID', 'source guard: ' + str(error))
    counts = {status: sum(row['status'] == status for row in results)
              for status in ['KILLED', 'SURVIVED', 'INVALID']}
    (out / 'summary.json').write_text(json.dumps(dict(controls=results, counts=counts), indent=2) + '\n')
    print(json.dumps(counts), flush=True)
    return int(not results or any(row['status'] != 'KILLED' for row in results))

if __name__ == '__main__':
    raise SystemExit(main())
