#!/usr/bin/env python3
# anchor-selfcheck: unsupported imperative replacement chain, no mutation table
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
    assert env.get('PDRS_BASELINE'), 'set PDRS_BASELINE for source hash verification'
    for name, key, diagnostic in [
        ('tamper-domain-source-hash', 'sourceSha256', 'oracle hash for src/session/events/SessionDomainValidation.ts does not match'),
        ('tamper-domain-source-commit', 'sourceCommit', 'oracle baseline commit'),
    ]:
        oracle = json.loads(domain.read_bytes())
        if key == 'sourceSha256':
            source = 'src/session/events/SessionDomainValidation.ts'
            assert oracle[key][source] != '0' * 64
            oracle[key][source] = '0' * 64
        else:
            oracle[key] = '0' * 40
        domain.write_text(json.dumps(oracle) + '\n')
        test(name, DOMAIN_TEST, env, diagnostic)
        shutil.copyfile(FIXTURES / 'session-domain/oracle.json', domain)
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
    # #1019 added begin_close / retire / check_retired between restore_entries
    # and the next doc comment, so the section now holds several `Ok(())`
    # lines; re-install the block before restore_entries' own terminator
    # (after the hydrate loops), never before every `Ok(())` in the section.
    without = section.replace(block, '')
    hydrate = without.index('        // WebHistory reads')
    terminator = without.index('        Ok(())', hydrate)
    assert '\n    }\n' not in without[hydrate:terminator]
    adapter.write_text(original[:start] + without[:terminator] + block
                       + without[terminator:] + original[end:])
    test('install-state-after-hydrate', ADAPTER_TEST, env,
         'restore-message-assistant_message-content-string: steps', cwd=copied_src)
    # Subsequent positive controls read the original source and immutable goldens.
    test('restored-adapter', ADAPTER_TEST, env)
    test('restored-domain', DOMAIN_TEST, env)
    print('PASS: 6 / 6 repair-2 and provenance mutants killed', flush=True)
