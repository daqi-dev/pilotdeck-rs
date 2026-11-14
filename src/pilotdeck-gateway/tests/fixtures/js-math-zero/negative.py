#!/usr/bin/env python3
"""#947 negative controls: every JS `Math.max` / `Math.min` mirror must fail
when it is replaced by the platform `f64::min` / `f64::max`.

Two mutant families:
- signed-zero ties: the explicit projection is replaced by an
  order-dependent (input-order-dependent) tie;
- NaN clamps: the shared `js_math` call is reverted to the platform method,
  which swallows the NaN that `Math.min` / `Math.max` propagate (retry
  delays and the background completion-preview bounds).

Platform caveat for the first family, measured on this host with
`rustc 1.99.0`: Rust `f64::max(0.0, -0.0)` / `f64::min(0.0, -0.0)` already
project the JS sign, so a "put back `left.max(right)`" mutant is
behaviorally equivalent here and would SURVIVE.  Those mutants therefore
keep the platform method for ordinary numbers but make the zero tie pick one
input (`left`, or `right` for the per-site max helper) instead of the
JS-projected value, which is the order-dependent behavior the explicit branch
exists to remove; the probe transcript is
`_tmp/20261026-issue-947/fix-r0/f64-probe/probe.out`.

The anchors are one exact source replacement per site (an import, helper line
or the clamped expression itself).  A mutant must first compile
(`cargo test --no-run`), then the named test must fail on a `#947` assertion,
then restoring the bytes must make the same test pass again.

The control does not stop at the first survivor: it records KILLED / SURVIVED
/ INVALID for every selected case, and exits non-zero unless every case is
KILLED.  A stale anchor is a harness defect, so it exits 2 before compiling
instead of being classified as SURVIVED.

#1087 fix-r4 (sibling #3/#4): the verdict rows stay in memory for the whole
guarded window and one final `summary.json` is published by a single rename
only after `mutating_sources` restored and byte-verified every guarded
source (the same skeleton as the five edgeclaw tables). An abort inside the
window — a cancellation, a restore failure, the source guard — voids the
recorded rows, publishes one INVALID summary and exits 3. A red baseline
keeps exit 1 but still publishes its row. The kill judge is the shared
strict #813 criterion (``named_test_killed``): the named test's own FAILED
status line, the bound ``#947`` assertion message inside that test's own
panic payload, and libtest's own ``test result: FAILED.`` summary line.
Every guarded-source write in the window — mutation and restore alike — is
one atomic rename (``write_source_atomic``), never a truncating write.

`mutating_sources` proves every target equals HEAD and restores it on exit.
A source tree with uncommitted #947 changes therefore refuses to run; that is
the intended guard, and the coordinator runs this after the commit.
"""
import json
import os
import re
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_killed
from negative_process import (dead_stream_print, detach_dead_streams,
                              mutating_sources, run_owned, wait_for_slot,
                              write_source_atomic)
from negative_selection import (claim_dir, missing_target, refuse,
                               refuse_unusable_target, select)

root = Path(__file__).resolve().parents[5]
protocol = root / 'src/pilotdeck-protocol/src/js_math.rs'
compaction = root / 'src/pilotdeck-gateway/src/runtime/context/compaction/mod.rs'
token_accounting = root / 'src/pilotdeck-gateway/src/runtime/model/token_accounting.rs'
recall = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/llm/recall.rs'
sdk_session = root / 'src/pilotdeck-gateway/src/runtime/sdk_session/mod.rs'
agent_policy = root / 'src/pilotdeck-agent/src/request_materialization_policy.rs'
http = root / 'src/pilotdeck-gateway/src/runtime/model/http.rs'
# #1111: the request retry policy moved to http/complete.rs; path-only change.
complete = root / 'src/pilotdeck-gateway/src/runtime/model/http/complete.rs'
router_policy = root / 'src/pilotdeck-gateway/src/runtime/model/router/policy.rs'
web_network = root / 'src/pilotdeck-gateway/src/runtime/tools/builtin/web/network.rs'
background = root / 'src/pilotdeck-gateway/src/runtime/background.rs'

# (name, source, before, after, exact test)
cases = [
    # Shared helper: the explicit tie projection becomes order-dependent.
    ('shared-max-left-tie', protocol,
     '''        return if left.is_sign_negative() && right.is_sign_negative() {
            -0.0
        } else {
            0.0
        };''',
     '        return left;',
     'js_math::tests::max_projects_positive_zero_for_every_signed_zero_tie'),
    ('shared-min-left-tie', protocol,
     '''        return if left.is_sign_negative() || right.is_sign_negative() {
            -0.0
        } else {
            0.0
        };''',
     '        return left;',
     'js_math::tests::min_projects_negative_zero_for_every_signed_zero_tie'),
    # Per-site imports: a local order-dependent helper is what a future
    # refactor would reintroduce if the site stopped routing through the
    # shared module.
    ('compaction-left-tie', compaction,
     'pub(crate) use pilotdeck_protocol::js_math::{max as js_max, min as js_min};',
     'pub(crate) fn js_max(left: f64, right: f64) -> f64 {\n'
     '    if left == 0.0 && right == 0.0 { right } else { left.max(right) }\n'
     '}\n'
     'pub(crate) fn js_min(left: f64, right: f64) -> f64 {\n'
     '    if left == 0.0 && right == 0.0 { left } else { left.min(right) }\n'
     '}',
     'runtime::context::compaction::tests::js_math_signed_zero_ties_match_ts'),
    ('token-accounting-left-tie', token_accounting,
     'use pilotdeck_protocol::js_math::{max as js_max, min as js_min};',
     'fn js_max(left: f64, right: f64) -> f64 {\n'
     '    if left == 0.0 && right == 0.0 { right } else { left.max(right) }\n'
     '}\n'
     'fn js_min(left: f64, right: f64) -> f64 {\n'
     '    if left == 0.0 && right == 0.0 { left } else { left.min(right) }\n'
     '}',
     'runtime::model::token_accounting::tests::js_math_signed_zero_ties_match_ts'),
    ('recall-left-tie', recall,
     'use pilotdeck_protocol::js_math::{max as js_max, min as js_min};',
     'fn js_max(left: f64, right: f64) -> f64 {\n'
     '    if left == 0.0 && right == 0.0 { right } else { left.max(right) }\n'
     '}\n'
     'fn js_min(left: f64, right: f64) -> f64 {\n'
     '    if left == 0.0 && right == 0.0 { left } else { left.min(right) }\n'
     '}',
     'runtime::memory::edgeclaw::llm::recall::tests::js_math_signed_zero_ties_match_ts'),
    ('sdk-session-left-tie', sdk_session,
     'use pilotdeck_protocol::js_math::min as math_min;',
     'fn math_min(left: f64, right: f64) -> f64 {\n'
     '    if left == 0.0 && right == 0.0 { left } else { left.min(right) }\n'
     '}',
     'runtime::sdk_session::tests::math_min_signed_zero_ties_match_ts'),
    ('agent-policy-left-tie', agent_policy,
     'use pilotdeck_protocol::js_math::min as js_min;',
     'fn js_min(left: f64, right: f64) -> f64 {\n'
     '    if left == 0.0 && right == 0.0 { left } else { left.min(right) }\n'
     '}',
     'request_materialization_policy::unit_tests::js_min_signed_zero_ties_match_ts'),
    # RetryPolicy.ts:44/50 call Math.min at the HTTP retry call sites; these
    # read the provider config as JsValue, so NaN and -0 must survive too.
    ('http-retry-after-min-nan', http,
     '        return (max_retries, js_math_min(after, max_delay));',
     '        return (max_retries, after.min(max_delay));',
     'runtime::model::http::tests::retry_delay_uses_js_math_min_for_nan_and_signed_zero'),
    ('http-retry-stream-min-nan', http,
     '''    let jitter = retry_number(provider, "jitter").unwrap_or(0.75);
    let deterministic = base * (attempt + 1.0);
    (
        max_retries,
        js_math_min(deterministic + deterministic * jitter * random, max_delay),
    )''',
     '''    let jitter = retry_number(provider, "jitter").unwrap_or(0.75);
    let deterministic = base * (attempt + 1.0);
    (
        max_retries,
        (deterministic + deterministic * jitter * random).min(max_delay),
    )''',
     'runtime::model::http::tests::retry_delay_uses_js_math_min_for_nan_and_signed_zero'),
    ('http-retry-request-min-nan', complete,
     '''    let jitter = retry_number(provider, "jitter").unwrap_or(0.0);
    let deterministic = base * (attempt + 1.0);
    (
        max_retries,
        js_math_min(deterministic + deterministic * jitter * random, max_delay),
    )''',
     '''    let jitter = retry_number(provider, "jitter").unwrap_or(0.0);
    let deterministic = base * (attempt + 1.0);
    (
        max_retries,
        (deterministic + deterministic * jitter * random).min(max_delay),
    )''',
     'runtime::model::http::tests::retry_delay_uses_js_math_min_for_nan_and_signed_zero'),
    # RouterRetryPolicy.ts:39/43/49/70/71 has the same Math.min/Math.max
    # mirrors: config values are read as JS numbers by the baseline.
    ('router-zero-max-attempts-nan', router_policy,
     '        let max_attempts = js_math_max(config.zero_usage_retry.max_attempts, 1.0);',
     '        let max_attempts = config.zero_usage_retry.max_attempts.max(1.0);',
     'runtime::model::router::tests::router_retry_policy_uses_js_math_min_max'),
    ('router-transient-max-clamp', router_policy,
     '    let transient_max_attempts = js_math_max(transient.max_attempts, 1.0);',
     '    let transient_max_attempts = transient.max_attempts;',
     'runtime::model::router::tests::router_retry_policy_uses_js_math_min_max'),
    ('router-zero-attempt-max-nan', router_policy,
     '            delay_ms: 500.0 * js_math_max(attempt, 0.0),',
     '            delay_ms: 500.0 * attempt.max(0.0),',
     'runtime::model::router::tests::router_retry_policy_uses_js_math_min_max'),
    ('router-retry-after-min-nan', router_policy,
     '        Some(value) => js_math_min(value, transient.max_delay_ms),',
     '        Some(value) => value.min(transient.max_delay_ms),',
     'runtime::model::router::tests::router_retry_policy_uses_js_math_min_max'),
    ('router-transient-min-nan', router_policy,
     '            js_math_min(deterministic + jitter, transient.max_delay_ms)',
     '            (deterministic + jitter).min(transient.max_delay_ms)',
     'runtime::model::router::tests::router_retry_policy_uses_js_math_min_max'),
    # network/fetch.ts:213-216 has the same Math.min/Math.max clamp.
    ('web-retry-after-min-nan', web_network,
     '        return js_math_min(cap, ms);',
     '        return cap.min(ms);',
     'runtime::tools::builtin::web::network::retry_delay_tests::retry_delay_uses_js_math_min_max'),
    ('web-retry-exponential-min-nan', web_network,
     '''    let exponential = js_math_min(cap, retry.base_delay_ms * 2f64.powi(attempt as i32));
    let jitter = (random() * js_math_max(1.0, (exponential * 0.25).floor())).floor();
    js_math_min(cap, exponential + jitter)''',
     '''    let exponential = cap.min(retry.base_delay_ms * 2f64.powi(attempt as i32));
    let jitter = (random() * (exponential * 0.25).floor().max(1.0)).floor();
    cap.min(exponential + jitter)''',
     'runtime::tools::builtin::web::network::retry_delay_tests::retry_delay_uses_js_math_min_max'),
    # Single-site revert: the first exponential clamp alone.  The final
    # `js_math_min` still propagates a NaN cap, so this mutant needs the
    # separate `baseDelayMs=NaN, maxDelayMs=5000, random=0` row.
    ('web-retry-exponential-base-nan', web_network,
     '    let exponential = js_math_min(cap, retry.base_delay_ms * 2f64.powi(attempt as i32));',
     '    let exponential = cap.min(retry.base_delay_ms * 2f64.powi(attempt as i32));',
     'runtime::tools::builtin::web::network::retry_delay_tests::retry_delay_uses_js_math_min_max'),
    # BackgroundTaskRuntime.ts:559/561 `Math.max`: a NaN preview size must
    # stay NaN, which makes readSlice treat offset/cap as 0 / ring size.
    ('background-preview-max-nan', background,
     '                    let preview_bytes = js_math_max(self.completion_preview_bytes, 0.0);',
     '                    let preview_bytes = self.completion_preview_bytes.max(0.0);',
     'runtime::background::tests::completion_preview_uses_js_math_max_for_nan'),
]

ASSERTIONS = {
    'http-retry-after-min-nan': '#947 retry_after must propagate NaN',
    'http-retry-stream-min-nan': '#947 retry_decide stream must propagate NaN',
    'http-retry-request-min-nan': '#947 retry_decide_request must propagate NaN',
    'router-zero-max-attempts-nan': '#947 zero_usage Math.max must keep a NaN maxAttempts',
    'router-transient-max-clamp': '#947 transient Math.max must clamp a negative maxAttempts',
    'router-zero-attempt-max-nan': '#947 zero_usage Math.max must propagate a NaN attempt',
    'router-retry-after-min-nan': '#947 retry-after Math.min must propagate a NaN maxDelayMs',
    'router-transient-min-nan': '#947 transient retry delay must propagate NaN',
    'web-retry-after-min-nan': '#947 retry-after Math.min must propagate a NaN cap',
    'web-retry-exponential-min-nan': '#947 exponential Math.min must propagate a NaN cap',
    'web-retry-exponential-base-nan': '#947 exponential base NaN must survive the first Math.min',
    'background-preview-max-nan':
        '#947 completion preview Math.max must propagate NaN previewBytes like TS',
    'shared-max-left-tie': '#947 js_math::max tie',
    'shared-min-left-tie': '#947 js_math::min tie',
    'compaction-left-tie': '#947 compaction js_max tie',
    'token-accounting-left-tie': '#947 token_accounting js_max tie',
    'recall-left-tie': '#947 recall js_max tie',
    'sdk-session-left-tie': '#947 sdk_session math_min tie',
    'agent-policy-left-tie': '#947 request_materialization js_min tie',
}

BASELINE = {
    'pilotdeck-protocol': (['-p', 'pilotdeck-protocol', '--lib'], 'js_math'),
    'pilotdeck-gateway': (['-p', 'pilotdeck-gateway', '--lib'], 'signed_zero_ties_match_ts'),
    'pilotdeck-agent': (
        ['-p', 'pilotdeck-agent', '--test', 'request_materialization'],
        'js_min_signed_zero_ties_match_ts',
    ),
    'pilotdeck-gateway-http': (
        ['-p', 'pilotdeck-gateway', '--lib'],
        'retry_delay_uses_js_math_min_for_nan_and_signed_zero',
    ),
    'pilotdeck-gateway-router': (
        ['-p', 'pilotdeck-gateway', '--lib'],
        'router_retry_policy_uses_js_math_min_max',
    ),
    'pilotdeck-gateway-web': (
        ['-p', 'pilotdeck-gateway', '--lib'],
        'retry_delay_uses_js_math_min_max',
    ),
    'pilotdeck-gateway-background': (
        ['-p', 'pilotdeck-gateway', '--lib'],
        'completion_preview_uses_js_math_max_for_nan',
    ),
}


def package_for(path):
    if path == protocol:
        return 'pilotdeck-protocol'
    if path == agent_policy:
        return 'pilotdeck-agent'
    if path == http or path == complete:
        return 'pilotdeck-gateway-http'
    if path == router_policy:
        return 'pilotdeck-gateway-router'
    if path == web_network:
        return 'pilotdeck-gateway-web'
    if path == background:
        return 'pilotdeck-gateway-background'
    return 'pilotdeck-gateway'


def cargo(extra):
    return ['cargo', 'test', '--offline', '--manifest-path', str(root / 'src/Cargo.toml'), *extra]


def classify(log, returncode, expected, test):
    """Three-way outcome for one mutant run.

    Every KILLED/SURVIVED verdict must prove that exactly one test actually
    ran. A renamed or filtered-out test prints ``running 0 tests`` and exits 0;
    without this guard it would be reported as SURVIVED (a false negative).
    #1087 fix-r4 (sibling #3): a KILLED verdict additionally requires the
    shared strict #813 judge — the named test's own FAILED status line, the
    bound ``#947 ...`` assertion message inside that test's own panic
    payload, and libtest's own ``test result: FAILED.`` summary line. The old
    shape accepted any failure with any ``panicked at`` — including an
    unrelated panic in the same binary, captured stdout, and an aborted run
    that never reached the summary — the false-kill shapes ``--self-test``
    and the twin controls in test_negative.py pin.
    """
    if returncode is None:
        return 'INVALID'
    if 'could not compile' in log:
        return 'INVALID'
    ran = re.search(r'^running (\d+) tests?$', log, re.MULTILINE)
    if ran is None or int(ran.group(1)) != 1:
        return 'INVALID'
    if returncode != 0:
        # #1087 fix-r5 (simplify #2): libtest's own ``test result: FAILED.``
        # summary requirement moved into the shared named_test_killed default
        # (require_summary=True), same verdict, one spelling.
        if named_test_killed(log, test, expected):
            return 'KILLED'
        return 'INVALID'
    if returncode == 0 and 'test result: ok.' in log:
        return 'SURVIVED'
    return 'INVALID'


def self_test():
    anchor = '#947 retry_after must propagate NaN'
    test = 't_selftest'
    killed_log = (
        'running 1 test\n'
        f'test {test} ... FAILED\n\n'
        'failures:\n\n'
        f"---- {test} stdout ----\n"
        f"thread '{test}' panicked at src/x.rs:1:1:\n"
        f'{anchor}\n'
        'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
        'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'
    )
    # The same FAILED shape on an unrelated unwrap: the anchor is nowhere in
    # the test's own panic payload, so it is not a kill.
    unrelated_log = killed_log.replace(anchor, 'called `Option::unwrap()` on a None value')
    # The anchor only in captured stdout, not in the test's own panic block.
    stdout_log = (
        'running 1 test\n'
        f'test {test} ... FAILED\n\n'
        'failures:\n\n'
        f'---- {test} stdout ----\n'
        f'replay dump for debugging: {anchor}\n'
        f"thread '{test}' panicked at src/x.rs:1:1:\n"
        'called `Option::unwrap()` on a None value\n'
        'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n'
        'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'
    )
    # An aborted run: the FAILED status line and the payload are there, but
    # the process never reached libtest's own summary (the SIGKILL shape).
    aborted_log = killed_log.replace(
        'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n', '')
    samples = [
        ('running 0 tests\n\ntest result: ok. 0 passed; 0 failed;', 0, 'INVALID'),
        ('running 1 test\n\ntest result: ok. 1 passed; 0 failed;', 0, 'SURVIVED'),
        ('running 2 tests\n\ntest result: ok. 2 passed; 0 failed;', 0, 'INVALID'),
        (killed_log, 101, 'KILLED'),
        (unrelated_log, 101, 'INVALID'),
        (stdout_log, 101, 'INVALID'),
        (aborted_log, 101, 'INVALID'),
        (killed_log, 0, 'INVALID'),
        ('error: could not compile `pilotdeck-gateway`', 101, 'INVALID'),
    ]
    for log, returncode, expected in samples:
        actual = classify(log, returncode, anchor, test)
        if actual != expected:
            raise RuntimeError((log, returncode, expected, actual))
    print('classify self-test: PASS')


def check_anchors(texts):
    stale = []
    for name, path, before, _after, _test in cases:
        count = texts[path].count(before)
        if count != 1:
            stale.append((name, path.relative_to(root).as_posix(), count))
    if stale:
        # #1087 fix-r8 (S1-a): the shared guarded print — a dead stderr
        # must not turn this exit-2 refusal into a crash (rc=1) or a
        # shutdown-flush 120; the code is the contract.
        for name, relative, count in stale:
            dead_stream_print(f'STALE ANCHOR {name}: {count} matches in {relative}',
                              stream='stderr')
        dead_stream_print('STALE ANCHORS: negative.py is out of sync with its sources; refusing to run',
                          stream='stderr')
        raise SystemExit(2)
    # #1087 fix-r8 (S1-a): the shared guarded print — the read-only
    # "anchors: ok" preflight keeps exit 0 under a dead stdout.
    dead_stream_print(f'{len(cases)} anchors: ok')


# #1908 (from #1692 r15-4): ONE dispatch rule for every mode. Whole-argv
# equality let `--self-test extra` fall through to the main path, which
# treated `--self-test` as an output directory; an unknown `--flag` did the
# same and ran the whole table green. Every mode now takes an exact argument
# list, and only a real OUT_DIR reaches the main path.
USAGE = 'usage: negative.py OUT_DIR [NAME ...] | negative.py --check | negative.py --self-test'
mode, arguments = (sys.argv[1], sys.argv[2:]) if len(sys.argv) > 1 else (None, [])
if mode == '--check':
    if arguments:
        raise SystemExit(f'--check takes no arguments: {arguments!r}\n{USAGE}')
    check_anchors({path: path.read_text() for _name, path, _b, _a, _t in cases})
    raise SystemExit(0)
if mode == '--self-test':
    if arguments:
        raise SystemExit(f'--self-test takes no arguments: {arguments!r}\n{USAGE}')
    self_test()
    raise SystemExit(0)
# Explicit exits, not asserts: `python3 -O` strips asserts and a bare
# IndexError traceback is not a usage diagnostic. Zero arguments and an
# unknown `-*` first argument are both usage errors (non-zero exit).
if mode is None:
    raise SystemExit(USAGE)
if mode.startswith('-'):
    raise SystemExit(f'unknown mode: {mode}\n{USAGE}')
# Judgment checks are explicit exits, not asserts: python3 -O strips asserts
# (same hardening as the mode dispatch above), so a misspelled control name
# must not silently run zero mutants and exit 0.
target = os.environ.get('CARGO_TARGET_DIR')
if missing_target():
    refuse('set CARGO_TARGET_DIR to the caller-owned build tree before running')
names = [case[0] for case in cases]
if len(names) != len(set(names)):
    refuse('duplicate control name')
try:
    run_rows = select(cases, arguments, always=())
except ValueError as error:
    refuse(str(error))
if not run_rows:
    refuse('no controls selected')

refuse_unusable_target()
out = claim_dir(mode)
env = dict(os.environ)
env['CARGO_TARGET_DIR'] = target
# #1908 r6 (DS r5 四角色同撞 P1): paths must keep the FULL frozen table's
# file set, not run_rows' subset. check_anchors() below iterates the whole
# frozen `cases` table, so a subset run would KeyError on the first
# unselected case's file (a traceback exit 1 that reads like "ran, not
# green", with an empty claimed evidence dir). Snapshot set = anchor-check
# set = the full table, same domain contract as edgeclaw-retriever:261;
# only the mutation loop below runs the selected rows.
paths = list(dict.fromkeys(case[1] for case in cases))
results = []
# #1087 fix-r4 (sibling #3): bound before the guarded window so the
# cancellation handler can always publish, even when the abort happened
# before the baseline run recorded anything.
baseline = {'name': 'baseline', 'outcome': 'INVALID', 'returncode': None}


def write_summary(current_baseline):
    """Publish the one final summary atomically (#1087 fix-r4, sibling #3).

    Called exactly once per run, after ``mutating_sources`` restored and
    byte-verified every guarded source — or from the cancellation handler,
    after the same guard unwound. The old per-row truncating ``write_text``
    left a partial all-KILLED summary on disk when a cancellation killed the
    run mid-suite, and an interrupt inside the write could leave an empty or
    half-written JSON document.
    """
    tmp = out / 'summary.json.tmp'
    tmp.write_text(json.dumps(
        {'baseline': current_baseline, 'cases': results}, indent=2) + '\n')
    tmp.replace(out / 'summary.json')


def _void_pending(error):
    """Void the recorded rows and publish one INVALID summary (sibling #3).

    A cancellation or crash inside the guarded window is not a verdict: the
    rows recorded so far are voided, the remaining selected cases are
    recorded INVALID, and the single summary is published after the guard
    restored the tree. The process then exits 3 — deliberately different
    from 2 ("never started", the stale-anchor refusal below) so a reader can
    tell a cancelled run from one that never ran.
    """
    reason = f'runner: {type(error).__name__}: {error}'
    for row in results:
        row['status'] = 'INVALID'
        row['reason'] = (f"{row['reason']}; voided: {reason}"
                         if row['reason'] else f"voided: {reason}")
    for name, path, _before, _after, test in run_rows[len(results):]:
        results.append({'name': name, 'source': path.relative_to(root).as_posix(),
                        'test': test, 'status': 'INVALID', 'reason': reason})
    write_summary(baseline)
    # #1087 fix-r8 (S1-a): the shared guarded print — the summary is
    # already published, "aborted = 3" is the only contract left
    # to keep; a dead stderr must not replace the exit code.
    dead_stream_print(f'voided {len(results)} row(s): {reason}', stream='stderr')
    if isinstance(error, (OSError, ValueError)):
        # #1087 fix-r5 (concurrency #2): a stream-failure abort detaches the
        # dead fds first, so the interpreter-shutdown flush cannot rewrite
        # the exit code below to 120 ("Exception ignored in:").
        detach_dead_streams()
    if isinstance(error, SystemExit):
        # Normalize: 128+signum and any other raw code must not decide how a
        # coordinator reads this run (a 256/-256 code truncates to a zero
        # process status on POSIX).
        raise SystemExit(3) from error
    if not isinstance(error, Exception):
        raise
    raise SystemExit(3) from error


def run(label, extra, timeout=600):
    try:
        slots = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600)
        (out / (label + '.slots.log')).write_text(slots.stdout + slots.stderr)
        result = run_owned(cargo(extra), cwd=root, env=env, timeout=timeout)
    except Exception as error:  # noqa: BLE001 - one cell must not hide the rest
        (out / (label + '.log')).write_text(f'{type(error).__name__}: {error}\n')
        return None, ''
    log = result.stdout + result.stderr
    (out / (label + '.log')).write_text(log)
    return result, log


try:
    with mutating_sources(root, paths) as originals:
        # Stale anchors are a harness defect: fail before any compile.
        check_anchors({path: data.decode() for path, data in originals.items()})
        baseline_ok = False
        try:
            # Positive baseline for every affected target, once each.
            baseline_ok = True
            for package, (extra, filter) in BASELINE.items():
                if not any(package_for(case[1]) == package for case in run_rows):
                    continue
                result, log = run('baseline-' + package, [*extra, filter])
                ok = (result is not None and 'test result: ok.' in log
                      and 'running 0 tests' not in log)
                if not ok:
                    baseline_ok = False
                    baseline = {'name': 'baseline', 'outcome': 'FAIL',
                                'returncode': None if result is None else result.returncode}
                    print(f'baseline {package}: FAIL', flush=True)
                    break
                baseline = {'name': 'baseline', 'outcome': 'PASS',
                            'returncode': None if result is None else result.returncode}
                print(f'baseline {package}: PASS', flush=True)
            if not baseline_ok:
                print('baseline failed; mutants not run', flush=True)
                raise SystemExit(1)

            for name, path, before, after, test in run_rows:
                package = package_for(path)
                extra, _filter = BASELINE[package]
                original = originals[path].decode()
                status, reason = 'INVALID', 'not run'
                try:
                    # Apply exactly one anchor replacement.
                    changed = original.replace(before, after, 1)
                    if changed == original:
                        raise RuntimeError('anchor replacement did not change the source')
                    # #1087 fix-r4 (sibling #4): one rename, never a
                    # truncating in-place write; the shared publish helper
                    # (#1903) keeps a SIGKILL mid-write from leaving a source
                    # the next preflight would refuse as "differs from HEAD".
                    # #1087 fix-r5 (boundary #5): no explicit `mode=` — the
                    # helper's own fallback stat covers a target the mutant
                    # deleted mid-window, and a bare stat here would raise
                    # FileNotFoundError and bypass DEFAULT_SOURCE_MODE.
                    write_source_atomic(root, path, changed.encode())

                    # Requirement: every mutant must compile before its test verdict.
                    compiled, compile_log = run(name + '-compile', [*extra, '--no-run'])
                    if compiled is None or compiled.returncode != 0:
                        reason = 'mutant did not compile; never a kill'
                    else:
                        # No `--nocapture`: it splices the test's stdout into the
                        # `test <name> ... FAILED` status line that classify()
                        # matches exactly (#1053).
                        result, log = run(name + '-negative',
                                          [*extra, test, '--', '--exact'])
                        status = classify(log, None if result is None else result.returncode,
                                          ASSERTIONS[name], test)
                        reason = 'named assertion failed' if status == 'KILLED' else 'classified'
                except Exception as error:  # noqa: BLE001
                    status, reason = 'INVALID', f'{type(error).__name__}: {error}'
                finally:
                    # Same #1087 fix-r4 window: the in-loop restore is also one
                    # rename, never a truncating write_bytes; the guard's own
                    # exit verification covers the bytes. No `mode=` (fix-r5,
                    # boundary #5): same helper fallback as the mutation write.
                    write_source_atomic(root, path, originals[path])

                # Restored bytes must pass the same named test.
                if status == 'KILLED':
                    result, log = run(name + '-restored', [*extra, test, '--', '--exact'])
                    if (result is None or result.returncode != 0
                            or f'test {test} ... ok' not in log
                            or 'running 0 tests' in log):
                        status, reason = 'INVALID', 'restored source did not pass the bound test'
                results.append({'name': name, 'source': path.relative_to(root).as_posix(),
                                'test': test, 'status': status, 'reason': reason})
                print(f'{name}: {status}: {reason}', flush=True)
        finally:
            # Same #1087 fix-r4 window: the final restore is also one rename,
            # never a truncating write_bytes. No `mode=` (fix-r5, boundary #5).
            for guarded in paths:
                write_source_atomic(root, guarded, originals[guarded])
except SystemExit as error:
    if error.code == 1:
        # The deliberate baseline failure: the guard already restored and
        # byte-verified the tree while unwinding the with-block; publish the
        # baseline row and keep the "ran and failed" exit code.
        write_summary(baseline)
        raise
    if error.code == 2:
        # The deliberate stale-anchor refusal: nothing was measured, so
        # nothing is published — "never started" leaves no summary behind.
        raise
    _void_pending(error)
except BaseException as error:  # noqa: BLE001 - void, publish, then re-raise
    _void_pending(error)
# Reaching here means mutating_sources restored every guarded file and
# verified it against the baseline bytes. Only now may a summary exist.
write_summary(baseline)

counts = {state: sum(row['status'] == state for row in results)
          for state in ('KILLED', 'SURVIVED', 'INVALID')}
print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
raise SystemExit(0 if results and all(row['status'] == 'KILLED' for row in results) else 1)
