#!/usr/bin/env python3
"""#562 repair 2: count/zip tripwires and real post-hydrate writer state."""
import json
from pathlib import Path
import shutil
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / '_shared'))
from negative_control import SRC, environment, run, scratch, source_copy

FIXTURES = Path(__file__).resolve().parents[1]
ADAPTER_TEST = 'storage::transcript_adapter_tests::real_ts_scenarios_match_outcomes_calls_and_bytes'
DOMAIN_TEST = 'storage::session_domain::tests::real_ts_domain_logs_and_events_match_bytes'


def test(name, target, env, diagnostic=None, cwd=SRC):
    run(name, ['cargo', 'test', '-p', 'pilotdeck-gateway', '--lib', target,
               '--', '--exact', '--nocapture', '--test-threads=4'], env, cwd, diagnostic)


with scratch('fix2') as root:
    env = environment(root)
    domain = root / 'domain.json'
    transcript = root / 'transcript.json'
    shutil.copyfile(FIXTURES / 'session-domain/oracle.json', domain)
    shutil.copyfile(FIXTURES / 'transcript-adapter/oracle.json', transcript)
    env.update(PDRS_SESSION_DOMAIN_ORACLE=str(domain), PDRS_TRANSCRIPT_ADAPTER_ORACLE=str(transcript))
    test('baseline-adapter', ADAPTER_TEST, env)
    test('baseline-domain', DOMAIN_TEST, env)
    for name, path, fixture, target, count in [
        ('drop-adapter-scenarios', transcript, 'transcript-adapter', ADAPTER_TEST, 397),
        ('drop-domain-scenarios', domain, 'session-domain', DOMAIN_TEST, 304),
    ]:
        oracle = json.loads(path.read_bytes())
        assert len(oracle['scenarios']) == count
        del oracle['scenarios'][-10:]
        path.write_text(json.dumps(oracle) + '\n')
        test(name, target, env, 'oracle scenario count')
        shutil.copyfile(FIXTURES / fixture / 'oracle.json', path)
    oracle = json.loads(domain.read_bytes())
    case = next(case for case in oracle['scenarios'] if case['outcomes'])
    case['outcomes'].pop()
    domain.write_text(json.dumps(oracle) + '\n')
    test('truncate-domain-outcomes', DOMAIN_TEST, env, 'outcome count')
    shutil.copyfile(FIXTURES / 'session-domain/oracle.json', domain)
    copied_src = source_copy(root / 'workspace')
    # Cargo otherwise reuses the same package/artifact identity across copies
    # in one target directory, letting a mutant binary poison a later positive.
    manifest = copied_src / 'pilotdeck-gateway/Cargo.toml'
    manifest_text = manifest.read_text()
    version = 'version = "0.1.0"'
    assert manifest_text.count(version) == 1
    manifest.write_text(manifest_text.replace(version, 'version = "0.1.0-pdrs222-negative"'))
    adapter = copied_src / 'pilotdeck-gateway/src/storage/transcript_adapter.rs'
    original = adapter.read_text()
    start = original.index('    fn restore_entries(')
    end = original.index('    /// JsonlTranscriptWriter.close:', start)
    section = original[start:end]
    block = '''        inner.sequence = state.sequence;
        inner.last_entry_id = state.last_entry_id;
        inner.entry_ids = ids;
        inner.recorder_domain = domain;
'''
    assert section.count(block) == 1
    section = section.replace(block, '').replace('        Ok(())', block + '        Ok(())')
    adapter.write_text(original[:start] + section + original[end:])
    test('install-state-after-hydrate', ADAPTER_TEST, env,
         'restore-message-assistant_message-content-string: steps', cwd=copied_src)
    # Subsequent positive controls read the original source and immutable goldens.
    test('restored-adapter', ADAPTER_TEST, env)
    test('restored-domain', DOMAIN_TEST, env)
    print('PASS: 4 / 4 repair-2 mutants killed', flush=True)
