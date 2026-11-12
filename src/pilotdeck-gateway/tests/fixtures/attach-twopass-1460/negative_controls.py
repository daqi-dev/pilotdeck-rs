#!/usr/bin/env python3
"""#1460/#1461: every two-pass and lone-surrogate mutation.

Usage: negative_controls.py --check
       negative_controls.py [--compile-only] NEW_OUTPUT_DIR [mutation ...]
Every mutation must compile, fail at its bound nonce receipt, and pass again
after restoration. Source bytes must equal HEAD; no alternate baseline exists.
The two #1460 registration rows pin the split itself — the collection pass
must still register a direct-content attachment (dropping the registration
gate, or un-skipping the resolver projection, both turn the golden test
red). The four silent-failure rows pin the other half of the split: a
content-produced attachment keeps every pinned path failure silent like the
audio skip (unresolved — twice: the plain spelling and, since the #1629
softening moved the ELOOP canonicalize failure into the same arm, that
spelling too — the lone-surrogate
diagnostic, and the open failure — the arms with a bound test; the deeper
gates share the same `no_resolver` continue). The seven #1461 rows pin the
two spellings — the outer diagnostic keeps the lone surrogate through the
TS-equivalent resolve while only the fs error spelling is lossy, the route
words follow the raw spelling rather than the resolved tail, the
diagnostics note carries the raw UTF-16 (not a replacement character) into
the block text, the resolve base is the process cwd (a project-root
base breaks the outer prefix), the absolute branch never reads the
cwd at all — a deleted cwd must not eat the diagnostic — and the
relative branch answers from the cached first read after the cwd is
deleted, the way Node's `process.cwd()` does.
"""
import argparse
import hashlib
import json
import os
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[5]
FIXTURES = Path(__file__).resolve().parents[1]
SERVICES = ROOT / 'src/pilotdeck-gateway/src/native_process/services/attachments.rs'
SERVICES_PARENT = ROOT / 'src/pilotdeck-gateway/src/native_process/services.rs'
TESTS = ROOT / 'src/pilotdeck-gateway/src/native_process/services_tests.rs'
sys.path.insert(0, str(FIXTURES))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import ReceiptRun, invalidate_completed, judge, passed

GOLDEN = 'native_process::services_tests::direct_content_with_channel_key_path_is_registered'
SILENT = 'native_process::services_tests::direct_content_with_an_invalid_channel_key_path_stays_silent'
ELOOP = 'native_process::services_tests::direct_content_with_an_eloop_path_stays_silent'
UNOPENABLE = 'native_process::services_tests::direct_content_with_an_unopenable_path_stays_silent'
SURROGATE = 'native_process::services_tests::lone_surrogate_path_becomes_not_found_diagnostic'
FOLDED = 'native_process::services_tests::folded_surrogate_path_routes_the_diagnostic_on_the_raw_spelling'
ABSOLUTE = 'native_process::services_tests::absolute_surrogate_diagnostic_survives_deleted_cwd'
WARM_CACHE = 'native_process::services_tests::relative_surrogate_diagnostic_uses_the_cached_first_cwd'
GOLDEN_BLOCKS = '#1460 direct content with a channelKey path must keep message, image and note blocks'
SILENT_BLOCKS = '#1460 content attachments with invalid paths must keep message and image blocks only'
ELOOP_BLOCKS = '#1460 an ELOOP content attachment must keep message and image blocks only'
UNOPENABLE_QUIET = '#1460 an unopenable path on a content attachment must not diagnose'
SURROGATE_SPELLING = '#1461 the not-found diagnostic must keep the surrogate outer and the lossy inner spelling'
FOLDED_ROUTE = '#1461 the not-found words route on the raw spelling, not the resolved tail'
ABSOLUTE_QUIET = '#1461 an absolute surrogate diagnostic must survive a deleted cwd without reading it'
WARM_CACHE_AFTER = '#1461 after the cwd is deleted the relative surrogate diagnostic still uses the cached first read'

mutations = [
    # #1460: the registration collection is a pass of its own — restoring the
    # pre-split gate (content attachments never registered) drops the note
    # and allowedReadFiles the golden test pins.
    ('produced-direct-registration-dropped', SERVICES,
     '        if metadata.is_file() && entry.channel_key {',
     '        if metadata.is_file() && entry.channel_key && !content_only {',
     GOLDEN, GOLDEN_BLOCKS),
    # #1460: the content `continue` must still skip the resolver projection —
    # clearing it makes the direct-content attachment resolve its path a
    # second time (an extra inline block), so the block count goes red.
    ('produced-direct-resolver-not-skipped', SERVICES,
     '        let content_only = produced_direct[index];',
     '        let content_only = false;',
     GOLDEN, GOLDEN_BLOCKS),
    # #1460: a content-produced attachment keeps an unresolved path silent
    # like the audio skip (composer :181-189 catch {}).
    ('content-failure-diagnosed', SERVICES,
     '''            Ok(LeaseGuard::Unresolved(error)) => {
                if no_resolver {
                    continue;
                }''',
     '''            Ok(LeaseGuard::Unresolved(error)) => {
                if audio {
                    continue;
                }''',
     SILENT, SILENT_BLOCKS),
    # #1460: the P2 internal error is equally swallowed for a content
    # attachment — the turn must not fail over a path the resolver
    # projection never sees. #1629 re-anchor: the path-level canonicalize
    # failure now softens inside lease_guard, so the content ELOOP fixture
    # flows through the same Unresolved arm as every other unresolvable
    # path; the row keeps that fixture separately bound to the arm's
    # content silence (restoring the audio-only skip adds the diagnostics
    # block the block-count assertion catches first, the same
    # first-failure binding the SILENT row above uses).
    ('content-eloop-arm-diagnosed', SERVICES,
     '''            Ok(LeaseGuard::Unresolved(error)) => {
                if no_resolver {
                    continue;
                }''',
     '''            Ok(LeaseGuard::Unresolved(error)) => {
                if audio {
                    continue;
                }''',
     ELOOP, ELOOP_BLOCKS),
    # #1460: the lone-surrogate not-found diagnostic belongs to the resolver
    # projection a content attachment never enters.
    ('content-surrogate-diagnosed', SERVICES,
     '''            if let Some(original) = &entry.invalid_path
                && !content_only
                && !is_audio_shape(&entry.mime, &String::from_utf16_lossy(original.units()))''',
     '''            if let Some(original) = &entry.invalid_path
                && !is_audio_shape(&entry.mime, &String::from_utf16_lossy(original.units()))''',
     SILENT, SILENT_BLOCKS),
    # #1460: the open failure is one of the content-skip's silent arms —
    # restoring the audio-only shape there turns the EACCES open of a
    # chmod-000 lease file into a not-found diagnostic.
    ('content-open-failure-diagnosed', SERVICES,
     '''        let mut file = match File::open(&resolved) {
            Ok(file) => file,
            Err(error) => {
                if no_resolver {
                    continue;
                }''',
     '''        let mut file = match File::open(&resolved) {
            Ok(file) => file,
            Err(error) => {
                if audio {
                    continue;
                }''',
     UNOPENABLE, UNOPENABLE_QUIET),
    # #1461: the outer path keeps the lone surrogate and the resolve prefix —
    # spelling it lossily restores the pre-#1461 single-replacement shape.
    ('surrogate-outer-lossy-restored', SERVICES,
     '    units.extend_from_slice(resolved.units());',
     '    units.extend(lossy.encode_utf16());',
     SURROGATE, SURROGATE_SPELLING),
    # #1461: only the fs error spelling is lossy, and it is the lossy form of
    # the *resolved* path — the resolve prefix belongs inside the ENOENT
    # message too.
    ('surrogate-inner-prefix-dropped', SERVICES,
     "    let lossy = String::from_utf16_lossy(resolved.units());",
     "    let lossy = String::from_utf16_lossy(original.units());",
     SURROGATE, SURROGATE_SPELLING),
    # #1461: the diagnostics note must carry the raw UTF-16 — downgrading the
    # block text to a lossy String erases the surrogate the test pins.
    ('diagnostics-note-lossy-restored', SERVICES,
     '            ("text", JsValue::String(JsString::from_utf16(units))),',
     '            ("text", JsValue::from(String::from_utf16_lossy(&units).as_str())),',
     SURROGATE, SURROGATE_SPELLING),
    # #1461: the route words follow the raw spelling — routing on the
    # resolved form folds `report.pdf/\uD800/..` into a `.pdf` tail and
    # says "PDF attachment not found" where the composer's raw spelling
    # ends in `..` and must stay the file request.
    ('surrogate-route-on-resolved', SERVICES,
     '    let prefix = match attachment_route(entry, &raw_lossy) {',
     '    let prefix = match attachment_route(entry, &lossy) {',
     FOLDED, FOLDED_ROUTE),
    # #1461 r2: the resolve base is the process cwd — restoring the
    # project-root base (the pre-r2 shape) breaks the outer prefix the
    # lone-surrogate test pins.
    ('surrogate-base-project-root-restored', SERVICES,
     '''                } else if let Some(cwd) = cached_process_cwd() {
                    diagnostics.push(surrogate_path_not_found(entry, original, &cwd));
                }''',
     '''                } else {
                    diagnostics.push(surrogate_path_not_found(entry, original, project_dir));
                }''',
     SURROGATE, SURROGATE_SPELLING),
    # #1461 r2 (anthropic sibling P2): the absolute branch is what keeps a
    # deleted cwd from eating the diagnostic — folding it back into the
    # cwd read leaves the isolated worker (its cwd is gone before the
    # compose) silent, and its assertion fires.
    ('surrogate-absolute-cwd-required', SERVICES,
     '''                if original.units().first() == Some(&(b'/' as u16)) {
                    diagnostics.push(surrogate_path_not_found(entry, original, Path::new("/")));
                } else if let Some(cwd) = cached_process_cwd() {''',
     '''                if let Some(cwd) = cached_process_cwd() {''',
     ABSOLUTE, ABSOLUTE_QUIET),
    # #1461 r5 (DS sibling P2): the cache is the point of the relative
    # branch — never writing the first read back makes every call re-read
    # the live cwd, and the deleted-cwd worker goes silent exactly where
    # Node's process.cwd() would still answer from its cache.
    ('surrogate-cwd-cache-bypassed', SERVICES,
     '''    let cwd = std::env::current_dir().ok()?;
    *cached = Some(cwd.clone());
    Some(cwd)''',
     '''    let cwd = std::env::current_dir().ok()?;
    Some(cwd)''',
     WARM_CACHE, WARM_CACHE_AFTER),
]


def test_body(test, texts=None):
    if texts is None:
        texts = {TESTS: TESTS.read_text()}
    # Top-level test functions: a column-zero closing brace ends this
    # function, so a sibling's assertion cannot satisfy it.
    pattern = r'^(?:async )?fn ' + re.escape(test.rsplit('::', 1)[-1]) + r'\([^\n]*\).*?^}'
    bodies = re.findall(pattern, texts[TESTS], re.M | re.S)
    return bodies[0] if len(bodies) == 1 else ''


def check_binding_scope():
    fixture = 'fn target() {\n}\nfn sibling() {\n assert!(false, "only sibling");\n}\n'
    if 'only sibling' in test_body('target', {TESTS: fixture}):
        raise RuntimeError('#1460 binding checker accepted sibling assertion')
    if 'only sibling' not in test_body('sibling', {TESTS: fixture}):
        raise RuntimeError('#1460 binding checker lost its own assertion')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    parser.add_argument('--compile-only', action='store_true')
    parser.add_argument('output', nargs='?')
    parser.add_argument('selected', nargs='*')
    args = parser.parse_args()
    check_binding_scope()
    stale = []
    for name, path, old, new, test, assertion in mutations:
        text = path.read_text()
        body = test_body(test)
        if (text.count(old) != 1 or old == new or
                not body or f'"{assertion}"' not in body):
            stale.append(name)
    if stale:
        raise SystemExit('#1460 stale anchor/test/assertion bindings: ' + ', '.join(stale))
    if args.check:
        print(f'#1460 {len(mutations)} unique anchors and assertion bindings: OK')
        return 0
    selected = [row for row in mutations if not args.selected or row[0] in args.selected]
    if not selected or (args.selected and len(selected) != len(args.selected)):
        raise SystemExit('#1460 unknown or repeated mutation')
    if not args.output or not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('#1460 output and caller CARGO_TARGET_DIR required')
    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ)
    for key in [ReceiptRun.ENV, ReceiptRun.PARENT_ENV, 'PDRS_MUTANT', 'LIVE_E2E_MUTANT']:
        env.pop(key, None)
    guarded = [SERVICES, SERVICES_PARENT, TESTS, Path(__file__).resolve(),
               FIXTURES / 'negative_process.py',
               FIXTURES / 'negative_suite.py',
               ROOT / 'src/pilotdeck-gateway/src/lib.rs']
    results = []
    guarded_sha256 = {}

    def run(label, run_env, test, *, compile_only=False):
        slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=600)
        (output / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
        command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
                   '-p', 'pilotdeck-gateway', '--lib']
        command += ['--no-run'] if compile_only else [test, '--', '--exact', '--include-ignored']
        (output / f'{label}.command.json').write_text(json.dumps(command) + '\n')
        result = run_owned(command, cwd=ROOT, env=run_env, timeout=900)
        (output / f'{label}.log').write_text(result.stdout + result.stderr)
        return result

    try:
        with mutating_sources(ROOT, guarded) as originals:
            guarded_sha256 = {str(path.relative_to(ROOT)): hashlib.sha256(originals[path]).hexdigest() for path in guarded}
            for name, path, old, new, test, assertion in selected:
                item = dict(name=name, test=test, assertion=assertion,
                            status='INVALID', reason='not run', compiled=False)
                try:
                    if not args.compile_only and not passed(run(name + '-positive', env, test), test):
                        raise RuntimeError('positive bound test failed')
                    path.write_text(originals[path].decode().replace(old, new, 1))
                    if run(name + '-compile', env, test, compile_only=True).returncode:
                        raise RuntimeError('mutant did not compile; not a runtime kill')
                    item['compiled'] = True
                    if args.compile_only:
                        item.update(status='COMPILED', reason='mutated source compiled')
                    else:
                        receipt = ReceiptRun(output, name, [(test, assertion)])
                        result = run(name + '-negative', receipt.environment(env), test)
                        killed, reason, hits = judge(result, '', [(test, assertion)], receipt)
                        item.update(status='KILLED' if killed else ('SURVIVED' if passed(result, test) else 'INVALID'),
                                    reason=reason, hits=hits)
                except Exception as error:
                    item.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                finally:
                    path.write_bytes(originals[path])
                try:
                    if not args.compile_only:
                        if not passed(run(name + '-restored', env, test), test):
                            raise RuntimeError('restored bound test failed')
                        item['restoredPass'] = True
                except Exception as error:
                    item.update(status='INVALID', reason=item['reason'] + f'; restored: {error}')
                results.append(item)
                print(f'{name}: {item["status"]}: {item["reason"]}', flush=True)
    except Exception as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate_completed(results, reason)
        for name, _, _, _, test, assertion in selected[len(results):]:
            results.append(dict(name=name, test=test, assertion=assertion,
                                status='INVALID', reason=reason, compiled=False))
    counts = {state: sum(row['status'] == state for row in results)
              for state in ['KILLED', 'SURVIVED', 'INVALID', 'COMPILED']}
    temporary = output / 'summary.json.tmp'
    temporary.write_text(json.dumps(
        {'controls': results, 'counts': counts, 'guardedSha256': guarded_sha256}, indent=2) + '\n')
    temporary.replace(output / 'summary.json')
    print(f'summary: KILLED={counts["KILLED"]}, SURVIVED={counts["SURVIVED"]}, '
          f'INVALID={counts["INVALID"]}', flush=True)
    print(json.dumps({'controls': results, 'counts': counts}), flush=True)
    success = 'COMPILED' if args.compile_only else 'KILLED'
    return int(any(row['status'] != success for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
