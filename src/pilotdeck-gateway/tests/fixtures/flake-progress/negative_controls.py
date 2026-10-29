#!/usr/bin/env python3
"""#978 HEAD-only progress/CPU controls; compile every mutant before judging it.

Run --check before committing. After committing, use a new output directory;
--compile-only verifies all mutants, and named rows allow bounded host batches.
The shared matrix runner accounts for every KILLED/SURVIVED/INVALID result,
restores inside mutating_sources, and requires the caller's CARGO_TARGET_DIR.
"""
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
# matrix_main performs mutations inside this guard and cargo through these APIs.
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import matrix_main

ROOT = Path(__file__).resolve().parents[5]
HOST = ROOT / 'src/pilotdeck-gateway/src/host/tests/modules.rs'
TIMING = ROOT / 'src/pilotdeck-gateway/tests/common/timing.rs'
HOST_TEST = 'host::tests::modules::'
HTML_TEST = 'runtime::tools::builtin::web::html_compat::tests::'
MUTATIONS = [
    ('total-execution-budget', HOST,
     'limits.execution_timeout = None;',
     'limits.execution_timeout = Some(Duration::from_nanos(1));',
     [(HOST_TEST + 'stdio_routes_multiple_modules_and_projects_error_details',
       '#978 routing execute')]),
    ('progress-reset-removed', HOST,
     'deadline.as_mut().reset(tokio::time::Instant::now() + idle);',
     'let _ = idle;',
     [(HOST_TEST + 'routing_progress_resets_idle_deadline',
       '#978 progress renews idle deadline')]),
    ('callback-progress-removed', HOST,
     'completed.send_modify(|count| *count += 1);',
     'let _ = completed;',
     [(HOST_TEST + 'routing_progress_resets_idle_deadline',
       '#978 progress renews idle deadline')]),
    ('closed-reporter-premature-timeout', HOST,
     'reporting = false;',
     'return Err("#978 routing made no progress for 10 seconds");',
     [(HOST_TEST + 'routing_progress_closed_reporter_keeps_idle_bound',
       '#978 closed reporter preserves idle budget')]),
    # #978 third batch: cpu_duration moved verbatim from html_compat.rs into
    # the shared tests/common/timing.rs, so the anchors follow the helper. The
    # bound unit test stayed in html_compat.rs (receipt macros are lib-only).
    # The CPU/wall switch is compiled in; this text mutant forces the else
    # branch to wall time as well, i.e. the pre-fix sampler on every path.
    ('cpu-counts-preemption', TIMING,
     '        sample["cpu_seconds"].as_f64()?\n    };',
     '        sample["wall_seconds"].as_f64()?\n    };',
     [(HTML_TEST + 'cpu_sample_requires_completed_work_and_ignores_preemption',
       '#978 CPU sample excludes preemption')]),
    ('cpu-accepts-failed-child', TIMING,
     'if sample["exit"].as_i64()? != 0 {',
     'if false && sample["exit"].as_i64()? != 0 {',
     [(HTML_TEST + 'cpu_sample_requires_completed_work_and_ignores_preemption',
       '#978 CPU sample requires completed work')]),
    ('cpu-accepts-zero-work', TIMING,
     'seconds <= 0.0', 'seconds < 0.0',
     [(HTML_TEST + 'cpu_sample_requires_completed_work_and_ignores_preemption',
       '#978 CPU sample requires completed work')]),
    ('cpu-accepts-no-test', TIMING,
     '!results[0].starts_with("test result: ok. 1 passed; 0 failed;")',
     'false && !results[0].starts_with("test result: ok. 1 passed; 0 failed;")',
     [(HTML_TEST + 'cpu_sample_requires_completed_work_and_ignores_preemption',
       '#978 CPU sample requires completed work')]),
    ('cpu-accepts-duplicate-results', TIMING,
     'results.len() != 1', 'results.is_empty()',
     [(HTML_TEST + 'cpu_sample_requires_completed_work_and_ignores_preemption',
       '#978 CPU sample requires completed work')]),
]


def main():
    return matrix_main(ROOT, __file__, '', MUTATIONS,
                       passed_count=1, child_count=0,
                       paths=[HOST, TIMING], exact=True)


if __name__ == '__main__':
    raise SystemExit(main())
