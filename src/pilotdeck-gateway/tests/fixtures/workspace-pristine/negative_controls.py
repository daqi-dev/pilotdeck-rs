#!/usr/bin/env python3
"""#1166 Python controls; commit first, never bypass the real HEAD guard.

--check validates every anchor and compiles replacements in memory (not kills).
Normal mode compiles each on-disk Python mutant, runs its bound unittest,
restores, reruns, and accounts for every mutation before a nonzero summary.
"""
import argparse
import importlib.util
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_process import exclusive_write, new_output_directory
from negative_suite import invalidate_completed, publish_summary

ROOT = Path(__file__).resolve().parents[5]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
SOURCE = FIXTURES / 'negative_process.py'
TESTS = Path(__file__).with_name('test_controls.py')
YAML = ROOT / 'src/pilotdeck-yaml/tests/run_resource_scaling.py'
ROUND6 = FIXTURES / 'edgeclaw-entries/negative_round6.py'
JUDGE = FIXTURES / 'negative-output-directory/negative.py'
spec = importlib.util.spec_from_file_location('workspace_python_judge', JUDGE)
judge = importlib.util.module_from_spec(spec)
spec.loader.exec_module(judge)

cases = [
    ('gateway-only', SOURCE, "areas = ('src', '.cargo', ':(top,glob)rust-toolchain*')",
     "areas = ('src/pilotdeck-gateway/src', '.cargo', ':(top,glob)rust-toolchain*')",
     'WorkspacePristine.test_clean_and_all_input_categories', '#1166 all workspace inputs reject residue'),
    ('skip-root-config', SOURCE, "areas = ('src', '.cargo', ':(top,glob)rust-toolchain*')",
     "areas = ('src', ':(top,glob)rust-toolchain*')",
     'WorkspacePristine.test_clean_and_all_input_categories', '#1166 all workspace inputs reject residue'),
    ('skip-toolchain', SOURCE, "areas = ('src', '.cargo', ':(top,glob)rust-toolchain*')",
     "areas = ('src', '.cargo')",
     'WorkspacePristine.test_clean_and_all_input_categories', '#1166 all workspace inputs reject residue'),
    ('no-staged-diff', SOURCE, "for options in ((), ('--cached',)):", "for options in ((),):",
     'WorkspacePristine.test_staged_cancellation_is_rejected',
     '#1166 staged drift cannot hide behind HEAD worktree'),
    ('index-as-baseline', SOURCE,
     "for options in ((), ('--cached',)):\n"
     "        changed = _git(root, 'diff', *options, '--name-only', '-z', baseline,",
     "for options in ((),):\n"
     "        changed = _git(root, 'diff', *options, '--name-only', '-z',",
     'WorkspacePristine.test_staged_deletion_is_rejected', '#1166 staged deletion must remain guarded'),
    ('allow-hidden-index', SOURCE,
     "if entry and (entry[:1].islower() or entry[:1] == b'S'):",
     "if False and entry and (entry[:1].islower() or entry[:1] == b'S'):",
     'WorkspacePristine.test_hidden_index_flags_are_rejected', '#1166 hidden index flags must fail closed'),
    ('ignore-untracked', SOURCE, 'if untracked[0]:', 'if False and untracked[0]:',
     'WorkspacePristine.test_untracked_and_ignored_inputs_are_rejected',
     '#1166 ignored and untracked inputs must fail closed'),
    ('ignore-ignored', SOURCE, "'ls-files', '--others', '-z', '--', *areas,",
     "'ls-files', '--others', '--exclude-standard', '-z', '--', *areas,",
     'WorkspacePristine.test_untracked_and_ignored_inputs_are_rejected',
     '#1166 ignored and untracked inputs must fail closed'),
    ('killed-other-crate', SOURCE, "areas = ('src', '.cargo', ':(top,glob)rust-toolchain*')",
     "areas = ('src/pilotdeck-gateway/src', '.cargo', ':(top,glob)rust-toolchain*')",
     'WorkspacePristine.test_sigkill_predecessor_other_crate_is_rejected',
     '#1166 next holder must reject killed other-crate residue'),
    ('skip-build-clean', SOURCE, "        _assert_product_pristine(root, 'HEAD')\n",
     '        # missing pristine verification\n',
     'SnapshotControls.test_build_copy_and_slot_are_inside_lock',
     '#1166 clean slot build copy must stay inside tree lease'),
    ('skip-build-slot', SOURCE,
     "slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900, interval=15)",
     "slot = subprocess.CompletedProcess([], 0, '', '')",
     'SnapshotControls.test_build_copy_and_slot_are_inside_lock',
     '#1166 clean slot build copy must stay inside tree lease'),
    ('skip-build-lock', SOURCE, 'with tree_lock(root, timeout=900):', 'with nullcontext():',
     'SnapshotControls.test_build_copy_and_slot_are_inside_lock',
     '#1166 clean slot build copy must stay inside tree lease'),
    ('return-shared-binary', SOURCE,
     "binary = copy_binary(executables.pop(), output / 'executable')",
     'binary = executables.pop()',
     'SnapshotControls.test_build_copy_and_slot_are_inside_lock',
     '#1166 clean slot build copy must stay inside tree lease'),
    ('compile-runs-measurements', SOURCE, "command.append('--no-run')", 'pass  # intentionally omit compile-only option',
     'SnapshotControls.test_build_copy_and_slot_are_inside_lock',
     '#1166 measurement test build must not execute tests'),
    ('bad-build-accepted', SOURCE, '        result.check_returncode()\n        executables',
     '        # missing build status\n        executables',
     'SnapshotControls.test_failed_or_missing_build_never_copies',
     '#1166 bad build must not create measurement snapshot'),
    ('missing-executable-accepted', SOURCE, 'if len(executables) != 1:', 'if len(executables) > 1:',
     'SnapshotControls.test_missing_build_is_attributable', '#1166 missing artifact must fail explicitly'),
    ('unpin-binary', SOURCE, 'if expected_sha256 is not None and digest != expected_sha256:',
     'if False and expected_sha256 is not None and digest != expected_sha256:',
     'SnapshotControls.test_pinned_copy_is_verified_and_exclusive',
     '#1166 pinned digest must match copied bytes'),
    ('overwrite-binary', SOURCE, "destination.open('xb')", "destination.open('wb')",
     'SnapshotControls.test_pinned_copy_is_verified_and_exclusive',
     '#1166 binary copy cannot overwrite evidence'),
    ('reuse-output', SOURCE, 'output.mkdir(parents=True, exist_ok=False)',
     'output.mkdir(parents=True, exist_ok=True)',
     'SnapshotControls.test_existing_empty_run_is_refused', '#1166 existing empty run must be refused'),
    ('missing-target-accepted', SOURCE,
     "if not env.get('CARGO_TARGET_DIR'):\n        raise RuntimeError('#1166 CARGO_TARGET_DIR must be supplied by the caller')",
     "env.setdefault('CARGO_TARGET_DIR', str(root / 'target'))",
     'SnapshotControls.test_no_target_no_build', '#1166 absent target must fail before build'),
    ('yaml-missing-pin-accepted', YAML, 'if sha256 is None:', 'if False and sha256 is None:',
     'SnapshotControls.test_yaml_pinned_snapshot_does_not_wait_for_parent',
     '#1166 explicit binary requires pinned digest'),
    ('artifact-kind-filter', SOURCE, "and row['target'].get('kind') == [kind] and row.get('executable')):",
     "and row.get('executable')):",
     'SnapshotControls.test_artifact_kind_filter', '#1166 select only requested executable kind'),
    ('build-progress-deadline', SOURCE, 'timeout=1800, idle_timeout=600)',
     'timeout=1800, idle_timeout=60)',
     'SnapshotControls.test_build_copy_and_slot_are_inside_lock',
     '#1166 cargo build must retain bounded progress deadlines'),
    ('missing-digest-record', SOURCE,
     "(output / 'sha256').write_text(hashlib.sha256(Path(binary).read_bytes()).hexdigest() + '\\n')",
     "(output / 'wrong-digest-path').write_text(hashlib.sha256(Path(binary).read_bytes()).hexdigest() + '\\n')",
     'SnapshotControls.test_digest_metadata', '#1166 snapshot digest is recorded'),
    ('overwrite-measurement-output', YAML, "with output.open('x') as stream:", "with output.open('w') as stream:",
     'SnapshotControls.test_yaml_output_is_exclusive_and_default_is_unique',
     '#1166 explicit measurement output is exclusive'),

    ('copy-drops-mode', SOURCE,
     'os.fchmod(writer.fileno(), stat.S_IMODE(os.fstat(reader.fileno()).st_mode))',
     'os.fchmod(writer.fileno(), 0o600)',
     'SnapshotControls.test_copy_preserves_mode', '#1166 snapshot must retain executable mode'),
    ('yaml-stray-digest', YAML, 'if sha256 is not None:', 'if False and sha256 is not None:',
     'SnapshotControls.test_yaml_digest_without_binary_is_rejected',
     '#1166 digest requires an explicit binary'),
    ('runner-candidate-not-loaded', ROUND6,
     "patch.object(io, 'open_code', side_effect=code_reader):", 'contextlib.nullcontext():',
     'SnapshotControls.test_runner_code_stream_keeps_real_sources_pristine',
     '#1166 runner meta-control must execute candidate code stream'),

    ('skip-literal-inputs', SOURCE, 'literal_inputs = _workspace_literal_inputs(root, baseline)',
     'literal_inputs = ()', 'WorkspacePristine.test_new_external_literal_inputs_are_automatically_guarded',
     '#1166 new external literal inputs must be guarded'),
    ('skip-module-recursion', SOURCE, "if rust_source or candidate.suffix == '.rs':",
     "if candidate.suffix == '.rs':",
     'WorkspacePristine.test_new_external_literal_inputs_are_automatically_guarded',
     '#1166 new external literal inputs must be guarded'),
    ('skip-default-module', SOURCE, "if values[i + 2] == ';' and not explicit_module:",
     "if False and values[i + 2] == ';' and not explicit_module:",
     'WorkspacePristine.test_external_default_modules_are_guarded',
     '#1166 external default modules remain guarded'),
    ('ignore-parse-error', SOURCE,
     'except (ValueError, OSError, IndexError) as error:\n            raise SourceBaselineError',
     'except (ValueError, OSError, IndexError) as error:\n            continue\n            raise SourceBaselineError',
     'WorkspacePristine.test_nonliteral_and_unparseable_inputs_fail_closed',
     '#1166 unresolved Rust inputs must fail closed'),
    ('cache-input-exempt', SOURCE,
     '_assert_input_areas(root, baseline, literal_inputs, exclude_caches=False)',
     '_assert_input_areas(root, baseline, literal_inputs, exclude_caches=True)',
     'WorkspacePristine.test_explicit_cache_input_is_not_exempt',
     '#1166 included cache names remain build inputs'),
    ('cache-names-rejected', SOURCE,
     "caches = ('target/**', '__pycache__/**', '*.pyc', '.DS_Store', '.pytest_cache/**')",
     "caches = ('__pycache__/**', '*.pyc')",
     'WorkspacePristine.test_only_untracked_caches_are_excluded',
     '#1166 disposable untracked caches are not build inputs'),
    ('accept-foreign-target', SOURCE,
     'if target_dir == root or not target_dir.is_relative_to(root):',
     'if False and (target_dir == root or not target_dir.is_relative_to(root)):',
     'SnapshotControls.test_target_directory_must_belong_to_worktree',
     '#1166 foreign cargo target must be rejected before work'),
    ('accept-input-symlink', SOURCE,
     'if any(part.is_symlink() for part in (candidate, *candidate.parents)):',
     'if False and any(part.is_symlink() for part in (candidate, *candidate.parents)):',
     'WorkspacePristine.test_internal_symlink_input_is_rejected',
     '#1166 input symlink identities cannot be hidden'),
    ('accept-source-symlink', SOURCE,
     'if any(part.is_symlink() for part in (path, *path.parents)):',
     'if False and any(part.is_symlink() for part in (path, *path.parents)):',
     'WorkspacePristine.test_symlink_workspace_source_is_rejected',
     '#1166 source symlink identities cannot be hidden'),
    ('raw-literal-misparsed', SOURCE, "'str' if raw[1] == 'r' else 'opaque'",
     "'opaque'", 'WorkspacePristine.test_new_external_literal_inputs_are_automatically_guarded',
     '#1166 literal discovery accepts valid Rust paths'),
    ('skip-include-recursion', SOURCE, "value == 'include', True))", "False, True))",
     'WorkspacePristine.test_external_default_modules_are_guarded',
     '#1166 external default modules remain guarded'),
    ('ambiguous-macros-accepted', SOURCE,
     'except (ValueError, OSError, IndexError) as error:\n            raise SourceBaselineError',
     'except (ValueError, OSError, IndexError) as error:\n            continue\n            raise SourceBaselineError',
     'WorkspacePristine.test_macro_expansion_and_aliases_fail_closed',
     '#1166 ambiguous macro input locations must fail closed'),

]


def changed(original, name, old, new):
    count = original.count(old)
    if count != 1:
        raise ValueError(f'#1166 stale anchor {name}: count={count}')
    return original.replace(old, new, 1)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path, nargs='?')
    parser.add_argument('--check', action='store_true')
    parser.add_argument('--case', choices=[row[0] for row in cases], help='run one bounded sample')
    args = parser.parse_args()
    if not os.environ.get('CARGO_TARGET_DIR'):
        parser.error('CARGO_TARGET_DIR must be supplied by the caller')
    if args.check:
        failures = []
        for name, path, old, new, _test, _marker in cases:
            try:
                compile(changed(path.read_text(), name, old, new), str(path), 'exec')
                print(f'{name}: anchor unique; Python candidate compile PASS (not executed)')
            except Exception as error:
                failures.append(name)
                print(f'{name}: INVALID: {error}')
        return int(bool(failures))
    if args.output is None:
        parser.error('OUTPUT is required without --check')
    out = new_output_directory(args.output)
    records = []
    guarded = [SOURCE, TESTS, YAML, ROUND6, JUDGE, Path(__file__).resolve(),
               FIXTURES / 'negative_suite.py', FIXTURES / 'negative_panic.py',
               FIXTURES / 'pending_reanchor.py']

    def run(label, test):
        result = run_owned([sys.executable, '-I', '-B', '-X',
                            'pycache_prefix=' + str(out / (label + '.pycache')),
                            str(TESTS), test], cwd=ROOT, timeout=90)
        exclusive_write(out / (label + '.log'), result.stdout + result.stderr)
        return result

    selected = [row for row in cases if args.case is None or row[0] == args.case]
    for name, path, old, new, test, marker in selected:
        status, reason, compiled = 'INVALID', 'not run', False
        try:
            with mutating_sources(ROOT, guarded) as originals:
                candidate = changed(originals[path].decode(), name, old, new)
                if not judge.passed(run(name + '-positive', test), test):
                    raise RuntimeError('#1166 bound positive did not pass')
                path.write_text(candidate)
                compile(path.read_bytes(), str(path), 'exec')
                compiled = True
                exclusive_write(out / (name + '-compile.log'), '#1166 on-disk Python compile PASS\n')
                status, reason = judge.classify(run(name + '-negative', test), test, marker)
            with mutating_sources(ROOT, guarded):
                if not judge.passed(run(name + '-restored', test), test):
                    raise RuntimeError('#1166 bound restored did not pass')
        except Exception as error:
            status, reason, compiled = 'INVALID', f'{type(error).__name__}: {error}', False
            invalidate_completed(records, '#1166 control/guard failure: ' + reason)
        records.append(dict(mutation=name, test=test, assertion=marker,
                            status=status, reason=reason, compiled=compiled))
        print(f'{name}: {status}: {reason}', flush=True)
    publish_summary(out, records)
    counts = {state: sum(row['status'] == state for row in records)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()))
    return int(not records or any(row['status'] != 'KILLED' for row in records))


if __name__ == '__main__':
    raise SystemExit(main())
