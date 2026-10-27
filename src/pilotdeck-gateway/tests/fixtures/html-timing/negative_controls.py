#!/usr/bin/env python3
"""Temporary source mutations; each must turn a timing assertion red, then restore.

The negative controls for the shared multi-sample timing budgets in
``tests/common/timing.rs`` (issue #802). Each mutation restores a historical
quadratic or constant-factor implementation, or breaks the halved-input
premise; the ratio, the per-sample absolute cap, or the premise assertion must
catch it, then the restored tree must pass the same test. Run with no other
writers/builds in this worktree.

Release mode, because the mutants must stay observable without debug-only
checks. The red runs set ``PDRS_TIMING_SAMPLES=1``: a mutant full sample costs
tens of seconds even in release. That proves the criterion can catch the
mutant, which is not the same shape as the CI's multi-sample run; for every
mutant here more samples only redden it further (a quadratic mutant is equally
slow in every round, the median of the paired ratios stays high, and the
per-sample cap is never divided by the sample count). The green runs use the
caller's default (three rounds, five for the reparented test).
``CARGO_TARGET_DIR`` must come from the caller; a private build directory would
hide slot contention from cargo-slots.sh.

Every mutation is executed and recorded as KILLED, SURVIVED or INVALID (with a
reason); the script exits non-zero unless all of them are KILLED. A non-zero
exit from an unrelated unwrap, a compile error or a timeout is INVALID: the
mutant must fail at the dedicated message named in its ``expects`` tuple.
"""
import difflib
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)

if not os.environ.get('CARGO_TARGET_DIR'):
    sys.exit('CARGO_TARGET_DIR must come from the caller; refusing a private build directory')

# Each mutation names the test whose timing assertion it must turn red.
LIB_DEEP_FRAME = 'runtime::tools::builtin::web::html_compat::tests::deep_frame_stack_select_end_tags_stay_within_linear_budget'
mutations = [
    # The pre-9511fb790 bottom-up frame scan: every `</select>` walked the whole
    # open frame stack from the bottom, over the 100k template frames below the
    # select, so N templates x M selects was quadratic. (A .rev() scan stays
    # O(1) here: the select frame sits on top when it closes.)
    ('frame-stack-rescan', 'src/pilotdeck-gateway/src/runtime/tools/builtin/web/html_compat.rs',
     '    fn contains(&self, context: Context) -> bool {\n'
     '        match context {\n'
     '            Context::Select => self.select_frames > 0,\n'
     '            Context::Template => self.template_frames > 0,\n'
     '        }\n'
     '    }',
     '    fn contains(&self, context: Context) -> bool {\n'
     '        self.frames.iter().any(|frame| frame.context == context)\n'
     '    }',
     ['--lib', LIB_DEEP_FRAME],
     ('8 MiB rewrite', 'did not scale from')),
    # The pre-4fbb5f78b rewalk of every moved subtree. Linear with the subtree
    # size; the retained absolute cap catches it, and its cache-unfriendly
    # constant can also cross the 8x ratio band on a loaded host. The sink
    # keeps the walk observable in release.
    ('moved-subtree-rewalk', 'src/pilotdeck-gateway/src/runtime/tools/builtin/web/html_dom.rs',
     '    fn attach_height(&mut self, parent: Id, child: Id) {\n'
     '        self.change_child_height(parent, 0, self.nodes[child].height);\n'
     '        self.update_height(parent);\n'
     '    }',
     '    fn attach_height(&mut self, parent: Id, child: Id) {\n'
     '        let mut visited = 0usize;\n'
     '        let mut work = vec![child];\n'
     '        while let Some(id) = work.pop() {\n'
     '            visited += 1;\n'
     '            work.extend(self.children(id));\n'
     '            if let Kind::Element {\n'
     '                template: Some(fragment),\n'
     '                ..\n'
     '            } = self.nodes[id].kind\n'
     '            {\n'
     '                work.push(fragment);\n'
     '            }\n'
     '        }\n'
     '        if visited > (usize::MAX >> 1) {\n'
     '            self.exceeded = true;\n'
     '        }\n'
     '        self.change_child_height(parent, 0, self.nodes[child].height);\n'
     '        self.update_height(parent);\n'
     '    }',
     ['--test', 'html_markdown', 'reparented_wide_subtrees_stay_fast'],
     ('#873',)),
    # The pre-rope join materialized both sides, so joining N siblings moved
    # the whole accumulated text every time. Byte-identical output; only the
    # scaling ratio and the cap turn red.
    ('materializing-join', 'src/pilotdeck-gateway/src/runtime/tools/builtin/web/html_markdown_buffer.rs',
     '        right.leading = 0;\n'
     '        self.concat(left, right)\n'
     '    }\n'
     '    pub fn indent',
     '        right.leading = 0;\n'
     '        let joined = self.flatten(left)? + &self.flatten(right)?;\n'
     '        self.text_owned(joined)\n'
     '    }\n'
     '    pub fn indent',
     ['--test', 'html_markdown', 'large_shallow_pages_match_real_ts_bytes_and_stay_fast'],
     ('did not scale from',)),
    # The same quadratic join on the reparented wide page. The 2 MiB probe
    # (256 KiB/2 MiB, 8x) reds the 22.6x band (linear ~8x) in ~12 s. The
    # 8 MiB full sample is SIGKILLed by host memory pressure (inferred) before
    # its own assertion, which is why the test carries the scaled probe.
    ('materializing-join-reparent',
     'src/pilotdeck-gateway/src/runtime/tools/builtin/web/html_markdown_buffer.rs',
     '        right.leading = 0;\n'
     '        self.concat(left, right)\n'
     '    }\n'
     '    pub fn indent',
     '        right.leading = 0;\n'
     '        let joined = self.flatten(left)? + &self.flatten(right)?;\n'
     '        self.text_owned(joined)\n'
     '    }\n'
     '    pub fn indent',
     ['--test', 'html_markdown', 'reparented_wide_subtrees_stay_fast'],
     ('#873',)),
    # A broken "half": quartering the dominant repeat leaves the halved input
    # at ~0.25x the full bytes, so the doubling premise of the scaling ratio
    # no longer holds. The premise assertion must catch this before any
    # timing is measured (the ratio itself would also redden, but later and
    # for the wrong reason: a quarter input reports ~4x growth).
    ('halving-unsound', 'src/pilotdeck-gateway/tests/html_markdown.rs',
     '            rule["repeat"] = (target / 2).into();',
     '            rule["repeat"] = (target / 4).into();',
     ['--test', 'html_markdown', 'large_shallow_pages_match_real_ts_bytes_and_stay_fast'],
     ('halved recipe',)),
]
selected = sys.argv[2:]
if selected:
    mutations = [mutation for mutation in mutations if mutation[0] in selected]
    assert len(mutations) == len(selected), 'unknown mutation name'


def run_timing_test(target, extra_env, timeout):
    env = os.environ.copy()
    env.update(extra_env)
    wait_for_slot(root / 'scripts/cargo-slots.sh')
    return run_owned(['cargo', 'test', '--release', '--locked', '--offline',
                      '--manifest-path', 'src/Cargo.toml', '-p', 'pilotdeck-gateway',
                      *target, '--', '--test-threads=1'],
                     cwd=root, env=env, timeout=timeout)


TIMING_MARKERS = ('did not scale from', 'per-sample budget', 'halved recipe')


def outcome(result, test, expects=()):
    """A KILLED mutant must fail the named test at its dedicated message.

    A non-zero exit from an unrelated unwrap, a compile error or a timeout is
    INVALID, never KILLED: the mutant must bind to the assertion under test.
    """
    log = result.stdout + result.stderr
    failed_line = f'{test} ... FAILED'
    failure = next((line.strip() for line in log.splitlines()
                    if any(marker in line for marker in TIMING_MARKERS)), '')
    killed = (failed_line in log and bool(failure)
              and all(token in failure for token in expects))
    return log, killed, failure


def record(entry):
    summary.append(entry)
    (out / 'summary.json').write_text(
        json.dumps(summary, ensure_ascii=False, indent=2) + '\n')


summary = []
with mutating_sources(root, sorted({root / filename for _, filename, _, _, _, _ in mutations})) as snapshots:
    for name, filename, old, new, target, expects in mutations:
        path = root / filename
        original = snapshots[path]
        before = hashlib.sha256(original).hexdigest()
        content = original.decode()
        assert content.count(old) == 1, (name, content.count(old))
        changed = content.replace(old, new, 1)
        (out / f'{name}.patch').write_text(''.join(difflib.unified_diff(
            content.splitlines(True), changed.splitlines(True),
            fromfile=filename, tofile=filename)))
        test = target[-1]
        entry = dict(name=name, sourceSha256=before, outcome='INVALID', reason='')
        mutated = False

        try:
            # Pristine tree first: the same assertion must pass before the
            # mutant is applied.
            result = run_timing_test(target, {}, timeout=1200)
            log, _, failure = outcome(result, test)
            (out / f'{name}.green.log').write_text(log)
            if result.returncode != 0:
                raise RuntimeError(
                    f'pristine tree did not pass: {failure or result.returncode}')
            entry['greenPassed'] = True

            path.write_text(changed)
            mutated = True
            result = run_timing_test(target, {'PDRS_TIMING_SAMPLES': '1'}, timeout=1800)
            log, killed, failure = outcome(result, test, expects)
            (out / f'{name}.red.log').write_text(log)
            entry.update(returncode=result.returncode, failure=failure)
            if result.returncode == 0:
                entry.update(outcome='SURVIVED', reason='mutant test passed')
            elif not killed:
                entry.update(outcome='INVALID',
                             reason='failed without the expected dedicated message')
            else:
                entry['outcome'] = 'KILLED'
        except subprocess.TimeoutExpired as error:
            entry.update(outcome='INVALID',
                         reason=f'timed out after {error.timeout}s')
        except Exception as error:
            entry.update(outcome='INVALID',
                         reason=f'{type(error).__name__}: {error}')
        finally:
            if mutated:
                path.write_bytes(original)
                if hashlib.sha256(path.read_bytes()).hexdigest() != before:
                    entry.update(outcome='INVALID', reason='restore failed')

        # Restored tree: the test must come back green before the next mutant,
        # so a broken restoration cannot masquerade as a killed mutant.
        if entry['outcome'] == 'KILLED':
            try:
                result = run_timing_test(target, {}, timeout=1200)
                log, _, failure = outcome(result, test)
                (out / f'{name}.restored.log').write_text(log)
                if result.returncode == 0:
                    entry['restoredPassed'] = True
                else:
                    entry.update(
                        outcome='INVALID',
                        reason=f'restored tree did not pass: '
                               f'{failure or result.returncode}')
            except Exception as error:
                entry.update(outcome='INVALID',
                             reason=f'restored run: {type(error).__name__}: {error}')

        record(entry)
        reason = f" ({entry['reason']})" if entry['reason'] else ''
        print(f"{name}: {entry['outcome']}{reason}", flush=True)

killed = [entry for entry in summary if entry['outcome'] == 'KILLED']
for entry in summary:
    reason = f" ({entry['reason']})" if entry['reason'] else ''
    print(f"{entry['name']}: {entry['outcome']}{reason}")
print(f'{len(killed)}/{len(summary)} killed; every source restored byte-for-byte')
sys.exit(0 if len(killed) == len(summary) else 1)
