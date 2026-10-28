#!/usr/bin/env python3
"""Owned, bounded source mutations. Product files must match HEAD on entry.

Each selected original test passes, the mutated test must fail its bound behavior
assertion or explicit impossible-branch guard (not compilation), then the same test passes after guarded restore.
No other writers/cargo are allowed in this worktree during this script.
"""
import re
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared.negative_control import mutation_suite
from negative_panic import panic_evidence, named_test_failed

root = Path(__file__).resolve().parents[5]
network = root / 'src/pilotdeck-gateway/src/runtime/network.rs'
web = root / 'src/pilotdeck-gateway/src/runtime/tools/builtin/web/network.rs'
fetcher = root / 'src/pilotdeck-gateway/src/runtime/tools/builtin/web/fetcher.rs'
cases = [
    ('io-as-microtask', network,
     "            timer: None,\n            microtask: false,",
     "            timer: None,\n            microtask: true,",
     ['background_io_headers_fulfilled_tie_parent_last']),
    ('io-parent-abort', network,
     '        if aborted && parent_at.is_none() {\n            return Poll::Ready(Exchange::Aborted);',
     '        if aborted && parent_at.is_none() {\n            return Poll::Ready(Exchange::Timeout { before_abort: false });',
     [f'fetch_redirect_io_headers_{event}_{order}' for event in ['fulfilled', 'rejected', 'idle'] for order in ['parent_first', 'tie_parent_first', 'tie_parent_last']]),
    ('io-parent-abort-background', network,
     '        if aborted && parent_at.is_none() {\n            return Poll::Ready(Exchange::Aborted);',
     '        if aborted && parent_at.is_none() {\n            return Poll::Ready(Exchange::Timeout { before_abort: false });',
     [f'background_io_headers_{event}_{order}' for event in ['fulfilled', 'rejected', 'idle'] for order in ['parent_first', 'tie_parent_first', 'tie_parent_last']]),
    ('parent-first', network, 'let response_before_parent = parent_at.is_none_or(|at| {',
     'let response_before_parent = true || parent_at.is_none_or(|at| {',
     ['fetch_redirect_headers_rejected_parent_first']),
    ('tie-parent-first', network, '(!parent_first && event_at == parent_at)',
     '(parent_first && event_at == parent_at)',
     ['fetch_redirect_headers_rejected_tie_parent_first']),
    ('tie-parent-last', network, 'event_at < parent_at || (!parent_first && event_at == parent_at)',
     'event_at < parent_at',
     ['fetch_redirect_headers_rejected_tie_parent_last_overdue', 'web_fetch_headers_fulfilled_tie_parent_last_overdue']),
    ('retry-tie', network, 'timer_precedes_parent(next, at, false)',
     'timer_precedes_parent(next, at, true)',
     [f'{path}_{phase}_fulfilled_tie_parent_last_overdue' for path in ['web_fetch', 'fetch_redirect'] for phase in ['retry_status', 'retry_error']]),
    ('retry-message', web, 'Exchange::Aborted => Err(abort_error("Network retry aborted.")),',
     'Exchange::Aborted => Err(abort_error("wrong retry message")),',
     ['fetch_redirect_retry_error_fulfilled_parent_first', 'fetch_redirect_retry_status_fulfilled_parent_first']),
    ('retry-outer-parent-message', web, 'if outer.parent {\n                abort_error("Network retry aborted.")',
     'if outer.parent {\n                abort_error("wrong outer retry message")',
     ['web_tools::web_fetch_hop_timeout_during_retry_uses_retry_abort_message']),
    ('error-name-network', fetcher, 'name: Some("NetworkFetchError"),\n            message: error.message,',
     'name: None,\n            message: error.message,',
     ['web_fetch_headers_fulfilled_parent_first', 'web_fetch_redirect_hop_rejected_continuation_first',
      'web_fetch_retry_status_fulfilled_parent_first']),
    ('error-name-body', fetcher, 'Err(BodyError::Terminated) => return Err(type_error("terminated")),',
     'Err(BodyError::Terminated) => return Err(untyped("terminated")),',
     ['web_fetch_body_rejected_continuation_first', 'web_fetch_body_idle_tie_parent_last']),
    ('error-name-body-aborted', fetcher,
     'fn network_error(message: &str) -> FetchError {\n    FetchError::Other {\n        name: Some("NetworkFetchError"),',
     'fn network_error(message: &str) -> FetchError {\n    FetchError::Other {\n        name: None,',
     ['known_differences_remain_exact_and_ignored']),
    ('error-name-plain', fetcher, 'fn other(message: &str) -> FetchError {\n    FetchError::Other {\n        name: Some("Error"),',
     'fn other(message: &str) -> FetchError {\n    FetchError::Other {\n        name: None,',
     ['web_tools::web_fetch_generic_error_keeps_ts_error_name']),
    ('error-name-invalid-url-redirect', fetcher,
     'let redirect_url = parse_with_base(&location, &url)\n                    .ok_or_else(|| type_error("Invalid URL"))?',
     'let redirect_url = parse_with_base(&location, &url)\n                    .ok_or_else(|| untyped("Invalid URL"))?',
     ['web_tools::web_fetch_invalid_redirect_location_keeps_ts_type_error_name']),
    ('error-name-invalid-url-upgrade', fetcher,
     'upgrade_http_to_https(url).ok_or_else(|| type_error("Invalid URL"))',
     'upgrade_http_to_https(url).ok_or_else(|| untyped("Invalid URL"))',
     ['lib::runtime::tools::builtin::web::fetcher::tests::web_fetch_upgrade_failure_keeps_ts_type_error_name']),
    ('retry-count', web, '&& attempt < retry.max_retries',
     '&& false && attempt < retry.max_retries',
     ['fetch_redirect_retry_status_fulfilled_continuation_first']),
    ('body-content', web, 'out.extend_from_slice(&data);',
     'out.extend_from_slice(&data[..1]);',
     ['web_fetch_body_fulfilled_continuation_first']),
    ('body-parent-name-delivery', network, 'if aborted {\n            return Poll::Ready(Exchange::Aborted);\n        }\n        if !response_first',
     'if false && aborted {\n            return Poll::Ready(Exchange::Aborted);\n        }\n        if !response_first',
     ['fetch_redirect_body_fulfilled_tie_parent_first', 'fetch_redirect_text_body_fulfilled_tie_parent_first']),
    ('matrix-parent-abort', network,
     'if aborted {\n            return Poll::Ready(Exchange::Aborted);\n        }\n        if !response_first',
     'if aborted {\n            return Poll::Ready(Exchange::Timeout { before_abort: false });\n        }\n        if !response_first', ['*']),
    ('matrix-response', network,
     'return Poll::Ready(Exchange::Response(result, aborted));',
     'let _ = result; return Poll::Ready(Exchange::Aborted);', ['*']),
    ('matrix-frame', network,
     'return Poll::Ready(Exchange::Response(result, false));',
     'let _ = result; return Poll::Ready(Exchange::Aborted);', ['*']),
    ('matrix-timeout', network,
     'return Poll::Ready(Exchange::Timeout {\n                before_abort: aborted && timeout_before_parent,\n            });',
     'return Poll::Ready(Exchange::Aborted);', ['*']),
]
background = root / 'src/pilotdeck-gateway/src/runtime/background.rs'
cases += [
    ('background-stop-signal', background,
     '        entry.controller.cancel_synchronously();',
     '        // negative control: stop did not signal the running child',
     ['background_headers_fulfilled_parent_first', 'background_headers_fulfilled_tie_parent_first',
      'background_retry_status_fulfilled_parent_first', 'background_retry_error_fulfilled_tie_parent_first',
      'background_retry_error_fulfilled_parent_first'] +
     [f'background_headers_{event}_{order}' for event in ['rejected', 'idle'] for order in ['parent_first', 'tie_parent_first']] +
     [f'background_redirect_hop_{event}_{order}' for event in ['fulfilled', 'rejected', 'idle'] for order in ['parent_first', 'tie_parent_first']]),
    ('background-stop-sync', background,
     '        entry.controller.cancel_synchronously();',
     '        entry.controller.cancel_at(tokio::time::Instant::now());',
     ['background_retry_status_fulfilled_tie_parent_first', 'background_retry_error_fulfilled_tie_parent_first']),
    ('background-pending', background,
     '                if task.status == TaskStatus::Cancelled {\n                    None',
     '                if false && task.status == TaskStatus::Cancelled {\n                    None',
     ['background_lifecycle_pending_stop_ordered']),
    ('background-wait', background,
     '        let mut done = entry.done.subscribe();\n        let _ = done.wait_for(|done| *done).await;',
     '        let mut done = entry.done.subscribe();\n        let _ = &mut done; // negative control: skipped child completion',
     ['background_lifecycle_running_uncooperative_ordered']),
    ('background-done', background,
     '            entry.done.send_replace(true);',
     '            entry.done.send_replace(false);',
     ['background_lifecycle_pending_stop_ordered']),
    ('background-ended', background,
     '        let clock = catch_port(|| (self.now)());\n        task.ended_at = Some(*clock.as_ref().unwrap_or(&task.started_at));',
     '        let clock = catch_port(|| (self.now)()); // negative control: stop omitted ended_at',
     ['background_lifecycle_running_uncooperative_ordered']),
    ('background-ended-stable', background,
     '                task.ended_at.is_none().then(|| {',
     '                Some(()).map(|()| {',
     ['background_lifecycle_running_uncooperative_ordered']),
    ('background-completed', background,
     '                            task.status = TaskStatus::Completed;',
     '                            task.status = TaskStatus::Running;',
     ['background_lifecycle_settled_fulfilled_ordered']),
    ('background-error', background,
     '                            task.error = Some(message);',
     '                            task.error = Some("wrong child failure".into());',
     ['background_lifecycle_settled_rejected_ordered']),
]
assertion_anchors = {
    'io-parent-abort': 'no fixture timeout',
    'background-pending': 'pending stop must not start child',
    'background-wait': 'stop_and_wait must remain pending until child settles',
    'background-completed': 'registry did not settle after child completion',
    'background-done': 'background done notification deadline',
}
# Full-suite controls still bind a concrete test and its exact oracle anchor;
# an unrelated red test must never satisfy their negative result.
required_failures = {
    'matrix-parent-abort': 'fetch_redirect/headers/fulfilled/parent-first',
    'matrix-response': 'background/headers/fulfilled/continuation-first',
    'matrix-frame': 'background/body/fulfilled/continuation-first',
    'matrix-timeout': 'background/headers/idle/continuation-first',
}

def assert_required_failure(name, log):
    if name in required_failures:
        cell = required_failures[name]
        test = cell.replace('/', '_').replace('-', '_')
        pattern = (r"thread '" + re.escape(test) + r"'[^\n]*\n" +
                   re.escape('assertion `left == right` failed: network timing cell: ' + cell
                             + ' #994 network-timing assertion') + r'\n')
        assert re.search(pattern, log), (name, 'bound test/anchor must fail', test, cell)

# Case-specific assertion anchors: the mutation must be rejected by the
# intended assertion, not by a compile error or an unrelated panic.
failure_anchors = {
    'error-name-network': ('"name": Null', '"name": String("NetworkFetchError")'),
    'error-name-body': ('"name": Null', '"name": String("TypeError")'),
    'error-name-body-aborted': ('known difference changed: web_fetch/body/',
                               '"name": Null', '"name": String("NetworkFetchError")'),
    'error-name-plain': ('name: None', 'name: Some("Error")'),
    'error-name-invalid-url-redirect': ('name: None', 'name: Some("TypeError")'),
    'error-name-invalid-url-upgrade': ('name: None', 'name: Some("TypeError")'),
}


test_source = (root / 'src/pilotdeck-gateway/tests/network_timing.rs').read_text()
expected_passes = len(re.findall(r'^#\[(?:tokio::)?test(?:\([^\n]*\))?\]', test_source, re.M)) - len(re.findall(r'^#\[ignore(?: = [^\n]*)?\]', test_source, re.M))


def selection(test):
    target, _, selected = test.partition('::')
    if target == 'lib':
        cargo_target = ('--lib',)
    else:
        target, selected = (target, selected) if selected else ('network_timing', test)
        cargo_target = ('--test', target)
    arguments = [] if test == '*' else [selected, '--exact']
    return cargo_target, [*arguments, '--show-output', '--test-threads=1']


def passed(result, test):
    count = expected_passes if test == '*' else 1
    selected = selection(test)[1][0]
    return (result.returncode == 0 and f'test result: ok. {count} passed; 0 failed;' in result.stdout
            and (test == '*' or f'test {selected} ... ok' in result.stdout))


def classify(name, result, test):
    if passed(result, test):
        return 'SURVIVED', 'bound test still passed'
    log = result.stdout + result.stderr
    bound = (required_failures[name].replace('/', '_').replace('-', '_')
             if test == '*' else selection(test)[1][0])
    evidence = panic_evidence(log, bound)
    log = evidence
    anchor_ok = all(needle in log for needle in failure_anchors.get(name, ()))
    if name in failure_anchors:
        failure_ok = 'assertion `left == right` failed' in log
    else:
        failure_ok = (
            f'left: Other {{ name: Some("NetworkFetchError"), message: {list(map(ord, "wrong outer retry message"))} }}' in log
            and f'right: Other {{ name: Some("NetworkFetchError"), message: {list(map(ord, "Network retry aborted."))} }}' in log
            and 'assertion `left == right` failed' in log
            if test.startswith('web_tools::')
            else assertion_anchors.get(name, 'assertion `left == right` failed: network timing cell:') in log)
    raw = result.stdout + result.stderr
    runtime = (result.returncode == 101 and 'test result: FAILED.' in raw
               and named_test_failed(raw, bound)
               and (test == '*' or 'test result: FAILED. 0 passed; 1 failed;' in raw))
    if runtime and failure_ok and anchor_ok:
        try:
            assert_required_failure(name, raw)
        except AssertionError as error:
            return 'INVALID', str(error)
        return 'KILLED', 'bound network timing assertion failed'
    return 'INVALID', 'missing bound oracle/typed assertion failure'


def main():
    if len(sys.argv) < 2:
        raise SystemExit('usage: negative_controls.py NEW_OUTPUT_DIR [mutation ...]')
    requested = sys.argv[2:]
    selected = [row for row in cases if not requested or row[0] in requested]
    if requested and (len(selected) != len(requested) or len(set(requested)) != len(requested)):
        raise SystemExit('unknown/duplicate control')
    return mutation_suite('network-timing', selected, sys.argv[1], selection, passed, classify,
                          guarded=[root / 'src/pilotdeck-gateway/tests/network_timing.rs',
                                   root / 'src/pilotdeck-gateway/tests/web_tools.rs',
                                   root / 'src/pilotdeck-gateway/tests/common/network_timing_background.rs',
                                   root / 'src/pilotdeck-gateway/tests/common/network_timing_io.rs',
                                   root / 'src/pilotdeck-gateway/tests/common/network_timing_registry.rs'])


if __name__ == '__main__':
    raise SystemExit(main())
