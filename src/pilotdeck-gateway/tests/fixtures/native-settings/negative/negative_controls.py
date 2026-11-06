#!/usr/bin/env python3
"""Temporary product mutations of the #1320 native settings wiring; each
must be killed by its named test's own assertion markers, then restore.

Run with no other writers/builds in this worktree. Evidence goes in a new
output folder; original source bytes and hashes are verified after every
mutation. Every mutant targets the U7 settings wiring:

* ``miss-wire-reload-extensions`` — the settings family no longer registers
  ``reload_extensions``; the #1305 registry inventory pins every wired wire
  name. (The #1320 replay and timing tests call
  ``Requests::settings_request`` directly and bypass the family lookup, so
  the registry inventory is the binding that actually watches ``METHODS``.)
* ``no-attach`` — ``services::native_settings`` never calls
  ``bundle.attach()``; the explicit reload still answers, but no store event
  ever drives the settlement, so the reconnect test's wire-order assertion
  (the ``config_changed`` notification between the reload responses) fails.
* ``error-code-rewrite`` — ``settings_error`` collapses every
  ``SettingsError`` code to ``gateway_request_failed``; the replay compares
  the error codes of eight oracle rows cell by cell against the TS golden.
* ``stale-generation`` — ``ResolvedSettingsPort`` resolves from a snapshot
  taken at assembly time (two rows: the struct field and its initialization
  are applied together); the reconnect test's second read must see the new
  generation, not the assembly-time one. (#1748 item 4 moved that adapter
  from ``services.rs`` into ``runtime/settings/gateway.rs`` so production and
  the replay harness share one port, hence the split anchors.)
* ``broadcast-before-dirty`` — the reload settlement swaps
  ``mark_all_dirty`` and ``broadcast``; the dirty-then-broadcast order test
  pins markAllDirty first.
* ``unbind-reload-config`` (#1761) — the production composition drops the
  ``reload_config`` delegate, so the gateway answers
  ``{reloaded: false, reason: "unsupported"}``; the capability guard's
  positive success-shape assertion must catch it. This is the negative half
  of the pair: the assertion and the mutant exist together.
* ``unbind-reload-extensions`` (#1761 review) — the same unbind for
  ``reload_extensions``: the guard's reload loop covers both methods, so each
  iteration needs its own negative control, not just the first.
* ``drop-source-path`` (#1762 review) — ``SourceRecord::value`` stops emitting
  ``path``, so no source names a file any more; the anchor test's
  ``has_file_source`` guard must red instead of comparing ``("file", "",
  hash)`` with ``("file", "", hash)``.
* ``empty-source-hash`` (#1762 review) — the wire projection re-emits an empty
  ``contentHash`` for every source; the strict ``text_of`` must reject the
  empty string instead of letting ``("", "")`` compare equal to ``("", "")``.
  The injection sits in ``config/settings.rs`` rather than the record
  serializer: ``put_truthy`` drops empty strings, so emptying the hash one
  layer up would reach the guard as a *missing* field and trip the older
  string check instead of the non-empty one.
* ``empty-top-level-hash`` (#1869) — the top-level projection empties only
  ``contentHash``; all three sides then compare ``"" == ""``. The #1869
  non-empty assertion must red instead of accepting any string.
* ``hollow-resolve-shape`` (#1869) — the same emptied projection is a hollow
  ``resolve_settings`` success on the wire; the capability guard's success
  shape must red instead of accepting any non-``CAPABILITY_UNAVAILABLE``
  answer.

Every mutant is applied, proven to compile (``cargo test -p
pilotdeck-gateway --lib --no-run``), run against its one named test, and
restored byte-for-byte; a mutant that does not compile is INVALID, never a
kill. The kill marker must appear inside the named test's own panic payload.

The mutated files must match the baseline tree byte-for-byte
(``mutating_sources`` proves it); on the host that baseline is ``HEAD``.
``PDRS_NEGATIVE_BASELINE`` may name another commit object (the sandbox
verification pass pins a ``git stash create`` commit of the reviewed tree,
because the sandbox cannot commit); unset it everywhere else. The ref is
resolved to a full SHA once, up front, and that same immutable SHA is both
recorded (every summary row, ``baseline.json``, the ``.log`` first lines)
and passed to ``mutating_sources`` — the bytes validated are the bytes the
evidence names, even if a movable ref moves mid-run. A non-``HEAD``
baseline is flagged as ``NON-HEAD BASELINE`` / ``NON-HOST EVIDENCE`` on
stdout and in ``baseline.json``, and the script then exits with the
dedicated code ``NON_HEAD_EXIT`` (42) *even when every mutant is killed*:
the host gate only trusts ``HEAD`` evidence, so a non-HEAD run can never
look like a green host run.
``CARGO_TARGET_DIR`` must be supplied by the caller.
"""
import difflib
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from negative_panic import named_test_failed, panic_evidence
from negative_selection import baseline_passed
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[6]
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)
METHODS_SETTINGS = 'src/pilotdeck-gateway/src/native_process/methods/settings.rs'
SERVICES = 'src/pilotdeck-gateway/src/native_process/services.rs'
REFRESH = 'src/pilotdeck-gateway/src/runtime/refresh.rs'
# #1320 fix-r2 F10 moved the six-name single source here; `methods/settings.rs`
# now only re-exports it, so the registry-table mutation must anchor on the
# component's list or the run aborts on an expired anchor.
GATEWAY = 'src/pilotdeck-gateway/src/runtime/settings/gateway.rs'
# #1762 review: the source-projection assertions live in the services suite but
# the fields are built here, so the mutants anchor on the record serializer.
SNAPSHOT = 'src/pilotdeck-gateway/src/config/snapshot.rs'
# ...but the wire projection drops empty strings (`put_truthy`), so an emptied
# hash never reaches the guard as "": it arrives as a missing field and trips
# the older string check instead. The empty-hash mutant must therefore inject
# after the projection, one layer down.
SETTINGS = 'src/pilotdeck-gateway/src/config/settings.rs'
REGISTRY = 'native_process::methods::tests::registry_matches_current_methods'
REPLAY = 'native_process::settings_tests::native_settings_replays_the_real_ts_oracle'
RECONNECT = 'native_process::settings_tests::resolve_settings_generation_across_reconnect'
ORDER = 'native_process::settings_tests::reload_config_dirty_then_broadcast_order'
# #1761: the capability guard's positive reload assertion lives in the
# management suite; the mutant that unbinds the delegate is composed in
# services.rs, so the pair spans the two files on purpose.
CAPABILITIES = 'native_process::management_tests::settings_capabilities_match_served_delegates'
UNBIND_MARKER = ('#1761 settings method reload_config must answer reloaded: true '
                 'from its real delegate')
# #1761 review: the guard's reload loop runs both methods, so the delegate
# half needs its own mutant per method — the same production composition line
# and the same success-shape assertion, one name apart.
UNBIND_EXTENSIONS_MARKER = ('#1761 settings method reload_extensions must answer '
                            'reloaded: true from its real delegate')
# #1762 review: the source-projection guard (`flag_reference` in the non-empty
# loop, strict `text_of`, `has_file_source`) shipped without any mutant, which
# is exactly the "positive assertion with no negative control" pattern this
# batch exists to remove. Both mutants below are killed by that one test.
ANCHORS = 'native_process::services_tests::settings_store_anchors_at_pilot_home_env'
DROP_PATH_MARKER = ('#1762 non-vacuous: resolved must carry at least one source '
                    'with a non-empty path')
EMPTY_HASH_MARKER = '#1762 non-vacuous: source `contentHash` must be non-empty'
# #1869 item 1: the top-level projection has its own non-empty guard; the
# mutant below empties only that field, so the kill must name this marker
# rather than `text_of`'s per-source one.
TOP_HASH_MARKER = ('#1869 non-vacuous: resolved must carry a non-empty '
                   'string contentHash')
# #1869 item 3: the advertised-half shape guard; the emptied projection is a
# hollow success, so this marker names the shape assertion, not the anchors
# one.
SHAPE_MARKER = ('#1869 advertised settings method resolve_settings must answer '
                'its success shape')
# Each row: (name, source path, original anchor text, mutant text, killed test).
# Rows sharing a name are one mutant: every edit is applied together and the
# group gets one compile gate, one kill run and one verdict ('stale-generation'
# owns the struct field and its initialization). The kill marker must appear
# inside that test's own panic payload.
mutations = [
    ('miss-wire-reload-extensions', GATEWAY,
     # #1748 item 2: the member line alone, not the array tail. Anchoring
     # `...\n];` pinned `reload_extensions` as METHODS' last entry, so adding
     # a seventh method would expire the anchor (and, under `-O`, would have
     # silently mutated nothing).
     '    "reload_extensions",\n',
     '', REGISTRY),
    ('no-attach', SERVICES,
     '    bundle.attach();',
     '    // negative control: attach skipped', RECONNECT),
    ('error-code-rewrite', METHODS_SETTINGS,
     '        code: error\n'
     '            .code()\n'
     '            .map(|code| String::from_utf16_lossy(code.units()))\n'
     '            .unwrap_or_else(|| "gateway_request_failed".into()),',
     '        code: "gateway_request_failed".into(),', REPLAY),
    ('stale-generation', GATEWAY,
     'pub(crate) struct ResolvedSettingsPort {\n'
     '    pub(crate) store: ConfigStore,\n'
     '}\n'
     'impl ResolveSettings for ResolvedSettingsPort {\n'
     '    fn resolve(&self) -> Result<JsValue, SettingsError> {\n'
     '        Ok(self.store.resolved_settings())\n'
     '    }\n'
     '}',
     'pub(crate) struct ResolvedSettingsPort {\n'
     '    pub(crate) store: ConfigStore,\n'
     '    snapshot: std::sync::Mutex<JsValue>,\n'
     '}\n'
     'impl ResolveSettings for ResolvedSettingsPort {\n'
     '    fn resolve(&self) -> Result<JsValue, SettingsError> {\n'
     '        Ok(self.snapshot.lock().expect("#1320").clone())\n'
     '    }\n'
     '}', RECONNECT),
    ('stale-generation', SERVICES,
     '        resolve_settings: Some(Arc::new(ResolvedSettingsPort {\n'
     '            store: store.clone(),\n'
     '        })),',
     '        resolve_settings: Some(Arc::new(ResolvedSettingsPort {\n'
     '            snapshot: std::sync::Mutex::new(store.resolved_settings()),\n'
     '            store: store.clone(),\n'
     '        })),', RECONNECT),
    ('unbind-reload-config', SERVICES,
     '        reload_config: Some(bundle.clone()),',
     '        reload_config: None,', CAPABILITIES),
    ('unbind-reload-extensions', SERVICES,
     '        reload_extensions: Some(bundle.clone()),',
     '        reload_extensions: None,', CAPABILITIES),
    ('drop-source-path', SNAPSHOT,
     '        if let Some(path) = &self.path {\n'
     '            o.insert("path".into(), path.as_str().into());\n'
     '        }',
     '        if let Some(path) = &self.path {\n'
     '            let _ = path;\n'
     '        }', ANCHORS),
    ('empty-source-hash', SETTINGS,
     '    for key in ["path", "contentHash", "phase"] {\n'
     '        put_truthy(&mut out, value, key);\n'
     '    }',
     '    for key in ["path", "contentHash", "phase"] {\n'
     '        put_truthy(&mut out, value, key);\n'
     '    }\n'
     '    // negative control: empty hash survives the truthiness filter\n'
     '    out.insert("contentHash".into(), "".into());', ANCHORS),
    # #1869 item 1: the top-level `contentHash` guard used to accept any
    # string, so emptying only that field on all three sides made the anchor
    # comparison "" == "" pass. This mutant leaves the per-source hashes
    # intact, which is what distinguishes it from `empty-source-hash`.
    ('empty-top-level-hash', SETTINGS,
     '    for key in ["schemaVersion", "version", "loadedAt", "contentHash"] {\n'
     '        out.insert(key.into(), field(snapshot, key));\n'
     '    }',
     '    for key in ["schemaVersion", "version", "loadedAt", "contentHash"] {\n'
     '        out.insert(key.into(), field(snapshot, key));\n'
     '    }\n'
     '    // negative control: empty top-level hash survives the projection\n'
     '    out.insert("contentHash".into(), "".into());', ANCHORS),
    # #1869 item 3: the advertised half now requires the full success shape,
    # not just "not CAPABILITY_UNAVAILABLE". The same emptied projection is a
    # hollow success on the wire, so the shape assertion must red on its own
    # test/marker (a separate mutant because a kill is bound to one test).
    ('hollow-resolve-shape', SETTINGS,
     '    for key in ["schemaVersion", "version", "loadedAt", "contentHash"] {\n'
     '        out.insert(key.into(), field(snapshot, key));\n'
     '    }',
     '    for key in ["schemaVersion", "version", "loadedAt", "contentHash"] {\n'
     '        out.insert(key.into(), field(snapshot, key));\n'
     '    }\n'
     '    // negative control: hollow resolve_settings success shape\n'
     '    out.insert("contentHash".into(), "".into());', CAPABILITIES),
    ('broadcast-before-dirty', REFRESH,
     '                if let Some(router) = router {\n'
     '                    router.mark_all_dirty("config_changed");\n'
     '                }\n'
     '                broadcast(&inner, &notice);',
     '                broadcast(&inner, &notice);\n'
     '                if let Some(router) = router {\n'
     '                    router.mark_all_dirty("config_changed");\n'
     '                }', ORDER),
]
MARKERS = {
    'miss-wire-reload-extensions': '#1305 registry contains every wired method',
    'no-attach': ('#1320 reconnect: the mid-flight read answers before '
                  'the reload response'),
    'error-code-rewrite': 'oracle row(s) diverged from the TS golden',
    'stale-generation': '#1320 reconnect: a reconnected client reads the new generation',
    'broadcast-before-dirty': ('#1320 dirty-then-broadcast: markAllDirty must '
                              'run before the broadcast'),
    'unbind-reload-config': UNBIND_MARKER,
    'unbind-reload-extensions': UNBIND_EXTENSIONS_MARKER,
    'drop-source-path': DROP_PATH_MARKER,
    'empty-source-hash': EMPTY_HASH_MARKER,
    'empty-top-level-hash': TOP_HASH_MARKER,
    'hollow-resolve-shape': SHAPE_MARKER,
}
# #1538/#1869 review: a denominator taken from the table in front of it shrinks
# with the table — delete a row and the run still prints "{n}/{n} killed". Pin
# the row count *and* the roster (order and repeats included) as literals, and
# require MARKERS to cover exactly the roster, so a dropped, renamed or
# duplicated mutation is refused before anything runs.
EXPECTED_TOTAL = 12
EXPECTED_NAMES = (
    'miss-wire-reload-extensions',
    'no-attach',
    'error-code-rewrite',
    'stale-generation',
    'stale-generation',
    'unbind-reload-config',
    'unbind-reload-extensions',
    'drop-source-path',
    'empty-source-hash',
    'empty-top-level-hash',
    'hollow-resolve-shape',
    'broadcast-before-dirty',
)
if len(mutations) != EXPECTED_TOTAL:
    sys.exit(f'#1538 fixed mutation denominator: expected {EXPECTED_TOTAL}, '
             f'found {len(mutations)}')
if tuple(mutation[0] for mutation in mutations) != EXPECTED_NAMES:
    sys.exit('#1538 registered mutation roster must equal the frozen list: '
             f'expected={list(EXPECTED_NAMES)}, '
             f'found={[mutation[0] for mutation in mutations]}')
if set(MARKERS) != set(EXPECTED_NAMES):
    sys.exit('#1538 marker table must cover exactly the registered mutations: '
             f'missing={sorted(set(EXPECTED_NAMES) - set(MARKERS))}, '
             f'extra={sorted(set(MARKERS) - set(EXPECTED_NAMES))}')


class StaleAnchor(Exception):
    """An anchor no longer matches exactly once.

    Raised inside the per-mutant ``try`` so an expired anchor is that mutant's
    INVALID verdict, not a dead run: the remaining mutants still get their
    compile gate and kill run. The previous bare ``assert`` was caught by the
    same ``except Exception`` but read like a run-level abort (see #1761).
    """
# A dedicated exit code for "everything was killed, but the baseline was not
# HEAD, so this run is not host evidence" — distinct from 1 (a mutant not
# killed / INVALID) so automation and humans cannot mistake a green non-HEAD
# sandbox run for a host-accepted run.
NON_HEAD_EXIT = 42
selected = sys.argv[2:]
order = []
groups = {}
for mutation in mutations:
    if selected and mutation[0] not in selected:
        continue
    if mutation[0] not in groups:
        groups[mutation[0]] = []
        order.append(mutation[0])
    groups[mutation[0]].append(mutation)
if selected and sorted(selected) != sorted(order):
    unknown = sorted(set(selected) - set(order))
    sys.exit(f'unknown mutation name(s): {", ".join(unknown)}')
env = os.environ.copy()
if not env.get('CARGO_TARGET_DIR'):
    sys.exit('CARGO_TARGET_DIR must be supplied by the caller')
# The actual baseline must be resolved and recorded: every summary row and
# every log carry the full commit SHA and where it came from, so sandbox
# stash-commit evidence cannot be mistaken for host HEAD evidence — the host
# gate only trusts HEAD. The resolved SHA (not the movable ref) is also what
# `mutating_sources` validates against: one resolution, one object for both
# the evidence and the byte checks.
baseline_ref = os.environ.get('PDRS_NEGATIVE_BASELINE') or 'HEAD'
baseline_source = ('PDRS_NEGATIVE_BASELINE'
                   if os.environ.get('PDRS_NEGATIVE_BASELINE') else 'HEAD')
resolved = subprocess.run(
    ['git', '-C', str(root), 'rev-parse', '--verify', baseline_ref + '^{commit}'],
    capture_output=True, check=False)
if resolved.returncode != 0:
    sys.exit(f'cannot resolve baseline {baseline_ref!r}: '
             f'{resolved.stderr.decode(errors="replace").strip()}')
baseline_sha = resolved.stdout.decode().strip()
baseline_line = f'baseline: {baseline_sha} (source: {baseline_source})'
print(baseline_line, flush=True)
if baseline_source != 'HEAD':
    print(f'NON-HEAD BASELINE: {baseline_sha} via PDRS_NEGATIVE_BASELINE; '
          'the host gate only trusts HEAD evidence', flush=True)
summary = []
failures = []
slot_script = root / 'scripts/cargo-slots.sh'
targets = sorted({group[0][4] for group in groups.values()})

with mutating_sources(root, sorted({root / row[1]
                                    for group in groups.values() for row in group}),
                      baseline=baseline_sha) as snapshots:
    def run(tests):
        # libtest takes several full-name filters after `--` (one positional
        # TESTNAME per cargo invocation); `--exact` keeps them literal.
        result = run_owned(['cargo', 'test', '--manifest-path', 'src/Cargo.toml',
                            '-p', 'pilotdeck-gateway', '--lib', '--', *tests,
                            '--exact'], cwd=root, env=env, timeout=1800)
        return result.returncode, result.stdout + result.stderr

    def compiles():
        # The compile gate proves the mutant builds before any verdict is
        # judged: a mutant that does not compile is INVALID, never a kill.
        # It compiles exactly the binary the kill run executes (`--lib`).
        result = run_owned(['cargo', 'test', '--manifest-path', 'src/Cargo.toml',
                            '-p', 'pilotdeck-gateway', '--lib', '--no-run'],
                           cwd=root, env=env, timeout=1800)
        return result.returncode, result.stdout + result.stderr

    # An unmutated baseline must be green first, or every verdict is INVALID.
    wait_for_slot(slot_script)
    code, log = run(targets)
    (out / 'negctl-baseline.log').write_text(baseline_line + '\n\n' + log)
    if code == 0 and all(baseline_passed(log, test) for test in targets):
        print('baseline: PASS', flush=True)
    else:
        print('baseline: INVALID', flush=True)
        sys.exit(1)

    for name in order:
        group = groups[name]
        test = group[0][4]
        marker = MARKERS[name]
        paths = sorted({root / row[1] for row in group})
        before = {path: hashlib.sha256(snapshots[path]).hexdigest()
                  for path in paths}
        try:
            changed = {}
            for _name, relative, old, new, _test in group:
                path = root / relative
                # Rows of one group stack: the later anchor is matched against
                # the text the earlier rows already rewrote.
                content = changed.get(path)
                if content is None:
                    content = snapshots[path].decode()
                # An expired anchor is this mutant's INVALID, never a SURVIVED
                # verdict and never an abort of the whole run.
                count = content.count(old)
                if count != 1:
                    raise StaleAnchor(
                        f'{name}: {relative} anchor matched {count} times (expected 1)')
                changed[path] = content.replace(old, new, 1)
            for path in paths:
                original_text = snapshots[path].decode()
                mutant_text = changed[path]
                relative = path.relative_to(root).as_posix()
                (out / f'{name}.patch').write_text(''.join(difflib.unified_diff(
                    original_text.splitlines(True),
                    mutant_text.splitlines(True),
                    fromfile=relative,
                    tofile=relative)))
            wait_for_slot(slot_script)
            for path in paths:
                path.write_text(changed[path])
            compile_code, compile_log = compiles()
            log = (f'{baseline_line}\n\n=== compile gate '
                   f'(cargo test -p pilotdeck-gateway --lib --no-run) rc={compile_code} ===\n\n'
                   f'{compile_log}\n=== kill run ({test}) ===\n\n')
            if compile_code != 0:
                code = compile_code
                log += 'skipped: the mutant did not compile\n'
                status = 'INVALID'
                failure = 'mutant did not compile'
            else:
                code, kill_log = run([test])
                log += kill_log
                evidence = panic_evidence(kill_log, test)
                killed = (code == 101
                          and named_test_failed(kill_log, test)
                          and marker in evidence
                          and 'could not compile' not in kill_log)
                failure = next((line.strip() for line in kill_log.splitlines()
                                if 'panicked at' in line or 'assertion' in line), '')
                status = 'KILLED' if killed else (
                    'SURVIVED' if code == 0 else 'INVALID')
            (out / f'negctl-{name}.log').write_text(log)
            summary.append(dict(name=name, test=test, status=status,
                                returncode=code, compileReturncode=compile_code,
                                failure=failure, marker=marker,
                                sourceSha256={path.as_posix(): digest
                                              for path, digest in before.items()},
                                baseline=baseline_sha,
                                baselineSource=baseline_source,
                                hostEvidence=baseline_source == 'HEAD'))
            print(f'{name}: {status} rc={code} {failure}', flush=True)
            if status != 'KILLED':
                failures.append(name)
        except Exception as error:
            summary.append(dict(name=name, test=test, status='INVALID',
                                reason=f'{type(error).__name__}: {error}',
                                sourceSha256={path.as_posix(): digest
                                              for path, digest in before.items()},
                                baseline=baseline_sha,
                                baselineSource=baseline_source,
                                hostEvidence=baseline_source == 'HEAD'))
            (out / f'negctl-{name}.log').write_text(
                f'{baseline_line}\n\nINVALID: {type(error).__name__}: {error}\n')
            failures.append(name)
        finally:
            restore_failure = None
            for path in paths:
                path.write_bytes(snapshots[path])
                # #1538 / #1748 item 2: an explicit check, not a bare
                # `assert` — `python3 -O` strips asserts, and an unverified
                # restore is exactly how a mutant's bytes survive into the
                # next row (or into a commit). Record the break *before*
                # raising so the evidence files below still get written;
                # raising first would leave the previous all-KILLED prefix on
                # disk as the last word.
                restored = (hashlib.sha256(path.read_bytes()).hexdigest()
                            == before[path])
                if not restored and restore_failure is None:
                    restore_failure = (
                        f'{path.relative_to(root).as_posix()} was not restored '
                        f'to its baseline bytes')
            if restore_failure is not None:
                failures.append(name)
                summary.append(dict(name=name, test=test, status='INVALID',
                                    reason=f'restore failed: {restore_failure}',
                                    baseline=baseline_sha,
                                    baselineSource=baseline_source,
                                    hostEvidence=baseline_source == 'HEAD'))
            (out / 'summary.json').write_text(
                json.dumps(summary, ensure_ascii=False, indent=2) + '\n')
            (out / 'baseline.json').write_text(json.dumps(
                {'baseline': baseline_sha, 'baselineSource': baseline_source,
                 'nonHeadBaseline': baseline_source != 'HEAD',
                 'hostEvidence': baseline_source == 'HEAD',
                 'mutations': [row['name'] for row in summary]},
                ensure_ascii=False, indent=2) + '\n')
            if restore_failure is not None:
                raise RuntimeError(
                    f'#1748 restore failed: {name}: {restore_failure}')
counts = {state: sum(row['status'] == state for row in summary)
          for state in ('KILLED', 'SURVIVED', 'INVALID')}
print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
if failures:
    print(f'not killed: {", ".join(failures)}', file=sys.stderr)
    sys.exit(1)
print(f'{len(summary)}/{len(groups)} killed; every source restored byte-for-byte '
      f'against baseline {baseline_sha} ({baseline_source})', flush=True)
if baseline_source != 'HEAD':
    # Even a fully-killed run against a non-HEAD baseline is not host
    # evidence: fail closed with the dedicated code and say so on stdout, so
    # a sandbox stash-commit run can never exit like a green host run.
    print(f'NON-HOST EVIDENCE (非宿主证据): baseline {baseline_sha} came from '
          f'{baseline_source}, not HEAD; the host gate only trusts HEAD '
          f'evidence; exit {NON_HEAD_EXIT}', flush=True)
    sys.exit(NON_HEAD_EXIT)
