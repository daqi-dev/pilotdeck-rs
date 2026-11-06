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
from negative_process import mutating_sources, new_output_directory, run_owned, wait_for_slot
from negative_source import squeeze, test_body
from negative_suite import Case, classify, passed, publish_summary

ROOT = Path(__file__).resolve().parents[5]
ENTRY = ROOT / 'src/pilotdeck-gateway/src/native_process.rs'
TEST_FILE = ROOT / 'src/pilotdeck-gateway/tests/native_process.rs'
TEST = 'native_entry_process_rejects_inherited_authority'
# Only the three key assertions kill (the marked check after the rejection
# gate; a child that advertises listening reaches it too). Launch, pipe,
# decode and exit-wait failures cannot be caused by displacing the
# validate_environment call and stay unbound (INVALID): a child that says
# neither the rejection nor its own listening advertisement is unjudgeable.
MARKERS = [
    '#1261 native entry rejects inherited PILOTDECK_STORAGE_ROOT',
    '#1261 native entry rejects inherited PILOTDECK_SNAPSHOT_ROOT',
    '#1261 native entry rejects inherited PILOTDECK_MANAGED_CONFIG',
]
# The three marked assertions all format this one template, so a `--check`
# needle has to include the call head and condition to say *which* of them it
# bound (#1401 review P2).
MARKER_TEMPLATE = '#1261 native entry rejects inherited {key}'
# #1401 item 6: the environment() call site has its own bound test. The
# unit tests call options.environment() directly; only the real --native
# child observes the call site, where a displaced call lets the inherited
# PILOTDECK_CONFIG_PATH survive, the child loads the injected invalid
# config, and fails fast with the default-config parse error (the error
# conversion drops the diagnostics that would carry the path, so the
# message text is the whole observation). Early exits without that message
# and deadlines without the advertisement stay unbound (INVALID).
SCRUB_TEST = 'native_entry_scrubs_inherited_process_environment'
SCRUB_MARKER = '#1261 native entry scrubs inherited PILOTDECK_CONFIG_PATH'
ANCHOR = (
    '    let inherited = capture_environment(std::env::vars_os());\n'
    '    validate_environment(&inherited)?;\n'
    '    let env = options.environment(inherited);\n'
    '    let (host, feishu_webhook) = assembly::start_with_webhook(options.clone(), env).await?;\n'
)
mutations = [
    ('run-entry-validate-omitted', ENTRY, ANCHOR,
     '    let inherited = capture_environment(std::env::vars_os());\n'
     '    let env = options.environment(inherited);\n'
     '    let (host, feishu_webhook) = assembly::start_with_webhook(options.clone(), env).await?;\n',
     TEST, MARKERS),
    # The realistic reorder threat: validate only after assembly already
    # consumed the inherited authorities. The rejection message is still
    # printed, so assembly's own writes under home are the kill evidence.
    ('run-entry-validate-reordered', ENTRY, ANCHOR,
     '    let inherited = capture_environment(std::env::vars_os());\n'
     '    let env = options.environment(inherited.clone());\n'
     '    let (host, feishu_webhook) = assembly::start_with_webhook(options.clone(), env).await?;\n'
     '    validate_environment(&inherited)?;\n',
     TEST, MARKERS),
    ('run-entry-environment-unscoured', ENTRY, ANCHOR,
     '    let inherited = capture_environment(std::env::vars_os());\n'
     '    validate_environment(&inherited)?;\n'
     '    let env = inherited;\n'
     '    let host = assembly::start(options.clone(), env).await?;\n',
     SCRUB_TEST, SCRUB_MARKER),
]


def mutate(case, text):
    if text.count(case.old) != 1 or case.old == case.new:
        raise ValueError(f'#1261 stale anchor: {case.mutation}: {text.count(case.old)}')
    return text.replace(case.old, case.new, 1)


def main():
    selected_rows = [Case(*row) for row in mutations]
    if sys.argv[1:] == ['--check']:
        test_source = TEST_FILE.read_text()
        for case in selected_rows:
            mutate(case, case.path.read_text())
        # Whitespace-normalized bodies: the needle below is the whole
        # `negative_assert!` call, which rustfmt may spread over several
        # lines. Literals keep their inner whitespace (negative_source).
        rejects = squeeze(test_body(test_source, TEST))
        scrub = squeeze(test_body(test_source, SCRUB_TEST))
        # The runtime judge matches substituted messages, so --check can only
        # bind the shared template and the key list each test iterates; drift
        # here fails before any cargo run is needed. Each needle is bound to
        # the test that actually observes it (#1401 review P2).
        if MARKER_TEMPLATE not in rejects:
            raise SystemExit(f'#1261 marker template missing from {TEST}')
        for key in ('PILOTDECK_STORAGE_ROOT', 'PILOTDECK_SNAPSHOT_ROOT',
                    'PILOTDECK_MANAGED_CONFIG'):
            if f'"{key}"' not in rejects:
                raise SystemExit(f'#1261 key missing from {TEST}: {key}')
        # #1401 item 4: the bound test's two observations beyond the
        # rejection itself -- assembly's leftovers under home and the
        # listening advertisement -- must stay in the source; a drift here
        # would silently unbind the reorder and omitted mutants.
        # Each kill binds the whole assertion call -- `negative_assert!`, the
        # condition with its polarity, and the message -- because the same
        # template is repeated by all three assertions: a needle that only
        # holds a fragment still passes when the call is deleted or rewritten
        # as `let _ = (!advertised, "…");`, which is exactly the drift that
        # leaves the mutant unkilled (#1401 review P2).
        for needle, what in (
                ('observe_child_stderr(stderr)',
                 'listening advertisement observation'),
                (f'negative_assert!(!advertised, "{MARKER_TEMPLATE}"',
                 'listening advertisement assertion'),
                (f'negative_assert!(!status.success(), "{MARKER_TEMPLATE}"',
                 'child exit status assertion'),
                (f'negative_assert!(leftover.is_empty(), "{MARKER_TEMPLATE}"',
                 'home leftover assertion')):
            if needle not in rejects:
                raise SystemExit(f'#1261 missing {what} from {TEST}')
        # #1401 item 6: the scrub mutant's marker and the three observations
        # its verdict rests on must stay in the scrub test, so the runtime
        # kill cannot drift away from the anchor and an unjudgeable branch
        # cannot be mistaken for the pass branch. The unjudgeable panic is
        # bound too: it is what makes an early exit INVALID instead of a
        # spurious pass or a spurious kill (#1401 review).
        for needle, what in (
                ('Failed to parse default config YAML.',
                 'inherited config parse observation'),
                ('observe_child_stderr(stderr)',
                 'scrub listening observation'),
                (f'negative_assert!(!leaked_parse, "{SCRUB_MARKER}"',
                 'scrub inherited config assertion'),
                ('if !advertised { panic!("'
                 '#1261 native entry scrub child neither advertised listening '
                 'nor exited, cannot judge"',
                 'scrub unjudgeable panic')):
            if needle not in scrub:
                raise SystemExit(f'#1261 missing {what} from {SCRUB_TEST}')
        # #1401 review P2: the advertisement shape and the observation edges
        # are pinned by ordinary tests, so nothing else would notice if their
        # case lists were trimmed. The rejects / scrub rows cannot tell a
        # finished line from a half one (neither prints one), so a loosened
        # shape would still pass both mutants: bind the pins themselves.
        shape = squeeze(test_body(test_source, 'native_listening_advertisement_shape'))
        for needle, what in (
                ('must count as an advertisement',
                 'accepted advertisement shape list'),
                ('must not count as an advertisement',
                 'rejected advertisement shape list'),
                (r'"native listening 127.0.0.1:4321 extra\n"',
                 'trailing junk rejection'),
                (r'"native listening [::1]:4321\r\n"',
                 'IPv6 advertisement acceptance')):
            if needle not in shape:
                raise SystemExit(f'#1261 missing {what} from the shape pin')
        observation = squeeze(test_body(
            test_source, 'native_listening_observation_needs_a_finished_line'))
        for needle, what in (
                (r'&b"native listening 127.0.0.1:4321\n"[..]',
                 'finished advertisement case'),
                (r'&b"native listening 127.0.0.1:4321"[..]',
                 'EOF half-line case'),
                ('line cut by the byte bound',
                 'byte bound half-line case'),
                ('CHILD_STDERR_LIMIT * 2',
                 'unterminated line longer than the byte bound'),
                ('"silence:', 'silence case')):
            if needle not in observation:
                raise SystemExit(f'#1261 missing {what} from the observation pin')
        # The newline guard is the P0 fix itself: without it an EOF-truncated
        # half line is accepted again.
        if 'if !line.ends_with(b"\\n")' not in squeeze(test_source):
            raise SystemExit('#1261 advertises lost its newline guard')
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
            with mutating_sources(ROOT, [case.path, Path(__file__).resolve(), TEST_FILE]) as originals:
                original = originals[case.path].decode()
                changed = mutate(case, original)
                if not passed(run(case.mutation + '-positive', args), case.test):
                    raise RuntimeError('positive control failed')
                (out / (case.mutation + '.patch')).write_text(''.join(difflib.unified_diff(
                    original.splitlines(True), changed.splitlines(True), fromfile=str(case.path), tofile=str(case.path))))
                case.path.write_text(changed)
                compiled = run(case.mutation + '-compile', ['--no-run'])
                row['compiled'] = compiled.returncode == 0
                if not row['compiled']:
                    raise RuntimeError('mutant did not compile')
                row['status'], row['reason'] = classify(run(case.mutation + '-negative', args), case)
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
