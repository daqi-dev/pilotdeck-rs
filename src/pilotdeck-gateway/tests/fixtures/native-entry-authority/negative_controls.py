#!/usr/bin/env python3
"""#1261 guarded native run()-entry call-site mutation; commit first, never bypass HEAD.

NEW_OUTPUT [mutation ...]. --check validates anchors without changing sources.
Every mutant must compile, kill its exact test, and pass after restoration.
The cargo target is the native_process integration test, not --lib: only the
real CARGO_BIN_EXE child exercises run()'s validate_environment call site.
"""
from pathlib import Path
import difflib
import os
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.path.insert(0, str(Path(__file__).resolve().parent))
from negative_process import mutating_sources, new_output_directory, run_owned, wait_for_slot, write_source_atomic
from negative_suite import Case, classify, passed, publish_summary
from native_entry_binding import (
    MARKERS,
    SCRUB_MARKER,
    SCRUB_TEST,
    TEST,
    binding_errors,
)

ROOT = Path(__file__).resolve().parents[5]
ENTRY = ROOT / 'src/pilotdeck-gateway/src/native_process.rs'
OPTIONS = ROOT / 'src/pilotdeck-gateway/src/native_process/options.rs'
TEST_FILE = ROOT / 'src/pilotdeck-gateway/tests/native_process.rs'
# Only the three key assertions kill (the marked check after the rejection
# gate; a child that advertises listening reaches it too). Launch, pipe,
# decode and exit-wait failures cannot be caused by displacing the
# validate_environment call and stay unbound (INVALID): a child that says
# neither the rejection nor its own listening advertisement is unjudgeable.
# The marker constants and every needle shared with `early_exit_invalid.py`
# live in `native_entry_binding` (#1989 item 6): the two copies had drifted.
# #1401 item 6: the environment() call site has its own bound test. The
# unit tests call options.environment() directly; only the real --native
# child observes the call site, where a displaced call lets the inherited
# PILOTDECK_CONFIG_PATH survive, the child loads the injected invalid
# config, and fails fast with the default-config parse error (the error
# conversion drops the diagnostics that would carry the path, so the
# message text is the whole observation). Early exits without that message
# and deadlines without the advertisement stay unbound (INVALID).
ANCHOR = (
    '    let inherited = capture_environment(std::env::vars_os());\n    validate_environment(&inherited)?;\n    let env = options.environment(inherited);\n    server_host::run_embedding(\n        options,\n        env,\n        server_host::Embedding {\n            token,\n            loopback: None,\n            static_assets: None,\n            control: None,\n            restart: None,\n            ready: Arc::new(|address| {\n                Box::pin(async move {\n                    eprintln!("native listening {address}");\n                    Ok(())\n                })\n            }),\n            native_diagnostics: true,\n        },\n    )\n    .await\n    .map(|_| ())'
)
mutations = [
    ('run-entry-validate-omitted', ENTRY, ANCHOR,
     '    let inherited = capture_environment(std::env::vars_os());\n    let env = options.environment(inherited);\n    server_host::run_embedding(\n        options,\n        env,\n        server_host::Embedding {\n            token,\n            loopback: None,\n            static_assets: None,\n            control: None,\n            restart: None,\n            ready: Arc::new(|address| {\n                Box::pin(async move {\n                    eprintln!("native listening {address}");\n                    Ok(())\n                })\n            }),\n            native_diagnostics: true,\n        },\n    )\n    .await\n    .map(|_| ())',
     TEST, MARKERS),
    # The realistic reorder threat: validate only after assembly already
    # consumed the inherited authorities. The rejection message is still
    # printed, so assembly's own writes under home are the kill evidence.
    ('run-entry-validate-reordered', ENTRY, ANCHOR,
     '    let inherited = capture_environment(std::env::vars_os());\n    let env = options.environment(inherited.clone());\n    let result = server_host::run_embedding(\n        options,\n        env,\n        server_host::Embedding {\n            token,\n            loopback: None,\n            static_assets: None,\n            control: None,\n            restart: None,\n            ready: Arc::new(|address| {\n                Box::pin(async move {\n                    eprintln!("native listening {address}");\n                    Ok(())\n                })\n            }),\n            native_diagnostics: true,\n        },\n    )\n    .await\n    .map(|_| ());\n    validate_environment(&inherited)?;\n    result',
     TEST, MARKERS),
    ('run-entry-environment-unscoured', ENTRY, ANCHOR,
     '    let inherited = capture_environment(std::env::vars_os());\n    validate_environment(&inherited)?;\n    let env = inherited;\n    server_host::run_embedding(\n        options,\n        env,\n        server_host::Embedding {\n            token,\n            loopback: None,\n            static_assets: None,\n            control: None,\n            restart: None,\n            ready: Arc::new(|address| {\n                Box::pin(async move {\n                    eprintln!("native listening {address}");\n                    Ok(())\n                })\n            }),\n            native_diagnostics: true,\n        },\n    )\n    .await\n    .map(|_| ())',
     SCRUB_TEST, SCRUB_MARKER),
]


def mutate(case, text):
    if text.count(case.old) != 1 or case.old == case.new:
        raise ValueError(f'#1261 stale anchor: {case.mutation}: {text.count(case.old)}')
    return text.replace(case.old, case.new, 1)


def check_errors(test_source):
    """The `--check` verdict, returned instead of raised (#1989 r3 review).

    Both runners must *consume* the shared table, not merely import it: the
    drift controls call this with an in-memory mutated source and require the
    miss, which a re-forked private table could not produce.
    """
    for case in (Case(*row) for row in mutations):
        mutate(case, case.path.read_text())
    # Every binding shared with `early_exit_invalid.py` lives in
    # `native_entry_binding` (#1989 item 6); each needle is the whole
    # assertion call after whitespace folding, bound to the test that
    # observes it, so a fragment or a rewritten call cannot satisfy it.
    return binding_errors(test_source)


def main():
    selected_rows = [Case(*row) for row in mutations]
    if sys.argv[1:] == ['--check']:
        errors = check_errors(TEST_FILE.read_text())
        if errors:
            raise SystemExit('#1261 ' + '; '.join(errors))
        print(f'#1261 {len(selected_rows)} mutation anchors OK')
        return 0
    if len(sys.argv) < 2:
        raise SystemExit('usage: negative_controls.py NEW_OUTPUT [mutation ...]')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    selected = sys.argv[2:]
    if set(selected) - {case.mutation for case in selected_rows}:
        raise SystemExit('unknown mutation name')
    if selected:
        selected_rows = [case for case in selected_rows if case.mutation in selected]
    out = new_output_directory(Path(sys.argv[1]).resolve())
    env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only', CARGO_PROFILE_TEST_DEBUG='line-tables-only', LANG='en_US.UTF-8')
    env.pop('LC_ALL', None)
    env.pop('LC_MESSAGES', None)
    command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml', '-p', 'pilotdeck-gateway',
               '--test', 'native_process']
    results = []

    def run(label, args):
        slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=900)
        (out / (label + '.slots.log')).write_text(slot.stdout + slot.stderr)
        result = run_owned(command + args, cwd=ROOT, env=env, timeout=900)
        (out / (label + '.log')).write_text(result.stdout + result.stderr)
        return result

    for case in selected_rows:
        row = dict(mutation=case.mutation, test=case.test, assertion=case.assertion,
                   status='INVALID', reason='', compiled=False)
        args = [case.test, '--', '--exact']
        try:
            # Protect the product call site, the judge and the bound test.
            with mutating_sources(ROOT, [case.path, OPTIONS, Path(__file__).resolve(), TEST_FILE]) as originals:
                original = originals[case.path].decode()
                changed = mutate(case, original)
                if not passed(run(case.mutation + '-positive', args), case.test):
                    raise RuntimeError('positive control failed')
                (out / (case.mutation + '.patch')).write_text(''.join(difflib.unified_diff(
                    original.splitlines(True), changed.splitlines(True), fromfile=str(case.path), tofile=str(case.path))))
                write_source_atomic(ROOT, case.path, changed.encode())
                compiled = run(case.mutation + '-compile', ['--no-run'])
                row['compiled'] = compiled.returncode == 0
                if not row['compiled']:
                    raise RuntimeError('mutant did not compile')
                row['status'], row['reason'] = classify(run(case.mutation + '-negative', args), case)
                # Restore and re-run inside the source guard: the restored
                # control compiles the tree, so it must never race another
                # runner that takes the same .negative-source.lock (#1440).
                # #1908: one atomic rename; a truncating write_bytes here
                # has the same SIGKILL half-write window as the mutation
                # write itself.
                write_source_atomic(ROOT, case.path, originals[case.path])
                if not passed(run(case.mutation + '-restored', args), case.test):
                    raise RuntimeError('restored positive control failed')
        except Exception as error:
            row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
        results.append(row)
        print(f"{case.mutation}: {row['status']}: {row['reason']}", flush=True)
    publish_summary(out, results)
    return int(not results or any(row['status'] != 'KILLED' for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
